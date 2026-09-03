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

/// Success.
pub const OAKCORE_OK: i32 = 0;
/// Null handle or invalid argument.
pub const OAKCORE_E_INVALID: i32 = -10001;
/// Call not valid in the current state.
pub const OAKCORE_E_STATE: i32 = -10002;
/// The underlying operation failed.
pub const OAKCORE_E_FAILED: i32 = -10003;
/// Index out of range / entry not found.
pub const OAKCORE_E_NOT_FOUND: i32 = -10004;
/// Allocation failed.
pub const OAKCORE_E_NOMEM: i32 = -10005;

/// Crate-internal result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Crate-internal error.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    /// Null handle or invalid argument.
    #[error("render: invalid argument")]
    Invalid,
    /// Wrong state.
    #[error("render: invalid state")]
    State,
    /// Operation failed (context string is log-only).
    #[error("render: operation failed: {0}")]
    Failed(String),
    /// Not found.
    #[error("render: not found")]
    NotFound,
    /// Out of memory.
    #[error("render: out of memory")]
    NoMem,
}

impl Error {
    /// Map to the public error code.
    pub fn code(&self) -> i32 {
        match self {
            Error::Invalid => OAKCORE_E_INVALID,
            Error::State => OAKCORE_E_STATE,
            Error::Failed(_) => OAKCORE_E_FAILED,
            Error::NotFound => OAKCORE_E_NOT_FOUND,
            Error::NoMem => OAKCORE_E_NOMEM,
        }
    }

    /// Create a [`Error::Failed`] carrying a context message (log-only).
    ///
    /// Convenience constructor used by the OCIO/`image` wrappers in
    /// `ocioutils.rs` / `oiioutils.rs` when an underlying library call fails.
    pub fn new(message: impl Into<String>) -> Self {
        Error::Failed(message.into())
    }
}

impl From<ocio_rs::OcioError> for Error {
    fn from(e: ocio_rs::OcioError) -> Self {
        // The Display impl of `OcioError` always produces a non-empty message
        // (every variant carries text or a fixed phrase); it becomes the
        // log-only context of `Error::Failed`.
        Error::Failed(e.to_string())
    }
}

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

    /// The `code()` mapping must be unchanged by the `Error` trait impl.
    #[test]
    fn code_mapping_unchanged() {
        assert_eq!(Error::Invalid.code(), OAKCORE_E_INVALID);
        assert_eq!(Error::State.code(), OAKCORE_E_STATE);
        assert_eq!(Error::Failed("context".into()).code(), OAKCORE_E_FAILED);
        assert_eq!(Error::NotFound.code(), OAKCORE_E_NOT_FOUND);
        assert_eq!(Error::NoMem.code(), OAKCORE_E_NOMEM);
    }
}
