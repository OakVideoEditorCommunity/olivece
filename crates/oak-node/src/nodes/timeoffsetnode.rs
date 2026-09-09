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

//! Time offset node (C++ `src/node/src/time/timeoffset/timeoffsetnode.{h,cpp}`,
//! `olive::TimeOffsetNode`).

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::{Rational, TimeRange};

/// Time offset input id (C++ `k_time_input`). Type: rational; default
/// `Rational(0)`; flags: not-connectable; properties: `view = time slider`,
/// `viewlock = true`.
pub const TIME_INPUT: &str = "time_in";

/// Effect input id (C++ `k_input_input`). Type: none (pass-through of any
/// connected type); flags: not-keyframable.
pub const INPUT_INPUT: &str = "input_in";

/// Time offset node. Shifts the time seen by the connected input by a
/// constant amount. The C++ class has no own data members (only the private
/// `get_remapped_time()`/`get_remapped_output_time()` helpers, which become
/// the behavior of `input_time_adjustment`/`output_time_adjustment` below),
/// so this is a unit-like struct.
pub struct TimeOffsetNode;

impl TimeOffsetNode {
	/// C++ `get_remapped_time()`: the `time_in` input's value at
	/// `input_time` added to `input_time` — `input + time_in`. `time_in`
	/// is not-connectable, so no connected edge is ever consulted.
	pub fn get_remapped_time(core: &NodeCore, input_time: Rational) -> Rational {
		input_time + Self::time_offset(core, input_time)
	}

	/// C++ `get_remapped_output_time()`: the inverse of
	/// [`Self::get_remapped_time`] — `input - time_in`.
	pub fn get_remapped_output_time(core: &NodeCore, input_time: Rational) -> Rational {
		input_time - Self::time_offset(core, input_time)
	}

	/// The `time_in` value evaluated at `input_time` (C++ `get_value_at_time
	/// (k_time_input, input).value<Rational>()`).
	fn time_offset(core: &NodeCore, input_time: Rational) -> Rational {
		match core.value_at_time(TIME_INPUT, -1, input_time) {
			NodeValue::Rational(r) => r,
			v => Rational::from_double(v.to_double()),
		}
	}

	/// The full C++ `input_time_adjustment()` with an explicit core. For
	/// `input_in`, both endpoints of the range are shifted forward by the
	/// `time_in` value evaluated at that endpoint; all other inputs fall
	/// through to the base-class identity behavior.
	///
	/// The [`NodeBehavior::input_time_adjustment`] trait method receives the
	/// core and delegates here; kept as an associated fn so the tests can
	/// drive the mapping without a behavior instance.
	pub fn input_time_adjustment_with(
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		let _ = (element, traverse);
		if input == INPUT_INPUT {
			TimeRange::new(
				Self::get_remapped_time(core, time.in_()),
				Self::get_remapped_time(core, time.out()),
			)
		} else {
			time
		}
	}

	/// The full C++ `output_time_adjustment()` with an explicit core: the
	/// exact inverse of [`Self::input_time_adjustment_with`] — for
	/// `input_in`, both endpoints are shifted back by the `time_in` value.
	pub fn output_time_adjustment_with(
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		let _ = (element, traverse);
		if input == INPUT_INPUT {
			TimeRange::new(
				Self::get_remapped_output_time(core, time.in_()),
				Self::get_remapped_output_time(core, time.out()),
			)
		} else {
			time
		}
	}
}

impl NodeBehavior for TimeOffsetNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Time Offset"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.timeoffset"
	}

	/// Categories (C++ `category()` returns `{ k_category_time }`; the Rust
	/// `Category` enum has no `Time` variant yet, so this is empty until one
	/// is added).
	fn categories(&self) -> &[Category] {
		&[]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Offset time passing through the graph."
	}

	/// Localized input names (C++ `retranslate()`): `time_in` -> "Time",
	/// `input_in` -> "Input".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TIME_INPUT => "Time",
			INPUT_INPUT => "Input",
			_ => id,
		}
	}

	/// Input-side time remap (C++ `input_time_adjustment()`): for
	/// `input_in`, both ends of the range are shifted forward by the current
	/// `time_in` value (C++ `get_remapped_time()`: `input + time_in`);
	/// all other inputs fall through to the base-class identity behavior.
	/// `core` carries the node's data (the keyframable `time_in` input),
	/// matching the C++ member read
	/// (`// CPP-PARITY: timeoffsetnode.cpp` `input_time_adjustment`).
	fn input_time_adjustment(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		Self::input_time_adjustment_with(core, input, element, time, traverse)
	}

	/// Output-side time remap (C++ `output_time_adjustment()`): the exact
	/// inverse of the input adjustment — for `input_in`, both ends of the
	/// range are shifted back by subtracting the `time_in` value (C++
	/// `get_remapped_output_time()`: `input - time_in`); all other inputs
	/// fall through to the base-class identity behavior
	/// (`// CPP-PARITY: timeoffsetnode.cpp` `output_time_adjustment`).
	fn output_time_adjustment(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		Self::output_time_adjustment_with(core, input, element, time, traverse)
	}

	/// Evaluate outputs (C++ `value()`): pushes the value arriving at
	/// `input_in` through unchanged (the actual time shift happens via the
	/// time-adjustment overrides above).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		let _ = (core, time);
		// `table->push(value.at(k_input_input))` — the value passes through
		// unchanged, whatever its type (texture values included).
		if let Some(v) = inputs.get(INPUT_INPUT) {
			table.push(v.value_type(), v.clone(), None);
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TimeOffsetNode))
	}
}

