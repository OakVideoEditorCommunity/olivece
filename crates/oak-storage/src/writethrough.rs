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

//! Live write-through to the project library (plan M13 §2/§3; M14 R1:
//! sunk from the engine facade's `storage.rs`).
//!
//! Every opened project is bound to a database session — a
//! `(library uri, project uuid)` pair — and re-saved after every
//! successful undo-path operation. The module subscribes to the oakundo
//! process-wide stack's **command-success observers**
//! ([`oak_undo::global::add_observer`]), so each command's diff lands in
//! the journal transactionally (the oakstorage database backend does the
//! diff itself) with no facade round-trip.
//!
//! ## Binding model
//!
//! The map is keyed by the project box's identity (`Arc::as_ptr` — one
//! per in-memory project instance), so several projects can be bound at
//! once (multi-project, plan §3) without confusing their library rows.
//! Every undo-path operation re-saves ALL bound projects
//! ([`note_command`]): the oakstorage backend diffs each project against
//! its own library head, so untouched projects are no-op touches and
//! only the project the command actually changed advances its journal.
//! (The plan's "current project" phrasing maps to this — the undo stack
//! is cleared on every project switch, so at most one project's graph
//! changes per command; a bound-but-untouched project can gain its
//! import row this way, which reflects its true state.) Closing a
//! project ([`unbind_project`], hooked from the facade's `project_free`)
//! flushes its pending writes and drops the binding. The binding holds
//! the boxed project ([`ProjectArc`]) directly — the `CHandle` facade
//! entries convert at the boundary — so a bound project outlives the
//! caller's own handle reference.
//!
//! The library is selected from the `Storage` config group (all defaults
//! are config-driven, plan §5):
//!
//! - `Storage/Backend` — `"sqlite"` (the documented default value),
//!   `"database"` or `"pg"` enable the write-through; any other value
//!   (e.g. `"off"`) disables it. When the key is absent, no library is
//!   configured: projects stay unbound and the undo path runs without
//!   touching a database (graceful degradation — this is what keeps
//!   headless consumers and the test suite from writing to the user's
//!   default library).
//! - `Storage/SqlitePath` — the SQLite library file; default (used when
//!   storage is enabled) `<system data directory>/library.db` (the same
//!   location `FileFunctions::get_configuration_location` derives,
//!   honoring `OAK_CONFIG_DIR` and portable mode).
//! - `Storage/PgUrl` — the PostgreSQL connection string (plan D3), used
//!   when `Storage/Backend` is `"pg"`: `user:pass@host:5432/dbname`
//!   (libpq URL form; an optional `postgres://`/`postgresql://` scheme is
//!   accepted and stripped). The resolved library URI is
//!   `oakdb+pg://<PgUrl>`. When `Backend = "pg"` but `PgUrl` is absent
//!   or empty, no library is configured (same graceful degradation).
//!
//! ## Snapshot thread and exit flush
//!
//! A background thread re-snapshots every *dirty* binding every
//! `Storage/SnapshotIntervalSec` seconds (default 600; ≤ 0 acts every
//! wake). Snapshots are latest-wins: the backend writes the full payload
//! at the current head seq and prunes to the newest three. The thread is
//! notified on bind and stops on the exit flush. [`flush_all`] (exported
//! by the facade as `oakengine_storage_flush`) is the exit path: it stops
//! the thread, then writes through and snapshots every still-bound
//! project (save + snapshot drain). A write-through failure never
//! propagates to the caller — it is recorded in the binding's
//! `last_error` ([`last_error`]) and the project keeps working (graceful
//! degradation, plan §5).

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use oak_undo::global;

use crate::backends::database::DatabaseBackend;
use crate::handle::CHandle;
use crate::nodeutil::ProjectArc;
use crate::uri::StorageUri;

/// One bound project: its session (library uri + row uuid) plus the
/// write state.
struct Binding {
	/// The boxed project (kept alive here past the caller's own handle
	/// reference).
	project: ProjectArc,
	/// Library uri (`oakdb+sqlite:///…`).
	uri: String,
	/// Library row uuid.
	uuid: String,
	/// Whether a write-through happened since the last successful
	/// snapshot (the snapshot thread and the flush drain on this).
	dirty: bool,
	/// Monotonic counter bumped on every write-through; a snapshot only
	/// clears `dirty` when the generation still matches, so a write that
	/// lands while the snapshot runs is not lost.
	write_gen: u64,
	/// Last write-through / snapshot error (graceful degradation).
	last_error: Option<String>,
}

