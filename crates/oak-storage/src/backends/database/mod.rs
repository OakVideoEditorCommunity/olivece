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

//! The database backend (plan M13 §1/§2): SQLite and PostgreSQL share
//! one code path — the same sea-orm entities, diff/save/replay logic and
//! migrations, with the dialect differences (BIGSERIAL vs AUTOINCREMENT,
//! `IF NOT EXISTS` DDL) converged in the connection and migration layer.
//!
//! URI forms:
//! - `oakdb+sqlite:///absolute/path.db[?project=<uuid>]` — local file
//!   database; the optional `project` query selects the library row to
//!   load (default: the most recently modified project).
//! - `oakdb+pg://user:pass@host:5432/db[?project=<uuid>]` — PostgreSQL;
//!   the body is the libpq connection string without a scheme. A `?…`
//!   part is the `?project=` selector only when it carries a `project=`
//!   key — a bare query (e.g. `?sslmode=disable`) belongs to the
//!   connection string and is passed through untouched.
//!
//! The async sea-orm API is driven by a private current-thread tokio
//! runtime so the public surface stays synchronous (plan §6: "后端内嵌
//! 私有 current_thread runtime").
//!
//! ## Storage model (plan §0 — the aggregation-granularity design)
//!
//! A project's state is its node graph plus its settings — nothing else.
//! Every command is persisted as a *diff*: the project is re-serialized
//! in memory (the same per-node XML the `.ove` writer emits) and
//! compared node-by-node against the previous state, so each affected
//! node lands in the journal as one row with its before image
//! (`old_xml`) and after image (`new_xml`). The journal is therefore the
//! persistent undo history: replaying `new_xml` rows forward reconstructs
//! any later state, replaying `old_xml` backwards undoes to any point.
//! Node addressing uses the packed `NodeId::identity`, stored offset by
//! +1 so that journal identity 0 can be reserved for the settings
//! pseudo-node (a real graph slot can have identity 0 — the root folder
//! is slot 0).
//!
//! Snapshots are periodic full payloads that only accelerate loading:
//! the newest snapshot at or before a target seq is the replay base, and
//! the journal rows after it are applied on top. A snapshot is written
//! when the project is dirty and `Storage/SnapshotIntervalSec` (default
//! 600) seconds have passed since the last one; pruning keeps the newest
//! [`SNAPSHOT_KEEP`] copies. Journal retention follows
//! `Storage/JournalRetentionDays` (default 0 = keep everything); rows
//! older than the window are dropped only when the newest snapshot
//! already covers them, so the head state always stays reconstructible.

pub mod entities;
pub mod migration;

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use sea_orm::entity::prelude::*;
use sea_orm::sea_query::Expr;
use sea_orm::{QueryOrder, QuerySelect, Set, TransactionTrait};

use entities::{journal, project, settings, snapshot};
use oak_node::id::NodeId;
use oak_node::project::Project;

use crate::backend::{LoadResult, StorageBackend};
use crate::error::{Error, Result};
use crate::handle::CHandle;
use crate::nodeutil::ProjectArc;
use crate::uri::StorageUri;

/// Journal kind for redo commands (the D1 save path).
pub const KIND_REDO: &str = "redo";
/// Journal kind for undo commands (D2 write-through hook).
pub const KIND_UNDO: &str = "undo";
/// Journal kind for playhead/time jump commands (D2).
pub const KIND_JUMP: &str = "jump";
/// Journal kind for grouped commands (D2 `group_end`).
pub const KIND_GROUP: &str = "group";
/// Journal kind of a first-entry import command (plan §2).
pub const KIND_IMPORT: &str = "import";
/// Journal identity of the settings pseudo-node (plan §1).
pub const SETTINGS_NODE: i64 = 0;
/// Snapshots kept after pruning (plan §0: the newest 3).
pub const SNAPSHOT_KEEP: u64 = 3;
/// The empty settings element (replay base before any settings row).
const EMPTY_SETTINGS: &str = "<settings/>";

/// A parsed `oakdb+…` target: the database location plus the optional
/// `?project=<uuid>` selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DbTarget {
	/// SQLite file database at an absolute path.
	Sqlite {
		/// Absolute database file path.
		path: String,
		/// `?project=` uuid; `None` = most recently modified row.
		project: Option<String>,
	},
	/// PostgreSQL server.
	Pg {
		/// Connection string body (`user:pass@host:port/db`, without a
		/// scheme; may carry its own `?param=…` query when it does not
		/// contain a `project=` key).
		conn: String,
		/// `?project=` uuid; `None` = most recently modified row.
		project: Option<String>,
	},
}

impl DbTarget {
	/// The `?project=` selector.
	fn project(&self) -> Option<&str> {
		match self {
			DbTarget::Sqlite { project, .. } | DbTarget::Pg { project, .. } => {
				project.as_deref()
			}
		}
	}
}

/// The connection-cache key of a parsed target (the database location —
/// an absolute SQLite path or a PostgreSQL connection string body).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum DbKey {
	/// SQLite file database at an absolute path.
	Sqlite(String),
	/// PostgreSQL server (connection string body, see [`DbTarget::Pg`]).
	Pg(String),
}

/// Parse a storage URI into a [`DbTarget`].
///
/// `oakdb+sqlite://` requires an absolute path (E_INVALID otherwise);
/// the body may carry `?project=<uuid>` (no percent-decoding — uuid
/// values contain only hex/braces/hyphens). `oakdb+pg://` requires a
/// non-empty, parseable connection string (E_INVALID otherwise). Unknown
/// `oakdb` variants are E_NO_BACKEND.
pub(crate) fn parse_target(uri: &StorageUri) -> Result<DbTarget> {
	match uri.scheme.as_str() {
		"oakdb+sqlite" => {
			let (path, query) = split_query(&uri.body);
			if !Path::new(path).is_absolute() {
				return Err(Error::Invalid);
			}
			Ok(DbTarget::Sqlite {
				path: path.to_string(),
				project: query_param(query, "project"),
			})
		}
		"oakdb+pg" => {
			let (base, query) = split_query(&uri.body);
			// A `?…` part is the `?project=` selector only when it carries
			// a `project=` key; otherwise (e.g. `?sslmode=disable`) it
			// belongs to the connection string and the whole body passes
			// through.
			let project = match query {
				Some(q) if query_param(Some(q), "project").is_some() => {
					query_param(Some(q), "project")
				}
				_ => None,
			};
			let conn = match &project {
				Some(_) => base.to_string(),
				None => uri.body.clone(),
			};
			if conn.is_empty() || !valid_pg_conn(&conn) {
				return Err(Error::Invalid);
			}
			Ok(DbTarget::Pg { conn, project })
		}
		_ => Err(Error::NoBackend),
	}
}

