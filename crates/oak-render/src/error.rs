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

//! Error codes, mirroring `include/render/error.h` verbatim; project-wide
//! -MMCCCC scheme (module registry in include/common/error.h), pass-through untranslated.
//!
//! With the oak-common/oak-core merge the render value/GPU types moved into
//! `oak-core`, so this crate no longer carries its own error enum:
//! [`Error`]/[`Result`] are re-exported from [`oak_core::error`] (identical
//! variant shape and messages). The `OAKRENDER_*` codes below remain as the
//! module's public-code contract; `Error::code()` reports the unified
//! `OAKCORE_*` values.

/// Success.
pub const OAKRENDER_OK: i32 = 0;
/// Null handle or invalid argument.
pub const OAKRENDER_E_INVALID: i32 = -70001;
/// Call not valid in the current state.
pub const OAKRENDER_E_STATE: i32 = -70002;
/// The underlying operation failed.
pub const OAKRENDER_E_FAILED: i32 = -70003;
/// Index out of range / entry not found.
pub const OAKRENDER_E_NOT_FOUND: i32 = -70004;
/// Allocation failed.
pub const OAKRENDER_E_NOMEM: i32 = -70005;

pub use oak_core::error::{Error, Result};

#[cfg(test)]
mod tests {
	use super::*;

	/// Every variant must produce a non-empty `Display` message; the
	/// `Failed` variant must surface its context string.
	#[test]
	fn display_is_non_empty() {
		for msg in [
			Error::Invalid.to_string(),
			Error::State.to_string(),
			Error::Failed("context".into()).to_string(),
			Error::NotFound.to_string(),
			Error::NoMem.to_string(),
		] {
			assert!(!msg.trim().is_empty(), "empty Display message");
		}
		assert!(
			Error::Failed("context".into())
				.to_string()
				.contains("context")
		);
	}

	/// `Error` must be usable behind a trait object.
	#[test]
	fn error_is_object_safe() {
		let errs: Vec<Box<dyn std::error::Error>> = vec![
			Box::new(Error::Invalid),
			Box::new(Error::State),
			Box::new(Error::Failed("context".into())),
			Box::new(Error::NotFound),
			Box::new(Error::NoMem),
		];
		assert_eq!(errs.len(), 5);
	}

	/// No variant wraps a downstream error, so `source()` stays `None`.
	#[test]
	fn source_is_none() {
		assert!(std::error::Error::source(&Error::Failed("context".into())).is_none());
	}
}
