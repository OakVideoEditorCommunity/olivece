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

//! Real-media tests for the FFmpeg decoder/encoder (`#[cfg(test)]` module).
//!
//! These exercise the real `ffmpeg-next` implementation against
//! `tests/demo.mp4` at the repository root (H.264 1920x1080@25fps + AAC
//! 48kHz stereo) and a full H.264 encode round-trip through `/tmp`.
//!
//! They live inside the crate (not `tests/`) because the crate's
//! `#[cfg(test)]` in-memory stubs — which the `Frame`/`FootageDescription`
//! paths need — are only linked for the lib test binary (`tests/` is
//! compiled without `#[cfg(test)]` and cannot resolve those symbols; see
//! `tests/ffi_contract_test.rs`).

use oak_core::ocioutils::PixelFormat as OakPixelFormat;
use oak_core::videoparams::VideoParams;
use crate::decoder::{
	CodecStream, Decoder, RenderMode, RetrieveAudioStatus, RetrieveVideoParams,
	K_COLOR_RANGE_DEFAULT,
};
use crate::encoder::create_from_params;
use crate::ffmpeg::FFmpegDecoder;
use crate::frame::Frame;
use oak_core::{PixelFormat, Rational, TimeRange};
use std::sync::Arc;

/// `tests/demo.mp4` at the repository root.
fn demo_path() -> std::path::PathBuf {
	std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../oak-app/tests/demo.mp4")
}

fn video_params(stream: CodecStream, time: Rational) -> RetrieveVideoParams {
	RetrieveVideoParams {
		stream,
		time,
		length: TimeRange::default(),
		force_range: K_COLOR_RANGE_DEFAULT,
		is_image_sequence: false,
		image_sequence_digits: 0,
		image_sequence_number: 0,
		mode: RenderMode::Offline,
		alpha_is_premultiplied: false,
		target_size: None,
	}
}

/// H.264 encoder parameters: 64x64, 10 fps, `out` as the target file.
fn h264_params(out: &std::path::Path) -> crate::encodingparams::EncodingParams {
	let mut p = crate::encodingparams::EncodingParams::default();
	let name = out.as_os_str().as_encoded_bytes();
	p.filename[..name.len()].copy_from_slice(name);
	p.format = 2; // MPEG-4 video
	p.video_enabled = 1;
	p.video_codec = 1; // H.264
	p.video_width = 64;
	p.video_height = 64;
	p.video_time_base_num = 1;
	p.video_time_base_den = 10;
	p.video_pixel_format = PixelFormat::F32;
	p.video_interlacing = 0;
	p.video_pixel_aspect_num = 1;
	p.video_pixel_aspect_den = 1;
	p
}

/// Build an allocated F32-RGBA frame with a moving color pattern.
fn pattern_frame(i: i32) -> Frame {
	let mut vp = VideoParams::new_basic(64, 64, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
	vp.set_format(OakPixelFormat::from_code(PixelFormat::F32 as i32));
	let mut f = Frame::with_params(vp);
	f.set_timestamp(Rational::new(i as i64, 10));
	f.allocate().unwrap();

	let linesize = f.linesize_bytes() as usize;
	let data = f.data_mut().unwrap();
	for y in 0..64usize {
		for x in 0..64usize {
			let off = y * linesize + x * 16;
			let r: f32 = if x < 32 { 0.4 + i as f32 * 0.05 } else { 0.1 };
			let g: f32 = y as f32 / 64.0;
			let b: f32 = if x >= 32 { 0.7 } else { 0.2 };
			data[off..off + 4].copy_from_slice(&r.to_le_bytes());
			data[off + 4..off + 8].copy_from_slice(&g.to_le_bytes());
			data[off + 8..off + 12].copy_from_slice(&b.to_le_bytes());
			data[off + 12..off + 16].copy_from_slice(&1.0f32.to_le_bytes());
		}
	}
	f
}

#[test]
fn probe_reports_streams_and_duration() {
	let d = FFmpegDecoder::new();
	let desc = d
		.probe(demo_path().to_str().unwrap(), None)
		.expect("demo.mp4 should probe");
	assert_eq!(desc.decoder(), "ffmpeg");
	// video + audio + data (timecode) stream.
	assert_eq!(desc.total_stream_count(), 3);
	assert_eq!(desc.video_stream_count(), 1);
	assert_eq!(desc.audio_stream_count(), 1);

	// Video stream: 1920x1080, 25fps, 17s at 1/12800 time base.
	let vp = desc.get_video_stream(0).expect("video stream");
	assert_eq!(vp.width(), 1920);
	assert_eq!(vp.height(), 1080);
	assert_eq!(vp.duration(), 17 * 12800);
	assert_eq!(vp.frame_rate(), (25, 1));
}

#[test]
fn decode_first_video_frame_has_dimensions_and_content() {
	let d = FFmpegDecoder::new();
	let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 0, None);
	d.open(&s).expect("open video stream");

	let f = d
		.retrieve_video_frame(&video_params(s, Rational::new(0, 1)))
		.expect("decode first frame");
	assert_eq!(f.width(), 1920);
	assert_eq!(f.height(), 1080);
	assert_eq!(f.format(), PixelFormat::F32);
	assert!(f.is_allocated());
	// Expected size: 4 channels x 4 bytes, linesize 32-byte aligned.
	assert_eq!(f.allocated_size(), (16 * 1920) * 1080);

	// The frame must contain non-zero pixels.
	let data = f.data().expect("allocated data");
	assert!(data.iter().any(|&b| b != 0), "decoded frame is all zeros");
}

