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

//! M15 S1 end-to-end: real oak-worker processes driven by the
//! main-process [`ProcessDispatcher`] — spawn, handshake, batched
//! renders into shared-memory slots, crash isolation with restart and
//! re-dispatch, and the zero-copy main-process guarantee.
//!
//! These tests spawn actual `oak-worker` child processes (located through
//! `CARGO_BIN_EXE_oak-worker`), so they double as the binary-resolution
//! regression gate for [`DispatcherConfig::worker_bin`].
//!
//! All tests in this file serialize on [`TEST_LOCK`]: they spawn worker
//! processes that inherit the process environment, and the crash test
//! sets crash-hook variables that must not leak into sibling runs.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::block::ClipBlockBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior};
use oak_render::ipc::SLOT_FORMAT_BGRA8;
use oak_render::procpool::{
	main_heap_frame_copies, reset_main_heap_frame_copies, DispatcherConfig, ProcessDispatcher,
};
use oak_render::ticket::{
	AudioTicketParams, TicketPayload, TicketResult, VideoTicketParams,
};
use oak_render::worker::{Job, JobDispatch, JobSchedule};

/// Serialize every test in this file (shared process environment +
/// real child processes).
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_test() -> std::sync::MutexGuard<'static, ()> {
	TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn worker_bin() -> std::path::PathBuf {
	env!("CARGO_BIN_EXE_oak-worker").into()
}

fn config(workers: usize, slots: u32) -> DispatcherConfig {
	DispatcherConfig {
		worker_bin: Some(worker_bin()),
		workers,
		slots_per_worker: slots,
		width: 64,
		height: 64,
		slot_format: SLOT_FORMAT_BGRA8,
		batch_size: 4,
		graph_snapshot: None,
		handshake_timeout_ms: 30_000,
	}
}

fn params(time: Rational, footage: Option<(String, i32)>) -> Arc<VideoTicketParams> {
	Arc::new(VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time,
		force_size: Some((64, 64)),
		// No forced format: the dispatcher's default slot format (BGRA8)
		// applies — the preview path these tests exercise (M15 S3: an F32
		// force would now request an F32 slot instead).
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage,
		montage: Vec::new(),
		adjustments: Vec::new(),
	})
}

/// Submit `count` generated-frame tickets; returns the shared results
/// sink (completions land there from the dispatcher's poll pump).
fn submit(
	dispatcher: &ProcessDispatcher,
	results: &Arc<Mutex<Vec<TicketResult>>>,
	count: usize,
	footage: Option<(String, i32)>,
) {
	for i in 0..count {
		let results = results.clone();
		let footage = footage.clone();
		let job = Job {
			node_identity: 1,
			time: Rational::new(i as i64, 25),
			params: params(Rational::new(i as i64, 25), footage),
			audio: None,
			// Never invoked on the process backend (workers render from
			// the wire spec); must still be a valid producer.
			produce: Arc::new(|_, _| {
				Err(oak_render::error::Error::Failed(
					"process backend does not use the in-process producer".into(),
				))
			}),
			done: Box::new(move |result| {
				results.lock().unwrap_or_else(|e| e.into_inner()).push(result);
			}),
			schedule: JobSchedule::seek(),
		};
		assert!(dispatcher.post(job), "post accepted while alive");
	}
}

/// Pump the dispatcher until `expect` completions arrive or the deadline
/// passes.
fn pump_until(dispatcher: &ProcessDispatcher, results: &Mutex<Vec<TicketResult>>, expect: usize) {
	let deadline = Instant::now() + Duration::from_secs(60);
	loop {
		dispatcher.poll();
		if results.lock().unwrap_or_else(|e| e.into_inner()).len() >= expect {
			return;
		}
		if Instant::now() > deadline {
			let have = results.lock().unwrap_or_else(|e| e.into_inner()).len();
			panic!("timeout: {have}/{expect} completions");
		}
		std::thread::sleep(Duration::from_millis(5));
	}
}

