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
	Github { full_name: String, lane: String, version: Version },
}

impl ToolRequest {
	pub(crate) fn identity(&self) -> Identity {
		match self {
			Self::Github { full_name, lane, version } => ("github", full_name.clone(), lane.clone(), version.clone()),
		}
	}

	fn parse(path: &Path, key: &str, options: &ToolOptions) -> Result<Self, ToolRequestError> {
		let (backend, full_name) = key.split_once(':').ok_or_else(|| ToolRequestError::UnsupportedBackend {
			path: path.to_path_buf(),
			key: key.to_owned(),
		})?;
		match backend {
			"github" => {
				if regex!(r"^(?:\.{1,2})?$|[^a-zA-Z0-9_./-]").is_match(full_name) {
					return Err(ToolRequestError::InvalidFullName {
						path: path.to_path_buf(),
						key: key.to_owned(),
					});
				}
				let version = options.version().trim();
				if version.is_empty() {
					return Err(ToolRequestError::EmptyVersion {
						path: path.to_path_buf(),
						key: key.to_owned(),
					});
				}
				let (lane, value) = split_lane(version);
				Ok(Self::Github {
					full_name: full_name.to_ascii_lowercase(),
					lane: lane.to_owned(),
					version: Version::parse(value),
				})
			}
			_ => Err(ToolRequestError::UnsupportedBackend {
				path: path.to_path_buf(),
				key: key.to_owned(),
			}),
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
	UnsupportedBackend { path: PathBuf, key: String },
	InvalidFullName { path: PathBuf, key: String },
	EmptyVersion { path: PathBuf, key: String },
}

impl fmt::Display for ToolRequestError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnsupportedBackend { path, key } => write!(f, "unsupported backend in `{key}` from `{}`", path.display()),
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
			},
		}]);
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
				},
			},
			ToolRequestEntry {
				origin: PathBuf::from("project/outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2"),
				},
			},
			// the same request from another file is still a separate entry
			ToolRequestEntry {
				origin: PathBuf::from("project/nested/outpost.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1"),
				},
			},
		]);
	}

	#[test]
	fn rejects_unsupported_backend() {
		let config = ConfigFile {
			path: PathBuf::from("outpost.toml"),
			config: toml::from_str(
				r#"
				[tools]
				"rust" = "latest"
				"#,
			)
			.unwrap(),
		};
		let error = requests(&[config]).unwrap_err();
		assert!(matches!(&error, ToolRequestError::UnsupportedBackend { .. }));
		assert!(error.to_string().contains("outpost.toml"));
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
				},
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("b.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				},
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("c.toml"),
				tool: ToolRequest::Github {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
				},
			},
		];
		let groups = ToolRequestGroup::from_entries(requests);
		assert_eq!(groups.len(), 2);
		assert_eq!(groups[0].tool, ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		});
		assert_eq!(groups[0].origins, vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]);
		assert_eq!(groups[1].tool, ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
		});
		assert_eq!(groups[1].origins, vec![PathBuf::from("c.toml")]);
	}
}
