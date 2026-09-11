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

//! Host 单例与插件缓存。
//!
//! 对应 C++ 的 `OliveHost` + HostSupport 的 `PluginCache`。职责：
//! bundle 扫描、`dlopen` 插件、调 `setHost`/`load`/`describe`
//! 建立 [`Plugin`] 记录、按标识创建实例。参照：
//! HS: ofxhPluginCache.cpp（扫描去重与索引语义）、
//! HS: ofxhImageEffectAPI.cpp:150-238（load/describe/
//! describeInContext 的 action 序列）。
//!
//! ## 扫描语义（对照 olivehost.cpp:118-191 + OFX 官方规范）
//!
//! 默认路径集按平台组织：
//! 用户级（`$HOME` 非空时）——`$HOME/.OFX/Plugins`、
//! `$HOME/.local/share/OFX/Plugins`、`$HOME/.local/share/olive/ofx/Plugins`
//! （macOS 另含 `$HOME/Library/OFX/Plugins`）；
//! 系统级——macOS `/Library/OFX/Plugins`，Linux `/usr/OFX/Plugins`、
//! `/usr/local/OFX/Plugins`，Windows `%ProgramFiles%\Common Files\OFX\Plugins`
//! （`%ProgramFiles%` 未设置时回退 `C:\Program Files\...`）；
//! Olive app-relative——`../OFX/Plugins`、`../share/olive/ofx/Plugins`、
//! `../lib/olive/ofx/Plugins`（olivehost.cpp:104-107）；
//! 以及 `OFX_PLUGIN_PATH`（OFX 官方）与 `OLIVE_OFX_PLUGIN_PATH`/
//! `OLIVE_PLUGIN_PATH`（Olive 扩展）三个环境变量（平台路径分隔符：
//! Unix ':' / Windows ';'）。不存在的目录直接跳过；重复扫描按规范化
//! 路径去重。
//! `// [P2]`：Info.plist 解析（CFBundleExecutable）不做——bundle 内
//! 二进制以探测方式定位（`Contents/MacOS/*`、`Contents/Linux-*/*`、
//! 根下 `*.so`/`*.dylib`）。
//!
//! ## dlopen（零依赖）
//!
//! 直接声明 `dlopen`/`dlsym`/`dlclose`（macOS libSystem / Linux libc
//! 内建；RTLD 常量按平台取值）。加载顺序按 ofxCore.h:574：
//! `OfxGetNumberOfPlugins` → `OfxGetPlugin`，随后
//! `OfxPlugin::setHost`（mandatory，ofxCore.h:124-132）→
//! `load` → `describe`（HS: ofxhImageEffectAPI.cpp:158-190）。

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::descriptor::EffectDescriptor;
use crate::handle::RefBox;
use crate::instance::Instance;
use crate::property::{PropertySet, Value};
use crate::suites::status;

// ---- dlopen FFI（零依赖；RTLD 常量按平台）----

#[cfg(target_os = "macos")]
const RTLD_NOW: c_int = 0x2;
#[cfg(target_os = "linux")]
const RTLD_NOW: c_int = 0x2;
#[cfg(target_os = "macos")]
const RTLD_LOCAL: c_int = 0x4;
#[cfg(target_os = "linux")]
const RTLD_LOCAL: c_int = 0x0;

// Windows has no dlopen/dlsym/dlclose: the POSIX FFI below is compiled
// out and replaced by LoadLibraryExW/GetProcAddress/FreeLibrary (see the
// `win32` module further down), keeping the same function signatures.
#[cfg(not(target_os = "windows"))]
extern "C" {
	fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
	fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
	fn dlclose(handle: *mut c_void) -> c_int;
}

#[cfg(not(target_os = "windows"))]
/// 动态加载共享库；失败返回 None。
fn dl_open(path: &Path) -> Option<*mut c_void> {
	let c = CString::new(path.to_str()?).ok()?;
	let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
	if h.is_null() {
		None
	} else {
		Some(h)
	}
}

#[cfg(not(target_os = "windows"))]
/// 查符号；失败返回 None。
fn dl_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
	let c = CString::new(name).ok()?;
	let p = unsafe { dlsym(handle, c.as_ptr()) };
	if p.is_null() {
		None
	} else {
		Some(p)
	}
}

#[cfg(not(target_os = "windows"))]
/// 从 `dlsym` 结果取函数指针（libloading 同款转换；调用方保证符号
/// 类型正确）。
///
/// # Safety
/// 符号必须确实是该函数类型。
unsafe fn dlsym_fn<T>(handle: *mut c_void, name: &str) -> Option<T> {
	let p = dl_sym(handle, name)?;
	Some(unsafe { std::mem::transmute_copy(&p) })
}

// ---- Windows dynamic loading (kernel32) ----
//
// LoadLibraryExW / GetProcAddress / FreeLibrary in place of the POSIX
// dlopen family. The Windows handle is an HMODULE, which is pointer-sized
// and stored in the same `*mut c_void` slot, so the public signatures
// (`dl_open` / `dl_sym` / `dlsym_fn` / `dlclose`) are unchanged.

/// Windows dynamic loading (`LoadLibraryExW` / `GetProcAddress` /
/// `FreeLibrary`, kernel32). The handle is an `HMODULE` kept as
/// `*mut c_void` for signature parity with the POSIX path.
#[cfg(target_os = "windows")]
mod win32 {
	use super::*;
	use std::os::windows::ffi::OsStrExt;

	/// `LOAD_WITH_ALTERED_SEARCH_PATH` (winbase.h): resolve the loaded
	/// module's dependent DLLs from the module's own directory first, so a
	/// plugin bundle's sibling DLLs are found next to the binary.
	const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;

	#[link(name = "kernel32")]
	unsafe extern "C" {
		fn LoadLibraryExW(
			lp_file_name: *const u16,
			h_file: *mut c_void,
			dw_flags: u32,
		) -> *mut c_void;
		fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const c_char) -> *mut c_void;
		fn FreeLibrary(h_module: *mut c_void) -> c_int;
	}

	/// Dynamically load a shared library from an arbitrary (possibly
	/// non-UTF-8) path; `None` on failure. The path is converted to a
	/// NUL-terminated UTF-16 string for `LoadLibraryExW`.
	pub(super) fn dl_open(path: &Path) -> Option<*mut c_void> {
		let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
		let h = unsafe {
			LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH)
		};
		if h.is_null() {
			None
		} else {
			Some(h)
		}
	}

	/// Look up an exported symbol by name; `None` on failure. The symbol
	/// name is a narrow (ANSI) string — `OfxGetNumberOfPlugins` /
	/// `OfxGetPlugin`, both plain ASCII.
	pub(super) fn dl_sym(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
		let c = CString::new(name).ok()?;
		let p = unsafe { GetProcAddress(handle, c.as_ptr()) };
		if p.is_null() {
			None
		} else {
			Some(p)
		}
	}

	/// Function pointer from a `GetProcAddress` result (same transmute as
	/// the POSIX `dlsym_fn`; the caller guarantees the symbol type).
	///
	/// # Safety
	/// The symbol must actually be that function type.
	pub(super) unsafe fn dlsym_fn<T>(handle: *mut c_void, name: &str) -> Option<T> {
		let p = dl_sym(handle, name)?;
		Some(unsafe { std::mem::transmute_copy(&p) })
	}

	/// Unload a library (`FreeLibrary`; reference-counted like POSIX
	/// `dlclose`).
	pub(super) unsafe fn dlclose(handle: *mut c_void) -> c_int {
		unsafe { FreeLibrary(handle) }
	}
}

#[cfg(target_os = "windows")]
use win32::{dl_open, dl_sym, dlsym_fn, dlclose};

// ---- OFX 宿主侧结构（ofxCore.h，字段序与 SDK 逐字一致）----

/// `OfxHost`（ofxCore.h:44）。
#[repr(C)]
struct OfxHost {
	/// 宿主属性集句柄（`Host::props` 的裸指针，标签 0）。
	host: *mut c_void,
	/// fetchSuite。
	fetch_suite: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> *const c_void,
}

/// `OfxPlugin`（ofxCore.h:96）。
#[repr(C)]
struct OfxPlugin {
	plugin_api: *const c_char,
	api_version: c_int,
	plugin_identifier: *const c_char,
	plugin_version_major: c_uint,
	plugin_version_minor: c_uint,
	set_host: Option<unsafe extern "C" fn(*mut OfxHost)>,
	main_entry: Option<
		unsafe extern "C" fn(*const c_char, *const c_void, *mut c_void, *mut c_void) -> c_int,
	>,
}

/// fetchSuite 的宿主出口（插件经 `OfxHost::fetchSuite` 拿到；
/// 经 [`crate::suites::fetch_suite`] 分发表）。
unsafe extern "C" fn host_fetch_suite(
	host: *mut c_void,
	name: *const c_char,
	version: c_int,
) -> *const c_void {
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if host.is_null() || name.is_null() {
			return None;
		}
		let name = unsafe { CStr::from_ptr(name) }.to_str().ok()?;
		crate::suites::fetch_suite(name, version)
	}));
	match result {
		Ok(Some(p)) => {
			if std::env::var_os("OAK_OFX_TRACE").is_some() && !name.is_null() {
				if let Ok(n) = unsafe { CStr::from_ptr(name) }.to_str() {
					eprintln!("[ofx] fetchSuite hit: {n} v{version}");
				}
			}
			p
		}
		Ok(None) => {
			// 诊断：插件请求的 suite 宿主没有。厂商套件（Nuke/Vegas/
			// Foundry）的探测是插件的正常行为，不打正式日志——需要
			// 排查时用 OAK_OFX_TRACE 打开。
			if std::env::var_os("OAK_OFX_TRACE").is_some() && !name.is_null() {
				if let Ok(n) = unsafe { CStr::from_ptr(name) }.to_str() {
					eprintln!("[ofx] fetchSuite miss: {n} v{version}");
				}
			}
			std::ptr::null()
		}
		_ => std::ptr::null(),
	}
}