/// A pool resize (the GPU-vram-driven shrink/grow on a resolution
/// switch): shrinking 3→1 retires the tail workers WITHOUT killing a busy
/// one — a shrink mid-wave finishes every in-flight ticket AND the
/// retired workers exit by themselves (the shutdown signal + natural
/// drain, no kill). Growing 1→3 spawns the missing workers and the pool
/// delivers again. Slots are released as frames arrive (the credit-based
/// flow control), exactly like [`two_workers_render_two_waves_zero_copy`].
#[test]
fn pool_resize_drains_shrunk_workers_and_regrows() {
	let _guard = lock_test();
	let dispatcher = ProcessDispatcher::new(config(3, 4)).expect("dispatcher config");
	dispatcher.start().expect("workers start + handshake");
	assert_eq!(dispatcher.worker_count(), 3);

	// Wave 1: enough tickets to keep all three workers busy while the
	// shrink lands; every frame is released on arrival.
	let results = Arc::new(Mutex::new(Vec::new()));
	submit(&dispatcher, &results, 12, None);
	dispatcher.set_target_workers(1);

	let mut completed = 0usize;
	let deadline = Instant::now() + Duration::from_secs(60);
	while completed < 12 {
		dispatcher.poll();
		let drained: Vec<TicketResult> =
			results.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
		for result in drained {
			let payload = result.expect("frame rendered");
			let TicketPayload::ShmFrame(frame) = payload else {
				panic!("process backend must deliver ShmFrame payloads");
			};
			dispatcher.release_frame(&frame);
			completed += 1;
		}
		if Instant::now() > deadline {
			panic!("timeout draining the shrunken pool: {completed}/12");
		}
		std::thread::sleep(Duration::from_millis(2));
	}
	assert_eq!(completed, 12, "all tickets completed before the resize settles");
	// The pool reached the target (the shrink took effect).
	assert_eq!(dispatcher.worker_count(), 1);
	// Let the retired children exit naturally (their EOF is reaped on
	// the next poll); never kill them mid-frame.
	std::thread::sleep(Duration::from_millis(100));
	dispatcher.poll();

	// Grow back to 3: fresh workers spawn, a new wave renders fine.
	dispatcher.set_target_workers(3);
	let deadline = Instant::now() + Duration::from_secs(60);
	while dispatcher.worker_count() < 3 && Instant::now() < deadline {
		dispatcher.poll();
		std::thread::sleep(Duration::from_millis(5));
	}
	assert_eq!(dispatcher.worker_count(), 3);
	let results2 = Arc::new(Mutex::new(Vec::new()));
	submit(&dispatcher, &results2, 6, None);
	pump_until(&dispatcher, &results2, 6);
	let frames: Vec<TicketResult> =
		results2.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
	assert_eq!(frames.len(), 6, "the regrown pool renders a new wave");
	for result in frames {
		let payload = result.expect("regrown wave rendered");
		if let TicketPayload::ShmFrame(frame) = payload {
			dispatcher.release_frame(&frame);
		}
	}
	dispatcher.shutdown();
}

/// Two real workers render two waves of generated frames into shm slots;
/// the main process never copies frame bytes. Slots are released as
/// frames arrive (the dispatcher's credit-based flow control then keeps
/// the remaining tickets flowing).
#[test]
fn two_workers_render_two_waves_zero_copy() {
	let _guard = lock_test();
	let dispatcher = ProcessDispatcher::new(config(2, 4)).expect("dispatcher config");
	dispatcher.start().expect("workers start + handshake");
	assert_eq!(dispatcher.worker_count(), 2);
	assert!(dispatcher.is_alive(0));
	assert!(dispatcher.is_alive(1));

	reset_main_heap_frame_copies();

	// Wave 1: more tickets than slots in one worker, so both workers and
	// the slot-recycling path are exercised.
	let results = Arc::new(Mutex::new(Vec::new()));
	submit(&dispatcher, &results, 12, None);

	let mut seen_worker = [false; 2];
	let mut completed = 0usize;
	let deadline = Instant::now() + Duration::from_secs(60);
	while completed < 12 {
		dispatcher.poll();
		let drained: Vec<TicketResult> =
			results.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
		for result in drained {
			let payload = result.expect("frame rendered");
			let TicketPayload::ShmFrame(frame) = payload else {
				panic!("process backend must deliver ShmFrame payloads");
			};
			assert!(frame.worker < 2);
			assert!(frame.slot < 4);
			assert_eq!(frame.meta.width, 64);
			assert_eq!(frame.meta.height, 64);
			assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
			assert_eq!(frame.meta.data_size, 64 * 64 * 4);
			assert_eq!(frame.meta.linesize, 64 * 4);
			// Generated frame (transparent black) converted to BGRA8: all
			// zero. Read through the mapping — never `slot_to_vec` (the
			// counted copy path).
			let pixels = frame.shm.slot_bytes(frame.slot);
			assert!(
				pixels[..frame.meta.data_size as usize]
					.iter()
					.all(|&b| b == 0),
				"generated frame is transparent black"
			);
			seen_worker[frame.worker as usize] = true;
			dispatcher.release_frame(&frame);
			completed += 1;
		}
		if Instant::now() > deadline {
			panic!("timeout: {completed}/12 completions");
		}
		if completed < 12 {
			std::thread::sleep(Duration::from_millis(5));
		}
	}
	// Zero copy: nothing bumped the main-process frame-copy counter.
	assert_eq!(main_heap_frame_copies(), 0);
	// Both workers participated (interleaved sharded claiming).
	assert!(seen_worker[0], "worker 0 rendered at least one frame");
	assert!(seen_worker[1], "worker 1 rendered at least one frame");

	// Wave 2 through the same recycled slots.
	submit(&dispatcher, &results, 8, None);
	pump_until(&dispatcher, &results, 8);
	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("second wave rendered");
		let TicketPayload::ShmFrame(frame) = payload else {
			panic!("ShmFrame payload");
		};
		dispatcher.release_frame(&frame);
	}
	assert_eq!(main_heap_frame_copies(), 0);

	dispatcher.shutdown();
	assert_eq!(main_heap_frame_copies(), 0);
}

