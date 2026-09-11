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

//! OfxInteractSuiteV1 + [`Interact`] 宿主对象。
//!
//! Interact 是插件自定义 UI 的宿主侧对象（主进程 UI 事件宿主）：与
//! worker 进程里的渲染实例并存（OFX 允许同一插件多实例），经
//! [`crate::instance::Instance::new_interact`] 创建、绑定到效果实例。
//!
//! ## 生命周期
//!
//! ```text
//! new_interact   → kOfxActionNewInteract（创建；插件拒绝 → None）
//! describe       → kOfxActionDescribe（kOfxActionDescribeInteract）
//! create_instance→ kOfxActionCreateInstance（kOfxActionCreateInstanceInteract）
//! draw/pen/key/idle → kOfxInteractAction*（宿主→插件）
//! destroy        → kOfxActionDestroyInstance（kOfxActionDestroyInstanceInteract；
//!                  Instance 销毁时自动连带）
//! ```
//!
//! 所有 action 走 `Interact::call` → 插件入口（overlay interact V2/V1
//! 入口，未声明则插件 main entry；任务契约：new_interact 向插件入口
//! 发 kOfxActionNewInteract）。返回值（OfxStatus）如实透传。
//!
//! ## 语义（ofxInteract.h）
//!
//! - pen/key action 的返回：kOfxStatOK = 插件已处理该事件，宿主不应再
//!   把事件传给视图中其他交互对象；kOfxStatReplyDefault = 插件未处理，
//!   宿主可自行处置。
//! - pen 坐标：调用方给 viewport 像素；inArgs 另带 canonical 坐标
//!   （kOfxInteractPropPenPosition，= viewport 像素 / pixelScale）与
//!   pixelScale、pressure（两态笔按 ofxInteract.h 映射 0/1）。
//! - key：kOfxPropKeySym（ofxKeySyms.h 关键码）+ kOfxPropKeyString
//!   （UTF-8；无 UTF8 编码的键为空串）。
//! - draw：宿主在调用前经 [`crate::gl_bridge::acquire`] 保持 GL current
//!   整个 action（插件发原生 GL 命令或经 Draw suite，ofxInteract.h
//!   "the openGL context for this interact has been set"）；inArgs 带
//!   viewport/pixelScale/time/backgroundImage/backgroundColour/
//!   drawContext。

