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

//! Direct oaknode domain operations backing the task module (single-lib
//! unification).
//!
//! The old `src/stubs.rs` stood in for the deleted oaknode / oaktimeline /
//! oakrender C ABIs with `CHandle`-typed failure paths. Every one of those
//! stubs is replaced here by real operations over `oaknode` domain types:
//! a project is `Arc<Mutex<oak_node::project::Project>>` and every node is
//! an `oak_node::id::NodeId` in that project's `oak_node::graph::Graph`.
//!
//! Undo commands follow the oaktimeline `undocommon` pattern: a command
//! struct is boxed into an `oak_undo::undocommand::UndoCommand` value
//! ([`box_command`]); `redo_now`/`undo_now` dispatch to it.

use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::videoparams::VideoParams as CommonVideoParams;
use oak_core::Rational;
use oak_node::block::{BlockCore, ClipBlockBehavior, GapBlockBehavior, TransitionBlockBehavior};
use oak_node::folder::FolderBehavior;
use oak_node::footage::{FootageBehavior, StreamInfo};
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::node::{NodeBehavior, NodeCore};
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{BlockRange, TrackBehavior, TrackListBehavior, TrackRange, TrackType};
use oak_undo::undocommand::UndoCommand;

/// A project, shared like the C++ `olive::Project` smart pointer.
pub type ProjectRef = Arc<Mutex<Project>>;

/// A reference to one node: its owning project and its id there. Replaces
/// the borrowed `CHandle` node handles of the deleted C ABI.
pub type NodeRef = (ProjectRef, NodeId);

/// `OAKNODE_TRANSITION_CROSS_DISSOLVE` (`include/node/transition.h`);
/// transition kinds other than cross-dissolve are not modelled.
pub const TRANSITION_CROSS_DISSOLVE: i32 = 0;

/// `OAKNODE_TYPE_TRANSFORM` (`include/node/factory.h`): the transform node
/// type id used by the OTIO loader.
pub const TRANSFORM_TYPE_ID: &str = "org.olivevideoeditor.Olive.transform";

/// `OAKNODE_TYPE_VOLUME` (`include/node/factory.h`): the volume node type id
/// used by the OTIO loader.
pub const VOLUME_TYPE_ID: &str = "org.olivevideoeditor.Olive.volume";

/// `OAKNODE_TYPE_SEQUENCE` (`include/node/factory.h`): the sequence type id
/// (matches `SequenceBehavior::type_id`).
pub const SEQUENCE_TYPE_ID: &str = "org.olivevideoeditor.Olive.sequence";

/// Clip texture/effect input id (oaknode `clip_input::TEXTURE_INPUT`; the
/// old `OAKNODE_SEQUENCE_TEXTURE_INPUT` value).
pub const CLIP_TEXTURE_INPUT: &str = "tex_in";

/// Transition connection inputs (oaknode `transition_input`).
pub const TRANSITION_OUT_BLOCK_INPUT: &str = "out_block_in";
/// Transition connection input (incoming side).
pub const TRANSITION_IN_BLOCK_INPUT: &str = "in_block_in";

/// Block kinds, mirroring the `OAKNODE_BLOCK_*` constants of the deleted
/// C ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
	/// Unknown block type (`OAKNODE_BLOCK_OTHER`).
	Other,
	/// Media-bearing clip (`OAKNODE_BLOCK_CLIP`).
	Clip,
	/// Empty span (`OAKNODE_BLOCK_GAP`).
	Gap,
	/// Transition (`OAKNODE_BLOCK_TRANSITION`).
	Transition,
}

/// Lock the project mutex (poisoned locks recover their guard — task code
/// has no invariants that survive a panic).
fn lock_project(project: &ProjectRef) -> MutexGuard<'_, Project> {
	project.lock().unwrap_or_else(|e| e.into_inner())
}

/// The shared block core of any block-typed node, or `None` for non-block
/// nodes / stale ids.
pub fn block_core_of(graph: &Graph, block: NodeId) -> Option<BlockCore> {
	let entry = graph.get(block)?;
	let any = entry.behavior.as_any()?;
	if let Some(b) = any.downcast_ref::<ClipBlockBehavior>() {
		return Some(b.core.clone());
	}
	if let Some(b) = any.downcast_ref::<GapBlockBehavior>() {
		return Some(b.core.clone());
	}
	if let Some(b) = any.downcast_ref::<TransitionBlockBehavior>() {
		return Some(b.core.clone());
	}
	None
}

