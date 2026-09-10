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

//! Adjustment layers on the graph render path: a video track whose enabled
//! adjustment block covers the request time composites every track
//! underneath, pushes that composite through the block's effect chain and
//! lets the chain's output replace the lower stack — one block affects all
//! lower tracks, clip boundaries included. A bare block (no effect chain)
//! has no head to feed and is inert.

use std::sync::{Arc, Mutex};

use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::block::{AdjustmentBlockBehavior, ClipBlockBehavior};
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior};

mod common;

/// Unique temp path per test (the process id disambiguates parallel test
/// binaries; the tag separates tests inside one binary).
fn clip_path(tag: &str) -> std::path::PathBuf {
	std::env::temp_dir().join(format!("oakrender_adjust_{tag}_{}.mp4", std::process::id()))
}

/// These tests verify graph/composite MECHANICS (the adjustment sweep), not
/// the color pipeline. Pin the working space to the legacy sRGB
/// pass-through so the decoded pixels stay display-referred and the
/// pixel-value assertions hold regardless of the ACEScg default. All tests
/// in this binary set the same value, so the shared global is race-free.
fn pin_legacy_working_space() {
	oak_core::color::set_pipeline_color_settings(
		oak_core::colormath::WorkingColorSpace::SrgbLegacy,
		oak_core::colormath::OutputColorSpec::default(),
	);
}

/// V1 (a single track carrying every clip of `clips`, in order) plus an
/// optional V2 adjustment layer. V2 goes in last, so it is the top of the
/// stack and sweeps V1. `adjustment` is `(in, out, opacity)`: `Some(opacity)`
/// builds the effect chain (an Opacity node feeding the block's `tex_in`,
/// its own `tex_in` left open as the chain head), `None` leaves the block
/// bare.
fn build_project(
	clips: &[(&str, Rational, Rational)],
	adjustment: Option<(Rational, Rational, Option<f64>)>,
) -> (Arc<Mutex<Project>>, NodeId) {
	pin_legacy_working_space();
	let project = Project::new();
	let seq;
	{
		let mut p = project.lock().unwrap();
		let (score, sbehavior) = SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);

		let (tcore, tbehavior) = TrackListBehavior::create();
		let tl = p.graph.add_node(tcore, tbehavior);

		let (tcore, tbehavior) = TrackBehavior::create();
		let v1 = p.graph.add_node(tcore, tbehavior);
		for &(path, in_, out) in clips {
			let mut footage = FootageBehavior::new(path);
			footage.probe().expect("probe the generated clip");
			let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

			let (ccore, cbehavior) = oak_node::block::clip_create();
			let clip = p.graph.add_node(ccore, cbehavior);
			p.graph
				.connect(
					footage,
					clip,
					oak_node::block::clip_input::TEXTURE_INPUT,
					-1,
				)
				.expect("connect footage to clip");

			p.graph
				.get_mut(clip)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<ClipBlockBehavior>()
				.expect("clip block")
				.core
				.range = TimeRange::new(in_, out);

			p.graph
				.get_mut(v1)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.expect("video track")
				.append_block(clip);
		}
		p.graph
			.get_mut(tl)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.expect("video track list")
			.tracks
			.push(v1);

		if let Some((in_, out, opacity)) = adjustment {
			let (tcore, tbehavior) = TrackBehavior::create();
			let v2 = p.graph.add_node(tcore, tbehavior);

			let (acore, abehavior) = oak_node::block::adjustment_create();
			let block = p.graph.add_node(acore, abehavior);
			p.graph
				.get_mut(block)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<AdjustmentBlockBehavior>()
				.expect("adjustment block")
				.core
				.range = TimeRange::new(in_, out);

			if let Some(value) = opacity {
				let (ecore, ebehavior) = oak_node::nodes::opacity::create();
				let effect = p.graph.add_node(ecore, ebehavior);
				p.graph
					.connect(
						effect,
						block,
						oak_node::block::adjustment_input::TEXTURE_INPUT,
						-1,
					)
					.expect("connect opacity to the adjustment block");
				p.graph.get_mut(effect).unwrap().core.set_standard_value(
					oak_node::nodes::opacity::VALUE_INPUT,
					-1,
					oak_node::value::NodeValue::Float(value),
				);
			}

			p.graph
				.get_mut(v2)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.expect("video track")
				.append_block(block);
			p.graph
				.get_mut(tl)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackListBehavior>()
				.expect("video track list")
				.tracks
				.push(v2);
		}

		p.graph
			.get_mut(seq)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<SequenceBehavior>()
			.expect("sequence")
			.track_lists
			.push(tl);
	}
	(project, seq)
}

/// The raw CPU frame bytes of a rendered texture.
fn frame_data(texture: &Texture) -> &[u8] {
	let Texture::Cpu(frame) = texture else {
		panic!("graph render produced a non-CPU texture");
	};
	&frame.data
}

/// Render one 64x64 F32 frame of `seq` at `time` and return its bytes.
fn render_frame(project: &Arc<Mutex<Project>>, seq: NodeId, time: Rational) -> Vec<u8> {
	let texture =
		oak_render::eval::render_graph_frame(project, seq, time, (64, 64), PixelFormat::F32)
			.expect("graph render");
	frame_data(&texture).to_vec()
}

