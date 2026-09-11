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

//! Traverser (evaluation engine) contract tests.

use oak_core::{Rational, TimeRange};

use oak_node::error::Error;
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::input::Input;
use oak_node::node::{NodeBehavior, NodeCore};
use oak_node::traverser::{EvalRequest, RenderHooks, Traverser};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

/// A source node: pushes its `val_in` standard value into the table.
struct Src;
impl NodeBehavior for Src {
	fn name(&self) -> &str {
		"Src"
	}
	fn type_id(&self) -> &str {
		"test.src"
	}
	fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(Src))
	}
	fn value(&self, core: &NodeCore, _i: &NodeValueRow, _t: Rational, table: &mut NodeValueTable) {
		table.push(ValueType::Float, core.standard_value("val_in", -1), None);
	}
}

/// A node that pushes `input + 1`.
struct Inc;
impl NodeBehavior for Inc {
	fn name(&self) -> &str {
		"Inc"
	}
	fn type_id(&self) -> &str {
		"test.inc"
	}
	fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(Inc))
	}
	fn value(
		&self,
		_c: &NodeCore,
		inputs: &NodeValueRow,
		_t: Rational,
		table: &mut NodeValueTable,
	) {
		let v = inputs
			.get("val_in")
			.cloned()
			.unwrap_or(NodeValue::Float(0.0));
		table.push(
			ValueType::Float,
			NodeValue::Float(v.to_double() + 1.0),
			None,
		);
	}
}

struct Noop;
impl RenderHooks for Noop {}

/// A behavior that counts its evaluations.
struct Count(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl NodeBehavior for Count {
	fn name(&self) -> &str {
		"Count"
	}
	fn type_id(&self) -> &str {
		"test.count"
	}
	fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(Count(self.0.clone())))
	}
	fn value(&self, _c: &NodeCore, _i: &NodeValueRow, _t: Rational, table: &mut NodeValueTable) {
		self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
		table.push(ValueType::Int, NodeValue::Int(1), None);
	}
}

fn node_with_input(g: &mut Graph, behavior: Box<dyn NodeBehavior>) -> NodeId {
	let mut core = NodeCore::new();
	core.add_input(Input::new(
		"val_in",
		ValueType::Float,
		NodeValue::Float(0.0),
	));
	core.add_input(Input::new(
		"val_in2",
		ValueType::Float,
		NodeValue::Float(0.0),
	));
	g.add_node(core, behavior)
}

/// A linear chain of test nodes evaluates in topological order and the
/// root table contains the expected value.
#[test]
fn linear_chain_evaluation_order() {
	let mut g = Graph::new();
	let src = node_with_input(&mut g, Box::new(Src));
	g.get_mut(src)
		.unwrap()
		.core
		.set_standard_value("val_in", -1, NodeValue::Float(1.0));
	let a = node_with_input(&mut g, Box::new(Inc));
	let b = node_with_input(&mut g, Box::new(Inc));
	let root = node_with_input(&mut g, Box::new(Inc));
	g.connect(src, a, "val_in", -1).unwrap();
	g.connect(a, b, "val_in", -1).unwrap();
	g.connect(b, root, "val_in", -1).unwrap();

	let mut t = Traverser::new();
	let mut hooks = Noop;
	let table = t
		.evaluate(&g, &EvalRequest::new(root, Rational::new(0, 1)), &mut hooks)
		.unwrap();
	assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(4.0)));
}

