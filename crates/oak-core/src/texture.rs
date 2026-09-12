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

//! Textures and CPU frames.

use crate::Rational;
use crate::PixelFormat;
use std::sync::Arc;

use crate::backend::{BackendKind, GpuContextLike};
use crate::error::Result;
use crate::frame::VideoParamsPod;

/// A CPU frame (the payload oakcodec frames bridge into, and the value
/// `OakCodecFrame` handles box).
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
	/// Width of the pixel buffer (effective resolution).
	pub width: i32,
	/// Height of the pixel buffer (effective resolution).
	pub height: i32,
	/// Pixel format (F32 on the main pipeline).
	pub format: PixelFormat,
	/// Channel count (4 on the main pipeline).
	pub channels: i32,
	/// Timestamp in the sequence timebase.
	pub timestamp: Rational,
	/// Pixel payload (row-major, tightly packed).
	pub data: Vec<u8>,
	/// Full video metadata (divider/aspect/interlacing etc.).
	pub params: VideoParamsPod,
}

impl Default for Frame {
	fn default() -> Self {
		Self {
			width: 0,
			height: 0,
			format: PixelFormat::Invalid,
			channels: 0,
			timestamp: Rational::NULL,
			data: Vec::new(),
			params: VideoParamsPod::default(),
		}
	}
}

impl Frame {
	/// An empty frame (C++ `Frame::create()` before allocation).
	pub fn new() -> Self {
		Self::default()
	}

	/// The dummy frame: 0×0, transparent black, never uploaded
	/// (`Texture::dummy` semantics).
	pub fn dummy() -> Self {
		Self {
			width: 0,
			height: 0,
			format: PixelFormat::F32,
			channels: VideoParamsPod::INTERNAL_CHANNEL_COUNT,
			timestamp: Rational::new(0, 1),
			data: Vec::new(),
			params: VideoParamsPod::default(),
		}
	}

	/// Bytes per channel for the frame's format.
	pub fn bytes_per_channel(&self) -> usize {
		self.format.bytes_per_channel()
	}

	/// Line stride in bytes (tightly packed rows: `width * channels * bpc`).
	pub fn linesize_bytes(&self) -> usize {
		(self.width as usize)
			.saturating_mul(self.channels as usize)
			.saturating_mul(self.bytes_per_channel())
	}

	/// Total pixel payload size.
	pub fn allocated_size(&self) -> usize {
		(self.height as usize).saturating_mul(self.linesize_bytes())
	}

	/// True when the pixel buffer is allocated.
	pub fn is_allocated(&self) -> bool {
		!self.data.is_empty()
	}

	/// Set the frame's video metadata (dims, format, divider, aspect…).
	/// The channel count stays at the pipeline constant (4).
	pub fn set_video_params(&mut self, pod: VideoParamsPod) {
		self.params = pod;
		self.width = pod.effective_width();
		self.height = pod.effective_height();
		self.format = match pod.format {
			f if f == PixelFormat::U8 as i32 => PixelFormat::U8,
			f if f == PixelFormat::U10 as i32 => PixelFormat::U10,
			f if f == PixelFormat::U16 as i32 => PixelFormat::U16,
			f if f == PixelFormat::F16 as i32 => PixelFormat::F16,
			f if f == PixelFormat::F32 as i32 => PixelFormat::F32,
			_ => PixelFormat::Invalid,
		};
		self.channels = VideoParamsPod::INTERNAL_CHANNEL_COUNT;
	}

	/// The frame's video metadata as the public POD.
	pub fn video_params(&self) -> VideoParamsPod {
		let mut p = self.params;
		p.width = self.width;
		p.height = self.height;
		p.format = self.format as i32;
		p
	}

