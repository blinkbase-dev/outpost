use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::archive::ToolResolutionSet;
use super::catalog::{InstallationItem, Installations};
use super::lock::LockedTool;
use super::tool_request::{RequestedTool, ToolRequest};
use crate::config::state::{ShimEntry, State, StateError};
use crate::tools::candidates;

#[cfg(windows)]
mod windows_import {
	pub(super) use std::os::windows::fs::OpenOptionsExt;
}

#[cfg(not(windows))]
mod unix_import {
	pub(super) use std::os::unix::fs::PermissionsExt;
}

#[cfg(not(windows))]
use unix_import::*;
#[cfg(windows)]
use windows_import::*;

pub(crate) fn index_installed_binaries(
	state: &mut State, tools_dir: &Path, installations: &Installations<'_>, groups: &[&ToolResolutionSet],
) -> Result<(), ShimError> {
	let mut indexed = candidates::find_binaries_by_tool(tools_dir, groups.iter().map(|group| group.install_path.as_path()))?;
	for (install_path, binaries) in &mut indexed {
		let Some(items) = installations.get(install_path.as_path()) else {
			continue;
		};
		_exposed_binaries(items, binaries)?;
	}
	state.binaries.extend(indexed);
	state.binaries.retain(|path, _| installations.contains_key(path.as_path()));
	Ok(())
}

pub(crate) fn index_binaries(state: &mut State, tools_dir: &Path, installations: &Installations<'_>) -> Result<(), ShimError> {
	let mut indexed = candidates::find_binaries_by_tool(tools_dir, installations.keys().copied())?;
	for (install_path, items) in installations {
		let Some(binaries) = indexed.get_mut(*install_path) else {
			continue;
		};
		_exposed_binaries(items, binaries)?;
	}
	state.binaries = indexed;
	Ok(())
}

fn _exposed_patterns<'a>(requests: impl IntoIterator<Item = &'a ToolRequest>) -> Option<Vec<(&'a ToolRequest, &'a str)>> {
	// a request without a list neither contributes nor clears the allowlist
	let mut specified = false;
	let mut patterns = Vec::new();
	for request in requests {
		match &request.tool {
			RequestedTool::Github(req) => {
				let Some(exposed) = &req.exposed else {
					continue;
				};
				specified = true;
				patterns.extend(exposed.iter().map(|pattern| (request, pattern.as_str())));
			}
		}
	}
	specified.then_some(patterns)
}

fn _exposed_binaries(items: &[InstallationItem<'_>], binaries: &mut Vec<PathBuf>) -> Result<(), ShimError> {
	let requests = items.iter().flat_map(|item| item.requests.iter().copied());
	// a request without a list keeps every binary
	let Some(patterns) = _exposed_patterns(requests) else {
		return Ok(());
	};
	let mut builder = GlobSetBuilder::new();
	for (_, pattern) in &patterns {
		// '*' stays within one path segment, '**' crosses them
		builder.add(GlobBuilder::new(pattern).literal_separator(true).build().map_err(ShimError::Exposed)?);
	}
	let set = builder.build().map_err(ShimError::Exposed)?;
	let mut matched = vec![false; patterns.len()];
	let mut unmatched = matched.len();
	let mut hits = Vec::new();
	binaries.retain(|binary| {
		// stop collecting indices once every pattern has matched
		if unmatched == 0 {
			return set.is_match(binary);
		}
		// use matches_into only while a pattern is unseen because it checks every pattern
		set.matches_into(binary, &mut hits);
		for &index in &hits {
			if !matched[index] {
				matched[index] = true;
				unmatched -= 1;
			}
		}
		!hits.is_empty()
	});
	let mut warned = HashSet::new();
	for ((request, pattern), is_matched) in patterns.iter().zip(&matched) {
		if *is_matched {
			continue;
		}
		let identity = request.tool.identity();
		if warned.insert((request.origin.clone(), identity.clone(), *pattern)) {
			eprintln!("warning: no binary matches '{pattern}' in {identity}\n  from {}", request.origin.display());
		}
	}
	Ok(())
}

pub(crate) fn index_shims(state: &mut State, installations: &Installations<'_>) -> Result<(), ShimError> {
	let index = _index_shims(state, installations)?;
	state.shims = index.shims;
	if index.conflicts.is_empty() {
		Ok(())
	} else {
		Err(ShimError::Conflict(index.conflicts))
	}
}

pub(crate) fn purge(shims_dir: &Path) -> Result<(), ShimError> {
	let entries = match std::fs::read_dir(shims_dir) {
		Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(error) => return Err(error.into()),
	};
	#[cfg(windows)]
	{
		let mut in_use = Vec::new();
		for entry in &entries {
			// only top-level files are probed because the shims directory is flat
			if !entry.file_type()?.is_file() {
				continue;
			}
			let path = entry.path();
			if std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).is_err() {
				in_use.push(path);
			}
		}
		if !in_use.is_empty() {
			return Err(ShimError::InUse(in_use));
		}
	}
	for entry in entries {
		let path = entry.path();
		if entry.file_type()?.is_dir() {
			std::fs::remove_dir_all(path)?;
		} else {
			std::fs::remove_file(path)?;
		}
	}
	Ok(())
}

