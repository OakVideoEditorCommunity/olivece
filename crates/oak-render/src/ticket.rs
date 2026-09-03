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

//! Render tickets: async render requests with completion delivery
//! (C++ `RenderTicket`/`RenderTicketWatcher`, Qt signals replaced by
//! boxed callbacks on a delivery thread).
//!
//! Exactly-once contract: a ticket's completion fires exactly once — on
//! success with the result, on cancel (or pool shutdown) with
//! `Error::State`. Cancellation never races the delivery: `cancel` only
//! sets a flag that `finish` honors, and `finish` is the single delivery
//! point.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use oak_core::{Rational, TimeRange};

use crate::error::{Error, Result};
use crate::eval;
use crate::worker::{JobDispatch, JobSchedule};
use oak_core::texture::Texture;

/// One effect of a montage clip's effect stack (M14 R3 effect-chain
/// wiring into the montage render path). The stack is ordered
/// source-first (the chain walk's signal order: the first element feeds
/// the media side, the last feeds the clip's effect input); the renderer
/// applies them in sequence after decode, before compositing.
#[derive(Clone, Debug)]
pub struct MontageEffect {
	/// Node type id: the built-in factory type id (e.g.
	/// `org.olivevideoeditor.Olive.opacity`) or the OFX plugin identifier.
	pub type_id: String,
	/// The effect's enabled flag. Disabled effects are bypassed (the C++
	/// traverser's bypass pushes the effect input through unchanged), so
	/// the renderer skips them; they are carried anyway so the render side
	/// can log what it skipped.
	pub enabled: bool,
	/// The clip/input name the source texture arrives on (the node's
	/// effect input id — "tex_in" for built-ins, "Source" for typical OFX
	/// filters; `None` when the node has no effect input).
	pub effect_input_id: Option<String>,
	/// Parameter values (input id -> value): the node's non-hidden,
	/// non-connection data inputs at their standard (non-keyframed)
	/// values — the same parameter set the inspector exposes.
	pub params: Vec<(String, oak_node::value::NodeValue)>,
}

/// One clip of a sequence montage (M12 P0): the facade resolves the
/// timeline into an ordered list of clips; the producer decodes each and
/// composites them topmost-last.
#[derive(Clone, Debug)]
pub struct MontageClip {
	/// Footage filename.
	pub filename: String,
	/// Media stream index.
	pub stream_index: i32,
	/// Clip in point (sequence time).
	pub in_time: Rational,
	/// Clip out point (sequence time).
	pub out_time: Rational,
	/// Media in point.
	pub media_in: Rational,
	/// Playback gain (1.0 = unity).
	pub gain: f32,
	/// The clip's effect stack (source-first; empty for audio clips and
	/// for montage builders that do not resolve effect chains).
	pub effects: Vec<MontageEffect>,
}

/// Audio ticket parameters (M12 P1): the output format plus the audio
/// montage to mix over the requested range.
#[derive(Clone, Debug)]
pub struct AudioTicketParams {
	/// Node graph context (copied project identity).
	pub viewer: u64,
	/// The range to render (sequence time).
	pub range: TimeRange,
	/// Output sample rate (Hz).
	pub sample_rate: i32,
	/// Output channel layout mask.
	pub channel_layout: u64,
	/// Clips to mix (ordered arbitrarily; gains applied, silence
	/// elsewhere).
	pub montage: Vec<MontageClip>,
}

/// Ticket parameters (Rust view of `oakrender_video_ticket_params`).
#[derive(Clone, Debug)]
pub struct VideoTicketParams {
	/// Node graph context (copied project identity).
	pub viewer: u64,
	/// The owning project's uuid (M16 S1): the worker renders the viewer's
	/// graph frame from the loaded snapshot ONLY when this matches the
	/// snapshot's project — empty means "no graph mode" (montage/footage/
	/// generated frames never match a loaded graph).
	pub project: String,
	/// Frame time.
	pub time: Rational,
	/// Forced size override (None = sequence size).
	pub force_size: Option<(i32, i32)>,
	/// Forced pixel format (None = pipeline default F32).
	pub force_format: Option<oak_core::PixelFormat>,
	/// Frame cache to record into (cache identity).
	pub cache: Option<u64>,
	/// Cache directory (marshalled from the cache handle; frame-cache
	/// write path).
	pub cache_dir: Option<String>,
	/// Cache uuid.
	pub cache_id: Option<String>,
	/// Cache timebase.
	pub cache_timebase: Option<oak_core::Rational>,
	/// Single-footage decode (footage node render; M12 P0).
	pub footage: Option<(String, i32)>,
	/// Sequence montage (ordered topmost-last; M12 P0). When set, the
	/// footage field is ignored.
	pub montage: Vec<MontageClip>,
}

