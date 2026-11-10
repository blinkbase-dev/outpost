use std::{env, fmt};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Client, Response, StatusCode};
use semver::Version as Semver;
use serde::Deserialize;

use super::_asset_selection::select;
use super::_backend::ArchiveFormat;
use crate::tools::version::{Prefix, Version, split_lane, tag_version};

#[derive(Debug)]
pub struct GithubBackend {
	client: GithubClient,
}

impl GithubBackend {
	pub fn new(http: Client, options: GithubOptions) -> Self {
		Self {
			client: GithubClient::new(http, options),
		}
	}

	pub async fn select_release(&self, full_name: &str, lane: &str, version: &Version) -> Result<Release, GithubError> {
		match version {
			Version::Latest => {
				// repo-wide latest
				if lane.is_empty() {
					let raw = match self.client.latest_release(full_name).await {
						Ok(raw) => raw,
						Err(GithubError::Status(status)) if status == StatusCode::NOT_FOUND => {
							return Err(GithubError::NotFound {
								full_name: full_name.to_owned(),
								tag: "latest".to_owned(),
							});
						}
						Err(error) => return Err(error),
					};
					return Ok(serde_json::from_str(&raw)?);
				}
				// literal "{lane}latest" tag
				let literal = format!("{lane}latest");
				match self.client.release_by_tag(full_name, &literal).await {
					Ok(raw) => return Ok(serde_json::from_str(&raw)?),
					Err(GithubError::Status(status)) if status == StatusCode::NOT_FOUND => {}
					Err(error) => return Err(error),
				}
				// lane-scoped latest
				let mut page = 1;
				loop {
					let (body, has_next) = self.client.releases(full_name, page).await?;
					let releases: Vec<Release> = serde_json::from_str(&body)?;
					if let Some(release) = _pick_latest_release(releases, lane) {
						return Ok(release);
					}
					if !has_next || page == 3 {
						return Err(GithubError::NoMatchingVersion {
							full_name: full_name.to_owned(),
							version: format!("{lane}latest"),
						});
					}
					page += 1;
				}
			}
			Version::Prefix(prefix) => {
				let mut page = 1;
				loop {
					let (body, has_next) = self.client.releases(full_name, page).await?;
					let releases: Vec<Release> = serde_json::from_str(&body)?;
					if let Some(release) = _pick_release_from_prefix(releases, lane, prefix) {
						return Ok(release);
					}
					if !has_next || page == 3 {
						return Err(GithubError::NoMatchingVersion {
							full_name: full_name.to_owned(),
							version: format!("{lane}{prefix}"),
						});
					}
					page += 1;
				}
			}
			Version::Exact(value) => {
				let alternate = match value.strip_prefix('v') {
					Some(stripped) => stripped.to_owned(),
					None => format!("v{value}"),
				};
				let tags = if lane.is_empty() {
					[value.clone(), alternate]
				} else {
					[format!("{lane}{value}"), format!("{lane}{alternate}")]
				};
				for tag in tags {
					match self.client.release_by_tag(full_name, &tag).await {
						Ok(raw) => return Ok(serde_json::from_str(&raw)?),
						Err(GithubError::Status(status)) if status == StatusCode::NOT_FOUND => {}
						Err(error) => return Err(error),
					}
				}
				Err(GithubError::NotFound {
					full_name: full_name.to_owned(),
					tag: if lane.is_empty() { value.clone() } else { format!("{lane}{value}") },
				})
			}
		}
	}

	pub fn select_asset(&self, full_name: &str, release: &Release) -> Result<(String, u64, ArchiveFormat), GithubError> {
		let (asset, format) = select(release.assets.iter().map(|asset| asset.name.as_str()))
			.and_then(|(index, _, format)| release.assets.get(index).map(|asset| (asset, format)))
			.ok_or_else(|| GithubError::NoAsset {
				full_name: full_name.to_owned(),
				tag: release.tag_name.clone(),
			})?;
		Ok((asset.browser_download_url.clone(), asset.size, format))
	}
}

fn _pick_latest_release(releases: Vec<Release>, lane: &str) -> Option<Release> {
	for release in releases {
		if release.draft || release.prerelease {
			continue;
		}
		let (tag_lane, version) = split_lane(&release.tag_name);
		if tag_lane != lane {
			continue;
		}
		let Some(parsed) = tag_version(version) else {
			continue;
		};
		if parsed.pre.is_empty() {
			return Some(release);
		}
	}
	None
}

