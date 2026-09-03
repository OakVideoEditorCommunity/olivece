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

//! The real-time resampler/format converter (`olive::AudioProcessor`).
//!
//! Drives an in-process FFmpeg audio filter graph (abuffer → atempo chain →
//! aformat → abuffersink) via ffmpeg-next; the C++ build went through the
//! `fb_audio_graph_*`/`fb_frame_*` symbols of libffmpeg_bridge, which only
//! existed to absorb FFmpeg API churn. The conversion output is always
//! planar 32-bit float
//! (`OAKAUDIO_PROCESSOR_OUTPUT_FORMAT == SampleFormat::F32Planar == 4`).

use std::ptr;
use std::sync::Mutex;

use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::format::Sample;
use ffmpeg::{ChannelLayout, Error as FfmpegError};
use ffmpeg_next as ffmpeg;

use crate::error::{Error, Result};
use crate::params::{AudioParams, SampleFormat};

/// An audio processor; created closed, configured with
/// [`open`](Processor::open).
pub struct Processor {
	inner: Mutex<ProcessorInner>,
}

/// Resampler state behind the handle's mutex.
struct ProcessorInner {
	/// Live filter graph (`None` = closed).
	graph: Option<ffmpeg::filter::Graph>,
	/// Scratch output frame reused for every pull.
	out_frame: ffmpeg::frame::Audio,
	/// Input spec recorded at `open`.
	from: AudioParams,
	/// Output spec recorded at `open`.
	to: AudioParams,
}

// SAFETY: the filter graph's raw pointers are only dereferenced through the
// FFmpeg API while the processor's mutex is held, so all access is
// serialized.
unsafe impl Send for ProcessorInner {}

impl Default for ProcessorInner {
	fn default() -> Self {
		ProcessorInner {
			graph: None,
			out_frame: ffmpeg::frame::Audio::empty(),
			from: AudioParams {
				sample_rate: 0,
				channel_layout: 0,
				format: SampleFormat::Invalid,
			},
			to: AudioParams {
				sample_rate: 0,
				channel_layout: 0,
				format: SampleFormat::Invalid,
			},
		}
	}
}

/// Map an oakcore [`SampleFormat`] to the equivalent ffmpeg [`Sample`].
/// Replaces `FFmpegUtils::get_ffmpeg_sample_format` crossing the oak_core
/// C ABI (`// CPP-PARITY: src/common/src/ffmpegutils.cpp:83`).
fn to_ffmpeg_sample_format(fmt: SampleFormat) -> Sample {
	match fmt {
		SampleFormat::U8Planar => Sample::U8(SampleType::Planar),
		SampleFormat::S16Planar => Sample::I16(SampleType::Planar),
		SampleFormat::S32Planar => Sample::I32(SampleType::Planar),
		SampleFormat::S64Planar => Sample::I64(SampleType::Planar),
		SampleFormat::F32Planar => Sample::F32(SampleType::Planar),
		SampleFormat::F64Planar => Sample::F64(SampleType::Planar),
		SampleFormat::U8 => Sample::U8(SampleType::Packed),
		SampleFormat::S16 => Sample::I16(SampleType::Packed),
		SampleFormat::S32 => Sample::I32(SampleType::Packed),
		SampleFormat::S64 => Sample::I64(SampleType::Packed),
		SampleFormat::F32 => Sample::F32(SampleType::Packed),
		SampleFormat::F64 => Sample::F64(SampleType::Packed),
		SampleFormat::Invalid => Sample::None,
	}
}

/// Rebuild an ffmpeg [`ChannelLayout`] from a channel mask (0 = unknown →
/// stereo fallback). Same construction as oakcodec's
/// `channel_layout_from_mask`.
fn channel_layout_from_mask(mask: u64) -> ChannelLayout {
	if mask == 0 {
		return ChannelLayout::default(2);
	}
	let channels = mask.count_ones() as i32;
	ChannelLayout(ffmpeg::ffi::AVChannelLayout {
		order: ffmpeg::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE,
		nb_channels: channels,
		u: ffmpeg::ffi::AVChannelLayout__bindgen_ty_1 { mask },
		opaque: ptr::null_mut(),
	})
}