	/// Allocate (or re-allocate) the pixel buffer per the current metadata,
	/// zeroed. Returns false when the metadata is invalid
	/// (C++ `Frame::allocate`).
	pub fn allocate(&mut self) -> bool {
		if self.width <= 0 || self.height <= 0 || self.channels <= 0 {
			return false;
		}
		let size = self.allocated_size();
		if size == 0 {
			return false;
		}
		if self.data.len() != size {
			self.data = vec![0u8; size];
		} else {
			self.data.fill(0);
		}
		true
	}

	/// Borrowed pixel data pointer (empty when not allocated).
	pub fn data(&self) -> *const u8 {
		self.data.as_ptr()
	}

	/// Mutable pixel data pointer (empty when not allocated).
	pub fn data_mut(&mut self) -> *mut u8 {
		self.data.as_mut_ptr()
	}

	/// True for the dummy frame.
	pub fn is_dummy(&self) -> bool {
		self.width == 0 && self.height == 0 && self.data.is_empty()
	}

	/// True when the pixel format is a float type.
	pub fn is_float(&self) -> bool {
		matches!(self.format, PixelFormat::F16 | PixelFormat::F32)
	}

	/// Number of pixels.
	pub fn pixel_count(&self) -> usize {
		(self.width as usize).saturating_mul(self.height as usize)
	}
}

/// Shared ownership of a registry texture (M2). A `Texture::Gpu` value can
/// be cloned freely — the table handles, the graph compositor and the
/// present path all do — and the registry entry is destroyed exactly once,
/// when the last lease drops. Without it, dropping any one clone would
/// pull the texture out from under the others.
pub struct GpuLease {
	token: u64,
	ctx: Arc<dyn GpuContextLike>,
}

impl GpuLease {
	/// A lease for `token` on `ctx`.
	pub fn new(ctx: Arc<dyn GpuContextLike>, token: u64) -> Arc<Self> {
		Arc::new(Self { token, ctx })
	}

	/// The leased token.
	pub fn token(&self) -> u64 {
		self.token
	}
}

impl Drop for GpuLease {
	fn drop(&mut self) {
		self.ctx.destroy_texture(self.token);
	}
}

/// A texture: either backend-resident (GPU) or a CPU-frame wrapper.
///
/// GPU textures carry an `Arc` to their [`GpuContext`](crate::backend::GpuContext)
/// (the C++ `TexturePtr` keeps its renderer alive the same way), so a
/// texture value can upload/download/blit without a separate renderer
/// handle, plus a [`GpuLease`] that releases the backend token when the
/// last clone goes away.
#[derive(Clone)]
pub enum Texture {
	/// Backend GPU texture.
	Gpu {
		/// Backend token (wgpu texture registry key).
		token: u64,
		/// Owning backend.
		backend: BackendKind,
		/// Width.
		width: i32,
		/// Height.
		height: i32,
		/// Pixel format.
		format: PixelFormat,
		/// The context owning the texture (trait object so tests can fake
		/// the GPU side; `GpuContext` is the only production implementor).
		ctx: Arc<dyn GpuContextLike>,
		/// Shared token ownership (destroyed with the last clone).
		lease: Arc<GpuLease>,
	},
	/// CPU-frame wrapper (uploaded lazily by the backend).
	Cpu(Frame),
}

impl std::fmt::Debug for Texture {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Texture::Gpu {
				token,
				backend,
				width,
				height,
				format,
				..
			} => f
				.debug_struct("Texture::Gpu")
				.field("token", token)
				.field("backend", backend)
				.field("width", width)
				.field("height", height)
				.field("format", format)
				.finish(),
			Texture::Cpu(frame) => f.debug_tuple("Texture::Cpu").field(frame).finish(),
		}
	}
}

impl Texture {
	/// Wrap a registry texture (M2): builds the shared [`GpuLease`] so
	/// clones release the token exactly once.
	pub fn gpu(
		ctx: Arc<dyn GpuContextLike>,
		token: u64,
		width: i32,
		height: i32,
		format: PixelFormat,
	) -> Self {
		let lease = GpuLease::new(ctx.clone(), token);
		Texture::Gpu {
			token,
			backend: ctx.kind(),
			width,
			height,
			format,
			ctx,
			lease,
		}
	}

