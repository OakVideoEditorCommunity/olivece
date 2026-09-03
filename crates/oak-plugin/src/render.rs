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

//! oakrender 桥（single-lib unification）：纹理/帧值类型与渲染调用面。
//!
//! oakrender 的 C ABI 已删除（单库化）：纹理是
//! [`oak_core::texture::Texture`]（value enum，无句柄），CPU 帧是
//! [`oak_core::texture::Frame`]。本 crate 的 render 驱动与 GL suite
//! 直接持值类型：
//!
//! - [`Texture`] = [`oak_core::texture::Texture`]（值别名；
//!   clone 即引用语义，drop 自动释放后端 token——原 `texture_free`/
//!   `frame_free` 调用面随值模型删除）；
//! - [`Frame`] = [`oak_core::texture::Frame`]（值别名）；
//! - [`Renderer`] = `Arc<dyn oak_core::backend::GpuContextLike>`
//!   （渲染器即 oakrender 后端上下文，facade 经
//!   [`oak_core::backend::GpuContext::create`] 创建）；
//! - [`VideoParams`] 直接别名 oakrender 的
//!   [`oak_core::frame::VideoParamsPod`]（同布局 POD）；
//! - 像素格式常量直接别名 [`oak_core::PixelFormat`]。
//!
//! 保留桩（GPU 相关、wgpu 模型无直接 Rust 等价物）：
//! [`texture_id`]——oakrender 纹理（wgpu/CPU）没有 OpenGL 纹理名，
//! 恒 0。**GL 模式的真实 GL 纹理名来自 [`crate::gl_bridge`]**（方案 B
//! 已落地）：render 驱动为输出帧建 GL 纹理 + FBO，GL suite 的
//! `OpenGLTextureIndex` 属性据此返回真实名；use_opengl 决策改用
//! [`crate::gl_bridge::gl_available`] 作"目标纹理有有效 GL 名"的门。

/// `oakrender_video_params` POD — single-lib unification: aliases the
/// oakrender crate's struct (identical layout;
/// include/render/renderer.h:78).
pub type VideoParams = oak_core::frame::VideoParamsPod;

/// olive::PixelFormat::Format 的 f32 值。
pub const PIXEL_FORMAT_F32: i32 = oak_core::PixelFormat::F32 as i32;
/// olive::PixelFormat::Format 的 u8 值。
pub const PIXEL_FORMAT_U8: i32 = oak_core::PixelFormat::U8 as i32;

/// oakrender 渲染器（后端上下文；Arc 共享，GPU 纹理据此 upload/
/// download/blit——无需独立渲染器句柄）。
pub type Renderer = std::sync::Arc<dyn oak_core::backend::GpuContextLike>;

/// oakrender 纹理（值型；GPU 或 CPU 包装）。
pub type Texture = oak_core::texture::Texture;

/// oakrender CPU 帧（值型）。
pub type Frame = oak_core::texture::Frame;

// ---- 桥调用面（值型实现；原 CHandle 桩随单库化重写）----------------------

/// 纹理是否占位（dummy：透明黑、从未上传）。
pub fn texture_is_dummy(texture: &Texture) -> bool {
	texture.is_dummy()
}

/// 纹理的 CPU 帧：GPU 纹理经后端下载，CPU 纹理克隆
/// （原 `texture_get_frame` 的 out 参数形态改为值返回；错误即旧
/// "纹理无 CPU 帧" 失败路径）。
pub fn texture_get_frame(texture: &Texture) -> crate::error::Result<Frame> {
	texture.to_frame().map_err(|e| {
		crate::error::Error::Failed(format!("纹理 readback 失败：{e}"))
	})
}

/// 纹理的视频参数（尺寸 + 像素格式；原 `texture_get_params` 的
/// out 参数形态改为值返回）。
pub fn texture_get_params(texture: &Texture) -> VideoParams {
	let (w, h) = texture.size();
	let mut p = VideoParams::default();
	p.width = w;
	p.height = h;
	p.format = texture.format() as i32;
	p
}