/// The process-wide oakstorage backend (shared by the write-throughs, the
/// snapshot thread and the library-manager exports of the facade; the
/// backend serializes its own operations).
pub fn backend() -> &'static DatabaseBackend {
	static BACKEND: OnceLock<DatabaseBackend> = OnceLock::new();
	BACKEND.get_or_init(DatabaseBackend::new)
}

/// project identity (boxed project allocation) -> binding.
fn bindings() -> &'static Mutex<HashMap<usize, Binding>> {
	static BINDINGS: OnceLock<Mutex<HashMap<usize, Binding>>> = OnceLock::new();
	BINDINGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Snapshot-thread runtime state.
struct SnapshotRuntime {
	/// The background thread (None after the exit flush joined it).
	handle: Option<std::thread::JoinHandle<()>>,
	/// Exit request (set by [`flush_all`]).
	stop: bool,
}

static SNAPSHOT: Mutex<SnapshotRuntime> = Mutex::new(SnapshotRuntime {
	handle: None,
	stop: false,
});

/// Wakes the snapshot thread on bind/flush/stop.
static SNAPSHOT_CV: Condvar = Condvar::new();

// ---------------------------------------------------------------------------
// Command-success subscription
// ---------------------------------------------------------------------------

/// Subscribe this module's [`note_command`] to the oakundo process-wide
/// stack's command-success observers (idempotent; the callback is a
/// `fn()`, so a single registration covers every future command).
fn ensure_command_observer() {
	static ONCE: OnceLock<()> = OnceLock::new();
	ONCE.get_or_init(|| {
		global::add_observer(note_command);
	});
}

// ---------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------

/// Bind `project` to the configured default library. No-op when the
/// backend is disabled by config, the project cannot be addressed (no
/// uuid), or it is already bound. No database write happens here — the
/// first undo-path operation creates the library row.
///
/// Facade entry: takes the project as a `CHandle` (the engine's project
/// handle form) and converts it to the boxed project at the boundary;
/// everything from here on works with the [`ProjectArc`].
pub fn bind_project(project: CHandle) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		if project.is_null() {
			return;
		}
		// SAFETY: facade project handles box a `ProjectArc`.
		let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
			return;
		};
		let key = Arc::as_ptr(&arc) as usize;
		{
			let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if g.contains_key(&key) {
				return; // Already bound (re-bind): nothing to refresh.
			}
		}
		if !storage_enabled() {
			return;
		}
		let Some(uri) = library_uri() else {
			return;
		};
		let uuid = {
			let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
			guard.uuid.clone()
		};
		if uuid.is_empty() {
			return;
		}
		bindings()
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(
				key,
				Binding {
					project: arc,
					uri,
					uuid,
					dirty: false,
					write_gen: 0,
					last_error: None,
				},
			);
		ensure_command_observer();
		ensure_thread();
	}));
}

/// Flush `project`'s pending writes and drop its binding (closing the
/// project). No-op when not bound.
pub fn unbind_project(project: CHandle) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		// SAFETY: facade project handles box a `ProjectArc`.
		let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
			return;
		};
		let key = Arc::as_ptr(&arc) as usize;
		flush_one(key);
		let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		// Dropping the binding releases the project reference.
		g.remove(&key);
	}));
}

/// Whether `project` currently has a binding (status-bar / D5 surface).
pub fn is_bound(project: CHandle) -> bool {
	if project.is_null() {
		return false;
	}
	// SAFETY: facade project handles box a `ProjectArc`.
	let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
		return false;
	};
	bindings()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.contains_key(&(Arc::as_ptr(&arc) as usize))
}

/// The last write-through / snapshot error of `project` (empty when none
/// or not bound).
pub fn last_error(project: CHandle) -> Option<String> {
	if project.is_null() {
		return None;
	}
	// SAFETY: facade project handles box a `ProjectArc`.
	let Ok(arc) = (unsafe { crate::nodeutil::project_arc(&project) }) else {
		return None;
	};
	bindings()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&(Arc::as_ptr(&arc) as usize))
		.and_then(|b| b.last_error.clone())
}

// ---------------------------------------------------------------------------
// Write-through
// ---------------------------------------------------------------------------

/// Persist every bound project after a successful undo-path operation
/// (push / group_end / jump). Registered as the oakundo global-stack
/// command-success observer ([`ensure_command_observer`]); the oakstorage
/// backend diffs each project against its own head, so unchanged projects
/// are no-op touches. No-op when nothing is bound.
pub fn note_command() {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		let keys: Vec<usize> = {
			let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			g.keys().copied().collect()
		};
		for key in keys {
			write_through(key);
		}
	}));
}

