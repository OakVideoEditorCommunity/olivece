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

//! oakstorage database-backend integration tests (plan M13 D1, the
//! aggregation-granularity design).
//!
//! End-to-end against real SQLite library files in temp directories:
//! full-feature round-trip through a fresh session, the node-granularity
//! journal (import + diff), snapshot + journal replay, undo to any
//! point, snapshot pruning and journal retention, the project-manager
//! API surface (list/delete/duplicate/rename/import/export) and the
//! error/URI matrices. Nothing touches a real library. The same
//! behaviors run against real PostgreSQL in `database_pg_test.rs`
//! (gated on `OAK_TEST_PG_URL`); both suites share the fixtures and
//! helpers in `tests/common/mod.rs`.

mod common;

use std::path::PathBuf;

use sea_orm::entity::prelude::*;
use sea_orm::QueryOrder;
use oak_node::id::NodeId;
use oak_node::value::NodeValue;
use oak_storage::backend::StorageBackend;
use oak_storage::backends::database::entities::{journal, project, snapshot};
use oak_storage::backends::database::{
	derive_stats, DatabaseBackend, KIND_IMPORT, KIND_REDO, ProjectStats, SNAPSHOT_KEEP,
};
use oak_storage::error::{
	OAKSTORAGE_E_FORMAT, OAKSTORAGE_E_INVALID, OAKSTORAGE_E_IO, OAKSTORAGE_E_NO_BACKEND,
	OAKSTORAGE_E_NOT_FOUND, OAKSTORAGE_OK,
};
use oak_storage::nodeutil::project_arc;
use oak_storage::registry::Registry;
use oak_storage::uri::StorageUri;

use common::*;

// ---------------------------------------------------------------------------
// Round-trip through a fresh session
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_field_by_field() {
	let dir = temp_dir("rt");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();

	// A new backend instance = a new session; the library row is
	// selected explicitly by uuid.
	let session = DatabaseBackend::new();
	let (loaded_uuid, loaded) = load_project(&session, &project_uri(&uri, &uuid));
	assert_eq!(loaded_uuid, uuid);
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_full_fields(&o, &l);
	}

	// The default (no ?project=) pick returns the same single row.
	let session = DatabaseBackend::new();
	let (loaded_uuid, _) = load_project(&session, &uri);
	assert_eq!(loaded_uuid, uuid);

	// The settings mirror carries the current keys.
	let row = inspect_sqlite(&db, |conn| async move {
		project::Entity::find()
			.filter(project::Column::Uuid.eq(&uuid))
			.one(&conn)
			.await
			.unwrap()
			.unwrap()
			.id
	});
	let mirrored = settings_rows(&db, row);
	assert!(mirrored.contains(&("projectname".to_string(), "full-fixture".to_string())), "{mirrored:?}");
}

#[test]
fn registry_routes_oakdb_schemes() {
	assert_eq!(
		Registry::global()
			.resolve(&StorageUri::parse("oakdb+sqlite:///tmp/x.db").unwrap())
			.unwrap()
			.name(),
		"oakdb"
	);
	assert_eq!(
		Registry::global()
			.resolve(&StorageUri::parse("oakdb+pg://user@host/db").unwrap())
			.unwrap()
			.name(),
		"oakdb"
	);
	// The legacy `oakdb://` (no sub-scheme) stays unclaimed.
	assert_eq!(
		Registry::global()
			.resolve(&StorageUri::parse("oakdb://user@host/db").unwrap())
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NO_BACKEND
	);
}

// ---------------------------------------------------------------------------
// Journal semantics: import, diff, no-op save
// ---------------------------------------------------------------------------

/// First save is one `kind='import'` command carrying every node plus
/// the settings pseudo-node (plan §2); the project row advances to seq 1.
#[test]
fn first_save_is_an_import_command() {
	let dir = temp_dir("imp");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	let node_count = project.lock().unwrap().graph.node_count();
	save_project(&backend, &project, &uri).unwrap();

	let rows = inspect_sqlite(&db, |conn| async move {
		let proj = project::Entity::find()
			.filter(project::Column::Uuid.eq(&uuid))
			.one(&conn)
			.await
			.unwrap()
			.unwrap();
		let rows = journal::Entity::find()
			.filter(journal::Column::ProjectId.eq(proj.id))
			.order_by_asc(journal::Column::Seq)
			.order_by_asc(journal::Column::NodeIdentity)
			.all(&conn)
			.await
			.unwrap();
		(proj.id, proj.command_seq, rows)
	});
	let (pid, command_seq, rows) = rows;
	assert_eq!(command_seq, 1);
	assert_eq!(rows.len(), node_count + 1, "every node + the settings pseudo-node");
	assert!(rows.iter().all(|r| r.seq == 1 && r.kind == KIND_IMPORT), "one import command");
	assert!(rows.iter().all(|r| r.old_xml.is_none()), "import has no before images");
	assert!(rows.iter().all(|r| r.new_xml.is_some()), "import has after images");
	// The settings pseudo-node (identity 0) is present.
	assert!(rows.iter().any(|r| r.node_identity == 0), "settings row present");
	assert_eq!(
		rows.iter().filter(|r| r.node_identity == 0).count(),
		1,
		"exactly one settings row"
	);
	// The real-node rows use the +1 offset (settings 0 reserved).
	assert!(rows.iter().all(|r| r.node_identity >= 1 || r.node_identity == 0));
	// The node fragments round-trip: the settings row holds the settings
	// element.
	let settings_row = rows.iter().find(|r| r.node_identity == 0).unwrap();
	assert!(settings_row.new_xml.as_deref().unwrap().starts_with("<settings>"));
	let _ = pid;
}

