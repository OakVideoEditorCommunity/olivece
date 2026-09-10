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

//! The inspector panel (检查器·效果栈): the `EffectStackView` over the
//! engine's node chain, shown as linear cards (媒体 → 变换 → OCIO LUT →
//! 输出) with add / remove / reorder — no engine, the mock applies the edits
//! to its own model.

use gpui::colors::DefaultColors;
use gpui::dock::{DockPanel, PanelEvent};
use gpui::effect_stack::{EffectCardKind, EffectId, EffectStackEvent, EffectStackView};
use gpui::{
	div, prelude::*, AnyElement, App, Context, Entity, EventEmitter, MouseButton, Render,
	SharedString, Window,
};
use crate::oakui::component::menu::{Menu, MenuItem};

use crate::oakui::component::menu::{ContextMenuHandle, ContextMenuTriggered};
use crate::oakui::effectchain::group_label;
use crate::oakui::engine::EffectEntry;
use crate::oakui::AppEngine;
use crate::panels::commands::PanelCommandHandler;
use crate::panels::ids::INSPECTOR;
use crate::panels::ofx_params::OfxParamsView;

/// The inspector / effect stack panel.
pub struct InspectorPanel<E: AppEngine> {
	stack: Entity<EffectStackView<E>>,
	engine: Entity<E>,
	/// An in-flight "add effect" flow: the stack insertion index carried
	/// by the last [`EffectStackEvent::AddRequested`]. While set (and the
	/// engine has a stack target), the panel renders a small menu of the
	/// engine's addable effects instead of forwarding the bare request.
	pending_add: Option<usize>,
	/// The right-click context menu.
	context_menu: ContextMenuHandle,
	/// The effect card the open context menu targets (id, enabled state,
	/// removable flag), when one is on the stack.
	context_effect: Option<(EffectId, bool, bool)>,
}

impl<E: AppEngine> InspectorPanel<E> {
	/// Builds the stack over `engine`'s effect model.
	pub fn new(engine: Entity<E>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let stack = cx.new(|cx| {
			let engine_for_params = engine.clone();
			EffectStackView::new(engine.clone(), cx).params_renderer(
				move |effect, window, cx| {
					// The OFX parameter view: auto-generated controls for the
					// effect's inputs (empty state for effects without a
					// parameter UI).
					cx.new(|cx| OfxParamsView::new(*effect, engine_for_params.clone(), window, cx))
						.into()
				},
			)
		});
		// The "edits are requests" loop: forward each request to the engine,
		// which applies it to its model and notifies. `AddRequested` carries
		// no effect type, so the panel records the insertion index and lets
		// the user pick one from the small menu below (the actual insert
		// runs through `AppEngine::add_effect`).
		cx.subscribe(&stack, |this, _stack, event: &EffectStackEvent, cx| {
			match event {
				EffectStackEvent::AddRequested { index } => {
					this.pending_add = Some(*index);
				}
				// A drop from the effect library carries the type id: insert
				// directly at the drop position, no picker menu.
				EffectStackEvent::AddTypeRequested { index, type_id } => {
					let (index, type_id) = (*index, type_id.clone());
					this.engine.update(cx, |engine, cx| {
						if let Err(err) = engine.add_effect(index, &type_id, cx) {
							println!("[inspector] add effect failed: {err}");
						}
					});
					return;
				}
				_ => {}
			}
			this.engine
				.update(cx, |engine, cx| engine.apply_effect_event(event, cx));
		})
		.detach();

		// The stack re-renders whenever the engine changes (selection-driven
		// highlight, effect edits, project drops) — the stack itself does not
		// subscribe to the engine entity.
		cx.observe(&engine, |_this, _engine, cx| cx.notify()).detach();

		let context_menu = ContextMenuHandle::new(Self::on_local_menu_item, window, cx);

		Self {
			stack,
			engine,
			pending_add: None,
			context_menu,
			context_effect: None,
		}
	}

