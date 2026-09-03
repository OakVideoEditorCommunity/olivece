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

//! Domain helpers over the module graph (M14 R3).
//!
//! The real engine ([`crate::oakui::real`]) drives the oak* module crates
//! directly (no liboakengine C ABI). This module is the app's own assembly
//! layer for the project/timeline/storage domain: project lifecycle (new /
//! load / save over the oaknode serializer), sequence + track + clip
//! queries, footage import, the per-sequence marker-list / workarea
//! auxiliaries, and the project-library operations over oakstorage's
//! write-through backend.
//!
//! The functions mirror the semantics the facade's `oakengine_*` exports
//! had (the facade keeps its own copies for the frozen C ABI); the
//! composition mirrors `crates/oak-cli/src/engine.rs` (M14 R2), which
//! established the same mapping for the CLI.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::{Rational, TimeRange};
use oak_node::block::ClipBlockBehavior;
use oak_node::folder::FolderBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use oak_node::value::VideoParams;
use oak_timeline::handle::CHandle;
use oak_timeline::util::{
	block_in, block_length, block_out, block_set_in, block_set_length_and_media_in,
	block_set_length_and_media_out, clip_set_media_in, NodeRef,
};

use oak_storage::backend::StorageBackend;

use super::engine::LibraryProject;

/// The shared project reference (the modules' domain project handle).
pub type ProjectRef = Arc<Mutex<Project>>;

/// Lock a project, recovering from a poisoned lock (a panicking command
/// body must not wedge every later edit).
pub fn lock(p: &ProjectRef) -> MutexGuard<'_, Project> {
	p.lock().unwrap_or_else(|e| e.into_inner())
}

/// A timeline node reference (the oaktimeline command addressing).
pub fn node_ref(p: &ProjectRef, id: NodeId) -> NodeRef {
	NodeRef::new(p.clone(), id)
}

/// The node id whose stable identity is `identity` (`None` for the
/// sentinel). The app's widget-facing ids (clip ids, explorer entry ids)
/// ARE the node identities.
pub fn id_of(identity: u64) -> Option<NodeId> {
	NodeId::from_identity(identity)
}

// ---------------------------------------------------------------------------
// Behavior borrows
// ---------------------------------------------------------------------------

/// Borrow the sequence behavior at `id`.
pub fn sequence_behavior(g: &Graph, id: NodeId) -> Option<&SequenceBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<SequenceBehavior>()
}

/// Borrow the track-list behavior at `id`.
pub fn track_list_behavior(g: &Graph, id: NodeId) -> Option<&TrackListBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackListBehavior>()
}

/// Borrow the track behavior at `id`.
pub fn track_behavior(g: &Graph, id: NodeId) -> Option<&TrackBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackBehavior>()
}

/// Borrow the clip behavior at `id`.
pub fn clip_behavior(g: &Graph, id: NodeId) -> Option<&ClipBlockBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<ClipBlockBehavior>()
}

/// Borrow the footage behavior at `id`.
pub fn footage_behavior(g: &Graph, id: NodeId) -> Option<&FootageBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<FootageBehavior>()
}

/// Whether `id` is a folder node.
pub fn is_folder(g: &Graph, id: NodeId) -> bool {
	g.get(id)
		.and_then(|e| e.behavior.as_any())
		.and_then(|a| a.downcast_ref::<oak_node::folder::FolderBehavior>())
		.is_some()
}

// ---------------------------------------------------------------------------
// Project lifecycle
// ---------------------------------------------------------------------------

/// Create a blank, initialized project (the facade's `project_create` +
/// `project_new`, minus the undo-stack clear and the storage bind — the
/// engine's adopt path owns those).
pub fn create_project() -> ProjectRef {
	let project = Project::new();
	lock(&project).initialize().ok();
	project
}

/// Load a `.ove` project file (plain XML; the module serializer ignores
/// the legacy compression flag). The filename is normalized to an absolute
/// path and the modified flag cleared, mirroring the facade's
/// `oakengine_project_load`.
pub fn load_ove(path: &Path) -> Result<ProjectRef, String> {
	let xml = std::fs::read_to_string(path)
		.map_err(|e| format!("failed to read \"{}\": {e}", path.display()))?;
	let project = oak_node::serializer::load(&xml).map_err(|e| format!("failed to parse: {e}"))?;
	let abs = if path.is_absolute() {
		path.to_path_buf()
	} else {
		std::env::current_dir()
			.map(|d| d.join(path))
			.unwrap_or_else(|_| path.to_path_buf())
	};
	let mut guard = lock(&project);
	guard.set_filename(&abs.to_string_lossy());
	guard.set_modified(false);
	drop(guard);
	Ok(project)
}

/// Write the project to `path` through the OVE serializer (the facade's
/// `oakengine_project_save` semantics: the target filename is recorded and
/// the modified flag cleared on success).
pub fn save_ove(project: &ProjectRef, path: &Path) -> Result<(), String> {
	let xml = {
		let guard = lock(project);
		oak_node::serializer::save(&guard).map_err(|e| format!("failed to serialize: {e}"))?
	};
	std::fs::write(path, &xml)
		.map_err(|e| format!("failed to write \"{}\": {e}", path.display()))?;
	let mut guard = lock(project);
	guard.set_filename(&path.to_string_lossy());
	guard.set_modified(false);
	Ok(())
}

/// The display name (`Project::name`: filename base or "(untitled)").
pub fn project_name(p: &Project) -> String {
	p.name()
}

// ---------------------------------------------------------------------------
// Sequences
// ---------------------------------------------------------------------------

/// Every sequence node in the graph, in arena order.
pub fn sequence_ids(p: &Project) -> Vec<NodeId> {
	p.graph
		.node_ids()
		.into_iter()
		.filter(|&id| sequence_behavior(&p.graph, id).is_some())
		.collect()
}

/// Every footage node in the graph, in arena order.
pub fn footage_ids(p: &Project) -> Vec<NodeId> {
	p.graph
		.node_ids()
		.into_iter()
		.filter(|&id| footage_behavior(&p.graph, id).is_some())
		.collect()
}

/// Create a sequence node named `name` directly in the project's graph
/// (unlike the facade's `oakengine_sequence_new`, which kept the sequence
/// in a scratch project, the direct-rlib app keeps it in the project so
/// saves and the write-through library cover it). The sequence is attached
/// to the project's root folder so the project explorer shows it.
pub fn create_sequence(project: &ProjectRef, name: &str) -> NodeId {
	create_sequence_with_params(project, name, None)
}

/// Create a sequence with an explicit first-video-stream format. `params`
/// `(width, height, rate, interlaced)` overrides the sequence's default
/// video parameters; `None` keeps the defaults. The sequence is attached
/// to the root folder.
pub fn create_sequence_with_params(
	project: &ProjectRef,
	name: &str,
	params: Option<(i32, i32, Rational, bool)>,
) -> NodeId {
	let mut guard = lock(project);
	let (mut core, mut behavior) = SequenceBehavior::create();
	core.label = name.to_string();
	if let Some((width, height, rate, interlaced)) = params {
		if let Some(seq) = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
		{
			if seq.video_params.is_empty() {
				seq.video_params.push(VideoParams {
					width,
					height,
					frame_rate: rate,
					pixel_format: 4, // f32
					channels: 4,
					interlaced,
				});
			} else {
				let v = &mut seq.video_params[0];
				v.width = width;
				v.height = height;
				v.frame_rate = rate;
				v.interlaced = interlaced;
			}
		}
	}
	let root = guard.root;
	let seq = guard.graph.add_node(core, behavior);
	drop(guard);
	// Mount the sequence under the root folder. Not pushed to the undo
	// stack: like the default track layout below, it is part of the
	// sequence's creation, not an undoable edit.
	oak_task::nodeops::folder_add_child_command((project.clone(), root), (project.clone(), seq))
		.redo_now();
	// A new sequence starts with the default 2 video + 2 audio track
	// layout (user-mandated NLE default: V1, V2 on top, A1, A2 below).
	// Driven directly through the add-track commands' redo — NOT pushed
	// to the undo stack, a sequence's default layout is part of its
	// creation, not an undoable edit.
	for kind in [
		TrackType::Video,
		TrackType::Video,
		TrackType::Audio,
		TrackType::Audio,
	] {
		let Some(list) = find_or_create_track_list(project, seq, kind) else {
			continue;
		};
		oak_timeline::undogeneral::TimelineAddTrackCommand::new(node_ref(project, list)).redo();
	}
	seq
}

/// Create a folder under the project's root (the C++ File > New Folder
/// action). The "New Folder" command is pushed to the undo stack — an
/// explicit user action, unlike sequence creation.
pub fn create_folder(project: &ProjectRef, name: &str) -> Result<NodeId, String> {
	let root = {
		let guard = lock(project);
		if !guard.root.valid() {
			return Err("the project has no root folder".to_string());
		}
		guard.root
	};
	let (core, behavior) = oak_node::folder::create(name);
	let id = {
		let mut guard = lock(project);
		guard.graph.add_node(core, behavior)
	};
	let cmd =
		oak_task::nodeops::folder_add_child_command((project.clone(), root), (project.clone(), id));
	oak_undo::global::push(cmd, "New Folder").map_err(|e| e.to_string())?;
	Ok(id)
}

/// Every folder node in the graph, in arena order.
pub fn folder_ids(p: &Project) -> Vec<NodeId> {
	p.graph
		.node_ids()
		.into_iter()
		.filter(|&id| is_folder(&p.graph, id))
		.collect()
}

/// Attach every orphaned sequence to the root folder with a non-undoable
/// command. Projects saved before sequences mounted under the root load
/// with their sequences free-floating; the open path runs this once to
/// migrate them.
pub fn ensure_sequences_mounted(project: &ProjectRef) {
	let (root, orphans) = {
		let guard = lock(project);
		let root = guard.root;
		let children = guard
			.graph
			.get(root)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<FolderBehavior>())
			.map(|f| f.children.clone())
			.unwrap_or_default();
		let orphans = sequence_ids(&guard)
			.into_iter()
			.filter(|&id| !children.contains(&id))
			.collect::<Vec<_>>();
		(root, orphans)
	};
	for id in orphans {
		oak_task::nodeops::folder_add_child_command((project.clone(), root), (project.clone(), id))
			.redo_now();
	}
}

/// Apply new display name and video parameters to a sequence (the
/// sequence-properties dialog's commit; mirrors the CLI's
/// `set_sequence_video_params`, plus the label). Not undoable, like the
/// CLI setter.
pub fn set_sequence_parameters(
	project: &ProjectRef,
	seq: NodeId,
	name: &str,
	width: i32,
	height: i32,
	rate: Rational,
	interlaced: bool,
) -> Result<(), String> {
	let mut guard = lock(project);
	let entry = guard
		.graph
		.get_mut(seq)
		.ok_or_else(|| "sequence no longer exists".to_string())?;
	entry.core.label = name.to_string();
	let Some(s) = entry
		.behavior
		.as_any_mut()
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	else {
		return Err("entry is not a sequence".to_string());
	};
	if s.video_params.is_empty() {
		s.video_params.push(VideoParams {
			width,
			height,
			frame_rate: rate,
			pixel_format: 4, // f32
			channels: 4,
			interlaced,
		});
	} else {
		let v = &mut s.video_params[0];
		v.width = width;
		v.height = height;
		v.frame_rate = rate;
		v.interlaced = interlaced;
	}
	Ok(())
}

/// The label of a node (`NodeCore::label`).
pub fn node_label(g: &Graph, id: NodeId) -> String {
	g.get(id).map(|e| e.core.label.clone()).unwrap_or_default()
}

/// The type id of a node (empty when the id is stale).
pub fn node_type_id(g: &Graph, id: NodeId) -> String {
	g.get(id)
		.map(|e| e.behavior.type_id().to_string())
		.unwrap_or_default()
}

/// The project's multicam nodes (the wizard/sequence drop rewire uses the
/// one whose source sequence matches).
pub fn multicam_nodes(g: &Graph) -> Vec<NodeId> {
	g.node_ids()
		.into_iter()
		.filter(|&id| {
			g.get(id)
				.map(|e| e.behavior.type_id() == "org.olivevideoeditor.Olive.multicam")
				.unwrap_or(false)
		})
		.collect()
}

/// The sequence's video format `(width, height, rate)` from its first
/// video stream.
pub fn sequence_video_params(g: &Graph, seq: NodeId) -> Option<(i32, i32, Rational)> {
	let v = sequence_behavior(g, seq)?.video_params.first()?;
	Some((v.width, v.height, v.frame_rate))
}

/// The sequence's frame duration as a `(num, den)` timebase pair (the
/// frame rate flipped; the facade's `seq_time_base`). `None` when the
/// sequence has no valid frame rate.
pub fn sequence_time_base(g: &Graph, seq: NodeId) -> Option<(i64, i64)> {
	let (_, _, rate) = sequence_video_params(g, seq)?;
	let num = rate.numerator();
	let den = rate.denominator();
	if num <= 0 || den <= 0 {
		return None;
	}
	Some((den, num))
}

/// The sequence content length (rational seconds): the longest track out
/// point across every track list (the module's `verify_length` overall).
pub fn sequence_length(g: &Graph, seq: NodeId) -> Rational {
	let mut best = Rational::new(0, 1);
	let Some(s) = sequence_behavior(g, seq) else {
		return best;
	};
	for &list_id in &s.track_lists {
		let Some(list) = track_list_behavior(g, list_id) else {
			continue;
		};
		for &track_id in &list.tracks {
			let Some(track) = track_behavior(g, track_id) else {
				continue;
			};
			for &block_id in &track.blocks {
				let Some(entry) = g.get(block_id) else {
					continue;
				};
				let Some(core) = block_core(entry) else {
					continue;
				};
				let out = core.out();
				if out > best {
					best = out;
				}
			}
		}
	}
	best
}

