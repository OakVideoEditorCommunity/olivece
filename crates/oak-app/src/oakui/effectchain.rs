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

//! The effect-chain composition (M14 R3).
//!
//! The facade's `oakengine_node_effect_*` exports (chain walk, undoable
//! insert / remove / reorder / enable) were never sunk into a module —
//! they are composition over the oaknode graph and the oakundo global
//! stack, so they live in the app now. The semantics mirror
//! `crates/oakengine/src/node.rs` (`effect_chain`, `node_effect_insert_impl`,
//! `oakengine_node_effect_remove` / `..._move` / `..._set_enabled`), rebuilt
//! over the direct graph API: chain nodes are `NodeId`s in the host's own
//! project, and the undoable edits are one stack entry each (a multi
//! command or a closure command from [`oak_undo::undocommand::UndoCommand`]).

use std::sync::{Arc, Mutex};

use oak_node::graph::{Graph, NodeEntry};
use oak_node::id::NodeId;
use oak_node::node::ENABLED_INPUT;
use oak_node::value::NodeValue;
use oak_undo::undocommand::UndoCommand;

use super::graphops::{connect_command, disconnect_command, lock, push_command as push, ProjectRef};

/// The effect-input id of `node`, or `None` when the node cannot host
/// effects (C++ `Node::GetEffectInputID`).
pub fn effect_input_of(g: &Graph, node: NodeId) -> Option<String> {
	let id = &g.get(node)?.core.effect_input;
	if id.is_empty() {
		None
	} else {
		Some(id.clone())
	}
}

/// The node feeding `node`'s input `input_id`, or `None` when the input
/// is unconnected.
fn connected_node(g: &Graph, node: NodeId, input_id: &str) -> Option<NodeId> {
	g.connected_output(node, input_id, -1)
}

/// The effect chain of `host`, **closest-to-source first** (signal order:
/// the first element feeds the media side, the last feeds `host`'s effect
/// input). The walk follows each node's effect input upstream until an
/// unconnected input or a node without an effect input; a `seen` guard
/// protects against malformed cycles.
pub fn chain(g: &Graph, host: NodeId) -> Vec<NodeId> {
	let mut chain = Vec::new();
	let mut cur = host;
	let mut seen: Vec<NodeId> = Vec::new();
	loop {
		if seen.contains(&cur) {
			break;
		}
		seen.push(cur);
		let Some(input) = effect_input_of(g, cur) else {
			break;
		};
		let Some(up) = connected_node(g, cur, &input) else {
			break;
		};
		chain.push(up);
		cur = up;
	}
	chain.reverse();
	chain
}

/// Whether `node` is enabled (the `enabled_in` standard value; the effect
/// stack's enable toggle).
pub fn is_enabled(g: &Graph, node: NodeId) -> bool {
	g.get(node)
		.map(|e| {
			matches!(
				e.core.standard_value(ENABLED_INPUT, -1),
				NodeValue::Boolean(true)
			)
		})
		.unwrap_or(false)
}

/// The built-in effect group keys, in presentation order.
///
/// The effect library and the inspector's add menu head the built-in
/// effects with these groups, in this order; `general` collects every
/// category without a group of its own and sorts last. The keys are
/// **language-pack keys**, never display strings: a header's label is
/// [`group_label`]'s `effect_library.group.<key>` lookup.
pub const GROUP_KEYS: &[&str] = &[
	"color",
	"filter",
	"distort",
	"keying",
	"generator",
	"math",
	"time",
	"channel",
	"transition",
	"general",
];

/// The group key of a node [`Category`](oak_node::node::Category).
///
/// The category enum mirrors the C++ `Node::CategoryID`, which is finer
/// grained than the create menu: outputs, plain effects, inputs, OpenFX
/// plugins, timeline and group nodes have no group of their own and land
/// in `general`. `time` / `channel` / `transition` are part of the key
/// set (the language packs carry their labels) but no category maps to
/// them yet.
pub fn category_group_key(category: oak_node::node::Category) -> &'static str {
	use oak_node::node::Category;
	match category {
		Category::Color => "color",
		Category::Filter => "filter",
		Category::Distort => "distort",
		Category::Keying => "keying",
		Category::Generator => "generator",
		Category::Math => "math",
		_ => "general",
	}
}

/// The display label of an effect-group key: the localized
/// `effect_library.group.<key>` string for the built-in keys, and the key
/// itself for OpenFX sub-categories (their labels come from the plugin
/// descriptor, not from the language packs).
pub fn group_label(group_key: &str) -> String {
	match group_key {
		"color" => crate::i18n::tr("effect_library.group.color"),
		"filter" => crate::i18n::tr("effect_library.group.filter"),
		"distort" => crate::i18n::tr("effect_library.group.distort"),
		"keying" => crate::i18n::tr("effect_library.group.keying"),
		"generator" => crate::i18n::tr("effect_library.group.generator"),
		"math" => crate::i18n::tr("effect_library.group.math"),
		"time" => crate::i18n::tr("effect_library.group.time"),
		"channel" => crate::i18n::tr("effect_library.group.channel"),
		"transition" => crate::i18n::tr("effect_library.group.transition"),
		"general" => crate::i18n::tr("effect_library.group.general"),
		// OpenFX sub-categories (and any key a future pack ships without a
		// label): shown verbatim rather than as a raw pack key.
		other => other,
	}
	.to_string()
}

/// The language-pack key of a built-in node's display name, built at
/// runtime: `node.<type-id suffix>.name`, the suffix being the type id's
/// last dotted segment (`org.olivevideoeditor.Olive.colorcorrect` →
/// `colorcorrect`).
fn node_name_key(type_id: &str) -> String {
	format!("node.{}.name", type_suffix(type_id))
}

/// The language-pack key of a built-in node input's display name:
/// `node.<type-id suffix>.input.<input id>`.
fn input_name_key(type_id: &str, input_id: &str) -> String {
	format!("node.{}.input.{input_id}", type_suffix(type_id))
}

/// The last dotted segment of a node type id — the language-pack key's
/// `<type-id suffix>`.
fn type_suffix(type_id: &str) -> &str {
	type_id.rsplit('.').next().unwrap_or(type_id)
}

