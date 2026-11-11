use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;
use regex::regex;
use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::backend::ArchiveFormat;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub async fn download_parallel(http: Client, resolved: Vec<ToolResolved>, downloads_dir: PathBuf) -> Result<(Vec<ToolDownloaded>, usize), Error> {
	let semaphore = Arc::new(Semaphore::new(8));
	let mut handles = Vec::with_capacity(resolved.len());
	for resolved in resolved {
		let semaphore = Arc::clone(&semaphore);
		let downloads_dir = downloads_dir.clone();
		let http = http.clone();
		handles.push(tokio::spawn(async move {
			match resolved {
				ToolResolved::Github {
					full_name,
					lane,
					requested,
					tag_name,
					url,
					size,
					format,
				} => {
					let tag = tag_name.strip_prefix('v').filter(|rest| !rest.is_empty()).unwrap_or(&tag_name);
					let name = format!(
						"{}_{}",
						full_name.replace(['.', '/', '-'], "_"),
						regex!(r"[^a-zA-Z0-9_.]").replace_all(tag, "_")
					);
					match semaphore.acquire_owned().await {
						Ok(_permit) => match download(&http, &url, &downloads_dir, &name, size).await {
							Ok(path) => Ok(ToolDownloaded::Github {
								full_name,
								lane,
								requested,
								tag_name,
								path,
								format,
							}),
							Err(error) => Err((full_name, error)),
						},
						Err(error) => Err((full_name, error.into())),
					}
				}
			}
		}));
	}
	let mut downloaded = Vec::with_capacity(handles.len());
	let mut failed = 0;
	for handle in handles {
		match handle.await? {
			Ok(tool) => downloaded.push(tool),
			Err((full_name, error)) => {
				eprintln!("{full_name}: {error}");
				failed += 1;
			}
		}
	}
	Ok((downloaded, failed))
}

async fn download(http: &Client, url: &str, dir: &Path, name: &str, size: u64) -> Result<PathBuf, Error> {
	let path = dir.join(name);
	let part = dir.join(format!("{name}.part"));
	let mut response = http.get(url).send().await?.error_for_status()?;
	let mut file = tokio::fs::File::create(&part).await?;
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
	tokio::fs::rename(&part, &path).await?;
	Ok(path)
}

pub fn extract_all(downloaded: Vec<ToolDownloaded>, tools_dir: PathBuf) -> Result<(Vec<(ToolDownloaded, PathBuf)>, usize), Error> {
	let pool = rayon::ThreadPoolBuilder::new().build()?;
	let results: Vec<Result<(ToolDownloaded, PathBuf), (String, Error)>> = pool.install(|| {
		downloaded
			.into_par_iter()
			.map(|item| match item {
				ToolDownloaded::Github {
					full_name,
					lane,
					requested,
					tag_name,
					path,
					format,
				} => {
					let tag = tag_name.strip_prefix('v').filter(|rest| !rest.is_empty()).unwrap_or(&tag_name);
					let dir = tools_dir
						.join(format!("github_{}", full_name.to_lowercase().replace(['.', '/', '-'], "_")))
						.join(regex!(r"[^a-zA-Z0-9_.]").replace_all(tag, "_").into_owned());
					match extract(&path, &dir, &format) {
						Ok(()) => Ok((
							ToolDownloaded::Github {
								full_name,
								lane,
								requested,
								tag_name,
								path,
								format,
							},
							dir,
						)),
						Err(error) => Err((full_name, error)),
					}
				}
			})
			.collect()
	});
	let mut installed = Vec::with_capacity(results.len());
	let mut failed = 0;
	for result in results {
		match result {
			Ok(item) => installed.push(item),
			Err((full_name, error)) => {
				eprintln!("{full_name}: {error}");
				failed += 1;
			}
		}
	}
	Ok((installed, failed))
}

fn extract(archive: &Path, dest: &Path, format: &ArchiveFormat) -> Result<(), Error> {
	std::fs::create_dir_all(dest.parent().ok_or("invalid install path")?)?;
	let name = archive.file_name().and_then(|name| name.to_str()).ok_or("invalid archive path")?;
	let temp = archive.with_file_name(format!("{name}.tmp"));
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
	// descends through single-child directories to strip wrapper directories
	let mut source = temp.clone();
	loop {
		let mut entries = std::fs::read_dir(&source)?;
		let Some(entry) = entries.next() else {
			break;
		};
		let entry = entry?;
		if !entry.file_type()?.is_dir() || entries.next().is_some() {
			break;
		}
		source = entry.path();
	}
	if dest.exists() {
		std::fs::remove_dir_all(dest)?;
	}
	std::fs::rename(&source, dest)?;
	// removes the temporary tree when the source was a nested directory
	if source != temp {
		let _ = std::fs::remove_dir_all(&temp);
	}
	Ok(())
}