use std::ffi::{c_int, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::host::{EntryPoint, Plugin};
use crate::property::{PropertySet, Value};
use crate::suites::{status, tag};

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// Interact 实例。`Arc<Interact>` 管理生命周期（app 侧持句柄 +
/// [`crate::instance::Instance`] 的 interact 字段）。
///
/// `#[repr(C)]` + props 在偏移 0（句柄约定：interact handle =
/// `&props` | INTERACT 标签；property suite 剥标签后直读属性集）。
#[repr(C)]
pub struct Interact {
	/// interact 实例级属性集（偏移 0，句柄约定）。
	pub props: PropertySet,
	/// 所属插件。
	pub plugin: Arc<Plugin>,
	/// interact 的入口（overlay interact V2/V1 入口，未声明则插件
	/// main entry）。interact 的所有 action 都发到这里。
	pub entry: EntryPoint,
	/// 效果实例句柄（tagged INSTANCE；interact action inArgs 的
	/// kOfxPropEffectInstance）。不透明令牌，不解引用。
	pub effect_handle: *mut c_void,
	/// 插件经 interactSwapBuffers 请求的缓冲交换（app 侧轮询后清零）。
	pub swap_requested: AtomicBool,
	/// 插件经 interactRedraw 请求的重绘（app 侧轮询后清零）。
	pub redraw_requested: AtomicBool,
	/// destroy 是否已通知（幂等门；Instance 销毁连带触发）。
	pub destroyed: AtomicBool,
}

// SAFETY: effect_handle 是不透明令牌（同 [`crate::property::Value::Pointer`]，
// 从不在此对象上解引用）；entry 是函数指针，调用永远发生在宿主控制的
// action 分发点；其余字段（Arc/Mutex/AtomicBool/PropertySet）均线程安全。
// Interact 经 Instance.interact 字段跨线程可达，因此 Send/Sync 成立。
unsafe impl Send for Interact {}
unsafe impl Sync for Interact {}

impl Interact {
	/// 构造（宿主内部；[`crate::instance::Instance::new_interact`] 调）。
	/// `effect_handle` 为 tagged INSTANCE 句柄（不透明令牌）。
	pub(crate) fn new(
		plugin: Arc<Plugin>,
		entry: EntryPoint,
		effect_handle: *mut c_void,
	) -> Arc<Interact> {
		let i = Arc::new(Interact {
			props: PropertySet::new(),
			plugin,
			entry,
			effect_handle,
			swap_requested: AtomicBool::new(false),
			redraw_requested: AtomicBool::new(false),
			destroyed: AtomicBool::new(false),
		});
		init_interact_props(&i.props);
		i
	}

	/// interact handle（tagged INTERACT）。
	pub(crate) fn handle(&self) -> *mut c_void {
		tag::make(&self.props as *const PropertySet, tag::INTERACT)
	}

	/// 向插件入口发一个 interact action。
	///
	/// # Safety
	/// handle 与 action 匹配（Interact 生命周期由宿主控制；destroy 后
	/// 不再调用）。
	pub(crate) fn call(&self, action: &str, in_args: &PropertySet, out_args: &PropertySet) -> i32 {
		unsafe {
			self.plugin.call_entry(self.entry, action, self.handle(), in_args, out_args)
		}
	}

	/// 取 effect 实例句柄（inArgs 的 kOfxPropEffectInstance）。
	fn effect_instance_prop(&self) -> Value {
		Value::Pointer(self.effect_handle)
	}

	/// 当前 pixelScale（interact props，默认 1,1）。
	fn pixel_scale(&self) -> (f64, f64) {
		read_double2(&self.props, crate::host::PROP_INTERACT_PIXEL_SCALE)
			.unwrap_or((1.0, 1.0))
	}

	/// describe（kOfxActionDescribe == kOfxActionDescribeInteract，
	/// ofxInteract.h:171）。inArgs/outArgs 冗余为 NULL。
	pub fn describe(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_DESCRIBE, &empty, &empty)
	}

	/// createInstance（kOfxActionCreateInstance ==
	/// kOfxActionCreateInstanceInteract，ofxInteract.h:198）。
	/// draw/pen/key 的前置（ofxInteract.h 各 action 的 \pre）。
	pub fn create_instance(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_CREATE_INSTANCE, &empty, &empty)
	}

	/// draw action（ofxInteract.h:265）。宿主在调用前
	/// [`crate::gl_bridge::acquire`] 保持 GL current 整个 action；inArgs
	/// 带 kOfxInteractPropPixelScale/ViewportSize/BackgroundColour/
	/// BackgroundImage/Time/RenderScale/DrawContext + kOfxPropEffectInstance。
	///
	/// `background_image`：宿主持有的背景图像句柄（本宿主 Phase 1 无
	/// 合成背景，传 None 即空——任务契约"可空"）。
	///
	/// 返回值：插件的 OfxStatus 如实透传；GL 上下文不可用（非 macOS /
	/// 无上下文）时返回 kOfxStatErrMissingHostFeature（宿主侧失败，未
	/// 触达插件）。
	pub fn draw(
		&self,
		viewport_size: (f64, f64),
		pixel_scale: (f64, f64),
		time: f64,
		background_image: Option<*mut c_void>,
	) -> i32 {
		// 1. GL current 整个 draw action（插件发原生 GL 命令或经 Draw
		//    suite；失败即无法绘制 → 不触达插件）。
		let _guard = match crate::gl_bridge::acquire() {
			Ok(g) => g,
			Err(_) => return status::ERR_MISSING_HOST_FEATURE,
		};

		// 2. Draw suite 上下文（存活注册；draw 返回后摘除）。
		let draw_ctx = crate::suites::draw::make_context(viewport_size, pixel_scale);

		let in_args = PropertySet::new();
		in_args.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(pixel_scale.0), Value::Double(pixel_scale.1)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_VIEWPORT_SIZE,
			vec![Value::Double(viewport_size.0), Value::Double(viewport_size.1)],
		);
		in_args.set_one(
			crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
			Value::Pointer(background_image.unwrap_or(std::ptr::null_mut())),
		);
		in_args.define(
			crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
			vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
		);
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.set_one(crate::host::PROP_INTERACT_DRAW_CONTEXT, Value::Pointer(draw_ctx));
		in_args.set_one(crate::host::PROP_EFFECT_INSTANCE, self.effect_instance_prop());

		// 3. interact 实例属性同步（kOfxInteractPropPixelScale 等是实例
		//    属性集上只读的"当前状态"，ofxInteract.h:58）。
		self.props.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(pixel_scale.0), Value::Double(pixel_scale.1)],
		);
		self.props.define(
			crate::host::PROP_INTERACT_VIEWPORT_SIZE,
			vec![Value::Double(viewport_size.0), Value::Double(viewport_size.1)],
		);
		self.props.set_one(
			crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
			Value::Pointer(background_image.unwrap_or(std::ptr::null_mut())),
		);

		let out = PropertySet::new();
		let st = self.call(crate::host::ACTION_INTERACT_DRAW, &in_args, &out);
		crate::suites::draw::drop_context(draw_ctx);
		st
	}

	/// 装配 pen 类 action 的 inArgs（PenMotion/PenDown/PenUp 共用，
	/// ofxInteract.h:274-321）。
	fn pen_in_args(&self, pen_viewport: (f64, f64), pressure: f64, time: f64) -> PropertySet {
		let (ps_x, ps_y) = self.pixel_scale();
		// canonical = viewport 像素 / pixelScale（pixelScale 是
		// canonical→屏幕像素的换算比例）。
		let in_args = PropertySet::new();
		in_args.set_one(crate::host::PROP_EFFECT_INSTANCE, self.effect_instance_prop());
		in_args.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(ps_x), Value::Double(ps_y)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
			vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
		);
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args.define(
			crate::host::PROP_INTERACT_PEN_POSITION,
			vec![
				Value::Double(pen_viewport.0 / ps_x.max(1e-9)),
				Value::Double(pen_viewport.1 / ps_y.max(1e-9)),
			],
		);
		in_args.define(
			crate::host::PROP_INTERACT_PEN_VIEWPORT_POSITION,
			vec![
				Value::Int(pen_viewport.0.round() as i32),
				Value::Int(pen_viewport.1.round() as i32),
			],
		);
		in_args.set_one(crate::host::PROP_INTERACT_PEN_PRESSURE, Value::Double(pressure));
		in_args
	}

	/// pen_motion（ofxInteract.h:302）。`pen_viewport` 为视口像素坐标；
	/// `pen_down` 表示笔是否按下（两态笔按 ofxInteract.h:114 映射压力
	/// 1.0/0.0）。
	pub fn pen_motion(&self, pen_viewport: (f64, f64), pen_down: bool, time: f64) -> i32 {
		let pressure = if pen_down { 1.0 } else { 0.0 };
		let in_args = self.pen_in_args(pen_viewport, pressure, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_MOTION, &in_args, &out)
	}

	/// pen_down（ofxInteract.h:340）。压力 1.0。
	pub fn pen_down(&self, pen_viewport: (f64, f64), time: f64) -> i32 {
		let in_args = self.pen_in_args(pen_viewport, 1.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_DOWN, &in_args, &out)
	}

	/// pen_up（ofxInteract.h:376）。压力 0.0。
	pub fn pen_up(&self, pen_viewport: (f64, f64), time: f64) -> i32 {
		let in_args = self.pen_in_args(pen_viewport, 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_PEN_UP, &in_args, &out)
	}

	/// 装配 key 类 action 的 inArgs（KeyDown/KeyUp 共用，
	/// ofxInteract.h:384-442）。
	fn key_in_args(&self, key_sym: i32, key_string: &str, time: f64) -> PropertySet {
		let in_args = PropertySet::new();
		in_args.set_one(crate::host::PROP_EFFECT_INSTANCE, self.effect_instance_prop());
		in_args.set_one(crate::host::PROP_KEY_SYM, Value::Int(key_sym));
		in_args.set_one(crate::host::PROP_KEY_STRING, Value::String(cs(key_string)));
		in_args.set_one(crate::host::PROP_TIME, Value::Double(time));
		in_args.define(
			crate::host::PROP_RENDER_SCALE,
			vec![Value::Double(1.0), Value::Double(1.0)],
		);
		in_args
	}

	/// key_down（ofxInteract.h:410）。`key_sym` 为 ofxKeySyms.h 关键码；
	/// `key_string` 为 UTF-8 字符（无 UTF8 编码的键为空串）。
	pub fn key_down(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		let in_args = self.key_in_args(key_sym, key_string, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_KEY_DOWN, &in_args, &out)
	}

	/// key_up（ofxInteract.h:443）。
	pub fn key_up(&self, key_sym: i32, key_string: &str, time: f64) -> i32 {
		let in_args = self.key_in_args(key_sym, key_string, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_KEY_UP, &in_args, &out)
	}

	/// idle（宿主空闲泵；任务契约的 kOfxInteractActionIdle，属本宿主
	/// 扩展）。插件未实现时返回 kOfxStatReplyDefault。
	pub fn idle(&self) -> i32 {
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_IDLE, &empty, &empty)
	}

	/// gain_focus（ofxInteract.h:501）：pen/key 事件的前置
	/// （ofxInteract.h 各 action 的 \pre 要求 interact 已获焦点）。
	pub fn gain_focus(&self, time: f64) -> i32 {
		let in_args = self.pen_in_args((0.0, 0.0), 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_GAIN_FOCUS, &in_args, &out)
	}

	/// lose_focus（ofxInteract.h:526）。
	pub fn lose_focus(&self, time: f64) -> i32 {
		let in_args = self.pen_in_args((0.0, 0.0), 0.0, time);
		let out = PropertySet::new();
		self.call(crate::host::ACTION_INTERACT_LOSE_FOCUS, &in_args, &out)
	}

	/// destroy（kOfxActionDestroyInstance == kOfxActionDestroyInstanceInteract，
	/// ofxInteract.h:227）。幂等：只通知一次。返回状态忽略（析构不可
	/// 回滚，ofxInteract.h "what is returned is moot"）。
	pub fn destroy(&self) {
		if self
			.destroyed
			.swap(true, Ordering::Relaxed)
		{
			return;
		}
		let empty = PropertySet::new();
		self.call(crate::host::ACTION_DESTROY_INSTANCE, &empty, &empty);
	}
}

