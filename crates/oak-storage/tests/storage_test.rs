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

//! oakstorage contract tests (M10 §4 mapping), calling the public Rust
//! API: the backend registry, the `StorageBackend` trait, `StorageUri`
//! and the `Session` shell. The former `ffi.rs`/`bridge::node.rs` C ABI
//! is gone; save/load now go through `Registry::global().resolve(uri)`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
use oak_storage::backend::{LoadResult, StorageBackend};
use oak_storage::error::{
	OAKSTORAGE_E_FORMAT, OAKSTORAGE_E_INVALID, OAKSTORAGE_E_IO, OAKSTORAGE_E_NO_BACKEND,
	OAKSTORAGE_E_STATE, OAKSTORAGE_OK, OAKSTORAGE_TOO_NEW, OAKSTORAGE_TOO_OLD,
	OAKSTORAGE_UNKNOWN_VERSION,
};
use oak_storage::handle::CHandle;
use oak_storage::nodeutil::{make_project_owned, project_arc};
use oak_storage::registry::Registry;
use oak_storage::session::Session;
use oak_storage::uri::StorageUri;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn file_uri(path: &Path) -> String {
	format!("file://{}", path.display())
}

/// A fresh, unique temp directory for one test.
fn temp_dir(tag: &str) -> PathBuf {
	let dir = std::env::temp_dir().join(format!(
		"oakstorage_it_{}_{}",
		std::process::id(),
		tag
	));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	dir
}

/// Release an owned handle (refcount 1).
fn release(h: CHandle) {
	if let Some(release) = h.release {
		unsafe { release(h.ctx) };
	}
}

/// The backend claiming `uri` (Err for none / invalid URI).
fn backend_for(uri: &str) -> Result<Arc<dyn StorageBackend>, oak_storage::error::Error> {
	let parsed = StorageUri::parse(uri)?;
	Registry::global().resolve(&parsed)
}

/// Save `handle` to `uri` with `options`.
fn save_handle(handle: CHandle, uri: &str, options: u32) -> oak_storage::error::Result<()> {
	let parsed = StorageUri::parse(uri)?;
	let backend = Registry::global().resolve(&parsed)?;
	backend.save(handle, &parsed, options)
}

/// Save an `Arc<Mutex<Project>>` to `uri`, releasing the wrapper handle.
fn save_project(
	project: &Arc<Mutex<Project>>,
	uri: &str,
	options: u32,
) -> oak_storage::error::Result<()> {
	let handle = make_project_owned(project.clone());
	let result = save_handle(handle, uri, options);
	release(handle);
	result
}

/// Open `uri`: resolve the backend, load, wrap the result in a [`Session`].
/// `Err` is a hard failure (I/O, format, no backend, invalid URI); `Ok`
/// carries the session plus the version info code (TOO_OLD / TOO_NEW /
/// UNKNOWN_VERSION) the backend reported.
fn open(uri: &str) -> oak_storage::error::Result<(Session, i32)> {
	let parsed = StorageUri::parse(uri)?;
	let backend = Registry::global().resolve(&parsed)?;
	let result = backend.load(&parsed)?;
	// The backend hands the project back as a handle (the facade-facing
	// form); the session stores the boxed project directly.
	let project = if result.project.is_null() {
		None
	} else {
		Some(unsafe { project_arc(&result.project) }.unwrap())
	};
	let session = Session::new(parsed, project);
	Ok((session, result.version_info))
}

fn r_to_f(r: Rational) -> f64 {
	r.numerator() as f64 / r.denominator() as f64
}

fn assert_close(a: f64, b: f64) {
	assert!((a - b).abs() < 1e-6, "expected {a} close to {b}");
}

// ---------------------------------------------------------------------------
// .ove round-trip
// ---------------------------------------------------------------------------

const MATH: &str = "org.olivevideoeditor.Olive.math";

/// Build the round-trip fixture: root folder + two math nodes with
/// values/keyframes/label/color/link/connection + settings.
fn build_roundtrip_project() -> Arc<Mutex<Project>> {
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

	p.settings
		.insert("projectname".to_string(), "roundtrip-fixture".to_string());
	drop(p);
	project
}

