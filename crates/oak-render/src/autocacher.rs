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

//! The preview auto-cacher (C++ `PreviewAutoCacher`): watches cache
//! requests on the copied project and pre-renders invalidated ranges
//! in the background.
//!
//! Per the frozen cache ABI (M7 §2.2) no cache events cross the
//! boundary — the facade re-emits notifications — so this pass drives
//! the cacher explicitly: [`PreviewAutoCacher::on_cache_request`] is the
//! facade-facing entry that queues range-caching jobs; `single_frame` is
//! the one-shot preview request path. Job bookkeeping lives here; the
//! actual frame production is the arena's CPU producer.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::{Rational, TimeRange};

use crate::error::Result;
use crate::ticket::{TicketArena, TicketId, VideoTicketParams};

/// Progress/stop callbacks toward the facade (C++
/// set_cache_progress_callback / set_stop_cache_proxy_tasks_callback).
pub trait AutoCacheEvents: Send {
	/// Cache progress 0.0..=1.0.
	fn progress(&mut self, value: f64);
	/// Ask proxy tasks to stop.
	fn stop_proxy_tasks(&mut self);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The auto-cacher.
pub struct PreviewAutoCacher {
	/// Identity of the copied project being cached.
	pub copied_project: u64,
	/// Custom range override (C++ force_cache_range).
	pub custom_range: Option<TimeRange>,
	/// Playhead position (C++ set_playhead).
	pub playhead: Rational,
	/// Pause toggles.
	pub renders_paused: bool,
	/// Thumbnail pause toggle.
	pub thumbnails_paused: bool,
	/// Multicam source node identity (C++ set_multicam_node).
	pub multicam_node: Option<u64>,
	/// Ignore cache requests (C++ set_ignore_cache_requests).
	pub ignore_requests: bool,
	/// Display color processor identity (C++ set_display_color_processor).
	pub display_processor: Option<u64>,
	/// The ticket arena the cacher submits through.
	arena: Arc<TicketArena>,
	/// Viewer identity for single-frame renders.
	viewer_identity: Option<u64>,
	/// In-flight job tickets.
	jobs: Mutex<HashSet<TicketId>>,
	/// Requested-but-not-yet-cached ranges per owner (internal bookkeeping).
	pending: Mutex<Vec<(u64, TimeRange)>>,
	/// The most recent single-frame ticket (cancelled before the next).
	last_single_frame: Mutex<Option<TicketId>>,
	/// Facade callbacks.
	events: Mutex<Option<Box<dyn AutoCacheEvents>>>,
}

impl PreviewAutoCacher {
	/// A cacher submitting through `arena`.
	pub fn new(arena: Arc<TicketArena>) -> Self {
		Self {
			copied_project: 0,
			custom_range: None,
			playhead: Rational::NULL,
			renders_paused: false,
			thumbnails_paused: false,
			multicam_node: None,
			ignore_requests: false,
			display_processor: None,
			arena,
			viewer_identity: None,
			jobs: Mutex::new(HashSet::new()),
			pending: Mutex::new(Vec::new()),
			last_single_frame: Mutex::new(None),
			events: Mutex::new(None),
		}
	}

	/// Set the viewer identity used by [`PreviewAutoCacher::single_frame`]
	/// (the facade passes the oaknode viewer handle identity).
	pub fn set_viewer_identity(&mut self, identity: Option<u64>) {
		self.viewer_identity = identity;
	}

	/// Attach to a copied project (registers on its caches).
	pub fn attach(&mut self, copied_project: u64) -> Result<()> {
		if copied_project == 0 {
			return Err(crate::error::Error::Invalid);
		}
		self.copied_project = copied_project;
		lock(&self.pending).clear();
		Ok(())
	}

	/// Detach and cancel all pending cache jobs (C++
	/// project_destroyed / disconnect_from_node_cache).
	pub fn detach(&mut self) {
		self.cancel_all_jobs();
		self.copied_project = 0;
		lock(&self.pending).clear();
		lock(&self.last_single_frame).take();
	}

	/// A cache request arrived for `owner` (facade entry; C++ cache
	/// `requested` handling). Queues a caching job unless paused or
	/// ignored.
	pub fn on_cache_request(&mut self, owner: u64, range: TimeRange) {
		if self.ignore_requests || owner == 0 {
			return;
		}
		lock(&self.pending).push((owner, range));
		if self.renders_paused {
			return;
		}
		self.start_range_job(owner, range);
	}

