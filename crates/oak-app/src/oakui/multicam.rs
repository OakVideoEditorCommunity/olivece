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

//! App-side multicam resolution: the bridge between the
//! [`oak_timeline::multicam`](oak_timeline::multicam) commands and the
//! UI's detection / menu needs.
//!
//! The C++ `MulticamWidget` detects a multicam by asking the viewer, then
//! walks the clip's texture chain for a `MultiCamNode` (`viewer.cpp`'s
//! `detect_multicam_node`). The Rust engine mirrors that here:
//!
//! * [`clip_connected_sequence`] is the C++ `ClipBlock::connected_viewer()`
//!   (a `ViewerOutput` = sequence feeding the clip's `buffer_in`), the
//!   timeline menu's enable condition;
//! * [`multicam_state_for_clip`] resolves a clip to its multicam node, the
//!   source sequence, the source count and the current source — the panel's
//!   grid state;
//! * [`clip_at_playhead_with_multicam`] is the third detection level (the
//!   clip under the playhead on the video tracks), used when nothing is
//!   selected.

use oak_core::Rational;
use oak_node::block::clip_input;
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::nodes::multicamnode::{SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT};
use oak_node::sequence::SequenceBehavior;
use oak_node::track::TrackType;

use super::engine::{MulticamState, WizardFootage};
use super::graphops::{
	self, lock, sequence_behavior, track_list_of, ProjectRef,
};

/// Whether `id` names a sequence node (C++ `dynamic_cast<Sequence*>` /
/// the facade's `oakengine_node_is_sequence`).
pub fn is_sequence(g: &Graph, id: NodeId) -> bool {
	g.get(id)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		.is_some()
}

/// The node feeding `node.input[input][element]`, if any.
fn connected_output(g: &Graph, node: NodeId, input: &str, element: i32) -> Option<NodeId> {
	g.connected_output(node, input, element)
}

/// Whether the clip's texture chain feeds directly from a multicam node —
/// the timeline overlay's flag (`true` for the wizard host clip and the
/// dropped multicam source clip; both get the accented treatment).
pub fn clip_is_multicam(g: &Graph, clip: NodeId) -> bool {
	connected_output(g, clip, clip_input::TEXTURE_INPUT, -1)
		.map(|source| {
			g.get(source)
				.map(|e| e.behavior.type_id() == "org.olivevideoeditor.Olive.multicam")
				.unwrap_or(false)
		})
		.unwrap_or(false)
}

/// The C++ `find_input_node_internal` walk: check `node`'s input
/// connections for a match, recursing into each source. Collects the first
/// sequence found (stopping at `maximum` matches, `0` = unlimited).
fn find_sequence_internal(
	g: &Graph,
	node: NodeId,
	maximum: usize,
	list: &mut Vec<NodeId>,
) {
	for (from, _input, _element) in g.input_connections(node) {
		if is_sequence(g, from) {
			list.push(from);
			if maximum != 0 && list.len() == maximum {
				return;
			}
		}
		find_sequence_internal(g, from, maximum, list);
		if maximum != 0 && list.len() == maximum {
			return;
		}
	}
}

/// The C++ `find_input_nodes_connected_to_input<ViewerOutput>(input, 1)`:
/// the first sequence feeding the clip's texture input (`buffer_in`), depth
/// 1 and then along the dependency chain — the clip's "connected viewer".
/// This is the timeline Multi-Cam menu's enable condition (a clip whose
/// source is a sequence can host a multicam).
pub fn clip_connected_sequence(g: &Graph, clip: NodeId) -> Option<NodeId> {
	let source = connected_output(g, clip, clip_input::TEXTURE_INPUT, -1)?;
	if is_sequence(g, source) {
		return Some(source);
	}
	let mut list = Vec::new();
	find_sequence_internal(g, source, 1, &mut list);
	list.first().copied()
}

