use std::fmt;

use regex::regex;

use super::tool_request::RequestedTool;
use super::version::{Version, split_lane};

#[derive(Debug, PartialEq)]
pub(crate) enum ToolSpec {
	Github {
		full_name: String,
		lane: String,
		version: Option<Version>,
	},
}

impl ToolSpec {
	pub(crate) fn parse(spec: &str) -> Result<Self, ToolSpecError> {
		let (backend, rest) = spec.split_once(':').ok_or_else(|| ToolSpecError::MissingBackend(spec.to_owned()))?;
		match backend {
			"github" => {
				let (full_name, version) = match rest.split_once('@') {
					Some((full_name, version)) => (full_name, Some(version)),
					None => (rest, None),
				};
				if regex!(r"^(?:\.{1,2})?$|[^a-zA-Z0-9_./-]").is_match(full_name) {
					return Err(ToolSpecError::InvalidFullName(full_name.to_owned()));
				}
				let (lane, version) = match version {
					Some(version) => {
						let (lane, value) = split_lane(version.trim());
						if lane.is_empty() && value.is_empty() {
							return Err(ToolSpecError::EmptyVersion(spec.to_owned()));
						}
						(lane.to_owned(), (!value.is_empty()).then(|| Version::parse(value)))
					}
					None => (String::new(), None),
				};
				Ok(Self::Github {
					full_name: full_name.to_ascii_lowercase(),
					lane,
					version,
				})
			}
			_ => Err(ToolSpecError::UnsupportedBackend(backend.to_owned())),
		}
	}

	pub(crate) fn matches(&self, request: &RequestedTool) -> bool {
		match (self, request) {
			(
				Self::Github { full_name, lane, version },
				RequestedTool::Github {
					full_name: request_name,
					lane: request_lane,
					version: request_version,
					..
				},
			) => {
				full_name == request_name
					&& (lane == request_lane || (lane.is_empty() && version.is_none()))
					&& version.as_ref().is_none_or(|version| version == request_version)
			}
		}
	}
}

#[derive(Debug, PartialEq)]
pub(crate) enum ToolSpecError {
	MissingBackend(String),
	UnsupportedBackend(String),
	InvalidFullName(String),
	EmptyVersion(String),
}

impl fmt::Display for ToolSpecError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::MissingBackend(spec) => write!(f, "invalid tool spec '{spec}': expected '<backend>:<tool>[@<version>]'"),
			Self::UnsupportedBackend(backend) => write!(f, "unsupported backend '{backend}'"),
			Self::InvalidFullName(name) => write!(f, "invalid tool name '{name}'"),
			Self::EmptyVersion(spec) => write!(f, "invalid tool spec '{spec}': empty version"),
		}
	}
}

impl std::error::Error for ToolSpecError {}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_specs() {
		assert_eq!(ToolSpec::parse("github:owner/repo").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: None,
		});
		assert_eq!(ToolSpec::parse("github:owner/repo@cli/").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: None,
		});
		assert_eq!(ToolSpec::parse("github:owner/repo@latest").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Some(Version::Latest),
		});
		assert_eq!(ToolSpec::parse("github:owner/repo@cli/latest").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Some(Version::Latest),
		});
		assert_eq!(ToolSpec::parse("github:owner/repo@cli/1.2.3").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Some(Version::Exact("1.2.3".to_owned())),
		});
		assert_eq!(ToolSpec::parse("github:owner/repo@cli@1.2.3").unwrap(), ToolSpec::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli@".to_owned(),
			version: Some(Version::Exact("1.2.3".to_owned())),
		});
	}

	#[test]
	fn matches_requests() {
		let latest = ToolSpec::parse("github:owner/repo@latest").unwrap();
		let request = RequestedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::Latest,
			api_url: None,
			exposed: None,
			rename_shims: None,
		};
		assert!(latest.matches(&request));

		let exact = RequestedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::Exact("1.0.0".to_owned()),
			api_url: None,
			exposed: None,
			rename_shims: None,
		};
		assert!(!latest.matches(&exact));

		let any = ToolSpec::parse("github:owner/repo").unwrap();
		assert!(any.matches(&request));
		assert!(any.matches(&exact));

		let lane = ToolSpec::parse("github:owner/repo@cli/1.0.0").unwrap();
		let lane_request = RequestedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Version::Exact("1.0.0".to_owned()),
			api_url: None,
			exposed: None,
			rename_shims: None,
		};
		assert!(lane.matches(&lane_request));
		assert!(!lane.matches(&exact));

		let any_lane_version = ToolSpec::parse("github:owner/repo@cli/").unwrap();
		assert!(any_lane_version.matches(&lane_request));
		assert!(any_lane_version.matches(&RequestedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Version::Latest,
			api_url: None,
			exposed: None,
			rename_shims: None,
		}));
		assert!(!any_lane_version.matches(&exact));
		assert!(!any_lane_version.matches(&RequestedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: "other/".to_owned(),
			version: Version::Latest,
			api_url: None,
			exposed: None,
			rename_shims: None,
		}));
	}

	#[test]
	fn rejects_invalid_specs() {
		let specs = vec![
			"owner/repo@1",
			"gitlab:owner/repo@1",
			"github:owner/repo@",
			"github:@1",
			"github:.@1",
			"github:owner repo@1",
		];
		let errors = vec![
			ToolSpecError::MissingBackend("owner/repo@1".to_owned()),
			ToolSpecError::UnsupportedBackend("gitlab".to_owned()),
			ToolSpecError::EmptyVersion("github:owner/repo@".to_owned()),
			ToolSpecError::InvalidFullName(String::new()),
			ToolSpecError::InvalidFullName(".".to_owned()),
			ToolSpecError::InvalidFullName("owner repo".to_owned()),
		];
		for (spec, error) in specs.into_iter().zip(errors) {
			assert_eq!(ToolSpec::parse(spec), Err(error), "{spec}");
		}
	}
}
