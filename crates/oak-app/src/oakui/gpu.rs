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

//! The 10-bit display path's GPU uploads.
//!
//! The real and mock engines both produce their viewer picture as F32 RGBA
//! samples (the pipeline's internal format). Instead of downconverting to
//! BGRA8 and going through gpui's 8-bit sprite atlas, this module packs the
//! samples into a half-float RGBA16F texture and hands it to the viewer as a
//! [`SurfaceSource::Texture`](gpui::SurfaceSource) — the wgpu renderer
//! samples it straight into the (10-bit, `Rgb10a2Unorm`) swapchain, so no
//! 8-bit quantization happens between the render and the panel.
//!
//! The wgpu device/queue come from the window the app opens; the window
//! builder registers them once via [`register_context`]. Uploads are
//! best-effort: without a registered context (tests, headless runs) they
//! return `None` and the caller falls back to the BGRA8 CPU-frame path,
//! which keeps the pre-existing behavior.

use std::sync::Mutex;

/// The window's wgpu device/queue, registered by the app's window builder
/// (the same pair gpui's renderer draws with, so textures created here are
/// visible to it). `None` before the first window opens — uploads no-op.
static GPU_CONTEXT: Mutex<Option<(std::sync::Arc<wgpu::Device>, std::sync::Arc<wgpu::Queue>)>> =
	Mutex::new(None);

/// The cached CPU-baked display LUT (M2): keyed by the display/color
/// generation so a settings or monitor change rebuilds it, and
/// re-installed whenever the engine context does not have it.
static DISPLAY_LUT: Mutex<Option<(String, oak_core::lut::Lut3d)>> = Mutex::new(None);

/// A key covering every input of the display chain: the displaycolor
/// generation (policy + monitor ICC) and the project's working/output
/// color settings.
fn display_lut_key() -> String {
	format!(
		"{}|{:?}|{:?}",
		super::displaycolor::generation(),
		oak_core::color::pipeline_working_space(),
		oak_core::color::pipeline_output_spec()
	)
}

/// Build the working-space → display-device 3D LUT with the exact CPU
/// reference implementation: the output node
/// ([`oak_core::colormath::working_to_display_target`]) followed by the
/// display ICC chain ([`super::displaycolor::apply_f32_rgba`]). This is
/// what makes the GPU present path color-managed: every per-pixel step the
/// CPU path performs runs here once per settings change, on the GPU's
/// behalf, at full precision.
fn build_display_lut() -> oak_core::lut::Lut3d {
	let edge = oak_core::lut::Lut3d::DISPLAY_EDGE;
	let lo = oak_core::lut::Lut3d::DISPLAY_LO;
	let hi = oak_core::lut::Lut3d::DISPLAY_HI;
	let n = (edge as usize).pow(3);
	let mut samples = vec![0.0f32; n * 4];
	let step = |i: usize, axis: usize| -> f32 {
		let t = i as f32 / (edge - 1) as f32;
		lo[axis] + (hi[axis] - lo[axis]) * t
	};
	for b in 0..edge as usize {
		for g in 0..edge as usize {
			for r in 0..edge as usize {
				let idx = ((b * edge as usize + g) * edge as usize + r) * 4;
				samples[idx] = step(r, 0);
				samples[idx + 1] = step(g, 1);
				samples[idx + 2] = step(b, 2);
				samples[idx + 3] = 1.0;
			}
		}
	}
	oak_core::colormath::working_to_display_target(
		&mut samples,
		oak_core::color::pipeline_working_space(),
		oak_core::color::pipeline_output_spec(),
	);
	super::displaycolor::apply_f32_rgba(&mut samples, n as i64);
	let mut data = Vec::with_capacity(n * 3);
	for px in samples.chunks_exact(4) {
		data.extend_from_slice(&px[..3]);
	}
	oak_core::lut::Lut3d { edge, lo, hi, data }
}

/// Install the display LUT on the engine context when missing or stale.
fn ensure_display_lut(ctx: &oak_core::backend::GpuContext) {
	let key = display_lut_key();
	let mut cache = DISPLAY_LUT.lock().unwrap_or_else(|e| e.into_inner());
	let fresh = cache.as_ref().is_some_and(|(k, _)| *k == key);
	if fresh && ctx.has_display_lut() {
		return;
	}
	let lut = if fresh {
		cache
			.as_ref()
			.map(|(_, l)| l.clone())
			.unwrap_or_else(build_display_lut)
	} else {
		build_display_lut()
	};
	if ctx.set_display_lut(&lut).is_ok() {
		*cache = Some((key, lut));
	}
}

