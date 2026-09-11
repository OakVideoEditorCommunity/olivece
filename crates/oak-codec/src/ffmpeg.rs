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

//! `FFmpegDecoder` / `FFmpegEncoder` — real FFmpeg-backed implementations.
//!
//! Mirrors `src/codec/src/ffmpeg/{ffmpegdecoder,ffmpegencoder}.{h,cpp}`.
//! Unlike the C++ side — which routes everything through the `ffmpeg_bridge`
//! C library — this module calls the [`ffmpeg_next`] crate directly. The
//! bridge existed only to absorb FFmpeg API churn (the `liboakffmpeg`
//! dylib) and is deliberately not replicated. `// CPP-PARITY:` comments
//! point at the oracle lines each behavior mirrors.
//!
//! ## Delivery formats
//!
//! Video decode delivers **F32 RGBA** frames ([`PixelFormat::F32`], the
//! primary oakcore pipeline format): the decoded frame is run through
//! swscale into a float-RGBA target and copied into the crate's [`Frame`]
//! (4 channels, linesize rounded to 32 bytes — see [`Frame`]). Audio is
//! delivered as interleaved `f32`, matching the C ABI.
//!
//! ## Deviations from the C++ oracle (all trait-shaped)
//!
//! * [`Decoder::retrieve_audio`] decodes directly (the C++ path goes
//!   through the ConformManager cache; the Rust trait carries no cache
//!   path or loop mode).
//! * [`RetrieveVideoParams`] drops `renderer`, `divider` and
//!   `maximum_format` (the Rust trait surface), so [`Decoder::retrieve_video`]
//!   only reports decode success — it returns a unit [`OakRenderTexture`]
//!   token, never a real texture — and there is no preview-divider scaling.
//! * Probing uses stream parameters (no second decode pass), so `is_still`
//!   is always false and interlacing always progressive.
//! * Subtitle streams are counted but not added as subtitle entries.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use ffmpeg::ffi as sys;
use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::format::{Pixel, Sample};
use ffmpeg::media::Type as MediaType;
use ffmpeg::software::{resampling, scaling};
use ffmpeg::{ChannelLayout, Dictionary, Error as FfmpegError, Rational as FfRational};
use ffmpeg_next as ffmpeg;

use oak_core::cancelatom::CancelAtom;
use oak_core::colormath::YuvMatrix;
use oak_core::ocioutils::PixelFormat as OakPixelFormat;
use oak_core::videoparams::{Interlacing, VideoParams, VideoType};
use oak_core::{PixelFormat, Rational, SampleFormat, TimeRange};

use crate::audioparams::AudioParams;
use crate::decoder::{CodecStream, Decoder, OakRenderTexture, RetrieveAudioStatus, RetrieveVideoParams};
use crate::encoder::Encoder;
use crate::encodingparams::EncodingParams;
use crate::footagedescription::{FootageDescription, StreamEntry};
use crate::frame::Frame;

/// `oak_core_COLOR_RANGE_FULL`.
///
/// Both range constants keep the C ABI's spelling (they name the frozen
/// `oak_core_COLOR_RANGE_*` values), hence the lint allowance.
#[allow(non_upper_case_globals)]
const oak_core_COLOR_RANGE_FULL: i32 = 1;
/// `oak_core_COLOR_RANGE_LIMITED`.
#[allow(non_upper_case_globals)]
const oak_core_COLOR_RANGE_LIMITED: i32 = 0;
/// `AVCOL_RANGE_JPEG` (full range; AVCOL_RANGE_MPEG = 1 is limited).
const AVCOL_RANGE_JPEG: i32 = 2;
/// swscale colorspace ids (`SWS_CS_*`, libswscale/swscale.h).
const SWS_CS_ITU709: i32 = 1;
const SWS_CS_ITU601: i32 = 5;
const SWS_CS_SMPTE240M: i32 = 7;
const SWS_CS_BT2020: i32 = 9;
/// AVCOL_SPC_* code points that map onto each swscale colorspace.
const AVCOL_SPC_BT709: i32 = 1;
const AVCOL_SPC_BT470BG: i32 = 5;
const AVCOL_SPC_SMPTE170M: i32 = 6;
const AVCOL_SPC_SMPTE240M: i32 = 7;
const AVCOL_SPC_BT2020_NCL: i32 = 9;
const AVCOL_SPC_BT2020_CL: i32 = 10;
/// The format-level time base (microseconds), `FB_TIME_BASE` in the bridge.
const FB_TIME_BASE: i64 = 1_000_000;
/// `AV_NOPTS_VALUE`.
const AV_NOPTS_VALUE: i64 = i64::MIN;
/// Channel count of the internal RGBA pipeline layout.
const VIDEO_CHANNELS: i32 = 4;
/// Number of f32 samples in one RGBA pixel (R, G, B, A).
const PIXEL_F32_BYTES: usize = 16;

/// Lazily initialize the FFmpeg libraries (idempotent, at most once).
fn ffmpeg_init() -> crate::error::Result<()> {
	static INIT: OnceLock<Result<(), String>> = OnceLock::new();
	if let Err(e) = INIT
		.get_or_init(|| ffmpeg::init().map_err(|e| format!("ffmpeg initialization failed: {e}")))
	{
		return Err(crate::error::Error::Failed(e.clone()));
	}
	Ok(())
}

/// Wrap an FFmpeg error in the crate error type.
fn ffmpeg_err(e: FfmpegError) -> crate::error::Error {
	crate::error::Error::Failed(format!("ffmpeg error: {e}"))
}

/// Build a generic failure.
fn fail(msg: impl Into<String>) -> crate::error::Error {
	crate::error::Error::Failed(msg.into())
}

/// `cancel_atom_is_cancelled` — check of a cancel atom (borrowed pointer).
///
/// # CPP-PARITY
/// `src/codec/src/ffmpeg/ffmpegdecoder.cpp` (anonymous namespace helper).
fn cancel_atom_is_cancelled(cancelled: Option<&CancelAtom>) -> bool {
	cancelled.is_some_and(|atom| atom.is_cancelled())
}

/// Whether an FFmpeg error means "end of stream" or "try again".
fn is_eof_or_eagain(e: &FfmpegError) -> bool {
	matches!(e, FfmpegError::Eof)
		|| matches!(e, FfmpegError::Other { errno } if *errno == ffmpeg::error::EAGAIN)
}

/// Map a "JPEG" (full-range) pixel format to its regular counterpart,
/// mirroring the bridge `convert_jpeg_space_to_regular_space`.
fn convert_jpeg_space_to_regular_space(p: Pixel) -> Pixel {
	match p {
		Pixel::YUVJ420P => Pixel::YUV420P,
		Pixel::YUVJ422P => Pixel::YUV422P,
		Pixel::YUVJ444P => Pixel::YUV444P,
		Pixel::YUVJ440P => Pixel::YUV440P,
		Pixel::YUVJ411P => Pixel::YUV411P,
		other => other,
	}
}

/// Native `OakPixelFormat` for a decoded pixel format (probe reporting).
/// Mirrors `FFmpegDecoder::get_native_pixel_format`: 8-bit sources map to
/// [`PixelFormat::U8`], 16-bit to [`PixelFormat::U16`], float to
/// [`PixelFormat::F32`].
fn native_pixel_format(p: Pixel) -> PixelFormat {
	match p {
		Pixel::RGBF32BE
		| Pixel::RGBF32LE
		| Pixel::RGBAF32BE
		| Pixel::RGBAF32LE
		| Pixel::GBRPF32BE
		| Pixel::GBRPF32LE => PixelFormat::F32,
		Pixel::RGB48BE
		| Pixel::RGB48LE
		| Pixel::RGBA64BE
		| Pixel::RGBA64LE
		| Pixel::GBRP9BE
		| Pixel::GBRP9LE
		| Pixel::GBRP10BE
		| Pixel::GBRP10LE
		| Pixel::GBRP12BE
		| Pixel::GBRP12LE
		| Pixel::GBRP14BE
		| Pixel::GBRP14LE
		| Pixel::GBRP16BE
		| Pixel::GBRP16LE => PixelFormat::U16,
		_ => PixelFormat::U8,
	}
}

/// Build an ffmpeg channel layout from a legacy ffmpeg channel mask.
///
/// Dipped into `ffmpeg-sys-next`: `ffmpeg-next` 9 exposes `ChannelLayout`
/// as a wrapper around `AVChannelLayout` (no mask constructor).
fn channel_layout_from_mask(mask: u64) -> ChannelLayout {
	if mask == 0 {
		return ChannelLayout::default(2);
	}
	let channels = mask.count_ones() as i32;
	ChannelLayout(sys::AVChannelLayout {
		order: sys::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
		nb_channels: channels,
		u: sys::AVChannelLayout__bindgen_ty_1 { mask },
		opaque: std::ptr::null_mut(),
	})
}

/// Convert a raw `AVSampleFormat` discriminant (as stored in
/// `AVCodecParameters.format`) into an ffmpeg [`Sample`].
///
/// Dipped into `ffmpeg-sys-next`: bindgen emits the C enum as a `#[repr(i32)]`
/// Rust enum, which cannot be built from an `i32` with an `as` cast.
fn sample_from_raw(v: i32) -> Sample {
	// SAFETY: `v` is always a discriminant FFmpeg itself produced; the
	// `repr(i32)` C enum has the same size and layout as `i32`.
	Sample::from(unsafe { std::mem::transmute::<i32, sys::AVSampleFormat>(v) })
}

/// Convert a raw `AVPixelFormat` discriminant (as stored in
/// `AVCodecParameters.format`) into an ffmpeg [`Pixel`].
///
/// Dipped into `ffmpeg-sys-next` (see [`sample_from_raw`]).
fn pixel_from_raw(v: i32) -> Pixel {
	// SAFETY: see `sample_from_raw`.
	Pixel::from(unsafe { std::mem::transmute::<i32, sys::AVPixelFormat>(v) })
}

/// The oakcore timebase equivalent of an ffmpeg rational.
fn oak_rational(r: FfRational) -> Rational {
	Rational::new(r.0 as i64, r.1 as i64)
}

// ---------------------------------------------------------------------------
// FFmpegDecoder
// ---------------------------------------------------------------------------

/// `olive::FFmpegDecoder` — FFmpeg-backed media decoder.
pub struct FFmpegDecoder {
	/// Opened stream (locked).
	stream: std::sync::Mutex<Option<CodecStream>>,
	/// The open FFmpeg decode session, if any.
	state: Mutex<Option<DecoderState>>,
}

impl FFmpegDecoder {
	/// New, closed decoder.
	pub fn new() -> Self {
		FFmpegDecoder {
			stream: std::sync::Mutex::new(None),
			state: Mutex::new(None),
		}
	}

	/// The hardware device driving this session (`None` = software
	/// decoding), as a display name (`videotoolbox` / `vaapi` /
	/// `cuda/nvdec` / `d3d11va`). An observability hook for the
	/// hardware-decode config and the first-frame fallback — tests
	/// assert on it to prove the hardware path is really taken (and
	/// really abandoned on fallback).
	pub fn hw_decoder_name(&self) -> Option<String> {
		let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state
			.as_ref()
			.and_then(|s| s.hw_device.map(crate::hwdecode::device_type_name))
			.map(|name| name.to_string())
	}

	/// Test-only: how many `seek()` calls the open session has performed.
	/// Contiguous audio chunks must not increase this (they continue the
	/// decode without re-seeking); a non-contiguous chunk must.
	#[cfg(test)]
	pub(crate) fn audio_seek_count(&self) -> u64 {
		let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.as_ref().map(|s| s.audio_seeks).unwrap_or(0)
	}
}

impl Default for FFmpegDecoder {
	fn default() -> Self {
		Self::new()
	}
}

impl Decoder for FFmpegDecoder {
	fn id(&self) -> String {
		"ffmpeg".to_string()
	}

	fn supports_video(&self) -> bool {
		true
	}

	fn supports_audio(&self) -> bool {
		true
	}

	fn probe(
		&self,
		filename: &str,
		cancelled: Option<&CancelAtom>,
	) -> Option<FootageDescription> {
		ffmpeg_init().ok()?;
		probe_file(filename, cancelled)
	}

	fn open(&self, stream: &CodecStream) -> crate::error::Result<()> {
		ffmpeg_init()?;

		// # CPP-PARITY
		// `src/codec/src/decoder.cpp` `Decoder::open`: already-open decoders
		// accept the same stream again, reject a different one; on failure the
		// stream is unset again.
		let mut stream_guard = self.stream.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(cur) = stream_guard.as_ref() {
			if cur == stream {
				return Ok(());
			}
			return Err(crate::error::Error::State);
		}
		if !stream.is_valid() {
			return Err(crate::error::Error::Invalid);
		}
		if !stream.exists() {
			return Err(crate::error::Error::NotFound);
		}

		let opened = DecoderState::open(stream)?;
		*self.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(opened);
		*stream_guard = Some(stream.clone());
		Ok(())
	}

	fn close(&self) -> crate::error::Result<()> {
		// C++ `Decoder::close` is void and safe when closed.
		*self.stream.lock().unwrap_or_else(|e| e.into_inner()) = None;
		*self.state.lock().unwrap_or_else(|e| e.into_inner()) = None;
		Ok(())
	}

	fn stream(&self) -> CodecStream {
		self.stream
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.unwrap_or_else(CodecStream::new)
	}

	fn hardware_decoding(&self) -> bool {
		let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.as_ref().is_some_and(|s| s.hw_device.is_some())
	}

	fn retrieve_video_frame(&self, p: &RetrieveVideoParams) -> crate::error::Result<Arc<Frame>> {
		ffmpeg_init()?;
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().ok_or(crate::error::Error::State)?;
		if !matches!(state.inner, DecoderInner::Video(_)) {
			return Err(fail("decoder is not open on a video stream"));
		}

		// # CPP-PARITY
		// `retrieve_video_frame_internal` (ffmpegdecoder.cpp): the decoded
		// frame is color-range-forced, scaled and copied into a `Frame` at
		// the requested timestamp.
		// Note: the Rust `RetrieveVideoParams` carries no cancellation atom
		// (dropped from the C++ struct), so decoding is not cancellable here.
		let mut decoded = state.retrieve_frame(&p.time, p.time == crate::decoder::k_any_timecode(), None);
		if decoded.is_err() && state.hw_device.is_some() {
			// First-frame hardware fallback: the hardware decoder opened
			// but cannot actually decode this stream (unsupported profile
			// / driver issue at decode time). Reopen as software and retry
			// once — this is the automatic fallback path; the config
			// switch is the manual one.
			state.reopen_software()?;
			decoded = state.retrieve_frame(&p.time, p.time == crate::decoder::k_any_timecode(), None);
		}
		let f = decoded?
			.ok_or_else(|| fail("no video frame available at the requested time"))?;

		let (w, h, bytes, color_meta) =
			state.scale_video_to_f32(f, p.force_range, p.target_size)?;
		let mut frame = copy_rgba_f32_to_frame(w, h, &bytes, p.time)?;
		// Carry the source colorimetry on the frame params: the render
		// layer maps it to its input transform (source → working space).
		if let Some(params) = frame.params.as_mut() {
			params.set_color_primaries(color_meta.color_primaries);
			params.set_color_transfer(color_meta.color_trc);
			params.set_color_range(if color_meta.full_range {
				oak_core::videoparams::ColorRange::Full
			} else {
				oak_core::videoparams::ColorRange::Limited
			});
		}
		Ok(Arc::new(frame))
	}

