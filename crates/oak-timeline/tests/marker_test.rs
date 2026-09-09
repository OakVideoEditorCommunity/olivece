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

//! Contract tests for the marker domain (`src/marker.rs`):
//! `TimelineMarker` and `TimelineMarkerList` value types plus the five
//! marker undo commands. The list stays sorted by time (C++
//! `TimelineMarkerList` re-sorts on mutation), which `resort` enforces
//! after out-of-band edits. The XML load/save contract left with the
//! deleted C ABI export layer (single-lib unification).

use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::{Rational, TimeRange};
use oak_timeline::common::EditToInfo;
use oak_timeline::handle::{get, make_owned, CHandle};
use oak_timeline::marker::{
	MarkerAddCommand, MarkerChangeColorCommand, MarkerChangeNameCommand, MarkerChangeTimeCommand,
	MarkerRemoveCommand, TimelineMarker, TimelineMarkerList,
};
use oak_timeline::undocommon::Command;

/// Lock the shared marker list behind a live handle. Every marker-list
/// handle in these tests comes from `make_owned`, which boxes an
/// `Arc<Mutex<TimelineMarkerList>>`.
fn list_of(h: &CHandle) -> MutexGuard<'_, TimelineMarkerList> {
	// SAFETY: as above; the handle is live.
	unsafe { get::<Arc<Mutex<TimelineMarkerList>>>(h) }
		.expect("live marker-list handle")
		.lock()
		.unwrap_or_else(|e| e.into_inner())
}

/// A default `TimelineMarker` starts at the null time with no name.
#[test]
fn marker_default_is_null_time() {
	let m = TimelineMarker::new();
	assert_eq!(m.color(), 0);
	assert_eq!(m.time().in_(), Rational::new(0, 1));
	assert_eq!(m.time().out(), Rational::new(0, 1));
	assert_eq!(m.name(), "");
}

/// `TimelineMarker::with_time` stores color, time range and name as
/// given and exposes them through the getters.
#[test]
fn marker_with_time_stores_fields() {
	let m = TimelineMarker::with_time(
		3,
		TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
		"hello",
	);
	assert_eq!(m.color(), 3);
	assert_eq!(m.time().in_(), Rational::new(10, 1));
	assert_eq!(m.time().out(), Rational::new(20, 1));
	assert_eq!(m.name(), "hello");
}

/// `set_time_point` moves a range marker to a single point, preserving
/// its length (and hence name and color).
#[test]
fn marker_set_time_point_preserves_name_color() {
	let mut m = TimelineMarker::with_time(
		5,
		TimeRange::new(Rational::new(10, 1), Rational::new(20, 1)),
		"tag",
	);
	let t = Rational::new(30, 1);
	m.set_time_point(t);
	// Color and name survive the move.
	assert_eq!(m.color(), 5);
	assert_eq!(m.name(), "tag");
	// The original length (10) is preserved.
	assert_eq!(m.time().in_(), t);
	assert_eq!(m.time().out(), Rational::new(40, 1));
}

/// Sibling detection goes through the owning list: two markers sharing
/// an in point are both found by `get_marker_at_time` (the per-marker
/// `has_sibling_at_time` is a documented De-Qt simplification with no
/// parent pointer).
#[test]
fn marker_sibling_detection() {
	let mut list = TimelineMarkerList::new();
	let mut a = TimelineMarker::new();
	a.set_time_point(Rational::new(42, 1));
	a.set_name("a");
	let mut b = TimelineMarker::new();
	b.set_time_point(Rational::new(42, 1));
	b.set_name("b");
	let mut other = TimelineMarker::new();
	other.set_time_point(Rational::new(7, 1));
	list.add_marker(a);
	list.add_marker(b);
	list.add_marker(other);
	assert_eq!(list.get_marker_at_time(Rational::new(42, 1)).map(|m| m.name()), Some("a"));
	assert!(list.get_marker_at_time(Rational::new(7, 1)).is_some());
	assert!(list.get_marker_at_time(Rational::new(8, 1)).is_none());
}

