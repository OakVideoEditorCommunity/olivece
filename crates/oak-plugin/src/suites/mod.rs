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

//! suite 层：插件调进来的 C 函数表（unsafe trampoline 集中区）。
//!
//! 每张 suite 是一个 `#[repr(C)]` 的函数表静态实例；插件经
//! [`fetch_suite`] 取得。所有入口的安全模式相同：
//!
//! 1. 解 handle（`RefBox`/`PropertySet` 指针），空指针 →
//!    kOfxStatErrBadHandle；
//! 2. 调 safe Rust 实现；
//! 3. `catch_unwind` 兜底 → kOfxStatFailed。
//!
//! 参照：HS: ofxhImageEffect.cpp:2776（fetchSuite 分发表与版本协商）。

pub mod draw;
pub mod gl_render;
pub mod image_effect;
pub mod interact;
pub mod memory;
pub mod message;
pub mod multithread;
pub mod param;
pub mod parametric;
pub mod progress;
pub mod property;
pub mod timeline;

/// OFX 状态码（全表与 SDK ofxCore.h:895-953 逐字一致；插件对状态码
/// 做精确比较，任何偏差都是静默兼容性破坏）。
pub mod status {
	/// kOfxStatOK：成功。
	pub const OK: i32 = 0;
	/// kOfxStatFailed：通用失败。
	pub const FAILED: i32 = 1;
	/// kOfxStatErrFatal：致命错误。
	pub const ERR_FATAL: i32 = 2;
	/// kOfxStatErrUnknown：未知对象/属性。
	pub const ERR_UNKNOWN: i32 = 3;
	/// kOfxStatErrMissingHostFeature：宿主缺功能。
	pub const ERR_MISSING_HOST_FEATURE: i32 = 4;
	/// kOfxStatErrUnsupported：不支持的操作。
	pub const ERR_UNSUPPORTED: i32 = 5;
	/// kOfxStatErrExists：对象已存在。
	pub const ERR_EXISTS: i32 = 6;
	/// kOfxStatErrFormat：格式错误。
	pub const ERR_FORMAT: i32 = 7;
	/// kOfxStatErrMemory：内存不足。
	pub const ERR_MEMORY: i32 = 8;
	/// kOfxStatErrBadHandle：非法 handle（含 NULL）。
	pub const ERR_BAD_HANDLE: i32 = 9;
	/// kOfxStatErrBadIndex：索引越界。
	pub const ERR_BAD_INDEX: i32 = 10;
	/// kOfxStatErrValue：非法值。
	pub const ERR_VALUE: i32 = 11;
	/// kOfxStatReplyYes。
	pub const REPLY_YES: i32 = 12;
	/// kOfxStatReplyNo。
	pub const REPLY_NO: i32 = 13;
	/// kOfxStatReplyDefault。
	pub const REPLY_DEFAULT: i32 = 14;
}

/// 句柄约定：发给插件的每个 OFX 句柄 = 对象 `props` 字段地址 | 低
/// 3 位标签。
///
/// 前置条件（本 crate 内部纪律，宿主创建对象时遵守）：
/// - 对象 `#[repr(C)]` 且 `props: PropertySet` 在**偏移 0**——句柄的
///   地址就是属性集地址，与 C++ "基类在偏移 0"（HS 里
///   OfxPropertySetHandle 即 Property::Set*）同构，属性 suite 无需
///   反查即可直接解引用；
/// - 对象堆上稳定：`Arc<RefBox<T>>` / 结构体内的 `Box<T>` 元素
///   （Vec 重分配只移动 Box 指针，不移动负载）。
///
/// 标签位编码：低 3 位（含 Mutex/Arc 的对象对齐 ≥8，低 3 位恒 0）。
pub mod tag {
	/// 标签掩码（低 3 位）。
	pub const MASK: usize = 0b111;
	/// 裸属性集（宿主内部直用 / 测试直传 `&props`）。
	pub const PROPERTY_SET: usize = 0;
	/// [`crate::descriptor::EffectDescriptor`]（describe 期 effect/
	/// param-set handle）。
	pub const DESCRIPTOR: usize = 1;
	/// [`crate::instance::Instance`]（实例期 effect/param-set handle）。
	pub const INSTANCE: usize = 2;
	/// [`crate::param::ParamDef`]（describe 期 param handle）。
	pub const PARAM_DEF: usize = 3;
	/// [`crate::param::ParamInstance`]（实例期 param handle）。
	pub const PARAM_INSTANCE: usize = 4;
	/// [`crate::clip::ClipInstance`]（实例期 clip handle；describe 期
	/// [`crate::descriptor::ClipDescriptor`] 共用此标签——两类都只在
	/// 各自的阶段被使用，属性 suite 不区分）。
	pub const CLIP: usize = 5;
	/// [`crate::image::Image`]（clipGetImage 的产物）。
	pub const IMAGE: usize = 6;
	/// [`crate::suites::interact::Interact`]（interact 实例 handle；
	/// 实例期 effect/param-set 之外的独立交互对象）。
	pub const INTERACT: usize = 7;

