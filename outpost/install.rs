use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate, state};
use crate::tools::archive::{self, ToolResolution, ToolResolutionSet};
use crate::tools::catalog::Catalog;
use crate::tools::lock::{self, GithubLock, Identity, Lock, LockedTool};
use crate::tools::resolution_cache::ResolutionCache;
use crate::tools::tool_request::{self, RequestedTool, ToolRequestSet};
use crate::tools::{shims, status};
use crate::ui::progress;

type Error = Box<dyn std::error::Error + Send + Sync>;
type ResolutionEntry = (ToolRequestSet, ToolResolution);
type ResolutionFailure = (ToolRequestSet, Error);

pub(crate) fn install() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let data_dir = paths.data_dir.as_deref().ok_or("no data directory")?;
	let downloads_dir = paths.downloads_dir.as_deref().ok_or("no downloads directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	std::fs::create_dir_all(data_dir)?;
	std::fs::create_dir_all(downloads_dir)?;
	std::fs::create_dir_all(tools_dir)?;
	let _install_lock = lock::acquire(data_dir)?;
	let (mut state, text) = state::read(data_dir)?;
	let configs = loader::load(&paths, &state.registered_paths)?;
	let requests = tool_request::requests(&configs)?;
	// one set per identity so each tool resolves once
	let request_sets = ToolRequestSet::from_requests(&requests);
	// skip tools already recorded in the lock
	let mut lock = lock::read(data_dir)?;
	let missing = status::missing_from_lock(&request_sets, &lock, tools_dir);
	if missing.is_empty() {
		// persist the project registration even when nothing needs installing
		state::register_config(&mut state, &paths.system, &paths.project);
		state::write(data_dir, &state, text.as_deref())?;
		return Ok(());
	}
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (resolutions, resolve_err) = runtime.block_on(resolve_parallel(missing, http.clone(), data_dir))?;
	// group resolutions by artifact so each tool downloads once
	let resolutions = ToolResolutionSet::from_entries(resolutions, &lock)?;
	let (bar, sink) = _progress_bar(&resolutions);
	let (installed, install_err) = runtime.block_on(archive::install_parallel(resolutions, http, downloads_dir, tools_dir, sink))?;
	drop(bar);
	drop(runtime);
	_update_lock_entries(&mut lock, &installed);
	lock::write(data_dir, &lock)?;
	// refresh binaries and shims for the newly installed tools
	state::register_config(&mut state, &paths.system, &paths.project);
	let catalog = Catalog::new(&lock, &requests);
	let installations = catalog.installations_by_path();
	shims::index_installed_binaries(&mut state, tools_dir, installations, &installed)?;
	let index_err = shims::index_shims(&mut state, installations).err();
	state::write(data_dir, &state, text.as_deref())?;
	let generate_err: Option<Error> = paths
		.shims_dir
		.as_deref()
		.ok_or_else(|| Error::from("no shims directory"))
		.and_then(|dir| shims::generate(&state, dir).map_err(Error::from))
		.err();
	// representative rows are appended after duplicate rows
	for tool in &lock.tools[lock.tools.len() - installed.len()..] {
		match tool {
			LockedTool::Github(lock) => println!("{} {} {}", lock.full_name, lock.tag_name, tools_dir.join(&lock.install_path).display()),
		}
	}
	let failures = resolve_err
		.into_iter()
		.map(|(request, error)| (vec![request], error))
		.chain(install_err.into_iter().map(|(group, error)| (group.requests, error)))
		.collect::<Vec<_>>();
	for (requests, error) in &failures {
		for request in requests {
			match &request.tool {
				RequestedTool::Github(req) => {
					eprintln!("{} {}{}: {error}", req.full_name, req.lane, req.version);
				}
			}
			for origin in &request.origins {
				eprintln!("  from {}", origin.display());
			}
		}
	}
	let failed = failures.iter().map(|(requests, _)| requests.len()).sum::<usize>();
	let mut message = (failed > 0).then(|| format!("failed to install {failed} tool(s)"));
	// conflicts surface while indexing
	if let Some(error) = index_err.map(Error::from).or(generate_err) {
		let error = error.to_string();
		match &mut message {
			Some(message) => message.push_str(&format!("\n{error}")),
			None => message = Some(error),
		}
	}
	message.map_or(Ok(()), |message| Err(message.into()))
}

