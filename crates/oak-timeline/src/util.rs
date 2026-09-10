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

//! Inline helpers from `src/timeline/src/timelineutil.h`: Rational ↔
//! num/den pairs, value-handle identity, and block/track/project queries.
//!
//! Since the single-lib unification the oaknode C ABI is gone; every query
//! below operates directly on the oaknode Rust domain. A [`NodeRef`] pins a
//! `NodeId` inside a project's graph (`Arc<Mutex<oak_node::project::Project>>`),
//! replacing the opaque `CHandle` node/block/track references. Nullable
//! references are plain `Option<NodeRef>` — the old `CHandle::null()`
//! sentinel is `None`, and identity (`same_*`) is `NodeId` equality.
//!
//! Blocks/tracks that a command detaches from the project graph are stored
//! as an owned [`oak_node::graph::NodeEntry`] on the command itself:
//! [`block_remove_from_graph`] takes the entry out of the arena (identity is
//! preserved for re-insertion) and [`block_add_to_graph`] puts it back — the
//! Rust equivalent of the C++ `setParent(&memory_manager_)` re-homing.

use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::Rational;
use oak_node::block::{
	AdjustmentBlockBehavior, BlockCore, ClipBlockBehavior, GapBlockBehavior,
	TransitionBlockBehavior,
};
use oak_node::graph::{Graph, NodeEntry};
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};

/// A reference to a node — block, track, track list or sequence — inside
/// a project's graph. `Clone` is cheap: the project `Arc` is shared and
/// [`NodeId`] is `Copy`; the lifetime of the node follows the project.
///
/// This is the domain replacement for the opaque node/block/track
/// `CHandle`s of the deleted oaknode C ABI.
#[derive(Clone)]
pub struct NodeRef {
	/// The owning project (its graph holds the node).
	pub project: Arc<Mutex<Project>>,
	/// The node's id in that project's graph.
	pub id: NodeId,
}

impl NodeRef {
	/// New reference.
	pub fn new(project: Arc<Mutex<Project>>, id: NodeId) -> NodeRef {
		NodeRef { project, id }
	}

	/// Lock the owning project. A poisoned lock is recovered so a
	/// panicking command body cannot wedge every later edit.
	fn lock(&self) -> MutexGuard<'_, Project> {
		self.project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner())
	}
}

/// Split a `Rational` into numerator/denominator pairs for the C ABI
/// (timelineutil.h `rat_nd`). Kept for the ABI-compatible command paths.
pub fn rat_nd(r: Rational, n: &mut i32, d: &mut i32) {
	*n = r.numerator() as i32;
	*d = r.denominator() as i32;
}

/// `same_block`: two block references name the same graph node
/// (`NodeId` equality; both are assumed to live in the same project).
pub fn same_block(a: &NodeRef, b: &NodeRef) -> bool {
	a.id == b.id
}

/// `same_track`: two track references name the same graph node.
pub fn same_track(a: &NodeRef, b: &NodeRef) -> bool {
	a.id == b.id
}

/// `same_node`: two node references name the same graph node.
pub fn same_node(a: &NodeRef, b: &NodeRef) -> bool {
	a.id == b.id
}

/// Block kind (C++ `Block::Type` / the deleted `oaknode/block.h`
/// `OAKNODE_BLOCK_*` values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
	/// Not a block.
	Other,
	/// Clip block.
	Clip,
	/// Gap block.
	Gap,
	/// Transition block.
	Transition,
}

// ---------------------------------------------------------------------------
// Internal graph/behavior accessors
// ---------------------------------------------------------------------------

/// Immutable [`BlockCore`] of the block node `id` (any block behavior
/// kind shares the same core), or `None` for a stale/non-block node.
fn block_core_of(project: &Project, id: NodeId) -> Option<&BlockCore> {
	let entry = project.graph.get(id)?;
	entry.behavior.as_any().and_then(|a| {
		if let Some(clip) = a.downcast_ref::<ClipBlockBehavior>() {
			Some(&clip.core)
		} else if let Some(gap) = a.downcast_ref::<GapBlockBehavior>() {
			Some(&gap.core)
		} else if let Some(transition) = a.downcast_ref::<TransitionBlockBehavior>() {
			Some(&transition.core)
		} else if let Some(adjustment) = a.downcast_ref::<AdjustmentBlockBehavior>() {
			Some(&adjustment.core)
		} else {
			None
		}
	})
}

