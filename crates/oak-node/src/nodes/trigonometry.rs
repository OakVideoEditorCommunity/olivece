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

//! Trigonometry node (C++
//! `src/node/src/math/trigonometry/trigonometry.{h,cpp}`,
//! `olive::TrigonometryNode`).

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Operation/method input id (C++ `k_method_in`). Type: combo;
/// flags: not-connectable, not-keyframable; combo strings: Sine,
/// Cosine, Tangent, Inverse Sine, Inverse Cosine, Inverse Tangent,
/// Hyperbolic Sine, Hyperbolic Cosine, Hyperbolic Tangent.
pub const METHOD_INPUT: &str = "method_in";

/// Operand input id (C++ `k_x_in`). Type: float; default `0.0`.
pub const X_INPUT: &str = "x_in";

/// Trigonometry operation (C++ private `TrigonometryNode::Operation`;
/// discriminants are the `method_in` combo indices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
	/// `sin(x)` (C++ `k_op_sine`).
	Sine,
	/// `cos(x)` (C++ `k_op_cosine`).
	Cosine,
	/// `tan(x)` (C++ `k_op_tangent`).
	Tangent,
	/// `asin(x)` (C++ `k_op_arc_sine`).
	ArcSine,
	/// `acos(x)` (C++ `k_op_arc_cosine`).
	ArcCosine,
	/// `atan(x)` (C++ `k_op_arc_tangent`).
	ArcTangent,
	/// `sinh(x)` (C++ `k_op_hyp_sine`).
	HyperbolicSine,
	/// `cosh(x)` (C++ `k_op_hyp_cosine`).
	HyperbolicCosine,
	/// `tanh(x)` (C++ `k_op_hyp_tangent`).
	HyperbolicTangent,
}

/// Trigonometry node. Unit-like — the C++ class has no own data
/// members (its `Operation` enum is modeled above; inputs live in
/// [`NodeCore`]).
pub struct TrigonometryNode;

impl NodeBehavior for TrigonometryNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Trigonometry"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.trigonometry"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Perform a trigonometry operation on a value."
	}

	/// Localized input names (C++ `retranslate()`): `method_in` ->
	/// "Method", `x_in` -> "Value"; also sets the combo strings on
	/// `method_in` to the nine function names documented on
	/// [`METHOD_INPUT`].
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			METHOD_INPUT => "Method",
			X_INPUT => "Value",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `method_in` -> "Sine", "Cosine",
	/// "Tangent", "Inverse Sine", "Inverse Cosine", "Inverse Tangent",
	/// "Hyperbolic Sine", "Hyperbolic Cosine", "Hyperbolic Tangent".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			METHOD_INPUT => vec![
				"Sine",
				"Cosine",
				"Tangent",
				"Inverse Sine",
				"Inverse Cosine",
				"Inverse Tangent",
				"Hyperbolic Sine",
				"Hyperbolic Cosine",
				"Hyperbolic Tangent",
			],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): reads `x_in` as a double,
	/// applies the [`Operation`] selected by `method_in`
	/// (sin/cos/tan/asin/acos/atan/sinh/cosh/tanh), and pushes the
	/// result as a float.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mut x = match inputs.get(X_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(X_INPUT, -1, time).to_double(),
		};

		match self.operation(core) {
			Operation::Sine => x = x.sin(),
			Operation::Cosine => x = x.cos(),
			Operation::Tangent => x = x.tan(),
			Operation::ArcSine => x = x.asin(),
			Operation::ArcCosine => x = x.acos(),
			Operation::ArcTangent => x = x.atan(),
			Operation::HyperbolicSine => x = x.sinh(),
			Operation::HyperbolicCosine => x = x.cosh(),
			Operation::HyperbolicTangent => x = x.tanh(),
		}

		table.push(
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(x),
			None,
		);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TrigonometryNode))
	}
}

impl TrigonometryNode {
	/// The operation selected by the `method_in` combo (C++
	/// `TrigonometryNode::get_operation()`).
	pub fn operation(&self, core: &NodeCore) -> Operation {
		match core.standard_value(METHOD_INPUT, -1).to_double() as usize {
			0 => Operation::Sine,
			1 => Operation::Cosine,
			2 => Operation::Tangent,
			3 => Operation::ArcSine,
			4 => Operation::ArcCosine,
			5 => Operation::ArcTangent,
			6 => Operation::HyperbolicSine,
			7 => Operation::HyperbolicCosine,
			_ => Operation::HyperbolicTangent,
		}
	}
}

