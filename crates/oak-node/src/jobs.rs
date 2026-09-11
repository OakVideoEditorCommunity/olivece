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

//! Render-job payloads (C++ `app/render/job/footagejob.h`,
//! `app/render/job/shaderjob.h`): boxed inside `Texture` values during
//! graph evaluation, then resolved to real textures by the render hooks
//! ([`crate::traverser::RenderHooks::resolve`]).
//! `// CPP-PARITY: app/render/job/footagejob.h, shaderjob.h`.

use oak_core::color::ColorProcessor;
use oak_core::Rational;
use crate::id::NodeId;
use crate::nodes::plugin::PluginJobPayload;
use crate::value::{NodeValue, NodeValueRow};

/// The render-job enum (C++ the `RenderJob` class hierarchy of
/// `app/render/job/*.h`).
///
/// One `Job` travels boxed inside a texture-typed [`NodeValue`] during
/// graph evaluation; the render seam probes the box for this type,
/// recurses into the payload's input values, then dispatches on the
/// variant. Wrapping every payload in one enum lets the seam recognize
/// and recurse into job boxes without knowing which node produced them.
#[derive(Clone, Debug)]
pub enum Job {
	/// A footage decode request ([`FootageJobPayload`]).
	FootageJob(FootageJobPayload),
	/// A GPU shader pass ([`ShaderJobPayload`]).
	ShaderJob(ShaderJobPayload),
	/// An OFX plugin render ([`PluginJobPayload`]).
	PluginJob(PluginJobPayload),
	/// An OCIO color transform ([`ColorTransformJobPayload`]).
	ColorTransformJob(ColorTransformJobPayload),
	/// A disk frame-cache read ([`CacheJobPayload`]).
	CacheJob(CacheJobPayload),
}

/// C++ `FootageJob` payload: the decode request a footage node emits at
/// its output instead of a texture. The render hooks decode it at the
/// request time and replace it with the resulting frame.
#[derive(Clone, Debug)]
pub struct FootageJobPayload {
	/// Footage file path.
	pub filename: String,
	/// Container stream index.
	pub stream_index: i32,
	/// Request time in media seconds.
	pub time: Rational,
}

/// C++ `ShaderJob` payload: the GPU shader pass a node emits at its
/// output. The fragment shader is looked up by `type_id`/`shader_id` in
/// the node behavior; the param row carries the uniforms (including the
/// effect input texture, keyed by `effect_input`).
#[derive(Clone, Debug)]
pub struct ShaderJobPayload {
	/// Emitting node identity (for diagnostics).
	pub node_id: NodeId,
	/// Request time in media seconds.
	pub time: Rational,
	/// Pass iterations (C++ `ShaderJob::iterations`).
	pub iterations: i32,
	/// Node behavior type id — the pipeline cache key and the lookup key
	/// for the emitting node (C++ `job.node`).
	pub type_id: String,
	/// Shader variant id passed to the behavior's `shader_code()` (C++
	/// `ShaderJob::shader_id`); empty for the default variant.
	pub shader_id: String,
	/// Effect input id: the param row key carrying the main input texture
	/// (C++ `node->GetEffectInput()`).
	pub effect_input: String,
	/// The param row at evaluation time (C++ `ShaderJob::params`): uniform
	/// values keyed by input id, the effect input texture among them.
	pub params: NodeValueRow,
	/// The texture the iterative passes feed back into (C++ `ShaderJob::
	/// iterative_input`; empty = the effect input).
	pub iterative_input: String,
}

/// C++ `ColorTransformJob` payload: the OCIO processor application a
/// color node emits at its output. The processor is shared by `Arc` —
/// the node keeps its cached processor while in-flight jobs reference
/// the same immutable instance. The input texture value rides along
/// (C++ `t->to_job(job)` wraps the texture the job applies to).
#[derive(Clone)]
pub struct ColorTransformJobPayload {
	/// The OCIO processor to apply (C++ `ColorTransformJob::processor`).
	pub color_processor: std::sync::Arc<ColorProcessor>,
	/// The input texture value (C++ the texture `to_job` was called on).
	pub input: crate::value::NodeValue,
	/// Request time in media seconds.
	pub time: Rational,
}

impl std::fmt::Debug for ColorTransformJobPayload {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ColorTransformJobPayload")
			.field("input", &self.input)
			.field("time", &self.time)
			.finish_non_exhaustive()
	}
}

/// C++ `CacheJob` payload (`app/render/job/cachejob.h`): read one frame
/// of the disk frame cache. The seam loads the frame stored at `path`
/// (as named by `oakrender::cache::PlaybackCache::frame_cache_path`);
/// when the file is missing or unreadable it substitutes `fallback`,
/// the value the cache node was fed, which the seam has already
/// resolved by the time the load is attempted.
#[derive(Clone, Debug)]
pub struct CacheJobPayload {
	/// Frame-cache file path (C++ `CacheJob::filename()`).
	pub path: String,
	/// Request time in media seconds.
	pub time: Rational,
	/// Value to substitute when the cache file cannot be read (C++
	/// `CacheJob::fallback()`); boxed so the job can carry a
	/// texture-typed value without bloating the enum.
	pub fallback: Box<NodeValue>,
}

