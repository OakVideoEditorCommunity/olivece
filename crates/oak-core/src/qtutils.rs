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

//! Small helpers for crossing the Qt <-> C boundary, mirroring
//! `include/common/qtutils.h`. No state and no handle.
//!
//! Port of `src/common/src/qtutils.{h,cpp}` and the thin wrapper in
//! `src/common/c_api/qtutils.cpp`.

use std::ffi::c_void;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use std::ffi::{c_char, c_int, CString};

use crate::error::{Error, Result};

/// Convert an opaque pointer to its numeric representation.
///
/// A null pointer yields `0`. Never fails; the `Result` mirrors the C ABI
/// shape (the c_api only rejects a null `out_value`).
pub fn ptr_to_value(ptr: *mut c_void) -> Result<u64> {
	// CPP-PARITY: `reinterpret_cast<uintptr_t>(ptr)`; zero-extended to 64 bits.
	Ok(ptr as usize as u64)
}

/// Convert a numeric representation back to an opaque pointer.
///
/// `0` yields a null pointer. Never fails; the `Result` mirrors the C ABI
/// shape (the c_api only rejects a null `out_ptr`).
pub fn value_to_ptr(value: u64) -> Result<*mut c_void> {
	// CPP-PARITY: `reinterpret_cast<T*>(value)`, truncating to the pointer
	// width like a 32-bit `uintptr_t` cast would.
	Ok(value as usize as *mut c_void)
}

/// macOS/FreeBSD `struct timespec` mirror (`sys/types.h`).
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[repr(C)]
struct Timespec {
	tv_sec: i64,
	tv_nsec: i64,
}

/// macOS/FreeBSD `struct stat` mirror (`sys/stat.h`), laid out field-for-field
/// so the raw `stat()` FFI below reads `st_birthtimespec` / `st_ctimespec`.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[repr(C)]
struct Stat {
	st_dev: i32,
	st_mode: u16,
	st_nlink: u16,
	st_ino: u64,
	st_uid: u32,
	st_gid: u32,
	st_rdev: i32,
	_st_pad: u32,
	st_atimespec: Timespec,
	st_mtimespec: Timespec,
	st_ctimespec: Timespec,
	st_birthtimespec: Timespec,
	st_size: i64,
	st_blocks: i64,
	st_blksize: i32,
	st_flags: u32,
	st_gen: u32,
	st_lspare: i32,
	st_qspare: [i64; 2],
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
extern "C" {
	fn stat(path: *const c_char, buf: *mut Stat) -> c_int;
}

/// File creation time as seconds since the Unix epoch.
///
/// On filesystems that record a birth time that value is used; otherwise the
/// metadata change time is returned. Mirrors `olive::QtUtils::get_creation_date`
/// plus the c_api wrapper, which turns a zero (epoch) result into
/// `E_NOT_FOUND`.
pub fn get_creation_date(path: &str) -> Result<i64> {
	#[cfg(any(target_os = "macos", target_os = "freebsd"))]
	{
		let cpath = CString::new(path).map_err(|_| Error::Invalid)?;
		let mut st = Stat {
			st_dev: 0,
			st_mode: 0,
			st_nlink: 0,
			st_ino: 0,
			st_uid: 0,
			st_gid: 0,
			st_rdev: 0,
			_st_pad: 0,
			st_atimespec: Timespec {
				tv_sec: 0,
				tv_nsec: 0,
			},
			st_mtimespec: Timespec {
				tv_sec: 0,
				tv_nsec: 0,
			},
			st_ctimespec: Timespec {
				tv_sec: 0,
				tv_nsec: 0,
			},
			st_birthtimespec: Timespec {
				tv_sec: 0,
				tv_nsec: 0,
			},
			st_size: 0,
			st_blocks: 0,
			st_blksize: 0,
			st_flags: 0,
			st_gen: 0,
			st_lspare: 0,
			st_qspare: [0, 0],
		};
		if unsafe { stat(cpath.as_ptr(), &mut st) } != 0 {
			return Err(Error::NotFound);
		}
		// CPP-PARITY: birth time, falling back to the metadata change time when
		// the birth time is unset (0 / -1) — qtutils.cpp on macOS/FreeBSD.
		let mut secs = st.st_birthtimespec.tv_sec;
		if secs == 0 || secs == -1 {
			secs = st.st_ctimespec.tv_sec;
		}
		// CPP-PARITY: the c_api maps a zero (epoch) result to E_NOT_FOUND.
		if secs == 0 {
			return Err(Error::NotFound);
		}
		Ok(secs)
	}

	#[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
	{
		let meta = std::fs::metadata(path).map_err(|_| Error::NotFound)?;
		// CPP-PARITY: `st_ctime` (metadata change time) has no std::fs
		// equivalent; fall back to creation then modification time.
		let t = meta
			.created()
			.or_else(|_| meta.modified())
			.map_err(|_| Error::NotFound)?;
		let secs = t
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_secs() as i64)
			.unwrap_or(0);
		if secs == 0 {
			return Err(Error::NotFound);
		}
		Ok(secs)
	}
}

