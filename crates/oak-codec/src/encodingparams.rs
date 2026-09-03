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

//! `olive::EncodingParams` — export encoding parameters.
//!
//! Mirrors the `EncodingParams` class carried in `src/codec/src/encoder.h`
//! and the flattened `oakcodec_encoding_params` POD in
//! `include/codec/encoder.h`. The Rust struct mirrors the POD verbatim so
//! `ffi.rs` can marshal it without translation.

use oak_core::{PixelFormat, SampleFormat};

/// `VideoParams::Interlacing` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum VideoScalingMethod {
	/// Fit the source into the destination, preserving aspect.
	Fit = 0,
	/// Stretch the source to the destination.
	Stretch = 1,
	/// Crop the source to the destination.
	Crop = 2,
}

/// `olive::EncodingParams` — flattened mirror of `oakcodec_encoding_params`.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct EncodingParams {
	/// Output filename (or image-sequence "[#####]" template).
	pub filename: [u8; 1024],
	/// `ExportFormat::Format`.
	pub format: i32,

	/// Video track enabled (1/0; C `int`).
	pub video_enabled: i32,
	/// `ExportCodec::Codec`.
	pub video_codec: i32,
	/// Output width.
	pub video_width: i32,
	/// Output height.
	pub video_height: i32,
	/// Frame duration (rational): numerator.
	pub video_time_base_num: i32,
	/// Frame duration (rational): denominator.
	pub video_time_base_den: i32,
	/// Delivery `OakPixelFormat`.
	pub video_pixel_format: PixelFormat,
	/// Interlacing (`Interlacing` value).
	pub video_interlacing: i32,
	/// Pixel aspect ratio numerator.
	pub video_pixel_aspect_num: i32,
	/// Pixel aspect ratio denominator.
	pub video_pixel_aspect_den: i32,
	/// Bit rate (bit/s), 0 = codec default.
	pub video_bit_rate: i64,
	/// Min bit rate.
	pub video_min_bit_rate: i64,
	/// Max bit rate.
	pub video_max_bit_rate: i64,
	/// Buffer size (bytes).
	pub video_buffer_size: i64,
	/// Encoding threads (0 = auto).
	pub video_threads: i32,
	/// Encoded pixel format name (e.g. "yuv420p").
	pub video_pix_fmt: [u8; 64],
	/// Image sequence output (1/0).
	pub video_is_image_sequence: i32,
	/// Scaling method (`VideoScalingMethod`).
	pub video_scaling_method: VideoScalingMethod,

	/// Audio track enabled (1/0).
	pub audio_enabled: i32,
	/// `ExportCodec::Codec`.
	pub audio_codec: i32,
	/// Audio sample rate.
	pub audio_sample_rate: i32,
	/// ffmpeg-style channel layout mask.
	pub audio_channel_layout: u64,
	/// `SampleFormat`.
	pub audio_sample_format: SampleFormat,
	/// Audio bit rate.
	pub audio_bit_rate: i64,

	/// Subtitles track enabled (1/0).
	pub subtitles_enabled: i32,
	/// `ExportCodec::Codec`.
	pub subtitles_codec: i32,
	/// Subtitles written as a sidecar file (1/0).
	pub subtitles_are_sidecar: i32,
	/// Sidecar format (`ExportFormat::Format`).
	pub subtitles_sidecar_format: i32,

	/// Output OCIO colorspace name; empty = reference space (no transform).
	pub color_transform_output: [u8; 256],

	/// Export length in seconds (rational).
	pub export_length_num: i32,
	/// Export length in seconds (rational).
	pub export_length_den: i32,

	/// Custom export range enabled (1/0).
	pub has_custom_range: i32,
	/// Custom range in, rational seconds.
	pub custom_range_in_num: i64,
	/// Custom range in denominator.
	pub custom_range_in_den: i64,
	/// Custom range out, rational seconds.
	pub custom_range_out_num: i64,
	/// Custom range out denominator.
	pub custom_range_out_den: i64,

	/// Delivery color metadata written into the output container
	/// (ISO/IEC 23001-8 / H.273 code points — the same numbering FFmpeg's
	/// `AVCodecContext` uses; 0 = leave unset). The export sets these from
	/// the project's output colorspace so the file declares its
	/// colorimetry (the mov `colr` atom / H.264-HEVC VUI) instead of
	/// leaving players to guess.
	pub color_primaries: i32,
	/// Delivery transfer characteristic code point (`AVCOL_TRC_*`).
	pub color_trc: i32,
	/// Delivery matrix coefficients code point (`AVCOL_SPC_*`; 0 = RGB).
	pub color_space: i32,
	/// Delivery color range (`AVCOL_RANGE_*`: 1 = limited, 2 = full).
	pub color_range: i32,
}

