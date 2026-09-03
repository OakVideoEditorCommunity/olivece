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

//! CPU frame payloads and the [`VideoParamsPod`] value (the Rust mirror of
//! the `oakrender_video_params` POD in `include/render/renderer.h`).
//!
//! The C ABI frame functions (`oakrender_codec_frame_*`) marshal this
//! type; the FFI layer stores [`Frame`] values in `OakCodecFrame` handles.

use crate::PixelFormat;
use crate::Rational;

/// Mirror of the `oakrender_video_params` POD (include/render/renderer.h,
/// field order and semantics verbatim). Stored inside [`crate::texture::Frame`]
/// so the ffi `*_get_params` exports can report the full metadata.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoParamsPod {
	/// Frame width (full resolution).
	pub width: i32,
	/// Frame height (full resolution).
	pub height: i32,
	/// Frame duration numerator (e.g. 1001/30000 s).
	pub time_base_num: i32,
	/// Frame duration denominator.
	pub time_base_den: i32,
	/// `olive::PixelFormat::Format` as int.
	pub format: i32,
	/// Pixel aspect numerator.
	pub pixel_aspect_num: i32,
	/// Pixel aspect denominator.
	pub pixel_aspect_den: i32,
	/// `olive::VideoParams::Interlacing` as int.
	pub interlacing: i32,
	/// `olive::VideoParams::ColorRange` as int.
	pub color_range: i32,
	/// Preview resolution divider (1 = full).
	pub divider: i32,
	/// `olive::VideoParams::Type` (0 = video).
	pub video_type: i32,
	/// 0/1.
	pub premultiplied_alpha: i32,
}

impl Default for VideoParamsPod {
	fn default() -> Self {
		Self {
			width: 0,
			height: 0,
			time_base_num: 1,
			time_base_den: 1,
			format: PixelFormat::F32 as i32,
			pixel_aspect_num: 1,
			pixel_aspect_den: 1,
			interlacing: 0,
			color_range: 0,
			divider: 1,
			video_type: 0,
			premultiplied_alpha: 0,
		}
	}
}

impl VideoParamsPod {
	/// The default render size used when a ticket carries no force size and
	/// the output node's video params cannot be queried (oak_core bridge
	/// pending).
	pub const DEFAULT_WIDTH: i32 = 1920;
	/// See [`VideoParamsPod::DEFAULT_WIDTH`].
	pub const DEFAULT_HEIGHT: i32 = 1080;

	/// Frame rate as a rational (time base flipped; null for a null time
	/// base).
	pub fn frame_rate(&self) -> Rational {
		if self.time_base_num <= 0 || self.time_base_den <= 0 {
			return Rational::NULL;
		}
		Rational::new(self.time_base_den as i64, self.time_base_num as i64)
	}

	/// The time base (frame duration).
	pub fn time_base(&self) -> Rational {
		if self.time_base_num <= 0 || self.time_base_den <= 0 {
			return Rational::NULL;
		}
		Rational::new(self.time_base_num as i64, self.time_base_den as i64)
	}

	/// The internal (fixed) channel count — the C++ engine uses 4 for the
	/// video pipeline; not exposed in the POD.
	pub const INTERNAL_CHANNEL_COUNT: i32 = 4;

	/// Pixel dimensions of the data buffer honoring the preview divider
	/// (C++ `VideoParams::effective_width/height` with a divider).
	pub fn effective_width(&self) -> i32 {
		(self.width / self.divider.max(1)).max(0)
	}

	/// See [`VideoParamsPod::effective_width`].
	pub fn effective_height(&self) -> i32 {
		(self.height / self.divider.max(1)).max(0)
	}
}

#[cfg(test)]
mod tests {
    use crate::PixelFormat;
use super::*;

    #[test]
	fn pod_defaults() {
		let p = VideoParamsPod::default();
		assert_eq!(p.format, PixelFormat::F32 as i32);
		assert_eq!(p.time_base(), Rational::new(1, 1));
	}

	#[test]
	fn effective_size_honors_divider() {
		let mut p = VideoParamsPod::default();
		p.width = 3840;
		p.height = 2160;
		p.divider = 2;
		assert_eq!(p.effective_width(), 1920);
		assert_eq!(p.effective_height(), 1080);
	}

	#[test]
	fn frame_rate_is_flipped_time_base() {
		let mut p = VideoParamsPod::default();
		p.time_base_num = 1001;
		p.time_base_den = 30000;
		assert_eq!(p.frame_rate(), Rational::new(30000, 1001));
		assert_eq!(p.time_base(), Rational::new(1001, 30000));
	}
}
