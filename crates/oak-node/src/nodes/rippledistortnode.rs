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

//! Ripple distort effect (C++
//! `src/node/src/distort/ripple/rippledistortnode.{h,cpp}`,
//! `olive::RippleDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Evolution input id (C++ `k_evolution_input`). Type: float; default
/// `0`.
pub const EVOLUTION_INPUT: &str = "evolution_in";

/// Intensity input id (C++ `k_intensity_input`). Type: float; default
/// `100`.
pub const INTENSITY_INPUT: &str = "intensity_in";

/// Frequency input id (C++ `k_frequency_input`). Type: float; default
/// `1`; properties: `base = 0.01`.
pub const FREQUENCY_INPUT: &str = "frequency_in";

/// Center position input id (C++ `k_position_input`). Type: vec2;
/// default `(0, 0)`.
pub const POSITION_INPUT: &str = "position_in";

/// Stretch input id (C++ `k_stretch_input`). Type: bool; default
/// `false`; when off the ripple is aspect-corrected in the shader.
pub const STRETCH_INPUT: &str = "stretch_in";

/// Ripple distort node. Radiates concentric wave displacement from a
/// center point.
pub struct RippleDistortNode {
	/// Center position drag handle (C++ `PointGizmo *gizmo_`; anchor
	/// point shape, drags both tracks of `position_in`).
	gizmo: Gizmo,
}

/// Fragment shader (C++ loads the `:/shaders/ripple.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/ripple.frag`.
const SHADER_FRAG: &str = r#"uniform float evolution_in;
uniform float intensity_in;
uniform float frequency_in;
uniform vec2 position_in;
uniform bool stretch_in;

uniform vec2 resolution_in;
uniform sampler2D tex_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
  vec2 center = position_in/resolution_in;

  vec2 adj_texcoord = ove_texcoord;

  adj_texcoord -= 0.5;
  if (!stretch_in) {
    // Adjust by aspect ratio
    float ar = (resolution_in.x/resolution_in.y);
    if (resolution_in.x > resolution_in.y) {
      adj_texcoord.y /= ar;
      center.y /= ar;
    } else {
      adj_texcoord.x *= ar;
      center.x *= ar;
    }
  }
  adj_texcoord += 0.5;
  center += 0.5;

  adj_texcoord -= center;

  float len = length(adj_texcoord);
  vec2 uv = ove_texcoord + (adj_texcoord/len)*cos((frequency_in)*(len*12.0-evolution_in))*(intensity_in*0.0005);
  // Displacement past the frame edge leaves transparent pixels, not the
  // clamped edge pixels.
  vec4 col = texture(tex_in, uv);
  float inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
  frag_color = col * inside;
}
"#;

impl RippleDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for RippleDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Ripple"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ripple"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Distorts an image with a ripple effect."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `frequency_in` -> "Frequency", `intensity_in` ->
	/// "Intensity", `evolution_in` -> "Evolution", `position_in` ->
	/// "Position", `stretch_in` -> "Stretch".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			FREQUENCY_INPUT => "Frequency",
			INTENSITY_INPUT => "Intensity",
			EVOLUTION_INPUT => "Evolution",
			POSITION_INPUT => "Position",
			STRETCH_INPUT => "Stretch",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// intensity != 0.0 -> shader job over the whole value row;
	/// intensity == 0.0 -> pass-through push of the input texture
	/// unchanged.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input,
	/// and `resolution_in` is filled by the runner from the input
	/// texture's size, matching the C++ insert of the texture's virtual
	/// resolution (`// CPP-PARITY: rippledistortnode.cpp` value()).
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

		let intensity = match inputs.get(INTENSITY_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(INTENSITY_INPUT, -1, time).to_double(),
		};

		if intensity != 0.0 {
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
	/// request id and always returns the ripple fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): with a texture,
	/// places the position gizmo at the texture's half resolution plus
	/// the `position_in` offset.
	///
	/// The placement needs the texture's virtual resolution (the Rust
	/// texture handle carries no params) and the resulting point has no
	/// storage in [`Gizmo`] — not representable here
	/// (`// CPP-PARITY: rippledistortnode.cpp`
	/// `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): drags the position input's
	/// X and Y track draggers by the mouse delta added to their
	/// drag-start values.
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: rippledistortnode.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(RippleDistortNode {
			gizmo: self.gizmo.clone(),
		}))
	}
}

/// Constructor (C++ `RippleDistortNode::RippleDistortNode()`): adds
/// `tex_in`, `evolution_in`, `intensity_in`, `frequency_in`,
/// `position_in` and `stretch_in` with the defaults and properties
/// documented on the constants; creates the anchor-shaped position point
/// gizmo bound to both tracks of `position_in`; sets the video-effect
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
		EVOLUTION_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));
	core.add_input(crate::input::Input::new(
		INTENSITY_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(100.0),
	));

	let mut frequency = crate::input::Input::new(
		FREQUENCY_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	frequency.properties = vec![("base".to_string(), crate::value::NodeValue::Float(0.01))];
	core.add_input(frequency);

	core.add_input(crate::input::Input::new(
		POSITION_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		STRETCH_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	// Anchor-shaped position point gizmo (C++ `PointGizmo` with
	// `k_anchor_point` shape) dragging both tracks of `position_in`.
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

	(core, Box::new(RippleDistortNode { gizmo }))
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
		let n = RippleDistortNode {
			gizmo: Gizmo {
				position_inputs: vec![],
				drag_point: (0.0, 0.0),
			},
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(FREQUENCY_INPUT), "Frequency");
		assert_eq!(n.input_name(INTENSITY_INPUT), "Intensity");
		assert_eq!(n.input_name(EVOLUTION_INPUT), "Evolution");
		assert_eq!(n.input_name(POSITION_INPUT), "Position");
		assert_eq!(n.input_name(STRETCH_INPUT), "Stretch");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ripple");
		assert_eq!(
			core.get_input(INTENSITY_INPUT).unwrap().default,
			NodeValue::Float(100.0)
		);
		assert_eq!(
			core.get_input(FREQUENCY_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		// One anchor-shaped position gizmo bound to both tracks.
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
	fn value_zero_intensity_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(INTENSITY_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_nonzero_intensity_pushes_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(INTENSITY_INPUT.to_string(), NodeValue::Float(100.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
			.expect("ripple output boxes a ShaderJobPayload");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.ripple");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn shader_code_returns_ripple_shader() {
		let (_, behavior) = create();
		let code = behavior.shader_code("anything").unwrap();
		assert!(code.contains("uniform float intensity_in;"));
		assert!(code.contains("adj_texcoord -= center;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Ripple");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.ripple`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ripple",
		name: "Ripple",
		categories: &[Category::Distort],
		create,
	});
}
