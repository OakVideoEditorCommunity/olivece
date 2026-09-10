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

//! M12 phase 2: the graph-driven sequence renderer.
//!
//! Builds a real node graph (sequence -> video track list -> video tracks
//! -> clip blocks -> footage), evaluates the clip overlapping the request
//! time through the traverser and composites the decoded frames — the same
//! path the engine's viewer uses. The LAST track is the topmost stack
//! element (NLE stacking: the highest-numbered track wins).

use std::sync::{Arc, Mutex};

use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::block::ClipBlockBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior};

use oak_core::texture::Texture;

mod common;

/// Unique temp path per test (the process id disambiguates parallel test
/// binaries; the tag separates tests inside one binary).
fn clip_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("oakrender_graph_{tag}_{}.mp4", std::process::id()))
}

/// These tests verify graph/decode/composite MECHANICS (stacking, scaling,
/// effects), not the color pipeline. Pin the working space to the legacy
/// sRGB pass-through so the decoded pixels stay display-referred and the
/// pixel-value assertions hold regardless of the ACEScg default. All tests
/// in this binary set the same value, so the shared global is race-free.
fn pin_legacy_working_space() {
    oak_core::color::set_pipeline_color_settings(
        oak_core::colormath::WorkingColorSpace::SrgbLegacy,
        oak_core::colormath::OutputColorSpec::default(),
    );
}

