use std::path::PathBuf;
use std::process::{Command, Output};

use flate2::Compression;
use flate2::write::GzEncoder;
use indoc::formatdoc;
use serde_json::json;
use sha2::{Digest, Sha256};
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
		let mut command = Command::new(env!("CARGO_BIN_EXE_op"));
		command.arg("install").current_dir(&self.config_dir).env_clear();
		#[cfg(windows)]
		if let Some(value) = std::env::var_os("SystemRoot") {
			command.env("SystemRoot", value);
		}
		// coverage runs write profiles to the path this names
		if let Some(value) = std::env::var_os("LLVM_PROFILE_FILE") {
			command.env("LLVM_PROFILE_FILE", value);
		}
		command
			.env("OUTPOST_CONFIG_DIR", &self.config_dir)
			.env("OUTPOST_DATA_DIR", &self.config_dir)
			.env("OUTPOST_ENV", "dev");
		command.output().unwrap()
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

fn _release_data(server: &MockServer, version: &str) -> (serde_json::Value, Vec<u8>, String, &'static str) {
	// pick an asset name that the selector accepts on this host
	let libc = if cfg!(windows) { "msvc" } else { "gnu" };
	let asset = format!("tool-{}-{}-{libc}.tar.gz", std::env::consts::OS, std::env::consts::ARCH);
	let binary = if cfg!(windows) { "tool.exe" } else { "tool" };
	let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
	let nested = format!("bin/{helper}");
	let content = format!("binary {version}");
	let bytes = _tarball(&[(binary, content.as_bytes()), (&nested, content.as_bytes())]);
	let release = json!({
		"tag_name": format!("v{version}"),
		"draft": false,
		"prerelease": false,
		"assets": [{
			"name": asset,
			"browser_download_url": format!("{}/download/{version}/{asset}", server.uri()),
			"size": bytes.len(),
		}],
	});
	(release, bytes, asset, binary)
}

async fn _mock_release(server: &MockServer, version: &str) -> (String, &'static str) {
	let (release, bytes, asset, binary) = _release_data(server, version);
	Mock::given(method("GET"))
		.and(path(format!("/repos/owner/repo/releases/tags/{version}")))
		.respond_with(ResponseTemplate::new(200).set_body_json(release.clone()))
		.mount(server)
		.await;
	Mock::given(method("GET"))
		.and(path("/repos/owner/repo/releases/latest"))
		.respond_with(ResponseTemplate::new(200).set_body_json(release.clone()))
		.mount(server)
		.await;
	Mock::given(method("GET"))
		.and(path("/repos/owner/repo/releases"))
		.respond_with(ResponseTemplate::new(200).set_body_json(json!([release])))
		.mount(server)
		.await;
	Mock::given(method("GET"))
		.and(path(format!("/download/{version}/{asset}")))
		.respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
		.mount(server)
		.await;
	(asset, binary)
}

fn _install_path(api_url: &str, repo: &str, version: &str) -> String {
	let suffix = if api_url.is_empty() {
		String::new()
	} else {
		let digest = Sha256::digest(api_url.as_bytes());
		let hash: String = digest.iter().take(4).map(|byte| format!("{byte:02x}")).collect();
		format!("_{hash}")
	};
	format!("github_{repo}{suffix}/{version}")
}

fn _resolution_cache_path(api_url: &str, repo: &str) -> String {
	let install_path = _install_path(api_url, repo, "_");
	format!("{}.toml", install_path.split('/').next().unwrap())
}

#[test]
fn skips_installed_tools() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let api_url = server.uri();
	let (asset, _) = runtime.block_on(_mock_release(&server, "1.0.0"));
	let config = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
	"#};
	let fixture = Fixture::new(&config, None);
	let first = fixture.run();
	assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
	let second = fixture.run();
	assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
	// the lock row and installed dir make the second run a no-op
	assert!(second.stdout.is_empty(), "{}", String::from_utf8_lossy(&second.stdout));
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec!["/repos/owner/repo/releases/tags/1.0.0".to_owned(), format!("/download/1.0.0/{asset}"),]);
}

