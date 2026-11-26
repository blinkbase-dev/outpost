use std::io::IsTerminal;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use console::Term;

const DRAW_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct ProgressBar {
	term: Option<Term>,
	inner: Mutex<Inner>,
}

struct Inner {
	state: State,
	drawn_lines: usize,
	last_draw: Instant,
}

impl ProgressBar {
	pub(crate) fn new(labels: Vec<String>) -> Self {
		let term = std::io::stderr().is_terminal().then(Term::stderr);
		Self {
			term,
			inner: Mutex::new(Inner {
				state: State::new(labels),
				drawn_lines: 0,
				last_draw: Instant::now(),
			}),
		}
	}

	pub(crate) fn record(&self, event: ProgressEvent) {
		let Some(term) = &self.term else {
			return;
		};
		let mut inner = self.inner.lock().unwrap();
		inner.state.update(event, Instant::now());
		if inner.last_draw.elapsed() < DRAW_INTERVAL {
			return;
		}
		let (_, columns) = term.size();
		let lines = draw(&inner.state, columns as usize);
		inner.drawn_lines = Self::_write(term, &lines, inner.drawn_lines);
		inner.last_draw = Instant::now();
	}

	fn _write(term: &Term, lines: &[String], drawn_lines: usize) -> usize {
		let _ = term.clear_last_lines(drawn_lines);
		for line in lines {
			let _ = term.write_line(line);
		}
		let _ = term.flush();
		lines.len()
	}
}

impl Drop for ProgressBar {
	fn drop(&mut self) {
		let Some(term) = &self.term else {
			return;
		};
		if let Ok(inner) = self.inner.lock() {
			let _ = term.clear_last_lines(inner.drawn_lines);
			let _ = term.flush();
		}
	}
}

fn draw(state: &State, width: usize) -> Vec<String> {
	let mut lines = vec![draw_header(&state.header())];
	lines.extend(state.rows().map(|row| draw_row(row, width)));
	lines
}

struct State {
	labels: Vec<String>,
	rows: Vec<Option<Row>>,
	done: usize,
	last_update: Vec<Option<Instant>>,
}

impl State {
	fn new(labels: Vec<String>) -> Self {
		let rows = vec![None; labels.len()];
		let last_update = vec![None; labels.len()];
		Self {
			labels,
			rows,
			done: 0,
			last_update,
		}
	}

	fn update(&mut self, event: ProgressEvent, now: Instant) {
		// guard against an out-of-range index
		let Some(slot) = self.rows.get_mut(event.index) else {
			return;
		};
		if event.stage == Stage::Extract && event.done >= event.total {
			if slot.take().is_some() {
				self.done += 1;
			}
			self.last_update[event.index] = None;
			return;
		}
		let label = self.labels[event.index].clone();
		let row = slot.get_or_insert_with(|| Row {
			label,
			stage: event.stage,
			done: 0,
			total: event.total,
			rate: 0.0,
		});
		if row.stage != event.stage {
			row.stage = event.stage;
			row.rate = 0.0;
		} else if let Some(last_seen) = self.last_update[event.index] {
			let elapsed = now.saturating_duration_since(last_seen).as_secs_f64();
			if elapsed > 0.0 {
				let instant = event.done.saturating_sub(row.done) as f64 / elapsed;
				// exponential moving average keeps 70% of the old rate and 30% of the new sample
				row.rate = if row.rate == 0.0 { instant } else { row.rate * 0.7 + instant * 0.3 };
			}
		}
		if event.done >= event.total {
			row.rate = 0.0;
		}
		row.done = event.done;
		row.total = event.total;
		self.last_update[event.index] = Some(now);
	}

	fn header(&self) -> Header {
		let mut download_rate = 0.0;
		let mut extract_rate = 0.0;
		for row in self.rows() {
			match row.stage {
				Stage::Download => download_rate += row.rate,
				Stage::Extract => extract_rate += row.rate,
			}
		}
		Header {
			done: self.done,
			total: self.labels.len(),
			download_rate,
			extract_rate,
		}
	}

	fn rows(&self) -> impl Iterator<Item = &Row> {
		self.rows.iter().flatten()
	}
}

pub(crate) struct ProgressEvent {
	pub(crate) index: usize,
	pub(crate) stage: Stage,
	pub(crate) done: u64,
	pub(crate) total: u64,
}

// 3/12  dl 8.1 MiB/s  ex 4.2 MiB/s
fn draw_header(header: &Header) -> String {
	let mut line = format!("{}/{}", header.done, header.total);
	if header.download_rate > 0.0 {
		line.push_str(&format!("  dl {}/s", _format_bytes(header.download_rate)));
	}
	if header.extract_rate > 0.0 {
		line.push_str(&format!("  ex {}/s", _format_bytes(header.extract_rate)));
	}
	line
}

struct Header {
	done: usize,
	total: usize,
	download_rate: f64,
	extract_rate: f64,
}

