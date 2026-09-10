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

//! Color Correct effect — a clean-room reimplementation of the
//! OpenFX-Misc `ColorCorrect` plugin's parameter semantics (upstream
//! `github.com/cgvirus/OpenFX-Misc`, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! The reference ships five color controls (saturation, contrast,
//! gamma, gain, offset) per tone group (master/shadows/midtones/
//! highlights) plus per-component enable booleans; this node keeps the
//! master group's global controls only — one float per control, applied
//! to all three color channels. Alpha is never touched (the reference
//! leaves `processA` off by default).
//!
//! Formula, applied per pixel in this order (per color channel `x`):
//!
//! ```text
//! luma      = 0.2126*r + 0.7152*g + 0.0722*b      (Rec. 709)
//! x         = (1 - saturation) * luma + saturation * x
//! x         = (x > 0) ? pow(x / 0.18, contrast) * 0.18 : x
//! x         = (x > 0) ? pow(x, 1 / gamma) : x
//! x         = x * gain + offset
//! ```
//!
//! with `contrast` applied only when it differs from `1.0` and `gamma`
//! only when it differs from `1.0`. Contrast pivots on the 0.18 mid
//! gray of the reference (a photographic gray card in sRGB), and the
//! gamma is the reciprocal exponent of the reference's gamma pass.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Saturation input id. Type: float; default `1.0`; properties:
/// `min = 0.0`, `max = 4.0`. Lerps every channel toward the pixel's
/// Rec. 709 luma (`0.0` = fully desaturated, `1.0` = unchanged).
pub const SATURATION_INPUT: &str = "saturation_in";

/// Contrast input id. Type: float; default `1.0`; properties:
/// `min = 0.0`, `max = 4.0`. Power curve around the 0.18 mid gray.
pub const CONTRAST_INPUT: &str = "contrast_in";

/// Gamma input id. Type: float; default `1.0`; properties:
/// `min = 0.2`, `max = 5.0`. Reciprocal exponent (`pow(x, 1/gamma)`).
pub const GAMMA_INPUT: &str = "gamma_in";

/// Gain input id. Type: float; default `1.0`; properties:
/// `min = 0.0`, `max = 4.0`. Multiplies the color channels.
pub const GAIN_INPUT: &str = "gain_in";

/// Offset input id. Type: float; default `0.0`; properties:
/// `min = -1.0`, `max = 1.0`. Added to the color channels after gain.
pub const OFFSET_INPUT: &str = "offset_in";

/// Color correct node. The reference class holds no state beyond its
/// parameter pointers, so this is a unit-like struct.
pub struct ColorCorrectNode;

/// Fragment shader (clean-room GLSL for the reference's
/// `ColorCorrecter::process` chain). The uniforms are named after the
/// node inputs: the renderer binds uniforms by matching the declared
/// name against the job's parameter row.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;

uniform float saturation_in;
uniform float contrast_in;
uniform float gamma_in;
uniform float gain_in;
uniform float offset_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 c = texture(tex_in, ove_texcoord);

  // Saturation: lerp each channel toward the pixel's Rec. 709 luma.
  float luma = dot(c.rgb, vec3(0.2126, 0.7152, 0.0722));
  c.rgb = (1.0 - saturation_in) * luma + saturation_in * c.rgb;

  // Contrast: power curve pivoting on 0.18 mid gray. Non-positive
  // values pass through unchanged (a negative base has no real power).
  if (contrast_in != 1.0) {
    if (c.r > 0.0) {
      c.r = pow(c.r / 0.18, contrast_in) * 0.18;
    }
    if (c.g > 0.0) {
      c.g = pow(c.g / 0.18, contrast_in) * 0.18;
    }
    if (c.b > 0.0) {
      c.b = pow(c.b / 0.18, contrast_in) * 0.18;
    }
  }

  // Gamma: reciprocal exponent, positive values only. A non-positive
  // gamma would divide by zero, so it is clamped to a tiny minimum.
  if (gamma_in != 1.0) {
    float g = max(gamma_in, 1.0e-8);
    if (c.r > 0.0) {
      c.r = pow(c.r, 1.0 / g);
    }
    if (c.g > 0.0) {
      c.g = pow(c.g, 1.0 / g);
    }
    if (c.b > 0.0) {
      c.b = pow(c.b, 1.0 / g);
    }
  }

  // Gain, then offset.
  c.rgb = c.rgb * gain_in + offset_in;

  frag_color = c;
}
"#;