pub enum ToolResolved {
	Github {
		full_name: String,
		lane: String,
		requested: String,
		tag_name: String,
		url: String,
		size: u64,
		format: ArchiveFormat,
	},
}

pub enum ToolDownloaded {
	Github {
		full_name: String,
		lane: String,
		requested: String,
		tag_name: String,
		path: PathBuf,
		format: ArchiveFormat,
	},
}

#[cfg(test)]
mod download_tests {
	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, ResponseTemplate};

	use super::*;

	fn http() -> Client {
		Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build().unwrap()
	}

	async fn serve(body: Vec<u8>, status: u16) -> MockServer {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path("/asset.tar.gz"))
			.respond_with(ResponseTemplate::new(status).set_body_bytes(body))
			.mount(&server)
			.await;
		server
	}

	#[tokio::test]
	async fn downloads_and_renames_part_file() {
		let server = serve(b"payload".to_vec(), 200).await;
		let dir = tempfile::tempdir().unwrap();
		let path = download(&http(), &format!("{}/asset.tar.gz", server.uri()), dir.path(), "asset.tar.gz", 7)
			.await
			.unwrap();
		assert_eq!(std::fs::read(&path).unwrap(), b"payload");
		assert!(!dir.path().join("asset.tar.gz.part").exists());
	}

	#[tokio::test]
	async fn streams_large_body() {
		let body = vec![b'x'; 1024 * 1024];
		let size = body.len() as u64;
		let server = serve(body, 200).await;
		let dir = tempfile::tempdir().unwrap();
		let path = download(&http(), &format!("{}/asset.tar.gz", server.uri()), dir.path(), "asset.tar.gz", size)
			.await
			.unwrap();
		assert_eq!(std::fs::read(&path).unwrap().len() as u64, size);
	}

	#[tokio::test]
	async fn rejects_size_mismatch() {
		let server = serve(b"payload".to_vec(), 200).await;
		let dir = tempfile::tempdir().unwrap();
		let error = download(&http(), &format!("{}/asset.tar.gz", server.uri()), dir.path(), "asset.tar.gz", 1)
			.await
			.unwrap_err();
		assert!(error.to_string().contains("size mismatch"), "{error}");
		assert!(!dir.path().join("asset.tar.gz").exists());
		assert!(dir.path().join("asset.tar.gz.part").exists());
	}

	#[tokio::test]
	async fn propagates_http_status() {
		let server = serve(Vec::new(), 404).await;
		let dir = tempfile::tempdir().unwrap();
		let error = download(&http(), &format!("{}/asset.tar.gz", server.uri()), dir.path(), "asset.tar.gz", 7)
			.await
			.is_err();
		assert!(error);
		assert!(!dir.path().join("asset.tar.gz").exists());
		assert!(!dir.path().join("asset.tar.gz.part").exists());
	}
}

#[cfg(test)]
mod download_parallel_tests {
	use std::sync::Mutex;
	use std::time::{Duration, Instant};

	use wiremock::matchers::{method, path};
	use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

	use super::*;

	fn http() -> Client {
		Client::builder().user_agent(concat!("outpost/", env!("CARGO_PKG_VERSION"))).build().unwrap()
	}

	struct Recorder {
		started: Arc<Mutex<Vec<Instant>>>,
		size: u64,
	}

	impl Respond for Recorder {
		fn respond(&self, _request: &Request) -> ResponseTemplate {
			self.started.lock().unwrap().push(Instant::now());
			ResponseTemplate::new(200)
				.set_body_bytes(vec![0u8; self.size as usize])
				.set_delay(Duration::from_millis(200))
		}
	}

