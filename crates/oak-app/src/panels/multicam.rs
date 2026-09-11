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

//! The multicam panel (多机位): the C++ `MulticamWidget` + `MulticamDisplay`.
//!
//! The panel shows every angle of the detected multicam clip in a
//! rows×cols grid ([`MultiCamNode::rows_and_columns`], square-ish like the
//! C++), highlights the current source with a yellow box, and switches the
//! source on a grid click or the `1..9` / `⌘1..⌘9` hotkeys (route through
//! the action registry, see [`crate::actions::ActionId::MulticamSwitch1`]).
//!
//! Frame pipeline (M15 S2): the panel asks the engine for each angle's
//! frame through [`AppEngine::multicam_angle_frame`]; the engine renders
//! the source sequence's track at the playhead on a background thread and
//! caches it per (multicam node, source). The panel keeps its own
//! last-image map, so a cell keeps showing its last frame while the next
//! one renders:
//!
//! * resting playhead or right after a switch — every source is refreshed
//!   in one pass;
//! * during playback — the refresh is throttled round-robin (a couple of
//!   sources per tick), the rest show slightly-stale frames.
//!
//! Switch requests during playback are queued (the C++ `play_queue_`
//! semantics) and applied when the program playhead reaches the target
//! time. Every switch goes through `oak_timeline::multicam::multicam_switch`
//! on the global undo stack.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gpui::colors::{Colors, DefaultColors};
use gpui::dock::{DockPanel, PanelEvent};
use gpui::{
	canvas, div, img, prelude::*, px, AnyElement, App, Bounds, Context, Entity, EventEmitter,
	FocusHandle, MouseButton, ObjectFit, Pixels, Point, Rgba, Render, SharedString, Window,
};
use gpui_widgets::viewer::PlaybackClock;

use oak_node::nodes::multicamnode::MultiCamNode;

use crate::oakui::timecode::format_timecode;
use crate::oakui::{AppEngine, MulticamState};
use crate::panels::commands::PanelCommandHandler;
use crate::panels::ids::MULTICAM;
use crate::panels::chip;

/// The number of sources refreshed per tick during playback (the rest keep
/// their last frame until their turn — the task's round-robin throttle).
const PLAYBACK_REFRESH_PER_TICK: i32 = 2;
/// A refresh cycle runs once every this many ticks (a tick is one frame
/// at the sequence rate). Decoding one angle is an FFmpeg keyframe-
/// scanning seek (~10-50 ms even on a 720p proxy); re-requesting ALL
/// sources EVERY tick made those seek costs compound and starved the
/// main viewer's worker pool. The grid still refreshes each source in
/// turn, just at a third of the frame rate.
const PLAYBACK_REFRESH_STRIDE: i32 = 3;

/// One pending switch (the C++ `MulticamWidget`'s `play_queue_`): the
/// switch applies once the program playhead reaches `target`.
#[derive(Debug, Clone, Copy)]
struct QueuedSwitch {
	/// The source index to switch to.
	source: i32,
	/// Whether to split the clip at the playhead first.
	split_clip: bool,
	/// The playhead frame the switch was requested at.
	target: i64,
}

/// The multicam panel.
pub struct MulticamPanel<E: AppEngine> {
	engine: Entity<E>,
	/// The program monitor's clock (playhead + playing state).
	clock: Entity<E::Clock>,
	/// The grid container's window-space bounds, recorded on every layout
	/// by the invisible `canvas` child (click-to-cell conversion).
	grid_bounds: Bounds<Pixels>,
	/// The last resolved multicam state (compared by identity so a node or
	/// source-count change clears the stale frames).
	state: Option<MulticamState>,
	/// The last image per source (stale-OK during playback; the engine's
	/// cache holds the fresh frames).
	frames: HashMap<i32, Arc<gpui::RenderImage>>,
	/// Pending switches during playback ([`QueuedSwitch`]).
	play_queue: VecDeque<QueuedSwitch>,
	/// Round-robin cursor over the sources (playback refresh throttle).
	refresh_cursor: i32,
	/// Tick counter for the playback refresh cycle (stride-scaled).
	refresh_tick_toggle: u64,
	/// The refresh cycle seen last (dedups re-requests within a cycle).
	last_refresh_cycle: i64,
	/// The last program playhead (detects a jump / rest).
	last_playhead: i64,
	/// Set when a switch cleared the frames: the next refresh pass covers
	/// every source immediately.
	full_refresh: bool,
	/// The panel's own focus handle: clicking the panel focuses it and puts
	/// `MulticamPanel` on the key context path, which is what gates the
	/// `1..9` / `⌘1..⌘9` source-switch bindings (see
	/// [`crate::actions::key_bindings`]) — outside the panel those digit
	/// keys stay free for text entry.
	focus: FocusHandle,
}