/// 进程级 `OfxHost`（ofxs 支持库的 setHost 只保存**指针**而不拷贝
/// 结构体——栈上临时变量会在 setHost 返回后悬垂，describe/渲染期
/// 再经 fetchSuite 回调就是野指针）。堆泄漏一次，永久有效。
fn global_ofx_host() -> *mut OfxHost {
	static PTR: OnceLock<usize> = OnceLock::new();
	*PTR.get_or_init(|| {
		let host = Host::global();
		Box::into_raw(Box::new(OfxHost {
			host: &host.props as *const PropertySet as *mut c_void,
			fetch_suite: host_fetch_suite,
		})) as usize
	}) as *mut OfxHost
}

// ---- 动作与属性常量（ofxCore.h / ofxImageEffect.h）----

/// kOfxActionLoad。
pub(crate) const ACTION_LOAD: &str = "OfxActionLoad";
/// kOfxActionDescribe。
pub(crate) const ACTION_DESCRIBE: &str = "OfxActionDescribe";
/// kOfxActionCreateInstance。
pub(crate) const ACTION_CREATE_INSTANCE: &str = "OfxActionCreateInstance";
/// kOfxActionDestroyInstance。
pub(crate) const ACTION_DESTROY_INSTANCE: &str = "OfxActionDestroyInstance";
/// kOfxImageEffectActionDescribeInContext。
pub(crate) const ACTION_DESCRIBE_IN_CONTEXT: &str = "OfxImageEffectActionDescribeInContext";
/// kOfxImageEffectActionGetClipPreferences。
pub(crate) const ACTION_GET_CLIP_PREFERENCES: &str = "OfxImageEffectActionGetClipPreferences";
/// kOfxImageEffectActionGetRegionOfDefinition。
pub(crate) const ACTION_GET_ROD: &str = "OfxImageEffectActionGetRegionOfDefinition";
/// kOfxImageEffectActionGetRegionsOfInterest。
pub(crate) const ACTION_GET_ROI: &str = "OfxImageEffectActionGetRegionsOfInterest";
/// kOfxImageEffectActionIsIdentity。
pub(crate) const ACTION_IS_IDENTITY: &str = "OfxImageEffectActionIsIdentity";
/// kOfxImageEffectActionRender。
pub(crate) const ACTION_RENDER: &str = "OfxImageEffectActionRender";
/// kOfxImageEffectActionBeginSequenceRender。
pub(crate) const ACTION_BEGIN_SEQUENCE: &str = "OfxImageEffectActionBeginSequenceRender";
/// kOfxImageEffectActionEndSequenceRender。
pub(crate) const ACTION_END_SEQUENCE: &str = "OfxImageEffectActionEndSequenceRender";
/// kOfxActionOpenGLContextAttached（ofxGPURender.h:345）。
pub(crate) const ACTION_GL_CONTEXT_ATTACHED: &str = "OfxActionOpenGLContextAttached";
/// kOfxActionOpenGLContextDetached（ofxGPURender.h:371；宏值自带
/// "kOfx" 前缀，是规范原文）。
pub(crate) const ACTION_GL_CONTEXT_DETACHED: &str = "kOfxActionOpenGLContextDetached";
/// kOfxImageEffectActionGetOutputColourspace（ofxColour.h:283）。
pub(crate) const ACTION_GET_OUTPUT_COLOURSPACE: &str = "OfxImageEffectActionGetOutputColourspace";
/// kOfxActionInstanceChanged（ofxCore.h:449）：宿主侧参数/时间变更
/// 通知。按下 push button 后宿主须以 kOfxChangeUserEdited 的原因调用
/// 它（ofxCore.h:405-435 的 inArgs 契约）。
pub(crate) const ACTION_INSTANCE_CHANGED: &str = "OfxActionInstanceChanged";
// ---- Interact（OFX 自定义交互；ofxInteract.h / ofxDrawSuite.h / ofxKeySyms.h）----

/// kOfxActionNewInteract：宿主创建 interact 时发给 interact 入口的
/// 首个 action。vendored ofxInteract.h 未定义该宏（官方头文件无此
/// action；任务契约按 "向插件 main entry 发 kOfxActionNewInteract"
/// 命名）——取值 "OfxActionNewInteract"，与 OFX action 命名惯例一致。
/// 插件不实现该 action 时返回 kOfxStatReplyDefault（宿主视为"无
/// interact"或"不参与 NewInteract 协议"，见 [`crate::instance::Instance::new_interact`]）。
pub(crate) const ACTION_NEW_INTERACT: &str = "OfxActionNewInteract";
/// kOfxInteractActionIdle：宿主空闲泵（任务契约；vendored ofxInteract.h
/// 未收录——OFX 官方规范无 Idle action，属本宿主扩展，取值
/// "OfxInteractActionIdle"）。插件实现与否自愿；未处理返回
/// kOfxStatReplyDefault。
pub(crate) const ACTION_INTERACT_IDLE: &str = "OfxInteractActionIdle";
/// kOfxInteractActionDraw（ofxInteract.h:265）。
pub(crate) const ACTION_INTERACT_DRAW: &str = "OfxInteractActionDraw";
/// kOfxInteractActionPenMotion（ofxInteract.h:302）。
pub(crate) const ACTION_INTERACT_PEN_MOTION: &str = "OfxInteractActionPenMotion";
/// kOfxInteractActionPenDown（ofxInteract.h:340）。
pub(crate) const ACTION_INTERACT_PEN_DOWN: &str = "OfxInteractActionPenDown";
/// kOfxInteractActionPenUp（ofxInteract.h:376）。
pub(crate) const ACTION_INTERACT_PEN_UP: &str = "OfxInteractActionPenUp";
/// kOfxInteractActionKeyDown（ofxInteract.h:410）。
pub(crate) const ACTION_INTERACT_KEY_DOWN: &str = "OfxInteractActionKeyDown";
/// kOfxInteractActionKeyUp（ofxInteract.h:443）。
pub(crate) const ACTION_INTERACT_KEY_UP: &str = "OfxInteractActionKeyUp";
/// kOfxInteractActionGainFocus（ofxInteract.h:501）。
pub(crate) const ACTION_INTERACT_GAIN_FOCUS: &str = "OfxInteractActionGainFocus";
/// kOfxInteractActionLoseFocus（ofxInteract.h:526）。
pub(crate) const ACTION_INTERACT_LOSE_FOCUS: &str = "OfxInteractActionLoseFocus";

/// kOfxInteractPropPixelScale（ofxInteract.h:58）：canonical→屏幕像素
/// 换算比例（Double×2）。
pub(crate) const PROP_INTERACT_PIXEL_SCALE: &str = "OfxInteractPropPixelScale";
/// kOfxInteractPropViewportSize（OFX 1.3 命名 "OfxInteractPropViewport"；
/// vendored 1.5 头文件已删，任务契约要求 draw inArgs 携带视口尺寸）。
pub(crate) const PROP_INTERACT_VIEWPORT_SIZE: &str = "OfxInteractPropViewport";
/// kOfxInteractPropBackgroundImage：任务契约的 draw inArgs 背景图像
/// 句柄（Pointer；无背景时为空——本宿主 Phase 1 无合成背景，恒空）。
/// 非官方 OFX 属性（官方只有 BackgroundColour），属本宿主扩展。
pub(crate) const PROP_INTERACT_BACKGROUND_IMAGE: &str = "OfxInteractPropBackgroundImage";
/// kOfxInteractPropBackgroundColour（ofxInteract.h:71）：宿主视口背景色
/// （Double×3）。
pub(crate) const PROP_INTERACT_BACKGROUND_COLOUR: &str = "OfxInteractPropBackgroundColour";
/// kOfxInteractPropSuggestedColour（ofxInteract.h:86）：宿主建议的 overlay
/// 颜色（Double×3；宿主不支持颜色选择时返回 ReplyDefault）。
pub(crate) const PROP_INTERACT_SUGGESTED_COLOUR: &str = "OfxInteractPropSuggestedColour";
/// kOfxInteractPropSlaveToParam（ofxInteract.h:50）：值变化触发 interact
/// 重绘的参数名（String×N）。
pub(crate) const PROP_INTERACT_SLAVE_TO_PARAM: &str = "OfxInteractPropSlaveToParam";
/// kOfxInteractPropPenPosition（ofxInteract.h:95）：笔的 canonical 位置
/// （Double×2，只读 inArgs）。
pub(crate) const PROP_INTERACT_PEN_POSITION: &str = "OfxInteractPropPenPosition";
/// kOfxInteractPropPenViewportPosition（ofxInteract.h:104）：笔的视口像素
/// 位置（Int×2，只读 inArgs）。
pub(crate) const PROP_INTERACT_PEN_VIEWPORT_POSITION: &str = "OfxInteractPropPenViewportPosition";
/// kOfxInteractPropPenPressure（ofxInteract.h:114）：笔压（Double×1，
/// 0..1；两态笔映射 0/1）。
pub(crate) const PROP_INTERACT_PEN_PRESSURE: &str = "OfxInteractPropPenPressure";
/// kOfxInteractPropBitDepth（ofxInteract.h:122）：interact 帧缓冲位深
/// （Int×1，只读）。
pub(crate) const PROP_INTERACT_BIT_DEPTH: &str = "OfxInteractPropBitDepth";
/// kOfxInteractPropHasAlpha（ofxInteract.h:132）：interact 帧缓冲是否含
/// alpha（Int×1，只读）。
pub(crate) const PROP_INTERACT_HAS_ALPHA: &str = "OfxInteractPropHasAlpha";
/// kOfxInteractPropDrawContext（ofxDrawSuite.h:34）：Draw suite 上下文句柄
/// （Pointer；draw inArgs 携带，插件取来传给 Draw suite 函数）。
pub(crate) const PROP_INTERACT_DRAW_CONTEXT: &str = "OfxInteractPropDrawContext";
/// kOfxPropKeySym（ofxKeySyms.h:30）：键盘事件的关键码（Int×1）。
pub(crate) const PROP_KEY_SYM: &str = "kOfxPropKeySym";
/// kOfxPropKeyString（ofxKeySyms.h:49）：键盘事件的 UTF-8 字符（String×1）。
pub(crate) const PROP_KEY_STRING: &str = "kOfxPropKeyString";
/// kOfxImageEffectPluginPropOverlayInteractV2（ofxImageEffect.h:825）：
/// 插件声明的 overlay interact 入口（Pointer→OfxPluginEntryPoint；V2
/// 要求 Draw suite 绘制）。
pub(crate) const PROP_OVERLAY_INTERACT_V2: &str = "OfxImageEffectPluginPropOverlayInteractV2";
/// kOfxImageEffectPluginPropOverlayInteractV1（ofxImageEffect.h:812）。
pub(crate) const PROP_OVERLAY_INTERACT_V1: &str = "OfxImageEffectPluginPropOverlayInteractV1";
/// kOfxImageEffectPropSupportsOverlays（ofxImageEffect.h:801）：宿主是否
/// 允许插件在输出图像上绘制 overlay（能力宣告）。
pub(crate) const PROP_SUPPORTS_OVERLAYS: &str = "OfxImageEffectPropSupportsOverlays";

