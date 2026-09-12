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

//! Module-native engine helpers (M14 R2).
//!
//! oak-cli links the oak* module rlibs directly (oaknode / oaktimeline /
//! oakcodec / oakrender / oaktask / oak_core) instead of the built
//! liboakengine dylib's C ABI. This module is the CLI's own assembly
//! layer: it reproduces the facade operations the subcommands need —
//! project load/create, footage probe, sequence + clip assembly, montage
//! resolution, ticket rendering and the synchronous export path — over
//! the modules' direct Rust APIs.
//!
//! The composition functions here mirror what the facade's C ABI exports
//! (`oakengine_project_load`, `oakengine_sequence_add_footage_clip_ex`,
//! `oakengine_renderer_render_frame`, ...) did over the same module APIs;
//! the facade keeps its own copies for the frozen C ABI. Combinators that
//! belong upstream (the effect-chain and timeline composites) are M14
//! follow-up candidates for `oak_node::ops`; kept local until then.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::{Rational, TimeRange};
use oak_node::block::{self, ClipBlockBehavior};
use oak_node::footage::FootageBehavior;
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use oak_node::value::VideoParams;
use oak_render::manager::RenderManager;
use oak_render::procpool::bgra8_to_rgba8;
use oak_render::ticket::{
    AudioTicketParams, MontageClip, TicketPayload, VideoTicketParams,
};
use oak_timeline::undogeneral::TimelineAddTrackCommand;
use oak_timeline::undopointer::TrackPlaceBlockCommand;
use oak_timeline::util::NodeRef;

/// The shared project reference (the modules' domain project handle).
pub type ProjectRef = Arc<Mutex<Project>>;

/// Lock a project, recovering from a poisoned lock (a panicking command
/// body must not wedge every later edit).
fn lock(p: &ProjectRef) -> MutexGuard<'_, Project> {
	p.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Project load / create
// ---------------------------------------------------------------------------

/// Load a `.ove` project file into a fresh project (the module
/// serializer's `load` owns the graph). The filename is normalized to an
/// absolute path and the modified flag cleared, mirroring the facade's
/// `oakengine_project_load`.
pub fn load_project(path: &str) -> Result<ProjectRef, String> {
	let xml = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
	let project = oak_node::serializer::load(&xml).map_err(|e| e.to_string())?;
	let p = Path::new(path);
	let abs = if p.is_absolute() {
		p.to_path_buf()
	} else {
		std::env::current_dir()
			.map(|d| d.join(p))
			.unwrap_or_else(|_| p.to_path_buf())
	};
	let mut guard = lock(&project);
	guard.set_filename(&abs.to_string_lossy());
	guard.set_modified(false);
	drop(guard);
	Ok(project)
}

/// Project display name (`Project::name`; "(untitled)" when empty).
pub fn project_name(p: &Project) -> String {
	p.name()
}

/// Project file path.
pub fn project_filename(p: &Project) -> String {
	p.filename().to_string()
}

/// Project modified flag.
pub fn project_modified(p: &Project) -> bool {
	p.is_modified()
}

// ---------------------------------------------------------------------------
// Graph walks
// ---------------------------------------------------------------------------

/// Every sequence node in the graph, in arena order (the facade's
/// `oakengine_project_sequence_count`/`sequence_at` walk order).
pub fn sequence_ids(p: &Project) -> Vec<NodeId> {
	p.graph
		.node_ids()
		.into_iter()
		.filter(|&id| seq_behavior(&p.graph, id).is_some())
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

/// Borrow the sequence behavior at `id`.
fn seq_behavior(g: &Graph, id: NodeId) -> Option<&SequenceBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<SequenceBehavior>()
}

/// Borrow the footage behavior at `id`.
fn footage_behavior(g: &Graph, id: NodeId) -> Option<&FootageBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<FootageBehavior>()
}

