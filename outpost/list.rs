use crate::config::{loader, locate};
use crate::tools::lock::{self, LockedTool};
use crate::tools::status::{self, InstallState, ToolStatus};
use crate::tools::tool_request;

type Error = Box<dyn std::error::Error + Send + Sync>;

pub fn list() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let config = loader::load(&paths)?;
	let requests = tool_request::requests(&config)?;
	let config_dir = paths.system_dir.as_deref().ok_or("no system config directory")?;
	let tools_dir = config_dir.join("tools");
	let lock = lock::read(config_dir)?;
	let report = status::report(&requests, &lock, &tools_dir);
	let mut rows = Vec::with_capacity(report.requested.len() + report.unrequested.len());
	for status in &report.requested {
		let (tool, requested, version) = match status {
			ToolStatus::Github {
				full_name,
				lane,
				requested,
				tag_name,
				install_state,
			} => {
				let version = match tag_name {
					Some(tag_name) => match install_state {
						InstallState::Installed => tag_name.clone(),
						InstallState::Missing => format!("{tag_name} (missing)"),
					},
					None => "missing".to_owned(),
				};
				(format!("github:{full_name}"), format!("{lane}{requested}"), version)
			}
		};
		rows.push(Row { tool, requested, version });
	}
	for entry in &report.unrequested {
		let (tool, version) = match entry {
			LockedTool::Github {
				full_name,
				tag_name,
				install_path,
				..
			} => {
				let version = if tools_dir.join(install_path).exists() {
					tag_name.clone()
				} else {
					format!("{tag_name} (missing)")
				};
				(format!("github:{full_name}"), version)
			}
		};
		rows.push(Row {
			tool,
			requested: "-".to_owned(),
			version,
		});
	}
	_print(&rows);
	Ok(())
}

struct Row {
	tool: String,
	requested: String,
	version: String,
}

fn _print(rows: &[Row]) {
	if rows.is_empty() {
		return;
	}
	const GAP: usize = 2;
	const MIN_TOOL: usize = 20;
	const MAX_REQUESTED: usize = 16;
	const MAX_VERSION: usize = 24;
	let mut tool_width = "TOOL".len();
	let mut requested_width = "REQUESTED".len();
	let mut version_width = "VERSION".len();
	for row in rows {
		tool_width = tool_width.max(row.tool.chars().count());
		requested_width = requested_width.max(row.requested.chars().count());
		version_width = version_width.max(row.version.chars().count());
	}
	if let Some(width) = terminal_size::terminal_size().map(|(width, _)| width.0 as usize) {
		// keep the fixed columns bounded even if a value is unusually long
		requested_width = requested_width.min(MAX_REQUESTED);
		version_width = version_width.min(MAX_VERSION);
		let fixed = requested_width + version_width + GAP * 2;
		// the table is wider than the terminal
		if tool_width + fixed > width {
			// shrink the tool column, but keep it wide enough to be useful
			if width > fixed + MIN_TOOL {
				tool_width = width - fixed;
			} else {
				_print_stacked(rows);
				return;
			}
		}
	}
	println!("{:<tool_width$}  {:<requested_width$}  VERSION", "TOOL", "REQUESTED");
	for row in rows {
		let tool = _truncate(&row.tool, tool_width);
		let requested = _truncate(&row.requested, requested_width);
		let version = _truncate(&row.version, version_width);
		println!("{tool:<tool_width$}  {requested:<requested_width$}  {version}");
	}
}

fn _print_stacked(rows: &[Row]) {
	for row in rows {
		println!("{}", row.tool);
		println!("  requested  {}", row.requested);
		println!("  version    {}", row.version);
	}
}

fn _truncate(value: &str, width: usize) -> String {
	let len = value.chars().count();
	if len <= width {
		return value.to_owned();
	}
	if width == 0 {
		return String::new();
	}
	let keep = width - 1;
	let head = keep / 2;
	let tail = keep - head;
	let mut truncated: String = value.chars().take(head).collect();
	truncated.push('…');
	truncated.extend(value.chars().skip(len - tail));
	truncated
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keeps_values_that_fit() {
		assert_eq!(_truncate("github:owner/repo", 20), "github:owner/repo");
	}

	#[test]
	fn truncates_long_values_in_the_middle() {
		assert_eq!(_truncate("github:owner/really-long-repo", 20), "github:ow…-long-repo");
	}
}