/// `// CPP-PARITY: src/audio/src/audioprocessor.cpp:50` — ensure a usable
/// channel layout mask: 0 (unknown) falls back to a default layout derived
/// from the channel count, itself defaulting to stereo.
fn fix_channel_layout(params: AudioParams) -> AudioParams {
	let mut result = params;
	if params.channel_layout == 0 {
		let mut channels = params.channel_count();
		if channels <= 0 {
			channels = 2;
		}
		result.channel_layout = ChannelLayout::default(channels).bits();
	}
	result
}

/// Build the conversion graph: abuffer → atempo chain → aformat (fltp at the
/// output rate/layout) → abuffersink. `atempo` accepts factors in
/// [0.5, 100], so out-of-range tempos are chained
/// (`// CPP-PARITY: ffmpeg_bridge.cpp` `fb_audio_graph_create`).
fn build_graph(from: &AudioParams, to: &AudioParams, speed: f64) -> Result<ffmpeg::filter::Graph> {
	let in_format = to_ffmpeg_sample_format(from.format);
	if in_format == Sample::None {
		return Err(Box::new(Error::Failed("invalid input sample format".to_string())));
	}

	let abuffer = ffmpeg::filter::find("abuffer")
		.ok_or_else(|| Error::Failed("abuffer filter not found".to_string()))?;
	let abuffersink = ffmpeg::filter::find("abuffersink")
		.ok_or_else(|| Error::Failed("abuffersink filter not found".to_string()))?;

	let mut graph = ffmpeg::filter::Graph::new();
	let in_args = format!(
		"time_base=1/{rate}:sample_rate={rate}:sample_fmt={fmt}:channel_layout=0x{layout:x}",
		rate = from.sample_rate,
		fmt = in_format.name(),
		layout = from.channel_layout,
	);
	graph
		.add(&abuffer, "in", &in_args)
		.map_err(|e| Error::Failed(format!("failed to add abuffer: {e}")))?;
	graph
		.add(&abuffersink, "out", "")
		.map_err(|e| Error::Failed(format!("failed to add abuffersink: {e}")))?;

	// Chain atempo for out-of-range factors, then force the output format
	// (planar f32 at the requested rate/layout) with aformat.
	let mut spec = String::new();
	let mut tempo = speed;
	while tempo > 100.0 {
		spec.push_str("atempo=100.0,");
		tempo /= 100.0;
	}
	while tempo < 0.5 {
		spec.push_str("atempo=0.5,");
		tempo /= 0.5;
	}
	if tempo != 1.0 {
		spec.push_str(&format!("atempo={tempo},"));
	}
	spec.push_str(&format!(
		"aformat=sample_fmts=fltp:sample_rates={}:channel_layouts=0x{:x}",
		to.sample_rate, to.channel_layout,
	));

	graph
		.output("in", 0)
		.and_then(|p| p.input("out", 0))
		.and_then(|p| p.parse(&spec))
		.map_err(|e| Error::Failed(format!("failed to parse filter spec: {e}")))?;
	graph
		.validate()
		.map_err(|e| Error::Failed(format!("failed to validate filter graph: {e}")))?;
	Ok(graph)
}

/// Whether a pull error just means "no output available right now" (needs
/// more input, or the drained end after a flush).
fn is_drain(e: &FfmpegError) -> bool {
	matches!(e, FfmpegError::Eof)
		|| matches!(e, FfmpegError::Other { errno } if *errno == ffmpeg::error::EAGAIN)
}

impl Processor {
	/// Create a closed processor.
	pub fn init() -> Processor {
		Processor {
			inner: Mutex::new(ProcessorInner::default()),
		}
	}