/// The label of a node (`NodeCore::label`).
pub fn node_label(g: &Graph, id: NodeId) -> String {
	g.get(id).map(|e| e.core.label.clone()).unwrap_or_default()
}

/// The footage filename at `id` (empty when the node is not footage).
pub fn footage_filename(p: &Project, id: NodeId) -> String {
	footage_behavior(&p.graph, id)
		.map(|f| f.filename.clone())
		.unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Sequence queries (info / render)
// ---------------------------------------------------------------------------

/// Sequence content length (seconds rational), 0/1 when unavailable.
pub fn sequence_length(p: &Project, id: NodeId) -> Rational {
	match seq_behavior(&p.graph, id) {
		Some(s) => s.last_length,
		None => Rational::new(0, 1),
	}
}

/// Sequence frame rate (rational), 0/0 (the NULL sentinel) when the
/// sequence has no video params — matching the facade's "0/0" report for
/// such sequences.
pub fn sequence_frame_rate(p: &Project, id: NodeId) -> Rational {
	match seq_behavior(&p.graph, id).and_then(|s| s.video_params.first()) {
		Some(v) => v.frame_rate,
		None => Rational::new(0, 0),
	}
}

/// Sequence output geometry `(width, height)` from its first video stream.
pub fn sequence_geometry(p: &Project, id: NodeId) -> (i32, i32) {
	match seq_behavior(&p.graph, id).and_then(|s| s.video_params.first()) {
		Some(v) => (v.width, v.height),
		None => (0, 0),
	}
}

/// Sequence track counts `(video, audio, subtitle)`.
pub fn sequence_track_counts(p: &Project, id: NodeId) -> (i64, i64, i64) {
	let mut counts = (0i64, 0i64, 0i64);
	let Some(seq) = seq_behavior(&p.graph, id) else {
		return counts;
	};
	for &list_id in &seq.track_lists {
		let Some(list) = track_list_behavior(&p.graph, list_id) else {
			continue;
		};
		let n = list.tracks.len() as i64;
		match list.kind {
			TrackType::Video => counts.0 += n,
			TrackType::Audio => counts.1 += n,
			TrackType::Subtitle => counts.2 += n,
		}
	}
	counts
}

/// Sequence playhead (seconds rational).
pub fn sequence_playhead(p: &Project, id: NodeId) -> Rational {
	match seq_behavior(&p.graph, id) {
		Some(s) => s.playhead,
		None => Rational::new(0, 1),
	}
}

// ---------------------------------------------------------------------------
// Transcode assembly
// ---------------------------------------------------------------------------

/// Create a sequence node in `project` (a scratch project, mirroring the
/// facade's `oakengine_sequence_new` documented deviation) and label it.
pub fn create_sequence(project: &ProjectRef, name: &str) -> NodeId {
	let id = {
		let mut guard = lock(project);
		let (core, behavior) = SequenceBehavior::create();
		guard.graph.add_node(core, behavior)
	};
	{
		let mut guard = lock(project);
		if let Some(e) = guard.graph.get_mut(id) {
			e.core.label = name.to_string();
		}
	}
	id
}

/// Set the sequence's first video stream geometry + frame rate.
pub fn set_sequence_video_params(
	p: &ProjectRef,
	id: NodeId,
	width: i32,
	height: i32,
	fr_num: i32,
	fr_den: i32,
) {
	let mut guard = lock(p);
	if let Some(s) = guard
		.graph
		.get_mut(id)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		if s.video_params.is_empty() {
			s.video_params.push(VideoParams {
				width,
				height,
				frame_rate: Rational::new(i64::from(fr_num), i64::from(fr_den)),
				pixel_format: 4, // f32
				channels: 4,
				interlaced: false,
			});
		} else {
			let v = &mut s.video_params[0];
			v.width = width;
			v.height = height;
			v.frame_rate = Rational::new(i64::from(fr_num), i64::from(fr_den));
		}
	}
}

