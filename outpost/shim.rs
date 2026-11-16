use std::path::Path;

use crate::config::{locate, state};
use crate::tools::candidates;
use crate::tools::lock::{self, LockedTool};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	_index_binaries(config_dir, tools_dir)?;
	Ok(())
}

fn _index_binaries(config_dir: &Path, tools_dir: &Path) -> Result<(), Error> {
	let lock = lock::read(config_dir)?;
	let install_paths: Vec<&str> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_str(),
		})
		.collect();
	let (mut state, text) = state::read(config_dir)?;
	state.binaries = candidates::find_binaries_by_tool(tools_dir, install_paths)?;
	state::write(config_dir, &state, text.as_deref())?;
	Ok(())
}
