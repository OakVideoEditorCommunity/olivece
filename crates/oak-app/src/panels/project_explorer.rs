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

//! The material bin panel (项目): the `ProjectExplorer` widget over the
//! engine's project data.

use std::process::Command;

use gpui::colors::DefaultColors;
use gpui::dock::{DockPanel, PanelEvent};
use gpui::{
	div, px, prelude::*, AnyElement, App, ClickEvent, Context, Entity, EventEmitter, MouseButton,
	PathPromptOptions, Pixels, Point, Render, SharedString, Window,
};
use crate::oakui::component::menu::{Menu, MenuItem};
use gpui_widgets::project_explorer::{ProjectExplorer, ProjectExplorerEvent};
use gpui_widgets::tooltip::tooltip_view;

use crate::actions::ActionId;
use crate::oakui::component::menu::{ContextMenuHandle, ContextMenuTriggered};
use crate::oakui::component::menu;
use crate::oakui::AppEngine;
use crate::panels::commands::PanelCommandHandler;
use crate::panels::ids::PROJECT;

/// The material bin panel.
pub struct ProjectExplorerPanel<E: AppEngine> {
	explorer: Entity<ProjectExplorer<E>>,
	engine: Entity<E>,
	/// The right-click context menu.
	context_menu: ContextMenuHandle,
	/// The entry under the currently open context menu (`None` = the menu
	/// was opened on the empty area).
	context_entry: Option<u64>,
}

impl<E: AppEngine> ProjectExplorerPanel<E> {
	/// Builds the explorer over `engine`'s project data.
	pub fn new(engine: Entity<E>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let explorer = cx.new(|cx| ProjectExplorer::new(1, engine.clone(), window, cx));
		let context_menu = ContextMenuHandle::new(Self::on_local_menu_item, window, cx);
		cx.subscribe(
			&explorer,
			|this, _explorer, event: &ProjectExplorerEvent, cx| match event {
				ProjectExplorerEvent::OpenRequested { id, .. } => {
					// A sequence entry opens in the timeline (the
					// double-click on a sequence acts like opening it);
					// footage entries select into the source viewer.
					if this.engine.read(cx).entry_is_sequence(*id) {
						this.engine
							.update(cx, |engine, cx| engine.open_sequence_id(*id, cx));
					} else {
						this.engine
							.update(cx, |engine, cx| engine.select_item(*id, cx));
					}
				}
				ProjectExplorerEvent::FileDropRequested { paths, .. } => {
					// Drag-and-drop import: probe and add each dropped file
					// (the first failure is logged after the rest run).
					let mut first_error = None;
					for path in paths {
						if let Err(err) =
							this.engine
								.update(cx, |engine, cx| engine.import_footage(path.clone(), cx))
						{
							first_error.get_or_insert(err);
						}
					}
					if let Some(err) = first_error {
						println!("[project explorer] import failed: {err}");
					}
				}
				ProjectExplorerEvent::ContextMenuRequested { id, position, .. } => {
					this.open_context_menu(*id, *position, cx);
				}
				other => println!("[project explorer] request: {other:?}"),
			},
		)
		.detach();

		Self {
			explorer,
			engine,
			context_menu,
			context_entry: None,
		}
	}

	/// Opens the context menu for `id` (`None` = the empty area) at
	/// `position`.
	fn open_context_menu(
		&mut self,
		id: Option<u64>,
		position: Point<Pixels>,
		cx: &mut Context<Self>,
	) {
		self.context_entry = id;
		let menu = match id {
			None => blank_menu(),
			Some(id) => {
				if self.engine.read(cx).entry_path(id).is_some() {
					let proxy = self.engine.read(cx).proxy_row(id);
					footage_menu(true, proxy.as_ref())
				} else if self.engine.read(cx).entry_is_sequence(id) {
					sequence_menu()
				} else {
					entry_menu()
				}
			}
		};
		self.context_menu.show(position, menu, cx);
	}