/// Save the project of `key` through the database backend (the backend
/// diffs and journals internally). Failures are recorded, never returned.
fn write_through(key: usize) {
	let (project, uri, _uuid) = {
		let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		match g.get_mut(&key) {
			Some(b) => (b.project.clone(), b.uri.clone(), b.uuid.clone()),
			None => return,
		}
	};
	let parsed = match StorageUri::parse(&uri) {
		Ok(u) => u,
		Err(_) => {
			record_error(key, "invalid storage uri");
			return;
		}
	};
	match backend().save_project(&project, &parsed, 0) {
		Ok(()) => {
			let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if let Some(b) = g.get_mut(&key) {
				b.dirty = true;
				b.write_gen = b.write_gen.wrapping_add(1);
				b.last_error = None;
			}
		}
		Err(e) => record_error(key, &format!("{e}")),
	}
}

fn record_error(key: usize, message: &str) {
	let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
	if let Some(b) = g.get_mut(&key) {
		b.last_error = Some(message.to_string());
	}
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Whether the write-through backend is enabled (config-driven, plan §5):
/// `Storage/Backend` set to `"sqlite"`, `"database"` or `"pg"` enables
/// it; any other explicit value (e.g. `"off"`) disables it. When NO
/// `Storage` configuration is present the backend is NOT enabled — "no
/// library configured" degrades gracefully to plain unbound projects,
/// which keeps headless consumers (the CLI) and the test suite from ever
/// writing to the user's default library. The documented default *values*
/// are `Backend = "sqlite"` and `SqlitePath = <system data dir>/library.db`
/// (used once storage is enabled without an explicit path).
pub fn storage_enabled() -> bool {
	let store = oak_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "Backend") {
		Ok(b) => b == "sqlite" || b == "database" || b == "pg",
		Err(_) => false,
	}
}

/// The configured SQLite library path (empty value = the default).
fn configured_sqlite_path() -> Option<String> {
	let store = oak_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "SqlitePath") {
		Ok(p) if !p.trim().is_empty() => Some(p.trim().to_string()),
		_ => None,
	}
}

/// The configured PostgreSQL connection string (`Storage/PgUrl`; empty
/// value = not configured).
fn configured_pg_url() -> Option<String> {
	let store = oak_core::configstore::ConfigStore::instance();
	match store.get(Some("Storage"), "PgUrl") {
		Ok(u) if !u.trim().is_empty() => Some(u.trim().to_string()),
		_ => None,
	}
}

/// The `oakdb+pg://…` uri of the configured PostgreSQL library (None
/// when `Storage/PgUrl` is absent). A `postgres://`/`postgresql://`
/// scheme on the config value is stripped — the oakdb uri body is the
/// bare connection string (`user:pass@host:5432/dbname`).
fn pg_library_uri() -> Option<String> {
	let url = configured_pg_url()?;
	let body = url
		.strip_prefix("postgres://")
		.or_else(|| url.strip_prefix("postgresql://"))
		.unwrap_or(&url);
	Some(format!("oakdb+pg://{body}"))
}

/// The default library file: `<system data directory>/library.db`, where
/// the data directory is the standard per-user location
/// (`FileFunctions::get_configuration_location`: macOS Application
/// Support / XDG config, honoring `OAK_CONFIG_DIR` and portable mode).
pub fn default_library_path() -> String {
	let dir = oak_core::filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
	format!("{}/library.db", dir)
}

/// The `oakdb+…` uri of the configured library (None when the path
/// cannot be made absolute, or the PG url is missing). Shared with the
/// facade's library-manager exports.
pub fn library_uri() -> Option<String> {
	let store = oak_core::configstore::ConfigStore::instance();
	if store.get(Some("Storage"), "Backend").ok().as_deref() == Some("pg") {
		return pg_library_uri();
	}
	let path = match configured_sqlite_path() {
		Some(p) => p,
		None => default_library_path(),
	};
	let abs = std::path::absolute(&path).ok()?;
	Some(format!("oakdb+sqlite://{}", abs.display()))
}

// ---------------------------------------------------------------------------
// Snapshot thread
// ---------------------------------------------------------------------------

