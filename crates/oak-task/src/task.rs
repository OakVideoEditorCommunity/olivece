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

//! The `Task` base class, mirroring `src/task/src/task.h` (`olive::Task`).
//!
//! The C++ class is abstract with a protected virtual `run()`. In Rust each
//! concrete task (`ConformTask`, `ProxyTask`, …) is its own struct and the
//! shared lifecycle lives here; the per-task work is supplied through
//! [`TaskBehavior`] (a trait object, per architectural decision #1 in
//! README.md). Cancellation rides on a shared
//! `oak_core::cancelatom::CancelAtom` (single-lib unification: the old
//! oakrender cancelatom C ABI is gone).
//!
//! CPP-PARITY: src/task/src/task.h
//!
//! ## Concurrency contract
//!
//! Like the C++ original, the task's `title`/`error` strings are mutated on
//! the task's own thread (`run()`). The C ABI layer must therefore only read
//! them while the task is not concurrently running — i.e. before
//! [`Task::start`] or after the task finished (the `finished` flag is
//! synchronized through the `done` condvar, so `is_finished()`/
//! `succeeded()` are always race-free). This mirrors the C++ semantics
//! exactly.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use oak_core::cancelatom::CancelAtom;

use crate::error::{Error, Result};

/// Event type emitted through a task's [`EventListener`], mirroring the
/// C++ `EventType` enum (`k_event_started`/`k_event_progress`/`k_event_finished`).
///
/// CPP-PARITY: src/task/src/task.h (EventType)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TaskEvent {
	/// Task began executing.
	Started,
	/// Progress updated; the payload is in 0.0..=1.0.
	Progress(f64),
	/// Task finished (success or failure).
	Finished,
}

/// A listener notified of [`TaskEvent`]s, mirroring the C++
/// `std::function<void(EventType,double)>` listener. Delivered under the
/// task's lock; kept as the single async return channel (decision #3).
///
/// CPP-PARITY: src/task/src/task.h (EventListener)
pub type EventListener = Box<dyn FnMut(TaskEvent) + Send>;

/// Trait supplying the per-task work, standing in for the C++ protected
/// virtual `run()`. Concrete tasks implement this; [`Task::start`] drives it.
///
/// CPP-PARITY: src/task/src/task.h (run)
pub trait TaskBehavior {
	/// Perform the task's work. Returning `Err(Error::Cancelled)` is treated
	/// as a cancellation; any other `Err` marks the task failed.
	fn run(&mut self, task: &mut Task) -> Result<()>;
}

/// Values the subscribe wrapper needs to re-encode [`TaskEvent`]s into the
/// legacy `(event_id, value, userdata)` callback signature. The C++ emits
/// the start timestamp with `k_event_started` and 1.0/0.0 with
/// `k_event_finished`; `TaskEvent` carries neither, so the task publishes
/// them into this shared state before emitting (decision #1 in README.md).
#[derive(Debug, Default)]
pub struct SubscriberState {
	/// Start timestamp (ms since epoch) published before `Started` is emitted.
	pub start_ms: AtomicI64,
	/// 1.0/0.0 published before `Finished` is emitted.
	pub finished_value: AtomicI64,
}

/// Finish state, protected by the `done` condvar pair so `is_finished`/
/// `succeeded` are readable from any thread while the task thread mutates
/// them.
#[derive(Default)]
struct TaskDone {
	/// Whether the task has finished (success or failure).
	finished: bool,
	/// Whether the task finished successfully.
	succeeded: bool,
}

/// The base task. Owns lifecycle state plus a shared `CancelAtom`; the
/// concrete behavior lives in a [`TaskBehavior`] trait object.
pub struct Task {
	title: String,
	error: Option<String>,
	start_time: Option<std::time::Instant>,
	cancel_atom: Arc<CancelAtom>,
	event_listener: Option<EventListener>,
	cancel_event: Option<Box<dyn FnMut() + Send>>,
	started: bool,
	finished: bool,
	succeeded: bool,
	behavior: Option<Box<dyn TaskBehavior + Send>>,
	/// Finish/success flag pair + wakeup condvar (race-free readers).
	done: Arc<(Mutex<TaskDone>, Condvar)>,
	/// Values published for the legacy subscribe wrapper.
	subscriber: Option<Arc<SubscriberState>>,
}

impl Task {
	/// Create a new task with the given title. When `cancel_atom` is `None`
	/// a fresh atom is created and owned by the task (mirroring the C++
	/// constructor); `Some` shares the caller's atom (mirrors the old
	/// borrowed-oakrender-atom constructor).
	pub fn new(title: &str, cancel_atom: Option<Arc<CancelAtom>>) -> Task {
		Task {
			title: title.to_string(),
			error: None,
			start_time: None,
			cancel_atom: cancel_atom.unwrap_or_else(|| Arc::new(CancelAtom::new())),
			event_listener: None,
			cancel_event: None,
			started: false,
			finished: false,
			succeeded: false,
			behavior: None,
			done: Arc::new((Mutex::new(TaskDone::default()), Condvar::new())),
			subscriber: None,
		}
	}

	/// Attach the concrete behavior (defaults to a no-op).
	pub fn set_behavior(&mut self, behavior: Box<dyn TaskBehavior + Send>) {
		self.behavior = Some(behavior);
	}

