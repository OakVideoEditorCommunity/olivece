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

//! OfxMemorySuite v1：宿主代分配内存。
//!
//! 语义参照 HS: ofxhHost.cpp:26-42（裸 malloc/free 语义：失败 →
//! kOfxStatErrMemory，free 恒 OK）——但本 crate 按骨架声明增加
//! **宿主级账本**：分配记录进账本，destroyInstance 时兜底回收
//! （泄漏防线；插件正常应自行 free）。未知指针 free →
//! kOfxStatErrBadHandle（SDK ofxMemory.h:51 的契约）。
//! image_effect suite 的 imageMemory* 与此共用账本。

use std::alloc::Layout;
use std::sync::Mutex;

use crate::suites::status;

/// 对齐：malloc 保证 max_align_t（本平台 16 字节）；插件会把缓冲当
/// float*/SIMD 用，16 字节是安全下限。
const ALIGN: usize = 16;

/// 一个已分配块：地址 + 布局（回收时按记录布局 dealloc）。
/// 地址以 `usize` 存（不透明令牌，避免裸指针破坏 `static` 的
/// Send/Sync 推导；账本只做 alloc/dealloc 配对，从不解引用）。
struct Block {
	ptr: usize,
	layout: Layout,
}

/// 宿主级账本（进程单例；memory suite 无宿主指针参数，全局即可）。
static LEDGER: Mutex<Vec<Block>> = Mutex::new(Vec::new());

/// 取锁（毒锁接管：一次持锁 panic 不级联）。
fn lock() -> std::sync::MutexGuard<'static, Vec<Block>> {
	LEDGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// 分配并记账。返回裸指针；失败（OOM/布局非法）返回 `None`。
pub(crate) fn alloc(size: usize) -> Option<*mut u8> {
	// 零尺寸分配：Layout 要求非零，取 1 字节兜底（free 按同布局回收）。
	let layout = Layout::from_size_align(size.max(1), ALIGN).ok()?;
	let ptr = unsafe { std::alloc::alloc(layout) };
	if ptr.is_null() {
		return None;
	}
	lock().push(Block {
		ptr: ptr as usize,
		layout,
	});
	Some(ptr)
}

/// 按指针释放并销账。未知指针返回 `false`（suite 层映射
/// kOfxStatErrBadHandle）；`NULL` 按 C 语义 no-op 返回 `true`。
pub(crate) fn free(ptr: *mut u8) -> bool {
	if ptr.is_null() {
		return true;
	}
	let mut ledger = lock();
	let pos = ledger.iter().position(|b| b.ptr == ptr as usize);
	match pos {
		Some(i) => {
			let b = ledger.remove(i);
			unsafe { std::alloc::dealloc(b.ptr as *mut u8, b.layout) };
			true
		}
		None => false,
	}
}

/// 函数表布局（与 SDK `OfxMemorySuiteV1` 一致；`size_t` 在本平台
/// 与 `usize` 同宽，stable Rust 用 `usize` 表达——骨架的 `c_size_t`
/// 是不稳定特性，弃用）。
#[repr(C)]
pub struct MemorySuiteV1 {
	/// memoryAlloc：分配 `size` 字节，写 `*out`。
	pub alloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> c_int,
	/// memoryFree：释放；NULL 是 no-op。
	pub free: unsafe extern "C" fn(*mut c_void) -> c_int,
}

use std::ffi::{c_int, c_void};

/// memoryAlloc 实现：`handle` 忽略（HS 同，ofxhHost.cpp:27）。
///
/// `# Safety`：`out` 必须是可写指针（插件契约）。
unsafe extern "C" fn memory_alloc(
	_handle: *mut c_void,
	byte_size: usize,
	out: *mut *mut c_void,
) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		match alloc(byte_size) {
			Some(ptr) => {
				unsafe { *out = ptr as *mut c_void };
				status::OK
			}
			// 与 HS 一致：分配失败 → kOfxStatErrMemory（ofxhHost.cpp:35）。
			None => status::ERR_MEMORY,
		}
	}))
	.unwrap_or(status::FAILED)
}

/// memoryFree 实现：未知指针 → kOfxStatErrBadHandle（SDK 契约）；
/// NULL 按 C 语义 no-op → OK（HS 的 free(NULL) 行为，ofxhHost.cpp:40）。
unsafe extern "C" fn memory_free(data: *mut c_void) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if free(data as *mut u8) {
			status::OK
		} else {
			status::ERR_BAD_HANDLE
		}
	}))
	.unwrap_or(status::FAILED)
}

/// 静态函数表实例。
pub fn suite_v1() -> &'static MemorySuiteV1 {
	static SUITE: std::sync::OnceLock<MemorySuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MemorySuiteV1 {
		alloc: memory_alloc,
		free: memory_free,
	})
}