/// The sequence a multicam node pulls its angles from (the `sequence_in`
/// edge target).
pub fn multicam_sequence(p: &ProjectRef, mc: NodeId) -> Option<NodeId> {
	let g = lock(p);
	connected_output(&g.graph, mc, SEQUENCE_INPUT, -1)
}

/// The source sequence's track for angle `source` (the track whose clip
/// makes up that angle), or `None` when the source is out of range.
pub fn multicam_source_track(p: &ProjectRef, mc: NodeId, source: i32) -> Option<NodeId> {
	let g = lock(p);
	let seq = connected_output(&g.graph, mc, SEQUENCE_INPUT, -1)?;
	let kind = multicam_sequence_type(&g.graph, mc);
	graphops::track_ids(&g.graph, seq, kind).get(source as usize).copied()
}

/// The track-type selector of a multicam node (`sequence_type_in`):
/// [`TrackType::Video`] (0) or [`TrackType::Audio`] (1); defaults to video
/// when the node is stale.
pub fn multicam_sequence_type(g: &Graph, mc: NodeId) -> TrackType {
	g.get(mc)
		.map(|e| {
			let v = e.core.standard_value(SEQUENCE_TYPE_INPUT, -1).to_double() as i32;
			TrackType::from_c(v).unwrap_or(TrackType::Video)
		})
		.unwrap_or(TrackType::Video)
}

/// The number of angle sources of a multicam node: the connected source
/// sequence's track count of the node's `sequence_type_in` kind (the C++
/// `get_source_count()` resolves the same way when a sequence is set).
pub fn multicam_source_count(p: &ProjectRef, mc: NodeId) -> i32 {
	let g = lock(p);
	let Some(seq) = connected_output(&g.graph, mc, SEQUENCE_INPUT, -1) else {
		return 0;
	};
	let kind = multicam_sequence_type(&g.graph, mc);
	graphops::track_list_of(&g.graph, seq, kind)
		.and_then(|list| graphops::track_list_behavior(&g.graph, list))
		.map(|l| l.tracks.len() as i32)
		.unwrap_or(0)
}

/// The currently selected source of a multicam node (`current_in` as int),
/// `-1` when stale.
pub fn multicam_current_source(p: &ProjectRef, mc: NodeId) -> i32 {
	let g = lock(p);
	g.graph
		.get(mc)
		.map(|e| e.core.standard_value(oak_node::nodes::multicamnode::CURRENT_INPUT, -1).to_double() as i32)
		.unwrap_or(-1)
}

/// Resolve a clip to the full multicam state the panel displays: its
/// multicam node, the source sequence, the source count and the current
/// source. `None` when the clip has no multicam or the multicam has no
/// connected sequence.
pub fn multicam_state_for_clip(p: &ProjectRef, clip: NodeId) -> Option<MulticamState> {
	let clip_ref = oak_timeline::util::NodeRef::new(p.clone(), clip);
	let mc = oak_timeline::multicam::clip_find_multicam(&clip_ref)?;
	let sequence_id = multicam_sequence(p, mc.id)?;
	let source_count = multicam_source_count(p, mc.id);
	Some(MulticamState {
		sequence_id: sequence_id.identity(),
		node_id: mc.id.identity(),
		clip_id: clip.identity(),
		source_count,
		current_source: multicam_current_source(p, mc.id),
	})
}

