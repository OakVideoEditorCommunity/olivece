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

//! # oaktimeline — the timeline edit module (Rust)
//!
//! Reimplements the C++ oaktimeline module (`src/timeline/src`). See
//! README.md for the architectural mapping (per-header domain modules, no
//! UndoCommand inheritance → boxed `oakundo` command values).
//!
//! ## Single-lib unification
//!
//! The C ABI export layer (`ffi.rs`) and the oaknode/oakundo/oak_core
//! bridge (`bridge/`) were deleted in the single-lib unification: undo
//! commands are now `oak_undo::undocommand::UndoCommand` values (the
//! crate's command structs implement `undocommon::Command` and are boxed
//! as trait objects) and every node/block/track reference is a
//! [`util::NodeRef`] — an `Arc<Mutex<oak_node::project::Project>>` +
//! `oak_node::id::NodeId` pair that the commands manipulate through the
//! oaknode Rust domain directly.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod common;
pub mod error;
pub mod handle;
pub mod marker;
pub mod multicam;
pub mod undocommon;
pub mod undogeneral;
pub mod undopointer;
pub mod undoripple;
pub mod undosplit;
pub mod undotrack;
pub mod util;
pub mod workarea;
