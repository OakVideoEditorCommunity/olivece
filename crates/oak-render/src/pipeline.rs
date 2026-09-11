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

//! M1: the thread pipeline skeleton (design §3.1 / §3.3 / §3.4).
//!
//! Three threads, one queue pair, no more:
//!
//! - **UI / main thread** — the consumer. It submits through the ticket
//!   arena and consumes finished frames. There is deliberately no
//!   presentation thread (design §3.3): the arena's completion mechanism
//!   *is* the present queue. A finished ticket (the `done` callback, or
//!   `TicketArena::wait` returning) is the signal that a frame is ready to
//!   be shown; the UI thread reads the payload on its next tick and
//!   uploads/blits it. Nothing in this module uploads or presents anything.
//! - **Render thread** ([`PipelineBackend`], thread name `oak-render`) —
//!   the pipeline backend's ONLY render executor: it takes jobs off the
//!   render queue and runs the same producer the inline and process
//!   backends run ([`crate::worker::execute_job`]), then completes the
//!   ticket. Every GPU call the backend makes happens on this thread;
//!   `GpuContext::shared()` is unchanged. A producer that submits follow-up
//!   work re-posts it to this same thread.
//! - **Decode thread** ([`DecodeService`], thread name `oak-decode`) — the
//!   optional process-wide decode service. [`crate::eval::render_footage_frame`]
//!   renders through it by rendezvous while it is installed, which is what
//!   moves real decoding (and the FFmpeg / NVDEC calls it makes) off the
//!   render thread. With no service installed the decode path is exactly
//!   the synchronous one it has always been.
//!
//! ### Queues and backpressure
//!
//! - **Render queue**: bounded at [`RENDER_QUEUE_CAP`] jobs, FIFO. A post
//!   blocks once it is full (the arena submits from the UI thread, so a
//!   stalled pipeline throttles submission instead of growing without
//!   bound). The single exception is a post made *from the render thread*,
//!   which can never wait for room it would have to make itself.
//! - **Decode queue**: bounded at [`DECODE_QUEUE_CAP`] commands. The
//!   rendezvous request blocks when the queue is full — the same throttle,
//!   one stage upstream of the render queue.
//! - **Prefetch** is best-effort: a prefetch is *dropped* (never queued)
//!   when the pipeline is saturated. [`DecodeService`] carries a
//!   [`PrefetchGate`] — the backend wires it to "the render queue has
//!   room" — so speculative decoding never delays a foreground request.
//!   Dropped prefetches are counted ([`DecodeStats::prefetch_refused`]).
//!
//! ### Present path
//!
//! Ticket completion → the UI thread's payload read → upload/blit, all on
//! the UI thread; the pipeline adds no thread and no queue for it. This is
//! the M1 reading of design §3.3 ("上屏队列 = 完成队列").
//!
//! ### Relationship to the process pool
//!
//! [`PipelineBackend`] is an additional [`crate::worker::JobDispatch`]
//! implementation next to [`crate::procpool::ProcessDispatcher`]: the
//! process pool stays the default backend, and `OAK_PIPELINE=threads` (or
//! [`crate::manager::RenderBackendChoice::Pipeline`]) selects the thread
//! backend explicitly. Ticket arena, scheduler hints, snapshot push and
//! cancellation semantics are identical on both — only the executor
//! differs.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};

use crate::error::{Error, Result};
use crate::worker::{execute_job, Job, JobDispatch};

/// Render-queue bound (jobs). Small on purpose: the queue exists to keep
/// the render thread fed across a UI tick, not to buffer a whole
/// pre-render window — a backlog this deep already means the pipeline is
/// slower than real time, and blocking the submitter is the honest
/// response (design §3.4).
pub const RENDER_QUEUE_CAP: usize = 8;

/// Decode-command queue bound (requests + prefetch messages). Deeper than
/// the render queue because decodes are short and the service is the only
/// path to the media, but still small enough to bound latency.
pub const DECODE_QUEUE_CAP: usize = 16;

/// Decoded-frame LRU capacity, in frames (~31 MB each at 1080p F32, so a
/// handful of frames is already hundreds of MB). Sized for a playback
/// window plus the montage baseline; the M2 decoder work lands here.
pub const DECODE_LRU_CAP: usize = 8;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Decode service
// ---------------------------------------------------------------------------

