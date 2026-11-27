use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::backend::node::{NodeBackend, NodeOptions};
use crate::config::{loader, locate, state};
use crate::tools::archive::{self, ToolResolution, ToolResolutionSet};
use crate::tools::catalog::Catalog;
use crate::tools::lock::{self, GithubLock, Identity, Lock, LockedTool, NodeLock};
use crate::tools::resolution_cache::{ResolutionCache, ResolutionCacheError};
use crate::tools::tool_request::{self, RequestedTool, ToolRequestSet};
use crate::tools::{shims, status};
use crate::ui::{progress, progress_list};

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
	let (list, sink) = _progress_list(&missing);
	let (resolutions, resolve_err, flush_err) = runtime.block_on(resolve_parallel(missing, http.clone(), data_dir, sink))?;
	drop(list);
	// print after the progress clears so a redraw cannot erase the warning
	if let Some(error) = flush_err {
		eprintln!("warning: {error}");
	}
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
			LockedTool::Github(lock) => println!("github:{} {} {}", lock.full_name, lock.tag_name, tools_dir.join(&lock.install_path).display()),
			LockedTool::Node(lock) => println!("core:node {} {}", lock.version, tools_dir.join(&lock.install_path).display()),
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
					eprintln!("github:{} {}{}: {error}", req.full_name, req.lane, req.version);
				}
				RequestedTool::Node(req) => {
					eprintln!("core:node {}: {error}", req.version);
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

async fn resolve_parallel(
	requests: Vec<ToolRequestSet>, http: Client, data_dir: &Path, progress: ResolveProgressFn,
) -> Result<(Vec<ResolutionEntry>, Vec<ResolutionFailure>, Option<ResolutionCacheError>), Error> {
	let mut resolutions = Vec::with_capacity(requests.len());
	let mut cache = ResolutionCache::load(data_dir, &requests);
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(requests.len());
	let github_options = Arc::new(GithubOptions::from_process());
	let node_options = Arc::new(NodeOptions::from_process());
	for (index, request) in requests.into_iter().enumerate() {
		if let Some(resolution) = cache.get(&request.tool) {
			// a cache hit starts and finishes at once
			progress(ResolveProgress {
				index,
				stage: ResolveStage::Resolve,
				done: false,
			});
			progress(ResolveProgress {
				index,
				stage: ResolveStage::Resolve,
				done: true,
			});
			resolutions.push((request, resolution));
			continue;
		}
		let semaphore = Arc::clone(&semaphore);
		let http = http.clone();
		let github_options = Arc::clone(&github_options);
		let node_options = Arc::clone(&node_options);
		let progress = Arc::clone(&progress);
		handles.push(tokio::spawn(async move {
			let permit = semaphore.acquire_owned().await;
			// start on either outcome so every finish has a start
			progress(ResolveProgress {
				index,
				stage: ResolveStage::Resolve,
				done: false,
			});
			let result = match permit {
				Ok(_permit) => match &request.tool {
					RequestedTool::Github(req) => {
						let backend = GithubBackend::new(http, github_options.as_ref().clone().with_api_url(&req.api_url));
						backend.resolve(req).await.map_err(Error::from)
					}
					RequestedTool::Node(req) => {
						let backend = NodeBackend::new(http, node_options.as_ref().clone());
						backend.resolve(req).await.map_err(Error::from)
					}
				},
				Err(error) => Err(error.into()),
			};
			progress(ResolveProgress {
				index,
				stage: ResolveStage::Resolve,
				done: true,
			});
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
	Ok((resolutions, failures, cache.flush().err()))
}

pub(crate) type ResolveProgressFn = Arc<dyn Fn(ResolveProgress) + Send + Sync>;

pub(crate) struct ResolveProgress {
	pub(crate) index: usize,
	pub(crate) stage: ResolveStage,
	pub(crate) done: bool,
}

pub(crate) enum ResolveStage {
	Resolve,
}

fn _progress_list(requests: &[ToolRequestSet]) -> (Arc<progress_list::ProgressList>, ResolveProgressFn) {
	let labels = requests
		.iter()
		.map(|request| match &request.tool {
			RequestedTool::Github(request) => format!("github:{} {}{}", request.full_name, request.lane, request.version),
			RequestedTool::Node(request) => format!("core:node {}", request.version),
		})
		.collect::<Vec<_>>();
	let list = Arc::new(progress_list::ProgressList::new(labels));
	let sink: ResolveProgressFn = {
		let list = Arc::clone(&list);
		Arc::new(move |event| {
			list.record(progress_list::ProgressEvent {
				index: event.index,
				stage: match event.stage {
					ResolveStage::Resolve => progress_list::Stage::Resolve,
				},
				done: event.done,
			});
		})
	};
	(list, sink)
}

fn _progress_bar(resolutions: &[ToolResolutionSet]) -> (Arc<progress::ProgressBar>, archive::ProgressFn) {
	let labels = resolutions
		.iter()
		.map(|group| match &group.requests[0].tool {
			RequestedTool::Github(request) => format!("github:{} {}", request.full_name, group.resolution.version()),
			RequestedTool::Node(_) => format!("core:node {}", group.resolution.version()),
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
	let row = |res: &ToolResolutionSet, req: &ToolRequestSet| match &req.tool {
		RequestedTool::Github(req) => LockedTool::Github(GithubLock {
			api_url: req.api_url.clone(),
			full_name: req.full_name.clone(),
			lane: req.lane.clone(),
			requested: req.version.clone(),
			tag_name: res.resolution.version().to_owned(),
			install_path: res.install_path.clone(),
		}),
		RequestedTool::Node(req) => LockedTool::Node(NodeLock {
			requested: req.version.clone(),
			version: res.resolution.version().to_owned(),
			install_path: res.install_path.clone(),
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
mod resolve_tests {
	use std::sync::Mutex;

	use serde_json::json;
	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, ResponseTemplate};

	use super::*;
	use crate::backend::_backend::ArchiveFormat;
	use crate::tools::archive::GithubResolution;
	use crate::tools::tool_request::GithubRequest;
	use crate::tools::version::Version;

	fn http() -> Client {
		Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build().unwrap()
	}

	fn _request(api_url: &str, full_name: &str) -> ToolRequestSet {
		ToolRequestSet {
			origins: Vec::new(),
			tool: RequestedTool::Github(GithubRequest {
				api_url: api_url.to_owned(),
				full_name: full_name.to_owned(),
				lane: String::new(),
				version: Version::parse("latest"),
				exposed: None,
				rename_shims: None,
			}),
		}
	}

	fn _resolution(checksum: &str) -> ToolResolution {
		ToolResolution::Github(GithubResolution {
			tag_name: "v1.0.0".to_owned(),
			url: String::new(),
			size: 0,
			format: ArchiveFormat::TarGz,
			sha256: checksum.to_owned(),
		})
	}

	fn _progress_events() -> (ResolveProgressFn, Arc<Mutex<Vec<ResolveProgress>>>) {
		let events = Arc::new(Mutex::new(Vec::new()));
		let progress: ResolveProgressFn = {
			let events = Arc::clone(&events);
			Arc::new(move |event| events.lock().unwrap().push(event))
		};
		(progress, events)
	}

	#[tokio::test]
	async fn reports_start_then_finish_for_every_request() {
		let server = MockServer::start().await;
		let api_url = server.uri();
		let libc = if cfg!(windows) { "msvc" } else { "gnu" };
		let asset = format!("tool-{}-{}-{libc}.tar.gz", std::env::consts::OS, std::env::consts::ARCH);
		let cached_checksum = "aa".repeat(32);
		let fresh_checksum = "bb".repeat(32);
		Mock::given(method("GET"))
			.and(path("/repos/owner/fresh/releases/latest"))
			.respond_with(ResponseTemplate::new(200).set_body_json(json!({
				"tag_name": "v1.0.0",
				"draft": false,
				"prerelease": false,
				"assets": [{
					"name": asset,
					"browser_download_url": format!("{api_url}/download/{asset}"),
					"size": 1,
					"digest": format!("sha256:{fresh_checksum}"),
				}],
			})))
			.mount(&server)
			.await;
		// owner/missing has no mock, so the server answers 404
		let requests = vec![
			_request(&api_url, "owner/cached"),
			_request(&api_url, "owner/fresh"),
			_request(&api_url, "owner/missing"),
		];
		let data_dir = tempfile::tempdir().unwrap();
		// seed the cache so owner/cached never reaches the server
		let mut cache = ResolutionCache::load(data_dir.path(), &requests[..1]);
		cache.put(&requests[0].tool, &_resolution(&cached_checksum));
		cache.flush().unwrap();
		let (progress, events) = _progress_events();
		let (resolutions, failures, flush_err) = resolve_parallel(requests, http(), data_dir.path(), progress).await.unwrap();
		assert_eq!(resolutions.len(), 2);
		assert_eq!(failures.len(), 1);
		assert!(flush_err.is_none());
		let checksum = |full_name: &str| {
			let entry = resolutions
				.iter()
				.find(|(request, _)| matches!(&request.tool, RequestedTool::Github(req) if req.full_name == full_name));
			entry.map(|(_, res)| res.checksum().to_owned())
		};
		// the cached checksum survives a flush and the fresh one drops the digest prefix
		assert_eq!(checksum("owner/cached"), Some(cached_checksum));
		assert_eq!(checksum("owner/fresh"), Some(fresh_checksum));
		let events = events.lock().unwrap().iter().map(|event| (event.index, event.done)).collect::<Vec<_>>();
		// the cache hit reports before any task is spawned
		assert_eq!(events[..2], [(0, false), (0, true)]);
		// every request, including the failed one, starts once and then finishes once
		for index in 0..3 {
			let own = events.iter().filter(|(event_index, _)| *event_index == index).copied().collect::<Vec<_>>();
			assert_eq!(own, [(index, false), (index, true)], "index {index}");
		}
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use super::*;
	use crate::backend::_backend::ArchiveFormat;
	use crate::tools::archive::GithubResolution;
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
		let entries = vec![(
			request,
			ToolResolution::Github(GithubResolution {
				tag_name: "v1.2.3".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
				sha256: String::new(),
			}),
		)];
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
		let entries = vec![(
			request,
			ToolResolution::Github(GithubResolution {
				tag_name: "v2.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
				sha256: String::new(),
			}),
		)];
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
		let entries = vec![(
			request,
			ToolResolution::Github(GithubResolution {
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
				sha256: String::new(),
			}),
		)];
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
		let entries = vec![(
			request,
			ToolResolution::Github(GithubResolution {
				tag_name: "v1.0.0".to_owned(),
				url: String::new(),
				size: 0,
				format: ArchiveFormat::TarGz,
				sha256: String::new(),
			}),
		)];
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
				(
					request,
					ToolResolution::Github(GithubResolution {
						tag_name: "v1.0.0".to_owned(),
						url: String::new(),
						size: 0,
						format: ArchiveFormat::TarGz,
						sha256: String::new(),
					}),
				)
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