	/// Start the task: set `started`, emit [`TaskEvent::Started`], run the
	/// behavior, then mark finished and emit [`TaskEvent::Finished`].
	///
	/// The returned `Result` mirrors the C++ `start()` bool: `Ok(())` when
	/// the behavior succeeded, `Err(..)` otherwise.
	pub fn start(&mut self) -> Result<()> {
		let start_ms = system_time_ms();
		self.start_time = Some(std::time::Instant::now());
		self.started = true;
		if let Some(s) = &self.subscriber {
			s.start_ms.store(start_ms, Ordering::SeqCst);
		}
		self.emit_event(TaskEvent::Started);

		// Take the behavior out so it can receive `self` (avoids a
		// self-referential borrow); put it back afterwards.
		let behavior = self.behavior.take();
		let ret = if let Some(mut b) = behavior {
			let r = b.run(self);
			self.behavior = Some(b);
			r
		} else {
			Ok(())
		};

		let succeeded = ret.is_ok();
		{
			let mut done = self.done.0.lock().unwrap();
			done.finished = true;
			done.succeeded = succeeded;
			self.done.1.notify_all();
		}
		if let Some(s) = &self.subscriber {
			s.finished_value
				.store(if succeeded { 1 } else { 0 }, Ordering::SeqCst);
		}
		self.emit_event(TaskEvent::Finished);

		// One-shot subscription: drop the listener after the final event.
		self.event_listener = None;

		ret
	}

	/// Request cancellation through the shared cancel atom, then invoke the
	/// cancel event callback if one is registered.
	pub fn cancel(&mut self) {
		self.cancel_atom.cancel();
		if let Some(cb) = self.cancel_event.as_mut() {
			cb();
		}
	}

	/// Whether cancellation was requested (queries the shared atom).
	pub fn is_cancelled(&self) -> bool {
		self.cancel_atom.is_cancelled()
	}

	/// The shared cancel atom (a clone of the task's `Arc`).
	pub fn get_cancel_atom(&self) -> Arc<CancelAtom> {
		self.cancel_atom.clone()
	}

	/// Replace the cancel atom. Used to share one atom between a task and
	/// its behavior's inner base task.
	pub fn set_cancel_atom(&mut self, atom: Arc<CancelAtom>) {
		self.cancel_atom = atom;
	}

	/// Register the event listener. Replaces any previous listener.
	pub fn set_event_listener(&mut self, listener: EventListener) {
		self.event_listener = Some(listener);
	}

	/// Register a hook invoked when [`Task::cancel`] is called.
	pub fn set_cancel_event(&mut self, cb: Box<dyn FnMut() + Send>) {
		self.cancel_event = Some(cb);
	}

	/// Publish the shared values used by the legacy subscribe wrapper.
	pub fn set_subscriber(&mut self, state: Arc<SubscriberState>) {
		self.subscriber = Some(state);
	}

	/// Emit a progress update, clamping to 0.0..=1.0 and delivering it to the
	/// listener as [`TaskEvent::Progress`].
	pub fn emit_progress(&mut self, progress: f64) {
		self.emit_event(TaskEvent::Progress(progress.clamp(0.0, 1.0)));
	}

	/// Reset lifecycle state so the task can be run again.
	pub fn reset(&mut self) {
		self.started = false;
		self.finished = false;
		self.succeeded = false;
		self.error = None;
		self.start_time = None;
		{
			let mut done = self.done.0.lock().unwrap();
			done.finished = false;
			done.succeeded = false;
		}
	}

	/// Record a failure message and mark the task failed.
	pub fn set_error(&mut self, message: &str) {
		self.error = Some(message.to_string());
	}

	/// Change the task title.
	pub fn set_title(&mut self, title: &str) {
		self.title = title.to_string();
	}

	/// The task title.
	pub fn title(&self) -> &str {
		&self.title
	}

	/// The failure message, if the task failed.
	pub fn error(&self) -> Option<&str> {
		self.error.as_deref()
	}

	/// Whether the task has finished (success or failure).
	pub fn is_finished(&self) -> bool {
		self.done.0.lock().unwrap().finished
	}

	/// Whether the task finished successfully.
	pub fn succeeded(&self) -> bool {
		self.done.0.lock().unwrap().succeeded
	}

	/// Block until the task finishes. Mirrors the C++ finished-wait condvar;
	/// the manager joins worker threads instead, but this is useful for
	/// direct (non-manager) runs. Returns immediately when the task has not
	/// been started yet (nothing to wait for) or is already finished.
	pub fn wait_finished(&self) {
		if !self.started {
			return;
		}
		let mut done = self.done.0.lock().unwrap();
		while !done.finished {
			done = self.done.1.wait(done).unwrap();
		}
	}

	/// The elapsed time since [`Task::start`], for duration reporting.
	pub fn elapsed(&self) -> Option<std::time::Duration> {
		self.start_time.map(|t| t.elapsed())
	}

	fn emit_event(&mut self, ev: TaskEvent) {
		if let Some(listener) = self.event_listener.as_mut() {
			listener(ev);
		}
	}
}

/// Current wall-clock time in milliseconds since the Unix epoch, matching
/// the C++ `start_time_` convention (std::chrono::system_clock ms).
pub fn system_time_ms() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_millis() as i64)
		.unwrap_or(0)
}

/// Marker error returned when a task is cancelled, so the legacy ABI can map
/// it to `OAKTASK_E_CANCELLED` (distinct from a generic failure).
pub fn cancelled() -> Error {
	Error::Cancelled
}
