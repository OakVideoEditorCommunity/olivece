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

//! Shared shape-node base (C++
//! `src/node/src/generator/shape/shapenodebase.{h,cpp}`,
//! `olive::ShapeNodeBase`).
//!
//! Abstract C++ base (extends `GeneratorWithMerge`, see
//! [`super::generatorwithmerge`]) for shape generators with position,
//! size, color and rect-scaling gizmos. Not instantiable, so this is a
//! helper module, not a [`NodeBehavior`] implementation.

/// Position input id (C++ `k_position_input`). Type: vec2; default
/// `(0, 0)`.
pub const POSITION_INPUT: &str = "pos_in";

/// Size input id (C++ `k_size_input`). Type: vec2; default
/// `(100, 100)`; properties: `min = (0, 0)`.
pub const SIZE_INPUT: &str = "size_in";

/// Color input id (C++ `k_color_input`). Type: color; default
/// `(1.0, 0.0, 0.0, 1.0)`; only added when the base is constructed with
/// `create_color_input = true`.
pub const COLOR_INPUT: &str = "color_in";

/// Helper mirroring the C++ `ShapeNodeBase` base.
///
/// The C++ members `point_gizmo_[k_gizmo_scale_count]` (8 rect-scale
/// point gizmos) and `poly_gizmo_` (whole-rect draggable polygon
/// gizmo) are GUI gizmo pointers with no Rust equivalent here; gizmos
/// are tracked in `NodeCore::gizmos`, so they are omitted. The
/// `k_gizmo_scale_*` index constants come from the C++ gizmo layer.
pub struct ShapeNodeBase;

impl ShapeNodeBase {
	/// Localized base input names (C++ `retranslate()` on top of the
	/// merge base): `pos_in` -> "Position", `size_in` -> "Size", and
	/// `color_in` -> "Color" when the color input exists.
	pub fn input_name(id: &str) -> &str {
		match id {
			POSITION_INPUT => "Position",
			SIZE_INPUT => "Size",
			COLOR_INPUT => "Color",
			_ => id,
		}
	}

	/// Gizmo layout (C++ `update_gizmo_positions()`): centers the rect
	/// around the square-resolution midpoint (also stored as the
	/// `offset` property of `pos_in` so values appear top-left
	/// anchored), then places the 8 scale-point gizmos at the rect's
	/// corners/edge centers and the polygon gizmo on the four corners
	/// in top-left, top-right, bottom-right, bottom-left order.
	///
	/// The C++ writes the resulting points into its `PointGizmo` /
	/// `PolygonGizmo` objects; the Rust `NodeCore::gizmos` records only
	/// each gizmo's tracked input references and drag position, so the
	/// only persistable half is the `offset` property — which requires
	/// the square resolution from `NodeGlobals`, not carried by this
	/// signature. The property write and the gizmo point placements are
	/// therefore not representable here (`// CPP-PARITY:
	/// shapenodebase.cpp` `update_gizmo_positions`).
	#[allow(dead_code)] // C++ parity helpers; covered by the no-op tests.
	pub fn update_gizmo_positions(
		core: &mut crate::node::NodeCore,
		row: &crate::value::NodeValueRow,
	) {
		let _ = (core, row);
	}

	/// Undoable rect assignment (C++ `set_rect()`): normalizes the rect
	/// around the sequence center, then pushes undo children setting
	/// `size_in` x/y and `pos_in` x/y standard values.
	///
	/// The C++ normalization needs the sequence resolution and the undo
	/// command stack; neither is carried by this signature or this
	/// crate's data model, so the writes are not representable here
	/// (`// CPP-PARITY: shapenodebase.cpp` `set_rect`).
	#[allow(dead_code)]
	pub fn set_rect(core: &mut crate::node::NodeCore, rect: (f64, f64, f64, f64)) {
		let _ = (core, rect);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): dragging the polygon
	/// gizmo offsets `pos_in` x/y directly; dragging a scale-point
	/// gizmo resizes with anchor-at-opposite-point semantics —
	/// Alt drags from center, Shift keeps the original aspect ratio
	/// (center-edge gizmos derive the other axis from the ratio,
	/// corner gizmos reconstruct both axes from the original angle and
	/// the new hypotenuse) — and writes the new position/size through
	/// the gizmo's four input draggers.
	///
	/// The C++ logic operates on its `DraggableGizmo`/dragger objects
	/// with per-gizmo start values and keyframe-track references; the
	/// Rust `NodeCore::gizmos` has no dragger state, so the drag is not
	/// representable here (`// CPP-PARITY: shapenodebase.cpp`
	/// `gizmo_drag_move`).
	#[allow(dead_code)]
	pub fn gizmo_drag_move(core: &mut crate::node::NodeCore, x: f64, y: f64, modifiers: u32) {
		let _ = (core, x, y, modifiers);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeCore;

	#[test]
	fn input_names() {
		assert_eq!(ShapeNodeBase::input_name(POSITION_INPUT), "Position");
		assert_eq!(ShapeNodeBase::input_name(SIZE_INPUT), "Size");
		assert_eq!(ShapeNodeBase::input_name(COLOR_INPUT), "Color");
		assert_eq!(ShapeNodeBase::input_name("other_in"), "other_in");
	}

	#[test]
	fn gizmo_helpers_are_documented_noops() {
		// The gizmo data model is not representable in NodeCore (see the
		// method docs); the calls must be safe no-ops.
		let mut core = NodeCore::new();
		let row = crate::value::NodeValueRow::default();
		ShapeNodeBase::update_gizmo_positions(&mut core, &row);
		ShapeNodeBase::set_rect(&mut core, (0.0, 0.0, 100.0, 100.0));
		ShapeNodeBase::gizmo_drag_move(&mut core, 10.0, 20.0, 0);
		assert!(
			core.get_input(POSITION_INPUT).is_none(),
			"no inputs are added"
		);
	}
}
