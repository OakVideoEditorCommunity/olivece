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

//! Background task submission hook (`include/codec/task.h`).
//!
//! The codec module needs occasional background work (audio conforms,
//! proxy transcodes). The task system itself splits out at milestone M8;
//! until then oakcodec exposes a single global submit callback. A host
//! (M8: oaktask) registers with [`set_task_submit_cb`]; the conform/proxy
//! managers call it whenever they need a task. With no callback, managers
//! report work as unavailable — they never crash and never block.

use std::ffi::{c_void, CString};
use std::sync::Mutex;

use crate::error::{Error, OAKCODEC_OK};

/// Kinds of background tasks oakcodec can request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum TaskKind {
	/// Audio conform to pcm cache files.
	Conform = 0,
	/// Video proxy transcode.
	Proxy = 1,
}

/// Description of one background task request.
///
/// All strings are borrowed and only valid for the duration of the submit
/// call; the callback must copy anything it retains.
#[repr(C)]
pub struct TaskRequest<'a> {
	/// `TaskKind`.
	pub kind: TaskKind,
	/// Source media filename.
	pub input_filename: &'a str,
	/// Final destination path (see field docs in include/codec/task.h).
	pub output_filename: &'a str,
	/// Stream inside the source media.
	pub stream_index: i32,
	/// Conform: target sample rate.
	pub sample_rate: i32,
	/// Conform: target channel layout mask.
	pub channel_layout: u64,
	/// Conform: target sample format (enum as int).
	pub sample_format: i32,
	/// Proxy: target width (0 = unspecified/divider-based).
	pub proxy_width: i32,
	/// Proxy: target height (0 = unspecified/divider-based).
	pub proxy_height: i32,
}

/// Task submit callback signature.
///
/// Returns `Ok(())` if the task was accepted (completed synchronously or
/// queued), `Err` if the request was rejected.
pub type TaskSubmitFn = dyn Fn(&TaskRequest, *mut std::ffi::c_void) -> crate::error::Result<()>;

/// `OakCodecTaskRequest` — C ABI mirror of [`TaskRequest`] for the submit
/// callback; see `include/codec/task.h`. Strings are borrowed C pointers,
/// valid only for the duration of the call.
#[repr(C)]
pub struct OakCodecTaskRequest {
	/// `TaskKind` value.
	pub kind: i32,
	/// Source media filename.
	pub input_filename: *const std::ffi::c_char,
	/// Final destination path.
	pub output_filename: *const std::ffi::c_char,
	/// Stream inside the source media.
	pub stream_index: i32,
	/// Conform: target sample rate.
	pub sample_rate: i32,
	/// Conform: target channel-layout mask.
	pub channel_layout: u64,
	/// Conform: target sample format (enum as int).
	pub sample_format: i32,
	/// Proxy: target width (0 = unspecified/divider-based).
	pub proxy_width: i32,
	/// Proxy: target height (0 = unspecified/divider-based).
	pub proxy_height: i32,
}

/// `oakcodec_task_submit_fn` — the extern-C submit callback typedef; see
/// `include/codec/task.h`. Returns `OAKCODEC_OK` on accept, else a
/// negative `OAKCODEC_E_*` code.
pub type OakCodecTaskSubmitFn =
	unsafe extern "C" fn(req: *const OakCodecTaskRequest, userdata: *mut std::ffi::c_void) -> i32;

/// One registered submit callback (extern-C from the host, or a crate
/// Rust closure). Mirrors the C++ `g_task_cb`/`g_task_cb_userdata` pair.
enum SubmitCb {
	/// No callback registered.
	None,
	/// Extern-C callback registered via `oakcodec_set_task_submit_cb`.
	///
	/// Only constructed by the tests below and by the extern-C entry point
	/// that the removed C ABI used to expose; kept for that lane.
	#[allow(dead_code)]
	Extern {
		/// The C function pointer.
		cb: OakCodecTaskSubmitFn,
		/// Opaque userdata passed back on each call.
		userdata: *mut c_void,
	},
	/// Crate-internal Rust closure registered via [`set_task_submit_cb`].
	Rust {
		/// Raw fat-pointer to the `&'static TaskSubmitFn` (kept `*const` so
		/// the registry is `Send`).
		cb: *const TaskSubmitFn,
		/// Opaque userdata passed back on each call.
		userdata: *mut c_void,
	},
}

