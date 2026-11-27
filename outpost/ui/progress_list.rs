use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use console::Term;

use super::progress::_truncate;

// resolve fires only start and finish events, so the ticker redraws to advance elapsed times
const TICK_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct ProgressList {
	term: Option<Term>,
	inner: Arc<Mutex<Inner>>,
	ticker: Option<JoinHandle<()>>,
}

struct Inner {
	state: State,
	drawn_lines: usize,
	stopped: bool,
}

impl ProgressList {
	pub(crate) fn new(labels: Vec<String>) -> Self {
		let term = std::io::stderr().is_terminal().then(Term::stderr);
		let now = Instant::now();
		let inner = Arc::new(Mutex::new(Inner {
			state: State::new(labels, now),
			drawn_lines: 0,
			stopped: false,
		}));
		let ticker = term.clone().map(|term| {
			let inner = Arc::clone(&inner);
			thread::spawn(move || Self::_tick(&term, &inner))
		});
		Self { term, inner, ticker }
	}

	// the ticker draws the change on its next tick
	pub(crate) fn record(&self, event: ProgressEvent) {
		if self.term.is_none() {
			return;
		}
		self.inner.lock().unwrap().state.update(event, Instant::now());
	}

	fn _tick(term: &Term, inner: &Mutex<Inner>) {
		loop {
			thread::park_timeout(TICK_INTERVAL);
			let Ok(mut inner) = inner.lock() else {
				return;
			};
			// stop once drop has cleared the lines
			if inner.stopped {
				return;
			}
			let (height, columns) = term.size();
			let lines = draw(&inner.state, Instant::now(), columns as usize, height as usize);
			inner.drawn_lines = Self::_write(term, &lines, inner.drawn_lines);
		}
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

impl Drop for ProgressList {
	fn drop(&mut self) {
		let Some(term) = &self.term else {
			return;
		};
		if let Ok(mut inner) = self.inner.lock() {
			inner.stopped = true;
			let _ = term.clear_last_lines(inner.drawn_lines);
			let _ = term.flush();
		}
		if let Some(ticker) = self.ticker.take() {
			// wake the ticker so it sees stopped without waiting out the interval
			ticker.thread().unpark();
			let _ = ticker.join();
		}
	}
}

fn draw(state: &State, now: Instant, width: usize, height: usize) -> Vec<String> {
	let mut lines = vec![draw_header(&state.header(now))];
	lines.extend(state.rows().map(|row| draw_row(row, now, width)));
	// reserve one line so the final newline does not scroll the frame
	let budget = height.saturating_sub(1);
	if lines.len() > budget {
		// reserve one line for the hidden row count
		let visible = budget.saturating_sub(1);
		// exclude the header line
		let hidden = lines.len() - 1 - visible.saturating_sub(1);
		lines.truncate(visible);
		lines.push(format!("+{hidden} more"));
	}
	lines
}

struct State {
	labels: Vec<String>,
	rows: Vec<Option<Row>>,
	done: usize,
	started: Instant,
}

impl State {
	fn new(labels: Vec<String>, now: Instant) -> Self {
		let rows = vec![None; labels.len()];
		Self {
			labels,
			rows,
			done: 0,
			started: now,
		}
	}

	fn update(&mut self, event: ProgressEvent, now: Instant) {
		// guard against an out-of-range index
		let Some(slot) = self.rows.get_mut(event.index) else {
			return;
		};
		if event.done {
			// a repeated finish counts once
			if slot.take().is_some() {
				self.done += 1;
			}
			return;
		}
		let label = self.labels[event.index].clone();
		let row = slot.get_or_insert_with(|| Row {
			label,
			stage: event.stage,
			started: now,
		});
		// a stage change keeps the start time so elapsed covers the whole item
		row.stage = event.stage;
	}

	fn header(&self, now: Instant) -> Header {
		Header {
			done: self.done,
			total: self.labels.len(),
			elapsed: now.saturating_duration_since(self.started),
		}
	}

	fn rows(&self) -> impl Iterator<Item = &Row> {
		self.rows.iter().flatten()
	}
}

pub(crate) struct ProgressEvent {
	pub(crate) index: usize,
	pub(crate) stage: Stage,
	pub(crate) done: bool,
}

// 3/12  00:04
fn draw_header(header: &Header) -> String {
	format!("{}/{}  {}", header.done, header.total, _format_elapsed(header.elapsed))
}

struct Header {
	done: usize,
	total: usize,
	elapsed: Duration,
}

// owner/repo v1.2.3  resolving  00:03
//
// label, stage, elapsed, 18 fixed cells
fn draw_row(row: &Row, now: Instant, width: usize) -> String {
	let stage = match row.stage {
		Stage::Resolve => "resolving",
	};
	let elapsed = _format_elapsed(now.saturating_duration_since(row.started));
	let tail = format!("  {stage:<9}  {elapsed:>5}");
	let label_width = width.saturating_sub(tail.len());
	let label = _truncate(&row.label, label_width);
	format!("{label:<label_width$}{tail}")
}

#[derive(Clone)]
struct Row {
	label: String,
	stage: Stage,
	started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
	Resolve,
}

fn _format_elapsed(elapsed: Duration) -> String {
	let seconds = elapsed.as_secs().min(3599);
	format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod draw_tests {
	use std::time::{Duration, Instant};

	use super::*;

	#[test]
	fn draws_at_exact_width_boundaries() {
		let now = Instant::now();
		let row = Row {
			label: "github:owner/repo v1.2.3".to_owned(),
			stage: Stage::Resolve,
			started: now - Duration::from_secs(3),
		};
		// 24 label cells and 18 fixed cells fit exactly
		let cases = [(42, "github:owner/repo v1.2.3"), (41, "github:owner/repo v1.2…")];
		for (width, label) in cases {
			let line = draw_row(&row, now, width);
			assert_eq!(line, format!("{label}  resolving  00:03"), "width {width}");
			assert_eq!(line.chars().count(), width, "line width {width}");
		}
	}
}

#[cfg(test)]
mod state_tests {
	use std::time::{Duration, Instant};

	use super::*;

	fn _event(index: usize, done: bool) -> ProgressEvent {
		ProgressEvent {
			index,
			stage: Stage::Resolve,
			done,
		}
	}

	#[test]
	fn tracks_one_tool_from_start_to_finish() {
		let t0 = Instant::now();
		let mut state = State::new(vec!["github:owner/zero latest".to_owned(), "github:owner/one latest".to_owned()], t0);
		// create the row
		state.update(_event(0, false), t0);
		let row = state.rows().next().unwrap();
		assert_eq!(row.label, "github:owner/zero latest");
		assert_eq!(row.stage, Stage::Resolve);
		assert_eq!(row.started, t0);
		assert_eq!(state.header(t0).done, 0);
		// a repeated start keeps the start time
		state.update(_event(0, false), t0 + Duration::from_secs(3));
		assert_eq!(state.rows().count(), 1);
		assert_eq!(state.rows().next().unwrap().started, t0);
		// finishing removes the row and counts it
		state.update(_event(0, true), t0 + Duration::from_secs(4));
		assert!(state.rows().next().is_none());
		assert_eq!(state.header(t0).done, 1);
		// a repeated finish counts once
		state.update(_event(0, true), t0 + Duration::from_secs(4));
		assert_eq!(state.header(t0).done, 1);
		// a finish without a start is not counted
		state.update(_event(1, true), t0 + Duration::from_secs(4));
		assert_eq!(state.header(t0).done, 1);
		// an out-of-range index is ignored
		state.update(_event(5, false), t0 + Duration::from_secs(4));
		assert!(state.rows().next().is_none());
		// the header elapsed time runs from the state's creation
		let header = state.header(t0 + Duration::from_secs(5));
		assert_eq!(header.done, 1);
		assert_eq!(header.total, 2);
		assert_eq!(header.elapsed, Duration::from_secs(5));
	}
}
