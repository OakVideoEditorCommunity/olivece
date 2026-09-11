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

//! Multi-cam source switcher node (C++
//! `src/node/src/input/multicam/multicamnode.{h,cpp}`,
//! `olive::MultiCamNode`).

use crate::factory::NodeMeta;
use crate::id::NodeId;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::Rational;

/// Current source selector input id (C++ `k_current_input`). Type:
/// combo; default `0`; flags: static (not-connectable +
/// not-keyframable). Its combo-box strings are built in `retranslate()`
/// as `"<index + 1>: <source name>"` entries.
pub const CURRENT_INPUT: &str = "current_in";

/// Sources array input id (C++ `k_sources_input`). Type: none (any);
/// flags: not-keyframable, array; properties: `arraystart = 1`.
pub const SOURCES_INPUT: &str = "sources_in";

/// Sequence input id (C++ `k_sequence_input`). Type: none (node
/// reference to a `Sequence`); flags: not-keyframable. Excluded from
/// rendering via `ignore_inputs_for_rendering()`.
pub const SEQUENCE_INPUT: &str = "sequence_in";

/// Sequence track-type selector input id (C++ `k_sequence_type_input`).
/// Type: combo; default `0` (video); flags: static, hidden (unhidden
/// while a sequence is connected). Combo strings: `"Video"`, `"Audio"`
/// (C++ `Track::Type` values).
pub const SEQUENCE_TYPE_INPUT: &str = "sequence_type_in";

/// Multi-cam node. Switches between multiple sources either from the
/// `sources_in` array or, when a `Sequence` is connected to
/// `sequence_in`, from that sequence's track list.
pub struct MultiCamNode {
	/// Connected sequence whose track list supplies the sources (C++
	/// `sequence_`, a raw `Sequence *` set/cleared by the
	/// `sequence_in` connect/disconnect events). Stored here as the
	/// sequence node's id; `None` = no sequence connected.
	sequence: Option<NodeId>,
}

/// The C++ `k_input_flag_static` mask: not-connectable +
/// not-keyframable.
const STATIC_FLAGS: u32 =
	crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;

impl MultiCamNode {
	/// Index of the currently selected source (C++
	/// `get_current_source()` — the standard value of
	/// [`CURRENT_INPUT`] as int).
	pub fn current_source(&self, core: &NodeCore) -> i32 {
		core.standard_value(CURRENT_INPUT, -1).to_double() as i32
	}

	/// The connected sequence node (C++ `sequence_`).
	pub fn sequence(&self) -> Option<NodeId> {
		self.sequence
	}

	/// Set/clear the connected-sequence state (the effects of the C++
	/// `InputConnectedEvent`/`InputDisconnectedEvent` on `sequence_in`:
	/// store the sequence and toggle the `sequence_type_in` hidden flag).
	/// The graph arena does not dispatch behavior events on edge edits, so
	/// the commands that edit the `sequence_in` edge call this to keep the
	/// behavior state in sync with the graph.
	pub fn set_sequence(&mut self, core: &mut NodeCore, sequence: Option<NodeId>) {
		if let Some(slot) = core.get_input_mut(SEQUENCE_TYPE_INPUT) {
			if sequence.is_some() {
				slot.flags &= !crate::input::flags::HIDDEN;
			} else {
				slot.flags |= crate::input::flags::HIDDEN;
			}
		}
		self.sequence = sequence;
	}

	/// Number of available sources (C++ `get_source_count()`): the
	/// connected sequence's track count when a sequence is set,
	/// otherwise the `sources_in` array size.
	///
	/// The sequence's track list lives in the sequence node's behavior
	/// (`crate::sequence::SequenceBehavior`) reachable only through the
	/// graph, which this signature does not carry; with a sequence set
	/// the count therefore falls back to the `sources_in` array size
	/// (`// CPP-PARITY: multicamnode.cpp` `get_source_count`).
	pub fn source_count(&self, core: &NodeCore) -> i32 {
		let _ = self;
		core.input_array_size(SOURCES_INPUT) as i32
	}

	/// Grid layout for `sources` sources (C++ static
	/// `get_rows_and_columns(sources, rows, cols)`): starts at 1x1 and
	/// grows the smaller dimension until rows*cols >= sources.
	pub fn rows_and_columns(sources: i32) -> (i32, i32) {
		let mut rows = 1;
		let mut cols = 1;
		while rows * cols < sources {
			if rows < cols {
				rows += 1;
			} else {
				cols += 1;
			}
		}
		(rows, cols)
	}

	/// Grid position of a source index (C++ static
	/// `index_to_row_cols()`): `col = index % total_cols`,
	/// `row = index / total_cols` (`total_rows` is unused in C++).
	pub fn index_to_row_cols(index: i32, total_rows: i32, total_cols: i32) -> (i32, i32) {
		let _ = total_rows;
		(index / total_cols, index % total_cols)
	}

