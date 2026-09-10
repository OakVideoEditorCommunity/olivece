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

//! The process-isolated render backend (M15 S1): the main-process side
//! of the oak-worker pool — spawn, handshake, NDJSON control, shared
//! memory creation, crash detection and restart, and the ticket-facing
//! [`JobDispatch`] implementation (design doc §3.1–§3.4).
//!
//! ```text
//! TicketArena --Job--> ProcessDispatcher
//!                          |  scheduler.claim_batch (interleaved shards)
//!                          v
//!                WorkerHandle x N  ---- stdio NDJSON (control plane)
//!                  shm segment          render_batch { tickets, slots }
//!                  FrameSlotPool  <---> oak-worker process
//!                          |
//!                frame_ready(ticket, slot)
//!                          v
//!                Completion(Ok(TicketPayload::ShmFrame(ShmFrameRef)))
//! ```
//!
//! Model:
//!   - **Single-threaded control plane.** All dispatcher state lives in
//!     one mutex-guarded [`Inner`] pumped by [`ProcessDispatcher::poll`]
//!     (non-blocking try_recv + try_wait). The mutex guards control
//!     structures only — frame bytes never pass through it: workers
//!     write pixels straight into the shm slots and consumers read them
//!     from the mapping via [`ShmFrameRef`] (zero copy; the only
//!     counted copy path is [`ShmRegionView::slot_to_vec`]).
//!   - **Slot addressing.** The dispatcher assigns destination slots
//!     (main-side addressing, design §3.1); the worker renders into the
//!     given slot and publishes it through the ready ring. Free-slot
//!     bookkeeping mirrors the free SPSC ring in FIFO order, so the
//!     worker's `acquire` always pops exactly the assigned slot.
//!   - **Crash isolation.** Stdout EOF or a non-zero exit marks the
//!     worker dead: its claimed frames are re-queued to the scheduler
//!     (any healthy worker may claim them), the child is reaped, the
//!     segment recreated and the process respawned (bounded restarts).
//!   - **Where rendering runs.** Each `oak-worker` child is a
//!     single-threaded NDJSON loop (no render thread inside the worker):
//!     a `render_batch` message renders every ticket synchronously on the
//!     child's loop thread (`WorkerSession::handle_render_batch_stream`
//!     in `crates/oak-worker/src/worker.rs`). Parallelism comes from
//!     the pool of worker processes spawned here (`spawn_worker`),
//!     never from threads inside a worker — see "Where the rendering
//!     happens" in `crates/oak-worker/README.md`.
//!   - **S2 model.** The in-process [`crate::worker::WorkerPool`] is
//!     gone (M15 S2 mandate); [`crate::manager::RenderManager`] defaults
//!     to this backend. The ticket arena also routes **playback-window**
//!     frames here via [`JobSchedule::playback`], and the app pumps the
//!     control plane from the UI tick ([`ProcessDispatcher::poll`]) and
//!     from blocking ticket waits.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::ipc::{
	write_message, AudioTicketSpec, BatchAcceptedMsg, BatchTicketSpec, FrameFailedMsg, FrameReadyMsg,
	FrameSlotMeta, FrameSlotPool, HandshakeMsg, HelloCapsMsg, PluginProgressMsg, RenderAudioBatchMsg,
	RenderBatchMsg, SharedMemoryRegion, ShmMode, WireMontageClip, SLOT_FORMAT_BGRA8,
	TYPE_BATCH_ACCEPTED, TYPE_ERROR, TYPE_FRAME_FAILED, TYPE_FRAME_READY, TYPE_HANDSHAKE,
	TYPE_HELLO_CAPS, TYPE_PLUGIN_CANCEL, TYPE_PLUGIN_PROGRESS, TYPE_RENDER_AUDIO_BATCH,
	plugin_cancel_json,
};
use crate::scheduler::{FrameKey, FrameRequest, PreviewScheduler, SubmitOutcome};
use crate::ticket::{
	AudioSamples, AudioTicketParams, Completion, TicketPayload, TicketResult, VideoTicketParams,
};
use crate::worker::{Job, JobDispatch};

/// Protocol version spoken by the dispatcher (v1 base; v2 messages are
/// additive — the oak-worker handshake check stays `== 1`).
pub const DISPATCH_PROTOCOL_VERSION: i32 = 1;

/// Restart attempts per worker before its tickets fail permanently.
const MAX_RESTARTS: u32 = 5;

/// Maximum audio bytes a process-backend audio ticket may occupy in a shm
/// slot (M15 S3). Larger ranges (long exports) are refused by `post` so
/// the arena falls back to main-process inline rendering — a several-
/// minute export audio buffer does not need (and should not force) a
/// giant shared-memory segment. ~64 MB ≈ 2.9 min of 48 kHz stereo.
const MAX_AUDIO_SLOT_BYTES: usize = 64 * 1024 * 1024;

/// Legacy fixed default slots per worker (design §3.1 "8 slots starting").
/// The M15 S3 adaptive [`default_slots_per_worker`] policy supersedes it
/// for auto-configured dispatchers; kept as the documented starting point
/// and the cap for small frames.
pub const DEFAULT_SLOTS_PER_WORKER: u32 = 8;

/// Frame bytes copied into main-process heap buffers. The playback path
/// is zero-copy by construction (completions carry [`ShmFrameRef`]s,
/// never pixel `Vec`s); only [`ShmRegionView::slot_to_vec`] bumps this.
/// Tests assert it stays 0 on the preview path.
static MAIN_FRAME_COPIES: AtomicU64 = AtomicU64::new(0);

/// The main-process frame-copy counter (zero-copy assertion; design
/// §3.5).
pub fn main_heap_frame_copies() -> u64 {
	MAIN_FRAME_COPIES.load(Ordering::Relaxed)
}

/// Reset the copy counter (tests).
pub fn reset_main_heap_frame_copies() {
	MAIN_FRAME_COPIES.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Plugin-progress forwarding (worker -> main) and cancel broadcast
// ---------------------------------------------------------------------------

/// The app-facing plugin-progress callback: invoked by the dispatcher (on
/// the UI tick's poll) for every worker-forwarded `plugin_progress` line
/// (label, message, fraction). The app's [`crate::oakui::ofx`] wiring
/// forwards these into its `PluginProgressEvent` channel. `Arc` so the
/// registry can hand out cheap clones (the callback is `Fn`, not `Clone`).
pub type PluginProgressCb = Arc<dyn Fn(String, String, f64) + Send + Sync>;

static PLUGIN_PROGRESS_CB: OnceLock<Mutex<Option<PluginProgressCb>>> = OnceLock::new();

/// Register (or clear) the plugin-progress forwarding callback.
pub fn set_plugin_progress_cb(cb: Option<PluginProgressCb>) {
	*PLUGIN_PROGRESS_CB
		.get_or_init(|| Mutex::new(None))
		.lock()
		.unwrap_or_else(|e| e.into_inner()) = cb;
}

fn plugin_progress_cb() -> Option<PluginProgressCb> {
	PLUGIN_PROGRESS_CB
		.get_or_init(|| Mutex::new(None))
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Weak handle to the live dispatcher, registered by
/// [`ProcessDispatcher::new`] so the cancel broadcast can reach the
/// workers without threading a handle through the app.
static DISPATCHER: OnceLock<Mutex<Weak<ProcessDispatcher>>> = OnceLock::new();

fn dispatcher_slot() -> &'static Mutex<Weak<ProcessDispatcher>> {
	DISPATCHER.get_or_init(|| Mutex::new(Weak::new()))
}

/// Broadcast a `plugin_cancel` message to every alive worker: the user
/// cancelled the plugin render; the workers set their sticky cancel flag
/// and their live progress reporters answer false from then on (the
/// plugin aborts at its next progressUpdate). Falls back to a no-op when
/// no dispatcher is live (inline/test backends).
pub fn request_plugin_cancel_all() {
	let dispatcher = dispatcher_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.upgrade();
	if let Some(dispatcher) = dispatcher {
		dispatcher.broadcast_plugin_cancel();
	}
}

// ---------------------------------------------------------------------------
// ShmRegionView — one worker segment as seen from the main process
// ---------------------------------------------------------------------------

/// Owned view of the FrameSlotMeta currently in a slot (the shm POD
/// copied out, colorspace as a string).
#[derive(Clone, Debug, PartialEq)]
pub struct ShmFrameMeta {
	/// Caller tag (ticket id).
	pub id: i64,
	/// Frame timestamp numerator.
	pub time_num: i64,
	/// Frame timestamp denominator.
	pub time_den: i64,
	/// Frame width.
	pub width: i32,
	/// Frame height.
	pub height: i32,
	/// Slot wire format (`PixelFormat` int or [`SLOT_FORMAT_BGRA8`]).
	pub format: i32,
	/// Channel count.
	pub channel_count: i32,
	/// Bytes per scanline.
	pub linesize: i32,
	/// Valid bytes in the slot.
	pub data_size: i32,
	/// Input colorspace name.
	pub colorspace: String,
}

impl ShmFrameMeta {
	fn from_pod(pod: &FrameSlotMeta) -> ShmFrameMeta {
		let colorspace = {
			// SAFETY: the POD char array is NUL-padded by the worker.
			let cstr = unsafe { std::ffi::CStr::from_ptr(pod.colorspace.as_ptr()) };
			cstr.to_string_lossy().into_owned()
		};
		ShmFrameMeta {
			id: pod.id,
			time_num: pod.time_num,
			time_den: pod.time_den,
			width: pod.width,
			height: pod.height,
			format: pod.format,
			channel_count: pod.channel_count,
			linesize: pod.linesize,
			data_size: pod.data_size,
			colorspace,
		}
	}
}

/// A worker's shared-memory segment + frame-slot pool, owned by the
/// main process (creator side). Shared through an `Arc` so delivered
/// [`ShmFrameRef`]s keep the mapping alive across worker restarts.
pub struct ShmRegionView {
	region: SharedMemoryRegion,
	pool: FrameSlotPool,
}

// The segment mapping is usable from any local thread; cross-process
// synchronization lives in the rings' atomics.
unsafe impl Send for ShmRegionView {}
unsafe impl Sync for ShmRegionView {}

impl ShmRegionView {
	/// Create (and initialize) a segment of `slots` x `slot_bytes` under
	/// `key`. A stale segment under the same name (left by a crashed
	/// previous owner) is unlinked and the create retried once.
	fn create(key: &str, slots: u32, slot_bytes: usize) -> Result<Arc<ShmRegionView>> {
		let mut region = SharedMemoryRegion::new();
		let bytes = FrameSlotPool::bytes_needed(slots, slot_bytes);
		if !region.open(key, bytes, ShmMode::Create) {
			SharedMemoryRegion::unlink_key(key);
			if !region.open(key, bytes, ShmMode::Create) {
				return Err(Error::Failed(format!(
					"create shm segment {key}: {}",
					region.error()
				)));
			}
		}
		// SAFETY: `region` is a live mapping of exactly `bytes` bytes.
		let pool = unsafe { FrameSlotPool::create(region.data(), slots, slot_bytes) };
		Ok(Arc::new(ShmRegionView { region, pool }))
	}

	/// The segment key.
	pub fn key(&self) -> &str {
		self.region.key()
	}

	/// Slot count.
	pub fn slot_count(&self) -> u32 {
		self.pool.slot_count()
	}

	/// Per-slot data capacity.
	pub fn slot_data_bytes(&self) -> usize {
		self.pool.slot_data_bytes()
	}

	/// Zero-copy read of a slot's pixel block (borrowed from the live
	/// mapping; valid until this view drops).
	pub fn slot_bytes(&self, slot: u32) -> &[u8] {
		let len = self.pool.slot_data_bytes();
		// SAFETY: `slot` is in range for the pool's lifetime and the
		// mapping outlives &self.
		unsafe { std::slice::from_raw_parts(self.pool.slot_data_const(slot), len) }
	}

	/// Copy a slot's pixel block into a heap buffer (the one counted
	/// copy path — long-term caches that must outlive the slot).
	pub fn slot_to_vec(&self, slot: u32) -> Vec<u8> {
		MAIN_FRAME_COPIES.fetch_add(1, Ordering::Relaxed);
		self.slot_bytes(slot).to_vec()
	}

	/// The slot's metadata, copied out of shm.
	pub fn meta_copy(&self, slot: u32) -> ShmFrameMeta {
		// SAFETY: `slot` is in range; the meta POD is fully initialized
		// by the pool create/attach.
		let pod = unsafe { &*self.pool.meta_const(slot) };
		ShmFrameMeta::from_pod(pod)
	}

	/// The pool view (dispatcher ring operations).
	pub(crate) fn pool(&self) -> &FrameSlotPool {
		&self.pool
	}
}

/// Zero-copy handle to a rendered frame in a worker segment: what a
/// video ticket completion carries on the process backend. No frame
/// bytes travel inside — the consumer reads them from the mapping with
/// [`ShmRegionView::slot_bytes`] and releases the slot through
/// [`ProcessDispatcher::release_frame`] when done (slot release =
/// cache eviction, design §3.1).
#[derive(Clone)]
pub struct ShmFrameRef {
	/// Worker index owning the segment.
	pub worker: u32,
	/// Slot index in that segment.
	pub slot: u32,
	/// Frame metadata (copied at delivery).
	pub meta: ShmFrameMeta,
	/// The segment view (keeps the mapping alive).
	pub shm: Arc<ShmRegionView>,
}

impl std::fmt::Debug for ShmFrameRef {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ShmFrameRef")
			.field("worker", &self.worker)
			.field("slot", &self.slot)
			.field("meta", &self.meta)
			.finish()
	}
}