/// Mutable [`BlockCore`] of the block node `id`, or `None`.
fn block_core_of_mut(project: &mut Project, id: NodeId) -> Option<&mut BlockCore> {
	let entry = project.graph.get_mut(id)?;
	let a = entry.behavior.as_any_mut()?;
	if a.is::<ClipBlockBehavior>() {
		Some(&mut a.downcast_mut::<ClipBlockBehavior>().expect("checked above").core)
	} else if a.is::<GapBlockBehavior>() {
		Some(&mut a.downcast_mut::<GapBlockBehavior>().expect("checked above").core)
	} else if a.is::<TransitionBlockBehavior>() {
		Some(
			&mut a
				.downcast_mut::<TransitionBlockBehavior>()
				.expect("checked above")
				.core,
		)
	} else if a.is::<AdjustmentBlockBehavior>() {
		Some(
			&mut a
				.downcast_mut::<AdjustmentBlockBehavior>()
				.expect("checked above")
				.core,
		)
	} else {
		None
	}
}

/// Immutable [`TrackBehavior`] of the track node `id`, or `None`.
fn track_behavior_of(project: &Project, id: NodeId) -> Option<&TrackBehavior> {
	project.graph.get(id).and_then(|e| {
		e.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<TrackBehavior>())
	})
}

/// Mutable [`TrackBehavior`] of the track node `id`, or `None`.
fn track_behavior_of_mut(project: &mut Project, id: NodeId) -> Option<&mut TrackBehavior> {
	project.graph.get_mut(id).and_then(|e| {
		e.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackBehavior>())
	})
}

/// Immutable [`TrackListBehavior`] of the track-list node `id`, or `None`.
fn tracklist_behavior_of(project: &Project, id: NodeId) -> Option<&TrackListBehavior> {
	project.graph.get(id).and_then(|e| {
		e.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<TrackListBehavior>())
	})
}

/// Mutable [`TrackListBehavior`] of the track-list node `id`, or `None`.
fn tracklist_behavior_of_mut(project: &mut Project, id: NodeId) -> Option<&mut TrackListBehavior> {
	project.graph.get_mut(id).and_then(|e| {
		e.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackListBehavior>())
	})
}

/// Immutable [`SequenceBehavior`] of the sequence node `id`, or `None`.
fn sequence_behavior_of(project: &Project, id: NodeId) -> Option<&SequenceBehavior> {
	project.graph.get(id).and_then(|e| {
		e.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
	})
}

/// Graph-backed [`oak_node::track::BlockRange`] view: the block span
/// queries the track needs (in/out), read from the `BlockCore`s in the
/// arena.
pub struct GraphBlockRange<'a> {
	/// The graph the block nodes live in.
	pub graph: &'a Graph,
}

impl oak_node::track::BlockRange for GraphBlockRange<'_> {
	fn in_(&self, block: NodeId) -> Rational {
		self.graph
			.get(block)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| {
				[
					a.downcast_ref::<ClipBlockBehavior>()
						.map(|b| &b.core),
					a.downcast_ref::<GapBlockBehavior>().map(|b| &b.core),
					a.downcast_ref::<TransitionBlockBehavior>()
						.map(|b| &b.core),
					a.downcast_ref::<AdjustmentBlockBehavior>()
						.map(|b| &b.core),
				]
				.into_iter()
				.flatten()
				.next()
			})
			.map(BlockCore::in_)
			.unwrap_or_else(|| Rational::new(0, 1))
	}

	fn out(&self, block: NodeId) -> Rational {
		self.graph
			.get(block)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| {
				[
					a.downcast_ref::<ClipBlockBehavior>()
						.map(|b| &b.core),
					a.downcast_ref::<GapBlockBehavior>().map(|b| &b.core),
					a.downcast_ref::<TransitionBlockBehavior>()
						.map(|b| &b.core),
					a.downcast_ref::<AdjustmentBlockBehavior>()
						.map(|b| &b.core),
				]
				.into_iter()
				.flatten()
				.next()
			})
			.map(BlockCore::out)
			.unwrap_or_else(|| Rational::new(0, 1))
	}
}

