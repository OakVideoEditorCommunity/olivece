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

//! Noise generator (C++ `src/node/src/generator/noise/noise.{h,cpp}`,
//! `olive::NoiseGeneratorNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Base texture input id (C++ `k_base_in`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const BASE_INPUT: &str = "base_in";

/// Color noise toggle input id (C++ `k_color_input`). Type: bool;
/// default `false`.
pub const COLOR_INPUT: &str = "color_in";

/// Noise strength input id (C++ `k_strength_input`). Type: float;
/// default `0.2`; properties: `view = percentage`, `min = 0`.
pub const STRENGTH_INPUT: &str = "strength_in";

/// Noise generator node. Adds gold-noise to an optional base texture
/// (or generates it standalone). Has no own member fields in C++.
pub struct NoiseGeneratorNode;

/// Fragment shader (C++ loads the `:/shaders/noise.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/noise.frag`.
const SHADER_FRAG: &str = r#"uniform float time_in;

uniform float strength_in;
uniform bool color_in;

uniform sampler2D base_in;
uniform bool base_in_enabled;

in vec2 ove_texcoord;
out vec4 frag_color;

float PHI = 1.61803398874989484820459 * 00000.1; // Golden Ratio
float PI  = 3.14159265358979323846264 *  00000.1; // PI
float SQ2 = 1.41421356237309504880169 * 10000.0; // Square Root of Two

bool isNan( float val )
{
  return ( val < 0.0 || 0.0 < val || val == 0.0 ) ? false : true;
  // important: some nVidias failed to cope with version below.
  // Probably wrong optimization.
  /*return ( val <= 0.0 || 0.0 <= val ) ? false : true;*/

  // Taken from: https://stackoverflow.com/questions/11810158/how-to-deal-with-nan-or-inf-in-opengl-es-2-0-shaders
}

float gold_noise(vec2 coordinate, float seed){
  float value = fract(tan(distance(coordinate*(seed+PHI), vec2(PHI, PI)))*SQ2)*(strength_in);
  return isNan(value) ? 0.0 : value;
}

void main(void) {
  vec3 noise;
  if (color_in) {
    noise = vec3(gold_noise(ove_texcoord, time_in + 42069.0), gold_noise(ove_texcoord, time_in + 69220.0), gold_noise(ove_texcoord, time_in + 1337.0));
  } else {
    noise = vec3(gold_noise(ove_texcoord, time_in + 69420.0));
  }

  if (base_in_enabled) {
    vec4 base = texture(base_in, ove_texcoord);
    base.rgb += noise;
    frag_color = base;
  } else {
    frag_color = vec4(noise, 1.0);
  }
}
"#;

impl NoiseGeneratorNode {
	/// Fragment shader for all shader requests (C++
	/// `get_shader_code()` ignores the request id).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for NoiseGeneratorNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Noise"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.noise"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Generates noise patterns"
	}

	/// Localized input names (C++ `retranslate()`): `base_in` ->
	/// "Base", `strength_in` -> "Strength", `color_in` -> "Color".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			BASE_INPUT => "Base",
			STRENGTH_INPUT => "Strength",
			COLOR_INPUT => "Color",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): always pushes a shader job —
	/// with a base texture connected it runs at the base's params,
	/// otherwise at the sequence params. The params row is the whole
	/// input row plus `time_in` (the request time in seconds as a
	/// float, C++ `globals.time().in().toDouble()`).
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the runner
	/// fills the shader's `base_in_enabled` flag from the presence of
	/// the `base_in` texture (`// CPP-PARITY: noise.cpp` `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mut params = inputs.clone();
		params.insert(
			"time_in".to_string(),
			crate::value::NodeValue::Float(time.to_f64()),
		);
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input: String::new(),
			})),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): returns the
	/// noise fragment shader for any request id.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(NoiseGeneratorNode))
	}
}

/// Constructor (C++ `NoiseGeneratorNode::NoiseGeneratorNode()`): adds
/// `base_in`, `strength_in` and `color_in` with the defaults, flags and
/// properties documented on the constants, sets the video-effect flag
/// and makes `base_in` the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut base = crate::input::Input::new(
		BASE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);

	let mut strength = crate::input::Input::new(
		STRENGTH_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.2),
	);
	strength.properties = vec![
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
	];
	core.add_input(strength);

	core.add_input(crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = BASE_INPUT.to_string();

	(core, Box::new(NoiseGeneratorNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = NoiseGeneratorNode;
		assert_eq!(n.input_name(BASE_INPUT), "Base");
		assert_eq!(n.input_name(STRENGTH_INPUT), "Strength");
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.noise");
		assert_eq!(
			core.get_input(STRENGTH_INPUT).unwrap().default,
			NodeValue::Float(0.2)
		);
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(core.effect_input, BASE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn value_pushes_shader_job() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(3, 1),
			&mut table,
		);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let job =
			unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
				.expect("noise output boxes a ShaderJobPayload");
		assert_eq!(job.type_id, "org.olivevideoeditor.Olive.noise");
		assert_eq!(job.shader_id, "");
		assert_eq!(job.iterations, 1);
		assert_eq!(job.effect_input, BASE_INPUT);
		// The params row carries the request time as the `time_in` float
		// (C++ `globals.time().in().toDouble()`).
		assert_eq!(job.params.get("time_in").map(|v| v.to_double()), Some(3.0));
	}

	#[test]
	fn value_with_base_texture_pushes_shader_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(
			BASE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let job =
			unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
				.expect("noise output boxes a ShaderJobPayload");
		assert_eq!(job.type_id, "org.olivevideoeditor.Olive.noise");
		assert!(job.params.contains_key(BASE_INPUT));
		assert!(job.params.contains_key("time_in"));
	}

	#[test]
	fn shader_code_returns_noise_shader() {
		let n = NoiseGeneratorNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform float time_in;"));
		assert!(code.contains("gold_noise"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Noise");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.noise`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.noise",
		name: "Noise",
		categories: &[Category::Generator],
		create,
	});
}