#[cfg(test)]
mod tests {
	use std::ffi::c_void;
	use std::sync::atomic::{AtomicU64, Ordering};

	use super::*;
	use crate::error::{Error, Result};

	#[test]
	fn ptr_to_value_null_is_zero() {
		assert_eq!(ptr_to_value(std::ptr::null_mut()).unwrap(), 0);
	}

	#[test]
	fn ptr_value_round_trip() {
		let value: u64 = 0x1234_5678_9abc_def0;
		let ptr = value_to_ptr(value).unwrap();
		assert_eq!(ptr_to_value(ptr).unwrap(), value);
		assert!(!ptr.is_null());
	}

	#[test]
	fn value_to_ptr_null_is_null() {
		assert!(value_to_ptr(0).unwrap().is_null());
	}

	#[test]
	fn value_to_ptr_accepts_any_u64() {
		// Mirrors `reinterpret_cast<void*>` which never rejects an input.
		assert!(value_to_ptr(u64::MAX).is_ok());
		assert!(value_to_ptr(1).is_ok());
	}

	#[test]
	fn ptr_to_value_of_real_pointer_round_trips() {
		let data = 42i32;
		let raw = &data as *const i32 as *mut c_void;
		let n = ptr_to_value(raw).unwrap();
		let back = value_to_ptr(n).unwrap();
		assert_eq!(back as *const i32 as *const i32, &data as *const i32);
	}

	// Stable temp-file helper: unique path under the system temp dir.
	fn temp_file_path(tag: &str) -> std::path::PathBuf {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		std::env::temp_dir().join(format!(
			"oak_core_qtutils_{}_{}_{}.tmp",
			std::process::id(),
			tag,
			n
		))
	}

	#[test]
	fn creation_date_of_existing_file() {
		let p = temp_file_path("exists");
		std::fs::write(&p, b"hello").unwrap();
		let secs = get_creation_date(p.to_str().unwrap());
		// On macOS both paths read the same birth time; just assert success
		// and plausibility rather than exact equality across fs providers.
		match secs {
			Ok(s) => assert!(s > 0, "creation time should be in the past"),
			Err(e) => panic!("expected Ok, got {e:?}"),
		}
		// Cross-check against the std metadata creation time when available.
		if let Ok(meta) = std::fs::metadata(&p) {
			if let Ok(created) = meta.created() {
				let expected = created
					.duration_since(std::time::UNIX_EPOCH)
					.map(|d| d.as_secs() as i64)
					.unwrap_or(0);
				assert_eq!(secs.unwrap(), expected);
			}
		}
		let _ = std::fs::remove_file(&p);
	}

	#[test]
	fn creation_date_matches_std_for_new_file() {
		let p = temp_file_path("match");
		std::fs::write(&p, b"data").unwrap();
		let ours = get_creation_date(p.to_str().unwrap()).unwrap();
		let expected = std::fs::metadata(&p)
			.and_then(|m| m.created())
			.map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64)
			.unwrap_or(0);
		assert!(expected > 0);
		assert_eq!(ours, expected);
		let _ = std::fs::remove_file(&p);
	}

	#[test]
	fn creation_date_missing_file_is_not_found() {
		let p = temp_file_path("missing");
		let _ = std::fs::remove_file(&p);
		let res = get_creation_date(p.to_str().unwrap());
		assert!(matches!(res, Err(Error::NotFound)));
	}

	#[test]
	fn creation_date_empty_path_is_not_found() {
		assert!(matches!(get_creation_date(""), Err(Error::NotFound)));
	}

	#[test]
	fn creation_date_returns_result_ok_type() {
		let p = temp_file_path("typetest");
		std::fs::write(&p, b"x").unwrap();
		let res: Result<i64> = get_creation_date(p.to_str().unwrap());
		assert!(res.is_ok());
		let _ = std::fs::remove_file(&p);
	}

	#[test]
	fn value_to_ptr_is_injective_over_u64() {
		// Distinct values that survive the pointer-width cast map back distinctly.
		let a = value_to_ptr(1).unwrap();
		let b = value_to_ptr(2).unwrap();
		assert_ne!(a, b);
	}
}