/// Set the shared block core fields of any block-typed node (`track`
/// ownership aside); false for non-block nodes / stale ids.
fn set_block_core(graph: &mut Graph, block: NodeId, f: impl FnOnce(&mut BlockCore)) -> bool {
	let Some(entry) = graph.get_mut(block) else {
		return false;
	};
	let Some(any) = entry.behavior.as_any_mut() else {
		return false;
	};
	if let Some(b) = any.downcast_mut::<ClipBlockBehavior>() {
		f(&mut b.core);
		return true;
	}
	if let Some(b) = any.downcast_mut::<GapBlockBehavior>() {
		f(&mut b.core);
		return true;
	}
	if let Some(b) = any.downcast_mut::<TransitionBlockBehavior>() {
		f(&mut b.core);
		return true;
	}
	false
}

/// The user label of a node (C++ `Node::label`); empty for missing nodes.
pub fn node_label(project: &ProjectRef, node: NodeId) -> String {
	let guard = lock_project(project);
	match guard.graph.get(node) {
		Some(entry) => entry.core.label.clone(),
		None => String::new(),
	}
}

/// Set the user label of a node (`oaknode_node_set_label`).
pub fn set_node_label(project: &ProjectRef, node: NodeId, label: &str) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(node) {
		entry.core.label = label.to_string();
	}
}

/// The node's stable type id (`oaknode_node_get_id`; C++ `Node::id()`).
pub fn node_type_id(project: &ProjectRef, node: NodeId) -> String {
	let guard = lock_project(project);
	match guard.graph.get(node) {
		Some(entry) => entry.behavior.type_id().to_string(),
		None => String::new(),
	}
}

/// True when the node's behavior is the footage type.
pub fn node_is_footage(graph: &Graph, node: NodeId) -> bool {
	graph
		.get(node)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.is_some()
}

/// True when the node's behavior is the sequence type.
pub fn node_is_sequence(graph: &Graph, node: NodeId) -> bool {
	graph
		.get(node)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		.is_some()
}

/// Set a node's position inside a context (the C++
/// `oaknode_node_set_context_position`; `expanded` replaces the old
/// `force != 0` flag).
pub fn node_set_context_position(
	project: &ProjectRef,
	node: NodeId,
	context: NodeId,
	x: f64,
	y: f64,
	expanded: bool,
) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(node) {
		entry.core.set_context_position(context, x, y, expanded);
	}
}

/// The total length of a footage or sequence node (footage: longest video
/// stream; sequence: longest track list; `0/1` otherwise) — the domain
/// replacement for `oaknode_footage_get_video_length` /
/// `oaknode_sequence_get_length`.
pub fn node_length(project: &ProjectRef, node: NodeId) -> Rational {
	let guard = lock_project(project);
	let Some(entry) = guard.graph.get(node) else {
		return Rational::new(0, 1);
	};
	let Some(any) = entry.behavior.as_any() else {
		return Rational::new(0, 1);
	};
	if let Some(f) = any.downcast_ref::<FootageBehavior>() {
		return f.video_length();
	}
	if let Some(seq) = any.downcast_ref::<SequenceBehavior>() {
		let blocks = GraphBlockRange {
			graph: &guard.graph,
		};
		let tracks = GraphTrackRange {
			graph: &guard.graph,
		};
		let mut longest = Rational::new(0, 1);
		for list_id in seq.track_lists.iter().filter(|i| i.valid()) {
			let Some(le) = guard.graph.get(*list_id) else {
				continue;
			};
			let Some(list) = le
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<TrackListBehavior>())
			else {
				continue;
			};
			let len = list.total_length(&tracks);
			if len > longest {
				longest = len;
			}
		}
		let _ = blocks;
		if longest > Rational::new(0, 1) {
			return longest;
		}
		return seq.last_length;
	}
	Rational::new(0, 1)
}

/// Block-range access over the project graph (the adapter [`TrackBehavior`]
/// queries need; `// CPP-PARITY: src/task/...` uses the node graph directly).
pub struct GraphBlockRange<'a> {
	/// The graph the blocks live in.
	pub graph: &'a Graph,
}

impl BlockRange for GraphBlockRange<'_> {
	fn in_(&self, block: NodeId) -> Rational {
		block_core_of(self.graph, block)
			.map(|c| c.in_())
			.unwrap_or_else(|| Rational::new(0, 1))
	}

	fn out(&self, block: NodeId) -> Rational {
		block_core_of(self.graph, block)
			.map(|c| c.out())
			.unwrap_or_else(|| Rational::new(0, 1))
	}
}

