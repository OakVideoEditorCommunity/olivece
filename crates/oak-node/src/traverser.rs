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

use std::collections::{HashMap, HashSet};

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
		let mut queued: HashSet<(NodeId, Rational)> = HashSet::new();
		let mut stack: Vec<Frame> = vec![Frame::Enter(request.root, request.time)];
		queued.insert((request.root, request.time));

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
					let row = build_row(graph, &cache, entry, node, time);
					let mut table = NodeValueTable::default();
					entry.behavior.value(&entry.core, &row, time, &mut table);
					hooks.resolve(node, &row, &mut table);
					cache.insert((node, time), table);
				}
			}
		}

		Ok(cache
			.remove(&(request.root, request.time))
			.unwrap_or_default())
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
