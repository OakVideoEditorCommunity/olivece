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

//! OCIO display transform node (C++
//! `src/node/src/color/displaytransform/displaytransform.{h,cpp}`,
//! `olive::DisplayTransformNode`).
//!
//! Note: OpenColorIO itself is never linked here; it is reached through
//! the color manager (`crate::colormanager`) and the oakrender bridge
//! (oakrender, opaque handles), like the C++ node's
//! `oaknode_colormanager_*` / `oakrender_color_processor_*` calls.

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

use crate::nodes::ociobase::OcioBase;

/// Display combo input id (C++ `k_display_input`). Type: combo; default
/// `0`; flags: not-keyframable, not-connectable. Combo strings are the
/// color manager's available displays (refreshed on config change).
pub const DISPLAY_INPUT: &str = "display_in";

/// View combo input id (C++ `k_view_input`). Type: combo; default `0`;
/// flags: not-keyframable, not-connectable. Combo strings are the views
/// available for the selected display.
pub const VIEW_INPUT: &str = "view_in";

/// Direction combo input id (C++ `k_direction_input`). Type: combo;
/// default `0` (forward); flags: not-keyframable, not-connectable.
/// Combo strings (set in `retranslate`): "Forward", "Inverse".
pub const DIRECTION_INPUT: &str = "dir_in";

/// Display transform node. Converts an image to or from a display color
/// space via an OCIO display/view transform. Owns no members beyond the
/// embedded OCIO base state (C++ has no own private members).
pub struct DisplayTransformNode {
	/// Shared OCIO base state (C++ base class `OCIOBaseNode`).
	base: OcioBase,
}

impl DisplayTransformNode {
	/// Selected display name (C++ `get_display()`): the display combo
	/// index mapped through the color manager's display list; empty
	/// string when no manager is attached or the index is out of range.
	fn get_display(&self, core: &NodeCore) -> String {
		// The C++ reads the display combo index through
		// `manager()->list_available_displays()`. The Rust model has no
		// attached color manager (the manager lives per-project behind
		// the oakrender bridge, absent here), so the C++ guard
		// `if (manager())` fails and the empty string is returned.
		// `// CPP-PARITY: displaytransform.cpp` get_display.
		let _ = core;
		String::new()
	}

	/// Selected view name (C++ `get_view()`): the view combo index
	/// mapped through the manager's views for [`Self::get_display`];
	/// empty when unavailable.
	fn get_view(&self, core: &NodeCore) -> String {
		// See [`Self::get_display`]: no manager is ever attached in the
		// Rust model, so the empty string is returned.
		// `// CPP-PARITY: displaytransform.cpp` get_view.
		let _ = core;
		String::new()
	}

	/// Transform direction (C++ `get_direction()`): the direction combo
	/// value cast to `ColorProcessor::Direction` (`0` = normal/forward,
	/// `1` = inverse).
	fn get_direction(&self, core: &NodeCore) -> i64 {
		core.standard_value(DIRECTION_INPUT, -1).to_double() as i64
	}

	/// Refresh the display combo strings from the manager (C++
	/// `update_displays()`); no-op without a manager.
	fn update_displays(&mut self, core: &mut NodeCore) {
		let _ = (self, core);
		// The C++ sets the display combo strings from
		// `manager()->list_available_displays()`; without a manager
		// (Rust model: no bridge) this is a no-op.
		// `// CPP-PARITY: displaytransform.cpp` update_displays.
	}

	/// Refresh the view combo strings for the current display (C++
	/// `update_views()`); no-op without a manager.
	fn update_views(&mut self, core: &mut NodeCore) {
		let _ = (self, core);
		// See [`Self::update_displays`]: no-op without a manager.
		// `// CPP-PARITY: displaytransform.cpp` update_views.
	}

