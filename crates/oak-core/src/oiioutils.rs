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

//! OpenImageIO utility queries, mirroring `src/common/src/oiioutils.h`
//! and `include/common/oiioutils.h`. Reuses [`crate::ocioutils::PixelFormat`]
//! rather than redefining the pixel format codes. The object is stateless;
//! the handle only satisfies the C API lifetime contract.
//!
//! The base-type mapping is derived from the `image` crate's own color-type
//! tables (per-channel bit depth → OIIO `TypeDesc::BASETYPE` code), with the
//! half-float case pinned from the frozen OIIO table since `image` 0.25 has
//! no f16 sample type. Aspect-ratio conversion uses
//! `oak_core::Rational::from_double`, the C++ `Rational::from_double`
//! port of FFmpeg's `av_d2q` (kept as a hand-written port rather than
//! pulling in `ffmpeg-next` — see README decision 6). 32-bit float image I/O
//! is provided by [`F32Image`] / [`read_image_f32`] / [`write_image_f32`],
//! built on the `image` crate.

use crate::error::{Error, Result};
use crate::ocioutils::PixelFormat;
use image::{ExtendedColorType, ImageBuffer, Rgb, Rgba};
use crate::Rational;

/// OIIO base type codes, matching `OIIO::TypeDesc::BASETYPE`.
///
/// Values cross the C ABI as plain ints. See the doc comment on
/// `OakOIIOUtils` in `include/common/oiioutils.h`: 0 = UNKNOWN, 1 = NONE,
/// 2 = UINT8, 3 = INT8, 4 = UINT16, 5 = INT16, 6 = UINT32, 7 = INT32,
/// 8 = UINT64, 9 = INT64, 10 = HALF, 11 = FLOAT, 12 = DOUBLE, 13 = STRING,
/// 14 = PTR (OIIO >= 2.5 additionally has 15 = USTRINGHASH).
mod basetype {
	/// Unknown / unmappable base type.
	pub(crate) const UNKNOWN: i32 = 0;
	/// 8-bit unsigned integer.
	pub(crate) const UINT8: i32 = 2;
	/// 16-bit unsigned integer.
	pub(crate) const UINT16: i32 = 4;
	/// 16-bit float (half).
	pub(crate) const HALF: i32 = 10;
	/// 32-bit float.
	pub(crate) const FLOAT: i32 = 11;
}

/// The OIIO utils family (stateless).
pub struct OIIOUtils;

impl OIIOUtils {
	/// Creates the OIIOUtils object.
	pub fn new() -> Self {
		Self
	}

	/// Map a native pixel format to an OIIO base type code. On invalid or
	/// unmappable formats returns `Ok(TypeDesc::UNKNOWN = 0)`.
	///
	/// CPP-PARITY: matches `OIIOUtils::get_oiio_base_type_from_format`. The
	/// per-channel bit depth comes from the `image` crate's own color-type
	/// tables (see [`image_color_type_for`]); the bit depth is then mapped to
	/// the OIIO base-type codes frozen in `include/common/oiioutils.h`.
	/// `u10` has no image/OIIO representation and maps to UNKNOWN;
	/// `invalid`/`count` also fall through to UNKNOWN (the C++ `break`s out
	/// of the switch and returns UNKNOWN). `f16` is a documented exception:
	/// `image` 0.25 has no half-float sample type, so HALF is pinned from the
	/// frozen OIIO table.
	pub fn get_oiio_base_type_from_format(&self, pixel_format: PixelFormat) -> Result<i32> {
		let base_type = match image_color_type_for(pixel_format) {
			Some(color_type) => match bits_per_channel(color_type) {
				8 => basetype::UINT8,
				16 => basetype::UINT16,
				32 => basetype::FLOAT,
				// Unreachable for the mapped types (L8=8, L16=16, Rgb32F=32);
				// kept as a fallback for future mappings.
				_ => basetype::UNKNOWN,
			},
			None => match pixel_format {
				// image 0.25 has no f16 sample type (see the `// TODO f16
				// types?` note in image's color.rs), so HALF is pinned from
				// the frozen OIIO base-type table (CPP-PARITY).
				PixelFormat::F16 => basetype::HALF,
				_ => basetype::UNKNOWN,
			},
		};
		Ok(base_type)
	}

