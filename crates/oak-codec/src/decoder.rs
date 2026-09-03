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

//! `olive::Decoder` and its supporting types — the media-decoder trait.
//!
//! Mirrors `src/codec/src/decoder.h`. The C++ abstract base plus its
//! FFmpeg/OIIO subclasses become the [`Decoder`] trait (decision 2 in
//! README.md); probe/dispatch lives on the registry functions at the
//! bottom of this module. Audio is handled in raw interleaved-float
//! buffers matching the C ABI, not `oak_core::SampleBuffer` (which the
//! crate does not export).

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use oak_core::cancelatom::CancelAtom;
use oak_core::{Rational, TimeRange};

use crate::footagedescription::FootageDescription;
use crate::frame::Frame;

/// `OakRenderTexture` — GPU texture token (an oakrender type, opaque to
/// oakcodec). The codec crate cannot depend on oakrender (dependency
/// cycle), so it never constructs a real texture: [`Decoder::retrieve_video`]
/// only reports whether the decode succeeded and returns this unit token
/// (the former empty `CHandle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OakRenderTexture;

/// `OakNodeBlock` — opaque timeline-block token owned elsewhere; the codec
/// only stores and forwards it (borrowed, never dereferenced). No module
/// ever constructs one (the former `CHandle` was only ever `None`), so the
/// type is an empty marker kept for API compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OakNodeBlock;

/// `oakcodec_video_stream_info` — POD probe output describing one video
/// stream; see `include/codec/decoder.h`.
#[repr(C)]
pub struct OakCodecVideoStreamInfo {
	/// Stream index.
	pub stream_index: i32,
	/// Width in pixels.
	pub width: i32,
	/// Height in pixels.
	pub height: i32,
	/// Frame-rate numerator.
	pub frame_rate_num: i32,
	/// Frame-rate denominator.
	pub frame_rate_den: i32,
	/// Stream length in time-base units.
	pub duration_ts: i64,
	/// Time-base numerator (seconds per time-base unit).
	pub time_base_num: i32,
	/// Time-base denominator.
	pub time_base_den: i32,
	/// Native delivery `OakPixelFormat`.
	pub format: i32,
	/// Plane channel count.
	pub channel_count: i32,
	/// ISO/IEC 23001-8 color-primaries code point (0 = unknown).
	pub color_primaries: i32,
	/// ISO/IEC 23001-8 color-transfer code point (0 = unknown).
	pub color_trc: i32,
	/// 1 when the stream is interlaced.
	pub interlaced: i32,
}

/// `oakcodec_audio_stream_info` — POD probe output describing one audio
/// stream; see `include/codec/decoder.h`.
#[repr(C)]
pub struct OakCodecAudioStreamInfo {
	/// Stream index.
	pub stream_index: i32,
	/// Sample rate (Hz).
	pub sample_rate: i32,
	/// ffmpeg-style channel mask (e.g. 0x3 = stereo).
	pub channel_layout: u64,
	/// Channel count.
	pub channel_count: i32,
	/// Stream length in time-base units.
	pub duration_ts: i64,
	/// Time-base numerator.
	pub time_base_num: i32,
	/// Time-base denominator.
	pub time_base_den: i32,
}

/// Local replacement for `render/rendermodes.h` (oakrender C API has no
/// render-mode counterpart). Values mirror engine/render/rendermodes.h:
/// k_offline = 0, k_online = 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum RenderMode {
	/// Offline / background render.
	Offline = 0,
	/// Online / real-time render.
	Online = 1,
}

/// "Don't force a color range" sentinel for
/// [`RetrieveVideoParams::force_range`] (the actual ranges are the
/// `oak_core_COLOR_RANGE_*` values).
pub const K_COLOR_RANGE_DEFAULT: i32 = -1;