// ---- OFX 关键码（ofxKeySyms.h；X11 keysym 值，测试/宿主常用子集）----
//
// 公共：app 侧（WG3b）经 [`crate::suites::interact::Interact::key_down`]/
// `key_up` 传关键码。

/// kOfxKey_Unknown（ofxKeySyms.h:121）。
pub const KEY_UNKNOWN: i32 = 0x0;
/// kOfxKey_BackSpace（ofxKeySyms.h:128）。
pub const KEY_BACKSPACE: i32 = 0xFF08;
/// kOfxKey_Tab（ofxKeySyms.h:129）。
pub const KEY_TAB: i32 = 0xFF09;
/// kOfxKey_Return（ofxKeySyms.h:132）。
pub const KEY_RETURN: i32 = 0xFF0D;
/// kOfxKey_Escape（ofxKeySyms.h:136）。
pub const KEY_ESCAPE: i32 = 0xFF1B;
/// kOfxKey_Delete（ofxKeySyms.h:137）。
pub const KEY_DELETE: i32 = 0xFFFF;
/// kOfxKey_Home（ofxKeySyms.h:172）。
pub const KEY_HOME: i32 = 0xFF50;
/// kOfxKey_Left（ofxKeySyms.h:173）。
pub const KEY_LEFT: i32 = 0xFF51;
/// kOfxKey_Up（ofxKeySyms.h:174）。
pub const KEY_UP: i32 = 0xFF52;
/// kOfxKey_Right（ofxKeySyms.h:175）。
pub const KEY_RIGHT: i32 = 0xFF53;
/// kOfxKey_Down（ofxKeySyms.h:176）。
pub const KEY_DOWN: i32 = 0xFF54;
/// kOfxKey_Page_Up（ofxKeySyms.h:178）。
pub const KEY_PAGE_UP: i32 = 0xFF55;
/// kOfxKey_Page_Down（ofxKeySyms.h:180）。
pub const KEY_PAGE_DOWN: i32 = 0xFF56;
/// kOfxKey_End（ofxKeySyms.h:181）。
pub const KEY_END: i32 = 0xFF57;
/// kOfxKey_F1（ofxKeySyms.h:252）。
pub const KEY_F1: i32 = 0xFFBE;
/// kOfxKey_Shift_L（ofxKeySyms.h:315）。
pub const KEY_SHIFT_L: i32 = 0xFFE1;
/// kOfxKey_Control_L（ofxKeySyms.h:317）。
pub const KEY_CONTROL_L: i32 = 0xFFE3;
/// kOfxKey_Alt_L（ofxKeySyms.h:323）。
pub const KEY_ALT_L: i32 = 0xFFE9;
/// kOfxKey_space（ofxKeySyms.h:331）。
pub const KEY_SPACE: i32 = 0x020;
/// kOfxKey_a（ofxKeySyms.h:398）。
pub const KEY_A: i32 = 0x061;
/// kOfxKey_z（ofxKeySyms.h:423）。
pub const KEY_Z: i32 = 0x07a;

/// kOfxPropChangeReason（ofxCore.h:763）：instanceChanged 的 inArgs 里
/// 说明变更来源（UserEdited / PluginEdited / Time）。
pub(crate) const PROP_CHANGE_REASON: &str = "OfxPropChangeReason";
/// kOfxChangeUserEdited（ofxCore.h:792）。
pub(crate) const CHANGE_USER_EDITED: &str = "OfxChangeUserEdited";
/// kOfxChangePluginEdited（ofxCore.h:795）。
pub(crate) const CHANGE_PLUGIN_EDITED: &str = "OfxChangePluginEdited";
/// kOfxChangeTime（ofxCore.h:798）。
pub(crate) const CHANGE_TIME: &str = "OfxChangeTime";

/// kOfxPropTime（ofxCore.h:613）。
pub(crate) const PROP_TIME: &str = "OfxPropTime";
/// kOfxPropEffectInstance（ofxCore.h:776）：interact/渲染 inArgs 里指向
/// 效果实例句柄的指针属性。
pub(crate) const PROP_EFFECT_INSTANCE: &str = "OfxPropEffectInstance";
/// kOfxImageEffectPropRenderScale。
pub(crate) const PROP_RENDER_SCALE: &str = "OfxImageEffectPropRenderScale";
/// kOfxImageEffectPropRenderWindow。
pub(crate) const PROP_RENDER_WINDOW: &str = "OfxImageEffectPropRenderWindow";
/// kOfxImageEffectPropRegionOfInterest。
pub(crate) const PROP_ROI: &str = "OfxImageEffectPropRegionOfInterest";
/// kOfxImageEffectPropRegionOfDefinition。
pub(crate) const PROP_ROD: &str = "OfxImageEffectPropRegionOfDefinition";
/// kOfxImageEffectPropIsIdentity。
pub(crate) const PROP_IS_IDENTITY: &str = "OfxImageEffectPropIsIdentity";
/// kOfxImageEffectPropFieldToRender。
pub(crate) const PROP_FIELD_TO_RENDER: &str = "OfxImageEffectPropFieldToRender";
/// kOfxImageEffectPropSequentialRenderStatus。
pub(crate) const PROP_SEQUENTIAL_RENDER: &str = "OfxImageEffectPropSequentialRenderStatus";
/// kOfxImageEffectPropInteractiveRenderStatus。
pub(crate) const PROP_INTERACTIVE_RENDER: &str = "OfxImageEffectPropInteractiveRenderStatus";
/// kOfxImageEffectPropRenderQualityDraft。
pub(crate) const PROP_RENDER_QUALITY_DRAFT: &str = "OfxImageEffectPropRenderQualityDraft";
/// kOfxImageEffectPropNoSpatialAwareness。
pub(crate) const PROP_NO_SPATIAL_AWARENESS: &str = "OfxImageEffectPropNoSpatialAwareness";
/// kOfxImageEffectPropFrameRange。
pub(crate) const PROP_FRAME_RANGE: &str = "OfxImageEffectPropFrameRange";
/// kOfxImageEffectPropFrameStep。
pub(crate) const PROP_FRAME_STEP: &str = "OfxImageEffectPropFrameStep";
/// kOfxImageEffectPropFrameRate。
pub(crate) const PROP_FRAME_RATE: &str = "OfxImageEffectPropFrameRate";
/// kOfxImageEffectPropPreMultiplication。
pub(crate) const PROP_PREMULT: &str = "OfxImageEffectPropPreMultiplication";
/// kOfxImageClipPropFieldOrder。
pub(crate) const PROP_FIELD_ORDER: &str = "OfxImageClipPropFieldOrder";
/// kOfxImageClipPropContinuousSamples。
pub(crate) const PROP_CONTINUOUS_SAMPLES: &str = "OfxImageClipPropContinuousSamples";
/// kOfxImageEffectFrameVarying。
pub(crate) const PROP_FRAME_VARYING: &str = "OfxImageEffectFrameVarying";
/// kOfxImageClipPropConnected。
pub(crate) const PROP_CLIP_CONNECTED: &str = "OfxImageClipPropConnected";
/// kOfxImageEffectPropContext。
pub(crate) const PROP_CONTEXT: &str = "OfxImageEffectPropContext";
/// kOfxImageEffectPropSupportedContexts。
pub(crate) const PROP_SUPPORTED_CONTEXTS: &str = "OfxImageEffectPropSupportedContexts";
/// kOfxImageEffectPropProjectSize。
pub(crate) const PROP_PROJECT_SIZE: &str = "OfxImageEffectPropProjectSize";
/// kOfxImageEffectPropProjectOffset。
pub(crate) const PROP_PROJECT_OFFSET: &str = "OfxImageEffectPropProjectOffset";
/// kOfxImageEffectPropProjectExtent。
pub(crate) const PROP_PROJECT_EXTENT: &str = "OfxImageEffectPropProjectExtent";
/// kOfxImageEffectPropProjectPixelAspectRatio（宏值即 PixelAspectRatio）。
pub(crate) const PROP_PROJECT_PAR: &str = "OfxImageEffectPropPixelAspectRatio";
/// kOfxImageEffectInstancePropEffectDuration。
pub(crate) const PROP_EFFECT_DURATION: &str = "OfxImageEffectInstancePropEffectDuration";
/// kOfxImageEffectInstancePropSequentialRender。
pub(crate) const PROP_SEQUENTIAL: &str = "OfxImageEffectInstancePropSequentialRender";
/// kOfxImageEffectPropPluginHandle。
pub(crate) const PROP_PLUGIN_HANDLE: &str = "OfxImageEffectPropPluginHandle";
/// kOfxPluginPropFilePath。
pub(crate) const PROP_PLUGIN_FILE_PATH: &str = "OfxPluginPropFilePath";
/// kOfxImageEffectPropSupportsTiles。
pub(crate) const PROP_SUPPORTS_TILES: &str = "OfxImageEffectPropSupportsTiles";
/// kOfxPropType。
pub(crate) const PROP_TYPE: &str = "OfxPropType";
/// kOfxPropLabel。
pub(crate) const PROP_LABEL: &str = "OfxPropLabel";