/// Borrow a block's core (any block kind).
fn block_core(entry: &oak_node::graph::NodeEntry) -> Option<&oak_node::block::BlockCore> {
	let any = entry.behavior.as_any()?;
	if let Some(c) = any.downcast_ref::<ClipBlockBehavior>() {
		return Some(&c.core);
	}
	if let Some(g) = any.downcast_ref::<oak_node::block::GapBlockBehavior>() {
		return Some(&g.core);
	}
	any.downcast_ref::<oak_node::block::TransitionBlockBehavior>()
		.map(|t| &t.core)
}

/// The sequence playhead (rational seconds).
pub fn sequence_playhead(g: &Graph, seq: NodeId) -> Rational {
	sequence_behavior(g, seq)
		.map(|s| s.playhead)
		.unwrap_or(Rational::new(0, 1))
}

/// Move the sequence playhead (rational seconds).
pub fn sequence_set_playhead(p: &ProjectRef, seq: NodeId, time: Rational) {
	let mut guard = lock(p);
	if let Some(s) = guard
		.graph
		.get_mut(seq)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		s.playhead = time;
	}
}

// ---------------------------------------------------------------------------
// Frame timestamps <-> rational seconds
// ---------------------------------------------------------------------------

/// Greatest common divisor (1 when both are zero).
fn gcd(a: i64, b: i64) -> i64 {
	let (mut a, mut b) = (a.abs(), b.abs());
	while b != 0 {
		let t = b;
		b = a % b;
		a = t;
	}
	if a == 0 {
		1
	} else {
		a
	}
}

/// Rational seconds -> timestamp in timebase units, rounding half away
/// from zero (the facade's `rational_to_ts`, `Timecode::k_round`).
pub fn rational_to_ts(r: Rational, tb: (i64, i64)) -> i64 {
	let (num, den) = (r.numerator(), r.denominator());
	if den == 0 || tb.0 == 0 || tb.1 == 0 {
		return 0;
	}
	let n = num as i128 * tb.1 as i128;
	let d = den as i128 * tb.0 as i128;
	let q = n / d;
	let r = n % d;
	let rr = if r < 0 { -r } else { r };
	let dd = if d < 0 { -d } else { d };
	if rr * 2 >= dd {
		(q + if n < 0 { -1 } else { 1 }) as i64
	} else {
		q as i64
	}
}

/// Timestamp -> reduced rational seconds (`time = ts * tb`).
pub fn ts_to_rational(ts: i64, tb: (i64, i64)) -> Rational {
	let num = ts as i128 * tb.0 as i128;
	let den = tb.1 as i128;
	let g = gcd((num % den) as i64, den as i64) as i128;
	Rational::new((num / g) as i64, (den / g) as i64)
}

// ---------------------------------------------------------------------------
// Tracks and clips
// ---------------------------------------------------------------------------

/// The sequence's track list of `kind`, when it exists.
pub fn track_list_of(g: &Graph, seq: NodeId, kind: TrackType) -> Option<NodeId> {
	for &list_id in &sequence_behavior(g, seq)?.track_lists {
		if track_list_behavior(g, list_id).map(|l| l.kind) == Some(kind) {
			return Some(list_id);
		}
	}
	None
}

/// Find (or create) the sequence's track list of `kind` (the facade's
/// `oaknode_sequence_get_track_list` find-or-create semantics; mirrors the
/// CLI's `find_or_create_track_list`).
pub fn find_or_create_track_list(p: &ProjectRef, seq: NodeId, kind: TrackType) -> Option<NodeId> {
	let mut guard = lock(p);
	if let Some(list) = track_list_of(&guard.graph, seq, kind) {
		return Some(list);
	}
	// Create it: a graph node owned by the sequence.
	let (core, behavior) = TrackListBehavior::create();
	let mut behavior = behavior;
	if let Some(a) = behavior.as_any_mut() {
		if let Some(list) = a.downcast_mut::<TrackListBehavior>() {
			list.kind = kind;
			list.array_base = sequence_behavior(&guard.graph, seq)?.track_lists.len() as i32;
		}
	}
	let list_id = guard.graph.add_node(core, behavior);
	if let Some(s) = guard
		.graph
		.get_mut(seq)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		s.track_lists.push(list_id);
	}
	if let Some(l) = guard
		.graph
		.get_mut(list_id)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<TrackListBehavior>())
	{
		l.sequence = Some(seq);
	}
	Some(list_id)
}

/// The tracks of the sequence's `kind` list, in stack order.
pub fn track_ids(g: &Graph, seq: NodeId, kind: TrackType) -> Vec<NodeId> {
	track_list_of(g, seq, kind)
		.and_then(|list| track_list_behavior(g, list).map(|l| l.tracks.clone()))
		.unwrap_or_default()
}

/// The clip blocks of a track (gaps skipped), in timeline order.
pub fn clip_ids(g: &Graph, track: NodeId) -> Vec<NodeId> {
	track_behavior(g, track)
		.map(|t| {
			t.blocks
				.iter()
				.copied()
				.filter(|&b| clip_behavior(g, b).is_some())
				.collect()
		})
		.unwrap_or_default()
}

/// A clip's timeline range and media in-point (rational seconds).
pub fn clip_range(g: &Graph, clip: NodeId) -> Option<(Rational, Rational, Rational)> {
	let c = clip_behavior(g, clip)?;
	Some((c.core.in_(), c.core.out(), c.core.media_in))
}

/// The clip's owning track.
pub fn clip_track(g: &Graph, clip: NodeId) -> Option<NodeId> {
	clip_behavior(g, clip)?.core.track
}

/// The first footage node feeding `id` (upstream BFS over input edges),
/// mirroring the facade's `oaknode_node_find_input_footage`.
pub fn find_input_footage(g: &Graph, id: NodeId) -> Option<NodeId> {
	let mut frontier = vec![id];
	let mut visited: Vec<NodeId> = Vec::new();
	while !frontier.is_empty() {
		let mut next = Vec::new();
		for cur in frontier {
			if visited.contains(&cur) {
				continue;
			}
			visited.push(cur);
			let entry = g.get(cur)?;
			if entry.behavior.type_id() == "org.olivevideoeditor.Olive.footage" && cur != id {
				return Some(cur);
			}
			for (src, _, _) in g.input_connections(cur) {
				next.push(src);
			}
		}
		frontier = next;
	}
	None
}

/// The media filename feeding a clip (upstream BFS + footage behavior).
pub fn clip_media_filename(g: &Graph, clip: NodeId) -> Option<String> {
	let footage = find_input_footage(g, clip)?;
	Some(footage_behavior(g, footage)?.filename.clone())
}

// ---------------------------------------------------------------------------
// Footage
// ---------------------------------------------------------------------------

/// The footage's probed duration in seconds (`None` when unprobed).
pub fn footage_duration_seconds(g: &Graph, id: NodeId) -> Option<f64> {
	let d = footage_behavior(g, id)?.duration();
	let den = d.denominator();
	if den == 0 {
		return None;
	}
	let seconds = d.numerator() as f64 / den as f64;
	(seconds > 0.0).then_some(seconds)
}

/// Reprobe footage whose stream metadata is missing: projects saved
/// before the probe recorded streams (C++ files have no `<streams>`
/// segment at all) load with an empty inventory, which leaves the
/// footage duration unknown. Relative filenames resolve against the
/// project file's directory (the C++ project-dir convention) and are
/// made absolute on the node. Best effort per node: an unreadable or
/// missing file stays unprobed.
pub fn reprobe_unprobed_footage(project: &ProjectRef) {
	let dir = {
		let guard = lock(project);
		std::path::Path::new(guard.filename())
			.parent()
			.map(|p| p.to_path_buf())
	};
	let targets: Vec<(NodeId, std::path::PathBuf)> = {
		let guard = lock(project);
		footage_ids(&guard)
			.into_iter()
			.filter_map(|id| {
				let f = footage_behavior(&guard.graph, id)?;
				if !f.streams.is_empty() || f.filename.is_empty() {
					return None;
				}
				let path = std::path::Path::new(&f.filename);
				let resolved = if path.is_absolute() {
					path.to_path_buf()
				} else {
					dir.as_ref()?.join(path)
				};
				resolved.is_file().then_some((id, resolved))
			})
			.collect()
	};
	for (id, resolved) in targets {
		let mut guard = lock(project);
		if let Some(f) = guard
			.graph
			.get_mut(id)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<FootageBehavior>())
		{
			f.filename = resolved.to_string_lossy().into_owned();
			// Best effort: a failed probe leaves the footage unprobed
			// (valid stays false), exactly like a failed import probe.
			let _ = f.probe();
		}
	}
}

/// Import a media file into the project's root folder (the facade's
/// `oakengine_project_import_footage`: the node is created in the graph
/// and one undoable "Import Footage" entry adds it to the root folder).
/// Media that fails the probe (missing, corrupt, or undecodable) is
/// rejected before it ever enters the graph — the facade's validity
/// rejection, which the C API skips.
pub fn import_footage(project: &ProjectRef, path: &Path) -> Result<NodeId, String> {
	if !path.exists() {
		return Err(format!("file does not exist: {}", path.display()));
	}
	let filename = path.to_string_lossy().into_owned();
	let (root, id) = {
		let mut guard = lock(project);
		if !guard.root.valid() {
			return Err("the project has no root folder".to_string());
		}
		let (mut core, mut behavior) = FootageBehavior::create();
		core.set_standard_value(
			"file_in",
			-1,
			oak_node::value::NodeValue::Text(filename.clone()),
		);
		let Some(f) = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<FootageBehavior>())
		else {
			return Err(
				"internal error: footage node created without footage behavior".to_string(),
			);
		};
		f.filename = filename.clone();
		// Probe before the node enters the graph so a failed probe leaves
		// no orphan behind (and nothing lands on the undo stack).
		f.probe()
			.map_err(|e| format!("failed to probe \"{}\": {e}", filename))?;
		let id = guard.graph.add_node(core, behavior);
		let label = path
			.file_name()
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_else(|| filename.clone());
		if let Some(e) = guard.graph.get_mut(id) {
			e.core.label = label;
		}
		(guard.root, id)
	};
	let cmd =
		oak_task::nodeops::folder_add_child_command((project.clone(), root), (project.clone(), id));
	oak_undo::global::push(cmd, "Import Footage").map_err(|e| e.to_string())?;
	Ok(id)
}

// ---------------------------------------------------------------------------
// Marker list / workarea auxiliaries
//
// The module's sequences never initialize their own marker/workarea state
// (`SequenceBehavior` defaults both to empty handles), so the app — like
// the facade before it — materializes one of each per open sequence. The
// engine owns the handles and releases them before the project drops.
// ---------------------------------------------------------------------------

/// Create a marker-list handle (owned, refcount 1).
pub fn marker_list_create() -> CHandle {
	oak_timeline::handle::make_owned(oak_timeline::marker::TimelineMarkerList::new())
}

/// Create a workarea handle (owned, refcount 1).
pub fn workarea_create() -> CHandle {
	oak_timeline::handle::make_owned(oak_timeline::workarea::TimelineWorkArea::new())
}

/// Release an owned marker-list / workarea handle (NULL-safe; the handle
/// is dead afterwards).
pub fn release_handle(h: &mut CHandle) {
	if let Some(release) = h.release {
		// SAFETY: `h` is an owned handle from `marker_list_create` /
		// `workarea_create`; the release runs the box's own destructor once
		// per owned reference, and the caller drops the handle afterwards.
		unsafe { release(h.ctx) };
	}
	h.ctx = std::ptr::null_mut();
}

/// One marker as the timeline shows it: in-point, name, color index.
pub fn markers_of(list: &CHandle) -> Vec<(Rational, String, i32)> {
	if list.is_null() {
		return Vec::new();
	}
	// SAFETY: `list` boxes a `TimelineMarkerList` (created by
	// `marker_list_create`); the read is shared and brief.
	let Some(l) = (unsafe {
		oak_timeline::handle::get::<
			std::sync::Arc<std::sync::Mutex<oak_timeline::marker::TimelineMarkerList>>,
		>(list)
	}) else {
		return Vec::new();
	};
	let l = l.lock().unwrap_or_else(|e| e.into_inner());
	(0..l.size())
		.filter_map(|i| l.at(i))
		.map(|m| (m.time().in_(), m.name().to_string(), m.color()))
		.collect()
}

/// The index of the first marker whose in-point equals `time`.
pub fn marker_index_at(list: &CHandle, time: Rational) -> Option<usize> {
	if list.is_null() {
		return None;
	}
	// SAFETY: as `markers_of`.
	let l = unsafe {
		oak_timeline::handle::get::<
			std::sync::Arc<std::sync::Mutex<oak_timeline::marker::TimelineMarkerList>>,
		>(list)
	}?;
	let l = l.lock().unwrap_or_else(|e| e.into_inner());
	(0..l.size()).find(|&i| l.at(i).map(|m| m.time().in_()) == Some(time))
}

/// The workarea's `(enabled, range)`.
pub fn workarea_state(wa: &CHandle) -> Option<(bool, TimeRange)> {
	if wa.is_null() {
		return None;
	}
	// SAFETY: `wa` boxes a `TimelineWorkArea` (created by
	// `workarea_create`); the read is shared and brief.
	let w = unsafe {
		oak_timeline::handle::get::<
			std::sync::Arc<std::sync::Mutex<oak_timeline::workarea::TimelineWorkArea>>,
		>(wa)
	}?;
	let w = w.lock().unwrap_or_else(|e| e.into_inner());
	Some((w.enabled(), *w.range()))
}

