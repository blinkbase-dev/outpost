use std::path::Path;

use crate::config::locate;
use crate::config::state::{self, State};
use crate::tools::candidates;
use crate::tools::lock::{self, LockedTool};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	let (mut state, text) = state::read(config_dir)?;
	_index_binaries(&mut state, config_dir, tools_dir)?;
	_index_shims(&mut state);
	state::write(config_dir, &state, text.as_deref())?;
	Ok(())
}

fn _index_binaries(state: &mut State, config_dir: &Path, tools_dir: &Path) -> Result<(), Error> {
	let lock = lock::read(config_dir)?;
	let install_paths: Vec<&str> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_str(),
		})
		.collect();
	state.binaries = candidates::find_binaries_by_tool(tools_dir, install_paths)?;
	Ok(())
}

#[cfg(windows)]
fn _index_shims(state: &mut State) {
	use std::collections::BTreeMap;
	use std::ffi::OsStr;

	let mut shims: BTreeMap<String, Vec<String>> = BTreeMap::new();
	for (install_path, binaries) in &state.binaries {
		for binary in binaries {
			let Some(name) = binary.file_stem().and_then(OsStr::to_str) else {
				continue;
			};
			let providers = shims.entry(name.to_ascii_lowercase()).or_default();
			if !providers.contains(install_path) {
				providers.push(install_path.clone());
			}
		}
	}
	state.shims = shims;
}

#[cfg(not(windows))]
fn _index_shims(_state: &mut State) {}
