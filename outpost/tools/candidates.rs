use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::{fmt, fs, io};

use super::tool_request::{self, ToolRequestEntry, ToolRequestError};
use crate::config::loader;
use crate::config::loader::LoadError;
use crate::config::locate::ConfigPaths;

// binaries live at the install root or one directory below
const MAX_BINARY_DEPTH: usize = 1;

pub(crate) fn requests(paths: &ConfigPaths) -> Result<Vec<ToolRequestEntry>, CandidatesError> {
	let configs = loader::load_paths(paths.project.iter().chain(paths.system.iter()))?;
	Ok(tool_request::requests(&configs)?)
}

pub(crate) fn find_binaries(install_dir: &Path, name: &str) -> io::Result<Vec<PathBuf>> {
	let mut components = Path::new(name).components();
	if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
		return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid binary name '{name}'")));
	}
	let mut matches = Vec::new();
	let mut directories = VecDeque::from([(install_dir.to_path_buf(), 0)]);
	while let Some((directory, depth)) = directories.pop_front() {
		let mut entries = match fs::read_dir(&directory) {
			Ok(entries) => entries.collect::<io::Result<Vec<_>>>()?,
			Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
			Err(error) => return Err(error),
		};
		// read_dir order is filesystem-dependent
		entries.sort_by_key(|entry| entry.file_name());
		for entry in entries {
			if entry.file_type()?.is_dir() {
				if depth < MAX_BINARY_DEPTH {
					directories.push_back((entry.path(), depth + 1));
				}
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

pub(crate) fn find_binaries_cached(install_dir: &Path, name: &str, cached: Option<&[PathBuf]>) -> io::Result<Vec<PathBuf>> {
	if let Some(cached) = cached {
		let mut components = Path::new(name).components();
		if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("invalid binary name '{name}'")));
		}
		let mut invalid = false;
		let mut matches = Vec::new();
		for path in cached {
			if path.as_os_str().is_empty() || !path.components().all(|component| matches!(component, Component::Normal(_))) {
				invalid = true;
				continue;
			}
			let Some(file_name) = path.file_name() else {
				continue;
			};
			if !_binary_matches(file_name, name) {
				continue;
			}
			let path = install_dir.join(path);
			match fs::metadata(&path) {
				Ok(metadata) if metadata.is_file() && _is_executable(&metadata) => matches.push(path),
				Ok(_) => {
					invalid = true;
					break;
				}
				Err(error) if error.kind() == io::ErrorKind::NotFound => {
					invalid = true;
					break;
				}
				Err(error) => return Err(error),
			}
		}
		if !invalid {
			return Ok(matches);
		}
	}
	// no cache entry or a cached match is no longer valid
	find_binaries(install_dir, name)
}