/// A later save is a diff: only the changed nodes land as `kind='redo'`
/// rows with both before and after images (plan §0).
#[test]
fn later_saves_are_diffs() {
	let dir = temp_dir("diff");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();

	// Change one math node's value and one setting key.
	{
		let mut p = project.lock().unwrap();
		let math_ids: Vec<NodeId> = p
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		let a = math_ids[0];
		p.graph
			.get_mut(a)
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(9.5));
		p.settings.insert("projectname".to_string(), "diffed".to_string());
	}
	save_project(&backend, &project, &uri).unwrap();

	let uuid_q = uuid.clone();
	let rows = inspect_sqlite(&db, move |conn| async move {
		let proj = project::Entity::find()
			.filter(project::Column::Uuid.eq(&uuid_q))
			.one(&conn)
			.await
			.unwrap()
			.unwrap();
		let rows = journal::Entity::find()
			.filter(journal::Column::ProjectId.eq(proj.id))
			.all(&conn)
			.await
			.unwrap();
		(proj.id, proj.command_seq, rows)
	});
	let (pid, command_seq, rows) = rows;
	assert_eq!(command_seq, 2);
	let seq2: Vec<&journal::Model> = rows.iter().filter(|r| r.seq == 2).collect();
	assert_eq!(seq2.len(), 2, "one changed node + the settings row, nothing else");
	assert!(seq2.iter().all(|r| r.kind == KIND_REDO));
	assert!(seq2.iter().all(|r| r.old_xml.is_some() && r.new_xml.is_some()), "diff carries both images");
	let settings_row = seq2.iter().find(|r| r.node_identity == 0).unwrap();
	assert!(settings_row.new_xml.as_deref().unwrap().contains(">diffed<"));
	let _ = pid;

	// Head state after the diff reflects both changes.
	let session = DatabaseBackend::new();
	let (_, loaded) = load_project(&session, &project_uri(&uri, &uuid));
	{
		let l = loaded.lock().unwrap();
		let math_ids: Vec<NodeId> = l
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| l.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		assert_eq!(
			l.graph.get(math_ids[0]).unwrap().core.standard_value("param_a_in", -1),
			NodeValue::Float(9.5)
		);
		assert_eq!(l.settings.get("projectname").cloned(), Some("diffed".to_string()));
	}
}

/// A save that changes nothing bumps no command seq and writes no rows.
#[test]
fn no_op_save_is_a_touch_only() {
	let dir = temp_dir("noop");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();
	save_project(&backend, &project, &uri).unwrap();

	let (head, count) = inspect_sqlite(&db, |conn| async move {
		let proj = project::Entity::find()
			.filter(project::Column::Uuid.eq(&uuid))
			.one(&conn)
			.await
			.unwrap()
			.unwrap();
		let n = journal::Entity::find()
			.filter(journal::Column::ProjectId.eq(proj.id))
			.count(&conn)
			.await
			.unwrap();
		(proj.command_seq, n)
	});
	assert_eq!(head, 1, "no-op save keeps the head seq");
	// The import wrote 12 content nodes + the 2 graph endpoints (M0b)
	// + 1 settings row; the no-op added none.
	assert_eq!(count, 15, "only the import rows remain");
}

// ---------------------------------------------------------------------------
// Snapshot + journal replay and undo to any point
// ---------------------------------------------------------------------------