	/// Map an OIIO base type code to a native pixel format. On unknown or
	/// unmappable base types returns `Ok(PixelFormat::Invalid)`; a negative
	/// base type is an error.
	///
	/// CPP-PARITY: matches `OIIOUtils::get_format_from_oiio_basetype`. The
	/// known-but-unmappable types (INT8/INT16/INT32/UINT32/INT64/UINT64/
	/// STRING/PTR/DOUBLE/LASTBASE) print to stderr in C++ and return
	/// `invalid`; here they all fall to `Ok(PixelFormat::Invalid)`. The
	/// `base_type < 0` error mirrors the `oak_core_oiioutils_get_format_from_oiio_basetype`
	/// c_api guard; the `>= LASTBASE` upper-bound guard is likewise a c_api
	/// concern and is not replicated in the domain function.
	pub fn get_format_from_oiio_basetype(&self, base_type: i32) -> Result<PixelFormat> {
		if base_type < 0 {
			return Err(Error::Invalid);
		}
		Ok(match base_type {
			basetype::UINT8 => PixelFormat::U8,
			basetype::UINT16 => PixelFormat::U16,
			basetype::HALF => PixelFormat::F16,
			basetype::FLOAT => PixelFormat::F32,
			_ => PixelFormat::Invalid,
		})
	}

	/// Convert a `PixelAspectRatio` attribute value to a reduced
	/// numerator/denominator pair.
	///
	/// CPP-PARITY: `olive::core::Rational::from_double`
	/// (`core/src/util/rational.cpp:39`) via oakcore-rs
	/// [`Rational::from_double`]. NaN and `|x| > INT_MAX + 3` yield the
	/// oracle's NaN rational `(0, 0)`; `0.0` reduces to `(0, 1)`. The
	/// c_api wrapper never fails for a valid `pixel_aspect_ratio`, so
	/// this always returns `Ok`.
	pub fn get_pixel_aspect_ratio(&self, pixel_aspect_ratio: f64) -> Result<(i32, i32)> {
		let r = Rational::from_double(pixel_aspect_ratio);
		// from_double caps the reduction at i32::MAX, so the cast is lossless.
		Ok((r.numerator() as i32, r.denominator() as i32))
	}
}

/// Map a native pixel format to the `image` crate's extended color type, or
/// `None` when the format has no image representation.
///
/// CPP-PARITY: the per-channel bit depth drives
/// [`OIIOUtils::get_oiio_base_type_from_format`]; this is the "which OIIO
/// base type would `image` emit" pivot. `u8`/`u16` map to `L8`/`L16`, `f32` to
/// `Rgb32F` (the `image` crate's only 32-bit float RGB layout; channel
/// count is derived separately via [`bits_per_channel`]). `u10` has no
/// `image`/OIIO representation, `f16` has no `image` sample type (the crate
/// has no half float; see the `// TODO f16 types?` note in its color tables),
/// and `invalid`/`count` fall through — all `None`.
fn image_color_type_for(pixel_format: PixelFormat) -> Option<ExtendedColorType> {
	match pixel_format {
		PixelFormat::U8 => Some(ExtendedColorType::L8),
		PixelFormat::U10 => None,
		PixelFormat::U16 => Some(ExtendedColorType::L16),
		PixelFormat::F16 => None,
		PixelFormat::F32 => Some(ExtendedColorType::Rgb32F),
		PixelFormat::Invalid | PixelFormat::Count => None,
	}
}

