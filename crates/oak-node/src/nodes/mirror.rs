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

//! Mirror effect: clean-room reimplementation of the OpenFX-Misc `Mirror`
//! plugin's parameter semantics (upstream github.com/cgvirus/OpenFX-Misc,
//! GPL2; read for behavior only, no upstream code copied).
//!
//! Upstream `Mirror` and this crate's [`super::flipdistortnode`] describe
//! the same operation with the same algebra (mirror the sample
//! coordinates about the frame center), so this node is a thin alias: it
//! reuses the flip fragment shader verbatim through
//! [`super::flipdistortnode::FlipDistortNode`]'s
//! [`NodeBehavior::shader_code`] and only maps its own input names onto
//! the shader's uniforms. No second shader is duplicated.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Main texture input id. Type: texture; flags: not-keyframable; this is
/// the node's effect input. Same key as the flip node's texture input, so
/// the flip shader's `tex_in` sampler finds it unchanged.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Horizontal mirror input id (upstream `kParamMirrorFlop` "Horizontal
/// (flop)"). Type: bool; default `false`; mirrors left/right.
pub const HORIZONTAL_INPUT: &str = "horizontal_in";

/// Vertical mirror input id (upstream `kParamMirrorFlip` "Vertical
/// (flip)"). Type: bool; default `false`; mirrors top/bottom.
pub const VERTICAL_INPUT: &str = "vertical_in";

/// Mirror node. Mirrors the image horizontally and/or vertically.
pub struct MirrorNode;

impl NodeBehavior for MirrorNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Mirror"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.mirror"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description.
	fn description(&self) -> &str {
		"Mirrors an image horizontally or vertically"
	}

	/// Localized input names. The upstream labels name the axis of the
	/// flip, so `horizontal_in` (flop, mirrors left/right) is labelled
	/// "Horizontal (flop)" and `vertical_in` (flip, mirrors top/bottom)
	/// "Vertical (flip)".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			HORIZONTAL_INPUT => "Horizontal (flop)",
			VERTICAL_INPUT => "Vertical (flip)",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; neither flag set ->
	/// pass-through push of the input texture unchanged; otherwise a shader
	/// job running the shared flip fragment shader.
	///
	/// The job boxes a [`ShaderJobPayload`] whose `type_id` is this node's
	/// (so the behavior is looked up here) while its `shader_id` is empty:
	/// [`shader_code`](NodeBehavior::shader_code) forwards to the flip
	/// node. The params row is the value row with this node's boolean keys
	/// renamed to the uniform names the flip shader declares
	/// (`horiz_in`/`vert_in`) — the renderer packs uniforms by declared
	/// name and silently leaves undeclared ones at 0, so a raw pass-through
	/// row would render an unmirrored image.
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

		if !horiz && !vert {
			table.push(crate::value::ValueType::Texture, tex, None);
			return;
		}

		let mut params = inputs.clone();
		params.remove(HORIZONTAL_INPUT);
		params.remove(VERTICAL_INPUT);
		params.insert(
			super::flipdistortnode::HORIZONTAL_INPUT.to_string(),
			crate::value::NodeValue::Boolean(horiz),
		);
		params.insert(
			super::flipdistortnode::VERTICAL_INPUT.to_string(),
			crate::value::NodeValue::Boolean(vert),
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
				iterative_input: TEXTURE_INPUT.to_string(),
			})),
			None,
		);
	}

	/// Shader code request: delegates to the flip node's shader (the flip
	/// request id is ignored there — it has a single shader variant).
	fn shader_code(&self, request: &str) -> Option<String> {
		super::flipdistortnode::FlipDistortNode.shader_code(request)
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MirrorNode))
	}
}

/// Constructor: adds `tex_in`, `horizontal_in` and `vertical_in` with the
/// defaults and flags documented on the constants, sets the video-effect
/// flag and the effect input.
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

	(core, Box::new(MirrorNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn tex() -> NodeValue {
        NodeValue::Texture(crate::handle::CHandle::null())
    }

    #[test]
    fn input_names() {
        let n = MirrorNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(HORIZONTAL_INPUT), "Horizontal (flop)");
        assert_eq!(n.input_name(VERTICAL_INPUT), "Vertical (flip)");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.mirror");
        assert_eq!(
            core.get_input(HORIZONTAL_INPUT).unwrap().default,
            NodeValue::Boolean(false)
        );
        assert_eq!(
            core.get_input(VERTICAL_INPUT).unwrap().default,
            NodeValue::Boolean(false)
        );
        assert_ne!(
            core.get_input(TEXTURE_INPUT).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
            0
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
            &NodeValueRow::default(),
            Rational::new(0, 1),
            &mut table,
        );
        assert!(table.is_empty());
    }

    #[test]
    fn value_no_mirror_passes_texture_through() {
        let (core, behavior) = create();
        let tex = tex();
        let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        assert_eq!(table.get(ValueType::Texture), Some(&tex));
    }

    #[test]
    fn value_translates_flag_names_to_the_flip_uniforms() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([
            (TEXTURE_INPUT.to_string(), tex()),
            (HORIZONTAL_INPUT.to_string(), NodeValue::Boolean(true)),
            (VERTICAL_INPUT.to_string(), NodeValue::Boolean(false)),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(&handle) }
            .expect("shader job payload expected");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.mirror");
        assert_eq!(payload.shader_id, "");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        assert_eq!(payload.iterative_input, TEXTURE_INPUT);
        // The shader reads `horiz_in`/`vert_in`; the node's own keys are gone.
        assert_eq!(
            payload
                .params
                .get(super::super::flipdistortnode::HORIZONTAL_INPUT),
            Some(&NodeValue::Boolean(true))
        );
        assert_eq!(
            payload
                .params
                .get(super::super::flipdistortnode::VERTICAL_INPUT),
            Some(&NodeValue::Boolean(false))
        );
        assert!(!payload.params.contains_key(HORIZONTAL_INPUT));
        assert!(!payload.params.contains_key(VERTICAL_INPUT));
    }

    #[test]
    fn value_connected_flags_win_over_the_node_values() {
        let (mut core, behavior) = create();
        core.set_standard_value(HORIZONTAL_INPUT, -1, NodeValue::Boolean(false));
        core.set_standard_value(VERTICAL_INPUT, -1, NodeValue::Boolean(true));
        let inputs = NodeValueRow::from([
            (TEXTURE_INPUT.to_string(), tex()),
            (VERTICAL_INPUT.to_string(), NodeValue::Boolean(false)),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        // The row's false for vertical wins over the node's true, so both
        // flags are off and the texture passes through unmirrored.
        match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => assert!(h.is_null()),
            other => panic!("pass-through texture expected, got {other:?}"),
        }
    }

    #[test]
    fn shader_code_returns_the_flip_shader() {
        let code = MirrorNode.shader_code("").unwrap();
        assert!(code.contains("uniform sampler2D tex_in;"));
        assert!(code.contains("uniform bool horiz_in;"));
        assert!(code.contains("uniform bool vert_in;"));
        assert!(code.contains("if (horiz_in) new_coord.x = 1.0 - new_coord.x;"));
        assert!(code.contains("if (vert_in) new_coord.y = 1.0 - new_coord.y;"));
        assert!(!code.contains("switch"));
    }

    #[test]
    fn duplicate_clones() {
        let (core, behavior) = create();
        let dup = behavior.duplicate(&core).unwrap();
        assert_eq!(dup.name(), "Mirror");
    }
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.mirror",
		name: "Mirror",
		categories: &[Category::Distort],
		create,
	});
}