/// Spawn the snapshot thread (a no-op when one is already running). The
/// thread is (re)startable after an exit flush.
fn ensure_thread() {
	let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
	let alive = rt
		.handle
		.as_ref()
		.map(|h| !h.is_finished())
		.unwrap_or(false);
	if !alive {
		rt.stop = false;
		rt.handle = Some(
			std::thread::Builder::new()
				.name("oak-storage-snapshot".into())
				.spawn(snapshot_loop)
				.expect("snapshot thread spawn"),
		);
	}
}

/// The thread loop: sleep a tick, then snapshot every dirty binding.
/// The condvar makes the sleep interruptible (bind notifications and the
/// exit flush's stop signal wake it immediately).
fn snapshot_loop() {
	loop {
		let interval = snapshot_tick();
		let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
		let (guard, _) = SNAPSHOT_CV
			.wait_timeout(rt, interval)
			.unwrap_or_else(|e| e.into_inner());
		rt = guard;
		if rt.stop {
			break;
		}
		drop(rt);
		snapshot_dirty();
	}
}

/// The sleep between snapshot passes: `Storage/SnapshotIntervalSec`
/// seconds (default 600), with ≤ 0 treated as "every wake" and a 100 ms
/// floor so the thread stays responsive to bind/stop signals.
fn snapshot_tick() -> Duration {
	let secs = oak_core::configstore::ConfigStore::instance()
		.get_int(Some("Storage"), "SnapshotIntervalSec", 600);
	if secs <= 0 {
		Duration::from_millis(100)
	} else {
		Duration::from_secs(secs as u64)
	}
}

/// Snapshot every dirty binding at its head seq (latest-wins: a project
/// edited since the last pass snapshots at the newer head; an unchanged
/// head is a backend no-op).
fn snapshot_dirty() {
	let jobs: Vec<(usize, String, String, u64)> = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		g.iter()
			.filter(|(_, b)| b.dirty)
			.map(|(k, b)| (*k, b.uri.clone(), b.uuid.clone(), b.write_gen))
			.collect()
	};
	for (key, uri, uuid, gen) in jobs {
		let res = match StorageUri::parse(&uri) {
			Ok(u) => backend().snapshot(&u, &uuid),
			Err(_) => {
				record_error(key, "invalid storage uri");
				continue;
			}
		};
		match res {
			Ok(()) => {
				let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
				if let Some(b) = g.get_mut(&key) {
					// Only clear when no write landed since the snapshot
					// started (the generation counter detects it).
					if b.write_gen == gen {
						b.dirty = false;
					}
				}
			}
			Err(e) => record_error(key, &format!("{e}")),
		}
	}
}

// ---------------------------------------------------------------------------
// Exit flush
// ---------------------------------------------------------------------------

/// The exit path: stop the snapshot thread, then save + snapshot every
/// still-bound project (plan §2 "退出前 flush"). Idempotent; safe to call
/// again after new projects are bound (the thread restarts on the next
/// bind).
pub fn flush_all() {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		// 1. Stop the thread and wait for it to finish its pass.
		{
			let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
			rt.stop = true;
		}
		SNAPSHOT_CV.notify_all();
		{
			let mut rt = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
			if let Some(h) = rt.handle.take() {
				drop(rt);
				let _ = h.join();
			}
		}
		// 2. Drain every binding (write-through + snapshot).
		let keys: Vec<usize> = {
			let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			g.keys().copied().collect()
		};
		for key in keys {
			flush_one(key);
		}
	}));
}

/// Save + snapshot one binding when it has pending writes (nothing to do
/// for a clean project — flush must not invent library rows).
fn flush_one(key: usize) {
	let dirty = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		g.get(&key).map(|b| b.dirty).unwrap_or(false)
	};
	if !dirty {
		return;
	}
	write_through(key);
	// Snapshot the head the write-through just reached (capture the
	// generation AFTER the write so the drain clears the dirty flag).
	let job = {
		let g = bindings().lock().unwrap_or_else(|e| e.into_inner());
		match g.get(&key) {
			Some(b) => (b.uri.clone(), b.uuid.clone(), b.write_gen),
			None => return,
		}
	};
	let res = match StorageUri::parse(&job.0) {
		Ok(u) => backend().snapshot(&u, &job.1),
		Err(_) => {
			record_error(key, "invalid storage uri");
			return;
		}
	};
	match res {
		Ok(()) => {
			let mut g = bindings().lock().unwrap_or_else(|e| e.into_inner());
			if let Some(b) = g.get_mut(&key) {
				if b.write_gen == job.2 {
					b.dirty = false;
				}
			}
		}
		Err(e) => record_error(key, &format!("{e}")),
	}
}
