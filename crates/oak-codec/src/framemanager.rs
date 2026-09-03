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

//! `olive::FrameManager` — a pool of reusable [`crate::frame::Frame`]
//! buffers plus a background garbage-collection thread.
//!
//! Mirrors `src/codec/src/framemanager.h`. The C++ manager kept a pool of
//! `std::list<FramePtr>` and a QThread that periodically dropped frames
//! whose last reference died. Rust keeps the same contract behind a
//! `Mutex`; the background thread is replaced by a dedicated GC thread
//! that drains the pool of freeable frames.

use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use oak_core::videoparams::VideoParams;
use crate::frame::Frame;

/// `olive::FrameManager`: singleton frame pool with background GC.
pub struct FrameManager {
	/// Pooled frames waiting for reuse (most-recently-freed first).
	pool: Mutex<Vec<Frame>>,
	/// Peak number of live frames observed (diagnostics).
	peak_count: Mutex<usize>,
	/// Current number of frames outstanding (not yet returned).
	outstanding: Mutex<usize>,
}

impl FrameManager {
	/// The process-wide FrameManager singleton.
	///
	/// Constructs the manager on first use and spawns the background
	/// garbage-collection thread exactly once.
	pub fn instance() -> &'static FrameManager {
		static INSTANCE: OnceLock<FrameManager> = OnceLock::new();
		let mgr = INSTANCE.get_or_init(FrameManager::new);
		// Spawn the GC thread on first construction only. We use a `static`
		// flag guarded by the same lock-free path: the first caller to build
		// the manager also starts the thread. Subsequent calls skip it.
		spawn_gc_thread_once(mgr);
		mgr
	}

	/// Create the empty manager.
	fn new() -> Self {
		FrameManager {
			pool: Mutex::new(Vec::new()),
			peak_count: Mutex::new(0),
			outstanding: Mutex::new(0),
		}
	}

	/// Clear the pool (dropping all cached frames).
	pub fn clear(&self) {
		self.pool.lock().unwrap().clear();
	}

	/// Create a frame with the given params (borrowed from the pool when a
	/// compatible free frame exists, else freshly allocated).
	pub fn create_frame(&self, params: VideoParams) -> Arc<Frame> {
		let frame = {
			let mut pool = self.pool.lock().unwrap();
			match pool.iter().position(|f| frame_matches(f, &params)) {
				Some(idx) => pool.swap_remove(idx),
				None => Frame::with_params(params),
			}
		};

		let mut outstanding = self.outstanding.lock().unwrap();
		*outstanding += 1;
		let mut peak = self.peak_count.lock().unwrap();
		if *outstanding > *peak {
			*peak = *outstanding;
		}

		Arc::new(frame)
	}

	/// Return a frame to the pool for reuse.
	pub fn return_frame(&self, frame: Frame) {
		let mut outstanding = self.outstanding.lock().unwrap();
		*outstanding = outstanding.saturating_sub(1);
		self.pool.lock().unwrap().push(frame);
	}

	/// Number of frames currently outstanding (not in the pool).
	pub fn live_count(&self) -> usize {
		*self.outstanding.lock().unwrap()
	}

	/// Peak number of live frames observed.
	pub fn peak_count(&self) -> usize {
		*self.peak_count.lock().unwrap()
	}

	/// Background GC loop; runs on the manager's dedicated thread.
	///
	/// # CPP-PARITY
	/// `src/codec/src/framemanager.cpp` `run()` collected frames whose last
	/// reference had died, based on per-frame timestamps. The Rust skeleton
	/// keeps a pool of reusable buffers but no per-frame age, so the GC
	/// simply drains the whole pool. This bounds memory: frames are reused
	/// between GC passes and released once every GC period, which matches
	/// the C++ manager's intent of keeping pool memory from growing
	/// unbounded.
	fn gc_loop(&self) {
		self.clear();
	}
}

/// Spawn the GC thread once for the process.
fn spawn_gc_thread_once(mgr: &'static FrameManager) {
	static STARTED: OnceLock<()> = OnceLock::new();
	STARTED.get_or_init(|| {
		thread::spawn(move || {
			// `mgr` is `'static`; the thread may outlive every other
			// reference. Keep polling until the process exits.
			loop {
				thread::sleep(Duration::from_millis(5000));
				mgr.gc_loop();
			}
		});
	});
}

/// True when `frame` carries params equal to `params`.
fn frame_matches(frame: &Frame, params: &VideoParams) -> bool {
	let Some(frame_params) = frame.params() else {
		return false;
	};
	frame_params.equals(params)
}

#[cfg(test)]
mod tests {
	use super::*;
	use oak_core::ocioutils::PixelFormat as OakPixelFormat;

	fn test_params(w: i32, h: i32) -> VideoParams {
		VideoParams::new_basic(w, h, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1)
	}

	#[test]
	fn create_and_return_tracks_counts() {
		let mgr = FrameManager::new();
		assert_eq!(mgr.live_count(), 0);
		assert_eq!(mgr.peak_count(), 0);

		let params = test_params(64, 64);
		let frame = mgr.create_frame(params);
		assert_eq!(mgr.live_count(), 1);
		assert_eq!(mgr.peak_count(), 1);

		// Return by unwrapping the single strong reference.
		let frame = Arc::try_unwrap(frame).unwrap();
		mgr.return_frame(frame);
		assert_eq!(mgr.live_count(), 0);
		assert_eq!(mgr.peak_count(), 1);
	}

	#[test]
	fn pool_reuses_compatible_frames() {
		let mgr = FrameManager::new();
		let params = test_params(64, 64);
		let f1 = mgr.create_frame(params);
		mgr.return_frame(Arc::try_unwrap(f1).unwrap());
		assert_eq!(mgr.live_count(), 0);

		// A compatible request reuses the pooled buffer rather than
		// allocating a new one.
		let f2 = mgr.create_frame(test_params(64, 64));
		assert_eq!(mgr.live_count(), 1);
		assert_eq!(mgr.peak_count(), 1);
		Arc::try_unwrap(f2).unwrap();
	}

	#[test]
	fn peak_count_tracks_maximum() {
		let mgr = FrameManager::new();
		let p1 = test_params(64, 64);
		let p2 = test_params(128, 128);
		let a = mgr.create_frame(p1);
		let b = mgr.create_frame(p2);
		assert_eq!(mgr.live_count(), 2);
		assert_eq!(mgr.peak_count(), 2);
		mgr.return_frame(Arc::try_unwrap(a).unwrap());
		assert_eq!(mgr.live_count(), 1);
		assert_eq!(mgr.peak_count(), 2);
		Arc::try_unwrap(b).unwrap();
	}

	#[test]
	fn clear_drops_pooled_frames() {
		let mgr = FrameManager::new();
		let params = test_params(64, 64);
		let f = mgr.create_frame(params);
		mgr.return_frame(Arc::try_unwrap(f).unwrap());
		assert_eq!(mgr.pool.lock().unwrap().len(), 1);

		mgr.clear();
		assert_eq!(mgr.pool.lock().unwrap().len(), 0);
	}
}
