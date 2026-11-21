use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::archive::ToolResolutionGroup;
use super::catalog::{InstallationItem, Installations};
use super::lock::LockedTool;
use super::tool_request::RequestedTool;
use crate::config::state::{ShimEntry, State, StateError};
use crate::tools::candidates;

#[cfg(windows)]
mod windows_import {
	pub(super) use std::os::windows::fs::OpenOptionsExt;
	pub(super) use std::path::Component;
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
	state: &mut State, tools_dir: &Path, installations: &Installations<'_>, groups: &[&ToolResolutionGroup],
) -> Result<(), ShimError> {
	let mut indexed = candidates::find_binaries_by_tool(tools_dir, groups.iter().map(|group| group.install_path.as_path()))?;
	for (install_path, binaries) in &mut indexed {
		let Some(items) = installations.get(install_path.as_path()) else {
			continue;
		};
		let requests = items.iter().flat_map(|item| &item.requests).map(|request| &request.tool);
		// an artifact with no requests stays unfiltered
		let Some(exposed_patterns) = _exposed_patterns(requests) else {
			continue;
		};
		let set = _exposed_set(exposed_patterns)?;
		binaries.retain(|binary| set.is_match(binary));
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
		let requests: Vec<&RequestedTool> = items.iter().flat_map(|item| &item.requests).map(|request| &request.tool).collect();
		// an artifact with no requests stays unfiltered
		if requests.is_empty() {
			continue;
		}
		let Some(exposed_patterns) = _exposed_patterns(requests) else {
			continue;
		};
		let set = _exposed_set(exposed_patterns)?;
		binaries.retain(|binary| set.is_match(binary));
	}
	state.binaries = indexed;
	Ok(())
}

fn _exposed_patterns<'a>(requests: impl IntoIterator<Item = &'a RequestedTool>) -> Option<Vec<&'a str>> {
	// a request without a list neither contributes nor clears the allowlist
	let mut specified = false;
	let mut patterns = Vec::new();
	for request in requests {
		match request {
			RequestedTool::Github { exposed: Some(exposed), .. } => {
				specified = true;
				patterns.extend(exposed.iter().map(String::as_str));
			}
			RequestedTool::Github { exposed: None, .. } => {}
		}
	}
	specified.then_some(patterns)
}

fn _exposed_set<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Result<GlobSet, ShimError> {
	let mut builder = GlobSetBuilder::new();
	for pattern in patterns {
		// '*' stays within one path segment, '**' crosses them
		builder.add(GlobBuilder::new(pattern).literal_separator(true).build().map_err(ShimError::Exposed)?);
	}
	builder.build().map_err(ShimError::Exposed)
}

#[cfg(windows)]
pub(crate) fn index_shims(state: &mut State, installations: &Installations<'_>) -> Result<(), ShimError> {
	let index = _index_shims(state, installations)?;
	state.shims = index.shims;
	if index.conflicts.is_empty() {
		Ok(())
	} else {
		Err(ShimError::Conflict(index.conflicts))
	}
}

#[cfg(not(windows))]
pub(crate) fn index_shims(_state: &mut State, _installations: &Installations<'_>) -> Result<(), ShimError> {
	Ok(())
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
pub(crate) fn generate(state: &State, shims_dir: &Path, _installations: &Installations<'_>) -> Result<(), ShimError> {
	let trampoline = std::env::current_exe()?.with_file_name("op_shim.exe");
	let bytes = std::fs::read(&trampoline).map_err(|source| ShimError::Trampoline { path: trampoline, source })?;
	std::fs::create_dir_all(shims_dir)?;
	for name in state.shims.keys().map(String::as_str) {
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
pub(crate) fn generate(state: &State, shims_dir: &Path, installations: &Installations<'_>) -> Result<(), ShimError> {
	std::fs::create_dir_all(shims_dir)?;
	let index = _index_shims(state, installations)?;
	for (name, entries) in &index.shims {
		let entry = &entries[0];
		let spec = entry.spec();
		// execute the stored binary path rather than the shim name
		let Some(binary) = entry.binary().to_str() else {
			continue;
		};
		let script = format!(
			"#!/bin/sh\nexec op exec '{}' -- '{}' \"$@\"\n",
			spec.replace('\'', "'\\''"),
			binary.replace('\'', "'\\''")
		);
		let target = shims_dir.join(name);
		let size_matches = std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() == script.len() as u64);
		if size_matches && std::fs::read_to_string(&target).is_ok_and(|existing| existing == script) {
			continue;
		}
		let temp = shims_dir.join(format!("{name}.tmp"));
		match std::fs::rename(&target, &temp) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		std::fs::write(&target, &script)?;
		std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
	}
	// drop stale scripts so conflicted names stop resolving
	for conflict in &index.conflicts {
		match std::fs::remove_file(shims_dir.join(&conflict.name)) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
	}
	if index.conflicts.is_empty() {
		Ok(())
	} else {
		Err(ShimError::Conflict(index.conflicts))
	}
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
					LockedTool::Github {
						api_url,
						full_name,
						lane,
						requested,
						..
					} => {
						shims.entry(name.clone()).or_default().push(ShimEntry::Github {
							api_url: api_url.clone(),
							full_name: full_name.clone(),
							lane: lane.clone(),
							requested: requested.clone(),
							install_path: install_path.to_path_buf(),
							binary: binary.to_path_buf(),
						});
					}
				}
			}
		}
	}
	for (name, entries) in &shims {
		// a name provided by more than one tool cannot be shimmed unambiguously
		let mut tools: HashSet<(&str, &str, &str)> = HashSet::new();
		for entry in entries {
			tools.insert(match entry {
				ShimEntry::Github { full_name, lane, .. } => ("github", full_name.as_str(), lane.as_str()),
			});
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
				RequestedTool::Github { rename_shims, .. } => rename_shims.as_ref(),
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

#[cfg(windows)]
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
			Self::Rename(error) => write!(f, "invalid rename pattern: {error}"),
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