// # Safety: the stored pointers (extern-C fn pointer, fat pointer to a
// 'static closure, userdata) are only dereferenced/called while holding the
// registry mutex; the Rust closure is 'static and the extern-C fn outlives
// registration by contract. Moving the enum between threads under the lock
// therefore cannot alias.
unsafe impl Send for SubmitCb {}

/// The global task submit callback registry. Only one callback is held at
/// a time; registering replaces it, `None` clears it. Thread-safe.
static TASK_SUBMIT: Mutex<SubmitCb> = Mutex::new(SubmitCb::None);

/// Registers (or replaces) the global task submit callback. Pass `None` to
/// unregister. Thread-safe. Interim state (pre-M8): nobody registers and all
/// task-dependent work reports unavailable.
pub fn set_task_submit_cb(cb: Option<&'static TaskSubmitFn>, userdata: *mut std::ffi::c_void) {
	let mut g = TASK_SUBMIT.lock().unwrap();
	*g = match cb {
		Some(cb) => SubmitCb::Rust {
			cb: cb as *const TaskSubmitFn,
			userdata,
		},
		None => SubmitCb::None,
	};
}

/// Register an extern-C submit callback (used by `ffi::task`).
///
/// Mirrors `oakcodec_set_task_submit_cb`: a `None` pointer clears it.
/// Only the tests below call it (the extern-C lane is exercised there).
#[allow(dead_code)]
pub(crate) fn set_task_submit_cb_extern(
	cb: Option<OakCodecTaskSubmitFn>,
	userdata: *mut std::ffi::c_void,
) {
	let mut g = TASK_SUBMIT.lock().unwrap();
	*g = match cb {
		Some(cb) => SubmitCb::Extern { cb, userdata },
		None => SubmitCb::None,
	};
}

/// Returns 1 if a submit callback is currently registered, else 0.
/// Thread-safe.
pub fn task_submit_is_registered() -> bool {
	let g = TASK_SUBMIT.lock().unwrap();
	!matches!(&*g, SubmitCb::None)
}

/// Submit a task through the registered callback, if any.
///
/// Returns `Ok(false)` when no callback is registered (nothing submitted),
/// `Ok(true)` when accepted, or `Err` when the callback rejected it.
pub fn submit_task(req: &TaskRequest) -> crate::error::Result<bool> {
	let g = TASK_SUBMIT.lock().unwrap();
	match &*g {
		SubmitCb::None => Ok(false),
		SubmitCb::Extern { cb, userdata } => {
			// Bind the C strings to locals so the temporaries outlive the
			// callback call (their pointers feed the request struct).
			let in_c = cstring_or_empty(req.input_filename);
			let out_c = cstring_or_empty(req.output_filename);
			let creq = OakCodecTaskRequest {
				kind: req.kind as i32,
				input_filename: in_c.as_ptr(),
				output_filename: out_c.as_ptr(),
				stream_index: req.stream_index,
				sample_rate: req.sample_rate,
				channel_layout: req.channel_layout,
				sample_format: req.sample_format,
				proxy_width: req.proxy_width,
				proxy_height: req.proxy_height,
			};
			// # Safety: the callback is a C function we registered; passing a
			// request whose string pointers are alive for the call duration.
			let ret = unsafe { cb(&creq, *userdata) };
			if ret == OAKCODEC_OK {
				Ok(true)
			} else {
				Err(Error::Failed(format!(
					"task submit rejected (code {})",
					ret
				)))
			}
		}
		SubmitCb::Rust { cb, userdata } => {
			// # Safety: the fat pointer was stored by set_task_submit_cb and
			// points to a 'static closure that outlives this call.
			let cb = unsafe { &**cb };
			match cb(req, *userdata) {
				Ok(()) => Ok(true),
				Err(e) => Err(e),
			}
		}
	}
}

/// Build a NUL-terminated C string from a Rust string; empty on embedded
/// NUL (callers pass sane filenames, so this is defensive only).
fn cstring_or_empty(s: &str) -> CString {
	CString::new(s).unwrap_or_else(|_| CString::new("").unwrap())
}

