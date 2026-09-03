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

//! `olive::Frame` — a CPU pixel buffer plus a [`VideoParams`] value.
//!
//! Mirrors `src/codec/src/frame.h`. The params are held as an oak_core
//! [`VideoParams`] value (single-lib unification; the former refcounted
//! oak_core handle is gone, so copies are plain clones); the pixel data
//! itself is a plain `Vec<u8>`. Line-size and pixel-format math lives
//! here.

use oak_core::videoparams::VideoParams;
use oak_core::{PixelFormat, Rational};

/// Number of channels in the internal RGBA pipeline layout
/// (`VideoParams::k_internal_channel_count == k_rgba_channel_count == 4`).
/// The frame math (linesize, per-pixel offsets) always assumes this layout,
/// matching the C++ decoder path which produces/consumes RGBA frames.
const VIDEO_CHANNELS: i32 = 4;

/// Interlacing of a frame's parameter set (VideoParams::Interlacing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Interlacing {
	/// Progressive.
	None = 0,
	/// Upper field first.
	TopFieldFirst = 1,
	/// Lower field first.
	BottomFieldFirst = 2,
}

/// `olive::Frame`: reference-counted CPU pixel buffer + params value.
#[derive(Debug)]
pub struct Frame {
	/// Video parameter set.
	pub params: Option<VideoParams>,
	/// Pixel buffer (unallocated until `allocate`).
	data: Vec<u8>,
	/// Distance between rows in bytes (0 until params are set).
	linesize_bytes: i32,
	/// Timestamp, rational seconds.
	timestamp: Rational,
	/// Allocated pixel format (may differ from params while converting).
	allocated_format: PixelFormat,
}

/// Map an `OakPixelFormat` int code back to a `PixelFormat` (unknown codes
/// become [`PixelFormat::Invalid`]).
fn pixel_format_from_i32(v: i32) -> PixelFormat {
	match v {
		0 => PixelFormat::U8,
		1 => PixelFormat::U10,
		2 => PixelFormat::U16,
		3 => PixelFormat::F16,
		4 => PixelFormat::F32,
		_ => PixelFormat::Invalid,
	}
}

/// Bytes per pixel for `format` at `channels`, matching
/// `VideoParams::get_bytes_per_pixel`:
/// - U10 is a packed RGBA10A2 pixel: 4 bytes for the RGBA layout,
///   regardless of channel count; anything else is rejected (0).
/// - All other formats are `bytes_per_channel * channels`.
fn bytes_per_pixel(format: PixelFormat, channels: i32) -> i32 {
	if format == PixelFormat::U10 {
		return if channels == VIDEO_CHANNELS { 4 } else { 0 };
	}
	(format.bytes_per_channel() as i32) * channels
}

impl Frame {
	/// New frame with default (invalid) params; buffer unallocated.
	pub fn new() -> Self {
		Frame {
			params: Some(VideoParams::new()),
			data: Vec::new(),
			linesize_bytes: 0,
			timestamp: Rational::new(0, 1),
			allocated_format: PixelFormat::Invalid,
		}
	}

	/// New frame with a copy of `params`.
	pub fn with_params(params: VideoParams) -> Self {
		let mut frame = Frame {
			params: Some(params),
			data: Vec::new(),
			linesize_bytes: 0,
			timestamp: Rational::new(0, 1),
			allocated_format: PixelFormat::Invalid,
		};
		frame.recompute_linesize();
		frame
	}

	/// The video parameter set, or `None` when empty.
	pub fn params(&self) -> Option<&VideoParams> {
		self.params.as_ref()
	}

	/// Replace the parameter set, recompute line sizes, do NOT reallocate
	/// the buffer.
	pub fn set_params(&mut self, params: VideoParams) {
		self.params = Some(params);
		self.recompute_linesize();
		// Deliberately do not touch `data`: an existing buffer keeps its
		// layout; `allocated_format` stays at the old format until the next
		// `allocate()`.
	}