/// Whether `conn` parses as a PostgreSQL connection string (the libpq
/// URL form, `user:pass@host:port/db[?params]`, with a scheme prepended).
fn valid_pg_conn(conn: &str) -> bool {
	use std::str::FromStr;
	sea_orm::sqlx::postgres::PgConnectOptions::from_str(&format!("postgres://{conn}")).is_ok()
}

/// Split `body` at the first `?` (the query part, if any).
fn split_query(body: &str) -> (&str, Option<&str>) {
	match body.split_once('?') {
		Some((base, query)) => (base, Some(query)),
		None => (body, None),
	}
}

/// Look up `key` in a `k=v&…` query string.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
	let query = query?;
	for pair in query.split('&') {
		if let Some((k, v)) = pair.split_once('=') {
			if k == key {
				return Some(v.to_string());
			}
		}
	}
	None
}

/// Derived per-project manager stats (plan §4 — derived from the node
/// graph when a project is opened, never stored).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectStats {
	/// Timeline duration in milliseconds (longest sequence).
	pub duration_ms: i64,
	/// Total tracks across all sequences.
	pub track_count: i32,
	/// Total clip blocks across all tracks.
	pub clip_count: i32,
	/// Total footage nodes in the graph.
	pub footage_count: i32,
}

/// Derive the manager stats from a live project (plan §4).
///
/// Traversal: sequences contribute their track lists' tracks and blocks;
/// the duration is the longest sequence's end point. Footage nodes are
/// counted directly in the graph. A clip that appears in more than one
/// track is counted once per membership.
pub fn derive_stats(p: &Project) -> ProjectStats {
	use oak_node::block::{ClipBlockBehavior, GapBlockBehavior};
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackListBehavior};

	let mut stats = ProjectStats::default();
	for id in p.graph.node_ids() {
		let entry = match p.graph.get(id) {
			Some(e) => e,
			None => continue,
		};
		if let Some(seq) = entry
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		{
			let mut seq_len = oak_core::Rational::new(0, 1);
			for tl_id in &seq.track_lists {
				let tl = match p.graph.get(*tl_id).and_then(|e| {
					e.behavior
						.as_any()
						.and_then(|a| a.downcast_ref::<TrackListBehavior>())
				}) {
					Some(t) => t,
					None => continue,
				};
				stats.track_count += tl.tracks.len() as i32;
				for t_id in &tl.tracks {
					let t = match p.graph.get(*t_id).and_then(|e| {
						e.behavior
							.as_any()
							.and_then(|a| a.downcast_ref::<TrackBehavior>())
					}) {
						Some(t) => t,
						None => continue,
					};
					for b in &t.blocks {
						let entry = p.graph.get(*b);
						if let Some(c) = entry.and_then(|e| {
							e.behavior
								.as_any()
								.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
						}) {
							stats.clip_count += 1;
							if c.core.out() > seq_len {
								seq_len = c.core.out();
							}
						} else if let Some(g) = entry.and_then(|e| {
							e.behavior
								.as_any()
								.and_then(|a| a.downcast_ref::<GapBlockBehavior>())
						}) {
							if g.core.out() > seq_len {
								seq_len = g.core.out();
							}
						}
					}
				}
			}
			let ms = (seq_len.to_f64() * 1000.0).round() as i64;
			if ms > stats.duration_ms {
				stats.duration_ms = ms;
			}
		} else if entry.behavior.type_id() == "org.olivevideoeditor.Olive.footage" {
			stats.footage_count += 1;
		}
	}
	stats
}

/// One library entry as returned by [`DatabaseBackend::list_projects`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectInfo {
	/// Surrogate row id.
	pub id: i64,
	/// Project uuid (serializer-identical).
	pub uuid: String,
	/// Display name.
	pub name: String,
	/// Payload format version (the serializer's CURRENT_VERSION).
	pub schema_ver: i32,
	/// Row creation time.
	pub created_at: DateTime,
	/// Last-write time (manager sort key).
	pub modified_at: DateTime,
	/// Head command seq.
	pub command_seq: i64,
}

/// The database backend (schemes `oakdb+sqlite` / `oakdb+pg`).
///
/// Connections and the private runtime are created lazily on the first
/// operation; the runtime is a single current-thread tokio runtime that
/// every synchronous call drives with `block_on`. An `op_lock` mutex
/// serializes calls into one backend (single-writer assumption, plan
/// §6) and the SQLite pool itself is capped at one connection, so a
/// transaction owns the only connection for its whole lifetime.
pub struct DatabaseBackend {
	/// Private current-thread tokio runtime (created on first use).
	runtime: OnceLock<tokio::runtime::Runtime>,
	/// Serializes all database operations of this backend.
	op_lock: Mutex<()>,
	/// Open connections by database location (path or PG conn string).
	connections: Mutex<HashMap<DbKey, DatabaseConnection>>,
}

impl DatabaseBackend {
	/// Construct (no connection is opened until first use).
	pub fn new() -> Self {
		DatabaseBackend {
			runtime: OnceLock::new(),
			op_lock: Mutex::new(()),
			connections: Mutex::new(HashMap::new()),
		}
	}

	/// Build the private current-thread runtime.
	fn build_runtime() -> tokio::runtime::Runtime {
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.expect("tokio current_thread runtime")
	}

	/// Run `f` against the database at `key` (an absolute SQLite path or a
	/// PostgreSQL connection string). The op lock is held for the whole
	/// call; the connection is opened and migrated on first use, then
	/// cached.
	fn run<T, F, Fut>(&self, key: DbKey, f: F) -> Result<T>
	where
		F: FnOnce(DatabaseConnection) -> Fut,
		Fut: Future<Output = Result<T>>,
	{
		let _guard = self.op_lock.lock().unwrap_or_else(|e| e.into_inner());
		let runtime = self.runtime.get_or_init(Self::build_runtime);
		runtime.block_on(async {
			let conn = self.connect_cached(&key).await?;
			f(conn).await
		})
	}

	/// Open (and migrate) the database at `key`, caching the connection.
	///
	/// Two writers racing on a fresh SQLite file surface `database is
	/// locked` (SQLITE_BUSY) — most often when a fresh connection's WAL
	/// mode switch collides with the other's, which `busy_timeout` cannot
	/// wait out — so the SQLite connect + migrate are retried a few times
	/// (plan §6: single-writer is the contract; a clean error beats a
	/// silent failure). PostgreSQL connections are never retried (their
	/// errors are not retryable by [`retryable`]).
	async fn connect_cached(&self, key: &DbKey) -> Result<DatabaseConnection> {
		if let Some(conn) = self
			.connections
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(key)
		{
			return Ok(conn.clone());
		}
		let mut attempt = 0;
		let conn = loop {
			let result = async {
				let conn = match key {
					DbKey::Sqlite(path) => connect_sqlite(path).await?,
					DbKey::Pg(conn) => connect_pg(conn).await?,
				};
				migration::migrate(&conn).await?;
				Ok::<_, Error>(conn)
			}
			.await;
			match result {
				Ok(conn) => break conn,
				Err(e) if retryable(&e) && attempt < 3 => {
					attempt += 1;
					tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
				}
				Err(e) => return Err(e),
			}
		};
		self.connections
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(key.clone(), conn.clone());
		Ok(conn)
	}

