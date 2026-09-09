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

//! OfxMessageSuite v1/v2：插件消息 → 宿主日志。
//!
//! stable Rust 无法定义 C-variadic 函数（c_variadic 仍不稳定），
//! v1 的 `...` 与 v2 的 va_list 入口由 C shim 承载
//! （cbits/ofx_message_shim.c，build.rs 经 `cc` 编译，符号随
//! staticlib 进入 liboakplugin）：C 侧 vsnprintf 成定长缓冲后转发
//! 到本模块的 `oak_ofx_message_impl`。
//!
//! 消息出口 = facade 注册的 `oakplugin_message_fn`
//! （include/plugin/host.h）。本模块按头文件契约建模。
//! （原 C ABI 出口层已随单库化删除；注册点由 facade 直接调用。）

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};

use crate::suites::status;

/// facade 消息回调（include/plugin/host.h `oakplugin_message_fn`）：
/// `(type, message, userdata) -> OAKPLUGIN_MESSAGE_ANSWER_NO/YES`。
pub(crate) type MessageHandler =
	unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void) -> c_int;

/// 消息出口注册表（facade 写入，suite 读；userdata 以 usize 存，
/// 避免裸指针破坏 static 的 Send/Sync 推导）。
static HANDLER: std::sync::Mutex<(Option<MessageHandler>, usize)> =
	std::sync::Mutex::new((None, 0));

// ---------------------------------------------------------------------------
// 持久消息计数（检查器效果卡的徽标数据源，阶段 6b）
// ---------------------------------------------------------------------------
//
// C++ 侧 `OlivePluginInstance::persistentErrors_` 按实例累计持久消息，
// 并 emit `node_->message_count_changed()`。这里以实例 handle（props
// 地址，见 [`crate::suites::tag`]）为键累计计数；app 检查器经
// [`crate::node_factory::instance_from_id`] 拿实例后按 props 地址查询。

static PERSISTENT: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, usize>>> =
	std::sync::OnceLock::new();

fn persistent_slot() -> &'static std::sync::Mutex<HashMap<usize, usize>> {
	PERSISTENT.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 实例 handle（props 地址）的持久消息数（未登记 → 0）。
pub fn persistent_message_count(handle: usize) -> usize {
	persistent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&handle)
		.copied()
		.unwrap_or(0)
}

/// 清空某实例的持久消息（实例释放/宿主关闭路径；未登记的键 no-op）。
pub fn clear_persistent_messages(handle: usize) {
	persistent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&handle);
}

/// 注册/注销消息出口（facade 调用；公开：测试直接注入捕获器）。
pub fn set_handler(f: Option<MessageHandler>, userdata: *mut c_void) {
	let mut h = HANDLER.lock().unwrap_or_else(|e| e.into_inner());
	*h = (f, userdata as usize);
}

/// 读取当前出口。
fn handler() -> (Option<MessageHandler>, *mut c_void) {
	let h = HANDLER.lock().unwrap_or_else(|e| e.into_inner());
	(h.0, h.1 as *mut c_void)
}

/// kOfxMessageQuestion（ofxMessage.h:61）：无出口时答"否"。
const K_MESSAGE_QUESTION: &[u8] = b"OfxMessageQuestion";

/// 消息类型是否为 question。
fn is_question(type_: *const c_char) -> bool {
	// 空指针已在调用点拦截；此处仍防御（CStr::from_ptr 的调用方义务）。
	if type_.is_null() {
		return false;
	}
	unsafe { CStr::from_ptr(type_) }.to_bytes() == K_MESSAGE_QUESTION
}

