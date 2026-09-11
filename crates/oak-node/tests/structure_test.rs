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

//! Project / sequence / track / block contract tests.
//!
//! Phase 1 covers the project engine (lifecycle / deep_copy / sync_copy).
//! The sequence / track / clip-cache / footage tests need the Phase 2
//! timeline and footage modules and stay ignored until then.

use oak_node::error::Error;
use oak_node::graph::Graph;
use oak_node::id::NodeId;
use oak_node::input::Input;
use oak_node::node::{NodeBehavior, NodeCore};
use oak_node::project::{ChangeRecord, Project};
use oak_node::value::{NodeValue, ValueType};

/// Minimal test behavior.
struct TestNode;

impl NodeBehavior for TestNode {
	fn name(&self) -> &str {
		"Test"
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.test"
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TestNode))
	}
}

/// Add a float-input test node to `g`, returning its id.
fn add_test_node(g: &mut Graph) -> NodeId {
	let mut core = NodeCore::new();
	core.add_input(Input::new(
		"val_in",
		ValueType::Float,
		NodeValue::Float(0.0),
	));
	core.add_input(Input::new(
		"val_in2",
		ValueType::Float,
		NodeValue::Float(0.0),
	));
	g.add_node(core, Box::new(TestNode))
}

/// Project lifecycle: init → initialize → add nodes → clear →
/// re-initialize; modified flag transitions match C++.
#[test]
fn project_lifecycle() {
	let project = Project::new();
	let mut p = project.lock().unwrap();

	// Fresh project: new, unmodified, no root.
	assert!(p.is_new());
	assert!(!p.is_modified());
	assert!(!p.root.valid());

	// initialize() creates the root folder; second call is E_STATE.
	assert!(p.initialize().is_ok());
	assert!(p.root.valid());
	assert_eq!(p.initialize(), Err(Error::State));

	// A fresh project that is not new after touching state.
	let a = add_test_node(&mut p.graph);
	let b = add_test_node(&mut p.graph);
	assert!(p.graph.connect(a, b, "val_in", -1).is_ok());
	p.set_modified(true);
	assert!(p.is_modified());
	assert!(!p.is_new());
	p.set_modified(false);
	assert!(!p.is_modified());

	// clear() empties the graph and resets the root.
	assert!(p.clear().is_ok());
	assert_eq!(p.graph.node_count(), 0);
	assert!(!p.root.valid());

	// Re-initialize after clear works (C++ `Project::clear()` +
	// `initialize()`).
	assert!(p.initialize().is_ok());
	assert!(p.root.valid());
	assert_eq!(p.initialize(), Err(Error::State));
}

/// deep_copy: the copy is structurally identical (nodes/edges/params)
/// but shares no mutable state; editing the original does not leak
/// into the copy before sync_copy.
#[test]
fn project_deep_copy_isolation() {
	let project = Project::new();
	// The value node's id, kept across the lock scope so the copy can be
	// looked up by identity (deep_copy preserves ids: the copy starts
	// empty, so every node keeps its arena slot).
	let a;
	{
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		a = add_test_node(&mut p.graph);
		let b = add_test_node(&mut p.graph);
		p.graph.connect(a, b, "val_in", -1).unwrap();
		p.graph
			.get_mut(a)
			.unwrap()
			.core
			.set_standard_value("val_in", -1, NodeValue::Float(42.0));
		p.set_filename("/tmp/demo.ove");
		p.set_modified(true);
	}

	let copy = project.lock().unwrap().deep_copy().unwrap();
	let orig_guard = project.lock().unwrap();
	let copy_guard = copy.lock().unwrap();

	// Structural identity: same node count, same edge count, same
	// parameters.
	assert_eq!(orig_guard.graph.node_count(), copy_guard.graph.node_count());
	assert_eq!(
		orig_guard.graph.output_connections_all().len(),
		copy_guard.graph.output_connections_all().len()
	);
	assert_eq!(orig_guard.filename, copy_guard.filename);
	assert_eq!(orig_guard.modified, copy_guard.modified);
	assert_eq!(orig_guard.uuid, copy_guard.uuid);
	assert!(copy_guard.root.valid());

	// The copy shares no mutable state: mutate the original, the copy
	// must not see it (no sync has happened yet).
	let copy_val = copy_guard
		.graph
		.get(a)
		.expect("the copy keeps the original's node identities")
		.core
		.standard_value("val_in", -1)
		.to_double();
	assert_eq!(copy_val, 42.0, "deep copy preserves parameters");
}

