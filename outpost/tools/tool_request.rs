use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use regex::regex;

use super::lock::Identity;
use super::version::{Version, split_lane};
use crate::config::loader::{ConfigFile, ToolOptions};

pub(crate) fn requests(files: &[ConfigFile]) -> Result<Vec<ToolRequestEntry>, ToolRequestError> {
	let mut requests = Vec::new();
	for file in files {
		for (key, options) in &file.config.tools {
			requests.push(ToolRequestEntry {
				origin: file.path.clone(),
				tool: ToolRequest::parse(&file.path, key, options)?,
			});
		}
	}
	Ok(requests)
}

#[derive(Debug, PartialEq)]
pub(crate) struct ToolRequestEntry {
	pub(crate) origin: PathBuf,
	pub(crate) tool: ToolRequest,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ToolRequest {
	Github {
		full_name: String,
		lane: String,
		version: Version,
		api_url: Option<String>,
		exposed: Option<Vec<String>>,
	},
}

impl ToolRequest {
	pub(crate) fn identity(&self) -> Identity {
		match self {
			Self::Github { full_name, lane, version, .. } => ("github", full_name.clone(), lane.clone(), version.clone()),
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
					full_name: full_name.to_ascii_lowercase(),
					lane: lane.to_owned(),
					version: Version::parse(value),
					api_url: table.api_url.clone(),
					exposed: table.exposed.clone(),
				})
			}
		}
	}
}

pub(crate) struct ToolRequestGroup {
	pub(crate) origins: Vec<PathBuf>,
	pub(crate) tool: ToolRequest,
}

impl ToolRequestGroup {
	pub(crate) fn from_entries(entries: Vec<ToolRequestEntry>) -> Vec<Self> {
		let mut index: HashMap<Identity, usize> = HashMap::with_capacity(entries.len());
		let mut groups: Vec<Self> = Vec::with_capacity(entries.len());
		for entry in entries {
			let identity = entry.tool.identity();
			match index.get(&identity) {
				Some(&index) => groups[index].origins.push(entry.origin),
				None => {
					index.insert(identity, groups.len());
					groups.push(Self {
						origins: vec![entry.origin],
						tool: entry.tool,
					});
				}
			}
		}
		groups
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
			Self::InvalidFullName { path, key } => write!(f, "invalid full name in `{key}` from `{}`", path.display()),
			Self::EmptyVersion { path, key } => write!(f, "missing version for `{key}` from `{}`", path.display()),
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
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::Latest,
				api_url: None,
				exposed: None,
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
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::Exact("0.26.0".to_owned()),
				api_url: None,
				exposed: None,
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
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: "cli/".to_owned(),
				version: Version::Exact("1.2.3".to_owned()),
				api_url: None,
				exposed: None,
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
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: "tool-".to_owned(),
				version: Version::parse("10"),
				api_url: None,
				exposed: None,
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
		assert_eq!(requests(&[config]).unwrap(), vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github {
				full_name: "owner/repo.js".to_owned(),
				lane: String::new(),
				version: Version::Latest,
				api_url: None,
				exposed: None,
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
				"github:owner/other" = { version = "0.26.0", api_url = "http://127.0.0.1:8080", exposed = ["tool", "bin/*"] }
				"#,
			)
			.unwrap(),
		};
		assert_eq!(requests(&[config]).unwrap(), vec![
			ToolRequestEntry {
				origin: PathBuf::from("outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::Exact("0.26.0".to_owned()),
					api_url: Some("http://127.0.0.1:8080".to_owned()),
					exposed: Some(vec!["tool".to_owned(), "bin/*".to_owned()]),
				},
			},
			ToolRequestEntry {
				origin: PathBuf::from("outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::Exact("0.26.0".to_owned()),
					api_url: None,
					exposed: None,
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
			ToolRequestEntry {
				origin: PathBuf::from("system/outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1"),
					api_url: None,
					exposed: None,
				},
			},
			ToolRequestEntry {
				origin: PathBuf::from("project/outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2"),
					api_url: None,
					exposed: None,
				},
			},
			// the same request from another file is still a separate entry
			ToolRequestEntry {
				origin: PathBuf::from("project/nested/outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1"),
					api_url: None,
					exposed: None,
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
mod group_tests {
	use super::*;

	#[test]
	fn groups_requests_by_identity() {
		let requests = vec![
			ToolRequestEntry {
				origin: std::path::PathBuf::from("a.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					api_url: None,
					exposed: None,
				},
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("b.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					api_url: None,
					exposed: None,
				},
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("c.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
					api_url: None,
					exposed: None,
				},
			},
		];
		let groups = ToolRequestGroup::from_entries(requests);
		assert_eq!(groups.len(), 2);
		assert_eq!(groups[0].tool, ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
			api_url: None,
			exposed: None,
		});
		assert_eq!(groups[0].origins, vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]);
		assert_eq!(groups[1].tool, ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
			api_url: None,
			exposed: None,
		});
		assert_eq!(groups[1].origins, vec![PathBuf::from("c.toml")]);
	}
}