#[cfg(test)]
mod tests {
	use super::*;
	// Same registry lock the conform/proxy/ffi tests use: the submit
	// callback is process-global and every test that mutates it must
	// serialize on the same mutex.
	use crate::conformmanager::test_util::REG_LOCK;

	unsafe extern "C" fn reject_cb(
		_req: *const OakCodecTaskRequest,
		_ud: *mut std::ffi::c_void,
	) -> i32 {
		-1 // rejected
	}

	#[test]
	fn submit_via_rust_closure_and_clear() {
		let _g = REG_LOCK.lock().unwrap();

		// A Rust closure that accepts and records the request.
		let accepted = std::sync::Arc::new(std::sync::Mutex::new(false));
		let recorded = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
		let acc = accepted.clone();
		let rec = recorded.clone();
		let cb: &'static TaskSubmitFn = Box::leak(Box::new(
			move |req: &TaskRequest, _ud: *mut std::ffi::c_void| {
				*acc.lock().unwrap() = true;
				*rec.lock().unwrap() = Some(req.output_filename.to_string());
				Ok(())
			},
		));
		set_task_submit_cb(Some(cb), std::ptr::null_mut());
		assert!(task_submit_is_registered());

		let req = TaskRequest {
			kind: TaskKind::Conform,
			input_filename: "in.mp4",
			output_filename: "out.pcm",
			stream_index: 1,
			sample_rate: 48000,
			channel_layout: 0x3,
			sample_format: 10,
			proxy_width: 0,
			proxy_height: 0,
		};
		assert!(submit_task(&req).unwrap());
		assert!(*accepted.lock().unwrap());
		assert_eq!(recorded.lock().unwrap().as_deref(), Some("out.pcm"));

		// Clearing the callback: nothing submitted.
		set_task_submit_cb(None, std::ptr::null_mut());
		assert!(!task_submit_is_registered());
		assert!(!submit_task(&req).unwrap());
	}

	#[test]
	fn extern_cb_reject_maps_to_err() {
		let _g = REG_LOCK.lock().unwrap();
		set_task_submit_cb_extern(Some(reject_cb), std::ptr::null_mut());
		let req = TaskRequest {
			kind: TaskKind::Proxy,
			input_filename: "in.mp4",
			output_filename: "out.mp4",
			stream_index: 0,
			sample_rate: 0,
			channel_layout: 0,
			sample_format: 0,
			proxy_width: 1280,
			proxy_height: 720,
		};
		assert!(submit_task(&req).is_err());
		set_task_submit_cb_extern(None, std::ptr::null_mut());
	}

	#[test]
	fn extern_cb_accept_returns_ok() {
		let _g = REG_LOCK.lock().unwrap();
		set_task_submit_cb_extern(
			Some(crate::conformmanager::test_util::accept_cb),
			std::ptr::null_mut(),
		);
		let req = TaskRequest {
			kind: TaskKind::Conform,
			input_filename: "in.mp4",
			output_filename: "out.pcm",
			stream_index: 0,
			sample_rate: 0,
			channel_layout: 0,
			sample_format: 0,
			proxy_width: 0,
			proxy_height: 0,
		};
		assert!(submit_task(&req).unwrap());
		set_task_submit_cb_extern(None, std::ptr::null_mut());
	}

	#[test]
	fn cstring_or_empty_handles_embedded_nul() {
		// Embedded NUL -> empty string (defensive).
		let c = cstring_or_empty("a\0b");
		assert_eq!(c.as_c_str().to_bytes(), b"");
		assert_eq!(cstring_or_empty("ok").as_c_str().to_bytes(), b"ok");
	}

	#[test]
	fn task_kind_values_match_abi() {
		assert_eq!(TaskKind::Conform as i32, OAKCODEC_TASK_CONFORM as i32);
		assert_eq!(TaskKind::Proxy as i32, OAKCODEC_TASK_PROXY as i32);
	}

	// ABI constants mirrored from include/codec/task.h.
	const OAKCODEC_TASK_CONFORM: i32 = 0;
	const OAKCODEC_TASK_PROXY: i32 = 1;
}