	#[tokio::test]
	async fn limits_downloads_to_eight_in_flight() {
		let started = Arc::new(Mutex::new(Vec::new()));
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.respond_with(Recorder {
				started: Arc::clone(&started),
				size: 4,
			})
			.mount(&server)
			.await;
		let dir = tempfile::tempdir().unwrap();
		let resolved = (0..16)
			.map(|index| ToolResolved::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: format!("1.0.{index}"),
				tag_name: format!("v1.0.{index}"),
				url: format!("{}/asset{index}", server.uri()),
				size: 4,
				format: ArchiveFormat::TarGz,
			})
			.collect();
		let (downloaded, failed) = download_parallel(http(), resolved, dir.path().to_owned()).await.unwrap();
		assert_eq!(downloaded.len(), 16);
		assert_eq!(failed, 0);
		let started = started.lock().unwrap();
		assert_eq!(started.len(), 16);
		assert!(started[7] - started[0] < Duration::from_millis(100));
		assert!(started[8] - started[0] >= Duration::from_millis(150));
	}

	#[tokio::test]
	async fn downloads_all_items() {
		let server = MockServer::start().await;
		for index in 0..10 {
			Mock::given(method("GET"))
				.and(path(format!("/asset{index}")))
				.respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4]))
				.mount(&server)
				.await;
		}
		let dir = tempfile::tempdir().unwrap();
		let resolved = (0..10)
			.map(|index| ToolResolved::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: format!("1.0.{index}"),
				tag_name: format!("1.0.{index}"),
				url: format!("{}/asset{index}", server.uri()),
				size: 4,
				format: ArchiveFormat::TarGz,
			})
			.collect();
		let (downloaded, failed) = download_parallel(http(), resolved, dir.path().to_owned()).await.unwrap();
		assert_eq!(downloaded.len(), 10);
		assert_eq!(failed, 0);
		for item in &downloaded {
			let ToolDownloaded::Github { path, .. } = item;
			assert!(path.exists(), "{}", path.display());
		}
	}

	#[tokio::test]
	async fn counts_download_failures_and_keeps_going() {
		let server = MockServer::start().await;
		Mock::given(method("GET"))
			.and(path("/good"))
			.respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 4]))
			.mount(&server)
			.await;
		Mock::given(method("GET"))
			.and(path("/missing"))
			.respond_with(ResponseTemplate::new(404))
			.mount(&server)
			.await;
		let dir = tempfile::tempdir().unwrap();
		let resolved = vec![
			ToolResolved::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				url: format!("{}/good", server.uri()),
				size: 4,
				format: ArchiveFormat::TarGz,
			},
			ToolResolved::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				requested: "1.1.0".to_owned(),
				tag_name: "v1.1.0".to_owned(),
				url: format!("{}/missing", server.uri()),
				size: 4,
				format: ArchiveFormat::TarGz,
			},
		];
		let (downloaded, failed) = download_parallel(http(), resolved, dir.path().to_owned()).await.unwrap();
		assert_eq!(failed, 1);
		assert_eq!(downloaded.len(), 1);
		let ToolDownloaded::Github { path, .. } = &downloaded[0];
		assert!(path.exists());
		assert!(!dir.path().join("owner_repo_1.1.0").exists());
		assert!(!dir.path().join("owner_repo_1.1.0.part").exists());
	}
}

#[cfg(test)]
mod extract_tests {
	use std::io::{Cursor, Write};

	use flate2::Compression;
	use flate2::write::GzEncoder;
	use liblzma::write::XzEncoder;
	use tar::{Builder, Header};
	use zip::ZipWriter;
	use zip::write::SimpleFileOptions;

	use super::*;

	fn tar<W: Write>(writer: W, files: &[(&str, &[u8])]) -> W {
		let mut builder = Builder::new(writer);
		for (path, data) in files {
			let mut header = Header::new_gnu();
			header.set_size(data.len() as u64);
			header.set_mode(0o644);
			builder.append_data(&mut header, path, *data).unwrap();
		}
		builder.into_inner().unwrap()
	}