/// Live (non-undoable) workarea write: enable flag plus range.
pub fn workarea_set(wa: &CHandle, enabled: bool, range: TimeRange) {
	if wa.is_null() {
		return;
	}
	// SAFETY: `wa` boxes a `TimelineWorkArea`; the engine writes it only
	// from the UI thread.
	if let Some(w) = unsafe {
		oak_timeline::handle::get_mut::<
			std::sync::Arc<std::sync::Mutex<oak_timeline::workarea::TimelineWorkArea>>,
		>(wa)
	} {
		let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
		w.set_enabled(enabled);
		w.set_range(range);
	}
}

// ---------------------------------------------------------------------------
// Project library (M13 D4): the write-through database the manager browses
//
// The facade's library_* exports serialized rows to JSON only because they
// crossed the C ABI; the direct calls below return plain values.
// ---------------------------------------------------------------------------

/// The configured default library as a parsed URI; an error when the
/// write-through backend is disabled or the path does not resolve.
fn library() -> Result<oak_storage::uri::StorageUri, String> {
	if !oak_storage::writethrough::storage_enabled() {
		return Err("the project library is not configured".to_string());
	}
	let uri = oak_storage::writethrough::library_uri()
		.ok_or_else(|| "the project library path does not resolve".to_string())?;
	oak_storage::uri::StorageUri::parse(&uri).map_err(|e| e.to_string())
}

/// The library URI selecting one row (`…?project=<uuid>`).
fn project_uri(uuid: &str) -> Result<oak_storage::uri::StorageUri, String> {
	let uri = library()?;
	oak_storage::uri::StorageUri::parse(&format!("{}?project={uuid}", uri.to_uri_string()))
		.map_err(|e| e.to_string())
}

/// The library rows, most recently modified first (the project manager's
/// data source). With storage disabled the result is the empty list, not
/// an error (the facade contract).
pub fn library_list() -> Result<Vec<LibraryProject>, String> {
	if !oak_storage::writethrough::storage_enabled() {
		return Ok(Vec::new());
	}
	let uri = library()?;
	let backend = oak_storage::writethrough::backend();
	let infos = backend.list_projects(&uri).map_err(|e| e.to_string())?;
	Ok(infos
		.into_iter()
		.map(|info| {
			let stats = backend.project_stats(&uri, &info.uuid).unwrap_or_default();
			LibraryProject {
				uuid: info.uuid,
				name: info.name,
				created_at: info.created_at.and_utc().timestamp(),
				modified_at: info.modified_at.and_utc().timestamp(),
				duration_ms: stats.duration_ms,
				track_count: stats.track_count,
				clip_count: stats.clip_count,
				footage_count: stats.footage_count,
			}
		})
		.collect())
}

/// Create a blank project row named `name`; returns its uuid. The row
/// lands immediately (one `kind='import'` command), so the manager list
/// shows it before the first edit.
pub fn library_create(name: &str) -> Result<String, String> {
	if name.trim().is_empty() {
		return Err("invalid name".to_string());
	}
	let uri = library()?;
	let project = create_project();
	let uuid = {
		let mut guard = lock(&project);
		guard
			.settings
			.insert("projectname".to_string(), name.to_string());
		guard.uuid.clone()
	};
	let result = oak_storage::writethrough::backend()
		.save_project(&project, &uri, 0)
		.map_err(|e| e.to_string());
	result?;
	Ok(uuid)
}

/// Delete the library row `uuid` (cascades settings / snapshots /
/// journal).
pub fn library_delete(uuid: &str) -> Result<(), String> {
	if uuid.is_empty() {
		return Err("invalid uuid".to_string());
	}
	oak_storage::writethrough::backend()
		.delete_project(&library()?, uuid)
		.map_err(|e| e.to_string())
}

/// Rename the library row `uuid` (the manager's list name).
pub fn library_rename(uuid: &str, name: &str) -> Result<(), String> {
	if uuid.is_empty() || name.trim().is_empty() {
		return Err("invalid uuid or name".to_string());
	}
	oak_storage::writethrough::backend()
		.rename_project(&library()?, uuid, name.trim())
		.map_err(|e| e.to_string())
}

/// Duplicate the library row `uuid` (history included) under a fresh
/// uuid; returns the new row's uuid. `None` name defaults to
/// `<name> (copy)` backend-side.
pub fn library_duplicate(uuid: &str) -> Result<String, String> {
	if uuid.is_empty() {
		return Err("invalid uuid".to_string());
	}
	let info = oak_storage::writethrough::backend()
		.duplicate_project(&library()?, uuid, None)
		.map_err(|e| e.to_string())?;
	Ok(info.uuid)
}

/// Import a `.ove` / `.otio` / `.fcpxml` project file as a new library
/// row; returns the new row's uuid.
pub fn library_import(path: &Path) -> Result<String, String> {
	let file_uri =
		oak_storage::uri::StorageUri::parse(&path.to_string_lossy()).map_err(|e| e.to_string())?;
	oak_storage::writethrough::backend()
		.import_from_file(&library()?, &file_uri)
		.map_err(|e| e.to_string())
}

/// Export the library row `uuid` to `path`; the format is dispatched by
/// extension through the oakstorage registry.
pub fn library_export(uuid: &str, path: &Path) -> Result<(), String> {
	if uuid.is_empty() {
		return Err("invalid uuid".to_string());
	}
	let file_uri =
		oak_storage::uri::StorageUri::parse(&path.to_string_lossy()).map_err(|e| e.to_string())?;
	oak_storage::writethrough::backend()
		.export_to_file(&library()?, uuid, &file_uri)
		.map_err(|e| e.to_string())
}

/// Load the library row `uuid` as a fresh project (the modified flag is
/// cleared, mirroring the facade's `oakengine_project_load_library`; the
/// undo-stack clear and the storage bind are the engine adopt path's job).
pub fn library_open(uuid: &str) -> Result<ProjectRef, String> {
	if uuid.is_empty() {
		return Err("invalid uuid".to_string());
	}
	let result = oak_storage::writethrough::backend()
		.load(&project_uri(uuid)?)
		.map_err(|e| e.to_string())?;
	if result.project.is_null() {
		return Err(format!(
			"library load of {uuid} returned no project (info code {})",
			result.version_info
		));
	}
	let handle = result.project;
	let project =
		unsafe { oak_storage::nodeutil::project_arc(&handle) }.map_err(|e| e.to_string())?;
	lock(&project).set_modified(false);
	Ok(project)
}

// ---------------------------------------------------------------------------
// Write-through binding (the engine's per-project session state)
// ---------------------------------------------------------------------------

/// Bind `project` to the configured default library and return the handle
/// the binding was registered under (the caller keeps it for
/// [`storage_bound`] / [`storage_last_error`] queries and releases it with
/// [`storage_unbind`]). No-op (None still returned) semantics mirror the
/// facade: an unconfigured library leaves the project unbound but the
/// handle is still usable for queries.
pub fn storage_bind(project: &ProjectRef) -> CHandle {
	let handle = oak_storage::nodeutil::make_project_owned(project.clone());
	oak_storage::writethrough::bind_project(handle);
	handle
}

/// Whether the project behind `handle` is bound to the write-through
/// session (the status bar's write state).
pub fn storage_bound(handle: &CHandle) -> bool {
	oak_storage::writethrough::is_bound(*handle)
}

/// The last write-through / snapshot error of the project behind
/// `handle`, if any.
pub fn storage_last_error(handle: &CHandle) -> Option<String> {
	oak_storage::writethrough::last_error(*handle)
}

/// Flush the project's pending writes, drop its binding and release the
/// query handle (the engine's project-close path).
pub fn storage_unbind(handle: CHandle) {
	oak_storage::writethrough::unbind_project(handle);
	oak_storage::nodeutil::release_project(handle);
}

/// Flush every bound project and stop the snapshot thread (app exit).
pub fn storage_flush() {
	oak_storage::writethrough::flush_all();
}

// ---------------------------------------------------------------------------
// Undoable edit primitives
//
// The timeline edits the facade's `oakengine_sequence_*` / `oakengine_clip_*`
// exports performed, rebuilt over the oaktimeline command structs and the
// oakundo global stack's safe [`oak_undo::global::push`]. Every entry point
// pushes exactly one undo row (composite edits assemble a multi command).
// ---------------------------------------------------------------------------

/// Push one command onto the global undo stack (redo then record).
pub fn push_command(cmd: oak_undo::undocommand::UndoCommand, name: &str) -> Result<(), String> {
	oak_undo::global::push(cmd, name).map_err(|e| e.to_string())
}

/// Assemble `children` into ONE multi command and push it (an empty set
/// is a no-op, mirroring the stack's empty-multi discard).
pub fn push_multi_command(
	children: Vec<oak_undo::undocommand::UndoCommand>,
	name: &str,
) -> Result<(), String> {
	if children.is_empty() {
		return Ok(());
	}
	let mut multi = oak_undo::undocommand::UndoCommand::multi();
	for child in children {
		multi.multi_add_child(child);
	}
	push_command(multi, name)
}

/// Push one command (private alias).
fn push(cmd: oak_undo::undocommand::UndoCommand, name: &str) -> Result<(), String> {
	push_command(cmd, name)
}

/// Assemble and push a multi command (private alias).
fn push_multi(children: Vec<oak_undo::undocommand::UndoCommand>, name: &str) -> Result<(), String> {
	push_multi_command(children, name)
}

/// An undoable edge add (the module's `oaknode_node_connect_undoable`
/// semantics): validated at creation (existence, connectability, not
/// already connected); the redo connects, the undo disconnects the input.
pub fn connect_command(
	p: &ProjectRef,
	from: NodeId,
	to: NodeId,
	input_id: &str,
) -> Result<oak_undo::undocommand::UndoCommand, String> {
	{
		let g = lock(p);
		if !g.graph.is_valid(from) || !g.graph.is_valid(to) {
			return Err("connect: node not found".to_string());
		}
		let entry = g.graph.get(to).ok_or("connect: node not found")?;
		let input = entry
			.core
			.get_input(input_id)
			.ok_or_else(|| format!("connect: no input \"{input_id}\""))?;
		if !input.is_connectable() {
			return Err(format!("connect: input \"{input_id}\" is not connectable"));
		}
		if g.graph.connected_output(to, input_id, -1).is_some() {
			return Err(format!(
				"connect: input \"{input_id}\" is already connected"
			));
		}
	}
	let (p1, p2) = (p.clone(), p.clone());
	let (id1, id2) = (input_id.to_string(), input_id.to_string());
	Ok(oak_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut g = lock(&p1);
			let _ = g.graph.connect(from, to, &id1, -1);
		},
		move || {
			let mut g = lock(&p2);
			g.graph.disconnect_input(to, &id2, -1);
		},
	))
}

/// An undoable edge remove: succeeds even when nothing is connected (the
/// redo is then a no-op, mirroring the C++ command's redo swallowing). The
/// undo re-connects the edge captured at construction — the module's
/// `oaknode_node_disconnect_undoable` did not model this (documented
/// deviation); the app retains the source node, so its effect-chain edits
/// undo faithfully.
pub fn disconnect_command(
	p: &ProjectRef,
	to: NodeId,
	input_id: &str,
) -> Result<oak_undo::undocommand::UndoCommand, String> {
	let source = {
		let g = lock(p);
		let has_input = g
			.graph
			.get(to)
			.map(|e| e.core.has_input(input_id))
			.unwrap_or(false);
		if !has_input {
			return Err(format!("disconnect: no input \"{input_id}\""));
		}
		g.graph.connected_output(to, input_id, -1)
	};
	let (p1, p2) = (p.clone(), p.clone());
	let (id1, id2) = (input_id.to_string(), input_id.to_string());
	Ok(oak_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut g = lock(&p1);
			g.graph.disconnect_input(to, &id1, -1);
		},
		move || {
			if let Some(from) = source {
				let mut g = lock(&p2);
				let _ = g.graph.connect(from, to, &id2, -1);
			}
		},
	))
}

/// An undoable context-position set (the facade's
/// `oakengine_node_set_context_position`): the first entry is established
/// by the redo itself; the undo restores the previous position or removes
/// the entry it created.
pub fn set_context_position_command(
	p: &ProjectRef,
	node: NodeId,
	context: NodeId,
	x: f64,
	y: f64,
) -> Result<oak_undo::undocommand::UndoCommand, String> {
	let old = {
		let g = lock(p);
		if !g.graph.is_valid(node) || !g.graph.is_valid(context) {
			return Err("set position: node not found".to_string());
		};
		g.graph.get(node).and_then(|e| {
			e.core
				.context_positions
				.iter()
				.find(|(c, _, _)| *c == context)
				.map(|(_, pos, expanded)| (*pos, *expanded))
		})
	};
	let (p1, p2) = (p.clone(), p.clone());
	Ok(oak_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut g = lock(&p1);
			if let Some(e) = g.graph.get_mut(node) {
				e.core.set_context_position(context, x, y, false);
			}
		},
		move || {
			let mut g = lock(&p2);
			if let Some(e) = g.graph.get_mut(node) {
				match old {
					Some(((ox, oy), expanded)) => {
						e.core.set_context_position(context, ox, oy, expanded);
					}
					None => {
						e.core.remove_from_context(context);
					}
				}
			}
		},
	))
}

