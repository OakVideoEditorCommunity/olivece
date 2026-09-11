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

//! The preview scheduler (M15 S1; design doc §3.2): frame-request
//! ordering, interleaved batch claims, crash re-dispatch and flow
//! control, as pure single-threaded logic.
//!
//! The scheduler knows nothing about processes or shared memory — it
//! turns a stream of [`FrameRequest`]s into [`ClaimedBatch`]es for the
//! [`crate::procpool::ProcessDispatcher`] to hand to workers:
//!
//!   - **Interleaved batch claims.** With `W` workers, the pending frame
//!     stream is sharded round-robin: worker `i` claims frames whose
//!     frame number is `≡ i (mod W)`, in batches of `B ≈ 120 / W`
//!     (configurable). Adjacent frame numbers therefore land on
//!     different workers and finish at nearly the same time; every
//!     frame belongs to exactly one worker (no work stealing).
//!   - **Priorities.** Seek/current frame > playback window (nearer the
//!     playhead first) > background (export/thumbnails). Within one
//!     priority class batches keep ascending frame order.
//!   - **Crash recovery.** A crashed worker's claimed frames (its whole
//!     un-started batches plus the un-finished frames of started ones)
//!     are re-queued and may be claimed by ANY healthy worker — crash
//!     re-dispatch is failure recovery, not stealing.
//!   - **Flow control.** [`PreviewScheduler::claim_batch`] never claims
//!     more frames than the caller's `credit` (the worker's free shm
//!     slot count): slots are the credit. Playback/Background claims
//!     leave one credit unused — the per-worker interactive reserve, so
//!     a window batch can never drain the last slot and starve a
//!     UI-blocking seek (window slots are released by UI-thread
//!     consumption; a seek waiting on them deadlocks the UI).
//!   - **Cancellation.** Frame keys carry a parameter `version`;
//!     submitting a newer version of a key invalidates the older one,
//!     and [`PreviewScheduler::cancel_sequence`] drops a whole sequence.
//!
//! All methods are non-blocking; the dispatcher drives them from its
//! poll loop (UI tick).

use std::collections::HashMap;

/// A frame request key: `(sequence, frame number, parameter version)`.
/// The version covers graph/proxy/resolution/color-parameter changes —
/// bumping it invalidates outstanding requests for the same frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
	/// Sequence identity.
	pub sequence: u64,
	/// Frame number within the sequence.
	pub frame: i64,
	/// Parameter version (graph version / proxy tier / resolution tier /
	/// color).
	pub version: u64,
}

/// Frame request priority class (lower value = more urgent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum FramePriority {
	/// Seek / current playhead frame (single-frame insert, top priority).
	#[default]
	Seek,
	/// Playback window frames (ordered by [`FrameRequest::distance`]).
	Playback,
	/// Background work (export, thumbnails, full-res stills).
	Background,
}

/// One pending frame request. `payload` is opaque to the scheduler (the
/// dispatcher uses it to find the ticket's params + completion).
#[derive(Clone, Debug)]
pub struct FrameRequest<P> {
	/// The request key.
	pub key: FrameKey,
	/// Priority class.
	pub priority: FramePriority,
	/// Distance from the playhead in frames (orders the Playback class;
	/// unused by Seek/Background).
	pub distance: i64,
	/// Caller payload.
	pub payload: P,
	/// Slot bytes this request needs (frame size x wire format; M15 S3).
	/// `claim_batch` skips requests whose bytes exceed the worker's current
	/// slot capacity, so the dispatcher can grow the segment first (grow-
	/// on-demand geometry, design §3.1).
	pub slot_bytes: usize,
}

/// A batch of frames one worker claimed.
#[derive(Clone, Debug)]
pub struct ClaimedBatch<P> {
	/// Batch identity (unique per scheduler).
	pub batch_id: u64,
	/// The claiming worker index.
	pub worker: usize,
	/// The claimed frames in render order (priority class first, then
	/// ascending frame number).
	pub frames: Vec<FrameRequest<P>>,
}

struct Claim<P> {
	worker: usize,
	request: FrameRequest<P>,
}

