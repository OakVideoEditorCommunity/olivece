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

//! The evaluation engine — the C++ `NodeTraverser` restructured.
//!
//! Key change from C++: no inheritance. C++ `RenderProcessor :
//! NodeTraverser` overrode virtuals to plug rendering in; here the
//! traverser is a free engine and oakrender supplies [`RenderHooks`].
//!
//! Evaluation is **time-aware and memoized per (node, time)**: a node's
//! inputs may pull upstream values at adjusted times (the consuming
//! node's `input_time_adjustment` — clips map sequence time to media
//! time, tracks clamp to the covering block, C++
//! `traverser.cpp` `ProcessInput`), so one evaluation pass can evaluate
//! the same node at several times (keyed like the C++ `value_cache_`,
//! which is per (node, range)). The walk is an explicit-stack DFS —
//! 10k-deep chains must not blow the call stack (the earlier
//! topological-order pass was recursion-free for the same reason).
//!
//! Input rows carry the C++ `GenerateRowValue` semantics: connected
//! inputs take the upstream output (evaluated at the adjusted time);
//! unconnected inputs take `NodeCore::value_at_time` — keyframe
//! interpolation when the track is non-empty, else the standard value
//! (C++ `ProcessInputElement` → `GetValueAtTime`).
//! `// CPP-PARITY: src/node/src/traverser.cpp`.

use std::collections::{BTreeSet, HashMap, HashSet};

use oak_core::{Rational, TimeRange};

use crate::graph::Graph;
use crate::id::NodeId;
use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

/// Backend hooks supplied by the consumer (oakrender). Default no-ops
/// give the C++ "offline evaluation" behavior.
pub trait RenderHooks {
	/// Whether cached textures may be used (C++ `use_cache()`).
	fn use_cache(&self) -> bool {
		false
	}

	/// Convert a finished value row into a backend job/texture
	/// (C++ `resolve_jobs` / `process_*_job` family).
	fn resolve(&mut self, node: NodeId, row: &NodeValueRow, table: &mut NodeValueTable) {
		let _ = (node, row, table);
	}

	/// Cancel-check polled between nodes (C++ `IsCancelled`).
	fn is_cancelled(&self) -> bool {
		false
	}
}

/// Evaluation request.
pub struct EvalRequest {
	/// Root node to evaluate.
	pub root: NodeId,
	/// Time.
	pub time: Rational,
	/// Optional range (for audio pulls).
	pub range: Option<TimeRange>,
}

impl EvalRequest {
	/// New request.
	pub fn new(root: NodeId, time: Rational) -> EvalRequest {
		EvalRequest {
			root,
			time,
			range: None,
		}
	}
}

/// The traversal engine.
pub struct Traverser {
	/// Nodes touched by the last [`Traverser::invalidate_downstream`]
	/// walk (observable for tests; the C++ fan-out has no return value).
	last_invalidation: Vec<NodeId>,
}

/// DFS stack frame: `Enter` queues the upstream nodes, `Exit` builds the
/// row and evaluates.
enum Frame {
	Enter(NodeId, Rational),
	Exit(NodeId, Rational),
}

impl Traverser {
	/// New empty engine (reusable across evaluations).
	pub fn new() -> Self {
		Traverser {
			last_invalidation: Vec::new(),
		}
	}

	/// Nodes marked by the last invalidation walk.
	pub fn last_invalidation(&self) -> &[NodeId] {
		&self.last_invalidation
	}

	/// Evaluate `request` against `graph`, calling `hooks` at the
	/// backend seams. Returns the root's output table.
	///
	/// Errors: `State` on cancellation, `NotFound` on an invalid root.
	/// Only nodes upstream of the root are evaluated (lazy — the C++
	/// recursion shares this property).
	pub fn evaluate(
		&mut self,
		graph: &Graph,
		request: &EvalRequest,
		hooks: &mut dyn RenderHooks,
	) -> crate::error::Result<NodeValueTable> {
		use crate::error::Error;
		if !graph.is_valid(request.root) {
			return Err(Error::NotFound);
		}

		// Per-pass memo: (node, time) -> evaluated output table. A shared
		// upstream evaluates once per requested time (C++ value_cache_).
		let mut cache: HashMap<(NodeId, Rational), NodeValueTable> = HashMap::new();
		walk_dfs(graph, &mut cache, request.root, request.time, hooks)?;
		Ok(cache
			.remove(&(request.root, request.time))
			.unwrap_or_default())
	}