impl VideoTicketParams {
	/// The render size: force_size when set, else the pipeline default.
	pub fn render_size(&self) -> (i32, i32) {
		self.force_size.unwrap_or((
			oak_core::frame::VideoParamsPod::DEFAULT_WIDTH,
			oak_core::frame::VideoParamsPod::DEFAULT_HEIGHT,
		))
	}
}

/// Audio samples produced by an audio ticket (M12 P1).
#[derive(Clone, Debug)]
pub struct AudioSamples {
	/// Interleaved f32 samples.
	pub samples: Vec<f32>,
	/// Sample rate (Hz).
	pub sample_rate: i32,
	/// Channel layout mask.
	pub channel_layout: u64,
	/// Channel count.
	pub channel_count: i32,
}

/// The ticket completion payload: video frames or audio samples.
#[derive(Clone, Debug)]
pub enum TicketPayload {
	/// A rendered video texture.
	Video(Texture),
	/// Rendered interleaved audio.
	Audio(AudioSamples),
	/// A rendered frame living in a worker's shared-memory slot (M15
	/// process backend): zero copy — the consumer reads the pixels from
	/// the mapping and releases the slot through the dispatcher.
	ShmFrame(crate::procpool::ShmFrameRef),
	/// Rendered audio living in a worker's shared-memory slot (M15 S3):
	/// interleaved f32 in `SLOT_FORMAT_AUDIO_F32` slots, consumed with
	/// [`crate::procpool::ShmAudioRef::samples`] and released through the
	/// dispatcher — the audio counterpart of `ShmFrame`.
	ShmAudio(crate::procpool::ShmAudioRef),
}

/// Completion payload: the rendered texture/samples or the failure
/// reason.
pub type TicketResult = Result<TicketPayload>;

/// Completion callback (exactly-once delivery).
pub type Completion = Box<dyn FnOnce(TicketResult) + Send>;

/// Frame producer: renders the frame for a ticket. Installed by the
/// manager (eval-based CPU generation for this pass); tests install
/// custom producers.
pub type Producer = Arc<dyn Fn(Rational, &VideoTicketParams) -> TicketResult + Send + Sync>;

/// Ticket metadata: the closed set of C++ `set_property` keys
/// (no Variant property bag).
#[derive(Clone, Debug, Default)]
pub struct TicketMeta {
	/// Ticket kind (video/audio).
	pub kind: Option<i32>,
	/// Frame time.
	pub time: Option<Rational>,
	/// Cache directory/uuid/timebase (video tickets writing the frame
	/// cache).
	pub cache_dir: Option<String>,
	/// Cache uuid.
	pub cache_id: Option<String>,
	/// Cache timebase.
	pub cache_timebase: Option<Rational>,
}

/// Ticket kinds (C++ `RenderManager::TicketType`).
pub mod ticket_kind {
	/// Video ticket.
	pub const VIDEO: i32 = 0;
	/// Audio ticket.
	pub const AUDIO: i32 = 1;
}

/// A submitted ticket (arena id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TicketId(pub u64);

enum SlotState {
	Running,
	Finished,
}

/// A single in-flight ticket (shared between the arena, the worker's job
/// closure and the FFI ticket handle).
struct TicketSlot {
	id: TicketId,
	kind: i32,
	time: Rational,
	range: TimeRange,
	meta: Mutex<TicketMeta>,
	state: Mutex<SlotState>,
	cv: Condvar,
	cancel: AtomicBool,
	delivered: AtomicBool,
	completion: Mutex<Option<Completion>>,
	result: Mutex<Option<Arc<TicketResult>>>,
	/// The dispatch the ticket posted through (M15 S2: a cancelled ticket
	/// whose render completed must recycle its shm slot — the consumer
	/// never sees the `ShmFrame` payload).
	dispatch: Arc<dyn JobDispatch>,
	/// The caller polls `wait()`/`result()` after completion (the sync
	/// render path): the arena entry must survive `finish()` until
	/// `result()` reaps it. Fire-and-forget tickets (completion-only:
	/// playback window, autocacher, manager) are reaped at the next
	/// `allocate()` once finished — the arena map must not grow
	/// unbounded (playback posts 50-100 tickets/sec).
	poll_after: bool,
}