	/// Handles the inspector's local (non-registry) context-menu items:
	/// they drive the same [`EffectStackEvent`]s the card widgets emit.
	fn on_local_menu_item(&mut self, item: usize, cx: &mut Context<Self>) {
		let Some((effect, enabled, _)) = self.context_effect else {
			return;
		};
		match item {
			LOCAL_ENABLE => {
				self.engine.update(cx, |engine, cx| {
					engine.apply_effect_event(
						&EffectStackEvent::EnableToggled {
							effect,
							enabled: !enabled,
						},
						cx,
					)
				});
			}
			LOCAL_REMOVE => {
				self.engine.update(cx, |engine, cx| {
					engine.apply_effect_event(&EffectStackEvent::RemoveRequested(effect), cx)
				});
			}
			LOCAL_RENAME | LOCAL_PROPERTIES => {
				println!("[inspector] context-menu item {item} (not implemented yet)");
			}
			_ => {
				println!("[inspector] unhandled local menu item {item}");
			}
		}
	}

	/// The "add effect" menu: a group header row per resolved effect group,
	/// each followed by that group's effect rows. Selecting an effect row
	/// inserts it at the recorded stack index; the ✕ in the pinned header
	/// closes the menu without adding. The list is height-capped and
	/// scrollable (with the OFX plugins registered it runs to 150+ rows — an
	/// uncapped list pushed the dismiss affordance far off-screen, making the
	/// menu impossible to close).
	fn render_add_menu(
		&mut self,
		index: usize,
		colors: &gpui::colors::Colors,
		cx: &mut Context<Self>,
	) -> impl IntoElement {
		let effects = self.engine.read(cx).addable_effects();
		let mut list = div()
			.id("inspector-add-menu-list")
			.max_h_64()
			.overflow_y_scroll()
			.flex()
			.flex_col()
			.gap_1()
			.px_2()
			.py_1();

		for row in add_menu_rows(&effects) {
			let entry = match row {
				AddMenuRow::Header { key, label } => {
					let key = key.unwrap_or_default().to_string();
					list = list.child(
						div()
							.id(SharedString::from(format!("add-effect-group-{key}")))
							.debug_selector(move || format!("add-effect-group-row-{key}").into())
							.pt_2()
							.pb_1()
							.px_2()
							.text_xs()
							.font_weight(gpui::FontWeight(600.0))
							.text_color(colors.disabled)
							.child(label),
					);
					continue;
				}
				AddMenuRow::Effect(entry) => entry,
			};
			let engine = self.engine.clone();
			let type_id = entry.type_id.clone();
			let name = entry.name.clone();
			let index = index;
			list = list.child(
				div()
					.id(SharedString::from(format!("add-effect-{type_id}")))
					.cursor_pointer()
					.px_2()
					.py_1()
					.rounded_sm()
					.hover(|style| style.bg(colors.selected))
					.text_color(colors.text)
					.text_sm()
					.child(name)
					.on_click(
						cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
							this.pending_add = None;
							engine.update(cx, |engine, cx| {
								if let Err(err) = engine.add_effect(index, &type_id, cx) {
									println!("[inspector] add effect failed: {err}");
								}
							});
							cx.notify();
						}),
					),
			);
		}

		div()
			.id("inspector-add-menu")
			.border_t_1()
			.border_color(colors.separator)
			.flex()
			.flex_col()
			.child(
				div()
					.flex()
					.items_center()
					.px_2()
					.py_1()
					.child(
						div()
							.flex_1()
							.text_xs()
							.text_color(colors.disabled)
							.child(crate::i18n::tr("inspector.add_effect")),
					)
					.child(
						div()
							.id("add-effect-dismiss")
							.cursor_pointer()
							.px_1()
							.rounded_sm()
							.hover(|style| style.bg(colors.selected))
							.text_color(colors.disabled)
							.text_sm()
							.child("✕")
							.on_click(cx.listener(
								move |this, _event: &gpui::ClickEvent, _window, cx| {
									this.pending_add = None;
									cx.notify();
								},
							)),
					),
			)
			.child(list)
	}
}