struct PendingEntry<P> {
	request: FrameRequest<P>,
	/// True when any worker may claim this frame (crash re-dispatch);
	/// false when the interleaved shard rule applies.
	any_worker: bool,
}

/// The outcome of [`PreviewScheduler::submit`].
#[derive(Debug)]
pub enum SubmitOutcome<P> {
	/// A new request was accepted (nothing was pending under its key).
	Accepted,
	/// An existing pending request under the same key was replaced; the
	/// superseded request is returned so the dispatcher can cancel its
	/// ticket completion (M15 S2).
	Replaced(FrameRequest<P>),
	/// The key is already claimed (in flight); the request was rejected.
	InFlight,
}

/// The scheduler state machine (single-threaded by contract).
pub struct PreviewScheduler<P> {
	workers: usize,
	batch_size: usize,
	pending: Vec<PendingEntry<P>>,
	claimed: HashMap<FrameKey, Claim<P>>,
	next_batch_id: u64,
	/// Total frames re-queued by worker crashes (tests/metrics).
	crash_requeued: u64,
}

impl<P: Clone> PreviewScheduler<P> {
	/// Scheduler for `workers` workers (at least 1). `batch_size` 0 picks
	/// the design default `max(1, 120 / workers)`.
	pub fn new(workers: usize, batch_size: usize) -> Self {
		let workers = workers.max(1);
		let batch_size = if batch_size == 0 {
			(120 / workers).max(1)
		} else {
			batch_size
		};
		Self {
			workers,
			batch_size,
			pending: Vec::new(),
			claimed: HashMap::new(),
			next_batch_id: 1,
			crash_requeued: 0,
		}
	}

	/// The configured worker count.
	pub fn workers(&self) -> usize {
		self.workers
	}

	/// Change the worker count at run time (dynamic pool resize driven by
	/// the GPU-vram budget on resolution switches: 1080p can run the full
	/// CPU-bound pool, 4K only a vram-bounded fraction). The sharding
	/// modulus follows the new count immediately — pending frames are
	/// re-sharded on their next claim, so a shrink only reshuffles the
	/// not-yet-claimed queue (in-flight frames stay on their worker).
	/// `batch_size` is recomputed for the new count (the design figure
	/// `120 / W`).
	pub fn set_worker_count(&mut self, workers: usize) {
		let workers = workers.max(1);
		let old = std::mem::replace(&mut self.workers, workers);
		if old != workers {
			self.batch_size = (120 / workers).max(1).min(self.batch_size.max(1));
		}
	}

	/// The configured batch size.
	pub fn batch_size(&self) -> usize {
		self.batch_size
	}

	/// Submit a frame request. An already-pending request with the same
	/// key is replaced; a key already claimed (in flight) is rejected —
	/// the dispatcher must cancel/re-version it first.
	///
	/// Seek-priority requests (interactive frame / real-time audio) are
	/// claimable by ANY worker, not just their interleaved shard: the
	/// no-stealing shard rule exists to keep ADJACENT PLAYBACK frames
	/// finishing together, but pinning a single urgent frame to one worker
	/// starves it whenever that worker's slots are all held by the window
	/// (the UI thread then waits on the seek while the window's slot
	/// releases run on that same thread — the seek-starvation deadlock).
	pub fn submit(&mut self, request: FrameRequest<P>) -> SubmitOutcome<P> {
		if self.claimed.contains_key(&request.key) {
			return SubmitOutcome::InFlight;
		}
		let any_worker = request.priority == FramePriority::Seek;
		if let Some(entry) = self.pending.iter_mut().find(|e| e.request.key == request.key) {
			entry.any_worker = any_worker;
			let old = std::mem::replace(&mut entry.request, request);
			return SubmitOutcome::Replaced(old);
		}
		self.pending.push(PendingEntry {
			request,
			any_worker,
		});
		SubmitOutcome::Accepted
	}

	/// Whether `key` is currently claimed (in flight).
	pub fn is_claimed(&self, key: &FrameKey) -> bool {
		self.claimed.contains_key(key)
	}

