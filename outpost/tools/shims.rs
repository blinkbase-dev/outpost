use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use super::lock::{Lock, LockedTool};
use crate::config::state::{State, StateError};
use crate::tools::candidates;

#[cfg(windows)]
mod windows_import {
	pub(super) use std::collections::BTreeMap;
	pub(super) use std::ffi::OsStr;

	pub(super) use crate::config::state::ShimEntry;
}

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

#[derive(Debug)]
pub(crate) enum ShimError {
	Io(std::io::Error),
	State(StateError),
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
		}
	}
}

impl std::error::Error for ShimError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::State(error) => Some(error),
		}
	}
}
