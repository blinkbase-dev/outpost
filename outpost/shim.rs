use clap::Args;

use crate::config::{loader, locate, state};
use crate::tools::catalog::Catalog;
use crate::tools::tool_request::{self, ToolRequestGroup};
use crate::tools::{lock, shims};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim(args: ShimArgs) -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let data_dir = paths.data_dir.as_deref().ok_or("no data directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	std::fs::create_dir_all(data_dir)?;
	let _guard = lock::acquire(data_dir)?;
	let lock = lock::read(data_dir)?;
	let (mut state, text) = state::read(data_dir)?;
	let configs = loader::load(&paths, &state.registered_paths)?;
	let requests = ToolRequestGroup::from_entries(tool_request::requests(&configs)?);
	let catalog = Catalog::new(&lock, &requests);
	let installations = catalog.installations();
	shims::index_binaries(&mut state, tools_dir, installations)?;
	let index_err = shims::index_shims(&mut state, installations).err();
	state::write(data_dir, &state, text.as_deref())?;
	let shims_dir = paths.shims_dir.as_deref().ok_or("no shims directory")?;
	if args.force {
		shims::purge(shims_dir)?;
	}
	let generate_err = shims::generate(&state, shims_dir, installations).err();
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
