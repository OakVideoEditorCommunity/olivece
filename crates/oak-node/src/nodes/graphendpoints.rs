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

//! The graph's virtual endpoints: `GraphInput` / `GraphOutput`.
//!
//! Oak-only design with no C++ counterpart: upstream Olive's traversal
//! walks back from the viewer output node and has no endpoint pair (see
//! `docs/zh/plans/render-pipeline-threads.md` §3.8). Every project graph
//! carries exactly one pair, created by `Graph::ensure_endpoints`
//! (default-wired `input -> output`, and only that pair — the graph
//! refuses to remove or copy them). `GraphInput` is the start of the
//! evaluation walk ([`crate::traverser::Traverser::eval_graph_bfs`]);
//! `GraphOutput` is where every branch converges and its value is the
//! frame.
//!
//! `GraphInput`'s `value()` forwards whatever its input row carries —
//! the node itself produces no pixels. `GraphOutput`'s `value()` hands
//! its `tex_in` row value back out as the node's own output, so a
//! consumer reads the frame from the output node's table.
//!
//! Deviation from the plan's literal wording ("only an output port"): the
//! input endpoint also declares one connectable texture input,
//! `feed_in`. Footage and generator nodes have no connectable inputs at
//! all (their media input is `NOT_CONNECTABLE`), so without this port a
//! single-clip graph could not be wired into the walk: the walk's live
//! set is anchored at `GraphInput`, and a source with no edge into that
//! anchor would be pruned. The sequence renderer feeds its composite
//! through the same port. The "only output ports" half of the plan
//! sentence still holds literally: the input endpoint's only output port
//! is `tex_out`.

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

/// Stable type id of the virtual graph input endpoint. Graph model code
/// identifies the endpoint by this id (never by storing a node id).
pub const GRAPH_INPUT_TYPE_ID: &str = "org.olivevideoeditor.Olive.graphinput";

/// Stable type id of the virtual graph output endpoint (`Graph::endpoints`
/// and `Graph::is_endpoint` match on it).
pub const GRAPH_OUTPUT_TYPE_ID: &str = "org.olivevideoeditor.Olive.graphoutput";

/// The input endpoint's texture input id: the graph's raw data entrance.
/// A footage, generator, or sequence-composite node's output feeds the
/// graph through here. Type: texture; flags: not-keyframable (see the
/// module doc for why this port exists at all).
pub const GRAPH_INPUT_FEED_INPUT: &str = "feed_in";

/// The input endpoint's output port id (the evaluation walk's root
/// output; the sequence renderer and the node editor use it as the
/// input node's only outgoing port).
pub const GRAPH_INPUT_OUTPUT: &str = "tex_out";

/// The output endpoint's texture input id: every live branch converges
/// here and the node's own value is this input's value. Type: texture;
/// flags: not-keyframable.
pub const GRAPH_OUTPUT_INPUT: &str = "tex_in";

/// The virtual graph input node. Has no member fields.
pub struct GraphInputNode;

/// The virtual graph output node. Has no member fields.
pub struct GraphOutputNode;

impl NodeBehavior for GraphInputNode {
	/// Human-readable name (shown as the node's fixed title).
	fn name(&self) -> &str {
		"Graph Input"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		GRAPH_INPUT_TYPE_ID
	}

	/// Categories. The pair is never in a create menu (the constructor
	/// sets `DONT_SHOW_IN_CREATE_MENU`); the category only groups it in
	/// listings that ignore that flag.
	fn categories(&self) -> &[Category] {
		&[Category::Input]
	}

	/// Description.
	fn description(&self) -> &str {
		"The graph's input endpoint: the evaluation walk starts here and its row is forwarded downstream."
	}

	/// Localized input names: `feed_in` -> "Feed".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			GRAPH_INPUT_FEED_INPUT => "Feed",
			_ => id,
		}
	}

	/// Forward the input row: every declared input that carries a
	/// non-`None` value is pushed under its declared type, so downstream
	/// nodes see exactly what was fed in. The node generates no pixels
	/// of its own (C++ `Node::value` counterpart absent — Oak-only node).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		_time: oak_core::Rational,
		table: &mut NodeValueTable,
	) {
		for input in &core.inputs {
			let Some(value) = inputs.get(&input.id) else {
				continue;
			};
			if matches!(value, NodeValue::None) {
				continue;
			}
			// `NodeValue::clone` addrefs texture handles so the table owns
			// its own reference (released on drop).
			table.push(input.value_type, value.clone(), None);
		}
	}

	/// The endpoint pair is fixed: copying either node is refused (the
	/// graph model also refuses `remove_node`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		None
	}
}

