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

//! Despill effect (C++ `src/node/src/keying/despill/despill.{h,cpp}`,
//! `olive::DespillNode`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Key color input id (C++ `k_color_input`). Type: combo; default `0`;
/// combo strings: "Green", "Blue" (set in `retranslate()`).
pub const COLOR_INPUT: &str = "color_in";

/// Despill method input id (C++ `k_method_input`). Type: combo;
/// default `0`; combo strings: "Average", "Double Red Average",
/// "Double Average", "Limit" (set in `retranslate()`).
pub const METHOD_INPUT: &str = "method_in";

/// Preserve-luminance toggle input id (C++
/// `k_preserve_luminance_input`; note the value ends in `_input`, not
/// `_in`). Type: boolean; default `false`.
pub const PRESERVE_LUMINANCE_INPUT: &str = "preserve_luminance_input";

/// Luma coefficients uniform id (C++ uses the `luma_coeffs` literal in
/// `value()`). Not a declared node input — it is the shader uniform name
/// fed per frame in `value()` with the project color manager's default
/// luma coefficients (Rec. 709 `{0.2126, 0.7152, 0.0722}` fallback).
/// Type: vec3.
pub const LUMA_COEFFS_INPUT: &str = "luma_coeffs";

/// Despill node: removes green/blue screen spill from the keyed
/// foreground using one of several channel-averaging methods. The C++
/// class has no own members.
pub struct DespillNode;

/// Fragment shader (C++ loads the `:/shaders/despill.frag` resource in
/// `get_shader_code`). Text adapted from
/// `engine/shaders/despill.frag`: the C++ `switch (method_in)`
/// dispatch is spelled as if/else chains because naga's WGSL emitter
/// rejects fall-through-capable GLSL switch blocks. The `luma_coeffs`
/// uniform is not a node input — it is injected into the shader job by
/// `value()`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform int color_in;
uniform int method_in;
uniform bool preserve_luminance_input;
uniform vec3 luma_coeffs;

in vec2 ove_texcoord;
out vec4 frag_color;

#define AVERAGE             0
#define DOUBLE_RED_AVERAGE  1
#define DOUBLE_AVERAGE      2
#define BLUE_LIMIT          3

void main(void) {
    vec4 original_col = texture(tex_in, ove_texcoord);
    vec4 tex_col = original_col;
    float color_average = 0.0;

    if(color_in == 0) { // Green screen
        if (method_in == AVERAGE) {
            color_average = dot(tex_col.rb, vec2(0.5)); // (tex_col.r + tex_col.b) / 2.0
            tex_col.g = tex_col.g > color_average ? color_average: tex_col.g;
        } else if (method_in == DOUBLE_RED_AVERAGE) {
            color_average = dot(tex_col.rb, vec2(2.0, 1.0) / 3.0); // (2.0 * tex_col.r + tex_col.b) / 3.0
            tex_col.g = tex_col.g > color_average ? color_average : tex_col.g;
        } else if (method_in == DOUBLE_AVERAGE) {
            color_average = dot(tex_col.br, vec2(2.0, 1.0) / 3.0); // (2.0 * tex_col.b + tex_col.r) / 3.0
            tex_col.g = tex_col.g > color_average ? color_average : tex_col.g;
        } else if (method_in == BLUE_LIMIT) {
            tex_col.g = tex_col.g > tex_col.b ? tex_col.b : tex_col.g;
        }
    } else { // Blue screen
        if (method_in == AVERAGE) {
            color_average = dot(tex_col.rg, vec2(0.5)); // (tex_col.r + tex_col.g) / 2.0
            tex_col.b = tex_col.b > color_average ? color_average : tex_col.b;
        } else if (method_in == DOUBLE_RED_AVERAGE) {
            color_average = dot(tex_col.rg, vec2(2.0, 1.0) / 3.0); // (2.0 * tex_col.r + tex_col.g) / 3.0
            tex_col.b = tex_col.b > color_average ? color_average : tex_col.b;
        } else if (method_in == DOUBLE_AVERAGE) {
            color_average = dot(tex_col.gr, vec2(2.0, 1.0) / 3.0); // (2.0 * tex_col.g+ tex_col.r) / 3.0
            tex_col.b = tex_col.b > color_average ? color_average : tex_col.b;
        } else if (method_in == BLUE_LIMIT) {
            tex_col.b = tex_col.b > tex_col.g ? tex_col.g : tex_col.b;
        }
    }

    if (preserve_luminance_input) {
        vec4 diff = original_col - tex_col;
        float luma = dot(abs(diff.rgb), luma_coeffs);
        tex_col.rgb += vec3(luma);
    }

    frag_color = tex_col;
}
"#;