/// `Decoder::RetrieveVideoParams` — what a video retrieve call needs.
pub struct RetrieveVideoParams {
	/// Stream to read from.
	pub stream: CodecStream,
	/// Timestamp, rational seconds.
	pub time: Rational,
	/// Length of footage before the start (for early-seek semantics).
	pub length: TimeRange,
	/// Color range override; [`K_COLOR_RANGE_DEFAULT`] means "don't force".
	pub force_range: i32,
	/// Image sequence: bake the frame number into the filename.
	pub is_image_sequence: bool,
	/// Image sequence digit count (derived from the filename).
	pub image_sequence_digits: i32,
	/// Image sequence number to substitute.
	pub image_sequence_number: i64,
	/// Render mode (drives texture-path choices in the implementations).
	pub mode: RenderMode,
	/// Frame alpha channel is premultiplied.
	pub alpha_is_premultiplied: bool,
	/// Target output size for the scaled frame: `Some((w, h))` lets the
	/// decoder's swscale pass convert AND resize in one step (native
	/// yuv → RGBA/F32 at the target size), skipping the full-resolution
	/// float intermediate (a 4K frame is ~132 MB as F32 RGBA — decoding
	/// to a 480px preview through it costs ~260 MB of churn per frame).
	/// `None` keeps the native size (the old behavior).
	pub target_size: Option<(u32, u32)>,
}

/// The colorimetry of a decoded frame, as carried out of the bitstream
/// (raw ISO/IEC 23001-8 / H.273 code points — the same numbering FFmpeg's
/// `AVCodecParameters` uses). The render layer maps these to its input
/// transform (source colorspace → the pipeline working space).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DecodedColorMeta {
	/// Color primaries code point (`AVCOL_PRI_*`; 0/2 = unknown).
	pub color_primaries: i32,
	/// Transfer characteristic code point (`AVCOL_TRC_*`; 0/2 = unknown).
	pub color_trc: i32,
	/// True when the decoded RGB is full range (the YUV→RGB used the
	/// full-range coefficients).
	pub full_range: bool,
}

/// `Decoder::RetrieveAudioStatus` — outcome of an audio retrieve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetrieveAudioStatus {
	/// Data written to the destination buffer.
	Success,
	/// The requested range is outside the footage.
	InvalidRange,
	/// The stream does not support audio.
	Unsupported,
	/// Media requires a conform that could not be produced.
	ConformNeeded,
	/// A decoder-level error occurred.
	Error,
}

/// `Decoder::RetrieveState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetrieveState {
	/// Ready to decode.
	Ready,
	/// Failed to open the stream.
	FailedToOpen,
	/// The stream index could not be located.
	IndexUnavailable,
}

/// `Decoder::CodecStream` — identifies one (filename, stream) pair plus an
/// optional associated timeline block.
///
/// The block is an opaque [`OakNodeBlock`] token that codec only stores and
/// compares, never dereferences or retains (borrowed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecStream {
	filename: String,
	stream: i32,
	block: Option<OakNodeBlock>,
}

impl CodecStream {
	/// Empty, invalid stream.
	pub fn new() -> Self {
		CodecStream {
			filename: String::new(),
			stream: -1,
			block: None,
		}
	}

	/// New stream for `(filename, stream)` with an optional block.
	pub fn with_block(
		filename: String,
		stream: i32,
		block: Option<OakNodeBlock>,
	) -> Self {
		CodecStream {
			filename,
			stream,
			block,
		}
	}

	/// Non-empty filename and non-negative stream index.
	pub fn is_valid(&self) -> bool {
		!self.filename.is_empty() && self.stream >= 0
	}

	/// The file exists on disk.
	pub fn exists(&self) -> bool {
		Path::new(&self.filename).exists()
	}

	/// Reset to the empty stream.
	pub fn reset(&mut self) {
		self.filename.clear();
		self.stream = -1;
		self.block = None;
	}

	/// Source filename.
	pub fn filename(&self) -> &str {
		&self.filename
	}

	/// Stream index within the source.
	pub fn stream(&self) -> i32 {
		self.stream
	}

	/// Associated timeline block (borrowed; only compared, never used).
	pub fn block(&self) -> Option<OakNodeBlock> {
		self.block.clone()
	}
}

/// `olive::Decoder` — abstraction over external media decoding.
///
/// Implementations are [`crate::ffmpeg::FFmpegDecoder`] and
/// [`crate::oiio::OIIODecoder`]. The trait surface mirrors the C++
/// abstract base; the public API hands out `Arc<dyn Decoder>` values.
pub trait Decoder: Send + Sync {
	/// Unique decoder id ("ffmpeg"/"oiio").
	fn id(&self) -> String;