impl<E: AppEngine> MulticamPanel<E> {
	/// Builds a panel over the program monitor's clock `clock`.
	pub fn new(
		engine: Entity<E>,
		clock: Entity<E::Clock>,
		_window: &mut Window,
		_cx: &mut Context<Self>,
	) -> Self {
		Self {
			engine,
			clock,
			grid_bounds: Bounds::default(),
			state: None,
			frames: HashMap::new(),
			play_queue: VecDeque::new(),
			refresh_cursor: 0,
			refresh_tick_toggle: 0,
			last_refresh_cycle: -1,
			last_playhead: i64::MIN,
			full_refresh: true,
			focus: _cx.focus_handle(),
		}
	}

	/// Re-reads the engine's detected multicam; a node / source-count change
	/// drops the stale angle frames.
	fn sync_state(&mut self, cx: &mut Context<Self>) {
		let state = self.engine.read(cx).multicam_state();
		let changed = self
			.state
			.map(|s| s.node_id)
			!= state.map(|s| s.node_id)
			|| self.state.map(|s| s.source_count) != state.map(|s| s.source_count);
		if changed {
			self.frames.clear();
			self.full_refresh = true;
			self.play_queue.clear();
		}
		self.state = state;
	}

	/// Requests the fresh frame for one source (the engine returns the
	/// cached frame for the current playhead, or schedules a background
	/// render and returns `None`).
	fn request_angle(&mut self, source: i32, cx: &mut Context<Self>) {
		if let Some(image) = self
			.engine
			.update(cx, |engine, cx| engine.multicam_angle_frame(source, cx))
		{
			self.frames.insert(source, image);
		}
	}

	/// Requests every angle; throttles to a round-robin subset during
	/// playback (the rest keep their last frame).
	fn refresh_frames(&mut self, cx: &mut Context<Self>) {
		let Some(state) = self.state else {
			return;
		};
		if state.source_count <= 0 {
			return;
		}
		let playhead = self.clock.read(cx).current_frame().0;
		let playing = self.clock.read(cx).is_playing();
		// `saturating_sub` guards the initial `i64::MIN` sentinel (a first
		// render counts as a jump → full refresh).
		let jumped = playhead.saturating_sub(self.last_playhead).abs() > 1;
		if self.full_refresh || !playing || jumped {
			// Resting playhead (or just after a switch / jump): refresh all.
			for source in 0..state.source_count {
				self.request_angle(source, cx);
			}
			self.refresh_cursor = 0;
		} else {
			// Playback: one refresh cycle per PLAYBACK_REFRESH_STRIDE
			// ticks, round-robin a couple of sources per cycle (the old
			// per-tick round-robin requested decodes faster than they can
			// complete — each is a keyframe-scanning seek and stole the
			// main viewer's worker capacity).
			let cycle = (self.refresh_tick_toggle / PLAYBACK_REFRESH_STRIDE as u64) as i64;
			if cycle != self.last_refresh_cycle {
				self.last_refresh_cycle = cycle;
				for _ in 0..state.source_count.min(PLAYBACK_REFRESH_PER_TICK) {
					let source = self.refresh_cursor % state.source_count;
					self.refresh_cursor += 1;
					self.request_angle(source, cx);
				}
			}
			self.refresh_tick_toggle = self.refresh_tick_toggle.wrapping_add(1);
		}
		self.full_refresh = false;
		self.last_playhead = playhead;
	}

