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

//! Phase-2 internal unit tests: block/track/colormanager/footage/
//! serializer/bridge::undo engine internals, closing the coverage gaps
//! the ffi contract tests leave open.

use oak_core::{Rational, TimeRange};

use oak_node::block::{BlockCore, ClipBlockBehavior, GapBlockBehavior, TransitionBlockBehavior};
use oak_node::colormanager::ColorManager;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::serializer::{string_to_value, value_to_string};
use oak_node::track::{
	pixel_height_to_internal_height, TrackBehavior, TrackListBehavior, TrackType,
};
use oak_node::value::{NodeValue, ValueType};

/// block.rs: BlockCore range/media arithmetic.
#[test]
fn block_core_ranges() {
	let mut core = BlockCore::default();
	assert_eq!(core.in_(), Rational::new(0, 1));
	assert_eq!(core.out(), Rational::new(1, 1));
	assert_eq!(core.length(), Rational::new(1, 1));
	assert_eq!(core.media_out(), Rational::new(1, 1));

	core.set_in(Rational::new(5, 1));
	assert_eq!(core.in_(), Rational::new(5, 1));
	assert_eq!(core.out(), Rational::new(6, 1), "length preserved");

	core.set_out(Rational::new(9, 1));
	assert_eq!(core.length(), Rational::new(4, 1));

	// media_out anchored: in shifts so out stays.
	core.media_in = Rational::new(2, 1);
	core.set_length_and_media_out(Rational::new(3, 1));
	assert_eq!(core.out(), Rational::new(9, 1), "out stays put");
	assert_eq!(core.length(), Rational::new(3, 1));

	// media_in anchored: in stays, out shifts.
	core.set_length_and_media_in(Rational::new(4, 1));
	assert_eq!(core.in_(), Rational::new(6, 1), "in stays put");
	assert_eq!(core.length(), Rational::new(4, 1));
}

/// block.rs: behavior constructors + node inputs.
#[test]
fn block_behaviors_and_inputs() {
	// Clip: new + the static inputs.
	let (core, behavior) = oak_node::block::clip_create();
	assert!(core.has_input("enabled_in"));
	assert!(core.has_input("media_in_in"));
	assert!(core.has_input("speed_in"));
	assert!(core.has_input("reverse_in"));
	assert!(core.has_input("maintain_audio_pitch_in"));
	assert!(core.has_input("loop_in"));
	assert_eq!(behavior.name(), "Clip");
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.clipblock");
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Clip");

	let clip = ClipBlockBehavior::new();
	assert_eq!(clip.core.length(), Rational::new(1, 1));
	assert!(clip.footage.is_none());

	// Gap.
	let (_core, behavior) = oak_node::block::gap_create();
	assert_eq!(behavior.name(), "Gap");
	assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.gapblock");
	let gap = GapBlockBehavior::new();
	assert_eq!(gap.core.speed, 1.0);

	// Transition.
	let (core, behavior) = oak_node::block::transition_create();
	assert!(core.has_input("out_block_in"));
	assert!(core.has_input("in_block_in"));
	assert_eq!(
		behavior.type_id(),
		"org.olivevideoeditor.Olive.transitionblock"
	);
	let t = TransitionBlockBehavior::new();
	assert_eq!(t.in_offset, Rational::new(0, 1));
	assert_eq!(t.out_offset, Rational::new(0, 1));
	assert!(!t.is_dual());
	let dup = behavior.duplicate(&core).unwrap();
	assert_eq!(dup.name(), "Transition");
}

