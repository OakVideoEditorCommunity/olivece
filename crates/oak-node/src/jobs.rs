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
use crate::value::NodeValueRow;

/// Job types
///
/// The payloads travel boxed inside `Texture` values during graph
/// evaluation (the renderer probes the box's payload type at resolve
/// time); the graph-shaped job model lands in v0.6.

pub enum Job{
	FootageJob(FootageJobPayload),
	ShaderJob(ShaderJobPayload),
	PluginJob(PluginJobPayload),
	ColorTransformJob(ColorTransformJobPayload)
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
pub struct ColorTransformJobPayload{
	/// The OCIO processor to apply (C++ `ColorTransformJob::processor`).
	pub color_processor: std::sync::Arc<ColorProcessor>,
	/// The input texture value (C++ the texture `to_job` was called on).
	pub input: crate::value::NodeValue,
	/// Request time in media seconds.
	pub time: Rational,
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