	/// Whether this decoder supports video streams.
	fn supports_video(&self) -> bool {
		false
	}

	/// Whether this decoder supports audio streams.
	fn supports_audio(&self) -> bool {
		false
	}

	/// Whether this decoder can read the given file (static probe).
	fn probe(
		&self,
		filename: &str,
		cancelled: Option<&CancelAtom>,
	) -> Option<FootageDescription>;

	/// Open `stream` for decoding. Thread-safe.
	fn open(&self, stream: &CodecStream) -> crate::error::Result<()>;

	/// Close the currently open stream (safe when closed).
	fn close(&self) -> crate::error::Result<()>;

	/// The currently open stream (locked accessor).
	fn stream(&self) -> CodecStream;

	/// Retrieve a video frame into CPU memory.
	fn retrieve_video_frame(&self, p: &RetrieveVideoParams) -> crate::error::Result<Arc<Frame>>;

	/// Retrieve a video frame as a render texture (owned by caller).
	fn retrieve_video(&self, p: &RetrieveVideoParams) -> crate::error::Result<OakRenderTexture>;

	/// Retrieve interleaved audio covering `range` into `dest` (floats).
	fn retrieve_audio(
		&self,
		dest: &mut [f32],
		range: &TimeRange,
		sample_rate: i32,
		channel_layout: u64,
	) -> crate::error::Result<RetrieveAudioStatus>;

	/// Conform the open stream's audio into per-channel pcm files.
	///
	/// `sample_rate` / `channel_layout` / `sample_format` describe the
	/// target audio format (`sample_format` is a
	/// `olive::core::SampleFormat::Format` value). The C++ side builds its
	/// `core::AudioParams` from these three — mirroring the C ABI
	/// `oakcodec_decoder_conform_audio` argument list.
	fn conform_audio(
		&self,
		output_filenames: &[String],
		sample_rate: i32,
		channel_layout: u64,
		sample_format: i32,
		cancelled: Option<&CancelAtom>,
	) -> crate::error::Result<()>;

	/// Unless the media requires it, prefer hardware over software (e.g. the
	/// FFmpeg decoder's platform hwaccel). The decode-session cache evicts
	/// hardware sessions first: each one pins GPU memory (an NVDEC 4K
	/// decoder holds ~10 surface frames of `4096×2160×1.5` ≈ 100+ MB of
	/// CUDA memory), while software sessions pin only system RAM.
	fn hardware_decoding(&self) -> bool {
		false
	}

	/// Offset of the audio start relative to the video (rational seconds).
	fn get_audio_start_offset(&self) -> Rational {
		// C++ default `virtual Rational get_audio_start_offset() const { return 0; }`
		Rational::new(0, 1)
	}
}

/// Placeholder decoder used by the built-in probe registry.
///
/// Reports the correct id and capability flags so id-based dispatch
/// (`create_from_id`) works, but every media operation is unimplemented
/// and returns `None` / an error. Used for the OIIO entry, whose Rust
/// implementation (`crate::oiio::OIIODecoder`) is still a dylib stub; the
/// FFmpeg entry is the real [`crate::ffmpeg::FFmpegDecoder`].
struct UnimplementedDecoder {
	id: &'static str,
	video: bool,
	audio: bool,
}

impl UnimplementedDecoder {
	fn new(id: &'static str, video: bool, audio: bool) -> Self {
		UnimplementedDecoder { id, video, audio }
	}
}

impl Decoder for UnimplementedDecoder {
	fn id(&self) -> String {
		self.id.to_string()
	}

	fn supports_video(&self) -> bool {
		self.video
	}

	fn supports_audio(&self) -> bool {
		self.audio
	}

	fn probe(
		&self,
		_filename: &str,
		_cancelled: Option<&CancelAtom>,
	) -> Option<FootageDescription> {
		None
	}

