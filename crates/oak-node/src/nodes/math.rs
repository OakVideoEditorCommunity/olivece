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

//! Math node (C++ `src/node/src/math/math/math.{h,cpp}`,
//! `olive::MathNode`); behavior shared with other binary math nodes
//! lives in [`super::mathbase`] (C++ `MathNodeBase`).

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Operation/method input id (C++ `k_method_in`). Type: combo;
/// flags: not-connectable, not-keyframable; combo strings are the
/// operation names Add/Subtract/Multiply/Divide/Power.
pub const METHOD_INPUT: &str = "method_in";

/// Operand A input id (C++ `k_param_a_in`). Type: float; default
/// `0.0`; properties: `decimalplaces = 8`, `autotrim = true`.
pub const PARAM_A_INPUT: &str = "param_a_in";

/// Operand B input id (C++ `k_param_b_in`). Type: float; default
/// `0.0`; properties: `decimalplaces = 8`, `autotrim = true`.
pub const PARAM_B_INPUT: &str = "param_b_in";

/// Operand C input id (C++ `k_param_c_in`). Declared as a static but
/// never added in the constructor — reserved/unused upstream.
pub const PARAM_C_INPUT: &str = "param_c_in";

/// Math node: applies a binary arithmetic operation to two values.
/// Unit-like — the C++ class has no own data members (inputs live in
/// [`NodeCore`], shared logic in [`super::mathbase`]).
pub struct MathNode;

impl MathNode {
	/// Construct the behavior struct (C++ `MathNode()`; the node's
	/// inputs are wired in [`create`]). Exposed so other nodes can own
	/// a child math node (e.g. `super::opacity` multiplies via one).
	pub fn new() -> Box<MathNode> {
		Box::new(MathNode)
	}

	/// The operation selected by the `method_in` combo (C++
	/// `MathNode::get_operation()`).
	pub fn operation(&self, core: &NodeCore) -> super::mathbase::Operation {
		let idx = core.standard_value(METHOD_INPUT, -1).to_double() as usize;
		match idx {
			0 => super::mathbase::Operation::Add,
			1 => super::mathbase::Operation::Subtract,
			2 => super::mathbase::Operation::Multiply,
			3 => super::mathbase::Operation::Divide,
			_ => super::mathbase::Operation::Power,
		}
	}
}

impl NodeBehavior for MathNode {
	/// Human-readable name (C++ `name()`): if the node is parented
	/// (i.e. owned as a child of another node) and the current
	/// operation has a name, returns that operation name
	/// (Add/Subtract/Multiply/Divide/Power); otherwise "Math".
	fn name(&self) -> &str {
		"Math"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.math"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Perform a mathematical operation between two values."
	}

	/// Localized input names (C++ `retranslate()`): `method_in` ->
	/// "Method", `param_a_in`/`param_b_in` -> "Value"; also sets the
	/// combo strings on `method_in` to the five operation names.
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			METHOD_INPUT => "Method",
			PARAM_A_INPUT | PARAM_B_INPUT => "Value",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `method_in` -> the five operation names
	/// "Add", "Subtract", "Multiply", "Divide", "Power".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			METHOD_INPUT => vec!["Add", "Subtract", "Multiply", "Divide", "Power"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): pushes both operands into
	/// single-value tables, runs the [`super::mathbase::PairingCalculator`]
	/// heuristic, and if a pairing was found delegates to
	/// [`super::mathbase::MathNodeBase::value_internal`] with the
	/// current operation; otherwise pushes nothing.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let a = inputs
			.get(PARAM_A_INPUT)
			.cloned()
			.unwrap_or_else(|| core.value_at_time(PARAM_A_INPUT, -1, time));
		let b = inputs
			.get(PARAM_B_INPUT)
			.cloned()
			.unwrap_or_else(|| core.value_at_time(PARAM_B_INPUT, -1, time));

		let mut at = crate::value::NodeValueTable::default();
		at.push(a.value_type(), a, None);
		let mut bt = crate::value::NodeValueTable::default();
		bt.push(b.value_type(), b, None);

