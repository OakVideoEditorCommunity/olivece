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

//! Edge detect filter (clean-room reimplementation of the CImg
//! `EdgeDetect`/`eu.cimg.EdgeDetect` effect; `ofx-misc` used for
//! parameter semantics only, no code copied).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Threshold input id. Type: float; default `0.0`; properties: `min =
/// 0.0`. Gradient magnitudes below this value output black.
pub const THRESHOLD_INPUT: &str = "threshold_in";

/// Edge detect filter node. Sobel gradient magnitude, per RGB channel.
pub struct EdgeDetectNode;

/// Fragment shader: 3x3 Sobel gradient, per RGB channel; the magnitude is
/// `sqrt(gx^2 + gy^2)`, zeroed where it falls below `threshold_in` (a
/// branch-free `step`, no `switch`). The alpha channel passes through
/// unchanged so the result stays a visible, composable image; magnitudes
/// are not clamped (the filter is meant to feed a grade downstream).
/// Pixel offsets are one texel (`resolution_in`), sampling at texel
/// centers; edge-of-frame taps clamp to the border pixel.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float threshold_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

vec3 edge_sample(vec2 uv) {
  return texture(tex_in, uv).rgb;
}

void main() {
  vec2 texel = vec2(1.0) / resolution_in;

  vec3 tl = edge_sample(ove_texcoord + texel * vec2(-1.0, -1.0));
  vec3 tc = edge_sample(ove_texcoord + texel * vec2( 0.0, -1.0));
  vec3 tr = edge_sample(ove_texcoord + texel * vec2( 1.0, -1.0));
  vec3 ml = edge_sample(ove_texcoord + texel * vec2(-1.0,  0.0));
  vec3 mr = edge_sample(ove_texcoord + texel * vec2( 1.0,  0.0));
  vec3 bl = edge_sample(ove_texcoord + texel * vec2(-1.0,  1.0));
  vec3 bc = edge_sample(ove_texcoord + texel * vec2( 0.0,  1.0));
  vec3 br = edge_sample(ove_texcoord + texel * vec2( 1.0,  1.0));

  vec3 gx = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
  vec3 gy = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
  vec3 magnitude = sqrt(gx * gx + gy * gy);

  vec3 masked = magnitude * step(vec3(threshold_in), magnitude);
  frag_color = vec4(masked, texture(tex_in, ove_texcoord).a);
}
"#;

impl EdgeDetectNode {
	/// Fragment shader for any request (this node has a single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for EdgeDetectNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Edge Detect"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.edgedetect"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description.
	fn description(&self) -> &str {
		"Detect edges with a Sobel gradient filter."
	}

	/// Localized input names: `tex_in` -> "Input", `threshold_in` ->
	/// "Threshold".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			THRESHOLD_INPUT => "Threshold",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; otherwise push the
	/// Sobel shader job (threshold rides in the params row as the
	/// `threshold_in` uniform).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		use crate::value::{NodeValue, ValueType};

		match inputs.get(TEXTURE_INPUT) {
			Some(NodeValue::Texture(_)) => {}
			_ => return,
		}

		let threshold = match inputs.get(THRESHOLD_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(THRESHOLD_INPUT, -1, time).to_double(),
		};

		let mut params = inputs.clone();
		params.insert(THRESHOLD_INPUT.to_string(), NodeValue::Float(threshold));

		table.push(
			ValueType::Texture,
			NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input: String::new(),
			}))),
			None,
		);
	}

	/// Shader code request: always the one fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(EdgeDetectNode))
	}
}

/// Constructor: adds `tex_in` and `threshold_in` with the defaults and
/// properties documented on the constants, sets the video-effect flag and
/// the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut threshold = crate::input::Input::new(
		THRESHOLD_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	threshold.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(threshold);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(EdgeDetectNode))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn run(core: &NodeCore, inputs: &crate::value::NodeValueRow) -> ShaderJobPayload {
		let behavior = EdgeDetectNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	#[test]
	fn input_names() {
		let n = EdgeDetectNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(THRESHOLD_INPUT), "Threshold");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.edgedetect");
		assert_eq!(behavior.categories(), &[Category::Filter]);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		let threshold = core.get_input(THRESHOLD_INPUT).expect("threshold input");
		assert_eq!(threshold.default, NodeValue::Float(0.0));
		assert!(threshold
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty());
	}

	#[test]
	fn value_pushes_job_with_threshold_default() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.edgedetect");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(
			payload.params.get(THRESHOLD_INPUT),
			Some(&NodeValue::Float(0.0))
		);
	}

	#[test]
	fn value_takes_threshold_from_row() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(THRESHOLD_INPUT.to_string(), NodeValue::Float(0.5)),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(THRESHOLD_INPUT),
			Some(&NodeValue::Float(0.5))
		);
	}

	#[test]
	fn value_takes_threshold_from_core() {
		let (mut core, _) = create();
		core.set_standard_value(THRESHOLD_INPUT, -1, NodeValue::Float(0.75));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(THRESHOLD_INPUT),
			Some(&NodeValue::Float(0.75))
		);
	}

	#[test]
	fn shader_code_is_sobel_without_switch() {
		let n = EdgeDetectNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform vec2 resolution_in;"));
		assert!(glsl.contains("sqrt(gx * gx + gy * gy)"));
		assert!(glsl.contains("step(vec3(threshold_in), magnitude)"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Edge Detect");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.edgedetect",
		name: "Edge Detect",
		categories: &[Category::Filter],
		create,
	});
}