/// Rust 侧实现（C shim 转发至此；`#[no_mangle]` 保证符号名与 C 侧
/// 引用一致）。语义逐条对照 C++：
/// - type/message 为空 → kOfxStatFailed（olivehost.cpp:267，vmessage
///   对 !type || !format 的处理；format 为空的 case 由 shim 以
///   NULL message 转发）；
/// - 有出口 → 转发 `(type, message, userdata)`，返回按头文件契约
///   映射为 REPLY_YES/REPLY_NO（olivehost.cpp:277-279 的 handler
///   路径；非 question 消息插件忽略返回值）；
/// - 无出口 → stderr 日志（镜像 olivehost.cpp:282 的 fprintf 默认），
///   question 答"否"（olivehost.cpp:283-284）。
///
/// `# Safety`：`type`/`message` 必须是有效 C 字符串（插件契约，
/// shim 已保证 message 非悬垂——指向其栈缓冲，调用期间有效）。
#[no_mangle]
pub unsafe extern "C" fn oak_ofx_message_impl(
	_handle: *mut c_void,
	type_: *const c_char,
	_id: *const c_char,
	message: *const c_char,
) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if type_.is_null() || message.is_null() {
			return status::FAILED;
		}
		// 持久消息：按实例 handle（剥标签后的 props 地址）累计计数
		// （徽标数据源；C++ persistentErrors_ 的等价物）。
		if !_handle.is_null() {
			let key = crate::suites::tag::strip(_handle) as usize;
			let mut map = persistent_slot().lock().unwrap_or_else(|e| e.into_inner());
			*map.entry(key).or_insert(0) += 1;
			drop(map);
		}
		let (handler, userdata) = handler();
		if let Some(h) = handler {
			// 头文件契约：返回值 0/1 即答复 NO/YES。
			let answer = unsafe { h(type_, message, userdata) };
			return if answer != 0 {
				status::REPLY_YES
			} else {
				status::REPLY_NO
			};
		}
		// Headless 默认：stderr（C++ fprintf 的 Rust 等价物）。
		eprintln!(
			"OFX message: {}",
			unsafe { CStr::from_ptr(message) }.to_string_lossy()
		);
		if is_question(type_) {
			status::REPLY_NO
		} else {
			status::OK
		}
	}))
	.unwrap_or(status::FAILED)
}

/// 函数表布局（OfxMessageSuiteV1）。
#[repr(C)]
pub struct MessageSuiteV1 {
	/// message（变长参数版；入口在 C shim）
	pub message: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*const c_char,
		*const c_char,
		...
	) -> c_int,
}

/// 函数表布局（OfxMessageSuiteV2：第 5 参为平台相关 `va_list`）。
///
/// 真实 ABI 是 C 的 va_list（Apple arm64 上是结构体按值传递）；
/// 声明为不透明指针仅作占位——本表只被插件→C shim 调用，Rust 侧
/// 永不直接调用，指针类型不参与任何实际调用。
#[repr(C)]
pub struct MessageSuiteV2 {
	/// message（va_list 版；入口在 C shim）
	pub message: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*const c_char,
		*const c_char,
		*mut c_void,
	) -> c_int,
}

extern "C" {
	/// C shim 的 v1 入口（cbits/ofx_message_shim.c）。
	fn ofx_message_shim_v1(
		handle: *mut c_void,
		type_: *const c_char,
		id: *const c_char,
		format: *const c_char,
		...
	) -> c_int;
	/// C shim 的 v2 入口（va_list 已由 C 侧消费，Rust 只见不透明值）。
	fn ofx_message_shim_v2(
		handle: *mut c_void,
		type_: *const c_char,
		id: *const c_char,
		format: *const c_char,
		args: *mut c_void,
	) -> c_int;
}

/// 静态函数表实例（v1）。
pub fn suite_v1() -> &'static MessageSuiteV1 {
	static SUITE: std::sync::OnceLock<MessageSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MessageSuiteV1 {
		message: ofx_message_shim_v1,
	})
}

