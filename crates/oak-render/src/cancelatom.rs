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
//! Single-lib unification (see `docs/zh/plans/riir/single-lib.md`): the
//! implementation moved to oak_core; this module re-exports it so the
//! render ffi's `oakrender_cancelatom_*` exports (and their C-ABI
//! consumers, e.g. oakcodec) keep working unchanged.

/// Shared cancellation atom (oak_core).
pub use oak_core::cancelatom::CancelAtom;