/// Zero-copy handle to rendered audio in a worker segment (M15 S3): what
/// an audio ticket completion carries on the process backend. The samples
/// live in the shm slot as little-endian interleaved f32 (wire format
/// [`crate::ipc::SLOT_FORMAT_AUDIO_F32`]); the consumer reads them with
/// [`ShmAudioRef::samples`] and releases the slot through
/// [`ProcessDispatcher::release_audio_frame`] when done. `sample_rate` /
/// `channel_layout` are carried from the ticket params (they are not part
/// of the slot meta POD).
#[derive(Clone)]
pub struct ShmAudioRef {
	/// Worker index owning the segment.
	pub worker: u32,
	/// Slot index in that segment.
	pub slot: u32,
	/// Slot metadata (format = `SLOT_FORMAT_AUDIO_F32`).
	pub meta: ShmFrameMeta,
	/// The segment view (keeps the mapping alive).
	pub shm: Arc<ShmRegionView>,
	/// Output sample rate (Hz; from the ticket params).
	pub sample_rate: i32,
	/// Output channel layout mask (from the ticket params).
	pub channel_layout: u64,
	/// Channel count (also in the slot meta).
	pub channel_count: i32,
}

impl ShmAudioRef {
	/// View the same slot as a generic [`ShmFrameRef`] (slot release paths
	/// that are shared with video frames).
	pub fn frame_ref(&self) -> ShmFrameRef {
		ShmFrameRef {
			worker: self.worker,
			slot: self.slot,
			meta: self.meta.clone(),
			shm: self.shm.clone(),
		}
	}

	/// Copy the interleaved f32 samples out of the slot (the counted
	/// copy path — audio bytes must outlive the slot to reach the output
	/// device / encoder).
	pub fn samples(&self) -> Vec<f32> {
		let bytes = self.shm.slot_bytes(self.slot);
		let valid = bytes
			.get(..self.meta.data_size.max(0) as usize)
			.unwrap_or(&[]);
		valid
			.chunks_exact(4)
			.map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
			.collect()
	}

	/// The decoded [`AudioSamples`] (sample rate / layout from the ticket
	/// params, not the slot).
	pub fn to_audio_samples(&self) -> AudioSamples {
		AudioSamples {
			samples: self.samples(),
			sample_rate: self.sample_rate,
			channel_layout: self.channel_layout,
			channel_count: self.channel_count,
		}
	}
}

impl std::fmt::Debug for ShmAudioRef {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ShmAudioRef")
			.field("worker", &self.worker)
			.field("slot", &self.slot)
			.field("sample_rate", &self.sample_rate)
			.field("channel_count", &self.channel_count)
			.finish()
	}
}

// ---------------------------------------------------------------------------
// Slot pixel conversions (M15 S2)
// ---------------------------------------------------------------------------
//
// The process backend writes BGRA8 into slots (the viewer preview format).
// Long-lived consumers that need the bytes in a different order/format
// convert once after copying out of the slot.

/// Convert a BGRA8 block into an RGBA8 block (swapping R and B). Used by
/// PNG writers (footage thumbnails) and PPM/CLI output, which require
/// RGB-order buffers. `src.len()` must be a multiple of 4.
pub fn bgra8_to_rgba8(src: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(src.len());
	for px in src.chunks_exact(4) {
		out.push(px[2]); // R
		out.push(px[1]); // G
		out.push(px[0]); // B
		out.push(px[3]); // A
	}
	out
}