/// One decode request: everything that identifies a decodable frame, and
/// the exact key of the service's frame cache. The size is part of the key
/// (the same media at a different target resolution is a different frame:
/// an interleaved viewer/proxy request must never reuse a wrongly-sized
/// buffer); `(0, 0)` means "native size".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DecodeRequest {
	/// Footage filename.
	pub filename: String,
	/// Media stream index.
	pub stream_index: i32,
	/// Frame time (the frame containing this time is decoded).
	pub time: Rational,
	/// Target size, or `(0, 0)` for the media's native size.
	pub size: (i32, i32),
	/// Target pixel format.
	pub format: PixelFormat,
}

/// Decode-service counters (M1's evidence that the service really decodes
/// and really caches; the M2 work keeps them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
	/// Rendezvous requests received.
	pub requests: u64,
	/// Requests served from the frame LRU (no decode).
	pub lru_hits: u64,
	/// Frames actually decoded (LRU misses and accepted prefetches).
	pub decodes: u64,
	/// Prefetch messages accepted for execution.
	pub prefetches: u64,
	/// Prefetch messages dropped by the gate or a full queue.
	pub prefetch_refused: u64,
	/// LRU entries evicted to stay under the capacity.
	pub evictions: u64,
	/// Failed decodes (reported to the requester, or swallowed for a
	/// prefetch).
	pub errors: u64,
}

#[derive(Default)]
struct DecodeCounters {
	requests: AtomicU64,
	lru_hits: AtomicU64,
	decodes: AtomicU64,
	prefetches: AtomicU64,
	prefetch_refused: AtomicU64,
	evictions: AtomicU64,
	errors: AtomicU64,
}

impl DecodeCounters {
	fn snapshot(&self) -> DecodeStats {
		DecodeStats {
			requests: self.requests.load(Ordering::Relaxed),
			lru_hits: self.lru_hits.load(Ordering::Relaxed),
			decodes: self.decodes.load(Ordering::Relaxed),
			prefetches: self.prefetches.load(Ordering::Relaxed),
			prefetch_refused: self.prefetch_refused.load(Ordering::Relaxed),
			evictions: self.evictions.load(Ordering::Relaxed),
			errors: self.errors.load(Ordering::Relaxed),
		}
	}
}

/// Prefetch gate: `true` = prefetch work may be queued. The backend wires
/// this to "the render queue has room", so prefetch never competes with
/// foreground frames for the decode queue.
pub type PrefetchGate = Arc<dyn Fn() -> bool + Send + Sync>;

/// A command for the decode thread.
enum DecodeCommand {
	/// Rendezvous decode: the reply carries the frame or the decode error.
	Request {
		request: DecodeRequest,
		reply: SyncSender<Result<Texture>>,
	},
	/// Cache-fill only: the result is never delivered anywhere.
	Prefetch { request: DecodeRequest },
	/// Barrier: replies once every command sent before it has been fully
	/// processed (used by tests and by the decode-service contract).
	Sync { reply: SyncSender<()> },
	/// Stop the thread (it drains, then exits).
	Shutdown,
}

struct DecodeInner {
	counters: DecodeCounters,
	lru_len: AtomicUsize,
}

/// The optional process-wide decode service (design §3.3): one decode
/// thread, a bounded command queue, and a bounded decoded-frame LRU.
///
/// Installed by [`PipelineBackend::new`] and removed by its shutdown; also
/// usable standalone ([`DecodeService::new`]), which is how the unit tests
/// drive it. [`crate::eval::render_footage_frame`] routes through it while
/// it is installed and falls back to the synchronous decode otherwise.
///
/// The LRU lives on the decode thread (no lock: it is only ever touched
/// there); [`DecodeService::stats`] and [`DecodeService::lru_len`] read
/// shared counters, so they are safe from any thread.
pub struct DecodeService {
	commands: SyncSender<DecodeCommand>,
	gate: PrefetchGate,
	inner: Arc<DecodeInner>,
	handle: Mutex<Option<JoinHandle<()>>>,
}