	fn retrieve_video(&self, p: &RetrieveVideoParams) -> crate::error::Result<OakRenderTexture> {
		// The Rust `RetrieveVideoParams` carries no `OakRenderRenderer`, so
		// texture creation cannot be performed — the C++ failure path returns
		// an empty texture (ffmpegdecoder.cpp `process_frame_into_texture`).
		ffmpeg_init()?;
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().ok_or(crate::error::Error::State)?;
		if !matches!(state.inner, DecoderInner::Video(_)) {
			return Err(fail("decoder is not open on a video stream"));
		}
		let _ = state.retrieve_frame(&p.time, p.time == crate::decoder::k_any_timecode(), None)?;
		Ok(OakRenderTexture)
	}

	fn retrieve_audio(
		&self,
		dest: &mut [f32],
		range: &TimeRange,
		sample_rate: i32,
		channel_layout: u64,
	) -> crate::error::Result<RetrieveAudioStatus> {
		ffmpeg_init()?;
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().ok_or(crate::error::Error::State)?;
		if !matches!(state.inner, DecoderInner::Audio(_)) {
			// The stream does not support audio.
			return Ok(RetrieveAudioStatus::Unsupported);
		}
		if sample_rate <= 0 || channel_layout == 0 {
			return Ok(RetrieveAudioStatus::Unsupported);
		}
		state.retrieve_audio_to(dest, range, sample_rate, channel_layout)
	}

	fn conform_audio(
		&self,
		output_filenames: &[String],
		sample_rate: i32,
		channel_layout: u64,
		sample_format: i32,
		cancelled: Option<&CancelAtom>,
	) -> crate::error::Result<()> {
		ffmpeg_init()?;
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		let state = state.as_mut().ok_or(crate::error::Error::State)?;
		if !matches!(state.inner, DecoderInner::Audio(_)) {
			return Err(fail("decoder is not open on an audio stream"));
		}
		state.conform_audio_to(
			output_filenames,
			sample_rate,
			channel_layout,
			sample_format,
			cancelled,
		)
	}

	fn get_audio_start_offset(&self) -> Rational {
		// # CPP-PARITY
		// `FFmpegDecoder::get_audio_start_offset`: format start minus stream
		// start, in seconds.
		let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		match state.as_ref() {
			Some(s) if s.format_start_time != AV_NOPTS_VALUE => {
				let fmt_start = Rational::new(s.format_start_time, FB_TIME_BASE);
				let str_start =
					oak_rational(s.stream_time_base).timestamp_to_time(s.stream_start_time);
				fmt_start - str_start
			}
			_ => Rational::new(0, 1),
		}
	}
}

/// A decoded frame plus its media type.
enum DecodedFrame {
	Video(ffmpeg::frame::Video),
	Audio(ffmpeg::frame::Audio),
}

/// Outcome of a single decoder receive attempt.
enum Pull {
	Frame(DecodedFrame),
	Eof,
	Eagain,
}

/// Whether feeding one packet advanced the decode.
enum PacketFeed {
	Sent,
	InputEof,
}

/// One opened (filename, stream) decode session.
struct DecoderState {
	input: ffmpeg::format::context::Input,
	/// The media file (kept for the hardware-decode fallback reopen).
	filename: String,
	stream_index: usize,
	inner: DecoderInner,
	/// The hardware device in use (`None` = software decoding). Kept so
	/// a hardware surface can be detected and the first-frame fallback
	/// can reopen the session as software.
	hw_device: Option<sys::AVHWDeviceType>,
	stream_time_base: FfRational,
	stream_start_time: i64,
	/// Format start time in microseconds (`AV_TIME_BASE`).
	format_start_time: i64,
	input_sample_format: Sample,
	input_sample_rate: u32,
	input_channel_layout_mask: u64,
	/// True once the decoder has been drained.
	eof: bool,
	video: Option<VideoDecodeState>,
	audio: Option<AudioDecodeState>,
	/// Test-only counter of `seek()` calls (proves contiguous audio chunks
	/// skip the seek).
	#[cfg(test)]
	audio_seeks: u64,
}

enum DecoderInner {
	Video(ffmpeg::codec::decoder::Video),
	Audio(ffmpeg::codec::decoder::Audio),
}

// SAFETY: `DecoderState` owns raw FFmpeg contexts that are not thread-safe
// themselves, but every access goes through the `FFmpegDecoder::state`
// `Mutex` (the C++ decoder uses the same lock-discipline around its bridge
// instance), so concurrent access never happens.
unsafe impl Send for DecoderState {}
unsafe impl Sync for DecoderState {}

/// Video decode state: the frame cache plus a cached swscale context.
struct VideoDecodeState {
	scaler: Option<ScalingCache>,
	cache: VecDeque<ffmpeg::frame::Video>,
	cache_at_zero: bool,
	cache_at_eof: bool,
	/// One second in the stream's time base.
	second_ts: i64,
	/// Whether the EOF-fallback warning has been printed for this session
	/// (the warning fires once, when the last cached frame is returned with
	/// a PTS far from the requested one).
	eof_fallback_warned: bool,
}

/// Cached swscale context (recreated when any dimension/format changes).
struct ScalingCache {
	ctx: scaling::Context,
	src_format: Pixel,
	src_width: u32,
	src_height: u32,
	dst_format: Pixel,
	dst_width: u32,
	dst_height: u32,
}

/// Audio decode state: a resampler keyed by (rate, layout).
struct AudioDecodeState {
	resampler: Option<(u32, u64, AudioResampler)>,
	/// The first sample of the next contiguous chunk: the sample index
	/// (at the current `sample_rate`) one past the last decoded chunk.
	/// Set on every successful `retrieve_audio_to`; cleared on every seek
	/// (any seek resets the decoder and resampler, breaking continuity).
	/// When the next chunk's start sample equals this, the decode can
	/// continue without re-seeking — skipping the per-chunk decoder flush
	/// and resampler reset that cause boundary artifacts (pops/clicks).
	contiguous_end_sample: Option<i64>,
	/// Tail of the last decoded codec frame that fell PAST the previous
	/// chunk's end, interleaved in the chunk's destination format;
	/// `carry_start` is the output-sample index `carry[0]` belongs to and
	/// `carry_format` the `(sample_rate, channel_layout)` it was rendered
	/// for. The decoder consumes whole codec frames, so without this
	/// carry the overflow (up to one codec frame — 1024 samples for AAC)
	/// would be lost and every chunk whose end does not align with the
	/// codec frame grid would start with a hole (a periodic stutter).
	/// Served at the start of the next contiguous chunk; cleared on
	/// seek, on format change, and on chunk failure.
	carry: Vec<f32>,
	carry_start: i64,
	carry_format: (u32, u64),
}

/// The frame-thread count handed to every FFmpeg decoder's `threads`
/// option. `auto` lets libavcodec spawn one decode thread per logical
/// core (16 observed on a 24-core box) PER DECODER; with ~`cores - 2`
/// render workers decoding concurrently that oversubscribes the machine
/// several times over and starves the playback-audio thread (late chunks
/// are dropped -> pops). 4 threads decode 1080p h264 far past real-time --
/// preview throughput comes from the worker pool, not from per-decoder
/// threading.
pub(crate) fn decoder_threads() -> String {
	let cores = std::thread::available_parallelism()
		.map(|n| n.get())
		.unwrap_or(4);
	cores.min(4).to_string()
}

/// A swresample conversion context.
struct AudioResampler {
	ctx: resampling::Context,
	dst_format: Sample,
	dst_layout: ChannelLayout,
	dst_rate: u32,
	dst_channels: usize,
}

impl DecoderState {
	/// Open `(filename, stream_index)` for decoding.
	fn open(stream: &CodecStream) -> crate::error::Result<DecoderState> {
		Self::open_impl(stream, true)
	}

	/// Open with the hardware-decode preference explicitly on/off (the
	/// off path is the hardware fallback's software reopen).
	fn open_impl(stream: &CodecStream, allow_hw: bool) -> crate::error::Result<DecoderState> {
		let mut dict = Dictionary::new();
		dict.set("analyzeduration", "5000000");
		dict.set("probesize", "20000000");
		let input =
			ffmpeg::format::input_with_dictionary(&stream.filename(), dict).map_err(ffmpeg_err)?;

		let stream_index = stream.stream() as usize;
		let fstream = input
			.stream(stream_index)
			.ok_or_else(|| fail(format!("no stream {}", stream.stream())))?;
		let params = fstream.parameters();
		let medium = params.medium();
		let codec_id = params.id();

		// Hardware decode is the mandated default: on video streams open
		// the regular decoder with the platform's hardware device
		// attached (the FFmpeg 8 hwaccel model: VideoToolbox / VA-API /
		// NVDEC / D3D11VA, config-gated); pure software is the fallback
		// when no hardware device is available.
		let mut hw_device: Option<sys::AVHWDeviceType> = None;
		let hw_opened = if allow_hw
			&& matches!(medium, MediaType::Video)
			&& crate::hwdecode::hardware_decoding_enabled()
		{
			ffmpeg::decoder::find(codec_id).and_then(|codec| {
				crate::hwdecode::device_type_candidates()
					.iter()
					.find_map(|device_type| {
						crate::hwdecode::open_hw_accel(&params, codec, *device_type).map(
							|(opened, device_type)| {
								hw_device = Some(device_type);
								opened
							},
						)
					})
			})
		} else {
			None
		};

		// Per-medium stream parameters (mirroring the bridge stream info);
		// read before `params` is moved into the codec context below.
		let mut input_sample_format = Sample::None;
		let mut input_sample_rate = 0;
		let mut input_channel_layout_mask = 0u64;
		if matches!(medium, MediaType::Audio) {
			let raw = unsafe { params.as_ptr() };
			input_sample_format = sample_from_raw(unsafe { (*raw).format });
			input_sample_rate = unsafe { (*raw).sample_rate }.max(0) as u32;
			input_channel_layout_mask = unsafe { ChannelLayout::from((*raw).ch_layout) }.bits();
		}

		let opened = match hw_opened {
			Some(opened) => opened,
			None => {
				let codec = ffmpeg::decoder::find(codec_id)
					.ok_or_else(|| fail(format!("no decoder for codec {codec_id:?}")))?;
				let mut open_opts = Dictionary::new();
				open_opts.set("threads", &decoder_threads());
				ffmpeg::codec::Context::from_parameters(params)
					.map_err(ffmpeg_err)?
					.decoder()
					.open_as_with(codec, open_opts)
					.map_err(ffmpeg_err)?
			}
		};

		let inner = match medium {
			MediaType::Video => DecoderInner::Video(ffmpeg::codec::decoder::Video(opened)),
			MediaType::Audio => DecoderInner::Audio(ffmpeg::codec::decoder::Audio(opened)),
			other => {
				return Err(fail(format!(
					"stream {} is {:?}, expected video or audio",
					stream.stream(),
					other
				)))
			}
		};

		let stream_time_base = fstream.time_base();
		let stream_start_time = fstream.start_time();
		let format_start_time = unsafe { (*input.as_ptr()).start_time };

		// One second in the stream's time base (round of den/num).
		let second_ts = (stream_time_base.1 as f64 / stream_time_base.0 as f64).round() as i64;
		let video = if matches!(medium, MediaType::Video) {
			Some(VideoDecodeState {
				scaler: None,
				cache: VecDeque::new(),
				cache_at_zero: false,
				cache_at_eof: false,
				second_ts,
				eof_fallback_warned: false,
			})
		} else {
			None
		};
		let audio = if matches!(medium, MediaType::Audio) {
			Some(AudioDecodeState {
				resampler: None,
				contiguous_end_sample: None,
				carry: Vec::new(),
				carry_start: 0,
				carry_format: (0, 0),
			})
		} else {
			None
		};

		Ok(DecoderState {
			input,
			filename: stream.filename().to_string(),
			stream_index,
			inner,
			hw_device,
			stream_time_base,
			stream_start_time,
			format_start_time,
			input_sample_format,
			input_sample_rate,
			input_channel_layout_mask,
			eof: false,
			video,
			audio,
			#[cfg(test)]
			audio_seeks: 0,
		})
	}

	/// Reopen the session as a pure software decode (the hardware decoder
	/// opened but cannot decode this stream). Swaps the state in place;
	/// the retry re-seeks to the requested frame on the fresh session.
	fn reopen_software(&mut self) -> crate::error::Result<()> {
		let stream = CodecStream::with_block(self.filename.clone(), self.stream_index as i32, None);
		let mut fresh = Self::open_impl(&stream, false)?;
		std::mem::swap(self, &mut fresh);
		Ok(())
	}

	fn send_packet(&mut self, packet: &ffmpeg::packet::Packet) -> crate::error::Result<()> {
		match &mut self.inner {
			DecoderInner::Video(d) => d.send_packet(packet).map_err(ffmpeg_err),
			DecoderInner::Audio(d) => d.send_packet(packet).map_err(ffmpeg_err),
		}
	}

	fn send_eof(&mut self) -> crate::error::Result<()> {
		match &mut self.inner {
			DecoderInner::Video(d) => d.send_eof().map_err(ffmpeg_err),
			DecoderInner::Audio(d) => d.send_eof().map_err(ffmpeg_err),
		}
	}

	/// One receive attempt on the decoder.
	fn pull(&mut self) -> crate::error::Result<Pull> {
		let result = match &mut self.inner {
			DecoderInner::Video(d) => {
				let mut scratch = ffmpeg::frame::Video::empty();
				d.receive_frame(&mut scratch).map(|_| {
					Pull::Frame(DecodedFrame::Video(std::mem::replace(
						&mut scratch,
						ffmpeg::frame::Video::empty(),
					)))
				})
			}
			DecoderInner::Audio(d) => {
				let mut scratch = ffmpeg::frame::Audio::empty();
				d.receive_frame(&mut scratch).map(|_| {
					Pull::Frame(DecodedFrame::Audio(std::mem::replace(
						&mut scratch,
						ffmpeg::frame::Audio::empty(),
					)))
				})
			}
		};
		match result {
			Ok(p) => Ok(p),
			Err(FfmpegError::Eof) => Ok(Pull::Eof),
			Err(FfmpegError::Other { errno }) if errno == ffmpeg::error::EAGAIN => Ok(Pull::Eagain),
			Err(e) => Err(ffmpeg_err(e)),
		}
	}

