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

//! Color Difference Key effect (C++
//! `src/node/src/keying/colordifferencekey/colordifferencekey.{h,cpp}`,
//! `olive::ColorDifferenceKeyNode`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Garbage matte texture input id (C++ `k_garbage_matte_input`). Type:
/// texture; flags: not-keyframable.
pub const GARBAGE_MATTE_INPUT: &str = "garbage_in";

/// Core matte texture input id (C++ `k_core_matte_input`). Type:
/// texture; flags: not-keyframable.
pub const CORE_MATTE_INPUT: &str = "core_in";

/// Key color input id (C++ `k_color_input`). Type: combo; default `0`;
/// combo strings: "Green", "Blue" (set in `retranslate()`).
pub const COLOR_INPUT: &str = "color_in";

/// Shadows input id (C++ `k_shadows_input`). Type: float; default
/// `1.0`; properties: `min = 0.0`, `base = 0.01`.
pub const SHADOWS_INPUT: &str = "shadows_in";

/// Highlights input id (C++ `k_highlights_input`). Type: float;
/// default `1.0`; properties: `min = 0.0`, `base = 0.01`.
pub const HIGHLIGHTS_INPUT: &str = "highlights_in";

/// Show-mask-only toggle input id (C++ `k_mask_only_input`). Type:
/// boolean; default `false`.
pub const MASK_ONLY_INPUT: &str = "mask_only_in";

/// Color difference key node: keys on how far one channel (green or
/// blue) stands out from the other two, with optional garbage/core
/// mattes. The C++ class has no own members.
pub struct ColorDifferenceKeyNode;

/// Fragment shader (C++ loads the `:/shaders/colordifferencekey.frag`
/// resource in `get_shader_code`). Text copied verbatim from
/// `engine/shaders/colordifferencekey.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform sampler2D garbage_in;
uniform sampler2D core_in;
uniform int color_in;
uniform bool garbage_in_enabled;
uniform bool core_in_enabled;
uniform float highlights_in;
uniform float shadows_in;
uniform bool mask_only_in;

in vec2 ove_texcoord;
out vec4 frag_color;

#define SCREEN_COLOR_GREEN 0
#define SCREEN_COLOR_BLUE  1

void main(void) {
    vec4 tex_col = texture(tex_in, ove_texcoord);

    // Unassociate RGB before calculating values
    vec4 unassoc = tex_col;
    if (unassoc.a > 0) {
      unassoc.rgb /= unassoc.a;
    }

    // Simple keyer, generates a inverted mask (background is white, foreground black)
    float mask;
    if (color_in == SCREEN_COLOR_GREEN) {
      mask = (unassoc.g - max(unassoc.r, unassoc.b));
    } else{ // Assume SCREEN_COLOR_BLUE
      mask = (unassoc.b - max(unassoc.r, unassoc.g));
    }

    mask = clamp(mask, 0.0, 1.0);

    if (garbage_in_enabled) {
      // Force anything we want to remove to be 1.0
      vec4 garbage = texture(garbage_in, ove_texcoord);
      // Assumes garbage is achromatic
      mask += garbage.r;
      mask = clamp(mask, 0.0, 1.0);
    }

    if (core_in_enabled) {
      // Force anything we want to keep to be 0.1
      vec3 core = texture(core_in, ove_texcoord).rgb;
      vec3 core_invert = 1.0 - core.rgb;
      // Assumes core is achromatic
      mask *= core_invert.r;
      mask = clamp(mask, 0.0, 1.0);
    }

    // Crush blacks and push whites
    mask = highlights_in * (shadows_in * mask - 1.0) + 1.0;
    mask = clamp(mask, 0.0, 1.0);

    // Invert mask
    mask = 1.0 - mask;

    // Multiply color by mask
    tex_col *= mask;

    if (!mask_only_in) {
        frag_color = tex_col;
    } else {
        frag_color = vec4(vec3(mask), 1.0);
    }
}
"#;

