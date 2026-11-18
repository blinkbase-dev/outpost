use std::path::{Path, PathBuf};
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

	fn run(&self, cwd: &Path, args: &[&str]) -> Output {
		Command::new(env!("CARGO_BIN_EXE_op"))
			.arg("shim")
			.args(args)
			.current_dir(cwd)
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env_remove("OUTPOST_ENV")
			.env_remove("OUTPOST_TOOLS_DIR")
			.env_remove("OUTPOST_SHIMS_DIR")
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

[[tool]]
backend = "github"
full_name = "a/repo"
lane = "cli/"
requested = "1"
tag_name = "cli/v1.0.0"
install_path = "github_a/cli_1"
"#;
	let fixture = Fixture::new("", lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	let other = if cfg!(windows) { "other.exe" } else { "other" };
	fixture.tool("github_a/1", name);
	fixture.tool("github_a/cli_1", other);
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"github_a/cli_1" = ["{other}"]

[[shims.other]]
backend = "github"
full_name = "a/repo"
lane = "cli/"
requested = "1"
install_path = "github_a/cli_1"
binary = "{other}"

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binary = "{name}"
"#;
	#[cfg(not(windows))]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"github_a/cli_1" = ["{other}"]
"#;
	assert_eq!(state, template.replace("{name}", name).replace("{other}", other));
	let shim = fixture.config_dir.join("shims").join(name);
	assert!(shim.exists());
	let other_shim = fixture.config_dir.join("shims").join(other);
	assert!(other_shim.exists());
	#[cfg(not(windows))]
	{
		use std::os::unix::fs::PermissionsExt;
		assert_eq!(std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111, 0o111);
		assert_eq!(
			std::fs::read_to_string(&shim).unwrap(),
			"#!/bin/sh\nexec op exec 'github:a/repo' -- 'tool' \"$@\"\n"
		);
		assert_eq!(
			std::fs::read_to_string(&other_shim).unwrap(),
			"#!/bin/sh\nexec op exec 'github:a/repo@cli/' -- 'other' \"$@\"\n"
		);
	}
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
	let fixture = Fixture::new("", lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	fixture.tool("github_a/1", name);
	let initial = r#"version = 1
[binaries]
"stale/1" = ["old"]
"#;
	std::fs::write(fixture.config_dir.join("state.toml"), initial).unwrap();
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binary = "{name}"
"#;
	#[cfg(not(windows))]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"#;
	assert_eq!(state, template.replace("{name}", name));
	let shim = fixture.config_dir.join("shims").join(name);
	assert!(shim.exists());
	#[cfg(not(windows))]
	{
		use std::os::unix::fs::PermissionsExt;
		assert_eq!(std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111, 0o111);
	}
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
	let fixture = Fixture::new("", lock);
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	fixture.tool("github_a/1", name);
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]

[[shims.tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
install_path = "github_a/1"
binary = "{name}"
"#;
	#[cfg(not(windows))]
	let template = r#"version = 1
registered_paths = []

[binaries]
"github_a/1" = ["{name}"]
"#;
	assert_eq!(state, template.replace("{name}", name));
	let shim = fixture.config_dir.join("shims").join(name);
	assert!(shim.exists());
	#[cfg(not(windows))]
	{
		use std::os::unix::fs::PermissionsExt;
		assert_eq!(std::fs::metadata(&shim).unwrap().permissions().mode() & 0o111, 0o111);
	}
}

#[test]
fn rejects_binaries_duplicated_within_and_across_tools() {
	let name = if cfg!(windows) { "tool.exe" } else { "tool" };
	let single = r#"
version = 1

[[tool]]
backend = "github"
full_name = "a/repo"
lane = ""
requested = "1"
tag_name = "v1.0.0"
install_path = "github_a/1"
"#;
	let pair = r#"
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
	// two binaries for one name in a single tool
	let fixture = Fixture::new("", single);
	fixture.tool("github_a/1", name);
	fixture.tool("github_a/1", &format!("nested/{name}"));
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("conflicting binaries, cannot generate shims:"), "{stderr}");
	let normalized = stderr.replace('\\', "/");
	assert!(normalized.contains(&format!("github_a/1/{name}")), "{stderr}");
	assert!(normalized.contains(&format!("github_a/1/nested/{name}")), "{stderr}");
	assert!(!fixture.config_dir.join("shims").join(name).exists());
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	assert!(!state.contains("shims"), "{state}");
	// the same name from two tools
	let fixture = Fixture::new("", pair);
	fixture.tool("github_a/1", name);
	fixture.tool("github_b/1", name);
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("conflicting binaries, cannot generate shims:"), "{stderr}");
	let normalized = stderr.replace('\\', "/");
	assert!(normalized.contains(&format!("github_a/1/{name}")), "{stderr}");
	assert!(normalized.contains(&format!("github_b/1/{name}")), "{stderr}");
	assert!(!fixture.config_dir.join("shims").join(name).exists());
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	assert!(!state.contains("shims"), "{state}");
	// a duplicate within one tool plus another tool providing the same name
	let fixture = Fixture::new("", pair);
	fixture.tool("github_a/1", name);
	fixture.tool("github_a/1", &format!("nested/{name}"));
	fixture.tool("github_b/1", name);
	let output = fixture.run(&fixture.config_dir, &[]);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("conflicting binaries, cannot generate shims:"), "{stderr}");
	let normalized = stderr.replace('\\', "/");
	assert!(normalized.contains(&format!("github_a/1/{name}")), "{stderr}");
	assert!(normalized.contains(&format!("github_a/1/nested/{name}")), "{stderr}");
	assert!(normalized.contains(&format!("github_b/1/{name}")), "{stderr}");
	assert!(!fixture.config_dir.join("shims").join(name).exists());
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	assert!(!state.contains("shims"), "{state}");
}

