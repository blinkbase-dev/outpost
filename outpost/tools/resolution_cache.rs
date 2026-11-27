use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backend::_asset_selection::Target;
use crate::backend::_backend::ArchiveFormat;
use crate::backend::github;
use crate::tools::archive::{GithubResolution, ToolResolution};
use crate::tools::tool_request::{RequestedTool, ToolRequestSet};
use crate::tools::version::Version;

const CACHE_VERSION: u32 = 1;
const RESOLUTION_TTL: u64 = 24 * 60 * 60;

pub(crate) struct ResolutionCache {
	data_dir: PathBuf,
	github: HashMap<PathBuf, GithubCacheFile>,
	github_dirty: HashSet<PathBuf>,
}

impl ResolutionCache {
	pub(crate) fn load(data_dir: &Path, requests: &[ToolRequestSet]) -> Self {
		let mut cache = Self {
			data_dir: data_dir.to_path_buf(),
			github: HashMap::new(),
			github_dirty: HashSet::new(),
		};
		let mut paths = HashSet::new();
		for request in requests {
			match &request.tool {
				RequestedTool::Github(req) => {
					let path = github::resolution_cache_path(data_dir, &req.api_url, &req.full_name);
					if !paths.insert(path.clone()) {
						continue;
					}
					let Ok(text) = std::fs::read_to_string(&path) else {
						continue;
					};
					let Ok(file) = toml::from_str::<GithubCacheFile>(&text) else {
						continue;
					};
					if !_valid_github_file(&file, &req.api_url, &req.full_name) {
						continue;
					}
					cache.github.insert(path, file);
				}
				// node resolutions skip the cache
				RequestedTool::Node(_) => {}
			}
		}
		cache
	}

	pub(crate) fn get(&self, tool: &RequestedTool) -> Option<ToolResolution> {
		match tool {
			RequestedTool::Github(req) => {
				let path = github::resolution_cache_path(&self.data_dir, &req.api_url, &req.full_name);
				let file = self.github.get(&path).filter(|file| _valid_github_file(file, &req.api_url, &req.full_name))?;
				let target = Target::current();
				let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
				file
					.resolution
					.iter()
					.find(|entry| {
						entry.lane == req.lane
							&& entry.requested == req.version
							&& entry.os == target.os
							&& entry.arch == target.arch
							&& entry.libc == target.libc
							&& now.saturating_sub(entry.resolved_at) < RESOLUTION_TTL
					})
					.map(|entry| {
						ToolResolution::Github(GithubResolution {
							tag_name: entry.tag_name.clone(),
							url: entry.url.clone(),
							size: entry.size,
							format: entry.format.clone(),
							sha256: entry.sha256.clone(),
						})
					})
			}
			RequestedTool::Node(_) => None,
		}
	}

	#[expect(clippy::single_match, reason = "more backends will add arms")]
	pub(crate) fn put(&mut self, tool: &RequestedTool, resolution: &ToolResolution) {
		match (tool, resolution) {
			(RequestedTool::Github(req), ToolResolution::Github(res)) => {
				let path = github::resolution_cache_path(&self.data_dir, &req.api_url, &req.full_name);
				let invalid = self
					.github
					.get(&path)
					.is_some_and(|file| !_valid_github_file(file, &req.api_url, &req.full_name));
				if invalid {
					self.github.remove(&path);
				}
				let file = self.github.entry(path.clone()).or_insert_with(|| GithubCacheFile {
					version: CACHE_VERSION,
					backend: "github".to_owned(),
					api_url: req.api_url.clone(),
					full_name: req.full_name.clone(),
					resolution: Vec::new(),
				});
				let target = Target::current();
				let incoming = GithubResolutionRecord {
					lane: req.lane.clone(),
					requested: req.version.clone(),
					os: target.os.to_owned(),
					arch: target.arch.to_owned(),
					libc: target.libc.to_owned(),
					resolved_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
					tag_name: res.tag_name.clone(),
					url: res.url.clone(),
					size: res.size,
					format: res.format.clone(),
					sha256: res.sha256.clone(),
				};
				let existing = file.resolution.iter_mut().find(|existing| {
					existing.lane == incoming.lane
						&& existing.requested == incoming.requested
						&& existing.os == incoming.os
						&& existing.arch == incoming.arch
						&& existing.libc == incoming.libc
				});
				if let Some(existing) = existing {
					*existing = incoming;
				} else {
					file.resolution.push(incoming);
				}
				self.github_dirty.insert(path);
			}
			_ => {}
		}
	}

	pub(crate) fn flush(&mut self) -> Result<(), ResolutionCacheError> {
		let mut paths: Vec<PathBuf> = self.github_dirty.iter().cloned().collect();
		paths.sort();
		for path in paths {
			let Some(file) = self.github.get(&path) else {
				return Err(ResolutionCacheError::MissingGithubFile(path.clone()));
			};
			if file.resolution.is_empty() {
				match std::fs::remove_file(&path) {
					Ok(()) => {}
					Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
					Err(error) => return Err(error.into()),
				}
			} else {
				let text = toml::to_string_pretty(file)?;
				std::fs::create_dir_all(path.parent().expect("resolution cache path has a parent"))?;
				let temp = path.with_extension("toml.tmp");
				let mut output = std::fs::File::create(&temp)?;
				output.write_all(text.as_bytes())?;
				output.sync_all()?;
				drop(output);
				std::fs::rename(&temp, &path)?;
			}
			self.github_dirty.remove(&path);
		}
		Ok(())
	}
}

fn _valid_github_file(file: &GithubCacheFile, api_url: &str, full_name: &str) -> bool {
	file.version == CACHE_VERSION && file.backend == "github" && file.api_url == api_url && file.full_name == full_name
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GithubCacheFile {
	version: u32,
	backend: String,
	api_url: String,
	full_name: String,
	resolution: Vec<GithubResolutionRecord>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GithubResolutionRecord {
	lane: String,
	requested: Version,
	os: String,
	arch: String,
	libc: String,
	resolved_at: u64,
	tag_name: String,
	url: String,
	size: u64,
	format: ArchiveFormat,
	sha256: String, // empty when the server published no digest
}

#[derive(Debug)]
pub(crate) enum ResolutionCacheError {
	Io(std::io::Error),
	Serialize(toml::ser::Error),
	MissingGithubFile(PathBuf),
}

impl From<std::io::Error> for ResolutionCacheError {
	fn from(error: std::io::Error) -> Self {
		Self::Io(error)
	}
}

impl From<toml::ser::Error> for ResolutionCacheError {
	fn from(error: toml::ser::Error) -> Self {
		Self::Serialize(error)
	}
}

impl fmt::Display for ResolutionCacheError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(f, "resolution cache io failed: {error}"),
			Self::Serialize(error) => write!(f, "resolution cache serialize failed: {error}"),
			Self::MissingGithubFile(path) => write!(f, "github cache file '{}' is dirty but missing from the cache", path.display()),
		}
	}
}

impl std::error::Error for ResolutionCacheError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::Serialize(error) => Some(error),
			Self::MissingGithubFile(_) => None,
		}
	}
}