// ---- GL 能力与协商属性（ofxGPURender.h，M11 第 2 期）----

/// kOfxImageEffectPropOpenGLRenderSupported（ofxGPURender.h:62）：
/// 宿主描述符（"true"）与插件描述符（"false"/"true"/"needed"）。
pub(crate) const PROP_GL_RENDER_SUPPORTED: &str = "OfxImageEffectPropOpenGLRenderSupported";
/// kOfxImageEffectPropOpenGLEnabled（ofxGPURender.h:117）：Render/
/// Begin/EndSequenceRender 的 in args。
pub(crate) const PROP_GL_ENABLED: &str = "OfxImageEffectPropOpenGLEnabled";
/// kOfxOpenGLPropPixelDepth（ofxGPURender.h:89）：插件描述符的 GL
/// 渲染支持位深（可选；格式协商的宿主侧输入）。
pub(crate) const PROP_GL_PIXEL_DEPTH: &str = "OfxOpenGLPropPixelDepth";

// ---- ofxColour 属性（ofxColour.h，M11 第 2 期）----

/// kOfxImageEffectPropColourManagementStyle（ofxColour.h:60）。
pub(crate) const PROP_COLOUR_STYLE: &str = "OfxImageEffectPropColourManagementStyle";
/// kOfxImageEffectColourManagementOCIO（ofxColour.h:73）：宿主声明的
/// 色彩管理模式（任务要求）。
pub(crate) const COLOUR_STYLE_OCIO: &str = "OfxImageEffectColourManagementOCIO";
/// kOfxImageEffectPropColourManagementAvailableConfigs（ofxColour.h:85）。
pub(crate) const PROP_COLOUR_AVAILABLE_CONFIGS: &str =
	"OfxImageEffectPropColourManagementAvailableConfigs";
/// kOfxImageEffectPropColourManagementConfig（ofxColour.h:98）：实例期
/// 协商出的 native 配置。
pub(crate) const PROP_COLOUR_CONFIG: &str = "OfxImageEffectPropColourManagementConfig";
/// kOfxImageEffectPropOCIOConfig（ofxColour.h:110）：实例期 OCIO 配置
/// 路径/URI。
pub(crate) const PROP_OCIO_CONFIG: &str = "OfxImageEffectPropOCIOConfig";
/// kOfxImageClipPropColourspace（ofxColour.h:142）：clip 实际色彩空间。
pub(crate) const PROP_CLIP_COLOURSPACE: &str = "OfxImageClipPropColourspace";
/// kOfxImageClipPropPreferredColourspaces（ofxColour.h:201）：clip 偏好
/// 色彩空间（GetClipPreferences 的 out args 可写）。
pub(crate) const PROP_CLIP_PREFERRED_COLOURSPACES: &str = "OfxImageClipPropPreferredColourspaces";
/// 宿主支持的 native 色彩配置标识（ofxColour.h:78 的唯一现行值）。
pub(crate) const NATIVE_CONFIG_ID: &str = "ofx-native-v1.5_aces-v1.3_ocio-v2.3";
/// 宿主 OCIO 配置 URI（ofxColour.h:110 允许 ocio:// 内建配置；本宿主
/// 无自有 config 文件，用 OCIO 内建默认配置并以 clip 色彩空间属性
/// 传达 ACEScg 工作空间）。
pub(crate) const OCIO_CONFIG_URI: &str = "ocio://default";

/// 协商期 per-clip 约定名（HS: "OfxImageClipPropComponents_<clip>"
/// 等，ofxhImageEffect.cpp:1725-1727）。
pub(crate) fn clip_pref_prop(prefix: &str, clip: &str) -> String {
	format!("{prefix}_{clip}")
}

/// 组件名前缀。
pub(crate) const CLIP_PREF_COMPONENTS: &str = "OfxImageClipPropComponents";
/// 位深名前缀。
pub(crate) const CLIP_PREF_DEPTH: &str = "OfxImageClipPropDepth";
/// 像素比名前缀。
pub(crate) const CLIP_PREF_PAR: &str = "OfxImageClipPropPAR";

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

// ---- Plugin --------------------------------------------------------------

/// 一个已加载插件（bundle 内的一个 effect 入口）。
pub struct Plugin {
	/// 插件标识（OfxPlugin::pluginIdentifier，如 "net.sf.cimg.CImgInvert"）。
	pub identifier: String,
	/// 版本号（major/minor）。
	pub version: (u32, u32),
	/// bundle 路径。
	pub bundle_path: PathBuf,
	/// describe 得到的支持上下文（filter/generator/transition）。
	pub contexts: Vec<String>,
	/// describe 产物（根描述符）。
	pub descriptor: EffectDescriptor,
	/// dlopen 句柄（公开字段供测试构造假插件）。
	pub lib: *mut c_void,
	/// 插件入口函数 `OfxPluginEntryPoint`（公开字段供测试构造；
	/// 签名与 SDK ofxCore.h:84 逐字一致——handle 是 `const void*`）。
	pub entry: unsafe extern "C" fn(
		action: *const c_char,
		handle: *const c_void,
		in_args: *mut c_void,
		out_args: *mut c_void,
	) -> i32,
	/// OfxPlugin 结构指针（kOfxImageEffectPropPluginHandle 属性用；
	/// 公开：测试构造假插件需要）。
	pub ofx_plugin: *mut c_void,
	/// 二进制已卸载（host shutdown / unload_all）。置位后任何
	/// call_action/call_entry 直接失败而不触碰入口——插件入口指向
	/// 已 dlclose 的代码，调用即 SIGSEGV（测试串行 shutdown/重扫描
	/// 会产生跨代实例，它们持有的正是旧代插件记录）。
	pub unloaded: std::sync::atomic::AtomicBool,
}

// dlopen 句柄与 OfxPlugin 指针是**不透明令牌**（只经 dlsym/插件
// 结构字段访问，不直接解引用）；跨线程搬运句柄是宿主分发语义。
// 与 property.rs 的 Value::Pointer 同理。
unsafe impl Send for Plugin {}
unsafe impl Sync for Plugin {}

/// 插件入口函数类型（`OfxPluginEntryPoint`，ofxCore.h:84）。
pub(crate) type EntryPoint = unsafe extern "C" fn(
	action: *const c_char,
	handle: *const c_void,
	in_args: *mut c_void,
	out_args: *mut c_void,
) -> i32;

/// 插件声明的 overlay interact 入口（ofxImageEffect.h:825/812）：
/// V2 优先、V1 次之；两者都未声明返回 None。属性值是
/// `Pointer→OfxPluginEntryPoint`（宿主预定义，插件 describe 期写入）。
pub(crate) fn overlay_interact_entry(props: &PropertySet) -> Option<EntryPoint> {
	for name in [PROP_OVERLAY_INTERACT_V2, PROP_OVERLAY_INTERACT_V1] {
		if let Some(Value::Pointer(p)) = props.get(name, 0) {
			if !p.is_null() {
				// 属性值是函数指针（void* 存放）；转换与 dlsym_fn 同款
				// （调用方保证类型正确）。
				return Some(unsafe { std::mem::transmute_copy(&p) });
			}
		}
	}
	None
}

impl Plugin {
	/// 调插件的 action。`handle` 视 action 而定（describe 时为
	/// descriptor，render 时为 instance）。`in_args`/`out_args` 按
	/// OFX 规范组装属性集。
	///
	/// 返回 OFX 状态码（kOfxStatOK == 0）。任何插件侧异常/崩溃信号
	/// 无法捕获——FFI 纪律只兜 Rust 侧 panic。
	///
	/// # Safety
	/// `handle`/参数集必须与 action 匹配（规范约定）。
	pub(crate) unsafe fn call_action(
		&self,
		action: &str,
		handle: *mut c_void,
		in_args: &PropertySet,
		out_args: &PropertySet,
	) -> i32 {
		if self.unloaded.load(std::sync::atomic::Ordering::Acquire) {
			return status::ERR_FATAL;
		}
		// 属性集经裸指针（标签 0）传给插件（property suite 接受）。
		let in_ptr = in_args as *const PropertySet as *mut c_void;
		let out_ptr = out_args as *const PropertySet as *mut c_void;
		let action = cs(action);
		unsafe { (self.entry)(action.as_ptr(), handle as *const c_void, in_ptr, out_ptr) }
	}

	/// 按指定入口函数调用（interact 的 overlay 入口与 main entry 可
	/// 不同——插件经 kOfxImageEffectPluginPropOverlayInteractV2 声明）。
	/// 语义同 [`Plugin::call_action`]。
	///
	/// # Safety
	/// `entry` 必须是插件导出的合法 OfxPluginEntryPoint；`handle`/参数
	/// 集与 action 匹配。
	pub(crate) unsafe fn call_entry(
		&self,
		entry: EntryPoint,
		action: &str,
		handle: *mut c_void,
		in_args: &PropertySet,
		out_args: &PropertySet,
	) -> i32 {
		if self.unloaded.load(std::sync::atomic::Ordering::Acquire) {
			return status::ERR_FATAL;
		}
		let in_ptr = in_args as *const PropertySet as *mut c_void;
		let out_ptr = out_args as *const PropertySet as *mut c_void;
		let action = cs(action);
		unsafe { (entry)(action.as_ptr(), handle as *const c_void, in_ptr, out_ptr) }
	}
}

// ---- PluginCache ---------------------------------------------------------

/// 一个已加载二进制（dlopen 句柄；多个 Plugin 可共享一个二进制）。
struct LoadedBinary {
	handle: *mut c_void,
	bundle_path: PathBuf,
}

impl Drop for LoadedBinary {
	fn drop(&mut self) {
		unsafe { dlclose(self.handle) };
	}
}

// 同 Plugin：dlopen 句柄为不透明令牌。
unsafe impl Send for LoadedBinary {}
unsafe impl Sync for LoadedBinary {}

