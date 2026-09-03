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

//! Montage resolution, ticket rendering and the export driver (M14 R3).
//!
//! The facade's renderer box (`crates/oakengine/src/render.rs`: geometry
//! validation, `build_video_montage` / `build_audio_montage`,
//! `clip_media`) and its export path (`oakengine_task_create_export` +
//! `oakengine_task_start_sync` + the task subscription) are app-side now:
//! the montage builders read the oaknode graph directly and the renders
//! go through the oakrender ticket arena, exactly like the CLI's
//! `engine.rs` (M14 R2). The export drives `oak_task::export::ExportTask`
//! synchronously on a background thread with the module task's event
//! listener and cancel atom wired to the app's [`ExportSession`].

use std::sync::mpsc;

use gpui::RenderImage;
use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::id::NodeId;
use oak_node::track::TrackType;
use oak_render::manager::RenderManager;
use oak_render::procpool::ShmFrameRef;
use oak_render::ticket::{AudioTicketParams, MontageClip, TicketPayload, VideoTicketParams};

use super::engine::{ExportEvent, ExportSession};
use super::frames::{bgra_bytes_to_render_image, f32_rgba_to_bgra_image};
use super::graphops::{
	clip_behavior, lock, sequence_behavior, track_behavior, track_list_behavior, ProjectRef,
};
use super::scopes::{analyze_bgra8, analyze_f32_rgba, ScopeData};

/// The pixel format the viewers render in (`oak_core::PixelFormat::F32`,
/// the pipeline's internal format; the app downconverts to BGRA itself).
pub const PIXEL_FORMAT_F32: i32 = 4;

/// The BGRA8 slot wire format the process backend renders into (M15 S2):
/// the worker converts its F32 pipeline output to BGRA8 at the end of the
/// render, so the main process reads display-ready bytes from the slot.
pub const SLOT_FORMAT_BGRA8: i32 = oak_render::ipc::SLOT_FORMAT_BGRA8;

// ---------------------------------------------------------------------------
// Render manager
// ---------------------------------------------------------------------------

/// Bring up the process-wide render manager if it is not running yet
/// (without it the ticket arena rejects submissions). Returns false when
/// the manager could not be started.
pub fn ensure_render_manager() -> bool {
	if RenderManager::global().is_some() {
		return true;
	}
	// Already-initialized is success (the module reports State for a
	// second init). Log the real failure before degrading to the bool —
	// "render manager failed to start" alone is undiagnosable in CI.
	if let Err(e) = RenderManager::init() {
		if RenderManager::global().is_none() {
			log::error!("render manager init failed: {e:?}");
		}
	}
	RenderManager::global().is_some()
}

// ---------------------------------------------------------------------------
// Montage resolution (the facade's build_video_montage / build_audio_montage
// over the module graph)
// ---------------------------------------------------------------------------

/// Whether preview decoding may substitute proxy media: the global
/// `UseProxyMedia` config switch (C++ `Tools > Use Proxy Media`; the
/// export path never consults it — exports always decode the original).
pub fn use_proxy_media() -> bool {
	oak_core::configstore::ConfigStore::instance().get_bool(None, "UseProxyMedia", 1) != 0
}

/// The preview media of a footage node with the three-level proxy switch
/// applied (C++ `Footage::value` proxy arms + `FootageJob::should_use_proxy`):
/// the global switch, the footage's proxy-enabled flag and an on-disk
/// `Ready` proxy all have to line up, otherwise the original is returned.
/// Video decodes from the proxy's stream 0 (the proxy's only video
/// stream); audio from stream 1 when the proxy was generated with audio
/// (the first source audio stream's rank + 1). A missing proxy file falls
/// back to the original.
pub fn preview_footage_media(
	f: &oak_node::footage::FootageBehavior,
	is_video: bool,
) -> (String, i32) {
	// The original media decodes from the footage's actual first stream of
	// the kind (C++ maps per-stream); the 0/1 fallbacks cover footage
	// whose streams were never probed (legacy project files).
	let original_stream = f
		.streams
		.iter()
		.find(|s| s.is_video == is_video)
		.map(|s| s.index)
		.unwrap_or(if is_video { 0 } else { 1 });
	let original = (f.filename.clone(), original_stream);
	if !use_proxy_media() || !f.proxy_enabled || f.proxy.is_empty() {
		return original;
	}
	if oak_codec::proxymanager::ProxyManager::get_proxy_state(&f.proxy)
		!= oak_codec::proxymanager::ProxyState::Ready
	{
		return original;
	}
	if is_video {
		// The proxy stands in for the footage's first video stream only
		// (C++ matches proxy_video_stream_index against the stream index).
		let first_video = f.streams.iter().find(|s| s.is_video);
		match first_video {
			Some(stream) if f.proxy_video_stream_index == stream.index => (f.proxy.clone(), 0),
			_ => original,
		}
	} else if oak_codec::proxymanager::ProxyManager::proxy_filename_has_audio(&f.proxy) {
		(f.proxy.clone(), 1)
	} else {
		original
	}
}

/// The preview media feeding a clip: the clip's footage with the proxy
/// switch applied ([`preview_footage_media`]).
fn clip_preview_media(
	g: &oak_node::graph::Graph,
	block_id: NodeId,
	is_video: bool,
) -> Option<(String, i32)> {
	let footage_id = super::graphops::find_input_footage(g, block_id)?;
	let f = super::graphops::footage_behavior(g, footage_id)?;
	if f.filename.is_empty() {
		return None;
	}
	Some(preview_footage_media(f, is_video))
}

/// A multi-cam host clip's playable media: the CURRENT source's angle clip
/// (footage + media_in) rendered across the host clip's span.
///
/// `None` when `clip` is not fed from a multicam node (or the resolved
/// angle has no footage). The montage clip's `media_in` maps the host
/// timeline position into the angle's media: `angle_media_in + (host_in -
/// angle_in)`.
///
/// CALLER HOLDS THE PROJECT LOCK: this is pure `Graph` walking — the
/// `multicam::*` project-locking helpers MUST NOT be called from here
/// (non-recursive mutex).
fn clip_multicam_media(
	g: &oak_node::graph::Graph,
	clip: NodeId,
	host_in: oak_core::Rational,
) -> Option<(String, i32, oak_core::Rational)> {
	use oak_node::nodes::multicamnode::{CURRENT_INPUT, SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT};
	// The clip's texture source must be a multicam node.
	let mc = g.connected_output(clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)?;
	let mc_entry = g.get(mc)?;
	if mc_entry.behavior.type_id() != "org.olivevideoeditor.Olive.multicam" {
		return None;
	}
	let source = mc_entry.core.standard_value(CURRENT_INPUT, -1).to_double() as i32;
	if source < 0 {
		return None;
	}
	// The source sequence and the current source's track.
	let seq = g.connected_output(mc, SEQUENCE_INPUT, -1)?;
	let kind = {
		let t = mc_entry.core.standard_value(SEQUENCE_TYPE_INPUT, -1).to_double() as i32;
		oak_node::track::TrackType::from_c(t).unwrap_or(oak_node::track::TrackType::Video)
	};
	let seq_pos = super::graphops::track_list_of(g, seq, kind)?;
	let list = super::graphops::track_list_behavior(g, seq_pos)?;
	let track = *list.tracks.get(source as usize)?;
	let track_behavior = super::graphops::track_behavior(g, track)?;
	// The angle clip covering the host position.
	let angle = track_behavior.blocks.iter().find_map(|&block_id| {
		let clip = super::graphops::clip_behavior(g, block_id)?;
		let in_ = clip.core.in_();
		let out = clip.core.out();
		if host_in < in_ || host_in >= out {
			return None;
		}
		let footage_id = super::graphops::find_input_footage(g, block_id)?;
		let f = super::graphops::footage_behavior(g, footage_id)?;
		if f.filename.is_empty() {
			return None;
		}
		let (filename, stream_index) = preview_footage_media(f, true);
		// Host position → angle media position: the host sits at angle
		// media_in when the host's in == the angle's in; the offset shifts
		// it by the difference.
		let host_delta = host_in - in_;
		let media_in = clip.core.media_in + host_delta;
		Some((filename, stream_index, media_in))
	})?;
	Some(angle)
}

/// The clip block's effect stack as montage effect descriptors
/// (source-first order — the chain walk's signal order, so the renderer
/// applies them media-side first). The chain walk runs all the way to
/// the media source, so its first element is the footage node feeding
/// the clip; the montage decodes that footage itself, so the source
/// node (the one WITHOUT an effect input) is dropped here. Disabled
/// effects are carried with `enabled = false`; the renderer bypasses
/// them (the C++ traverser's bypass pushes the effect input through
/// unchanged). Parameters are the inspector's parameter set (non-hidden,
/// non-connection inputs at their standard values; keyframed values are
/// not time-resolved on this path).
fn clip_effects(g: &oak_node::graph::Graph, block_id: NodeId) -> Vec<oak_render::ticket::MontageEffect> {
	use super::effectchain;
	effectchain::chain(g, block_id)
		.into_iter()
		.filter(|&fx| effectchain::effect_input_of(g, fx).is_some())
		.filter_map(|fx| {
			let entry = g.get(fx)?;
			Some(oak_render::ticket::MontageEffect {
				type_id: entry.behavior.type_id().to_string(),
				enabled: effectchain::is_enabled(g, fx),
				effect_input_id: effectchain::effect_input_of(g, fx),
				params: effectchain::effect_params(g, fx)
					.unwrap_or_default()
					.into_iter()
					.map(|p| (p.input_id, p.value))
					.collect(),
			})
		})
		.collect()
}

