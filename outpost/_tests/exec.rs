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

	fn script(&self, install_path: &str, relative: &str, code: i32) {
		let path = self.config_dir.join("tools").join(install_path).join(relative);
		std::fs::create_dir_all(path.parent().unwrap()).unwrap();
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::write(&path, format!("#!/bin/sh\necho \"$@\"\nexit {code}\n")).unwrap();
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
		}
		#[cfg(windows)]
		std::fs::write(&path, format!("@echo off\r\necho %*\r\nexit /b {code}\r\n")).unwrap();
	}

	fn run(&self, args: &[&str], env: Option<&str>) -> Output {
		let mut command = Command::new(env!("CARGO_BIN_EXE_op"));
		command.args(args).current_dir(&self.config_dir).env_clear();
		#[cfg(windows)]
		if let Some(value) = std::env::var_os("SystemRoot") {
			command.env("SystemRoot", value);
		}
		command.env("OUTPOST_CONFIG_DIR", &self.config_dir).env("OUTPOST_DATA_DIR", &self.config_dir);
		if let Some(env) = env {
			command.env("OUTPOST_ENV", env);
		}
		command.output().unwrap()
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
	fixture.script("github_owner_repo/1.2.3", name, 7);
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "hello", "world"], None);
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
	fixture.script("github_owner_repo/1.2.3", name, 7);
	let template = r#"version = 1
[binaries]
"github_owner_repo/1.2.3" = ["{name}"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), template.replace("{name}", name)).unwrap();
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "cached"], None);
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
	fixture.script("github_owner_repo/1.2.3", &format!("bin/{}", name), 7);
	let template = r#"version = 1
[binaries]
"github_owner_repo/1.2.3" = ["{name}"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), template.replace("{name}", name)).unwrap();
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool", "fallback"], None);
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
	fixture.script("github_owner_repo/1.2.3", name, 7);
	let output = fixture.run(&["exec", "github:owner/repo", "--", "tool"], None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success());
	assert!(stderr.contains("is not requested by the current config"), "{stderr}");
}

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
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	fixture.script("github_owner_repo/1.2.3", name, 7);
	let state = format!(
		r#"version = 1
[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
install_path = "github_owner_repo/1.2.3"
binary = "{name}"
"#
	);
	std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool", "shimmed"], None);
	let stdout = String::from_utf8(output.stdout).unwrap();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert_eq!(output.status.code(), Some(7), "{stderr}");
	assert!(stdout.contains("shimmed"), "{stdout}");
}

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
	let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
	let state = format!(
		r#"version = 1
[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
install_path = "github_owner_repo/1.2.3"
binary = "{name}"
"#
	);
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
	fixture.script("github_owner_repo/1.2.3", name, 7);
	std::fs::write(fixture.config_dir.join("state.toml"), state.replace("[[shims.tool]]", "[[shims.other]]")).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"], None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("no installed tool provides 'tool'"), "{stderr}");
	// indexed, but the config asks for another version
	let fixture = Fixture::new(other, lock);
	fixture.script("github_owner_repo/1.2.3", name, 7);
	std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"], None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("'tool' is not installed for 'github:owner/repo@2.0.0'"), "{stderr}");
	// indexed, but the config requests a different tool
	let fixture = Fixture::new(unrelated, lock);
	fixture.script("github_owner_repo/1.2.3", name, 7);
	std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"], None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("'tool' is provided by tools that the current config does not request:"), "{stderr}");
	assert!(stderr.contains("github:owner/repo 1.2.3"), "{stderr}");
	// indexed and requested, but the binary is gone
	let fixture = Fixture::new(requested, lock);
	std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
	let output = fixture.run(&["exec", "--shim", "--", "tool"], None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("'tool' was not found in"), "{stderr}");
}

mod github {
	use indoc::formatdoc;
	use sha2::{Digest, Sha256};

	use super::*;

	pub fn _install_path(api_url: &str, repo: &str, version: &str) -> String {
		let suffix = if api_url.is_empty() {
			String::new()
		} else {
			let digest = Sha256::digest(api_url.as_bytes());
			let hash: String = digest.iter().take(4).map(|byte| format!("{byte:02x}")).collect();
			format!("_{hash}")
		};
		format!("github_{repo}{suffix}/{version}")
	}

	#[test]
	fn runs_tool_from_the_requested_origin() {
		let origin_a = "https://a.example.com";
		let origin_b = "https://b.example.com";
		let path_a = _install_path(origin_a, "owner_repo", "1.2.3");
		let path_b = _install_path(origin_b, "owner_repo", "1.2.3");
		let lock = formatdoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_a}"