/// 插件缓存（进程单例，见 [`Host::global`]）。
pub struct PluginCache {
	/// 已加载插件（identifier 唯一）。
	plugins: Mutex<Vec<Arc<Plugin>>>,
	/// 已扫描的搜索路径（防重复扫描）。
	scanned_paths: Mutex<Vec<PathBuf>>,
	/// 已加载二进制（dlclose 责任；shutdown 时释放）。
	binaries: Mutex<Vec<LoadedBinary>>,
}

/// bundle 目录判定：目录名以 `.bundle` 或 `.plugin` 结尾。
fn is_bundle_dir(path: &Path) -> bool {
	let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
		return false;
	};
	name.ends_with(".bundle") || name.ends_with(".plugin")
}

/// 在 bundle 目录内定位二进制（探测式；`// [P2]` 起读 Info.plist 的
/// CFBundleExecutable）。
fn find_binary_in_bundle(bundle: &Path) -> Option<PathBuf> {
	let contents = bundle.join("Contents");
	for platform_dir in [
		contents.join("MacOS"),
		contents.join("Linux-x86-64"),
		contents.join("Linux-aarch64"),
		// OFX 规范的 64 位 Windows 平台目录。
		contents.join("Win64"),
	] {
		if let Ok(entries) = std::fs::read_dir(&platform_dir) {
			for e in entries.flatten() {
				let p = e.path();
				if p.is_file() && !p.extension().is_some_and(|x| x == "plist") {
					return Some(p);
				}
			}
		}
	}
	// 退而求其次：bundle 根下的 .so/.dylib/.ofx。
	if let Ok(entries) = std::fs::read_dir(bundle) {
		for e in entries.flatten() {
			let p = e.path();
			let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
			if p.is_file() && (ext == "so" || ext == "dylib" || ext == "ofx") {
				return Some(p);
			}
		}
	}
	None
}

/// 默认插件搜索路径（扫描顺序）：OFX 规范的用户/系统级位置（按平台
/// cfg 组织）、Olive app-relative 位置（C++ 对齐，olivehost.cpp:104-107）。
///
/// OFX 官方标准位置（见 OFX "plug-in discovery" 规范）：
///
/// * macOS：用户 `~/Library/OFX/Plugins`、系统 `/Library/OFX/Plugins`
/// * Windows：`%ProgramFiles%\Common Files\OFX\Plugins`（`%ProgramFiles%`
///   未设置时回退字面 `C:\Program Files\...`）
/// * Linux：`/usr/OFX/Plugins`、`/usr/local/OFX/Plugins`
///
/// 外加 Olive 的历史位置：`$HOME/.OFX/Plugins`、
/// `$HOME/.local/share/OFX/Plugins`、`$HOME/.local/share/olive/ofx/Plugins`、
/// `../OFX/Plugins`、`../share/olive/ofx/Plugins`、`../lib/olive/ofx/Plugins`。
/// 环境变量（`OFX_PLUGIN_PATH`/`OLIVE_OFX_PLUGIN_PATH`/`OLIVE_PLUGIN_PATH`）
/// 由 [`PluginCache::scan`] 追加。`home` 为 `$HOME`；`None` 时跳过所有
/// home 基路径。
fn default_plugin_paths(home: Option<&Path>) -> Vec<PathBuf> {
	let mut paths = Vec::new();
	if let Some(home) = home {
		paths.push(home.join(".OFX/Plugins"));
		paths.push(home.join(".local/share/OFX/Plugins"));
		paths.push(home.join(".local/share/olive/ofx/Plugins"));
		// OFX 规范：macOS 用户级位置。
		#[cfg(target_os = "macos")]
		paths.push(home.join("Library/OFX/Plugins"));
	}
	// OFX 规范：macOS 系统级位置。
	#[cfg(target_os = "macos")]
	paths.push(PathBuf::from("/Library/OFX/Plugins"));
	// OFX 规范：Windows 系统级位置（优先真实安装根 `%ProgramFiles%`）。
	#[cfg(target_os = "windows")]
	{
		match std::env::var("ProgramFiles")
			.or_else(|_| std::env::var("ProgramW6432"))
			.map(PathBuf::from)
		{
			Ok(root) => paths.push(root.join("Common Files").join("OFX").join("Plugins")),
			Err(_) => paths.push(PathBuf::from(r"C:\Program Files\Common Files\OFX\Plugins")),
		}
	}
	// OFX 规范：Linux 系统级位置。
	#[cfg(target_os = "linux")]
	{
		paths.push(PathBuf::from("/usr/OFX/Plugins"));
		paths.push(PathBuf::from("/usr/local/OFX/Plugins"));
	}
	// Olive app-relative 位置（olivehost.cpp:104-107）。
	paths.push(PathBuf::from("../OFX/Plugins"));
	paths.push(PathBuf::from("../share/olive/ofx/Plugins"));
	paths.push(PathBuf::from("../lib/olive/ofx/Plugins"));
	paths
}

/// 把 `canonical` 记入已扫描路径表，返回 `false` 时表示已存在（调用方
/// 跳过再次扫描）。保持首见顺序；去重键是规范化后的路径（HS:
/// addFileToPath 的 weakly_canonical 语义）。
fn record_scanned_path(seen: &mut Vec<PathBuf>, canonical: PathBuf) -> bool {
	if seen.iter().any(|p| p == &canonical) {
		return false;
	}
	seen.push(canonical);
	true
}

impl PluginCache {
	/// 扫描标准目录（OFX 规范 + C++ 对齐）：用户级 `$HOME` 路径、平台
	/// 系统级路径（macOS/Linux/Windows）、Olive app-relative 路径，以及
	/// `OFX_PLUGIN_PATH`/`OLIVE_OFX_PLUGIN_PATH`/`OLIVE_PLUGIN_PATH` 三个
	/// 环境变量。重复调用是 no-op（按规范化路径去重）；不存在的目录
	/// 直接跳过（见 [`Self::scan_path`]）。
	pub fn scan(&self) -> crate::error::Result<()> {
		let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
		let mut paths = default_plugin_paths(home.as_deref().map(Path::new));
		for var in [
			"OLIVE_OFX_PLUGIN_PATH",
			"OLIVE_PLUGIN_PATH",
			"OFX_PLUGIN_PATH",
		] {
			if let Ok(raw) = std::env::var(var) {
				// 平台路径分隔符（Unix ':' / Windows ';'），与 C++ 的
				// `QDir::listSeparator` 一致。
				paths.extend(std::env::split_paths(&raw));
			}
		}
		for p in paths {
			self.scan_path(&p)?;
		}
		Ok(())
	}

	/// 追加一个自定义搜索目录并扫描之（递归至深度 3；按路径去重）。
	pub fn scan_path(&self, path: &std::path::Path) -> crate::error::Result<()> {
		// 规范化去重（HS: addFileToPath 的 weakly_canonical）。
		let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
		{
			let mut seen = self.scanned_paths.lock().unwrap_or_else(|e| e.into_inner());
			if !record_scanned_path(&mut seen, canonical.clone()) {
				return Ok(());
			}
		}
		if !canonical.is_dir() {
			return Ok(());
		}
		let mut bundles = Vec::new();
		walk_dirs(&canonical, 0, 3, &mut bundles);
		for bundle in bundles {
			self.load_bundle(&bundle);
		}
		Ok(())
	}

	/// 加载一个 bundle（幂等：按 bundle 路径去重）。每个早退分支都打
	/// 诊断日志——静默失败会让效果库毫无线索地缺插件。
	fn load_bundle(&self, bundle: &Path) {
		let Some(binary) = find_binary_in_bundle(bundle) else {
			eprintln!("[ofx] {}: no plugin binary in bundle", bundle.display());
			return;
		};
		let Some(handle) = dl_open(&binary) else {
			eprintln!("[ofx] {}: dlopen failed", binary.display());
			return;
		};
		{
			let bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
			if bins.iter().any(|b| b.bundle_path == bundle) {
				unsafe { dlclose(handle) };
				return;
			}
		}
		let plugins = unsafe { self.collect_plugins(handle, bundle) };
		if plugins.is_empty() {
			eprintln!(
				"[ofx] {}: no usable plugins (setHost/load/describe failed)",
				binary.display()
			);
			unsafe { dlclose(handle) };
			return;
		}
		let mut plugs = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
		let mut bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
		for p in plugins {
			// identifier 去重（后加载者忽略，HS 语义）。
			if !plugs.iter().any(|e| e.identifier == p.identifier) {
				plugs.push(p);
			}
		}
		bins.push(LoadedBinary {
			handle,
			bundle_path: bundle.to_path_buf(),
		});
	}

	/// 从已加载二进制收集插件（setHost → load → describe）。
	///
	/// # Safety
	/// `handle` 必须是有效 dlopen 句柄。
	unsafe fn collect_plugins(&self, handle: *mut c_void, bundle: &Path) -> Vec<Arc<Plugin>> {
		let get_no: unsafe extern "C" fn() -> c_int =
			match unsafe { dlsym_fn(handle, "OfxGetNumberOfPlugins") } {
				Some(f) => f,
				None => {
					return Vec::new();
				}
			};
		let get_plug: unsafe extern "C" fn(c_int) -> *mut OfxPlugin =
			match unsafe { dlsym_fn(handle, "OfxGetPlugin") } {
				Some(f) => f,
				None => {
					return Vec::new();
				}
			};
		let count = unsafe { get_no() };
		if count <= 0 {
			return Vec::new();
		}
		let mut out = Vec::new();
		for i in 0..count {
			let ofx = unsafe { get_plug(i) };
			if ofx.is_null() {
				continue;
			}
			if let Some(p) = unsafe { self.build_plugin(ofx, handle, bundle) } {
				out.push(p);
			}
		}
		out
	}

