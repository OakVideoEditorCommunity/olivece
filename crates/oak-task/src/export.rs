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

//! `ExportTask`, mirroring `src/task/src/export/export.h`.
//!
//! Renders the viewer output through [`crate::render::RenderTask`] and
//! writes it to a file via the direct oakcodec encoder API
//! (`oak_codec::encoder::{create_from_params, Encoder}` — single-lib
//! unification; the old encoder C ABI is gone), mapping each rendered
//! frame to an `Encoder::write_video` call and each rendered audio buffer
//! to `Encoder::write_audio`.
//!
//! The viewer and color-manager arguments of the deleted C ABI path are
//! replaced by a single [`crate::nodeops::NodeRef`] viewer (the color
//! manager no longer crosses into the render tickets — the direct ticket
//! arena performs no color management). Rendered frames arrive as
//! `oak_core::texture::Texture` values and are converted to
//! `oak_codec::frame::Frame` values for the encoder.
//!
//! **Simplifications over the C++**: no temporary-file rename dance (the
//! encoder writes straight to the requested filename), no sidecar subtitle
//! encoder (subtitles go through the main encoder), and no frame/audio
//! reordering maps — the [`RenderTask`] loop delivers frames and audio in
//! timestamp order (its reorder buffer), so the encoder is written directly.
//!
//! CPP-PARITY: src/task/src/export/export.h

use std::sync::Arc;

use oak_codec::encoder::{create_from_params, Encoder};
use oak_codec::encodingparams::EncodingParams as CodecEncodingParams;
use oak_core::videoparams::VideoParams as CommonVideoParams;
use oak_core::texture::Texture;

use crate::error::{Error, Result};
use crate::nodeops::{self, NodeRef};
use crate::render::{ForceParams, RenderTask, RenderTaskBehavior};
use crate::task::{Task, TaskBehavior};
use oak_core::{Rational, TimeRange};

/// An export task. Owns the encoder; the base [`RenderTask`] does the
/// frame/audio production.
pub struct ExportTask {
	/// The render base (itself a task).
	pub render: RenderTask,
	/// The node being exported (footage or sequence).
	pub viewer_node: NodeRef,
	/// The encoding parameters (mirror of the codec-side params).
	pub encoding_params: EncodingParams,
	/// The opened encoder (direct oakcodec trait object).
	encoder: Option<Arc<dyn Encoder>>,
}

/// Mirror of the codec encoding params, kept as plain fields so the export
/// task can be configured without a codec-side handle. Only the fields the
/// task reads are declared; see `oak_codec::encodingparams::EncodingParams`
/// for the full inventory.
pub struct EncodingParams {
	/// Output filename.
	pub filename: String,
	/// Export format id (`olive::ExportFormat::Format`).
	pub format: i32,
	/// Whether video is exported.
	pub video_enabled: bool,
	/// Video codec id.
	pub video_codec: i32,
	/// Output width.
	pub video_width: i32,
	/// Output height.
	pub video_height: i32,
	/// Frame duration rational (numerator/denominator).
	pub video_time_base_num: i32,
	/// Frame duration denominator.
	pub video_time_base_den: i32,
	/// Delivery pixel format (`oak_core::PixelFormat` as int).
	pub video_pixel_format: i32,
	/// Whether audio is exported.
	pub audio_enabled: bool,
	/// Audio codec id.
	pub audio_codec: i32,
	/// Audio sample rate (Hz) the encoder opens with.
	pub audio_sample_rate: i32,
	/// ffmpeg-style channel layout mask the encoder opens with.
	pub audio_channel_layout: u64,
	/// Whether subtitles are exported.
	pub subtitles_enabled: bool,
	/// Export length numerator (seconds rational).
	pub export_length_num: i32,
	/// Export length denominator.
	pub export_length_den: i32,
	/// Whether a custom in/out export range is set (work-area export). When
	/// set, [`ExportTask::export_range`] renders exactly `[in, out)` instead
	/// of the whole viewer length.
	pub has_custom_range: bool,
	/// Custom range in, rational seconds.
	pub custom_range_in_num: i32,
	/// Custom range in denominator.
	pub custom_range_in_den: i32,
	/// Custom range out, rational seconds.
	pub custom_range_out_num: i32,
	/// Custom range out denominator.
	pub custom_range_out_den: i32,
	/// Video bit rate (bit/s; 0 = codec default).
	pub video_bit_rate: i64,
	/// Audio bit rate (bit/s; 0 = codec default).
	pub audio_bit_rate: i64,
	/// Delivery colorimetry override flags: `0` = use the project's output
	/// spec (legacy), `1` = apply the `color_*` code points from the export
	/// dialog (SDR 709 / HDR 2020 selection).
	pub color_override_enabled: bool,
	/// AVColorPrimaries (SDR 709 = 1, HDR BT.2020 = 9).
	pub color_primaries: i32,
	/// AVColorTransferCharacteristic (SDR 709 = 1, HDR PQ = 16).
	pub color_trc: i32,
	/// AVColorSpace (SDR 709 = 1, HDR BT.2020 = 9).
	pub color_space: i32,

}

