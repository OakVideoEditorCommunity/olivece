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

//! Corner pin distort effect (C++
//! `src/node/src/distort/cornerpin/cornerpindistortnode.{h,cpp}`,
//! `olive::CornerPinDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Perspective-correct interpolation input id (C++
/// `k_perspective_input`). Type: bool; default `true`.
pub const PERSPECTIVE_INPUT: &str = "perspective_in";

/// Top-left corner offset input id (C++ `k_top_left_input`). Type:
/// vec2; default `(0.0, 0.0)`; property `offset` is set per-frame by
/// `update_gizmo_positions()` to the corner's pixel origin.
pub const TOP_LEFT_INPUT: &str = "top_left_in";

/// Top-right corner offset input id (C++ `k_top_right_input`). Type:
/// vec2; default `(0.0, 0.0)`; property `offset` is set per-frame to
/// `(resolution.x, 0)`.
pub const TOP_RIGHT_INPUT: &str = "top_right_in";

/// Bottom-right corner offset input id (C++ `k_bottom_right_input`).
/// Type: vec2; default `(0.0, 0.0)`; property `offset` is set per-frame
/// to the full resolution.
pub const BOTTOM_RIGHT_INPUT: &str = "bottom_right_in";

/// Bottom-left corner offset input id (C++ `k_bottom_left_input`).
/// Type: vec2; default `(0.0, 0.0)`; property `offset` is set per-frame
/// to `(0, resolution.y)`.
pub const BOTTOM_LEFT_INPUT: &str = "bottom_left_in";

/// Number of corner point gizmos (C++ `k_gizmo_corner_count`).
pub const GIZMO_CORNER_COUNT: usize = 4;

/// Corner pin distort node. Warps the image by dragging its four
/// corners, optionally with perspective-correct interpolation.
pub struct CornerPinDistortNode {
	/// Corner drag handles, one per corner in top-left, top-right,
	/// bottom-right, bottom-left order (C++
	/// `PointGizmo *gizmo_resize_handle_[k_gizmo_corner_count]`; each
	/// drags both tracks of its corner input).
	gizmo_resize_handle: [Gizmo; GIZMO_CORNER_COUNT],
	/// Whole-quad outline gizmo (C++ `PolygonGizmo *gizmo_whole_rect_`;
	/// draggable but ignored by `gizmo_drag_move`, so dragging it does
	/// nothing).
	gizmo_whole_rect: Gizmo,
}

/// Fragment shader (C++ loads the `:/shaders/cornerpin.frag` resource
/// in `get_shader_code`). Text copied verbatim from
/// `engine/shaders/cornerpin.frag`.
const SHADER_FRAG: &str = r#"// Input texture
uniform sampler2D ove_maintex;
uniform sampler2D tex_in;
uniform bool perspective_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

in vec2 q;
in vec2 b1;
in vec2 b2;
in vec2 b3;

float Wedge2D(vec2 v, vec2 w) {
  return (v.x*w.y) - (v.y*w.x);
}

void main() {
    if(perspective_in){
        frag_color = texture(tex_in, ove_texcoord);
    } else {
      float A = Wedge2D(b2, b3);
    float B = Wedge2D(b3, q) - Wedge2D(b1, b2);
    float C = Wedge2D(b1, q);

    vec2 uv;

    // solve for v
    if (abs(A) < 0.001) {
      uv.y = -C/B;
    } else {
      float discrim = B*B - 4.0*A*C;
      uv.y = 0.5 * (-B + sqrt(discrim)) / A;
    }

    // solve for u
    vec2 denom = b1 + uv.y * b3;
    if (abs(denom.x) > abs(denom.y)) {
      uv.x = (q.x - b2.x * uv.y) / denom.x;
    } else {
      uv.x = (q.y - b2.y * uv.y) / denom.y;
    }

    uv.y = 1.0 - uv.y;

    frag_color = texture(tex_in, uv);
  }
}
"#;

/// Vertex shader (C++ loads the `:/shaders/cornerpin.vert` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/cornerpin.vert`.
const SHADER_VERT: &str = r#"uniform bool perspective_in;
uniform vec2 top_left_in;
uniform vec2 top_right_in;
uniform vec2 bottom_left_in;
uniform vec2 bottom_right_in;

uniform vec2 resolution_in;

uniform mat4 ove_mvpmat;

in vec4 a_position;
in vec2 a_texcoord;

out vec2 ove_texcoord;

out vec2 q;
out vec2 b1;
out vec2 b2;
out vec2 b3;