// ---------------------------------------------------------------------------
// Block value queries / mutations
// ---------------------------------------------------------------------------

/// `block_in`: in point as a `Rational` (0/1 for a stale/non-block node,
/// matching the deleted bridge's untouched-output failure path).
pub fn block_in(b: &NodeRef) -> Rational {
	let p = b.lock();
	block_core_of(&p, b.id).map(BlockCore::in_).unwrap_or_else(|| Rational::new(0, 1))
}

/// `block_out`: out point as a `Rational`.
pub fn block_out(b: &NodeRef) -> Rational {
	let p = b.lock();
	block_core_of(&p, b.id).map(BlockCore::out).unwrap_or_else(|| Rational::new(0, 1))
}

/// `block_length`: block length as a `Rational`.
pub fn block_length(b: &NodeRef) -> Rational {
	let p = b.lock();
	block_core_of(&p, b.id)
		.map(BlockCore::length)
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// `block_kind`: which block behavior backs the node (C++
/// `oaknode_block_get_kind`).
pub fn block_kind(b: &NodeRef) -> BlockKind {
	let p = b.lock();
	match p.graph.get(b.id).and_then(|e| e.behavior.as_any()) {
		Some(a) if a.downcast_ref::<ClipBlockBehavior>().is_some() => BlockKind::Clip,
		Some(a) if a.downcast_ref::<GapBlockBehavior>().is_some() => BlockKind::Gap,
		Some(a) if a.downcast_ref::<TransitionBlockBehavior>().is_some() => BlockKind::Transition,
		_ => BlockKind::Other,
	}
}

/// `oaknode_block_get_enabled`: the block's enabled flag (false for a
/// stale node).
pub fn block_enabled(b: &NodeRef) -> bool {
	let p = b.lock();
	block_core_of(&p, b.id).map(|c| c.enabled).unwrap_or(false)
}

/// `oaknode_block_set_enabled`.
pub fn block_set_enabled(b: &NodeRef, enabled: bool) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.enabled = enabled;
	}
}

/// `block_set_length_and_media_out`: keep the out point anchored (the
/// in point shifts).
pub fn block_set_length_and_media_out(b: &NodeRef, len: Rational) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.set_length_and_media_out(len);
	}
}

/// `block_set_length_and_media_in`: keep the in point anchored (the
/// out point shifts).
pub fn block_set_length_and_media_in(b: &NodeRef, len: Rational) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.set_length_and_media_in(len);
	}
}

/// `block_set_in`: set the in point, keeping the length (the out follows).
pub fn block_set_in(b: &NodeRef, in_: Rational) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.set_in(in_);
	}
}

/// `oaknode_clip_get_media_in`: the block's media-in point. The media
/// fields live on the shared [`BlockCore`], so this works for any block
/// kind (the deleted ABI only exposed it for clips; closest behavior).
pub fn clip_media_in(b: &NodeRef) -> Rational {
	let p = b.lock();
	block_core_of(&p, b.id)
		.map(|c| c.media_in)
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// `oaknode_clip_set_media_in`.
pub fn clip_set_media_in(b: &NodeRef, media_in: Rational) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.media_in = media_in;
	}
}

/// The block's stored timeline range (`None` for a stale/non-block node).
pub fn block_range(b: &NodeRef) -> Option<oak_core::TimeRange> {
	let p = b.lock();
	block_core_of(&p, b.id).map(|c| c.range)
}

