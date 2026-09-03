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

//! Tile distort effect (C++
//! `src/node/src/distort/tile/tiledistortnode.{h,cpp}`,
//! `olive::TileDistortNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Scale input id (C++ `k_scale_input`). Type: float; default `0.5`;
/// properties: `min = 0`, `view = percentage`.
pub const SCALE_INPUT: &str = "scale_in";

/// Position input id (C++ `k_position_input`). Type: vec2; default
/// `(0, 0)`.
pub const POSITION_INPUT: &str = "position_in";

/// Anchor combo input id (C++ `k_anchor_input`). Type: combo; default
/// `4` (C++ `k_middle_center`); combo strings: "Top-Left",
/// "Top-Center", "Top-Right", "Middle-Left", "Middle-Center",
/// "Middle-Right", "Bottom-Left", "Bottom-Center", "Bottom-Right".
pub const ANCHOR_INPUT: &str = "anchor_in";

/// Horizontal mirror input id (C++ `k_mirror_x_input`). Type: bool;
/// default `false`.
pub const MIRROR_X_INPUT: &str = "mirrorx_in";

/// Vertical mirror input id (C++ `k_mirror_y_input`). Type: bool;
/// default `false`.
pub const MIRROR_Y_INPUT: &str = "mirrory_in";

/// Anchor point for tiling (C++ private enum `Anchor`); values match
/// the `anchor_in` combo indices and the shader's `anchor_in` defines.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
	/// Top-left corner.
	TopLeft = 0,
	/// Top edge center.
	TopCenter = 1,
	/// Top-right corner.
	TopRight = 2,
	/// Left edge center.
	MiddleLeft = 3,
	/// Image center.
	MiddleCenter = 4,
	/// Right edge center.
	MiddleRight = 5,
	/// Bottom-left corner.
	BottomLeft = 6,
	/// Bottom edge center.
	BottomCenter = 7,
	/// Bottom-right corner.
	BottomRight = 8,
}

/// Tile distort node. Repeats the image infinitely in both directions,
/// optionally mirroring alternate tiles.
pub struct TileDistortNode {
	/// Position drag handle (C++ `PointGizmo *gizmo_`; anchor point
	/// shape, drags both tracks of `position_in`).
	gizmo: Gizmo,
}

/// Fragment shader (C++ loads the `:/shaders/tile.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/tile.frag`.
const SHADER_FRAG: &str = r#"uniform float scale_in;
uniform vec2 position_in;
uniform vec2 resolution_in;
uniform bool mirrorx_in;
uniform bool mirrory_in;
uniform int anchor_in;

uniform sampler2D tex_in;

in vec2 ove_texcoord;
out vec4 frag_color;

#define TOP_LEFT        0
#define TOP_CENTER      1
#define TOP_RIGHT       2
#define MIDDLE_LEFT     3
#define MIDDLE_CENTER   4
#define MIDDLE_RIGHT    5
#define BOTTOM_LEFT     6
#define BOTTOM_CENTER   7
#define BOTTOM_RIGHT    8

void main(void) {
  vec2 coord = ove_texcoord;

  vec2 offset;

  if (anchor_in == TOP_LEFT || anchor_in == TOP_CENTER || anchor_in == TOP_RIGHT) {
    offset.y = 0.0;
  } else if (anchor_in == MIDDLE_LEFT || anchor_in == MIDDLE_CENTER || anchor_in == MIDDLE_RIGHT) {
    offset.y = 0.5;
  } else if (anchor_in == BOTTOM_LEFT || anchor_in == BOTTOM_CENTER || anchor_in == BOTTOM_RIGHT) {
    offset.y = 1.0;
  }

  if (anchor_in == TOP_LEFT || anchor_in == MIDDLE_LEFT || anchor_in == BOTTOM_LEFT) {
    offset.x = 0.0;
  } else if (anchor_in == TOP_CENTER || anchor_in == MIDDLE_CENTER || anchor_in == BOTTOM_CENTER) {
    offset.x = 0.5;
  } else if (anchor_in == TOP_RIGHT || anchor_in == MIDDLE_RIGHT || anchor_in == BOTTOM_RIGHT) {
    offset.x = 1.0;
  }

  coord -= position_in/resolution_in;

  coord -= offset;
  coord /= scale_in;
  coord += offset;

  vec2 modcoord = mod(coord, 1.0);

  if (mirrorx_in && mod(coord.x, 2.0) > 1.0) {
    modcoord.x = 1.0 - modcoord.x;
  }

  if (mirrory_in && mod(coord.y, 2.0) > 1.0) {
    modcoord.y = 1.0 - modcoord.y;
  }

  frag_color = vec4(texture(tex_in, modcoord));
}
"#;