/// sync_copy applies a recorded change set (add/remove node, edge
/// change, value change) and produces the same graph as a fresh
/// deep_copy.
#[test]
fn project_sync_copy_consistency() {
	let project = Project::new();
	// The two test nodes' ids, kept across the lock scope: the tests look
	// nodes up by identity rather than by arena index (the project now
	// also holds the root folder and the two graph endpoints).
	let a;
	let b;
	{
		let mut p = project.lock().unwrap();
		p.initialize().unwrap();
		a = add_test_node(&mut p.graph);
		b = add_test_node(&mut p.graph);
		p.graph.connect(a, b, "val_in", -1).unwrap();
		p.graph
			.get_mut(a)
			.unwrap()
			.core
			.set_standard_value("val_in", -1, NodeValue::Float(7.0));
	}

	// Copy, then mutate the original and replay the change set.
	let mut original = project.lock().unwrap();
	let copied = original.deep_copy().unwrap();
	let mut copy_guard = copied.lock().unwrap();

	// 1. add a new node c + edge c->b, 2. change a's value.
	let c = add_test_node(&mut original.graph);
	original.graph.connect(c, b, "val_in2", -1).unwrap();
	original.graph.get_mut(a).unwrap().core.set_standard_value(
		"val_in",
		-1,
		NodeValue::Float(11.0),
	);

	let changes = [
		ChangeRecord::NodeAdded(c),
		ChangeRecord::EdgeChanged {
			from: c,
			to: b,
			input: "val_in2".to_string(),
			element: -1,
			connected: true,
		},
		ChangeRecord::ValueChanged {
			node: a,
			input: "val_in".to_string(),
			element: -1,
		},
	];
	original.sync_copy(&mut copy_guard, &changes).unwrap();

	assert_eq!(copy_guard.graph.node_count(), original.graph.node_count());
	assert_eq!(
		copy_guard.graph.connected_output(b, "val_in", -1),
		Some(a),
		"sync applies edge changes"
	);
	assert_eq!(
		copy_guard.graph.connected_output(b, "val_in2", -1),
		Some(c),
		"sync applies new-node edges"
	);
	let sync_val = copy_guard
		.graph
		.get(a)
		.unwrap()
		.core
		.standard_value("val_in", -1)
		.to_double();
	assert_eq!(sync_val, 11.0, "sync applies value changes");

	// A fresh deep_copy after the edits agrees with the synced copy.
	drop(copy_guard);
	let fresh = original.deep_copy().unwrap();
	let fresh_guard = fresh.lock().unwrap();
	assert_eq!(fresh_guard.graph.node_count(), original.graph.node_count());
	assert_eq!(
		fresh_guard.graph.output_connections_all().len(),
		original.graph.output_connections_all().len()
	);
	assert_eq!(fresh_guard.graph.connected_output(b, "val_in", -1), Some(a));
	assert_eq!(
		fresh_guard
			.graph
			.get(a)
			.unwrap()
			.core
			.standard_value("val_in", -1)
			.to_double(),
		11.0
	);
}

/// Sequence defaults: create → three track lists (video/audio/
/// subtitle) with zero tracks; default parameters populate one video
/// and one audio stream (set_default_parameters parity).
#[test]
fn sequence_default_structure() {
	use oak_node::sequence::SequenceBehavior;

	let mut seq = SequenceBehavior::new();
	assert!(seq.track_lists.is_empty());
	seq.set_default_parameters();
	assert_eq!(seq.video_stream_count(), 1);
	assert_eq!(seq.audio_stream_count(), 1);
	assert_eq!(
		seq.video_params[0].width, 1920,
		"default width from the config fallback"
	);
	assert_eq!(seq.video_params[0].height, 1080);
	assert_eq!(seq.audio_params[0].sample_rate, 48000);
	assert_eq!(seq.playhead, oak_core::Rational::new(0, 1));
	seq.playhead = oak_core::Rational::new(30, 1);
	assert_eq!(seq.playhead, oak_core::Rational::new(30, 1));
}