/// Track-range access over the project graph (the adapter
/// [`TrackListBehavior::total_length`] needs).
pub struct GraphTrackRange<'a> {
	/// The graph the tracks live in.
	pub graph: &'a Graph,
}

impl TrackRange for GraphTrackRange<'_> {
	fn length(&self, track: NodeId) -> Rational {
		let Some(entry) = self.graph.get(track) else {
			return Rational::new(0, 1);
		};
		let Some(t) = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<TrackBehavior>())
		else {
			return Rational::new(0, 1);
		};
		t.length(&GraphBlockRange { graph: self.graph })
	}
}

/// Convert an oaknode video-params value to the oak_core `VideoParams`
/// value type the task code works with (the C ABI marshalled the same
/// fields).
fn common_from_node_video(v: oak_node::value::VideoParams) -> CommonVideoParams {
	let mut p = CommonVideoParams::new_basic(
        v.width,
        v.height,
        oak_core::ocioutils::PixelFormat::from_code(v.pixel_format),
        v.channels,
        1,
        1,
        0,
        1,
	);
	p.set_frame_rate(v.frame_rate.numerator() as i32, v.frame_rate.denominator() as i32);
	p
}

/// Convert an oak_core video-params value back to the oaknode value type
/// (the fields that survive the C ABI marshalling; `video_type`,
/// `start_time`/`duration` and color fields have no oaknode counterpart).
fn node_video_from_common(v: &CommonVideoParams) -> oak_node::value::VideoParams {
	oak_node::value::VideoParams {
		width: v.width(),
		height: v.height(),
		frame_rate: Rational::new(v.frame_rate().0 as i64, v.frame_rate().1 as i64),
		pixel_format: v.format().code(),
		channels: v.channel_count(),
		interlaced: false,
	}
}

/// The sequence's `index`th video parameter stream as the oak_core value
/// type (`oaknode_sequence_get_video_params`).
pub fn sequence_video_params(
	project: &ProjectRef,
	sequence: NodeId,
	index: usize,
) -> Option<CommonVideoParams> {
	let guard = lock_project(project);
	let entry = guard.graph.get(sequence)?;
	let seq = entry
		.behavior
		.as_any()?
		.downcast_ref::<SequenceBehavior>()?;
	seq.video_params.get(index).copied().map(common_from_node_video)
}

/// The sequence's audio output parameters `(sample_rate, channel_layout)`
/// (defaults to 48000 Hz stereo, the sequence defaults).
pub fn sequence_audio_params(project: &ProjectRef, sequence: NodeId) -> (i32, u64) {
	let guard = lock_project(project);
	let Some(entry) = guard.graph.get(sequence) else {
		return (48000, 0x3);
	};
	let Some(seq) = entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
	else {
		return (48000, 0x3);
	};
	match seq.audio_params.first() {
		Some(a) => (a.sample_rate, a.channel_layout),
		None => (48000, 0x3),
	}
}

/// Create a new footage node in `project` (optionally pre-set with a
/// filename; unprobed — call [`footage_set_filename`] to probe). `None` is
/// no longer reachable (the old null-handle allocation path); the `Option`
/// keeps the call sites' shape.
pub fn footage_create(project: &ProjectRef, filename: Option<&str>) -> Option<NodeId> {
	let mut guard = lock_project(project);
	let (core, behavior) = oak_node::footage::FootageBehavior::create();
	let id = guard.graph.add_node(core, behavior);
	if let Some(filename) = filename {
		if let Some(entry) = guard.graph.get_mut(id) {
			if let Some(f) = entry
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<FootageBehavior>())
			{
				f.filename = filename.to_string();
			}
		}
	}
	Some(id)
}

/// Set the footage cancellation flag (the C++ footage cancel atom; the
/// footage behavior owns its own atom, mirrored from the task's).
pub fn footage_set_cancelled(project: &ProjectRef, footage: NodeId, cancelled: bool) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(footage) {
		if let Some(f) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<FootageBehavior>())
		{
			f.set_cancel(cancelled);
		}
	}
}

