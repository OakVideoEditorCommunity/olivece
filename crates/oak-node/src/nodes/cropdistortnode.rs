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

//! Crop distort effect (C++
//! `src/node/src/distort/crop/cropdistortnode.{h,cpp}`,
//! `olive::CropDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Left crop input id (C++ `k_left_input`). Type: float; default `0.0`;
/// properties: `min = 0.0`, `max = 1.0`, `view = percentage` (created by
/// C++ `create_crop_side_input()`).
pub const LEFT_INPUT: &str = "left_in";

/// Top crop input id (C++ `k_top_input`). Type: float; default `0.0`;
/// properties: `min = 0.0`, `max = 1.0`, `view = percentage`.
pub const TOP_INPUT: &str = "top_in";

/// Right crop input id (C++ `k_right_input`). Type: float; default
/// `0.0`; properties: `min = 0.0`, `max = 1.0`, `view = percentage`.
pub const RIGHT_INPUT: &str = "right_in";

/// Bottom crop input id (C++ `k_bottom_input`). Type: float; default
/// `0.0`; properties: `min = 0.0`, `max = 1.0`, `view = percentage`.
pub const BOTTOM_INPUT: &str = "bottom_in";

/// Feather input id (C++ `k_feather_input`). Type: float; default
/// `0.0`; properties: `min = 0.0`.
pub const FEATHER_INPUT: &str = "feather_in";

/// Number of crop point gizmos (C++ `k_gizmo_scale_count` from
/// `node.h`: top-left, top-center, top-right, bottom-left,
/// bottom-center, bottom-right, center-left, center-right).
pub const GIZMO_SCALE_COUNT: usize = 8;

/// Crop distort node. Crops the edges of an image with an optional
/// feather.
pub struct CropDistortNode {
	/// Edge/corner drag handles in `k_gizmo_scale_*` order (C++
	/// `PointGizmo *point_gizmo_[k_gizmo_scale_count]`; each handle drags
	/// the one or two crop inputs it touches).
	point_gizmo: [Gizmo; GIZMO_SCALE_COUNT],
	/// Crop rectangle outline gizmo (C++ `PolygonGizmo *poly_gizmo_`;
	/// drags all four crop inputs).
	poly_gizmo: Gizmo,
	/// Resolution captured by the last `update_gizmo_positions()` call,
	/// used to normalize drag deltas (C++ `Vector2D temp_resolution_`).
	temp_resolution: (f64, f64),
}

/// Fragment shader (C++ loads the `:/shaders/crop.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/crop.frag`.
const SHADER_FRAG: &str = r#"// Input variables
uniform sampler2D tex_in;
uniform float left_in;
uniform float top_in;
uniform float right_in;
uniform float bottom_in;
uniform float feather_in;
uniform vec2 resolution_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    float multiplier = 1.0;

    vec2 feather_normalized = vec2(feather_in / resolution_in.x, feather_in / resolution_in.y);
    vec2 feather_normalized_half = feather_normalized * 0.5;

    // Calculate left cropping
    float left_adjustment;
    float right_adjustment;
    float top_adjustment;
    float bottom_adjustment;

    if (feather_in == 0.0) {
        if (ove_texcoord.x < left_in
            || ove_texcoord.x > (1.0-right_in)
            || ove_texcoord.y < (top_in)
            || ove_texcoord.y > (1.0-bottom_in)) {
            multiplier = 0.0;
        }
    } else {
        float left_adjustment = clamp((ove_texcoord.x - (left_in - feather_normalized.x*(1.0-left_in))) / feather_normalized.x, 0.0, 1.0);
        multiplier *= left_adjustment;

        float right_adjustment = 1.0-clamp((ove_texcoord.x - ((1.0-right_in) - feather_normalized.x*(right_in))) / feather_normalized.x, 0.0, 1.0);
        multiplier *= right_adjustment;

        float top_adjustment = clamp((ove_texcoord.y - (top_in - feather_normalized.y*(1.0-top_in))) / feather_normalized.y, 0.0, 1.0);
        multiplier *= top_adjustment;

        float bottom_adjustment = 1.0-clamp((ove_texcoord.y - ((1.0-bottom_in) - feather_normalized.y*(bottom_in))) / feather_normalized.y, 0.0, 1.0);
        multiplier *= bottom_adjustment;
    }

    if (multiplier > 0.0) {
        vec4 color = texture(tex_in, ove_texcoord) * multiplier;
        frag_color = color;
    } else {
        frag_color = vec4(0.0);
    }
}
"#;