/// The effect types the user can add to a clip's chain — the built-in
/// factory entries flagged `video_effect` and not hidden from the create
/// menu (grouped by their first category, see [`category_group_key`]),
/// plus every runtime-registered OpenFX plugin entry (grouped by its
/// sub-category: Filter / Generator / Transition / General — the C++
/// `factorymenu.cpp` OpenFX branch).
pub fn addable_effects() -> Vec<super::engine::EffectEntry> {
	use super::engine::EffectEntry;
	use oak_node::node::flags as node_flags;
	let mut out = Vec::new();
	for meta in oak_node::factory::Factory::global().entries() {
		// A scratch instance per entry just to read its flags (the factory
		// metadata carries no flag copy).
		let (core, _behavior) = (meta.create)();
		let flags = core.flags;
		if flags & node_flags::VIDEO_EFFECT != 0
			&& flags & node_flags::DONT_SHOW_IN_CREATE_MENU == 0
		{
			let name = if meta.name.is_empty() {
				meta.type_id.to_string()
			} else {
				meta.name.to_string()
			};
			// The library shows the pack's `node.<type-id suffix>.name`; a
			// type the packs don't know keeps the node's own English name.
			let name = crate::i18n::tr_or(&node_name_key(meta.type_id), &name);
			let group = meta
				.categories
				.first()
				.map(|category| category_group_key(*category))
				.unwrap_or("general");
			out.push(EffectEntry {
				type_id: meta.type_id.to_string(),
				name,
				group: Some(group.to_string()),
			});
		}
	}
	for meta in oak_node::factory::Factory::global().dynamic_entries() {
		// OpenFX plugin entries: grouped by sub-category. The factory
		// metadata carries no flags; plugin nodes are always video effects.
		let name = if meta.name.is_empty() {
			meta.type_id.clone()
		} else {
			meta.name
		};
		out.push(EffectEntry {
			type_id: meta.type_id,
			name,
			group: Some(meta.sub_category),
		});
	}
	// Default presentation order: built-in groups first, in GROUP_KEYS
	// order (`general` last), then the OpenFX sub-category groups
	// alphabetically; names alphabetical (folded) within each group.
	out.sort_by(|a, b| {
		let ga = a.group.as_deref().unwrap_or("");
		let gb = b.group.as_deref().unwrap_or("");
		match (group_rank(ga), group_rank(gb)) {
			(Some(ra), Some(rb)) => ra.cmp(&rb),
			(Some(_), None) => std::cmp::Ordering::Less,
			(None, Some(_)) => std::cmp::Ordering::Greater,
			(None, None) => ga.cmp(gb),
		}
		.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
	});
	out
}

/// The presentation rank of a built-in group key, or `None` for a key
/// outside [`GROUP_KEYS`] (an OpenFX sub-category, which the sort orders
/// after every built-in group).
fn group_rank(group: &str) -> Option<usize> {
	GROUP_KEYS.iter().position(|key| *key == group)
}

// ---------------------------------------------------------------------------
// OFX parameter data model (stage 6b)
// ---------------------------------------------------------------------------

/// The OFX plugin instance handle of a plugin node (the oakplugin registry
/// key), or `None` for built-in nodes. Used by the inspector to read the
/// persistent-message badge and to trigger push buttons.
pub fn plugin_instance_handle(g: &Graph, node: NodeId) -> Option<u64> {
	let behavior = g.get(node)?.behavior.as_any()?;
	let plugin = behavior.downcast_ref::<oak_node::nodes::plugin::PluginNode>()?;
	let handle = plugin.instance_handle();
	(!handle.is_null()).then_some(handle.0)
}

/// The parameter controls of `effect` for the inspector, or `None` when
/// the effect exposes no parameter UI. Any effect node — built-in or OFX
/// plugin — exposes its inputs as parameters (C++ parity: the parameter
/// editor lists every non-hidden, non-connection input); connection/data
/// inputs (texture / samples / matrix) and the structural enabled input
/// are excluded.
pub fn effect_params(
	g: &Graph,
	node: NodeId,
) -> Option<Vec<super::engine::EffectParam>> {
	use oak_node::input::flags as input_flags;
	use oak_node::value::ValueType;

	let entry = g.get(node)?;
	// The pack keys describe the built-in nodes only: an OpenFX plugin's
	// labels come from the plugin (and the OFX translation pass), and a
	// plugin type id happens to end in a built-in type's suffix, the
	// built-in name would otherwise leak into it.
	let builtin = oak_node::factory::Factory::global()
		.find(entry.behavior.type_id())
		.is_some();
	let mut out = Vec::new();
	for input in &entry.core.inputs {
		// Clip/texture/sample/matrix inputs are graph connections or
		// internal data, not params; hidden (secret) inputs never render.
		if matches!(
			input.value_type,
			ValueType::Texture | ValueType::Samples | ValueType::Matrix
		) {
			continue;
		}
		if input.flags & input_flags::HIDDEN != 0 {
			continue;
		}
		// The standard enabled input is structural, not a parameter.
		if input.id == oak_node::node::ENABLED_INPUT {
			continue;
		}
		// Display name: the behavior's localized input name (C++
		// `retranslate`) wins; OFX plugin nodes don't override it, so
		// their translation-pass label (input.display_name) is kept.
		let behavior_name = entry.behavior.input_name(&input.id);
		let display_name = if behavior_name != input.id {
			behavior_name.to_string()
		} else {
			input.display_name.clone()
		};
		// Built-in nodes label their inputs from the pack instead
		// (`node.<type-id suffix>.input.<input id>`), so an input the
		// behavior names in English — or leaves at its raw input id —
		// follows the active language. The behavior's name is the fallback.
		let display_name = if builtin {
			crate::i18n::tr_or(
				&input_name_key(entry.behavior.type_id(), &input.id),
				&display_name,
			)
		} else {
			display_name
		};
		// Combo options: built-in nodes carry them on the behavior (C++
		// `set_combo_box_strings`); OFX plugin params already carry the
		// ("combo_option", _) properties from the translation pass.
		let mut properties = input.properties.clone();
		if input.value_type == ValueType::Combo && !properties.iter().any(|(k, _)| k == "combo_option")
		{
			for option in entry.behavior.input_combo_strings(&input.id) {
				properties.push((
					"combo_option".to_string(),
					oak_node::value::NodeValue::Text(option.to_string()),
				));
			}
		}
		// The v3 text node's `font_family_in` is a str-combo whose options
		// the node layer cannot supply: oak-node deliberately links no font
		// or shaping crate, so the app's text backend enumerates the system
		// families and the facade injects them here (the same
		// `("combo_option", Text)` shape the OFX translation pass uses).
		if input.id == "font_family_in" && !properties.iter().any(|(k, _)| k == "combo_option") {
			for name in super::textengine::font_families() {
				properties.push((
					"combo_option".to_string(),
					oak_node::value::NodeValue::Text(name),
				));
			}
		}
		// The v3 text node's editable text inputs hold more than one line
		// (the node shapes each `\n`-separated line of the string), and the
		// inspector renders them as multi-line fields so the user can type
		// line breaks. The node layer cannot say so — an input carries no
		// presentation metadata beyond its properties — so the facade marks
		// the multi-line text ids here, the same way it injects the font
		// families above.
		if input.value_type == ValueType::Text
			&& matches!(input.id.as_str(), "plain_text_in" | "text_in")
			&& !properties.iter().any(|(k, _)| k == "multiline")
		{
			properties.push((
				"multiline".to_string(),
				oak_node::value::NodeValue::Boolean(true),
			));
		}
		out.push(super::engine::EffectParam {
			input_id: input.id.clone(),
			display_name,
			value_type: input.value_type,
			value: entry.core.standard_value(&input.id, -1),
			flags: input.flags,
			properties,
		});
	}
	Some(out)
}

