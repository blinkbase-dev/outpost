use std::ffi::OsString;

use clap::Args;

use crate::config::locate;
use crate::tools::tool_spec::ToolSpec;
use crate::tools::{candidates, lock};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn exec(args: ExecArgs) -> Result<i32, Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = config_dir.join("tools");
	let lock = lock::read(config_dir)?;
	let installed = candidates::installed(candidates::requests(&paths)?, &lock, &tools_dir);
	let spec = ToolSpec::parse(&args.tool)?;
	let tool = installed
		.iter()
		.find(|tool| spec.matches(&tool.request))
		.ok_or_else(|| format!("`{}` is not installed or not requested by the current config", args.tool))?;
	let mut command = args.command.into_iter();
	let binary = command.next().ok_or("missing binary name after `--`")?;
	let binary = binary.to_str().ok_or("binary name must be valid utf-8")?;
	let matches = candidates::find_binaries(&tool.install_dir, binary)?;
	let path = match matches.as_slice() {
		[path] => path,
		[] => {
			return Err(format!("`{binary}` was not found in {} ({})", tool.origin.display(), tool.installed_version).into());
		}
		paths => {
			let mut message = format!("multiple `{binary}` binaries found in {}:", tool.install_dir.display());
			for path in paths {
				message.push_str(&format!("\n  {}", path.display()));
			}
			return Err(message.into());
		}
	};
	let status = std::process::Command::new(path).args(command).status()?;
	Ok(_exit_code(status))
}

#[derive(Args)]
pub(crate) struct ExecArgs {
	tool: String,
	#[arg(last = true)]
	command: Vec<OsString>,
}

#[cfg(unix)]
fn _exit_code(status: std::process::ExitStatus) -> i32 {
	if let Some(code) = status.code() {
		return code;
	}
	use std::os::unix::process::ExitStatusExt;
	status.signal().map_or(1, |signal| 128 + signal)
}

#[cfg(not(unix))]
fn _exit_code(status: std::process::ExitStatus) -> i32 {
	status.code().unwrap_or(1)
}
