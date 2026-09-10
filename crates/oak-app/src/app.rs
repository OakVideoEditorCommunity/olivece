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

//! The application shell: menu bar, dock layout, status bar, modal dialogs
//! and the tick loop that drives playback, playhead sync, the audio meter
//! and the export progress.
//!
//! The shell is generic over the engine backend ([`AppEngine`]); [`run`]
//! picks the backend at startup: the real engine by default, the mock when
//! the `--mock` flag / `OAK_ENGINE=mock` env var is given or the
//! `mock-engine` cargo feature is enabled.
//!
//! Layout per the design (`design/Oak-UI设计图-主界面-标注版.png`):
//!
//! ```text
//! ┌ menu bar (文件 编辑 视图 回放 序列 窗口 工具 帮助)
//! ├─────────────────────────────────────────────────────
//! │ dock: 项目 | 素材查看器 | 序列查看器+节点编辑器 | 检查器+历史记录
//! │       (vertical split) 时间线 (full width, 31px toolbar on top)
//! ├─────────────────────────────────────────────────────
//! └ status bar: 就绪 | 缓存 | 代理 | 库写入状态 || 时间码/时长 | 帧率 | 分辨率 | 引擎
//! ```

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::oakui::component::menu::{self, Menu, MenuBar, MenuBarEntry, MenuBarEvent, MenuItem};
use crate::oakui::component::text_input::install_text_input_bindings;
use gpui::dock::{
    DockArea, DockEvent, DockLayout, DropTarget, DropZone, NodePath, PanelHandle, PanelId,
    PanelRegistry,
};
use gpui::timeline::{ClipData, ClipId, Frame, FrameRange, TimelineEvent, TimelineView, TrackData};
use gpui::{
    colors::DefaultColors, div, prelude::*, px, size, App, AsyncWindowContext, Bounds, Context,
    Entity, PathPromptOptions, Render, SharedString, Window, WindowBounds, WindowOptions,
};
use gpui_widgets::audio_meter::{AudioLevelMeter, MeterOrientation};
use gpui_widgets::dialog::progress::{progress_dialog, ProgressContent};
use gpui_widgets::dialog::{DialogButton, Modal, ModalEvent, ModalOptions};
use gpui_widgets::theme::{apply_theme, OakTheme};
use gpui_widgets::viewer::PlaybackClock;

use crate::actions::{ActionId, TimelineToolExt, Tool};
use crate::dialogs::{
    DropSequenceChoice, ExportDialogContent, PreferencesDialogContent, SequenceFormatSeed,
};
use crate::oakui::{AppEngine, ExportSession, MockEngine, Monitor, RealEngine};
use crate::panels::commands as panel_commands;
use crate::panels::effect_library::EffectLibraryPanel;
use crate::panels::history::HistoryPanel;
use crate::panels::ids::*;
use crate::panels::inspector::InspectorPanel;
use crate::panels::multicam::MulticamPanel;
use crate::panels::node_editor::NodeEditorPanel;
use crate::panels::program_viewer::ProgramViewerPanel;
use crate::panels::project_explorer::ProjectExplorerPanel;
use crate::panels::source_viewer::SourceViewerPanel;
use crate::panels::status_bar::StatusBar;
use crate::panels::timeline::{FootageDropNeedsSequence, TimelinePanel};

// Menu item ids: thin aliases over the action registry's menu ids (the
// registry is the single source; these keep the test call sites readable).
#[cfg(test)]
pub(crate) mod menu_ids {
    use crate::actions::ActionId;

    pub const NEW_PROJECT: usize = ActionId::NewProject.menu_id();
	pub const OPEN_PROJECT: usize = ActionId::OpenProject.menu_id();
	pub const EXPORT_PROJECT: usize = ActionId::SaveProject.menu_id();
	pub const CLOSE: usize = ActionId::CloseProject.menu_id();
	pub const EXPORT: usize = ActionId::Export.menu_id();
	pub const QUIT: usize = ActionId::Exit.menu_id();
	pub const IMPORT_FOOTAGE: usize = ActionId::Import.menu_id();
	pub const PROJECT_MANAGER: usize = ActionId::ProjectManager.menu_id();
	pub const OPEN_FROM_LIBRARY: usize = ActionId::OpenFromLibrary.menu_id();

	pub const UNDO: usize = ActionId::Undo.menu_id();
	pub const REDO: usize = ActionId::Redo.menu_id();
	pub const DELETE: usize = ActionId::Delete.menu_id();
	pub const RIPPLE_DELETE: usize = ActionId::RippleDelete.menu_id();

	pub const THEME_DARK: usize = ActionId::ThemeDark.menu_id();
	pub const PREFERENCES: usize = ActionId::Preferences.menu_id();

	pub const FOCUS_PROJECT: usize = ActionId::FocusProject.menu_id();
	pub const FOCUS_SOURCE_VIEWER: usize = ActionId::FocusSourceViewer.menu_id();
	pub const FOCUS_PROGRAM_VIEWER: usize = ActionId::FocusProgramViewer.menu_id();
	pub const FOCUS_NODE_EDITOR: usize = ActionId::FocusNodeEditor.menu_id();
	pub const FOCUS_INSPECTOR: usize = ActionId::FocusInspector.menu_id();
	pub const FOCUS_HISTORY: usize = ActionId::FocusHistory.menu_id();
	pub const FOCUS_TIMELINE: usize = ActionId::FocusTimeline.menu_id();
	pub const FOCUS_EFFECT_LIBRARY: usize = ActionId::FocusEffectLibrary.menu_id();
	pub const FOCUS_MULTICAM: usize = ActionId::FocusMulticam.menu_id();
	pub const RESET_DEFAULT_LAYOUT: usize = ActionId::ResetDefaultLayout.menu_id();
}

/// Modal-dialog control ids (see [`ModalEvent::control`]).
mod modal_ids {
	pub const PREFERENCES: usize = 3;
	pub const EXPORT: usize = 4;
	pub const EXPORT_PROGRESS: usize = 5;
	pub const MANAGER: usize = 6;
	pub const MANAGER_RENAME: usize = 7;
	pub const MANAGER_DELETE: usize = 8;
	pub const PROXY: usize = 9;
	/// The OFX plugin progress dialog (driven by the plugin-progress
	/// channel in the tick loop).
	pub const PLUGIN_PROGRESS: usize = 10;
	/// The action search dialog (Help > Search Actions…, the `/` key).
	pub const ACTION_SEARCH: usize = 11;
	/// The about dialog (Help > About Oak…).
	pub const ABOUT: usize = 13;
	/// The project properties dialog (File > Project Properties…).
	pub const PROJECT_PROPERTIES: usize = 12;
	/// The new-sequence dialog (File > New > Sequence…).
	pub const NEW_SEQUENCE: usize = 14;
	/// The sequence properties dialog (right-click a sequence in the
	/// project explorer > Sequence Properties).
	pub const SEQUENCE_PROPERTIES: usize = 15;
	/// The drop-onto-empty-timeline choice (probe the footage's params as
	/// the sequence's, or set them up manually).
	pub const DROP_SEQUENCE_CHOICE: usize = 16;
	/// The multicam wizard (窗口 → 多机位向导).
	pub const MULTICAM_WIZARD: usize = 17;
	/// The rename dialog (project explorer 重命名).
	pub const RENAME: usize = 18;
	/// The project export dialog (文件 → 导出工程文件…).
	pub const EXPORT_PROJECT_DFD: usize = 19;
	/// The new-project dialog (文件 → 新建项目).
	pub const NEW_PROJECT: usize = 20;
	/// The save-as dialog (文件 → 另存为).
	pub const SAVE_AS: usize = 21;
}

/// What a picked platform-dialog path should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileAction {
	ImportFootage,
	Open,
	/// Export the current project to a file (the 导出工程文件… action's
	/// target; `.ove` / `.otio` / `.fcpxml`, dispatched by extension).
	ExportProjectFile,
	/// Import a project file into the library (the manager's 导入).
	ImportProject,
	/// Export the selected library project (the manager's 导出; the row's
	/// uuid is stashed in [`OakApp::pending_export`]).
	ExportProject,
}

/// The modal currently layered on top of the shell, if any.
enum ModalState<E: AppEngine> {
	None,
	Preferences {
		modal: Entity<Modal>,
		content: Entity<PreferencesDialogContent>,
	},
	Export {
		modal: Entity<Modal>,
		content: Entity<ExportDialogContent>,
	},
	Progress {
		modal: Entity<Modal>,
		content: Entity<ProgressContent>,
	},
	/// The project manager (M13 D4).
	Manager {
		modal: Entity<Modal>,
		content: Entity<crate::manager::ProjectManager<E>>,
	},
	/// The manager's rename prompt.
	ManagerRename {
		modal: Entity<Modal>,
		content: Entity<crate::manager::NamePrompt>,
		uuid: String,
	},
	/// The manager's delete confirmation.
	ManagerDelete {
		modal: Entity<Modal>,
		uuid: String,
	},
	/// The proxy settings dialog (Tools > Proxy Settings).
	Proxy {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::ProxyDialogContent<E>>,
	},
	/// The action search dialog (Help > Search Actions…, the `/` key).
	ActionSearch {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::ActionSearchContent>,
	},
	/// The project properties dialog (File > Project Properties…; the
	/// per-project OCIO config override and the disk-cache location).
	ProjectProperties {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::ProjectPropertiesContent<E>>,
	},
	/// The about dialog (Help > About Oak…; static content).
	About {
		modal: Entity<Modal>,
	},
	/// The new-sequence dialog (File > New > Sequence…).
	NewSequence {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::NewSequenceContent<E>>,
	},
	/// The sequence properties dialog (project-explorer context menu >
	/// Sequence Properties).
	SequenceProperties {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::SequencePropertiesContent<E>>,
	},
	/// The drop-onto-empty-timeline choice (拖入素材到没有序列的时间轴):
	/// probe the footage's params as the sequence's, or open the manual
	/// setup dialog. The skipped drop is resumed by the shell once a
	/// sequence exists (the paused drop lives in [`OakApp::pending_drop`]).
	/// The buttons carry the decision, so the state keeps only the modal.
	DropSequenceChoice { modal: Entity<Modal> },
	/// The multicam wizard (窗口 → 多机位向导): pick the angle footage,
	/// choose the sync mode, then create the multicam sequence. The OK
	/// button runs the sync + creation through the content.
	MulticamWizard {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::MulticamWizardContent<E>>,
	},
	/// The project-explorer rename dialog (right-click → 重命名): the
	/// content's field is prefilled with the entry's current name.
	EntryRename {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::RenameContent>,
		entry_id: u64,
	},
	/// The project export dialog (文件 → 导出工程文件…): choose the
	/// project format (OTIO / OVE / FCPXML) + the output path.
	ExportProjectDfd {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::ExportProjectDialogContent>,
	},
	/// The new-project dialog (文件 → 新建项目): name + sequence format.
	NewProject {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::NewProjectContent>,
	},
	/// The save-as dialog (文件 → 另存为): the copy's name.
	SaveAs {
		modal: Entity<Modal>,
		content: Entity<crate::dialogs::RenameContent>,
	},
}

/// A running export: the session the tick loop drains for progress.
struct ExportRun {
	session: ExportSession,
}

impl<E: AppEngine> ModalState<E> {
	/// The modal entity currently shown, if any.
	fn modal_entity(&self) -> Option<Entity<Modal>> {
		match self {
			ModalState::None => None,
			ModalState::Preferences { modal, .. }
			| ModalState::Export { modal, .. }
			| ModalState::Progress { modal, .. }
			| ModalState::Manager { modal, .. }
			| ModalState::ManagerRename { modal, .. }
			| ModalState::ManagerDelete { modal, .. }
			| ModalState::Proxy { modal, .. }
			| ModalState::ActionSearch { modal, .. }
			| ModalState::ProjectProperties { modal, .. }
			| ModalState::About { modal }
			| ModalState::NewSequence { modal, .. }
			| ModalState::SequenceProperties { modal, .. }
			| ModalState::DropSequenceChoice { modal, .. }
			| ModalState::MulticamWizard { modal, .. }
			| ModalState::EntryRename { modal, .. } => Some(modal.clone()),
			| ModalState::ExportProjectDfd { modal, .. } => Some(modal.clone()),
			| ModalState::NewProject { modal, .. } => Some(modal.clone()),
			| ModalState::SaveAs { modal, .. } => Some(modal.clone()),
		}
	}
}

/// The panel registry: string keys for layout persistence, and the ability
/// to rebuild any panel from its key.
struct AppPanelRegistry<E: AppEngine> {
	engine: Entity<E>,
	source_clock: Entity<E::Clock>,
	program_clock: Entity<E::Clock>,
}

impl<E: AppEngine> PanelRegistry for AppPanelRegistry<E> {
	fn panel_key(&self, id: gpui::dock::PanelId) -> Option<String> {
		Some(
			match id {
				PROJECT => "project",
				SOURCE_VIEWER => "source-viewer",
				PROGRAM_VIEWER => "program-viewer",
				NODE_EDITOR => "node-editor",
				INSPECTOR => "inspector",
				HISTORY => "history",
				TIMELINE => "timeline",
				EFFECT_LIBRARY => "effect-library",
				MULTICAM => "multicam",
				_ => return None,
			}
			.to_string(),
		)
	}

	fn build_panel(&self, key: &str, window: &mut Window, cx: &mut App) -> Option<PanelHandle> {
		match key {
			"project" => Some(PanelHandle::new(
				cx.new(|cx| ProjectExplorerPanel::new(self.engine.clone(), window, cx)),
				cx,
			)),
			"source-viewer" => Some(PanelHandle::new(
				cx.new(|cx| {
					SourceViewerPanel::new(
						self.engine.clone(),
						self.source_clock.clone(),
						window,
						cx,
					)
				}),
				cx,
			)),
			"program-viewer" => Some(PanelHandle::new(
				cx.new(|cx| {
					let meter = cx.new(|cx| {
						AudioLevelMeter::new(30, self.engine.clone(), window, cx)
							.with_orientation(MeterOrientation::Vertical)
					});
					ProgramViewerPanel::new(
						self.engine.clone(),
						self.program_clock.clone(),
						meter,
						window,
						cx,
					)
				}),
				cx,
			)),
			"node-editor" => Some(PanelHandle::new(
				cx.new(|cx| NodeEditorPanel::new(self.engine.clone(), window, cx)),
				cx,
			)),
			"inspector" => Some(PanelHandle::new(
				cx.new(|cx| InspectorPanel::new(self.engine.clone(), window, cx)),
				cx,
			)),
			"history" => Some(PanelHandle::new(
				cx.new(|cx| HistoryPanel::new(self.engine.clone(), window, cx)),
				cx,
			)),
			"effect-library" => Some(PanelHandle::new(
				cx.new(|cx| EffectLibraryPanel::new(self.engine.clone(), window, cx)),
				cx,
			)),
			"timeline" => Some(PanelHandle::new(
				cx.new(|cx| {
					let timeline =
						cx.new(|cx| TimelineView::new(self.engine.clone(), window, cx).zoom(2.0));
					TimelinePanel::new(self.engine.clone(), timeline, window, cx)
				}),
				cx,
			)),
			"multicam" => Some(PanelHandle::new(
				cx.new(|cx| {
					MulticamPanel::new(self.engine.clone(), self.program_clock.clone(), window, cx)
				}),
				cx,
			)),
			_ => None,
		}
	}
}

/// The application root view.
pub struct OakApp<E: AppEngine> {
	engine: Entity<E>,
	program_clock: Entity<E::Clock>,
	timeline: Entity<TimelineView<E>>,
	meter: Entity<AudioLevelMeter<E>>,
	menu_bar: Entity<MenuBar>,
	dock: Entity<DockArea>,
	status_bar: Entity<StatusBar<E>>,
	/// Whether the dark theme is active (toggles via 视图 → 主题).
	dark: bool,
	/// The modal currently shown on top of the shell, if any.
	modal: ModalState<E>,
	/// The shell's own focus handle: the root element tracks it so the
	/// action dispatch layer (the root's `on_action` listeners) sits on the
	/// dispatch path even before any panel takes focus.
	shell_focus: gpui::FocusHandle,
	/// The running export session, if any.
	export: Option<ExportRun>,
	/// A footage drop paused on the empty-timeline drop choice: kept while
	/// its modal (the choice dialog, or the new-sequence dialog it spawns)
	/// is up; performed when a sequence exists. `None` outside the flow.
	pending_drop: Option<FootageDropNeedsSequence>,
	/// Whether the progress modal currently on screen is the OFX plugin
	/// progress dialog (as opposed to the export progress). Guards
	/// [`poll_plugin_progress`] from hijacking the export's bar.
	plugin_progress_open: bool,
	/// The receiving half of the OFX plugin-progress channel (the sending
	/// half is registered into the oakplugin progress suite by
	/// [`crate::oakui::ofx::set_progress_tx`]). Drained in the tick loop to
	/// drive the progress dialog.
	plugin_progress_rx: Mutex<mpsc::Receiver<crate::oakui::ofx::PluginProgressEvent>>,
	/// The library row pending an export save dialog (manager 导出).
	pending_export: Option<String>,
	/// The suggested file stem for the export-project system save dialog.
	export_suggested_name: String,
	/// The panel that most recently took focus (the target of
	/// [`Route::FocusedPanel`](crate::actions::Route::FocusedPanel)
	/// commands; `None` before any panel is focused).
	focused_panel: Option<gpui::dock::PanelId>,
	/// The eight dock panels, kept so focused-panel commands can be routed
	/// to the right entity (the dock only hands back type-erased handles).
	panels: ShellPanels<E>,
	/// The active timeline tool (the Tools menu's exclusive group; the
	/// timeline toolbar keeps its own visual selection for now).
	active_tool: Tool,
	/// The tool the timeline widget last reported; app tool follows it via
	/// the observe hook on [`Self::timeline`] (toolbar ↔ menu stay in sync).
	last_timeline_tool: gpui::timeline::TimelineTool,
	/// 回放 → 循环播放 is on (the C++ `Loop` config flag; playback looping
	/// itself is a transport gap, so this only drives the checkmark).
	loop_playback: bool,
	/// 视图 → Toggle Show All is on (placeholder state for the checkmark).
	show_all: bool,
	/// 视图 → Full Screen is on (placeholder state for the checkmark).
	full_screen: bool,
}

/// The dock panels the shell builds up front, kept for focused-panel command
/// routing (the dock area only exposes type-erased handles).
struct ShellPanels<E: AppEngine> {
	project: Entity<ProjectExplorerPanel<E>>,
	source_viewer: Entity<SourceViewerPanel<E>>,
	program_viewer: Entity<ProgramViewerPanel<E>>,
	node_editor: Entity<NodeEditorPanel<E>>,
	inspector: Entity<InspectorPanel<E>>,
	history: Entity<HistoryPanel<E>>,
	timeline: Entity<TimelinePanel<E>>,
	effect_library: Entity<EffectLibraryPanel<E>>,
	multicam: Entity<MulticamPanel<E>>,
}

