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

//! # oakrender — the render engine (Rust)
//!
//! Reimplements the C++ oakrender module behind its frozen C ABI
//! (`include/render/*.h`). See README.md for the architectural mapping.
//!
//! Module map (mirrors `COVERAGE.md`):
//! - `handle` — refcounted C-handle scaffolding (C++ internalhandles.h)
//! - `cache` — PlaybackCache/FrameHashCache family + disk state
//! - `ticket` — ticket arena with exactly-once completion
//! - `worker` — the JobDispatch seam + thread-free inline dispatcher +
//!   graph snapshot store (the in-process thread pool was deleted in M15 S2)
//! - `manager` — RenderManager singleton + disk cache
//! - `autocacher` — PreviewAutoCacher
//! - `eval` — the evaluation seam (RenderHooks)
//! - `frameio` — the disk frame-cache container (liboakoiio pending)
//! - `shaderfx` — effect GLSL→WGSL translation + uniform packing
//! - `copier` — render-side project-copy client (oaknode C ABI)
//! - `cancelatom` — the cancellation primitive
//! - `ipc` — render-worker NDJSON protocol + shm frame-slot transport
//! - `scheduler` — preview frame scheduler (interleaved batch claims)
//! - `procpool` — process-isolated render backend (M15)
//! - `pipeline` — the M1 thread pipeline backend (render thread + decode
//!   thread; selected by `OAK_PIPELINE=threads`)
//! - `error` — re-exports `oak_core::error` (the backend/color/texture/
//!   frame value types moved to `oak-core` in the oak-common merge)

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod autocacher;
pub mod cache;
pub mod cancelatom;
pub mod copier;
pub mod error;
pub mod eval;
pub mod frameio;
pub mod handle;
pub mod ipc;
pub mod manager;
pub mod ofxhost;
pub mod pipeline;
pub mod procpool;
pub mod scheduler;
pub mod shaderfx;
pub mod textengine;
pub mod ticket;
pub mod worker;