/// The video montage at sequence time `time`: every clip covering `time`
/// on video tracks, ordered bottom-to-top (the highest-numbered track —
/// the list's last — is topmost, so it is composited last; NLE stacking
/// matches the timeline UI, which shows the highest-numbered track on
/// top). Hidden tracks (the muted flag doubles as the video visibility
/// toggle, Olive parity) contribute nothing.
pub fn video_montage(p: &ProjectRef, seq: NodeId, time: Rational) -> Vec<MontageClip> {
	let g = lock(p);
	let mut clips = Vec::new();
	let Some(s) = sequence_behavior(&g.graph, seq) else {
		return clips;
	};
	for &list_id in &s.track_lists {
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
			if track.muted {
				continue;
			}
			for &block_id in &track.blocks {
				let Some(clip) = clip_behavior(&g.graph, block_id) else {
					continue;
				};
				let in_ = clip.core.in_();
				let out = clip.core.out();
				if time < in_ || time >= out {
					continue;
				}
				// A multi-cam host clip decodes through its multicam node's
				// CURRENT source: the montage carries that angle's angle-clip
				// media (footage + the angle clip's media_in), rendered at the
				// host clip's timeline span. Without this the host clip's
				// texture chain has no footage to preview (the sequence viewer
				// painted black).
				let montage_media = clip_multicam_media(&g.graph, block_id, in_);
				let (filename, stream_index, media_in) = match montage_media {
					Some(mc) => mc,
					None => {
						let Some((filename, stream_index)) =
							clip_preview_media(&g.graph, block_id, true)
						else {
							continue;
						};
						(filename, stream_index, clip.core.media_in)
					}
				};
				clips.push(MontageClip {
					filename,
					stream_index,
					in_time: in_,
					out_time: out,
					media_in,
					gain: 1.0,
					effects: clip_effects(&g.graph, block_id),
				});
			}
		}
	}
	clips
}

/// The video montage at sequence time `time` containing ONLY the clip on
/// `track` (a track of the sequence's video track list), if any covers
/// `time`. This is the multicam angle render: each angle is the source
/// sequence's track `i` at the playhead, so the montage carries just that
/// track's clip instead of the whole stack.
pub fn single_track_video_montage(
	p: &ProjectRef,
	seq: NodeId,
	track: NodeId,
	time: Rational,
) -> Vec<MontageClip> {
	let g = lock(p);
	let mut clips = Vec::new();
	let Some(s) = sequence_behavior(&g.graph, seq) else {
		return clips;
	};
	// The track must belong to the sequence's video track list.
	let in_list = s.track_lists.iter().any(|&list_id| {
		track_list_behavior(&g.graph, list_id)
			.map(|l| l.kind == TrackType::Video && l.tracks.contains(&track))
			.unwrap_or(false)
	});
	if !in_list {
		return clips;
	}
	let Some(track) = track_behavior(&g.graph, track) else {
		return clips;
	};
	if track.muted {
		return clips;
	}
	for &block_id in &track.blocks {
		let Some(clip) = clip_behavior(&g.graph, block_id) else {
			continue;
		};
		let in_ = clip.core.in_();
		let out = clip.core.out();
		if time < in_ || time >= out {
			continue;
		}
		let Some((filename, stream_index)) = clip_preview_media(&g.graph, block_id, true) else {
			continue;
		};
		clips.push(MontageClip {
			filename,
			stream_index,
			in_time: in_,
			out_time: out,
			media_in: clip.core.media_in,
			gain: 1.0,
			effects: clip_effects(&g.graph, block_id),
		});
	}
	clips
}

/// Build the video ticket params for one multicam angle: the clip on
/// `track` (a video track of `seq`, the multicam's source sequence) at the
/// playhead timestamp `frame_ts`.
pub fn multicam_angle_frame_params(
	p: &ProjectRef,
	seq: NodeId,
	track: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
) -> Result<VideoTicketParams, String> {
	validate_geometry(width, height, tb)?;
	let time = Rational::new(frame_ts * tb.0, tb.1);
	// Bind the uuid before the literal: the struct expression is the tail of
	// the block, so an inline `lock(p)` temporary would outlive the field
	// initializers and deadlock the reentrant lock in `single_track_video_montage`.
	let project = lock(p).uuid.clone();
	Ok(VideoTicketParams {
		// The angle grid is a single-track MONTAGE render, not a full
		// sequence-graph frame: viewer stays 0 so the worker takes the
		// montage path. A nonzero viewer (the source sequence identity)
		// made every angle request re-evaluate the WHOLE source sequence
		// through the traverser — three grid cells × every tick of
		// playback ground the machine to a halt ("switch then everything
		// crawls"), and the graph render of the source sequence returned
		// the stacked composite (all angles at once) rather than this
		// one angle, which is why the cells showed the wrong/no picture.
		viewer: 0,
		project,
		time,
		force_size: Some((width, height)),
		force_format: None,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: single_track_video_montage(p, seq, track, time),
	})
}

/// Render one multicam angle frame (the clip on `track` of the source
/// sequence at `frame_ts`) into a `(width, height)` frame.
pub fn render_multicam_angle_frame(
	p: &ProjectRef,
	seq: NodeId,
	track: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
) -> Result<RenderedFrame, String> {
	render_video(multicam_angle_frame_params(p, seq, track, frame_ts, tb, width, height)?)
}

/// The audio montage over `range`: every audio clip overlapping the
/// range, media times resolved from the clip ranges, audio stream 1.
/// Muted tracks are silenced (skipped entirely).
pub fn audio_montage(p: &ProjectRef, seq: NodeId, range: TimeRange) -> Vec<MontageClip> {
	let g = lock(p);
	let mut clips = Vec::new();
	let Some(s) = sequence_behavior(&g.graph, seq) else {
		return clips;
	};
	for &list_id in &s.track_lists {
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
			if track.muted {
				continue;
			}
			for &block_id in &track.blocks {
				let Some(clip) = clip_behavior(&g.graph, block_id) else {
					continue;
				};
				let in_ = clip.core.in_();
				let out = clip.core.out();
				if out <= range.in_() || in_ >= range.out() {
					continue;
				}
				let Some((filename, stream_index)) =
					clip_preview_media(&g.graph, block_id, false)
				else {
					continue;
				};
				clips.push(MontageClip {
					filename,
					stream_index,
					in_time: in_,
					out_time: out,
					media_in: clip.core.media_in,
					gain: 1.0,
					// Audio clips: the effect stack is a video-side
					// concept; the mixer never consults it.
					effects: Vec::new(),
				});
			}
		}
	}
	clips
}

// ---------------------------------------------------------------------------
// Ticket rendering
// ---------------------------------------------------------------------------

/// A rendered frame's pixel payload (M15 S2): a process-backend shm slot
/// (BGRA8, zero-copy read) or an in-process F32 CPU frame (the test-only
/// inline backend).
pub enum RenderedFrame {
	/// Process backend: a BGRA8 frame in a worker's shared-memory slot.
	/// Read with `shm.slot_bytes(slot)` (no counted copy on the preview
	/// path), build the display image, then release the slot through the
	/// render manager.
	Shm(ShmFrameRef),
	/// In-process (test) backend: an F32 RGBA CPU frame.
	CpuF32 {
		/// Width in pixels.
		width: i32,
		/// Height in pixels.
		height: i32,
		/// Bytes per scanline (stride).
		linesize: i32,
		/// Pixel data (at least `linesize * height` bytes).
		data: Vec<u8>,
	},
}

impl RenderedFrame {
	/// Width in pixels.
	pub fn width(&self) -> i32 {
		match self {
			RenderedFrame::Shm(f) => f.meta.width,
			RenderedFrame::CpuF32 { width, .. } => *width,
		}
	}

	/// Height in pixels.
	pub fn height(&self) -> i32 {
		match self {
			RenderedFrame::Shm(f) => f.meta.height,
			RenderedFrame::CpuF32 { height, .. } => *height,
		}
	}

	/// The pixel format: the slot's wire format for the shm variant
	/// (`PIXEL_FORMAT_F32` when the worker rendered F32 for the 10-bit
	/// display path, `SLOT_FORMAT_BGRA8` for the 8-bit fallback),
	/// `PIXEL_FORMAT_F32` for the in-process variant.
	pub fn format(&self) -> i32 {
		match self {
			RenderedFrame::Shm(f) => f.meta.format,
			RenderedFrame::CpuF32 { .. } => PIXEL_FORMAT_F32,
		}
	}

	/// True for the process-backend shm variant.
	pub fn is_shm(&self) -> bool {
		matches!(self, RenderedFrame::Shm(_))
	}

	/// Build the viewer display image plus the scope samples (M15 S2
	/// zero-copy onscreen path). For the shm variant the slot's bytes are
	/// wrapped into the display buffer — the GPU-upload staging copy, the
	/// single permitted main-process copy on the preview path (design §3.5).
	/// The display color transform (display ICC) is applied in place on that
	/// staging copy / on the F32 samples, so it costs no extra copy.
	///
	/// The third return value is the display-transformed F32 RGBA samples
	/// when the frame arrived in F32 (the 10-bit display path: the caller
	/// uploads them as an RGBA16F texture and skips the BGRA8 image), `None`
	/// for BGRA8 frames (whose 8-bit quantization is already baked in). The
	/// caller releases the slot afterwards.
	pub fn to_display(&self) -> Option<(RenderImage, ScopeData, Option<Vec<f32>>)> {
		match self {
			RenderedFrame::Shm(f) => {
				let meta = &f.meta;
				let (w, h) = (meta.width.max(0) as u32, meta.height.max(0) as u32);
				let pixels = f.shm.slot_bytes(f.slot);
				let data = pixels.get(..meta.data_size.max(0) as usize)?;
			if meta.format == PIXEL_FORMAT_F32 {
				// M15 S3: the worker rendered F32 (the 10-bit display
				// path) — repack the padded rows, transform and hand the
				// samples back for the RGBA16F texture.
				let mut samples = repack_f32_rows(meta.width, meta.height, meta.linesize, data)?;
				// Output node: working space → the project's output
				// colorspace. The scopes below read the output-colorspace
				// signal (same convention as the BGRA8 slot); the display
				// policy then decides whether the display ICC is applied on
				// top (self-managed) or the OS maps the declared content
				// colorspace (OS-managed).
				apply_output_node_f32(&mut samples);
				let scope = analyze_f32_rgba(w, h, &samples);
				super::displaycolor::apply_f32_rgba(&mut samples, (w * h) as i64);
				let image = f32_rgba_to_bgra_image(w, h, &samples);
				Some((image, scope, Some(samples)))
			} else {
					// BGRA8 slot: the worker already downconverted — wrap the
					// bytes directly (no 10-bit path available for them).
					let scope = analyze_bgra8(w, h, data);
					// The display transform edits the staging copy in place.
					let mut owned = data.to_vec();
					super::displaycolor::apply_bgra8(&mut owned, (w * h) as i64);
					let image = bgra_bytes_to_render_image(w, h, &owned)?;
					Some((image, scope, None))
				}
			}
			RenderedFrame::CpuF32 {
				width,
				height,
				linesize,
				data,
			} => {
				let (w, h) = ((*width).max(0) as u32, (*height).max(0) as u32);
				let mut samples = repack_f32_rows(*width, *height, *linesize, data)?;
				// Output node first, as in the shm path: the scopes read the
				// output-colorspace signal (BGRA8-slot convention).
				apply_output_node_f32(&mut samples);
				let scope = analyze_f32_rgba(w, h, &samples);
				super::displaycolor::apply_f32_rgba(&mut samples, (w * h) as i64);
				Some((
					f32_rgba_to_bgra_image(w, h, &samples),
					scope,
					Some(samples),
				))
			}
		}
	}
}

