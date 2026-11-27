use std::fmt;
use std::path::PathBuf;

use reqwest::header::CONTENT_LENGTH;
use reqwest::{Client, StatusCode};
use semver::Version as Semver;
use serde::Deserialize;

use super::_asset_selection::{self, Target};
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
		let Some((index, name, format)) = _asset_selection::select(entries.iter().map(|(_, name)| *name)) else {
			let target = Target::current();
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
}

impl Default for NodeOptions {
	fn default() -> Self {
		Self {
			dist_url: "https://nodejs.org/dist".to_owned(),
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
