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

//! General-purpose timeline commands (`src/timeline/src/timelineundogeneral.h`):
//! block resizing, media-in, track add/remove, transition removal, gap
//! replacement, enable/disable, gap insertion and default transitions.
//!
//! Graph mutation routes directly through the oaknode Rust domain (the
//! oaknode C ABI was deleted in the single-lib unification); command
//! wrapping through `oak_undo::undocommand::UndoCommand` values. Commands
//! that compose child commands own them as plain structs rather than
//! pointers (see `undocommon`). Detached blocks are held as owned arena
//! entries on the command that detached them.

use oak_core::{Rational, TimeRange};
use oak_node::block::transition_input::{IN_BLOCK, OUT_BLOCK};
use oak_node::graph::NodeEntry;
use oak_node::track::TrackType;
use oak_undo::undocommand::UndoCommand;

use crate::undocommon::{box_command, create_block_remove_command, Command};
use crate::undosplit::BlockSplitPreservingLinksCommand;
use crate::util::{
	block_add_to_graph, block_connect, block_connected_input, block_disconnect_input,
	block_enabled, block_gap_create, block_in, block_kind, block_length, block_next, block_out,
	block_previous, block_range, block_remove_from_graph, block_set_enabled, block_set_in,
	block_set_length_and_media_in, block_set_length_and_media_out, block_set_range, block_track,
	clip_media_in, clip_set_media_in, same_block, track_append_block, track_create,
	track_insert_block_after, track_insert_block_before, track_replace_block,
	track_ripple_remove_block, tracklist_append, tracklist_remove, tracklist_track_at,
	tracklist_track_count, tracklist_type, transition_offsets,
	transition_set_offsets, BlockKind, NodeRef,
};

// `oaknode/sequence.h` element input ids (the C++ automerge branch).
const OAKNODE_SEQUENCE_TEXTURE_INPUT: &str = "tex_in";
const OAKNODE_SEQUENCE_SAMPLES_INPUT: &str = "samples_in";

/// `BlockResizeCommand` — change a block's length without touching its
/// media-in point (timelineundogeneral.h). The old length is captured at
/// `redo` time.
pub struct BlockResizeCommand {
	/// Block to resize.
	block: NodeRef,
	/// New length.
	new_length: Rational,
	/// Length captured at `redo` time, restored by `undo`.
	old_length: Rational,
}

