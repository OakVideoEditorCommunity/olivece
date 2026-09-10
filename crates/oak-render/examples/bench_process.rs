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

//! M15 S3 process-pool frame-throughput benchmark (design §3.2 scheduling
//! metrics): renders `N` 1080p BGRA8 generated frames through the real
//! oak-worker pool and reports
//!
//!   - **throughput**: total frames / wall time (frames per second);
//!   - **adjacent-frame completion delta**: for each pair of adjacent
//!     frame numbers `(f, f+1)`, `|completion(f+1) - completion(f)|` —
//!     the design doc's "相邻帧完成时间差", which the interleaved
//!     batch-claim scheduler keeps bounded because adjacent frames land on
//!     different workers. Reported as max / mean / p95 / count.
//!
//! Run from the repo root:
//!
//! ```sh
//! cargo run --release -p oakrender --example bench_process [frames] [workers]
//! ```
//!
//! `frames` defaults to 240, `workers` to the adaptive
//! [`oak_render::procpool::default_worker_count`] policy. The oak-worker
//! binary is located next to the build output (`target/<profile>/oak-worker`),
//! or via `$OAK_WORKER_BIN`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oak_core::Rational;
use oak_render::ipc::SLOT_FORMAT_BGRA8;
use oak_render::procpool::{DispatcherConfig, ProcessDispatcher};
use oak_render::ticket::{TicketPayload, TicketResult, VideoTicketParams};
use oak_render::worker::{Job, JobDispatch, JobSchedule};

/// Locate the oak-worker binary: `$OAK_WORKER_BIN`, else the sibling of
/// the current executable's `examples/` directory, else `oak-worker` on
/// `PATH`.
fn worker_bin() -> PathBuf {
	if let Ok(p) = std::env::var("OAK_WORKER_BIN") {
		return PathBuf::from(p);
	}
	if let Ok(exe) = std::env::current_exe() {
		// target/<profile>/examples/bench_process -> target/<profile>/oak-worker
		if let Some(examples) = exe.parent() {
			if let Some(profile) = examples.parent() {
				let candidate =
					profile.join(format!("oak-worker{}", std::env::consts::EXE_SUFFIX));
				if candidate.exists() {
					return candidate;
				}
			}
		}
	}
	PathBuf::from("oak-worker")
}

fn main() {
	let frames: usize = std::env::args()
		.nth(1)
		.and_then(|s| s.parse().ok())
		.unwrap_or(240);
	let workers: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
	let width: i32 = 1920;
	let height: i32 = 1080;

	let config = DispatcherConfig {
		worker_bin: Some(worker_bin()),
		workers: workers.unwrap_or(0),
		slots_per_worker: 8,
		width,
		height,
		slot_format: SLOT_FORMAT_BGRA8,
		batch_size: 0,
		graph_snapshot: None,
		handshake_timeout_ms: 30_000,
	};
	let dispatcher = ProcessDispatcher::new(config).expect("dispatcher config");
	dispatcher.start().expect("workers start + handshake");
	let worker_count = dispatcher.worker_count();
	println!(
		"oak-worker pool: {worker_count} worker(s), {frames} x {width}x{height} BGRA8 frames"
	);

	// One completion record per frame: (frame number, wall-clock completion).
	let results = Arc::new(Mutex::new(Vec::<(i64, Instant)>::new()));
	let start = Instant::now();
	let dispatcher_for_closure = dispatcher.clone();
	for i in 0..frames {
		let results = results.clone();
		let dc = dispatcher_for_closure.clone();
		let frame = i as i64;
		let job = Job {
			node_identity: 1,
			time: Rational::new(frame, 25),
			params: Arc::new(VideoTicketParams {
				viewer: 1,
				project: String::new(),
				time: Rational::new(frame, 25),
				force_size: Some((width, height)),
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
			// Never invoked on the process backend (workers render from the
			// wire spec); must still be a valid producer.
			produce: Arc::new(|_, _| {
				Err(oak_render::error::Error::Failed(
					"process backend does not use the in-process producer".into(),
				))
			}),
			done: Box::new(move |result: TicketResult| match result {
				Ok(TicketPayload::ShmFrame(f)) => {
					results
						.lock()
						.unwrap_or_else(|e| e.into_inner())
						.push((f.meta.id, Instant::now()));
					// Slot release = credit (cache eviction): without it the
					// worker's free slots never come back and the pool
					// stalls at slots-per-worker frames.
					dc.release_frame(&f);
				}
				Ok(TicketPayload::ShmAudio(a)) => {
					dc.release_audio_frame(&a);
				}
				Ok(other) => {
					eprintln!("unexpected payload: {other:?}");
				}
				Err(e) => {
					eprintln!("frame {frame} failed: {e}");
				}
			}),
			// Playback priority: the pre-render window schedule (seek/playback
			// prioritization is what the scheduler benchmark measures).
			schedule: JobSchedule::playback(frame, 0, 0),
		};
		if !dispatcher.post(job) {
			eprintln!("post refused at frame {frame}");
			break;
		}
	}

	// Pump until every completion has landed.
	let deadline = Instant::now() + Duration::from_secs(120);
	loop {
		dispatcher.poll();
		let done = results.lock().unwrap_or_else(|e| e.into_inner()).len();
		if done >= frames {
			break;
		}
		if Instant::now() > deadline {
			eprintln!("timeout: {done}/{frames} completions");
			break;
		}
		std::thread::sleep(Duration::from_millis(2));
	}
	let elapsed = start.elapsed();

	let mut entries: Vec<(i64, Instant)> = results.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
	entries.sort_by_key(|(id, _)| *id);
	let completed = entries.len();
	let throughput = completed as f64 / elapsed.as_secs_f64();

	// Adjacent-frame completion delta: |t(f+1) - t(f)| over pairs that
	// completed in order. With interleaved claiming these stay small.
	let mut deltas: Vec<f64> = Vec::new();
	for w in entries.windows(2) {
		if w[1].0 == w[0].0 + 1 {
			deltas.push((w[1].1 - w[0].1).as_secs_f64().abs());
		}
	}
	deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());

	let report = |name: &str, value: String| {
		println!("{name:<38} {value}");
	};
	report("frames completed", completed.to_string());
	report("total wall time", format!("{:.2} s", elapsed.as_secs_f64()));
	report("throughput", format!("{throughput:.1} fps ({:.1} ms/frame)", 1000.0 / throughput));
	if !deltas.is_empty() {
		let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
		let p95 = deltas[((deltas.len() as f64 * 0.95) as usize).min(deltas.len() - 1)];
		report("adjacent-frame delta (pairs)", deltas.len().to_string());
		report("  max", format!("{:.3} ms", deltas.last().unwrap() * 1000.0));
		report("  mean", format!("{:.3} ms", mean * 1000.0));
		report("  p95", format!("{:.3} ms", p95 * 1000.0));
	} else {
		report("adjacent-frame delta", "no adjacent pairs completed".to_string());
	}
	report("main-heap frame copies", oak_render::procpool::main_heap_frame_copies().to_string());

	dispatcher.shutdown();
}