	/// Claim the next batch for `worker`: the worker's interleaved shard
	/// (frame number `≡ worker (mod W)`, plus crash-requeued frames and
	/// Seek-priority requests — both claimable by any worker), ordered by
	/// priority class / playhead distance / ascending frame, capped at
	/// `min(batch_size, credit)`. Playback/Background claims additionally
	/// leave one credit unused (the per-worker interactive reserve; see
	/// [`PreviewScheduler::submit`]). Requests needing more than
	/// `max_bytes` of slot space are skipped (they stay pending until the
	/// dispatcher grows the segment). Returns `None` when nothing
	/// claimable (`credit == 0`, unknown worker, empty shard, all
	/// oversized, or only the reserve remains).
	///
	/// Claimed Playback frames never go to another worker while in flight
	/// (no stealing).
	pub fn claim_batch(
		&mut self,
		worker: usize,
		credit: usize,
		max_bytes: usize,
	) -> Option<ClaimedBatch<P>> {
		if worker >= self.workers || credit == 0 {
			return None;
		}
		let workers = self.workers;
		let mut indexes: Vec<usize> = self
			.pending
			.iter()
			.enumerate()
			.filter(|(_, e)| {
				e.request.slot_bytes <= max_bytes
					&& (e.any_worker || e.request.key.frame.rem_euclid(workers as i64) == worker as i64)
			})
			.map(|(i, _)| i)
			.collect();
		if indexes.is_empty() {
			return None;
		}
		indexes.sort_by(|&a, &b| {
			let ra = &self.pending[a].request;
			let rb = &self.pending[b].request;
			ra.priority
				.cmp(&rb.priority)
				.then(ra.distance.cmp(&rb.distance))
				.then(ra.key.frame.cmp(&rb.key.frame))
				.then(ra.key.sequence.cmp(&rb.key.sequence))
		});
		// Per-worker interactive reserve: Playback/Background claims must
		// leave one slot free. Window frames are released by UI-thread
		// consumption/eviction, so a batch that drains the worker's last
		// slot can starve a UI-blocking seek FOREVER; seeks and audio
		// (Seek priority) complete on the worker without UI involvement,
		// so they may use the last slot. (The global
		// `preview_window_capacity` reserve alone did not prevent this:
		// its accounting is pool-wide, while slot exhaustion happens per
		// worker.)
		let cap = self.batch_size.min(credit);
		let mut taken: Vec<usize> = Vec::with_capacity(cap);
		for &i in &indexes {
			if taken.len() >= cap {
				break;
			}
			let seek = self.pending[i].request.priority == FramePriority::Seek;
			if !seek && taken.len() + 1 >= credit {
				continue; // keep the reserve slot free
			}
			taken.push(i);
		}
		if taken.is_empty() {
			return None;
		}
		let indexes = taken;

		let batch_id = self.next_batch_id;
		self.next_batch_id += 1;

		// Collect claimed frames (removal order does not matter; the batch
		// keeps the sorted order).
		let mut frames: Vec<FrameRequest<P>> = Vec::with_capacity(indexes.len());
		let mut marked: Vec<bool> = vec![false; self.pending.len()];
		for &i in &indexes {
			marked[i] = true;
		}
		let mut kept: Vec<PendingEntry<P>> = Vec::with_capacity(self.pending.len() - indexes.len());
		for (i, entry) in self.pending.drain(..).enumerate() {
			if marked[i] {
				self.claimed.insert(
					entry.request.key,
					Claim {
						worker,
						request: entry.request.clone(),
					},
				);
				frames.push(entry.request);
			} else {
				kept.push(entry);
			}
		}
		self.pending = kept;
		frames.sort_by(|a, b| {
			a.priority
				.cmp(&b.priority)
				.then(a.distance.cmp(&b.distance))
				.then(a.key.frame.cmp(&b.key.frame))
		});
		Some(ClaimedBatch {
			batch_id,
			worker,
			frames,
		})
	}

	/// Report a claimed frame as rendered (frame_ready). Returns the
	/// request when the key was in flight.
	pub fn frame_done(&mut self, key: &FrameKey) -> Option<FrameRequest<P>> {
		self.claimed.remove(key).map(|c| c.request)
	}

	/// Pending-request count (dispatcher diagnostics).
	pub fn pending_len(&self) -> usize {
		self.pending.len()
	}