	/// Handles the panel's local (non-registry) context-menu items.
	fn on_local_menu_item(&mut self, item: usize, cx: &mut Context<Self>) {
		match item {
			LOCAL_REVEAL_IN_FINDER => {
				let path = self
					.context_entry
					.and_then(|id| self.engine.read(cx).entry_path(id));
				if let Some(path) = path {
					reveal_in_finder(&path);
				}
			}
			LOCAL_REPLACE_FOOTAGE => {
				let Some(id) = self.context_entry else {
					return;
				};
				let receiver = cx.prompt_for_paths(PathPromptOptions {
					files: true,
					directories: false,
					multiple: false,
					prompt: Some(crate::i18n::tr("project.context.replace_footage").into()),
				allowed_extensions: Vec::new(),
				});
				cx.spawn(async move |this, cx| {
					if let Ok(Ok(Some(paths))) = receiver.await {
						if let Some(path) = paths.into_iter().next() {
							let _ = this.update(cx, |this, cx| {
								if let Err(err) = this.engine.update(cx, |engine, cx| {
									engine.replace_footage(id, path.clone(), cx)
								}) {
									println!("[project explorer] replace failed: {err}");
								}
							});
						}
					}
				})
				.detach();
			}
			LOCAL_RENAME => {
				let Some(id) = self.context_entry else {
					return;
				};
				cx.emit(RenameRequested(id));
			}
			LOCAL_DELETE => {
				let Some(id) = self.context_entry else {
					return;
				};
				cx.emit(DeleteRequested(id));
			}
			LOCAL_OPEN_IN_NEW_TAB => {
				println!("[project explorer] menu action {item} (not implemented yet)");
			}
			LOCAL_PROPERTIES => {
				let Some(id) = self.context_entry else {
					return;
				};
				if self.engine.read(cx).entry_is_sequence(id) {
					cx.emit(SequencePropertiesRequested(id));
				} else {
					println!("[project explorer] properties for non-sequence entry {id} (not implemented yet)");
				}
			}
			LOCAL_EXPORT_SEQUENCE => {
				let Some(id) = self.context_entry else {
					return;
				};
				cx.emit(ExportSequenceRequested(id));
			}
			LOCAL_PROXY_GENERATE | LOCAL_PROXY_USE | LOCAL_PROXY_REVEAL | LOCAL_PROXY_DELETE => {
				let Some(id) = self.context_entry else {
					return;
				};
				match item {
					LOCAL_PROXY_GENERATE => {
						if let Err(err) = self.engine.update(cx, |engine, cx| {
							engine.proxy_generate(id, cx)
						}) {
							println!("[project explorer] proxy generate failed: {err}");
						}
					}
					LOCAL_PROXY_USE => {
						let enabled = self
							.engine
							.read(cx)
							.proxy_row(id)
							.is_some_and(|row| row.enabled);
						self.engine.update(cx, |engine, cx| {
							engine.proxy_set_enabled(id, !enabled, cx)
						});
					}
					LOCAL_PROXY_REVEAL => {
						self.engine.read(cx).proxy_reveal(id);
					}
					LOCAL_PROXY_DELETE => {
						self.engine.update(cx, |engine, cx| engine.proxy_delete(id, cx));
					}
					_ => {}
				}
			}
			_ => {
				println!("[project explorer] unhandled local menu item {item}");
			}
		}
	}
}

/// The project bin implements no focused-panel commands: everything falls
/// through to the shell's global handler.
impl<E: AppEngine> PanelCommandHandler for ProjectExplorerPanel<E> {}

