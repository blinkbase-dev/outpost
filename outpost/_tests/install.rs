use std::path::PathBuf;
use std::process::{Command, Output};

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
	_dir: tempfile::TempDir, // keeps the temp tree alive until the fixture drops
	config_dir: PathBuf,
}

impl Fixture {
	fn new(config: &str, env: Option<&str>) -> Self {
		let dir = tempfile::tempdir().unwrap();
		let config_dir = dir.path().join("config");
		std::fs::create_dir_all(&config_dir).unwrap();
		std::fs::write(config_dir.join("outpost.toml"), config).unwrap();
		if let Some(env) = env {
			std::fs::write(config_dir.join("outpost.dev.toml"), env).unwrap();
		}
		Self { _dir: dir, config_dir }
	}

	fn run(&self) -> Output {
		Command::new(env!("CARGO_BIN_EXE_op"))
			.arg("install")
			.current_dir(&self.config_dir)
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env("OUTPOST_ENV", "dev")
			.env_remove("OUTPOST_TOOLS_DIR")
			.env_remove("OUTPOST_SHIMS_DIR")
			.env_remove("OUTPOST_DOWNLOADS_DIR")
			.env_remove("GH_TOKEN")
			.env_remove("GITHUB_TOKEN")
			.output()
			.unwrap()
	}
}

fn _tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
	let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
	for (name, bytes) in files {
		let mut header = tar::Header::new_gnu();
		header.set_path(name).unwrap();
		header.set_size(bytes.len() as u64);
		header.set_mode(0o755);
		header.set_cksum();
		builder.append(&header, *bytes).unwrap();
	}
	builder.into_inner().unwrap().finish().unwrap()
}

async fn _mock_release(server: &MockServer, version: &str) -> (String, &'static str) {
	// pick an asset name that the selector accepts on this host
	let libc = if cfg!(windows) { "msvc" } else { "gnu" };
	let asset = format!("tool-{}-{}-{libc}.tar.gz", std::env::consts::OS, std::env::consts::ARCH);
	let binary = if cfg!(windows) { "tool.exe" } else { "tool" };
	let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
	let nested = format!("bin/{helper}");
	let content = format!("binary {version}");
	let bytes = _tarball(&[(binary, content.as_bytes()), (&nested, content.as_bytes())]);
	Mock::given(method("GET"))
		.and(path(format!("/repos/owner/repo/releases/tags/{version}")))
		.respond_with(ResponseTemplate::new(200).set_body_json(json!({
			"tag_name": format!("v{version}"),
			"draft": false,
			"prerelease": false,
			"assets": [{
				"name": asset,
				"browser_download_url": format!("{}/download/{version}/{asset}", server.uri()),
				"size": bytes.len(),
			}],
		})))
		.mount(server)
		.await;
	Mock::given(method("GET"))
		.and(path(format!("/download/{version}/{asset}")))
		.respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
		.mount(server)
		.await;
	(asset, binary)
}

#[test]
fn skips_installed_tools() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, _) = runtime.block_on(_mock_release(&server, "1.0.0"));
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config, None);
	let first = fixture.run();
	assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
	let second = fixture.run();
	assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
	// the lock row and installed dir make the second run a no-op
	assert!(second.stdout.is_empty(), "{}", String::from_utf8_lossy(&second.stdout));
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec![
		"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		format!("/download/1.0.0/{asset}"),
	]);
}

#[test]
fn installs_github_release_asset() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
	// install against the mock server
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config, None);
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	// the binary is extracted into the repo/tag directory
	let installed = fixture.config_dir.join("tools").join("github_owner_repo/1.0.0").join(binary);
	assert_eq!(std::fs::read(&installed).unwrap(), b"binary 1.0.0".as_slice());
	// the lock records the resolved tag and install path
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	let expected_lock = r#"version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "github_owner_repo/1.0.0"
"#;
	assert_eq!(lock, expected_lock);
	// the binary is indexed and a shim is generated for it
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	#[cfg(windows)]
	let expected_state = r#"version = 1
registered_paths = []

[binaries]
"github_owner_repo/1.0.0" = [
    "tool.exe",
    'bin\helper.exe',
]

[[shims.helper]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
install_path = "github_owner_repo/1.0.0"
binary = 'bin\helper.exe'

[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
install_path = "github_owner_repo/1.0.0"
binary = "tool.exe"
"#;
	#[cfg(not(windows))]
	let expected_state = r#"version = 1
registered_paths = []

[binaries]
"github_owner_repo/1.0.0" = [
    "tool",
    "bin/helper",
]
"#;
	assert_eq!(state, expected_state);
	let shim = fixture.config_dir.join("shims").join(binary);
	assert!(shim.exists());
	// the first tag candidate resolves, so the fallback is never requested
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec![
		"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		format!("/download/1.0.0/{asset}"),
	]);
}

#[test]
fn installs_two_versions_of_same_tool() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
	runtime.block_on(_mock_release(&server, "2.0.0"));
	let one = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let two = r#"[tools]
