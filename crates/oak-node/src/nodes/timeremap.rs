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

//! Time remap node (C++ `src/node/src/time/timeremap/timeremap.{h,cpp}`,
//! `olive::TimeRemapNode`).

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::{Rational, TimeRange};

/// Target-time input id (C++ `k_time_input`). Type: rational; default
/// `Rational(0)`; flags: not-connectable; properties: `view = time slider`,
/// `viewlock = true`.
pub const TIME_INPUT: &str = "time_in";

/// Effect input id (C++ `k_input_input`). Type: none (pass-through of any
/// connected type); flags: not-keyframable.
pub const INPUT_INPUT: &str = "input_in";

/// Time remap node. Replaces the time seen by the connected input with an
/// arbitrary (keyframable) time. The C++ class has no own data members
/// (only the private `get_remapped_time()` helper, which becomes the
/// behavior of `input_time_adjustment` below), so this is a unit-like
/// struct.
pub struct TimeRemapNode;

impl TimeRemapNode {
	/// C++ `get_remapped_time()`: the `time_in` input's value at
	/// `input_time` (keyframes when the track is set, else the standard
	/// value). `time_in` is not-connectable, so no connected edge is ever
	/// consulted.
	pub fn get_remapped_time(core: &NodeCore, input_time: Rational) -> Rational {
		match core.value_at_time(TIME_INPUT, -1, input_time) {
			NodeValue::Rational(r) => r,
			v => Rational::from_double(v.to_double()),
		}
	}

	/// The full C++ `input_time_adjustment()` with an explicit core. For
	/// `input_in`, both endpoints of the range are replaced by the
	/// `time_in` value evaluated at that endpoint (the remap discards the
	/// original time); all other inputs fall through to the base-class
	/// identity behavior.
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
}

impl NodeBehavior for TimeRemapNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Time Remap"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.timeremap"
	}

	/// Categories (C++ `category()` returns `{ k_category_time }`; the Rust
	/// `Category` enum has no `Time` variant yet, so this is empty until one
	/// is added).
	fn categories(&self) -> &[Category] {
		&[]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Arbitrarily remap time through the nodes."
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
	/// `input_in`, both ends of the range are replaced by the `time_in`
	/// value at that time (C++ `get_remapped_time()`: `time_in` evaluated at
	/// `input`, discarding the original time); all other inputs fall through
	/// to the base-class identity behavior. `core` carries the node's data
	/// (the keyframable `time_in` input), matching the C++ member read
	/// (`// CPP-PARITY: timeremap.cpp` `input_time_adjustment`).
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

	/// Output-side time remap (C++ `output_time_adjustment()`): the C++
	/// override has its real inverse implementation commented out (an
	/// arbitrary remap is not invertible) and unconditionally defers to the
	/// base-class identity behavior; declared here for parity.
	fn output_time_adjustment(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
		time: TimeRange,
		traverse: bool,
	) -> TimeRange {
		let _ = (core, input, element, traverse);
		time
	}

	/// Evaluate outputs (C++ `value()`): pushes the value arriving at
	/// `input_in` through unchanged (the actual time remap happens via the
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
		Some(Box::new(TimeRemapNode))
	}
}

/// Constructor (C++ `TimeRemapNode::TimeRemapNode()`): adds `time_in`
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

	(core, Box::new(TimeRemapNode))
}

/// Register this node type (C++ factory listing for
/// `org.olivevideoeditor.Olive.timeremap`; see the note on
/// [`NodeBehavior::categories`] about the missing `Time` category).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.timeremap",
		name: "Time Remap",
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
		let n = TimeRemapNode;
		assert_eq!(n.input_name(TIME_INPUT), "Time");
		assert_eq!(n.input_name(INPUT_INPUT), "Input");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.timeremap");
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
	fn get_remapped_time_uses_standard_value() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		assert_eq!(
			TimeRemapNode::get_remapped_time(&core, Rational::new(0, 1)),
			Rational::new(5, 1)
		);
		assert_eq!(
			TimeRemapNode::get_remapped_time(&core, Rational::new(30, 1)),
			Rational::new(5, 1)
		);
	}

	#[test]
	fn get_remapped_time_uses_keyframe_curve() {
		let (mut core, _) = create();
		// A time-remap curve: at 0s the input shows 10s, at 10s it shows 0s
		// (reverse). Evaluated exactly at the keyframe times, so no
		// interpolation is involved.
		core.keyframe_track_mut(TIME_INPUT, -1)
			.set_key(crate::keyframe::Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Rational(Rational::new(10, 1)),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		core.keyframe_track_mut(TIME_INPUT, -1)
			.set_key(crate::keyframe::Keyframe {
				time: Rational::new(10, 1),
				value: NodeValue::Rational(Rational::new(0, 1)),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		assert_eq!(
			TimeRemapNode::get_remapped_time(&core, Rational::new(0, 1)),
			Rational::new(10, 1)
		);
		assert_eq!(
			TimeRemapNode::get_remapped_time(&core, Rational::new(10, 1)),
			Rational::new(0, 1)
		);
		// Mid-way between the keys the curve is linear: 10s -> 5s.
		assert_eq!(
			TimeRemapNode::get_remapped_time(&core, Rational::new(5, 1)),
			Rational::new(5, 1)
		);
	}

	#[test]
	fn input_time_adjustment_remaps_both_endpoints() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		// Both endpoints are replaced by the time_in value at that time
		// (the original time is discarded), so the range collapses to (5,5).
		let r = TimeRemapNode::input_time_adjustment_with(
			&core,
			INPUT_INPUT,
			-1,
			TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
			true,
		);
		assert_eq!(r.in_(), Rational::new(5, 1));
		assert_eq!(r.out(), Rational::new(5, 1));
	}

	#[test]
	fn input_time_adjustment_other_inputs_are_identity() {
		let (mut core, _) = create();
		core.set_standard_value(TIME_INPUT, -1, NodeValue::Rational(Rational::new(5, 1)));
		let t = TimeRange::new(Rational::new(10, 1), Rational::new(20, 1));
		let r = TimeRemapNode::input_time_adjustment_with(&core, "other_in", -1, t, true);
		assert_eq!(r, t);
	}

	#[test]
	fn output_time_adjustment_is_identity() {
		let n = TimeRemapNode;
		let t = TimeRange::new(Rational::new(10, 1), Rational::new(20, 1));
		assert_eq!(
			n.output_time_adjustment(&NodeCore::new(), INPUT_INPUT, -1, t, true),
			t
		);
		assert_eq!(
			n.output_time_adjustment(&NodeCore::new(), "other_in", -1, t, true),
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
	fn value_pushes_nothing_when_input_absent() {
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
	fn duplicate_copies_node() {
		let (_core, behavior) = create();
		let copy = behavior.duplicate(&_core).unwrap();
		assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.timeremap");
		assert_eq!(copy.name(), "Time Remap");
	}
}