/// interact 实例属性表（ofxInteract.h 的 PropertiesInteract 子集）。
fn init_interact_props(props: &PropertySet) {
	props.define(
		crate::host::PROP_INTERACT_PIXEL_SCALE,
		vec![Value::Double(1.0), Value::Double(1.0)],
	);
	props.define(
		crate::host::PROP_INTERACT_VIEWPORT_SIZE,
		vec![Value::Double(0.0), Value::Double(0.0)],
	);
	props.set_one(
		crate::host::PROP_INTERACT_BACKGROUND_IMAGE,
		Value::Pointer(std::ptr::null_mut()),
	);
	props.define(
		crate::host::PROP_INTERACT_SUGGESTED_COLOUR,
		vec![Value::Double(1.0), Value::Double(1.0), Value::Double(1.0)],
	);
	props.define(crate::host::PROP_INTERACT_SLAVE_TO_PARAM, vec![]);
	props.define(
		crate::host::PROP_INTERACT_BACKGROUND_COLOUR,
		vec![Value::Double(0.0), Value::Double(0.0), Value::Double(0.0)],
	);
	// 帧缓冲位深：离屏 FBO 是 RGBA32F → 每分量 32 bit，含 alpha。
	props.set_one(crate::host::PROP_INTERACT_BIT_DEPTH, Value::Int(32));
	props.set_one(crate::host::PROP_INTERACT_HAS_ALPHA, Value::Int(1));
}