/// The clip covering `time` on the sequence's video tracks whose texture
/// chain contains a multicam node — the C++ detection's third level (the
/// playhead's nearest clip). `None` when no such clip exists.
pub fn clip_at_playhead_with_multicam(p: &ProjectRef, seq: NodeId, time: Rational) -> Option<NodeId> {
	// Collect the candidates under the lock, then re-lock per clip via
	// `clip_find_multicam` (which takes the project lock itself) — holding
	// the guard across it would deadlock.
	let candidates: Vec<NodeId> = {
		let g = lock(p);
		let mut out = Vec::new();
		for track_id in graphops::track_ids(&g.graph, seq, TrackType::Video) {
			let Some(track) = graphops::track_behavior(&g.graph, track_id) else {
				continue;
			};
			for &block_id in &track.blocks {
				let Some(clip) = graphops::clip_behavior(&g.graph, block_id) else {
					continue;
				};
				if time < clip.core.in_() || time >= clip.core.out() {
					continue;
				}
				out.push(block_id);
			}
		}
		out
	};
	candidates.into_iter().find(|block_id| {
		let clip_ref = oak_timeline::util::NodeRef::new(p.clone(), *block_id);
		oak_timeline::multicam::clip_find_multicam(&clip_ref).is_some()
	})
}

/// The footage entries the multicam wizard offers: root-level footage
/// nodes (skipping folders). Display labels are the node labels (file
/// names); source timecode and duration come from the probed behavior.
pub fn wizard_footage(p: &ProjectRef) -> Vec<WizardFootage> {
	// The root identity first, with the guard dropped BEFORE children
	// runs: `children` takes the project lock again internally (through
	// `find_by_identity`), and a still-live guard from the argument
	// expression re-enters the non-recursive std Mutex — the wizard
	// freeze regression.
	let root_id = graphops::lock(p).root.identity();
	let entries = super::projectbrowser::children(p, root_id);
	let mut out = Vec::new();
	for entry in entries {
		if entry.is_dir {
			continue;
		}
		let Some(node) = graphops::id_of(entry.id) else {
			continue;
		};
		let g = graphops::lock(p);
		let Some(f) = graphops::footage_behavior(&g.graph, node) else {
			continue;
		};
		if f.filename.is_empty() {
			continue;
		}
		let name = graphops::node_label(&g.graph, node);
		let source_timecode = f.has_source_start_time.then(|| {
			let t = f.source_start_time;
			(t.numerator() as f64 / t.denominator().max(1) as f64).round() as i64
		});
		let duration_s = {
			let d = f.duration();
			(d.denominator() != 0).then(|| d.numerator() as f64 / d.denominator() as f64)
		};
		let has_audio = Some(f.audio_stream_count() > 0);
		out.push(WizardFootage {
			id: entry.id,
			name: name.into(),
			source_timecode,
			duration_s,
			has_audio,
		});
	}
	out
}