	/// List library rows, most recently modified first (project manager
	/// data source; per-project stats come from
	/// [`DatabaseBackend::project_stats`], which derives them from the
	/// node graph).
	pub fn list_projects(&self, uri: &StorageUri) -> Result<Vec<ProjectInfo>> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		self.run(key, |conn| async move {
			let models = project::Entity::find()
				.order_by_desc(project::Column::ModifiedAt)
				.all(&conn)
				.await
				.map_err(db_err)?;
			Ok(models
				.into_iter()
				.map(|m| ProjectInfo {
					id: m.id,
					uuid: m.uuid,
					name: m.name,
					schema_ver: m.schema_ver,
					created_at: m.created_at,
					modified_at: m.modified_at,
					command_seq: m.command_seq,
				})
				.collect())
		})
	}

	/// Delete a library row by uuid (cascades settings/snapshots/journal;
	/// the project's data is gone — the manager confirms before calling).
	pub fn delete_project(&self, uri: &StorageUri, uuid: &str) -> Result<()> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		self.run(key, move |conn| async move {
			let tx = conn.begin().await.map_err(db_err)?;
			let res = project::Entity::delete_many()
				.filter(project::Column::Uuid.eq(&uuid))
				.exec(&tx)
				.await
				.map_err(db_err)?;
			tx.commit().await.map_err(db_err)?;
			if res.rows_affected == 0 {
				return Err(Error::NotFound);
			}
			Ok(())
		})
	}

	/// Copy a library row — settings, snapshots and the full journal
	/// history included — under a fresh uuid. `new_name` defaults to
	/// `<name> (copy)`.
	pub fn duplicate_project(
		&self,
		uri: &StorageUri,
		uuid: &str,
		new_name: Option<&str>,
	) -> Result<ProjectInfo> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		let new_name = new_name.map(str::to_string);
		self.run(key, move |conn| async move {
			let tx = conn.begin().await.map_err(db_err)?;
			let src = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid))
				.one(&tx)
				.await
				.map_err(db_err)?
				.ok_or(Error::NotFound)?;
			let new_uuid = new_uuid();
			let name = new_name.unwrap_or_else(|| format!("{} (copy)", src.name));
			let now = chrono::Utc::now().naive_utc();
			let new_id = project::Entity::insert(project::ActiveModel {
				uuid: Set(new_uuid.clone()),
				name: Set(name.clone()),
				schema_ver: Set(src.schema_ver),
				created_at: Set(now),
				modified_at: Set(now),
				command_seq: Set(src.command_seq),
				..Default::default()
			})
			.exec(&tx)
			.await
			.map_err(db_err)?
			.last_insert_id;

			// Settings mirror.
			for s in settings::Entity::find()
				.filter(settings::Column::ProjectId.eq(src.id))
				.all(&tx)
				.await
				.map_err(db_err)?
			{
				settings::Entity::insert(settings::ActiveModel {
					project_id: Set(new_id),
					key: Set(s.key),
					value: Set(s.value),
					..Default::default()
				})
				.exec(&tx)
				.await
				.map_err(db_err)?;
			}
			// Snapshots: the payloads keep their node fragments; the
			// assembled uuid always comes from the project row at load
			// time, so the copy replays under its own uuid.
			for s in snapshot::Entity::find()
				.filter(snapshot::Column::ProjectId.eq(src.id))
				.all(&tx)
				.await
				.map_err(db_err)?
			{
				snapshot::Entity::insert(snapshot::ActiveModel {
					project_id: Set(new_id),
					command_seq: Set(s.command_seq),
					payload: Set(s.payload),
					written_at: Set(s.written_at),
					..Default::default()
				})
				.exec(&tx)
				.await
				.map_err(db_err)?;
			}
			// Journal history (the PK is per-project, so the same
			// seq/node_identity pairs are legal on the new row).
			for r in journal::Entity::find()
				.filter(journal::Column::ProjectId.eq(src.id))
				.all(&tx)
				.await
				.map_err(db_err)?
			{
				journal::Entity::insert(journal::ActiveModel {
					project_id: Set(new_id),
					seq: Set(r.seq),
					node_identity: Set(r.node_identity),
					kind: Set(r.kind),
					old_xml: Set(r.old_xml),
					new_xml: Set(r.new_xml),
					at: Set(r.at),
					..Default::default()
				})
				.exec(&tx)
				.await
				.map_err(db_err)?;
			}
			tx.commit().await.map_err(db_err)?;

			Ok(ProjectInfo {
				id: new_id,
				uuid: new_uuid,
				name,
				schema_ver: src.schema_ver,
				created_at: now,
				modified_at: now,
				command_seq: src.command_seq,
			})
		})
	}

	/// Rename a library row by uuid (E_NOT_FOUND when absent).
	///
	/// A rename is library metadata (the manager's list name); the
	/// in-app project name (`settings["projectname"]`) is untouched.
	pub fn rename_project(&self, uri: &StorageUri, uuid: &str, new_name: &str) -> Result<()> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		let new_name = new_name.to_string();
		self.run(key, move |conn| async move {
			let now = chrono::Utc::now().naive_utc();
			let tx = conn.begin().await.map_err(db_err)?;
			let res = project::Entity::update_many()
				.col_expr(project::Column::Name, Expr::value(new_name))
				.col_expr(project::Column::ModifiedAt, Expr::value(now))
				.filter(project::Column::Uuid.eq(&uuid))
				.exec(&tx)
				.await
				.map_err(db_err)?;
			tx.commit().await.map_err(db_err)?;
			if res.rows_affected == 0 {
				return Err(Error::NotFound);
			}
			Ok(())
		})
	}

	/// Export a library project to a `.ove` file, using the ove-xml
	/// backend's save semantics (same serializer output, file container;
	/// assembled in memory, nothing is written to the library). The
	/// target URI must be a `file://` URI.
	pub fn export_to_file(&self, uri: &StorageUri, uuid: &str, file_uri: &StorageUri) -> Result<()> {
		if file_uri.scheme != "file" {
			return Err(Error::Invalid);
		}
		let project = self.load_project_by_uuid(uri, uuid)?;
		let backend = crate::backends::ove_xml::OveXmlBackend::new();
		backend.save_project(&project, file_uri, 0)
	}

	/// Import a `.ove`/`.otio`/`.fcpxml` file as a new library row
	/// (plan §2 "导入"): the file backend parses it into a project, a
	/// fresh uuid is assigned, and the first save writes the whole
	/// project as one `kind='import'` command (seq 1). Returns the new
	/// row's uuid.
	pub fn import_from_file(&self, uri: &StorageUri, file_uri: &StorageUri) -> Result<String> {
		if file_uri.scheme != "file" {
			return Err(Error::Invalid);
		}
		let backend = crate::registry::Registry::global().resolve(file_uri)?;
		// The file backend's `load` returns its project as a handle (the
		// facade-facing trait form); convert it to the boxed project the
		// crate uses internally, and release the temporary handle.
		let result = backend.load(file_uri)?;
		let handle = result.project;
		if handle.is_null() {
			return Err(Error::Format(format!(
				"cannot import: backend reported info code {}",
				result.version_info
			)));
		}
		let uuid = {
			let fresh = new_uuid();
			unsafe { crate::nodeutil::project_arc(&handle)? }
				.lock()
				.map_err(|_| Error::State)?
				.uuid = fresh.clone();
			fresh
		};
		let arc = unsafe { crate::nodeutil::project_arc(&handle)? };
		let outcome = self.save_project(&arc, uri, 0).map(|()| uuid.clone());
		if let Some(release) = handle.release {
			unsafe { release(handle.ctx) };
		}
		outcome
	}

	/// Force a snapshot of the project at its head seq (the D2 snapshot
	/// thread and the exit flush call this; tests use it to exercise the
	/// snapshot+replay path). No-op when a snapshot already exists at
	/// the head seq.
	pub fn snapshot(&self, uri: &StorageUri, uuid: &str) -> Result<()> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		self.run(key, move |conn| async move {
			let model = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid))
				.one(&conn)
				.await
				.map_err(db_err)?
				.ok_or(Error::NotFound)?;
			let now = chrono::Utc::now().naive_utc();
			let tx = conn.begin().await.map_err(db_err)?;
			write_snapshot(&tx, model.id, &model.uuid, model.command_seq, now).await?;
			prune_snapshots(&tx, model.id).await?;
			tx.commit().await.map_err(db_err)?;
			Ok(())
		})
	}

	/// Load the project state at an arbitrary command seq — the
	/// persistent undo history (plan §0): a snapshot at or before `seq`
	/// is replayed forward with the journal rows up to `seq`. `seq` 0 is
	/// the empty project. E_INVALID when `seq` is out of range.
	///
	/// Returns the project as an owned oaknode handle (the facade-facing
	/// form — the engine's integration tests consume it this way).
	pub fn load_at(&self, uri: &StorageUri, uuid: &str, seq: i64) -> Result<CHandle> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		let project = self.run(key, move |conn| async move {
			let model = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid))
				.one(&conn)
				.await
				.map_err(db_err)?
				.ok_or(Error::NotFound)?;
			if seq < 0 || seq > model.command_seq {
				return Err(Error::Invalid);
			}
			let xml = assemble_at(&conn, model.id, &model.uuid, seq).await?;
			crate::nodeutil::serializer_load(&xml)
		})?;
		Ok(crate::nodeutil::make_project_owned(project))
	}

	/// The manager stats of a library project (plan §4): the head state
	/// is replayed and the stats derived from the node graph.
	pub fn project_stats(&self, uri: &StorageUri, uuid: &str) -> Result<ProjectStats> {
		let project = self.load_project_by_uuid(uri, uuid)?;
		let guard = project.lock().map_err(|_| Error::State)?;
		Ok(derive_stats(&guard))
	}

	/// Load the project payload of the library row `uuid` as the boxed
	/// project (the head state).
	fn load_project_by_uuid(&self, uri: &StorageUri, uuid: &str) -> Result<ProjectArc> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let uuid = uuid.to_string();
		self.run(key, move |conn| async move {
			let model = project::Entity::find()
				.filter(project::Column::Uuid.eq(&uuid))
				.one(&conn)
				.await
				.map_err(db_err)?
				.ok_or(Error::NotFound)?;
			let xml = assemble_at(&conn, model.id, &model.uuid, model.command_seq).await?;
			crate::nodeutil::serializer_load(&xml)
		})
	}

	/// Load the head state of the library's selected project (the
	/// `?project=` uuid, or the most recently modified row) as the boxed
	/// project. The Rust-typed inner load: the facade-facing trait `load`
	/// wraps the result in an owned handle.
	pub fn load_project(&self, uri: &StorageUri) -> Result<ProjectArc> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		let project = target.project().map(str::to_string);
		self.run(key, move |conn| async move {
			let model = pick_project(&conn, project.as_deref()).await?;
			let xml = assemble_at(&conn, model.id, &model.uuid, model.command_seq).await?;
			crate::nodeutil::serializer_load(&xml)
		})
	}

	/// Save a project (already read out of its handle) to the library,
	/// journaling the diff. The Rust-typed inner save: the facade-facing
	/// trait `save` converts the project handle and forwards here.
	pub fn save_project(
		&self,
		project: &ProjectArc,
		uri: &StorageUri,
		_options: u32,
	) -> Result<()> {
		let target = parse_target(uri)?;
		let key = db_key_of(&target);
		// Serialize under the project lock (the same per-node writer the
		// `.ove` backend uses — one serialization truth).
		let (uuid, name, nodes, settings_xml, settings_map) = {
			let guard = project.lock().map_err(|_| Error::State)?;
			let uuid = guard.uuid.clone();
			let name = project_display_name(&guard);
			let (nodes, settings_xml, settings_map) = serialize_project_state(&guard)?;
			(uuid, name, nodes, settings_xml, settings_map)
		};
		self.run(key, move |conn| async move {
			save_tx(&conn, &uuid, &name, &nodes, &settings_xml, &settings_map).await
		})?;
		Ok(())
	}
}