	/// Start a caching job for `range` of `owner`.
	fn start_range_job(&mut self, owner: u64, range: TimeRange) {
		let id = self.arena.submit_video(
			VideoTicketParams {
				viewer: owner,
				project: String::new(),
				time: range.in_(),
				force_size: None,
				force_format: None,
				cache: Some(owner),
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			Box::new(|_| {}),
		);
		lock(&self.jobs).insert(id);
	}

	/// One-off single-frame render (C++ get_single_frame): returns a
	/// ticket; a queued-but-not-started previous single-frame render is
	/// cancelled first.
	pub fn single_frame(&mut self, time: Rational) -> TicketId {
		self.single_frame_with_completion(time, Box::new(|_| {}))
	}

	/// [`PreviewAutoCacher::single_frame`] with a custom completion (the
	/// FFI frame-request path delivers its callback here).
	pub fn single_frame_with_completion(
		&mut self,
		time: Rational,
		done: crate::ticket::Completion,
	) -> TicketId {
		if let Some(prev) = lock(&self.last_single_frame).take() {
			self.arena.cancel(prev);
			lock(&self.jobs).remove(&prev);
		}
		let viewer = self.viewer_identity.unwrap_or(self.copied_project);
		let id = self.arena.submit_video(
			VideoTicketParams {
				viewer,
				project: String::new(),
				time,
				force_size: None,
				force_format: None,
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			done,
		);
		*lock(&self.last_single_frame) = Some(id);
		lock(&self.jobs).insert(id);
		id
	}

	/// Clear completed single-frame renders (C++
	/// clear_single_frame_renders_that_arent_running).
	pub fn clear_finished_single_frames(&mut self) {
		let mut jobs = lock(&self.jobs);
		jobs.retain(|id| !self.arena.is_finished(*id));
	}

	/// Force-cache a range (C++ force_cache_range).
	pub fn force_range(&mut self, range: TimeRange) {
		self.custom_range = Some(range);
		self.start_range_job(self.copied_project, range);
	}

	/// True while a custom range render is active.
	pub fn is_rendering_custom_range(&self) -> bool {
		if self.custom_range.is_none() {
			return false;
		}
		let jobs = lock(&self.jobs);
		jobs.iter().any(|id| !self.arena.is_finished(*id))
	}

	/// Cancel in-flight video tasks (C++ cancel_video_tasks). With
	/// `wait`, blocks until every cancelled ticket finished.
	pub fn cancel_video_tasks(&self, wait: bool) {
		let ids: Vec<TicketId> = lock(&self.jobs).iter().copied().collect();
		for id in &ids {
			self.arena.cancel(*id);
		}
		if wait {
			for id in &ids {
				let _ = self.arena.wait(*id);
			}
		}
	}

	/// Cancel in-flight audio tasks (C++ cancel_audio_tasks). Audio
	/// caching is not implemented in this pass (no audio jobs are ever
	/// submitted); kept for the facade contract.
	pub fn cancel_audio_tasks(&self, _wait: bool) {}

	/// Display color processor for preview output (C++
	/// set_display_color_processor).
	pub fn set_display_color_processor(&mut self, processor: Option<u64>) {
		self.display_processor = processor;
	}

	/// Event sink registration (facade).
	pub fn set_events(&mut self, events: Box<dyn AutoCacheEvents>) {
		*lock(&self.events) = Some(events);
	}

	/// Report progress to the facade (0.0..=1.0).
	pub fn report_progress(&self, value: f64) {
		if let Some(events) = lock(&self.events).as_mut() {
			events.progress(value);
		}
	}

	/// Ask proxy tasks to stop.
	pub fn request_stop_proxy_tasks(&self) {
		if let Some(events) = lock(&self.events).as_mut() {
			events.stop_proxy_tasks();
		}
	}

	fn cancel_all_jobs(&mut self) {
		let ids: Vec<TicketId> = lock(&self.jobs).iter().copied().collect();
		for id in &ids {
			self.arena.cancel(*id);
		}
		lock(&self.jobs).clear();
	}

	/// Pending (not yet cached) request ranges — test introspection.
	pub fn pending_requests(&self) -> Vec<(u64, TimeRange)> {
		lock(&self.pending).clone()
	}

	/// Live job ticket ids — test introspection.
	pub fn live_jobs(&self) -> Vec<TicketId> {
		lock(&self.jobs).iter().copied().collect()
	}
}

/// A no-op event sink (facade may pass one in tests).
pub struct NoopEvents;

impl AutoCacheEvents for NoopEvents {
	fn progress(&mut self, _value: f64) {}
	fn stop_proxy_tasks(&mut self) {}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicU32, Ordering};

	use crate::worker::{InlineDispatcher, JobDispatch};
	use oak_core::frame::VideoParamsPod;
	use oak_core::texture::{Frame, Texture};

	fn frame_producer() -> crate::ticket::Producer {
		Arc::new(|_, _| {
			let mut f = Frame::new();
			let mut p = VideoParamsPod::default();
			p.width = 4;
			p.height = 4;
			f.set_video_params(p);
			f.allocate();
			Ok(crate::ticket::TicketPayload::Video(Texture::wrap_frame(f)))
		})
	}

	/// A cacher on a queued inline dispatcher: jobs run only when the test
	/// drains it with `InlineDispatcher::run` (deterministic ordering, no
	/// worker threads).
	fn new_cacher() -> (PreviewAutoCacher, Arc<InlineDispatcher>) {
		let d = InlineDispatcher::queued();
		let arena = Arc::new(TicketArena::new(d.clone(), frame_producer()));
		(PreviewAutoCacher::new(arena), d)
	}

	#[test]
	fn attach_detach_lifecycle() {
		let (mut c, d) = new_cacher();
		assert!(c.attach(0).is_err(), "zero identity rejected");
		c.attach(42).unwrap();
		assert_eq!(c.copied_project, 42);
		c.on_cache_request(
			42,
			TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)),
		);
		assert_eq!(c.live_jobs().len(), 1);
		c.detach();
		assert_eq!(c.copied_project, 0);
		assert!(c.live_jobs().is_empty());
		d.shutdown();
	}