#[cfg(windows)]
pub(crate) fn generate(state: &State, shims_dir: &Path) -> Result<(), ShimError> {
	let trampoline = std::env::current_exe()?.with_file_name("op_shim.exe");
	let bytes = std::fs::read(&trampoline).map_err(|source| ShimError::Trampoline { path: trampoline, source })?;
	std::fs::create_dir_all(shims_dir)?;
	for name in state.shims.keys() {
		if !_is_plain_file_name(name) {
			continue;
		}
		let target = shims_dir.join(format!("{name}.exe"));
		let size_matches = std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() == bytes.len() as u64);
		if size_matches && std::fs::read(&target).is_ok_and(|existing| existing == bytes) {
			continue;
		}
		let temp = target.with_extension("exe.tmp");
		match std::fs::rename(&target, &temp) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		std::fs::write(&target, &bytes)?;
	}
	Ok(())
}

#[cfg(not(windows))]
pub(crate) fn generate(state: &State, shims_dir: &Path) -> Result<(), ShimError> {
	std::fs::create_dir_all(shims_dir)?;
	const SCRIPT: &str = "#!/bin/sh\nexec op exec --shim -- \"${0##*/}\" \"$@\"\n";
	for name in state.shims.keys() {
		if !_is_plain_file_name(name) {
			continue;
		}
		let target = shims_dir.join(name);
		let size_matches = std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() == SCRIPT.len() as u64);
		if size_matches && std::fs::read_to_string(&target).is_ok_and(|existing| existing == SCRIPT) {
			continue;
		}
		let temp = shims_dir.join(format!("{name}.tmp"));
		match std::fs::rename(&target, &temp) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		std::fs::write(&target, SCRIPT)?;
		std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
	}
	Ok(())
}

struct ShimIndex {
	shims: BTreeMap<String, Vec<ShimEntry>>,
	conflicts: Vec<ShimConflict>,
}

// a shim name comes from a binary name and must be unique across tools
fn _index_shims(state: &State, installations: &Installations<'_>) -> Result<ShimIndex, ShimError> {
	let mut shims: BTreeMap<String, Vec<ShimEntry>> = BTreeMap::new();
	let mut conflicts: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
	for (install_path, items) in installations {
		let install_path = *install_path;
		let Some(binaries) = state.binaries.get(install_path) else {
			continue;
		};
		let rules = _compile_rename_shims(items)?;
		let mut names: BTreeMap<String, Vec<&PathBuf>> = BTreeMap::new();
		for binary in binaries {
			// windows strips the executable extension and folds case
			let name = if cfg!(windows) {
				binary.file_stem().and_then(OsStr::to_str).map(str::to_ascii_lowercase)
			} else {
				binary.file_name().and_then(OsStr::to_str).map(str::to_owned)
			};
			let Some(name) = name else {
				continue;
			};
			let name = _apply_rename_shims(name, binary, rules.as_ref())?;
			names.entry(name).or_default().push(binary);
		}
		for (name, binaries) in names {
			// a name with more than one binary cannot be shimmed unambiguously
			let [binary] = binaries.as_slice() else {
				conflicts
					.entry(name)
					.or_default()
					.extend(binaries.into_iter().map(|binary| install_path.join(binary)));
				continue;
			};
			for item in items {
				match item.locked {
					LockedTool::Github(lock) => {
						shims.entry(name.clone()).or_default().push(ShimEntry::Github {
							api_url: lock.api_url.clone(),
							full_name: lock.full_name.clone(),
							lane: lock.lane.clone(),
							requested: lock.requested.clone(),
							install_path: install_path.to_path_buf(),
							binary: binary.to_path_buf(),
						});
					}
				}
			}
		}
	}
	// this loop compares tools, not binaries, so exec must pick the real binary
	// at run time for versions that share a name with different layouts
	for (name, entries) in &shims {
		// a name provided by more than one tool cannot be shimmed unambiguously
		let mut tools: HashSet<String> = HashSet::new();
		for entry in entries {
			tools.insert(entry.identity_key());
		}
		if tools.len() > 1 || conflicts.contains_key(name) {
			conflicts.entry(name.clone()).or_default().extend(entries.iter().map(|entry| match entry {
				ShimEntry::Github { install_path, binary, .. } => install_path.join(binary),
			}));
		}
	}
	let conflicts: Vec<ShimConflict> = conflicts.into_iter().map(|(name, paths)| ShimConflict { name, paths }).collect();
	// conflicted names never resolve
	for conflict in &conflicts {
		shims.remove(&conflict.name);
	}
	Ok(ShimIndex { shims, conflicts })
}

