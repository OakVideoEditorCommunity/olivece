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

//! The action registry: the single data source behind the menu bar, the
//! keyboard shortcuts and (later) the shortcut preferences page and the
//! action search dialog — the Rust counterpart of the C++ `MainMenu` +
//! `MenuShared` action system (`app/window/mainwindow/mainmenu.cpp`,
//! `app/widget/menu/menushared.cpp`).
//!
//! Every entry pairs a gpui action (defined through the [`gpui::actions`]
//! macro) with the C++ action's stable id string (`newproj`, `rippledelete`,
//! `snapping`, … — kept for future shortcut-file compatibility), its i18n
//! key, its default key(s) in gpui keystroke syntax (`secondary-` is the
//! platform command key) and a routing target:
//!
//! * [`Route::Global`] — the app shell handles it (file dialogs, undo,
//!   preferences, tool selection, …);
//! * [`Route::FocusedPanel`] — the command goes to the currently focused
//!   panel through [`crate::panels::commands::PanelCommandHandler`] first,
//!   falling back to the shell's global handler when the panel does not
//!   implement it (the C++ `PanelManager::currently_focused()` pattern).
//!
//! Menu clicks and key presses dispatch through the same path: the menu bar
//! reports the item id, the keymap dispatches the gpui action, and both end
//! up in `OakApp::dispatch_action_id`.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use gpui::{Action, KeyBinding, KeyBindingContextPredicate};

// One macro call generates all three views of the registry, so they can
// never drift apart: the gpui action structs, the `ActionId` enum and the
// `REGISTRY` table. Menu ids reuse the pre-action-system values for the
// actions that existed before (the tests' `menu_ids` constants).
macro_rules! define_actions {
	($(
		$name:ident {
			cpp: $cpp:literal,
			i18n: $i18n:literal,
			keys: [$($key:literal),*],
			route: $route:ident,
			menu_id: $menu_id:expr
		}
	);* ;) => {
		gpui::actions!(oak, [ $($name),* ]);

		/// The stable identity of every app action (registry index).
		#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
		pub enum ActionId {
			$($name),*
		}

		impl ActionId {
			/// The menu item id this action dispatches from (the menu bar's
			/// numeric item ids; unique across the whole menu tree).
			pub const fn menu_id(self) -> usize {
				match self {
					$( ActionId::$name => $menu_id ),*
				}
			}

			/// This action's registry entry.
			pub fn entry(self) -> &'static ActionEntry {
				REGISTRY
					.iter()
					.find(|entry| entry.action == self)
					.expect("every ActionId is in the registry")
			}
		}

		/// The action registry, in menu order.
		pub const REGISTRY: &[ActionEntry] = &[
			$( ActionEntry {
				action: ActionId::$name,
				cpp_id: $cpp,
				i18n_key: $i18n,
				default_keys: &[$($key),*],
				route: Route::$route,
				build: || Box::new($name),
			} ),*
		];
	};
}