/// Compare the round-trip-able surface field by field.
fn assert_roundtrip_fields(orig: &Project, loaded: &Project) {
	assert_eq!(loaded.uuid, orig.uuid, "uuid");
	assert_eq!(loaded.settings, orig.settings, "settings");

	let o_ids = orig.graph.node_ids();
	let l_ids = loaded.graph.node_ids();
	assert_eq!(l_ids.len(), o_ids.len(), "node count");
	// Slot order is preserved by the writer, so the ids line up.
	let o_types: Vec<&str> = o_ids
		.iter()
		.map(|id| orig.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	let l_types: Vec<&str> = l_ids
		.iter()
		.map(|id| loaded.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	assert_eq!(l_types, o_types, "node types");

	// The graph endpoints (M0b) sit between the root folder and the
	// content nodes, so pick the two math nodes by type instead of by
	// position (slot order still lines them up pairwise).
	let o_math: Vec<NodeId> = o_ids
		.iter()
		.copied()
		.filter(|id| orig.graph.get(*id).unwrap().behavior.type_id() == MATH)
		.collect();
	let l_math: Vec<NodeId> = l_ids
		.iter()
		.copied()
		.filter(|id| loaded.graph.get(*id).unwrap().behavior.type_id() == MATH)
		.collect();
	assert_eq!(l_math.len(), 2, "fixture math nodes");
	let a_o = o_math[0];
	let a_l = l_math[0];
	let b_o = o_math[1];
	let b_l = l_math[1];

	// Label + color.
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

	// Standard values.
	assert_eq!(
		loaded.graph.get(a_l).unwrap().core.standard_value("param_a_in", -1),
		orig.graph.get(a_o).unwrap().core.standard_value("param_a_in", -1),
		"value a"
	);
	assert_eq!(
		loaded.graph.get(b_l).unwrap().core.standard_value("param_a_in", -1),
		orig.graph.get(b_o).unwrap().core.standard_value("param_a_in", -1),
		"value b"
	);

	// Keyframes: count, times, values, interpolation, bezier handles.
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
		loaded.graph.connected_output(b_l, "param_b_in", -1),
		Some(a_l),
		"connection"
	);
	assert!(loaded.graph.are_linked(a_l, b_l), "link");
}

#[test]
fn ove_xml_roundtrip_field_by_field() {
	let dir = temp_dir("ove_roundtrip");
	let path = dir.join("roundtrip.ove");
	let uri = file_uri(&path);

	let project = build_roundtrip_project();
	save_project(&project, &uri, 0).unwrap();

	// The file exists and is plain XML.
	let text = std::fs::read_to_string(&path).unwrap();
	assert!(text.starts_with("<project version=\"1\">"), "{text}");

	let (session, rc) = open(&uri).unwrap();
	assert!(session.project().is_some(), "open failed rc={rc}");
	assert_eq!(rc, OAKSTORAGE_OK);
	assert_eq!(session.uri().to_uri_string(), uri);

	let loaded = session.project().cloned().unwrap();
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_roundtrip_fields(&o, &l);
	}
}

#[test]
fn ove_xml_compress_flag_still_round_trips() {
	// OAKSTORAGE_SAVE_COMPRESS (bit 0) is accepted but not implemented (the
	// oaknode serializer emits plain XML); the file still round-trips.
	let dir = temp_dir("ove_compress");
	let path = dir.join("compressed.ove");
	let uri = file_uri(&path);

	let project = build_roundtrip_project();
	save_project(&project, &uri, 1).unwrap();

	let text = std::fs::read_to_string(&path).unwrap();
	assert!(text.starts_with("<project"), "compression must not corrupt the file");

	let (session, rc) = open(&uri).unwrap();
	assert!(session.project().is_some(), "open failed rc={rc}");
	assert_eq!(rc, OAKSTORAGE_OK);
	let loaded = session.project().cloned().unwrap();
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_eq!(l.uuid, o.uuid);
		assert_eq!(l.graph.node_count(), o.graph.node_count());
		assert_eq!(l.settings, o.settings);
	}
}

