use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::ToolRequest;

// builds identity indexes for config requests and lock rows on demand
pub(crate) struct Catalog<'a> {
	lock: &'a Lock,
	requests: &'a [ToolRequest],
	requests_by_identity: OnceCell<HashMap<Identity, Vec<&'a ToolRequest>>>,
	locked_by_identity: OnceCell<HashMap<Identity, &'a LockedTool>>,
	installations: OnceCell<Installations<'a>>,
}

impl<'a> Catalog<'a> {
	pub(crate) fn new(lock: &'a Lock, requests: &'a [ToolRequest]) -> Self {
		Self {
			lock,
			requests,
			locked_by_identity: OnceCell::new(),
			requests_by_identity: OnceCell::new(),
			installations: OnceCell::new(),
		}
	}

	pub(crate) fn lock(&self) -> &'a Lock {
		self.lock
	}

	pub(crate) fn requests(&self) -> &'a [ToolRequest] {
		self.requests
	}

	pub(crate) fn request_for(&self, identity: &Identity) -> &[&'a ToolRequest] {
		// every entry for an identity is kept so consumers own the precedence rule
		let index = self.requests_by_identity.get_or_init(|| {
			let mut index: HashMap<Identity, Vec<&'a ToolRequest>> = HashMap::with_capacity(self.requests.len());
			for request in self.requests {
				index.entry(request.tool.identity()).or_default().push(request);
			}
			index
		});
		index.get(identity).map_or(&[], Vec::as_slice)
	}

	pub(crate) fn lock_for(&self, identity: &Identity) -> Option<&'a LockedTool> {
		self
			.locked_by_identity
			.get_or_init(|| self.lock.tools.iter().map(|tool| (tool.identity(), tool)).collect())
			.get(identity)
			.copied()
	}

	pub(crate) fn installations_by_path(&self) -> &Installations<'a> {
		self.installations.get_or_init(|| {
			let mut items_by_path: BTreeMap<&'a Path, Vec<InstallationItem<'a>>> = BTreeMap::new();
			for tool in &self.lock.tools {
				let requests = self.request_for(&tool.identity()).to_vec();
				items_by_path
					.entry(tool.install_path())
					.or_default()
					.push(InstallationItem { locked: tool, requests });
			}
			items_by_path
		})
	}
}

pub(crate) type Installations<'a> = BTreeMap<&'a Path, Vec<InstallationItem<'a>>>;

pub(crate) struct InstallationItem<'a> {
	pub(crate) locked: &'a LockedTool,
	pub(crate) requests: Vec<&'a ToolRequest>,
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::*;
	use crate::tools::lock::GithubLock;
	use crate::tools::tool_request::{GithubRequest, RequestedTool};
	use crate::tools::version::Version;

	fn _request(version: &str) -> ToolRequest {
		ToolRequest {
			origin: PathBuf::from("outpost.toml"),
			tool: RequestedTool::Github(GithubRequest {
				api_url: String::new(),
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse(version),
				exposed: None,
				rename_shims: None,
			}),
		}
	}

	fn _lock(tools: Vec<LockedTool>) -> Lock {
		Lock { version: 1, tools }
	}

	fn _locked(requested: &str, installed_version: &str) -> LockedTool {
		let version = installed_version.strip_prefix('v').unwrap_or(installed_version);
		LockedTool::Github(GithubLock {
			api_url: String::new(),
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse(requested),
			tag_name: installed_version.to_owned(),
			install_path: PathBuf::from(format!("github_owner_repo/{version}")),
		})
	}

	#[test]
	fn looks_up_requests_and_lock_rows_by_identity() {
		let request = _request("1.0.0");
		let lock = _lock(vec![_locked("1.0.0", "v1.0.0")]);
		let catalog = Catalog::new(&lock, std::slice::from_ref(&request));
		let identity = request.tool.identity();
		assert_eq!(catalog.request_for(&identity).len(), 1);
		assert!(catalog.lock_for(&identity).is_some());
		let missing = _request("2.0.0").tool.identity();
		assert!(catalog.request_for(&missing).is_empty());
		assert!(catalog.lock_for(&missing).is_none());
	}

	#[test]
	fn groups_installation_items_by_install_path() {
		let lock = _lock(vec![_locked("latest", "v1.0.0"), _locked("1.0.0", "v1.0.0"), _locked("2.0.0", "v2.0.0")]);
		let requests = [_request("latest"), _request("1.0.0")];
		let catalog = Catalog::new(&lock, &requests);
		let installations = catalog.installations_by_path();
		assert_eq!(installations.len(), 2);
		let shared = installations.get(Path::new("github_owner_repo/1.0.0")).unwrap();
		assert_eq!(shared.len(), 2);
		let separate = installations.get(Path::new("github_owner_repo/2.0.0")).unwrap();
		assert_eq!(separate.len(), 1);
	}
}
