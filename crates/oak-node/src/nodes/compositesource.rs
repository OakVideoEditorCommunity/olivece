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

//! Composite source (Rust-only render seam; no C++ counterpart).
//!
//! The graph renderer builds one of these per adjustment-layer flush: it
//! prefills `tex_in` with the composite of the tracks below the layer
//! (a standard value, so nothing has to be wired upstream) and connects
//! the node to the head of the layer's effect chain for the duration of
//! one [`oak_node::traverser::Traverser::evaluate`] pass. `value()`
//! pushes that texture straight into the output table — the effect chain
//! downstream sees an ordinary texture input, so nested shader jobs,
//! plugin jobs and the pass-through case all work unchanged.
//!
//! It is hidden from the create menu (users never place one; the renderer
//! owns its lifetime — see `oakrender::eval::render_graph_frame`) and has
//! no shader of its own. It is deliberately absent from the factory table:
//! `factory_smoke_test` pins that table to the C++ registration order, and
//! this type has no C++ counterpart — the renderer reaches it through
//! [`create`] directly.

use crate::node::{Category, NodeBehavior, NodeCore};

/// Stable type id (`org.olivevideoeditor.Olive.composite_source`).
pub const TYPE_ID: &str = "org.olivevideoeditor.Olive.composite_source";

/// Texture input id: the prefilled source texture (same id every effect
/// node uses, so it can feed an effect chain's head input directly).
pub const TEXTURE_INPUT: &str = "tex_in";

/// The texture source node. Has no member fields.
pub struct CompositeSource;

impl NodeBehavior for CompositeSource {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Composite Source"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		TYPE_ID
	}

	/// Categories (never used for menus — the node carries
	/// `DONT_SHOW_IN_CREATE_MENU`).
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description.
	fn description(&self) -> &str {
		"Supplies a renderer-composed texture to an effect chain."
	}

	/// Localized input names: `tex_in` -> "Texture".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			_ => id,
		}
	}

	/// Evaluate outputs: push the prefilled texture. The render driver
	/// sets it as the standard value of the (unconnected) `tex_in`, which
	/// the traverser's row builder hands over as an ordinary texture row
	/// entry; a bare node (no prefill) yields nothing.
	fn value(
		&self,
		_core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		_time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let Some(value) = inputs.get(TEXTURE_INPUT) else {
			return;
		};
		// `NodeValue::clone` addrefs the texture handle so the table owns
		// its own reference (released on drop); a plain handle copy would
		// double-release the row's reference.
		table.push(crate::value::ValueType::Texture, value.clone(), None);
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(CompositeSource))
	}
}

/// Constructor: one connectable, non-keyframable `tex_in` texture input
/// (default `None` — the renderer always prefills it) and the
/// create-menu hiding flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();
	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);
	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;
	core.effect_input = TEXTURE_INPUT.to_string();
	(core, Box::new(CompositeSource))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::input::flags::NOT_KEYFRAMABLE;
	use crate::node::flags::DONT_SHOW_IN_CREATE_MENU;
	use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), TYPE_ID);
		assert_eq!(behavior.name(), "Composite Source");
		let tex = core.get_input(TEXTURE_INPUT).expect("tex_in");
		assert_eq!(tex.value_type, ValueType::Texture);
		assert_eq!(tex.default, NodeValue::None);
		assert_eq!(tex.flags & NOT_KEYFRAMABLE, NOT_KEYFRAMABLE);
		assert_eq!(
			core.flags & DONT_SHOW_IN_CREATE_MENU,
			DONT_SHOW_IN_CREATE_MENU
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn input_names() {
		let n = CompositeSource;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
	}

	#[test]
	fn value_pushes_row_texture() {
		let (core, behavior) = create();
		let texture = oak_core::texture::Texture::dummy();
		let inputs = NodeValueRow::from([(
			TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::make_owned(texture)),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, oak_core::Rational::new(0, 1), &mut table);
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(_))
		));
	}

	#[test]
	fn value_without_texture_pushes_nothing() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&NodeValueRow::new(),
			oak_core::Rational::new(0, 1),
			&mut table,
		);
		assert!(table.get(ValueType::Texture).is_none());
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.type_id(), TYPE_ID);
	}
}