/// The .ove backend preserves the full timeline structure: the
/// sequence, its track lists, the track's block order, each block's
/// span, and the clip -> footage references (this surface did not
/// round-trip before the timeline serialization landed).
#[test]
fn ove_xml_timeline_roundtrip() {
	use oak_node::block::ClipBlockBehavior;
	use oak_node::footage::FootageBehavior;
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};

	let dir = temp_dir("ove_timeline");
	let path = dir.join("timeline.ove");
	let uri = file_uri(&path);

	let project = build_timeline_project();
	save_project(&project, &uri, 0).unwrap();

	let (session, rc) = open(&uri).unwrap();
	assert!(session.project().is_some(), "open failed rc={rc}");
	assert_eq!(rc, OAKSTORAGE_OK);
	let loaded = session.project().cloned().unwrap();
	{
		let l = loaded.lock().unwrap();
		assert_imported_timeline(&l);
	}

	// The bin-level detail too: the sequence keeps its three track
	// lists (video/audio/subtitle) with the right kinds and bases.
	{
		let l = loaded.lock().unwrap();
		let mut seq: Option<(NodeId, Vec<NodeId>)> = None;
		for id in l.graph.node_ids() {
			let entry = l.graph.get(id).unwrap();
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
		let vlist = lists[0];
		let list = l
			.graph
			.get(vlist)
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<TrackListBehavior>())
			.unwrap();
		assert_eq!(list.kind, TrackType::Video);
		assert_eq!(list.array_base, 0);
		assert_eq!(list.tracks.len(), 1);
		let track = l
			.graph
			.get(list.tracks[0])
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<TrackBehavior>())
			.unwrap();
		assert_eq!(track.kind, TrackType::Video);
		assert_eq!(track.blocks.len(), 2);
		let c1 = l
			.graph
			.get(track.blocks[0])
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
			.unwrap();
		assert_close(r_to_f(c1.core.length()), 4.0);
		assert_close(r_to_f(c1.core.media_in), 0.0);
		let f1 = l
			.graph
			.get(c1.footage.unwrap())
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<FootageBehavior>())
			.unwrap();
		assert_eq!(f1.filename, "/a/b.mp4");
		let c2 = l
			.graph
			.get(track.blocks[1])
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
			.unwrap();
		assert_close(r_to_f(c2.core.length()), 2.0);
		assert_close(r_to_f(c2.core.media_in), 0.4);
		let f2 = l
			.graph
			.get(c2.footage.unwrap())
			.unwrap()
			.behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<FootageBehavior>())
			.unwrap();
		assert_eq!(f2.filename, "/a/c.mp4");
	}
}

// ---------------------------------------------------------------------------
// Probe dispatch
// ---------------------------------------------------------------------------

#[test]
fn probe_dispatch() {
	assert_eq!(backend_for("file:///tmp/x.ove").unwrap().name(), "ove-xml");
	assert_eq!(
		backend_for("file:///tmp/x.OTIO").unwrap().name(),
		"otio",
		"case-insensitive"
	);
	assert_eq!(backend_for("file:///tmp/x.fcpxml").unwrap().name(), "otio");
	assert_eq!(backend_for("/tmp/x.ove").unwrap().name(), "ove-xml", "bare path");

	// No backend claims these.
	assert_eq!(
		backend_for("oakdb://user@host/db").err().unwrap().code(),
		OAKSTORAGE_E_NO_BACKEND
	);
	assert_eq!(
		backend_for("file:///tmp/x.txt").err().unwrap().code(),
		OAKSTORAGE_E_NO_BACKEND
	);

	// Empty URI -> E_INVALID.
	assert_eq!(backend_for("").err().unwrap().code(), OAKSTORAGE_E_INVALID);
}

// ---------------------------------------------------------------------------
// Open error paths and version info codes
// ---------------------------------------------------------------------------

#[test]
fn open_error_paths() {
	// Nonexistent file: Err(Io).
	let missing = file_uri(&temp_dir("ove_missing").join("nope.ove"));
	assert_eq!(open(&missing).err().unwrap().code(), OAKSTORAGE_E_IO);

	// Too-new version header -> TOO_NEW info code, no project.
	let dir = temp_dir("ove_versions");
	let future = dir.join("future.ove");
	std::fs::write(&future, "<olive version=\"999999\"></olive>").unwrap();
	let (session, rc) = open(&file_uri(&future)).unwrap();
	assert!(session.project().is_none());
	assert_eq!(rc, OAKSTORAGE_TOO_NEW);

	// Corrupt XML -> E_FORMAT.
	let corrupt = dir.join("corrupt.ove");
	std::fs::write(&corrupt, "<project><nodes><node></project>").unwrap();
	let err = open(&file_uri(&corrupt)).err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_FORMAT);

	// Unparseable garbage -> E_FORMAT (not a version info code).
	let garbage = dir.join("garbage.ove");
	std::fs::write(&garbage, "not xml at all").unwrap();
	let err = open(&file_uri(&garbage)).err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_FORMAT);

	// Recognized olive root without a version -> UNKNOWN_VERSION.
	let unversioned = dir.join("unversioned.ove");
	std::fs::write(&unversioned, "<olive></olive>").unwrap();
	let (session, rc) = open(&file_uri(&unversioned)).unwrap();
	assert!(session.project().is_none());
	assert_eq!(rc, OAKSTORAGE_UNKNOWN_VERSION);

	// A known older version loads, reporting TOO_OLD.
	let old = dir.join("old.ove");
	std::fs::write(
		&old,
		"<olive version=\"210528\"><project version=\"1\"><uuid>{old}</uuid><nodes></nodes><settings></settings></project></olive>",
	)
	.unwrap();
	let (session, rc) = open(&file_uri(&old)).unwrap();
	assert!(session.project().is_some(), "old version must load");
	assert_eq!(rc, OAKSTORAGE_TOO_OLD);

	// Empty URI -> E_INVALID.
	assert_eq!(open("").err().unwrap().code(), OAKSTORAGE_E_INVALID);
}