/// Constructor (C++ `TimeOffsetNode::TimeOffsetNode()`): adds `time_in`
/// (rational, default 0, not-connectable, time-slider view with viewlock)
/// and `input_in` (type-none pass-through, not-keyframable).
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut time_input = crate::input::Input::new(
		TIME_INPUT,
		crate::value::ValueType::Rational,
		NodeValue::Rational(Rational::new(0, 1)),
	);
	time_input.flags |= crate::input::flags::NOT_CONNECTABLE;
	time_input.properties = vec![
		("view".to_string(), NodeValue::Text("time".to_string())),
		("viewlock".to_string(), NodeValue::Boolean(true)),
	];
	core.add_input(time_input);

	let mut input_input =
		crate::input::Input::new(INPUT_INPUT, crate::value::ValueType::None, NodeValue::None);
	input_input.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(input_input);

	(core, Box::new(TimeOffsetNode))
}

/// Register this node type (C++ factory listing for
/// `org.olivevideoeditor.Olive.timeoffset`; see the note on
/// [`NodeBehavior::categories`] about the missing `Time` category).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.timeoffset",
		name: "Time Offset",
		categories: &[],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValueTable, ValueType};
	use oak_core::Rational;

	#[test]
	fn input_names() {
		let n = TimeOffsetNode;
		assert_eq!(n.input_name(TIME_INPUT), "Time");
		assert_eq!(n.input_name(INPUT_INPUT), "Input");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.timeoffset");
		let time_in = core.get_input(TIME_INPUT).unwrap();
		assert_eq!(time_in.value_type, ValueType::Rational);
		assert_eq!(time_in.default, NodeValue::Rational(Rational::new(0, 1)));
		assert_ne!(time_in.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert!(time_in
			.properties
			.iter()
			.any(|(k, v)| { k == "view" && v == &NodeValue::Text("time".to_string()) }));
		assert!(time_in
			.properties
			.iter()
			.any(|(k, v)| { k == "viewlock" && v == &NodeValue::Boolean(true) }));
		let input_in = core.get_input(INPUT_INPUT).unwrap();
		assert_eq!(input_in.value_type, ValueType::None);
		assert_ne!(input_in.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	}

	#[test]
	fn get_remapped_time_shifts_forward() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		assert_eq!(
			TimeOffsetNode::get_remapped_time(&core, Rational::new(10, 1)),
			Rational::new(15, 1)
		);
	}

	#[test]
	fn get_remapped_output_time_shifts_back() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		assert_eq!(
			TimeOffsetNode::get_remapped_output_time(&core, Rational::new(10, 1)),
			Rational::new(5, 1)
		);
	}

	#[test]
	fn input_time_adjustment_shifts_range_forward() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		let r = TimeOffsetNode::input_time_adjustment_with(
			&core,
			INPUT_INPUT,
			-1,
			TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
			true,
		);
		assert_eq!(r.in_(), Rational::new(15, 1));
		assert_eq!(r.out(), Rational::new(25, 1));
	}

	#[test]
	fn output_time_adjustment_shifts_range_back() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		let r = TimeOffsetNode::output_time_adjustment_with(
			&core,
			INPUT_INPUT,
			-1,
			TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
			true,
		);
		assert_eq!(r.in_(), Rational::new(5, 1));
		assert_eq!(r.out(), Rational::new(15, 1));
	}

	#[test]
	fn adjustments_evaluate_offset_per_endpoint() {
		let (mut core, _) = create();
		// A non-constant (keyframed) offset: each endpoint is shifted by the
		// time_in value evaluated at that endpoint.
		core.keyframe_track_mut(TIME_INPUT, -1)
			.set_key(crate::keyframe::Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Rational(Rational::new(1, 1)),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		core.keyframe_track_mut(TIME_INPUT, -1)
			.set_key(crate::keyframe::Keyframe {
				time: Rational::new(20, 1),
				value: NodeValue::Rational(Rational::new(3, 1)),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		let t = TimeRange::new(Rational::new(0, 1), Rational::new(20, 1));
		let shifted = TimeOffsetNode::input_time_adjustment_with(&core, INPUT_INPUT, -1, t, true);
		// 0 + offset(0s) = 1; 20 + offset(20s) = 23.
		assert_eq!(shifted.in_(), Rational::new(1, 1));
		assert_eq!(shifted.out(), Rational::new(23, 1));
	}

	#[test]
	fn adjustments_are_mutually_inverse_for_constant_offset() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		let t = TimeRange::new(Rational::new(10, 1), Rational::new(20, 1));
		let shifted = TimeOffsetNode::input_time_adjustment_with(&core, INPUT_INPUT, -1, t, true);
		assert_eq!(shifted.in_(), Rational::new(15, 1));
		assert_eq!(shifted.out(), Rational::new(25, 1));
		let unshifted =
			TimeOffsetNode::output_time_adjustment_with(&core, INPUT_INPUT, -1, shifted, true);
		assert_eq!(unshifted, t);
	}

	#[test]
	fn adjustments_other_inputs_are_identity() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		let t = TimeRange::new(Rational::new(10, 1), Rational::new(20, 1));
		assert_eq!(
			TimeOffsetNode::input_time_adjustment_with(&core, "other_in", -1, t, true),
			t
		);
		assert_eq!(
			TimeOffsetNode::output_time_adjustment_with(&core, "other_in", -1, t, true),
			t
		);
	}

	#[test]
	fn value_passes_input_through() {
		let (core, behavior) = create();
		let mut row = crate::value::NodeValueRow::default();
		row.insert(INPUT_INPUT.to_string(), NodeValue::Float(42.0));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(42.0)));
	}

	#[test]
	fn duplicate_copies_node() {
		let (_core, behavior) = create();
		let copy = behavior.duplicate(&_core).unwrap();
		assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.timeoffset");
		assert_eq!(copy.name(), "Time Offset");
	}
}
