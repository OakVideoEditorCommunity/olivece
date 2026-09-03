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

//! Program-generated test media (M12 P0).
//!
//! Encodes a small H.264 clip of known content through the real FFmpeg
//! encoder — no network, no committed binary assets. Used by the
//! oakrender decode tests and the app's render e2e to prove that the
//! footage-decode path produces real pixels with known values.
//!
//! Content contract (what a decode must observe):
//! - frame 0: left half solid `[r, g, b]`, right half solid
//!   `[b, r, g]` (transposed), alpha 1 everywhere;
//! - later frames: the color columns sweep right by one column per
//!   frame, so per-frame content is distinguishable after decode.
//!
//! MPEG-2 is used instead of H.264: the H.264 default profile emits
//! B-frames, and this FFmpeg pairing's movenc writes B-frame streams
//! with mangled packet timestamps (decode seeks then fail). MPEG-2's
//! default encoder is B-frame-free and its streams round-trip cleanly.
//!
//! H.264/H.265 is lossy, so assertions downstream should use generous
//! tolerances (channel dominance rather than exact values).

use std::path::Path;

use oak_core::ocioutils::PixelFormat as OakPixelFormat;
use oak_core::videoparams::VideoParams;
use oak_core::{PixelFormat, Rational, SampleFormat};

use crate::encodingparams::EncodingParams;
use crate::encoder::create_from_params;
use crate::frame::Frame;
use crate::error::{Error, Result};

/// Encode `frame_count` frames of the known pattern into `out` (an MP4 at
/// `fps` frames per second, `width`x`height`, H.264).
pub fn write_test_clip(
	out: &Path,
	width: i32,
	height: i32,
	frame_count: i32,
	fps: i32,
) -> Result<()> {
	if width <= 0 || height <= 0 || frame_count <= 0 || fps <= 0 {
		return Err(Error::Invalid);
	}
	let mut params = video_params(out, width, height, fps)?;
	// A stereo PCM audio track with a known 440 Hz sine (M12 P1: the
	// audio-render path needs a decodable audio stream; PCM avoids
	// codec sample-format negotiation issues in this FFmpeg pairing).
	params.audio_enabled = 1;
	params.audio_codec = 13; // PCM S16LE
	params.audio_sample_rate = 48000;
	params.audio_channel_layout = 0x3; // stereo
	params.audio_sample_format = SampleFormat::F32;

	let encoder = create_from_params(&params)
		.ok_or_else(|| Error::Failed("no encoder for test clip params".into()))?;
	encoder.configure(&params)?;
	encoder.open()?;
	for i in 0..frame_count {
		encoder.write_video(&pattern_frame(i, width, height, fps))?;
	}
	// One second of 440 Hz sine (stereo, both channels identical).
	let rate = 48000u32;
	let mut tone: Vec<f32> = Vec::with_capacity(rate as usize * 2);
	for i in 0..rate {
		let v = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
		tone.push(v);
		tone.push(v);
	}
	encoder.write_audio(&tone, rate as i32)?;
	encoder.flush()
}

/// Encode `frame_count` frames of a solid `rgba` color into `out` (same
/// MPEG-2/MP4 pairing as [`write_test_clip`], video only). The stacking
/// tests need two OPAQUE clips of different known colors covering the
/// same time — the sweeping pattern cannot provide that (every clip's
/// frame 0 is identical, and decode-time seeking is not exercised here).
pub fn write_test_clip_solid(
	out: &Path,
	width: i32,
	height: i32,
	frame_count: i32,
	fps: i32,
	rgba: [f32; 4],
) -> Result<()> {
	if width <= 0 || height <= 0 || frame_count <= 0 || fps <= 0 {
		return Err(Error::Invalid);
	}
	let params = video_params(out, width, height, fps)?;
	let encoder = create_from_params(&params)
		.ok_or_else(|| Error::Failed("no encoder for test clip params".into()))?;
	encoder.configure(&params)?;
	encoder.open()?;
	for i in 0..frame_count {
		encoder.write_video(&solid_frame(i, width, height, fps, rgba))?;
	}
	encoder.flush()
}

/// The shared MPEG-2-video-in-MP4 encoder params of the test clips
/// (B-frame-free; the H.264 B-frame streams hit a muxer timing bug).
fn video_params(out: &Path, width: i32, height: i32, fps: i32) -> Result<EncodingParams> {
	let mut params = EncodingParams::default();
	let name = out.as_os_str().as_encoded_bytes();
	if name.len() >= params.filename.len() {
		return Err(Error::Failed("output path too long".into()));
	}
	params.filename[..name.len()].copy_from_slice(name);
	params.format = 2; // MPEG-4 video container
	params.video_enabled = 1;
	params.video_codec = 10; // MPEG-2 (B-frame-free; the H.264 B-frame streams hit a muxer timing bug)
	params.video_width = width;
	params.video_height = height;
	params.video_time_base_num = 1;
	params.video_time_base_den = fps;
	params.video_pixel_format = PixelFormat::F32;
	params.video_interlacing = 0;
	params.video_pixel_aspect_num = 1;
	params.video_pixel_aspect_den = 1;
	Ok(params)
}

/// One frame of solid `rgba` (F32 RGBA rows, like [`pattern_frame`]).
fn solid_frame(i: i32, width: i32, height: i32, fps: i32, rgba: [f32; 4]) -> Frame {
	let mut f = pattern_frame(0, width, height, fps);
	f.set_timestamp(Rational::new(i as i64, fps as i64));
	let linesize = f.linesize_bytes() as usize;
	let data = f.data_mut().expect("test frame buffer");
	let [r, g, b, a] = rgba;
	for y in 0..height as usize {
		for x in 0..width as usize {
			let off = y * linesize + x * 16;
			data[off..off + 4].copy_from_slice(&r.to_le_bytes());
			data[off + 4..off + 8].copy_from_slice(&g.to_le_bytes());
			data[off + 8..off + 12].copy_from_slice(&b.to_le_bytes());
			data[off + 12..off + 16].copy_from_slice(&a.to_le_bytes());
		}
	}
	f
}

/// One frame of the known pattern (see module doc).
fn pattern_frame(i: i32, width: i32, height: i32, fps: i32) -> Frame {
	let mut vp = VideoParams::new_basic(
		width,
		height,
		OakPixelFormat::from_code(0),
		4,
		1,
		1,
		0,
		1,
	);
	vp.set_format(OakPixelFormat::from_code(PixelFormat::F32 as i32));
	let mut f = Frame::with_params(vp);
	f.set_timestamp(Rational::new(i as i64, fps as i64));
	f.allocate().expect("test frame allocation");

	let linesize = f.linesize_bytes() as usize;
	let data = f.data_mut().expect("test frame buffer");
	let shift = (i * width / (2 * fps.max(1))) % width;
	for y in 0..height as usize {
		for x in 0..width as usize {
			let off = y * linesize + x * 16;
			let (r, g, b) = if (x as i32 + shift) % width < width / 2 {
				(0.9f32, 0.15f32, 0.05f32)
			} else {
				(0.05f32, 0.2f32, 0.85f32)
			};
			data[off..off + 4].copy_from_slice(&r.to_le_bytes());
			data[off + 4..off + 8].copy_from_slice(&g.to_le_bytes());
			data[off + 8..off + 12].copy_from_slice(&b.to_le_bytes());
			data[off + 12..off + 16].copy_from_slice(&1.0f32.to_le_bytes());
		}
	}
	f
}