impl Default for DatabaseBackend {
	fn default() -> Self {
		Self::new()
	}
}

impl StorageBackend for DatabaseBackend {
	fn name(&self) -> &'static str {
		"oakdb"
	}

	fn uri_scheme(&self) -> &'static str {
		"oakdb"
	}

	fn can_handle(&self, uri: &StorageUri) -> bool {
		// `oakdb://` (no sub-scheme) is deliberately not claimed — the
		// legacy URI is unresolvable (E_NO_BACKEND), per the contract
		// tests in `tests/storage_test.rs`.
		uri.scheme == "oakdb+sqlite" || uri.scheme == "oakdb+pg"
	}

	fn load(&self, uri: &StorageUri) -> Result<LoadResult> {
		// Facade boundary: box the loaded project into an owned handle.
		Ok(LoadResult::success(crate::nodeutil::make_project_owned(
			self.load_project(uri)?,
		)))
	}

	fn save(&self, project: CHandle, uri: &StorageUri, options: u32) -> Result<()> {
		// Facade boundary: convert the project handle to the boxed
		// project, then run the Rust-typed save.
		let arc = unsafe { crate::nodeutil::project_arc(&project)? };
		self.save_project(&arc, uri, options)
	}
}

/// The connection-cache key of a target (SQLite path or PG conn string).
fn db_key_of(target: &DbTarget) -> DbKey {
	match target {
		DbTarget::Sqlite { path, .. } => DbKey::Sqlite(path.clone()),
		DbTarget::Pg { conn, .. } => DbKey::Pg(conn.clone()),
	}
}