impl TileDistortNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored, this is the only shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for TileDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Tile"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.tile"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Infinitely tile an image horizontally and vertically."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Input", `scale_in` -> "Scale", `position_in` -> "Position",
	/// `mirrorx_in` -> "Mirror Horizontally", `mirrory_in` -> "Mirror
	/// Vertically", `anchor_in` -> "Anchor" (with the nine combo strings
	/// documented on [`ANCHOR_INPUT`]).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			SCALE_INPUT => "Scale",
			POSITION_INPUT => "Position",
			MIRROR_X_INPUT => "Mirror Horizontally",
			MIRROR_Y_INPUT => "Mirror Vertically",
			// The `anchor_in` combo strings ("Top-Left" ... "Bottom-Right")
			// are a UI-level property of the input (C++
			// `set_combo_box_strings`).
			ANCHOR_INPUT => "Anchor",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `anchor_in` -> "Top-Left", "Top-Center",
	/// "Top-Right", "Middle-Left", "Middle-Center", "Middle-Right",
	/// "Bottom-Left", "Bottom-Center", "Bottom-Right".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			ANCHOR_INPUT => vec![
				"Top-Left",
				"Top-Center",
				"Top-Right",
				"Middle-Left",
				"Middle-Center",
				"Middle-Right",
				"Bottom-Left",
				"Bottom-Center",
				"Bottom-Right",
			],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// scale differs from 1.0 (an approximate-equality epsilon test:
	/// `abs(scale-1)*1e12 > min(abs(scale), 1)`) -> shader job over the
	/// whole value row; scale ~== 1.0 -> pass-through push of the input
	/// texture unchanged.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input,
	/// and `resolution_in` is filled by the runner from the input
	/// texture's size, matching the C++ insert of the texture's virtual
	/// resolution (`// CPP-PARITY: tiledistortnode.cpp` value()).
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

		let scale_value = match inputs.get(SCALE_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(SCALE_INPUT, -1, time).to_double(),
		};

		// `!qFuzzyCompare(scale, 1.0)` (double overload) — job when the
		// scale is not approximately 1.0.
		if (scale_value - 1.0).abs() * 1e12 > scale_value.abs().min(1.0) {
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
	/// request id and always returns the tile fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): with a texture,
	/// places the position gizmo at `position_in` offset from the anchor
	/// point selected by `anchor_in` (rows add 0 / half / full height,
	/// columns add 0 / half / full width of the texture resolution).
	///
	/// The placement needs the texture's virtual resolution (the Rust
	/// texture handle carries no params) and the resulting point has no
	/// storage in [`Gizmo`] — not representable here
	/// (`// CPP-PARITY: tiledistortnode.cpp` `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): drags the position input's
	/// X and Y track draggers by the mouse delta added to their
	/// drag-start values.
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: tiledistortnode.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TileDistortNode {
			gizmo: self.gizmo.clone(),
		}))
	}
}

/// Constructor (C++ `TileDistortNode::TileDistortNode()`): adds
/// `tex_in`, `scale_in`, `position_in`, `anchor_in`, `mirrorx_in` and
/// `mirrory_in` with the defaults and properties documented on the
/// constants; creates the anchor-shaped position point gizmo bound to
/// both tracks of `position_in`; sets the video-effect flag and the
/// effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut scale = crate::input::Input::new(
		SCALE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.5),
	);
	scale.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	core.add_input(scale);

	core.add_input(crate::input::Input::new(
		POSITION_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));
	core.add_input(crate::input::Input::new(
		ANCHOR_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(Anchor::MiddleCenter as i64),
	));
	core.add_input(crate::input::Input::new(
		MIRROR_X_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		MIRROR_Y_INPUT,
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

	(core, Box::new(TileDistortNode { gizmo }))
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
		let n = TileDistortNode {
			gizmo: Gizmo {
				position_inputs: vec![],
				drag_point: (0.0, 0.0),
			},
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(SCALE_INPUT), "Scale");
		assert_eq!(n.input_name(POSITION_INPUT), "Position");
		assert_eq!(n.input_name(MIRROR_X_INPUT), "Mirror Horizontally");
		assert_eq!(n.input_name(MIRROR_Y_INPUT), "Mirror Vertically");
		assert_eq!(n.input_name(ANCHOR_INPUT), "Anchor");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.tile");
		assert_eq!(
			core.get_input(SCALE_INPUT).unwrap().default,
			NodeValue::Float(0.5)
		);
		assert_eq!(
			core.get_input(ANCHOR_INPUT).unwrap().default,
			NodeValue::Combo(4)
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
	fn value_unit_scale_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(SCALE_INPUT, -1, NodeValue::Float(1.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_non_unit_scale_pushes_job_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(SCALE_INPUT, -1, NodeValue::Float(0.5));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
			.expect("tile output boxes a ShaderJobPayload");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.tile");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn shader_code_returns_tile_shader() {
		let (_, behavior) = create();
		let code = behavior.shader_code("anything").unwrap();
		assert!(code.contains("uniform float scale_in;"));
		assert!(code.contains("vec2 modcoord = mod(coord, 1.0);"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Tile");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.tile`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.tile",
		name: "Tile",
		categories: &[Category::Distort],
		create,
	});
}