impl DecodeService {
	/// Start a decode thread with an LRU of `lru_capacity` frames and
	/// `gate` deciding whether prefetch work is wanted. `lru_capacity` 0
	/// disables caching (every request decodes).
	pub fn new(lru_capacity: usize, gate: PrefetchGate) -> Arc<Self> {
		let (tx, rx) = mpsc::sync_channel(DECODE_QUEUE_CAP);
		let inner = Arc::new(DecodeInner {
			counters: DecodeCounters::default(),
			lru_len: AtomicUsize::new(0),
		});
		let service = Arc::new(Self {
			commands: tx,
			gate,
			inner: inner.clone(),
			handle: Mutex::new(None),
		});
		let spawned = std::thread::Builder::new()
			.name("oak-decode".into())
			.spawn(move || decode_loop(rx, inner, lru_capacity));
		// A failed spawn leaves the receiver dropped, so `request` reports
		// the service as unavailable and the caller decodes inline.
		if let Ok(handle) = spawned {
			*lock(&service.handle) = Some(handle);
		}
		service
	}

	/// Decode `request`, returning the frame. `None` means the service is
	/// gone (shutting down, or already down) — the caller must decode
	/// inline instead; `Some(Err(_))` is a real decode failure and must be
	/// propagated as such.
	///
	/// Blocks while the decode queue is full: that block is the backpressure
	/// the render side propagates to submission.
	pub fn request(&self, request: DecodeRequest) -> Option<Result<Texture>> {
		let (reply, rx) = mpsc::sync_channel(1);
		match self.commands.send(DecodeCommand::Request { request, reply }) {
			Ok(()) => {}
			Err(_) => return None,
		}
		match rx.recv() {
			Ok(result) => Some(result),
			// The command was dropped (shutdown drained the queue) or the
			// decode thread died mid-request: the caller falls back inline.
			Err(_) => None,
		}
	}

	/// Queue a speculative decode of `request`; `false` when the gate says
	/// no (or the queue is full / the service is down), in which case
	/// nothing was decoded and nothing was queued.
	pub fn prefetch(&self, request: DecodeRequest) -> bool {
		if !(self.gate)() {
			self.inner
				.counters
				.prefetch_refused
				.fetch_add(1, Ordering::Relaxed);
			return false;
		}
		match self.commands.try_send(DecodeCommand::Prefetch { request }) {
			Ok(()) => {
				self.inner.counters.prefetches.fetch_add(1, Ordering::Relaxed);
				true
			}
			Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
				self.inner
					.counters
					.prefetch_refused
					.fetch_add(1, Ordering::Relaxed);
				false
			}
		}
	}

	/// Wait until every command sent before this call has been processed
	/// (a barrier through the same FIFO queue). `false` when the service is
	/// already gone.
	pub fn wait_idle(&self) -> bool {
		let (reply, rx) = mpsc::sync_channel(0);
		if self.commands.send(DecodeCommand::Sync { reply }).is_err() {
			return false;
		}
		rx.recv().is_ok()
	}

	/// The current counters.
	pub fn stats(&self) -> DecodeStats {
		self.inner.counters.snapshot()
	}

	/// The number of frames currently in the LRU.
	pub fn lru_len(&self) -> usize {
		self.inner.lru_len.load(Ordering::Relaxed)
	}

	/// Stop the decode thread and wait for it to exit. Idempotent; a
	/// request arriving after this (or racing it) reports `None` and the
	/// caller decodes inline.
	pub fn shutdown(&self) {
		let handle = lock(&self.handle).take();
		let Some(handle) = handle else { return };
		let _ = self.commands.send(DecodeCommand::Shutdown);
		let _ = handle.join();
	}
}

/// The decode thread: one command at a time, LRU in a plain `HashMap`
/// keyed by [`DecodeRequest`] with a monotonic recency tick.
fn decode_loop(rx: mpsc::Receiver<DecodeCommand>, inner: Arc<DecodeInner>, lru_capacity: usize) {
	let mut lru: HashMap<DecodeRequest, (Texture, u64)> = HashMap::new();
	let mut tick: u64 = 1;
	while let Ok(command) = rx.recv() {
		match command {
			DecodeCommand::Request { request, reply } => {
				let result = serve(&request, &mut lru, &mut tick, lru_capacity, &inner);
				// The requester may have gone away; the frame is still
				// cached, so the decode was not wasted.
				let _ = reply.send(result);
			}
			DecodeCommand::Prefetch { request } => {
				prefetch_into(&request, &mut lru, &mut tick, lru_capacity, &inner);
			}
			DecodeCommand::Sync { reply } => {
				let _ = reply.send(());
			}
			DecodeCommand::Shutdown => break,
		}
	}
	// Anything still queued is dropped with the receiver: the pending
	// reply senders disconnect, so blocked requesters see `None` and fall
	// back to the inline decode.
	inner.lru_len.store(0, Ordering::Relaxed);
}