	/// Inverse of [`Self::index_to_row_cols`] (C++ static
	/// `rows_cols_to_index()`): `col + row * total_cols`.
	pub fn rows_cols_to_index(row: i32, col: i32, total_rows: i32, total_cols: i32) -> i32 {
		let _ = total_rows;
		col + row * total_cols
	}

	/// Active `sources_in` element set (C++
	/// `get_active_elements_at_time()` for `sources_in`): only the
	/// element at the current source index, or no elements when the
	/// index is out of range. Core-carrying variant of
	/// [`NodeBehavior::active_elements_at_time`], which receives no
	/// `NodeCore`.
	pub fn active_elements(&self, core: &NodeCore) -> Vec<i32> {
		let src = self.current_source(core);
		if src >= 0 && src < self.source_count(core) {
			vec![src]
		} else {
			Vec::new()
		}
	}
}

impl NodeBehavior for MultiCamNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Multi-Cam"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.multicam"
	}

	/// Categories (C++ `category()` returns `{ k_category_timeline }`).
	fn categories(&self) -> &[Category] {
		&[Category::Timeline]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Allows easy switching between multiple sources."
	}

	/// Localized input names (C++ `retranslate()`): `current_in` ->
	/// "Current", `sources_in` -> "Sources", `sequence_in` ->
	/// "Sequence", `sequence_type_in` -> "Sequence Type". The C++
	/// override also refreshes the combo strings: `sequence_type_in`
	/// gets {"Video", "Audio"} and `current_in` gets per-source
	/// `"<i + 1>: <connected source name>"` entries — that part has no
	/// trait surface and is noted here only.
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			CURRENT_INPUT => "Current",
			SOURCES_INPUT => "Sources",
			SEQUENCE_INPUT => "Sequence",
			SEQUENCE_TYPE_INPUT => "Sequence Type",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `sequence_type_in` -> "Video", "Audio".
	/// `current_in`'s strings are built dynamically per connected source
	/// (`"<i + 1>: <name>"`) and cannot be expressed statically.
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			SEQUENCE_TYPE_INPUT => vec!["Video", "Audio"],
			_ => Vec::new(),
		}
	}

	/// Inputs excluded from rendering (C++
	/// `ignore_inputs_for_rendering()`): always
	/// `{ k_sequence_input }`.
	fn ignore_inputs_for_rendering(&self) -> &[String] {
		static IGNORED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
		std::slice::from_ref(IGNORED.get_or_init(|| SEQUENCE_INPUT.to_string()))
	}

	/// Active array elements (C++ `get_active_elements_at_time()`): for
	/// `sources_in`, only the element at the current source index (or
	/// no elements if the index is out of range); any other input
	/// defers to the base-class behavior.
	///
	/// The C++ reads the `current_in` standard value
	/// (`get_current_source()`), which requires the node's data
	/// ([`NodeCore`]) — not carried by this trait signature. The
	/// core-carrying equivalent is [`Self::active_elements`] (tested
	/// there); until the API gains core access, the base-class (empty)
	/// set is returned (`// CPP-PARITY: multicamnode.cpp`
	/// `get_active_elements_at_time`).
	fn active_elements_at_time(&self, input: &str, time: Rational) -> Vec<i32> {
		let _ = (input, time);
		Vec::new()
	}

	/// Render-time connection resolution (C++
	/// `get_connected_render_output()`): with a sequence connected,
	/// `sources_in` element `i` (in range) resolves to the track at
	/// index `i` of the selected track list instead of any connected
	/// edge; otherwise defers to the base-class behavior (no virtual
	/// output — the base returns `None`).
	///
	/// The track-at-index lookup needs the sequence's track list
	/// (graph-owned, unreachable from this signature), so the
	/// sequence-connected branch resolves to nothing here
	/// (`// CPP-PARITY: multicamnode.cpp` `get_connected_render_output`).
	///
	/// NOTE: the C++ class also overrides
	/// `is_input_connected_for_render()` (reports `sources_in` elements
	/// as connected whenever a sequence is set); the trait has no such
	/// method, so that behavior folds into this one.
	fn connected_render_output(
		&self,
		core: &NodeCore,
		input: &str,
		element: i32,
	) -> Option<NodeId> {
		if self.sequence.is_some() && input == SOURCES_INPUT && element >= 0 {
			let _ = (core, element);
			None
		} else {
			None
		}
	}

	/// Evaluate outputs (C++ `value()`): pushes the ARRAY element of
	/// `sources_in` selected by `current_in` (the current source) — the
	/// traverser stores array elements under `{input}[{element}]` keys
	/// ([`crate::traverser`]), so the selected source's texture is the
	/// value at `sources_in[source]`. A missing element (or an absent
	/// array) pushes nothing.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		let _ = time;
		let source = core.standard_value(CURRENT_INPUT, -1).to_double() as i32;
		let key = if source >= 0 {
			format!("{SOURCES_INPUT}[{source}]")
		} else {
			SOURCES_INPUT.to_string()
		};
		if let Some(v) = inputs.get(&key) {
			table.push(v.value_type(), v.clone(), None);
			return;
		}
		// Fallback: an unindexed source value (single-source wiring from
		// before the element-tagged rows).
		if let Some(v) = inputs.get(SOURCES_INPUT) {
			table.push(v.value_type(), v.clone(), None);
		}
	}

	/// Edge connected (C++ `InputConnectedEvent()`): when a `Sequence`
	/// connects to `sequence_in`, stores it and unhides
	/// `sequence_type_in`.
	fn input_connected(&mut self, core: &mut NodeCore, input: &str, element: i32, source: NodeId) {
		let _ = element;
		if input == SEQUENCE_INPUT {
			// C++ additionally dynamic_casts the source to `Sequence`;
			// the graph type-checks connections at edit time, so the
			// identity is trusted here.
			if let Some(slot) = core.get_input_mut(SEQUENCE_TYPE_INPUT) {
				slot.flags &= !crate::input::flags::HIDDEN;
			}
			self.sequence = Some(source);
		}
	}

	/// Edge disconnected (C++ `InputDisconnectedEvent()`): on
	/// `sequence_in` disconnect, clears the stored sequence and re-hides
	/// `sequence_type_in`.
	fn input_disconnected(
		&mut self,
		core: &mut NodeCore,
		input: &str,
		element: i32,
		source: NodeId,
	) {
		let _ = (element, source);
		if input == SEQUENCE_INPUT {
			if let Some(slot) = core.get_input_mut(SEQUENCE_TYPE_INPUT) {
				slot.flags |= crate::input::flags::HIDDEN;
			}
			self.sequence = None;
		}
	}

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MultiCamNode {
			sequence: self.sequence,
		}))
	}

	/// Downcast to [`Self`] (sequence state access).
	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	/// Mutable downcast (see [`NodeBehavior::as_any`]).
	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