/// 从属性集读 Double×2（缺失/类型不符 → None）。
fn read_double2(props: &PropertySet, name: &str) -> Option<(f64, f64)> {
	let x = match props.get(name, 0)? {
		Value::Double(d) => d,
		_ => return None,
	};
	let y = match props.get(name, 1)? {
		Value::Double(d) => d,
		_ => return None,
	};
	Some((x, y))
}

// ---- OfxInteractSuiteV1（ofxInteract.h:534-544）------------------------

/// interact 句柄解析（tag INTERACT）。
fn resolve_interact(handle: *mut c_void) -> Result<&'static Interact, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		if tag::kind(handle) != tag::INTERACT {
			return Err(status::ERR_BAD_HANDLE);
		}
		// Interact 的 props 在偏移 0：剥标签后的地址即 &Interact。
		Ok(&*(tag::strip(handle) as *const Interact))
	}
}

/// 公共入口模板：panic 兜底。
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	)
}

/// interactSwapBuffers：插件请求宿主交换 GL 缓冲（宿主 UI 视图中）。
/// 本宿主无 UI 视图，把请求记入 `swap_requested`（app 侧轮询后清零）并
/// 返回 OK——宿主服务真实生效（请求被记录、可被 UI 消费）。
unsafe extern "C" fn interact_swap_buffers(handle: *mut c_void) -> c_int {
	caught(|| {
		let i = resolve_interact(handle)?;
		i.swap_requested.store(true, Ordering::Relaxed);
		Ok(())
	})
}

