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

//! OfxMultiThreadSuite v1：插件自起线程的登记与索引分配。
//!
//! 纪律：插件线程可在本 suite 存活期回调任意 suite——所有共享状态
//! 必须 Mutex（README §3）。线程经 `std::thread::scope` 启动并同步
//! join（OFX 语义：multiThread 返回时全部线程已完成；
//! HS: ofxhImageEffect.cpp gMultiThreadSuite），故无需常驻线程表。
//! 线程索引经 TLS 分配：宿主线程 [`index`] 为 -1、
//! [`is_spawned`] 为 0。
//!
//! 参照：HS: ofxhImageEffect.cpp gMultiThreadSuite。

use std::collections::HashMap;
use std::ffi::{c_int, c_uint, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::suites::status;

thread_local! {
	/// 本线程的 OFX 线程索引（宿主线程 = None）。
	static THREAD_INDEX: std::cell::Cell<Option<c_uint>> = const { std::cell::Cell::new(None) };
}

/// 函数表布局（OfxMultiThreadSuiteV1）。
#[repr(C)]
pub struct MultiThreadSuiteV1 {
	/// multiThread：启动 `n_threads` 个线程跑 `func`，线程索引
	/// 0..n_threads-1 经 `thread_arg` 传入。
	pub multi_thread: unsafe extern "C" fn(
		func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
		n_threads: c_uint,
		thread_arg: *mut c_void,
	) -> c_int,
	/// multiThreadNumCPUs
	pub num_cpus: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// multiThreadIndex：当前线程的 OFX 线程索引（宿主线程为 -1）。
	pub index: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// multiThreadIsSpawnedThread：当前线程是否插件线程。
	pub is_spawned: unsafe extern "C" fn(*mut c_int) -> c_int,
	/// mutexCreate：`count` 是初始可用计数（>1 时是信号量语义）。
	pub mutex_create: unsafe extern "C" fn(*mut *mut c_void, c_int) -> c_int,
	/// mutexDestroy
	pub mutex_destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexLock
	pub mutex_lock: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexUnLock
	pub mutex_unlock: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// mutexTryLock
	pub mutex_try_lock: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// 公共入口模板：panic 兜底。
fn caught(f: impl FnOnce() -> c_int) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(status::FAILED)
}

/// multiThread 实现：同步起 n 个线程（0..n-1），join 后返回。
/// 线程体只做两件事：登记 TLS 索引、调 `func`（C 代码，不 panic）。
unsafe extern "C" fn multi_thread(
	func: unsafe extern "C" fn(c_uint, c_uint, *mut c_void),
	n_threads: c_uint,
	thread_arg: *mut c_void,
) -> c_int {
	caught(|| {
		if std::env::var_os("OAK_OFX_TRACE").is_some() {
			eprintln!("[ofx] multiThread: n_threads={n_threads} func={:#x} arg={thread_arg:p}", func as usize);
		}
		// 裸指针不可 Send：以 usize 搬运，线程内还原（C 侧本即
		// 整数传递语义）。
		let arg = thread_arg as usize;
		let _ = std::thread::scope(|scope| {
			for i in 0..n_threads {
				scope.spawn(move || {
					THREAD_INDEX.with(|t| t.set(Some(i)));
					// func 是插件代码：按 OFX 契约不 panic；Rust 侧
					// 只做 TLS 登记，无 panic 源。
					unsafe { (func)(i, n_threads, arg as *mut c_void) };
					THREAD_INDEX.with(|t| t.set(None));
				});
			}
		});
		status::OK
	})
}

unsafe extern "C" fn multi_thread_num_cpus(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		// 物理核数（HS 同：系统 CPU 数）。
		let n = std::thread::available_parallelism()
			.map(|n| n.get() as c_int)
			.unwrap_or(1);
		unsafe { *out = n };
		status::OK
	})
}

unsafe extern "C" fn multi_thread_index(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		// 宿主线程 → -1（HS 约定）。
		unsafe { *out = THREAD_INDEX.with(|t| t.get()).map_or(-1, |i| i as c_int) };
		status::OK
	})
}

unsafe extern "C" fn multi_thread_is_spawned(out: *mut c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		unsafe { *out = THREAD_INDEX.with(|t| t.get().is_some()) as c_int };
		status::OK
	})
}

// ---- 互斥锁（OfxMultiThreadSuiteV1 的 mutex* 函数组）-----------------------
//
// 句柄表：全局注册表按 usize 键发号；`count > 1` 是信号量语义（ofxMultiThread.h
// "a mutex with a count greater than 1 is a counting semaphore"）。
// 插件在多线程渲染期用它们保护共享状态——ofxs 支持库的
// ofxsThreadSuiteCheck 在 load 时逐一检查这些函数非空，缺一个整批插件
// 直接拒载。

/// 计数信号量（count==1 时即普通互斥锁）。
struct OfxMutex {
	permits: std::sync::Mutex<c_int>,
	released: std::sync::Condvar,
}

/// 句柄注册表（句柄即下一个递增 id 转指针；0 保留给空）。
static MUTEXES: std::sync::LazyLock<Mutex<HashMap<usize, Arc<OfxMutex>>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 下一个互斥锁 id。
static NEXT_MUTEX: AtomicUsize = AtomicUsize::new(1);