/// The list starts empty and grows with each `add_marker`, preserving
/// sorted order (by in point) for markers at distinct times.
#[test]
fn marker_list_grows_with_adds() {
	let mut list = TimelineMarkerList::new();
	assert!(list.empty());
	assert_eq!(list.size(), 0);

	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(10, 1), Rational::new(11, 1)),
		"a",
	));
	list.add_marker(TimelineMarker::with_time(
		2,
		TimeRange::new(Rational::new(5, 1), Rational::new(6, 1)),
		"b",
	));

	assert_eq!(list.size(), 2);
	assert!(!list.empty());
	// Sorted by in point: b (5) before a (10).
	assert_eq!(list.front().unwrap().name(), "b");
	assert_eq!(list.back().unwrap().name(), "a");
}

/// `at`/`back`/`front` expose the stored markers; `at` returns `None`
/// for an out-of-range index.
#[test]
fn marker_list_indexing() {
	let mut list = TimelineMarkerList::new();
	assert!(list.at(0).is_none());
	assert!(list.front().is_none());
	assert!(list.back().is_none());

	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"one",
	));
	assert!(list.at(0).is_some());
	assert_eq!(list.at(0).unwrap().name(), "one");
	assert!(list.at(1).is_none());
	assert!(list.front().is_some());
	assert!(list.back().is_some());
}

/// `remove_marker` removes exactly the given marker and returns it;
/// removing a marker that is not present is a no-op returning `None`.
#[test]
fn marker_list_remove() {
	let mut list = TimelineMarkerList::new();
	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"one",
	));

	// `remove_marker` takes a reference into the list; drop the borrow by
	// going through a raw pointer first (as `marker.rs` does internally).
	let m = list.at(0).unwrap();
	let mptr = m as *const TimelineMarker;
	let removed = list.remove_marker(unsafe { &*mptr });
	assert!(removed.is_some());
	assert_eq!(removed.unwrap().name(), "one");
	assert_eq!(list.size(), 0);

	// Removing a marker not present returns None.
	let stray = TimelineMarker::new();
	assert!(list.remove_marker(&stray).is_none());
	assert_eq!(list.size(), 0);
}

/// `get_marker_at_time` finds the marker at an exact time; adding a
/// marker and then asking at the same time finds it.
#[test]
fn marker_list_find_at_time() {
	let mut list = TimelineMarkerList::new();
	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(7, 1), Rational::new(8, 1)),
		"x",
	));

	let found = list.get_marker_at_time(Rational::new(7, 1));
	assert!(found.is_some());
	assert_eq!(found.unwrap().name(), "x");
	assert!(list.get_marker_at_time(Rational::new(9, 1)).is_none());
}

/// `get_closest_marker_to_time` finds the nearest marker; an exact match
/// at the first marker early-exits once the distance starts increasing,
/// and queries before/after all markers exercise both signs of the
/// absolute-difference comparison.
#[test]
fn marker_list_closest_to_time() {
	let mut list = TimelineMarkerList::new();
	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(5, 1), Rational::new(6, 1)),
		"a",
	));
	list.add_marker(TimelineMarker::with_time(
		2,
		TimeRange::new(Rational::new(10, 1), Rational::new(11, 1)),
		"b",
	));
	list.add_marker(TimelineMarker::with_time(
		3,
		TimeRange::new(Rational::new(20, 1), Rational::new(21, 1)),
		"c",
	));

	// Empty list -> None.
	assert!(TimelineMarkerList::new()
		.get_closest_marker_to_time(Rational::new(0, 1))
		.is_none());
	// Exact match at the first marker (early-exit on the next).
	assert_eq!(
		list.get_closest_marker_to_time(Rational::new(5, 1))
			.unwrap()
			.name(),
		"a"
	);
	// Query after all markers (negative differences).
	assert_eq!(
		list.get_closest_marker_to_time(Rational::new(100, 1))
			.unwrap()
			.name(),
		"c"
	);
	// Query before all markers (positive differences).
	assert_eq!(
		list.get_closest_marker_to_time(Rational::new(1, 1))
			.unwrap()
			.name(),
		"a"
	);
}

