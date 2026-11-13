use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::{ToolRequest, ToolRequestEntry};

pub(crate) fn report(requests: Vec<ToolRequestEntry>, lock: &Lock, tools_dir: &Path) -> Report {
	let locked_entries: HashMap<Identity, &LockedTool> = lock.tools.iter().map(|tool| (tool.identity(), tool)).collect();
	let mut requested_identities = HashSet::with_capacity(requests.len());
	let mut requested = Vec::with_capacity(requests.len());
	for request in requests {
		let identity = request.tool.identity();
		requested_identities.insert(identity.clone());
		let (install_state, installed_version) = match locked_entries.get(&identity).copied() {
			Some(LockedTool::Github { tag_name, install_path, .. }) => {
				let install_state = if _installed(install_path, tools_dir) {
					InstallState::Installed
				} else {
					InstallState::Missing
				};
				(install_state, Some(tag_name.clone()))
			}
			None => (InstallState::Missing, None),
		};
		requested.push(ToolStatus {
			origin: request.origin,
			install_state,
			installed_version,
			tool: request.tool,
		});
	}
	let mut unrequested = Vec::new();
	for tool in &lock.tools {
		if !requested_identities.contains(&tool.identity()) {
			unrequested.push(tool.clone());
		}
	}
	Report { requested, unrequested }
}

pub(crate) struct Report {
	pub(crate) requested: Vec<ToolStatus>,
	pub(crate) unrequested: Vec<LockedTool>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ToolStatus {
	pub(crate) origin: PathBuf,
	pub(crate) install_state: InstallState,
	pub(crate) installed_version: Option<String>,
	pub(crate) tool: ToolRequest,
}

#[derive(Debug, PartialEq)]
pub(crate) enum InstallState {
	Installed,
	Missing,
}

pub(crate) fn unique_requests(requests: Vec<ToolRequestEntry>) -> Vec<ToolRequest> {
	let mut seen = HashSet::with_capacity(requests.len());
	let mut unique = Vec::with_capacity(requests.len());
	for request in requests {
		if seen.insert(request.tool.identity()) {
			unique.push(request.tool);
		}
	}
	unique
}

pub(crate) fn missing_from_lock(requests: Vec<ToolRequest>, lock: &Lock, tools_dir: &Path) -> Vec<ToolRequest> {
	let installed: HashSet<Identity> = lock
		.tools
		.iter()
		.filter(|tool| match tool {
			LockedTool::Github { install_path, .. } => _installed(install_path, tools_dir),
		})
		.map(LockedTool::identity)
		.collect();
	requests.into_iter().filter(|request| !installed.contains(&request.identity())).collect()
}

fn _installed(install_path: &str, tools_dir: &Path) -> bool {
	tools_dir.join(install_path).exists()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::tools::tool_request::GithubRequest;
	use crate::tools::version::Version;

	#[test]
	fn reports_installed_and_missing_requests() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let requests = vec![
			ToolRequestEntry {
				origin: PathBuf::from("outpost.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolRequestEntry {
				origin: PathBuf::from("outpost.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
				}),
			},
		];
		let report = report(requests, &lock, &tools);
		assert_eq!(report.requested, vec![
			ToolStatus {
				origin: PathBuf::from("outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolStatus {
				origin: PathBuf::from("outpost.toml"),
				install_state: InstallState::Missing,
				installed_version: None,
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
				}),
			},
		]);
		assert!(report.unrequested.is_empty());
	}

	#[test]
	fn reports_stale_lock_entry_with_tag() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(&tools).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let requests = vec![ToolRequestEntry {
			origin: PathBuf::from("outpost.toml"),
			tool: ToolRequest::Github(GithubRequest {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
			}),
		}];
		let report = report(requests, &lock, &tools);
		assert_eq!(report.requested, vec![ToolStatus {
			origin: PathBuf::from("outpost.toml"),
			install_state: InstallState::Missing,
			installed_version: Some("v1.2.3".to_owned()),
			tool: ToolRequest::Github(GithubRequest {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
			}),
		}]);
		assert!(report.unrequested.is_empty());
	}

	#[test]
	fn reports_unrequested_entries() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/old".to_owned(),
				lane: String::new(),
				requested: Version::parse("9.0.0"),
				tag_name: "v9.0.0".to_owned(),
				install_path: "github_old_repo/9.0.0".to_owned(),
			}],
		};
		let report = report(Vec::new(), &lock, &tools);
		assert!(report.requested.is_empty());
		assert_eq!(report.unrequested, lock.tools);
	}

	#[test]
	fn reports_duplicate_requests_per_origin() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let requests = vec![
			ToolRequestEntry {
				origin: PathBuf::from("system/outpost.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolRequestEntry {
				origin: PathBuf::from("project/outpost.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
		];
		let report = report(requests, &lock, &tools);
		assert_eq!(report.requested, vec![
			ToolStatus {
				origin: PathBuf::from("system/outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolStatus {
				origin: PathBuf::from("project/outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
		]);
	}

	#[test]
	fn deduplicates_requests_by_identity() {
		let requests = vec![
			ToolRequestEntry {
				origin: std::path::PathBuf::from("a.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("b.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
				}),
			},
			ToolRequestEntry {
				origin: std::path::PathBuf::from("c.toml"),
				tool: ToolRequest::Github(GithubRequest {
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
				}),
			},
		];
		assert_eq!(unique_requests(requests), vec![
			ToolRequest::Github(GithubRequest {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
			}),
			ToolRequest::Github(GithubRequest {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("2.0.0"),
			}),
		]);
	}

	#[test]
	fn treats_tool_with_existing_directory_as_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let requests = vec![ToolRequest::Github(GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		})];
		assert!(missing_from_lock(requests, &lock, &tools).is_empty());
	}

	#[test]
	fn treats_tool_with_missing_directory_as_not_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let requests = vec![ToolRequest::Github(GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		})];
		assert_eq!(missing_from_lock(requests, &lock, &tools).len(), 1);
	}

	#[test]
	fn treats_different_request_as_not_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let other_lane = ToolRequest::Github(GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Version::parse("1.2.3"),
		});
		let other_version = ToolRequest::Github(GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
		});
		let requests = vec![other_lane, other_version];
		assert_eq!(missing_from_lock(requests, &lock, &tools).len(), 2);
	}

	#[test]
	fn treats_mixed_batch_as_partially_missing() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: "github_owner_repo/1.2.3".to_owned(),
			}],
		};
		let installed = ToolRequest::Github(GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		});
		let missing = ToolRequest::Github(GithubRequest {
			full_name: "owner/other".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
		});
		let requests = missing_from_lock(vec![installed, missing], &lock, &tools);
		assert_eq!(requests.len(), 1);
		assert!(matches!(&requests[0], ToolRequest::Github(GithubRequest { full_name, .. }) if full_name == "owner/other"));
	}
}