/// Per-channel bit depth of an `ExtendedColorType` (derived from the crate's
/// own tables rather than a hand-maintained list).
fn bits_per_channel(color_type: ExtendedColorType) -> u32 {
	// `ExtendedColorType` is `#[non_exhaustive]`; the mapped types are the
	// only ones fed in, so any new variant the crate adds would surface as a
	// `0` here and fall through to UNKNOWN upstream.
	let bits_per_pixel = color_type.bits_per_pixel();
	let channel_count = color_type.channel_count();
	if channel_count == 0 {
		return 0;
	}
	bits_per_pixel as u32 / channel_count as u32
}

/// A decoded image stored as 32-bit float, packed row-major with `channels`
/// interleaved values per pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct F32Image {
	/// Image width in pixels.
	pub width: i32,
	/// Image height in pixels.
	pub height: i32,
	/// Interleaved values per pixel (1 = luma, 2 = luma+alpha, 3 = RGB,
	/// 4 = RGBA).
	pub channels: i32,
	/// The interleaved float pixel data.
	pub pixels: Vec<f32>,
}

impl F32Image {
	/// Total number of float values.
	pub fn len(&self) -> usize {
		self.pixels.len()
	}

	/// Whether the image contains no pixels.
	pub fn is_empty(&self) -> bool {
		self.pixels.is_empty()
	}

	/// Access to the interleaved pixel data.
	pub fn as_slice(&self) -> &[f32] {
		&self.pixels
	}
}

/// Reads a TIFF image as 32-bit float.
///
/// The channel count comes from the file itself; RGBA images yield
/// `channels == 4`, RGB `3`, gray+alpha `2`, grayscale `1`. Lower bit depths
/// are upscaled to float. Only the TIFF format is enabled in this crate's
/// `image` dependency; other formats fail with an error.
pub fn read_image_f32(path: &str) -> Result<F32Image> {
	let img = image::open(path).map_err(|e| Error::new(format!("image::read_image_f32: {e}")))?;
	let width = img.width() as i32;
	let height = img.height() as i32;
	let channels = img.color().channel_count() as i32;
	let pixels = match channels {
		1 => img.to_luma32f().into_raw(),
		2 => img.to_luma_alpha32f().into_raw(),
		3 => img.to_rgb32f().into_raw(),
		4 => img.to_rgba32f().into_raw(),
		n => {
			return Err(Error::new(format!(
				"image::read_image_f32: unsupported channel count {n}"
			)))
		}
	};
	Ok(F32Image {
		width,
		height,
		channels,
		pixels,
	})
}