impl NodeBehavior for GraphOutputNode {
	/// Human-readable name (shown as the node's fixed title).
	fn name(&self) -> &str {
		"Graph Output"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		GRAPH_OUTPUT_TYPE_ID
	}

	/// Categories. See [`GraphInputNode::categories`].
	fn categories(&self) -> &[Category] {
		&[Category::Output]
	}

	/// Description.
	fn description(&self) -> &str {
		"The graph's output endpoint: every live branch converges here; the node's value is the frame."
	}

	/// Localized input names: `tex_in` -> "Texture".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			GRAPH_OUTPUT_INPUT => "Texture",
			_ => id,
		}
	}

	/// Evaluate outputs: push the incoming texture as this node's own
	/// value, so the walk's result is read from the output endpoint's
	/// table (nothing incoming -> nothing pushed). The sibling-branch
	/// convergence itself happens in the walk (Kahn in-degree), not here.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		_time: oak_core::Rational,
		table: &mut NodeValueTable,
	) {
		let Some(value) = inputs.get(GRAPH_OUTPUT_INPUT) else {
			return;
		};
		if matches!(value, NodeValue::None) {
			return;
		}
		let Some(data_type) = core.input_data_type(GRAPH_OUTPUT_INPUT) else {
			return;
		};
		table.push(data_type, value.clone(), None);
	}

	/// The endpoint pair is fixed: see [`GraphInputNode::duplicate`].
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		None
	}
}

/// Constructor: the standard `enabled_in`, one connectable,
/// non-keyframable `feed_in` texture input (default `None` — nothing is
/// fed in until the project wires a source), and the create-menu hiding
/// flag. No effect flags: the node is not addable, not an effect input,
/// and not an item.
pub fn create_graph_input() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();
	let mut feed = crate::input::Input::new(
		GRAPH_INPUT_FEED_INPUT,
		ValueType::Texture,
		NodeValue::None,
	);
	feed.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(feed);
	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;
	(core, Box::new(GraphInputNode))
}

/// Constructor: the standard `enabled_in`, one connectable,
/// non-keyframable `tex_in` texture input (default `None`), and the
/// create-menu hiding flag. See [`create_graph_input`].
pub fn create_graph_output() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();
	let mut tex = crate::input::Input::new(
		GRAPH_OUTPUT_INPUT,
		ValueType::Texture,
		NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);
	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;
	(core, Box::new(GraphOutputNode))
}

