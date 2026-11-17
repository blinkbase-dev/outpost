use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use super::lock::{Lock, LockedTool};
use crate::config::state::{State, StateError};
use crate::tools::candidates;

#[cfg(windows)]
mod windows_import {
	pub(super) use std::collections::BTreeMap;

	pub(super) use crate::config::state::ShimEntry;
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
pub(crate) fn index_shims(state: &mut State, lock: &Lock) {
	let mut shims: BTreeMap<String, Vec<ShimEntry>> = BTreeMap::new();
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
					shims.entry(name).or_default().push(ShimEntry::Github {
						full_name: full_name.clone(),
						lane: lane.clone(),
						requested: requested.clone(),
						install_path: install_path.clone(),
						binaries,
					});
				}
			}
		}
	}
	state.shims = shims;
}

#[cfg(not(windows))]
pub(crate) fn index_shims(_state: &mut State, _lock: &Lock) {}

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
		if std::fs::read(&target).is_ok_and(|existing| existing == bytes) {
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
	for tool in &lock.tools {
		match tool {
			LockedTool::Github { full_name, install_path, .. } => {
				let Some(binaries) = state.binaries.get(install_path) else {
					continue;
				};
				for binary in binaries {
					let Some(name) = binary.file_name().and_then(OsStr::to_str) else {
						continue;
					};
					if !_is_plain_file_name(name) {
						continue;
					}
					let script = format!(
						"#!/bin/sh\nexec op exec 'github:{}' -- '{}' \"$@\"\n",
						full_name.replace('\'', "'\\''"),
						name.replace('\'', "'\\''")
					);
					let target = shims_dir.join(name);
					if std::fs::read_to_string(&target).is_ok_and(|existing| existing == script) {
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
			}
		}
	}
	Ok(())
}

fn _is_plain_file_name(name: &str) -> bool {
	let mut components = Path::new(name).components();
	matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[derive(Debug)]
pub(crate) enum ShimError {
	Io(std::io::Error),
	State(StateError),
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
			#[cfg(windows)]
			Self::Trampoline { source, .. } => Some(source),
		}
	}
}