impl<E: AppEngine> OakApp<E> {
	/// Builds the whole shell. `initial_path` (a CLI argument) is opened
	/// after the layout is up.
	pub fn new(window: &mut Window, initial_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
		// The persisted theme (config `Theme`) wins; the app defaults to dark.
		let dark = crate::oakui::real::theme_is_dark();
		if dark {
			apply_theme(cx, &OakTheme::olive_dark());
		} else {
			apply_theme(cx, &OakTheme::olive_light());
		}
		crate::oakui::icons::init(cx);

		// --- engine and shared state ---------------------------------------
		let engine = cx.new(|cx| E::create(cx));
		let source_clock = engine.read(cx).source_clock().clone();
		let program_clock = engine.read(cx).program_clock().clone();
		let timeline = cx.new(|cx| TimelineView::new(engine.clone(), window, cx).zoom(2.0));
		// The toolbar and the Tools menu both drive the widget's tool; the
		// widget is the single source of truth, and any change (from either
		// side) is mirrored into the app tool + menu checkmark here.
		cx.observe(&timeline, |this, timeline, cx| {
			let tool = timeline.read(cx).tool();
			if tool != this.last_timeline_tool {
				this.last_timeline_tool = tool;
				this.active_tool = tool.app_tool();
				this.rebuild_menu_bar(cx);
			}
		});
		// M12 P4: install the waveform decorator when the engine provides
		// a waveform cache.
		if let Some(cache) = engine.read(cx).waveform_cache() {
			let decorator: std::sync::Arc<
				std::sync::RwLock<dyn gpui::timeline::clip::ClipDecorator>,
			> = std::sync::Arc::new(std::sync::RwLock::new(
				crate::oakui::waveform::OakClipDecorator { cache },
			));
			timeline.update(cx, |view, _| view.set_clip_decorator(decorator));
		}
		let meter = cx.new(|cx| {
			AudioLevelMeter::new(3, engine.clone(), window, cx)
				.with_orientation(MeterOrientation::Vertical)
		});

		// --- menu bar ------------------------------------------------------
		// The menus are generated from the action registry; the dynamic
		// checkmarks (theme, tool, snapping, loop, …) come from this state.
		let menu_bar = cx.new(|cx| MenuBar::new(1, make_menus(MenuState::new(dark)), window, cx));
		cx.subscribe(
			&menu_bar,
			|this, _menu: Entity<MenuBar>, event: &MenuBarEvent, cx| {
				if let MenuBarEvent::Triggered { item, .. } = event {
					this.on_menu(*item, cx);
				}
			},
		)
		.detach();

		// --- keyboard map ---------------------------------------------------
		// Load the persisted custom shortcut overrides (`<config>/shortcuts`)
		// *before* binding, so a user's shortcuts file wins over the registry
		// defaults. The bindings below are built from the effective keys.
		crate::actions::load_custom_shortcuts();
		// Register every action's effective key as a global binding (context
		// `None`): the keystrokes dispatch the gpui actions, which bubble to
		// the shell's `on_action` listeners — the same path the menu clicks
		// take through `on_menu`.
		cx.bind_keys(crate::actions::key_bindings());
		// The text-input editing keys (Backspace/Delete/arrows/Home/End/
		// select-all …) are scoped to the `EditableText` key context; without
		// them every field accepts IME text but ignores its editing keys.
		install_text_input_bindings(cx);

		// --- dock ----------------------------------------------------------
		let dock = cx.new(|cx| {
			DockArea::new(cx).with_registry(Arc::new(AppPanelRegistry {
				engine: engine.clone(),
				source_clock: source_clock.clone(),
				program_clock: program_clock.clone(),
			}))
		});

		let project = cx.new(|cx| ProjectExplorerPanel::new(engine.clone(), window, cx));
		let effect_library = cx.new(|cx| EffectLibraryPanel::new(engine.clone(), window, cx));
		let source_viewer =
			cx.new(|cx| SourceViewerPanel::new(engine.clone(), source_clock.clone(), window, cx));
		let program_viewer = cx.new(|cx| {
			ProgramViewerPanel::new(
				engine.clone(),
				program_clock.clone(),
				meter.clone(),
				window,
				cx,
			)
		});
		let node_editor = cx.new(|cx| NodeEditorPanel::new(engine.clone(), window, cx));
		let inspector = cx.new(|cx| InspectorPanel::new(engine.clone(), window, cx));
		let history = cx.new(|cx| HistoryPanel::new(engine.clone(), window, cx));
		let timeline_panel =
			cx.new(|cx| TimelinePanel::new(engine.clone(), timeline.clone(), window, cx));
		let multicam_panel =
			cx.new(|cx| MulticamPanel::new(engine.clone(), program_clock.clone(), window, cx));

		// Keep the panel entities for focused-panel command routing (the dock
		// only hands back type-erased handles).
		let panels = ShellPanels {
			project: project.clone(),
			source_viewer: source_viewer.clone(),
			program_viewer: program_viewer.clone(),
			node_editor: node_editor.clone(),
			inspector: inspector.clone(),
			history: history.clone(),
			timeline: timeline_panel.clone(),
			effect_library: effect_library.clone(),
			multicam: multicam_panel.clone(),
		};

		// Wire each panel's right-click menu: registry-backed items come
		// back through `ContextMenuTriggered` and are dispatched exactly
		// like menu-bar clicks. A right-click does not emit
		// `PanelEvent::Focused`, so the subscription points `focused_panel`
		// at the panel before dispatching (the C++
		// `PanelManager::currently_focused` target is the clicked panel).
		Self::wire_panel_context_menu(cx, &panels.project, PROJECT);
		Self::wire_panel_context_menu(cx, &panels.source_viewer, SOURCE_VIEWER);
		Self::wire_panel_context_menu(cx, &panels.program_viewer, PROGRAM_VIEWER);
		Self::wire_panel_context_menu(cx, &panels.node_editor, NODE_EDITOR);
		Self::wire_panel_context_menu(cx, &panels.inspector, INSPECTOR);
		Self::wire_panel_context_menu(cx, &panels.timeline, TIMELINE);

		// The project explorer's 序列属性 context item opens the sequence
		// properties dialog.
		cx.subscribe(
			&panels.project,
			|this,
			 _panel,
			 event: &crate::panels::project_explorer::SequencePropertiesRequested,
			 cx| {
				this.open_sequence_properties(event.0, cx);
			},
		)
		.detach();

		// The project explorer's 导出序列 context item opens the export
		// dialog with that sequence preselected.
		cx.subscribe(
			&panels.project,
			|this,
			 _panel,
			 event: &crate::panels::project_explorer::ExportSequenceRequested,
			 cx| {
				this.open_export_dialog(Some(event.0), cx);
			},
		)
		.detach();

		// The project explorer's 重命名 / 删除 context items operate on
		// the project's entry (rename prompts; delete removes the node).
		cx.subscribe(
			&panels.project,
			|this,
			 _panel,
			 event: &crate::panels::project_explorer::RenameRequested,
			 cx| {
				this.open_entry_rename(event.0, cx);
			},
		)
		.detach();
		cx.subscribe(
			&panels.project,
			|this,
			 _panel,
			 event: &crate::panels::project_explorer::DeleteRequested,
			 cx| {
				this.engine
					.update(cx, |engine, cx| engine.delete_entry(event.0, cx));
			},
		)
		.detach();

		// The project explorer's 新建序列 button opens the new-sequence
		// dialog (the same one File > New > Sequence… uses).
		cx.subscribe(
			&panels.project,
			|this,
			 _panel,
			 _event: &crate::panels::project_explorer::NewSequenceRequested,
			 cx| {
				this.open_new_sequence(cx);
			},
		)
		.detach();

		// A footage drop onto a timeline with no open sequence goes through
		// the probe-vs-manual choice before any sequence is created.
		cx.subscribe(
			&panels.timeline,
			|this, _panel, event: &FootageDropNeedsSequence, cx| {
				this.on_footage_drop_needs_sequence(*event, cx);
			},
		)
		.detach();

		// The viewer panels re-emit their monitor-level requests (full-screen
		// and the loop in/out range) for the shell to apply: full-screen
		// toggles the window, and the in/out/clear requests act on the shared
		// program workarea (both monitors use it).
		cx.subscribe(
			&panels.source_viewer,
			|this, _panel, event: &menu::ViewerPanelEvent, cx| {
				this.apply_viewer_panel_event(*event, cx);
			},
		)
		.detach();
		cx.subscribe(
			&panels.program_viewer,
			|this, _panel, event: &menu::ViewerPanelEvent, cx| {
				this.apply_viewer_panel_event(*event, cx);
			},
		)
		.detach();

		// Arrange the default workspace: the design's 素材查看器 | 序列查看器 |
		// 检查器 row (project bin docked on the left), node editor + history
		// as tabs, timeline full width at the bottom.
		dock.update(cx, |dock, cx| {
			dock.add_panel(PanelHandle::new(project, cx), None, cx);
			// The effect library tabs behind the project bin, per the
			// design's left-top tab group (项目 | 效果库).
			dock.add_panel(
				PanelHandle::new(effect_library, cx),
				Some(DropTarget {
					panel: Some(PROJECT),
					zone: DropZone::Center,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(source_viewer, cx),
				Some(DropTarget {
					panel: Some(PROJECT),
					zone: DropZone::Right,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(program_viewer, cx),
				Some(DropTarget {
					panel: Some(SOURCE_VIEWER),
					zone: DropZone::Right,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(node_editor, cx),
				Some(DropTarget {
					panel: Some(PROGRAM_VIEWER),
					zone: DropZone::Center,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(inspector, cx),
				Some(DropTarget {
					panel: Some(PROGRAM_VIEWER),
					zone: DropZone::Right,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(history, cx),
				Some(DropTarget {
					panel: Some(INSPECTOR),
					zone: DropZone::Center,
				}),
				cx,
			);
			dock.add_panel(
				PanelHandle::new(timeline_panel, cx),
				Some(DropTarget {
					panel: None,
					zone: DropZone::Bottom,
				}),
				cx,
			);
			// The multicam panel tabs behind the SOURCE viewer (the C++
			// default is hidden; the 窗口 menu's Focus Multicam brings it
			// forward). Sharing the source viewer's pane lets the user
			// toggle between inspecting source media and switching angles
			// in the same window-half — the multicam is NOT tabbed with
			// the node editor.
			dock.add_panel(
				PanelHandle::new(multicam_panel, cx),
				Some(DropTarget {
					panel: Some(SOURCE_VIEWER),
					zone: DropZone::Center,
				}),
				cx,
			);
		});

		// Tune the default split ratios and active tabs — the shared tuning
		// block (`tune_default_layout`) that 窗口 → 重置布局 also applies.
		let mut layout: DockLayout = dock.read(cx).layout().clone();
		tune_default_layout(&mut layout);
		dock.update(cx, |dock, cx| dock.set_layout(layout, cx));

		// --- status bar ----------------------------------------------------
		let status_bar = cx.new(|cx| StatusBar::new(engine.clone(), program_clock.clone(), cx));

		// Repaint the shell whenever the engine notifies.
		cx.observe(&engine, |this, _engine, cx| {
			cx.notify();
			let _ = this;
		})
		.detach();

		// Track the focused panel: the dock re-emits every panel's
		// `PanelEvent::Focused` as `DockEvent::PanelFocused` (and its own
		// `focus_panel` calls), so a single subscription keeps the shell's
		// focused-panel routing target up to date. Structural changes (a panel
		// closed via its tab ✕, torn off, or re-docked) refresh the 窗口 menu's
		// checkmarks, which read the dock's live visible-panel set.
		cx.subscribe(&dock, |this, _dock, event: &DockEvent, cx| match event {
			DockEvent::PanelFocused(id) => this.focused_panel = Some(*id),
			DockEvent::PanelAdded(_) | DockEvent::PanelRemoved(_) | DockEvent::PanelClosed(_) => {
				this.rebuild_menu_bar(cx);
				cx.notify();
			}
			_ => {}
		})
		.detach();

		// --- timeline events -----------------------------------------------
		// Every timeline widget request (playhead seek, trim, move, track
		// height) is applied by the engine through its backend's edit
		// commands; the playhead is routed to the program monitor.
		// `SelectionChanged` carries no payload — the selection is read from
		// the view and forwarded to the engine so the inspector's effect
		// stack can target the selected clip.
		cx.subscribe(&timeline, |this, timeline, event: &TimelineEvent, cx| {
			if matches!(event, TimelineEvent::SelectionChanged) {
				let clips: Vec<ClipId> = timeline.read(cx).selection().iter().copied().collect();
				this.engine
					.update(cx, |engine, cx| engine.set_selected_clips(clips, cx));
			}
			this.engine
				.update(cx, |engine, cx| engine.apply_timeline_event(event, cx));
		})
		.detach();

		// --- tick loop -----------------------------------------------------
		// Drives playback clocks, playhead sync, the audio meter and the
		// export progress at ~60Hz.
		let this = cx.weak_entity();
		window
			.spawn(cx, async move |cx: &mut AsyncWindowContext| loop {
				cx.background_executor()
					.timer(Duration::from_millis(16))
					.await;
				let _ = cx.update(|window, app| {
					// Multi-monitor: track which physical display the window
					// sits on so the self-managed display transform follows
					// window moves (throttled inside `poll_monitor`).
					crate::oakui::displaycolor::poll_monitor(window, app);
					if let Some(this) = this.upgrade() {
						this.update(app, |this, cx| this.tick(cx));
					}
				});
			})
			.detach();

		let shell_focus = cx.focus_handle();
		// The shell starts focused so the action dispatch layer works before
		// any panel grabs focus.
		window.focus(&shell_focus, cx);
		// OFX plugin-progress channel: the oakplugin progress suite pushes
		// (label, message, fraction) events here; the tick loop drives the
		// progress dialog.
		let (plugin_progress_tx, plugin_progress_rx) =
			mpsc::channel::<crate::oakui::ofx::PluginProgressEvent>();
		crate::oakui::ofx::set_progress_tx(plugin_progress_tx);
		let shell = Self {
			engine,
			program_clock,
			timeline,
			meter,
			menu_bar,
			dock,
			status_bar,
			dark,
			modal: ModalState::None,
			shell_focus,
			export: None,
			pending_drop: None,
			pending_export: None,
			export_suggested_name: String::new(),
			plugin_progress_open: false,
			plugin_progress_rx: Mutex::new(plugin_progress_rx),
			focused_panel: None,
			panels,
			active_tool: Tool::Pointer,
			last_timeline_tool: gpui::timeline::TimelineTool::Select,
			loop_playback: false,
			show_all: false,
			full_screen: false,
		};

		// Open the CLI-provided project once the shell is up.
		if let Some(path) = initial_path {
			shell.engine.update(cx, |engine, cx| {
				if let Err(err) = engine.open_project_path(path.clone(), cx) {
					println!("[app] failed to open {}: {err}", path.display());
				}
			});
		}

		shell
	}

	/// One animation-frame tick: advance the engine, sync the timeline
	/// playhead to the program clock, refresh the audio meter and drain the
	/// export progress events.
	fn tick(&mut self, cx: &mut Context<Self>) {
		self.engine.update(cx, |engine, cx| engine.tick(cx));
		let frame = self.program_clock.read(cx).current_frame();
		self.timeline
			.update(cx, |timeline, cx| timeline.seek(frame, cx));
		// Mirror the engine's work area into the ruler's view state (M12 P4).
		// Read every tick so undo/redo and the ruler-drag commit land on the
		// band promptly; the read is a cheap engine getter.
		let work_area = self.engine.read(cx).workarea();
		self.timeline.update(cx, |timeline, _| {
			timeline.state.work_area = work_area.map(|(s, e)| FrameRange::new(s, e))
		});
		self.meter.update(cx, |meter, cx| meter.update(cx));
		self.poll_export(cx);
		self.poll_plugin_progress(cx);
		cx.notify();
	}

	/// Drains the OFX plugin-progress channel: the first event of a render
	/// opens the progress dialog, subsequent events update the bar, and the
	/// dialog closes when the fraction reaches 1.0 (the plugin's
	/// progressEnd is not surfaced by the reporter, so completion is
	/// inferred from the fraction).
	fn poll_plugin_progress(&mut self, cx: &mut Context<Self>) {
		let mut events = Vec::new();
		while let Ok(event) = self.plugin_progress_rx.lock().unwrap().try_recv() {
			events.push(event);
		}
		if events.is_empty() {
			return;
		}
		let last = events.last().cloned().unwrap_or_default();
		// Ensure the plugin progress dialog is on screen. If another modal
		// is already up (e.g. export progress), leave it alone.
		if !self.plugin_progress_open && matches!(self.modal, ModalState::None) {
			let title = crate::i18n::tr("ofx.progress.title");
			let label = last.label.clone();
			let message = last.message.clone();
			self.spawn_modal(cx, move |window, app| {
				let (modal, content) = progress_dialog(
					modal_ids::PLUGIN_PROGRESS,
					title,
					format!("{label}\n{message}"),
					window,
					app,
				);
				ModalState::Progress { modal, content }
			});
			self.plugin_progress_open = true;
		}
		if self.plugin_progress_open {
			if let ModalState::Progress { content, .. } = &self.modal {
				let fraction = last.fraction as f32;
				content.update(cx, |content, cx| content.set_progress(fraction, cx));
			}
			// The reporter answers false after the user cancels, and the
			// fraction reaches 1.0 when the render completes; either way the
			// dialog is dismissed.
			if last.fraction >= 1.0 {
				self.modal = ModalState::None;
				self.plugin_progress_open = false;
				cx.notify();
			}
		}
	}

	/// Routes a menu click: the dynamic language items (one per discovered
	/// pack, above the registry id range) switch the language directly;
	/// everything else resolves to its registry action and dispatches
	/// through the same path the keyboard shortcuts use, so a menu click
	/// and a key press can never diverge.
	fn on_menu(&mut self, item: usize, cx: &mut Context<Self>) {
		if let Some(index) = menu::language_item_index(item) {
			let languages = crate::i18n::available_languages();
			if let Some(code) = languages.get(index) {
				let code = code.clone();
				self.switch_language(&code, cx);
			}
			return;
		}
		let Some(entry) = crate::actions::entry_for_menu_id(item) else {
			println!("[menu] unknown item {item}");
			return;
		};
		self.dispatch_action_id(entry.action, cx);
	}

	/// Subscribes the shell to a panel's right-click menu: a triggered
	/// registry item is dispatched like a menu-bar click, with the panel set
	/// as the focused panel first (a right-click does not emit
	/// `PanelEvent::Focused`).
	fn wire_panel_context_menu<P>(cx: &mut Context<Self>, panel: &Entity<P>, id: PanelId)
	where
		P: gpui::EventEmitter<menu::ContextMenuTriggered>,
	{
		cx.subscribe(
			panel,
			move |this, _panel, event: &menu::ContextMenuTriggered, cx| {
				this.focused_panel = Some(id);
				this.on_menu(event.item, cx);
			},
		)
		.detach();
	}

	/// The central action dispatch (menu clicks and keyboard shortcuts both
	/// end up here). [`Route::FocusedPanel`](crate::actions::Route::FocusedPanel)
	/// actions go to the focused panel first; whatever the panel does not
	/// implement falls through to the shell's global handler.
	fn dispatch_action_id(&mut self, action: ActionId, cx: &mut Context<Self>) {
		let entry = action.entry();
		if entry.route == crate::actions::Route::FocusedPanel
			&& self.dispatch_to_focused_panel(action, cx)
		{
			return;
		}
		self.handle_global_action(action, cx);
	}

	/// Hands `action` to the focused panel's
	/// [`PanelCommandHandler`](panel_commands::PanelCommandHandler); whether
	/// it was handled. No focused panel (or the panel declines) means the
	/// shell's global handler runs instead.
	fn dispatch_to_focused_panel(&mut self, action: ActionId, cx: &mut Context<Self>) -> bool {
		let Some(id) = self.focused_panel else {
			return false;
		};
		match id {
			PROJECT => self.panels.project.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			SOURCE_VIEWER => self.panels.source_viewer.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			PROGRAM_VIEWER => self.panels.program_viewer.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			NODE_EDITOR => self.panels.node_editor.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			INSPECTOR => self.panels.inspector.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			HISTORY => self.panels.history.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			TIMELINE => self.panels.timeline.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			EFFECT_LIBRARY => self.panels.effect_library.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			MULTICAM => self.panels.multicam.update(cx, |panel, cx| {
				panel_commands::dispatch_to(panel, action, cx)
			}),
			_ => false,
		}
	}

	/// The shell's global action handler: file dialogs, undo, view
	/// preferences, tools, transport fallbacks — and the placeholder
	/// `println!` for the actions not wired yet.
	fn handle_global_action(&mut self, action: ActionId, cx: &mut Context<Self>) {
        use crate::actions::ActionId as A;
        match action {
			// --- File ------------------------------------------------------
			A::NewProject => self.open_new_project(cx),
			A::OpenProject => self.open_file_dialog(FileAction::Open, cx),
			A::OpenFromLibrary | A::ProjectManager => self.show_project_manager(cx),
			A::Import => self.open_file_dialog(FileAction::ImportFootage, cx),
			// The 导出工程文件… dialog: choose OTIO / OVE / FCPXML + path.
			A::SaveProject => self.open_export_project_dfd(cx),
			A::SaveProjectAs => self.open_save_as(cx),
			A::CloseProject => self
				.engine
				.update(cx, |engine, cx| engine.close_project(cx)),
			A::Export => self.open_export_dialog(None, cx),
			A::Exit => cx.quit(),
			// --- Edit ------------------------------------------------------
			A::Undo => self.engine.update(cx, |engine, cx| engine.undo(cx)),
			A::Redo => self.engine.update(cx, |engine, cx| engine.redo(cx)),
			A::Delete => self.delete_timeline_selection(false, cx),
			A::RippleDelete => self.delete_timeline_selection(true, cx),
			A::SelectAll => self.select_all_clips(cx),
			A::DeselectAll => self.deselect_all_clips(cx),
			A::SplitAtPlayhead => self
				.engine
				.update(cx, |engine, cx| engine.split_at_playhead(cx)),
			A::SetInPoint => self.set_point_at_playhead(true, cx),
			A::SetOutPoint => self.set_point_at_playhead(false, cx),
			A::ClearInOut | A::ClearWorkArea => self
				.engine
				.update(cx, |engine, cx| engine.clear_workarea(cx)),
			A::Marker => self
				.engine
				.update(cx, |engine, cx| engine.add_marker_at_playhead(cx)),
			// --- View ------------------------------------------------------
			A::ThemeDark => {
				crate::oakui::real::set_theme_dark(true);
				self.apply_dark(true, cx);
			}
			A::ThemeLight => {
				crate::oakui::real::set_theme_dark(false);
				self.apply_dark(false, cx);
			}
			A::ZoomIn => self.zoom_timeline(1.25, cx),
			A::ZoomOut => self.zoom_timeline(0.8, cx),
			A::IncreaseTrackHeight => self.nudge_track_height(8.0, cx),
			A::DecreaseTrackHeight => self.nudge_track_height(-8.0, cx),
			A::ToggleShowAll => {
				self.show_all = !self.show_all;
				println!("[view] toggle show all: {} (placeholder)", self.show_all);
				self.rebuild_menu_bar(cx);
			}
			A::FullScreen => self.toggle_full_screen(cx),
			A::Preferences => self.open_preferences(cx),
			// --- Playback (the program monitor) ----------------------------
			A::PlayPause => {
				let playing = self.program_clock.read(cx).is_playing();
				let monitor = Monitor::Program;
				self.engine.update(cx, |engine, cx| {
					if playing {
						engine.pause(monitor, cx);
					} else {
						engine.play(monitor, cx);
					}
				});
			}
			A::PrevFrame => {
				let monitor = Monitor::Program;
				self.engine
					.update(cx, |engine, cx| engine.step(monitor, -1, cx));
			}
			A::NextFrame => {
				let monitor = Monitor::Program;
				self.engine
					.update(cx, |engine, cx| engine.step(monitor, 1, cx));
			}
			A::ShuttleLeft => {
				// J steps back — true reverse playback is an engine transport
				// gap (the old shortcut table did the same).
				let monitor = Monitor::Program;
				self.engine
					.update(cx, |engine, cx| engine.step(monitor, -1, cx));
			}
			A::ShuttleStop => {
				let monitor = Monitor::Program;
				self.engine
					.update(cx, |engine, cx| engine.pause(monitor, cx));
			}
			A::ShuttleRight => {
				let monitor = Monitor::Program;
				self.engine
					.update(cx, |engine, cx| engine.play(monitor, cx));
			}
			A::GoToStart => {
				let monitor = Monitor::Program;
				self.engine.update(cx, |engine, cx| {
					engine.request_frame(monitor, Frame::ZERO, cx)
				});
			}
			A::GoToEnd => {
				let monitor = Monitor::Program;
				let length = self.engine.read(cx).sequence_length();
				self.engine
					.update(cx, |engine, cx| engine.request_frame(monitor, length, cx));
			}
			A::GoToIn => {
				if let Some((start, _)) = self.engine.read(cx).workarea() {
					let monitor = Monitor::Program;
					self.engine
						.update(cx, |engine, cx| engine.request_frame(monitor, start, cx));
				}
			}
			A::GoToOut => {
				if let Some((_, end)) = self.engine.read(cx).workarea() {
					let monitor = Monitor::Program;
					self.engine
						.update(cx, |engine, cx| engine.request_frame(monitor, end, cx));
				}
			}
			A::PlayInToOut => {
				// Seek to the work area's start, then play (out-point
				// stopping is a transport gap).
				let monitor = Monitor::Program;
				if let Some((start, _)) = self.engine.read(cx).workarea() {
					self.engine
						.update(cx, |engine, cx| engine.request_frame(monitor, start, cx));
				}
				self.engine
					.update(cx, |engine, cx| engine.play(monitor, cx));
			}
			A::Loop => {
				self.loop_playback = !self.loop_playback;
				println!("[playback] loop: {} (placeholder)", self.loop_playback);
				self.rebuild_menu_bar(cx);
			}
			// --- Sequence --------------------------------------------------
			A::AddVideoTrack => {
				let kind = gpui::timeline::TrackKind::Video;
				self.engine
					.update(cx, |engine, cx| engine.add_track(kind, cx));
			}
			A::AddAudioTrack => {
				let kind = gpui::timeline::TrackKind::Audio;
				self.engine
					.update(cx, |engine, cx| engine.add_track(kind, cx));
			}
			A::RemoveTrack => self.remove_selected_track(cx),
			A::RemoveMarker => self
				.engine
				.update(cx, |engine, cx| engine.remove_marker_at_playhead(cx)),
			A::SetWorkArea => self.set_workarea_from_selection(cx),
			// --- Window ----------------------------------------------------
			// The 窗口 menu toggles each panel: clicking an open panel closes
			// it (through the dock's remove flow, the same path the tab ✕
			// takes), clicking a closed one re-opens it.
			A::FocusProject => self.toggle_panel(PROJECT, cx),
			A::FocusSourceViewer => self.toggle_panel(SOURCE_VIEWER, cx),
			A::FocusProgramViewer => self.toggle_panel(PROGRAM_VIEWER, cx),
			A::FocusNodeEditor => self.toggle_panel(NODE_EDITOR, cx),
			A::FocusInspector => self.toggle_panel(INSPECTOR, cx),
			A::FocusHistory => self.toggle_panel(HISTORY, cx),
			A::FocusTimeline => self.toggle_panel(TIMELINE, cx),
			A::FocusEffectLibrary => self.toggle_panel(EFFECT_LIBRARY, cx),
			A::FocusMulticam => self.toggle_panel(MULTICAM, cx),
			A::ResetDefaultLayout => self.reset_default_layout(cx),
			// --- Tools -----------------------------------------------------
			A::Snapping => {
				let enabled = !self.timeline.read(cx).state.snap_enabled;
				self.timeline.update(cx, |timeline, cx| {
					timeline.state.snap_enabled = enabled;
					cx.notify();
				});
				println!("[tools] snapping: {enabled}");
				self.rebuild_menu_bar(cx);
			}
			tool_action if Tool::from_action(tool_action).is_some() => {
				let tool = Tool::from_action(tool_action).unwrap();
				self.active_tool = tool;
				println!("[tools] selected: {tool:?}");
				// Push the app tool into the timeline widget when it models
				// one; unmodeled tools (edit/transition/add/record/hand) keep
				// the widget's current tool.
				if let Some(tt) = tool.timeline_tool() {
					self.timeline.update(cx, |timeline, cx| {
						timeline.set_tool(tt, cx);
					});
				}
				self.rebuild_menu_bar(cx);
			}
			// --- Proxy (Tools) ---------------------------------------------
			A::UseProxyMedia => {
				let enabled = !self.engine.read(cx).use_proxy_media();
				self.engine
					.update(cx, |engine, cx| engine.set_use_proxy_media(enabled, cx));
				self.rebuild_menu_bar(cx);
			}
			A::ProxySettings => self.open_proxy_dialog(cx),
			A::ProjectProperties => self.open_project_properties(cx),
			A::MulticamWizard => self.open_multicam_wizard(cx),
			A::NewSequence => self.open_new_sequence(cx),
			A::NewFolder => {
				if let Err(err) = self
					.engine
					.update(cx, |engine, cx| engine.create_folder(String::new(), cx))
				{
					println!("[sequence] new folder: {err}");
				}
			}
			// The multicam source-switch hotkeys are scoped to the Multicam
			// panel (the focused-panel route handles them there); a fall-through
			// from any other focused panel is a silent no-op.
			A::MulticamSwitch1
			| A::MulticamSwitch2
			| A::MulticamSwitch3
			| A::MulticamSwitch4
			| A::MulticamSwitch5
			| A::MulticamSwitch6
			| A::MulticamSwitch7
			| A::MulticamSwitch8
			| A::MulticamSwitch9
			| A::MulticamSwitchNoSplit1
			| A::MulticamSwitchNoSplit2
			| A::MulticamSwitchNoSplit3
			| A::MulticamSwitchNoSplit4
			| A::MulticamSwitchNoSplit5
			| A::MulticamSwitchNoSplit6
			| A::MulticamSwitchNoSplit7
			| A::MulticamSwitchNoSplit8
			| A::MulticamSwitchNoSplit9 => {}
			// --- Help --------------------------------------------------------
			A::ActionSearch => self.open_action_search(cx),
			A::About => self.open_about(cx),
			// --- everything else is a placeholder --------------------------
			other => println!(
				"[action] {} not wired yet (placeholder)",
				other.entry().cpp_id
			),
		}
	}

	/// 新建项目: creates a blank project in the library and opens it (the
	/// write-through persists it from the first edit). Falls back to the
	/// engine's plain new-project path when the library is unavailable.
	fn open_new_project(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| crate::dialogs::NewProjectContent::new(window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::NEW_PROJECT,
					ModalOptions::new(
						crate::i18n::tr("menu.file.new_project"),
						px(480.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::NewProject { modal, content }
		});
	}

	/// Creates the project from the dialog's name + format, then seeds its
	/// first sequence (the old `new_project` path kept for manager fallback).
	fn commit_new_project(&mut self, content: &Entity<crate::dialogs::NewProjectContent>, cx: &mut Context<Self>) {
		let name = content.read(cx).name(cx).to_string();
		let format = content.read(cx).format(cx);
		let interlaced = content.read(cx).interlaced(cx);
		let engine = self.engine.clone();
		let result = engine.update(cx, |engine, cx| {
			engine.library_create_project(&name, cx)
		});
		match result {
			Ok(()) => {
				self.close_modal(cx);
				let engine = self.engine.clone();
				engine.update(cx, |engine, cx| {
					let _ = engine.create_sequence_with_params(
						crate::i18n::tr("manager.new.default_name").to_string(),
						format,
						interlaced,
						cx,
					);
				});
			}
			Err(err) => {
				println!("[file] library create failed ({err}); plain new project");
				self.close_modal(cx);
				self.engine.update(cx, |engine, cx| engine.new_project(cx));
			}
		}
	}

	/// Deletes the timeline's selected clips (ripple or gap) through the
	/// engine's edit commands.
	fn delete_timeline_selection(&mut self, ripple: bool, cx: &mut Context<Self>) {
		let ids: Vec<gpui::timeline::ClipId> =
			self.timeline.read(cx).selection().iter().copied().collect();
		if ids.is_empty() {
			println!("[timeline] delete: nothing selected");
			return;
		}
		for id in ids {
			self.engine
				.update(cx, |engine, cx| engine.delete_clip(id, ripple, cx));
		}
	}

	/// 编辑 → 全选: selects every timeline clip and forwards the selection
	/// to the engine (the effect stack's target), mirroring what the
	/// timeline's own `SelectionChanged` subscription does.
	fn select_all_clips(&mut self, cx: &mut Context<Self>) {
		let ids: Vec<ClipId> = {
			let engine = self.engine.read(cx);
			let mut ids = Vec::new();
			for index in 0..engine.track_count() {
				if let Some(track) = engine.track(index) {
					ids.extend(track.clips().iter().map(|clip| clip.id()));
				}
			}
			ids
		};
		self.timeline.update(cx, |view, cx| {
			view.state.select_range(ids.iter().copied());
			cx.notify();
		});
		self.engine
			.update(cx, |engine, cx| engine.set_selected_clips(ids, cx));
	}

	/// 编辑 → 取消全选: clears the timeline selection and tells the engine
	/// (the effect stack's target follows).
	fn deselect_all_clips(&mut self, cx: &mut Context<Self>) {
		self.timeline.update(cx, |view, cx| {
			view.state.select_range(std::iter::empty::<ClipId>());
			cx.notify();
		});
		self.engine
			.update(cx, |engine, cx| engine.set_selected_clips(Vec::new(), cx));
	}

	/// 视图 → 放大/缩小: scales the timeline zoom around its left edge
	/// (`TimelineState::set_zoom` clamps to the widget's zoom range).
	fn zoom_timeline(&mut self, factor: f32, cx: &mut Context<Self>) {
		self.timeline.update(cx, |view, cx| {
			let zoom = view.state.zoom * factor;
			view.state.set_zoom(zoom, px(0.));
			cx.notify();
		});
	}

	/// 视图 → 轨道高度 ±: steps every track's height by `delta` pixels,
	/// clamped to the track-height slider's range (24–160px).
	fn nudge_track_height(&mut self, delta: f32, cx: &mut Context<Self>) {
		let current = self
			.engine
			.read(cx)
			.track(0)
			.map(|track| f32::from(track.height()))
			.unwrap_or(64.0);
		let next = (current + delta).clamp(24.0, 160.0);
		self.engine
			.update(cx, |engine, cx| engine.set_track_height(px(next), cx));
	}

	/// 回放 → 设置入点/出点: moves the work area's start (`in_point`) or end
	/// to the program playhead as ONE undoable entry (the same commit the
	/// ruler drag and 序列 → 设置工作区 use). Without an existing work area
	/// the far edge falls back to the sequence bounds.
	fn set_point_at_playhead(&mut self, in_point: bool, cx: &mut Context<Self>) {
		let playhead = self.program_clock.read(cx).current_frame();
		let seq_len = self
			.engine
			.read(cx)
			.current_sequence()
			.map(|s| s.length)
			.unwrap_or(Frame(playhead.0 + 1));
		let (old_start, old_end) = self
			.engine
			.read(cx)
			.workarea()
			.unwrap_or((Frame::ZERO, seq_len));
		let (start, end) = if in_point {
			(playhead, old_end.max(Frame(playhead.0 + 1)))
		} else {
			(old_start.min(Frame((playhead.0 - 1).max(0))), playhead)
		};
		if end.0 <= start.0 {
			println!("[playback] set in/out point: empty range, ignored");
			return;
		}
		self.engine.update(cx, |engine, cx| {
			engine.commit_workarea(old_start, old_end, start, end, cx);
		});
	}

	/// Applies the dark/light theme and rebuilds the menu bar (the theme
	/// checkmark moves). The caller persists the choice through
	/// [`crate::oakui::real::set_theme_dark`].
	fn apply_dark(&mut self, dark: bool, cx: &mut Context<Self>) {
		self.dark = dark;
		if dark {
			apply_theme(cx, &OakTheme::olive_dark());
		} else {
			apply_theme(cx, &OakTheme::olive_light());
		}
		self.rebuild_menu_bar(cx);
		cx.notify();
	}

	/// The shell's keyboard entry point: the global key bindings dispatch
	/// gpui actions, which bubble up to the root's action listeners and land
	/// here — the same [`Self::dispatch_action_id`] path the menu clicks
	/// take. While a modal dialog is open the shell stays keyboard-quiet, so
	/// the dialogs' text fields never trigger editing actions.
	fn on_action_dispatched(&mut self, action: ActionId, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		self.dispatch_action_id(action, cx);
	}

	/// Removes the first track selected in the timeline header.
	fn remove_selected_track(&mut self, cx: &mut Context<Self>) {
		let Some(&index) = self.timeline.read(cx).selected_tracks().iter().next() else {
			println!("[timeline] remove track: nothing selected");
			return;
		};
		self.engine
			.update(cx, |engine, cx| engine.remove_track(index, cx));
	}

	/// 序列 → 设置工作区: sets the work area to the bounding range of the
	/// selected clips, or one frame at the program playhead when nothing is
	/// selected. Committed as ONE undoable entry whose old side is the
	/// engine's current work area (mirrors the ruler drag's commit).
	fn set_workarea_from_selection(&mut self, cx: &mut Context<Self>) {
		let (old_start, old_end) = self
			.engine
			.read(cx)
			.workarea()
			.unwrap_or((Frame::ZERO, Frame::ZERO));
		let (start, end) = self.selection_workarea_range(cx);
		self.engine.update(cx, |engine, cx| {
			engine.commit_workarea(old_start, old_end, start, end, cx);
		});
	}

	/// The bounding range of the timeline's selected clips; `[playhead,
	/// playhead + 1)` when nothing is selected (the menu's fallback).
	fn selection_workarea_range(&self, cx: &App) -> (Frame, Frame) {
		let ids: Vec<ClipId> = self.timeline.read(cx).selection().iter().copied().collect();
		let engine = self.engine.read(cx);
		let mut start: Option<i64> = None;
		let mut end: i64 = 0;
		for index in 0..engine.track_count() {
			if let Some(track) = engine.track(index) {
				for clip in track.clips() {
					if ids.contains(&clip.id()) {
						let range = clip.range();
						start = Some(start.map_or(range.start.0, |s| s.min(range.start.0)));
						end = end.max(range.end.0);
					}
				}
			}
		}
		match start {
			Some(s) if end > s => (Frame(s), Frame(end)),
			_ => {
				let playhead = self.program_clock.read(cx).current_frame();
				(playhead, Frame(playhead.0 + 1))
			}
		}
	}

	/// 窗口 menu toggle: clicking a listed panel opens it if it is closed and
	/// closes it if it is open. Docked panels are removed through the dock's
	/// remove flow — the same path the tab ✕ button takes (see
	/// [`DockArea::remove_panel`]) — so the close is symmetrical and the
	/// position is recorded for a later reopen. Floating (tear-off) panels
	/// have their window closed for good. Rebuilds the menu bar so the
	/// checkmarks follow.
	fn toggle_panel(&mut self, id: gpui::dock::PanelId, cx: &mut Context<Self>) {
		if self.dock.read(cx).is_floating(id) {
			let dock = self.dock.clone();
			dock.update(cx, |dock, cx| dock.close_floating(id, cx));
		} else if self.dock.read(cx).is_docked(id) {
			let dock = self.dock.clone();
			dock.update(cx, |dock, cx| {
				let _ = dock.remove_panel(id, cx);
			});
		} else {
			self.reopen_panel(id, cx);
		}
		self.rebuild_menu_bar(cx);
		cx.notify();
	}

	/// Re-opens a closed panel, preferring its last docked position when the
	/// anchor panel is still present, then falling back to the design's
	/// default placement. The panel entity is reused, so its per-panel state
	/// survives the close/reopen round trip.
	fn reopen_panel(&mut self, id: gpui::dock::PanelId, cx: &mut Context<Self>) {
		let Some(handle) = self.panel_handle(id, cx) else {
			return;
		};
		let dock = self.dock.clone();
		dock.update(cx, |dock, cx| {
			let target = dock.last_target(id).or_else(|| default_dock_target(id));
			let target = dock.fallback_target(target);
			let _ = dock.add_panel(handle, target, cx);
		});
		cx.notify();
	}

	/// Wraps the shell's pre-built panel entity for `id` in a fresh
	/// [`PanelHandle`], so a reopened panel keeps its live state.
	fn panel_handle(&self, id: gpui::dock::PanelId, cx: &App) -> Option<PanelHandle> {
		match id {
			PROJECT => Some(PanelHandle::new(self.panels.project.clone(), cx)),
			SOURCE_VIEWER => Some(PanelHandle::new(self.panels.source_viewer.clone(), cx)),
			PROGRAM_VIEWER => Some(PanelHandle::new(self.panels.program_viewer.clone(), cx)),
			NODE_EDITOR => Some(PanelHandle::new(self.panels.node_editor.clone(), cx)),
			INSPECTOR => Some(PanelHandle::new(self.panels.inspector.clone(), cx)),
			HISTORY => Some(PanelHandle::new(self.panels.history.clone(), cx)),
			TIMELINE => Some(PanelHandle::new(self.panels.timeline.clone(), cx)),
			EFFECT_LIBRARY => Some(PanelHandle::new(self.panels.effect_library.clone(), cx)),
			MULTICAM => Some(PanelHandle::new(self.panels.multicam.clone(), cx)),
			_ => None,
		}
	}

	/// 窗口 → 重置布局: rebuilds the dock from scratch with every panel at its
	/// design position and the default ratios and active tabs restored.
	/// Docked panels are removed through the dock's remove flow; floating
	/// (tear-off) panels have their window closed for good.
	fn reset_default_layout(&mut self, cx: &mut Context<Self>) {
		let dock = self.dock.clone();
		let ids: Vec<PanelId> = WINDOW_PANELS
			.iter()
			.map(|(id, _)| *id)
			.filter(|id| dock.read(cx).is_panel_visible(*id))
			.collect();
		dock.update(cx, |dock, cx| {
			for id in ids {
				if dock.is_floating(id) {
					dock.close_floating(id, cx);
				} else {
					let _ = dock.remove_panel(id, cx);
				}
			}
		});
		self.apply_default_layout(cx);
		self.rebuild_menu_bar(cx);
		cx.notify();
	}

	/// (Re-)seeds every panel at its design position, mirroring the seeding in
	/// [`OakApp::new`]: panels are added in the 窗口 menu's order
	/// ([`WINDOW_PANELS`]) at their [`default_dock_target`] placement, then
	/// the default split ratios and active tabs are applied.
	fn apply_default_layout(&mut self, cx: &mut Context<Self>) {
		let mut panels = Vec::new();
		for (id, _) in WINDOW_PANELS {
			if let Some(handle) = self.panel_handle(id, cx) {
				panels.push((handle, default_dock_target(id)));
			}
		}
		let dock = self.dock.clone();
		dock.update(cx, |dock, cx| {
			for (handle, target) in panels {
				let _ = dock.add_panel(handle, target, cx);
			}
			let mut layout = dock.layout().clone();
			tune_default_layout(&mut layout);
			dock.set_layout(layout, cx);
		});
	}

	/// Switches the UI language live: updates the [`i18n`] global, rebuilds
	/// the menu bar (so the menu labels and the language checkmark move
	/// immediately), and repaints the whole shell.
	fn switch_language(&mut self, code: &str, cx: &mut Context<Self>) {
		crate::i18n::set_language_code(code);
		self.rebuild_menu_bar(cx);
		cx.notify();
	}

	/// Toggles the window's full-screen state (the 视图 → 全屏 menu entry and
	/// the viewer context menu's `FullScreenRequested`), then refreshes the
	/// menu bar so the checkmark follows the real window state.
	fn toggle_full_screen(&mut self, cx: &mut Context<Self>) {
		let windows = cx.windows();
		let Some(window) = windows.first() else {
			return;
		};
		let Ok(full_screen) = cx.update_window(*window, |_root, window, _app| {
			window.toggle_fullscreen();
			window.is_fullscreen()
		}) else {
			return;
		};
		self.full_screen = full_screen;
		self.rebuild_menu_bar(cx);
	}

	/// Applies a viewer panel's shell-level request: full-screen toggles the
	/// window, and the loop-range requests move the shared program workarea
	/// (both monitors act on it through the same undoable path as the menu).
	fn apply_viewer_panel_event(&mut self, event: menu::ViewerPanelEvent, cx: &mut Context<Self>) {
		match event {
			menu::ViewerPanelEvent::FullScreenRequested => self.toggle_full_screen(cx),
			menu::ViewerPanelEvent::SetInPoint => self.set_point_at_playhead(true, cx),
			menu::ViewerPanelEvent::SetOutPoint => self.set_point_at_playhead(false, cx),
			menu::ViewerPanelEvent::ClearRange => {
				self.engine
					.update(cx, |engine, cx| engine.clear_workarea(cx));
			}
		}
	}

	/// Replaces the `MenuBar` entity with one built from the current language
	/// and the dynamic checkmark state, re-subscribing to its trigger events.
	fn rebuild_menu_bar(&mut self, cx: &mut Context<Self>) {
		let windows = cx.windows();
		let Some(handle) = windows.first() else {
			return;
		};
		let state = MenuState {
			dark: self.dark,
			active_tool: self.active_tool,
			snapping: self.timeline.read(cx).state.snap_enabled,
			loop_playback: self.loop_playback,
			show_all: self.show_all,
			full_screen: self.full_screen,
			use_proxy_media: self.engine.read(cx).use_proxy_media(),
			// The 窗口 menu's checkmarks mirror the dock's live visible panel
			// set (docked or floating), so a panel closed via its tab ✕ (or
			// torn off) loses its checkmark on the next rebuild.
			open_panels: open_panels_mask(self.dock.read(cx)),
		};
		let Ok(menu_bar) = cx.update_window(*handle, |_root, window, app| {
			app.new(|cx| MenuBar::new(1, make_menus(state), window, cx))
		}) else {
			return;
		};
		self.menu_bar = menu_bar;
		let menu_bar = self.menu_bar.clone();
		cx.subscribe(
			&menu_bar,
			|this, _menu: Entity<MenuBar>, event: &MenuBarEvent, cx| {
				if let MenuBarEvent::Triggered { item, .. } = event {
					this.on_menu(*item, cx);
				}
			},
		)
		.detach();
	}

	// -----------------------------------------------------------------------
	// Project manager (M13 D4)
	// -----------------------------------------------------------------------

	/// Opens the project manager (startup without a project argument, and
	/// 文件 → 项目管理器 / 从库中打开…). Re-entrant: an already-open
	/// manager just reloads its list.
	pub fn show_project_manager(&mut self, cx: &mut Context<Self>) {
		if let ModalState::Manager { content, .. } = &self.modal {
			let content = content.clone();
			content.update(cx, |manager, cx| manager.reload(cx));
			return;
		}
		let engine = self.engine.clone();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| crate::manager::ProjectManager::new(engine, window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::MANAGER,
					ModalOptions::new(crate::i18n::tr("manager.title"), px(880.0))
						.with_button(DialogButton::cancel(crate::i18n::tr("dialog.close"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::Manager { modal, content }
		});
		// The content's requests (open / create / rename / ...) route here.
		if let ModalState::Manager { content, .. } = &self.modal {
			let content = content.clone();
			cx.subscribe(
				&content,
				|this, _content, event: &crate::manager::ManagerEvent, cx| {
					this.on_manager_event(event, cx);
				},
			)
			.detach();
		}
	}

	/// Routes a project-manager request through the engine. Open / Create
	/// close the dialog on success; the mutating actions reload the list;
	/// failures land in the dialog's status line.
	fn on_manager_event(&mut self, event: &crate::manager::ManagerEvent, cx: &mut Context<Self>) {
        use crate::manager::ManagerEvent as E;
        match event {
			E::Create => {
				let name = crate::i18n::tr("manager.new.default_name").to_string();
				let result = self
					.engine
					.update(cx, |engine, cx| engine.library_create_project(&name, cx));
				match result {
					Ok(()) => self.close_modal(cx),
					Err(err) => self.manager_status(err, cx),
				}
			}
			E::Open(uuid) => {
				let uuid = uuid.clone();
				let result = self
					.engine
					.update(cx, |engine, cx| engine.library_open_project(&uuid, cx));
				match result {
					Ok(()) => self.close_modal(cx),
					Err(err) => self.manager_status(err, cx),
				}
			}
			E::Rename(uuid) => self.open_manager_rename(uuid.clone(), cx),
			E::Duplicate(uuid) => {
				let result = self
					.engine
					.update(cx, |engine, _cx| engine.library_duplicate_project(uuid));
				match result {
					Ok(()) => self.reload_manager(cx),
					Err(err) => self.manager_status(err, cx),
				}
			}
			E::Delete(uuid) => self.open_manager_delete(uuid.clone(), cx),
			E::Import => self.open_file_dialog(FileAction::ImportProject, cx),
			E::Export(uuid) => self.open_manager_export(uuid.clone(), cx),
		}
	}

	/// Reloads the open manager's list (after a library mutation).
	fn reload_manager(&mut self, cx: &mut Context<Self>) {
		if let ModalState::Manager { content, .. } = &self.modal {
			let content = content.clone();
			content.update(cx, |manager, cx| manager.reload(cx));
		}
	}

	/// Shows an engine error in the open manager's status line (or logs it
	/// when the manager is gone).
	fn manager_status(&mut self, message: String, cx: &mut Context<Self>) {
		if let ModalState::Manager { content, .. } = &self.modal {
			let content = content.clone();
			content.update(cx, |manager, cx| manager.set_status(Some(message), cx));
		} else {
			println!("[manager] {message}");
		}
	}

	/// The selected row's display name in the open manager (used to seed
	/// the rename prompt / the delete confirmation / the export filename).
	fn manager_selected_name(&self, cx: &App) -> Option<String> {
		match &self.modal {
			ModalState::Manager { content, .. } => content.read(cx).selected_name(),
			_ => None,
		}
	}

	/// Swaps the manager for the rename prompt of row `uuid`.
	fn open_manager_rename(&mut self, uuid: String, cx: &mut Context<Self>) {
		let initial = self.manager_selected_name(cx).unwrap_or_default();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| crate::manager::NamePrompt::new(&initial, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::MANAGER_RENAME,
					ModalOptions::new(crate::i18n::tr("manager.rename.title"), px(380.0))
						.with_button(DialogButton::primary(crate::i18n::tr(
							"manager.rename.title",
						)))
						.with_button(DialogButton::cancel(crate::i18n::tr("dialog.cancel"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::ManagerRename {
				modal,
				content,
				uuid,
			}
		});
	}

	/// Swaps the manager for the delete confirmation of row `uuid`.
	fn open_manager_delete(&mut self, uuid: String, cx: &mut Context<Self>) {
		let name = self.manager_selected_name(cx).unwrap_or_default();
		let text = crate::i18n::tr("manager.delete.confirm").replace("{name}", &name);
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|_cx| crate::manager::ConfirmContent::new(text));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::MANAGER_DELETE,
					ModalOptions::new(crate::i18n::tr("manager.delete.title"), px(420.0))
						.with_button(DialogButton::primary(crate::i18n::tr("manager.delete")))
						.with_button(DialogButton::cancel(crate::i18n::tr("dialog.cancel"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::ManagerDelete { modal, uuid }
		});
	}

	/// Confirms the rename prompt (button 0).
	fn confirm_manager_rename(&mut self, cx: &mut Context<Self>) {
		let ModalState::ManagerRename { content, uuid, .. } = &self.modal else {
			return;
		};
		let name = content.read(cx).value(cx);
		let uuid = uuid.clone();
		if name.is_empty() {
			self.back_to_manager(cx);
			return;
		}
		let result = self.engine.update(cx, |engine, _cx| {
			engine.library_rename_project(&uuid, &name)
		});
		match result {
			Ok(()) => self.back_to_manager(cx),
			Err(err) => {
				self.back_to_manager(cx);
				self.manager_status(err, cx);
			}
		}
	}

	/// Confirms the delete confirmation (button 0).
	fn confirm_manager_delete(&mut self, cx: &mut Context<Self>) {
		let ModalState::ManagerDelete { uuid, .. } = &self.modal else {
			return;
		};
		let uuid = uuid.clone();
		let result = self
			.engine
			.update(cx, |engine, _cx| engine.library_delete_project(&uuid));
		match result {
			Ok(()) => self.back_to_manager(cx),
			Err(err) => {
				self.back_to_manager(cx);
				self.manager_status(err, cx);
			}
		}
	}

	/// Returns from a manager sub-dialog (rename / delete) to the manager.
	fn back_to_manager(&mut self, cx: &mut Context<Self>) {
		self.modal = ModalState::None;
		self.show_project_manager(cx);
	}

	/// Opens the platform save dialog for exporting the library row `uuid`
	/// (the suggested name is `<project>.ove`; the format follows the
	/// extension the user picks).
	fn open_manager_export(&mut self, uuid: String, cx: &mut Context<Self>) {
		let name = self
			.manager_selected_name(cx)
			.filter(|n| !n.is_empty())
			.unwrap_or_else(|| "project".to_string());
		self.pending_export = Some(uuid);
		let receiver = cx.prompt_for_new_path(&PathBuf::from("."), Some(&format!("{name}.ove")));
		cx.spawn(async move |this, cx| {
			if let Ok(Ok(Some(path))) = receiver.await {
				this.update(cx, |this, cx| {
					this.on_file_paths(FileAction::ExportProject, vec![path], cx);
				});
			}
		})
		.detach();
	}

	// -----------------------------------------------------------------------
	// Modal dialogs
	// -----------------------------------------------------------------------

	/// Closes the current modal.
	pub fn close_modal(&mut self, cx: &mut Context<Self>) {
		self.modal = ModalState::None;
		cx.notify();
	}

	/// Builds a modal on the main window, subscribes it to
	/// [`Self::on_modal`] and layers it onto the shell.
	///
	/// The modal is created inside `update_window` (modal widgets need a
	/// `&mut Window`); the state swap and the subscription happen *after* the
	/// window update returns, on this entity's own `Context` — swapping state
	/// through a weak handle *inside* the window callback would re-enter this
	/// entity while it is already being updated (the crash seen when opening
	/// Preferences from a menu action).
	///
	/// The caller must NOT be inside a window update itself (e.g.
	/// `WindowHandle::update`): the nested `update_window` below would fail
	/// and the modal would silently not open. Drive the root entity instead
	/// (menu actions and entity updates are fine).
	/// Builds and installs a modal. When the caller is already inside a
	/// window update (an action listener / menu dispatch / tick), a nested
	/// `update_window` would silently fail and drop the dialog — the phase-7
	/// fix for Preferences / Action Search, applied centrally: probe first,
	/// and defer the build to the end of the current app update when nested.
	/// The fast path stays synchronous so callers can rely on `self.modal`
	/// right after the call.
	fn spawn_modal(
		&mut self,
		cx: &mut Context<Self>,
		build: impl FnOnce(&mut Window, &mut App) -> ModalState<E> + 'static,
	) {
		let handle = cx.windows().first().copied();
		let nested = match handle {
			Some(handle) => cx.update_window(handle, |_root, _window, _app| ()).is_err(),
			None => false,
		};
		if nested {
			let this = cx.weak_entity();
			cx.defer(move |app| {
				let _ = this.update(app, |this, cx| this.spawn_modal_now(cx, build));
			});
			return;
		}
		self.spawn_modal_now(cx, build);
	}

	/// Builds and installs a modal immediately (the deferred half of
	/// [`Self::spawn_modal`]; also called directly when the caller already
	/// runs outside a window update).
	fn spawn_modal_now(
		&mut self,
		cx: &mut Context<Self>,
		build: impl FnOnce(&mut Window, &mut App) -> ModalState<E>,
	) {
		let windows = cx.windows();
		let Some(handle) = windows.first() else {
			return;
		};
		let Ok(state) = cx.update_window(*handle, |_root, window, app| build(window, app)) else {
			return;
		};
		let modal = state
			.modal_entity()
			.expect("spawned modal always carries a Modal");
		cx.subscribe(&modal, |this, _entity, event: &ModalEvent, cx| {
			this.on_modal(event, cx);
		})
		.detach();
		self.modal = state;
		cx.notify();
	}

	/// Opens the platform file dialog for `action` and routes the picked
	/// path(s) through the engine. Open / Import use the path picker (import
	/// footage allows multiple files); the 导出工程文件… and the manager's
	/// export ask for a new path. The picker resolves asynchronously, so the
	/// chosen path is applied in a spawned task via [`Self::on_file_paths`].
	fn open_file_dialog(&mut self, action: FileAction, cx: &mut Context<Self>) {
		match action {
			FileAction::Open | FileAction::ImportFootage | FileAction::ImportProject => {
				let prompt = match action {
					FileAction::Open => crate::i18n::tr("file.open.title"),
					FileAction::ImportProject => crate::i18n::tr("manager.import.title"),
					_ => crate::i18n::tr("file.import_footage.title"),
				};
				let receiver = cx.prompt_for_paths(PathPromptOptions {
					files: true,
					directories: false,
					multiple: action == FileAction::ImportFootage,
					prompt: Some(prompt.into()),
					allowed_extensions: if action == FileAction::Open {
						// Project files only: OVE / OTIO / FCPXML.
						vec![
							"ove".to_string(),
							"ovexml".to_string(),
							"otio".to_string(),
							"fcpxml".to_string(),
						]
					} else {
						Vec::new()
					},
				});
				cx.spawn(async move |this, cx| {
					if let Ok(Ok(Some(paths))) = receiver.await {
						if !paths.is_empty() {
							this.update(cx, |this, cx| this.on_file_paths(action, paths, cx));
						}
					}
				})
				.detach();
			}
			FileAction::ExportProjectFile => {
				let current = self
					.engine
					.read(cx)
					.project()
					.map(|p| p.path.clone())
					.filter(|p| !p.as_os_str().is_empty());
				let (directory, suggested) = match current {
					Some(path) => (
						path.parent()
							.map(|dir| dir.to_path_buf())
							.unwrap_or_else(|| PathBuf::from(".")),
						path.file_name()
							.map(|name| name.to_string_lossy().into_owned()),
					),
					None => (PathBuf::from("."), None),
				};
				let receiver = cx.prompt_for_new_path(&directory, suggested.as_deref());
				cx.spawn(async move |this, cx| {
					if let Ok(Ok(Some(path))) = receiver.await {
						this.update(cx, |this, cx| {
							this.on_file_paths(FileAction::ExportProjectFile, vec![path], cx);
						});
					}
				})
				.detach();
			}
			// The manager's export prompts in `open_manager_export` (it needs
			// the selection's suggested filename).
			FileAction::ExportProject => {}
		}
	}

	/// Applies paths picked in the platform dialog through the engine, using
	/// the action's routing (open / import / export).
	fn on_file_paths(&mut self, action: FileAction, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
		// The manager's library import/export refresh the open manager and
		// report failures in its status line.
		match action {
			FileAction::ImportProject => {
				let Some(path) = paths.first() else {
					return;
				};
				let result = self.engine.update(cx, |engine, _cx| {
					engine.library_import_project(path.clone())
				});
				match result {
					Ok(uuid) => {
						println!("[manager] imported \"{}\" as {uuid}", path.display());
						self.reload_manager(cx);
					}
					Err(err) => self.manager_status(err, cx),
				}
				return;
			}
			FileAction::ExportProject => {
				let (Some(uuid), Some(path)) = (self.pending_export.take(), paths.first()) else {
					return;
				};
				let result = self.engine.update(cx, |engine, _cx| {
					engine.library_export_project(&uuid, path.clone())
				});
				match result {
					Ok(()) => println!("[manager] exported to \"{}\"", path.display()),
					Err(err) => self.manager_status(err, cx),
				}
				return;
			}
			_ => {}
		}
		let result = self.engine.update(cx, |engine, cx| match action {
			FileAction::Open => match paths.first() {
				Some(path) => engine.open_project_path(path.clone(), cx),
				None => Ok(()),
			},
			FileAction::ExportProjectFile => match paths.first() {
				Some(path) => engine.export_project_path(path.clone(), cx),
				None => Ok(()),
			},
			FileAction::ImportFootage => {
				// Import accepts several files at once; keep the first failure
				// for the log after the rest have been attempted.
				let mut first_error = None;
				for path in paths {
					if let Err(err) = engine.import_footage(path.clone(), cx) {
						first_error.get_or_insert(err);
					}
				}
				match first_error {
					Some(err) => Err(err),
					None => Ok(()),
				}
			}
			// Handled above (the manager's library import/export).
			FileAction::ImportProject | FileAction::ExportProject => Ok(()),
		});
		if let Err(err) = result {
			println!("[file] {action:?} failed: {err}");
		}
	}

	/// Opens the preferences dialog (General + Keyboard tabs). Theme/language
	/// selections emit [`crate::dialogs::PreferencesEvent`]s, applied to the
	/// shell chrome immediately; the typed cache directory commits when the
	/// dialog closes; a shortcut change re-binds the key map and rebuilds the
	/// menu bar, so the new keys are live without a restart.
	///
	/// The build is deferred to the end of the current app update (like
	/// [`Self::open_action_search`]): the dialog is usually opened from a
	/// shortcut or a menu click, both of which dispatch inside a window
	/// update where `spawn_modal`'s nested `update_window` would silently
	/// fail.
	pub fn open_preferences(&mut self, cx: &mut Context<Self>) {
		let weak = cx.weak_entity();
		cx.defer(move |app| {
			if let Some(this) = weak.upgrade() {
				this.update(app, |this, cx| this.open_preferences_now(cx));
			}
		});
	}

	/// The deferred half of [`Self::open_preferences`]: builds the tabbed
	/// dialog and subscribes to its [`crate::dialogs::PreferencesEvent`]s.
	fn open_preferences_now(&mut self, cx: &mut Context<Self>) {
		self.spawn_modal(cx, |window, app| {
			let content = app.new(|cx| PreferencesDialogContent::new(window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::PREFERENCES,
					ModalOptions::new(crate::i18n::tr("preferences.title"), px(720.0))
						.with_button(DialogButton::primary(crate::i18n::tr("dialog.close"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::Preferences { modal, content }
		});
		if let ModalState::Preferences { content, .. } = &self.modal {
			let content = content.clone();
			cx.subscribe(
				&content,
				|this, _content, event: &crate::dialogs::PreferencesEvent, cx| match *event {
					crate::dialogs::PreferencesEvent::ThemeChanged(dark) => {
						this.apply_dark(dark, cx);
					}
					crate::dialogs::PreferencesEvent::LanguageChanged => {
						this.rebuild_menu_bar(cx);
						cx.notify();
					}
					crate::dialogs::PreferencesEvent::ShortcutsChanged => {
						this.rebind_keys(cx);
					}
					crate::dialogs::PreferencesEvent::DisplayColorChanged => {
						this.reapply_display_color(cx);
					}
				},
			)
			.detach();
		}
	}

	/// Commits the preferences dialog's free-text fields (the cache
	/// directory path) before the modal closes.
	fn commit_preferences(&mut self, cx: &mut Context<Self>) {
		if let ModalState::Preferences { content, .. } = &self.modal {
			content.update(cx, |content, cx| content.commit_cache_dir(cx));
		}
		// The committed custom-ICC path may change the effective display
		// transform (or the policy, if the profile availability flipped):
		// re-declare on every window.
		self.reapply_display_color(cx);
	}

	/// Re-applies the global key bindings and rebuilds the menu bar after a
	/// shortcut override change. The gpui key map is replaced wholesale (its
	/// `bind_keys` only appends) and the menu labels re-read the effective
	/// keys, so the change is live immediately — no restart.
	fn rebind_keys(&mut self, cx: &mut Context<Self>) {
		cx.clear_key_bindings();
		cx.bind_keys(crate::actions::key_bindings());
		self.rebuild_menu_bar(cx);
		cx.notify();
	}

	/// Re-evaluates the display color policy and retags every window after
	/// the color-management preference changed (the single-mapping rule must
	/// hold at runtime, not just at startup). Drops the cached display
	/// processors, then re-declares the policy on each open window.
	fn reapply_display_color(&mut self, cx: &mut Context<Self>) {
		crate::oakui::displaycolor::invalidate();
		for handle in cx.windows() {
			let _ = cx.update_window(handle, |_root, window, _app| {
				crate::oakui::displaycolor::apply_to_window(window);
			});
		}
		cx.notify();
	}

	/// Opens the action search dialog (Help > Search Actions…, the `/` key).
	/// Enter / double-click in the dialog executes the action through the same
	/// [`Self::dispatch_action_id`] path the menu clicks take, so the behavior
	/// can never diverge from a menu click.
	///
	/// The actual modal build is deferred to the end of the current app update:
	/// keyboard shortcuts and menu clicks dispatch *inside* a window update,
	/// and `spawn_modal`'s nested `update_window` would fail (and silently
	/// drop the dialog) there. `defer` runs after the window is back in the
	/// app's map, so the modal opens one tick later.
	pub fn open_action_search(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let weak = cx.weak_entity();
		cx.defer(move |app| {
			if let Some(this) = weak.upgrade() {
				this.update(app, |this, cx| this.open_action_search_now(cx));
			}
		});
	}

	/// The deferred half of [`Self::open_action_search`]: builds the modal on
	/// the main window, subscribes to its execute events and focuses the search
	/// field.
	fn open_action_search_now(&mut self, cx: &mut Context<Self>) {
		self.spawn_modal(cx, |window, app| {
			let content = app.new(|cx| crate::dialogs::ActionSearchContent::new(window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::ACTION_SEARCH,
					ModalOptions::new(crate::i18n::tr("menu.help.action_search"), px(640.0))
						// Launcher-style: a backdrop click dismisses (same as
						// Escape and the title-row ✕).
						.with_dismiss_on_mask(true),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::ActionSearch { modal, content }
		});
		if let ModalState::ActionSearch { content, .. } = &self.modal {
			let content = content.clone();
			cx.subscribe(
				&content,
				|this, _content, event: &crate::dialogs::ActionSearchEvent, cx| {
					let crate::dialogs::ActionSearchEvent::Execute(action) = event;
					this.close_modal(cx);
					this.dispatch_action_id(*action, cx);
				},
			)
			.detach();
			// Keyboard-first: focus the search field as soon as the dialog is
			// up (the shell's action dispatch already suppresses the global
			// key map while the modal is open).
			if let Some(handle) = cx.windows().first() {
				let content = content.clone();
				let _ = cx.update_window(*handle, |_root, window, app| {
					let focus = content.read(app).search_focus(app);
					window.focus(&focus, app);
				});
			}
		}
	}

	/// Opens the about dialog (Help > About Oak…): the app name, version,
	/// license line and fork notice. No engine interaction.
	fn open_about(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		self.spawn_modal(cx, |window, app| {
			let content = app.new(|_cx| crate::dialogs::AboutContent::new());
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::ABOUT,
					ModalOptions::new(crate::i18n::tr("about.title"), px(480.0))
						.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
						// Launcher-style dismissal is natural here too.
						.with_dismiss_on_mask(true),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::About { modal }
		});
	}

	/// Opens the proxy settings dialog (Tools > Proxy Settings; the C++
	/// `ProxyDialog`): the global generation settings plus the footage
	/// proxy list with Generate / Delete buttons.
	fn open_proxy_dialog(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let engine = self.engine.clone();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| crate::dialogs::ProxyDialogContent::new(engine, window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::PROXY,
					ModalOptions::new(crate::i18n::tr("proxydialog.title"), px(560.0))
						.with_button(DialogButton::new(
							crate::i18n::tr("proxydialog.generate"),
							gpui_widgets::dialog::DialogButtonRole::Secondary,
						))
						.with_button(DialogButton::new(
							crate::i18n::tr("proxydialog.delete"),
							gpui_widgets::dialog::DialogButtonRole::Secondary,
						))
						.with_button(DialogButton::primary(crate::i18n::tr("proxydialog.close"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::Proxy { modal, content }
		});
	}

	/// Opens the project properties dialog (File > Project Properties…; the
	/// C++ `ProjectPropertiesDialog`): the read-only project name, the
	/// per-project OCIO config override and the disk-cache location. The OK
	/// button applies through the content's `commit` (an invalid OCIO config
	/// keeps the dialog open), Escape / Cancel discard without applying.
	fn open_project_properties(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let engine = self.engine.clone();
		let project_name = self
			.engine
			.read(cx)
			.project()
			.map(|p| p.name.clone())
			.unwrap_or_default();
		self.spawn_modal(cx, move |window, app| {
			let content =
				app.new(|cx| crate::dialogs::ProjectPropertiesContent::new(engine, window, cx));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::PROJECT_PROPERTIES,
					ModalOptions::new(
						format!("{} — {project_name}", crate::i18n::tr("projprops.title")),
						px(560.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::ProjectProperties { modal, content }
		});
	}

	/// Opens the new-sequence dialog (File > New > Sequence…; the C++
	/// `NewSequenceDialog`): the sequence name plus the format fields. The
	/// OK button creates the sequence through the content's `commit`.
	fn open_new_sequence(&mut self, cx: &mut Context<Self>) {
		self.spawn_new_sequence(SequenceFormatSeed::hd_1080p25(), cx);
	}

	/// Opens the new-sequence dialog seeded from the footage's probed video
	/// parameters (the drop-onto-empty-timeline flow), named like the
	/// footage so the user's OK match the media.
	fn open_new_sequence_for_footage(
		&mut self,
		drop: FootageDropNeedsSequence,
		cx: &mut Context<Self>,
	) {
		let probe = self
			.engine
			.read(cx)
			.footage_video_params(drop.footage_id);
		let seed = probe
			.map(|(width, height, rate, interlaced)| {
				SequenceFormatSeed::from_format(
					&crate::oakui::engine::VideoFormat { width, height, rate },
					interlaced,
				)
			})
			.unwrap_or_else(SequenceFormatSeed::hd_1080p25);
		self.spawn_new_sequence(seed, cx);
	}

	/// Opens the new-sequence dialog with the given format seed, while no
	/// other modal is up.
	fn spawn_new_sequence(
		&mut self,
		seed: SequenceFormatSeed,
		cx: &mut Context<Self>,
	) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let engine = self.engine.clone();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::NewSequenceContent::new_seeded(engine, seed, window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::NEW_SEQUENCE,
					ModalOptions::new(crate::i18n::tr("seqprops.new.title"), px(480.0))
						.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
						.with_button(DialogButton::new(
							crate::i18n::tr("dialog.cancel"),
							gpui_widgets::dialog::DialogButtonRole::Secondary,
						)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::NewSequence { modal, content }
		});
	}

	/// A footage drop landed on the timeline while no sequence is open: the
	/// shell pauses it and asks whether to probe the footage's parameters
	/// as the sequence's, or set them up manually.
	fn on_footage_drop_needs_sequence(
		&mut self,
		drop: FootageDropNeedsSequence,
		cx: &mut Context<Self>,
	) {
		if !matches!(self.modal, ModalState::None) {
			// A modal is already up (the drop cannot have been committed
			// against it): resume the drop through the probe path, keeping
			// the engine's auto-create behaviour.
			println!("[shell] drop while a modal is up: using the footage probe params");
			self.engine.update(cx, |engine, cx| {
				engine.drop_footage(drop.footage_id, drop.track_kind, drop.track_index, drop.time, cx);
			});
			return;
		}
		self.pending_drop = Some(drop);
		let probe = self.engine.read(cx).footage_video_params(drop.footage_id);
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|_cx| crate::dialogs::DropSequenceChoiceContent::new(probe));
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::DROP_SEQUENCE_CHOICE,
					ModalOptions::new(crate::i18n::tr("seqprops.drop.title"), px(440.0))
						.with_button(DialogButton::primary(crate::i18n::tr(
							"seqprops.drop.use_footage",
						)))
						.with_button(DialogButton::new(
							crate::i18n::tr("seqprops.drop.manual"),
							gpui_widgets::dialog::DialogButtonRole::Secondary,
						))
						.with_button(DialogButton::cancel(crate::i18n::tr("dialog.cancel"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::DropSequenceChoice { modal }
		});
	}

	/// The drop-pause flow's chosen outcome: resume the paused drop, open
	/// the manual setup dialog, or drop nothing (cancel).
	fn resolve_drop_sequence_choice(
		&mut self,
		choice: DropSequenceChoice,
		cx: &mut Context<Self>,
	) {
		let Some(drop) = self.pending_drop.take() else {
			self.close_modal(cx);
			return;
		};
		match choice {
			DropSequenceChoice::UseFootageParams => {
				// The engine's drop-footage path sizes the new sequence
				// from the footage's probed video stream (the NLE
				// convention); close the choice and place the clip.
				self.engine.update(cx, |engine, cx| {
					engine.drop_footage(drop.footage_id, drop.track_kind, drop.track_index, drop.time, cx);
				});
				self.close_modal(cx);
			}
			DropSequenceChoice::SpecifyManually => {
				// The manual path re-stashes the drop; the new-sequence
				// dialog's OK resumes it through the same commit below.
				self.pending_drop = Some(drop);
				self.close_modal(cx);
				self.open_new_sequence_for_footage(drop, cx);
			}
		}
	}

	/// The paused drop's last chance to place its clip: called after a
	/// manual new-sequence commit succeeded (the new sequence is open, so
	/// the original floor coordinates still resolve).
	fn resume_pending_drop(&mut self, cx: &mut Context<Self>) {
		let Some(drop) = self.pending_drop.take() else {
			return;
		};
		self.engine.update(cx, |engine, cx| {
			engine.drop_footage(drop.footage_id, drop.track_kind, drop.track_index, drop.time, cx);
		});
	}

	/// Opens the sequence properties dialog for the given sequence (the
	/// project explorer's context menu; the C++ `SequencePropertiesDialog`).
	/// The OK button applies the name / format edits through the content's
	/// `commit`.
	fn open_sequence_properties(&mut self, sequence_id: u64, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let engine = self.engine.clone();
		let sequence_name = self
			.engine
			.read(cx)
			.sequence_parameters(sequence_id)
			.map(|p| p.name)
			.unwrap_or_default();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::SequencePropertiesContent::new(engine, sequence_id, window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::SEQUENCE_PROPERTIES,
					ModalOptions::new(
						format!("{} — {sequence_name}", crate::i18n::tr("seqprops.title")),
						px(480.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::SequenceProperties { modal, content }
		});
	}

	/// Opens the rename dialog for the project-entry `id` (the project
	/// explorer's 重命名 context item), prefilled with the current name.
	fn open_entry_rename(&mut self, id: u64, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let current = self
			.engine
			.read(cx)
			.project_entry_name(id)
			.unwrap_or_default();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::RenameContent::new(current.clone(), window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::RENAME,
					ModalOptions::new(
						crate::i18n::tr("project.context.rename"),
						px(420.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::EntryRename { modal, content, entry_id: id }
		});
	}

	/// Opens the 导出工程文件 dialog: choose the project file format
	/// (OTIO / OVE / FCPXML); the OK button then asks the SYSTEM save
	/// dialog for the output path (the suggested file name carries the
	/// chosen extension).
	fn open_export_project_dfd(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let current = self
			.engine
			.read(cx)
			.project()
			.map(|p| p.path.clone())
			.filter(|p| !p.as_os_str().is_empty());
		let suggested = current
			.and_then(|path| {
				path.file_stem()
					.map(|name| name.to_string_lossy().into_owned())
			})
			.unwrap_or_else(|| "untitled".to_string());
		self.export_suggested_name = suggested;
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::ExportProjectDialogContent::new(window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::EXPORT_PROJECT_DFD,
					ModalOptions::new(
						crate::i18n::tr("menu.file.export_project"),
						px(480.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::ExportProjectDfd { modal, content }
		});
	}

	/// Opens the 另存为 dialog: the copy's new name (prefilled with the
	/// current project name). OK duplicates the project into the library
	/// under that name and switches to the copy.
	fn open_save_as(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let current_name = self
			.engine
			.read(cx)
			.project()
			.map(|p| p.name.clone())
			.unwrap_or_else(|| crate::i18n::tr("manager.new.default_name").to_string());
		let mut name = current_name.clone();
		if let Some((stem, _)) = name.rsplit_once('.') {
			name = stem.to_string();
		}
		name = format!("{name} Copy");
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::RenameContent::new(name.clone().into(), window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::SAVE_AS,
					ModalOptions::new(
						crate::i18n::tr("menu.file.save_as"),
						px(420.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("dialog.ok")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::SaveAs { modal, content }
		});
	}

	/// Confirms the save-as dialog: duplicate + switch to the copy.
	fn commit_save_as(
		&mut self,
		content: &Entity<crate::dialogs::RenameContent>,
		cx: &mut Context<Self>,
	) {
		let name = content.read(cx).value(cx).to_string();
		let engine = self.engine.clone();
		let result = engine.update(cx, |engine, cx| engine.save_project_as(&name, cx));
		if let Err(err) = result {
			println!("[file] save as failed: {err}");
		}
		self.close_modal(cx);
	}

	/// Opens the multicam wizard (窗口 → 多机位向导): pick the angle
	/// footage, choose the sync mode, then create the multicam sequence.
	fn open_multicam_wizard(&mut self, cx: &mut Context<Self>) {
		if !matches!(self.modal, ModalState::None) {
			return;
		}
		let engine = self.engine.clone();
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| {
				crate::dialogs::MulticamWizardContent::new(engine, window, cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::MULTICAM_WIZARD,
					ModalOptions::new(
						crate::i18n::tr("multicam.wizard.title"),
						px(480.0),
					)
					.with_button(DialogButton::primary(crate::i18n::tr("multicam.wizard.create")))
					.with_button(DialogButton::new(
						crate::i18n::tr("dialog.cancel"),
						gpui_widgets::dialog::DialogButtonRole::Secondary,
					)),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::MulticamWizard { modal, content }
		});
	}

	/// The wizard's OK: run the sync (or use timecode/no-align per the
	/// chosen mode), create the multicam sequence, then close. A rejected
	/// create keeps the dialog open with a message row.
	fn commit_multicam_wizard(
		&mut self,
		content: &Entity<crate::dialogs::MulticamWizardContent<E>>,
		cx: &mut Context<Self>,
	) -> Result<(), String> {
		let selection = content.read(cx).selection();
		if selection.len() < 2 {
			return Err(crate::i18n::tr("multicam.wizard.need_two").to_string());
		}
		let sync_mode = content.read(cx).sync_mode(cx);
		let name = content.read(cx).name(cx).to_string();
		// Sync: audio-waveform correlation (0), source timecode (1), or
		// no alignment (2 — all angles start at 0). The engine's sync
		// fails when the reference angle has no decodable audio: the
		// wizard then falls back to source timecode deltas (absolute
		// timestamps, so the first angle is the alignment reference).
		let offsets: Vec<f64> = match sync_mode {
			0 => {
				let synced = self
					.engine
					.read(cx)
					.multicam_wizard_sync_offsets(&selection);
				match synced {
					Ok(offsets) => {
						let reference_tc = selection[0].source_timecode;
						let mut by_id = std::collections::HashMap::new();
						for off in &offsets {
							by_id.insert(off.footage, off.offset_s);
						}
						selection
							.iter()
							.map(|f| {
								by_id.get(&f.id).copied().unwrap_or_else(|| {
									// No audio-sync answer for this angle:
									// the source timecode delta relative to
									// the reference (the fallback).
									let ref_tc = reference_tc.unwrap_or(0);
									f.source_timecode.unwrap_or(0) as f64 / 1000.0
										- ref_tc as f64 / 1000.0
								})
							})
							.collect()
					}
					Err(_) => {
						let reference_tc = selection[0].source_timecode.
							map(|t| t as f64 / 1000.0).unwrap_or(0.0);
						selection
							.iter()
							.map(|f| {
								f.source_timecode.map(|t| t as f64 / 1000.0)
									.unwrap_or(0.0)
									- reference_tc
							})
							.collect()
					}
				}
			}
			1 => {
				let reference_tc = selection[0].source_timecode.map(|t| t as f64 / 1000.0)
					.unwrap_or(0.0);
				selection
					.iter()
					.map(|f| {
						f.source_timecode.map(|t| t as f64 / 1000.0).unwrap_or(0.0)
							- reference_tc
					})
					.collect()
			}
			_ => vec![0.0; selection.len()],
		};
		let result = self
			.engine
			.update(cx, |engine, cx| {
				engine.multicam_create_sequence(selection, offsets, name, cx)
			});
		result.map(|_| ())
	}

	/// Opens the export dialog.
	/// Opens the 导出序列 dialog: any sequence of the open project is
	/// pickable (the current one preselected, or `preselect` when the
	/// project explorer's 导出序列 context item opened the dialog).
	fn open_export_dialog(&mut self, preselect: Option<u64>, cx: &mut Context<Self>) {
		let sequences = self.engine.read(cx).sequence_entries();
		if sequences.is_empty() {
			println!("[export] the project has no sequences");
			return;
		}
		let selected = preselect
			.or_else(|| self.engine.read(cx).current_sequence_id())
			.filter(|id| sequences.iter().any(|(entry, _)| entry == id));
		let default_path = self.default_export_path(cx, &sequences, selected);
		self.spawn_modal(cx, move |window, app| {
			let content = app.new(|cx| ExportDialogContent::new(window, cx));
			content.update(app, |content, cx| {
				content.set_sequences(sequences, selected, cx);
				content.set_path(default_path.clone(), cx)
			});
			let modal = app.new(|cx| {
				Modal::new(
					modal_ids::EXPORT,
					ModalOptions::new(crate::i18n::tr("export.title"), px(440.0))
						.with_button(DialogButton::primary(crate::i18n::tr("export.run")))
						.with_button(DialogButton::cancel(crate::i18n::tr("dialog.cancel"))),
					window,
					cx,
				)
				.with_content(content.clone())
			});
			ModalState::Export { modal, content }
		});
	}

	/// A default output path for the export dialog: the picked sequence's
	/// name with the format's extension, next to the project file.
	fn default_export_path(
		&self,
		cx: &App,
		sequences: &[(u64, SharedString)],
		selected: Option<u64>,
	) -> String {
		let seq_name = selected
			.and_then(|id| sequences.iter().find(|(entry, _)| *entry == id))
			.or_else(|| sequences.first())
			.map(|(_, name)| name.to_string());
		let project = self.engine.read(cx).project();
		let name = seq_name
			.filter(|n| !n.is_empty())
			.or_else(|| project.map(|p| p.name.clone()).filter(|n| !n.is_empty()))
			.unwrap_or_else(|| "untitled".to_string());
		let dir = project
			.and_then(|p| p.path.parent().map(|d| d.to_path_buf()))
			.unwrap_or_else(|| PathBuf::from("."));
		dir.join(format!("{name}.mp4"))
			.to_string_lossy()
			.into_owned()
	}

	/// Starts the export from the export dialog's state and swaps the dialog
	/// for the progress dialog.
	fn begin_export(&mut self, cx: &mut Context<Self>) {
		let ModalState::Export { content, .. } = &self.modal else {
			return;
		};
		let settings = content.read(cx).settings(cx);
		let ext = content.read(cx).extension(cx);
		let mut path = content.read(cx).path(cx).to_string();
		if path.trim().is_empty() {
			return;
		}
		let Some(sequence) = content.read(cx).selected_sequence(cx) else {
			return;
		};
		// Append the format's extension when the user left it off.
		let has_ext = std::path::Path::new(&path)
			.extension()
			.map(|e| !e.to_string_lossy().is_empty())
			.unwrap_or(false);
		if !has_ext {
			path = format!("{path}.{ext}");
		}

		let result = self.engine.update(cx, |engine, _cx| {
			engine.start_export_of(sequence, &settings, PathBuf::from(&path))
		});
		match result {
			Ok(session) => {
				self.export = Some(ExportRun { session });
				self.spawn_modal(cx, |window, app| {
					let (modal, content) = progress_dialog(
						modal_ids::EXPORT_PROGRESS,
						crate::i18n::tr("export.progress.title"),
						crate::i18n::tr("export.progress.label"),
						window,
						app,
					);
					ModalState::Progress { modal, content }
				});
			}
			Err(err) => {
				println!("[export] failed to start: {err}");
				self.close_modal(cx);
			}
		}
	}

	/// Cancels the running export (the task aborts at the next frame).
	fn cancel_export(&mut self, cx: &mut Context<Self>) {
		if let Some(run) = &self.export {
			(run.session.cancel)();
		}
		let _ = cx;
	}

	/// Drains the export progress events on the tick loop: updates the
	/// progress bar and closes the dialog when the task finishes.
	fn poll_export(&mut self, cx: &mut Context<Self>) {
		let Some(run) = &self.export else {
			return;
		};
		let mut events = Vec::new();
		while let Ok(event) = run.session.events.try_recv() {
			events.push(event);
		}
		if events.is_empty() {
			return;
		}
		let mut finished: Option<(bool, String)> = None;
		for event in events {
			match event {
				crate::oakui::ExportEvent::Started => {}
				crate::oakui::ExportEvent::Progress(fraction) => {
					if let ModalState::Progress { content, .. } = &self.modal {
						let fraction = fraction as f32;
						content.update(cx, |content, cx| content.set_progress(fraction, cx));
					}
				}
				crate::oakui::ExportEvent::Finished(ok, err) => finished = Some((ok, err)),
			}
		}
		if let Some((ok, err)) = finished {
			self.export = None;
			self.modal = ModalState::None;
			if ok {
				println!("[export] finished");
			} else {
				println!("[export] failed: {err}");
			}
			cx.notify();
		}
	}

	/// Routes a modal dialog event.
	fn on_modal(&mut self, event: &ModalEvent, cx: &mut Context<Self>) {
		match event {
			ModalEvent::ButtonClicked { control, button } => match *control {
				modal_ids::EXPORT => {
					if *button == 0 {
						self.begin_export(cx);
					} else {
						self.close_modal(cx);
					}
				}
				modal_ids::EXPORT_PROGRESS => {
					if *button == 1 {
						// Cancel button: ask the task to abort; the finished
						// event closes the dialog.
						self.cancel_export(cx);
					}
				}
				modal_ids::PLUGIN_PROGRESS => {
					if *button == 1 {
						// Cancel the plugin render; the reporter then answers
						// false and the render aborts at the next progress
						// update (or the dialog closes on the 1.0 fraction).
						crate::oakui::ofx::cancel_plugin_render();
						self.plugin_progress_open = false;
						self.close_modal(cx);
					}
				}
				modal_ids::PREFERENCES => {
					self.commit_preferences(cx);
					self.close_modal(cx);
				}
				modal_ids::MANAGER => self.close_modal(cx),
				modal_ids::MANAGER_RENAME => {
					if *button == 0 {
						self.confirm_manager_rename(cx);
					} else {
						self.back_to_manager(cx);
					}
				}
				modal_ids::MANAGER_DELETE => {
					if *button == 0 {
						self.confirm_manager_delete(cx);
					} else {
						self.back_to_manager(cx);
					}
				}
				modal_ids::PROXY => {
					if let ModalState::Proxy { content, .. } = &self.modal {
						let content = content.clone();
						match *button {
							0 => content.update(cx, |dialog, cx| dialog.generate(cx)),
							1 => content.update(cx, |dialog, cx| dialog.delete(cx)),
							_ => {
								content.update(cx, |dialog, cx| dialog.accept(cx));
								self.close_modal(cx);
							}
						}
					}
				}
				modal_ids::ABOUT => self.close_modal(cx),
				modal_ids::PROJECT_PROPERTIES => {
					if let ModalState::ProjectProperties { content, .. } = &self.modal {
						let content = content.clone();
						if *button == 0 {
							// OK: apply the settings; an invalid OCIO config
							// keeps the dialog open with the error shown.
							match content.update(cx, |dialog, cx| dialog.commit(cx)) {
								Ok(()) => self.close_modal(cx),
								Err(err) => {
									content
										.update(cx, |dialog, cx| dialog.set_error(Some(err), cx));
								}
							}
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::NEW_SEQUENCE => {
					if let ModalState::NewSequence { content, .. } = &self.modal {
						let content = content.clone();
						if *button == 0 {
							// OK: create the sequence; a rejected create
							// keeps the dialog open with the error shown.
							match content.update(cx, |dialog, cx| dialog.commit(cx)) {
								Ok(()) => {
									self.close_modal(cx);
									// The manual path of the drop-onto-empty-
									// timeline flow resumes the paused drop
									// now that a sequence is open.
									self.resume_pending_drop(cx);
								}
								Err(err) => {
									content
										.update(cx, |dialog, cx| dialog.set_error(Some(err), cx));
								}
							}
						} else {
							// Cancel: any paused drop is abandoned too.
							self.pending_drop = None;
							self.close_modal(cx);
						}
					}
				}
				modal_ids::DROP_SEQUENCE_CHOICE => {
					// 0 = handle it from the footage probe, 1 = manual
					// dialog, 2 = cancel.
					match *button {
						0 => self.resolve_drop_sequence_choice(
							DropSequenceChoice::UseFootageParams,
							cx,
						),
						1 => self.resolve_drop_sequence_choice(
							DropSequenceChoice::SpecifyManually,
							cx,
						),
						_ => {
							// Cancel: abandon the paused drop.
							self.pending_drop = None;
							self.close_modal(cx);
						}
					}
				}
				modal_ids::MULTICAM_WIZARD => {
					if let ModalState::MulticamWizard { content, .. } = &self.modal {
						let content = content.clone();
						if *button == 0 {
							// OK: create the multicam; a rejected create
							// keeps the dialog open.
							match self.commit_multicam_wizard(&content, cx) {
								Ok(()) => self.close_modal(cx),
								Err(err) => {
									println!("[multicam wizard] {err}");
								}
							}
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::RENAME => {
					if let ModalState::EntryRename { content, entry_id, .. } = &self.modal {
						if *button == 0 {
							// OK: apply the new name.
							let name = content.read(cx).value(cx);
							let id = *entry_id;
							let engine = self.engine.clone();
							engine.update(cx, |engine, cx| {
								engine.rename_entry(id, name.to_string(), cx)
							});
							self.close_modal(cx);
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::EXPORT_PROJECT_DFD => {
					if let ModalState::ExportProjectDfd { content, .. } = &self.modal {
						if *button == 0 {
							// OK: keep the chosen format; the SYSTEM save
							// dialog then picks the output path (the
							// suggested name carries the format's
							// extension).
							let format = content.read(cx).format(cx);
							let ext = match format {
								crate::dialogs::PROJECT_FORMAT_OVE => "ove",
								crate::dialogs::PROJECT_FORMAT_FCPXML => "fcpxml",
								_ => "otio",
							};
							let suggested = self.export_suggested_name.clone();
							let receiver = cx.prompt_for_new_path(
								std::path::Path::new("."),
								Some(&format!("{suggested}.{ext}")),
							);
							self.close_modal(cx);
							cx.spawn(async move |this, cx| {
								if let Ok(Ok(Some(path))) = receiver.await {
									this.update(cx, |this, cx| {
										let engine = this.engine.clone();
										engine.update(cx, |engine, cx| {
											engine.export_project_path(path, cx)
										});
									});
								}
							})
							.detach();
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::NEW_PROJECT => {
					if let ModalState::NewProject { content, .. } = &self.modal {
						if *button == 0 {
							let content = content.clone();
							self.commit_new_project(&content, cx);
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::SAVE_AS => {
					if let ModalState::SaveAs { content, .. } = &self.modal {
						if *button == 0 {
							let content = content.clone();
							self.commit_save_as(&content, cx);
						} else {
							self.close_modal(cx);
						}
					}
				}
				modal_ids::SEQUENCE_PROPERTIES => {
					if let ModalState::SequenceProperties { content, .. } = &self.modal {
						let content = content.clone();
						if *button == 0 {
							// OK: apply the edits; a rejected update keeps
							// the dialog open with the error shown.
							match content.update(cx, |dialog, cx| dialog.commit(cx)) {
								Ok(()) => self.close_modal(cx),
								Err(err) => {
									content
										.update(cx, |dialog, cx| dialog.set_error(Some(err), cx));
								}
							}
						} else {
							self.close_modal(cx);
						}
					}
				}
				_ => {}
			},
			ModalEvent::Dismissed { control } => match *control {
				modal_ids::EXPORT_PROGRESS => {
					// Escape cancels the running export and closes the dialog.
					self.cancel_export(cx);
					self.close_modal(cx);
				}
				// Escape from a manager sub-dialog returns to the manager.
				modal_ids::MANAGER_RENAME | modal_ids::MANAGER_DELETE => {
					self.back_to_manager(cx);
				}
				modal_ids::PREFERENCES => {
					self.commit_preferences(cx);
					self.close_modal(cx);
				}
				// Escape closes the proxy dialog without applying (the
				// Close button is the apply path, like the C++ accept()).
				modal_ids::PROXY => self.close_modal(cx),
				// Escape / backdrop close the project properties dialog
				// without applying (only OK commits).
				modal_ids::PROJECT_PROPERTIES => self.close_modal(cx),
				// Escape from the drop choice (or the manual new-sequence
				// dialog it spawned) abandons the paused drop.
				modal_ids::DROP_SEQUENCE_CHOICE | modal_ids::NEW_SEQUENCE => {
					self.pending_drop = None;
					self.close_modal(cx);
				}
				_ => self.close_modal(cx),
			},
		}
	}
}

impl<E: AppEngine> Render for OakApp<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let mut root = div()
			.size_full()
			.flex()
			.flex_col()
			// The shell paints the design's near-black base behind the dock;
			// panels and chrome layer their own fills on top.
			.bg(cx.default_colors().background)
			.track_focus(&self.shell_focus);
		// The shell's keyboard dispatch layer: the global key bindings
		// (registered in `new` from the action registry) dispatch gpui
		// actions, which bubble up from the focused widget and land here —
		// one listener per registry action, all routing through the same
		// `dispatch_action_id` the menu clicks use.
		for entry in crate::actions::REGISTRY {
			let action = entry.action;
			root = root.on_boxed_action(
				&*(entry.build)(),
				cx.listener(move |this, _action: &dyn gpui::Action, _window, cx| {
					cx.stop_propagation();
					this.on_action_dispatched(action, cx);
				}),
			);
		}
		let mut root = root
			.child(self.menu_bar.clone())
			.child(div().flex_1().min_h_0().child(self.dock.clone()))
			.child(self.status_bar.clone());
		if let Some(modal) = self.modal.modal_entity() {
			root = root.child(modal);
		}
		root
	}
}

/// The nine dockable panels in the 窗口 menu's order, each with the registry
/// action that toggles it.
const WINDOW_PANELS: [(PanelId, crate::actions::ActionId); 9] = [
	(PROJECT, crate::actions::ActionId::FocusProject),
	(SOURCE_VIEWER, crate::actions::ActionId::FocusSourceViewer),
	(PROGRAM_VIEWER, crate::actions::ActionId::FocusProgramViewer),
	(NODE_EDITOR, crate::actions::ActionId::FocusNodeEditor),
	(INSPECTOR, crate::actions::ActionId::FocusInspector),
	(HISTORY, crate::actions::ActionId::FocusHistory),
	(TIMELINE, crate::actions::ActionId::FocusTimeline),
	(EFFECT_LIBRARY, crate::actions::ActionId::FocusEffectLibrary),
	(MULTICAM, crate::actions::ActionId::FocusMulticam),
];

/// The bit for `panel` in [`MenuState::open_panels`] (one bit per `PanelId`).
fn panel_bit(id: PanelId) -> u16 {
	1u16 << (id.raw() as u16)
}

/// Applies the design's default split ratios and active tabs to `layout`,
/// shared by [`OakApp::new`] and the 窗口 → 重置布局 action.
///
/// Viewers 60% / timeline 40%, project bin 17% of the row — the timeline
/// share leaves room for all four tracks (V2/V1 video + A1/A2 audio) plus
/// the ruler and toolbar at 1600×900, the viewers keep the remaining ~60%.
/// The program viewer's transport row plus its 26px meter strip needs ~430px
/// at 1600×900, so the source/program and program/inspector boundaries are
/// tilted (0.52 / 0.62) to keep the transport's trailing toggles unclipped.
/// The program viewer and project bin end up active in their groups.
fn tune_default_layout(layout: &mut DockLayout) {
	layout.resize_split(&NodePath(vec![]), 0.60);
	layout.resize_split(&NodePath(vec![0]), 0.17);
	layout.resize_split_child(&NodePath(vec![0]), 1, 0.52);
	layout.resize_split_child(&NodePath(vec![0]), 2, 0.62);
	if let Some(path) = layout.find_panel(PROGRAM_VIEWER) {
		layout.set_tabs_active(&path, PROGRAM_VIEWER);
	}
	if let Some(path) = layout.find_panel(PROJECT) {
		layout.set_tabs_active(&path, PROJECT);
	}
}

/// The design's default dock position for each panel, mirroring the seeding
/// in [`OakApp::new`] — 项目 | 素材查看器 | 序列查看器+节点编辑器 |
/// 检查器+历史记录 row, timeline full width at the bottom. Used to re-open a
/// closed panel when its last known position's anchor is gone.
fn default_dock_target(id: PanelId) -> Option<DropTarget> {
	let t = |panel: PanelId, zone: DropZone| DropTarget {
		panel: Some(panel),
		zone,
	};
	match id {
		PROJECT => None,
		EFFECT_LIBRARY => Some(t(PROJECT, DropZone::Center)),
		SOURCE_VIEWER => Some(t(PROJECT, DropZone::Right)),
		PROGRAM_VIEWER => Some(t(SOURCE_VIEWER, DropZone::Right)),
		NODE_EDITOR => Some(t(PROGRAM_VIEWER, DropZone::Center)),
		INSPECTOR => Some(t(PROGRAM_VIEWER, DropZone::Right)),
		HISTORY => Some(t(INSPECTOR, DropZone::Center)),
		TIMELINE => Some(DropTarget {
			panel: None,
			zone: DropZone::Bottom,
		}),
		MULTICAM => Some(t(SOURCE_VIEWER, DropZone::Center)),
		_ => Some(t(PROJECT, DropZone::Center)),
	}
}

/// The 窗口 menu's open-panel bitmask for `dock`'s current visible panel set
/// (docked or floating). Shared by [`OakApp::rebuild_menu_bar`] and tests.
fn open_panels_mask(dock: &DockArea) -> u16 {
	WINDOW_PANELS
		.iter()
		.map(|(id, _)| {
			if dock.is_panel_visible(*id) {
				panel_bit(*id)
			} else {
				0
			}
		})
		.fold(0, |a, b| a | b)
}

/// The dynamic menu-bar state the registry-driven menu tree reads its
/// checkmarks from (theme, active tool, snapping, loop, show-all,
/// full-screen, and which dock panels are open).
#[derive(Clone, Copy)]
struct MenuState {
	dark: bool,
	active_tool: Tool,
	snapping: bool,
	loop_playback: bool,
	show_all: bool,
	full_screen: bool,
	use_proxy_media: bool,
	/// Bitmask of the dock panels currently visible (docked or floating), one
	/// bit per [`PanelId`] via [`panel_bit`]. Read from the live dock area at
	/// menu-bar build time.
	open_panels: u16,
}

impl MenuState {
	/// The shell's startup state (the timeline widget defaults to snapping
	/// on, the pointer tool is active; every panel starts docked, so all bits
	/// are set).
	fn new(dark: bool) -> Self {
		Self {
			dark,
			active_tool: Tool::Pointer,
			snapping: true,
			loop_playback: false,
			show_all: false,
			full_screen: false,
			use_proxy_media: oak_core::configstore::ConfigStore::instance().get_bool(
				None,
				"UseProxyMedia",
				1,
			) != 0,
			open_panels: WINDOW_PANELS
				.iter()
				.map(|(id, _)| panel_bit(*id))
				.fold(0, |a, b| a | b),
		}
	}
}

/// One menu item straight from the registry — delegates to
/// [`menu::action_item`](menu::action_item) so the
/// menu bar and the context menus build items the same way.
fn menu_item(action: ActionId) -> MenuItem {
	menu::action_item(action)
}

/// Builds the menu bar entries (文件/编辑/视图/回放/序列/窗口/工具/帮助) from
/// the action registry (`src/actions.rs`): this function decides placement,
/// grouping and separators, while ids, labels and shortcut annotations all
/// come from the registry — so a menu click and a key press can never
/// diverge. All labels come from the [`crate::i18n`] tables, so rebuilding
/// the menu bar after a language switch repaints it in the new language;
/// `state` drives the dynamic checkmarks (theme, tool, snapping, loop, …).
fn make_menus(state: MenuState) -> Vec<MenuBarEntry> {
    use crate::actions::ActionId as A;
    use crate::i18n::tr;

    let theme_submenu = Menu::new(vec![
		menu_item(A::ThemeDark).with_checked(state.dark),
		menu_item(A::ThemeLight).with_checked(!state.dark),
	]);

	// 工具: the mutually exclusive tool group in the registry's order, the
	// add tool growing its addable-items submenu, then snapping + proxy.
	let mut tools: Vec<MenuItem> = Vec::new();
	for tool in [
		Tool::Pointer,
		Tool::TrackSelect,
		Tool::Edit,
		Tool::Ripple,
		Tool::Rolling,
		Tool::Razor,
		Tool::Slip,
		Tool::Slide,
		Tool::Hand,
		Tool::Zoom,
		Tool::Transition,
		Tool::Add,
		Tool::Record,
	] {
		let mut item = menu_item(tool.action()).with_checked(tool == state.active_tool);
		if tool == Tool::Add {
			item = item.with_submenu(Menu::new(vec![
				menu_item(A::AddEmpty),
				menu_item(A::AddBars),
				menu_item(A::AddShape),
				menu_item(A::AddSolid),
				menu_item(A::AddTitle),
				menu_item(A::AddTone),
				menu_item(A::AddSubtitle),
			]));
		}
		tools.push(item);
	}
	tools.push(
		menu_item(A::Snapping)
			.with_checked(state.snapping)
			.separated(),
	);
	tools.push(menu_item(A::UseProxyMedia).with_checked(state.use_proxy_media));
	tools.push(menu_item(A::ProxySettings));
	// 首选项 belongs to the Tools menu (the C++ layout); on macOS it ALSO
	// stays here (the Qt menu-role relocation does not exist in gpui).
	tools.push(menu_item(A::Preferences));

	vec![
		MenuBarEntry::new(
			tr("menu.file"),
			Menu::new(vec![
				// 新建 ▸ group: the parent carries the first child's id (the
				// same shape the theme/language submenus use).
				MenuItem::new(A::NewProject.menu_id(), tr("menu.file.new")).with_submenu(
					Menu::new(vec![
						menu_item(A::NewProject),
						menu_item(A::NewSequence),
						menu_item(A::NewFolder),
					]),
				),
				menu_item(A::OpenFromLibrary),
				menu_item(A::OpenProject),
				// 最近打开 ▸ (no recent-files backend yet: only the clear item).
				MenuItem::new(A::ClearOpenRecent.menu_id(), tr("menu.file.open_recent"))
					.with_submenu(Menu::new(vec![menu_item(A::ClearOpenRecent)])),
				menu_item(A::ProjectManager).separated(),
				menu_item(A::Import),
				menu_item(A::SaveProject),
				menu_item(A::SaveProjectAs),
				menu_item(A::Revert),
				menu_item(A::Export).separated(),
				menu_item(A::ProjectProperties),
				menu_item(A::CloseProject).separated(),
				menu_item(A::Exit),
			]),
		),
		MenuBarEntry::new(
			tr("menu.edit"),
			Menu::new(vec![
				menu_item(A::Undo),
				menu_item(A::Redo).separated(),
				menu_item(A::Cut),
				menu_item(A::Copy),
				menu_item(A::Paste),
				menu_item(A::PasteInsert),
				menu_item(A::Duplicate).separated(),
				menu_item(A::Rename),
				menu_item(A::Delete),
				menu_item(A::RippleDelete).separated(),
				menu_item(A::SplitAtPlayhead),
				menu_item(A::SpeedDuration),
				menu_item(A::DefaultTransition),
				menu_item(A::LinkUnlink),
				menu_item(A::EnableDisable),
				menu_item(A::Nest).separated(),
				menu_item(A::SelectAll),
				menu_item(A::DeselectAll).separated(),
				menu_item(A::Insert),
				menu_item(A::Overwrite).separated(),
				menu_item(A::RippleToIn),
				menu_item(A::RippleToOut),
				menu_item(A::EditToIn),
				menu_item(A::EditToOut).separated(),
				menu_item(A::NudgeLeft),
				menu_item(A::NudgeRight).separated(),
				menu_item(A::MoveInToPlayhead),
				menu_item(A::MoveOutToPlayhead).separated(),
				menu_item(A::SetInPoint),
				menu_item(A::SetOutPoint),
				menu_item(A::ResetIn),
				menu_item(A::ResetOut),
				menu_item(A::ClearInOut).separated(),
				menu_item(A::DeleteInOut),
				menu_item(A::RippleDeleteInOut).separated(),
				menu_item(A::Marker),
			]),
		),
		MenuBarEntry::new(
			tr("menu.view"),
			Menu::new(vec![
				menu_item(A::ThemeDark).with_submenu(theme_submenu),
				menu::language_menu(),
				menu_item(A::ZoomIn).separated(),
				menu_item(A::ZoomOut),
				menu_item(A::IncreaseTrackHeight),
				menu_item(A::DecreaseTrackHeight).separated(),
				menu_item(A::ToggleShowAll).with_checked(state.show_all),
				menu_item(A::FullScreen).with_checked(state.full_screen),
				menu_item(A::FullScreenViewer),
			]),
		),
		MenuBarEntry::new(
			tr("menu.playback"),
			Menu::new(vec![
				menu_item(A::PlayPause),
				menu_item(A::PlayInToOut),
				menu_item(A::Loop)
					.with_checked(state.loop_playback)
					.separated(),
				menu_item(A::ShuttleLeft),
				menu_item(A::ShuttleStop),
				menu_item(A::ShuttleRight).separated(),
				menu_item(A::PrevFrame),
				menu_item(A::NextFrame).separated(),
				menu_item(A::GoToStart),
				menu_item(A::GoToEnd),
				menu_item(A::GoToPrevCut),
				menu_item(A::GoToNextCut).separated(),
				menu_item(A::GoToIn),
				menu_item(A::GoToOut),
			]),
		),
		MenuBarEntry::new(
			tr("menu.sequence"),
			Menu::new(vec![
				menu_item(A::AddVideoTrack),
				menu_item(A::AddAudioTrack),
				menu_item(A::RemoveTrack).separated(),
				menu_item(A::SetWorkArea),
				menu_item(A::ClearWorkArea).separated(),
				menu_item(A::RemoveMarker).separated(),
				menu_item(A::SeqCache),
				menu_item(A::SeqCacheInOut),
				menu_item(A::SeqCacheClear).separated(),
				menu_item(A::SequenceSettings).disabled(),
			]),
		),
		MenuBarEntry::new(
			tr("menu.window"),
			Menu::new({
				// Every panel is listed, checked when it is currently visible
				// in the dock (docked or floating); clicking toggles it open
				// or closed.
				let mut window_items: Vec<MenuItem> = WINDOW_PANELS
					.iter()
					.map(|(id, action)| {
						menu_item(*action).with_checked(state.open_panels & panel_bit(*id) != 0)
					})
					.collect();
				if let Some(last) = window_items.last_mut() {
					last.separator_after = true;
				}
				window_items.push(menu_item(A::MaximizePanel));
				window_items.push(menu_item(A::ResetDefaultLayout).separated());
				window_items
			}),
		),
		MenuBarEntry::new(tr("menu.tools"), Menu::new(tools)),
		MenuBarEntry::new(
			tr("menu.help"),
			Menu::new(vec![
				menu_item(A::ActionSearch),
				menu_item(A::Feedback).separated(),
				menu_item(A::About),
			]),
		),
	]
}

/// Builds the menu bar with the startup [`MenuState`] (tests).
#[cfg(test)]
pub(crate) fn make_menus_for_test() -> Vec<MenuBarEntry> {
	make_menus(MenuState::new(true))
}

/// The menu-bar *leaf* actions in hierarchy order, each with its localized
/// "Menu > Submenu > …" path. The Keyboard preferences tab and the action
/// search dialog both enumerate the menu bar like the C++ does
/// (`PreferencesKeyboardTab::setup_kbd_shortcuts` /
/// `ActionSearch::search_update`), so the two share one walk. Panel-context
/// hotkeys (the [`HIDDEN_MENU_ID`](crate::actions::HIDDEN_MENU_ID) multicam
/// switches) are excluded by construction — they have no menu item.
pub(crate) fn menu_action_paths() -> Vec<(ActionId, String)> {
	let mut out = Vec::new();
	for entry in make_menus(MenuState::new(true)) {
		let top = entry.title.to_string();
		walk_menu_for_actions(&entry.menu, &top, &mut out);
	}
	out
}

/// Recurses a menu, emitting leaf items (submenu headers extend the path and
/// are not emitted themselves — their id is the first child's id).
fn walk_menu_for_actions(menu: &Menu, path: &str, out: &mut Vec<(ActionId, String)>) {
	for item in &menu.items {
		if let Some(submenu) = &item.submenu {
			let label = item.label.to_string();
			walk_menu_for_actions(submenu, &format!("{path} > {label}"), out);
		} else if let Some(entry) = crate::actions::entry_for_menu_id(item.id) {
			out.push((entry.action, path.to_string()));
		}
	}
}

/// Command-line arguments the app accepts.
#[derive(Debug, Clone, Default)]
struct AppArgs {
	/// A project file to open at startup.
	project: Option<PathBuf>,
	/// Force the mock engine.
	mock: bool,
}

impl AppArgs {
	/// The pure parse step: `oakapp [project.ove] [--mock]`. The first
	/// positional wins; later positionals are ignored. `oak_engine` is
	/// the `OAK_ENGINE` override ("mock", case-insensitive). `--help` is
	/// skipped here — `from_env` handles it (print + exit).
	fn parse_args(
		argv: impl IntoIterator<Item = std::ffi::OsString>,
		oak_engine: Option<&str>,
	) -> Self {
		let mut args = AppArgs::default();
		for arg in argv {
			let text = arg.to_string_lossy();
			match text.as_ref() {
				"--mock" => args.mock = true,
				"--help" | "-h" => {}
				_ if args.project.is_none() => args.project = Some(arg.into()),
				other => println!("[app] ignoring unknown argument {other:?}"),
			}
		}
		if oak_engine
			.map(|v| v.eq_ignore_ascii_case("mock"))
			.unwrap_or(false)
		{
			args.mock = true;
		}
		args
	}

	/// Parses `std::env::args` plus the `OAK_ENGINE` override:
	/// `oakapp [project.ove] [--mock]`.
	fn from_env() -> Self {
		// --help prints and exits before the pure parse.
		if std::env::args_os()
			.skip(1)
			.any(|a| a == "--help" || a == "-h")
		{
			println!("oakapp — Oak Video Editor");
			println!("usage: oakapp [project.ove] [--mock]");
			println!("  --mock   use the mock engine (or set OAK_ENGINE=mock)");
			std::process::exit(0);
		}
		Self::parse_args(
			std::env::args_os().skip(1),
			std::env::var("OAK_ENGINE").ok().as_deref(),
		)
	}
}

/// Builds the app root entity for the chosen backend.
fn build_root<E: AppEngine>(
	window: &mut Window,
	initial: Option<PathBuf>,
	cx: &mut App,
) -> Entity<OakApp<E>> {
	cx.new(|cx| OakApp::new(window, initial, cx))
}

/// The crate entry point: applies the olive-dark theme and opens the main
/// window, running on the real engine by default (mock with `--mock` /
/// `OAK_ENGINE=mock` / the `mock-engine` feature).
pub fn run() {
	let args = AppArgs::from_env();
	let use_mock = args.mock || cfg!(feature = "mock-engine");
	if use_mock {
		run_with::<MockEngine>(args.clone());
	} else {
		run_with::<RealEngine>(args);
	}
}

/// Runs the app window with `E` as the engine backend.
fn run_with<E: AppEngine>(args: AppArgs) {
	let initial = args.project.clone();
	gpui_platform::application().run(move |cx: &mut App| {
		// Load the persisted preferences (config.ini) before anything reads
		// them — the language, the theme, the storage backend and the audio
		// devices all come from the config store.
		crate::oakui::real::config_load();
		// Restore the persisted UI language (config `Language` key) before the
		// first window renders.
		crate::i18n::init();
		// M13 D4: enable the write-through project library (SQLite at the
		// default location) unless the user configured the backend
		// explicitly.
		crate::oakui::real::configure_storage();
		// Bring up the audio manager and apply the persisted device choices
		// (without an instance, playback pushes fail silently).
		crate::oakui::real::audio_init_from_config();
		// Stage 6b: wire the optional OFX plugin host — scan the standard
		// plugin paths, register every discovered plugin into the node
		// factory (effect library / add-effect menu), install the render
		// executor, and register the progress / viewer-time bridges. All
		// failures degrade to logs; plugins are optional.
		let plugin_count = crate::oakui::ofx::init();
		if plugin_count > 0 {
			println!("[ofx] registered {plugin_count} OFX plugin node type(s)");
		}
		// Display color management: the platform layer must know who maps
		// the pixels to the display (the single-mapping rule). Declared
		// per-window below, right after the window exists, and re-declared
		// whenever the preference changes (see displaycolor::apply_to_window).
		// Display bit depth: the wgpu window layer must pick the swapchain
		// format before the surface is created, so surface the persisted
		// choice through the process environment. Read by gpui_wgpu as
		// OAK_DISPLAY_BIT_DEPTH ("8" opts into the 8-bit pair; anything
		// else requests 10-bit).
		let bit_depth = oak_core::backend::DisplayBitDepth::from_config_string(
			&crate::oakui::real::config_get_string(
                oak_core::backend::CONFIG_KEY_DISPLAY_BIT_DEPTH,
			),
		);
		// SAFETY: single-threaded startup, before any window exists.
		unsafe { std::env::set_var("OAK_DISPLAY_BIT_DEPTH", bit_depth.to_config_string()) };
		cx.init_colors();
		let bounds = Bounds::centered(None, size(px(1600.0), px(900.0)), cx);
		let initial = initial.clone();
		let show_manager = initial.is_none();
		// The window/taskbar icon, per platform: X11 takes the pixels up
		// front (_NET_WM_ICON, decoded here from the embedded PNG);
		// Wayland resolves the icon from oak.desktop via the app id;
		// Windows reads it from the exe's icon resource (see build.rs).
		let icon = image::load_from_memory(include_bytes!("../../../assets/appicon/icon.png"))
			.expect("the embedded app icon must decode")
			.into_rgba8();
		let mut root_slot = None;
		let window = cx
			.open_window(
				WindowOptions {
					window_bounds: Some(WindowBounds::Windowed(bounds)),
					app_id: Some("oak".into()),
					icon: Some(Arc::new(icon)),
					..Default::default()
				},
				|window, cx| {
					// Declare who maps this window's pixels to the display
					// before the first frame paints.
					crate::oakui::displaycolor::apply_to_window(window);
					// The 10-bit display path: hand the window's wgpu device
					// to the engine so it can upload RGBA16F textures gpui's
					// renderer samples straight into the swapchain (no 8-bit
					// quantization). Linux/FreeBSD only — `gpu_context` does
					// not exist on other platforms, where the BGRA8 CPU path
					// stays in effect.
					#[cfg(any(target_os = "linux", target_os = "freebsd"))]
					if let Some(Ok(ctx)) = window
						.gpu_context()
						.map(|c| c.downcast::<(Arc<wgpu::Device>, Arc<wgpu::Queue>)>())
					{
						crate::oakui::gpu::register_context(ctx.0, ctx.1);
					}
					// Compact pro-app text metrics: gpui's default rem is
					// 16px (desktop-app large); 14px matches the design's
					// density. All rem-based text scales; px spacing is
					// unaffected.
					window.set_rem_size(px(14.0));
					let root = build_root::<E>(window, initial, cx);
					root_slot = Some(root.clone());
					root
				},
			)
			.expect("failed to open the main window");
		let _ = window;

		// No project on the command line: the DaVinci-style project manager
		// greets instead of an empty shell. Drives the ROOT ENTITY (not the
		// window handle): a window update borrows the window, and building a
		// modal inside it would re-enter it (spawn_modal needs a free
		// `update_window`).
		if show_manager {
			if let Some(root) = &root_slot {
				root.update(cx, |app, cx| app.show_project_manager(cx));
			}
		}

		cx.activate(true);
		cx.on_window_closed(|cx, _| {
			if cx.windows().is_empty() {
				// Persist the preferences (config.ini), then drain the
				// write-through backlog (save + snapshot of every still-bound
				// project) and stop oakstorage's snapshot thread.
				crate::oakui::real::config_save();
				crate::oakui::real::storage_flush();
				cx.quit();
			}
		})
		.detach();
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oakui::EngineGateway as _;
    use gpui::timeline::TimelineDataSource as _;
    use gpui::{
        px, size, AnyWindowHandle, ExternalPaths, FileDropEvent, TestAppContext, VisualTestContext,
    };

    /// The 视图/View menu carries a 语言/Language submenu whose items are
	/// labeled in their own language and whose checkmark follows the active
	/// language — and the whole menu bar flips language with `i18n`.
	#[test]
	fn language_menu_tracks_the_active_language() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let view_entry = |dark: bool| -> MenuBarEntry {
			make_menus(MenuState::new(dark))
				.into_iter()
				.find(|entry| entry.title == crate::i18n::tr("menu.view"))
				.expect("视图/View menu exists")
		};

		// The language submenu sits under 视图/View.
		let language_item = |entry: &MenuBarEntry| -> Menu {
			entry
				.menu
				.items
				.iter()
				.find(|item| item.label == crate::i18n::tr("menu.view.language"))
				.map(|item| item.submenu.clone().map(|m| *m).unwrap_or_default())
				.expect("语言/Language submenu exists")
		};

		crate::i18n::set_language_code("en-US");
		let languages = crate::i18n::available_languages();
		let en_index = languages.iter().position(|c| c == "en-US").unwrap();
		let zh_index = languages.iter().position(|c| c == "zh-CN").unwrap();
		let submenu = language_item(&view_entry(true));
		assert_eq!(
			submenu.items.len(),
			languages.len(),
			"one menu item per discovered language pack"
		);
		let zh = submenu
			.items
			.iter()
			.find(|i| i.id == menu::LANG_ITEM_BASE + zh_index)
			.expect("zh item");
		let en = submenu
			.items
			.iter()
			.find(|i| i.id == menu::LANG_ITEM_BASE + en_index)
			.expect("en item");
		assert_eq!(zh.label, "简体中文 (zh-CN)");
		assert_eq!(en.label, "English (en-US)");
		assert_eq!(zh.checked, Some(false));
		assert_eq!(en.checked, Some(true), "en-US is active → checked");

		crate::i18n::set_language_code("zh-CN");
		let submenu = language_item(&view_entry(true));
		let zh = submenu
			.items
			.iter()
			.find(|i| i.id == menu::LANG_ITEM_BASE + zh_index)
			.expect("zh item");
		let en = submenu
			.items
			.iter()
			.find(|i| i.id == menu::LANG_ITEM_BASE + en_index)
			.expect("en item");
		assert_eq!(zh.checked, Some(true), "zh-CN is active → checked");
		assert_eq!(en.checked, Some(false));

		// The menu titles themselves are localized.
		assert_eq!(view_entry(true).title, "视图(V)");
		crate::i18n::set_language_code("en-US");
		assert_eq!(view_entry(true).title, "View(V)");
	}

	/// The theme submenu's checkmark follows the `dark` flag.
	#[test]
	fn theme_menu_checkmark_follows_dark_flag() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let dark_item = |dark: bool| -> menu::MenuItem {
			let entries = make_menus(MenuState::new(dark));
			let view = entries
				.iter()
				.find(|entry| entry.title == crate::i18n::tr("menu.view"))
				.expect("视图/View menu");
			let theme = view
				.menu
				.items
				.iter()
				.find(|i| i.id == menu_ids::THEME_DARK)
				.and_then(|i| i.submenu.clone())
				.expect("theme submenu");
			theme
				.items
				.into_iter()
				.find(|i| i.id == menu_ids::THEME_DARK)
				.expect("Olive Dark item")
		};
		assert_eq!(dark_item(true).checked, Some(true));
		assert_eq!(dark_item(false).checked, Some(false));
	}

	/// The 窗口 menu lists every dockable panel, and each item's checkmark
	/// mirrors the `open_panels` bitmask carried in [`MenuState`] (which the
	/// shell reads from the dock's live visible-panel set).
	#[test]
	fn window_menu_lists_all_panels_and_checks_the_open_ones() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		let mask = panel_bit(INSPECTOR) | panel_bit(TIMELINE);
		let mut state = MenuState::new(true);
		state.open_panels = mask;
		let window = make_menus(state)
			.into_iter()
			.find(|entry| entry.title == crate::i18n::tr("menu.window"))
			.expect("Window menu exists")
			.menu;

		for (panel, action) in WINDOW_PANELS {
			let item = window
				.items
				.iter()
				.find(|item| item.id == action.menu_id())
				.unwrap_or_else(|| panic!("Window menu is missing panel {}", panel.raw()));
			assert_eq!(
				item.checked,
				Some(mask & panel_bit(panel) != 0),
				"panel {} checkmark follows open_panels",
				panel.raw()
			);
		}
	}

	/// 窗口 → 检查器 closes the inspector through the dock's remove flow (the
	/// same path the tab ✕ takes) and re-opens it from the closed state — the
	/// round trip that used to leave a dismissed inspector unrecoverable.
	#[gpui::test]
	async fn window_menu_toggle_closes_and_reopens_the_inspector(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);

		// The checkmark shown in the 窗口 menu for the inspector, derived from
		// the dock's current visible-panel set exactly as `rebuild_menu_bar`
		// computes it.
		let inspector_checked = |app: &App| {
			let dock = root.read(app).dock.read(app);
			let mut state = MenuState::new(true);
			state.open_panels = open_panels_mask(dock);
			make_menus(state)
				.into_iter()
				.find(|e| e.title == crate::i18n::tr("menu.window"))
				.expect("Window menu exists")
				.menu
				.items
				.iter()
				.find(|item| item.id == ActionId::FocusInspector.menu_id())
				.expect("inspector item exists")
				.checked
		};

		// The inspector starts docked and checked.
		assert!(cx.read(|app| root.read(app).dock.read(app).is_docked(INSPECTOR)));
		assert_eq!(cx.read(|app| inspector_checked(app)), Some(true));

		// Toggle closed via the menu action: removed from the dock and
		// unchecked.
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::FOCUS_INSPECTOR, cx)));
		cx.run_until_parked();
		assert!(!cx.read(|app| root.read(app).dock.read(app).is_docked(INSPECTOR)));
		assert!(
			!cx.read(|app| root.read(app).dock.read(app).is_panel_visible(INSPECTOR)),
			"the inspector is fully closed, not floating"
		);
		assert_eq!(cx.read(|app| inspector_checked(app)), Some(false));

		// Toggle open again: the inspector comes back (its entity is reused,
		// so its state survives the close).
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::FOCUS_INSPECTOR, cx)));
		cx.run_until_parked();
		assert!(cx.read(|app| root.read(app).dock.read(app).is_docked(INSPECTOR)));
		assert_eq!(cx.read(|app| inspector_checked(app)), Some(true));
	}

	/// 窗口 → 重置布局 tears the current dock arrangement down and rebuilds
	/// every panel at its design position with the default ratios: panels
	/// dismissed via the 窗口 menu come back, floating (tear-off) panels'
	/// windows are closed for good, and tilted splits are restored.
	#[gpui::test]
	async fn reset_layout_restores_the_default_workspace(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);

		// Distort the workspace on every axis: dismiss two panels, tear the
		// timeline off into its own window, and tilt the root split to 0.5.
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::FOCUS_INSPECTOR, cx)));
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::FOCUS_HISTORY, cx)));
		cx.run_until_parked();
		cx.update(|app| {
			root.update(app, |app, cx| {
				let dock = app.dock.clone();
				dock.update(cx, |dock, cx| {
					assert!(dock.float_panel(TIMELINE, cx));
				});
			});
		});
		cx.run_until_parked();
		cx.update(|app| {
			root.update(app, |app, cx| {
				let mut layout = app.dock.read(cx).layout().clone();
				layout.resize_split(&NodePath(vec![]), 0.5);
				let dock = app.dock.clone();
				dock.update(cx, |dock, cx| dock.set_layout(layout, cx));
			});
		});
		cx.run_until_parked();

		// The distorted state is in place.
		assert_eq!(
			cx.read(|app| {
				let dock = root.read(app).dock.read(app);
				(
					dock.is_docked(INSPECTOR),
					dock.is_docked(HISTORY),
					dock.is_floating(TIMELINE),
					dock
						.layout()
						.split_ratios(&NodePath(vec![]))
						.map(|r| r[0]),
				)
			}),
			(false, false, true, Some(0.5))
		);

		// 窗口 → 重置布局 restores every panel to its design position.
		cx.update(|app| {
			root.update(app, |app, cx| app.on_menu(menu_ids::RESET_DEFAULT_LAYOUT, cx));
		});
		cx.run_until_parked();

		assert!(
			cx.read(|app| {
				let dock = root.read(app).dock.read(app);
				let all_docked =
					WINDOW_PANELS.iter().all(|(panel, _)| dock.is_docked(*panel));
				let none_floating =
					WINDOW_PANELS.iter().all(|(panel, _)| !dock.is_floating(*panel));
				let root_ratio = dock
					.layout()
					.split_ratios(&NodePath(vec![]))
					.map(|r| r[0]);
				all_docked && none_floating && (root_ratio.unwrap_or(0.0) - 0.60).abs() < 1e-4
			}),
			"every panel is docked at the default 0.60 split after the reset"
		);

		// The 窗口 menu's checkmarks follow the restored visible-panel set.
		let mask = cx.read(|app| open_panels_mask(root.read(app).dock.read(app)));
		let mut state = MenuState::new(true);
		state.open_panels = mask;
		let window_menu = make_menus(state)
			.into_iter()
			.find(|e| e.title == crate::i18n::tr("menu.window"))
			.expect("Window menu exists")
			.menu;
		for (panel, action) in WINDOW_PANELS {
			let item = window_menu
				.items
				.iter()
				.find(|item| item.id == action.menu_id())
				.unwrap_or_else(|| panic!("Window menu is missing panel {}", panel.raw()));
			assert_eq!(
				item.checked,
				Some(true),
				"{} is re-checked after the reset",
				panel.raw()
			);
		}
	}

	/// Closing a panel via its tab ✕ — the dock's own close flow — also
	/// refreshes the 窗口 menu: the dock's structural event rebuilds the menu
	/// bar, so the dismissed panel loses its checkmark without waiting for any
	/// other rebuild trigger.
	#[gpui::test]
	async fn closing_a_panel_via_its_tab_unchecks_it_in_the_window_menu(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		let menu_bar_before = cx.read(|app| root.read(app).menu_bar.entity_id());
		assert!(cx.read(|app| root.read(app).dock.read(app).is_docked(INSPECTOR)));

		// Click the inspector's tab close button.
		let mut vcx = VisualTestContext::from_window(window.into(), cx);
		let close = vcx
			.debug_bounds("dock-tab-close-5")
			.expect("inspector tab close button rendered");
		vcx.simulate_click(close.center(), gpui::Modifiers::none());
		drop(vcx);
		cx.run_until_parked();

		// The inspector is gone and the menu bar was rebuilt (new entity) from
		// the dock's shrunken visible-panel set.
		assert!(!cx.read(|app| root.read(app).dock.read(app).is_docked(INSPECTOR)));
		let menu_bar_after = cx.read(|app| root.read(app).menu_bar.entity_id());
		assert_ne!(
			menu_bar_after, menu_bar_before,
			"the dock's structural event rebuilds the menu bar"
		);
	}

	/// The File menu exposes the full project lifecycle actions (new /
	/// open-from-library / open-file / manager / export-project / close /
	/// export) and the Edit menu the undo stack plus the delete variants,
	/// across both languages.
	#[test]
	fn file_and_edit_menus_cover_the_project_lifecycle() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let entry = |title: &str| -> MenuBarEntry {
			make_menus_for_test()
				.into_iter()
				.find(|entry| entry.title == title)
				.expect("menu exists")
		};

		crate::i18n::set_language_code("en-US");
		let file = entry("File(F)");
		for id in [
			menu_ids::NEW_PROJECT,
			menu_ids::OPEN_FROM_LIBRARY,
			menu_ids::OPEN_PROJECT,
			menu_ids::PROJECT_MANAGER,
			menu_ids::EXPORT_PROJECT,
			menu_ids::CLOSE,
			menu_ids::EXPORT,
			menu_ids::QUIT,
		] {
			assert!(
				file.menu.items.iter().any(|item| item.id == id),
				"File menu is missing item {id}"
			);
		}
		let edit = entry("Edit(E)");
		for id in [
			menu_ids::UNDO,
			menu_ids::REDO,
			menu_ids::DELETE,
			menu_ids::RIPPLE_DELETE,
		] {
			assert!(
				edit.menu.items.iter().any(|item| item.id == id),
				"Edit menu is missing item {id}"
			);
		}

		// The same ids exist in the zh-CN menu bar.
		crate::i18n::set_language_code("zh-CN");
		let file = entry("文件(F)");
		assert!(file
			.menu
			.items
			.iter()
			.any(|item| item.id == menu_ids::EXPORT_PROJECT));
		// Restore the default language for the next (lock-free) test.
		crate::i18n::set_language_code("en-US");
	}

	/// Opening 视图 → Preferences… must not crash: the dialog content and the
	/// modal are built on the main window and the shell state swaps over the
	/// entity's weak handle (regression test for the Preferences crash).
	#[gpui::test]
	async fn preferences_dialog_opens_without_crashing(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(1600.0), px(900.0)), |window, cx| {
			OakApp::<MockEngine>::new(window, None, cx)
		});
		cx.run_until_parked();
		let root = window.root(cx).expect("app root");

		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::PREFERENCES, cx)));
		cx.run_until_parked();
		// Force a draw so render-time panics in the dialog content surface.
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");

		let has_modal =
			cx.read(|app| matches!(root.read(app).modal, ModalState::Preferences { .. }));
		assert!(
			has_modal,
			"preferences modal should be shown after the menu action"
		);
	}

	/// The General tab's new rows are live end to end: the cache-ahead spin
	/// box writes `PlaybackPreRenderFrames` (the playback pre-render window
	/// reads it every tick) and the storage backend combo writes
	/// `Storage/Backend` — PostgreSQL reveals the `Storage/PgUrl` field the
	/// write-through library consumes.
	#[gpui::test]
	async fn preferences_general_tab_writes_cache_ahead_and_storage(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		// The config store is process-wide: pin the keys to a known state and
		// restore them afterwards so no other test inherits these writes.
		struct Restore(&'static str, String);
		impl Drop for Restore {
			fn drop(&mut self) {
				crate::oakui::real::config_set_string(self.0, &self.1);
			}
		}
		let _ahead = Restore(
			"PlaybackPreRenderFrames",
			crate::oakui::real::config_get_string("PlaybackPreRenderFrames"),
		);
		let _backend = Restore(
			"Storage/Backend",
			crate::oakui::real::config_get_string("Storage/Backend"),
		);
		let _pg_url = Restore(
			"Storage/PgUrl",
			crate::oakui::real::config_get_string("Storage/PgUrl"),
		);
		crate::oakui::real::config_set_int("PlaybackPreRenderFrames", 120);
		crate::oakui::real::config_set_string("Storage/Backend", "sqlite");

		let (window, root) = mock_shell(cx);
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::PREFERENCES, cx)));
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		cx.run_until_parked();
		// Clear the app keymap: the global up/down/home/end bindings
		// (GoToPrevCut / GoToNextCut / GoToStart / GoToEnd) dispatch before
		// the controls' element key handlers and would shadow the spin
		// box's arrow keys and the combo's selection keys.
		cx.update(|cx| cx.clear_key_bindings());

		let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
		// The connection-string row only exists for the PostgreSQL backend.
		assert!(
			cx.debug_bounds("preferences-storage-pg-url").is_none(),
			"the PostgreSQL connection-string row is hidden for SQLite"
		);

		// Step the cache-ahead spin box once (8 frames): 120 → 128 lands in
		// the config the playback pre-render window reads.
		let ahead = cx
			.debug_bounds("preferences-cache-ahead")
			.expect("the cache-ahead row is rendered");
		cx.simulate_click(ahead.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("up").unwrap());
		cx.run_until_parked();
		assert_eq!(
			crate::oakui::real::config_get_int("PlaybackPreRenderFrames", 120),
			128,
			"the cache-ahead spin box writes its stepped value to the config"
		);

		// Re-select the storage backend: PostgreSQL (the combo starts on
		// SQLite; with the popup closed the arrow keys drive the selection).
		let backend = cx
			.debug_bounds("preferences-storage-backend")
			.expect("the storage backend row is rendered");
		cx.simulate_click(backend.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("escape").unwrap());
		cx.run_until_parked();
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("down").unwrap());
		cx.run_until_parked();
		assert_eq!(
			crate::oakui::real::config_get_string("Storage/Backend"),
			"pg",
			"selecting PostgreSQL writes Storage/Backend to the config"
		);
		// A fresh draw reveals the connection-string row.
		cx.update(|window, cx| window.draw(cx).clear());
		cx.run_until_parked();
		cx.debug_bounds("preferences-storage-pg-url")
			.expect("the PostgreSQL connection-string row appears after the switch");
	}

	/// Opening File → Project Properties… must not crash: the content is
	/// built with the engine and the modal is layered onto the shell (the
	/// same deferred-build path as Preferences).
	#[gpui::test]
	async fn project_properties_dialog_opens_without_crashing(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(1600.0), px(900.0)), |window, cx| {
			OakApp::<MockEngine>::new(window, None, cx)
		});
		cx.run_until_parked();
		let root = window.root(cx).expect("app root");

		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_menu(ActionId::ProjectProperties.menu_id(), cx)
			})
		});
		cx.run_until_parked();
		// Force a draw so render-time panics in the dialog content surface.
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");

		let has_modal =
			cx.read(|app| matches!(root.read(app).modal, ModalState::ProjectProperties { .. }));
		assert!(
			has_modal,
			"project properties modal should be shown after the menu action"
		);
	}

	/// The dialog's OK button applies the chosen cache location to the
	/// engine: choosing 自定义位置 + a path, then clicking OK, lands in
	/// `project_cache_location()` as `(2, path)` and closes the dialog.
	#[gpui::test]
	async fn project_properties_commit_applies_cache_location(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_menu(ActionId::ProjectProperties.menu_id(), cx)
			})
		});
		cx.run_until_parked();
		let content = cx.read(|app| match &root.read(app).modal {
			ModalState::ProjectProperties { content, .. } => content.clone(),
			_ => panic!("project properties modal should be open"),
		});

		// Choose 自定义位置 and type a path.
		cx.update(|app| {
			content.update(app, |dialog, cx| {
				dialog.select_cache_setting(2, cx);
				dialog.set_custom_cache_path("/tmp/oak-cache", cx);
			})
		});
		cx.run_until_parked();

		// Click the OK button (the modal's primary button).
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		cx.run_until_parked();
		let mut vcx = VisualTestContext::from_window(window.into(), cx);
		let ok = vcx
			.debug_bounds("dialog-button-0")
			.expect("OK button rendered");
		vcx.simulate_click(ok.center(), gpui::Modifiers::none());
		drop(vcx);
		cx.run_until_parked();

		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::None)),
			"OK closes the dialog"
		);
		let location = cx.read(|app| root.read(app).engine.read(app).project_cache_location());
		assert_eq!(location, (2, "/tmp/oak-cache".to_string()));
	}

	/// A bogus OCIO config keeps the dialog open: clicking OK runs the
	/// engine's validation, which rejects the path, shows the error label
	/// under the OCIO row and leaves the modal on screen.
	#[gpui::test]
	async fn project_properties_invalid_ocio_keeps_the_dialog_open(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_menu(ActionId::ProjectProperties.menu_id(), cx)
			})
		});
		cx.run_until_parked();
		let content = cx.read(|app| match &root.read(app).modal {
			ModalState::ProjectProperties { content, .. } => content.clone(),
			_ => panic!("project properties modal should be open"),
		});

		// Type a path that cannot load as an OCIO config.
		cx.update(|app| {
			content.update(app, |dialog, cx| {
				dialog.set_ocio_config_path("/nonexistent/ocio/config.ocio", cx);
			})
		});
		cx.run_until_parked();

		// Click OK: the commit fails, the dialog stays open, the error shows.
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		cx.run_until_parked();
		let mut vcx = VisualTestContext::from_window(window.into(), cx);
		let ok = vcx
			.debug_bounds("dialog-button-0")
			.expect("OK button rendered");
		vcx.simulate_click(ok.center(), gpui::Modifiers::none());
		drop(vcx);
		cx.run_until_parked();

		let still_open =
			cx.read(|app| matches!(root.read(app).modal, ModalState::ProjectProperties { .. }));
		assert!(still_open, "an invalid OCIO config keeps the dialog open");
		let error = cx.read(|app| match &root.read(app).modal {
			ModalState::ProjectProperties { content, .. } => content.read(app).error().cloned(),
			_ => None,
		});
		let error = error.expect("the commit error is recorded");
		assert!(
			error.to_lowercase().contains("ocio"),
			"the error mentions the config: {error}"
		);

		// The error label renders under the OCIO row.
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		let mut vcx = VisualTestContext::from_window(window.into(), cx);
		assert!(
			vcx.debug_bounds("projprops-error").is_some(),
			"the OCIO error label renders"
		);
		drop(vcx);
	}

	// -------------------------------------------------------------------
	// Keyboard shortcuts (M12 P5c)
	// -------------------------------------------------------------------

	/// Every registry action with a default key exists as a menu item
	/// (recursing into submenus), so a key press can never dispatch a dead
	/// action.
	#[test]
	fn every_shortcut_maps_to_a_menu_item() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		fn collect(menu: &Menu, out: &mut Vec<usize>) {
			for item in &menu.items {
				out.push(item.id);
				if let Some(sub) = &item.submenu {
					collect(sub, out);
				}
			}
		}
		let mut ids = Vec::new();
		for entry in make_menus_for_test() {
			collect(&entry.menu, &mut ids);
		}
		// Context-menu-only actions (the timeline clip menu's synchronize
		// group) appear in the clip menu instead of the menu bar.
		collect(
			&crate::panels::timeline::clip_menu(
				crate::oakui::engine::SyncEligibility::default(),
				&[],
				None,
			),
			&mut ids,
		);
		for entry in crate::actions::REGISTRY {
			if entry.default_keys.is_empty() {
				continue;
			}
			assert!(
				ids.contains(&entry.menu_id()),
				"shortcut {} → action {} has no menu item",
				entry.default_keys[0],
				entry.cpp_id
			);
		}
	}