/// The app-side output node for F32 delivery frames: working colorspace →
/// the project's output colorspace, in place on tightly packed samples.
/// Pass-through in the legacy sRGB working space.
fn apply_output_node_f32(samples: &mut [f32]) {
	oak_core::colormath::working_to_display_target(
        samples,
        oak_core::color::pipeline_working_space(),
        oak_core::color::pipeline_output_spec(),
	);
}

/// Repack one F32 RGBA rendered frame (rows padded to `linesize`) into
/// tightly packed samples. Returns `(width, height, samples)`-style
/// samples only; geometry is validated by the caller.
fn repack_f32_rows(width: i32, height: i32, linesize: i32, data: &[u8]) -> Option<Vec<f32>> {
	if width <= 0 || height <= 0 {
		return None;
	}
	let row_bytes = (width * 4 * 4) as usize;
	let linesize = (linesize as usize).max(row_bytes);
	if data.len() < linesize * height as usize {
		return None;
	}
	let mut samples = vec![0.0f32; (width * height * 4) as usize];
	for y in 0..height as usize {
		let row = &data[y * linesize..y * linesize + row_bytes];
		for (i, px) in row.chunks_exact(4).enumerate() {
			let v = f32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
			samples[y * (width as usize) * 4 + i] = v;
		}
	}
	Some(samples)
}

/// The renderer geometry / format validation (the facade's
/// `make_renderer_box` argument checks).
fn validate_geometry(width: i32, height: i32, tb: (i64, i64)) -> Result<(), String> {
	if width <= 0 || height <= 0 || tb.0 <= 0 || tb.1 <= 0 {
		return Err("invalid render geometry or frame rate".to_string());
	}
	Ok(())
}

/// The background full-res render geometry: the sequence's native size
/// scaled by the playback resolution divider (the viewer `Playback
/// Resolution ▸` menu). A paused preview at Half/Quarter/Eighth must not
/// be overwritten by a native-size background fill, so the full-res job
/// renders at the same divider as the preview window.
pub fn full_res_render_size(seq_width: u32, seq_height: u32, divider: u32) -> (i32, i32) {
	let w = seq_width.max(1) as f64;
	let h = seq_height.max(1) as f64;
	let scale = 1.0 / divider.max(1) as f64;
	let width = ((w * scale).round() as u32).max(2) as i32;
	let height = ((h * scale).round() as u32).max(2) as i32;
	(width, height)
}

/// The resolution a footage's proxy decodes at, inferred from its
/// generation parameters: the absolute `ProxyParams` width/height when
/// configured, otherwise the source video resolution divided by the
/// proxy divider. `None` when there is no probed video stream to base a
/// divider mode on.
fn proxy_resolution(f: &oak_node::footage::FootageBehavior) -> Option<(i32, i32)> {
	let params = f
		.custom_proxy_params
		.clone()
		.unwrap_or_else(oak_codec::proxymanager::ProxyManager::proxy_params_from_config);
	if params.width > 0 && params.height > 0 {
		return Some((params.width, params.height));
	}
	let source = f.streams.iter().find(|s| s.is_video)?.video.as_ref()?;
	let d = params.divider.max(1) as u32;
	Some((
		(source.width as u32 / d).max(2) as i32,
		(source.height as u32 / d).max(2) as i32,
	))
}

/// Clamp `(w, h)` (keeping the aspect ratio) down to the largest size
/// that fits inside every limit rectangle — i.e. the render never
/// upscales any of the limit sources. Unchanged when it already fits.
fn clamp_render_size(w: i32, h: i32, limits: &[(i32, i32)]) -> (i32, i32) {
	let mut scale = 1.0f64;
	for &(lw, lh) in limits {
		if lw <= 0 || lh <= 0 {
			continue;
		}
		scale = scale.min(lw as f64 / w as f64).min(lh as f64 / h as f64);
	}
	if scale >= 1.0 {
		return (w, h);
	}
	(
		((w as f64 * scale).round() as i32).max(2),
		((h as f64 * scale).round() as i32).max(2),
	)
}

/// The proxy resolutions of the clips covering `time` on `seq`'s video
/// tracks whose preview media actually IS the proxy (the global
/// `UseProxyMedia` switch, the footage's proxy flag and an on-disk Ready
/// proxy all have to line up — the same test [`preview_footage_media`]
/// applies). The montage composites every clip at one shared size, so an
/// active proxy bounds the whole render: decoding a 720p proxy up to a
/// 1080p target (then scaling back down for display) is the wasted
/// upscale this limit removes.
fn proxy_limits_at(p: &ProjectRef, seq: NodeId, time: Rational) -> Vec<(i32, i32)> {
	let g = lock(p);
	let mut limits = Vec::new();
	let Some(s) = sequence_behavior(&g.graph, seq) else {
		return limits;
	};
	for &list_id in &s.track_lists {
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
			if track.muted {
				continue;
			}
			for &block_id in &track.blocks {
				let Some(clip) = clip_behavior(&g.graph, block_id) else {
					continue;
				};
				let in_ = clip.core.in_();
				let out = clip.core.out();
				if time < in_ || time >= out {
					continue;
				}
				let Some(footage_id) = super::graphops::find_input_footage(&g.graph, block_id)
				else {
					continue;
				};
				let Some(f) = super::graphops::footage_behavior(&g.graph, footage_id) else {
					continue;
				};
				// A proxy that does not substitute the original (off switch,
				// disabled flag, missing file) leaves the render unclamped.
				if preview_footage_media(f, true).0 != f.proxy {
					continue;
				}
				if let Some(limit) = proxy_resolution(f) {
					limits.push(limit);
				}
			}
		}
	}
	limits
}

/// Drive one video ticket synchronously (M15 S2: returns the payload
/// variant without copying frame bytes — the shm slot is read zero-copy
/// by the caller and released after building the display image).
fn render_video(params: VideoTicketParams) -> Result<RenderedFrame, String> {
	let m = RenderManager::global().ok_or_else(|| "render manager is not initialized".to_string())?;
	let id = m.tickets.next_id();
	if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
		eprintln!("renderops: sync render submit arena ticket {}", id.0);
	}
	m.tickets.submit_video_with_id(id, params, Box::new(|_| {}));
	m.tickets.wait(id).map_err(|e| e.to_string())?;
	if std::env::var_os("OAK_DEBUG_DISPATCH").is_some() {
		eprintln!("renderops: sync render wait done arena ticket {}", id.0);
	}
	let result = m
		.tickets
		.result(id)
		.ok_or_else(|| "render ticket produced no result".to_string())?;
	match &result {
		Ok(TicketPayload::Video(Texture::Cpu(frame))) => Ok(RenderedFrame::CpuF32 {
			width: frame.width,
			height: frame.height,
			linesize: frame.linesize_bytes() as i32,
			data: frame.data.clone(),
		}),
		Ok(TicketPayload::ShmFrame(frame)) => Ok(RenderedFrame::Shm(frame.clone())),
		Ok(TicketPayload::Video(_)) => Err("render produced a non-CPU frame".to_string()),
		_ => Err("render produced no video frame".to_string()),
	}
}

/// Build the video ticket params for one sequence-montage frame (M15 S2:
/// shared by the synchronous render and the playback pre-render window).
/// `format` is the slot wire format the worker renders into: `None` keeps
/// the BGRA8 8-bit slot (the default), `Some(PixelFormat::F32)` renders F32
/// for the 10-bit display path.
pub fn sequence_frame_params(
	p: &ProjectRef,
	seq: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
	format: Option<oak_core::PixelFormat>,
) -> Result<VideoTicketParams, String> {
	validate_geometry(width, height, tb)?;
	let time = Rational::new(frame_ts * tb.0, tb.1);
	// Proxy clamping: the montage composites every clip at one shared
	// size, so any active proxy bounds the whole render — never decode a
	// 720p proxy up to a 1080p target.
	let (width, height) = clamp_render_size(width, height, &proxy_limits_at(p, seq, time));
	// Bind the uuid before the literal: an inline `lock(p)` temporary in the
	// block-tail struct expression would still be alive when `video_montage`
	// re-locks the project, deadlocking the same thread.
	let project = lock(p).uuid.clone();
	Ok(VideoTicketParams {
		viewer: seq.identity(),
		project,
		time,
		force_size: Some((width, height)),
		force_format: format,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: None,
		montage: video_montage(p, seq, time),
	})
}

/// Render one frame of the sequence's montage at `frame_ts` (a timestamp
/// in the `(tb.1 / tb.0)`-per-frame timebase) into a `(width, height)`
/// frame. `format` is the slot wire format (see
/// [`sequence_frame_params`]).
pub fn render_sequence_frame(
	p: &ProjectRef,
	seq: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
	format: Option<oak_core::PixelFormat>,
) -> Result<RenderedFrame, String> {
	render_video(sequence_frame_params(p, seq, frame_ts, tb, width, height, format)?)
}

