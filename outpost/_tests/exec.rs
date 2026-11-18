use std::path::PathBuf;
use std::process::{Command, Output};

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

	fn script(&self, install_path: &str, relative: &str) {
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
	}

	fn run(&self, args: &[&str]) -> Output {
		Command::new(env!("CARGO_BIN_EXE_op"))
			.args(args)
			.current_dir(&self.config_dir)
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env("OUTPOST_DATA_DIR", &self.config_dir)
			.env_remove("OUTPOST_ENV")
			.env_remove("OUTPOST_TOOLS_DIR")
			.env_remove("OUTPOST_SHIMS_DIR")
			.env_remove("OUTPOST_DOWNLOADS_DIR")
			.output()
			.unwrap()
	}
}

#[test]
fn runs_tool_and_forwards_args() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let config = r#"[tools]
"github:owner/repo" = "1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", name);
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "hello", "world"]);
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(7), "{stderr}");
	assert!(stdout.contains("hello world"), "{stdout}");
}

#[test]
fn runs_from_cached_index() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let config = r#"[tools]
"github:owner/repo" = "1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", name);
	let template = r#"version = 1
[binaries]
"github_owner_repo/1.2.3" = ["{name}"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), template.replace("{name}", name)).unwrap();
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "cached"]);
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(7), "{stderr}");
	assert!(stdout.contains("cached"), "{stdout}");
}

#[test]
fn falls_back_when_cached_match_is_missing() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let config = r#"[tools]
"github:owner/repo" = "1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", &format!("bin/{}", name));
	let template = r#"version = 1
[binaries]
"github_owner_repo/1.2.3" = ["{name}"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), template.replace("{name}", name)).unwrap();
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "fallback"]);
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(7), "{stderr}");
	assert!(stdout.contains("fallback"), "{stdout}");
}

#[test]
fn reports_tool_not_requested() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let fixture = Fixture::new("", lock);
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", name);
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool"]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success());
	assert!(stderr.contains("is not requested by the current config"), "{stderr}");
}

#[cfg(windows)]
#[test]
fn runs_shim_from_shims() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let config = r#"[tools]
"github:owner/repo" = "1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	fixture.script("github_owner_repo/1.2.3", "tool.cmd");
	std::fs::write(
		fixture.config_dir.join("state.toml"),
		r#"version = 1
[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
install_path = "github_owner_repo/1.2.3"
binary = "tool.cmd"
"#,
	)
	.unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool", "shimmed"]);
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(7), "{stderr}");
	assert!(stdout.contains("shimmed"), "{stdout}");
}

#[cfg(windows)]
#[test]
fn reports_shim_failures() {
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"
"#;
	let state = r#"version = 1
[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
install_path = "github_owner_repo/1.2.3"
binary = "tool.cmd"
"#;
	let requested = r#"[tools]
"github:owner/repo" = "1.2.3"
"#;
	let other = r#"[tools]
"github:owner/repo" = "2.0.0"
"#;
	let unrelated = r#"[tools]
"github:other/repo" = "1.2.3"
"#;
	// the shim name is not indexed
	let fixture = Fixture::new(requested, lock);
	fixture.script("github_owner_repo/1.2.3", "tool.cmd");
	std::fs::write(fixture.config_dir.join("state.toml"), state.replace("[[shims.tool]]", "[[shims.other]]")).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("no installed tool provides `tool`"), "{stderr}");
	// indexed, but the config asks for another version
	let fixture = Fixture::new(other, lock);
	fixture.script("github_owner_repo/1.2.3", "tool.cmd");
	std::fs::write(fixture.config_dir.join("state.toml"), state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("`tool` is not installed for `github:owner/repo@2.0.0`"), "{stderr}");
	// indexed, but the config requests a different tool
	let fixture = Fixture::new(unrelated, lock);
	fixture.script("github_owner_repo/1.2.3", "tool.cmd");
	std::fs::write(fixture.config_dir.join("state.toml"), state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("`tool` is not requested by the current config:"), "{stderr}");
	assert!(stderr.contains("github:owner/repo 1.2.3"), "{stderr}");
	// indexed and requested, but the binary is gone
	let fixture = Fixture::new(requested, lock);
	std::fs::write(fixture.config_dir.join("state.toml"), state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("`tool` was not found in"), "{stderr}");
}