	/// Evaluate the graph as one Kahn-order sweep of the *live set* and
	/// return the [`GraphOutput`](crate::nodes::graphendpoints) value —
	/// the frame (C++ has no counterpart; the C++ traversal starts at the
	/// consumer and walks backwards, Oak starts at the graph's input
	/// endpoint and converges on its output endpoint).
	///
	/// The live set is `(downstream of the input ∨ feeding the input) ∧
	/// (upstream of the output)`: the input's forward cone plus the nodes
	/// that reach the input (a sequence feeds the graph through
	/// `GraphInput.feed_in`, which puts it in that feeder cone),
	/// intersected with the output's backward cone. Isolated nodes and
	/// branches that never reach the output are never queued. A
	/// connection coming from *outside* the live set leaves `None` in the
	/// consuming row (same as a missing cache entry in the DFS walk).
	///
	/// Each live node is dequeued once, in ascending [`NodeId`] order
	/// among the ready nodes, with the DFS walk's per-node semantics
	/// (`GenerateRowValue` → `value()` → [`RenderHooks::resolve`]), and
	/// the resolved table is what its downstream consumers see. The
	/// ascending-id order is the only deterministic order available:
	/// `Graph::edges` is a `BTreeSet` keyed by `(from, to, input,
	/// element)`, so the order in which connections were made is not
	/// stored and cannot be recovered.
	///
	/// Time semantics are single-time: `time` is the frame being
	/// rendered, and the consuming node's `input_time_adjustment` applies
	/// as usual — an upstream at a different time is evaluated through
	/// the DFS memo (exactly what [`Traverser::evaluate`] would do),
	/// while same-time upstreams are the sweep's own job. Multi-time
	/// memoization across a range is M1+ work.
	///
	/// Errors: `NotFound` when either endpoint is missing; `Failed` when
	/// the live set cannot be ordered (the message names a cycle found
	/// in it) or when the output is not reachable from the input; `State`
	/// on cancellation.
	pub fn eval_graph_bfs(
		&mut self,
		graph: &Graph,
		time: Rational,
		hooks: &mut dyn RenderHooks,
	) -> crate::error::Result<NodeValue> {
		use crate::error::Error;
		let (input, output) = graph.endpoints().ok_or(Error::NotFound)?;
		let live = live_nodes(graph, input, output);

		// In-degree over the live-induced subgraph only: an edge whose
		// source lies outside the live set must not keep its target from
		// ever becoming ready.
		let mut indegree: HashMap<NodeId, usize> = HashMap::new();
		for node in &live {
			let n = graph
				.input_connections(*node)
				.iter()
				.filter(|(from, _, _)| live.contains(from))
				.count();
			indegree.insert(*node, n);
		}
		let mut ready: BTreeSet<NodeId> = indegree
			.iter()
			.filter(|(_, n)| **n == 0)
			.map(|(node, _)| *node)
			.collect();

		let mut cache: HashMap<(NodeId, Rational), NodeValueTable> = HashMap::new();
		let mut done = 0usize;
		while let Some(node) = ready.iter().next().copied() {
			ready.remove(&node);
			if hooks.is_cancelled() {
				return Err(Error::State);
			}
			let Some(entry) = graph.get(node) else {
				continue;
			};
			done += 1;
			// Time-shifted upstreams are not part of this sweep; pull them
			// through the DFS memo. A node may already own a table at
			// `time` (a pull deeper down evaluated it out of order) — in
			// that case it is not evaluated (and resolved) twice.
			pull_adjusted_upstreams(graph, &mut cache, entry, node, time, hooks)?;
			if !cache.contains_key(&(node, time)) {
				let row = build_row(graph, &cache, entry, node, time);
				let mut table = NodeValueTable::default();
				entry.behavior.value(&entry.core, &row, time, &mut table);
				hooks.resolve(node, &row, &mut table);
				cache.insert((node, time), table);
			}
			for (to, _, _) in graph.output_connections(node) {
				if !live.contains(&to) {
					continue;
				}
				if let Some(n) = indegree.get_mut(&to) {
					*n -= 1;
					if *n == 0 {
						ready.insert(to);
					}
				}
			}
		}

		if done < live.len() {
			let cycle = find_cycle(graph, &indegree);
			return Err(Error::Failed(format!("graph contains a cycle: {cycle:?}")));
		}

		let Some(table) = cache.remove(&(output, time)) else {
			return Err(Error::Failed(
				"graph output is not reachable from the graph input".to_string(),
			));
		};
		Ok(table
			.get(ValueType::Texture)
			.cloned()
			.or_else(|| table.rows().last().map(|(_, value, _)| value.clone()))
			.unwrap_or(NodeValue::None))
	}

