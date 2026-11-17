use std::path::Path;

use crate::config::locate;
use crate::config::state::{self, State};
use crate::tools::lock::{self, Lock, LockedTool};
use crate::tools::{candidates, shims};

#[cfg(not(windows))]
mod unix_import {
	pub(super) use std::collections::BTreeMap;
	pub(super) use std::path::PathBuf;
}

#[cfg(not(windows))]
use unix_import::*;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn shim() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let _guard = lock::acquire(config_dir)?;
	let lock = lock::read(config_dir)?;
	let (mut state, text) = state::read(config_dir)?;
	_index_binaries(&mut state, &lock, tools_dir)?;
	shims::index_shims(&mut state, &lock);
	state::write(config_dir, &state, text.as_deref())?;
	let shims_dir = paths.shims_dir.as_deref().ok_or("no shims directory")?;
	#[cfg(windows)]
	{
		let trampoline = std::env::current_exe()?.with_file_name("op_shim.exe");
		_generate_shims(shims_dir, &trampoline, state.shims.keys().map(String::as_str))?;
	}
	#[cfg(not(windows))]
	{
		_generate_shims(shims_dir, &lock, &state.binaries)?;
	}
	Ok(())
}

fn _index_binaries(state: &mut State, lock: &Lock, tools_dir: &Path) -> Result<(), Error> {
	let install_paths: Vec<&Path> = lock
		.tools
		.iter()
		.map(|tool| match tool {
			LockedTool::Github { install_path, .. } => install_path.as_path(),
		})
		.collect();
	state.binaries = candidates::find_binaries_by_tool(tools_dir, install_paths)?;
	Ok(())
}

#[cfg(windows)]
fn _generate_shims<'a>(shims_dir: &Path, trampoline: &Path, names: impl IntoIterator<Item = &'a str>) -> Result<(), Error> {
	let bytes = std::fs::read(trampoline).map_err(|error| -> Error { format!("failed to read trampoline `{}`: {error}", trampoline.display()).into() })?;
	std::fs::create_dir_all(shims_dir)?;
	for name in names {
		if !_is_plain_file_name(name) {
			continue;
		}
		let target = shims_dir.join(format!("{name}.exe"));
		if std::fs::read(&target).is_ok_and(|existing| existing == bytes) {
			continue;
		}
		let temp = target.with_extension("exe.tmp");
		match std::fs::rename(&target, &temp) {
			Ok(()) => {}
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
			Err(error) => return Err(error.into()),
		}
		std::fs::write(&target, &bytes)?;
	}
	Ok(())
}

#[cfg(not(windows))]
fn _generate_shims(shims_dir: &Path, lock: &Lock, binaries: &BTreeMap<PathBuf, Vec<PathBuf>>) -> Result<(), Error> {
	use std::ffi::OsStr;
	use std::os::unix::fs::PermissionsExt;

	std::fs::create_dir_all(shims_dir)?;
	for tool in &lock.tools {
		match tool {
			LockedTool::Github { full_name, install_path, .. } => {
				let Some(binaries) = binaries.get(install_path) else {
					continue;
				};
				for binary in binaries {
					let Some(name) = binary.file_name().and_then(OsStr::to_str) else {
						continue;
					};
					if !_is_plain_file_name(name) {
						continue;
					}
					let script = format!(
						"#!/bin/sh\nexec op exec 'github:{}' -- '{}' \"$@\"\n",
						full_name.replace('\'', "'\\''"),
						name.replace('\'', "'\\''")
					);
					let target = shims_dir.join(name);
					if std::fs::read_to_string(&target).is_ok_and(|existing| existing == script) {
						continue;
					}
					let temp = shims_dir.join(format!("{name}.tmp"));
					match std::fs::rename(&target, &temp) {
						Ok(()) => {}
						Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
						Err(error) => return Err(error.into()),
					}
					std::fs::write(&target, &script)?;
					std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
				}
			}
		}
	}
	Ok(())
}

fn _is_plain_file_name(name: &str) -> bool {
	use std::path::Component;

	let mut components = Path::new(name).components();
	matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}