impl Default for FootageJobPayload {
	fn default() -> Self {
		FootageJobPayload {
			filename: String::new(),
			stream_index: 0,
			time: Rational::new(0, 1),
		}
	}
}

impl Default for ShaderJobPayload {
	fn default() -> Self {
		ShaderJobPayload {
			node_id: NodeId::INVALID,
			time: Rational::new(0, 1),
			iterations: 1,
			type_id: String::new(),
			shader_id: String::new(),
			effect_input: String::new(),
			params: NodeValueRow::new(),
			iterative_input: String::new(),
		}
	}
}

impl Job {
	/// Borrow the payload when this is a [`Job::FootageJob`].
	pub fn as_footage(&self) -> Option<&FootageJobPayload> {
		match self {
			Job::FootageJob(payload) => Some(payload),
			_ => None,
		}
	}

	/// Borrow the payload when this is a [`Job::ShaderJob`].
	pub fn as_shader(&self) -> Option<&ShaderJobPayload> {
		match self {
			Job::ShaderJob(payload) => Some(payload),
			_ => None,
		}
	}

	/// Borrow the payload when this is a [`Job::PluginJob`].
	pub fn as_plugin(&self) -> Option<&PluginJobPayload> {
		match self {
			Job::PluginJob(payload) => Some(payload),
			_ => None,
		}
	}

	/// Borrow the payload when this is a [`Job::ColorTransformJob`].
	pub fn as_color_transform(&self) -> Option<&ColorTransformJobPayload> {
		match self {
			Job::ColorTransformJob(payload) => Some(payload),
			_ => None,
		}
	}

	/// Borrow the payload when this is a [`Job::CacheJob`].
	pub fn as_cache(&self) -> Option<&CacheJobPayload> {
		match self {
			Job::CacheJob(payload) => Some(payload),
			_ => None,
		}
	}
}

/// Borrow the [`Job`] boxed in `h`; `None` when the handle is empty or
/// boxes anything else (a real texture, say) — the probe the render
/// seam makes on texture-channel values.
///
/// # Safety
/// `h` must be empty or a live handle created by
/// [`crate::handle::make_owned`]/[`crate::handle::make_owned_with`] for
/// the duration of the call.
pub unsafe fn job_ref(h: &crate::handle::CHandle) -> Option<&Job> {
	// SAFETY: the caller guarantees liveness; `get_checked` adds the
	// boxed-type check and returns `None` for empty handles.
	unsafe { crate::handle::get_checked::<Job>(h) }
}

/// Borrow the [`FootageJobPayload`] boxed in `h` (a
/// [`Job::FootageJob`] probe); `None` for empty handles or any other
/// payload.
///
/// # Safety
/// Same contract as [`job_ref`].
pub unsafe fn footage_job(h: &crate::handle::CHandle) -> Option<&FootageJobPayload> {
	unsafe { job_ref(h) }.and_then(Job::as_footage)
}

/// Borrow the [`ShaderJobPayload`] boxed in `h` (a [`Job::ShaderJob`]
/// probe); `None` for empty handles or any other payload.
///
/// # Safety
/// Same contract as [`job_ref`].
pub unsafe fn shader_job(h: &crate::handle::CHandle) -> Option<&ShaderJobPayload> {
	unsafe { job_ref(h) }.and_then(Job::as_shader)
}

/// Borrow the [`PluginJobPayload`] boxed in `h` (a [`Job::PluginJob`]
/// probe); `None` for empty handles or any other payload.
///
/// # Safety
/// Same contract as [`job_ref`].
pub unsafe fn plugin_job(h: &crate::handle::CHandle) -> Option<&PluginJobPayload> {
	unsafe { job_ref(h) }.and_then(Job::as_plugin)
}

/// Borrow the [`ColorTransformJobPayload`] boxed in `h` (a
/// [`Job::ColorTransformJob`] probe); `None` for empty handles or any
/// other payload.
///
/// # Safety
/// Same contract as [`job_ref`].
pub unsafe fn color_transform_job(h: &crate::handle::CHandle) -> Option<&ColorTransformJobPayload> {
	unsafe { job_ref(h) }.and_then(Job::as_color_transform)
}

/// Borrow the [`CacheJobPayload`] boxed in `h` (a [`Job::CacheJob`]
/// probe); `None` for empty handles or any other payload.
///
/// # Safety
/// Same contract as [`job_ref`].
pub unsafe fn cache_job(h: &crate::handle::CHandle) -> Option<&CacheJobPayload> {
	unsafe { job_ref(h) }.and_then(Job::as_cache)
}