/// Convert a BGRA8 block into tightly-packed F32 RGBA samples (`0..=1`).
/// Used by the export/encoder path, which declares F32 input: the worker
/// converts its F32 pipeline output to BGRA8 for the slot (design §3.1),
/// and the export converts back — a necessary conversion at the encoder
/// boundary with 8-bit quantization (S2; per-ticket slot formats are S3
/// work).
pub fn bgra8_to_f32_rgba(src: &[u8]) -> Vec<f32> {
	let mut out = Vec::with_capacity(src.len() / 4 * 4);
	for px in src.chunks_exact(4) {
		out.push(f32::from(px[2]) / 255.0); // R
		out.push(f32::from(px[1]) / 255.0); // G
		out.push(f32::from(px[0]) / 255.0); // B
		out.push(f32::from(px[3]) / 255.0); // A
	}
	out
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Bytes one slot needs for `width` x `height` at a wire format
/// ([`oak_core::PixelFormat`] int or [`SLOT_FORMAT_BGRA8`]).
pub fn slot_bytes_for(width: i32, height: i32, format: i32) -> usize {
	let pixels = (width.max(0) as usize).saturating_mul(height.max(0) as usize);
	let bytes_per_pixel = if format == SLOT_FORMAT_BGRA8 {
		4
	} else {
		let fmt = match format {
			0 => oak_core::PixelFormat::U8,
			1 => oak_core::PixelFormat::U10,
			2 => oak_core::PixelFormat::U16,
			3 => oak_core::PixelFormat::F16,
			4 => oak_core::PixelFormat::F32,
			_ => oak_core::PixelFormat::F32,
		};
		fmt.bytes_per_channel() * 4
	};
	pixels.saturating_mul(bytes_per_pixel)
}

/// The worker-count policy (design doc S1 item 3):
/// `max(1, min(logical_cores - 2, memory_budget / per_worker_slots))`,
/// with a memory budget of one quarter of physical RAM.
pub fn default_worker_count(slots_per_worker: u32, slot_bytes: usize) -> usize {
	let cores = std::thread::available_parallelism()
		.map(|n| n.get())
		.unwrap_or(4);
	let by_cores = cores.saturating_sub(2).max(1);
	let mem = physical_memory_bytes().unwrap_or(8u64 << 30);
	let budget = mem / 4;
	let per_worker = (slots_per_worker as usize).saturating_mul(slot_bytes).max(1);
	let by_mem = (budget as usize / per_worker).max(1);
	by_cores.min(by_mem).max(1)
}

/// Per-worker GPU budget at `frame_size` / `fps` — the surface + render
/// chain's peak vram, scaled from the 1080p figure: 1 GiB peak at
/// 1080p24 (NVDEC surface pool + uploads + render targets), scaled by
/// pixel ratio and a sqrt-fps factor (higher rates hold more surfaces in
/// flight), plus a 256 MiB idle floor the worker never releases.
/// Exposed for the budget test (the exact formula is observable).
fn per_worker_gpu_budget(frame_size: (i32, i32), fps: u32) -> u64 {
	let pixels = (frame_size.0.max(0) as f64) * (frame_size.1.max(0) as f64);
	let pixel_ratio = pixels / (1920.0 * 1080.0);
	let fps_factor = (fps.max(1) as f64 / 24.0).sqrt().max(1.0);
	// Peak-vs-quiet spread: a worker sustains a NVDEC session (surface
	// pool + an in-flight frame upload) plus the wgpu pipeline and a
	// couple of montage uploads. The 1 GiB 1080p / 4 GiB 4K estimates
	// have been shown to still exhaust a 16 GB card when the pool pushes
	// them simultaneously on several sources (the CUDA_ERROR_OUT_OF_MEMORY
	// mid-playback flood), so the budget includes an extra headroom
	// factor for undetected spikes (shared decoder surface growth during
	// long GOP scans at a keyframe miss).
	let headroom = 2;
	let peak = (1u64 << 30) * headroom;
	let idle = 256u64 << 20;
	((peak as f64 * pixel_ratio * fps_factor + idle as f64) as u64).max(1)
}

/// The GPU-vram side of the worker-count policy, combined with the
/// RAM/CPU policy by [`worker_count_for_size`].
///
/// Each worker process decodes with a hardware device when available
/// (NVDEC/VAAPI/VideoToolbox — the codec crate's mandated default), and
/// a hardware decoder pins GPU video memory: an NVDEC 1080p decoder
/// holds ~10 surface frames of `1920×1080×1.5` ≈ 30 MB plus the
/// pipeline's uploads/render targets, ~1 GiB at peak per worker with
/// the montage/render chain; at 4K the same chain scales by pixel
/// count → ~4 GiB. The pool must fit in the GPU's *free* memory with a
/// 10% emergency reserve, or a resolution switch to 4K exhausts the
/// device and every decoder open starts failing (`cuvidCreateDecoder`
/// CUDA_ERROR_OUT_OF_MEMORY — the log flood).
///
/// Returns `Some(max_workers)` when a GPU is present and its free video
/// memory can be queried, `None` when there is no GPU / no usable query
/// (the caller then relies on the CPU/RAM policy alone).
pub fn gpu_worker_capacity(frame_size: (i32, i32), fps: u32) -> Option<usize> {
	let (free_bytes, _total_bytes) = gpu_vram_bytes()?;
	// 10% emergency reserve: never plan to burn the last frame of the
	// device (other processes, the compositor and the worker's own
	// startup allocations live there too).
	let usable = (free_bytes.saturating_mul(9)) / 10;
	let budget = per_worker_gpu_budget(frame_size, fps);
	Some((usable / budget).max(1) as usize)
}

/// Worker count combining the existing core/RAM policy with the GPU-vram
/// policy: the GPU bound applies only when hardware decoding is actually
/// enabled (no GPU → pure CPU/RAM), and caps the result (never raises it
/// beyond the RAM/core policy — the shm segments are system memory).
pub fn worker_count_for_size(
	slots_per_worker: u32,
	slot_bytes: usize,
	frame_size: (i32, i32),
	fps: u32,
) -> usize {
	let base = default_worker_count(slots_per_worker, slot_bytes);
	if !hwdecode_available() {
		return base;
	}
	match gpu_worker_capacity(frame_size, fps) {
		Some(gpu_cap) => base.min(gpu_cap).max(1),
		None => base,
	}
}

/// Whether hardware decoding can engage (the codec crate's switch: the
/// process-wide config key `HardwareDecoding`, with the
/// `OAK_HWACCEL=0` escape hatch). Only when hardware is on does the
/// worker pool need the GPU-vram budget.
pub(crate) fn hwdecode_available() -> bool {
	oak_codec::hwdecode::hardware_decoding_enabled()
}

/// Free / total GPU video memory in bytes, queried per platform/vendor:
///
/// 1. **NVIDIA** (the NVDEC acceleration path): `nvidia-smi
///    --query-gpu=memory.free,memory.total` — available on every
///    NVIDIA-equipped machine regardless of OS (the NVIDIA driver ships
///    `nvidia-smi` on Linux, Windows and Intel-Mac alike), returns MiB.
/// 2. **AMD / Intel on Linux**: the DRM `mem_info_vram_*` sysfs
///    attributes (`/sys/class/drm/card*/device/mem_info_vram_total` and
///    `mem_info_vram_used`) — provided by the amdgpu, i915 and Xe
///    kernel drivers, queried without any driver CLI. Free = total −
///    used. The first card that reports a non-zero total wins (the
///    primary device; on iGPU+discrete boxes the discrete card has the
///    vram the decode chain uses).
/// 3. **Apple Silicon (macOS)**: unified memory — there is NO separate
///    GPU vram to exhaust; the GPU's decode surfaces and render targets
///    live in the same physical RAM the CPU uses. The RAM/4 budget in
///    [`default_worker_count`] IS the correct vram bound on UMA, so no
///    query here (an explicit `ram_budget` would double-count the same
///    pool). Intel Macs with a discrete NVIDIA GPU take path 1; an AMD
///    eGPU on macOS falls back to the RAM policy (the eGPU's own vram
///    cannot be split from the host pool anyway — its decode surfaces
///    also spill into system RAM on UMA-ish forwarding).
/// 4. **Windows AMD / Intel**: no standard CLI exists (DXGI
///    `QueryVideoMemoryInfo` is the real API, not exposed by wgpu);
///    without a query the fallback RAM policy is SAFE-side —
///    underestimating the pool only loses throughput, never OOMs the
///    device. AMD/Intel discrete graphics on Windows ship with sensible
///    system ram; `physical_memory_bytes/4` under-covers their vram, and
///    the worker threads bound by cores anyway.
///
/// Returns `None` when no query path matched (headless / no GPU /
/// missing support): the caller falls back to the pure RAM policy.
pub fn gpu_vram_bytes() -> Option<(u64, u64)> {
	// NVIDIA first: the query is precise (free, not total−used) and
	// covers the discrete GPU the decode chain prefers on multi-GPU
	// boxes.
	if let Some(v) = nvidia_vram_bytes() {
		return Some(v);
	}
	// AMD / Intel: sysfs DRM attrs.
	#[cfg(target_os = "linux")]
	if let Some(v) = linux_drm_vram_bytes() {
		return Some(v);
	}
	// macOS: unified memory (see the type comment) — the RAM budget is
	// the correct bound. Windows AMD/Intel: no portable query; the RAM
	// policy is safe-side. Both: None.
	None
}

/// NVIDIA vram pair (`(free, total)` bytes) via `nvidia-smi`.
fn nvidia_vram_bytes() -> Option<(u64, u64)> {
	let output = std::process::Command::new("nvidia-smi")
		.args([
			"--query-gpu=memory.free,memory.total",
			"--format=csv,noheader,nounits",
		])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let text = String::from_utf8(output.stdout).ok()?;
	// First line: "16155, 24576" (MiB). Negative values mean "unknown"
	// on some drivers — treat as no query.
	let (free, total) = text.lines().next()?.trim().split_once(',')?;
	let free: i64 = free.trim().parse().ok()?;
	let total: i64 = total.trim().parse().ok()?;
	if free < 0 || total <= 0 {
		return None;
	}
	Some(((free as u64) << 20, (total as u64) << 20))
}

/// AMD / Intel vram pair on Linux: the DRM `mem_info_vram_*` sysfs
/// attributes looped over the cards, first non-zero total wins.
#[cfg(target_os = "linux")]
fn linux_drm_vram_bytes() -> Option<(u64, u64)> {
	linux_drm_vram_bytes_from(std::path::Path::new("/sys/class/drm"))
}

/// The sysfs walk behind [`linux_drm_vram_bytes`], split out so tests
/// can point it at a fixture directory.
#[cfg(target_os = "linux")]
fn linux_drm_vram_bytes_from(dir: &std::path::Path) -> Option<(u64, u64)> {
	// The attribute files are device-tree attachments under card-N:
	//   /sys/class/drm/card<idx>/device/mem_info_vram_total
	//   /sys/class/drm/card<idx>/device/mem_info_vram_used
	// Both are plain decimal byte counts (amdgpu, i915, Xe). A card
	// whose driver does not expose them (e.g. a static display device,
	// or a card with no render node) has no mem_info_vram_total —
	// `read_to_string` fails and the loop moves on.
	let entries = std::fs::read_dir(dir).ok()?;
	let mut cards: Vec<u32> = entries
		.filter_map(|e| e.ok())
		.filter_map(|e| {
			let name = e.file_name();
			let name = name.to_str()?;
			name.strip_prefix("card")?.parse::<u32>().ok()
		})
		.collect();
	cards.sort_unstable();
	cards.dedup();
	for card in cards {
		let base = dir.join(format!("card{card}")).join("device");
		// A card without the attrs (or a transient read error) skips to
		// the next one — one broken card never aborts the whole walk.
		let Ok(total_text) = std::fs::read_to_string(base.join("mem_info_vram_total")) else {
			continue;
		};
		let Ok(total) = total_text.trim().parse::<u64>() else {
			continue;
		};
		if total == 0 {
			continue;
		}
		let used: u64 = std::fs::read_to_string(base.join("mem_info_vram_used"))
			.ok()
			.and_then(|s| s.trim().parse().ok())
			.unwrap_or(0);
		let free = total.saturating_sub(used);
		return Some((free, total));
	}
	None
}

/// Android / non-Linux non-NVIDIA: no query (see [`gpu_vram_bytes`]).
#[cfg(not(target_os = "linux"))]
fn linux_drm_vram_bytes() -> Option<(u64, u64)> {
	None
}

/// Slot-count policy when a segment grows (M15 S3 grow-on-demand): cap
/// the per-worker segment memory at `GROWN_SEGMENT_BUDGET`, never drop
/// below 2 slots (enough to keep a worker flowing), never exceed the
/// current count.
pub fn default_slots_for_bytes(slot_bytes: usize, current_slots: u32) -> u32 {
	/// Per-worker segment budget for a grown segment (256 MiB).
	const GROWN_SEGMENT_BUDGET: usize = 256 * 1024 * 1024;
	let by_mem = (GROWN_SEGMENT_BUDGET / slot_bytes.max(1)).max(2) as u32;
	by_mem.min(current_slots).max(2)
}

/// Default slots per worker segment (M15 S3 adaptive policy): the
/// segment is sized so per-worker shared memory stays within
/// `DEFAULT_SEGMENT_BUDGET` (128 MiB), bounded to `[2, 8]`. Small preview
/// frames (BGRA8 1080p ≈ 8.3 MB) get the full 8 slots (~66 MB); F32 1080p
/// (≈ 33 MB) drops to 3; F32 4K (≈ 132 MB) to 2. The worker-count policy
/// then bounds the whole pool against RAM/4.
pub fn default_slots_per_worker(slot_bytes: usize) -> u32 {
	const DEFAULT_SEGMENT_BUDGET: usize = 128 * 1024 * 1024;
	const MIN_SLOTS: u32 = 2;
	const MAX_SLOTS: u32 = 8;
	((DEFAULT_SEGMENT_BUDGET / slot_bytes.max(1)).max(MIN_SLOTS as usize) as u32)
		.clamp(MIN_SLOTS, MAX_SLOTS)
}

/// Default batch size B (M15 S3 adaptive policy): the design figure
/// `120 / workers` (a full playback pre-render window split across the
/// pool), capped at the per-worker slot count — credit caps a batch at
/// the free slots anyway, so a B larger than the slots just wastes a
/// claim round trip.
pub fn default_batch_size(workers: usize, slots: u32) -> usize {
	let design = (120 / workers.max(1)).max(1);
	design.min(slots.max(1) as usize)
}

/// Physical memory in bytes (macOS `hw.memsize`, Linux `sysconf`,
/// Windows `GlobalMemoryStatusEx`).
fn physical_memory_bytes() -> Option<u64> {
	#[cfg(target_os = "macos")]
	{
		let mut size: u64 = 0;
		let mut len = std::mem::size_of::<u64>();
		let name = b"hw.memsize\0";
		let rc = unsafe {
			libc::sysctlbyname(
				name.as_ptr() as *const libc::c_char,
				&mut size as *mut u64 as *mut libc::c_void,
				&mut len,
				std::ptr::null_mut(),
				0,
			)
		};
		if rc == 0 {
			Some(size)
		} else {
			None
		}
	}
	#[cfg(target_os = "linux")]
	{
		unsafe {
			let pages = libc::sysconf(libc::_SC_PHYS_PAGES);
			let page = libc::sysconf(libc::_SC_PAGESIZE);
			if pages > 0 && page > 0 {
				Some(pages as u64 * page as u64)
			} else {
				None
			}
		}
	}
	#[cfg(target_os = "windows")]
	{
		// GlobalMemoryStatusEx (kernel32): ullTotalPhys.
		#[repr(C)]
		struct MemoryStatusEx {
			length: u32,
			memory_load: u32,
			total_phys: u64,
			avail_phys: u64,
			total_page_file: u64,
			avail_page_file: u64,
			total_virtual: u64,
			avail_virtual: u64,
			avail_extended_virtual: u64,
		}
		#[link(name = "kernel32")]
		unsafe extern "system" {
			fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
		}
		let mut status = MemoryStatusEx {
			length: std::mem::size_of::<MemoryStatusEx>() as u32,
			memory_load: 0,
			total_phys: 0,
			avail_phys: 0,
			total_page_file: 0,
			avail_page_file: 0,
			total_virtual: 0,
			avail_virtual: 0,
			avail_extended_virtual: 0,
		};
		let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
		if ok != 0 && status.total_phys > 0 {
			Some(status.total_phys)
		} else {
			None
		}
	}
	#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
	{
		None
	}
}

/// Dispatcher configuration.
#[derive(Clone, Debug)]
pub struct DispatcherConfig {
	/// Path to the oak-worker binary. `None` = `$OAK_WORKER_BIN`, else
	/// `oak-worker` next to the current executable.
	pub worker_bin: Option<PathBuf>,
	/// Worker process count. `0` = the [`default_worker_count`] policy.
	pub workers: usize,
	/// Output slots per worker segment. `0` = the
	/// [`default_slots_per_worker`] policy (adaptive to the frame size).
	pub slots_per_worker: u32,
	/// Frame width of the segment geometry. `0` = 1920.
	pub width: i32,
	/// Frame height of the segment geometry. `0` = 1080.
	pub height: i32,
	/// Slot wire format: an `oak_core::PixelFormat` int or
	/// [`SLOT_FORMAT_BGRA8`]. Default BGRA8 (the viewer preview path).
	pub slot_format: i32,
	/// Batch size `B`. `0` = the [`default_batch_size`] policy (adaptive
	/// to workers and slots).
	pub batch_size: usize,
	/// Graph snapshot path sent to every worker via `load_graph` after
	/// the handshake (`None` = no graph).
	pub graph_snapshot: Option<String>,
	/// Handshake timeout per (re)spawn.
	pub handshake_timeout_ms: u64,
}

impl Default for DispatcherConfig {
	fn default() -> Self {
		Self {
			worker_bin: None,
			workers: 0,
			slots_per_worker: 0,
			width: 0,
			height: 0,
			slot_format: SLOT_FORMAT_BGRA8,
			batch_size: 0,
			graph_snapshot: None,
			handshake_timeout_ms: 10_000,
		}
	}
}

impl DispatcherConfig {
	fn normalize(&self) -> DispatcherConfig {
		// Only the geometry defaults are resolved here; the adaptive
		// policies (workers / slots / batch size) are resolved in
		// `ProcessDispatcher::new` where slot_bytes is known.
		let mut c = self.clone();
		c.width = if c.width == 0 { 1920 } else { c.width };
		c.height = if c.height == 0 { 1080 } else { c.height };
		c
	}
}

// ---------------------------------------------------------------------------
// WorkerHandle
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerState {
	/// Spawned, handshake in flight.
	Starting,
	/// Handshaken, accepting batches.
	Alive,
	/// Exited / EOF detected; a restart is pending.
	Dead,
	/// Restart budget exhausted; tickets fail permanently.
	PermanentlyDead,
}

enum WorkerEvent {
	Line {
		worker: usize,
		generation: u64,
		line: String,
	},
	Eof {
		worker: usize,
		generation: u64,
	},
}

struct WorkerHandle {
	index: usize,
	/// Spawn generation (increments on every restart): reader-thread
	/// events carry the generation of the child they read from, so a
	/// late EOF from a dead child cannot kill its replacement.
	generation: u64,
	state: WorkerState,
	child: Option<Child>,
	stdin: Option<std::process::ChildStdin>,
	shm: Arc<ShmRegionView>,
	/// Current per-slot data capacity (grows on demand, M15 S3: a ticket
	/// requesting F32 or a larger frame rebuilds the segment first).
	slot_bytes: usize,
	/// FIFO mirror of the shm free ring's contents (the credit).
	free_slots: VecDeque<u32>,
	/// Dispatched ticket -> assigned slot (awaiting frame_ready).
	outstanding: HashMap<i64, u32>,
	/// Slots delivered to consumers, awaiting release_frame.
	held: HashSet<u32>,
	startup_seen: bool,
	graph_sent: bool,
	caps: Option<HelloCapsMsg>,
	restarts: u32,
	spawned_at: Instant,
	accepted_batches: u64,
	/// True between a segment grow (M15 S3) and the worker's hello_caps
	/// re-attach: the dispatcher must not send new batches while the worker
	/// is still attached to the old pool.
	reconfiguring: bool,
	/// True once the pool shrank below this worker: it no longer claims
	/// new batches; when `outstanding` drains empty the dispatcher sends
	/// the shutdown signal and reaps the worker's natural exit (never a
	/// mid-work kill).
	retiring: bool,
	/// When the retirement shutdown signal was sent (hang deadline
	/// anchor). `None` until signalled.
	retire_sent_at: Option<Instant>,
}

impl WorkerHandle {
	fn shell(
		index: usize,
		generation: u64,
		shm: Arc<ShmRegionView>,
		slots: u32,
		slot_bytes: usize,
	) -> WorkerHandle {
		WorkerHandle {
			index,
			generation,
			state: WorkerState::Starting,
			child: None,
			stdin: None,
			shm,
			slot_bytes,
			free_slots: (0..slots).collect(),
			outstanding: HashMap::new(),
			held: HashSet::new(),
			startup_seen: false,
			graph_sent: false,
			caps: None,
			restarts: 0,
			spawned_at: Instant::now(),
			accepted_batches: 0,
			reconfiguring: false,
			retiring: false,
			retire_sent_at: None,
		}
	}
}

// ---------------------------------------------------------------------------
// ProcessDispatcher
// ---------------------------------------------------------------------------

struct PendingTicket {
	key: FrameKey,
	params: Arc<VideoTicketParams>,
	/// Audio ticket params when this ticket is an audio range pull (M15
	/// S3); `None` for video tickets.
	audio: Option<Arc<AudioTicketParams>>,
	done: Option<Completion>,
}

struct Inner {
	config: DispatcherConfig,
	bin: PathBuf,
	slots: u32,
	slot_bytes: usize,
	workers: Vec<WorkerHandle>,
	scheduler: PreviewScheduler<i64>,
	tickets: HashMap<i64, PendingTicket>,
	next_ticket: i64,
	events_rx: mpsc::Receiver<WorkerEvent>,
	events_tx: mpsc::Sender<WorkerEvent>,
	/// Segment rebuild generation (M15 S3 grow-on-demand geometry): bumped
	/// on every per-worker segment resize so re-created segments never
	/// reuse the name of a live mapping.
	seg_generation: u64,
	/// The pool's target worker count (the sharding modulus the scheduler
	/// uses; the workers VECTOR may be larger while a shrink is draining
	/// its tail). Managed by [`ProcessDispatcher::set_target_workers`]:
	/// grows spawn new workers, shrinks retire the tail ones once their
	/// in-flight batch drains.
	target_workers: usize,
	/// Timestamp of the last applied worker-count resize (the throttle
	/// anchor of [`ProcessDispatcher::set_target_workers`]).
	last_resize_at: Option<Instant>,
	/// A target requested inside the throttle window — applied by the
	/// pump when the interval has passed (the LATEST of a burst wins).
	next_target: Option<usize>,
	started: bool,
	shutting_down: bool,
}

/// Minimum interval between applied pool resizes. A resolution switch
/// (or preview-size flapping) can fire `set_target_workers` on every
/// frame while 4K loads; each resize takes real time (spawn a worker
/// process, or drain + exit one) — thrashing it cancels out the
/// throughput a resize was meant to buy. Requests inside the window
/// merge into the target; the pump applies the decision once.
const MIN_RESIZE_INTERVAL: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The process-isolated dispatcher (design doc §2 ProcessDispatcher).
/// Cloning shares one dispatcher; the control plane is a single mutex
/// pumped by [`ProcessDispatcher::poll`] — frame bytes never touch it.
pub struct ProcessDispatcher {
	inner: Mutex<Inner>,
}

impl ProcessDispatcher {
	/// Build a dispatcher from `config` (does not spawn; call
	/// [`ProcessDispatcher::start`]). The adaptive policies resolve here
	/// (M15 S3): slots scale with the frame's slot bytes, workers with
	/// cores/RAM, batch size with workers and slots.
	pub fn new(config: DispatcherConfig) -> Result<Arc<ProcessDispatcher>> {
		let config = config.normalize();
		let slot_bytes = slot_bytes_for(config.width, config.height, config.slot_format);
		let slots = if config.slots_per_worker == 0 {
			default_slots_per_worker(slot_bytes)
		} else {
			config.slots_per_worker
		};
		let workers = if config.workers == 0 {
			// GPU-vram aware: the vram budget caps the CPU/RAM policy when
			// hardware decoding is on (each worker pins NVDEC surface
			// pools + render targets; a 4K pool that ignores vram exhausts
			// the device and floods the log with cuvidCreateDecoder OOM).
			worker_count_for_size(slots, slot_bytes, (config.width, config.height), 30)
		} else {
			config.workers
		};
		let batch_size = if config.batch_size == 0 {
			default_batch_size(workers, slots)
		} else {
			config.batch_size
		};
		let bin = resolve_worker_bin(&config)?;
		let (events_tx, events_rx) = mpsc::channel();
		let dispatcher = Arc::new(ProcessDispatcher {
			inner: Mutex::new(Inner {
				config,
				bin,
				slots,
				slot_bytes,
				workers: Vec::new(),
				scheduler: PreviewScheduler::new(workers, batch_size),
				tickets: HashMap::new(),
				next_ticket: 1,
				events_rx,
				events_tx,
				seg_generation: 0,
				target_workers: workers,
				last_resize_at: None,
				next_target: None,
				started: false,
				shutting_down: false,
			}),
		});
		// Register the weak handle for the plugin-cancel broadcast.
		*dispatcher_slot()
			.lock()
			.unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&dispatcher);
		Ok(dispatcher)
	}

	/// Spawn all workers and wait for the handshakes (bounded by
	/// `handshake_timeout_ms`).
	pub fn start(&self) -> Result<()> {
		let timeout = {
			let mut inner = lock(&self.inner);
			if inner.started {
				return Err(Error::State);
			}
			inner.started = true;
			let count = inner.scheduler.workers();
			for i in 0..count {
				self.spawn_worker(&mut inner, i)?;
			}
			Duration::from_millis(inner.config.handshake_timeout_ms)
		};
		let deadline = Instant::now() + timeout;
		loop {
			self.poll();
			{
				let inner = lock(&self.inner);
				if inner
					.workers
					.iter()
					.all(|w| matches!(w.state, WorkerState::Alive))
				{
					return Ok(());
				}
				if inner
					.workers
					.iter()
					.any(|w| matches!(w.state, WorkerState::PermanentlyDead))
				{
					return Err(Error::Failed("worker failed to start permanently".into()));
				}
			}
			if Instant::now() > deadline {
				return Err(Error::Failed(
					"render workers did not finish the startup handshake in time".into(),
				));
			}
			std::thread::sleep(Duration::from_millis(2));
		}
	}

	/// Slot headroom for best-effort pre-render windows: the pool's total
	/// slots minus one per worker, so interactive (seek / synchronous
	/// display) and audio tickets always keep credit to dispatch. A
	/// pre-render window larger than the pool exhausted every slot, which
	/// deadlocked the UI's synchronous frame wait (the playback freeze).
	///
	/// The count is over ALIVE workers: with the configured count a window
	/// opened during worker startup (or after a crash) could still claim
	/// every slot of the smaller live pool — the same deadlock, just
	/// timing-dependent (seen as the intermittent Linux CI hang in
	/// playback_display_tracks_the_playhead).
	pub fn preview_window_capacity(&self) -> usize {
		let inner = lock(&self.inner);
		let alive = inner
			.workers
			.iter()
			.filter(|w| matches!(w.state, WorkerState::Alive))
			.count();
		// Nobody alive yet: nothing can be claimed right now anyway, so
		// reporting the configured pool keeps the window building instead
		// of stalling at the 1-frame floor.
		let workers = if alive == 0 {
			inner.scheduler.workers()
		} else {
			alive
		};
		workers
			.saturating_mul(inner.slots as usize)
			.saturating_sub(workers)
			.max(1)
	}

	/// The configured worker count.
	pub fn worker_count(&self) -> usize {
		lock(&self.inner).scheduler.workers()
	}

	/// Per-worker slot count (the segment geometry the workers run).
	pub fn slots_per_worker(&self) -> u32 {
		lock(&self.inner).slots
	}

	/// The segment's per-slot byte capacity.
	pub fn slot_bytes(&self) -> usize {
		lock(&self.inner).slot_bytes
	}

	/// The slot wire format (an `oak_core::PixelFormat` int or
	/// [`SLOT_FORMAT_BGRA8`]).
	pub fn slot_format(&self) -> i32 {
		lock(&self.inner).config.slot_format
	}

	/// Dynamically resize the pool to `target` workers (a resolution
	/// switch changed the GPU-vram budget: 1080p runs the CPU-bound pool,
	/// 4K a vram-bounded fraction — see [`gpu_worker_capacity`]).
	///
	/// Grow: spawn the missing workers immediately (they join the shard
	/// set when they handshake). Shrink: the tail workers stop claiming
	/// and are shut down once their in-flight batch drains; the
	/// scheduler's modulus follows the TARGET immediately, so pending
	/// frames re-shard onto the surviving workers from their next claim.
	///
	/// **Throttled**: a resolution switch can fire this once per frame
	/// while 4K footage loads (each resize spawns/kills processes — a
	/// real 100 ms+ each). Changes within [`MIN_RESIZE_INTERVAL`] of the
	/// previous resize update the pending target only; the pool itself
	/// resizes on the next poll after the interval. An in-flight resize
	/// is never interrupted mid-drain: hitting the interval is what
	/// decides, so no churn.
	///
	/// Idempotent; no-op when the pool is not started.
	pub fn set_target_workers(&self, target: usize) {
		let target = target.max(1);
		let mut inner = lock(&self.inner);
		if !inner.started || inner.shutting_down {
			return;
		}
		// Throttle window: same-or-newer target within the interval is
		// merged into `next_target`; the pump applies it when the window
		// has passed (and applies the LATEST target — the final decision
		// of a burst, not an intermediate one).
		let now = Instant::now();
		if let Some(last) = inner.last_resize_at {
			if now.duration_since(last) < MIN_RESIZE_INTERVAL {
				if inner.target_workers != target {
					inner.next_target = Some(target);
				}
				return;
			}
		}
		self.apply_target_workers(&mut inner, target, now);
	}

	/// Immediately resize to `target` (the pump's throttled-apply path
	/// calls this too). Skipped when the target equals the current one.
	fn apply_target_workers(&self, inner: &mut Inner, target: usize, now: Instant) {
		let target = target.max(1);
		if inner.target_workers == target {
			inner.last_resize_at = Some(now);
			inner.next_target = None;
			return;
		}
		if target > inner.workers.len() {
			for i in inner.workers.len()..target {
				if let Err(e) = self.spawn_worker(inner, i) {
					eprintln!("procpool: pool grow spawn worker {i} failed: {e}");
					break;
				}
			}
		} else {
			// Shrink: retire the tail. The scheduler re-shards at the
			// target NOW (frames not yet claimed follow the new modulus);
			// the draining workers finish what they hold — a worker
			// rendering the pre-render window's FUTURE frames is NOT
			// killed mid-flight; it drains its outstanding batch first,
			// then exits naturally on the shutdown signal (pump step 4).
			for handle in inner.workers.iter_mut().skip(target) {
				if matches!(handle.state, WorkerState::Alive | WorkerState::Starting) {
					handle.retiring = true;
				}
			}
		}
		inner.target_workers = target;
		inner.scheduler.set_worker_count(target);
		inner.last_resize_at = Some(now);
		inner.next_target = None;
	}

	/// True when worker `i` is alive (handshaken).
	pub fn is_alive(&self, worker: usize) -> bool {
		lock(&self.inner)
			.workers
			.get(worker)
			.map(|w| matches!(w.state, WorkerState::Alive))
			.unwrap_or(false)
	}

	/// Restart count of worker `i` (crash-isolation metric).
	pub fn restarts_of(&self, worker: usize) -> u32 {
		lock(&self.inner)
			.workers
			.get(worker)
			.map(|w| w.restarts)
			.unwrap_or(0)
	}

	/// Batches accepted by worker `i` (claim-confirmation metric).
	pub fn accepted_batches_of(&self, worker: usize) -> u64 {
		lock(&self.inner)
			.workers
			.get(worker)
			.map(|w| w.accepted_batches)
			.unwrap_or(0)
	}

	/// The segment view of worker `i` (tests / S2 cache integration).
	pub fn shm_of(&self, worker: usize) -> Option<Arc<ShmRegionView>> {
		lock(&self.inner).workers.get(worker).map(|w| w.shm.clone())
	}

	/// Pump the control plane: drain worker events, restart the dead,
	/// claim + dispatch batches. Non-blocking; call from the UI tick (or
	/// after any submit/release). Completions fire after the lock drops.
	/// A no-op once shutting down (M16 S1: a stale poll must not restart
	/// dead workers during/after teardown).
	pub fn poll(&self) {
		let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
		{
			let mut inner = lock(&self.inner);
			if inner.shutting_down {
				return;
			}
			self.pump(&mut inner, &mut fired);
		}
		for (done, result) in fired {
			done(result);
		}
	}

	/// Release a consumed frame back to its worker's free pool (slot
	/// release = cache eviction). Stale refs (worker restarted since)
	/// are ignored — their segment is already gone.
	pub fn release_frame(&self, frame: &ShmFrameRef) {
		let mut inner = lock(&self.inner);
		let Some(handle) = inner.workers.get_mut(frame.worker as usize) else {
			return;
		};
		if !Arc::ptr_eq(&handle.shm, &frame.shm) {
			return; // stale ref: the segment was recreated
		}
		if !handle.held.remove(&frame.slot) {
			return; // double release
		}
		handle.free_slots.push_back(frame.slot);
		// SAFETY: the pool is a live view of the worker's segment; the
		// dispatcher is the drainer, so pushing to the free ring is its
		// SPSC role.
		unsafe { handle.shm.pool().release(frame.slot) };
	}

	/// Release a consumed audio frame's slot (M15 S3) — the audio
	/// counterpart of [`ProcessDispatcher::release_frame`].
	pub fn release_audio_frame(&self, frame: &ShmAudioRef) {
		self.release_frame(&frame.frame_ref());
	}

	/// Cancel one frame request (pending or in flight). The completion
	/// fires with `Error::State` exactly once; a late frame_ready for an
	/// in-flight cancel recycles the slot silently.
	pub fn cancel_frame(&self, key: &FrameKey) {
		let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
		{
			let mut inner = lock(&self.inner);
			if !inner.scheduler.cancel_key(key) {
				return;
			}
			// Find the ticket behind the key and deliver the cancellation.
			let ticket = inner
				.tickets
				.iter()
				.find(|(_, pt)| &pt.key == key)
				.map(|(id, _)| *id);
			if let Some(id) = ticket {
				// 完成即移除（ticket 表只增不减是内存泄漏——播放时
				// 每秒 50-100 个 ticket 全部永久驻留）。
				if let Some(mut pt) = inner.tickets.remove(&id) {
					if let Some(done) = pt.done.take() {
						fired.push((done, Err(Error::State)));
					}
				}
			}
		}
		for (done, result) in fired {
			done(result);
		}
	}

	/// Broadcast the plugin-cancel signal to every alive worker (the user
	/// cancelled the plugin render from the progress dialog). The message
	/// is a fire-and-forget control line; the worker sets its sticky cancel
	/// flag and the next reporter update answers false. A send failure
	/// recycles that worker (it will restart on the next pump).
	pub fn broadcast_plugin_cancel(&self) {
		let mut inner = lock(&self.inner);
		for handle in inner.workers.iter_mut() {
			if matches!(handle.state, WorkerState::Alive | WorkerState::Starting) {
				if self.send_json(handle, &plugin_cancel_json()).is_err() {
					handle.state = WorkerState::Dead;
				}
			}
		}
	}

	/// Set (or clear) the graph snapshot path shipped to every worker via
	/// `load_graph` (M16 S1). A new path is sent to every alive worker —
	/// reloading a snapshot is idempotent, and the manager only re-sends
	/// when the snapshot revision actually changes. Clearing only updates
	/// the config: the protocol has no clear message, so alive workers
	/// keep their loaded graph and only new/restarted workers skip it.
	pub fn set_graph_snapshot(&self, path: Option<String>) {
		let mut inner = lock(&self.inner);
		if inner.shutting_down {
			return;
		}
		inner.config.graph_snapshot = path.clone();
		let Some(path) = path else { return };
		for handle in inner.workers.iter_mut() {
			if matches!(handle.state, WorkerState::Alive | WorkerState::Starting) {
				handle.graph_sent = true;
				if self
					.send_json(handle, &json!({ "type": "load_graph", "path": path }))
					.is_err()
				{
					handle.state = WorkerState::Dead;
				}
			}
		}
	}

	// ---- internals ------------------------------------------------------

	fn pump(&self, inner: &mut Inner, fired: &mut Vec<(Completion, TicketResult)>) {
		// 0. Throttled resize: a target requested inside the throttle
		//    window is applied once the window has passed (the pool does
		//    NOT churn on every 4K->1080p->4K flap; the latest target of
		//    the burst wins).
		if let Some(target) = inner.next_target {
			let window_passed = inner
				.last_resize_at
				.is_none_or(|last| last.elapsed() >= MIN_RESIZE_INTERVAL);
			if window_passed {
				self.apply_target_workers(inner, target, Instant::now());
			}
		}

		// 1. Drain worker events (non-blocking). Events from a previous
		//    spawn generation (a dead child's reader) are dropped so a
		//    late EOF cannot kill the replacement worker.
		while let Ok(ev) = inner.events_rx.try_recv() {
			match ev {
				WorkerEvent::Line {
					worker,
					generation,
					line,
				} => {
					let current = inner
						.workers
						.get(worker)
						.map(|w| w.generation)
						.unwrap_or(u64::MAX);
					if current != generation {
						continue;
					}
					self.on_line(inner, worker, &line, fired);
				}
				WorkerEvent::Eof { worker, generation } => {
					if let Some(handle) = inner.workers.get_mut(worker) {
						if handle.generation != generation {
							continue;
						}
						if !matches!(handle.state, WorkerState::PermanentlyDead) {
							handle.state = WorkerState::Dead;
						}
					}
				}
			}
		}

		// 2. Restart dead workers / handshake timeouts. RETIRING workers
		//    (a pool shrink) are NOT restarted — they were told to shut
		//    down and are simply being drained; the EOF that follows their
		//    natural exit is reaped in step 4.
		let timeout = Duration::from_millis(inner.config.handshake_timeout_ms);
		for i in 0..inner.workers.len() {
			let action = {
				let handle = &inner.workers[i];
				if handle.retiring {
					false
				} else {
					match handle.state {
						WorkerState::Dead => true,
						WorkerState::Starting => handle.spawned_at.elapsed() > timeout,
						_ => false,
					}
				}
			};
			if action {
				self.restart_worker(inner, i, fired);
			}
		}

		// 3. Interleaved batch claims + dispatch (free slots = credit).
		//    Retiring workers (a pool shrink) claim nothing — they finish
		//    their in-flight batch and exit naturally on the shutdown
		//    signal (step 4 sends it once; the worker drains its current
		//    frame first — never killed mid-work).
		for i in 0..inner.workers.len() {
			if matches!(inner.workers[i].state, WorkerState::Alive)
				&& !inner.workers[i].reconfiguring
				&& !inner.workers[i].retiring
			{
				self.dispatch_to(inner, i);
			}
		}

		// 4. Shrink drain: a retiring worker with nothing outstanding (and
		//    nothing held) got its shutdown signal. We do NOT kill it: the
		//    worker finishes whatever is in flight and exits by itself
		//    (worker.cpp's control loop exits on the shutdown flag); the
		//    EOF/exit is reaped here ASYNCHRONOUSLY — the slot is removed
		//    only once the child actually exited, so a busy worker may
		//    linger a few polls before its entry goes away (that's fine:
		//    the scheduler already re-sharded at the target, and the entry
		//    claims nothing while retiring). Only a child that is still
		//    alive 30 s after the signal (worker.cpp's own deadline —
		//    a hung decode?) is killed as the last resort.
		let mut signaled = Vec::new();
		for (i, handle) in inner.workers.iter().enumerate() {
			if handle.retiring
				&& handle.outstanding.is_empty()
				&& handle.held.is_empty()
				&& handle.retire_sent_at.is_none()
				&& matches!(handle.state, WorkerState::Alive | WorkerState::Starting)
			{
				signaled.push(i);
			}
		}
		for i in signaled {
			let handle = &mut inner.workers[i];
			_ = self.send_json(handle, &json!({ "type": "shutdown" }));
			handle.retire_sent_at = Some(Instant::now());
		}
		let mut reaped_flags: Vec<bool> = Vec::with_capacity(inner.workers.len());
		for (i, handle) in inner.workers.iter_mut().enumerate() {
			if !handle.retiring {
				reaped_flags.push(false);
				continue;
			}
			let reaped = match handle.child.as_mut() {
				Some(child) => child.try_wait().ok().flatten().is_some(),
				None => true,
			};
			// EOF (state Dead) means the child's reader thread saw exit;
			// the try_wait above confirms it. The 30 s deadline is only
			// the kill-last-resort anchor, not an unlock.
			let deadline_reached = handle
				.retire_sent_at
				.is_some_and(|sent| sent.elapsed() > Duration::from_secs(30));
			reaped_flags.push(reaped || deadline_reached);
		}
		for (i, reaped) in reaped_flags.into_iter().enumerate().rev() {
			if !reaped {
				continue;
			}
			let mut handle = inner.workers.remove(i);
			// A retiring worker that exited WITHOUT draining its
			// outstanding batch (crash, or the 30 s deadline hit) leaves
			// its assigned frames unclaimed: re-queue them so a SURVIVING
			// worker renders them. The re-queue happens after this resize
			// pass (the scheduler already runs at the new modulus, and
			// the pump's step-3 dispatch walk above used the OLD vector —
			// next pump's walk sees the surviving set only, so the frames
			// cannot land on a worker that exits next). `worker_crashed`
			// marks them any_worker=true, exactly the crash path.
			inner.scheduler.worker_crashed(i);
			if let Some(mut child) = handle.child.take() {
				let deadline_reached = handle
					.retire_sent_at
					.is_some_and(|sent| sent.elapsed() > Duration::from_secs(30));
				if deadline_reached && handle.state != WorkerState::Dead {
					let _ = child.kill();
					let _ = child.wait();
					eprintln!("procpool: retiring worker {i} hung past its deadline; killed last-resort");
				}
			}
			handle.stdin = None;
		}
	}

	fn on_line(
		&self,
		inner: &mut Inner,
		worker: usize,
		line: &str,
		fired: &mut Vec<(Completion, TicketResult)>,
	) {
		let msg: Value = match serde_json::from_str::<Value>(line) {
			Ok(v) if v.is_object() => v,
			_ => return,
		};
		let typ = msg.get("type").and_then(Value::as_str).unwrap_or("");
		let handle = match inner.workers.get_mut(worker) {
			Some(h) => h,
			None => return,
		};
		match typ {
			TYPE_HANDSHAKE => {
				// The worker's startup handshake: answer with the shm
				// geometry (protocol v1 flow). A mid-session handshake is a
				// segment grow (M15 S3): the worker re-attaches the new pool.
				handle.startup_seen = true;
				if self.send_json(handle, &handshake_for(handle)).is_err() {
					handle.state = WorkerState::Dead;
				}
			}
			TYPE_HELLO_CAPS => {
				if let Ok(caps) = serde_json::from_value::<HelloCapsMsg>(msg) {
					handle.caps = Some(caps);
					handle.state = WorkerState::Alive;
					// A re-attach after a segment grow is complete: the
					// dispatcher may send batches again.
					handle.reconfiguring = false;
					// One load_graph right after the first handshake.
					if !handle.graph_sent {
						if let Some(path) = inner.config.graph_snapshot.clone() {
							handle.graph_sent = true;
							if self
								.send_json(handle, &json!({ "type": "load_graph", "path": path }))
								.is_err()
							{
								handle.state = WorkerState::Dead;
							}
						}
					}
				}
			}
			TYPE_BATCH_ACCEPTED => {
				if let Ok(accepted) = serde_json::from_value::<BatchAcceptedMsg>(msg) {
					let _ = accepted;
					handle.accepted_batches += 1;
				}
			}
			TYPE_FRAME_READY => {
				if let Ok(ready) = serde_json::from_value::<FrameReadyMsg>(msg) {
					self.on_frame_ready(inner, worker, ready.ticket, ready.slot, fired);
				}
			}
			TYPE_FRAME_FAILED => {
				if let Ok(failed) = serde_json::from_value::<FrameFailedMsg>(msg) {
					self.on_frame_failed(inner, worker, failed.ticket, &failed.error, fired);
				}
			}
			TYPE_ERROR => {
				let ticket = msg.get("ticket").and_then(Value::as_i64);
				let message = msg
					.get("message")
					.and_then(Value::as_str)
					.unwrap_or("(no message)")
					.to_string();
				match ticket {
					Some(t) => self.on_frame_failed(inner, worker, t, &message, fired),
					None => {
						// A session-level error (e.g. load_graph or shm
						// attach failed): recycle the worker.
						eprintln!("procpool: worker {worker} error: {message}");
						if matches!(handle.state, WorkerState::Starting) {
							handle.state = WorkerState::Dead;
						}
					}
				}
			}
			TYPE_PLUGIN_PROGRESS => {
				// A worker forwarded an OFX plugin progress event; hand it
				// to the app's registered callback (which drives the
				// plugin-progress dialog).
				if let Ok(progress) = serde_json::from_value::<PluginProgressMsg>(msg) {
					if let Some(cb) = plugin_progress_cb() {
						cb(progress.label, progress.message, progress.fraction);
					}
				}
			}
			_ => {}
		}
	}

	fn on_frame_ready(
		&self,
		inner: &mut Inner,
		worker: usize,
		ticket: i64,
		slot: i32,
		fired: &mut Vec<(Completion, TicketResult)>,
	) {
		let handle = match inner.workers.get_mut(worker) {
			Some(h) => h,
			None => return,
		};
		if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
			eprintln!("procpool: worker {worker} frame_ready ticket {ticket} slot {slot}");
		}
		if handle.outstanding.remove(&ticket).is_none() {
			return; // late / duplicate / post-restart frame
		}
		// Drain the ready ring in lockstep (the SPSC hand-off contract);
		// frame_ready is authoritative about the slot.
		let mut ring_slot = 0;
		// SAFETY: live pool view; the dispatcher is the ready-ring
		// consumer.
		let popped = unsafe { handle.shm.pool().consume(&mut ring_slot) };
		if !popped || ring_slot != slot as u32 {
			eprintln!(
				"procpool: worker {worker} ready-ring out of sync (popped {popped}, ring {ring_slot}, msg {slot})"
			);
		}
		let meta = handle.shm.meta_copy(slot as u32);
		let shm = handle.shm.clone();
		handle.held.insert(slot as u32);

		let pt = inner.tickets.remove(&ticket);
		match pt {
			Some(mut pt) => {
				let key = pt.key;
				inner.scheduler.frame_done(&key);
				if let Some(done) = pt.done.take() {
					// M15 S3: audio tickets complete with the shm audio
					// payload (the consumer reads the slot and releases it);
					// video tickets keep the ShmFrame payload.
					if let Some(audio) = &pt.audio {
						let params = audio.clone();
						fired.push((
							done,
							Ok(TicketPayload::ShmAudio(ShmAudioRef {
								worker: worker as u32,
								slot: slot as u32,
								meta,
								shm,
								sample_rate: params.sample_rate,
								channel_layout: params.channel_layout,
								channel_count: params.channel_layout.count_ones().max(1) as i32,
							})),
						));
					} else {
						fired.push((
							done,
							Ok(TicketPayload::ShmFrame(ShmFrameRef {
								worker: worker as u32,
								slot: slot as u32,
								meta,
								shm,
							})),
						));
					}
				} else {
					// Cancelled while in flight: recycle the slot now.
					self.recycle_slot(inner, worker, slot as u32);
				}
			}
			None => {
				self.recycle_slot(inner, worker, slot as u32);
			}
		}
	}

	fn on_frame_failed(
		&self,
		inner: &mut Inner,
		worker: usize,
		ticket: i64,
		error: &str,
		fired: &mut Vec<(Completion, TicketResult)>,
	) {
		if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
			eprintln!("procpool: worker {worker} frame_failed ticket {ticket}: {error}");
		}
		let slot = {
			let handle = match inner.workers.get_mut(worker) {
				Some(h) => h,
				None => return,
			};
			handle.outstanding.remove(&ticket)
		};
		let Some(slot) = slot else { return };
		// The worker acquired the slot but never published it: the
		// dispatcher (drainer) hands it back to the free pool.
		self.recycle_slot(inner, worker, slot);
		if let Some(mut pt) = inner.tickets.remove(&ticket) {
			inner.scheduler.frame_failed(&pt.key);
			if let Some(done) = pt.done.take() {
				fired.push((done, Err(Error::Failed(format!("render failed: {error}")))));
			}
		}
	}

	/// Return a slot to the worker's free pool (queue + ring).
	fn recycle_slot(&self, inner: &mut Inner, worker: usize, slot: u32) {
		let Some(handle) = inner.workers.get_mut(worker) else {
			return;
		};
		handle.held.remove(&slot);
		handle.free_slots.push_back(slot);
		// SAFETY: live pool view; drainer-side free-ring push.
		unsafe { handle.shm.pool().release(slot) };
	}

	fn dispatch_to(&self, inner: &mut Inner, worker: usize) {
		loop {
			let credit = inner.workers[worker].free_slots.len();
			if credit == 0 {
				return;
			}
			// Grow-on-demand (M15 S3): if a pending request for this worker
			// needs a bigger slot than the segment provides, and the worker
			// has no in-flight frames, rebuild its segment first (the worker
			// re-attaches on a fresh handshake). While the worker is busy the
			// oversized request simply stays pending — claim_batch filters it
			// by max_bytes, so it is served after the drain.
			let grow = {
				let handle = &inner.workers[worker];
				if handle.outstanding.is_empty() {
					inner
						.scheduler
						.max_pending_bytes_for_worker(worker, handle.slot_bytes)
				} else {
					None
				}
			};
			if let Some(need) = grow {
				if let Err(e) = self.rebuild_segment(inner, worker, need) {
					eprintln!("procpool: worker {worker} segment grow to {need} B failed: {e}");
				}
				// Stop here: the worker is re-attaching to the new segment
				// (hello_caps pending). Dispatch resumes on the next pump
				// once `reconfiguring` clears — sending a batch now would
				// race the pool swap.
				return;
			}
			let max_bytes = inner.workers[worker].slot_bytes;
			let Some(batch) = inner.scheduler.claim_batch(worker, credit, max_bytes) else {
				// Starvation diagnostics (OAK_DEBUG_DISPATCH=1): pending work
				// exists but this worker claimed none of it — log why (no
				// credit, shard mismatch or oversized slot) instead of
				// spinning silently (the seek-starvation hang).
				if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
					let pending = inner.scheduler.pending_len();
					if pending > 0 {
						eprintln!(
							"procpool: worker {worker} idle with {pending} pending (credit {credit}, slot_bytes {max_bytes}): {:?}",
							inner.scheduler.pending_summary()
						);
					}
				}
				return;
			};
			// Slot assignment order MUST match the worker's acquisition
			// order: the batch is delivered as the video message first and
			// the audio message second, and the worker pops one slot per
			// ticket in that message order, checking each pop against the
			// assignment. Assigning in the scheduler's interleaved frame
			// order scrambles the free ring (every audio ticket in a mixed
			// batch mismatched, and each mismatch leaked a slot — the
			// "slot assignment mismatch" flood). Two passes: video first.
			let (video_reqs, audio_reqs): (Vec<_>, Vec<_>) =
				batch.frames.iter().partition(|r| {
					!inner
						.tickets
						.get(&r.payload)
						.is_some_and(|pt| pt.audio.is_some())
				});
			let mut video_tickets = Vec::with_capacity(video_reqs.len());
			let mut audio_tickets: Vec<AudioTicketSpec> = Vec::with_capacity(audio_reqs.len());
			for req in video_reqs.into_iter().chain(audio_reqs) {
				let ticket = req.payload;
				let Some(slot) = inner.workers[worker].free_slots.pop_front() else {
					break; // credit accounting drifted; stop cleanly
				};
				inner.workers[worker].outstanding.insert(ticket, slot);
				let Some(pt) = inner.tickets.get(&ticket) else {
					continue;
				};
				if let Some(audio) = &pt.audio {
					audio_tickets.push(build_audio_ticket_spec(ticket, slot, audio));
				} else {
					video_tickets.push(build_ticket_spec(
						ticket,
						slot,
						&pt.params,
						inner.config.slot_format,
					));
				}
			}
			// A single claim may mix audio and video (different scheduler
			// keys in one batch); they are delivered as two messages under
			// the same batch id, claimed by the worker in order.
			if !video_tickets.is_empty() {
				let msg = RenderBatchMsg {
					batch_id: batch.batch_id as i64,
					tickets: video_tickets,
				};
				// The `type` tag is added by hand: the parse-side structs only
				// carry the payload fields.
				let mut value = match serde_json::to_value(&msg) {
					Ok(v) => v,
					Err(_) => return,
				};
				if let Some(obj) = value.as_object_mut() {
					obj.insert(
						"type".to_string(),
						Value::String(crate::ipc::TYPE_RENDER_BATCH.to_string()),
					);
				}
				if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
					let ids: Vec<i64> = msg.tickets.iter().map(|t| t.ticket).collect();
					eprintln!("procpool: worker {worker} sent video batch {} tickets {ids:?}", msg.batch_id);
				}
				if self.send_json(&mut inner.workers[worker], &value).is_err() {
					inner.workers[worker].state = WorkerState::Dead;
					return;
				}
			}
			if !audio_tickets.is_empty() {
				let msg = RenderAudioBatchMsg {
					batch_id: batch.batch_id as i64,
					tickets: audio_tickets,
				};
				let mut value = match serde_json::to_value(&msg) {
					Ok(v) => v,
					Err(_) => return,
				};
				if let Some(obj) = value.as_object_mut() {
					obj.insert(
						"type".to_string(),
						Value::String(TYPE_RENDER_AUDIO_BATCH.to_string()),
					);
				}
				if self.send_json(&mut inner.workers[worker], &value).is_err() {
					inner.workers[worker].state = WorkerState::Dead;
					return;
				}
			}
		}
	}

	fn send_json(&self, handle: &mut WorkerHandle, msg: &Value) -> Result<()> {
		let stdin = handle.stdin.as_mut().ok_or(Error::State)?;
		write_message(stdin, msg).map_err(|e| Error::Failed(format!("worker stdin: {e}")))?;
		stdin
			.flush()
			.map_err(|e| Error::Failed(format!("worker stdin flush: {e}")))
	}

	fn spawn_worker(&self, inner: &mut Inner, index: usize) -> Result<()> {
		// One segment generation per (re)spawn: the key carries the restart
		// count so a restart never reuses the previous name — dropping the
		// old handle unlinks the OLD segment by name and must not remove
		// the freshly created one (SharedMemoryRegion::close unlinks by
		// name for Create-mode regions).
		let generation = inner
			.workers
			.get(index)
			.map(|w| w.restarts as u64)
			.unwrap_or(0);
		let key = format!(
			"{}-g{generation}",
			SharedMemoryRegion::make_key(std::process::id() as i64, index as i32)
		);
		let shm = ShmRegionView::create(&key, inner.slots, inner.slot_bytes)?;

		let mut child = Command::new(&inner.bin)
			// Auto backend: prefer the GPU, fall back to the CPU renderer
			// (M16 S1 — the worker tolerates a GPU init failure and keeps
			// evaluating headless).
			.args(["--backend", "auto"])
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit())
			.spawn()
			.map_err(|e| Error::Failed(format!("spawn oak-worker: {e}")))?;
		let stdin = child.stdin.take();
		let stdout = child
			.stdout
			.take()
			.ok_or_else(|| Error::Failed("oak-worker stdout not piped".into()))?;

		// Reader thread: stdout lines -> event channel (control plane).
		// Events carry the spawn generation so stale events from a dead
		// child are dropped after a restart.
		let tx = inner.events_tx.clone();
		std::thread::Builder::new()
			.name(format!("oak-worker-{index}-reader"))
			.spawn(move || {
				use std::io::BufRead;
				let mut reader = std::io::BufReader::new(stdout);
				let mut line = String::new();
				loop {
					line.clear();
					match reader.read_line(&mut line) {
						Ok(0) => {
							let _ = tx.send(WorkerEvent::Eof {
								worker: index,
								generation,
							});
							return;
						}
						Ok(_) => {
							let _ = tx.send(WorkerEvent::Line {
								worker: index,
								generation,
								line: line.trim_end().to_string(),
							});
						}
						Err(_) => {
							let _ = tx.send(WorkerEvent::Eof {
								worker: index,
								generation,
							});
							return;
						}
					}
				}
			})
			.map_err(|e| Error::Failed(format!("spawn reader thread: {e}")))?;

		let mut handle = WorkerHandle::shell(index, generation, shm, inner.slots, inner.slot_bytes);
		handle.child = Some(child);
		handle.stdin = stdin;
		handle.spawned_at = Instant::now();
		if index < inner.workers.len() {
			// Restart path: keep the restart counter.
			handle.restarts = inner.workers[index].restarts;
			inner.workers[index] = handle;
		} else {
			inner.workers.push(handle);
		}
		Ok(())
	}

	fn restart_worker(
		&self,
		inner: &mut Inner,
		worker: usize,
		fired: &mut Vec<(Completion, TicketResult)>,
	) {
		// Reap the child and drop the pipes.
		let retiring = inner.workers[worker].retiring;
		let restarts = {
			let handle = &mut inner.workers[worker];
			if let Some(mut child) = handle.child.take() {
				let _ = child.kill();
				let _ = child.wait();
			}
			handle.stdin = None;
			handle.startup_seen = false;
			handle.graph_sent = false;
			handle.caps = None;
			handle.held.clear();
			handle.outstanding.clear();
			handle.restarts += 1;
			handle.restarts
		};

		// Crash recovery (design §3.2): every claimed frame of this
		// worker — un-started batches and un-finished frames alike — is
		// re-queued; any healthy worker may claim it.
		let reclaimed = inner.scheduler.worker_crashed(worker);

		if retiring {
			// A RETIRING worker crashed (dying, but died before its
			// in-flight batch drained): its frames were just re-queued —
			// they must go to SURVIVING workers only, so the re-claim
			// below (which happens on the next dispatch walk) trusts the
			// retiring guard in step 3. The pool's shrink target already
			// dropped this index, so the slot is removed now; the worker
			// is done either way (this is a crash mid-retirement, not a
			// respawn — retrying would revive a worker the pool explicitly
			// downsized).
			inner.workers.remove(worker);
			// Leave the re-claimed frames pending: the next pump's
			// dispatch_to walks the SURVIVING workers only (retiring ones
			// claim nothing), so these frames land on a live worker. Their
			// tickets were never removed — they complete normally.
			// Do not fire them here; they stay claimable.
			let _ = restarts;
			return;
		}

		if restarts > MAX_RESTARTS {
			// Restart budget exhausted: the worker stays down and its
			// frames fail permanently (main paints the fallback).
			inner.workers[worker].state = WorkerState::PermanentlyDead;
			for req in reclaimed {
				inner.scheduler.cancel_key(&req.key);
				if let Some(mut pt) = inner.tickets.remove(&req.payload) {
					if let Some(done) = pt.done.take() {
						fired.push((
							done,
							Err(Error::Failed(
								"render worker crashed repeatedly; frame dropped".into(),
							)),
						));
					}
				}
			}
			return;
		}

		// Fresh segment + respawn (a Create unlinks any stale segment).
		if let Err(e) = self.spawn_worker(inner, worker) {
			eprintln!("procpool: worker {worker} respawn failed: {e}");
			inner.workers[worker].state = WorkerState::Dead;
		}
	}

	/// Grow a worker's segment to `need_bytes` per slot (M15 S3 grow-on-
	/// demand geometry, design §3.1 "段按需扩容或重建"): create a fresh
	/// segment under a new key (the old mapping stays alive for consumers
	/// still holding [`ShmFrameRef`]s into it — they release as stale
	/// refs), re-point the handle, reseed the free slots and have the
	/// worker re-attach through a fresh handshake. The caller guarantees
	/// `outstanding` is empty (no frame is mid-render in the old pool).
	/// The worker's hello_caps clears `reconfiguring`, unblocking dispatch.
	fn rebuild_segment(&self, inner: &mut Inner, worker: usize, need_bytes: usize) -> Result<()> {
		let current_slots = inner.workers[worker].shm.slot_count();
		let slots = default_slots_for_bytes(need_bytes, current_slots);
		let generation = inner.seg_generation;
		inner.seg_generation += 1;
		let base = SharedMemoryRegion::make_key(std::process::id() as i64, worker as i32);
		let key = format!(
			"{base}-g{}-s{generation}",
			inner.workers[worker].generation
		);
		let shm = ShmRegionView::create(&key, slots, need_bytes)?;
		{
			let handle = &mut inner.workers[worker];
			handle.shm = shm;
			handle.slot_bytes = need_bytes;
			handle.free_slots = (0..slots).collect();
			handle.held.clear();
			// No new batches until the worker re-attaches the new pool.
			handle.reconfiguring = true;
		}
		let hs = {
			let handle = &inner.workers[worker];
			handshake_for(handle)
		};
		if self.send_json(&mut inner.workers[worker], &hs).is_err() {
			inner.workers[worker].state = WorkerState::Dead;
		}
		Ok(())
	}
}