/// With `Storage/SnapshotIntervalSec` ≤ 0 every dirty save writes a
/// snapshot; the newest snapshot is the replay base and the journal rows
/// after it are applied on top. Deleting every snapshot still recovers
/// the same state from an empty base plus the full journal (plan §0:
/// "快照损坏也能从空工程 + 全 journal 重建").
#[test]
fn snapshot_and_journal_replay() {
	let dir = temp_dir("replay");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	with_config("Storage", "SnapshotIntervalSec", 0, || {
		let project = build_full_project();
		let uuid = uuid_of(&project);
		save_project(&backend, &project, &uri).unwrap();
		// Second command: one value change.
		{
			let mut p = project.lock().unwrap();
			let math_ids: Vec<NodeId> = p
				.graph
				.node_ids()
				.into_iter()
				.filter(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
				.collect();
			p.graph
				.get_mut(math_ids[0])
				.unwrap()
				.core
				.set_standard_value("param_a_in", -1, NodeValue::Float(7.25));
		}
		save_project(&backend, &project, &uri).unwrap();

		// Both saves produced snapshots (head seq 2).
		let uuid_q = uuid.clone();
		let snaps = inspect_sqlite(&db, move |conn| async move {
			let proj = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid_q))
				.one(&conn)
				.await
				.unwrap()
				.unwrap();
			snapshot::Entity::find()
				.filter(snapshot::Column::ProjectId.eq(proj.id))
				.all(&conn)
				.await
				.unwrap()
		});
		assert_eq!(snaps.len(), 2, "one snapshot per dirty save");

		// Load (head): snapshot seq 2 is the base, nothing after it.
		let session = DatabaseBackend::new();
		let (_, loaded) = load_project(&session, &project_uri(&uri, &uuid));
		{
			let o = project.lock().unwrap();
			let l = loaded.lock().unwrap();
			assert_full_fields(&o, &l);
		}

		// Destroy the snapshots: replay degrades to empty base + full
		// journal and still reconstructs the head.
		let uuid_q = uuid.clone();
		inspect_sqlite(&db, move |conn| async move {
			let proj = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid_q))
				.one(&conn)
				.await
				.unwrap()
				.unwrap();
			snapshot::Entity::delete_many()
				.filter(snapshot::Column::ProjectId.eq(proj.id))
				.exec(&conn)
				.await
				.unwrap();
		});
		let session = DatabaseBackend::new();
		let (_, loaded) = load_project(&session, &project_uri(&uri, &uuid));
		{
			let o = project.lock().unwrap();
			let l = loaded.lock().unwrap();
			assert_full_fields(&o, &l);
		}
	});
}

/// The journal is the persistent undo history: `load_at(seq)` replays to
/// any point (plan §0 "撤销到任意点").
#[test]
fn undo_to_any_point() {
	let dir = temp_dir("undo");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	let math_a = {
		let p = project.lock().unwrap();
		p.graph
			.node_ids()
			.into_iter()
			.find(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.unwrap()
	};
	// Command 1: value 2.5 (fixture default).
	save_project(&backend, &project, &uri).unwrap();
	// Command 2: value 6.0.
	{
		let mut p = project.lock().unwrap();
		p.graph
			.get_mut(math_a)
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(6.0));
	}
	save_project(&backend, &project, &uri).unwrap();
	// Command 3: add a node.
	let (new_id, value) = {
		let mut p = project.lock().unwrap();
		let (core, behavior) = (oak_node::factory::Factory::global().find(MATH).unwrap().create)();
		let id = p.graph.add_node(core, behavior);
		p.graph
			.get_mut(id)
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(11.0));
		let v = p
			.graph
			.get(id)
			.unwrap()
			.core
			.standard_value("param_a_in", -1);
		(id, v)
	};
	save_project(&backend, &project, &uri).unwrap();

	// Head (seq 3): the extra node exists.
	let session = DatabaseBackend::new();
	let (_, head) = load_project(&session, &project_uri(&uri, &uuid));
	{
		let h = head.lock().unwrap();
		assert!(h.graph.is_valid(new_id), "node added in command 3 is live");
		assert_eq!(
			h.graph.get(new_id).unwrap().core.standard_value("param_a_in", -1),
			value
		);
	}

	// Undo to seq 2: the node is gone, the value is 6.0.
	let session = DatabaseBackend::new();
	let at2 = load_at(&session, &uri, &uuid, 2);
	{
		let l = at2.lock().unwrap();
		assert!(!l.graph.is_valid(new_id), "command 3 rolled back");
		let id = l
			.graph
			.node_ids()
			.into_iter()
			.find(|id| l.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.unwrap();
		assert_eq!(
			l.graph.get(id).unwrap().core.standard_value("param_a_in", -1),
			NodeValue::Float(6.0)
		);
	}

	// Undo to seq 1: the value is back to the fixture default.
	let session = DatabaseBackend::new();
	let at1 = load_at(&session, &uri, &uuid, 1);
	{
		let l = at1.lock().unwrap();
		// The graph endpoints (M0b) are not part of the fixture's 12
		// content nodes.
		let content = l
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| !l.graph.is_endpoint(*id))
			.count();
		assert_eq!(content, 12, "fixture node count");
		let id = l
			.graph
			.node_ids()
			.into_iter()
			.find(|id| l.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.unwrap();
		assert_eq!(
			l.graph.get(id).unwrap().core.standard_value("param_a_in", -1),
			NodeValue::Float(2.5)
		);
	}

	// Undo to seq 0: an empty project. The graph endpoints (M0b) are
	// recreated at load time, so "empty" means no content nodes.
	let session = DatabaseBackend::new();
	let at0 = load_at(&session, &uri, &uuid, 0);
	{
		let l = at0.lock().unwrap();
		assert_eq!(
			l.graph
				.node_ids()
				.into_iter()
				.filter(|id| !l.graph.is_endpoint(*id))
				.count(),
			0,
			"empty project at seq 0"
		);
		assert!(l.graph.endpoints().is_some(), "endpoints exist even in the empty state");
	}

	// Out of range -> E_INVALID.
	let session = DatabaseBackend::new();
	assert_eq!(
		session
			.load_at(&StorageUri::parse(&uri).unwrap(), &uuid, 99)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_INVALID
	);
}

