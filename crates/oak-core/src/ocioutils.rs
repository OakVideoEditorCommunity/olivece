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

//! OpenColorIO utility queries, mirroring `src/common/src/ocioutils.h`
//! and `include/common/ocioutils.h`. Also the canonical home of
//! [`PixelFormat`], which other modules reuse (mirrors
//! `olive::core::PixelFormat`). The object is stateless; the handle only
//! satisfies the C API lifetime contract.
//!
//! Real OCIO access — config loading, color-space/role enumeration,
//! display/view resolution and RGBA CPU transforms — is provided by
//! [`OcioConfig`] / [`OcioProcessor`], thin wrappers over the crates.io
//! `ocio-rs` bindings. The bit-depth mapping in
//! [`OCIOUtils::get_ocio_bit_depth_from_pixel_format`] reads from the real
//! `ocio_rs::BitDepth` enum rather than hard-coded constants.

use crate::error::{Error, Result};
use ocio_rs::TransformDirection;

/// Native pixel format codes, mirroring `olive::core::PixelFormat`. Values
/// are load-bearing (they cross the C ABI as ints) and must stay in sync
/// with `olive/core/render/pixelformat.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
	/// Invalid/unknown format.
	Invalid = -1,
	/// 8-bit unsigned integer.
	U8 = 0,
	/// 10-bit unsigned integer.
	U10 = 1,
	/// 16-bit unsigned integer.
	U16 = 2,
	/// 16-bit float (half).
	F16 = 3,
	/// 32-bit float.
	F32 = 4,
	/// Sentinel, not a valid format.
	Count = 5,
}

impl PixelFormat {
	/// One of [`PixelFormat`] for an integer code (invalid/unknown codes map
	/// to `Invalid`).
	pub fn from_code(code: i32) -> PixelFormat {
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

	/// The integer code.
	pub fn code(self) -> i32 {
		self as i32
	}
}

/// The OCIO utils family (stateless).
pub struct OCIOUtils;

impl OCIOUtils {
	/// Creates the OCIOUtils object.
	pub fn new() -> Self {
		Self
	}

	/// Map a native pixel format to an OCIO bit depth code (`0` unknown,
	/// `1` uint8, `2` uint10, `3` uint12, `4` uint14, `5` uint16,
	/// `6` uint32, `7` f16, `8` f32).
	///
	/// The return type is `Result` to match the C ABI surface, but the
	/// mapping is total (never fails); invalid/out-of-range formats yield
	/// `BIT_DEPTH_UNKNOWN = 0`.
	pub fn get_ocio_bit_depth_from_pixel_format(&self, pixel_format: PixelFormat) -> Result<i32> {
		// The codes are the real `ocio_rs::BitDepth` discriminants
		// (`#[repr(i32)]`): `Uint8=1`, `Uint10=2`, `Uint16=5`, `F16=7`,
		// `F32=8`, `Unknown=0`. CPP-PARITY: matches
		// `OCIOUtils::get_ocio_bit_depth_from_pixel_format` in
		// src/common/src/ocioutils.cpp, where these are the OCIO `BitDepth`
		// constants `BIT_DEPTH_UINT8` … `BIT_DEPTH_F32`. `u10` maps to uint10
		// (not uint12) and `u16` maps to uint16 (not uint14/uint32), exactly
		// as C++.
		let depth = match pixel_format {
			PixelFormat::U8 => ocio_rs::BitDepth::Uint8,
			PixelFormat::U10 => ocio_rs::BitDepth::Uint10,
			PixelFormat::U16 => ocio_rs::BitDepth::Uint16,
			PixelFormat::F16 => ocio_rs::BitDepth::F16,
			PixelFormat::F32 => ocio_rs::BitDepth::F32,
			PixelFormat::Invalid | PixelFormat::Count => ocio_rs::BitDepth::Unknown,
		};
		Ok(depth as i32)
	}
}

/// An OCIO color-config file loaded from disk (see [`OcioConfig::from_file`]) or
/// the built-in raw config (see [`OcioConfig::raw`]).
pub struct OcioConfig {
	inner: ocio_rs::Config,
}

/// An OCIO CPU processor that transforms a single RGBA pixel.
pub struct OcioProcessor {
	inner: ocio_rs::CPUProcessor,
}

// The wrapped `ocio-rs` objects own OCIO rcptrs that are immutable for the
// lifetime of the objects; OCIO documents its config/processor objects as
// safe to share for read-only use, and `ocio-rs` keeps all mutable error
// state in thread-local storage. `ocio-rs` itself marks `Processor` and
// `CPUProcessor` `Send`; `Config` holds a raw handle, so we claim `Send` and
// `Sync` here for both wrapper types.
unsafe impl Send for OcioConfig {}
unsafe impl Sync for OcioConfig {}
unsafe impl Send for OcioProcessor {}
unsafe impl Sync for OcioProcessor {}

impl OcioConfig {
	/// Loads a config from an OCIO `.ocio` file.
	pub fn from_file(path: &str) -> Result<Self> {
		let inner = ocio_rs::Config::from_file(path)?;
		Ok(OcioConfig { inner })
	}