impl Default for EncodingParams {
	fn default() -> Self {
		Self {
			filename: [0; 1024],
			format: -1,

			video_enabled: 0,
			video_codec: -1,
			video_width: 0,
			video_height: 0,
			video_time_base_num: 1,
			video_time_base_den: 1,
			video_pixel_format: PixelFormat::Invalid,
			video_interlacing: 0,
			video_pixel_aspect_num: 0,
			video_pixel_aspect_den: 0,
			video_bit_rate: 0,
			video_min_bit_rate: 0,
			video_max_bit_rate: 0,
			video_buffer_size: 0,
			video_threads: 0,
			video_pix_fmt: [0; 64],
			video_is_image_sequence: 0,
			video_scaling_method: VideoScalingMethod::Stretch,

			audio_enabled: 0,
			audio_codec: -1,
			audio_sample_rate: 0,
			audio_channel_layout: 0,
			audio_sample_format: SampleFormat::Invalid,
			audio_bit_rate: 0,

			subtitles_enabled: 0,
			subtitles_codec: -1,
			subtitles_are_sidecar: 0,
			subtitles_sidecar_format: -1,

			color_transform_output: [0; 256],

			export_length_num: 0,
			export_length_den: 0,

			has_custom_range: 0,
			custom_range_in_num: 0,
			custom_range_in_den: 0,
			custom_range_out_num: 0,
			custom_range_out_den: 0,

			color_primaries: 0,
			color_trc: 0,
			color_space: 0,
			color_range: 0,
		}
	}
}

impl EncodingParams {
	/// Scaling matrix for a scaling method; row-major 4x4 into `out[16]`.
	///
	/// # CPP-PARITY
	/// `src/codec/src/encoder.cpp` `EncodingParams::generate_matrix` —
	/// returns a row-major `std::array<float,16>` (formerly QMatrix4x4).
	pub fn generate_matrix(
		method: VideoScalingMethod,
		src_width: i32,
		src_height: i32,
		dst_width: i32,
		dst_height: i32,
		out: &mut [f64; 16],
	) {
		// Identity (former default-constructed QMatrix4x4), row-major.
		*out = [
			1.0, 0.0, 0.0, 0.0, //
			0.0, 1.0, 0.0, 0.0, //
			0.0, 0.0, 1.0, 0.0, //
			0.0, 0.0, 0.0, 1.0,
		];

		if method == VideoScalingMethod::Stretch {
			return;
		}

		// Guard degenerate sizes: the C++ would produce inf/NaN here; an
		// identity is the only safe output for the rendering pipeline.
		if src_width <= 0 || src_height <= 0 || dst_width <= 0 || dst_height <= 0 {
			return;
		}

		let export_ar = dst_width as f64 / dst_height as f64;
		let source_ar = src_width as f64 / src_height as f64;

		// qFuzzyCompare(export_ar, source_ar): within one part in 100000.
		if (export_ar - source_ar).abs() * 100000.0 <= export_ar.abs().min(source_ar.abs()) {
			return;
		}

		if (export_ar > source_ar) == (method == VideoScalingMethod::Fit) {
			// scale(source_ar / export_ar, 1)
			out[0] = source_ar / export_ar;
		} else {
			// scale(1, export_ar / source_ar)
			out[5] = export_ar / source_ar;
		}
	}

	/// File extension for this format (e.g. "mp4").
	///
	/// # CPP-PARITY
	/// `ExportFormat::get_extension` (`src/codec/src/exportformat.cpp`) keyed
	/// on the serialized `Format` discriminant. Unknown / invalid formats
	/// return the empty string, matching the C++ default case.
	pub fn extension(&self) -> &str {
		match self.format {
			0 => "mxf",   // DNxHD
			1 => "mkv",   // Matroska
			2 => "mp4",   // MPEG-4 video
			3 => "exr",   // OpenEXR
			4 => "mov",   // QuickTime
			5 => "png",   // PNG
			6 => "tiff",  // TIFF
			7 => "wav",   // WAV
			8 => "aiff",  // AIFF
			9 => "mp3",   // MP3
			10 => "flac", // FLAC
			11 => "ogg",  // Ogg
			12 => "webm", // WebM
			13 => "srt",  // SRT
			14 => "m4a",  // MPEG-4 audio
			_ => "",
		}
	}