// ---------------------------------------------------------------------------
// Add-effect menu rows — the engine's addable-effect table (already grouped
// and sorted) flattened into one non-clickable header per group followed by
// the group's entries. The labels come from the same
// `effectchain::group_label` resolution the effect library uses, so both
// surfaces name a group alike.
// ---------------------------------------------------------------------------

/// One row of the add-effect menu.
enum AddMenuRow<'a> {
	/// A group's header: the raw group key (`None` for an entry without a
	/// group, i.e. a built-in from before the category grouping) and its
	/// display label.
	Header {
		/// The group key, the same value [`EffectEntry::group`] holds.
		key: Option<&'a str>,
		/// The localized label.
		label: String,
	},
	/// One addable effect.
	Effect(&'a EffectEntry),
}

/// Flattens `entries` — in the order the engine yields them, groups
/// contiguous and sorted — into header + entry rows.
fn add_menu_rows(entries: &[EffectEntry]) -> Vec<AddMenuRow<'_>> {
	let mut rows = Vec::with_capacity(entries.len() + 8);
	let mut last: Option<Option<&str>> = None;
	for entry in entries {
		let group = entry.group.as_deref();
		if last != Some(group) {
			last = Some(group);
			rows.push(AddMenuRow::Header {
				key: group,
				label: match group {
					Some(group) => group_label(group),
					None => crate::i18n::tr("effect_library.group.builtin").to_string(),
				},
			});
		}
		rows.push(AddMenuRow::Effect(entry));
	}
	rows
}

/// The inspector implements no focused-panel commands: everything falls
/// through to the shell's global handler.
impl<E: AppEngine> PanelCommandHandler for InspectorPanel<E> {}

impl<E: AppEngine> Render for InspectorPanel<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let mut root = div()
			.id("inspector-panel")
			.size_full()
			.flex()
			.flex_col()
			// Any click inside the panel makes it the focused panel (the
			// dock re-emits this as `DockEvent::PanelFocused`, which the
			// shell uses to route focused-panel commands).
			.on_mouse_down(MouseButton::Left, {
				cx.listener(|_this, _event: &gpui::MouseDownEvent, _window, cx| {
					cx.emit(PanelEvent::Focused);
				})
			})
			// The stack widget has no right-click handling of its own; the
			// panel opens a menu targeting the first effect card (the stack
			// has no per-card pointer context yet).
			.on_mouse_down(MouseButton::Right, {
				cx.listener(|this, event: &gpui::MouseDownEvent, _window, cx| {
					let target = this
						.engine
						.read(cx)
						.effects()
						.into_iter()
						.find(|effect| effect.kind() == EffectCardKind::Effect)
						.map(|effect| (effect.id(), effect.is_enabled(), effect.is_removable()));
					this.context_effect = target;
					if let Some((_, enabled, removable)) = target {
						this.context_menu.show(
							event.position,
							stack_card_menu(enabled, removable),
							cx,
						);
					}
				})
			});
		root = root.child(self.stack.clone());
		// The add-effect menu sits below the stack while an add is
		// pending. It only makes sense while the engine has a stack
		// target (a clip that can host effects).
		if let Some(index) = self.pending_add {
			if self.engine.read(cx).target_label().is_some() {
				root = root.child(self.render_add_menu(index, colors.as_ref(), cx));
			} else {
				self.pending_add = None;
			}
		}
		// The right-click popup renders anchored above the panel.
		root.child(self.context_menu.widget())
	}
}

impl<E: AppEngine> EventEmitter<PanelEvent> for InspectorPanel<E> {}

impl<E: AppEngine> EventEmitter<ContextMenuTriggered> for InspectorPanel<E> {}

impl<E: AppEngine> DockPanel for InspectorPanel<E> {
	fn panel_id(&self) -> gpui::dock::PanelId {
		INSPECTOR
	}

	fn title(&self, _cx: &App) -> SharedString {
		crate::i18n::tr("panel.inspector").into()
	}

	fn tab_content(&self, _cx: &App) -> AnyElement {
		div()
			.child(crate::i18n::tr("panel.inspector"))
			.into_any_element()
	}
}