/// Build the video ticket params for one single-footage frame (M15 S2:
/// shared by the synchronous render and the source-monitor pre-render
/// window). `format` is the slot wire format (see
/// [`sequence_frame_params`]).
pub fn footage_frame_params(
	p: &ProjectRef,
	footage: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
	format: Option<oak_core::PixelFormat>,
) -> Result<VideoTicketParams, String> {
	validate_geometry(width, height, tb)?;
	let (filename, stream_index, limits) = {
		let g = lock(p);
		let f = super::graphops::footage_behavior(&g.graph, footage)
			.ok_or_else(|| "the node is not footage".to_string())?;
		let (media, stream) = preview_footage_media(f, true);
		// The proxy is the selected preview media: bound the render to the
		// proxy's own resolution instead of upscaling it to the requested
		// target (the source monitor's full-res fill runs this path too).
		let mut limits = Vec::new();
		if media == f.proxy {
			if let Some(resolution) = proxy_resolution(f) {
				limits.push(resolution);
			}
		}
		(media, stream, limits)
	};
	let (width, height) = clamp_render_size(width, height, &limits);
	let time = Rational::new(frame_ts * tb.0, tb.1);
	// Bind the uuid before the literal (same tail-expression temporary rule
	// as the montage paths; harmless here but keeps the pattern uniform).
	let project = lock(p).uuid.clone();
	Ok(VideoTicketParams {
		viewer: footage.identity(),
		project,
		time,
		force_size: Some((width, height)),
		force_format: format,
		cache: None,
		cache_dir: None,
		cache_id: None,
		cache_timebase: None,
		footage: Some((filename, stream_index)),
		montage: Vec::new(),
	})
}

