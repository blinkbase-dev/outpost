use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use regex::regex;

use super::lock::{Identity, LockedTool};
use super::version::{Version, split_lane};
use crate::backend::github;
use crate::config::loader::{ConfigFile, ToolOptions};

pub(crate) fn requests(files: &[ConfigFile]) -> Result<Vec<ToolRequest>, ToolRequestError> {
	let mut requests = Vec::new();
	for file in files {
		for (key, options) in &file.config.tools {
			requests.push(ToolRequest {
				origin: file.path.clone(),
				tool: RequestedTool::parse(&file.path, key, options)?,
			});
		}
	}
	Ok(requests)
}

#[derive(Debug, PartialEq)]
pub(crate) struct ToolRequest {
	pub(crate) origin: PathBuf,
	pub(crate) tool: RequestedTool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RequestedTool {
	Github(GithubRequest),
}

impl RequestedTool {
	pub(crate) fn identity(&self) -> Identity {
		match self {
			Self::Github(req) => Identity::Github {
				api_url: req.api_url.clone(),
				full_name: req.full_name.clone(),
				lane: req.lane.clone(),
				version: req.version.clone(),
			},
		}
	}

	pub(crate) fn identity_key(&self) -> String {
		match self {
			Self::Github(req) => format!("github:{}:{}@{}", req.api_url, req.full_name, req.lane),
		}
	}

	pub(crate) fn install_path(&self, version: &str) -> PathBuf {
		match self {
			Self::Github(req) => github::install_path(&req.api_url, &req.full_name, version),
		}
	}

	pub(crate) fn install_prefix_matches(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Github(req), Self::Github(other)) => req.api_url == other.api_url && req.full_name == other.full_name,
		}
	}

	pub(crate) fn install_prefix_matches_locked(&self, locked: &LockedTool) -> bool {
		match (self, locked) {
			(Self::Github(req), LockedTool::Github(lock)) => req.api_url == lock.api_url && req.full_name == lock.full_name,
		}
	}

	fn parse(path: &Path, key: &str, options: &ToolOptions) -> Result<Self, ToolRequestError> {
		let Some((_, full_name)) = key.split_once(':') else {
			return Err(ToolRequestError::InvalidFullName {
				path: path.to_path_buf(),
				key: key.to_owned(),
			});
		};
		if regex!(r"^(?:\.{1,2})?$|[^a-zA-Z0-9_./-]").is_match(full_name) {
			return Err(ToolRequestError::InvalidFullName {
				path: path.to_path_buf(),
				key: key.to_owned(),
			});
		}
		match options {
			ToolOptions::Github(table) => {
				let (lane, value) = split_lane(table.version.trim());
				if value.is_empty() {
					return Err(ToolRequestError::EmptyVersion {
						path: path.to_path_buf(),
						key: key.to_owned(),
					});
				}
				Ok(Self::Github(GithubRequest {
					api_url: table.api_url.clone(),
					full_name: full_name.to_ascii_lowercase(),
					lane: lane.to_owned(),
					version: Version::parse(value),
					exposed: table.exposed.clone(),
					rename_shims: table.rename_shims.clone(),
				}))
			}
		}
	}
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GithubRequest {
	pub(crate) api_url: String,
	pub(crate) full_name: String,
	pub(crate) lane: String,
	pub(crate) version: Version,
	pub(crate) exposed: Option<Vec<String>>,
	pub(crate) rename_shims: Option<BTreeMap<String, String>>,
}

#[derive(Clone)]
pub(crate) struct ToolRequestSet {
	pub(crate) origins: Vec<PathBuf>,
	pub(crate) tool: RequestedTool,
}

impl ToolRequestSet {
	pub(crate) fn from_requests(requests: &[ToolRequest]) -> Vec<Self> {
		let mut index: HashMap<Identity, usize> = HashMap::with_capacity(requests.len());
		let mut sets: Vec<Self> = Vec::with_capacity(requests.len());
		for request in requests {
			let identity = request.tool.identity();
			match index.get(&identity) {
				Some(&index) => sets[index].origins.push(request.origin.clone()),
				None => {
					index.insert(identity, sets.len());
					sets.push(Self {
						origins: vec![request.origin.clone()],
						tool: request.tool.clone(),
					});
				}
			}
		}
		sets
	}
}

#[derive(Debug)]
pub(crate) enum ToolRequestError {
	InvalidFullName { path: PathBuf, key: String },
	EmptyVersion { path: PathBuf, key: String },
}

impl fmt::Display for ToolRequestError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::InvalidFullName { path, key } => write!(f, "invalid full name in '{key}' from '{}'", path.display()),
			Self::EmptyVersion { path, key } => write!(f, "missing version for '{key}' from '{}'", path.display()),
		}
	}
}

impl std::error::Error for ToolRequestError {}

#[cfg(test)]
mod tests {
	use indoc::{formatdoc, indoc};

	use super::*;

