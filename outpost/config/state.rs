use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) fn read(config_dir: &Path) -> Result<(State, Option<String>), StateError> {
	let path = config_dir.join("state.toml");
	match std::fs::read_to_string(&path) {
		Ok(text) if text.trim().is_empty() => Ok((State::default(), Some(text))),
		Ok(text) => {
			let state = toml::from_str::<State>(&text).map_err(|source| (path.clone(), source))?;
			Ok((state, Some(text)))
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((State::default(), None)),
		Err(source) => Err((path, source).into()),
	}
}

pub(crate) fn write(config_dir: &Path, state: &State, text: Option<&str>) -> Result<(), StateError> {
	let serialized = toml::to_string_pretty(state)?;
	if text == Some(serialized.as_str()) {
		return Ok(());
	}
	let path = config_dir.join("state.toml");
	let temp = path.with_extension("toml.tmp");
	std::fs::write(&temp, serialized).map_err(|source| (temp.clone(), source))?;
	std::fs::rename(&temp, &path).map_err(|source| (path.clone(), source))?;
	Ok(())
}

pub(crate) fn acquire(config_dir: &Path) -> Result<File, StateError> {
	let file = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(config_dir.join("outpost.lock.guard"))
		.map_err(|source| (config_dir.join("outpost.lock.guard"), source))?;
	match file.try_lock() {
		Ok(()) => Ok(file),
		Err(TryLockError::WouldBlock) => Err(StateError::Busy),
		Err(TryLockError::Error(error)) => Err((config_dir.join("outpost.lock.guard"), error).into()),
	}
}

#[derive(Default, Deserialize, Serialize)]
pub(crate) struct State {
	#[serde(default)]
	pub(crate) registered_paths: Vec<PathBuf>,
	// executable paths relative to the install directory, keyed by install path
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub(crate) binaries: BTreeMap<String, Vec<PathBuf>>,
	// install paths that provide a shim name, keyed by lowercased name (windows)
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub(crate) shims: BTreeMap<String, Vec<String>>,
}

#[derive(Debug)]
pub(crate) enum StateError {
	Io { path: PathBuf, source: std::io::Error },
	Parse { path: PathBuf, source: toml::de::Error },
	Serialize(toml::ser::Error),
	Busy,
}

impl From<(PathBuf, std::io::Error)> for StateError {
	fn from((path, source): (PathBuf, std::io::Error)) -> Self {
		Self::Io { path, source }
	}
}

impl From<(PathBuf, toml::de::Error)> for StateError {
	fn from((path, source): (PathBuf, toml::de::Error)) -> Self {
		Self::Parse { path, source }
	}
}

impl From<toml::ser::Error> for StateError {
	fn from(source: toml::ser::Error) -> Self {
		Self::Serialize(source)
	}
}

impl fmt::Display for StateError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => write!(f, "failed to access `{}`: {source}", path.display()),
			Self::Parse { path, source } => write!(f, "failed to parse state `{}`: {source}", path.display()),
			Self::Serialize(error) => write!(f, "failed to serialize state: {error}"),
			Self::Busy => write!(f, "another outpost process is updating state"),
		}
	}
}

impl std::error::Error for StateError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Parse { source, .. } => Some(source),
			Self::Serialize(error) => Some(error),
			Self::Busy => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;
	use std::path::PathBuf;

	use super::*;

	#[test]
	fn round_trips_binaries_and_shims() {
		let dir = tempfile::tempdir().unwrap();
		let state = State {
			registered_paths: vec![PathBuf::from("project/outpost.toml")],
			binaries: BTreeMap::from([("github_owner_repo/1.2.3".to_owned(), vec![PathBuf::from("rg"), PathBuf::from("doc/rg")])]),
			shims: BTreeMap::from([("rg".to_owned(), vec!["github_owner_repo/1.2.3".to_owned()])]),
		};
		write(dir.path(), &state, None).unwrap();
		let (read, _) = read(dir.path()).unwrap();
		assert_eq!(read.registered_paths, state.registered_paths);
		assert_eq!(read.binaries, state.binaries);
		assert_eq!(read.shims, state.shims);
	}

	#[test]
	fn omits_empty_indexes() {
		let dir = tempfile::tempdir().unwrap();
		write(dir.path(), &State::default(), None).unwrap();
		let text = std::fs::read_to_string(dir.path().join("state.toml")).unwrap();
		assert!(!text.contains("binaries"));
		assert!(!text.contains("shims"));
	}

	#[test]
	fn reads_state_without_indexes() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("state.toml"), "registered_paths = [\"project/outpost.toml\"]\n").unwrap();
		let (state, _) = read(dir.path()).unwrap();
		assert_eq!(state.registered_paths, vec![PathBuf::from("project/outpost.toml")]);
		assert!(state.binaries.is_empty());
		assert!(state.shims.is_empty());
	}
}