	fn open(&self, _stream: &CodecStream) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}

	fn close(&self) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}

	fn stream(&self) -> CodecStream {
		CodecStream::new()
	}

	fn retrieve_video_frame(&self, _p: &RetrieveVideoParams) -> crate::error::Result<Arc<Frame>> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}

	fn retrieve_video(&self, _p: &RetrieveVideoParams) -> crate::error::Result<OakRenderTexture> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}

	fn retrieve_audio(
		&self,
		_dest: &mut [f32],
		_range: &TimeRange,
		_sample_rate: i32,
		_channel_layout: u64,
	) -> crate::error::Result<RetrieveAudioStatus> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}

	fn conform_audio(
		&self,
		_output_filenames: &[String],
		_sample_rate: i32,
		_channel_layout: u64,
		_sample_format: i32,
		_cancelled: Option<&CancelAtom>,
	) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"decoder not yet implemented".to_string(),
		))
	}
}

/// `Decoder::create_from_id` — instantiate a decoder by id, or `None`.
pub fn create_from_id(id: &str) -> Option<Arc<dyn Decoder>> {
	if id.is_empty() {
		return None;
	}

	receive_list_of_all_decoders()
		.into_iter()
		.find(|d| d.id() == id)
}

/// Test-injected decoder registry (see [`set_test_decoders`]); empty when
/// not injected, in which case the built-in list below is used.
static TEST_DECODERS: OnceLock<Mutex<Vec<Arc<dyn Decoder>>>> = OnceLock::new();

/// Serializes every test that reads the built-in decoder registry. Tests
/// inject through [`set_test_decoders`] under [`crate::lock_tests`] (the
/// shared test lock), so the registry assertions below take that same lock
/// to never race with an injected list.
#[cfg(test)]
fn registry_guard() -> crate::TestLock {
	crate::lock_tests()
}

/// Replace the decoder registry with `list`; pass an empty list to restore
/// the built-in decoders.
///
/// Test/extension support (the C ABI has no way to register a decoder, so
/// the contract tests drive the probe/dispatch paths through a fake
/// decoder). Hidden from docs; never called by production code.
#[doc(hidden)]
pub fn set_test_decoders(list: Vec<Arc<dyn Decoder>>) {
	let store = TEST_DECODERS.get_or_init(|| Mutex::new(Vec::new()));
	*store.lock().unwrap() = list;
}

/// `Decoder::receive_list_of_all_decoders` — all registered decoders.
///
/// Order is probe priority, mirroring C++: OIIO (more specific) before
/// FFmpeg (format-agnostic fallback). The OIIO entry is an
/// [`UnimplementedDecoder`] stub (the OIIO engine is not ported); the
/// FFmpeg entry is the real [`crate::ffmpeg::FFmpegDecoder`]. When tests
/// injected a non-empty list via [`set_test_decoders`], that list takes
/// precedence.
pub fn receive_list_of_all_decoders() -> Vec<Arc<dyn Decoder>> {
	if let Some(store) = TEST_DECODERS.get() {
		let injected = store.lock().unwrap();
		if !injected.is_empty() {
			return injected.clone();
		}
	}
	vec![
		Arc::new(UnimplementedDecoder::new("oiio", false, false)),
		Arc::new(crate::ffmpeg::FFmpegDecoder::new()),
	]
}

/// Image-sequence filename heuristics (static).
///
/// Replace the trailing digit run of the filename stem with the
/// zero-padded decimal representation of `number` (keeps the same digit
/// count), mirroring `Decoder::transform_image_sequence_file_name`.
pub fn transform_image_sequence_file_name(filename: &str, number: i64) -> String {
	let digit_count = get_image_sequence_digit_count(filename) as usize;

	let path = Path::new(filename);
	let file_name = path
		.file_name()
		.and_then(|n| n.to_str())
		.unwrap_or(filename);

	// QFileInfo::completeBaseName(): filename up to the first '.'.
	let original_basename = match file_name.find('.') {
		Some(dot) => &file_name[..dot],
		None => file_name,
	};

	// New stem = original stem minus the trailing digit run, plus the
	// zero-padded number (`snprintf("%0*lld", digit_count, number)`).
	let cut = original_basename.len().saturating_sub(digit_count);
	let new_basename = format!(
		"{}{:0width$}",
		&original_basename[..cut],
		number,
		width = digit_count
	);

	// Replace every occurrence of the original stem in the filename.
	let mut new_filename = file_name.to_string();
	let mut pos = 0;
	while let Some(rel) = new_filename[pos..].find(original_basename) {
		let start = pos + rel;
		let end = start + original_basename.len();
		new_filename.replace_range(start..end, &new_basename);
		pos = start + new_basename.len();
	}

	match path.parent() {
		Some(parent) if !parent.as_os_str().is_empty() => Path::new(parent)
			.join(&new_filename)
			.to_string_lossy()
			.into_owned(),
		_ => new_filename,
	}
}

