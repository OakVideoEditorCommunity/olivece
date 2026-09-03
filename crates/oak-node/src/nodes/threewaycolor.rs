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

//! Three-way color corrector node (C++
//! `src/node/src/color/threewaycolor/threewaycolor.{h,cpp}`,
//! `olive::ThreeWayColorNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Shadows color input id (C++ `k_shadows_color_input`). Type: color;
/// default neutral gray `{0.5, 0.5, 0.5, 1.0}`.
pub const SHADOWS_COLOR_INPUT: &str = "shadows_color_in";

/// Midtones color input id (C++ `k_midtones_color_input`). Type: color;
/// default neutral gray `{0.5, 0.5, 0.5, 1.0}`.
pub const MIDTONES_COLOR_INPUT: &str = "midtones_color_in";

/// Highlights color input id (C++ `k_highlights_color_input`). Type:
/// color; default neutral gray `{0.5, 0.5, 0.5, 1.0}`.
pub const HIGHLIGHTS_COLOR_INPUT: &str = "highlights_color_in";

/// Shadows amount input id (C++ `k_shadows_amount_input`). Type: float;
/// default `1.0`; properties: `min = 0.0`, `view = percentage`.
pub const SHADOWS_AMOUNT_INPUT: &str = "shadows_amount_in";

/// Midtones amount input id (C++ `k_midtones_amount_input`). Type:
/// float; default `1.0`; properties: `min = 0.0`, `view = percentage`.
pub const MIDTONES_AMOUNT_INPUT: &str = "midtones_amount_in";

/// Highlights amount input id (C++ `k_highlights_amount_input`). Type:
/// float; default `1.0`; properties: `min = 0.0`, `view = percentage`.
pub const HIGHLIGHTS_AMOUNT_INPUT: &str = "highlights_amount_in";

/// Luma coefficients input id (C++ `k_luma_coefficients_input`). Not a
/// declared node input — the C++ never calls `add_input` for it; it is
/// the shader uniform name fed per frame in `value()` from the project
/// color manager's default luma coefficients (Rec. 709
/// `{0.2126, 0.7152, 0.0722}` fallback). Type: vec3.
pub const LUMA_COEFFICIENTS_INPUT: &str = "luma_coefficients_in";

/// Three-way color corrector node. Adjusts shadows, midtones, and
/// highlights separately. The C++ class has no own private members, so
/// this is a unit-like struct (caches/inputs live in `NodeCore`).
pub struct ThreeWayColorNode;

/// Fragment shader (C++ `get_shader_code` loads the
/// `:/shaders/threewaycolor.frag` resource). Text copied verbatim from
/// `engine/shaders/threewaycolor.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;

uniform vec4 shadows_color_in;
uniform vec4 midtones_color_in;
uniform vec4 highlights_color_in;
uniform float shadows_amount_in;
uniform float midtones_amount_in;
uniform float highlights_amount_in;
uniform vec3 luma_coefficients_in;

in vec2 ove_texcoord;
out vec4 frag_color;

vec3 color_offset(vec4 control, float amount)
{
    return (control.rgb - vec3(0.5)) * 2.0 * amount;
}

void main(void)
{
    vec4 source = texture(tex_in, ove_texcoord);
    float luma = clamp(dot(source.rgb, luma_coefficients_in), 0.0, 1.0);

    float shadow_weight = smoothstep(0.75, 0.0, luma);
    float highlight_weight = smoothstep(0.25, 1.0, luma);
    float midtone_weight = clamp(1.0 - abs(luma - 0.5) * 2.0, 0.0, 1.0);

    vec3 adjustment =
        color_offset(shadows_color_in, shadows_amount_in) * shadow_weight +
        color_offset(midtones_color_in, midtones_amount_in) * midtone_weight +
        color_offset(highlights_color_in, highlights_amount_in) * highlight_weight;

    vec3 graded = source.rgb + adjustment * source.rgb * (1.0 - source.rgb);
    frag_color = vec4(clamp(graded, 0.0, 1.0), source.a);
}
"#;

impl ThreeWayColorNode {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns this shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for ThreeWayColorNode {
	/// Human-readable name (C++ `name()`, inline in the header).
	fn name(&self) -> &str {
		"Three-Way Color"
	}

	/// Stable type id (C++ `id()`, inline in the header).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.threewaycolor"
	}

	/// Categories (C++ `category()`, inline in the header).
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description (C++ `description()`, inline in the header).
	fn description(&self) -> &str {
		"Adjusts shadows, midtones, and highlights separately."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `shadows_color_in` -> "Shadows", `midtones_color_in` ->
	/// "Midtones", `highlights_color_in` -> "Highlights",
	/// `shadows_amount_in` -> "Shadows Amount", `midtones_amount_in` ->
	/// "Midtones Amount", `highlights_amount_in` -> "Highlights Amount".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			SHADOWS_COLOR_INPUT => "Shadows",
			MIDTONES_COLOR_INPUT => "Midtones",
			HIGHLIGHTS_COLOR_INPUT => "Highlights",
			SHADOWS_AMOUNT_INPUT => "Shadows Amount",
			MIDTONES_AMOUNT_INPUT => "Midtones Amount",
			HIGHLIGHTS_AMOUNT_INPUT => "Highlights Amount",
			_ => id,
		}
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns [`SHADER_FRAG`].
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// otherwise builds a `ShaderJob` from the whole input row, inserts
	/// `luma_coefficients_in` as a vec3 from the project color manager's
	/// default luma coefficients (Rec. 709 `{0.2126, 0.7152, 0.0722}`
	/// when no project/manager is attached), and pushes the texture as
	/// that job.
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