		let calc = super::mathbase::PairingCalculator::new(&at, &bt);
		if calc.found_most_likely_pairing() {
			super::mathbase::MathNodeBase::value_internal(
				self.operation(core),
				calc.most_likely_pairing,
				PARAM_A_INPUT,
				&calc.most_likely_value_a,
				PARAM_B_INPUT,
				&calc.most_likely_value_b,
				time,
				self.type_id(),
				core,
				inputs,
				table,
			);
		}
	}

	/// Process a span of samples (C++ `process_samples()`): delegates to
	/// [`super::mathbase::MathNodeBase::process_samples_internal`]
	/// with the current operation and the `param_a_in`/`param_b_in`
	/// ids (only used for the sample*number pairing). The C++ signature
	/// receives the input buffer and a sample index; the Rust trait
	/// instead hands over a time `range` and the destination buffer, so
	/// the samples operand is located in the row and every index of the
	/// output span is filled.
	fn process_samples(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		range: oak_core::TimeRange,
		output: &mut crate::value::SampleBuffer,
	) {
		let _ = range;
		let input = match inputs.get(PARAM_A_INPUT) {
			Some(crate::value::NodeValue::Samples(b)) => b.clone(),
			_ => match inputs.get(PARAM_B_INPUT) {
				Some(crate::value::NodeValue::Samples(b)) => b.clone(),
				_ => return,
			},
		};
		for index in 0..output.sample_count {
			super::mathbase::MathNodeBase::process_samples_internal(
				inputs,
				self.operation(core),
				PARAM_A_INPUT,
				PARAM_B_INPUT,
				&input,
				output,
				index,
			);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): delegates to
	/// [`super::mathbase::MathNodeBase::shader_code_internal`] with the
	/// request id (expected `"<op>.<pairing>.<type_a>.<type_b>"`) and
	/// the `param_a_in`/`param_b_in` uniform names. The C++ `ShaderCode`
	/// carries a vertex shader for the texture*matrix case; the trait's
	/// single-string return carries only the fragment shader
	/// (`// CPP-PARITY: math.cpp` `get_shader_code`).
	fn shader_code(&self, request: &str) -> Option<String> {
		let (frag, _vert) = super::mathbase::MathNodeBase::shader_code_internal(
			request,
			PARAM_A_INPUT,
			PARAM_B_INPUT,
		);
		if frag.is_empty() {
			None
		} else {
			Some(frag)
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MathNode))
	}
}

/// Operation combo strings (C++ `MathNodeBase::retranslate` order).
pub const OPERATION_NAMES: [&str; 5] = ["Add", "Subtract", "Multiply", "Divide", "Power"];