/// 静态函数表实例（v2）。
pub fn suite_v2() -> &'static MessageSuiteV2 {
	static SUITE: std::sync::OnceLock<MessageSuiteV2> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MessageSuiteV2 {
		message: ofx_message_shim_v2,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;

	/// 测试共享全局 HANDLER，cargo 默认并行执行——串行化契约用例。
	static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	/// 捕获型出口：把 (type, message) 记进 userdata 指向的 Vec。
	unsafe extern "C" fn capture(
		type_: *const c_char,
		message: *const c_char,
		userdata: *mut c_void,
	) -> c_int {
		let v = unsafe { &mut *(userdata as *mut Vec<(String, String)>) };
		v.push((
			unsafe { CStr::from_ptr(type_) }
				.to_string_lossy()
				.into_owned(),
			unsafe { CStr::from_ptr(message) }
				.to_string_lossy()
				.into_owned(),
		));
		1
	}

	/// v1 入口：`...` 在 C shim 侧 vsnprintf 后转发，格式化正确。
	#[test]
	fn shim_v1_formats_and_forwards() {
		let _g = TEST_LOCK.lock().unwrap();
		let mut captured: Vec<(String, String)> = Vec::new();
		set_handler(Some(capture), &mut captured as *mut _ as *mut c_void);

		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("test-id").unwrap();
		let fmt = CString::new("value=%d name=%s").unwrap();
		let name = CString::new("oak").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				42,
				name.as_ptr(),
			)
		};

		// 出口答 YES → REPLY_YES。
		assert_eq!(r, status::REPLY_YES);
		assert_eq!(captured.len(), 1);
		assert_eq!(captured[0].0, "OfxMessageError");
		assert_eq!(captured[0].1, "value=42 name=oak");

		set_handler(None, std::ptr::null_mut());
	}

	/// v1 入口：NULL format → kOfxStatFailed（C++ olivehost.cpp:267）。
	#[test]
	fn shim_v1_null_format_fails() {
		let _g = TEST_LOCK.lock().unwrap();
		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				std::ptr::null(),
			)
		};
		assert_eq!(r, status::FAILED);
	}

	/// v2 入口：表字段非空且指向 C shim 符号（va_list 路径无法从
	/// Rust 构造 va_list 调用，链的其余部分与 v1 共用 forward）。
	#[test]
	fn shim_v2_table_entry_resolves() {
		let _g = TEST_LOCK.lock().unwrap();
		let s = suite_v2();
		assert!(std::ptr::eq(
			s.message as *const (),
			ofx_message_shim_v2 as *const ()
		));
	}

	/// 无出口（headless 默认）：非 question → OK；question → REPLY_NO。
	#[test]
	fn no_handler_headless_defaults() {
		let _g = TEST_LOCK.lock().unwrap();
		set_handler(None, std::ptr::null_mut());
		let s = suite_v1();
		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let fmt = CString::new("hello %s").unwrap();
		let arg = CString::new("x").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				arg.as_ptr(),
			)
		};
		assert_eq!(r, status::OK);

		let q = CString::new("OfxMessageQuestion").unwrap();
		let r = unsafe {
			(s.message)(
				std::ptr::null_mut(),
				q.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				arg.as_ptr(),
			)
		};
		assert_eq!(r, status::REPLY_NO);
	}

	/// 持久消息按实例 handle 累计（徽标数据源）；clear 摘除。
	#[test]
	fn persistent_messages_accumulate_per_handle() {
		let _g = TEST_LOCK.lock().unwrap();
		set_handler(None, std::ptr::null_mut());
		let props = crate::property::PropertySet::new();
		let handle = crate::suites::tag::make(&props, crate::suites::tag::INSTANCE);
		let key = crate::suites::tag::strip(handle) as usize;
		clear_persistent_messages(key);

		let type_ = CString::new("OfxMessageError").unwrap();
		let id = CString::new("id").unwrap();
		let msg = CString::new("boom").unwrap();
		unsafe {
			oak_ofx_message_impl(handle, type_.as_ptr(), id.as_ptr(), msg.as_ptr());
			oak_ofx_message_impl(handle, type_.as_ptr(), id.as_ptr(), msg.as_ptr());
		}
		assert_eq!(persistent_message_count(key), 2);
		clear_persistent_messages(key);
		assert_eq!(persistent_message_count(key), 0);
		// 空 handle 不计数。
		unsafe {
			oak_ofx_message_impl(
				std::ptr::null_mut(),
				type_.as_ptr(),
				id.as_ptr(),
				msg.as_ptr(),
			);
		}
		assert_eq!(persistent_message_count(key), 0);
	}
}
