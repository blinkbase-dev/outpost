use std::path::Path;

use crate::config::locate;
use crate::config::state::{self, State};
use crate::tools::candidates;
use crate::tools::lock::{self, Lock, LockedTool};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	let lock = lock::read(config_dir)?;
	let (mut state, text) = state::read(config_dir)?;
	_index_binaries(&mut state, &lock, tools_dir)?;
	_index_shims(&mut state, &lock);
	state::write(config_dir, &state, text.as_deref())?;
	Ok(())
}

fn _index_binaries(state: &mut State, lock: &Lock, tools_dir: &Path) -> Result<(), Error> {
	let install_paths: Vec<&Path> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_path(),
		})
		.collect();
	state.binaries = candidates::find_binaries_by_tool(tools_dir, install_paths)?;
	Ok(())
}

#[cfg(windows)]
fn _index_shims(state: &mut State, lock: &Lock) {
	use std::collections::BTreeMap;
	use std::ffi::OsStr;
	use std::path::PathBuf;

	use crate::config::state::ShimEntry;

	let mut shims: BTreeMap<String, Vec<ShimEntry>> = BTreeMap::new();
	for tool in &lock.tools {
		match tool {
			LockedTool::Github {
				full_name,
				lane,
				requested,
				install_path,
				..
			} => {
				let Some(binaries) = state.binaries.get(install_path) else {
					continue;
				};
				let mut names: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
				for binary in binaries {
					let Some(name) = binary.file_stem().and_then(OsStr::to_str) else {
						continue;
					};
					names.entry(name.to_ascii_lowercase()).or_default().push(binary.clone());
				}
				for (name, binaries) in names {
					shims.entry(name).or_default().push(ShimEntry::Github {
						full_name: full_name.clone(),
						lane: lane.clone(),
						requested: requested.clone(),
						install_path: install_path.clone(),
						binaries,
					});
				}
			}
		}
	}
	state.shims = shims;
}

#[cfg(not(windows))]
fn _index_shims(_state: &mut State, _lock: &Lock) {}