/// Set the footage's filename and re-probe it through the oakcodec decoder
/// registry (`oaknode_footage_set_filename`); returns the post-probe
/// validity (the C++ set_filename → Probe() → is_valid chain).
pub fn footage_set_filename(project: &ProjectRef, footage: NodeId, filename: &str) -> bool {
	let mut guard = lock_project(project);
	let Some(entry) = guard.graph.get_mut(footage) else {
		return false;
	};
	let Some(f) = entry
		.behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<FootageBehavior>())
	else {
		return false;
	};
	f.filename = filename.to_string();
	f.valid = false;
	entry
		.core
		.set_standard_value("file_in", -1, oak_node::value::NodeValue::Text(filename.to_string()));
	if f.probe().is_err() {
		return false;
	}
	f.valid
}

/// Whether the footage's last probe succeeded (`oaknode_footage_is_valid`).
pub fn footage_is_valid(project: &ProjectRef, footage: NodeId) -> bool {
	let guard = lock_project(project);
	guard
		.graph
		.get(footage)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.map(|f| f.valid)
		.unwrap_or(false)
}

/// The footage's absolute filename (`oaknode_footage_filename`).
pub fn footage_filename(project: &ProjectRef, footage: NodeId) -> String {
	let guard = lock_project(project);
	guard
		.graph
		.get(footage)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.map(|f| f.filename.clone())
		.unwrap_or_default()
}

/// Total stream count of the footage (`oaknode_footage_total_stream_count`).
pub fn footage_total_stream_count(project: &ProjectRef, footage: NodeId) -> usize {
	let guard = lock_project(project);
	guard
		.graph
		.get(footage)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.map(|f| f.total_stream_count())
		.unwrap_or(0)
}

/// The footage's `index`th video parameter stream as the oak_core value
/// type (`oaknode_footage_get_video_params`).
pub fn footage_video_params(
	project: &ProjectRef,
	footage: NodeId,
	index: usize,
) -> Option<CommonVideoParams> {
	let guard = lock_project(project);
	let f = guard
		.graph
		.get(footage)?
		.behavior
		.as_any()?
		.downcast_ref::<FootageBehavior>()?;
	f.video_params(index).map(common_from_node_video)
}

/// Overwrite (or create) the footage's `index`th video stream parameters
/// (`oaknode_footage_set_video_params`).
pub fn footage_set_video_params(
	project: &ProjectRef,
	footage: NodeId,
	index: usize,
	params: &CommonVideoParams,
) {
	let mut guard = lock_project(project);
	let Some(entry) = guard.graph.get_mut(footage) else {
		return;
	};
	let Some(f) = entry
		.behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<FootageBehavior>())
	else {
		return;
	};
	let node_params = node_video_from_common(params);
	let mut seen = 0usize;
	let mut found = false;
	for s in f.streams.iter_mut().filter(|s| s.is_video) {
		if seen == index {
			s.video = Some(node_params);
			found = true;
			break;
		}
		seen += 1;
	}
	if !found {
		for _ in seen..=index {
			f.streams.push(StreamInfo {
				index: f.streams.len() as i32,
				is_video: true,
				video: None,
				audio: None,
				duration: Rational::new(0, 1),
			});
		}
		if let Some(s) = f.streams.iter_mut().filter(|s| s.is_video).nth(index) {
			s.video = Some(node_params);
		}
	}
}

/// Create a new bin folder node (`oaknode_folder_create`; `None` keeps the
/// call-site shape of the old null-handle path).
pub fn folder_create(project: &ProjectRef) -> Option<NodeId> {
	let mut guard = lock_project(project);
	let (core, behavior) = oak_node::folder::create("");
	Some(guard.graph.add_node(core, behavior))
}

/// Create a new sequence node with default parameters
/// (`oaknode_sequence_create` + `oaknode_sequence_set_default_parameters`;
/// the domain constructor applies the defaults itself).
pub fn sequence_create(project: &ProjectRef) -> Option<NodeId> {
	let mut guard = lock_project(project);
	let (core, behavior) = oak_node::sequence::SequenceBehavior::create();
	Some(guard.graph.add_node(core, behavior))
}

/// The sequence's track list for `kind`, creating it on first use (the C++
/// `Sequence::get_track_list`).
pub fn sequence_track_list(
	project: &ProjectRef,
	sequence: NodeId,
	kind: TrackType,
) -> Option<NodeId> {
	let base = match kind {
		TrackType::Video => 0usize,
		TrackType::Audio => 1usize,
		TrackType::Subtitle => 2usize,
	};
	let mut guard = lock_project(project);
	{
		let entry = guard.graph.get(sequence)?;
		let seq = entry
			.behavior
			.as_any()?
			.downcast_ref::<SequenceBehavior>()?;
		if let Some(id) = seq.track_lists.get(base).filter(|i| i.valid()) {
			return Some(*id);
		}
	}
	let list_id = guard.graph.add_node(
		NodeCore::new(),
		Box::new(TrackListBehavior::new(kind)),
	);
	if let Some(le) = guard.graph.get_mut(list_id) {
		if let Some(l) = le
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackListBehavior>())
		{
			l.sequence = Some(sequence);
			l.array_base = base as i32;
		}
	}
	if let Some(entry) = guard.graph.get_mut(sequence) {
		if let Some(seq) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			while seq.track_lists.len() <= base {
				seq.track_lists.push(NodeId::INVALID);
			}
			seq.track_lists[base] = list_id;
		}
	}
	Some(list_id)
}