/// The handshake reply the dispatcher sends a worker (startup and M15 S3
/// segment-grow re-attach): the worker's current shm geometry.
fn handshake_for(handle: &WorkerHandle) -> Value {
	HandshakeMsg {
		protocol_version: DISPATCH_PROTOCOL_VERSION,
		shm_key: handle.shm.key().to_string(),
		input_shm_key: String::new(),
		input_slots: 0,
		output_slots: handle.shm.slot_count() as i32,
		slot_data_bytes: handle.shm.slot_data_bytes() as i64,
		input_slot_data_bytes: 0,
	}
	.to_json()
}

impl JobDispatch for ProcessDispatcher {
	/// Submit one frame job (the ticket-arena seam). The job joins the
	/// scheduler under its [`JobSchedule`] (Seek single-frame by default,
	/// Playback for the pre-render window, Background for exports) and is
	/// dispatched on the next pump; the completion fires with
	/// `TicketPayload::ShmFrame(ShmFrameRef)` — never a pixel buffer.
	/// Re-submitting a key that is still pending replaces the old request
	/// and cancels its ticket; a key already in flight is left running
	/// (its result is still valid for the same params).
	fn post(&self, job: Job) -> bool {
		let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
		{
			let mut inner = lock(&self.inner);
			if inner.shutting_down {
				return false;
			}
			// M15 S3: audio ranges larger than a practical shm slot (export
			// of many minutes of audio) are refused here so the arena falls
			// back to main-process inline rendering (design §3.7) — the
			// process backend stays for the real-time chunks and short
			// ranges that fit a segment.
			if let Some(audio) = &job.audio {
				let too_large = match crate::eval::audio_samples_byte_len(audio) {
					Ok(bytes) => bytes > MAX_AUDIO_SLOT_BYTES,
					Err(_) => true, // invalid range: let the inline path report it
				};
				if too_large {
					return false;
				}
			}
			let id = inner.next_ticket;
			inner.next_ticket += 1;
			let frame = job.schedule.frame.unwrap_or(id);
			let key = FrameKey {
				sequence: job.node_identity,
				frame,
				version: job.schedule.version,
			};
			// M15 S3: per-request slot geometry. Audio tickets need the
			// sample bytes of their range; video tickets need the frame
			// size x the ticket's wire format (force_format honored).
			let slot_bytes = match &job.audio {
				Some(audio) => crate::eval::audio_samples_byte_len(audio).unwrap_or(0),
				None => {
					let (w, h) = job.params.render_size();
					slot_bytes_for(w, h, ticket_wire_format(&job.params, inner.config.slot_format))
				}
			};
			inner.tickets.insert(
				id,
				PendingTicket {
					key,
					params: job.params,
					audio: job.audio,
					done: Some(job.done),
				},
			);
			let request = FrameRequest {
				key,
				priority: job.schedule.priority,
				distance: job.schedule.distance,
				payload: id,
				slot_bytes,
			};
			if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
				let pools: Vec<String> = inner
					.workers
					.iter()
					.enumerate()
					.map(|(i, w)| {
						format!("w{i}: free {} held {} out {} state {:?}",
							w.free_slots.len(), w.held.len(), w.outstanding.len(), w.state)
					})
					.collect();
				eprintln!(
					"procpool: post ticket {id} key ({}, {}, {}) prio {:?} shard {} | {}",
					key.sequence, key.frame, key.version, request.priority,
					key.frame.rem_euclid(inner.scheduler.workers() as i64),
					pools.join(" | ")
				);
			}
			match inner.scheduler.submit(request) {
				SubmitOutcome::Accepted => {}
				SubmitOutcome::Replaced(old) => {
					// A newer request for the same key superseded the old
					// pending one: cancel the old ticket's completion (and
					// reap the entry — the table must not grow unbounded).
					if let Some(mut pt) = inner.tickets.remove(&old.payload) {
						if let Some(done) = pt.done.take() {
							fired.push((done, Err(Error::State)));
						}
					}
				}
				SubmitOutcome::InFlight => {
					// Already claimed by a worker under the same key: the
					// worker will deliver the OLD ticket only. Fire the new
					// ticket's completion as cancelled so its entry does
					// not leak and the caller (playback window) may
					// re-request once the in-flight render lands.
					if let Some(mut pt) = inner.tickets.remove(&id) {
						if let Some(done) = pt.done.take() {
							fired.push((done, Err(Error::State)));
						}
					}
				}
			}
		}
		for (done, result) in fired {
			done(result);
		}
		// Pump once so a live worker picks the frame up immediately.
		self.poll();
		true
	}

	/// Cancel every pending AND claimed request of `sequence` (M15 S2
	/// preview-window invalidation — graph/proxy/resolution/color bump or
	/// a sequence switch). Dropped completions fire `Error::State`;
	/// frames already dispatched recycle their slots when the late
	/// `frame_ready` arrives.
	fn cancel_preview_sequence(&self, sequence: u64) {
		let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
		{
			let mut inner = lock(&self.inner);
			let dropped = inner.scheduler.cancel_sequence(sequence);
			for request in dropped {
				if let Some(mut pt) = inner.tickets.remove(&request.payload) {
					if let Some(done) = pt.done.take() {
						fired.push((done, Err(Error::State)));
					}
				}
			}
		}
		for (done, result) in fired {
			done(result);
		}
	}

	/// Pump the control plane (delegates to the inherent poll — the UI
	/// tick and blocking ticket waits call this through the trait seam).
	fn poll(&self) {
		self.poll();
	}

	/// The pre-render window's slot headroom (see the inherent
	/// [`ProcessDispatcher::preview_window_capacity`]).
	fn preview_window_capacity(&self) -> Option<usize> {
		Some(self.preview_window_capacity())
	}

	/// Cancel one pre-render window frame (delegates to the inherent
	/// [`ProcessDispatcher::cancel_frame`]).
	fn cancel_preview_frame(&self, sequence: u64, frame: i64, version: u64) {
		self.cancel_frame(&FrameKey {
			sequence,
			frame,
			version,
		});
	}

	/// Ship a graph snapshot to the worker pool (M16 S1; delegates to the
	/// inherent [`ProcessDispatcher::set_graph_snapshot`]).
	fn set_graph_snapshot(&self, path: Option<String>) {
		self.set_graph_snapshot(path);
	}

	/// Release a consumed frame's slot (delegates to the inherent
	/// release — see [`ProcessDispatcher::release_frame`]).
	fn release_frame(&self, frame: &ShmFrameRef) {
		self.release_frame(frame);
	}

	/// Release a consumed audio frame's slot (M15 S3; delegates to the
	/// inherent release).
	fn release_audio_frame(&self, frame: &ShmAudioRef) {
		self.release_audio_frame(frame);
	}

	/// Graceful shutdown: `shutdown` messages, a short drain pumping
	/// completions, then kill stragglers; every ticket still open
	/// completes with `Error::State`.
	fn shutdown(&self) {
		let mut fired: Vec<(Completion, TicketResult)> = Vec::new();
		{
			let mut inner = lock(&self.inner);
			if inner.shutting_down {
				return;
			}
			inner.shutting_down = true;
			for i in 0..inner.workers.len() {
				let handle = &mut inner.workers[i];
				if matches!(handle.state, WorkerState::Alive | WorkerState::Starting) {
					let _ = self.send_json(handle, &json!({ "type": "shutdown" }));
				}
			}
		}
		// Drain window: let workers finish in-flight frames and deliver
		// the completions.
		let deadline = Instant::now() + Duration::from_secs(3);
		loop {
			{
				let mut inner = lock(&self.inner);
				self.pump(&mut inner, &mut fired);
				let any_running = inner.workers.iter_mut().any(|w| {
					w.child
						.as_mut()
						.map(|c| c.try_wait().ok().flatten().is_none())
						.unwrap_or(false)
				});
				if !any_running {
					break;
				}
			}
			if Instant::now() > deadline {
				break;
			}
			std::thread::sleep(Duration::from_millis(2));
			// Deliver what pumped so far before the next round.
			for (done, result) in fired.drain(..) {
				done(result);
			}
		}
		{
			let mut inner = lock(&self.inner);
			// Kill stragglers and reap.
			for w in inner.workers.iter_mut() {
				if let Some(mut child) = w.child.take() {
					let _ = child.kill();
					let _ = child.wait();
				}
				w.stdin = None;
			}
			// Every ticket still open completes with cancellation; the map
			// is dropped with the dispatcher (clear for hygiene — leaked
			// entries pin shm region views).
			for (_, pt) in inner.tickets.iter_mut() {
				if let Some(done) = pt.done.take() {
					fired.push((done, Err(Error::State)));
				}
			}
			inner.tickets.clear();
		}
		for (done, result) in fired {
			done(result);
		}
	}
}