/// Add a track of `kind` to the sequence (undoable "Add Track"; the
/// module's `TimelineAddTrackCommand`), returning the new track's index.
pub fn add_track(p: &ProjectRef, seq: NodeId, kind: TrackType) -> Result<usize, String> {
	let list = find_or_create_track_list(p, seq, kind)
		.ok_or_else(|| "sequence has no track list for this type".to_string())?;
	let before: Vec<NodeId> = {
		let g = lock(p);
		track_list_behavior(&g.graph, list)
			.map(|l| l.tracks.clone())
			.unwrap_or_default()
	};
	push(
		oak_timeline::undogeneral::TimelineAddTrackCommand::new(node_ref(p, list)).to_command(),
		"Add Track",
	)?;
	let g = lock(p);
	let tracks = track_list_behavior(&g.graph, list)
		.map(|l| l.tracks.clone())
		.ok_or_else(|| "add track command produced no track list".to_string())?;
	tracks
		.iter()
		.position(|id| !before.contains(id))
		.ok_or_else(|| "add track command produced no track".to_string())
}

/// Remove `track` from its list (undoable "Remove Track"; the module's
/// `TimelineRemoveTrackCommand` redo performs the full list+graph
/// removal, so no live compensation is needed).
pub fn remove_track(p: &ProjectRef, track: NodeId) -> Result<(), String> {
	push(
		oak_timeline::undogeneral::TimelineRemoveTrackCommand::new(node_ref(p, track)).to_command(),
		"Remove Track",
	)
}

/// A track's height in internal units (`None` when the id is stale).
pub fn track_height(p: &ProjectRef, track: NodeId) -> Option<f64> {
	let g = lock(p);
	track_behavior(&g.graph, track).map(|t| t.height)
}

/// A track's muted flag (`None` when the id is stale). Muted means
/// "silenced" on audio tracks and "hidden" on video/subtitle tracks
/// (Olive parity: one flag drives both).
pub fn track_muted(p: &ProjectRef, track: NodeId) -> Option<bool> {
	let g = lock(p);
	track_behavior(&g.graph, track).map(|t| t.muted)
}

/// A track's locked flag (`None` when the id is stale).
pub fn track_locked(p: &ProjectRef, track: NodeId) -> Option<bool> {
	let g = lock(p);
	track_behavior(&g.graph, track).map(|t| t.locked)
}

/// An undoable track-flag set (the closures capture the previous value, so
/// the undo restores it exactly). Shared by the mute/hide and lock toggles.
fn set_track_flag(
	p: &ProjectRef,
	track: NodeId,
	field: TrackFlag,
	value: bool,
	name: &str,
) -> Result<(), String> {
	let old = {
		let g = lock(p);
		let t = track_behavior(&g.graph, track)
			.ok_or_else(|| "set track flag: the node is not a track".to_string())?;
		match field {
			TrackFlag::Muted => t.muted,
			TrackFlag::Locked => t.locked,
		}
	};
	if old == value {
		return Ok(());
	}
	let (p1, p2) = (p.clone(), p.clone());
	push(
		oak_undo::undocommand::UndoCommand::from_closures(
			move || {
				let mut g = lock(&p1);
				if let Some(t) = g
					.graph
					.get_mut(track)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<TrackBehavior>())
				{
					match field {
						TrackFlag::Muted => t.muted = value,
						TrackFlag::Locked => t.locked = value,
					}
				}
			},
			move || {
				let mut g = lock(&p2);
				if let Some(t) = g
					.graph
					.get_mut(track)
					.and_then(|e| e.behavior.as_any_mut())
					.and_then(|a| a.downcast_mut::<TrackBehavior>())
				{
					match field {
						TrackFlag::Muted => t.muted = old,
						TrackFlag::Locked => t.locked = old,
					}
				}
			},
		),
		name,
	)
}

/// The track flags the undoable setters cover.
#[derive(Clone, Copy)]
enum TrackFlag {
	/// Muted (audio) / hidden (video, subtitle).
	Muted,
	/// Locked against clip edits.
	Locked,
}

/// Set a track's muted flag (undoable "Set Track Muted"). On video and
/// subtitle tracks this is the visibility (show/hide) toggle.
pub fn set_track_muted(p: &ProjectRef, track: NodeId, muted: bool) -> Result<(), String> {
	set_track_flag(p, track, TrackFlag::Muted, muted, "Set Track Muted")
}

/// Set a track's locked flag (undoable "Set Track Locked"). Locked tracks
/// reject clip edits (the app layer refuses trim/move/split/delete).
pub fn set_track_locked(p: &ProjectRef, track: NodeId, locked: bool) -> Result<(), String> {
	set_track_flag(p, track, TrackFlag::Locked, locked, "Set Track Locked")
}

/// Set a track's height in internal units (NOT undoable, mirroring the
/// facade's `oakengine_track_set_height`).
pub fn set_track_height(p: &ProjectRef, track: NodeId, height: f64) {
	if height <= 0.0 {
		return;
	}
	let mut g = lock(p);
	if let Some(t) = g
		.graph
		.get_mut(track)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<TrackBehavior>())
	{
		t.height = height;
	}
}

/// The stable per-clip color index, fixed when the clip is created: the
/// node identity is unique per creation and never shifts when clips are
/// added or removed elsewhere, so a clip keeps its color for its whole
/// life (persisted through [`crate::oakui::real`]'s `override_color`).
fn clip_color_index(id: NodeId) -> i32 {
	(id.identity() % 8) as i32
}

/// The display name of `footage` for clip labels: its node label (the
/// imported basename), falling back to the file basename.
fn footage_display_name(g: &Graph, footage: NodeId) -> String {
	if let Some(e) = g.get(footage) {
		if !e.core.label.is_empty() {
			return e.core.label.clone();
		}
	}
	footage_behavior(g, footage)
		.and_then(|f| {
			Path::new(&f.filename)
				.file_name()
				.map(|n| n.to_string_lossy().into_owned())
		})
		.unwrap_or_default()
}

/// Create a footage clip block in `g`: the shared span/state setup plus
/// the per-clip color and footage-name label, both fixed at creation
/// time (C++ `oaknode_clip_set_media_in` +
/// `oaknode_block_set_length_and_media_in`).
fn create_footage_clip(
	g: &mut Graph,
	footage: NodeId,
	media_in: Rational,
	length: Rational,
) -> NodeId {
	let label = footage_display_name(g, footage);
	let (core, behavior) = oak_node::block::clip_create();
	let id = g.add_node(core, behavior);
	if let Some(entry) = g.get_mut(id) {
		entry.core.override_color = clip_color_index(id);
		entry.core.label = label;
		if let Some(c) = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
		{
			c.core.media_in = media_in;
			c.core.set_length_and_media_in(length);
		}
	}
	id
}

/// Place a clip of `footage` on track `track_index` of the sequence's
/// `kind` list (undoable "Add Clip", one row): the clip block is created
/// in the project, placed by the module's `TrackPlaceBlockCommand`, and
/// the footage is wired to the clip's `tex_in` — the facade's
/// `oakengine_sequence_add_footage_clip_ex` composition, minus the
/// scratch-project dance (the app keeps everything in one project).
///
/// `in_ts`/`out_ts`/`media_in_ts` are frame timestamps in the sequence's
/// frame-rate timebase. On undo the block is detached from the track but
/// left as an orphan node in the project graph (the module command's
/// documented behavior).
pub fn place_footage_clip(
	p: &ProjectRef,
	seq: NodeId,
	footage: NodeId,
	kind: TrackType,
	track_index: usize,
	in_ts: i64,
	out_ts: i64,
	media_in_ts: i64,
) -> Result<NodeId, String> {
	if in_ts < 0 || out_ts <= in_ts || media_in_ts < 0 {
		return Err("invalid clip range (need 0 <= in < out and media_in >= 0)".to_string());
	}
	if kind != TrackType::Video && kind != TrackType::Audio {
		return Err("clips are only supported on video and audio tracks".to_string());
	}
	let (tb, list) = {
		let g = lock(p);
		if footage_behavior(&g.graph, footage).is_none() {
			return Err("the footage node is not in the project".to_string());
		}
		let tb = sequence_time_base(&g.graph, seq)
			.ok_or_else(|| "sequence has no valid frame rate".to_string())?;
		let list = track_list_of(&g.graph, seq, kind)
			.ok_or_else(|| "sequence has no track list for this type".to_string())?;
		(tb, list)
	};
	let track_count = {
		let g = lock(p);
		track_list_behavior(&g.graph, list)
			.map(|l| l.tracks.len())
			.unwrap_or(0)
	};
	if track_index >= track_count {
		return Err(format!(
			"track index {track_index} out of range ({track_count} tracks)"
		));
	}

	let in_r = ts_to_rational(in_ts, tb);
	let out_r = ts_to_rational(out_ts, tb);
	let media_r = ts_to_rational(media_in_ts, tb);
	let length = out_r - in_r;

	// The clip block, positioned by media-in + length and stamped with
	// its creation-time color + footage-name label.
	let clip = {
		let mut g = lock(p);
		create_footage_clip(&mut g.graph, footage, media_r, length)
	};

	let place = oak_timeline::undopointer::TrackPlaceBlockCommand::new(
		node_ref(p, list),
		track_index as i32,
		node_ref(p, clip),
		in_r,
	)
	.to_command();
	let edge = connect_command(p, footage, clip, oak_node::block::clip_input::TEXTURE_INPUT)?;
	push_multi(vec![place, edge], "Add Clip")?;
	Ok(clip)
}

/// Places ONE video clip fed by a sequence node — a nested-sequence clip
/// (drag a sequence entry from the project explorer onto the timeline).
/// The clip reads the sequence's output during playback; the sequence's
/// own tracks stay in the source sequence (a frame, not a folder of
/// tracks, on the host timeline).
pub fn place_nested_sequence_clip(
	p: &ProjectRef,
	host_seq: NodeId,
	source_seq: NodeId,
	track_index: usize,
	in_ts: i64,
	out_ts: i64,
) -> Result<NodeId, String> {
	if in_ts < 0 || out_ts <= in_ts {
		return Err("invalid clip range (need 0 <= in < out)".to_string());
	}
	let (tb, list, source_behavior) = {
		let g = lock(p);
		let s = sequence_behavior(&g.graph, source_seq)
			.ok_or_else(|| "the source sequence is not in the project".to_string())?;
		let tb = sequence_time_base(&g.graph, host_seq)
			.ok_or_else(|| "host sequence has no valid frame rate".to_string())?;
		let list = track_list_of(&g.graph, host_seq, TrackType::Video)
			.ok_or_else(|| "host sequence has no video track list".to_string())?;
		let label = format!("{}（序列）", node_label(&g.graph, source_seq));
		(tb, list, label)
	};
	let track_count = {
		let g = lock(p);
		track_list_behavior(&g.graph, list)
			.map(|l| l.tracks.len())
			.unwrap_or(0)
	};
	if track_index >= track_count {
		return Err(format!(
			"track index {track_index} out of range ({track_count} tracks)"
		));
	}

	let in_r = ts_to_rational(in_ts, tb);
	let out_r = ts_to_rational(out_ts, tb);
	let length = out_r - in_r;

	let clip = {
		let mut g = lock(p);
		let (ccore, cbehavior) = oak_node::block::clip_create();
		let mut core = ccore;
		core.label = source_behavior;
		g.graph.add_node(core, cbehavior)
	};
	{
		let mut g = lock(p);
		let Some(clip_behavior) = g
			.graph
			.get_mut(clip)
			.and_then(|e| e.behavior.as_any_mut())
			.and_then(|a| a.downcast_mut::<oak_node::block::ClipBlockBehavior>())
		else {
			return Err("clip node is not a clip".to_string());
		};
		clip_behavior.core.range = oak_core::TimeRange::new(in_r, length);
	}

	let place = oak_timeline::undopointer::TrackPlaceBlockCommand::new(
		node_ref(p, list),
		track_index as i32,
		node_ref(p, clip),
		in_r,
	)
	.to_command();
	let edge = connect_command(
		p,
		source_seq,
		clip,
		oak_node::block::clip_input::TEXTURE_INPUT,
	)?;
	push_multi(vec![place, edge], "Add Clip")?;
	Ok(clip)
}

/// Place linked clips of one footage on several tracks in ONE undoable
/// "Add Clip" entry (the NLE A/V-drop: a video-with-audio file lands as
/// a video clip plus a linked audio clip). `placements` lists the
/// `(track kind, track index)` targets in order; every clip shares the
/// same timeline range and media-in, and all clips are linked both ways
/// (C++ `block_links_` semantics: grouped edits like split/ripple apply
/// to the whole group).
pub fn place_footage_clips_linked(
	p: &ProjectRef,
	seq: NodeId,
	footage: NodeId,
	placements: &[(TrackType, usize)],
	in_ts: i64,
	out_ts: i64,
	media_in_ts: i64,
) -> Result<Vec<NodeId>, String> {
	if placements.len() < 2 {
		return Err("linked placement needs at least two tracks".to_string());
	}
	let tb = {
		let g = lock(p);
		if footage_behavior(&g.graph, footage).is_none() {
			return Err("the footage node is not in the project".to_string());
		}
		sequence_time_base(&g.graph, seq)
			.ok_or_else(|| "sequence has no valid frame rate".to_string())?
	};
	let in_r = ts_to_rational(in_ts, tb);
	let out_r = ts_to_rational(out_ts, tb);
	let media_r = ts_to_rational(media_in_ts, tb);
	let length = out_r - in_r;

	// Create all clips first (graph writes are not undoable; the placement
	// commands below are). Each gets its creation-time color + footage
	// name label.
	let mut clips = Vec::with_capacity(placements.len());
	for _ in placements {
		let mut g = lock(p);
		let id = create_footage_clip(&mut g.graph, footage, media_r, length);
		clips.push(id);
	}

	let mut commands = Vec::new();
	for (&(kind, track_index), &clip) in placements.iter().zip(&clips) {
		let list = {
			let g = lock(p);
			track_list_of(&g.graph, seq, kind)
				.ok_or_else(|| "sequence has no track list for this type".to_string())?
		};
		commands.push(
			oak_timeline::undopointer::TrackPlaceBlockCommand::new(
				node_ref(p, list),
				track_index as i32,
				node_ref(p, clip),
				in_r,
			)
			.to_command(),
		);
		commands.push(connect_command(
			p,
			footage,
			clip,
			oak_node::block::clip_input::TEXTURE_INPUT,
		)?);
	}
	// Link the group both ways. Each ordered pair is its own command whose
	// undo removes ONLY the direction it added (incremental undo), so a
	// link created after the drop survives the drop's undo.
	for (i, &a) in clips.iter().enumerate() {
		for (j, &b) in clips.iter().enumerate() {
			if i == j {
				continue;
			}
			let (p1, p2) = (p.clone(), p.clone());
			commands.push(oak_undo::undocommand::UndoCommand::from_closures(
				move || {
					let mut g = lock(&p1);
					if let Some(entry) = g.graph.get_mut(a) {
						let links = &mut entry.core.links;
						if !links.contains(&b) {
							links.push(b);
						}
					}
				},
				move || {
					let mut g = lock(&p2);
					if let Some(entry) = g.graph.get_mut(a) {
						entry.core.links.retain(|&l| l != b);
					}
				},
			));
		}
	}
	push_multi(commands, "Add Clip")?;
	Ok(clips)
}