/// A command's undo before any redo is a no-op.
#[test]
fn marker_add_command_undo_before_redo() {
	let list_h = make_owned(TimelineMarkerList::new());
	let mut cmd = MarkerAddCommand::new(
		list_h.clone(),
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
		0,
	);
	cmd.undo();
	assert_eq!(list_of(&list_h).size(), 0);
}

/// `MarkerRemoveCommand` redo is idempotent (a second redo is a no-op).
#[test]
fn marker_remove_command_double_redo() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));
	let mut cmd = MarkerRemoveCommand::new(list_h.clone(), 0);
	cmd.redo();
	cmd.redo();
	assert_eq!(list_of(&list_h).size(), 0);
}

/// Every marker command dispatches through the `Command` trait, which is
/// how the undo stack invokes them.
#[test]
fn marker_commands_trait_dispatch() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let mut add = MarkerAddCommand::new(
		list_h.clone(),
		TimeRange::new(Rational::new(9, 1), Rational::new(10, 1)),
		"n",
		0,
	);
	Command::redo(&mut add);
	assert_eq!(list_of(&list_h).size(), 2);
	Command::undo(&mut add);
	assert_eq!(list_of(&list_h).size(), 1);

	let mut remove = MarkerRemoveCommand::new(list_h.clone(), 0);
	Command::redo(&mut remove);
	assert_eq!(list_of(&list_h).size(), 0);
	Command::undo(&mut remove);
	assert_eq!(list_of(&list_h).size(), 1);

	let mut color = MarkerChangeColorCommand::new(list_h.clone(), 0, 7);
	Command::redo(&mut color);
	assert_eq!(list_of(&list_h).at(0).unwrap().color(), 7);
	Command::undo(&mut color);
	assert_eq!(list_of(&list_h).at(0).unwrap().color(), 1);

	let mut name = MarkerChangeNameCommand::new(list_h.clone(), 0, "z");
	Command::redo(&mut name);
	assert_eq!(list_of(&list_h).at(0).unwrap().name(), "z");
	Command::undo(&mut name);
	assert_eq!(list_of(&list_h).at(0).unwrap().name(), "m");

	let mut time = MarkerChangeTimeCommand::new(
		list_h.clone(),
		0,
		TimeRange::new(Rational::new(50, 1), Rational::new(51, 1)),
	);
	Command::redo(&mut time);
	assert_eq!(
		list_of(&list_h).at(0).unwrap().time().in_(),
		Rational::new(50, 1)
	);
	Command::undo(&mut time);
	assert_eq!(
		list_of(&list_h).at(0).unwrap().time().in_(),
		Rational::new(1, 1)
	);
}

/// Re-establishing time order after an out-of-band time change: the
/// list exposes markers immutably, so the edit is expressed as
/// detach + mutate a copy + re-insert, which re-sorts exactly as
/// `resort`/`MarkerChangeTimeCommand` do.
#[test]
fn marker_list_resort_after_time_change() {
	let mut list = TimelineMarkerList::new();
	list.add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(10, 1), Rational::new(11, 1)),
		"a",
	));
	list.add_marker(TimelineMarker::with_time(
		2,
		TimeRange::new(Rational::new(5, 1), Rational::new(6, 1)),
		"b",
	));
	// Order: b (5), a (10).
	assert_eq!(list.at(0).unwrap().name(), "b");

	// Detach `a`, move it to time 3, re-insert.
	let mptr = list.at(1).unwrap() as *const TimelineMarker;
	let a = list.remove_marker(unsafe { &*mptr }).unwrap();
	let mut moved = a;
	moved.set_time(TimeRange::new(Rational::new(3, 1), Rational::new(4, 1)));
	list.add_marker(moved);

	assert_eq!(list.size(), 2);
	assert_eq!(list.front().unwrap().name(), "a");
	assert_eq!(list.back().unwrap().name(), "b");
}