	/// Load from a compact XML preset string (oak_core C++ XmlStreamReader).
	///
	/// # CPP-PARITY
	/// `EncodingParams::load` — uses oak_core's C++ `XmlStreamReader`
	/// (`src/common/src/xmlutils.h`), a C++-to-C++ coupling the bridge
	/// cannot cover (NOTES.md §7). Preserves the load_v1 bug of not
	/// assigning `custom_range`.
	pub fn load(&mut self, data: &str) -> crate::error::Result<()> {
		let root = parse_preset(data)
			.map_err(|e| crate::error::Error::Failed(format!("invalid export preset: {e}")))?;
		if root.name != "export" {
			return Err(crate::error::Error::Failed(
				"not an export preset document".to_string(),
			));
		}

		// `range` / `customrangein` / `customrangeout` are parsed for shape
		// parity but never assigned — mirroring the C++ `load_v1` bug that
		// reads them into locals and forgets to store `custom_range`.
		if let Some(e) = child(&root, "range") {
			let _ = parse_bool(&text(e));
		}
		if let Some(e) = child(&root, "customrangein") {
			let _ = &text(e);
		}
		if let Some(e) = child(&root, "customrangeout") {
			let _ = &text(e);
		}

		if let Some(e) = child(&root, "filename") {
			set_cstr(&mut self.filename, &text(e));
		}
		if let Some(e) = child(&root, "format") {
			self.format = parse_i32(&text(e));
		}

		if let Some(e) = child(&root, "video") {
			if let Some(v) = attr(e, "enabled") {
				self.video_enabled = parse_bool(v) as i32;
			}
			if let Some(e) = child(e, "codec") {
				self.video_codec = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "width") {
				self.video_width = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "height") {
				self.video_height = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "format") {
				self.video_pixel_format = match parse_i32(&text(e)) {
					-1 => PixelFormat::Invalid,
					0 => PixelFormat::U8,
					1 => PixelFormat::U10,
					2 => PixelFormat::U16,
					3 => PixelFormat::F16,
					4 => PixelFormat::F32,
					_ => PixelFormat::Invalid,
				};
			}
			if let Some(e) = child(e, "timebase") {
				let tb = text(e);
				if let Some((n, d)) = tb.split_once('/') {
					self.video_time_base_num = parse_i32(n);
					self.video_time_base_den = parse_i32(d);
				}
			}
			if let Some(e) = child(e, "divider") {
				self.video_interlacing = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "pixelaspect") {
				let par = text(e);
				if let Some((n, d)) = par.split_once('/') {
					self.video_pixel_aspect_num = parse_i32(n);
					self.video_pixel_aspect_den = parse_i32(d);
				}
			}
			if let Some(e) = child(e, "bitrate") {
				self.video_bit_rate = parse_i64(&text(e));
			}
			if let Some(e) = child(e, "minbitrate") {
				self.video_min_bit_rate = parse_i64(&text(e));
			}
			if let Some(e) = child(e, "maxbitrate") {
				self.video_max_bit_rate = parse_i64(&text(e));
			}
			if let Some(e) = child(e, "bufsize") {
				self.video_buffer_size = parse_i64(&text(e));
			}
			if let Some(e) = child(e, "threads") {
				self.video_threads = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "pixfmt") {
				set_cstr(&mut self.video_pix_fmt, &text(e));
			}
			if let Some(e) = child(e, "imgseq") {
				self.video_is_image_sequence = parse_bool(&text(e)) as i32;
			}
			if let Some(e) = child(e, "vscale") {
				self.video_scaling_method = scaling_from_i32(parse_i32(&text(e)));
			}
		}

		if let Some(e) = child(&root, "audio") {
			if let Some(v) = attr(e, "enabled") {
				self.audio_enabled = parse_bool(v) as i32;
			}
			if let Some(e) = child(e, "codec") {
				self.audio_codec = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "samplerate") {
				self.audio_sample_rate = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "channellayout") {
				self.audio_channel_layout = parse_i64(&text(e)) as u64;
			}
			if let Some(e) = child(e, "format") {
				self.audio_sample_format = sample_format_from_i32(parse_i32(&text(e)));
			}
			if let Some(e) = child(e, "bitrate") {
				self.audio_bit_rate = parse_i64(&text(e));
			}
		}

		if let Some(e) = child(&root, "subtitles") {
			if let Some(v) = attr(e, "enabled") {
				self.subtitles_enabled = parse_bool(v) as i32;
			}
			if let Some(e) = child(e, "sidecar") {
				self.subtitles_are_sidecar = parse_bool(&text(e)) as i32;
			}
			if let Some(e) = child(e, "sidecarformat") {
				self.subtitles_sidecar_format = parse_i32(&text(e));
			}
			if let Some(e) = child(e, "codec") {
				self.subtitles_codec = parse_i32(&text(e));
			}
		}

		Ok(())
	}

