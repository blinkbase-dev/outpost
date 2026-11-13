use std::fmt;
use std::path::PathBuf;

use regex::regex;

use super::version::{Version, split_lane};
use crate::config::loader::{ConfigFile, ToolOptions};

pub(crate) fn requests(files: &[ConfigFile]) -> Result<Vec<ToolRequestEntry>, ToolRequestError> {
	let mut requests = Vec::new();
	for file in files {
		for (key, options) in &file.config.tools {
			requests.push(ToolRequestEntry {
				origin: file.path.clone(),
				tool: ToolRequest::parse(key, options)?,
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
	fn parse(key: &str, options: &ToolOptions) -> Result<Self, ToolRequestError> {
		let (backend, full_name) = key.split_once(':').ok_or_else(|| ToolRequestError::UnsupportedBackend(key.to_owned()))?;
		match backend {
			"github" => {
				if regex!(r"^(?:\.{1,2})?$|[^a-zA-Z0-9_./-]").is_match(full_name) {
					return Err(ToolRequestError::InvalidFullName(key.to_owned()));
				}
				let version = options.version().trim();
				if version.is_empty() {
					return Err(ToolRequestError::EmptyVersion(key.to_owned()));
				}
				let (lane, value) = split_lane(version);
				Ok(Self::Github {
					full_name: full_name.to_owned(),
					lane: lane.to_owned(),
					version: Version::parse(value),
				})
			}
			_ => Err(ToolRequestError::UnsupportedBackend(key.to_owned())),
		}
	}
}

#[derive(Debug)]
pub(crate) enum ToolRequestError {
	UnsupportedBackend(String),
	InvalidFullName(String),
	EmptyVersion(String),
}

impl fmt::Display for ToolRequestError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::UnsupportedBackend(key) => write!(f, "unsupported backend in `{key}`"),
			Self::InvalidFullName(key) => write!(f, "invalid full name in `{key}`"),
			Self::EmptyVersion(key) => write!(f, "missing version for `{key}`"),
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
		assert!(matches!(requests(&[config]), Err(ToolRequestError::UnsupportedBackend(_))));
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
				matches!(requests(&[config]), Err(ToolRequestError::InvalidFullName(_))),
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
		assert!(matches!(requests(&[config]), Err(ToolRequestError::EmptyVersion(_))));
	}
}
