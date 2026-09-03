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

//! Pipeline invariant tests: F32 + ACEScg must survive end to end.
//! These are the regression alarms for "someone silently downgraded
//! the pipeline to 8-bit".
//!
//! GPU tests skip gracefully when no adapter is available.
//! FFI success paths for the display renderer / texture / frame /
//! color processor families live here too (they need a real OCIO config
//! and a renderer).

mod common;

use oak_core::{PixelFormat, Rational};

use oak_core::backend::{BackendKind, DisplayRenderer, GpuContext};
use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};

/// A frame rendered through the CPU backend is F32 RGBA (not u8) and
/// preserves out-of-[0,1] HDR values without clamping.
#[test]
fn cpu_path_stays_f32_unclamped() {
	// The ticket producer renders the pipeline frame: F32 RGBA.
	let frame =
		oak_render::eval::generate_frame(Rational::new(0, 1), (16, 16), PixelFormat::F32).unwrap();
	assert_eq!(frame.format, PixelFormat::F32);
	assert_eq!(frame.channels, 4);

	// Write out-of-range HDR values and round-trip through a CPU texture.
	let mut frame = frame;
	let n = frame.pixel_count();
	let f32s: &mut [f32] =
		unsafe { std::slice::from_raw_parts_mut(frame.data.as_mut_ptr() as *mut f32, n * 4) };
	f32s[0] = 2.5; // > 1.0 highlight
	f32s[1] = -0.5; // < 0.0 shadow
	f32s[2] = 1.0;
	f32s[3] = 0.0;

	let tex = Texture::wrap_frame(frame);
	let back = tex.to_frame().unwrap();
	let back_f32: &[f32] =
		unsafe { std::slice::from_raw_parts(back.data.as_ptr() as *const f32, n * 4) };
	assert_eq!(back_f32[0], 2.5, "no clamping of HDR values");
	assert_eq!(back_f32[1], -0.5, "no clamping of sub-black values");
	assert_eq!(back_f32[2], 1.0);
}

/// Blit with a color processor applies the OCIO transform in float
/// (CPU path; the GPU color-managed path is documented-deferred).
#[test]
fn blit_applies_ocio_in_float() {
	let _ = oak_core::color::set_up_default_config();
	let processor = oak_core::color::ColorProcessor::create(
		"ACEScg",
		"sRGB Encoded Rec.709 (sRGB)",
		oak_core::color::Direction::Normal,
	)
	.expect("handle always returned");
	if !processor.is_valid() {
		eprintln!("no valid processor for ACEScg→sRGB Encoded; skipping");
		return;
	}

	let mut renderer = DisplayRenderer::new(BackendKind::Cpu);
	let mut pod = VideoParamsPod::default();
	pod.width = 4;
	pod.height = 4;
	let mut src = renderer.create_texture(&pod, None).unwrap();
	let mut dst = renderer.create_texture(&pod, None).unwrap();

	// 18% grey (0.18) scene linear; the transform must change it and keep
	// alpha untouched (float math end to end).
	let Texture::Cpu(sf) = &mut src else {
		unreachable!()
	};
	let f32s: &mut [f32] = unsafe {
		std::slice::from_raw_parts_mut(sf.data.as_mut_ptr() as *mut f32, sf.pixel_count() * 4)
	};
	for px in f32s.chunks_exact_mut(4) {
		px[0] = 0.18;
		px[1] = 0.18;
		px[2] = 0.18;
		px[3] = 1.0;
	}

	renderer
		.blit_color_managed(Some(&src), &mut dst, Some(&processor))
		.unwrap();
	let Texture::Cpu(df) = &dst else {
		unreachable!()
	};
	let out: &[f32] =
		unsafe { std::slice::from_raw_parts(df.data.as_ptr() as *const f32, df.pixel_count() * 4) };
	assert!(
		(out[0] - 0.18).abs() > 1e-4,
		"the OCIO transform must actually change the pixel (got {})",
		out[0]
	);
	assert!((out[3] - 1.0).abs() < 1e-5, "alpha preserved");

	// Pass-through processor is a no-op copy.
	renderer
		.blit_color_managed(
			Some(&src),
			&mut dst,
			Some(&oak_core::color::ColorProcessor::pass_through()),
		)
		.unwrap();
}

/// Texture upload/download round-trip preserves F32 bit patterns
/// (CPU path; NaN-safe comparison excluded).
#[test]
fn texture_roundtrip_bit_exact_f32() {
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = 4;
	pod.height = 2;
	frame.set_video_params(pod);
	frame.allocate();
	// Distinct bit patterns per pixel.
	for i in 0..frame.data.len() / 4 {
		frame.data[i * 4] = (i % 251) as u8;
		frame.data[i * 4 + 1] = (i * 3 % 251) as u8;
		frame.data[i * 4 + 2] = (i * 7 % 251) as u8;
		frame.data[i * 4 + 3] = (i * 11 % 251) as u8;
	}
	let tex = Texture::wrap_frame(frame.clone());
	let back = tex.to_frame().unwrap();
	assert_eq!(back.data, frame.data, "bit-exact F32 round-trip");
}

/// GPU path (skipped without a GPU): same invariants through the wgpu
/// backend; tolerance 1e-4 for driver variance.
#[test]
fn gpu_path_f32_invariants() {
	let Some(ctx) = GpuContext::create(BackendKind::Auto) else {
		eprintln!("no GPU adapter; skipping gpu_path_f32_invariants");
		return;
	};
	let w = 16;
	let h = 8;
	let token = ctx.create_texture(w, h).unwrap();

	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = w;
	pod.height = h;
	frame.set_video_params(pod);
	frame.allocate();
	let f32s: &mut [f32] = unsafe {
		std::slice::from_raw_parts_mut(frame.data.as_mut_ptr() as *mut f32, frame.pixel_count() * 4)
	};
	for (i, px) in f32s.chunks_exact_mut(4).enumerate() {
		px[0] = 0.1 * i as f32;
		px[1] = -0.25;
		px[2] = 1.5; // HDR out of [0,1]
		px[3] = 1.0;
	}

	ctx.upload(token, &frame).unwrap();
	let out = ctx.download(token).unwrap();
	let out_f32: &[f32] = unsafe {
		std::slice::from_raw_parts(out.data.as_ptr() as *const f32, out.pixel_count() * 4)
	};
	for (i, px) in out_f32.chunks_exact(4).enumerate() {
		assert!(
			(px[0] - 0.1 * i as f32).abs() < 1e-4,
			"R channel pixel {i} preserved"
		);
		assert!((px[1] + 0.25).abs() < 1e-4, "negative values unclamped");
		assert!((px[2] - 1.5).abs() < 1e-4, "HDR values unclamped");
		assert!((px[3] - 1.0).abs() < 1e-4);
	}

	// Blit preserves pixels through the WGSL pipeline.
	let dst = ctx.create_texture(w, h).unwrap();
	ctx.blit(token, dst, None).unwrap();
	let blit = ctx.download(dst).unwrap();
	assert_eq!(blit.data, out.data, "plain-copy blit is pixel-exact on GPU");

	ctx.destroy_texture(token);
	ctx.destroy_texture(dst);
}