/// Number of tracks on the list (`oaknode_tracklist_get_track_count`).
pub fn tracklist_track_count(project: &ProjectRef, list: NodeId) -> usize {
	let guard = lock_project(project);
	guard
		.graph
		.get(list)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TrackListBehavior>())
		.map(|l| l.tracks.len())
		.unwrap_or(0)
}

/// Track at `index` on the list (`oaknode_tracklist_get_track_at`).
pub fn tracklist_track_at(project: &ProjectRef, list: NodeId, index: usize) -> Option<NodeId> {
	let guard = lock_project(project);
	guard
		.graph
		.get(list)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackListBehavior>()?
		.track_at(index)
}

/// The track's media type (`oaknode_track_get_type`).
pub fn track_type(project: &ProjectRef, track: NodeId) -> Option<TrackType> {
	let guard = lock_project(project);
	guard
		.graph
		.get(track)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.map(|t| t.kind)
}

/// Number of blocks on the track (`oaknode_track_get_block_count`).
pub fn track_block_count(project: &ProjectRef, track: NodeId) -> usize {
	let guard = lock_project(project);
	guard
		.graph
		.get(track)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.map(|t| t.blocks.len())
		.unwrap_or(0)
}

/// Block at `index` on the track (`oaknode_track_get_block_at`).
pub fn track_block_at(project: &ProjectRef, track: NodeId, index: usize) -> Option<NodeId> {
	let guard = lock_project(project);
	guard
		.graph
		.get(track)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackBehavior>()?
		.block_at(index)
}

/// The track's total length (end of its last block; `oaknode_track_get_length`).
pub fn track_length(project: &ProjectRef, track: NodeId) -> Rational {
	let guard = lock_project(project);
	let Some(entry) = guard.graph.get(track) else {
		return Rational::new(0, 1);
	};
	let Some(t) = entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
	else {
		return Rational::new(0, 1);
	};
	t.length(&GraphBlockRange { graph: &guard.graph })
}

/// Append a block to a track and record the track ownership on the block
/// (`oaknode_track_append_block`); false for stale ids.
pub fn track_append_block(project: &ProjectRef, track: NodeId, block: NodeId) -> bool {
	let mut guard = lock_project(project);
	let _ = set_block_core(&mut guard.graph, block, |core| core.track = Some(track));
	let Some(entry) = guard.graph.get_mut(track) else {
		return false;
	};
	let Some(t) = entry
		.behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<TrackBehavior>())
	else {
		return false;
	};
	t.append_block(block);
	true
}

/// Create a block node of the given kind (`oaknode_block_clip_create` /
/// `oaknode_block_gap_create` / `oaknode_block_transition_create`; the
/// transition kind argument is ignored — only cross-dissolve is modelled).
pub fn block_create(project: &ProjectRef, kind: BlockKind) -> Option<NodeId> {
	let mut guard = lock_project(project);
	let (core, behavior): (NodeCore, Box<dyn NodeBehavior>) = match kind {
		BlockKind::Clip => oak_node::block::clip_create(),
		BlockKind::Gap => oak_node::block::gap_create(),
		BlockKind::Transition => oak_node::block::transition_create(),
		BlockKind::Other => return None,
	};
	Some(guard.graph.add_node(core, behavior))
}

/// The block's kind (`oaknode_block_get_kind`).
pub fn block_kind(project: &ProjectRef, block: NodeId) -> BlockKind {
	let guard = lock_project(project);
	let Some(entry) = guard.graph.get(block) else {
		return BlockKind::Other;
	};
	let Some(any) = entry.behavior.as_any() else {
		return BlockKind::Other;
	};
	if any.downcast_ref::<ClipBlockBehavior>().is_some() {
		BlockKind::Clip
	} else if any.downcast_ref::<GapBlockBehavior>().is_some() {
		BlockKind::Gap
	} else if any.downcast_ref::<TransitionBlockBehavior>().is_some() {
		BlockKind::Transition
	} else {
		BlockKind::Other
	}
}

