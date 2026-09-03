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

//! Shared fixtures and helpers for the database-backend integration
//! tests (plan M13 D1/D3).
//!
//! Everything here is dialect-agnostic: the test bodies run against a
//! `oakdb+sqlite://…` or `oakdb+pg://…` uri string, and the helpers that
//! inspect rows behind the backend's back open a raw sea-orm connection
//! ([`inspect_sqlite`] / [`inspect_pg`]). `tests/database_test.rs` runs
//! the SQLite suite; `tests/database_pg_test.rs` runs the same behaviors
//! against a real PostgreSQL server gated on `OAK_TEST_PG_URL`.

// The module is compiled into two test binaries, each of which uses only
// the helpers for its own dialect, so the other dialect's helpers look
// dead to one binary while the other uses them.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::entity::prelude::*;
use oak_core::Rational;
use oak_node::block::ClipBlockBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::keyframe::{Interpolation, Keyframe};
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use oak_node::value::NodeValue;
use oak_storage::backend::StorageBackend;
use oak_storage::backends::database::entities::settings;
use oak_storage::backends::database::DatabaseBackend;
use oak_storage::error::OAKSTORAGE_OK;
use oak_storage::handle::CHandle;
use oak_storage::nodeutil::{make_project_owned, project_arc};
use oak_storage::uri::StorageUri;

// ---------------------------------------------------------------------------
// URIs and paths
// ---------------------------------------------------------------------------

/// A fresh, unique temp directory for one test.
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
	let dir = std::env::temp_dir().join(format!("oakstorage_db_{}_{}", std::process::id(), tag));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

/// `oakdb+sqlite:///…` URI for a database file.
pub(crate) fn db_uri(path: &Path) -> String {
	format!("oakdb+sqlite://{}", path.display())
}

/// `oakdb+sqlite:///…?project=<uuid>` URI selecting one library row.
pub(crate) fn project_uri(db: &str, uuid: &str) -> String {
	format!("{db}?project={uuid}")
}

/// `file://…` URI for a plain file.
pub(crate) fn file_uri(path: &Path) -> String {
	format!("file://{}", path.display())
}

// ---------------------------------------------------------------------------
// Save / load through the backend
// ---------------------------------------------------------------------------

/// Release an owned handle (refcount 1).
pub(crate) fn release(h: CHandle) {
	if let Some(release) = h.release {
		unsafe { release(h.ctx) };
	}
}

/// Save an `Arc<Mutex<Project>>` through the database backend.
pub(crate) fn save_project(
	backend: &DatabaseBackend,
	project: &Arc<Mutex<Project>>,
	uri: &str,
) -> oak_storage::error::Result<()> {
	let parsed = StorageUri::parse(uri).unwrap();
	let handle = make_project_owned(project.clone());
	let result = backend.save(handle, &parsed, 0);
	release(handle);
	result
}

/// Load a project through a *new* database backend session (a fresh
/// connection pool = a fresh session), returning `(uuid, loaded)`.
pub(crate) fn load_project(backend: &DatabaseBackend, uri: &str) -> (String, Arc<Mutex<Project>>) {
	let parsed = StorageUri::parse(uri).unwrap();
	let result = backend.load(&parsed).unwrap();
	assert_eq!(result.version_info, OAKSTORAGE_OK);
	let handle = result.project;
	let loaded = unsafe { project_arc(&handle) }.unwrap();
	let uuid = loaded.lock().unwrap().uuid.clone();
	release(handle);
	(uuid, loaded)
}

/// Load the state at `seq` (undo to any point).
pub(crate) fn load_at(backend: &DatabaseBackend, uri: &str, uuid: &str, seq: i64) -> Arc<Mutex<Project>> {
	let parsed = StorageUri::parse(uri).unwrap();
	let handle = backend.load_at(&parsed, uuid, seq).unwrap();
	let loaded = unsafe { project_arc(&handle) }.unwrap();
	release(handle);
	loaded
}

pub(crate) fn r_to_f(r: Rational) -> f64 {
	r.numerator() as f64 / r.denominator() as f64
}