/// Writes 32-bit float pixels to `path` as a TIFF (format inferred from the
/// `.tif`/`.tiff` extension).
///
/// `pixels` must hold exactly `width * height * channels` values, interleaved
/// row-major. `channels` must be 3 (RGB) or 4 (RGBA): the TIFF encoder in the
/// `image` crate supports 32-bit float only for those two layouts, so 1- and
/// 2-channel writes are rejected with an error.
pub fn write_image_f32(
	path: &str,
	width: i32,
	height: i32,
	channels: i32,
	pixels: &[f32],
) -> Result<()> {
	if width <= 0 || height <= 0 {
		return Err(Error::new("image::write_image_f32: invalid dimensions"));
	}
	if channels != 3 && channels != 4 {
		return Err(Error::new(format!(
			"image::write_image_f32: unsupported channel count {channels} (TIFF float writes support RGB=3 or RGBA=4 only)"
		)));
	}
	let expected = (width as i64) * (height as i64) * (channels as i64);
	if expected != pixels.len() as i64 {
		return Err(Error::new(format!(
			"image::write_image_f32: pixel buffer length {} does not match {width}x{height}x{channels} = {expected}",
			pixels.len()
		)));
	}
	// Only the TIFF feature is enabled; anything else is a caller mistake.
	let format = image::ImageFormat::from_path(path)
		.map_err(|e| Error::new(format!("image::write_image_f32: {e}")))?;
	if format != image::ImageFormat::Tiff {
		return Err(Error::new(format!(
			"image::write_image_f32: unsupported image format for '{path}' (only TIFF is enabled)"
		)));
	}

	let (w, h) = (width as u32, height as u32);
	let result =
		if channels == 3 {
			let buf = ImageBuffer::<Rgb<f32>, Vec<f32>>::from_raw(w, h, pixels.to_vec())
				.ok_or_else(|| {
					Error::new(format!(
					"image::write_image_f32: pixel buffer does not match {width}x{height}x{channels}"
				))
				})?;
			buf.save_with_format(path, image::ImageFormat::Tiff)
		} else {
			let buf = ImageBuffer::<Rgba<f32>, Vec<f32>>::from_raw(w, h, pixels.to_vec())
				.ok_or_else(|| {
					Error::new(format!(
					"image::write_image_f32: pixel buffer does not match {width}x{height}x{channels}"
				))
				})?;
			buf.save_with_format(path, image::ImageFormat::Tiff)
		};
	result.map_err(|e| Error::new(format!("image::write_image_f32: {e}")))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn utils() -> OIIOUtils {
		OIIOUtils::new()
	}

	#[test]
	fn base_type_from_format_mapping() {
		let u = utils();
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::U8).unwrap(),
			2
		); // UINT8
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::U10).unwrap(),
			0
		); // UNKNOWN
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::U16).unwrap(),
			4
		); // UINT16
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::F16).unwrap(),
			10
		); // HALF
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::F32).unwrap(),
			11
		); // FLOAT
	 // Invalid / count fall through to UNKNOWN.
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::Invalid)
				.unwrap(),
			0
		);
		assert_eq!(
			u.get_oiio_base_type_from_format(PixelFormat::Count)
				.unwrap(),
			0
		);
	}

	#[test]
	fn format_from_oiio_basetype_mapping() {
		let u = utils();
		assert_eq!(u.get_format_from_oiio_basetype(2).unwrap(), PixelFormat::U8);
		assert_eq!(
			u.get_format_from_oiio_basetype(4).unwrap(),
			PixelFormat::U16
		);
		assert_eq!(
			u.get_format_from_oiio_basetype(10).unwrap(),
			PixelFormat::F16
		);
		assert_eq!(
			u.get_format_from_oiio_basetype(11).unwrap(),
			PixelFormat::F32
		);
		// Unknown / unmappable base types map to Invalid.
		assert_eq!(
			u.get_format_from_oiio_basetype(0).unwrap(),
			PixelFormat::Invalid
		); // UNKNOWN
		assert_eq!(
			u.get_format_from_oiio_basetype(1).unwrap(),
			PixelFormat::Invalid
		); // NONE
		assert_eq!(
			u.get_format_from_oiio_basetype(3).unwrap(),
			PixelFormat::Invalid
		); // INT8
		assert_eq!(
			u.get_format_from_oiio_basetype(12).unwrap(),
			PixelFormat::Invalid
		); // DOUBLE
		assert_eq!(
			u.get_format_from_oiio_basetype(15).unwrap(),
			PixelFormat::Invalid
		); // USTRINGHASH
		assert_eq!(
			u.get_format_from_oiio_basetype(99).unwrap(),
			PixelFormat::Invalid
		);
	}

	#[test]
	fn format_from_oiio_basetype_negative_is_error() {
		let u = utils();
		assert!(u.get_format_from_oiio_basetype(-1).is_err());
	}

	#[test]
	fn pixel_aspect_ratio_common_values() {
		let u = utils();
		// 1:1
		assert_eq!(u.get_pixel_aspect_ratio(1.0).unwrap(), (1, 1));
		// 16:9
		assert_eq!(u.get_pixel_aspect_ratio(16.0 / 9.0).unwrap(), (16, 9));
		// 4:3
		assert_eq!(u.get_pixel_aspect_ratio(4.0 / 3.0).unwrap(), (4, 3));
		// 2:1
		assert_eq!(u.get_pixel_aspect_ratio(2.0).unwrap(), (2, 1));
		// 1:2
		assert_eq!(u.get_pixel_aspect_ratio(0.5).unwrap(), (1, 2));
		// 1:1.5 = 2:3
		assert_eq!(u.get_pixel_aspect_ratio(2.0 / 3.0).unwrap(), (2, 3));
	}

	#[test]
	fn pixel_aspect_ratio_zero() {
		let u = utils();
		assert_eq!(u.get_pixel_aspect_ratio(0.0).unwrap(), (0, 1));
	}

	#[test]
	fn pixel_aspect_ratio_nan_returns_nan_rational() {
		let u = utils();
		assert_eq!(u.get_pixel_aspect_ratio(f64::NAN).unwrap(), (0, 0));
	}

	#[test]
	fn pixel_aspect_ratio_out_of_range_returns_nan_rational() {
		let u = utils();
		let too_big = i32::MAX as f64 + 4.0;
		assert_eq!(u.get_pixel_aspect_ratio(too_big).unwrap(), (0, 0));
		assert_eq!(u.get_pixel_aspect_ratio(-too_big).unwrap(), (0, 0));
	}

	#[test]
	fn pixel_aspect_ratio_reduction() {
		let u = utils();
		// 4.0 / 6.0 reduces to 2:3.
		assert_eq!(u.get_pixel_aspect_ratio(4.0 / 6.0).unwrap(), (2, 3));
		// 0.25 = 1:4.
		assert_eq!(u.get_pixel_aspect_ratio(0.25).unwrap(), (1, 4));
		// 1.5 = 3:2.
		assert_eq!(u.get_pixel_aspect_ratio(1.5).unwrap(), (3, 2));
	}

	#[test]
	fn pixel_aspect_ratio_round_trip() {
		let u = utils();
		// The recovered rational should reproduce the input within the
		// precision of a reduced fraction.
		for input in [
			0.5,
			1.0,
			1.333_333_333_333_333_3,
			1.777_777_777_777_777_7,
			2.0,
			2.35,
		] {
			let (n, d) = u.get_pixel_aspect_ratio(input).unwrap();
			if d == 0 {
				continue;
			}
			let recovered = n as f64 / d as f64;
			let err = (recovered - input).abs();
			assert!(
				err < 1e-9,
				"input={input} recovered={recovered} ({n}/{d}) err={err}"
			);
		}
	}

	#[test]
	fn oiioutils_new_is_stateless() {
		// The object is a stateless unit; construction must succeed and be
		// cheap to repeat.
		let _a = OIIOUtils::new();
		let _b = OIIOUtils::new();
	}

	#[test]
	fn image_color_type_maps_formats() {
		assert_eq!(
			image_color_type_for(PixelFormat::U8),
			Some(ExtendedColorType::L8)
		);
		assert_eq!(
			image_color_type_for(PixelFormat::U16),
			Some(ExtendedColorType::L16)
		);
		assert_eq!(
			image_color_type_for(PixelFormat::F32),
			Some(ExtendedColorType::Rgb32F)
		);
		// No image representation.
		assert_eq!(image_color_type_for(PixelFormat::U10), None);
		assert_eq!(image_color_type_for(PixelFormat::F16), None);
		assert_eq!(image_color_type_for(PixelFormat::Invalid), None);
		assert_eq!(image_color_type_for(PixelFormat::Count), None);
	}

	#[test]
	fn bits_per_channel_matches_crate_tables() {
		// Derived from the image crate's own bits-per-pixel / channel tables.
		assert_eq!(bits_per_channel(ExtendedColorType::L8), 8);
		assert_eq!(bits_per_channel(ExtendedColorType::L16), 16);
		assert_eq!(bits_per_channel(ExtendedColorType::Rgb32F), 32);
		assert_eq!(bits_per_channel(ExtendedColorType::Rgba32F), 32);
		// A channel-less type yields 0 (the upstream fallback to UNKNOWN).
		assert_eq!(bits_per_channel(ExtendedColorType::Unknown(0)), 0);
	}

	#[test]
	fn pixel_aspect_ratio_tiny_value_stays_representable() {
		// Very small magnitudes must not panic or produce a NaN rational
		// (av_d2q rescales internally).
		let u = utils();
		let (n, d) = u.get_pixel_aspect_ratio(1e-9).unwrap();
		assert_ne!(d, 0);
		let recovered = n as f64 / d as f64;
		assert!((recovered - 1e-9).abs() / 1e-9 < 1e-6, "got {recovered}");
	}

	fn temp_tiff_path(name: &str) -> (std::path::PathBuf, String) {
		let dir = std::env::temp_dir().join("oak_core-oiioutils");
		std::fs::create_dir_all(&dir).unwrap();
		let path = dir.join(name);
		let path_str = path.to_str().unwrap().to_string();
		(path, path_str)
	}

	#[test]
	fn image_f32_write_read_round_trip() {
		// 2x2 RGBA float image through a temp TIFF.
		let w = 2;
		let h = 2;
		let c = 4;
		let pixels: Vec<f32> = vec![
			0.0, 0.25, 0.5, 1.0, 0.75, 0.5, 0.25, 1.0, 1.0, 0.0, 0.5, 0.0, 0.125, 0.625, 0.875, 1.0,
		];

		let (path, path_str) = temp_tiff_path("roundtrip.tif");
		write_image_f32(&path_str, w, h, c, &pixels).expect("write should succeed");

		let img = read_image_f32(&path_str).expect("read should succeed");
		assert_eq!(img.width, w);
		assert_eq!(img.height, h);
		assert_eq!(img.channels, c);
		assert_eq!(img.pixels.len(), (w * h * c) as usize);
		assert_eq!(img.len(), img.pixels.len());
		assert!(!img.is_empty());
		assert_eq!(img.as_slice(), img.pixels.as_slice());

		for (i, (a, b)) in img.pixels.iter().zip(pixels.iter()).enumerate() {
			let diff = (a - b).abs();
			assert!(diff < 1e-6, "pixel {i}: wrote {b}, read back {a}");
		}

		std::fs::remove_file(&path).ok();
	}

	#[test]
	fn image_f32_write_rgb_round_trip() {
		// RGB (3-channel) writes are supported too; the read reports the
		// channel count from the file.
		let pixels: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
		let (path, path_str) = temp_tiff_path("roundtrip_rgb.tif");
		write_image_f32(&path_str, 2, 1, 3, &pixels).expect("write should succeed");
		let img = read_image_f32(&path_str).expect("read should succeed");
		assert_eq!((img.width, img.height, img.channels), (2, 1, 3));
		for (a, b) in img.pixels.iter().zip(pixels.iter()) {
			assert!((a - b).abs() < 1e-6, "wrote {b}, read back {a}");
		}
		std::fs::remove_file(&path).ok();
	}

	#[test]
	fn image_f32_write_rejects_invalid_dimensions() {
		let err = write_image_f32("/unused.tif", 0, 1, 4, &[]).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
		let err = write_image_f32("/unused.tif", 1, -2, 4, &[]).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
	}

	#[test]
	fn image_f32_write_rejects_unsupported_channels() {
		let pixels = vec![0.0f32; 2];
		let err = write_image_f32("/unused.tif", 1, 1, 1, &pixels).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
		let err = write_image_f32("/unused.tif", 1, 1, 2, &pixels).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
	}

	#[test]
	fn image_f32_write_rejects_length_mismatch() {
		let err = write_image_f32("/unused.tif", 2, 2, 4, &[0.0f32; 3]).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
	}

	#[test]
	fn image_f32_write_rejects_non_tiff_extension() {
		let pixels = vec![0.0f32; 4];
		let err = write_image_f32("/unused.png", 1, 1, 4, &pixels).unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
	}

	#[test]
	fn image_f32_read_missing_file_errors() {
		let err = read_image_f32("/nonexistent/oak_core-oiioutils.tif").unwrap_err();
		assert!(matches!(err, Error::Failed(_)));
	}
}