impl CropDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for CropDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Crop"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.crop"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Crop the edges of an image."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Texture", `left_in` -> "Left", `top_in` -> "Top", `right_in` ->
	/// "Right", `bottom_in` -> "Bottom", `feather_in` -> "Feather".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			LEFT_INPUT => "Left",
			TOP_INPUT => "Top",
			RIGHT_INPUT => "Right",
			BOTTOM_INPUT => "Bottom",
			FEATHER_INPUT => "Feather",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): copies the whole value row into
	/// a shader job and inserts `resolution_in` from the texture params;
	/// no texture -> push nothing; any of left/right/top/bottom != 0.0 ->
	/// shader job; all zero -> pass-through push of the input texture
	/// unchanged.
	///
	/// The job case boxes a [`ShaderJobPayload`] that the renderer's
	/// resolve hook executes and replaces with the result texture; the
	/// params row carries the input texture and uniforms, keyed by the
	/// effect input, and `resolution_in` is filled by the runner from the
	/// input texture's size, matching the C++ `texture->params()`
	/// insertion (`// CPP-PARITY: cropdistortnode.cpp` `value()`).
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

		let left = match inputs.get(LEFT_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(LEFT_INPUT, -1, time).to_double(),
		};
		let right = match inputs.get(RIGHT_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(RIGHT_INPUT, -1, time).to_double(),
		};
		let top = match inputs.get(TOP_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(TOP_INPUT, -1, time).to_double(),
		};
		let bottom = match inputs.get(BOTTOM_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(BOTTOM_INPUT, -1, time).to_double(),
		};

		if left != 0.0 || right != 0.0 || top != 0.0 || bottom != 0.0 {
			// The shader-job box (C++ `texture->toJob(job)`): the behavior's
			// type id selects the fragment source, and the effect input key
			// locates the main texture inside the params row.
			table.push(
				crate::value::ValueType::Texture,
				crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
					node_id: crate::id::NodeId::INVALID,
					time,
					iterations: 1,
					type_id: self.type_id().to_string(),
					shader_id: String::new(),
					effect_input: core.effect_input.clone(),
					params: inputs.clone(),
					iterative_input: TEXTURE_INPUT.to_string(),
				}))),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and always returns the crop fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): with a texture,
	/// caches the resolution in `temp_resolution`, converts the four 0..1
	/// crop values to pixel points (left/top straight, right/bottom as
	/// `1.0 - value`), places the eight edge/corner point gizmos (center
	/// handles at the midpoints) and the rectangle polygon gizmo.
	///
	/// The pixel points need the texture's virtual resolution (the Rust
	/// texture handle carries no params), the `temp_resolution` cache
	/// needs `&mut self` (the trait hands out `&self`), and the gizmo
	/// point positions have no storage in [`Gizmo`] — so the update is
	/// not representable here (`// CPP-PARITY: cropdistortnode.cpp`
	/// `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): normalizes the mouse delta
	/// by `temp_resolution`, then for each dragger of the current gizmo
	/// adds the delta to its drag-start value with a per-input sign
	/// (left `+x`, top `+y`, right `-x`, bottom `-y`).
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: cropdistortnode.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(CropDistortNode {
			point_gizmo: self.point_gizmo.clone(),
			poly_gizmo: self.poly_gizmo.clone(),
			temp_resolution: self.temp_resolution,
		}))
	}
}

