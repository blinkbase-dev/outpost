use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub fn read(config_dir: &Path) -> Result<Lock, LockError> {
	match std::fs::read_to_string(config_dir.join("outpost.lock")) {
		Ok(text) if text.trim().is_empty() => Ok(Lock::default()),
		Ok(text) => {
			let lock: Lock = toml::from_str(&text)?;
			if lock.version != 1 {
				return Err(LockError::UnsupportedVersion(lock.version));
			}
			Ok(lock)
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
		Err(error) => Err(error.into()),
	}
}

pub fn write(config_dir: &Path, lock: &Lock) -> Result<(), LockError> {
	let mut tools = lock.tools.clone();
	tools.sort_by(|a, b| a.key().cmp(&b.key()));
	let text = toml::to_string_pretty(&Lock { version: lock.version, tools })?;
	let path = config_dir.join("outpost.lock");
	let temp = path.with_extension("lock.tmp");
	std::fs::write(&temp, text)?;
	std::fs::rename(&temp, &path)?;
	Ok(())
}

pub fn acquire(config_dir: &Path) -> Result<File, LockError> {
	let file = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(false)
		.open(config_dir.join("outpost.lock.guard"))?;
	match file.try_lock() {
		Ok(()) => Ok(file),
		Err(TryLockError::WouldBlock) => Err(LockError::Busy),
		Err(TryLockError::Error(error)) => Err(error.into()),
	}
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Lock {
	pub version: u32,
	#[serde(default, rename = "tool")]
	pub tools: Vec<LockedTool>,
}

impl Default for Lock {
	fn default() -> Self {
		Self { version: 1, tools: Vec::new() }
	}
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "backend", rename_all = "lowercase")]
pub enum LockedTool {
	Github {
		full_name: String,
		lane: String,
		requested: String,
		tag_name: String,
		install_path: String,
	},
}

impl LockedTool {
	fn key(&self) -> (&str, &str, &str, &str) {
		match self {
			Self::Github {
				full_name, lane, requested, ..
			} => ("github", full_name, lane, requested),
		}
	}
}

#[derive(Debug)]
pub enum LockError {
	Io(std::io::Error),
	Parse(toml::de::Error),
	Serialize(toml::ser::Error),
	Busy,
	UnsupportedVersion(u32),
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
		}
	}
}

#[cfg(test)]
mod tests {
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
	fn round_trips_entries() {
		let dir = tempfile::tempdir().unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: "2.20".to_owned(),
				tag_name: "2.20".to_owned(),
				install_path: "github_owner_repo/2.20".to_owned(),
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
					full_name: "z/repo".to_owned(),
					lane: String::new(),
					requested: "1".to_owned(),
					tag_name: "1".to_owned(),
					install_path: "github_z_repo/1".to_owned(),
				},
				LockedTool::Github {
					full_name: "a/repo".to_owned(),
					lane: "cli/".to_owned(),
					requested: "1".to_owned(),
					tag_name: "1".to_owned(),
					install_path: "github_a_repo/1".to_owned(),
				},
				LockedTool::Github {
					full_name: "a/repo".to_owned(),
					lane: String::new(),
					requested: "2".to_owned(),
					tag_name: "2".to_owned(),
					install_path: "github_a_repo/2".to_owned(),
				},
			],
		};
		write(dir.path(), &lock).unwrap();
		let lock = read(dir.path()).unwrap();
		let order: Vec<_> = lock.tools.iter().map(|tool| tool.key()).collect();
		assert_eq!(order, vec![
			("github", "a/repo", "", "2"),
			("github", "a/repo", "cli/", "1"),
			("github", "z/repo", "", "1"),
		]);
	}

	#[test]
	fn writes_array_of_tables() {
		let dir = tempfile::tempdir().unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: "2.20".to_owned(),
				tag_name: "2.20".to_owned(),
				install_path: "github_owner_repo/2.20".to_owned(),
			}],
		};
		write(dir.path(), &lock).unwrap();
		let text = std::fs::read_to_string(dir.path().join("outpost.lock")).unwrap();
		let expected = "\
version = 1

[[tool]]
backend = \"github\"
full_name = \"owner/repo\"
lane = \"\"
requested = \"2.20\"
tag_name = \"2.20\"
install_path = \"github_owner_repo/2.20\"
";
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