impl<E: AppEngine> Render for ProjectExplorerPanel<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();

		// The panel title row, per the design's panel headers: the
		// widget below only shows the bare tree/icon view toggles, so
		// without this row the panel reads as anonymous. The trailing 新建
		// 序列 button mirrors the blank-area context menu's new-sequence
		// item (and the File > New > Sequence… action).
		let new_sequence_button = div()
			.id("project-new-sequence")
			.debug_selector(|| "project-new-sequence".into())
			.px_2()
			.py_0p5()
			.rounded_sm()
			.flex()
			.items_center()
			.cursor_pointer()
			.text_color(colors.text)
			.text_xs()
			.hover(|style| style.bg(colors.selected))
			.tooltip(move |window, cx| {
				tooltip_view(crate::i18n::tr("project.new_sequence").into(), window, cx)
			})
			.on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
				cx.emit(NewSequenceRequested);
			}))
			.child(crate::i18n::tr("project.new_sequence"));
		let header = div()
			.flex()
			.items_center()
			.h(px(28.0))
			.flex_shrink_0()
			.px_2()
			.border_b_1()
			.border_color(colors.border)
			.bg(colors.container)
			.text_sm()
			.text_color(colors.text)
			.child(div().flex_1().child(crate::i18n::tr("panel.project")))
			.child(new_sequence_button);
		div()
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
			.child(header)
			.child(
				div()
					.flex_1()
					.min_h_0()
					.child(self.explorer.clone()),
			)
			// The right-click popup renders anchored above the panel.
			.child(self.context_menu.widget())
	}
}

impl<E: AppEngine> EventEmitter<PanelEvent> for ProjectExplorerPanel<E> {}

impl<E: AppEngine> EventEmitter<ContextMenuTriggered> for ProjectExplorerPanel<E> {}

/// The project explorer asked the shell to open the sequence properties
/// dialog for the given sequence entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SequencePropertiesRequested(pub u64);

impl<E: AppEngine> EventEmitter<SequencePropertiesRequested> for ProjectExplorerPanel<E> {}

/// The project explorer asked the shell to open the new-sequence dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewSequenceRequested;

impl<E: AppEngine> EventEmitter<NewSequenceRequested> for ProjectExplorerPanel<E> {}

/// The project explorer asked the shell to rename entry `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameRequested(pub u64);

impl<E: AppEngine> EventEmitter<RenameRequested> for ProjectExplorerPanel<E> {}

/// The project explorer asked the shell to delete entry `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteRequested(pub u64);

impl<E: AppEngine> EventEmitter<DeleteRequested> for ProjectExplorerPanel<E> {}

/// The project explorer asked the shell to open the 导出序列 dialog for
/// the given sequence entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportSequenceRequested(pub u64);

impl<E: AppEngine> EventEmitter<ExportSequenceRequested> for ProjectExplorerPanel<E> {}

impl<E: AppEngine> DockPanel for ProjectExplorerPanel<E> {
	fn panel_id(&self) -> gpui::dock::PanelId {
		PROJECT
	}

	fn title(&self, _cx: &App) -> SharedString {
		crate::i18n::tr("panel.project").into()
	}

	fn tab_content(&self, _cx: &App) -> AnyElement {
		div()
			.child(crate::i18n::tr("panel.project"))
			.into_any_element()
	}
}

// ---------------------------------------------------------------------------
// Context menus — the Rust counterpart of the C++
// `ProjectExplorer::show_context_menu` (`app/widget/projectexplorer/`).
// ---------------------------------------------------------------------------

/// Local (non-registry) item ids of the project explorer's context menus.
const LOCAL_OPEN_IN_NEW_TAB: usize = 2201;
const LOCAL_OPEN_IN_NEW_WINDOW: usize = 2202;
const LOCAL_REVEAL_IN_FINDER: usize = 2203;
const LOCAL_REPLACE_FOOTAGE: usize = 2204;
const LOCAL_PROXY_GENERATE: usize = 2205;
const LOCAL_PROXY_USE: usize = 2206;
const LOCAL_PROXY_REVEAL: usize = 2207;
const LOCAL_PROXY_DELETE: usize = 2208;
const LOCAL_RENAME: usize = 2209;
const LOCAL_DELETE: usize = 2210;
const LOCAL_PROPERTIES: usize = 2211;
const LOCAL_EXPORT_SEQUENCE: usize = 2212;