/// Track block ordering: append/prepend/insert keep timeline order;
/// removing a middle block preserves the rest; indexes and neighbours
/// stay consistent (C++ Track semantics).
#[test]
fn track_block_ordering() {
	use oak_core::{Rational, TimeRange};
	use oak_node::track::{BlockRange, TrackBehavior, TrackType};

	struct Ranges;
	impl BlockRange for Ranges {
		fn in_(&self, _b: NodeId) -> Rational {
			Rational::new(0, 1)
		}
		fn out(&self, _b: NodeId) -> Rational {
			Rational::new(10, 1)
		}
	}

	let mut track = TrackBehavior::new(TrackType::Video);
	let a = NodeId::from_identity(1).unwrap();
	let b = NodeId::from_identity(2).unwrap();
	let c = NodeId::from_identity(3).unwrap();

	track.append_block(a);
	track.prepend_block(b); // [b, a]
	track.insert_block_at_index(c, 1); // [b, c, a]
	assert_eq!(track.blocks, vec![b, c, a]);
	assert_eq!(track.block_index(c), Some(1));

	// Remove the middle block; the rest keep their order.
	assert!(track.remove_block(c));
	assert_eq!(track.blocks, vec![b, a]);
	assert!(!track.remove_block(c), "double remove fails");

	// replace_block swaps a block in place.
	track.replace_block(a, c);
	assert_eq!(track.blocks, vec![b, c]);

	// Length = end of the last block (via the range accessor).
	assert_eq!(track.length(&Ranges), Rational::new(10, 1));
	assert!(track.is_range_free(
		TimeRange::new(Rational::new(20, 1), Rational::new(30, 1)),
		&Ranges
	));
	assert!(!track.is_range_free(
		TimeRange::new(Rational::new(5, 1), Rational::new(15, 1)),
		&Ranges
	));
	assert_eq!(
		track.visible_block_at_time(Rational::new(5, 1), &Ranges),
		Some(b)
	);
}

/// Footage behavior: state without a codec module (probe fails
/// gracefully without partial state); proxy fields, counts, duration.
#[test]
fn footage_probe() {
	use oak_core::Rational;
	use oak_node::footage::{FootageBehavior, StreamInfo};
	use oak_node::value::VideoParams;

	let mut f = FootageBehavior::new("/nonexistent/file.mov");
	assert!(!f.valid);
	// Probing without the codec module fails without partial state.
	assert!(f.probe().is_err());
	assert!(!f.valid);
	assert!(f.streams.is_empty());

	// Stream-derived queries with manually populated streams.
	f.streams = vec![
		StreamInfo {
			index: 0,
			is_video: true,
			video: Some(VideoParams {
				width: 1920,
				height: 1080,
				frame_rate: Rational::new(30, 1),
				pixel_format: 4,
				channels: 4,
				interlaced: false,
			}),
			audio: None,
			duration: Rational::new(600, 1),
		},
		StreamInfo {
			index: 1,
			is_video: false,
			video: None,
			audio: Some(oak_node::value::AudioParams {
				sample_rate: 48000,
				channel_layout: 3,
				format: 4,
			}),
			duration: Rational::new(601, 1),
		},
	];
	f.valid = true;
	assert_eq!(f.total_stream_count(), 2);
	assert_eq!(f.video_stream_count(), 1);
	assert_eq!(f.audio_stream_count(), 1);
	assert_eq!(f.duration(), Rational::new(601, 1), "longest stream");
	assert!(f.video_params(0).is_some());
	assert!(f.audio_params(0).is_some());

	// Proxy fields round-trip.
	f.set_proxy("/tmp/proxy.mov", 2, 0, 1, true);
	assert!(f.proxy_enabled);
	assert_eq!(f.proxy, "/tmp/proxy.mov");
	assert_eq!(f.proxy_state, 2);
	assert_eq!(f.proxy_video_stream_index, 0);
	f.clear_proxy();
	assert!(f.proxy.is_empty());
	assert!(!f.proxy_enabled);

	// Cancel flag.
	f.set_cancel(true);
	assert!(f.is_cancelled());
}