#[test]
fn installs_github_release_asset() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let api_url = server.uri();
	let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
	// install against the mock server
	let config = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
	"#};
	let fixture = Fixture::new(&config, None);
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	// the binary is extracted into the origin/repo/tag directory
	let install_path = _install_path(&api_url, "owner_repo", "1.0.0");
	let installed = fixture.config_dir.join("tools").join(&install_path).join(binary);
	assert_eq!(std::fs::read(&installed).unwrap(), b"binary 1.0.0".as_slice());
	// the lock records the origin, resolved tag and install path
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	let expected_lock = formatdoc! {r#"
	version = 1

	[[tool]]
	backend = "github"
	api_url = "{api_url}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.0.0"
	tag_name = "v1.0.0"
	install_path = "{install_path}"
	"#};
	assert_eq!(lock, expected_lock);
	// the binary is indexed and a shim is generated for it
	let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
	let helper_toml = if cfg!(windows) { "'bin\\helper.exe'" } else { "\"bin/helper\"" };
	let expected_state = formatdoc! {r#"
	version = 1
	registered_paths = []

	[binaries]
	"{install_path}" = [
	    "{binary}",
	    {helper_toml},
	]

	[[shims.helper]]
	backend = "github"
	api_url = "{api_url}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.0.0"
	install_path = "{install_path}"
	binary = {helper_toml}

	[[shims.tool]]
	backend = "github"
	api_url = "{api_url}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.0.0"
	install_path = "{install_path}"
	binary = "{binary}"
	"#};
	assert_eq!(state, expected_state);
	let shim = fixture.config_dir.join("shims").join(binary);
	assert!(shim.exists());
	// the first tag candidate resolves, so the fallback is never requested
	let requests = runtime.block_on(server.received_requests()).unwrap();
	let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
	assert_eq!(paths, vec!["/repos/owner/repo/releases/tags/1.0.0".to_owned(), format!("/download/1.0.0/{asset}"),]);
}

#[test]
fn installs_two_versions_of_same_tool() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let api_url = server.uri();
	let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
	runtime.block_on(_mock_release(&server, "2.0.0"));
	let one = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
	"#};
	let two = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "2.0.0", api_url = "{api_url}" }}
	"#};
	let fixture = Fixture::new(&one, None);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(output.status.success(), "{stderr}");
	let stdout = String::from_utf8(output.stdout).unwrap();
	assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
	assert!(stdout.contains("owner/repo v2.0.0"), "{stdout}");
	// each version is extracted into its own directory
	let one_path = _install_path(&api_url, "owner_repo", "1.0.0");
	let two_path = _install_path(&api_url, "owner_repo", "2.0.0");
	let tools = fixture.config_dir.join("tools");
	assert_eq!(std::fs::read(tools.join(&one_path).join(binary)).unwrap(), b"binary 1.0.0".as_slice());
	assert_eq!(std::fs::read(tools.join(&two_path).join(binary)).unwrap(), b"binary 2.0.0".as_slice());
	// the lock carries a row per requested version
	let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
	let expected_lock = formatdoc! {r#"
	version = 1

	[[tool]]
	backend = "github"
	api_url = "{api_url}"
	full_name = "owner/repo"
	lane = ""
	requested = "1.0.0"
	tag_name = "v1.0.0"
	install_path = "{one_path}"

	[[tool]]
	backend = "github"
	api_url = "{api_url}"
	full_name = "owner/repo"
	lane = ""
	requested = "2.0.0"
	tag_name = "v2.0.0"
	install_path = "{two_path}"
	"#};
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
fn reports_failed_install_per_origin() {
	let runtime = tokio::runtime::Runtime::new().unwrap();
	let server = runtime.block_on(MockServer::start());
	let api_url = server.uri();
	let config = formatdoc! {r#"
	[tools]
	"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
	"#};
	let fixture = Fixture::new(&config, None);
	std::fs::write(fixture.config_dir.join("outpost.dev.toml"), config).unwrap();
	let output = fixture.run();
	let stderr = String::from_utf8(output.stderr).unwrap();
	assert!(!output.status.success(), "{stderr}");
	assert!(stderr.contains("owner/repo 1.0.0: no release found"), "{stderr}");
	// a failed resolve names every config that requested the tool
	let system = std::fs::canonicalize(fixture.config_dir.join("outpost.toml")).unwrap().display().to_string();
	let dev = std::fs::canonicalize(fixture.config_dir.join("outpost.dev.toml"))
		.unwrap()
		.display()
		.to_string();
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

mod resolution_cache {
	use std::path::Path;

	use regex::regex;

	use super::*;

	fn _expire_resolution_cache(path: &Path) {
		let cache = std::fs::read_to_string(path).unwrap();
		let cache = regex!(r"(?m)^resolved_at = \d+$").replace_all(&cache, "resolved_at = 0");
		std::fs::write(path, cache.as_bytes()).unwrap();
	}

	#[test]
	fn reinstalls_from_cached_resolution() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		for (requested, resolution_path) in [
			("1.0.0", "/repos/owner/repo/releases/tags/1.0.0"),
			("latest", "/repos/owner/repo/releases/latest"),
			("1", "/repos/owner/repo/releases"),
		] {
			let server = runtime.block_on(MockServer::start());
			let api_url = server.uri();
			let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
			let config = formatdoc! {r#"
			[tools]
			"github:owner/repo" = {{ version = "{requested}", api_url = "{api_url}" }}
			"#};
			let fixture = Fixture::new(&config, None);
			// the first install resolves and downloads
			let first = fixture.run();
			assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
			let install_path = _install_path(&api_url, "owner_repo", "1.0.0");
			let installed = fixture.config_dir.join("tools").join(&install_path);
			std::fs::remove_dir_all(&installed).unwrap();
			// the second install reuses the cached resolution and downloads again
			let second = fixture.run();
			assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
			assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
			let requests = runtime.block_on(server.received_requests()).unwrap();
			let paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
			// resolve once and download twice for every version form
			assert_eq!(
				paths,
				vec![
					resolution_path.to_owned(),
					format!("/download/1.0.0/{asset}"),
					format!("/download/1.0.0/{asset}"),
				],
				"{requested}"
			);
		}
	}

	#[test]
	fn refreshes_stale_resolutions() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		for (requested, resolution_path) in [
			("1.0.0", "/repos/owner/repo/releases/tags/1.0.0"),
			("latest", "/repos/owner/repo/releases/latest"),
			("1", "/repos/owner/repo/releases"),
		] {
			let server = runtime.block_on(MockServer::start());
			let api_url = server.uri();
			let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
			let config = formatdoc! {r#"
			[tools]
			"github:owner/repo" = {{ version = "{requested}", api_url = "{api_url}" }}
			"#};
			let fixture = Fixture::new(&config, None);
			// the first install writes the cache
			let first = fixture.run();
			assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
			let cache_path = fixture.config_dir.join("resolutions").join(_resolution_cache_path(&api_url, "owner_repo"));
			_expire_resolution_cache(&cache_path);
			assert!(std::fs::read_to_string(&cache_path).unwrap().contains("resolved_at = 0"));
			let installed = fixture.config_dir.join("tools").join(_install_path(&api_url, "owner_repo", "1.0.0"));
			std::fs::remove_dir_all(&installed).unwrap();
			// the expired entry forces a new resolution
			let second = fixture.run();
			assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
			assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
			assert!(!std::fs::read_to_string(&cache_path).unwrap().contains("resolved_at = 0"));
			std::fs::remove_dir_all(&installed).unwrap();
			// the refreshed entry serves the third install
			let third = fixture.run();
			assert!(third.status.success(), "{}", String::from_utf8_lossy(&third.stderr));
			assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
			let requests = runtime.block_on(server.received_requests()).unwrap();
			let mut paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
			paths.sort();
			// the stale entry refreshes once and every install downloads
			assert_eq!(paths, vec![
				format!("/download/1.0.0/{asset}"),
				format!("/download/1.0.0/{asset}"),
				format!("/download/1.0.0/{asset}"),
				resolution_path.to_owned(),
				resolution_path.to_owned(),
			]);
		}
	}

	#[test]
	fn combines_cached_and_fresh_resolutions() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		runtime.block_on(_mock_release(&server, "2.0.0"));
		let one = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
		"#};
		let two = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "2.0.0", api_url = "{api_url}" }}
		"#};
		let fixture = Fixture::new(&one, None);
		// the first install caches 1.0.0
		let first = fixture.run();
		assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
		let one_installed = fixture.config_dir.join("tools").join(_install_path(&api_url, "owner_repo", "1.0.0"));
		std::fs::remove_dir_all(&one_installed).unwrap();
		// the second install reuses 1.0.0 and resolves 2.0.0
		let second = fixture.run();
		assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
		let tools = fixture.config_dir.join("tools");
		let one_installed = tools.join(_install_path(&api_url, "owner_repo", "1.0.0"));
		let two_installed = tools.join(_install_path(&api_url, "owner_repo", "2.0.0"));
		assert_eq!(std::fs::read(one_installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		assert_eq!(std::fs::read(two_installed.join(binary)).unwrap(), b"binary 2.0.0".as_slice());
		std::fs::remove_dir_all(&one_installed).unwrap();
		std::fs::remove_dir_all(&two_installed).unwrap();
		// the third install reuses both entries from one cache file
		let third = fixture.run();
		assert!(third.status.success(), "{}", String::from_utf8_lossy(&third.stderr));
		assert_eq!(std::fs::read(one_installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		assert_eq!(std::fs::read(two_installed.join(binary)).unwrap(), b"binary 2.0.0".as_slice());
		let requests = runtime.block_on(server.received_requests()).unwrap();
		let mut paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
		paths.sort();
		// each version resolves once and downloads on every install
		assert_eq!(paths, vec![
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			format!("/download/2.0.0/{asset}"),
			format!("/download/2.0.0/{asset}"),
			"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
			"/repos/owner/repo/releases/tags/2.0.0".to_owned(),
		]);
	}

	#[test]
	fn rejects_cached_resolution_for_another_target() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
		"#};
		let fixture = Fixture::new(&config, None);
		// the first install writes the cache for the current target
		let first = fixture.run();
		assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
		let cache_path = fixture.config_dir.join("resolutions").join(_resolution_cache_path(&api_url, "owner_repo"));
		let cache = std::fs::read_to_string(&cache_path).unwrap();
		let cache = cache.replacen(&format!("os = \"{}\"", std::env::consts::OS), "os = \"other\"", 1);
		assert!(cache.contains("os = \"other\""));
		std::fs::write(&cache_path, cache).unwrap();
		let installed = fixture.config_dir.join("tools").join(_install_path(&api_url, "owner_repo", "1.0.0"));
		std::fs::remove_dir_all(&installed).unwrap();
		// the modified target forces a new resolution and adds the current target entry
		let second = fixture.run();
		assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
		assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		let cache = std::fs::read_to_string(&cache_path).unwrap();
		assert!(cache.contains("os = \"other\""));
		assert!(cache.contains(&format!("os = \"{}\"", std::env::consts::OS)));
		assert_eq!(cache.matches("[[resolution]]").count(), 2);
		std::fs::remove_dir_all(&installed).unwrap();
		// the current target entry serves the third install
		let third = fixture.run();
		assert!(third.status.success(), "{}", String::from_utf8_lossy(&third.stderr));
		assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		let requests = runtime.block_on(server.received_requests()).unwrap();
		let mut paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
		paths.sort();
		// three installs download three times and resolve twice
		assert_eq!(paths, vec![
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
			"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		]);
	}

	#[test]
	fn recovers_from_corrupted_cache() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (asset, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}" }}
		"#};
		let fixture = Fixture::new(&config, None);
		// the first install writes the cache
		let first = fixture.run();
		assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
		let cache_path = fixture.config_dir.join("resolutions").join(_resolution_cache_path(&api_url, "owner_repo"));
		assert!(cache_path.is_file());
		std::fs::write(&cache_path, "not toml").unwrap();
		let installed = fixture.config_dir.join("tools").join(_install_path(&api_url, "owner_repo", "1.0.0"));
		std::fs::remove_dir_all(&installed).unwrap();
		// corruption forces a new resolution and rewrites the cache
		let second = fixture.run();
		assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
		assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		std::fs::remove_dir_all(&installed).unwrap();
		// the repaired cache serves the third install
		let third = fixture.run();
		assert!(third.status.success(), "{}", String::from_utf8_lossy(&third.stderr));
		assert_eq!(std::fs::read(installed.join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		let requests = runtime.block_on(server.received_requests()).unwrap();
		let mut paths: Vec<String> = requests.iter().map(|request| request.url.path().to_owned()).collect();
		paths.sort();
		// three installs download three times and resolve twice
		assert_eq!(paths, vec![
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			format!("/download/1.0.0/{asset}"),
			"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
			"/repos/owner/repo/releases/tags/1.0.0".to_owned(),
		]);
	}
}

mod exposed {
	use super::*;

	#[test]
	fn exposes_only_top_level_binaries() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let install_path = _install_path(&api_url, "owner_repo", "1.0.0");
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["*"] }}
		"#};
		let fixture = Fixture::new(&config, None);
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// `*` does not cross `/`, so the nested helper stays hidden
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"{install_path}" = ["{binary}"]

		[[shims.tool]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{install_path}"
		binary = "{binary}"
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(binary).exists());
		assert!(!fixture.config_dir.join("shims").join(helper).exists());
	}

	#[test]
	fn exposes_nested_binaries() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let install_path = _install_path(&api_url, "owner_repo", "1.0.0");
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["**/{helper}"] }}
		"#};
		let fixture = Fixture::new(&config, None);
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// '**' crosses '/', so the nested helper is exposed and the tool is not
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		let nested_binary = if cfg!(windows) {
			format!("'bin\\{helper}'")
		} else {
			format!("\"bin/{helper}\"")
		};
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"{install_path}" = [{nested_binary}]

		[[shims.helper]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{install_path}"
		binary = {nested_binary}
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(helper).exists());
		assert!(!fixture.config_dir.join("shims").join(binary).exists());
	}

	#[test]
	fn exposes_binaries_from_every_request() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let server = runtime.block_on(MockServer::start());
		let api_url = server.uri();
		let (_, binary) = runtime.block_on(_mock_release(&server, "1.0.0"));
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let install_path = _install_path(&api_url, "owner_repo", "1.0.0");
		// both requests resolve to the same artifact
		let config = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{api_url}", exposed = ["{binary}"] }}
		"#};
		let env = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "v1.0.0", api_url = "{api_url}", exposed = ["bin/{helper}"] }}
		"#};
		let fixture = Fixture::new(&config, Some(&env));
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// both allowlists apply to the shared artifact
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		let nested_binary = if cfg!(windows) {
			format!("'bin\\{helper}'")
		} else {
			format!("\"bin/{helper}\"")
		};
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"{install_path}" = [
		    "{binary}",
		    {nested_binary},
		]

		[[shims.helper]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{install_path}"
		binary = {nested_binary}

		[[shims.helper]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "v1.0.0"
		install_path = "{install_path}"
		binary = {nested_binary}

		[[shims.tool]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{install_path}"
		binary = "{binary}"

		[[shims.tool]]
		backend = "github"
		api_url = "{api_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "v1.0.0"
		install_path = "{install_path}"
		binary = "{binary}"
		"#};
		assert_eq!(state, expected);
		assert!(fixture.config_dir.join("shims").join(binary).exists());
		assert!(fixture.config_dir.join("shims").join(helper).exists());
	}
}

mod rename_shims {
	use super::*;

	#[test]
	fn installs_same_version_from_two_origins() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let one_server = runtime.block_on(MockServer::start());
		let two_server = runtime.block_on(MockServer::start());
		let one_url = one_server.uri();
		let two_url = two_server.uri();
		let (asset, binary) = runtime.block_on(_mock_release(&one_server, "1.0.0"));
		runtime.block_on(_mock_release(&two_server, "1.0.0"));
		let one = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{one_url}" }}
		"#};
		let two = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{two_url}", rename_shims = {{ "tool*" = "tool-two", "**/helper*" = "helper-two" }} }}
		"#};
		let fixture = Fixture::new(&one, None);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(output.status.success(), "{stderr}");
		// origins do not merge, so both artifacts are installed
		let stdout = String::from_utf8(output.stdout).unwrap();
		assert_eq!(stdout.lines().count(), 2, "{stdout}");
		assert!(stdout.contains("owner/repo v1.0.0"), "{stdout}");
		let one_path = _install_path(&one_url, "owner_repo", "1.0.0");
		let two_path = _install_path(&two_url, "owner_repo", "1.0.0");
		let tools = fixture.config_dir.join("tools");
		assert_eq!(std::fs::read(tools.join(&one_path).join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		assert_eq!(std::fs::read(tools.join(&two_path).join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		// the lock carries a row per origin
		let lock = std::fs::read_to_string(fixture.config_dir.join("outpost.lock")).unwrap();
		assert_eq!(lock.matches("[[tool]]").count(), 2, "{lock}");
		for (api_url, install_path) in [(&one_url, &one_path), (&two_url, &two_path)] {
			let row = formatdoc! {r#"
			api_url = "{api_url}"
			full_name = "owner/repo"
			lane = ""
			requested = "1.0.0"
			tag_name = "v1.0.0"
			install_path = "{install_path}"
			"#};
			assert!(lock.contains(&row), "{lock}");
		}
		let state = std::fs::read_to_string(fixture.config_dir.join("state.toml")).unwrap();
		let helper = if cfg!(windows) { "helper.exe" } else { "helper" };
		let nested_binary = if cfg!(windows) {
			format!("'bin\\{helper}'")
		} else {
			format!("\"bin/{helper}\"")
		};
		let (first_path, second_path) = if one_path < two_path {
			(&one_path, &two_path)
		} else {
			(&two_path, &one_path)
		};
		let expected = formatdoc! {r#"
		version = 1
		registered_paths = []

		[binaries]
		"{first_path}" = [
		    "{binary}",
		    {nested_binary},
		]
		"{second_path}" = [
		    "{binary}",
		    {nested_binary},
		]

		[[shims.helper]]
		backend = "github"
		api_url = "{one_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{one_path}"
		binary = {nested_binary}

		[[shims.helper-two]]
		backend = "github"
		api_url = "{two_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{two_path}"
		binary = {nested_binary}

		[[shims.tool]]
		backend = "github"
		api_url = "{one_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{one_path}"
		binary = "{binary}"

		[[shims.tool-two]]
		backend = "github"
		api_url = "{two_url}"
		full_name = "owner/repo"
		lane = ""
		requested = "1.0.0"
		install_path = "{two_path}"
		binary = "{binary}"
		"#};
		assert_eq!(state, expected);
		let suffix = if cfg!(windows) { ".exe" } else { "" };
		for name in ["tool", "tool-two", "helper", "helper-two"] {
			let path = fixture.config_dir.join("shims").join(format!("{name}{suffix}"));
			assert!(path.is_file(), "{}", path.display());
		}
		// each origin resolves and downloads its own artifact
		let mut paths: Vec<String> = runtime
			.block_on(one_server.received_requests())
			.unwrap()
			.iter()
			.map(|request| request.url.path().to_owned())
			.collect();
		paths.sort();
		assert_eq!(paths, vec![format!("/download/1.0.0/{asset}"), "/repos/owner/repo/releases/tags/1.0.0".to_owned(),]);
		let mut paths: Vec<String> = runtime
			.block_on(two_server.received_requests())
			.unwrap()
			.iter()
			.map(|request| request.url.path().to_owned())
			.collect();
		paths.sort();
		assert_eq!(paths, vec![format!("/download/1.0.0/{asset}"), "/repos/owner/repo/releases/tags/1.0.0".to_owned(),]);
	}

	#[test]
	fn rejects_two_origins_with_one_shim_name() {
		let runtime = tokio::runtime::Runtime::new().unwrap();
		let one_server = runtime.block_on(MockServer::start());
		let two_server = runtime.block_on(MockServer::start());
		let one_url = one_server.uri();
		let two_url = two_server.uri();
		let (_, binary) = runtime.block_on(_mock_release(&one_server, "1.0.0"));
		runtime.block_on(_mock_release(&two_server, "1.0.0"));
		let one = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{one_url}" }}
		"#};
		let two = formatdoc! {r#"
		[tools]
		"github:owner/repo" = {{ version = "1.0.0", api_url = "{two_url}" }}
		"#};
		let fixture = Fixture::new(&one, None);
		std::fs::write(fixture.config_dir.join("outpost.dev.toml"), two).unwrap();
		let output = fixture.run();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert!(!output.status.success(), "{stderr}");
		assert!(stderr.contains("conflicting binaries, cannot generate shims:"), "{stderr}");
		assert!(stderr.contains("tool"), "{stderr}");
		// the artifacts are installed even though the shim name conflicts
		let one_path = _install_path(&one_url, "owner_repo", "1.0.0");
		let two_path = _install_path(&two_url, "owner_repo", "1.0.0");
		let tools = fixture.config_dir.join("tools");
		assert_eq!(std::fs::read(tools.join(&one_path).join(binary)).unwrap(), b"binary 1.0.0".as_slice());
		assert_eq!(std::fs::read(tools.join(&two_path).join(binary)).unwrap(), b"binary 1.0.0".as_slice());
	}
}