	/// 读 OfxPlugin 头部并跑 setHost/load/describe。
	///
	/// # Safety
	/// `ofx` 指向插件导出的有效 OfxPlugin 结构。
	unsafe fn build_plugin(
		&self,
		ofx: *mut OfxPlugin,
		lib: *mut c_void,
		bundle: &Path,
	) -> Option<Arc<Plugin>> {
		let ofx_ref = unsafe { &*ofx };
		// API 匹配（kOfxImageEffectPluginApi "OfxImageEffectPluginAPI" v1，
		// ofxImageEffect.h:28-32）。每个拒绝分支都打诊断日志（静默拒绝
		// 会让效果库毫无线索地缺插件）。
		let api = if ofx_ref.plugin_api.is_null() {
			eprintln!("[ofx] {}: null plugin_api", bundle.display());
			return None;
		} else {
			unsafe { CStr::from_ptr(ofx_ref.plugin_api) }
				.to_str()
				.ok()?
		};
		if api != "OfxImageEffectPluginAPI" || ofx_ref.api_version != 1 {
			eprintln!(
				"[ofx] {}: unsupported api {api} v{}",
				bundle.display(),
				ofx_ref.api_version
			);
			return None;
		}
		let identifier = unsafe { CStr::from_ptr(ofx_ref.plugin_identifier) }
			.to_str()
			.ok()?
			.to_string();
		let entry = ofx_ref.main_entry?;

		// setHost 是 mandatory 的第一个调用（ofxCore.h:124-132）。宿主
		// 结构体是进程级静态（ofxs 只存指针，见 global_ofx_host）。
		if let Some(f) = ofx_ref.set_host {
			unsafe { f(global_ofx_host()) };
		}

		let mut plugin = Plugin {
			identifier,
			version: (ofx_ref.plugin_version_major, ofx_ref.plugin_version_minor),
			bundle_path: bundle.to_path_buf(),
			contexts: Vec::new(),
			descriptor: EffectDescriptor::new(),
			lib,
			entry,
			ofx_plugin: ofx as *mut c_void,
			unloaded: std::sync::atomic::AtomicBool::new(false),
		};

		// 根描述符属性（HS: effectDescriptorStuff，ofxhImageEffect.cpp:133-155）。
		init_descriptor_props(&plugin.descriptor.props, bundle);

		let empty = PropertySet::new();

		// load（HS: ofxhImageEffectAPI.cpp:158-165；OK/ReplyDefault 接受）。
		let stat = unsafe { plugin.call_action(ACTION_LOAD, std::ptr::null_mut(), &empty, &empty) };
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			eprintln!("[ofx] {}: load action returned {stat}", plugin.identifier);
			return None;
		}

		// describe（handle = 打标描述符；HS: ofxhImageEffectAPI.cpp:173-180）。
		let desc_handle = crate::suites::tag::make(
			&plugin.descriptor.props as *const PropertySet,
			crate::suites::tag::DESCRIPTOR,
		);
		let stat = unsafe { plugin.call_action(ACTION_DESCRIBE, desc_handle, &empty, &empty) };
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			eprintln!("[ofx] {}: describe action returned {stat}", plugin.identifier);
			return None;
		}

		// 支持上下文（describe 产物；HS 从 props 读）。
		let contexts = read_contexts(&plugin.descriptor.props);
		// 只收标准上下文（Filter/Generator/Transition 之外还有
		// General——kOfxImageEffectContextGeneral 同样是规范上下文，
		// Roto/AppendClip/STMap 等插件只声明它）。
		let contexts: Vec<String> = contexts
			.into_iter()
			.filter(|c| {
				matches!(
					c.as_str(),
					"OfxImageEffectContextFilter"
						| "OfxImageEffectContextGenerator"
						| "OfxImageEffectContextTransition"
						| "OfxImageEffectContextGeneral"
				)
			})
			.collect();
		if contexts.is_empty() {
			eprintln!(
				"[ofx] {}: no standard contexts after describe",
				plugin.identifier
			);
			return None;
		}
		plugin.contexts = contexts;
		Some(Arc::new(plugin))
	}

	/// 已加载插件数量。
	pub fn count(&self) -> usize {
		self.plugins.lock().unwrap_or_else(|e| e.into_inner()).len()
	}

	/// 第 `index` 个插件（扫描顺序稳定）。
	pub fn at(&self, index: usize) -> Option<Arc<Plugin>> {
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(index)
			.cloned()
	}

	/// 按标识查找。
	pub fn find(&self, identifier: &str) -> Option<Arc<Plugin>> {
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.find(|p| p.identifier == identifier)
			.cloned()
	}

	/// 卸载全部（shutdown 路径）。`scanned_paths` 一并清空——
	/// 否则再 init 后的扫描会被去重短路（缓存已空但路径仍在）。
	/// 卸载前先给每个插件置 `unloaded`：实例可能跨代存活（测试的
	/// 串行 shutdown/重扫描），它们持有的旧代入口已随 dlclose 失效，
	/// 置位后 call_action 直接失败而非跳野指针。
	pub(crate) fn unload_all(&self) {
		{
			let plugins = self.plugins.lock().unwrap_or_else(|e| e.into_inner());
			for p in plugins.iter() {
				p.unloaded.store(true, std::sync::atomic::Ordering::Release);
			}
		}
		self.plugins
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
		let mut bins = self.binaries.lock().unwrap_or_else(|e| e.into_inner());
		bins.clear(); // Drop → dlclose
		self.scanned_paths
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clear();
	}
}

/// 递归枚举目录下的 bundle 目录（深度受限，防符号链接环）。
fn walk_dirs(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
	if depth > max_depth {
		return;
	}
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for e in entries.flatten() {
		let p = e.path();
		if !p.is_dir() {
			continue;
		}
		if is_bundle_dir(&p) {
			out.push(p);
		} else {
			walk_dirs(&p, depth + 1, max_depth, out);
		}
	}
}

/// 根描述符属性表（HS: effectDescriptorStuff 的子集，
/// ofxhImageEffect.cpp:133-155）。
fn init_descriptor_props(props: &PropertySet, bundle: &Path) {
	props.set_one(PROP_TYPE, Value::String(cs("OfxTypeImageEffect")));
	props.set_one(PROP_LABEL, Value::String(cs("")));
	props.set_one("OfxPropShortLabel", Value::String(cs("")));
	props.set_one("OfxPropLongLabel", Value::String(cs("")));
	props.define("OfxPropVersion", vec![Value::Int(0)]);
	props.set_one("OfxPropVersionLabel", Value::String(cs("")));
	props.set_one("OfxPropPluginDescription", Value::String(cs("")));
	props.define(PROP_SUPPORTED_CONTEXTS, vec![]);
	props.set_one("OfxImageEffectPluginPropGrouping", Value::String(cs("")));
	props.set_one("OfxImageEffectPluginPropSingleInstance", Value::Int(0));
	props.set_one(
		"OfxImageEffectPluginRenderThreadSafety",
		Value::String(cs("OfxImageEffectRenderInstanceSafe")),
	);
	props.set_one("OfxImageEffectPluginPropHostFrameThreading", Value::Int(1));
	props.set_one(
		"OfxImageEffectPluginPropOverlayInteractV1",
		Value::Pointer(std::ptr::null_mut()),
	);
	// overlay interact V2（ofxImageEffect.h:825）：插件 describe 期声明
	// 自定义交互入口（Pointer→OfxPluginEntryPoint；V2 要求 Draw suite
	// 绘制）；宿主预定义空指针默认（propSet 不创建属性，宿主预定义
	// 属性宇宙，与 V1 同款）。
	props.set_one(PROP_OVERLAY_INTERACT_V2, Value::Pointer(std::ptr::null_mut()));
	props.set_one("OfxImageEffectPropSupportsMultiResolution", Value::Int(1));
	props.set_one(PROP_SUPPORTS_TILES, Value::Int(1));
	props.set_one("OfxImageEffectPropTemporalClipAccess", Value::Int(0));
	props.define("OfxImageEffectPropSupportedPixelDepths", vec![]);
	props.set_one(
		"OfxImageEffectPluginPropFieldRenderTwiceAlways",
		Value::Int(1),
	);
	props.set_one(
		"OfxImageEffectPropSupportsMultipleClipDepths",
		Value::Int(0),
	);
	props.set_one("OfxImageEffectPropSupportsMultipleClipPARs", Value::Int(0));
	props.define("OfxImageEffectPropClipPreferencesSlaveParam", vec![]);
	props.set_one(PROP_SEQUENTIAL, Value::Int(0));
	props.set_one(
		PROP_PLUGIN_FILE_PATH,
		Value::String(cs(bundle.to_str().unwrap_or(""))),
	);
	// GL 能力声明（M11 §4；ofxGPURender.h）：描述符预定义
	// OpenGLRenderSupported（默认 "false"）与 kOfxOpenGLPropPixelDepth
	// （空数组）——插件在 describe 期经属性 suite 写入（HostSupport
	// 同款：propSet 不创建属性，宿主预定义属性宇宙）。
	props.set_one(PROP_GL_RENDER_SUPPORTED, Value::String(cs("false")));
	props.define(PROP_GL_PIXEL_DEPTH, vec![]);
	// ofxColour 描述符预定义（M11 §4；ofxColour.h）：style（默认
	// None）与可用配置（空数组），插件 describe 期声明自身能力。
	props.set_one(
		PROP_COLOUR_STYLE,
		Value::String(cs("OfxImageEffectColourManagementNone")),
	);
	props.define(PROP_COLOUR_AVAILABLE_CONFIGS, vec![]);
}

/// describe 后读支持上下文。
fn read_contexts(props: &PropertySet) -> Vec<String> {
	let mut out = Vec::new();
	let dim = props.dimension(PROP_SUPPORTED_CONTEXTS);
	for i in 0..dim {
		if let Some(Value::String(s)) = props.get(PROP_SUPPORTED_CONTEXTS, i) {
			out.push(s.to_string_lossy().into_owned());
		}
	}
	out
}

// ---- Host -----------------------------------------------------------------

/// 宿主。持有宿主级属性集（kOfxPropName/Version 与能力宣告属性）
/// 与插件缓存；是 suite 的注册与分发点。
pub struct Host {
	/// 宿主属性集（kOfxProp* 宿主级属性；能力宣告如
	/// kOfxImageEffectPropSupportedPixelDepths——协商的宿主侧输入）。
	pub props: PropertySet,
	/// 插件缓存。
	pub cache: PluginCache,
	/// 活跃实例注册表（泄漏断言与调试）。
	pub(crate) instances: Mutex<Vec<std::sync::Weak<RefBox<Instance>>>>,
}