// ---------------------------------------------------------------------------
// Backend pluggability (the database-swap interface proof)
// ---------------------------------------------------------------------------

/// Record of the mock backend's calls.
static MOCK_SAVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A mock backend claiming `mem://` (the Rust trait shape of the former
/// C vtable).
struct MemBackend;

impl StorageBackend for MemBackend {
	fn name(&self) -> &'static str {
		"mem-test"
	}

	fn uri_scheme(&self) -> &'static str {
		"mem"
	}

	fn can_handle(&self, uri: &StorageUri) -> bool {
		uri.scheme == "mem"
	}

	fn load(&self, uri: &StorageUri) -> oak_storage::error::Result<LoadResult> {
		MOCK_SAVED
			.lock()
			.unwrap()
			.push(format!("load:{}", uri.to_uri_string()));
		// A real project handle is the "loaded project".
		let project = Project::new();
		Ok(LoadResult::success(make_project_owned(project)))
	}

	fn save(
		&self,
		_project: CHandle,
		uri: &StorageUri,
		options: u32,
	) -> oak_storage::error::Result<()> {
		MOCK_SAVED
			.lock()
			.unwrap()
			.push(format!("save:{}:options={options}", uri.to_uri_string()));
		Ok(())
	}
}

#[test]
fn backend_pluggability() {
	// The process-global registry already holds the built-ins; the mock
	// is registered under its own name and unregistered at the end.
	Registry::global().register(Arc::new(MemBackend)).unwrap();

	// Probe routes through the registered backend.
	assert_eq!(backend_for("mem://x").unwrap().name(), "mem-test");

	// Open routes through the backend's load.
	let (session, rc) = open("mem://in").unwrap();
	assert!(session.project().is_some(), "open failed rc={rc}");
	assert_eq!(rc, OAKSTORAGE_OK);
	assert!(session.uri().to_uri_string() == "mem://in");

	// Save routes through the backend's save, options passed through.
	let project = Project::new();
	let handle = make_project_owned(project);
	save_handle(handle, "mem://out", 1).unwrap();
	release(handle);
	{
		let saved = MOCK_SAVED.lock().unwrap();
		assert!(saved.iter().any(|s| s == "load:mem://in"), "{saved:?}");
		assert!(
			saved.iter().any(|s| s == "save:mem://out:options=1"),
			"{saved:?}"
		);
	}

	// Duplicate registration is rejected.
	assert_eq!(
		Registry::global().register(Arc::new(MemBackend)).err().unwrap().code(),
		OAKSTORAGE_E_STATE
	);

	// Unregister: probe reports E_NO_BACKEND again.
	assert!(Registry::global().unregister("mem-test").is_ok());
	assert_eq!(
		backend_for("mem://x").err().unwrap().code(),
		OAKSTORAGE_E_NO_BACKEND
	);

	// Unregister of an unknown name -> E_NOT_FOUND.
	assert_eq!(
		Registry::global().unregister("nope").err().unwrap().code(),
		oak_storage::error::OAKSTORAGE_E_NOT_FOUND
	);
}

// ---------------------------------------------------------------------------
// otio / fcpxml interchange round-trip
// ---------------------------------------------------------------------------

