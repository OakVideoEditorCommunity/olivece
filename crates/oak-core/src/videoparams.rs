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

//! Video parameter set, mirroring `src/common/src/videoparams.h` and
//! `include/common/videoparams.h`. A handle-wrapped value object with
//! typed getters/setters plus a set of static (handle-free) helpers.
//! The C++-only `_init_from_native` / `_get_native` entry points deal
//! with `olive::VideoParams` and are served by the C++ adapter layer,
//! not here.

use crate::error::{Error, Result};
use crate::ocioutils::PixelFormat;

/// Interlacing modes, mirroring `olive::VideoParams::Interlacing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interlacing {
	/// Progressive.
	None = 0,
	/// Interlaced, top field first.
	TopFirst = 1,
	/// Interlaced, bottom field first.
	BottomFirst = 2,
}

/// Video stream types, mirroring `olive::VideoParams::Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoType {
	/// Regular video.
	Video = 0,
	/// Still image.
	Still = 1,
	/// Image sequence.
	ImageSequence = 2,
}

/// Color range codes, mirroring `olive::VideoParams::ColorRange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRange {
	/// Limited range (16-235).
	Limited = 0,
	/// Full range (0-255).
	Full = 1,
}

/// `olive::VideoParams` — a handle-wrapped video parameter set.
#[derive(Clone, Debug)]
pub struct VideoParams {
	/// Frame width in pixels.
	width: i32,
	/// Frame height in pixels.
	height: i32,
	/// Bit depth per channel.
	depth: i32,
	/// Time base (`numerator`/`denominator`).
	time_base: (i32, i32),
	/// Frame rate (`numerator`/`denominator`).
	frame_rate: (i32, i32),
	/// Pixel aspect ratio (`numerator`/`denominator`).
	pixel_aspect_ratio: (i32, i32),
	/// Pixel format.
	format: PixelFormat,
	/// Number of channels.
	channel_count: i32,
	/// Interlacing mode.
	interlacing: Interlacing,
	/// Resolution divider.
	divider: i32,
	/// Whether the stream is enabled.
	enabled: bool,
	/// X offset in the containing canvas.
	x: f32,
	/// Y offset in the containing canvas.
	y: f32,
	/// Stream index within the source file.
	stream_index: i32,
	/// Stream type.
	video_type: VideoType,
	/// Stream start time (time-base units).
	start_time: i64,
	/// Stream duration (time-base units).
	duration: i64,
	/// Whether alpha is premultiplied.
	premultiplied_alpha: bool,
	/// Color range.
	color_range: ColorRange,
	/// Color primaries code.
	color_primaries: i32,
	/// Color transfer function code.
	color_transfer: i32,
	/// Colorspace name (for color-managed workflows).
	colorspace: String,
}

impl VideoParams {
	/// Create a default (invalid) parameter set.
	pub fn new() -> Self {
		// Matches the C++ default ctor: width/height/depth 0, time base 0/1,
		// invalid format, 0 channels, square pixels, progressive, divider 1,
		// then `set_defaults_for_footage()`.
		Self {
			width: 0,
			height: 0,
			depth: 0,
			time_base: (0, 1),
			frame_rate: (0, 1),
			pixel_aspect_ratio: (1, 1),
			format: PixelFormat::Invalid,
			channel_count: 0,
			interlacing: Interlacing::None,
			divider: 1,
			enabled: true,
			x: 0.0,
			y: 0.0,
			stream_index: 0,
			video_type: VideoType::Video,
			start_time: 0,
			duration: 0,
			premultiplied_alpha: false,
			color_range: ColorRange::Limited,
			color_primaries: 0,
			color_transfer: 0,
			colorspace: String::new(),
		}
	}

	/// Create a parameter set without a time base.
	#[allow(clippy::too_many_arguments)]
	pub fn new_basic(
		width: i32,
		height: i32,
		pixel_format: PixelFormat,
		nb_channels: i32,
		pixel_aspect_num: i32,
		pixel_aspect_den: i32,
		interlacing: i32,
		divider: i32,
	) -> Self {
		let mut par = make_rational(pixel_aspect_num, pixel_aspect_den);
		if rational_is_null(par) {
			par = (1, 1); // validate_pixel_aspect_ratio()
		}
		Self {
			width,
			height,
			depth: 1,
			time_base: (0, 1),
			frame_rate: (0, 1),
			pixel_aspect_ratio: par,
			format: pixel_format,
			channel_count: nb_channels,
			interlacing: interlacing_from_i32(interlacing),
			divider,
			enabled: true,
			x: 0.0,
			y: 0.0,
			stream_index: 0,
			video_type: VideoType::Video,
			start_time: 0,
			duration: 0,
			premultiplied_alpha: false,
			color_range: ColorRange::Limited,
			color_primaries: 0,
			color_transfer: 0,
			colorspace: String::new(),
		}
	}

	/// Create a parameter set with a time base; the frame rate is derived
	/// as the flipped time base.
	#[allow(clippy::too_many_arguments)]
	pub fn new_with_time_base(
		width: i32,
		height: i32,
		time_base_num: i32,
		time_base_den: i32,
		pixel_format: PixelFormat,
		nb_channels: i32,
		pixel_aspect_num: i32,
		pixel_aspect_den: i32,
		interlacing: i32,
		divider: i32,
	) -> Self {
		let time_base = make_rational(time_base_num, time_base_den);
		let frame_rate = rational_flipped(time_base);
		let mut par = make_rational(pixel_aspect_num, pixel_aspect_den);
		if rational_is_null(par) {
			par = (1, 1); // validate_pixel_aspect_ratio()
		}
		Self {
			width,
			height,
			depth: 1,
			time_base,
			frame_rate,
			pixel_aspect_ratio: par,
			format: pixel_format,
			channel_count: nb_channels,
			interlacing: interlacing_from_i32(interlacing),
			divider,
			enabled: true,
			x: 0.0,
			y: 0.0,
			stream_index: 0,
			video_type: VideoType::Video,
			start_time: 0,
			duration: 0,
			premultiplied_alpha: false,
			color_range: ColorRange::Limited,
			color_primaries: 0,
			color_transfer: 0,
			colorspace: String::new(),
		}
	}

	/// Frame width.
	pub fn width(&self) -> i32 {
		self.width
	}

	/// Set the frame width.
	pub fn set_width(&mut self, width: i32) {
		self.width = width;
	}

	/// Frame height.
	pub fn height(&self) -> i32 {
		self.height
	}

	/// Set the frame height.
	pub fn set_height(&mut self, height: i32) {
		self.height = height;
	}

	/// Bit depth per channel.
	pub fn depth(&self) -> i32 {
		self.depth
	}

	/// Set the bit depth per channel.
	pub fn set_depth(&mut self, depth: i32) {
		self.depth = depth;
	}

	/// Whether the frame is stereoscopic 3D.
	pub fn is_3d(&self) -> bool {
		// CPP-PARITY: C++ `is_3d()` is `depth_ > 1`, derived. The skeleton
		// declared a stored `is_3d: bool` field, but C++ has none; we derive
		// from depth so the value can never drift out of sync with `depth`.
		self.depth > 1
	}

	/// The time base as a numerator/denominator pair.
	pub fn time_base(&self) -> (i32, i32) {
		self.time_base
	}

	/// Set the time base.
	pub fn set_time_base(&mut self, numerator: i32, denominator: i32) {
		self.time_base = make_rational(numerator, denominator);
	}

	/// The frame rate as a numerator/denominator pair.
	pub fn frame_rate(&self) -> (i32, i32) {
		self.frame_rate
	}

	/// Set the frame rate.
	pub fn set_frame_rate(&mut self, numerator: i32, denominator: i32) {
		self.frame_rate = make_rational(numerator, denominator);
	}

	/// The frame rate expressed as a (flipped) time base.
	pub fn frame_rate_as_time_base(&self) -> (i32, i32) {
		rational_flipped(self.frame_rate)
	}

	/// The pixel aspect ratio as a numerator/denominator pair.
	pub fn pixel_aspect_ratio(&self) -> (i32, i32) {
		self.pixel_aspect_ratio
	}

	/// Set the pixel aspect ratio.
	pub fn set_pixel_aspect_ratio(&mut self, numerator: i32, denominator: i32) {
		let mut r = make_rational(numerator, denominator);
		if rational_is_null(r) {
			r = (1, 1); // validate_pixel_aspect_ratio()
		}
		self.pixel_aspect_ratio = r;
	}

	/// The pixel format.
	pub fn format(&self) -> PixelFormat {
		self.format
	}

	/// Set the pixel format.
	pub fn set_format(&mut self, format: PixelFormat) {
		self.format = format;
	}

	/// Number of channels.
	pub fn channel_count(&self) -> i32 {
		self.channel_count
	}

	/// Set the number of channels.
	pub fn set_channel_count(&mut self, count: i32) {
		self.channel_count = count;
	}

	/// The interlacing mode.
	pub fn interlacing(&self) -> Interlacing {
		self.interlacing
	}

	/// Set the interlacing mode.
	pub fn set_interlacing(&mut self, interlacing: Interlacing) {
		self.interlacing = interlacing;
	}

	/// The resolution divider.
	pub fn divider(&self) -> i32 {
		self.divider
	}

	/// Set the resolution divider.
	pub fn set_divider(&mut self, divider: i32) {
		self.divider = divider;
	}

	/// Whether the stream is enabled.
	pub fn enabled(&self) -> bool {
		self.enabled
	}

