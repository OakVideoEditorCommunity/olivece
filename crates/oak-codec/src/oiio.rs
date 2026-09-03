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

//! `OIIODecoder` / `OIIOEncoder` — the OpenImageIO-backed implementations.
//!
//! Mirrors `src/codec/src/oiio/{oiiodecoder,oiioencoder}.{h,cpp}`. OIIO
//! frame conversion goes through the local
//! [`crate::oiioframebridge`] helpers plus oak_core's OIIO mapping
//! functions.
//!
//! The OIIO dylib (`liboakoiio`) is not linked into this build, so every
//! operation that would touch the media engine is a documented stub returning
//! [`crate::error::Error::Failed`]; only bookkeeping that keeps the
//! decoder/encoder safe to use when closed succeeds. The stream is still
//! recorded on [`Decoder::open`] so [`Decoder::stream`] reflects the target,
//! and [`Decoder::close`] / [`Encoder::close`] are no-ops.

use crate::decoder::{CodecStream, Decoder, RetrieveVideoParams};
use crate::encoder::Encoder;
use crate::encodingparams::EncodingParams;

/// `olive::OIIODecoder` — OpenImageIO-backed media decoder (still images).
pub struct OIIODecoder {
	/// Opened stream (locked).
	stream: std::sync::Mutex<Option<CodecStream>>,
}

impl OIIODecoder {
	/// Error returned for operations that need the missing `liboakoiio`.
	const NOT_AVAILABLE: &'static str =
		"OIIO decoding is not available in this build (needs liboakoiio dylib)";
}

impl Decoder for OIIODecoder {
	fn id(&self) -> String {
		"oiio".to_string()
	}

	fn supports_video(&self) -> bool {
		false
	}

	fn supports_audio(&self) -> bool {
		false
	}

	fn probe(
        &self,
        _filename: &str,
        _cancelled: Option<&oak_core::cancelatom::CancelAtom>,
	) -> Option<crate::footagedescription::FootageDescription> {
		// Probing is a dylib operation; without it we cannot report anything.
		None
	}

	fn open(&self, stream: &CodecStream) -> crate::error::Result<()> {
		// Record the requested stream so `stream()` reflects the open target,
		// but actual decoding is unavailable without the dylib.
		*self.stream.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.clone());
		Err(crate::error::Error::Failed(Self::NOT_AVAILABLE.to_string()))
	}

	fn close(&self) -> crate::error::Result<()> {
		// Trait contract: "safe when closed". Clear the opened stream and
		// no-op; nothing was ever decoded.
		*self.stream.lock().unwrap_or_else(|e| e.into_inner()) = None;
		Ok(())
	}

	fn stream(&self) -> CodecStream {
		self.stream
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.unwrap_or_else(CodecStream::new)
	}

	fn retrieve_video_frame(
		&self,
		_p: &RetrieveVideoParams,
	) -> crate::error::Result<std::sync::Arc<crate::frame::Frame>> {
		Err(crate::error::Error::Failed(Self::NOT_AVAILABLE.to_string()))
	}

	fn retrieve_video(
		&self,
		_p: &RetrieveVideoParams,
	) -> crate::error::Result<crate::decoder::OakRenderTexture> {
		Err(crate::error::Error::Failed(Self::NOT_AVAILABLE.to_string()))
	}

	fn retrieve_audio(
		&self,
		_dest: &mut [f32],
		_range: &oak_core::TimeRange,
		_sample_rate: i32,
		_channel_layout: u64,
	) -> crate::error::Result<crate::decoder::RetrieveAudioStatus> {
		Err(crate::error::Error::Failed(Self::NOT_AVAILABLE.to_string()))
	}

	fn conform_audio(
        &self,
        _output_filenames: &[String],
        _sample_rate: i32,
        _channel_layout: u64,
        _sample_format: i32,
        _cancelled: Option<&oak_core::cancelatom::CancelAtom>,
	) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(Self::NOT_AVAILABLE.to_string()))
	}
}

/// `olive::OIIOEncoder` — OpenImageIO-backed media encoder (still images).
pub struct OIIOEncoder {
	/// The encoding parameters this encoder was configured with.
	pub params: EncodingParams,
}

impl Encoder for OIIOEncoder {
	fn id(&self) -> String {
		"oiio".to_string()
	}

	fn supports_video(&self) -> bool {
		false
	}

	fn supports_audio(&self) -> bool {
		false
	}

	fn supports_subtitles(&self) -> bool {
		false
	}

	fn supports_image_sequences(&self) -> bool {
		true
	}

	fn is_configurable(&self) -> bool {
		true
	}

	fn configure(&self, _params: &EncodingParams) -> crate::error::Result<()> {
		// `configure` writes `self.params`, which an `&self` receiver cannot
		// do, and real encoding needs the dylib anyway.
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn open(&self) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn close(&self) -> crate::error::Result<()> {
		// `Encoder::close` is documented idempotent; with no encoder opened
		// there is nothing to release.
		Ok(())
	}

	fn write_video(&self, _frame: &crate::frame::Frame) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn write_audio(&self, _samples: &[f32], _frame_count: i32) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn write_subtitle(
		&self,
		_text: &str,
		_in_seconds: f64,
		_out_seconds: f64,
	) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn flush(&self) -> crate::error::Result<()> {
		Err(crate::error::Error::Failed(
			"OIIO encoding is not available in this build (needs liboakoiio dylib)".to_string(),
		))
	}

	fn desired_pixel_format(&self) -> Option<oak_core::PixelFormat> {
		None
	}

	fn desired_sample_format(&self) -> Option<oak_core::SampleFormat> {
		None
	}

	fn filename(&self) -> String {
		c_string_1024(&self.params.filename)
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
	use crate::encodingparams::EncodingParams;
	use oak_core::{Rational, TimeRange};

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
	fn oiio_decoder_identity_and_stub_operations() {
		let d = OIIODecoder {
			stream: std::sync::Mutex::new(None),
		};
		assert_eq!(d.id(), "oiio");
		// Still images only: no audio support.
		assert!(!d.supports_video());
		assert!(!d.supports_audio());
		assert!(d.probe("any.png", None).is_none());

		let s = CodecStream::with_block("in.exr".to_string(), 0, None);
		assert!(d.open(&s).is_err());
		assert_eq!(d.stream().filename(), "in.exr");
		assert!(d.close().is_ok());
		assert_eq!(d.stream().filename(), "");

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
	}

	#[test]
	fn oiio_encoder_stub_behaviors() {
		let mut params = EncodingParams::default();
		let name = b"out/img.exr";
		params.filename[..name.len()].copy_from_slice(name);
		let e = OIIOEncoder { params };

		assert_eq!(e.id(), "oiio");
		assert!(!e.supports_video());
		assert!(!e.supports_audio());
		assert!(!e.supports_subtitles());
		assert!(e.supports_image_sequences());
		assert!(e.is_configurable());
		assert_eq!(e.filename(), "out/img.exr");
		assert_eq!(e.desired_pixel_format(), None);
		assert_eq!(e.get_error(), "");

		assert!(e.configure(&EncodingParams::default()).is_err());
		assert!(e.open().is_err());
		assert!(e.close().is_ok());
		let frame = crate::frame::Frame::new();
		assert!(e.write_video(&frame).is_err());
		assert!(e.write_audio(&[0f32; 4], 1).is_err());
		assert!(e.write_subtitle("hi", 0.0, 1.0).is_err());
		assert!(e.flush().is_err());
	}
}