pub(crate) fn assert_close(a: f64, b: f64) {
	assert!((a - b).abs() < 1e-6, "expected {a} close to {b}");
}

// ---------------------------------------------------------------------------
// Row inspection (raw sea-orm connections behind the backend's back)
// ---------------------------------------------------------------------------

/// Open a raw sea-orm SQLite connection to `path` and drive one future
/// against it on a private current-thread runtime.
pub(crate) fn inspect_sqlite<R, Fut>(path: &Path, f: impl FnOnce(sea_orm::DatabaseConnection) -> Fut) -> R
where
	Fut: std::future::Future<Output = R>,
{
	let rt = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	rt.block_on(async {
		let options = sea_orm::sqlx::sqlite::SqliteConnectOptions::new()
			.filename(path)
			.create_if_missing(true)
			.journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Wal)
			.busy_timeout(Duration::from_secs(5))
			.foreign_keys(true);
		let pool = sea_orm::sqlx::sqlite::SqlitePoolOptions::new()
			.max_connections(1)
			.connect_with(options)
			.await
			.unwrap();
		f(sea_orm::DatabaseConnection::from(pool)).await
	})
}

/// Open a raw sea-orm PostgreSQL connection to `url` and drive one
/// future against it on a private current-thread runtime.
pub(crate) fn inspect_pg<R, Fut>(url: &str, f: impl FnOnce(sea_orm::DatabaseConnection) -> Fut) -> R
where
	Fut: std::future::Future<Output = R>,
{
	let rt = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	rt.block_on(async {
		let pool = sea_orm::sqlx::postgres::PgPoolOptions::new()
			.max_connections(1)
			.connect(url)
			.await
			.unwrap();
		f(sea_orm::DatabaseConnection::from(pool)).await
	})
}

/// The settings mirror rows of a project (SQLite).
pub(crate) fn settings_rows(path: &Path, project_id: i64) -> Vec<(String, String)> {
	inspect_sqlite(path, |conn| async move {
		settings::Entity::find()
			.filter(settings::Column::ProjectId.eq(project_id))
			.all(&conn)
			.await
			.unwrap()
			.into_iter()
			.map(|s| (s.key, s.value))
			.collect()
	})
}

/// The settings mirror rows of a project (PostgreSQL).
pub(crate) fn settings_rows_pg(url: &str, project_id: i64) -> Vec<(String, String)> {
	inspect_pg(url, |conn| async move {
		settings::Entity::find()
			.filter(settings::Column::ProjectId.eq(project_id))
			.all(&conn)
			.await
			.unwrap()
			.into_iter()
			.map(|s| (s.key, s.value))
			.collect()
	})
}

/// The project uuid.
pub(crate) fn uuid_of(project: &Arc<Mutex<Project>>) -> String {
	project.lock().unwrap().uuid.clone()
}

/// Read a config value for the duration of a test (the config store is
/// process-global, so restore it afterwards).
pub(crate) fn with_config(group: &str, key: &str, value: i32, f: impl FnOnce()) {
	let store = oak_core::configstore::ConfigStore::instance();
	let before = store.get_int(Some(group), key, 0);
	store.set_int(Some(group), key, value);
	f();
	store.set_int(Some(group), key, before);
}

// ---------------------------------------------------------------------------
// Full-feature fixture (footage / sequence / track / clip / effect /
// keyframes) — the union of the ove round-trip and timeline fixtures.
// ---------------------------------------------------------------------------

pub(crate) const MATH: &str = "org.olivevideoeditor.Olive.math";

