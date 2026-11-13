use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::lock::{Lock, LockedTool};
use super::tool_request::ToolRequest;
use super::version::Version;

pub type Identity = (&'static str, String, String, Version);

pub fn report(requests: &[ToolRequest], lock: &Lock, tools_dir: &Path) -> Report {
	let entries: HashMap<Identity, &LockedTool> = lock.tools.iter().map(|tool| (_locked_identity(tool), tool)).collect();
	let mut requested_identities = HashSet::with_capacity(requests.len());
	let mut requested = Vec::with_capacity(requests.len());
	for request in requests {
		let identity = _request_identity(request);
		requested_identities.insert(identity.clone());
		requested.push(match request {
			ToolRequest::Github { full_name, lane, version } => {
				let (install_state, locked_version) = match entries.get(&identity).copied() {
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
				ToolStatus::Github {
					full_name: full_name.clone(),
					lane: lane.clone(),
					requested: version.clone(),
					tag_name: locked_version,
					install_state,
				}
			}
		});
	}
	let mut unrequested = Vec::new();
	for tool in &lock.tools {
		if !requested_identities.contains(&_locked_identity(tool)) {
			unrequested.push(tool.clone());
		}
	}
	Report { requested, unrequested }
}

pub struct Report {
	pub requested: Vec<ToolStatus>,
	pub unrequested: Vec<LockedTool>,
}

#[derive(Debug, PartialEq)]
pub enum ToolStatus {
	Github {
		full_name: String,
		lane: String,
		requested: Version,
		tag_name: Option<String>,
		install_state: InstallState,
	},
}

#[derive(Debug, PartialEq)]
pub enum InstallState {
	Installed,
	Missing,
}

pub fn missing_from_lock(requests: Vec<ToolRequest>, lock: &Lock, tools_dir: &Path) -> Vec<ToolRequest> {
	let installed: HashSet<Identity> = lock
		.tools
		.iter()
		.filter(|tool| match tool {
			LockedTool::Github { install_path, .. } => _installed(install_path, tools_dir),
		})
		.map(_locked_identity)
		.collect();
	requests
		.into_iter()
		.filter(|request| !installed.contains(&_request_identity(request)))
		.collect()
}

fn _request_identity(request: &ToolRequest) -> Identity {
	match request {
		ToolRequest::Github { full_name, lane, version } => ("github", full_name.clone(), lane.clone(), version.clone()),
	}
}

fn _locked_identity(tool: &LockedTool) -> Identity {
	match tool {
		LockedTool::Github {
			full_name, lane, requested, ..
		} => ("github", full_name.clone(), lane.clone(), requested.clone()),
	}
}

fn _installed(install_path: &str, tools_dir: &Path) -> bool {
	tools_dir.join(install_path).exists()
}

#[cfg(test)]
mod tests {
	use super::*;

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
			ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
			},
			ToolRequest::Github {
				full_name: "owner/other".to_owned(),
				lane: String::new(),
				version: Version::parse("2.0.0"),
			},
		];
		let report = report(&requests, &lock, &tools);
		assert_eq!(report.requested, vec![
			ToolStatus::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: Some("v1.2.3".to_owned()),
				install_state: InstallState::Installed,
			},
			ToolStatus::Github {
				full_name: "owner/other".to_owned(),
				lane: String::new(),
				requested: Version::parse("2.0.0"),
				tag_name: None,
				install_state: InstallState::Missing,
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
		let requests = vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		}];
		let report = report(&requests, &lock, &tools);
		assert_eq!(report.requested, vec![ToolStatus::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.2.3"),
			tag_name: Some("v1.2.3".to_owned()),
			install_state: InstallState::Missing,
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
		let report = report(&[], &lock, &tools);
		assert!(report.requested.is_empty());
		assert_eq!(report.unrequested, lock.tools);
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
		let requests = vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		}];
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
		let requests = vec![ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		}];
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
		let other_lane = ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Version::parse("1.2.3"),
		};
		let other_version = ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
		};
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
		let installed = ToolRequest::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
		};
		let missing = ToolRequest::Github {
			full_name: "owner/other".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
		};
		let requests = missing_from_lock(vec![installed, missing], &lock, &tools);
		assert_eq!(requests.len(), 1);
		assert!(matches!(&requests[0], ToolRequest::Github { full_name, .. } if full_name == "owner/other"));
	}
}