impl TicketSlot {
	fn finish(&self, mut result: TicketResult) {
		if self.cancel.load(Ordering::Acquire) {
			// A cancelled ticket still holds its shm slot when the render
			// completed: recycle it now, before the payload is replaced by
			// `Error::State` and the consumer loses it.
			if let Ok(TicketPayload::ShmFrame(frame)) = &result {
				self.dispatch.release_frame(frame);
			}
			if let Ok(TicketPayload::ShmAudio(audio)) = &result {
				self.dispatch.release_audio_frame(audio);
			}
			result = Err(Error::State);
		}
		// Publish the result before flipping the state flag: `wait()` only
		// observes the state, so a waiter must never see `Finished` before
		// the result is readable (otherwise `wait()` → `result()` races).
		{
			let mut s = lock(&self.state);
			if !matches!(*s, SlotState::Running) {
				return; // already finished: keep exactly-once
			}
			*lock(&self.result) = Some(Arc::new(result));
			*s = SlotState::Finished;
		}
		self.cv.notify_all();

		if self.delivered.swap(true, Ordering::AcqRel) {
			return;
		}
		let done = lock(&self.completion).take();
		if let Some(done) = done {
			let stored = lock(&self.result).clone();
			match stored {
				Some(r) => done((*r).clone()),
				None => done(Err(Error::State)),
			}
		}
	}

	fn is_finished(&self) -> bool {
		matches!(*lock(&self.state), SlotState::Finished)
	}

	fn result(&self) -> Option<TicketResult> {
		lock(&self.result).as_ref().map(|r| (**r).clone())
	}
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The ticket arena (owned by the manager).
pub struct TicketArena {
	next: AtomicU64,
	dispatch: Arc<dyn JobDispatch>,
	audio_dispatch: Arc<dyn JobDispatch>,
	/// M15 S3: main-process inline audio fallback, used when the process
	/// dispatcher is unavailable (shutting down) — design §3.7. Audio
	/// rendering normally runs in oak-worker; the inline fallback keeps
	/// playback alive during teardown.
	audio_fallback: Option<Arc<dyn JobDispatch>>,
	slots: Mutex<HashMap<TicketId, Arc<TicketSlot>>>,
	shutting_down: AtomicBool,
	producer: Producer,
}

impl TicketArena {
	/// Arena dispatching video and audio through the same backend.
	pub fn new(dispatch: Arc<dyn JobDispatch>, producer: Producer) -> Self {
		Self::new_with_audio(dispatch.clone(), dispatch, producer)
	}

	/// Arena with separate video/audio backends (M15: video may run on
	/// the process dispatcher while audio stays on the main-process
	/// thread dispatch until S3 — design §3.7).
	pub fn new_with_audio(
		video: Arc<dyn JobDispatch>,
		audio: Arc<dyn JobDispatch>,
		producer: Producer,
	) -> Self {
		Self::new_with_audio_fallback(video, audio, None, producer)
	}

	/// Arena with separate video/audio backends plus a main-process inline
	/// audio fallback (M15 S3): when `audio` (the process dispatcher)
	/// refuses a job, it is re-posted to `audio_fallback` (an
	/// [`crate::worker::InlineDispatcher::sync`]); both gone, the ticket
	/// cancels.
	pub fn new_with_audio_fallback(
		video: Arc<dyn JobDispatch>,
		audio: Arc<dyn JobDispatch>,
		audio_fallback: Option<Arc<dyn JobDispatch>>,
		producer: Producer,
	) -> Self {
		Self {
			next: AtomicU64::new(1),
			dispatch: video,
			audio_dispatch: audio,
			audio_fallback,
			slots: Mutex::new(HashMap::new()),
			shutting_down: AtomicBool::new(false),
			producer,
		}
	}

	/// A producer that always fails (kept for tests that need a failing
	/// producer).
	pub fn audio_producer() -> Producer {
		Arc::new(|_, _| Err(Error::Failed("audio rendering not implemented".into())))
	}