/// Display name for a project on first entry: the `projectname` setting
/// (what the editor stores), falling back to "Untitled Project".
fn project_display_name(p: &Project) -> String {
	p.settings
		.get("projectname")
		.cloned()
		.unwrap_or_else(|| "Untitled Project".to_string())
}

/// Open the SQLite database at `path` (WAL + busy timeout + FK
/// enforcement; a missing parent directory is an I/O error).
async fn connect_sqlite(path: &str) -> Result<DatabaseConnection> {
	use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
	let options = SqliteConnectOptions::new()
		.filename(path)
		.create_if_missing(true)
		.journal_mode(SqliteJournalMode::Wal)
		.busy_timeout(Duration::from_secs(5))
		.foreign_keys(true);
	let pool = SqlitePoolOptions::new()
		.max_connections(1)
		.connect_with(options)
		.await
		.map_err(|e| Error::Io(format!("cannot open sqlite database '{path}': {e}")))?;
	Ok(DatabaseConnection::from(pool))
}

/// Open the PostgreSQL database at `conn` (a connection string body,
/// e.g. `user:pass@host:5432/db`; a scheme is prepended for sqlx).
///
/// The server is first probed with a single, non-retrying connection
/// attempt: the pool retries refused connections with backoff for the
/// whole acquire timeout, which would hold a dead server for seconds.
/// A single `connect()` surfaces the refusal immediately, so a dead /
/// unreachable host fails fast with a clean E_IO.
async fn connect_pg(conn: &str) -> Result<DatabaseConnection> {
	use std::str::FromStr;
	use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
	use sea_orm::sqlx::ConnectOptions;
	let url = format!("postgres://{conn}");
	let options = PgConnectOptions::from_str(&url)
		.map_err(|e| Error::Io(format!("invalid postgres connection string: {e}")))?;
	options
		.connect()
		.await
		.map_err(|e| Error::Io(format!("cannot connect to postgres database: {e}")))?;
	let pool = PgPoolOptions::new()
		.max_connections(1)
		.acquire_timeout(Duration::from_secs(10))
		.connect_with(options)
		.await
		.map_err(|e| Error::Io(format!("cannot connect to postgres database: {e}")))?;
	Ok(DatabaseConnection::from(pool))
}

/// Whether an error is worth retrying (SQLite BUSY — plan §6 note in
/// [`DatabaseBackend::run`]).
fn retryable(e: &Error) -> bool {
	if let Error::Io(msg) = e {
		msg.contains("database is locked") || msg.contains("is busy")
	} else {
		false
	}
}

/// The project row to load: `?project=` uuid, or the most recently
/// modified row. E_NOT_FOUND when the library is empty / uuid unknown.
async fn pick_project(conn: &DatabaseConnection, uuid: Option<&str>) -> Result<project::Model> {
	let model = match uuid {
		Some(uuid) => project::Entity::find()
			.filter(project::Column::Uuid.eq(uuid))
			.one(conn)
			.await
			.map_err(db_err)?,
		None => project::Entity::find()
			.order_by_desc(project::Column::ModifiedAt)
			.one(conn)
			.await
			.map_err(db_err)?,
	};
	model.ok_or(Error::NotFound)
}

/// Map a sea-orm error to a crate error (I/O class; the context string
/// is log-only per the error module's contract).
fn db_err(e: sea_orm::DbErr) -> Error {
	Error::Io(format!("database error: {e}"))
}

/// A fresh uuid in the project's `{…}` text format (mirrors the
/// oaknode `project::generate_uuid` splitmix64 generator, which is
/// private to that crate; identical shape so the serializer round-trips
/// it unchanged).
fn new_uuid() -> String {
	use std::time::{SystemTime, UNIX_EPOCH};
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_nanos() as u64)
		.unwrap_or(0);
	let mut seed = nanos ^ 0x9E3779B97F4A7C15;
	let mut next = move || {
		seed = seed.wrapping_add(0x9E3779B97F4A7C15);
		let mut z = seed;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
		z ^ (z >> 31)
	};
	let mut b = [0u8; 16];
	for chunk in b.chunks_mut(8) {
		let r = next().to_le_bytes();
		chunk.copy_from_slice(&r);
	}
	b[6] = (b[6] & 0x0F) | 0x40; // version 4
	b[8] = (b[8] & 0x3F) | 0x80; // variant 1
	format!(
		"{{{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
		b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
		b[14], b[15]
	)
}

// ---------------------------------------------------------------------------
// Serialization / diff / replay (plan §0)
// ---------------------------------------------------------------------------

/// Serialize the project's whole state: per-node XML fragments keyed by
/// real graph identity, the settings element, and the settings map.
/// Every fragment is self-contained `<node …>…</node>` text the
/// serializer can load back, byte-identical to what a `.ove` write
/// embeds inside `<nodes>`.
fn serialize_project_state(
	p: &Project,
) -> Result<(HashMap<u64, String>, String, HashMap<String, String>)> {
	let mut nodes = HashMap::new();
	for id in p.graph.node_ids() {
		nodes.insert(id.identity(), serialize_node_xml(p, id)?);
	}
	let settings_map = p.settings.clone();
	let settings = settings_xml_from_map(&settings_map);
	Ok((nodes, settings, settings_map))
}

/// Serialize one node as a standalone `<node>…</node>` element (the
/// same writer `serializer::save` uses inside `<nodes>`).
fn serialize_node_xml(p: &Project, id: NodeId) -> Result<String> {
	use oak_node::serializer::{XmlWrite, XmlWriterBridge};
	let entry = p.graph.get(id).ok_or(Error::NotFound)?;
	let type_id = entry.behavior.type_id().to_string();
	let connections: Vec<(NodeId, String, i32)> = p
		.graph
		.output_connections_all()
		.into_iter()
		.filter(|(_, to, _, _)| *to == id)
		.map(|(from, _, input, element)| (from, input, element))
		.collect();
	let mut w = XmlWriterBridge::new()
		.ok_or_else(|| Error::Failed("oak_core XML writer unavailable".to_string()))?;
	w.start_element("node");
	oak_node::serializer::save_node(&mut w, &entry.core, &*entry.behavior, id, &type_id, &connections)
		.map_err(|e| Error::Format(e.to_string()))?;
	w.end_element();
	Ok(w.output())
}

/// Serialize a settings map as the `<settings>…</settings>` element
/// (sorted keys, exactly like `serializer::save`).
fn settings_xml_from_map(settings: &HashMap<String, String>) -> String {
	use oak_node::serializer::{XmlWrite, XmlWriterBridge};
	let mut w = XmlWriterBridge::new().expect("oak_core XML writer");
	w.start_element("settings");
	let mut keys: Vec<&String> = settings.keys().collect();
	keys.sort();
	for k in keys {
		w.text_element(k, settings.get(k).unwrap_or(&String::new()));
	}
	w.end_element();
	w.output()
}