	/// Invalidate walk: mark downstream caches dirty after an input
	/// change (C++ `invalidate_cache` fan-out, signal-free). Records the
	/// walked set in [`Traverser::last_invalidation`].
	pub fn invalidate_downstream(&mut self, graph: &Graph, from: NodeId, range: TimeRange) {
		let _ = range;
		self.last_invalidation.clear();
		let mut seen: HashSet<NodeId> = HashSet::new();
		let mut queue: Vec<NodeId> = vec![from];
		while let Some(n) = queue.pop() {
			if !seen.insert(n) {
				continue;
			}
			self.last_invalidation.push(n);
			queue.extend(graph.downstream(n));
		}
	}
}

impl Default for Traverser {
	fn default() -> Self {
		Self::new()
	}
}

/// Explicit-stack DFS from `root` at `time` (the C++ recursion, minus
/// the recursion), memoizing every evaluated `(node, time)` table into
/// `cache`. `Enter` queues the connected upstreams at their adjusted
/// times, `Exit` builds the row and runs `GenerateRowValue` →
/// `value()` → [`RenderHooks::resolve`]; a node is entered at most once
/// per time (`queued` is the DFS gray set, `cache` the black set).
///
/// Shared by [`Traverser::evaluate`] (whole walk) and
/// [`Traverser::eval_graph_bfs`] (time-shifted upstream pulls).
///
/// Errors: `State` on cancellation.
fn walk_dfs(
	graph: &Graph,
	cache: &mut HashMap<(NodeId, Rational), NodeValueTable>,
	root: NodeId,
	time: Rational,
	hooks: &mut dyn RenderHooks,
) -> crate::error::Result<()> {
	use crate::error::Error;
	let mut queued: HashSet<(NodeId, Rational)> = HashSet::new();
	let mut stack: Vec<Frame> = vec![Frame::Enter(root, time)];
	queued.insert((root, time));

	while let Some(frame) = stack.pop() {
		if hooks.is_cancelled() {
			return Err(Error::State);
		}
		match frame {
			Frame::Enter(node, time) => {
				if cache.contains_key(&(node, time)) {
					continue;
				}
				let Some(entry) = graph.get(node) else {
					continue;
				};
				stack.push(Frame::Exit(node, time));
				// Queue every connected upstream at its adjusted time.
				for (from, input, element) in graph.input_connections(node) {
					let from = entry
						.behavior
						.connected_render_output(&entry.core, &input, element)
						.unwrap_or(from);
					let adjusted = adjusted_time(entry, &input, element, time);
					let key = (from, adjusted);
					if !cache.contains_key(&key) && queued.insert(key) {
						stack.push(Frame::Enter(from, adjusted));
					}
				}
			}
			Frame::Exit(node, time) => {
				if cache.contains_key(&(node, time)) {
					continue;
				}
				let Some(entry) = graph.get(node) else {
					continue;
				};
				let row = build_row(graph, cache, entry, node, time);
				let mut table = NodeValueTable::default();
				entry.behavior.value(&entry.core, &row, time, &mut table);
				hooks.resolve(node, &row, &mut table);
				cache.insert((node, time), table);
			}
		}
	}
	Ok(())
}

/// Pull the upstreams of `node` that the sweep will not visit itself:
/// connections whose `input_time_adjustment` maps `time` to a different
/// time are evaluated on the spot through the DFS memo (the very walk
/// [`Traverser::evaluate`] performs, so those values carry the same
/// semantics), and their tables land in `cache` for [`build_row`].
///
/// Errors: `State` on cancellation (propagated from the pull).
fn pull_adjusted_upstreams(
	graph: &Graph,
	cache: &mut HashMap<(NodeId, Rational), NodeValueTable>,
	entry: &crate::graph::NodeEntry,
	node: NodeId,
	time: Rational,
	hooks: &mut dyn RenderHooks,
) -> crate::error::Result<()> {
	for (from, input, element) in graph.input_connections(node) {
		let from = entry
			.behavior
			.connected_render_output(&entry.core, &input, element)
			.unwrap_or(from);
		let adjusted = adjusted_time(entry, &input, element, time);
		if adjusted == time || !graph.is_valid(from) {
			continue;
		}
		if !cache.contains_key(&(from, adjusted)) {
			walk_dfs(graph, cache, from, adjusted, hooks)?;
		}
	}
	Ok(())
}