/// interactRedraw：插件请求宿主重绘 interact 视图。同上记入
/// `redraw_requested`（app 侧轮询）。
unsafe extern "C" fn interact_redraw(handle: *mut c_void) -> c_int {
	caught(|| {
		let i = resolve_interact(handle)?;
		i.redraw_requested.store(true, Ordering::Relaxed);
		Ok(())
	})
}

/// interactGetPropertySet：返回 interact 的属性集句柄（tagged INTERACT）。
unsafe extern "C" fn interact_get_property_set(
	handle: *mut c_void,
	property: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property.is_null() {
			return Err(status::ERR_VALUE);
		}
		let i = resolve_interact(handle)?;
		unsafe { *property = tag::make(&i.props as *const PropertySet, tag::INTERACT) };
		Ok(())
	})
}

/// 函数表布局（与 SDK `OfxInteractSuiteV1` 逐字段一致；ofxInteract.h:534）。
#[repr(C)]
pub struct InteractSuiteV1 {
	/// interactSwapBuffers。
	pub interact_swap_buffers: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// interactRedraw。
	pub interact_redraw: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// interactGetPropertySet。
	pub interact_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
}

/// 函数表实例。
pub fn suite_v1() -> &'static InteractSuiteV1 {
	static SUITE: std::sync::OnceLock<InteractSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| InteractSuiteV1 {
		interact_swap_buffers,
		interact_redraw,
		interact_get_property_set,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::{c_char, CStr};
	use std::sync::Mutex;

	/// interact 属性表：PixelScale/ViewportSize/SuggestedColour/
	/// SlaveToParam/BitDepth/HasAlpha 等按头文件预置，经 property suite
	/// 可读写。
	#[test]
	fn interact_props_initialized() {
		let props = PropertySet::new();
		init_interact_props(&props);
		// Value 无 PartialEq（指针/CString 成员），用 matches! 断言。
		assert!(
			matches!(
				props.get(crate::host::PROP_INTERACT_PIXEL_SCALE, 0),
				Some(Value::Double(1.0))
			),
			"PixelScale[0] 应为 1.0"
		);
		assert_eq!(props.dimension(crate::host::PROP_INTERACT_PIXEL_SCALE), 2);
		assert!(
			matches!(
				props.get(crate::host::PROP_INTERACT_SUGGESTED_COLOUR, 2),
				Some(Value::Double(1.0))
			),
			"SuggestedColour[2] 应为 1.0"
		);
		assert_eq!(props.dimension(crate::host::PROP_INTERACT_SLAVE_TO_PARAM), 0);
		assert!(
			matches!(
				props.get(crate::host::PROP_INTERACT_BIT_DEPTH, 0),
				Some(Value::Int(32))
			),
			"BitDepth 应为 32"
		);
		assert!(
			matches!(
				props.get(crate::host::PROP_INTERACT_HAS_ALPHA, 0),
				Some(Value::Int(1))
			),
			"HasAlpha 应为 1"
		);
	}

	/// read_double2 帮助函数。
	#[test]
	fn read_double2_helper() {
		let props = PropertySet::new();
		props.define("P", vec![Value::Double(2.0), Value::Double(3.0)]);
		assert_eq!(read_double2(&props, "P"), Some((2.0, 3.0)));
		assert_eq!(read_double2(&props, "Q"), None);
	}

	/// 空指针/错标签 → ErrBadHandle（interact suite 的句柄校验）。
	#[test]
	fn interact_suite_handle_validation() {
		unsafe {
			assert_eq!(interact_swap_buffers(std::ptr::null_mut()), status::ERR_BAD_HANDLE);
			assert_eq!(interact_redraw(std::ptr::null_mut()), status::ERR_BAD_HANDLE);
			let mut out: *mut c_void = std::ptr::null_mut();
			assert_eq!(
				interact_get_property_set(std::ptr::null_mut(), &mut out),
				status::ERR_BAD_HANDLE
			);
			// 错标签：裸 PropertySet（标签 0）当 interact 用。
			let props = PropertySet::new();
			let raw = &props as *const PropertySet as *mut c_void;
			assert_eq!(interact_swap_buffers(raw), status::ERR_BAD_HANDLE);
		}
	}

	/// interact suite 服务端：swap/redraw 请求被记录（app 侧轮询面），
	/// getPropertySet 返回 interact 属性集句柄。
	#[test]
	fn interact_suite_requests_are_recorded() {
		// 用假插件构造 Interact（不进插件入口——只测 suite 服务端）。
		let plugin = crate::host::Plugin {
			identifier: "fake".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::from("fake"),
			contexts: Vec::new(),
			descriptor: crate::descriptor::EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};
		let interact = Interact::new(Arc::new(plugin), dummy_entry, std::ptr::null_mut());
		let handle = interact.handle();

		unsafe {
			assert_eq!(interact_swap_buffers(handle), status::OK);
			assert_eq!(interact_redraw(handle), status::OK);
		}
		assert!(interact.swap_requested.load(Ordering::Relaxed));
		assert!(interact.redraw_requested.load(Ordering::Relaxed));

		let mut props_handle: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(interact_get_property_set(handle, &mut props_handle), status::OK);
		}
		assert_eq!(tag::kind(props_handle), tag::INTERACT);
		assert_eq!(
			tag::strip(props_handle) as usize,
			&interact.props as *const PropertySet as usize
		);

		// property suite 可直读 interact 属性集（句柄约定打通）。
		let addr = tag::strip(props_handle);
		let set: &PropertySet = unsafe { &*addr };
		assert!(
			matches!(
				set.get(crate::host::PROP_INTERACT_BIT_DEPTH, 0),
				Some(Value::Int(32))
			),
			"经 interact 句柄的 property suite 应读到 BitDepth=32"
		);

		// 摘除引用计数：Arc 释放（interact 已无引用）。
		drop(interact);
	}

	/// 假入口：所有 action 返回 ReplyDefault（不触达真实插件）。
	unsafe extern "C" fn dummy_entry(
		_action: *const c_char,
		_handle: *const c_void,
		_in: *mut c_void,
		_out: *mut c_void,
	) -> c_int {
		status::REPLY_DEFAULT
	}

	/// call 面透传：entry 收到 action 与 tagged handle（假插件记录）。
	#[test]
	fn interact_call_dispatches_to_entry() {
		static CALLED_ACTION: Mutex<Vec<String>> = Mutex::new(Vec::new());
		unsafe extern "C" fn recording_entry(
			action: *const c_char,
			handle: *const c_void,
			_in: *mut c_void,
			_out: *mut c_void,
		) -> c_int {
			CALLED_ACTION
				.lock()
				.unwrap()
				.push(unsafe { CStr::from_ptr(action) }.to_string_lossy().into_owned());
			// handle 必须是 tagged INTERACT。
			assert_eq!(tag::kind(handle as *mut c_void), tag::INTERACT);
			status::OK
		}
		let plugin = crate::host::Plugin {
			identifier: "fake".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::from("fake"),
			contexts: Vec::new(),
			descriptor: crate::descriptor::EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: recording_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};
		let interact = Interact::new(Arc::new(plugin), recording_entry, std::ptr::null_mut());

		assert_eq!(interact.describe(), status::OK);
		assert_eq!(interact.create_instance(), status::OK);
		assert_eq!(interact.pen_motion((10.0, 20.0), true, 5.0), status::OK);
		assert_eq!(interact.pen_down((10.0, 20.0), 5.0), status::OK);
		assert_eq!(interact.pen_up((10.0, 20.0), 5.0), status::OK);
		assert_eq!(interact.key_down(crate::host::KEY_A, "a", 5.0), status::OK);
		assert_eq!(interact.key_up(crate::host::KEY_A, "a", 5.0), status::OK);
		assert_eq!(interact.idle(), status::OK);
		interact.destroy();

		let calls = CALLED_ACTION.lock().unwrap().clone();
		let want = [
			crate::host::ACTION_DESCRIBE,
			crate::host::ACTION_CREATE_INSTANCE,
			crate::host::ACTION_INTERACT_PEN_MOTION,
			crate::host::ACTION_INTERACT_PEN_DOWN,
			crate::host::ACTION_INTERACT_PEN_UP,
			crate::host::ACTION_INTERACT_KEY_DOWN,
			crate::host::ACTION_INTERACT_KEY_UP,
			crate::host::ACTION_INTERACT_IDLE,
			crate::host::ACTION_DESTROY_INSTANCE,
		];
		assert_eq!(calls, want, "interact action 序列应按序透传到入口");
	}

	/// pen 事件的真实参数：inArgs 的 canonical 位置/视口位置/压力按
	/// pixelScale 换算（假插件读取记录）。
	#[test]
	fn pen_in_args_canonical_conversion() {
		// 记录 pen_motion 的 inArgs 关键属性。
		static CAPTURED: Mutex<Option<(f64, f64, f64)>> = Mutex::new(None);
		unsafe extern "C" fn capture_entry(
			_action: *const c_char,
			_handle: *const c_void,
			in_args: *mut c_void,
			_out: *mut c_void,
		) -> c_int {
			if in_args.is_null() {
				return status::OK;
			}
			let set = unsafe { &*(in_args as *const PropertySet) };
			// 视口位置（Int×2）与 canonical 位置（Double×2）与压力。
			let vx = match set.get(crate::host::PROP_INTERACT_PEN_VIEWPORT_POSITION, 0) {
				Some(Value::Int(i)) => i,
				_ => 0,
			};
			let px = match set.get(crate::host::PROP_INTERACT_PEN_POSITION, 0) {
				Some(Value::Double(d)) => d,
				_ => 0.0,
			};
			let pressure = match set.get(crate::host::PROP_INTERACT_PEN_PRESSURE, 0) {
				Some(Value::Double(d)) => d,
				_ => 0.0,
			};
			*CAPTURED.lock().unwrap() = Some((vx as f64, px, pressure));
			status::OK
		}
		let plugin = crate::host::Plugin {
			identifier: "fake".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::from("fake"),
			contexts: Vec::new(),
			descriptor: crate::descriptor::EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: capture_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};
		let interact = Interact::new(Arc::new(plugin), capture_entry, std::ptr::null_mut());
		// 设 pixelScale=2 → canonical = viewport/2。
		interact.props.define(
			crate::host::PROP_INTERACT_PIXEL_SCALE,
			vec![Value::Double(2.0), Value::Double(2.0)],
		);

		assert_eq!(interact.pen_motion((10.0, 20.0), true, 0.0), status::OK);
		let (vx, px, pressure) = CAPTURED.lock().unwrap().take().unwrap();
		assert_eq!(vx, 10.0);
		assert_eq!(px, 5.0);
		assert_eq!(pressure, 1.0);

		// pen_up 压力 0。
		assert_eq!(interact.pen_up((10.0, 20.0), 0.0), status::OK);
		let (_, _, pressure) = CAPTURED.lock().unwrap().take().unwrap();
		assert_eq!(pressure, 0.0);
	}

	/// key 事件真实参数：keySym/keyString 进 inArgs。
	#[test]
	fn key_in_args_carry_sym_and_string() {
		static CAPTURED: Mutex<Option<(i32, String)>> = Mutex::new(None);
		unsafe extern "C" fn capture_entry(
			_action: *const c_char,
			_handle: *const c_void,
			in_args: *mut c_void,
			_out: *mut c_void,
		) -> c_int {
			let set = unsafe { &*(in_args as *const PropertySet) };
			let sym = match set.get(crate::host::PROP_KEY_SYM, 0) {
				Some(Value::Int(i)) => i,
				_ => 0,
			};
			let s = match set.get(crate::host::PROP_KEY_STRING, 0) {
				Some(Value::String(c)) => c.to_string_lossy().into_owned(),
				_ => String::new(),
			};
			*CAPTURED.lock().unwrap() = Some((sym, s));
			status::OK
		}
		let plugin = crate::host::Plugin {
			identifier: "fake".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::from("fake"),
			contexts: Vec::new(),
			descriptor: crate::descriptor::EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: capture_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};
		let interact = Interact::new(Arc::new(plugin), capture_entry, std::ptr::null_mut());
		assert_eq!(interact.key_down(crate::host::KEY_RETURN, "", 1.0), status::OK);
		let (sym, s) = CAPTURED.lock().unwrap().take().unwrap();
		assert_eq!(sym, crate::host::KEY_RETURN);
		assert_eq!(s, "");
		assert_eq!(interact.key_up(crate::host::KEY_A, "a", 1.0), status::OK);
		let (sym, s) = CAPTURED.lock().unwrap().take().unwrap();
		assert_eq!(sym, crate::host::KEY_A);
		assert_eq!(s, "a");
	}

	/// destroy 幂等：只发一次。
	#[test]
	fn destroy_is_idempotent() {
		static COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
		unsafe extern "C" fn counting_entry(
			action: *const c_char,
			_handle: *const c_void,
			_in: *mut c_void,
			_out: *mut c_void,
		) -> c_int {
			let a = unsafe { CStr::from_ptr(action) }.to_string_lossy();
			if a == crate::host::ACTION_DESTROY_INSTANCE {
				COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			}
			status::OK
		}
		let plugin = crate::host::Plugin {
			identifier: "fake".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::from("fake"),
			contexts: Vec::new(),
			descriptor: crate::descriptor::EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: counting_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};
		let interact = Interact::new(Arc::new(plugin), counting_entry, std::ptr::null_mut());
		interact.destroy();
		interact.destroy();
		assert_eq!(COUNT.load(std::sync::atomic::Ordering::Relaxed), 1);
	}
}
