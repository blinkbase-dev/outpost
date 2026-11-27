use std::ffi::OsString;

use clap::Args;

use crate::exec;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn which(args: WhichArgs) -> Result<(), Error> {
	let path = exec::resolve_shim(&args.name)?;
	println!("{}", path.display());
	Ok(())
}

#[derive(Args)]
pub(crate) struct WhichArgs {
	name: OsString,
}