/// A `MarkerAddCommand` redo appends the marker; undo removes it
/// again, leaving the list as it was.
#[test]
fn marker_add_command_redo_undo() {
	let list_h = make_owned(TimelineMarkerList::new());
	let mut cmd = MarkerAddCommand::new(
		list_h.clone(),
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
		4,
	);

	cmd.redo();
	let list = list_of(&list_h);
	assert_eq!(list.size(), 1);
	assert_eq!(list.at(0).unwrap().name(), "m");
	assert_eq!(list.at(0).unwrap().color(), 4);
	drop(list);

	// Redo is idempotent.
	cmd.redo();
	assert_eq!(list_of(&list_h).size(), 1);

	cmd.undo();
	assert_eq!(list_of(&list_h).size(), 0);
}

/// A `MarkerRemoveCommand` redo drops the marker; undo re-inserts it
/// at its time position.
#[test]
fn marker_remove_command_redo_undo() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let mut cmd = MarkerRemoveCommand::new(list_h.clone(), 0);
	cmd.redo();
	assert_eq!(list_of(&list_h).size(), 0);

	cmd.undo();
	let list = list_of(&list_h);
	assert_eq!(list.size(), 1);
	assert_eq!(list.at(0).unwrap().name(), "m");
}

/// `MarkerChangeColorCommand` redo changes the color and undo restores
/// the original.
#[test]
fn marker_change_color_command_redo_undo() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let mut cmd = MarkerChangeColorCommand::new(list_h.clone(), 0, 9);
	cmd.redo();
	assert_eq!(list_of(&list_h).at(0).unwrap().color(), 9);
	cmd.undo();
	assert_eq!(list_of(&list_h).at(0).unwrap().color(), 1);
}

/// `MarkerChangeNameCommand` redo changes the name and undo restores
/// the original.
#[test]
fn marker_change_name_command_redo_undo() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let mut cmd = MarkerChangeNameCommand::new(list_h.clone(), 0, "renamed");
	cmd.redo();
	assert_eq!(list_of(&list_h).at(0).unwrap().name(), "renamed");
	cmd.undo();
	assert_eq!(list_of(&list_h).at(0).unwrap().name(), "m");
}

/// `MarkerChangeTimeCommand` redo moves the marker and undo restores
/// the original time.
#[test]
fn marker_change_time_command_redo_undo() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let new_t = TimeRange::new(Rational::new(50, 1), Rational::new(51, 1));
	let mut cmd = MarkerChangeTimeCommand::new(list_h.clone(), 0, new_t);
	cmd.redo();
	assert_eq!(
		list_of(&list_h).at(0).unwrap().time().in_(),
		Rational::new(50, 1)
	);
	cmd.undo();
	assert_eq!(
		list_of(&list_h).at(0).unwrap().time().in_(),
		Rational::new(1, 1)
	);
}

/// `to_command` boxes a marker command into an oakundo `UndoCommand`
/// value for the undo stack (the old C ABI command handle is gone with
/// the single-lib unification); `redo_now`/`undo_now` drive the same
/// redo/undo bodies the stack would.
#[test]
fn marker_commands_box_to_undo_command() {
	let list_h = make_owned(TimelineMarkerList::new());
	list_of(&list_h).add_marker(TimelineMarker::with_time(
		1,
		TimeRange::new(Rational::new(1, 1), Rational::new(2, 1)),
		"m",
	));

	let mut cmd = MarkerAddCommand::new(
		list_h.clone(),
		TimeRange::new(Rational::new(5, 1), Rational::new(6, 1)),
		"n",
		0,
	)
	.to_command();
	cmd.redo_now();
	assert_eq!(list_of(&list_h).size(), 2);
	cmd.undo_now();
	assert_eq!(list_of(&list_h).size(), 1);
}