/// Assemble a full `<project>` document from a node map and a settings
/// element. The fragments are already-escaped XML, so this is plain text
/// assembly; the uuid (the only foreign text) is escaped defensively.
fn assemble_xml(uuid: &str, nodes: &HashMap<u64, String>, settings: &str) -> String {
	let mut out = String::with_capacity(512 + nodes.values().map(String::len).sum::<usize>());
	out.push_str("<project version=\"1\"><uuid>");
	out.push_str(&xml_escape(uuid));
	out.push_str("</uuid><nodes>");
	let mut ids: Vec<&u64> = nodes.keys().collect();
	ids.sort();
	for id in ids {
		out.push_str(nodes.get(id).expect("key present"));
	}
	out.push_str("</nodes>");
	out.push_str(settings);
	out.push_str("</project>");
	out
}

/// Escape the three XML-significant characters (uuid text is normally
/// `{hex}`, but imported files may carry anything).
fn xml_escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

/// Diff two node maps into journal rows (plan §0). `prev`/`cur` are
/// keyed by real graph identity; the returned rows use journal
/// identities (real identity + 1, reserving 0 for settings).
fn diff_rows(
	prev: &HashMap<u64, String>,
	cur: &HashMap<u64, String>,
) -> Vec<(i64, Option<String>, Option<String>)> {
	let mut rows = Vec::new();
	for (id, xml) in cur {
		match prev.get(id) {
			Some(old) if old != xml => {
				rows.push(((*id as i64) + 1, Some(old.clone()), Some(xml.clone())));
			}
			Some(_) => {}
			None => rows.push(((*id as i64) + 1, None, Some(xml.clone()))),
		}
	}
	for (id, old) in prev {
		if !cur.contains_key(id) {
			rows.push(((*id as i64) + 1, Some(old.clone()), None));
		}
	}
	rows.sort();
	rows
}

/// Replay the state at `target_seq` into a node map + settings element
/// (plan §0): the newest snapshot at or before `target_seq` (nothing if
/// absent) plus the journal rows after it, forward-applying `new_xml`.
/// A node whose row has no `new_xml` was deleted; a settings row
/// (identity 0) replaces the settings element wholesale.
async fn replay_state<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	target_seq: i64,
) -> Result<(HashMap<u64, String>, String)> {
	let snap = snapshot::Entity::find()
		.filter(snapshot::Column::ProjectId.eq(project_id))
		.filter(snapshot::Column::CommandSeq.lte(target_seq))
		.order_by_desc(snapshot::Column::CommandSeq)
		.one(conn)
		.await
		.map_err(db_err)?;
	let (mut nodes, mut settings, base_seq) = match &snap {
		Some(s) => (
			extract_nodes(&s.payload),
			extract_settings(&s.payload),
			s.command_seq,
		),
		None => (HashMap::new(), EMPTY_SETTINGS.to_string(), 0),
	};
	let rows = journal::Entity::find()
		.filter(journal::Column::ProjectId.eq(project_id))
		.filter(journal::Column::Seq.gt(base_seq))
		.filter(journal::Column::Seq.lte(target_seq))
		.order_by_asc(journal::Column::Seq)
		.order_by_asc(journal::Column::NodeIdentity)
		.all(conn)
		.await
		.map_err(db_err)?;
	for r in rows {
		if r.node_identity == SETTINGS_NODE {
			settings = r.new_xml.clone().unwrap_or_else(|| EMPTY_SETTINGS.to_string());
		} else if let Some(new) = r.new_xml {
			nodes.insert((r.node_identity - 1) as u64, new);
		} else {
			nodes.remove(&((r.node_identity - 1) as u64));
		}
	}
	Ok((nodes, settings))
}

/// Assemble the full project XML for a state.
async fn assemble_at<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	uuid: &str,
	target_seq: i64,
) -> Result<String> {
	let (nodes, settings) = replay_state(conn, project_id, target_seq).await?;
	Ok(assemble_xml(uuid, &nodes, &settings))
}

/// Extract the per-node fragments of an assembled project document into
/// an identity-keyed map. The document is produced by
/// [`assemble_xml`]/[`serializer::save`], whose `<nodes>` children are
/// self-contained `<node …>…</node>` elements (node bodies never nest
/// `<node>` elements, so the first `</node>` closes each fragment).
fn extract_nodes(xml: &str) -> HashMap<u64, String> {
	let mut map = HashMap::new();
	let mut rest = xml;
	while let Some(rel) = rest.find("<node") {
		let tag = &rest[rel..];
		// `<node` must start a node element: the next byte is a space
		// (`<node version=…`), `>` (`<node>`) or `/` (`<node/>`); skip
		// lookalikes such as `<nodes>`.
		match tag.as_bytes().get(5).copied() {
			Some(b' ' | b'>' | b'/') => {}
			_ => {
				rest = &tag[5..];
				continue;
			}
		}
		let body = &tag[5..];
		match body.find("</node>") {
			Some(end) => {
				let frag = &tag[..5 + end + 7];
				if let Some(identity) = fragment_identity(frag) {
					map.insert(identity, frag.to_string());
				}
				rest = &body[end + 7..];
			}
			None => break,
		}
	}
	map
}

/// The `ptr="…"` identity of a node fragment (the first `ptr=` in the
/// start tag; node bodies never contain `ptr="`).
fn fragment_identity(frag: &str) -> Option<u64> {
	let p = frag.find("ptr=\"")?;
	let after = &frag[p + 5..];
	let end = after.find('"')?;
	after[..end].parse().ok()
}

/// Extract the settings element of an assembled project document
/// (fallback: the empty settings element).
fn extract_settings(xml: &str) -> String {
	if let Some(s) = xml.find("<settings>") {
		let after = &xml[s + 10..];
		if let Some(e) = after.find("</settings>") {
			return xml[s..s + 10 + e + 11].to_string();
		}
	}
	EMPTY_SETTINGS.to_string()
}

// ---------------------------------------------------------------------------
// Command write path
// ---------------------------------------------------------------------------

/// Write one command (plan §2): first entry is a `kind='import'`
/// command carrying every node; later entries diff against the replayed
/// head and write only the affected nodes as `kind='redo'`. The
/// settings table is mirrored, snapshots follow
/// `Storage/SnapshotIntervalSec`, and the journal is pruned to the
/// retention window — all in one transaction.
///
/// A concurrent writer's short WAL lock window can surface `database is
/// locked` (SQLITE_BUSY) despite `busy_timeout` (the lock is only
/// acquired mid-transaction); such attempts are retried a few times
/// (plan §6: single-writer is the contract; a clean error beats a
/// silent failure).
async fn save_tx(
	conn: &DatabaseConnection,
	uuid: &str,
	name: &str,
	nodes: &HashMap<u64, String>,
	settings_xml: &str,
	settings_map: &HashMap<String, String>,
) -> Result<i64> {
	let mut attempt = 0;
	loop {
		match save_tx_once(conn, uuid, name, nodes, settings_xml, settings_map).await {
			Ok(seq) => return Ok(seq),
			Err(e) if retryable(&e) && attempt < 3 => {
				attempt += 1;
				tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
			}
			Err(e) => return Err(e),
		}
	}
}

