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

//! Orthographic matrix generator (C++
//! `src/node/src/generator/matrix/matrix.{h,cpp}`,
//! `olive::MatrixGenerator`).

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Position input id (C++ `k_position_input`). Type: vec2; default
/// `(0.0, 0.0)`.
pub const POSITION_INPUT: &str = "pos_in";

/// Rotation input id (C++ `k_rotation_input`). Type: float; default
/// `0.0`.
pub const ROTATION_INPUT: &str = "rot_in";

/// Scale input id (C++ `k_scale_input`). Type: vec2; default
/// `(1.0, 1.0)`; properties: `min = (0, 0)`, `view = percentage`,
/// `disable1 = true` (the second component starts disabled because
/// uniform scale defaults to on).
pub const SCALE_INPUT: &str = "scale_in";

/// Uniform scale input id (C++ `k_uniform_scale_input`). Type: bool;
/// default `true`; flags: not-connectable, not-keyframable.
pub const UNIFORM_SCALE_INPUT: &str = "uniform_scale_in";

/// Anchor point input id (C++ `k_anchor_input`). Type: vec2; default
/// `(0.0, 0.0)`.
pub const ANCHOR_INPUT: &str = "anchor_in";

/// Orthographic matrix generator node. Builds a 2D transform matrix
/// from position, rotation, scale and anchor inputs. Has no own member
/// fields in C++ (state lives in the `Node` inputs).
pub struct MatrixGenerator;

impl NodeBehavior for MatrixGenerator {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Orthographic Matrix"
	}

	/// Short menu name (C++ `short_name()`).
	fn short_name(&self) -> &str {
		"Ortho"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ortho"
	}

	/// Categories (C++ `category()`): generator and math.
	fn categories(&self) -> &[Category] {
		&[Category::Generator, Category::Math]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Generate an orthographic matrix using position, rotation, and scale."
	}

	/// Localized input names (C++ `retranslate()`): `pos_in` ->
	/// "Position", `rot_in` -> "Rotation", `scale_in` -> "Scale",
	/// `uniform_scale_in` -> "Uniform Scale", `anchor_in` ->
	/// "Anchor Point".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			POSITION_INPUT => "Position",
			ROTATION_INPUT => "Rotation",
			SCALE_INPUT => "Scale",
			UNIFORM_SCALE_INPUT => "Uniform Scale",
			ANCHOR_INPUT => "Anchor Point",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): builds the matrix via
	/// `generate_matrix(value, false, false, false, Matrix4x4())` and
	/// pushes it as a `k_matrix` value. The C++ helper composes, in
	/// order: translate(position), rotate(rotation around Z),
	/// scale(scale.x, uniform ? scale.x : scale.y, 1), then
	/// translate(-anchor).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mat = MatrixGenerator::generate_matrix(
			inputs,
			core,
			time,
			false,
			false,
			false,
			super::mathbase::identity_matrix(),
		);
		table.push(
			crate::value::ValueType::Matrix,
			crate::value::NodeValue::Matrix(mat),
			None,
		);
	}

	/// Input value changed (C++ `InputValueChangedEvent`): when
	/// `uniform_scale_in` changes, sets the scale input's `disable1`
	/// property to the uniform-scale value (disabling the Y component
	/// while uniform scale is on).
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		if input == UNIFORM_SCALE_INPUT && element == -1 {
			let uniform = core.standard_value(UNIFORM_SCALE_INPUT, -1).to_double() != 0.0;
			if let Some(scale) = core.get_input_mut(SCALE_INPUT) {
				scale.properties.retain(|(k, _)| k != "disable1");
				scale.properties.push((
					"disable1".to_string(),
					crate::value::NodeValue::Boolean(uniform),
				));
			}
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MatrixGenerator))
	}
}

