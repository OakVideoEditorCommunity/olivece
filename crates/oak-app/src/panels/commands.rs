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

//! Focused-panel command routing: the Rust counterpart of the C++
//! `PanelWidget` virtual command interface (`app/panel/panel.h`'s ~39
//! `play_pause` / `set_in` / `delete_selected` / … overrides). Menu clicks
//! and key presses whose registry entry carries [`Route::FocusedPanel`]
//! land here first: the currently focused panel gets the command, and when
//! it does not implement it (the default methods return `false`) the app
//! shell's global handler runs instead — the
//! `PanelManager::currently_focused()` pattern.
//!
//! Panels override only the subset they implement; [`dispatch_to`] maps an
//! [`ActionId`] onto the matching trait method so panel implementations and
//! the action registry stay in one place.

use gpui::timeline::Frame;
use gpui::{App, Context, Entity};
use gpui_widgets::viewer::PlaybackClock;

use crate::actions::ActionId;
use crate::oakui::{AppEngine, Monitor};

/// The commands a dock panel can handle when it is focused. Every method
/// defaults to "not handled" (`false`), so a panel overrides only its
/// subset; returning `true` stops the shell's global fallback from running.
pub trait PanelCommandHandler: Sized {
	// --- transport (the panel's monitor) ----------------------------------
	fn play_pause(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn prev_frame(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn next_frame(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_start(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_end(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn play_in_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_prev_cut(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_next_cut(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn go_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn shuttle_left(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn shuttle_stop(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn shuttle_right(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}

	// --- in / out points ----------------------------------------------------
	fn set_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn set_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn reset_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn reset_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn clear_in_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}

	// --- selection ----------------------------------------------------------
	fn select_all(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn deselect_all(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}

	// --- editing -------------------------------------------------------------
	fn cut_selected(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn copy_selected(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn paste(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn paste_insert(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn duplicate(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn rename_selected(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn delete_selected(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn ripple_delete(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn split_at_playhead(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn speed_duration(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	/// The clip menu's "Default Transition" item / the Ctrl+Shift+D action:
	/// add a transition of the configured default length at every seam
	/// around the selected clips.
	fn default_transition(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn toggle_links(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn toggle_selected_enabled(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn insert(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn overwrite(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn ripple_to_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn ripple_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn edit_to_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn edit_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn nudge_left(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn nudge_right(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn move_in_to_playhead(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn move_out_to_playhead(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn delete_in_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn ripple_delete_in_to_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn set_marker(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}

	// --- synchronization (the timeline clip menu's Synchronize group) ------
	fn sync_by_source_time(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn sync_by_waveform(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn sync_by_waveform_speed(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}

	// --- multi-camera (the Multicam panel's source-switch hotkeys) ---------
	/// Switches the panel's multicam to source index `source`. `split_clip`
	/// = the change applies from the playhead forward (the clip is split
	/// first). The Multicam panel handles it; every other panel falls
	/// through to the shell's no-op handler.
	fn multicam_switch(&mut self, source: i32, split_clip: bool, cx: &mut Context<Self>) -> bool {
		let _ = (source, split_clip, cx);
		false
	}

	// --- view ----------------------------------------------------------------
	fn zoom_in(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn zoom_out(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn increase_track_height(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn decrease_track_height(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
	fn toggle_show_all(&mut self, _cx: &mut Context<Self>) -> bool {
		false
	}
}

/// Routes `action` to the matching [`PanelCommandHandler`] method; whether
/// the panel handled it. Actions without a panel command (file ops, tools,
/// …) return `false` and fall through to the shell's global handler.
pub fn dispatch_to<P: PanelCommandHandler>(
	panel: &mut P,
	action: ActionId,
	cx: &mut Context<P>,
) -> bool {
	match action {
		ActionId::PlayPause => panel.play_pause(cx),
		ActionId::PrevFrame => panel.prev_frame(cx),
		ActionId::NextFrame => panel.next_frame(cx),
		ActionId::GoToStart => panel.go_to_start(cx),
		ActionId::GoToEnd => panel.go_to_end(cx),
		ActionId::PlayInToOut => panel.play_in_to_out(cx),
		ActionId::GoToPrevCut => panel.go_to_prev_cut(cx),
		ActionId::GoToNextCut => panel.go_to_next_cut(cx),
		ActionId::GoToIn => panel.go_to_in(cx),
		ActionId::GoToOut => panel.go_to_out(cx),
		ActionId::ShuttleLeft => panel.shuttle_left(cx),
		ActionId::ShuttleStop => panel.shuttle_stop(cx),
		ActionId::ShuttleRight => panel.shuttle_right(cx),
		ActionId::SetInPoint => panel.set_in(cx),
		ActionId::SetOutPoint => panel.set_out(cx),
		ActionId::ResetIn => panel.reset_in(cx),
		ActionId::ResetOut => panel.reset_out(cx),
		ActionId::ClearInOut => panel.clear_in_out(cx),
		ActionId::SelectAll => panel.select_all(cx),
		ActionId::DeselectAll => panel.deselect_all(cx),
		ActionId::Cut => panel.cut_selected(cx),
		ActionId::Copy => panel.copy_selected(cx),
		ActionId::Paste => panel.paste(cx),
		ActionId::PasteInsert => panel.paste_insert(cx),
		ActionId::Duplicate => panel.duplicate(cx),
		ActionId::Rename => panel.rename_selected(cx),
		ActionId::Delete => panel.delete_selected(cx),
		ActionId::RippleDelete => panel.ripple_delete(cx),
		ActionId::SplitAtPlayhead => panel.split_at_playhead(cx),
		ActionId::SpeedDuration => panel.speed_duration(cx),
		ActionId::DefaultTransition => panel.default_transition(cx),
		ActionId::LinkUnlink => panel.toggle_links(cx),
		ActionId::EnableDisable => panel.toggle_selected_enabled(cx),
		ActionId::Insert => panel.insert(cx),
		ActionId::Overwrite => panel.overwrite(cx),
		ActionId::RippleToIn => panel.ripple_to_in(cx),
		ActionId::RippleToOut => panel.ripple_to_out(cx),
		ActionId::EditToIn => panel.edit_to_in(cx),
		ActionId::EditToOut => panel.edit_to_out(cx),
		ActionId::NudgeLeft => panel.nudge_left(cx),
		ActionId::NudgeRight => panel.nudge_right(cx),
		ActionId::MoveInToPlayhead => panel.move_in_to_playhead(cx),
		ActionId::MoveOutToPlayhead => panel.move_out_to_playhead(cx),
		ActionId::DeleteInOut => panel.delete_in_to_out(cx),
		ActionId::RippleDeleteInOut => panel.ripple_delete_in_to_out(cx),
		ActionId::Marker => panel.set_marker(cx),
		ActionId::SyncBySourceTime => panel.sync_by_source_time(cx),
		ActionId::SyncByWaveform => panel.sync_by_waveform(cx),
		ActionId::SyncByWaveformSpeed => panel.sync_by_waveform_speed(cx),
		ActionId::MulticamSwitch1 => panel.multicam_switch(0, true, cx),
		ActionId::MulticamSwitch2 => panel.multicam_switch(1, true, cx),
		ActionId::MulticamSwitch3 => panel.multicam_switch(2, true, cx),
		ActionId::MulticamSwitch4 => panel.multicam_switch(3, true, cx),
		ActionId::MulticamSwitch5 => panel.multicam_switch(4, true, cx),
		ActionId::MulticamSwitch6 => panel.multicam_switch(5, true, cx),
		ActionId::MulticamSwitch7 => panel.multicam_switch(6, true, cx),
		ActionId::MulticamSwitch8 => panel.multicam_switch(7, true, cx),
		ActionId::MulticamSwitch9 => panel.multicam_switch(8, true, cx),
		ActionId::MulticamSwitchNoSplit1 => panel.multicam_switch(0, false, cx),
		ActionId::MulticamSwitchNoSplit2 => panel.multicam_switch(1, false, cx),
		ActionId::MulticamSwitchNoSplit3 => panel.multicam_switch(2, false, cx),
		ActionId::MulticamSwitchNoSplit4 => panel.multicam_switch(3, false, cx),
		ActionId::MulticamSwitchNoSplit5 => panel.multicam_switch(4, false, cx),
		ActionId::MulticamSwitchNoSplit6 => panel.multicam_switch(5, false, cx),
		ActionId::MulticamSwitchNoSplit7 => panel.multicam_switch(6, false, cx),
		ActionId::MulticamSwitchNoSplit8 => panel.multicam_switch(7, false, cx),
		ActionId::MulticamSwitchNoSplit9 => panel.multicam_switch(8, false, cx),
		ActionId::ZoomIn => panel.zoom_in(cx),
		ActionId::ZoomOut => panel.zoom_out(cx),
		ActionId::IncreaseTrackHeight => panel.increase_track_height(cx),
		ActionId::DecreaseTrackHeight => panel.decrease_track_height(cx),
		ActionId::ToggleShowAll => panel.toggle_show_all(cx),
		_ => false,
	}
}

/// The transport commands a viewer panel routes to one of the engine's
/// monitors (the program viewer drives [`Monitor::Program`], the source
/// viewer [`Monitor::Source`]). Shuttle left steps back one frame — true
/// reverse playback is an engine transport gap (the C++ `decspeed`
/// behavior approximated like the old shortcut table did). Cut navigation
/// and loop are not implemented by the viewers and fall through.
pub fn viewer_transport<E: AppEngine>(
	engine: &Entity<E>,
	clock: &Entity<E::Clock>,
	monitor: Monitor,
	action: ActionId,
	cx: &mut App,
) -> bool {
	match action {
		ActionId::PlayPause => {
			let playing = clock.read(cx).is_playing();
			engine.update(cx, |engine, cx| {
				if playing {
					engine.pause(monitor, cx);
				} else {
					engine.play(monitor, cx);
				}
			});
			true
		}
		ActionId::PrevFrame => {
			engine.update(cx, |engine, cx| engine.step(monitor, -1, cx));
			true
		}
		ActionId::NextFrame => {
			engine.update(cx, |engine, cx| engine.step(monitor, 1, cx));
			true
		}
		ActionId::GoToStart => {
			engine.update(cx, |engine, cx| engine.request_frame(monitor, Frame::ZERO, cx));
			true
		}
		ActionId::GoToEnd => {
			let length = engine.read(cx).sequence_length();
			engine.update(cx, |engine, cx| engine.request_frame(monitor, length, cx));
			true
		}
		ActionId::ShuttleLeft => {
			engine.update(cx, |engine, cx| engine.step(monitor, -1, cx));
			true
		}
		ActionId::ShuttleStop => {
			engine.update(cx, |engine, cx| engine.pause(monitor, cx));
			true
		}
		ActionId::ShuttleRight => {
			engine.update(cx, |engine, cx| engine.play(monitor, cx));
			true
		}
		ActionId::GoToIn => {
			if let Some((start, _)) = engine.read(cx).workarea() {
				engine.update(cx, |engine, cx| engine.request_frame(monitor, start, cx));
			}
			true
		}
		ActionId::GoToOut => {
			if let Some((_, end)) = engine.read(cx).workarea() {
				engine.update(cx, |engine, cx| engine.request_frame(monitor, end, cx));
			}
			true
		}
		ActionId::PlayInToOut => {
			// Seek to the work area's start, then play (the engine stops at
			// the sequence end; out-point stopping is a transport gap).
			if let Some((start, _)) = engine.read(cx).workarea() {
				engine.update(cx, |engine, cx| engine.request_frame(monitor, start, cx));
			}
			engine.update(cx, |engine, cx| engine.play(monitor, cx));
			true
		}
		_ => false,
	}
}
