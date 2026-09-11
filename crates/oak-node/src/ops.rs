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

//! Free functions replacing the C++ `Node` static methods
//! (COVERAGE.md §6/§9).

use oak_core::TimeRange;
use oak_undo::undocommand::UndoCommand;

use crate::graph::Graph;
use crate::id::NodeId;
use crate::node::Category;

/// Human-readable category name (C++ `Node::get_category_name`).
pub fn category_name(c: Category) -> &'static str {
	match c {
		Category::Output => "Output",
		Category::Effect => "Effect",
		Category::Generator => "Generator",
		Category::Input => "Input",
		Category::Math => "Math",
		Category::Color => "Color",
		Category::Distort => "Distort",
		Category::Filter => "Filter",
		Category::Keying => "Keying",
		Category::OpenFx => "OpenFX",
		Category::Timeline => "Timeline",
		Category::Group => "Group",
	}
}

/// Copy inputs (and optionally connections) between two nodes
/// (C++ `Node::copy_inputs` / `copy_input` /
/// `copy_values_of_element`).
pub fn copy_inputs(
	graph: &mut Graph,
	src: NodeId,
	dst: NodeId,
	include_connections: bool,
) -> crate::error::Result<()> {
	use crate::error::Error;
	if graph.get(src).is_none() || graph.get(dst).is_none() {
		return Err(Error::NotFound);
	}
	let src_inputs: Vec<String> = graph
		.get(src)
		.map(|e| e.core.inputs.iter().map(|i| i.id.clone()).collect())
		.ok_or(Error::NotFound)?;

	// Copy every declared input's standard value (whole-value semantics;
	// C++ copies per-element too — array elements are covered when the
	// array family lands).
	for id in &src_inputs {
		if !graph
			.get(dst)
			.map(|e| e.core.has_input(id))
			.unwrap_or(false)
		{
			continue;
		}
		let value = {
			let entry = graph.get(src).ok_or(Error::NotFound)?;
			entry.core.standard_value(id, -1)
		};
		let declared = {
			let entry = graph.get(src).ok_or(Error::NotFound)?;
			entry
				.core
				.input_data_type(id)
				.unwrap_or(crate::value::ValueType::None)
		};
		let value = {
			// Re-quantize to the destination's declared type so the copy
			// never stores a mismatched payload.
			let entry = graph.get(dst).ok_or(Error::NotFound)?;
			let dst_declared = entry.core.input_data_type(id).ok_or(Error::NotFound)?;
			if dst_declared == declared {
				value
			} else {
				crate::value::NodeValue::with_scalar(&value, dst_declared, value.to_double())
			}
		};
		let entry = graph.get_mut(dst).ok_or(Error::NotFound)?;
		entry.core.set_standard_value(id, -1, value);
	}

	if include_connections {
		// Recreate src's scalar input connections on dst.
		for id in &src_inputs {
			if let Some(from) = graph.connected_output(src, id, -1) {
				graph.connect(from, dst, id, -1).ok();
			}
		}
	}
	Ok(())
}

/// Copy a node with its upstream dependency subgraph
/// (C++ `copy_dependency_graph` / `copy_node_in_graph` /
/// `copy_node_and_dependency_graph_minus_items`). Returns the new
/// node ids (source order). Undo packaging happens at the caller via
/// oakundo's `UndoCommand`.
pub fn copy_subgraph(
	_graph: &mut Graph,
	_nodes: &[NodeId],
	_exclude_items: bool,
) -> crate::error::Result<Vec<NodeId>> {
	todo!()
}

/// Transform a time range from one node's frame of reference to
/// another's along the connection path (C++ `Node::transform_time_to`).
pub fn transform_time_to(
	_graph: &Graph,
	_time: TimeRange,
	_from: NodeId,
	_to: NodeId,
) -> crate::error::Result<TimeRange> {
	todo!()
}

/// Undo-command display strings (C++
/// `get_connect_command_string`/`get_disconnect_command_string`).
pub fn connect_command_string(_output: NodeId, _input: NodeId, _input_id: &str) -> String {
	todo!()
}

/// See [`connect_command_string`].
pub fn disconnect_command_string(_output: NodeId, _input: NodeId, _input_id: &str) -> String {
	todo!()
}