/// Build the timeline fixture: a sequence "My Seq" with one video track
/// carrying two clips (footage /a/b.mp4 and /a/c.mp4).
fn build_timeline_project() -> Arc<Mutex<Project>> {
	let project = Project::new();
	let mut p = project.lock().unwrap();
	p.initialize().unwrap();

	let (seq_id, lists) = oak_storage::nodeutil::create_sequence(&mut p.graph);
	p.graph.get_mut(seq_id).unwrap().core.label = "My Seq".to_string();

	let mut tb = TrackBehavior::new(TrackType::Video);
	tb.track_list = Some(lists[0]);
	let track_id = p.graph.add_node(NodeCore::new(), Box::new(tb));

	// Clip 1: 100/25 long, media in 0/1, footage /a/b.mp4.
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

	// Clip 2: 50/25 long, media in 10/25, footage /a/c.mp4.
	let foot2 = p
		.graph
		.add_node(NodeCore::new(), Box::new(FootageBehavior::new("/a/c.mp4")));
	let clip2 = {
		let (core, mut behavior) = oak_node::block::clip_create();
		let clip = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		clip.core.range = oak_core::TimeRange::new(Rational::new(100, 25), Rational::new(150, 25));
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
	drop(p);
	project
}

/// Assertions over the otio model of a written interchange file.
fn assert_otio_model(timelines: &[oak_otio::Timeline]) {
	assert_eq!(timelines.len(), 1);
	let timeline = &timelines[0];
	assert_eq!(timeline.name(), "My Seq");
	let tracks: Vec<&oak_otio::Track> = timeline
		.tracks()
		.children()
		.iter()
		.filter_map(|c| c.as_track())
		.collect();
	assert_eq!(tracks.len(), 1, "one video track");
	let track = tracks[0];
	assert_eq!(track.kind(), "Video");
	let clips: Vec<&oak_otio::Clip> = track
		.children()
		.iter()
		.filter_map(|c| c.as_clip())
		.collect();
	assert_eq!(clips.len(), 2, "two clips");

	assert_eq!(clips[0].name(), "b");
	assert_eq!(
		clips[0]
			.media_reference()
			.unwrap()
			.as_external_reference()
			.unwrap()
			.target_url(),
		"file:///a/b.mp4"
	);
	let r0 = clips[0].source_range().unwrap();
	assert_close(r0.duration().to_seconds(), 4.0);
	assert_close(r0.start_time().to_seconds(), 0.0);

	assert_eq!(clips[1].name(), "c");
	assert_eq!(
		clips[1]
			.media_reference()
			.unwrap()
			.as_external_reference()
			.unwrap()
			.target_url(),
		"file:///a/c.mp4"
	);
	let r1 = clips[1].source_range().unwrap();
	assert_close(r1.duration().to_seconds(), 2.0);
	assert_close(r1.start_time().to_seconds(), 0.4);
}

/// Assertions over the oaknode project imported from an interchange
/// file: one sequence, one video track, two clips with footage.
fn assert_imported_timeline(project: &Project) {
	// Find the sequence and its track lists.
	let mut seq: Option<(NodeId, Vec<NodeId>)> = None;
	for id in project.graph.node_ids() {
		let entry = project.graph.get(id).unwrap();
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
	let (seq_id, lists) = seq.expect("imported project has a sequence");
	assert_eq!(project.graph.get(seq_id).unwrap().core.label, "My Seq");

	// Video list -> one track.
	let list_entry = project.graph.get(lists[0]).unwrap();
	let list = list_entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackListBehavior>())
		.unwrap();
	assert_eq!(list.tracks.len(), 1);
	let track_id = list.tracks[0];
	let track = project
		.graph
		.get(track_id)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	assert_eq!(track.kind, TrackType::Video);
	assert_eq!(track.blocks.len(), 2);

	// Clip 1.
	let c1 = project
		.graph
		.get(track.blocks[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.expect("block 0 is a clip");
	assert_close(r_to_f(c1.core.length()), 4.0);
	assert_close(r_to_f(c1.core.media_in), 0.0);
	let f1 = c1.footage.expect("clip 1 has footage");
	let filename1 = project
		.graph
		.get(f1)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap()
		.filename
		.clone();
	assert_eq!(filename1, "/a/b.mp4");

	// Clip 2.
	let c2 = project
		.graph
		.get(track.blocks[1])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.expect("block 1 is a clip");
	assert_close(r_to_f(c2.core.length()), 2.0);
	assert_close(r_to_f(c2.core.media_in), 0.4);
	let f2 = c2.footage.expect("clip 2 has footage");
	let filename2 = project
		.graph
		.get(f2)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap()
		.filename
		.clone();
	assert_eq!(filename2, "/a/c.mp4");
}

fn assert_interchange_roundtrip(ext: &str) {
	let dir = temp_dir(&format!("interchange_{ext}"));
	let path = dir.join(format!("timeline.{ext}"));
	let uri = file_uri(&path);

	let project = build_timeline_project();
	save_project(&project, &uri, 0).unwrap();

	// The written file parses back through oakotio with clips/tracks.
	match ext {
		"otio" => {
			let text = std::fs::read_to_string(&path).unwrap();
			let root = oak_otio::from_json_string(&text).unwrap();
			let timelines: Vec<oak_otio::Timeline> = match root {
				oak_otio::Serializable::Timeline(t) => vec![t],
				other => panic!("expected a timeline root, got {}", other.schema_name()),
			};
			assert_otio_model(&timelines);
		}
		"fcpxml" => {
			let text = std::fs::read_to_string(&path).unwrap();
			let timelines = oak_otio::from_fcpxml_string(&text).unwrap();
			assert_otio_model(&timelines);
		}
		_ => unreachable!(),
	}

	// And imports back into an equivalent project.
	let (session, rc) = open(&uri).unwrap();
	assert!(session.project().is_some(), "open failed rc={rc}");
	assert_eq!(rc, OAKSTORAGE_OK);
	let loaded = session.project().cloned().unwrap();
	{
		let l = loaded.lock().unwrap();
		assert_imported_timeline(&l);
	}
}

#[test]
fn otio_roundtrip() {
	assert_interchange_roundtrip("otio");
}

#[test]
fn fcpxml_roundtrip() {
	assert_interchange_roundtrip("fcpxml");
}

/// A file whose root is neither a timeline nor a collection fails with
/// E_FORMAT on open.
#[test]
fn otio_bad_root_rejected() {
	let dir = temp_dir("otio_badroot");
	let path = dir.join("bad.otio");
	std::fs::write(&path, "{\"OTIO_SCHEMA\": \"NotATimeline.1\", \"x\": 1}").unwrap();
	let err = open(&file_uri(&path)).err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_FORMAT);
}

// ---------------------------------------------------------------------------
// NULL / invalid-handle matrix
// ---------------------------------------------------------------------------

#[test]
fn null_and_invalid_handles() {
	// open with an empty URI -> E_INVALID.
	assert_eq!(open("").err().unwrap().code(), OAKSTORAGE_E_INVALID);

	// save with a null project handle -> E_INVALID (the project cannot be
	// read out of an empty handle).
	let uri = "file:///tmp/x.ove";
	let backend = backend_for(uri).unwrap();
	let err = backend
		.save(CHandle::null(), &StorageUri::parse(uri).unwrap(), 0)
		.err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_INVALID);

	// save to an unknown-scheme URI -> E_NO_BACKEND (never reaches a
	// backend).
	let project = Project::new();
	let handle = make_project_owned(project);
	let err = save_handle(handle, "oakdb://x", 0).err().unwrap();
	assert_eq!(err.code(), OAKSTORAGE_E_NO_BACKEND);
	release(handle);

	// take transfers the project; the second take is empty.
	let dir = temp_dir("null_matrix");
	let path = dir.join("x.ove");
	let uri = file_uri(&path);
	let project = build_roundtrip_project();
	save_project(&project, &uri, 0).unwrap();

	let (mut session, _) = open(&uri).unwrap();
	let taken = session.take().expect("take transfers the project");
	assert!(session.project().is_none(), "empty shell after take");
	drop(taken);
}

// ---------------------------------------------------------------------------
// Session shell lifecycle
// ---------------------------------------------------------------------------

/// open -> Session owns the project handle; take hands it out and the
/// shell stays usable (drop-safe).
#[test]
fn session_take_transfers_project() {
	let dir = temp_dir("session");
	let path = dir.join("x.ove");
	let uri = file_uri(&path);
	let project = build_roundtrip_project();
	save_project(&project, &uri, 0).unwrap();

	let (mut session, rc) = open(&uri).unwrap();
	assert_eq!(rc, OAKSTORAGE_OK);
	assert!(session.project().is_some(), "open counts one project");

	// Take transfers the project; the session shell stays empty.
	let taken = session.take().unwrap();
	assert!(session.project().is_none(), "take empties the shell");
	drop(taken);
	// Dropping the shell (with the project already taken) is a no-op.
	drop(session);

	// A second open/take pairing works the same.
	let (mut session, _) = open(&uri).unwrap();
	let taken = session.take().unwrap();
	assert!(session.project().is_none(), "take empties the shell");
	drop(taken);
}