/// 从视频参数与像素数据构造 CPU 纹理（原 `texture_create` 的
/// renderer+参数+像素形态改为值形态）：按 `linesize` 行跨度做行优先
/// 拷贝（0 = 紧凑行），F32/RGBA 4 通道为管线常规。GPU 上传由后端
/// 延迟（wrapped frame 语义，`Texture::wrap_frame`）。
pub fn texture_create(
	params: &VideoParams,
	pixels: &[u8],
	linesize: i32,
) -> crate::error::Result<Texture> {
	use crate::error::Error;
	let mut frame = Frame::new();
	frame.set_video_params(*params);
	let w = frame.width.max(0) as usize;
	let h = frame.height.max(0) as usize;
	if w == 0 || h == 0 {
		return Err(Error::Invalid);
	}
	let bpc = frame.bytes_per_channel();
	if bpc == 0 {
		return Err(Error::Invalid);
	}
	// 行跨度推导通道数（RGB 3 通道等非常规分量）。
	if linesize > 0 && w > 0 && (linesize as usize) % (w * bpc) == 0 {
		frame.channels = (linesize as usize / (w * bpc)) as i32;
	}
	let tight = frame.linesize_bytes();
	let row = if linesize > 0 { linesize as usize } else { tight };
	if pixels.len() < row.saturating_mul(h) {
		return Err(Error::Invalid);
	}
	frame.data = vec![0u8; tight * h];
	for y in 0..h {
		let s = y * row;
		let d = y * tight;
		frame.data[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
	}
	Ok(Texture::wrap_frame(frame))
}

/// oakrender 纹理在 GL 命名空间中的纹理名。oakrender 纹理（wgpu
/// Metal / CPU 帧）没有 OpenGL 纹理名，恒 0。
///
/// **GL 模式的真实纹理名不由本函数提供**：宿主自建的离屏 GL 纹理
/// （输出帧 + FBO 挂载、输入 clip 上传）由 [`crate::gl_bridge`] 产生，
/// 经 GL suite 的 `OpenGLTextureIndex` 属性直接写出；use_opengl 决策
/// 用 [`crate::gl_bridge::gl_available`] 当"目标纹理有有效 GL 名"的
/// 门（桥能为目标帧建出真实 GL 纹理 ⟺ 可用）。评估历史见
/// [`crate::gl_bridge`] 模块文档。
pub fn texture_id(_texture: &Texture) -> i32 {
	0
}

/// 渲染器是否为 OpenGL 后端（[`oak_core::backend::BackendKind::Gl`]；
/// 原 `renderer_is_open_gl` 的句柄形态改为后端上下文 kind 查询）。
pub fn renderer_is_open_gl(renderer: &Renderer) -> bool {
	renderer.kind() == oak_core::backend::BackendKind::Gl
}

/// A GL-kind marker context for [`RenderJob::renderer`]. The field's only
/// consumer is the use_opengl decision's kind check
/// ([`renderer_is_open_gl`]); the actual GL work (offscreen context, FBO,
/// readback) goes through [`crate::gl_bridge`], which needs no renderer
/// object. Passing this marker flips OpenGL-requiring plugins (the CImg
/// suite, which answers `kOfxStatErrMissingHostFeature` on the CPU path)
/// onto the real GL path in any process — including the headless
/// oak-worker, where gl_bridge creates its own offscreen context.
pub struct GlKindMarker;

impl oak_core::backend::GpuContextLike for GlKindMarker {
	fn kind(&self) -> oak_core::backend::BackendKind {
		oak_core::backend::BackendKind::Gl
	}
	fn destroy_texture(&self, _token: u64) {}
	fn upload(&self, _token: u64, _frame: &oak_core::texture::Frame) -> oak_render::error::Result<()> {
		Ok(())
	}
	fn download(&self, _token: u64) -> oak_render::error::Result<oak_core::texture::Frame> {
		Ok(oak_core::texture::Frame::new())
	}
	fn blit(
        &self,
        _src: u64,
        _dst: u64,
        _processor: Option<&oak_core::color::ColorProcessor>,
	) -> oak_render::error::Result<()> {
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// 测试渲染器：最小 GpuContextLike 假实现（无 GPU 适配器需求）。
	struct FakeGpu;
	impl oak_core::backend::GpuContextLike for FakeGpu {
		fn kind(&self) -> oak_core::backend::BackendKind {
			oak_core::backend::BackendKind::Cpu
		}
		fn destroy_texture(&self, _token: u64) {}
		fn upload(&self, _token: u64, _frame: &Frame) -> oak_render::error::Result<()> {
			Ok(())
		}
		fn download(&self, _token: u64) -> oak_render::error::Result<Frame> {
			Ok(Frame::new())
		}
		fn blit(
            &self,
            _src: u64,
            _dst: u64,
            _processor: Option<&oak_core::color::ColorProcessor>,
		) -> oak_render::error::Result<()> {
			Ok(())
		}
	}

	/// 纹理创建：参数 + 行跨度感知拷贝往返（含 RGB 3 通道推导）。
	#[test]
	fn texture_create_linesize_aware() {
		let mut params = VideoParams::default();
		params.width = 3;
		params.height = 2;
		params.format = PIXEL_FORMAT_F32;
		// 紧凑 3×2 RGBA F32：3*4*4 = 48 B/行。
		let tight = 3 * 4 * 4;
		let mut pixels = vec![0u8; tight * 2];
		for (i, b) in pixels.iter_mut().enumerate() {
			*b = (i % 251) as u8;
		}
		let tex = texture_create(&params, &pixels, 0).expect("紧凑行应可创建");
		assert!(!texture_is_dummy(&tex));
		let frame = texture_get_frame(&tex).expect("CPU 帧应可读");
		assert_eq!(frame.width, 3);
		assert_eq!(frame.height, 2);
		assert_eq!(frame.channels, 4);
		assert_eq!(frame.linesize_bytes(), tight);
		assert_eq!(frame.data, pixels);
		// 行跨度变体（每行 4 字节填充）。
		let row = tight + 4;
		let mut padded = vec![0u8; row * 2];
		for y in 0..2 {
			padded[y * row..y * row + tight].copy_from_slice(&pixels[y * tight..(y + 1) * tight]);
		}
		let tex = texture_create(&params, &padded, row as i32).expect("带填充行应可创建");
		let frame = texture_get_frame(&tex).unwrap();
		assert_eq!(frame.data, pixels, "填充行应剥离为紧凑存储");
		// 像素不足 → Invalid。
		assert!(texture_create(&params, &padded[..row * 2 - 1], row as i32).is_err());
		// 无效尺寸 → Invalid。
		let mut bad = params;
		bad.width = 0;
		assert!(texture_create(&bad, &pixels, 0).is_err());
	}

	/// 纹理参数查询与 dummy 语义。
	#[test]
	fn texture_params_and_dummy() {
		let dummy = Texture::dummy();
		assert!(texture_is_dummy(&dummy));
		let p = texture_get_params(&dummy);
		assert_eq!((p.width, p.height), (0, 0));

		let mut params = VideoParams::default();
		params.width = 8;
		params.height = 4;
		params.format = PIXEL_FORMAT_F32;
		let tex = texture_create(&params, &[0u8; 8 * 4 * 4 * 4], 0).unwrap();
		let p = texture_get_params(&tex);
		assert_eq!((p.width, p.height), (8, 4));
		assert_eq!(p.format, PIXEL_FORMAT_F32);
	}

	/// 渲染器 kind 查询与 GL 判断；texture_id 桩恒 0（无 GL 命名空间）。
	#[test]
	fn renderer_kind_and_gl_id_stub() {
		let r: Renderer = std::sync::Arc::new(FakeGpu);
		assert!(!renderer_is_open_gl(&r));
		let dummy = Texture::dummy();
		assert_eq!(texture_id(&dummy), 0, "wgpu 无 GL 纹理名：桩恒 0");
	}
}