/// Register both endpoint types with the factory: the graph model and
/// the project loader construct them by type id, so serialization can
/// rebuild a loaded graph's missing endpoints. Not in the C++ menu
/// order (no C++ counterpart) — the entries are appended after the
/// built-ins, and the hiding flag keeps them out of every create menu.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: GRAPH_INPUT_TYPE_ID,
		name: "Graph Input",
		categories: &[Category::Input],
		create: create_graph_input,
	});
	meta.push(NodeMeta {
		type_id: GRAPH_OUTPUT_TYPE_ID,
		name: "Graph Output",
		categories: &[Category::Output],
		create: create_graph_output,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::factory::Factory;

	#[test]
	fn names_and_type_ids() {
		let (_, input) = create_graph_input();
		assert_eq!(input.name(), "Graph Input");
		assert_eq!(input.type_id(), GRAPH_INPUT_TYPE_ID);
		assert_eq!(input.categories(), &[Category::Input]);
		let (_, output) = create_graph_output();
		assert_eq!(output.name(), "Graph Output");
		assert_eq!(output.type_id(), GRAPH_OUTPUT_TYPE_ID);
		assert_eq!(output.categories(), &[Category::Output]);
	}

	#[test]
	fn create_sets_ports_and_hiding_flag() {
		use crate::input::flags as input_flags;
		use crate::node::flags as node_flags;

		let (core, _) = create_graph_input();
		let feed = core.get_input(GRAPH_INPUT_FEED_INPUT).expect("feed_in");
		assert_eq!(feed.value_type, ValueType::Texture);
		assert_eq!(feed.default, NodeValue::None);
		assert_eq!(feed.flags & input_flags::NOT_KEYFRAMABLE, input_flags::NOT_KEYFRAMABLE);
		assert!(feed.is_connectable(), "the data entrance accepts edges");
		assert!(core.get_input(crate::node::ENABLED_INPUT).is_some());
		assert_ne!(core.flags & node_flags::DONT_SHOW_IN_CREATE_MENU, 0);
		// Not an effect, not an item, no effect input.
		assert_eq!(core.flags & node_flags::VIDEO_EFFECT, 0);
		assert_eq!(core.flags & node_flags::AUDIO_EFFECT, 0);
		assert_eq!(core.flags & node_flags::IS_ITEM, 0);
		assert!(core.effect_input.is_empty());

		let (core, _) = create_graph_output();
		let tex = core.get_input(GRAPH_OUTPUT_INPUT).expect("tex_in");
		assert_eq!(tex.value_type, ValueType::Texture);
		assert_eq!(tex.default, NodeValue::None);
		assert!(tex.is_connectable());
		assert_ne!(core.flags & node_flags::DONT_SHOW_IN_CREATE_MENU, 0);
		assert_eq!(core.flags & node_flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn duplicate_is_refused() {
		let (core, input) = create_graph_input();
		assert!(input.duplicate(&core).is_none());
		let (core, output) = create_graph_output();
		assert!(output.duplicate(&core).is_none());
	}

	#[test]
	fn input_names_and_enabled_fallthrough() {
		let (_, input) = create_graph_input();
		assert_eq!(input.input_name(GRAPH_INPUT_FEED_INPUT), "Feed");
		// The raw id (not the default "Enabled") so no pack entry is
		// required for the structural input (the param view hides it).
		assert_eq!(input.input_name(crate::node::ENABLED_INPUT), "enabled_in");
		let (_, output) = create_graph_output();
		assert_eq!(output.input_name(GRAPH_OUTPUT_INPUT), "Texture");
		assert_eq!(output.input_name("other_in"), "other_in");
	}

	#[test]
	fn factory_resolves_both_types() {
		let (core, behavior) = Factory::global()
			.create_any(GRAPH_INPUT_TYPE_ID)
			.expect("the factory registers the graph input endpoint");
		assert_eq!(
			core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU,
			crate::node::flags::DONT_SHOW_IN_CREATE_MENU
		);
		assert_eq!(behavior.type_id(), GRAPH_INPUT_TYPE_ID);
		let (core, behavior) = Factory::global()
			.create_any(GRAPH_OUTPUT_TYPE_ID)
			.expect("the factory registers the graph output endpoint");
		assert_eq!(
			core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU,
			crate::node::flags::DONT_SHOW_IN_CREATE_MENU
		);
		assert_eq!(behavior.type_id(), GRAPH_OUTPUT_TYPE_ID);
	}

	#[test]
	fn graph_input_forwards_the_row() {
		let (core, behavior) = create_graph_input();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let row = NodeValueRow::from([
			(crate::node::ENABLED_INPUT.to_string(), NodeValue::Boolean(true)),
			(GRAPH_INPUT_FEED_INPUT.to_string(), tex),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, oak_core::Rational::new(0, 1), &mut table);
		assert_eq!(table.count(), 2);
		assert_eq!(table.get(ValueType::Boolean), Some(&NodeValue::Boolean(true)));
		assert!(matches!(table.get(ValueType::Texture), Some(NodeValue::Texture(_))));
	}

	#[test]
	fn graph_input_skips_a_missing_or_none_row_value() {
		let (core, behavior) = create_graph_input();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&NodeValueRow::default(),
			oak_core::Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty(), "no row entries -> nothing forwarded");

		let row = NodeValueRow::from([(
			GRAPH_INPUT_FEED_INPUT.to_string(),
			NodeValue::None,
		)]);
		behavior.value(&core, &row, oak_core::Rational::new(0, 1), &mut table);
		assert!(table.is_empty(), "a None value is not forwarded");
	}

	#[test]
	fn graph_output_pushes_its_texture() {
		let (core, behavior) = create_graph_output();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let row = NodeValueRow::from([(GRAPH_OUTPUT_INPUT.to_string(), tex)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, oak_core::Rational::new(0, 1), &mut table);
		let handle = match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => *h,
			other => panic!("texture expected, got {other:?}"),
		};
		assert!(handle.ctx.is_null());

		// Nothing incoming -> nothing pushed.
		let mut empty = NodeValueTable::default();
		behavior.value(
			&core,
			&NodeValueRow::default(),
			oak_core::Rational::new(0, 1),
			&mut empty,
		);
		assert!(empty.is_empty());
	}
}
