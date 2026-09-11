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

//! Serializer tests. The C++ writer's golden files do not exist in this
//! tree (`tests/golden/` is empty), so the byte-exact comparison is
//! ignored with a doc reason; everything else (round-trip idempotence,
//! version rejection, corrupt input) is live.

/// Save format parity: a fixture project saves byte-identical XML to
/// the C++ 230220 writer output (attribute order included).
///
/// Ignored: the C++ golden fixtures are not committed in this tree
/// (src/node/tests/ has no .xml/.ove files) and the value text codecs
/// use Rust formatting, so byte-exact parity cannot be pinned here. The
/// C++ gtest suite (`src/node/tests/serializer_test.cpp`) pins the
/// writer; this crate's serializer_test covers structure + round-trip.
#[test]
#[ignore = "C++ golden fixtures unavailable in this tree"]
fn save_matches_cpp_byte_exact() {
	todo!()
}

/// Round-trip: load(save(p)) yields a project whose re-saved XML is
/// identical (idempotence).
///
/// Needs the `test-stubs` feature: save/load route XML through the
/// oakcommon bridge, whose symbols only resolve in the test binary when
/// the in-crate stubs are compiled in.
#[cfg(feature = "test-stubs")]
#[test]
fn roundtrip_idempotent() {
	use oak_node::input::Input;
	use oak_node::node::NodeCore;
	use oak_node::project::Project;
	use oak_node::value::{NodeValue, ValueType};

	let project = Project::new();
	{
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		// A math node with a set value + a keyframe.
		let (core, behavior) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		let id = p.graph.add_node(core, behavior);
		p.graph.get_mut(id).unwrap().core.set_standard_value(
			"param_a_in",
			-1,
			NodeValue::Float(2.5),
		);
		p.graph
			.get_mut(id)
			.unwrap()
			.core
			.keyframe_track_mut("param_a_in", -1)
			.set_key(oak_node::keyframe::Keyframe {
				time: oak_core::Rational::new(0, 1),
				value: NodeValue::Float(1.0),
				interpolation: oak_node::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		// A second node connected to the first.
		let (core2, behavior2) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		let id2 = p.graph.add_node(core2, behavior2);
		p.graph.connect(id, id2, "param_a_in", -1).unwrap();
	}

	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	let loaded = oak_node::serializer::load(&xml).unwrap();
	let xml2 = {
		let p = loaded.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	assert_eq!(xml, xml2, "re-save is idempotent");

	// Structural equivalence: node count + edges.
	{
		let a = project.lock().unwrap();
		let b = loaded.lock().unwrap();
		assert_eq!(a.graph.node_count(), b.graph.node_count());
		assert_eq!(
			a.graph.output_connections_all().len(),
			b.graph.output_connections_all().len()
		);
	}
}

/// Version ladder: historical `<olive ...>` roots with known versions
/// load (the body upgrades to the current model); unknown roots are
/// rejected.
///
/// Needs the `test-stubs` feature (same XML-bridge rationale as
/// [`roundtrip_idempotent`]).
#[cfg(feature = "test-stubs")]
#[test]
fn historical_versions_upgrade() {
	// A current-format document round-trips.
	let xml =
		"<project version=\"1\"><uuid>{test}</uuid><nodes></nodes><settings></settings></project>";
	let project = oak_node::serializer::load(xml).unwrap();
	assert_eq!(project.lock().unwrap().uuid, "{test}");

	// An unknown root is rejected.
	assert!(oak_node::serializer::load("<olive-unknown/>").is_err());
	assert!(oak_node::serializer::load("").is_err());
}

/// Corrupt XML and newer-than-build versions are rejected with the
/// documented error codes, never a panic.
#[test]
fn corrupt_and_future_files_rejected() {
	// A newer-than-build version is rejected.
	let future = "<olive version=\"999999\"></olive>";
	assert!(oak_node::serializer::load(future).is_err());

	// Malformed XML (unbalanced) is rejected without a panic.
	let corrupt = "<project><nodes><node></project>";
	assert!(oak_node::serializer::load(corrupt).is_err());

	// Garbage text.
	assert!(oak_node::serializer::load("not xml at all").is_err());
}

/// The C++-era fixture project (`tests/project_with_footage.ove`, a
/// 230220 full save with the `<olive>`/`<project>` container) loads with
/// its bin and timeline structure intact: the root folder holds the
/// footage + sequence children, the footage file name and timestamp
/// survive, and the sequence's viewer connections resolve.
#[test]
fn golden_project_with_footage_loads() {
	use oak_node::folder::FolderBehavior;
	use oak_node::footage::FootageBehavior;
	use oak_node::sequence::SequenceBehavior;

	let xml = std::fs::read_to_string(concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../oak-app/tests/project_with_footage.ove"
	))
	.unwrap();
	let project = oak_node::serializer::load(&xml).unwrap();
	let p = project.lock().unwrap();

	// The root folder (from the settings "root" key) holds the bin.
	let root = p.graph.get(p.root).expect("root folder resolves");
	assert_eq!(
		root.behavior.type_id(),
		"org.olivevideoeditor.Olive.folder"
	);
	let folder = root
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FolderBehavior>())
		.expect("root is a folder");
	assert_eq!(folder.name, "Root");
	assert_eq!(folder.children.len(), 2, "folder holds footage + sequence");

	// Child 0: footage with the demo.mp4 file name (the C++ fixture
	// stores it in the `file_in` input) and its media timestamp.
	let footage_id = folder.children[0];
	let footage_entry = p.graph.get(footage_id).unwrap();
	assert_eq!(
		footage_entry.behavior.type_id(),
		"org.olivevideoeditor.Olive.footage"
	);
	let footage = footage_entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap();
	assert_eq!(footage.filename, "demo.mp4");
	assert_eq!(footage.timestamp, 1780763070093);

	// Child 1: the sequence, connected to the footage (tex_in/samples_in).
	let seq_id = folder.children[1];
	let seq_entry = p.graph.get(seq_id).unwrap();
	assert_eq!(
		seq_entry.behavior.type_id(),
		"org.olivevideoeditor.Olive.sequence"
	);
	assert_eq!(seq_entry.core.label, "Fixture Sequence");
	let seq = seq_entry
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		.unwrap();
	// The fixture carries no tracks: the three `track_in_%1` inputs
	// exist but no track-list nodes are present.
	assert!(seq.track_lists.is_empty());
	assert_eq!(
		p.graph.connected_output(seq_id, "tex_in", -1),
		Some(footage_id)
	);
	assert_eq!(
		p.graph.connected_output(seq_id, "samples_in", -1),
		Some(footage_id)
	);

	// Settings survived (incl. the root key).
	assert_eq!(
		p.settings.get("root").map(String::as_str),
		Some("94432914284304")
	);
}

/// Build a full-featured project for the timeline round-trip: a root
/// folder holding a footage (streams/proxy/timestamp) and a sequence
/// with video/audio track lists, tracks with clips and a gap, an effect
/// chain with keyframes, plus settings and a link.
fn build_full_project() -> std::sync::Arc<std::sync::Mutex<oak_node::project::Project>> {
	use oak_core::{Rational, TimeRange};
	use oak_node::block::{
		adjustment_create, clip_create, gap_create, AdjustmentBlockBehavior, ClipBlockBehavior,
		GapBlockBehavior,
	};
	use oak_node::folder::FolderBehavior;
	use oak_node::footage::{FootageBehavior, StreamInfo};
	use oak_node::keyframe::{Interpolation, Keyframe};
	use oak_node::node::NodeCore;
	use oak_node::project::Project;
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
	use oak_node::value::{AudioParams, NodeValue, VideoParams};

	let project = Project::new();
	let mut p = project.lock().unwrap();
	p.initialize().unwrap();
	p.settings
		.insert("projectname".to_string(), "full-featured".to_string());
	let folder_id = p.root;

	// Footage with streams, proxy state and a media timestamp.
	let footage_id = {
		let (core, mut behavior) = FootageBehavior::create();
		let f = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<FootageBehavior>())
			.unwrap();
		f.filename = "/media/demo.mp4".to_string();
		f.timestamp = 1780763070093;
		f.proxy = "/media/demo_proxy.mp4".to_string();
		f.proxy_enabled = true;
		f.proxy_state = 2;
		f.proxy_video_stream_index = 0;
		f.proxy_preset_version = 1;
		f.streams = vec![
			StreamInfo {
				index: 0,
				is_video: true,
				video: Some(VideoParams {
					width: 1920,
					height: 1080,
					frame_rate: Rational::new(25, 1),
					pixel_format: 4,
					channels: 4,
					interlaced: false,
				}),
				audio: None,
				duration: Rational::new(217600, 12800),
			},
			StreamInfo {
				index: 1,
				is_video: false,
				video: None,
				audio: Some(AudioParams {
					sample_rate: 48000,
					channel_layout: 3,
					format: 4,
				}),
				duration: Rational::new(816000, 48000),
			},
		];
		p.graph.add_node(core, behavior)
	};
	{
		let entry = p.graph.get_mut(folder_id).unwrap();
		let folder = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<FolderBehavior>())
			.unwrap();
		folder.add_child(footage_id);
	}

	// Sequence + its video/audio track lists.
	let (score, sbehavior) = SequenceBehavior::create();
	let seq_id = p.graph.add_node(score, sbehavior);
	let vlist_id = {
		let mut behavior = TrackListBehavior::new(TrackType::Video);
		behavior.sequence = Some(seq_id);
		behavior.array_base = 0;
		p.graph.add_node(NodeCore::new(), Box::new(behavior))
	};
	let alist_id = {
		let mut behavior = TrackListBehavior::new(TrackType::Audio);
		behavior.sequence = Some(seq_id);
		behavior.array_base = 1;
		p.graph.add_node(NodeCore::new(), Box::new(behavior))
	};

	// Video track: clips + gap.
	let vtrack_id = {
		let mut behavior = TrackBehavior::new(TrackType::Video);
		behavior.track_list = Some(vlist_id);
		behavior.index = 0;
		behavior.height = 4.0;
		behavior.muted = true;
		p.graph.add_node(NodeCore::new(), Box::new(behavior))
	};
	let clip1_id = {
		let (core, mut behavior) = clip_create();
		let c = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		c.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(100, 25));
		c.core.media_in = Rational::new(0, 1);
		c.core.speed = 1.0;
		c.core.enabled = true;
		c.core.track = Some(vtrack_id);
		c.footage = Some(footage_id);
		p.graph.add_node(core, behavior)
	};
	let gap1_id = {
		let (core, mut behavior) = gap_create();
		let g = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<GapBlockBehavior>())
			.unwrap();
		g.core.range = TimeRange::new(Rational::new(100, 25), Rational::new(120, 25));
		g.core.track = Some(vtrack_id);
		p.graph.add_node(core, behavior)
	};
	let clip2_id = {
		let (core, mut behavior) = clip_create();
		let c = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		c.core.range = TimeRange::new(Rational::new(120, 25), Rational::new(220, 25));
		c.core.media_in = Rational::new(10, 25);
		c.core.speed = 1.5;
		c.core.reversed = true;
		c.core.loop_mode = 2;
		c.core.track = Some(vtrack_id);
		c.footage = Some(footage_id);
		p.graph.add_node(core, behavior)
	};

	// An opacity effect on clip1 with a keyframed value.
	let _effect_id = {
		let (core, behavior) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.opacity")
			.unwrap()
			.create)();
		let id = p.graph.add_node(core, behavior);
		p.graph.get_mut(id).unwrap().core.set_standard_value(
			"opacity_in",
			-1,
			NodeValue::Float(0.5),
		);
		p.graph
			.get_mut(id)
			.unwrap()
			.core
			.keyframe_track_mut("opacity_in", -1)
			.set_key(Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(1.0),
				interpolation: Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		p.graph
			.get_mut(id)
			.unwrap()
			.core
			.keyframe_track_mut("opacity_in", -1)
			.set_key(Keyframe {
				time: Rational::new(1, 1),
				value: NodeValue::Float(0.0),
				interpolation: Interpolation::Bezier,
				bezier_in: (0.1, 0.2),
				bezier_out: (0.3, 0.4),
			});
		p.graph.connect(id, clip1_id, "tex_in", -1).unwrap();
		id
	};

	// Audio track + its clip.
	let atrack_id = {
		let mut behavior = TrackBehavior::new(TrackType::Audio);
		behavior.track_list = Some(alist_id);
		behavior.index = 0;
		behavior.locked = true;
		p.graph.add_node(NodeCore::new(), Box::new(behavior))
	};
	let clip3_id = {
		let (core, mut behavior) = clip_create();
		let c = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<ClipBlockBehavior>())
			.unwrap();
		c.core.range = TimeRange::new(Rational::new(0, 1), Rational::new(50, 25));
		c.core.media_in = Rational::new(5, 25);
		c.core.track = Some(atrack_id);
		c.footage = Some(footage_id);
		p.graph.add_node(core, behavior)
	};

	// A standalone adjustment layer at the end of the video track.
	let adj_id = {
		let (core, mut behavior) = adjustment_create();
		let a = behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<AdjustmentBlockBehavior>())
			.unwrap();
		a.core.range = TimeRange::new(Rational::new(220, 25), Rational::new(260, 25));
		a.core.track = Some(vtrack_id);
		p.graph.add_node(core, behavior)
	};

	// Wire the hierarchy (behavior fields, the Rust model).
	{
		let entry = p.graph.get_mut(seq_id).unwrap();
		entry.core.label = "Full Sequence".to_string();
		entry.core.override_color = 3;
		let s = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<SequenceBehavior>())
			.unwrap();
		s.track_lists = vec![vlist_id, alist_id];
	}
	// The bin: the sequence joins the folder after the footage.
	{
		let entry = p.graph.get_mut(folder_id).unwrap();
		let folder = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<FolderBehavior>())
			.unwrap();
		folder.add_child(seq_id);
	}
	{
		let entry = p.graph.get_mut(vlist_id).unwrap();
		let tl = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackListBehavior>())
			.unwrap();
		tl.tracks = vec![vtrack_id];
	}
	{
		let entry = p.graph.get_mut(vtrack_id).unwrap();
		let t = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackBehavior>())
			.unwrap();
		t.blocks = vec![clip1_id, gap1_id, clip2_id, adj_id];
	}
	{
		let entry = p.graph.get_mut(alist_id).unwrap();
		let tl = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackListBehavior>())
			.unwrap();
		tl.tracks = vec![atrack_id];
	}
	{
		let entry = p.graph.get_mut(atrack_id).unwrap();
		let t = entry
			.behavior
			.as_any_mut()
			.and_then(|a| a.downcast_mut::<TrackBehavior>())
			.unwrap();
		t.blocks = vec![clip3_id];
	}

	// Link the two video clips.
	p.graph.link(clip1_id, clip2_id);

	drop(p);
	project
}