	/// Set whether the stream is enabled.
	pub fn set_enabled(&mut self, enabled: bool) {
		self.enabled = enabled;
	}

	/// X offset in the containing canvas.
	pub fn x(&self) -> f32 {
		self.x
	}

	/// Set the X offset.
	pub fn set_x(&mut self, x: f32) {
		self.x = x;
	}

	/// Y offset in the containing canvas.
	pub fn y(&self) -> f32 {
		self.y
	}

	/// Set the Y offset.
	pub fn set_y(&mut self, y: f32) {
		self.y = y;
	}

	/// Stream index within the source file.
	pub fn stream_index(&self) -> i32 {
		self.stream_index
	}

	/// Set the stream index.
	pub fn set_stream_index(&mut self, index: i32) {
		self.stream_index = index;
	}

	/// The stream type.
	pub fn video_type(&self) -> VideoType {
		self.video_type
	}

	/// Set the stream type.
	pub fn set_video_type(&mut self, video_type: VideoType) {
		self.video_type = video_type;
	}

	/// Stream start time (time-base units).
	pub fn start_time(&self) -> i64 {
		self.start_time
	}

	/// Set the stream start time.
	pub fn set_start_time(&mut self, start_time: i64) {
		self.start_time = start_time;
	}

	/// Stream duration (time-base units).
	pub fn duration(&self) -> i64 {
		self.duration
	}

	/// Set the stream duration.
	pub fn set_duration(&mut self, duration: i64) {
		self.duration = duration;
	}

	/// Whether alpha is premultiplied.
	pub fn premultiplied_alpha(&self) -> bool {
		self.premultiplied_alpha
	}

	/// Set whether alpha is premultiplied.
	pub fn set_premultiplied_alpha(&mut self, premultiplied: bool) {
		self.premultiplied_alpha = premultiplied;
	}

	/// The color range.
	pub fn color_range(&self) -> ColorRange {
		self.color_range
	}

	/// Set the color range.
	pub fn set_color_range(&mut self, color_range: ColorRange) {
		self.color_range = color_range;
	}

	/// Color primaries code.
	pub fn color_primaries(&self) -> i32 {
		self.color_primaries
	}

	/// Set the color primaries code.
	pub fn set_color_primaries(&mut self, primaries: i32) {
		self.color_primaries = primaries;
	}

	/// Color transfer function code.
	pub fn color_transfer(&self) -> i32 {
		self.color_transfer
	}

	/// Set the color transfer function code.
	pub fn set_color_transfer(&mut self, transfer: i32) {
		self.color_transfer = transfer;
	}

	/// The colorspace name.
	pub fn colorspace(&self) -> &str {
		&self.colorspace
	}

	/// Set the colorspace name.
	pub fn set_colorspace(&mut self, colorspace: &str) {
		self.colorspace = colorspace.to_string();
	}

	/// Width multiplied by the pixel aspect ratio.
	pub fn square_pixel_width(&self) -> i32 {
		// calculate_square_pixel_width(): denominator()==0 -> NaN par -> width
		let (num, den) = self.pixel_aspect_ratio;
		if den != 0 {
			(self.width as f64 * rational_to_double((num, den))).round() as i32
		} else {
			self.width
		}
	}

	/// Effective (scaled) width.
	pub fn effective_width(&self) -> i32 {
		Self::get_scaled_dimension(self.width, self.divider)
	}

	/// Effective (scaled) height.
	pub fn effective_height(&self) -> i32 {
		Self::get_scaled_dimension(self.height, self.divider)
	}

	/// Effective (scaled) depth.
	pub fn effective_depth(&self) -> i32 {
		if self.depth == 1 {
			self.depth
		} else {
			Self::get_scaled_dimension(self.depth, self.divider)
		}
	}

	/// Whether the parameter set describes a valid stream.
	pub fn is_valid(&self) -> bool {
		let c = pf_code(self.format);
		self.width > 0
			&& self.height > 0
			&& !rational_is_null(self.pixel_aspect_ratio)
			&& c > pf_code(PixelFormat::Invalid)
			&& c < pf_code(PixelFormat::Count)
			&& self.channel_count > 0
	}

	/// Bytes per channel.
	pub fn bytes_per_channel(&self) -> i32 {
		Self::bytes_per_channel_for_format(self.format)
	}

	/// Bytes per pixel.
	pub fn bytes_per_pixel(&self) -> i32 {
		Self::bytes_per_pixel_for_format(self.format, self.channel_count)
	}

	/// Total buffer size for a single frame.
	pub fn buffer_size(&self) -> i32 {
		Self::calculate_buffer_size(self.width, self.height, self.format, self.channel_count)
	}

	/// Convert a time (in seconds, as a rational) to time-base units.
	/// Returns `None` when no time base is set.
	pub fn time_in_timebase_units(&self, time_num: i32, time_den: i32) -> Option<i64> {
		if rational_is_null(self.time_base) {
			return None; // C++ returns INT64_MIN (AV_NOPTS_VALUE)
		}
		let d = rational_to_double((time_num, time_den))
			* rational_to_double(rational_flipped(self.time_base));
		let ts = if d.is_nan() { 0 } else { d.round() as i64 }; // llround()
		Some(ts + self.start_time)
	}

	/// Compare two parameter sets for equality.
	pub fn equals(&self, other: &VideoParams) -> bool {
		self.width == other.width
			&& self.height == other.height
			&& self.depth == other.depth
			&& self.interlacing == other.interlacing
			&& self.time_base == other.time_base
			&& self.format == other.format
			&& self.pixel_aspect_ratio == other.pixel_aspect_ratio
			&& self.divider == other.divider
			&& self.channel_count == other.channel_count
	}

	/// Load parameters from an XML fragment.
	pub fn load_xml(&mut self, xml: &str) -> Result<()> {
		// Mirrors oak_core_videoparams_load_xml + VideoParams::load():
		// parse, fail on error, position on the root element, then consume
		// its children.
		let events = parse_xml(xml).ok_or_else(|| Error::Failed("XML parse error".to_string()))?;
		let mut cur = XmlCursor::new(events);
		if !cur.next_start_element() {
			return Err(Error::Failed("no root element".to_string()));
		}
		while cur.next_start_element() {
			let name = cur.name.clone();
			match name.as_str() {
				"width" => self.set_width(stoi_field(&cur.read_element_text())?),
				"height" => self.set_height(stoi_field(&cur.read_element_text())?),
				"depth" => self.set_depth(stoi_field(&cur.read_element_text())?),
				"timebase" => {
					let (n, d) = rational_from_string(&cur.read_element_text());
					self.set_time_base(n, d);
				}
				"format" => self.set_format(pf_from_code(stoi_field(&cur.read_element_text())?)),
				"channelcount" => {
					self.set_channel_count(stoi_field(&cur.read_element_text())?);
				}
				"pixelaspectratio" => {
					let (n, d) = rational_from_string(&cur.read_element_text());
					self.set_pixel_aspect_ratio(n, d);
				}
				"interlacing" => {
					self.set_interlacing(interlacing_from_i32(stoi_field(
						&cur.read_element_text(),
					)?));
				}
				"divider" => self.set_divider(stoi_field(&cur.read_element_text())?),
				"enabled" => self.set_enabled(stoi_field(&cur.read_element_text())? != 0),
				"x" => self.set_x(parse_f32_field(&cur.read_element_text())?),
				"y" => self.set_y(parse_f32_field(&cur.read_element_text())?),
				"streamindex" => self.set_stream_index(stoi_field(&cur.read_element_text())?),
				"videotype" => {
					self.set_video_type(video_type_from_i32(stoi_field(&cur.read_element_text())?));
				}
				"framerate" => {
					let (n, d) = rational_from_string(&cur.read_element_text());
					self.set_frame_rate(n, d);
				}
				"starttime" => self.set_start_time(stoll_field(&cur.read_element_text())?),
				"duration" => self.set_duration(stoll_field(&cur.read_element_text())?),
				"premultipliedalpha" => {
					self.set_premultiplied_alpha(stoi_field(&cur.read_element_text())? != 0);
				}
				"colorspace" => self.set_colorspace(&cur.read_element_text()),
				"colorrange" => {
					self.set_color_range(color_range_from_i32(stoi_field(
						&cur.read_element_text(),
					)?));
				}
				"colorprimaries" => {
					self.set_color_primaries(stoi_field(&cur.read_element_text())?);
				}
				"colortransfer" => {
					self.set_color_transfer(stoi_field(&cur.read_element_text())?);
				}
				_ => cur.skip_current_element(),
			}
		}
		Ok(())
	}