impl ColorDifferenceKeyNode {
	/// Fragment shader (C++ `get_shader_code()`; the request is
	/// ignored — there is a single shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for ColorDifferenceKeyNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Color Difference Key"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.colordifferencekey"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Keying]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"A simple color key based on the distance of one color from other colors."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `garbage_in` -> "Garbage Matte", `core_in` ->
	/// "Core Matte", `color_in` -> "Key Color" (combo strings
	/// "Green"/"Blue"), `shadows_in` -> "Shadows", `highlights_in`
	/// -> "Highlights", `mask_only_in` -> "Show Mask Only".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			GARBAGE_MATTE_INPUT => "Garbage Matte",
			CORE_MATTE_INPUT => "Core Matte",
			COLOR_INPUT => "Key Color",
			SHADOWS_INPUT => "Shadows",
			HIGHLIGHTS_INPUT => "Highlights",
			MASK_ONLY_INPUT => "Show Mask Only",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `color_in` -> "Green", "Blue".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			COLOR_INPUT => vec!["Green", "Blue"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture on `tex_in` ->
	/// push nothing; texture present -> push a `ShaderJob` with the
	/// whole input row inserted.
	///
	/// The job is boxed here as a [`ShaderJobPayload`] that the
	/// renderer's resolve hook executes and replaces with the result
	/// texture (exactly like despill). The previous null-handle
	/// "deferred job" marker was never resolved by the Rust renderer,
	/// so the node's output table handed the downstream clip a null
	/// texture and the rendered frame lost the whole clip.
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
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params: inputs.clone(),
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
		Some(Box::new(ColorDifferenceKeyNode))
	}
}

/// Constructor (C++ `ColorDifferenceKeyNode::ColorDifferenceKeyNode()`):
/// adds `tex_in`, `garbage_in`, `core_in`, `color_in`,
/// `highlights_in`, `shadows_in`, and `mask_only_in` with the defaults
/// and properties documented on the constants, sets the video-effect
/// flag, and makes `tex_in` the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut texture_input = |id: &str| {
		let mut input = crate::input::Input::new(
			id,
			crate::value::ValueType::Texture,
			crate::value::NodeValue::None,
		);
		input.flags |= crate::input::flags::NOT_KEYFRAMABLE;
		core.add_input(input);
	};
	texture_input(TEXTURE_INPUT);
	texture_input(GARBAGE_MATTE_INPUT);
	texture_input(CORE_MATTE_INPUT);

	core.add_input(crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));

	let mut highlights = crate::input::Input::new(
		HIGHLIGHTS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	highlights.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	core.add_input(highlights);

	let mut shadows = crate::input::Input::new(
		SHADOWS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	shadows.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	core.add_input(shadows);

	core.add_input(crate::input::Input::new(
		MASK_ONLY_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(ColorDifferenceKeyNode))
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.colordifferencekey`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.colordifferencekey",
		name: "Color Difference Key",
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
		let n = ColorDifferenceKeyNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(GARBAGE_MATTE_INPUT), "Garbage Matte");
		assert_eq!(n.input_name(CORE_MATTE_INPUT), "Core Matte");
		assert_eq!(n.input_name(COLOR_INPUT), "Key Color");
		assert_eq!(n.input_name(SHADOWS_INPUT), "Shadows");
		assert_eq!(n.input_name(HIGHLIGHTS_INPUT), "Highlights");
		assert_eq!(n.input_name(MASK_ONLY_INPUT), "Show Mask Only");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.colordifferencekey"
		);
		for id in [TEXTURE_INPUT, GARBAGE_MATTE_INPUT, CORE_MATTE_INPUT] {
			assert_ne!(
				core.get_input(id).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
				0
			);
		}
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(
			core.get_input(SHADOWS_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		assert_eq!(
			core.get_input(HIGHLIGHTS_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		assert_eq!(
			core.get_input(MASK_ONLY_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn shader_code_returns_colordifferencekey_frag() {
		let code = ColorDifferenceKeyNode.shader_code("anything").unwrap();
		assert!(code.contains("mask = (unassoc.g - max(unassoc.r, unassoc.b));"));
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
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.colordifferencekey");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert!(payload.params.contains_key(TEXTURE_INPUT));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Color Difference Key");
	}
}