impl ExportTask {
	/// Build a new export task from the viewer node and the encoding
	/// params. The old `viewer: CHandle` / `color_manager: CHandle`
	/// signature is replaced by the domain viewer [`NodeRef`] (single-lib
	/// unification; the color manager is dropped with the C ABI — see the
	/// module docs).
	pub fn new(viewer: NodeRef, params: EncodingParams) -> ExportTask {
		let label = nodeops::node_label(&viewer.0, viewer.1);
		let title = format!("Exporting \"{label}\"");
		let base = Task::new(&title, None);
		// The render base needs the viewer's frame rate to step one frame
		// per video frame (the export range is in sequence/footage time);
		// without it the frame loop falls back to a 1/1 timebase and
		// exports one frame per second (observed in the facade's
		// `it_export` run).
		let video_params = nodeops::sequence_video_params(&viewer.0, viewer.1, 0)
			.or_else(|| nodeops::footage_video_params(&viewer.0, viewer.1, 0));
		let render = RenderTask::new(base, video_params, viewer.clone(), ForceParams::default(), None);
		ExportTask {
			render,
			viewer_node: viewer,
			encoding_params: params,
			encoder: None,
		}
	}

	/// Build the codec `EncodingParams` POD for the encoder from the Rust
	/// mirror (zeroed fields = disabled/defaults).
	fn build_codec_params(&self) -> CodecEncodingParams {
		let mut params = CodecEncodingParams::default();
		let bytes = self.encoding_params.filename.as_bytes();
		let n = bytes.len().min(1023);
		params.filename[..n].copy_from_slice(&bytes[..n]);
		params.format = self.encoding_params.format;
		params.video_enabled = self.encoding_params.video_enabled as i32;
		params.video_codec = self.encoding_params.video_codec;
		params.video_width = self.encoding_params.video_width;
		params.video_height = self.encoding_params.video_height;
		params.video_time_base_num = self.encoding_params.video_time_base_num;
		params.video_time_base_den = self.encoding_params.video_time_base_den;
		params.video_pixel_format =
			crate::nodeops::pixel_format_from_code(self.encoding_params.video_pixel_format);
		params.audio_enabled = self.encoding_params.audio_enabled as i32;
		params.audio_codec = self.encoding_params.audio_codec;
		params.audio_sample_rate = self.encoding_params.audio_sample_rate;
		params.audio_channel_layout = self.encoding_params.audio_channel_layout;
		params.subtitles_enabled = self.encoding_params.subtitles_enabled as i32;
		params.export_length_num = self.encoding_params.export_length_num;
		params.export_length_den = self.encoding_params.export_length_den;
		params.video_bit_rate = self.encoding_params.video_bit_rate;
		params.audio_bit_rate = self.encoding_params.audio_bit_rate;
		// Delivery colorimetry: the EXPORT DIALOG's choice (SDR 709 8-bit /
		// HDR 2020 10-bit) wins when enabled; otherwise the project's
		// output colorspace (H.273 code points → mov `colr` atom / VUI).
		// Limited range is the video-delivery convention; the encoder's
		// RGB→YCbCr runs limited.
		if self.encoding_params.color_override_enabled {
			params.color_primaries = self.encoding_params.color_primaries;
			params.color_trc = self.encoding_params.color_trc;
			params.color_space = self.encoding_params.color_space;
			params.color_range = 1; // AVCOL_RANGE_MPEG (limited)
		} else {
			let (_working, spec) = self.delivery_color();
			params.color_primaries = spec.gamut.av_color_primaries();
			params.color_trc = spec.transfer.av_color_trc();
			params.color_space = spec.gamut.av_color_space();
			params.color_range = 1; // AVCOL_RANGE_MPEG (limited)
		}
		params
	}

