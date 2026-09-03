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

//! Ticket / job-dispatch contract tests (M15 S2: the in-process thread
//! pool is gone; the tests drive the thread-free inline dispatcher, which
//! makes the arena's exactly-once / cancel / shutdown semantics
//! deterministic without any worker threads).

mod common;

use std::sync::{mpsc, Arc};
use std::time::Duration;

use oak_core::{Rational, TimeRange};

use oak_render::error::Error;
use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};
use oak_render::ticket::{TicketArena, TicketId, VideoTicketParams};
use oak_render::worker::{GraphSnapshotStore, InlineDispatcher, JobDispatch};

/// Unwrap a video ticket payload for assertions.
fn res_video(res: &oak_render::ticket::TicketPayload) -> &oak_core::texture::Texture {
	match res {
		oak_render::ticket::TicketPayload::Video(t) => t,
		_ => panic!("expected a video payload"),
	}
}

fn small_frame() -> Frame {
	let mut f = Frame::new();
	let mut p = VideoParamsPod::default();
	p.width = 8;
	p.height = 4;
	f.set_video_params(p);
	f.allocate();
	f
}

fn ok_producer() -> oak_render::ticket::Producer {
	Arc::new(|_, _| Ok(oak_render::ticket::TicketPayload::Video(Texture::wrap_frame(small_frame()))))
}

fn params(time: Rational) -> VideoTicketParams {
	VideoTicketParams {
		viewer: 1,
		project: String::new(),
		time,
		force_size: Some((8, 4)),
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: Vec::new(),
	}
}

/// An arena on a queued inline dispatcher: jobs run only when the test
/// drains it with `InlineDispatcher::run` / `shutdown`.
fn test_arena(producer: oak_render::ticket::Producer) -> (TicketArena, Arc<InlineDispatcher>) {
	let d = InlineDispatcher::queued();
	let arena = TicketArena::new(d.clone(), producer);
	(arena, d)
}

/// Completion fires exactly once on success; the payload texture has
/// the requested size/format.
#[test]
fn ticket_completion_once_success() {
	let (arena, d) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let id = arena.submit_video(
		params(Rational::new(0, 1)),
		Box::new(move |r| {
			let _ = tx.send(r.is_ok());
		}),
	);
	d.run();
	arena.wait(id).unwrap();
	assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap());
	assert!(
		rx.recv_timeout(Duration::from_millis(50)).is_err(),
		"exactly once"
	);

	// is_finished 必须在 result() 之前查：result() 是终止性读取
	// （连同条目一起回收，arena 表不能无限增长）。
	assert!(arena.is_finished(id));
	let res = arena.result(id).unwrap().unwrap();
	assert_eq!(res_video(&res).size(), (8, 4));
	assert_eq!(res_video(&res).format(), oak_core::PixelFormat::F32);
	d.shutdown();
}

/// Completion fires exactly once on cancel (Error::State), even when
/// cancel races the running job. On the queued inline dispatcher the
/// "running" state is the queued-but-not-yet-drained job: a cancel before
/// `run` delivers State deterministically.
#[test]
fn ticket_completion_once_on_cancel() {
	let (arena, d) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let id = arena.submit_video(
		params(Rational::new(1, 1)),
		Box::new(move |r| {
			let _ = tx.send(r);
		}),
	);
	arena.cancel(id);
	d.run();
	arena.wait(id).unwrap();
	let res = rx.recv_timeout(Duration::from_secs(5)).unwrap();
	assert_eq!(res.unwrap_err().code(), Error::State.code());
	assert!(
		rx.recv_timeout(Duration::from_millis(50)).is_err(),
		"exactly once"
	);
	d.shutdown();
}

/// cancel_all during shutdown delivers cancellation to every pending
/// ticket; no completion fires after shutdown returns.
#[test]
fn shutdown_drains_completions() {
	let (arena, d) = test_arena(ok_producer());

	let (tx, rx) = mpsc::channel();
	let mut ids = Vec::new();
	for i in 0..8 {
		let tx = tx.clone();
		let id = arena.submit_video(
			params(Rational::new(i, 1)),
			Box::new(move |r| {
				let _ = tx.send(r.is_err());
			}),
		);
		ids.push(id);
	}
	drop(tx);
	arena.cancel_all();
	d.shutdown();

	let mut completions = Vec::new();
	while let Ok(err) = rx.recv_timeout(Duration::from_secs(5)) {
		completions.push(err);
	}
	assert_eq!(completions.len(), 8, "every ticket completed");
	assert!(
		completions.iter().all(|&e| e),
		"all cancelled with Error::State"
	);
	// Everything is finished after shutdown.
	assert!(ids.iter().all(|id| arena.is_finished(*id)));
}

/// Saturation: 64 jobs all complete on the queued dispatcher after one
/// drain; no job runs twice (arena ids unique).
#[test]
fn pool_saturation() {
	let (arena, d) = test_arena(ok_producer());
	let (tx, rx) = mpsc::channel();
	let mut ids = Vec::new();
	for i in 0..64u64 {
		let tx = tx.clone();
		let id = arena.submit_video(
			params(Rational::new(i as i64, 1)),
			Box::new(move |r| {
				let _ = tx.send(r.is_ok());
			}),
		);
		ids.push(id);
	}
	drop(tx);
	d.run();
	let mut ok = 0;
	while let Ok(true) = rx.recv_timeout(Duration::from_secs(10)) {
		ok += 1;
	}
	assert_eq!(ok, 64, "all 64 jobs completed successfully");
	ids.sort_by_key(|i| i.0);
	let unique: std::collections::HashSet<_> = ids.iter().collect();
	assert_eq!(unique.len(), 64, "unique arena ids");
	d.shutdown();
}

/// Ticket arena ids are monotonic and never reused within a manager
/// lifetime.
#[test]
fn ticket_id_monotonic() {
	let (arena, d) = test_arena(ok_producer());
	let a = arena.submit_video(params(Rational::new(0, 1)), Box::new(|_| {}));
	let b = arena.submit_video(params(Rational::new(1, 1)), Box::new(|_| {}));
	let c = arena.submit_video(params(Rational::new(2, 1)), Box::new(|_| {}));
	assert!(a.0 < b.0 && b.0 < c.0);
	d.shutdown();
}

/// GraphSnapshotStore: acquire twice shares one file; release never
/// unlinks at zero refs (M16 S1 — a worker may still hold the path for
/// a late load_graph); the whole store is removed by cleanup() at
/// manager shutdown.
#[test]
fn snapshot_store_refcount() {
	let store = GraphSnapshotStore::new();
	let project = oak_node::project::Project::new();
	let p1 = store.acquire(&project, 1).unwrap();
	let p2 = store.acquire(&project, 1).unwrap();
	assert_eq!(p1, p2);
	assert!(std::path::Path::new(&p1).exists());
	assert_eq!(store.refs(&p1), 2);
	store.release(&p1);
	assert!(std::path::Path::new(&p1).exists());
	store.mark_cached(&p1, true);
	assert!(store.is_cached(&p1));
	store.release(&p1);
	// Zero refs: file intentionally kept (late worker loads), entry dropped.
	assert_eq!(store.refs(&p1), 0);
	assert!(
		std::path::Path::new(&p1).exists(),
		"file kept after release at refcount 0"
	);
	store.cleanup();
	assert!(
		!std::path::Path::new(&p1).exists(),
		"store directory removed by cleanup"
	);
}

/// TimeRange sanity (used above).
#[test]
fn range_sanity() {
	let r = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
	assert_eq!(r.length(), Rational::new(10, 1));
}