async fn resolve_parallel(requests: Vec<ToolRequestSet>, http: Client, data_dir: &Path) -> Result<(Vec<ResolutionEntry>, Vec<ResolutionFailure>), Error> {
	let mut resolutions = Vec::with_capacity(requests.len());
	let mut cache = ResolutionCache::load(data_dir, &requests);
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(requests.len());
	let github_options = Arc::new(GithubOptions::from_process());
	for request in requests {
		if let Some(resolution) = cache.get(&request.tool) {
			resolutions.push((request, resolution));
			continue;
		}
		let semaphore = Arc::clone(&semaphore);
		let http = http.clone();
		let github_options = Arc::clone(&github_options);
		handles.push(tokio::spawn(async move {
			let result = match semaphore.acquire_owned().await {
				Ok(_permit) => match &request.tool {
					RequestedTool::Github(req) => {
						let backend = GithubBackend::new(http, github_options.as_ref().clone().with_api_url(&req.api_url));
						backend.resolve(&request.tool).await.map_err(Error::from)
					}
				},
				Err(error) => Err(error.into()),
			};
			(request, result)
		}));
	}
	let mut failures = Vec::new();
	for handle in handles {
		let (request, result) = handle.await?;
		match result {
			Ok(item) => {
				cache.put(&request.tool, &item);
				resolutions.push((request, item));
			}
			Err(error) => failures.push((request, error)),
		}
	}
	if let Err(error) = cache.flush() {
		eprintln!("warning: {error}");
	}
	Ok((resolutions, failures))
}

fn _progress_bar(resolutions: &[ToolResolutionSet]) -> (Arc<progress::ProgressBar>, archive::ProgressFn) {
	let labels = resolutions
		.iter()
		.map(|group| match (&group.requests[0].tool, &group.resolution) {
			(RequestedTool::Github(request), ToolResolution::Github { tag_name, .. }) => format!("github:{} {tag_name}", request.full_name),
		})
		.collect::<Vec<_>>();
	let bar = Arc::new(progress::ProgressBar::new(labels));
	let sink: archive::ProgressFn = {
		let bar = Arc::clone(&bar);
		Arc::new(move |event| {
			bar.record(progress::ProgressEvent {
				index: event.index,
				stage: match event.stage {
					archive::Stage::Download => progress::Stage::Download,
					archive::Stage::Extract => progress::Stage::Extract,
				},
				done: event.done,
				total: event.total,
			});
		})
	};
	(bar, sink)
}