/// A worker crashing mid-render (SIGSEGV hook) must not take down the
/// main process: the frame is re-queued, the worker restarted and the
/// ticket still completes with a rendered frame.
#[test]
fn crash_isolation_restarts_worker_and_frame_still_renders() {
	let _guard = lock_test();

	// One-shot crash hook: the worker dies with SIGSEGV while rendering
	// ticket 1; the marker file it leaves behind makes the restarted
	// worker render the re-queued frame for real.
	let marker = std::env::temp_dir().join(format!(
		"oak-procpool-crash-marker-{}",
		std::process::id()
	));
	let _ = std::fs::remove_file(&marker);
	std::env::set_var("OAK_WORKER_CRASH_ON_TICKET", "1");
	std::env::set_var("OAK_WORKER_CRASH_MARKER", &marker);
	struct EnvGuard;
	impl Drop for EnvGuard {
		fn drop(&mut self) {
			std::env::remove_var("OAK_WORKER_CRASH_ON_TICKET");
			std::env::remove_var("OAK_WORKER_CRASH_MARKER");
		}
	}
	let _env_guard = EnvGuard;

	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");
	assert_eq!(dispatcher.worker_count(), 1);

	let results = Arc::new(Mutex::new(Vec::new()));
	// The dispatcher's first ticket id is 1 — exactly the crash ticket.
	submit(&dispatcher, &results, 4, None);
	pump_until(&dispatcher, &results, 4);

	// The crash hit the worker (it restarted at least once)...
	assert!(
		dispatcher.restarts_of(0) >= 1,
		"crashed worker must be restarted (restarts={})",
		dispatcher.restarts_of(0)
	);
	// ...and every ticket still completed with a real frame.
	let mut crashed_ticket_seen = false;
	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("frame rendered despite the worker crash");
		let TicketPayload::ShmFrame(frame) = payload else {
			panic!("ShmFrame payload");
		};
		if frame.meta.id == 1 {
			crashed_ticket_seen = true;
			assert_eq!(frame.meta.width, 64);
			assert_eq!(frame.meta.data_size, 64 * 64 * 4);
		}
		dispatcher.release_frame(&frame);
	}
	assert!(crashed_ticket_seen, "ticket 1 delivered after the restart");
	assert!(marker.exists(), "the crash hook fired exactly once");
	let _ = std::fs::remove_file(&marker);

	dispatcher.shutdown();
}

/// Footage decode inside the worker process: a real H.264 frame from
/// `tests/demo.mp4` is decoded, scaled and converted into a BGRA8 slot.
#[test]
fn worker_decodes_real_footage_into_slot() {
	let _guard = lock_test();
	let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../oak-app/tests/demo.mp4");
	assert!(demo.exists(), "repo fixture tests/demo.mp4 missing");

	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	let results = Arc::new(Mutex::new(Vec::new()));
	submit(
		&dispatcher,
		&results,
		2,
		Some((demo.display().to_string(), 0)),
	);
	pump_until(&dispatcher, &results, 2);

	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("footage frame rendered");
		let TicketPayload::ShmFrame(frame) = payload else {
			panic!("ShmFrame payload");
		};
		assert_eq!(frame.meta.width, 64);
		assert_eq!(frame.meta.height, 64);
		assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
		// Decoded video is opaque: every BGRA alpha byte is 255.
		let pixels = &frame.shm.slot_bytes(frame.slot)[..frame.meta.data_size as usize];
		let alpha_ok = pixels
			.chunks_exact(4)
			.filter(|px| px[3] == 255)
			.count();
		assert!(
			alpha_ok as f64 >= 0.99 * (64 * 64) as f64,
			"decoded frame must be opaque ({alpha_ok}/4096)"
		);
		dispatcher.release_frame(&frame);
	}

	dispatcher.shutdown();
}