#[test]
fn decode_video_frame_at_midpoint() {
	let d = FFmpegDecoder::new();
	let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 0, None);
	d.open(&s).expect("open video stream");

	let f = d
		.retrieve_video_frame(&video_params(s, Rational::new(8, 1)))
		.expect("decode mid frame");
	assert_eq!(f.width(), 1920);
	assert_eq!(f.height(), 1080);
	assert!(f.data().unwrap().iter().any(|&b| b != 0));
}

#[test]
fn audio_decode_is_non_empty() {
	let d = FFmpegDecoder::new();
	let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 1, None);
	d.open(&s).expect("open audio stream");

	// One second of stereo at 48 kHz.
	let mut dest = vec![0f32; 48000 * 2];
	let status = d
		.retrieve_audio(
			&mut dest,
			&TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
			48000,
			0x3, // stereo mask
		)
		.expect("retrieve audio");
	assert_eq!(status, RetrieveAudioStatus::Success);

	let peak = dest.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
	assert!(peak > 0.0, "decoded audio is all silence");
}

/// Contiguous audio chunks (each chunk's start is exactly where the
/// previous chunk ended) must continue the decode without re-seeking:
/// the seek count stays flat, and the concatenated chunks must match a
/// one-shot decode of the same span. A non-contiguous chunk re-seeks.
/// This is the regression test for the boundary pops/clicks caused by the
/// per-chunk `av_seek_frame` + decoder flush + resampler reset.
#[test]
fn contiguous_audio_chunks_skip_seek_and_match_oneshot() {
	let d = FFmpegDecoder::new();
	let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 1, None);
	d.open(&s).expect("open audio stream");

	// Chunk [0s, 1s): opens the session (one seek inside retrieve).
	let mut c1 = vec![0f32; 48000 * 2];
	d.retrieve_audio(
		&mut c1,
		&TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		48000,
		0x3,
	)
	.expect("chunk [0,1)");
	assert!(c1.iter().any(|&v| v != 0.0), "chunk [0,1) is all silence");
	let seeks_after_first = d.audio_seek_count();

	// Chunk [1s, 2s): contiguous with the previous one — must not seek.
	let mut c2 = vec![0f32; 48000 * 2];
	d.retrieve_audio(
		&mut c2,
		&TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		48000,
		0x3,
	)
	.expect("chunk [1,2)");
	assert!(
		d.audio_seek_count() == seeks_after_first,
		"contiguous chunk must skip the seek ({} -> {})",
		seeks_after_first,
		d.audio_seek_count()
	);
	assert!(c2.iter().any(|&v| v != 0.0), "chunk [1,2) is all silence");

	// Chunk [3s, 4s): not contiguous — must seek again.
	let mut c4 = vec![0f32; 48000 * 2];
	d.retrieve_audio(
		&mut c4,
		&TimeRange::new(Rational::new(3, 1), Rational::new(4, 1)),
		48000,
		0x3,
	)
	.expect("chunk [3,4)");
	assert!(
		d.audio_seek_count() > seeks_after_first,
		"non-contiguous chunk must seek"
	);

	// Concatenating [0,1)+[1,2) must equal a one-shot decode of [0,2):
	// both read the same decoder continuously from sample 0, so the
	// samples must match exactly (the second chunk only differs in that it
	// skipped its seek — no flush/resampler reset was involved).
	let mut combined = c1;
	combined.extend_from_slice(&c2);
	let mut oneshot = vec![0f32; 48000 * 2 * 2];
	d.retrieve_audio(
		&mut oneshot,
		&TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
		48000,
		0x3,
	)
	.expect("oneshot [0,2)");
	let mut max_diff = 0.0f32;
	for (a, b) in combined.iter().zip(oneshot.iter()) {
		max_diff = max_diff.max((a - b).abs());
	}
	assert!(
		max_diff < 0.01,
		"contiguous chunks diverge from one-shot decode (max diff {max_diff})"
	);
}

