use std::fmt;

use regex::regex;

use super::version::{Version, split_lane};
use crate::config::loader::{Config, ToolOptions};

pub fn requests(config: &Config) -> Result<Vec<ToolRequest>, ToolRequestError> {
	config.tools.iter().map(|(key, options)| ToolRequest::parse(key, options)).collect()
}

#[derive(Debug, PartialEq)]
pub enum ToolRequest {
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
pub enum ToolRequestError {
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

	fn config(toml: &str) -> Config {
		toml::from_str(toml).unwrap()
	}

	#[test]
	fn parses_latest() {
		let config = config(
			r#"
			[tools]
			"github:owner/repo" = "latest"
			"#,
		);
		assert_eq!(requests(&config).unwrap(), vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::Latest,
		}]);
	}

	#[test]
	fn parses_exact() {
		let config = config(
			r#"
			[tools]
			"github:owner/repo" = "0.26.0"
			"#,
		);
		assert_eq!(requests(&config).unwrap(), vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("0.26.0"),
		}]);
	}

	#[test]
	fn parses_lanes() {
		let configs = [
			r#"
			[tools]
			"github:owner/repo" = "cli/1.2.3"
			"#,
			r#"
			[tools]
			"github:owner/repo" = "tool-10"
			"#,
		];
		let expected = [
			ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: "cli/".to_owned(),
				version: Version::Exact("1.2.3".to_owned()),
			},
			ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: "tool-".to_owned(),
				version: Version::parse("10"),
			},
		];
		for (toml, expected) in configs.into_iter().zip(expected) {
			assert_eq!(requests(&config(toml)).unwrap(), vec![expected], "{toml}");
		}
	}

	#[test]
	fn parses_dotted_full_name() {
		let config = config(
			r#"
			[tools]
			"github:owner/repo.js" = "latest"
			"#,
		);
		assert_eq!(requests(&config).unwrap(), vec![ToolRequest::Github {
			full_name: "owner/repo.js".to_owned(),
			lane: String::new(),
			version: Version::Latest,
		}]);
	}

	#[test]
	fn parses_table_form() {
		let config = config(
			r#"
			[tools]
			"github:owner/repo" = { version = "0.26.0" }
			"#,
		);
		assert_eq!(requests(&config).unwrap(), vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("0.26.0"),
		}]);
	}

	#[test]
	fn rejects_unsupported_backend() {
		let config = config(
			r#"
			[tools]
			"rust" = "latest"
			"#,
		);
		assert!(matches!(requests(&config), Err(ToolRequestError::UnsupportedBackend(_))));
	}

	#[test]
	fn rejects_invalid_full_name() {
		for key in ["github:", "github:.", "github:..", r"github:a\b"] {
			let config = config(&format!(
				r#"
				[tools]
				'{key}' = "latest"
				"#,
			));
			assert!(
				matches!(requests(&config), Err(ToolRequestError::InvalidFullName(_))),
				"{key} should be rejected"
			);
		}
	}

	#[test]
	fn rejects_empty_version() {
		let config = config(
			r#"
			[tools]
			"github:owner/repo" = ""
			"#,
		);
		assert!(matches!(requests(&config), Err(ToolRequestError::EmptyVersion(_))));
	}
}