/// track.rs: block-list ops with a stub range accessor.
#[test]
fn track_behavior_ops() {
	struct Ranges;
	impl oak_node::track::BlockRange for Ranges {
		fn in_(&self, b: NodeId) -> Rational {
			match b.index() {
				0 => Rational::new(0, 1),
				1 => Rational::new(5, 1),
				_ => Rational::new(10, 1),
			}
		}
		fn out(&self, b: NodeId) -> Rational {
			match b.index() {
				0 => Rational::new(5, 1),
				1 => Rational::new(10, 1),
				_ => Rational::new(20, 1),
			}
		}
	}

	let b0 = NodeId::from_identity(0).unwrap();
	let b1 = NodeId::from_identity(1).unwrap();
	let b2 = NodeId::from_identity(2).unwrap();

	let mut track = TrackBehavior::new(TrackType::Video);
	assert_eq!(track.kind, TrackType::Video);
	track.append_block(b0);
	track.append_block(b1);
	assert_eq!(track.block_at(1), Some(b1));
	assert_eq!(track.block_index(b1), Some(1));
	assert!(track.insert_block_after(b2, b0));
	assert_eq!(track.blocks, vec![b0, b2, b1]);
	let missing = NodeId::from_identity(99).unwrap();
	assert!(
		!track.insert_block_after(b2, missing),
		"missing ref rejected"
	);
	assert!(
		!track.insert_block_before(b1, missing),
		"missing ref rejected"
	);
	let mut track = TrackBehavior::new(TrackType::Audio);
	track.append_block(b0);
	track.append_block(b1);
	assert!(
		track.insert_block_after(b2, b0),
		"insert after a present ref"
	);
	assert_eq!(track.blocks, vec![b0, b2, b1]);
	track.insert_block_at_index(NodeId::from_identity(7).unwrap(), 99); // clamped

	// Range queries via the stub.
	assert_eq!(
		track.block_containing_time(Rational::new(7, 1), &Ranges),
		Some(b1)
	);
	assert_eq!(
		track.visible_block_at_time(Rational::new(5, 1), &Ranges),
		Some(b1),
		"inclusive in"
	);
	assert!(track.is_range_free(
		TimeRange::new(Rational::new(20, 1), Rational::new(25, 1)),
		&Ranges
	));
	assert!(!track.is_range_free(
		TimeRange::new(Rational::new(4, 1), Rational::new(6, 1)),
		&Ranges
	));
	assert_eq!(track.length(&Ranges), Rational::new(20, 1));
	assert_eq!(track.reference(), (1, 0));

	// Ripple remove + replace (the clamped-inserted id-7 block remains).
	let b7 = NodeId::from_identity(7).unwrap();
	assert!(track.ripple_remove_block(b2));
	assert_eq!(track.blocks, vec![b0, b1, b7]);
	assert!(track.replace_block(b0, b2));
	assert_eq!(track.blocks, vec![b2, b1, b7]);
}

/// track.rs: track list length + height conversions.
#[test]
fn tracklist_and_height() {
	struct Ranges;
	impl oak_node::track::TrackRange for Ranges {
		fn length(&self, t: NodeId) -> Rational {
			Rational::new(t.index() as i64 + 1, 1)
		}
	}
	let mut list = TrackListBehavior::new(TrackType::Audio);
	assert_eq!(list.kind, TrackType::Audio);
	list.tracks.push(NodeId::from_identity(2).unwrap());
	list.tracks.push(NodeId::from_identity(5).unwrap());
	assert_eq!(list.track_at(0), Some(NodeId::from_identity(2).unwrap()));
	assert_eq!(list.track_index(NodeId::from_identity(5).unwrap()), Some(1));
	assert_eq!(
		list.total_length(&Ranges),
		Rational::new(6, 1),
		"longest track"
	);

	// Height conversions (C++ Track::internal_height_to_pixel_height).
	assert_eq!(
		oak_node::track::internal_height_to_pixel_height(3.0),
		39,
		"default height in px"
	);
	assert_eq!(
		oak_node::track::internal_height_to_pixel_height(1.5),
		20,
		"minimum height in px"
	);
	assert_eq!(pixel_height_to_internal_height(39), 3.0);
}

/// footage.rs: remaining stream queries + cancel.
#[test]
fn footage_queries() {
	let mut f = FootageBehavior::new("x.mov");
	f.streams = vec![
		oak_node::footage::StreamInfo {
			index: 0,
			is_video: true,
			video: Some(oak_node::value::VideoParams::default()),
			audio: None,
			duration: Rational::new(10, 1),
		},
		oak_node::footage::StreamInfo {
			index: 1,
			is_video: true,
			video: Some(oak_node::value::VideoParams::default()),
			audio: None,
			duration: Rational::new(20, 1),
		},
		oak_node::footage::StreamInfo {
			index: 2,
			is_video: false,
			video: None,
			audio: Some(oak_node::value::AudioParams::default()),
			duration: Rational::new(30, 1),
		},
	];
	assert_eq!(f.video_stream_count(), 2);
	assert_eq!(f.audio_stream_count(), 1);
	assert_eq!(f.subtitle_stream_count(), 0);
	assert_eq!(f.duration(), Rational::new(30, 1));
	assert_eq!(
		f.video_length(),
		Rational::new(20, 1),
		"longest video stream"
	);
	assert!(f.video_params(1).is_some());
	assert!(f.video_params(5).is_none());
	assert!(f.audio_params(0).is_some());
	assert!(f.audio_params(5).is_none());
	f.set_cancel(true);
	assert!(f.is_cancelled());
	f.set_cancel(false);
	assert!(!f.is_cancelled());
}