	/// 打标签（宿主创建对象句柄用；公开供宿主/测试构造句柄）。
	pub fn make(props: *const crate::property::PropertySet, t: usize) -> *mut std::ffi::c_void {
		(props as usize | t) as *mut std::ffi::c_void
	}

	/// 剥标签，取 props 指针（标签 0 的裸指针原样返回）。
	pub fn strip(handle: *mut std::ffi::c_void) -> *const crate::property::PropertySet {
		((handle as usize) & !MASK) as *const crate::property::PropertySet
	}

	/// 读标签。
	pub fn kind(handle: *mut std::ffi::c_void) -> usize {
		(handle as usize) & MASK
	}
}

/// 渲染上下文（TLS）：render 驱动在调用插件 action 前设置，suite
/// 回调（timeline、clipGetImage 的 scale、进度）从中读取；对应 HS
/// 中实例在渲染期的状态（ofxhImageEffect.cpp Render in_args 的子集）。
#[derive(Clone, Copy)]
pub struct RenderCtx {
	/// 当前渲染时间（秒）。
	pub time: f64,
	/// 渲染比例。
	pub scale: crate::instance::RenderScale,
	/// 项目时间域（timeline getTimeBounds）。
	pub range: crate::instance::OfxRangeD,
}

thread_local! {
	static RENDER_CTX: std::cell::RefCell<Option<RenderCtx>> = const { std::cell::RefCell::new(None) };
}

/// 设置/清除渲染上下文（render 驱动调用；action 返回前必须清除；
/// 公开：render 驱动在 host 层，测试也直接注入）。
pub fn set_render_ctx(ctx: Option<RenderCtx>) {
	RENDER_CTX.with(|c| *c.borrow_mut() = ctx);
}

/// 读取渲染上下文（无上下文返回 None；无 TLS 时插件时间查询得到
/// 0 的 headless 默认）。
pub(crate) fn render_ctx() -> Option<RenderCtx> {
	RENDER_CTX.with(|c| *c.borrow())
}

thread_local! {
	/// 当前渲染的输出图像（render 驱动设置；image effect suite 的
	/// clipGetImage 对 Output clip 返回它）。
	static CURRENT_OUTPUT: std::cell::RefCell<Option<std::sync::Arc<crate::image::Image>>> =
		const { std::cell::RefCell::new(None) };
}

/// 设置/清除当前输出图像（render 驱动调用；action 返回前必须清除）。
pub(crate) fn set_current_output(image: Option<std::sync::Arc<crate::image::Image>>) {
	CURRENT_OUTPUT.with(|c| *c.borrow_mut() = image);
}

/// 读取当前输出图像。
pub(crate) fn current_output() -> Option<std::sync::Arc<crate::image::Image>> {
	CURRENT_OUTPUT.with(|c| c.borrow().clone())
}

/// GL 渲染上下文（TLS）：GL render 驱动在调用插件 action 前设置，
/// OpenGLRender suite 的 clipLoadTexture 从中取当前渲染器与输出
/// 纹理（对应 C++ 里实例渲染期的 GL 状态；ofxGPURender.h
/// "OpenGL Current Context" 一节要求宿主在 Render 期间持有 GL
/// 上下文——本设计的约定是调用方（oakrender 的 PluginJob 路径）在
/// 进入 render_job 前已把渲染器上下文置为 current，本表只传递
/// 渲染器引用与输出纹理值）。
#[derive(Clone)]
pub struct GlCtx {
	/// 当前渲染器（oakrender 后端上下文）。
	pub renderer: crate::render::Renderer,
	/// 已附着的输出纹理（渲染目标；GL 模式下插件把结果画进它）。
	pub output_texture: crate::render::Texture,
	/// 当前 GL 纹理的实际像素深度（kOfxBitDepth* 静态串；Phase 2
	/// 全链路 F32，由 render_gl 按插件 kOfxOpenGLPropPixelDepth 协商
	/// 后填入——纹理句柄的 kOfxImageEffectPropPixelDepth 以它为准）。
	pub gl_pixel_depth: &'static str,
	/// 已附着的**真实 GL 输出纹理名**（GL 模式下宿主为输出帧建的
	/// GL 纹理 + FBO 颜色附件；clipLoadTexture(Output) 的
	/// OpenGLTextureIndex 以它为准——ofxGPURender.h 要求宿主把输出
	/// 纹理绑定为当前颜色缓冲）。None = 无真实 GL 名（CPU 回退）。
	pub output_gl_texture: Option<i32>,
}