/// Register the window's wgpu device/queue for the 10-bit display path
/// and adopt it into the engine (M2). The engine's render thread then
/// renders on the very device gpui presents with, so finished frames are
/// sampled zero-copy via [`gpui::SurfaceSource::Texture`]. The adoption
/// is a no-op when the engine already created its own device (then
/// [`present_gpu_frame`] reports `None` and the caller stages through the
/// CPU as before).
pub fn register_context(device: std::sync::Arc<wgpu::Device>, queue: std::sync::Arc<wgpu::Queue>) {
	if let Ok(mut ctx) = GPU_CONTEXT.lock() {
		*ctx = Some((device.clone(), queue.clone()));
	}
	let adopted = oak_core::backend::GpuContext::adopt(device, queue, oak_core::backend::BackendKind::Auto);
	if !oak_core::backend::GpuContext::install_shared(Some(adopted)) {
		// The engine context was already used for GPU work before the
		// window opened: it cannot be replaced, so present falls back to
		// the single staging readback. Log once — this is the only silent
		// degradation of the M2 zero-copy path.
		static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
		if oak_core::backend::GpuContext::shared().is_some_and(|c| !c.is_adopted())
			&& !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
		{
			log::error!(
				"GPU device adoption refused: the engine created and used a device before the \
				 window opened; preview presentation falls back to a staging readback"
			);
		}
	}
}

/// Whether a GPU context is registered (a window is open). The viewer uses
/// this to decide between the 10-bit surface path and the BGRA8 fallback.
pub fn context_ready() -> bool {
	GPU_CONTEXT.lock().map(|ctx| ctx.is_some()).unwrap_or(false)
}

/// Present an engine-rendered GPU texture (M2): apply the display LUT on
/// the shared device and return the raw `wgpu::Texture` gpui samples.
/// `None` when the texture is CPU-resident, the engine device is not the
/// adopted one, or the LUT pass is unavailable — the caller then falls
/// back to the CPU display path.
pub fn present_gpu_frame(
	texture: &oak_core::texture::Texture,
) -> Option<std::sync::Arc<wgpu::Texture>> {
	let oak_core::texture::Texture::Gpu { token, ctx, .. } = texture else {
		return None;
	};
	let concrete = ctx
		.as_any()?
		.downcast_ref::<oak_core::backend::GpuContext>()?;
	if !concrete.is_adopted() {
		return None;
	}
	ensure_display_lut(concrete);
	let dst = concrete.present_texture(*token).ok()?;
	let handle = concrete.texture_handle(dst);
	// gpui's `Arc` owns the texture now; release the engine registry entry.
	concrete.destroy_texture(dst);
	handle
}

/// Upload F32 RGBA samples (tightly packed, `width * height * 4` values) as
/// a half-float RGBA16F GPU texture for the 10-bit display path. Returns
/// `None` when no context is registered or the samples are malformed — the
/// caller then falls back to the BGRA8 CPU-frame path.
pub fn upload_rgba16f(
	width: u32,
	height: u32,
	samples: &[f32],
) -> Option<std::sync::Arc<wgpu::Texture>> {
	if width == 0 || height == 0 || samples.len() != (width * height * 4) as usize {
		return None;
	}
	let (device, queue) = {
		let ctx = GPU_CONTEXT.lock().ok()?;
		ctx.as_ref().map(|(d, q)| (d.clone(), q.clone()))?
	};
	let (bytes, bytes_per_row) =
		super::frames::f32_rgba_to_16f_bytes(width, height, samples)?;
	let texture = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("oak_display_rgba16f"),
		size: wgpu::Extent3d {
			width,
			height,
			depth_or_array_layers: 1,
		},
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: wgpu::TextureFormat::Rgba16Float,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	queue.write_texture(
		wgpu::TexelCopyTextureInfo {
			texture: &texture,
			mip_level: 0,
			origin: wgpu::Origin3d::ZERO,
			aspect: wgpu::TextureAspect::All,
		},
		&bytes,
		wgpu::TexelCopyBufferLayout {
			offset: 0,
			// Multi-row copies require an explicit, 256-aligned pitch; a
			// `None` pitch fails validation and the texture stays black.
			bytes_per_row: Some(bytes_per_row),
			rows_per_image: None,
		},
		wgpu::Extent3d {
			width,
			height,
			depth_or_array_layers: 1,
		},
	);
	Some(std::sync::Arc::new(texture))
}

/// Upload F32 display samples as an RGBA16F texture and register it for
/// `image_id` (the `RenderImage` it replaces) so the viewer switches to the
/// 10-bit surface path. Best-effort: without a registered context it's a
/// no-op and the viewer keeps the BGRA8 CPU-frame fallback.
pub fn register_display_frame(image_id: usize, width: u32, height: u32, samples: &[f32]) {
	let Some(texture) = upload_rgba16f(width, height, samples) else {
		return;
	};
	register_texture(image_id, width, height, texture);
}

/// Register an already-created `wgpu::Texture` (the M2 zero-copy present
/// result) for `image_id`, so the viewer samples it instead of a CPU
/// upload. The texture must live on the registered window device.
pub fn register_texture(
	image_id: usize,
	width: u32,
	height: u32,
	texture: std::sync::Arc<wgpu::Texture>,
) {
	gpui_widgets::viewer::register_gpu_frame(
		image_id,
		texture,
		gpui::Size {
			width: gpui::DevicePixels::from(width),
			height: gpui::DevicePixels::from(height),
		},
	);
}