/// M14 R3 end-to-end: a montage clip carrying an effect stack renders
/// through a REAL worker process — the effects ride the wire (protocol
/// v2 additive `effects` field on the montage clip), the worker applies
/// the stack between decode and compositing, and the main process reads
/// the changed pixels back from the shm slot. 50% Opacity quarters the
/// output (the shader halves every channel; the composite over
/// transparent black halves again via the halved alpha).
#[test]
fn montage_effects_render_through_the_worker() {
	let _guard = lock_test();
	let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../oak-app/tests/demo.mp4");
	assert!(demo.exists(), "repo fixture tests/demo.mp4 missing");

	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	let clip = |effects| oak_render::ticket::MontageClip {
		filename: demo.display().to_string(),
		stream_index: 0,
		in_time: Rational::new(0, 1),
		out_time: Rational::new(10, 1),
		media_in: Rational::new(0, 1),
		gain: 1.0,
		effects,
	};
	let montages = [
		vec![clip(Vec::new())],
		vec![clip(vec![oak_render::ticket::MontageEffect {
			type_id: "org.olivevideoeditor.Olive.opacity".into(),
			enabled: true,
			effect_input_id: Some("tex_in".into()),
			params: vec![(
				"opacity_in".into(),
				oak_node::value::NodeValue::Float(0.5),
			)],
		}])],
	];

	let results = Arc::new(Mutex::new(Vec::new()));
	for (i, montage) in montages.into_iter().enumerate() {
		let results = results.clone();
		let time = Rational::new(i as i64, 25);
		let mut p = params(time, None).as_ref().clone();
		p.montage = montage;
		let job = Job {
			node_identity: 1,
			time,
			params: Arc::new(p),
			audio: None,
			produce: Arc::new(|_, _| {
				Err(oak_render::error::Error::Failed(
					"process backend does not use the in-process producer".into(),
				))
			}),
			done: Box::new(move |result| {
				results.lock().unwrap_or_else(|e| e.into_inner()).push(result);
			}),
			schedule: JobSchedule::seek(),
		};
		assert!(dispatcher.post(job), "post accepted while alive");
	}
	pump_until(&dispatcher, &results, 2);

	// Results arrive in ticket order (one batch, in-order rendering).
	let mut frames: Vec<Vec<u8>> = Vec::new();
	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("montage frame rendered");
		let TicketPayload::ShmFrame(frame) = payload else {
			panic!("ShmFrame payload");
		};
		frames.push(frame.shm.slot_bytes(frame.slot)[..frame.meta.data_size as usize].to_vec());
		dispatcher.release_frame(&frame);
	}
	dispatcher.shutdown();

	let [plain, dimmed] = &frames[..] else {
		panic!("two frames rendered");
	};
	// Pick an opaque, non-black pixel in the plain render as the probe.
	let probe = plain
		.chunks_exact(4)
		.position(|px| px[3] == 255 && px[0] > 40)
		.expect("the fixture frame has an opaque non-black pixel");
	let p = &plain[probe * 4..probe * 4 + 4];
	let d = &dimmed[probe * 4..probe * 4 + 4];
	assert_eq!(p[3], 255, "the plain montage render is opaque");
	assert!(
		(d[3] as i32 - 128).abs() <= 3,
		"50% opacity halves the alpha ({} vs 128)",
		d[3]
	);
	// 50% opacity reduces the color. The exact ratio depends on the color
	// pipeline (gamma-encoded vs linear working space), so assert the
	// pipeline-agnostic property: dimmed is darker than plain, but not
	// black (the opacity effect actually changed the pixel).
	assert!(
		d[0] < p[0],
		"50% opacity darkens the color ({} vs {})",
		d[0],
		p[0]
	);
	assert!(d[0] > 0, "dimmed pixel is not black ({})", d[0]);
}

/// Submit an empty-montage audio range pull through the dispatcher
/// (M15 S3): the worker mixes silence into a shm slot and the main
/// process reads it back as [`TicketPayload::ShmAudio`].
fn submit_audio(
	dispatcher: &ProcessDispatcher,
	results: &Arc<Mutex<Vec<TicketResult>>>,
	viewer: u64,
	start: Rational,
	duration: Rational,
) {
	let range = TimeRange::new(start, start + duration);
	let audio = Arc::new(AudioTicketParams {
		viewer,
		range,
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
	});
	let results = results.clone();
	let job = Job {
		node_identity: viewer,
		time: start,
		params: Arc::new(VideoTicketParams {
			viewer,
			project: String::new(),
			time: start,
			force_size: None,
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		}),
		audio: Some(audio),
		// Never invoked on the process backend (the worker renders audio
		// from the wire spec); must still be a valid producer.
		produce: Arc::new(|_, _| {
			Err(oak_render::error::Error::Failed(
				"process backend does not use the in-process producer".into(),
			))
		}),
		done: Box::new(move |result| {
			results.lock().unwrap_or_else(|e| e.into_inner()).push(result);
		}),
		schedule: JobSchedule::seek(),
	};
	assert!(dispatcher.post(job), "post accepted while alive");
}