/// Diamond graph: shared upstream evaluates once (memoization).
#[test]
fn diamond_evaluates_shared_node_once() {
	let mut g = Graph::new();
	let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
	let shared = {
		let mut core = NodeCore::new();
		core.add_input(Input::new("val_in", ValueType::Int, NodeValue::Int(0)));
		core.add_input(Input::new("val_in2", ValueType::Int, NodeValue::Int(0)));
		g.add_node(core, Box::new(Count(counter.clone())))
	};
	let leaf = {
		let mut core = NodeCore::new();
		core.add_input(Input::new("a", ValueType::Int, NodeValue::Int(0)));
		core.add_input(Input::new("b", ValueType::Int, NodeValue::Int(0)));
		g.add_node(
			core,
			Box::new(Count(std::sync::Arc::new(
				std::sync::atomic::AtomicUsize::new(0),
			))),
		)
	};
	g.connect(shared, leaf, "a", -1).unwrap();
	g.connect(shared, leaf, "b", -1).unwrap();

	let mut t = Traverser::new();
	let mut hooks = Noop;
	let _ = t
		.evaluate(&g, &EvalRequest::new(leaf, Rational::new(0, 1)), &mut hooks)
		.unwrap();
	assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Cancellation: hook returning cancelled stops evaluation with E_STATE.
#[test]
fn cancellation_stops_evaluation() {
	struct Cancel;
	impl RenderHooks for Cancel {
		fn is_cancelled(&self) -> bool {
			true
		}
	}

	let mut g = Graph::new();
	let id = node_with_input(&mut g, Box::new(Src));
	let mut t = Traverser::new();
	let mut hooks = Cancel;
	let r = t.evaluate(&g, &EvalRequest::new(id, Rational::new(0, 1)), &mut hooks);
	match r {
		Err(Error::State) => {}
		other => panic!("expected E_STATE, got {:?}", other.map(|_| ())),
	}
}

/// Deep chain (10k nodes) completes without recursion (stack-safe).
#[test]
fn deep_graph_is_iterative() {
	let mut g = Graph::new();
	let mut prev = node_with_input(&mut g, Box::new(Inc));
	for _ in 0..10_000 {
		let next = node_with_input(&mut g, Box::new(Inc));
		g.connect(prev, next, "val_in", -1).unwrap();
		prev = next;
	}
	let mut t = Traverser::new();
	let mut hooks = Noop;
	let table = t
		.evaluate(&g, &EvalRequest::new(prev, Rational::new(0, 1)), &mut hooks)
		.unwrap();
	assert!(table.get(ValueType::Float).is_some());
}

/// invalidate_downstream marks exactly the downstream caches and only
/// once per node on a diamond (signal-free fan-out parity).
#[test]
fn invalidation_fanout() {
	let mut g = Graph::new();
	let a = node_with_input(&mut g, Box::new(Src));
	let b = node_with_input(&mut g, Box::new(Inc));
	let c = node_with_input(&mut g, Box::new(Inc));
	let d = node_with_input(&mut g, Box::new(Inc));
	g.connect(a, b, "val_in", -1).unwrap();
	g.connect(a, c, "val_in", -1).unwrap();
	g.connect(b, d, "val_in", -1).unwrap();
	g.connect(c, d, "val_in2", -1).unwrap();

	let mut t = Traverser::new();
	t.invalidate_downstream(
		&g,
		a,
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
	);
	let walked = t.last_invalidation();
	assert_eq!(walked.len(), 4, "a, b, c, d each exactly once");
	assert!(walked.contains(&a) && walked.contains(&d));
	let _ = NodeId::INVALID;
}

/// A behavior that echoes its `val_in` row value into the table (probes
/// what the traverser fed it).
struct Echo;
impl NodeBehavior for Echo {
	fn name(&self) -> &str {
		"Echo"
	}
	fn type_id(&self) -> &str {
		"test.echo"
	}
	fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(Echo))
	}
	fn value(&self, _c: &NodeCore, inputs: &NodeValueRow, _t: Rational, table: &mut NodeValueTable) {
		if let Some(v) = inputs.get("val_in") {
			table.push(ValueType::Float, v.clone(), None);
		}
	}
}

/// Unconnected inputs are filled with the keyframe-interpolated value at
/// the evaluation time (C++ GetValueAtTime): a linear 0→10 keyframe
/// track read at its midpoint feeds 5.
#[test]
fn unconnected_input_evaluates_keyframes_at_time() {
	use oak_node::keyframe::Keyframe;
	let mut g = Graph::new();
	let mut core = NodeCore::new();
	core.add_input(Input::new("val_in", ValueType::Float, NodeValue::Float(0.0)));
	core.keyframe_track_mut("val_in", -1).set_key(Keyframe {
		time: Rational::new(0, 1),
		value: NodeValue::Float(0.0),
		interpolation: oak_node::keyframe::Interpolation::Linear,
		bezier_in: (0.0, 0.0),
		bezier_out: (0.0, 0.0),
	});
	core.keyframe_track_mut("val_in", -1).set_key(Keyframe {
		time: Rational::new(10, 1),
		value: NodeValue::Float(10.0),
		interpolation: oak_node::keyframe::Interpolation::Linear,
		bezier_in: (0.0, 0.0),
		bezier_out: (0.0, 0.0),
	});
	let id = g.add_node(core, Box::new(Echo));

	let mut t = Traverser::new();
	let mut hooks = Noop;
	let table = t
		.evaluate(&g, &EvalRequest::new(id, Rational::new(5, 1)), &mut hooks)
		.unwrap();
	assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(5.0)));
}