/// Number of trailing digits in the filename stem (0 = not a sequence).
pub fn get_image_sequence_digit_count(filename: &str) -> i32 {
	let file_name = Path::new(filename)
		.file_name()
		.and_then(|n| n.to_str())
		.unwrap_or(filename);

	// QFileInfo::completeBaseName(): filename up to the first '.'.
	let stem = match file_name.find('.') {
		Some(dot) => &file_name[..dot],
		None => file_name,
	};

	let mut count: i32 = 0;
	for ch in stem.chars().rev() {
		if ch.is_ascii_digit() {
			count += 1;
		} else {
			break;
		}
	}

	count
}

/// Numeric value of the trailing digits (0 when there are none).
///
/// Mirrors C++ `Decoder::get_image_sequence_index`, which slices the
/// trailing digit run (`basename.substr(basename.size() - digit_count)`) and
/// passes it to `strtoll`. Because that slice is empty when there are no
/// trailing digits (digit_count == 0) and all-digits otherwise, the value is
/// the parsed number, or `0` for a non-sequence.
pub fn get_image_sequence_index(filename: &str) -> i64 {
	let digit_count = get_image_sequence_digit_count(filename) as usize;

	let file_name = Path::new(filename)
		.file_name()
		.and_then(|n| n.to_str())
		.unwrap_or(filename);

	// QFileInfo::completeBaseName(): filename up to the first '.'.
	let stem = match file_name.find('.') {
		Some(dot) => &file_name[..dot],
		None => file_name,
	};

	// Trailing digit run (empty when the stem has no trailing digits).
	let start = stem.len().saturating_sub(digit_count);
	let number_only = &stem[start..];

	// `strtoll(..., base 10)`: the slice is empty-or-digits, so a plain
	// decimal parse with 0 on failure reproduces the C++ result.
	number_only.parse::<i64>().unwrap_or(0)
}

