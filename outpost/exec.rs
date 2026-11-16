use std::ffi::{OsStr, OsString};

use clap::Args;

use crate::config::locate;
use crate::tools::tool_spec::ToolSpec;
use crate::tools::{candidates, lock};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn exec(args: ExecArgs) -> Result<i32, Error> {
	if args.shim {
		let mut command = args.command.into_iter();
		let shim = command.next().ok_or("missing shim name")?;
		let args: Vec<_> = command.collect();
		return exec_shim(shim.as_os_str(), &args);
	}
	exec_tool(args)
}

fn exec_tool(args: ExecArgs) -> Result<i32, Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let lock = lock::read(config_dir)?;
	let requests = candidates::requests(&paths)?;
	let installed = candidates::installed(&requests, &lock);
	let tool = args.tool.ok_or("missing tool")?;
	let tool_spec = tool.to_str().ok_or("tool must be valid utf-8")?;
	let spec = ToolSpec::parse(tool_spec)?;
	let Some(request) = requests.iter().find(|entry| spec.matches(&entry.tool)) else {
		return Err(format!("`{tool_spec}` is not requested by the current config").into());
	};
	let identity = request.tool.identity();
	let tool = match installed.get(&identity) {
		Some(tool) => tool,
		None => {
			return Err(
				format!(
					"`{tool_spec}` is not installed\n  from {}\nrun `op install` to install it",
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
			if !install_dir.is_dir() {
				return Err(
					format!(
						"`{tool_spec}` is not installed\n  from {}\nrun `op install` to install it",
						request.origin.display()
					)
					.into(),
				);
			}
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

#[cfg(windows)]
fn exec_shim(shim: &OsStr, args: &[OsString]) -> Result<i32, Error> {
	use std::collections::{HashMap, HashSet};
	use std::path::Path;

	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let lock = lock::read(config_dir)?;
	let requests = candidates::requests(&paths)?;
	let name = Path::new(shim).file_stem().and_then(OsStr::to_str).ok_or("invalid shim name")?;
	let shims = paths.state.shims.get(&name.to_ascii_lowercase());
	if !paths.state.shims.is_empty() && shims.is_none() {
		return Err(format!("no installed tool provides `{name}`").into());
	}
	let locked: HashMap<_, _> = lock.tools.iter().map(|tool| (tool.key(), tool)).collect();
	let mut seen = HashSet::new();
	for request in &requests {
		let Some(tool) = locked.get(&request.tool.key()) else {
			continue;
		};
		let install_path = match tool {
			lock::LockedTool::Github { install_path, .. } => install_path,
		};
		if !seen.insert(install_path) {
			continue;
		}
		if shims.is_some_and(|shims| !shims.contains(install_path)) {
			continue;
		}
		let install_dir = tools_dir.join(install_path);
		let cached = paths.state.binaries.get(install_path).map(Vec::as_slice);
		let matches = candidates::find_binaries_cached(&install_dir, name, cached)?;
		match matches.as_slice() {
			[path] => return Ok(candidates::run(path, args)?),
			[] => continue,
			paths => {
				let mut message = format!("multiple `{name}` binaries found in {}:", install_dir.display());
				for path in paths {
					message.push_str(&format!("\n  {}", path.display()));
				}
				return Err(message.into());
			}
		}
	}
	Err(format!("no installed tool provides `{name}`").into())
}

#[cfg(not(windows))]
fn exec_shim(_shim: &OsStr, _args: &[OsString]) -> Result<i32, Error> {
	Ok(0)
}

#[derive(Args)]
pub(crate) struct ExecArgs {
	#[arg(long, hide = true, conflicts_with = "tool")]
	shim: bool,
	tool: Option<OsString>,
	#[arg(last = true)]
	command: Vec<OsString>,
}