fn _update_lock_entries(lock: &mut Lock, installed: &[ToolResolutionSet]) {
	let row = |group: &ToolResolutionSet, request: &ToolRequestSet| match (&request.tool, &group.resolution) {
		(RequestedTool::Github(req), ToolResolution::Github { tag_name, .. }) => LockedTool::Github(GithubLock {
			api_url: req.api_url.clone(),
			full_name: req.full_name.clone(),
			lane: req.lane.clone(),
			requested: req.version.clone(),
			tag_name: tag_name.clone(),
			install_path: group.install_path.clone(),
		}),
	};
	let mut rows = Vec::new();
	// duplicate rows are appended before representative rows
	for group in installed {
		for request in group.requests.iter().skip(1) {
			rows.push(row(group, request));
		}
	}
	for group in installed {
		if let Some(request) = group.requests.first() {
			rows.push(row(group, request));
		}
	}
	// identities of the tools being installed
	let incoming: HashSet<Identity> = rows.iter().map(LockedTool::identity).collect();
	// drop entries replaced by a new install before appending
	lock.tools.retain(|existing| !incoming.contains(&existing.identity()));
	lock.tools.extend(rows);
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;
	use crate::backend::_backend::ArchiveFormat;
	use crate::tools::tool_request::GithubRequest;
	use crate::tools::version::Version;

	#[test]
	fn locks_installed_tools() {
		let request = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
				exposed: None,
				rename_shims: None,
			}),
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.2.3".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionSet::from_entries(entries, &Lock::default()).unwrap();
		let mut lock = Lock::default();
		_update_lock_entries(&mut lock, &resolutions);
		assert_eq!(resolutions.len(), 1);
		assert_eq!(lock.tools, vec![LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.2.3".to_owned(),
			install_path: PathBuf::from("github_owner_repo/1.2.3"),
		})]);
	}

	#[test]
	fn locks_newer_tag_replacing_older() {
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("latest"),
			tag_name: "v1.0.0".to_owned(),
			install_path: PathBuf::from("github_owner_repo/1.0.0"),
		}));
		let request = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("latest"),
				exposed: None,
				rename_shims: None,
			}),
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v2.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionSet::from_entries(entries, &Lock::default()).unwrap();
		_update_lock_entries(&mut lock, &resolutions);
		assert_eq!(resolutions.len(), 1);
		assert_eq!(lock.tools, vec![LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("latest"),
			tag_name: "v2.0.0".to_owned(),
			install_path: PathBuf::from("github_owner_repo/2.0.0"),
		})]);
	}

	#[test]
	fn locks_requests_separately() {
		let mut lock = Lock::default();
		lock.tools.push(LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: "cli/".to_owned(),
			requested: Version::parse("1"),
			tag_name: "cli/v1.0.0".to_owned(),
			install_path: PathBuf::from("github_owner_repo/cli_v1.0.0"),
		}));
		let request = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
				exposed: None,
				rename_shims: None,
			}),
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionSet::from_entries(entries, &Lock::default()).unwrap();
		_update_lock_entries(&mut lock, &resolutions);
		assert_eq!(resolutions.len(), 1);
		assert_eq!(lock.tools, vec![
			LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: "cli/".to_owned(),
				requested: Version::parse("1"),
				tag_name: "cli/v1.0.0".to_owned(),
				install_path: PathBuf::from("github_owner_repo/cli_v1.0.0"),
			}),
			LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.0.0"),
			})
		]);
	}

	#[test]
	fn locks_unrelated_tools() {
		let other = LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "other/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse("1.0.0"),
			tag_name: "v1.0.0".to_owned(),
			install_path: PathBuf::from("github_other_repo/1.0.0"),
		});
		let mut lock = Lock {
			version: 1,
			tools: vec![other.clone()],
		};
		let request = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
				exposed: None,
				rename_shims: None,
			}),
		};
		let entries = vec![(request, ToolResolution::Github {
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
		})];
		let resolutions = ToolResolutionSet::from_entries(entries, &Lock::default()).unwrap();
		_update_lock_entries(&mut lock, &resolutions);
		assert_eq!(resolutions.len(), 1);
		assert_eq!(lock.tools, vec![
			other,
			LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.0.0"),
			})
		]);
	}

	#[test]
	fn locks_duplicate_requests() {
		let latest = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("latest"),
				exposed: None,
				rename_shims: None,
			}),
		};
		let exact = ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse("1.0.0"),
				exposed: None,
				rename_shims: None,
			}),
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
		let resolutions = ToolResolutionSet::from_entries(entries, &Lock::default()).unwrap();
		let mut lock = Lock::default();
		_update_lock_entries(&mut lock, &resolutions);
		assert_eq!(resolutions.len(), 1);
		assert_eq!(lock.tools, vec![
			LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("1.0.0"),
				tag_name: "v1.0.0".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.0.0"),
			}),
			LockedTool::Github(GithubLock {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: Version::parse("latest"),
				tag_name: "v1.0.0".to_owned(),
				install_path: PathBuf::from("github_owner_repo/1.0.0"),
			})
		]);
	}
}