/// Constructor (C++ `MathNode::MathNode()`): adds `method_in` as a
/// not-connectable/not-keyframable combo, and `param_a_in`/
/// `param_b_in` as float inputs defaulting to 0.0 with
/// `decimalplaces = 8` and `autotrim = true`.
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

	let mut a = crate::input::Input::new(
		PARAM_A_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	a.properties = vec![
		("decimalplaces".to_string(), crate::value::NodeValue::Int(8)),
		(
			"autotrim".to_string(),
			crate::value::NodeValue::Boolean(true),
		),
	];
	core.add_input(a);

	let mut b = crate::input::Input::new(
		PARAM_B_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	b.properties = vec![
		("decimalplaces".to_string(), crate::value::NodeValue::Int(8)),
		(
			"autotrim".to_string(),
			crate::value::NodeValue::Boolean(true),
		),
	];
	core.add_input(b);

	(core, MathNode::new())
}

/// Register this node type (C++ `k_math_node` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.math",
		name: "Math",
		categories: &[Category::Math],
		create,
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn row(values: &[(&str, NodeValue)]) -> crate::value::NodeValueRow {
		values
			.iter()
			.map(|(k, v)| (k.to_string(), v.clone()))
			.collect()
	}

	#[test]
	fn value_adds_numbers() {
		let (core, behavior) = create();
		let inputs = row(&[
			(PARAM_A_INPUT, NodeValue::Float(2.0)),
			(PARAM_B_INPUT, NodeValue::Float(3.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(5.0)));
	}

	#[test]
	fn value_uses_standard_operands_when_unconnected() {
		let (mut core, behavior) = create();
		core.set_standard_value(PARAM_A_INPUT, -1, NodeValue::Float(10.0));
		core.set_standard_value(PARAM_B_INPUT, -1, NodeValue::Float(4.0));
		let inputs = crate::value::NodeValueRow::default();
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(14.0)));
	}

	#[test]
	fn value_multiply_color_by_number() {
		let (core, behavior) = create();
		let mut core = core;
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2)); // Multiply
		let inputs = row(&[
			(PARAM_A_INPUT, NodeValue::Color([1.0, 1.0, 1.0, 1.0])),
			(PARAM_B_INPUT, NodeValue::Float(0.5)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(
			table.get(ValueType::Color),
			Some(&NodeValue::Color([0.5, 0.5, 0.5, 0.5]))
		);
	}

	#[test]
	fn value_empty_operands_push_nothing() {
		let (core, behavior) = create();
		let inputs = row(&[
			(PARAM_A_INPUT, NodeValue::None),
			(PARAM_B_INPUT, NodeValue::None),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn operation_from_combo() {
		let (mut core, _) = create();
		let n = MathNode;
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(0));
		assert_eq!(n.operation(&core), super::super::mathbase::Operation::Add);
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(4));
		assert_eq!(n.operation(&core), super::super::mathbase::Operation::Power);
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(99));
		assert_eq!(
			n.operation(&core),
			super::super::mathbase::Operation::Power,
			"clamped"
		);
	}

	#[test]
	fn shader_code_delegates_to_mathbase() {
		let n = MathNode;
		let code = n.shader_code("0.0.2.2").unwrap();
		assert!(code.contains("param_a_in + param_b_in"));
		assert_eq!(n.shader_code("garbage"), None);
	}

	#[test]
	fn process_samples_multiplies_number() {
		let (mut core, behavior) = create();
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2)); // Multiply
		let mut buf = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32Planar,
			channels: 1,
			sample_count: 2,
			data: vec![0u8; 8],
		};
		buf.set_sample_value(0, 0, 1.0);
		buf.set_sample_value(0, 1, 2.0);
		let inputs = row(&[
			(PARAM_A_INPUT, NodeValue::Samples(buf.clone())),
			(PARAM_B_INPUT, NodeValue::Float(3.0)),
		]);
		let mut out = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32Planar,
			channels: 1,
			sample_count: 2,
			data: vec![0u8; 8],
		};
		behavior.process_samples(
			&core,
			&inputs,
			oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)),
			&mut out,
		);
		assert_eq!(out.sample_value(0, 0), 3.0);
		assert_eq!(out.sample_value(0, 1), 6.0);
	}

	#[test]
	fn value_texture_multiplied_pushes_job_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2)); // Multiply
		let tex = NodeValue::Texture(crate::handle::make_owned::<u8>(7));
		let inputs = row(&[
			(PARAM_A_INPUT, tex.clone()),
			(PARAM_B_INPUT, NodeValue::Float(2.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(7, 1), &mut table);
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe {
			crate::jobs::shader_job(&handle)
		}
		.expect("shader job payload boxed in the pushed texture");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.math");
		// op=2 (multiply), pairing=8 (texture_number), a=10 (texture),
		// b=2 (float).
		assert_eq!(payload.shader_id, "2.8.10.2");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, "");
		assert_eq!(payload.time, Rational::new(7, 1));
		assert_eq!(payload.params.get(PARAM_A_INPUT), Some(&tex));
		assert_eq!(
			payload.params.get(PARAM_B_INPUT),
			Some(&NodeValue::Float(2.0))
		);
	}

	#[test]
	fn value_null_texture_pushes_no_payload() {
		let (mut core, behavior) = create();
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2)); // Multiply
		let inputs = row(&[
			(
				PARAM_A_INPUT,
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
			(PARAM_B_INPUT, NodeValue::Float(2.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		// Null texture operand -> no-op push-through, not a job payload.
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe {
			crate::jobs::shader_job(&handle)
		};
		assert!(payload.is_none(), "no shader job for a null texture");
	}
}