	/// Resolve the export range: the custom range when set, otherwise the
	/// whole viewer length (direct `oaknode` domain query; the deleted
	/// `oaknode_sequence_get_length` stub is gone). The custom range is the
	/// work-area / in-out export: `export_params_pod` copies it from the
	/// facade params handle, so `oakengine_export_render_with_params` and the
	/// app's work-area export render exactly `[in, out)`.
	fn export_range(&self) -> TimeRange {
		if self.encoding_params.has_custom_range
			&& self.encoding_params.custom_range_in_den != 0
			&& self.encoding_params.custom_range_out_den != 0
		{
			let in_ = Rational::new(
				i64::from(self.encoding_params.custom_range_in_num),
				i64::from(self.encoding_params.custom_range_in_den),
			);
			let out = Rational::new(
				i64::from(self.encoding_params.custom_range_out_num),
				i64::from(self.encoding_params.custom_range_out_den),
			);
			if out > in_ {
				return TimeRange::new(in_, out);
			}
		}
		let length = nodeops::node_length(&self.viewer_node.0, self.viewer_node.1);
		TimeRange::new(Rational::new(0, 1), length)
	}

	/// The project's pipeline color settings (working colorspace + the
	/// delivery output spec) read off the exported node's project — the
	/// export renders to the project's delivery target, not to the display.
	fn delivery_color(&self) -> (
        oak_core::colormath::WorkingColorSpace,
        oak_core::colormath::OutputColorSpec,
	) {
		let guard = self
			.viewer_node
			.0
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		(guard.working_color_space(), guard.output_color_spec())
	}

	/// Convert an F32 codec frame from the pipeline working space to the
	/// project's delivery colorspace (row-wise — the codec frame rows are
	/// 32-byte aligned, so each row is handled through the byte-based
	/// transform). A no-op for non-F32 frames and in the legacy sRGB
	/// working space.
	fn apply_output_node(&self, frame: &mut oak_codec::frame::Frame) {
		if frame.format() != oak_core::PixelFormat::F32 {
			return;
		}
		let (working, spec) = self.delivery_color();
		if working == oak_core::colormath::WorkingColorSpace::SrgbLegacy {
			return;
		}
		let w = frame.width().max(0) as usize;
		let h = frame.height().max(0) as usize;
		if w == 0 || h == 0 {
			return;
		}
		let row_bytes = w * 16; // F32 RGBA
		let linesize = frame.linesize_bytes() as usize;
		let Some(data) = frame.data_mut() else {
			return;
		};
		for y in 0..h {
			let start = y * linesize;
			if start + row_bytes > data.len() {
				break;
			}
			oak_core::colormath::acescg_to_output_bytes(
				&mut data[start..start + row_bytes],
				w,
				spec,
			);
		}
	}

	/// Copy a rendered `oakrender` texture into an `oakcodec` frame with
	/// the matching video params (row-wise copy — line sizes may differ
	/// between the render and codec frame layouts). A GPU texture is read
	/// back here: the encoder input is one of the three explicit CPU
	/// boundaries (M2).
	fn to_codec_frame(texture: &Texture) -> Result<oak_codec::frame::Frame> {
		let frame = match texture {
			Texture::Cpu(frame) => frame.clone(),
			Texture::Gpu { .. } => texture.to_frame().map_err(|e| {
				Error::Failed(format!("Render frame readback for the encoder failed: {e:?}"))
			})?,
		};
		let frame = &frame;
		let params = CommonVideoParams::new_basic(
            frame.width,
            frame.height,
            oak_core::ocioutils::PixelFormat::from_code(frame.format as i32),
            4,
            1,
            1,
            0,
            1,
		);
		let mut out = oak_codec::frame::Frame::with_params(params);
		out.set_timestamp(frame.timestamp);
		out.allocate().map_err(|e| {
			Error::Failed(format!("Failed to allocate encoder frame: {e:?}"))
		})?;
		let dst_stride = out.linesize_bytes() as usize;
		let Some(dst) = out.data_mut() else {
			return Err(Error::Failed(
				"Encoder frame allocation produced no buffer".to_string(),
			));
		};
		let src = frame.data.as_slice();
		let src_stride = frame.linesize_bytes();
		let row_bytes = std::cmp::min(src_stride, dst_stride)
			.min(src.len())
			.min(dst.len());
		if src_stride == dst_stride && src.len() == dst.len() {
			dst.copy_from_slice(src);
		} else {
			for y in 0..frame.height as usize {
				let src_start = y * src_stride;
				let dst_start = y * dst_stride;
				if src_start + row_bytes > src.len() || dst_start + row_bytes > dst.len() {
					break;
				}
				dst[dst_start..dst_start + row_bytes]
					.copy_from_slice(&src[src_start..src_start + row_bytes]);
			}
		}
		Ok(out)
	}
}