/// The combo option labels of a parameter, collected from the repeated
/// `("combo_option", Text)` property keys (the OFX translation pass
/// carries the choice options that way; `str_combo` values ride the
/// `("combo_value", Text)` keys).
pub fn combo_options(p: &super::engine::EffectParam) -> Vec<String> {
	p.properties
		.iter()
		.filter(|(k, _)| k == "combo_option")
		.filter_map(|(_, v)| match v {
			oak_node::value::NodeValue::Text(s) => Some(s.clone()),
			_ => None,
		})
		.collect()
}

/// The string-combo values of a parameter (`("combo_value", Text)` keys).
pub fn combo_values(p: &super::engine::EffectParam) -> Vec<String> {
	p.properties
		.iter()
		.filter(|(k, _)| k == "combo_value")
		.filter_map(|(_, v)| match v {
			oak_node::value::NodeValue::Text(s) => Some(s.clone()),
			_ => None,
		})
		.collect()
}

/// The `ui_group` / `ui_page` property of a parameter, if any (the OFX
/// group/page headers; the inspector renders them as section titles).
pub fn ui_section_of(p: &super::engine::EffectParam) -> Option<(String, String)> {
	let group = p
		.properties
		.iter()
		.find(|(k, _)| k == "ui_group")
		.and_then(|(_, v)| match v {
			oak_node::value::NodeValue::Text(s) => Some(s.clone()),
			_ => None,
		});
	let page = p
		.properties
		.iter()
		.find(|(k, _)| k == "ui_page")
		.and_then(|(_, v)| match v {
			oak_node::value::NodeValue::Text(s) => Some(s.clone()),
			_ => None,
		});
	match (group, page) {
		(None, None) => None,
		(group, page) => Some((group.unwrap_or_default(), page.unwrap_or_default())),
	}
}

// ---------------------------------------------------------------------------
// Command pieces
// ---------------------------------------------------------------------------



/// Shared state of an add-node command: the node's entry while detached
/// plus its arena id once added (kept across undo/redo so the rewiring
/// commands of the same group keep addressing the same slot).
type AddNodeState = Arc<Mutex<(Option<NodeEntry>, Option<NodeId>)>>;

/// A command that adds a node entry to the project's graph on `redo` and
/// detaches it (entry preserved, arena slot retained) on `undo`. Returns
/// the command and its shared state — the caller reads the assigned id
/// after the eager redo (the group push executes redos immediately, so
/// the rewiring commands pushed next address the live id).
fn add_node_command(p: &ProjectRef, entry: NodeEntry) -> (UndoCommand, AddNodeState) {
	let state: AddNodeState = Arc::new(Mutex::new((Some(entry), None)));
	let (s1, s2) = (state.clone(), state.clone());
	let (p1, p2) = (p.clone(), p.clone());
	(
		UndoCommand::from_closures(
			move || {
				let mut st = s1.lock().unwrap_or_else(|e| e.into_inner());
				let Some(entry) = st.0.take() else {
					return;
				};
				let mut g = lock(&p1);
				st.1 = Some(match st.1 {
					Some(id) => g.graph.add_entry(entry, id),
					None => g.graph.add_node(entry.core, entry.behavior),
				});
			},
			move || {
				let mut st = s2.lock().unwrap_or_else(|e| e.into_inner());
				let Some(id) = st.1 else {
					return;
				};
				let mut g = lock(&p2);
				st.0 = g.graph.take_node(id);
			},
		),
		state,
	)
}

// ---------------------------------------------------------------------------
// Chain edits (each is ONE undo row)
// ---------------------------------------------------------------------------

/// Run `f` inside an undo group: every push redoes eagerly (so a later
/// child's construction and validation see the post-edit graph — the
/// facade's `oakengine_undo_group_begin` flow), and the group closes as
/// ONE undo row on success / aborts (undoing the executed children) on
/// error.
fn grouped<T>(name: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
	oak_undo::global::group_begin(name).map_err(|e| e.to_string())?;
	match f() {
		Ok(v) => {
			oak_undo::global::group_end().map_err(|e| e.to_string())?;
			Ok(v)
		}
		Err(e) => {
			let _ = oak_undo::global::group_abort();
			Err(e)
		}
	}
}

/// Undoable enable toggle of an effect node ("Toggle Effect"; the
/// `enabled_in` flag).
pub fn set_enabled(p: &ProjectRef, effect: NodeId, enabled: bool) -> Result<(), String> {
	let old = {
		let g = lock(p);
		if !g.graph.is_valid(effect) {
			return Err("toggle effect: node not found".to_string());
		}
		is_enabled(&g.graph, effect)
	};
	let (p1, p2) = (p.clone(), p.clone());
	push(
		UndoCommand::from_closures(
			move || {
				let mut g = lock(&p1);
				if let Some(e) = g.graph.get_mut(effect) {
					e.core
						.set_standard_value(ENABLED_INPUT, -1, NodeValue::Boolean(enabled));
				}
			},
			move || {
				let mut g = lock(&p2);
				if let Some(e) = g.graph.get_mut(effect) {
					e.core
						.set_standard_value(ENABLED_INPUT, -1, NodeValue::Boolean(old));
				}
			},
		),
		"Toggle Effect",
	)
}

/// Undoable set of an effect parameter (a node input's standard value) —
/// "Set Parameter". `input_id` must exist on the node; the value is
/// written as the standard value of element -1 (non-keyframed). Used by
/// the inspector's OFX parameter controls.
pub fn set_input_value(
	p: &ProjectRef,
	effect: NodeId,
	input_id: &str,
	value: NodeValue,
) -> Result<(), String> {
	let old = {
		let g = lock(p);
		let entry = g
			.graph
			.get(effect)
			.ok_or_else(|| "set parameter: node not found".to_string())?;
		if entry.core.get_input(input_id).is_none() {
			return Err(format!("set parameter: unknown input \"{input_id}\""));
		}
		entry.core.standard_value(input_id, -1)
	};
	// NodeValue is not Copy: each closure owns its own clone (the closures
	// are FnMut and may run more than once across undo/redo cycles).
	let (p1, p2) = (p.clone(), p.clone());
	let (input_id, value) = (input_id.to_string(), value);
	let (redo_input, undo_input) = (input_id.clone(), input_id);
	let redo_value = value.clone();
	let undo_old = old.clone();
	push(
		UndoCommand::from_closures(
			move || {
				let mut g = lock(&p1);
				if let Some(e) = g.graph.get_mut(effect) {
					e.core
						.set_standard_value(&redo_input, -1, redo_value.clone());
				}
			},
			move || {
				let mut g = lock(&p2);
				if let Some(e) = g.graph.get_mut(effect) {
					e.core
						.set_standard_value(&undo_input, -1, undo_old.clone());
				}
			},
		),
		"Set Parameter",
	)
}