/// Set the block's stored timeline range (a no-op for a stale/non-block
/// node). The range is written whole, so the caller can move the in/out
/// pair together without a media-in side effect (unlike
/// [`block_set_length_and_media_out`] / [`block_set_length_and_media_in`]).
pub fn block_set_range(b: &NodeRef, range: oak_core::TimeRange) {
	let mut p = b.lock();
	if let Some(core) = block_core_of_mut(&mut p, b.id) {
		core.range = range;
	}
}

// ---------------------------------------------------------------------------
// Transition blocks
// ---------------------------------------------------------------------------

/// The transition block's `(in_offset, out_offset)` — the two wedges that
/// reach into its neighbours — or `None` for a non-transition node.
pub fn transition_offsets(t: &NodeRef) -> Option<(Rational, Rational)> {
	let p = t.lock();
	let a = p.graph.get(t.id)?.behavior.as_any()?;
	let transition = a.downcast_ref::<TransitionBlockBehavior>()?;
	Some((transition.in_offset, transition.out_offset))
}

/// Set the transition block's offsets (a no-op for a non-transition node).
pub fn transition_set_offsets(t: &NodeRef, in_offset: Rational, out_offset: Rational) {
	let mut p = t.lock();
	if let Some(a) = p.graph.get_mut(t.id).and_then(|e| e.behavior.as_any_mut()) {
		if let Some(transition) = a.downcast_mut::<TransitionBlockBehavior>() {
			transition.in_offset = in_offset;
			transition.out_offset = out_offset;
		}
	}
}

// ---------------------------------------------------------------------------
// Graph edges (the transition block's two sides)
// ---------------------------------------------------------------------------

/// The node feeding `b`'s `input` (the input's connected source), or `None`
/// when the input is unconnected or the node is stale.
pub fn block_connected_input(b: &NodeRef, input: &str) -> Option<NodeRef> {
	let p = b.lock();
	p.graph
		.connected_output(b.id, input, -1)
		.map(|id| NodeRef::new(b.project.clone(), id))
}

/// Connect `from`'s output into `to`'s `input`. No undo row of its own: the
/// calling command owns its undo/redo semantics.
pub fn block_connect(from: &NodeRef, to: &NodeRef, input: &str) {
	let mut p = from.lock();
	let _ = p.graph.connect(from.id, to.id, input, -1);
}

/// Disconnect `to`'s `input`, returning the node that fed it (`None` when
/// nothing was connected).
pub fn block_disconnect_input(to: &NodeRef, input: &str) -> Option<NodeRef> {
	let mut p = to.lock();
	p.graph
		.disconnect_input(to.id, input, -1)
		.map(|id| NodeRef::new(to.project.clone(), id))
}

// ---------------------------------------------------------------------------
// Block navigation (track membership)
// ---------------------------------------------------------------------------

/// `block_previous`: the block before `b` on its track (`None` when
/// trackless or first).
pub fn block_previous(b: &NodeRef) -> Option<NodeRef> {
	let p = b.lock();
	let track_id = block_core_of(&p, b.id).and_then(|c| c.track)?;
	let blocks = &track_behavior_of(&p, track_id)?.blocks;
	let index = blocks.iter().position(|id| *id == b.id)?;
	if index == 0 {
		return None;
	}
	Some(NodeRef::new(b.project.clone(), blocks[index - 1]))
}

/// `block_next`: the block after `b` on its track (`None` when trackless
/// or last).
pub fn block_next(b: &NodeRef) -> Option<NodeRef> {
	let p = b.lock();
	let track_id = block_core_of(&p, b.id).and_then(|c| c.track)?;
	let blocks = &track_behavior_of(&p, track_id)?.blocks;
	let index = blocks.iter().position(|id| *id == b.id)?;
	blocks
		.get(index + 1)
		.map(|id| NodeRef::new(b.project.clone(), *id))
}

/// `block_track`: the track owning `b` (`None` when detached from its
/// track, e.g. after a ripple-remove).
pub fn block_track(b: &NodeRef) -> Option<NodeRef> {
	let p = b.lock();
	block_core_of(&p, b.id)
		.and_then(|c| c.track)
		.map(|id| NodeRef::new(b.project.clone(), id))
}