/// Split `clip` at `time_ts` (a frame timestamp strictly inside the
/// clip's range), undoable "Split Clip" (the module's
/// `BlockSplitCommand`).
pub fn split_clip(p: &ProjectRef, clip: NodeId, time_ts: i64) -> Result<(), String> {
	let (tb, in_r, out_r) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (in_r, out_r, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		(tb, in_r, out_r)
	};
	let point = ts_to_rational(time_ts, tb);
	if point <= in_r || point >= out_r {
		return Err(format!(
			"split time {time_ts} is not strictly inside the clip"
		));
	}
	push(
		oak_timeline::undosplit::BlockSplitCommand::new(node_ref(p, clip), point).to_command(),
		"Split Clip",
	)
}

/// Split every clip of `blocks` at `time_ts` as ONE undoable
/// `BlockSplitPreservingLinksCommand` (the Cmd+K path): originally linked
/// pairs that are both split get their rear halves linked too — the
/// per-block plain split left the rear half of an A/V pair unlinked.
/// Blocks whose range does not strictly contain the time are skipped by
/// the command itself.
pub fn split_clips_preserving_links(
	p: &ProjectRef,
	blocks: &[NodeId],
	time_ts: i64,
) -> Result<(), String> {
	let Some((&first, rest)) = blocks.split_first() else {
		return Ok(());
	};
	let tb = {
		let g = lock(p);
		clip_track(&g.graph, first)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?
	};
	let point = ts_to_rational(time_ts, tb);
	let mut refs = Vec::with_capacity(blocks.len());
	refs.push(node_ref(p, first));
	refs.extend(rest.iter().map(|&b| node_ref(p, b)));
	let times = vec![point; refs.len()];
	push(
		oak_timeline::undosplit::BlockSplitPreservingLinksCommand::new(refs, times).to_command(),
		"Split Clips",
	)
}

/// Trim `clip`'s timeline range to `[new_in_ts, new_out_ts)` (undoable
/// "Trim Clip"; the facade's `oakengine_clip_trim` semantics: one end at
/// a time, trim-in anchors the OUT, trim-out anchors the IN — the
/// module's own `BlockTrimCommand` applies its length setters with
/// inverted semantics, so the closures carry the correct mapping).
/// An undoable length change for `clip` (trim semantics: `out_anchored`
/// keeps the out point — trim-in — and shifts the in; otherwise the in
/// stays — trim-out). Shared by [`trim_clip`] and [`ripple_trim_clip`];
/// the closure applies the new length on redo and restores `old` on undo.
pub(crate) fn trim_command(
	p: &ProjectRef,
	clip: NodeId,
	out_anchored: bool,
	old: Rational,
	new: Rational,
) -> oak_undo::undocommand::UndoCommand {
	use oak_node::block::BlockCore;
	let (p1, p2) = (p.clone(), p.clone());
	oak_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut g = lock(&p1);
			if let Some(c) = g
				.graph
				.get_mut(clip)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			{
				if out_anchored {
					BlockCore::set_length_and_media_out(&mut c.core, new);
				} else {
					BlockCore::set_length_and_media_in(&mut c.core, new);
				}
			}
		},
		move || {
			let mut g = lock(&p2);
			if let Some(c) = g
				.graph
				.get_mut(clip)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			{
				if out_anchored {
					BlockCore::set_length_and_media_out(&mut c.core, old);
				} else {
					BlockCore::set_length_and_media_in(&mut c.core, old);
				}
			}
		},
	)
}

/// Trim `clip`'s timeline range to `[new_in_ts, new_out_ts)` (undoable
/// "Trim Clip"; the facade's `oakengine_clip_trim` semantics: one end at
/// a time, trim-in anchors the OUT, trim-out anchors the IN — the
/// module's own `BlockTrimCommand` applies its length setters with
/// inverted semantics, so the closures carry the correct mapping).
pub fn trim_clip(
	p: &ProjectRef,
	clip: NodeId,
	new_in_ts: i64,
	new_out_ts: i64,
) -> Result<(), String> {
	if new_in_ts < 0 || new_out_ts <= new_in_ts {
		return Err("invalid trim range (need 0 <= new_in < new_out)".to_string());
	}
	let (tb, old_in, old_out, old_length) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (in_r, out_r, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		let length = out_r - in_r;
		(tb, in_r, out_r, length)
	};
	if new_in_ts == rational_to_ts(old_in, tb) && new_out_ts == rational_to_ts(old_out, tb) {
		return Ok(());
	}
	let new_in = ts_to_rational(new_in_ts, tb);
	let new_out = ts_to_rational(new_out_ts, tb);

	let mut children = Vec::new();
	if new_in != old_in {
		// in-trim: length = block out - new in (out anchored).
		children.push(trim_command(p, clip, true, old_length, old_out - new_in));
	}
	if new_out != old_out {
		// out-trim: length = new out - new in (in anchored); the old
		// length is the post-in-trim length (out - new in) when both ends
		// move.
		let post_in_trim = old_out - new_in;
		children.push(trim_command(p, clip, false, post_in_trim, new_out - new_in));
	}
	push_multi(children, "Trim Clip")
}

/// Ripple-trim `clip` so its `start_edge` lands on `new_frame` (undoable
/// "Ripple Trim Clip"): the trimmed clip's media anchor is kept (trim-in
/// anchors the OUT, trim-out anchors the IN, exactly like [`trim_clip`]),
/// and every block after it on the same track shifts rigidly by the same
/// delta — no gap opens behind the trimmed edge, the tail just follows.
pub fn ripple_trim_clip(
	p: &ProjectRef,
	clip: NodeId,
	start_edge: bool,
	new_frame: i64,
) -> Result<(), String> {
	let (tb, old_in, old_out, tail) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (in_r, out_r, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		let track =
			clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?;
		let blocks = track_behavior(&g.graph, track)
			.map(|t| t.blocks.clone())
			.unwrap_or_default();
		let index = blocks
			.iter()
			.position(|&b| b == clip)
			.ok_or_else(|| "the clip is not on its track".to_string())?;
		(tb, in_r, out_r, blocks[index + 1..].to_vec())
	};
	let new = ts_to_rational(new_frame, tb);
	let old_length = old_out - old_in;
	// start_edge (trim-in): the out stays anchored, length = out - new;
	// the tail shifts by (new - in). Otherwise (trim-out): the in stays
	// anchored, length = new - in; the tail shifts by (new - out).
	let (trim_len, anchored) = if start_edge {
		(old_out - new, true)
	} else {
		(new - old_in, false)
	};
	let delta = if start_edge {
		new - old_in
	} else {
		new - old_out
	};
	if delta.is_null() {
		return Ok(());
	}
	let mut children = Vec::new();
	children.push(trim_command(p, clip, anchored, old_length, trim_len));
	for b in tail {
		let b_ref = node_ref(p, b);
		let b_old_in = block_in(&b_ref);
		let (r1, r2) = (b_ref.clone(), b_ref);
		children.push(oak_undo::undocommand::UndoCommand::from_closures(
			move || block_set_in(&r1, b_old_in + delta),
			move || block_set_in(&r2, b_old_in),
		));
	}
	push_multi(children, "Ripple Trim Clip")
}

/// Roll-edit the shared boundary of adjacent clips `a`/`b` (both on
/// `track`) to `new_frame` (undoable "Roll Edit"): the boundary moves
/// without disturbing anything else on the track — the module's own
/// `BlockTrimCommand` in roll-edit mode trims `a` out-anchored and
/// compensates `b` in-anchored, so both media anchors are preserved.
pub fn roll_edit(
	p: &ProjectRef,
	track: NodeId,
	a: NodeId,
	b: NodeId,
	new_frame: i64,
) -> Result<(), String> {
	let (tb, a_in, a_out, b_in, b_out) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, a)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (a_in, a_out, _) =
			clip_range(&g.graph, a).ok_or_else(|| "node a is not a clip".to_string())?;
		let (b_in, b_out, _) =
			clip_range(&g.graph, b).ok_or_else(|| "node b is not a clip".to_string())?;
		(tb, a_in, a_out, b_in, b_out)
	};
	if a_out != b_in {
		return Err("roll edit: the clips do not share a boundary".to_string());
	}
	let new = ts_to_rational(new_frame, tb);
	if new <= a_in || new >= b_out {
		return Err("roll edit: boundary would escape the adjacent clips".to_string());
	}
	if new == a_out {
		// Boundary didn't move — nothing to do.
		return Ok(());
	}
	let mut cmd = oak_timeline::undopointer::BlockTrimCommand::new(
		node_ref(p, track),
		node_ref(p, a),
		new - a_in,
		oak_timeline::common::MovementMode::TrimOut,
	);
	cmd.set_trim_is_a_roll_edit(true);
	cmd.prepare();
	push(cmd.to_command(), "Roll Edit")
}

/// Slide `clip` so its in point becomes `new_start` (undoable "Slide
/// Clip"): the clip's own media window and length are untouched — the
/// in point moves and the out follows — while the adjacent blocks are
/// shortened/lengthened to fill the gap or absorb the overlap (the left
/// neighbor's out moves to `new_start`, the right neighbor's in follows
/// the clip's new out). A neighbor that would collapse to a negative
/// length is left alone, leaving the hole open.
pub fn slide_clip(p: &ProjectRef, clip: NodeId, new_start: i64) -> Result<(), String> {
	let (tb, old_in, old_out, left, right) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (in_r, out_r, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		let track =
			clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?;
		let blocks = track_behavior(&g.graph, track)
			.map(|t| t.blocks.clone())
			.unwrap_or_default();
		let index = blocks
			.iter()
			.position(|&b| b == clip)
			.ok_or_else(|| "the clip is not on its track".to_string())?;
		(
			tb,
			in_r,
			out_r,
			blocks.get(index.wrapping_sub(1)).copied(),
			blocks.get(index + 1).copied(),
		)
	};
	let new = ts_to_rational(new_start.max(0), tb);
	let delta = new - old_in;
	if delta.is_null() {
		return Ok(());
	}
	let clip_len = old_out - old_in;
	let mut children = Vec::new();
	// The clip itself slides: in moves, length and media window stay put
	// (`set_in` keeps the length and never touches the media in-point).
	{
		let c_ref = node_ref(p, clip);
		let (r1, r2) = (c_ref.clone(), c_ref);
		children.push(oak_undo::undocommand::UndoCommand::from_closures(
			move || block_set_in(&r1, new),
			move || block_set_in(&r2, old_in),
		));
	}
	// Left neighbor: its out follows the clip's new in (in anchored,
	// media untouched). A neighbor that would collapse to a negative
	// length is left alone, leaving the hole open.
	if let Some(l) = left {
		let l_ref = node_ref(p, l);
		let l_len = block_length(&l_ref);
		if new > block_in(&l_ref) {
			let (r1, r2) = (l_ref.clone(), l_ref);
			children.push(oak_undo::undocommand::UndoCommand::from_closures(
				move || block_set_length_and_media_in(&r1, l_len + delta),
				move || block_set_length_and_media_in(&r2, l_len),
			));
		}
	}
	// Right neighbor: its in follows the clip's new out (out anchored,
	// media untouched); skipped when it would collapse to a negative
	// length.
	if let Some(r) = right {
		let r_ref = node_ref(p, r);
		let r_len = block_length(&r_ref);
		if new + clip_len < block_out(&r_ref) {
			let (r1, r2) = (r_ref.clone(), r_ref);
			children.push(oak_undo::undocommand::UndoCommand::from_closures(
				move || block_set_length_and_media_out(&r1, r_len - delta),
				move || block_set_length_and_media_out(&r2, r_len),
			));
		}
	}
	push_multi(children, "Slide Clip")
}

/// Slip `clip` so its media in-point becomes `new_media_in` (undoable
/// "Slip Clip"): the timeline range and length stay put, only the media
/// window slides inside the clip (clamped to frame 0).
pub fn slip_clip(p: &ProjectRef, clip: NodeId, new_media_in: i64) -> Result<(), String> {
	let (tb, old_media_in) = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (_, _, media_in) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		(tb, media_in)
	};
	let new = ts_to_rational(new_media_in.max(0), tb);
	if new == old_media_in {
		return Ok(());
	}
	let c_ref = node_ref(p, clip);
	let (r1, r2) = (c_ref.clone(), c_ref);
	push(
		oak_undo::undocommand::UndoCommand::from_closures(
			move || clip_set_media_in(&r1, new),
			move || clip_set_media_in(&r2, old_media_in),
		),
		"Slip Clip",
	)
}