	/// Serialize to a compact XML string (no declaration, no indentation;
	/// element/attribute names and order preserved).
	///
	/// # CPP-PARITY
	/// The C++ writer also emits per-codec `video_opts_`; those moved out
	/// of the byte-exact `EncodingParams` POD (see `encoding_params_c_abi_layout`),
	/// so the options are not serialized here.
	pub fn save_to_string(&self) -> String {
		let mut s = String::new();
		s.push_str("<export version=\"1\">");
		s.push_str(&format!(
			"<filename>{}</filename>",
			escape_xml(cstr(&self.filename))
		));
		s.push_str(&format!("<format>{}</format>", self.format));
		s.push_str(&format!("<range>{}</range>", self.has_custom_range));
		s.push_str(&format!(
			"<customrangein>{}/{}</customrangein>",
			self.custom_range_in_num, self.custom_range_in_den
		));
		s.push_str(&format!(
			"<customrangeout>{}/{}</customrangeout>",
			self.custom_range_out_num, self.custom_range_out_den
		));

		s.push_str(&format!("<video enabled=\"{}\">", self.video_enabled));
		if self.video_enabled != 0 {
			s.push_str(&format!("<codec>{}</codec>", self.video_codec));
			s.push_str(&format!("<width>{}</width>", self.video_width));
			s.push_str(&format!("<height>{}</height>", self.video_height));
			s.push_str(&format!(
				"<format>{}</format>",
				self.video_pixel_format as i32
			));
			s.push_str(&format!(
				"<timebase>{}/{}</timebase>",
				self.video_time_base_num, self.video_time_base_den
			));
			s.push_str(&format!("<divider>{}</divider>", self.video_interlacing));
			s.push_str(&format!(
				"<pixelaspect>{}/{}</pixelaspect>",
				self.video_pixel_aspect_num, self.video_pixel_aspect_den
			));
			s.push_str(&format!("<bitrate>{}</bitrate>", self.video_bit_rate));
			s.push_str(&format!(
				"<minbitrate>{}</minbitrate>",
				self.video_min_bit_rate
			));
			s.push_str(&format!(
				"<maxbitrate>{}</maxbitrate>",
				self.video_max_bit_rate
			));
			s.push_str(&format!("<bufsize>{}</bufsize>", self.video_buffer_size));
			s.push_str(&format!("<threads>{}</threads>", self.video_threads));
			s.push_str(&format!(
				"<pixfmt>{}</pixfmt>",
				escape_xml(cstr(&self.video_pix_fmt))
			));
			s.push_str(&format!(
				"<imgseq>{}</imgseq>",
				self.video_is_image_sequence
			));
			s.push_str(&format!(
				"<vscale>{}</vscale>",
				self.video_scaling_method as i32
			));
		}
		s.push_str("</video>");

		s.push_str(&format!("<audio enabled=\"{}\">", self.audio_enabled));
		if self.audio_enabled != 0 {
			s.push_str(&format!("<codec>{}</codec>", self.audio_codec));
			s.push_str(&format!(
				"<samplerate>{}</samplerate>",
				self.audio_sample_rate
			));
			s.push_str(&format!(
				"<channellayout>{}</channellayout>",
				self.audio_channel_layout
			));
			s.push_str(&format!(
				"<format>{}</format>",
				self.audio_sample_format as i32
			));
			s.push_str(&format!("<bitrate>{}</bitrate>", self.audio_bit_rate));
		}
		s.push_str("</audio>");

		s.push_str(&format!(
			"<subtitles enabled=\"{}\">",
			self.subtitles_enabled
		));
		if self.subtitles_enabled != 0 {
			s.push_str(&format!(
				"<sidecar>{}</sidecar>",
				self.subtitles_are_sidecar
			));
			s.push_str(&format!(
				"<sidecarformat>{}</sidecarformat>",
				self.subtitles_sidecar_format
			));
			s.push_str(&format!("<codec>{}</codec>", self.subtitles_codec));
		}
		s.push_str("</subtitles>");

		s.push_str("</export>");
		s
	}
}

// ---------------------------------------------------------------------------
// Minimal XML helpers for the round-trip `load`/`save_to_string`.
//
// CPP-PARITY: the C++ `load`/`save_to_string` go through oak_core's
// `XmlStreamReader`/`XmlStreamWriter` (a C++-to-C++ coupling the Rust bridge
// cannot cover, NOTES.md §7). Rather than returning `Err`, this port keeps a
// minimal but faithful round-trip for the fields representable without the
// videoparams/colortransform/audio bridge. Deviations vs the C++ writer:
//   * the `<video>` width/height/format are taken from the Rust fields
//     directly (the C++ reads them out of a live `OakVideoParams` handle);
//   * `<timebase>`, `<divider>`, `<pixelaspect>` mirror the corresponding
//     flattened fields;
//   * `customrangein`/`customrangeout` are written but, on `load`, parsed and
//     discarded — preserving the C++ `load_v1` bug that never assigns
//     `custom_range`.
// ---------------------------------------------------------------------------

/// Minimal element of the round-trip XML tree.
struct El {
	name: String,
	attrs: Vec<(String, String)>,
	text: Option<String>,
	children: Vec<El>,
}

/// Positional cursor over the XML text.
struct Cursor<'a> {
	s: &'a str,
	i: usize,
}

impl<'a> Cursor<'a> {
	fn peek(&self) -> Option<char> {
		self.s[self.i..].chars().next()
	}

	fn skip_ws(&mut self) {
		while self.peek().map_or(false, char::is_whitespace) {
			self.i += 1;
		}
	}

	fn starts_with(&self, p: &str) -> bool {
		self.s[self.i..].starts_with(p)
	}

	fn read_name(&mut self) -> Result<String, String> {
		let start = self.i;
		while let Some(ch) = self.peek() {
			if ch.is_alphanumeric() || ch == '_' || ch == '-' {
				self.i += ch.len_utf8();
			} else {
				break;
			}
		}
		if self.i == start {
			return Err("expected element/attribute name".to_string());
		}
		Ok(self.s[start..self.i].to_string())
	}