	/// Feed the next packet of the opened stream into the decoder.
	fn feed_packet(&mut self) -> crate::error::Result<PacketFeed> {
		let mut pkt = ffmpeg::packet::Packet::empty();
		match pkt.read(&mut self.input) {
			Ok(()) => {
				if pkt.stream() == self.stream_index {
					self.send_packet(&pkt)?;
				}
				Ok(PacketFeed::Sent)
			}
			Err(FfmpegError::Eof) => {
				self.send_eof()?;
				Ok(PacketFeed::InputEof)
			}
			Err(e) => Err(ffmpeg_err(e)),
		}
	}

	/// Pull the next decoded frame, feeding packets as needed. Returns
	/// `Ok(None)` once the decoder is drained.
	fn next_frame(&mut self) -> crate::error::Result<Option<DecodedFrame>> {
		if self.eof {
			return Ok(None);
		}
		loop {
			match self.pull()? {
				Pull::Frame(f) => return Ok(Some(f)),
				Pull::Eof => {
					self.eof = true;
					return Ok(None);
				}
				Pull::Eagain => {
					self.feed_packet()?;
				}
			}
		}
	}

	/// Seek to `timestamp` (in the stream's time base) and flush the decoder.
	///
	/// # CPP-PARITY
	/// `fb_decoder_seek` semantics (backward seek on the stream, decoder
	/// flushed afterwards).
	fn seek(&mut self, timestamp: i64) -> crate::error::Result<()> {
		let stream_tb = self
			.input
			.stream(self.stream_index)
			.map(|s| s.time_base())
			.unwrap_or(FfRational(1, 1));
		let target =
			unsafe { sys::av_rescale_q(timestamp, self.stream_time_base.into(), stream_tb.into()) };
		let ret = unsafe {
			sys::av_seek_frame(
				self.input.as_mut_ptr(),
				self.stream_index as i32,
				target,
				sys::AVSEEK_FLAG_BACKWARD,
			)
		};
		if ret < 0 {
			return Err(ffmpeg_err(FfmpegError::from(ret)));
		}
		match &mut self.inner {
			DecoderInner::Video(d) => d.flush(),
			DecoderInner::Audio(d) => d.flush(),
		}
		#[cfg(test)]
		{
			self.audio_seeks += 1;
		}
		self.eof = false;
		// Any seek breaks audio continuity (decoder flush + resampler
		// reset): the next audio chunk must not skip its own seek, the
		// carried overflow belongs to the pre-seek position, and the
		// cached resampler's internal state is stale — drop it so a
		// post-seek decode is identical to a fresh one (reusing it
		// measurably shifted the output).
		if let Some(a) = &mut self.audio {
			a.contiguous_end_sample = None;
			a.carry.clear();
			a.resampler = None;
		}
		Ok(())
	}

	/// The smallest positive PTS gap between consecutive cached frames, in
	/// the stream's time base. `None` when it cannot be derived (fewer than
	/// two frames, or all frames share a PTS).
	fn frame_interval_ts(video: &VideoDecodeState) -> Option<i64> {
		let mut min: Option<i64> = None;
		for pair in video.cache.iter().zip(video.cache.iter().skip(1)) {
			let a = pair.0.pts().unwrap_or(AV_NOPTS_VALUE);
			let b = pair.1.pts().unwrap_or(AV_NOPTS_VALUE);
			if a != AV_NOPTS_VALUE && b != AV_NOPTS_VALUE {
				let gap = (b - a).abs();
				if gap > 0 {
					min = Some(min.map_or(gap, |m: i64| m.min(gap)));
				}
			}
		}
		min
	}

	/// Retrieve the video frame at (or before) `time`, mirroring the C++
	/// `retrieve_frame` seek/cache logic.
	fn retrieve_frame(
		&mut self,
		time: &Rational,
		any_timecode: bool,
		cancelled: Option<&CancelAtom>,
	) -> crate::error::Result<Option<ffmpeg::frame::Video>> {
		// Move the video state out so `self.seek` / `self.pull` (which touch
		// other fields) can be called without conflicting borrows.
		let mut video = self
			.video
			.take()
			.expect("retrieve_frame requires a video session");

		let mut target_ts = oak_rational(self.stream_time_base).time_to_timestamp(*time);
		if self.format_start_time != AV_NOPTS_VALUE {
			target_ts += unsafe {
				sys::av_rescale_q(
					self.format_start_time,
					FfRational(1, FB_TIME_BASE as i32).into(),
					self.stream_time_base.into(),
				)
			};
		}

		const MIN_SEEK: i64 = 0;
		let mut seek_ts = (target_ts - maximum_queue_size() as i64).max(MIN_SEEK);
		let mut still_seeking = false;

		if !any_timecode {
			// If the frame wasn't in the frame cache, see if this cache is
			// too old to use (CPP-PARITY ffmpegdecoder.cpp:975).
			if video.cache.is_empty()
				|| target_ts < pts_of(video.cache.front()).unwrap_or(AV_NOPTS_VALUE)
				|| target_ts
					> pts_of(video.cache.back()).unwrap_or(AV_NOPTS_VALUE) + 2 * video.second_ts
			{
				video.cache.clear();
				video.cache_at_eof = false;

				self.seek(seek_ts)?;
				if seek_ts == MIN_SEEK {
					video.cache_at_zero = true;
				}

				still_seeking = true;
			} else if let Some(cached) = get_frame_from_cache(&video, target_ts) {
				self.video = Some(video);
				return Ok(Some(cached));
			}
		}

		let mut retried_after_eof = false;
		let mut return_frame: Option<ffmpeg::frame::Video> = None;

		loop {
			if cancel_atom_is_cancelled(cancelled) {
				break;
			}

			let frame = match self.pull()? {
				Pull::Frame(DecodedFrame::Video(f)) => {
					// A hardware decoder yields hardware surfaces
					// (AV_PIX_FMT_VIDEOTOOLBOX/VAAPI/CUDA/D3D11*):
					// transfer to system memory so the cache and swscale
					// only ever see CPU frames.
					// SAFETY: plain read of the frame's format field.
					let raw_format = unsafe { (*f.as_ptr()).format };
					if self.hw_device.is_some()
						&& crate::hwdecode::is_hw_format(unsafe {
							std::mem::transmute::<i32, sys::AVPixelFormat>(raw_format)
						}) {
						crate::hwdecode::transfer_to_cpu(&f)?
					} else {
						f
					}
				}
				Pull::Frame(_) => unreachable!("video session yields only video frames"),
				Pull::Eof => {
					// Handle an "expected" EOF by using the last cached frame
					// (CPP-PARITY ffmpegdecoder.cpp:1043).
					video.cache_at_eof = true;
					if video.cache.is_empty() {
						if !retried_after_eof {
							retried_after_eof = true;
							video.cache.clear();
							self.seek(MIN_SEEK)?;
							video.cache_at_zero = true;
							still_seeking = true;
							continue;
						}
						self.video = Some(video);
						return Err(fail("unexpected codec EOF - unable to retrieve frame"));
					}
					return_frame = video.cache.back().cloned();
					// The returned frame is the last cached one, which may
					// sit far from the requested timestamp (e.g. a corrupt
					// or truncated file whose frames all decode to the
					// start). Warn once per session with the actual offsets
					// so the mismatch is diagnosable; skip files whose
					// frames carry no usable PTS spacing (still images).
					if !video.eof_fallback_warned {
						video.eof_fallback_warned = true;
						if let (Some(frame), Some(gap)) =
							(return_frame.as_ref(), Self::frame_interval_ts(&video))
						{
							let got = frame.pts().unwrap_or(AV_NOPTS_VALUE);
							if got != AV_NOPTS_VALUE && (got - target_ts).abs() > gap {
								eprintln!(
									"oak-codec: EOF fallback in '{}': target {} got {} (frame gap {})",
									self.filename, target_ts, got, gap
								);
							}
						}
					}
					break;
				}
				Pull::Eagain => {
					self.feed_packet()?;
					continue;
				}
			};

			if cancel_atom_is_cancelled(cancelled) {
				break;
			}

			let frame_pts = frame.pts().unwrap_or(AV_NOPTS_VALUE);

			if still_seeking {
				// Handle a failed seek: step back by one second and retry
				// (CPP-PARITY ffmpegdecoder.cpp:1028).
				if !video.cache_at_zero && frame_pts > target_ts {
					seek_ts = (seek_ts - video.second_ts).max(MIN_SEEK);
					self.seek(seek_ts)?;
					if seek_ts == MIN_SEEK {
						video.cache_at_zero = true;
					}
					continue;
				}
				still_seeking = false;
			}

			// Cut the cache down to `maximum_queue_size` before pushing
			// (CPP-PARITY ffmpegdecoder.cpp:1065).
			if video.cache.len() > maximum_queue_size() {
				video.cache.pop_front();
				video.cache_at_zero = false;
			}

			let previous = video.cache.back().cloned();
			video.cache.push_back(frame);

			if frame_pts == target_ts || any_timecode {
				return_frame = video.cache.back().cloned();
				break;
			} else if frame_pts > target_ts {
				if previous.is_none() && video.cache_at_zero {
					return_frame = video.cache.back().cloned();
				} else {
					return_frame = previous;
				}
				break;
			}
		}

		self.video = Some(video);
		Ok(return_frame)
	}

	/// Scale a decoded frame to float RGBA (F32, 4 channels), returning the
	/// raw pixel bytes, dimensions, and the source colorimetry. Mirrors
	/// `pre_process_frame` + `retrieve_video_frame_internal` scaling; unlike
	/// the old bridge path the YUV→RGB honors the frame's own colorspace
	/// (BT.601/709/2020) and range instead of assuming BT.601 limited.
	/// `target_size` resizes in the same swscale pass (native → RGBA/F32 at
	/// the target size) instead of converting at native size first — the
	/// caller's downscale then degenerates to a plain copy, and no
	/// full-resolution float intermediate (~132 MB at 4K) ever exists.
	fn scale_video_to_f32(
		&mut self,
		f: ffmpeg::frame::Video,
		force_range: i32,
		target_size: Option<(u32, u32)>,
	) -> crate::error::Result<(u32, u32, Vec<u8>, crate::decoder::DecodedColorMeta)> {
		let video = self
			.video
			.as_mut()
			.expect("scale_video_to_f32 requires a video session");

		// The frame's own colorimetry (set by the decoder from the
		// bitstream); raw code points pass through to the render layer.
		let (raw_primaries, raw_trc, raw_space, raw_range) = unsafe {
			let av = f.as_ptr();
			(
				(*av).color_primaries as i32,
				(*av).color_trc as i32,
				(*av).colorspace as i32,
				(*av).color_range as i32,
			)
		};

		// # CPP-PARITY ffmpegdecoder.cpp:376: disregard "JPEG" pixel formats
		// — but a YUVJ source is full range by definition, so remember it
		// for the range decision below.
		let orig_format = f.format();
		let src_format = convert_jpeg_space_to_regular_space(orig_format);
		let yuvj_full = orig_format != src_format;
		let mut f = f;
		f.set_format(src_format);

		// The effective color range: the caller's force wins; otherwise the
		// frame's own metadata (YUVJ sources are full range). The old path
		// forced MPEG/limited for everything, crushing full-range screen
		// captures and JPEG-derived footage.
		let full_range = if force_range == oak_core_COLOR_RANGE_FULL {
			true
		} else if force_range == oak_core_COLOR_RANGE_LIMITED {
			false
		} else {
			yuvj_full || raw_range == AVCOL_RANGE_JPEG
		};
		f.set_color_range(if full_range {
			ffmpeg::color::Range::JPEG
		} else {
			ffmpeg::color::Range::MPEG
		});

		let (src_w, src_h) = (f.width(), f.height());
		// 目标尺寸（None = 原生；0 边回退原生，swscale 不接受 0）。
		let (w, h) = match target_size {
			Some((tw, th)) if tw > 0 && th > 0 => (tw, th),
			_ => (src_w, src_h),
		};

		// swscale cannot reliably output float RGBA on every build:
		// float output is missing from some static FFmpeg swscale
		// builds ("128bpp not supported by yuv2rgb"), and creating a
		// float context there can abort instead of erroring — RGBA64 is
		// REPORTED supported but still aborts, so only RGBAF32LE is
		// probed (on the builds that have it, e.g. the system FFmpeg,
		// it works). High-bit-depth YUV sources fall back to 16-bit
		// planar YUV 4:4:4 (converted to F32 RGBA in Rust) so their
		// precision survives; 8-bit and RGB sources take the universal
		// 8-bit RGBA path.
		let supported = ffmpeg::software::scaling::support::output(Pixel::RGBAF32LE);
		let (depth, is_yuv) = pix_fmt_depth_and_yuv(src_format);
		let (out_fmt, f32_ok) = if supported {
			(Pixel::RGBAF32LE, true)
		} else if depth > 8 && is_yuv {
			(Pixel::YUV444P16LE, false)
		} else {
			(Pixel::RGBA, false)
		};
		let ctx = get_or_create_scaler(&mut video.scaler, src_format, src_w, src_h, out_fmt, w, h)?;
		if out_fmt == Pixel::YUV444P16LE {
			// YUV→YUV pass-through: the 16-bit code values must reach the
			// Rust matrix conversion bit-exact. sws_setColorspaceDetails
			// has to see the SAME coefficient table for source and
			// destination — differing tables would insert a cascaded
			// YUV→RGB→YUV round trip — and both ranges are set full so
			// the YUV→YUV range recompression is skipped entirely (it
			// only runs when src_range != dst_range). The matrix and
			// full/limited expansion happen later, in
			// convert_yuv444p16_to_rgba_f32.
			unsafe {
				let table = sys::sws_getCoefficients(sws_colorspace_for(raw_space, src_w, src_h));
				sys::sws_setColorspaceDetails(
					ctx.as_mut_ptr(),
					table,
					1, // src full range (no recompression)
					table,
					1, // dst full range (no recompression)
					0,
					1 << 16,
					1 << 16,
				);
			}
		} else {
			// The YUV→RGB matrix: BT.601/709/2020 per the frame's
			// colorspace tag, with the full/limited range decided above.
			// RGB sources are untouched by the colorspace tables (swscale
			// ignores them there).
			apply_sws_colorspace(ctx, raw_space, full_range, src_w, src_h);
		}
		let mut out = ffmpeg::frame::Video::empty();
		ctx.run(&f, &mut out).map_err(ffmpeg_err)?;
		let bytes = if f32_ok {
			let stride = out.stride(0);
			convert_rgba_f32_le(&out.data(0), w, h, stride)
		} else if out_fmt == Pixel::YUV444P16LE {
			convert_yuv444p16_to_rgba_f32(&out, w, h, yuv_matrix_for(raw_space, src_w, src_h), full_range)
		} else {
			let stride = out.stride(0);
			convert_rgba8_to_f32(&out.data(0), w, h, stride)
		};
		let meta = crate::decoder::DecodedColorMeta {
			color_primaries: raw_primaries,
			color_trc: raw_trc,
			full_range,
		};
		Ok((w, h, bytes, meta))
	}

