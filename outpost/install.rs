use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
use crate::tools::archive::{self, ArtifactKey, ToolDownloaded, ToolResolved};
use crate::tools::lock::{self, Identity, Lock, LockedTool};
use crate::tools::status;
use crate::tools::tool_request::{self, ToolRequest, ToolRequestGroup};

type Error = Box<dyn std::error::Error + Send + Sync>;
type ResolvedRequest = (ToolRequestGroup, ToolResolved);
type RejectedRequest = (ToolRequestGroup, Error);

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
	let groups = ToolRequestGroup::from_entries(requests);
	let mut lock = lock::read(config_dir)?;
	// skip tools already recorded in the lock
	let groups = status::missing_from_lock(groups, &lock, &tools_dir);
	if groups.is_empty() {
		return Ok(());
	}
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (resolved, mut rejected) = runtime.block_on(resolve_parallel(groups, http.clone()))?;
	// download each artifact once but keep duplicate requests for the lock
	let (artifacts, duplicates) = _partition_by_artifact_key(resolved)?;
	let (downloaded, download_rejected) = runtime.block_on(archive::download_parallel(artifacts, http, downloads_dir))?;
	rejected.extend(download_rejected);
	drop(runtime);
	let (extracted, extract_rejected) = archive::extract_parallel(downloaded, &tools_dir)?;
	rejected.extend(extract_rejected);
	let installed_count = _update_lock_entries(&mut lock, extracted, duplicates);
	lock::write(config_dir, &lock)?;
	// representative rows are appended after duplicate rows
	for tool in &lock.tools[lock.tools.len() - installed_count..] {
		match tool {
			LockedTool::Github {
				full_name,
				tag_name,
				install_path,
				..
			} => println!("{full_name} {tag_name} {}", tools_dir.join(install_path).display()),
		}
	}
	let failed = rejected.len();
	for (request, error) in &rejected {
		match &request.tool {
			ToolRequest::Github { full_name, lane, version } => {
				eprintln!("{full_name} {lane}{version}: {error}");
			}
		}
		for origin in &request.origins {
			eprintln!("  from {}", origin.display());
		}
	}
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
}

async fn resolve_parallel(requests: Vec<ToolRequestGroup>, http: Client) -> Result<(Vec<ResolvedRequest>, Vec<RejectedRequest>), Error> {
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
	let mut resolved = Vec::with_capacity(handles.len());
	let mut rejected = Vec::new();
	for handle in handles {
		let (request, result) = handle.await?;
		match result {
			Ok(item) => resolved.push((request, item)),
			Err(error) => rejected.push((request, error)),
		}
	}
	Ok((resolved, rejected))
}