/// The block's timeline in point (`oaknode_block_get_in`).
pub fn block_in(project: &ProjectRef, block: NodeId) -> Rational {
	let guard = lock_project(project);
	block_core_of(&guard.graph, block)
		.map(|c| c.in_())
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// The block's timeline length (`oaknode_block_get_length`).
pub fn block_length(project: &ProjectRef, block: NodeId) -> Rational {
	let guard = lock_project(project);
	block_core_of(&guard.graph, block)
		.map(|c| c.length())
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// The block's media in point (C++ `Block::media_in`).
pub fn block_media_in(project: &ProjectRef, block: NodeId) -> Rational {
	let guard = lock_project(project);
	block_core_of(&guard.graph, block)
		.map(|c| c.media_in)
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// Set the block's length keeping the media out anchored
/// (`oaknode_block_set_length_and_media_out`).
pub fn block_set_length_and_media_out(project: &ProjectRef, block: NodeId, n: i64, d: i64) {
	let mut guard = lock_project(project);
	let _ = set_block_core(&mut guard.graph, block, |core| {
		core.set_length_and_media_out(Rational::new(n, d))
	});
}

/// Set a clip's media in point (`oaknode_clip_set_media_in`).
pub fn clip_set_media_in(project: &ProjectRef, block: NodeId, n: i64, d: i64) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(block) {
		if let Some(c) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
		{
			c.core.media_in = Rational::new(n, d);
		}
	}
}

/// Record the footage connected to a clip (the domain model keeps the
/// clip-footage link in [`ClipBlockBehavior::footage`]).
pub fn clip_set_footage(project: &ProjectRef, block: NodeId, footage: NodeId) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(block) {
		if let Some(c) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
		{
			c.footage = Some(footage);
		}
	}
}

/// The clip footage recorded on a clip block, if any.
pub fn clip_footage(project: &ProjectRef, block: NodeId) -> Option<NodeId> {
	let guard = lock_project(project);
	guard
		.graph
		.get(block)?
		.behavior
		.as_any()?
		.downcast_ref::<ClipBlockBehavior>()?
		.footage
}

/// A transition's in offset (`oaknode_transition_get_in_offset`).
pub fn transition_in_offset(project: &ProjectRef, block: NodeId) -> Rational {
	let guard = lock_project(project);
	guard
		.graph
		.get(block)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TransitionBlockBehavior>())
		.map(|t| t.in_offset)
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// A transition's out offset (`oaknode_transition_get_out_offset`).
pub fn transition_out_offset(project: &ProjectRef, block: NodeId) -> Rational {
	let guard = lock_project(project);
	guard
		.graph
		.get(block)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<TransitionBlockBehavior>())
		.map(|t| t.out_offset)
		.unwrap_or_else(|| Rational::new(0, 1))
}

/// Set a transition's offsets and derive its length from them
/// (`oaknode_transition_set_offsets_and_length`).
pub fn transition_set_offsets_and_length(
	project: &ProjectRef,
	block: NodeId,
	in_n: i64,
	in_d: i64,
	out_n: i64,
	out_d: i64,
) {
	let mut guard = lock_project(project);
	if let Some(entry) = guard.graph.get_mut(block) {
		if let Some(t) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TransitionBlockBehavior>())
		{
			t.in_offset = Rational::new(in_n, in_d);
			t.out_offset = Rational::new(out_n, out_d);
			let length = t.in_offset + t.out_offset;
			t.core.set_length_and_media_out(length);
		}
	}
}

/// Connect `from`'s output to `to`'s `input_id` (`oaknode_node_connect`);
/// false on invalid endpoints/inputs or cycles.
pub fn node_connect(project: &ProjectRef, from: NodeId, to: NodeId, input_id: &str) -> bool {
	let mut guard = lock_project(project);
	guard.graph.connect(from, to, input_id, -1).is_ok()
}