	/// Recompute `linesize_bytes` from the current params (0 when unset).
	fn recompute_linesize(&mut self) {
		self.linesize_bytes = match &self.params {
			Some(p) => {
				let fmt = pixel_format_from_i32(p.format().code());
				Self::generate_linesize_bytes(fmt, p.width())
			}
			None => 0,
		};
	}

	/// Allocate the pixel buffer from the current params.
	pub fn allocate(&mut self) -> crate::error::Result<()> {
		let params = match &self.params {
			Some(p) => p,
			None => return Err(crate::error::Error::State),
		};

		if !params.is_valid() {
			return Err(crate::error::Error::State);
		}

		if self.is_allocated() {
			// Already allocated; leave the buffer alone.
			return Ok(());
		}

		let width = params.width();
		let height = params.height();
		let format = pixel_format_from_i32(params.format().code());

		let linesize = Self::generate_linesize_bytes(format, width);
		let size = (linesize as usize).wrapping_mul(height as usize);

		self.data.resize(size, 0);
		self.linesize_bytes = linesize;
		self.allocated_format = format;

		Ok(())
	}

	/// 1 when the pixel buffer is allocated.
	pub fn is_allocated(&self) -> bool {
		!self.data.is_empty()
	}

	/// Writable pixel buffer slice, or `None` when unallocated.
	pub fn data(&self) -> Option<&[u8]> {
		if self.is_allocated() {
			Some(&self.data)
		} else {
			None
		}
	}

	/// Mutable pixel buffer slice, or `None` when unallocated.
	pub fn data_mut(&mut self) -> Option<&mut [u8]> {
		if self.is_allocated() {
			Some(&mut self.data)
		} else {
			None
		}
	}

	/// Size of the pixel buffer in bytes (0 when unallocated).
	pub fn allocated_size(&self) -> usize {
		self.data.len()
	}

	/// Distance between two rows in bytes (0 when params are unset).
	pub fn linesize_bytes(&self) -> i32 {
		self.linesize_bytes
	}

	/// Distance between two rows in pixels.
	pub fn linesize_pixels(&self) -> i32 {
		let bpp = self.bytes_per_pixel();
		if bpp > 0 {
			self.linesize_bytes / bpp
		} else {
			0
		}
	}

	/// Bytes per pixel for the current params format (RGBA layout).
	fn bytes_per_pixel(&self) -> i32 {
		bytes_per_pixel(self.format(), VIDEO_CHANNELS)
	}

	/// Frame width in pixels (0 when params are empty).
	pub fn width(&self) -> i32 {
		match &self.params {
			Some(p) => p.width(),
			None => 0,
		}
	}

	/// Frame height in pixels (0 when params are empty).
	pub fn height(&self) -> i32 {
		match &self.params {
			Some(p) => p.height(),
			None => 0,
		}
	}

	/// Pixel format (`OakPixelFormat` value).
	pub fn format(&self) -> PixelFormat {
		match &self.params {
			Some(p) => pixel_format_from_i32(p.format().code()),
			None => PixelFormat::Invalid,
		}
	}

	/// Plane channel count of the params format.
	///
	/// # CPP-PARITY
	/// `src/codec/src/frame.h` reads this from the params via
	/// `VideoParams::channel_count`. Decoder frames are always produced in
	/// the internal RGBA layout, so this returns [`VIDEO_CHANNELS`] (4).
	pub fn channel_count(&self) -> i32 {
		VIDEO_CHANNELS
	}

	/// Timestamp as a rational number of seconds.
	pub fn timestamp(&self) -> Rational {
		self.timestamp
	}

	/// Set the timestamp.
	pub fn set_timestamp(&mut self, ts: Rational) {
		self.timestamp = ts;
	}

	/// Distance between rows for a (format, width) pair, in bytes.
	///
	/// Matches `Frame::generate_linesize_bytes(width, format, channel_count)`
	/// in `src/codec/src/frame.cpp` with `channel_count` fixed at
	/// [`VIDEO_CHANNELS`]: bytes per pixel times the width rounded up to a
	/// 32-byte boundary. Uses wrapping arithmetic so extreme (or negative)
	/// widths behave like the C++ `int` math rather than panicking.
	pub fn generate_linesize_bytes(format: PixelFormat, width: i32) -> i32 {
		let bpp = bytes_per_pixel(format, VIDEO_CHANNELS);
		let aligned = width.wrapping_add(31) & !31;
		bpp.wrapping_mul(aligned)
	}

