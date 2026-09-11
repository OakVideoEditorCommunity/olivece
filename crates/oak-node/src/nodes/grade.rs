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

//! Grade effect — a clean-room reimplementation of the OpenFX-Misc
//! `Grade` plugin's parameter semantics (upstream
//! `github.com/cgvirus/OpenFX-Misc`, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! A five-point level remap of the three color channels:
//!
//! ```text
//! d = whitepoint - blackpoint
//! a = (d != 0) ? (white - black) / d : 0
//! b = black - a * blackpoint
//! x = a * x + b
//! x = (x > 0) ? pow(x, 1 / gamma) : x
//! ```
//!
//! Alpha is left untouched (the reference's grade pass is color-only).
//! The first line is the reference's black/white point stretch and the
//! second its gamma pass; only the reciprocal exponent is kept from the
//! reference's gamma stage. Deviations from the reference, all
//! deliberate:
//!
//! * The reference's `multiply`/`offset` parameters are dropped — they
//!   sit at their identity values (`1.0`/`0.0`) in the reference's
//!   default setup and would add two no-op controls.
//! * The reference guards its gamma pass with a small positive
//!   threshold; here only non-positive channels are skipped (`pow` has
//!   no real value there) and a non-positive gamma is clamped to a tiny
//!   minimum rather than producing an infinity or a division by zero.
//! * A degenerate point range (`whitepoint == blackpoint`) leaves the
//!   slope at `0.0` instead of dividing by zero.

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Black point input id. Type: float; default `0.0`. Input level mapped
/// to [`BLACK_INPUT`].
pub const BLACKPOINT_INPUT: &str = "blackpoint_in";

/// White point input id. Type: float; default `1.0`. Input level mapped
/// to [`WHITE_INPUT`].
pub const WHITEPOINT_INPUT: &str = "whitepoint_in";

/// Black output input id. Type: float; default `0.0`. Output value the
/// black point maps to.
pub const BLACK_INPUT: &str = "black_in";

/// White output input id. Type: float; default `1.0`. Output value the
/// white point maps to.
pub const WHITE_INPUT: &str = "white_in";

/// Gamma input id. Type: float; default `1.0`. Reciprocal exponent
/// applied after the level remap (`pow(x, 1/gamma)`).
pub const GAMMA_INPUT: &str = "gamma_in";

/// Grade node. The reference class holds no state beyond its parameter
/// pointers, so this is a unit-like struct.
pub struct GradeNode;

/// Fragment shader (clean-room GLSL for the reference's
/// `GradePlugin::render` chain). The uniforms are named after the node
/// inputs: the renderer binds uniforms by matching the declared name
/// against the job's parameter row.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;

uniform float blackpoint_in;
uniform float whitepoint_in;
uniform float black_in;
uniform float white_in;
uniform float gamma_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

// Remap one channel: stretch [blackpoint, whitepoint] onto
// [black, white], then apply the reciprocal-exponent gamma.
float ove_grade(float v) {
  float d = whitepoint_in - blackpoint_in;
  float a = 0.0;
  if (d != 0.0) {
    a = (white_in - black_in) / d;
  }
  float b = black_in - a * blackpoint_in;
  v = a * v + b;

  if (gamma_in != 1.0) {
    float g = max(gamma_in, 1.0e-8);
    if (v > 0.0) {
      v = pow(v, 1.0 / g);
    }
  }
  return v;
}

void main() {
  vec4 c = texture(tex_in, ove_texcoord);

  c.r = ove_grade(c.r);
  c.g = ove_grade(c.g);
  c.b = ove_grade(c.b);

  frag_color = c;
}
"#;

impl NodeBehavior for GradeNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Grade"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.grade"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Remap the color range with black/white points and a gamma."
	}

	/// Localized input names: `tex_in` -> "Input", `blackpoint_in` ->
	/// "Black Point", `whitepoint_in` -> "White Point", `black_in` ->
	/// "Black Output", `white_in` -> "White Output", `gamma_in` ->
	/// "Gamma".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			BLACKPOINT_INPUT => "Black Point",
			WHITEPOINT_INPUT => "White Point",
			BLACK_INPUT => "Black Output",
			WHITE_INPUT => "White Output",
			GAMMA_INPUT => "Gamma",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture on `tex_in` -> push nothing;
	/// texture present -> push a shader job over the input row with
	/// every control resolved (so the renderer always finds a value for
	/// each uniform, whether the row carried the input or the node's own
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
		for id in [
			BLACKPOINT_INPUT,
			WHITEPOINT_INPUT,
			BLACK_INPUT,
			WHITE_INPUT,
			GAMMA_INPUT,
		] {
			params.insert(id.to_string(), resolve(id));
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
		Some(Box::new(GradeNode))
	}
}