// owner/repo v1.2.3  downloading  [##########----------]       2.6 MiB/5.2 MiB     6.1 MiB/s  00:03
//
// 108+ columns: label, stage, 20 cell bar, done/total, speed, eta, 80 fixed cells
//  96+ columns: label, stage, 10 cell bar, done/total, speed, eta, 70 fixed cells
//  84+ columns: label, stage, done/total, speed, eta, 57 fixed cells
//  60+ columns: label, stage, done/total, 36 fixed cells
//  60< columns: label, stage, 13 fixed cells
fn draw_row(row: &Row, width: usize) -> String {
	let stage = match row.stage {
		Stage::Download if row.done >= row.total => "queued",
		Stage::Download => "downloading",
		Stage::Extract => "extracting",
	};
	// total is zero only before the first progress event
	let ratio = row.done.min(row.total) as f64 / row.total.max(1) as f64;
	let bar_cells = if width >= 108 { 20 } else { 10 };
	let filled = (ratio * bar_cells as f64).round() as usize;
	let bar = format!("[{}{}]", "#".repeat(filled), "-".repeat(bar_cells - filled));
	let amount = format!("{}/{}", _format_bytes(row.done as f64), _format_bytes(row.total as f64));
	let speed = format!("{}/s", _format_bytes(row.rate));
	let eta = _format_eta(row.total.saturating_sub(row.done) as f64 / row.rate);
	let tail = if width >= 96 {
		format!("  {stage:<11}  {bar} {amount:>21}  {speed:>12}  {eta:>5}")
	} else if width >= 84 {
		format!("  {stage:<11}  {amount:>21}  {speed:>12}  {eta:>5}")
	} else if width >= 60 {
		format!("  {stage:<11}  {amount:>21}")
	} else {
		format!("  {stage:<11}")
	};
	let label_width = width.saturating_sub(tail.len());
	let label = _truncate(&row.label, label_width);
	format!("{label:<label_width$}{tail}")
}

#[derive(Clone)]
struct Row {
	label: String,
	stage: Stage,
	done: u64,
	total: u64,
	rate: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
	Download,
	Extract,
}

fn _format_bytes(value: f64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = value.max(0.0);
	let mut unit = 0;
	while value >= 1024.0 && unit + 1 < UNITS.len() {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{value:.0} {}", UNITS[unit])
	} else {
		format!("{value:.1} {}", UNITS[unit])
	}
}