/// Borrow the track list behavior at `id`.
fn track_list_behavior(g: &Graph, id: NodeId) -> Option<&TrackListBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackListBehavior>()
}

/// Borrow the track behavior at `id`.
fn track_behavior(g: &Graph, id: NodeId) -> Option<&TrackBehavior> {
	g.get(id)?
		.behavior
		.as_any()?
		.downcast_ref::<TrackBehavior>()
}

/// Find (or create) the sequence's track list of `kind` (the facade's
/// `oaknode_sequence_get_track_list` find-or-create semantics).
pub fn find_or_create_track_list(
	p: &ProjectRef,
	seq_id: NodeId,
	kind: TrackType,
) -> Option<NodeId> {
	let mut guard = lock(p);
	// Existing list of the kind.
	for &list_id in seq_behavior(&guard.graph, seq_id)?.track_lists.iter() {
		if track_list_behavior(&guard.graph, list_id).map(|l| l.kind) == Some(kind) {
			return Some(list_id);
		}
	}
	// Create it: a graph node owned by the sequence.
	let (core, behavior) = TrackListBehavior::create();
	let mut behavior = behavior;
	if let Some(a) = behavior.as_any_mut() {
		if let Some(list) = a.downcast_mut::<TrackListBehavior>() {
			list.kind = kind;
			list.array_base = seq_behavior(&guard.graph, seq_id)?.track_lists.len() as i32;
		}
	}
	let list_id = guard.graph.add_node(core, behavior);
	if let Some(seq) = guard
		.graph
		.get_mut(seq_id)
		.and_then(|e| e.behavior.as_any_mut())
		.and_then(|a| a.downcast_mut::<SequenceBehavior>())
	{
		seq.track_lists.push(list_id);
	}
	if let Some(list) = track_list_behavior_mut(&mut guard.graph, list_id) {
		list.sequence = Some(seq_id);
	}
	Some(list_id)
}

/// Mutable track-list borrow helper.
fn track_list_behavior_mut(g: &mut Graph, id: NodeId) -> Option<&mut TrackListBehavior> {
	g.get_mut(id)?
		.behavior
		.as_any_mut()?
		.downcast_mut::<TrackListBehavior>()
}

/// Append a track of `kind` to the sequence (the module's
/// `TimelineAddTrackCommand`), returning the new track's index (the
/// facade's `oakengine_sequence_add_track` contract).
pub fn add_track(p: &ProjectRef, seq_id: NodeId, kind: TrackType) -> Result<i32, String> {
	let list_id = find_or_create_track_list(p, seq_id, kind)
		.ok_or_else(|| "sequence has no track list for this type".to_string())?;
	let mut cmd = TimelineAddTrackCommand::new(NodeRef::new(p.clone(), list_id));
	cmd.redo();
	let guard = lock(p);
	let n = track_list_behavior(&guard.graph, list_id)
		.map(|l| l.tracks.len() as i32)
		.ok_or_else(|| "add track command produced no track list".to_string())?;
	if n > 0 {
		Ok(n - 1)
	} else {
		Err("add track command produced no track".to_string())
	}
}

/// Convert a frame timestamp (sequence frame-rate timebase ticks) to a
/// seconds rational: `ts` ticks of `fr_den/fr_num` seconds.
pub fn ts_to_seconds(ts: i64, fr_num: i32, fr_den: i32) -> Rational {
	Rational::new(ts * i64::from(fr_den), i64::from(fr_num))
}

