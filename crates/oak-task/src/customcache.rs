// Oak Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Oak Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! `CustomCacheTask`, mirroring `src/task/src/customcache/customcachetask.h`.
//!
//! A task that delegates its completion to an external cache fill: it does
//! not do work itself but waits until the cache reports it is done (via
//! [`CustomCacheTask::finish`]) or is cancelled.
//!
//! CPP-PARITY: src/task/src/customcache/customcachetask.h

use std::sync::{Arc, Condvar, Mutex};

use oak_core::cancelatom::CancelAtom;

use crate::error::{Error, Result};
use crate::task::{Task, TaskBehavior};

/// Shared state between the parked task thread, the owning cache
/// ([`CustomCacheTask::finish`]) and the cancel hook. Mirrors the C++
/// `mutex_`/`wait_cond_`/`cancelled_through_finish_` trio; the cancel atom
/// copy lets `finish()` wake the task through the same channel the C++
/// `cancel()` uses.
pub struct CustomCacheState {
	/// Set by `finish()` before the cancellation request.
	cancelled_through_finish: Mutex<bool>,
	/// Parked-task wakeup.
	wait: Condvar,
	/// Callback invoked when the task is cancelled (not by `finish()`).
	cancelled_callback: Mutex<Option<Box<dyn FnMut() + Send>>>,
	/// Copy of the task's cancellation atom.
	atom: Arc<CancelAtom>,
}

impl CustomCacheState {
	/// Mark the cache fill as complete and wake the parked task.
	pub fn finish(&self) {
		*self.cancelled_through_finish.lock().unwrap() = true;
		self.atom.cancel();
		self.wait.notify_one();
	}

	/// The C++ `cancel_event()` hook: fires the callback unless the task was
	/// finished through `finish()`, then wakes the parked task.
	pub fn cancel_event(&self) {
		if !*self.cancelled_through_finish.lock().unwrap() {
			if let Some(cb) = self.cancelled_callback.lock().unwrap().as_mut() {
				cb();
			}
		}
		self.wait.notify_one();
	}

	/// The full C++ `cancel()`: cancel the atom and run the cancel-event
	/// hook. Used by the owning cache to abort a fill.
	pub fn cancel(&self) {
		self.atom.cancel();
		self.cancel_event();
	}

	/// Whether the fill was completed through `finish()`.
	fn is_finished_through_finish(&self) -> bool {
		*self.cancelled_through_finish.lock().unwrap()
	}
}

/// A cache-fill task that finishes only when [`CustomCacheTask::finish`] is
/// called by the owning cache. Uses a `Mutex` + `Condvar` to park the task
/// thread until completion or cancellation.
pub struct CustomCacheTask {
	/// The shared task base.
	pub base: Task,
	/// The name of the sequence being cached.
	pub sequence_name: String,
	/// Shared park/notify state.
	state: Arc<CustomCacheState>,
}

impl CustomCacheTask {
	/// Create a cache-fill task with the given sequence name. The cancel
	/// hook that wakes the parked thread is installed on the base task.
	pub fn new(sequence_name: &str) -> CustomCacheTask {
		let base = Task::new(
			&format!("Caching custom range for \"{sequence_name}\""),
			None,
		);
		let state = Arc::new(CustomCacheState {
			cancelled_through_finish: Mutex::new(false),
			wait: Condvar::new(),
			cancelled_callback: Mutex::new(None),
			atom: base.get_cancel_atom(),
		});
		let mut task = CustomCacheTask {
			base,
			sequence_name: sequence_name.to_string(),
			state,
		};
		let hook_state = task.state.clone();
		task.base
			.set_cancel_event(Box::new(move || hook_state.cancel_event()));
		task
	}

	/// Mark the cache fill as complete, waking the parked task thread.
	pub fn finish(&mut self) {
		self.state.finish();
	}

	/// Install the callback invoked when the task is cancelled.
	pub fn set_cancelled_callback(&mut self, cb: Box<dyn FnMut() + Send>) {
		*self.state.cancelled_callback.lock().unwrap() = Some(cb);
	}

	/// Shared-state accessor for the owning cache (and tests) to call
	/// [`CustomCacheTask::finish`] from another thread.
	pub fn state(&self) -> Arc<CustomCacheState> {
		self.state.clone()
	}
}

impl TaskBehavior for CustomCacheTask {
	/// Park until [`CustomCacheTask::finish`] or cancellation; return
	/// `Err(Error::Cancelled)` if cancelled while waiting.
	fn run(&mut self, task: &mut Task) -> Result<()> {
		// Install the cancel hook on the task this behavior actually runs
		// under (the outer task driven by the manager). The
		// constructor-installed hook on the inner `base` only fires when the
		// base itself is cancelled; without this the outer `cancel()` would
		// set the shared atom but never wake the parked thread.
		let hook_state = self.state.clone();
		task.set_cancel_event(Box::new(move || hook_state.cancel_event()));

		let mut guard = self.state.cancelled_through_finish.lock().unwrap();
		while !task.is_cancelled() {
			guard = self.state.wait.wait(guard).unwrap();
		}
		drop(guard);
		if self.state.is_finished_through_finish() {
			Ok(())
		} else {
			Err(Error::Cancelled)
		}
	}
}