impl Host {
	/// 进程单例。首次调用构建宿主属性集（能力宣告在此写入）。
	pub fn global() -> &'static Host {
		static HOST: OnceLock<Host> = OnceLock::new();
		HOST.get_or_init(|| {
			let props = PropertySet::new();
			init_host_props(&props);
			Host {
				props,
				cache: PluginCache {
					plugins: Mutex::new(Vec::new()),
					scanned_paths: Mutex::new(Vec::new()),
					binaries: Mutex::new(Vec::new()),
				},
				instances: Mutex::new(Vec::new()),
			}
		})
	}

	/// 上下文选择 + 实例化（filter 优先；实例化失败时按 general →
	/// tracker → paint → 其余支持上下文的顺序回退——TrackerPM 这类
	/// 插件 describe 支持 filter，但其 createInstance 在非
	/// paint/tracker/general 上下文抓不到 Mask clip 会炸
	/// BadHandle；Natron 上它天然以 tracker 上下文实例化）。
	/// 返回实际使用的上下文与实例；所有上下文都失败时返回最后一个
	/// 错误。
	pub fn create_instance_preferred(
		&self,
		identifier: &str,
	) -> crate::error::Result<(String, Arc<RefBox<Instance>>)> {
		let plugin = self.cache.find(identifier).ok_or(crate::error::Error::NotFound)?;
		let mut order: Vec<&str> = Vec::with_capacity(plugin.contexts.len());
		for pref in [
			"OfxImageEffectContextFilter",
			"OfxImageEffectContextGeneral",
			"OfxImageEffectContextTracker",
			"OfxImageEffectContextPaint",
		] {
			if plugin.contexts.iter().any(|c| c == pref) {
				order.push(pref);
			}
		}
		for c in &plugin.contexts {
			if !order.contains(&c.as_str()) {
				order.push(c);
			}
		}
		let mut last_err = crate::error::Error::Failed("插件无支持上下文".into());
		for ctx in order {
			match self.create_instance(identifier, Some(ctx)) {
				Ok(inst) => return Ok((ctx.to_string(), inst)),
				Err(e) => last_err = e,
			}
		}
		Err(last_err)
	}

	/// 按标识创建实例（describeInContext → createInstance）。
	/// 参照 HS: ofxhImageEffectAPI.cpp:200-238（describeInContext 的
	/// in-args 只有 kOfxImageEffectPropContext）与
	/// ofxhImageEffect.cpp:389-460（populate：clip/param 实例化）。
	/// `context` 为空用插件首个支持上下文。失败返回
	/// [`crate::error::Error::Failed`]/[`crate::error::Error::NotFound`]。
	pub fn create_instance(
		&self,
		identifier: &str,
		context: Option<&str>,
	) -> crate::error::Result<Arc<RefBox<Instance>>> {
		let plugin = self
			.cache
			.find(identifier)
			.ok_or(crate::error::Error::NotFound)?;

		let context = match context {
			Some(c) => c.to_string(),
			None => plugin
				.contexts
				.first()
				.cloned()
				.ok_or(crate::error::Error::Failed("插件无支持上下文".into()))?,
		};
		if !plugin.contexts.iter().any(|c| c == &context) {
			return Err(crate::error::Error::Failed(format!(
				"插件不支持上下文 {context}"
			)));
		}

		// describeInContext：上下文描述符 = 根描述符属性拷贝 +
		// 插件在 action 内重定义参数/clip。
		let mut ctx_desc = EffectDescriptor::new();
		ctx_desc.props = plugin.descriptor.props.clone();
		let in_args = PropertySet::new();
		in_args.set_one(PROP_CONTEXT, Value::String(cs(&context)));
		let desc_handle = crate::suites::tag::make(
			&ctx_desc.props as *const PropertySet,
			crate::suites::tag::DESCRIPTOR,
		);
		let stat = unsafe {
			plugin.call_action(ACTION_DESCRIBE_IN_CONTEXT, desc_handle, &in_args, &in_args)
		};
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			return Err(crate::error::Error::Failed(format!(
				"describeInContext 失败：{stat}"
			)));
		}

		// populate（HS: Instance::populate：clip 实例 + param 实例，
		// ofxhImageEffect.cpp:389-460）。
		let clips = ctx_desc
			.clips
			.iter()
			.map(|c| Box::new(crate::clip::ClipInstance::from_descriptor(c.as_ref())))
			.collect();
		let params = crate::param::ParamSetInstance {
			params: ctx_desc
				.params
				.iter()
				.map(|d| Box::new(crate::param::ParamInstance::from_def((**d).clone())))
				.collect(),
		};

		let instance = Instance {
			props: PropertySet::new(),
			plugin,
			context: context.clone(),
			params,
			clips,
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		init_instance_props(&instance.props, &instance);

		let arc = Arc::new(RefBox {
			refs: std::sync::atomic::AtomicU32::new(1),
			value: instance,
		});

		// createInstance action（HS: ofxhImageEffect.cpp:675-688）。
		let inst_handle = crate::suites::tag::make(
			&arc.value.props as *const PropertySet,
			crate::suites::tag::INSTANCE,
		);
		let empty = PropertySet::new();
		let stat = unsafe {
			arc.value
				.plugin
				.call_action(ACTION_CREATE_INSTANCE, inst_handle, &empty, &empty)
		};
		if stat != status::OK && stat != status::REPLY_DEFAULT {
			// 插件拒绝了 createInstance——它从没认领这个实例；把
			// destroyed 门置位，让随后的 drop 跳过 destroyInstance 通知
			// （否则插件对一个它没创建的实例回 BadIndex 之类的错误）。
			arc.value
				.destroyed
				.store(true, std::sync::atomic::Ordering::Relaxed);
			return Err(crate::error::Error::Failed(format!(
				"createInstance 失败：{stat}"
			)));
		}

		// 登记活跃实例表（泄漏断言用）+ param→instance 回写表。
		self.instances
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push(Arc::downgrade(&arc));
		let inst_props = &arc.value.props as *const PropertySet as usize;
		for p in &arc.value.params.params {
			let p_addr = &p.props as *const PropertySet as usize;
			crate::suites::param::register_param_owner(p_addr, inst_props);
		}

		Ok(arc)
	}

	/// 关闭宿主（`oakplugin_host_shutdown` 的宿主侧）：
	/// 1. 对全部活实例发 destroyInstance 通知（此刻插件仍加载）；
	/// 2. 打取消标记——shutdown 后仍被 C 侧持有的句柄不得再调
	///    已卸载的插件入口（render 入口即短路）；
	/// 3. 卸载插件与二进制。
	///
	/// 说明：C 侧句柄持有 Arc，宿主无法（也不应）强制析构——实例
	/// 内存随句柄释放；destroyed 标记保证 Drop 不再碰插件入口。
	pub fn shutdown(&self) {
		let list = self.instances.lock().unwrap_or_else(|e| e.into_inner());
		for w in list.iter() {
			if let Some(arc) = w.upgrade() {
				// 置 destroyed 标记后再通知：实例随后的 Drop 不得再
				// 调已卸载的插件入口（二次 destroyInstance → 悬垂）。
				arc.value
					.destroyed
					.store(true, std::sync::atomic::Ordering::Relaxed);
				arc.value.notify_destroy();
				arc.value
					.cancel
					.store(true, std::sync::atomic::Ordering::Relaxed);
			}
		}
		self.cache.unload_all();
	}

	/// 当前活跃实例数（泄漏断言）。
	pub fn alive_count(&self) -> usize {
		let mut list = self.instances.lock().unwrap_or_else(|e| e.into_inner());
		let alive = list.iter().filter(|w| w.strong_count() > 0).count();
		list.retain(|w| w.strong_count() > 0);
		alive
	}
}