/// colormanager.rs: state machine + listing fallbacks.
#[test]
fn color_manager_state() {
	let mut cm = ColorManager::new();
	assert!(!cm.is_loaded());
	assert!(cm.list_colorspaces().is_empty());
	assert!(cm.list_displays().is_empty());
	assert!(cm.list_views("").is_empty());
	assert!(cm.list_looks().is_empty());

	cm.initialize().unwrap();
	assert!(cm.is_loaded());
	assert_eq!(cm.list_colorspaces(), vec!["linear"]);
	assert_eq!(cm.list_displays(), vec!["sRGB"]);
	assert_eq!(cm.list_views("sRGB"), vec!["Standard"]);
	assert!(cm.list_looks().is_empty());

	cm.config_filename = "/custom.ocio".to_string();
	cm.update_config_from_filename().unwrap();
	assert!(cm.is_loaded(), "invalid file keeps the previous config");

	cm.set_up_default_config().unwrap();
	assert!(cm.is_loaded());

	let empty = ColorManager::new();
	assert!(empty.list_colorspaces().is_empty());
}

/// serializer.rs: value text codec round-trips for every declared type.
#[test]
fn serializer_value_codecs() {
	let cases = [
		(ValueType::Float, NodeValue::Float(3.5)),
		(ValueType::Int, NodeValue::Int(7)),
		(ValueType::Combo, NodeValue::Combo(2)),
		(ValueType::Boolean, NodeValue::Boolean(true)),
		(
			ValueType::Rational,
			NodeValue::Rational(Rational::new(3, 4)),
		),
		(ValueType::Text, NodeValue::Text("hello".to_string())),
		(
			ValueType::StrCombo,
			NodeValue::StrCombo("choice".to_string()),
		),
	];
	for (declared, value) in cases {
		let text = value_to_string(declared, &value, true);
		let back = string_to_value(declared, &text);
		match declared {
			ValueType::Float => assert_eq!(back.to_double(), 3.5),
			ValueType::Int | ValueType::Combo => assert_eq!(back.to_double(), value.to_double()),
			ValueType::Boolean => assert_eq!(back.to_double(), 1.0),
			ValueType::Rational => assert_eq!(back.to_double(), 0.75),
			ValueType::Text => assert_eq!(back, NodeValue::Text("hello".to_string())),
			ValueType::StrCombo => assert_eq!(back, NodeValue::StrCombo("choice".to_string())),
			_ => {}
		}
	}

	// Whole (non-key-track) vec/color forms use ":" separators.
	let v2 = NodeValue::Vec2([1.5, 2.5]);
	assert_eq!(value_to_string(ValueType::Vec2, &v2, false), "1.5:2.5");
	assert_eq!(
		string_to_value(ValueType::Vec2, "1.5:2.5"),
		NodeValue::Vec2([1.5, 2.5])
	);
	let c = NodeValue::Color([0.1, 0.2, 0.3, 0.4]);
	assert_eq!(
		value_to_string(ValueType::Color, &c, false),
		"0.1:0.2:0.3:0.4"
	);
	assert_eq!(
		string_to_value(ValueType::Color, "0.1:0.2:0.3:0.4"),
		NodeValue::Color([0.1, 0.2, 0.3, 0.4])
	);
	// Key-track form is the plain first component.
	assert_eq!(value_to_string(ValueType::Vec2, &v2, true), "1.5");

	// Interpolation mapping.
	assert_eq!(
		oak_node::serializer::interpolation_from_c(0),
		oak_node::keyframe::Interpolation::Linear
	);
	assert_eq!(
		oak_node::serializer::interpolation_from_c(1),
		oak_node::keyframe::Interpolation::Hold
	);
	assert_eq!(
		oak_node::serializer::interpolation_from_c(2),
		oak_node::keyframe::Interpolation::Bezier
	);
}