/// Lock a project mutex (poison-tolerant).
fn lock_any<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Build an un-executed [`UndoCommand`] from redo/undo closures (the
/// direct-Rust replacement of the former oakundo bridge
/// `command_from_closures`).
fn command_from_closures(
	redo: impl FnMut() + Send + 'static,
	undo: impl FnMut() + Send + 'static,
) -> UndoCommand {
	UndoCommand::from_closures(redo, undo)
}

/// Set a keyframed/standard value at a time, returning an un-executed
/// undo command (C++ `Node::set_value_at_time` static; command creation
/// via oakundo's `UndoCommand`). The mutation is chosen from the current
/// state at creation time, like the C++ (`// CPP-PARITY: node.cpp:1782`):
/// - keyframing input with a key at `time` -> replace the key's value;
/// - keyframing input without a key -> insert a key (best type =
///   closest key's type, default Linear);
/// - non-keyframing input -> set the standard value.
///
/// `project` is the project owning `graph`; the returned command's
/// closures lock it on redo/undo.
pub fn set_value_at_time_command(
	project: &std::sync::Arc<std::sync::Mutex<crate::project::Project>>,
	graph: &Graph,
	node: NodeId,
	input: &str,
	element: i32,
	time: oak_core::Rational,
	value: &crate::value::NodeValue,
) -> crate::error::Result<UndoCommand> {
	use crate::error::Error;
	use crate::keyframe::{Interpolation, Keyframe};

	// Determine the mutation from the current state. The declared type is
	// validated but not otherwise used yet.
	let _declared = graph
		.get(node)
		.and_then(|e| e.core.input_data_type(input))
		.ok_or(Error::NotFound)?;
	let keyframing = graph
		.get(node)
		.and_then(|e| e.core.keyframe_track(input, element))
		.map(|t| !t.keys().is_empty())
		.unwrap_or(false);

	let project = project.clone();
	let node = node;
	let input = input.to_string();
	let value = value.clone();
	let element = element;

	if keyframing {
		let existing = graph
			.get(node)
			.and_then(|e| e.core.keyframe_track(&input, element))
			.and_then(|t| {
				t.keys()
					.iter()
					.find(|k| k.time == time)
					.map(|k| (k.time, k.value.clone(), k.interpolation))
			});
		match existing {
			Some((key_time, old_value, _interp)) => {
				// Replace the key's value (preserving type/handles).
				let project_redo = project.clone();
				let project_undo = project.clone();
				Ok(command_from_closures(
					{
						let value = value.clone();
						let input = input.clone();
						let project = project_redo;
						move || {
							let mut g = lock_any(&project);
							if let Some(e) = g.graph.get_mut(node) {
								e.core
									.keyframe_track_mut(&input, element)
									.set_key_value(key_time, value.clone());
							}
						}
					},
					{
						let old_value = old_value.clone();
						let input = input.clone();
						let project = project_undo;
						move || {
							let mut g = lock_any(&project);
							if let Some(e) = g.graph.get_mut(node) {
								e.core
									.keyframe_track_mut(&input, element)
									.set_key_value(key_time, old_value.clone());
							}
						}
					},
				))
			}
			None => {
				let input_redo = input.clone();
				let input_undo = input.clone();
				// Insert a new key.
				let interp = graph
					.get(node)
					.and_then(|e| e.core.keyframe_track(&input, element))
					.and_then(|t| {
						t.keys()
							.iter()
							.filter(|k| k.time <= time)
							.next_back()
							.map(|k| k.interpolation)
					})
					.unwrap_or(Interpolation::Linear);
				let project_redo = project.clone();
				let project_undo = project.clone();
				let value_redo = value.clone();
				Ok(command_from_closures(
					move || {
						let mut g = lock_any(&project_redo);
						if let Some(e) = g.graph.get_mut(node) {
							let track = e.core.keyframe_track_mut(&input_redo, element);
							track.set_key(Keyframe {
								time,
								value: value_redo.clone(),
								interpolation: interp,
								bezier_in: (0.0, 0.0),
								bezier_out: (0.0, 0.0),
							});
						}
					},
					move || {
						let mut g = lock_any(&project_undo);
						if let Some(e) = g.graph.get_mut(node) {
							e.core
								.keyframe_track_mut(&input_undo, element)
								.remove_key(time);
						}
					},
				))
			}
		}
	} else {
		// Set the standard value.
		let old = graph
			.get(node)
			.map(|e| e.core.standard_value(&input, element))
			.unwrap_or(crate::value::NodeValue::None);
		let project_redo = project.clone();
		let project_undo = project.clone();
		let input_redo = input.clone();
		let input_undo = input.clone();
		let value_redo = value.clone();
		Ok(command_from_closures(
			move || {
				let mut g = lock_any(&project_redo);
				if let Some(e) = g.graph.get_mut(node) {
					e.core
						.set_standard_value(&input_redo, element, value_redo.clone());
				}
			},
			move || {
				let mut g = lock_any(&project_undo);
				if let Some(e) = g.graph.get_mut(node) {
					e.core.set_standard_value(&input_undo, element, old.clone());
				}
			},
		))
	}
}