impl TaskBehavior for ExportTask {
	fn run(&mut self, task: &mut Task) -> Result<()> {
		// Share the caller's cancellation atom with the inner render base.
		self.render.base.set_cancel_atom(task.get_cancel_atom());

		let codec_params = self.build_codec_params();
		let Some(encoder) = create_from_params(&codec_params) else {
			task.set_error("Failed to create encoder");
			return Err(Error::Failed("Failed to create encoder".to_string()));
		};
		self.encoder = Some(encoder.clone());

		if let Err(e) = encoder.open() {
			let err = encoder.get_error();
			task.set_error(&format!("Failed to open file: {err}"));
			let _ = e;
			return Err(Error::Failed("Failed to open file".to_string()));
		}

		let export_range = self.export_range();

		// Force params: force the renderer to the requested output size and
		// pixel format (no color-matrix scaling in this simplified path).
		let mut force = ForceParams::default();
		if self.encoding_params.video_enabled {
			force.force_width = self.encoding_params.video_width;
			force.force_height = self.encoding_params.video_height;
			force.force_format = encoder
				.desired_pixel_format()
				.map(|f| f as i32)
				.unwrap_or(-1);
			force.force_channel_count = 4; // RGBA
		}
		self.render.force_params = force;
		self.render.set_render_inputs(
			0, // RenderMode::k_online
			self.encoding_params.audio_enabled,
			export_range,
		);

		// Drive the render with `self` as the subclass behavior (the C++
		// virtual dispatch receiver). The render is temporarily moved out of
		// `self` to avoid a self-referential borrow; it is put back right
		// after, before the encoder flush below.
		let mut render =
			std::mem::replace(&mut self.render, crate::render::RenderTask::placeholder());
		let result = render.render(task, self);
		self.render = render;
		result?;

		// Flush the encoder and surface any trailing error.
		if let Err(_) = encoder.flush() {
			// Fall through to the error read below (the flush error is
			// surfaced through `get_error()` like the C ABI did).
		}
		let err = encoder.get_error();
		if !err.is_empty() {
			task.set_error(&err);
			return Err(Error::Failed("Encoder flush failed".to_string()));
		}

		Ok(())
	}
}

impl RenderTaskBehavior for ExportTask {
	fn frame_downloaded(&mut self, task: &mut Task, frame: &Texture) -> Result<()> {
		let Some(encoder) = &self.encoder else {
			return Ok(());
		};
		let mut codec_frame = Self::to_codec_frame(frame)?;
		// Output node: the rendered frame is in the pipeline working space
		// (ACEScg linear by default); the export converts it to the
		// project's delivery colorspace before encoding. (No-op in the
		// legacy sRGB working space.)
		self.apply_output_node(&mut codec_frame);
		if let Err(_) = encoder.write_video(&codec_frame) {
			let err = encoder.get_error();
			task.set_error(&err);
			return Err(Error::Failed("Failed to write frame".to_string()));
		}
		// Progress is reported by the render loop itself (native signalling).
		Ok(())
	}

	fn audio_downloaded(
		&mut self,
		task: &mut Task,
		samples: &oak_render::ticket::AudioSamples,
	) -> Result<()> {
		let Some(encoder) = &self.encoder else {
			return Ok(());
		};
		let frame_count = if samples.channel_count > 0 {
			samples.samples.len() / samples.channel_count as usize
		} else {
			0
		};
		if let Err(_) = encoder.write_audio(&samples.samples, frame_count as i32) {
			let err = encoder.get_error();
			task.set_error(&err);
			return Err(Error::Failed("Failed to write audio".to_string()));
		}
		Ok(())
	}

	fn encode_subtitle(&mut self, task: &mut Task, text: &str) -> Result<()> {
		if !self.encoding_params.subtitles_enabled {
			return Ok(());
		}
		let Some(encoder) = &self.encoder else {
			return Ok(());
		};
		// The simplified path does not carry the subtitle block's in/out
		// times; write with 0.0/0.0 (the encoder default interval).
		if let Err(_) = encoder.write_subtitle(text, 0.0, 0.0) {
			let err = encoder.get_error();
			task.set_error(&err);
			return Err(Error::Failed("Failed to write subtitle".to_string()));
		}
		Ok(())
	}
}
