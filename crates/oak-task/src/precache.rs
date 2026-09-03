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

//! `PreCacheTask`, mirroring `src/task/src/precache/precachetask.h`.
//!
//! Renders a footage node through [`crate::render::RenderTask`] to fill
//! the playback cache. The footage and its sequence are now
//! [`crate::nodeops::NodeRef`] domain references (the deleted oaknode C
//! ABI stubs are gone); video params come from the sequence's parameter
//! streams and the cache range is the full footage video length.
//!
//! **Simplifications over the C++**: the deep project copy / viewer
//! wiring is not built (the render drives the given footage directly —
//! the ticket carries the footage filename and the footage node's
//! identity as the cache key) and the timeline work-area intersection is
//! replaced by the full footage length. The direct ticket arena's eval
//! producer does not persist frames to the frame cache yet, so this
//! render pass warms nothing on disk; the plumbing is in place.
//!
//! CPP-PARITY: src/task/src/precache/precachetask.h

use oak_core::videoparams::VideoParams;

use crate::error::Result;
use crate::nodeops::{self, NodeRef};
use crate::render::{RenderTask, RenderTaskBehavior};
use crate::task::{Task, TaskBehavior};
use oak_core::{Rational, TimeRange};

/// A pre-cache task: renders frames of a footage node into the playback
/// cache without any output file.
pub struct PreCacheTask {
	/// The render base (itself a task).
	pub render: RenderTask,
	/// The footage being cached.
	pub footage: NodeRef,
	/// Frame index within the footage being cached.
	pub index: i32,
	/// The sequence context the footage lives in.
	pub sequence: NodeRef,
}

impl PreCacheTask {
	/// Create a pre-cache task for the given footage at `index` inside
	/// `sequence`. The old `footage: CHandle` / `sequence: CHandle`
	/// signature is replaced by domain [`NodeRef`]s (single-lib
	/// unification).
	pub fn new(footage: NodeRef, index: i32, sequence: NodeRef) -> PreCacheTask {
		// The sequence's video params drive the render timebase (the
		// deleted `oaknode_sequence_get_video_params` stub is replaced by
		// the direct behavior query).
		let video_params: Option<VideoParams> =
			nodeops::sequence_video_params(&sequence.0, sequence.1, 0);

		// The footage filename labels the task (the deleted
		// `oaknode_footage_filename` stub is replaced by the direct
		// behavior query).
		let filename = nodeops::footage_filename(&footage.0, footage.1);
		let title = format!("Pre-caching {filename}:{index}");
		let base = Task::new(&title, None);

		// The render target is the footage node itself (pre-cache fills
		// that footage's frame cache).
		let render = RenderTask::new(base, video_params, footage.clone(), Default::default(), None);

		PreCacheTask {
			render,
			footage,
			index,
			sequence,
		}
	}
}

impl TaskBehavior for PreCacheTask {
	fn run(&mut self, task: &mut Task) -> Result<()> {
		// Share the caller's cancellation atom with the inner render base.
		self.render.base.set_cancel_atom(task.get_cancel_atom());

		// The full footage length is the cache range (simplified: no
		// work-area intersection). The deleted `oaknode_footage_get_video_length`
		// stub is replaced by the direct behavior query.
		let length = nodeops::node_length(&self.footage.0, self.footage.1);
		let range = TimeRange::new(Rational::new(0, 1), length);

		// No color manager / frame-cache handles exist on the direct
		// ticket path (the arena keys the cache by the footage node
		// identity — see `RenderTask::build_video_ticket`).
		self.render.set_render_inputs(0 /* k_online */, false, range);

		// Drive the render with `self` as the subclass behavior (the C++
		// virtual dispatch receiver); the render is temporarily moved out to
		// avoid a self-referential borrow and put back before returning.
		let mut render =
			std::mem::replace(&mut self.render, crate::render::RenderTask::placeholder());
		let result = render.render(task, self);
		self.render = render;

		result
	}
}

impl RenderTaskBehavior for PreCacheTask {
	fn frame_downloaded(&mut self, task: &mut Task, frame: &oak_core::texture::Texture) -> Result<()> {
		// Do nothing: pre-cache just fills the frame cache (the direct
		// ticket arena records the render through the ticket's cache
		// identity; see the module docs).
		let _ = (task, frame);
		Ok(())
	}

	fn audio_downloaded(
		&mut self,
		task: &mut Task,
		buffer: &oak_render::ticket::AudioSamples,
	) -> Result<()> {
		// Pre-cache doesn't cache any audio.
		let _ = (task, buffer);
		Ok(())
	}

	fn encode_subtitle(&mut self, task: &mut Task, text: &str) -> Result<()> {
		let _ = (task, text);
		Ok(())
	}
}