	/// One-line summary of the pending queue for starvation debugging
	/// (frame, pinned shard, needed slot bytes, any_worker).
	pub fn pending_summary(&self) -> Vec<(i64, i64, usize, bool)> {
		self.pending
			.iter()
			.map(|e| {
				(
					e.request.key.frame,
					e.request.key.frame.rem_euclid(self.workers as i64),
					e.request.slot_bytes,
					e.any_worker,
				)
			})
			.collect()
	}

	/// Report a claimed frame as permanently failed (frame_failed; the
	/// main process paints the fallback). The claim is dropped WITHOUT
	/// re-dispatch — a render error is not a crash. Returns the request
	/// when the key was in flight.
	pub fn frame_failed(&mut self, key: &FrameKey) -> Option<FrameRequest<P>> {
		self.claimed.remove(key).map(|c| c.request)
	}

	/// Re-queue every frame claimed by `worker` (crash recovery): its
	/// un-started batches and the un-finished frames of started batches
	/// all come back as pending, claimable by ANY healthy worker.
	/// Returns the re-queued requests.
	pub fn worker_crashed(&mut self, worker: usize) -> Vec<FrameRequest<P>> {
		let mut reclaimed = Vec::new();
		self.claimed.retain(|_, claim| {
			if claim.worker == worker {
				reclaimed.push(claim.request.clone());
				false
			} else {
				true
			}
		});
		for request in reclaimed.iter().cloned() {
			self.pending.push(PendingEntry {
				request,
				any_worker: true,
			});
		}
		self.crash_requeued += reclaimed.len() as u64;
		reclaimed
	}

	/// Cancel one key, pending OR claimed (single-frame cancellation;
	/// the dispatcher delivers the ticket's `Error::State` itself).
	/// Returns true when the key was known.
	pub fn cancel_key(&mut self, key: &FrameKey) -> bool {
		let before = self.pending.len();
		self.pending.retain(|e| &e.request.key != key);
		if self.pending.len() != before {
			return true;
		}
		self.claimed.remove(key).is_some()
	}

	/// Cancel every pending AND claimed frame of `sequence` (frame-key
	/// invalidation; the `cancel` wire message covers the worker side).
	/// Returns the dropped requests so the dispatcher can fire their
	/// completions with `Error::State`.
	pub fn cancel_sequence(&mut self, sequence: u64) -> Vec<FrameRequest<P>> {
		let mut dropped = Vec::new();
		self.pending.retain(|e| {
			if e.request.key.sequence == sequence {
				dropped.push(e.request.clone());
				false
			} else {
				true
			}
		});
		self.claimed.retain(|_, c| {
			if c.request.key.sequence == sequence {
				dropped.push(c.request.clone());
				false
			} else {
				true
			}
		});
		dropped
	}

	/// Pending (unclaimed) request count.
	pub fn pending_count(&self) -> usize {
		self.pending.len()
	}

	/// The largest `slot_bytes` among pending requests claimable by
	/// `worker` (its shard plus crash-requeued frames) that exceeds
	/// `current`, if any. The dispatcher uses this to grow a worker's
	/// segment before the next claim (M15 S3 grow-on-demand geometry).
	pub fn max_pending_bytes_for_worker(&self, worker: usize, current: usize) -> Option<usize> {
		if worker >= self.workers {
			return None;
		}
		let workers = self.workers;
		self.pending
			.iter()
			.filter(|e| {
				e.any_worker || e.request.key.frame.rem_euclid(workers as i64) == worker as i64
			})
			.map(|e| e.request.slot_bytes)
			.max()
			.filter(|&m| m > current)
	}

	/// Claimed (in-flight) request count.
	pub fn claimed_count(&self) -> usize {
		self.claimed.len()
	}

	/// The worker currently holding `key` (None when not claimed).
	pub fn claimed_worker(&self, key: &FrameKey) -> Option<usize> {
		self.claimed.get(key).map(|c| c.worker)
	}

