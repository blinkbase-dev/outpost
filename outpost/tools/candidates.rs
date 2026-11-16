use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::{fmt, fs, io};

use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::{self, ToolRequestEntry, ToolRequestError};
use crate::config::loader;
use crate::config::loader::LoadError;
use crate::config::locate::ConfigPaths;

pub(crate) fn requests(paths: &ConfigPaths) -> Result<Vec<ToolRequestEntry>, CandidatesError> {
	let configs = loader::load_paths(paths.project.iter().rev().chain(paths.system.iter().rev()))?;
	Ok(tool_request::requests(&configs)?)
}

pub(crate) struct InstalledTool {
	pub(crate) origin: PathBuf,
	pub(crate) install_dir: PathBuf,
	pub(crate) installed_version: String,
}

pub(crate) fn installed(requests: &[ToolRequestEntry], lock: &Lock, tools_dir: &Path) -> HashMap<Identity, InstalledTool> {
	let locked: HashMap<Identity, &LockedTool> = lock.tools.iter().map(|tool| (tool.identity(), tool)).collect();
	let mut tools = HashMap::new();
	for entry in requests {
		let identity = entry.tool.identity();
		match locked.get(&identity) {
			Some(LockedTool::Github { install_path, tag_name, .. }) => {
				let install_dir = tools_dir.join(install_path);
				if !install_dir.is_dir() {
					continue;
				}
				tools.entry(identity).or_insert(InstalledTool {
					origin: entry.origin.clone(),
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
	if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
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

pub(crate) fn run(path: &Path, args: impl IntoIterator<Item = OsString>) -> io::Result<i32> {
	let args: Vec<OsString> = args.into_iter().collect();
	let mut command = Command::new(path);
	#[cfg(windows)]
	if path
		.extension()
		.and_then(OsStr::to_str)
		.is_some_and(|extension| ["bat", "cmd"].iter().any(|candidate| extension.eq_ignore_ascii_case(candidate)))
	{
		let comspec = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
		let stem = Path::new(&comspec).file_stem().and_then(OsStr::to_str);
		match stem {
			Some(stem) if stem.eq_ignore_ascii_case("cmd") => {
				command = Command::new(comspec);
				command.arg("/d").arg("/c").arg(path).args(&args);
			}
			Some(stem) if ["powershell", "pwsh"].iter().any(|candidate| stem.eq_ignore_ascii_case(candidate)) => {
				command = Command::new(comspec);
				command.arg("-NoProfile").arg("-Command").arg(_powershell_command(path, &args));
			}
			_ => {
				return Err(io::Error::new(
					io::ErrorKind::InvalidInput,
					format!("COMSPEC `{}` is not supported", comspec.to_string_lossy()),
				));
			}
		}
	} else {
		command.args(&args);
	}
	#[cfg(not(windows))]
	command.args(&args);
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

#[cfg(windows)]
fn _powershell_command(path: &Path, args: &[OsString]) -> OsString {
	let mut command = format!("& '{}'", path.to_string_lossy().replace('\'', "''"));
	for arg in args {
		command.push_str(&format!(" '{}'", arg.to_string_lossy().replace('\'', "''")));
	}
	command.push_str("; exit $LASTEXITCODE");
	command.into()
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
	fn installed_dedupes_by_identity() {
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
		let candidates = installed(&requests(&paths).unwrap(), &lock, &tools);
		assert_eq!(candidates.len(), 2);

		let project_request = ToolRequest::Github {
			full_name: "o/project".to_owned(),
			lane: String::new(),
			version: Version::parse("1"),
		};
		let project_tool = candidates.get(&project_request.identity()).unwrap();
		assert_eq!(project_tool.origin, project);
		assert_eq!(project_tool.installed_version, "v1.0.0");

		let system_request = ToolRequest::Github {
			full_name: "o/system".to_owned(),
			lane: String::new(),
			version: Version::parse("1"),
		};
		let system_tool = candidates.get(&system_request.identity()).unwrap();
		assert_eq!(system_tool.installed_version, "v2.0.0");

		let registered_request = ToolRequest::Github {
			full_name: "o/registered".to_owned(),
			lane: String::new(),
			version: Version::parse("1"),
		};
		assert!(!candidates.contains_key(&registered_request.identity()));
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
		assert!(installed(&requests(&paths).unwrap(), &lock, &tools).is_empty());
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
