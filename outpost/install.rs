use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
use crate::tools::archive::{self, ToolDownloaded, ToolResolved};
use crate::tools::lock::{self, Lock, LockedTool};
use crate::tools::tool_request::{self, ToolRequest};
use crate::tools::version::Version;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Identity = (&'static str, String, String, Version);

pub fn install() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config = loader::load(&paths)?;
	let mut requests = tool_request::requests(&config)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let downloads_dir = config_dir.join("downloads");
	let tools_dir = config_dir.join("tools");
	std::fs::create_dir_all(&downloads_dir)?;
	std::fs::create_dir_all(&tools_dir)?;
	let _install_lock = lock::acquire(config_dir)?;
	// skip tools already recorded in the lock
	let mut lock = lock::read(config_dir)?;
	let requests = missing_from_lock(&lock, requests, &tools_dir);
	if requests.is_empty() {
		return Ok(());
	}
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (downloaded, failed) = runtime.block_on(resolve_and_download(http, requests, downloads_dir))?;
	drop(runtime);
	let (installed, extract_failed) = archive::extract_all(downloaded, tools_dir.clone())?;
	for (tool, dir) in &installed {
		let ToolDownloaded::Github { full_name, tag_name, .. } = tool;
		println!("{full_name} {tag_name} {}", dir.display());
	}
	// record successful installs in the lock
	update_lock(&mut lock, &installed, &tools_dir)?;
	lock::write(config_dir, &lock)?;
	let failed = failed + extract_failed;
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
}

async fn resolve_and_download(http: Client, requests: Vec<ToolRequest>, downloads_dir: PathBuf) -> Result<(Vec<ToolDownloaded>, usize), Error> {
	let (resolved, resolve_failed) = resolve_parallel(http.clone(), requests).await?;
	let (downloaded, download_failed) = archive::download_parallel(http, resolved, downloads_dir).await?;
	Ok((downloaded, resolve_failed + download_failed))
}

async fn resolve_parallel(http: Client, requests: Vec<ToolRequest>) -> Result<(Vec<ToolResolved>, usize), Error> {
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(requests.len());
	for request in requests {
		let semaphore = Arc::clone(&semaphore);
		let http = http.clone();
		handles.push(tokio::spawn(async move {
			let result = match semaphore.acquire_owned().await {
				Ok(_permit) => resolve(http, &request).await,
				Err(error) => Err(error.into()),
			};
			(request, result)
		}));
	}
	let mut resolved = Vec::with_capacity(handles.len());
	let mut failed = 0;
	for handle in handles {
		let (request, result) = handle.await?;
		match result {
			Ok(item) => resolved.push(item),
			Err(error) => {
				match &request {
					ToolRequest::Github { full_name, .. } => eprintln!("{full_name}: {error}"),
				}
				failed += 1;
			}
		}
	}
	Ok((resolved, failed))
}