	/// Fill `dest` (interleaved f32) with the decoded audio covering
	/// `range`, resampled to `sample_rate` / `channel_layout`. Samples
	/// outside the decoded media are left as silence.
	fn retrieve_audio_to(
		&mut self,
		dest: &mut [f32],
		range: &TimeRange,
		sample_rate: i32,
		channel_layout: u64,
	) -> crate::error::Result<RetrieveAudioStatus> {
		let dst_layout = channel_layout_from_mask(channel_layout);
		let dst_channels = dst_layout.channels().max(0) as usize;
		if dst_channels == 0 {
			return Ok(RetrieveAudioStatus::Unsupported);
		}

		let start_sec = range.in_().to_f64();
		let end_sec = range.out().to_f64();
		let start_sample = (start_sec * sample_rate as f64).round() as i64;
		let end_sample = (end_sec * sample_rate as f64).round() as i64;
		if end_sample <= start_sample {
			return Ok(RetrieveAudioStatus::Success);
		}

		dest.fill(0.0);

		// Continuity fast path: when this chunk's start sample equals the
		// sample one past the previous chunk's end, the decoder and
		// resampler are still positioned exactly there — continue without
		// seeking. Skipping the per-chunk seek avoids the decoder flush and
		// resampler reset whose boundary artifacts (silence gaps, phase
		// resets) were audible as pops/clicks at chunk edges. The caller's
		// anchored sample grid (`round(out·rate) − round(in·rate)`) makes
		// the next chunk's start exactly the previous chunk's recorded end,
		// so exact comparison is correct.
		let contiguous = self
			.audio
			.as_ref()
			.map(|a| a.contiguous_end_sample == Some(start_sample))
			.unwrap_or(false);
		if !contiguous {
			// Seek to just before the range start.
			let start_ts =
				oak_rational(self.stream_time_base).time_to_timestamp(Rational::from_double(start_sec));
			self.seek(start_ts)?;
		}

		// Serve the overflow tail carried over from the previous chunk:
		// it belongs exactly at this chunk's start (the anchored grid
		// makes positions exact). A stale or format-mismatched carry is
		// discarded (a pre-seek carry was already cleared by seek()).
		let mut carried_frames: i64 = 0;
		{
			let a = self.audio.as_mut().expect("audio session");
			let valid = contiguous
				&& a.carry_format == (sample_rate as u32, channel_layout)
				&& a.carry_start == start_sample;
			if valid && !a.carry.is_empty() {
				let dest_frames = dest.len() / dst_channels;
				let n_frames = (a.carry.len() / dst_channels).min(dest_frames);
				dest[..n_frames * dst_channels].copy_from_slice(&a.carry[..n_frames * dst_channels]);
				carried_frames = n_frames as i64;
				// A chunk shorter than the carry (degenerate grid) keeps
				// the remainder for the next chunk.
				a.carry.drain(..n_frames * dst_channels);
				a.carry_start += n_frames as i64;
			} else {
				a.carry.clear();
			}
		}

		// The fill+decode+flush body runs in a closure so the success path
		// can record the chunk's end sample (continuity) and an error path
		// can clear it (the decoder/resampler position is unknown after a
		// mid-chunk failure).
		let read = (|| -> crate::error::Result<RetrieveAudioStatus> {
			// Take the cached resampler out (or create one) so the decode loop
			// below can borrow `self` freely; it is put back before returning.
			let src_layout = channel_layout_from_mask(self.input_channel_layout_mask);
			let mut resampler = match self.audio.as_mut().expect("audio session").resampler.take() {
				Some((rate, layout, rs)) if rate == sample_rate as u32 && layout == channel_layout => {
					rs
				}
				_ => AudioResampler::get(
					self.input_sample_format,
					src_layout,
					self.input_sample_rate,
					Sample::F32(SampleType::Packed),
					dst_layout,
					sample_rate as u32,
				)?,
			};
			let stream_time_base = self.stream_time_base;

			let mut next_sample: Option<i64> = if carried_frames > 0 {
				Some(start_sample + carried_frames)
			} else {
				None
			};
			let dest_frames = (dest.len() / dst_channels) as i64;
			// Overflow past this chunk's end (see AudioDecodeState::carry):
			// the decoder consumes whole codec frames, so the tail of the
			// last frame crossing the chunk end would otherwise be lost
			// and the next chunk would start with a hole.
			let mut new_carry: Vec<f32> = Vec::new();
			let mut new_carry_start: i64 = start_sample + dest_frames;

			while let Some(frame) = self.next_frame()? {
				let DecodedFrame::Audio(audio) = frame else {
					continue;
				};
				let converted = resample_to_interleaved_f32(&mut resampler, &audio)?;
				if converted.is_empty() {
					continue;
				}
				let chunk_samples = (converted.len() / dst_channels) as i64;
				let frame_start = match audio.pts() {
					Some(pts) => {
						let secs = oak_rational(stream_time_base)
							.timestamp_to_time(pts)
							.to_f64();
						(secs * sample_rate as f64).round() as i64
					}
					None => next_sample.unwrap_or(start_sample),
				};
				let offset = frame_start - start_sample;
				if offset < dest_frames && offset + chunk_samples > 0 {
					let copy_start = offset.max(0) as usize;
					let copy_end = (offset + chunk_samples).min(dest_frames).max(0) as usize;
					if copy_end > copy_start {
						let src_off = (copy_start as i64 - offset) as usize * dst_channels;
						let n = (copy_end - copy_start) * dst_channels;
						let dst_off = copy_start * dst_channels;
						if dst_off + n <= dest.len() {
							dest[dst_off..dst_off + n]
								.copy_from_slice(&converted[src_off..src_off + n]);
						}
					}
				}
				if offset + chunk_samples > dest_frames {
					let inside = (dest_frames - offset).clamp(0, chunk_samples) as usize;
					let pos = frame_start + inside as i64;
					if !new_carry.is_empty()
						&& pos != new_carry_start + (new_carry.len() / dst_channels) as i64
					{
						// Non-adjacent overflow (should not happen): keep
						// only the latest contiguous run.
						new_carry.clear();
						new_carry_start = pos;
					}
					if new_carry.is_empty() {
						new_carry_start = pos;
					}
					new_carry.extend_from_slice(&converted[inside * dst_channels..]);
				}
				next_sample = Some(frame_start + chunk_samples);
				if offset + chunk_samples >= dest_frames {
					break;
				}
			}

			// Put the resampler back into the cache for the next call.
			self.audio.as_mut().expect("audio session").resampler =
				Some((sample_rate as u32, channel_layout, resampler));

			// Flush any samples still buffered in the resampler (rate conversion
			// tail), appending after the last decoded sample.
			let resampler = self
				.audio
				.as_mut()
				.expect("audio session")
				.resampler
				.as_mut()
				.map(|r| &mut r.2)
				.unwrap();
			if let Some(flush) = flush_resampler_interleaved_f32(resampler)? {
				if !flush.is_empty() {
					let chunk_samples = (flush.len() / dst_channels) as i64;
					let frame_start = next_sample.unwrap_or(start_sample);
					let offset = frame_start - start_sample;
					if offset < dest_frames && offset + chunk_samples > 0 {
						let copy_start = offset.max(0) as usize;
						let copy_end = (offset + chunk_samples).min(dest_frames).max(0) as usize;
						if copy_end > copy_start {
							let src_off = (copy_start as i64 - offset) as usize * dst_channels;
							let n = (copy_end - copy_start) * dst_channels;
							let dst_off = copy_start * dst_channels;
							if dst_off + n <= dest.len() {
								dest[dst_off..dst_off + n]
									.copy_from_slice(&flush[src_off..src_off + n]);
							}
						}
					}
					if offset + chunk_samples > dest_frames {
						let inside = (dest_frames - offset).clamp(0, chunk_samples) as usize;
						let pos = frame_start + inside as i64;
						if !new_carry.is_empty()
							&& pos != new_carry_start + (new_carry.len() / dst_channels) as i64
						{
							new_carry.clear();
							new_carry_start = pos;
						}
						if new_carry.is_empty() {
							new_carry_start = pos;
						}
						new_carry.extend_from_slice(&flush[inside * dst_channels..]);
					}
				}
			}

			// Record the end sample: the next chunk starting exactly here
			// continues without a seek — and is served the carried overflow
			// first (see the chunk entry above).
			{
				let a = self.audio.as_mut().expect("audio session");
				a.carry = new_carry;
				a.carry_start = new_carry_start;
				a.carry_format = (sample_rate as u32, channel_layout);
				a.contiguous_end_sample = Some(start_sample + dest_frames);
			}

			Ok(RetrieveAudioStatus::Success)
		})();

		match read {
			Ok(status) => Ok(status),
			Err(e) => {
				// A failed chunk leaves the decoder/resampler position
				// unknown: force a fresh seek on the next chunk.
				if let Some(a) = &mut self.audio {
					a.contiguous_end_sample = None;
					a.carry.clear();
				}
				Err(e)
			}
		}
	}

	/// Conform the open stream's audio into per-channel PCM files, mirroring
	/// `FFmpegDecoder::conform_audio_internal`.
	fn conform_audio_to(
		&mut self,
		output_filenames: &[String],
		sample_rate: i32,
		channel_layout: u64,
		sample_format: i32,
		cancelled: Option<&CancelAtom>,
	) -> crate::error::Result<()> {
		if self.input_channel_layout_mask == 0 {
			return Err(fail(
				"could not determine the channel layout of the audio file",
			));
		}

		let target_fmt = crate::encodingparams::sample_format_from_i32(sample_format);
		let dst_sample = sample_format_to_ffmpeg(target_fmt)
			.ok_or_else(|| fail(format!("unsupported conform sample format {sample_format}")))?;
		let dst_layout = channel_layout_from_mask(channel_layout);
		let planar = dst_sample.is_planar();
		let planes = if planar {
			dst_layout.channels().max(0) as usize
		} else {
			1
		};
		if planes == 0 {
			return Err(fail("invalid conform channel layout"));
		}

		// Seek to the start (CPP-PARITY ffmpegdecoder.cpp:694).
		self.seek(0)?;

		let mut resampler = AudioResampler::get(
			self.input_sample_format,
			channel_layout_from_mask(self.input_channel_layout_mask),
			self.input_sample_rate,
			dst_sample,
			dst_layout,
			sample_rate as u32,
		)?;

		let filenames: Vec<PathBuf> = output_filenames.iter().map(PathBuf::from).collect();
		let mut wave_out = crate::planarfiledevice::PlanarFileDevice::new();
		if !wave_out.open(&filenames, crate::planarfiledevice::OpenMode::WriteOnly) {
			return Err(fail("failed to open conform output files"));
		}

		let bytes_per_sample = dst_sample.bytes();
		while let Some(frame) = self.next_frame()? {
			if cancel_atom_is_cancelled(cancelled) {
				break;
			}
			let DecodedFrame::Audio(audio) = frame else {
				continue;
			};
			let planes_buf = resampler.convert_to_planes(&audio)?;
			let written = planes_buf.first().map_or(0, Vec::len) / bytes_per_sample;
			if written > 0 {
				let refs: Vec<&[u8]> = planes_buf.iter().map(|p| p.as_slice()).collect();
				wave_out.write(&refs, written as i64 * bytes_per_sample as i64, 0);
			}
		}
		// Flush the resampler tail.
		for planes_buf in resampler.flush_planes()? {
			let written = planes_buf.first().map_or(0, Vec::len) / bytes_per_sample;
			if written > 0 {
				let refs: Vec<&[u8]> = planes_buf.iter().map(|p| p.as_slice()).collect();
				wave_out.write(&refs, written as i64 * bytes_per_sample as i64, 0);
			}
		}
		wave_out.close();

		if cancel_atom_is_cancelled(cancelled) {
			return Err(crate::error::Error::Cancelled);
		}
		Ok(())
	}
}

impl AudioResampler {
	/// Create a resampler converting from the source definition to the
	/// destination definition.
	fn get(
		src_format: Sample,
		src_layout: ChannelLayout,
		src_rate: u32,
		dst_format: Sample,
		dst_layout: ChannelLayout,
		dst_rate: u32,
	) -> crate::error::Result<AudioResampler> {
		let ctx = resampling::Context::get(
			src_format, src_layout, src_rate, dst_format, dst_layout, dst_rate,
		)
		.map_err(ffmpeg_err)?;
		Ok(AudioResampler {
			ctx,
			dst_format,
			dst_layout,
			dst_rate,
			dst_channels: dst_layout.channels().max(0) as usize,
		})
	}

	/// Convert one input frame, returning one byte buffer per output plane
	/// (plane 0 for packed destinations).
	fn convert_to_planes(
		&mut self,
		input: &ffmpeg::frame::Audio,
	) -> crate::error::Result<Vec<Vec<u8>>> {
		let (written, bufs) = self.swr_convert_buffers(input)?;
		let _ = written;
		Ok(bufs)
	}

	/// Convert one input frame into a freshly allocated output frame in the
	/// destination format, layout and rate.
	fn convert_to_frame(
		&mut self,
		input: &ffmpeg::frame::Audio,
	) -> crate::error::Result<ffmpeg::frame::Audio> {
		let (written, bufs) = self.swr_convert_buffers(input)?;
		let mut out = ffmpeg::frame::Audio::new(self.dst_format, written, self.dst_layout);
		out.set_rate(self.dst_rate);
		for (plane, buf) in bufs.iter().enumerate() {
			let len = buf.len().min(out.data_mut(plane).len());
			out.data_mut(plane)[..len].copy_from_slice(&buf[..len]);
		}
		Ok(out)
	}

	/// Flush any samples still buffered inside the resampler, as planes.
	fn flush_planes(&mut self) -> crate::error::Result<Vec<Vec<Vec<u8>>>> {
		let mut all: Vec<Vec<Vec<u8>>> = Vec::new();
		for _ in 0..32 {
			let out_samples = unsafe { sys::swr_get_out_samples(self.ctx.as_mut_ptr(), 0) };
			if out_samples <= 0 {
				break;
			}
			let (bufs, written) = self.swr_convert(out_samples as usize, None, 0)?;
			if written == 0 {
				break;
			}
			all.push(bufs);
		}
		Ok(all)
	}

	/// Run the swr conversion for `input`, returning the number of written
	/// samples and one byte buffer per output plane.
	fn swr_convert_buffers(
		&mut self,
		input: &ffmpeg::frame::Audio,
	) -> crate::error::Result<(usize, Vec<Vec<u8>>)> {
		let in_samples = input.samples() as i32;
		let out_samples = unsafe { sys::swr_get_out_samples(self.ctx.as_mut_ptr(), in_samples) };
		if out_samples < 0 {
			return Err(ffmpeg_err(FfmpegError::from(out_samples)));
		}
		if out_samples == 0 {
			return Ok((0, Vec::new()));
		}
		let in_ptrs: Vec<*const u8> = (0..input.planes())
			.map(|i| input.data(i).as_ptr())
			.collect();
		let (bufs, written) = self.swr_convert(out_samples as usize, Some(&in_ptrs), in_samples)?;
		Ok((written, bufs))
	}

