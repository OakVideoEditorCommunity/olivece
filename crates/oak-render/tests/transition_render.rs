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

//! Timeline transitions on the graph render path: an enabled transition
//! block covering the request time replaces the plain clip read with a
//! blend of the two clips it joins, driven by the block's style combo and
//! its position across the block's own span. Outside the span the render
//! is byte-identical to the same project without the transition.

use std::sync::{Arc, Mutex};

use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::block::{ClipBlockBehavior, TransitionBlockBehavior};
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
	std::env::temp_dir().join(format!("oakrender_trans_{tag}_{}.mp4", std::process::id()))
}

/// The transition mechanics are color-space independent, so pin the
/// working space to the legacy sRGB pass-through like the adjustment
/// layer tests do and compare decoded pixel values directly.
fn pin_legacy_working_space() {
	oak_core::color::set_pipeline_color_settings(
		oak_core::colormath::WorkingColorSpace::SrgbLegacy,
		oak_core::colormath::OutputColorSpec::default(),
	);
}

/// V1 (a single track carrying `clips`, in order) plus an optional
/// transition block. `transition` is `(seam, in_offset, out_offset)`: the
/// block is created with `[seam - in_offset, seam + out_offset]` and
/// inserted on the track between the first two clips, its two inputs
/// wired to them (`out_block_in` from the first clip, `in_block_in` from
/// the second). Returns the transition block's id so a test can flip its
/// style combo.
fn build_project(
	clips: &[(&str, Rational, Rational)],
	transition: Option<(Rational, Rational, Rational)>,
) -> (Arc<Mutex<Project>>, NodeId, Option<NodeId>) {
	pin_legacy_working_space();
	let project = Project::new();
	let mut transition_id = None;
	let seq;
	{
		let mut p = project.lock().unwrap();
		let (score, sbehavior) = SequenceBehavior::create();
		seq = p.graph.add_node(score, sbehavior);

		let (tcore, tbehavior) = TrackListBehavior::create();
		let tl = p.graph.add_node(tcore, tbehavior);

		let (tcore, tbehavior) = TrackBehavior::create();
		let v1 = p.graph.add_node(tcore, tbehavior);

		let mut clip_ids: Vec<NodeId> = Vec::new();
		for &(path, in_, out) in clips {
			let mut footage = FootageBehavior::new(path);
			footage.probe().expect("probe the generated clip");
			let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

			let (ccore, cbehavior) = oak_node::block::clip_create();
			let clip = p.graph.add_node(ccore, cbehavior);
			p.graph
				.connect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
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

			clip_ids.push(clip);
		}

		if let Some((seam, in_offset, out_offset)) = transition {
			let (tcore, tbehavior) = oak_node::block::transition_create();
			let block = p.graph.add_node(tcore, tbehavior);
			p.graph
				.get_mut(block)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TransitionBlockBehavior>()
				.expect("transition block")
				.core
				.range = TimeRange::new(seam - in_offset, seam + out_offset);
			{
				let behavior = p
					.graph
					.get_mut(block)
					.unwrap()
					.behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<TransitionBlockBehavior>()
					.expect("transition block");
				behavior.in_offset = in_offset;
				behavior.out_offset = out_offset;
			}
			p.graph
				.connect(
					clip_ids[0],
					block,
					oak_node::block::transition_input::OUT_BLOCK,
					-1,
				)
				.expect("connect the outgoing clip to the transition");
			p.graph
				.connect(
					clip_ids[1],
					block,
					oak_node::block::transition_input::IN_BLOCK,
					-1,
				)
				.expect("connect the incoming clip to the transition");
			transition_id = Some(block);

			// Track order: the outgoing clip, the transition, the incoming
			// clip — the order `transition_commands` builds.
			let track = p
				.graph
				.get_mut(v1)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.expect("video track");
			track.append_block(clip_ids[0]);
			track.append_block(block);
			track.append_block(clip_ids[1]);
		} else {
			let track = p
				.graph
				.get_mut(v1)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackBehavior>()
				.expect("video track");
			for &clip in &clip_ids {
				track.append_block(clip);
			}
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
	(project, seq, transition_id)
}

/// Flip the transition block's style combo (index into
/// `oak_node::nodes::transitions::TYPE_NAMES`).
fn set_style(project: &Arc<Mutex<Project>>, block: NodeId, style: i64) {
	project
		.lock()
		.unwrap()
		.graph
		.get_mut(block)
		.unwrap()
		.core
		.set_standard_value(
			oak_node::block::transition_input::TYPE_INPUT,
			-1,
			oak_node::value::NodeValue::Combo(style),
		);
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

/// The mean of channel `c` over `xs` x `ys` (8..56 both ways: away from
/// the encoder's frame borders).
fn channel_mean(data: &[u8], c: usize, xs: &[usize], ys: &[usize]) -> f32 {
	let mut sum = 0.0;
	let mut n = 0;
	for &y in ys {
		for &x in xs {
			sum += channel(data, x, y, c);
			n += 1;
		}
	}
	sum / n as f32
}

/// Two solid clips abut at t=1/2; a transition spanning [1/4, 3/4] mixes
/// them across the cut. At the seam (progress 0.5) a cross dissolve must
/// show the average of the two sides, while outside the span the render
/// is byte-identical to the same project without the transition.
///
/// Skipped (with a note) when no GPU adapter exists.
#[test]
fn cross_dissolve_blends_the_two_sides_of_a_cut() {
	if oak_core::backend::GpuContext::shared().is_none() {
		eprintln!("skipping cross_dissolve_blends_the_two_sides_of_a_cut: no GPU adapter");
		return;
	}
	let red = clip_path("dissolve_red");
	let blue = clip_path("dissolve_blue");
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
	let dissolved = build_project(
		&clips,
		Some((Rational::new(1, 2), Rational::new(1, 4), Rational::new(1, 4))),
	);
	let transition = dissolved.2.expect("the transition block");

	// The two sides, rendered by the same project without the transition.
	let plain_red = render_frame(&plain.0, plain.1, Rational::new(1, 4));
	let plain_blue = render_frame(&plain.0, plain.1, Rational::new(5, 8));
	assert!(
		channel_mean(&plain_red, 0, &(8..56).collect::<Vec<_>>(), &(8..56).collect::<Vec<_>>()) > 0.5,
		"the outgoing reference must be the red clip"
	);
	assert!(
		channel_mean(&plain_blue, 0, &(8..56).collect::<Vec<_>>(), &(8..56).collect::<Vec<_>>()) < 0.5,
		"the incoming reference must be the blue clip"
	);

	// At the exact seam the block is halfway through its span, so a cross
	// dissolve shows the average of the two sides. Sample away from the
	// frame borders (MPEG-2 chroma bleed lives at the edges).
	let seam = render_frame(&dissolved.0, dissolved.1, Rational::new(1, 2));
	let xs: Vec<usize> = (8..56).collect();
	let ys: Vec<usize> = (8..56).collect();
	for c in 0..3 {
		let expected = 0.5 * (channel_mean(&plain_red, c, &xs, &ys)
			+ channel_mean(&plain_blue, c, &xs, &ys));
		let got = channel_mean(&seam, c, &xs, &ys);
		assert!(
			(got - expected).abs() < 0.02,
			"channel {c} at the seam must be the average of the two sides: expected {expected}, got {got}"
		);
	}

	// Outside the block's span the render is byte-identical to the plain
	// project: the transition is inert there.
	for time in [Rational::new(1, 8), Rational::new(7, 8)] {
		assert_eq!(
			render_frame(&dissolved.0, dissolved.1, time),
			render_frame(&plain.0, plain.1, time),
			"t={time:?} is outside the transition's span and must render unchanged"
		);
	}

	// The blend is the block's own output: the plain clip read is replaced,
	// not added under it. Drop the block's style back to a fade and the
	// seam goes through black (the fade's midpoint), proving the style
	// rides on this block.
	set_style(&dissolved.0, transition, 1);
	let fade = render_frame(&dissolved.0, dissolved.1, Rational::new(1, 2));
	assert!(
		channel_mean(&fade, 0, &xs, &ys) < 0.1 && channel_mean(&fade, 2, &xs, &ys) < 0.1,
		"a fade at progress 0.5 is fully in the fade color (black), got r={} b={}",
		channel_mean(&fade, 0, &xs, &ys),
		channel_mean(&fade, 2, &xs, &ys)
	);

	let _ = std::fs::remove_file(&red);
	let _ = std::fs::remove_file(&blue);
}

/// The block's style combo selects the shader: a wipe at progress 0.5
/// puts the incoming clip on the left of the sweeping boundary and the
/// outgoing one on the right (the outgoing image leads the sweep).
#[test]
fn wipe_style_splits_the_frame_at_the_boundary() {
	if oak_core::backend::GpuContext::shared().is_none() {
		eprintln!("skipping wipe_style_splits_the_frame_at_the_boundary: no GPU adapter");
		return;
	}
	let red = clip_path("wipe_red");
	let blue = clip_path("wipe_blue");
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
	let project = build_project(
		&clips,
		Some((Rational::new(1, 2), Rational::new(1, 4), Rational::new(1, 4))),
	);
	set_style(&project.0, project.2.expect("the transition block"), 2);
	let wipe = render_frame(&project.0, project.1, Rational::new(1, 2));

	// Left of the boundary (x=32 at progress 0.5) trails the sweep and
	// shows the incoming (blue) clip; right of it the outgoing (red) one.
	// Keep 4 px clear of the boundary's soft edge (`soft = 0.02` of the
	// width). Green is 0.1 in both clips, so only the red and blue
	// channels tell the two sides apart.
	let left = (8..28).collect::<Vec<_>>();
	let right = (36..56).collect::<Vec<_>>();
	let ys: Vec<usize> = (8..56).collect();
	let (l_red, r_red) = (
		channel_mean(&wipe, 0, &left, &ys),
		channel_mean(&wipe, 0, &right, &ys),
	);
	let (l_blue, r_blue) = (
		channel_mean(&wipe, 2, &left, &ys),
		channel_mean(&wipe, 2, &right, &ys),
	);
	assert!(
		l_red < 0.5 && l_blue > 0.5,
		"left of the boundary must be the incoming clip (blue): r={l_red} b={l_blue}"
	);
	assert!(
		r_red > 0.5 && r_blue < 0.5,
		"right of the boundary must be the outgoing clip (red): r={r_red} b={r_blue}"
	);

	let _ = std::fs::remove_file(&red);
	let _ = std::fs::remove_file(&blue);
}

/// Single-sided transitions (PR-style edge transitions): a head
/// transition wired only `in_block_in` fades the clip in from black, a
/// tail transition wired only `out_block_in` fades it out to black — the
/// same shader with the open side generated transparent.
#[test]
fn single_sided_transitions_fade_from_and_to_black() {
    if oak_core::backend::GpuContext::shared().is_none() {
        eprintln!("skipping single_sided_transitions_fade_from_and_to_black: no GPU adapter");
        return;
    }
    let red = clip_path("edge_red");
    oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [0.9, 0.1, 0.1, 1.0])
        .expect("red clip generation");
    let red_path = red.to_string_lossy().to_string();
    let xs: Vec<usize> = (8..56).collect();
    let ys: Vec<usize> = (8..56).collect();

    let build = |start_edge: bool| {
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

            let mut footage = FootageBehavior::new(&red_path);
            footage.probe().expect("probe the generated clip");
            let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));
            let (ccore, cbehavior) = oak_node::block::clip_create();
            let clip = p.graph.add_node(ccore, cbehavior);
            p.graph
                .connect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
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
                .range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 1));

            let (tcore, tbehavior) = oak_node::block::transition_create();
            let block = p.graph.add_node(tcore, tbehavior);
            {
                let t = p
                    .graph
                    .get_mut(block)
                    .unwrap()
                    .behavior
                    .as_any_mut()
                    .unwrap()
                    .downcast_mut::<TransitionBlockBehavior>()
                    .expect("transition block");
                if start_edge {
                    t.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(1, 2));
                    t.in_offset = Rational::new(0, 1);
                    t.out_offset = Rational::new(1, 2);
                } else {
                    t.core.range = TimeRange::new(Rational::new(1, 2), Rational::new(1, 1));
                    t.in_offset = Rational::new(1, 2);
                    t.out_offset = Rational::new(0, 1);
                }
            }
            p.graph
                .connect(
                    clip,
                    block,
                    if start_edge {
                        oak_node::block::transition_input::IN_BLOCK
                    } else {
                        oak_node::block::transition_input::OUT_BLOCK
                    },
                    -1,
                )
                .expect("wire the clip to the transition");
            {
                let track = p
                    .graph
                    .get_mut(v1)
                    .unwrap()
                    .behavior
                    .as_any_mut()
                    .unwrap()
                    .downcast_mut::<TrackBehavior>()
                    .expect("video track");
                if start_edge {
                    track.append_block(block);
                    track.append_block(clip);
                } else {
                    track.append_block(clip);
                    track.append_block(block);
                }
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
    };

    // Head (fade-in): progress 0 = black, 0.5 = the half blend, 1 = the
    // clip (and the plain clip beyond the span).
    let (project, seq) = build(true);
    let full = render_frame(&project, seq, Rational::new(3, 4));
    assert!(
        channel_mean(&full, 0, &xs, &ys) > 0.5,
        "past the span the plain clip shows"
    );
    let start = render_frame(&project, seq, Rational::new(0, 1));
    assert!(
        channel_mean(&start, 0, &xs, &ys) < 0.02,
        "the fade-in starts at black, got {}",
        channel_mean(&start, 0, &xs, &ys)
    );
    let mid = render_frame(&project, seq, Rational::new(1, 4));
    // The pipeline's composite convention (the same one the opacity
    // stack carries): the half blend carries half alpha, and the
    // alpha-over composite applies that alpha once more, so the
    // midpoint reads a quarter of the clip's channels.
    let expected = 0.25 * channel_mean(&full, 0, &xs, &ys);
    let got = channel_mean(&mid, 0, &xs, &ys);
    assert!(
        (got - expected).abs() < 0.02,
        "fade-in midpoint must be the half blend composited: expected {expected}, got {got}"
    );

    // Tail (fade-out): progress 0 = the clip, 0.5 = the half blend, 1 =
    // black.
    let (project, seq) = build(false);
    let full = render_frame(&project, seq, Rational::new(1, 4));
    assert!(
        channel_mean(&full, 0, &xs, &ys) > 0.5,
        "before the span the plain clip shows"
    );
    let mid = render_frame(&project, seq, Rational::new(3, 4));
    // Same composite convention as the fade-in: half blend, alpha
    // applied again, a quarter of the clip at the midpoint.
    let expected = 0.25 * channel_mean(&full, 0, &xs, &ys);
    let got = channel_mean(&mid, 0, &xs, &ys);
    assert!(
        (got - expected).abs() < 0.02,
        "fade-out midpoint must be the half blend composited: expected {expected}, got {got}"
    );
    let end = render_frame(&project, seq, Rational::new(1, 1));
    assert!(
        channel_mean(&end, 0, &xs, &ys) < 0.02,
        "the fade-out ends at black, got {}",
        channel_mean(&end, 0, &xs, &ys)
    );

    let _ = std::fs::remove_file(&red);
}
