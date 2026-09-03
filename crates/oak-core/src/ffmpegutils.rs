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

//! Stateless pixel/sample format mappings between the native formats and
//! the opaque `FBPixelFormat` / `FBSampleFormat` constants of ffmpeg_bridge.
//! Mirrors `src/common/src/ffmpegutils.h` and
//! `include/common/ffmpegutils.h`. There is no handle to create or free.
//!
//! The bridge constants are plain ints copied verbatim from the ffmpeg_bridge
//! header (kept for C ABI compatibility); native formats are plain ints
//! matching `olive::core` enum values.

/// RGB channel count (flattened from `VideoParams`).
pub const RGB_CHANNEL_COUNT: i32 = 3;
/// RGBA channel count (flattened from `VideoParams`).
pub const RGBA_CHANNEL_COUNT: i32 = 4;

//
// Native `olive::core::PixelFormat::Format` values (see
// `core/include/olive/core/render/pixelformat.h`). Plain ints matching the
// C++ enum discriminants.
//
const PIX_FMT_INVALID: i32 = -1;
const PIX_FMT_U8: i32 = 0;
const PIX_FMT_U10: i32 = 1;
const PIX_FMT_U16: i32 = 2;
const PIX_FMT_F16: i32 = 3;
const PIX_FMT_F32: i32 = 4;
#[cfg(test)]
const PIX_FMT_COUNT: i32 = 5;

//
// Native `olive::core::SampleFormat::Format` values (see
// `core/include/olive/core/render/sampleformat.h`). The `_p` (planar)
// variants come first in the C++ enum.
//
const SMP_FMT_INVALID: i32 = -1;
const SMP_FMT_U8_P: i32 = 0;
const SMP_FMT_S16_P: i32 = 1;
const SMP_FMT_S32_P: i32 = 2;
const SMP_FMT_S64_P: i32 = 3;
const SMP_FMT_F32_P: i32 = 4;
const SMP_FMT_F64_P: i32 = 5;
const SMP_FMT_U8: i32 = 6;
const SMP_FMT_S16: i32 = 7;
const SMP_FMT_S32: i32 = 8;
const SMP_FMT_S64: i32 = 9;
const SMP_FMT_F32: i32 = 10;
const SMP_FMT_F64: i32 = 11;
#[cfg(test)]
const SMP_FMT_COUNT: i32 = 12;

//
// Opaque `FBPixelFormat` constants (see
// `ffmpeg_bridge/include/ffmpeg_bridge/ffmpeg_bridge.h`). Only the subset
// this module's mappings produce/consume is declared; the values are copied
// verbatim from the header.
//
const FB_PIX_FMT_NONE: i32 = -1;
const FB_PIX_FMT_YU_V420_P: i32 = 0;
const FB_PIX_FMT_RG_B24: i32 = 2;
const FB_PIX_FMT_YU_V422_P: i32 = 4;
const FB_PIX_FMT_YU_V444_P: i32 = 5;
const FB_PIX_FMT_YU_V411_P: i32 = 7;
const FB_PIX_FMT_YUV_J420_P: i32 = 12;
const FB_PIX_FMT_YUV_J422_P: i32 = 13;
const FB_PIX_FMT_YUV_J444_P: i32 = 14;
const FB_PIX_FMT_RGBA: i32 = 26;
const FB_PIX_FMT_YU_V440_P: i32 = 31;
const FB_PIX_FMT_YUV_J440_P: i32 = 32;
const FB_PIX_FMT_RG_B48_LE: i32 = 35;
const FB_PIX_FMT_RGB_A64_LE: i32 = 105;
const FB_PIX_FMT_YUV_J411_P: i32 = 138;
const FB_PIX_FMT_RGBA_F16_LE: i32 = 207;
const FB_PIX_FMT_RGB_F32_LE: i32 = 218;
const FB_PIX_FMT_RGBA_F32_LE: i32 = 220;
const FB_PIX_FMT_RGB_F16_LE: i32 = 234;