	/// The built-in raw (identity) config — the OCIO 2.x replacement for the
	/// removed `CreateDefault`.
	pub fn raw() -> Result<Self> {
		let inner = ocio_rs::Config::raw()?;
		Ok(OcioConfig { inner })
	}

	/// Number of color spaces registered in the config.
	pub fn colorspace_count(&self) -> Result<i32> {
		Ok(self.inner.num_color_spaces())
	}

	/// Name of the color space at `index` (0-based).
	pub fn colorspace_name(&self, index: i32) -> Result<String> {
		self.inner.color_space_name_by_index(index).ok_or_else(|| {
			Error::new(format!(
				"OcioConfig::colorspace_name: no color space at index {index}"
			))
		})
	}

	/// Names of all color spaces in config order.
	pub fn colorspaces(&self) -> Result<Vec<String>> {
		let count = self.colorspace_count()?;
		let mut out = Vec::with_capacity(count as usize);
		for i in 0..count {
			out.push(self.colorspace_name(i)?);
		}
		Ok(out)
	}

	/// Number of roles registered in the config.
	pub fn role_count(&self) -> Result<i32> {
		Ok(self.inner.num_roles())
	}

	/// Name of the role at `index` (0-based).
	pub fn role_name(&self, index: i32) -> Result<String> {
		self.inner
			.role_name(index)
			.ok_or_else(|| Error::new(format!("OcioConfig::role_name: no role at index {index}")))
	}

	/// Names of all roles in config order.
	pub fn roles(&self) -> Result<Vec<String>> {
		let count = self.role_count()?;
		let mut out = Vec::with_capacity(count as usize);
		for i in 0..count {
			out.push(self.role_name(i)?);
		}
		Ok(out)
	}

	/// Whether the config defines `role`.
	pub fn has_role(&self, role: &str) -> Result<bool> {
		Ok(self.inner.has_role(role))
	}

	/// Canonical name for a color space or role. Unknown names come back
	/// unchanged (OCIO's documented behavior).
	pub fn canonical_name(&self, name: &str) -> Result<String> {
		Ok(self
			.inner
			.canonical_name(name)
			.unwrap_or_else(|| name.to_string()))
	}

	/// The config's default display name.
	pub fn default_display(&self) -> Result<String> {
		self.inner
			.default_display()
			.ok_or_else(|| Error::new("OcioConfig::default_display: config defines no displays"))
	}

	/// The default view for `display`.
	pub fn default_view(&self, display: &str) -> Result<String> {
		self.inner.default_view(display).ok_or_else(|| {
			Error::new(format!(
				"OcioConfig::default_view: no default view for display '{display}'"
			))
		})
	}

	/// Builds a processor transforming `src` to `dst` (either may be a role
	/// name, an alias, or a color space name).
	pub fn processor(&self, src: &str, dst: &str) -> Result<OcioProcessor> {
		let processor = self.inner.processor(src, dst)?;
		Ok(OcioProcessor {
			inner: processor.default_cpu_processor()?,
		})
	}