	fn allocate(&self, slot: Arc<TicketSlot>) -> TicketId {
		let id = slot.id;
		let mut slots = lock(&self.slots);
		slots.insert(id, slot);
		// Reap finished fire-and-forget tickets (their completions were
		// delivered at finish(); nobody will poll them). Poll-path slots
		// survive until `result()` reaps them.
		slots.retain(|_, s| !(s.is_finished() && !s.poll_after));
		id
	}

	/// Allocate the next ticket id without submitting a job. Used by the
	/// FFI ticket layer to stamp its handle *before* the job is posted —
	/// a fast worker must never observe the placeholder id in a completion
	/// callback (see `oakrender_ticket_render_frame` in ffi.rs).
	pub fn next_id(&self) -> TicketId {
		TicketId(self.next.fetch_add(1, Ordering::Relaxed))
	}

	/// Submit a video ticket with a caller-reserved id (allocated by
	/// [`TicketArena::next_id`]); completion fires exactly once, including
	/// on cancellation (with `Error::State`). The job posts as a Seek
	/// single-frame request (M15 S2; see
	/// [`TicketArena::submit_playback`] for the pre-render window).
	/// The caller is expected to poll `wait()`/`result()` (the sync
	/// render path); the arena entry survives until `result()` reaps it.
	pub fn submit_video_with_id(
		&self,
		id: TicketId,
		params: VideoTicketParams,
		done: Completion,
	) -> TicketId {
		self.submit_video_impl(id, params, done, JobSchedule::seek(), true)
	}

	/// The shared video-ticket submit path: register the slot, post the
	/// job with `schedule` (M15 S2), and deliver `Error::State`
	/// immediately when the backend is gone. `poll_after` marks entries
	/// the caller reaps via `result()` (sync path) rather than the
	/// completion (fire-and-forget — reaped once finished).
	fn submit_video_impl(
		&self,
		id: TicketId,
		params: VideoTicketParams,
		done: Completion,
		schedule: JobSchedule,
		poll_after: bool,
	) -> TicketId {
		let meta = TicketMeta {
			kind: Some(ticket_kind::VIDEO),
			time: Some(params.time),
			cache_dir: params.cache_dir.clone(),
			cache_id: params.cache_id.clone(),
			cache_timebase: params.cache_timebase,
		};
		let slot = Arc::new(TicketSlot {
			id,
			kind: ticket_kind::VIDEO,
			time: params.time,
			range: TimeRange::new(params.time, params.time),
			meta: Mutex::new(meta),
			state: Mutex::new(SlotState::Running),
			cv: Condvar::new(),
			cancel: AtomicBool::new(false),
			delivered: AtomicBool::new(false),
			completion: Mutex::new(Some(done)),
			result: Mutex::new(None),
			dispatch: self.dispatch.clone(),
			poll_after,
		});
		self.allocate(slot.clone());

		let params = Arc::new(params);
		let producer = self.producer.clone();
		let slot_done = slot.clone();
		let job = crate::worker::Job {
			node_identity: params.viewer,
			time: params.time,
			params,
			audio: None,
			produce: producer,
			done: Box::new(move |result| slot_done.finish(result)),
			schedule,
		};
		if !self.dispatch.post(job) {
			// Backend is gone (shutdown raced the submit): deliver now.
			slot.finish(Err(Error::State));
		}
		id
	}

	/// Submit a playback-window frame (M15 S2 pre-render window): the
	/// frame joins the scheduler at Playback priority, ordered by
	/// `distance` from the playhead and keyed under `version`. `frame` is
	/// the sequence frame number (the scheduler key / interleaved shard).
	/// The completion fires (from the dispatcher's poll) with
	/// `TicketPayload::ShmFrame`.
	pub fn submit_playback(
		&self,
		params: VideoTicketParams,
		frame: i64,
		distance: i64,
		version: u64,
		done: Completion,
	) -> TicketId {
		let id = self.next_id();
		self.submit_video_impl(
			id,
			params,
			done,
			JobSchedule::playback(frame, distance, version),
			false,
		)
	}