/// Place a footage clip on a track of the sequence (the facade's
/// `oakengine_sequence_add_footage_clip_ex` semantics: the sequence lives
/// in its own scratch project, so a scratch footage node is created there
/// and connected to the clip; the real-project footage is untouched).
///
/// `in_ts`/`out_ts`/`media_in_ts` are frame timestamps in the sequence's
/// frame-rate timebase.
pub fn place_footage_clip(
	project: &ProjectRef,
	seq_id: NodeId,
	filename: &str,
	kind: TrackType,
	track_index: i32,
	in_ts: i64,
	out_ts: i64,
	media_in_ts: i64,
	fr_num: i32,
	fr_den: i32,
) -> Result<(), String> {
	if in_ts < 0 || out_ts <= in_ts || media_in_ts < 0 {
		return Err("invalid clip range (need 0 <= in < out and media_in >= 0)".to_string());
	}
	let list_id = find_or_create_track_list(project, seq_id, kind)
		.ok_or_else(|| "sequence has no track list for this type".to_string())?;

	// Track-index validation against the current list.
	let track_count = {
		let guard = lock(project);
		track_list_behavior(&guard.graph, list_id)
			.map(|l| l.tracks.len() as i32)
			.unwrap_or(0)
	};
	if track_index < 0 || track_index >= track_count {
		return Err(format!(
			"track index {track_index} out of range ({track_count} tracks)"
		));
	}

	let in_r = ts_to_seconds(in_ts, fr_num, fr_den);
	let out_r = ts_to_seconds(out_ts, fr_num, fr_den);
	let media_r = ts_to_seconds(media_in_ts, fr_num, fr_den);
	let length = out_r - in_r;

	// The scratch footage node (created directly in the sequence's project;
	// the graph edge cannot cross projects).
	let footage_id = {
		let mut guard = lock(project);
		let (mut core, behavior) = FootageBehavior::create();
		core.set_standard_value("file_in", -1, oak_node::value::NodeValue::Text(filename.to_string()));
		let id = guard.graph.add_node(core, behavior);
		if let Some(f) = footage_behavior_mut(&mut guard.graph, id) {
			f.filename = filename.to_string();
			let _ = f.probe();
		}
		id
	};

	// The clip block, positioned by media-in + length (the facade's
	// `oaknode_clip_set_media_in` + `oaknode_block_set_length_and_media_in`).
	let clip_id = {
		let mut guard = lock(project);
		let (core, behavior) = block::clip_create();
		let id = guard.graph.add_node(core, behavior);
		if let Some(c) = clip_behavior_mut(&mut guard.graph, id) {
			c.core.media_in = media_r;
			c.core.set_length_and_media_in(length);
		}
		id
	};

	// Place on the track (the module's TrackPlaceBlockCommand redo).
	let mut place =
		TrackPlaceBlockCommand::new(NodeRef::new(project.clone(), list_id), track_index, NodeRef::new(project.clone(), clip_id), in_r);
	place.redo();

	// Connect the scratch footage to the clip's texture input.
	{
		let mut guard = lock(project);
		guard
			.graph
			.connect(footage_id, clip_id, block::clip_input::TEXTURE_INPUT, -1)
			.map_err(|e| e.to_string())?;
	}
	Ok(())
}

/// Mutable footage borrow helper.
fn footage_behavior_mut(g: &mut Graph, id: NodeId) -> Option<&mut FootageBehavior> {
	g.get_mut(id)?
		.behavior
		.as_any_mut()?
		.downcast_mut::<FootageBehavior>()
}

/// Mutable clip borrow helper.
fn clip_behavior_mut(g: &mut Graph, id: NodeId) -> Option<&mut ClipBlockBehavior> {
	g.get_mut(id)?
		.behavior
		.as_any_mut()?
		.downcast_mut::<ClipBlockBehavior>()
}

// ---------------------------------------------------------------------------
// Montage resolution (the facade's build_video_montage /
// build_audio_montage over the module graph)
// ---------------------------------------------------------------------------

