use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
		command.arg("list").current_dir(cwd).env("OUTPOST_CONFIG_DIR", &self.config_dir);
		if let Some(envs) = envs {
			command.env("OUTPOST_ENV", envs);
		} else {
			command.env_remove("OUTPOST_ENV");
		}
		command.output().unwrap()
	}
}

#[test]
fn lists_requested_and_unrequested_tools() {
	let config = r#"
[tools]
"github:owner/repo" = "1.2.3"
"github:owner/other" = "2.0.0"
"github:owner/new" = "3.0.0"
"#;
	let lock = r#"
version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.2.3"
tag_name = "v1.2.3"
install_path = "github_owner_repo/1.2.3"

[[tool]]
backend = "github"
full_name = "owner/other"
lane = ""
requested = "2.0.0"
tag_name = "v2.0.0"
install_path = "github_owner_other/2.0.0"

[[tool]]
backend = "github"
full_name = "owner/old"
lane = ""
requested = "9.0.0"
tag_name = "v9.0.0"
install_path = "github_owner_old/9.0.0"

[[tool]]
backend = "github"
full_name = "owner/gone"
lane = ""
requested = "8.0.0"
tag_name = "v8.0.0"
install_path = "github_owner_gone/8.0.0"
"#;
	let fixture = Fixture::new(config, lock);
	fixture.install("github_owner_repo/1.2.3");
	fixture.install("github_owner_old/9.0.0");
	let output = fixture.run(&fixture.config_dir, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let source = fixture.config_dir.join("outpost.toml").display().to_string();
	let template = r#"TOOL                REQUESTED  VERSION           SOURCE
github:owner/new    3.0.0      missing           {source}
github:owner/other  2.0.0      v2.0.0 (missing)  {source}
github:owner/repo   1.2.3      v1.2.3            {source}
github:owner/old    -          v9.0.0            -
github:owner/gone   -          v8.0.0 (missing)  -
"#;
	let expected = template.replace("{source}", &source);
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
	let template = r#"TOOL               REQUESTED  VERSION  SOURCE
github:owner/repo  1.2.3      missing  {source1}
github:owner/repo  1.2.3      missing  {source2}
"#;
	let source1 = fixture.config_dir.join("outpost.toml").display().to_string();
	let source2 = fixture.config_dir.join("outpost.dev.toml").display().to_string();
	let expected = template.replace("{source1}", &source1).replace("{source2}", &source2);
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
	for (project, _) in &projects {
		let output = fixture.run(project, None);
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
	}
	let elsewhere = root.join("elsewhere");
	std::fs::create_dir_all(&elsewhere).unwrap();
	let output = fixture.run(&elsewhere, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let sources: Vec<_> = projects
		.iter()
		.map(|(project, _)| std::fs::canonicalize(project.join("outpost.toml")).unwrap().display().to_string())
		.collect();
	let template = r#"TOOL               REQUESTED  VERSION  SOURCE
github:owner/repo  1.0.0      missing  {source1}
github:owner/repo  2.0.0      missing  {source2}
github:owner/repo  1.0.0      missing  {source3}
"#;
	let expected = template
		.replace("{source1}", &sources[0])
		.replace("{source2}", &sources[1])
		.replace("{source3}", &sources[2]);
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
	let padding = " ".repeat(tool.len() - "TOOL".len());
	let template = r#"TOOL{padding}  REQUESTED  VERSION  SOURCE
{tool}  latest     missing  {source}
"#;
	let source = fixture.config_dir.join("outpost.toml").display().to_string();
	let expected = template.replace("{padding}", &padding).replace("{tool}", &tool).replace("{source}", &source);
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
install_path = "github_owner_repo/cli/v1.2.3"
"#;
	let fixture = Fixture::new(config, lock);
	fixture.install("github_owner_repo/cli/v1.2.3");
	let output = fixture.run(&fixture.config_dir, None);
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let template = r#"TOOL               REQUESTED  VERSION     SOURCE
github:owner/repo  cli/1.2.3  cli/v1.2.3  {source}
"#;
	let source = fixture.config_dir.join("outpost.toml").display().to_string();
	let expected = template.replace("{source}", &source);
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
	assert!(String::from_utf8(output.stderr).unwrap().contains("invalid install path `../escape` in lock"));
}

#[test]
fn reports_unsupported_backend() {
	let fixture = Fixture::new("[tools]\n\"rust\" = \"latest\"\n", "");
	let output = fixture.run(&fixture.config_dir, None);
	assert!(!output.status.success());
	assert!(String::from_utf8(output.stderr).unwrap().contains("unsupported backend in `rust`"));
}
