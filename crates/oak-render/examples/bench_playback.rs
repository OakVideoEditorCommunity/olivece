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

//! Real-footage playback benchmark: renders `N` sequential frames of a
//! real media file through the real oak-worker pool at the app's preview
//! proxy size, mimicking the playback pre-render window (Playback
//! priority, interleaved claiming, immediate slot release). Reports
//! throughput and completion latency so worker-side decode/render
//! hotspots can be measured end to end.
//!
//! Run from the repo root:
//!
//! ```sh
//! cargo run --release -p oakrender --example bench_playback -- <media> [frames] [workers] [long_edge]
//! ```
//!
//! `frames` defaults to 240, `workers` to the adaptive policy, and
//! `long_edge` to 480 (the app's preview proxy size). To profile a
//! worker while this runs: `pgrep oak-worker | head -1 | xargs sample 10`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oak_core::Rational;
use oak_render::ipc::SLOT_FORMAT_BGRA8;
use oak_render::procpool::{DispatcherConfig, ProcessDispatcher};
use oak_render::ticket::{TicketPayload, TicketResult, VideoTicketParams};
use oak_render::worker::{Job, JobDispatch, JobSchedule};

/// Locate the oak-worker binary (see bench_process).
fn worker_bin() -> PathBuf {
	if let Ok(p) = std::env::var("OAK_WORKER_BIN") {
		return PathBuf::from(p);
	}
	if let Ok(exe) = std::env::current_exe() {
		if let Some(examples) = exe.parent() {
			if let Some(profile) = examples.parent() {
				let candidate = profile.join(format!("oak-worker{}", std::env::consts::EXE_SUFFIX));
				if candidate.exists() {
					return candidate;
				}
			}
		}
	}
	PathBuf::from("oak-worker")
}

fn main() {
	let media = std::env::args()
		.nth(1)
		.unwrap_or_else(|| "tests/demo.mp4".to_string());
	let frames: usize = std::env::args()
		.nth(2)
		.and_then(|s| s.parse().ok())
		.unwrap_or(240);
	let workers: Option<usize> = std::env::args().nth(3).and_then(|s| s.parse().ok());
	let long_edge: i32 = std::env::args()
		.nth(4)
		.and_then(|s| s.parse().ok())
		.unwrap_or(480);

	// The app's preview proxy size: the sequence's aspect scaled to the
	// long edge (demo.mp4 is 16:9 1080p).
	let (width, height) = ((long_edge as f64 * 16.0 / 9.0).round() as i32, long_edge);

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
	println!("oak-worker pool: {worker_count} worker(s), {frames} x {width}x{height} BGRA8 frames of {media}");

	// One completion record per frame: (ticket/frame, submit, completion).
	let results = Arc::new(Mutex::new(Vec::<(i64, Instant, Instant)>::new()));
	let start = Instant::now();
	for i in 0..frames {
		let results = results.clone();
		let dc = dispatcher.clone();
		let frame = i as i64;
		let media_clone = media.clone();
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
				// A single footage clip covers the whole timeline.
				footage: Some((media_clone, 0)),
				montage: Vec::new(),
				adjustments: Vec::new(),
			}),
			audio: None,
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
						.push((frame, start, Instant::now()));
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
			// Playback priority, the pre-render window's schedule.
			schedule: JobSchedule::playback(frame, frame, 0),
		};
		if !dispatcher.post(job) {
			eprintln!("post refused at frame {frame}");
			break;
		}
	}

	// Pump until every completion has landed.
	let deadline = Instant::now() + Duration::from_secs(300);
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

	let entries: Vec<(i64, Instant, Instant)> =
		results.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
	let completed = entries.len();
	let throughput = completed as f64 / elapsed.as_secs_f64();

	// Per-frame completion latency (submit -> done), an end-to-end proxy
	// for the worker's per-frame render cost under load.
	let mut latencies: Vec<f64> = entries
		.iter()
		.map(|(_, submit, done)| (*done - *submit).as_secs_f64() * 1000.0)
		.collect();
	latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());

	let report = |name: &str, value: String| println!("{name:<38} {value}");
	report("frames completed", completed.to_string());
	report("total wall time", format!("{:.2} s", elapsed.as_secs_f64()));
	report(
		"throughput",
		format!("{throughput:.1} fps ({:.1} ms/frame)", 1000.0 / throughput.max(f64::EPSILON)),
	);
	if !latencies.is_empty() {
		let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;
		report("completion latency mean", format!("{mean:.1} ms"));
		report(
			"completion latency p50/p95/max",
			format!(
				"{:.1} / {:.1} / {:.1} ms",
				latencies[latencies.len() / 2],
				latencies[((latencies.len() as f64 * 0.95) as usize).min(latencies.len() - 1)],
				latencies.last().unwrap()
			),
		);
	}
	report(
		"main-heap frame copies",
		oak_render::procpool::main_heap_frame_copies().to_string(),
	);

	dispatcher.shutdown();
}
