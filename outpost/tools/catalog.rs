use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use super::lock::{Identity, Lock, LockedTool};
use super::tool_request::ToolRequestGroup;

// builds identity indexes for config requests and lock rows on demand
pub(crate) struct Catalog<'a> {
	lock: &'a Lock,
	requests: &'a [ToolRequestGroup],
	requests_by_identity: OnceCell<HashMap<Identity, &'a ToolRequestGroup>>,
	locked_by_identity: OnceCell<HashMap<Identity, &'a LockedTool>>,
	installations: OnceCell<Installations<'a>>,
}

impl<'a> Catalog<'a> {
	pub(crate) fn new(lock: &'a Lock, requests: &'a [ToolRequestGroup]) -> Self {
		Self {
			lock,
			requests,
			requests_by_identity: OnceCell::new(),
			locked_by_identity: OnceCell::new(),
			installations: OnceCell::new(),
		}
	}

	pub(crate) fn request(&self, identity: &Identity) -> Option<&'a ToolRequestGroup> {
		self
			.requests_by_identity
			.get_or_init(|| self.requests.iter().map(|group| (group.tool.identity(), group)).collect())
			.get(identity)
			.copied()
	}

	pub(crate) fn locked(&self, identity: &Identity) -> Option<&'a LockedTool> {
		self
			.locked_by_identity
			.get_or_init(|| self.lock.tools.iter().map(|tool| (tool.identity(), tool)).collect())
			.get(identity)
			.copied()
	}

	pub(crate) fn installations(&self) -> &Installations<'a> {
		self.installations.get_or_init(|| {
			let mut items_by_path: BTreeMap<&'a Path, Vec<InstallationItem<'a>>> = BTreeMap::new();
			for tool in &self.lock.tools {
				let install_path = match tool {
					LockedTool::Github { install_path, .. } => install_path.as_path(),
				};
				items_by_path.entry(install_path).or_default().push(InstallationItem {
					locked: tool,
					requests: self.request(&tool.identity()),
				});
			}
			items_by_path
		})
	}
}

pub(crate) type Installations<'a> = BTreeMap<&'a Path, Vec<InstallationItem<'a>>>;

pub(crate) struct InstallationItem<'a> {
	pub(crate) locked: &'a LockedTool,
	pub(crate) requests: Option<&'a ToolRequestGroup>,
}

#[cfg(test)]
mod tests {
	use std::path::{Path, PathBuf};

	use super::*;
	use crate::tools::tool_request::ToolRequest;
	use crate::tools::version::Version;

	fn _request_group(version: &str) -> ToolRequestGroup {
		ToolRequestGroup {
			origins: Vec::new(),
			tool: ToolRequest::Github {
				full_name: "owner/repo".to_owned(),
				lane: String::new(),
				version: Version::parse(version),
				api_url: None,
				exposed: None,
				rename_shims: None,
			},
		}
	}

	fn _lock(tools: Vec<LockedTool>) -> Lock {
		Lock { version: 1, tools }
	}

	fn _locked(requested: &str, installed_version: &str) -> LockedTool {
		let version = installed_version.strip_prefix('v').unwrap_or(installed_version);
		LockedTool::Github {
			full_name: "owner/repo".to_owned(),
			lane: String::new(),
			requested: Version::parse(requested),
			tag_name: installed_version.to_owned(),
			install_path: PathBuf::from(format!("github_owner_repo/{version}")),
		}
	}

	#[test]
	fn looks_up_requests_and_lock_rows_by_identity() {
		let group = _request_group("1.0.0");
		let lock = _lock(vec![_locked("1.0.0", "v1.0.0")]);
		let catalog = Catalog::new(&lock, std::slice::from_ref(&group));
		let identity = group.tool.identity();
		assert!(catalog.request(&identity).is_some());
		assert!(catalog.locked(&identity).is_some());
		let missing = _request_group("2.0.0").tool.identity();
		assert!(catalog.request(&missing).is_none());
		assert!(catalog.locked(&missing).is_none());
	}

	#[test]
	fn groups_installation_items_by_install_path() {
		let lock = _lock(vec![_locked("latest", "v1.0.0"), _locked("1.0.0", "v1.0.0"), _locked("2.0.0", "v2.0.0")]);
		let requests = [_request_group("latest"), _request_group("1.0.0")];
		let catalog = Catalog::new(&lock, &requests);
		let installations = catalog.installations();
		assert_eq!(installations.len(), 2);
		let shared = installations.get(Path::new("github_owner_repo/1.0.0")).unwrap();
		assert_eq!(shared.len(), 2);
		assert!(shared.iter().all(|item| item.requests.is_some()));
		let separate = installations.get(Path::new("github_owner_repo/2.0.0")).unwrap();
		assert!(separate[0].requests.is_none());
	}
}