// ---------------------------------------------------------------------------
// Context menu — an effect card's right-click menu: enable/disable and
// remove drive the same `EffectStackEvent`s the card widgets emit; rename
// and properties are placeholders until the dialogs land.
// ---------------------------------------------------------------------------

/// Local (non-registry) item ids of the inspector's context menu.
const LOCAL_ENABLE: usize = 2501;
const LOCAL_REMOVE: usize = 2502;
const LOCAL_RENAME: usize = 2503;
const LOCAL_PROPERTIES: usize = 2504;

/// The effect-card context menu. `enabled` picks the Enable/Disable label;
/// `removable` gates the Remove entry.
pub(crate) fn stack_card_menu(enabled: bool, removable: bool) -> Menu {
	use crate::i18n::tr;
	let enable_label = if enabled {
		tr("inspector.context.disable")
	} else {
		tr("inspector.context.enable")
	};
	let mut remove = MenuItem::new(LOCAL_REMOVE, tr("inspector.context.remove"));
	if !removable {
		remove = remove.disabled();
	}
	Menu::new(vec![
		MenuItem::new(LOCAL_ENABLE, enable_label),
		remove.separated(),
		MenuItem::new(LOCAL_RENAME, tr("inspector.context.rename")),
		MenuItem::new(LOCAL_PROPERTIES, tr("menu.context.properties")),
	])
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The enable entry flips its label with the card state, and `removable`
	/// gates only the remove entry.
	#[test]
	fn stack_card_menu_flips_label_and_gates_remove() {
		for enabled in [true, false] {
			for removable in [true, false] {
				let menu = stack_card_menu(enabled, removable);
				assert_eq!(menu.items.len(), 4);

				let enable = &menu.items[0];
				assert_eq!(enable.id, LOCAL_ENABLE);
				let expected = if enabled {
					crate::i18n::tr("inspector.context.disable")
				} else {
					crate::i18n::tr("inspector.context.enable")
				};
				assert_eq!(enable.label, expected);

				let remove = &menu.items[1];
				assert_eq!(remove.id, LOCAL_REMOVE);
				assert_eq!(remove.enabled, removable);

				assert_eq!(menu.items[2].id, LOCAL_RENAME);
				assert_eq!(menu.items[3].id, LOCAL_PROPERTIES);
			}
		}
	}

	/// The add menu flattens the engine's table into one header per group,
	/// labelled through the same resolution the effect library uses, with
	/// the entries keeping the engine's (grouped, sorted) order.
	#[test]
	fn add_menu_rows_emit_one_header_per_group() {
		let entries = vec![
			EffectEntry {
				type_id: "oak:first".to_string(),
				name: "First".to_string(),
				group: Some("color".to_string()),
			},
			EffectEntry {
				type_id: "oak:second".to_string(),
				name: "Second".to_string(),
				group: Some("color".to_string()),
			},
			EffectEntry {
				type_id: "ofx:plugin".to_string(),
				name: "Plugin".to_string(),
				group: Some("Filter".to_string()),
			},
			EffectEntry {
				type_id: "oak:ungrouped".to_string(),
				name: "Ungrouped".to_string(),
				group: None,
			},
		];
		let shape: Vec<String> = add_menu_rows(&entries)
			.iter()
			.map(|row| match row {
				AddMenuRow::Header { key, label } => {
					format!("header:{}:{label}", key.unwrap_or("-"))
				}
				AddMenuRow::Effect(entry) => format!("effect:{}", entry.type_id),
			})
			.collect();
		assert_eq!(
			shape,
			vec![
				format!("header:color:{}", group_label("color")),
				"effect:oak:first".to_string(),
				"effect:oak:second".to_string(),
				format!("header:Filter:{}", group_label("Filter")),
				"effect:ofx:plugin".to_string(),
				format!(
					"header:-:{}",
					crate::i18n::tr("effect_library.group.builtin")
				),
				"effect:oak:ungrouped".to_string(),
			]
		);
	}
}
