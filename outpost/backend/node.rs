use std::path::PathBuf;
use std::{env, fmt};

use reqwest::header::CONTENT_LENGTH;
use reqwest::{Client, StatusCode};
use semver::Version as Semver;
use serde::Deserialize;

use super::_asset_selection::{Target, select_with};
use super::github::tag_component;
use crate::tools::archive::{NodeResolution, ToolResolution};
use crate::tools::tool_request::NodeRequest;
use crate::tools::version::{Version, tag_version};

pub(crate) struct NodeBackend {
	client: NodeClient,
}

impl NodeBackend {
	pub(crate) fn new(http: Client, options: NodeOptions) -> Self {
		Self {
			client: NodeClient::new(http, options),
		}
	}

	pub(crate) async fn resolve(&self, req: &NodeRequest) -> Result<ToolResolution, NodeError> {
		let releases: Vec<Release> = serde_json::from_str(&self.client.index().await?)?;
		let release = _pick_release(releases, &req.version).ok_or_else(|| NodeError::NoMatchingVersion(req.version.to_string()))?;
		let shasums = self.client.shasums(&release.version).await?;
		// each line holds a sha256 then a file name
		let entries: Vec<(&str, &str)> = shasums
			.lines()
			.filter_map(|line| {
				let mut parts = line.split_whitespace();
				Some((parts.next()?, parts.next()?))
			})
			.collect();
		let target = &self.client.options.target;
		let Some((index, name, format)) = select_with(entries.iter().map(|(_, name)| *name), target) else {
			return Err(NodeError::UnsupportedPlatform {
				version: release.version,
				os: target.os.to_owned(),
				arch: target.arch.to_owned(),
			});
		};
		let url = self.client.archive_url(&release.version, name);
		let size = self.client.size(&url).await?;
		Ok(ToolResolution::Node(NodeResolution {
			version: release.version,
			url,
			size,
			format,
			sha256: entries[index].0.to_owned(),
		}))
	}
}

fn _pick_release(releases: Vec<Release>, version: &Version) -> Option<Release> {
	let mut releases = releases.into_iter();
	match version {
		Version::Latest => _pick_highest(releases, |release| tag_version(&release.version)),
		Version::Prefix(prefix) => _pick_highest(releases, |release| prefix.matches(&release.version)),
		Version::Exact(exact) => {
			let exact = exact.strip_prefix('v').unwrap_or(exact);
			releases.find(|release| release.version.strip_prefix('v').unwrap_or(&release.version) == exact)
		}
	}
}

// prereleases install only by exact version
fn _pick_highest(releases: impl Iterator<Item = Release>, parse: impl Fn(&Release) -> Option<Semver>) -> Option<Release> {
	let mut best: Option<(Semver, Release)> = None;
	for release in releases {
		let Some(version) = parse(&release) else {
			continue;
		};
		if !version.pre.is_empty() {
			continue;
		}
		if best.as_ref().is_none_or(|(best, _)| version > *best) {
			best = Some((version, release));
		}
	}
	best.map(|(_, release)| release)
}

pub(crate) fn install_path(version: &str) -> PathBuf {
	PathBuf::from(format!("core_node/{}", tag_component(version)))
}

// one entry of index.json
#[derive(Deserialize)]
pub(crate) struct Release {
	pub(crate) version: String,
}

#[derive(Clone)]
pub(crate) struct NodeOptions {
	dist_url: String,
	target: Target,
}

impl NodeOptions {
	pub(crate) fn from_process() -> Self {
		let dist_url = env::var("OUTPOST_NODE_DIST_URL")
			.map(|url| url.trim().trim_end_matches('/').to_owned())
			.ok()
			.filter(|url| !url.is_empty());
		match dist_url {
			Some(dist_url) => Self { dist_url, ..Self::default() },
			None => Self::default(),
		}
	}
}

impl Default for NodeOptions {
	fn default() -> Self {
		Self {
			dist_url: "https://nodejs.org/dist".to_owned(),
			target: Target::current(),
		}
	}
}