	/// Applies queued switches whose target time the playhead reached (and
	/// flushes the queue when playback stopped).
	fn process_play_queue(&mut self, cx: &mut Context<Self>) {
		let playing = self.clock.read(cx).is_playing();
		let playhead = self.clock.read(cx).current_frame().0;
		while let Some(front) = self.play_queue.front().copied() {
			if !playing || front.target <= playhead {
				self.play_queue.pop_front();
				self.apply_switch(front.source, front.split_clip, cx);
			} else {
				break;
			}
		}
	}

	/// Switches the multicam source through the engine (one undo entry).
	/// During playback the switch is deferred to the playhead reaching the
	/// request time — the C++ `play_queue_` semantics.
	fn request_switch(&mut self, source: i32, split_clip: bool, cx: &mut Context<Self>) {
		if self.state.is_none() {
			return;
		}
		let playing = self.clock.read(cx).is_playing();
		if playing {
			let target = self.clock.read(cx).current_frame().0;
			self.play_queue.push_back(QueuedSwitch {
				source,
				split_clip,
				target,
			});
		} else {
			self.apply_switch(source, split_clip, cx);
		}
	}

	/// Applies a switch immediately and schedules the full angle refresh.
	fn apply_switch(&mut self, source: i32, split_clip: bool, cx: &mut Context<Self>) {
		self.engine
			.update(cx, |engine, cx| engine.multicam_switch_to(source, split_clip, cx));
		self.frames.clear();
		self.full_refresh = true;
	}

	/// Converts a click inside the grid container to a source index and
	/// switches to it (the C++ `display_clicked`).
	fn handle_grid_click(&mut self, window_position: Point<Pixels>, cx: &mut Context<Self>) {
		let Some(state) = self.state else {
			return;
		};
		if state.source_count <= 0 {
			return;
		}
		let local = window_position - self.grid_bounds.origin;
		if local.x < px(0.0)
			|| local.y < px(0.0)
			|| local.x > self.grid_bounds.size.width
			|| local.y > self.grid_bounds.size.height
		{
			return;
		}
		let (rows, cols) = MultiCamNode::rows_and_columns(state.source_count);
		let multi = rows.max(cols).max(1);
		let cell_w = f32::from(self.grid_bounds.size.width) / multi as f32;
		let cell_h = f32::from(self.grid_bounds.size.height) / multi as f32;
		if cell_w <= 0.0 || cell_h <= 0.0 {
			return;
		}
		let c = (f32::from(local.x) / cell_w).floor() as i32;
		let r = (f32::from(local.y) / cell_h).floor() as i32;
		if c >= cols || r >= rows {
			return;
		}
		let source = MultiCamNode::rows_cols_to_index(r, c, rows, cols);
		if (0..state.source_count).contains(&source) {
			self.request_switch(source, true, cx);
		}
	}

	/// One grid cell: the angle frame (or a placeholder) with a yellow box
	/// around the current source.
	fn cell(&mut self, source: i32, cell_w: f32, cell_h: f32, colors: &Colors) -> AnyElement {
		let is_current = self.state.is_some_and(|s| s.current_source == source);
		let content: AnyElement = match self.frames.get(&source) {
			Some(image) => img(image.clone())
				.size_full()
				.object_fit(ObjectFit::Contain)
				.into_any_element(),
			None => div()
				.size_full()
				.flex()
				.items_center()
				.justify_center()
				.text_xs()
				.text_color(colors.disabled)
				.child(format!("CAM {}", source + 1))
				.into_any_element(),
		};
		div()
			.w(px(cell_w))
			.h(px(cell_h))
			.border_2()
			.border_color(if is_current {
				// The C++ MulticamDisplay's yellow current-source box.
				Rgba {
					r: 1.0,
					g: 0.95,
					b: 0.1,
					a: 1.0,
				}
			} else {
				colors.border
			})
			.bg(colors.container)
			.overflow_hidden()
			.child(content)
			.into_any_element()
	}