/// Constructor: adds `tex_in` (texture, effect input) and the five
/// controls with the defaults documented on the constants, and sets the
/// video-effect flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	add_float_input(&mut core, BLACKPOINT_INPUT, 0.0);
	add_float_input(&mut core, WHITEPOINT_INPUT, 1.0);
	add_float_input(&mut core, BLACK_INPUT, 0.0);
	add_float_input(&mut core, WHITE_INPUT, 1.0);
	add_float_input(&mut core, GAMMA_INPUT, 1.0);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(GradeNode))
}

/// Add a float input with its default. The reference declares no range
/// for these parameters, so no `min`/`max` properties are attached.
fn add_float_input(core: &mut NodeCore, id: &str, default: f64) {
	core.add_input(crate::input::Input::new(
		id,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(default),
	));
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.grade",
		name: "Grade",
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
        let n = GradeNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(BLACKPOINT_INPUT), "Black Point");
        assert_eq!(n.input_name(WHITEPOINT_INPUT), "White Point");
        assert_eq!(n.input_name(BLACK_INPUT), "Black Output");
        assert_eq!(n.input_name(WHITE_INPUT), "White Output");
        assert_eq!(n.input_name(GAMMA_INPUT), "Gamma");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.grade");
        assert_eq!(behavior.name(), "Grade");
        assert_eq!(behavior.categories(), &[Category::Color]);
        let tex = core.get_input(TEXTURE_INPUT).unwrap();
        assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
        assert_eq!(
            core.get_input(BLACKPOINT_INPUT).unwrap().default,
            NodeValue::Float(0.0)
        );
        assert_eq!(
            core.get_input(WHITEPOINT_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(BLACK_INPUT).unwrap().default,
            NodeValue::Float(0.0)
        );
        assert_eq!(
            core.get_input(WHITE_INPUT).unwrap().default,
            NodeValue::Float(1.0)
        );
        assert_eq!(
            core.get_input(GAMMA_INPUT).unwrap().default,
            NodeValue::Float(1.0)
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
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.grade");
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        assert_eq!(
            payload.params.get(BLACKPOINT_INPUT),
            Some(&NodeValue::Float(0.0))
        );
        assert_eq!(
            payload.params.get(WHITEPOINT_INPUT),
            Some(&NodeValue::Float(1.0))
        );
        assert_eq!(payload.params.get(BLACK_INPUT), Some(&NodeValue::Float(0.0)));
        assert_eq!(payload.params.get(WHITE_INPUT), Some(&NodeValue::Float(1.0)));
        assert_eq!(payload.params.get(GAMMA_INPUT), Some(&NodeValue::Float(1.0)));
    }

    #[test]
    fn value_row_values_win_over_defaults() {
        let (mut core, behavior) = create();
        core.set_standard_value(BLACKPOINT_INPUT, -1, NodeValue::Float(0.0));
        let inputs = crate::value::NodeValueRow::from([
            (
                TEXTURE_INPUT.to_string(),
                NodeValue::Texture(crate::handle::CHandle::null()),
            ),
            (BLACKPOINT_INPUT.to_string(), NodeValue::Float(0.1)),
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
            payload.params.get(BLACKPOINT_INPUT),
            Some(&NodeValue::Float(0.1))
        );
    }

    #[test]
    fn shader_declares_uniforms_and_avoids_switch() {
        let code = GradeNode.shader_code("").unwrap();
        for uniform in [
            "tex_in",
            BLACKPOINT_INPUT,
            WHITEPOINT_INPUT,
            BLACK_INPUT,
            WHITE_INPUT,
            GAMMA_INPUT,
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
        assert_eq!(dup.name(), "Grade");
        assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.grade");
    }
}
