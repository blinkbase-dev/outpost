use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::tools::version::Version;

pub(crate) fn read(data_dir: &Path) -> Result<Lock, LockError> {
	let lock = read_unverified(data_dir)?;
	let mut identities = HashSet::new();
	let mut tool_by_install_path: HashMap<&Path, &LockedTool> = HashMap::new();
	for tool in &lock.tools {
		let identity = tool.identity();
		if !identities.insert(identity.clone()) {
			return Err(LockError::DuplicateIdentity(identity));
		}
		let install_path = match tool {
			LockedTool::Github { install_path, .. } => install_path,
		};
		if !valid_install_path(install_path) {
			return Err(LockError::InvalidInstallPath(install_path.clone()));
		}
		match tool_by_install_path.get(install_path.as_path()) {
			Some(existing) if !existing.install_prefix_matches(tool) => {
				return Err(LockError::ConflictingInstallPath {
					path: install_path.clone(),
					existing: existing.identity().to_string(),
					incoming: identity.to_string(),
				});
			}
			Some(_) => {}
			None => {
				tool_by_install_path.insert(install_path.as_path(), tool);
			}
		}
	}
	Ok(lock)
}

pub(crate) fn read_unverified(data_dir: &Path) -> Result<Lock, LockError> {
	match std::fs::read_to_string(data_dir.join("outpost.lock")) {
		Ok(text) if text.trim().is_empty() => Ok(Lock::default()),
		Ok(text) => {
			let mut lock: Lock = toml::from_str(&text)?;
			if lock.version != 1 {
				return Err(LockError::UnsupportedVersion(lock.version));
			}
			for tool in &mut lock.tools {
				match tool {
					LockedTool::Github { full_name, .. } => full_name.make_ascii_lowercase(),
				}
			}
			Ok(lock)
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
		Err(error) => Err(error.into()),
	}
}

pub(crate) fn valid_install_path(path: &Path) -> bool {
	let mut components = path.components();
	matches!(components.next(), Some(Component::Normal(_))) && matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

pub(crate) fn write(data_dir: &Path, lock: &Lock) -> Result<(), LockError> {
	let mut tools: Vec<&LockedTool> = lock.tools.iter().collect();
	tools.sort();
	let text = toml::to_string_pretty(&LockRef { version: lock.version, tools })?;
	let path = data_dir.join("outpost.lock");
	let temp = path.with_extension("lock.tmp");
	let mut file = std::fs::File::create(&temp)?;
	file.write_all(text.as_bytes())?;
	file.sync_all()?;
	drop(file);
	std::fs::rename(&temp, &path)?;
	Ok(())
}

pub(crate) fn acquire(data_dir: &Path) -> Result<File, LockError> {
	let file = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(data_dir.join("outpost.lock.guard"))?;
	match file.try_lock() {
		Ok(()) => Ok(file),
		Err(TryLockError::WouldBlock) => Err(LockError::Busy),
		Err(TryLockError::Error(error)) => Err(error.into()),
	}
}

#[derive(Deserialize)]
pub(crate) struct Lock {
	pub(crate) version: u32,
	#[serde(default, rename = "tool")]
	pub(crate) tools: Vec<LockedTool>,
}

#[derive(Serialize)]
struct LockRef<'a> {
	version: u32,
	#[serde(rename = "tool")]
	tools: Vec<&'a LockedTool>,
}

impl Default for Lock {
	fn default() -> Self {
		Self { version: 1, tools: Vec::new() }
	}
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Identity {
	Github {
		api_url: String,
		full_name: String,
		lane: String,
		version: Version,
	},
}

impl fmt::Display for Identity {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Github {
				api_url,
				full_name,
				lane,
				version,
			} => {
				write!(f, "github:{full_name}@{lane}{version}")?;
				if !api_url.is_empty() {
					write!(f, " ({api_url})")?;
				}
				Ok(())
			}
		}
	}
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub(crate) enum LockedTool {
	Github {
		#[serde(default)]
		api_url: String,
		full_name: String,
		lane: String,
		requested: Version,
		tag_name: String,
		install_path: PathBuf,
	},
}

impl LockedTool {
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

	pub(crate) fn install_path(&self) -> &Path {
		match self {
			Self::Github { install_path, .. } => install_path,
		}
	}

	fn install_prefix_matches(&self, other: &Self) -> bool {
		match (self, other) {
			(
				Self::Github { api_url, full_name, .. },
				Self::Github {
					api_url: other_url,
					full_name: other_name,
					..
				},
			) => api_url == other_url && full_name == other_name,
		}
	}
}

#[derive(Debug)]
pub(crate) enum LockError {
	Io(std::io::Error),
	Parse(toml::de::Error),
	Serialize(toml::ser::Error),
	Busy,
	UnsupportedVersion(u32),
	DuplicateIdentity(Identity),
	InvalidInstallPath(PathBuf),
	ConflictingInstallPath { path: PathBuf, existing: String, incoming: String },
}

impl From<std::io::Error> for LockError {
	fn from(error: std::io::Error) -> Self {
		Self::Io(error)
	}
}

impl From<toml::de::Error> for LockError {
	fn from(error: toml::de::Error) -> Self {
		Self::Parse(error)
	}
}

impl From<toml::ser::Error> for LockError {
	fn from(error: toml::ser::Error) -> Self {
		Self::Serialize(error)
	}
}

impl fmt::Display for LockError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(f, "lock file io failed: {error}"),
			Self::Parse(error) => write!(f, "lock file parse failed: {error}"),
			Self::Serialize(error) => write!(f, "lock file serialize failed: {error}"),
			Self::Busy => write!(f, "another outpost install is already running"),
			Self::UnsupportedVersion(version) => write!(f, "unsupported lock version {version}, expected 1"),
			Self::DuplicateIdentity(identity) => write!(f, "duplicate tool '{identity}' in lock"),
			Self::InvalidInstallPath(path) => write!(f, "invalid install path '{}' in lock", path.display()),
			Self::ConflictingInstallPath { path, existing, incoming } => {
				write!(f, "install path '{}' is shared by '{existing}' and '{incoming}' in lock", path.display())
			}
		}
	}
}