/// The undoable same-track move command for one clip (its in point becomes
/// `new_in_ts`; the module's `TrackMoveBlockCommand` — the old spot becomes
/// a gap, length and media-in are preserved).
fn move_clip_command(
	p: &ProjectRef,
	clip: NodeId,
	new_in_ts: i64,
) -> Result<oak_undo::undocommand::UndoCommand, String> {
	let (tb, list, track_index) = {
		let g = lock(p);
		let track =
			clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?;
		let list = track_behavior(&g.graph, track)
			.and_then(|t| t.track_list)
			.ok_or_else(|| "the clip's track has no list".to_string())?;
		let track_index = track_behavior(&g.graph, track)
			.map(|t| t.index)
			.unwrap_or(0);
		let tb = track_list_behavior(&g.graph, list)
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the sequence has no valid frame rate".to_string())?;
		(tb, list, track_index)
	};
	Ok(oak_timeline::undopointer::TrackMoveBlockCommand::new(
		node_ref(p, list),
		track_index,
		node_ref(p, clip),
		ts_to_rational(new_in_ts, tb),
	)
	.to_command())
}

/// Move `clip` within its track so its in point becomes `new_in_ts`
/// (undoable "Move Clip"; the module's `TrackMoveBlockCommand` — the old
/// spot becomes a gap, length and media-in are preserved). A negative
/// target clamps to frame 0 (the NLE drop-past-the-start behavior).
pub fn move_clip(p: &ProjectRef, clip: NodeId, new_in_ts: i64) -> Result<(), String> {
	push(move_clip_command(p, clip, new_in_ts.max(0))?, "Move Clip")
}

/// The undoable cross-track move commands for one clip (gap on the source
/// track, re-homed in point, place on the destination track), WITHOUT
/// pushing — assembled by callers that move several clips in one undoable
/// entry.
fn move_clip_to_track_commands(
	p: &ProjectRef,
	clip: NodeId,
	dest_track: NodeId,
	new_in_ts: i64,
) -> Result<Vec<oak_undo::undocommand::UndoCommand>, String> {
	let (tb, list, dest_index, source_track) = {
		let g = lock(p);
		let source_track =
			clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?;
		let list = track_behavior(&g.graph, dest_track)
			.and_then(|t| t.track_list)
			.ok_or_else(|| "the destination track has no list".to_string())?;
		let dest_index = track_behavior(&g.graph, dest_track)
			.map(|t| t.index)
			.unwrap_or(0);
		let tb = track_list_behavior(&g.graph, list)
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the sequence has no valid frame rate".to_string())?;
		(tb, list, dest_index, source_track)
	};
	let in_r = ts_to_rational(new_in_ts, tb);

	let gap = oak_timeline::undogeneral::TrackReplaceBlockWithGapCommand::new(
		node_ref(p, source_track),
		node_ref(p, clip),
		true,
	)
	.to_command();
	// The module stores a block's position on the block itself, so the
	// place command alone would keep the old in point: re-home it between
	// the gap and the place (the facade's BlockInCmdData step).
	let old_in = {
		let g = lock(p);
		clip_range(&g.graph, clip)
			.map(|(in_r, _, _)| in_r)
			.ok_or_else(|| "the node is not a clip".to_string())?
	};
	let (p1, p2) = (p.clone(), p.clone());
	let rehome = oak_undo::undocommand::UndoCommand::from_closures(
		move || {
			let mut g = lock(&p1);
			if let Some(c) = g
				.graph
				.get_mut(clip)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			{
				c.core.set_in(in_r);
			}
		},
		move || {
			let mut g = lock(&p2);
			if let Some(c) = g
				.graph
				.get_mut(clip)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			{
				c.core.set_in(old_in);
			}
		},
	);
	let place = oak_timeline::undopointer::TrackPlaceBlockCommand::new(
		node_ref(p, list),
		dest_index,
		node_ref(p, clip),
		in_r,
	)
	.to_command();
	Ok(vec![gap, rehome, place])
}

/// Move `clip` to a different track at `new_in_ts` (undoable "Move Clip
/// to Track", one row): the source spot becomes a gap, the block's in
/// point is re-homed, and the clip is placed on the destination track
/// (the facade's `oakengine_sequence_move_clip_to_track` composition).
pub fn move_clip_to_track(
	p: &ProjectRef,
	clip: NodeId,
	dest_track: NodeId,
	new_in_ts: i64,
) -> Result<(), String> {
	push_multi(
		move_clip_to_track_commands(p, clip, dest_track, new_in_ts.max(0))?,
		"Move Clip to Track",
	)
}

