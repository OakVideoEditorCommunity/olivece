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

//! The shared ABI value-handle type (single-lib unification).
//!
//! Every module crate used to define its own `CHandle` with an identical
//! `{ctx, addref, release, abi_version}` layout but a distinct Rust type,
//! so cross-module calls had to cross an `extern "C"`/dlsym boundary.
//! With the module-to-module calls going back to direct Rust calls (see
//! `docs/zh/plans/riir/single-lib.md`), one canonical handle type lives
//! here in `oakcore-rs` (the crate every module already depends on) and
//! every crate re-exports it. The C layout is unchanged, so the frozen
//! module C ABIs (`Oak<Mod><Type>*` handles in `include/<mod>/*.h`) are
//! unaffected.

use std::ffi::c_void;

/// ABI value handle mirroring the C handle struct
/// (`{ctx, addref, release, abi_version}`) from `include/common/handle.h`.
///
/// `ctx` points to an opaque refcounted box; `addref`/`release` are the
/// box's atomic increment/decrement (release destroys at zero);
/// `abi_version` is a tag stamped by each crate's `make_owned` (0 for
/// empty handles). Structurally identical to every `Oak<Mod><Type>` value
/// handle, so a handle can cross any module boundary by value.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CHandle {
	/// Opaque box pointer.
	pub ctx: *mut c_void,
	/// Atomic increment.
	pub addref: Option<unsafe extern "C" fn(*mut c_void)>,
	/// Atomic decrement; destroys at zero.
	pub release: Option<unsafe extern "C" fn(*mut c_void)>,
	/// ABI version.
	pub abi_version: u32,
}

impl PartialEq for CHandle {
	fn eq(&self, other: &Self) -> bool {
		// `ctx` and `abi_version` by value; the callbacks by address
		// (`==` on fn pointers is rejected by the compiler, and
		// `std::ptr::fn_addr_eq` rejects the `Option<fn>` fields).
		self.ctx == other.ctx
			&& self.addref.map(|f| f as usize) == other.addref.map(|f| f as usize)
			&& self.release.map(|f| f as usize) == other.release.map(|f| f as usize)
			&& self.abi_version == other.abi_version
	}
}

impl Eq for CHandle {}

impl CHandle {
	/// The empty handle.
	pub const fn null() -> Self {
		CHandle {
			ctx: std::ptr::null_mut(),
			addref: None,
			release: None,
			abi_version: 0,
		}
	}
	/// Whether this is the empty (zero) handle.
	pub fn is_null(&self) -> bool {
		self.ctx.is_null()
	}

	/// Take an additional reference through the handle's `addref` and
	/// return the (now refcount-incremented) copy.
	///
	/// # Safety
	/// `self` must be a live handle returned by a module function.
	pub unsafe fn addref(&self) -> Self {
		let mut copy = *self;
		if let Some(addref) = copy.addref {
			let _ = &mut copy;
			unsafe {
				addref(copy.ctx);
			}
		}
		copy
	}
}

/// The empty handle is the natural default (some crates derive `Default`
/// on structs holding a handle).
impl Default for CHandle {
	fn default() -> Self {
		Self::null()
	}
}

// Handles follow the shared_ptr-like convention of
// `include/common/handle.h`: they are opaque, refcounted and safe to
// share across threads (the module crates use them behind mutexes).
unsafe impl Send for CHandle {}
unsafe impl Sync for CHandle {}