	/// Submit a Background-priority frame (M15 S2 exports/precache): the
	/// scheduler renders it whenever no Seek/Playback work is pending.
	pub fn submit_video_background(
		&self,
		params: VideoTicketParams,
		done: Completion,
	) -> TicketId {
		let id = self.next_id();
		self.submit_video_background_with_id(id, params, done)
	}

	/// [`TicketArena::submit_video_background`] with a caller-reserved id
	/// (the export loop pre-allocates ids through [`TicketArena::next_id`]).
	pub fn submit_video_background_with_id(
		&self,
		id: TicketId,
		params: VideoTicketParams,
		done: Completion,
	) -> TicketId {
		self.submit_video_impl(id, params, done, JobSchedule::background(), false)
	}

	/// Submit a video ticket; completion fires exactly once, including
	/// on cancellation (with `Error::State`).
	pub fn submit_video(&self, params: VideoTicketParams, done: Completion) -> TicketId {
		let id = self.next_id();
		self.submit_video_impl(id, params, done, JobSchedule::seek(), false)
	}

	/// Submit an audio ticket (range pull; C++ render_audio) with a
	/// caller-reserved id (allocated by [`TicketArena::next_id`]). The
	/// completion fires exactly once. The caller is expected to poll
	/// `wait()`/`result()` (the sync path); the arena entry survives
	/// until `result()` reaps it.
	pub fn submit_audio_with_id(
		&self,
		id: TicketId,
		params: AudioTicketParams,
		done: Completion,
	) -> TicketId {
		self.submit_audio_impl(id, params, done, true)
	}

	/// The shared audio-ticket submit path (`poll_after` 语义同
	/// [`TicketArena::submit_video_impl`]).
	fn submit_audio_impl(
		&self,
		id: TicketId,
		params: AudioTicketParams,
		done: Completion,
		poll_after: bool,
	) -> TicketId {
		let range = params.range;
		let meta = TicketMeta {
			kind: Some(ticket_kind::AUDIO),
			time: Some(range.in_()),
			..Default::default()
		};
		let slot = Arc::new(TicketSlot {
			id,
			kind: ticket_kind::AUDIO,
			time: range.in_(),
			range,
			meta: Mutex::new(meta),
			state: Mutex::new(SlotState::Running),
			cv: Condvar::new(),
			cancel: AtomicBool::new(false),
			delivered: AtomicBool::new(false),
			completion: Mutex::new(Some(done)),
			result: Mutex::new(None),
			dispatch: self.audio_dispatch.clone(),
			poll_after,
		});
		self.allocate(slot.clone());

		// The audio producer captures the montage + output params; the
		// video-params field of the job is a dummy (unused by the audio
		// path). M15 S3: `audio` carries the params to the process
		// dispatcher, which renders the range in oak-worker via
		// render_audio_batch; the producer is the inline-fallback path.
		let ap = Arc::new(params);
		let viewer = ap.viewer;
		let make_job = |slot_done: Arc<TicketSlot>| {
			let ap_job = ap.clone();
			let ap_prod = ap.clone();
			let producer: Producer = Arc::new(move |_, _| eval::render_audio_samples(&ap_prod));
			crate::worker::Job {
				node_identity: viewer,
				time: range.in_(),
				params: Arc::new(VideoTicketParams {
					viewer,
					project: String::new(),
					time: range.in_(),
					force_size: None,
					force_format: None,
					cache: None,
					cache_dir: None,
					cache_id: None,
					cache_timebase: None,
					footage: None,
					montage: Vec::new(),
				}),
				audio: Some(ap_job),
				produce: producer,
				done: Box::new(move |result| slot_done.finish(result)),
				schedule: JobSchedule::seek(),
			}
		};
		let job = make_job(slot.clone());
		if !self.audio_dispatch.post(job) {
			// The process dispatcher is unavailable (shutting down): fall
			// back to main-process inline audio rendering (design §3.7) so
			// playback/export audio keeps flowing during teardown.
			if let Some(fallback) = &self.audio_fallback {
				let job = make_job(slot.clone());
				if !fallback.post(job) {
					slot.finish(Err(Error::State));
				}
			} else {
				slot.finish(Err(Error::State));
			}
		}
		id
	}

	/// Submit an audio ticket (range pull; C++ render_audio). The
	/// completion fires exactly once.
	pub fn submit_audio(&self, params: AudioTicketParams, done: Completion) -> TicketId {
		let id = self.next_id();
		self.submit_audio_impl(id, params, done, false)
	}