fn _format_eta(seconds: f64) -> String {
	if !seconds.is_finite() || seconds <= 0.0 {
		return "--".to_owned();
	}
	let seconds = (seconds.ceil() as u64).min(3599);
	format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn _truncate(value: &str, width: usize) -> String {
	if value.chars().count() <= width {
		return value.to_owned();
	}
	if width == 0 {
		return String::new();
	}
	let mut text: String = value.chars().take(width - 1).collect();
	text.push('…');
	text
}

#[cfg(test)]
mod draw_tests {
	use std::time::{Duration, Instant};

	use indoc::indoc;

	use super::*;

	fn _row() -> Row {
		Row {
			label: "github:owner/repo v1.2.3".to_owned(),
			stage: Stage::Download,
			done: 1024 * 1024,
			total: 2 * 1024 * 1024,
			rate: 6.0 * 1024.0 * 1024.0,
		}
	}

	fn _event(index: usize, stage: Stage, done: u64, total: u64) -> ProgressEvent {
		ProgressEvent { index, stage, done, total }
	}

	#[test]
	fn draws_at_exact_width_boundaries() {
		let row = _row();
		let tails = indoc! {r#"
		downloading  [##########----------]       1.0 MiB/2.0 MiB     6.0 MiB/s  00:01
		downloading  [#####-----]       1.0 MiB/2.0 MiB     6.0 MiB/s  00:01
		downloading  [#####-----]       1.0 MiB/2.0 MiB     6.0 MiB/s  00:01
		downloading        1.0 MiB/2.0 MiB     6.0 MiB/s  00:01
		downloading        1.0 MiB/2.0 MiB     6.0 MiB/s  00:01
		downloading        1.0 MiB/2.0 MiB
		downloading        1.0 MiB/2.0 MiB
		downloading
		downloading
		"#};
		let cases = [
			(108, "github:owner/repo v1.2.3"),
			(107, "github:owner/repo v1.2.3"),
			(96, "github:owner/repo v1.2.3"),
			(95, "github:owner/repo v1.2.3"),
			(84, "github:owner/repo v1.2.3"),
			(83, "github:owner/repo v1.2.3"),
			(60, "github:owner/repo v1.2.3"),
			(59, "github:owner/repo v1.2.3"),
			(36, "github:owner/repo v1.2…"),
		];
		for ((width, label), tail) in cases.into_iter().zip(tails.lines()) {
			let label_width = width - tail.len() - 2;
			let line = draw_row(&row, width);
			assert_eq!(line, format!("{label:<label_width$}  {tail}"), "width {width}");
			assert_eq!(line.chars().count(), width, "line width {width}");
		}
	}

	#[test]
	fn draws_header_first_and_index_order() {
		let mut state = State::new(vec![
			"github:owner/zero v1".to_owned(),
			"github:owner/one v1".to_owned(),
			"github:owner/two v1".to_owned(),
		]);
		let total = 1000;
		let t0 = Instant::now();
		// start tools 0 and 2 and leave tool 1 inactive
		state.update(_event(0, Stage::Download, 0, total), t0);
		state.update(_event(2, Stage::Download, 0, total), t0);
		let lines = draw(&state, 80);
		assert_eq!(lines.len(), 3);
		assert_eq!(lines[0], "0/3");
		assert!(lines[1].starts_with("github:owner/zero v1"));
		assert!(lines[2].starts_with("github:owner/two v1"));
		assert!(lines.iter().all(|line| !line.contains("github:owner/one v1")));
		// extraction completion removes tool 0 and increments done
		state.update(_event(0, Stage::Extract, total, total), t0 + Duration::from_secs(1));
		let lines = draw(&state, 80);
		assert_eq!(lines.len(), 2);
		assert_eq!(lines[0], "1/3");
		assert!(lines[1].starts_with("github:owner/two v1"));
		assert!(lines.iter().all(|line| !line.contains("github:owner/zero v1")));
	}
}

#[cfg(test)]
mod state_tests {
	use std::time::{Duration, Instant};

	use super::*;

	fn _event(index: usize, stage: Stage, done: u64, total: u64) -> ProgressEvent {
		ProgressEvent { index, stage, done, total }
	}

	fn _assert_rate(rate: f64, expected: f64) {
		assert!((rate - expected).abs() < 1e-6, "expected {expected} B/s, got {rate}");
	}

	#[test]
	fn tracks_one_tool_through_both_stages() {
		let mut state = State::new(vec!["github:owner/repo v1.2.3".to_owned()]);
		let total = 2000;
		let t0 = Instant::now();
		// create the download row
		state.update(_event(0, Stage::Download, 0, total), t0);
		let row = state.rows().next().unwrap();
		assert_eq!(row.label, "github:owner/repo v1.2.3");
		assert_eq!(row.stage, Stage::Download);
		assert_eq!(row.done, 0);
		assert_eq!(row.total, total);
		_assert_rate(row.rate, 0.0);
		// samples are one second apart, so each rate is a byte delta
		state.update(_event(0, Stage::Download, 500, total), t0 + Duration::from_secs(1));
		let row = state.rows().next().unwrap();
		assert_eq!(row.done, 500);
		_assert_rate(row.rate, 500.0);
		// the exponential average smooths the next sample
		state.update(_event(0, Stage::Download, 1500, total), t0 + Duration::from_secs(2));
		let row = state.rows().next().unwrap();
		assert_eq!(row.done, 1500);
		_assert_rate(row.rate, 650.0);
		// a completed download has no rate
		state.update(_event(0, Stage::Download, total, total), t0 + Duration::from_secs(3));
		let row = state.rows().next().unwrap();
		assert_eq!(row.stage, Stage::Download);
		assert_eq!(row.done, total);
		_assert_rate(row.rate, 0.0);
		// the stage change resets the row
		state.update(_event(0, Stage::Extract, 0, total), t0 + Duration::from_secs(4));
		let row = state.rows().next().unwrap();
		assert_eq!(row.stage, Stage::Extract);
		assert_eq!(row.done, 0);
		assert_eq!(row.total, total);
		_assert_rate(row.rate, 0.0);
		// the extraction rate is the byte delta
		state.update(_event(0, Stage::Extract, 500, total), t0 + Duration::from_secs(5));
		let row = state.rows().next().unwrap();
		assert_eq!(row.done, 500);
		_assert_rate(row.rate, 500.0);
		// extraction completion removes the row
		state.update(_event(0, Stage::Extract, total, total), t0 + Duration::from_secs(6));
		assert!(state.rows().next().is_none());
		let header = state.header();
		assert_eq!(header.done, 1);
		assert_eq!(header.total, 1);
		assert_eq!(header.download_rate, 0.0);
		assert_eq!(header.extract_rate, 0.0);
	}

	#[test]
	fn splits_header_rates_by_stage() {
		let mut state = State::new(vec!["github:owner/download v1".to_owned(), "github:owner/extract v1".to_owned()]);
		let total = 2000;
		let t0 = Instant::now();
		// tool 0 downloads and tool 1 extracts
		state.update(_event(0, Stage::Download, 0, total), t0);
		state.update(_event(0, Stage::Download, 500, total), t0 + Duration::from_secs(1));
		state.update(_event(1, Stage::Extract, 0, total), t0);
		state.update(_event(1, Stage::Extract, 300, total), t0 + Duration::from_secs(1));
		let header = state.header();
		assert_eq!(header.done, 0);
		assert_eq!(header.total, 2);
		_assert_rate(header.download_rate, 500.0);
		_assert_rate(header.extract_rate, 300.0);
		assert_eq!(state.rows().count(), 2);
		// tool 1 completes and leaves the extraction rate empty
		state.update(_event(1, Stage::Extract, total, total), t0 + Duration::from_secs(2));
		let header = state.header();
		assert_eq!(header.done, 1);
		assert_eq!(header.total, 2);
		_assert_rate(header.download_rate, 500.0);
		_assert_rate(header.extract_rate, 0.0);
		assert_eq!(state.rows().count(), 1);
	}
}
