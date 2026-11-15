use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, io};

use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::{self, ToolRequest, ToolRequestEntry, ToolRequestError};
use crate::config::loader;
use crate::config::loader::LoadError;
use crate::config::locate::ConfigPaths;

pub(crate) fn requests(paths: &ConfigPaths) -> Result<Vec<ToolRequestEntry>, CandidatesError> {
	let configs = loader::load_paths(paths.project.iter().rev().chain(paths.system.iter().rev()))?;
	Ok(tool_request::requests(&configs)?)
}

pub(crate) struct InstalledTool {
	pub(crate) request: ToolRequest,
	pub(crate) origin: PathBuf,
	pub(crate) install_dir: PathBuf,
	pub(crate) installed_version: String,
}

pub(crate) fn installed(requests: Vec<ToolRequestEntry>, lock: &Lock, tools_dir: &Path) -> Vec<InstalledTool> {
	let locked: HashMap<Identity, &LockedTool> = lock.tools.iter().map(|tool| (tool.identity(), tool)).collect();
	let mut tools = Vec::new();
	let mut seen = HashSet::new();
	for entry in requests {
		let identity = entry.tool.identity();
		if !seen.insert(identity.clone()) {
			continue;
		}
		match locked.get(&identity) {
			Some(LockedTool::Github { install_path, tag_name, .. }) => {
				let install_dir = tools_dir.join(install_path);
				if !install_dir.is_dir() {
					continue;
				}
				tools.push(InstalledTool {
					request: entry.tool,
					origin: entry.origin,
					install_dir,
					installed_version: tag_name.clone(),
				});
			}
			_ => continue,
		}
	}
	tools
}

pub(crate) fn find_binaries(install_dir: &Path, name: &str) -> io::Result<Vec<PathBuf>> {
	let mut components = Path::new(name).components();
	if !(matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()) {
		return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid binary name `{name}`")));
	}
	let mut matches = Vec::new();
	let mut directories = VecDeque::from([install_dir.to_path_buf()]);
	while let Some(directory) = directories.pop_front() {
		let mut entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
		// read_dir order is filesystem-dependent
		entries.sort_by_key(|entry| entry.file_name());
		for entry in entries {
			if entry.file_type()?.is_dir() {
				directories.push_back(entry.path());
				continue;
			}
			if !_binary_matches(&entry.file_name(), name) {
				continue;
			}
			// broken symlinks resolve to nothing
			let metadata = match fs::metadata(entry.path()) {
				Ok(metadata) => metadata,
				Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
				Err(error) => return Err(error),
			};
			if metadata.is_file() && _is_executable(&metadata) {
				matches.push(entry.path());
			}
		}
	}
	Ok(matches)
}

fn _binary_matches(file_name: &OsStr, name: &str) -> bool {
	let Some(file_name) = file_name.to_str() else {
		return false;
	};
	if cfg!(windows) {
		let Some((stem, extension)) = file_name.rsplit_once('.') else {
			return false;
		};
		if !(stem.eq_ignore_ascii_case(name) || file_name.eq_ignore_ascii_case(name)) {
			return false;
		}
		["exe", "bat", "cmd"].iter().any(|candidate| extension.eq_ignore_ascii_case(candidate))
	} else {
		file_name == name
	}
}

#[cfg(unix)]
fn _is_executable(metadata: &fs::Metadata) -> bool {
	use std::os::unix::fs::PermissionsExt;
	metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn _is_executable(_metadata: &fs::Metadata) -> bool {
	true
}

#[derive(Debug)]
pub(crate) enum CandidatesError {
	Load(LoadError),
	Request(ToolRequestError),
}

impl fmt::Display for CandidatesError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Load(error) => fmt::Display::fmt(error, f),
			Self::Request(error) => fmt::Display::fmt(error, f),
		}
	}
}

impl std::error::Error for CandidatesError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Load(error) => Some(error),
			Self::Request(error) => Some(error),
		}
	}
}

impl From<LoadError> for CandidatesError {
	fn from(error: LoadError) -> Self {
		Self::Load(error)
	}
}

impl From<ToolRequestError> for CandidatesError {
	fn from(error: ToolRequestError) -> Self {
		Self::Request(error)
	}
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::*;
	use crate::tools::lock::{Lock, LockedTool};
	use crate::tools::tool_request::ToolRequest;
	use crate::tools::version::Version;

	fn write(root: &Path, relative: &str, keys: &[&str]) -> PathBuf {
		let path = root.join(relative);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		let tools: String = keys.iter().map(|key| format!("\"{key}\" = \"1\"\n")).collect();
		std::fs::write(&path, format!("[tools]\n{tools}")).unwrap();
		path
	}