	/// Allocate output buffers for `out_samples` and run `swr_convert`
	/// (with the given input pointers, or a drain when `None`).
	fn swr_convert(
		&mut self,
		out_samples: usize,
		in_ptrs: Option<&[*const u8]>,
		in_samples: i32,
	) -> crate::error::Result<(Vec<Vec<u8>>, usize)> {
		let planar = self.dst_format.is_planar();
		let planes = if planar { self.dst_channels.max(1) } else { 1 };
		let plane_len =
			out_samples * self.dst_format.bytes() * if planar { 1 } else { self.dst_channels };
		let mut bufs: Vec<Vec<u8>> = (0..planes).map(|_| vec![0u8; plane_len]).collect();
		let mut out_ptrs: Vec<*mut u8> = bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
		let written = unsafe {
			sys::swr_convert(
				self.ctx.as_mut_ptr(),
				out_ptrs.as_mut_ptr(),
				out_samples as i32,
				in_ptrs.map_or(std::ptr::null(), |p| p.as_ptr()),
				in_samples,
			)
		};
		if written < 0 {
			return Err(ffmpeg_err(FfmpegError::from(written)));
		}
		for buf in bufs.iter_mut() {
			let keep = written as usize
				* self.dst_format.bytes()
				* if planar { 1 } else { self.dst_channels };
			buf.truncate(keep);
		}
		Ok((bufs, written as usize))
	}
}

/// Convert a decoded audio frame to interleaved f32 at the resampler's
/// destination definition.
fn resample_to_interleaved_f32(
	resampler: &mut AudioResampler,
	input: &ffmpeg::frame::Audio,
) -> crate::error::Result<Vec<f32>> {
	let in_samples = input.samples() as i32;
	let out_samples = unsafe { sys::swr_get_out_samples(resampler.ctx.as_mut_ptr(), in_samples) };
	if out_samples < 0 {
		return Err(ffmpeg_err(FfmpegError::from(out_samples)));
	}
	if out_samples == 0 {
		return Ok(Vec::new());
	}
	let out_samples = out_samples as usize;
	let channels = resampler.dst_channels;
	let mut buf = vec![0f32; out_samples * channels];
	let out_ptrs = [buf.as_mut_ptr() as *mut u8];
	let in_ptrs: Vec<*const u8> = (0..input.planes())
		.map(|i| input.data(i).as_ptr())
		.collect();
	let written = unsafe {
		sys::swr_convert(
			resampler.ctx.as_mut_ptr(),
			out_ptrs.as_ptr(),
			out_samples as i32,
			in_ptrs.as_ptr(),
			in_samples,
		)
	};
	if written < 0 {
		return Err(ffmpeg_err(FfmpegError::from(written)));
	}
	buf.truncate(written as usize * channels);
	Ok(buf)
}

/// Flush any samples buffered in the resampler as interleaved f32.
fn flush_resampler_interleaved_f32(
	resampler: &mut AudioResampler,
) -> crate::error::Result<Option<Vec<f32>>> {
	let channels = resampler.dst_channels;
	let mut out = Vec::new();
	for _ in 0..32 {
		let out_samples = unsafe { sys::swr_get_out_samples(resampler.ctx.as_mut_ptr(), 0) };
		if out_samples <= 0 {
			break;
		}
		let out_samples = out_samples as usize;
		let mut buf = vec![0f32; out_samples * channels];
		let out_ptrs = [buf.as_mut_ptr() as *mut u8];
		let written = unsafe {
			sys::swr_convert(
				resampler.ctx.as_mut_ptr(),
				out_ptrs.as_ptr(),
				out_samples as i32,
				std::ptr::null(),
				0,
			)
		};
		if written < 0 {
			return Err(ffmpeg_err(FfmpegError::from(written)));
		}
		if written == 0 {
			break;
		}
		buf.truncate(written as usize * channels);
		out.extend_from_slice(&buf);
	}
	if out.is_empty() {
		Ok(None)
	} else {
		Ok(Some(out))
	}
}

/// Get the cached scaler for a (src → dst) pair, recreating it when the
/// parameters change.
fn get_or_create_scaler(
	cache: &mut Option<ScalingCache>,
	src_format: Pixel,
	src_width: u32,
	src_height: u32,
	dst_format: Pixel,
	dst_width: u32,
	dst_height: u32,
) -> crate::error::Result<&mut scaling::Context> {
	let recreate = match cache.as_ref() {
		Some(s) => {
			s.src_format != src_format
				|| s.src_width != src_width
				|| s.src_height != src_height
				|| s.dst_format != dst_format
				|| s.dst_width != dst_width
				|| s.dst_height != dst_height
		}
		None => true,
	};
	if recreate {
		// BILINEAR：与渲染侧原 Rust 双线性重采样器等效（POINT 会
		// 明显锯齿）；同尺寸纯格式转换时滤波器不参与运算。
		let ctx = scaling::Context::get(
			src_format,
			src_width,
			src_height,
			dst_format,
			dst_width,
			dst_height,
			scaling::Flags::BILINEAR,
		)
		.map_err(ffmpeg_err)?;
		*cache = Some(ScalingCache {
			ctx,
			src_format,
			src_width,
			src_height,
			dst_format,
			dst_width,
			dst_height,
		});
	}
	Ok(&mut cache.as_mut().expect("set above").ctx)
}

/// Map a frame's `AVCOL_SPC_*` tag to a swscale colorspace id (the YUV→RGB
/// coefficient set). Untagged frames fall back by size (HD material is
/// overwhelmingly BT.709, SD is BT.601 — the old code used BT.601 for
/// everything, tinting every HD source).
fn sws_colorspace_for(av_colorspace: i32, src_w: u32, src_h: u32) -> i32 {
	match av_colorspace {
		AVCOL_SPC_BT709 => SWS_CS_ITU709,
		AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M => SWS_CS_ITU601,
		AVCOL_SPC_SMPTE240M => SWS_CS_SMPTE240M,
		AVCOL_SPC_BT2020_NCL | AVCOL_SPC_BT2020_CL => SWS_CS_BT2020,
		// Untagged: HD → BT.709, SD → BT.601.
		_ => {
			if src_w >= 1280 || src_h > 576 {
				SWS_CS_ITU709
			} else {
				SWS_CS_ITU601
			}
		}
	}
}

/// The Rust-side YUV→RGB matrix for a frame's `AVCOL_SPC_*` tag. Unlike
/// [`sws_colorspace_for`] only tags with an exact matrix in [`YuvMatrix`]
/// are honored; everything else (including SMPTE 240M and BT.2020 CL) falls
/// back by size.
fn yuv_matrix_for(av_colorspace: i32, src_w: u32, src_h: u32) -> YuvMatrix {
	match av_colorspace {
		AVCOL_SPC_BT709 => YuvMatrix::Bt709,
		AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M => YuvMatrix::Bt601,
		AVCOL_SPC_BT2020_NCL => YuvMatrix::Bt2020,
		_ => {
			if src_w >= 1280 || src_h > 576 {
				YuvMatrix::Bt709
			} else {
				YuvMatrix::Bt601
			}
		}
	}
}

/// Bit depth (bits per component) and YUV-ness of a pixel format, from its
/// `AVPixFmtDescriptor` (8 and false for formats without one — none in
/// practice for decoder output).
fn pix_fmt_depth_and_yuv(fmt: Pixel) -> (i32, bool) {
	unsafe {
		let desc = sys::av_pix_fmt_desc_get(fmt.into());
		if desc.is_null() {
			(8, false)
		} else {
			((*desc).comp[0].depth, (*desc).flags & sys::AV_PIX_FMT_FLAG_RGB as u64 == 0)
		}
	}
}

/// Configure a swscale context's YUV→RGB matrix and range.
///
/// The range flag selects full/limited input coefficients; the RGB output is
/// always full range. `sws_setColorspaceDetails` ignores the tables for
/// non-YUV sources, so RGB footage passes through unchanged.
fn apply_sws_colorspace(
	ctx: &mut scaling::Context,
	av_colorspace: i32,
	full_range: bool,
	src_w: u32,
	src_h: u32,
) {
	let sws_cs = sws_colorspace_for(av_colorspace, src_w, src_h);
	unsafe {
		let inv_table = sys::sws_getCoefficients(sws_cs);
		let dst_table = sys::sws_getCoefficients(SWS_CS_ITU601);
		// brightness 0, contrast/saturation unity (16.16 fixed point).
		sys::sws_setColorspaceDetails(
			ctx.as_mut_ptr(),
			inv_table,
			full_range as i32,
			dst_table,
			1, // RGB out is full range
			0,
			1 << 16,
			1 << 16,
		);
	}
}

/// The frame's presentation timestamp (NOPTS when unset).
fn pts_of(f: Option<&ffmpeg::frame::Video>) -> Option<i64> {
	f.and_then(|f| f.pts())
}

/// Search the frame cache for the frame at (or closest to) `t`.
///
/// # CPP-PARITY
/// `FFmpegDecoder::get_frame_from_cache`.
fn get_frame_from_cache(video: &VideoDecodeState, t: i64) -> Option<ffmpeg::frame::Video> {
	let front = pts_of(video.cache.front()).unwrap_or(AV_NOPTS_VALUE);
	let back = pts_of(video.cache.back()).unwrap_or(AV_NOPTS_VALUE);
	if t < front {
		if video.cache_at_zero {
			return video.cache.front().cloned();
		}
	} else if t > back {
		if video.cache_at_eof {
			return video.cache.back().cloned();
		}
	} else {
		for (i, frame) in video.cache.iter().enumerate() {
			let this_pts = frame.pts().unwrap_or(AV_NOPTS_VALUE);
			let next_pts = video
				.cache
				.get(i + 1)
				.and_then(|f| f.pts())
				.unwrap_or(AV_NOPTS_VALUE);
			if this_pts == t || next_pts > t {
				return Some(frame.clone());
			}
		}
	}
	None
}

/// Decoder frame-cache size (C++ `maximum_queue_size`).
fn maximum_queue_size() -> usize {
	// # CPP-PARITY ffmpegdecoder.cpp:1184 — "Fairly arbitrary size... This
	// value may be tweaked over time."
	2
}

/// Copy a packed F32-RGBA byte buffer (as produced by swscale RGBAF32LE)
/// into an owned buffer, forcing alpha opaque (1.0) per pixel
/// (CPP-PARITY ffmpegdecoder.cpp:428).
fn convert_rgba_f32_le(data: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
	let mut out = vec![0u8; (w as usize) * (h as usize) * PIXEL_F32_BYTES];
	for y in 0..h as usize {
		let row = &data[y * stride..y * stride + (w as usize) * PIXEL_F32_BYTES];
		let dst =
			&mut out[y * (w as usize) * PIXEL_F32_BYTES..(y + 1) * (w as usize) * PIXEL_F32_BYTES];
		dst.copy_from_slice(row);
		for px in dst.chunks_exact_mut(PIXEL_F32_BYTES) {
			px[12..16].copy_from_slice(&1.0f32.to_le_bytes());
		}
	}
	out
}

/// Copy a packed 8-bit RGBA buffer into F32 RGBA bytes (the universal
/// swscale fallback; M12 P0 — some static FFmpeg swscale builds lack
/// float output formats).
fn convert_rgba8_to_f32(data: &[u8], w: u32, h: u32, stride: usize) -> Vec<u8> {
	let mut out = vec![0u8; (w as usize) * (h as usize) * PIXEL_F32_BYTES];
	for y in 0..h as usize {
		let row = &data[y * stride..y * stride + (w as usize) * 4];
		let dst =
			&mut out[y * (w as usize) * PIXEL_F32_BYTES..(y + 1) * (w as usize) * PIXEL_F32_BYTES];
		for (px, src_px) in dst
			.chunks_exact_mut(PIXEL_F32_BYTES)
			.zip(row.chunks_exact(4))
		{
			for c in 0..4 {
				px[c * 4..c * 4 + 4].copy_from_slice(&((src_px[c] as f32 / 255.0).to_le_bytes()));
			}
		}
	}
	out
}

/// Convert a 16-bit planar YUV 4:4:4 frame (YUV444P16LE, as emitted by the
/// high-bit-depth swscale fallback) to interleaved F32 RGBA little-endian
/// bytes. The YUV→RGB matrix and full/limited expansion run here instead of
/// inside swscale so the 16-bit code values survive intact: swscale only
/// converted the format (and resized), with identical source/destination
/// colorspace tables and full ranges on both sides, so no matrix and no
/// range recompression was applied. 10/12-bit sources arrive left-shifted
/// to 16-bit (code << 6 / code << 4) — exactly the code-value scale
/// [`oak_core::colormath::yuv444p16_to_rgb_f32`] expects.
fn convert_yuv444p16_to_rgba_f32(
	out: &ffmpeg::frame::Video,
	w: u32,
	h: u32,
	matrix: YuvMatrix,
	full_range: bool,
) -> Vec<u8> {
	let mut rgba = vec![0.0f32; (w as usize) * (h as usize) * 4];
	oak_core::colormath::yuv444p16_to_rgb_f32(
		out.data(0),
		out.stride(0),
		out.data(1),
		out.stride(1),
		out.data(2),
		out.stride(2),
		w as usize,
		h as usize,
		matrix,
		full_range,
		&mut rgba,
	);
	let mut bytes = vec![0u8; rgba.len() * 4];
	for (dst, v) in bytes.chunks_exact_mut(4).zip(&rgba) {
		dst.copy_from_slice(&v.to_le_bytes());
	}
	bytes
}

