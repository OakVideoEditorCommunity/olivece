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

//! Linear ramp generator: clean-room reimplementation of the OpenFX-Misc
//! `Ramp` plugin's parameter semantics (upstream github.com/cgvirus/
//! OpenFX-Misc, GPL2; read for behavior only, no upstream code copied).
//!
//! Deviations from the upstream plugin: it additionally offers a ramp
//! `type` combo and an `interactive` gizmo switch, neither of which is
//! reproduced here. The upstream parameter descriptors live in
//! `ofxsRamp.h`, which is not part of the reference copy, so the default
//! points below are this node's own choice (a horizontal black-to-white
//! ramp across a 200px-wide frame).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Base texture input id (the shared generator-with-merge base). Type:
/// texture; flags: not-keyframable; this is the generator's effect input.
pub const BASE_INPUT: &str = super::generatorwithmerge::BASE_INPUT;

/// First point input id (upstream `kParamPoint0` "Point 0"). Type: vec2;
/// default `[-100.0, 0.0]`; units: pixels of the center-origin pixel
/// space (see the shader note).
pub const POINT0_INPUT: &str = "point0_in";

/// Second point input id (upstream `kParamPoint1` "Point 1"). Type: vec2;
/// default `[100.0, 0.0]`; units: pixels of the center-origin pixel
/// space.
pub const POINT1_INPUT: &str = "point1_in";

/// First color input id (upstream `kParamColor0`). Type: color; default
/// `[0.0, 0.0, 0.0, 1.0]` (black); properties: `view = color`.
pub const COLOR0_INPUT: &str = "color0_in";

/// Second color input id (upstream `kParamColor1`). Type: color; default
/// `[1.0, 1.0, 1.0, 1.0]` (white); properties: `view = color`.
pub const COLOR1_INPUT: &str = "color1_in";

/// Linear ramp generator node.
pub struct RampNode;

/// Fragment shader for the `"ramp"` shader id.
///
/// The gradient parameter `t` is the projection of the pixel onto the
/// `point0_in -> point1_in` axis: `t = dot(px - point0, d) / dot(d, d)`
/// with `d = point1 - point0`, exactly the upstream ramp function. `t`
/// is not clamped, so values outside the `[0, 1]` span extrapolate the
/// gradient past the endpoint colors (the upstream linear type does the
/// same); a degenerate axis (`point0 == point1`) leaves `t = 0`.
///
/// Pixel space: `ove_texcoord * resolution_in - resolution_in * 0.5`,
/// i.e. the center-origin pixel coordinates used by
/// [`super::transformdistortnode`], with y running downward.
const SHADER_FRAG: &str = r#"uniform vec2 resolution_in;
uniform vec2 point0_in;
uniform vec2 point1_in;
uniform vec4 color0_in;
uniform vec4 color1_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec2 px = ove_texcoord * resolution_in - resolution_in * 0.5;
    vec2 d = point1_in - point0_in;
    float norm2 = dot(d, d);

    float t = 0.0;
    if (norm2 > 0.0) {
        t = dot(px - point0_in, d) / norm2;
    }

    frag_color = color0_in * (1.0 - t) + color1_in * t;
}
"#;

impl RampNode {
	/// Fragment shader for the `"ramp"` request.
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for RampNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Ramp"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ramp"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description.
	fn description(&self) -> &str {
		"Generate a linear color ramp between two points."
	}

	/// Localized input names: the merge base's `base_in` -> "Base" plus
	/// `point0_in` -> "Point 0", `point1_in` -> "Point 1", `color0_in` ->
	/// "Color 0", `color1_in` -> "Color 1".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			BASE_INPUT => "Base",
			POINT0_INPUT => "Point 0",
			POINT1_INPUT => "Point 1",
			COLOR0_INPUT => "Color 0",
			COLOR1_INPUT => "Color 1",
			_ => id,
		}
	}

	/// Evaluate outputs: a `"ramp"` shader job over the value row, pushed
	/// through `push_mergable_job` — merged alpha-over `base_in` when a
	/// base texture is connected, pushed bare otherwise.
	///
	/// The params row carries the two points and the two colors;
	/// `resolution_in` is filled by the runner from the render target
	/// size, so it is not part of the params here.
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
		for id in [POINT0_INPUT, POINT1_INPUT, COLOR0_INPUT, COLOR1_INPUT] {
			if !params.contains_key(id) {
				params.insert(id.to_string(), core.value_at_time(id, -1, time));
			}
		}

		let job = crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations: 1,
			type_id: self.type_id().to_string(),
			shader_id: "ramp".to_string(),
			effect_input: core.effect_input.clone(),
			params,
			iterative_input: String::new(),
		}));
		super::generatorwithmerge::GeneratorWithMerge::push_mergable_job(inputs, job, table);
	}

	/// Shader code request: `"ramp"` returns this node's shader; the
	/// `"mrg"` request returns the shared alpha-over merge shader;
	/// anything else is unsupported.
	fn shader_code(&self, request: &str) -> Option<String> {
		match request {
			"ramp" => Some(Self::shader_frag().to_string()),
			"mrg" => Some(super::generatorwithmerge::merge_shader_frag().to_string()),
			_ => None,
		}
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(RampNode))
	}
}