async fn resolve(http: Client, request: &ToolRequest) -> Result<ToolResolved, Error> {
	match request {
		ToolRequest::Github { full_name, lane, version } => {
			let backend = GithubBackend::new(http, GithubOptions::from_process());
			let release = backend.select_release(full_name, lane, version).await?;
			let (url, size, format) = backend.select_asset(full_name, &release)?;
			Ok(ToolResolved::Github {
				full_name: full_name.clone(),
				lane: lane.clone(),
				requested: version.clone(),
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}

fn missing_from_lock(lock: &Lock, requests: Vec<ToolRequest>, tools_dir: &Path) -> Vec<ToolRequest> {
	let installed: HashSet<Identity> = lock
		.tools
		.iter()
		.filter_map(|entry| match entry {
			LockedTool::Github {
				full_name,
				lane,
				requested,
				install_path,
				..
			} => {
				if tools_dir.join(install_path).exists() {
					Some(("github", full_name.clone(), lane.clone(), requested.clone()))
				} else {
					None
				}
			}
		})
		.collect();
	requests
		.into_iter()
		.filter(|request| match request {
			ToolRequest::Github { full_name, lane, version } => !installed.contains(&("github", full_name.clone(), lane.clone(), version.clone())),
		})
		.collect()
}

fn update_lock(lock: &mut Lock, installed: &[(ToolDownloaded, PathBuf)], tools_dir: &Path) -> Result<(), Error> {
	// identities of the tools being installed
	let incoming: HashSet<Identity> = installed
		.iter()
		.map(|(tool, _)| match tool {
			ToolDownloaded::Github {
				full_name, lane, requested, ..
			} => ("github", full_name.clone(), lane.clone(), requested.clone()),
		})
		.collect();
	// drop entries replaced by a new install before appending
	lock.tools.retain(|existing| match existing {
		LockedTool::Github {
			full_name, lane, requested, ..
		} => !incoming.contains(&("github", full_name.clone(), lane.clone(), requested.clone())),
	});
	for (tool, dir) in installed {
		let install_path = dir.strip_prefix(tools_dir)?.to_string_lossy().replace('\\', "/");
		match tool {
			ToolDownloaded::Github {
				full_name,
				lane,
				requested,
				tag_name,
				..
			} => lock.tools.push(LockedTool::Github {
				full_name: full_name.clone(),
				lane: lane.clone(),
				requested: requested.clone(),
				tag_name: tag_name.clone(),
				install_path,
			}),
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::backend::ArchiveFormat;
	use crate::tools::version::Version;

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
		assert!(missing_from_lock(&lock, requests, &tools).is_empty());
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
		assert_eq!(missing_from_lock(&lock, requests, &tools).len(), 1);
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
		assert_eq!(missing_from_lock(&lock, vec![other_lane, other_version], &tools).len(), 2);
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
		let requests = missing_from_lock(&lock, vec![installed, missing], &tools);
		assert_eq!(requests.len(), 1);
		assert!(matches!(&requests[0], ToolRequest::Github { full_name, .. } if full_name == "owner/other"));
	}

	#[test]
	fn locks_installed_tools() {
		let tools = PathBuf::from("tools");
		let installed = vec![(
			ToolDownloaded::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.2.3".to_owned(),
				path: PathBuf::from("tool.tar.gz"),
				format: ArchiveFormat::TarGz,
			},
			tools.join("github_owner_repo").join("1.2.3"),
		)];
		let mut lock = Lock::default();
		update_lock(&mut lock, &installed, &tools).unwrap();
		assert_eq!(lock.tools, vec![LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.2.3".to_owned(),
			install_path: "github_owner_repo/1.2.3".to_owned(),
		}]);
	}

	#[test]
	fn locks_newer_tag_replacing_older() {
		let tools = PathBuf::from("tools");
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("latest"),
			tag_name: "v1.0.0".to_owned(),
			install_path: "github_owner_repo/1.0.0".to_owned(),
		});
		let installed = vec![(
			ToolDownloaded::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("latest"),
				tag_name: "v2.0.0".to_owned(),
				path: PathBuf::from("tool.tar.gz"),
				format: ArchiveFormat::TarGz,
			},
			tools.join("github_owner_repo/2.0.0"),
		)];
		update_lock(&mut lock, &installed, &tools).unwrap();
		assert_eq!(lock.tools, vec![LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("latest"),
			tag_name: "v2.0.0".to_owned(),
			install_path: "github_owner_repo/2.0.0".to_owned(),
		}]);
	}

	#[test]
	fn locks_requests_separately() {
		let tools = PathBuf::from("tools");
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			requested: Version::parse("1"),
			tag_name: "cli/v1.0.0".to_owned(),
			install_path: "github_owner_repo/cli_v1.0.0".to_owned(),
		});
		let installed = vec![(
			ToolDownloaded::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				path: PathBuf::from("tool.tar.gz"),
				format: ArchiveFormat::TarGz,
			},
			tools.join("github_owner_repo/1.0.0"),
		)];
		update_lock(&mut lock, &installed, &tools).unwrap();
		assert_eq!(lock.tools, vec![
			LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: "cli/".to_owned(),
				requested: Version::parse("1"),
				tag_name: "cli/v1.0.0".to_owned(),
				install_path: "github_owner_repo/cli_v1.0.0".to_owned(),
			},
			LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				install_path: "github_owner_repo/1.0.0".to_owned(),
			}
		]);
	}

	#[test]
	fn locks_unrelated_tools() {
		let tools = PathBuf::from("tools");
		let other = LockedTool::Github {
			full_name: "other/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.0.0".to_owned(),
			install_path: "github_other_repo/1.0.0".to_owned(),
		};
		let mut lock = Lock {
			version: 1,
			tools: vec![other.clone()],
		};
		let installed = vec![(
			ToolDownloaded::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				path: PathBuf::from("tool.tar.gz"),
				format: ArchiveFormat::TarGz,
			},
			tools.join("github_owner_repo/1.0.0"),
		)];
		update_lock(&mut lock, &installed, &tools).unwrap();
		assert_eq!(lock.tools, vec![other, LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.0.0".to_owned(),
			install_path: "github_owner_repo/1.0.0".to_owned(),
		}]);
	}

	#[test]
	fn rejects_paths_outside_tools() {
		let installed = vec![(
			ToolDownloaded::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				path: PathBuf::from("tool.tar.gz"),
				format: ArchiveFormat::TarGz,
			},
			PathBuf::from("elsewhere/owner_repo/1.0.0"),
		)];
		assert!(update_lock(&mut Lock::default(), &installed, Path::new("tools")).is_err());
	}
}