/// Borrowed track list of a node.
fn list_of<'a>(
	p: &'a oak_node::project::Project,
	id: oak_node::id::NodeId,
) -> &'a oak_node::track::TrackListBehavior {
	p.graph
		.get(id)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::track::TrackListBehavior>())
		.unwrap()
}

/// Field-by-field comparison of the round-tripped full project.
fn assert_full_roundtrip_fields(orig: &oak_node::project::Project, loaded: &oak_node::project::Project) {
	use oak_node::block::ClipBlockBehavior;
	use oak_node::folder::FolderBehavior;
	use oak_node::footage::FootageBehavior;
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackType};

	// Project shell: uuid + settings.
	assert_eq!(loaded.uuid, orig.uuid, "uuid");
	assert_eq!(loaded.settings, orig.settings, "settings");

	// The graph keeps its node count, types and edge count.
	assert_eq!(loaded.graph.node_count(), orig.graph.node_count(), "node count");
	let o_types: Vec<&str> = orig
		.graph
		.node_ids()
		.iter()
		.map(|id| orig.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	let l_types: Vec<&str> = loaded
		.graph
		.node_ids()
		.iter()
		.map(|id| loaded.graph.get(*id).unwrap().behavior.type_id())
		.collect();
	assert_eq!(l_types, o_types, "node types");
	assert_eq!(
		loaded.graph.output_connections_all().len(),
		orig.graph.output_connections_all().len(),
		"edge count"
	);

	// Map original id -> loaded id (slot order is preserved).
	let o_ids = orig.graph.node_ids();
	let l_ids = loaded.graph.node_ids();

	// Root folder: children + bin membership.
	let of = orig
		.graph
		.get(orig.root)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FolderBehavior>())
		.unwrap();
	let lf = loaded
		.graph
		.get(loaded.root)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FolderBehavior>())
		.unwrap();
	assert_eq!(lf.name, of.name, "folder name");
	assert_eq!(lf.children.len(), of.children.len(), "folder children");
	let o_footage = of.children[0];
	let o_seq = of.children[1];
	let l_footage = lf.children[0];
	let l_seq = lf.children[1];

	// Footage: filename, timestamp, proxy and streams.
	let o_f = orig
		.graph
		.get(o_footage)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap();
	let l_f = loaded
		.graph
		.get(l_footage)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<FootageBehavior>())
		.unwrap();
	assert_eq!(l_f.filename, o_f.filename, "footage filename");
	assert_eq!(l_f.timestamp, o_f.timestamp, "footage timestamp");
	assert_eq!(l_f.proxy, o_f.proxy, "footage proxy path");
	assert_eq!(l_f.proxy_enabled, o_f.proxy_enabled, "proxy enabled");
	assert_eq!(l_f.proxy_state, o_f.proxy_state, "proxy state");
	assert_eq!(
		l_f.proxy_video_stream_index, o_f.proxy_video_stream_index,
		"proxy stream"
	);
	assert_eq!(l_f.proxy_preset_version, o_f.proxy_preset_version, "proxy preset");
	assert_eq!(l_f.streams.len(), o_f.streams.len(), "stream count");
	for (ls, os) in l_f.streams.iter().zip(&o_f.streams) {
		assert_eq!(ls.index, os.index, "stream index");
		assert_eq!(ls.is_video, os.is_video, "stream video flag");
		assert_eq!(ls.video, os.video, "stream video params");
		assert_eq!(ls.audio, os.audio, "stream audio params");
		assert_eq!(ls.duration, os.duration, "stream duration");
	}

	// Sequence: label/color, track lists (kind, base, backrefs).
	let o_s = orig
		.graph
		.get(o_seq)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		.unwrap();
	let l_s = loaded
		.graph
		.get(l_seq)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<SequenceBehavior>())
		.unwrap();
	assert_eq!(
		loaded.graph.get(l_seq).unwrap().core.label,
		orig.graph.get(o_seq).unwrap().core.label,
		"sequence label"
	);
	assert_eq!(
		loaded.graph.get(l_seq).unwrap().core.override_color,
		orig.graph.get(o_seq).unwrap().core.override_color,
		"sequence color"
	);
	assert_eq!(l_s.track_lists.len(), o_s.track_lists.len(), "track list count");
	let (o_vlist, l_vlist) = (o_s.track_lists[0], l_s.track_lists[0]);
	let (o_alist, l_alist) = (o_s.track_lists[1], l_s.track_lists[1]);

	let o_vl = list_of(orig, o_vlist);
	let l_vl = list_of(loaded, l_vlist);
	assert_eq!(l_vl.kind, o_vl.kind, "video list kind");
	assert_eq!(l_vl.array_base, o_vl.array_base, "video list base");
	assert_eq!(l_vl.sequence, Some(l_seq), "video list sequence backref");
	assert_eq!(l_vl.tracks.len(), o_vl.tracks.len(), "video list track count");
	let (o_vtrack, l_vtrack) = (o_vl.tracks[0], l_vl.tracks[0]);
	let o_al = list_of(orig, o_alist);
	let l_al = list_of(loaded, l_alist);
	assert_eq!(l_al.kind, o_al.kind, "audio list kind");
	assert_eq!(l_al.array_base, o_al.array_base, "audio list base");
	assert_eq!(l_al.sequence, Some(l_seq), "audio list sequence backref");
	assert_eq!(l_al.tracks.len(), o_al.tracks.len(), "audio list track count");
	let (o_atrack, l_atrack) = (o_al.tracks[0], l_al.tracks[0]);

	// Video track: kind/blocks/muted/locked/height/index/backref.
	let o_t = orig
		.graph
		.get(o_vtrack)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	let l_t = loaded
		.graph
		.get(l_vtrack)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	assert_eq!(l_t.kind, TrackType::Video, "video track kind");
	assert_eq!(l_t.muted, o_t.muted, "video track muted");
	assert_eq!(l_t.locked, o_t.locked, "video track locked");
	assert_eq!(l_t.height, o_t.height, "video track height");
	assert_eq!(l_t.index, o_t.index, "video track index");
	assert_eq!(l_t.track_list, Some(l_vlist), "video track list backref");
	assert_eq!(l_t.blocks.len(), o_t.blocks.len(), "video track block count");
	let o_clip1 = o_t.blocks[0];
	let o_gap1 = o_t.blocks[1];
	let o_clip2 = o_t.blocks[2];
	let o_adj = o_t.blocks[3];
	let l_clip1 = l_t.blocks[0];
	let l_gap1 = l_t.blocks[1];
	let l_clip2 = l_t.blocks[2];
	let l_adj = l_t.blocks[3];

	// Clip 1: range/media_in/speed/reversed/enabled/pitch/loop, track
	// and footage backrefs, and the effect connection.
	let o_c1 = orig
		.graph
		.get(o_clip1)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	let l_c1 = loaded
		.graph
		.get(l_clip1)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	assert_eq!(l_c1.core.range, o_c1.core.range, "clip1 range");
	assert_eq!(l_c1.core.media_in, o_c1.core.media_in, "clip1 media in");
	assert_eq!(l_c1.core.speed, o_c1.core.speed, "clip1 speed");
	assert_eq!(l_c1.core.reversed, o_c1.core.reversed, "clip1 reversed");
	assert_eq!(l_c1.core.enabled, o_c1.core.enabled, "clip1 enabled");
	assert_eq!(
		l_c1.core.maintain_audio_pitch, o_c1.core.maintain_audio_pitch,
		"clip1 pitch"
	);
	assert_eq!(l_c1.core.loop_mode, o_c1.core.loop_mode, "clip1 loop");
	assert_eq!(l_c1.core.track, Some(l_vtrack), "clip1 track backref");
	assert_eq!(l_c1.footage, Some(l_footage), "clip1 footage backref");

	// Gap 1: range + track backref.
	let o_g1 = orig
		.graph
		.get(o_gap1)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::block::GapBlockBehavior>())
		.unwrap();
	let l_g1 = loaded
		.graph
		.get(l_gap1)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::block::GapBlockBehavior>())
		.unwrap();
	assert_eq!(l_g1.core.range, o_g1.core.range, "gap range");
	assert_eq!(l_g1.core.track, Some(l_vtrack), "gap track backref");

	// Clip 2 (speed/reversed set).
	let o_c2 = orig
		.graph
		.get(o_clip2)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	let l_c2 = loaded
		.graph
		.get(l_clip2)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	assert_eq!(l_c2.core.range, o_c2.core.range, "clip2 range");
	assert_eq!(l_c2.core.media_in, o_c2.core.media_in, "clip2 media in");
	assert_eq!(l_c2.core.speed, o_c2.core.speed, "clip2 speed");
	assert_eq!(l_c2.core.reversed, o_c2.core.reversed, "clip2 reversed");
	assert_eq!(l_c2.core.loop_mode, o_c2.core.loop_mode, "clip2 loop");
	assert_eq!(l_c2.footage, Some(l_footage), "clip2 footage backref");

	// Adjustment layer: type, range and track backref.
	let o_a = orig
		.graph
		.get(o_adj)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::block::AdjustmentBlockBehavior>())
		.unwrap();
	let l_a = loaded
		.graph
		.get(l_adj)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::block::AdjustmentBlockBehavior>())
		.unwrap();
	assert_eq!(
		loaded.graph.get(l_adj).unwrap().behavior.type_id(),
		"org.olivevideoeditor.Olive.adjustment",
		"adjustment type id"
	);
	assert_eq!(l_a.core.range, o_a.core.range, "adjustment range");
	assert_eq!(l_a.core.track, Some(l_vtrack), "adjustment track backref");

	// Audio track + clip 3.
	let o_at = orig
		.graph
		.get(o_atrack)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	let l_at = loaded
		.graph
		.get(l_atrack)
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<TrackBehavior>())
		.unwrap();
	assert_eq!(l_at.kind, TrackType::Audio, "audio track kind");
	assert_eq!(l_at.locked, o_at.locked, "audio track locked");
	assert_eq!(l_at.track_list, Some(l_alist), "audio track list backref");
	assert_eq!(l_at.blocks.len(), o_at.blocks.len(), "audio track block count");
	let o_c3 = orig
		.graph
		.get(o_at.blocks[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	let l_c3 = loaded
		.graph
		.get(l_at.blocks[0])
		.unwrap()
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<ClipBlockBehavior>())
		.unwrap();
	assert_eq!(l_c3.core.range, o_c3.core.range, "clip3 range");
	assert_eq!(l_c3.core.media_in, o_c3.core.media_in, "clip3 media in");
	assert_eq!(l_c3.core.track, Some(l_atrack), "clip3 track backref");
	assert_eq!(l_c3.footage, Some(l_footage), "clip3 footage backref");

	// Effect chain: the effect feeds clip1's tex_in.
	let o_effect = orig
		.graph
		.connected_output(o_clip1, "tex_in", -1)
		.expect("clip1 has an effect");
	let l_effect = loaded
		.graph
		.connected_output(l_clip1, "tex_in", -1)
		.expect("clip1 has an effect after load");
	let o_effect_i = o_ids.iter().position(|id| *id == o_effect).unwrap();
	let l_effect_i = l_ids.iter().position(|id| *id == l_effect).unwrap();
	assert_eq!(l_effect_i, o_effect_i, "effect slot");
	// The effect's keyframes survive.
	let ok = orig
		.graph
		.get(o_effect)
		.unwrap()
		.core
		.keyframe_track("opacity_in", -1)
		.unwrap()
		.keys()
		.to_vec();
	let lk = loaded
		.graph
		.get(l_effect)
		.unwrap()
		.core
		.keyframe_track("opacity_in", -1)
		.unwrap()
		.keys()
		.to_vec();
	assert_eq!(lk.len(), ok.len(), "keyframe count");
	for (ko, kl) in ok.iter().zip(&lk) {
		assert_eq!(kl.time, ko.time, "key time");
		assert_eq!(kl.value.to_double(), ko.value.to_double(), "key value");
		assert_eq!(kl.interpolation, ko.interpolation, "key interpolation");
		assert_eq!(kl.bezier_in, ko.bezier_in, "key bezier in");
		assert_eq!(kl.bezier_out, ko.bezier_out, "key bezier out");
	}

	// The clip link survives.
	assert!(
		loaded.graph.are_linked(l_clip1, l_clip2),
		"clip link survives"
	);
	assert!(
		loaded.graph.are_linked(l_clip2, l_clip1),
		"clip link symmetric"
	);
}