//
// Opaque `FBSampleFormat` constants (same header).
//
const FB_SAMPLE_FMT_NONE: i32 = -1;
const FB_SAMPLE_FMT_U8: i32 = 0;
const FB_SAMPLE_FMT_S16: i32 = 1;
const FB_SAMPLE_FMT_S32: i32 = 2;
const FB_SAMPLE_FMT_FLT: i32 = 3;
const FB_SAMPLE_FMT_DBL: i32 = 4;
const FB_SAMPLE_FMT_U8_P: i32 = 5;
const FB_SAMPLE_FMT_S16_P: i32 = 6;
const FB_SAMPLE_FMT_S32_P: i32 = 7;
const FB_SAMPLE_FMT_FLTP: i32 = 8;
const FB_SAMPLE_FMT_DBLP: i32 = 9;
const FB_SAMPLE_FMT_S64: i32 = 10;
const FB_SAMPLE_FMT_S64_P: i32 = 11;

/// Picks the entry of a `FB_PIX_FMT_NONE`-terminated candidate list closest
/// to `pix_fmt`: an exact match wins, otherwise the first (most desirable)
/// candidate — for this module's RGB-only candidate lists that is also what
/// the bridge's loss-metric selection (`avcodec_find_best_pix_fmt_of_list`
/// via the former `fb_find_best_pix_fmt_of_list` C ABI import) produces.
/// The logic now runs in-process; nothing links ffmpeg_bridge anymore.
fn find_best_pix_fmt_of_list(list: &[i32; 4], pix_fmt: i32) -> i32 {
	for &candidate in list {
		if candidate == pix_fmt {
			return candidate;
		}
		if candidate == FB_PIX_FMT_NONE {
			break;
		}
	}
	if list[0] == FB_PIX_FMT_NONE {
		FB_PIX_FMT_NONE
	} else {
		list[0]
	}
}

/// Builds the `FB_PIX_FMT_NONE`-terminated candidate list for
/// [`get_compatible_bridge_pixel_format`], clamped to `maximum_pix_fmt`
/// (a native pixel format, or `PIX_FMT_INVALID` for no limit).
///
/// # CPP-PARITY
/// Mirrors the `possible_pix_fmts` construction in
/// `FFmpegUtils::get_compatible_bridge_pixel_format` (the `maximum` clamp:
/// `u8` only allows RGBA; `f32` additionally allows RGBA-f32).
fn compatible_bridge_pixel_format_list(maximum_pix_fmt: i32) -> [i32; 4] {
	let mut possible = [FB_PIX_FMT_NONE; 4];
	possible[0] = FB_PIX_FMT_RGBA;
	if maximum_pix_fmt == PIX_FMT_U8 {
		possible[1] = FB_PIX_FMT_NONE;
	} else {
		possible[1] = FB_PIX_FMT_RGB_A64_LE;
		if maximum_pix_fmt == PIX_FMT_F32 {
			possible[2] = FB_PIX_FMT_RGBA_F32_LE;
			possible[3] = FB_PIX_FMT_NONE;
		} else {
			possible[2] = FB_PIX_FMT_NONE;
		}
	}
	possible
}

/// Bridge pixel format a frame can be converted to with minimal data loss,
/// clamped to a maximum native precision (`maximum_pix_fmt == -1` for no
/// limit).
pub fn get_compatible_bridge_pixel_format(pix_fmt: i32, maximum_pix_fmt: i32) -> i32 {
	find_best_pix_fmt_of_list(
		&compatible_bridge_pixel_format_list(maximum_pix_fmt),
		pix_fmt,
	)
}

/// Native pixel format usable to convert from a native frame to a bridge
/// frame with minimal data loss (`-1` if none).
pub fn get_compatible_pixel_format(pix_fmt: i32) -> i32 {
	match pix_fmt {
		PIX_FMT_U8 | PIX_FMT_U10 => PIX_FMT_U8,
		PIX_FMT_U16 | PIX_FMT_F16 | PIX_FMT_F32 => PIX_FMT_U16,
		_ => PIX_FMT_INVALID,
	}
}

/// Bridge pixel format for a given native pixel format and channel count.
pub fn get_ffmpeg_pixel_format(pix_fmt: i32, channel_count: i32) -> i32 {
	if channel_count == RGB_CHANNEL_COUNT {
		match pix_fmt {
			PIX_FMT_U8 => FB_PIX_FMT_RG_B24,
			PIX_FMT_U10 => FB_PIX_FMT_NONE,
			PIX_FMT_U16 => FB_PIX_FMT_RG_B48_LE,
			PIX_FMT_F16 => FB_PIX_FMT_RGB_F16_LE,
			PIX_FMT_F32 => FB_PIX_FMT_RGB_F32_LE,
			_ => FB_PIX_FMT_NONE,
		}
	} else if channel_count == RGBA_CHANNEL_COUNT {
		match pix_fmt {
			PIX_FMT_U8 => FB_PIX_FMT_RGBA,
			PIX_FMT_U10 => FB_PIX_FMT_NONE,
			PIX_FMT_U16 => FB_PIX_FMT_RGB_A64_LE,
			PIX_FMT_F16 => FB_PIX_FMT_RGBA_F16_LE,
			PIX_FMT_F32 => FB_PIX_FMT_RGBA_F32_LE,
			_ => FB_PIX_FMT_NONE,
		}
	} else {
		FB_PIX_FMT_NONE
	}
}

