use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::catalog::Catalog;
use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::{RequestedTool, ToolRequestSet};

pub(crate) fn report(catalog: &Catalog<'_>, tools_dir: &Path) -> Report {
	let requests = catalog.requests();
	let mut requested_identities = HashSet::with_capacity(requests.len());
	let mut requested = Vec::with_capacity(requests.len());
	let mut checked: HashMap<&Path, bool> = HashMap::with_capacity(requests.len());
	for request in requests {
		let identity = request.tool.identity();
		requested_identities.insert(identity.clone());
		let (install_state, installed_version) = match catalog.lock_for(&identity) {
			Some(LockedTool::Github(lock)) => {
				let install_state = if _installed_cached(&mut checked, tools_dir, lock.install_path.as_path()) {
					InstallState::Installed
				} else {
					InstallState::Missing
				};
				(install_state, Some(lock.tag_name.clone()))
			}
			None => (InstallState::Missing, None),
		};
		requested.push(ToolStatus {
			origin: request.origin.clone(),
			install_state,
			installed_version,
			tool: request.tool.clone(),
		});
	}
	let mut unrequested = Vec::new();
	for tool in &catalog.lock().tools {
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
	pub(crate) tool: RequestedTool,
}

#[derive(Debug, PartialEq)]
pub(crate) enum InstallState {
	Installed,
	Missing,
}

pub(crate) fn missing_from_lock(request_sets: &[ToolRequestSet], lock: &Lock, tools_dir: &Path) -> Vec<ToolRequestSet> {
	let mut checked: HashMap<&Path, bool> = HashMap::with_capacity(lock.tools.len());
	let mut installed: HashSet<Identity> = HashSet::with_capacity(lock.tools.len());
	for tool in &lock.tools {
		if _installed_cached(&mut checked, tools_dir, tool.install_path()) {
			installed.insert(tool.identity());
		}
	}
	request_sets.iter().filter(|set| !installed.contains(&set.tool.identity())).cloned().collect()
}

fn _installed_cached<'a>(cache: &mut HashMap<&'a Path, bool>, tools_dir: &Path, install_path: &'a Path) -> bool {
	*cache.entry(install_path).or_insert_with(|| tools_dir.join(install_path).exists())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::tools::lock::GithubLock;
	use crate::tools::tool_request::{GithubRequest, ToolRequest};
	use crate::tools::version::Version;

	#[test]
	fn reports_installed_and_missing_requests() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let requests = vec![
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
					exposed: None,
					rename_shims: None,
				}),
			},
		];
		let report = report(&Catalog::new(&lock, &requests), &tools);
		assert_eq!(report.requested, vec![
			ToolStatus {
				origin: PathBuf::from("outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolStatus {
				origin: PathBuf::from("outpost.toml"),
				install_state: InstallState::Missing,
				installed_version: None,
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/other".to_owned(),
					lane: String::new(),
					version: Version::parse("2.0.0"),
					exposed: None,
					rename_shims: None,
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
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let requests = vec![ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
				exposed: None,
				rename_shims: None,
			}),
		}];
		let report = report(&Catalog::new(&lock, &requests), &tools);
		assert_eq!(report.requested, vec![ToolStatus {
			origin: PathBuf::from("outpost.toml"),
			install_state: InstallState::Missing,
			installed_version: Some("v1.2.3".to_owned()),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
				exposed: None,
				rename_shims: None,
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
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/old".to_owned(),
				lane: String::new(),
				requested: Version::parse("9.0.0"),
				tag_name: "v9.0.0".to_owned(),
				install_path: PathBuf::from("github_old_repo/9.0.0"),
			})],
		};
		let report = report(&Catalog::new(&lock, &[]), &tools);
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
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let requests = vec![
			ToolRequest {
				origin: PathBuf::from("system/outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolRequest {
				origin: PathBuf::from("project/outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
		];
		let report = report(&Catalog::new(&lock, &requests), &tools);
		assert_eq!(report.requested, vec![
			ToolStatus {
				origin: PathBuf::from("system/outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolStatus {
				origin: PathBuf::from("project/outpost.toml"),
				install_state: InstallState::Installed,
				installed_version: Some("v1.2.3".to_owned()),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.2.3"),
					exposed: None,
					rename_shims: None,
				}),
			},
		]);
	}

	#[test]
	fn reports_installed_requests_that_share_one_path() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.0.0")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![
				LockedTool::Github(GithubLock {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("latest"),
					tag_name: "v1.0.0".to_owned(),
					install_path: PathBuf::from("github_owner_repo/1.0.0"),
				}),
				LockedTool::Github(GithubLock {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("1.0.0"),
					tag_name: "v1.0.0".to_owned(),
					install_path: PathBuf::from("github_owner_repo/1.0.0"),
				}),
			],
		};
		let requests = vec![
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("latest"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolRequest {
				origin: PathBuf::from("outpost.toml"),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.0.0"),
					exposed: None,
					rename_shims: None,
				}),
			},
		];
		let report = report(&Catalog::new(&lock, &requests), &tools);
		assert_eq!(report.requested.len(), 2);
		for status in &report.requested {
			assert_eq!(status.install_state, InstallState::Installed);
			assert_eq!(status.installed_version.as_deref(), Some("v1.0.0"));
		}
		assert!(report.unrequested.is_empty());
	}

	#[test]
	fn treats_shared_install_path_as_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.0.0")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![
				LockedTool::Github(GithubLock {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("latest"),
					tag_name: "v1.0.0".to_owned(),
					install_path: PathBuf::from("github_owner_repo/1.0.0"),
				}),
				LockedTool::Github(GithubLock {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					requested: Version::parse("1.0.0"),
					tag_name: "v1.0.0".to_owned(),
					install_path: PathBuf::from("github_owner_repo/1.0.0"),
				}),
			],
		};
		let request_sets = ToolRequestSet::from_requests(&[
			ToolRequest {
				origin: PathBuf::new(),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("latest"),
					exposed: None,
					rename_shims: None,
				}),
			},
			ToolRequest {
				origin: PathBuf::new(),
				tool: RequestedTool::Github(GithubRequest {
					api_url: String::new(),
					full_name: "owner/repo".to_owned(),
					lane: String::new(),
					version: Version::parse("1.0.0"),
					exposed: None,
					rename_shims: None,
				}),
			},
		]);
		assert!(missing_from_lock(&request_sets, &lock, &tools).is_empty());
	}

	#[test]
	fn treats_tool_with_existing_directory_as_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let request_sets = ToolRequestSet::from_requests(&[ToolRequest {
			origin: PathBuf::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
				exposed: None,
				rename_shims: None,
			}),
		}]);
		assert!(missing_from_lock(&request_sets, &lock, &tools).is_empty());
	}

	#[test]
	fn treats_tool_with_missing_directory_as_not_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let request_sets = ToolRequestSet::from_requests(&[ToolRequest {
			origin: PathBuf::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.2.3"),
				exposed: None,
				rename_shims: None,
			}),
		}]);
		assert_eq!(missing_from_lock(&request_sets, &lock, &tools).len(), 1);
	}

	#[test]
	fn treats_different_request_as_not_installed() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let other_lane = RequestedTool::Github(GithubRequest {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			version: Version::parse("1.2.3"),
			exposed: None,
			rename_shims: None,
		});
		let other_version = RequestedTool::Github(GithubRequest {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
			exposed: None,
			rename_shims: None,
		});
		let request_sets = ToolRequestSet::from_requests(&[
			ToolRequest {
				origin: PathBuf::new(),
				tool: other_lane,
			},
			ToolRequest {
				origin: PathBuf::new(),
				tool: other_version,
			},
		]);
		assert_eq!(missing_from_lock(&request_sets, &lock, &tools).len(), 2);
	}

	#[test]
	fn treats_mixed_batch_as_partially_missing() {
		let dir = tempfile::tempdir().unwrap();
		let tools = dir.path().join("tools");
		std::fs::create_dir_all(tools.join("github_owner_repo/1.2.3")).unwrap();
		let lock = Lock {
			version: 1,
			tools: vec![LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.2.3"),
				tag_name: "v1.2.3".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.2.3"),
			})],
		};
		let installed = RequestedTool::Github(GithubRequest {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.2.3"),
			exposed: None,
			rename_shims: None,
		});
		let missing = RequestedTool::Github(GithubRequest {
			api_url: String::new(),
			full_name: "owner/other".to_owned(),
			lane: String::new(),
			version: Version::parse("2.0.0"),
			exposed: None,
			rename_shims: None,
		});
		let entries = [
			ToolRequest {
				origin: PathBuf::new(),
				tool: installed,
			},
			ToolRequest {
				origin: PathBuf::new(),
				tool: missing,
			},
		];
		let missing = missing_from_lock(&ToolRequestSet::from_requests(&entries), &lock, &tools);
		assert_eq!(missing.len(), 1);
		assert!(matches!(&missing[0].tool, RequestedTool::Github(req) if req.full_name == "owner/other"));
	}
}