	/// Save parameters to an XML fragment.
	pub fn save_xml(&self) -> Result<String> {
		// Mirrors oak_core_videoparams_save_xml: a `<videoparams>` root with
		// the exact child order of VideoParams::save(). The writer emits no
		// whitespace; each child is `<name>text</name>`.
		let mut out = String::new();
		out.push_str("<videoparams>");
		push_text_element(&mut out, "width", &int_str(self.width));
		push_text_element(&mut out, "height", &int_str(self.height));
		push_text_element(&mut out, "depth", &int_str(self.depth));
		push_text_element(&mut out, "timebase", &rational_to_string(self.time_base));
		push_text_element(&mut out, "format", &int_str(pf_code(self.format)));
		push_text_element(&mut out, "channelcount", &int_str(self.channel_count));
		push_text_element(
			&mut out,
			"pixelaspectratio",
			&rational_to_string(self.pixel_aspect_ratio),
		);
		push_text_element(&mut out, "interlacing", &int_str(self.interlacing as i32));
		push_text_element(&mut out, "divider", &int_str(self.divider));
		push_text_element(&mut out, "enabled", &bool_str(self.enabled));
		push_text_element(&mut out, "x", &f32_str(self.x));
		push_text_element(&mut out, "y", &f32_str(self.y));
		push_text_element(&mut out, "streamindex", &int_str(self.stream_index));
		push_text_element(&mut out, "videotype", &int_str(self.video_type as i32));
		push_text_element(&mut out, "framerate", &rational_to_string(self.frame_rate));
		push_text_element(&mut out, "starttime", &self.start_time.to_string());
		push_text_element(&mut out, "duration", &self.duration.to_string());
		push_text_element(
			&mut out,
			"premultipliedalpha",
			&bool_str(self.premultiplied_alpha),
		);
		push_text_element(&mut out, "colorspace", &self.colorspace);
		push_text_element(&mut out, "colorrange", &int_str(self.color_range as i32));
		push_text_element(&mut out, "colorprimaries", &int_str(self.color_primaries));
		push_text_element(&mut out, "colortransfer", &int_str(self.color_transfer));
		out.push_str("</videoparams>");
		Ok(out)
	}

	/// Bytes per channel for a given format.
	pub fn bytes_per_channel_for_format(pixel_format: PixelFormat) -> i32 {
		match pixel_format {
			PixelFormat::U8 => 1,
			// Packed 10-bit: use bytes_per_pixel_for_format instead.
			PixelFormat::U10 => 0,
			PixelFormat::U16 | PixelFormat::F16 => 2,
			PixelFormat::F32 => 4,
			PixelFormat::Invalid | PixelFormat::Count => 0,
		}
	}

	/// Bytes per pixel for a given format and channel count.
	pub fn bytes_per_pixel_for_format(pixel_format: PixelFormat, channels: i32) -> i32 {
		if pixel_format == PixelFormat::U10 {
			// Packed 10-bit RGBA10A2: 4 bytes per RGBA pixel.
			return if channels == 4 { 4 } else { 0 };
		}
		Self::bytes_per_channel_for_format(pixel_format).wrapping_mul(channels)
	}

	/// Total buffer size for a frame.
	pub fn calculate_buffer_size(
		width: i32,
		height: i32,
		pixel_format: PixelFormat,
		channels: i32,
	) -> i32 {
		// CPP-PARITY: C++ uses int `width * height * bpp` (wraps on
		// overflow); use wrapping arithmetic so debug builds don't panic.
		let bpp = Self::bytes_per_pixel_for_format(pixel_format, channels);
		width.wrapping_mul(height).wrapping_mul(bpp)
	}

	/// Whether the format stores floating-point channels.
	pub fn format_is_float(pixel_format: PixelFormat) -> bool {
		matches!(pixel_format, PixelFormat::F16 | PixelFormat::F32)
	}

	/// Generate an auto divider for the given dimensions.
	pub fn generate_auto_divider(width: i64, height: i64) -> i32 {
		// CPP-PARITY: the C++ computes `int64_t width*height`; we use i128 to
		// avoid overflow for large dimensions (identical results in range).
		const TARGET_RES: i64 = 1280 * 720;
		const DIVIDERS: [i32; 8] = [1, 2, 3, 4, 6, 8, 12, 16];

		let megapixels = (width as i128) * (height as i128);
		let squared_divider = megapixels as f64 / TARGET_RES as f64;
		let divider = squared_divider.sqrt();

		if divider <= DIVIDERS[0] as f64 {
			return DIVIDERS[0];
		} else if divider >= DIVIDERS[7] as f64 {
			return DIVIDERS[7];
		} else {
			for i in 1..DIVIDERS.len() {
				let prev = DIVIDERS[i - 1];
				let next = DIVIDERS[i];
				if divider >= prev as f64 && divider <= next as f64 {
					let prev_diff = (prev as f64 - divider).abs();
					let next_diff = (next as f64 - divider).abs();
					return if prev_diff < next_diff { prev } else { next };
				}
			}
			return 1; // fallback, unreachable
		}
	}

	/// Scale a dimension by a divider.
	pub fn get_scaled_dimension(dimension: i32, divider: i32) -> i32 {
		dimension / divider
	}

	/// Divider that maps `src_*` resolution to no more than `dst_*`.
	pub fn get_divider_for_target_resolution(
		src_width: i32,
		src_height: i32,
		dst_width: i32,
		dst_height: i32,
	) -> i32 {
		let mut divider = 0;
		loop {
			divider += 1;
			let test_width = Self::get_scaled_dimension(src_width, divider);
			let test_height = Self::get_scaled_dimension(src_height, divider);
			if test_width <= dst_width && test_height <= dst_height {
				break;
			}
		}
		divider
	}

	/// Human-readable name for a divider (`"Full"`, `"1/2"`, ...).
	pub fn name_for_divider(divider: i32) -> Result<String> {
		// C++ never fails here; the Result mirrors the c_api try/catch.
		Ok(if divider == 1 {
			"Full".to_string()
		} else {
			format!("1/{}", divider)
		})
	}

	/// Human-readable name for a pixel format.
	pub fn format_name(pixel_format: PixelFormat) -> Result<String> {
		Ok(match pixel_format {
			PixelFormat::U8 => "8-bit".to_string(),
			PixelFormat::U10 => "10-bit Packed".to_string(),
			PixelFormat::U16 => "16-bit Integer".to_string(),
			PixelFormat::F16 => "Half-Float (16-bit)".to_string(),
			PixelFormat::F32 => "Full-Float (32-bit)".to_string(),
			PixelFormat::Invalid | PixelFormat::Count => {
				// `%X` of the negative code produces FFFFFFFF for invalid.
				format!("Unknown (0x{:X})", pf_code(pixel_format))
			}
		})
	}

	/// Human-readable frame-rate string (`"23.976 FPS"`).
	pub fn frame_rate_to_string(numerator: i32, denominator: i32) -> Result<String> {
		// CPP-PARITY: `%g` is hand-rolled (no libc); see `format_g`.
		let value = numerator as f64 / denominator as f64;
		Ok(format!("{} FPS", format_g(value)))
	}
}

// ---- Private helpers -----------------------------------------------------

/// Map an `i32` code to the [`PixelFormat`] variant.
///
/// CPP-PARITY: C++ `static_cast`s arbitrary ints onto the enum; unknown
/// codes can't be represented in Rust, so they map to `Invalid` (which, like
/// any out-of-range code, fails `is_valid()`).
fn pf_from_code(code: i32) -> PixelFormat {
	match code {
		-1 => PixelFormat::Invalid,
		0 => PixelFormat::U8,
		1 => PixelFormat::U10,
		2 => PixelFormat::U16,
		3 => PixelFormat::F16,
		4 => PixelFormat::F32,
		5 => PixelFormat::Count,
		_ => PixelFormat::Invalid,
	}
}

/// The integer code of a [`PixelFormat`].
///
/// CPP-PARITY: the discriminant is load-bearing and cast directly, matching
/// `std::to_string(format_)` / `static_cast<PixelFormat::Format>(int)` in the
/// C++ load/save paths.
fn pf_code(pf: PixelFormat) -> i32 {
	pf as i32
}

fn interlacing_from_i32(v: i32) -> Interlacing {
	match v {
		1 => Interlacing::TopFirst,
		2 => Interlacing::BottomFirst,
		_ => Interlacing::None,
	}
}

fn video_type_from_i32(v: i32) -> VideoType {
	match v {
		1 => VideoType::Still,
		2 => VideoType::ImageSequence,
		_ => VideoType::Video,
	}
}

fn color_range_from_i32(v: i32) -> ColorRange {
	match v {
		1 => ColorRange::Full,
		_ => ColorRange::Limited,
	}
}

fn int_str(v: i32) -> String {
	v.to_string()
}

fn bool_str(b: bool) -> String {
	if b {
		"1".to_string()
	} else {
		"0".to_string()
	}
}

/// `std::to_string(float)` — fixed notation with 6 decimal places.
///
/// CPP-PARITY: C++ `std::to_string(float)` is `%f` (6 decimals); Rust's
/// `{:.6}` is the closest match.
fn f32_str(v: f32) -> String {
	format!("{:.6}", v)
}

fn escape_xml_text(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			_ => out.push(c),
		}
	}
	out
}

fn push_text_element(out: &mut String, name: &str, text: &str) {
	out.push('<');
	out.push_str(name);
	out.push('>');
	out.push_str(&escape_xml_text(text));
	out.push_str("</");
	out.push_str(name);
	out.push('>');
}

fn i64_gcd(mut a: i64, mut b: i64) -> i64 {
	if a < 0 {
		a = -a;
	}
	if b < 0 {
		b = -b;
	}
	while b != 0 {
		let t = a % b;
		a = b;
		b = t;
	}
	a
}

/// Construct a normalized/reduced rational, mirroring `Rational(num, den)`:
/// `fix_signs()` then gcd `reduce()` (reduce_fraction with max = INT_MAX,
/// which is a no-op beyond the gcd for i32 inputs).
fn make_rational(num: i32, den: i32) -> (i32, i32) {
	let mut n = num as i64;
	let mut d = den as i64;
	if d < 0 {
		d = -d;
		n = -n;
	} else if d == 0 {
		n = 0;
	} else if n == 0 {
		d = 1;
	}
	if d == 0 {
		return (0, 0); // NaN rational
	}
	let g = i64_gcd(n, d);
	if g != 0 {
		n /= g;
		d /= g;
	}
	(n as i32, d as i32)
}