/// 宿主属性集（对照 C++ OliveHost::OliveHost，olivehost.cpp:193-207：
/// Name/Label/Version；能力宣告为 phase 1 最小集，协商按需扩展）。
fn init_host_props(props: &PropertySet) {
	// ofxCore.h 宿主属性集的必备项：OfxType=OfxTypeHost 与
	// OfxPropAPIVersion（int[2]，宿主实现的 API 版本）——ofxs 支持库的
	// loadAction 以 throwOnFailure=true 读它们，缺失会被映射成
	// kOfxStatErrMissingHostFeature 让插件加载直接失败。
	props.set_one("OfxPropType", Value::String(cs("OfxTypeHost")));
	props.define("OfxPropAPIVersion", vec![Value::Int(1), Value::Int(4)]);
	props.set_one("OfxPropName", Value::String(cs("Oak Video Editor")));
	props.set_one("OfxPropLabel", Value::String(cs("Oak Video Editor")));
	props.set_one(
		"OfxPropVersionLabel",
		Value::String(cs(env!("CARGO_PKG_VERSION"))),
	);
	props.define(
		"OfxPropVersion",
		vec![Value::Int(0), Value::Int(0), Value::Int(0)],
	);
	// 能力宣告：协商的宿主侧输入（phase 1 全链路 F32+RGBA）。
	props.define(
		"OfxImageEffectPropSupportedPixelDepths",
		vec![Value::String(cs("OfxBitDepthFloat"))],
	);
	// 宿主支持的上下文集（ofxs 以 throwOnFailure=true 读）。
	props.define(
		"OfxImageEffectPropSupportedContexts",
		vec![
			Value::String(cs("OfxImageEffectContextFilter")),
			Value::String(cs("OfxImageEffectContextGenerator")),
			Value::String(cs("OfxImageEffectContextTransition")),
			Value::String(cs("OfxImageEffectContextGeneral")),
		],
	);
	props.define(
		"OfxImageEffectPropSupportedComponents",
		vec![
			Value::String(cs("OfxImageComponentRGBA")),
			Value::String(cs("OfxImageComponentRGB")),
			Value::String(cs("OfxImageComponentAlpha")),
		],
	);
	// ofxs 支持库 fetchHostDescription 以 throwOnFailure=true 读取的
	// 全部宿主能力位（IsBackground 缺一个都会让读取链中断——
	// gHostDescriptionHasInit 已置位，后续插件拿到半初始化描述，
	// temporalClipAccess 为 0 → 时序类插件集体拒载）。
	props.set_one("OfxImageEffectHostPropIsBackground", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsStringAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsChoiceAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsBooleanAnimation", Value::Int(0));
	props.set_one("OfxParamHostPropSupportsCustomAnimation", Value::Int(0));
	// 自定义 interact（检视器叠加层）宿主支持。
	props.set_one("OfxParamHostPropSupportsCustomInteract", Value::Int(1));
	// 能力宣告（ofxImageEffect.h 宿主属性集；ofxs 支持库在 load 时读成
	// ImageEffectHostDescription，temporalClipAccess 等为 0 时整类插件
	// ——Retime/FrameHold/TimeOffset/SlitScan——直接在 load 里拒载）。
	props.set_one("OfxImageEffectPropTemporalClipAccess", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsMultiResolution", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsTiles", Value::Int(1));
	props.set_one("OfxImageEffectPropSupportsMultipleClipPARs", Value::Int(1));
	// 像素深度只支持 Float（phase 1 全链路 F32），不做多深度协商。
	props.set_one("OfxImageEffectPropSupportsMultipleClipDepths", Value::Int(0));
	// 旧版 ofxs 的 fetchHostDescription 读的是不带 Supports 的同义名
	// （ofxImageEffect.h 的 kOfxImageEffectPropMultipleClipDepths）。
	props.set_one("OfxImageEffectPropMultipleClipDepths", Value::Int(0));
	props.set_one("OfxImageEffectPropSetableFrameRate", Value::Int(0));
	props.set_one("OfxImageEffectPropSetableFielding", Value::Int(0));
	// fetchHostDescription 的其余读取项（部分 ofxs 版本以
	// throwOnFailure=true 读，缺一个首插件的 load 就炸）：
	// 顺序渲染状态 0=宿主不强制；Draft 渲染质量支持（播放降档）；
	// 参数数无上限；不支持参数化曲线动画；macOS 无 OS 窗口句柄；
	// 原生坐标原点按 OFX 规范默认 BottomLeft。
	props.set_one("OfxImageEffectInstancePropSequentialRender", Value::Int(0));
	props.set_one("OfxImageEffectPropRenderQualityDraft", Value::Int(1));
	props.set_one("OfxParamHostPropMaxParameters", Value::Int(-1));
	// 参数页数无上限；页内行列 0,0 = 自动排布（这两个也是
	// throwOnFailure=true 的读取项）。
	props.set_one("OfxParamHostPropMaxPages", Value::Int(-1));
	props.define(
		"OfxParamHostPropPageRowColumnCount",
		vec![Value::Int(0), Value::Int(0)],
	);
	props.set_one("OfxParamHostPropSupportsParametricAnimation", Value::Int(0));
	props.set_one("OfxPropHostOSHandle", Value::Pointer(std::ptr::null_mut()));
	props.set_one(
		"OfxImageEffectHostPropNativeOrigin",
		Value::String(cs("OfxHostNativeOriginBottomLeft")),
	);
	// 序列渲染：插件可随意选（0 = 宿主不强制）。
	props.set_one("OfxImageEffectPropSequentialRenderStatus", Value::Int(0));
	// 逐帧线程化程度：全帧线程安全（kOfxImageEffectRenderUnsafe 之外的
	// 最强档——渲染在独立 worker 进程内串行驱动，无共享状态）。
	props.set_one("OfxImageEffectPropRenderThreadSafety", Value::String(cs("OfxImageEffectRenderFullySafe")));
	// GL 能力宣告（M11 §4；ofxGPURender.h "OpenGL House Keeping"：
	// 宿主在描述符置 "true"）。
	props.set_one(PROP_GL_RENDER_SUPPORTED, Value::String(cs("true")));
	// overlay 能力宣告（ofxImageEffect.h:801）：宿主允许插件在输出
	// 图像上绘制 overlay（interact draw 的前置）。
	props.set_one(PROP_SUPPORTS_OVERLAYS, Value::Int(1));
	// ofxColour 能力宣告（M11 §4）：OCIO 模式 + native 配置列表。
	props.set_one(PROP_COLOUR_STYLE, Value::String(cs(COLOUR_STYLE_OCIO)));
	props.define(
		PROP_COLOUR_AVAILABLE_CONFIGS,
		vec![Value::String(cs(NATIVE_CONFIG_ID))],
	);
}

/// 实例属性表（HS: effectInstanceStuff 的 phase 1 子集，
/// ofxhImageEffect.cpp:313-330）。
fn init_instance_props(props: &PropertySet, instance: &Instance) {
	props.set_one(PROP_TYPE, Value::String(cs("OfxTypeImageEffectInstance")));
	props.set_one(PROP_CONTEXT, Value::String(cs(&instance.context)));
	props.set_one(
		PROP_PLUGIN_HANDLE,
		Value::Pointer(instance.plugin.ofx_plugin),
	);
	props.define(
		PROP_PROJECT_SIZE,
		vec![Value::Double(1920.0), Value::Double(1080.0)],
	);
	props.define(
		PROP_PROJECT_OFFSET,
		vec![Value::Double(0.0), Value::Double(0.0)],
	);
	props.define(
		PROP_PROJECT_EXTENT,
		vec![Value::Double(1920.0), Value::Double(1080.0)],
	);
	props.set_one(PROP_PROJECT_PAR, Value::Double(1.0));
	props.set_one(PROP_EFFECT_DURATION, Value::Double(1.0));
	props.set_one(PROP_SEQUENTIAL, Value::Int(0));
	props.set_one(PROP_FRAME_RATE, Value::Double(24.0));
	props.set_one("OfxPropIsInteractive", Value::Int(0));
	props.set_one(PROP_SUPPORTS_TILES, Value::Int(1));
	// ofxColour 实例期协商结果（M11 §4；ofxColour.h：宿主必须在实例
	// 上设置 style/config/OCIOConfig）。
	props.set_one(PROP_COLOUR_STYLE, Value::String(cs(COLOUR_STYLE_OCIO)));
	props.set_one(PROP_COLOUR_CONFIG, Value::String(cs(NATIVE_CONFIG_ID)));
	props.set_one(PROP_OCIO_CONFIG, Value::String(cs(OCIO_CONFIG_URI)));
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The default path table includes every home- and app-relative location
	/// plus the platform-gated OFX standard locations.
	#[test]
	fn default_paths_cover_home_and_standard_locations() {
		let home = Path::new("/home/octa");
		let paths = default_plugin_paths(Some(home));
		// Olive legacy + home locations.
		assert!(paths.contains(&home.join(".OFX/Plugins")));
		assert!(paths.contains(&home.join(".local/share/OFX/Plugins")));
		assert!(paths.contains(&home.join(".local/share/olive/ofx/Plugins")));
		// App-relative (C++ parity).
		assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
		assert!(paths.contains(&PathBuf::from("../share/olive/ofx/Plugins")));
		assert!(paths.contains(&PathBuf::from("../lib/olive/ofx/Plugins")));
		// OFX standard locations, gated by platform.
		#[cfg(target_os = "macos")]
		{
			assert!(paths.contains(&home.join("Library/OFX/Plugins")));
			assert!(paths.contains(&PathBuf::from("/Library/OFX/Plugins")));
		}
		#[cfg(target_os = "linux")]
		{
			assert!(paths.contains(&PathBuf::from("/usr/OFX/Plugins")));
			assert!(paths.contains(&PathBuf::from("/usr/local/OFX/Plugins")));
		}
		#[cfg(target_os = "windows")]
		{
			assert!(paths
				.iter()
				.any(|p| p.ends_with("Common Files/OFX/Plugins") || p.ends_with("Common Files\\OFX\\Plugins")));
		}
	}

	/// With no `$HOME` the home-based entries are skipped; the rest remain.
	#[test]
	fn default_paths_skip_home_entries_when_home_unset() {
		let paths = default_plugin_paths(None);
		assert!(!paths.iter().any(|p| p.to_string_lossy().starts_with("/home/")));
		assert!(paths.contains(&PathBuf::from("../OFX/Plugins")));
		assert_eq!(
			paths
				.iter()
				.filter(|p| p.to_string_lossy().contains(".OFX"))
				.count(),
			0
		);
	}

	/// The dedupe record rejects an already-seen path and keeps first-seen
	/// order.
	#[test]
	fn record_scanned_path_dedupes() {
		let mut seen: Vec<PathBuf> = Vec::new();
		assert!(record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
		assert!(record_scanned_path(&mut seen, PathBuf::from("/b/Plugins")));
		// A duplicate is rejected without being recorded again.
		assert!(!record_scanned_path(&mut seen, PathBuf::from("/a/Plugins")));
		assert_eq!(seen, vec![PathBuf::from("/a/Plugins"), PathBuf::from("/b/Plugins")]);
	}

	/// Scanning an existing directory twice only records it once, and a
	/// non-existent directory is skipped without error.
	#[test]
	fn scan_path_skips_missing_and_dedupes_existing() {
		let dir = std::env::temp_dir().join(format!("oakplugin-scan-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let cache = PluginCache {
			plugins: Mutex::new(Vec::new()),
			scanned_paths: Mutex::new(Vec::new()),
			binaries: Mutex::new(Vec::new()),
		};
		cache.scan_path(&dir).expect("existing dir scans");
		cache.scan_path(&dir).expect("duplicate scan is a no-op");
		{
			let seen = cache.scanned_paths.lock().unwrap_or_else(|e| e.into_inner());
			assert_eq!(seen.len(), 1, "the same directory is scanned only once");
		} // drop the guard before scanning again

		// A missing directory is skipped (no error, no plugin scan).
		let missing = dir.join("does-not-exist");
		cache.scan_path(&missing).expect("missing dir scan is a no-op");
		let plugins = cache.plugins.lock().unwrap_or_else(|e| e.into_inner());
		assert!(plugins.is_empty(), "no bundles were loaded");

		let _ = std::fs::remove_dir_all(&dir);
	}
}