/// Audio tickets flow through the worker pool into shm slots (M15 S3):
/// empty-montage ranges render as interleaved f32 silence, the main
/// process reads them back with `ShmAudioRef::samples()` (zero copy —
/// the copy counter stays 0) and releases the slots.
#[test]
fn audio_tickets_roundtrip_through_shm_slots() {
	let _guard = lock_test();
	let dispatcher = ProcessDispatcher::new(config(2, 4)).expect("dispatcher config");
	dispatcher.start().expect("workers start");

	reset_main_heap_frame_copies();

	let results = Arc::new(Mutex::new(Vec::new()));
	// 1/24 s at 48 kHz stereo = 2000 sample frames x 2 ch = 16000 bytes —
	// fits the 64x64 BGRA8 slot (16384 bytes).
	for i in 0..4 {
		submit_audio(&dispatcher, &results, 1, Rational::new(i, 24), Rational::new(1, 24));
	}
	pump_until(&dispatcher, &results, 4);

	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("audio rendered");
		let TicketPayload::ShmAudio(audio) = payload else {
			panic!("audio tickets must deliver ShmAudio payloads");
		};
		assert_eq!(audio.sample_rate, 48000);
		assert_eq!(audio.channel_count, 2);
		assert_eq!(audio.meta.format, oak_render::ipc::SLOT_FORMAT_AUDIO_F32);
		assert_eq!(audio.meta.channel_count, 2);
		assert_eq!(audio.meta.linesize, 2 * 4);
		assert_eq!(audio.meta.data_size, 2000 * 2 * 4);
		// Empty montage: total silence, parsed back as f32.
		let samples = audio.samples();
		assert_eq!(samples.len(), 2000 * 2);
		assert!(samples.iter().all(|&v| v == 0.0), "empty montage is silence");
		// Audio tickets are Seek priority — claimable by ANY worker (the
		// seek-starvation fix), so there is no shard-spread assertion; what
		// matters is that every ticket rendered on a live worker.
		assert!(audio.worker < 2, "rendered on a live worker");
		dispatcher.release_audio_frame(&audio);
	}
	// Samples are read via the slot mapping and parsed into a Vec — never
	// through the counted `slot_to_vec` copy path.
	assert_eq!(main_heap_frame_copies(), 0);

	dispatcher.shutdown();
}

/// A claim batch that mixes audio and video tickets must assign slots in
/// the worker's acquisition order (the video message is processed first,
/// the audio message second). Interleaved assignment scrambled the
/// worker's free ring and flooded "slot assignment mismatch" failures
/// during playback — this is the regression guard.
#[test]
fn mixed_audio_video_batch_keeps_slot_assignment_order() {
	let _guard = lock_test();
	// One worker, four slots: the first four posts dispatch singly (post
	// pumps once itself), the remaining posts queue behind the busy
	// slots. As completions are released below, claims gather up to
	// `batch_size` queued tickets — mixed audio/video batches.
	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	let results = Arc::new(Mutex::new(Vec::new()));
	for i in 0..8 {
		submit(&dispatcher, &results, 1, None);
		submit_audio(
			&dispatcher,
			&results,
			1,
			Rational::new(i, 24),
			Rational::new(1, 24),
		);
	}
	// Drain completions, releasing each slot immediately so the queued
	// tickets keep flowing into new (mixed) claims.
	let deadline = Instant::now() + Duration::from_secs(60);
	let mut total = 0usize;
	loop {
		dispatcher.poll();
		let done: Vec<TicketResult> = results
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.drain(..)
			.collect();
		total += done.len();
		for result in &done {
			match result {
				Ok(TicketPayload::ShmFrame(frame)) => dispatcher.release_frame(frame),
				Ok(TicketPayload::ShmAudio(audio)) => dispatcher.release_audio_frame(audio),
				Err(e) => panic!("mixed-batch ticket failed: {e}"),
				_ => panic!("unexpected payload variant"),
			}
		}
		if total >= 16 {
			break;
		}
		if Instant::now() > deadline {
			panic!("timeout waiting for the mixed batch ({total}/16)");
		}
		std::thread::sleep(Duration::from_millis(2));
	}

	dispatcher.shutdown();
}