	/// The angle grid: rows×cols cells filling the container.
	fn grid(&mut self, state: MulticamState, colors: &Colors) -> AnyElement {
		let (rows, cols) = MultiCamNode::rows_and_columns(state.source_count);
		let multi = rows.max(cols).max(1);
		let cell_w = f32::from(self.grid_bounds.size.width) / multi as f32;
		let cell_h = f32::from(self.grid_bounds.size.height) / multi as f32;
		let cell_w = cell_w.max(1.0);
		let cell_h = cell_h.max(1.0);
		let cells: Vec<AnyElement> =
			(0..state.source_count).map(|s| self.cell(s, cell_w, cell_h, colors)).collect();
		div()
			.size_full()
			.flex()
			.flex_wrap()
			.bg(colors.background)
			.children(cells)
			.into_any_element()
	}
}

impl<E: AppEngine> Render for MulticamPanel<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		self.sync_state(cx);
		self.process_play_queue(cx);
		self.refresh_frames(cx);

		let colors = cx.default_colors().clone();
		let rate = self.clock.read(cx).frame_rate();
		let playhead = self.clock.read(cx).current_frame();
		let timecode = format_timecode(playhead, rate);
		let this = cx.weak_entity();

		let body: AnyElement = match self.state {
			Some(state) => self.grid(state, &colors),
			None => div()
				.size_full()
				.flex()
				.items_center()
				.justify_center()
				.text_color(colors.disabled)
				.child(crate::i18n::tr("multicam.no_multicam"))
				.into_any_element(),
		};

		div()
			.size_full()
			.flex()
			.flex_col()
			.overflow_hidden()
			// The panel owns the key context that gates the `1..9` /
			// `⌘1..⌘9` source-switch bindings: focusing the panel (click)
			// makes the keys switch angles, anywhere else they type digits.
			.key_context("MulticamPanel")
			.track_focus(&self.focus)
			// Any click inside the panel makes it the focused panel (the
			// dock re-emits this as `DockEvent::PanelFocused`, which the
			// shell uses to route the focused-panel hotkeys).
			.on_mouse_down(MouseButton::Left, {
				cx.listener(|this, _event: &gpui::MouseDownEvent, window, cx| {
					window.focus(&this.focus, cx);
					cx.emit(PanelEvent::Focused);
				})
			})
			.child(
				div()
					.flex()
					.items_center()
					.gap_2()
					.px_2()
					.py_1()
					.border_b_1()
					.border_color(colors.border)
					.child(chip(&colors, crate::i18n::tr("panel.multicam")))
					.child(chip(&colors, timecode.clone())),
			)
			.child(
				div()
					.flex_1()
					.min_h_0()
					.min_w_0()
					.relative()
					// The grid click handler converts the window-space click
					// through the recorded grid origin (see the canvas below).
					.on_mouse_down(MouseButton::Left, {
						cx.listener(|this, event: &gpui::MouseDownEvent, _window, cx| {
							this.handle_grid_click(event.position, cx);
						})
					})
					.child(body)
					.child(
						// Records the grid's window-space origin on every
						// layout (the click hit-test converts window
						// positions through it); paints nothing.
						canvas(
							move |bounds, _window, cx| {
								if let Some(this) = this.upgrade() {
									this.update(cx, |this, _cx| this.grid_bounds = bounds);
								}
							},
							|_bounds, (), _window, _cx| {},
						)
						.absolute()
						.size_full(),
					),
			)
			// A simplified time ruler strip (the C++ TimeRuler; this port
			// shows the program playhead's timecode and a marker line).
			.child(
				div()
					.h(px(20.0))
					.flex()
					.items_center()
					.px_2()
					.border_t_1()
					.border_color(colors.border)
					.bg(colors.container)
					.text_xs()
					.text_color(colors.disabled)
					.child(crate::i18n::tr("panel.multicam"))
					.child(" · ")
					.child(timecode),
			)
	}
}

