use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use indoc::formatdoc;

struct Fixture {
	_dir: tempfile::TempDir, // keeps the temp tree alive until the fixture drops
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

	fn install(&self, path: &str) {
		std::fs::create_dir_all(self.config_dir.join("tools").join(path)).unwrap();
	}

	fn run(&self, cwd: &Path, envs: Option<&str>) -> Output {
		let mut command = Command::new(env!("CARGO_BIN_EXE_op"));
		command.arg("list").current_dir(cwd).env_clear();
		#[cfg(windows)]
		if let Some(value) = std::env::var_os("SystemRoot") {
			command.env("SystemRoot", value);
		}
		command.env("OUTPOST_CONFIG_DIR", &self.config_dir).env("OUTPOST_DATA_DIR", &self.config_dir);
		if let Some(envs) = envs {
			command.env("OUTPOST_ENV", envs);
		}
		command.output().unwrap()
	}
}

#[test]
fn lists_requested_and_unrequested_tools() {
	let origin_a = "https://a.example.com";
	let origin_b = "https://b.example.com";
	let base = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_a}" }}
	"github:owner/other" = {{ version = "2.0.0", api_url = "{origin_a}" }}
	"github:owner/new" = {{ version = "3.0.0", api_url = "{origin_a}" }}
	"#};
	let dev = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.2.3", api_url = "{origin_b}" }}
	"#};
	let lock = formatdoc! {r#"
	version = 1

	[[tool]]
	backend = "github"
	api_url = "{origin_b}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.2.3"
	tag_name = "v1.2.3"
	install_path = "github_owner_repo_b/1.2.3"

	[[tool]]
	backend = "github"
	api_url = "{origin_a}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.2.3"
	tag_name = "v1.2.3"
	install_path = "github_owner_repo_a/1.2.3"

	[[tool]]
	backend = "github"
	api_url = "{origin_a}"
	full_name = "owner/other"
	lane = ""
	requested = "2.0.0"
	tag_name = "v2.0.0"
	install_path = "github_owner_other/2.0.0"

	[[tool]]
	backend = "github"
	api_url = "{origin_a}"
	full_name = "owner/old"
	lane = ""
	requested = "9.0.0"
	tag_name = "v9.0.0"
	install_path = "github_owner_old/9.0.0"

	[[tool]]
	backend = "github"
	api_url = ""
	full_name = "owner/gone"
	lane = ""
	requested = "8.0.0"
	tag_name = "v8.0.0"
	install_path = "github_owner_gone/8.0.0"
	"#};
	let fixture = Fixture::new(&base, &lock);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), &dev).unwrap();
	fixture.install("github_owner_repo_a/1.2.3");
	fixture.install("github_owner_repo_b/1.2.3");
	fixture.install("github_owner_old/9.0.0");
	let output = fixture.run(&fixture.config_dir, Some("dev"));
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let source_base = std::fs::canonicalize(fixture.config_dir.join("outpost.toml")).unwrap().display().to_string();
	let source_dev = std::fs::canonicalize(fixture.config_dir.join("outpost.dev.toml"))
		.unwrap()
		.display()
		.to_string();
	let source_padding = " ".repeat(source_dev.len() - "SOURCE".len());
	let dash_padding = " ".repeat(source_dev.len() - 1);
	let expected = formatdoc! {r#"
	TOOL                REQUESTED  VERSION           SOURCE{source_padding}  ORIGIN
	github:owner/repo   1.2.3      v1.2.3            {source_dev}  {origin_b}
	github:owner/new    3.0.0      missing           {source_base}      {origin_a}
	github:owner/other  2.0.0      v2.0.0 (missing)  {source_base}      {origin_a}
	github:owner/repo   1.2.3      v1.2.3            {source_base}      {origin_a}
	github:owner/old    -          v9.0.0            -{dash_padding}  {origin_a}
	github:owner/gone   -          v8.0.0 (missing)  -
	"#};
	assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn lists_duplicate_requests_per_config_file() {
	let config = r#"
[tools]
"github:owner/repo" = "1.2.3"
"#;
	let fixture = Fixture::new(config, "");
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), config).unwrap();
	let output = fixture.run(&fixture.config_dir, Some("dev"));
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let source1 = std::fs::canonicalize(fixture.config_dir.join("outpost.dev.toml"))
		.unwrap()
		.display()
		.to_string();
	let source2 = std::fs::canonicalize(fixture.config_dir.join("outpost.toml")).unwrap().display().to_string();
	let source_padding = " ".repeat(source1.len() - "SOURCE".len());
	let expected = formatdoc! {r#"
	TOOL               REQUESTED  VERSION  SOURCE{source_padding}  ORIGIN
	github:owner/repo  1.2.3      missing  {source1}
	github:owner/repo  1.2.3      missing  {source2}
	"#};
	assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn lists_registered_versions_and_origins() {
	let fixture = Fixture::new("", "");
	let root = fixture._dir.path();
	let projects = [(root.join("first"), "1.0.0"), (root.join("second"), "2.0.0"), (root.join("third"), "1.0.0")];
	let template = r#"
[tools]
"github:owner/repo" = "{version}"
"#;
	for (project, version) in &projects {
		std::fs::create_dir_all(project).unwrap();
		std::fs::write(project.join("outpost.toml"), template.replace("{version}", version)).unwrap();
	}
	let sources: Vec<_> = projects
		.iter()
		.map(|(project, _)| std::fs::canonicalize(project.join("outpost.toml")).unwrap().display().to_string())
		.collect();
	let registered = sources.iter().map(|source| format!("{source:?}")).collect::<Vec<_>>().join(", ");
	std::fs::write(fixture.config_dir.join("state.toml"), format!("version = 1\nregistered_paths = [{registered}]\n")).unwrap();
	let elsewhere = root.join("elsewhere");
	std::fs::create_dir_all(&elsewhere).unwrap();
	let output = fixture.run(&elsewhere, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let source1 = &sources[0];
	let source2 = &sources[1];
	let source3 = &sources[2];
	let source_padding = " ".repeat(source2.len() - "SOURCE".len());
	let expected = formatdoc! {r#"
	TOOL               REQUESTED  VERSION  SOURCE{source_padding}  ORIGIN
	github:owner/repo  1.0.0      missing  {source1}
	github:owner/repo  2.0.0      missing  {source2}
	github:owner/repo  1.0.0      missing  {source3}
	"#};
	assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn prints_nothing_without_tools() {
	let fixture = Fixture::new("", "");
	let output = fixture.run(&fixture.config_dir, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
}

#[test]
fn keeps_full_values_when_output_is_piped() {
	let long_name = format!("owner/{}", "a".repeat(60));
	let config = format!("[tools]\n\"github:{long_name}\" = \"latest\"\n");
	let fixture = Fixture::new(&config, "");
	let output = fixture.run(&fixture.config_dir, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let tool = format!("github:{long_name}");
	let tool_padding = " ".repeat(tool.len() - "TOOL".len());
	let source = std::fs::canonicalize(fixture.config_dir.join("outpost.toml")).unwrap().display().to_string();
	let source_padding = " ".repeat(source.len() - "SOURCE".len());
	let expected = formatdoc! {r#"
	TOOL{tool_padding}  REQUESTED  VERSION  SOURCE{source_padding}  ORIGIN
	{tool}  latest     missing  {source}
	"#};
	assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn shows_lane_with_requested_version() {
	let config = r#"
[tools]
"github:owner/repo" = "cli/1.2.3"
"#;
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = "cli/"
requested = "1.2.3"
tag_name = "cli/v1.2.3"
install_path = "github_owner_repo/cli_v1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	fixture.install("github_owner_repo/cli_v1.2.3");
	let output = fixture.run(&fixture.config_dir, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let source = std::fs::canonicalize(fixture.config_dir.join("outpost.toml")).unwrap().display().to_string();
	let source_padding = " ".repeat(source.len() - "SOURCE".len());
	let expected = formatdoc! {r#"
	TOOL               REQUESTED  VERSION     SOURCE{source_padding}  ORIGIN
	github:owner/repo  cli/1.2.3  cli/v1.2.3  {source}
	"#};
	assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

#[test]
fn reports_unsupported_lock_version() {
	let fixture = Fixture::new("", "version = 2\n");
	let output = fixture.run(&fixture.config_dir, None);
	assert!(!output.status.success());
	assert!(String::from_utf8(output.stderr).unwrap().contains("unsupported lock version 2"));
}

#[test]
fn reports_invalid_install_path() {
	let config = r#"
[tools]
"github:owner/repo" = "1.2.3"
"#;
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "../escape"
"#;
	let fixture = Fixture::new(config, lock);
	let output = fixture.run(&fixture.config_dir, None);
	assert!(!output.status.success());
	assert!(String::from_utf8(output.stderr).unwrap().contains("invalid install path '../escape' in lock"));
}