/// An audio render crashing mid-mix (SIGSEGV hook) must not take down the
/// main process: the audio ticket is re-queued, the worker restarted and
/// the samples still arrive.
#[test]
fn audio_crash_isolation_restarts_worker_and_audio_still_renders() {
	let _guard = lock_test();

	let marker = std::env::temp_dir().join(format!(
		"oak-procpool-audio-crash-marker-{}",
		std::process::id()
	));
	let _ = std::fs::remove_file(&marker);
	std::env::set_var("OAK_WORKER_CRASH_ON_TICKET", "1");
	std::env::set_var("OAK_WORKER_CRASH_MARKER", &marker);
	struct EnvGuard;
	impl Drop for EnvGuard {
		fn drop(&mut self) {
			std::env::remove_var("OAK_WORKER_CRASH_ON_TICKET");
			std::env::remove_var("OAK_WORKER_CRASH_MARKER");
		}
	}
	let _env_guard = EnvGuard;

	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	let results = Arc::new(Mutex::new(Vec::new()));
	// The dispatcher's first ticket id is 1 — exactly the crash ticket
	// (an audio ticket this time).
	for i in 0..4 {
		submit_audio(&dispatcher, &results, 1, Rational::new(i, 24), Rational::new(1, 24));
	}
	pump_until(&dispatcher, &results, 4);

	assert!(
		dispatcher.restarts_of(0) >= 1,
		"crashed audio worker must be restarted (restarts={})",
		dispatcher.restarts_of(0)
	);
	let mut crashed_ticket_seen = false;
	for result in results.lock().unwrap().drain(..) {
		let payload = result.expect("audio rendered despite the worker crash");
		let TicketPayload::ShmAudio(audio) = payload else {
			panic!("ShmAudio payload");
		};
		if audio.meta.id == 1 {
			crashed_ticket_seen = true;
			assert_eq!(audio.sample_rate, 48000);
			assert_eq!(audio.meta.data_size, 2000 * 2 * 4);
		}
		dispatcher.release_audio_frame(&audio);
	}
	assert!(crashed_ticket_seen, "ticket 1 delivered after the restart");
	assert!(marker.exists(), "the crash hook fired exactly once");
	let _ = std::fs::remove_file(&marker);

	dispatcher.shutdown();
}

/// An audio range too large for a practical shm slot (> 64 MB, i.e. a
/// multi-minute export) is refused by the dispatcher's `post`, so the
/// arena can fall back to main-process inline rendering (design §3.7)
/// instead of forcing a giant shared-memory segment.
#[test]
fn oversized_audio_ticket_is_refused_by_process_backend() {
	let _guard = lock_test();
	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	// ~3 min of 48 kHz stereo > 64 MB: post refuses it (false).
	let duration = Rational::new(175, 1); // 175 s
	let results = Arc::new(Mutex::new(Vec::new()));
	let results2 = results.clone();
	let audio = Arc::new(AudioTicketParams {
		viewer: 1,
		range: TimeRange::new(Rational::new(0, 1), duration),
		sample_rate: 48000,
		channel_layout: 0x3,
		montage: Vec::new(),
	});
	let job = Job {
		node_identity: 1,
		time: Rational::new(0, 1),
		params: Arc::new(VideoTicketParams {
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
			adjustments: Vec::new(),
		}),
		audio: Some(audio),
		produce: Arc::new(|_, _| {
			Err(oak_render::error::Error::Failed(
				"process backend does not use the in-process producer".into(),
			))
		}),
		done: Box::new(move |_| {
			results2.lock().unwrap_or_else(|e| e.into_inner()).push(());
		}),
		schedule: JobSchedule::seek(),
	};
	assert!(
		!dispatcher.post(job),
		"oversized audio must be refused so the arena falls back inline"
	);
	assert_eq!(results.lock().unwrap().len(), 0, "no completion fires");

	dispatcher.shutdown();
}
/// Per-ticket slot formats (M15 S3): a forced-F32 ticket gets an F32 slot
/// (the segment grows on demand) while the default BGRA8 tickets keep
/// BGRA8 slots — the export path's F32 request no longer round-trips
/// through BGRA8.
#[test]
fn f32_ticket_gets_f32_slot_and_bgra8_stays_bgra8() {
	let _guard = lock_test();
	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	// 1. Default BGRA8 preview ticket first.
	let results = Arc::new(Mutex::new(Vec::new()));
	submit(&dispatcher, &results, 1, None);
	pump_until(&dispatcher, &results, 1);
	let bg = results.lock().unwrap().pop().unwrap().expect("frame rendered");
	let TicketPayload::ShmFrame(frame) = bg else {
		panic!("ShmFrame payload");
	};
	assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
	assert_eq!(frame.meta.data_size, 64 * 64 * 4);
	dispatcher.release_frame(&frame);

	// 2. Forced-F32 ticket: the segment grows on demand and the slot holds
	//    F32 RGBA (16 bytes per pixel).
	let results2 = Arc::new(Mutex::new(Vec::new()));
	{
		let results2 = results2.clone();
		let job = Job {
			node_identity: 1,
			time: Rational::new(0, 1),
			params: Arc::new(VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(0, 1),
				force_size: Some((64, 64)),
				force_format: Some(PixelFormat::F32),
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage: Vec::new(),
				adjustments: Vec::new(),
			}),
			audio: None,
			produce: Arc::new(|_, _| {
				Err(oak_render::error::Error::Failed(
					"process backend does not use the in-process producer".into(),
				))
			}),
			done: Box::new(move |result| {
				results2.lock().unwrap_or_else(|e| e.into_inner()).push(result);
			}),
			schedule: JobSchedule::seek(),
		};
		assert!(dispatcher.post(job), "f32 post accepted");
	}
	pump_until(&dispatcher, &results2, 1);
	let f32res = results2.lock().unwrap().pop().unwrap().expect("f32 frame rendered");
	let TicketPayload::ShmFrame(frame) = f32res else {
		panic!("ShmFrame payload");
	};
	assert_eq!(frame.meta.format, PixelFormat::F32 as i32);
	assert_eq!(frame.meta.data_size, 64 * 64 * 16);
	// The F32 bytes are zero (transparent black pipeline output).
	let pixels = frame.shm.slot_bytes(frame.slot);
	assert!(
		pixels[..frame.meta.data_size as usize].iter().all(|&b| b == 0),
		"generated F32 frame is transparent black"
	);
	dispatcher.release_frame(&frame);

	// 3. A later default BGRA8 ticket still lands as BGRA8 in the grown
	//    segment (the wire format is per ticket).
	submit(&dispatcher, &results, 1, None);
	pump_until(&dispatcher, &results, 1);
	let bg2 = results.lock().unwrap().pop().unwrap().expect("frame rendered");
	let TicketPayload::ShmFrame(frame) = bg2 else {
		panic!("ShmFrame payload");
	};
	assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
	assert_eq!(frame.meta.data_size, 64 * 64 * 4);
	dispatcher.release_frame(&frame);

	dispatcher.shutdown();
}