/// `Rational::isNull()` — numerator == 0.
fn rational_is_null((n, _): (i32, i32)) -> bool {
	n == 0
}

/// `Rational::to_double()` — num/den, NaN when den == 0.
fn rational_to_double((n, d): (i32, i32)) -> f64 {
	if d != 0 {
		n as f64 / d as f64
	} else {
		f64::NAN
	}
}

/// `Rational::flipped()` — swap num/den (no-op on a null rational), then
/// `fix_signs()`.
fn rational_flipped((n, d): (i32, i32)) -> (i32, i32) {
	if n == 0 {
		return (n, d);
	}
	let mut n2 = d;
	let mut d2 = n;
	if d2 < 0 {
		d2 = -d2;
		n2 = -n2;
	} else if d2 == 0 {
		n2 = 0;
	} else if n2 == 0 {
		d2 = 1;
	}
	(n2, d2)
}

/// `Rational::to_string()` — `"%d/%d"`.
fn rational_to_string((n, d): (i32, i32)) -> String {
	format!("{}/{}", n, d)
}

/// `Rational::from_string()` — split on '/', 1 element -> integer, 2 ->
/// num/den, else NaN. Non-numeric parts become 0.
fn rational_from_string(s: &str) -> (i32, i32) {
	let parts: Vec<&str> = s.split('/').collect();
	match parts.len() {
		1 => make_rational(int_or_zero(parts[0]), 1),
		2 => make_rational(int_or_zero(parts[0]), int_or_zero(parts[1])),
		_ => (0, 0),
	}
}

/// `std::stoi`-style prefix parse (skip leading whitespace, optional sign,
/// then digits). `None` when there are no digits.
fn parse_int_prefix(s: &str) -> Option<i64> {
	let bytes = s.trim_start().as_bytes();
	let mut i = 0;
	let mut neg = false;
	if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
		neg = bytes[i] == b'-';
		i += 1;
	}
	let mut val: i64 = 0;
	let mut any = false;
	while i < bytes.len() && bytes[i].is_ascii_digit() {
		val = val * 10 + (bytes[i] - b'0') as i64;
		i += 1;
		any = true;
	}
	if !any {
		None
	} else {
		Some(if neg { -val } else { val })
	}
}

/// Field `std::stoi` (throws on no digits) -> error propagates to the c_api
/// as `E_FAILED`.
fn stoi_field(s: &str) -> Result<i32> {
	parse_int_prefix(s)
		.map(|v| v as i32)
		.ok_or_else(|| Error::Failed("invalid integer".to_string()))
}

/// Field `std::stoll`.
fn stoll_field(s: &str) -> Result<i64> {
	parse_int_prefix(s).ok_or_else(|| Error::Failed("invalid integer".to_string()))
}

/// `StringUtils::to_int` — 0 on parse failure.
fn int_or_zero(s: &str) -> i32 {
	parse_int_prefix(s).unwrap_or(0) as i32
}

/// Field `std::stof` — skip leading whitespace, parse the longest valid
/// floating-point prefix and ignore trailing junk (exactly like `std::stof`,
/// which only throws when there is no valid prefix at all).
fn parse_f32_field(s: &str) -> Result<f32> {
	let s = s.trim_start();
	let bytes = s.as_bytes();
	let mut i = 0;
	if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
		i += 1;
	}
	let int_start = i;
	while i < bytes.len() && bytes[i].is_ascii_digit() {
		i += 1;
	}
	let int_digits = i - int_start;
	let mut frac_digits = 0;
	if i < bytes.len() && bytes[i] == b'.' {
		i += 1;
		let frac_start = i;
		while i < bytes.len() && bytes[i].is_ascii_digit() {
			i += 1;
		}
		frac_digits = i - frac_start;
	}
	if int_digits == 0 && frac_digits == 0 {
		return Err(Error::Failed("invalid float".to_string()));
	}
	// Optional exponent, consumed only when it has at least one digit.
	if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
		let mut j = i + 1;
		if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
			j += 1;
		}
		let exp_start = j;
		while j < bytes.len() && bytes[j].is_ascii_digit() {
			j += 1;
		}
		if j > exp_start {
			i = j;
		}
	}
	s[..i]
		.parse::<f32>()
		.map_err(|_| Error::Failed("invalid float".to_string()))
}

/// Hand-rolled C `%g` formatting (default precision 6).
///
/// CPP-PARITY: the crate may not pull in libc/libm `%g`; this reproduces the
/// C99 semantics (6 significant digits, `%f` style when `P > E >= -4`,
/// otherwise `%e` style with a `e±dd` exponent, trailing zeros stripped).
fn format_g(value: f64) -> String {
	const PREC: i32 = 6;

	if value.is_nan() {
		return "nan".to_string();
	}
	if value.is_infinite() {
		return if value < 0.0 { "-inf" } else { "inf" }.to_string();
	}
	let sign = if value < 0.0 { "-" } else { "" };
	let v = value.abs();
	if v == 0.0 {
		return format!("{}0", sign);
	}

	let mut e = v.log10().floor() as i32;
	// Correct floating-point undershoot near exact powers of ten.
	if v >= 10f64.powi(e + 1) {
		e += 1;
	}

	let body = if PREC > e && e >= -4 {
		let decimals = (PREC - 1 - e).max(0) as usize;
		trim_frac_trailing(format!("{:.*}", decimals, v))
	} else {
		let mantissa = v / 10f64.powi(e);
		let mant = trim_frac_trailing(format!("{:.5}", mantissa));
		format!("{}e{}{:02}", mant, if e < 0 { "-" } else { "+" }, e.abs())
	};

	format!("{}{}", sign, body)
}

/// Strip trailing zeros (and a trailing '.') from a fixed-point string.
fn trim_frac_trailing(s: String) -> String {
	if !s.contains('.') {
		return s;
	}
	let mut chars = s;
	while chars.ends_with('0') {
		chars.pop();
	}
	if chars.ends_with('.') {
		chars.pop();
	}
	chars
}

// ---- Minimal hand-written XML event stream -------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenType {
	StartElement,
	EndElement,
	Characters,
	EndDocument,
}

enum XmlEvent {
	StartElement(String),
	EndElement(String),
	Characters(String),
}

/// A cursor over a pre-parsed event list, mirroring `olive::XmlStreamReader`.
struct XmlCursor {
	events: Vec<XmlEvent>,
	pos: usize,
	cur: TokenType,
	name: String,
	text: String,
}

impl XmlCursor {
	fn new(events: Vec<XmlEvent>) -> Self {
		Self {
			events,
			pos: 0,
			cur: TokenType::EndDocument,
			name: String::new(),
			text: String::new(),
		}
	}

	fn read_next(&mut self) -> TokenType {
		if self.pos >= self.events.len() {
			self.cur = TokenType::EndDocument;
			self.name.clear();
			self.text.clear();
		} else {
			match &self.events[self.pos] {
				XmlEvent::StartElement(n) => {
					self.cur = TokenType::StartElement;
					self.name = n.clone();
					self.text.clear();
				}
				XmlEvent::EndElement(n) => {
					self.cur = TokenType::EndElement;
					self.name = n.clone();
					self.text.clear();
				}
				XmlEvent::Characters(t) => {
					self.cur = TokenType::Characters;
					self.name.clear();
					self.text = t.clone();
				}
			}
			self.pos += 1;
		}
		self.cur
	}

	/// `xml_read_next_start_element()`: true when positioned on a start
	/// element, false at an end element or end of document.
	fn next_start_element(&mut self) -> bool {
		loop {
			match self.read_next() {
				TokenType::StartElement => return true,
				TokenType::EndElement | TokenType::EndDocument => return false,
				_ => {}
			}
		}
	}

	/// `read_element_text()`: consume through the matching end element,
	/// concatenating the depth-1 character data.
	fn read_element_text(&mut self) -> String {
		let mut result = String::new();
		let mut depth = 1;
		loop {
			match self.read_next() {
				TokenType::StartElement => depth += 1,
				TokenType::EndElement => {
					depth -= 1;
					if depth == 0 {
						break;
					}
				}
				TokenType::Characters => {
					if depth == 1 {
						result.push_str(&self.text);
					}
				}
				TokenType::EndDocument => break,
			}
		}
		result
	}

	/// `skip_current_element()`.
	fn skip_current_element(&mut self) {
		let mut depth = 1;
		loop {
			match self.read_next() {
				TokenType::StartElement => depth += 1,
				TokenType::EndElement => {
					depth -= 1;
					if depth == 0 {
						break;
					}
				}
				TokenType::EndDocument => break,
				_ => {}
			}
		}
	}
}

fn parse_name(data: &str, start: usize) -> Option<(String, usize)> {
	let bytes = data.as_bytes();
	let mut i = start;
	let mut name = String::new();
	while i < bytes.len() {
		let c = bytes[i];
		if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' || c == b':' {
			name.push(c as char);
			i += 1;
		} else {
			break;
		}
	}
	if name.is_empty() {
		None
	} else {
		Some((name, i))
	}
}