/// Walk the node's input chain to its footage source (C++
/// `node_find_input_footage`): footage nodes answer with themselves, clip
/// blocks answer with their recorded footage, other nodes are walked
/// upstream along graph edges. Pure graph query — callers already holding
/// the project lock use this variant.
pub fn find_input_footage(graph: &Graph, node: NodeId) -> Option<NodeId> {
	if node_is_footage(graph, node) {
		return Some(node);
	}
	if let Some(clip) = graph
		.get(node)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
	{
		if let Some(f) = clip.footage {
			if graph.is_valid(f) {
				return Some(f);
			}
		}
	}
	let mut stack = vec![node];
	let mut seen = std::collections::HashSet::new();
	while let Some(n) = stack.pop() {
		if !seen.insert(n) {
			continue;
		}
		for (from, _, _) in graph.input_connections(n) {
			if node_is_footage(graph, from) {
				return Some(from);
			}
			stack.push(from);
		}
	}
	None
}

/// Locking variant of [`find_input_footage`]
/// (`oaknode_node_find_input_footage`).
pub fn node_find_input_footage(project: &ProjectRef, node: NodeId) -> Option<NodeId> {
	let guard = lock_project(project);
	find_input_footage(&guard.graph, node)
}

/// Map an `olive::core::PixelFormat` code to `oak_core::PixelFormat`
/// (unknown codes map to `Invalid`).
pub fn pixel_format_from_code(code: i32) -> oak_core::PixelFormat {
	match code {
		-1 => oak_core::PixelFormat::Invalid,
		0 => oak_core::PixelFormat::U8,
		1 => oak_core::PixelFormat::U10,
		2 => oak_core::PixelFormat::U16,
		3 => oak_core::PixelFormat::F16,
		4 => oak_core::PixelFormat::F32,
		_ => oak_core::PixelFormat::Invalid,
	}
}

// ---------------------------------------------------------------------------
// Undo commands
// ---------------------------------------------------------------------------

/// Command trait for the task-local undo commands (the oakundo command
/// trait; mirrors the oaktimeline `undocommon::Command` pattern).
pub use oak_undo::undocommand::Command as TaskCommand;

/// Box a task command into an oakundo command value.
fn box_command<T: TaskCommand + 'static>(cmd: T) -> UndoCommand {
	UndoCommand::new(cmd)
}

/// `FolderAddChildCommand` — add an item to a bin folder
/// (`oaknode_command_create_folder_add_child`).
struct FolderAddChildCommand {
	/// The destination folder.
	folder: NodeRef,
	/// The item (footage/folder/sequence) to add.
	child: NodeRef,
}

impl TaskCommand for FolderAddChildCommand {
	fn redo(&mut self) {
		let mut guard = lock_project(&self.folder.0);
		if let Some(entry) = guard.graph.get_mut(self.folder.1) {
			if let Some(f) = entry
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<FolderBehavior>())
			{
				f.add_child(self.child.1);
			}
		}
		if let Some(entry) = guard.graph.get_mut(self.child.1) {
			entry.core.bin_folder = Some(self.folder.1);
		}
	}

	fn undo(&mut self) {
		let mut guard = lock_project(&self.folder.0);
		if let Some(entry) = guard.graph.get_mut(self.folder.1) {
			if let Some(f) = entry
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<FolderBehavior>())
			{
				f.remove_child(self.child.1);
			}
		}
		if let Some(entry) = guard.graph.get_mut(self.child.1) {
			entry.core.bin_folder = None;
		}
	}
}

/// Build the undo command that adds `child` to `folder`
/// (`oaknode_command_create_folder_add_child`).
pub fn folder_add_child_command(folder: NodeRef, child: NodeRef) -> UndoCommand {
	box_command(FolderAddChildCommand { folder, child })
}

/// `RemoveNodeCommand` — detach a node from the project graph
/// (`oaknode_command_create_remove_node`). The node's entry (core +
/// behavior + generation) is kept between redo and undo; undo re-inserts
/// it into its original slot. Edges are dropped on redo and not restored
/// on undo (the C++ command's connections survive; this task module only
/// removes unconnected nodes).
struct RemoveNodeCommand {
	/// The owning project.
	project: ProjectRef,
	/// The node to remove.
	node: NodeId,
	/// The detached entry, captured at redo.
	entry: Option<oak_node::graph::NodeEntry>,
	/// The node's bin folder at redo time.
	folder: Option<NodeId>,
	/// The node's child index in that folder.
	folder_index: Option<usize>,
}