// ---------------------------------------------------------------------------
// Track queries / mutations
// ---------------------------------------------------------------------------

/// `track_length`: track length as a `Rational` (end of the last block).
pub fn track_length(t: &NodeRef) -> Rational {
	let p = t.lock();
	match track_behavior_of(&p, t.id) {
		Some(track) => track.length(&GraphBlockRange { graph: &p.graph }),
		None => Rational::new(0, 1),
	}
}

/// `oaknode_track_get_block_count`.
pub fn track_block_count(t: &NodeRef) -> usize {
	let p = t.lock();
	track_behavior_of(&p, t.id).map(|t| t.blocks.len()).unwrap_or(0)
}

/// `oaknode_track_get_block_at`: block at `index` on the track.
pub fn track_block_at(t: &NodeRef, index: usize) -> Option<NodeRef> {
	let p = t.lock();
	track_behavior_of(&p, t.id)
		.and_then(|t| t.blocks.get(index))
		.map(|id| NodeRef::new(t.project.clone(), *id))
}

/// `oaknode_track_get_locked`.
pub fn track_locked(t: &NodeRef) -> bool {
	let p = t.lock();
	track_behavior_of(&p, t.id).map(|t| t.locked).unwrap_or(false)
}

/// `oaknode_track_get_nearest_block_before_or_at`: the block containing
/// `time`, else the last block entirely before it. `None` when the track
/// is empty or `time` precedes the first block.
///
/// CPP-PARITY timelineundoripple.cpp:64.
pub fn track_nearest_block_before_or_at(t: &NodeRef, time: Rational) -> Option<NodeRef> {
	let p = t.lock();
	let blocks = track_behavior_of(&p, t.id)?.blocks.clone();
	let mut before: Option<NodeId> = None;
	for id in blocks {
		let core = block_core_of(&p, id)?;
		if core.in_() <= time && core.out() > time {
			return Some(NodeRef::new(t.project.clone(), id));
		}
		if core.out() <= time {
			before = Some(id);
		}
	}
	before.map(|id| NodeRef::new(t.project.clone(), id))
}

/// `oaknode_track_get_nearest_block_after_or_at`: the block containing
/// `time`, else the first block starting at/after it.
///
/// CPP-PARITY timelineundoripple.cpp:551.
pub fn track_nearest_block_after_or_at(t: &NodeRef, time: Rational) -> Option<NodeRef> {
	let p = t.lock();
	let blocks = track_behavior_of(&p, t.id)?.blocks.clone();
	let mut after: Option<NodeId> = None;
	for id in blocks {
		let core = block_core_of(&p, id)?;
		if core.in_() <= time && core.out() > time {
			return Some(NodeRef::new(t.project.clone(), id));
		}
		if core.in_() >= time {
			after = Some(id);
			break;
		}
	}
	after.map(|id| NodeRef::new(t.project.clone(), id))
}

/// C++ `oaknode_track_append_block` — append a block at the end of the
/// track's block list.
pub fn track_append_block(track: &NodeRef, block: &NodeRef) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| {
			t.append_block(block.id);
			true
		})
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, block.id) {
			core.track = Some(track.id);
		}
	}
}

/// C++ `oaknode_track_insert_block_before`: insert `block` before `next`
/// on the track (prepend when `next` is absent).
pub fn track_insert_block_before(track: &NodeRef, block: &NodeRef, next: &NodeRef) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| t.insert_block_before(block.id, next.id))
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, block.id) {
			core.track = Some(track.id);
		}
	}
}

/// C++ `oaknode_track_insert_block_after`: insert `block` after `before`
/// on the track. `None` for `before` prepends (the deleted ABI tolerated
/// an empty predecessor like C++ did).
pub fn track_insert_block_after(track: &NodeRef, block: &NodeRef, before: Option<&NodeRef>) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| match before {
			Some(b) => t.insert_block_after(block.id, b.id),
			None => {
				t.prepend_block(block.id);
				true
			}
		})
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, block.id) {
			core.track = Some(track.id);
		}
	}
}