fn _compile_rename_shims(items: &[InstallationItem<'_>]) -> Result<Option<(GlobSet, Vec<String>)>, ShimError> {
	// requests sharing an installation contribute one set of rename rules
	let mut pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
	for item in items {
		for request in &item.requests {
			let rename_shims = match &request.tool {
				RequestedTool::Github(req) => req.rename_shims.as_ref(),
			};
			let Some(rename_shims) = rename_shims else {
				continue;
			};
			for (source, target) in rename_shims {
				pairs.insert((source.as_str(), target.as_str()));
			}
		}
	}
	if pairs.is_empty() {
		return Ok(None);
	}
	let mut builder = GlobSetBuilder::new();
	let mut targets = Vec::with_capacity(pairs.len());
	for (source, target) in pairs {
		builder.add(GlobBuilder::new(source).literal_separator(true).build().map_err(ShimError::Rename)?);
		// windows shim names are lowercase
		targets.push(if cfg!(windows) { target.to_ascii_lowercase() } else { target.to_owned() });
	}
	Ok(Some((builder.build().map_err(ShimError::Rename)?, targets)))
}

fn _apply_rename_shims(name: String, binary: &Path, rules: Option<&(GlobSet, Vec<String>)>) -> Result<String, ShimError> {
	let Some((globset, targets)) = rules else {
		return Ok(name);
	};
	// several rules can match one binary, so keep the distinct targets
	let mut matched = BTreeSet::new();
	for index in globset.matches(binary) {
		matched.insert(targets[index].as_str());
	}
	let mut matched = matched.into_iter();
	let Some(target) = matched.next() else {
		return Ok(name);
	};
	// two targets mean the rules rename this binary differently
	let Some(other) = matched.next() else {
		return Ok(target.to_owned());
	};
	let mut conflicts = vec![target.to_owned(), other.to_owned()];
	conflicts.extend(matched.map(str::to_owned));
	Err(ShimError::RenameConflict { name, targets: conflicts })
}

fn _is_plain_file_name(name: &str) -> bool {
	let mut components = Path::new(name).components();
	matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[derive(Debug)]
pub(crate) struct ShimConflict {
	pub(crate) name: String,
	pub(crate) paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum ShimError {
	Io(std::io::Error),
	State(StateError),
	Exposed(globset::Error),
	Rename(globset::Error),
	RenameConflict {
		name: String,
		targets: Vec<String>,
	},
	Conflict(Vec<ShimConflict>),
	#[cfg(windows)]
	InUse(Vec<PathBuf>),
	#[cfg(windows)]
	Trampoline {
		path: PathBuf,
		source: std::io::Error,
	},
}

impl From<std::io::Error> for ShimError {
	fn from(error: std::io::Error) -> Self {
		Self::Io(error)
	}
}

impl From<StateError> for ShimError {
	fn from(error: StateError) -> Self {
		Self::State(error)
	}
}

impl fmt::Display for ShimError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => fmt::Display::fmt(error, f),
			Self::State(error) => fmt::Display::fmt(error, f),
			Self::Exposed(error) => write!(f, "invalid exposed pattern: {error}"),
			Self::Rename(error) => write!(f, "invalid rename_shims pattern: {error}"),
			Self::RenameConflict { name, targets } => write!(f, "conflicting rename for '{name}':\n  {}", targets.join("\n  ")),
			Self::Conflict(conflicts) => {
				f.write_str("conflicting binaries, cannot generate shims:")?;
				for conflict in conflicts {
					write!(f, "\n  {}:", conflict.name)?;
					for path in &conflict.paths {
						write!(f, "\n    {}", path.display())?;
					}
				}
				Ok(())
			}
			#[cfg(windows)]
			Self::InUse(paths) => {
				f.write_str("cannot regenerate shims, in use:")?;
				for path in paths {
					write!(f, "\n  {}", path.display())?;
				}
				Ok(())
			}
			#[cfg(windows)]
			Self::Trampoline { path, source } => write!(f, "failed to read trampoline '{}': {source}", path.display()),
		}
	}
}

impl std::error::Error for ShimError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::State(error) => Some(error),
			Self::Exposed(error) => Some(error),
			Self::Rename(error) => Some(error),
			Self::RenameConflict { .. } => None,
			Self::Conflict(_) => None,
			#[cfg(windows)]
			Self::InUse(_) => None,
			#[cfg(windows)]
			Self::Trampoline { source, .. } => Some(source),
		}
	}
}
