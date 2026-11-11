use std::path::PathBuf;
use std::sync::Arc;

use reqwest::Client;
use tokio::sync::Semaphore;

use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
use crate::tools::archive::{self, Downloaded, Resolved};
use crate::tools::tool_request::{self, ToolRequest};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn install() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config = loader::load(&paths)?;
	let requests = tool_request::requests(&config)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let downloads_dir = config_dir.join("downloads");
	let tools_dir = config_dir.join("tools");
	std::fs::create_dir_all(&downloads_dir)?;
	std::fs::create_dir_all(&tools_dir)?;
	let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
	let http = Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build()?;
	let (downloaded, failed) = runtime.block_on(resolve_and_download(http, requests, downloads_dir))?;
	drop(runtime);
	let failed = failed + archive::extract_all(downloaded, tools_dir)?;
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
}

async fn resolve_and_download(http: Client, requests: Vec<ToolRequest>, downloads_dir: PathBuf) -> Result<(Vec<Downloaded>, usize), Error> {
	let (resolved, resolve_failed) = resolve_parallel(http.clone(), requests).await?;
	let (downloaded, download_failed) = archive::download_parallel(http, resolved, downloads_dir).await?;
	Ok((downloaded, resolve_failed + download_failed))
}

async fn resolve_parallel(http: Client, requests: Vec<ToolRequest>) -> Result<(Vec<Resolved>, usize), Error> {
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

async fn resolve(http: Client, request: &ToolRequest) -> Result<Resolved, Error> {
	match request {
		ToolRequest::Github { full_name, lane, version } => {
			let backend = GithubBackend::new(http, GithubOptions::from_process());
			let release = backend.select_release(full_name, lane, version).await?;
			let (url, size, format) = backend.select_asset(full_name, &release)?;
			Ok(Resolved {
				backend: "github".to_owned(),
				full_name: full_name.clone(),
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}