/// Build the round-trip fixture: root folder + two math nodes with
/// values/keyframes/label/color/link/connection + a sequence "My Seq"
/// with one video track carrying two clips (footage /a/b.mp4, /a/c.mp4).
pub(crate) fn build_full_project() -> Arc<Mutex<Project>> {
	let project = Project::new();
	let mut p = project.lock().unwrap();
	p.initialize().unwrap();

	let (core, behavior) = (oak_node::factory::Factory::global().find(MATH).unwrap().create)();
	let a = p.graph.add_node(core, behavior);
	{
		let e = p.graph.get_mut(a).unwrap();
		e.core.label = "Math A".to_string();
		e.core.override_color = 2;
		e.core.set_standard_value("param_a_in", -1, NodeValue::Float(2.5));
		e.core
			.keyframe_track_mut("param_a_in", -1)
			.set_key(Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(1.0),
				interpolation: Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		e.core
			.keyframe_track_mut("param_a_in", -1)
			.set_key(Keyframe {
				time: Rational::new(1, 1),
				value: NodeValue::Float(3.0),
				interpolation: Interpolation::Bezier,
				bezier_in: (0.1, 0.2),
				bezier_out: (0.3, 0.4),
			});
	}
	let (core, behavior) = (oak_node::factory::Factory::global().find(MATH).unwrap().create)();
	let b = p.graph.add_node(core, behavior);
	p.graph
		.get_mut(b)
		.unwrap()
		.core
		.set_standard_value("param_a_in", -1, NodeValue::Float(4.0));
	p.graph.connect(a, b, "param_b_in", -1).unwrap();
	p.graph.link(a, b);

	// Timeline: sequence + video track + two clips with footage.
	let (seq_id, lists) = oak_storage::nodeutil::create_sequence(&mut p.graph);
	p.graph.get_mut(seq_id).unwrap().core.label = "My Seq".to_string();

	let mut tb = TrackBehavior::new(TrackType::Video);
	tb.track_list = Some(lists[0]);
	let track_id = p.graph.add_node(NodeCore::new(), Box::new(tb));

	let foot1 = p
		.graph
		.add_node(NodeCore::new(), Box::new(FootageBehavior::new("/a/b.mp4")));
	let clip1 = {
		let (core, mut behavior) = oak_node::block::clip_create();
		let clip = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		clip.core.range = oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(100, 25));
		clip.core.media_in = Rational::new(0, 1);
		clip.core.track = Some(track_id);
		clip.footage = Some(foot1);
		p.graph.add_node(core, behavior)
	};

	let foot2 = p
		.graph
		.add_node(NodeCore::new(), Box::new(FootageBehavior::new("/a/c.mp4")));
	let clip2 = {
		let (core, mut behavior) = oak_node::block::clip_create();
		let clip = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		clip.core.range =
			oak_core::TimeRange::new(Rational::new(100, 25), Rational::new(150, 25));
		clip.core.media_in = Rational::new(10, 25);
		clip.core.track = Some(track_id);
		clip.footage = Some(foot2);
		p.graph.add_node(core, behavior)
	};

	if let Some(entry) = p.graph.get_mut(track_id) {
		entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackBehavior>())
			.unwrap()
			.blocks = vec![clip1, clip2];
	}
	if let Some(entry) = p.graph.get_mut(lists[0]) {
		entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackListBehavior>())
			.unwrap()
			.tracks = vec![track_id];
	}

	p.settings
		.insert("projectname".to_string(), "full-fixture".to_string());
	drop(p);
	project
}

/// Field-by-field comparison of the full fixture after a round-trip
/// (uuid included).
pub(crate) fn assert_full_fields(orig: &Project, loaded: &Project) {
	assert_eq!(loaded.uuid, orig.uuid, "uuid");
	assert_full_state(orig, loaded);
}