impl std::error::Error for LockError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::Parse(error) => Some(error),
			Self::Serialize(error) => Some(error),
			Self::Busy => None,
			Self::UnsupportedVersion(_) => None,
			Self::DuplicateIdentity(_) => None,
			Self::InvalidInstallPath(_) => None,
			Self::ConflictingInstallPath { .. } => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use indoc::indoc;

	use super::*;

	#[test]
	fn reads_missing_file_as_empty() {
		let dir = tempfile::tempdir().unwrap();
		assert!(read(dir.path()).unwrap().tools.is_empty());
	}

	#[test]
	fn reads_empty_file_as_empty() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("outpost.lock"), "").unwrap();
		assert!(read(dir.path()).unwrap().tools.is_empty());
	}

	#[test]
	fn rejects_unsupported_version() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("outpost.lock"), "version = 2\n").unwrap();
		assert!(matches!(read(dir.path()), Err(LockError::UnsupportedVersion(2))));
	}

	#[test]
	fn rejects_duplicate_identities() {
		let dir = tempfile::tempdir().unwrap();
		let text = r#"version = 1

[[tool]]
backend = "github"
api_url = ""
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "github_owner_repo/1.0.0"

[[tool]]
backend = "github"
api_url = ""
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.1"
install_path = "github_owner_repo/1.0.1"
"#;
		std::fs::write(dir.path().join("outpost.lock"), text).unwrap();
		assert!(matches!(read(dir.path()), Err(LockError::DuplicateIdentity(_))));
	}

	#[test]
	fn rejects_unsafe_install_paths() {
		let text = r#"version = 1

[[tool]]
backend = "github"
api_url = ""
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "{}"
"#;
		for path in ["/tmp/tool", "../tool", "./tool", "", "github_a", "github_a/.", "github_a/..", "github_a/1/2"] {
			let dir = tempfile::tempdir().unwrap();
			let text = text.replace("{}", path);
			std::fs::write(dir.path().join("outpost.lock"), text).unwrap();
			assert!(matches!(read(dir.path()), Err(LockError::InvalidInstallPath(_))), "{path}");
		}
	}

	#[cfg(windows)]
	#[test]
	fn rejects_windows_prefix_install_path() {
		let dir = tempfile::tempdir().unwrap();
		let text = r#"version = 1

[[tool]]
backend = "github"
api_url = ""
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "C:"
"#;
		std::fs::write(dir.path().join("outpost.lock"), text).unwrap();
		assert!(matches!(read(dir.path()), Err(LockError::InvalidInstallPath(_))));
	}

	#[test]
	fn allows_same_tool_at_one_install_path() {
		let dir = tempfile::tempdir().unwrap();
		let text = indoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		tag_name = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "latest"
		tag_name = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"
		"#};
		std::fs::write(dir.path().join("outpost.lock"), text).unwrap();
		assert!(read(dir.path()).is_ok());
	}

	#[test]
	fn rejects_different_tools_at_one_install_path() {
		let dir = tempfile::tempdir().unwrap();
		let text = indoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		tag_name = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/other"
		lane = ""
		requested = "1.0.0"
		tag_name = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"
		"#};
		std::fs::write(dir.path().join("outpost.lock"), text).unwrap();
		let error = read(dir.path()).err().unwrap();
		assert!(matches!(error, LockError::ConflictingInstallPath { .. }));
		assert!(error.to_string().contains("github:owner/repo@1.0.0"), "{error}");
		assert!(error.to_string().contains("github:owner/other@1.0.0"), "{error}");
	}

	#[test]
	fn round_trips_entries() {
		let dir = tempfile::tempdir().unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("2.20"),
				tag_name: "2.20".to_owned(),
				install_path: PathBuf::from("github_owner_repo/2.20"),
			}],
		};
		write(dir.path(), &lock).unwrap();
		assert_eq!(read(dir.path()).unwrap().tools, lock.tools);
	}

	#[test]
	fn writes_sorted_entries() {
		let dir = tempfile::tempdir().unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![
				LockedTool::Github {
					api_url: String::new(),
					full_name: "z/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("1"),
					tag_name: "1".to_owned(),
					install_path: PathBuf::from("github_z_repo/1"),
				},
				LockedTool::Github {
					api_url: String::new(),
					full_name: "a/repo".to_owned(),
					lane: "cli/".to_owned(),
					requested: Version::parse("1"),
					tag_name: "1".to_owned(),
					install_path: PathBuf::from("github_a_repo/1"),
				},
				LockedTool::Github {
					api_url: String::new(),
					full_name: "a/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("2"),
					tag_name: "2".to_owned(),
					install_path: PathBuf::from("github_a_repo/2"),
				},
			],
		};
		write(dir.path(), &lock).unwrap();
		let lock = read(dir.path()).unwrap();
		let order: Vec<Identity> = lock.tools.iter().map(LockedTool::identity).collect();
		assert_eq!(order, vec![
			Identity::Github {
				api_url: String::new(),
				full_name: "a/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("2"),
			},
			Identity::Github {
				api_url: String::new(),
				full_name: "a/repo".to_owned(),
				lane: "cli/".to_owned(),
				version: Version::parse("1"),
			},
			Identity::Github {
				api_url: String::new(),
				full_name: "z/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1"),
			},
		]);
	}

	#[test]
	fn writes_array_of_tables() {
		let dir = tempfile::tempdir().unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("2.20"),
				tag_name: "2.20".to_owned(),
				install_path: PathBuf::from("github_owner_repo/2.20"),
			}],
		};
		write(dir.path(), &lock).unwrap();
		let text = std::fs::read_to_string(dir.path().join("outpost.lock")).unwrap();
		let expected = r#"version = 1

[[tool]]
backend = "github"
api_url = ""
full_name = "owner/repo"
lane = ""
requested = "2.20"
tag_name = "2.20"
install_path = "github_owner_repo/2.20"
"#;
		assert_eq!(text, expected);
	}

	#[test]
	fn leaves_no_temp_file() {
		let dir = tempfile::tempdir().unwrap();
		write(dir.path(), &Lock::default()).unwrap();
		assert!(!dir.path().join("outpost.lock.tmp").exists());
	}

	#[test]
	fn acquire_is_exclusive() {
		let dir = tempfile::tempdir().unwrap();
		let _first = acquire(dir.path()).unwrap();
		assert!(matches!(acquire(dir.path()), Err(LockError::Busy)));
	}

	#[test]
	fn acquire_releases_on_drop() {
		let dir = tempfile::tempdir().unwrap();
		drop(acquire(dir.path()).unwrap());
		assert!(acquire(dir.path()).is_ok());
	}
}