impl<E: AppEngine> PanelCommandHandler for MulticamPanel<E> {
	fn multicam_switch(&mut self, source: i32, split_clip: bool, cx: &mut Context<Self>) -> bool {
		self.request_switch(source, split_clip, cx);
		true
	}
}

impl<E: AppEngine> EventEmitter<PanelEvent> for MulticamPanel<E> {}

impl<E: AppEngine> DockPanel for MulticamPanel<E> {
	fn panel_id(&self) -> gpui::dock::PanelId {
		MULTICAM
	}

	fn title(&self, _cx: &App) -> SharedString {
		crate::i18n::tr("panel.multicam").into()
	}

	fn tab_content(&self, _cx: &App) -> AnyElement {
		div().child(crate::i18n::tr("panel.multicam")).into_any_element()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::actions::MulticamSwitch5;
	use gpui::{size, Focusable, TestAppContext, VisualTestContext};
	use std::cell::Cell;
	use std::rc::Rc;

	/// The grid conversion helpers used by the panel (rows/cols from the
	/// node, index round-trips) plus the panel's click math.
	fn rows_cols(sources: i32) -> (i32, i32) {
		MultiCamNode::rows_and_columns(sources)
	}

	/// Click-point → source index with the same math [`MulticamPanel`]
	/// applies: cell = grid/multi, source = rows_cols_to_index(r, c).
	fn click_to_source(
		grid: (f32, f32),
		click: (f32, f32),
		sources: i32,
	) -> Option<i32> {
		if click.0 < 0.0 || click.1 < 0.0 || click.0 >= grid.0 || click.1 >= grid.1 {
			return None;
		}
		let (rows, cols) = rows_cols(sources);
		let multi = rows.max(cols).max(1);
		let cell_w = grid.0 / multi as f32;
		let cell_h = grid.1 / multi as f32;
		let c = (click.0 / cell_w).floor() as i32;
		let r = (click.1 / cell_h).floor() as i32;
		if c >= cols || r >= rows {
			return None;
		}
		let source = MultiCamNode::rows_cols_to_index(r, c, rows, cols);
		(0..sources).contains(&source).then_some(source)
	}

	/// The C++ grid is "as square as possible"; a click lands on the cell
	/// it geometrically falls in.
	#[test]
	fn click_to_cell_maps_sources() {
		// 4 sources → 2×2.
		assert_eq!(click_to_source((400.0, 200.0), (100.0, 50.0), 4), Some(0));
		assert_eq!(click_to_source((400.0, 200.0), (300.0, 50.0), 4), Some(1));
		assert_eq!(click_to_source((400.0, 200.0), (100.0, 150.0), 4), Some(2));
		assert_eq!(click_to_source((400.0, 200.0), (300.0, 150.0), 4), Some(3));
		// 3 sources → 2×2 with the 4th cell empty.
		assert_eq!(click_to_source((400.0, 200.0), (300.0, 150.0), 3), None);
		// Out of bounds.
		assert_eq!(click_to_source((400.0, 200.0), (400.0, 100.0), 4), None);
		assert_eq!(click_to_source((400.0, 200.0), (100.0, 200.0), 4), None);
		// 5 sources → 2×3 (2 rows × 3 cols, 6 cells, 5 used): source 4 sits
		// at row 1, col 1.
		assert_eq!(click_to_source((400.0, 300.0), (150.0, 150.0), 5), Some(4));
	}

	/// The panel renders the demo grid without crashing and fills the
	/// angle-frame map from the mock engine's synthetic frames.
	#[gpui::test]
	async fn panel_renders_the_mock_grid(cx: &mut TestAppContext) {
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(640.0), px(360.0)), |window, cx| {
			let engine = cx.new(|cx| crate::oakui::MockEngine::demo(cx));
			let clock = engine.read(cx).program_clock().clone();
			MulticamPanel::new(engine, clock, window, cx)
		});
		cx.run_until_parked();
		let panel = window.root(cx).expect("multicam panel root");
		let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
		cx.update(|window, cx| {
			window.draw(cx).clear();
		});
		cx.run_until_parked();