fn _pick_release_from_prefix(releases: Vec<Release>, lane: &str, prefix: &Prefix) -> Option<Release> {
	let mut best: Option<(Semver, Release)> = None;
	for release in releases {
		if release.draft || release.prerelease {
			continue;
		}
		let (tag_lane, version) = split_lane(&release.tag_name);
		if tag_lane != lane {
			continue;
		}
		let Some(version) = prefix.matches(version) else {
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

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
	pub tag_name: String,
	pub draft: bool,
	pub prerelease: bool,
	pub assets: Vec<Asset>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
	pub name: String,
	pub browser_download_url: String,
	pub size: u64,
}

#[derive(Debug)]
pub struct GithubOptions {
	pub api_url: String,
	pub api_version: String,
	pub token: Option<String>,
}

impl GithubOptions {
	pub fn from_process() -> Self {
		let token = ["GITHUB_TOKEN", "GH_TOKEN"]
			.into_iter()
			.find_map(|key| env::var(key).ok())
			.map(|token| token.trim().to_owned())
			.filter(|token| !token.is_empty());
		Self { token, ..Self::default() }
	}
}

impl Default for GithubOptions {
	fn default() -> Self {
		Self {
			api_url: "https://api.github.com".to_owned(),
			api_version: "2026-03-10".to_owned(),
			token: None,
		}
	}
}

#[derive(Debug)]
struct GithubClient {
	http: Client,
	options: GithubOptions,
}

impl GithubClient {
	fn latest_release_path(full_name: &str) -> String {
		format!("/repos/{full_name}/releases/latest")
	}

	fn releases_path(full_name: &str) -> String {
		format!("/repos/{full_name}/releases")
	}

	fn release_by_tag_path(full_name: &str, tag: &str) -> String {
		format!("/repos/{full_name}/releases/tags/{}", utf8_percent_encode(tag, NON_ALPHANUMERIC))
	}

	fn new(http: Client, options: GithubOptions) -> Self {
		Self { http, options }
	}

	async fn latest_release(&self, full_name: &str) -> Result<String, GithubError> {
		let url = format!("{}{}", self.options.api_url, Self::latest_release_path(full_name));
		Ok(self._get(&url).await?.text().await?)
	}

	async fn releases(&self, full_name: &str, page: usize) -> Result<(String, bool), GithubError> {
		let url = format!("{}{}?per_page=100&page={page}", self.options.api_url, Self::releases_path(full_name));
		let response = self._get(&url).await?;
		let has_next = response
			.headers()
			.get(reqwest::header::LINK)
			.and_then(|value| value.to_str().ok())
			.is_some_and(|value| value.contains("rel=\"next\""));
		let body = response.text().await?;
		Ok((body, has_next))
	}

	async fn release_by_tag(&self, full_name: &str, tag: &str) -> Result<String, GithubError> {
		let url = format!("{}{}", self.options.api_url, Self::release_by_tag_path(full_name, tag));
		Ok(self._get(&url).await?.text().await?)
	}

	async fn _get(&self, url: &str) -> Result<Response, GithubError> {
		let mut request = self
			.http
			.get(url)
			.header("Accept", "application/vnd.github+json")
			.header("X-GitHub-Api-Version", self.options.api_version.as_str());
		if let Some(token) = &self.options.token {
			request = request.bearer_auth(token);
		}
		let response = request.send().await?;
		let status = response.status();
		if !status.is_success() {
			return Err(GithubError::Status(status));
		}
		Ok(response)
	}
}

#[derive(Debug)]
pub enum GithubError {
	Http(reqwest::Error),
	Parse(serde_json::Error),
	Status(StatusCode),
	NotFound { full_name: String, tag: String },
	NoMatchingVersion { full_name: String, version: String },
	NoAsset { full_name: String, tag: String },
}

impl From<reqwest::Error> for GithubError {
	fn from(error: reqwest::Error) -> Self {
		Self::Http(error)
	}
}

impl From<serde_json::Error> for GithubError {
	fn from(error: serde_json::Error) -> Self {
		Self::Parse(error)
	}
}

impl fmt::Display for GithubError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Http(error) => write!(f, "github request failed: {error}"),
			Self::Parse(error) => write!(f, "github response parse failed: {error}"),
			Self::Status(status) => write!(f, "github returned {status}"),
			Self::NotFound { full_name, tag } => write!(f, "no release found for `{full_name}` at tag `{tag}`"),
			Self::NoMatchingVersion { full_name, version } => write!(f, "no release found for `{full_name}` matching version `{version}`"),
			Self::NoAsset { full_name, tag } => write!(f, "no compatible asset for `{full_name}` at tag `{tag}`"),
		}
	}
}