/// Constructor (C++ `MultiCamNode::MultiCamNode()`): adds
/// `current_in`/`sources_in`/`sequence_in`/`sequence_type_in` with the
/// defaults, flags and properties documented on the constants, and
/// initializes `sequence_` to null.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut current = crate::input::Input::new(
		CURRENT_INPUT,
		crate::value::ValueType::Combo,
		NodeValue::Combo(0),
	);
	current.flags |= STATIC_FLAGS;
	core.add_input(current);

	let mut sources = crate::input::Input::new(
		SOURCES_INPUT,
		crate::value::ValueType::None,
		NodeValue::None,
	);
	sources.flags |= crate::input::flags::NOT_KEYFRAMABLE | crate::input::flags::ARRAY;
	sources.properties = vec![("arraystart".to_string(), NodeValue::Int(1))];
	core.add_input(sources);

	let mut sequence = crate::input::Input::new(
		SEQUENCE_INPUT,
		crate::value::ValueType::None,
		NodeValue::None,
	);
	sequence.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(sequence);

	let mut sequence_type = crate::input::Input::new(
		SEQUENCE_TYPE_INPUT,
		crate::value::ValueType::Combo,
		NodeValue::Combo(0),
	);
	sequence_type.flags |= STATIC_FLAGS | crate::input::flags::HIDDEN;
	core.add_input(sequence_type);

	(core, Box::new(MultiCamNode { sequence: None }))
}