impl MatrixGenerator {
	/// C++ `generate_matrix(const NodeValueRow&, bool, bool, bool,
	/// const Matrix4x4&)` — reads the transform inputs from the render
	/// row (falling back to the keyframed/standard values via `core`),
	/// honoring the ignore flags, and composes the result onto `mat`
	/// (used by [`MatrixGenerator::value`] and `TransformDistortNode`).
	pub fn generate_matrix(
		row: &crate::value::NodeValueRow,
		core: &NodeCore,
		time: oak_core::Rational,
		ignore_anchor: bool,
		ignore_position: bool,
		ignore_scale: bool,
		mat: [f64; 16],
	) -> [f64; 16] {
		let anchor = if ignore_anchor {
			[0.0, 0.0]
		} else {
			row_vec2(row, core, time, ANCHOR_INPUT)
		};
		let scale = if ignore_scale {
			[1.0, 1.0]
		} else {
			row_vec2(row, core, time, SCALE_INPUT)
		};
		let position = if ignore_position {
			[0.0, 0.0]
		} else {
			row_vec2(row, core, time, POSITION_INPUT)
		};
		let rotation = row_float(row, core, time, ROTATION_INPUT);
		let uniform_scale = row_bool(row, core, time, UNIFORM_SCALE_INPUT);

		Self::compose_matrix(position, rotation, scale, uniform_scale, anchor, mat)
	}

	/// Static matrix composition (C++
	/// `generate_matrix(const Vector2D&, const float&, const Vector2D&,
	/// bool, const Vector2D&, Matrix4x4)`): `mat` post-multiplied by
	/// translate(position), rotate(rotation degrees around Z),
	/// scale(scale.x, uniform ? scale.x : scale.y, 1), then
	/// translate(-anchor). Row-major 16-element storage; every
	/// post-multiply matches the C++ `Matrix4x4` member operations
	/// (`// CPP-PARITY: matrix.cpp` `generate_matrix`).
	pub fn compose_matrix(
		pos: [f64; 2],
		rot: f64,
		scale: [f64; 2],
		uniform_scale: bool,
		anchor: [f64; 2],
		mat: [f64; 16],
	) -> [f64; 16] {
		// Position
		let mut m = matrix_translate(mat, pos[0], pos[1]);

		// Rotation (2D rotation around the Z axis).
		m = matrix_rotate_z(m, rot);

		// Scale (uniform scale replicates the X component).
		let full_scale = if uniform_scale {
			(scale[0], scale[0])
		} else {
			(scale[0], scale[1])
		};
		m = matrix_scale(m, full_scale.0, full_scale.1, 1.0);

		// Anchor point
		matrix_translate(m, -anchor[0], -anchor[1])
	}
}

/// Post-multiply `a` by `b` (C++ `Matrix4x4::operator*`; row-major
/// `m[r*4+c]` storage).
pub fn matrix_mul(a: [f64; 16], b: [f64; 16]) -> [f64; 16] {
	let mut out = [0.0f64; 16];
	for r in 0..4 {
		for c in 0..4 {
			let mut acc = 0.0;
			for k in 0..4 {
				acc += a[r * 4 + k] * b[k * 4 + c];
			}
			out[r * 4 + c] = acc;
		}
	}
	out
}

/// Post-multiply a 2D translation (C++ `Matrix4x4::translate(x, y)`).
pub fn matrix_translate(m: [f64; 16], x: f64, y: f64) -> [f64; 16] {
	let mut t = super::mathbase::identity_matrix();
	t[3] = x;
	t[7] = y;
	matrix_mul(m, t)
}

/// Post-multiply a scale (C++ `Matrix4x4::scale(x, y, z)`).
pub fn matrix_scale(m: [f64; 16], x: f64, y: f64, z: f64) -> [f64; 16] {
	let mut s = super::mathbase::identity_matrix();
	s[0] = x;
	s[5] = y;
	s[10] = z;
	matrix_mul(m, s)
}

/// Post-multiply a Z-axis rotation in degrees (C++
/// `Matrix4x4::rotate(degrees)`).
fn matrix_rotate_z(m: [f64; 16], degrees: f64) -> [f64; 16] {
	let radians = degrees * std::f64::consts::PI / 180.0;
	let (c, s) = (radians.cos(), radians.sin());
	let mut r = super::mathbase::identity_matrix();
	r[0] = c;
	r[1] = -s;
	r[4] = s;
	r[5] = c;
	matrix_mul(m, r)
}