/// The `k_any_timecode` rational constant.
///
/// C++ `const Rational Decoder::k_any_timecode = RATIONAL_MIN;`, which the
/// i32 reduction cap normalizes to `-2147483647/1`.
pub fn k_any_timecode() -> Rational {
	Rational::new(-2147483647, 1)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn codec_stream_new_is_invalid() {
		let s = CodecStream::new();
		assert!(!s.is_valid());
		assert!(s.filename().is_empty());
		assert_eq!(s.stream(), -1);
		assert_eq!(s.block(), None);
	}

	#[test]
	fn codec_stream_with_block_is_valid() {
		let s = CodecStream::with_block("video.mov".to_string(), 1, None);
		assert!(s.is_valid());
		assert_eq!(s.filename(), "video.mov");
		assert_eq!(s.stream(), 1);

		// Negative stream index is invalid regardless of filename.
		let bad = CodecStream::with_block("video.mov".to_string(), -1, None);
		assert!(!bad.is_valid());
	}

	#[test]
	fn codec_stream_reset_clears() {
		let mut s = CodecStream::with_block("video.mov".to_string(), 2, None);
		s.reset();
		assert!(!s.is_valid());
		assert!(s.filename().is_empty());
		assert_eq!(s.stream(), -1);
	}

	#[test]
	fn digit_count_counts_trailing_digits() {
		assert_eq!(get_image_sequence_digit_count("frame_0001.png"), 4);
		assert_eq!(get_image_sequence_digit_count("frame.png"), 0);
		assert_eq!(get_image_sequence_digit_count("img000.jpg"), 3);
		// Digits before the final char are not trailing digits.
		assert_eq!(get_image_sequence_digit_count("a1b.png"), 0);
	}

	#[test]
	fn image_sequence_index_parses_number() {
		assert_eq!(get_image_sequence_index("frame_0001.png"), 1);
		assert_eq!(get_image_sequence_index("img012.jpg"), 12);
		assert_eq!(get_image_sequence_index("0009.png"), 9);
		// No trailing digits: the sliced run is empty, so the value is 0.
		assert_eq!(get_image_sequence_index("frame.png"), 0);
		assert_eq!(get_image_sequence_index("12abc.png"), 0);
	}

	#[test]
	fn transform_image_sequence_substitutes_number() {
		assert_eq!(
			transform_image_sequence_file_name("frame_0001.png", 5),
			"frame_0005.png"
		);
		assert_eq!(
			transform_image_sequence_file_name("dir/img012.jpg", 7),
			// Path::join separators are platform-native (\ on Windows).
			std::path::Path::new("dir")
				.join("img007.jpg")
				.to_string_lossy()
		);
		// No digit run: number appended with no padding (C++ behavior).
		assert_eq!(
			transform_image_sequence_file_name("frame.png", 3),
			"frame3.png"
		);
		// All-digit stem: whole run is replaced.
		assert_eq!(
			transform_image_sequence_file_name("0001.png", 7),
			"0007.png"
		);
	}

	#[test]
	fn k_any_timecode_is_rational_min() {
		let tc = k_any_timecode();
		assert_eq!(tc.numerator(), -2147483647);
		assert_eq!(tc.denominator(), 1);
	}

	#[test]
	fn registry_lists_oiio_then_ffmpeg() {
		let _g = registry_guard();
		let list = receive_list_of_all_decoders();
		let ids: Vec<String> = list.iter().map(|d| d.id()).collect();
		// Probe priority: OIIO (specific) first, FFmpeg (fallback) last.
		assert_eq!(ids, vec!["oiio".to_string(), "ffmpeg".to_string()]);
	}

	#[test]
	fn create_from_id_matches_registry() {
		let _g = registry_guard();
		assert!(create_from_id("ffmpeg").is_some());
		assert!(create_from_id("oiio").is_some());
		assert_eq!(create_from_id("ffmpeg").unwrap().id(), "ffmpeg");
		assert_eq!(create_from_id("oiio").unwrap().id(), "oiio");
		// Unknown and empty ids return None.
		assert!(create_from_id("nope").is_none());
		assert!(create_from_id("").is_none());
	}
}

#[cfg(test)]
mod tests_unimplemented {
	use super::*;

	fn builtin(id: &str) -> Arc<dyn Decoder> {
		create_from_id(id).unwrap()
	}

	#[test]
	fn ffmpeg_builtin_fails_on_missing_media_and_closes() {
		let _g = registry_guard();
		let d = builtin("ffmpeg");
		assert!(d.supports_video());
		assert!(d.supports_audio());
		// A nonexistent file cannot be probed or opened.
		assert!(d.probe("x.mp4", None).is_none());

		let s = CodecStream::with_block("x.mp4".to_string(), 0, None);
		assert!(d.open(&s).is_err());
		// C++ parity: a failed open leaves the decoder closed.
		assert_eq!(d.stream().filename(), "");
		assert!(d.close().is_ok());

		let p = RetrieveVideoParams {
			stream: CodecStream::new(),
			time: Rational::new(0, 1),
			length: TimeRange::default(),
			force_range: K_COLOR_RANGE_DEFAULT,
			is_image_sequence: false,
			image_sequence_digits: 0,
			image_sequence_number: 0,
			mode: RenderMode::Offline,
			alpha_is_premultiplied: false,
			target_size: None,
		};
		assert!(d.retrieve_video_frame(&p).is_err());
		assert!(d.retrieve_video(&p).is_err());
		let mut dest = [0f32; 4];
		assert!(d
			.retrieve_audio(
				&mut dest,
				&TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
				48000,
				0x3
			)
			.is_err());
		assert!(d
			.conform_audio(&["a.pcm".to_string()], 48000, 0x3, 10, None)
			.is_err());

		// OIIO reports no media capabilities.
		let o = builtin("oiio");
		assert!(!o.supports_video());
		assert!(!o.supports_audio());
	}

	#[test]
	fn get_audio_start_offset_defaults_to_zero() {
		let _g = registry_guard();
		let d = builtin("ffmpeg");
		let off = d.get_audio_start_offset();
		assert_eq!(off.numerator(), 0);
		assert_eq!(off.denominator(), 1);
	}
}