/// Snapshot pruning keeps the newest [`SNAPSHOT_KEEP`] copies.
#[test]
fn snapshot_pruning_keeps_three() {
	let dir = temp_dir("prune");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	with_config("Storage", "SnapshotIntervalSec", 0, || {
		let project = build_full_project();
		let uuid = uuid_of(&project);
		// 6 commands, each dirty-snapshotted.
		for i in 0..6i64 {
			{
				let mut p = project.lock().unwrap();
				let id = p.graph.node_ids()[0];
				p.graph
					.get_mut(id)
					.unwrap()
					.core
					.label = format!("step {i}");
			}
			save_project(&backend, &project, &uri).unwrap();
		}
		let seqs = inspect_sqlite(&db, |conn| async move {
			let proj = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid))
				.one(&conn)
				.await
				.unwrap()
				.unwrap();
			snapshot::Entity::find()
				.filter(snapshot::Column::ProjectId.eq(proj.id))
				.order_by_desc(snapshot::Column::CommandSeq)
				.all(&conn)
				.await
				.unwrap()
				.into_iter()
				.map(|s| s.command_seq)
				.collect::<Vec<i64>>()
		});
		assert_eq!(seqs.len() as u64, SNAPSHOT_KEEP, "only the newest {SNAPSHOT_KEEP}");
		assert_eq!(seqs[0], 6, "the newest survives");
	});
}

/// Journal retention (`Storage/JournalRetentionDays`): rows older than
/// the window and covered by the newest snapshot are dropped; the head
/// stays reconstructible (snapshot + remaining rows).
#[test]
fn journal_retention_truncation() {
	let dir = temp_dir("retain");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	// Command 1 (import) + command 2 (a value change).
	save_project(&backend, &project, &uri).unwrap();
	{
		let mut p = project.lock().unwrap();
		let math_ids: Vec<NodeId> = p
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		p.graph
			.get_mut(math_ids[0])
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(3.25));
	}
	save_project(&backend, &project, &uri).unwrap();
	// Snapshot at the head so rows ≤ 2 are covered.
	backend.snapshot(&StorageUri::parse(&uri).unwrap(), &uuid).unwrap();

	// Backdate every journal row two days, then save a third command
	// with a 1-day retention window.
	inspect_sqlite(&db, |conn| async move {
		let old = chrono::Utc::now().naive_utc() - chrono::Duration::days(2);
		journal::Entity::update_many()
			.col_expr(journal::Column::At, sea_orm::sea_query::Expr::value(old))
			.exec(&conn)
			.await
			.unwrap();
	});
	with_config("Storage", "JournalRetentionDays", 1, || {
		{
			let mut p = project.lock().unwrap();
			let math_ids: Vec<NodeId> = p
				.graph
				.node_ids()
				.into_iter()
				.filter(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
				.collect();
			p.graph
				.get_mut(math_ids[0])
				.unwrap()
				.core
				.set_standard_value("param_a_in", -1, NodeValue::Float(4.5));
		}
		save_project(&backend, &project, &uri).unwrap();
	});

	// Only the third command's rows survive.
	let uuid_q = uuid.clone();
	let (pid, rows) = inspect_sqlite(&db, move |conn| async move {
		let proj = project::Entity::find()
			.filter(project::Column::Uuid.eq(&uuid_q))
			.one(&conn)
			.await
			.unwrap()
			.unwrap();
		let rows = journal::Entity::find()
			.filter(journal::Column::ProjectId.eq(proj.id))
			.all(&conn)
			.await
			.unwrap();
		(proj.id, rows)
	});
	let seqs: Vec<i64> = rows.iter().map(|r| r.seq).collect();
	assert!(!seqs.contains(&1) && !seqs.contains(&2), "old commands pruned: {seqs:?}");
	assert_eq!(seqs, vec![3], "only the fresh command remains");
	let _ = pid;

	// The head state is still correct (snapshot at seq 2 + command 3).
	let session = DatabaseBackend::new();
	let (_, loaded) = load_project(&session, &project_uri(&uri, &uuid));
	{
		let l = loaded.lock().unwrap();
		let math_ids: Vec<NodeId> = l
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| l.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		assert_eq!(
			l.graph.get(math_ids[0]).unwrap().core.standard_value("param_a_in", -1),
			NodeValue::Float(4.5)
		);
		assert_eq!(
			l.graph
				.node_ids()
				.into_iter()
				.filter(|id| !l.graph.is_endpoint(*id))
				.count(),
			12
		);
	}
}