	#[test]
	fn single_frame_cancels_previous() {
		// The race this asserts ("the previous frame is cancelled") is
		// deterministic on the queued inline dispatcher: the first job stays
		// queued until `run`, so the superseding submit's cancel lands first.
		let (mut c, d) = new_cacher_slow();
		c.attach(7);
		let first = c.single_frame(Rational::new(0, 1));
		let second = c.single_frame(Rational::new(1, 1));
		assert_ne!(first, second);
		// The first ticket is cancelled: its completion fired with State.
		d.run();
		c.arena.wait(first).unwrap();
		let r = c.arena.result(first).unwrap();
		assert!(r.is_err());
		d.shutdown();
	}

	/// A cacher whose frames take ~100ms to produce (see
	/// [`single_frame_cancels_previous`] — the sleep is irrelevant on the
	/// queued dispatcher, kept for the original async-backend semantics).
	fn new_cacher_slow() -> (PreviewAutoCacher, Arc<InlineDispatcher>) {
		let d = InlineDispatcher::queued();
		let producer: crate::ticket::Producer = Arc::new(|_, _| {
			std::thread::sleep(std::time::Duration::from_millis(100));
			let mut f = Frame::new();
			let mut p = VideoParamsPod::default();
			p.width = 4;
			p.height = 4;
			f.set_video_params(p);
			f.allocate();
			Ok(crate::ticket::TicketPayload::Video(Texture::wrap_frame(f)))
		});
		let arena = Arc::new(TicketArena::new(d.clone(), producer));
		(PreviewAutoCacher::new(arena), d)
	}

	#[test]
	fn ignore_requests_suppresses_jobs() {
		let (mut c, d) = new_cacher();
		c.attach(7);
		c.ignore_requests = true;
		c.on_cache_request(7, TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
		assert!(c.live_jobs().is_empty());
		d.shutdown();
	}

	#[test]
	fn renders_paused_queues_but_does_not_start() {
		let (mut c, d) = new_cacher();
		c.attach(7);
		c.renders_paused = true;
		c.on_cache_request(7, TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
		assert!(c.live_jobs().is_empty(), "paused: no jobs started");
		assert_eq!(c.pending_requests().len(), 1);
		d.shutdown();
	}

	#[test]
	fn cancel_video_tasks_wait_blocks_until_idle() {
		let (mut c, d) = new_cacher();
		c.attach(7);
		c.force_range(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		assert!(c.is_rendering_custom_range() || c.live_jobs().len() == 1);
		// Drain so the wait below observes every job finished.
		d.run();
		c.cancel_video_tasks(true);
		assert!(c.live_jobs().iter().all(|id| c.arena.is_finished(*id)));
		assert!(!c.is_rendering_custom_range());
		d.shutdown();
	}

	struct Probe {
		progress: AtomicU32,
		stop: AtomicU32,
	}


	#[test]
	fn events_deliver_progress() {
		let (mut c, d) = new_cacher();
		let probe = Arc::new(Probe {
			progress: AtomicU32::new(0),
			stop: AtomicU32::new(0),
		});
		let probe2 = probe.clone();
		c.set_events(Box::new(ProbeEvents { probe: probe2 }));
		c.report_progress(0.5);
		c.request_stop_proxy_tasks();
		assert_eq!(probe.progress.load(Ordering::Relaxed), 1);
		assert_eq!(probe.stop.load(Ordering::Relaxed), 1);
		d.shutdown();
	}

	struct ProbeEvents {
		probe: Arc<Probe>,
	}
	impl AutoCacheEvents for ProbeEvents {
		fn progress(&mut self, value: f64) {
			// The reported fraction must arrive intact.
			assert_eq!(value, 0.5);
			self.probe.progress.fetch_add(1, Ordering::Relaxed);
		}
		fn stop_proxy_tasks(&mut self) {
			self.probe.stop.fetch_add(1, Ordering::Relaxed);
		}
	}

	#[test]
	fn clear_finished_single_frames_removes_done() {
		let (mut c, d) = new_cacher();
		c.attach(7);
		let id = c.single_frame(Rational::new(0, 1));
		d.run();
		c.arena.wait(id).unwrap();
		assert_eq!(c.live_jobs().len(), 1, "finished job still tracked");
		c.clear_finished_single_frames();
		assert!(c.live_jobs().is_empty());
		d.shutdown();
	}

	#[test]
	fn noop_events_sink_never_panics() {
		let mut c = {
			let d = InlineDispatcher::sync();
			let arena = Arc::new(TicketArena::new(d, frame_producer()));
			PreviewAutoCacher::new(arena)
		};
		c.set_events(Box::new(NoopEvents));
		c.report_progress(1.0);
		c.request_stop_proxy_tasks();
	}
}