/// One sequence + one video track list with one track per clip
/// `(filename, [in, out))`. The LAST entry's track composites on top
/// (NLE stacking: the highest-numbered track is topmost).
fn build_project(clips: &[(&str, Rational, Rational)]) -> (Arc<Mutex<Project>>, NodeId) {
    pin_legacy_working_space();
    let project = Project::new();
    let seq;
    {
        let mut p = project.lock().unwrap();
        let (score, sbehavior) = SequenceBehavior::create();
        seq = p.graph.add_node(score, sbehavior);

        let (tcore, tbehavior) = TrackListBehavior::create();
        let tl = p.graph.add_node(tcore, tbehavior);

        for &(path, in_, out) in clips {
            let (tcore, tbehavior) = TrackBehavior::create();
            let track = p.graph.add_node(tcore, tbehavior);

            let mut footage = FootageBehavior::new(path);
            footage.probe().expect("probe the generated clip");
            let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

            let (ccore, cbehavior) = oak_node::block::clip_create();
            let clip = p.graph.add_node(ccore, cbehavior);
            p.graph
                .connect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
                .expect("connect footage to clip");

            let clip_behavior = p
                .graph
                .get_mut(clip)
                .unwrap()
                .behavior
                .as_any_mut()
                .unwrap()
                .downcast_mut::<ClipBlockBehavior>()
                .expect("clip block");
            clip_behavior.core.range = TimeRange::new(in_, out);

            p.graph
                .get_mut(track)
                .unwrap()
                .behavior
                .as_any_mut()
                .unwrap()
                .downcast_mut::<TrackBehavior>()
                .expect("video track")
                .append_block(clip);
            p.graph
                .get_mut(tl)
                .unwrap()
                .behavior
                .as_any_mut()
                .unwrap()
                .downcast_mut::<TrackListBehavior>()
                .expect("video track list")
                .tracks
                .push(track);
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

/// Two clips on two tracks, non-overlapping in time: at each request time
/// exactly one clip covers, and its output must match the single-track
/// render byte for byte (same decode + same composite path).
#[test]
fn graph_sequence_renders_two_tracks() {
    let path_a = clip_path("two_tracks_a");
    let path_b = clip_path("two_tracks_b");
    oak_codec::testmedia::write_test_clip(&path_a, 64, 64, 10, 10).expect("clip A generation");
    oak_codec::testmedia::write_test_clip(&path_b, 32, 32, 10, 10).expect("clip B generation");

    let (project, seq) = build_project(&[
        (&path_a.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
        (&path_b.to_string_lossy(), Rational::new(1, 1), Rational::new(2, 1)),
    ]);

    let t05 = oak_render::eval::render_graph_frame(
        &project,
        seq,
        Rational::new(1, 2),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("render t=0.5");
    let t15 = oak_render::eval::render_graph_frame(
        &project,
        seq,
        Rational::new(3, 2),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("render t=1.5");
    assert_eq!(t05.size(), (64, 64));
    assert_eq!(t15.size(), (64, 64));

    // Solo renders of each clip for byte comparison.
    let (solo_a, seq_a) = build_project(&[(&path_a.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1))]);
    let solo_a_tex = oak_render::eval::render_graph_frame(
        &solo_a,
        seq_a,
        Rational::new(1, 2),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("solo A render");
    let (solo_b, seq_b) = build_project(&[(&path_b.to_string_lossy(), Rational::new(1, 1), Rational::new(2, 1))]);
    let solo_b_tex = oak_render::eval::render_graph_frame(
        &solo_b,
        seq_b,
        Rational::new(3, 2),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("solo B render");

    // Each time picks exactly the clip covering it, unchanged by the other
    // track (B is 32x32 and must scale up to the 64x64 target).
    assert_eq!(frame_data(&t05), frame_data(&solo_a_tex), "t=0.5 renders clip A");
    assert_eq!(frame_data(&t15), frame_data(&solo_b_tex), "t=1.5 renders clip B");
    assert_ne!(frame_data(&t05), frame_data(&t15), "the two clips differ");

    // Both frames carry real content.
    assert!(frame_data(&t05).iter().any(|&b| b != 0), "t=0.5 is not black");
    assert!(frame_data(&t15).iter().any(|&b| b != 0), "t=1.5 is not black");

    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

/// NLE stacking regression: two OPAQUE solid-color clips covering the
/// same time on two video tracks — the clip on the LAST track (V2, blue)
/// composites on top of the clip on the first track (V1, red), matching
/// the timeline UI (the highest-numbered track displays on top).
#[test]
fn graph_sequence_stacks_highest_track_on_top() {
    let red = clip_path("stack_red");
    let blue = clip_path("stack_blue");
    oak_codec::testmedia::write_test_clip_solid(&red, 64, 64, 10, 10, [0.9, 0.1, 0.1, 1.0])
        .expect("red clip generation");
    oak_codec::testmedia::write_test_clip_solid(&blue, 64, 64, 10, 10, [0.1, 0.1, 0.9, 1.0])
        .expect("blue clip generation");

    // V1 = red (bottom), V2 = blue (top).
    let (project, seq) = build_project(&[
        (&red.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
        (&blue.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
    ]);
    let tex = oak_render::eval::render_graph_frame(&project, seq, Rational::new(0, 1), (64, 64), PixelFormat::F32)
        .expect("stacked render");
    let data = frame_data(&tex);
    assert!(
        channel(data, 8, 8, 2) > 0.5 && channel(data, 8, 8, 0) < 0.4,
        "V2's blue covers V1's red (r={}, b={})",
        channel(data, 8, 8, 0),
        channel(data, 8, 8, 2)
    );

    // Distinguishability guard: solo, the V1 clip really is red (the two
    // tracks carry different content).
    let (solo, solo_seq) = build_project(&[(&red.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1))]);
    let solo_tex = oak_render::eval::render_graph_frame(&solo, solo_seq, Rational::new(0, 1), (64, 64), PixelFormat::F32)
        .expect("solo V1 render");
    let solo_data = frame_data(&solo_tex);
    assert!(
        channel(solo_data, 8, 8, 0) > 0.5 && channel(solo_data, 8, 8, 2) < 0.4,
        "solo V1 is red (r={}, b={})",
        channel(solo_data, 8, 8, 0),
        channel(solo_data, 8, 8, 2)
    );

    let _ = std::fs::remove_file(&red);
    let _ = std::fs::remove_file(&blue);
}

/// The driver rejects bad arguments explainably: non-F32 format, a
/// non-positive size, and a missing viewer.
#[test]
fn graph_render_rejects_bad_inputs() {
    let (project, seq) = build_project(&[]);

    let err = oak_render::eval::render_graph_frame(&project, seq, Rational::new(0, 1), (64, 64), PixelFormat::U8)
        .err()
        .expect("non-F32 format rejected");
    assert_eq!(err.code(), oak_render::error::Error::Invalid.code());

    let err = oak_render::eval::render_graph_frame(&project, seq, Rational::new(0, 1), (0, 64), PixelFormat::F32)
        .err()
        .expect("non-positive size rejected");
    assert_eq!(err.code(), oak_render::error::Error::Invalid.code());

    let err = oak_render::eval::render_graph_frame(&project, NodeId::INVALID, Rational::new(0, 1), (64, 64), PixelFormat::F32)
        .err()
        .expect("missing viewer rejected");
    assert_eq!(err.code(), oak_render::error::Error::NotFound.code());
}

/// One sequence + one track with a single clip, with an effect node
/// inserted between the footage and the clip block: `insert_effect`
/// receives the project lock plus the footage and clip node ids, rewires
/// the graph, and returns the effect node id. The clip keeps the
/// `(in, out)` range from `clip`.
fn build_effect_project(
    clip: (&str, Rational, Rational),
    insert_effect: impl FnOnce(&mut Project, NodeId, NodeId) -> NodeId,
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
        let track = p.graph.add_node(tcore, tbehavior);

        let mut footage = FootageBehavior::new(clip.0);
        footage.probe().expect("probe the generated clip");
        let footage = p.graph.add_node(NodeCore::new(), Box::new(footage));

        let (ccore, cbehavior) = oak_node::block::clip_create();
        let clip_node = p.graph.add_node(ccore, cbehavior);
        p.graph
            .connect(footage, clip_node, oak_node::block::clip_input::TEXTURE_INPUT, -1)
            .expect("connect footage to clip");

        let clip_behavior = p
            .graph
            .get_mut(clip_node)
            .unwrap()
            .behavior
            .as_any_mut()
            .unwrap()
            .downcast_mut::<ClipBlockBehavior>()
            .expect("clip block");
        clip_behavior.core.range = TimeRange::new(clip.1, clip.2);

        let _effect = insert_effect(&mut p, footage, clip_node);

        p.graph
            .get_mut(track)
            .unwrap()
            .behavior
            .as_any_mut()
            .unwrap()
            .downcast_mut::<TrackBehavior>()
            .expect("video track")
            .append_block(clip_node);
        p.graph
            .get_mut(tl)
            .unwrap()
            .behavior
            .as_any_mut()
            .unwrap()
            .downcast_mut::<TrackListBehavior>()
            .expect("video track list")
            .tracks
            .push(track);
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

/// The F32 RGBA channel of a 64x64 frame at `(x, y)`.
fn channel(data: &[u8], x: usize, y: usize, c: usize) -> f32 {
    let off = (y * 64 + x) * 16 + c * 4;
    f32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

/// M12 phase 3a: an opacity shader job (scalar 0.5) pushed by the effect
/// node is resolved on the shared GPU context and composited — each color
/// channel ends up as the plain render halved twice (the shader scales the
/// straight-alpha vec4 by 0.5, then the alpha-over composite applies the
/// halved alpha again), i.e. a 0.25 channel ratio. Skipped (with a note)
/// when no GPU adapter exists.
#[test]
fn shader_job_opacity_halves_pixels() {
    if oak_core::backend::GpuContext::shared().is_none() {
        eprintln!("skipping shader_job_opacity_halves_pixels: no GPU adapter");
        return;
    }
    let path = clip_path("opacity_job");
    oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("clip generation");

    let (plain_project, plain_seq) = build_project(&[(
        &path.to_string_lossy(),
        Rational::new(0, 1),
        Rational::new(1, 1),
    )]);
    let plain_tex = oak_render::eval::render_graph_frame(
        &plain_project,
        plain_seq,
        Rational::new(0, 1),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("plain render");
    let plain = frame_data(&plain_tex).to_vec();

    let (effect_project, effect_seq) = build_effect_project(
        (&path.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
        |p, footage, clip| {
            let (ecore, ebehavior) = oak_node::nodes::opacity::create();
            let effect = p.graph.add_node(ecore, ebehavior);
            p.graph.disconnect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1);
            p.graph
                .connect(footage, effect, oak_node::nodes::opacity::TEXTURE_INPUT, -1)
                .expect("connect footage to effect");
            p.graph
                .connect(effect, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
                .expect("connect effect to clip");
            p.graph
                .get_mut(effect)
                .unwrap()
                .core
                .set_standard_value(
                    oak_node::nodes::opacity::VALUE_INPUT,
                    -1,
                    oak_node::value::NodeValue::Float(0.5),
                );
            effect
        },
    );
    let effect_tex = oak_render::eval::render_graph_frame(
        &effect_project,
        effect_seq,
        Rational::new(0, 1),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("opacity render");
    let blurred = frame_data(&effect_tex).to_vec();

    // Sample away from the x=32 half boundary (MPEG-2 chroma bleed and
    // luma ringing stay within a few pixels of it).
    let mut ratios: Vec<f32> = Vec::new();
    for y in 4..60 {
        for x in (4..24).chain(40..60) {
            for c in 0..3 {
                let a = channel(&plain, x, y, c);
                if a > 0.02 {
                    ratios.push(channel(&blurred, x, y, c) / a);
                }
            }
        }
    }
    assert!(ratios.len() >= 512, "too few comparable samples: {}", ratios.len());
    let mean = ratios.iter().sum::<f32>() / ratios.len() as f32;
    assert!(
        (mean - 0.25).abs() < 0.02,
        "opacity channel ratio {mean} is not 0.25"
    );

    let _ = std::fs::remove_file(&path);
}

/// M12 phase 3a: a box-blur shader job (radius 2, both axes) is resolved
/// on the shared GPU context — the output differs from the plain render
/// byte-wise, the hard left/right half boundary softens (the per-pixel
/// step at the boundary shrinks), and left-half content bleeds into the
/// boundary pixel on the right half. Skipped when no GPU adapter exists.
#[test]
fn shader_job_blur_smooths_edge() {
    if oak_core::backend::GpuContext::shared().is_none() {
        eprintln!("skipping shader_job_blur_smooths_edge: no GPU adapter");
        return;
    }
    let path = clip_path("blur_job");
    oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("clip generation");

    let (plain_project, plain_seq) = build_project(&[(
        &path.to_string_lossy(),
        Rational::new(0, 1),
        Rational::new(1, 1),
    )]);
    let plain_tex = oak_render::eval::render_graph_frame(
        &plain_project,
        plain_seq,
        Rational::new(0, 1),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("plain render");
    let plain = frame_data(&plain_tex).to_vec();

    let (effect_project, effect_seq) = build_effect_project(
        (&path.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
        |p, footage, clip| {
            let (ecore, ebehavior) = oak_node::nodes::blur::create();
            let effect = p.graph.add_node(ecore, ebehavior);
            p.graph.disconnect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1);
            p.graph
                .connect(footage, effect, oak_node::nodes::blur::TEXTURE_INPUT, -1)
                .expect("connect footage to effect");
            p.graph
                .connect(effect, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
                .expect("connect effect to clip");
            let core = &mut p.graph.get_mut(effect).unwrap().core;
            core.set_standard_value(
                oak_node::nodes::blur::METHOD_INPUT,
                -1,
                oak_node::value::NodeValue::Combo(0),
            );
            core.set_standard_value(
                oak_node::nodes::blur::RADIUS_INPUT,
                -1,
                oak_node::value::NodeValue::Float(2.0),
            );
            core.set_standard_value(
                oak_node::nodes::blur::HORIZ_INPUT,
                -1,
                oak_node::value::NodeValue::Boolean(true),
            );
            core.set_standard_value(
                oak_node::nodes::blur::VERT_INPUT,
                -1,
                oak_node::value::NodeValue::Boolean(true),
            );
            effect
        },
    );
    let effect_tex = oak_render::eval::render_graph_frame(
        &effect_project,
        effect_seq,
        Rational::new(0, 1),
        (64, 64),
        PixelFormat::F32,
    )
    .expect("blur render");
    let blurred = frame_data(&effect_tex).to_vec();

    // The blur must actually change pixels (a silently dropped job would
    // fall back to the pass-through input and byte-match the plain frame).
    assert_ne!(plain, blurred, "the blur job must actually change pixels");

    // Row y=32 (vertically uniform): the boundary step x=31 -> x=32 must
    // shrink, and the right-side boundary pixel picks up left-half content.
    let r = |data: &[u8], x: usize| channel(data, x, 32, 0);
    let plain_step = (r(&plain, 32) - r(&plain, 31)).abs();
    let blurred_step = (r(&blurred, 32) - r(&blurred, 31)).abs();
    assert!(
        blurred_step < plain_step,
        "boundary step {blurred_step} not below the plain {plain_step}"
    );
    assert!(
        r(&blurred, 32) > r(&plain, 32),
        "blurred boundary pixel {} not above the plain {}",
        r(&blurred, 32),
        r(&plain, 32)
    );

    let _ = std::fs::remove_file(&path);
}

/// Chroma Key on a green clip: the keying node's job payload flows through
/// the whole graph path (footage → chromakey → clip → traverser → GPU
/// shader job → composite), and the key color actually drives the mask — a
/// green key on a green frame makes the frame transparent, a red key keeps
/// it opaque. Regression test for the chromakey node's old value(): gated
/// on the OCIO processor (never populated without the render bridge), it
/// pushed an empty output table, the traverser handed the clip
/// `NodeValue::None`, and the rendered frame lost the clip entirely.
/// Skipped when no GPU adapter or OCIO config exists.
#[test]
fn chromakey_job_keys_green_with_ociobased_stub() {
    if oak_core::backend::GpuContext::shared().is_none() {
        eprintln!("skipping chromakey_job_keys_green_with_ociobased_stub: no GPU adapter");
        return;
    }
    if oak_core::color::set_up_default_config().is_err() {
        eprintln!("skipping chromakey_job_keys_green_with_ociobased_stub: no OCIO config");
        return;
    }

    let path = clip_path("chromakey_job");
    oak_codec::testmedia::write_test_clip_solid(&path, 64, 64, 10, 10, [0.0, 1.0, 0.0, 1.0])
        .expect("green clip generation");

    // The chromakey built from the factory (the same `create()` the
    // inspector's effect stack uses), defaulting to the green key color.
    let render = |key: [f64; 4]| {
        let (project, seq) = build_effect_project(
            (&path.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
            |p, footage, clip| {
                let (ecore, ebehavior) =
                    oak_node::factory::Factory::global()
                        .create_any("org.olivevideoeditor.Olive.chromakey")
                        .expect("chromakey factory entry");
                let effect = p.graph.add_node(ecore, ebehavior);
                p.graph
                    .disconnect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1);
                p.graph
                    .connect(footage, effect, "tex_in", -1)
                    .expect("connect footage to chromakey");
                p.graph
                    .connect(effect, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
                    .expect("connect chromakey to clip");
                p.graph
                    .get_mut(effect)
                    .unwrap()
                    .core
                    .set_standard_value("color_key", -1, oak_node::value::NodeValue::Color(key));
                effect
            },
        );
        let tex = oak_render::eval::render_graph_frame(
            &project,
            seq,
            Rational::new(0, 1),
            (64, 64),
            PixelFormat::F32,
        )
        .expect("chromakey render");
        frame_data(&tex).to_vec()
    };

    // Sum the RGB channels over a 48x48 center crop (MPEG-2 chroma bleed
    // stays near the edges); a keyed-out frame contributes nothing.
    let energy = |frame: &[u8]| -> f32 {
        let mut total = 0.0f32;
        for y in 8..56 {
            for x in 8..56 {
                let off = (y * 64 + x) * 16;
                for c in 0..3 {
                    total += f32::from_le_bytes(frame[off + c * 4..off + c * 4 + 4].try_into().unwrap());
                }
            }
        }
        total
    };

    // Green key on the green clip: fully keyed (transparent black).
    let green_keyed = render([0.0, 1.0, 0.0, 1.0]);
    assert!(
        energy(&green_keyed) < 1.0,
        "green frame against the green key must key out (energy {})",
        energy(&green_keyed)
    );

    // Red key on the green clip: far from the key color, mask ~1, the
    // frame stays opaque — the key color parametrizes the result.
    let red_keyed = render([1.0, 0.0, 0.0, 1.0]);
    assert!(
        energy(&red_keyed) > 10.0,
        "green frame against the red key must stay (energy {})",
        energy(&red_keyed)
    );

    let _ = std::fs::remove_file(&path);
}

/// Generator sizing is anchored to the SEQUENCE resolution (C++ inserts
/// the NodeGlobals square resolution as `resolution_in` at job-build
/// time), not the render target: a proxy-size playback render and a
/// full-res paused frame must draw the shape at the same relative size,
/// or the generated layer visibly changes size whenever the transport
/// stops. A 96px rectangle on the default 1920x1080 sequence spans 5%
/// of the width at BOTH 192x108 and 768x432 (the pre-fix
/// target-anchored behavior drew 50% vs 12.5%).
#[test]
fn shape_generator_size_is_sequence_relative() {
    if oak_core::backend::GpuContext::shared().is_none() {
        eprintln!("skipping shape_generator_size_is_sequence_relative: no GPU adapter");
        return;
    }
    let path = clip_path("shape_seqrel");
    oak_codec::testmedia::write_test_clip_solid(&path, 64, 64, 10, 10, [0.0, 0.0, 1.0, 1.0])
        .expect("blue clip generation");

    let (project, seq) = build_effect_project(
        (&path.to_string_lossy(), Rational::new(0, 1), Rational::new(1, 1)),
        |p, footage, clip| {
            let (ecore, ebehavior) = oak_node::factory::Factory::global()
                .create_any("org.olivevideoeditor.Olive.shape")
                .expect("shape factory entry");
            let effect = p.graph.add_node(ecore, ebehavior);
            p.graph.disconnect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1);
            // The shape module is crate-private; the input ids are the C++
            // kBaseInput/kSizeInput strings.
            p.graph
                .connect(footage, effect, "base_in", -1)
                .expect("connect footage to shape base");
            p.graph
                .connect(effect, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
                .expect("connect shape to clip");
            p.graph.get_mut(effect).unwrap().core.set_standard_value(
                "size_in",
                -1,
                oak_node::value::NodeValue::Vec2([96.0, 96.0]),
            );
            effect
        },
    );

    // The fraction of the middle row covered by the opaque red square.
    let red_fraction = |width: i32, height: i32| -> f32 {
        let tex = oak_render::eval::render_graph_frame(
            &project,
            seq,
            Rational::new(0, 1),
            (width, height),
            PixelFormat::F32,
        )
        .expect("shape render");
        let Texture::Cpu(frame) = &tex else {
            panic!("graph render produced a non-CPU texture");
        };
        let stride = frame.linesize_bytes() as usize;
        let y = (height / 2) as usize;
        let mut red = 0usize;
        for x in 0..width as usize {
            let off = y * stride + x * 16;
            let r = f32::from_le_bytes(frame.data[off..off + 4].try_into().unwrap());
            let g = f32::from_le_bytes(frame.data[off + 4..off + 8].try_into().unwrap());
            let a = f32::from_le_bytes(frame.data[off + 12..off + 16].try_into().unwrap());
            if r > 0.9 && g < 0.1 && a > 0.9 {
                red += 1;
            }
        }
        red as f32 / width as f32
    };

    let small = red_fraction(192, 108);
    let large = red_fraction(768, 432);
    assert!(
        (small - large).abs() < 0.02,
        "the shape's relative size must not depend on the render target: {small} at 192px vs {large} at 768px"
    );
    assert!(
        (small - 0.05).abs() < 0.02,
        "96px on the 1920px sequence is 5%%, got {small}"
    );

    let _ = std::fs::remove_file(&path);
}