/// Register this node type (C++ `k_multicam_node` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.multicam",
		name: "Multi-Cam",
		categories: &[Category::Timeline],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::id::NodeId;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValueTable, ValueType};
	use oak_core::Rational;

	/// A distinct, valid-looking node id for sequence connect tests.
	fn fake_id(n: u32) -> NodeId {
		NodeId::from_identity(n as u64).unwrap()
	}

	#[test]
	fn input_names() {
		let n = MultiCamNode { sequence: None };
		assert_eq!(n.input_name(CURRENT_INPUT), "Current");
		assert_eq!(n.input_name(SOURCES_INPUT), "Sources");
		assert_eq!(n.input_name(SEQUENCE_INPUT), "Sequence");
		assert_eq!(n.input_name(SEQUENCE_TYPE_INPUT), "Sequence Type");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(NodeBehavior::type_id(&*behavior), "org.olivevideoeditor.Olive.multicam");
		assert_eq!(
			core.get_input(CURRENT_INPUT).unwrap().value_type,
			ValueType::Combo
		);
		assert_eq!(
			core.get_input(CURRENT_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		let sources = core.get_input(SOURCES_INPUT).unwrap();
		assert_ne!(sources.flags & crate::input::flags::ARRAY, 0);
		assert_ne!(sources.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		assert!(sources
			.properties
			.iter()
			.any(|(k, v)| k == "arraystart" && v == &NodeValue::Int(1)));
		assert_ne!(
			core.get_input(SEQUENCE_INPUT).unwrap().flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		let seq_type = core.get_input(SEQUENCE_TYPE_INPUT).unwrap();
		assert_ne!(seq_type.flags & crate::input::flags::HIDDEN, 0);
		assert_ne!(seq_type.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_ne!(seq_type.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
	}

	#[test]
	fn current_source_reads_standard_value() {
		let (mut core, _) = create();
		let node = MultiCamNode { sequence: None };
		assert_eq!(node.current_source(&core), 0);
		core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(2));
		assert_eq!(node.current_source(&core), 2);
	}

	#[test]
	fn source_count_is_array_size() {
		let (mut core, _) = create();
		// Grow the sources array to 3 elements.
		core.input_array_insert(SOURCES_INPUT, 0);
		core.input_array_insert(SOURCES_INPUT, 1);
		core.input_array_insert(SOURCES_INPUT, 2);
		let node = MultiCamNode { sequence: None };
		assert_eq!(node.source_count(&core), 3);
	}

	#[test]
	fn rows_and_columns_square_growth() {
		assert_eq!(MultiCamNode::rows_and_columns(0), (1, 1));
		assert_eq!(MultiCamNode::rows_and_columns(1), (1, 1));
		assert_eq!(MultiCamNode::rows_and_columns(2), (1, 2));
		assert_eq!(MultiCamNode::rows_and_columns(3), (2, 2));
		assert_eq!(MultiCamNode::rows_and_columns(4), (2, 2));
		// The smaller dimension grows first: 1x1 -> 1x2 -> 2x2 -> 2x3.
		assert_eq!(MultiCamNode::rows_and_columns(5), (2, 3));
		assert_eq!(MultiCamNode::rows_and_columns(6), (2, 3));
		assert_eq!(MultiCamNode::rows_and_columns(9), (3, 3));
	}

	#[test]
	fn index_row_cols_round_trip() {
		for (i, rows, cols) in [
			(0, 3, 3),
			(1, 3, 3),
			(2, 3, 3),
			(3, 3, 3),
			(8, 3, 3),
			(5, 2, 3),
		] {
			let (r, c) = MultiCamNode::index_to_row_cols(i, rows, cols);
			assert_eq!(r, i / cols);
			assert_eq!(c, i % cols);
			assert_eq!(MultiCamNode::rows_cols_to_index(r, c, rows, cols), i);
		}
	}

	#[test]
	fn active_elements_selects_current_source() {
		let (mut core, _) = create();
		core.input_array_insert(SOURCES_INPUT, 0);
		core.input_array_insert(SOURCES_INPUT, 1);
		core.input_array_insert(SOURCES_INPUT, 2);
		let node = MultiCamNode { sequence: None };
		core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(1));
		assert_eq!(node.active_elements(&core), vec![1]);
		// Out of range -> no active elements.
		core.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(9));
		assert!(node.active_elements(&core).is_empty());
	}

	#[test]
	fn ignore_inputs_always_sequence() {
		let node = MultiCamNode { sequence: None };
		assert_eq!(
			node.ignore_inputs_for_rendering(),
			&[SEQUENCE_INPUT.to_string()]
		);
	}

	#[test]
	fn value_pushes_connected_source_value() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(SOURCES_INPUT.to_string(), NodeValue::Float(7.0));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Float), Some(&NodeValue::Float(7.0)));
	}

	#[test]
	fn value_pushes_nothing_when_sources_empty() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty());
	}

	#[test]
	fn sequence_connect_toggles_hidden_flag() {
		let (mut core, mut behavior) = create();
		assert_ne!(
			core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
			0
		);
		let seq = fake_id(7);
		behavior.input_connected(&mut core, SEQUENCE_INPUT, -1, seq);
		assert_eq!(
			core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
			0
		);
		behavior.input_disconnected(&mut core, SEQUENCE_INPUT, -1, seq);
		assert_ne!(
			core.get_input(SEQUENCE_TYPE_INPUT).unwrap().flags & crate::input::flags::HIDDEN,
			0
		);
	}

	#[test]
	fn duplicate_copies_sequence() {
		let seq = fake_id(7);
		let node = MultiCamNode {
			sequence: Some(seq),
		};
		let copy = node.duplicate(&NodeCore::new()).unwrap();
		assert_eq!(NodeBehavior::type_id(&*copy), "org.olivevideoeditor.Olive.multicam");
		let down = copy
			.as_any()
			.unwrap()
			.downcast_ref::<MultiCamNode>()
			.unwrap();
		assert_eq!(down.sequence, Some(seq));
	}
}