	/// Open the resampling/format-conversion graph. `out_format` is accepted
	/// for interface completeness but the conversion output is always planar
	/// f32.
	///
	/// `// CPP-PARITY: src/audio/c_api/processor.cpp:43` (validation order:
	/// already-open state, invalid rates/speed, forced output format) and
	/// `src/audio/src/audioprocessor.cpp:82` (graph creation).
	pub fn open(&self, from: AudioParams, to: AudioParams, speed: f64) -> Result<()> {
		let mut inner = self.inner.lock().unwrap();

		if inner.graph.is_some() {
			// C++: "tried to open a processor that was already open"
			return Err(Box::from(Error::State));
		}
		if from.sample_rate <= 0 || to.sample_rate <= 0 || speed <= 0.0 {
			return Err(Box::from(Error::Invalid));
		}
		// The C ABI delivers planar float output only; force the output format
		// stage to f32p (OAKAUDIO_PROCESSOR_OUTPUT_FORMAT == 4).
		if to.format != SampleFormat::F32Planar {
			return Err(Box::from(Error::Invalid));
		}

		let from_fixed = fix_channel_layout(from);
		let to_fixed = fix_channel_layout(to);

		// C++: "failed to create audio filter graph"
		let graph = build_graph(&from_fixed, &to_fixed, speed)?;

		inner.graph = Some(graph);
		inner.out_frame = ffmpeg::frame::Audio::empty();
		inner.from = from_fixed;
		inner.to = to_fixed;
		Ok(())
	}

	/// Close the graph (safe when closed).
	pub fn close(&self) -> Result<()> {
		let mut inner = self.inner.lock().unwrap();

		inner.graph = None;
		inner.out_frame = ffmpeg::frame::Audio::empty();
		Ok(())
	}

	/// 1 when open, 0 when closed.
	pub fn is_open(&self) -> Result<bool> {
		let inner = self.inner.lock().unwrap();
		Ok(inner.graph.is_some())
	}