// ---------------------------------------------------------------------------
// Project-manager API surface
// ---------------------------------------------------------------------------

#[test]
fn list_delete_duplicate_rename() {
	let dir = temp_dir("mgr");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	// Empty library lists nothing.
	assert!(backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap().is_empty());

	let alpha = save_named_project(&backend, &uri, "Alpha");
	let beta = save_named_project(&backend, &uri, "Beta");

	let list = backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap();
	assert_eq!(list.len(), 2, "two rows");
	// Most recently modified first.
	assert_eq!(list[0].name, "Beta");
	assert_eq!(list[1].name, "Alpha");
	assert_eq!(list[0].command_seq, 1);
	assert_eq!(list[0].schema_ver, oak_node::serializer::CURRENT_VERSION.0 as i32);

	// Rename (library metadata).
	backend
		.rename_project(&StorageUri::parse(&uri).unwrap(), &alpha, "Alpha Renamed")
		.unwrap();
	let list = backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap();
	assert!(list.iter().any(|p| p.name == "Alpha Renamed"), "{list:?}");
	assert_eq!(
		backend
			.rename_project(&StorageUri::parse(&uri).unwrap(), "{missing}", "X")
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);

	// Duplicate: fresh uuid, default "(copy)" name, history copied.
	let copy = backend
		.duplicate_project(&StorageUri::parse(&uri).unwrap(), &beta, None)
		.unwrap();
	assert_ne!(copy.uuid, beta, "fresh uuid");
	assert_eq!(copy.name, "Beta (copy)");
	assert_eq!(copy.command_seq, 1);
	let list = backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap();
	assert_eq!(list.len(), 3);
	let renamed_copy = backend
		.duplicate_project(&StorageUri::parse(&uri).unwrap(), &beta, Some("Beta Clone"))
		.unwrap();
	assert_eq!(renamed_copy.name, "Beta Clone");
	assert_eq!(
		backend
			.duplicate_project(&StorageUri::parse(&uri).unwrap(), "{missing}", None)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);

	// Delete.
	for uuid in [&alpha, &beta, &copy.uuid, &renamed_copy.uuid] {
		backend
			.delete_project(&StorageUri::parse(&uri).unwrap(), uuid)
			.unwrap();
	}
	assert!(backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap().is_empty());
	assert_eq!(
		backend
			.delete_project(&StorageUri::parse(&uri).unwrap(), "{missing}")
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);
}

/// Duplicating a full-feature project copies the whole history: the copy
/// loads identically under its own uuid and keeps the undo history.
#[test]
fn duplicate_preserves_history() {
	let dir = temp_dir("dup");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();
	{
		let mut p = project.lock().unwrap();
		let math_ids: Vec<NodeId> = p
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| p.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		p.graph
			.get_mut(math_ids[0])
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(6.0));
	}
	save_project(&backend, &project, &uri).unwrap();

	let copy = backend
		.duplicate_project(&StorageUri::parse(&uri).unwrap(), &uuid, None)
		.unwrap();
	assert_eq!(copy.command_seq, 2);

	// The copy loads through a fresh session, field-for-field (uuid is
	// fresh by design).
	let session = DatabaseBackend::new();
	let (loaded_uuid, loaded) = load_project(&session, &project_uri(&uri, &copy.uuid));
	assert_eq!(loaded_uuid, copy.uuid);
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_full_state(&o, &l);
	}

	// Undo history travels with the copy: load_at(1) on the copy gives
	// the pre-change state.
	let session = DatabaseBackend::new();
	let at1 = load_at(&session, &uri, &copy.uuid, 1);
	{
		let l = at1.lock().unwrap();
		let math_ids: Vec<NodeId> = l
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| l.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		assert_eq!(
			l.graph.get(math_ids[0]).unwrap().core.standard_value("param_a_in", -1),
			NodeValue::Float(2.5)
		);
	}
}

/// Manager stats are derived from the node graph, not stored (plan §4).
#[test]
fn project_stats_derived_from_graph() {
	let dir = temp_dir("stats");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();

	let stats = backend
		.project_stats(&StorageUri::parse(&uri).unwrap(), &uuid)
		.unwrap();
	assert_eq!(
		stats,
		ProjectStats {
			duration_ms: 6000,
			track_count: 1,
			clip_count: 2,
			footage_count: 2,
		}
	);
	// Same numbers derive_stats yields directly on the live project.
	let guard = project.lock().unwrap();
	assert_eq!(derive_stats(&guard), stats);
	assert_eq!(
		backend
			.project_stats(&StorageUri::parse(&uri).unwrap(), "{missing}")
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);
}