/// Invert a 2D affine transform (C++ `Matrix4x4::inverted` on the 2D
/// path). The matrices this crate generates are affine —
/// `[a b 0 tx; c d 0 ty; 0 0 sz 0; 0 0 0 1]` in the row-major
/// `m[r*4+c]` layout — so the inverse is analytic. `None` when the
/// affine part is singular (a zero scale collapses the plane; the C++
/// callers fall back to identity there).
pub fn matrix_invert_2d(m: [f64; 16]) -> Option<[f64; 16]> {
	let (a, b, tx) = (m[0], m[1], m[3]);
	let (c, d, ty) = (m[4], m[5], m[7]);
	let det = a * d - b * c;
	if det.abs() < 1e-12 {
		return None;
	}
	let inv = 1.0 / det;
	let (ia, ib, ic, id) = (d * inv, -b * inv, -c * inv, a * inv);
	let mut out = super::mathbase::identity_matrix();
	out[0] = ia;
	out[1] = ib;
	out[3] = -(ia * tx + ib * ty);
	out[4] = ic;
	out[5] = id;
	out[7] = -(ic * tx + id * ty);
	let sz = m[10];
	if sz.abs() > 1e-12 {
		out[10] = 1.0 / sz;
	}
	Some(out)
}

/// Resolve a vec2 input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_vec2()`; missing values read as
/// `(0, 0)`).
fn row_vec2(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: oak_core::Rational,
	id: &str,
) -> [f64; 2] {
	match row.get(id) {
		Some(crate::value::NodeValue::Vec2(v)) => *v,
		Some(v) => [v.to_double(), 0.0],
		None => match core.value_at_time(id, -1, time) {
			crate::value::NodeValue::Vec2(v) => v,
			v => [v.to_double(), 0.0],
		},
	}
}

/// Resolve a float input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_double()`).
fn row_float(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: oak_core::Rational,
	id: &str,
) -> f64 {
	match row.get(id) {
		Some(v) => v.to_double(),
		None => core.value_at_time(id, -1, time).to_double(),
	}
}

/// Resolve a boolean input from the render row or the keyframed/standard
/// value (C++ `value.at(id).to_bool()`).
fn row_bool(
	row: &crate::value::NodeValueRow,
	core: &NodeCore,
	time: oak_core::Rational,
	id: &str,
) -> bool {
	match row.get(id) {
		Some(v) => v.to_double() != 0.0,
		None => core.value_at_time(id, -1, time).to_double() != 0.0,
	}
}