	fn binary(dir: &Path, relative: &str) -> PathBuf {
		let path = dir.join(relative);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, b"binary").unwrap();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
		}
		path
	}

	#[test]
	fn orders_project_before_system_and_skips_registered() {
		let dir = tempfile::tempdir().unwrap();
		let base = write(dir.path(), "system/config.toml", &["github:o/base"]);
		let dev = write(dir.path(), "system/outpost.dev.toml", &["github:o/dev"]);
		let prod = write(dir.path(), "system/outpost.prod.toml", &["github:o/prod"]);
		let outer = write(dir.path(), "outer/outpost.toml", &["github:o/outer"]);
		let inner = write(dir.path(), "inner/outpost.toml", &["github:o/inner"]);
		let registered = write(dir.path(), "other/outpost.toml", &["github:o/registered"]);
		let paths = ConfigPaths {
			system: vec![base, dev, prod],
			project: vec![outer, inner],
			registered: vec![registered],
			..ConfigPaths::default()
		};
		let requests = requests(&paths).unwrap();
		let names: Vec<_> = requests
			.iter()
			.map(|entry| match &entry.tool {
				ToolRequest::Github { full_name, .. } => full_name.as_str(),
			})
			.collect();
		assert_eq!(names, vec!["o/inner", "o/outer", "o/prod", "o/dev", "o/base"]);
	}

	#[test]
	fn installed_dedupes_and_preserves_precedence() {
		let dir = tempfile::tempdir().unwrap();
		let base = write(dir.path(), "system/config.toml", &["github:o/project"]);
		let env = write(dir.path(), "system/outpost.dev.toml", &["github:o/system"]);
		let project = write(dir.path(), "inner/outpost.toml", &["github:o/project"]);
		let registered = write(dir.path(), "other/outpost.toml", &["github:o/registered"]);
		let tools = dir.path().join("tools");
		for path in ["github_o_project/1", "github_o_system/1", "github_o_registered/1"] {
			std::fs::create_dir_all(tools.join(path)).unwrap();
		}
		let lock = Lock {
			version: 1,
			tools: vec![
				LockedTool::Github {
					full_name: "o/project".to_owned(),
					lane: String::new(),
					requested: Version::parse("1"),
					tag_name: "v1.0.0".to_owned(),
					install_path: "github_o_project/1".to_owned(),
				},
				LockedTool::Github {
					full_name: "o/system".to_owned(),
					lane: String::new(),
					requested: Version::parse("1"),
					tag_name: "v2.0.0".to_owned(),
					install_path: "github_o_system/1".to_owned(),
				},
				LockedTool::Github {
					full_name: "o/registered".to_owned(),
					lane: String::new(),
					requested: Version::parse("1"),
					tag_name: "v3.0.0".to_owned(),
					install_path: "github_o_registered/1".to_owned(),
				},
			],
		};
		let paths = ConfigPaths {
			system: vec![base, env],
			project: vec![project.clone()],
			registered: vec![registered],
			..ConfigPaths::default()
		};
		let candidates = installed(requests(&paths).unwrap(), &lock, &tools);
		assert_eq!(candidates.len(), 2);
		assert_eq!(candidates[0].origin, project);
		assert_eq!(candidates[0].installed_version, "v1.0.0");
		assert_eq!(candidates[1].installed_version, "v2.0.0");
		assert!(matches!(&candidates[0].request, ToolRequest::Github { full_name, .. } if full_name == "o/project"));
		assert!(matches!(&candidates[1].request, ToolRequest::Github { full_name, .. } if full_name == "o/system"));
	}

	#[test]
	fn installed_skips_missing_lock_entries_and_directories() {
		let dir = tempfile::tempdir().unwrap();
		let system = write(dir.path(), "system/config.toml", &["github:o/missing", "github:o/gone"]);
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "o/gone".to_owned(),
				lane: String::new(),
				requested: Version::parse("1"),
				tag_name: "v1.0.0".to_owned(),
				install_path: "github_o_gone/1".to_owned(),
			}],
		};
		let tools = dir.path().join("tools");
		let paths = ConfigPaths {
			system: vec![system],
			..ConfigPaths::default()
		};
		assert!(installed(requests(&paths).unwrap(), &lock, &tools).is_empty());
	}

	#[test]
	fn finds_binaries_at_root_and_nested() {
		let dir = tempfile::tempdir().unwrap();
		let root = binary(dir.path(), if cfg!(windows) { "tool.exe" } else { "tool" });
		let nested = binary(dir.path(), if cfg!(windows) { "bin/tool.exe" } else { "bin/tool" });
		let matches = find_binaries(dir.path(), "tool").unwrap();
		assert_eq!(matches, vec![root, nested]);
	}

	#[test]
	fn ignores_directories_with_binary_name() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::create_dir_all(dir.path().join("tool")).unwrap();
		assert!(find_binaries(dir.path(), "tool").unwrap().is_empty());
	}

	#[test]
	fn rejects_invalid_binary_names() {
		let dir = tempfile::tempdir().unwrap();
		for name in ["", "bin/tool", "../tool", "."] {
			assert_eq!(find_binaries(dir.path(), name).unwrap_err().kind(), std::io::ErrorKind::InvalidInput, "{name}");
		}
	}

	#[test]
	fn skips_broken_symlinks() {
		let dir = tempfile::tempdir().unwrap();
		let link = dir.path().join("tool");
		let missing = dir.path().join("missing");
		#[cfg(unix)]
		let created = std::os::unix::fs::symlink(&missing, &link).is_ok();
		#[cfg(windows)]
		let created = std::os::windows::fs::symlink_file(&missing, &link).is_ok();
		#[cfg(not(any(unix, windows)))]
		let created = false;
		if !created {
			eprintln!("skipping: cannot create a file symlink");
			return;
		}
		assert!(find_binaries(dir.path(), "tool").unwrap().is_empty());
	}

	#[cfg(unix)]
	#[test]
	fn ignores_non_executable_files() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("tool"), b"binary").unwrap();
		assert!(find_binaries(dir.path(), "tool").unwrap().is_empty());
	}

	#[cfg(windows)]
	#[test]
	fn finds_windows_script_binaries() {
		let dir = tempfile::tempdir().unwrap();
		let cmd = binary(dir.path(), "tool.cmd");
		assert_eq!(find_binaries(dir.path(), "tool").unwrap(), vec![cmd]);
	}
}