struct NodeClient {
	http: Client,
	options: NodeOptions,
}

impl NodeClient {
	fn index_path() -> &'static str {
		"/index.json"
	}

	fn shasums_path(version: &str) -> String {
		format!("/{version}/SHASUMS256.txt")
	}

	fn archive_path(version: &str, name: &str) -> String {
		format!("/{version}/{name}")
	}

	fn archive_url(&self, version: &str, name: &str) -> String {
		format!("{}{}", self.options.dist_url, Self::archive_path(version, name))
	}

	fn new(http: Client, options: NodeOptions) -> Self {
		Self { http, options }
	}

	async fn index(&self) -> Result<String, NodeError> {
		self._get(Self::index_path()).await
	}

	async fn shasums(&self, version: &str) -> Result<String, NodeError> {
		self._get(&Self::shasums_path(version)).await
	}

	async fn _get(&self, path: &str) -> Result<String, NodeError> {
		let url = format!("{}{path}", self.options.dist_url);
		let response = self.http.get(&url).send().await?;
		let status = response.status();
		if !status.is_success() {
			return Err(NodeError::Status(status));
		}
		Ok(response.text().await?)
	}

	// reqwest reports a zero length for head responses so the header is read directly
	async fn size(&self, url: &str) -> Result<u64, NodeError> {
		let response = self.http.head(url).send().await?;
		let status = response.status();
		if !status.is_success() {
			return Err(NodeError::Status(status));
		}
		response
			.headers()
			.get(CONTENT_LENGTH)
			.and_then(|value| value.to_str().ok())
			.and_then(|value| value.parse().ok())
			.ok_or_else(|| NodeError::MissingSize(url.to_owned()))
	}
}

#[derive(Debug)]
pub(crate) enum NodeError {
	Http(reqwest::Error),
	Parse(serde_json::Error),
	Status(StatusCode),
	NoMatchingVersion(String),
	UnsupportedPlatform { version: String, os: String, arch: String },
	MissingSize(String),
}

impl From<reqwest::Error> for NodeError {
	fn from(error: reqwest::Error) -> Self {
		Self::Http(error)
	}
}

impl From<serde_json::Error> for NodeError {
	fn from(error: serde_json::Error) -> Self {
		Self::Parse(error)
	}
}

impl fmt::Display for NodeError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Http(error) => write!(f, "node request failed: {error}"),
			Self::Parse(error) => write!(f, "node index parse failed: {error}"),
			Self::Status(status) => write!(f, "node dist returned {status}"),
			Self::NoMatchingVersion(version) => write!(f, "no node release found matching version '{version}'"),
			Self::UnsupportedPlatform { version, os, arch } => write!(f, "no node {version} build for {os} {arch}"),
			Self::MissingSize(url) => write!(f, "node dist returned no size for '{url}'"),
		}
	}
}

