use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
use crate::tools::archive::{self, ArtifactKey, ToolDownloaded, ToolResolved};
use crate::tools::lock::{self, Identity, Lock, LockedTool};
use crate::tools::status;
use crate::tools::tool_request::{self, GithubRequest, ToolRequest, ToolRequestGroup};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn install() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let configs = loader::load(&paths)?;
	let requests = tool_request::requests(&configs)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let downloads_dir = config_dir.join("downloads");
	let tools_dir = config_dir.join("tools");
	std::fs::create_dir_all(&downloads_dir)?;
	std::fs::create_dir_all(&tools_dir)?;
	let _install_lock = lock::acquire(config_dir)?;
	// skip tools already recorded in the lock
	let groups = ToolRequestGroup::from_entries(requests);
	let mut lock = lock::read(config_dir)?;
	let groups = status::missing_from_lock(groups, &lock, &tools_dir);
	if groups.is_empty() {
		return Ok(());
	}
	let requests = groups.into_iter().map(|group| group.tool).collect();
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (resolved, resolve_failed) = runtime.block_on(resolve_parallel(http.clone(), requests))?;
	// download each artifact once, keeping duplicate requests for the lock
	let (artifacts, duplicates) = _partition_by_artifact_key(resolved)?;
	let (downloaded, download_failed) = runtime.block_on(archive::download_parallel(http, artifacts, downloads_dir))?;
	drop(runtime);
	let (installed, extract_failed) = archive::extract_parallel(downloaded, &tools_dir)?;
	let installed_count = _update_lock_entries(&mut lock, installed, duplicates);
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
	let failed = resolve_failed + download_failed + extract_failed;
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
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
					ToolRequest::Github(GithubRequest { full_name, .. }) => eprintln!("{full_name}: {error}"),
				}
				failed += 1;
			}
		}
	}
	Ok((resolved, failed))
}

async fn resolve(http: Client, request: &ToolRequest) -> Result<ToolResolved, Error> {
	match request {
		ToolRequest::Github(GithubRequest { full_name, lane, version }) => {
			let backend = GithubBackend::new(http, GithubOptions::from_process());
			let release = backend.select_release(full_name, lane, version).await?;
			let (url, size, format) = backend.select_asset(full_name, &release)?;
			Ok(ToolResolved::Github {
				request: GithubRequest {
					full_name: full_name.clone(),
					lane: lane.clone(),
					version: version.clone(),
				},
				artifact_key: ArtifactKey::new(request, &release.tag_name),
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}

fn _partition_by_artifact_key(resolved: Vec<ToolResolved>) -> Result<(Vec<ToolResolved>, Vec<ToolResolved>), Error> {
	let mut artifacts = Vec::new();
	let mut duplicates = Vec::new();
	let mut index_by_key = HashMap::new();
	for resolved in resolved {
		match &resolved {
			ToolResolved::Github { artifact_key, tag_name, .. } => {
				if let Some(&index) = index_by_key.get(artifact_key) {
					match &artifacts[index] {
						ToolResolved::Github { tag_name: installed_tag, .. } => {
							if installed_tag != tag_name {
								return Err(format!("artifact collision: `{tag_name}` and `{installed_tag}` share `{}`", artifact_key.as_str()).into());
							}
						}
					}
					duplicates.push(resolved);
				} else {
					// point the key at the representative's index
					index_by_key.insert(artifact_key.clone(), artifacts.len());
					artifacts.push(resolved);
				}
			}
		}
	}
	Ok((artifacts, duplicates))
}

fn _update_lock_entries(lock: &mut Lock, installed: Vec<ToolDownloaded>, duplicates: Vec<ToolResolved>) -> usize {
	let installed_count = installed.len();
	let mut rows = Vec::with_capacity(installed_count + duplicates.len());
	let installed_by_key: HashMap<_, _> = installed
		.iter()
		.map(|tool| match tool {
			ToolDownloaded::Github { artifact_key, tag_name, .. } => (artifact_key, tag_name.as_str()),
		})
		.collect();
	for duplicate in duplicates {
		match duplicate {
			ToolResolved::Github { request, artifact_key, .. } => {
				let Some(&installed_tag) = installed_by_key.get(&artifact_key) else {
					continue;
				};
				rows.push(LockedTool::Github {
					full_name: request.full_name,
					lane: request.lane,
					requested: request.version,
					tag_name: installed_tag.to_owned(),
					install_path: artifact_key.as_str().to_owned(),
				});
			}
		}
	}
	for tool in installed {
		match tool {
			ToolDownloaded::Github {
				request,
				artifact_key,
				tag_name,
				..
			} => {
				rows.push(LockedTool::Github {
					full_name: request.full_name,
					lane: request.lane,
					requested: request.version,
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
		let latest = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("latest"),
		};
		let exact = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(latest.clone()), "v1.0.0");
		let resolved = vec![
			ToolResolved::Github {
				request: latest,
				artifact_key: artifact_key.clone(),
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			},
			ToolResolved::Github {
				request: exact,
				artifact_key,
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			},
		];
		let (artifacts, duplicates) = _partition_by_artifact_key(resolved).unwrap();
		assert_eq!(artifacts.len(), 1);
		assert_eq!(duplicates.len(), 1);
	}

	#[test]
	fn rejects_artifact_collisions() {
		let request = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let tagged = ArtifactKey::new(&ToolRequest::Github(request.clone()), "v1.0.0");
		let plain = ArtifactKey::new(&ToolRequest::Github(request.clone()), "1.0.0");
		let resolved = vec![
			ToolResolved::Github {
				request: request.clone(),
				artifact_key: tagged,
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			},
			ToolResolved::Github {
				request,
				artifact_key: plain,
				tag_name: "1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
			},
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
		let request = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(request.clone()), "v1.2.3");
		let installed = vec![ToolDownloaded::Github {
			request,
			artifact_key,
			tag_name: "v1.2.3".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		}];
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
		let request = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("latest"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(request.clone()), "v2.0.0");
		let installed = vec![ToolDownloaded::Github {
			request,
			artifact_key,
			tag_name: "v2.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		}];
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
		let request = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(request.clone()), "v1.0.0");
		let installed = vec![ToolDownloaded::Github {
			request,
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		}];
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
		let request = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(request.clone()), "v1.0.0");
		let installed = vec![ToolDownloaded::Github {
			request,
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		}];
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
		let latest = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("latest"),
		};
		let exact = GithubRequest {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			version: Version::parse("1.0.0"),
		};
		let artifact_key = ArtifactKey::new(&ToolRequest::Github(latest.clone()), "v1.0.0");
		let installed = vec![ToolDownloaded::Github {
			request: latest,
			artifact_key: artifact_key.clone(),
			tag_name: "v1.0.0".to_owned(),
			path: PathBuf::from("tool.tar.gz"),
			format: ArchiveFormat::TarGz,
		}];
		let duplicates = vec![ToolResolved::Github {
			request: exact,
			artifact_key,
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		}];
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