#[test]
fn export_and_import_round_trip() {
	let dir = temp_dir("xi");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let project = build_full_project();
	let uuid = uuid_of(&project);
	save_project(&backend, &project, &uri).unwrap();

	// Export: .ove written from an in-memory assembly; the file backend
	// re-imports it byte-for-byte.
	let out = dir.join("exported.ove");
	let file = file_uri(&out);
	backend
		.export_to_file(
			&StorageUri::parse(&uri).unwrap(),
			&uuid,
			&StorageUri::parse(&file).unwrap(),
		)
		.unwrap();
	assert!(out.exists(), ".ove written");
	let text = std::fs::read_to_string(&out).unwrap();
	assert!(text.starts_with("<project version=\"1\">"), "{text}");
	let file_backend = Registry::global()
		.resolve(&StorageUri::parse(&file).unwrap())
		.unwrap();
	let result = file_backend
		.load(&StorageUri::parse(&file).unwrap())
		.unwrap();
	assert_eq!(result.version_info, OAKSTORAGE_OK);
	let loaded = unsafe { project_arc(&result.project) }.unwrap();
	release(result.project);
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_full_fields(&o, &l);
	}

	// Import: the .ove lands as a new library row under a fresh uuid;
	// importing it again yields a distinct row.
	let imported = backend
		.import_from_file(
			&StorageUri::parse(&uri).unwrap(),
			&StorageUri::parse(&file).unwrap(),
		)
		.unwrap();
	assert_ne!(imported, uuid, "import gets a fresh uuid");
	let imported2 = backend
		.import_from_file(
			&StorageUri::parse(&uri).unwrap(),
			&StorageUri::parse(&file).unwrap(),
		)
		.unwrap();
	assert_ne!(imported2, imported, "repeat imports are new rows");
	let list = backend.list_projects(&StorageUri::parse(&uri).unwrap()).unwrap();
	assert_eq!(list.len(), 3, "original + two imports");
	let session = DatabaseBackend::new();
	let (imported_uuid, imported_proj) = load_project(&session, &project_uri(&uri, &imported));
	assert_eq!(imported_uuid, imported);
	{
		let l = imported_proj.lock().unwrap();
		assert_eq!(
			l.graph
				.node_ids()
				.into_iter()
				.filter(|id| !l.graph.is_endpoint(*id))
				.count(),
			12
		);
		assert_eq!(l.settings.get("projectname").cloned(), Some("full-fixture".to_string()));
	}

	// Error paths: unknown project on export; non-file target; corrupt
	// file on import.
	assert_eq!(
		backend
			.export_to_file(
				&StorageUri::parse(&uri).unwrap(),
				"{missing}",
				&StorageUri::parse(&file).unwrap(),
			)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);
	assert_eq!(
		backend
			.export_to_file(
				&StorageUri::parse(&uri).unwrap(),
				&uuid,
				&StorageUri::parse("oakdb+sqlite:///tmp/x.db").unwrap(),
			)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_INVALID
	);
	let corrupt = dir.join("corrupt.ove");
	std::fs::write(&corrupt, "<project><nodes><node></project>").unwrap();
	assert_eq!(
		backend
			.import_from_file(
				&StorageUri::parse(&uri).unwrap(),
				&StorageUri::parse(&file_uri(&corrupt)).unwrap(),
			)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_FORMAT
	);
}

// ---------------------------------------------------------------------------
// Project selection (`?project=` vs default)
// ---------------------------------------------------------------------------

#[test]
fn project_selection_via_query() {
	let dir = temp_dir("sel");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	let a = save_named_project(&backend, &uri, "A");
	let b = save_named_project(&backend, &uri, "B");

	// Explicit uuid picks the right row regardless of recency.
	let session = DatabaseBackend::new();
	let (loaded_uuid, _) = load_project(&session, &project_uri(&uri, &a));
	assert_eq!(loaded_uuid, a);

	// Default pick = most recently modified (B was written last).
	let session = DatabaseBackend::new();
	let (loaded_uuid, loaded) = load_project(&session, &uri);
	assert_eq!(loaded_uuid, b);
	assert_eq!(
		loaded.lock().unwrap().settings.get("projectname").cloned(),
		Some("B".to_string())
	);

	// Unknown uuid -> E_NOT_FOUND; empty library -> E_NOT_FOUND.
	let session = DatabaseBackend::new();
	assert_eq!(
		session
			.load(&StorageUri::parse(&project_uri(&uri, "{missing}")).unwrap())
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NOT_FOUND
	);
}