"github:owner/repo" = { version = "2.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&one, None);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	assert!(stdout.contains("owner/repo v2.0.0"), "{stdout}");
	// each version is extracted into its own directory
	let tools = fixture.config_dir.join("tools").join("github_owner_repo");
	assert_eq!(std::fs::read(tools.join("1.0.0").join(binary)).unwrap(), b"binary 1.0.0".as_slice());
	assert_eq!(std::fs::read(tools.join("2.0.0").join(binary)).unwrap(), b"binary 2.0.0".as_slice());
	// the lock carries a row per requested version
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	let expected_lock = r#"version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "github_owner_repo/1.0.0"

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "2.0.0"
tag_name = "v2.0.0"
install_path = "github_owner_repo/2.0.0"
"#;
	assert_eq!(lock, expected_lock);
	// both releases are resolved and downloaded
	let mut paths: Vec<String> = runtime
		.block_on(server.received_requests())
		.unwrap()
		.iter()
		.map(|request| request.url.path().to_owned())
		.collect();
	paths.sort();
	assert_eq!(paths, vec![
		format!("/download/1.0.0/{asset}"),
		format!("/download/2.0.0/{asset}"),
		"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		"/repos/owner/repo/releases/tags/2.0.0".to_owned(),
	]);
}

#[test]
fn installs_same_version_from_two_origins() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, _) = runtime.block_on(_mock_release(&server, "1.0.0"));
	let one = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let two = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&one, None);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	// both origins merge into one request, so one artifact is installed
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert_eq!(stdout.lines().count(), 1, "{stdout}");
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	let expected_lock = r#"version = 1

[[tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
tag_name = "v1.0.0"
install_path = "github_owner_repo/1.0.0"
"#;
	assert_eq!(lock, expected_lock);
	// the tool is resolved and downloaded once
	let mut paths: Vec<String> = runtime
		.block_on(server.received_requests())
		.unwrap()
		.iter()
		.map(|request| request.url.path().to_owned())
		.collect();
	paths.sort();
	assert_eq!(paths, vec![
		format!("/download/1.0.0/{asset}"),
		"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
	]);
}

#[test]
fn reports_failed_install_per_origin() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config, None);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), config).unwrap();
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("owner/repo 1.0.0: no release found"), "{stderr}");
	// a failed resolve names every config that requested the tool
	let system = fixture.config_dir.join("outpost.toml").display().to_string();
	let dev = fixture.config_dir.join("outpost.dev.toml").display().to_string();
	assert!(stderr.contains(&format!("  from {system}")), "{stderr}");
	assert!(stderr.contains(&format!("  from {dev}")), "{stderr}");
	// both tag candidates are tried, and nothing is installed
	let mut paths: Vec<String> = runtime
		.block_on(server.received_requests())
		.unwrap()
		.iter()
		.map(|request| request.url.path().to_owned())
		.collect();
	paths.sort();
	assert_eq!(paths, vec![
		"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		"/repos/owner/repo/releases/tags/v1.0.0".to_owned(),
	]);
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	assert!(!lock.contains("[[tool]]"), "{lock}");
	assert_eq!(std::fs::read_dir(fixture.config_dir.join("tools")).unwrap().count(), 0);
}

mod exposed {
	use indoc::formatdoc;

	use super::*;

	#[test]
	fn exposes_only_top_level_binaries() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["*"] }}
		"#,
		api_url = server.uri()
		};
		let fixture = Fixture::new(&config, None);
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// `*` does not cross `/`, so the nested helper stays hidden
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		#[cfg(windows)]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = ["{binary}"]

		[[shims.tool]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = "{binary}"
		"#};
		#[cfg(not(windows))]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = ["{binary}"]
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(binary).exists());
		assert!(!fixture.config_dir.join("shims").join(helper).exists());
	}

	#[test]
	fn exposes_nested_binaries() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["**/{exposed}"] }}
		"#,
		api_url = server.uri(),
		exposed = helper
		};
		let fixture = Fixture::new(&config, None);
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// '**' crosses '/', so the nested helper is exposed and the tool is not
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		#[cfg(windows)]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = ['bin\helper.exe']

		[[shims.helper]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = 'bin\helper.exe'
		"#};
		#[cfg(not(windows))]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = ["bin/helper"]
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(helper).exists());
		assert!(!fixture.config_dir.join("shims").join(binary).exists());
	}

	#[test]
	fn exposes_binaries_from_every_request() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		// both requests resolve to the same artifact
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["{binary}"] }}
		"#,
		api_url = server.uri()
		};
		let env = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "v1.0.0", api_url = "{api_url}", exposed = ["bin/{helper}"] }}
		"#,
		api_url = server.uri()
		};
		let fixture = Fixture::new(&config, Some(&env));
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// both allowlists apply to the shared artifact
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		#[cfg(windows)]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = [
		    "{binary}",
		    'bin\helper.exe',
		]

		[[shims.helper]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = 'bin\helper.exe'

		[[shims.helper]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = 'bin\helper.exe'

		[[shims.tool]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = "{binary}"

		[[shims.tool]]
		backend = "github"
		full_name = "owner/repo"
		lane = ""
		requested = "v1.0.0"
		install_path = "github_owner_repo/1.0.0"
		binary = "{binary}"
		"#};
		#[cfg(not(windows))]
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"github_owner_repo/1.0.0" = [
		    "{binary}",
		    "bin/helper",
		]
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(binary).exists());
		assert!(fixture.config_dir.join("shims").join(helper).exists());
	}
}