/// oakundo `UndoCommand`: closure commands + multi commands (direct Rust
/// calls, single-lib unification).
#[test]
fn undo_command_roundtrip() {
	use std::sync::atomic::{AtomicI32, Ordering};
	use std::sync::Arc;
	use oak_undo::undocommand::UndoCommand;

	fn from_closures(
		redo: impl FnMut() + Send + 'static,
		undo: impl FnMut() + Send + 'static,
	) -> UndoCommand {
		UndoCommand::from_closures(redo, undo)
	}

	let value = Arc::new(AtomicI32::new(0));
	let value_redo = value.clone();
	let value_undo = value.clone();
	let value_check = value.clone();
	let mut cmd = from_closures(
		move || {
			value_redo.fetch_add(1, Ordering::SeqCst);
		},
		move || {
			value_undo.fetch_sub(1, Ordering::SeqCst);
		},
	);

	// redo applies, undo reverts; redo_now is idempotent.
	cmd.redo_now();
	assert_eq!(value_check.load(Ordering::SeqCst), 1);
	cmd.redo_now();
	assert_eq!(
		value_check.load(Ordering::SeqCst),
		1,
		"redo no-ops when done"
	);
	cmd.undo_now();
	assert_eq!(value_check.load(Ordering::SeqCst), 0);
	cmd.undo_now();
	assert_eq!(
		value_check.load(Ordering::SeqCst),
		0,
		"undo no-ops when not done"
	);

	// Multi command batches children.
	let mut multi = UndoCommand::multi();
	let child = from_closures(
		{
			let value = value_check.clone();
			move || {
				value.fetch_add(1, Ordering::SeqCst);
			}
		},
		{
			let value = value_check.clone();
			move || {
				value.fetch_sub(1, Ordering::SeqCst);
			}
		},
	);
	multi.multi_add_child(child);
	multi.redo_now();
	assert_eq!(value_check.load(Ordering::SeqCst), 1);
	multi.undo_now();
	assert_eq!(value_check.load(Ordering::SeqCst), 0);
}

/// track.rs: branch coverage the ffi contract tests leave open —
/// `TrackType::from_c` invalid values, insert/remove/replace failure
/// paths, empty range queries, duplicates, and naming.
#[test]
fn track_edge_cases() {
	use oak_node::node::NodeBehavior;

	struct Ranges;
	impl oak_node::track::BlockRange for Ranges {
		fn in_(&self, b: NodeId) -> Rational {
			Rational::new(b.index() as i64, 1)
		}
		fn out(&self, b: NodeId) -> Rational {
			Rational::new(b.index() as i64 + 1, 1)
		}
	}
	struct Tr;
	impl oak_node::track::TrackRange for Tr {
		fn length(&self, t: NodeId) -> Rational {
			Rational::new(t.index() as i64, 1)
		}
	}

	// TrackType::from_c round-trip + invalid values.
	assert_eq!(TrackType::from_c(0), Some(TrackType::Video));
	assert_eq!(TrackType::from_c(1), Some(TrackType::Audio));
	assert_eq!(TrackType::from_c(2), Some(TrackType::Subtitle));
	assert_eq!(TrackType::from_c(3), None);
	assert_eq!(TrackType::from_c(-1), None);
	assert_eq!(TrackType::Subtitle.to_c(), 2);

	let b0 = NodeId::from_identity(0).unwrap();
	let b1 = NodeId::from_identity(1).unwrap();
	let b9 = NodeId::from_identity(9).unwrap();

	let mut track = TrackBehavior::new(TrackType::Subtitle);
	assert_eq!(track.name(), "Subtitle Track");
	assert!(
		!track.insert_block_before(b1, b9),
		"absent reference rejected"
	);
	track.append_block(b0);
	assert!(track.insert_block_before(b1, b0));
	assert_eq!(track.blocks, vec![b1, b0]);
	assert!(!track.remove_block(b9), "absent block");
	assert!(!track.replace_block(b9, b0), "absent replace");

	// Empty-track range queries.
	let empty = TrackBehavior::new(TrackType::Video);
	assert_eq!(empty.length(&Ranges), Rational::new(0, 1));
	assert_eq!(
		empty.block_containing_time(Rational::new(1, 1), &Ranges),
		None
	);
	assert_eq!(
		empty.visible_block_at_time(Rational::new(1, 1), &Ranges),
		None
	);
	assert!(empty.is_range_free(
		TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
		&Ranges
	));
	assert_eq!(empty.reference(), (0, 0), "video type, default index");

	// duplicate preserves the block list and kind.
	let dup = track.duplicate(&oak_node::node::NodeCore::new()).unwrap();
	let d = dup
		.as_any()
		.unwrap()
		.downcast_ref::<TrackBehavior>()
		.unwrap();
	assert_eq!(d.blocks, vec![b1, b0]);
	assert_eq!(d.kind, TrackType::Subtitle);
	assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.track");

	// TrackListBehavior empty queries + duplicate.
	let mut list = TrackListBehavior::new(TrackType::Video);
	assert_eq!(list.name(), "Video Tracks");
	assert_eq!(list.track_at(0), None);
	assert_eq!(list.track_index(b0), None);
	assert_eq!(list.total_length(&Tr), Rational::new(0, 1));
	list.tracks.push(b0);
	let dup = list.duplicate(&oak_node::node::NodeCore::new()).unwrap();
	let d = dup
		.as_any()
		.unwrap()
		.downcast_ref::<TrackListBehavior>()
		.unwrap();
	assert_eq!(d.tracks, vec![b0]);
	assert_eq!(d.kind, TrackType::Video);
	assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.tracklist");
}

