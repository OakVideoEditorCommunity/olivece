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

//! The M1 thread pipeline: the same ticket stream on the inline (test)
//! backend and on the thread pipeline must produce byte-identical frames.
//!
//! Covers the pipeline backend selection (`OAK_PIPELINE=threads`), the
//! decode-service LRU under seek patterns, the prefetch gate under render
//! queue backpressure, and the decode-service install/uninstall lifecycle.
//! The manager singleton, the `OAK_PIPELINE` variable and the decode
//! service slot are process-wide, so every test here serializes on `LOCK`
//! and each creates and tears its manager down explicitly.

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use oak_core::commonutil::ENV_TEST_LOCK;
use oak_core::texture::{Frame, Texture};
use oak_core::{PixelFormat, Rational, TimeRange};

use oak_node::block::{clip_create, clip_input, ClipBlockBehavior};
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior};

use oak_render::error::Error;
use oak_render::eval::{decode_invocations, reset_decode_invocations};
use oak_render::manager::{RenderBackendChoice, RenderManager};
use oak_render::pipeline::{
	decode_service, DecodeRequest, DecodeStats, PipelineBackend, PipelineStats,
	RENDER_QUEUE_CAP,
};
use oak_render::ticket::{
	Completion, MontageClip, Producer, TicketPayload, TicketResult, VideoTicketParams,
};
use oak_render::worker::{Job, JobDispatch, JobSchedule};

mod common;

/// Serializes this binary's tests: the manager singleton, the environment
/// variable, the decode-service slot and the eval decode caches are all
/// process-wide.
static LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
	LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A unique clip per test (the process id separates test binaries, the
/// tag separates tests inside one binary — the decode caches are
/// process-wide).
fn test_clip(tag: &str) -> PathBuf {
	let path = std::env::temp_dir().join(format!(
		"oakrender_threads_{tag}_{}.mp4",
		std::process::id()
	));
	oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10)
		.expect("test clip generation");
	path
}