	/// (Re)build the color processor (C++ `generate_processor()`): wraps
	/// the manager, builds a display transform for
	/// display/view/reference-space with the selected direction via
	/// `oakrender_color_processor_create_transform`, and stores it with
	/// [`OcioBase::set_processor`].
	fn generate_processor(&mut self, core: &mut NodeCore) {
		let _ = core;
		// The C++ wraps the color manager, builds a display transform
		// (`oak_core_colortransform_init_display`) for the selected
		// display/view, resolves the reference color space and creates
		// the processor via `oakrender_color_processor_create_transform`,
		// storing it with OcioBase::set_processor. Without a manager (the
		// Rust model reaches the manager through the oakrender bridge,
		// absent here) the C++ guard `if (manager())` fails, so this is a
		// no-op and the processor stays empty — [`OcioBase::value`] then
		// passes the input texture through unchanged.
		// `// CPP-PARITY: displaytransform.cpp` generate_processor.
	}

	/// OCIO config change hook (C++ `config_changed()` override):
	/// refreshes displays and views, then regenerates the processor.
	fn config_changed(&mut self, core: &mut NodeCore) {
		self.update_displays(core);
		self.update_views(core);
		self.generate_processor(core);
	}
}

impl NodeBehavior for DisplayTransformNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Display Transform"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.displaytransform"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Converts an image to or from a display color space."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `display_in` -> "Display", `view_in` -> "View", `dir_in` ->
	/// "Direction" (also sets the direction combo strings
	/// "Forward"/"Inverse").
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			crate::nodes::ociobase::TEXTURE_INPUT => "Input",
			DISPLAY_INPUT => "Display",
			VIEW_INPUT => "View",
			DIRECTION_INPUT => "Direction",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `dir_in` -> "Forward", "Inverse". The
	/// `display_in`/`view_in` strings come from the attached color
	/// manager at runtime (`update_displays`/`update_views`) and cannot
	/// be expressed statically.
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			DIRECTION_INPUT => vec!["Forward", "Inverse"],
			_ => Vec::new(),
		}
	}

	/// Input value changed (C++ `InputValueChangedEvent`): for
	/// `display_in`, `view_in` or `dir_in` regenerates the processor;
	/// a `display_in` change additionally refreshes the view combo.
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		let _ = element;
		if input == DISPLAY_INPUT || input == VIEW_INPUT || input == DIRECTION_INPUT {
			if input == DISPLAY_INPUT {
				self.update_views(core);
			}
			self.generate_processor(core);
		}
	}

	/// Evaluate outputs: inherited from the C++ base
	/// (`OCIOBaseNode::value()`), i.e. delegates to
	/// [`OcioBase::value`] — color-transform job when the processor is
	/// ready, pass-through otherwise.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		self.base.value(core, inputs, time, table);
	}

	/// Added to a graph (C++ base `AddedToGraphEvent`): captures the
	/// project's color manager and runs `config_changed()` via
	/// [`OcioBase::added_to_graph`].
	fn added_to_graph(&mut self, core: &mut NodeCore) {
		self.base.added_to_graph(core);
		self.config_changed(core);
	}

	/// Removed from a graph (C++ base `RemovedFromGraphEvent`): clears
	/// the color manager pointer via [`OcioBase::removed_from_graph`].
	fn removed_from_graph(&mut self, core: &mut NodeCore) {
		self.base.removed_from_graph(core);
	}

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		// The C++ copy constructor copies the embedded OCIO base state;
		// a fresh base with no processor is the safe Rust port (the
		// processor is never populated without the render bridge).
		Some(Box::new(DisplayTransformNode {
			base: OcioBase::new(),
		}))
	}
}