/// footage.rs: probe failure path, proxy field round-trip, duplicate.
#[test]
fn footage_edge_cases() {
	use oak_node::node::NodeBehavior;

	let mut f = FootageBehavior::new("nonexistent.mov");
	// probe errors when oakcodec is unavailable (test build).
	assert!(f.probe().is_err());
	assert!(!f.valid, "failed probe leaves valid unset");
	assert_eq!(f.total_stream_count(), 0);

	// set_proxy / clear_proxy field round-trip.
	f.set_proxy("/p.mov", 2, 3, 4, true);
	assert_eq!(f.proxy, "/p.mov");
	assert_eq!(f.proxy_state, 2);
	assert_eq!(f.proxy_video_stream_index, 3);
	assert_eq!(f.proxy_preset_version, 4);
	assert!(f.proxy_enabled);
	f.clear_proxy();
	assert_eq!(f.proxy, "");
	assert_eq!(f.proxy_state, 0);
	assert_eq!(f.proxy_video_stream_index, -1);
	assert_eq!(f.proxy_preset_version, 0);
	assert!(!f.proxy_enabled);

	// duplicate preserves the fields.
	f.set_proxy("/p2.mov", 1, 0, 0, true);
	let dup = f.duplicate(&oak_node::node::NodeCore::new()).unwrap();
	let d = dup
		.as_any()
		.unwrap()
		.downcast_ref::<FootageBehavior>()
		.unwrap();
	assert_eq!(d.filename, "nonexistent.mov");
	assert_eq!(d.proxy, "/p2.mov");
	assert_eq!(d.proxy_state, 1);
	assert!(d.proxy_enabled);
	assert_eq!(dup.type_id(), "org.olivevideoeditor.Olive.footage");
}

/// sequence.rs: stream counts, verify_length, defaults, duplicate.
#[test]
fn sequence_edge_cases() {
	use oak_node::node::NodeBehavior;
	use oak_node::sequence::SequenceBehavior;

	let mut seq = SequenceBehavior::new();
	assert_eq!(seq.name(), "Sequence");
	assert_eq!(seq.type_id(), "org.olivevideoeditor.Olive.sequence");
	assert_eq!(seq.video_stream_count(), 0);
	assert_eq!(seq.audio_stream_count(), 0);
	seq.verify_length((
		Rational::new(10, 1),
		Rational::new(20, 1),
		Rational::new(30, 1),
	));
	assert_eq!(seq.last_length, Rational::new(30, 1));
	seq.set_default_parameters();
	assert_eq!(seq.video_stream_count(), 1, "default video params");
	assert_eq!(seq.audio_stream_count(), 1, "default audio params");

	let dup = seq.duplicate(&oak_node::node::NodeCore::new()).unwrap();
	let d = dup
		.as_any()
		.unwrap()
		.downcast_ref::<SequenceBehavior>()
		.unwrap();
	// C++ `copy()` clones without values/params; the duplicate is empty.
	assert_eq!(d.video_stream_count(), 0);
	let def = SequenceBehavior::default();
	assert_eq!(def.audio_stream_count(), 0);
}

