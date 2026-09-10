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

//! Clamp effect — a clean-room reimplementation of the OpenFX-Misc
//! `Clamp` plugin's parameter semantics (upstream
//! `github.com/cgvirus/OpenFX-Misc`, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! Two scalars, one lower and one upper bound, applied to every
//! channel:
//!
//! ```text
//! lo = min(min_in, max_in)
//! hi = max(min_in, max_in)
//! x  = min(max(x, lo), hi)
//! ```
//!
//! The min bound is applied first and the max bound second, so when
//! `max_in` is below `min_in` the result is `max_in` (the max wins the
//! conflicting range). The reference clamps all four components —
//! `processA` defaults to `true` — so alpha is clamped as well, which
//! differs from the color-only nodes in this group.
//!
//! The reference clamps with an explicit low/high `vec4` built from its
//! two parameters; the shader here does the equivalent with `min`/
//! `max` over `vec4` operands (the scalar-operand `clamp` overloads are
//! avoided on purpose — one less overload for the WGSL emitter to
//! resolve).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Lower bound input id. Type: float; default `0.0`. Values below it
/// are raised to it.
pub const MIN_INPUT: &str = "min_in";

/// Upper bound input id. Type: float; default `1.0`. Values above it
/// are lowered to it.
pub const MAX_INPUT: &str = "max_in";

/// Clamp node. The reference class holds no state beyond its parameter
/// pointers, so this is a unit-like struct.
pub struct ClampNode;

/// Fragment shader (clean-room GLSL for the reference's
/// `ClampPlugin::render` chain). The uniforms are named after the node
/// inputs: the renderer binds uniforms by matching the declared name
/// against the job's parameter row.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;

uniform float min_in;
uniform float max_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 c = texture(tex_in, ove_texcoord);

  // Lower bound first, upper bound second: when max_in < min_in the
  // upper bound wins.
  vec4 lo = vec4(min_in, min_in, min_in, min_in);
  vec4 hi = vec4(max_in, max_in, max_in, max_in);
  c = min(max(c, lo), hi);

  frag_color = c;
}
"#;

impl NodeBehavior for ClampNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Clamp"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.clamp"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Clamp every channel to a lower and an upper bound."
	}

	/// Localized input names: `tex_in` -> "Input", `min_in` -> "Min",
	/// `max_in` -> "Max".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			MIN_INPUT => "Min",
			MAX_INPUT => "Max",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture on `tex_in` -> push nothing;
	/// texture present -> push a shader job over the input row with
	/// both bounds resolved (so the renderer always finds a value for
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
		params.insert(MIN_INPUT.to_string(), resolve(MIN_INPUT));
		params.insert(MAX_INPUT.to_string(), resolve(MAX_INPUT));

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
		Some(Box::new(ClampNode))
	}
}

/// Constructor: adds `tex_in` (texture, effect input) and the two
/// bounds with the defaults documented on the constants, and sets the
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

	add_float_input(&mut core, MIN_INPUT, 0.0);
	add_float_input(&mut core, MAX_INPUT, 1.0);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(ClampNode))
}

/// Add a float input with its default. The bounds themselves are
/// unbounded (the reference parameter has no range), so no `min`/`max`
/// properties are attached.
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
		type_id: "org.olivevideoeditor.Olive.clamp",
		name: "Clamp",
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
        let n = ClampNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(MIN_INPUT), "Min");
        assert_eq!(n.input_name(MAX_INPUT), "Max");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.clamp");
        assert_eq!(behavior.name(), "Clamp");
        assert_eq!(behavior.categories(), &[Category::Color]);
        let tex = core.get_input(TEXTURE_INPUT).unwrap();
        assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
        assert_eq!(
            core.get_input(MIN_INPUT).unwrap().default,
            NodeValue::Float(0.0)
        );
        assert_eq!(
            core.get_input(MAX_INPUT).unwrap().default,
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
            unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
                .expect("shader job pushed");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.clamp");
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        assert_eq!(payload.params.get(MIN_INPUT), Some(&NodeValue::Float(0.0)));
        assert_eq!(payload.params.get(MAX_INPUT), Some(&NodeValue::Float(1.0)));
    }

    #[test]
    fn value_row_values_win_over_defaults() {
        let (mut core, behavior) = create();
        core.set_standard_value(MAX_INPUT, -1, NodeValue::Float(1.0));
        let inputs = crate::value::NodeValueRow::from([
            (
                TEXTURE_INPUT.to_string(),
                NodeValue::Texture(crate::handle::CHandle::null()),
            ),
            (MAX_INPUT.to_string(), NodeValue::Float(0.5)),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
            panic!("expected a texture-typed value");
        };
        let payload =
            unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
                .expect("shader job pushed");
        assert_eq!(payload.params.get(MAX_INPUT), Some(&NodeValue::Float(0.5)));
    }

    #[test]
    fn shader_declares_uniforms_and_avoids_switch() {
        let code = ClampNode.shader_code("").unwrap();
        for uniform in ["tex_in", MIN_INPUT, MAX_INPUT] {
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
        assert_eq!(dup.name(), "Clamp");
        assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.clamp");
    }
}