void main() {
    // The slider inputs only contain the amount they have changed rather than
    // their pixel locations so we adjust them here.
    vec2 t_l = top_left_in;
    vec2 t_r = top_right_in + vec2(resolution_in.x, 0.0);
    vec2 b_r = bottom_right_in + resolution_in;
    vec2 b_l = bottom_left_in + vec2(0.0, resolution_in.y);

    gl_Position = ove_mvpmat * a_position;

    if (perspective_in){
        // Find the center of the quadrilateral by finding where the two diagonals intersect.
        // https://www.reedbeta.com/blog/quadrilateral-interpolation-part-1/

        // Here we calculate the gradient and constant (y = mx + c) for each diagonal.
        float m1 = (t_r.y - b_l.y)/(t_r.x - b_l.x);
        float c1 = b_l.y - m1 * b_l.x;
        float m2 = (b_r.y - t_l.y)/(b_r.x - t_l.x);
        float c2 = t_l.y - m2 * t_l.x;

        // Find the intersection by setting the two line equations equal and rearrange.
        float mid_x = (c2 - c1) / (m1 - m2);
        float mid_y = m1 * mid_x + c1;

        // Find the distance from each corner to our center point
        float d0 = length(vec2(mid_x - b_l.x, mid_y - b_l.y));
        float d1 = length(vec2(b_r.x - mid_x, mid_y - b_r.y));
        float d2 = length(vec2(t_r.x - mid_x, t_r.y - mid_y));
        float d3 = length(vec2(mid_x - t_l.x, t_l.y - mid_y));

        float q  = 1.0;

        /*
        Vertex IDs (aspect ratio irrelevant):
             0_____1
            3|\    |
             | \   |
             |  \  |
             |   \ |
             |____\|2
             4     5
        */

        if (gl_VertexID == 0 || gl_VertexID == 3) {
            q = (d1+d3)/d3;
        } else if (gl_VertexID == 1) {
            q = (d0+d2)/d2;
        } else if (gl_VertexID == 2 || gl_VertexID == 5) {
            q = (d3+d1)/d1;
        } else {
            q = (d2+d0)/d0;
        }

        gl_Position[0] *= q;
        gl_Position[1] *= q;
        gl_Position[3] = q;
    } else{
        // https://www.reedbeta.com/blog/quadrilateral-interpolation-part-2/
        vec2 pos;

        if (gl_VertexID == 0 || gl_VertexID == 3) { // top left
            pos = t_l;
        } else if (gl_VertexID == 1) { // top right
            pos = t_r;
        } else if (gl_VertexID == 2 || gl_VertexID == 5) { // bottom right
            pos = b_r;
        } else if (gl_VertexID == 4) { // bottom left
            pos = b_l;
        }

        q = pos - b_l;
        b1 = b_r - b_l;
        b2 = t_l - b_l;
        b3 = b_l - b_r - t_l + t_r;
    }


    ove_texcoord = a_texcoord;
}
"#;

impl CornerPinDistortNode {
	/// Fragment shader (C++ `get_shader_code()` frag half; the request
	/// id is ignored).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}

	/// Vertex shader (C++ `get_shader_code()` vert half; the request id
	/// is ignored).
	fn shader_vert() -> &'static str {
		SHADER_VERT
	}
}