/// Constructor (C++ `TrigonometryNode::TrigonometryNode()`): adds
/// `method_in` as a not-connectable/not-keyframable combo and `x_in`
/// as a float input defaulting to 0.0.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut method = crate::input::Input::new(
		METHOD_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	);
	method.flags |= crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;
	method.properties = vec![(
		"combobox_strings".to_string(),
		crate::value::NodeValue::Binary(OPERATION_NAMES.concat().into_bytes()),
	)];
	core.add_input(method);

	let x = crate::input::Input::new(
		X_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	core.add_input(x);

	(core, Box::new(TrigonometryNode))
}

/// Operation combo strings (C++ `retranslate` order).
pub const OPERATION_NAMES: [&str; 9] = [
	"Sine",
	"Cosine",
	"Tangent",
	"Inverse Sine",
	"Inverse Cosine",
	"Inverse Tangent",
	"Hyperbolic Sine",
	"Hyperbolic Cosine",
	"Hyperbolic Tangent",
];

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	#[test]
	fn input_names() {
		let n = TrigonometryNode;
		assert_eq!(n.input_name(METHOD_INPUT), "Method");
		assert_eq!(n.input_name(X_INPUT), "Value");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.trigonometry"
		);
		assert_eq!(
			core.get_input(X_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		let method = core.get_input(METHOD_INPUT).unwrap();
		assert_ne!(method.flags & crate::input::flags::NOT_CONNECTABLE, 0);
	}

	#[test]
	fn value_sine() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(
			X_INPUT.to_string(),
			NodeValue::Float(std::f64::consts::FRAC_PI_2),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(1.0)));
	}

	#[test]
	fn value_cosine_and_tangent() {
		let (mut core, behavior) = create();
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(1));
		let inputs =
			crate::value::NodeValueRow::from([(X_INPUT.to_string(), NodeValue::Float(0.0))]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(1.0)));

		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2));
		let inputs = crate::value::NodeValueRow::from([(
			X_INPUT.to_string(),
			NodeValue::Float(std::f64::consts::FRAC_PI_4),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let v = table.get(ValueType::Float).unwrap().to_double();
		assert!((v - 1.0).abs() < 1e-9);
	}

	#[test]
	fn value_uses_standard_operand() {
		let (mut core, behavior) = create();
		core.set_standard_value(X_INPUT, -1, NodeValue::Float(0.0));
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(0.0)));
	}

	#[test]
	fn operation_mapping() {
		let (mut core, _) = create();
		let n = TrigonometryNode;
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(3));
		assert_eq!(n.operation(&core), Operation::ArcSine);
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(8));
		assert_eq!(n.operation(&core), Operation::HyperbolicTangent);
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(99));
		assert_eq!(n.operation(&core), Operation::HyperbolicTangent, "clamped");
	}

	#[test]
	fn value_all_operations() {
		use std::f64::consts::FRAC_PI_4;
		let (mut core, behavior) = create();
		// (combo index, input, expected fn applied)
		let cases: [(i64, f64, fn(f64) -> f64); 9] = [
			(0, FRAC_PI_4, f64::sin),
			(1, FRAC_PI_4, f64::cos),
			(2, FRAC_PI_4, f64::tan),
			(3, 0.5, f64::asin),
			(4, 0.5, f64::acos),
			(5, 0.5, f64::atan),
			(6, 0.5, f64::sinh),
			(7, 0.5, f64::cosh),
			(8, 0.5, f64::tanh),
		];
		for (op, x, f) in cases {
			core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(op));
			let inputs =
				crate::value::NodeValueRow::from([(X_INPUT.to_string(), NodeValue::Float(x))]);
			let mut table = NodeValueTable::default();
			behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
			let got = table.get(ValueType::Float).unwrap().to_double();
			assert!(
				(got - f(x)).abs() < 1e-12,
				"op {}: got {}, want {}",
				op,
				got,
				f(x)
			);
		}
	}

	#[test]
	fn value_keyframed_operand() {
		let (mut core, behavior) = create();
		core.keyframe_track_mut(X_INPUT, -1)
			.set_key(crate::keyframe::Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(1.0),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		let v = table.get(ValueType::Float).unwrap().to_double();
		assert!((v - 1.0_f64.sin()).abs() < 1e-12);
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Trigonometry");
	}
}

/// Register this node type (C++ `k_trigonometry_node` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.trigonometry",
		name: "Trigonometry",
		categories: &[Category::Math],
		create,
	});
}
