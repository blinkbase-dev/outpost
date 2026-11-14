use std::collections::HashSet;
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
use crate::tools::archive::{self, ToolArchive, ToolResolution, ToolResolutionGroup};
use crate::tools::lock::{self, Identity, Lock, LockedTool};
use crate::tools::status;
use crate::tools::tool_request::{self, ToolRequest, ToolRequestGroup};

type Error = Box<dyn std::error::Error + Send + Sync>;
type ResolutionEntry = (ToolRequestGroup, ToolResolution);
type ResolutionFailure = (ToolRequestGroup, Error);
type DownloadEntry = (ToolResolutionGroup, ToolArchive);

pub(crate) fn install() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let configs = loader::load(&paths)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let downloads_dir = config_dir.join("downloads");
	let tools_dir = config_dir.join("tools");
	std::fs::create_dir_all(&downloads_dir)?;
	std::fs::create_dir_all(&tools_dir)?;
	let _install_lock = lock::acquire(config_dir)?;
	let requests = tool_request::requests(&configs)?;
	// group requests by identity so each tool resolves once
	let requests = ToolRequestGroup::from_entries(requests);
	// skip tools already recorded in the lock
	let mut lock = lock::read(config_dir)?;
	let requests = status::missing_from_lock(requests, &lock, &tools_dir);
	if requests.is_empty() {
		return Ok(());
	}
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (resolutions, resolve_err) = runtime.block_on(resolve_parallel(requests, http.clone()))?;
	// group resolutions by artifact so each tool downloads once
	let resolutions = ToolResolutionGroup::from_entries(resolutions)?;
	let (archives, download_err) = runtime.block_on(archive::download_parallel(resolutions, http, downloads_dir))?;
	drop(runtime);
	let (extractions, extract_err) = archive::extract_parallel(archives, &tools_dir)?;
	let installed_artifacts = _update_lock_entries(&mut lock, extractions);
	lock::write(config_dir, &lock)?;
	// representative rows are appended after duplicate rows
	for tool in &lock.tools[lock.tools.len() - installed_artifacts..] {
		match tool {
			LockedTool::Github {
				full_name,
				tag_name,
				install_path,
				..
			} => println!("{full_name} {tag_name} {}", tools_dir.join(install_path).display()),
		}
	}
	let failures = resolve_err
		.into_iter()
		.map(|(request, error)| (vec![request], error))
		.chain(download_err.into_iter().map(|(group, error)| (group.requests, error)))
		.chain(extract_err.into_iter().map(|(group, error)| (group.requests, error)))
		.collect::<Vec<_>>();
	for (requests, error) in &failures {
		for request in requests {
			match &request.tool {
				ToolRequest::Github { full_name, lane, version } => {
					eprintln!("{full_name} {lane}{version}: {error}");
				}
			}
			for origin in &request.origins {
				eprintln!("  from {}", origin.display());
			}
		}
	}
	let failed = failures.iter().map(|(requests, _)| requests.len()).sum::<usize>();
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
}

async fn resolve_parallel(requests: Vec<ToolRequestGroup>, http: Client) -> Result<(Vec<ResolutionEntry>, Vec<ResolutionFailure>), Error> {
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(requests.len());
	for request in requests {
		let semaphore = Arc::clone(&semaphore);
		let http = http.clone();
		handles.push(tokio::spawn(async move {
			let result = match semaphore.acquire_owned().await {
				Ok(_permit) => resolve(http, &request.tool).await,
				Err(error) => Err(error.into()),
			};
			(request, result)
		}));
	}
	let mut resolutions = Vec::with_capacity(handles.len());
	let mut failures = Vec::new();
	for handle in handles {
		let (request, result) = handle.await?;
		match result {
			Ok(item) => resolutions.push((request, item)),
			Err(error) => failures.push((request, error)),
		}
	}
	Ok((resolutions, failures))
}