/// Playback-shaped chunks (one 25 fps frame = 1920 samples at 48 kHz) tile
/// the AAC 1024-sample grid badly: the codec frame crossing each chunk end
/// leaves a 128..896-sample tail past the chunk, and because the decoder
/// has already consumed that frame the next chunk used to start with a
/// hole of exactly that size (7 of 8 chunks — the periodic stutter). The
/// overflow carry (`AudioDecodeState::carry`) must make chunked decoding
/// sample-exact against a one-shot decode of the same span. The fixture is
/// a continuous 440 Hz tone (demo.mp4's audio is digital silence and
/// cannot expose the holes).
#[test]
fn playback_sized_chunks_match_oneshot_sample_exact() {
	let d = FFmpegDecoder::new();
	let tone = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../oak-app/tests/tone48k.mp4");
	let s = CodecStream::with_block(tone.to_string_lossy().into_owned(), 0, None);
	d.open(&s).expect("open tone audio stream");

	const CHUNK: usize = 1920; // one 25 fps frame at 48 kHz
	const CHUNKS: usize = 175; // 7 seconds (the tone is 8 s)
	let mut chunked = Vec::with_capacity(CHUNK * CHUNKS * 2);
	for i in 0..CHUNKS as i64 {
		let mut dest = vec![0f32; CHUNK * 2];
		d.retrieve_audio(
			&mut dest,
			&TimeRange::new(Rational::new(i, 25), Rational::new(i + 1, 25)),
			48000,
			0x3,
		)
		.expect("playback-sized chunk");
		chunked.extend_from_slice(&dest);
	}

	// Guard against a vacuous pass on silent media: the span must
	// contain real content.
	let peak = chunked.iter().fold(0.0f32, |a, &v| a.max(v.abs()));
	assert!(peak > 0.05, "tone span is unexpectedly quiet");

	// A separate fresh decoder: both paths decode the span straight from
	// a seek to 0, so the only tolerated difference is the chunking
	// itself (none — the carry makes them sample-exact). NOTE: decoding
	// on the SAME decoder after a seek is NOT bit-identical to a fresh
	// decode (libavcodec/demuxer priming state survives avcodec_flush;
	// ~1e-2 wobble, inaudible) — a separate pre-existing quirk, not what
	// this test guards.
	let d2 = FFmpegDecoder::new();
	let s2 = CodecStream::with_block(tone.to_string_lossy().into_owned(), 0, None);
	d2.open(&s2).expect("open tone audio stream (oneshot)");
	let mut oneshot = vec![0f32; CHUNK * CHUNKS * 2];
	d2.retrieve_audio(
		&mut oneshot,
		&TimeRange::new(Rational::new(0, 1), Rational::new(7, 1)),
		48000,
		0x3,
	)
	.expect("oneshot [0,7)");

	let mut max_diff = 0.0f32;
	let mut worst: Vec<(usize, f32)> = Vec::new();
	for (i, (a, b)) in chunked.iter().zip(oneshot.iter()).enumerate() {
		let d = (a - b).abs();
		max_diff = max_diff.max(d);
		if d > 1e-6 {
			worst.push((i, d));
		}
	}
	if !worst.is_empty() {
		worst.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
		let positions: Vec<String> = worst
			.iter()
			.take(20)
			.map(|(i, d)| format!("{}(chunk {},+{},{:.4})", i, i / (CHUNK * 2), (i / 2) % CHUNK, d))
			.collect();
		eprintln!("diffs>1e-6: {} total; worst: {}", worst.len(), positions.join(" "));
	}
	assert!(
		max_diff < 1e-6,
		"playback-sized chunks diverge from one-shot decode (max diff {max_diff}); \
		 the chunk-end overflow carry is dropping samples"
	);
}

