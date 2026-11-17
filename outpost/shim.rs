use clap::Args;

use crate::config::{locate, state};
use crate::tools::{lock, shims};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim(args: ShimArgs) -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	let lock = lock::read(config_dir)?;
	let (mut state, text) = state::read(config_dir)?;
	shims::index_binaries(&mut state, tools_dir, &lock)?;
	let index_err = shims::index_shims(&mut state, &lock).err();
	state::write(config_dir, &state, text.as_deref())?;
	let shims_dir = paths.shims_dir.as_deref().ok_or("no shims directory")?;
	if args.force {
		shims::purge(shims_dir)?;
	}
	let generate_err = shims::generate(&state, shims_dir, &lock).err();
	// conflicts surface while indexing on windows and while generating on unix
	match index_err.or(generate_err) {
		Some(error) => Err(error.into()),
		None => Ok(()),
	}
}

#[derive(Args)]
pub(crate) struct ShimArgs {
	#[arg(long)]
	pub(crate) force: bool,
}
