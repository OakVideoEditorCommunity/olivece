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

//! Swirl distort effect (C++
//! `src/node/src/distort/swirl/swirldistortnode.{h,cpp}`,
//! `olive::SwirlDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Radius input id (C++ `k_radius_input`). Type: float; default `200`;
/// properties: `min = 0`.
pub const RADIUS_INPUT: &str = "radius_in";

/// Angle input id (C++ `k_angle_input`). Type: float; default `10`;
/// properties: `base = 0.1`.
pub const ANGLE_INPUT: &str = "angle_in";

/// Center position input id (C++ `k_position_input`). Type: vec2;
/// default `(0, 0)`.
pub const POSITION_INPUT: &str = "pos_in";

/// Swirl distort node. Rotates the image around a center point with a
/// falloff by radius.
pub struct SwirlDistortNode {
	/// Center position drag handle (C++ `PointGizmo *gizmo_`; anchor
	/// point shape, drags both tracks of `pos_in`).
	gizmo: Gizmo,
}

/// Fragment shader (C++ loads the `:/shaders/swirl.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/swirl.frag`.
const SHADER_FRAG: &str = r#"// Swirl effect parameters
uniform float radius_in;
uniform float angle_in;
uniform vec2 pos_in;
uniform vec2 resolution_in;

uniform sampler2D tex_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
  vec2 center = resolution_in*0.5 + pos_in;

  vec2 uv = ove_texcoord;

  vec2 tc = uv * resolution_in;
  tc -= center;
  float dist = length(tc);
  if (dist < radius_in) {
    float percent = (radius_in - dist) / radius_in;
    float theta = percent * percent * -angle_in;
    float s = sin(theta);
    float c = cos(theta);
    tc = vec2(dot(tc, vec2(c, -s)), dot(tc, vec2(s, c)));
  }
  tc += center;
  vec2 uv = tc / resolution_in;
  // A swirl displacing content past the frame edge leaves transparent
  // pixels, not the clamped edge pixels.
  vec4 col = texture(tex_in, uv);
  float inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
  frag_color = col * inside;
}
"#;

impl SwirlDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for SwirlDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Swirl"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.swirl"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Distorts an image by swirling it around a center point."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `radius_in` -> "Radius", `angle_in` -> "Angle",
	/// `pos_in` -> "Position".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			RADIUS_INPUT => "Radius",
			ANGLE_INPUT => "Angle",
			POSITION_INPUT => "Position",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// angle != 0.0 AND radius != 0.0 -> shader job over the whole value
	/// row; otherwise pass-through push of the input texture unchanged.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input,
	/// and `resolution_in` is filled by the runner from the input
	/// texture's size, matching the C++ insert of the texture's virtual
	/// resolution (`// CPP-PARITY: swirldistortnode.cpp` value()).
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

		let radius = match inputs.get(RADIUS_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(RADIUS_INPUT, -1, time).to_double(),
		};
		let angle = match inputs.get(ANGLE_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(ANGLE_INPUT, -1, time).to_double(),
		};

		if angle != 0.0 && radius != 0.0 {
			// The shader-job box (C++ ShaderJob): the behavior's type id
			// selects the fragment source; the effect input key locates the
			// main texture inside the params row.
			table.push(
				crate::value::ValueType::Texture,
				crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
					node_id: crate::id::NodeId::INVALID,
					time,
					iterations: 1,
					type_id: self.type_id().to_string(),
					shader_id: String::new(),
					effect_input: core.effect_input.clone(),
					params: inputs.clone(),
					iterative_input: String::new(),
				})),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and always returns the swirl fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): places the
	/// position gizmo at half the globals' square resolution plus the
	/// `pos_in` offset (note: unlike Ripple, this does not require a
	/// texture).
	///
	/// The placement needs the square resolution from the C++ globals,
	/// which this signature does not carry, and the resulting point has
	/// no storage in [`Gizmo`] — not representable here
	/// (`// CPP-PARITY: swirldistortnode.cpp` `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): drags the position input's
	/// X and Y track draggers by the mouse delta added to their
	/// drag-start values.
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: swirldistortnode.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(SwirlDistortNode {
			gizmo: self.gizmo.clone(),
		}))
	}
}

/// Constructor (C++ `SwirlDistortNode::SwirlDistortNode()`): adds
/// `tex_in`, `radius_in`, `angle_in` and `pos_in` with the defaults and
/// properties documented on the constants; creates the anchor-shaped
/// position point gizmo bound to both tracks of `pos_in`; sets the
/// video-effect flag and the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut radius = crate::input::Input::new(
		RADIUS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(200.0),
	);
	radius.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(radius);

	let mut angle = crate::input::Input::new(
		ANGLE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	);
	angle.properties = vec![("base".to_string(), crate::value::NodeValue::Float(0.1))];
	core.add_input(angle);

	core.add_input(crate::input::Input::new(
		POSITION_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));

	// Anchor-shaped position point gizmo (C++ `PointGizmo` with
	// `k_anchor_point` shape) dragging both tracks of `pos_in`.
	let gizmo = Gizmo {
		position_inputs: vec![
			(POSITION_INPUT.to_string(), -1, 0),
			(POSITION_INPUT.to_string(), -1, 1),
		],
		drag_point: (0.0, 0.0),
	};
	core.gizmos = vec![gizmo.clone()];

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(SwirlDistortNode { gizmo }))
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

	#[test]
	fn input_names() {
		let n = SwirlDistortNode {
			gizmo: Gizmo {
				position_inputs: vec![],
				drag_point: (0.0, 0.0),
			},
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(RADIUS_INPUT), "Radius");
		assert_eq!(n.input_name(ANGLE_INPUT), "Angle");
		assert_eq!(n.input_name(POSITION_INPUT), "Position");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.swirl");
		assert_eq!(
			core.get_input(RADIUS_INPUT).unwrap().default,
			NodeValue::Float(200.0)
		);
		assert_eq!(
			core.get_input(ANGLE_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(core.gizmos.len(), 1);
		assert_eq!(core.gizmos[0].position_inputs.len(), 2);
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
	fn value_zero_angle_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(ANGLE_INPUT, -1, NodeValue::Float(0.0));
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(200.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_zero_radius_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(ANGLE_INPUT, -1, NodeValue::Float(10.0));
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_angle_and_radius_pushes_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(ANGLE_INPUT.to_string(), NodeValue::Float(10.0)),
			(RADIUS_INPUT.to_string(), NodeValue::Float(200.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
			.expect("swirl output boxes a ShaderJobPayload");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.swirl");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn shader_code_returns_swirl_shader() {
		let (_, behavior) = create();
		let code = behavior.shader_code("anything").unwrap();
		assert!(code.contains("uniform float radius_in;"));
		assert!(code.contains("percent * percent * -angle_in"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Swirl");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.swirl`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.swirl",
		name: "Swirl",
		categories: &[Category::Distort],
		create,
	});
}
