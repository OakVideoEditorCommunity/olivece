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

//! M12 P0: the footage-decode integration test.
//!
//! Generates a small MPEG-2 clip of known content (programmatically, via
//! oakcodec's real encoder — M12 P0 item 4), then renders it through the
//! eval decode path (`render_footage_frame`) and asserts the pixels match
//! the known pattern. The decode bridge calls into the oakcodec crate
//! directly, so this runs the same real path the engine uses.

use oak_core::{PixelFormat, Rational};

use oak_core::texture::Texture;

mod common;

fn test_clip_path() -> std::path::PathBuf {
	std::env::temp_dir().join(format!("oakrender_footage_{}.mp4", std::process::id()))
}

#[test]
fn footage_decode_renders_known_content() {
	// This test verifies DECODE correctness (known red/blue content), not
	// the color pipeline. Pin the working space to the legacy sRGB
	// pass-through so the decoded pixels stay display-referred and the
	// assertions below hold regardless of the ACEScg default.
	oak_core::color::set_pipeline_color_settings(
		oak_core::colormath::WorkingColorSpace::SrgbLegacy,
		oak_core::colormath::OutputColorSpec::default(),
	);

	// Program-generated media: 10 frames at 10fps, 64x64, known pattern
	// (left half red, right half blue on frame 0).
	let path = test_clip_path();
	oak_codec::testmedia::write_test_clip(&path, 64, 64, 10, 10).expect("test clip generation");

	// Decode frame 0 at native size through the eval decode path.
	let tex = oak_render::eval::render_footage_frame(
		&path.to_string_lossy(),
		0,
		Rational::new(0, 1),
		(64, 64),
		PixelFormat::F32,
	)
	.expect("decode frame 0");
	let Texture::Cpu(frame) = &tex else {
		panic!("decode produced a non-CPU texture");
	};
	assert_eq!((frame.width, frame.height), (64, 64));
	assert_eq!(frame.format, PixelFormat::F32);
	assert!(
		frame.data.iter().any(|&b| b != 0),
		"decoded frame must contain non-black pixels"
	);

	// Known content: left half red (r > 0.5, g,b < 0.5), right half
	// blue (b > 0.5, r,g < 0.5). MPEG-2 is lossy: channel-dominance
	// assertions with generous margins.
	let stride = frame.linesize_bytes();
	let read = |x: usize, y: usize| -> [f32; 4] {
		let off = y * stride + x * 16;
		let mut out = [0f32; 4];
		for i in 0..4 {
			out[i] = f32::from_le_bytes(
				frame.data[off + i * 4..off + i * 4 + 4]
					.try_into()
					.unwrap(),
			);
		}
		out
	};
	for &(x, y) in &[(8usize, 32usize), (20, 10)] {
		let [r, g, b, a] = read(x, y);
		assert!(r > 0.5, "left half red at ({x},{y}): {r}");
		assert!(g < 0.4 && b < 0.4, "left half not red at ({x},{y}): {r},{g},{b}");
		assert!(a > 0.9, "opaque at ({x},{y}): {a}");
	}
	for &(x, y) in &[(56usize, 32usize), (40, 50)] {
		let [r, g, b, a] = read(x, y);
		assert!(b > 0.5, "right half blue at ({x},{y}): {b}");
		assert!(r < 0.4 && g < 0.4, "right half not blue at ({x},{y}): {r},{g},{b}");
		assert!(a > 0.9, "opaque at ({x},{y}): {a}");
	}

	// Scaling: decode to a smaller proxy size keeps the known content.
	let proxy = oak_render::eval::render_footage_frame(
		&path.to_string_lossy(),
		0,
		Rational::new(0, 1),
		(32, 32),
		PixelFormat::F32,
	)
	.expect("decode proxy");
	let Texture::Cpu(proxy_frame) = &proxy else {
		panic!("proxy decode produced a non-CPU texture");
	};
	assert_eq!((proxy_frame.width, proxy_frame.height), (32, 32));
	let pstride = proxy_frame.linesize_bytes();
	let off = 4 * pstride + 4 * 16;
	let [r, _, _, _] = [
		f32::from_le_bytes(proxy_frame.data[off..off + 4].try_into().unwrap()),
		f32::from_le_bytes(proxy_frame.data[off + 4..off + 8].try_into().unwrap()),
		f32::from_le_bytes(proxy_frame.data[off + 8..off + 12].try_into().unwrap()),
		f32::from_le_bytes(proxy_frame.data[off + 12..off + 16].try_into().unwrap()),
	];
	assert!(r > 0.5, "proxy left half stays red: {r}");

	let _ = std::fs::remove_file(&path);
}

#[test]
fn footage_decode_fails_explainably_for_missing_file() {
	let err = oak_render::eval::render_footage_frame(
		"/definitely/not/here.mp4",
		0,
		Rational::new(0, 1),
		(16, 16),
		PixelFormat::F32,
	)
	.err()
	.expect("decode of a missing file must fail");
	let _ = err.code(); // explainable error, not a panic
}