/// Native sample format for a given bridge sample format (`-1` if unknown).
pub fn get_native_sample_format(smp_fmt: i32) -> i32 {
	match smp_fmt {
		FB_SAMPLE_FMT_U8 => SMP_FMT_U8,
		FB_SAMPLE_FMT_S16 => SMP_FMT_S16,
		FB_SAMPLE_FMT_S32 => SMP_FMT_S32,
		FB_SAMPLE_FMT_S64 => SMP_FMT_S64,
		FB_SAMPLE_FMT_FLT => SMP_FMT_F32,
		FB_SAMPLE_FMT_DBL => SMP_FMT_F64,
		FB_SAMPLE_FMT_U8_P => SMP_FMT_U8_P,
		FB_SAMPLE_FMT_S16_P => SMP_FMT_S16_P,
		FB_SAMPLE_FMT_S32_P => SMP_FMT_S32_P,
		FB_SAMPLE_FMT_S64_P => SMP_FMT_S64_P,
		FB_SAMPLE_FMT_FLTP => SMP_FMT_F32_P,
		FB_SAMPLE_FMT_DBLP => SMP_FMT_F64_P,
		_ => SMP_FMT_INVALID,
	}
}

/// Bridge sample format for a given native sample format.
pub fn get_ffmpeg_sample_format(smp_fmt: i32) -> i32 {
	match smp_fmt {
		SMP_FMT_U8 => FB_SAMPLE_FMT_U8,
		SMP_FMT_S16 => FB_SAMPLE_FMT_S16,
		SMP_FMT_S32 => FB_SAMPLE_FMT_S32,
		SMP_FMT_S64 => FB_SAMPLE_FMT_S64,
		SMP_FMT_F32 => FB_SAMPLE_FMT_FLT,
		SMP_FMT_F64 => FB_SAMPLE_FMT_DBL,
		SMP_FMT_U8_P => FB_SAMPLE_FMT_U8_P,
		SMP_FMT_S16_P => FB_SAMPLE_FMT_S16_P,
		SMP_FMT_S32_P => FB_SAMPLE_FMT_S32_P,
		SMP_FMT_S64_P => FB_SAMPLE_FMT_S64_P,
		SMP_FMT_F32_P => FB_SAMPLE_FMT_FLTP,
		SMP_FMT_F64_P => FB_SAMPLE_FMT_DBLP,
		// `invalid` and `count` both fall through to "no bridge format".
		_ => FB_SAMPLE_FMT_NONE,
	}
}

