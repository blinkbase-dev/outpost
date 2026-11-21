use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use regex::regex;

use super::lock::{Identity, LockedTool};
use super::version::{Version, split_lane};
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
	Github {
		api_url: String,
		full_name: String,
		lane: String,
		version: Version,
		exposed: Option<Vec<String>>,
		rename_shims: Option<BTreeMap<String, String>>,
	},
}

impl RequestedTool {
	pub(crate) fn identity(&self) -> Identity {
		match self {
			Self::Github {
				api_url,
				full_name,
				lane,
				version,
				..
			} => Identity::Github {
				api_url: api_url.clone(),
				full_name: full_name.clone(),
				lane: lane.clone(),
				version: version.clone(),
			},
		}
	}

	pub(crate) fn install_prefix_matches(&self, other: &Self) -> bool {
		match (self, other) {
			(
				Self::Github { api_url, full_name, .. },
				Self::Github {
					api_url: other_url,
					full_name: other_name,
					..
				},
			) => api_url == other_url && full_name == other_name,
		}
	}

	pub(crate) fn install_prefix_matches_locked(&self, locked: &LockedTool) -> bool {
		match (self, locked) {
			(
				Self::Github { api_url, full_name, .. },
				LockedTool::Github {
					api_url: other_url,
					full_name: other_name,
					..
				},
			) => api_url == other_url && full_name == other_name,
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
				Ok(Self::Github {
					api_url: table.api_url.clone(),
					full_name: full_name.to_ascii_lowercase(),
					lane: lane.to_owned(),
					version: Version::parse(value),
					exposed: table.exposed.clone(),
					rename_shims: table.rename_shims.clone(),
				})
			}
		}
	}
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
	use super::*;

	#[test]
	fn parses_latest() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "latest"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::Latest,
				exposed: None,
				rename_shims: None,
			},
		}]);
	}

	#[test]
	fn parses_exact() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "0.26.0"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::Exact("0.26.0".to_owned()),
				exposed: None,
				rename_shims: None,
			},
		}]);
	}

	#[test]
	fn parses_lanes() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "cli/1.2.3"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: "cli/".to_owned(),
				version: Version::Exact("1.2.3".to_owned()),
				exposed: None,
				rename_shims: None,
			},
		}]);

		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "tool-10"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: "tool-".to_owned(),
				version: Version::parse("10"),
				exposed: None,
				rename_shims: None,
			},
		}]);
	}

	#[test]
	fn parses_dotted_full_name() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo.js" = "latest"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github {
				api_url: String::new(),
				full_name: "owner/repo.js".to_owned(),
				lane: String::new(),
				version: Version::Latest,
				exposed: None,
				rename_shims: None,
			},
		}]);
	}

	#[test]
	fn parses_table_form() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = { version = "0.26.0" }
				"github:owner/other" = { version = "0.26.0", api_url = "http://127.0.0.1:8080", exposed = ["tool", "bin/*"], rename_shims = { "tool_*" = "tool" } }
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github {
					api_url: "http://127.0.0.1:8080".to_owned(),
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::Exact("0.26.0".to_owned()),
					exposed: Some(vec!["tool".to_owned(), "bin/*".to_owned()]),
					rename_shims: Some(BTreeMap::from([("tool_*".to_owned(), "tool".to_owned())])),
				},
			},
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::Exact("0.26.0".to_owned()),
					exposed: None,
					rename_shims: None,
				},
			},
		]);
	}

	#[test]
	fn keeps_requests_from_every_file() {
		let system = ConfigFile {
			path: PathBuf::from("system/outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "1"
				"#,
			)
			.unwrap(),
		};
		let project = ConfigFile {
			path: PathBuf::from("project/outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "2"
				"#,
			)
			.unwrap(),
		};
		let nested = ConfigFile {
			path: PathBuf::from("project/nested/outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = "1"
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[system, project, nested]).unwrap(), vec![
			ToolRequest {
				origin: PathBuf::from("system/outpost.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1"),
					exposed: None,
					rename_shims: None,
				},
			},
			ToolRequest {
				origin: PathBuf::from("project/outpost.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2"),
					exposed: None,
					rename_shims: None,
				},
			},
			// the same request from another file is still a separate request
			ToolRequest {
				origin: PathBuf::from("project/nested/outpost.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1"),
					exposed: None,
					rename_shims: None,
				},
			},
		]);
	}

	#[test]
	fn rejects_invalid_full_name() {
		for key in ["github:", "github:.", "github:..", r"github:a\b"] {
			let config = ConfigFile {
				path: PathBuf::from("outpost.toml"),
				config: toml::from_str(&format!(
					r#"
					[tools]
					'{key}' = "latest"
					"#,
				))
				.unwrap(),
			};
			assert!(
				matches!(requests(&[config]), Err(ToolRequestError::InvalidFullName { .. })),
				"{key} should be rejected"
			);
		}
	}

	#[test]
	fn rejects_empty_version() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"github:owner/repo" = ""
				"#,
			)
			.unwrap(),
		};
		assert!(matches!(requests(&[config]), Err(ToolRequestError::EmptyVersion { .. })));
	}
}

#[cfg(test)]
mod set_tests {
	use super::*;

	#[test]
	fn sets_requests_by_identity() {
		let requests = vec![
			ToolRequest {
				origin: std::path::PathBuf::from("a.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				},
			},
			ToolRequest {
				origin: std::path::PathBuf::from("b.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				},
			},
			ToolRequest {
				origin: std::path::PathBuf::from("c.toml"),
				tool: RequestedTool::Github {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
					exposed: None,
					rename_shims: None,
				},
			},
		];
		let sets = ToolRequestSet::from_requests(&requests);
		assert_eq!(sets.len(), 2);
		assert_eq!(sets[0].tool, RequestedTool::Github {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
			exposed: None,
			rename_shims: None,
		});
		assert_eq!(sets[0].origins, vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]);
		assert_eq!(sets[1].tool, RequestedTool::Github {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
			exposed: None,
			rename_shims: None,
		});
		assert_eq!(sets[1].origins, vec![PathBuf::from("c.toml")]);
	}
}