	pub(super) fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
		tar(GzEncoder::new(Vec::new(), Compression::default()), files).finish().unwrap()
	}

	pub(super) fn tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
		tar(XzEncoder::new(Vec::new(), 6), files).finish().unwrap()
	}

	pub(super) fn zip_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
		let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
		for (path, data) in files {
			writer.start_file(*path, SimpleFileOptions::default()).unwrap();
			writer.write_all(data).unwrap();
		}
		writer.finish().unwrap().into_inner()
	}

	#[test]
	fn extracts_tar_gz() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("tool-1.0.0/example-tool", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert_eq!(std::fs::read(dest.join("example-tool")).unwrap(), b"binary");
		assert!(!dir.path().join("tool.tar.gz.tmp").exists());
	}

	#[test]
	fn extracts_tar_xz() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.xz");
		std::fs::write(&archive, tar_xz(&[("tool-1.0.0/example-tool", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarXz).unwrap();
		assert_eq!(std::fs::read(dest.join("example-tool")).unwrap(), b"binary");
		assert!(!dir.path().join("tool.tar.xz.tmp").exists());
	}

	#[test]
	fn extracts_zip() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.zip");
		std::fs::write(&archive, zip_bytes(&[("tool-1.0.0/tool.exe", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::Zip).unwrap();
		assert_eq!(std::fs::read(dest.join("tool.exe")).unwrap(), b"binary");
		assert!(!dir.path().join("tool.zip.tmp").exists());
	}

	#[test]
	fn keeps_single_top_level_file() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("tool", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert_eq!(std::fs::read(dest.join("tool")).unwrap(), b"binary");
		assert!(!dir.path().join("tool.tar.gz.tmp").exists());
	}

	#[test]
	fn keeps_multiple_top_level_entries() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("bin/tool", b"binary"), ("LICENSE", b"license")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert_eq!(std::fs::read(dest.join("bin/tool")).unwrap(), b"binary");
		assert_eq!(std::fs::read(dest.join("LICENSE")).unwrap(), b"license");
	}

	#[test]
	fn strips_nested_wrapper_directories() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("path/to/tool", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert_eq!(std::fs::read(dest.join("tool")).unwrap(), b"binary");
		assert!(!dir.path().join("tool.tar.gz.tmp").exists());
	}

	#[test]
	fn strips_wrapper_with_same_name_as_child() {
		let dir = tempfile::tempdir().unwrap();
		let archive = dir.path().join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("tool/tool", b"binary")])).unwrap();
		let dest = dir.path().join("tools/owner_repo/1.0.0");
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert_eq!(std::fs::read(dest.join("tool")).unwrap(), b"binary");
	}

	#[test]
	fn replaces_existing_destination() {
		let dir = tempfile::tempdir().unwrap();
		let dest = dir.path().join("tools/tool");
		std::fs::create_dir_all(&dest).unwrap();
		std::fs::write(dest.join("stale"), b"old").unwrap();
		let archive = dir.path().join("downloads/tool.tar.gz");
		std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
		std::fs::write(&archive, tar_gz(&[("tool-1.0.0/fresh", b"new")])).unwrap();
		extract(&archive, &dest, &ArchiveFormat::TarGz).unwrap();
		assert!(!dest.join("stale").exists());
		assert_eq!(std::fs::read(dest.join("fresh")).unwrap(), b"new");
		assert!(!dir.path().join("downloads/tool.tar.gz.tmp").exists());
	}

	#[test]
	fn keeps_destination_on_corrupt_archive() {
		let dir = tempfile::tempdir().unwrap();
		let dest = dir.path().join("tools/tool");
		std::fs::create_dir_all(&dest).unwrap();
		std::fs::write(dest.join("keep"), b"old").unwrap();
		let archive = dir.path().join("downloads/corrupt.tar.gz");
		std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
		std::fs::write(&archive, b"not an archive").unwrap();
		assert!(extract(&archive, &dest, &ArchiveFormat::TarGz).is_err());
		assert_eq!(std::fs::read(dest.join("keep")).unwrap(), b"old");
	}
}

#[cfg(test)]
mod extract_all_tests {
	use super::extract_tests::{tar_gz, tar_xz, zip_bytes};
	use super::*;

	#[test]
	fn extracts_into_repo_and_tag_directory() {
		let dir = tempfile::tempdir().unwrap();
		let downloads = dir.path().join("downloads");
		std::fs::create_dir_all(&downloads).unwrap();
		let tools = dir.path().join("tools");
		let archive = downloads.join("tool.tar.gz");
		std::fs::write(&archive, tar_gz(&[("tool-1.0.0/example-tool", b"binary")])).unwrap();
		let downloaded = vec![ToolDownloaded::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: "1.0.0".to_owned(),
			tag_name: "v1.2.3".to_owned(),
			path: archive,
			format: ArchiveFormat::TarGz,
		}];
		let (installed, failed) = extract_all(downloaded, tools.clone()).unwrap();
		let dest = tools.join("github_owner_repo/1.2.3");
		assert_eq!(failed, 0);
		assert_eq!(installed.len(), 1);
		assert_eq!(std::fs::read(dest.join("example-tool")).unwrap(), b"binary");