	/// Convert the buffer to another pixel format (`convert_to_olive_format`).
	///
	/// # CPP-PARITY
	/// `src/codec/src/frame.cpp` — the destination params are carried by
	/// the C++ callers via `VideoParams` setters; Rust keeps the
	/// equivalent state in `self.params`.
	///
	/// When the current params format already matches the format the buffer
	/// was allocated in, this is a no-op (`Ok`). A genuine pixel-format
	/// conversion requires the OIIO bridge (`convert_to_olive_format`), which
	/// is not yet ported to the pure-Rust crate; until then a mismatched
	/// conversion is rejected with [`crate::error::Error::State`].
	pub fn convert(&mut self) -> crate::error::Result<()> {
		if !self.is_allocated() {
			return Err(crate::error::Error::State);
		}
		let fmt = self.format();
		if self.allocated_format == fmt {
			self.recompute_linesize();
			return Ok(());
		}
		Err(crate::error::Error::State)
	}

	/// True when `(x, y)` lies inside the allocated buffer.
	fn contains_pixel(&self, x: i32, y: i32) -> bool {
		self.is_allocated() && x >= 0 && x < self.width() && y >= 0 && y < self.height()
	}

	/// Read a pixel sample at (x, y).
	///
	/// Returns the first byte of the pixel at `(x, y)` (the R channel for
	/// RGBA). Out-of-bounds reads return 0, matching the C++ default
	/// (transparent black) color.
	pub fn get_pixel(&self, x: i32, y: i32) -> u8 {
		if !self.contains_pixel(x, y) {
			return 0;
		}
		let offset = (y as usize).wrapping_mul(self.linesize_bytes as usize)
			+ (x as usize).wrapping_mul(self.bytes_per_pixel() as usize);
		*self.data.get(offset).unwrap_or(&0)
	}

