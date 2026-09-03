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

//! The cancellation primitive (`olive::CancelAtom`): a thread-safe cancel
//! flag shared between a render/encode caller and its worker.
//!
//! Single-lib unification (see `docs/zh/plans/riir/single-lib.md`): moved
//! here from the oakrender crate so the oakcodec crate can use it without
//! depending on oakrender (the oaknode/oakcodec/oakrender call cycle).

use std::sync::{Mutex, MutexGuard};

/// A cancellation atom (C++ `CancelAtom`). Reading a set flag also records
/// that a consumer heard the cancellation.
#[derive(Default)]
pub struct CancelAtom {
	state: Mutex<AtomState>,
}

#[derive(Default)]
struct AtomState {
	cancelled: bool,
	heard: bool,
}

fn lock(m: &Mutex<AtomState>) -> MutexGuard<'_, AtomState> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

impl CancelAtom {
	/// A not-cancelled atom.
	pub fn new() -> Self {
		Self::default()
	}

	/// Set the cancel flag (C++ `CancelAtom::cancel()`).
	pub fn cancel(&self) {
		lock(&self.state).cancelled = true;
	}

	/// Read the cancel flag; reading a set flag records that the
	/// cancellation was heard (C++ `is_cancelled()`).
	pub fn is_cancelled(&self) -> bool {
		let mut s = lock(&self.state);
		if s.cancelled {
			s.heard = true;
		}
		s.cancelled
	}

	/// Whether any consumer has observed the cancel flag through
	/// [`CancelAtom::is_cancelled`] (C++ `heard_cancel()`).
	pub fn heard_cancel(&self) -> bool {
		lock(&self.state).heard
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fresh_atom_is_not_cancelled() {
		let a = CancelAtom::new();
		assert!(!a.is_cancelled());
		assert!(!a.heard_cancel());
	}

	#[test]
	fn cancel_sets_flag_and_reading_marks_heard() {
		let a = CancelAtom::new();
		a.cancel();
		assert!(!a.heard_cancel(), "not heard until read");
		assert!(a.is_cancelled());
		assert!(a.heard_cancel(), "reading a set flag marks it heard");
	}

	#[test]
	fn repeated_cancel_is_idempotent() {
		let a = CancelAtom::new();
		a.cancel();
		a.cancel();
		assert!(a.is_cancelled());
	}
}