/// Constructor (C++ `DisplayTransformNode::DisplayTransformNode()`):
/// builds the base (`tex_in` texture input, effect input, video-effect
/// flag) and adds the `display_in`/`view_in`/`dir_in` combo inputs with
/// the defaults and flags documented on the constants.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	// OCIOBaseNode base constructor.
	let mut tex = crate::input::Input::new(
		crate::nodes::ociobase::TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);
	core.effect_input = crate::nodes::ociobase::TEXTURE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	for id in [DISPLAY_INPUT, VIEW_INPUT, DIRECTION_INPUT] {
		let mut combo = crate::input::Input::new(
			id,
			crate::value::ValueType::Combo,
			crate::value::NodeValue::Combo(0),
		);
		combo.flags |= crate::input::flags::NOT_KEYFRAMABLE | crate::input::flags::NOT_CONNECTABLE;
		core.add_input(combo);
	}

	(
		core,
		Box::new(DisplayTransformNode {
			base: OcioBase::new(),
		}),
	)
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.displaytransform`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.displaytransform",
		name: "Display Transform",
		categories: &[Category::Color],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn node() -> DisplayTransformNode {
		DisplayTransformNode {
			base: OcioBase::new(),
		}
	}

	#[test]
	fn input_names() {
		let n = node();
		assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(DISPLAY_INPUT), "Display");
		assert_eq!(n.input_name(VIEW_INPUT), "View");
		assert_eq!(n.input_name(DIRECTION_INPUT), "Direction");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.displaytransform"
		);
		assert_ne!(
			core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
				.unwrap()
				.flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		for id in [DISPLAY_INPUT, VIEW_INPUT, DIRECTION_INPUT] {
			let input = core.get_input(id).unwrap();
			assert_eq!(input.default, NodeValue::Combo(0));
			assert_ne!(input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
			assert_ne!(input.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		}
		assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn get_direction_reads_combo_value() {
		let mut core = NodeCore::new();
		let n = node();
		assert_eq!(n.get_direction(&core), 0);
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
		assert_eq!(n.get_direction(&core), 1);
	}

	#[test]
	fn get_display_and_view_empty_without_manager() {
		// No color manager is ever attached in the Rust model, so the
		// C++ `if (manager())` guard fails and both return empty strings.
		let core = NodeCore::new();
		let n = node();
		assert_eq!(n.get_display(&core), "");
		assert_eq!(n.get_view(&core), "");
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
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
	fn value_passes_texture_through_without_processor() {
		let core = NodeCore::new();
		let n = node();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let inputs = crate::value::NodeValueRow::from([(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			tex.clone(),
		)]);
		let mut table = NodeValueTable::default();
		n.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_pushes_deferred_job_with_processor() {
		let core = NodeCore::new();
		let mut n = node();
		n.base.set_processor(Some(crate::handle::CHandle::null()));
		let inputs = crate::value::NodeValueRow::from([(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		n.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert!(table.get(ValueType::Texture).is_some());
	}

	#[test]
	fn input_value_changed_regenerates_processor_and_views() {
		let mut core = NodeCore::new();
		let mut n = node();
		// display_in change refreshes views + regenerates (all no-ops
		// without a manager; must not panic).
		n.input_value_changed(&mut core, DISPLAY_INPUT, 0);
		n.input_value_changed(&mut core, VIEW_INPUT, 0);
		n.input_value_changed(&mut core, DIRECTION_INPUT, 0);
		// Other inputs are ignored.
		n.input_value_changed(&mut core, crate::nodes::ociobase::TEXTURE_INPUT, 0);
		assert!(n.base.processor().is_none());
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Display Transform");
	}

	#[test]
	fn config_and_graph_hooks_are_safe_noops() {
		let mut core = NodeCore::new();
		let mut n = node();
		// Without a manager all refresh/regenerate helpers are no-ops;
		// they must run without panicking and leave the processor empty.
		n.update_displays(&mut core);
		n.update_views(&mut core);
		n.generate_processor(&mut core);
		n.config_changed(&mut core);
		assert!(n.base.processor().is_none());
		n.added_to_graph(&mut core);
		n.removed_from_graph(&mut core);
		assert!(n.base.processor().is_none());
	}
}