fn serve(
	request: &DecodeRequest,
	lru: &mut HashMap<DecodeRequest, (Texture, u64)>,
	tick: &mut u64,
	lru_capacity: usize,
	inner: &DecodeInner,
) -> Result<Texture> {
	inner.counters.requests.fetch_add(1, Ordering::Relaxed);
	if let Some(texture) = lru_get(lru, request, tick) {
		inner.counters.lru_hits.fetch_add(1, Ordering::Relaxed);
		return Ok(texture);
	}
	let texture = decode(request, inner)?;
	lru_insert(lru, request.clone(), texture.clone(), lru_capacity, inner, tick);
	inner.lru_len.store(lru.len(), Ordering::Relaxed);
	Ok(texture)
}

fn prefetch_into(
	request: &DecodeRequest,
	lru: &mut HashMap<DecodeRequest, (Texture, u64)>,
	tick: &mut u64,
	lru_capacity: usize,
	inner: &DecodeInner,
) {
	if lru_get(lru, request, tick).is_some() {
		// Already decoded: `lru_get` refreshed its recency, nothing to do.
		return;
	}
	match decode(request, inner) {
		Ok(texture) => {
			lru_insert(lru, request.clone(), texture, lru_capacity, inner, tick);
			inner.lru_len.store(lru.len(), Ordering::Relaxed);
		}
		// Best-effort: a prefetch failure is counted, never raised — no
		// ticket is waiting on it, and the foreground request that follows
		// will report the same error through its own channel.
		Err(_) => {}
	}
}

/// The real decode: the same producer code the inline path runs, on the
/// decode thread.
fn decode(request: &DecodeRequest, inner: &DecodeInner) -> Result<Texture> {
	inner.counters.decodes.fetch_add(1, Ordering::Relaxed);
	let result = crate::eval::render_footage_frame_inner(
		&request.filename,
		request.stream_index,
		request.time,
		request.size,
		request.format,
	);
	if result.is_err() {
		inner.counters.errors.fetch_add(1, Ordering::Relaxed);
	}
	result
}

/// LRU read + recency refresh. Returns the cached frame; the clone is the
/// price of handing a texture to the render thread while keeping the
/// service's copy (a GPU-handle texture makes this a token in M2 — the
/// texture type is already the seam for it).
fn lru_get(
	lru: &mut HashMap<DecodeRequest, (Texture, u64)>,
	key: &DecodeRequest,
	tick: &mut u64,
) -> Option<Texture> {
	let entry = lru.get_mut(key)?;
	let texture = entry.0.clone();
	entry.1 = *tick;
	*tick += 1;
	Some(texture)
}

/// LRU insert under the capacity, evicting the least recently used entry.
fn lru_insert(
	lru: &mut HashMap<DecodeRequest, (Texture, u64)>,
	key: DecodeRequest,
	texture: Texture,
	capacity: usize,
	inner: &DecodeInner,
	tick: &mut u64,
) {
	if capacity == 0 {
		return;
	}
	while lru.len() >= capacity && !lru.contains_key(&key) {
		let Some(victim) = lru
			.iter()
			.min_by_key(|(_, (_, t))| *t)
			.map(|(k, _)| k.clone())
		else {
			break;
		};
		lru.remove(&victim);
		inner.counters.evictions.fetch_add(1, Ordering::Relaxed);
	}
	lru.insert(key, (texture, *tick));
	*tick += 1;
}

// ---------------------------------------------------------------------------
// Process-wide service slot (the `PLUGIN_EXECUTOR` pattern from eval.rs)
// ---------------------------------------------------------------------------

static DECODE_SERVICE: OnceLock<Mutex<Option<Arc<DecodeService>>>> = OnceLock::new();

fn decode_service_slot() -> &'static Mutex<Option<Arc<DecodeService>>> {
	DECODE_SERVICE.get_or_init(|| Mutex::new(None))
}

/// Install (or clear) the process-wide decode service. Installed by
/// [`PipelineBackend::new`]; `None` restores the synchronous decode path
/// for every renderer in the process.
pub fn install_decode_service(service: Option<Arc<DecodeService>>) {
	*lock(decode_service_slot()) = service;
}

/// The installed decode service, if any. `eval` consults this per footage
/// frame; `None` means "decode inline as before".
pub fn decode_service() -> Option<Arc<DecodeService>> {
	lock(decode_service_slot()).clone()
}

// ---------------------------------------------------------------------------
// Render thread
// ---------------------------------------------------------------------------

