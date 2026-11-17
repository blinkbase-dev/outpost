use crate::config::{locate, state};
use crate::tools::{lock, shims};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	let lock = lock::read(config_dir)?;
	let (mut state, text) = state::read(config_dir)?;
	shims::index_binaries(&mut state, tools_dir, &lock)?;
	shims::index_shims(&mut state, &lock);
	state::write(config_dir, &state, text.as_deref())?;
	let shims_dir = paths.shims_dir.as_deref().ok_or("no shims directory")?;
	shims::generate(&state, shims_dir, &lock)?;
	Ok(())
}