		[[tool]]
		backend = "github"
		api_url = "{origin_b}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_b}"
		"#};
		let base = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_a}" }}
		"#};
		let dev = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_b}" }}
		"#};
		let fixture = Fixture::new(&base, &lock);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
		let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
		fixture.script(&path_a, name, 7);
		fixture.script(&path_b, name, 8);
		let output = fixture.run(&["exec", "github:owner/repo", "--", "tool"], Some("dev"));
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert_eq!(output.status.code(), Some(8), "{stderr}");
	}

	#[test]
	fn fails_when_the_requested_origin_is_not_installed() {
		let origin_a = "https://a.example.com";
		let origin_b = "https://b.example.com";
		let path_a = _install_path(origin_a, "owner_repo", "1.2.3");
		let base = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_a}" }}
		"#};
		let dev = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_b}" }}
		"#};
		// the lock only has the base origin, so the winning dev origin has no row
		let lock = formatdoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_a}"
		"#};
		let fixture = Fixture::new(&base, &lock);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
		let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
		fixture.script(&path_a, name, 7);
		let output = fixture.run(&["exec", "github:owner/repo", "--", name], Some("dev"));
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(!output.status.success(), "{stderr}");
		assert_ne!(output.status.code(), Some(7), "{stderr}");
		assert!(stderr.contains("'github:owner/repo' is not installed"), "{stderr}");
		assert!(stderr.contains("outpost.dev.toml"), "{stderr}");
	}
}

mod github_shims {
	use indoc::formatdoc;

	use super::github::_install_path;
	use super::*;

	#[test]
	fn runs_binary_from_the_version_selected_by_precedence() {
		let path_one = _install_path("", "owner_repo", "1.0.0");
		let path_two = _install_path("", "owner_repo", "2.0.0");
		let lock = formatdoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		tag_name = "v1.0.0"
		install_path = "{path_one}"

		[[tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "2.0.0"
		tag_name = "v2.0.0"
		install_path = "{path_two}"
		"#};
		let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
		let nested = format!("bin/{name}");
		let base = formatdoc! {r#"
		[tools]
		"github:owner/repo" = "1.0.0"
		"#};
		let dev = formatdoc! {r#"
		[tools]
		"github:owner/repo" = "2.0.0"
		"#};
		let state = formatdoc! {r#"
		version = 1
		[[shims.tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{path_one}"
		binary = "{nested}"

		[[shims.tool]]
		backend = "github"
		api_url = ""
		full_name = "owner/repo"
		lane = ""
		requested = "2.0.0"
		install_path = "{path_two}"
		binary = "{name}"
		"#};
		let fixture = Fixture::new(&base, &lock);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
		fixture.script(&path_one, &nested, 7);
		fixture.script(&path_two, name, 8);
		std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
		let output = fixture.run(&["exec", "--shim", "--", "tool"], Some("dev"));
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert_eq!(output.status.code(), Some(8), "{stderr}");
	}

	#[test]
	fn runs_renamed_shim_from_the_requested_origin() {
		let origin_a = "https://a.example.com";
		let origin_b = "https://b.example.com";
		let path_a = _install_path(origin_a, "owner_repo", "1.2.3");
		let path_b = _install_path(origin_b, "owner_repo", "1.2.3");
		let lock = formatdoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_a}"

		[[tool]]
		backend = "github"
		api_url = "{origin_b}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_b}"
		"#};
		let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
		let base = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_a}" }}
		"#};
		let dev = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_b}", rename_shims = {{ "{name}" = "tool-two" }} }}
		"#};
		let state = formatdoc! {r#"
		version = 1
		[[shims.tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		install_path = "{path_a}"
		binary = "{name}"

		[[shims.tool-two]]
		backend = "github"
		api_url = "{origin_b}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		install_path = "{path_b}"
		binary = "{name}"
		"#};
		let fixture = Fixture::new(&base, &lock);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
		fixture.script(&path_a, name, 7);
		fixture.script(&path_b, name, 8);
		std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
		let output = fixture.run(&["exec", "--shim", "--", "tool-two"], Some("dev"));
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert_eq!(output.status.code(), Some(8), "{stderr}");
	}

	#[test]
	fn fails_when_the_winning_shim_target_is_missing() {
		let origin_a = "https://a.example.com";
		let origin_b = "https://b.example.com";
		let path_a = _install_path(origin_a, "owner_repo", "1.2.3");
		let path_b = _install_path(origin_b, "owner_repo", "1.2.3");
		let name = if cfg!(windows) { "tool.cmd" } else { "tool" };
		let base = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_a}" }}
		"#};
		let dev = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_b}" }}
		"#};
		let lock = formatdoc! {r#"
		version = 1

		[[tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_a}"

		[[tool]]
		backend = "github"
		api_url = "{origin_b}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		tag_name = "v1.2.3"
		install_path = "{path_b}"
		"#};
		let state = formatdoc! {r#"
		version = 1
		[[shims.tool]]
		backend = "github"
		api_url = "{origin_a}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		install_path = "{path_a}"
		binary = "{name}"

		[[shims.tool]]
		backend = "github"
		api_url = "{origin_b}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.2.3"
		install_path = "{path_b}"
		binary = "{name}"
		"#};
		let fixture = Fixture::new(&base, &lock);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
		// the winning dev origin has an entry, but its binary is gone
		fixture.script(&path_a, name, 7);
		std::fs::write(fixture.config_dir.join("state.toml"), &state).unwrap();
		let output = fixture.run(&["exec", "--shim", "--", "tool"], Some("dev"));
		let stderr = String::from_utf8(output.stderr).unwrap();
		let missing = fixture.config_dir.join("tools").join(&path_b);
		assert!(!output.status.success(), "{stderr}");
		assert_ne!(output.status.code(), Some(7), "{stderr}");
		assert!(stderr.contains("'tool' was not found in"), "{stderr}");
		assert!(stderr.contains(&missing.display().to_string()), "{stderr}");
	}
}