/// Round-trip the full-featured project: save, load, compare field by
/// field, and re-save idempotently.
#[test]
fn roundtrip_full_timeline() {
	let project = build_full_project();
	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	let loaded = oak_node::serializer::load(&xml).unwrap();
	{
		let o = project.lock().unwrap();
		let l = loaded.lock().unwrap();
		assert_full_roundtrip_fields(&o, &l);
	}

	// Re-save is idempotent (byte-identical).
	let xml2 = {
		let l = loaded.lock().unwrap();
		oak_node::serializer::save(&l).unwrap()
	};
	assert_eq!(xml, xml2, "re-save is idempotent");
}

/// Standalone `MultiCamNode` round-trip: the `current_in` standard value
/// and the node's type survive save/load (C++
/// `ProjectSerializer::FileRoundTripPreservesMultiCamNode`).
#[test]
fn multicam_node_round_trip_preserves_current_in() {
	use oak_node::nodes::multicamnode::{create, CURRENT_INPUT};
	use oak_node::project::Project;
	use oak_node::value::NodeValue;

	let project = Project::new();
	let mc_id = {
		let mut p = project.lock().unwrap();
		let (core, behavior) = create();
		let id = p.graph.add_node(core, behavior);
		p.graph.get_mut(id).unwrap().core.set_standard_value(
			CURRENT_INPUT,
			-1,
			NodeValue::Combo(2),
		);
		id
	};
	let _ = mc_id;

	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	let loaded = oak_node::serializer::load(&xml).unwrap();
	let l = loaded.lock().unwrap();

	let loaded_mc = l
		.graph
		.node_ids()
		.into_iter()
		.find(|id| {
			l.graph.get(*id).map(|e| e.behavior.type_id())
				== Some("org.olivevideoeditor.Olive.multicam")
		})
		.expect("loaded project has a MultiCamNode");
	// The combo's numeric value survives the round-trip (the value codec
	// serializes combo indices as Int — `string_to_value` maps
	// `ValueType::Combo` to `NodeValue::Int`).
	assert_eq!(
		l.graph.get(loaded_mc).unwrap().core.standard_value(CURRENT_INPUT, -1),
		NodeValue::Int(2),
		"current_in survives the round-trip"
	);
}