		// The mock reports a demo multicam with several sources; the panel's
		// frame cache is populated (synthetic colored cells).
		let (state, frame_count) = cx.read(|app| {
			let panel = panel.read(app);
			(panel.state, panel.frames.len())
		});
		let state = state.expect("the mock reports a demo multicam");
		assert!(state.source_count >= 1, "demo sources: {}", state.source_count);
		assert_eq!(frame_count, state.source_count as usize, "every angle frame is cached");
	}

	/// A minimal window root that mirrors the multicam panel's key-context
	/// wiring: a `MulticamPanel`-scoped interactive region next to a text
	/// field, with the source-switch action bound the way the shell binds
	/// it (through the app keymap).
	struct KeyContextHarness {
		panel_focus: FocusHandle,
		switched: Rc<Cell<bool>>,
		editable: Entity<gpui_elements::editable_text::EditableTextState>,
	}

	impl KeyContextHarness {
		fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
			let _ = window;
			let editable = cx.new(|cx| {
				gpui_elements::editable_text::EditableTextState::new(
					gpui_elements::editable_text::StringStorage::from("0"),
					cx,
				)
			});
			Self {
				panel_focus: cx.focus_handle(),
				switched: Rc::new(Cell::new(false)),
				editable,
			}
		}
	}

	impl EventEmitter<()> for KeyContextHarness {}

	impl Render for KeyContextHarness {
		fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
			let switched = self.switched.clone();
			div()
				.size_full()
				.flex()
				.gap_4()
				.child(
					div()
						.key_context("MulticamPanel")
						.track_focus(&self.panel_focus)
						.w_1_2()
						.bg(gpui::rgb(0x202020))
						.on_action(
							move |_: &MulticamSwitch5, _: &mut Window, _: &mut App| {
								switched.set(true);
							},
						),
				)
				.child(
					crate::oakui::component::text_input("value-box", cx)
						.state(self.editable.downgrade()),
				)
		}
	}

	/// The digit keys must type into a focused text field and only switch
	/// sources while the multicam panel itself is focused. (Regression:
	/// the `1..9` / `⌘1..⌘9` switch bindings were global, so a digit in
	/// any value box / hex field / text input dispatched the switch action
	/// instead of reaching the input handler — every field could only ever
	/// type `0`.)
	#[gpui::test]
	async fn digit_keys_type_in_text_fields_but_switch_inside_the_panel(
		cx: &mut TestAppContext,
	) {
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(640.0), px(360.0)), |window, cx| {
			cx.bind_keys(crate::actions::key_bindings());
			KeyContextHarness::new(window, cx)
		});
		cx.run_until_parked();
		let harness = window.root(cx).expect("key-context harness root");
		let cx = VisualTestContext::from_window(window.into(), cx).into_mut();

		// Focus the text field: `5` must type into it — no binding matches
		// on a focus path that lacks the `MulticamPanel` context. (The
		// field's caret starts at position 0, so the digit lands at the
		// front of the initial "0".)
		let field_focus = cx.read(|app| harness.read(app).editable.focus_handle(app));
		cx.update(|window, app| window.focus(&field_focus, app));
		cx.run_until_parked();
		cx.simulate_keystrokes("5");
		let text = cx.read(|app| harness.read(app).editable.read(app).as_str().to_string());
		assert_eq!(text, "50", "a digit reaches the focused text field");

		// Focus the panel: the same key now dispatches the switch action
		// and never reaches the (still registered) input handler.
		let panel_focus = cx.read(|app| harness.read(app).panel_focus.clone());
		cx.update(|window, app| window.focus(&panel_focus, app));
		cx.run_until_parked();
		cx.simulate_keystrokes("5");
		let (switched, text) = cx.read(|app| {
			let harness = harness.read(app);
			(harness.switched.get(), harness.editable.read(app).as_str().to_string())
		});
		assert!(
			switched,
			"the panel-scoped binding fires while the panel is focused"
		);
		assert_eq!(
			text, "50",
			"the same key no longer types into the field"
		);
	}
}