impl NodeBehavior for CornerPinDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Corner Pin"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.cornerpin"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Distort the image by dragging the corners."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Texture", `perspective_in` -> "Perspective", `top_left_in` ->
	/// "Top Left", `top_right_in` -> "Top Right", `bottom_right_in` ->
	/// "Bottom Right", `bottom_left_in` -> "Bottom Left".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			PERSPECTIVE_INPUT => "Perspective",
			TOP_LEFT_INPUT => "Top Left",
			TOP_RIGHT_INPUT => "Top Right",
			BOTTOM_RIGHT_INPUT => "Bottom Right",
			BOTTOM_LEFT_INPUT => "Bottom Left",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// all four corner sliders at their `(0, 0)` default -> pass-through
	/// push of the input texture unchanged; otherwise push a shader job.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input,
	/// and `resolution_in` is filled by the runner from the input
	/// texture's size, matching the C++ insert of the texture's virtual
	/// resolution (`// CPP-PARITY: cornerpindistortnode.cpp` value()).
	///
	/// TODO(vertex-shader): C++ additionally overrides the vertex
	/// coordinates — each corner offset converted to pixels via
	/// `value_to_pixel` and then to clip space (`/ half_resolution - 1.0`),
	/// pushed as two triangles (TL, TR, BR / TL, BL, BR) via
	/// `job.SetVertexCoordinates(...)` — and the quad warp needs the custom
	/// vertex shader `cornerpin.vert` (`ove_mvpmat`), which the
	/// [`ShaderJobPayload`] (no vertex field) and the single-fragment
	/// `shader_code()` seam cannot carry. The fragment path (perspective
	/// interpolation) is testable without it.
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

		// C++ `to_vec2().is_null()`: a corner is at its default only when
		// both components are zero.
		let corner_is_null = |id: &str| {
			let v = match inputs.get(id) {
				Some(crate::value::NodeValue::Vec2(v)) => *v,
				_ => match core.value_at_time(id, -1, time) {
					crate::value::NodeValue::Vec2(v) => v,
					v => [v.to_double(), v.to_double()],
				},
			};
			v[0] == 0.0 && v[1] == 0.0
		};

		if !(corner_is_null(TOP_LEFT_INPUT)
			&& corner_is_null(TOP_RIGHT_INPUT)
			&& corner_is_null(BOTTOM_RIGHT_INPUT)
			&& corner_is_null(BOTTOM_LEFT_INPUT))
		{
			// The shader-job box (C++ ShaderJob): the behavior's type id
			// selects the fragment source; the effect input key locates the
			// main texture inside the params row.
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
					iterative_input: String::new(),
				}))),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and always returns the corner pin fragment and vertex
	/// shaders together.
	///
	/// The trait returns only the fragment shader, so the vertex shader
	/// (see [`SHADER_VERT`]) is not representable in the return value.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): with a texture,
	/// converts the four corner offsets to pixels (`value_to_pixel`,
	/// which adds the corner's resolution-based origin), sets each corner
	/// input's `offset` property to that origin, sets the polygon gizmo
	/// to the quad TL->TR->BR->BL->TL and each point gizmo to its
	/// corner. Also covers C++ `value_to_pixel()`.
	///
	/// The pixel conversions and `offset` property writes need the
	/// texture's virtual resolution (the Rust texture handle carries no
	/// params), and the gizmo point positions have no storage in
	/// [`Gizmo`] — so the update is not representable here
	/// (`// CPP-PARITY: cornerpindistortnode.cpp`
	/// `update_gizmo_positions`). [`Self::value_to_pixel`] below ports the
	/// pure math for tests.
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): for a corner handle, drags
	/// both its X and Y track draggers by the mouse delta added to their
	/// drag-start values; dragging the whole-rect polygon gizmo is a
	/// no-op.
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: cornerpindistortnode.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(CornerPinDistortNode {
			gizmo_resize_handle: self.gizmo_resize_handle.clone(),
			gizmo_whole_rect: self.gizmo_whole_rect.clone(),
		}))
	}
}

impl CornerPinDistortNode {
	/// Corner offset to pixel position (C++ `value_to_pixel()`): the
	/// four corner inputs hold offsets from their resolution-based
	/// origin, so corner 0 (top-left) maps straight, corner 1 (top-right)
	/// adds `(resolution.x, 0)`, corner 2 (bottom-right) adds the full
	/// resolution, and corner 3 (bottom-left) adds `(0, resolution.y)`.
	fn value_to_pixel(
		value: i32,
		row: &crate::value::NodeValueRow,
		resolution: (f64, f64),
	) -> (f64, f64) {
		let vec_at = |id: &str| match row.get(id) {
			Some(crate::value::NodeValue::Vec2(v)) => *v,
			Some(v) => [v.to_double(), 0.0],
			None => [0.0, 0.0],
		};
		match value {
			0 => {
				let v = vec_at(TOP_LEFT_INPUT);
				(v[0], v[1])
			}
			1 => {
				let v = vec_at(TOP_RIGHT_INPUT);
				(resolution.0 + v[0], v[1])
			}
			2 => {
				let v = vec_at(BOTTOM_RIGHT_INPUT);
				(resolution.0 + v[0], resolution.1 + v[1])
			}
			3 => {
				let v = vec_at(BOTTOM_LEFT_INPUT);
				(v[0], resolution.1 + v[1])
			}
			_ => (0.0, 0.0),
		}
	}
}