/// Resolve the oak-worker binary path.
fn resolve_worker_bin(config: &DispatcherConfig) -> Result<PathBuf> {
	if let Some(p) = &config.worker_bin {
		return Ok(p.clone());
	}
	if let Ok(p) = std::env::var("OAK_WORKER_BIN") {
		return Ok(PathBuf::from(p));
	}
	let exe = std::env::current_exe()
		.map_err(|e| Error::Failed(format!("resolve oak-worker: current exe: {e}")))?;
	let candidate = exe
		.parent()
		.ok_or_else(|| Error::Failed("resolve oak-worker: no exe parent".into()))?
		.join(format!("oak-worker{}", std::env::consts::EXE_SUFFIX));
	if candidate.exists() {
		return Ok(candidate);
	}
	Err(Error::Failed(format!(
		"oak-worker binary not found at {}; set DispatcherConfig::worker_bin or OAK_WORKER_BIN",
		candidate.display()
	)))
}

/// The wire slot format a video ticket requests: the ticket's forced
/// PixelFormat when set (F32 for exports / full-resolution / scopes, M15
/// S3 — the worker then writes F32 straight into the slot and the export
/// reads it back with no BGRA8 round trip), else the dispatcher's default
/// slot format (BGRA8 for the viewer preview path).
fn ticket_wire_format(params: &VideoTicketParams, config_format: i32) -> i32 {
	params.force_format.map(|f| f as i32).unwrap_or(config_format)
}