mod force {
	use indoc::indoc;

	use super::*;

	const LOCK: &str = indoc! {r#"
	version = 1

	[[tool]]
	backend = "github"
	full_name = "a/repo"
	lane = ""
	requested = "1"
	tag_name = "v1.0.0"
	install_path = "github_a/1"
	"#};

	#[test]
	fn recreates_all_shims() {
		let fixture = Fixture::new("", LOCK);
		let name = if cfg!(windows) { "tool.exe" } else { "tool" };
		fixture.tool("github_a/1", name);
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// without --force, files not in the lock are not touched
		let shims = fixture.config_dir.join("shims");
		let extra = shims.join(if cfg!(windows) { "extra.exe" } else { "extra" });
		std::fs::write(&extra, b"junk").unwrap();
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		assert!(extra.exists());
		// --force clears the directory before regenerating
		let output = fixture.run(&fixture.config_dir, &["--force"]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		assert!(!extra.exists());
		assert!(shims.join(name).exists());
	}

	#[cfg(windows)]
	#[test]
	fn rejects_in_use_shims() {
		use std::os::windows::fs::OpenOptionsExt;

		let fixture = Fixture::new("", LOCK);
		fixture.tool("github_a/1", "tool.exe");
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// hold the shim open so purge sees it as in use
		let shims = fixture.config_dir.join("shims");
		let shim = shims.join("tool.exe");
		let extra = shims.join("extra.exe");
		std::fs::write(&extra, b"junk").unwrap();
		let _held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&shim).unwrap();
		let output = fixture.run(&fixture.config_dir, &["--force"]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(!output.status.success());
		assert!(stderr.contains("cannot regenerate shims, in use:"), "{stderr}");
		assert!(shim.exists());
		assert!(extra.exists());
	}
}

mod exposed {
	use indoc::indoc;

	use super::*;

	const LOCK: &str = indoc! {r#"
	version = 1

	[[tool]]
	backend = "github"
	full_name = "a/repo"
	lane = ""
	requested = "1"
	tag_name = "v1.0.0"
	install_path = "github_a/1"
	"#};

	#[cfg(windows)]
	const INDEXED_STATE: &str = indoc! {r#"
	version = 1
	registered_paths = []

	[binaries]
	"github_a/1" = ["tool.exe"]

	[[shims.tool]]
	backend = "github"
	full_name = "a/repo"
	lane = ""
	requested = "1"
	install_path = "github_a/1"
	binary = "tool.exe"
	"#};
	#[cfg(not(windows))]
	const INDEXED_STATE: &str = indoc! {r#"
	version = 1
	registered_paths = []

	[binaries]
	"github_a/1" = ["tool"]
	"#};

	#[test]
	fn exposes_only_listed_binaries() {
		let name = if cfg!(windows) { "tool.exe" } else { "tool" };
		let hidden = if cfg!(windows) { "hidden.exe" } else { "hidden" };
		let config = indoc! {r#"
		[tools]
		"github:a/repo" = { version = "1", exposed = ["{name}"] }
		"#}
		.replace("{name}", name);
		let fixture = Fixture::new(&config, LOCK);
		fixture.tool("github_a/1", name);
		fixture.tool("github_a/1", hidden);
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		assert_eq!(state, INDEXED_STATE);
		let shims = fixture.config_dir.join("shims");
		assert!(shims.join(name).exists());
		assert!(!shims.join(hidden).exists());
	}

	#[test]
	fn exposes_listed_binaries_after_narrowing() {
		let initial = indoc! {r#"
		[tools]
		"github:a/repo" = "1"
		"#};
		let fixture = Fixture::new(initial, LOCK);
		let name = if cfg!(windows) { "tool.exe" } else { "tool" };
		let hidden = if cfg!(windows) { "hidden.exe" } else { "hidden" };
		fixture.tool("github_a/1", name);
		fixture.tool("github_a/1", hidden);
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		let shims = fixture.config_dir.join("shims");
		assert!(shims.join(name).exists());
		assert!(shims.join(hidden).exists());
		// narrowing the list drops the binary from the next reindex
		let config = indoc! {r#"
		[tools]
		"github:a/repo" = { version = "1", exposed = ["{name}"] }
		"#}
		.replace("{name}", name);
		std::fs::write(fixture.config_dir.join("outpost.toml"), &config).unwrap();
		let output = fixture.run(&fixture.config_dir, &[]);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		assert_eq!(state, INDEXED_STATE);
		assert!(shims.join(name).exists());
	}
}