/// A runtime-registered (dynamic) node type — the OFX plugin seam — is
/// rebuilt from a snapshot by the serializer: a type id that lives only
/// in the factory's dynamic table (the C++ `register_plugin_nodes`
/// library entries) is rejected before registration and round-trips
/// once registered. The worker process runs the identical plugin scan
/// at startup (worker.rs `register_plugin_nodes`), so a snapshot
/// carrying plugin nodes deserializes there the same way.
#[test]
fn dynamic_plugin_node_round_trips_across_load() {
	use oak_node::factory::{DynamicNodeMeta, Factory};
	use oak_node::input::Input;
	use oak_node::node::{Category, NodeBehavior, NodeCore};
	use oak_node::project::Project;
	use oak_node::value::{NodeValue, ValueType};

	const TYPE_ID: &str = "org.test.dynamic-rebuild-probe";
	const INPUT_ID: &str = "probe_in";

	// A pure-Rust stand-in for a discovered OFX plugin node: its type id
	// is unknown to the static menu table, so only the dynamic path of
	// `create_any` can construct it (mirroring the plugin closure that
	// captures the identifier).
	struct Probe;
	impl NodeBehavior for Probe {
		fn name(&self) -> &str {
			"Dynamic Probe"
		}
		fn type_id(&self) -> &str {
			TYPE_ID
		}
		fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			Some(Box::new(Probe))
		}
	}

	// Build a project with one probe node carrying a standard value.
	let project = Project::new();
	// The dynamic constructor must rebuild the same declared inputs as
	// the original (a real plugin's `create_plugin_node` builds the core
	// from its OFX params).
	let probe_core = || {
		let mut core = NodeCore::new();
		core.inputs
			.push(Input::new(INPUT_ID, ValueType::Float, NodeValue::Float(0.0)));
		core
	};
	{
		let mut p = project.lock().unwrap();
		let id = p.graph.add_node(probe_core(), Box::new(Probe));
		p.graph
			.get_mut(id)
			.unwrap()
			.core
			.set_standard_value(INPUT_ID, -1, NodeValue::Float(2.5));
	}
	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	assert!(xml.contains(TYPE_ID), "the dynamic type id is persisted");

	// Before the plugin scan registers the entry the snapshot is
	// unreadable (the plugin is not installed in this process).
	assert!(oak_node::serializer::load(&xml).is_err());

	// The scan registers the dynamic entry...
	let registered = Factory::global().register_dynamic(DynamicNodeMeta {
		type_id: TYPE_ID.to_string(),
		name: "Dynamic Probe".to_string(),
		categories: vec![Category::OpenFx],
		sub_category: "Filter".to_string(),
		description: "test probe".to_string(),
		create: std::sync::Arc::new(move || (probe_core(), Box::new(Probe))),
	});
	assert!(registered, "the probe type id was not registered before");

	// ...and the same snapshot now rebuilds the node in-process, with
	// its type id and standard value intact.
	let loaded = oak_node::serializer::load(&xml).unwrap();
	let l = loaded.lock().unwrap();
	let probe = l
		.graph
		.node_ids()
		.into_iter()
		.find(|id| l.graph.get(*id).map(|e| e.behavior.type_id()) == Some(TYPE_ID))
		.expect("loaded project has the dynamic node");
	assert_eq!(
		l.graph.get(probe).unwrap().core.standard_value(INPUT_ID, -1),
		NodeValue::Float(2.5),
		"the standard value survives the rebuild"
	);
}