define_actions! {
	// --- File --------------------------------------------------------------
	NewProject { cpp: "newproj", i18n: "menu.file.new_project", keys: ["secondary-n"], route: Global, menu_id: 101 };
	NewSequence { cpp: "newseq", i18n: "menu.file.new_sequence", keys: ["secondary-shift-n"], route: Global, menu_id: 1001 };
	NewFolder { cpp: "newfolder", i18n: "menu.file.new_folder", keys: [], route: Global, menu_id: 1002 };
	OpenProject { cpp: "openproj", i18n: "menu.file.open_project", keys: ["secondary-o"], route: Global, menu_id: 102 };
	OpenFromLibrary { cpp: "openlibrary", i18n: "menu.file.open_library", keys: [], route: Global, menu_id: 110 };
	ClearOpenRecent { cpp: "clearopenrecent", i18n: "menu.file.clear_recent", keys: [], route: Global, menu_id: 1003 };
	SaveProject { cpp: "saveproj", i18n: "menu.file.export_project", keys: ["secondary-s"], route: Global, menu_id: 103 };
	SaveProjectAs { cpp: "saveprojas", i18n: "menu.file.save_as", keys: ["secondary-shift-s"], route: Global, menu_id: 1004 };
	Revert { cpp: "revert", i18n: "menu.file.revert", keys: ["f12"], route: Global, menu_id: 1005 };
	Import { cpp: "import", i18n: "menu.file.import_footage", keys: ["secondary-i"], route: Global, menu_id: 108 };
	Export { cpp: "export", i18n: "menu.file.export_media", keys: ["secondary-m"], route: Global, menu_id: 106 };
	ProjectProperties { cpp: "projectproperties", i18n: "menu.file.project_properties", keys: ["shift-f10"], route: Global, menu_id: 1006 };
	CloseProject { cpp: "closeproj", i18n: "menu.file.close", keys: [], route: Global, menu_id: 105 };
	ProjectManager { cpp: "projectmanager", i18n: "menu.file.project_manager", keys: [], route: Global, menu_id: 109 };
	Exit { cpp: "exit", i18n: "menu.file.quit", keys: ["secondary-q"], route: Global, menu_id: 107 };

	// --- Edit ---------------------------------------------------------------
	Undo { cpp: "undo", i18n: "menu.edit.undo", keys: ["secondary-z"], route: Global, menu_id: 201 };
	Redo { cpp: "redo", i18n: "menu.edit.redo", keys: ["secondary-shift-z"], route: Global, menu_id: 202 };
	Cut { cpp: "cut", i18n: "menu.edit.cut", keys: ["secondary-x"], route: FocusedPanel, menu_id: 203 };
	Copy { cpp: "copy", i18n: "menu.edit.copy", keys: ["secondary-c"], route: FocusedPanel, menu_id: 204 };
	Paste { cpp: "paste", i18n: "menu.edit.paste", keys: ["secondary-v"], route: FocusedPanel, menu_id: 205 };
	PasteInsert { cpp: "pasteinsert", i18n: "menu.edit.paste_insert", keys: ["secondary-shift-v"], route: FocusedPanel, menu_id: 1010 };
	Duplicate { cpp: "duplicate", i18n: "menu.edit.duplicate", keys: ["secondary-d"], route: FocusedPanel, menu_id: 1011 };
	Rename { cpp: "rename", i18n: "menu.edit.rename", keys: ["f2"], route: FocusedPanel, menu_id: 1012 };
	Delete { cpp: "delete", i18n: "menu.edit.delete", keys: ["delete", "backspace"], route: FocusedPanel, menu_id: 206 };
	RippleDelete { cpp: "rippledelete", i18n: "menu.edit.ripple_delete", keys: ["shift-delete", "shift-backspace"], route: FocusedPanel, menu_id: 207 };
	SplitAtPlayhead { cpp: "split", i18n: "menu.edit.split", keys: ["secondary-k"], route: FocusedPanel, menu_id: 504 };
	SpeedDuration { cpp: "speeddur", i18n: "menu.edit.speed_duration", keys: ["secondary-r"], route: FocusedPanel, menu_id: 1014 };
	DefaultTransition { cpp: "deftransition", i18n: "menu.edit.default_transition", keys: ["secondary-shift-d"], route: FocusedPanel, menu_id: 1015 };
	LinkUnlink { cpp: "linkunlink", i18n: "menu.edit.link_unlink", keys: ["secondary-l"], route: FocusedPanel, menu_id: 1016 };
	EnableDisable { cpp: "enabledisable", i18n: "menu.edit.enable_disable", keys: ["shift-e"], route: FocusedPanel, menu_id: 1017 };
	Nest { cpp: "nest", i18n: "menu.edit.nest", keys: [], route: FocusedPanel, menu_id: 1018 };
	SelectAll { cpp: "selectall", i18n: "menu.edit.select_all", keys: ["secondary-a"], route: FocusedPanel, menu_id: 208 };
	DeselectAll { cpp: "deselectall", i18n: "menu.edit.deselect_all", keys: ["secondary-shift-a"], route: FocusedPanel, menu_id: 1019 };
	Insert { cpp: "insert", i18n: "menu.edit.insert", keys: [","], route: FocusedPanel, menu_id: 1020 };
	Overwrite { cpp: "overwrite", i18n: "menu.edit.overwrite", keys: ["."], route: FocusedPanel, menu_id: 1021 };
	RippleToIn { cpp: "rippletoin", i18n: "menu.edit.ripple_to_in", keys: ["q"], route: FocusedPanel, menu_id: 1022 };
	RippleToOut { cpp: "rippletoout", i18n: "menu.edit.ripple_to_out", keys: ["w"], route: FocusedPanel, menu_id: 1023 };
	EditToIn { cpp: "edittoin", i18n: "menu.edit.edit_to_in", keys: ["secondary-alt-q"], route: FocusedPanel, menu_id: 1024 };
	EditToOut { cpp: "edittoout", i18n: "menu.edit.edit_to_out", keys: ["secondary-alt-w"], route: FocusedPanel, menu_id: 1025 };
	NudgeLeft { cpp: "nudgeleft", i18n: "menu.edit.nudge_left", keys: ["alt-left"], route: FocusedPanel, menu_id: 1026 };
	NudgeRight { cpp: "nudgeright", i18n: "menu.edit.nudge_right", keys: ["alt-right"], route: FocusedPanel, menu_id: 1027 };
	MoveInToPlayhead { cpp: "moveintoplayhead", i18n: "menu.edit.move_in_to_playhead", keys: ["["], route: FocusedPanel, menu_id: 1028 };
	MoveOutToPlayhead { cpp: "moveouttoplayhead", i18n: "menu.edit.move_out_to_playhead", keys: ["]"], route: FocusedPanel, menu_id: 1029 };
	SetInPoint { cpp: "setinpoint", i18n: "menu.edit.set_in_point", keys: ["i"], route: FocusedPanel, menu_id: 407 };
	SetOutPoint { cpp: "setoutpoint", i18n: "menu.edit.set_out_point", keys: ["o"], route: FocusedPanel, menu_id: 408 };
	ResetIn { cpp: "resetin", i18n: "menu.edit.reset_in", keys: [], route: FocusedPanel, menu_id: 1030 };
	ResetOut { cpp: "resetout", i18n: "menu.edit.reset_out", keys: [], route: FocusedPanel, menu_id: 1031 };
	ClearInOut { cpp: "clearinout", i18n: "menu.edit.clear_in_out", keys: ["g"], route: FocusedPanel, menu_id: 1032 };
	DeleteInOut { cpp: "deleteinout", i18n: "menu.edit.delete_in_out", keys: [";"], route: FocusedPanel, menu_id: 1033 };
	RippleDeleteInOut { cpp: "rippledeleteinout", i18n: "menu.edit.ripple_delete_in_out", keys: ["'"], route: FocusedPanel, menu_id: 1034 };
	Marker { cpp: "marker", i18n: "menu.edit.marker", keys: ["m"], route: FocusedPanel, menu_id: 505 };

	// --- View ---------------------------------------------------------------
	ZoomIn { cpp: "zoomin", i18n: "menu.view.zoom_in", keys: ["=", "shift-="], route: FocusedPanel, menu_id: 306 };
	ZoomOut { cpp: "zoomout", i18n: "menu.view.zoom_out", keys: ["-"], route: FocusedPanel, menu_id: 307 };
	IncreaseTrackHeight { cpp: "vzoomin", i18n: "menu.view.increase_track_height", keys: ["secondary-="], route: FocusedPanel, menu_id: 1040 };
	DecreaseTrackHeight { cpp: "vzoomout", i18n: "menu.view.decrease_track_height", keys: ["secondary--"], route: FocusedPanel, menu_id: 1041 };
	ToggleShowAll { cpp: "showall", i18n: "menu.view.show_all", keys: ["\\"], route: FocusedPanel, menu_id: 1042 };
	FullScreen { cpp: "fullscreen", i18n: "menu.view.full_screen", keys: ["f11"], route: Global, menu_id: 1043 };
	FullScreenViewer { cpp: "fullscreenviewer", i18n: "menu.view.full_screen_viewer", keys: [], route: FocusedPanel, menu_id: 1044 };
	ThemeDark { cpp: "themedark", i18n: "menu.view.theme.dark", keys: [], route: Global, menu_id: 301 };
	ThemeLight { cpp: "themelight", i18n: "menu.view.theme.light", keys: [], route: Global, menu_id: 302 };
	// Language switching is deliberately NOT a registry action: the
	// language menu is built from the discovered packs (menu ids in the
	// LANG_ITEM_BASE range, see menus::shared), so a new language is a new
	// YAML file, not a code change. The C++ `langzh`/`langen` action slots
	// (menu ids 303/304) stay reserved.

	// --- Playback -----------------------------------------------------------
	GoToStart { cpp: "gotostart", i18n: "menu.playback.to_start", keys: ["home"], route: FocusedPanel, menu_id: 404 };
	PrevFrame { cpp: "prevframe", i18n: "menu.playback.prev_frame", keys: ["left"], route: FocusedPanel, menu_id: 402 };
	PlayPause { cpp: "playpause", i18n: "menu.playback.play_pause", keys: ["space"], route: FocusedPanel, menu_id: 401 };
	PlayInToOut { cpp: "playintoout", i18n: "menu.playback.play_in_to_out", keys: ["shift-space"], route: FocusedPanel, menu_id: 1050 };
	NextFrame { cpp: "nextframe", i18n: "menu.playback.next_frame", keys: ["right"], route: FocusedPanel, menu_id: 403 };
	GoToEnd { cpp: "gotoend", i18n: "menu.playback.go_to_end", keys: ["end"], route: FocusedPanel, menu_id: 1051 };
	GoToPrevCut { cpp: "prevcut", i18n: "menu.playback.prev_cut", keys: ["up"], route: FocusedPanel, menu_id: 1052 };
	GoToNextCut { cpp: "nextcut", i18n: "menu.playback.next_cut", keys: ["down"], route: FocusedPanel, menu_id: 1053 };
	GoToIn { cpp: "gotoin", i18n: "menu.playback.go_to_in", keys: ["shift-i"], route: FocusedPanel, menu_id: 1054 };
	GoToOut { cpp: "gotoout", i18n: "menu.playback.go_to_out", keys: ["shift-o"], route: FocusedPanel, menu_id: 1055 };
	ShuttleLeft { cpp: "decspeed", i18n: "menu.playback.shuttle_left", keys: ["j"], route: FocusedPanel, menu_id: 1056 };
	ShuttleStop { cpp: "pause", i18n: "menu.playback.shuttle_stop", keys: ["k"], route: FocusedPanel, menu_id: 406 };
	ShuttleRight { cpp: "incspeed", i18n: "menu.playback.shuttle_right", keys: ["l"], route: FocusedPanel, menu_id: 405 };
	Loop { cpp: "loop", i18n: "menu.playback.loop", keys: [], route: Global, menu_id: 1057 };

	// --- Sequence -----------------------------------------------------------
	AddVideoTrack { cpp: "addvideotrack", i18n: "menu.sequence.add_video_track", keys: [], route: Global, menu_id: 501 };
	AddAudioTrack { cpp: "addaudiotrack", i18n: "menu.sequence.add_audio_track", keys: [], route: Global, menu_id: 502 };
	RemoveTrack { cpp: "removetrack", i18n: "menu.sequence.remove_track", keys: [], route: Global, menu_id: 503 };
	SetWorkArea { cpp: "setworkarea", i18n: "menu.sequence.set_workarea", keys: [], route: Global, menu_id: 507 };
	ClearWorkArea { cpp: "clearworkarea", i18n: "menu.sequence.clear_workarea", keys: [], route: Global, menu_id: 508 };
	RemoveMarker { cpp: "removemarker", i18n: "menu.sequence.remove_marker", keys: [], route: Global, menu_id: 506 };
	SeqCache { cpp: "seqcache", i18n: "menu.sequence.cache", keys: [], route: Global, menu_id: 1060 };
	SeqCacheInOut { cpp: "seqcacheinout", i18n: "menu.sequence.cache_in_out", keys: [], route: Global, menu_id: 1061 };
	SeqCacheClear { cpp: "seqcacheclear", i18n: "menu.sequence.cache_clear", keys: [], route: Global, menu_id: 1062 };
	SequenceSettings { cpp: "seqsettings", i18n: "menu.sequence.settings", keys: [], route: Global, menu_id: 704 };

	// --- Window -------------------------------------------------------------
	FocusProject { cpp: "focusproject", i18n: "menu.window.project", keys: [], route: Global, menu_id: 601 };
	FocusSourceViewer { cpp: "focussourceviewer", i18n: "menu.window.source_viewer", keys: [], route: Global, menu_id: 602 };
	FocusProgramViewer { cpp: "focusprogramviewer", i18n: "menu.window.program_viewer", keys: [], route: Global, menu_id: 603 };
	FocusNodeEditor { cpp: "focusnodeeditor", i18n: "menu.window.node_editor", keys: [], route: Global, menu_id: 604 };
	FocusInspector { cpp: "focusinspector", i18n: "menu.window.inspector", keys: [], route: Global, menu_id: 605 };
	FocusHistory { cpp: "focushistory", i18n: "menu.window.history", keys: [], route: Global, menu_id: 606 };
	FocusTimeline { cpp: "focustimeline", i18n: "menu.window.timeline", keys: [], route: Global, menu_id: 607 };
	FocusEffectLibrary { cpp: "focuseffectlibrary", i18n: "menu.window.effect_library", keys: [], route: Global, menu_id: 608 };
	FocusMulticam { cpp: "focusmulticam", i18n: "menu.window.multicam", keys: [], route: Global, menu_id: 609 };
	// The multicam wizard was cut from the v0.5 surface: the menu item and
	// the action-search entry are gone (menu_id 0 = hidden, like the
	// panel-context hotkeys), the engine pathway stays for a future
	// release.
	MulticamWizard { cpp: "multicamwizard", i18n: "menu.sequence.multicam_wizard", keys: [], route: Global, menu_id: 0 };
	MaximizePanel { cpp: "maximizepanel", i18n: "menu.window.maximize_panel", keys: ["`"], route: Global, menu_id: 1070 };
	ResetDefaultLayout { cpp: "resetdefaultlayout", i18n: "menu.window.reset_layout", keys: [], route: Global, menu_id: 1071 };

	// --- Tools (the mutually exclusive tool group + snapping + proxy) -------
	PointerTool { cpp: "pointertool", i18n: "menu.tools.pointer", keys: ["v"], route: Global, menu_id: 1080 };
	TrackSelectTool { cpp: "trackselecttool", i18n: "menu.tools.track_select", keys: ["d"], route: Global, menu_id: 1081 };
	EditTool { cpp: "edittool", i18n: "menu.tools.edit", keys: ["x"], route: Global, menu_id: 1082 };
	RippleTool { cpp: "rippletool", i18n: "menu.tools.ripple", keys: ["b"], route: Global, menu_id: 1083 };
	RollingTool { cpp: "rollingtool", i18n: "menu.tools.rolling", keys: ["n"], route: Global, menu_id: 1084 };
	RazorTool { cpp: "razortool", i18n: "menu.tools.razor_tool", keys: ["c"], route: Global, menu_id: 1085 };
	SlipTool { cpp: "sliptool", i18n: "menu.tools.slip", keys: ["y"], route: Global, menu_id: 1086 };
	SlideTool { cpp: "slidetool", i18n: "menu.tools.slide", keys: ["u"], route: Global, menu_id: 1087 };
	HandTool { cpp: "handtool", i18n: "menu.tools.hand", keys: ["h"], route: Global, menu_id: 1088 };
	ZoomTool { cpp: "zoomtool", i18n: "menu.tools.zoom_tool", keys: ["z"], route: Global, menu_id: 1089 };
	TransitionTool { cpp: "transitiontool", i18n: "menu.tools.transition", keys: ["t"], route: Global, menu_id: 1090 };
	AddTool { cpp: "addtool", i18n: "menu.tools.add", keys: ["a"], route: Global, menu_id: 1091 };
	RecordTool { cpp: "recordtool", i18n: "menu.tools.record", keys: ["r"], route: Global, menu_id: 1092 };
	AddEmpty { cpp: "add:empty", i18n: "menu.tools.addable.empty", keys: [], route: Global, menu_id: 1100 };
	AddBars { cpp: "add:bars", i18n: "menu.tools.addable.bars", keys: [], route: Global, menu_id: 1101 };
	AddShape { cpp: "add:shape", i18n: "menu.tools.addable.shape", keys: [], route: Global, menu_id: 1102 };
	AddSolid { cpp: "add:solid", i18n: "menu.tools.addable.solid", keys: [], route: Global, menu_id: 1103 };
	AddTitle { cpp: "add:title", i18n: "menu.tools.addable.title", keys: [], route: Global, menu_id: 1104 };
	AddTone { cpp: "add:tone", i18n: "menu.tools.addable.tone", keys: [], route: Global, menu_id: 1105 };
	AddSubtitle { cpp: "add:subtitle", i18n: "menu.tools.addable.subtitle", keys: [], route: Global, menu_id: 1106 };
	Snapping { cpp: "snapping", i18n: "menu.tools.snapping", keys: ["s"], route: Global, menu_id: 1110 };
	UseProxyMedia { cpp: "useproxymedia", i18n: "menu.tools.use_proxy", keys: [], route: Global, menu_id: 1111 };
	ProxySettings { cpp: "proxysettings", i18n: "menu.tools.proxy_settings", keys: [], route: Global, menu_id: 1112 };
	// The timeline clip context menu's synchronize entries (context-menu
	// only — they appear in the clip menu, not the menu bar).
	SyncBySourceTime { cpp: "syncsourcetime", i18n: "timeline.context.sync_source_time", keys: [], route: FocusedPanel, menu_id: 1130 };
	SyncByWaveform { cpp: "syncwaveform", i18n: "timeline.context.sync_waveform", keys: ["ctrl-shift-w"], route: FocusedPanel, menu_id: 1131 };
	SyncByWaveformSpeed { cpp: "syncwaveformspeed", i18n: "timeline.context.sync_waveform_speed", keys: [], route: FocusedPanel, menu_id: 1132 };
	// The multicam source-switch hotkeys. Like the C++ `QShortcut`s attached
	// directly to the `MulticamWidget`, they are panel-context hotkeys, not
	// menu items: `menu_id` is [`HIDDEN_MENU_ID`] and the menus never list
	// them. They route to the focused panel (the multicam panel handles
	// them; any other focused panel falls through to the no-op global
	// handler). The digit keys switch and split the clip (the C++ plain
	// `1`..`9`); the `secondary-` variants switch without splitting
	// (`Ctrl+1`..`Ctrl+9`).
	MulticamSwitch1 { cpp: "multicamswitch1", i18n: "multicam.switch_1", keys: ["1"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch2 { cpp: "multicamswitch2", i18n: "multicam.switch_2", keys: ["2"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch3 { cpp: "multicamswitch3", i18n: "multicam.switch_3", keys: ["3"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch4 { cpp: "multicamswitch4", i18n: "multicam.switch_4", keys: ["4"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch5 { cpp: "multicamswitch5", i18n: "multicam.switch_5", keys: ["5"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch6 { cpp: "multicamswitch6", i18n: "multicam.switch_6", keys: ["6"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch7 { cpp: "multicamswitch7", i18n: "multicam.switch_7", keys: ["7"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch8 { cpp: "multicamswitch8", i18n: "multicam.switch_8", keys: ["8"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitch9 { cpp: "multicamswitch9", i18n: "multicam.switch_9", keys: ["9"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit1 { cpp: "multicamswitch1nosplit", i18n: "multicam.switch_1", keys: ["secondary-1"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit2 { cpp: "multicamswitch2nosplit", i18n: "multicam.switch_2", keys: ["secondary-2"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit3 { cpp: "multicamswitch3nosplit", i18n: "multicam.switch_3", keys: ["secondary-3"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit4 { cpp: "multicamswitch4nosplit", i18n: "multicam.switch_4", keys: ["secondary-4"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit5 { cpp: "multicamswitch5nosplit", i18n: "multicam.switch_5", keys: ["secondary-5"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit6 { cpp: "multicamswitch6nosplit", i18n: "multicam.switch_6", keys: ["secondary-6"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit7 { cpp: "multicamswitch7nosplit", i18n: "multicam.switch_7", keys: ["secondary-7"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit8 { cpp: "multicamswitch8nosplit", i18n: "multicam.switch_8", keys: ["secondary-8"], route: FocusedPanel, menu_id: 0 };
	MulticamSwitchNoSplit9 { cpp: "multicamswitch9nosplit", i18n: "multicam.switch_9", keys: ["secondary-9"], route: FocusedPanel, menu_id: 0 };
	Preferences { cpp: "prefs", i18n: "menu.view.preferences", keys: ["secondary-,"], route: Global, menu_id: 305 };

	// --- Help ---------------------------------------------------------------
	ActionSearch { cpp: "actionsearch", i18n: "menu.help.action_search", keys: ["/"], route: Global, menu_id: 1120 };
	Feedback { cpp: "feedback", i18n: "menu.help.feedback", keys: [], route: Global, menu_id: 1121 };
	About { cpp: "about", i18n: "menu.help.about", keys: [], route: Global, menu_id: 801 };
}

/// Where an action is dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
	/// The app shell handles it directly.
	Global,
	/// The focused panel gets it first (through
	/// [`crate::panels::commands::PanelCommandHandler`]); the shell's global
	/// handler is the fallback when the panel does not implement it.
	FocusedPanel,
}

/// One registry entry: everything the menu bar, the keymap and the (future)
/// shortcut preferences / action search need to know about an action.
pub struct ActionEntry {
	/// The action's identity.
	pub action: ActionId,
	/// The stable C++ action id (`mainmenu.cpp` / `menushared.cpp`), kept
	/// for future shortcut-file (`id\t键序`) compatibility.
	pub cpp_id: &'static str,
	/// The menu label's i18n key (present in both language tables).
	pub i18n_key: &'static str,
	/// The default key(s) in gpui keystroke syntax; the first one is the
	/// one the menus display. Empty = unbound.
	pub default_keys: &'static [&'static str],
	/// Where the action routes (shell or focused panel).
	pub route: Route,
	/// Builds the gpui action value (for key bindings and dispatch).
	pub build: fn() -> Box<dyn Action>,
}

impl ActionEntry {
	/// The menu item id of this entry's action.
	pub const fn menu_id(&self) -> usize {
		self.action.menu_id()
	}
}

/// The menu id of the panel-context hotkeys that have no menu item (the
/// multicam source-switch keys). [`ActionId::menu_id`] returns it for those
/// actions; the menus never build an item with this id, and
/// [`entry_for_menu_id`] refuses it so a stray menu dispatch can never hit
/// a hidden action.
pub const HIDDEN_MENU_ID: usize = 0;

/// The registry entry bound to a menu item id, if any. Menu ids
/// [`HIDDEN_MENU_ID`] (the panel-context hotkeys' placeholder) resolve to
/// `None`.
pub fn entry_for_menu_id(id: usize) -> Option<&'static ActionEntry> {
	if id == HIDDEN_MENU_ID {
		return None;
	}
	REGISTRY.iter().find(|entry| entry.action.menu_id() == id)
}

/// The global key bindings for every action's *effective* key. The shell
/// dispatches them wherever the focus is (the modal guard in the shell's
/// action listeners suppresses them while a dialog is open). An unbound
/// action contributes nothing.
///
/// The multicam source-switch keys (`1..9` / `⌘1..⌘9`) are the exception:
/// they are scoped to the multicam panel's key context. Bound globally
/// they would swallow the digit keys everywhere — the matched action
/// bubbles through the focused panel and stops the keystroke before any
/// value box / hex field / text input can type `1..9`.
pub fn key_bindings() -> Vec<KeyBinding> {
	let mut bindings = Vec::new();
	for entry in REGISTRY {
		// The stable C++ ids of the 18 source-switch actions all start
		// with `multicamswitch` (1..9 and 1nosplit..9nosplit); nothing
		// else shares the prefix.
		let context = entry
			.cpp_id
			.starts_with("multicamswitch")
			.then(|| Rc::new(KeyBindingContextPredicate::parse("MulticamPanel").unwrap()));
		for key in effective_keys(entry) {
			let binding = KeyBinding::load(
				&key,
				(entry.build)(),
				context.clone(),
				false,
				None,
				&gpui::DummyKeyboardMapper,
			)
			.unwrap_or_else(|_| panic!("invalid effective key {key:?} for {}", entry.cpp_id));
			bindings.push(binding);
		}
	}
	bindings
}

/// The menu-bar label for a keystroke pattern: macOS-style glyphs (⌘⇧⌥⌃)
/// plus arrow / space glyphs, e.g. `secondary-shift-z` → `⇧⌘Z` (on the
/// other platforms the same keys render as `Ctrl+Shift+Z`). The space key
/// name is localized through i18n (`shortcut.space`).
pub fn display_shortcut(action: ActionId) -> Option<String> {
	let keys = effective_keys(action.entry());
	let key = keys.first()?;
	let keystroke = gpui::Keystroke::parse(key).expect("effective keys parse (tests enforce it)");

	// `secondary-` parses to `platform` on macOS and `control` elsewhere;
	// the modifier renderers below follow the same split.
	let macos = cfg!(target_os = "macos");
	let mut label = String::new();
	let mut push = |name: &str, glyph: char| {
		if macos {
			label.push(glyph);
		} else {
			label.push_str(name);
			label.push('+');
		}
	};
	if keystroke.modifiers.control {
		push("Ctrl", '⌃');
	}
	if keystroke.modifiers.alt {
		push("Alt", '⌥');
	}
	if keystroke.modifiers.shift {
		push("Shift", '⇧');
	}
	if keystroke.modifiers.platform {
		push("Win", '⌘');
	}
	match keystroke.key.as_str() {
		"left" => label.push('←'),
		"right" => label.push('→'),
		"up" => label.push('↑'),
		"down" => label.push('↓'),
		"backspace" => label.push('⌫'),
		"delete" => label.push_str("Del"),
		"space" => label.push_str(crate::i18n::tr("shortcut.space")),
		"home" => label.push_str("Home"),
		"end" => label.push_str("End"),
		other if other.len() == 1 => label.push_str(&other.to_uppercase()),
		other => {
			// Named keys (f12, tab, …): capitalize the f-number form.
			if let Some(rest) = other.strip_prefix('f') {
				label.push_str(&format!("F{rest}"));
			} else {
				label.push_str(&format!("{}{}", other[..1].to_uppercase(), &other[1..]));
			}
		}
	}
	Some(label)
}

// ---------------------------------------------------------------------------
// Custom shortcut overrides (the `<config>/shortcuts` file)
// ---------------------------------------------------------------------------
//
// The C++ `MainWindow` keeps a `<config>/shortcuts` text file of
// `id<TAB>keys` lines (mainwindow.cpp:768-863): each line overrides one
// action's key(s), the file is read at startup and written back only with the
// entries that differ from the defaults (an empty diff removes the file).
// This port keeps the same line shape but stores the key sequence in **gpui
// keystroke syntax** (`secondary-s`, `alt-shift-z`, `delete`, …) instead of
// Qt's `QKeySequence::toString` (`Ctrl+S`):
//
// * `id\tkey` — bind that single key;
// * `id\t` (empty value) — unbind the action entirely;
// * absent line — the registry defaults.
//
// That is a deliberate deviation from the C++ file (a C++ `Ctrl+S` line
// would not parse as a gpui keystroke and vice versa): the C++ *format* is
// kept, not the byte-level values. The key strings themselves are
// platform-independent — `cmd-`, `super-` and `win-` all parse to gpui's
// "platform" modifier on every OS, so a file written on macOS loads on
// Linux (and the canonical form used for comparisons normalizes the
// `secondary-` spelling used by the registry defaults).
//
// The override table is a process-global behind a mutex (like the config
// store): the menu bar, `key_bindings` and `display_shortcut` all read the
// effective keys on demand, so a change is visible the moment it is made and
// the shell's `rebind_keys` + `rebuild_menu_bar` turn it live.

/// One action's override:
/// * absent from the map — the registry defaults apply;
/// * [`ShortcutOverride::Unbound`] — explicitly unbound (no keys at all);
/// * [`ShortcutOverride::Keys`] — the custom key(s).
enum ShortcutOverride {
	Unbound,
	Keys(Vec<String>),
}

/// The process-global override table, keyed by the stable cpp id.
static OVERRIDES: OnceLock<Mutex<HashMap<String, ShortcutOverride>>> = OnceLock::new();

/// The global override table.
fn overrides() -> &'static Mutex<HashMap<String, ShortcutOverride>> {
	OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Serializes tests that touch the process-global override table (the same
/// pattern the i18n tests use for the language global).
#[cfg(test)]
pub(crate) fn shortcuts_test_lock() -> &'static Mutex<()> {
	static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
	LOCK.get_or_init(|| Mutex::new(()))
}

/// The path of the custom-shortcuts file: `<config>/shortcuts`, exactly like
/// the C++ `MainWindow::get_custom_shortcuts_file`.
pub fn custom_shortcuts_path() -> String {
	let dir = oak_core::filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
	format!("{}/shortcuts", dir.trim_end_matches('/'))
}

/// Parses one `id\tkey` line. `None` for blank lines, missing ids and keys
/// that are not valid gpui keystrokes. An empty key value means "unbound".
///
/// The tab is split *before* trimming (a bare trailing `\t` would otherwise
/// be trimmed away and the unbound marker lost); only the id and the key
/// value are trimmed.
fn parse_shortcut_line(line: &str) -> Option<(String, Vec<String>)> {
	// `str::lines` strips `\n` but keeps the `\r` of CRLF files.
	let line = line.strip_suffix('\r').unwrap_or(line);
	if line.trim().is_empty() {
		return None;
	}
	let (id, value) = line.split_once('\t')?; // a line without a tab is malformed
	let id = id.trim();
	if REGISTRY.iter().all(|entry| entry.cpp_id != id) {
		return None;
	}
	let value = value.trim();
	let keys = if value.is_empty() {
		Vec::new()
	} else {
		if gpui::Keystroke::parse(value).is_err() {
			return None;
		}
		vec![value.to_string()]
	};
	Some((id.to_string(), keys))
}

/// Parses a whole shortcuts file body (`id\tkey` lines; blank lines and
/// malformed lines are skipped).
pub fn parse_shortcut_file(contents: &str) -> Vec<(String, Vec<String>)> {
	contents.lines().filter_map(parse_shortcut_line).collect()
}

/// Serializes override lines back to `id\tkey` (empty key = unbound).
pub fn serialize_shortcut_file(entries: &[(String, Vec<String>)]) -> String {
	let mut out = String::new();
	for (index, (id, keys)) in entries.iter().enumerate() {
		if index > 0 {
			out.push('\n');
		}
		out.push_str(id);
		out.push('\t');
		if let Some(key) = keys.first() {
			out.push_str(key);
		}
	}
	out
}

/// Replaces the whole override table with `entries` (startup load and the
/// Keyboard tab's Import button — anything not listed falls back to default).
pub fn apply_shortcut_overrides(entries: Vec<(String, Vec<String>)>) {
	let mut map = HashMap::new();
	for (id, keys) in entries {
		map.insert(
			id,
			if keys.is_empty() {
				ShortcutOverride::Unbound
			} else {
				ShortcutOverride::Keys(keys)
			},
		);
	}
	*overrides().lock().unwrap() = map;
}

/// Removes every override (the Keyboard tab's Reset All).
pub fn reset_all_custom_shortcuts() {
	overrides().lock().unwrap().clear();
}

/// Removes the override of one action (back to the registry defaults).
pub fn reset_custom_shortcut(cpp_id: &str) {
	overrides().lock().unwrap().remove(cpp_id);
}

/// Sets the custom keys of one action; an empty list unbinds it.
pub fn set_custom_shortcut(cpp_id: &str, keys: Vec<String>) {
	overrides().lock().unwrap().insert(
		cpp_id.to_string(),
		if keys.is_empty() {
			ShortcutOverride::Unbound
		} else {
			ShortcutOverride::Keys(keys)
		},
	);
}

/// Whether any override is currently set (tests / file presence checks).
pub fn has_custom_shortcuts() -> bool {
	!overrides().lock().unwrap().is_empty()
}

/// The canonical (parse → unparse) form of a keystroke string, used to
/// compare keys regardless of the `secondary-` vs `cmd-`/`super-`/`win-`
/// spelling (the parser maps all of them to the same modifier bits).
fn canonical_key(key: &str) -> Option<String> {
	gpui::Keystroke::parse(key)
		.ok()
		.map(|keystroke| keystroke.unparse())
}

/// The canonical form of a key list (accepts both `&str` and `String`
/// slices, so the `&'static [&str]` registry defaults and the `Vec<String>`
/// overrides share one code path).
fn canonical_keys<K: AsRef<str>>(keys: &[K]) -> Vec<String> {
	keys.iter()
		.filter_map(|key| canonical_key(key.as_ref()))
		.collect()
}

/// The action's effective key list: the override when one is set, else the
/// registry defaults (empty = unbound).
pub fn effective_keys(entry: &ActionEntry) -> Vec<String> {
	match overrides().lock().unwrap().get(entry.cpp_id) {
		Some(ShortcutOverride::Unbound) => Vec::new(),
		Some(ShortcutOverride::Keys(keys)) => keys.clone(),
		None => entry
			.default_keys
			.iter()
			.map(|key| key.to_string())
			.collect(),
	}
}

/// The registry defaults in canonical form (the save-diff baseline).
fn default_canonical(entry: &ActionEntry) -> Vec<String> {
	canonical_keys(entry.default_keys)
}

/// Loads `<config>/shortcuts` and applies it (startup). A missing file is a
/// no-op (everything stays at the defaults).
pub fn load_custom_shortcuts() {
	let path = custom_shortcuts_path();
	if let Ok(contents) = std::fs::read_to_string(&path) {
		apply_shortcut_overrides(parse_shortcut_file(&contents));
	}
}

/// Loads overrides from an explicit file path (the Keyboard tab's Import).
/// Returns the number of lines applied.
pub fn load_custom_shortcuts_from(path: &str) -> Result<usize, String> {
	let contents = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
	let entries = parse_shortcut_file(&contents);
	let count = entries.len();
	apply_shortcut_overrides(entries);
	Ok(count)
}

/// Writes the overrides that differ from the registry defaults to `path`
/// (canonical comparison, so an override spelling `cmd-s` for a default
/// `secondary-s` counts as unchanged). An all-default state removes the
/// file, exactly like the C++. Returns the number of lines written.
pub fn save_custom_shortcuts_to(path: &str) -> Result<usize, String> {
	let mut lines: Vec<(String, Vec<String>)> = Vec::new();
	{
		let map = overrides().lock().unwrap();
		for entry in REGISTRY {
			let Some(override_) = map.get(entry.cpp_id) else {
				continue;
			};
			let effective = match override_ {
				ShortcutOverride::Unbound => Vec::new(),
				ShortcutOverride::Keys(keys) => keys.clone(),
			};
			if canonical_keys(&effective) != default_canonical(entry) {
				lines.push((entry.cpp_id.to_string(), effective));
			}
		}
	}
	if lines.is_empty() {
		match std::fs::remove_file(path) {
			Ok(()) => {}
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
			Err(e) => return Err(e.to_string()),
		}
		return Ok(0);
	}
	std::fs::write(path, serialize_shortcut_file(&lines)).map_err(|e| e.to_string())?;
	Ok(lines.len())
}

/// Saves the diff to the configured location (every key-capture commit
/// calls this so the change survives restarts).
pub fn save_custom_shortcuts() -> Result<usize, String> {
	save_custom_shortcuts_to(&custom_shortcuts_path())
}

/// The first registry action whose *effective* keys contain the canonical
/// `canon`, if any (the current owner of a key, for conflict detection).
pub fn owner_of_shortcut(canon: &str) -> Option<ActionId> {
	REGISTRY
		.iter()
		.find(|entry| {
			effective_keys(entry)
				.iter()
				.any(|key| canonical_key(key).as_deref() == Some(canon))
		})
		.map(|entry| entry.action)
}

/// Moves the canonical key `canon` away from every *other* action that
/// currently binds it, shrinking their override by one key (or unbinding
/// them). Returns the action the key was first taken from, if any — the
/// conflict policy: the new assignment wins and the displaced action falls
/// back to its remaining keys / to none. Simple and explicit (the alternative
/// — refusing the assignment — leaves the user stuck when the default keys
/// collide with a popular choice).
pub fn steal_shortcut_for(entry: &ActionEntry, canon: &str) -> Option<ActionId> {
	let mut stolen = None;
	for other in REGISTRY {
		if other.action == entry.action {
			continue;
		}
		let has = effective_keys(other)
			.iter()
			.any(|key| canonical_key(key).as_deref() == Some(canon));
		if has {
			let mut keys = effective_keys(other);
			keys.retain(|key| canonical_key(key).as_deref() != Some(canon));
			set_custom_shortcut(other.cpp_id, keys);
			stolen.get_or_insert(other.action);
		}
	}
	stolen
}

/// The timeline's editing tools (the C++ `Tool` enum's pointer tools): the
/// Tools menu's mutually exclusive group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
	Pointer,
	TrackSelect,
	Edit,
	Ripple,
	Rolling,
	Razor,
	Slip,
	Slide,
	Hand,
	Zoom,
	Transition,
	Add,
	Record,
}

impl Tool {
	/// The action selecting this tool.
	pub const fn action(self) -> ActionId {
		match self {
			Tool::Pointer => ActionId::PointerTool,
			Tool::TrackSelect => ActionId::TrackSelectTool,
			Tool::Edit => ActionId::EditTool,
			Tool::Ripple => ActionId::RippleTool,
			Tool::Rolling => ActionId::RollingTool,
			Tool::Razor => ActionId::RazorTool,
			Tool::Slip => ActionId::SlipTool,
			Tool::Slide => ActionId::SlideTool,
			Tool::Hand => ActionId::HandTool,
			Tool::Zoom => ActionId::ZoomTool,
			Tool::Transition => ActionId::TransitionTool,
			Tool::Add => ActionId::AddTool,
			Tool::Record => ActionId::RecordTool,
		}
	}

	/// The tool an action selects, if it is a tool action.
	pub const fn from_action(action: ActionId) -> Option<Tool> {
		match action {
			ActionId::PointerTool => Some(Tool::Pointer),
			ActionId::TrackSelectTool => Some(Tool::TrackSelect),
			ActionId::EditTool => Some(Tool::Edit),
			ActionId::RippleTool => Some(Tool::Ripple),
			ActionId::RollingTool => Some(Tool::Rolling),
			ActionId::RazorTool => Some(Tool::Razor),
			ActionId::SlipTool => Some(Tool::Slip),
			ActionId::SlideTool => Some(Tool::Slide),
			ActionId::HandTool => Some(Tool::Hand),
			ActionId::ZoomTool => Some(Tool::Zoom),
			ActionId::TransitionTool => Some(Tool::Transition),
			ActionId::AddTool => Some(Tool::Add),
			ActionId::RecordTool => Some(Tool::Record),
			_ => None,
		}
	}

	/// The timeline-widget tool this app tool drives, if the widget models it.
	///
	/// `Edit`/`Transition`/`Add`/`Record`/`Hand` have no widget counterpart
	/// (the widget's pointer tool covers them); those map to `None` and the
	/// timeline keeps its previous tool.
	pub const fn timeline_tool(self) -> Option<gpui::timeline::TimelineTool> {
		match self {
			Tool::Pointer => Some(gpui::timeline::TimelineTool::Select),
			Tool::TrackSelect => Some(gpui::timeline::TimelineTool::TrackSelect),
			Tool::Ripple => Some(gpui::timeline::TimelineTool::Ripple),
			Tool::Rolling => Some(gpui::timeline::TimelineTool::Roll),
			Tool::Razor => Some(gpui::timeline::TimelineTool::Razor),
			Tool::Slip => Some(gpui::timeline::TimelineTool::Slip),
			Tool::Slide => Some(gpui::timeline::TimelineTool::Slide),
			Tool::Zoom => Some(gpui::timeline::TimelineTool::Zoom),
			_ => None,
		}
	}
}

/// Extension trait mapping the timeline widget's tool back to the app tool
/// (for the toolbar highlight). Unmodeled tools fall back to the pointer.
pub trait TimelineToolExt {
	fn app_tool(self) -> Tool;
}

impl TimelineToolExt for gpui::timeline::TimelineTool {
	fn app_tool(self) -> Tool {
		match self {
			gpui::timeline::TimelineTool::Select => Tool::Pointer,
			gpui::timeline::TimelineTool::TrackSelect => Tool::TrackSelect,
			gpui::timeline::TimelineTool::Ripple => Tool::Ripple,
			gpui::timeline::TimelineTool::Roll => Tool::Rolling,
			gpui::timeline::TimelineTool::Razor => Tool::Razor,
			gpui::timeline::TimelineTool::Slip => Tool::Slip,
			gpui::timeline::TimelineTool::Slide => Tool::Slide,
			gpui::timeline::TimelineTool::Zoom => Tool::Zoom,
			_ => Tool::Pointer,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every stable id is unique (the shortcut-file format keys on it).
	#[test]
	fn registry_ids_are_unique() {
		let mut seen = std::collections::HashSet::new();
		for entry in REGISTRY {
			assert!(
				seen.insert(entry.cpp_id),
				"duplicate cpp id {}",
				entry.cpp_id
			);
		}
	}

	/// Every menu id is unique (the menu bar reports plain ids; a duplicate
	/// would make two items dispatch the same action). Panel-context
	/// hotkeys share [`HIDDEN_MENU_ID`] (no menu item) and are skipped.
	#[test]
	fn registry_menu_ids_are_unique() {
		let mut seen = std::collections::HashSet::new();
		for entry in REGISTRY {
			if entry.menu_id() == HIDDEN_MENU_ID {
				continue;
			}
			assert!(
				seen.insert(entry.menu_id()),
				"duplicate menu id {} ({})",
				entry.menu_id(),
				entry.cpp_id
			);
		}
	}

	/// Every default key parses as a gpui keystroke (the table is static
	/// data, so a typo would otherwise surface only as a dead shortcut at
	/// runtime — `key_bindings` would panic at startup).
	#[test]
	fn every_default_key_parses() {
		for entry in REGISTRY {
			for key in entry.default_keys {
				assert!(
					gpui::Keystroke::parse(key).is_ok(),
					"invalid keystroke {key:?} on {}",
					entry.cpp_id
				);
			}
		}
	}

	/// No two actions claim the same keystroke (a conflict would make the
	/// keymap dispatch whichever binding was registered last).
	#[test]
	fn default_keys_are_conflict_free() {
		let mut seen = std::collections::HashMap::new();
		for entry in REGISTRY {
			for key in entry.default_keys {
				let parsed = gpui::Keystroke::parse(key).unwrap();
				let canon = parsed.unparse();
				let previous = seen.insert(canon.clone(), entry.cpp_id);
				assert!(
					previous.is_none(),
					"keystroke {canon} bound to both {} and {}",
					previous.unwrap(),
					entry.cpp_id
				);
			}
		}
	}

	/// The multicam source-switch hotkeys are panel-context: their
	/// bindings carry the `MulticamPanel` key context so the digit keys
	/// stay free for text entry outside the panel, and every other binding
	/// stays global. (Regression: bound globally, `1..9` matched in every
	/// focused panel and swallowed the digits before any value box / hex
	/// field / text input could type them.)
	#[test]
	fn multicam_bindings_are_scoped_to_the_panel_context() {
		let panel_context = Some(Rc::new(
			KeyBindingContextPredicate::parse("MulticamPanel").unwrap(),
		));
		let multicam_actions: &[&dyn Action] = &[
			&MulticamSwitch1, &MulticamSwitch2, &MulticamSwitch3, &MulticamSwitch4,
			&MulticamSwitch5, &MulticamSwitch6, &MulticamSwitch7, &MulticamSwitch8,
			&MulticamSwitch9, &MulticamSwitchNoSplit1, &MulticamSwitchNoSplit2,
			&MulticamSwitchNoSplit3, &MulticamSwitchNoSplit4, &MulticamSwitchNoSplit5,
			&MulticamSwitchNoSplit6, &MulticamSwitchNoSplit7, &MulticamSwitchNoSplit8,
			&MulticamSwitchNoSplit9,
		];
		let mut scoped = 0usize;
		for binding in key_bindings() {
			let is_multicam = multicam_actions
				.iter()
				.any(|known| binding.action().partial_eq(*known));
			if is_multicam {
				scoped += 1;
				assert_eq!(
					binding.predicate(),
					panel_context,
					"the source-switch binding must be gated on the MulticamPanel key context"
				);
			} else {
				assert!(
					binding.predicate().is_none(),
					"non-multicam bindings must stay global"
				);
			}
		}
		assert_eq!(scoped, 18, "all 18 source-switch bindings are panel-scoped");
	}

	/// Every registry action appears somewhere in the menu tree (an action
	/// without a menu entry is unreachable with the mouse, and the registry
	/// is meant to drive the menus).
	#[test]
	fn every_action_appears_in_the_menu_tree() {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");

		use crate::oakui::component::menu;
		fn collect(menu: &menu::Menu, out: &mut Vec<usize>) {
			for item in &menu.items {
				out.push(item.id);
				if let Some(sub) = &item.submenu {
					collect(sub, out);
				}
			}
		}
		let mut ids = Vec::new();
		for entry in crate::app::make_menus_for_test() {
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
		for entry in REGISTRY {
			// The panel-context hotkeys (multicam source switches) are bound
			// to the focused panel, not to any menu — the C++ attaches them
			// straight to the MulticamWidget.
			if entry.menu_id() == HIDDEN_MENU_ID {
				continue;
			}
			assert!(
				ids.contains(&entry.menu_id()),
				"action {} (menu id {}) has no menu item",
				entry.cpp_id,
				entry.menu_id()
			);
		}
	}

	/// Every registry i18n key exists in both language tables with a
	/// non-empty value (the menu labels come straight from `tr`).
	#[test]
	fn every_i18n_key_exists_in_both_languages() {
		for entry in REGISTRY {
			for code in ["en-US", "zh-CN"] {
				let _guard = crate::i18n::lang_test_lock()
					.lock()
					.unwrap_or_else(|e| e.into_inner());
				crate::i18n::set_language_code(code);
				let value = crate::i18n::tr(entry.i18n_key);
				assert_ne!(
					value, entry.i18n_key,
					"i18n key {} ({code}) is missing for action {}",
					entry.i18n_key, entry.cpp_id
				);
				assert!(!value.is_empty());
			}
		}
		// The loop leaves zh-CN active; restore the default so lock-free
		// tests building English menus are not fooled mid-assert.
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");
	}

	/// The shortcut display formatter renders the documented labels for a
	/// sample of shapes (modifier stacks, arrows, named keys, punctuation).
	/// The glyph form is the macOS rendering; other platforms spell the
	/// modifiers out (covered by the parser tests instead).
	#[test]
	#[cfg(target_os = "macos")]
	fn display_shortcut_formats_labels() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		assert_eq!(display_shortcut(ActionId::Redo).as_deref(), Some("⇧⌘Z"));
		assert_eq!(
			display_shortcut(ActionId::SplitAtPlayhead).as_deref(),
			Some("⌘K")
		);
		assert_eq!(display_shortcut(ActionId::NudgeLeft).as_deref(), Some("⌥←"));
		assert_eq!(
			display_shortcut(ActionId::FullScreen).as_deref(),
			Some("F11")
		);
		assert_eq!(display_shortcut(ActionId::Insert).as_deref(), Some(","));
		assert!(display_shortcut(ActionId::About).is_none());
	}

	// -------------------------------------------------------------------
	// Custom shortcut overrides (stage 7)
	// -------------------------------------------------------------------

	/// A unique temporary directory for one test (the `shortcuts` file
	/// round-trip), so parallel tests never collide on the same path.
	fn temp_dir(label: &str) -> String {
		static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
		let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let path = std::env::temp_dir().join(format!(
			"oak-shortcuts-test-{label}-{}-{n}",
			std::process::id()
		));
		let _ = std::fs::remove_dir_all(&path);
		std::fs::create_dir_all(&path).unwrap();
		path.to_string_lossy().into_owned()
	}

	/// Parsing skips blank lines, unknown ids and unparseable keystrokes; an
	/// empty value after the tab means "unbound".
	#[test]
	fn parse_shortcut_file_skips_malformed_lines() {
		let entries = parse_shortcut_file(
			"newproj\tsecondary-n\n\nbogusaction\tsecondary-x\nundo\t\nsaveproj\tnot-a-key\ncut\tsecondary-c\n",
		);
		assert_eq!(
			entries,
			vec![
				("newproj".to_string(), vec!["secondary-n".to_string()]),
				("undo".to_string(), vec![]), // unbound
				("cut".to_string(), vec!["secondary-c".to_string()]),
			]
		);
	}

	/// Write → clear → reload round-trips the effective keys, and the file
	/// only carries the entries that differ from the registry defaults.
	#[test]
	fn shortcuts_file_round_trips_through_the_override_table() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		reset_all_custom_shortcuts();
		let dir = temp_dir("roundtrip");
		let path = format!("{dir}/shortcuts");

		// `newproj` = its own default (spelled in registry syntax) → not a
		// diff; the other two differ (`undo` gets unbound).
		set_custom_shortcut("newproj", vec!["secondary-n".to_string()]);
		set_custom_shortcut("saveproj", vec!["cmd-alt-s".to_string()]);
		set_custom_shortcut("undo", Vec::new()); // unbound
		assert_eq!(save_custom_shortcuts_to(&path).unwrap(), 2);

		let contents = std::fs::read_to_string(&path).unwrap();
		assert!(contents.contains("saveproj\t"), "file: {contents}");
		assert!(contents.contains("undo\t"), "file: {contents}");
		assert!(!contents.contains("newproj"), "file: {contents}");

		// Reload into a fresh table.
		reset_all_custom_shortcuts();
		assert!(!has_custom_shortcuts());
		assert_eq!(load_custom_shortcuts_from(&path).unwrap(), 2);
		assert_eq!(
			effective_keys(ActionId::SaveProject.entry()),
			vec!["cmd-alt-s".to_string()]
		);
		assert_eq!(effective_keys(ActionId::Undo.entry()), Vec::<String>::new());
		// The un-overridden action keeps its registry default.
		assert_eq!(
			effective_keys(ActionId::NewProject.entry()),
			vec!["secondary-n".to_string()]
		);
	}

	/// A default-equal override is dropped on save (canonical comparison), and
	/// an all-default table removes the file entirely (the C++ behavior).
	#[test]
	fn save_writes_only_entries_that_differ_from_default() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		reset_all_custom_shortcuts();
		let dir = temp_dir("diff");
		let path = format!("{dir}/shortcuts");

		set_custom_shortcut("newproj", vec!["secondary-n".to_string()]);
		set_custom_shortcut("undo", vec!["cmd-alt-z".to_string()]);
		assert_eq!(save_custom_shortcuts_to(&path).unwrap(), 1);
		let contents = std::fs::read_to_string(&path).unwrap();
		assert!(contents.contains("undo\tcmd-alt-z"), "file: {contents}");
		assert!(!contents.contains("newproj"), "file: {contents}");

		// All-default → the file disappears.
		reset_all_custom_shortcuts();
		set_custom_shortcut("newproj", vec!["secondary-n".to_string()]);
		assert_eq!(save_custom_shortcuts_to(&path).unwrap(), 0);
		assert!(!std::path::Path::new(&path).exists());
	}

	/// Effective keys fall back to the registry defaults and the multi-key
	/// defaults stay intact until overridden.
	#[test]
	fn effective_keys_fall_back_to_defaults() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		reset_all_custom_shortcuts();
		// Delete defaults to ["delete", "backspace"].
		assert_eq!(
			effective_keys(ActionId::Delete.entry()),
			vec!["delete".to_string(), "backspace".to_string()]
		);
		set_custom_shortcut("delete", vec!["secondary-x".to_string()]);
		assert_eq!(
			effective_keys(ActionId::Delete.entry()),
			vec!["secondary-x".to_string()]
		);
		reset_custom_shortcut("delete");
		assert_eq!(
			effective_keys(ActionId::Delete.entry()),
			vec!["delete".to_string(), "backspace".to_string()]
		);
	}

	/// Conflict resolution: taking a key away from another action moves the
	/// binding (the displaced action falls back to its remaining keys, or to
	/// none).
	#[test]
	fn stealing_a_key_moves_the_binding_away() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		reset_all_custom_shortcuts();
		let canon = canonical_key("secondary-c").expect("copy's key parses");
		assert_eq!(owner_of_shortcut(&canon), Some(ActionId::Copy));

		// The capture flow: steal the key away from its current owner, then
		// assign it to the target.
		let stolen = steal_shortcut_for(ActionId::Paste.entry(), &canon);
		assert_eq!(stolen, Some(ActionId::Copy));
		set_custom_shortcut(ActionId::Paste.entry().cpp_id, vec![canon.clone()]);
		assert_eq!(owner_of_shortcut(&canon), Some(ActionId::Paste));
		assert!(
			effective_keys(ActionId::Copy.entry()).is_empty(),
			"the displaced Copy must be unbound"
		);

		// A key nobody owns steals nothing.
		reset_all_custom_shortcuts();
		assert_eq!(steal_shortcut_for(ActionId::Paste.entry(), "f24"), None);
	}

	/// Overriding a default with the same key keeps `display_shortcut` stable
	/// (the canonical comparison treats `secondary-` and `cmd-` as equal).
	#[test]
	#[cfg(target_os = "macos")]
	fn display_shortcut_uses_the_effective_key() {
		let _guard = shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		set_custom_shortcut("saveproj", vec!["cmd-alt-s".to_string()]);
		assert_eq!(
			display_shortcut(ActionId::SaveProject).as_deref(),
			Some("⌥⌘S")
		);
		set_custom_shortcut("saveproj", Vec::new()); // unbound → no label
		assert!(display_shortcut(ActionId::SaveProject).is_none());
		reset_all_custom_shortcuts();
	}
}