impl std::error::Error for GithubError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Http(error) => Some(error),
			Self::Parse(error) => Some(error),
			Self::Status(_) => None,
			Self::NotFound { .. } => None,
			Self::NoMatchingVersion { .. } => None,
			Self::NoAsset { .. } => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use tokio::sync::OnceCell;
	use wiremock::matchers::{method, path, query_param};
	use wiremock::{Mock, MockServer, ResponseTemplate};

	use super::*;

	fn http() -> Client {
		Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build().unwrap()
	}

	async fn read_or_record<F, Fut>(name: &str, fetch: F) -> String
	where
		F: FnOnce() -> Fut,
		Fut: std::future::Future<Output = Result<String, GithubError>>,
	{
		let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("backend/__fixtures").join(name);
		match fs::read_to_string(&file) {
			Ok(body) => body,
			Err(_) => {
				let body = fetch().await.unwrap();
				fs::create_dir_all(file.parent().unwrap()).unwrap();
				fs::write(&file, &body).unwrap();
				body
			}
		}
	}

	static MAGIKA_RELEASES: OnceCell<String> = OnceCell::const_new();

	async fn magika_releases() -> &'static str {
		MAGIKA_RELEASES
			.get_or_init(|| async {
				read_or_record("github_google_magika_releases_1.json", || async {
					let client = GithubClient::new(http(), GithubOptions::from_process());
					let (body, _) = client.releases("google/magika", 1).await?;
					Ok(body)
				})
				.await
			})
			.await
			.as_str()
	}

	#[tokio::test]
	async fn resolves_latest_release_from_snapshot() {
		let body = read_or_record("github_cli_cli_latest.json", || async {
			GithubClient::new(http(), GithubOptions::from_process()).latest_release("cli/cli").await
		})
		.await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::latest_release_path("cli/cli")))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend.select_release("cli/cli", "", &Version::Latest).await.unwrap();
		let asset = &release.assets[0];
		let url = "https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_checksums.txt";
		assert_eq!(release.tag_name, "v2.103.3");
		assert_eq!(release.assets.len(), 22);
		assert_eq!(asset.name, "gh_2.103.3_checksums.txt");
		assert_eq!(asset.browser_download_url, url);
		assert_eq!(asset.size, 1971);
	}

	#[tokio::test]
	async fn resolves_literal_latest_release_from_snapshot() {
		let body = read_or_record("github_google_magika_cli_latest.json", || async {
			let client = GithubClient::new(http(), GithubOptions::from_process());
			client.release_by_tag("google/magika", "cli-latest").await
		})
		.await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("google/magika", "cli-latest")))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend.select_release("google/magika", "cli-", &Version::Latest).await.unwrap();
		assert_eq!(release.tag_name, "cli-latest");
	}

	#[tokio::test]
	async fn resolves_lane_latest_release_from_snapshot() {
		let body = magika_releases().await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("google/magika", "cli/latest")))
			.respond_with(ResponseTemplate::new(404))
			.expect(1)
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path(GithubClient::releases_path("google/magika")))
			.and(query_param("per_page", "100"))
			.and(query_param("page", "1"))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend.select_release("google/magika", "cli/", &Version::Latest).await.unwrap();
		assert_eq!(release.tag_name, "cli/v1.1.0");
		server.verify().await;
	}

	#[tokio::test]
	async fn resolves_prefixes_from_snapshot() {
		let first = read_or_record("github_cli_cli_releases_1.json", || async {
			let client = GithubClient::new(http(), GithubOptions::from_process());
			let (body, _) = client.releases("cli/cli", 1).await?;
			Ok(body)
		})
		.await;
		let second = read_or_record("github_cli_cli_releases_2.json", || async {
			let client = GithubClient::new(http(), GithubOptions::from_process());
			let (body, _) = client.releases("cli/cli", 2).await?;
			Ok(body)
		})
		.await;
		let server = MockServer::start().await;
		let next = format!("{}{}?per_page=100&page=2", server.uri(), GithubClient::releases_path("cli/cli"));
		Mock::given(method("GET"))
			.and(path(GithubClient::releases_path("cli/cli")))
			.and(query_param("per_page", "100"))
			.and(query_param("page", "1"))
			.respond_with(
				ResponseTemplate::new(200)
					.insert_header("Link", format!("<{next}>; rel=\"next\""))
					.set_body_string(first),
			)
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path(GithubClient::releases_path("cli/cli")))
			.and(query_param("per_page", "100"))
			.and(query_param("page", "2"))
			.respond_with(ResponseTemplate::new(200).set_body_string(second))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		for (prefix, expected) in [
			("2", "v2.103.3"),
			("2.103", "v2.103.3"),
			("2.34", "v2.34.0"),
			("1", "v1.14.0"),
			("1.14", "v1.14.0"),
			("2.20", "v2.20.2"),
			("0.6", "v0.6.4"),
			("0", "v0.12.0"),
		] {
			let release = backend.select_release("cli/cli", "", &Version::parse(prefix)).await.unwrap();
			assert_eq!(release.tag_name, expected, "{prefix}");
		}

		for prefix in ["3", "2.999"] {
			let result = backend.select_release("cli/cli", "", &Version::parse(prefix)).await;
			assert!(matches!(result, Err(GithubError::NoMatchingVersion { .. })), "{prefix}");
		}
	}

	#[tokio::test]
	async fn resolves_lane_prefixes_from_snapshot() {
		let body = magika_releases().await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::releases_path("google/magika")))
			.and(query_param("per_page", "100"))
			.and(query_param("page", "1"))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		for (lane, prefix, expected) in [
			("cli/", "1", "cli/v1.1.0"),
			("cli/", "1.0", "cli/v1.0.2"),
			("cli/", "0.1", "cli/v0.1.4"),
			("python-", "1", "python-v1.0.2"),
			("python-", "0.6", "python-v0.6.3"),
		] {
			let release = backend.select_release("google/magika", lane, &Version::parse(prefix)).await.unwrap();
			assert_eq!(release.tag_name, expected, "{lane}{prefix}");
		}

		for (lane, prefix) in [("cli/", "9"), ("lib/", "1"), ("", "1")] {
			let result = backend.select_release("google/magika", lane, &Version::parse(prefix)).await;
			assert!(matches!(result, Err(GithubError::NoMatchingVersion { .. })), "{lane}{prefix}");
		}
	}

	#[tokio::test]
	async fn resolves_exact_version_from_snapshot() {
		let body = read_or_record("github_cli_cli_v2.102.1.json", || async {
			let body = GithubClient::new(http(), GithubOptions::from_process())
				.release_by_tag("cli/cli", "v2.102.1")
				.await
				.unwrap();
			Ok(body)
		})
		.await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("cli/cli", "2.102.1")))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("cli/cli", "v2.102.1")))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend.select_release("cli/cli", "", &Version::Exact("2.102.1".to_owned())).await.unwrap();
		let asset = &release.assets[0];
		let url = "https://github.com/cli/cli/releases/download/v2.102.1/gh_2.102.1_checksums.txt";
		assert_eq!(release.tag_name, "v2.102.1");
		assert_eq!(release.assets.len(), 22);
		assert_eq!(asset.name, "gh_2.102.1_checksums.txt");
		assert_eq!(asset.browser_download_url, url);
		assert_eq!(asset.size, 1971);
	}

	#[tokio::test]
	async fn resolves_lane_exact_version_from_snapshot() {
		let body = read_or_record("github_google_magika_v1.0.2.json", || async {
			let client = GithubClient::new(http(), GithubOptions::from_process());
			client.release_by_tag("google/magika", "cli/v1.0.2").await
		})
		.await;
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("google/magika", "cli/1.0.2")))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path(GithubClient::release_by_tag_path("google/magika", "cli/v1.0.2")))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend
			.select_release("google/magika", "cli/", &Version::Exact("1.0.2".to_owned()))
			.await
			.unwrap();
		assert_eq!(release.tag_name, "cli/v1.0.2");
	}
}