/// The neighbors of chain position `pos` in `chain` (length `len`):
/// `(upstream, downstream)` — the chain source is `None`, the host closes
/// the chain.
fn neighbors(g: &Graph, host: NodeId, chain: &[NodeId], pos: usize) -> (Option<NodeId>, NodeId) {
	let len = chain.len();
	if pos == 0 {
		if len == 0 {
			(None, host)
		} else {
			let d = chain[0];
			let up = effect_input_of(g, d).and_then(|i| connected_node(g, d, &i));
			(up, d)
		}
	} else if pos >= len {
		(Some(chain[len - 1]), host)
	} else {
		(Some(chain[pos - 1]), chain[pos])
	}
}

/// Undoable insertion of a new effect of `type_id` at chain position
/// `index` (0 = closest to the source, `len` = closest to the host;
/// out-of-range indices clamp to the ends). The node is created from the
/// factory, added to the host's project, and wired into the chain — one
/// undo row for the whole edit ("Add Effect"). Returns the new node's id.
pub fn insert(p: &ProjectRef, host: NodeId, index: usize, type_id: &str) -> Result<NodeId, String> {
	let (chain_vec, new_entry, new_input) = {
		let g = lock(p);
		// The host must be able to host effects.
		if effect_input_of(&g.graph, host).is_none() {
			return Err("node cannot host effects (no effect input)".to_string());
		}
		let Some((core, behavior)) = oak_node::factory::Factory::global().create_any(type_id) else {
			return Err(format!("unknown node type id \"{type_id}\""));
		};
		if core.effect_input.is_empty() {
			return Err("node type has no effect input; cannot be chained".to_string());
		}
		let new_input = core.effect_input.clone();
		let entry = NodeEntry {
			core,
			behavior,
			generation: 0,
			vacant: false,
		};
		(chain(&g.graph, host), entry, new_input)
	};
	let pos = index.min(chain_vec.len());
	let (upstream, downstream) = {
		let g = lock(p);
		neighbors(&g.graph, host, &chain_vec, pos)
	};
	let downstream_input = {
		let g = lock(p);
		effect_input_of(&g.graph, downstream).unwrap_or_default()
	};

	grouped("Add Effect", || {
		// 1. Add the node to the project (the eager redo runs now; the
		//    rewiring commands below address the live id).
		let (add, state) = add_node_command(p, new_entry);
		push(add, "Add Effect")?;
		let fresh = state
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.1
			.ok_or_else(|| "the add-node command produced no id".to_string())?;
		// 2. Unhook the downstream input from its current upstream (when
		//    there is one).
		if upstream.is_some() {
			push(disconnect_command(p, downstream, &downstream_input)?, "Add Effect")?;
		}
		// 3. Wire the new effect between upstream and downstream (each
		//    command validates against the live post-edit graph).
		push(connect_command(p, fresh, downstream, &downstream_input)?, "Add Effect")?;
		if let Some(upstream) = upstream {
			push(connect_command(p, upstream, fresh, &new_input)?, "Add Effect")?;
		}
		Ok(fresh)
	})
}

/// Undoable removal of `effect` (a node in `host`'s chain): unhook both
/// edges and bridge the gap, one undo row ("Remove Effect"). The node
/// itself is left orphaned in the project graph: the module node-transfer
/// commands are one-way, so a reversible detach does not exist there yet
/// — documented limitation carried over from the facade.
pub fn remove(p: &ProjectRef, host: NodeId, effect: NodeId) -> Result<(), String> {
	let (chain_vec, pos) = {
		let g = lock(p);
		let chain_vec = chain(&g.graph, host);
		let Some(pos) = chain_vec.iter().position(|&c| c == effect) else {
			return Err("remove effect: the node is not in the chain".to_string());
		};
		(chain_vec, pos)
	};
	// The neighbors of the REMOVED node: upstream is chain[pos - 1] (or the
	// chain source — the node feeding the first effect — for pos == 0),
	// downstream is chain[pos + 1] (or the host at the chain's end).
	let (upstream, downstream) = {
		let g = lock(p);
		let upstream = if pos == 0 {
			effect_input_of(&g.graph, chain_vec[0])
				.and_then(|i| connected_node(&g.graph, chain_vec[0], &i))
		} else {
			Some(chain_vec[pos - 1])
		};
		let downstream = if pos + 1 == chain_vec.len() {
			host
		} else {
			chain_vec[pos + 1]
		};
		(upstream, downstream)
	};
	let (eff_input, downstream_input) = {
		let g = lock(p);
		(
			effect_input_of(&g.graph, effect).unwrap_or_default(),
			effect_input_of(&g.graph, downstream).unwrap_or_default(),
		)
	};

	grouped("Remove Effect", || {
		// 1. Unhook the effect from its upstream.
		if upstream.is_some() {
			push(disconnect_command(p, effect, &eff_input)?, "Remove Effect")?;
		}
		// 2. Unhook the downstream from the effect, then bridge it back to
		//    the upstream (validated against the live post-edit graph).
		push(disconnect_command(p, downstream, &downstream_input)?, "Remove Effect")?;
		if let Some(upstream) = upstream {
			push(connect_command(p, upstream, downstream, &downstream_input)?, "Remove Effect")?;
		}
		Ok(())
	})
}