/// A connected input is evaluated at the consumer's adjusted time (C++
/// InputTimeAdjustment with traverse=true): the consumer doubles the
/// time, the upstream time-echo reports what it was evaluated at.
#[test]
fn connected_input_uses_adjusted_time() {
	struct TimeEcho;
	impl NodeBehavior for TimeEcho {
		fn name(&self) -> &str {
			"TimeEcho"
		}
		fn type_id(&self) -> &str {
			"test.timeecho"
		}
		fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			Some(Box::new(TimeEcho))
		}
		fn value(&self, _c: &NodeCore, _i: &NodeValueRow, t: Rational, table: &mut NodeValueTable) {
			table.push(ValueType::Rational, NodeValue::Rational(t), None);
		}
	}
	struct Doubler;
	impl NodeBehavior for Doubler {
		fn name(&self) -> &str {
			"Doubler"
		}
		fn type_id(&self) -> &str {
			"test.doubler"
		}
		fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			Some(Box::new(Doubler))
		}
		fn input_time_adjustment(
			&self,
			_core: &NodeCore,
			input: &str,
			_element: i32,
			time: TimeRange,
			traverse: bool,
		) -> TimeRange {
			if input == "val_in" && traverse {
				TimeRange::new(time.in_() * Rational::new(2, 1), time.out() * Rational::new(2, 1))
			} else {
				time
			}
		}
		fn value(&self, _c: &NodeCore, inputs: &NodeValueRow, _t: Rational, table: &mut NodeValueTable) {
			if let Some(v) = inputs.get("val_in") {
				table.push(ValueType::Rational, v.clone(), None);
			}
		}
	}

	let mut g = Graph::new();
	let src = node_with_input(&mut g, Box::new(TimeEcho));
	let consumer = node_with_input(&mut g, Box::new(Doubler));
	g.connect(src, consumer, "val_in", -1).unwrap();

	let mut t = Traverser::new();
	let mut hooks = Noop;
	let table = t
		.evaluate(&g, &EvalRequest::new(consumer, Rational::new(3, 1)), &mut hooks)
		.unwrap();
	assert_eq!(
		table.get(ValueType::Rational),
		Some(&NodeValue::Rational(Rational::new(6, 1))),
		"the upstream was evaluated at the doubled time"
	);
}

/// The real TimeOffsetNode (not a test double) remaps its upstream's
/// evaluation time through the trait: the keyframable offset lives on
/// the node core, which the trait method now receives. Evaluating the
/// offset at t=10 with `time_in` = 5 pulls the upstream at t=15 (C++
/// `get_remapped_time`: `input + time_in`).
#[test]
fn time_offset_node_shifts_upstream_evaluation_time() {
	struct TimeEcho;
	impl NodeBehavior for TimeEcho {
		fn name(&self) -> &str {
			"TimeEcho"
		}
		fn type_id(&self) -> &str {
			"test.timeecho"
		}
		fn duplicate(&self, _c: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			Some(Box::new(TimeEcho))
		}
		fn value(&self, _c: &NodeCore, _i: &NodeValueRow, t: Rational, table: &mut NodeValueTable) {
			table.push(ValueType::Rational, NodeValue::Rational(t), None);
		}
	}

	let mut g = Graph::new();
	let src = node_with_input(&mut g, Box::new(TimeEcho));
	let (mut core, behavior) = oak_node::factory::Factory::global()
		.create_any("org.olivevideoeditor.Olive.timeoffset")
		.expect("timeoffset registered");
	core.set_standard_value("time_in", -1, NodeValue::Rational(Rational::new(5, 1)));
	let offset = g.add_node(core, behavior);
	g.connect(src, offset, "input_in", -1).unwrap();

	let mut t = Traverser::new();
	let mut hooks = Noop;
	let table = t
		.evaluate(&g, &EvalRequest::new(offset, Rational::new(10, 1)), &mut hooks)
		.unwrap();
	assert_eq!(
		table.get(ValueType::Rational),
		Some(&NodeValue::Rational(Rational::new(15, 1))),
		"the upstream must be evaluated at the offset time (10 + 5)"
	);
}