thread_local! {
	static GL_CTX: std::cell::RefCell<Option<GlCtx>> = const { std::cell::RefCell::new(None) };
}

/// 设置/清除 GL 渲染上下文（GL render 驱动调用；action 返回前必须
/// 清除；公开：测试直接注入）。
pub fn set_gl_ctx(ctx: Option<GlCtx>) {
	GL_CTX.with(|c| *c.borrow_mut() = ctx);
}

/// 读取 GL 渲染上下文（无上下文返回 None——clipLoadTexture 在非
/// GL 渲染期调用时按规范返回 kOfxStatErrMissingHostFeature）。
pub(crate) fn gl_ctx() -> Option<GlCtx> {
	GL_CTX.with(|c| c.borrow().clone())
}

/// 宿主进程身份（fetchSuite 的 version 检查用；= OFX API 1.5）。
pub(crate) const OFX_API_VERSION: i32 = 105;

/// fetchSuite 宿主入口：按名字与版本返回函数表指针；不认识或版本
/// 不符返回 `None`（FFI 层转 NULL）。
///
/// 第 1 期注册：OfxPropertySuite v1、OfxMemorySuite v1、
/// OfxImageEffectSuite v1、OfxParameterSuite v1、OfxMessageSuite
/// v1/v2、OfxProgressSuite v1/v2、OfxTimeLineSuite v1、
/// OfxMultiThreadSuite v1。
/// 第 2 期追加：OfxImageEffectOpenGLRenderSuite v1（GL 路径）；
/// ofxColour 无 suite 表（纯属性 + GetOutputColourspace action）。
/// 第 3 期追加：OfxInteractSuite v1、OfxDrawSuite v1（interact 宿主）。
/// 第 4 期追加：OfxParametricParameterSuite v1（曲线参数，
/// suites/parametric.rs）。
pub fn fetch_suite(name: &str, version: i32) -> Option<*const std::ffi::c_void> {
	let suite: *const std::ffi::c_void = match (name, version) {
		("OfxPropertySuite", 1) => ptr(property::suite_v1()),
		("OfxMemorySuite", 1) => ptr(memory::suite_v1()),
		("OfxImageEffectSuite", 1) => ptr(image_effect::suite_v1()),
		("OfxParameterSuite", 1) => ptr(param::suite_v1()),
		("OfxMessageSuite", 1) => ptr(message::suite_v1()),
		("OfxMessageSuite", 2) => ptr(message::suite_v2()),
		("OfxProgressSuite", 1) => ptr(progress::suite_v1()),
		("OfxProgressSuite", 2) => ptr(progress::suite_v2()),
		("OfxTimeLineSuite", 1) => ptr(timeline::suite_v1()),
		("OfxMultiThreadSuite", 1) => ptr(multithread::suite_v1()),
		("OfxImageEffectOpenGLRenderSuite", 1) => ptr(gl_render::suite_v1()),
		("OfxInteractSuite", 1) => ptr(interact::suite_v1()),
		("OfxDrawSuite", 1) => ptr(draw::suite_v1()),
		("OfxParametricParameterSuite", 1) => ptr(parametric::suite_v1()),
		_ => return None,
	};
	Some(suite)
}

/// 表引用 → 不透明指针（fetch_suite 的统一出口）。
fn ptr<T>(p: &'static T) -> *const std::ffi::c_void {
	p as *const T as *const std::ffi::c_void
}

#[cfg(test)]
mod tests {
	use super::*;