/// Pipeline counters (tests/reporting).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipelineStats {
	/// Jobs accepted into the render queue.
	pub posted: u64,
	/// Jobs executed on the render thread.
	pub executed: u64,
	/// Jobs drained unexecuted by shutdown (completed with
	/// [`Error::State`]).
	pub drained: u64,
}

/// The thread-backed [`JobDispatch`]: one render thread draining a bounded
/// FIFO, and the decode service that feeds its footage decodes.
///
/// Only one instance may be live per process at a time — [`PipelineBackend::new`]
/// installs its decode service into the process-wide slot, and a second
/// live backend would steal the first one's slot (the manager owns the
/// singleton in production; tests serialize on a lock).
pub struct PipelineBackend {
	inner: Arc<PipelineInner>,
}

struct PipelineInner {
	queue: Mutex<VecDeque<Job>>,
	/// A job arrived.
	work: Condvar,
	/// Room appeared (or shutdown started).
	room: Condvar,
	/// `queue` length, mirror of the queue for gate/stat reads without the
	/// lock; shared with the decode service's prefetch gate.
	depth: Arc<AtomicUsize>,
	stopping: AtomicBool,
	/// The render thread's id, set by the thread itself on entry.
	render_thread: OnceLock<std::thread::ThreadId>,
	decode: Arc<DecodeService>,
	handle: Mutex<Option<JoinHandle<()>>>,
	posted: AtomicU64,
	executed: AtomicU64,
	drained: AtomicU64,
}

impl PipelineBackend {
	/// Start the render and decode threads and install the decode service.
	pub fn new() -> Result<Arc<Self>> {
		let depth = Arc::new(AtomicUsize::new(0));
		// Prefetch is allowed only while the render queue has room: it must
		// never fill the decode queue behind a saturated pipeline.
		let gate_depth = depth.clone();
		let gate: PrefetchGate = Arc::new(move || {
			gate_depth.load(Ordering::Relaxed) < RENDER_QUEUE_CAP
		});
		let decode = DecodeService::new(DECODE_LRU_CAP, gate);
		let inner = Arc::new(PipelineInner {
			queue: Mutex::new(VecDeque::new()),
			work: Condvar::new(),
			room: Condvar::new(),
			depth,
			stopping: AtomicBool::new(false),
			render_thread: OnceLock::new(),
			decode: decode.clone(),
			handle: Mutex::new(None),
			posted: AtomicU64::new(0),
			executed: AtomicU64::new(0),
			drained: AtomicU64::new(0),
		});
		let backend = Arc::new(Self { inner: inner.clone() });
		let handle = std::thread::Builder::new()
			.name("oak-render".into())
			.spawn(move || render_loop(inner))
			.map_err(|e| Error::Failed(format!("render thread spawn: {e}")))?;
		*lock(&backend.inner.handle) = Some(handle);
		install_decode_service(Some(decode));
		Ok(backend)
	}

	/// The backend's decode service (for prefetch injection and stats).
	pub fn decode_service(&self) -> Arc<DecodeService> {
		self.inner.decode.clone()
	}

	/// Jobs waiting (not yet taken by the render thread).
	pub fn queue_depth(&self) -> usize {
		self.inner.depth.load(Ordering::Relaxed)
	}

	/// Free render-queue slots — the decode service's prefetch gate.
	pub fn queue_free(&self) -> usize {
		RENDER_QUEUE_CAP.saturating_sub(self.queue_depth())
	}

	/// Counters.
	pub fn stats(&self) -> PipelineStats {
		PipelineStats {
			posted: self.inner.posted.load(Ordering::Relaxed),
			executed: self.inner.executed.load(Ordering::Relaxed),
			drained: self.inner.drained.load(Ordering::Relaxed),
		}
	}

	/// Non-blocking enqueue: `false` when the queue is full or the backend
	/// is stopping. The blocking path is [`JobDispatch::post`]; this exists
	/// for callers that prefer to drop work over waiting (and for tests
	/// that fill the queue deterministically).
	pub fn try_post(&self, job: Job) -> bool {
		self.push(job, false)
	}