#[test]
fn encode_h264_roundtrip_to_tmp() {
	let out = std::env::temp_dir().join(format!("oakcodec_roundtrip_{}.mp4", std::process::id()));
	let params = h264_params(&out);
	let out_str = out.to_str().expect("utf8 temp path").to_string();

	let e = create_from_params(&params).expect("create ffmpeg encoder");
	assert_eq!(e.id(), "ffmpeg");
	e.configure(&params).expect("configure");
	e.open().expect("open output");

	// Encode 10 frames with a moving pattern.
	for i in 0..10 {
		let f = pattern_frame(i);
		e.write_video(&f).expect("write video frame");
	}
	e.flush().expect("flush");

	// The output exists and has a plausible size.
	assert!(out.exists(), "round-trip file was not created");
	assert!(
		out.metadata().unwrap().len() > 1000,
		"round-trip file is empty"
	);

	// Probe the result: one 64x64 video stream.
	let d = FFmpegDecoder::new();
	let desc = d.probe(&out_str, None).expect("probe round-trip output");
	assert_eq!(desc.video_stream_count(), 1);
	let vp = desc.get_video_stream(0).expect("video stream");
	assert_eq!(vp.width(), 64);
	assert_eq!(vp.height(), 64);

	// Decode the first frame of the result.
	let s = CodecStream::with_block(out_str.clone(), 0, None);
	d.open(&s).expect("open round-trip video");
	let f = d
		.retrieve_video_frame(&video_params(s, Rational::new(0, 1)))
		.expect("decode round-trip first frame");
	assert_eq!(f.width(), 64);
	assert_eq!(f.height(), 64);
	assert_eq!(f.format(), PixelFormat::F32);
	assert!(f.data().unwrap().iter().any(|&b| b != 0));

	let _ = std::fs::remove_file(&out);
}

#[test]
fn audio_conform_writes_planar_pcm() {
	let d = FFmpegDecoder::new();
	let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 1, None);
	d.open(&s).expect("open audio stream");

	let dir = std::env::temp_dir().join(format!("oakcodec_conform_{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let ch0 = dir.join("0.pcm").to_string_lossy().into_owned();
	let ch1 = dir.join("1.pcm").to_string_lossy().into_owned();

	d.conform_audio(&[ch0.clone(), ch1.clone()], 48000, 0x3, 4, None)
		.expect("conform to f32 planar");

	for path in [&ch0, &ch1] {
		let meta = std::fs::metadata(path).expect("conform output exists");
		assert!(meta.len() > 0, "conform file is empty");
		// 1 second at 48kHz * 4 bytes = 192 KB minimum.
		assert!(
			meta.len() >= 192_000,
			"conform file too short: {}",
			meta.len()
		);
	}

	let _ = std::fs::remove_dir_all(&dir);
}

/// Count red-dominant pixels on `row` within the left half of the frame
/// (`x < width/2`). The test pattern's red/blue boundary sweeps right, so
/// the left half is solid red for exactly `32 - shift` columns; the right
/// half contains the wrap-around red strip, which is excluded here. MPEG-2's
/// lossy YUV round-trip keeps the dominance, shifting the boundary by at
/// most a couple of columns.
fn red_row_count(f: &Frame, row: usize) -> usize {
	let stride = f.linesize_bytes() as usize;
	let data = f.data().expect("allocated frame data");
	let width = f.width() as usize;
	let mut count = 0;
	for x in 0..width / 2 {
		let off = row * stride + x * 16;
		let r = f32::from_le_bytes(data[off..off + 4].try_into().unwrap());
		let b = f32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap());
		if r > b {
			count += 1;
		}
	}
	count
}

#[test]
fn testmedia_clip_probe_roundtrip() {
	let out = std::env::temp_dir().join(format!("oakcodec_tm3_{}.mp4", std::process::id()));
	crate::testmedia::write_test_clip(&out, 64, 64, 10, 10).expect("generate");
	println!("out: {}", out.display());
	let d = FFmpegDecoder::new();

	// The container duration must be ~1s (10 frames at 10 fps) in the video
	// stream's own time base. This is the regression test for the encoder
	// timestamp fix: when the muxer's `write_header` time-base change was
	// ignored, the whole clip was crammed into ~1ms and this failed.
	let desc = d.probe(&out.to_string_lossy(), None).expect("probe");
	let vp = desc.get_video_stream(0).expect("video stream");
	let (tb_num, tb_den) = vp.time_base();
	let duration_secs = vp.duration() as f64 * tb_num as f64 / tb_den as f64;
	assert!(
		(0.8..=1.2).contains(&duration_secs),
		"container duration {duration_secs}s is not ~1s (tb {tb_num}/{tb_den}, {} ticks)",
		vp.duration()
	);

	// Every frame must decode to its own content: the pattern's red/blue
	// boundary sweeps right `shift` columns per frame, so the red-run
	// length in the left half of row 32 is 32 - shift (tolerance for
	// MPEG-2 loss).
	let s = CodecStream::with_block(out.to_string_lossy().into_owned(), 0, None);
	d.open(&s).expect("open");
	for t in 0..10i64 {
		let f = d
			.retrieve_video_frame(&video_params(s.clone(), Rational::new(t, 10)))
			.unwrap_or_else(|e| panic!("decode t={t}: {e:?}"));
		let red = red_row_count(&f, 32);
		let shift = (t * 64 / (2 * 10)) % 64;
		let expected = 32 - shift;
		assert!(
			(red as i64 - expected).abs() <= 3,
			"t={t}/10 red pixels on row 32: {red}, expected ~{expected} (shift {shift})"
		);
	}

	// OAK_KEEP_TESTMEDIA keeps the clip for external inspection (ffprobe).
	if std::env::var_os("OAK_KEEP_TESTMEDIA").is_none() {
		let _ = std::fs::remove_file(&out);
	}
}