/// Resolve the five named entities plus decimal/hex numeric entities.
fn resolve_entity(data: &str, start: usize, semi: usize) -> Option<String> {
	let ent = &data[start + 1..semi];
	match ent {
		"amp" => Some("&".to_string()),
		"lt" => Some("<".to_string()),
		"gt" => Some(">".to_string()),
		"quot" => Some("\"".to_string()),
		"apos" => Some("'".to_string()),
		_ => {
			if let Some(hex) = ent
				.strip_prefix('#')
				.and_then(|h| h.strip_prefix(['x', 'X']))
			{
				let code = u32::from_str_radix(hex, 16).ok()?;
				char::from_u32(code).map(|c| c.to_string())
			} else if let Some(dec) = ent.strip_prefix('#') {
				let code = dec.parse::<u32>().ok()?;
				char::from_u32(code).map(|c| c.to_string())
			} else {
				None
			}
		}
	}
}

/// Parse character data (with entity resolution) up to the next '<'.
fn parse_chars(data: &str, start: usize) -> Option<(String, usize)> {
	let bytes = data.as_bytes();
	let mut i = start;
	let mut out = String::new();
	while i < bytes.len() && bytes[i] != b'<' {
		if bytes[i] == b'&' {
			let semi = data[i + 1..].find(';').map(|off| i + 1 + off)?;
			out.push_str(&resolve_entity(data, i, semi)?);
			i = semi + 1;
		} else {
			let ch = data[i..].chars().next()?;
			out.push(ch);
			i += ch.len_utf8();
		}
	}
	Some((out, i))
}

/// Parse an XML start tag (including attributes, which are validated but
/// discarded), returning the position just after '>' or None on error.
fn parse_start_tag(data: &str, start: usize) -> Option<(String, usize, bool)> {
	let bytes = data.as_bytes();
	let (name, mut i) = parse_name(data, start)?;
	let mut self_closing = false;
	loop {
		while i < bytes.len()
			&& (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r' || bytes[i] == b'\n')
		{
			i += 1;
		}
		if i >= bytes.len() {
			return None;
		}
		match bytes[i] {
			b'>' => {
				i += 1;
				break;
			}
			b'/' => {
				if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
					self_closing = true;
					i += 2;
					break;
				} else {
					return None;
				}
			}
			_ => {
				// attribute name="value"
				let (_, mut j) = parse_name(data, i)?;
				while j < bytes.len()
					&& (bytes[j] == b' '
						|| bytes[j] == b'\t'
						|| bytes[j] == b'\r'
						|| bytes[j] == b'\n')
				{
					j += 1;
				}
				if j >= bytes.len() || bytes[j] != b'=' {
					return None;
				}
				j += 1;
				while j < bytes.len()
					&& (bytes[j] == b' '
						|| bytes[j] == b'\t'
						|| bytes[j] == b'\r'
						|| bytes[j] == b'\n')
				{
					j += 1;
				}
				if j >= bytes.len() || (bytes[j] != b'"' && bytes[j] != b'\'') {
					return None;
				}
				let quote = bytes[j];
				let vstart = j + 1;
				let vend = data[vstart..].find(quote as char).map(|off| vstart + off)?;
				if data[vstart..vend].contains('&') {
					// Validate entities in the attribute value.
					let mut k = vstart;
					while k < vend {
						if bytes[k] == b'&' {
							let semi = data[k + 1..vend].find(';').map(|o| k + 1 + o)?;
							resolve_entity(data, k, semi)?;
							k = semi + 1;
						} else {
							k += 1;
						}
					}
				}
				i = vend + 1;
			}
		}
	}
	Some((name, i, self_closing))
}