/// C++ `oaknode_track_prepend_block`.
pub fn track_prepend_block(track: &NodeRef, block: &NodeRef) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| {
			t.prepend_block(block.id);
			true
		})
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, block.id) {
			core.track = Some(track.id);
		}
	}
}

/// C++ `oaknode_track_replace_block`: swap `old` for `replace` at the
/// same track position. `old` loses its track membership, `replace`
/// gains it.
pub fn track_replace_block(track: &NodeRef, old: &NodeRef, replace: &NodeRef) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| t.replace_block(old.id, replace.id))
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, old.id) {
			core.track = None;
		}
		if let Some(core) = block_core_of_mut(&mut p, replace.id) {
			core.track = Some(track.id);
		}
	}
}

/// C++ `oaknode_track_ripple_remove_block`: remove the block from its
/// track. In the module model the block's in/out points are stored on
/// the block (not derived from track order), so "ripple" is only the
/// membership removal — positions of later blocks stay as stored, which
/// the undo re-insertion mirrors exactly (CPP-PARITY deviation).
pub fn track_ripple_remove_block(track: &NodeRef, block: &NodeRef) {
	let mut p = track.lock();
	let ok = track_behavior_of_mut(&mut p, track.id)
		.map(|t| t.ripple_remove_block(block.id))
		.unwrap_or(false);
	if ok {
		if let Some(core) = block_core_of_mut(&mut p, block.id) {
			core.track = None;
		}
	}
}

// ---------------------------------------------------------------------------
// Project graph attach / detach (C++ `setParent(&memory_manager_)` paths)
// ---------------------------------------------------------------------------

/// `block_add_to_graph`: re-insert a previously detached block entry
/// into the project graph, preserving its identity where the original
/// slot is free (C++ re-parenting from the scratch memory manager back
/// into the project). Passing `None` is a no-op — blocks created via
/// [`block_gap_create`]/[`block_clip_create`] are already in the graph.
pub fn block_add_to_graph(block: &NodeRef, entry: Option<NodeEntry>) {
	if let Some(entry) = entry {
		let mut p = block.lock();
		p.graph.add_entry(entry, block.id);
	}
}

/// `block_remove_from_graph`: detach `block` from the project graph,
/// returning its full entry for the caller to own (and re-add with
/// [`block_add_to_graph`]). `None` when the node is stale or already
/// detached.
pub fn block_remove_from_graph(block: &NodeRef) -> Option<NodeEntry> {
	let mut p = block.lock();
	p.graph.take_node(block.id)
}

// ---------------------------------------------------------------------------
// Block / track / track-list creation
// ---------------------------------------------------------------------------

/// C++ `oaknode_block_clip_create`: a fresh clip block node in the given
/// project's graph.
pub fn block_clip_create(project: &Arc<Mutex<Project>>) -> NodeRef {
	let (core, behavior) = oak_node::block::clip_create();
	let id = {
		let mut p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		p.graph.add_node(core, behavior)
	};
	NodeRef::new(project.clone(), id)
}

/// C++ `oaknode_block_gap_create`: a fresh gap block node in the given
/// project's graph.
pub fn block_gap_create(project: &Arc<Mutex<Project>>) -> NodeRef {
	let (core, behavior) = oak_node::block::gap_create();
	let id = {
		let mut p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		p.graph.add_node(core, behavior)
	};
	NodeRef::new(project.clone(), id)
}

/// C++ `oaknode_track_create`: a fresh track node of the given type in
/// the project's graph (detached from any track list until appended).
pub fn track_create(project: &Arc<Mutex<Project>>, kind: TrackType) -> NodeRef {
	let core = NodeCore::new();
	let behavior = Box::new(TrackBehavior::new(kind));
	let id = {
		let mut p = project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		p.graph.add_node(core, behavior)
	};
	NodeRef::new(project.clone(), id)
}

// ---------------------------------------------------------------------------
// Track list / sequence queries
// ---------------------------------------------------------------------------