/// Convert a "JPEG" full-range bridge pixel format to its regular
/// counterpart (unchanged if not JPEG).
pub fn convert_jpeg_space_to_regular_space(pix_fmt: i32) -> i32 {
	match pix_fmt {
		FB_PIX_FMT_YUV_J420_P => FB_PIX_FMT_YU_V420_P,
		FB_PIX_FMT_YUV_J422_P => FB_PIX_FMT_YU_V422_P,
		FB_PIX_FMT_YUV_J444_P => FB_PIX_FMT_YU_V444_P,
		FB_PIX_FMT_YUV_J440_P => FB_PIX_FMT_YU_V440_P,
		FB_PIX_FMT_YUV_J411_P => FB_PIX_FMT_YU_V411_P,
		// Any other format is passed through unchanged.
		other => other,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn compatible_bridge_pixel_format_no_limit_prefers_rgba_then_rgb_a64() {
		// `maximum == invalid (-1)`: candidate list is [rgba, rgb_a64_le, none].
		let list = compatible_bridge_pixel_format_list(PIX_FMT_INVALID);
		assert_eq!(list[0], FB_PIX_FMT_RGBA);
		assert_eq!(list[1], FB_PIX_FMT_RGB_A64_LE);
		assert_eq!(list[2], FB_PIX_FMT_NONE);
	}

	#[test]
	fn compatible_bridge_pixel_format_u8_clamp_allows_only_rgba() {
		// `maximum == u8`: candidate list is [rgba, none].
		let list = compatible_bridge_pixel_format_list(PIX_FMT_U8);
		assert_eq!(list[0], FB_PIX_FMT_RGBA);
		assert_eq!(list[1], FB_PIX_FMT_NONE);
	}

	#[test]
	fn compatible_bridge_pixel_format_f32_clamp_adds_rgba_f32() {
		// `maximum == f32`: candidate list is [rgba, rgb_a64_le, rgba_f32_le, none].
		let list = compatible_bridge_pixel_format_list(PIX_FMT_F32);
		assert_eq!(list[0], FB_PIX_FMT_RGBA);
		assert_eq!(list[1], FB_PIX_FMT_RGB_A64_LE);
		assert_eq!(list[2], FB_PIX_FMT_RGBA_F32_LE);
		assert_eq!(list[3], FB_PIX_FMT_NONE);
	}

	#[test]
	fn compatible_bridge_pixel_format_u16_clamp_matches_default() {
		// `maximum == u16` is neither u8 nor f32, so it matches the no-limit
		// candidate list.
		assert_eq!(
			compatible_bridge_pixel_format_list(PIX_FMT_U16),
			compatible_bridge_pixel_format_list(PIX_FMT_INVALID)
		);
	}

	#[test]
	fn compatible_bridge_pixel_format_picks_exact_candidate() {
		// With the test stub an exact match in the candidate list is preferred.
		assert_eq!(
			get_compatible_bridge_pixel_format(FB_PIX_FMT_RGBA, PIX_FMT_INVALID),
			FB_PIX_FMT_RGBA
		);
		assert_eq!(
			get_compatible_bridge_pixel_format(FB_PIX_FMT_RGB_A64_LE, PIX_FMT_F32),
			FB_PIX_FMT_RGB_A64_LE
		);
		assert_eq!(
			get_compatible_bridge_pixel_format(FB_PIX_FMT_RGBA_F32_LE, PIX_FMT_F32),
			FB_PIX_FMT_RGBA_F32_LE
		);
	}

	#[test]
	fn compatible_bridge_pixel_format_unknown_falls_back_to_first() {
		// A source format that is not among the candidates falls back to the
		// first (most desirable) candidate.
		assert_eq!(
			get_compatible_bridge_pixel_format(FB_PIX_FMT_YU_V420_P, PIX_FMT_INVALID),
			FB_PIX_FMT_RGBA
		);
	}

	#[test]
	fn compatible_pixel_format_maps_native_to_least_lossy() {
		assert_eq!(get_compatible_pixel_format(PIX_FMT_U8), PIX_FMT_U8);
		assert_eq!(get_compatible_pixel_format(PIX_FMT_U10), PIX_FMT_U8);
		assert_eq!(get_compatible_pixel_format(PIX_FMT_U16), PIX_FMT_U16);
		assert_eq!(get_compatible_pixel_format(PIX_FMT_F16), PIX_FMT_U16);
		assert_eq!(get_compatible_pixel_format(PIX_FMT_F32), PIX_FMT_U16);
	}

	#[test]
	fn compatible_pixel_format_invalid_and_count_map_to_invalid() {
		assert_eq!(
			get_compatible_pixel_format(PIX_FMT_INVALID),
			PIX_FMT_INVALID
		);
		assert_eq!(get_compatible_pixel_format(PIX_FMT_COUNT), PIX_FMT_INVALID);
	}

	#[test]
	fn ffmpeg_pixel_format_rgb_channel_layout() {
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U8, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_RG_B24
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U10, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_NONE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U16, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_RG_B48_LE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_F16, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_RGB_F16_LE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_F32, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_RGB_F32_LE
		);
	}

	#[test]
	fn ffmpeg_pixel_format_rgba_channel_layout() {
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U8, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_RGBA
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U10, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_NONE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_U16, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_RGB_A64_LE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_F16, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_RGBA_F16_LE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_F32, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_RGBA_F32_LE
		);
	}

	#[test]
	fn ffmpeg_pixel_format_other_channel_layout_returns_none() {
		assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 0), FB_PIX_FMT_NONE);
		assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 2), FB_PIX_FMT_NONE);
		assert_eq!(get_ffmpeg_pixel_format(PIX_FMT_U8, 5), FB_PIX_FMT_NONE);
	}

	#[test]
	fn ffmpeg_pixel_format_invalid_and_count_return_none() {
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_INVALID, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_NONE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_COUNT, RGB_CHANNEL_COUNT),
			FB_PIX_FMT_NONE
		);
		assert_eq!(
			get_ffmpeg_pixel_format(PIX_FMT_INVALID, RGBA_CHANNEL_COUNT),
			FB_PIX_FMT_NONE
		);
	}

	#[test]
	fn native_sample_format_maps_bridge_to_native() {
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_U8), SMP_FMT_U8);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S16), SMP_FMT_S16);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S32), SMP_FMT_S32);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S64), SMP_FMT_S64);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_FLT), SMP_FMT_F32);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_DBL), SMP_FMT_F64);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_U8_P), SMP_FMT_U8_P);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S16_P), SMP_FMT_S16_P);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S32_P), SMP_FMT_S32_P);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_S64_P), SMP_FMT_S64_P);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_FLTP), SMP_FMT_F32_P);
		assert_eq!(get_native_sample_format(FB_SAMPLE_FMT_DBLP), SMP_FMT_F64_P);
	}

	#[test]
	fn native_sample_format_unknown_maps_to_invalid() {
		assert_eq!(
			get_native_sample_format(FB_SAMPLE_FMT_NONE),
			SMP_FMT_INVALID
		);
		// A bogus code that does not match any bridge sample format.
		assert_eq!(get_native_sample_format(12345), SMP_FMT_INVALID);
	}

	#[test]
	fn ffmpeg_sample_format_maps_native_to_bridge() {
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_U8), FB_SAMPLE_FMT_U8);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S16), FB_SAMPLE_FMT_S16);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S32), FB_SAMPLE_FMT_S32);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S64), FB_SAMPLE_FMT_S64);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F32), FB_SAMPLE_FMT_FLT);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F64), FB_SAMPLE_FMT_DBL);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_U8_P), FB_SAMPLE_FMT_U8_P);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S16_P), FB_SAMPLE_FMT_S16_P);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S32_P), FB_SAMPLE_FMT_S32_P);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_S64_P), FB_SAMPLE_FMT_S64_P);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F32_P), FB_SAMPLE_FMT_FLTP);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_F64_P), FB_SAMPLE_FMT_DBLP);
	}

	#[test]
	fn ffmpeg_sample_format_invalid_and_count_return_none() {
		assert_eq!(
			get_ffmpeg_sample_format(SMP_FMT_INVALID),
			FB_SAMPLE_FMT_NONE
		);
		assert_eq!(get_ffmpeg_sample_format(SMP_FMT_COUNT), FB_SAMPLE_FMT_NONE);
	}

	#[test]
	fn sample_format_mappings_are_inverse() {
		for native in [
			SMP_FMT_U8,
			SMP_FMT_S16,
			SMP_FMT_S32,
			SMP_FMT_S64,
			SMP_FMT_F32,
			SMP_FMT_F64,
			SMP_FMT_U8_P,
			SMP_FMT_S16_P,
			SMP_FMT_S32_P,
			SMP_FMT_S64_P,
			SMP_FMT_F32_P,
			SMP_FMT_F64_P,
		] {
			let bridge = get_ffmpeg_sample_format(native);
			assert_eq!(get_native_sample_format(bridge), native);
		}
	}

	#[test]
	fn jpeg_space_converts_to_regular_space() {
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J420_P),
			FB_PIX_FMT_YU_V420_P
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J422_P),
			FB_PIX_FMT_YU_V422_P
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J444_P),
			FB_PIX_FMT_YU_V444_P
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J440_P),
			FB_PIX_FMT_YU_V440_P
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YUV_J411_P),
			FB_PIX_FMT_YU_V411_P
		);
	}

	#[test]
	fn jpeg_space_leaves_non_jpeg_formats_unchanged() {
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_RGBA),
			FB_PIX_FMT_RGBA
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_NONE),
			FB_PIX_FMT_NONE
		);
		assert_eq!(
			convert_jpeg_space_to_regular_space(FB_PIX_FMT_YU_V420_P),
			FB_PIX_FMT_YU_V420_P
		);
	}
}
