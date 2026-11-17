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
	fn new(config: &str) -> Self {
		let dir = tempfile::tempdir().unwrap();
		let config_dir = dir.path().join("config");
		std::fs::create_dir_all(&config_dir).unwrap();
		std::fs::write(config_dir.join("outpost.toml"), config).unwrap();
		Self { _dir: dir, config_dir }
	}

	fn run(&self) -> Output {
		Command::new(env!("CARGO_BIN_EXE_op"))
			.arg("install")
			.current_dir(&self.config_dir)
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env_remove("OUTPOST_ENV")
			.env_remove("OUTPOST_TOOLS_DIR")
			.env_remove("OUTPOST_SHIMS_DIR")
			.env_remove("OUTPOST_DOWNLOADS_DIR")
			.env_remove("GH_TOKEN")
			.env_remove("GITHUB_TOKEN")
			.output()
			.unwrap()
	}
}

fn _tarball(name: &str, bytes: &[u8]) -> Vec<u8> {
	let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
	let mut header = tar::Header::new_gnu();
	header.set_path(name).unwrap();
	header.set_size(bytes.len() as u64);
	header.set_mode(0o755);
	header.set_cksum();
	builder.append(&header, bytes).unwrap();
	builder.into_inner().unwrap().finish().unwrap()
}

async fn _mock_release(server: &MockServer) -> (String, &'static str) {
	// pick an asset name that the selector accepts on this host
	let libc = if cfg!(windows) { "msvc" } else { "gnu" };
	let asset = format!("tool-{}-{}-{libc}.tar.gz", std::env::consts::OS, std::env::consts::ARCH);
	let binary = if cfg!(windows) { "tool.exe" } else { "tool" };
	let bytes = _tarball(binary, b"binary");
	Mock::given(method("GET"))
		.and(path("/repos/owner/repo/releases/tags/1.0.0"))
		.respond_with(ResponseTemplate::new(200).set_body_json(json!({
			"tag_name": "v1.0.0",
			"draft": false,
			"prerelease": false,
			"assets": [{
				"name": asset,
				"browser_download_url": format!("{}/download/{asset}", server.uri()),
				"size": bytes.len(),
			}],
		})))
		.mount(server)
		.await;
	Mock::given(method("GET"))
		.and(path(format!("/download/{asset}")))
		.respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
		.mount(server)
		.await;
	(asset, binary)
}

#[test]
fn resolves_against_api_url() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config);
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("owner/repo 1.0.0: no release found"), "{stderr}");
	// exact versions are tried with and without the "v" prefix
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
	assert_eq!(paths, vec!["/repos/owner/repo/releases/tags/1.0.0", "/repos/owner/repo/releases/tags/v1.0.0"]);
	// the lock is rewritten, but nothing is installed
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	assert!(!lock.contains("[[tool]]"), "{lock}");
	assert_eq!(std::fs::read_dir(fixture.config_dir.join("tools")).unwrap().count(), 0);
}

#[test]
fn skips_installed_tools() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, _) = runtime.block_on(_mock_release(&server));
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config);
	let first = fixture.run();
	assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
	let second = fixture.run();
	assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
	// the lock row and installed dir make the second run a no-op
	assert!(second.stdout.is_empty(), "{}", String::from_utf8_lossy(&second.stdout));
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec!["/repos/owner/repo/releases/tags/1.0.0".to_owned(), format!("/download/{asset}"),]);
}

#[test]
fn installs_github_release_asset() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let (asset, binary) = runtime.block_on(_mock_release(&server));
	// install against the mock server
	let config = r#"[tools]
"github:owner/repo" = { version = "1.0.0", api_url = "{api_url}" }
"#
	.replace("{api_url}", &server.uri());
	let fixture = Fixture::new(&config);
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	// the binary is extracted into the repo/tag directory
	let installed = fixture.config_dir.join("tools").join("github_owner_repo/1.0.0").join(binary);
	assert_eq!(std::fs::read(&installed).unwrap(), b"binary".as_slice());
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
"github_owner_repo/1.0.0" = ["tool.exe"]

[[shims.tool]]
backend = "github"
full_name = "owner/repo"
lane = ""
requested = "1.0.0"
install_path = "github_owner_repo/1.0.0"
binaries = ["tool.exe"]
"#;
	#[cfg(not(windows))]
	let expected_state = r#"version = 1
registered_paths = []

[binaries]
"github_owner_repo/1.0.0" = ["tool"]
"#;
	assert_eq!(state, expected_state);
	let shim = fixture.config_dir.join("shims").join(binary);
	assert!(shim.exists());
	// the first tag candidate resolves, so the fallback is never requested
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec!["/repos/owner/repo/releases/tags/1.0.0".to_owned(), format!("/download/{asset}"),]);
}