/// Builds the multicam sequence graph for the wizard (non-undoable —
/// the caller pushes the whole construction as one undo entry).
///
/// * a new sequence `source` with one VIDEO track per selected footage
///   (top track = angle 0); the clip on each track starts at
///   `offsets[i]` seconds (media-in 0 — the sync offset lands on the
///   timeline, so switching angles aligns the clips);
/// * a multicam node wired `sequence_in` = the source sequence and the
///   source's video track list as its `sources_in`.
///
/// The timeline clip that FEEDS the multicam output is the caller's job
/// (the app places it on the current sequence at the playhead and
/// pushes everything as one undo entry). Returns
/// `(source_sequence, multicam_node)`.
pub fn build_multicam_sequence(
	p: &ProjectRef,
	selected: &[WizardFootage],
	offsets: &[f64],
	name: &str,
) -> Result<(NodeId, NodeId), String> {
	if selected.is_empty() {
		return Err("no angles selected".to_string());
	}
	if selected.len() != offsets.len() {
		return Err("angle/offset count mismatch".to_string());
	}

	let seq: Option<NodeId>;
	let mc: Option<NodeId>;
	{
		let mut g = lock(p);
		let (_score, sbehavior) = oak_node::sequence::SequenceBehavior::create();
		let mut core = oak_node::node::NodeCore::new();
		core.label = name.to_string();
		let seq_id = g.graph.add_node(core, sbehavior);
		seq = Some(seq_id);

		// The video track list (the vec-shape course: `track_lists` is a
		// Vec<NodeId> on the sequence, `tracks` a Vec on the list — not
		// graph edges; mirror `find_or_create_track_list`).
		let list = if let Some(list) = track_list_of(&g.graph, seq_id, TrackType::Video) {
			list
		} else {
			let (lcore, lbehavior) = oak_node::track::TrackListBehavior::create();
			let mut behavior = lbehavior;
			if let Some(a) = behavior.as_any_mut() {
				if let Some(list) = a.downcast_mut::<oak_node::track::TrackListBehavior>() {
					list.kind = TrackType::Video;
					list.array_base =
						sequence_behavior(&g.graph, seq_id).map(|s| s.track_lists.len()).unwrap_or(0)
							as i32;
				}
			}
			let list = g.graph.add_node(lcore, behavior);
			if let Some(s) = g
				.graph
				.get_mut(seq_id)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<SequenceBehavior>())
			{
				s.track_lists.push(list);
			}
			list
		};
		// One video track per angle, pushed onto the list's `tracks`.
		let mut track_ids = Vec::new();
		for _ in 0..selected.len() {
			let (tcore, tbehavior) = oak_node::track::TrackBehavior::create();
			let track = g.graph.add_node(tcore, tbehavior);
			if let Some(l) = g
				.graph
				.get_mut(list)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<oak_node::track::TrackListBehavior>())
			{
				l.tracks.push(track);
			}
			track_ids.push(track);
		}

		let tb_den = 25i64; // 25 fps default wizard rate
		// The source's AUDIO track list (built only when one or more
		// angles carries audio — the AFV half of the multicam: each
		// angle's audio clip sits on its own track, and the host audio
		// clip re-points at the CURRENT source's audio on switch).
		let mut audio_list: Option<NodeId> = None;
		let mut audio_track_ids: Vec<NodeId> = Vec::new();
		if selected.iter().any(|e| e.has_audio.unwrap_or(false)) {
			audio_list = Some(if let Some(list) =
				track_list_of(&g.graph, seq_id, TrackType::Audio)
			{
				list
			} else {
				let (lcore, lbehavior) = oak_node::track::TrackListBehavior::create();
				let mut behavior = lbehavior;
				if let Some(a) = behavior.as_any_mut() {
					if let Some(list) = a.downcast_mut::<oak_node::track::TrackListBehavior>() {
						list.kind = TrackType::Audio;
						list.array_base =
							sequence_behavior(&g.graph, seq_id).map(|s| s.track_lists.len()).unwrap_or(0)
								as i32;
					}
				}
				let list = g.graph.add_node(lcore, behavior);
				if let Some(s) = g
					.graph
					.get_mut(seq_id)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<SequenceBehavior>())
				{
					s.track_lists.push(list);
				}
				list
			});
			let list = audio_list.unwrap();
			for _ in 0..selected.len() {
				let (tcore, tbehavior) = oak_node::track::TrackBehavior::create();
				let track = g.graph.add_node(tcore, tbehavior);
				if let Some(l) = g
					.graph
					.get_mut(list)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<oak_node::track::TrackListBehavior>())
				{
					l.tracks.push(track);
				}
				audio_track_ids.push(track);
			}
		}

		let mut angle_clip_ids: Vec<NodeId> = Vec::new();
		for (i, entry) in selected.iter().enumerate() {
			let footage = graphops::id_of(entry.id)
				.ok_or_else(|| format!("angle {i} is not a footage node"))?;
			if graphops::footage_behavior(&g.graph, footage).is_none() {
				return Err(format!("angle {i} is not a footage node"));
			}
			let offset_ticks = ((offsets[i].max(0.0)) * tb_den as f64).round() as i64;
			let length_ticks = ((entry.duration_s.unwrap_or(10.0) * tb_den as f64).round() as i64)
				.max(1);
			let in_r = Rational::new(offset_ticks, tb_den);
			let out_r = Rational::new(offset_ticks + length_ticks, tb_den);

			let (ccore, cbehavior) = oak_node::block::clip_create();			let clip = g.graph.add_node(ccore, cbehavior);
			if let Some(clip_behavior) = g
				.graph
				.get_mut(clip)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<oak_node::block::ClipBlockBehavior>())
			{
				clip_behavior.core.range = oak_core::TimeRange::new(in_r, out_r);
			}
			// The clip is owned by its track's `blocks` vec (not an edge).
			if let Some(track) = track_ids.get(i) {
				let idx = &mut g
					.graph
					.get_mut(*track)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<oak_node::track::TrackBehavior>());
				if let Some(t) = idx {
					t.index = i as i32;
				}
				if let Some(t) = g
					.graph
					.get_mut(*track)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<oak_node::track::TrackBehavior>())
				{
					t.blocks.push(clip);
				}
			}
			// The footage feeds the clip through the texture-input EDGE.
			g.graph
				.connect(
					footage,
					clip,
					oak_node::block::clip_input::TEXTURE_INPUT,
					-1,
				)
				.map_err(|e| format!("connect footage to clip: {e:?}"))?;

			// AFV: the angle's audio clip on its own source audio track —
			// same range as the video angle (the sync offset aligns them),
			// fed from the footage's audio stream.
			if let (Some(_audio_list), Some(audio_track)) = (audio_list, audio_track_ids.get(i)) {
				if entry.has_audio.unwrap_or(false) {
					let (acore, abehavior) = oak_node::block::clip_create();
					let aclip = g.graph.add_node(acore, abehavior);
					if let Some(clip_behavior) = g
						.graph
						.get_mut(aclip)
						.and_then(|e| e.behavior.as_any_mut())
						.and_then(|a| a.downcast_mut::<oak_node::block::ClipBlockBehavior>())
					{
						clip_behavior.core.range = oak_core::TimeRange::new(in_r, out_r);
					}
					if let Some(t) = g
						.graph
						.get_mut(*audio_track)
						.and_then(|e| e.behavior.as_any_mut())
						.and_then(|a| a.downcast_mut::<oak_node::track::TrackBehavior>())
					{
						t.blocks.push(aclip);
					}
					g.graph
						.connect(
							footage,
							aclip,
							oak_node::block::clip_input::TEXTURE_INPUT,
							-1,
						)
						.map_err(|e| format!("connect footage to audio clip: {e:?}"))?;
					// The angle's audio + video clips are linked (grouped
					// edits stay together).
					if let Some(a) = g.graph.get_mut(aclip) {
						a.core.links.push(clip);
					}
					if let Some(v) = g.graph.get_mut(clip) {
						v.core.links.push(aclip);
					}
				}
			}
			angle_clip_ids.push(clip);
			let _ = audio_list;
		}

		let (mcore, mbehavior) = oak_node::nodes::multicamnode::create();
		let mc_id = g.graph.add_node(mcore, mbehavior);
		g.graph
			.connect(
				seq_id,
				mc_id,
				oak_node::nodes::multicamnode::SEQUENCE_INPUT,
				-1,
			)
			.map_err(|e| format!("connect multicam sequence: {e:?}"))?;
		// The multicam's source array input picks the CURRENT source's
		// video track clip: one edge per angle, at the element index (the
		// C++ shape — `sources_in[element]` is the angle's texture, and
		// the traverser's `active_elements_at_time` retains ONLY the
		// current source's element, which multi's `value()` forwards). The
		// sequence + track-list connections alone cannot feed a texture
		// (see the "value() always read None" debug report).
		for (i, clip) in angle_clip_ids.iter().enumerate() {
			// Grow the array slot so the element index is addressable
			// (C++ `MakeArraySlot`; `source_count` reads the array size).
			g.graph
				.input_array_insert(mc_id, oak_node::nodes::multicamnode::SOURCES_INPUT, i as i32)
				.map_err(|e| format!("multicam array slot {i}: {e:?}"))?;
			g.graph
				.connect(
					*clip,
					mc_id,
					oak_node::nodes::multicamnode::SOURCES_INPUT,
					i as i32,
				)
				.map_err(|e| format!("connect multicam source {i}: {e:?}"))?;
		}
		mc = Some(mc_id);
	}
	let (seq, mc) = (seq.ok_or("sequence")?, mc.ok_or("multicam")?);
	Ok((seq, mc))
}