/// Every node reachable from `root` (including `root`) walking
/// downstream, or — with `upstream` — every node that can reach `root`.
fn reachable(graph: &Graph, root: NodeId, upstream: bool) -> HashSet<NodeId> {
	let mut seen: HashSet<NodeId> = HashSet::new();
	let mut stack: Vec<NodeId> = vec![root];
	while let Some(node) = stack.pop() {
		if !seen.insert(node) {
			continue;
		}
		if upstream {
			stack.extend(graph.upstream(node));
		} else {
			stack.extend(graph.downstream(node));
		}
	}
	seen
}

/// The live set of the endpoint-to-endpoint sweep: the input's forward
/// cone plus its feeder cone (nodes that reach the input — how a
/// sequence feeds `GraphInput.feed_in`), intersected with the output's
/// backward cone. See [`Traverser::eval_graph_bfs`].
fn live_nodes(graph: &Graph, input: NodeId, output: NodeId) -> HashSet<NodeId> {
	let mut live = reachable(graph, input, false);
	live.extend(reachable(graph, input, true));
	let reaches_output = reachable(graph, output, true);
	live.retain(|node| reaches_output.contains(node));
	live
}

/// A cycle inside the nodes the sweep could not order: residual nodes
/// (positive in-degree left) walked upstream, always taking the
/// smallest-id residual feeder, until a node repeats — the repeated
/// suffix is the cycle. Deterministic; empty when there is no residual
/// node.
fn find_cycle(graph: &Graph, indegree: &HashMap<NodeId, usize>) -> Vec<NodeId> {
	let residual: BTreeSet<NodeId> = indegree
		.iter()
		.filter(|(_, n)| **n > 0)
		.map(|(node, _)| *node)
		.collect();
	let mut path: Vec<NodeId> = Vec::new();
	let mut seen: HashMap<NodeId, usize> = HashMap::new();
	let Some(start) = residual.iter().next().copied() else {
		return path;
	};
	let mut node = start;
	loop {
		if let Some(&at) = seen.get(&node) {
			return path.split_off(at);
		}
		seen.insert(node, path.len());
		path.push(node);
		// Every residual node has a residual feeder: in-degree was only
		// counted over live edges, and an unprocessed feeder keeps its
		// own positive in-degree.
		match graph
			.upstream(node)
			.into_iter()
			.find(|from| residual.contains(from))
		{
			Some(from) => node = from,
			None => return path,
		}
	}
}

/// The consuming node's time adjustment for `input` (C++
/// `Node::InputTimeAdjustment` with `traverse = true`): clips map
/// sequence time to media time, tracks clamp to the covering block. The
/// trait speaks ranges; a video frame evaluates at a point, so the
/// adjusted range's `in` is the upstream time.
fn adjusted_time(
	entry: &crate::graph::NodeEntry,
	input: &str,
	element: i32,
	time: Rational,
) -> Rational {
	entry
		.behavior
		.input_time_adjustment(&entry.core, input, element, TimeRange::new(time, time), true)
		.in_()
}