/// `oaknode_tracklist_get_track_count`.
pub fn tracklist_track_count(list: &NodeRef) -> usize {
	let p = list.lock();
	tracklist_behavior_of(&p, list.id)
		.map(|l| l.tracks.len())
		.unwrap_or(0)
}

/// `oaknode_tracklist_get_track_at`: track at `index` in the list.
pub fn tracklist_track_at(list: &NodeRef, index: usize) -> Option<NodeRef> {
	let p = list.lock();
	tracklist_behavior_of(&p, list.id)
		.and_then(|l| l.tracks.get(index))
		.map(|id| NodeRef::new(list.project.clone(), *id))
}

/// `oaknode_tracklist_get_type`: the list's media type (`None` for a
/// stale node).
pub fn tracklist_type(list: &NodeRef) -> Option<TrackType> {
	let p = list.lock();
	tracklist_behavior_of(&p, list.id).map(|l| l.kind)
}

/// `oaknode_tracklist_array_append`: append `track` to the list (sets the
/// track's list membership and its index).
pub fn tracklist_append(list: &NodeRef, track: &NodeRef) {
	let mut p = list.lock();
	let index = tracklist_behavior_of_mut(&mut p, list.id)
		.map(|l| {
			l.tracks.push(track.id);
			l.tracks.len() as i32 - 1
		})
		.unwrap_or(-1);
	if index >= 0 {
		if let Some(t) = track_behavior_of_mut(&mut p, track.id) {
			t.track_list = Some(list.id);
			t.index = index;
		}
	}
}

/// `oaknode_tracklist_array_remove_last`: drop the last track from the
/// list (detaching its list membership; the node itself is removed from
/// the graph separately by the caller).
pub fn tracklist_remove_last(list: &NodeRef) -> Option<NodeRef> {
	let mut p = list.lock();
	let removed = tracklist_behavior_of_mut(&mut p, list.id)
		.and_then(|l| l.tracks.pop())?;
	if let Some(t) = track_behavior_of_mut(&mut p, removed) {
		t.track_list = None;
		t.index = 0;
	}
	Some(NodeRef::new(list.project.clone(), removed))
}

/// `oaknode_sequence_get_track_list`: the sequence's track list of the
/// given type.
pub fn sequence_track_list(sequence: &NodeRef, kind: TrackType) -> Option<NodeRef> {
	let p = sequence.lock();
	let lists = sequence_behavior_of(&p, sequence.id)?.track_lists.clone();
	for id in lists {
		if tracklist_behavior_of(&p, id).map(|l| l.kind) == Some(kind) {
			return Some(NodeRef::new(sequence.project.clone(), id));
		}
	}
	None
}

/// Remove `track` from the list by id and renumber the remaining tracks
/// (the precise counterpart of `tracklist_remove_last` for undo paths
/// that must not eat the wrong track).
pub fn tracklist_remove(list: &NodeRef, track: &NodeRef) {
	let mut p = list.lock();
	let ids: Vec<NodeId> = {
		let Some(l) = tracklist_behavior_of_mut(&mut p, list.id) else {
			return;
		};
		l.tracks.retain(|&t| t != track.id);
		l.tracks.clone()
	};
	for (i, id) in ids.iter().enumerate() {
		if let Some(t) = track_behavior_of_mut(&mut p, *id) {
			t.index = i as i32;
		}
	}
}

/// `oaknode_sequence_get_all_track_count` / `get_all_track_at`: every
/// track of every track list owned by the sequence, in list order.
pub fn sequence_all_tracks(sequence: &NodeRef) -> Vec<NodeRef> {
	let p = sequence.lock();
	let mut tracks = Vec::new();
	if let Some(seq) = sequence_behavior_of(&p, sequence.id) {
		let lists = seq.track_lists.clone();
		for list_id in lists {
			if let Some(list) = tracklist_behavior_of(&p, list_id) {
				for track_id in &list.tracks {
					tracks.push(NodeRef::new(sequence.project.clone(), *track_id));
				}
			}
		}
	}
	tracks
}
