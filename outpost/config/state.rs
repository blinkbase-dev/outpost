use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools::lock::Identity;
use crate::tools::version::Version;

pub(crate) const VERSION: u32 = 1;

pub(crate) fn read(data_dir: &Path) -> Result<(State, Option<String>), StateError> {
	let path = data_dir.join("state.toml");
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

pub(crate) fn write(data_dir: &Path, state: &State, text: Option<&str>) -> Result<(), StateError> {
	let serialized = toml::to_string_pretty(state)?;
	if text == Some(serialized.as_str()) {
		return Ok(());
	}
	let path = data_dir.join("state.toml");
	let temp = path.with_extension("toml.tmp");
	std::fs::write(&temp, serialized).map_err(|source| (temp.clone(), source))?;
	std::fs::rename(&temp, &path).map_err(|source| (path.clone(), source))?;
	Ok(())
}

pub(crate) fn register_config(state: &mut State, system_configs: &[PathBuf], project_configs: &[PathBuf]) {
	// registered paths are written canonical
	let mut registered: Vec<_> = std::mem::take(&mut state.registered_paths)
		.into_iter()
		.chain(project_configs.iter().cloned())
		.filter(|path| !system_configs.contains(path))
		.collect();
	registered.sort();
	registered.dedup();
	state.registered_paths = registered;
}

#[derive(Deserialize, Serialize)]
pub(crate) struct State {
	pub(crate) version: u32,
	#[serde(default)]
	pub(crate) registered_paths: Vec<PathBuf>,
	// executable paths relative to the install directory, keyed by install path
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub(crate) binaries: BTreeMap<PathBuf, Vec<PathBuf>>,
	// shim name -> one entry per installed version with its identity, install
	// path, and binary
	#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
	pub(crate) shims: BTreeMap<String, Vec<ShimEntry>>,
}

impl Default for State {
	fn default() -> Self {
		Self {
			version: VERSION,
			registered_paths: Vec::new(),
			binaries: BTreeMap::new(),
			shims: BTreeMap::new(),
		}
	}
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub(crate) enum ShimEntry {
	Github {
		#[serde(default)]
		api_url: String,
		full_name: String,
		lane: String,
		requested: Version,
		install_path: PathBuf,
		binary: PathBuf,
	},
}

impl ShimEntry {
	pub(crate) fn identity(&self) -> Identity {
		match self {
			Self::Github {
				api_url,
				full_name,
				lane,
				requested,
				..
			} => Identity::Github {
				api_url: api_url.clone(),
				full_name: full_name.clone(),
				lane: lane.clone(),
				version: requested.clone(),
			},
		}
	}

	// key identifies a tool, so a shim name is rejected when more than one tool
	// provides the same binary, and the names would collide
	pub(crate) fn identity_key(&self) -> String {
		match self {
			Self::Github { api_url, full_name, lane, .. } => format!("github:{api_url}:{full_name}@{lane}"),
		}
	}
}

#[derive(Debug)]
pub(crate) enum StateError {
	Io { path: PathBuf, source: std::io::Error },
	Parse { path: PathBuf, source: toml::de::Error },
	Serialize(toml::ser::Error),
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
			Self::Io { path, source } => write!(f, "failed to access '{}': {source}", path.display()),
			Self::Parse { path, source } => write!(f, "failed to parse state '{}': {source}", path.display()),
			Self::Serialize(error) => write!(f, "failed to serialize state: {error}"),
		}
	}
}

impl std::error::Error for StateError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Parse { source, .. } => Some(source),
			Self::Serialize(error) => Some(error),
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
			version: VERSION,
			registered_paths: vec![PathBuf::from("project/outpost.toml")],
			binaries: BTreeMap::from([(PathBuf::from("github_owner_repo/1.2.3"), vec![PathBuf::from("rg"), PathBuf::from("doc/rg")])]),
			shims: BTreeMap::from([("rg".to_owned(), vec![ShimEntry::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
				binary: PathBuf::from("rg"),
			}])]),
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
	fn registers_only_project_configs() {
		let project = PathBuf::from("project/outpost.toml");
		let system = PathBuf::from("system/outpost.toml");
		let mut state = State::default();
		register_config(&mut state, std::slice::from_ref(&system), &[system.clone(), project.clone()]);
		assert_eq!(state.registered_paths.as_slice(), std::slice::from_ref(&project));
		// registering the same project again does not duplicate it
		register_config(&mut state, std::slice::from_ref(&system), std::slice::from_ref(&project));
		assert_eq!(state.registered_paths, vec![project]);
	}

	#[test]
	fn reads_state_without_indexes() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("state.toml"), "version = 1\nregistered_paths = [\"project/outpost.toml\"]\n").unwrap();
		let (state, _) = read(dir.path()).unwrap();
		assert_eq!(state.version, VERSION);
		assert_eq!(state.registered_paths, vec![PathBuf::from("project/outpost.toml")]);
		assert!(state.binaries.is_empty());
		assert!(state.shims.is_empty());
	}

	#[test]
	fn rejects_invalid_state() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("state.toml"), "[registered_paths\n").unwrap();
		assert!(matches!(read(dir.path()), Err(StateError::Parse { .. })));
	}
}
