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

//! Checkerboard generator: clean-room reimplementation of the
//! OpenFX-Misc `CheckerBoard` plugin's parameter semantics (upstream
//! github.com/cgvirus/OpenFX-Misc, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! Deviations from the upstream plugin: it offers four checker colors
//! (`color0`..`color3`, the second pair for a two-cell pattern), a line
//! color/width pair and a centerline color/width pair; this node renders
//! the plain two-color checkerboard only (`color1_in`/`color2_in`, the
//! upstream `color1`/`color2`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Base texture input id (the shared generator-with-merge base). Type:
/// texture; flags: not-keyframable; this is the generator's effect input.
pub const BASE_INPUT: &str = super::generatorwithmerge::BASE_INPUT;

/// Box size input id (upstream `kParamBoxSize` "Box Size"). Type: vec2;
/// default `[64.0, 64.0]`; properties: `min = [0.0, 0.0]`; units: pixels
/// of `resolution_in`.
pub const SIZE_INPUT: &str = "size_in";

/// First checker color input id (upstream `kParamColor1`, the color of
/// the top-left box). Type: color; default `[0.1, 0.1, 0.1, 1.0]`;
/// properties: `view = color`.
pub const COLOR1_INPUT: &str = "color1_in";

/// Second checker color input id (upstream `kParamColor2`). Type: color;
/// default `[0.5, 0.5, 0.5, 1.0]`; properties: `view = color`.
pub const COLOR2_INPUT: &str = "color2_in";

/// Checkerboard generator node.
pub struct CheckerBoardNode;

/// Fragment shader for the `"checkerboard"` shader id.
///
/// Pixel space: `ove_texcoord * resolution_in - resolution_in * 0.5`,
/// i.e. the center-origin pixel coordinates used by
/// [`super::transformdistortnode`]. The upstream plugin anchors the
/// pattern origin at the center of the image (its `color0` hint names
/// "the top-left of the image center"), so the cell index is
/// `floor(px / box_size)` and the pattern's parity is the sum of the cell
/// indices — the shaded cells alternate like a checkerboard. Sizes below
/// one pixel would divide by zero, so the box size is clamped to
/// `>= 1.0`.
const SHADER_FRAG: &str = r#"uniform vec2 resolution_in;
uniform vec2 size_in;
uniform vec4 color1_in;
uniform vec4 color2_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec2 px = ove_texcoord * resolution_in - resolution_in * 0.5;
    vec2 box = max(size_in, vec2(1.0));
    vec2 cell = floor(px / box);

    // Parity of the cell index sum, valid for negative cell indices too
    // (`mod` in GLSL is `x - y * floor(x / y)`).
    float s = cell.x + cell.y;
    float parity = s - 2.0 * floor(s * 0.5);

    if (parity < 0.5) {
        frag_color = color1_in;
    } else {
        frag_color = color2_in;
    }
}
"#;

impl CheckerBoardNode {
	/// Fragment shader for the `"checkerboard"` request.
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for CheckerBoardNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Checkerboard"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.checkerboard"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description.
	fn description(&self) -> &str {
		"Generate a checkerboard pattern."
	}

	/// Localized input names: the merge base's `base_in` -> "Base" plus
	/// `size_in` -> "Box Size", `color1_in` -> "Color 1", `color2_in` ->
	/// "Color 2".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			BASE_INPUT => "Base",
			SIZE_INPUT => "Box Size",
			COLOR1_INPUT => "Color 1",
			COLOR2_INPUT => "Color 2",
			_ => id,
		}
	}

	/// Evaluate outputs: a `"checkerboard"` shader job over the value row,
	/// pushed through `push_mergable_job` — merged alpha-over `base_in`
	/// when a base texture is connected, pushed bare otherwise (matching
	/// the shared generator-with-merge wiring).
	///
	/// The params row carries `size_in` and the two colors; `resolution_in`
	/// is filled by the runner from the render target size, so it is not
	/// part of the params here.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mut params = inputs.clone();
		// The inputs are part of the row in the traverser flow (the bare
		// key is always inserted for an unconnected input); fall back to
		// the node's own values for direct `value()` calls.
		for id in [SIZE_INPUT, COLOR1_INPUT, COLOR2_INPUT] {
			if !params.contains_key(id) {
				params.insert(id.to_string(), core.value_at_time(id, -1, time));
			}
		}

		let job = crate::handle::make_owned(ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations: 1,
			type_id: self.type_id().to_string(),
			shader_id: "checkerboard".to_string(),
			effect_input: core.effect_input.clone(),
			params,
			iterative_input: String::new(),
		});
		super::generatorwithmerge::GeneratorWithMerge::push_mergable_job(inputs, job, table);
	}

	/// Shader code request: `"checkerboard"` returns this node's shader;
	/// the `"mrg"` request returns the shared alpha-over merge shader;
	/// anything else is unsupported.
	fn shader_code(&self, request: &str) -> Option<String> {
		match request {
			"checkerboard" => Some(Self::shader_frag().to_string()),
			"mrg" => Some(super::generatorwithmerge::merge_shader_frag().to_string()),
			_ => None,
		}
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(CheckerBoardNode))
	}
}