	fn read_attr_value(&mut self) -> Result<String, String> {
		self.skip_ws();
		if !self.starts_with("\"") {
			return Err("expected quoted attribute value".to_string());
		}
		self.i += 1;
		let start = self.i;
		while self.i < self.s.len() && !self.starts_with("\"") {
			self.i += 1;
		}
		if self.i >= self.s.len() {
			return Err("unterminated attribute value".to_string());
		}
		let v = self.s[start..self.i].to_string();
		self.i += 1;
		Ok(v)
	}

	fn expect_end(&mut self, name: &str) -> Result<(), String> {
		self.skip_ws();
		if !self.starts_with("</") {
			return Err(format!("expected </{}>", name));
		}
		self.i += 2;
		let n = self.read_name()?;
		self.skip_ws();
		if !self.starts_with(">") {
			return Err("expected '>' closing end tag".to_string());
		}
		self.i += 1;
		if n != name {
			return Err(format!("mismatched end tag {} vs {}", n, name));
		}
		Ok(())
	}

	fn parse_element(&mut self) -> Result<El, String> {
		self.skip_ws();
		if !self.starts_with("<") {
			return Err("expected element".to_string());
		}
		self.i += 1;
		let name = self.read_name()?;
		self.skip_ws();
		let mut attrs = Vec::new();
		loop {
			self.skip_ws();
			if self.starts_with("/>") {
				self.i += 2;
				return Ok(El {
					name,
					attrs,
					text: None,
					children: Vec::new(),
				});
			}
			if self.starts_with(">") {
				self.i += 1;
				break;
			}
			let an = self.read_name()?;
			self.skip_ws();
			if self.starts_with("=") {
				self.i += 1;
			}
			let v = self.read_attr_value()?;
			attrs.push((an, v));
		}
		// Content: either nested elements or a text node.
		self.skip_ws();
		if self.i < self.s.len() && self.starts_with("<") {
			let mut children = Vec::new();
			loop {
				self.skip_ws();
				if self.i >= self.s.len() {
					return Err("unexpected EOF in element".to_string());
				}
				if self.starts_with("</") {
					break;
				}
				if self.starts_with("<") {
					children.push(self.parse_element()?);
				} else {
					return Err("unexpected text at element level".to_string());
				}
			}
			self.expect_end(&name)?;
			Ok(El {
				name,
				attrs,
				text: None,
				children,
			})
		} else {
			let start = self.i;
			while self.i < self.s.len() && !self.starts_with("<") {
				self.i += 1;
			}
			let text = self.s[start..self.i].to_string();
			self.expect_end(&name)?;
			Ok(El {
				name,
				attrs,
				text: Some(text),
				children: Vec::new(),
			})
		}
	}
}

/// Parse the single root element of a preset document.
fn parse_preset(data: &str) -> Result<El, String> {
	let mut c = Cursor { s: data, i: 0 };
	c.parse_element()
}

fn child<'a>(el: &'a El, name: &str) -> Option<&'a El> {
	el.children.iter().find(|c| c.name == name)
}

fn text(el: &El) -> String {
	unescape_xml(el.text.as_deref().unwrap_or(""))
}

fn attr<'a>(el: &'a El, name: &str) -> Option<&'a str> {
	el.attrs
		.iter()
		.find(|(k, _)| k == name)
		.map(|(_, v)| v.as_str())
}

fn parse_i32(s: &str) -> i32 {
	s.trim().parse().unwrap_or(0)
}

fn parse_i64(s: &str) -> i64 {
	s.trim().parse().unwrap_or(0)
}

fn parse_bool(s: &str) -> bool {
	parse_i32(s) != 0
}

/// Convert an `i32` code to a [`VideoScalingMethod`]; unknown -> `Stretch`
/// (the C++ default). `pub(crate)` so the ffi encoder layer can map the
/// `oakcodec_encoding_params.video_scaling_method` field.
pub(crate) fn scaling_from_i32(v: i32) -> VideoScalingMethod {
	match v {
		0 => VideoScalingMethod::Fit,
		1 => VideoScalingMethod::Stretch,
		2 => VideoScalingMethod::Crop,
		_ => VideoScalingMethod::Stretch,
	}
}

/// Convert an `i32` code to a [`SampleFormat`]; unknown -> `Invalid`.
/// `pub(crate)` so the ffi encoder layer can map the
/// `oakcodec_encoding_params.audio_sample_format` field.
pub(crate) fn sample_format_from_i32(v: i32) -> SampleFormat {
	match v {
		-1 => SampleFormat::Invalid,
		0 => SampleFormat::U8Planar,
		1 => SampleFormat::S16Planar,
		2 => SampleFormat::S32Planar,
		3 => SampleFormat::S64Planar,
		4 => SampleFormat::F32Planar,
		5 => SampleFormat::F64Planar,
		6 => SampleFormat::U8,
		7 => SampleFormat::S16,
		8 => SampleFormat::S32,
		9 => SampleFormat::S64,
		10 => SampleFormat::F32,
		11 => SampleFormat::F64,
		_ => SampleFormat::Invalid,
	}
}