#[test]
fn testmedia_audio_track_encodes() {
	let out = std::env::temp_dir().join(format!("oakcodec_tm_audio_{}.mp4", std::process::id()));
	crate::testmedia::write_test_clip(&out, 64, 64, 10, 10).expect("generate with audio");
	let d = FFmpegDecoder::new();
	let desc = d.probe(&out.to_string_lossy(), None).expect("probe");
	assert!(desc.audio_stream_count() >= 1, "audio stream present");
	let _ = std::fs::remove_file(&out);
}

/// Serialize the config-toggling hardware-decode test (the config store
/// is process-global; decode tests must not observe each other's flag).
static HW_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hardware decoding (the mandated default): with the switch ON the
/// session must open the platform's hardware decoder (on macOS,
/// `h264_videotoolbox` for the H.264 demo); with the switch OFF it must
/// be pure software. Both paths must decode the same frame to matching
/// pixels (small tolerance for decoder rounding).
#[test]
fn hardware_decode_matches_software_decode() {
	let _guard = HW_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let config = oak_core::configstore::ConfigStore::instance();
	let key = crate::hwdecode::CONFIG_KEY_HARDWARE_DECODING;

	let decode_at = |time: i64| -> (Option<String>, Arc<Frame>) {
		let d = FFmpegDecoder::new();
		let s = CodecStream::with_block(demo_path().to_string_lossy().into_owned(), 0, None);
		d.open(&s).expect("open video stream");
		let hw = d.hw_decoder_name();
		let f = d
			.retrieve_video_frame(&video_params(s, Rational::new(time, 1)))
			.expect("decode frame");
		(hw, f)
	};

	// Hardware path.
	config.set(None, key, "true");
	let (hw_name, hw_frame) = decode_at(5);
	#[cfg(target_os = "macos")]
	{
		let transfers =
			crate::hwdecode::HW_TRANSFERS.load(std::sync::atomic::Ordering::Relaxed);
		if hw_name.as_deref() != Some("videotoolbox") || transfers == 0 {
			// Headless/virtualized macOS (CI runners) cannot bring up
			// VideoToolbox ("hwaccel initialisation returned error"); the
			// decoder then falls back to software and the engagement
			// mandate can only be asserted where the hardware path exists.
			eprintln!("VideoToolbox unavailable on this host; skipping hw assertion");
			return;
		}
	}
	#[cfg(not(target_os = "macos"))]
	assert!(
		hw_name.is_none()
			|| hw_name.as_deref().unwrap().contains("vaapi")
			|| hw_name.as_deref().unwrap().contains("nvdec")
			|| hw_name.as_deref().unwrap().contains("d3d11va"),
		"unexpected decoder {hw_name:?}"
	);

	// Software path (the switch off).
	config.set(None, key, "false");
	let (sw_name, sw_frame) = decode_at(5);
	assert!(sw_name.is_none(), "switch off must force software decoding");
	config.set(None, key, "true");

	// Same geometry, same pixels (within decoder rounding — the threshold
	// is generous because VideoToolbox's YUV→RGB conversion legitimately
	// differs from swscale by ~1 LSB of the intermediate depth).
	assert_eq!(
		(hw_frame.width(), hw_frame.height()),
		(sw_frame.width(), sw_frame.height())
	);
	let (hw_data, sw_data) = (hw_frame.data().unwrap(), sw_frame.data().unwrap());
	assert_eq!(hw_data.len(), sw_data.len());
	let mut max_diff = 0.0f32;
	for (a, b) in hw_data.chunks_exact(4).zip(sw_data.chunks_exact(4)) {
		let fa = f32::from_le_bytes(a.try_into().unwrap());
		let fb = f32::from_le_bytes(b.try_into().unwrap());
		max_diff = max_diff.max((fa - fb).abs());
	}
	assert!(
		max_diff < 0.08,
		"hardware and software decodes diverge (max channel diff {max_diff})"
	);
}