	/// Builds a processor applying `src` through the display transform for
	/// `display`/`view` in the forward direction.
	pub fn display_processor(&self, src: &str, display: &str, view: &str) -> Result<OcioProcessor> {
		let processor =
			self.inner
				.processor_display(src, display, view, TransformDirection::Forward)?;
		Ok(OcioProcessor {
			inner: processor.default_cpu_processor()?,
		})
	}
}

impl OcioProcessor {
	/// Applies the transform to one RGBA pixel in place (0..=1 floats).
	///
	/// OCIO applies the curve to all four channels; the alpha channel is not
	/// preserved verbatim, so callers must not assume `px[3]` is unchanged.
	pub fn apply_rgba(&self, pixel: &mut [f32; 4]) -> Result<()> {
		self.inner.try_apply_rgba(pixel)?;
		Ok(())
	}
}

impl std::fmt::Debug for OcioConfig {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("OcioConfig").finish_non_exhaustive()
	}
}

impl std::fmt::Debug for OcioProcessor {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("OcioProcessor").finish_non_exhaustive()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pixel_format_from_code_maps_known_codes() {
		assert_eq!(PixelFormat::from_code(-1), PixelFormat::Invalid);
		assert_eq!(PixelFormat::from_code(0), PixelFormat::U8);
		assert_eq!(PixelFormat::from_code(1), PixelFormat::U10);
		assert_eq!(PixelFormat::from_code(2), PixelFormat::U16);
		assert_eq!(PixelFormat::from_code(3), PixelFormat::F16);
		assert_eq!(PixelFormat::from_code(4), PixelFormat::F32);
		assert_eq!(PixelFormat::from_code(5), PixelFormat::Count);
	}

	#[test]
	fn pixel_format_from_code_maps_unknown_codes_to_invalid() {
		assert_eq!(PixelFormat::from_code(-2), PixelFormat::Invalid);
		assert_eq!(PixelFormat::from_code(6), PixelFormat::Invalid);
		assert_eq!(PixelFormat::from_code(100), PixelFormat::Invalid);
		assert_eq!(PixelFormat::from_code(i32::MIN), PixelFormat::Invalid);
		assert_eq!(PixelFormat::from_code(i32::MAX), PixelFormat::Invalid);
	}

	#[test]
	fn pixel_format_code_round_trips() {
		for fmt in [
			PixelFormat::Invalid,
			PixelFormat::U8,
			PixelFormat::U10,
			PixelFormat::U16,
			PixelFormat::F16,
			PixelFormat::F32,
			PixelFormat::Count,
		] {
			assert_eq!(PixelFormat::from_code(fmt.code()), fmt);
		}
	}

	#[test]
	fn pixel_format_codes_have_load_bearing_values() {
		assert_eq!(PixelFormat::Invalid.code(), -1);
		assert_eq!(PixelFormat::U8.code(), 0);
		assert_eq!(PixelFormat::U10.code(), 1);
		assert_eq!(PixelFormat::U16.code(), 2);
		assert_eq!(PixelFormat::F16.code(), 3);
		assert_eq!(PixelFormat::F32.code(), 4);
		assert_eq!(PixelFormat::Count.code(), 5);
	}

	#[test]
	fn new_returns_a_value() {
		let utils = OCIOUtils::new();
		// Stateless; just confirm construction works and stays usable.
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::U8)
				.unwrap(),
			1
		);
	}

	#[test]
	fn ocio_bit_depth_maps_supported_formats() {
		let utils = OCIOUtils::new();
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::U8)
				.unwrap(),
			1
		);
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::U10)
				.unwrap(),
			2
		);
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::U16)
				.unwrap(),
			5
		);
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::F16)
				.unwrap(),
			7
		);
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::F32)
				.unwrap(),
			8
		);
	}

	#[test]
	fn ocio_bit_depth_maps_invalid_and_count_to_unknown() {
		let utils = OCIOUtils::new();
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::Invalid)
				.unwrap(),
			0
		);
		assert_eq!(
			utils
				.get_ocio_bit_depth_from_pixel_format(PixelFormat::Count)
				.unwrap(),
			0
		);
	}

	#[test]
	fn ocio_config_raw_round_trips() {
		// A real OCIO library call through the ocio-sys bridge that needs no
		// config file and no OCIO env var: the built-in raw (identity) config
		// always defines at least the "default" role, whose canonical color
		// space is named "raw".
		let config = OcioConfig::raw().expect("raw config should load");
		assert!(config.colorspace_count().expect("count should work") > 0);
		assert!(
			config.has_role("default").expect("has_role should work"),
			"raw config should define the default role"
		);
		let canonical = config
			.canonical_name("default")
			.expect("canonical should work");
		assert_eq!(canonical, "raw");
	}
}
