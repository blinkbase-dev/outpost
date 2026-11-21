use std::ffi::{OsStr, OsString};
use std::path::{Component, Path};

use clap::Args;

use crate::config::state::ShimEntry;
use crate::config::{locate, state};
use crate::tools::tool_request::RequestedTool;
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
	let data_dir = paths.data_dir.as_deref().ok_or("no data directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let lock = lock::read(data_dir)?;
	let requests = candidates::requests(&paths)?;
	let tool = args.tool.ok_or("missing tool")?;
	let tool_spec = tool.to_str().ok_or("tool must be valid utf-8")?;
	let spec = ToolSpec::parse(tool_spec)?;
	let Some(request) = requests.iter().find(|entry| spec.matches(&entry.tool)) else {
		return Err(format!("'{tool_spec}' is not requested by the current config").into());
	};
	let Some((install_path, installed_version)) = lock.tools.iter().find_map(|tool| match (&request.tool, tool) {
		(
			RequestedTool::Github {
				api_url: request_url,
				full_name: request_name,
				lane: request_lane,
				version,
				..
			},
			lock::LockedTool::Github {
				api_url,
				full_name,
				lane,
				requested,
				install_path,
				tag_name,
				..
			},
		) => (api_url == request_url && full_name == request_name && lane == request_lane && requested == version).then_some((install_path, tag_name)),
	}) else {
		return Err(
			format!(
				"'{tool_spec}' is not installed\n  from {}\nrun 'op install' to install it",
				request.origin.display()
			)
			.into(),
		);
	};
	let mut command = args.command.into_iter();
	let binary = command.next().ok_or("missing binary name after '--'")?;
	let binary = binary.to_str().ok_or("binary name must be valid utf-8")?;
	let install_dir = tools_dir.join(install_path);
	let (state, _) = state::read(data_dir)?;
	// shims pass an install-relative path so same-named binaries do not collide
	let binary_path = Path::new(binary);
	let matches = if binary_path.components().count() > 1 && binary_path.components().all(|component| matches!(component, Component::Normal(_))) {
		vec![install_dir.join(binary_path)]
	} else {
		candidates::find_binaries_cached(&install_dir, binary, state.binaries.get(install_path).map(Vec::as_slice))?
	};
	let path = match matches.as_slice() {
		[path] => path,
		[] => {
			if !install_dir.is_dir() {
				return Err(
					format!(
						"'{tool_spec}' is not installed\n  from {}\nrun 'op install' to install it",
						request.origin.display()
					)
					.into(),
				);
			}
			return Err(format!("'{binary}' was not found in {} ({})", request.origin.display(), installed_version).into());
		}
		paths => {
			let mut message = format!("multiple '{binary}' binaries found in {}:", install_dir.display());
			for path in paths {
				message.push_str(&format!("\n  {}", path.display()));
			}
			return Err(message.into());
		}
	};
	Ok(candidates::run(path, command)?)
}

fn exec_shim(shim: &OsStr, args: &[OsString]) -> Result<i32, Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let data_dir = paths.data_dir.as_deref().ok_or("no data directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let name = _shim_name(shim)?;
	let (state, _) = state::read(data_dir)?;
	let Some(shims) = state.shims.get(&name) else {
		return Err(format!("no installed tool provides '{name}'").into());
	};
	let requests = candidates::requests(&paths)?;
	let mut missing = None;
	// several versions can share a name, so the first configured request wins
	for request in &requests {
		let Some((install_path, binary)) = shims.iter().find_map(|entry| match (&request.tool, entry) {
			(
				RequestedTool::Github {
					api_url: request_url,
					full_name: request_name,
					lane: request_lane,
					version,
					..
				},
				ShimEntry::Github {
					api_url,
					full_name,
					lane,
					requested,
					install_path,
					binary,
					..
				},
			) => (api_url == request_url && full_name == request_name && lane == request_lane && requested == version).then_some((install_path, binary)),
		}) else {
			continue;
		};
		if !install_path.components().all(|component| matches!(component, Component::Normal(_))) {
			continue;
		}
		if !binary.components().all(|component| matches!(component, Component::Normal(_))) {
			continue;
		}
		let path = tools_dir.join(install_path).join(binary);
		if !path.is_file() {
			missing = Some(tools_dir.join(install_path));
			continue;
		}
		return Ok(candidates::run(&path, args)?);
	}
	if let Some(dir) = missing {
		return Err(format!("'{name}' was not found in {}", dir.display()).into());
	}
	// the config requests this tool, but not a version that is installed
	for request in &requests {
		let spec = match &request.tool {
			RequestedTool::Github {
				api_url,
				full_name,
				lane,
				version,
				..
			} => {
				let installed = shims.iter().any(|entry| match entry {
					ShimEntry::Github {
						api_url: installed_url,
						full_name: installed_name,
						lane: installed_lane,
						..
					} => installed_url == api_url && installed_name == full_name && installed_lane == lane,
				});
				installed.then(|| format!("github:{full_name}@{lane}{version}"))
			}
		};
		if let Some(spec) = spec {
			return Err(format!("'{name}' is not installed for '{spec}'\nrun 'op install' to install it").into());
		}
	}
	let mut message = format!("'{name}' is not requested by the current config:");
	for entry in shims {
		message.push_str(&match entry {
			ShimEntry::Github {
				full_name, lane, requested, ..
			} => format!("\n  github:{full_name} {lane}{requested}"),
		});
	}
	Err(message.into())
}

// windows strips .exe and lowercases, unix keeps the full file name
#[cfg(windows)]
fn _shim_name(shim: &OsStr) -> Result<String, Error> {
	let name = Path::new(shim).file_stem().and_then(OsStr::to_str).ok_or("invalid shim name")?;
	Ok(name.to_ascii_lowercase())
}

#[cfg(not(windows))]
fn _shim_name(shim: &OsStr) -> Result<String, Error> {
	let name = Path::new(shim).file_name().and_then(OsStr::to_str).ok_or("invalid shim name")?;
	Ok(name.to_owned())
}

#[derive(Args)]
pub(crate) struct ExecArgs {
	#[arg(long, hide = true, conflicts_with = "tool")]
	shim: bool,
	tool: Option<OsString>,
	#[arg(last = true)]
	command: Vec<OsString>,
}