/// The first footage node feeding `id` (upstream BFS over input edges),
/// mirroring the facade's `oaknode_node_find_input_footage`.
fn find_input_footage(g: &Graph, id: NodeId) -> Option<NodeId> {
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

/// The footage filename feeding a clip (upstream BFS + footage behavior).
fn clip_media(g: &Graph, block_id: NodeId) -> Option<(String, i32)> {
	let footage_id = find_input_footage(g, block_id)?;
	let filename = footage_behavior(g, footage_id)?.filename.clone();
	Some((filename, 0))
}

/// The video montage at sequence time `time`: every clip covering `time`
/// on video tracks, ordered bottom-to-top (the highest-numbered track —
/// the list's last — is topmost, so it is composited last).
pub fn video_montage(p: &ProjectRef, seq_id: NodeId, time: Rational) -> Vec<MontageClip> {
	let g = lock(p);
	let mut clips = Vec::new();
	let Some(seq) = seq_behavior(&g.graph, seq_id) else {
		return clips;
	};
	for &list_id in &seq.track_lists {
		let Some(list) = track_list_behavior(&g.graph, list_id) else {
			continue;
		};
		if list.kind != TrackType::Video {
			continue;
		}
		for &track_id in list.tracks.iter() {
			let Some(track) = track_behavior(&g.graph, track_id) else {
				continue;
			};
			for &block_id in &track.blocks {
				let Some(clip) = g
					.graph
					.get(block_id)
					.and_then(|e| e.behavior.as_any())
					.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
				else {
					continue;
				};
				let in_ = clip.core.in_();
				let out = clip.core.out();
				if time < in_ || time >= out {
					continue;
				}
				let Some((filename, _)) = clip_media(&g.graph, block_id) else {
					continue;
				};
				clips.push(MontageClip {
					filename,
					stream_index: 0,
					in_time: in_,
					out_time: out,
					media_in: clip.core.media_in,
					gain: 1.0,
					// The CLI's simplified montage builder does not resolve
					// effect chains (unchanged behavior).
					effects: Vec::new(),
				});
			}
		}
	}
	clips
}

/// The audio montage over `range`: every audio clip overlapping the
/// range, media times resolved from the clip ranges, audio stream 1.
pub fn audio_montage(p: &ProjectRef, seq_id: NodeId, range: TimeRange) -> Vec<MontageClip> {
	let g = lock(p);
	let mut clips = Vec::new();
	let Some(seq) = seq_behavior(&g.graph, seq_id) else {
		return clips;
	};
	for &list_id in &seq.track_lists {
		let Some(list) = track_list_behavior(&g.graph, list_id) else {
			continue;
		};
		if list.kind != TrackType::Audio {
			continue;
		}
		for &track_id in &list.tracks {
			let Some(track) = track_behavior(&g.graph, track_id) else {
				continue;
			};
			for &block_id in &track.blocks {
				let Some(clip) = g
					.graph
					.get(block_id)
					.and_then(|e| e.behavior.as_any())
					.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
				else {
					continue;
				};
				let in_ = clip.core.in_();
				let out = clip.core.out();
				if out <= range.in_() || in_ >= range.out() {
					continue;
				}
				let Some((filename, _)) = clip_media(&g.graph, block_id) else {
					continue;
				};
				clips.push(MontageClip {
					filename,
					stream_index: 1,
					in_time: in_,
					out_time: out,
					media_in: clip.core.media_in,
					gain: 1.0,
					effects: Vec::new(),
				});
			}
		}
	}
	clips
}

// ---------------------------------------------------------------------------
// Rendering (the facade's renderer over the oakrender ticket arena)
// ---------------------------------------------------------------------------

/// Bring up the process-wide render manager; already-initialized is
/// success (the facade's `oakengine_render_manager_init` returns the
/// module's OAKRENDER_E_STATE for a second init, which the CLI treats as
/// "already up").
pub fn render_manager_init() -> Result<(), String> {
	match RenderManager::init() {
		Ok(()) => Ok(()),
		Err(oak_render::error::Error::State) => Ok(()),
		Err(e) => Err(e.to_string()),
	}
}

/// Tear down the process-wide render manager (no-op when down).
pub fn render_manager_shutdown() {
	RenderManager::shutdown();
}

/// A rendered frame's pixel payload (the module frame a video ticket
/// produces).
pub struct RenderedFrame {
	/// Width in pixels.
	pub width: i32,
	/// Height in pixels.
	pub height: i32,
	/// Pixel format (`oak_core::PixelFormat` as int).
	pub format: i32,
	/// Bytes per scanline (stride).
	pub linesize: i32,
	/// Pixel data (at least `linesize * height` bytes).
	pub data: Vec<u8>,
}

/// Render one frame of the sequence's montage at `time` (seconds
/// rational) into a `(width, height)` F32 frame. M15 S2: the process
/// backend renders a BGRA8 shm slot; the frame is copied out once as
/// RGBA8 u8 (the PPM writer's format 0) and the slot released.
pub fn render_frame(
	seq_id: NodeId,
	time: Rational,
	montage: Vec<MontageClip>,
	width: i32,
	height: i32,
) -> Result<RenderedFrame, String> {
	let m = RenderManager::global().ok_or_else(|| "render manager is not initialized".to_string())?;
	let params = VideoTicketParams {
		viewer: seq_id.identity(),
		project: String::new(),
		time,
		force_size: Some((width, height)),
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage,
		adjustments: Vec::new(),
	};
	let id = m.tickets.next_id();
	m.tickets.submit_video_with_id(id, params, Box::new(|_| {}));
	m.tickets.wait(id).map_err(|e| e.to_string())?;
	let result = m.tickets.result(id).ok_or_else(|| "render ticket produced no result".to_string())?;
	match &result {
		Ok(TicketPayload::Video(oak_core::texture::Texture::Cpu(frame))) => {
			Ok(RenderedFrame {
				width: frame.width,
				height: frame.height,
				format: frame.format as i32,
				linesize: frame.linesize_bytes() as i32,
				data: frame.data.clone(),
			})
		}
		Ok(TicketPayload::Video(texture @ oak_core::texture::Texture::Gpu { .. })) => {
			// M2: the thread pipeline renders all-GPU; the CLI writes CPU
			// pixels, so this is an explicit readback boundary.
			let frame = texture
				.to_frame()
				.map_err(|e| format!("render readback: {e:?}"))?;
			Ok(RenderedFrame {
				width: frame.width,
				height: frame.height,
				format: frame.format as i32,
				linesize: frame.linesize_bytes() as i32,
				data: frame.data,
			})
		}
		Ok(TicketPayload::ShmFrame(frame)) => {
			let out = shm_to_rendered_frame(frame);
			m.release_frame(frame);
			Ok(out)
		}
		_ => Err("render produced no video frame".to_string()),
	}
}

/// Copy a process-backend shm frame (BGRA8 slot) out into the CLI's
/// RGBA8 u8 frame layout (format 0, 4 channels — the PPM writer's RGB
/// order). Necessary copy: the CLI owns the pixels it writes to disk.
fn shm_to_rendered_frame(frame: &oak_render::procpool::ShmFrameRef) -> RenderedFrame {
	let meta = &frame.meta;
	let pixels = frame
		.shm
		.slot_bytes(frame.slot)
		.get(..meta.data_size.max(0) as usize)
		.unwrap_or_default();
	RenderedFrame {
		width: meta.width,
		height: meta.height,
		format: 0,
		linesize: meta.width.max(0) * 4,
		data: bgra8_to_rgba8(pixels),
	}
}

/// Rendered interleaved f32 audio (the module audio ticket payload).
pub struct RenderedAudio {
	/// Interleaved samples (`frame_count * channel_count` values).
	pub data: Vec<f32>,
	/// Sample rate (Hz).
	pub sample_rate: i32,
	/// Channel count.
	pub channel_count: i32,
}

/// Render the audio range `[start, end)` (seconds rational) as
/// interleaved f32 at 48 kHz stereo (the facade's
/// `oakengine_renderer_render_audio` defaults; uncovered parts are
/// silent).
pub fn render_audio(
	seq_id: NodeId,
	range: TimeRange,
	montage: Vec<MontageClip>,
) -> Result<RenderedAudio, String> {
	let m = RenderManager::global().ok_or_else(|| "render manager is not initialized".to_string())?;
	let params = AudioTicketParams {
		viewer: seq_id.identity(),
		range,
		sample_rate: 48000,
		channel_layout: 0x3,
		montage,
	};
	let id = m.tickets.next_id();
	m.tickets
		.submit_audio_with_id(id, params, Box::new(|_| {}));
	m.tickets.wait(id).map_err(|e| e.to_string())?;
	let result = m.tickets.result(id).ok_or_else(|| "audio ticket produced no result".to_string())?;
	match result {
		Ok(TicketPayload::Audio(samples)) => Ok(RenderedAudio {
			data: samples.samples,
			sample_rate: samples.sample_rate,
			channel_count: samples.channel_count,
		}),
		// M15 S3 process backend: the audio sits in a worker shm slot; copy
		// it out and release the slot (the bytes must outlive the slot).
		Ok(TicketPayload::ShmAudio(audio)) => {
			let samples = audio.to_audio_samples();
			if let Some(m) = RenderManager::global() {
				m.release_audio_frame(&audio);
			}
			Ok(RenderedAudio {
				data: samples.samples,
				sample_rate: samples.sample_rate,
				channel_count: samples.channel_count,
			})
		}
		_ => Err("audio render produced no samples".to_string()),
	}
}

// ---------------------------------------------------------------------------
// Export (the facade's `oakengine_export_render` over the module export
// task)
// ---------------------------------------------------------------------------

/// Synchronously export `[0, frames)` of the sequence to `out` as
/// H.264/AAC MP4 (the facade's `oakengine_export_render` defaults: codec
/// default bit rates, 48 kHz stereo, fit scaling; the output geometry is
/// the sequence's).
pub fn export_sequence(
	project: &ProjectRef,
	seq_id: NodeId,
	out: &str,
	fr_num: i32,
	fr_den: i32,
	frames: i64,
) -> Result<(), String> {
	let (width, height) = {
		let guard = lock(project);
		sequence_geometry(&guard, seq_id)
	};
	if width <= 0 || height <= 0 {
		return Err("sequence has no valid video dimensions".to_string());
	}
	if fr_num <= 0 || fr_den <= 0 {
		return Err("sequence has no valid frame rate".to_string());
	}
	let out_num = frames * i64::from(fr_den);
	let encoding = oak_task::export::EncodingParams {
		filename: out.to_string(),
		format: oak_codec::exportformat::Format::MPEG4Video as i32,
		video_enabled: true,
		video_codec: oak_codec::exportcodec::Codec::H264 as i32,
		video_width: width,
		video_height: height,
		video_time_base_num: fr_den,
		video_time_base_den: fr_num,
		video_pixel_format: 0,
		audio_enabled: true,
		audio_codec: oak_codec::exportcodec::Codec::AAC as i32,
		audio_sample_rate: 48000,
		audio_channel_layout: 0x3,
		subtitles_enabled: false,
		export_length_num: out_num as i32,
		export_length_den: fr_num,
		has_custom_range: true,
		custom_range_in_num: 0,
		custom_range_in_den: fr_num,
		custom_range_out_num: out_num as i32,
		custom_range_out_den: fr_num,
		video_bit_rate: 0,
		audio_bit_rate: 0,
		color_override_enabled: false,
		color_primaries: 0,
		color_trc: 0,
		color_space: 0,
	};
	let inner = oak_task::export::ExportTask::new((project.clone(), seq_id), encoding);
	let mut driver = oak_task::task::Task::new("Exporting...", None);
	driver.set_behavior(Box::new(inner));
	driver.start().map_err(|_| {
		driver
			.error()
			.map(|s| s.to_string())
			.unwrap_or_else(|| "export failed".to_string())
	})
}