/// Map ticket params to the wire ticket spec (main assigns `slot`).
fn build_ticket_spec(
	ticket: i64,
	slot: u32,
	params: &VideoTicketParams,
	slot_format: i32,
) -> BatchTicketSpec {
	let (width, height) = params.render_size();
	let (footage_file, footage_stream) = match &params.footage {
		Some((f, s)) => (f.clone(), *s),
		None => (String::new(), 0),
	};
	let montage = params
		.montage
		.iter()
		.map(|c| WireMontageClip {
			filename: c.filename.clone(),
			stream_index: c.stream_index,
			in_num: c.in_time.numerator(),
			in_den: c.in_time.denominator(),
			out_num: c.out_time.numerator(),
			out_den: c.out_time.denominator(),
			media_in_num: c.media_in.numerator(),
			media_in_den: c.media_in.denominator(),
			gain: c.gain,
			effects: c.effects.iter().map(crate::ipc::wire_effect_from).collect(),
		})
		.collect();
	BatchTicketSpec {
		ticket,
		slot: slot as i32,
		time_num: params.time.numerator(),
		time_den: params.time.denominator(),
		width,
		height,
		format: ticket_wire_format(params, slot_format),
		channels: 4,
		footage_file,
		footage_stream,
		montage,
		adjustments: params
			.adjustments
			.iter()
			.map(crate::ipc::wire_adjustment_from)
			.collect(),
		// M16 S1 graph mode: the worker renders the viewer's graph frame
		// when nonzero (else the montage path above) — and only when the
		// ticket's project matches the worker's loaded snapshot (the
		// `project_key` uuid guard).
		viewer_node: params.viewer,
		project_key: params.project.clone(),
	}
}

