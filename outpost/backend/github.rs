use std::{env, fmt};

use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::_asset_selection::select;
use super::_backend::ArchiveFormat;

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

	pub async fn latest_release(&self, full_name: &str) -> Result<Release, GithubError> {
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
		Ok(serde_json::from_str(&raw)?)
	}

	pub async fn release_by_tag(&self, full_name: &str, tag: &str) -> Result<Release, GithubError> {
		let raw = match self.client.release_by_tag(full_name, tag).await {
			Ok(raw) => raw,
			Err(GithubError::Status(status)) if status == StatusCode::NOT_FOUND => {
				return Err(GithubError::NotFound {
					full_name: full_name.to_owned(),
					tag: tag.to_owned(),
				});
			}
			Err(error) => return Err(error),
		};
		Ok(serde_json::from_str(&raw)?)
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

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
	pub tag_name: String,
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

	fn release_by_tag_path(full_name: &str, tag: &str) -> String {
		format!("/repos/{full_name}/releases/tags/{tag}")
	}

	fn new(http: Client, options: GithubOptions) -> Self {
		Self { http, options }
	}

	pub async fn latest_release(&self, full_name: &str) -> Result<String, GithubError> {
		let url = format!("{}{}", self.options.api_url, Self::latest_release_path(full_name));
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
		Ok(response.text().await?)
	}

	pub async fn release_by_tag(&self, full_name: &str, tag: &str) -> Result<String, GithubError> {
		let url = format!("{}{}", self.options.api_url, Self::release_by_tag_path(full_name, tag));
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
		Ok(response.text().await?)
	}
}

#[derive(Debug)]
pub enum GithubError {
	Http(reqwest::Error),
	Parse(serde_json::Error),
	Status(StatusCode),
	NotFound { full_name: String, tag: String },
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
			Self::NoAsset { .. } => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use wiremock::matchers::{method, path};
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
		let release = backend.latest_release("cli/cli").await.unwrap();
		let asset = &release.assets[0];
		let url = "https://github.com/cli/cli/releases/download/v2.103.3/gh_2.103.3_checksums.txt";
		assert_eq!(release.tag_name, "v2.103.3");
		assert_eq!(release.assets.len(), 22);
		assert_eq!(asset.name, "gh_2.103.3_checksums.txt");
		assert_eq!(asset.browser_download_url, url);
		assert_eq!(asset.size, 1971);
	}

	#[tokio::test]
	async fn resolves_release_by_tag_from_snapshot() {
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
			.and(path(GithubClient::release_by_tag_path("cli/cli", "v2.102.1")))
			.respond_with(ResponseTemplate::new(200).set_body_string(body))
			.mount(&server)
			.await;
		let backend = GithubBackend::new(http(), GithubOptions {
			api_url: server.uri(),
			..GithubOptions::default()
		});
		let release = backend.release_by_tag("cli/cli", "v2.102.1").await.unwrap();
		let asset = &release.assets[0];
		let url = "https://github.com/cli/cli/releases/download/v2.102.1/gh_2.102.1_checksums.txt";
		assert_eq!(release.tag_name, "v2.102.1");
		assert_eq!(release.assets.len(), 22);
		assert_eq!(asset.name, "gh_2.102.1_checksums.txt");
		assert_eq!(asset.browser_download_url, url);
		assert_eq!(asset.size, 1971);
	}
}