// ---------------------------------------------------------------------------
// Error paths, locking, URI matrix
// ---------------------------------------------------------------------------

/// PG targets are now live: malformed connection strings fail cleanly at
/// parse time (E_INVALID, no network touched) and an unreachable server
/// fails cleanly at connect (E_IO, no panic). Both run without a real
/// PostgreSQL server — the connection-refused case uses port 1 on
/// loopback, which nothing listens on.
#[test]
fn pg_invalid_and_unreachable_targets_error_cleanly() {
	let dir = temp_dir("pg");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();
	let project = build_full_project();
	save_project(&backend, &project, &uri).unwrap();
	let uuid = uuid_of(&project);

	// Invalid connection strings -> E_INVALID at parse (no connection).
	for bad in ["oakdb+pg://", "oakdb+pg://?project={x}", "oakdb+pg://user@"] {
		let err = backend
			.list_projects(&StorageUri::parse(bad).unwrap())
			.err()
			.unwrap();
		assert_eq!(err.code(), OAKSTORAGE_E_INVALID, "{bad}");
	}

	// Unreachable server (port 1 on loopback) -> E_IO at connect, and the
	// error is clean (no panic) across every operation. The connect probe
	// fails on the refused TCP connection without waiting out the pool's
	// acquire timeout.
	let dead = "oakdb+pg://user@127.0.0.1:1/db";
	for call in [
		backend.load(&StorageUri::parse(dead).unwrap()).map(|_| ()),
		save_project(&backend, &project, dead),
		backend.list_projects(&StorageUri::parse(dead).unwrap()).map(|_| ()),
		backend.delete_project(&StorageUri::parse(dead).unwrap(), &uuid),
		backend.rename_project(&StorageUri::parse(dead).unwrap(), &uuid, "X"),
		backend
			.duplicate_project(&StorageUri::parse(dead).unwrap(), &uuid, None)
			.map(|_| ()),
		backend
			.export_to_file(
				&StorageUri::parse(dead).unwrap(),
				&uuid,
				&StorageUri::parse("file:///tmp/x.ove").unwrap(),
			),
		backend.snapshot(&StorageUri::parse(dead).unwrap(), &uuid),
		backend.load_at(&StorageUri::parse(dead).unwrap(), &uuid, 1).map(|_| ()),
	] {
		assert_eq!(call.err().unwrap().code(), OAKSTORAGE_E_IO, "dead pg target");
	}

	// The sqlite library next to it is untouched by all of the above.
	let session = DatabaseBackend::new();
	let (loaded_uuid, _) = load_project(&session, &project_uri(&uri, &uuid));
	assert_eq!(loaded_uuid, uuid);
}

#[test]
fn invalid_uri_matrix() {
	let dir = temp_dir("uri");
	// Relative path / empty body -> E_INVALID.
	let backend = DatabaseBackend::new();
	for bad in ["oakdb+sqlite://relative.db", "oakdb+sqlite://"] {
		let err = backend
			.list_projects(&StorageUri::parse(bad).unwrap())
			.err()
			.unwrap();
		assert_eq!(err.code(), OAKSTORAGE_E_INVALID, "{bad}");
	}
	// Nonexistent parent directory -> E_IO at connect.
	let err = backend
		.list_projects(&StorageUri::parse(&db_uri(&dir.join("no/such/dir/lib.db"))).unwrap())
		.err()
		.unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_IO, "missing parent dir");

	// Unknown scheme stays unclaimed by the registry.
	assert_eq!(
		Registry::global()
			.resolve(&StorageUri::parse("oakdb+sqlite3:///tmp/x.db").unwrap())
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_NO_BACKEND
	);
}

/// Write failures surface as errors, not panics or silent corruption:
/// (a) read-only database files, (b) an out-of-range undo target, and
/// (c) concurrent writers on one file.
#[test]
fn failure_paths_report_cleanly() {
	let dir = temp_dir("fail");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);
	let backend = DatabaseBackend::new();

	// (a) Read-only db + WAL files: a fresh backend cannot write.
	let project = build_full_project();
	save_project(&backend, &project, &uri).unwrap();
	let mut targets = vec![db.clone()];
	for suffix in ["-wal", "-shm"] {
		let f = PathBuf::from(format!("{}{}", db.display(), suffix));
		if f.exists() {
			targets.push(f);
		}
	}
	let saved = targets
		.iter()
		.map(|f| std::fs::metadata(f).unwrap().permissions())
		.collect::<Vec<_>>();
	for f in &targets {
		let mut p = std::fs::metadata(f).unwrap().permissions();
		p.set_readonly(true);
		std::fs::set_permissions(f, p).unwrap();
	}
	let fresh = DatabaseBackend::new();
	let err = save_project(&fresh, &project, &uri).err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_IO, "read-only library write must fail");
	for (f, p) in targets.iter().zip(saved) {
		std::fs::set_permissions(f, p).unwrap();
	}

	// (b) Undo beyond the head -> E_INVALID (the head-seq range is
	// enforced; load_at(0) stays valid).
	let uuid = uuid_of(&project);
	let session = DatabaseBackend::new();
	assert_eq!(
		session
			.load_at(&StorageUri::parse(&uri).unwrap(), &uuid, -1)
			.err()
			.unwrap()
			.code(),
		OAKSTORAGE_E_INVALID
	);
}