/// Constructor: adds `base_in` (not-keyframable), the two points and the
/// two colors with the defaults and properties documented on the
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

	core.add_input(crate::input::Input::new(
		POINT0_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([-100.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		POINT1_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([100.0, 0.0]),
	));

	for (id, default) in [
		(COLOR0_INPUT, [0.0, 0.0, 0.0, 1.0]),
		(COLOR1_INPUT, [1.0, 1.0, 1.0, 1.0]),
	] {
		let mut color = crate::input::Input::new(
			id,
			crate::value::ValueType::Color,
			crate::value::NodeValue::Color(default),
		);
		color.properties = vec![(
			"view".to_string(),
			crate::value::NodeValue::Text("color".into()),
		)];
		core.add_input(color);
	}

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = BASE_INPUT.to_string();

	(core, Box::new(RampNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
    fn input_names() {
        let n = RampNode;
        assert_eq!(n.input_name(POINT0_INPUT), "Point 0");
        assert_eq!(n.input_name(POINT1_INPUT), "Point 1");
        assert_eq!(n.input_name(COLOR0_INPUT), "Color 0");
        assert_eq!(n.input_name(COLOR1_INPUT), "Color 1");
        assert_eq!(
            n.input_name(super::super::generatorwithmerge::BASE_INPUT),
            "Base"
        );
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ramp");
        assert_eq!(
            core.get_input(POINT0_INPUT).unwrap().default,
            NodeValue::Vec2([-100.0, 0.0])
        );
        assert_eq!(
            core.get_input(POINT1_INPUT).unwrap().default,
            NodeValue::Vec2([100.0, 0.0])
        );
        assert_eq!(
            core.get_input(COLOR0_INPUT).unwrap().default,
            NodeValue::Color([0.0, 0.0, 0.0, 1.0])
        );
        assert_eq!(
            core.get_input(COLOR1_INPUT).unwrap().default,
            NodeValue::Color([1.0, 1.0, 1.0, 1.0])
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
        let payload = unsafe { crate::jobs::shader_job(&handle) }
            .expect("shader job payload expected");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.ramp");
        assert_eq!(payload.shader_id, "ramp");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, BASE_INPUT);
        assert_eq!(payload.iterative_input, "");
        assert_eq!(
            payload.params.get(POINT0_INPUT),
            Some(&NodeValue::Vec2([-100.0, 0.0]))
        );
        assert_eq!(
            payload.params.get(COLOR1_INPUT),
            Some(&NodeValue::Color([1.0, 1.0, 1.0, 1.0]))
        );
    }

    #[test]
    fn value_row_points_win_over_the_node_defaults() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([
            (POINT0_INPUT.to_string(), NodeValue::Vec2([-4.0, 0.0])),
            (POINT1_INPUT.to_string(), NodeValue::Vec2([4.0, 0.0])),
            (COLOR0_INPUT.to_string(), NodeValue::Color([1.0, 0.0, 0.0, 1.0])),
        ]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::jobs::shader_job(&handle) }
            .expect("shader job payload expected");
        assert_eq!(
            payload.params.get(POINT0_INPUT),
            Some(&NodeValue::Vec2([-4.0, 0.0]))
        );
        assert_eq!(
            payload.params.get(COLOR0_INPUT),
            Some(&NodeValue::Color([1.0, 0.0, 0.0, 1.0]))
        );
        // The unset color falls back to the node default.
        assert_eq!(
            payload.params.get(COLOR1_INPUT),
            Some(&NodeValue::Color([1.0, 1.0, 1.0, 1.0]))
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
        let merge = unsafe { crate::jobs::shader_job(&handle) }
            .expect("merge job payload expected");
        assert_eq!(merge.shader_id, "mrg");
        assert_eq!(merge.effect_input, BASE_INPUT);
        match merge.params.get(crate::nodes::merge::BLEND_INPUT) {
            Some(NodeValue::Texture(blend)) => {
                let nested = unsafe { crate::jobs::shader_job(blend) }
                    .expect("nested job payload boxed");
                assert_eq!(nested.shader_id, "ramp");
            }
            _ => panic!("nested blend job expected"),
        }
    }

    #[test]
    fn shader_code_dispatches() {
        let n = RampNode;
        let code = n.shader_code("ramp").unwrap();
        assert!(code.contains("uniform vec2 point0_in;"));
        assert!(code.contains("uniform vec2 point1_in;"));
        assert!(code.contains("uniform vec4 color0_in;"));
        assert!(code.contains("uniform vec4 color1_in;"));
        assert!(code.contains("uniform vec2 resolution_in;"));
        assert!(code.contains("t = dot(px - point0_in, d) / norm2;"));
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
        assert_eq!(dup.name(), "Ramp");
    }
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ramp",
		name: "Ramp",
		categories: &[Category::Generator],
		create,
	});
}