	/// Write a pixel sample at (x, y).
	///
	/// Writes `value` to the first byte of the pixel at `(x, y)`. Out-of-bounds
	/// writes are ignored, matching the C++ `set_pixel`.
	pub fn set_pixel(&mut self, x: i32, y: i32, value: u8) {
		if !self.contains_pixel(x, y) {
			return;
		}
		let offset = (y as usize).wrapping_mul(self.linesize_bytes as usize)
			+ (x as usize).wrapping_mul(self.bytes_per_pixel() as usize);
		if let Some(byte) = self.data.get_mut(offset) {
			*byte = value;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use oak_core::ocioutils::PixelFormat as OakPixelFormat;

	fn frame(w: i32, h: i32) -> Frame {
		let params = VideoParams::new_basic(w, h, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
		Frame::with_params(params)
	}

	#[test]
	fn linesize_is_32_byte_aligned_for_u8() {
		// U8 RGBA: 4 bytes/pixel, width rounded up to a 32-byte boundary.
		assert_eq!(Frame::generate_linesize_bytes(PixelFormat::U8, 9), 4 * 32);
		assert_eq!(
			Frame::generate_linesize_bytes(PixelFormat::U8, 100),
			4 * 128
		);
		assert_eq!(Frame::generate_linesize_bytes(PixelFormat::U8, 0), 0);
	}

	#[test]
	fn linesize_respects_16bit_and_u10() {
		assert_eq!(Frame::generate_linesize_bytes(PixelFormat::U16, 16), 8 * 32);
		// U10 is a packed 4-byte RGBA pixel regardless of channel count.
		assert_eq!(Frame::generate_linesize_bytes(PixelFormat::U10, 16), 4 * 32);
	}

	#[test]
	fn allocate_fills_buffer_and_reports_size() {
		let mut f = frame(100, 50);
		assert!(!f.is_allocated());
		assert_eq!(f.allocated_size(), 0);
		assert!(f.data().is_none());

		f.allocate().unwrap();
		assert!(f.is_allocated());
		assert_eq!(f.allocated_size(), (4 * 128) * 50);
		assert_eq!(f.data().map(|d| d.len()), Some((4 * 128) * 50));
		assert_eq!(f.linesize_bytes(), 4 * 128);
	}

	#[test]
	fn allocate_invalid_params_is_error() {
		// init_basic(0, 0) is not valid -> allocate must reject.
		let mut f = frame(0, 0);
		assert!(f.allocate().is_err());
	}

	#[test]
	fn get_set_pixel_round_trip() {
		let mut f = frame(100, 50);
		f.allocate().unwrap();

		f.set_pixel(3, 4, 0xAB);
		assert_eq!(f.get_pixel(3, 4), 0xAB);

		// pixel (0,0) is the first byte; pixel (1,0) is bpp bytes later.
		f.set_pixel(0, 0, 0x11);
		f.set_pixel(1, 0, 0x22);
		assert_eq!(f.get_pixel(0, 0), 0x11);
		assert_eq!(f.get_pixel(1, 0), 0x22);
	}

	#[test]
	fn out_of_bounds_reads_zero_and_writes_ignored() {
		let mut f = frame(10, 10);
		f.allocate().unwrap();
		assert_eq!(f.get_pixel(50, 50), 0);
		assert_eq!(f.get_pixel(-1, 0), 0);
		f.set_pixel(50, 50, 0xFF);
		// untouched
		assert_eq!(f.data().unwrap()[0], 0);
	}

	#[test]
	fn set_params_recomputes_linesize_without_realloc() {
		let params = VideoParams::new_basic(10, 10, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
		let mut f = Frame::with_params(params);
		f.allocate().unwrap();
		let before = f.allocated_size();

		let wider =
			VideoParams::new_basic(100, 10, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
		f.set_params(wider);
		// linesize reflects the new width, but the buffer is untouched.
		assert_eq!(f.linesize_bytes(), 4 * 128);
		assert_eq!(f.allocated_size(), before);
	}
}

#[cfg(test)]
mod tests_extra {
	use super::*;
	use oak_core::ocioutils::PixelFormat as OakPixelFormat;

	fn frame(w: i32, h: i32) -> Frame {
		let params = VideoParams::new_basic(w, h, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
		Frame::with_params(params)
	}

	#[test]
	fn linesize_pixels_derives_from_bytes() {
		// U8 RGBA: bpp 4 -> linesize_pixels = linesize_bytes / 4.
		let mut f = frame(32, 16);
		f.allocate().unwrap();
		assert_eq!(f.linesize_bytes(), 4 * 32);
		assert_eq!(f.linesize_pixels(), 32);
		// Unallocated / unset params -> 0.
		let g = Frame::new();
		assert_eq!(g.linesize_pixels(), 0);
	}

	#[test]
	fn channel_count_is_internal_rgba_layout() {
		let f = frame(4, 4);
		assert_eq!(f.channel_count(), VIDEO_CHANNELS);
	}

	#[test]
	fn convert_is_noop_when_format_matches() {
		let mut f = frame(16, 16);
		f.allocate().unwrap();
		// allocated U8 == params U8 -> no-op Ok.
		assert!(f.convert().is_ok());
		// Unallocated -> Err(State).
		let mut g = Frame::new();
		assert!(g.convert().is_err());
	}

	#[test]
	fn pixel_format_from_unknown_code_is_invalid() {
		let mut p = VideoParams::new_basic(1, 1, OakPixelFormat::from_code(0), 4, 1, 1, 0, 1);
		p.set_format(OakPixelFormat::from_code(99));
		let f = Frame::with_params(p);
		assert_eq!(f.format(), PixelFormat::Invalid);
	}

	#[test]
	fn default_timestamp_is_zero() {
		let f = Frame::new();
		let ts = f.timestamp();
		assert_eq!(ts.numerator(), 0);
		assert_eq!(ts.denominator(), 1);
	}
}