async fn resolve(http: Client, request: &ToolRequest) -> Result<ToolResolution, Error> {
	match request {
		ToolRequest::Github { full_name, lane, version } => {
			let backend = GithubBackend::new(http, GithubOptions::from_process());
			let release = backend.select_release(full_name, lane, version).await?;
			let (url, size, format) = backend.select_asset(full_name, &release)?;
			Ok(ToolResolution::Github {
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}

fn _update_lock_entries(lock: &mut Lock, installed: Vec<DownloadEntry>) -> usize {
	let installed_artifacts = installed.len();
	let groups: Vec<ToolResolutionGroup> = installed.into_iter().map(|(group, _)| group).collect();
	let row = |group: &ToolResolutionGroup, request: &ToolRequestGroup| match (&request.tool, &group.resolution) {
		(ToolRequest::Github { full_name, lane, version }, ToolResolution::Github { tag_name, .. }) => LockedTool::Github {
			full_name: full_name.clone(),
			lane: lane.clone(),
			requested: version.clone(),
			tag_name: tag_name.clone(),
			install_path: group.artifact_key.as_str().to_owned(),
		},
	};
	let mut rows = Vec::new();
	// duplicate rows are appended before representative rows
	for group in &groups {
		for request in group.requests.iter().skip(1) {
			rows.push(row(group, request));
		}
	}
	for group in &groups {
		if let Some(request) = group.requests.first() {
			rows.push(row(group, request));
		}
	}
	// identities of the tools being installed
	let incoming: HashSet<Identity> = rows.iter().map(LockedTool::identity).collect();
	// drop entries replaced by a new install before appending
	lock.tools.retain(|existing| !incoming.contains(&existing.identity()));
	lock.tools.extend(rows);
	installed_artifacts
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;
	use crate::backend::ArchiveFormat;
	use crate::tools::version::Version;

	#[test]
	fn locks_installed_tools() {
		let request = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.2.3".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionGroup::from_entries(entries).unwrap();
		let installed = resolutions
			.into_iter()
			.map(|group| {
				(group, ToolArchive::Github {
					path: PathBuf::from("tool.tar.gz"),
				})
			})
			.collect();
		let mut lock = Lock::default();
		let installed_artifacts = _update_lock_entries(&mut lock, installed);
		assert_eq!(installed_artifacts, 1);
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
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("latest"),
			tag_name: "v1.0.0".to_owned(),
			install_path: "github_owner_repo/1.0.0".to_owned(),
		});
		let request = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("latest"),
			},
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v2.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionGroup::from_entries(entries).unwrap();
		let installed = resolutions
			.into_iter()
			.map(|group| {
				(group, ToolArchive::Github {
					path: PathBuf::from("tool.tar.gz"),
				})
			})
			.collect();
		let installed_artifacts = _update_lock_entries(&mut lock, installed);
		assert_eq!(installed_artifacts, 1);
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
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			requested: Version::parse("1"),
			tag_name: "cli/v1.0.0".to_owned(),
			install_path: "github_owner_repo/cli_v1.0.0".to_owned(),
		});
		let request = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionGroup::from_entries(entries).unwrap();
		let installed = resolutions
			.into_iter()
			.map(|group| {
				(group, ToolArchive::Github {
					path: PathBuf::from("tool.tar.gz"),
				})
			})
			.collect();
		let installed_artifacts = _update_lock_entries(&mut lock, installed);
		assert_eq!(installed_artifacts, 1);
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
		let request = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionGroup::from_entries(entries).unwrap();
		let installed = resolutions
			.into_iter()
			.map(|group| {
				(group, ToolArchive::Github {
					path: PathBuf::from("tool.tar.gz"),
				})
			})
			.collect();
		let installed_artifacts = _update_lock_entries(&mut lock, installed);
		assert_eq!(installed_artifacts, 1);
		assert_eq!(lock.tools, vec![other, LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.0.0".to_owned(),
			install_path: "github_owner_repo/1.0.0".to_owned(),
		}]);
	}

	#[test]
	fn locks_duplicate_requests() {
		let latest = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("latest"),
			},
		};
		let exact = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let requests = vec![latest, exact];
		let entries = requests
			.into_iter()
			.map(|request| {
				(request, ToolResolution::Github {
					tag_name: "v1.0.0".to_owned(),
					url: String::new(),
					size: 0,
					format: ArchiveFormat::TarGz,
				})
			})
			.collect();
		let resolutions = ToolResolutionGroup::from_entries(entries).unwrap();
		let installed = resolutions
			.into_iter()
			.map(|group| {
				(group, ToolArchive::Github {
					path: PathBuf::from("tool.tar.gz"),
				})
			})
			.collect();
		let mut lock = Lock::default();
		let installed_artifacts = _update_lock_entries(&mut lock, installed);
		assert_eq!(installed_artifacts, 1);
		assert_eq!(lock.tools, vec![
			LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				install_path: "github_owner_repo/1.0.0".to_owned(),
			},
			LockedTool::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("latest"),
				tag_name: "v1.0.0".to_owned(),
				install_path: "github_owner_repo/1.0.0".to_owned(),
			}
		]);
	}
}