/// group.rs: direct behavior edges the ffi contract tests do not reach —
/// metadata, id lookup, force-id minting, resolution through a real
/// passthrough, and duplication.
#[test]
fn group_behavior_edges() {
	use oak_node::node::NodeBehavior;
	use oak_node::nodes::group::{InnerInput, NodeGroup};
	use oak_node::project::Project;

	let project = Project::new();
	let (inner_id, group_id) = {
		let mut p = project.lock().unwrap();
		let (core, behavior) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		let inner = p.graph.add_node(core, behavior);
		let (gcore, gbehavior) = oak_node::nodes::group::create();
		let group = p.graph.add_node(gcore, gbehavior);
		(inner, group)
	};

	// Metadata + default state.
	{
		let p = project.lock().unwrap();
		let entry = p.graph.get(group_id).unwrap();
		let gb = entry
			.behavior
			.as_any()
			.unwrap()
			.downcast_ref::<NodeGroup>()
			.unwrap();
		assert_eq!(gb.name(), "Group");
		assert_eq!(gb.type_id(), "org.olivevideoeditor.Olive.group");
		assert_eq!(
			gb.description(),
			"A group of nodes that is represented as a single node."
		);
		assert!(gb.categories().is_empty());
		assert_eq!(gb.output_passthrough(), None);
		assert!(gb.passthroughs().is_empty());
		assert!(!gb.contains_input_passthrough(&InnerInput {
			node: inner_id,
			input: "param_a_in".to_string(),
			element: -1,
		}));
		assert_eq!(
			gb.id_of_passthrough(&InnerInput {
				node: inner_id,
				input: "param_a_in".to_string(),
				element: -1,
			}),
			""
		);
		assert_eq!(gb.input_name("param_a_in"), "param_a_in");
		assert_eq!(gb.input_from_id("param_a_in"), None);
	}

	// Add two passthroughs of the same inner input id from different
	// inner nodes: the second mints `param_a_in_2` (id-collision loop).
	let (g1, g2) = {
		let mut p = project.lock().unwrap();
		let (core2, behavior2) = (oak_node::factory::Factory::global()
			.find("org.olivevideoeditor.Olive.math")
			.unwrap()
			.create)();
		let inner2 = p.graph.add_node(core2, behavior2);
		let descriptor = p
			.graph
			.get(inner_id)
			.unwrap()
			.core
			.get_input("param_a_in")
			.unwrap()
			.clone();
		let descriptor2 = p
			.graph
			.get(inner2)
			.unwrap()
			.core
			.get_input("param_a_in")
			.unwrap()
			.clone();
		let entry = p.graph.get_mut(group_id).unwrap();
		let gb = entry
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<NodeGroup>()
			.unwrap();
		let id1 = gb.add_input_passthrough(
			&mut entry.core,
			InnerInput {
				node: inner_id,
				input: "param_a_in".to_string(),
				element: -1,
			},
			"",
			&descriptor,
		);
		let id2 = gb.add_input_passthrough(
			&mut entry.core,
			InnerInput {
				node: inner2,
				input: "param_a_in".to_string(),
				element: -1,
			},
			"",
			&descriptor2,
		);
		let id3 = gb.add_input_passthrough(
			&mut entry.core,
			InnerInput {
				node: inner2,
				input: "param_b_in".to_string(),
				element: -1,
			},
			"forced",
			&descriptor2,
		);
		(id1, (id2, id3, inner2))
	};
	assert_eq!(g1, "param_a_in");
	assert_eq!(g2.0, "param_a_in_2", "collision mints a suffixed id");
	assert_eq!(g2.1, "forced", "force_id is used verbatim");

	// id_of_passthrough / input_from_id / contains now resolve.
	{
		let p = project.lock().unwrap();
		let entry = p.graph.get(group_id).unwrap();
		let gb = entry
			.behavior
			.as_any()
			.unwrap()
			.downcast_ref::<NodeGroup>()
			.unwrap();
		let target = InnerInput {
			node: inner_id,
			input: "param_a_in".to_string(),
			element: -1,
		};
		assert!(gb.contains_input_passthrough(&target));
		assert_eq!(gb.id_of_passthrough(&target), "param_a_in");
		assert!(gb.input_from_id("param_a_in").is_some());
		assert!(gb.input_from_id("nope").is_none());
	}

	// get_inner rewrites a group-passthrough input to the inner node;
	// resolve_input follows it to the end.
	{
		let p = project.lock().unwrap();
		let mut input = InnerInput {
			node: group_id,
			input: "param_a_in".to_string(),
			element: -1,
		};
		assert!(NodeGroup::get_inner(&p.graph, &mut input));
		assert_eq!(input.node, inner_id);
		assert_eq!(input.input, "param_a_in");
		let resolved = NodeGroup::resolve_input(
			&p.graph,
			InnerInput {
				node: group_id,
				input: "param_a_in".to_string(),
				element: -1,
			},
		);
		assert_eq!(resolved.node, inner_id);
		// A non-group node does not resolve through.
		let mut noop = InnerInput {
			node: inner_id,
			input: "param_a_in".to_string(),
			element: -1,
		};
		assert!(!NodeGroup::get_inner(&p.graph, &mut noop));
	}

	// duplicate clones the passthrough table and output reference.
	{
		let mut p = project.lock().unwrap();
		p.graph
			.get_mut(group_id)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<NodeGroup>()
			.unwrap()
			.set_output_passthrough(Some(inner_id));
		let entry = p.graph.get(group_id).unwrap();
		let gb = entry
			.behavior
			.as_any()
			.unwrap()
			.downcast_ref::<NodeGroup>()
			.unwrap();
		assert_eq!(gb.output_passthrough(), Some(inner_id));
		let dup = gb.duplicate(&entry.core).unwrap();
		let d = dup.as_any().unwrap().downcast_ref::<NodeGroup>().unwrap();
		assert_eq!(d.output_passthrough(), Some(inner_id));
		assert_eq!(d.passthroughs().len(), 3);
	}

	// remove_input_passthrough clears the table entry.
	{
		let mut p = project.lock().unwrap();
		let entry = p.graph.get_mut(group_id).unwrap();
		let gb = entry
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<NodeGroup>()
			.unwrap();
		gb.remove_input_passthrough(
			&mut entry.core,
			&InnerInput {
				node: inner_id,
				input: "param_a_in".to_string(),
				element: -1,
			},
		);
		assert_eq!(gb.passthroughs().len(), 2);
	}
}