/// Parse a full document into an event list, mirroring the expat SAX events
/// used by `XmlStreamReader`. Returns `None` on malformed XML.
fn parse_xml(data: &str) -> Option<Vec<XmlEvent>> {
	let bytes = data.as_bytes();
	let n = bytes.len();
	let mut i = 0;
	let mut events: Vec<XmlEvent> = Vec::new();
	let mut pending = String::new();
	let mut stack: Vec<String> = Vec::new();

	while i < n {
		if bytes[i] == b'<' {
			if !pending.is_empty() {
				events.push(XmlEvent::Characters(std::mem::take(&mut pending)));
			}
			if data[i..].starts_with("<!--") {
				let end = data[i + 4..].find("-->").map(|off| i + 4 + off)?;
				i = end + 3;
				continue;
			}
			if data[i..].starts_with("<?") {
				let end = data[i + 2..].find("?>").map(|off| i + 2 + off)?;
				i = end + 2;
				continue;
			}
			if data[i..].starts_with("<![CDATA[") {
				let end = data[i + 9..].find("]]>").map(|off| i + 9 + off)?;
				// CDATA is emitted as character data.
				pending.push_str(&data[i + 9..end]);
				i = end + 3;
				continue;
			}
			if data[i..].starts_with("<!") {
				// DOCTYPE etc: skip to the matching '>'.
				let mut depth = 0usize;
				let mut j = i + 2;
				loop {
					if j >= n {
						return None;
					}
					match bytes[j] {
						b'[' => depth += 1,
						b']' => depth = depth.saturating_sub(1),
						b'>' if depth == 0 => {
							i = j + 1;
							break;
						}
						_ => {}
					}
					j += 1;
				}
				continue;
			}
			if bytes.get(i + 1) == Some(&b'/') {
				// End element
				let (name, ni) = parse_name(data, i + 2)?;
				let mut j = ni;
				while j < n
					&& (bytes[j] == b' '
						|| bytes[j] == b'\t'
						|| bytes[j] == b'\r'
						|| bytes[j] == b'\n')
				{
					j += 1;
				}
				if j >= n || bytes[j] != b'>' {
					return None;
				}
				if stack.last().map(|s| s.as_str()) != Some(name.as_str()) {
					return None;
				}
				stack.pop();
				events.push(XmlEvent::EndElement(name));
				i = j + 1;
				continue;
			}
			// Start element
			let (name, ni, self_closing) = parse_start_tag(data, i + 1)?;
			if !name.is_empty() {
				events.push(XmlEvent::StartElement(name.clone()));
			}
			stack.push(name.clone());
			if self_closing {
				// `<name/>` produces an immediate end element; balance the stack
				// so trailing unclosed detection stays correct.
				stack.pop();
				events.push(XmlEvent::EndElement(name));
			}
			i = ni;
		} else {
			let (text, ni) = parse_chars(data, i)?;
			pending.push_str(&text);
			i = ni;
		}
	}

	if !pending.is_empty() {
		events.push(XmlEvent::Characters(pending));
	}
	if !stack.is_empty() {
		return None; // unclosed element
	}
	Some(events)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn default_vp() -> VideoParams {
		VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 1)
	}

	#[test]
	fn default_constructor() {
		let vp = VideoParams::new();
		assert_eq!(vp.width(), 0);
		assert_eq!(vp.height(), 0);
		assert_eq!(vp.depth(), 0);
		assert_eq!(vp.time_base(), (0, 1));
		assert_eq!(vp.frame_rate(), (0, 1));
		assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
		assert_eq!(vp.format(), PixelFormat::Invalid);
		assert_eq!(vp.channel_count(), 0);
		assert_eq!(vp.interlacing(), Interlacing::None);
		assert_eq!(vp.divider(), 1);
		assert!(vp.enabled());
		assert_eq!(vp.stream_index(), 0);
		assert_eq!(vp.video_type(), VideoType::Video);
		assert_eq!(vp.start_time(), 0);
		assert_eq!(vp.duration(), 0);
		assert!(!vp.premultiplied_alpha());
		assert_eq!(vp.color_range(), ColorRange::Limited);
		assert_eq!(vp.color_primaries(), 0);
		assert_eq!(vp.color_transfer(), 0);
		assert_eq!(vp.colorspace(), "");
		assert!(!vp.is_3d());
		assert!(!vp.is_valid());
	}

	#[test]
	fn new_basic_fields() {
		let vp = default_vp();
		assert_eq!(vp.width(), 1920);
		assert_eq!(vp.height(), 1080);
		assert_eq!(vp.depth(), 1);
		assert_eq!(vp.format(), PixelFormat::U8);
		assert_eq!(vp.channel_count(), 4);
		assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
		assert_eq!(vp.interlacing(), Interlacing::None);
		assert_eq!(vp.divider(), 1);
		assert_eq!(vp.time_base(), (0, 1));
		assert!(vp.is_valid());
	}

	#[test]
	fn new_basic_reduces_par() {
		// Rational(16, 15) is stored as-is; Rational(0, 1) is null -> 1/1.
		let vp = VideoParams::new_basic(100, 100, PixelFormat::U8, 3, 16, 15, 0, 1);
		assert_eq!(vp.pixel_aspect_ratio(), (16, 15));
		let vp = VideoParams::new_basic(100, 100, PixelFormat::U8, 3, 0, 5, 0, 1);
		assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
	}

	#[test]
	fn new_with_time_base() {
		let vp = VideoParams::new_with_time_base(
			1920,
			1080,
			1001,
			30000,
			PixelFormat::U8,
			4,
			1,
			1,
			0,
			1,
		);
		assert_eq!(vp.time_base(), (1001, 30000));
		// frame rate is the flipped time base.
		assert_eq!(vp.frame_rate(), (30000, 1001));
		assert_eq!(vp.frame_rate_as_time_base(), (1001, 30000));
	}

	#[test]
	fn setters_and_getters() {
		let mut vp = default_vp();
		vp.set_width(640);
		vp.set_height(480);
		vp.set_depth(2);
		vp.set_format(PixelFormat::F32);
		vp.set_channel_count(3);
		vp.set_interlacing(Interlacing::TopFirst);
		vp.set_divider(2);
		vp.set_enabled(false);
		vp.set_x(1.5);
		vp.set_y(-2.5);
		vp.set_stream_index(7);
		vp.set_video_type(VideoType::Still);
		vp.set_start_time(100);
		vp.set_duration(50);
		vp.set_premultiplied_alpha(true);
		vp.set_color_range(ColorRange::Full);
		vp.set_color_primaries(9);
		vp.set_color_transfer(18);
		vp.set_colorspace("sRGB");

		assert_eq!(vp.width(), 640);
		assert_eq!(vp.height(), 480);
		assert_eq!(vp.depth(), 2);
		assert_eq!(vp.format(), PixelFormat::F32);
		assert_eq!(vp.channel_count(), 3);
		assert_eq!(vp.interlacing(), Interlacing::TopFirst);
		assert_eq!(vp.divider(), 2);
		assert!(!vp.enabled());
		assert_eq!(vp.x(), 1.5);
		assert_eq!(vp.y(), -2.5);
		assert_eq!(vp.stream_index(), 7);
		assert_eq!(vp.video_type(), VideoType::Still);
		assert_eq!(vp.start_time(), 100);
		assert_eq!(vp.duration(), 50);
		assert!(vp.premultiplied_alpha());
		assert_eq!(vp.color_range(), ColorRange::Full);
		assert_eq!(vp.color_primaries(), 9);
		assert_eq!(vp.color_transfer(), 18);
		assert_eq!(vp.colorspace(), "sRGB");
	}

	#[test]
	fn is_3d_derived_from_depth() {
		let mut vp = default_vp();
		assert!(!vp.is_3d());
		vp.set_depth(2);
		assert!(vp.is_3d());
		assert!(vp.depth() > 1);
		vp.set_depth(1);
		assert!(!vp.is_3d());
	}

	#[test]
	fn effective_sizes() {
		let vp = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 2);
		assert_eq!(vp.effective_width(), 960);
		assert_eq!(vp.effective_height(), 540);
		assert_eq!(vp.effective_depth(), 1); // depth == 1 is not scaled
		assert_eq!(vp.square_pixel_width(), 1920);
	}

	#[test]
	fn square_pixel_width_uses_par() {
		let vp = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 16, 15, 0, 1);
		assert_eq!(vp.square_pixel_width(), 2048); // lround(1920 * 16/15)
	}

	#[test]
	fn is_valid_conditions() {
		assert!(default_vp().is_valid());
		assert!(!VideoParams::new().is_valid());
		let mut vp = default_vp();
		vp.set_channel_count(0);
		assert!(!vp.is_valid());
		let mut vp = default_vp();
		vp.set_format(PixelFormat::Invalid);
		assert!(!vp.is_valid());
		let mut vp = default_vp();
		vp.set_pixel_aspect_ratio(0, 1); // validated to 1/1, still valid
		assert!(vp.is_valid());
	}

	#[test]
	fn bytes_per_channel_for_format() {
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::U8),
			1
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::U10),
			0
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::U16),
			2
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::F16),
			2
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::F32),
			4
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::Invalid),
			0
		);
		assert_eq!(
			VideoParams::bytes_per_channel_for_format(PixelFormat::Count),
			0
		);
	}

	#[test]
	fn bytes_per_pixel_for_format() {
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U8, 4),
			4
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, 4),
			4
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, 3),
			0
		);
		assert_eq!(
			VideoParams::bytes_per_pixel_for_format(PixelFormat::F32, 4),
			16
		);
	}

	#[test]
	fn buffer_size() {
		assert_eq!(
			VideoParams::calculate_buffer_size(1920, 1080, PixelFormat::U8, 4),
			8294400
		);
		let vp = default_vp();
		assert_eq!(vp.buffer_size(), 8294400);
		assert_eq!(vp.bytes_per_channel(), 1);
		assert_eq!(vp.bytes_per_pixel(), 4);
	}

	#[test]
	fn format_is_float() {
		assert!(VideoParams::format_is_float(PixelFormat::F16));
		assert!(VideoParams::format_is_float(PixelFormat::F32));
		assert!(!VideoParams::format_is_float(PixelFormat::U8));
		assert!(!VideoParams::format_is_float(PixelFormat::U10));
		assert!(!VideoParams::format_is_float(PixelFormat::U16));
	}

	#[test]
	fn generate_auto_divider_values() {
		assert_eq!(VideoParams::generate_auto_divider(100, 100), 1);
		assert_eq!(VideoParams::generate_auto_divider(1920, 1080), 2);
		assert_eq!(VideoParams::generate_auto_divider(3840, 2160), 3);
		assert_eq!(VideoParams::generate_auto_divider(10000, 10000), 12);
		assert_eq!(VideoParams::generate_auto_divider(20000, 20000), 16);
	}

	#[test]
	fn scaled_dimension_and_target() {
		assert_eq!(VideoParams::get_scaled_dimension(1920, 2), 960);
		assert_eq!(
			VideoParams::get_divider_for_target_resolution(3840, 2160, 1920, 1080),
			2
		);
		assert_eq!(
			VideoParams::get_divider_for_target_resolution(1920, 1080, 1920, 1080),
			1
		);
	}

	#[test]
	fn name_for_divider() {
		assert_eq!(VideoParams::name_for_divider(1).unwrap(), "Full");
		assert_eq!(VideoParams::name_for_divider(2).unwrap(), "1/2");
		assert_eq!(VideoParams::name_for_divider(8).unwrap(), "1/8");
	}

	#[test]
	fn format_name() {
		assert_eq!(VideoParams::format_name(PixelFormat::U8).unwrap(), "8-bit");
		assert_eq!(
			VideoParams::format_name(PixelFormat::U10).unwrap(),
			"10-bit Packed"
		);
		assert_eq!(
			VideoParams::format_name(PixelFormat::U16).unwrap(),
			"16-bit Integer"
		);
		assert_eq!(
			VideoParams::format_name(PixelFormat::F16).unwrap(),
			"Half-Float (16-bit)"
		);
		assert_eq!(
			VideoParams::format_name(PixelFormat::F32).unwrap(),
			"Full-Float (32-bit)"
		);
		assert_eq!(
			VideoParams::format_name(PixelFormat::Invalid).unwrap(),
			"Unknown (0xFFFFFFFF)"
		);
		assert_eq!(
			VideoParams::format_name(PixelFormat::Count).unwrap(),
			"Unknown (0x5)"
		);
	}

	#[test]
	fn frame_rate_to_string_values() {
		assert_eq!(
			VideoParams::frame_rate_to_string(24000, 1001).unwrap(),
			"23.976 FPS"
		);
		assert_eq!(VideoParams::frame_rate_to_string(24, 1).unwrap(), "24 FPS");
		assert_eq!(VideoParams::frame_rate_to_string(25, 1).unwrap(), "25 FPS");
		assert_eq!(
			VideoParams::frame_rate_to_string(30000, 1001).unwrap(),
			"29.97 FPS"
		);
		assert_eq!(
			VideoParams::frame_rate_to_string(60000, 1001).unwrap(),
			"59.9401 FPS"
		);
		assert_eq!(
			VideoParams::frame_rate_to_string(48000, 1001).unwrap(),
			"47.952 FPS"
		);
		assert_eq!(VideoParams::frame_rate_to_string(1, 1).unwrap(), "1 FPS");
		assert_eq!(VideoParams::frame_rate_to_string(0, 1).unwrap(), "0 FPS");
	}

	#[test]
	fn time_in_timebase_units() {
		let mut vp = default_vp();
		// No time base set -> None.
		assert_eq!(vp.time_in_timebase_units(1, 1), None);

		vp.set_time_base(1, 1);
		assert_eq!(vp.time_in_timebase_units(1, 1), Some(1));

		vp.set_time_base(1001, 30000);
		vp.set_start_time(5);
		// 1 second at 30000/1001 fps -> 30 frames, + start_time 5.
		assert_eq!(vp.time_in_timebase_units(1, 1), Some(35));
	}

	#[test]
	fn equals_compares_domain_fields() {
		let a = default_vp();
		let b = VideoParams::new_basic(1920, 1080, PixelFormat::U8, 4, 1, 1, 0, 1);
		assert!(a.equals(&b));
		let mut c = a.clone();
		c.set_width(1919);
		assert!(!a.equals(&c));
		let mut d = a.clone();
		d.set_start_time(999); // not part of the C++ operator==
		assert!(a.equals(&d));
	}

	#[test]
	fn xml_round_trip() {
		let mut vp = VideoParams::new_with_time_base(
			1920,
			1080,
			1001,
			30000,
			PixelFormat::U8,
			4,
			16,
			15,
			1,
			2,
		);
		vp.set_enabled(true);
		vp.set_x(1.5);
		vp.set_y(-2.5);
		vp.set_stream_index(3);
		vp.set_video_type(VideoType::Still);
		vp.set_start_time(100);
		vp.set_duration(50);
		vp.set_premultiplied_alpha(true);
		vp.set_color_range(ColorRange::Full);
		vp.set_color_primaries(9);
		vp.set_color_transfer(18);
		vp.set_colorspace("sRGB");

		let xml = vp.save_xml().unwrap();
		assert!(xml.starts_with("<videoparams>"));
		assert!(xml.ends_with("</videoparams>"));
		assert!(xml.contains("<width>1920</width>"));

		let mut loaded = VideoParams::new();
		loaded.load_xml(&xml).unwrap();
		assert_eq!(loaded.width(), vp.width());
		assert_eq!(loaded.height(), vp.height());
		assert_eq!(loaded.depth(), vp.depth());
		assert_eq!(loaded.time_base(), vp.time_base());
		assert_eq!(loaded.frame_rate(), vp.frame_rate());
		assert_eq!(loaded.format(), vp.format());
		assert_eq!(loaded.channel_count(), vp.channel_count());
		assert_eq!(loaded.pixel_aspect_ratio(), vp.pixel_aspect_ratio());
		assert_eq!(loaded.interlacing(), vp.interlacing());
		assert_eq!(loaded.divider(), vp.divider());
		assert_eq!(loaded.enabled(), vp.enabled());
		assert_eq!(loaded.x(), vp.x());
		assert_eq!(loaded.y(), vp.y());
		assert_eq!(loaded.stream_index(), vp.stream_index());
		assert_eq!(loaded.video_type(), vp.video_type());
		assert_eq!(loaded.start_time(), vp.start_time());
		assert_eq!(loaded.duration(), vp.duration());
		assert_eq!(loaded.premultiplied_alpha(), vp.premultiplied_alpha());
		assert_eq!(loaded.color_range(), vp.color_range());
		assert_eq!(loaded.color_primaries(), vp.color_primaries());
		assert_eq!(loaded.color_transfer(), vp.color_transfer());
		assert_eq!(loaded.colorspace(), vp.colorspace());
		// Re-saving after a load must be byte-identical.
		assert_eq!(loaded.save_xml().unwrap(), xml);
	}

	#[test]
	fn xml_skips_unknown_elements() {
		let xml = "<videoparams><width>640</width><bogus><nested>1</nested></bogus><height>480</height></videoparams>";
		let mut vp = VideoParams::new();
		vp.load_xml(xml).unwrap();
		assert_eq!(vp.width(), 640);
		assert_eq!(vp.height(), 480);
	}

	#[test]
	fn xml_error_paths() {
		let mut vp = VideoParams::new();
		assert!(vp.load_xml("").is_err());
		assert!(vp.load_xml("not xml").is_err());
		assert!(vp.load_xml("<width>").is_err());
		assert!(vp.load_xml("<width>5</height>").is_err());
		// A non-numeric value inside a properly-rooted doc must error (stoi).
		assert!(vp
			.load_xml("<videoparams><width>notanumber</width></videoparams>")
			.is_err());
		// Self-closing element is not malformed; the empty value is accepted
		// for a text field (colorspace) but rejected for an integer field.
		assert!(vp
			.load_xml("<videoparams><width>640</width><colorspace/><depth>2</depth></videoparams>")
			.is_ok());
		assert_eq!(vp.width(), 640);
		assert_eq!(vp.depth(), 2);
		assert_eq!(vp.colorspace(), "");
		assert!(vp
			.load_xml("<videoparams><width/><colorspace>a</colorspace></videoparams>")
			.is_err());
	}

	// ---- Extended coverage --------------------------------------------------

	#[test]
	fn bytes_per_pixel_matrix() {
		// Full format × channel matrix, mirroring get_bytes_per_pixel():
		// packed u10 only supports RGBA (4); everything else is bpc*channels.
		for ch in [0, 1, 3, 4] {
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::U8, ch),
				ch
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::U16, ch),
				2 * ch
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::F16, ch),
				2 * ch
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::F32, ch),
				4 * ch
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::Invalid, ch),
				0
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::Count, ch),
				0
			);
			assert_eq!(
				VideoParams::bytes_per_pixel_for_format(PixelFormat::U10, ch),
				if ch == 4 { 4 } else { 0 }
			);
		}
	}

	#[test]
	fn bytes_per_channel_instance_and_buffer_size_f32() {
		let vp = VideoParams::new_basic(4, 2, PixelFormat::F32, 4, 1, 1, 0, 1);
		assert_eq!(vp.bytes_per_channel(), 4);
		assert_eq!(vp.bytes_per_pixel(), 16);
		assert_eq!(vp.buffer_size(), 4 * 2 * 16);
	}

	#[test]
	fn buffer_size_wraps_like_cpp_int() {
		// C++ computes int w*h*bpp (wraps on overflow); the port must use
		// wrapping arithmetic so debug builds don't panic.
		let _ = VideoParams::calculate_buffer_size(i32::MAX, i32::MAX, PixelFormat::F32, 4);
	}

	#[test]
	fn is_valid_matrix() {
		// Every real pixel format is valid (C++: format > invalid && < count).
		for f in [
			PixelFormat::U8,
			PixelFormat::U10,
			PixelFormat::U16,
			PixelFormat::F16,
			PixelFormat::F32,
		] {
			assert!(VideoParams::new_basic(2, 2, f, 4, 1, 1, 0, 1).is_valid());
		}
		// Count is out of range.
		assert!(!VideoParams::new_basic(2, 2, PixelFormat::Count, 4, 1, 1, 0, 1).is_valid());
		// Zero/negative dimensions are invalid.
		assert!(!VideoParams::new_basic(0, 2, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
		assert!(!VideoParams::new_basic(2, 0, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
		assert!(!VideoParams::new_basic(-2, 2, PixelFormat::U8, 4, 1, 1, 0, 1).is_valid());
		// Zero/negative channel counts are invalid.
		assert!(!VideoParams::new_basic(2, 2, PixelFormat::U8, 0, 1, 1, 0, 1).is_valid());
		assert!(!VideoParams::new_basic(2, 2, PixelFormat::U8, -1, 1, 1, 0, 1).is_valid());
	}

	#[test]
	fn effective_size_edges() {
		// Odd dimensions truncate like C++ integer division.
		let vp = VideoParams::new_basic(1921, 1081, PixelFormat::U8, 4, 1, 1, 0, 2);
		assert_eq!(vp.effective_width(), 960);
		assert_eq!(vp.effective_height(), 540);
		// depth > 1 is scaled by the divider; depth == 1 is not.
		let mut vp = VideoParams::new_basic(16, 16, PixelFormat::U8, 4, 1, 1, 0, 2);
		vp.set_depth(4);
		assert_eq!(vp.effective_depth(), 2);
		vp.set_depth(1);
		assert_eq!(vp.effective_depth(), 1);
	}

	#[test]
	fn square_pixel_width_rounding() {
		// std::lround rounds half away from zero: 5 * 3/2 = 7.5 -> 8.
		let vp = VideoParams::new_basic(5, 5, PixelFormat::U8, 4, 3, 2, 0, 1);
		assert_eq!(vp.square_pixel_width(), 8);
	}

	#[test]
	fn generate_auto_divider_boundaries() {
		// sqrt(1.0) == 1 -> smallest supported divider.
		assert_eq!(VideoParams::generate_auto_divider(1280, 720), 1);
		// Exactly at the top end (sqrt == 16) -> 16.
		assert_eq!(VideoParams::generate_auto_divider(20480, 11520), 16);
		// Tie between 2 and 3 (divider == 2.5 exactly): C++ picks `next`
		// because prev_diff < next_diff is false on a tie.
		assert_eq!(VideoParams::generate_auto_divider(2400, 2400), 3);
	}

	#[test]
	fn divider_for_target_resolution_edges() {
		// Source already fits -> 1.
		assert_eq!(
			VideoParams::get_divider_for_target_resolution(100, 100, 1920, 1080),
			1
		);
		// 1921/2 = 960 fits 960, 1081/2 = 540 fits 540 -> 2.
		assert_eq!(
			VideoParams::get_divider_for_target_resolution(1921, 1081, 960, 540),
			2
		);
		assert_eq!(
			VideoParams::get_divider_for_target_resolution(1921, 1081, 959, 540),
			3
		);
	}

	#[test]
	fn name_for_divider_unusual() {
		// C++ never validates; "1/" + std::to_string(div).
		assert_eq!(VideoParams::name_for_divider(0).unwrap(), "1/0");
		assert_eq!(VideoParams::name_for_divider(-2).unwrap(), "1/-2");
	}

	#[test]
	fn frame_rate_to_string_edge_values() {
		assert_eq!(
			VideoParams::frame_rate_to_string(-24, 1).unwrap(),
			"-24 FPS"
		);
		assert_eq!(
			VideoParams::frame_rate_to_string(1, 1000000).unwrap(),
			"1e-06 FPS"
		);
		assert_eq!(
			VideoParams::frame_rate_to_string(1000000, 1).unwrap(),
			"1e+06 FPS"
		);
		assert_eq!(VideoParams::frame_rate_to_string(0, 0).unwrap(), "nan FPS");
	}

	#[test]
	fn format_g_helper() {
		assert_eq!(format_g(23.976023976), "23.976");
		assert_eq!(format_g(0.0), "0");
		assert_eq!(format_g(-2.5), "-2.5");
		assert_eq!(format_g(f64::NAN), "nan");
		assert_eq!(format_g(f64::INFINITY), "inf");
		assert_eq!(format_g(f64::NEG_INFINITY), "-inf");
		assert_eq!(format_g(123456.0), "123456");
		assert_eq!(format_g(1234567.0), "1.23457e+06");
		assert_eq!(format_g(0.0001), "0.0001");
		assert_eq!(format_g(0.00001), "1e-05");
	}

	#[test]
	fn time_base_normalization() {
		let mut vp = default_vp();
		vp.set_time_base(2, 4);
		assert_eq!(vp.time_base(), (1, 2));
		vp.set_time_base(1, -2);
		assert_eq!(vp.time_base(), (-1, 2));
		vp.set_time_base(0, 5);
		assert_eq!(vp.time_base(), (0, 1));
		// Zero denominator -> NaN rational -> treated as "no time base".
		vp.set_time_base(1, 0);
		assert_eq!(vp.time_base(), (0, 0));
		assert_eq!(vp.time_in_timebase_units(1, 1), None);
	}

	#[test]
	fn pixel_aspect_ratio_sign_fix() {
		let mut vp = default_vp();
		vp.set_pixel_aspect_ratio(4, -3);
		assert_eq!(vp.pixel_aspect_ratio(), (-4, 3));
		vp.set_pixel_aspect_ratio(8, 8);
		assert_eq!(vp.pixel_aspect_ratio(), (1, 1));
	}

	#[test]
	fn frame_rate_as_time_base_null() {
		// A null frame rate flips to itself (C++ Rational::flipped no-op).
		let vp = VideoParams::new();
		assert_eq!(vp.frame_rate_as_time_base(), (0, 1));
	}

	#[test]
	fn time_in_timebase_units_edges() {
		let mut vp = default_vp();
		vp.set_time_base(1, 2);
		// Half-unit rounds away from zero (llround): 0.25s * 2 = 0.5 -> 1.
		assert_eq!(vp.time_in_timebase_units(1, 4), Some(1));
		// Negative times round away from zero too: -0.5 -> -1.
		assert_eq!(vp.time_in_timebase_units(-1, 4), Some(-1));
		// NaN time (0/0) converts to 0 + start_time, matching
		// Timecode::time_to_timestamp.
		vp.set_start_time(7);
		assert_eq!(vp.time_in_timebase_units(0, 0), Some(7));
	}

	#[test]
	fn equals_field_by_field() {
		let a = default_vp();
		let mut b = a.clone();
		b.set_height(1079);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_depth(2);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_interlacing(Interlacing::BottomFirst);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_time_base(1, 25);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_format(PixelFormat::U16);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_pixel_aspect_ratio(4, 3);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_divider(4);
		assert!(!a.equals(&b));
		let mut b = a.clone();
		b.set_channel_count(3);
		assert!(!a.equals(&b));
		// Fields outside C++ operator== don't participate.
		let mut b = a.clone();
		b.set_enabled(false);
		b.set_colorspace("rec709");
		b.set_frame_rate(24, 1);
		b.set_duration(10);
		assert!(a.equals(&b));
	}

	#[test]
	fn save_xml_exact() {
		let vp = VideoParams::new_basic(640, 480, PixelFormat::U8, 3, 1, 1, 0, 1);
		let expected = "<videoparams>\
			<width>640</width><height>480</height><depth>1</depth>\
			<timebase>0/1</timebase><format>0</format><channelcount>3</channelcount>\
			<pixelaspectratio>1/1</pixelaspectratio><interlacing>0</interlacing>\
			<divider>1</divider><enabled>1</enabled>\
			<x>0.000000</x><y>0.000000</y>\
			<streamindex>0</streamindex><videotype>0</videotype>\
			<framerate>0/1</framerate><starttime>0</starttime><duration>0</duration>\
			<premultipliedalpha>0</premultipliedalpha><colorspace></colorspace>\
			<colorrange>0</colorrange><colorprimaries>0</colorprimaries>\
			<colortransfer>0</colortransfer>\
			</videoparams>";
		assert_eq!(vp.save_xml().unwrap(), expected);
	}

	#[test]
	fn xml_round_trip_default() {
		let vp = VideoParams::new();
		let xml = vp.save_xml().unwrap();
		let mut loaded = VideoParams::new();
		loaded.load_xml(&xml).unwrap();
		assert!(loaded.equals(&vp));
		assert_eq!(loaded.save_xml().unwrap(), xml);
	}

	#[test]
	fn load_xml_with_declaration_comments_and_cdata() {
		let xml = "<?xml version=\"1.0\"?><!-- comment --><videoparams>\
			<width>640</width><colorspace><![CDATA[a <b> & c]]></colorspace></videoparams>";
		let mut vp = VideoParams::new();
		vp.load_xml(xml).unwrap();
		assert_eq!(vp.width(), 640);
		assert_eq!(vp.colorspace(), "a <b> & c");
	}

	#[test]
	fn load_xml_entities_and_boolean_coercion() {
		let xml = "<videoparams><enabled>2</enabled><premultipliedalpha>-1</premultipliedalpha>\
			<colorspace>a&amp;b</colorspace></videoparams>";
		let mut vp = VideoParams::new();
		vp.load_xml(xml).unwrap();
		assert!(vp.enabled()); // std::stoi("2") -> bool true
		assert!(vp.premultiplied_alpha()); // std::stoi("-1") -> bool true
		assert_eq!(vp.colorspace(), "a&b");
	}

	#[test]
	fn load_xml_partial_update_keeps_existing() {
		// C++ load() only assigns fields present in the document.
		let mut vp = default_vp();
		vp.load_xml("<videoparams><width>320</width></videoparams>")
			.unwrap();
		assert_eq!(vp.width(), 320);
		assert_eq!(vp.height(), 1080); // untouched
		assert_eq!(vp.format(), PixelFormat::U8);
	}

	#[test]
	fn load_xml_enum_fallbacks() {
		let xml = "<videoparams><interlacing>7</interlacing><videotype>9</videotype>\
			<colorrange>5</colorrange><format>99</format></videoparams>";
		let mut vp = default_vp();
		vp.load_xml(xml).unwrap();
		assert_eq!(vp.interlacing(), Interlacing::None);
		assert_eq!(vp.video_type(), VideoType::Video);
		assert_eq!(vp.color_range(), ColorRange::Limited);
		// Unknown format codes can't be represented in Rust; they land on
		// Invalid, which (like any out-of-range code) fails is_valid().
		assert_eq!(vp.format(), PixelFormat::Invalid);
		assert!(!vp.is_valid());
	}

	#[test]
	fn load_xml_numeric_prefix_parsing() {
		let mut vp = VideoParams::new();
		// std::stoi/std::stof consume the longest valid prefix, ignore junk.
		vp.load_xml(
			"<videoparams><width>640abc</width><x>1.5garbage</x><y> -2.5 </y></videoparams>",
		)
		.unwrap();
		assert_eq!(vp.width(), 640);
		assert_eq!(vp.x(), 1.5);
		assert_eq!(vp.y(), -2.5);
	}

	#[test]
	fn parse_f32_field_prefixes() {
		assert_eq!(parse_f32_field("1.5").unwrap(), 1.5);
		assert_eq!(parse_f32_field("  -2.5  ").unwrap(), -2.5);
		assert_eq!(parse_f32_field("1.5junk").unwrap(), 1.5);
		assert_eq!(parse_f32_field(".5").unwrap(), 0.5);
		assert_eq!(parse_f32_field("2.").unwrap(), 2.0);
		assert_eq!(parse_f32_field("1e3").unwrap(), 1000.0);
		assert_eq!(parse_f32_field("1.25e-2x").unwrap(), 0.0125);
		// An exponent with no digits is not part of the prefix.
		assert_eq!(parse_f32_field("1.5e").unwrap(), 1.5);
		assert!(parse_f32_field("").is_err());
		assert!(parse_f32_field("abc").is_err());
		assert!(parse_f32_field(".e5").is_err());
		assert!(parse_f32_field("-").is_err());
	}

	#[test]
	fn load_xml_malformed_entities_fail() {
		let mut vp = VideoParams::new();
		// Unknown entity in character data is a parse error in this reader
		// (stricter than the subtitle reader, which preserves it verbatim).
		assert!(vp
			.load_xml("<videoparams><colorspace>a &bogus; b</colorspace></videoparams>")
			.is_err());
		// Unterminated comment.
		assert!(vp.load_xml("<videoparams><!-- never ends").is_err());
		// Mismatched nesting.
		assert!(vp
			.load_xml("<videoparams><width>1</videoparams></width>")
			.is_err());
	}

	#[test]
	fn rational_helpers_match_oakcore() {
		// The hand-rolled tuple rationals must agree with oak_core::Rational,
		// the canonical port of the C++ Rational.
		for (n, d) in [
			(2i32, 4i32),
			(0, 5),
			(5, 0),
			(-3, -1),
			(1, -2),
			(7, 3),
			(100, 10),
		] {
			let r = crate::Rational::new(n as i64, d as i64);
			assert_eq!(
				make_rational(n, d),
				(r.numerator() as i32, r.denominator() as i32)
			);
		}
		for s in ["1/2", "7", "4/2", "junk", "a/b", "1/2/3", "-6/3"] {
			let r = crate::Rational::from_string(s);
			assert_eq!(
				rational_from_string(s),
				(r.numerator() as i32, r.denominator() as i32),
				"input {s:?}"
			);
		}
	}

	#[test]
	fn time_conversion_matches_oakcore() {
		let mut vp = VideoParams::new_with_time_base(
			1920,
			1080,
			1001,
			30000,
			PixelFormat::U8,
			4,
			1,
			1,
			0,
			1,
		);
		vp.set_start_time(11);
		let tb = crate::Rational::new(1001, 30000);
		for (n, d) in [(1i32, 1i32), (1, 2), (24000, 1001), (-3, 1), (0, 1)] {
			let expected = tb.time_to_timestamp(crate::Rational::new(n as i64, d as i64)) + 11;
			assert_eq!(vp.time_in_timebase_units(n, d), Some(expected));
		}
	}
}
