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

//! Saturation effect — a clean-room reimplementation of the OpenFX-Misc
//! `Saturation` plugin's parameter semantics (upstream
//! `github.com/cgvirus/OpenFX-Misc`, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! One saturation control that lerps every color channel between the
//! pixel's Rec. 709 luma and its original value:
//!
//! ```text
//! luma = 0.2126*r + 0.7152*g + 0.0722*b
//! x    = (1 - saturation) * luma + saturation * x
//! ```
//!
//! so `0.0` produces a grayscale image, `1.0` is the identity, and
//! values above `1.0` extrapolate away from gray. Alpha is left
//! untouched (the reference's saturation pass has no alpha term). The
//! lerp runs unconditionally — unlike the gamma-style passes there is
//! no "changed" test in the reference.

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Saturation input id. Type: float; default `1.0`; properties:
/// `min = 0.0`, `max = 4.0`. Lerps every channel toward the pixel's
/// Rec. 709 luma (`0.0` = grayscale, `1.0` = unchanged).
pub const SATURATION_INPUT: &str = "saturation_in";

/// Saturation node. The reference class holds no state beyond its
/// parameter pointers, so this is a unit-like struct.
pub struct SaturationNode;

/// Fragment shader (clean-room GLSL for the reference's
/// `SaturationPlugin::render` chain). The uniforms are named after the
/// node inputs: the renderer binds uniforms by matching the declared
/// name against the job's parameter row.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;

uniform float saturation_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 c = texture(tex_in, ove_texcoord);

  // Lerp every color channel between the pixel's Rec. 709 luma and its
  // original value.
  float luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
  c.rgb = (1.0 - saturation_in) * luma + saturation_in * c.rgb;

  frag_color = c;
}
"#;

impl NodeBehavior for SaturationNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Saturation"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.saturation"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Adjust the color intensity by lerping toward the image luma."
	}

	/// Localized input names: `tex_in` -> "Input", `saturation_in` ->
	/// "Saturation".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			SATURATION_INPUT => "Saturation",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture on `tex_in` -> push nothing;
	/// texture present -> push a shader job over the input row with the
	/// control resolved (so the renderer always finds a value for the
	/// uniform, whether the row carried the input or the node's own
	/// default/keyframe supplied it).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		if !matches!(
			inputs.get(TEXTURE_INPUT),
			Some(crate::value::NodeValue::Texture(_))
		) {
			return;
		}

		let resolve = |id: &str| match inputs.get(id) {
			Some(v) => v.clone(),
			None => core.value_at_time(id, -1, time),
		};

		let mut params = inputs.clone();
		params.insert(SATURATION_INPUT.to_string(), resolve(SATURATION_INPUT));

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

	/// Shader code request: the request id is ignored; always returns
	/// [`SHADER_FRAG`].
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(SaturationNode))
	}
}

/// Constructor: adds `tex_in` (texture, effect input) and the
/// saturation control with the default and range documented on the
/// constant, and sets the video-effect flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	add_float_input(&mut core, SATURATION_INPUT, 1.0, 0.0, 4.0);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(SaturationNode))
}

/// Add a float input with its default and `min`/`max`/`view`
/// properties.
fn add_float_input(core: &mut NodeCore, id: &str, default: f64, min: f64, max: f64) {
	let mut input = crate::input::Input::new(
		id,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(default),
	);
	input.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(min)),
		("max".to_string(), crate::value::NodeValue::Float(max)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("normal".into()),
		),
	];
	core.add_input(input);
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.saturation",
		name: "Saturation",
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
        let n = SaturationNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(SATURATION_INPUT), "Saturation");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.saturation");
        assert_eq!(behavior.name(), "Saturation");
        assert_eq!(behavior.categories(), &[Category::Color]);
        let tex = core.get_input(TEXTURE_INPUT).unwrap();
        assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
        assert_eq!(
            core.get_input(SATURATION_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(core.effect_input, TEXTURE_INPUT);
        assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
    }

    #[test]
    fn create_sets_control_range() {
        let (core, _) = create();
        let saturation = core.get_input(SATURATION_INPUT).unwrap();
        assert_eq!(saturation.properties[0].1, NodeValue::Float(0.0));
        assert_eq!(saturation.properties[1].1, NodeValue::Float(4.0));
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
    fn value_with_texture_pushes_shader_job_with_resolved_params() {
        let (core, behavior) = create();
        let inputs = crate::value::NodeValueRow::from([(
            TEXTURE_INPUT.to_string(),
            NodeValue::Texture(crate::handle::CHandle::null()),
        )]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
            panic!("expected a texture-typed value");
        };
        let payload =
            unsafe { crate::jobs::shader_job(handle) }
                .expect("shader job pushed");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.saturation");
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        assert_eq!(
            payload.params.get(SATURATION_INPUT),
            Some(&NodeValue::Float(1.0))
        );
    }

    #[test]
    fn value_row_values_win_over_defaults() {
        let (mut core, behavior) = create();
        core.set_standard_value(SATURATION_INPUT, -1, NodeValue::Float(3.0));
        let inputs = crate::value::NodeValueRow::from([
            (
                TEXTURE_INPUT.to_string(),
                NodeValue::Texture(crate::handle::CHandle::null()),
            ),
            (SATURATION_INPUT.to_string(), NodeValue::Float(0.25)),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
            panic!("expected a texture-typed value");
        };
        let payload =
            unsafe { crate::jobs::shader_job(handle) }
                .expect("shader job pushed");
        assert_eq!(
            payload.params.get(SATURATION_INPUT),
            Some(&NodeValue::Float(0.25))
        );
    }

    #[test]
    fn shader_declares_uniforms_and_avoids_switch() {
        let code = SaturationNode.shader_code("").unwrap();
        for uniform in ["tex_in", SATURATION_INPUT] {
            assert!(code.contains(uniform), "uniform {uniform} declared");
        }
        assert!(code.contains("ove_texcoord"));
        assert!(code.contains("frag_color"));
        assert!(!code.contains("switch"), "naga rejects GLSL switch");
    }

    #[test]
    fn duplicate_clones() {
        let (core, behavior) = create();
        let dup = behavior.duplicate(&core).unwrap();
        assert_eq!(dup.name(), "Saturation");
        assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.saturation");
    }
}
