use crate::config::{loader, locate, state};
use crate::tools::catalog::Catalog;
use crate::tools::lock::{self, LockedTool};
use crate::tools::status::{self, InstallState};
use crate::tools::tool_request::{self, RequestedTool};

type Error = Box<dyn std::error::Error + Send + Sync>;

pub(crate) fn list() -> Result<(), Error> {
	let cwd = std::env::current_dir()?;
	let paths = locate::locate(&cwd)?;
	let data_dir = paths.data_dir.as_deref().ok_or("no data directory")?;
	let tools_dir = paths.tools_dir.as_deref().ok_or("no tools directory")?;
	let (state, _) = state::read(data_dir)?;
	let configs = loader::load(&paths, &state.registered_paths)?;
	let requests = tool_request::requests(&configs)?;
	let lock = lock::read(data_dir)?;
	let catalog = Catalog::new(&lock, &requests);
	let report = status::report(&catalog, tools_dir);
	let mut rows = Vec::with_capacity(report.requested.len() + report.unrequested.len());
	for entry in &report.requested {
		let (tool, requested, origin) = match &entry.tool {
			RequestedTool::Github(req) => (format!("github:{}", req.full_name), format!("{}{}", req.lane, req.version), req.api_url.clone()),
		};
		let version = match &entry.installed_version {
			Some(version) => match entry.install_state {
				InstallState::Installed => version.clone(),
				InstallState::Missing => format!("{version} (missing)"),
			},
			None => "missing".to_owned(),
		};
		rows.push(Row {
			tool,
			requested,
			version,
			source: entry.origin.display().to_string(),
			origin,
		});
	}
	for entry in &report.unrequested {
		let (tool, version, origin) = match entry {
			LockedTool::Github(lock) => {
				let version = if tools_dir.join(&lock.install_path).exists() {
					lock.tag_name.clone()
				} else {
					format!("{} (missing)", lock.tag_name)
				};
				(format!("github:{}", lock.full_name), version, lock.api_url.clone())
			}
		};
		rows.push(Row {
			tool,
			requested: "-".to_owned(),
			version,
			source: "-".to_owned(),
			origin,
		});
	}
	_print(&rows);
	Ok(())
}

struct Row {
	tool: String,
	requested: String,
	version: String,
	source: String,
	origin: String,
}

fn _print(rows: &[Row]) {
	if rows.is_empty() {
		return;
	}
	let mut tool_width = "TOOL".len();
	let mut requested_width = "REQUESTED".len();
	let mut version_width = "VERSION".len();
	let mut source_width = "SOURCE".len();
	let mut origin_width = "ORIGIN".len();
	for row in rows {
		tool_width = tool_width.max(row.tool.chars().count());
		requested_width = requested_width.max(row.requested.chars().count());
		version_width = version_width.max(row.version.chars().count());
		source_width = source_width.max(row.source.chars().count());
		origin_width = origin_width.max(row.origin.chars().count());
	}
	let fitted = match terminal_size::terminal_size_of(std::io::stdout()).map(|(width, _)| width.0 as usize) {
		Some(width) => _fit(tool_width, requested_width, version_width, source_width, origin_width, width),
		None => Some((tool_width, requested_width, version_width, source_width, origin_width)),
	};
	let Some((tool_width, requested_width, version_width, source_width, origin_width)) = fitted else {
		_print_stacked(rows);
		return;
	};
	println!(
		"{:<tool_width$}  {:<requested_width$}  {:<version_width$}  {:<source_width$}  ORIGIN",
		"TOOL", "REQUESTED", "VERSION", "SOURCE"
	);
	for row in rows {
		let tool = _truncate(&row.tool, tool_width);
		let requested = _truncate(&row.requested, requested_width);
		let version = _truncate(&row.version, version_width);
		let source = _truncate(&row.source, source_width);
		let origin = _truncate(&row.origin, origin_width);
		let line = format!("{tool:<tool_width$}  {requested:<requested_width$}  {version:<version_width$}  {source:<source_width$}  {origin}");
		println!("{}", line.trim_end());
	}
}

fn _fit(tool: usize, requested: usize, version: usize, source: usize, origin: usize, width: usize) -> Option<(usize, usize, usize, usize, usize)> {
	const GAP: usize = 2;
	const MIN_TOOL: usize = 20;
	const MAX_REQUESTED: usize = 16;
	const MAX_VERSION: usize = 24;
	const MAX_SOURCE: usize = 48;
	const MAX_ORIGIN: usize = 32;
	let fixed = requested + version + source + origin + GAP * 4;
	let available = width.saturating_sub(fixed);
	// the tool column fits naturally, or can shrink to its minimum
	if available >= tool.min(MIN_TOOL) {
		return Some((tool.min(available), requested, version, source, origin));
	}
	// retry with the fixed columns capped
	let requested = requested.min(MAX_REQUESTED);
	let version = version.min(MAX_VERSION);
	let source = source.min(MAX_SOURCE);
	let origin = origin.min(MAX_ORIGIN);
	let fixed = requested + version + source + origin + GAP * 4;
	let available = width.saturating_sub(fixed);
	if available >= tool.min(MIN_TOOL) {
		return Some((tool.min(available), requested, version, source, origin));
	}
	None
}

fn _print_stacked(rows: &[Row]) {
	for row in rows {
		println!("{}", row.tool);
		println!("  requested  {}", row.requested);
		println!("  version    {}", row.version);
		println!("  source     {}", row.source);
		println!("{}", format!("  origin     {}", row.origin).trim_end());
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
	fn fits_the_natural_table() {
		assert_eq!(_fit(10, 5, 6, 10, 6, 80), Some((10, 5, 6, 10, 6)));
	}

	#[test]
	fn shrinks_the_tool_column_before_capping() {
		assert_eq!(_fit(60, 5, 6, 10, 6, 60), Some((25, 5, 6, 10, 6)));
	}

	#[test]
	fn caps_fixed_columns_when_the_tool_column_is_too_narrow() {
		assert_eq!(_fit(10, 30, 30, 60, 6, 124), Some((10, 16, 24, 48, 6)));
	}

	#[test]
	fn stacks_when_the_capped_table_still_does_not_fit() {
		assert_eq!(_fit(30, 30, 30, 60, 6, 40), None);
	}

	#[test]
	fn keeps_values_that_fit() {
		assert_eq!(_truncate("github:owner/repo", 20), "github:owner/repo");
	}

	#[test]
	fn truncates_long_values_in_the_middle() {
		assert_eq!(_truncate("github:owner/really-long-repo", 20), "github:ow…-long-repo");
	}
}
