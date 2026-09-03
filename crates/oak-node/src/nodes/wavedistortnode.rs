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

//! Wave distort effect (C++
//! `src/node/src/distort/wave/wavedistortnode.{h,cpp}`,
//! `olive::WaveDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Frequency input id (C++ `k_frequency_input`). Type: float; default
/// `10`.
pub const FREQUENCY_INPUT: &str = "frequency_in";

/// Intensity input id (C++ `k_intensity_input`). Type: float; default
/// `10`.
pub const INTENSITY_INPUT: &str = "intensity_in";

/// Evolution input id (C++ `k_evolution_input`). Type: float; default
/// `0`.
pub const EVOLUTION_INPUT: &str = "evolution_in";

/// Direction combo input id (C++ `k_vertical_input`). Type: combo;
/// default `false` (0 = "Horizontal"); combo strings: "Horizontal",
/// "Vertical".
pub const VERTICAL_INPUT: &str = "vertical_in";

/// Wave distort node. Displaces the image along a sine wave. Has no own
/// member fields in C++ (state lives in the `Node` inputs).
pub struct WaveDistortNode;

/// Fragment shader (C++ loads the `:/shaders/wave.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/wave.frag`.
const SHADER_FRAG: &str = r#"uniform float frequency_in;
uniform float intensity_in;
uniform float evolution_in;
uniform bool vertical_in;

uniform sampler2D tex_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
  vec2 pos = ove_texcoord;

  if (vertical_in) {
    pos.x -= sin((ove_texcoord.y-(evolution_in*0.01))*frequency_in)*intensity_in*0.01;
  } else {
    pos.y -= sin((ove_texcoord.x-(evolution_in*0.01))*frequency_in)*intensity_in*0.01;
  }

  if (pos.x < 0.0 || pos.x >= 1.0 || pos.y < 0.0 || pos.y >= 1.0) {
    discard;
  } else {
    frag_color = texture(tex_in, pos);
  }
}
"#;

impl WaveDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for WaveDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Wave"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.wave"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Distorts an image along a sine wave."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `frequency_in` -> "Frequency", `intensity_in` ->
	/// "Intensity", `evolution_in` -> "Evolution", `vertical_in` ->
	/// "Direction" (combo strings "Horizontal"/"Vertical").
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			FREQUENCY_INPUT => "Frequency",
			INTENSITY_INPUT => "Intensity",
			EVOLUTION_INPUT => "Evolution",
			// The `vertical_in` combo strings "Horizontal"/"Vertical" are a
			// UI-level property of the input (C++ `set_combo_box_strings`).
			VERTICAL_INPUT => "Direction",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `vertical_in` -> "Horizontal",
	/// "Vertical".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			VERTICAL_INPUT => vec!["Horizontal", "Vertical"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// intensity != 0.0 -> shader job over the whole value row rendered
	/// at the texture's own params; intensity == 0.0 -> pass-through
	/// push of the input texture unchanged.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input.
	/// C++ renders the job at the texture's own params and inserts no
	/// `resolution_in` (the wave shader declares none), so the runner has
	/// nothing extra to fill (`// CPP-PARITY: wavedistortnode.cpp` value()).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let tex = match inputs.get(TEXTURE_INPUT) {
			Some(tex @ crate::value::NodeValue::Texture(_)) => tex.clone(),
			_ => return,
		};

		let intensity = match inputs.get(INTENSITY_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(INTENSITY_INPUT, -1, time).to_double(),
		};

		if intensity != 0.0 {
			// The shader-job box (C++ ShaderJob): the behavior's type id
			// selects the fragment source; the effect input key locates the
			// main texture inside the params row.
			table.push(
				crate::value::ValueType::Texture,
				crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
					node_id: crate::id::NodeId::INVALID,
					time,
					iterations: 1,
					type_id: self.type_id().to_string(),
					shader_id: String::new(),
					effect_input: core.effect_input.clone(),
					params: inputs.clone(),
					iterative_input: String::new(),
				})),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and always returns the wave fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(WaveDistortNode))
	}
}

/// Constructor (C++ `WaveDistortNode::WaveDistortNode()`): adds
/// `tex_in`, `frequency_in`, `intensity_in`, `evolution_in` and
/// `vertical_in` with the defaults documented on the constants, sets
/// the video-effect flag and the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	core.add_input(crate::input::Input::new(
		FREQUENCY_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	));
	core.add_input(crate::input::Input::new(
		INTENSITY_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	));
	core.add_input(crate::input::Input::new(
		EVOLUTION_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.add_input(crate::input::Input::new(
		VERTICAL_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(WaveDistortNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	#[test]
	fn input_names() {
		let n = WaveDistortNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(FREQUENCY_INPUT), "Frequency");
		assert_eq!(n.input_name(INTENSITY_INPUT), "Intensity");
		assert_eq!(n.input_name(EVOLUTION_INPUT), "Evolution");
		assert_eq!(n.input_name(VERTICAL_INPUT), "Direction");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.wave");
		assert_eq!(
			core.get_input(FREQUENCY_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(
			core.get_input(INTENSITY_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(
			core.get_input(VERTICAL_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
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
	fn value_zero_intensity_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(INTENSITY_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_nonzero_intensity_pushes_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(INTENSITY_INPUT.to_string(), NodeValue::Float(10.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
			.expect("wave output boxes a ShaderJobPayload");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.wave");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn shader_code_returns_wave_shader() {
		let n = WaveDistortNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform float frequency_in;"));
		assert!(code.contains("if (vertical_in)"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Wave");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.wave`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.wave",
		name: "Wave",
		categories: &[Category::Distort],
		create,
	});
}