/// One sequence with a single video track holding one clip over `clip`
/// (range [0, 1)); returns the project and the sequence node.
fn build_graph_project(clip: &std::path::Path) -> (Arc<Mutex<Project>>, NodeId) {
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();

		// Footage is created first so the sequence lands on slot 1
		// (identity != 0): identity 0 is the "no viewer" sentinel in
		// ticket specs, and the graph branch short-circuits on it.
		let mut footage = FootageBehavior::new(clip.to_str().unwrap());
		footage.probe().expect("probe the test clip");
		let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

		let (score, sbehavior) = SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);

		let (tcore, tbehavior) = TrackListBehavior::create();
		let tl = p.graph.add_node(tcore, tbehavior);

		let (tcore, tbehavior) = TrackBehavior::create();
		let track = p.graph.add_node(tcore, tbehavior);

		let (ccore, cbehavior) = oak_node::block::clip_create();
		let clip_node = p.graph.add_node(ccore, cbehavior);
		p.graph
			.connect(footage, clip_node, oak_node::block::clip_input::TEXTURE_INPUT, -1)
			.expect("connect footage to clip");

		let clip_behavior = p
			.graph
			.get_mut(clip_node)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<ClipBlockBehavior>()
			.expect("clip block");
		clip_behavior.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 1));

		p.graph
			.get_mut(track)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackBehavior>()
			.expect("video track")
			.append_block(clip_node);
		p.graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.expect("video track list")
			.tracks
			.push(track);
		p.graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<SequenceBehavior>()
			.expect("sequence")
			.track_lists
			.push(tl);
	}
	(project, seq)
}

/// Post a single graph-mode ticket rendering the sequence `viewer` at
/// t=0 through the worker pool. `project_uuid` must be the owning project's
/// uuid (M16 S1: workers render graph tickets only when the snapshot's
/// project matches the ticket's).
fn post_graph_job(
	dispatcher: &ProcessDispatcher,
	results: &Arc<Mutex<Vec<TicketResult>>>,
	viewer: u64,
	project_uuid: &str,
) {
	let results = results.clone();
	let job = Job {
		node_identity: viewer,
		time: Rational::new(0, 1),
		params: Arc::new(VideoTicketParams {
			viewer,
			project: project_uuid.to_string(),
			time: Rational::new(0, 1),
			force_size: Some((64, 64)),
			force_format: None,
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage: Vec::new(),
			adjustments: Vec::new(),
		}),
		audio: None,
		produce: Arc::new(|_, _| {
			Err(oak_render::error::Error::Failed(
				"process backend does not use the in-process producer".into(),
			))
		}),
		done: Box::new(move |result| {
			results.lock().unwrap_or_else(|e| e.into_inner()).push(result);
		}),
		schedule: JobSchedule::seek(),
	};
	assert!(dispatcher.post(job), "post accepted while alive");
}

/// Assert a delivered ticket is a 64x64 BGRA8 slot carrying opaque
/// non-black content — the signature of a real graph render (the
/// transparent-black montage fallback would be all zero), then release
/// the slot.
fn assert_graph_frame_opaque(dispatcher: &ProcessDispatcher, payload: TicketResult) {
	let payload = payload.expect("graph-mode frame rendered");
	let TicketPayload::ShmFrame(frame) = payload else {
		panic!("graph-mode tickets must deliver ShmFrame payloads");
	};
	assert_eq!(frame.meta.width, 64);
	assert_eq!(frame.meta.height, 64);
	assert_eq!(frame.meta.format, SLOT_FORMAT_BGRA8);
	assert_eq!(frame.meta.data_size, 64 * 64 * 4);
	let pixels = &frame.shm.slot_bytes(frame.slot)[..frame.meta.data_size as usize];
	let alpha_ok = pixels.chunks_exact(4).filter(|px| px[3] == 255).count();
	assert!(
		alpha_ok as f64 >= 0.99 * (64 * 64) as f64,
		"graph-rendered frame must be opaque ({alpha_ok}/4096)"
	);
	assert!(
		pixels.iter().any(|&b| b != 0),
		"graph-rendered frame is not black"
	);
	dispatcher.release_frame(&frame);
}