/// The proxy submenu (shared shape with the timeline's): enable state
/// follows the footage's proxy fields (the C++ project explorer gates
/// Generate on a video stream, Use/Reveal/Delete on the proxy path); the
/// settings entry is the real registry action. `row` is `None` when the
/// entry is not footage — every entry but the settings stays disabled.
fn proxy_submenu(row: Option<&crate::oakui::engine::ProxyFootageRow>) -> Menu {
	let can_generate = row.is_some_and(|row| row.can_generate);
	let enabled = row.is_some_and(|row| row.enabled);
	let has_proxy = row.is_some_and(|row| row.has_proxy);
	let mut generate =
		MenuItem::new(LOCAL_PROXY_GENERATE, crate::i18n::tr("timeline.context.generate_proxy"));
	if !can_generate {
		generate = generate.disabled();
	}
	let mut use_proxy = MenuItem::new(LOCAL_PROXY_USE, crate::i18n::tr("timeline.context.use_proxy"))
		.with_checked(enabled);
	if row.is_none() {
		use_proxy = use_proxy.disabled();
	}
	let mut reveal =
		MenuItem::new(LOCAL_PROXY_REVEAL, crate::i18n::tr("timeline.context.reveal_proxy"));
	if !has_proxy {
		reveal = reveal.disabled();
	}
	let mut delete =
		MenuItem::new(LOCAL_PROXY_DELETE, crate::i18n::tr("timeline.context.delete_proxy"));
	if !has_proxy {
		delete = delete.disabled();
	}
	Menu::new(vec![
		generate,
		use_proxy,
		reveal,
		delete,
		menu::action_item(ActionId::ProxySettings).separated(),
	])
}

/// The empty-area context menu: New + Import.
pub(crate) fn blank_menu() -> Menu {
	Menu::new(vec![
		MenuItem::new(0, crate::i18n::tr("project.context.new"))
			.with_submenu(Menu::new(menu::new_section())),
		menu::action_item(ActionId::Import),
	])
}

/// A footage entry's context menu: reveal + replace, the proxy submenu,
/// then rename / delete / properties. `proxy` carries the entry's proxy
/// state so the submenu enables Generate/Use/Reveal/Delete correctly.
pub(crate) fn footage_menu(
	reveal_enabled: bool,
	proxy: Option<&crate::oakui::engine::ProxyFootageRow>,
) -> Menu {
	let mut reveal =
		MenuItem::new(LOCAL_REVEAL_IN_FINDER, crate::i18n::tr("project.context.reveal_in_finder"));
	if !reveal_enabled {
		reveal = reveal.disabled();
	}
	Menu::new(vec![
		reveal,
		MenuItem::new(LOCAL_REPLACE_FOOTAGE, crate::i18n::tr("project.context.replace_footage"))
			.separated(),
		MenuItem::new(0, crate::i18n::tr("timeline.context.proxy"))
			.with_submenu(proxy_submenu(proxy)),
		MenuItem::new(LOCAL_RENAME, crate::i18n::tr("project.context.rename")).separated(),
		MenuItem::new(LOCAL_DELETE, crate::i18n::tr("project.context.delete")),
		MenuItem::new(LOCAL_PROPERTIES, crate::i18n::tr("menu.context.properties")).separated(),
	])
}

/// A non-footage entry's context menu (folder / sequence): open-in-new-tab,
/// then rename / delete / properties. Open-in-new-tab and the window
/// variant are pending the sequence tabs.
pub(crate) fn entry_menu() -> Menu {
	let open_tab = MenuItem::new(
		LOCAL_OPEN_IN_NEW_TAB,
		crate::i18n::tr("project.context.open_in_new_tab"),
	)
	.disabled();
	let open_window = MenuItem::new(
		LOCAL_OPEN_IN_NEW_WINDOW,
		crate::i18n::tr("project.context.open_in_new_window"),
	)
	.disabled();
	Menu::new(vec![
		open_tab,
		open_window.separated(),
		MenuItem::new(LOCAL_RENAME, crate::i18n::tr("project.context.rename")).separated(),
		MenuItem::new(LOCAL_DELETE, crate::i18n::tr("project.context.delete")),
		MenuItem::new(LOCAL_PROPERTIES, crate::i18n::tr("menu.context.properties")).separated(),
	])
}