	/// A dummy/empty texture (C++ `Texture::dummy` semantics): reads as
	/// transparent black, never uploaded.
	pub fn dummy() -> Self {
		Texture::Cpu(Frame::dummy())
	}

	/// True for dummy textures.
	pub fn is_dummy(&self) -> bool {
		match self {
			Texture::Gpu { .. } => false,
			Texture::Cpu(f) => f.is_dummy(),
		}
	}

	/// Wrap a CPU frame (no copy).
	pub fn wrap_frame(frame: Frame) -> Self {
		Texture::Cpu(frame)
	}

	/// Read back into a CPU frame (downloads for GPU textures).
	pub fn to_frame(&self) -> Result<Frame> {
		match self {
			Texture::Cpu(f) => Ok(f.clone()),
			Texture::Gpu { token, ctx, .. } => ctx.download(*token),
		}
	}

	/// Dimensions (0x0 for dummy).
	pub fn size(&self) -> (i32, i32) {
		match self {
			Texture::Gpu { width, height, .. } => (*width, *height),
			Texture::Cpu(f) => (f.width, f.height),
		}
	}

	/// The texture's pixel format.
	pub fn format(&self) -> PixelFormat {
		match self {
			Texture::Gpu { format, .. } => *format,
			Texture::Cpu(f) => f.format,
		}
	}

	/// The backend kind hosting the texture.
	pub fn backend(&self) -> BackendKind {
		match self {
			Texture::Gpu { backend, .. } => *backend,
			Texture::Cpu(_) => BackendKind::Cpu,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn frame_allocate_and_linesize() {
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 4;
		p.height = 3;
		f.set_video_params(p);
		assert_eq!(f.width, 4);
		assert_eq!(f.height, 3);
		assert_eq!(f.channels, 4);
		assert_eq!(f.linesize_bytes(), 4 * 4 * 4);
		assert!(f.allocate());
		assert_eq!(f.data.len(), 4 * 3 * 4 * 4);
		assert!(f.data.iter().all(|&b| b == 0));
	}

	#[test]
	fn allocate_rejects_invalid() {
		let mut f = Frame::new();
		assert!(!f.allocate());
		f.width = 0;
		f.height = 10;
		f.channels = 4;
		f.format = PixelFormat::F32;
		assert!(!f.allocate());
	}

	#[test]
	fn dummy_frame_semantics() {
		let d = Frame::dummy();
		assert!(d.is_dummy());
		assert_eq!(d.width, 0);
		let t = Texture::dummy();
		assert!(t.is_dummy());
		assert_eq!(t.size(), (0, 0));
		assert_eq!(t.backend(), BackendKind::Cpu);
	}

	#[test]
	fn video_params_roundtrip_and_pointers() {
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 6;
		p.height = 4;
		p.divider = 2;
		p.pixel_aspect_num = 2;
		p.pixel_aspect_den = 1;
		f.set_video_params(p);
		// Divider shrinks the buffer dims (effective resolution).
		assert_eq!(f.width, 3);
		assert_eq!(f.height, 2);
		let pod = f.video_params();
		assert_eq!(pod.width, 3);
		assert_eq!(pod.pixel_aspect_num, 2);
		assert_eq!(f.is_float(), true);
		assert_eq!(f.pixel_count(), 6);
		f.allocate();
		assert!(!f.data().is_null());
		assert!(!f.data_mut().is_null());
		// timestamp default null.
		assert!(f.timestamp.is_null());
	}

	#[test]
	fn wrap_and_to_frame_roundtrip() {
		let mut f = Frame::new();
		let mut p = VideoParamsPod::default();
		p.width = 2;
		p.height = 2;
		f.set_video_params(p);
		f.allocate();
		f.data[0] = 0xAB;
		let t = Texture::wrap_frame(f.clone());
		assert_eq!(t.to_frame().unwrap(), f);
	}
}