impl std::error::Error for NodeError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Http(error) => Some(error),
			Self::Parse(error) => Some(error),
			Self::Status(_) => None,
			Self::NoMatchingVersion(_) => None,
			Self::UnsupportedPlatform { .. } => None,
			Self::MissingSize(_) => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, ResponseTemplate};

	use super::*;
	use crate::backend::_backend::ArchiveFormat;
	use crate::backend::_fixtures::read_or_record;

	fn http() -> Client {
		Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build().unwrap()
	}

	fn _target(os: &'static str, arch: &'static str, libc: &'static str) -> Target {
		Target { os, arch, libc }
	}

	#[tokio::test]
	async fn resolves_archive_from_snapshot() {
		let index = read_or_record("node_index.json", || async { NodeClient::new(http(), NodeOptions::default()).index().await }).await;
		let shasums = read_or_record("node_v26.13.1_shasums256.txt", || async {
			NodeClient::new(http(), NodeOptions::default()).shasums("v26.13.1").await
		})
		.await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(NodeClient::index_path()))
			.respond_with(ResponseTemplate::new(200).set_body_string(index))
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path(NodeClient::shasums_path("v26.13.1")))
			.respond_with(ResponseTemplate::new(200).set_body_string(shasums.clone()))
			.mount(&server)
			.await;
		let request = NodeRequest {
			version: Version::parse("26.13.1"),
		};
		let targets = [
			_target("linux", "x86_64", "gnu"),
			_target("linux", "x86_64", "musl"),
			_target("linux", "aarch64", "gnu"),
			_target("macos", "x86_64", ""),
			_target("macos", "aarch64", ""),
			_target("windows", "x86_64", "msvc"),
			_target("windows", "aarch64", "msvc"),
		];
		let expected = [
			("node-v26.13.1-linux-x64.tar.xz", ArchiveFormat::TarXz),
			("node-v26.13.1-linux-x64-musl.tar.xz", ArchiveFormat::TarXz),
			("node-v26.13.1-linux-arm64.tar.xz", ArchiveFormat::TarXz),
			("node-v26.13.1-darwin-x64.tar.xz", ArchiveFormat::TarXz),
			("node-v26.13.1-darwin-arm64.tar.xz", ArchiveFormat::TarXz),
			("node-v26.13.1-win-x64.zip", ArchiveFormat::Zip),
			("node-v26.13.1-win-arm64.zip", ArchiveFormat::Zip),
		];
		for (target, (name, format)) in targets.into_iter().zip(expected) {
			// the size request must reach the selected archive
			Mock::given(method("HEAD"))
				.and(path(NodeClient::archive_path("v26.13.1", name)))
				.respond_with(ResponseTemplate::new(200).insert_header("content-length", "1234"))
				.mount(&server)
				.await;
			let backend = NodeBackend::new(http(), NodeOptions {
				dist_url: server.uri(),
				target: target.clone(),
			});
			let res = backend.resolve(&request).await.unwrap();
			// the checksum comes from the line that names the selected archive
			let checksum = shasums.lines().find_map(|line| {
				let mut parts = line.split_whitespace();
				let checksum = parts.next()?;
				(parts.next()? == name).then_some(checksum)
			});
			assert_eq!(res.version(), "v26.13.1", "{target:?}");
			assert_eq!(res.url(), format!("{}/v26.13.1/{name}", server.uri()), "{target:?}");
			assert_eq!(res.size(), 1234, "{target:?}");
			assert_eq!(res.format(), &format, "{target:?}");
			assert_eq!(Some(res.checksum()), checksum, "{target:?}");
		}
		// node publishes no 32-bit windows archive
		let backend = NodeBackend::new(http(), NodeOptions {
			dist_url: server.uri(),
			target: _target("windows", "x86", "msvc"),
		});
		let error = backend.resolve(&request).await.err().unwrap();
		assert!(matches!(error, NodeError::UnsupportedPlatform { .. }), "{error}");
	}

	#[tokio::test]
	async fn picks_release_from_snapshot() {
		let index = read_or_record("node_index.json", || async { NodeClient::new(http(), NodeOptions::default()).index().await }).await;
		let pick = |version: &str| {
			let releases: Vec<Release> = serde_json::from_str(&index).unwrap();
			_pick_release(releases, &Version::parse(version)).map(|release| release.version)
		};
		// latest and prefixes skip the v27 prereleases and pick the highest stable
		assert_eq!(pick("latest").as_deref(), Some("v26.13.1"));
		assert_eq!(pick("26").as_deref(), Some("v26.13.1"));
		assert_eq!(pick("26.12").as_deref(), Some("v26.12.1"));
		// an exact version matches with or without the v
		assert_eq!(pick("v26.12.0").as_deref(), Some("v26.12.0"));
		assert_eq!(pick("26.12.0").as_deref(), Some("v26.12.0"));
		// a prerelease installs only by exact version
		assert_eq!(pick("v27.0.0-alpha.0.1.0").as_deref(), Some("v27.0.0-alpha.0.1.0"));
		// a version missing from the index picks nothing
		assert_eq!(pick("99"), None);
	}
}