	fn push(&self, job: Job, blocking: bool) -> bool {
		let inner = &self.inner;
		let mut queue = lock(&inner.queue);
		if inner.stopping.load(Ordering::Acquire) {
			return false;
		}
		if !blocking && queue.len() >= RENDER_QUEUE_CAP {
			return false;
		}
		// Backpressure, with one exception: the render thread itself may
		// re-post from a completion callback (a producer that submits
		// follow-up work). It can never make room by waiting — it is the
		// only thread that drains the queue — so it is allowed to run one
		// ahead of the bound instead of deadlocking.
		if blocking && !is_render_thread(inner) {
			while queue.len() >= RENDER_QUEUE_CAP {
				if inner.stopping.load(Ordering::Acquire) {
					return false;
				}
				queue = inner.room.wait(queue).unwrap_or_else(|e| e.into_inner());
			}
		}
		queue.push_back(job);
		inner.depth.store(queue.len(), Ordering::Relaxed);
		inner.posted.fetch_add(1, Ordering::Relaxed);
		drop(queue);
		inner.work.notify_one();
		true
	}

	fn shutdown_impl(&self) {
		let inner = &self.inner;
		if inner.stopping.swap(true, Ordering::AcqRel) {
			return;
		}
		// The job the render thread is running finishes (M1 does not
		// interrupt an in-flight frame); everything still queued is
		// delivered as cancelled, exactly like the inline dispatcher.
		let jobs: Vec<Job> = {
			let mut queue = lock(&inner.queue);
			let jobs: Vec<Job> = queue.drain(..).collect();
			inner.depth.store(0, Ordering::Relaxed);
			jobs
		};
		inner.drained.fetch_add(jobs.len() as u64, Ordering::Relaxed);
		inner.work.notify_all();
		inner.room.notify_all();
		for job in jobs {
			(job.done)(Err(Error::State));
		}
		if !is_render_thread(inner) {
			if let Some(handle) = lock(&inner.handle).take() {
				let _ = handle.join();
			}
		}
		// Uninstall first (no new request may be routed to a service that
		// is about to stop), then stop the decode thread.
		if decode_service().is_some_and(|s| Arc::ptr_eq(&s, &inner.decode)) {
			install_decode_service(None);
		}
		inner.decode.shutdown();
	}
}

impl Drop for PipelineBackend {
	fn drop(&mut self) {
		// Safety net: the last handle going away must not leave the render
		// thread parked on an empty queue.
		self.shutdown_impl();
	}
}

impl JobDispatch for PipelineBackend {
	fn post(&self, job: Job) -> bool {
		self.push(job, true)
	}

	fn shutdown(&self) {
		self.shutdown_impl();
	}
}

fn is_render_thread(inner: &PipelineInner) -> bool {
	inner.render_thread.get() == Some(&std::thread::current().id())
}

