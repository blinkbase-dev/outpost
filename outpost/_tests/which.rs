use std::path::PathBuf;
use std::process::{Command, Output};

use indoc::{formatdoc, indoc};

struct Fixture {
	_dir: tempfile::TempDir,
	config_dir: PathBuf,
}

impl Fixture {
	fn new(config: &str, lock: &str) -> Self {
		let dir = tempfile::tempdir().unwrap();
		let config_dir = dir.path().join("config");
		std::fs::create_dir_all(&config_dir).unwrap();
		std::fs::write(config_dir.join("outpost.toml"), config).unwrap();
		std::fs::write(config_dir.join("outpost.lock"), lock).unwrap();
		Self { _dir: dir, config_dir }
	}

	fn script(&self, install_path: &str, relative: &str) -> PathBuf {
		let path = self.config_dir.join("tools").join(install_path).join(relative);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::write(&path, "#!/bin/sh\necho \"$@\"\nexit 7\n").unwrap();
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
		}
		#[cfg(windows)]
		std::fs::write(&path, "@echo off\r\necho %*\r\nexit /b 7\r\n").unwrap();
		path
	}

	fn shim(&self, shim: &str, binary: &str) {
		let state = formatdoc! {r#"
		version = 1
		[[shims.{shim}]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		install_path = "github_owner_repo/1.2.3"
		binary = "{binary}"
		"#};
		std::fs::write(self.config_dir.join("state.toml"), state).unwrap();
	}

	fn run(&self, name: &str) -> Output {
		let mut command = Command::new(env!("CARGO_BIN_EXE_op"));
		command.args(["which", name]).current_dir(&self.config_dir).env_clear();
		#[cfg(windows)]
		if let Some(value) = std::env::var_os("SystemRoot") {
			command.env("SystemRoot", value);
		}
		command.env("OUTPOST_CONFIG_DIR", &self.config_dir).env("OUTPOST_DATA_DIR", &self.config_dir);
		command.output().unwrap()
	}
}

const CONFIG: &str = indoc! {r#"
[tools]
"github:owner/repo" = "1.2.3"
"#};

const LOCK: &str = indoc! {r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#};

#[test]
fn prints_binary_path_of_shim() {
	let fixture = Fixture::new(CONFIG, LOCK);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	let path = fixture.script("github_owner_repo/1.2.3", name);
	fixture.shim("tool", name);
	let output = fixture.run("tool");
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	// the script exits with 7 so a zero code means it never ran
	assert_eq!(output.status.code(), Some(0), "{stderr}");
	assert_eq!(stdout, format!("{}\n", path.display()));
}

#[test]
fn reports_unknown_name() {
	let fixture = Fixture::new(CONFIG, LOCK);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", name);
	fixture.shim("other", name);
	let output = fixture.run("tool");
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(1), "{stderr}");
	assert!(stderr.contains("no installed tool provides 'tool'"), "{stderr}");
	// errors go to stderr so stdout stays empty
	assert_eq!(stdout, "");
}
