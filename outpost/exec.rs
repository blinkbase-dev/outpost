use std::collections::{HashMap, HashSet};
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
	let lock = lock::read_unverified(data_dir)?;
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
	let mut shims_map: HashMap<lock::Identity, &ShimEntry> = HashMap::with_capacity(shims.len());
	let mut tools = HashSet::with_capacity(shims.len());
	for entry in shims {
		// keep the first entry if a state file has duplicates
		shims_map.entry(entry.identity()).or_insert(entry);
		tools.insert(entry.identity_key());
	}
	// several versions can share a name, so the first request in config precedence order wins
	for request in &requests {
		let identity = request.tool.identity();
		let Some(entry) = shims_map.get(&identity) else {
			// the entry exists for another version of this tool
			if tools.contains(&request.tool.identity_key()) {
				return Err(format!("'{name}' is not installed for '{identity}'\nrun 'op install' to install it").into());
			}
			continue;
		};
		let ShimEntry::Github { install_path, binary, .. } = entry;
		if !install_path.components().all(|component| matches!(component, Component::Normal(_))) {
			return Err(format!("'{name}' has an invalid shim target\nrun 'op shim' to rebuild it").into());
		}
		if !binary.components().all(|component| matches!(component, Component::Normal(_))) {
			return Err(format!("'{name}' has an invalid shim target\nrun 'op shim' to rebuild it").into());
		}
		let path = tools_dir.join(install_path).join(binary);
		if !path.is_file() {
			return Err(format!("'{name}' was not found in {}", tools_dir.join(install_path).display()).into());
		}
		return Ok(candidates::run(&path, args)?);
	}
	let mut message = format!("'{name}' is provided by tools that the current config does not request:");
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