pub(crate) fn find_binaries_by_tool<'a>(tools_dir: &Path, install_paths: impl IntoIterator<Item = &'a Path>) -> io::Result<BTreeMap<PathBuf, Vec<PathBuf>>> {
	let mut index = BTreeMap::new();
	for install_path in install_paths {
		let install_dir = tools_dir.join(install_path);
		if !install_dir.is_dir() {
			continue;
		}
		let mut matches = Vec::new();
		// walk relative to the install dir so cached paths stay relative
		let mut directories = VecDeque::from([(PathBuf::new(), 0)]);
		while let Some((directory, depth)) = directories.pop_front() {
			let mut entries = fs::read_dir(install_dir.join(&directory))?.collect::<io::Result<Vec<_>>>()?;
			// read_dir order is filesystem-dependent
			entries.sort_by_key(|entry| entry.file_name());
			for entry in entries {
				let path = directory.join(entry.file_name());
				if entry.file_type()?.is_dir() {
					if depth < MAX_BINARY_DEPTH {
						directories.push_back((path, depth + 1));
					}
					continue;
				}
				if !_binary_matches_any(&entry.file_name()) {
					continue;
				}
				// broken symlinks resolve to nothing
				let metadata = match fs::metadata(install_dir.join(&path)) {
					Ok(metadata) => metadata,
					Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
					Err(error) => return Err(error),
				};
				if metadata.is_file() && _is_executable(&metadata) {
					matches.push(path);
				}
			}
		}
		index.insert(install_path.to_owned(), matches);
	}
	Ok(index)
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

fn _binary_matches_any(file_name: &OsStr) -> bool {
	let Some(file_name) = file_name.to_str() else {
		return false;
	};
	if cfg!(windows) {
		let Some((_, extension)) = file_name.rsplit_once('.') else {
			return false;
		};
		["exe", "bat", "cmd"].iter().any(|candidate| extension.eq_ignore_ascii_case(candidate))
	} else {
		true
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

pub(crate) fn run(path: &Path, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> io::Result<i32> {
	let mut command = Command::new(path);
	#[cfg(windows)]
	if path
		.extension()
		.and_then(OsStr::to_str)
		.is_some_and(|extension| ["bat", "cmd"].iter().any(|candidate| extension.eq_ignore_ascii_case(candidate)))
	{
		command = Command::new("cmd.exe");
		command.arg("/d").arg("/c").arg(_strip_verbatim(path)).args(args);
	} else {
		command.args(args);
	}
	#[cfg(not(windows))]
	command.args(args);
	// let the program find sibling binaries
	if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
		let mut paths = vec![parent.to_path_buf()];
		if let Some(existing) = std::env::var_os("PATH") {
			paths.extend(std::env::split_paths(&existing));
		}
		if let Ok(value) = std::env::join_paths(paths) {
			command.env("PATH", value);
		}
	}
	let status = command.status()?;
	Ok(_exit_code(status))
}

// cmd.exe does not accept canonicalized `\\?\` paths
#[cfg(windows)]
fn _strip_verbatim(path: &Path) -> PathBuf {
	let text = path.to_string_lossy();
	if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
		return PathBuf::from(format!(r"\\{rest}"));
	}
	if let Some(rest) = text.strip_prefix(r"\\?\") {
		return PathBuf::from(rest);
	}
	PathBuf::from(text.as_ref())
}

#[cfg(unix)]
fn _exit_code(status: std::process::ExitStatus) -> i32 {
	if let Some(code) = status.code() {
		return code;
	}
	use std::os::unix::process::ExitStatusExt;
	status.signal().map_or(1, |signal| 128 + signal)
}

#[cfg(not(unix))]
fn _exit_code(status: std::process::ExitStatus) -> i32 {
	status.code().unwrap_or(1)
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
	use crate::tools::tool_request::ToolRequest;

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
	fn orders_project_before_system() {
		let dir = tempfile::tempdir().unwrap();
		let inner = write(dir.path(), "inner/outpost.toml", &["github:o/inner"]);
		let outer = write(dir.path(), "outer/outpost.toml", &["github:o/outer"]);
		let dev = write(dir.path(), "system/outpost.dev.toml", &["github:o/dev"]);
		let prod = write(dir.path(), "system/outpost.prod.toml", &["github:o/prod"]);
		let base = write(dir.path(), "system/config.toml", &["github:o/base"]);
		let paths = ConfigPaths {
			system: vec![dev, prod, base],
			project: vec![inner, outer],
			..ConfigPaths::default()
		};
		let requests = requests(&paths).unwrap();
		let names: Vec<_> = requests
			.iter()
			.map(|entry| match &entry.tool {
				ToolRequest::Github { full_name, .. } => full_name.as_str(),
			})
			.collect();
		assert_eq!(names, vec!["o/inner", "o/outer", "o/dev", "o/prod", "o/base"]);
	}

	#[test]
	fn finds_binaries_at_root_and_nested() {
		let dir = tempfile::tempdir().unwrap();
		let name = if cfg!(windows) { "tool.exe" } else { "tool" };
		let root = binary(dir.path(), name);
		let nested = binary(dir.path(), &format!("bin/{name}"));
		binary(dir.path(), &format!("a/b/{name}"));
		let matches = find_binaries(dir.path(), "tool").unwrap();
		assert_eq!(matches, vec![root, nested]);
	}

	#[test]
	fn missing_directory_has_no_binaries() {
		let dir = tempfile::tempdir().unwrap();
		assert!(find_binaries(&dir.path().join("missing"), "tool").unwrap().is_empty());
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

	#[test]
	fn cached_binaries_returns_indexed_path() {
		let dir = tempfile::tempdir().unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		let path = binary(dir.path(), tool);
		let cached = vec![PathBuf::from(tool)];
		assert_eq!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap(), vec![path]);
	}

	#[test]
	fn cached_binaries_falls_back_when_missing() {
		let dir = tempfile::tempdir().unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		let nested = binary(dir.path(), &format!("bin/{tool}"));
		let cached = vec![PathBuf::from(tool)];
		assert_eq!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap(), vec![nested]);
	}

	#[test]
	fn cached_binaries_returns_empty_when_name_absent() {
		let dir = tempfile::tempdir().unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		binary(dir.path(), tool);
		let other = if cfg!(windows) { "other.exe" } else { "other" };
		let cached = vec![PathBuf::from(other)];
		assert!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap().is_empty());
	}

	#[test]
	fn cached_binaries_rejects_non_relative() {
		let dir = tempfile::tempdir().unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		let path = binary(dir.path(), tool);
		let cached = vec![PathBuf::from("..").join(tool)];
		assert_eq!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap(), vec![path]);
	}

	#[test]
	fn cached_binaries_returns_ambiguous_matches() {
		let dir = tempfile::tempdir().unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		let root = binary(dir.path(), tool);
		let nested = binary(dir.path(), &format!("bin/{tool}"));
		let cached = vec![PathBuf::from(tool), PathBuf::from("bin").join(tool)];
		assert_eq!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap(), vec![root, nested]);
	}

	#[cfg(unix)]
	#[test]
	fn cached_binaries_falls_back_when_not_executable() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("tool"), b"binary").unwrap();
		let cached = vec![PathBuf::from("tool")];
		assert!(find_binaries_cached(dir.path(), "tool", Some(&cached)).unwrap().is_empty());
	}

	#[test]
	fn cached_binaries_rejects_invalid_names() {
		let dir = tempfile::tempdir().unwrap();
		let cached = vec![PathBuf::from("tool")];
		for name in ["", "bin/tool", "../tool", "."] {
			assert_eq!(
				find_binaries_cached(dir.path(), name, Some(&cached)).unwrap_err().kind(),
				std::io::ErrorKind::InvalidInput,
				"{name}"
			);
		}
	}

	#[test]
	fn indexes_binaries_by_tool() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let root = if cfg!(windows) { "root.exe" } else { "root" };
		let nested = if cfg!(windows) { "nested.exe" } else { "nested" };
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		binary(&tools, &format!("github_a/1/{root}"));
		binary(&tools, &format!("github_a/1/bin/{nested}"));
		binary(&tools, &format!("github_a/1/a/b/{nested}"));
		binary(&tools, &format!("github_b/1/{tool}"));
		let index = find_binaries_by_tool(&tools, [Path::new("github_a/1"), Path::new("github_b/1"), Path::new("missing/1")]).unwrap();
		assert_eq!(index.len(), 2);
		assert_eq!(index[Path::new("github_a/1")], vec![PathBuf::from(root), PathBuf::from("bin").join(nested)]);
		assert_eq!(index[Path::new("github_b/1")], vec![PathBuf::from(tool)]);
	}

	#[test]
	fn indexes_binaries_ignores_non_binaries() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let install = tools.join("github_a/1");
		std::fs::create_dir_all(&install).unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		binary(&tools, &format!("github_a/1/{tool}"));
		std::fs::write(install.join(if cfg!(windows) { "readme.md" } else { "readme" }), b"text").unwrap();
		let index = find_binaries_by_tool(&tools, [Path::new("github_a/1")]).unwrap();
		assert_eq!(index[Path::new("github_a/1")], vec![PathBuf::from(tool)]);
	}

	#[test]
	fn indexes_binaries_skips_broken_symlinks() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let install = tools.join("github_a/1");
		std::fs::create_dir_all(&install).unwrap();
		let tool = if cfg!(windows) { "tool.exe" } else { "tool" };
		let link = install.join(tool);
		let missing = install.join("missing");
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
		let index = find_binaries_by_tool(&tools, [Path::new("github_a/1")]).unwrap();
		assert!(index[Path::new("github_a/1")].is_empty());
	}
}
