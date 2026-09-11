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

//! Safe-layer behavior matrix for `UndoCommand` / `UndoStack`
//! (mirrors `src/undo/src/undostack.cpp` / `undocommand.cpp`).

use std::sync::{Arc, Mutex};

use oak_undo::error::Error;
use oak_undo::undocommand::UndoCommand;
use oak_undo::undostack::{EmptyCommand, UndoStack, K_MAX_UNDO_COMMANDS};

/// Shared event recorder; commands record `redo:<name>` / `undo:<name>`
/// through their closures.
type Trace = Arc<Mutex<Vec<String>>>;

fn new_trace() -> Trace {
	Arc::new(Mutex::new(Vec::new()))
}

/// A closure-backed command whose redo/undo record into `trace`.
fn trace_cmd(name: &'static str, trace: &Trace) -> UndoCommand {
	let (t_redo, t_undo) = (trace.clone(), trace.clone());
	let (name_redo, name_undo) = (name.to_string(), name.to_string());
	UndoCommand::from_closures(
		move || t_redo.lock().unwrap().push(format!("redo:{name_redo}")),
		move || t_undo.lock().unwrap().push(format!("undo:{name_undo}")),
	)
}

fn events(trace: &Trace) -> Vec<String> {
	trace.lock().unwrap().clone()
}

/// A `Drop` guard that records `free:<name>` exactly once — the new-API
/// analogue of the C-ABI `free_fn` callback (dropping a command drops its
/// boxed closures, which drop their captures).
struct FreeProbe {
	name: &'static str,
	trace: Trace,
}

impl Drop for FreeProbe {
	fn drop(&mut self) {
		self.trace.lock().unwrap().push(format!("free:{}", self.name));
	}
}

#[test]
fn command_redo_undo_lifecycle() {
	let trace = new_trace();
	let mut cmd = trace_cmd("a", &trace);

	assert!(!cmd.is_done());
	assert!(cmd.has_prepared());
	assert_eq!(events(&trace), Vec::<String>::new());

	cmd.redo_now();
	assert!(cmd.is_done());
	assert_eq!(events(&trace), vec!["redo:a"]);

	// Idempotent: a second redo does nothing.
	cmd.redo_now();
	assert_eq!(events(&trace), vec!["redo:a"]);

	cmd.undo_now();
	assert!(!cmd.is_done());
	assert_eq!(events(&trace), vec!["redo:a", "undo:a"]);

	// Idempotent undo.
	cmd.undo_now();
	assert_eq!(events(&trace), vec!["redo:a", "undo:a"]);

	// redo_and_set_modified / undo_and_set_modified alias the *_now forms.
	cmd.redo_and_set_modified();
	cmd.undo_and_set_modified();
	assert_eq!(events(&trace), vec!["redo:a", "undo:a", "redo:a", "undo:a"]);

	// set_done marks executed without running anything.
	let mut cmd2 = trace_cmd("x", &trace);
	cmd2.set_done(true);
	assert!(cmd2.is_done());
	cmd2.redo_now(); // no-op (already done)
	assert_eq!(events(&trace), vec!["redo:a", "undo:a", "redo:a", "undo:a"]);
}

/// Dropping a command runs its destruction exactly once (the C-ABI
/// `free_fn` exactly-once contract, now via `Drop`).
#[test]
fn command_drop_frees_exactly_once() {
	let trace = new_trace();
	let probe = FreeProbe {
		name: "a",
		trace: trace.clone(),
	};
	let cmd = UndoCommand::from_closures(move || {}, move || {
		// `probe` must be captured by value so that dropping the command
		// frees it; dropping a reference (the previous body) did nothing.
		let _ = &probe;
	});

	drop(cmd);
	assert_eq!(events(&trace), vec!["free:a"]);

	// Re-dropping an already-dropped value is impossible by construction;
	// a second drop of a clone of the event log stays empty.
	assert_eq!(events(&trace), vec!["free:a"]);
}

#[test]
fn empty_command_is_noop() {
	let mut cmd = EmptyCommand::new();
	assert!(cmd.has_prepared());
	cmd.redo_now();
	assert!(cmd.is_done());
	cmd.undo_now();
	assert!(!cmd.is_done());
	// No panics, no side effects.
}

#[test]
fn multi_redo_undo_ordering() {
	let trace = new_trace();
	let mut multi = UndoCommand::multi();
	multi.multi_add_child(trace_cmd("a", &trace));
	multi.multi_add_child(trace_cmd("b", &trace));
	multi.multi_add_child(trace_cmd("c", &trace));

	assert_eq!(multi.multi_child_count(), 3);
	// Children are reachable and named.
	assert_eq!(multi.multi_child(0).unwrap().is_done(), false);
	assert_eq!(multi.multi_child(2).unwrap().is_done(), false);
	assert!(matches!(multi.multi_child(3), Err(Error::NotFound)));

	// Redo in insertion order; undo in reverse.
	multi.redo_now();
	assert!(multi.is_done());
	assert_eq!(events(&trace), vec!["redo:a", "redo:b", "redo:c"]);

	multi.undo_now();
	assert_eq!(
		events(&trace),
		vec!["redo:a", "redo:b", "redo:c", "undo:c", "undo:b", "undo:a"]
	);
}

#[test]
#[should_panic]
fn multi_add_child_on_plain_command_panics() {
	let trace = new_trace();
	let mut cmd = trace_cmd("a", &trace);
	cmd.multi_add_child(trace_cmd("b", &trace));
}

#[test]
fn multi_child_helpers_on_plain_command() {
	let trace = new_trace();
	let mut cmd = trace_cmd("a", &trace);
	assert_eq!(cmd.multi_child_count(), 0);
	assert!(matches!(cmd.multi_child(0), Err(Error::Invalid)));
	assert!(matches!(cmd.multi_child_mut(0), Err(Error::Invalid)));
}