#[cfg(test)]
mod unit_tests {
	use super::*;

	#[test]
	fn selects_newest_in_lane() {
		let releases = vec![
			Release {
				tag_name: "cli/1.9.1".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "cli/2.0.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "lib/9.0.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let selected = _pick_latest_release(releases, "cli/").unwrap();
		assert_eq!(selected.tag_name, "cli/1.9.1");
	}

	#[test]
	fn skips_prerelease_in_lane() {
		let releases = vec![
			Release {
				tag_name: "cli/1.9.2".to_owned(),
				draft: false,
				prerelease: true,
				assets: Vec::new(),
			},
			Release {
				tag_name: "cli/1.9.1".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let selected = _pick_latest_release(releases, "cli/").unwrap();
		assert_eq!(selected.tag_name, "cli/1.9.1");
	}

	#[test]
	fn selects_highest_stable_prefix_match() {
		let releases = vec![
			Release {
				tag_name: "v2.19.9".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v2.20.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v2.20.1".to_owned(),
				draft: false,
				prerelease: true,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v2.20.2-rc.1".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v3.0.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let Version::Prefix(prefix) = Version::parse("2.20") else {
			panic!("not a prefix");
		};
		let selected = _pick_release_from_prefix(releases, "", &prefix).unwrap();
		assert_eq!(selected.tag_name, "v2.20.0");
	}

	#[test]
	fn selects_highest_across_components() {
		let releases = vec![
			Release {
				tag_name: "v2.9.9".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v2.20.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "v2.20.1".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let Version::Prefix(prefix) = Version::parse("2") else {
			panic!("not a prefix");
		};
		let selected = _pick_release_from_prefix(releases, "", &prefix).unwrap();
		assert_eq!(selected.tag_name, "v2.20.1");
	}

	#[test]
	fn selects_from_requested_lane() {
		let releases = vec![
			Release {
				tag_name: "cli/1.0.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "cli/1.1.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "lib/9.0.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "1.2.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let Version::Prefix(prefix) = Version::parse("1") else {
			panic!("not a prefix");
		};
		let selected = _pick_release_from_prefix(releases, "cli/", &prefix).unwrap();
		assert_eq!(selected.tag_name, "cli/1.1.0");
	}

	#[test]
	fn selects_from_bare_tags() {
		let releases = vec![
			Release {
				tag_name: "cli/1.1.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "1.2.0".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let Version::Prefix(prefix) = Version::parse("1") else {
			panic!("not a prefix");
		};
		let selected = _pick_release_from_prefix(releases, "", &prefix).unwrap();
		assert_eq!(selected.tag_name, "1.2.0");
	}

	#[test]
	fn selects_two_component_latest_tags() {
		let bare = vec![Release {
			tag_name: "2.21".to_owned(),
			draft: false,
			prerelease: false,
			assets: Vec::new(),
		}];
		assert_eq!(_pick_latest_release(bare, "").unwrap().tag_name, "2.21");

		let laned = vec![Release {
			tag_name: "cli/2.20".to_owned(),
			draft: false,
			prerelease: false,
			assets: Vec::new(),
		}];
		assert_eq!(_pick_latest_release(laned, "cli/").unwrap().tag_name, "cli/2.20");
	}

	#[test]
	fn selects_two_component_tags() {
		let bare = vec![
			Release {
				tag_name: "2.19.9".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "2.20".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		let Version::Prefix(prefix) = Version::parse("2") else {
			panic!("not a prefix");
		};
		assert_eq!(_pick_release_from_prefix(bare, "", &prefix).unwrap().tag_name, "2.20");

		let laned = vec![
			Release {
				tag_name: "cli/2.19.9".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
			Release {
				tag_name: "cli/2.20".to_owned(),
				draft: false,
				prerelease: false,
				assets: Vec::new(),
			},
		];
		assert_eq!(_pick_release_from_prefix(laned, "cli/", &prefix).unwrap().tag_name, "cli/2.20");
	}

	#[test]
	fn ignores_unrelated_prefix() {
		let releases = vec![Release {
			tag_name: "v2.20.1".to_owned(),
			draft: false,
			prerelease: false,
			assets: Vec::new(),
		}];
		let Version::Prefix(prefix) = Version::parse("3") else {
			panic!("not a prefix");
		};
		assert!(_pick_release_from_prefix(releases, "", &prefix).is_none());
	}
}