/// Build the input row of `node` at `time` from the memoized upstream
/// tables plus the standard/keyframed values of unconnected inputs
/// (C++ `GenerateRowValue` + `ProcessInputElement`).
fn build_row(
	graph: &Graph,
	cache: &HashMap<(NodeId, Rational), NodeValueTable>,
	entry: &crate::graph::NodeEntry,
	node: NodeId,
	time: Rational,
) -> NodeValueRow {
	let mut row: NodeValueRow = std::collections::BTreeMap::new();
	let connections = graph.input_connections(node);
	for input in &entry.core.inputs {
		let id = input.id.as_str();
		let mut conns: Vec<(NodeId, i32)> = connections
			.iter()
			.filter(|(_, i, _)| i == id)
			.map(|(from, _, element)| (*from, *element))
			.collect();
		if conns.is_empty() {
			// Unconnected: keyframe interpolation when the track is
			// non-empty, else the standard value (C++ GetValueAtTime).
			row.insert(id.to_string(), entry.core.value_at_time(id, -1, time));
			continue;
		}
		// Array inputs (element >= 0): the consuming node may restrict
		// which elements are live at this time (C++
		// `GetActiveElementsAtTime` — a track pulls only the blocks
		// covering the frame). An empty answer means "no restriction".
		// Element-tagged keys: an array input's per-element values coexist
		// in the row under `{input}[{element}]` (C++ `GetValueAtTime`
		// indexes the array; the multi-cam node reads exactly the element
		// of its current source — a plain `id` key would collapse the
		// array to its last element).
		if conns.iter().any(|(_, e)| *e >= 0) {
			let active = entry.behavior.active_elements_at_time(id, time);
			if !active.is_empty() {
				conns.retain(|(_, e)| active.contains(e));
			}
			conns.sort_by_key(|(_, e)| *e);
		}
		for (from, element) in conns {
			let from = entry
				.behavior
				.connected_render_output(&entry.core, id, element)
				.unwrap_or(from);
			let upstream_time = adjusted_time(entry, id, element, time);
			let value = cache
				.get(&(from, upstream_time))
				.map(|t| pick_value(t, entry.core.input_data_type(id)))
				.unwrap_or(NodeValue::None);
			let key = if element >= 0 {
				format!("{id}[{element}]")
			} else {
				id.to_string()
			};
			row.insert(key, value);
		}
	}
	row
}

/// Pick the row value for an input of `data_type` from an upstream
/// output table. Texture inputs take the upstream texture directly (the
/// scalar chain would otherwise hand a plugin node's tagged param
/// passthrough to a downstream clip input); everything else takes the
/// last value of the first matching scalar type (C++ value-hint
/// resolution's common case).
fn pick_value(table: &NodeValueTable, data_type: Option<ValueType>) -> NodeValue {
	if data_type == Some(ValueType::Texture) {
		return table
			.get(ValueType::Texture)
			.cloned()
			.unwrap_or(NodeValue::None);
	}
	table
		.get(ValueType::Float)
		.or_else(|| table.get(ValueType::Int))
		.or_else(|| table.get(ValueType::Color))
		.or_else(|| table.get(ValueType::Vec2))
		.or_else(|| table.get(ValueType::Vec3))
		.or_else(|| table.get(ValueType::Vec4))
		.or_else(|| table.get(ValueType::Boolean))
		.or_else(|| table.get(ValueType::Rational))
		.or_else(|| table.get(ValueType::Text))
		.or_else(|| table.get(ValueType::Combo))
		.or_else(|| table.get(ValueType::StrCombo))
		.or_else(|| table.get(ValueType::Texture))
		.cloned()
		.unwrap_or(NodeValue::None)
}

/// A value database: per-node input rows over a time range (C++
/// `NodeValueDatabase`), exposed by the traverser ffi family.
pub struct ValueDatabase {
	/// Rows keyed by node input id.
	pub rows: Vec<(String, Vec<(ValueType, NodeValue)>)>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::error::Error;
	use crate::handle::CHandle;
	use crate::input::Input;
	use crate::node::{NodeBehavior, NodeCore};
	use crate::nodes::graphendpoints::{GRAPH_INPUT_FEED_INPUT, GRAPH_OUTPUT_INPUT};
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::{Arc, Mutex};

	/// What a [`TestNode`] pushes to its own output table.
	enum Emit {
		/// Nothing at all.
		Nothing,
		/// A constant float.
		Float(f64),
		/// The time the node was evaluated at.
		Time,
		/// A null texture handle.
		Texture,
	}

	/// Counting stand-in for a real node: emits on demand so a test can
	/// observe evaluation counts, dequeue order, and input rows.
	struct TestNode {
		emit: Emit,
		calls: Arc<AtomicUsize>,
	}

	impl NodeBehavior for TestNode {
		fn name(&self) -> &str {
			"Test Node"
		}

		fn type_id(&self) -> &str {
			"org.olivevideoeditor.Olive.test-bfs-node"
		}

		fn value(
			&self,
			_core: &NodeCore,
			_inputs: &NodeValueRow,
			time: Rational,
			table: &mut NodeValueTable,
		) {
			self.calls.fetch_add(1, Ordering::Relaxed);
			match self.emit {
				Emit::Nothing => {}
				Emit::Float(value) => table.push(ValueType::Float, NodeValue::Float(value), None),
				Emit::Time => table.push(ValueType::Float, NodeValue::Float(time.to_f64()), None),
				Emit::Texture => {
					table.push(ValueType::Texture, NodeValue::Texture(CHandle::null()), None)
				}
			}
		}

		fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			None
		}
	}

	/// Add a [`TestNode`] declaring `inputs`; returns its id and call counter.
	fn add_node(
		graph: &mut Graph,
		inputs: &[(&str, ValueType)],
		emit: Emit,
	) -> (NodeId, Arc<AtomicUsize>) {
		let calls = Arc::new(AtomicUsize::new(0));
		let mut core = NodeCore::new();
		for (id, value_type) in inputs {
			core.add_input(Input::new(id, *value_type, NodeValue::None));
		}
		let id = graph.add_node(
			core,
			Box::new(TestNode {
				emit,
				calls: Arc::clone(&calls),
			}),
		);
		(id, calls)
	}

	/// The endpoint pair with the default `input -> output` edge removed,
	/// so a test can wire the convergence itself.
	fn detached_endpoints(graph: &mut Graph) -> (NodeId, NodeId) {
		let (input, output) = graph.ensure_endpoints();
		graph.disconnect(input, output, GRAPH_OUTPUT_INPUT, -1);
		(input, output)
	}

	/// A node that samples `val_in` one second late: observes that the
	/// sweep pulls a time-shifted upstream through the DFS memo.
	struct Delay {
		rows: Arc<Mutex<Vec<NodeValueRow>>>,
	}

	impl NodeBehavior for Delay {
		fn name(&self) -> &str {
			"Delay"
		}

		fn type_id(&self) -> &str {
			"org.olivevideoeditor.Olive.test-bfs-delay"
		}

		fn input_time_adjustment(
			&self,
			_core: &NodeCore,
			input: &str,
			_element: i32,
			time: TimeRange,
			_traverse: bool,
		) -> TimeRange {
			if input == "val_in" {
				TimeRange::new(
					time.in_() + Rational::new(1, 1),
					time.out() + Rational::new(1, 1),
				)
			} else {
				time
			}
		}

		fn value(
			&self,
			_core: &NodeCore,
			inputs: &NodeValueRow,
			_time: Rational,
			table: &mut NodeValueTable,
		) {
			self.rows.lock().expect("delay rows").push(inputs.clone());
			if let Some(value @ NodeValue::Float(_)) = inputs.get("val_in") {
				table.push(ValueType::Float, value.clone(), None);
			}
		}

		fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			None
		}
	}

	/// Records resolve order/rows and can replace a node's resolved table.
	struct Probe {
		order: Vec<NodeId>,
		rows: HashMap<NodeId, NodeValueRow>,
		replace: Option<(NodeId, Vec<(ValueType, NodeValue)>)>,
	}

	impl Probe {
		fn new() -> Probe {
			Probe {
				order: Vec::new(),
				rows: HashMap::new(),
				replace: None,
			}
		}
	}

	impl RenderHooks for Probe {
		fn resolve(&mut self, node: NodeId, row: &NodeValueRow, table: &mut NodeValueTable) {
			self.order.push(node);
			self.rows.insert(node, row.clone());
			if let Some((target, rows)) = &self.replace {
				if *target == node {
					table.clear();
					for (ty, value) in rows {
						table.push(*ty, value.clone(), None);
					}
				}
			}
		}
	}

	#[test]
	fn bfs_starts_at_graph_input_and_skips_unreachable_nodes() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (dead, dead_calls) =
			add_node(&mut graph, &[("x_in", ValueType::Texture)], Emit::Nothing);
		let (_island, island_calls) = add_node(&mut graph, &[], Emit::Float(1.0));
		graph
			.connect(input, dead, "x_in", -1)
			.expect("input -> dead");
		graph
			.connect(input, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("input -> output");

		let mut probe = Probe::new();
		let value = Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("the input -> output edge alone is a valid graph");
		assert_eq!(probe.order, vec![input, output]);
		assert_eq!(dead_calls.load(Ordering::Relaxed), 0, "dead branch skipped");
		assert_eq!(
			island_calls.load(Ordering::Relaxed),
			0,
			"island never queued"
		);
		assert_eq!(value, NodeValue::None);
	}

	#[test]
	fn bfs_waits_for_every_input_of_a_converging_node() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (a, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(1.0));
		let (b, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(2.0));
		let (merge, _) = add_node(
			&mut graph,
			&[("a_in", ValueType::Float), ("b_in", ValueType::Float)],
			Emit::Float(3.0),
		);
		graph.connect(input, a, "tex_in", -1).expect("input -> a");
		graph.connect(input, b, "tex_in", -1).expect("input -> b");
		graph.connect(a, merge, "a_in", -1).expect("a -> merge");
		graph.connect(b, merge, "b_in", -1).expect("b -> merge");
		graph
			.connect(merge, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("merge -> output");

		let mut probe = Probe::new();
		Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("the converging graph is orderable");
		assert_eq!(probe.order, vec![input, a, b, merge, output]);
		let row = probe.rows.get(&merge).expect("merge resolved");
		assert_eq!(row.get("a_in"), Some(&NodeValue::Float(1.0)));
		assert_eq!(row.get("b_in"), Some(&NodeValue::Float(2.0)));
	}

	#[test]
	fn bfs_evaluates_a_shared_source_once() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (source, source_calls) = add_node(
			&mut graph,
			&[("tex_in", ValueType::Texture)],
			Emit::Float(7.0),
		);
		let (m, _) = add_node(&mut graph, &[("a_in", ValueType::Float)], Emit::Float(1.0));
		let (n, _) = add_node(&mut graph, &[("a_in", ValueType::Float)], Emit::Float(2.0));
		let (f, _) = add_node(
			&mut graph,
			&[("c_in", ValueType::Float), ("d_in", ValueType::Float)],
			Emit::Float(3.0),
		);
		graph
			.connect(input, source, "tex_in", -1)
			.expect("input -> source");
		graph.connect(source, m, "a_in", -1).expect("source -> m");
		graph.connect(source, n, "a_in", -1).expect("source -> n");
		graph.connect(m, f, "c_in", -1).expect("m -> f");
		graph.connect(n, f, "d_in", -1).expect("n -> f");
		graph
			.connect(f, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("f -> output");

		let mut probe = Probe::new();
		Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("the fan-out graph is orderable");
		assert_eq!(probe.order, vec![input, source, m, n, f, output]);
		assert_eq!(
			source_calls.load(Ordering::Relaxed),
			1,
			"the shared source runs once"
		);
		assert_eq!(
			probe.rows.get(&m).and_then(|r| r.get("a_in")),
			Some(&NodeValue::Float(7.0))
		);
		assert_eq!(
			probe.rows.get(&n).and_then(|r| r.get("a_in")),
			Some(&NodeValue::Float(7.0))
		);
	}

	#[test]
	fn bfs_dequeue_order_is_deterministic() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (a, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(1.0));
		let (b, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(2.0));
		let (c, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(3.0));
		let (merge, _) = add_node(
			&mut graph,
			&[
				("a_in", ValueType::Float),
				("b_in", ValueType::Float),
				("c_in", ValueType::Float),
			],
			Emit::Float(4.0),
		);
		for (from, to, input) in [
			(input, a, "tex_in"),
			(input, b, "tex_in"),
			(input, c, "tex_in"),
			(a, merge, "a_in"),
			(b, merge, "b_in"),
			(c, merge, "c_in"),
			(merge, output, GRAPH_OUTPUT_INPUT),
		] {
			graph
				.connect(from, to, input, -1)
				.unwrap_or_else(|e| panic!("{from:?} -> {to:?}.{input}: {e:?}"));
		}

		let mut first = Probe::new();
		Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut first)
			.expect("the fan-in graph is orderable");
		let mut second = Probe::new();
		Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut second)
			.expect("the fan-in graph is orderable");
		assert_eq!(first.order, vec![input, a, b, c, merge, output]);
		assert_eq!(first.order, second.order, "same graph -> same order");
	}

	#[test]
	fn bfs_pulls_time_shifted_upstreams_through_the_dfs_memo() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (source, source_calls) = add_node(&mut graph, &[], Emit::Time);
		let rows = Arc::new(Mutex::new(Vec::new()));
		let mut core = NodeCore::new();
		core.add_input(Input::new("tex_in", ValueType::Texture, NodeValue::None));
		core.add_input(Input::new("val_in", ValueType::Float, NodeValue::None));
		let delay = graph.add_node(
			core,
			Box::new(Delay {
				rows: Arc::clone(&rows),
			}),
		);
		graph
			.connect(input, delay, "tex_in", -1)
			.expect("input -> delay.tex_in");
		graph
			.connect(source, delay, "val_in", -1)
			.expect("source -> delay.val_in");
		graph
			.connect(delay, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("delay -> output");

		let mut probe = Probe::new();
		let value = Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("an out-of-live-set feeder does not stall the sweep");
		assert_eq!(
			probe.order,
			vec![input, source, delay, output],
			"the far end is pulled during delay's turn, not queued"
		);
		assert_eq!(source_calls.load(Ordering::Relaxed), 1);
		let rows = rows.lock().expect("delay rows");
		assert_eq!(rows.len(), 1);
		// One second late: the sweep pulled the source at time 1, not 0.
		assert_eq!(rows[0].get("val_in"), Some(&NodeValue::Float(1.0)));
		assert_eq!(value, NodeValue::None);
	}

	#[test]
	fn bfs_hands_resolved_tables_to_downstream_nodes() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (a, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(1.0));
		let (b, _) = add_node(&mut graph, &[("tex_in", ValueType::Texture)], Emit::Float(2.0));
		let (merge, _) = add_node(
			&mut graph,
			&[("a_in", ValueType::Float), ("b_in", ValueType::Float)],
			Emit::Float(3.0),
		);
		graph.connect(input, a, "tex_in", -1).expect("input -> a");
		graph.connect(input, b, "tex_in", -1).expect("input -> b");
		graph.connect(a, merge, "a_in", -1).expect("a -> merge");
		graph.connect(b, merge, "b_in", -1).expect("b -> merge");
		graph
			.connect(merge, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("merge -> output");

		let mut probe = Probe::new();
		probe.replace = Some((
			a,
			vec![(ValueType::Float, NodeValue::Float(999.0))],
		));
		Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("the converging graph is orderable");
		let row = probe.rows.get(&merge).expect("merge resolved");
		assert_eq!(
			row.get("a_in"),
			Some(&NodeValue::Float(999.0)),
			"the downstream row carries what resolve() left in a's table"
		);
		assert_eq!(row.get("b_in"), Some(&NodeValue::Float(2.0)));
	}

	#[test]
	fn bfs_reports_a_cycle_and_names_its_nodes() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (a, a_calls) = add_node(&mut graph, &[("in", ValueType::Float)], Emit::Float(1.0));
		let (b, b_calls) = add_node(&mut graph, &[("in", ValueType::Float)], Emit::Float(2.0));
		graph.connect(input, a, "in", -1).expect("input -> a");
		graph.connect(a, b, "in", -1).expect("a -> b");
		graph
			.connect(b, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("b -> output");
		graph.force_connect(b, a, "in", -1);

		let mut probe = Probe::new();
		let error = Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect_err("a cycle must be reported, not ordered");
		let message = match error {
			Error::Failed(message) => message,
			other => panic!("Failed expected, got {other:?}"),
		};
		assert!(message.contains("cycle"), "message names the failure: {message}");
		assert!(
			message.contains(&format!("{a:?}")),
			"message names {a:?}: {message}"
		);
		assert!(
			message.contains(&format!("{b:?}")),
			"message names {b:?}: {message}"
		);
		assert_eq!(probe.order, vec![input], "nothing past the cycle resolves");
		assert_eq!(a_calls.load(Ordering::Relaxed), 0);
		assert_eq!(b_calls.load(Ordering::Relaxed), 0);
	}

	#[test]
	fn bfs_returns_the_output_texture() {
		let mut graph = Graph::new();
		let (input, output) = detached_endpoints(&mut graph);
		let (source, source_calls) = add_node(&mut graph, &[], Emit::Texture);
		graph
			.connect(source, input, GRAPH_INPUT_FEED_INPUT, -1)
			.expect("source -> feed_in");
		graph
			.connect(input, output, GRAPH_OUTPUT_INPUT, -1)
			.expect("input -> output");

		let mut probe = Probe::new();
		let value = Traverser::new()
			.eval_graph_bfs(&graph, Rational::new(0, 1), &mut probe)
			.expect("a fed input -> output graph is orderable");
		match value {
			NodeValue::Texture(handle) => assert!(handle.ctx.is_null()),
			other => panic!("texture expected, got {other:?}"),
		}
		assert_eq!(probe.order, vec![source, input, output]);
		assert_eq!(source_calls.load(Ordering::Relaxed), 1);
	}

	#[test]
	fn bfs_requires_both_endpoints() {
		let mut probe = Probe::new();
		let error = Traverser::new()
			.eval_graph_bfs(&Graph::new(), Rational::new(0, 1), &mut probe)
			.expect_err("an endpointless graph cannot be swept");
		assert_eq!(error, Error::NotFound);
	}
}