/// Render one frame of a single footage node (the source monitor) at
/// `frame_ts`, decoded straight from the media file. `format` is the slot
/// wire format (see [`sequence_frame_params`]).
pub fn render_footage_frame(
	p: &ProjectRef,
	footage: NodeId,
	frame_ts: i64,
	tb: (i64, i64),
	width: i32,
	height: i32,
	format: Option<oak_core::PixelFormat>,
) -> Result<RenderedFrame, String> {
	render_video(footage_frame_params(p, footage, frame_ts, tb, width, height, format)?)
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

/// Render the audio range `[start_ts, start_ts + len_ts)` (frame
/// timestamps in the `(tb.1 / tb.0)`-per-frame timebase) as interleaved
/// f32 at 48 kHz stereo (the facade's `oakengine_renderer_render_audio`
/// defaults; uncovered parts are silent).
pub fn render_audio_range(
	p: &ProjectRef,
	seq: NodeId,
	start_ts: i64,
	len_ts: i64,
	tb: (i64, i64),
) -> Result<RenderedAudio, String> {
	if tb.0 <= 0 || tb.1 <= 0 {
		return Err("the sequence has no valid frame rate".to_string());
	}
	let range = TimeRange::new(
		Rational::new(start_ts * tb.0, tb.1),
		Rational::new((start_ts + len_ts) * tb.0, tb.1),
	);
	let montage = audio_montage(p, seq, range);
	let m = RenderManager::global().ok_or_else(|| "render manager is not initialized".to_string())?;
	let id = m.tickets.next_id();
	m.tickets.submit_audio_with_id(
		id,
		AudioTicketParams {
			viewer: seq.identity(),
			range,
			sample_rate: 48000,
			channel_layout: 0x3,
			montage,
		},
		Box::new(|_| {}),
	);
	m.tickets.wait(id).map_err(|e| e.to_string())?;
	let result = m
		.tickets
		.result(id)
		.ok_or_else(|| "audio ticket produced no result".to_string())?;
	match result {
		Ok(TicketPayload::Audio(samples)) => Ok(RenderedAudio {
			data: samples.samples,
			sample_rate: samples.sample_rate,
			channel_count: samples.channel_count,
		}),
		// M15 S3 process backend: the audio sits in a worker shm slot; copy
		// the samples out and release the slot (the bytes must outlive it).
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

/// Submit one audio-chunk render for the playback prefetch (M15 S3; M16
/// S3: the chunk is queued on the dedicated audio-render thread — see
/// [`audio_thread`](super::audio_thread) — so the render never runs on the
/// UI tick; a slow decode no longer freezes the UI or delays the next
/// chunk (the old inline-dispatch path underran the audio device). The
/// completion sends `(start_ts, samples)` on `tx`. Render errors send
/// silence of the expected length so the playback buffer stays aligned
/// (the real-time path must never stall the UI thread on a worker).
pub fn submit_audio_chunk(
	p: &ProjectRef,
	seq: NodeId,
	start_ts: i64,
	len_ts: i64,
	tb: (i64, i64),
	tx: mpsc::Sender<(i64, RenderedAudio)>,
) -> Result<(), String> {
	let range = TimeRange::new(
		Rational::new(start_ts * tb.0, tb.1),
		Rational::new((start_ts + len_ts) * tb.0, tb.1),
	);
	let montage = audio_montage(p, seq, range);
	let sample_rate = 48000;
	let channel_layout = 0x3u64;
	super::audio_thread::submit(
		AudioTicketParams {
			viewer: seq.identity(),
			range,
			sample_rate,
			channel_layout,
			montage,
		},
		start_ts,
		tx,
	)
}

// ---------------------------------------------------------------------------
// Export (the facade's `oakengine_task_create_export` + sync run over the
// module export task)
// ---------------------------------------------------------------------------

/// The export audio defaults (the facade's): 48 kHz stereo.
const EXPORT_SAMPLE_RATE: i32 = 48000;
/// Stereo channel-layout bitmask (`OLIVE_CHANNEL_LAYOUT_STEREO`).
const EXPORT_CHANNEL_LAYOUT: u64 = 0x3;

/// Build the export encoding params for `seq` from the app's export
/// dialog inputs: the container `format`, the target `path`, and the
/// work-area range when enabled (frame timestamps). The codecs are the
/// format's first video/audio codecs (the facade's
/// `oakengine_encoding_format_*_codec_at(format, 0)`).
pub fn encoding_params(
	p: &ProjectRef,
	seq: NodeId,
	format: i32,
	path: &std::path::Path,
	workarea: Option<(i64, i64)>,
	length_frames: i64,
) -> Result<oak_task::export::EncodingParams, String> {
	let container = oak_codec::exportformat::Format::from_i32(format)
		.ok_or_else(|| format!("unknown export format {format}"))?;
	let video_codec = oak_codec::exportformat::Format::get_video_codecs(container)
		.first()
		.copied()
		.ok_or_else(|| format!("format {format} has no video codec"))?;
	let audio_codec = oak_codec::exportformat::Format::get_audio_codecs(container)
		.first()
		.copied()
		.ok_or_else(|| format!("format {format} has no audio codec"))?;
	let (width, height, rate) = {
		let g = lock(p);
		super::graphops::sequence_video_params(&g.graph, seq)
			.ok_or_else(|| "the sequence has no video parameters".to_string())?
	};
	let rate_num = rate.numerator().max(1) as i32;
	let rate_den = rate.denominator().max(1) as i32;

	// Export range: the work area when enabled, otherwise the whole
	// sequence. Frames -> seconds rationals in the sequence's frame-rate
	// timebase (frame duration = rate_den / rate_num).
	let (has_custom_range, range_in, range_out, length) =
		match workarea.filter(|(s, e)| e > s) {
			Some((in_ts, out_ts)) => (
				true,
				Rational::new(in_ts * i64::from(rate_den), i64::from(rate_num)),
				Rational::new(out_ts * i64::from(rate_den), i64::from(rate_num)),
				Rational::new((out_ts - in_ts) * i64::from(rate_den), i64::from(rate_num)),
			),
			None => (
				false,
				Rational::new(0, 1),
				Rational::new(0, 1),
				Rational::new(length_frames.max(0) * i64::from(rate_den), i64::from(rate_num)),
			),
		};
	Ok(oak_task::export::EncodingParams {
		filename: path.to_string_lossy().into_owned(),
		format,
		video_enabled: true,
		video_codec: video_codec as i32,
		video_width: width.max(1),
		video_height: height.max(1),
		video_time_base_num: rate_den,
		video_time_base_den: rate_num,
		video_pixel_format: 0,
		audio_enabled: true,
		audio_codec: audio_codec as i32,
		audio_sample_rate: EXPORT_SAMPLE_RATE,
		audio_channel_layout: EXPORT_CHANNEL_LAYOUT,
		subtitles_enabled: false,
		export_length_num: length.numerator() as i32,
		export_length_den: length.denominator() as i32,
		has_custom_range,
		custom_range_in_num: range_in.numerator() as i32,
		custom_range_in_den: range_in.denominator() as i32,
		custom_range_out_num: range_out.numerator() as i32,
		custom_range_out_den: range_out.denominator() as i32,
		video_bit_rate: 0,
		audio_bit_rate: 0,
		color_override_enabled: false,
		color_primaries: 0,
		color_trc: 0,
		color_space: 0,
	})
}

/// Like [`encoding_params`] but with the dialog's full settings: chosen
/// video/audio codecs, color space + bit depth, output size / rate /
/// bitrate and the export range. The sequence's parameters fill anything
/// the settings left at zero.
pub fn encoding_params_with_settings(
	p: &ProjectRef,
	seq: NodeId,
	settings: &crate::oakui::engine::ExportSettings,
	path: &std::path::Path,
	workarea: Option<(i64, i64)>,
	length_frames: i64,
) -> Result<oak_task::export::EncodingParams, String> {
	let container = oak_codec::exportformat::Format::from_i32(settings.format)
		.ok_or_else(|| format!("unknown export format {}", settings.format))?;
	// ONLY the codecs the container supports are selectable upstream (the
	// dialog rebuilds its lists from these tables); the chosen codec must
	// be in the table — a mismatch is a hard error, not a silent default.
	let video_codec = oak_codec::exportformat::Format::get_video_codecs(container)
		.iter()
		.find(|c| **c as i32 == settings.video_codec)
		.copied()
		.ok_or_else(|| format!("codec {} not supported by {container:?}", settings.video_codec))?;
	let audio_codec = oak_codec::exportformat::Format::get_audio_codecs(container)
		.iter()
		.find(|c| **c as i32 == settings.audio_codec)
		.copied()
		.ok_or_else(|| format!("audio codec {} not supported by {container:?}", settings.audio_codec))?;
	let (mut width, mut height, rate) = {
		let g = lock(p);
		super::graphops::sequence_video_params(&g.graph, seq)
			.ok_or_else(|| "the sequence has no video parameters".to_string())?
	};
	if settings.size.0 > 0 && settings.size.1 > 0 {
		width = settings.size.0;
		height = settings.size.1;
	}
	let mut rate_num = rate.numerator().max(1) as i32;
	let mut rate_den = rate.denominator().max(1) as i32;
	if settings.frame_rate > 0.0 {
		// Round to the nearest 1/1 frame; 29.97 etc. stay as-is.
		rate_num = settings.frame_rate.round().max(1.0) as i32;
		rate_den = 1;
	}

	let (has_custom_range, range_in, range_out, length) = match settings.range {
		Some((in_s, out_s)) if out_s > in_s && in_s >= 0.0 => {
			let fps = i64::from(rate_num.max(1));
			(
				true,
				Rational::new((in_s * fps as f64).round().max(0.0) as i64, fps),
				Rational::new((out_s * fps as f64).round().max(0.0) as i64, fps),
				Rational::new(((out_s - in_s) * fps as f64).round().max(0.0) as i64, fps),
			)
		}
		_ => match workarea.filter(|(s, e)| e > s) {
			Some((in_ts, out_ts)) => (
				true,
				Rational::new(in_ts * i64::from(rate_den.max(1)), i64::from(rate_num.max(1))),
				Rational::new(out_ts * i64::from(rate_den.max(1)), i64::from(rate_num.max(1))),
				Rational::new((out_ts - in_ts) * i64::from(rate_den.max(1)), i64::from(rate_num.max(1))),
			),
			None => (
				false,
				Rational::new(0, 1),
				Rational::new(0, 1),
				Rational::new(length_frames.max(0) * i64::from(rate_den.max(1)), i64::from(rate_num.max(1))),
			),
		},
	};
	let bit_depth = settings.bit_depth;
	let pixel_format = match bit_depth {
		10 => 1, // PixelFormat::U10 (only when the codec supports it; the
		         // dialog gates 10-bit behind HDR — codecs without 10-bit
		         // stay at the 8-bit default and the dialog disables HDR).
		_ => 0,  // PixelFormat::U8
	};
	Ok(oak_task::export::EncodingParams {
		filename: path.to_string_lossy().into_owned(),
		format: settings.format,
		video_enabled: true,
		video_codec: video_codec as i32,
		video_width: width.max(1),
		video_height: height.max(1),
		video_time_base_num: rate_den.max(1),
		video_time_base_den: rate_num.max(1),
		video_pixel_format: pixel_format,
		audio_enabled: true,
		audio_codec: audio_codec as i32,
		audio_sample_rate: EXPORT_SAMPLE_RATE,
		audio_channel_layout: EXPORT_CHANNEL_LAYOUT,
		subtitles_enabled: false,
		export_length_num: length.numerator() as i32,
		export_length_den: length.denominator() as i32,
		has_custom_range,
		custom_range_in_num: range_in.numerator() as i32,
		custom_range_in_den: range_in.denominator() as i32,
		custom_range_out_num: range_out.numerator() as i32,
		custom_range_out_den: range_out.denominator() as i32,
		video_bit_rate: settings.video_bitrate,
		audio_bit_rate: settings.audio_bitrate,
		color_override_enabled: true,
		color_primaries: settings.color_primaries,
		color_trc: settings.color_transfer,
		color_space: settings.color_space,
	})
}

/// Start an export of `seq` with `params` on a background thread; the
/// returned session carries the event channel and the cancel handle (the
/// facade's export task wiring over the module task's event listener and
/// cancel atom).
pub fn spawn_export(
	p: &ProjectRef,
	seq: NodeId,
	params: oak_task::export::EncodingParams,
) -> ExportSession {
	let (tx, rx) = mpsc::channel::<ExportEvent>();
	let mut driver = oak_task::task::Task::new("Exporting...", None);
	let cancel_atom = driver.get_cancel_atom();
	{
		let tx = tx.clone();
		driver.set_event_listener(Box::new(move |event| {
			let event = match event {
				oak_task::task::TaskEvent::Started => ExportEvent::Started,
				oak_task::task::TaskEvent::Progress(value) => ExportEvent::Progress(value),
				// Finished is reported by the worker below (with the error).
				oak_task::task::TaskEvent::Finished => return,
			};
			let _ = tx.send(event);
		}));
	}
	driver.set_behavior(Box::new(oak_task::export::ExportTask::new(
		(p.clone(), seq),
		params,
	)));
	std::thread::spawn(move || {
		let result = driver.start();
		let error = if result.is_ok() {
			String::new()
		} else {
			driver
				.error()
				.map(|s| s.to_string())
				.unwrap_or_else(|| "export failed".to_string())
		};
		let _ = tx.send(ExportEvent::Finished(result.is_ok(), error));
	});
	ExportSession {
		events: rx,
		cancel: Box::new(move || cancel_atom.cancel()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::oakui::graphops;

	/// Serializes the media/FFmpeg-heavy tests (the codec library is not
	/// thread-safe against concurrent decode sessions) and shares the
	/// process-global undo stack with the other app test modules.
	fn media_lock() -> std::sync::MutexGuard<'static, ()> {
		crate::oakui::graphops::test_lock()
	}

	/// A project with an HD sequence carrying one clip of the generated
	/// media on its video track.
	fn project_with_clip(media: &std::path::Path) -> (ProjectRef, NodeId, NodeId) {
		let project = graphops::create_project();
		let seq = graphops::create_sequence(&project, "Montage Test");
		let footage = graphops::import_footage(&project, media).expect("import the generated media");
		graphops::add_track(&project, seq, TrackType::Video).expect("add a video track");
		graphops::place_footage_clip(&project, seq, footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place the clip");
		(project, seq, footage)
	}

	/// The Chroma Key effect's boolean inputs read as `Boolean(false)`
	/// through the inspector's parameter path (`effect_params`), so the
	/// OfxParamsView checkboxes start UNCHECKED (white fill, black border
	/// in the app's `CheckBox`). Regression guard for the "Invert Mask 复
	/// 选框是水蓝色" report: if the boolean value ever reads as
	/// `Boolean(true)` (or anything else that maps to `Checked`) the
	/// unchecked boxes would render with the blue accent fill.
	#[test]
	fn chromakey_boolean_params_start_unchecked() {
		let _media = media_lock();
		let project = graphops::create_project();
		let clip = {
			let mut g = graphops::lock(&project);
			let (core, behavior) = oak_node::block::clip_create();
			g.graph.add_node(core, behavior)
		};
		let fx = crate::oakui::effectchain::insert(
			&project,
			clip,
			usize::MAX,
			"org.olivevideoeditor.Olive.chromakey",
		)
		.expect("insert the chromakey effect");

		let params = {
			let g = graphops::lock(&project);
			crate::oakui::effectchain::effect_params(&g.graph, fx).expect("params")
		};
		let invert = params
			.iter()
			.find(|p| p.input_id == "invert_in")
			.expect("invert_in param");
		let mask_only = params
			.iter()
			.find(|p| p.input_id == "mask_only_in")
			.expect("mask_only_in param");
		// Exactly `Boolean(false)` (NOT None / Int / anything else): a
		// different shape would still map to Unchecked, but a `true`
		// would make the checkbox render as the blue accent fill.
		assert_eq!(
			invert.value,
			oak_node::value::NodeValue::Boolean(false),
			"invert_in must start false"
		);
		assert_eq!(
			mask_only.value,
			oak_node::value::NodeValue::Boolean(false),
			"mask_only_in must start false"
		);
	}

	#[test]
	fn video_montage_covers_the_clip_range() {
		let _media = media_lock();
		let media = std::env::temp_dir().join(format!("oakapp_montage_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let (project, seq, _) = project_with_clip(&media);
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let at = |frame: i64| graphops::ts_to_rational(frame, tb);

		let montage = video_montage(&project, seq, at(0));
		assert_eq!(montage.len(), 1, "one clip covers frame 0");
		assert_eq!(montage[0].filename, media.to_string_lossy());
		assert!(video_montage(&project, seq, at(9)).len() == 1, "frame 9 is still covered");
		assert!(
			video_montage(&project, seq, at(10)).is_empty(),
			"frame 10 is past the clip's out point"
		);
		assert!(
			video_montage(&project, seq, at(-1)).is_empty(),
			"a negative time covers nothing"
		);
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// NLE stacking regression: two OPAQUE solid-color clips covering the
	/// same time on V1 (red) and V2 (blue) — the montage lists V1's clip
	/// first and V2's LAST (the topmost composites last), and the rendered
	/// frame shows V2's blue (the highest-numbered track wins, matching
	/// the timeline UI).
	#[test]
	fn video_montage_stacks_highest_track_on_top() {
		let _media = media_lock();
		oak_undo::global::clear().unwrap();
		let red =
			std::env::temp_dir().join(format!("oakapp_stack_red_{}.mp4", std::process::id()));
		let blue =
			std::env::temp_dir().join(format!("oakapp_stack_blue_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [0.9, 0.1, 0.1, 1.0])
			.expect("generate the red media");
		oak_codec::testmedia::write_test_clip_solid(&blue, 64, 64, 10, 10, [0.1, 0.1, 0.9, 1.0])
			.expect("generate the blue media");

		let project = graphops::create_project();
		let seq = graphops::create_sequence(&project, "Stack Montage");
		let red_footage = graphops::import_footage(&project, &red).expect("import the red media");
		let blue_footage = graphops::import_footage(&project, &blue).expect("import the blue media");
		graphops::place_footage_clip(&project, seq, red_footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place the V1 (red) clip");
		graphops::place_footage_clip(&project, seq, blue_footage, TrackType::Video, 1, 0, 10, 0)
			.expect("place the V2 (blue) clip");
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let time = graphops::ts_to_rational(0, tb);

		// Bottom-to-top: V1's (red) clip first, V2's (blue) last.
		let montage = video_montage(&project, seq, time);
		assert_eq!(montage.len(), 2, "both clips cover frame 0");
		assert_eq!(montage[0].filename, red.to_string_lossy(), "V1's clip is the bottom of the stack");
		assert_eq!(
			montage[1].filename,
			blue.to_string_lossy(),
			"V2's clip is the top of the stack (composited last)"
		);

		// Render through the same entry point the render worker uses: V2's
		// blue covers V1's red.
		let params = VideoTicketParams {
			viewer: 0,
			project: String::new(),
			time,
			force_size: Some((64, 64)),
			force_format: Some(oak_core::PixelFormat::F32),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage,
		};
		let mut dst = vec![0u8; 64 * 64 * 16];
		oak_render::eval::render_montage_frame_into(time, &params, (64, 64), &mut dst, 64 * 16)
			.expect("montage render");
		let off = (8 * 64 + 8) * 16;
		let r = f32::from_le_bytes(dst[off..off + 4].try_into().unwrap());
		let b = f32::from_le_bytes(dst[off + 8..off + 12].try_into().unwrap());
		assert!(b > 0.5 && r < 0.4, "V2's blue covers V1's red (r={r}, b={b})");

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&red);
		let _ = std::fs::remove_file(&blue);
	}

	/// The acceptance gate for montage effect rendering: a 50% Opacity on
	/// a real clip (generated media, real graph, real decode, real
	/// compositing — nothing mocked) must change the rendered pixels, and
	/// disabling the effect must restore the plain render byte-for-byte.
	#[test]
	fn montage_effect_stack_reaches_the_rendered_pixels() {
		let _media = media_lock();
		// This test verifies the effect stack MECHANICS (opacity changes
		// pixels; disabling restores them), not the color pipeline. Pin the
		// legacy sRGB pass-through so the pixel-value assertions hold
		// regardless of the ACEScg default.
		oak_core::color::set_pipeline_color_settings(
            oak_core::colormath::WorkingColorSpace::SrgbLegacy,
            oak_core::colormath::OutputColorSpec::default(),
		);
		oak_undo::global::clear().unwrap();
		let media =
			std::env::temp_dir().join(format!("oakapp_montage_fx_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let project = graphops::create_project();
		let seq = graphops::create_sequence(&project, "Effect Montage");
		let footage = graphops::import_footage(&project, &media).expect("import the generated media");
		graphops::add_track(&project, seq, TrackType::Video).expect("add a video track");
		let block = graphops::place_footage_clip(&project, seq, footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place the clip");

		// The effect-library double-click flow: append a 50% Opacity to the
		// footage-backed clip (the insert rewires footage -> effect -> clip).
		let fx = crate::oakui::effectchain::insert(
			&project,
			block,
			usize::MAX,
			"org.olivevideoeditor.Olive.opacity",
		)
		.expect("append the opacity effect");
		crate::oakui::effectchain::set_input_value(
			&project,
			fx,
			"opacity_in",
			oak_node::value::NodeValue::Float(0.5),
		)
		.expect("set the opacity value");

		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let time = graphops::ts_to_rational(0, tb);

		// Render the montage through the same entry point the render worker
		// uses (`render_montage_frame_into`, F32 RGBA rows).
		let render = |montage: Vec<MontageClip>| {
			let params = VideoTicketParams {
				viewer: 0,
				project: String::new(),
				time,
				force_size: Some((64, 64)),
				force_format: Some(oak_core::PixelFormat::F32),
				cache: None,
				cache_dir: None,
				cache_id: None,
				cache_timebase: None,
				footage: None,
				montage,
			};
			let mut dst = vec![0u8; 64 * 64 * 16];
			oak_render::eval::render_montage_frame_into(time, &params, (64, 64), &mut dst, 64 * 16)
				.expect("montage render");
			dst
		};
		// A left-half pixel of the test pattern (known content, r ~= 0.9).
		let pixel = |frame: &[u8]| {
			let off = (8 * 64 + 8) * 16;
			[0, 1, 2, 3].map(|i| f32::from_le_bytes(frame[off + i * 4..off + i * 4 + 4].try_into().unwrap()))
		};

		let with_fx = video_montage(&project, seq, time);
		assert_eq!(with_fx.len(), 1, "one clip covers frame 0");
		assert_eq!(
			with_fx[0].effects.len(),
			1,
			"the montage carries the clip's effect stack (not the footage source node)"
		);
		assert_eq!(with_fx[0].effects[0].type_id, "org.olivevideoeditor.Olive.opacity");
		assert!(with_fx[0].effects[0].enabled);

		let plain = render(
			with_fx
				.iter()
				.cloned()
				.map(|mut c| {
					c.effects.clear();
					c
				})
				.collect(),
		);
		let effected = render(with_fx);
		let (p, e) = (pixel(&plain), pixel(&effected));
		assert!(p[0] > 0.6, "the plain render shows the test pattern (r={})", p[0]);
		// 50% opacity: the shader halves every channel, then the composite
		// over transparent black halves it again via the halved alpha.
		assert!(
			(e[0] - p[0] * 0.25).abs() < 0.1,
			"50% opacity quarters the output ({} vs {})",
			e[0],
			p[0] * 0.25
		);
		assert!(
			(e[3] - 0.5).abs() < 0.1,
			"the composited alpha halves (a={})",
			e[3]
		);

		// Disabled effect = bypass: pixels match the plain render.
		crate::oakui::effectchain::set_enabled(&project, fx, false).expect("disable the effect");
		let disabled_montage = video_montage(&project, seq, time);
		assert_eq!(disabled_montage[0].effects.len(), 1);
		assert!(!disabled_montage[0].effects[0].enabled);
		let disabled = render(disabled_montage);
		let d = pixel(&disabled);
		assert!(
			(d[0] - p[0]).abs() < 0.05,
			"a disabled effect leaves the pixels alone ({} vs {})",
			d[0],
			p[0]
		);

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// A multi-cam HOST clip's montage media is the CURRENT source's angle
	/// clip (footage + media-in mapped from the host position) — without
	/// this the host clip's texture chain has no footage and the sequence
	/// viewer paints black.
	#[test]
	fn clip_multicam_media_resolves_the_current_angle() {
		let _media = media_lock();
		let media =
			std::env::temp_dir().join(format!("oakapp_mcmedia_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let (project, seq, footage) = project_with_clip(&media);
		// Build the multicam shape: source seq (the angle track), multicam
		// node, host seq with a clip fed from the multicam output.
		let source_seq = graphops::create_sequence(&project, "Angles");
		graphops::add_track(&project, source_seq, TrackType::Video).expect("angle track");
		graphops::place_footage_clip(&project, source_seq, footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place the angle clip");
		let angle_clip = {
			let g2 = graphops::lock(&project);
			let track = graphops::track_ids(&g2.graph, source_seq, TrackType::Video)[0];
			graphops::track_behavior(&g2.graph, track)
				.expect("angle track")
				.blocks
				.first()
				.copied()
				.expect("angle clip")
		};
		let mc = {
			let mut g = graphops::lock(&project);
			let (core, behavior) = oak_node::nodes::multicamnode::create();
			let id = g.graph.add_node(core, behavior);
			// Default current source: 0.
			if let Some(core) = g.graph.get_mut(id).map(|e| &mut e.core) {
				core.set_standard_value(
					oak_node::nodes::multicamnode::CURRENT_INPUT,
					-1,
					oak_node::value::NodeValue::Combo(0),
				);
				}
			g.graph
				.connect(source_seq, id, oak_node::nodes::multicamnode::SEQUENCE_INPUT, -1)
				.unwrap();
			// The angle CLIP feeds the source array at element 0 (the
			// traverser's `active_elements_at_time` keeps only the current
			// source, so multi `value()` forwards exactly it).
			g.graph
				.input_array_insert(
					id,
					oak_node::nodes::multicamnode::SOURCES_INPUT,
					0,
				)
				.unwrap();
			g.graph
				.connect(
					angle_clip,
					id,
					oak_node::nodes::multicamnode::SOURCES_INPUT,
					0,
				)
				.unwrap();
			id
		};
		// A host clip on the host sequence (default layout V1) fed from the
		// multicam output; the clip's own input must first be connected.
		let host_clip = graphops::place_footage_clip(&project, seq, footage, TrackType::Video, 0, 0, 10, 0)
			.expect("place the host clip");
		{
			let mut g = graphops::lock(&project);
			let _ = g
				.graph
				.disconnect(footage, host_clip, oak_node::block::clip_input::TEXTURE_INPUT, -1);
			g.graph
				.connect(mc, host_clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}
		// The montage at host time 0 resolves to the angle's media.
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let montage = {
			let g = graphops::lock(&project);
			clip_multicam_media(&g.graph, host_clip, graphops::ts_to_rational(0, tb))
		};
		let (filename, _stream, _media_in) = montage.expect("the current angle resolves");
		assert_eq!(
			filename,
			media.to_string_lossy(),
			"the angle's footage decodes for the host clip"
		);

		// The FULL sequence-viewer path: video_montage of the host sequence
		// -> render_montage_frame_into must produce a non-black frame (the
		// host clip's angle media reaches the pixels — the "multi-cam clip
		// shows black in the sequence viewer" report).
		let montage = video_montage(&project, seq, graphops::ts_to_rational(0, tb));
		assert_eq!(montage.len(), 1, "the host clip is the only video entry");
		let params = VideoTicketParams {
			viewer: 0,
			project: String::new(),
			time: graphops::ts_to_rational(0, tb),
			force_size: Some((64, 64)),
			force_format: Some(oak_core::PixelFormat::F32),
			cache: None,
			cache_dir: None,
			cache_id: None,
			cache_timebase: None,
			footage: None,
			montage,
		};
		let mut dst = vec![0u8; 64 * 64 * 16];
		oak_render::eval::render_montage_frame_into(
			graphops::ts_to_rational(0, tb),
			&params,
			(64, 64),
			&mut dst,
			64 * 16,
		)
		.expect("montage render");
		let off = (8 * 64 + 8) * 16;
		let r = f32::from_le_bytes(dst[off..off + 4].try_into().unwrap());
		assert!(
			r > 0.05,
			"the sequence viewer shows the angle's frame, not black (r={r})"
		);

		// The NODE-GRAPH evaluation path: `render_graph_frame` must also
		// produce the current angle (footage → angle clip → multi →
		// host clip → composite, all through `traverser::evaluate` + the
		// value() chain — the "preview bypasses the node graph" fix).
		// Needs the render manager initialized? No — this inline eval is
		// pure (render_footage_frame decodes directly).
		let graph_frame = oak_render::eval::render_graph_frame(
			&project,
			seq,
			graphops::ts_to_rational(0, tb),
			(64, 64),
			oak_core::PixelFormat::F32,
		)
		.expect("graph render");
		let grow;
		let gdata;
		let goff;
		{
			let oak_core::texture::Texture::Cpu(ref gf) = &graph_frame else {
				panic!("graph render produced a non-CPU frame");
			};
			grow = gf.linesize_bytes();
			gdata = gf.data.clone();
		}
		goff = (8 * grow as usize + 8 * 16) as usize;
		let gr = f32::from_le_bytes(gdata[goff..goff + 4].try_into().unwrap());
		assert!(
			gr > 0.05,
			"the node-graph preview shows the angle's frame, not black (r={gr})"
		);

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// The multi-cam NODE-GRAPH source switch: two solid-color angles
	/// (red/blue), the traverser evaluates `sources_in[source]` — the
	/// rendered frame turns blue when `current_in` switches to source 1
	/// (the "value() always read None / the node graph is bypassed"
	/// report's full regression).
	#[test]
	fn graph_render_switches_multicam_source() {
		let _media = media_lock();
		let red = std::env::temp_dir().join(format!("oakapp_mcsw_r_{}.mp4", std::process::id()));
		let blue = std::env::temp_dir().join(format!("oakapp_mcsw_b_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [1.0, 0.0, 0.0, 1.0])
			.expect("red angle");
		oak_codec::testmedia::write_test_clip_solid(&blue, 64, 64, 10, 10, [0.0, 0.0, 1.0, 1.0])
			.expect("blue angle");

		let (project, seq, _rfoot) = project_with_clip(&red);
		let bfoot = graphops::import_footage(&project, &blue).expect("import blue");
		graphops::add_track(&project, seq, TrackType::Video).expect("track 1");
		graphops::place_footage_clip(&project, seq, _rfoot, TrackType::Video, 1, 0, 10, 0)
			.expect("red angle slot");

		// The multicam: order matters — source 0 = red, source 1 = blue.
		let track_red = {
			let g = graphops::lock(&project);
			graphops::track_ids(&g.graph, seq, TrackType::Video)[0]
		};
		let red_clip = {
			let g = graphops::lock(&project);
			graphops::track_behavior(&g.graph, track_red)
				.expect("track 0")
				.blocks
				.first()
				.copied()
				.expect("red clip")
		};
		let blue_clip = graphops::place_footage_clip(&project, seq, bfoot, TrackType::Video, 1, 0, 10, 0)
			.expect("blue angle slot");
		let mc = {
			let mut g = graphops::lock(&project);
			let (core, behavior) = oak_node::nodes::multicamnode::create();
			let id = g.graph.add_node(core, behavior);
			if let Some(core) = g.graph.get_mut(id).map(|e| &mut e.core) {
				core.set_standard_value(
					oak_node::nodes::multicamnode::CURRENT_INPUT,
					-1,
					oak_node::value::NodeValue::Combo(0),
				);
			}
			g.graph
				.input_array_insert(id, oak_node::nodes::multicamnode::SOURCES_INPUT, 0)
				.unwrap();
			g.graph
				.connect(red_clip, id, oak_node::nodes::multicamnode::SOURCES_INPUT, 0)
				.unwrap();
			g.graph
				.input_array_insert(id, oak_node::nodes::multicamnode::SOURCES_INPUT, 1)
				.unwrap();
			g.graph
				.connect(blue_clip, id, oak_node::nodes::multicamnode::SOURCES_INPUT, 1)
				.unwrap();
			id
		};
		// Host clip fed from the multicam output.
		let host_clip =
			graphops::place_footage_clip(&project, seq, _rfoot, TrackType::Video, 2, 0, 10, 0)
				.expect("host clip");
		{
			let mut g = graphops::lock(&project);
			let _ = g.graph.disconnect(
				_rfoot,
				host_clip,
				oak_node::block::clip_input::TEXTURE_INPUT,
				-1,
			);
			g.graph
				.connect(mc, host_clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}

		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let at = graphops::ts_to_rational(0, tb);
		let render_rgb = |mc_id: u64| -> (f32, f32) {
			let pixel = {
				let texture = oak_render::eval::render_graph_frame(
					&project,
					seq,
					at,
					(64, 64),
					oak_core::PixelFormat::F32,
				)
				.expect("graph render");
				let oak_core::texture::Texture::Cpu(ref gf) = texture else {
					panic!("non-CPU frame");
				};
				let stride = gf.linesize_bytes();
				let off = (8 * stride as usize + 8 * 16) as usize;
				(
					f32::from_le_bytes(gf.data[off..off + 4].try_into().unwrap()),
					f32::from_le_bytes(gf.data[off + 8..off + 12].try_into().unwrap()),
				)
			};
			let _ = mc_id;
			pixel
		};
		// Source 0 = red angle.
		let (r0, b0) = render_rgb(0);
		assert!(r0 > 0.4 && b0 < 0.4, "source 0 is the red angle (r={r0} b={b0})");

		// Switch to source 1: current_in drives the element the multi
		// `value()` forwards — the frame must turn blue.
		{
			let mut g = graphops::lock(&project);
			if let Some(core) = g.graph.get_mut(mc).map(|e| &mut e.core) {
				core.set_standard_value(
					oak_node::nodes::multicamnode::CURRENT_INPUT,
					-1,
					oak_node::value::NodeValue::Combo(1),
				);
			}
		}
		let (r1, b1) = render_rgb(0);
		assert!(
			b1 > 0.4 && r1 < 0.4,
			"source 1 switches the node-graph frame to blue (r={r1} b={b1})"
		);

		// Playback load profile: 30 sequential graph frames vs the montage
		// shortcut — the "now that the graph renders, previews crawl"
		// report's baseline (in-process, no worker). A wide gap means the
		// graph path needs a frame/decoder cache, not just correctness.
		let measure_frames = |graph_mode: bool| -> f64 {
			let start = std::time::Instant::now();
			let mut frames = 0;
			for f in 1..=30 {
				if graph_mode {
					let texture = oak_render::eval::render_graph_frame(
						&project,
						seq,
						graphops::ts_to_rational(f, tb),
						(64, 64),
						oak_core::PixelFormat::F32,
					)
					.expect("graph frame");
					let _ = texture;
				} else {
					let montage = video_montage(&project, seq, graphops::ts_to_rational(f, tb));
					let params = VideoTicketParams {
						viewer: 0,
						project: String::new(),
						time: graphops::ts_to_rational(f, tb),
						force_size: Some((64, 64)),
						force_format: Some(oak_core::PixelFormat::F32),
						cache: None,
						cache_dir: None,
						cache_id: None,
						cache_timebase: None,
						footage: None,
						montage,
					};
					let mut dst = vec![0u8; 64 * 64 * 16];
					oak_render::eval::render_montage_frame_into(
						graphops::ts_to_rational(f, tb),
						&params,
						(64, 64),
						&mut dst,
						64 * 16,
					)
					.expect("montage frame");
				}
				frames += 1;
			}
			start.elapsed().as_secs_f64() / frames as f64
		};
		let graph_ms = measure_frames(true) * 1000.0;
		let montage_ms = measure_frames(false) * 1000.0;
		eprintln!(
			"[perf] graph {:?} ms/frame vs montage {:?} ms/frame (30 frames, 64x64)",
			graph_ms, montage_ms
		);

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&red);
		let _ = std::fs::remove_file(&blue);
	}
	/// the kind (not the hardcoded 0/1 of a typical layout): a file whose
	/// video stream is not stream 0 must still decode its own video when
	/// the proxy switch is off or the proxy is not ready.
	#[test]
	fn preview_media_uses_the_probed_stream_indices() {
		let stream = |index: i32, is_video: bool| oak_node::footage::StreamInfo {
			index,
			is_video,
			video: None,
			audio: None,
			duration: oak_core::Rational::new(0, 1),
		};
		// An audio-first container: audio at 0, video at 1… plus a second
		// audio track at 2. The video must come from stream 1 and the audio
		// from stream 0, not the legacy 0/1 assumption.
		let mut f = oak_node::footage::FootageBehavior::new("/tmp/audio-first.mov");
		f.streams = vec![stream(0, false), stream(1, true), stream(2, false)];
		assert_eq!(preview_footage_media(&f, true).1, 1);
		assert_eq!(preview_footage_media(&f, false).1, 0);
		// Unprobed footage (no stream metadata) keeps the 0/1 fallbacks.
		let unprobed = oak_node::footage::FootageBehavior::new("/tmp/legacy.mov");
		assert_eq!(preview_footage_media(&unprobed, true).1, 0);
		assert_eq!(preview_footage_media(&unprobed, false).1, 1);
	}

	/// The multicam angle montage ([`single_track_video_montage`]) carries
	/// ONLY the clip on the requested track — the whole-stack `video_montage`
	/// is the parity reference. This is the montage the angle-frame ticket
	/// renders for each grid cell.
	#[test]
	fn single_track_montage_isolates_its_track() {
		let _media = media_lock();
		let media =
			std::env::temp_dir().join(format!("oakapp_montage_ang_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let (project, seq, footage) = project_with_clip(&media);
		graphops::add_track(&project, seq, TrackType::Video).expect("add a second video track");
		// A second clip on track 1 overlapping the same time.
		graphops::place_footage_clip(&project, seq, footage, TrackType::Video, 1, 0, 10, 0)
			.expect("place the second clip");
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let at = |frame: i64| graphops::ts_to_rational(frame, tb);
		let tracks = {
			let g = lock(&project);
			graphops::track_ids(&g.graph, seq, TrackType::Video)
		};
		// The two default video tracks plus the two added here.
		assert_eq!(tracks.len(), 4, "default 2 video tracks + 2 added");

		// The whole stack sees both clips; the single-track montage sees only
		// its own track's clip.
		assert_eq!(video_montage(&project, seq, at(0)).len(), 2);
		let track0_only = single_track_video_montage(&project, seq, tracks[0], at(0));
		assert_eq!(track0_only.len(), 1, "track 0 contributes its own clip");
		assert_eq!(track0_only[0].filename, media.to_string_lossy());
		let track1_only = single_track_video_montage(&project, seq, tracks[1], at(0));
		assert_eq!(track1_only.len(), 1, "track 1 contributes its own clip");

		// A hidden video track contributes nothing (the angle's track is
		// skipped, matching the full montage's muted-track rule).
		graphops::set_track_muted(&project, tracks[0], true).expect("hide track 0");
		assert!(
			single_track_video_montage(&project, seq, tracks[0], at(0)).is_empty(),
			"a hidden angle track renders nothing"
		);
		assert_eq!(
			single_track_video_montage(&project, seq, tracks[1], at(0)).len(),
			1,
			"the other track is unaffected"
		);

		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	#[test]
	fn audio_montage_overlaps_the_range() {
		let _media = media_lock();
		let media = std::env::temp_dir().join(format!("oakapp_montage_a_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let (project, seq, footage) = project_with_clip(&media);
		graphops::add_track(&project, seq, TrackType::Audio).expect("add an audio track");
		graphops::place_footage_clip(&project, seq, footage, TrackType::Audio, 0, 0, 10, 0)
			.expect("place the audio clip");
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let at = |frame: i64| graphops::ts_to_rational(frame, tb);

		let overlapping = audio_montage(&project, seq, TimeRange::new(at(0), at(5)));
		assert_eq!(overlapping.len(), 1, "the range overlaps the audio clip");
		assert_eq!(overlapping[0].stream_index, 1, "the audio stream is selected");
		let disjoint = audio_montage(&project, seq, TimeRange::new(at(10), at(20)));
		assert!(disjoint.is_empty(), "a range past the clip overlaps nothing");
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// Hidden video tracks and muted audio tracks (both are the track's
	/// `muted` flag) contribute nothing to the montages; undoing the flag
	/// set restores them.
	#[test]
	fn montages_skip_muted_tracks() {
		let _media = media_lock();
		let media = std::env::temp_dir().join(format!("oakapp_montage_m_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("generate test media");

		let (project, seq, footage) = project_with_clip(&media);
		graphops::add_track(&project, seq, TrackType::Audio).expect("add an audio track");
		graphops::place_footage_clip(&project, seq, footage, TrackType::Audio, 0, 0, 10, 0)
			.expect("place the audio clip");
		let (video_track, audio_track, tb) = {
			let g = lock(&project);
			(
				graphops::track_ids(&g.graph, seq, TrackType::Video)[0],
				graphops::track_ids(&g.graph, seq, TrackType::Audio)[0],
				graphops::sequence_time_base(&g.graph, seq).unwrap(),
			)
		};
		let at = |frame: i64| graphops::ts_to_rational(frame, tb);
		let range = TimeRange::new(at(0), at(5));
		assert_eq!(video_montage(&project, seq, at(0)).len(), 1);
		assert_eq!(audio_montage(&project, seq, range).len(), 1);

		graphops::set_track_muted(&project, video_track, true).expect("hide the video track");
		graphops::set_track_muted(&project, audio_track, true).expect("mute the audio track");
		assert!(
			video_montage(&project, seq, at(0)).is_empty(),
			"a hidden video track contributes nothing"
		);
		assert!(
			audio_montage(&project, seq, range).is_empty(),
			"a muted audio track contributes nothing"
		);

		oak_undo::global::undo().unwrap();
		oak_undo::global::undo().unwrap();
		assert_eq!(video_montage(&project, seq, at(0)).len(), 1, "undo restores the video clip");
		assert_eq!(audio_montage(&project, seq, range).len(), 1, "undo restores the audio clip");
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&media);
	}

	/// The background full-res geometry follows the playback resolution
	/// divider (the viewer `Playback Resolution ▸` menu): a paused
	/// Half/Quarter/Eighth preview must not be overwritten by a native-size
	/// background fill, so the fill renders at the same divider the window
	/// shows.
	#[test]
	fn full_res_size_follows_the_playback_divider() {
		assert_eq!(full_res_render_size(1920, 1080, 1), (1920, 1080));
		assert_eq!(full_res_render_size(1920, 1080, 2), (960, 540));
		assert_eq!(full_res_render_size(1920, 1080, 4), (480, 270));
		assert_eq!(full_res_render_size(1920, 1080, 8), (240, 135));
		// An unprobed/zero format stays sane, and the divider never
		// underflows the renderable minimum.
		assert_eq!(full_res_render_size(0, 0, 1), (2, 2));
		assert_eq!(full_res_render_size(1, 1, 8), (2, 2));
		assert_eq!(full_res_render_size(1920, 1080, 0), (1920, 1080), "a zero divider means Full");
	}

	/// Clamping only shrinks: a render that already fits a limit is left
	/// alone (no pointless upscale), and the aspect ratio is preserved
	/// while every limit rectangle is respected.
	#[test]
	fn render_size_clamps_to_proxy_without_upscaling() {
		assert_eq!(clamp_render_size(1920, 1080, &[(1280, 720)]), (1280, 720));
		assert_eq!(
			clamp_render_size(960, 540, &[(1280, 720)]),
			(960, 540),
			"a smaller request is not upscaled to the proxy"
		);
		assert_eq!(clamp_render_size(1920, 1080, &[]), (1920, 1080), "no limits = no clamping");
		assert_eq!(
			clamp_render_size(1920, 1080, &[(640, 640)]),
			(640, 360),
			"the tighter dimension (width 640/1920 = 1/3) wins"
		);
		assert_eq!(
			clamp_render_size(1920, 1080, &[(640, 640), (1280, 720)]),
			(640, 360),
			"the smallest limit binds"
		);
	}

	/// The proxy-clamping acceptance gate: with the global `UseProxyMedia`
	/// switch on and a footage whose proxy is a ready file on disk, the
	/// sequence render is bounded by the proxy's own resolution — the 64x64
	/// source proxied at 32x32 renders at 32x32 (never decoded up to the
	/// native size) and the pixels come from the proxy file.
	#[test]
	fn proxy_resolution_bounds_the_sequence_render() {
		let _media = media_lock();
		oak_undo::global::clear().unwrap();
		let orig =
			std::env::temp_dir().join(format!("oakapp_proxy_src_{}.mp4", std::process::id()));
		let proxy =
			std::env::temp_dir().join(format!("oakapp_proxy_32_{}.mp4", std::process::id()));
		oak_codec::testmedia::write_test_clip(&orig, 64, 64, 10, 10).expect("generate the source media");
		oak_codec::testmedia::write_test_clip_solid(&proxy, 32, 32, 10, 10, [0.1, 0.1, 0.9, 1.0])
			.expect("generate the proxy media");

		// Force the global proxy switch on and restore it afterwards (the
		// config store is process-global; the serialization lock held
		// above is the same one the other app test modules use).
		let store = oak_core::configstore::ConfigStore::instance();
		let old = store.get(None, "UseProxyMedia").unwrap_or_default();
		store.set_bool(None, "UseProxyMedia", 1);

		let (project, seq, footage) = project_with_clip(&orig);
		{
			let mut g = lock(&project);
			let f = g
				.graph
				.get_mut(footage)
				.and_then(|e| e.behavior.as_any_mut()?.downcast_mut::<oak_node::footage::FootageBehavior>())
				.expect("the footage behavior");
			f.proxy = proxy.to_string_lossy().into_owned();
			f.proxy_enabled = true;
			f.proxy_state = 2;
			f.proxy_video_stream_index = 0;
			f.custom_proxy_params = Some(oak_codec::proxymanager::ProxyParams {
				width: 32,
				height: 32,
				..Default::default()
			});
		}
		// The preview media is the proxy file…
		let selected = {
			let g = lock(&project);
			let f = graphops::footage_behavior(&g.graph, footage).expect("the footage behavior");
			preview_footage_media(f, true).0
		};
		assert_eq!(selected, proxy.to_string_lossy(), "the proxy stands in for the source");

		// …and the sequence frame ticket is clamped to its 32x32 size.
		let tb = graphops::sequence_time_base(&lock(&project).graph, seq).unwrap();
		let params =
			sequence_frame_params(&project, seq, 0, tb, 64, 64, None).expect("sequence frame params");
		assert_eq!(params.force_size, Some((32, 32)), "the proxy bounds the render size");

		// The decoded pixels are the proxy's (solid blue), not the source's.
		let mut dst = vec![0u8; 32 * 32 * 16];
		oak_render::eval::render_montage_frame_into(params.time, &params, (32, 32), &mut dst, 32 * 16)
			.expect("montage render at the proxy size");
		let off = (8 * 32 + 8) * 16;
		let r = f32::from_le_bytes(dst[off..off + 4].try_into().unwrap());
		let b = f32::from_le_bytes(dst[off + 8..off + 12].try_into().unwrap());
		assert!(b > 0.5 && r < 0.4, "the proxy's blue covers the frame (r={r}, b={b})");

		store.set(None, "UseProxyMedia", &old);
		oak_undo::global::clear().unwrap();
		let _ = std::fs::remove_file(&orig);
		let _ = std::fs::remove_file(&proxy);
	}
}