/// Undoable reorder of `effect` to chain position `new_index` (an
/// insertion index **after** removal, matching the effect stack's
/// `ReorderRequested`; `0..=len-1` where `len` is the post-removal chain
/// length). One undo row ("Reorder Effect").
pub fn move_effect(
	p: &ProjectRef,
	host: NodeId,
	effect: NodeId,
	new_index: usize,
) -> Result<(), String> {
	let (chain_vec, from) = {
		let g = lock(p);
		let chain_vec = chain(&g.graph, host);
		let Some(from) = chain_vec.iter().position(|&c| c == effect) else {
			return Err("reorder effect: the node is not in the chain".to_string());
		};
		(chain_vec, from)
	};
	let len = chain_vec.len();
	let to = new_index.min(len - 1);

	let (upstream, downstream, eff_input, downstream_input) = {
		let g = lock(p);
		let upstream = if from == 0 {
			effect_input_of(&g.graph, chain_vec[0])
				.and_then(|i| connected_node(&g.graph, chain_vec[0], &i))
		} else {
			Some(chain_vec[from - 1])
		};
		let downstream = if from + 1 == len {
			host
		} else {
			chain_vec[from + 1]
		};
		(
			upstream,
			downstream,
			effect_input_of(&g.graph, effect).unwrap_or_default(),
			effect_input_of(&g.graph, downstream).unwrap_or_default(),
		)
	};
	// The post-removal chain and the reinsertion neighbors: position `to`
	// (0..=len-1) sits between index `to - 1` and `to` of the remaining
	// list, where index `-1` is the chain source and index `len-1` is the
	// host.
	let remaining: Vec<NodeId> = chain_vec
		.iter()
		.enumerate()
		.filter(|(i, _)| *i != from)
		.map(|(_, &c)| c)
		.collect();
	let (up2, down2) = {
		let g = lock(p);
		let rlen = remaining.len();
		if to == 0 {
			if rlen == 0 {
				(None, host)
			} else {
				let d = remaining[0];
				let up = effect_input_of(&g.graph, d).and_then(|i| connected_node(&g.graph, d, &i));
				(up, d)
			}
		} else if to >= rlen {
			(Some(remaining[rlen - 1]), host)
		} else {
			(Some(remaining[to - 1]), remaining[to])
		}
	};
	let down2_input = {
		let g = lock(p);
		effect_input_of(&g.graph, down2).unwrap_or_default()
	};

	grouped("Reorder Effect", || {
		// ---- remove the effect from position `from` ----------------------
		if upstream.is_some() {
			push(disconnect_command(p, effect, &eff_input)?, "Reorder Effect")?;
		}
		push(disconnect_command(p, downstream, &downstream_input)?, "Reorder Effect")?;
		if let Some(upstream) = upstream {
			push(connect_command(p, upstream, downstream, &downstream_input)?, "Reorder Effect")?;
		}
		// ---- reinsert at position `to` (each command validates against
		// the live post-edit graph) ------------------------------------
		if up2.is_some() {
			push(disconnect_command(p, down2, &down2_input)?, "Reorder Effect")?;
		}
		push(connect_command(p, effect, down2, &down2_input)?, "Reorder Effect")?;
		// The upstream -> effect connect addresses the EFFECT's own effect
		// input (identical to the upstream's for the standard tex_in chains).
		if let Some(up2) = up2 {
			push(connect_command(p, up2, effect, &eff_input)?, "Reorder Effect")?;
		}
		Ok(())
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::oakui::graphops;

	/// The global undo stack is process-wide; serialize these tests (shared
	/// with the other app test modules).
	fn stack_lock() -> std::sync::MutexGuard<'static, ()> {
		crate::oakui::graphops::test_lock()
	}

	/// A project with a clip node (an effect-chain host: clips carry the
	/// `tex_in` effect input).
	fn project_with_clip() -> (ProjectRef, NodeId) {
		let project = graphops::create_project();
		let clip = {
			let mut g = lock(&project);
			let (core, behavior) = oak_node::block::clip_create();
			g.graph.add_node(core, behavior)
		};
		(project, clip)
	}

	/// A video-effect type id the factory really registers.
	fn some_effect_type() -> String {
		addable_effects()
			.into_iter()
			.next()
			.expect("the factory registers at least one video effect")
			.type_id
	}

	#[test]
	fn insert_walks_the_chain_and_undoes() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();
		let ty = some_effect_type();

		let first = insert(&project, host, 0, &ty).expect("insert into an empty chain");
		assert_eq!(chain(&lock(&project).graph, host), vec![first]);
		let second = insert(&project, host, 0, &ty).expect("insert at the source side");
		assert_eq!(chain(&lock(&project).graph, host), vec![second, first]);
		let third = insert(&project, host, 1, &ty).expect("insert in the middle");
		assert_eq!(chain(&lock(&project).graph, host), vec![second, third, first]);

		// One undo row per insert.
		oak_undo::global::undo().unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![second, first]);
		oak_undo::global::undo().unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![first]);
		oak_undo::global::redo().unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![second, first]);
		oak_undo::global::clear().unwrap();
	}

	#[test]
	fn remove_bridges_the_gap() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();
		let ty = some_effect_type();
		let a = insert(&project, host, 0, &ty).unwrap();
		let b = insert(&project, host, 1, &ty).unwrap();
		let c = insert(&project, host, 2, &ty).unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![a, b, c]);

		remove(&project, host, b).expect("remove the middle effect");
		assert_eq!(chain(&lock(&project).graph, host), vec![a, c]);
		oak_undo::global::undo().unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![a, b, c]);
		oak_undo::global::clear().unwrap();
	}

	#[test]
	fn move_reorders_the_chain() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();
		let ty = some_effect_type();
		let a = insert(&project, host, 0, &ty).unwrap();
		let b = insert(&project, host, 1, &ty).unwrap();
		let c = insert(&project, host, 2, &ty).unwrap();

		move_effect(&project, host, c, 0).expect("move the last effect to the source side");
		assert_eq!(chain(&lock(&project).graph, host), vec![c, a, b]);
		oak_undo::global::undo().unwrap();
		assert_eq!(chain(&lock(&project).graph, host), vec![a, b, c]);
		oak_undo::global::clear().unwrap();
	}

	#[test]
	fn set_enabled_toggles_and_undoes() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();
		let ty = some_effect_type();
		let eff = insert(&project, host, 0, &ty).unwrap();
		assert!(is_enabled(&lock(&project).graph, eff));

		set_enabled(&project, eff, false).unwrap();
		assert!(!is_enabled(&lock(&project).graph, eff));
		oak_undo::global::undo().unwrap();
		assert!(is_enabled(&lock(&project).graph, eff));
		oak_undo::global::clear().unwrap();
	}

	#[test]
	fn addable_effects_are_video_effects() {
		let entries = addable_effects();
		assert!(!entries.is_empty());
		for entry in &entries {
			assert!(!entry.type_id.is_empty());
			assert!(!entry.name.is_empty());
		}
	}

	/// Every built-in entry carries its category's group key — one of
	/// [`GROUP_KEYS`], never a display string — and the shipped nodes use
	/// more than one group (a single "general" for everything would mean
	/// the category mapping is dead).
	#[test]
	fn addable_effects_group_builtins_by_category() {
		let factory = oak_node::factory::Factory::global();
		let mut groups: Vec<&'static str> = Vec::new();
		let mut builtins = 0usize;
		for entry in addable_effects() {
			if factory.find(&entry.type_id).is_none() {
				continue; // an OpenFX plugin entry
			}
			builtins += 1;
			let group = entry
				.group
				.as_deref()
				.unwrap_or_else(|| panic!("built-in {} has no group", entry.type_id));
			let key = GROUP_KEYS.iter().copied().find(|key| *key == group).unwrap_or_else(|| {
				panic!("built-in {} has the unknown group {group:?}", entry.type_id)
			});
			if !groups.contains(&key) {
				groups.push(key);
			}
		}
		assert!(builtins > 0, "the factory ships built-in video effects");
		assert!(
			groups.len() > 1,
			"built-ins spread over several groups: {groups:?}"
		);
	}

	/// OpenFX plugin entries keep their plugin-supplied sub-category
	/// (Filter / Generator / Transition / General), which never collides
	/// with a built-in group key.
	#[test]
	fn addable_effects_group_openfx_by_sub_category() {
		let factory = oak_node::factory::Factory::global();
		for entry in addable_effects() {
			if factory.find_dynamic(&entry.type_id).is_none() {
				continue; // a built-in
			}
			let group = entry
				.group
				.as_deref()
				.expect("an OpenFX entry carries a sub-category");
			assert!(
				["Filter", "Generator", "Transition", "General"].contains(&group),
				"unexpected OpenFX sub-category {group:?}"
			);
			assert!(
				!GROUP_KEYS.contains(&group),
				"OpenFX sub-category {group:?} collides with a built-in group key"
			);
		}
	}

	/// The sort the library and the inspector render: built-in groups in
	/// [`GROUP_KEYS`] order, then the OpenFX sub-categories, names folded
	/// alphabetically inside a group.
	#[test]
	fn addable_effects_sort_builtins_first() {
		let entries = addable_effects();
		let rank = |entry: &crate::oakui::engine::EffectEntry| {
			entry
				.group
				.as_deref()
				.and_then(group_rank)
				.unwrap_or(GROUP_KEYS.len())
		};
		for pair in entries.windows(2) {
			let (a, b) = (&pair[0], &pair[1]);
			let (ra, rb) = (rank(a), rank(b));
			if ra != rb {
				assert!(
					ra < rb,
					"group order: {a:?} ({ra}) before {b:?} ({rb})"
				);
			} else if a.group == b.group {
				assert!(
					a.name.to_lowercase() <= b.name.to_lowercase(),
					"name order inside {:?}: {a:?} before {b:?}",
					a.group
				);
			} else {
				// Two OpenFX sub-categories (rank only separates built-ins).
				assert!(
					a.group.as_deref().unwrap_or("") <= b.group.as_deref().unwrap_or(""),
					"OpenFX sub-category order: {a:?} before {b:?}"
				);
			}
		}
	}

	/// The category → group-key mapping covers every `Category` variant
	/// and falls back to `general`.
	#[test]
	fn category_group_key_maps_every_category() {
		use oak_node::node::Category;
		let all = [
			Category::Output,
			Category::Effect,
			Category::Generator,
			Category::Input,
			Category::Math,
			Category::Color,
			Category::Distort,
			Category::Filter,
			Category::Keying,
			Category::OpenFx,
			Category::Timeline,
			Category::Group,
		];
		for category in all {
			assert!(
				GROUP_KEYS.contains(&category_group_key(category)),
				"{category:?} maps outside GROUP_KEYS"
			);
		}
		assert_eq!(category_group_key(Category::Color), "color");
		assert_eq!(category_group_key(Category::Keying), "keying");
		assert_eq!(category_group_key(Category::Effect), "general");
		assert_eq!(category_group_key(Category::OpenFx), "general");
	}

	/// A group label is the language pack's string for the built-in keys —
	/// never the raw key — and passes OpenFX sub-categories through.
	#[test]
	fn group_label_localizes_keys_and_passes_sub_categories_through() {
		for key in GROUP_KEYS {
			let label = group_label(key);
			assert!(
				!label.starts_with("effect_library.group."),
				"language pack entry missing for the {key} group (got {label:?})"
			);
		}
		assert_eq!(group_label("Filter"), "Filter");
		assert_eq!(group_label("Sapphire"), "Sapphire");
	}

	/// `set_input_value` writes the standard value undoably and rejects
	/// unknown input ids.
	#[test]
	fn set_input_value_undoes() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();

		// Choose a built-in effect whose scratch core exposes a float input
		// (the value write/undo round-trip needs one).
		let ty = addable_effects()
			.into_iter()
			.find(|entry| {
				let (core, _behavior) = oak_node::factory::Factory::global()
					.create_any(&entry.type_id)
					.expect("the entry resolves");
				core.inputs.iter().any(|i| {
					i.value_type == oak_node::value::ValueType::Float
						&& i.flags & oak_node::input::flags::HIDDEN == 0
				})
			})
			.expect("at least one built-in effect exposes a float input")
			.type_id;
		let eff = insert(&project, host, 0, &ty).unwrap();

		// Unknown input: rejected without touching the graph.
		assert!(set_input_value(&project, eff, "nope", NodeValue::Float(1.0)).is_err());

		// Pick an editable (non-texture, non-hidden) float input of the
		// inserted effect to exercise the write/undo round-trip.
		let input_id = {
			let g = lock(&project);
			g.graph
				.get(eff)
				.and_then(|e| {
					e.core.inputs.iter().find(|i| {
						i.value_type == oak_node::value::ValueType::Float
							&& i.flags & oak_node::input::flags::HIDDEN == 0
					})
				})
				.map(|i| i.id.clone())
				.expect("the effect exposes a float input")
		};
		let before = lock(&project)
			.graph
			.get(eff)
			.unwrap()
			.core
			.standard_value(&input_id, -1);
		set_input_value(&project, eff, &input_id, NodeValue::Float(42.0)).unwrap();
		assert_eq!(
			lock(&project).graph.get(eff).unwrap().core.standard_value(&input_id, -1),
			NodeValue::Float(42.0)
		);
		oak_undo::global::undo().unwrap();
		assert_eq!(
			lock(&project).graph.get(eff).unwrap().core.standard_value(&input_id, -1),
			before
		);
		oak_undo::global::clear().unwrap();
	}

	/// The inspector's colour-parameter loop: the value the UI commits
	/// (`NodeValue::Color` built from `f32` channels) must read back
	/// **exactly** through `effect_params` — the per-frame value sync
	/// (`sync_values` → `set_committed`) pulls the swatch back to the engine
	/// value whenever the readback differs, so any rounding here would make
	/// a freshly committed colour snap back to the old one.
	#[test]
	fn color_param_set_read_round_trip() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, host) = project_with_clip();

		// A built-in video effect whose scratch core exposes a colour input
		// (the inspector's swatch picker handles `ValueType::Color`).
		let ty = addable_effects()
			.into_iter()
			.find(|entry| {
				let (core, _behavior) = oak_node::factory::Factory::global()
					.create_any(&entry.type_id)
					.expect("the entry resolves");
				core.inputs.iter().any(|i| {
					i.value_type == oak_node::value::ValueType::Color
						&& i.flags & oak_node::input::flags::HIDDEN == 0
				})
			})
			.expect("at least one built-in effect exposes a colour input")
			.type_id;
		let eff = insert(&project, host, 0, &ty).unwrap();

		let input_id = {
			let g = lock(&project);
			g.graph
				.get(eff)
				.and_then(|e| {
					e.core.inputs.iter().find(|i| {
						i.value_type == oak_node::value::ValueType::Color
							&& i.flags & oak_node::input::flags::HIDDEN == 0
					})
				})
				.map(|i| i.id.clone())
				.expect("the effect exposes a colour input")
		};

		// The snapshot lists the param under the same id the UI writes to.
		let params = effect_params(&lock(&project).graph, eff).unwrap();
		let param = params
			.iter()
			.find(|p| p.input_id == input_id)
			.expect("the colour param appears in the snapshot");
		assert_eq!(param.value_type, oak_node::value::ValueType::Color);

		// Commit the way `OfxColorPicker::commit` does: f32 channels widened
		// to f64. The readback (f64 → f32) must round-trip exactly, or the
		// swatch would snap back on the next `sync_values` pass.
		let color = NodeValue::Color([0.25, 0.5, 0.75, 0.4]);
		set_input_value(&project, eff, &input_id, color.clone()).unwrap();
		let snapshot = effect_params(&lock(&project).graph, eff).unwrap();
		let read = snapshot
			.iter()
			.find(|p| p.input_id == input_id)
			.unwrap();
		assert_eq!(read.value, color, "the engine must echo the committed colour");

		oak_undo::global::clear().unwrap();
	}

	/// The plugin-node colour loop, shaped exactly like the OFX translation
	/// pass's `build_core` (default cached at element 0 AND on
	/// `Input::default`; the panel reads/writes element -1; the renderer's
	/// traverser fills unconnected inputs from `value_at_time(id, -1)`):
	/// the inspector's commit must echo back through `effect_params` and
	/// reach the render-side input row, or the plugin would keep rendering
	/// the old colour while the swatch snapped back.
	#[test]
	fn plugin_color_param_set_read_round_trip() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, _host) = project_with_clip();

		// A plugin-shaped node: colour param + texture clip input, defaults
		// cached at element 0 and on the input (node_factory `build_core`).
		let plugin = {
			let mut g = lock(&project);
			let mut core = oak_node::node::NodeCore::new();
			let default = oak_node::value::NodeValue::Color([1.0, 0.0, 0.0, 1.0]);
			core.add_input(oak_node::input::Input::new(
				"TintColor",
				oak_node::value::ValueType::Color,
				default.clone(),
			));
			core.set_standard_value("TintColor", 0, default);
			core.add_input(oak_node::input::Input::new(
				"Source",
				oak_node::value::ValueType::Texture,
				oak_node::value::NodeValue::None,
			));
			core.effect_input = "Source".to_string();
			let behavior = Box::new(oak_node::nodes::plugin::PluginNode::new(
				oak_node::nodes::plugin::PluginInstanceHandle::null(),
				"Tint".to_string(),
				"org.example.tint".to_string(),
				"A test tint plugin".to_string(),
				"Filter".to_string(),
			));
			g.graph.add_node(core, behavior)
		};

		// The snapshot lists the colour param under the OFX param name.
		let params = effect_params(&lock(&project).graph, plugin).unwrap();
		let param = params
			.iter()
			.find(|p| p.input_id == "TintColor")
			.expect("the plugin colour param appears in the snapshot");
		assert_eq!(param.value_type, oak_node::value::ValueType::Color);
		assert_eq!(
			param.value,
			oak_node::value::NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
			"the panel must open with the plugin's default colour"
		);

		// Commit the way `OfxColorPicker::commit` does (f64 channels from
		// the f32 draft).
		let color = oak_node::value::NodeValue::Color([0.25, 0.5, 0.75, 0.4]);
		set_input_value(&project, plugin, "TintColor", color.clone()).unwrap();
		let snapshot = effect_params(&lock(&project).graph, plugin).unwrap();
		let read = snapshot
			.iter()
			.find(|p| p.input_id == "TintColor")
			.unwrap();
		assert_eq!(read.value, color, "the engine must echo the committed colour");

		// The render-side read: the traverser fills unconnected inputs from
		// `value_at_time(id, -1)`, which the plugin job payload then carries
		// to `apply_param_overrides`.
		let render_read = lock(&project)
			.graph
			.get(plugin)
			.unwrap()
			.core
			.value_at_time("TintColor", -1, oak_core::Rational::new(0, 1));
		assert_eq!(render_read, color, "the renderer must see the committed colour");

		// Undo restores the default; redo reapplies the commit.
		oak_undo::global::undo().unwrap();
		assert_eq!(
			lock(&project)
				.graph
				.get(plugin)
				.unwrap()
				.core
				.standard_value("TintColor", -1),
			oak_node::value::NodeValue::Color([1.0, 0.0, 0.0, 1.0])
		);
		oak_undo::global::redo().unwrap();
		assert_eq!(
			lock(&project)
				.graph
				.get(plugin)
				.unwrap()
				.core
				.standard_value("TintColor", -1),
			color
		);

		oak_undo::global::clear().unwrap();
	}

	/// The combo-option collector reads the repeated `("combo_option", _)`
	/// property keys (and the string-combo values from `("combo_value", _)`).
	#[test]
	fn combo_option_collectors_read_repeated_properties() {
		use crate::oakui::engine::EffectParam;
		use oak_node::value::{NodeValue, ValueType};
		let param = EffectParam {
			input_id: "mode".into(),
			display_name: "Mode".into(),
			value_type: ValueType::Combo,
			value: NodeValue::Combo(0),
			flags: 0,
			properties: vec![
				("combo_option".into(), NodeValue::Text("Fast".into())),
				("combo_option".into(), NodeValue::Text("High".into())),
				("combo_value".into(), NodeValue::Text("fast".into())),
				("combo_value".into(), NodeValue::Text("high".into())),
			],
		};
		assert_eq!(combo_options(&param), vec!["Fast", "High"]);
		assert_eq!(combo_values(&param), vec!["fast", "high"]);
	}

	/// ui_group / ui_page surface as a section header (empty halves are
	/// fine); params without either have no section.
	#[test]
	fn ui_section_collects_group_and_page() {
		use crate::oakui::engine::EffectParam;
		use oak_node::value::{NodeValue, ValueType};
		let plain = EffectParam {
			input_id: "p".into(),
			display_name: "P".into(),
			value_type: ValueType::Int,
			value: NodeValue::Int(0),
			flags: 0,
			properties: vec![],
		};
		assert!(ui_section_of(&plain).is_none());

		let grouped = EffectParam {
			properties: vec![
				("ui_group".into(), NodeValue::Text("Basic".into())),
				("ui_page".into(), NodeValue::Text("Main".into())),
			],
			..plain
		};
		assert_eq!(ui_section_of(&grouped), Some(("Basic".into(), "Main".into())));
	}

	/// W6: the v3 text node is a footage entry now (the project panel's
	/// "add text footage" button and the timeline drop), so the effect
	/// library / add menus no longer offer it. A text3 node that is in a
	/// project anyway — a legacy chain, or the one the footage path
	/// creates — must keep chaining and keep exposing its params.
	#[test]
	fn text3_is_not_addable_but_a_chain_node_still_reports_its_params() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();

		const TEXT3: &str = "org.olivevideoeditor.Olive.text3";
		assert!(
			!addable_effects().iter().any(|e| e.type_id == TEXT3),
			"text3 left the effect add menus"
		);

		// `insert` resolves through the factory (which still registers the
		// type) rather than the addable table: legacy chains and the
		// footage path keep working.
		let (project, host) = project_with_clip();
		let node = insert(&project, host, 0, TEXT3).expect("chain a legacy text3 effect");
		assert_eq!(chain(&lock(&project).graph, host), vec![node]);

		let params = effect_params(&lock(&project).graph, node).expect("the node exposes params");
		let plain_text = params
			.iter()
			.find(|p| p.input_id == "plain_text_in")
			.expect("the structured text parameter survives");
		assert_eq!(plain_text.value_type, oak_node::value::ValueType::Text);
		// The facade marks the multi-line text ids (the inspector renders a
		// multi-line field off this property); the node itself carries no
		// presentation metadata.
		assert!(
			plain_text
				.properties
				.iter()
				.any(|(k, v)| k == "multiline"
					&& matches!(v, oak_node::value::NodeValue::Boolean(true))),
			"plain_text_in is marked as multi-line"
		);
		assert!(params.iter().any(|p| p.input_id == "font_size_in"));
		assert!(params.iter().any(|p| p.input_id == "outline_enabled_in"));
		// The hidden legacy HTML input stays a param *input* in the node,
		// never in the inspector.
		assert!(!params.iter().any(|p| p.input_id == "text_in"));

		oak_undo::global::clear().unwrap();
	}

	/// The effect library and the inspector name built-in effects and their
	/// parameters from runtime-built pack keys (`node.<type-id suffix>.name`
	/// / `node.<type-id suffix>.input.<input id>`): switching the active
	/// language renames the whole library and parameter list, while a node
	/// the packs don't know keeps the name it ships with.
	#[test]
	fn effect_and_parameter_names_follow_the_language_pack() {
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		crate::i18n::set_language_code("zh-CN");

		// Every addable built-in shows its pack name, never the factory's
		// English one.
		let factory = oak_node::factory::Factory::global();
		let mut builtins = 0usize;
		for entry in addable_effects() {
			let Some(meta) = factory.find(&entry.type_id) else {
				continue; // an OpenFX plugin entry: the plugin names it
			};
			assert_ne!(
				entry.name, meta.name,
				"{} is still English in zh-CN",
				entry.type_id
			);
			builtins += 1;
		}
		assert!(builtins > 0, "the factory ships addable built-ins");
		let name_of = |type_id: &str| {
			addable_effects()
				.into_iter()
				.find(|e| e.type_id == type_id)
				.unwrap_or_else(|| panic!("{type_id} is addable"))
				.name
		};
		assert_eq!(
			name_of("org.olivevideoeditor.Olive.colorcorrect"),
			"颜色校正"
		);
		assert_eq!(name_of("org.olivevideoeditor.Olive.position"), "位置");

		// The inspector's parameter labels follow the same keys.
		let (project, host) = project_with_clip();
		let node = insert(&project, host, 0, "org.olivevideoeditor.Olive.colorcorrect")
			.expect("chain Color Correct");
		let params = effect_params(&lock(&project).graph, node).expect("the node exposes params");
		let label = |input_id: &str| {
			params
				.iter()
				.find(|p| p.input_id == input_id)
				.unwrap_or_else(|| panic!("{input_id} is a parameter"))
				.display_name
				.clone()
		};
		assert_eq!(label("saturation_in"), "饱和度");
		assert_eq!(label("gamma_in"), "伽马");
		assert_eq!(label("gain_in"), "增益");

		crate::i18n::set_language_code("en-US");
		oak_undo::global::clear().unwrap();
	}

	/// The pack coverage behind the lookup above: every name a built-in
	/// node gives one of its inputs has a `node.<type-id suffix>.input.<id>`
	/// entry, so no parameter label falls back to English. Inputs the
	/// behavior does not name (`enabled_in`, `use_args_in`) carry no entry —
	/// their label is the raw input id, and the enabled input never renders.
	#[test]
	fn every_builtin_parameter_name_has_a_pack_entry() {
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("zh-CN");
		let mut missing = Vec::new();
		let mut named = 0usize;
		for meta in oak_node::factory::Factory::global().entries() {
			let (core, behavior) = (meta.create)();
			for input in &core.inputs {
				let english = behavior.input_name(&input.id);
				if english == input.id {
					continue;
				}
				if crate::i18n::tr_or(&input_name_key(meta.type_id, &input.id), english) == english {
					missing.push(format!("node.{}.input.{}", type_suffix(meta.type_id), input.id));
				}
				named += 1;
			}
		}
		assert!(missing.is_empty(), "no pack entry for {missing:?}");
		assert!(named >= 250, "the built-in nodes name their inputs ({named})");
		crate::i18n::set_language_code("en-US");
	}

	/// The v3 text node's `font_family_in` is a str-combo the node layer
	/// cannot populate (oak-node links no font crate): `effect_params`
	/// injects the app's font backend families as its combo options, sorted
	/// and de-duplicated.
	#[test]
	fn text_font_family_combo_is_injected_from_the_font_backend() {
		let _g = stack_lock();
		let (project, _clip) = project_with_clip();
		let node = {
			let mut g = lock(&project);
			let (core, behavior) = oak_node::factory::Factory::global()
				.create_any("org.olivevideoeditor.Olive.text3")
				.expect("the factory registers the v3 text node");
			g.graph.add_node(core, behavior)
		};

		let params = effect_params(&lock(&project).graph, node).expect("the node exposes params");
		let param = params
			.iter()
			.find(|p| p.input_id == "font_family_in")
			.expect("the font family combo is a parameter");
		assert_eq!(param.value_type, oak_node::value::ValueType::StrCombo);

		let options = combo_options(param);
		assert!(!options.is_empty(), "the font backend found no families");
		assert_eq!(options, crate::oakui::textengine::font_families());
		let mut sorted = options.clone();
		sorted.sort();
		sorted.dedup();
		assert_eq!(options, sorted);
	}
}