impl NodeBehavior for ColorCorrectNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Color Correct"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.colorcorrect"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Adjust color using saturation, contrast, gamma, gain, and offset."
	}

	/// Localized input names: `tex_in` -> "Input", `saturation_in` ->
	/// "Saturation", `contrast_in` -> "Contrast", `gamma_in` ->
	/// "Gamma", `gain_in` -> "Gain", `offset_in` -> "Offset".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			SATURATION_INPUT => "Saturation",
			CONTRAST_INPUT => "Contrast",
			GAMMA_INPUT => "Gamma",
			GAIN_INPUT => "Gain",
			OFFSET_INPUT => "Offset",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture on `tex_in` -> push nothing;
	/// texture present -> push a shader job over the input row with
	/// every control resolved (so the renderer always finds a value for
	/// each uniform, whether the row carried the input or the node's
	/// own default/keyframe supplied it).
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
		for id in [
			SATURATION_INPUT,
			CONTRAST_INPUT,
			GAMMA_INPUT,
			GAIN_INPUT,
			OFFSET_INPUT,
		] {
			params.insert(id.to_string(), resolve(id));
		}

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

	/// Shader code request: the request id is ignored; always returns
	/// [`SHADER_FRAG`].
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ColorCorrectNode))
	}
}

/// Constructor: adds `tex_in` (texture, effect input) and the five
/// controls with the defaults and ranges documented on the constants,
/// and sets the video-effect flag.
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
	add_float_input(&mut core, CONTRAST_INPUT, 1.0, 0.0, 4.0);
	add_float_input(&mut core, GAMMA_INPUT, 1.0, 0.2, 5.0);
	add_float_input(&mut core, GAIN_INPUT, 1.0, 0.0, 4.0);
	add_float_input(&mut core, OFFSET_INPUT, 0.0, -1.0, 1.0);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(ColorCorrectNode))
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
		type_id: "org.olivevideoeditor.Olive.colorcorrect",
		name: "Color Correct",
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
        let n = ColorCorrectNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(SATURATION_INPUT), "Saturation");
        assert_eq!(n.input_name(CONTRAST_INPUT), "Contrast");
        assert_eq!(n.input_name(GAMMA_INPUT), "Gamma");
        assert_eq!(n.input_name(GAIN_INPUT), "Gain");
        assert_eq!(n.input_name(OFFSET_INPUT), "Offset");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(
            behavior.type_id(),
            "org.olivevideoeditor.Olive.colorcorrect"
        );
        assert_eq!(behavior.name(), "Color Correct");
        assert_eq!(behavior.categories(), &[Category::Color]);
        let tex = core.get_input(TEXTURE_INPUT).unwrap();
        assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
        assert_eq!(
            core.get_input(SATURATION_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(CONTRAST_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(GAMMA_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(GAIN_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(OFFSET_INPUT).unwrap().default,
            NodeValue::Float(0.0)
        );
        assert_eq!(core.effect_input, TEXTURE_INPUT);
        assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
    }

    #[test]
    fn create_sets_control_ranges() {
        let (core, _) = create();
        let gamma = core.get_input(GAMMA_INPUT).unwrap();
        assert_eq!(gamma.properties[0].1, NodeValue::Float(0.2));
        assert_eq!(gamma.properties[1].1, NodeValue::Float(5.0));
        let offset = core.get_input(OFFSET_INPUT).unwrap();
        assert_eq!(offset.properties[0].1, NodeValue::Float(-1.0));
        assert_eq!(offset.properties[1].1, NodeValue::Float(1.0));
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
            unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
                .expect("shader job pushed");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.colorcorrect");
        assert_eq!(payload.shader_id, "");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        // Every control is resolved into the row, so the renderer finds
        // a value for each uniform even when the row omitted the input.
        assert_eq!(
            payload.params.get(SATURATION_INPUT),
            Some(&NodeValue::Float(1.0))
        );
        assert_eq!(
            payload.params.get(CONTRAST_INPUT),
            Some(&NodeValue::Float(1.0))
        );
        assert_eq!(payload.params.get(GAMMA_INPUT), Some(&NodeValue::Float(1.0)));
        assert_eq!(payload.params.get(GAIN_INPUT), Some(&NodeValue::Float(1.0)));
        assert_eq!(
            payload.params.get(OFFSET_INPUT),
            Some(&NodeValue::Float(0.0))
        );
    }

    #[test]
    fn value_row_values_win_over_defaults() {
        let (mut core, behavior) = create();
        core.set_standard_value(CONTRAST_INPUT, -1, NodeValue::Float(1.5));
        let inputs = crate::value::NodeValueRow::from([
            (
                TEXTURE_INPUT.to_string(),
                NodeValue::Texture(crate::handle::CHandle::null()),
            ),
            (CONTRAST_INPUT.to_string(), NodeValue::Float(2.0)),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
            panic!("expected a texture-typed value");
        };
        let payload =
            unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
                .expect("shader job pushed");
        assert_eq!(
            payload.params.get(CONTRAST_INPUT),
            Some(&NodeValue::Float(2.0))
        );
    }

    #[test]
    fn shader_declares_uniforms_and_avoids_switch() {
        let code = ColorCorrectNode.shader_code("").unwrap();
        for uniform in [
            "tex_in",
            SATURATION_INPUT,
            CONTRAST_INPUT,
            GAMMA_INPUT,
            GAIN_INPUT,
            OFFSET_INPUT,
        ] {
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
        assert_eq!(dup.name(), "Color Correct");
        assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.colorcorrect");
    }
}