/// The one-attempt body of [`save_tx`].
async fn save_tx_once(
	conn: &DatabaseConnection,
	uuid: &str,
	name: &str,
	nodes: &HashMap<u64, String>,
	settings_xml: &str,
	settings_map: &HashMap<String, String>,
) -> Result<i64> {
	let now = chrono::Utc::now().naive_utc();
	let schema_ver = oak_node::serializer::CURRENT_VERSION.0 as i32;

	let tx = conn.begin().await.map_err(db_err)?;

	let existing = project::Entity::find()
		.filter(project::Column::Uuid.eq(uuid))
		.one(&tx)
		.await
		.map_err(db_err)?;
	let (project_id, head_seq) = match existing {
		Some(m) => (m.id, m.command_seq),
		None => {
			let res = project::Entity::insert(project::ActiveModel {
				uuid: Set(uuid.to_string()),
				name: Set(name.to_string()),
				schema_ver: Set(schema_ver),
				created_at: Set(now),
				modified_at: Set(now),
				command_seq: Set(0),
				..Default::default()
			})
			.exec(&tx)
			.await
			.map_err(db_err)?;
			(res.last_insert_id, 0)
		}
	};

	let seq: i64;
	if head_seq == 0 {
		// First entry (import): one command with every node plus the
		// settings pseudo-node; all old images are NULL.
		seq = 1;
		let mut rows: Vec<(i64, Option<String>, Option<String>)> = Vec::new();
		for (id, xml) in nodes {
			rows.push(((*id as i64) + 1, None, Some(xml.clone())));
		}
		rows.push((SETTINGS_NODE, None, Some(settings_xml.to_string())));
		rows.sort();
		write_journal_rows(&tx, project_id, seq, KIND_IMPORT, &rows, now).await?;
	} else {
		// Diff against the replayed head.
		let (prev_nodes, prev_settings) = replay_state(&tx, project_id, head_seq).await?;
		let mut rows = diff_rows(&prev_nodes, nodes);
		if prev_settings != *settings_xml {
			rows.push((
				SETTINGS_NODE,
				Some(prev_settings),
				Some(settings_xml.to_string()),
			));
		}
		rows.sort();
		if rows.is_empty() {
			// No-op save: touch modified_at, keep the head seq.
			project::Entity::update(project::ActiveModel {
				id: Set(project_id),
				modified_at: Set(now),
				..Default::default()
			})
			.exec(&tx)
			.await
			.map_err(db_err)?;
			tx.commit().await.map_err(db_err)?;
			return Ok(head_seq);
		}
		seq = head_seq + 1;
		write_journal_rows(&tx, project_id, seq, KIND_REDO, &rows, now).await?;
	}

	project::Entity::update(project::ActiveModel {
		id: Set(project_id),
		name: Set(name.to_string()),
		schema_ver: Set(schema_ver),
		modified_at: Set(now),
		command_seq: Set(seq),
		..Default::default()
	})
	.exec(&tx)
	.await
	.map_err(db_err)?;

	replace_settings(&tx, project_id, settings_map).await?;
	maybe_snapshot(&tx, project_id, uuid, seq, now).await?;
	retention_prune(&tx, project_id, now).await?;

	tx.commit().await.map_err(db_err)?;
	Ok(seq)
}

/// Insert the journal rows of one command.
async fn write_journal_rows<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	seq: i64,
	kind: &str,
	rows: &[(i64, Option<String>, Option<String>)],
	now: DateTime,
) -> Result<()> {
	for (node_identity, old, new) in rows {
		journal::Entity::insert(journal::ActiveModel {
			project_id: Set(project_id),
			seq: Set(seq),
			node_identity: Set(*node_identity),
			kind: Set(kind.to_string()),
			old_xml: Set(old.clone()),
			new_xml: Set(new.clone()),
			at: Set(now),
			..Default::default()
		})
		.exec(conn)
		.await
		.map_err(db_err)?;
	}
	Ok(())
}

