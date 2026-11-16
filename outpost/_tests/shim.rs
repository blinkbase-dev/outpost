use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
	_dir: tempfile::TempDir,
	config_dir: PathBuf,
}

impl Fixture {
	fn new(lock: &str) -> Self {
		let dir = tempfile::tempdir().unwrap();
		let config_dir = dir.path().join("config");
		std::fs::create_dir_all(&config_dir).unwrap();
		std::fs::write(config_dir.join("outpost.toml"), "").unwrap();
		std::fs::write(config_dir.join("outpost.lock"), lock).unwrap();
		Self { _dir: dir, config_dir }
	}

	fn tool(&self, install_path: &str, binary: &str) {
		let path = self.config_dir.join("tools").join(install_path).join(binary);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		std::fs::write(&path, b"binary").unwrap();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
		}
	}

	fn run(&self, cwd: &Path) -> Output {
		Command::new(env!("CARGO_BIN_EXE_op"))
			.arg("shim")
			.current_dir(cwd)
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env_remove("OUTPOST_ENV")
			.env_remove("OUTPOST_TOOLS_DIR")
			.env_remove("OUTPOST_DOWNLOADS_DIR")
			.output()
			.unwrap()
	}
}

#[test]
fn indexes_locked_tools() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
tag_name = "v1.0.0"
install_path = "github_a/1"
"#;
	let fixture = Fixture::new(lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	fixture.tool("github_a/1", name);
	let output = fixture.run(&fixture.config_dir);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binaries = ["{name}"]
"#;
	#[cfg(not(windows))]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"#;
	assert_eq!(state, template.replace("{name}", name));
}

#[test]
fn replaces_existing_index() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
tag_name = "v1.0.0"
install_path = "github_a/1"
"#;
	let fixture = Fixture::new(lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	fixture.tool("github_a/1", name);
	let initial = r#"[binaries]
"stale/1" = ["old"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), initial).unwrap();
	let output = fixture.run(&fixture.config_dir);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binaries = ["{name}"]
"#;
	#[cfg(not(windows))]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"#;
	assert_eq!(state, template.replace("{name}", name));
}

#[test]
fn skips_missing_install_dirs() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
tag_name = "v1.0.0"
install_path = "github_a/1"

[[tool]]
backend = "github"
full_name = "b/repo"
lane = ""
requested = "1"
tag_name = "v1.0.0"
install_path = "github_b/1"
"#;
	let fixture = Fixture::new(lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	fixture.tool("github_a/1", name);
	let output = fixture.run(&fixture.config_dir);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binaries = ["{name}"]
"#;
	#[cfg(not(windows))]
	let template = r#"registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"#;
	assert_eq!(state, template.replace("{name}", name));
}