/// Field-by-field comparison that ignores the uuid (a duplicated row
/// carries a fresh uuid by design).
pub(crate) fn assert_full_state(orig: &Project, loaded: &Project) {
	assert_eq!(loaded.settings, orig.settings, "settings");

	// Node set: count, type order (slot order preserved by the writer).
	let o_ids = orig.graph.node_ids();
	let l_ids = loaded.graph.node_ids();
	assert_eq!(l_ids.len(), o_ids.len(), "node count");
	let o_types: Vec<&str> = o_ids
		.iter()
		.map(|id| orig.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	let l_types: Vec<&str> = l_ids
		.iter()
		.map(|id| loaded.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	assert_eq!(l_types, o_types, "node types");

	// Math A: label, color, value, keyframes.
	let (a_o, a_l) = (o_ids[1], l_ids[1]);
	assert_eq!(
		loaded.graph.get(a_l).unwrap().core.label,
		orig.graph.get(a_o).unwrap().core.label,
		"label"
	);
	assert_eq!(
		loaded.graph.get(a_l).unwrap().core.override_color,
		orig.graph.get(a_o).unwrap().core.override_color,
		"color"
	);
	assert_eq!(
		loaded
			.graph
			.get(a_l)
			.unwrap()
			.core
			.standard_value("param_a_in", -1),
		orig.graph
			.get(a_o)
			.unwrap()
			.core
			.standard_value("param_a_in", -1),
		"value a"
	);
	let keys_o = orig
		.graph
		.get(a_o)
		.unwrap()
		.core
		.keyframe_track("param_a_in", -1)
		.unwrap()
		.keys()
		.to_vec();
	let keys_l = loaded
		.graph
		.get(a_l)
		.unwrap()
		.core
		.keyframe_track("param_a_in", -1)
		.unwrap()
		.keys()
		.to_vec();
	assert_eq!(keys_l.len(), keys_o.len(), "keyframe count");
	for (ko, kl) in keys_o.iter().zip(&keys_l) {
		assert_eq!(kl.time, ko.time, "key time");
		assert_eq!(kl.value.to_double(), ko.value.to_double(), "key value");
		assert_eq!(kl.interpolation, ko.interpolation, "key interpolation");
		assert_eq!(kl.bezier_in, ko.bezier_in, "key bezier in");
		assert_eq!(kl.bezier_out, ko.bezier_out, "key bezier out");
	}

	// Connection a -> b.param_b_in and the link.
	assert_eq!(
		loaded.graph.connected_output(l_ids[2], "param_b_in", -1),
		Some(a_l),
		"connection"
	);
	assert!(loaded.graph.are_linked(a_l, l_ids[2]), "link");

	// Timeline: sequence, track lists, track, clips, footage.
	let mut seq: Option<(NodeId, Vec<NodeId>)> = None;
	for id in loaded.graph.node_ids() {
		let entry = loaded.graph.get(id).unwrap();
		if entry.behavior.type_id() == "org.olivevideoeditor.Olive.sequence" {
			let s = entry
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<SequenceBehavior>())
				.unwrap();
			seq = Some((id, s.track_lists.clone()));
			break;
		}
	}
	let (_seq_id, lists) = seq.expect("sequence present");
	assert_eq!(lists.len(), 3, "video/audio/subtitle lists");
	let list = loaded
		.graph
		.get(lists[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackListBehavior>())
		.unwrap();
	assert_eq!(list.kind, TrackType::Video);
	assert_eq!(list.array_base, 0);
	assert_eq!(list.tracks.len(), 1);
	let track = loaded
		.graph
		.get(list.tracks[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	assert_eq!(track.kind, TrackType::Video);
	assert_eq!(track.blocks.len(), 2);
	let c1 = loaded
		.graph
		.get(track.blocks[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	assert_close(r_to_f(c1.core.length()), 4.0);
	assert_close(r_to_f(c1.core.media_in), 0.0);
	let f1 = loaded
		.graph
		.get(c1.footage.unwrap())
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap();
	assert_eq!(f1.filename, "/a/b.mp4");
	let c2 = loaded
		.graph
		.get(track.blocks[1])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	assert_close(r_to_f(c2.core.length()), 2.0);
	assert_close(r_to_f(c2.core.media_in), 0.4);
	let f2 = loaded
		.graph
		.get(c2.footage.unwrap())
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap();
	assert_eq!(f2.filename, "/a/c.mp4");
}

/// Build a small project with a `projectname` setting (fast saves).
pub(crate) fn build_named_project(name: &str) -> Arc<Mutex<Project>> {
	let project = Project::new();
	{
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		p.settings.insert("projectname".to_string(), name.to_string());
	}
	project
}

/// Save a fresh named project, returning its uuid.
pub(crate) fn save_named_project(backend: &DatabaseBackend, uri: &str, name: &str) -> String {
	let project = build_named_project(name);
	let uuid = uuid_of(&project);
	save_project(backend, &project, uri).unwrap();
	uuid
}