/// Normalized cross-correlation over two windowed peak envelopes: for
/// each candidate, slide it against the reference over a ±5 s window in
/// 0.1 s steps and pick the shift with the best score. Pure wrapper
/// over [`oak_audio::waveformsync`]-style correlation, kept testable
/// without audio decoding.
pub fn estimate_wizard_offsets(
	reference: &[f64],
	candidates: &[Vec<f64>],
) -> Vec<f64> {
	const WINDOW_S: f64 = 0.1;
	let mut out = Vec::with_capacity(candidates.len());
	for candidate in candidates {
		let (offset_s, _confidence) = best_correlation_offset(reference, candidate, WINDOW_S);
		out.push(offset_s);
	}
	out
}

/// Raw sliding correlation: the candidate shifted by `offset_s * 10`
/// windows (integer windows) against the reference. Returns
/// `(offset_seconds, score)`.
fn best_correlation_offset(reference: &[f64], candidate: &[f64], window_s: f64) -> (f64, f64) {
	if reference.len() < 2 || candidate.len() < 2 {
		return (0.0, 0.0);
	}
	let mut best = (0.0f64, f64::NEG_INFINITY);
	let max_shift = (5.0 / window_s).round() as i64;
	for shift in -max_shift..=max_shift {
		let mut dot = 0.0;
		let mut ref_norm = 0.0;
		let mut cand_norm = 0.0;
		let mut count = 0usize;
		let ref_slice = reference;
		// `j = i + shift` (the candidate sample aligning to reference i).
		for i in 0..ref_slice.len() {
			let j = i as i64 + shift;
			if j < 0 {
				continue;
			}
			let Some(&c) = candidate.get(j as usize) else {
				continue;
			};
			let r = ref_slice[i];
			dot += r * c;
			ref_norm += r * r;
			cand_norm += c * c;
			count += 1;
		}
		if count < 2 {
			continue;
		}
		let denom = (ref_norm * cand_norm).sqrt();
		let score = if denom > 1e-9 { dot / denom } else { 0.0 };
		if score > best.1 {
			best = (-(shift as f64) * window_s, score);
		}
	}
	best
}