/// Constructor: adds `base_in` (not-keyframable), `size_in`, `color1_in`
/// and `color2_in` with the defaults and properties documented on the
/// constants, sets the video-effect flag and makes `base_in` the effect
/// input (the `GeneratorWithMerge` constructor side effects).
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut base = crate::input::Input::new(
		BASE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);

	let mut size = crate::input::Input::new(
		SIZE_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([64.0, 64.0]),
	);
	size.properties = vec![("min".to_string(), crate::value::NodeValue::Vec2([0.0, 0.0]))];
	core.add_input(size);

	let mut color1 = crate::input::Input::new(
		COLOR1_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([0.1, 0.1, 0.1, 1.0]),
	);
	color1.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("color".into()),
	)];
	core.add_input(color1);

	let mut color2 = crate::input::Input::new(
		COLOR2_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([0.5, 0.5, 0.5, 1.0]),
	);
	color2.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("color".into()),
	)];
	core.add_input(color2);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = BASE_INPUT.to_string();

	(core, Box::new(CheckerBoardNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
    fn input_names() {
        let n = CheckerBoardNode;
        assert_eq!(n.input_name(SIZE_INPUT), "Box Size");
        assert_eq!(n.input_name(COLOR1_INPUT), "Color 1");
        assert_eq!(n.input_name(COLOR2_INPUT), "Color 2");
        assert_eq!(
            n.input_name(super::super::generatorwithmerge::BASE_INPUT),
            "Base"
        );
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.checkerboard");
        assert_eq!(
            core.get_input(SIZE_INPUT).unwrap().default,
            NodeValue::Vec2([64.0, 64.0])
        );
        assert_eq!(
            core.get_input(COLOR1_INPUT).unwrap().default,
            NodeValue::Color([0.1, 0.1, 0.1, 1.0])
        );
        assert_eq!(
            core.get_input(COLOR2_INPUT).unwrap().default,
            NodeValue::Color([0.5, 0.5, 0.5, 1.0])
        );
        assert!(core.get_input(COLOR1_INPUT).unwrap().properties.iter().any(
            |(k, v)| k == "view" && *v == NodeValue::Text("color".into())
        ));
        assert_eq!(core.effect_input, BASE_INPUT);
        assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
    }

    #[test]
    fn value_pushes_generator_job() {
        let (core, behavior) = create();
        let mut table = NodeValueTable::default();
        behavior.value(
            &core,
            &NodeValueRow::default(),
            Rational::new(0, 1),
            &mut table,
        );
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(&handle) }
            .expect("shader job payload expected");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.checkerboard");
        assert_eq!(payload.shader_id, "checkerboard");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, BASE_INPUT);
        assert_eq!(payload.iterative_input, "");
        // The unconnected inputs resolve from the node defaults.
        assert_eq!(
            payload.params.get(SIZE_INPUT),
            Some(&NodeValue::Vec2([64.0, 64.0]))
        );
        assert_eq!(
            payload.params.get(COLOR1_INPUT),
            Some(&NodeValue::Color([0.1, 0.1, 0.1, 1.0]))
        );
        assert_eq!(
            payload.params.get(COLOR2_INPUT),
            Some(&NodeValue::Color([0.5, 0.5, 0.5, 1.0]))
        );
    }

    #[test]
    fn value_row_values_win_over_the_node_defaults() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([
            (SIZE_INPUT.to_string(), NodeValue::Vec2([4.0, 4.0])),
            (COLOR1_INPUT.to_string(), NodeValue::Color([1.0, 0.0, 0.0, 1.0])),
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
            payload.params.get(SIZE_INPUT),
            Some(&NodeValue::Vec2([4.0, 4.0]))
        );
        assert_eq!(
            payload.params.get(COLOR1_INPUT),
            Some(&NodeValue::Color([1.0, 0.0, 0.0, 1.0]))
        );
    }

    #[test]
    fn value_with_base_merges_nested_job() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([(
            BASE_INPUT.to_string(),
            NodeValue::Texture(crate::handle::CHandle::null()),
        )]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let merge = unsafe { crate::handle::get_checked::<ShaderJobPayload>(&handle) }
            .expect("merge job payload expected");
        assert_eq!(merge.shader_id, "mrg");
        assert_eq!(merge.iterations, 1);
        assert_eq!(merge.effect_input, BASE_INPUT);
        assert!(merge.params.contains_key(BASE_INPUT));
        // The generator's own parameters travel with the nested job; the
        // merge payload carries only the base and the blend input.
        assert!(!merge.params.contains_key(COLOR1_INPUT));
        // The checkerboard job is nested as the merge's blend texture.
        match merge.params.get(crate::nodes::merge::BLEND_INPUT) {
            Some(NodeValue::Texture(blend)) => {
                let nested = unsafe { crate::handle::get_checked::<ShaderJobPayload>(blend) }
                    .expect("nested job payload boxed");
                assert_eq!(nested.shader_id, "checkerboard");
                assert_eq!(
                    nested.params.get(COLOR1_INPUT),
                    Some(&NodeValue::Color([0.1, 0.1, 0.1, 1.0]))
                );
            }
            _ => panic!("nested blend job expected"),
        }
    }

    #[test]
    fn shader_code_dispatches() {
        let n = CheckerBoardNode;
        let code = n.shader_code("checkerboard").unwrap();
        assert!(code.contains("uniform vec2 size_in;"));
        assert!(code.contains("uniform vec4 color1_in;"));
        assert!(code.contains("uniform vec4 color2_in;"));
        assert!(code.contains("uniform vec2 resolution_in;"));
        assert!(code.contains("float parity = s - 2.0 * floor(s * 0.5);"));
        assert!(!code.contains("switch"));
        assert!(n
            .shader_code("mrg")
            .unwrap()
            .contains("base_col *= 1.0 - blend_col.a;"));
        assert!(n.shader_code("other").is_none());
    }

    #[test]
    fn duplicate_clones() {
        let (core, behavior) = create();
        let dup = behavior.duplicate(&core).unwrap();
        assert_eq!(dup.name(), "Checkerboard");
    }
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.checkerboard",
		name: "Checkerboard",
		categories: &[Category::Generator],
		create,
	});
}