/// Constructor (C++ `CornerPinDistortNode::CornerPinDistortNode()`):
/// adds `tex_in`, `perspective_in` and the four corner inputs with the
/// defaults and flags documented on the constants; creates the polygon
/// gizmo and the four corner point gizmos (each bound to both tracks of
/// its corner input); sets the video-effect flag and the effect input.
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
		PERSPECTIVE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	));
	core.add_input(crate::input::Input::new(
		TOP_LEFT_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		TOP_RIGHT_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		BOTTOM_RIGHT_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		BOTTOM_LEFT_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));

	// Gizmos, in C++ add order: the whole-quad polygon gizmo first, then
	// the four corner point gizmos (each dragging both tracks of its
	// corner input) in TL, TR, BR, BL order.
	let corner_gizmo = |id: &str| Gizmo {
		position_inputs: vec![(id.to_string(), -1, 0), (id.to_string(), -1, 1)],
		drag_point: (0.0, 0.0),
	};
	let gizmo_whole_rect = Gizmo {
		position_inputs: vec![],
		drag_point: (0.0, 0.0),
	};
	let tl = corner_gizmo(TOP_LEFT_INPUT);
	let tr = corner_gizmo(TOP_RIGHT_INPUT);
	let br = corner_gizmo(BOTTOM_RIGHT_INPUT);
	let bl = corner_gizmo(BOTTOM_LEFT_INPUT);

	core.gizmos = vec![
		gizmo_whole_rect.clone(),
		tl.clone(),
		tr.clone(),
		br.clone(),
		bl.clone(),
	];

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(
		core,
		Box::new(CornerPinDistortNode {
			gizmo_resize_handle: [tl, tr, br, bl],
			gizmo_whole_rect,
		}),
	)
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
		let n = CornerPinDistortNode {
			gizmo_resize_handle: std::array::from_fn(|_| empty_gizmo()),
			gizmo_whole_rect: empty_gizmo(),
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(PERSPECTIVE_INPUT), "Perspective");
		assert_eq!(n.input_name(TOP_LEFT_INPUT), "Top Left");
		assert_eq!(n.input_name(TOP_RIGHT_INPUT), "Top Right");
		assert_eq!(n.input_name(BOTTOM_RIGHT_INPUT), "Bottom Right");
		assert_eq!(n.input_name(BOTTOM_LEFT_INPUT), "Bottom Left");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.cornerpin");
		assert_eq!(
			core.get_input(PERSPECTIVE_INPUT).unwrap().default,
			NodeValue::Boolean(true)
		);
		assert_eq!(
			core.get_input(TOP_LEFT_INPUT).unwrap().default,
			NodeValue::Vec2([0.0, 0.0])
		);
		// Five gizmos: one polygon + four corner points.
		assert_eq!(core.gizmos.len(), 5);
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
	fn value_default_corners_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(TOP_LEFT_INPUT, -1, NodeValue::Vec2([0.0, 0.0]));
		core.set_standard_value(TOP_RIGHT_INPUT, -1, NodeValue::Vec2([0.0, 0.0]));
		core.set_standard_value(BOTTOM_RIGHT_INPUT, -1, NodeValue::Vec2([0.0, 0.0]));
		core.set_standard_value(BOTTOM_LEFT_INPUT, -1, NodeValue::Vec2([0.0, 0.0]));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_moved_corner_pushes_job_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(TOP_RIGHT_INPUT, -1, NodeValue::Vec2([10.0, 5.0]));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let payload = unsafe { crate::jobs::shader_job(handle) }
			.expect("cornerpin output boxes a ShaderJobPayload");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.cornerpin");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn value_corner_moved_on_y_only_pushes_job_payload() {
		// C++ `is_null()` requires both components zero: a corner at
		// (0, 5) is not at its default.
		let (mut core, behavior) = create();
		core.set_standard_value(BOTTOM_LEFT_INPUT, -1, NodeValue::Vec2([0.0, 5.0]));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		// A corner off its default (y-only is enough — C++ `is_null()`
		// requires BOTH components zero) pushes the job; a pass-through
		// would push the input's own (null) handle instead.
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("texture expected");
		};
		assert!(
			unsafe { crate::jobs::shader_job(h) }.is_some(),
			"a moved corner must push the cornerpin shader job"
		);
	}

	#[test]
	fn value_to_pixel_adds_resolution_origins() {
		let res = (1920.0, 1080.0);
		let mut row = crate::value::NodeValueRow::new();
		row.insert(TOP_LEFT_INPUT.to_string(), NodeValue::Vec2([5.0, 6.0]));
		row.insert(TOP_RIGHT_INPUT.to_string(), NodeValue::Vec2([7.0, 8.0]));
		row.insert(BOTTOM_RIGHT_INPUT.to_string(), NodeValue::Vec2([9.0, 10.0]));
		row.insert(BOTTOM_LEFT_INPUT.to_string(), NodeValue::Vec2([11.0, 12.0]));
		assert_eq!(
			CornerPinDistortNode::value_to_pixel(0, &row, res),
			(5.0, 6.0)
		);
		assert_eq!(
			CornerPinDistortNode::value_to_pixel(1, &row, res),
			(1927.0, 8.0)
		);
		assert_eq!(
			CornerPinDistortNode::value_to_pixel(2, &row, res),
			(1929.0, 1090.0)
		);
		assert_eq!(
			CornerPinDistortNode::value_to_pixel(3, &row, res),
			(11.0, 1092.0)
		);
	}

	#[test]
	fn shader_code_returns_cornerpin_fragment() {
		let (_, behavior) = create();
		let code = behavior.shader_code("anything").unwrap();
		assert!(code.contains("uniform sampler2D ove_maintex;"));
		assert!(code.contains("Wedge2D"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Corner Pin");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.cornerpin`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.cornerpin",
		name: "Corner Pin",
		categories: &[Category::Distort],
		create,
	});
}