		let archive = downloads.join("magika.tar.gz");
		std::fs::write(&archive, tar_gz(&[("tool-1.0.0/example-tool", b"binary")])).unwrap();
		let downloaded = vec![ToolDownloaded::Github {
			full_name: "google/magika".to_owned(),
			lane: String::new(),
			requested: "1.0.0".to_owned(),
			tag_name: "cli/v1.0.0".to_owned(),
			path: archive,
			format: ArchiveFormat::TarGz,
		}];
		let (installed, failed) = extract_all(downloaded, tools.clone()).unwrap();
		let dest = tools.join("github_google_magika/cli_v1.0.0");
		assert_eq!(failed, 0);
		assert_eq!(installed.len(), 1);
		assert_eq!(std::fs::read(dest.join("example-tool")).unwrap(), b"binary");
	}

	#[test]
	fn extracts_every_format() {
		let dir = tempfile::tempdir().unwrap();
		let downloads = dir.path().join("downloads");
		std::fs::create_dir_all(&downloads).unwrap();
		let gz_archive = downloads.join("tool.tar.gz");
		std::fs::write(&gz_archive, tar_gz(&[("tool-1.0.0/example-tool", b"one")])).unwrap();
		let xz_archive = downloads.join("tool.tar.xz");
		std::fs::write(&xz_archive, tar_xz(&[("tool-1.0.0/example-tool", b"two")])).unwrap();
		let zip_archive = downloads.join("tool.zip");
		std::fs::write(&zip_archive, zip_bytes(&[("tool-1.0.0/example-tool", b"three")])).unwrap();
		let tools = dir.path().join("tools");
		let downloaded = vec![
			ToolDownloaded::Github {
				full_name: "owner/tar-gz".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				path: gz_archive,
				format: ArchiveFormat::TarGz,
			},
			ToolDownloaded::Github {
				full_name: "owner/tar-xz".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				path: xz_archive,
				format: ArchiveFormat::TarXz,
			},
			ToolDownloaded::Github {
				full_name: "owner/zip".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				path: zip_archive,
				format: ArchiveFormat::Zip,
			},
		];
		let (installed, failed) = extract_all(downloaded, tools.clone()).unwrap();
		assert_eq!(failed, 0);
		assert_eq!(installed.len(), 3);
		assert_eq!(std::fs::read(tools.join("github_owner_tar_gz/1.0.0/example-tool")).unwrap(), b"one");
		assert_eq!(std::fs::read(tools.join("github_owner_tar_xz/1.0.0/example-tool")).unwrap(), b"two");
		assert_eq!(std::fs::read(tools.join("github_owner_zip/1.0.0/example-tool")).unwrap(), b"three");
	}

	#[test]
	fn counts_extraction_failures_and_keeps_going() {
		let dir = tempfile::tempdir().unwrap();
		let downloads = dir.path().join("downloads");
		std::fs::create_dir_all(&downloads).unwrap();
		let good = downloads.join("good.tar.gz");
		std::fs::write(&good, tar_gz(&[("tool-1.0.0/example-tool", b"binary")])).unwrap();
		let bad = downloads.join("bad.tar.gz");
		std::fs::write(&bad, b"not an archive").unwrap();
		let tools = dir.path().join("tools");
		let downloaded = vec![
			ToolDownloaded::Github {
				full_name: "owner/good".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				path: good,
				format: ArchiveFormat::TarGz,
			},
			ToolDownloaded::Github {
				full_name: "owner/bad".to_owned(),
				lane: String::new(),
				requested: "1.0.0".to_owned(),
				tag_name: "v1.0.0".to_owned(),
				path: bad,
				format: ArchiveFormat::TarGz,
			},
		];
		let (installed, failed) = extract_all(downloaded, tools.clone()).unwrap();
		assert_eq!(failed, 1);
		assert_eq!(installed.len(), 1);
		assert_eq!(std::fs::read(tools.join("github_owner_good/1.0.0/example-tool")).unwrap(), b"binary");
		assert!(!tools.join("github_owner_bad/1.0.0").exists());
	}
}
