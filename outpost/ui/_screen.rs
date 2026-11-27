#[cfg(test)]
use std::sync::{Arc, Mutex};

use console::Term;

// terminal operations the progress views draw with
pub(crate) trait Screen: Send + Sync {
	// (rows, columns)
	fn size(&self) -> (u16, u16);
	fn clear_last_lines(&self, n: usize);
	fn write_line(&self, line: &str);
	fn flush(&self);
}

// write errors are ignored because progress output is best effort
impl Screen for Term {
	fn size(&self) -> (u16, u16) {
		Term::size(self)
	}

	fn clear_last_lines(&self, n: usize) {
		let _ = Term::clear_last_lines(self, n);
	}

	fn write_line(&self, line: &str) {
		let _ = Term::write_line(self, line);
	}

	fn flush(&self) {
		let _ = Term::flush(self);
	}
}

// clones share state so the view writes through one and the test reads through another
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FakeScreen {
	size: (u16, u16),
	state: Arc<Mutex<FakeState>>,
}

#[cfg(test)]
#[derive(Default)]
struct FakeState {
	lines: Vec<String>,
	frames: Vec<Vec<String>>,
}

#[cfg(test)]
impl FakeScreen {
	pub(crate) fn new(rows: u16, columns: u16) -> Self {
		Self {
			size: (rows, columns),
			state: Default::default(),
		}
	}

	// lines currently visible
	pub(crate) fn lines(&self) -> Vec<String> {
		self.state.lock().unwrap().lines.clone()
	}

	// visible lines at each flush
	pub(crate) fn frames(&self) -> Vec<Vec<String>> {
		self.state.lock().unwrap().frames.clone()
	}
}

#[cfg(test)]
impl Screen for FakeScreen {
	fn size(&self) -> (u16, u16) {
		self.size
	}

	// clearing more lines than were written is a caller bug
	fn clear_last_lines(&self, n: usize) {
		let mut state = self.state.lock().unwrap();
		let visible = state.lines.len();
		assert!(n <= visible, "cleared {n} lines but {visible} are visible");
		state.lines.truncate(visible - n);
	}

	fn write_line(&self, line: &str) {
		self.state.lock().unwrap().lines.push(line.to_owned());
	}

	fn flush(&self) {
		let mut state = self.state.lock().unwrap();
		let frame = state.lines.clone();
		state.frames.push(frame);
	}
}

#[cfg(test)]
mod fake_tests {
	use super::*;

	#[test]
	fn clears_and_records_frames() {
		let screen = FakeScreen::new(24, 80);
		let view = screen.clone();
		screen.write_line("one");
		screen.write_line("two");
		screen.flush();
		// clearing removes the most recent lines
		screen.clear_last_lines(1);
		screen.write_line("three");
		screen.flush();
		assert_eq!(view.size(), (24, 80));
		assert_eq!(view.lines(), ["one", "three"]);
		assert_eq!(view.frames(), [vec!["one", "two"], vec!["one", "three"]]);
	}

	#[test]
	#[should_panic(expected = "cleared 2 lines but 1 are visible")]
	fn rejects_clearing_unwritten_lines() {
		let screen = FakeScreen::new(24, 80);
		screen.write_line("one");
		screen.clear_last_lines(2);
	}
}