/// A sequence entry's context menu: the generic entry items plus 导出序列
/// (the 文件 → 导出序列 dialog preselected to this sequence).
pub(crate) fn sequence_menu() -> Menu {
	let mut menu = entry_menu();
	menu.items.push(
		MenuItem::new(
			LOCAL_EXPORT_SEQUENCE,
			crate::i18n::tr("project.context.export_sequence"),
		)
		.separated(),
	);
	menu
}

/// Reveals `path` in the platform file manager (Finder on macOS, Explorer
/// on Windows, `xdg-open` on the parent directory elsewhere).
fn reveal_in_finder(path: &std::path::Path) {
	let result = if cfg!(target_os = "macos") {
		Command::new("open").arg("-R").arg(path).spawn()
	} else if cfg!(target_os = "windows") {
		Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
	} else {
		let dir = path.parent().unwrap_or(path);
		Command::new("xdg-open").arg(dir).spawn()
	};
	if let Err(err) = result {
		println!("[project explorer] reveal failed: {err}");
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The blank-area menu is New ▸ (the shared new section) + Import.
	#[test]
	fn blank_menu_is_new_and_import() {
		let menu = blank_menu();
		assert_eq!(menu.items.len(), 2);
		let new = &menu.items[0];
		assert_eq!(new.label, crate::i18n::tr("project.context.new"));
		assert_eq!(new.submenu.as_ref().unwrap().items.len(), 3);
		assert_eq!(
			menu.items[1].id,
			ActionId::Import.entry().menu_id()
		);
	}

	/// The footage menu gates only the reveal entry on `reveal_enabled`;
	/// without proxy state every proxy entry but the settings action stays
	/// disabled.
	#[test]
	fn footage_menu_gates_the_reveal_entry() {
		for reveal_enabled in [true, false] {
			let menu = footage_menu(reveal_enabled, None);
			let reveal = menu
				.items
				.iter()
				.find(|item| item.id == LOCAL_REVEAL_IN_FINDER)
				.expect("reveal entry");
			assert_eq!(reveal.enabled, reveal_enabled);

			let ids: Vec<usize> = menu.items.iter().map(|item| item.id).collect();
			assert_eq!(
				ids,
				vec![
					LOCAL_REVEAL_IN_FINDER,
					LOCAL_REPLACE_FOOTAGE,
					0, // proxy submenu header
					LOCAL_RENAME,
					LOCAL_DELETE,
					LOCAL_PROPERTIES,
				]
			);
			let proxy = &menu.items[2].submenu.as_ref().unwrap().items;
			assert_eq!(proxy.len(), 5);
			assert!(proxy[..4].iter().all(|item| !item.enabled));
			assert!(proxy[4].enabled);
		}
	}

	/// The non-footage entry menu offers the open-in-new-tab/window pair
	/// before rename/delete/properties.
	#[test]
	fn entry_menu_offers_open_in_new_tab_first() {
		let ids: Vec<usize> = entry_menu().items.iter().map(|item| item.id).collect();
		assert_eq!(
			ids,
			vec![
				LOCAL_OPEN_IN_NEW_TAB,
				LOCAL_OPEN_IN_NEW_WINDOW,
				LOCAL_RENAME,
				LOCAL_DELETE,
				LOCAL_PROPERTIES,
			]
		);
	}

	/// The sequence menu appends 导出序列 to the generic entry items.
	#[test]
	fn sequence_menu_appends_export() {
		let menu = sequence_menu();
		let ids: Vec<usize> = menu.items.iter().map(|item| item.id).collect();
		assert_eq!(
			ids,
			vec![
				LOCAL_OPEN_IN_NEW_TAB,
				LOCAL_OPEN_IN_NEW_WINDOW,
				LOCAL_RENAME,
				LOCAL_DELETE,
				LOCAL_PROPERTIES,
				LOCAL_EXPORT_SEQUENCE,
			]
		);
		let export = menu.items.last().expect("export item");
		assert_eq!(
			export.label,
			crate::i18n::tr("project.context.export_sequence")
		);
		assert!(export.enabled);
	}
}
