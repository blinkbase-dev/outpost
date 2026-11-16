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
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let lock = lock::read(config_dir)?;
	let requests = candidates::requests(&paths)?;
	let installed = candidates::installed(&requests, &lock, tools_dir);
	let spec = ToolSpec::parse(&args.tool)?;
	let Some(request) = requests.iter().find(|entry| spec.matches(&entry.tool)) else {
		return Err(format!("`{}` is not requested by the current config", args.tool).into());
	};
	let identity = request.tool.identity();
	let tool = match installed.get(&identity) {
		Some(tool) => tool,
		None => {
			return Err(
				format!(
					"`{}` is not installed\n  from {}\nrun `op install` to install it",
					args.tool,
					request.origin.display()
				)
				.into(),
			);
		}
	};
	let mut command = args.command.into_iter();
	let binary = command.next().ok_or("missing binary name after `--`")?;
	let binary = binary.to_str().ok_or("binary name must be valid utf-8")?;
	let install_dir = tools_dir.join(&tool.install_path);
	let matches = candidates::find_binaries_cached(&install_dir, binary, paths.state.binaries.get(&tool.install_path).map(Vec::as_slice))?;
	let path = match matches.as_slice() {
		[path] => path,
		[] => {
			return Err(format!("`{binary}` was not found in {} ({})", tool.origin.display(), tool.installed_version).into());
		}
		paths => {
			let mut message = format!("multiple `{binary}` binaries found in {}:", install_dir.display());
			for path in paths {
				message.push_str(&format!("\n  {}", path.display()));
			}
			return Err(message.into());
		}
	};
	Ok(candidates::run(path, command)?)
}

#[derive(Args)]
pub(crate) struct ExecArgs {
	tool: String,
	#[arg(last = true)]
	command: Vec<OsString>,
}
