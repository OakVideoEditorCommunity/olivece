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

//! Flip distort effect (C++
//! `src/node/src/distort/flip/flipdistortnode.{h,cpp}`,
//! `olive::FlipDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Horizontal flip input id (C++ `k_horizontal_input`). Type: bool;
/// default `false`.
pub const HORIZONTAL_INPUT: &str = "horiz_in";

/// Vertical flip input id (C++ `k_vertical_input`). Type: bool;
/// default `false`.
pub const VERTICAL_INPUT: &str = "vert_in";

/// Flip distort node. Mirrors the image horizontally and/or vertically.
/// Has no own member fields in C++ (state lives in the `Node` inputs).
pub struct FlipDistortNode;

/// Fragment shader (C++ loads the `:/shaders/flip.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/flip.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform bool horiz_in;
uniform bool vert_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    if (!horiz_in && !vert_in) {
        frag_color = texture(tex_in, ove_texcoord);
        return;
    }

    vec2 new_coord = ove_texcoord;

    if (horiz_in) new_coord.x = 1.0 - new_coord.x;
    if (vert_in) new_coord.y = 1.0 - new_coord.y;

    frag_color = texture(tex_in, new_coord);
}
"#;

impl FlipDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for FlipDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Flip"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.flip"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Flips an image horizontally or vertically"
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `horiz_in` -> "Horizontal", `vert_in` -> "Vertical".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			HORIZONTAL_INPUT => "Horizontal",
			VERTICAL_INPUT => "Vertical",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// either flip flag set -> shader job over the whole value row;
	/// neither set -> pass-through push of the input texture unchanged.
	///
	/// The job case boxes a [`ShaderJobPayload`] that the renderer's
	/// resolve hook executes and replaces with the result texture; the
	/// params row carries the input texture and uniforms, keyed by the
	/// effect input (`// CPP-PARITY: flipdistortnode.cpp` `value()`).
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

		let horiz = match inputs.get(HORIZONTAL_INPUT) {
			Some(v) => v.to_double() != 0.0,
			None => core.value_at_time(HORIZONTAL_INPUT, -1, time).to_double() != 0.0,
		};
		let vert = match inputs.get(VERTICAL_INPUT) {
			Some(v) => v.to_double() != 0.0,
			None => core.value_at_time(VERTICAL_INPUT, -1, time).to_double() != 0.0,
		};

		if horiz || vert {
			// The shader-job box (C++ `tex->toJob(ShaderJob(value))`): the
			// behavior's type id selects the fragment source, and the effect
			// input key locates the main texture inside the params row.
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
					iterative_input: TEXTURE_INPUT.to_string(),
				})),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and always returns the flip fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(FlipDistortNode))
	}
}

/// Constructor (C++ `FlipDistortNode::FlipDistortNode()`): adds
/// `tex_in`, `horiz_in` and `vert_in` with the defaults and flags
/// documented on the constants, sets the video-effect flag and the
/// effect input.
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
		HORIZONTAL_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		VERTICAL_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(FlipDistortNode))
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
		let n = FlipDistortNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(HORIZONTAL_INPUT), "Horizontal");
		assert_eq!(n.input_name(VERTICAL_INPUT), "Vertical");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.flip");
		assert_eq!(
			core.get_input(HORIZONTAL_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(
			core.get_input(VERTICAL_INPUT).unwrap().default,
			NodeValue::Boolean(false)
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
	fn value_no_flip_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(HORIZONTAL_INPUT, -1, NodeValue::Boolean(false));
		core.set_standard_value(VERTICAL_INPUT, -1, NodeValue::Boolean(false));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_flip_pushes_shader_job_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(VERTICAL_INPUT, -1, NodeValue::Boolean(true));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(&handle) }
			.expect("shader job payload expected");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.flip");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.time, Rational::new(0, 1));
		assert!(payload.params.contains_key(TEXTURE_INPUT));
	}

	#[test]
	fn value_connected_horizontal_flip_pushes_deferred_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(HORIZONTAL_INPUT.to_string(), NodeValue::Boolean(true)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(&handle) }
			.expect("shader job payload expected");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.flip");
		assert_eq!(payload.iterations, 1);
	}

	#[test]
	fn shader_code_returns_flip_shader() {
		let n = FlipDistortNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform sampler2D tex_in;"));
		assert!(code.contains("if (horiz_in) new_coord.x = 1.0 - new_coord.x;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Flip");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.flip`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.flip",
		name: "Flip",
		categories: &[Category::Distort],
		create,
	});
}