	/// Total frames re-queued by crashes so far (tests/metrics).
	pub fn crash_requeued(&self) -> u64 {
		self.crash_requeued
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn req(seq: u64, frame: i64, prio: FramePriority) -> FrameRequest<u64> {
		FrameRequest {
			key: FrameKey {
				sequence: seq,
				frame,
				version: 0,
			},
			priority: prio,
			distance: frame,
			payload: frame as u64,
			slot_bytes: 0,
		}
	}

	/// Claim until nothing is claimable by any worker; returns
	/// (worker, frame) pairs in claim order.
	fn claim_all(s: &mut PreviewScheduler<u64>) -> Vec<(usize, i64)> {
		let mut out = Vec::new();
		loop {
			let mut progress = false;
			for w in 0..s.workers() {
				while let Some(batch) = s.claim_batch(w, 1024, usize::MAX) {
					for f in &batch.frames {
						out.push((w, f.key.frame));
					}
					progress = true;
				}
			}
			if !progress {
				break;
			}
		}
		out
	}

	#[test]
	fn no_stealing_every_frame_claimed_exactly_once() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(3, 4);
		for f in 0..40 {
			assert!(matches!(s.submit(req(1, f, FramePriority::Playback)), SubmitOutcome::Accepted));
		}
		let claims = claim_all(&mut s);
		assert_eq!(claims.len(), 40, "every frame claimed");
		let mut frames: Vec<i64> = claims.iter().map(|(_, f)| *f).collect();
		frames.sort_unstable();
		frames.dedup();
		assert_eq!(frames.len(), 40, "no frame claimed twice (no stealing)");
		assert_eq!(s.pending_count(), 0);
		assert_eq!(s.claimed_count(), 40, "all claimed, none completed yet");
	}