	/// The menu's shortcut labels mirror the registry's display strings (a
	/// drift between them would show the user the wrong key). Both sides
	/// come from [`crate::actions::display_shortcut`], so this holds on
	/// every platform.
	#[test]
	fn menu_shortcut_labels_match_the_table() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		fn walk(menu: &Menu) -> Vec<(usize, Option<gpui::SharedString>)> {
			let mut out = Vec::new();
			for item in &menu.items {
				out.push((item.id, item.shortcut.clone()));
				if let Some(sub) = &item.submenu {
					out.extend(walk(sub));
				}
			}
			out
		}
		for entry in make_menus_for_test() {
			for (id, label) in walk(&entry.menu) {
				let Some(label) = label else { continue };
				let Some(registry_entry) = crate::actions::entry_for_menu_id(id) else {
					panic!("menu item {id} shows {label} but is not a registry action");
				};
				let expected = crate::actions::display_shortcut(registry_entry.action)
					.unwrap_or_else(|| panic!("menu item {id} shows {label} but has no shortcut"));
				assert_eq!(
					label.as_ref(),
					expected,
					"shortcut label drift on menu item {id}"
				);
			}
		}
	}

	/// Pressing space on the shell toggles program playback (the keystroke
	/// bubbles to the shell's key listener and dispatches 回放 → 播放/暂停).
	#[gpui::test]
	async fn space_toggles_program_playback(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		let playing = cx.read(|app| root.read(app).program_clock.read(app).is_playing());
		assert!(!playing, "the shell starts paused");

		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("space").unwrap());
		cx.run_until_parked();
		let playing = cx.read(|app| root.read(app).program_clock.read(app).is_playing());
		assert!(playing, "space starts program playback");

		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("space").unwrap());
		cx.run_until_parked();
		let playing = cx.read(|app| root.read(app).program_clock.read(app).is_playing());
		assert!(!playing, "space again pauses program playback");
	}

	/// The registry's default keys dispatch their actions end to end:
	/// shuttle (j/k/l), in/out points (i/o), marker (m), snapping (s),
	/// timeline zoom (=/-), track height (⌘=) and — with the timeline
	/// focused — select-all/deselect-all (⌘A/⌘⇧A) plus the fall-through of
	/// the still-unwired ripple-to-in/out (q/w).
	#[gpui::test]
	async fn keymap_defaults_dispatch_their_actions(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		let key = |keystroke: &str| gpui::Keystroke::parse(keystroke).unwrap();

		// --- shuttle: l plays, k stops, j steps back ----------------------
		settle_key(cx, window.into(), "l");
		assert!(
			cx.read(|app| root.read(app).program_clock.read(app).is_playing()),
			"l starts playback"
		);
		settle_key(cx, window.into(), "k");
		assert!(
			!cx.read(|app| root.read(app).program_clock.read(app).is_playing()),
			"k stops playback"
		);
		for _ in 0..10 {
			settle_key(cx, window.into(), "right");
		}
		settle_key(cx, window.into(), "j");
		assert_eq!(
			cx.read(|app| root.read(app).program_clock.read(app).current_frame()),
			Frame(9),
			"j steps one frame back from frame 10"
		);

		// --- in/out points: i marks the in at 9, o the out at 15 ----------
		cx.dispatch_keystroke(window.into(), key("i"));
		for _ in 0..6 {
			settle_key(cx, window.into(), "right");
		}
		settle_key(cx, window.into(), "o");
		let workarea = cx.read(|app| root.read(app).engine.read(app).workarea());
		assert_eq!(
			workarea,
			Some((Frame(9), Frame(15))),
			"i/o set the work area"
		);

		// --- marker: m adds one at the playhead ---------------------------
		settle_key(cx, window.into(), "m");
		let markers = cx.read(|app| root.read(app).engine.read(app).markers());
		assert!(
			markers.iter().any(|marker| marker.frame == Frame(15)),
			"m adds a marker at the playhead"
		);

		// --- snapping: s toggles the timeline's snap flag ------------------
		settle_key(cx, window.into(), "s");
		assert!(
			!cx.read(|app| root.read(app).timeline.read(app).state.snap_enabled),
			"s toggles snapping off"
		);

		// --- zoom: = zooms in, - zooms out ---------------------------------
		let zoom_before = cx.read(|app| root.read(app).timeline.read(app).state.zoom);
		settle_key(cx, window.into(), "=");
		let zoom_in = cx.read(|app| root.read(app).timeline.read(app).state.zoom);
		assert!(zoom_in > zoom_before, "= zooms the timeline in");
		settle_key(cx, window.into(), "-");
		let zoom_out = cx.read(|app| root.read(app).timeline.read(app).state.zoom);
		assert!(zoom_out < zoom_in, "- zooms the timeline out");

		// --- track height: ⌘= grows the tracks by 8px (24–160px clamp) ----
		let height = |cx: &mut TestAppContext| -> f32 {
			cx.read(|app| {
				root.read(app)
					.engine
					.read(app)
					.track(0)
					.map(|track| f32::from(track.height()))
					.unwrap_or_default()
			})
		};
		let height_before = height(cx);
		settle_key(cx, window.into(), "secondary-=");
		assert_eq!(height(cx), height_before + 8.0, "⌘= grows the tracks");

		// --- focused-panel routing: ⌘A / ⌘⇧A hit the timeline panel --------
		cx.update(|app| root.update(app, |app, _cx| app.focused_panel = Some(TIMELINE)));
		settle_key(cx, window.into(), "secondary-a");
		let selected = cx.read(|app| root.read(app).timeline.read(app).selection().len());
		assert!(selected > 0, "⌘A selects every clip via the timeline panel");
		settle_key(cx, window.into(), "secondary-shift-a");
		let selected = cx.read(|app| root.read(app).timeline.read(app).selection().len());
		assert_eq!(selected, 0, "⌘⇧A deselects via the timeline panel");

		// q/w (ripple-to-in/out) reach the panel but are not wired yet —
		// they fall through to the shell's placeholder, state untouched.
		cx.dispatch_keystroke(window.into(), key("q"));
		settle_key(cx, window.into(), "w");
		let workarea = cx.read(|app| root.read(app).engine.read(app).workarea());
		assert_eq!(
			workarea,
			Some((Frame(9), Frame(15))),
			"q/w are inert placeholders for now"
		);
	}

	/// ⌘Z on the shell dispatches 编辑 → 撤销 to the engine, and ⌘K splits
	/// at the playhead.
	#[gpui::test]
	async fn edit_shortcuts_dispatch_to_the_engine(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		// Move the playhead inside the first clip (the → shortcut steps one
		// frame), then split with ⌘K: the mock sequence grows by one clip.
		for _ in 0..10 {
			settle_key(cx, window.into(), "right");
		}
		let playhead = cx.read(|app| root.read(app).program_clock.read(app).current_frame());
		assert_eq!(playhead, Frame(10), "→ steps the playhead");

		let clips_before: usize = cx.read(|app| {
			let engine = root.read(app).engine.read(app);
			(0..engine.track_count())
				.filter_map(|i| engine.track(i))
				.map(|t| t.clips().len())
				.sum()
		});
		settle_key(cx, window.into(), "secondary-k");
		let clips_after: usize = cx.read(|app| {
			let engine = root.read(app).engine.read(app);
			(0..engine.track_count())
				.filter_map(|i| engine.track(i))
				.map(|t| t.clips().len())
				.sum()
		});
		assert!(
			clips_after > clips_before,
			"split at the playhead adds a clip ({clips_before} → {clips_after})"
		);

		// ⌘Z / ⌘⇧Z reach the engine's undo/redo (the mock counts the
		// calls). settle_key flushes the deferred binding dispatch that
		// intermittently landed after the assert on Windows CI.
		settle_key(cx, window.into(), "secondary-z");
		settle_key(cx, window.into(), "secondary-shift-z");
		let (undo, redo) = cx.read(|app| root.read(app).engine.read(app).undo_redo_calls());
		assert!(
			undo >= 1 && redo >= 1,
			"⌘Z/⌘⇧Z dispatch undo/redo (undo {undo}, redo {redo})"
		);
	}

	/// While a modal dialog is open the shell's shortcuts are inert (the
	/// dialog's text fields must never trigger editing actions).
	#[gpui::test]
	async fn shortcuts_are_suppressed_while_a_modal_is_open(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());

		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::PREFERENCES, cx)));
		cx.run_until_parked();

		// ⌘K would split at the playhead, "s" would toggle snapping and
		// space would start playback without the modal guard.
		let clips_before: usize = cx.read(|app| {
			let engine = root.read(app).engine.read(app);
			(0..engine.track_count())
				.filter_map(|i| engine.track(i))
				.map(|t| t.clips().len())
				.sum()
		});
		let snapping_before = cx.read(|app| root.read(app).timeline.read(app).state.snap_enabled);
		cx.dispatch_keystroke(
			window.into(),
			gpui::Keystroke::parse("secondary-k").unwrap(),
		);
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("s").unwrap());
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("space").unwrap());
		cx.run_until_parked();

		let (clips_after, playing, snapping_after) = cx.read(|app| {
			let root = root.read(app);
			let clips: usize = (0..root.engine.read(app).track_count())
				.filter_map(|i| root.engine.read(app).track(i))
				.map(|t| t.clips().len())
				.sum();
			(
				clips,
				root.program_clock.read(app).is_playing(),
				root.timeline.read(app).state.snap_enabled,
			)
		});
		assert_eq!(
			clips_after, clips_before,
			"no split while the modal is open"
		);
		assert!(!playing, "no playback toggle while the modal is open");
		assert_eq!(
			snapping_after, snapping_before,
			"no snapping toggle while the modal is open"
		);
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::Preferences { .. })),
			"the modal is still open"
		);
	}

	// -------------------------------------------------------------------
	// Action search + custom shortcuts (stage 7)
	// -------------------------------------------------------------------

	/// The `/` key opens the action search dialog (the registry's ActionSearch
	/// default key), and Escape dismisses it.
	#[gpui::test]
	async fn action_search_opens_from_the_slash_key(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		// A leftover real shortcuts file must not shadow the default `/`
		// binding under test.
		crate::actions::reset_all_custom_shortcuts();
		cx.update(|app| root.update(app, |app, cx| app.rebind_keys(cx)));
		cx.run_until_parked();

		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("/").unwrap());
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::ActionSearch { .. })),
			"the / key should open the action search dialog"
		);

		// Escape closes it again (the modal's own handler).
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("escape").unwrap());
		cx.run_until_parked();
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::None)),
			"escape closes the action search dialog"
		);
	}

	/// The 窗口/Window → 多机位向导 menu item opens the wizard modal with
	/// the engine's wizard footage rows (the mock offers a demo list), and
	/// The multicam wizard action still opens the modal via the direct
	/// dispatch path (the 窗口 → 多机位向导 MENU ITEM was cut from v0.5 —
	/// the feature is out of the shipped surface, but the action code path
	/// keeps its guard).
	#[gpui::test]
	async fn multicam_wizard_menu_opens_the_modal(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);

		cx.update(|app| {
			root.update(app, |app, cx| {
				app.dispatch_action_id(
					crate::actions::ActionId::MulticamWizard,
					cx,
				)
			})
		});
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		let modal_rows = cx.read(|app| match &root.read(app).modal {
			ModalState::MulticamWizard { content, .. } => content.read(app).selection().len(),
			_ => panic!("the multicam wizard modal should be open"),
		});
		// The mock offers four demo footage rows.
		let footage = cx.read(|app| {
			let engine = root.read(app).engine.read(app);
			engine.multicam_wizard_footage().unwrap_or_default()
		});
		assert_eq!(footage.len(), 4, "the mock exposes the demo footage list");
		assert_eq!(modal_rows, 0, "no angles are pre-checked");
	}

	/// Arrow keys move the search selection and Enter runs the selected action
	/// through the same dispatch path the menu clicks take (here the dialog
	/// closes because the action dispatched successfully).
	#[gpui::test]
	async fn action_search_arrows_and_enter_dispatch_the_selection(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		crate::actions::reset_all_custom_shortcuts();
		cx.update(|app| root.update(app, |app, cx| app.rebind_keys(cx)));
		cx.run_until_parked();

		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("/").unwrap());
		cx.run_until_parked();
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::ActionSearch { .. })),
			"the search dialog is open"
		);

		// Down selects the first listed action, Enter runs it. The empty query
		// lists every menu-bar action, so the first one is New Project — the
		// v0.5 flow opens the new-project dialog (name + format), so the
		// search dialog hands off to it.
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("down").unwrap());
		cx.run_until_parked();
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("enter").unwrap());
		cx.run_until_parked();

		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::NewProject { .. })),
			"the search dialog hands off to the new-project dialog"
		);
	}

	/// The preferences dialog opens from its keyboard shortcut too (⌘,), and
	/// its Keyboard tab enumerates the menu-bar actions — the deferred-modal
	/// fix matters here: a shortcut dispatches inside a window update where
	/// `spawn_modal` would otherwise silently fail.
	#[gpui::test]
	async fn preferences_opens_from_its_shortcut_with_the_keyboard_tab(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		crate::actions::reset_all_custom_shortcuts();
		cx.update(|app| root.update(app, |app, cx| app.rebind_keys(cx)));
		cx.run_until_parked();

		cx.dispatch_keystroke(
			window.into(),
			gpui::Keystroke::parse("secondary-,").unwrap(),
		);
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::Preferences { .. })),
			"⌘, opens the preferences dialog"
		);

		// The tabbed content carries the Keyboard tab with a non-empty action
		// list (the general tab stays the default active tab).
		let rows = cx.read(|app| match &root.read(app).modal {
			ModalState::Preferences { content, .. } => {
				content.read(app).keyboard_tab_row_count(app)
			}
			_ => 0,
		});
		assert!(rows > 0, "the keyboard tab lists the menu-bar actions");
	}

	/// End to end through the Keyboard tab: switching to it, clicking the first
	/// action's capture field and pressing a key assigns the new binding (the
	/// override layer + interceptor + save path in one flow).
	#[gpui::test]
	async fn keyboard_tab_capture_assigns_a_shortcut(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		crate::actions::reset_all_custom_shortcuts();
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::PREFERENCES, cx)));
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		cx.run_until_parked();

		let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
		// Switch to the Keyboard tab.
		let tab = cx
			.debug_bounds("prefs-tab-keyboard")
			.expect("keyboard tab button rendered");
		cx.simulate_click(tab.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		// Enter capture on the first row (New Project).
		let field = cx
			.debug_bounds("keyboard-capture-0")
			.expect("first capture field rendered");
		cx.simulate_click(field.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		// Press a key → it becomes the new binding.
		cx.dispatch_keystroke(
			window.into(),
			gpui::Keystroke::parse("secondary-x").unwrap(),
		);
		cx.run_until_parked();
		drop(cx);

		let expected = gpui::Keystroke::parse("secondary-x").unwrap().unparse();
		assert_eq!(
			crate::actions::effective_keys(ActionId::NewProject.entry()),
			vec![expected],
			"capture assigns the pressed key to the row's action"
		);
		// The change is live: the shortcut display (used by the menus and the
		// row label) follows the override.
		assert_ne!(
			crate::actions::display_shortcut(ActionId::NewProject),
			Some("⌘N".to_string()),
			"the label no longer shows the default key"
		);
		assert!(
			crate::actions::display_shortcut(ActionId::NewProject).is_some(),
			"the assigned key still shows a label"
		);
	}

	/// Escape during a capture cancels the capture but keeps the dialog open
	/// (the interceptor stops the key before it can bubble to the modal's own
	/// Escape handler).
	#[gpui::test]
	async fn keyboard_tab_capture_escape_cancels_without_closing(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		crate::actions::reset_all_custom_shortcuts();
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::PREFERENCES, cx)));
		cx.run_until_parked();
		cx.update_window(window.into(), |_root, window, cx| {
			window.draw(cx).clear();
		})
		.expect("window is still open");
		cx.run_until_parked();

		let mut cx = VisualTestContext::from_window(window.into(), cx).into_mut();
		let tab = cx
			.debug_bounds("prefs-tab-keyboard")
			.expect("keyboard tab button rendered");
		cx.simulate_click(tab.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		let field = cx
			.debug_bounds("keyboard-capture-0")
			.expect("first capture field rendered");
		cx.simulate_click(field.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("escape").unwrap());
		cx.run_until_parked();

		let modal_still_open =
			cx.read(|app| matches!(root.read(app).modal, ModalState::Preferences { .. }));
		let override_keys = crate::actions::effective_keys(ActionId::NewProject.entry());
		drop(cx);
		assert!(
			modal_still_open,
			"escape cancels the capture, not the dialog"
		);
		assert_eq!(
			override_keys,
			vec!["secondary-n".to_string()],
			"no override was written by the cancelled capture"
		);
	}

	/// A shortcut override re-binds the global key map immediately: the new
	/// key drives the action, the displaced default key no longer does.
	#[gpui::test]
	async fn custom_shortcut_overrides_are_live_after_rebind(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		crate::actions::reset_all_custom_shortcuts();

		// Move Snapping from its default `s` to `f5` and apply the new map.
		crate::actions::set_custom_shortcut("snapping", vec!["f5".to_string()]);
		cx.update(|app| root.update(app, |app, cx| app.rebind_keys(cx)));
		cx.run_until_parked();

		let snap = |cx: &TestAppContext| {
			cx.read(|app| root.read(app).timeline.read(app).state.snap_enabled)
		};
		let before = snap(cx);
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("f5").unwrap());
		cx.run_until_parked();
		let after = snap(cx);
		assert_ne!(before, after, "f5 toggles snapping after the override");

		// The displaced default key is inert now.
		let steady = snap(cx);
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse("s").unwrap());
		cx.run_until_parked();
		assert_eq!(
			snap(cx),
			steady,
			"the displaced default s no longer toggles snapping"
		);
	}

	/// 文件 → 导入素材… opens the *platform* path picker (not the in-window
	/// file dialog) and routes the picked path to the engine's import; the
	/// mock engine records it, so the async round trip is observable.
	#[gpui::test]
	async fn import_footage_prompts_and_routes_the_picked_path(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(1600.0), px(900.0)), |window, cx| {
			OakApp::<MockEngine>::new(window, None, cx)
		});
		cx.run_until_parked();
		let root = window.root(cx).expect("app root");

		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::IMPORT_FOOTAGE, cx)));
		cx.run_until_parked();
		assert!(
			cx.did_prompt_for_paths(),
			"the platform path picker should be shown"
		);

		// Answer the picker with one file: the async continuation must land it
		// in the engine's import.
		let picked = PathBuf::from("/media/raw/interview.mov");
		cx.simulate_path_prompt_response({
			let picked = picked.clone();
			move |options| {
				assert!(options.multiple, "import allows multiple selection");
				Some(vec![picked])
			}
		});
		cx.run_until_parked();

		let imported = cx.read(|app| root.read(app).engine.read(app).imported_footage().to_vec());
		assert_eq!(imported, vec![picked]);

		// Cancelling the picker imports nothing.
		cx.update(|app| root.update(app, |app, cx| app.on_menu(menu_ids::IMPORT_FOOTAGE, cx)));
		cx.run_until_parked();
		cx.simulate_path_prompt_response(|_options| None);
		cx.run_until_parked();
		let imported = cx.read(|app| root.read(app).engine.read(app).imported_footage().len());
		assert_eq!(imported, 1, "a cancelled picker imports nothing");
	}

	/// Dragging files onto the project explorer routes them to the engine's
	/// import: the explorer emits `FileDropRequested` on a real drop and the
	/// panel's subscription calls `AppEngine::import_footage` per path (the
	/// mock engine records them). The drop is simulated as the platform
	/// delivers it — the OS drag entering the window, then the release.
	#[gpui::test]
	async fn dropping_files_onto_the_project_browser_imports_them(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		let mut cx = VisualTestContext::from_window(window.into(), cx);

		// The mock shell's parked frame is live: dispatch the drop against it
		// (an extra draw would repaint the cached element tree and consume the
		// one-shot interactive listeners).
		let bounds = cx
			.debug_bounds("gpui-widgets-explorer-entry-1")
			.expect("the explorer's first row is rendered");
		let dropped = PathBuf::from("/media/raw/drag-drop.mov");
		let paths = ExternalPaths(std::iter::once(dropped.clone()).collect());
		// Deliver the whole drag sequence against the same rendered frame:
		// gpui's interactive listeners are registered per render, so a repaint
		// between the events would consume them before the drop lands.
		cx.update(|window, cx| {
			window.dispatch_event(
				gpui::PlatformInput::FileDrop(FileDropEvent::Entered {
					position: bounds.center(),
					paths,
				}),
				cx,
			);
			window.dispatch_event(
				gpui::PlatformInput::FileDrop(FileDropEvent::Pending {
					position: bounds.center(),
				}),
				cx,
			);
			window.dispatch_event(
				gpui::PlatformInput::FileDrop(FileDropEvent::Submit {
					position: bounds.center(),
				}),
				cx,
			);
		});
		cx.run_until_parked();

		let imported = cx.read(|app| root.read(app).engine.read(app).imported_footage().to_vec());
		assert_eq!(imported, vec![dropped]);
	}

	/// Dragging a media entry from the project explorer onto the timeline
	/// places a clip there: the row starts a [`FootageDrag`] carrying the
	/// entry id, the timeline panel resolves the cursor to a display track +
	/// frame, and the engine records the `drop_footage` request with the
	/// correct parameters.
	#[gpui::test]
	async fn dragging_footage_onto_the_timeline_places_a_clip(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, root) = mock_shell(cx);
		let mut cx = VisualTestContext::from_window(window.into(), cx);

		// The mock project's root-level footage entry ("第一稿.mp4", id 3) is
		// the drag source; the timeline's clip area is the drop target.
		let row = cx
			.debug_bounds("gpui-widgets-explorer-entry-3")
			.expect("root-level footage row rendered");
		let canvas = cx
			.debug_bounds("timeline-canvas")
			.expect("timeline body rendered");
		// A point inside the clip area: 100 px into the ruler's content
		// (frame 50 at the shell's default zoom of 2 px/frame) on the first
		// track row (V2, 64 px tall).
		let drop = gpui::point(
			canvas.left() + px(gpui::timeline::HEADER_WIDTH + 100.0),
			canvas.top() + px(gpui::timeline::RULER_HEIGHT + 20.0),
		);

		// Dispatch the whole gesture against the same rendered frame: gpui's
		// interactive listeners are registered per render, so a repaint
		// between the events would consume them before the drop lands (same
		// caveat as the file-drop test above).
		cx.update(|window, cx| {
			window.dispatch_event(
				gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
					position: row.center(),
					modifiers: gpui::Modifiers::none(),
					button: gpui::MouseButton::Left,
					click_count: 1,
					first_mouse: false,
				}),
				cx,
			);
			// Move past the drag threshold to start the drag.
			window.dispatch_event(
				gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
					position: row.center() + gpui::point(px(6.0), px(0.0)),
					modifiers: gpui::Modifiers::none(),
					pressed_button: Some(gpui::MouseButton::Left),
				}),
				cx,
			);
			// Hover the drop point (the panel resolves the track + frame).
			window.dispatch_event(
				gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
					position: drop,
					modifiers: gpui::Modifiers::none(),
					pressed_button: Some(gpui::MouseButton::Left),
				}),
				cx,
			);
			// Release over the drop point.
			window.dispatch_event(
				gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
					position: drop,
					modifiers: gpui::Modifiers::none(),
					button: gpui::MouseButton::Left,
					click_count: 1,
				}),
				cx,
			);
		});
		cx.run_until_parked();

		let drops = cx.read(|app| root.read(app).engine.read(app).footage_drops().to_vec());
		assert_eq!(
			drops.len(),
			1,
			"the drop must reach the engine exactly once"
		);
		assert_eq!(drops[0].id, 3);
		assert_eq!(drops[0].track_kind, gpui::timeline::TrackKind::Video);
		assert_eq!(drops[0].track_index, 0);
		assert_eq!(drops[0].time, gpui::timeline::Frame(50));
	}

	/// The project explorer lists root-level imported footage in BOTH views:
	/// the tree shows the entry as a row, and after switching to the icon
	/// grid the same entry (plus folder children) appears as icons.
	#[gpui::test]
	async fn explorer_views_list_root_level_footage(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (window, _root) = mock_shell(cx);
		let mut cx = VisualTestContext::from_window(window.into(), cx);

		// Tree view: the root-level footage entry (id 3 "第一稿.mp4") renders
		// as a row.
		assert!(
			cx.debug_bounds("gpui-widgets-explorer-entry-3").is_some(),
			"tree view shows the root-level footage row"
		);

		// Switch to the icon grid; root-level footage and folder children
		// both appear as icons, the folder root itself does not.
		let toggle = cx
			.debug_bounds("gpui-widgets-explorer-icons")
			.expect("icons toggle rendered");
		cx.simulate_click(toggle.center(), gpui::Modifiers::none());
		cx.run_until_parked();
		cx.update(|window, cx| {
			window.draw(cx).clear();
		});

		assert!(
			cx.debug_bounds("gpui-widgets-explorer-icon-3").is_some(),
			"root-level footage shows as an icon"
		);
		assert!(
			cx.debug_bounds("gpui-widgets-explorer-icon-10").is_some(),
			"a folder child shows as an icon"
		);
		assert!(
			cx.debug_bounds("gpui-widgets-explorer-icon-1").is_none(),
			"the footage folder root itself has no icon"
		);
	}

	/// The command-line parser understands the project path and the mock
	/// flag, and the `OAK_ENGINE` env var forces the mock.
	#[test]
	fn app_args_parse_path_and_mock_flag() {
		// Drive the real parser with synthetic argv (the env override is
		// a parameter, no process-global env is touched).
		let parse = |argv: &[&str], env: Option<&str>| {
			AppArgs::parse_args(argv.iter().map(std::ffi::OsString::from), env)
		};
		let a = parse(&["/tmp/a.ove"], None);
		assert_eq!(a.project, Some(PathBuf::from("/tmp/a.ove")));
		assert!(!a.mock);

		let b = parse(&["/tmp/a.ove", "--mock"], None);
		assert!(b.mock);

		let c = parse(&["/tmp/a.ove"], Some("MOCK"));
		assert!(c.mock);

		let d = parse(&[], None);
		assert!(d.project.is_none());
		assert!(!d.mock);

		// The first positional wins; later positionals are ignored.
		let e = parse(&["/tmp/a.ove", "/tmp/b.ove"], None);
		assert_eq!(e.project, Some(PathBuf::from("/tmp/a.ove")));
	}

	// -------------------------------------------------------------------
	// Project manager (M13 D4)
	// -------------------------------------------------------------------

	/// 文件 → 导出序列 opens the dialog with the project's sequences (the
	/// current one preselected); OK exports the picked sequence and swaps
	/// in the progress dialog.
	#[gpui::test]
	async fn export_sequence_dialog_picks_and_starts(cx: &mut TestAppContext) {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);

		cx.update(|app| {
			root.update(app, |app, cx| app.on_menu(ActionId::Export.menu_id(), cx))
		});
		cx.run_until_parked();

		let content = cx.read(|app| match &root.read(app).modal {
			ModalState::Export { content, .. } => content.clone(),
			_ => panic!("the export dialog should be open"),
		});
		// The mock bin's one sequence (entry 4) is preselected; the
		// default output path is a real file name.
		assert_eq!(
			cx.read(|app| content.read(app).selected_sequence(app)),
			Some(4),
			"the current sequence is preselected"
		);
		let path = cx.read(|app| content.read(app).path(app).to_string());
		assert!(path.ends_with(".mp4"), "default export path: {path}");

		// OK starts the export of the picked sequence: the progress dialog
		// replaces the settings dialog.
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_modal(
					&ModalEvent::ButtonClicked {
						control: modal_ids::EXPORT,
						button: 0,
					},
					cx,
				)
			})
		});
		cx.run_until_parked();
		assert!(
			cx.read(|app| matches!(root.read(app).modal, ModalState::Progress { .. })),
			"OK starts the export and opens the progress dialog"
		);
	}

	/// Dispatch a synthetic keystroke and settle with a double park.
	/// gpui's window key handling may defer the binding dispatch (its
	/// pending/repeat machinery); parking twice drains the executor
	/// across two idle cycles so a deferred delivery lands before the
	/// assertion. Deliberately does NOT advance the simulated clock —
	/// the mock engine's playback ticks with executor time, so a clock
	/// advance would move the playhead out from under the assertions.
	fn settle_key(cx: &mut TestAppContext, window: AnyWindowHandle, key: &str) {
		cx.dispatch_keystroke(window.into(), gpui::Keystroke::parse(key).unwrap());
		cx.run_until_parked();
		cx.run_until_parked();
	}

	/// A running app shell on the mock engine (en-US), plus its root. The
	/// caller holds the language lock (the tests flip the process-global
	/// language).
	fn mock_shell(
		cx: &mut TestAppContext,
	) -> (
		gpui::WindowHandle<OakApp<MockEngine>>,
		Entity<OakApp<MockEngine>>,
	) {
		crate::i18n::set_language_code("en-US");
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(1600.0), px(900.0)), |window, cx| {
			OakApp::<MockEngine>::new(window, None, cx)
		});
		cx.run_until_parked();
		let root = window.root(cx).expect("app root");
		(window, root)
	}

	/// Opens the manager and returns its content entity.
	fn open_manager(
		cx: &mut TestAppContext,
		root: &Entity<OakApp<MockEngine>>,
	) -> Entity<crate::manager::ProjectManager<MockEngine>> {
		cx.update(|app| root.update(app, |app, cx| app.show_project_manager(cx)));
		cx.run_until_parked();
		let content = cx.read(|app| match &root.read(app).modal {
			ModalState::Manager { content, .. } => content.clone(),
			_ => panic!("the manager modal should be open"),
		});
		content
	}

	/// The manager's listed rows.
	fn manager_rows(
		cx: &mut TestAppContext,
		content: &Entity<crate::manager::ProjectManager<MockEngine>>,
	) -> Vec<crate::oakui::LibraryProject> {
		cx.read(|app| content.read(app).rows().to_vec())
	}

	/// The manager lists the mock library; opening a row (the double-click
	/// / 打开 route) drives the engine's library open and closes the dialog.
	#[gpui::test]
	async fn manager_lists_and_opens(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);
		let content = open_manager(cx, &root);
		let rows = manager_rows(cx, &content);
		assert_eq!(rows.len(), 3, "the mock library seeds three rows");
		assert_eq!(rows[0].name, "第一稿", "most recently modified first");
		assert!(rows[0].track_count > 0 && rows[0].footage_count > 0);

		// Select the second row and open it.
		let uuid = rows[1].uuid.clone();
		cx.update(|app| content.update(app, |m, cx| m.select(&uuid, cx)));
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(&crate::manager::ManagerEvent::Open(uuid.clone()), cx)
			})
		});
		cx.run_until_parked();

		let opened = cx.read(|app| root.read(app).engine.read(app).library_opened().to_vec());
		assert_eq!(opened, vec![uuid], "the engine opened the selected row");
		let name = cx.read(|app| {
			root.read(app)
				.engine
				.read(app)
				.project()
				.unwrap()
				.name
				.clone()
		});
		assert_eq!(name, "宣传片 v3");
		let modal_none = cx.read(|app| matches!(root.read(app).modal, ModalState::None));
		assert!(modal_none, "a successful open closes the manager");
	}

	/// Create / rename / duplicate / delete round-trip through the manager
	/// and its sub-dialogs.
	#[gpui::test]
	async fn manager_create_rename_duplicate_delete(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);
		let content = open_manager(cx, &root);

		// Create: a new row appears and the project opens (dialog closes).
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(&crate::manager::ManagerEvent::Create, cx)
			})
		});
		cx.run_until_parked();
		let rows = cx.read(|app| root.read(app).engine.read(app).library_projects().unwrap());
		assert_eq!(rows.len(), 4);
		let created = rows
			.iter()
			.find(|row| row.name == "Untitled Project")
			.expect("the created row")
			.clone();
		let project_name = cx.read(|app| {
			root.read(app)
				.engine
				.read(app)
				.project()
				.unwrap()
				.name
				.clone()
		});
		assert_eq!(
			project_name, "Untitled Project",
			"create opens the new project"
		);

		// Reopen the manager, select the created row, rename it.
		let content = open_manager(cx, &root);
		cx.update(|app| content.update(app, |m, cx| m.select(&created.uuid, cx)));
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(
					&crate::manager::ManagerEvent::Rename(created.uuid.clone()),
					cx,
				)
			})
		});
		cx.run_until_parked();
		let prompt = cx.read(|app| match &root.read(app).modal {
			ModalState::ManagerRename { content, uuid, .. } => {
				assert_eq!(uuid, &created.uuid);
				content.clone()
			}
			_ => panic!("the rename prompt should be open"),
		});
		cx.update(|app| prompt.update(app, |p, cx| p.set_value("改名为正稿", cx)));
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_modal(
					&ModalEvent::ButtonClicked {
						control: modal_ids::MANAGER_RENAME,
						button: 0,
					},
					cx,
				)
			})
		});
		cx.run_until_parked();
		// The confirmation swaps in a FRESH manager (the captured content is
		// stale from here on); assert against the engine's library instead.
		let rows = cx.read(|app| root.read(app).engine.read(app).library_projects().unwrap());
		let renamed = rows.iter().find(|row| row.uuid == created.uuid).unwrap();
		assert_eq!(renamed.name, "改名为正稿");
		let back = cx.read(|app| matches!(root.read(app).modal, ModalState::Manager { .. }));
		assert!(back, "a confirmed rename returns to the manager");

		// Duplicate the renamed row.
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(
					&crate::manager::ManagerEvent::Duplicate(created.uuid.clone()),
					cx,
				)
			})
		});
		cx.run_until_parked();
		let rows = cx.read(|app| root.read(app).engine.read(app).library_projects().unwrap());
		assert_eq!(rows.len(), 5);
		assert!(
			rows.iter().any(|row| row.name == "改名为正稿 (copy)"),
			"the copy is named '<name> (copy)': {rows:?}"
		);

		// Delete the original through the confirmation dialog.
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(
					&crate::manager::ManagerEvent::Delete(created.uuid.clone()),
					cx,
				)
			})
		});
		cx.run_until_parked();
		let confirming =
			cx.read(|app| matches!(root.read(app).modal, ModalState::ManagerDelete { .. }));
		assert!(confirming, "the delete confirmation should be open");
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_modal(
					&ModalEvent::ButtonClicked {
						control: modal_ids::MANAGER_DELETE,
						button: 0,
					},
					cx,
				)
			})
		});
		cx.run_until_parked();
		let rows = cx.read(|app| root.read(app).engine.read(app).library_projects().unwrap());
		assert_eq!(rows.len(), 4);
		assert!(!rows.iter().any(|row| row.uuid == created.uuid));
		let back = cx.read(|app| matches!(root.read(app).modal, ModalState::Manager { .. }));
		assert!(back, "a confirmed delete returns to the manager");
	}

	/// The manager's 导入 opens the platform path picker and lands the file
	/// as a new row; 导出 asks for a new path and routes it to the engine.
	#[gpui::test]
	async fn manager_import_and_export_route_through_the_platform_dialogs(cx: &mut TestAppContext) {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let (_window, root) = mock_shell(cx);
		let content = open_manager(cx, &root);

		// Import.
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(&crate::manager::ManagerEvent::Import, cx)
			})
		});
		cx.run_until_parked();
		assert!(cx.did_prompt_for_paths(), "import shows the path picker");
		cx.simulate_path_prompt_response(|options| {
			assert!(!options.multiple, "project import is single-file");
			Some(vec![PathBuf::from("/library/先导片.ove")])
		});
		cx.run_until_parked();
		let rows = manager_rows(cx, &content);
		assert_eq!(rows.len(), 4);
		assert!(
			rows.iter().any(|row| row.name == "先导片"),
			"the imported file becomes a row named by its stem: {rows:?}"
		);

		// Export the imported row.
		let uuid = rows
			.iter()
			.find(|row| row.name == "先导片")
			.unwrap()
			.uuid
			.clone();
		cx.update(|app| content.update(app, |m, cx| m.select(&uuid, cx)));
		cx.update(|app| {
			root.update(app, |app, cx| {
				app.on_manager_event(&crate::manager::ManagerEvent::Export(uuid.clone()), cx)
			})
		});
		cx.run_until_parked();
		assert!(cx.did_prompt_for_new_path(), "export shows the save dialog");
		cx.simulate_new_path_selection(|_dir| Some(PathBuf::from("/library/先导片.otio")));
		cx.run_until_parked();
		let exported = cx.read(|app| root.read(app).engine.read(app).library_exported().to_vec());
		assert_eq!(
			exported,
			vec![(uuid, PathBuf::from("/library/先导片.otio"))],
			"the picked path routes to the engine's library export"
		);
	}
}