	/// True when the ticket has finished.
	pub fn is_finished(&self, id: TicketId) -> bool {
		match lock(&self.slots).get(&id) {
			Some(slot) => slot.is_finished(),
			None => false,
		}
	}

	/// Blocking wait for completion (C++ wait_for_finished). M15 S2: the
	/// process dispatcher delivers completions from its poll loop, so a
	/// blocking wait pumps it (the UI tick may be blocked right here).
	pub fn wait(&self, id: TicketId) -> Result<()> {
		let slot = lock(&self.slots).get(&id).cloned().ok_or(Error::NotFound)?;
		let mut state = lock(&slot.state);
		while !matches!(*state, SlotState::Finished) {
			// Pump the backend before blocking again. The slot lock is
			// dropped first: poll() delivers completions that finish this
			// very slot (finish() takes the same lock).
			drop(state);
			self.dispatch.poll();
			state = lock(&slot.state);
			if matches!(*state, SlotState::Finished) {
				break;
			}
			let timeout = std::time::Duration::from_millis(5);
			let (g, _) = slot
				.cv
				.wait_timeout(state, timeout)
				.unwrap_or_else(|e| e.into_inner());
			state = g;
		}
		Ok(())
	}

	/// The ticket result, when finished (clone; unknown/unfinished ids give
	/// `None`).
	pub fn result(&self, id: TicketId) -> Option<TicketResult> {
		// Terminal read: the poll path's only observation point — reap
		// the entry with it (the arena map must not grow unbounded).
		lock(&self.slots).remove(&id).and_then(|s| s.result())
	}

	/// Ticket metadata query (C++ property()).
	pub fn meta(&self, id: TicketId) -> Option<TicketMeta> {
		lock(&self.slots).get(&id).map(|s| lock(&s.meta).clone())
	}

	/// The ticket's kind.
	pub fn kind(&self, id: TicketId) -> Option<i32> {
		lock(&self.slots).get(&id).map(|s| s.kind)
	}

	/// The ticket's time (video tickets).
	pub fn time(&self, id: TicketId) -> Option<Rational> {
		lock(&self.slots).get(&id).map(|s| s.time)
	}

	/// The ticket's range (audio tickets).
	pub fn range(&self, id: TicketId) -> Option<TimeRange> {
		lock(&self.slots).get(&id).map(|s| s.range)
	}

	/// Cancel a pending ticket (its completion still fires with
	/// `Error::State`; unknown ids are ignored).
	pub fn cancel(&self, id: TicketId) {
		if let Some(slot) = lock(&self.slots).get(&id) {
			slot.cancel.store(true, Ordering::Release);
		}
	}

	/// Cancel all pending tickets (manager shutdown path). Delivery happens
	/// when the backend drains the queued jobs (or the running jobs
	/// finish); call the backend's `shutdown` afterwards to guarantee all
	/// completions have fired.
	pub fn cancel_all(&self) {
		self.shutting_down.store(true, Ordering::Release);
		for slot in lock(&self.slots).values() {
			slot.cancel.store(true, Ordering::Release);
		}
	}