	#[test]
	fn interleave_adjacent_frames_on_different_workers() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(4, 2);
		for f in 0..16 {
			s.submit(req(1, f, FramePriority::Playback));
		}
		let claims = claim_all(&mut s);
		for (worker, frame) in &claims {
			assert_eq!(
				frame.rem_euclid(4),
				*worker as i64,
				"frame {frame} must be claimed by worker {}",
				frame.rem_euclid(4)
			);
		}
		// Adjacent frames are on different workers.
		let worker_of: HashMap<i64, usize> = claims
			.iter()
			.map(|(w, f)| (*f, *w))
			.collect();
		for f in 0..15 {
			assert_ne!(worker_of[&f], worker_of[&(f + 1)]);
		}
	}

	#[test]
	fn crash_requeues_to_any_healthy_worker() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(2, 8);
		for f in 0..8 {
			s.submit(req(1, f, FramePriority::Playback));
		}
		// Both workers claim their shards first.
		let batch0 = s.claim_batch(0, 8, 1024).unwrap();
		assert_eq!(batch0.frames.len(), 4); // frames 0,2,4,6
		let batch1 = s.claim_batch(1, 8, 1024).unwrap();
		assert_eq!(batch1.frames.len(), 4); // frames 1,3,5,7

		// Worker 0 crashes: its frames come back...
		let reclaimed = s.worker_crashed(0);
		assert_eq!(reclaimed.len(), 4);
		assert_eq!(s.claimed_count(), 4, "worker 1 keeps its own batch");
		assert_eq!(s.pending_count(), 4);
		assert_eq!(s.crash_requeued(), 4);

		// ...and worker 1 (NOT their shard) can claim them all.
		let batch = s.claim_batch(1, 8, 1024).unwrap();
		assert_eq!(batch.frames.len(), 4);
		let mut frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
		frames.sort_unstable();
		assert_eq!(frames, vec![0, 2, 4, 6]);
	}

	#[test]
	fn priority_seek_beats_playback_beats_background() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
		// All frames on worker 0's shard (W=1): background first, then a
		// playback window, then a seek frame submitted last.
		for f in 0..5 {
			s.submit(req(1, f, FramePriority::Background));
		}
		for f in 10..15 {
			let mut r = req(1, f, FramePriority::Playback);
			r.distance = (f - 12).abs();
			s.submit(r);
		}
		s.submit(req(1, 100, FramePriority::Seek));

		let batch = s.claim_batch(0, 100, 1024).unwrap();
		let order: Vec<(FramePriority, i64)> = batch
			.frames
			.iter()
			.map(|f| (f.priority, f.key.frame))
			.collect();
		// Seek first...
		assert_eq!(order[0], (FramePriority::Seek, 100));
		// ...then playback by playhead distance (12 nearest first)...
		assert_eq!(
			order[1..6]
				.iter()
				.map(|(_, f)| *f)
				.collect::<Vec<_>>(),
			vec![12, 11, 13, 10, 14]
		);
		// ...then background ascending.
		assert_eq!(
			order[6..]
				.iter()
				.map(|(_, f)| *f)
				.collect::<Vec<_>>(),
			vec![0, 1, 2, 3, 4]
		);
	}

	#[test]
	fn flow_control_credit_limits_batch() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
		for f in 0..10 {
			s.submit(req(1, f, FramePriority::Playback));
		}
		// Zero credit claims nothing.
		assert!(s.claim_batch(0, 0, 1024).is_none());
		// Credit 3 claims 2 playback frames: the last slot stays free as
		// the per-worker interactive reserve (window frames are released
		// by UI-thread consumption, so a full drain can starve a
		// UI-blocking seek).
		let batch = s.claim_batch(0, 3, 1024).unwrap();
		assert_eq!(batch.frames.len(), 2);
		assert_eq!(s.pending_count(), 8);
	}

	#[test]
	fn playback_claims_keep_the_reserve_but_seeks_may_use_it() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
		for f in 0..4 {
			s.submit(req(1, f, FramePriority::Playback));
		}
		// One free slot: playback claims nothing (reserve kept)...
		assert!(s.claim_batch(0, 1, 1024).is_none());
		assert_eq!(s.pending_count(), 4);
		// ...but a Seek (interactive frame / real-time audio) may use it:
		// seeks complete on the worker without UI-thread involvement, so
		// taking the last slot cannot deadlock the UI.
		s.submit(req(1, 100, FramePriority::Seek));
		let batch = s.claim_batch(0, 1, 1024).unwrap();
		assert_eq!(batch.frames.len(), 1);
		assert_eq!(batch.frames[0].priority, FramePriority::Seek);
	}

	#[test]
	fn seek_is_claimable_by_any_worker() {
		// The no-stealing shard rule applies to Playback frames (adjacent
		// frames finish together); a single urgent seek pinned to a full
		// worker would starve even while other workers idle.
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(4, 100);
		s.submit(req(1, 3, FramePriority::Seek)); // shard 3
		let batch = s.claim_batch(0, 4, 1024).unwrap();
		assert_eq!(batch.frames.len(), 1);
		assert_eq!(batch.frames[0].key.frame, 3);
		// Playback frames stay pinned to their shard.
		s.submit(req(1, 7, FramePriority::Playback)); // shard 3
		assert!(s.claim_batch(0, 4, 1024).is_none());
		assert!(s.claim_batch(3, 4, 1024).is_some());
	}

	#[test]
	fn batch_size_caps_the_claim() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
		for f in 0..10 {
			s.submit(req(1, f, FramePriority::Playback));
		}
		let batch = s.claim_batch(0, 100, 1024).unwrap();
		assert_eq!(batch.frames.len(), 4, "batch size B caps the claim");
		// Ascending frame order inside the batch.
		let frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
		assert_eq!(frames, vec![0, 1, 2, 3]);
	}

	#[test]
	fn claim_batch_skips_requests_exceeding_slot_capacity() {
		// M15 S3 grow-on-demand: a request needing more bytes than the
		// worker's current slot capacity stays pending (the dispatcher can
		// grow the segment and claim it later); smaller requests still flow.
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 100);
		for f in 0..4 {
			let mut r = req(1, f, FramePriority::Playback);
			r.slot_bytes = if f == 1 { 33_000_000 } else { 8_300_000 };
			s.submit(r);
		}
		// max_bytes 8_300_000: frame 1 (33 MB) is skipped.
		let batch = s.claim_batch(0, 100, 8_300_000).unwrap();
		let frames: Vec<i64> = batch.frames.iter().map(|f| f.key.frame).collect();
		assert_eq!(frames, vec![0, 2, 3]);
		assert_eq!(s.pending_count(), 1, "the oversized frame stays pending");
		assert_eq!(
			s.max_pending_bytes_for_worker(0, 8_300_000),
			Some(33_000_000),
			"the dispatcher sees the growth need"
		);
		// After the segment grows, the oversized frame is claimable.
		let batch = s.claim_batch(0, 100, 33_000_000).unwrap();
		assert_eq!(batch.frames.len(), 1);
		assert_eq!(batch.frames[0].key.frame, 1);
		assert_eq!(s.pending_count(), 0);
	}

	#[test]
	fn done_and_failed_drop_the_claim() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
		s.submit(req(1, 0, FramePriority::Playback));
		s.submit(req(1, 1, FramePriority::Playback));
		let batch = s.claim_batch(0, 4, 1024).unwrap();
		assert_eq!(batch.frames.len(), 2);
		let k0 = batch.frames[0].key;
		let k1 = batch.frames[1].key;
		assert!(s.frame_done(&k0).is_some());
		assert!(s.frame_failed(&k1).is_some());
		assert_eq!(s.claimed_count(), 0);
		assert_eq!(s.pending_count(), 0, "frame_failed is terminal (purple frame fallback)");
		// Unknown keys are no-ops.
		assert!(s.frame_done(&k0).is_none());
	}

	#[test]
	fn resubmit_of_claimed_key_is_rejected_until_done() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
		let r = req(1, 5, FramePriority::Playback);
		let key = r.key;
		assert!(matches!(s.submit(r), SubmitOutcome::Accepted));
		let _ = s.claim_batch(0, 4, 1024).unwrap();
		// In flight: rejected.
		assert!(matches!(
			s.submit(req(1, 5, FramePriority::Seek)),
			SubmitOutcome::InFlight
		));
		s.frame_done(&key);
		// After completion the same key may be requested again (new
		// version in practice).
		assert!(matches!(
			s.submit(req(1, 5, FramePriority::Seek)),
			SubmitOutcome::Accepted
		));
	}

	#[test]
	fn resubmit_replaces_pending_and_returns_the_old_request() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
		let first = req(1, 5, FramePriority::Playback);
		let key = first.key;
		assert!(matches!(s.submit(first), SubmitOutcome::Accepted));
		// A second submission under the same key replaces the pending entry
		// and hands the superseded request back (the dispatcher cancels its
		// ticket completion with Error::State).
		let second = req(1, 5, FramePriority::Seek);
		let second_key = second.key;
		match s.submit(second) {
			SubmitOutcome::Replaced(old) => {
				assert_eq!(old.key, key);
				assert_eq!(old.priority, FramePriority::Playback);
			}
			other => panic!("expected Replaced, got {other:?}"),
		}
		let _ = s.claim_batch(0, 4, 1024).unwrap();
		assert_eq!(s.claimed_worker(&second_key), Some(0));
	}

	#[test]
	fn cancel_sequence_drops_pending_and_claimed() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(1, 4);
		for f in 0..6 {
			s.submit(req(7, f, FramePriority::Playback));
		}
		let _ = s.claim_batch(0, 4, 1024).unwrap(); // claims 3 of sequence 7 (reserve)
		s.submit(req(8, 0, FramePriority::Playback)); // other sequence
		let dropped = s.cancel_sequence(7);
		assert_eq!(dropped.len(), 6, "all 6 sequence-7 requests dropped");
		assert_eq!(s.pending_count(), 1, "sequence 8 untouched");
		assert_eq!(s.claimed_count(), 0);
	}

	#[test]
	fn default_batch_size_is_120_over_workers() {
		let s: PreviewScheduler<u64> = PreviewScheduler::new(4, 0);
		assert_eq!(s.batch_size(), 30);
		let s: PreviewScheduler<u64> = PreviewScheduler::new(0, 0);
		assert_eq!(s.workers(), 1);
		assert_eq!(s.batch_size(), 120);
	}

	#[test]
	fn unknown_worker_claims_nothing() {
		let mut s: PreviewScheduler<u64> = PreviewScheduler::new(2, 4);
		s.submit(req(1, 0, FramePriority::Playback));
		assert!(s.claim_batch(2, 4, 1024).is_none());
	}
}