/// M16 S1 end-to-end graph mode: the dispatcher starts with a real
/// project snapshot (oaknode XML), the worker loads it right after the
/// handshake, and a ticket naming the sequence node as its viewer
/// renders the sequence's clip through the graph path into the slot.
#[test]
fn graph_mode_renders_sequence_viewer_from_snapshot() {
	let _guard = lock_test();

	let clip = std::env::temp_dir().join(format!(
		"oak-procpool-graph-clip-{}.mp4",
		std::process::id()
	));
	let snapshot = std::env::temp_dir().join(format!(
		"oak-procpool-graph-snapshot-{}.xml",
		std::process::id()
	));
	let _ = std::fs::remove_file(&clip);
	let _ = std::fs::remove_file(&snapshot);
	oak_codec::testmedia::write_test_clip(&clip, 64, 64, 10, 10).expect("test clip generation");

	let (project, seq) = build_graph_project(&clip);
	let xml = {
		let p = project.lock().unwrap_or_else(|e| e.into_inner());
		oak_node::serializer::save(&p).expect("project serializes")
	};
	std::fs::write(&snapshot, xml).expect("snapshot written");
	let viewer = seq.identity();
	assert_ne!(viewer, 0, "viewer must not be the no-viewer sentinel 0");

	let mut cfg = config(1, 4);
	cfg.graph_snapshot = Some(snapshot.display().to_string());
	let dispatcher = ProcessDispatcher::new(cfg).expect("dispatcher config");
	dispatcher.start().expect("worker starts");

	let results = Arc::new(Mutex::new(Vec::new()));
	let project_uuid = project.lock().unwrap_or_else(|e| e.into_inner()).uuid.clone();
	post_graph_job(&dispatcher, &results, viewer, &project_uuid);
	pump_until(&dispatcher, &results, 1);

	let result = results.lock().unwrap_or_else(|e| e.into_inner()).pop().unwrap();
	assert_graph_frame_opaque(&dispatcher, result);

	dispatcher.shutdown();
	let _ = std::fs::remove_file(&clip);
	let _ = std::fs::remove_file(&snapshot);
}

/// M16 S1: a snapshot pushed after startup reroutes subsequent viewer
/// tickets — `set_graph_snapshot` sends `load_graph` down the same FIFO
/// as the batches, so the worker loads the graph before it claims the
/// ticket (no restart needed).
#[test]
fn set_graph_snapshot_after_start_reroutes_tickets() {
	let _guard = lock_test();

	let clip = std::env::temp_dir().join(format!(
		"oak-procpool-reroute-clip-{}.mp4",
		std::process::id()
	));
	let snapshot = std::env::temp_dir().join(format!(
		"oak-procpool-reroute-snapshot-{}.xml",
		std::process::id()
	));
	let _ = std::fs::remove_file(&clip);
	let _ = std::fs::remove_file(&snapshot);
	oak_codec::testmedia::write_test_clip(&clip, 64, 64, 10, 10).expect("test clip generation");

	let (project, seq) = build_graph_project(&clip);
	let xml = {
		let p = project.lock().unwrap_or_else(|e| e.into_inner());
		oak_node::serializer::save(&p).expect("project serializes")
	};
	std::fs::write(&snapshot, xml).expect("snapshot written");
	let viewer = seq.identity();
	assert_ne!(viewer, 0, "viewer must not be the no-viewer sentinel 0");

	let dispatcher = ProcessDispatcher::new(config(1, 4)).expect("dispatcher config");
	dispatcher.start().expect("worker starts");
	dispatcher.set_graph_snapshot(Some(snapshot.display().to_string()));

	let results = Arc::new(Mutex::new(Vec::new()));
	let project_uuid = project.lock().unwrap_or_else(|e| e.into_inner()).uuid.clone();
	post_graph_job(&dispatcher, &results, viewer, &project_uuid);
	pump_until(&dispatcher, &results, 1);

	let result = results.lock().unwrap_or_else(|e| e.into_inner()).pop().unwrap();
	assert_graph_frame_opaque(&dispatcher, result);

	dispatcher.shutdown();
	let _ = std::fs::remove_file(&clip);
	let _ = std::fs::remove_file(&snapshot);
}
