use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use super::lock::{Lock, LockedTool};
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

pub(crate) fn index_installed_binaries(state: &mut State, tools_dir: &Path, lock: &Lock, install_paths: &[PathBuf]) -> Result<(), ShimError> {
	let indexed = candidates::find_binaries_by_tool(tools_dir, install_paths.iter().map(PathBuf::as_path))?;
	state.binaries.extend(indexed);
	let locked_install_paths: HashSet<&Path> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_path(),
		})
		.collect();
	state.binaries.retain(|path, _| locked_install_paths.contains(path.as_path()));
	Ok(())
}

pub(crate) fn index_binaries(state: &mut State, tools_dir: &Path, lock: &Lock) -> Result<(), ShimError> {
	let install_paths: Vec<&Path> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_path(),
		})
		.collect();
	state.binaries = candidates::find_binaries_by_tool(tools_dir, install_paths)?;
	Ok(())
}

#[cfg(windows)]
pub(crate) fn index_shims(state: &mut State, lock: &Lock) -> Result<(), ShimError> {
	let mut shims: BTreeMap<String, Vec<ShimEntry>> = BTreeMap::new();
	let mut conflicts: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
	for tool in &lock.tools {
		match tool {
			LockedTool::Github {
				full_name,
				lane,
				requested,
				install_path,
				..
			} => {
				let Some(binaries) = state.binaries.get(install_path) else {
					continue;
				};
				let mut names: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
				for binary in binaries {
					let Some(name) = binary.file_stem().and_then(OsStr::to_str) else {
						continue;
					};
					names.entry(name.to_ascii_lowercase()).or_default().push(binary.clone());
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
					shims.entry(name).or_default().push(ShimEntry::Github {
						full_name: full_name.clone(),
						lane: lane.clone(),
						requested: requested.clone(),
						install_path: install_path.clone(),
						binary: binary.clone(),
					});
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
	// drop stale entries so conflicted names stop resolving
	for name in conflicts.keys() {
		shims.remove(name);
	}
	state.shims = shims;
	let conflicts: Vec<ShimConflict> = conflicts.into_iter().map(|(name, paths)| ShimConflict { name, paths }).collect();
	if conflicts.is_empty() { Ok(()) } else { Err(ShimError::Conflict(conflicts)) }
}

#[cfg(not(windows))]
pub(crate) fn index_shims(_state: &mut State, _lock: &Lock) -> Result<(), ShimError> {
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
pub(crate) fn generate(state: &State, shims_dir: &Path, _lock: &Lock) -> Result<(), ShimError> {
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
pub(crate) fn generate(state: &State, shims_dir: &Path, lock: &Lock) -> Result<(), ShimError> {
	std::fs::create_dir_all(shims_dir)?;
	// the same index as windows, but only used to generate the scripts
	let mut shims: BTreeMap<String, Vec<ShimEntry>> = BTreeMap::new();
	let mut conflicts: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
	for tool in &lock.tools {
		match tool {
			LockedTool::Github {
				full_name,
				lane,
				requested,
				install_path,
				..
			} => {
				let Some(binaries) = state.binaries.get(install_path) else {
					continue;
				};
				let mut names: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
				for binary in binaries {
					let Some(name) = binary.file_name().and_then(OsStr::to_str) else {
						continue;
					};
					names.entry(name.to_owned()).or_default().push(binary.clone());
				}
				for (name, binaries) in names {
					// a name with more than one binary in one tool cannot be shimmed
					let [binary] = binaries.as_slice() else {
						conflicts
							.entry(name)
							.or_default()
							.extend(binaries.into_iter().map(|binary| install_path.join(binary)));
						continue;
					};
					shims.entry(name).or_default().push(ShimEntry::Github {
						full_name: full_name.clone(),
						lane: lane.clone(),
						requested: requested.clone(),
						install_path: install_path.clone(),
						binary: binary.clone(),
					});
				}
			}
		}
	}
	for (name, entries) in &shims {
		// a name provided by more than one tool cannot be scripted unambiguously
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
			continue;
		}
		let spec = match &entries[0] {
			ShimEntry::Github { full_name, lane, .. } if !lane.is_empty() => format!("github:{full_name}@{lane}"),
			ShimEntry::Github { full_name, .. } => format!("github:{full_name}"),
		};
		let script = format!(
			"#!/bin/sh\nexec op exec '{}' -- '{}' \"$@\"\n",
			spec.replace('\'', "'\\''"),
			name.replace('\'', "'\\''")
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
	let conflicts: Vec<ShimConflict> = conflicts.into_iter().map(|(name, paths)| ShimConflict { name, paths }).collect();
	// drop stale scripts so conflicted names stop resolving
	for conflict in &conflicts {
		match std::fs::remove_file(shims_dir.join(&conflict.name)) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
	}
	if conflicts.is_empty() { Ok(()) } else { Err(ShimError::Conflict(conflicts)) }
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
			Self::Trampoline { path, source } => write!(f, "failed to read trampoline `{}`: {source}", path.display()),
		}
	}
}

impl std::error::Error for ShimError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::State(error) => Some(error),
			Self::Conflict(_) => None,
			#[cfg(windows)]
			Self::InUse(_) => None,
			#[cfg(windows)]
			Self::Trampoline { source, .. } => Some(source),
		}
	}
}
