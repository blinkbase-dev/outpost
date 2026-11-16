use std::ffi::{OsStr, OsString};

use clap::Args;

use crate::config::locate;
use crate::tools::tool_request::ToolRequest;
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
	let tool = args.tool.ok_or("missing tool")?;
	let tool_spec = tool.to_str().ok_or("tool must be valid utf-8")?;
	let spec = ToolSpec::parse(tool_spec)?;
	let Some(request) = requests.iter().find(|entry| spec.matches(&entry.tool)) else {
		return Err(format!("`{tool_spec}` is not requested by the current config").into());
	};
	let Some((install_path, installed_version)) = lock.tools.iter().find_map(|tool| match (&request.tool, tool) {
		(
			ToolRequest::Github {
				full_name: request_name,
				lane: request_lane,
				version,
			},
			lock::LockedTool::Github {
				full_name,
				lane,
				requested,
				install_path,
				tag_name,
				..
			},
		) => (full_name == request_name && lane == request_lane && requested == version).then_some((install_path, tag_name)),
	}) else {
		return Err(
			format!(
				"`{tool_spec}` is not installed\n  from {}\nrun `op install` to install it",
				request.origin.display()
			)
			.into(),
		);
	};
	let mut command = args.command.into_iter();
	let binary = command.next().ok_or("missing binary name after `--`")?;
	let binary = binary.to_str().ok_or("binary name must be valid utf-8")?;
	let install_dir = tools_dir.join(install_path);
	let matches = candidates::find_binaries_cached(&install_dir, binary, paths.state.binaries.get(install_path).map(Vec::as_slice))?;
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
			return Err(format!("`{binary}` was not found in {} ({})", request.origin.display(), installed_version).into());
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
	use std::path::{Component, Path};

	use crate::config::state::ShimEntry;

	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let name = Path::new(shim).file_stem().and_then(OsStr::to_str).ok_or("invalid shim name")?;
	let Some(shims) = paths.state.shims.get(&name.to_ascii_lowercase()) else {
		return Err(format!("no installed tool provides `{name}`").into());
	};
	let requests = candidates::requests(&paths)?;
	for request in &requests {
		let Some((install_path, binaries)) = shims.iter().find_map(|entry| match (&request.tool, entry) {
			(
				ToolRequest::Github {
					full_name: request_name,
					lane: request_lane,
					version,
				},
				ShimEntry::Github {
					full_name,
					lane,
					requested,
					install_path,
					binaries,
				},
			) => (full_name == request_name && lane == request_lane && requested == version).then_some((install_path, binaries)),
		}) else {
			continue;
		};
		if !install_path.components().all(|component| matches!(component, Component::Normal(_))) {
			continue;
		}
		let install_dir = tools_dir.join(install_path);
		let binary = match binaries.as_slice() {
			[binary] => binary,
			[] => continue,
			binaries => {
				let mut message = format!("multiple `{name}` binaries found in {}:", install_dir.display());
				for binary in binaries {
					message.push_str(&format!("\n  {}", install_dir.join(binary).display()));
				}
				return Err(message.into());
			}
		};
		if !binary.components().all(|component| matches!(component, Component::Normal(_))) {
			continue;
		}
		let path = install_dir.join(binary);
		if !path.is_file() {
			continue;
		}
		return Ok(candidates::run(&path, args)?);
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
