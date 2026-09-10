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

//! End-to-end export: a sequence carrying one clip renders through
//! [`oak_task::export::ExportTask`] into a real H.264/AAC MP4 — the file
//! exists, probes to the sequence geometry, and decodes back to the
//! clip's content. This is the "export is a fake" report's regression
//! guard: the whole chain (graph montage -> render tickets -> encoder)
//! must produce a playable file.

use std::sync::{Arc, Mutex};

use oak_core::{PixelFormat, Rational, TimeRange};
use oak_node::block::ClipBlockBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior};
use oak_render::manager::{RenderBackendChoice, RenderManager};

/// Unique temp path per test (the process id disambiguates parallel test
/// binaries; the tag separates files inside one binary).
fn clip_path(tag: &str) -> std::path::PathBuf {
	std::env::temp_dir().join(format!("oaktask_export_{tag}_{}.mp4", std::process::id()))
}

/// One sequence + one video track list + one track + one clip of `media`
/// covering `[0, 1s)`.
fn build_project(media: &str) -> (Arc<Mutex<Project>>, NodeId) {
	// The export converts through the pipeline working space on the way
	// out; pin the legacy sRGB pass-through so the decoded-back pixels
	// keep their display-referred values for the assertions below.
	oak_core::color::set_pipeline_color_settings(
		oak_core::colormath::WorkingColorSpace::SrgbLegacy,
		oak_core::colormath::OutputColorSpec::default(),
	);
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

		let mut footage = FootageBehavior::new(media);
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

/// The full export chain: a 64x64 one-second sequence exports to MP4;
/// the result probes to 64x64 and decodes back to the clip's solid green.
#[test]
fn export_writes_a_real_playable_mp4() {
	let media = clip_path("src");
	oak_codec::testmedia::write_test_clip_solid(&media, 64, 64, 10, 10, [0.0, 1.0, 0.0, 1.0])
		.expect("green clip generation");
	let (project, seq) = build_project(&media.to_string_lossy());
	let out = clip_path("out");
	let _ = std::fs::remove_file(&out);

	// The export's render loop runs on the process-wide manager arena;
	// the inline backend keeps the test in-process.
	if RenderManager::global().is_none() {
		RenderManager::init_with_backend(RenderBackendChoice::Threads)
			.expect("render manager init");
	}

	let encoding = oak_task::export::EncodingParams {
		filename: out.to_string_lossy().into_owned(),
		format: oak_codec::exportformat::Format::MPEG4Video as i32,
		video_enabled: true,
		video_codec: oak_codec::exportcodec::Codec::H264 as i32,
		video_width: 64,
		video_height: 64,
		video_time_base_num: 1,
		video_time_base_den: 30,
		video_pixel_format: 0,
		audio_enabled: true,
		audio_codec: oak_codec::exportcodec::Codec::AAC as i32,
		audio_sample_rate: 48000,
		audio_channel_layout: 0x3,
		subtitles_enabled: false,
		export_length_num: 1,
		export_length_den: 1,
		has_custom_range: false,
		custom_range_in_num: 0,
		custom_range_in_den: 1,
		custom_range_out_num: 0,
		custom_range_out_den: 1,
		video_bit_rate: 0,
		audio_bit_rate: 0,
		color_override_enabled: false,
		color_primaries: 0,
		color_trc: 0,
		color_space: 0,
	};
	let inner = oak_task::export::ExportTask::new((project.clone(), seq), encoding);
	let mut driver = oak_task::task::Task::new("Exporting...", None);
	driver.set_behavior(Box::new(inner));
	if let Err(e) = driver.start() {
		panic!(
			"export failed: {e:?} / {}",
			driver.error().unwrap_or("unknown error")
		);
	}

	// The file exists and is a real container (video + audio for a second
	// is far past a header-only file).
	let bytes = std::fs::metadata(&out)
		.expect("the export wrote the file")
		.len();
	assert!(
		bytes > 1024,
		"a playable mp4 with video+audio is more than 1K, got {bytes}"
	);

	// It probes to the sequence geometry.
	let mut probe = FootageBehavior::new(&out.to_string_lossy());
	probe.probe().expect("the export probes as media");
	let params = probe
		.video_params(0)
		.expect("the export has a video stream");
	assert_eq!(
		(params.width, params.height),
		(64, 64),
		"the export's video stream is the sequence geometry"
	);

	// And a decoded frame is the clip's solid green (H.264 YCbCr keeps
	// the hue; loose thresholds).
	let tex = oak_render::eval::render_footage_frame(
		&out.to_string_lossy(),
		0,
		Rational::new(0, 1),
		(64, 64),
		PixelFormat::F32,
	)
	.expect("decode the exported frame");
	let oak_core::texture::Texture::Cpu(frame) = &tex else {
		panic!("decode produced a GPU texture");
	};
	let off = 32 * frame.linesize_bytes() as usize + 32 * 16;
	let r = f32::from_le_bytes(frame.data[off..off + 4].try_into().unwrap());
	let g = f32::from_le_bytes(frame.data[off + 4..off + 8].try_into().unwrap());
	assert!(
		g > 0.4 && r < 0.3,
		"the exported frame is the clip's green (r={r} g={g})"
	);

	let _ = std::fs::remove_file(&media);
	let _ = std::fs::remove_file(&out);
}