	/// Push planar float input and pull converted output. Returns the number
	/// of output frames written.
	///
	/// `// CPP-PARITY: src/audio/c_api/processor.cpp:91` (validation, state
	/// check, null `out_planar` short-circuit) and
	/// `src/audio/src/audioprocessor.cpp:141` (push/pull loop, byte counting).
	pub fn convert(
		&self,
		in_planar: *const *const f32,
		in_frame_count: i32,
		out_planar: *const *mut f32,
		out_capacity_frames: i32,
	) -> Result<i32> {
		let mut guard = self.inner.lock().unwrap();
		let inner = &mut *guard;

		if inner.graph.is_none() {
			return Err(Box::from(Error::State));
		}
		if in_frame_count < 0 || out_capacity_frames < 0 || (in_frame_count > 0 && in_planar.is_null())
		{
			return Err(Box::from(Error::Invalid));
		}

		let channels = inner.to.channel_count();
		if channels <= 0 {
			return Err(Box::from(Error::State));
		}

		let from = inner.from;
		let graph = inner.graph.as_mut().unwrap();
		let out_frame = &mut inner.out_frame;

		if in_frame_count > 0 {
			// The FFI layer has no way to know the input plane count, so the
			// plane pointer array is walked using the input spec recorded at
			// `open` (`// CPP-PARITY: src/audio/src/audioprocessor.cpp:141`).
			let nb = in_frame_count as usize;
			let in_channels = from.channel_count().max(0) as usize;
			let layout = channel_layout_from_mask(from.channel_layout);
			let mut frame = ffmpeg::frame::Audio::new(to_ffmpeg_sample_format(from.format), nb, layout);
			frame.set_rate(from.sample_rate as u32);
			let planar = from.format.is_planar();
			// `plane_mut::<T>` requires the exact sample type of the frame
			// format, so the copy dispatches on the recorded input format.
			macro_rules! fill {
				($t:ty) => {{
					if planar {
						for ch in 0..in_channels {
							// SAFETY: `in_planar` is non-null here and the FFI
							// contract guarantees at least `from.channel_count()`
							// entries, each pointing at `nb` samples of the
							// recorded input format.
							let src = unsafe { *in_planar.add(ch) } as *const $t;
							let dst = frame.plane_mut::<$t>(ch);
							unsafe { ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), nb) };
						}
					} else {
						// Packed input: a single plane at `in_planar[0]`.
						// SAFETY: see above; the plane holds `nb * channels`
						// samples.
						let src = unsafe { *in_planar } as *const $t;
						let dst = frame.plane_mut::<$t>(0);
						unsafe { ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), nb * in_channels) };
					}
				}};
			}
			match from.format {
				SampleFormat::U8Planar | SampleFormat::U8 => fill!(u8),
				SampleFormat::S16Planar | SampleFormat::S16 => fill!(i16),
				SampleFormat::S32Planar | SampleFormat::S32 => fill!(i32),
				SampleFormat::S64Planar | SampleFormat::S64 => {
					// ffmpeg-next's typed plane API has no `i64` impl; copy the
					// 8-byte samples through the raw plane pointers.
					if planar {
						for ch in 0..in_channels {
							// SAFETY: same contract as above; the plane is
							// `nb * 8` bytes.
							let src = unsafe { *in_planar.add(ch) } as *const u8;
							let dst = unsafe { *(*frame.as_mut_ptr()).extended_data.add(ch) };
							unsafe { ptr::copy_nonoverlapping(src, dst, nb * 8) };
						}
					} else {
						// SAFETY: same contract as above; the plane is
						// `nb * channels * 8` bytes.
						let src = unsafe { *in_planar } as *const u8;
						let dst = unsafe { *(*frame.as_mut_ptr()).extended_data };
						unsafe { ptr::copy_nonoverlapping(src, dst, nb * in_channels * 8) };
					}
				}
				SampleFormat::F32Planar | SampleFormat::F32 => fill!(f32),
				SampleFormat::F64Planar | SampleFormat::F64 => fill!(f64),
				SampleFormat::Invalid => return Err(Box::new(Error::State)),
			}
			if let Err(e) = graph.get("in").unwrap().source().add(&frame) {
				return Err(Box::new(Error::Failed(format!(
					"failed to add frame to buffersrc: {e}"
				))));
			}
		}

		// C++: `out_planar ? &buf : nullptr` — with no destination, the input is
		// pushed but nothing is pulled.
		if out_planar.is_null() {
			return Ok(0);
		}

		let mut total: i64 = 0;
		loop {
			let pulled = graph.get("out").unwrap().sink().frame(out_frame);
			match pulled {
				Ok(()) => {}
				Err(e) if is_drain(&e) => break,
				Err(e) => {
					return Err(Box::from(Error::Failed(format!(
						"failed to pull from buffersink: {e}"
					))))
				}
			}

			let nb = out_frame.samples() as i32;
			if nb > 0 && total < i64::from(out_capacity_frames) {
				let to_copy = (i64::from(out_capacity_frames) - total).min(i64::from(nb)) as i32;
				for ch in 0..channels {
					// SAFETY: the FFI contract guarantees at least `channels`
					// entries in `out_planar` (NULL entries are skipped).
					let dst = unsafe { *out_planar.add(ch as usize) };
					if dst.is_null() {
						continue;
					}
					// Output is planar f32 (enforced by open()); each plane is
					// `to_copy` float samples.
					let src = out_frame.plane::<f32>(ch as usize);
					unsafe {
						ptr::copy_nonoverlapping(src.as_ptr(), dst, to_copy as usize);
					}
				}
			}
			total += i64::from(nb);
		}

		Ok(total.min(i64::from(out_capacity_frames)) as i32)
	}

	/// Signal end-of-input to the graph (flushes internal delay).
	///
	/// `// CPP-PARITY: src/audio/c_api/processor.cpp:137` (state check)
	/// and `src/audio/src/audioprocessor.cpp:210` (flush has no failure path; a
	/// negative push return is logged only).
	pub fn flush(&self) -> Result<()> {
		let mut inner = self.inner.lock().unwrap();

		let Some(graph) = inner.graph.as_mut() else {
			return Err(Box::from(Error::State));
		};
		let _ = graph.get("in").unwrap().source().flush();
		Ok(())
	}
}

/// Format of the conversion output (always planar f32).
pub const OUTPUT_FORMAT: SampleFormat = SampleFormat::F32Planar;