/// Two backends writing different projects to one file concurrently
/// complete without deadlock, and a same-uuid write race surfaces a
/// clean error instead of corrupting the library.
#[test]
fn concurrent_writers_are_serialized() {
	let dir = temp_dir("conc");
	let db = dir.join("lib.db");
	let uri = db_uri(&db);

	// Two projects, two threads, different uuids: both writers land.
	let p1 = build_full_project();
	let p2 = build_full_project();
	let uri_t1 = uri.clone();
	let uri_t2 = uri.clone();
	let b1 = DatabaseBackend::new();
	let b2 = DatabaseBackend::new();
	let t1 = std::thread::spawn(move || {
		for _ in 0..5 {
			save_project(&b1, &p1, &uri_t1).unwrap();
		}
	});
	let t2 = std::thread::spawn(move || {
		for _ in 0..5 {
			save_project(&b2, &p2, &uri_t2).unwrap();
		}
	});
	t1.join().unwrap();
	t2.join().unwrap();
	let list = DatabaseBackend::new()
		.list_projects(&StorageUri::parse(&uri).unwrap())
		.unwrap();
	assert_eq!(list.len(), 2, "both projects persisted");
	assert!(list.iter().all(|p| p.name == "full-fixture"), "{list:?}");

	// Same-uuid race: both threads save projects with the same uuid but
	// different content, so the writes genuinely contend (on the import
	// row, or on journal seqs). Results are Ok or a clean error, never a
	// panic, and the library still loads afterwards.
	let b3 = DatabaseBackend::new();
	let b4 = DatabaseBackend::new();
	let p3 = build_full_project();
	let p4 = build_full_project();
	{
		let mut g = p4.lock().unwrap();
		g.uuid = uuid_of(&p3); // force the same row identity
		let math_ids: Vec<NodeId> = g
			.graph
			.node_ids()
			.into_iter()
			.filter(|id| g.graph.get(*id).unwrap().behavior.type_id() == MATH)
			.collect();
		g.graph
			.get_mut(math_ids[0])
			.unwrap()
			.core
			.set_standard_value("param_a_in", -1, NodeValue::Float(8.0));
	}
	let same = uuid_of(&p3);
	let uri_a = uri.clone();
	let uri_b = uri.clone();
	let ta = std::thread::spawn(move || {
		(0..3)
			.map(|_| save_project(&b3, &p3, &uri_a).map_err(|e| e.code()))
			.collect::<Vec<_>>()
	});
	let tb = std::thread::spawn(move || {
		(0..3)
			.map(|_| save_project(&b4, &p4, &uri_b).map_err(|e| e.code()))
			.collect::<Vec<_>>()
	});
	let (ra, rb) = (ta.join().unwrap(), tb.join().unwrap());
	for (i, r) in ra.iter().chain(rb.iter()).enumerate() {
		match r {
			Ok(()) => {}
			Err(code) => {
				// A same-uuid/seq insert conflict surfaces as a database
				// error (or a retried BUSY gives up).
				assert!(
					*code == OAKSTORAGE_E_IO || *code == oak_storage::error::OAKSTORAGE_E_FAILED,
					"writer {i} failed with unexpected code {code}"
				);
			}
		}
	}
	// The library is still consistent: the same-uuid project loads and
	// the journal's newest command matches the row's head seq.
	let session = DatabaseBackend::new();
	let (loaded_uuid, _) = load_project(&session, &project_uri(&uri, &same));
	assert_eq!(loaded_uuid, same, "library consistent after races");
	let (head, seqs) = inspect_sqlite(&db, |conn| async move {
		let proj = project::Entity::find()
			.filter(project::Column::Uuid.eq(&same))
			.one(&conn)
			.await
			.unwrap()
			.unwrap();
		let mut seqs: Vec<i64> = journal::Entity::find()
			.filter(journal::Column::ProjectId.eq(proj.id))
			.all(&conn)
			.await
			.unwrap()
			.into_iter()
			.map(|r| r.seq)
			.collect();
		seqs.sort();
		(proj.command_seq, seqs)
	});
	assert!(!seqs.is_empty(), "the same-uuid project has commands");
	assert_eq!(
		seqs.last().copied(),
		Some(head),
		"head seq matches the newest command (seqs={seqs:?})"
	);
}
