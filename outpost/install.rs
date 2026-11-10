use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use regex::regex;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::backend::ArchiveFormat;
use crate::backend::github::{GithubBackend, GithubOptions};
use crate::config::{loader, locate};
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
	let failed = failed + extract_all(downloaded, tools_dir)?;
	if failed > 0 {
		return Err(format!("failed to install {failed} tool(s)").into());
	}
	Ok(())
}

async fn resolve_and_download(http: Client, requests: Vec<ToolRequest>, downloads_dir: PathBuf) -> Result<(Vec<Downloaded>, usize), Error> {
	let (resolved, resolve_failed) = resolve_parallel(http.clone(), requests).await?;
	let (downloaded, download_failed) = download_parallel(http, resolved, downloads_dir).await?;
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
				full_name: full_name.clone(),
				tag_name: release.tag_name,
				url,
				size,
				format,
			})
		}
	}
}

async fn download_parallel(http: Client, resolved: Vec<Resolved>, downloads_dir: PathBuf) -> Result<(Vec<Downloaded>, usize), Error> {
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(resolved.len());
	for resolved in resolved {
		let semaphore = Arc::clone(&semaphore);
		let downloads_dir = downloads_dir.clone();
		let http = http.clone();
		handles.push(tokio::spawn(async move {
			let tag = resolved
				.tag_name
				.strip_prefix('v')
				.filter(|rest| !rest.is_empty())
				.unwrap_or(&resolved.tag_name);
			let name = format!(
				"{}_{}",
				resolved.full_name.replace(['.', '/', '-'], "_"),
				regex!(r"[^a-zA-Z0-9_.]").replace_all(tag, "_")
			);
			let path = downloads_dir.join(&name);
			let part = downloads_dir.join(format!("{name}.part"));
			let result = match semaphore.acquire_owned().await {
				Ok(_permit) => download(&http, &resolved.url, &path, &part, resolved.size).await.map(|()| path),
				Err(error) => Err(error.into()),
			};
			(resolved, result)
		}));
	}
	let mut downloaded = Vec::with_capacity(handles.len());
	let mut failed = 0;
	for handle in handles {
		let (resolved, result) = handle.await?;
		match result {
			Ok(path) => downloaded.push(Downloaded {
				full_name: resolved.full_name,
				tag_name: resolved.tag_name,
				path,
				format: resolved.format,
			}),
			Err(error) => {
				eprintln!("{}: {error}", resolved.full_name);
				failed += 1;
			}
		}
	}
	Ok((downloaded, failed))
}

async fn download(http: &Client, url: &str, path: &Path, part: &Path, size: u64) -> Result<(), Error> {
	let mut response = http.get(url).send().await?.error_for_status()?;
	let mut file = tokio::fs::File::create(part).await?;
	let mut written = 0u64;
	while let Some(chunk) = response.chunk().await? {
		written += chunk.len() as u64;
		file.write_all(&chunk).await?;
	}
	file.flush().await?;
	drop(file);
	if written != size {
		return Err(format!("size mismatch for {}: expected {size}, got {written}", path.display()).into());
	}
	tokio::fs::rename(part, path).await?;
	Ok(())
}

fn extract_all(downloaded: Vec<Downloaded>, tools_dir: PathBuf) -> Result<usize, Error> {
	let pool = rayon::ThreadPoolBuilder::new().build()?;
	let results: Vec<(Downloaded, Result<PathBuf, Error>)> = pool.install(|| {
		downloaded
			.into_par_iter()
			.map(|item| {
				let tag = item.tag_name.strip_prefix('v').filter(|rest| !rest.is_empty()).unwrap_or(&item.tag_name);
				let dir = tools_dir
					.join(item.full_name.replace(['.', '/', '-'], "_"))
					.join(regex!(r"[^a-zA-Z0-9_.]").replace_all(tag, "_").into_owned());
				let result = extract(&item.path, &dir, &item.format).map(|()| dir);
				(item, result)
			})
			.collect()
	});
	let mut failed = 0;
	for (item, result) in results {
		match result {
			Ok(dir) => println!("{} {} {}", item.full_name, item.tag_name, dir.display()),
			Err(error) => {
				eprintln!("{}: {error}", item.full_name);
				failed += 1;
			}
		}
	}
	Ok(failed)
}

fn extract(archive: &Path, dest: &Path, format: &ArchiveFormat) -> Result<(), Error> {
	let name = dest.file_name().and_then(|name| name.to_str()).ok_or("invalid install path")?;
	let parent = dest.parent().ok_or("invalid install path")?;
	std::fs::create_dir_all(parent)?;
	let temp = parent.join(format!("{name}.tmp"));
	let _ = std::fs::remove_dir_all(&temp);
	std::fs::create_dir_all(&temp)?;
	let file = std::fs::File::open(archive)?;
	match format {
		ArchiveFormat::TarGz => {
			let decoder = flate2::read::GzDecoder::new(file);
			let mut archive = tar::Archive::new(decoder);
			for entry in archive.entries()? {
				entry?.unpack_in(&temp)?;
			}
		}
		ArchiveFormat::TarXz => {
			let decoder = liblzma::read::XzDecoder::new(file);
			let mut archive = tar::Archive::new(decoder);
			for entry in archive.entries()? {
				entry?.unpack_in(&temp)?;
			}
		}
		ArchiveFormat::Zip => {
			let mut archive = zip::ZipArchive::new(file)?;
			archive.extract(&temp)?;
		}
	}
	if dest.exists() {
		std::fs::remove_dir_all(dest)?;
	}
	std::fs::rename(&temp, dest)?;
	Ok(())
}

struct Resolved {
	full_name: String,
	tag_name: String,
	url: String,
	size: u64,
	format: ArchiveFormat,
}

struct Downloaded {
	full_name: String,
	tag_name: String,
	path: PathBuf,
	format: ArchiveFormat,
}