/// Build an allocated [`Frame`] (F32, RGBA) from raw pixel bytes.
///
/// # CPP-PARITY
/// `copy_packed_av_frame_to_frame` (ffmpegdecoder.cpp:116): params are built
/// from the decoded dimensions, the buffer is allocated and each row is
/// copied with the destination linesize.
fn copy_rgba_f32_to_frame(
	width: u32,
	height: u32,
	bytes: &[u8],
	timestamp: Rational,
) -> crate::error::Result<Frame> {
	let mut params = VideoParams::new_basic(
		width as i32,
		height as i32,
		OakPixelFormat::from_code(0),
		4,
		1,
		1,
		0,
		1,
	);
	params.set_format(OakPixelFormat::from_code(PixelFormat::F32 as i32));
	params.set_channel_count(VIDEO_CHANNELS);
	let mut frame = Frame::with_params(params);
	frame.set_timestamp(timestamp);
	frame.allocate()?;

	let row_bytes = (width as usize) * PIXEL_F32_BYTES;
	let linesize = frame.linesize_bytes() as usize;
	let out = frame.data_mut().ok_or(crate::error::Error::State)?;
	if out.len() < linesize * (height as usize) {
		return Err(crate::error::Error::State);
	}
	for y in 0..(height as usize) {
		let src = &bytes[y * row_bytes..y * row_bytes + row_bytes];
		let dst = &mut out[y * linesize..y * linesize + row_bytes];
		dst.copy_from_slice(src);
	}
	Ok(frame)
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Probe `filename`, building a [`FootageDescription`].
///
/// # CPP-PARITY
/// `FFmpegDecoder::probe` (ffmpegdecoder.cpp:484). Differences: video
/// stream details are taken from stream parameters (no second decode pass),
/// so `is_still` is always false and interlacing always progressive;
/// subtitle streams are counted but not added.
fn probe_file(filename: &str, cancelled: Option<&CancelAtom>) -> Option<FootageDescription> {
	let mut dict = Dictionary::new();
	dict.set("analyzeduration", "5000000");
	dict.set("probesize", "20000000");
	let input = ffmpeg::format::input_with_dictionary(filename, dict).ok()?;

	let mut desc = FootageDescription::new("ffmpeg");
	let footage_duration = input.duration();

	let file_meta: Vec<(String, String)> = input
		.metadata()
		.iter()
		.map(|(k, v)| (k.to_string(), v.to_string()))
		.collect();
	let mut source_start_time =
		extract_source_start_time(&file_meta, FfRational(1, FB_TIME_BASE as i32), 0);

	let stream_count = input.nb_streams();
	for i in 0..stream_count {
		if cancel_atom_is_cancelled(cancelled) {
			return None;
		}
		let Some(stream) = input.stream(i as usize) else {
			continue;
		};
		let params = stream.parameters();
		let medium = params.medium();

		if !source_start_time.valid {
			let stream_meta: Vec<(String, String)> = stream
				.metadata()
				.iter()
				.map(|(k, v)| (k.to_string(), v.to_string()))
				.collect();
			let raw = unsafe { params.as_ptr() };
			source_start_time =
				extract_source_start_time(&stream_meta, stream.time_base(), unsafe {
					(*raw).sample_rate
				});
		}

		// Only proceed if a decoder exists for this stream
		// (CPP-PARITY ffmpegdecoder.cpp:530).
		if ffmpeg::decoder::find(params.id()).is_none() {
			continue;
		}

		let raw = unsafe { params.as_ptr() };
		match medium {
			MediaType::Video => {
				let pixel = pixel_from_raw(unsafe { (*raw).format });
				let native = native_pixel_format(pixel);
				let frame_rate = stream.avg_frame_rate();
				let tb = stream.time_base();

				let mut vp =
					VideoParams::new_basic(1, 1, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
				vp.set_stream_index(i as i32);
				// SAFETY: `raw` points at the live stream's parameters (from
				// `params.as_ptr()` above), valid for the duration of `probe_file`.
				unsafe {
					vp.set_width((*raw).width);
					vp.set_height((*raw).height);
				}
				vp.set_video_type(VideoType::Video);
				vp.set_format(OakPixelFormat::from_code(native as i32));
				vp.set_channel_count(VIDEO_CHANNELS);
				vp.set_interlacing(Interlacing::None);
				vp.set_pixel_aspect_ratio(1, 1);
				vp.set_frame_rate(frame_rate.0 as i32, frame_rate.1 as i32);
				vp.set_start_time(stream.start_time());
				vp.set_time_base(tb.0 as i32, tb.1 as i32);
				vp.set_duration(stream.duration());
				vp.set_premultiplied_alpha(false);
				// Stream colorimetry (drives the input→working transform
				// and lets the UI show what the footage is).
				unsafe {
					vp.set_color_primaries((*raw).color_primaries as i32);
					vp.set_color_transfer((*raw).color_trc as i32);
					vp.set_color_range(if (*raw).color_range as i32 == AVCOL_RANGE_JPEG {
						oak_core::videoparams::ColorRange::Full
					} else {
						oak_core::videoparams::ColorRange::Limited
					});
				}
				desc.push_stream(StreamEntry::Video(vp));
			}
			MediaType::Audio => {
				let mut stream_duration = stream.duration();
				if stream_duration == AV_NOPTS_VALUE && footage_duration != AV_NOPTS_VALUE {
					// Fall back to the container duration rescaled into the
					// stream time base (CPP-PARITY ffmpegdecoder.cpp:621).
					stream_duration = unsafe {
						sys::av_rescale_q(
							footage_duration,
							FfRational(1, FB_TIME_BASE as i32).into(),
							stream.time_base().into(),
						)
					};
				}
				let sample_rate = unsafe { (*raw).sample_rate };
				let raw_layout = unsafe { ChannelLayout::from((*raw).ch_layout) };
				// Count-only layouts (WAV and other PCM containers report
				// AV_CHANNEL_ORDER_UNSPEC with a channel count but no mask)
				// yield a zero mask; derive a default mask from the count so
				// the stream stays usable (CPP-PARITY channel_layout_from_mask
				// fallback in the audio processors).
				let layout_mask = if raw_layout.bits() == 0 && raw_layout.channels() > 0 {
					ChannelLayout::default(raw_layout.channels()).bits()
				} else {
					raw_layout.bits()
				};
				let tb = stream.time_base();
				desc.push_stream(StreamEntry::Audio(AudioParams {
					sample_rate,
					channel_layout: layout_mask,
					format: 0,
					stream_index: i as i32,
					duration: stream_duration,
					time_base: (tb.0 as i32, tb.1 as i32),
				}));
			}
			_ => {}
		}
	}

	desc.set_stream_count(stream_count as usize);
	if source_start_time.valid {
		desc.set_source_start_time(source_start_time.time, 0);
	}
	Some(desc)
}

/// Parsed source start time.
struct SourceTime {
	valid: bool,
	time: Rational,
}

/// Extract a source start time from `timecode` / `time_reference` metadata,
/// mirroring `extract_source_start_time` (ffmpegdecoder.cpp:176).
fn extract_source_start_time(
	metadata: &[(String, String)],
	timebase: FfRational,
	sample_rate: i32,
) -> SourceTime {
	let mut out = SourceTime {
		valid: false,
		time: Rational::new(0, 1),
	};
	for (key, value) in metadata {
		if key == "timecode" {
			let parsed = crate::timecodemetadata::SourceTime::from_timecode_string(
				value,
				&oak_rational(timebase),
			);
			if parsed.valid {
				out.valid = true;
				out.time = parsed.time;
				return out;
			}
		} else if key == "time_reference" {
			let parsed =
				crate::timecodemetadata::SourceTime::from_bwf_time_reference(value, sample_rate);
			if parsed.valid {
				out.valid = true;
				out.time = parsed.time;
				return out;
			}
		}
	}
	out
}

// ---------------------------------------------------------------------------
// FFmpegEncoder
// ---------------------------------------------------------------------------

/// `olive::FFmpegEncoder` — FFmpeg-backed media encoder.
pub struct FFmpegEncoder {
	/// The encoding parameters this encoder was configured with.
	pub params: EncodingParams,
	/// Encoder session state.
	state: Mutex<EncoderState>,
}

/// Encoder session state.
struct EncoderState {
	/// Parameters from [`Encoder::configure`] (overrides `params`).
	configured: Option<EncodingParams>,
	/// The open output, if any.
	output: Option<OutputState>,
	/// Last error detail.
	last_error: String,
}

/// One opened output file.
struct OutputState {
	output: ffmpeg::format::context::Output,
	video: Option<VideoEncoderState>,
	audio: Option<AudioEncoderState>,
	/// Audio presentation timestamp in 1/sample_rate time base units.
	audio_pts: i64,
	flushed: bool,
}

// SAFETY: `EncoderState` owns raw FFmpeg contexts (output + encoders) that
// are not thread-safe themselves, but every access goes through the
// `FFmpegEncoder::state` `Mutex`, so concurrent access never happens.
unsafe impl Send for EncoderState {}
unsafe impl Sync for EncoderState {}

/// Opened video encoder + its conversion scaler.
struct VideoEncoderState {
	encoder: ffmpeg::codec::encoder::video::Encoder,
	stream_index: usize,
	/// swscale from the incoming F32-RGBA bytes to the encoder pixel format.
	scaler: scaling::Context,
	width: u32,
	height: u32,
	/// The encoder's time base after `open` (the frame PTS are expressed
	/// in it; see `FFmpegEncoder::open`).
	time_base: FfRational,
	/// One frame in the encoder's time base (the last packet's duration;
	/// see `FFmpegEncoder::open`).
	frame_duration: i64,
	/// The output stream's time base as left by `write_header`. The muxer
	/// may re-set it while writing the header (mp4/mov force a video
	/// timescale >= 10000) and FFmpeg 9 no longer rescales packet
	/// timestamps to match, so every packet must be rescaled into this
	/// value before `write_interleaved` (see `EncoderState::open`).
	stream_time_base: FfRational,
}

/// Opened audio encoder + its conversion resampler.
struct AudioEncoderState {
	encoder: ffmpeg::codec::encoder::audio::Encoder,
	stream_index: usize,
	/// swr from interleaved f32 to the encoder sample format.
	resampler: AudioResampler,
}

impl FFmpegEncoder {
	/// New encoder configured with `params`.
	pub(crate) fn with_params(params: EncodingParams) -> Self {
		FFmpegEncoder {
			params,
			state: Mutex::new(EncoderState {
				configured: None,
				output: None,
				last_error: String::new(),
			}),
		}
	}

	/// The effective encoding parameters (configured overrides construction).
	fn effective_params(&self) -> EncodingParams {
		let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state
			.configured
			.clone()
			.unwrap_or_else(|| self.params.clone())
	}
}

impl Encoder for FFmpegEncoder {
	fn id(&self) -> String {
		"ffmpeg".to_string()
	}

	fn supports_video(&self) -> bool {
		true
	}

	fn supports_audio(&self) -> bool {
		true
	}

	fn supports_subtitles(&self) -> bool {
		true
	}

	fn supports_image_sequences(&self) -> bool {
		false
	}

	fn is_configurable(&self) -> bool {
		true
	}

	fn configure(&self, params: &EncodingParams) -> crate::error::Result<()> {
		if !is_ffmpeg_format(params.format) {
			return Err(fail(format!("unknown export format {}", params.format)));
		}
		// `configure` cannot write the public `params` field through `&self`;
		// the configured copy is stored in the session state and used by
		// `open` (CPP-PARITY encoder.cpp `configure` stores the params).
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.configured = Some(params.clone());
		state.last_error.clear();
		Ok(())
	}

	fn open(&self) -> crate::error::Result<()> {
		let params = self.effective_params();
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.open(&params)
	}

	fn close(&self) -> crate::error::Result<()> {
		// `Encoder::close` is documented idempotent; flush if needed.
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.close()
	}

	fn write_video(&self, frame: &Frame) -> crate::error::Result<()> {
		let params = self.effective_params();
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.write_video(frame, &params)
	}

	fn write_audio(&self, samples: &[f32], frame_count: i32) -> crate::error::Result<()> {
		let params = self.effective_params();
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.write_audio(samples, frame_count, &params)
	}

	fn write_subtitle(
		&self,
		_text: &str,
		_in_seconds: f64,
		_out_seconds: f64,
	) -> crate::error::Result<()> {
		// Subtitle muxing is not exposed by the crate's encoder trait flow
		// (the C++ writes through the bridge's SRT encoder); report the same
		// unsupported state the stub did.
		Err(fail(
			"subtitle encoding is not supported by the ffmpeg encoder",
		))
	}

	fn flush(&self) -> crate::error::Result<()> {
		let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
		state.close()
	}

	fn desired_pixel_format(&self) -> Option<PixelFormat> {
		// The encoder accepts (and converts) F32 RGBA frames.
		Some(PixelFormat::F32)
	}

	fn desired_sample_format(&self) -> Option<SampleFormat> {
		// The encoder accepts interleaved f32 audio.
		Some(SampleFormat::F32)
	}

	fn filename(&self) -> String {
		c_string_1024(&self.params.filename)
	}

	fn get_error(&self) -> String {
		self.state
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.last_error
			.clone()
	}
}

/// Apply the export's delivery color metadata (H.273 code points, carried
/// in [`EncodingParams`]) to the video encoder before it opens. The values
/// are FFmpeg's own enum numbering, so each is re-interpreted into the
/// matching sys enum and handed to the typed setter; 0 (unset) fields keep
/// the codec default.
fn set_encoder_color_metadata(
	encoder: &mut ffmpeg::codec::encoder::video::Video,
	params: &EncodingParams,
) {
	if params.color_primaries != 0 {
		let v: sys::AVColorPrimaries =
			unsafe { std::mem::transmute(params.color_primaries) };
		encoder.set_color_primaries(v.into());
	}
	if params.color_trc != 0 {
		let v: sys::AVColorTransferCharacteristic =
			unsafe { std::mem::transmute(params.color_trc) };
		encoder.set_color_transfer_characteristic(v.into());
	}
	if params.color_space != 0 {
		let v: sys::AVColorSpace = unsafe { std::mem::transmute(params.color_space) };
		encoder.set_colorspace(v.into());
	}
	if params.color_range != 0 {
		let v: sys::AVColorRange = unsafe { std::mem::transmute(params.color_range) };
		encoder.set_color_range(v.into());
	}
}

/// Configure the encoder's RGB→YUV scaler so the produced YUV matches the
/// delivery tag written by [`set_encoder_color_metadata`] (otherwise swscale
/// defaults to BT.601/limited regardless of the tag, and players decode with
/// the wrong matrix). `params.color_space` is the `AVCOL_SPC_*` value; the
/// range follows `params.color_range` (1 = limited, 2 = full; 0 → limited).
fn apply_sws_output_colorspace(scaler: &mut scaling::Context, params: &EncodingParams) {
	let sws_cs = match params.color_space {
		1 => SWS_CS_ITU709,      // AVCOL_SPC_BT709
		9 | 10 => SWS_CS_BT2020, // AVCOL_SPC_BT2020_NCL / _CL
		_ => SWS_CS_ITU601,
	};
	let full_range = params.color_range == 2; // AVCOL_RANGE_JPEG
	unsafe {
		let table = sys::sws_getCoefficients(sws_cs);
		// src is RGB (always full range); dst is YUV with the delivery
		// matrix and range.
		sys::sws_setColorspaceDetails(
			scaler.as_mut_ptr(),
			table, // inv_table unused for an RGB source
			1,
			table,
			full_range as i32,
			0,
			1 << 16,
			1 << 16,
		);
	}
}

impl EncoderState {
	/// Open the output file, create the streams and encoders and write the
	/// header.
	fn open(&mut self, params: &EncodingParams) -> crate::error::Result<()> {		if self.output.is_some() {
			return Ok(());
		}
		let filename = c_string_1024(&params.filename);
		if filename.is_empty() {
			return Err(fail("no output filename"));
		}
		if params.video_enabled == 0 && params.audio_enabled == 0 {
			return Err(fail("no output tracks enabled"));
		}

		let mut output = ffmpeg::format::output(&filename)
			.map_err(|e| fail(format!("failed to create output '{filename}': {e}")))?;

		let mut video = None;
		if params.video_enabled != 0 {
			let codec_id = export_codec_to_id(params.video_codec)
				.ok_or_else(|| fail(format!("unknown video codec {}", params.video_codec)))?;
			let codec = ffmpeg::encoder::find(codec_id)
				.ok_or_else(|| fail(format!("no encoder for codec {:?}", codec_id)))?;

			let width = params.video_width.max(1) as u32;
			let height = params.video_height.max(1) as u32;
			let time_base = FfRational(params.video_time_base_num, params.video_time_base_den);
			let frame_rate = FfRational(
				params.video_time_base_den,
				params.video_time_base_num.max(1),
			);

			let mut stream = output.add_stream(codec).map_err(ffmpeg_err)?;
			let stream_index = stream.index();

			let mut encoder = ffmpeg::codec::Context::new_with_codec(codec)
				.encoder()
				.video()
				.map_err(ffmpeg_err)?;
			stream.set_parameters(&encoder);

			encoder.set_width(width);
			encoder.set_height(height);
			encoder.set_aspect_ratio(FfRational(
				params.video_pixel_aspect_num.max(1),
				params.video_pixel_aspect_den.max(1),
			));
			encoder.set_frame_rate(Some(frame_rate));
			// The codecs' packet timestamps use a fine tick (x264 encodes at
			// 1024 ticks per frame); give H.264 an encoder time base scaled
			// to that so the frame PTS stay integral. Other codecs (e.g.
			// MPEG-2) reject the scaled rate and keep the nominal
			// frame-duration time base. The stream is synced to this value
			// below (pre-header); packets are rescaled into the stream's
			// post-header time base when written (`stream_time_base`).
			let tick = if codec_id == ffmpeg::codec::Id::H264 {
				FfRational(time_base.0, time_base.1 * 1024)
			} else {
				time_base
			};
			encoder.set_time_base(tick);
			if params.video_bit_rate > 0 {
				encoder.set_bit_rate(params.video_bit_rate as usize);
			}
			if params.video_max_bit_rate > 0 {
				encoder.set_max_bit_rate(params.video_max_bit_rate as usize);
			}

			// Choose the target pixel format: explicit `video_pix_fmt`, else
			// a codec-appropriate default.
			let pix_fmt = pixel_format_from_name(&params.video_pix_fmt)
				.unwrap_or_else(|| default_pixel_format_for_codec(codec_id));
			encoder.set_format(pix_fmt);

			// Delivery color metadata (H.273 code points) → the container's
			// colr atom / H.264-HEVC VUI, so the exported file declares its
			// colorimetry instead of leaving players to guess. Only set when
			// the export populated them (0 = leave the codec default).
			set_encoder_color_metadata(&mut encoder, params);

			let opened = encoder.open().map_err(|e| { eprintln!("DBG-AUD: audio open failed: {e:?}"); ffmpeg_err(e) })?;
			stream.set_parameters(&opened);
			// The encoder may adjust the time base during `open` (x264
			// picks its own); sync the stream to the encoder's ACTUAL time
			// base. This is only the pre-header value though: the muxer
			// re-sets the stream time base inside `write_header` (mp4/mov
			// force a video timescale >= 10000, so a 10 fps stream's 1/10
			// becomes 1/10240) and FFmpeg 9 no longer rescales packet
			// timestamps for us, so the packets written after that point
			// must already be in the stream's final time base. `open`
			// records that post-header value and every video packet is
			// rescaled into it (identity when both match, e.g. mkv).
			let time_base = opened.time_base();
			stream.set_time_base(time_base);
			// One frame in the encoder's time base, used to fill the last
			// packet's duration: the muxer normally derives it from the
			// codec context attached to the stream, but the ffmpeg-next
			// flow never attaches one, so the final frame would carry
			// duration 0 and the track would be one frame short.
			let frame_duration = (time_base.1 as i64 * i64::from(frame_rate.1))
				/ (i64::from(time_base.0) * i64::from(frame_rate.0)).max(1);

			let mut scaler = scaling::Context::get(
				Pixel::RGBA,
				width,
				height,
				pix_fmt,
				width,
				height,
				scaling::Flags::BILINEAR,
			)
			.map_err(ffmpeg_err)?;
			// Match the RGB→YUV conversion to the delivery color tag so
			// players decode with the matrix/range the container declares.
			apply_sws_output_colorspace(&mut scaler, params);

			video = Some(VideoEncoderState {
				encoder: opened,
				stream_index,
				scaler,
				width,
				height,
				time_base,
				frame_duration,
				// Overwritten below with the stream's real post-header value.
				stream_time_base: time_base,
			});
		}

		let mut audio = None;
		if params.audio_enabled != 0 {
			let codec_id = export_codec_to_id(params.audio_codec)
				.ok_or_else(|| fail(format!("unknown audio codec {}", params.audio_codec)))?;
			let codec = ffmpeg::encoder::find(codec_id)
				.ok_or_else(|| fail(format!("no encoder for codec {:?}", codec_id)))?;

			let sample_rate = params.audio_sample_rate.max(1) as u32;
			let layout = channel_layout_from_mask(params.audio_channel_layout);

			let mut stream = output.add_stream(codec).map_err(ffmpeg_err)?;
			let stream_index = stream.index();
			stream.set_time_base(FfRational(1, sample_rate as i32));

			let mut encoder = ffmpeg::codec::Context::new_with_codec(codec)
				.encoder()
				.audio()
				.map_err(ffmpeg_err)?;
			stream.set_parameters(&encoder);

			encoder.set_rate(sample_rate as i32);
			encoder.set_channel_layout(layout);
			if params.audio_bit_rate > 0 {
				encoder.set_bit_rate(params.audio_bit_rate as usize);
			}
			// The encoder always runs in the codec's native sample
			// format (the params' delivery format is bridged by the
			// resampler below); forcing an incompatible format here
			// makes `open` fail with Invalid argument.
			let sample_fmt = default_sample_format_for_codec(codec_id);
			encoder.set_format(sample_fmt);

			let opened = encoder.open().map_err(ffmpeg_err)?;
			stream.set_parameters(&opened);

			let resampler = AudioResampler::get(
				Sample::F32(SampleType::Packed),
				layout,
				sample_rate,
				sample_fmt,
				layout,
				sample_rate,
			)?;

			audio = Some(AudioEncoderState {
				encoder: opened,
				stream_index,
				resampler,
			});
		}

		output.write_header().map_err(ffmpeg_err)?;

		// The muxer may have adjusted the stream time base while writing
		// the header (see the note above); read the value the container
		// actually uses so packets can be rescaled into it.
		if let Some(video) = video.as_mut() {
			if let Some(tb) = stream_time_base_after_header(&output, video.stream_index) {
				video.stream_time_base = tb;
			}
		}

		self.output = Some(OutputState {
			output,
			video,
			audio,
			audio_pts: 0,
			flushed: false,
		});
		Ok(())
	}

	/// Encode one frame (F32 RGBA) into the open output.
	fn write_video(&mut self, frame: &Frame, _params: &EncodingParams) -> crate::error::Result<()> {
		let output = self
			.output
			.as_mut()
			.ok_or_else(|| fail("encoder is not open"))?;
		if output.flushed {
			return Err(fail("encoder is already flushed"));
		}
		let video = output
			.video
			.as_mut()
			.ok_or_else(|| fail("encoder has no video track"))?;

		let data = frame.data().ok_or(crate::error::Error::State)?;
		let (w, h) = (frame.width() as u32, frame.height() as u32);
		if w != video.width || h != video.height {
			return Err(fail(format!(
				"frame size {w}x{h} does not match the encoder size {}x{}",
				video.width, video.height
			)));
		}

		// Convert the F32 RGBA buffer to 8-bit RGBA (swscale cannot take
		// float input on every build) and scale into the encoder format.
		let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
		let linesize = frame.linesize_bytes() as usize;
		for y in 0..(h as usize) {
			let row = &data[y * linesize..y * linesize + (w as usize) * PIXEL_F32_BYTES];
			let dst = &mut rgba[y * (w as usize) * 4..(y + 1) * (w as usize) * 4];
			for (out_px, in_px) in dst
				.chunks_exact_mut(4)
				.zip(row.chunks_exact(PIXEL_F32_BYTES))
			{
			for c in 0..4 {
				let v = f32::from_le_bytes([
					in_px[c * 4],
					in_px[c * 4 + 1],
					in_px[c * 4 + 2],
					in_px[c * 4 + 3],
				]);
				out_px[c] = (v * 255.0).clamp(0.0, 255.0) as u8;
			}
			}
		}

		let mut src = ffmpeg::frame::Video::new(Pixel::RGBA, w, h);
		src.data_mut(0).copy_from_slice(&rgba);

		let mut scaled = ffmpeg::frame::Video::empty();
		video.scaler.run(&src, &mut scaled).map_err(ffmpeg_err)?;

		// # CPP-PARITY
		// `FFmpegEncoder::write_frame` passes the frame time in seconds; the
		// Rust `Frame` carries the timestamp as a rational. The frame PTS
		// given to the encoder stays in the encoder's own time base
		// (captured at open) — the encoder validates them against its own
		// rate — and the packets it emits are rescaled into the stream's
		// post-`write_header` time base when written (`drain_video_packets`).
		let secs = frame.timestamp().to_f64();
		let tb = video.time_base;
		let pts = (secs * tb.1 as f64 / tb.0 as f64).round() as i64;
		scaled.set_pts(Some(pts));

		video.encoder.send_frame(&scaled).map_err(ffmpeg_err)?;
		drain_video_packets(&mut output.output, video)
	}

	/// Encode interleaved f32 audio into the open output.
	fn write_audio(
		&mut self,
		samples: &[f32],
		_frame_count: i32,
		params: &EncodingParams,
	) -> crate::error::Result<()> {
		let output = self
			.output
			.as_mut()
			.ok_or_else(|| fail("encoder is not open"))?;
		if output.flushed {
			return Err(fail("encoder is already flushed"));
		}
		let audio = output
			.audio
			.as_mut()
			.ok_or_else(|| fail("encoder has no audio track"))?;

		if samples.is_empty() {
			return Ok(());
		}
		let channels = audio.resampler.dst_channels.max(1);
		let in_frames = samples.len() / channels;

		// The encoder accepts at most `frame_size` samples per frame (AAC:
		// 1024), so the (possibly whole-range) input buffer is split into
		// chunks. The resampler converts at the same rate (the rendered
		// audio rate equals the encoder rate), but swr may buffer a small
		// delay, so the input chunk is shrunk until its predicted output
		// fits `frame_size`.
		let frame_size = audio.encoder.frame_size().max(1) as usize;
		let mut offset = 0usize;
		while offset < in_frames {
			let mut take = frame_size.min(in_frames - offset);
			loop {
				let out = unsafe { sys::swr_get_out_samples(audio.resampler.ctx.as_mut_ptr(), take as i32) };
				if out >= 0 && out as usize <= frame_size {
					break;
				}
				take = take.saturating_sub(1);
				if take == 0 {
					take = 1;
					break;
				}
			}

			// Presentation timestamp in the output stream time base (1/sample_rate).
			let pts = output.audio_pts;
			let layout = channel_layout_from_mask(params.audio_channel_layout);
			let chunk = &samples[offset * channels..(offset + take) * channels];
			let mut input = ffmpeg::frame::Audio::new(Sample::F32(SampleType::Packed), take, layout);
			let bytes = unsafe {
				std::slice::from_raw_parts(chunk.as_ptr() as *const u8, chunk.len() * 4)
			};
			input.data_mut(0)[..bytes.len()].copy_from_slice(bytes);

			let mut converted = audio.resampler.convert_to_frame(&input).map_err(|e| {
				eprintln!("DBG-AUD: convert failed: {e:?}");
				fail(format!("{e:?}"))
			})?;
			if converted.samples() > 0 {
				converted.set_pts(Some(pts));
				output.audio_pts += converted.samples() as i64;
				audio
					.encoder
					.send_frame(&converted)
					.map_err(|e| {
						eprintln!("DBG-AUD: send failed: {e:?}");
						ffmpeg_err(e)
					})?;
				drain_audio_packets(&mut output.output, audio)?;
			}
			offset += take;
		}
		Ok(())
	}

	/// Flush encoders, write the trailer and close the output (idempotent).
	fn close(&mut self) -> crate::error::Result<()> {
		let Some(output) = self.output.as_mut() else {
			return Ok(());
		};
		if !output.flushed {
			if let Some(v) = output.video.as_mut() {
				drain_video_encoder(&mut output.output, v)?;
			}
			if let Some(a) = output.audio.as_mut() {
				drain_audio_encoder(&mut output.output, a)?;
			}
			output.output.write_trailer().map_err(ffmpeg_err)?;
			output.flushed = true;
		}
		// Drop the output (closes the file).
		self.output = None;
		Ok(())
	}
}

/// Drain a video encoder after EOF, writing any packets.
fn drain_video_encoder(
	output: &mut ffmpeg::format::context::Output,
	video: &mut VideoEncoderState,
) -> crate::error::Result<()> {
	video.encoder.send_eof().map_err(ffmpeg_err)?;
	let mut pkt = ffmpeg::packet::Packet::empty();
	loop {
		match video.encoder.receive_packet(&mut pkt) {
			Ok(()) => {
				pkt.set_stream(video.stream_index);
				if pkt.duration() <= 0 {
					pkt.set_duration(video.frame_duration);
				}
				// Same rescale as `drain_video_packets`: the final flushed
				// frame is emitted with encoder time base, which the muxer
				// cannot consume directly.
				pkt.rescale_ts(video.time_base, video.stream_time_base);
				pkt.write_interleaved(output).map_err(ffmpeg_err)?;
			}
			Err(e) if is_eof_or_eagain(&e) => break,
			Err(e) => return Err(ffmpeg_err(e)),
		}
	}
	Ok(())
}

/// Drain an audio encoder after EOF, writing any packets.
fn drain_audio_encoder(
	output: &mut ffmpeg::format::context::Output,
	audio: &mut AudioEncoderState,
) -> crate::error::Result<()> {
	audio.encoder.send_eof().map_err(ffmpeg_err)?;
	let mut pkt = ffmpeg::packet::Packet::empty();
	loop {
		match audio.encoder.receive_packet(&mut pkt) {
			Ok(()) => {
				pkt.set_stream(audio.stream_index);
				pkt.write_interleaved(output).map_err(ffmpeg_err)?;
			}
			Err(e) if is_eof_or_eagain(&e) => break,
			Err(e) => return Err(ffmpeg_err(e)),
		}
	}
	Ok(())
}

/// Drain ready packets from a video encoder into the muxer.
fn drain_video_packets(
	output: &mut ffmpeg::format::context::Output,
	video: &mut VideoEncoderState,
) -> crate::error::Result<()> {
	let mut pkt = ffmpeg::packet::Packet::empty();
	loop {
		match video.encoder.receive_packet(&mut pkt) {
			Ok(()) => {
				pkt.set_stream(video.stream_index);
				// The final frame carries no duration (see the
				// `frame_duration` note); fill it so the track length is
				// the full export range.
				if pkt.duration() <= 0 {
					pkt.set_duration(video.frame_duration);
				}
				// The encoder emits timestamps in its own time base; the
				// muxer expects them in the stream's post-`write_header`
				// time base (see `EncoderState::open`), so rescale the
				// whole packet (pts/dts/duration) before writing.
				pkt.rescale_ts(video.time_base, video.stream_time_base);
				pkt.write_interleaved(output).map_err(ffmpeg_err)?;
			}
			Err(e) if is_eof_or_eagain(&e) => break,
			Err(e) => return Err(ffmpeg_err(e)),
		}
	}
	Ok(())
}

/// Drain ready packets from an audio encoder into the muxer.
fn drain_audio_packets(
	output: &mut ffmpeg::format::context::Output,
	audio: &mut AudioEncoderState,
) -> crate::error::Result<()> {
	let mut pkt = ffmpeg::packet::Packet::empty();
	loop {
		match audio.encoder.receive_packet(&mut pkt) {
			Ok(()) => {
				pkt.set_stream(audio.stream_index);
				pkt.write_interleaved(output).map_err(ffmpeg_err)?;
			}
			Err(e) if is_eof_or_eagain(&e) => break,
			Err(e) => return Err(ffmpeg_err(e)),
		}
	}
	Ok(())
}

/// The stream's time base as recorded in the container after
/// `write_header` (the muxer may have re-set it). `None` if the stream is
/// missing or the muxer left the time base unset (num/den == 0).
fn stream_time_base_after_header(
	output: &ffmpeg::format::context::Output,
	index: usize,
) -> Option<FfRational> {
	// SAFETY: `output` is a live `AVFormatContext`; `nb_streams` bounds the
	// `streams` array, and `index` came from the same context's
	// `add_stream`. Reading the (muxer-owned) stream time base is a plain
	// field read with no aliasing.
	unsafe {
		let ctx = output.as_ptr();
		let streams = std::slice::from_raw_parts((*ctx).streams, (*ctx).nb_streams as usize);
		let st = *streams.get(index)?;
		let tb = (*st).time_base;
		if tb.num == 0 || tb.den == 0 {
			None
		} else {
			Some(FfRational(tb.num, tb.den))
		}
	}
}

/// Map an `ExportCodec::Codec` raw value to an FFmpeg codec id
/// (CPP-PARITY `FFmpegEncoder::export_codec_to_bridge`). Values are the
/// documented `ExportCodec::Codec` discriminants (see `exportcodec.rs`).
fn export_codec_to_id(codec: i32) -> Option<ffmpeg::codec::Id> {
	match codec {
		0 => Some(ffmpeg::codec::Id::DNXHD),       // DNxHD
		1 | 2 => Some(ffmpeg::codec::Id::H264),    // H264 / H264 RGB
		3 => Some(ffmpeg::codec::Id::HEVC),        // H265
		6 => Some(ffmpeg::codec::Id::PRORES),      // ProRes
		7 => Some(ffmpeg::codec::Id::CFHD),        // CineForm
		10 => Some(ffmpeg::codec::Id::MPEG2VIDEO), // MP2
		11 => Some(ffmpeg::codec::Id::MP3),        // MP3
		12 => Some(ffmpeg::codec::Id::AAC),        // AAC
		13 => Some(ffmpeg::codec::Id::PCM_S16LE),  // PCM
		14 => Some(ffmpeg::codec::Id::OPUS),       // Opus
		15 => Some(ffmpeg::codec::Id::VORBIS),     // Vorbis
		16 => Some(ffmpeg::codec::Id::FLAC),       // FLAC
		17 => Some(ffmpeg::codec::Id::SUBRIP),     // SRT
		18 => Some(ffmpeg::codec::Id::AV1),        // AV1
		_ => None,
	}
}

/// Whether `format` is a container the FFmpeg encoder handles
/// (CPP-PARITY `Encoder::create_from_params` format mapping).
fn is_ffmpeg_format(format: i32) -> bool {
	matches!(format, 0 | 1 | 2 | 4 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14)
}

/// Default encoder pixel format for a codec.
fn default_pixel_format_for_codec(codec: ffmpeg::codec::Id) -> Pixel {
	match codec {
		ffmpeg::codec::Id::PRORES | ffmpeg::codec::Id::DNXHD => Pixel::YUV422P10LE,
		_ => Pixel::YUV420P,
	}
}

/// Default encoder sample format for a codec.
fn default_sample_format_for_codec(codec: ffmpeg::codec::Id) -> Sample {
	match codec {
		ffmpeg::codec::Id::PCM_S16LE => Sample::I16(SampleType::Packed),
		ffmpeg::codec::Id::FLAC => Sample::I16(SampleType::Planar),
		ffmpeg::codec::Id::MP3 => Sample::F32(SampleType::Packed),
		_ => Sample::F32(SampleType::Planar),
	}
}

/// Parse a NUL-terminated pixel-format name (e.g. "yuv420p").
fn pixel_format_from_name(buf: &[u8; 64]) -> Option<Pixel> {
	let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
	let name = std::str::from_utf8(&buf[..end]).ok()?;
	if name.is_empty() {
		return None;
	}
	name.parse::<Pixel>().ok()
}

/// Map an oakcore sample format to an FFmpeg sample format.
fn sample_format_to_ffmpeg(f: SampleFormat) -> Option<Sample> {
	match f {
		SampleFormat::U8Planar => Some(Sample::U8(SampleType::Planar)),
		SampleFormat::S16Planar => Some(Sample::I16(SampleType::Planar)),
		SampleFormat::S32Planar => Some(Sample::I32(SampleType::Planar)),
		SampleFormat::S64Planar => Some(Sample::I64(SampleType::Planar)),
		SampleFormat::F32Planar => Some(Sample::F32(SampleType::Planar)),
		SampleFormat::F64Planar => Some(Sample::F64(SampleType::Planar)),
		SampleFormat::U8 => Some(Sample::U8(SampleType::Packed)),
		SampleFormat::S16 => Some(Sample::I16(SampleType::Packed)),
		SampleFormat::S32 => Some(Sample::I32(SampleType::Packed)),
		SampleFormat::S64 => Some(Sample::I64(SampleType::Packed)),
		SampleFormat::F32 => Some(Sample::F32(SampleType::Packed)),
		SampleFormat::F64 => Some(Sample::F64(SampleType::Packed)),
		SampleFormat::Invalid => None,
	}
}

/// Read a NUL-terminated C string out of the `EncodingParams.filename` byte
/// buffer, stopping at the first NUL (empty string when unset).
fn c_string_1024(buf: &[u8; 1024]) -> String {
	let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
	String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decoder::RetrieveVideoParams;
	use oak_core::Rational;

	fn video_params() -> RetrieveVideoParams {
		RetrieveVideoParams {
			stream: CodecStream::new(),
			time: Rational::new(1, 30),
			length: TimeRange::default(),
			force_range: crate::decoder::K_COLOR_RANGE_DEFAULT,
			is_image_sequence: false,
			image_sequence_digits: 0,
			image_sequence_number: 0,
			mode: crate::decoder::RenderMode::Offline,
			alpha_is_premultiplied: false,
			target_size: None,
		}
	}

	#[test]
	fn ffmpeg_decoder_identity_and_capabilities() {
		let d = FFmpegDecoder::new();
		assert_eq!(d.id(), "ffmpeg");
		assert!(d.supports_video());
		assert!(d.supports_audio());
		assert!(d.probe("any.mp4", None).is_none());
	}

	#[test]
	fn ffmpeg_decoder_closed_state_errors() {
		let d = FFmpegDecoder::new();
		// Open on a missing file fails and leaves the decoder closed
		// (C++ parity: the stream is unset on failure).
		let s = CodecStream::with_block("in.mp4".to_string(), 0, None);
		assert!(d.open(&s).is_err());
		assert_eq!(d.stream().filename(), "");
		assert!(d.close().is_ok());

		// All media operations on a closed decoder fail.
		assert!(d.retrieve_video_frame(&video_params()).is_err());
		assert!(d.retrieve_video(&video_params()).is_err());
		let mut dest = [0f32; 8];
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
		assert_eq!(d.get_audio_start_offset(), Rational::new(0, 1));
	}

	#[test]
	fn ffmpeg_encoder_identity_and_config_validation() {
		let mut params = EncodingParams::default();
		let name = b"out/ffmpeg.mp4";
		params.filename[..name.len()].copy_from_slice(name);
		params.format = 2; // mp4
		params.video_enabled = 1;
		params.video_codec = 1; // H264
		params.video_width = 64;
		params.video_height = 64;
		params.video_time_base_num = 1;
		params.video_time_base_den = 10;
		let e = FFmpegEncoder::with_params(params);

		assert_eq!(e.id(), "ffmpeg");
		assert!(e.supports_video());
		assert!(e.supports_audio());
		assert!(e.supports_subtitles());
		assert!(!e.supports_image_sequences());
		assert!(e.is_configurable());
		assert_eq!(e.filename(), "out/ffmpeg.mp4");
		assert_eq!(e.desired_pixel_format(), Some(PixelFormat::F32));
		assert_eq!(e.desired_sample_format(), Some(SampleFormat::F32));
		assert_eq!(e.get_error(), "");

		// configure rejects unknown export formats.
		let mut bad = EncodingParams::default();
		bad.format = 99;
		assert!(e.configure(&bad).is_err());

		// A valid configures succeeds but nothing is open yet.
		let mut good = EncodingParams::default();
		good.format = 2;
		good.video_enabled = 1;
		good.video_codec = 1;
		good.video_width = 64;
		good.video_height = 64;
		good.video_time_base_num = 1;
		good.video_time_base_den = 10;
		assert!(e.configure(&good).is_ok());

		// Operations before open fail cleanly; close/flush are safe.
		let frame = Frame::new();
		assert!(e.write_video(&frame).is_err());
		assert!(e.write_audio(&[0f32; 4], 1).is_err());
		assert!(e.write_subtitle("hi", 0.0, 1.0).is_err());
		assert!(e.close().is_ok());
		assert!(e.flush().is_ok());
	}

	#[test]
	fn ffmpeg_encoder_open_rejects_invalid_config() {
		// No filename.
		let e = FFmpegEncoder::with_params(EncodingParams::default());
		assert!(e.open().is_err());

		// No enabled tracks.
		let mut p = EncodingParams::default();
		let name = b"out/x.mp4";
		p.filename[..name.len()].copy_from_slice(name);
		p.format = 2;
		let e = FFmpegEncoder::with_params(p);
		assert!(e.open().is_err());
	}

	#[test]
	fn pix_fmt_depth_and_yuv_detects_depth_and_kind() {
		// YUV luma depths (on the YUVJ→regular-normalized format).
		assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV420P), (8, true));
		assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV420P10LE), (10, true));
		assert_eq!(pix_fmt_depth_and_yuv(Pixel::YUV444P16LE), (16, true));
		// RGB formats never take the high-bit-depth YUV fallback.
		assert_eq!(pix_fmt_depth_and_yuv(Pixel::RGBA), (8, false));
		assert_eq!(pix_fmt_depth_and_yuv(Pixel::RGB48LE), (16, false));
	}

	#[test]
	fn yuv_matrix_mapping_is_strict() {
		use oak_core::colormath::YuvMatrix;
		assert_eq!(yuv_matrix_for(AVCOL_SPC_BT709, 1920, 1080), YuvMatrix::Bt709);
		assert_eq!(yuv_matrix_for(AVCOL_SPC_BT470BG, 640, 480), YuvMatrix::Bt601);
		assert_eq!(yuv_matrix_for(AVCOL_SPC_SMPTE170M, 1920, 1080), YuvMatrix::Bt601);
		assert_eq!(yuv_matrix_for(AVCOL_SPC_BT2020_NCL, 1920, 1080), YuvMatrix::Bt2020);
		// SMPTE 240M / BT.2020 CL / unknown tags are NOT mapped directly —
		// they fall back by size (HD → BT.709, SD → BT.601).
		assert_eq!(yuv_matrix_for(AVCOL_SPC_SMPTE240M, 1920, 1080), YuvMatrix::Bt709);
		assert_eq!(yuv_matrix_for(AVCOL_SPC_BT2020_CL, 640, 480), YuvMatrix::Bt601);
		assert_eq!(yuv_matrix_for(0, 1920, 1080), YuvMatrix::Bt709);
		assert_eq!(yuv_matrix_for(0, 640, 480), YuvMatrix::Bt601);
		assert_eq!(yuv_matrix_for(0, 1000, 600), YuvMatrix::Bt709); // h > 576
		assert_eq!(yuv_matrix_for(0, 720, 576), YuvMatrix::Bt601); // 576 is SD
	}

	#[test]
	fn sws_colorspace_mapping_keeps_legacy_behavior() {
		assert_eq!(sws_colorspace_for(AVCOL_SPC_BT709, 0, 0), SWS_CS_ITU709);
		assert_eq!(sws_colorspace_for(AVCOL_SPC_BT470BG, 0, 0), SWS_CS_ITU601);
		assert_eq!(sws_colorspace_for(AVCOL_SPC_SMPTE170M, 0, 0), SWS_CS_ITU601);
		assert_eq!(sws_colorspace_for(AVCOL_SPC_SMPTE240M, 0, 0), SWS_CS_SMPTE240M);
		assert_eq!(sws_colorspace_for(AVCOL_SPC_BT2020_NCL, 0, 0), SWS_CS_BT2020);
		assert_eq!(sws_colorspace_for(AVCOL_SPC_BT2020_CL, 0, 0), SWS_CS_BT2020);
		assert_eq!(sws_colorspace_for(0, 1920, 1080), SWS_CS_ITU709);
		assert_eq!(sws_colorspace_for(0, 640, 480), SWS_CS_ITU601);
	}

	/// A 2×1 YUV444P16LE frame: full-range white (Y=65535, neutral C) left,
	/// full-range black (Y=0, neutral C) right. Each sample is a u16.
	fn synthetic_yuv444p16_frame() -> ffmpeg::frame::Video {
		let mut f = ffmpeg::frame::Video::new(Pixel::YUV444P16LE, 2, 1);
		for plane in 0..3 {
			let data = f.data_mut(plane);
			for (px, v) in data.chunks_exact_mut(2).take(2).enumerate() {
				let code = match plane {
					0 => [65535u16, 0u16][px], // luma: white, black
					_ => 32768u16,             // chroma: neutral
				};
				v[..2].copy_from_slice(&code.to_le_bytes());
			}
		}
		f
	}

	#[test]
	fn yuv444p16_fallback_round_trips_full_range_white_and_black() {
		let f = synthetic_yuv444p16_frame();
		let bytes = convert_yuv444p16_to_rgba_f32(&f, 2, 1, YuvMatrix::Bt709, true);
		let px = |i: usize| -> [f32; 4] {
			let b = &bytes[i * 16..i * 16 + 16];
			[
				f32::from_le_bytes(b[0..4].try_into().unwrap()),
				f32::from_le_bytes(b[4..8].try_into().unwrap()),
				f32::from_le_bytes(b[8..12].try_into().unwrap()),
				f32::from_le_bytes(b[12..16].try_into().unwrap()),
			]
		};
		let white = px(0);
		let black = px(1);
		for c in 0..3 {
			assert!((white[c] - 1.0).abs() < 1e-6, "white[{c}] = {}", white[c]);
			assert!(black[c].abs() < 1e-6, "black[{c}] = {}", black[c]);
		}
		assert_eq!(white[3], 1.0);
		assert_eq!(black[3], 1.0);
	}
}