/// A second copy of `src` under a fresh name: the decode and eval frame
/// caches are keyed by filename, so the two backends must not share one
/// (otherwise the pipeline run would replay the inline run's cache instead
/// of decoding).
fn test_clip_copy(src: &Path, tag: &str) -> PathBuf {
	let path = std::env::temp_dir().join(format!(
		"oakrender_threads_{tag}_{}.mp4",
		std::process::id()
	));
	std::fs::copy(src, &path).expect("copy the test clip");
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

fn base_params(time: Rational) -> VideoTicketParams {
	VideoTicketParams {
		viewer: 0,
		project: String::new(),
		time,
		force_size: Some((64, 64)),
		force_format: Some(PixelFormat::F32),
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: Vec::new(),
		adjustments: Vec::new(),
	}
}

/// A one-clip montage ticket over `[0s, 1s)`.
fn montage_params(filename: &Path, time: Rational) -> VideoTicketParams {
	VideoTicketParams {
		montage: vec![MontageClip {
			filename: filename.to_string_lossy().to_string(),
			stream_index: 0,
			in_time: Rational::new(0, 1),
			out_time: Rational::new(1, 1),
			media_in: Rational::new(0, 1),
			gain: 1.0,
			effects: Vec::new(),
		}],
		..base_params(time)
	}
}

/// A viewer ticket: the manager's graph mode renders `viewer` of the
/// project whose uuid is `uuid` (armed via `set_inline_project`).
fn viewer_params(uuid: &str, viewer: u64, time: Rational) -> VideoTicketParams {
	VideoTicketParams {
		viewer,
		project: uuid.to_string(),
		..base_params(time)
	}
}

/// Submit one video ticket and wait for its frame. The reserved id keeps
/// the arena slot alive past the completion; `result()` reaps it.
fn render_video(params: VideoTicketParams) -> Texture {
	let manager = RenderManager::global().expect("manager installed");
	let id = manager.tickets.next_id();
	let (tx, rx) = mpsc::sync_channel::<TicketResult>(1);
	let done: Completion = Box::new(move |result: TicketResult| {
		let _ = tx.send(result);
	});
	manager.tickets.submit_video_with_id(id, params, done);
	let result = rx
		.recv_timeout(Duration::from_secs(60))
		.expect("ticket completed within 60s");
	let _ = manager.tickets.result(id);
	match result {
		Ok(TicketPayload::Video(texture)) => texture,
		Ok(other) => panic!("unexpected ticket payload: {other:?}"),
		Err(e) => panic!("ticket failed: {e}"),
	}
}

fn frame_of(texture: &Texture) -> &Frame {
	let Texture::Cpu(frame) = texture else {
		panic!("ticket produced a non-CPU texture");
	};
	frame
}

/// Byte-for-byte frame equality with a first-difference report.
fn assert_same_frame(expected: &Frame, actual: &Frame, tag: &str) {
	assert_eq!(
		(expected.width, expected.height),
		(actual.width, actual.height),
		"{tag}: frame size"
	);
	assert_eq!(expected.format, actual.format, "{tag}: pixel format");
	assert_eq!(expected.channels, actual.channels, "{tag}: channel count");
	assert_eq!(expected.data.len(), actual.data.len(), "{tag}: data length");
	if let Some(offset) = expected
		.data
		.iter()
		.zip(actual.data.iter())
		.position(|(a, b)| a != b)
	{
		let stride = expected.linesize_bytes();
		let row = offset / stride;
		let byte_column = offset % stride;
		panic!(
			"{tag}: pixel bytes differ at offset {offset} (row {row}, byte column {byte_column}): \
			 expected {}, got {}",
			expected.data[offset], actual.data[offset]
		);
	}
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

/// Poll `check` until it holds (10s cap) — the pipeline counters advance
/// on the render thread, so a fixed sleep is both slow and flaky.
fn wait_until(what: &str, check: &mut dyn FnMut() -> bool) {
	let deadline = Instant::now() + Duration::from_secs(10);
	if check() {
		return;
	}
	while Instant::now() < deadline {
		std::thread::sleep(Duration::from_millis(5));
		if check() {
			return;
		}
	}
	panic!("timed out waiting for {what}");
}

/// Render `times` on the inline backend (no extra threads, no children).
fn render_inline(
	project: Option<&Arc<Mutex<Project>>>,
	times: &[Rational],
	params: impl Fn(Rational) -> VideoTicketParams,
) -> Vec<Frame> {
	let _guard = common::ManagerGuard::init();
	if let Some(project) = project {
		RenderManager::global()
			.expect("manager installed")
			.set_inline_project(project.clone());
	}
	times
		.iter()
		.map(|&time| frame_of(&render_video(params(time))).clone())
		.collect()
}

/// Render `times` on the thread pipeline, then drain and report its
/// counters. The manager is torn down before returning (the decode service
/// must be uninstalled with it).
fn render_pipeline(
	project: Option<&Arc<Mutex<Project>>>,
	times: &[Rational],
	params: impl Fn(Rational) -> VideoTicketParams,
) -> (Vec<Frame>, PipelineStats, DecodeStats) {
	let guard = common::ManagerGuard::init_with(RenderBackendChoice::Pipeline);
	let manager = RenderManager::global().expect("manager installed");
	if let Some(project) = project {
		manager.set_inline_project(project.clone());
	}
	let backend = manager
		.pipeline_backend()
		.expect("thread pipeline selected");
	reset_decode_invocations();
	let frames: Vec<Frame> = times
		.iter()
		.map(|&time| frame_of(&render_video(params(time))).clone())
		.collect();
	let rendered = frames.len() as u64;
	wait_until("all pipeline jobs executed", &mut || {
		backend.stats().executed == rendered && backend.queue_depth() == 0
	});
	let stats = backend.stats();
	let service = backend.decode_service();
	assert!(service.wait_idle(), "decode service drained");
	let decode = service.stats();
	drop(service);
	drop(backend);
	drop(manager);
	drop(guard);
	assert!(
		decode_service().is_none(),
		"manager shutdown uninstalls the decode service"
	);
	(frames, stats, decode)
}

/// A job whose producer always fails: fills the queue / proves that a
/// stopped backend refuses work.
fn filler_job() -> Job {
	let params = Arc::new(montage_params(
		Path::new("/definitely/not/here-filler.mp4"),
		Rational::new(0, 1),
	));
	let produce: Producer =
		Arc::new(|_time: Rational, _params: &VideoTicketParams| -> TicketResult {
			Err(Error::State)
		});
	Job {
		node_identity: 0,
		time: Rational::new(0, 1),
		params,
		audio: None,
		produce,
		done: Box::new(|_result: TicketResult| {}),
		schedule: JobSchedule::seek(),
	}
}

/// One sequence + one video track list with one track per clip
/// `(filename, [in, out))`. The LAST entry's track composites on top
/// (NLE stacking: the highest-numbered track is topmost).
///
/// The project is initialized like a real one, so the root folder takes
/// the first arena slot: a ticket names its viewer by `NodeId::identity`,
/// and identity 0 is the ticket API's "no graph viewer" sentinel — a
/// sequence created into slot 0 would silently take the montage fall-back
/// instead of the graph.
fn build_project(clips: &[(&str, Rational, Rational)]) -> (Arc<Mutex<Project>>, NodeId) {
	pin_legacy_working_space();
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		p.initialize().expect("initialize the project");
		let (score, sbehavior) = SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);

		let (tcore, tbehavior) = TrackListBehavior::create();
		let tl = p.graph.add_node(tcore, tbehavior);

		for &(path, in_, out) in clips {
			let (tcore, tbehavior) = TrackBehavior::create();
			let track = p.graph.add_node(tcore, tbehavior);

			let mut footage = FootageBehavior::new(path);
			footage.probe().expect("probe the generated clip");
			let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

			let (ccore, cbehavior) = clip_create();
			let clip = p.graph.add_node(ccore, cbehavior);
			p.graph
				.connect(footage, clip, clip_input::TEXTURE_INPUT, -1)
				.expect("connect footage to clip");

			let clip_behavior = p
				.graph
				.get_mut(clip)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<ClipBlockBehavior>()
				.expect("clip block");
			clip_behavior.core.range = TimeRange::new(in_, out);

			p.graph
				.get_mut(track)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.expect("video track")
				.append_block(clip);
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
		}

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

/// `OAK_PIPELINE=threads` selects the thread pipeline (the default stays
/// the process backend, which the manager-guard init below exercises as
/// the test-only inline choice).
#[test]
fn oak_pipeline_env_selects_the_thread_backend() {
	let _lock = lock();
	let _env = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	RenderManager::shutdown();
	std::env::set_var("OAK_PIPELINE", "threads");
	RenderManager::init().expect("manager init with OAK_PIPELINE=threads");
	{
		let manager = RenderManager::global().expect("manager installed");
		let backend = manager
			.pipeline_backend()
			.expect("OAK_PIPELINE=threads selects the thread pipeline");
		assert_eq!(backend.queue_depth(), 0, "idle queue");
		assert_eq!(backend.queue_free(), RENDER_QUEUE_CAP);
		let stats = backend.stats();
		assert_eq!(
			(stats.posted, stats.executed, stats.drained),
			(0, 0, 0),
			"fresh pipeline counters"
		);
		assert!(
			decode_service().is_some(),
			"the pipeline installs its decode service"
		);
	}
	RenderManager::shutdown();
	std::env::remove_var("OAK_PIPELINE");
	assert!(
		decode_service().is_none(),
		"shutdown uninstalls the decode service"
	);
	let _guard = common::ManagerGuard::init();
	assert!(
		RenderManager::global()
			.unwrap()
			.pipeline_backend()
			.is_none(),
		"the inline test backend runs no thread pipeline"
	);
}

/// Six consecutive frames: the two backends must agree byte for byte and
/// every frame must be a real decode through the service.
#[test]
fn pipeline_matches_inline_pixels_across_consecutive_frames() {
	let _lock = lock();
	pin_legacy_working_space();
	let inline_path = test_clip("consecutive_inline");
	let pipeline_path = test_clip_copy(&inline_path, "consecutive_pipeline");
	let times: Vec<Rational> = (0..6).map(|n| Rational::new(n, 10)).collect();

	let inline_frames = render_inline(None, &times, |time| montage_params(&inline_path, time));
	let (pipeline_frames, stats, decode) =
		render_pipeline(None, &times, |time| montage_params(&pipeline_path, time));

	assert_eq!((stats.posted, stats.executed, stats.drained), (6, 6, 0));
	assert_eq!(decode.requests, 6, "one decode request per frame");
	assert_eq!(decode.decodes, 6, "each frame decodes once");
	assert_eq!(decode.lru_hits, 0, "consecutive frames never repeat");
	assert_eq!(decode_invocations(), 6, "the service did the decoding");
	for (frame, time) in pipeline_frames.iter().zip(times.iter()) {
		assert_eq!(frame.timestamp, *time, "the rendered frame keeps its time");
	}
	for (index, (a, b)) in inline_frames.iter().zip(pipeline_frames.iter()).enumerate() {
		assert_same_frame(a, b, &format!("frame {index}"));
	}
	assert_known_pattern(&inline_frames[0], 0, "inline frame 0");
	assert_known_pattern(&pipeline_frames[0], 0, "pipeline frame 0");
	assert!(
		inline_frames.windows(2).any(|w| w[0].data != w[1].data),
		"the generated clip really moves between frames"
	);

	let _ = std::fs::remove_file(&inline_path);
	let _ = std::fs::remove_file(&pipeline_path);
}

/// Out-of-order seeks with a repeat: the service's LRU must absorb the
/// repeated frame, and the pixels must still match the inline path.
#[test]
fn pipeline_seek_out_of_order_matches_inline() {
	let _lock = lock();
	pin_legacy_working_space();
	let inline_path = test_clip("seek_inline");
	let pipeline_path = test_clip_copy(&inline_path, "seek_pipeline");
	let seeks: [i64; 5] = [7, 2, 5, 0, 5];
	let times: Vec<Rational> = seeks.iter().map(|&n| Rational::new(n, 10)).collect();

	let inline_frames = render_inline(None, &times, |time| montage_params(&inline_path, time));
	let (pipeline_frames, stats, decode) =
		render_pipeline(None, &times, |time| montage_params(&pipeline_path, time));

	assert_eq!((stats.posted, stats.executed, stats.drained), (5, 5, 0));
	assert_eq!(decode.requests, 5, "one decode request per seek");
	assert_eq!(decode.lru_hits, 1, "the repeated 5/10 seek hits the LRU");
	assert_eq!(decode.decodes, 4, "four distinct frames decode");
	assert_eq!(decode_invocations(), 4, "the LRU absorbed the repeat");
	for (index, (a, b)) in inline_frames.iter().zip(pipeline_frames.iter()).enumerate() {
		assert_same_frame(a, b, &format!("seek {index}"));
	}
	assert_known_pattern(&inline_frames[3], 0, "inline seek to 0/10");
	assert_known_pattern(&pipeline_frames[3], 0, "pipeline seek to 0/10");

	let _ = std::fs::remove_file(&inline_path);
	let _ = std::fs::remove_file(&pipeline_path);
}

/// The graph (viewer) path through the pipeline: the same node-graph
/// render as the inline backend — not a silent fall-back to a blank
/// generated frame (which the pattern assertions would catch).
#[test]
fn pipeline_viewer_ticket_matches_inline_pixels() {
	let _lock = lock();
	let path = test_clip("viewer");
	let filename = path.to_string_lossy().to_string();
	let clip = (filename.as_str(), Rational::new(0, 1), Rational::new(1, 1));
	let (project, sequence) = build_project(&[clip]);
	let uuid = project.lock().unwrap().uuid.clone();
	let viewer = sequence.identity();
	let times = [
		Rational::new(0, 1),
		Rational::new(3, 10),
		Rational::new(8, 10),
	];

	let inline_frames = render_inline(Some(&project), &times, |time| {
		viewer_params(&uuid, viewer, time)
	});
	let (pipeline_frames, stats, decode) = render_pipeline(Some(&project), &times, |time| {
		viewer_params(&uuid, viewer, time)
	});

	assert_eq!((stats.posted, stats.executed, stats.drained), (3, 3, 0));
	assert_eq!(decode.requests, 3, "the graph path decodes via the service");
	assert_known_pattern(&inline_frames[0], 0, "inline graph frame 0");
	assert_known_pattern(&pipeline_frames[0], 0, "pipeline graph frame 0");
	for (index, (a, b)) in inline_frames.iter().zip(pipeline_frames.iter()).enumerate() {
		assert_same_frame(a, b, &format!("graph frame {index}"));
	}

	let _ = std::fs::remove_file(&path);
}

/// A saturated render queue closes the decode service's prefetch gate: a
/// speculative decode must be refused while a frame is in flight and the
/// queue is full, and everything queued must still run once the in-flight
/// frame completes.
#[test]
fn pipeline_queue_backpressure_closes_the_prefetch_gate() {
	let _lock = lock();
	assert!(
		decode_service().is_none(),
		"the decode service slot starts empty"
	);
	let backend = PipelineBackend::new().expect("pipeline backend starts");

	// The in-flight job parks in its producer until released; that is what
	// lets this test fill the queue deterministically.
	let started = Arc::new((Mutex::new(None::<String>), Condvar::new()));
	let release = Arc::new((Mutex::new(false), Condvar::new()));
	let job_started = started.clone();
	let job_release = release.clone();
	let produce: Producer = Arc::new(
		move |_time: Rational, _params: &VideoTicketParams| -> TicketResult {
			{
				let (name, work) = &*job_started;
				*name.lock().unwrap_or_else(|e| e.into_inner()) =
					Some(std::thread::current().name().unwrap_or_default().to_string());
				work.notify_all();
			}
			let (released, work) = &*job_release;
			let mut released = released.lock().unwrap_or_else(|e| e.into_inner());
			while !*released {
				released = work.wait(released).unwrap_or_else(|e| e.into_inner());
			}
			Err(Error::State)
		},
	);
	let hold_job = Job {
		node_identity: 0,
		time: Rational::new(0, 1),
		params: Arc::new(montage_params(
			Path::new("/definitely/not/here-hold.mp4"),
			Rational::new(0, 1),
		)),
		audio: None,
		produce,
		done: Box::new(|_result: TicketResult| {}),
		schedule: JobSchedule::seek(),
	};

	assert!(backend.try_post(hold_job), "the in-flight job is accepted");
	wait_until("the render thread to pick up the in-flight job", &mut || {
		started
			.0
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.is_some()
	});
	let thread_name = started
		.0
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone();
	assert_eq!(
		thread_name.as_deref(),
		Some("oak-render"),
		"the render thread runs the producer"
	);

	let mut accepted = 0usize;
	while accepted < RENDER_QUEUE_CAP && backend.try_post(filler_job()) {
		accepted += 1;
	}
	assert_eq!(accepted, RENDER_QUEUE_CAP, "the bounded queue fills up");
	assert_eq!(backend.queue_depth(), RENDER_QUEUE_CAP);
	assert_eq!(backend.queue_free(), 0, "the prefetch gate's depth reads 0");
	assert!(
		!backend.try_post(filler_job()),
		"the queue refuses overflow"
	);

	let service = backend.decode_service();
	let refused = service.prefetch(DecodeRequest {
		filename: "/definitely/not/here-prefetch.mp4".to_string(),
		stream_index: 0,
		time: Rational::new(0, 1),
		size: (64, 64),
		format: PixelFormat::F32,
	});
	assert!(!refused, "a saturated pipeline refuses prefetch");
	let decode = service.stats();
	assert_eq!(decode.prefetch_refused, 1, "the refusal is counted");
	assert_eq!(decode.prefetches, 0, "nothing was queued");
	assert_eq!(decode.decodes, 0, "nothing was decoded");
	assert!(
		!backend.try_post(filler_job()),
		"the refused prefetch made no room"
	);

	{
		let (released, work) = &*release;
		*released.lock().unwrap_or_else(|e| e.into_inner()) = true;
		work.notify_all();
	}
	wait_until("every queued job to execute", &mut || {
		backend.stats().executed == RENDER_QUEUE_CAP as u64 + 1
			&& backend.queue_depth() == 0
	});
	let stats = backend.stats();
	assert_eq!(
		(stats.posted, stats.drained),
		(RENDER_QUEUE_CAP as u64 + 1, 0),
		"all posted jobs ran"
	);

	backend.shutdown();
	assert!(
		!backend.try_post(filler_job()),
		"shutdown rejects new work"
	);
	assert!(decode_service().is_none(), "shutdown uninstalls the service");
}

/// The backend owns the process-wide decode service slot: it is installed
/// at startup and uninstalled on shutdown.
#[test]
fn pipeline_installs_and_uninstalls_the_decode_service() {
	let _lock = lock();
	assert!(
		decode_service().is_none(),
		"the decode service slot starts empty"
	);
	let backend = PipelineBackend::new().expect("pipeline backend starts");
	let installed = decode_service().expect("decode service installed");
	assert!(Arc::ptr_eq(&installed, &backend.decode_service()));

	backend.shutdown();
	assert!(
		decode_service().is_none(),
		"shutdown uninstalls the decode service"
	);
	assert!(
		!backend.try_post(filler_job()),
		"shutdown rejects new work"
	);
}