impl BlockResizeCommand {
	/// Construct from block + new length.
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, new_length: Rational) -> BlockResizeCommand`
	pub fn new(block: NodeRef, new_length: Rational) -> Self {
		Self {
			block,
			new_length,
			old_length: Rational::new(0, 1),
		}
	}

	/// `redo`: capture `old_length`, then set the block's length.
	pub fn redo(&mut self) {
		self.old_length = block_length(&self.block);
		block_set_length_and_media_out(&self.block, self.new_length);
	}

	/// `undo`: restore `old_length`.
	pub fn undo(&mut self) {
		block_set_length_and_media_out(&self.block, self.old_length);
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for BlockResizeCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `BlockResizeWithMediaInCommand` — change a block's length while keeping
/// its out point fixed (media-in moves with the length)
/// (timelineundogeneral.h). The old length is captured at `redo` time.
pub struct BlockResizeWithMediaInCommand {
	/// Block to resize.
	block: NodeRef,
	/// New length.
	new_length: Rational,
	/// Length captured at `redo` time, restored by `undo`.
	old_length: Rational,
}

impl BlockResizeWithMediaInCommand {
	/// Construct from block + new length.
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, new_length: Rational) -> BlockResizeWithMediaInCommand`
	pub fn new(block: NodeRef, new_length: Rational) -> Self {
		Self {
			block,
			new_length,
			old_length: Rational::new(0, 1),
		}
	}

	/// `redo`: capture `old_length`, then resize keeping the out point fixed.
	pub fn redo(&mut self) {
		self.old_length = block_length(&self.block);
		block_set_length_and_media_in(&self.block, self.new_length);
	}

	/// `undo`: restore `old_length`.
	pub fn undo(&mut self) {
		block_set_length_and_media_in(&self.block, self.old_length);
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for BlockResizeWithMediaInCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `BlockSetMediaInCommand` — change a block's media-in point
/// (timelineundogeneral.h). The old media-in is captured at `redo` time.
pub struct BlockSetMediaInCommand {
	/// Block whose media-in changes.
	block: NodeRef,
	/// New media-in.
	new_media_in: Rational,
	/// Media-in captured at `redo` time, restored by `undo`.
	old_media_in: Rational,
}

impl BlockSetMediaInCommand {
	/// Construct from block + new media-in.
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, new_media_in: Rational) -> BlockSetMediaInCommand`
	pub fn new(block: NodeRef, new_media_in: Rational) -> Self {
		Self {
			block,
			new_media_in,
			old_media_in: Rational::new(0, 1),
		}
	}

	/// `redo`: capture `old_media_in`, then set the media-in.
	pub fn redo(&mut self) {
		self.old_media_in = clip_media_in(&self.block);
		clip_set_media_in(&self.block, self.new_media_in);
	}

	/// `undo`: restore `old_media_in`.
	pub fn undo(&mut self) {
		clip_set_media_in(&self.block, self.old_media_in);
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for BlockSetMediaInCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TimelineAddTrackCommand` — append a track (and optionally merge an
/// existing one) to a track list (timelineundogeneral.h). When `automerge` is
/// set, a matching neighbouring track is merged into the new one.
///
/// The merge/input-id members below mirror the C++ class layout and are
/// reserved for the automerge step (not modelled in Rust yet).
#[allow(dead_code)]
pub struct TimelineAddTrackCommand {
	/// Target track list.
	timeline: NodeRef,
	/// Track created by this command.
	track: NodeRef,
	/// Merge node used when `automerge_tracks` is set (unused: see `new`).
	merge: Option<NodeRef>,
	/// Input id the track connects to (`tex_in`/`samples_in`).
	direct_input: String,
	/// Merge base input id (unused: see `new`).
	base_input: String,
	/// Merge blend input id (unused: see `new`).
	blend_input: String,
	/// Whether to automerge a matching adjacent track.
	automerge_tracks: bool,
	/// The detached arena entry while `track` is out of the graph
	/// (between `undo` and the next `redo`).
	track_entry: Option<NodeEntry>,
}

impl TimelineAddTrackCommand {
	/// Construct with the default automerge behaviour.
	///
	/// New signature (single-lib): `pub fn new(timeline: NodeRef) -> TimelineAddTrackCommand`
	pub fn new(timeline: NodeRef) -> Self {
		// NOTE: the C++ reads the `AutoMergeTracks` config via
		// `oak_core_config_get_bool`; the module exposes no config
		// getter, so the default is hardcoded to `false`.
		Self::with_automerge(timeline, false)
	}

	/// Construct with an explicit automerge flag.
	///
	/// New signature (single-lib): `pub fn with_automerge(timeline: NodeRef, automerge: bool) -> TimelineAddTrackCommand`
	pub fn with_automerge(timeline: NodeRef, automerge: bool) -> Self {
		let kind = tracklist_type(&timeline).unwrap_or(TrackType::Video);
		// SAFETY-free: the track is created directly in the list's project
		// graph (the C++ creates it in the scratch project then adds it on
		// redo; here both steps collapse into the constructor).
		let track = track_create(&timeline.project, kind);
		let direct_input = match kind {
			TrackType::Video => OAKNODE_SEQUENCE_TEXTURE_INPUT.to_string(),
			TrackType::Audio => OAKNODE_SEQUENCE_SAMPLES_INPUT.to_string(),
			TrackType::Subtitle => String::new(),
		};
		// NOTE: the C++ merge branch creates a merge/math node via
		// `oaknode_factory_create_from_id` when the sequence input is
		// already connected; the node-graph connection machinery for
		// track inputs has no Rust equivalent here, so `merge`/
		// `base_input`/`blend_input` stay empty and the automerge flag is
		// retained but not acted on.
		Self {
			timeline,
			track,
			merge: None,
			direct_input,
			base_input: String::new(),
			blend_input: String::new(),
			automerge_tracks: automerge,
			track_entry: None,
		}
	}

	/// `redo`: create (and merge) the track; `track()` then reports it.
	pub fn redo(&mut self) {
		// NOTE: the C++ redo copies the last track's height, connects the
		// track to the sequence element and builds an undoable position
		// command; those steps have no Rust equivalent here (the track
		// input edges are not modelled), so only the list append is
		// performed.
		if self.track_entry.is_some() {
			block_add_to_graph(&self.track, self.track_entry.take());
		}
		// NLE track-growth direction is a DISPLAY concern: the graph list
		// always appends, and the UI renders video/subtitle lists reversed
		// (new track on top) but audio lists in order (new track below).
		tracklist_append(&self.timeline, &self.track);
	}

	/// `undo`: remove the created track and restore the merged track.
	pub fn undo(&mut self) {
		// NOTE: the C++ undo disconnects the merge/direct input and removes
		// the track node from the project graph; the disconnects have no
		// Rust equivalent, but the graph removal is real: the track's arena
		// entry is detached and owned here until the next redo. Removes
		// THIS track by id (not blindly the last one) and renumbers the
		// remaining tracks.
		tracklist_remove(&self.timeline, &self.track);
		if self.track_entry.is_none() {
			self.track_entry = block_remove_from_graph(&self.track);
		}
	}

	/// The track created by this command; only meaningful after `redo`.
	///
	/// New signature (single-lib): `pub fn track(&self) -> NodeRef`
	pub fn track(&self) -> NodeRef {
		self.track.clone()
	}

	/// `TimelineAddTrackCommand::run_immediately` — construct, `redo` and
	/// return the created track in one step (static-semantics factory).
	///
	/// New signature (single-lib): `pub fn run_immediately(timeline: NodeRef) -> NodeRef`
	pub fn run_immediately(timeline: NodeRef) -> NodeRef {
		let mut c = Self::new(timeline);
		c.redo();
		c.track()
	}

	/// Overload of `run_immediately` with an explicit automerge flag.
	///
	/// New signature (single-lib): `pub fn run_immediately_with_automerge(timeline: NodeRef, automerge: bool) -> NodeRef`
	pub fn run_immediately_with_automerge(timeline: NodeRef, automerge: bool) -> NodeRef {
		let mut c = Self::with_automerge(timeline, automerge);
		c.redo();
		c.track()
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TimelineAddTrackCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TimelineRemoveTrackCommand` — remove a track from its list and restore it
/// on `undo` (timelineundogeneral.h).
pub struct TimelineRemoveTrackCommand {
	/// Track to remove.
	track: NodeRef,
	/// Owning track list, located at `prepare` time.
	list: Option<NodeRef>,
	/// Index of the track in its list (before removal).
	index: Option<usize>,
	/// The detached arena entry while `track` is out of the graph
	/// (between `redo` and `undo`).
	track_entry: Option<NodeEntry>,
}

impl TimelineRemoveTrackCommand {
	/// Construct from the track to remove.
	///
	/// New signature (single-lib): `pub fn new(track: NodeRef) -> TimelineRemoveTrackCommand`
	pub fn new(track: NodeRef) -> Self {
		Self {
			track,
			list: None,
			index: None,
			track_entry: None,
		}
	}

	/// `prepare`: locate the owning list and index.
	pub fn prepare(&mut self) {
		if self.list.is_some() {
			return;
		}
		let (list_id, index) = {
			let project = self.track.project.clone();
			let p = project.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
			let Some(entry) = p.graph.get(self.track.id) else {
				return;
			};
			let Some(behavior) = entry.behavior.as_any() else {
				return;
			};
			let Some(track_behavior) = behavior.downcast_ref::<oak_node::track::TrackBehavior>()
			else {
				return;
			};
			let Some(list_id) = track_behavior.track_list else {
				return;
			};
			let Some(list_entry) = p.graph.get(list_id) else {
				return;
			};
			let index = list_entry
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<oak_node::track::TrackListBehavior>())
				.and_then(|l| l.track_index(self.track.id));
			(list_id, index)
		};
		self.list = Some(NodeRef::new(self.track.project.clone(), list_id));
		self.index = index;
	}

	/// `redo`: remove the track from the list and from the graph.
	pub fn redo(&mut self) {
		self.prepare();
		let Some(list) = &self.list else {
			return;
		};
		let index = self.index;
		// First pass: remove the track from the list and collect the ids of
		// the tracks after it (their indexes shift).
		let mut removed = false;
		let mut later: Vec<oak_node::id::NodeId> = Vec::new();
		{
			let mut p = self
				.track
				.project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			if let Some(entry) = p.graph.get_mut(list.id) {
				if let Some(a) = entry.behavior.as_any_mut() {
					if let Some(l) = a.downcast_mut::<oak_node::track::TrackListBehavior>() {
						if let Some(i) = l.tracks.iter().position(|t| *t == self.track.id) {
							l.tracks.remove(i);
							later = l.tracks[i..].to_vec();
							removed = true;
						}
					}
				}
			}
		}
		// Second pass: shift the later tracks' indexes (C++ array semantics).
		if removed {
			{
				let mut p = self
					.track
					.project
					.lock()
					.unwrap_or_else(|poisoned| poisoned.into_inner());
				for t in &later {
					if let Some(te) = p.graph.get_mut(*t) {
						if let Some(ta) = te.behavior.as_any_mut() {
							if let Some(tb) = ta.downcast_mut::<oak_node::track::TrackBehavior>() {
								if tb.index > index.unwrap_or(0) as i32 {
									tb.index -= 1;
								}
							}
						}
					}
				}
			}
			// Detach the track's sequence element membership from the graph
			// (the C++ also removes the sequence input connection, which the
			// Rust model does not track as edges).
			if self.track_entry.is_none() {
				self.track_entry = block_remove_from_graph(&self.track);
			}
		}
	}

	/// `undo`: re-insert the track at its former index.
	pub fn undo(&mut self) {
		let Some(list) = &self.list else {
			return;
		};
		if self.track_entry.is_some() {
			block_add_to_graph(&self.track, self.track_entry.take());
		}
		let index = self.index.unwrap_or(0);
		// Re-insert the track into the list and collect the tracks after it
		// (their indexes shift).
		let later: Vec<oak_node::id::NodeId> = 'block: {
			let mut p = self
				.track
				.project
				.lock()
				.unwrap_or_else(|poisoned| poisoned.into_inner());
			if let Some(entry) = p.graph.get_mut(list.id) {
				if let Some(a) = entry.behavior.as_any_mut() {
					if let Some(l) = a.downcast_mut::<oak_node::track::TrackListBehavior>() {
						let insert_at = index.min(l.tracks.len());
						l.tracks.insert(insert_at, self.track.id);
						break 'block l.tracks[insert_at + 1..].to_vec();
					}
				}
			}
			Vec::new()
		};
		let mut p = self
			.track
			.project
			.lock()
			.unwrap_or_else(|poisoned| poisoned.into_inner());
		for t in &later {
			if let Some(te) = p.graph.get_mut(*t) {
				if let Some(ta) = te.behavior.as_any_mut() {
					if let Some(tb) = ta.downcast_mut::<oak_node::track::TrackBehavior>() {
						if tb.index >= index as i32 {
							tb.index += 1;
						}
					}
				}
			}
		}
		if let Some(entry) = p.graph.get_mut(self.track.id) {
			if let Some(a) = entry.behavior.as_any_mut() {
				if let Some(tb) = a.downcast_mut::<oak_node::track::TrackBehavior>() {
					tb.track_list = Some(list.id);
					tb.index = index as i32;
				}
			}
		}
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TimelineRemoveTrackCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TransitionSetOffsetsCommand` — change a transition block's two wedge
/// offsets, keeping the block pinned to the same seam.
///
/// The C++ timeline has no equivalent (a transition's offsets are written
/// once, at creation); the Rust port needs one so the timeline's wedge drag
/// (`TransitionChanged`) can resize a transition undoably. A transition spans
/// `seam - in_offset .. seam + out_offset`, so the stored range follows the
/// offsets — growing one wedge without moving the matching range end would
/// drift the block off its seam.
pub struct TransitionSetOffsetsCommand {
	/// Transition block to change.
	block: NodeRef,
	/// New in offset (`None` keeps the current one).
	new_in_offset: Option<Rational>,
	/// New out offset (`None` keeps the current one).
	new_out_offset: Option<Rational>,
	/// Whether the pre-change state has been captured.
	captured: bool,
	/// In offset captured at `redo` time, restored by `undo`.
	old_in_offset: Rational,
	/// Out offset captured at `redo` time, restored by `undo`.
	old_out_offset: Rational,
	/// Stored range captured at `redo` time, restored by `undo`.
	old_range: TimeRange,
	/// The seam the offsets straddle, captured at `redo` time.
	seam: Rational,
}

impl TransitionSetOffsetsCommand {
	/// Construct from a transition block and the offsets to set (`None`
	/// leaves that side as it is).
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, new_in_offset: Option<Rational>, new_out_offset: Option<Rational>) -> TransitionSetOffsetsCommand`
	pub fn new(
		block: NodeRef,
		new_in_offset: Option<Rational>,
		new_out_offset: Option<Rational>,
	) -> Self {
		Self {
			block,
			new_in_offset,
			new_out_offset,
			captured: false,
			old_in_offset: Rational::new(0, 1),
			old_out_offset: Rational::new(0, 1),
			old_range: TimeRange::default(),
			seam: Rational::new(0, 1),
		}
	}

	/// `redo`: capture the offsets/range once, then write the new offsets
	/// and move the range with them.
	pub fn redo(&mut self) {
		if !self.captured {
			if let Some((in_offset, out_offset)) = transition_offsets(&self.block) {
				self.old_in_offset = in_offset;
				self.old_out_offset = out_offset;
			}
			if let Some(range) = block_range(&self.block) {
				self.old_range = range;
			}
			self.seam = block_in(&self.block) + self.old_in_offset;
			self.captured = true;
		}

		let in_offset = self.new_in_offset.unwrap_or(self.old_in_offset);
		let out_offset = self.new_out_offset.unwrap_or(self.old_out_offset);
		transition_set_offsets(&self.block, in_offset, out_offset);
		block_set_range(
			&self.block,
			TimeRange::new(self.seam - in_offset, self.seam + out_offset),
		);
	}

	/// `undo`: restore the captured offsets and range.
	pub fn undo(&mut self) {
		transition_set_offsets(&self.block, self.old_in_offset, self.old_out_offset);
		block_set_range(&self.block, self.old_range);
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TransitionSetOffsetsCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TransitionRemoveCommand` — detach a transition block from its neighbours,
/// optionally removing it from the whole graph (timelineundogeneral.h).
pub struct TransitionRemoveCommand {
	/// Transition block to remove.
	block: NodeRef,
	/// Whether to also remove the block from the graph.
	remove_from_graph: bool,
	/// Owning track, captured at `redo` time.
	track: Option<NodeRef>,
	/// Block after the transition.
	out_block: Option<NodeRef>,
	/// Block before the transition.
	in_block: Option<NodeRef>,
	/// Whether the pre-removal state has been captured.
	captured: bool,
	/// In offset captured before removal, restored by `undo`.
	in_offset: Rational,
	/// Out offset captured before removal, restored by `undo`.
	out_offset: Rational,
	/// Stored range captured before removal, restored by `undo`.
	range: TimeRange,
	/// Node feeding the transition's `out_block_in` input (the outgoing
	/// clip), captured before removal and restored by `undo`.
	connected_out: Option<NodeRef>,
	/// Node feeding the transition's `in_block_in` input (the incoming
	/// clip), captured before removal and restored by `undo`.
	connected_in: Option<NodeRef>,
	/// Graph-removal command built at `redo` time.
	remove_command: Option<UndoCommand>,
}

impl TransitionRemoveCommand {
	/// Construct from the transition block + remove-from-graph flag.
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, remove_from_graph: bool) -> TransitionRemoveCommand`
	pub fn new(block: NodeRef, remove_from_graph: bool) -> Self {
		Self {
			block,
			remove_from_graph,
			track: None,
			out_block: None,
			in_block: None,
			captured: false,
			in_offset: Rational::new(0, 1),
			out_offset: Rational::new(0, 1),
			range: TimeRange::default(),
			connected_out: None,
			connected_in: None,
			remove_command: None,
		}
	}

	/// `redo`: unlink the transition from its neighbours and remove it.
	pub fn redo(&mut self) {
		self.track = block_track(&self.block);
		self.out_block = block_next(&self.block);
		self.in_block = block_previous(&self.block);
		// The C++ also extends the neighbouring blocks by the transition's
		// offsets; the Rust blocks already meet at the seam once the
		// transition leaves the track (a transition's span overlaps its
		// neighbours instead of filling a gap of its own), so only the detach
		// is performed. The offsets, the stored range and both input edges
		// are captured first so `undo` can put the block back as it was.
		if !self.captured {
			if let Some((in_offset, out_offset)) = transition_offsets(&self.block) {
				self.in_offset = in_offset;
				self.out_offset = out_offset;
			}
			if let Some(range) = block_range(&self.block) {
				self.range = range;
			}
			self.connected_out = block_connected_input(&self.block, OUT_BLOCK);
			self.connected_in = block_connected_input(&self.block, IN_BLOCK);
			self.captured = true;
		}
		block_disconnect_input(&self.block, OUT_BLOCK);
		block_disconnect_input(&self.block, IN_BLOCK);
		if let Some(track) = &self.track {
			track_ripple_remove_block(track, &self.block);
		}
		if self.remove_from_graph {
			if self.remove_command.is_none() {
				self.remove_command = Some(create_block_remove_command(&self.block));
			}
			if let Some(c) = self.remove_command.as_mut() {
				c.redo_now();
			}
		}
	}

	/// `undo`: restore the transition, its offsets and its two edges.
	pub fn undo(&mut self) {
		if self.remove_from_graph {
			if let Some(c) = self.remove_command.as_mut() {
				c.undo_now();
			}
		}
		if let Some(track) = &self.track {
			match &self.in_block {
				Some(before) => track_insert_block_after(track, &self.block, Some(before)),
				None => track_insert_block_after(track, &self.block, None),
			}
		}
		transition_set_offsets(&self.block, self.in_offset, self.out_offset);
		block_set_range(&self.block, self.range);
		if let Some(source) = &self.connected_out {
			block_connect(source, &self.block, OUT_BLOCK);
		}
		if let Some(source) = &self.connected_in {
			block_connect(source, &self.block, IN_BLOCK);
		}
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TransitionRemoveCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TrackReplaceBlockWithGapCommand` — replace a block on a track with a gap
/// of equal length (timelineundogeneral.h). When `handle_transitions` is set,
/// attached transitions are also removed.
pub struct TrackReplaceBlockWithGapCommand {
	/// Owning track.
	track: NodeRef,
	/// Block to replace.
	block: NodeRef,
	/// The block's in point captured at construction: the gap must fill
	/// the block's stored span, even when a parent command (e.g.
	/// `TrackMoveBlockCommand`) re-homes the block's position before
	/// this command runs.
	block_in_point: Rational,
	/// Whether to also remove attached transitions.
	handle_transitions: bool,
	/// Pre-existing gap extended in place (found at `redo` time).
	existing_gap: Option<NodeRef>,
	/// Second gap merged into `existing_gap` (found at `redo` time).
	existing_merged_gap: Option<NodeRef>,
	/// Whether `existing_gap` preceded the block (decides re-insert order).
	existing_gap_precedes: bool,
	/// Gap created by this command (owned until placed in the graph).
	our_gap: Option<NodeRef>,
	/// Arena entry of `our_gap` while detached from the graph.
	our_gap_entry: Option<NodeEntry>,
	/// Arena entry of `existing_merged_gap` while detached from the graph.
	merged_gap_entry: Option<NodeEntry>,
	/// Child transition-removal commands (empty: see
	/// `create_remove_transition_command_if_necessary`).
	transition_remove_commands: Vec<TransitionRemoveCommand>,
}

impl TrackReplaceBlockWithGapCommand {
	/// Construct from track + block + transition handling flag.
	///
	/// New signature (single-lib): `pub fn new(track: NodeRef, block: NodeRef, handle_transitions: bool) -> TrackReplaceBlockWithGapCommand`
	pub fn new(track: NodeRef, block: NodeRef, handle_transitions: bool) -> Self {
		let block_in_point = block_in(&block);
		Self {
			track,
			block,
			block_in_point,
			handle_transitions,
			existing_gap: None,
			existing_merged_gap: None,
			existing_gap_precedes: false,
			our_gap: None,
			our_gap_entry: None,
			merged_gap_entry: None,
			transition_remove_commands: Vec::new(),
		}
	}

	/// `redo`: merge/replace with a gap, removing transitions as configured.
	pub fn redo(&mut self) {
		if self.handle_transitions && self.transition_remove_commands.is_empty() {
			self.create_remove_transition_command_if_necessary(false);
			self.create_remove_transition_command_if_necessary(true);
		}
		for c in self.transition_remove_commands.iter_mut() {
			c.redo();
		}

		if block_next(&self.block).is_some() {
			// The block is not at the end of the track, so a gap must fill its space.
			let mut new_gap_length = block_length(&self.block);
			let previous = block_previous(&self.block);
			let next = block_next(&self.block);

			let previous_is_a_gap = previous.as_ref().map(|b| block_kind(b) == BlockKind::Gap).unwrap_or(false);
			let next_is_a_gap = next.as_ref().map(|b| block_kind(b) == BlockKind::Gap).unwrap_or(false);

			if previous_is_a_gap && next_is_a_gap {
				// The clip is preceded and followed by a gap, so merge the two.
				self.existing_gap = previous.clone();
				self.existing_merged_gap = next;
				let merged = self.existing_merged_gap.clone();
				if let Some(m) = &merged {
					new_gap_length = new_gap_length + block_length(m);
					track_ripple_remove_block(&self.track, m);
					self.merged_gap_entry = block_remove_from_graph(m);
				}
			} else if previous_is_a_gap {
				// Extend this gap to fill the space left by the block.
				self.existing_gap = previous.clone();
			} else if next_is_a_gap {
				// Extend this gap to fill the space left by the block.
				self.existing_gap = next;
			}

			if let Some(gap) = &self.existing_gap {
				// Extend an existing gap. In the module world the block's in/out
				// points are stored on the block (Olive derives them from the track
				// order), so the extension must keep the gap's in point anchored —
				// an out-anchored `set_length_and_media_out` would push the in point
				// negative.
				new_gap_length = new_gap_length + block_length(gap);
				block_set_length_and_media_in(gap, new_gap_length);
				track_ripple_remove_block(&self.track, &self.block);
				self.existing_gap_precedes =
					previous.as_ref().map(|p| same_block(gap, p)).unwrap_or(false);
			} else {
				// No gap exists to fill this space, create a new one and swap it in.
				if self.our_gap.is_none() {
					self.our_gap = Some(block_gap_create(&self.track.project));
					if let Some(gap) = &self.our_gap {
						// In-anchored at the block's construction-time in point
						// (the module stores positions on the block; the gap
						// fills the block's stored span).
						block_set_in(gap, self.block_in_point);
						block_set_length_and_media_in(gap, new_gap_length);
					}
				}
				if let Some(gap) = &self.our_gap {
					block_add_to_graph(gap, self.our_gap_entry.take());
					track_replace_block(&self.track, &self.block, gap);
				}
			}
		} else {
			// The block is at the end of the track, simply remove it.
			let preceding = block_previous(&self.block);
			track_ripple_remove_block(&self.track, &self.block);

			// Remove a preceding gap too, if there is one.
			if preceding.as_ref().map(|b| block_kind(b) == BlockKind::Gap).unwrap_or(false) {
				if let Some(gap) = &preceding {
					track_ripple_remove_block(&self.track, gap);
					self.merged_gap_entry = block_remove_from_graph(gap);
					self.existing_merged_gap = Some(gap.clone());
				}
			}
		}
	}

	/// `undo`: replace the gap back with the block.
	pub fn undo(&mut self) {
		if self.our_gap.is_some() || self.existing_gap.is_some() {
			if let Some(gap) = &self.our_gap {
				// We made this gap, simply swap it back.
				track_replace_block(&self.track, gap, &self.block);
				if self.our_gap_entry.is_none() {
					self.our_gap_entry = block_remove_from_graph(gap);
				}
			} else if let Some(gap) = &self.existing_gap {
				// We extended an existing gap; restore its original length.
				let mut original_gap_length = block_length(gap) - block_length(&self.block);

				// If we merged two gaps together, restore the second one now.
				if let Some(merged) = &self.existing_merged_gap {
					original_gap_length = original_gap_length - block_length(merged);
					track_insert_block_after(&self.track, merged, Some(gap));
					block_add_to_graph(merged, self.merged_gap_entry.take());
					self.existing_merged_gap = None;
				}

				// Restore the original block.
				if self.existing_gap_precedes {
					track_insert_block_after(&self.track, &self.block, Some(gap));
				} else {
					// The gap followed the block, so the block is restored BEFORE it.
					track_insert_block_before(&self.track, &self.block, gap);
				}

				// Restore the gap's original length (in-anchored: its in point was
				// untouched by the redo extension).
				block_set_length_and_media_in(gap, original_gap_length);
				self.existing_gap = None;
			}
		} else {
			// Both gaps null: the block was at the end of the track, so no gap
			// extension/replacement was needed. Re-append the merged gap (if any)
			// and then the block.
			if let Some(merged) = &self.existing_merged_gap {
				track_append_block(&self.track, merged);
				block_add_to_graph(merged, self.merged_gap_entry.take());
				self.existing_merged_gap = None;
			}
			track_append_block(&self.track, &self.block);
		}

		for c in self.transition_remove_commands.iter_mut().rev() {
			c.undo();
		}
	}

	fn create_remove_transition_command_if_necessary(&mut self, next: bool) {
		let relevant_block = if next {
			block_next(&self.block)
		} else {
			block_previous(&self.block)
		};

		let Some(relevant) = relevant_block else {
			return;
		};
		if block_kind(&relevant) != BlockKind::Transition {
			return;
		}
		// NOTE: the C++ checks whether the transition is connected only to this
		// block (via `oaknode_transition_get_connected_out_block`/`_in_block` +
		// `same_block`) before adding a `TransitionRemoveCommand`; that guard is
		// still not ported (the inputs are reachable through
		// `util::block_connected_input`), so a command removing the transition
		// outright is produced.
		self.transition_remove_commands
			.push(TransitionRemoveCommand::new(relevant, true));
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TrackReplaceBlockWithGapCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `BlockEnableDisableCommand` — flip a block's enabled flag
/// (timelineundogeneral.h). The old value is captured at construction.
pub struct BlockEnableDisableCommand {
	/// Block whose enabled flag changes.
	block: NodeRef,
	/// New enabled value.
	new_enabled: bool,
	/// Enabled value captured at construction, restored by `undo`.
	old_enabled: bool,
}

impl BlockEnableDisableCommand {
	/// Construct from block + new enabled value (captures old at ctor).
	///
	/// New signature (single-lib): `pub fn new(block: NodeRef, enabled: bool) -> BlockEnableDisableCommand`
	pub fn new(block: NodeRef, enabled: bool) -> Self {
		let old_enabled = block_enabled(&block);
		Self {
			block,
			new_enabled: enabled,
			old_enabled,
		}
	}

	/// `redo`: set the new enabled value.
	pub fn redo(&mut self) {
		block_set_enabled(&self.block, self.new_enabled);
	}

	/// `undo`: restore the old enabled value.
	pub fn undo(&mut self) {
		block_set_enabled(&self.block, self.old_enabled);
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for BlockEnableDisableCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TrackListInsertGaps` — insert a gap of `length` at `point` on every track
/// in a list (timelineundogeneral.h), splitting blocks as needed and extending
/// existing gaps rather than adding duplicates where possible.
pub struct TrackListInsertGaps {
	/// Track list to operate on.
	track_list: NodeRef,
	/// Insertion point.
	point: Rational,
	/// Gap length to insert.
	length: Rational,
	/// Unlocked tracks gathered at `prepare` time.
	working_tracks: Vec<NodeRef>,
	/// Existing gaps crossed by `point`, extended by `length` on `redo`.
	gaps_to_extend: Vec<NodeRef>,
	/// Gaps inserted after their `before` blocks on their tracks.
	gaps_added: Vec<AddGap>,
	/// Optional per-track split command built at `prepare` time.
	split_command: Option<BlockSplitPreservingLinksCommand>,
}

/// One gap inserted by [`TrackListInsertGaps`] on `redo` and removed on
/// `undo`; the C++ `TrackListInsertGaps::AddGap` struct.
struct AddGap {
	/// The inserted gap block.
	gap: NodeRef,
	/// Arena entry while `gap` is detached from the graph.
	entry: Option<NodeEntry>,
	/// Block the gap is inserted after (`None` = front of the track).
	before: Option<NodeRef>,
	/// Track the gap is inserted on.
	track: NodeRef,
}

impl TrackListInsertGaps {
	/// Construct from track list + point + length.
	///
	/// New signature (single-lib): `pub fn new(track_list: NodeRef, point: Rational, length: Rational) -> TrackListInsertGaps`
	pub fn new(track_list: NodeRef, point: Rational, length: Rational) -> Self {
		Self {
			track_list,
			point,
			length,
			working_tracks: Vec::new(),
			gaps_to_extend: Vec::new(),
			gaps_added: Vec::new(),
			split_command: None,
		}
	}

	/// `prepare`: build the per-track split/gap commands.
	pub fn prepare(&mut self) {
		let track_count = tracklist_track_count(&self.track_list);
		for i in 0..track_count {
			let Some(track) = tracklist_track_at(&self.track_list, i) else {
				continue;
			};
			if crate::util::track_locked(&track) {
				continue;
			}
			self.working_tracks.push(track);
		}

		let mut blocks_to_split: Vec<NodeRef> = Vec::new();
		let mut blocks_to_append_gap_to: Vec<Option<NodeRef>> = Vec::new();
		let mut tracks_to_append_gap_to: Vec<NodeRef> = Vec::new();

		for track in self.working_tracks.clone() {
			let block_count = crate::util::track_block_count(&track);
			for i in 0..block_count {
				let Some(b) = crate::util::track_block_at(&track, i) else {
					continue;
				};
				let kind = block_kind(&b);

				if kind == BlockKind::Gap
					&& block_in(&b) <= self.point
					&& block_out(&b) >= self.point
				{
					// Found a gap at the location.
					self.gaps_to_extend.push(b);
					break;
				} else if kind == BlockKind::Clip && block_out(&b) >= self.point {
					let mut append_gap = true;
					let mut before = Some(b.clone());

					if block_in(&b) == self.point {
						// The block is at the start of the track; no split needs to occur.
						before = None;
					} else if block_out(&b) > self.point {
						// The block must be split as well as having a gap appended to it.
						blocks_to_split.push(b.clone());
					} else if block_next(&b).is_none() {
						// At the end of a track, no gap needs to be added at all.
						append_gap = false;
					}

					if append_gap {
						tracks_to_append_gap_to.push(track.clone());
						blocks_to_append_gap_to.push(before);
					}
					break;
				}
			}
		}

		if !blocks_to_split.is_empty() {
			self.split_command = Some(BlockSplitPreservingLinksCommand::new(
				blocks_to_split,
				vec![self.point],
			));
		}

		for i in 0..blocks_to_append_gap_to.len() {
			let gap = block_gap_create(&self.track_list.project);
			self.gaps_added.push(AddGap {
				gap,
				entry: None,
				before: blocks_to_append_gap_to[i].clone(),
				track: tracks_to_append_gap_to[i].clone(),
			});
		}
	}

	/// `redo`: apply the gap insertions.
	pub fn redo(&mut self) {
		for gap in self.gaps_to_extend.clone() {
			block_set_length_and_media_out(&gap, block_length(&gap) + self.length);
		}

		if let Some(s) = self.split_command.as_mut() {
			s.redo();
		}

		for g in self.gaps_added.iter_mut() {
			// The gap fills [point, point + length): the `before` block now
			// ends at the split point (the split ran above), or the gap
			// starts at `point` when it goes to the front of the track.
			// Positions are stored on the block, so both ends are written
			// explicitly.
			let gap_in = g.before.as_ref().map(block_out).unwrap_or(self.point);
			block_set_in(&g.gap, gap_in);
			block_set_length_and_media_in(&g.gap, self.length);
			block_add_to_graph(&g.gap, g.entry.take());
			track_insert_block_after(&g.track, &g.gap, g.before.as_ref());
		}
	}

	/// `undo`: remove the inserted gaps.
	pub fn undo(&mut self) {
		// Remove added gaps.
		for g in self.gaps_added.iter_mut() {
			if let Some(t) = block_track(&g.gap) {
				track_ripple_remove_block(&t, &g.gap);
			}
			if g.entry.is_none() {
				g.entry = block_remove_from_graph(&g.gap);
			}
		}

		// Un-split blocks.
		if let Some(s) = self.split_command.as_mut() {
			s.undo();
		}

		// Restore the original length of the extended gaps.
		for gap in self.gaps_to_extend.clone() {
			block_set_length_and_media_out(&gap, block_length(&gap) - self.length);
		}
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TrackListInsertGaps {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}

/// `TimelineAddDefaultTransitionCommand` — add the default transition at the
/// start/end (or both) of the given clips (timelineundogeneral.h). Composes a
/// set of child commands driven by `timebase_`.
///
/// `timebase`/`lengths` mirror the C++ members used by the length
/// adjustment once a default transition config exists.
#[allow(dead_code)]
pub struct TimelineAddDefaultTransitionCommand {
	/// Clips to receive a transition.
	clips: Vec<NodeRef>,
	/// Timeline timebase.
	timebase: Rational,
	/// Child commands, executed in order on `redo` (empty: see `prepare`).
	commands: Vec<Box<dyn Command>>,
	/// Clip length bookkeeping used by the C++ length adjustment (unused:
	/// see `add_transition`).
	lengths: Vec<(NodeRef, Rational)>,
}

/// The side(s) of a clip a transition is added to (the C++
/// `CreateTransitionMode` enum).
enum CreateTransitionMode {
	/// Transition at the clip's in point.
	In,
	/// Transition at the clip's out point.
	Out,
	/// Transitions at the in point of the next clip and the out point of this
	/// one (dual).
	OutDual,
}

impl TimelineAddDefaultTransitionCommand {
	/// Construct from clips + timebase.
	///
	/// New signature (single-lib): `pub fn new(clips: Vec<NodeRef>, timebase: Rational) -> TimelineAddDefaultTransitionCommand`
	pub fn new(clips: Vec<NodeRef>, timebase: Rational) -> Self {
		Self {
			clips,
			timebase,
			commands: Vec::new(),
			lengths: Vec::new(),
		}
	}

	/// `prepare`: build the per-clip transition commands.
	pub fn prepare(&mut self) {
		let selection = self.clips.clone();
		for c in self.clips.clone() {
			let previous = block_previous(&c);
			let next = block_next(&c);

			let is_clip_in_selection = |b: &Option<NodeRef>| -> bool {
				match b {
					Some(b) => selection.iter().any(|clip| same_block(clip, b)),
					None => false,
				}
			};

			let prev_kind = previous.as_ref().map(block_kind).unwrap_or(BlockKind::Other);
			let next_kind = next.as_ref().map(block_kind).unwrap_or(BlockKind::Other);

			// Handle the in transition.
			if is_clip_in_selection(&previous) {
				// Do nothing; assume this will be handled by a dual transition from
				// that clip.
			} else if prev_kind == BlockKind::Gap || previous.is_none() {
				// Create an in transition.
				self.add_transition(&c, CreateTransitionMode::In);
			}

			// Handle the out transition.
			if is_clip_in_selection(&next) {
				self.add_transition(&c, CreateTransitionMode::OutDual);
			} else if next_kind == BlockKind::Gap || next.is_none() {
				// Create an out transition.
				self.add_transition(&c, CreateTransitionMode::Out);
			}
		}
	}

	fn add_transition(&mut self, c: &NodeRef, mode: CreateTransitionMode) {
		let _ = (c, mode);
		// NOTE: the C++ builds one child command per transition: it looks up the
		// default transition id in the config (`oak_core_config_get`), creates
		// the node (`oaknode_factory_create_from_id`), clips the neighbours'
		// lengths (`BlockResizeCommand`/`BlockResizeWithMediaInCommand`), adds the
		// node and connects it; the default-transition config key has no Rust
		// equivalent here, so no child commands are produced.
	}

	/// `redo`: redo every child command in order.
	pub fn redo(&mut self) {
		for c in self.commands.iter_mut() {
			c.redo();
		}
	}

	/// `undo`: undo every child command in reverse.
	pub fn undo(&mut self) {
		for c in self.commands.iter_mut().rev() {
			c.undo();
		}
	}

	/// Wrap as an oakundo command value.
	pub fn to_command(self) -> UndoCommand {
		box_command(self)
	}
}

impl Command for TimelineAddDefaultTransitionCommand {
	/// `Command::redo` — the inherent method takes precedence.
	fn redo(&mut self) {
		self.redo();
	}

	/// `Command::undo` — the inherent method takes precedence.
	fn undo(&mut self) {
		self.undo();
	}
}