/// group.rs: the custom XML serialization half (`save_custom` /
/// `load_custom` / `post_load`), which needs the oakcommon XML bridge.
#[cfg(feature = "test-stubs")]
#[test]
fn group_serialization() {
	use oak_node::node::NodeBehavior;
	use oak_node::nodes::group::NodeGroup;

	let mut gb = oak_node::nodes::group::create().1;
	let gb = gb
		.as_any_mut()
		.unwrap()
		.downcast_mut::<NodeGroup>()
		.unwrap();

	// save_custom writes the passthrough elements.
	let mut writer = oak_node::serializer::XmlWriterBridge::new().unwrap();
	{
		use oak_node::node::NodeBehavior;
		let core = oak_node::node::NodeCore::new();
		gb.save_custom(&core, &mut writer);
	}
	let xml = writer.output();
	assert!(xml.contains("inputpassthroughs"), "writes the container");

	// load_custom parses them back (node references deferred).
	let mut gb = oak_node::nodes::group::create().1;
	let gb = gb
		.as_any_mut()
		.unwrap()
		.downcast_mut::<NodeGroup>()
		.unwrap();
	let mut reader = oak_node::serializer::XmlReaderBridge::new(&xml).unwrap();
	{
		use oak_node::node::NodeBehavior;
		let mut core = oak_node::node::NodeCore::new();
		assert!(gb.load_custom(&mut core, &mut reader));
	}
	gb.post_load(&mut oak_node::node::NodeCore::new());
}
