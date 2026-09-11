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

//! Position effect: clean-room reimplementation of the OpenFX-Misc
//! `Position` plugin's parameter semantics (upstream
//! github.com/cgvirus/OpenFX-Misc, GPL2; read for behavior only, no
//! upstream code copied).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Main texture input id (upstream `PositionPlugin`'s image input). Type:
/// texture; flags: not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Translate input id (upstream `kParamTranslate` "Translate"). Type:
/// vec2; default `[0.0, 0.0]`; units: whole pixels of the center-origin
/// pixel space (see the shader note). Not clamped.
pub const OFFSET_INPUT: &str = "offset_in";

/// Position node. Translates the input image by a whole-pixel offset.
/// Has no own member fields in C++ (state lives in the `Node` inputs).
pub struct PositionNode;

/// Fragment shader. The sampling is the inverse of the visual offset:
/// the output pixel at center-origin `px` reads the source at
/// `px - offset`, so the image content moves by `+offset` on screen.
///
/// Pixel space: `ove_texcoord * resolution_in - resolution_in * 0.5`,
/// i.e. the center-origin pixel coordinates used by
/// [`super::transformdistortnode`]. This crate's frame rows run top to
/// bottom, so `+y` moves the image DOWN — the opposite screen direction
/// of the upstream OFX plugin, whose y axis points up (upstream hints
/// the new position of the "bottom-left pixel"). The offset is rounded
/// with the upstream integer rounding, `floor(x + 0.5)`.
///
/// There is deliberately no identity fast path: an all-zero offset still
/// runs the shader pass (the upstream plugin does the same).
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform vec2 offset_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec2 half_res = resolution_in * 0.5;
    vec2 px = ove_texcoord * resolution_in - half_res;
    vec2 offset = floor(offset_in + 0.5);
    vec2 uv = (px - offset + half_res) / resolution_in;
    // Whole-pixel translation past the frame edge leaves transparent
    // pixels, not the clamped edge column.
    vec4 col = texture(tex_in, uv);
    float inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    frag_color = col * inside;
}
"#;

impl PositionNode {
	/// Fragment shader (the node has a single shader variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for PositionNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Position"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.position"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description.
	fn description(&self) -> &str {
		"Translate an image by a whole-pixel offset."
	}

	/// Localized input names: `tex_in` -> "Input", `offset_in` ->
	/// "Translate".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			OFFSET_INPUT => "Translate",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; otherwise a shader
	/// job over the whole value row, translating the sampled coordinates
	/// by `offset_in` whole pixels.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and the uniforms, keyed by the effect
	/// input. `resolution_in` is filled by the runner, so it is not part
	/// of the params here.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		if !matches!(inputs.get(TEXTURE_INPUT), Some(crate::value::NodeValue::Texture(_))) {
			return;
		}

		// The offset is part of the row in the traverser flow (the bare key
		// is always inserted for an unconnected input); fall back to the
		// node's own value for direct `value()` calls.
		let mut params = inputs.clone();
		if !params.contains_key(OFFSET_INPUT) {
			params.insert(
				OFFSET_INPUT.to_string(),
				core.value_at_time(OFFSET_INPUT, -1, time),
			);
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
				iterative_input: TEXTURE_INPUT.to_string(),
			})),
			None,
		);
	}

	/// Shader code request: the node has a single shader variant, so the
	/// request id is ignored.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(PositionNode))
	}
}

/// Constructor: adds `tex_in` and `offset_in` with the defaults and flags
/// documented on the constants, sets the video-effect flag and the effect
/// input.
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
		OFFSET_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(PositionNode))
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
        let n = PositionNode;
        assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
        assert_eq!(n.input_name(OFFSET_INPUT), "Translate");
        assert_eq!(n.input_name("other_in"), "other_in");
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.position");
        assert_eq!(
            core.get_input(OFFSET_INPUT).unwrap().default,
            NodeValue::Vec2([0.0, 0.0])
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
    fn value_pushes_shader_job_payload() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(&handle) }
            .expect("shader job payload expected");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.position");
        assert_eq!(payload.shader_id, "");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, TEXTURE_INPUT);
        assert_eq!(payload.iterative_input, TEXTURE_INPUT);
        assert_eq!(payload.time, Rational::new(0, 1));
        assert!(payload.params.contains_key(TEXTURE_INPUT));
        // The offset is not in the row: it resolves from the node default.
        assert_eq!(
            payload.params.get(OFFSET_INPUT),
            Some(&NodeValue::Vec2([0.0, 0.0]))
        );
    }

    #[test]
    fn value_row_offset_wins_over_the_node_value() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([
            (TEXTURE_INPUT.to_string(), tex()),
            (OFFSET_INPUT.to_string(), NodeValue::Vec2([3.0, 2.0])),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(&handle) }
            .expect("shader job payload expected");
        assert_eq!(
            payload.params.get(OFFSET_INPUT),
            Some(&NodeValue::Vec2([3.0, 2.0]))
        );
    }

    #[test]
    fn shader_declares_the_uniforms_it_reads() {
        let code = PositionNode.shader_code("").unwrap();
        assert!(code.contains("uniform sampler2D tex_in;"));
        assert!(code.contains("uniform vec2 offset_in;"));
        assert!(code.contains("uniform vec2 resolution_in;"));
        assert!(code.contains("vec2 uv = (px - offset + half_res) / resolution_in;"));
        // Off-frame samples are masked to transparent, not clamped.
        assert!(code.contains("frag_color = col * inside;"));
        assert!(!code.contains("switch"));
    }

    #[test]
    fn duplicate_clones() {
        let (core, behavior) = create();
        let dup = behavior.duplicate(&core).unwrap();
        assert_eq!(dup.name(), "Position");
    }
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.position",
		name: "Position",
		categories: &[Category::Distort],
		create,
	});
}