/// A clip with multicam enabled round-trips: the `current_in` value, the
/// `sequence_in` edge, and the `sequence_type_in` selector all survive.
#[test]
fn multicam_clip_round_trip_preserves_wiring() {
	use oak_core::Rational;
	use oak_node::block::{clip_create, ClipBlockBehavior};
	use oak_node::node::NodeCore;
	use oak_node::nodes::multicamnode::{
		create, CURRENT_INPUT, SEQUENCE_INPUT, SEQUENCE_TYPE_INPUT,
	};
	use oak_node::project::Project;
	use oak_node::sequence::SequenceBehavior;
	use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
	use oak_node::value::NodeValue;

	let project = Project::new();
	let (seq_id, clip_id, mc_id) = {
		let mut p = project.lock().unwrap();
		let (core, behavior) = SequenceBehavior::create();
		let seq_id = p.graph.add_node(core, behavior);
		let (core, behavior) = TrackListBehavior::create();
		let list_id = p.graph.add_node(core, behavior);
		let (core, behavior) = (NodeCore::new(), Box::new(TrackBehavior::new(TrackType::Video)));
		let track_id = p.graph.add_node(core, behavior);
		{
			let seq = p.graph.get_mut(seq_id).unwrap();
			let s = seq
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<SequenceBehavior>()
				.unwrap();
			s.track_lists.push(list_id);
		}
		let list = p.graph.get_mut(list_id).unwrap();
		let l = list
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackListBehavior>()
			.unwrap();
		l.sequence = Some(seq_id);
		l.tracks.push(track_id);
		let track = p.graph.get_mut(track_id).unwrap();
		let t = track
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackBehavior>()
			.unwrap();
		t.kind = TrackType::Video;
		t.track_list = Some(list_id);

		// A clip on the track.
		let (core, behavior) = clip_create();
		let clip_id = p.graph.add_node(core, behavior);
		let clip = p.graph.get_mut(clip_id).unwrap();
		let c = clip
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<ClipBlockBehavior>()
			.unwrap();
		c.core.range = oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(100, 1));
		c.core.track = Some(track_id);
		let track = p.graph.get_mut(track_id).unwrap();
		let t = track
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TrackBehavior>()
			.unwrap();
		t.blocks.push(clip_id);

		// The multicam node routed between the sequence and the clip.
		let (core, behavior) = create();
		let mc_id = p.graph.add_node(core, behavior);
		p.graph
			.connect(mc_id, clip_id, "tex_in", -1)
			.unwrap();
		p.graph.connect(seq_id, mc_id, SEQUENCE_INPUT, -1).unwrap();
		let mc = p.graph.get_mut(mc_id).unwrap();
		mc.core
			.set_standard_value(CURRENT_INPUT, -1, NodeValue::Combo(2));
		mc.core.set_standard_value(
			SEQUENCE_TYPE_INPUT,
			-1,
			NodeValue::Combo(0),
		);
		(seq_id, clip_id, mc_id)
	};
	let _ = (seq_id, clip_id, mc_id);

	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	let loaded = oak_node::serializer::load(&xml).unwrap();
	let l = loaded.lock().unwrap();
	// The multicam node round-tripped with its current_in and type selector.
	let loaded_mc = l
		.graph
		.node_ids()
		.into_iter()
		.find(|id| {
			l.graph.get(*id).map(|e| e.behavior.type_id())
				== Some("org.olivevideoeditor.Olive.multicam")
		})
		.expect("loaded project has a MultiCamNode");
	assert_eq!(
		l.graph.get(loaded_mc).unwrap().core.standard_value(CURRENT_INPUT, -1),
		NodeValue::Int(2),
		"current_in survives"
	);
	assert_eq!(
		l.graph.get(loaded_mc).unwrap().core.standard_value(SEQUENCE_TYPE_INPUT, -1),
		NodeValue::Int(0),
		"sequence_type_in survives"
	);

	// The wiring survives: a sequence feeds the multicam's sequence_in and
	// the multicam feeds a clip's tex_in.
	let seq_src = l
		.graph
		.connected_output(loaded_mc, SEQUENCE_INPUT, -1)
		.expect("sequence_in edge restored");
	assert_eq!(
		l.graph.get(seq_src).unwrap().behavior.type_id(),
		"org.olivevideoeditor.Olive.sequence"
	);
	let clip_dst = l
		.graph
		.output_connections(loaded_mc)
		.into_iter()
		.find(|(_, input, _)| input == "tex_in")
		.map(|(to, _, _)| to)
		.expect("multicam still feeds a clip's tex_in");
	assert_eq!(
		l.graph.get(clip_dst).unwrap().behavior.type_id(),
		"org.olivevideoeditor.Olive.clipblock"
	);

	// The multicam behavior's cached sequence reference was rebuilt from
	// the restored edge.
	let behavior = l.graph.get(loaded_mc).unwrap();
	let mc_node = behavior
		.behavior
		.as_any()
		.and_then(|a| a.downcast_ref::<oak_node::nodes::multicamnode::MultiCamNode>())
		.expect("loaded node is a MultiCamNode");
	assert_eq!(mc_node.sequence(), Some(seq_src));
}