impl DespillNode {
	/// Fragment shader (C++ `get_shader_code()`; the request is
	/// ignored — there is a single shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for DespillNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Despill"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.despill"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Keying]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Selection of simple despill operations"
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `color_in` -> "Key Color" (combo strings
	/// "Green"/"Blue"), `method_in` -> "Method" (combo strings
	/// "Average"/"Double Red Average"/"Double Average"/"Limit"),
	/// `preserve_luminance_input` -> "Preserve Luminance".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			COLOR_INPUT => "Key Color",
			METHOD_INPUT => "Method",
			PRESERVE_LUMINANCE_INPUT => "Preserve Luminance",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `color_in` -> "Green", "Blue";
	/// `method_in` -> "Average", "Double Red Average", "Double Average",
	/// "Limit".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			COLOR_INPUT => vec!["Green", "Blue"],
			METHOD_INPUT => vec!["Average", "Double Red Average", "Double Average", "Limit"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): builds a `ShaderJob` from
	/// the whole input row, then inserts a `luma_coeffs` vec3 taken
	/// from the project's color manager default luma coefficients
	/// (falling back to Rec.709 `0.2126/0.7152/0.0722` when there is
	/// no project or color manager); pushes the job only when
	/// `tex_in` holds a texture.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		match inputs.get(TEXTURE_INPUT) {
			Some(crate::value::NodeValue::Texture(_)) => {}
			_ => return,
		}

		// `// CPP-PARITY: despill.cpp` `value()` — the C++ inserts
		// `luma_coeffs` (Rec. 709 {0.2126, 0.7152, 0.0722}, or the project
		// color manager's default luma coefficients when one is attached —
		// the Rust model has no project/manager access, so the fallback
		// always applies) into a ShaderJob over the whole input row and
		// pushes `tex->to_job(job)`. The job is boxed here as a
		// [`ShaderJobPayload`] that the renderer's resolve hook executes
		// and replaces with the result texture; the params row carries the
		// luma coefficients under the shader uniform name.
		let mut params = inputs.clone();
		params.insert(
			LUMA_COEFFS_INPUT.to_string(),
			crate::value::NodeValue::Vec3([0.2126, 0.7152, 0.0722]),
		);
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
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

	/// Shader code request (C++ `get_shader_code()`): returns the
	/// single fragment shader regardless of the request id.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(DespillNode))
	}
}

/// Constructor (C++ `DespillNode::DespillNode()`): adds `tex_in`,
/// `color_in`, `method_in`, and `preserve_luminance_input` with the
/// defaults documented on the constants, sets the video-effect flag,
/// and makes `tex_in` the effect input.
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
		COLOR_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));
	core.add_input(crate::input::Input::new(
		METHOD_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));
	core.add_input(crate::input::Input::new(
		PRESERVE_LUMINANCE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(DespillNode))
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.despill`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.despill",
		name: "Despill",
		categories: &[Category::Keying],
		create,
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = DespillNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(COLOR_INPUT), "Key Color");
		assert_eq!(n.input_name(METHOD_INPUT), "Method");
		assert_eq!(n.input_name(PRESERVE_LUMINANCE_INPUT), "Preserve Luminance");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_defaults() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.despill");
		assert_ne!(
			core.get_input(TEXTURE_INPUT).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(
			core.get_input(METHOD_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(
			core.get_input(PRESERVE_LUMINANCE_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn shader_code_returns_despill_frag() {
		let code = DespillNode.shader_code("anything").unwrap();
		assert!(code.contains("color_average = dot(tex_col.rb, vec2(0.5));"));
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
	fn value_with_texture_pushes_shader_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(
			TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			panic!("expected a texture-typed value");
		};
		let payload =
			unsafe { crate::jobs::shader_job(handle) }
				.expect("payload boxed behind the handle");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.despill");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		// The luma coefficients are injected under the shader uniform name
		// (C++ `job.Insert("luma_coeffs", ...)`).
		assert_eq!(
			payload.params.get(LUMA_COEFFS_INPUT),
			Some(&NodeValue::Vec3([0.2126, 0.7152, 0.0722]))
		);
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Despill");
	}
}