impl TaskCommand for RemoveNodeCommand {
	fn redo(&mut self) {
		let mut guard = lock_project(&self.project);
		let bin_folder = guard
			.graph
			.get(self.node)
			.and_then(|e| e.core.bin_folder);
		let index = bin_folder.and_then(|fid| {
			guard
				.graph
				.get(fid)
				.and_then(|e| e.behavior.as_any())
				.and_then(|a| a.downcast_ref::<FolderBehavior>())
				.and_then(|f| f.index_of_child(self.node))
		});
		if let (Some(fid), Some(_)) = (bin_folder, index) {
			if let Some(entry) = guard.graph.get_mut(fid) {
				if let Some(f) = entry
					.behavior
					.as_any_mut()
					.and_then(|a| a.downcast_mut::<FolderBehavior>())
				{
					f.remove_child(self.node);
				}
			}
		}
		let entry = guard.graph.take_node(self.node);
		self.entry = entry;
		self.folder = bin_folder;
		self.folder_index = index;
	}

	fn undo(&mut self) {
		let Some(entry) = self.entry.take() else {
			return;
		};
		let mut guard = lock_project(&self.project);
		guard.graph.add_entry(entry, self.node);
		if let (Some(fid), Some(index)) = (self.folder, self.folder_index) {
			if let Some(entry) = guard.graph.get_mut(fid) {
				if let Some(f) = entry
					.behavior
					.as_any_mut()
					.and_then(|a| a.downcast_mut::<FolderBehavior>())
				{
					let index = index.min(f.children.len());
					f.children.insert(index, self.node);
				}
			}
			if let Some(entry) = guard.graph.get_mut(self.node) {
				entry.core.bin_folder = Some(fid);
			}
		}
	}
}

/// Build the undo command that removes `node` from its project
/// (`oaknode_command_create_remove_node`).
pub fn remove_node_command(project: ProjectRef, node: NodeId) -> UndoCommand {
	box_command(RemoveNodeCommand {
		project,
		node,
		entry: None,
		folder: None,
		folder_index: None,
	})
}

/// `TimelineAddTrackCommand` — append a track to a track list
/// (`TimelineAddTrackCommand::run_immediately` semantics).
///
/// Domain replacement for oaktimeline's `TimelineAddTrackCommand`: that
/// crate is mid-migration to `Arc<Mutex<Project>>` + `NodeId` and still
/// only exposes `CHandle`-based constructors (whose internals are local
/// stubs), so oaktask cannot wire domain objects through it yet. This
/// command performs the same work directly over the oaknode domain model;
/// once oaktimeline's domain constructor lands, oaktask should switch to
/// it. The C++ merge-node/automerge branch is not modelled (automerge is
/// disabled by default and the sequence-input connection is implicit in
/// the Rust track-list model).
struct TimelineAddTrackCommand {
	/// The owning project.
	project: ProjectRef,
	/// The target track list.
	list: NodeId,
	/// The created track (set by redo).
	track: Option<NodeId>,
	/// The list's media type (copied at redo).
	kind: TrackType,
}

impl TaskCommand for TimelineAddTrackCommand {
	fn redo(&mut self) {
		let mut guard = lock_project(&self.project);
		let track_index = {
			let Some(entry) = guard.graph.get(self.list) else {
				return;
			};
			let Some(list) = entry
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<TrackListBehavior>())
			else {
				return;
			};
			self.kind = list.kind;
			list.tracks.len() as i32
		};
		let track_id = guard.graph.add_node(
			NodeCore::new(),
			Box::new(TrackBehavior::new(self.kind)),
		);
		if let Some(te) = guard.graph.get_mut(track_id) {
			if let Some(t) = te
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<TrackBehavior>())
			{
				t.index = track_index;
				t.track_list = Some(self.list);
			}
		}
		if let Some(le) = guard.graph.get_mut(self.list) {
			if let Some(l) = le
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<TrackListBehavior>())
			{
				l.tracks.push(track_id);
			}
		}
		self.track = Some(track_id);
	}

	fn undo(&mut self) {
		let Some(track_id) = self.track.take() else {
			return;
		};
		let mut guard = lock_project(&self.project);
		if let Some(le) = guard.graph.get_mut(self.list) {
			if let Some(l) = le
				.behavior
				.as_any_mut()
				.and_then(|a| a.downcast_mut::<TrackListBehavior>())
			{
				l.tracks.retain(|t| *t != track_id);
			}
		}
		guard.graph.remove_node(track_id);
	}
}

/// Build the undo command that appends a track to `list`
/// (`oaktimeline_add_track_command`; see [`TimelineAddTrackCommand`] for
/// the oaktimeline-migration note).
pub fn add_track_command(project: ProjectRef, list: NodeId) -> UndoCommand {
	box_command(TimelineAddTrackCommand {
		project,
		list,
		track: None,
		kind: TrackType::Video,
	})
}