/// The virtual endpoint pair round-trips: the saved file names both
/// endpoint types, the loaded graph rebuilds them with their identities,
/// and the default `input -> output` wiring survives.
#[test]
fn endpoints_round_trip_and_stay_wired() {
	use oak_node::nodes::graphendpoints::{
		GRAPH_INPUT_TYPE_ID, GRAPH_OUTPUT_INPUT, GRAPH_OUTPUT_TYPE_ID,
	};
	use oak_node::project::Project;

	let project = Project::new();
	let (input, output) = {
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		// Some real content so the round trip is not vacuous.
		let (core, behavior) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		p.graph.add_node(core, behavior);
		p.graph.endpoints().expect("initialize creates the pair")
	};

	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	assert!(
		xml.contains(GRAPH_INPUT_TYPE_ID) && xml.contains(GRAPH_OUTPUT_TYPE_ID),
		"both endpoints are written to the file"
	);

	let loaded = oak_node::serializer::load(&xml).unwrap();
	{
		let l = loaded.lock().unwrap();
		let (li, lo) = l
			.graph
			.endpoints()
			.expect("the endpoint pair survives the round trip");
		assert_eq!(
			(li, lo),
			(input, output),
			"loaded endpoints keep their identities"
		);
		assert_eq!(
			l.graph.connected_output(lo, GRAPH_OUTPUT_INPUT, -1),
			Some(li),
			"the default wiring survives"
		);
	}

	// Re-save is idempotent (no duplicate pair is added by the load-time
	// `ensure_endpoints`).
	let xml2 = {
		let l = loaded.lock().unwrap();
		oak_node::serializer::save(&l).unwrap()
	};
	assert_eq!(xml, xml2, "re-save is idempotent");
}