		// `// CPP-PARITY: threewaycolor.cpp` `value()` — the C++ inserts
		// `luma_coefficients_in` (Rec. 709 {0.2126, 0.7152, 0.0722}, or the
		// project color manager's default luma coefficients when one is
		// attached — the Rust model has no project/manager access, so the
		// fallback always applies) into a ShaderJob over the whole input
		// row and pushes `tex->to_job(job)`. The job is boxed here as a
		// [`ShaderJobPayload`] that the renderer's resolve hook executes
		// and replaces with the result texture; the params row carries the
		// luma coefficients under the shader uniform name.
		let mut params = inputs.clone();
		params.insert(
			LUMA_COEFFICIENTS_INPUT.to_string(),
			crate::value::NodeValue::Vec3([0.2126, 0.7152, 0.0722]),
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

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ThreeWayColorNode))
	}
}

/// Constructor (C++ `ThreeWayColorNode::ThreeWayColorNode()`): adds
/// `tex_in` (texture, effect input), the three color inputs with the
/// neutral-gray default, the three amount inputs with the percentage
/// view and `min = 0.0`, and sets the video-effect flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let neutral = crate::value::NodeValue::Color([0.5, 0.5, 0.5, 1.0]);
	core.add_input(crate::input::Input::new(
		SHADOWS_COLOR_INPUT,
		crate::value::ValueType::Color,
		neutral.clone(),
	));
	core.add_input(crate::input::Input::new(
		MIDTONES_COLOR_INPUT,
		crate::value::ValueType::Color,
		neutral.clone(),
	));
	core.add_input(crate::input::Input::new(
		HIGHLIGHTS_COLOR_INPUT,
		crate::value::ValueType::Color,
		neutral,
	));

	let amount_props = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	let mut shadows_amount = crate::input::Input::new(
		SHADOWS_AMOUNT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	shadows_amount.properties = amount_props.clone();
	core.add_input(shadows_amount);
	let mut midtones_amount = crate::input::Input::new(
		MIDTONES_AMOUNT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	midtones_amount.properties = amount_props.clone();
	core.add_input(midtones_amount);
	let mut highlights_amount = crate::input::Input::new(
		HIGHLIGHTS_AMOUNT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	highlights_amount.properties = amount_props;
	core.add_input(highlights_amount);

	core.effect_input = TEXTURE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	(core, Box::new(ThreeWayColorNode))
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.threewaycolor`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.threewaycolor",
		name: "Three-Way Color",
		categories: &[Category::Color],
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
		let n = ThreeWayColorNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(SHADOWS_COLOR_INPUT), "Shadows");
		assert_eq!(n.input_name(MIDTONES_COLOR_INPUT), "Midtones");
		assert_eq!(n.input_name(HIGHLIGHTS_COLOR_INPUT), "Highlights");
		assert_eq!(n.input_name(SHADOWS_AMOUNT_INPUT), "Shadows Amount");
		assert_eq!(n.input_name(MIDTONES_AMOUNT_INPUT), "Midtones Amount");
		assert_eq!(n.input_name(HIGHLIGHTS_AMOUNT_INPUT), "Highlights Amount");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.threewaycolor"
		);
		let tex = core.get_input(TEXTURE_INPUT).unwrap();
		assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		let neutral = NodeValue::Color([0.5, 0.5, 0.5, 1.0]);
		for id in [
			SHADOWS_COLOR_INPUT,
			MIDTONES_COLOR_INPUT,
			HIGHLIGHTS_COLOR_INPUT,
		] {
			assert_eq!(core.get_input(id).unwrap().default, neutral);
		}
		for id in [
			SHADOWS_AMOUNT_INPUT,
			MIDTONES_AMOUNT_INPUT,
			HIGHLIGHTS_AMOUNT_INPUT,
		] {
			let input = core.get_input(id).unwrap();
			assert_eq!(input.default, NodeValue::Float(1.0));
			assert!(input
				.properties
				.iter()
				.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
			assert!(input
				.properties
				.iter()
				.any(|(k, v)| k == "view" && *v == NodeValue::Text("percentage".into())));
		}
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn shader_code_returns_threewaycolor_frag() {
		let code = ThreeWayColorNode.shader_code("anything").unwrap();
		assert!(code.contains("color_offset(shadows_color_in, shadows_amount_in)"));
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
			unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
				.expect("payload boxed behind the handle");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.threewaycolor");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		// The luma coefficients are injected under the shader uniform name
		// (C++ `job.Insert("luma_coefficients_in", ...)`).
		assert_eq!(
			payload.params.get(LUMA_COEFFICIENTS_INPUT),
			Some(&NodeValue::Vec3([0.2126, 0.7152, 0.0722]))
		);
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Three-Way Color");
	}
}