/// Mirror the current settings into the settings table (replace-all).
async fn replace_settings<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	settings: &HashMap<String, String>,
) -> Result<()> {
	settings::Entity::delete_many()
		.filter(settings::Column::ProjectId.eq(project_id))
		.exec(conn)
		.await
		.map_err(db_err)?;
	for (k, v) in settings {
		settings::Entity::insert(settings::ActiveModel {
			project_id: Set(project_id),
			key: Set(k.clone()),
			value: Set(v.clone()),
			..Default::default()
		})
		.exec(conn)
		.await
		.map_err(db_err)?;
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Snapshots and retention (plan §0/§1)
// ---------------------------------------------------------------------------

/// Snapshot policy: write a full payload when the project is dirty and
/// `Storage/SnapshotIntervalSec` (default 600) seconds have passed since
/// the last snapshot (an interval ≤ 0 snapshots on every dirty save).
async fn maybe_snapshot<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	uuid: &str,
	head_seq: i64,
	now: DateTime,
) -> Result<()> {
	let last = snapshot::Entity::find()
		.filter(snapshot::Column::ProjectId.eq(project_id))
		.order_by_desc(snapshot::Column::CommandSeq)
		.one(conn)
		.await
		.map_err(db_err)?;
	let interval = oak_core::configstore::ConfigStore::instance()
		.get_int(Some("Storage"), "SnapshotIntervalSec", 600);
	let due = match &last {
		None => true,
		Some(s) => {
			s.command_seq < head_seq
				&& (interval <= 0
					|| now.signed_duration_since(s.written_at).num_seconds() >= interval as i64)
		}
	};
	if due {
		write_snapshot(conn, project_id, uuid, head_seq, now).await?;
		prune_snapshots(conn, project_id).await?;
	}
	Ok(())
}

/// Write the full project payload at `seq` (no-op when a snapshot at
/// that seq already exists).
async fn write_snapshot<C: ConnectionTrait>(
	conn: &C,
	project_id: i64,
	uuid: &str,
	seq: i64,
	now: DateTime,
) -> Result<()> {
	if snapshot::Entity::find_by_id((project_id, seq))
		.one(conn)
		.await
		.map_err(db_err)?
		.is_some()
	{
		return Ok(());
	}
	let payload = assemble_at(conn, project_id, uuid, seq).await?;
	snapshot::Entity::insert(snapshot::ActiveModel {
		project_id: Set(project_id),
		command_seq: Set(seq),
		payload: Set(payload),
		written_at: Set(now),
		..Default::default()
	})
	.exec(conn)
	.await
	.map_err(db_err)?;
	Ok(())
}

/// Keep the newest [`SNAPSHOT_KEEP`] snapshots, drop the rest.
async fn prune_snapshots<C: ConnectionTrait>(conn: &C, project_id: i64) -> Result<()> {
	let keep: Vec<i64> = snapshot::Entity::find()
		.filter(snapshot::Column::ProjectId.eq(project_id))
		.order_by_desc(snapshot::Column::CommandSeq)
		.limit(SNAPSHOT_KEEP as u64)
		.all(conn)
		.await
		.map_err(db_err)?
		.into_iter()
		.map(|s| s.command_seq)
		.collect();
	if keep.is_empty() {
		return Ok(());
	}
	snapshot::Entity::delete_many()
		.filter(snapshot::Column::ProjectId.eq(project_id))
		.filter(snapshot::Column::CommandSeq.is_not_in(keep))
		.exec(conn)
		.await
		.map_err(db_err)?;
	Ok(())
}

/// Journal retention window (`Storage/JournalRetentionDays`, default 0 =
/// keep everything). Rows older than the window are dropped only when
/// the newest snapshot already covers them, so the head state stays
/// reconstructible from the snapshot plus the remaining rows (plan §1:
/// history beyond the window is forfeit).
async fn retention_prune<C: ConnectionTrait>(conn: &C, project_id: i64, now: DateTime) -> Result<()> {
	let days = oak_core::configstore::ConfigStore::instance()
		.get_int(Some("Storage"), "JournalRetentionDays", 0);
	if days <= 0 {
		return Ok(());
	}
	let snap_seq = snapshot::Entity::find()
		.filter(snapshot::Column::ProjectId.eq(project_id))
		.order_by_desc(snapshot::Column::CommandSeq)
		.one(conn)
		.await
		.map_err(db_err)?
		.map(|s| s.command_seq);
	if let Some(snap_seq) = snap_seq {
		let cutoff = now - chrono::Duration::days(days as i64);
		journal::Entity::delete_many()
			.filter(journal::Column::ProjectId.eq(project_id))
			.filter(journal::Column::Seq.lte(snap_seq))
			.filter(journal::Column::At.lt(cutoff))
			.exec(conn)
			.await
			.map_err(db_err)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn uri(s: &str) -> StorageUri {
		StorageUri::parse(s).unwrap()
	}

	/// An absolute database path for the platform: `/tmp/...` is not
	/// absolute on Windows (no drive prefix), and parse_target rejects
	/// relative paths.
	#[cfg(unix)]
	const ABS_DB: &str = "/tmp/lib.db";
	#[cfg(windows)]
	const ABS_DB: &str = "C:/tmp/lib.db";

	#[test]
	fn parse_target_sqlite_absolute() {
		let t = parse_target(&uri(&format!("oakdb+sqlite://{ABS_DB}"))).unwrap();
		assert_eq!(
			t,
			DbTarget::Sqlite {
				path: ABS_DB.to_string(),
				project: None
			}
		);
	}

	#[test]
	fn parse_target_sqlite_project_query() {
		let t = parse_target(&uri(&format!(
			"oakdb+sqlite://{ABS_DB}?project={{abc-123}}"
		)))
		.unwrap();
		assert_eq!(
			t,
			DbTarget::Sqlite {
				path: ABS_DB.to_string(),
				project: Some("{abc-123}".to_string())
			}
		);
	}

	#[test]
	fn parse_target_sqlite_relative_rejected() {
		assert!(matches!(
			parse_target(&uri("oakdb+sqlite://relative.db")),
			Err(Error::Invalid)
		));
		assert!(matches!(
			parse_target(&uri("oakdb+sqlite://")),
			Err(Error::Invalid)
		));
	}

	#[test]
	fn parse_target_pg_conn_string() {
		let t = parse_target(&uri("oakdb+pg://user:pass@host:5432/db")).unwrap();
		assert_eq!(
			t,
			DbTarget::Pg {
				conn: "user:pass@host:5432/db".to_string(),
				project: None
			}
		);
		// A `?project=` query is the row selector, not a conn param.
		let t = parse_target(&uri("oakdb+pg://user@host/db?project={abc-123}")).unwrap();
		assert_eq!(
			t,
			DbTarget::Pg {
				conn: "user@host/db".to_string(),
				project: Some("{abc-123}".to_string())
			}
		);
		// A query without a `project=` key belongs to the conn string
		// (e.g. `?sslmode=disable` passes through untouched).
		let t = parse_target(&uri("oakdb+pg://user@host/db?sslmode=disable")).unwrap();
		assert_eq!(
			t,
			DbTarget::Pg {
				conn: "user@host/db?sslmode=disable".to_string(),
				project: None
			}
		);
	}

	#[test]
	fn parse_target_pg_invalid_rejected() {
		// Empty / unparseable connection strings are E_INVALID (clean, no
		// network touched), matching the sqlite relative-path rule.
		assert!(matches!(
			parse_target(&uri("oakdb+pg://")),
			Err(Error::Invalid)
		));
		assert!(matches!(
			parse_target(&uri("oakdb+pg://?project={x}")),
			Err(Error::Invalid)
		));
		assert!(matches!(
			parse_target(&uri("oakdb+pg://user@")),
			Err(Error::Invalid)
		));
		assert!(matches!(
			parse_target(&uri("oakdb+pg://not a url")),
			Err(Error::Invalid)
		));
	}

	#[test]
	fn parse_target_unknown_scheme() {
		assert!(matches!(
			parse_target(&uri("oakdb://x")),
			Err(Error::NoBackend)
		));
		assert!(matches!(
			parse_target(&uri("file:///a.ove")),
			Err(Error::NoBackend)
		));
	}

	#[test]
	fn node_fragment_round_trip() {
		// serialize_project_state → assemble_xml → extract_nodes must be
		// byte-stable for a real project.
		let p = Project::new();
		{
			let mut guard = p.lock().unwrap();
			guard.initialize().unwrap();
			let (core, behavior) = (oak_node::factory::Factory::global()
				.find("org.olivevideoeditor.Olive.math")
				.unwrap()
				.create)();
			let a = guard.graph.add_node(core, behavior);
			guard
				.graph
				.get_mut(a)
				.unwrap()
				.core
				.set_standard_value("param_a_in", -1, oak_node::value::NodeValue::Float(2.5));
		}
		let guard = p.lock().unwrap();
		let (nodes, settings, _) = serialize_project_state(&guard).unwrap();
		let xml = assemble_xml(&guard.uuid, &nodes, &settings);
		let back = extract_nodes(&xml);
		assert_eq!(back, nodes, "fragment extraction is byte-stable");
		assert_eq!(extract_settings(&xml), settings);
		assert!(xml.contains(&guard.uuid));
		// And the assembled document loads.
		let loaded = oak_node::serializer::load(&xml).unwrap();
		let loaded_guard = loaded.lock().unwrap();
		assert_eq!(loaded_guard.uuid, guard.uuid);
		assert_eq!(loaded_guard.graph.node_count(), guard.graph.node_count());
	}
}