/// Migration: a project file saved before the endpoints existed (no
/// endpoint nodes at all) loads with the pair created and default-wired,
/// and re-loading the migrated save adds nothing further.
#[test]
fn legacy_project_without_endpoints_migrates_on_load() {
	use oak_node::nodes::graphendpoints::{
		GRAPH_INPUT_TYPE_ID, GRAPH_OUTPUT_INPUT, GRAPH_OUTPUT_TYPE_ID,
	};
	use oak_node::project::Project;

	let project = Project::new();
	let legacy_count = {
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		let (core, behavior) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		p.graph.add_node(core, behavior);
		// Simulate a pre-endpoint file: detach the pair before saving.
		// `remove_node` refuses endpoints by design, so the file-level
		// simulation goes through the transfer seam (`take_node`).
		let (input, output) = p.graph.endpoints().expect("initialize creates the pair");
		assert!(p.graph.take_node(input).is_some());
		assert!(p.graph.take_node(output).is_some());
		assert!(p.graph.endpoints().is_none());
		p.graph.node_count()
	};

	let xml = {
		let p = project.lock().unwrap();
		oak_node::serializer::save(&p).unwrap()
	};
	assert!(
		!xml.contains(GRAPH_INPUT_TYPE_ID) && !xml.contains(GRAPH_OUTPUT_TYPE_ID),
		"the simulated legacy file carries no endpoints"
	);

	let loaded = oak_node::serializer::load(&xml).unwrap();
	let (li, lo) = {
		let l = loaded.lock().unwrap();
		let (li, lo) = l.graph.endpoints().expect("load migrates the pair in");
		assert_eq!(
			l.graph.connected_output(lo, GRAPH_OUTPUT_INPUT, -1),
			Some(li),
			"the migrated pair is default-wired"
		);
		assert_eq!(
			l.graph.node_count(),
			legacy_count + 2,
			"exactly the missing pair is added"
		);
		(li, lo)
	};

	// The migrated save reloads without adding a second pair.
	let xml2 = {
		let l = loaded.lock().unwrap();
		oak_node::serializer::save(&l).unwrap()
	};
	let reloaded = oak_node::serializer::load(&xml2).unwrap();
	let r = reloaded.lock().unwrap();
	assert_eq!(r.graph.endpoints(), Some((li, lo)));
	assert_eq!(r.graph.node_count(), legacy_count + 2);
	assert_eq!(r.graph.connected_output(lo, GRAPH_OUTPUT_INPUT, -1), Some(li));
}