#[test]
fn stack_new_has_empty_bottom() {
	let s = UndoStack::new();
	assert_eq!(s.command_count(), 1);
	assert_eq!(s.done_count(), 1);
	assert!(!s.can_undo());
	assert!(!s.can_redo());
	assert_eq!(s.command_name(0).unwrap(), "New/Open Project");
	assert_eq!(s.command_is_done(0).unwrap(), true);
}

#[test]
fn stack_push_undo_redo_branch() {
	let trace = new_trace();
	let mut s = UndoStack::new();

	s.push(trace_cmd("a", &trace), "A");
	s.push(trace_cmd("b", &trace), "B");
	assert_eq!(s.command_count(), 3);
	assert!(s.can_undo());
	assert!(!s.can_redo());
	assert_eq!(events(&trace), vec!["redo:a", "redo:b"]);

	// Undo the top.
	s.undo().unwrap();
	assert_eq!(events(&trace), vec!["redo:a", "redo:b", "undo:b"]);
	assert!(s.can_redo());
	assert_eq!(s.done_count(), 2);
	assert_eq!(s.command_name(2).unwrap(), "B"); // undone row is still labeled
	assert_eq!(s.command_is_done(2).unwrap(), false);

	// Redo restores.
	s.redo().unwrap();
	assert_eq!(events(&trace), vec!["redo:a", "redo:b", "undo:b", "redo:b"]);
	assert_eq!(s.done_count(), 3);

	// Undo then push drops the redoable tail.
	s.undo().unwrap();
	assert!(s.can_redo());
	s.push(trace_cmd("c", &trace), "C");
	assert!(!s.can_redo(), "pushing drops redoable tail");
	assert_eq!(s.command_count(), 3);
	assert_eq!(s.command_name(2).unwrap(), "C");
}

#[test]
fn stack_undo_redo_noop_when_invalid() {
	let mut s = UndoStack::new();
	// Undo on a stack with only the bottom empty command is a no-op.
	s.undo().unwrap();
	assert_eq!(s.command_count(), 1);
	assert!(!s.can_redo());
	s.redo().unwrap();
	assert_eq!(s.command_count(), 1);
}

#[test]
fn stack_jump_clamps_and_lands() {
	let trace = new_trace();
	let mut s = UndoStack::new();
	s.push(trace_cmd("a", &trace), "A");
	s.push(trace_cmd("b", &trace), "B");
	s.push(trace_cmd("c", &trace), "C");
	assert_eq!(s.done_count(), 4); // empty + A + B + C

	// Jump back to just the empty command (clamps negative to 0).
	s.jump(0);
	assert_eq!(s.done_count(), 1);
	assert!(!s.can_undo());
	assert!(s.can_redo());
	assert_eq!(
		events(&trace),
		vec!["redo:a", "redo:b", "redo:c", "undo:c", "undo:b", "undo:a"]
	);

	// Jump forward redoes in order.
	s.jump(3);
	assert_eq!(s.done_count(), 3);
	assert_eq!(
		events(&trace),
		vec!["redo:a", "redo:b", "redo:c", "undo:c", "undo:b", "undo:a", "redo:a", "redo:b"]
	);

	// Jump beyond the top redoes up to the top (matches the C++ `jump`:
	// the undone C is redoable, so the second loop runs).
	s.jump(100);
	assert_eq!(s.done_count(), 4);
}

#[test]
fn stack_discards_empty_multi() {
	let mut s = UndoStack::new();
	let empty_multi = UndoCommand::multi();
	s.push(empty_multi, "empty");
	assert_eq!(
		s.command_count(),
		1,
		"empty multi command is dropped on push"
	);

	s.push_pre_executed(UndoCommand::multi(), "empty2");
	assert_eq!(s.command_count(), 1);
}

#[test]
fn stack_push_pre_executed_skips_redo() {
	let trace = new_trace();
	let mut s = UndoStack::new();
	s.push_pre_executed(trace_cmd("a", &trace), "A");
	assert_eq!(
		events(&trace),
		Vec::<String>::new(),
		"push_pre_executed does not run redo"
	);
	assert_eq!(s.command_count(), 2);
	assert!(s.can_undo());
	assert!(s.command_is_done(1).unwrap());

	// Undoing still runs the undo callback.
	s.undo().unwrap();
	assert_eq!(events(&trace), vec!["undo:a"]);
}

#[test]
fn stack_clear_resets_to_bottom() {
	let trace = new_trace();
	let mut s = UndoStack::new();
	s.push(trace_cmd("a", &trace), "A");
	s.undo().unwrap();
	assert_eq!(s.command_count(), 2);

	s.clear();
	assert_eq!(s.command_count(), 1);
	assert_eq!(s.command_name(0).unwrap(), "New/Open Project");
	assert!(!s.can_undo());
	assert!(!s.can_redo());
}

#[test]
fn stack_caps_at_k_max() {
	let trace = new_trace();
	let mut s = UndoStack::new();
	for i in 0..(K_MAX_UNDO_COMMANDS + 20) {
		s.push(trace_cmd("x", &trace), &format!("cmd{i}"));
	}
	assert_eq!(s.command_count() as usize, K_MAX_UNDO_COMMANDS);
}

#[test]
fn stack_query_bounds_errors() {
	let s = UndoStack::new();
	assert!(matches!(s.command_name(-1), Err(Error::NotFound)));
	assert!(matches!(s.command_name(1), Err(Error::NotFound)));
	assert!(matches!(s.command_is_done(-1), Err(Error::NotFound)));
	assert!(matches!(s.command_is_done(1), Err(Error::NotFound)));
}