/// Map audio ticket params to the wire audio ticket spec (M15 S3; main
/// assigns `slot`).
fn build_audio_ticket_spec(ticket: i64, slot: u32, params: &AudioTicketParams) -> AudioTicketSpec {
	let duration = params.range.out() - params.range.in_();
	let montage = params
		.montage
		.iter()
		.map(|c| WireMontageClip {
			filename: c.filename.clone(),
			stream_index: c.stream_index,
			in_num: c.in_time.numerator(),
			in_den: c.in_time.denominator(),
			out_num: c.out_time.numerator(),
			out_den: c.out_time.denominator(),
			media_in_num: c.media_in.numerator(),
			media_in_den: c.media_in.denominator(),
			gain: c.gain,
			effects: c.effects.iter().map(crate::ipc::wire_effect_from).collect(),
		})
		.collect();
	AudioTicketSpec {
		ticket,
		slot: slot as i32,
		time_num: params.range.in_().numerator(),
		time_den: params.range.in_().denominator(),
		duration_num: duration.numerator(),
		duration_den: duration.denominator(),
		sample_rate: params.sample_rate,
		channel_layout: params.channel_layout,
		channels: params.channel_layout.count_ones().max(1) as i32,
		montage,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slot_bytes_for_formats() {
		// F32 RGBA: 16 bytes per pixel.
		assert_eq!(slot_bytes_for(1920, 1080, 4), 1920 * 1080 * 16);
		// BGRA8: 4 bytes per pixel (the 8.3 MB design figure).
		assert_eq!(slot_bytes_for(1920, 1080, SLOT_FORMAT_BGRA8), 1920 * 1080 * 4);
		// U8 RGBA.
		assert_eq!(slot_bytes_for(64, 64, 0), 64 * 64 * 4);
	}

	#[test]
	fn worker_count_policy_is_clamped() {
		// With absurd slot sizes the memory budget clamps to 1.
		let n = default_worker_count(64, 1 << 30); // 64 GiB per worker
		assert_eq!(n, 1);
		// With tiny slots the core policy dominates (>= 1).
		let n = default_worker_count(1, 64);
		assert!(n >= 1);
	}

	/// The Linux sysfs walk reads total/used per card, picks the first
	/// non-zero total, and skips (not aborts on) cards without the
	/// attributes — the AMD/Intel query path (amdgpu, i915, Xe expose
	/// `mem_info_vram_*`; an iGPU with no dedicated vram reports 0 and is
	/// skipped for the discrete card).
	#[cfg(target_os = "linux")]
	#[test]
	fn linux_drm_vram_walk_reads_cards_and_skips_missing() {
		let dir = std::env::temp_dir().join(format!(
			"oak_vram_fixture_{}_{}",
			std::process::id(),
			std::thread::current().name().unwrap_or("t")
		));
		let _ = std::fs::remove_dir_all(&dir);
		let write = |card: u32, total: Option<u64>, used: u64| {
			let base = dir.join(format!("card{card}")).join("device");
			std::fs::create_dir_all(&base).unwrap();
			if let Some(total) = total {
				std::fs::write(base.join("mem_info_vram_total"), total.to_string()).unwrap();
			}
			std::fs::write(base.join("mem_info_vram_used"), used.to_string()).unwrap();
		};
		// card0: no attributes (a display-only card) -> skipped.
		let card0 = dir.join("card0").join("device");
		std::fs::create_dir_all(&card0).unwrap();
		// card1: 8 GiB total, 2 GiB used -> the winner.
		write(1, Some(8 << 30), 2 << 30);
		// card2: 0 total (iGPU without dedicated memory) -> skipped.
		write(2, Some(0), 0);

		let (free, total) = linux_drm_vram_bytes_from(&dir)
			.expect("the walk must find the first non-zero card");
		assert_eq!(total, 8 << 30);
		assert_eq!(free, 6 << 30);

		// No non-zero card -> None (a totally attr-less tree).
		for f in [&dir.join("card1"), &dir.join("card0")] {
			let _ = std::fs::remove_dir_all(f);
		}
		assert!(linux_drm_vram_bytes_from(&dir).is_none());

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// `per_worker_gpu_budget` follows the pixel-count figure: 1080p is
	/// the 1 GiB + 256 MiB baseline, 4K ~4× of it, and higher fps
	/// over-provisions slightly (more surfaces in flight).
	#[test]
	fn per_worker_budget_scales_with_frame_size() {
		let hd = per_worker_gpu_budget((1920, 1080), 24);
		let fhd = per_worker_gpu_budget((1920, 1080), 60);
		let uhd = per_worker_gpu_budget((3840, 2160), 24);
		assert!(uhd > hd * 3, "4K budget must be ~4× 1080p ({uhd} vs {hd})");
		assert!(fhd >= hd, "60 fps must over-provision ({fhd} vs {hd})");
		// 1080p24 = 2 GiB peak (1 GiB × headroom 2) + 256 MiB idle.
		assert_eq!(hd, (2 << 30) + (256 << 20));
	}

	/// The GPU-vram worker budget scales with the frame's pixel count: a
	/// 1080p-sized budget allows the CPU-bound count, a 4K budget only
	/// the vram-fitting fraction (the user's "1080p 22 workers but 4K
	/// only 5" figure). Pure arithmetic — the vram probe is mocked by
	/// calling the budget formula directly.
	#[test]
	fn gpu_worker_budget_scales_with_frame_size() {
		// Use the real budget formula (headroom included): at 1080p24 the
		// per-worker budget is 2 GiB peak + 256 MiB idle = 2.25 GiB; at
		// 4K it is 4× that (pixel ratio 4) + idle = 8.25 GiB. With 24 GiB
		// free / 10% reserve (21.6 GiB usable): 9 workers at 1080p, 2 at
		// 4K — the pool must shrink sharply on resolution switch.
		let usable = (24u64 << 30) * 9 / 10;
		let n_1080p = usable / per_worker_gpu_budget((1920, 1080), 24);
		let n_4k = usable / per_worker_gpu_budget((3840, 2160), 24);
		let n_60 = usable / per_worker_gpu_budget((1920, 1080), 60);
		let n_24_1080 = usable / per_worker_gpu_budget((1920, 1080), 24);
		assert!(
			n_1080p > n_4k * 3,
			"4K must cut the worker count sharply ({n_1080p} vs {n_4k})"
		);
		assert_eq!(n_4k, 2, "24 GiB free @ 4K with the headroom budget: 2 workers");
		assert!(n_60 < n_24_1080, "higher fps must not get MORE workers ({n_60} vs {n_24_1080})");
	}

	#[test]
	fn preview_window_capacity_reserves_one_slot_per_worker() {
		// workers=3 × slots=4 → the window may hold 12-3=9 slots; the
		// reserve keeps interactive/audio tickets dispatchable (the
		// playback-freeze regression guard).
		let config = DispatcherConfig {
			worker_bin: Some(std::path::PathBuf::from("/bin/true")),
			workers: 3,
			slots_per_worker: 4,
			width: 64,
			height: 64,
			batch_size: 2,
			..Default::default()
		};
		let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
		assert_eq!(dispatcher.preview_window_capacity(), 9);
	}

	#[test]
	fn config_normalization_defaults() {
		let c = DispatcherConfig::default().normalize();
		// Geometry defaults resolve here; the adaptive counts stay 0 (auto)
		// and resolve in `ProcessDispatcher::new` where slot_bytes is known.
		assert_eq!(c.width, 1920);
		assert_eq!(c.height, 1080);
		assert_eq!(c.slot_format, SLOT_FORMAT_BGRA8);
		assert_eq!(c.slots_per_worker, 0);
		assert_eq!(c.workers, 0);
		assert_eq!(c.batch_size, 0);
	}

	#[test]
	fn slots_policy_adapts_to_slot_size() {
		// BGRA8 1080p: the full 8 slots (~66 MB per worker segment).
		let bgra8_1080p = slot_bytes_for(1920, 1080, SLOT_FORMAT_BGRA8);
		assert_eq!(default_slots_per_worker(bgra8_1080p), 8);
		// F32 1080p: drops to 4 (~133 MB per worker segment).
		let f32_1080p = slot_bytes_for(1920, 1080, 4);
		assert_eq!(default_slots_per_worker(f32_1080p), 4);
		// F32 4K: 2 slots (the floor).
		let f32_4k = slot_bytes_for(3840, 2160, 4);
		assert_eq!(default_slots_per_worker(f32_4k), 2);
		// Tiny slots: the cap at 8.
		assert_eq!(default_slots_per_worker(16), 8);
	}

	#[test]
	fn batch_size_policy_scales_with_workers_and_slots() {
		// 4 workers x 8 slots: the design 120/4 = 30 caps at the 8 slots.
		assert_eq!(default_batch_size(4, 8), 8);
		// 1 worker x 8 slots: 120/1 = 120 caps at 8.
		assert_eq!(default_batch_size(1, 8), 8);
		// 2 workers x 4 slots: 120/2 = 60 caps at 4.
		assert_eq!(default_batch_size(2, 4), 4);
		// 8 workers x 8 slots: 120/8 = 15 caps at 8.
		assert_eq!(default_batch_size(8, 8), 8);
	}

	#[test]
	fn grown_segment_slots_stay_bounded() {
		// Growing a segment keeps a sane slot count: 8.3 MB slots keep 8;
		// 33 MB slots keep 8 (still within the 256 MiB grown budget);
		// absurd sizes clamp at 2.
		assert_eq!(default_slots_for_bytes(8_300_000, 8), 8);
		assert_eq!(default_slots_for_bytes(33_000_000, 8), 8);
		assert_eq!(default_slots_for_bytes(1 << 30, 8), 2);
	}

	#[test]
	fn copy_counter_counts_only_slot_to_vec() {
		// The zero-copy contract: only `slot_to_vec` bumps the counter —
		// `slot_bytes` (the borrowed view the preview path uses) must not.
		reset_main_heap_frame_copies();
		let key = format!("oak-procpool-copycounter-{}", std::process::id());
		let view = ShmRegionView::create(&key, 2, 64).expect("shm segment");
		let _borrowed = view.slot_bytes(0);
		assert_eq!(
			main_heap_frame_copies(),
			0,
			"borrowed slot reads stay zero-copy"
		);
		let _copied = view.slot_to_vec(0);
		assert_eq!(
			main_heap_frame_copies(),
			1,
			"slot_to_vec is the one counted copy"
		);
		drop(view);
		SharedMemoryRegion::unlink_key(&key);
		reset_main_heap_frame_copies();
	}

	/// A worker's `plugin_progress` NDJSON line is forwarded to the
	/// registered app callback as (label, message, fraction) — the seam
	/// that drives the main-process plugin-progress dialog.
	#[test]
	fn plugin_progress_line_forwards_to_callback() {
		let config = DispatcherConfig {
			worker_bin: Some(std::path::PathBuf::from("/bin/true")),
			workers: 1,
			slots_per_worker: 2,
			width: 16,
			height: 16,
			batch_size: 1,
			..Default::default()
		};
		let dispatcher = ProcessDispatcher::new(config).expect("dispatcher");
		// `new` registered the weak handle (the cancel broadcast seam).
		assert!(dispatcher_slot().lock().unwrap().upgrade().is_some());

		let received: Arc<Mutex<Vec<(String, String, f64)>>> = Arc::new(Mutex::new(Vec::new()));
		set_plugin_progress_cb(Some(Arc::new({
			let received = received.clone();
			move |label, message, fraction| {
				received.lock().unwrap().push((label, message, fraction));
			}
		})));

		// A fake worker handle so on_line has a target (no spawn needed).
		{
			let mut inner = dispatcher.inner.lock().unwrap_or_else(|e| e.into_inner());
			let key = SharedMemoryRegion::make_key(std::process::id() as i64, 999);
			let shm = ShmRegionView::create(&key, 2, 256).expect("shm");
			inner.workers.push(WorkerHandle::shell(0, 0, shm, 2, 256));
		}

		let mut fired = Vec::new();
		{
			let mut inner = dispatcher.inner.lock().unwrap_or_else(|e| e.into_inner());
			dispatcher.on_line(
				&mut inner,
				0,
				r#"{"type":"plugin_progress","label":"render","message":"pass 1","fraction":0.5}"#,
				&mut fired,
			);
		}
		set_plugin_progress_cb(None);

		let events = received.lock().unwrap().clone();
		assert_eq!(events.len(), 1);
		assert_eq!(events[0], ("render".to_string(), "pass 1".to_string(), 0.5));
	}
}