/// The render thread body: take a job, run it, repeat. Exits when the
/// queue is empty *and* shutdown has been requested.
fn render_loop(inner: Arc<PipelineInner>) {
	let _ = inner.render_thread.set(std::thread::current().id());
	loop {
		let job = {
			let mut queue = lock(&inner.queue);
			loop {
				if let Some(job) = queue.pop_front() {
					inner.depth.store(queue.len(), Ordering::Relaxed);
					inner.room.notify_all();
					break Some(job);
				}
				if inner.stopping.load(Ordering::Acquire) {
					break None;
				}
				queue = inner.work.wait(queue).unwrap_or_else(|e| e.into_inner());
			}
		};
		match job {
			Some(job) => {
				execute_job(job);
				inner.executed.fetch_add(1, Ordering::Relaxed);
			}
			None => break,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use oak_core::texture::Frame;

	/// A unique clip per test (the process id separates test binaries, the
	/// tag separates tests inside one binary — the decode caches are
	/// process-wide).
	fn test_clip(tag: &str) -> std::path::PathBuf {
		let path = std::env::temp_dir().join(format!(
			"oakrender_pipeline_{tag}_{}.mp4",
			std::process::id()
		));
		oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10)
			.expect("test clip generation");
		path
	}

	/// Pin the working space to the legacy sRGB pass-through: these tests
	/// assert the decoded pattern, not the color transform (the ACEScg
	/// default would remap the values).
	fn pin_legacy_working_space() {
		oak_core::color::set_pipeline_color_settings(
			oak_core::colormath::WorkingColorSpace::SrgbLegacy,
			oak_core::colormath::OutputColorSpec::default(),
		);
	}

	fn request(filename: &std::path::Path, time: Rational) -> DecodeRequest {
		DecodeRequest {
			filename: filename.to_string_lossy().to_string(),
			stream_index: 0,
			time,
			size: (64, 64),
			format: PixelFormat::F32,
		}
	}

	fn always() -> PrefetchGate {
		Arc::new(|| true)
	}

	fn frame_of(texture: &Texture) -> &Frame {
		let Texture::Cpu(frame) = texture else {
			panic!("decode produced a non-CPU texture");
		};
		frame
	}

	/// The decoded test pattern: a red|blue split that steps with the frame
	/// index — `oak_codec::testmedia` shifts it by `index * width / (2 * fps)`
	/// columns (9 columns for frame 3 of this 64 px / 10 fps clip). The
	/// sampled columns track that shift; MPEG-2 is lossy, so the assertions
	/// use dominance with generous margins.
	fn assert_known_pattern(frame: &Frame, index: i32, tag: &str) {
		assert_eq!((frame.width, frame.height), (64, 64), "{tag}");
		assert_eq!(frame.format, PixelFormat::F32, "{tag}");
		let stride = frame.linesize_bytes();
		let shift = (index * frame.width / 20).rem_euclid(frame.width);
		let read = |x: usize, y: usize| -> [f32; 4] {
			let off = y * stride + x * 16;
			let mut out = [0f32; 4];
			for i in 0..4 {
				out[i] = f32::from_le_bytes(frame.data[off + i * 4..off + i * 4 + 4].try_into().unwrap());
			}
			out
		};
		// Sample the middle of each half: the generator's split has the red
		// half where `(x + shift) % 64` is below 32.
		let [r, g, b, a] = read((16 - shift).rem_euclid(64) as usize, 32);
		assert!(r > 0.5 && g < 0.4 && b < 0.4, "{tag}: red half {r},{g},{b}");
		assert!(a > 0.9, "{tag}: opaque {a}");
		let [r, g, b, a] = read((48 - shift).rem_euclid(64) as usize, 32);
		assert!(b > 0.5 && r < 0.4 && g < 0.4, "{tag}: blue half {r},{g},{b}");
		assert!(a > 0.9, "{tag}: opaque {a}");
	}

	/// The service decodes real media through the real codec path.
	#[test]
	fn request_decodes_real_media() {
		pin_legacy_working_space();
		let path = test_clip("real");
		let service = DecodeService::new(DECODE_LRU_CAP, always());

		let texture = service
			.request(request(&path, Rational::new(0, 1)))
			.expect("service available")
			.expect("frame 0 decodes");
		assert_known_pattern(frame_of(&texture), 0, "frame 0");

		let stats = service.stats();
		assert_eq!(stats.requests, 1);
		assert_eq!(stats.decodes, 1, "one real decode");
		assert_eq!(stats.lru_hits, 0);
		assert_eq!(stats.errors, 0);
		assert_eq!(service.lru_len(), 1);

		service.shutdown();
		let _ = std::fs::remove_file(&path);
	}

	/// Prefetch really decodes and really fills the LRU: the request that
	/// follows is served from the cache with no decode at all.
	#[test]
	fn prefetch_then_request_hits_the_lru() {
		pin_legacy_working_space();
		let path = test_clip("prefetch");
		let service = DecodeService::new(DECODE_LRU_CAP, always());

		let req = request(&path, Rational::new(3, 10));
		assert!(service.prefetch(req.clone()), "prefetch accepted");
		assert!(service.wait_idle(), "decode thread processed the prefetch");
		let after_prefetch = service.stats();
		assert_eq!(after_prefetch.prefetches, 1);
		assert_eq!(after_prefetch.decodes, 1, "the prefetch really decoded");
		assert_eq!(service.lru_len(), 1);

		// The rendezvous request for the same frame: served from the LRU,
		// so the decode counter does NOT move (this is the assertion that
		// distinguishes a real cache hit from a re-decode).
		let texture = service
			.request(req)
			.expect("service available")
			.expect("cached frame");
		assert_known_pattern(frame_of(&texture), 3, "prefetched frame");
		let after_request = service.stats();
		assert_eq!(after_request.lru_hits, 1);
		assert_eq!(after_request.decodes, after_prefetch.decodes);
		assert_eq!(after_request.requests, 1);

		service.shutdown();
		let _ = std::fs::remove_file(&path);
	}

	/// Decode failures travel back to the requester unchanged.
	#[test]
	fn request_propagates_decode_errors() {
		let service = DecodeService::new(DECODE_LRU_CAP, always());
		let req = DecodeRequest {
			filename: "/definitely/not/here.mp4".into(),
			stream_index: 0,
			time: Rational::new(0, 1),
			size: (64, 64),
			format: PixelFormat::F32,
		};
		let err = service
			.request(req)
			.expect("service available")
			.err()
			.expect("decoding a missing file must fail");
		let _ = err.code(); // an explainable error, not a panic
		let stats = service.stats();
		assert_eq!(stats.errors, 1);
		assert_eq!(stats.decodes, 1);
		assert_eq!(service.lru_len(), 0, "failures are not cached");
		service.shutdown();
	}

	/// The LRU is bounded and evicts least-recently-used first (an evicted
	/// frame must be decoded again).
	#[test]
	fn lru_evicts_bounded() {
		pin_legacy_working_space();
		let path = test_clip("evict");
		let service = DecodeService::new(2, always());
		let time = |n: i64| request(&path, Rational::new(n, 10));

		service.request(time(0)).unwrap().unwrap();
		service.request(time(1)).unwrap().unwrap();
		assert_eq!(service.lru_len(), 2);
		assert_eq!(service.stats().evictions, 0);

		// Frame 2 evicts frame 0 (the least recently used).
		service.request(time(2)).unwrap().unwrap();
		assert_eq!(service.lru_len(), 2, "capacity holds");
		assert_eq!(service.stats().evictions, 1);
		let decodes = service.stats().decodes;
		assert_eq!(decodes, 3);

		// Frame 0 was evicted: asking for it decodes again...
		service.request(time(0)).unwrap().unwrap();
		assert_eq!(service.stats().decodes, decodes + 1);
		assert_eq!(service.stats().lru_hits, 0);
		// ...while frame 2 (still cached) does not.
		service.request(time(2)).unwrap().unwrap();
		assert_eq!(service.stats().decodes, decodes + 1);
		assert_eq!(service.stats().lru_hits, 1);

		service.shutdown();
		let _ = std::fs::remove_file(&path);
	}

	/// The prefetch gate is consulted per message: a closed gate refuses
	/// (and counts) without queueing anything.
	#[test]
	fn prefetch_gate_refuses_and_recovers() {
		pin_legacy_working_space();
		let path = test_clip("gate");
		let open = Arc::new(AtomicBool::new(false));
		let gate_open = open.clone();
		let service = DecodeService::new(DECODE_LRU_CAP, Arc::new(move || {
			gate_open.load(Ordering::Relaxed)
		}));

		let req = request(&path, Rational::new(1, 10));
		assert!(!service.prefetch(req.clone()), "closed gate refuses");
		let stats = service.stats();
		assert_eq!(stats.prefetch_refused, 1);
		assert_eq!(stats.prefetches, 0);
		assert_eq!(stats.decodes, 0, "nothing was decoded behind the gate");

		open.store(true, Ordering::Relaxed);
		assert!(service.prefetch(req.clone()), "open gate accepts");
		assert!(service.wait_idle());
		let stats = service.stats();
		assert_eq!(stats.prefetches, 1);
		assert_eq!(stats.decodes, 1);

		service.shutdown();
		let _ = std::fs::remove_file(&path);
	}

	/// `wait_idle` is a real barrier through the command queue: every
	/// prefetch sent before it has been decoded when it returns.
	#[test]
	fn wait_idle_barrier_covers_queued_commands() {
		pin_legacy_working_space();
		let path = test_clip("barrier");
		let service = DecodeService::new(DECODE_LRU_CAP, always());
		for n in 0..4 {
			assert!(service.prefetch(request(&path, Rational::new(n, 10))));
		}
		assert!(service.wait_idle());
		let stats = service.stats();
		assert_eq!(stats.prefetches, 4);
		assert_eq!(stats.decodes, 4);
		assert_eq!(service.lru_len(), 4);
		service.shutdown();
		let _ = std::fs::remove_file(&path);
	}

	/// After shutdown the service reports itself gone (`None`), so the
	/// eval path falls back to decoding inline instead of failing frames.
	#[test]
	fn shutdown_makes_the_service_unavailable() {
		pin_legacy_working_space();
		let path = test_clip("shutdown");
		let service = DecodeService::new(DECODE_LRU_CAP, always());
		service.shutdown();
		service.shutdown(); // idempotent
		assert!(service.request(request(&path, Rational::new(0, 1))).is_none());
		assert!(!service.prefetch(request(&path, Rational::new(0, 1))));
		assert!(!service.wait_idle());
		let _ = std::fs::remove_file(&path);
	}
}