	/// suite 分发表：版本精确匹配、未知版本/名字 → None。
	#[test]
	fn fetch_suite_dispatch() {
		assert!(fetch_suite("OfxPropertySuite", 1).is_some());
		assert!(fetch_suite("OfxMemorySuite", 1).is_some());
		assert!(fetch_suite("OfxImageEffectSuite", 1).is_some());
		assert!(fetch_suite("OfxParameterSuite", 1).is_some());
		assert!(fetch_suite("OfxMessageSuite", 1).is_some());
		assert!(fetch_suite("OfxMessageSuite", 2).is_some());
		assert!(fetch_suite("OfxProgressSuite", 1).is_some());
		assert!(fetch_suite("OfxProgressSuite", 2).is_some());
		assert!(fetch_suite("OfxTimeLineSuite", 1).is_some());
		assert!(fetch_suite("OfxMultiThreadSuite", 1).is_some());
		assert!(fetch_suite("OfxImageEffectOpenGLRenderSuite", 1).is_some());
		assert!(fetch_suite("OfxInteractSuite", 1).is_some());
		assert!(fetch_suite("OfxDrawSuite", 1).is_some());
		assert!(fetch_suite("OfxParametricParameterSuite", 1).is_some());

		assert!(fetch_suite("OfxPropertySuite", 2).is_none());
		assert!(fetch_suite("OfxMessageSuite", 3).is_none());
		assert!(fetch_suite("OfxImageEffectOpenGLRenderSuite", 2).is_none());
		assert!(fetch_suite("OfxInteractSuite", 2).is_none());
		assert!(fetch_suite("OfxDrawSuite", 2).is_none());
		assert!(fetch_suite("OfxParametricParameterSuite", 2).is_none());
		assert!(fetch_suite("OfxBogusSuite", 1).is_none());
		assert!(fetch_suite("", 1).is_none());
	}

	/// 句柄标签：打标/剥标/读 kind 往返；对齐地址低位为 0。
	#[test]
	fn handle_tag_roundtrip() {
		let set = crate::property::PropertySet::new();
		let props = &set as *const crate::property::PropertySet;
		assert_eq!(props as usize & tag::MASK, 0, "PropertySet 必须 ≥8 对齐");
		for t in [
			tag::PROPERTY_SET,
			tag::DESCRIPTOR,
			tag::INSTANCE,
			tag::PARAM_DEF,
			tag::PARAM_INSTANCE,
			tag::CLIP,
			tag::IMAGE,
			tag::INTERACT,
		] {
			let h = tag::make(props, t);
			assert_eq!(tag::kind(h), t);
			assert_eq!(tag::strip(h) as usize, props as usize);
		}
	}

	/// 渲染上下文 TLS：设置/读取/清除。
	#[test]
	fn render_ctx_tls() {
		assert!(render_ctx().is_none());
		let ctx = RenderCtx {
			time: 1.5,
			scale: crate::instance::RenderScale { x: 2.0, y: 2.0 },
			range: crate::instance::OfxRangeD {
				min: 0.0,
				max: 100.0,
			},
		};
		set_render_ctx(Some(ctx));
		let got = render_ctx().unwrap();
		assert_eq!(got.time, 1.5);
		assert_eq!(got.scale.x, 2.0);
		assert_eq!(got.range.max, 100.0);
		set_render_ctx(None);
		assert!(render_ctx().is_none());
	}

	/// GL 上下文 TLS：设置/读取/清除（clipLoadTexture 的渲染期取值）。
	#[test]
	fn gl_ctx_tls() {
		assert!(gl_ctx().is_none());
		// 最小 GpuContextLike 假实现（无 GPU 适配器需求）。
		struct FakeGpu;
		impl oak_core::backend::GpuContextLike for FakeGpu {
			fn kind(&self) -> oak_core::backend::BackendKind {
				oak_core::backend::BackendKind::Cpu
			}
			fn destroy_texture(&self, _token: u64) {}
			fn upload(
				&self,
				_token: u64,
				_frame: &oak_core::texture::Frame,
			) -> oak_render::error::Result<()> {
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
		let renderer: crate::render::Renderer = std::sync::Arc::new(FakeGpu);
		let tex = crate::render::Texture::dummy();
		set_gl_ctx(Some(GlCtx {
			renderer,
			output_texture: tex.clone(),
			gl_pixel_depth: "OfxBitDepthFloat",
			output_gl_texture: Some(42),
		}));
		let got = gl_ctx().unwrap();
		assert_eq!(
			got.renderer.kind(),
			oak_core::backend::BackendKind::Cpu
		);
		assert!(got.output_texture.is_dummy());
		assert_eq!(got.gl_pixel_depth, "OfxBitDepthFloat");
		assert_eq!(got.output_gl_texture, Some(42));
		set_gl_ctx(None);
		assert!(gl_ctx().is_none());
	}
}