/// Constructor (C++ `CropDistortNode::CropDistortNode()`): adds
/// `tex_in`; adds the four crop side inputs via
/// `create_crop_side_input()` (float, default 0.0, min 0.0, max 1.0,
/// percentage view); adds `feather_in` (float, default 0.0, min 0.0);
/// creates the rectangle polygon gizmo and the eight point gizmos bound
/// to their crop inputs; sets the video-effect flag and the effect
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

	create_crop_side_input(&mut core, LEFT_INPUT);
	create_crop_side_input(&mut core, TOP_INPUT);
	create_crop_side_input(&mut core, RIGHT_INPUT);
	create_crop_side_input(&mut core, BOTTOM_INPUT);

	let mut feather = crate::input::Input::new(
		FEATHER_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	feather.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(feather);

	// Gizmos, in C++ add order: the rectangle polygon gizmo first, then
	// the eight edge/corner point gizmos in `k_gizmo_scale_*` order. Each
	// gizmo records the crop inputs it drags (float inputs, one track).
	let poly_gizmo = Gizmo {
		position_inputs: vec![
			(LEFT_INPUT.to_string(), -1, 0),
			(TOP_INPUT.to_string(), -1, 0),
			(RIGHT_INPUT.to_string(), -1, 0),
			(BOTTOM_INPUT.to_string(), -1, 0),
		],
		drag_point: (0.0, 0.0),
	};
	let tl = Gizmo {
		position_inputs: vec![
			(LEFT_INPUT.to_string(), -1, 0),
			(TOP_INPUT.to_string(), -1, 0),
		],
		drag_point: (0.0, 0.0),
	};
	let tc = Gizmo {
		position_inputs: vec![(TOP_INPUT.to_string(), -1, 0)],
		drag_point: (0.0, 0.0),
	};
	let tr = Gizmo {
		position_inputs: vec![
			(RIGHT_INPUT.to_string(), -1, 0),
			(TOP_INPUT.to_string(), -1, 0),
		],
		drag_point: (0.0, 0.0),
	};
	let bl = Gizmo {
		position_inputs: vec![
			(LEFT_INPUT.to_string(), -1, 0),
			(BOTTOM_INPUT.to_string(), -1, 0),
		],
		drag_point: (0.0, 0.0),
	};
	let bc = Gizmo {
		position_inputs: vec![(BOTTOM_INPUT.to_string(), -1, 0)],
		drag_point: (0.0, 0.0),
	};
	let br = Gizmo {
		position_inputs: vec![
			(RIGHT_INPUT.to_string(), -1, 0),
			(BOTTOM_INPUT.to_string(), -1, 0),
		],
		drag_point: (0.0, 0.0),
	};
	let cl = Gizmo {
		position_inputs: vec![(LEFT_INPUT.to_string(), -1, 0)],
		drag_point: (0.0, 0.0),
	};
	let cr = Gizmo {
		position_inputs: vec![(RIGHT_INPUT.to_string(), -1, 0)],
		drag_point: (0.0, 0.0),
	};

	core.gizmos = vec![
		poly_gizmo.clone(),
		tl.clone(),
		tc.clone(),
		tr.clone(),
		bl.clone(),
		bc.clone(),
		br.clone(),
		cl.clone(),
		cr.clone(),
	];

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(
		core,
		Box::new(CropDistortNode {
			point_gizmo: [tl, tc, tr, bl, bc, br, cl, cr],
			poly_gizmo,
			temp_resolution: (0.0, 0.0),
		}),
	)
}

/// Helper mirroring the C++ `create_crop_side_input()`: a float input
/// with default 0.0 and `min = 0.0`, `max = 1.0`, `view = percentage`
/// properties.
fn create_crop_side_input(core: &mut NodeCore, id: &str) {
	let mut input = crate::input::Input::new(
		id,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	input.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("max".to_string(), crate::value::NodeValue::Float(1.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	core.add_input(input);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn empty_gizmo() -> Gizmo {
		Gizmo {
			position_inputs: vec![],
			drag_point: (0.0, 0.0),
		}
	}

	#[test]
	fn input_names() {
		let n = CropDistortNode {
			point_gizmo: std::array::from_fn(|_| empty_gizmo()),
			poly_gizmo: empty_gizmo(),
			temp_resolution: (0.0, 0.0),
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(LEFT_INPUT), "Left");
		assert_eq!(n.input_name(TOP_INPUT), "Top");
		assert_eq!(n.input_name(RIGHT_INPUT), "Right");
		assert_eq!(n.input_name(BOTTOM_INPUT), "Bottom");
		assert_eq!(n.input_name(FEATHER_INPUT), "Feather");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.crop");
		assert_eq!(
			core.get_input(LEFT_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		assert_eq!(
			core.get_input(BOTTOM_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		assert_eq!(
			core.get_input(FEATHER_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		// Nine gizmos: one polygon + eight points.
		assert_eq!(core.gizmos.len(), 9);
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
	fn value_all_zero_crops_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(LEFT_INPUT, -1, NodeValue::Float(0.0));
		core.set_standard_value(TOP_INPUT, -1, NodeValue::Float(0.0));
		core.set_standard_value(RIGHT_INPUT, -1, NodeValue::Float(0.0));
		core.set_standard_value(BOTTOM_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_any_crop_pushes_shader_job_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(LEFT_INPUT, -1, NodeValue::Float(0.25));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe { crate::jobs::shader_job(&handle) }
			.expect("shader job payload expected");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.crop");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.time, Rational::new(0, 1));
		assert!(payload.params.contains_key(TEXTURE_INPUT));
	}

	#[test]
	fn shader_code_returns_crop_shader() {
		let (_, behavior) = create();
		let code = behavior.shader_code("anything").unwrap();
		assert!(code.contains("uniform float feather_in;"));
		assert!(code.contains("multiplier *= left_adjustment;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Crop");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.crop`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.crop",
		name: "Crop",
		categories: &[Category::Distort],
		create,
	});
}