/// NUL-terminated byte array -> `&str` (empty when the first byte is NUL).
fn cstr(arr: &[u8]) -> &str {
	let end = arr.iter().position(|&b| b == 0).unwrap_or(arr.len());
	std::str::from_utf8(&arr[..end]).unwrap_or("")
}

/// Write a string into a NUL-padded byte array (truncating if too long).
fn set_cstr(dst: &mut [u8], s: &str) {
	dst.fill(0);
	let n = s.len().min(dst.len());
	dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// Minimal XML escaping for text content.
fn escape_xml(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

/// Minimal XML unescaping for text content.
fn unescape_xml(s: &str) -> String {
	s.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
	use super::*;

	const EPS: f64 = 1e-9;

	fn assert_close(a: f64, b: f64) {
		assert!((a - b).abs() < EPS, "expected {a} close to {b} (eps {EPS})");
	}

	#[test]
	fn default_has_expected_values() {
		let p = EncodingParams::default();
		assert_eq!(p.filename, [0; 1024]);
		assert_eq!(p.format, -1);
		assert_eq!(p.video_enabled, 0);
		assert_eq!(p.video_codec, -1);
		assert_eq!(p.video_width, 0);
		assert_eq!(p.video_height, 0);
		assert_eq!(p.video_time_base_num, 1);
		assert_eq!(p.video_time_base_den, 1);
		assert_eq!(p.video_pixel_format, PixelFormat::Invalid);
		assert_eq!(p.video_interlacing, 0);
		assert_eq!(p.video_pixel_aspect_num, 0);
		assert_eq!(p.video_pixel_aspect_den, 0);
		assert_eq!(p.video_bit_rate, 0);
		assert_eq!(p.video_min_bit_rate, 0);
		assert_eq!(p.video_max_bit_rate, 0);
		assert_eq!(p.video_buffer_size, 0);
		assert_eq!(p.video_threads, 0);
		assert_eq!(p.video_pix_fmt, [0; 64]);
		assert_eq!(p.video_is_image_sequence, 0);
		assert_eq!(p.video_scaling_method, VideoScalingMethod::Stretch);
		assert_eq!(p.audio_enabled, 0);
		assert_eq!(p.audio_codec, -1);
		assert_eq!(p.audio_sample_rate, 0);
		assert_eq!(p.audio_channel_layout, 0);
		assert_eq!(p.audio_sample_format, SampleFormat::Invalid);
		assert_eq!(p.audio_bit_rate, 0);
		assert_eq!(p.subtitles_enabled, 0);
		assert_eq!(p.subtitles_codec, -1);
		assert_eq!(p.subtitles_are_sidecar, 0);
		assert_eq!(p.subtitles_sidecar_format, -1);
		assert_eq!(p.color_transform_output, [0; 256]);
		assert_eq!(p.export_length_num, 0);
		assert_eq!(p.export_length_den, 0);
		assert_eq!(p.has_custom_range, 0);
		assert_eq!(p.custom_range_in_num, 0);
		assert_eq!(p.custom_range_in_den, 0);
		assert_eq!(p.custom_range_out_num, 0);
		assert_eq!(p.custom_range_out_den, 0);
	}

	#[test]
	fn extension_mapping() {
		let mut p = EncodingParams::default();
		let cases: [(i32, &str); 15] = [
			(0, "mxf"),
			(1, "mkv"),
			(2, "mp4"),
			(3, "exr"),
			(4, "mov"),
			(5, "png"),
			(6, "tiff"),
			(7, "wav"),
			(8, "aiff"),
			(9, "mp3"),
			(10, "flac"),
			(11, "ogg"),
			(12, "webm"),
			(13, "srt"),
			(14, "m4a"),
		];
		for (fmt, ext) in cases {
			p.format = fmt;
			assert_eq!(p.extension(), ext, "format {fmt}");
		}
		// Unknown / invalid formats map to the empty string.
		p.format = 99;
		assert_eq!(p.extension(), "");
		p.format = -3;
		assert_eq!(p.extension(), "");
	}

	#[test]
	fn generate_matrix_stretch_is_identity() {
		let mut out = [0.0; 16];
		EncodingParams::generate_matrix(
			VideoScalingMethod::Stretch,
			1920,
			1080,
			1280,
			720,
			&mut out,
		);
		assert_eq!(out, identity());
	}

	#[test]
	fn generate_matrix_equal_aspect_is_identity() {
		// Same 16:9 aspect: export_ar == source_ar -> fuzzy-equal -> identity.
		let mut out = [0.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Fit, 1920, 1080, 1280, 720, &mut out);
		assert_eq!(out, identity());
	}

	#[test]
	fn generate_matrix_fit_scales_x() {
		// src square (ar 1.0) -> dst 2:1 (ar 2.0). Fit: source wider/narrower
		// relative to export -> the x axis is squeezed to source_ar/export_ar.
		let mut out = [0.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Fit, 1000, 1000, 2000, 1000, &mut out);
		// export_ar(2.0) > source_ar(1.0); Fit => scale(source_ar/export_ar, 1).
		assert_close(out[0], 0.5);
		assert_close(out[5], 1.0);
	}

	#[test]
	fn generate_matrix_crop_scales_y() {
		// src square -> dst 2:1. Crop: fit inside, so we zoom the y axis by
		// export_ar/source_ar and keep x unscaled.
		let mut out = [0.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Crop, 1000, 1000, 2000, 1000, &mut out);
		// export_ar(2.0) > source_ar(1.0); not Fit => scale(1, export_ar/source_ar).
		assert_close(out[0], 1.0);
		assert_close(out[5], 2.0);
	}

	#[test]
	fn generate_matrix_crop_scales_x() {
		// src square -> dst 0.5:1 (ar 0.5). Crop: export narrower than source,
		// zoom the x axis by source_ar/export_ar.
		let mut out = [0.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Crop, 1000, 1000, 500, 1000, &mut out);
		// export_ar(0.5) < source_ar(1.0); (ar>source) == false, (method==Fit)
		// false => false == false true => scale(source_ar/export_ar, 1) = 2.0.
		assert_close(out[0], 2.0);
		assert_close(out[5], 1.0);
	}

	#[test]
	fn generate_matrix_degenerate_is_identity() {
		// Zero / negative source sizes must not produce inf/NaN.
		let mut out = [9.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Fit, 0, 1080, 1280, 720, &mut out);
		assert_eq!(out, identity());

		let mut out = [9.0; 16];
		EncodingParams::generate_matrix(VideoScalingMethod::Crop, -1, 1080, 1280, 720, &mut out);
		assert_eq!(out, identity());
	}

	#[test]
	fn save_load_roundtrip() {
		let mut p = EncodingParams::default();
		p.filename = [0; 1024];
		let name = b"out/video.mp4";
		p.filename[..name.len()].copy_from_slice(name);
		p.format = 2;
		p.video_enabled = 1;
		p.video_codec = 3;
		p.video_width = 1280;
		p.video_height = 720;
		p.video_time_base_num = 1;
		p.video_time_base_den = 30;
		p.video_pixel_format = PixelFormat::U8;
		p.video_interlacing = 1;
		p.video_pixel_aspect_num = 1;
		p.video_pixel_aspect_den = 1;
		p.video_bit_rate = 8_000_000;
		p.video_min_bit_rate = 4_000_000;
		p.video_max_bit_rate = 16_000_000;
		p.video_buffer_size = 4_000_000;
		p.video_threads = 4;
		let pix = b"yuv420p";
		p.video_pix_fmt[..pix.len()].copy_from_slice(pix);
		p.video_is_image_sequence = 1;
		p.video_scaling_method = VideoScalingMethod::Crop;

		p.audio_enabled = 1;
		p.audio_codec = 1;
		p.audio_sample_rate = 48000;
		p.audio_channel_layout = 3; // stereo mask
		p.audio_sample_format = SampleFormat::F32Planar;
		p.audio_bit_rate = 320_000;

		p.subtitles_enabled = 1;
		p.subtitles_codec = 1;
		p.subtitles_are_sidecar = 1;
		p.subtitles_sidecar_format = 13; // srt

		let xml = p.save_to_string();
		assert!(xml.starts_with("<export version=\"1\">"), "xml: {xml}");

		let mut q = EncodingParams::default();
		q.load(&xml).expect("load round-trip");

		assert_eq!(q.filename[..name.len()], name[..]);
		assert_eq!(q.format, 2);
		assert_eq!(q.video_enabled, 1);
		assert_eq!(q.video_codec, 3);
		assert_eq!(q.video_width, 1280);
		assert_eq!(q.video_height, 720);
		assert_eq!(q.video_time_base_num, 1);
		assert_eq!(q.video_time_base_den, 30);
		assert_eq!(q.video_pixel_format, PixelFormat::U8);
		assert_eq!(q.video_interlacing, 1);
		assert_eq!(q.video_pixel_aspect_num, 1);
		assert_eq!(q.video_pixel_aspect_den, 1);
		assert_eq!(q.video_bit_rate, 8_000_000);
		assert_eq!(q.video_min_bit_rate, 4_000_000);
		assert_eq!(q.video_max_bit_rate, 16_000_000);
		assert_eq!(q.video_buffer_size, 4_000_000);
		assert_eq!(q.video_threads, 4);
		assert_eq!(q.video_pix_fmt[..pix.len()], pix[..]);
		assert_eq!(q.video_is_image_sequence, 1);
		assert_eq!(q.video_scaling_method, VideoScalingMethod::Crop);

		assert_eq!(q.audio_enabled, 1);
		assert_eq!(q.audio_codec, 1);
		assert_eq!(q.audio_sample_rate, 48000);
		assert_eq!(q.audio_channel_layout, 3);
		assert_eq!(q.audio_sample_format, SampleFormat::F32Planar);
		assert_eq!(q.audio_bit_rate, 320_000);

		assert_eq!(q.subtitles_enabled, 1);
		assert_eq!(q.subtitles_codec, 1);
		assert_eq!(q.subtitles_are_sidecar, 1);
		assert_eq!(q.subtitles_sidecar_format, 13);
	}

	#[test]
	fn load_malformed_returns_err() {
		let mut p = EncodingParams::default();
		assert!(p.load("not xml at all").is_err());
		assert!(p.load("<unclosed").is_err());
		assert!(p.load("<export><video enabled=\"1\"></export>").is_err());
	}

	#[test]
	fn load_non_export_returns_err() {
		let mut p = EncodingParams::default();
		assert!(p.load("<foo/>").is_err());
	}

	#[test]
	fn custom_range_not_assigned_on_load() {
		// The C++ load_v1 bug: custom range is parsed but never assigned.
		let mut p = EncodingParams::default();
		p.has_custom_range = 1;
		p.custom_range_in_num = 5;
		p.custom_range_in_den = 1;
		p.custom_range_out_num = 10;
		p.custom_range_out_den = 1;
		let xml = p.save_to_string();

		let mut q = EncodingParams::default();
		q.load(&xml).expect("load");
		// Fields stay at their defaults despite being serialized.
		assert_eq!(q.has_custom_range, 0);
		assert_eq!(q.custom_range_in_num, 0);
		assert_eq!(q.custom_range_in_den, 0);
		assert_eq!(q.custom_range_out_num, 0);
		assert_eq!(q.custom_range_out_den, 0);
	}

	#[test]
	fn cstr_set_cstr_roundtrip_and_truncate() {
		let mut arr = [0u8; 8];
		set_cstr(&mut arr, "hello");
		assert_eq!(cstr(&arr), "hello");
		assert_eq!(arr[5], 0);

		// Longer than the buffer -> truncated to the first 8 bytes (no
		// terminator required when the buffer is exactly full).
		set_cstr(&mut arr, "this string is far too long");
		assert_eq!(cstr(&arr), "this str");
		assert_eq!(arr.len(), 8);
		assert_eq!(&arr[..], b"this str");

		// Empty -> all NUL.
		set_cstr(&mut arr, "");
		assert_eq!(arr, [0u8; 8]);
		assert_eq!(cstr(&arr), "");
	}

	#[test]
	fn xml_escaping_roundtrips() {
		let s = "a&b<c>d";
		assert_eq!(unescape_xml(&escape_xml(s)), s);
		assert_eq!(escape_xml("plain"), "plain");
	}

	fn identity() -> [f64; 16] {
		[
			1.0, 0.0, 0.0, 0.0, //
			0.0, 1.0, 0.0, 0.0, //
			0.0, 0.0, 1.0, 0.0, //
			0.0, 0.0, 0.0, 1.0,
		]
	}

	/// `oakcodec_encoding_params` byte-level layout lock. The original C
	/// ABI (`include/codec/encoder.h`) has been retired, but the offsets of
	/// the pre-existing fields stay frozen so any on-disk/IPC copy of the
	/// POD still reads in place; the delivery color-metadata fields are
	/// appended at the end.
	#[test]
	fn encoding_params_c_abi_layout() {
		use std::mem::{offset_of, size_of};

		assert_eq!(size_of::<EncodingParams>(), 1552);
		assert_eq!(offset_of!(EncodingParams, filename), 0);
		assert_eq!(offset_of!(EncodingParams, format), 1024);
		assert_eq!(offset_of!(EncodingParams, video_enabled), 1028);
		assert_eq!(offset_of!(EncodingParams, video_pixel_aspect_den), 1064);
		assert_eq!(offset_of!(EncodingParams, video_bit_rate), 1072);
		assert_eq!(offset_of!(EncodingParams, video_pix_fmt), 1108);
		assert_eq!(offset_of!(EncodingParams, video_is_image_sequence), 1172);
		assert_eq!(offset_of!(EncodingParams, audio_channel_layout), 1192);
		assert_eq!(offset_of!(EncodingParams, audio_bit_rate), 1208);
		assert_eq!(offset_of!(EncodingParams, color_transform_output), 1232);
		assert_eq!(offset_of!(EncodingParams, has_custom_range), 1496);
		assert_eq!(offset_of!(EncodingParams, custom_range_in_num), 1504);
		assert_eq!(offset_of!(EncodingParams, custom_range_out_den), 1528);
		// Appended delivery color metadata (H.273 code points).
		assert_eq!(offset_of!(EncodingParams, color_primaries), 1536);
		assert_eq!(offset_of!(EncodingParams, color_trc), 1540);
		assert_eq!(offset_of!(EncodingParams, color_space), 1544);
		assert_eq!(offset_of!(EncodingParams, color_range), 1548);
	}
}