	/// True after [`TicketArena::cancel_all`].
	pub fn is_shutting_down(&self) -> bool {
		self.shutting_down.load(Ordering::Acquire)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::mpsc;
	use std::time::Duration;

	use crate::worker::{InlineDispatcher, JobDispatch};
	use oak_core::frame::VideoParamsPod;
	use oak_core::texture::Frame;

	fn small_frame() -> Frame {
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 4;
		p.height = 4;
		f.set_video_params(p);
		f.allocate();
		f
	}

	fn ok_producer() -> Producer {
		Arc::new(|_, _| Ok(TicketPayload::Video(Texture::wrap_frame(small_frame()))))
	}

	/// A queued inline dispatcher for video (deterministic cancel-race and
	/// shutdown semantics) plus a sync inline dispatcher for audio (the
	/// production audio mode). `run` drains the video queue.
	fn test_arena(producer: Producer) -> (TicketArena, Arc<InlineDispatcher>) {
		let video = InlineDispatcher::queued();
		let audio = InlineDispatcher::sync();
		let arena = TicketArena::new_with_audio(video.clone(), audio, producer);
		(arena, video)
	}

	#[test]
	fn completion_fires_exactly_once_on_success() {
		let (arena, video) = test_arena(ok_producer());

		let (tx, rx) = mpsc::channel();
		let id = arena.submit_video(
			VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(0, 1),
				force_size: Some((4, 4)),
				force_format: None,
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			Box::new(move |r| {
				let _ = tx.send(r.is_ok());
			}),
		);

		video.run();
		arena.wait(id).unwrap();
		let ok = rx.recv_timeout(Duration::from_secs(5)).unwrap();
		assert!(ok, "completion fired with success");
		assert!(
			rx.recv_timeout(Duration::from_millis(50)).is_err(),
			"exactly once"
		);

		// is_finished 必须在 result() 之前查：result() 是终止性读取
		// （连同条目一起回收，arena 表不能无限增长）。
		assert!(arena.is_finished(id));
		let res = arena.result(id).unwrap().unwrap();
		assert_eq!(
			match res {
				TicketPayload::Video(t) => t.size(),
				_ => panic!("video ticket"),
			},
			(4, 4)
		);
		video.shutdown();
	}

	#[test]
	fn completion_fires_exactly_once_on_cancel() {
		// Queued mode: the job is posted but not run yet, so a cancel
		// before `run` must deliver State exactly once (the old worker-pool
		// test used a blocking producer; the inline queue makes the same
		// cancel-before-completion race deterministic without threads).
		let (arena, video) = test_arena(ok_producer());

		let (tx, rx) = mpsc::channel();
		let id = arena.submit_video(
			VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(0, 1),
				force_size: None,
				force_format: None,
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			Box::new(move |r| {
				let _ = tx.send(r);
			}),
		);

		// Cancel before the job runs.
		arena.cancel(id);
		video.run();
		arena.wait(id).unwrap();

		let res = rx.recv_timeout(Duration::from_secs(5)).unwrap();
		assert_eq!(res.unwrap_err().code(), Error::State.code());
		assert!(
			rx.recv_timeout(Duration::from_millis(50)).is_err(),
			"exactly once"
		);
		video.shutdown();
	}

	#[test]
	fn cancel_of_unknown_id_is_ignored() {
		let (arena, video) = test_arena(ok_producer());
		arena.cancel(TicketId(12345));
		assert!(!arena.is_finished(TicketId(12345)));
		video.shutdown();
	}

	#[test]
	fn wait_unknown_id_errors() {
		let (arena, video) = test_arena(ok_producer());
		assert_eq!(
			arena.wait(TicketId(999)).unwrap_err().code(),
			Error::NotFound.code()
		);
		video.shutdown();
	}

	#[test]
	fn audio_ticket_meta_and_kind() {
		// Audio runs on the sync inline backend (the production audio
		// mode), so the completion fires during the submit.
		let (arena, video) = test_arena(ok_producer());

		let (tx, rx) = mpsc::channel();
		let range = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
		let id = arena.submit_audio(
			AudioTicketParams {
				viewer: 7,
				range,
				sample_rate: 48000,
				channel_layout: 0x3,
				montage: Vec::new(),
			},
			Box::new(move |r: TicketResult| {
				let _ = tx.send(r.is_err());
			}),
		);
		assert_eq!(arena.kind(id), Some(ticket_kind::AUDIO));
		assert_eq!(arena.meta(id).unwrap().kind, Some(ticket_kind::AUDIO));
		assert_eq!(arena.range(id), Some(range));
		arena.wait(id).unwrap();
		// M12 P1: an empty-montage audio ticket succeeds with silence
		// (the completion must NOT report an error).
		assert!(!rx.recv_timeout(Duration::from_secs(5)).unwrap());
		// get_time equivalent: audio tickets report range.in.
		assert_eq!(arena.time(id), Some(range.in_()));
		video.shutdown();
	}

	#[test]
	fn ticket_ids_are_monotonic() {
		let (arena, video) = test_arena(ok_producer());
		let a = arena.submit_video(
			VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(0, 1),
				force_size: None,
				force_format: None,
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			Box::new(|_| {}),
		);
		let b = arena.submit_video(
			VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(1, 1),
				force_size: None,
				force_format: None,
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
			},
			Box::new(|_| {}),
		);
		assert!(b.0 > a.0);
		assert_ne!(a, b);
		video.shutdown();
	}
}