/// Move `clip` to `new_in_ts` (`dest_track` when the gesture crosses
/// tracks) while every clip in `linked` follows in lockstep: each linked
/// clip keeps its own track and moves by the same frame offset. The whole
/// group lands as ONE undoable "Move Clip" entry (C++ `block_links_`
/// semantics — grouped edits apply to the whole group). The shared delta
/// is clamped so no clip of the group lands before frame 0 (a drag past
/// the timeline start pins the group at 0 instead of failing).
pub fn move_clip_with_links(
	p: &ProjectRef,
	clip: NodeId,
	dest_track: Option<NodeId>,
	new_in_ts: i64,
	linked: &[NodeId],
) -> Result<(), String> {
	// The dragged clip's old in point frames the shared frame delta.
	let old_in_ts = {
		let g = lock(p);
		let tb = clip_track(&g.graph, clip)
			.and_then(|t| track_behavior(&g.graph, t))
			.and_then(|t| t.track_list)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.and_then(|l| l.sequence)
			.and_then(|s| sequence_time_base(&g.graph, s))
			.ok_or_else(|| "the clip's sequence has no valid frame rate".to_string())?;
		let (in_r, _, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		rational_to_ts(in_r, tb)
	};
	// The linked clips' current in points (each stays on its own track;
	// only its in point follows the shared delta). A linked clip that is
	// off any track (e.g. ripple-removed) is left in place instead of
	// failing the whole move.
	let mut linked_ins: Vec<(NodeId, i64)> = Vec::new();
	for &other in linked {
		if other == clip {
			continue;
		}
		let other_in_ts = {
			let g = lock(p);
			let Some(track) = clip_track(&g.graph, other) else {
				continue;
			};
			let tb = track_behavior(&g.graph, track)
				.and_then(|t| t.track_list)
				.and_then(|l| track_list_behavior(&g.graph, l))
				.and_then(|l| l.sequence)
				.and_then(|s| sequence_time_base(&g.graph, s))
				.ok_or_else(|| "a linked clip's sequence has no valid frame rate".to_string())?;
			let (in_r, _, _) = clip_range(&g.graph, other)
				.ok_or_else(|| "a linked node is not a clip".to_string())?;
			rational_to_ts(in_r, tb)
		};
		linked_ins.push((other, other_in_ts));
	}
	// Group-aware clamp: the shared delta may not push ANY clip of the
	// group below frame 0 (per-clip clamping would silently de-sync the
	// group).
	let min_in = linked_ins
		.iter()
		.map(|(_, ts)| *ts)
		.fold(old_in_ts, i64::min);
	let delta = (new_in_ts - old_in_ts).max(-min_in);
	let new_in_ts = old_in_ts + delta;

	let mut commands = Vec::new();
	match dest_track {
		Some(track) => commands.extend(move_clip_to_track_commands(p, clip, track, new_in_ts)?),
		None => commands.push(move_clip_command(p, clip, new_in_ts)?),
	}
	for (other, other_in_ts) in linked_ins {
		commands.push(move_clip_command(p, other, other_in_ts + delta)?);
	}
	push_multi(commands, "Move Clip")
}

/// Link or unlink a set of clips as ONE undoable entry (the C++
/// `TimelineWidget::toggle_links_on_selected` → `oakengine_clip_set_linked`
/// composition): linking connects every pair of the set, unlinking clears
/// every link among them. The undo restores the exact prior topology among
/// the set (links to nodes OUTSIDE the set are untouched, like the C++
/// command's).
pub fn set_clips_linked(p: &ProjectRef, blocks: &[NodeId], linked: bool) -> Result<(), String> {
	if blocks.len() < 2 {
		return Ok(());
	}
	let blocks: Vec<NodeId> = blocks.to_vec();
	// Snapshot the prior links among the set (the undo's target state).
	let prior: Vec<(NodeId, NodeId)> = {
		let g = lock(p);
		let mut v = Vec::new();
		for (i, &a) in blocks.iter().enumerate() {
			for &b in &blocks[i + 1..] {
				if g.graph.links_of(a).contains(&b) {
					v.push((a, b));
				}
			}
		}
		v
	};
	let all_pairs: Vec<(NodeId, NodeId)> = {
		let mut v = Vec::new();
		for (i, &a) in blocks.iter().enumerate() {
			for &b in &blocks[i + 1..] {
				v.push((a, b));
			}
		}
		v
	};
	// The shared mutation: clear the set's internal links, then restore the
	// target topology (redo: all pairs when linking, none when unlinking;
	// undo: the snapshot).
	fn apply(p: &ProjectRef, blocks: &[NodeId], target: &[(NodeId, NodeId)]) {
		let mut g = lock(p);
		for (i, &a) in blocks.iter().enumerate() {
			for &b in &blocks[i + 1..] {
				g.graph.unlink(a, b);
			}
		}
		for &(a, b) in target {
			g.graph.link(a, b);
		}
	}
	let redo_target = if linked { all_pairs } else { Vec::new() };
	let (p1, p2) = (p.clone(), p.clone());
	let (b1, b2) = (blocks.clone(), blocks);
	push_command(
		oak_undo::undocommand::UndoCommand::from_closures(
			move || apply(&p1, &b1, &redo_target),
			move || apply(&p2, &b2, &prior),
		),
		if linked { "Link Clips" } else { "Unlink Clips" },
	)
}

/// Delete `clip` leaving a gap (undoable "Delete Clips"; the facade's
/// single-clip `oakengine_sequence_delete_clips` composition).
pub fn delete_clip(p: &ProjectRef, clip: NodeId) -> Result<(), String> {
	let source_track = {
		let g = lock(p);
		clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?
	};
	let gap = oak_timeline::undogeneral::TrackReplaceBlockWithGapCommand::new(
		node_ref(p, source_track),
		node_ref(p, clip),
		true,
	)
	.to_command();
	let remove = oak_task::nodeops::remove_node_command(p.clone(), clip);
	push_multi(vec![gap, remove], "Delete Clips")
}

/// Delete `clip` and ripple the following content left (undoable
/// "Ripple Delete Clip"; the module's `TrackRippleRemoveAreaCommand`
/// over the clip's range).
pub fn ripple_delete_clip(p: &ProjectRef, clip: NodeId) -> Result<(), String> {
	let (track, range) = {
		let g = lock(p);
		let track =
			clip_track(&g.graph, clip).ok_or_else(|| "the clip is not on a track".to_string())?;
		let (in_r, out_r, _) =
			clip_range(&g.graph, clip).ok_or_else(|| "the node is not a clip".to_string())?;
		(track, TimeRange::new(in_r, out_r))
	};
	push(
		oak_timeline::undoripple::TrackRippleRemoveAreaCommand::new(node_ref(p, track), range)
			.to_command(),
		"Ripple Delete Clip",
	)
}

/// Undoable marker add ("Add Marker"). `time` is a rational seconds
/// in-point; a marker at the same time is rejected (the engine's marker
/// insertion asserts on duplicate times).
pub fn marker_add(markers: &CHandle, time: Rational, name: &str, color: i32) -> Result<(), String> {
	if marker_index_at(markers, time).is_some() {
		return Err("a marker already exists at that time".to_string());
	}
	push(
		oak_timeline::marker::MarkerAddCommand::new(
			*markers,
			TimeRange::new(time, time),
			name,
			color,
		)
		.to_command(),
		"Add Marker",
	)
}

/// Undoable marker remove ("Remove Marker"); `None`-equivalent when no
/// marker sits at `time` (the caller treats it as a benign no-op).
pub fn marker_remove(markers: &CHandle, time: Rational) -> Result<(), String> {
	let Some(index) = marker_index_at(markers, time) else {
		return Err("no marker at that time".to_string());
	};
	push(
		oak_timeline::marker::MarkerRemoveCommand::new(*markers, index).to_command(),
		"Remove Marker",
	)
}

/// Undoable workarea set ("Set Workarea"): the enabled flag plus the
/// in/out range as ONE entry. `old_range` is supplied by the caller (the
/// range read before the change — e.g. the drag-start range of a ruler
/// work-area drag); the enabled flag's previous value is captured by the
/// module command itself.
pub fn workarea_set_undoable(
	wa: &CHandle,
	enabled: bool,
	range: TimeRange,
	old_range: TimeRange,
) -> Result<(), String> {
	push_multi(
		vec![
			oak_timeline::workarea::WorkareaSetEnabledCommand::new(*wa, enabled).to_command(),
			oak_timeline::workarea::WorkareaSetRangeCommand::new_with_old(*wa, range, old_range)
				.to_command(),
		],
		"Set Workarea",
	)
}

/// Undoable removal of `node` from the project graph ("Remove Node"; the
/// module's remove command drops incident edges and restores the entry on
/// undo). The sequence node itself (the graph's output) cannot be
/// removed.
pub fn remove_node(p: &ProjectRef, node: NodeId) -> Result<(), String> {
	push(
		oak_timeline::undocommon::create_remove_command(&node_ref(p, node)),
		"Remove Node",
	)
}

// ---------------------------------------------------------------------------
// Test serialization
// ---------------------------------------------------------------------------

/// A process-wide test lock: the app's tests share the oakundo global
/// stack, the oak_core config store and the codec decode sessions, so any
/// test touching them serializes on this lock.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Clipboard (Cut / Copy / Paste)
// ---------------------------------------------------------------------------

/// One clip captured in the clipboard (Cut/Copy). Everything needed to
/// re-place it on a timeline is here; effects/multicam contexts are not
/// modeled yet (footage clips carry their block core only).
#[derive(Clone, Debug)]
pub struct ClipboardClip {
	/// The footage node the clip decodes.
	pub footage: NodeId,
	/// Track type the clip was on.
	pub kind: TrackType,
	/// Per-type index of the track it was on (re-used on paste when the
	/// track still exists).
	pub track_index: usize,
	/// Media in-point in frame timestamps.
	pub media_in_ts: i64,
	/// Timeline in-point in frame timestamps.
	pub start_ts: i64,
	/// Duration in frame timestamps.
	pub length_ts: i64,
	/// Playback speed.
	pub speed: f64,
}

/// Capture the selected clips into clipboard form (`Copy`; `Cut` copies
/// then deletes). Clips without a footage upstream are skipped.
pub fn copy_clips(p: &ProjectRef, clips: &[NodeId]) -> Vec<ClipboardClip> {
	let g = lock(p);
	let mut out = Vec::new();
	for &clip in clips {
		let Some(footage) = find_input_footage(&g.graph, clip) else {
			continue;
		};
		let Some((in_r, out_r, media_in)) = clip_range(&g.graph, clip) else {
			continue;
		};
		let Some(track) = clip_track(&g.graph, clip) else {
			continue;
		};
		let Some(t) = track_behavior(&g.graph, track) else {
			continue;
		};
		let Some(list) = t.track_list.and_then(|l| track_list_behavior(&g.graph, l)) else {
			continue;
		};
		let (tb_num, tb_den) = {
			// Frame timestamps are stored rationally; the clipboard keeps
			// the sequence timebase for exactness.
			let seq = list.sequence.and_then(|s| sequence_time_base(&g.graph, s));
			seq.unwrap_or((1, 25))
		};
		let to_ts = |r: Rational| {
			(r.numerator() * tb_den)
				.checked_div(r.denominator() * tb_num)
				.unwrap_or(0)
		};
		let speed = clip_behavior(&g.graph, clip)
			.map(|c| c.core.speed)
			.unwrap_or(1.0);
		out.push(ClipboardClip {
			footage,
			kind: list.kind,
			track_index: t.index.max(0) as usize,
			media_in_ts: to_ts(media_in),
			start_ts: to_ts(in_r),
			length_ts: to_ts(out_r - in_r),
			speed,
		});
	}
	out.sort_by_key(|c| c.start_ts);
	out
}

/// Paste clipboard clips at `playhead_ts` (frame timestamps), undoable
/// "Paste". The first clip's in-point moves to the playhead; the others
/// keep their relative offsets. Tracks are reused by kind+index (falling
/// back to the last track of the kind). Clips that were linked stay
/// linked inside the pasted group.
pub fn paste_clips(
	p: &ProjectRef,
	seq: NodeId,
	items: &[ClipboardClip],
	playhead_ts: i64,
) -> Result<Vec<NodeId>, String> {
	if items.is_empty() {
		return Err("the clipboard is empty".to_string());
	}
	let anchor = items[0].start_ts;
	let mut clips = Vec::with_capacity(items.len());
	for item in items {
		let start = playhead_ts + (item.start_ts - anchor);
		let out = start + item.length_ts;
		// Re-use the placement machinery one clip at a time, collecting
		// the commands so everything lands in ONE undo entry.
		clips.push((item, start, out));
	}

	let tb = {
		let g = lock(p);
		sequence_time_base(&g.graph, seq)
			.ok_or_else(|| "sequence has no valid frame rate".to_string())?
	};
	let track_count_of = |kind: TrackType| -> usize {
		let g = lock(p);
		track_list_of(&g.graph, seq, kind)
			.and_then(|l| track_list_behavior(&g.graph, l))
			.map(|l| l.tracks.len())
			.unwrap_or(0)
	};

	let mut new_ids = Vec::with_capacity(items.len());
	let mut commands = Vec::new();
	for (item, start, out) in clips {
		let list = {
			let g = lock(p);
			track_list_of(&g.graph, seq, item.kind)
				.ok_or_else(|| "sequence has no track list for this type".to_string())?
		};
		let count = track_count_of(item.kind);
		if count == 0 {
			return Err("no track of the clip's kind to paste onto".to_string());
		}
		let track_index = item.track_index.min(count - 1);
		let in_r = ts_to_rational(start, tb);
		let media_r = ts_to_rational(item.media_in_ts, tb);
		let length_r = ts_to_rational(out, tb) - in_r;

		let clip = {
			let mut g = lock(p);
			let id = create_footage_clip(&mut g.graph, item.footage, media_r, length_r);
			if let Some(c) = g
				.graph
				.get_mut(id)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			{
				c.core.speed = item.speed;
			}
			id
		};
		new_ids.push(clip);
		commands.push(
			oak_timeline::undopointer::TrackPlaceBlockCommand::new(
				node_ref(p, list),
				track_index as i32,
				node_ref(p, clip),
				in_r,
			)
			.to_command(),
		);
		commands.push(connect_command(
			p,
			item.footage,
			clip,
			oak_node::block::clip_input::TEXTURE_INPUT,
		)?);
	}
	// Link the pasted group both ways via the graph API, so the links land
	// on NodeCore.links where links_of/are_linked read them (the C++ pastes
	// linked selections as a linked group).
	if new_ids.len() > 1 {
		let first = new_ids[0];
		for &other in &new_ids[1..] {
			let (p1, p2) = (p.clone(), p.clone());
			commands.push(oak_undo::undocommand::UndoCommand::from_closures(
				move || {
					lock(&p1).graph.link(first, other);
				},
				move || {
					lock(&p2).graph.unlink(first, other);
				},
			));
		}
	}
	push_multi(commands, "Paste")?;
	Ok(new_ids)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	/// A project with one sequence holding one track of `kind`; returns the
	/// project, the sequence and the track's node id.
	fn project_with_track(kind: TrackType) -> (ProjectRef, NodeId, NodeId) {
		let project = create_project();
		let seq = create_sequence(&project, "Seq");
		let index = add_track(&project, seq, kind).expect("add a track");
		let track = {
			let g = lock(&project);
			track_ids(&g.graph, seq, kind)[index]
		};
		(project, seq, track)
	}

	/// The track flag setters flip the flag as ONE undoable entry each;
	/// undo restores the previous value and redo re-applies.
	#[test]
	fn track_flag_setters_toggle_and_undo() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let (project, _seq, track) = project_with_track(TrackType::Video);

		assert_eq!(track_muted(&project, track), Some(false));
		assert_eq!(track_locked(&project, track), Some(false));

		set_track_muted(&project, track, true).expect("mute the track");
		assert_eq!(track_muted(&project, track), Some(true));
		set_track_locked(&project, track, true).expect("lock the track");
		assert_eq!(track_locked(&project, track), Some(true));

		oak_undo::global::undo().unwrap();
		assert_eq!(track_locked(&project, track), Some(false));
		oak_undo::global::undo().unwrap();
		assert_eq!(track_muted(&project, track), Some(false));
		oak_undo::global::redo().unwrap();
		assert_eq!(track_muted(&project, track), Some(true));
		oak_undo::global::clear().unwrap();
	}

	/// Setting a flag to its current value pushes no undo row.
	#[test]
	fn track_flag_setter_noop_when_unchanged() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let (project, _seq, track) = project_with_track(TrackType::Audio);
		let before = oak_undo::global::count().unwrap();

		set_track_muted(&project, track, false).expect("mute already false");
		set_track_locked(&project, track, false).expect("lock already false");
		assert_eq!(oak_undo::global::count().unwrap(), before);
		oak_undo::global::clear().unwrap();
	}

	/// Undo/redo stability: undo → redo → undo → redo must converge to the
	/// SAME graph state every cycle (the user's "撤销再前进再撤销再前进，
	/// 结果居然变了" regression). Snapshot the sequence's track blocks and
	/// the graph node count around two full cycles.
	#[test]
	fn undo_redo_cycles_converge_to_the_same_state() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Undo Cycles");
		let media = std::env::temp_dir().join(format!("oak_undo_cycle_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement");

		let snapshot = |p: &ProjectRef| -> (usize, Vec<(TrackType, Vec<usize>)>) {
			let g = lock(p);
			let node_count = g.graph.node_count();
			let mut tracks = Vec::new();
			for kind in [TrackType::Video, TrackType::Audio] {
				let blocks: Vec<usize> = track_ids(&g.graph, seq, kind)
					.iter()
					.map(|t| {
						track_behavior(&g.graph, *t)
							.map(|t| t.blocks.len())
							.unwrap_or(0)
					})
					.collect();
				tracks.push((kind, blocks));
			}
			(node_count, tracks)
		};

		let before = snapshot(&project);
		// Two full undo/redo cycles; the state must be identical after
		// every redo and match the pre-undo state after every undo.
		for cycle in 0..2 {
			oak_undo::global::undo().expect("undo");
			let g = lock(&project);
			let video_blocks: usize = track_ids(&g.graph, seq, TrackType::Video)
				.iter()
				.map(|t| {
					track_behavior(&g.graph, *t)
						.map(|t| t.blocks.len())
						.unwrap_or(0)
				})
				.sum();
			assert_eq!(video_blocks, 0, "cycle {cycle}: undo removes the clips");
			drop(g);
			oak_undo::global::redo().expect("redo");
			let state = snapshot(&project);
			assert_eq!(
				state, before,
				"cycle {cycle}: redo must restore the exact state"
			);
		}
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// A stale (non-track) id is rejected, not silently ignored.
	#[test]
	fn track_flag_setters_reject_non_tracks() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let (project, seq, _track) = project_with_track(TrackType::Video);
		assert!(set_track_muted(&project, seq, true).is_err());
		assert!(set_track_locked(&project, seq, true).is_err());
		oak_undo::global::clear().unwrap();
	}

	/// Every clip is stamped at creation with the footage's name as its
	/// label and a color index fixed for its whole life — adding or
	/// removing other clips must not reshuffle it (the old
	/// track-position-derived color did).
	#[test]
	fn clips_get_a_creation_time_label_and_stable_color() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Clip Color");
		let media = std::env::temp_dir().join(format!("oak_clip_color_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		let name = media.file_name().unwrap().to_string_lossy().into_owned();

		let color_of = |p: &ProjectRef, id: NodeId| -> i32 {
			let g = lock(p);
			g.graph
				.get(id)
				.expect("the clip node exists")
				.core
				.override_color
		};

		// Label = the footage name; the color is locked in at creation.
		let first = place_footage_clip(&project, seq, footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place first");
		let color1 = color_of(&project, first);
		assert_ne!(color1, -1, "the clip color is locked in at creation");
		{
			let g = lock(&project);
			assert_eq!(
				g.graph.get(first).unwrap().core.label,
				name,
				"the clip is named after its footage"
			);
		}

		// Adding another clip must not reshuffle the first one's color.
		let _second = place_footage_clip(&project, seq, footage, TrackType::Video, 0, 10, 20, 0)
			.expect("place second");
		assert_eq!(
			color_of(&project, first),
			color1,
			"the color survives later edits"
		);

		// The linked A/V drop names both clips after the footage too.
		let linked = place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			20,
			30,
			0,
		)
		.expect("linked placement");
		for clip in linked {
			let g = lock(&project);
			assert_eq!(g.graph.get(clip).unwrap().core.label, name);
		}

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// Paste regression: the pasted group's links must land on
	/// NodeCore.links (where links_of/are_linked read them), not on the
	/// parallel BlockCore.links mirror the graph API never sees. Paste a
	/// copied linked A/V pair and require links_of to find the link; undo
	/// clears it, redo restores it.
	#[test]
	fn paste_links_the_group_in_node_core_links() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Paste Links");
		let media =
			std::env::temp_dir().join(format!("oak_paste_links_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		let dropped = place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement");

		let items = copy_clips(&project, &dropped);
		assert_eq!(items.len(), 2, "both clips copied");
		let pasted = paste_clips(&project, seq, &items, 20).expect("paste");
		assert_eq!(pasted.len(), 2);
		{
			let g = lock(&project);
			assert!(
				g.graph.links_of(pasted[0]).contains(&pasted[1]),
				"links_of finds the pasted link (the wrong-field regression)"
			);
			assert!(g.graph.are_linked(pasted[1], pasted[0]));
		}

		// Undo unlinks the group; redo relinks it.
		oak_undo::global::undo().expect("undo paste");
		{
			let g = lock(&project);
			assert!(
				!g.graph.are_linked(pasted[0], pasted[1]),
				"undo unlinks the pair"
			);
		}
		oak_undo::global::redo().expect("redo paste");
		{
			let g = lock(&project);
			assert!(
				g.graph.are_linked(pasted[0], pasted[1]),
				"redo relinks the pair"
			);
		}
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// Delete-one-of-a-pair regression: deleting a linked clip removes the
	/// survivor's back-reference (take_node cleans up symmetrically), so
	/// moving the survivor afterwards neither trips over a stale id nor
	/// fails the whole move.
	#[test]
	fn deleting_one_of_a_linked_pair_clears_the_partner_link() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Delete Linked");
		let media =
			std::env::temp_dir().join(format!("oak_delete_linked_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		let dropped = place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement");
		let (video, audio) = (dropped[0], dropped[1]);

		delete_clip(&project, video).expect("delete the video clip");
		{
			let g = lock(&project);
			assert!(
				!g.graph.links_of(audio).contains(&video),
				"the survivor holds no stale reference to the deleted clip"
			);
		}

		// Dragging the survivor works — even when the stale id is still
		// handed in as a linked partner (the old whole-move failure).
		move_clip_with_links(&project, audio, None, 30, &[video]).expect("move the survivor");
		{
			let g = lock(&project);
			let tb = sequence_time_base(&g.graph, seq).expect("a valid frame rate");
			let (in_r, _, _) = clip_range(&g.graph, audio).expect("audio is still a clip");
			assert_eq!(rational_to_ts(in_r, tb), 30, "the survivor moved");
		}
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// Incremental link-undo regression: undoing the A/V drop removes ONLY
	/// the links the drop created — a link added afterwards (here: the
	/// video clip to a third clip) survives (the old undo restored the
	/// whole links vector from a construction-time snapshot and clobbered
	/// it).
	#[test]
	fn drop_undo_keeps_links_created_after_the_drop() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Drop Undo Links");
		let media = std::env::temp_dir().join(format!("oak_drop_undo_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		let dropped = place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement");
		let (video, audio) = (dropped[0], dropped[1]);

		// A third clip, linked to the dropped video clip AFTER the drop.
		let third = place_footage_clip(&project, seq, footage, TrackType::Video, 0, 20, 30, 0)
			.expect("place third");
		{
			let mut g = lock(&project);
			assert!(g.graph.link(video, third), "the third link is established");
		}

		// Undo twice: the third clip's placement, then the drop. Neither
		// may touch the video<->third link.
		oak_undo::global::undo().expect("undo the third placement");
		oak_undo::global::undo().expect("undo the drop");
		{
			let g = lock(&project);
			assert!(
				g.graph.links_of(video).contains(&third),
				"the later link survives the drop's undo"
			);
			assert!(g.graph.links_of(third).contains(&video));
			assert!(
				!g.graph.links_of(video).contains(&audio),
				"the drop's own link is removed"
			);
			assert!(!g.graph.links_of(audio).contains(&video));
		}
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// Off-track linked clip regression: a linked clip that was
	/// ripple-removed from its track (track = None) is SKIPPED by a group
	/// move instead of failing the whole move — the on-track clip moves
	/// and the off-track one keeps its range.
	#[test]
	fn moving_a_linked_clip_skips_the_off_track_partner() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Off Track Move");
		let media = std::env::temp_dir().join(format!("oak_off_track_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");
		let footage = import_footage(&project, &media).expect("import");
		let dropped = place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement");
		let (video, audio) = (dropped[0], dropped[1]);

		// Ripple-remove the audio clip from its track: the node stays in
		// the graph, its links intact, but it is off any track.
		let audio_track = {
			let g = lock(&project);
			clip_track(&g.graph, audio).expect("audio starts on a track")
		};
		oak_timeline::util::track_ripple_remove_block(
			&node_ref(&project, audio_track),
			&node_ref(&project, audio),
		);
		{
			let g = lock(&project);
			assert!(
				clip_track(&g.graph, audio).is_none(),
				"audio is off-track now"
			);
			assert!(g.graph.are_linked(video, audio), "the link itself survives");
		}

		// Moving the video clip succeeds and moves ONLY the on-track clip.
		move_clip_with_links(&project, video, None, 40, &[audio]).expect("the move succeeds");
		{
			let g = lock(&project);
			let tb = sequence_time_base(&g.graph, seq).expect("a valid frame rate");
			let (v_in, _, _) = clip_range(&g.graph, video).expect("video is a clip");
			assert_eq!(rational_to_ts(v_in, tb), 40, "the on-track clip moved");
			let (a_in, _, _) = clip_range(&g.graph, audio).expect("audio is a clip");
			assert_eq!(rational_to_ts(a_in, tb), 0, "the off-track clip stayed put");
		}
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}
}

#[cfg(test)]
mod undo_cycle_track_tests {
	use super::*;

	/// add_track undo/redo cycles must converge (the track count AND the
	/// created track's identity stay stable; the C++ remove-last undo must
	/// not eat a default track).
	#[test]
	fn add_track_undo_redo_cycles_converge() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Add Track Cycles");
		let count_of = |p: &ProjectRef, kind: TrackType| {
			let g = lock(p);
			track_ids(&g.graph, seq, kind).len()
		};
		assert_eq!(
			count_of(&project, TrackType::Video),
			2,
			"default 2 video tracks"
		);

		let index = add_track(&project, seq, TrackType::Video).expect("add a track");
		assert_eq!(index, 2, "the new track is the third video track");
		let track_id = {
			let g = lock(&project);
			track_ids(&g.graph, seq, TrackType::Video)[index]
		};

		for cycle in 0..3 {
			oak_undo::global::undo().expect("undo");
			assert_eq!(
				count_of(&project, TrackType::Video),
				2,
				"cycle {cycle}: undo removes only the added track"
			);
			oak_undo::global::redo().expect("redo");
			assert_eq!(
				count_of(&project, TrackType::Video),
				3,
				"cycle {cycle}: redo restores the added track"
			);
			let g = lock(&project);
			assert!(
				g.graph.is_valid(track_id),
				"cycle {cycle}: the same track node is back in the graph"
			);
			assert_eq!(
				track_ids(&g.graph, seq, TrackType::Video)[index],
				track_id,
				"cycle {cycle}: the track keeps its identity and position"
			);
			drop(g);
		}
		oak_undo::global::clear().unwrap();
	}
}

#[cfg(test)]
mod undo_cycle_ops_tests {
	use super::*;

	/// Full-graph snapshot for convergence checks: node count plus, for
	/// every track, the ordered block ids and each block's range.
	fn snapshot(p: &ProjectRef, seq: NodeId) -> (usize, Vec<(NodeId, Vec<(NodeId, i128, i128)>)>) {
		let g = lock(p);
		let mut tracks = Vec::new();
		for kind in [TrackType::Video, TrackType::Audio] {
			for t in track_ids(&g.graph, seq, kind) {
				let blocks: Vec<(NodeId, i128, i128)> = track_behavior(&g.graph, t)
					.map(|t| {
						t.blocks
							.iter()
							.map(|&b| {
								let (in_r, out_r, _) = clip_range(&g.graph, b).unwrap_or_default();
								(
									b,
									in_r.numerator() as i128 * 1_000_000
										/ in_r.denominator().max(1) as i128,
									out_r.numerator() as i128 * 1_000_000
										/ out_r.denominator().max(1) as i128,
								)
							})
							.collect()
					})
					.unwrap_or_default();
				tracks.push((t, blocks));
			}
		}
		(g.graph.node_count(), tracks)
	}

	fn cycle_assert(
		p: &ProjectRef,
		seq: NodeId,
		post: &(usize, Vec<(NodeId, Vec<(NodeId, i128, i128)>)>),
		what: &str,
	) {
		for cycle in 0..3 {
			oak_undo::global::undo().unwrap_or_else(|e| panic!("{what}: undo failed: {e:?}"));
			oak_undo::global::redo().unwrap_or_else(|e| panic!("{what}: redo failed: {e:?}"));
			let state = snapshot(p, seq);
			assert_eq!(&state, post, "{what}: cycle {cycle} diverged");
		}
	}

	fn project_with_two_clips(media: &std::path::Path) -> (ProjectRef, NodeId, NodeId) {
		let project = create_project();
		let seq = create_sequence(&project, "Cycle Ops");
		let footage = import_footage(&project, media).expect("import");
		place_footage_clips_linked(
			&project,
			seq,
			footage,
			&[(TrackType::Video, 0), (TrackType::Audio, 0)],
			0,
			10,
			0,
		)
		.expect("linked placement")
		.into_iter()
		.next()
		.map(|_| (project.clone(), seq, footage))
		.expect("one clip")
	}

	/// Move / trim / delete / split undo-redo cycles must all converge.
	#[test]
	fn move_trim_delete_split_cycles_converge() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let media = std::env::temp_dir().join(format!("oak_cycle_ops_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate");

		// --- move ---
		let (project, seq, _footage) = project_with_two_clips(&media);
		let clip = {
			let g = lock(&project);
			track_ids(&g.graph, seq, TrackType::Video)
				.iter()
				.find_map(|t| track_behavior(&g.graph, *t).and_then(|t| t.blocks.first().copied()))
				.expect("a clip")
		};
		move_clip(&project, clip, 20).expect("move");
		let post = snapshot(&project, seq);
		cycle_assert(&project, seq, &post, "move");
		oak_undo::global::clear().unwrap();

		// --- trim ---
		trim_clip(&project, clip, 22, 28).expect("trim");
		let post = snapshot(&project, seq);
		cycle_assert(&project, seq, &post, "trim");
		oak_undo::global::clear().unwrap();

		// --- split ---
		split_clip(&project, clip, 25).expect("split");
		let post = snapshot(&project, seq);
		cycle_assert(&project, seq, &post, "split");
		oak_undo::global::clear().unwrap();

		// --- delete ---
		delete_clip(&project, clip).expect("delete");
		let post = snapshot(&project, seq);
		cycle_assert(&project, seq, &post, "delete");
		oak_undo::global::clear().unwrap();

		// --- ripple delete (on a fresh project, then check) ---
		let (project, seq, _footage) = project_with_two_clips(&media);
		let clip = {
			let g = lock(&project);
			track_ids(&g.graph, seq, TrackType::Video)
				.iter()
				.find_map(|t| track_behavior(&g.graph, *t).and_then(|t| t.blocks.first().copied()))
				.expect("a clip")
		};
		ripple_delete_clip(&project, clip).expect("ripple delete");
		let post = snapshot(&project, seq);
		cycle_assert(&project, seq, &post, "ripple delete");
		oak_undo::global::clear().unwrap();

		let _ = std::fs::remove_file(&media);
	}

	// ---- sequence folder mounting ------------------------------------------

	/// The root folder's direct children.
	fn root_children(p: &ProjectRef) -> Vec<NodeId> {
		let guard = lock(p);
		guard
			.graph
			.get(guard.root)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<FolderBehavior>())
			.map(|f| f.children.clone())
			.unwrap_or_default()
	}

	/// Folders and sequences created through the module helpers mount under
	/// the root folder, so the project explorer lists them.
	#[test]
	fn create_folder_and_sequence_mount_under_root() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let folder = create_folder(&project, "Folder 1").expect("create folder");
		let seq = create_sequence_with_params(
			&project,
			"Seq 4K",
			Some((3840, 2160, Rational::new(24000, 1001), true)),
		);
		let children = root_children(&project);
		assert!(children.contains(&folder), "the folder mounts under root");
		assert!(children.contains(&seq), "the sequence mounts under root");
		assert_eq!(
			folder_ids(&lock(&project)).len(),
			2,
			"the root folder itself plus the new folder"
		);
		oak_undo::global::clear().unwrap();
	}

	/// `create_sequence_with_params` writes the format into the sequence's
	/// first video stream, including the new `interlaced` flag.
	#[test]
	fn create_sequence_with_params_sets_first_stream_format() {
		let _g = test_lock();
		let project = create_project();
		let seq = create_sequence_with_params(
			&project,
			"Interlaced",
			Some((1280, 720, Rational::new(30000, 1001), true)),
		);
		let guard = lock(&project);
		let (width, height, rate) = sequence_video_params(&guard.graph, seq).expect("video params");
		assert_eq!((width, height), (1280, 720));
		assert_eq!((rate.numerator(), rate.denominator()), (30000, 1001));
		let interlaced = guard
			.graph
			.get(seq)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
			.expect("sequence behavior")
			.video_params
			.first()
			.expect("a video stream")
			.interlaced;
		assert!(interlaced, "the interlaced flag lands in the first stream");
		oak_undo::global::clear().unwrap();
	}

	/// Sequences saved before they mounted under the root load free-floating;
	/// `ensure_sequences_mounted` reattaches them (the open path's migration).
	#[test]
	fn ensure_sequences_mounted_reattaches_orphans() {
		let _g = test_lock();
		oak_undo::global::clear().unwrap();
		let project = create_project();
		let seq = create_sequence(&project, "Legacy");
		// Detach the sequence from the root: a legacy project loaded without
		// the mount migration.
		{
			let mut guard = lock(&project);
			let root = guard.root;
			let folder = guard
				.graph
				.get_mut(root)
				.and_then(|e| e.behavior.as_any_mut())
				.and_then(|a| a.downcast_mut::<FolderBehavior>())
				.expect("root folder behavior");
			folder.children.retain(|&c| c != seq);
		}
		assert!(
			!root_children(&project).contains(&seq),
			"the sequence floats free after the detach"
		);
		ensure_sequences_mounted(&project);
		assert!(
			root_children(&project).contains(&seq),
			"the orphan sequence reattaches under root"
		);
		oak_undo::global::clear().unwrap();
	}

	/// `set_sequence_parameters` rewrites the display name and the first video
	/// stream's format in one call (the properties dialog's commit path).
	#[test]
	fn set_sequence_parameters_updates_name_and_format() {
		let _g = test_lock();
		let project = create_project();
		let seq = create_sequence_with_params(
			&project,
			"Before",
			Some((1280, 720, Rational::new(25, 1), false)),
		);
		set_sequence_parameters(
			&project,
			seq,
			"After",
			1920,
			1080,
			Rational::new(30000, 1001),
			true,
		)
		.expect("update parameters");
		let guard = lock(&project);
		assert_eq!(node_label(&guard.graph, seq), "After");
		let (width, height, rate) = sequence_video_params(&guard.graph, seq).expect("video params");
		assert_eq!((width, height), (1920, 1080));
		assert_eq!((rate.numerator(), rate.denominator()), (30000, 1001));
		let interlaced = guard
			.graph
			.get(seq)
			.and_then(|e| e.behavior.as_any())
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
			.expect("sequence behavior")
			.video_params
			.first()
			.expect("a video stream")
			.interlaced;
		assert!(interlaced, "the interlaced flag updates too");
		oak_undo::global::clear().unwrap();
	}
}