/// Constructor (C++ `MatrixGenerator::MatrixGenerator()`): adds
/// `pos_in`, `rot_in`, `scale_in`, `uniform_scale_in` and `anchor_in`
/// with the defaults, flags and properties documented on the constants.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut pos = crate::input::Input::new(
		POSITION_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	);
	pos.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("percentage".into()),
	)];
	core.add_input(pos);

	let mut rot = crate::input::Input::new(
		ROTATION_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	rot.properties = vec![
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		("min".to_string(), crate::value::NodeValue::Float(-360.0)),
		("max".to_string(), crate::value::NodeValue::Float(360.0)),
	];
	core.add_input(rot);

	let mut scale = crate::input::Input::new(
		SCALE_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([1.0, 1.0]),
	);
	scale.properties = vec![
		("min".to_string(), crate::value::NodeValue::Vec2([0.0, 0.0])),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		(
			"disable1".to_string(),
			crate::value::NodeValue::Boolean(true),
		),
	];
	core.add_input(scale);

	let mut uniform = crate::input::Input::new(
		UNIFORM_SCALE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	);
	uniform.flags |= crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(uniform);

	let mut anchor = crate::input::Input::new(
		ANCHOR_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	);
	anchor.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("percentage".into()),
	)];
	core.add_input(anchor);

	(core, Box::new(MatrixGenerator))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	#[test]
	fn input_names() {
		let n = MatrixGenerator;
		assert_eq!(n.input_name(POSITION_INPUT), "Position");
		assert_eq!(n.input_name(ROTATION_INPUT), "Rotation");
		assert_eq!(n.input_name(SCALE_INPUT), "Scale");
		assert_eq!(n.input_name(UNIFORM_SCALE_INPUT), "Uniform Scale");
		assert_eq!(n.input_name(ANCHOR_INPUT), "Anchor Point");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ortho");
		assert_eq!(
			core.get_input(POSITION_INPUT).unwrap().default,
			NodeValue::Vec2([0.0, 0.0])
		);
		assert_eq!(
			core.get_input(SCALE_INPUT).unwrap().default,
			NodeValue::Vec2([1.0, 1.0])
		);
		assert_eq!(
			core.get_input(UNIFORM_SCALE_INPUT).unwrap().default,
			NodeValue::Boolean(true)
		);
	}

	#[test]
	fn value_defaults_produce_identity() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert_eq!(
			table.get(ValueType::Matrix),
			Some(&NodeValue::Matrix(super::super::mathbase::identity_matrix()))
		);
	}

	#[test]
	fn value_translate_position() {
		let (mut core, behavior) = create();
		core.set_standard_value(POSITION_INPUT, -1, NodeValue::Vec2([10.0, 20.0]));
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		// translate(10, 20) is stored at m[0][3] / m[1][3].
		let m = match table.get(ValueType::Matrix).unwrap() {
			NodeValue::Matrix(m) => *m,
			_ => panic!("matrix expected"),
		};
		assert_eq!(m[3], 10.0);
		assert_eq!(m[7], 20.0);
	}

	#[test]
	fn value_non_uniform_scale() {
		let (mut core, behavior) = create();
		core.set_standard_value(UNIFORM_SCALE_INPUT, -1, NodeValue::Boolean(false));
		core.set_standard_value(SCALE_INPUT, -1, NodeValue::Vec2([2.0, 3.0]));
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		let m = match table.get(ValueType::Matrix).unwrap() {
			NodeValue::Matrix(m) => *m,
			_ => panic!("matrix expected"),
		};
		assert_eq!(m[0], 2.0);
		assert_eq!(m[5], 3.0);
	}

	#[test]
	fn value_uniform_scale_replicates_x() {
		let (mut core, behavior) = create();
		// uniform_scale defaults to true.
		core.set_standard_value(SCALE_INPUT, -1, NodeValue::Vec2([2.0, 9.0]));
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		let m = match table.get(ValueType::Matrix).unwrap() {
			NodeValue::Matrix(m) => *m,
			_ => panic!("matrix expected"),
		};
		assert_eq!(m[0], 2.0);
		assert_eq!(m[5], 2.0, "uniform scale replicates X");
	}

	#[test]
	fn compose_matrix_rotation_translate_order() {
		// rotate(90) post-multiplied by translate(-10, 0): a pure
		// translation (10, 0) under a 90-degree rotation lands in the
		// negative Y translation slot.
		let m = MatrixGenerator::compose_matrix(
			[0.0, 0.0],
			90.0,
			[1.0, 1.0],
			false,
			[-10.0, 0.0],
			super::super::mathbase::identity_matrix(),
		);
		assert!((m[0] - 0.0).abs() < 1e-9);
		assert!((m[5] - 0.0).abs() < 1e-9);
		assert!((m[7] - 10.0).abs() < 1e-9, "m[1][3] = +10 after rotate(90)");
		assert!((m[3] - 0.0).abs() < 1e-9);
	}

	#[test]
	fn compose_matrix_identity_inputs() {
		let m = MatrixGenerator::compose_matrix(
			[0.0, 0.0],
			0.0,
			[1.0, 1.0],
			false,
			[0.0, 0.0],
			super::super::mathbase::identity_matrix(),
		);
		assert!(super::super::mathbase::matrix_is_identity(m));
	}

	#[test]
	fn matrix_helpers_are_row_major() {
		let mut a = super::super::mathbase::identity_matrix();
		a[3] = 100.0; // translate x
		let mut b = super::super::mathbase::identity_matrix();
		b[5] = 2.0; // scale y
		let prod = matrix_mul(a, b);
		assert_eq!(prod[5], 2.0);
		assert_eq!(prod[3], 100.0, "post-multiplied translate survives");
	}

	#[test]
	fn input_value_changed_toggles_disable1() {
		let (mut core, behavior) = create();
		let mut b = behavior;
		b.input_value_changed(&mut core, UNIFORM_SCALE_INPUT, -1);
		let scale = core.get_input(SCALE_INPUT).unwrap();
		assert!(scale
			.properties
			.iter()
			.any(|(k, v)| { k == "disable1" && *v == NodeValue::Boolean(true) }));

		core.set_standard_value(UNIFORM_SCALE_INPUT, -1, NodeValue::Boolean(false));
		b.input_value_changed(&mut core, UNIFORM_SCALE_INPUT, -1);
		let scale = core.get_input(SCALE_INPUT).unwrap();
		assert!(scale
			.properties
			.iter()
			.any(|(k, v)| { k == "disable1" && *v == NodeValue::Boolean(false) }));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Orthographic Matrix");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.ortho`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ortho",
		name: "Orthographic Matrix",
		categories: &[Category::Generator, Category::Math],
		create,
	});
}
