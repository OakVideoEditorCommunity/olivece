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

//! Invert effect — a clean-room reimplementation of the OpenFX-Misc
//! `Invert` plugin's parameter semantics (upstream
//! `github.com/cgvirus/OpenFX-Misc`, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! One enable toggle per channel:
//!
//! ```text
//! x = enabled ? 1 - x : x      (independently for r, g, b, a)
//! ```
//!
//! All four toggles default to on, matching the reference, whose
//! `processA`/`processR`/`processG`/`processB` booleans all default to
//! `true`. Unlike the reference (fixed per-component instance
//! parameters), each toggle is a separate node input here.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Red channel toggle id. Type: boolean; default `true`.
pub const INVERT_R_INPUT: &str = "invert_r_in";

/// Green channel toggle id. Type: boolean; default `true`.
pub const INVERT_G_INPUT: &str = "invert_g_in";

/// Blue channel toggle id. Type: boolean; default `true`.
pub const INVERT_B_INPUT: &str = "invert_b_in";

/// Alpha channel toggle id. Type: boolean; default `true`.
pub const INVERT_A_INPUT: &str = "invert_a_in";

/// Invert node. The reference class holds no state beyond its
/// parameter pointers, so this is a unit-like struct.
pub struct InvertNode;

/// Fragment shader (clean-room GLSL for the reference's
/// `InvertPlugin::render` chain). The uniforms are named after the node
/// inputs: the renderer binds uniforms by matching the declared name
/// against the job's parameter row.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;

uniform bool invert_r_in;
uniform bool invert_g_in;
uniform bool invert_b_in;
uniform bool invert_a_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 c = texture(tex_in, ove_texcoord);

  // One independent toggle per channel.
  if (invert_r_in) {
    c.r = 1.0 - c.r;
  }
  if (invert_g_in) {
    c.g = 1.0 - c.g;
  }
  if (invert_b_in) {
    c.b = 1.0 - c.b;
  }
  if (invert_a_in) {
    c.a = 1.0 - c.a;
  }

  frag_color = c;
}
"#;

impl NodeBehavior for InvertNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Invert"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.invert"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Invert individual color channels."
	}

	/// Localized input names: `tex_in` -> "Input" and one "Invert
	/// <Channel>" label per toggle.
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			INVERT_R_INPUT => "Invert Red",
			INVERT_G_INPUT => "Invert Green",
			INVERT_B_INPUT => "Invert Blue",
			INVERT_A_INPUT => "Invert Alpha",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture on `tex_in` -> push nothing;
	/// texture present -> push a shader job over the input row with
	/// every toggle resolved (so the renderer always finds a value for
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
			INVERT_R_INPUT,
			INVERT_G_INPUT,
			INVERT_B_INPUT,
			INVERT_A_INPUT,
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
		Some(Box::new(InvertNode))
	}
}

/// Constructor: adds `tex_in` (texture, effect input) and the four
/// channel toggles, all defaulting to on, and sets the video-effect
/// flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	for id in [
		INVERT_R_INPUT,
		INVERT_G_INPUT,
		INVERT_B_INPUT,
		INVERT_A_INPUT,
	] {
		core.add_input(crate::input::Input::new(
			id,
			crate::value::ValueType::Boolean,
			crate::value::NodeValue::Boolean(true),
		));
	}

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(InvertNode))
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.invert",
		name: "Invert",
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
        let n = InvertNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(INVERT_R_INPUT), "Invert Red");
        assert_eq!(n.input_name(INVERT_G_INPUT), "Invert Green");
        assert_eq!(n.input_name(INVERT_B_INPUT), "Invert Blue");
        assert_eq!(n.input_name(INVERT_A_INPUT), "Invert Alpha");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.invert");
        assert_eq!(behavior.name(), "Invert");
        assert_eq!(behavior.categories(), &[Category::Color]);
        let tex = core.get_input(TEXTURE_INPUT).unwrap();
        assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
        // The reference defaults every process* toggle to on.
        for id in [
            INVERT_R_INPUT,
            INVERT_G_INPUT,
            INVERT_B_INPUT,
            INVERT_A_INPUT,
        ] {
            assert_eq!(
                core.get_input(id).unwrap().default,
                NodeValue::Boolean(true),
                "{id} defaults to on"
            );
        }
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
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.invert");
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        for id in [
            INVERT_R_INPUT,
            INVERT_G_INPUT,
            INVERT_B_INPUT,
            INVERT_A_INPUT,
        ] {
            assert_eq!(payload.params.get(id), Some(&NodeValue::Boolean(true)));
        }
    }

    #[test]
    fn value_row_values_win_over_defaults() {
        let (mut core, behavior) = create();
        core.set_standard_value(INVERT_A_INPUT, -1, NodeValue::Boolean(true));
        let inputs = crate::value::NodeValueRow::from([
            (
                TEXTURE_INPUT.to_string(),
                NodeValue::Texture(crate::handle::CHandle::null()),
            ),
            (INVERT_A_INPUT.to_string(), NodeValue::Boolean(false)),
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
            payload.params.get(INVERT_A_INPUT),
            Some(&NodeValue::Boolean(false))
        );
    }

    #[test]
    fn shader_declares_uniforms_and_avoids_switch() {
        let code = InvertNode.shader_code("").unwrap();
        for uniform in [
            "tex_in",
            INVERT_R_INPUT,
            INVERT_G_INPUT,
            INVERT_B_INPUT,
            INVERT_A_INPUT,
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
        assert_eq!(dup.name(), "Invert");
        assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.invert");
    }
}
