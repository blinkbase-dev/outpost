use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use console::Term;

use super::_screen::Screen;
use super::progress::_truncate;

// resolve fires only start and finish events, so the ticker redraws to advance elapsed times
const TICK_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) struct ProgressList<S: Screen = Term> {
	screen: Option<S>,
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
		let screen = std::io::stderr().is_terminal().then(Term::stderr);
		Self::with_screen(labels, screen, TICK_INTERVAL)
	}
}

impl<S: Screen + Clone + 'static> ProgressList<S> {
	// skip the ticker and events without a screen
	fn with_screen(labels: Vec<String>, screen: Option<S>, tick_interval: Duration) -> Self {
		let now = Instant::now();
		let inner = Arc::new(Mutex::new(Inner {
			state: State::new(labels, now),
			drawn_lines: 0,
			stopped: false,
		}));
		let ticker = screen.clone().map(|screen| {
			let inner = Arc::clone(&inner);
			thread::spawn(move || Self::_tick(&screen, &inner, tick_interval))
		});
		Self { screen, inner, ticker }
	}
}

impl<S: Screen> ProgressList<S> {
	pub(crate) fn record(&self, event: ProgressEvent) {
		if self.screen.is_none() {
			return;
		}
		self.inner.lock().unwrap().state.update(event, Instant::now());
		// wake the ticker so the change draws now
		if let Some(ticker) = &self.ticker {
			ticker.thread().unpark();
		}
	}

	fn _tick(screen: &S, inner: &Mutex<Inner>, tick_interval: Duration) {
		loop {
			// unparks from record merge into one pending wake so bursts draw once
			thread::park_timeout(tick_interval);
			let Ok(mut inner) = inner.lock() else {
				return;
			};
			// stop once drop has cleared the lines
			if inner.stopped {
				return;
			}
			let (height, columns) = screen.size();
			let lines = draw(&inner.state, Instant::now(), columns as usize, height as usize);
			inner.drawn_lines = Self::_write(screen, &lines, inner.drawn_lines);
		}
	}

	fn _write(screen: &S, lines: &[String], drawn_lines: usize) -> usize {
		screen.clear_last_lines(drawn_lines);
		for line in lines {
			screen.write_line(line);
		}
		screen.flush();
		lines.len()
	}
}

impl<S: Screen> Drop for ProgressList<S> {
	fn drop(&mut self) {
		let Some(screen) = &self.screen else {
			return;
		};
		if let Ok(mut inner) = self.inner.lock() {
			inner.stopped = true;
			screen.clear_last_lines(inner.drawn_lines);
			screen.flush();
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
mod screen_tests {
	use std::thread;
	use std::time::{Duration, Instant};

	use super::*;
	use crate::ui::_screen::FakeScreen;

	fn _event(index: usize, done: bool) -> ProgressEvent {
		ProgressEvent {
			index,
			stage: Stage::Resolve,
			done,
		}
	}

	// the ticker draws on its own schedule, so poll until the screen matches or a second passes
	fn _wait_for(screen: &FakeScreen, expected: impl Fn(&[String]) -> bool) {
		let deadline = Instant::now() + Duration::from_secs(1);
		while !expected(&screen.lines()) {
			assert!(Instant::now() < deadline, "screen never matched: {:?}", screen.lines());
			thread::sleep(Duration::from_millis(1));
		}
	}

	#[test]
	fn redraws_each_tick_in_place() {
		let screen = FakeScreen::new(24, 80);
		let labels = vec!["github:owner/zero latest".to_owned(), "github:owner/one latest".to_owned()];
		let list = ProgressList::with_screen(labels, Some(screen.clone()), Duration::from_millis(1));
		// start both tools
		list.record(_event(0, false));
		list.record(_event(1, false));
		_wait_for(&screen, |lines| {
			lines.len() == 3 && lines[0].starts_with("0/2") && lines[1].starts_with("github:owner/zero latest") && lines[2].starts_with("github:owner/one latest")
		});
		// finishing tool 0 shrinks the frame so a stale row means the clear count was wrong
		list.record(_event(0, true));
		_wait_for(&screen, |lines| {
			lines.len() == 2 && lines[0].starts_with("1/2") && lines[1].starts_with("github:owner/one latest")
		});
	}

	#[test]
	fn wakes_ticker_and_clears_screen_on_drop() {
		let screen = FakeScreen::new(24, 80);
		let labels = vec!["github:owner/zero latest".to_owned()];
		let list = ProgressList::with_screen(labels, Some(screen.clone()), Duration::from_secs(60));
		// record wakes the ticker so the row draws without waiting out the interval
		list.record(_event(0, false));
		_wait_for(&screen, |lines| {
			lines.len() == 2 && lines[0].starts_with("0/1") && lines[1].starts_with("github:owner/zero latest")
		});
		let started = Instant::now();
		drop(list);
		// drop unparks the ticker so joining it skips the tick interval
		assert!(started.elapsed() < Duration::from_secs(1), "drop took {:?}", started.elapsed());
		assert!(screen.lines().is_empty(), "screen not cleared: {:?}", screen.lines());
		// drop flushes the cleared screen last, so the ticker drew nothing after it
		assert_eq!(screen.frames().last(), Some(&Vec::new()));
	}
}

#[cfg(test)]
mod draw_tests {
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

	#[test]
	fn draws_capped_rows_and_hidden_count() {
		let t0 = Instant::now();
		let mut state = State::new((0..5).map(|index| format!("github:owner/{index} v1")).collect(), t0);
		// start all five tools
		for index in 0..5 {
			state.update(_event(index, false), t0);
		}
		// a height of five draws four lines: the header, two rows, and the count
		let lines = draw(&state, t0, 80, 5);
		assert_eq!(lines.len(), 4);
		assert_eq!(lines[0], "0/5  00:00");
		assert!(lines[1].starts_with("github:owner/0 v1"));
		assert!(lines[2].starts_with("github:owner/1 v1"));
		assert_eq!(lines[3], "+3 more");
		// three lines fit only the header and the hidden row count
		assert_eq!(draw(&state, t0, 80, 3), ["0/5  00:00", "+5 more"]);
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