async fn resolve(http: Client, request: &ToolRequest) -> Result<ToolResolved, Error> {
	match request {
		ToolRequest::Github { full_name, lane, version } => {
			let backend = GithubBackend::new(http, GithubOptions::from_process());
			let release = backend.select_release(full_name, lane, version).await?;
			let (url, size, format) = backend.select_asset(full_name, &release)?;
			Ok(ToolResolved::Github {
				artifact_key: ArtifactKey::new(request, &release.tag_name),
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}

fn _partition_by_artifact_key(resolved: Vec<ResolvedRequest>) -> Result<(Vec<ResolvedRequest>, Vec<ResolvedRequest>), Error> {
	let mut artifacts: Vec<ResolvedRequest> = Vec::new();
	let mut duplicates: Vec<ResolvedRequest> = Vec::new();
	let mut index_by_key: HashMap<ArtifactKey, usize> = HashMap::new();
	for (request, resolved) in resolved {
		match &resolved {
			ToolResolved::Github { artifact_key, tag_name, .. } => {
				if let Some(&index) = index_by_key.get(artifact_key) {
					match &artifacts[index].1 {
						ToolResolved::Github { tag_name: installed_tag, .. } => {
							if installed_tag.as_str() != tag_name.as_str() {
								return Err(format!("artifact collision: `{tag_name}` and `{installed_tag}` share `{}`", artifact_key.as_str()).into());
							}
						}
					}
					duplicates.push((request, resolved));
				} else {
					// point the key at the representative's index
					index_by_key.insert(artifact_key.clone(), artifacts.len());
					artifacts.push((request, resolved));
				}
			}
		}
	}
	Ok((artifacts, duplicates))
}

fn _update_lock_entries(lock: &mut Lock, installed: Vec<(ToolRequestGroup, ToolDownloaded)>, duplicates: Vec<(ToolRequestGroup, ToolResolved)>) -> usize {
	let installed_count = installed.len();
	let mut rows = Vec::with_capacity(installed_count + duplicates.len());
	let installed_by_key: HashMap<_, _> = installed
		.iter()
		.map(|(_, tool)| match tool {
			ToolDownloaded::Github { artifact_key, tag_name, .. } => (artifact_key, tag_name.as_str()),
		})
		.collect();
	for (request, duplicate) in duplicates {
		match (request.tool, duplicate) {
			(ToolRequest::Github { full_name, lane, version }, ToolResolved::Github { artifact_key, .. }) => {
				let Some(&installed_tag) = installed_by_key.get(&artifact_key) else {
					continue;
				};
				rows.push(LockedTool::Github {
					full_name,
					lane,
					requested: version,
					tag_name: installed_tag.to_owned(),
					install_path: artifact_key.as_str().to_owned(),
				});
			}
		}
	}
	for (request, downloaded) in installed {
		match (request.tool, downloaded) {
			(ToolRequest::Github { full_name, lane, version }, ToolDownloaded::Github { artifact_key, tag_name, .. }) => {
				rows.push(LockedTool::Github {
					full_name,
					lane,
					requested: version,
					tag_name,
					install_path: artifact_key.as_str().to_owned(),
				});
			}
		}
	}
	// identities of the tools being installed
	let incoming: HashSet<Identity> = rows.iter().map(LockedTool::identity).collect();
	// drop entries replaced by a new install before appending
	lock.tools.retain(|existing| !incoming.contains(&existing.identity()));
	lock.tools.extend(rows);
	installed_count
}

#[cfg(test)]
mod partition_tests {
	use super::*;
	use crate::backend::ArchiveFormat;
	use crate::tools::version::Version;

	#[test]
	fn partitions_duplicate_requests() {
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
		let artifact_key = ArtifactKey::new(&latest.tool, "v1.0.0");
		let resolved = vec![
			(latest, ToolResolved::Github {
				artifact_key: artifact_key.clone(),
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			}),
			(exact, ToolResolved::Github {
				artifact_key,
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			}),
		];
		let (artifacts, duplicates) = _partition_by_artifact_key(resolved).unwrap();
		assert_eq!(artifacts.len(), 1);
		assert_eq!(duplicates.len(), 1);
	}

	#[test]
	fn rejects_artifact_collisions() {
		let first = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let second = ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
			},
		};
		let tagged = ArtifactKey::new(&first.tool, "v1.0.0");
		let plain = ArtifactKey::new(&first.tool, "1.0.0");
		let resolved = vec![
			(first, ToolResolved::Github {
				artifact_key: tagged,
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			}),
			(second, ToolResolved::Github {
				artifact_key: plain,
				tag_name: "1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			}),
		];
		assert!(_partition_by_artifact_key(resolved).is_err());
	}
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
		let artifact_key = ArtifactKey::new(&request.tool, "v1.2.3");
		let installed = vec![(request, ToolDownloaded::Github {
			artifact_key,
			tag_name: "v1.2.3".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		})];
		let mut lock = Lock::default();
		let installed_count = _update_lock_entries(&mut lock, installed, vec![]);
		assert_eq!(installed_count, 1);
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
		let artifact_key = ArtifactKey::new(&request.tool, "v2.0.0");
		let installed = vec![(request, ToolDownloaded::Github {
			artifact_key,
			tag_name: "v2.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		})];
		let installed_count = _update_lock_entries(&mut lock, installed, vec![]);
		assert_eq!(installed_count, 1);
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
		let artifact_key = ArtifactKey::new(&request.tool, "v1.0.0");
		let installed = vec![(request, ToolDownloaded::Github {
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		})];
		let installed_count = _update_lock_entries(&mut lock, installed, vec![]);
		assert_eq!(installed_count, 1);
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
		let artifact_key = ArtifactKey::new(&request.tool, "v1.0.0");
		let installed = vec![(request, ToolDownloaded::Github {
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		})];
		let installed_count = _update_lock_entries(&mut lock, installed, vec![]);
		assert_eq!(installed_count, 1);
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
		let artifact_key = ArtifactKey::new(&latest.tool, "v1.0.0");
		let installed = vec![(latest, ToolDownloaded::Github {
			artifact_key: artifact_key.clone(),
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		})];
		let duplicates = vec![(exact, ToolResolved::Github {
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let mut lock = Lock::default();
		let installed_count = _update_lock_entries(&mut lock, installed, duplicates);
		assert_eq!(installed_count, 1);
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