fn mutex_lookup(handle: *mut c_void) -> Option<Arc<OfxMutex>> {
	if handle.is_null() {
		return None;
	}
	MUTEXES
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&(handle as usize))
		.cloned()
}

unsafe extern "C" fn mutex_create(out: *mut *mut c_void, count: c_int) -> c_int {
	caught(|| {
		if out.is_null() {
			return status::ERR_VALUE;
		}
		let id = NEXT_MUTEX.fetch_add(1, Ordering::Relaxed);
		MUTEXES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(id, Arc::new(OfxMutex {
				permits: std::sync::Mutex::new(count.max(1)),
				released: std::sync::Condvar::new(),
			}));
		unsafe { *out = id as *mut c_void };
		status::OK
	})
}

unsafe extern "C" fn mutex_destroy(handle: *mut c_void) -> c_int {
	caught(|| {
		if mutex_lookup(handle).is_none() {
			return status::ERR_BAD_HANDLE;
		}
		MUTEXES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.remove(&(handle as usize));
		status::OK
	})
}

unsafe extern "C" fn mutex_lock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
		while *permits <= 0 {
			permits = m.released.wait(permits).unwrap_or_else(|e| e.into_inner());
		}
		*permits -= 1;
		status::OK
	})
}

unsafe extern "C" fn mutex_unlock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		{
			let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
			*permits += 1;
		}
		m.released.notify_one();
		status::OK
	})
}

unsafe extern "C" fn mutex_try_lock(handle: *mut c_void) -> c_int {
	caught(|| {
		let Some(m) = mutex_lookup(handle) else {
			return status::ERR_BAD_HANDLE;
		};
		let mut permits = m.permits.lock().unwrap_or_else(|e| e.into_inner());
		if *permits <= 0 {
			// 规范：占不到锁返回 kOfxStatFailed（不是错误）。
			return status::FAILED;
		}
		*permits -= 1;
		status::OK
	})
}

/// 静态函数表实例。
pub fn suite_v1() -> &'static MultiThreadSuiteV1 {
	static SUITE: std::sync::OnceLock<MultiThreadSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| MultiThreadSuiteV1 {
		multi_thread: multi_thread,
		num_cpus: multi_thread_num_cpus,
		index: multi_thread_index,
		is_spawned: multi_thread_is_spawned,
		mutex_create: mutex_create,
		mutex_destroy: mutex_destroy,
		mutex_lock: mutex_lock,
		mutex_unlock: mutex_unlock,
		mutex_try_lock: mutex_try_lock,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};

	/// 每个插件线程把 (index, count) 累计进 userdata。
	unsafe extern "C" fn worker(index: c_uint, count: c_uint, arg: *mut c_void) {
		let state = unsafe { &mut *(arg as *mut (AtomicUsize, AtomicUsize)) };
		state.0.fetch_add(index as usize, Ordering::Relaxed);
		state.1.fetch_add(count as usize, Ordering::Relaxed);
	}

	#[test]
	fn multi_thread_spawns_and_joins() {
		let s = suite_v1();
		let state = (AtomicUsize::new(0), AtomicUsize::new(0));
		unsafe {
			assert_eq!(
				(s.multi_thread)(worker, 8, &state as *const _ as *mut c_void),
				status::OK
			);
		}
		// 索引 0..7 各一次 → 28；count 各 8 → 64。
		assert_eq!(state.0.load(Ordering::Relaxed), 28);
		assert_eq!(state.1.load(Ordering::Relaxed), 64);

		// 0 线程：no-op OK。
		unsafe {
			assert_eq!(
				(s.multi_thread)(worker, 0, &state as *const _ as *mut c_void),
				status::OK
			);
		}
	}

	/// 插件线程内查询 index/isSpawned 并把结果写回 arg 指向的槽。
	/// （闭包捕获 suite 无法转 fn 指针，用具名函数 + 静态槽。）
	static SPAWNED_RESULT: std::sync::LazyLock<std::sync::Mutex<Option<(c_int, c_int)>>> =
		std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

	unsafe extern "C" fn query_worker(_index: c_uint, _count: c_uint, _arg: *mut c_void) {
		let s = suite_v1();
		let mut i = 0;
		let mut sp = 1;
		unsafe {
			let _ = (s.index)(&mut i);
			let _ = (s.is_spawned)(&mut sp);
		}
		*SPAWNED_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some((i, sp));
	}

	#[test]
	fn index_and_spawned() {
		let s = suite_v1();
		let mut i = 0;
		let mut spawned = 1;
		unsafe {
			// 宿主线程：index = -1，isSpawned = 0。
			assert_eq!((s.index)(&mut i), status::OK);
			assert_eq!(i, -1);
			assert_eq!((s.is_spawned)(&mut spawned), status::OK);
			assert_eq!(spawned, 0);

			// 插件线程内：index 正确、isSpawned = 1。
			assert_eq!(
				(s.multi_thread)(query_worker, 1, std::ptr::null_mut()),
				status::OK
			);
			assert_eq!(
				*SPAWNED_RESULT.lock().unwrap_or_else(|e| e.into_inner()),
				Some((0, 1))
			);
		}
	}

	#[test]
	fn num_cpus_positive() {
		let s = suite_v1();
		let mut n = 0;
		unsafe {
			assert_eq!((s.num_cpus)(&mut n), status::OK);
		}
		assert!(n >= 1);
	}
}