#[cfg(test)]
mod tests {
	use super::*;
	use oak_node::block::ClipBlockBehavior;
	use oak_node::node::NodeCore;
	use oak_node::project::Project;
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackListBehavior};
	use std::sync::{Arc, Mutex};

	use oak_timeline::util::{
		block_clip_create, block_in, track_append_block, NodeRef,
	};

	/// Project fixture: a sequence owning one video track list with one
	/// video track.
	struct Fixture {
		project: Arc<Mutex<oak_node::project::Project>>,
		seq: NodeRef,
		track: NodeRef,
	}

	fn fixture() -> Fixture {
		let project = Project::new();
		let (seq_id, _list_id, track_id) = {
			let mut p = project.lock().unwrap();
			let (core, behavior) = SequenceBehavior::create();
			let seq_id = p.graph.add_node(core, behavior);
			let (core, behavior) = TrackListBehavior::create();
			let list_id = p.graph.add_node(core, behavior);
			let (core, behavior) = (NodeCore::new(), Box::new(TrackBehavior::new(TrackType::Video)));
			let track_id = p.graph.add_node(core, behavior);
			{
				let seq = p
					.graph
					.get_mut(seq_id)
					.unwrap()
					.behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<SequenceBehavior>()
					.unwrap();
				seq.track_lists.push(list_id);
			}
			let list = p.graph.get_mut(list_id).unwrap();
			let l = list
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackListBehavior>()
				.unwrap();
			l.sequence = Some(seq_id);
			l.tracks.push(track_id);
			let track = p.graph.get_mut(track_id).unwrap();
			let t = track
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.unwrap();
			t.kind = TrackType::Video;
			t.track_list = Some(list_id);
			(seq_id, list_id, track_id)
		};
		let _ = _list_id;
		Fixture {
			project: project.clone(),
			seq: NodeRef::new(project.clone(), seq_id),
			track: NodeRef::new(project, track_id),
		}
	}

	/// A clip spanning `[0, 50)` on the fixture's video track.
	fn add_clip(fx: &Fixture) -> NodeRef {
		let clip = block_clip_create(&fx.project);
		{
			let mut p = fx.project.lock().unwrap();
			let c = p
				.graph
				.get_mut(clip.id)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<ClipBlockBehavior>()
				.unwrap();
			c.core.range = oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(50, 1));
		}
		track_append_block(&fx.track, &clip);
		clip
	}

	/// A plain footage-fed clip has no connected sequence (the C++ viewer
	/// check); a sequence-fed clip resolves its source.
	#[test]
	fn connected_sequence_resolves_the_source() {
		let fx = fixture();
		let clip = add_clip(&fx);
		let g = lock(&fx.project);
		// No source at all: no connected sequence.
		assert!(clip_connected_sequence(&g.graph, clip.id).is_none());
		drop(g);

		// Feed the clip from a sequence (a nested-sequence clip).
		{
			let mut p = fx.project.lock().unwrap();
			p.graph
				.connect(fx.seq.id, clip.id, clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}
		let g = lock(&fx.project);
		assert_eq!(clip_connected_sequence(&g.graph, clip.id), Some(fx.seq.id));
	}

	/// `multicam_state_for_clip` resolves the multicam node, its source
	/// sequence and the source count; `None` without a multicam.
	#[test]
	fn multicam_state_resolves_node_sequence_and_count() {
		let fx = fixture();
		let clip = add_clip(&fx);
		{
			let mut p = fx.project.lock().unwrap();
			p.graph
				.connect(fx.seq.id, clip.id, clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}
		// No multicam yet.
		assert!(multicam_state_for_clip(&fx.project, clip.id).is_none());

		// Enable multicam through the real command, then resolve.
		let mut cmd = oak_timeline::multicam::MultiCamEnableCommand::new(
			vec![clip.clone()],
			fx.seq.clone(),
		);
		cmd.redo();
		let state = multicam_state_for_clip(&fx.project, clip.id).expect("multicam enabled");
		assert_eq!(state.sequence_id, fx.seq.id.identity());
		assert_eq!(state.source_count, 1, "one video track = one source");
		assert_eq!(state.current_source, 0);

		// The connected sequence still resolves through the multicam.
		let mc = oak_timeline::multicam::clip_find_multicam(&clip).unwrap();
		assert_eq!(multicam_sequence(&fx.project, mc.id), Some(fx.seq.id));
	}

	/// `clip_at_playhead_with_multicam` finds the clip under the playhead on
	/// the video tracks; a non-multicam clip under the playhead is skipped.
	#[test]
	fn playhead_clip_detection_prefers_multicam_clips() {
		let fx = fixture();
		let plain = add_clip(&fx);
		// A second track with a multicam-enabled clip.
		let (core, behavior) = (NodeCore::new(), Box::new(TrackBehavior::new(TrackType::Video)));
		let track2 = {
			let mut p = fx.project.lock().unwrap();
			let id = p.graph.add_node(core, behavior);
			let list = {
				let seq = p
					.graph
					.get(fx.seq.id)
					.unwrap()
					.behavior
					.as_any()
					.unwrap()
					.downcast_ref::<SequenceBehavior>()
					.unwrap();
				seq.track_lists[0]
			};
			let l = p
				.graph
				.get_mut(list)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackListBehavior>()
				.unwrap();
			l.tracks.push(id);
			let t = p
				.graph
				.get_mut(id)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.unwrap();
			t.kind = TrackType::Video;
			t.track_list = Some(list);
			id
		};
		let track2 = NodeRef::new(fx.project.clone(), track2);
		let mc_clip = block_clip_create(&fx.project);
		{
			let mut p = fx.project.lock().unwrap();
			let c = p
				.graph
				.get_mut(mc_clip.id)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<ClipBlockBehavior>()
				.unwrap();
			c.core.range = oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(50, 1));
		}
		track_append_block(&track2, &mc_clip);
		{
			let mut p = fx.project.lock().unwrap();
			p.graph
				.connect(fx.seq.id, mc_clip.id, clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}
		let mut cmd = oak_timeline::multicam::MultiCamEnableCommand::new(
			vec![mc_clip.clone()],
			fx.seq.clone(),
		);
		cmd.redo();

		// At frame 25 the multicam clip (topmost video track) wins over the
		// plain clip below it.
		let found = clip_at_playhead_with_multicam(&fx.project, fx.seq.id, Rational::new(25, 1));
		assert_eq!(found, Some(mc_clip.id));
		let _ = plain;
		assert_eq!(block_in(&mc_clip), Rational::new(0, 1));
	}

	/// `estimate_wizard_offsets` recovers each candidate's lag against the
	/// reference: a noise envelope shifted by +2 s must come back as +2 s
	/// (the candidate's clip then starts 2 s later on the timeline).
	#[test]
	fn wizard_offsets_recover_known_lags() {
		// Deterministic pseudo-random envelope (positive vs. silence).
		let window_s = 0.1;
		let n = (20.0 / window_s) as usize;
		let mut state = 0x2545_F491_4F6C_DD1Du64;
		let reference: Vec<f64> = (0..n)
			.map(|_| {
				state = state
					.wrapping_mul(6364136223846793005)
					.wrapping_add(1442695040888963407);
				let v = (state >> 33) as f64 / (1u64 << 31) as f64;
				if v < 0.5 {
					1.0
				} else {
					0.0
				}
			})
			.collect();

		// candidate[i] = reference[i + k] (content runs k windows ahead,
		// e.g. the candidate started k windows earlier in real time).
		let shift_of = |k: isize| {
			let mut s = state;
			let mut shifted: Vec<f64> = (0..n)
				.map(|_| {
					s = s
						.wrapping_mul(6364136223846793005)
						.wrapping_add(1442695040888963407);
					((((s >> 33) as f64) / ((1u64 << 31) as f64)) < 0.5) as u8 as f64
				})
				.collect();
			for i in 0..n {
				let src = i as isize + k;
				shifted[i] = if (0..n as isize).contains(&src) {
					reference[src as usize]
				} else {
					shifted[i]
				};
			}
			shifted
		};
		let cand_plus2 = shift_of(20);
		let cand_minus3 = shift_of(-30);

		let offsets = estimate_wizard_offsets(&reference, &[cand_plus2, cand_minus3]);
		assert_eq!(offsets.len(), 2);
		assert!((offsets[0] - 2.0).abs() < 0.35, "got {}", offsets[0]);
		assert!((offsets[1] + 3.0).abs() < 0.35, "got {}", offsets[1]);
	}

	/// `estimate_wizard_offsets` is robust to empty or tiny inputs: it
	/// returns 0 offsets rather than panicking.
	#[test]
	fn wizard_offsets_tolerate_short_inputs() {
		assert_eq!(estimate_wizard_offsets(&[], &[vec![]]), vec![0.0]);
		assert_eq!(estimate_wizard_offsets(&[1.0], &[vec![2.0, 3.0]]), vec![0.0]);
	}
}