	pub(super) fn _request(full_name: &str, lane: &str, version: Version) -> ToolRequest {
		ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: full_name.to_owned(),
				lane: lane.to_owned(),
				version,
				exposed: None,
				rename_shims: None,
			}),
		}
	}

	fn _config(path: &str, body: &str) -> ConfigFile {
		ConfigFile {
			path: PathBuf::from(path),
			config: toml::from_str(body).unwrap(),
		}
	}

	#[test]
	fn parses_latest() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = "latest"
		"#};
		let config = _config("outpost.toml", body);
		assert_eq!(requests(&[config]).unwrap(), vec![_request("owner/repo", "", Version::Latest)]);
	}

	#[test]
	fn parses_exact() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = "0.26.0"
		"#};
		let config = _config("outpost.toml", body);
		assert_eq!(requests(&[config]).unwrap(), vec![_request("owner/repo", "", Version::Exact("0.26.0".to_owned()))]);
	}

	#[test]
	fn parses_lanes() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = "cli/1.2.3"
		"#};
		let config = _config("outpost.toml", body);
		assert_eq!(requests(&[config]).unwrap(), vec![_request(
			"owner/repo",
			"cli/",
			Version::Exact("1.2.3".to_owned())
		)]);
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = "tool-10"
		"#};
		let config = _config("outpost.toml", body);
		assert_eq!(requests(&[config]).unwrap(), vec![_request("owner/repo", "tool-", Version::parse("10"))]);
	}

	#[test]
	fn parses_dotted_full_name() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo.js" = "latest"
		"#};
		let config = _config("outpost.toml", body);
		assert_eq!(requests(&[config]).unwrap(), vec![_request("owner/repo.js", "", Version::Latest)]);
	}

	#[test]
	fn parses_table_form() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = { version = "0.26.0" }
		"github:owner/other" = { version = "0.26.0", api_url = "http://127.0.0.1:8080", exposed = ["tool", "bin/*"], rename_shims = { "tool_*" = "tool" } }
		"#};
		let config = _config("outpost.toml", body);
		let mut other = _request("owner/other", "", Version::Exact("0.26.0".to_owned()));
		let RequestedTool::Github(req) = &mut other.tool;
		req.api_url = "http://127.0.0.1:8080".to_owned();
		req.exposed = Some(vec!["tool".to_owned(), "bin/*".to_owned()]);
		req.rename_shims = Some(BTreeMap::from([("tool_*".to_owned(), "tool".to_owned())]));
		assert_eq!(requests(&[config]).unwrap(), vec![
			other,
			_request("owner/repo", "", Version::Exact("0.26.0".to_owned()))
		]);
	}

	#[test]
	fn keeps_requests_from_every_file() {
		let system_body = indoc! {r#"
		[tools]
		"github:owner/repo" = "1"
		"#};
		let system = _config("system/outpost.toml", system_body);
		let project_body = indoc! {r#"
		[tools]
		"github:owner/repo" = "2"
		"#};
		let project = _config("project/outpost.toml", project_body);
		let nested_body = indoc! {r#"
		[tools]
		"github:owner/repo" = "1"
		"#};
		let nested = _config("project/nested/outpost.toml", nested_body);
		assert_eq!(requests(&[system, project, nested]).unwrap(), vec![
			ToolRequest {
				origin: PathBuf::from("system/outpost.toml"),
				.._request("owner/repo", "", Version::parse("1"))
			},
			ToolRequest {
				origin: PathBuf::from("project/outpost.toml"),
				.._request("owner/repo", "", Version::parse("2"))
			},
			// the same request from another file is still a separate request
			ToolRequest {
				origin: PathBuf::from("project/nested/outpost.toml"),
				.._request("owner/repo", "", Version::parse("1"))
			},
		]);
	}

	#[test]
	fn rejects_invalid_full_name() {
		for key in ["github:", "github:.", "github:..", r"github:a\b"] {
			let body = formatdoc! {r#"
			[tools]
			'{key}' = "latest"
			"#};
			let config = _config("outpost.toml", &body);
			assert!(
				matches!(requests(&[config]), Err(ToolRequestError::InvalidFullName { .. })),
				"{key} should be rejected"
			);
		}
	}

	#[test]
	fn rejects_empty_version() {
		let body = indoc! {r#"
		[tools]
		"github:owner/repo" = ""
		"#};
		let config = _config("outpost.toml", body);
		assert!(matches!(requests(&[config]), Err(ToolRequestError::EmptyVersion { .. })));
	}
}

#[cfg(test)]
mod set_tests {
	use super::tests::_request;
	use super::*;

	#[test]
	fn sets_requests_by_identity() {
		let requests = vec![
			ToolRequest {
				origin: PathBuf::from("a.toml"),
				.._request("owner/repo", "", Version::parse("1.2.3"))
			},
			ToolRequest {
				origin: PathBuf::from("b.toml"),
				.._request("owner/repo", "", Version::parse("1.2.3"))
			},
			ToolRequest {
				origin: PathBuf::from("c.toml"),
				.._request("owner/repo", "", Version::parse("2.0.0"))
			},
		];
		let sets = ToolRequestSet::from_requests(&requests);
		assert_eq!(sets.len(), 2);
		assert_eq!(sets[0].tool, _request("owner/repo", "", Version::parse("1.2.3")).tool);
		assert_eq!(sets[0].origins, vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]);
		assert_eq!(sets[1].tool, _request("owner/repo", "", Version::parse("2.0.0")).tool);
		assert_eq!(sets[1].origins, vec![PathBuf::from("c.toml")]);
	}
}