/// The F32 RGBA channel of a 64x64 frame at `(x, y)`.
fn channel(data: &[u8], x: usize, y: usize, c: usize) -> f32 {
	let off = (y * 64 + x) * 16 + c * 4;
	f32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

/// An adjustment layer over a single V1 track whose two solid clips abut at
/// t=1/2: inside the layer's span BOTH clips come out of the effect chain
/// (the red clip on the left half, the blue one on the right half of the
/// span), so one block reaches across the clip boundary; outside the span
/// the render is byte-identical to the same project without the layer.
///
/// The chain is an Opacity node at 0.5, which scales the straight-alpha
/// vec4 by 0.5; the frame then goes through the alpha-over composite once
/// more (like `shader_job_opacity_halves_pixels`), so each color channel
/// ends up at a 0.25 ratio of the plain render and the alpha at 0.5.
/// Skipped (with a note) when no GPU adapter exists.
#[test]
fn adjustment_layer_affects_lower_tracks_across_clips() {
	if oak_core::backend::GpuContext::shared().is_none() {
		eprintln!("skipping adjustment_layer_affects_lower_tracks_across_clips: no GPU adapter");
		return;
	}
	let red = clip_path("across_red");
	let blue = clip_path("across_blue");
	oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [0.9, 0.1, 0.1, 1.0])
		.expect("red clip generation");
	oak_codec::testmedia::write_test_clip_solid(&blue, 64, 64, 10, 10, [0.1, 0.1, 0.9, 1.0])
		.expect("blue clip generation");

	let red_path = red.to_string_lossy().to_string();
	let blue_path = blue.to_string_lossy().to_string();
	let clips: Vec<(&str, Rational, Rational)> = vec![
		(&red_path, Rational::new(0, 1), Rational::new(1, 2)),
		(&blue_path, Rational::new(1, 2), Rational::new(1, 1)),
	];
	let plain = build_project(&clips, None);
	let adjusted = build_project(
		&clips,
		Some((Rational::new(1, 4), Rational::new(3, 4), Some(0.5))),
	);
	let nodes_before = adjusted.0.lock().unwrap().graph.node_count();

	// Inside the span, a point covered by each clip in turn.
	for (time, label) in [
		(Rational::new(1, 4), "the red clip"),
		(Rational::new(5, 8), "the blue clip"),
	] {
		let base = render_frame(&plain.0, plain.1, time);
		let swept = render_frame(&adjusted.0, adjusted.1, time);

		// Sample away from the x=32 half boundary (MPEG-2 chroma bleed and
		// luma ringing stay within a few pixels of it).
		let mut ratios: Vec<f32> = Vec::new();
		let mut alphas: Vec<f32> = Vec::new();
		for y in 4..60 {
			for x in (4..24).chain(40..60) {
				for c in 0..3 {
					let a = channel(&base, x, y, c);
					if a > 0.02 {
						ratios.push(channel(&swept, x, y, c) / a);
					}
				}
				alphas.push(channel(&swept, x, y, 3));
			}
		}
		assert!(
			ratios.len() >= 512,
			"{label}: too few comparable samples: {}",
			ratios.len()
		);
		let mean = ratios.iter().sum::<f32>() / ratios.len() as f32;
		assert!(
			(mean - 0.25).abs() < 0.02,
			"{label}: a 0.5-opacity layer must leave 0.25 of each channel (shader x0.5, alpha-over x0.5), got {mean}"
		);
		let mean_alpha = alphas.iter().sum::<f32>() / alphas.len() as f32;
		assert!(
			(mean_alpha - 0.5).abs() < 0.1,
			"{label}: the swept frame's alpha must be 0.5, got {mean_alpha}"
		);
	}

	// Outside the span, nothing changes — not even a rounding step.
	for time in [Rational::new(1, 8), Rational::new(7, 8)] {
		assert_eq!(
			render_frame(&plain.0, plain.1, time),
			render_frame(&adjusted.0, adjusted.1, time),
			"t={time:?} is outside the layer's span and must render unchanged"
		);
	}

	assert_eq!(
		adjusted.0.lock().unwrap().graph.node_count(),
		nodes_before,
		"the sweep must tear its temporary source node back down"
	);

	let _ = std::fs::remove_file(&red);
	let _ = std::fs::remove_file(&blue);
}

/// A bare adjustment layer (a block with no effect chain) is inert: there
/// is no chain head to feed, so the tracks underneath render byte for byte
/// as if the block were absent, and the block leaves no node behind.
#[test]
fn bare_adjustment_layer_passes_lower_tracks_through() {
	let red = clip_path("bare_red");
	oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [0.9, 0.1, 0.1, 1.0])
		.expect("red clip generation");
	let red_path = red.to_string_lossy().to_string();
	let clips: Vec<(&str, Rational, Rational)> =
		vec![(&red_path, Rational::new(0, 1), Rational::new(1, 1))];

	let plain = build_project(&clips, None);
	let bare = build_project(
		&clips,
		Some((Rational::new(1, 4), Rational::new(3, 4), None)),
	);
	let nodes_before = bare.0.lock().unwrap().graph.node_count();

	let time = Rational::new(1, 2);
	let base = render_frame(&plain.0, plain.1, time);
	assert!(
		base.iter().any(|&b| b != 0),
		"the reference frame is not black"
	);
	assert_eq!(
		render_frame(&bare.0, bare.1, time),
		base,
		"a bare adjustment layer must not change the render"
	);
	assert_eq!(
		bare.0.lock().unwrap().graph.node_count(),
		nodes_before,
		"a bare adjustment layer must not grow the graph"
	);

	let _ = std::fs::remove_file(&red);
}
