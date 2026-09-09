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

//! Direct unit tests for the crate-internal engine pieces (value.rs,
//! node.rs, folder.rs, ops.rs, handle.rs, error.rs, input.rs). These
//! drive every conversion/interpolation/query arm so the implemented
//! modules hold the ≥80% coverage gate.

use oak_core::{Rational, TimeRange};

use oak_node::error::{Error, OAKNODE_E_INVALID};
use oak_node::handle::{self, CHandle, RefBox};
use oak_node::id::NodeId;
use oak_node::input::{flags, Input, ValueHint};
use oak_node::keyframe::{Interpolation, Keyframe};
use oak_node::node::{Category, NodeBehavior, NodeCore};
use oak_node::value::{
	oak, AudioParams, NodeValue, NodeValueTable, OakNodeValue, SampleBuffer, ValueType, VideoParams,
};

fn float(v: f64) -> NodeValue {
	NodeValue::Float(v)
}

/// value.rs: value_type / to_double / split / combine / with_scalar
/// across every variant.
#[test]
fn value_type_and_conversions() {
	// value_type for every variant.
	assert_eq!(NodeValue::None.value_type(), ValueType::None);
	assert_eq!(NodeValue::Int(1).value_type(), ValueType::Int);
	assert_eq!(float(1.0).value_type(), ValueType::Float);
	assert_eq!(NodeValue::Color([0.0; 4]).value_type(), ValueType::Color);
	assert_eq!(NodeValue::Text("x".into()).value_type(), ValueType::Text);
	assert_eq!(NodeValue::Boolean(true).value_type(), ValueType::Boolean);
	assert_eq!(
		NodeValue::Samples(SampleBuffer::default()).value_type(),
		ValueType::Samples
	);
	assert_eq!(
		NodeValue::Rational(Rational::new(1, 2)).value_type(),
		ValueType::Rational
	);
	assert_eq!(NodeValue::Vec2([0.0; 2]).value_type(), ValueType::Vec2);
	assert_eq!(NodeValue::Vec3([0.0; 3]).value_type(), ValueType::Vec3);
	assert_eq!(NodeValue::Vec4([0.0; 4]).value_type(), ValueType::Vec4);
	assert_eq!(NodeValue::Combo(0).value_type(), ValueType::Combo);
	assert_eq!(
		NodeValue::StrCombo("s".into()).value_type(),
		ValueType::StrCombo
	);
	assert_eq!(
		NodeValue::VideoParams(VideoParams::default()).value_type(),
		ValueType::VideoParams
	);
	assert_eq!(
		NodeValue::AudioParams(AudioParams::default()).value_type(),
		ValueType::AudioParams
	);
	assert_eq!(NodeValue::Binary(vec![1]).value_type(), ValueType::Binary);
	assert_eq!(
		NodeValue::NodeRef(NodeId::from_identity(2).unwrap()).value_type(),
		ValueType::NodeRef
	);
	assert_eq!(NodeValue::PushButton.value_type(), ValueType::PushButton);

	// to_double across the numeric surface.
	assert_eq!(NodeValue::Int(3).to_double(), 3.0);
	assert_eq!(NodeValue::Int(-4).to_double(), -4.0);
	assert_eq!(float(2.5).to_double(), 2.5);
	assert_eq!(NodeValue::Color([7.0, 0.0, 0.0, 0.0]).to_double(), 7.0);
	assert_eq!(NodeValue::Boolean(true).to_double(), 1.0);
	assert_eq!(NodeValue::Boolean(false).to_double(), 0.0);
	assert_eq!(NodeValue::Rational(Rational::new(1, 4)).to_double(), 0.25);
	assert_eq!(NodeValue::Vec2([9.0, 0.0]).to_double(), 9.0);
	assert_eq!(NodeValue::Vec3([8.0, 0.0, 0.0]).to_double(), 8.0);
	assert_eq!(NodeValue::Vec4([6.0, 0.0, 0.0, 0.0]).to_double(), 6.0);
	assert_eq!(NodeValue::Combo(5).to_double(), 5.0);
	assert_eq!(
		NodeValue::Text("x".into()).to_double(),
		0.0,
		"non-numeric -> 0"
	);
	assert_eq!(NodeValue::None.to_double(), 0.0);

	// can_interpolate.
	assert!(float(1.0).can_interpolate());
	assert!(NodeValue::Vec2([0.0; 2]).can_interpolate());
	assert!(!NodeValue::Int(1).can_interpolate());
	assert!(!NodeValue::Text("x".into()).can_interpolate());

	// ValueType helpers.
	assert_eq!(ValueType::Text.to_oak(), oak::STRING);
	assert_eq!(ValueType::StrCombo.to_oak(), oak::STRING);
	assert_eq!(ValueType::Combo.to_oak(), oak::COMBO);
	assert_eq!(ValueType::Texture.to_oak(), oak::NONE);
	assert!(ValueType::Text.is_string());
	assert!(ValueType::StrCombo.is_string());
	assert!(!ValueType::Float.is_string());
	assert_eq!(ValueType::Vec2.keyframe_track_count(), 2);
	assert_eq!(ValueType::Vec3.keyframe_track_count(), 3);
	assert_eq!(ValueType::Vec4.keyframe_track_count(), 4);
	assert_eq!(ValueType::Color.keyframe_track_count(), 4);
	assert_eq!(ValueType::Float.keyframe_track_count(), 1);
	assert!(ValueType::Rational.can_interpolate());
	assert!(ValueType::Color.can_interpolate());
	assert!(ValueType::Vec4.can_interpolate());
	assert!(!ValueType::Int.can_interpolate());
	assert!(!ValueType::Text.can_interpolate());
}

/// value.rs: split/combine/with_scalar/lerp arms.
#[test]
fn value_split_combine_lerp() {
	let eps = 1e-12;

	// split_into_tracks.
	let vec2 = NodeValue::Vec2([1.0, 2.0]);
	assert_eq!(
		vec2.split_into_tracks(ValueType::Vec2),
		vec![float(1.0), float(2.0)]
	);
	assert_eq!(
		NodeValue::Vec3([1.0, 2.0, 3.0]).split_into_tracks(ValueType::Vec3),
		vec![float(1.0), float(2.0), float(3.0)]
	);
	assert_eq!(
		NodeValue::Vec4([1.0, 2.0, 3.0, 4.0]).split_into_tracks(ValueType::Vec4),
		vec![float(1.0), float(2.0), float(3.0), float(4.0)]
	);
	assert_eq!(
		NodeValue::Color([1.0, 2.0, 3.0, 4.0]).split_into_tracks(ValueType::Color),
		vec![float(1.0), float(2.0), float(3.0), float(4.0)]
	);
	// Scalar types hold the whole value in track 0.
	assert_eq!(
		float(9.0).split_into_tracks(ValueType::Float),
		vec![float(9.0)]
	);
	assert_eq!(
		NodeValue::Int(7).split_into_tracks(ValueType::Int),
		vec![NodeValue::Int(7)]
	);

	// combine_tracks.
	assert_eq!(
		NodeValue::combine_tracks(&[float(1.0), float(2.0)], ValueType::Vec2),
		NodeValue::Vec2([1.0, 2.0])
	);
	assert_eq!(
		NodeValue::combine_tracks(&[float(1.0), float(2.0), float(3.0)], ValueType::Vec3),
		NodeValue::Vec3([1.0, 2.0, 3.0])
	);
	{
		let v = vec![float(1.0); 4];
		assert_eq!(
			NodeValue::combine_tracks(&v, ValueType::Vec4),
			NodeValue::Vec4([1.0; 4])
		);
		assert_eq!(
			NodeValue::combine_tracks(&v, ValueType::Color),
			NodeValue::Color([1.0; 4])
		);
	}
	// Short track lists pad with zeros; empty -> None.
	assert_eq!(
		NodeValue::combine_tracks(&[float(1.0)], ValueType::Vec2),
		NodeValue::Vec2([1.0, 0.0])
	);
	assert_eq!(
		NodeValue::combine_tracks(&[], ValueType::Float),
		NodeValue::None
	);
	assert_eq!(
		NodeValue::combine_tracks(&[float(3.0)], ValueType::Float),
		float(3.0)
	);

	// with_scalar for every declared type.
	assert_eq!(
		float(0.0).with_scalar(ValueType::Int, 4.0),
		NodeValue::Int(4)
	);
	assert_eq!(float(0.0).with_scalar(ValueType::Float, 4.0), float(4.0));
	assert_eq!(
		float(0.0).with_scalar(ValueType::Boolean, 1.0),
		NodeValue::Boolean(true)
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Boolean, 0.0),
		NodeValue::Boolean(false)
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Combo, 2.0),
		NodeValue::Combo(2)
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Color, 0.5),
		NodeValue::Color([0.5, 0.0, 0.0, 0.0])
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Vec2, 0.5),
		NodeValue::Vec2([0.5, 0.0])
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Vec3, 0.5),
		NodeValue::Vec3([0.5, 0.0, 0.0])
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Vec4, 0.5),
		NodeValue::Vec4([0.5, 0.0, 0.0, 0.0])
	);
	assert_eq!(
		float(0.0).with_scalar(ValueType::Texture, 0.5),
		float(0.0),
		"non-numeric declared type keeps the value"
	);

	// lerp component-wise.
	assert_eq!(float(0.0).lerp(&float(10.0), 0.5), float(5.0));
	match float(0.0).lerp(&float(10.0), 0.5) {
		NodeValue::Float(f) => assert!((f - 5.0).abs() < eps),
		_ => unreachable!(),
	}
	assert_eq!(
		NodeValue::Vec2([0.0, 0.0]).lerp(&NodeValue::Vec2([2.0, 4.0]), 0.5),
		NodeValue::Vec2([1.0, 2.0])
	);
	assert_eq!(
		NodeValue::Vec3([0.0; 3]).lerp(&NodeValue::Vec3([2.0; 3]), 0.5),
		NodeValue::Vec3([1.0; 3])
	);
	assert_eq!(
		NodeValue::Vec4([0.0; 4]).lerp(&NodeValue::Vec4([2.0; 4]), 0.5),
		NodeValue::Vec4([1.0; 4])
	);
	assert_eq!(
		NodeValue::Color([0.0; 4]).lerp(&NodeValue::Color([2.0; 4]), 0.5),
		NodeValue::Color([1.0; 4])
	);
	match NodeValue::Rational(Rational::new(0, 1))
		.lerp(&NodeValue::Rational(Rational::new(1, 1)), 0.5)
	{
		NodeValue::Rational(r) => assert!((r.to_f64() - 0.5).abs() < eps),
		_ => unreachable!(),
	}
	// Non-interpolable types snap to self.
	assert_eq!(
		NodeValue::Int(3).lerp(&NodeValue::Int(9), 0.5),
		NodeValue::Int(3)
	);
	assert_eq!(
		NodeValue::Text("a".into()).lerp(&NodeValue::Text("b".into()), 0.5),
		NodeValue::Text("a".into())
	);
}

/// value.rs: NodeValue equality + SampleBuffer default + Drop on
/// texture.
#[test]
fn value_equality_and_buffer() {
	assert_eq!(NodeValue::None, NodeValue::None);
	assert_ne!(NodeValue::None, float(1.0));
	assert_eq!(NodeValue::Int(1), NodeValue::Int(1));
	assert_ne!(NodeValue::Int(1), NodeValue::Int(2));
	assert_eq!(float(1.0), float(1.0));
	assert_eq!(NodeValue::Color([0.0; 4]), NodeValue::Color([0.0; 4]));
	assert_eq!(NodeValue::Text("a".into()), NodeValue::Text("a".into()));
	assert_eq!(NodeValue::Boolean(true), NodeValue::Boolean(true));
	assert_eq!(
		NodeValue::Rational(Rational::new(1, 2)),
		NodeValue::Rational(Rational::new(1, 2))
	);
	assert_eq!(NodeValue::Vec2([0.0; 2]), NodeValue::Vec2([0.0; 2]));
	assert_eq!(NodeValue::Vec3([0.0; 3]), NodeValue::Vec3([0.0; 3]));
	assert_eq!(NodeValue::Vec4([0.0; 4]), NodeValue::Vec4([0.0; 4]));
	assert_eq!(NodeValue::Combo(1), NodeValue::Combo(1));
	assert_eq!(
		NodeValue::StrCombo("s".into()),
		NodeValue::StrCombo("s".into())
	);
	assert_eq!(
		NodeValue::VideoParams(VideoParams::default()),
		NodeValue::VideoParams(VideoParams::default())
	);
	assert_eq!(
		NodeValue::AudioParams(AudioParams::default()),
		NodeValue::AudioParams(AudioParams::default())
	);
	assert_eq!(NodeValue::Binary(vec![1, 2]), NodeValue::Binary(vec![1, 2]));
	assert_eq!(
		NodeValue::NodeRef(NodeId::from_identity(4).unwrap()),
		NodeValue::NodeRef(NodeId::from_identity(4).unwrap())
	);
	assert_eq!(NodeValue::PushButton, NodeValue::PushButton);
	// Mismatched variants never equal.
	assert_ne!(float(1.0), NodeValue::Int(1));
	assert_ne!(NodeValue::Text("a".into()), NodeValue::StrCombo("a".into()));

	// SampleBuffer::default.
	let sb = SampleBuffer::default();
	assert_eq!(sb.channels, 0);
	assert_eq!(sb.sample_count, 0);
	assert!(sb.data.is_empty());

	// Samples equality compares payload.
	let a = SampleBuffer {
		format: oak_core::SampleFormat::F32,
		channels: 2,
		sample_count: 4,
		data: vec![0u8; 32],
	};
	let b = SampleBuffer {
		format: oak_core::SampleFormat::F32,
		channels: 2,
		sample_count: 4,
		data: vec![0u8; 32],
	};
	assert_eq!(NodeValue::Samples(a.clone()), NodeValue::Samples(b));
}

/// value.rs: the oaknode_value POD conversions.
#[test]
fn oaknode_value_pod_roundtrip() {
	// to_node_value for every POD kind.
	assert_eq!(
		OakNodeValue {
			kind: oak::INT,
			num: 42,
			den: 0,
			f: [0.0; 4]
		}
		.to_node_value(ValueType::Int)
		.unwrap(),
		NodeValue::Int(42)
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::COMBO,
			num: 1,
			den: 0,
			f: [0.0; 4]
		}
		.to_node_value(ValueType::Combo)
		.unwrap(),
		NodeValue::Int(1)
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::FLOAT,
			num: 0,
			den: 0,
			f: [2.5, 0.0, 0.0, 0.0]
		}
		.to_node_value(ValueType::Float)
		.unwrap(),
		float(2.5)
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::BOOL,
			num: 1,
			den: 0,
			f: [0.0; 4]
		}
		.to_node_value(ValueType::Boolean)
		.unwrap(),
		NodeValue::Boolean(true)
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::RATIONAL,
			num: 1,
			den: 2,
			f: [0.0; 4]
		}
		.to_node_value(ValueType::Rational)
		.unwrap(),
		NodeValue::Rational(Rational::new(1, 2))
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::COLOR,
			num: 0,
			den: 0,
			f: [1.0, 2.0, 3.0, 4.0]
		}
		.to_node_value(ValueType::Color)
		.unwrap(),
		NodeValue::Color([1.0, 2.0, 3.0, 4.0])
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::VEC2,
			num: 0,
			den: 0,
			f: [1.0, 2.0, 0.0, 0.0]
		}
		.to_node_value(ValueType::Vec2)
		.unwrap(),
		NodeValue::Vec2([1.0, 2.0])
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::VEC3,
			num: 0,
			den: 0,
			f: [1.0, 2.0, 3.0, 0.0]
		}
		.to_node_value(ValueType::Vec3)
		.unwrap(),
		NodeValue::Vec3([1.0, 2.0, 3.0])
	);
	assert_eq!(
		OakNodeValue {
			kind: oak::VEC4,
			num: 0,
			den: 0,
			f: [1.0, 2.0, 3.0, 4.0]
		}
		.to_node_value(ValueType::Vec4)
		.unwrap(),
		NodeValue::Vec4([1.0, 2.0, 3.0, 4.0])
	);
	// Invalid kinds are rejected.
	assert!(OakNodeValue::none()
		.to_node_value(ValueType::Float)
		.is_err());
	assert!(OakNodeValue {
		kind: oak::STRING,
		num: 0,
		den: 0,
		f: [0.0; 4]
	}
	.to_node_value(ValueType::Text)
	.is_err());

	// from_node_value: round-trips and error arms.
	let pod = OakNodeValue::from_node_value(ValueType::Float, &float(3.0)).unwrap();
	assert_eq!(pod.kind, oak::FLOAT);
	assert_eq!(pod.f[0], 3.0);

	let pod = OakNodeValue::from_node_value(ValueType::Int, &NodeValue::Int(7)).unwrap();
	assert_eq!(pod.kind, oak::INT);
	assert_eq!(pod.num, 7);
	let pod = OakNodeValue::from_node_value(ValueType::Combo, &NodeValue::Combo(2)).unwrap();
	assert_eq!(pod.num, 2);
	let pod = OakNodeValue::from_node_value(ValueType::Boolean, &NodeValue::Boolean(true)).unwrap();
	assert_eq!(pod.num, 1);
	let pod = OakNodeValue::from_node_value(
		ValueType::Rational,
		&NodeValue::Rational(Rational::new(3, 4)),
	)
	.unwrap();
	assert_eq!((pod.num, pod.den), (3, 4));
	// Rational declared with a non-rational payload coerces via to_double.
	let pod = OakNodeValue::from_node_value(ValueType::Rational, &float(1.5)).unwrap();
	assert_eq!((pod.num, pod.den), (1, 1));
	let pod = OakNodeValue::from_node_value(ValueType::None, &NodeValue::None).unwrap();
	assert_eq!(pod.kind, oak::NONE);
	let pod =
		OakNodeValue::from_node_value(ValueType::Color, &NodeValue::Color([1.0, 2.0, 3.0, 4.0]))
			.unwrap();
	assert_eq!(pod.f, [1.0, 2.0, 3.0, 4.0]);
	let pod = OakNodeValue::from_node_value(ValueType::Vec2, &NodeValue::Vec2([1.0, 2.0])).unwrap();
	assert_eq!((pod.f[0], pod.f[1]), (1.0, 2.0));
	let pod =
		OakNodeValue::from_node_value(ValueType::Vec3, &NodeValue::Vec3([1.0, 2.0, 3.0])).unwrap();
	assert_eq!(pod.f[2], 3.0);
	let pod =
		OakNodeValue::from_node_value(ValueType::Vec4, &NodeValue::Vec4([1.0, 2.0, 3.0, 4.0]))
			.unwrap();
	assert_eq!(pod.f[3], 4.0);
	// Error arms: string declared -> Invalid; wrong payload -> Failed;
	// non-POD declared -> Failed.
	assert!(OakNodeValue::from_node_value(ValueType::Text, &NodeValue::Text("x".into())).is_err());
	assert!(OakNodeValue::from_node_value(ValueType::Color, &float(1.0)).is_err());
	assert!(OakNodeValue::from_node_value(ValueType::Texture, &NodeValue::None).is_err());
}

/// node.rs: NodeCore helper surface.
#[test]
fn node_core_helpers() {
	let mut core = NodeCore::new();
	// new() adds enabled_in first.
	assert!(core.has_input("enabled_in"));
	assert_eq!(core.input_index("enabled_in"), Some(0));
	assert_eq!(core.inputs.len(), 1);
	assert_eq!(core.input_data_type("enabled_in"), Some(ValueType::Boolean));
	assert_eq!(core.input_flags("enabled_in"), 0);
	assert_eq!(core.input_display_name("enabled_in"), "enabled_in");

	// add_input + queries.
	let mut input = Input::new("val_in", ValueType::Float, float(0.0));
	input.flags |= flags::ARRAY | flags::NOT_KEYFRAMABLE;
	input.display_name = "Value".to_string();
	core.add_input(input);
	assert!(core.has_input("val_in"));
	assert_eq!(core.input_index("val_in"), Some(1));
	assert_eq!(core.input_data_type("val_in"), Some(ValueType::Float));
	assert_eq!(
		core.input_flags("val_in"),
		flags::ARRAY | flags::NOT_KEYFRAMABLE
	);
	assert_eq!(core.input_display_name("val_in"), "Value");
	assert_eq!(core.input_display_name("missing"), "missing");
	assert!(core.get_input("missing").is_none());
	assert!(core.get_input_mut("missing").is_none());

	// Standard values: fallback to default, then override.
	assert_eq!(core.standard_value("val_in", -1), float(0.0));
	core.set_standard_value("val_in", -1, float(5.0));
	assert_eq!(core.standard_value("val_in", -1), float(5.0));
	core.set_standard_value("val_in", 2, float(9.0));
	assert_eq!(core.standard_value("val_in", 2), float(9.0));
	assert_eq!(
		core.standard_value("val_in", 3),
		float(0.0),
		"unset element -> default"
	);

	// Array size / insert / remove.
	assert_eq!(core.input_array_size("val_in"), 0);
	core.input_array_insert("val_in", 0);
	core.input_array_insert("val_in", 1);
	assert_eq!(core.input_array_size("val_in"), 2);
	core.set_standard_value("val_in", 1, float(7.0));
	core.input_array_insert("val_in", 0); // shifts element 1 -> 2
	assert_eq!(
		core.standard_value("val_in", 2),
		float(7.0),
		"values shift on insert"
	);
	assert_eq!(
		core.standard_value("val_in", 1),
		float(0.0),
		"inserted slot cleared"
	);
	core.input_array_remove("val_in", 0);
	assert_eq!(
		core.standard_value("val_in", 1),
		float(7.0),
		"values shift on remove"
	);
	assert_eq!(core.input_array_size("val_in"), 2);
	core.input_array_remove("val_in", 99); // out of range: no-op
	assert_eq!(core.input_array_size("val_in"), 2);

	// value_at_time uses the standard value when the track is empty
	// (element 1 holds 7.0 after the shifts above).
	assert_eq!(
		core.value_at_time("val_in", 1, Rational::new(0, 1)),
		float(7.0)
	);
	assert_eq!(
		core.value_at_time("val_in", 9, Rational::new(0, 1)),
		float(0.0)
	);

	// Keyframe tracks.
	{
		let track = core.keyframe_track_mut("val_in", -1);
		track.set_key(Keyframe {
			time: Rational::new(0, 1),
			value: float(1.0),
			interpolation: Interpolation::Linear,
			bezier_in: (0.0, 0.0),
			bezier_out: (0.0, 0.0),
		});
	}
	// Element keyframes shift with array insert/remove.
	{
		let track = core.keyframe_track_mut("val_in", 0);
		track.set_key(Keyframe {
			time: Rational::new(1, 1),
			value: float(5.0),
			interpolation: Interpolation::Hold,
			bezier_in: (0.0, 0.0),
			bezier_out: (0.0, 0.0),
		});
	}
	core.input_array_insert("val_in", 0); // element-0 track shifts to 1
	assert!(core.keyframe_track("val_in", 0).is_none());
	assert!(core.keyframe_track("val_in", 1).is_some());
	core.input_array_remove("val_in", 0); // element-1 track shifts back to 0
	assert!(core.keyframe_track("val_in", 0).is_some());
	assert!(core.keyframe_track("val_in", 1).is_none());
	core.input_array_remove("val_in", 0);
	assert!(
		core.keyframe_track("val_in", 0).is_none(),
		"removed element drops its track"
	);
	assert!(core.keyframe_track("val_in", -1).is_some());
	assert!(core.keyframe_track("missing", -1).is_none());
	// value_at_time uses keyframes when the track is non-empty.
	assert_eq!(
		core.value_at_time("val_in", -1, Rational::new(0, 1)),
		float(1.0)
	);

	// Value hints.
	assert!(core.value_hint("val_in", -1).is_none());
	let hint = ValueHint {
		types: vec![ValueType::Texture],
		index: 0,
		tag: "0:1".to_string(),
	};
	core.set_value_hint("val_in", -1, hint.clone());
	match core.value_hint("val_in", -1) {
		Some(h) => {
			assert_eq!(h.types, &[ValueType::Texture]);
			assert_eq!(h.index, 0);
			assert_eq!(h.tag, "0:1");
		}
		None => panic!("hint missing"),
	}
	core.set_value_hint("val_in", -1, hint.clone()); // replace
	assert!(core.value_hint("val_in", -1).is_some());

	// Context positions.
	let ctx = NodeId::from_identity(7).unwrap();
	assert!(!core.context_contains(ctx));
	assert!(core.set_context_position(ctx, 1.0, 2.0, true));
	assert!(core.context_contains(ctx));
	assert!(
		!core.set_context_position(ctx, 3.0, 4.0, false),
		"replace returns false"
	);
	assert_eq!(core.context_positions.len(), 1);
	assert_eq!(core.context_positions[0].1, (3.0, 4.0));
	assert!(core.remove_from_context(ctx));
	assert!(!core.remove_from_context(ctx));

	// Links.
	core.links.push(NodeId::from_identity(9).unwrap());
	assert_eq!(core.links, vec![NodeId::from_identity(9).unwrap()]);

	// NodeCore::empty has no enabled_in.
	assert!(!NodeCore::empty().has_input("enabled_in"));
}

/// node.rs: every NodeBehavior default trait method runs without panic
/// and returns its documented neutral value.
#[test]
fn node_behavior_defaults() {
	use oak_node::node::NodeBehavior;

	struct Minimal;
	impl NodeBehavior for Minimal {
		fn name(&self) -> &str {
			"N"
		}
		fn type_id(&self) -> &str {
			"t"
		}
		fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
			Some(Box::new(Minimal))
		}
	}

	let mut b = Minimal;
	let core = NodeCore::new();
	assert_eq!(b.short_name(), "N");
	assert_eq!(b.categories(), &[] as &[Category]);
	assert_eq!(b.sub_category(), "");
	assert_eq!(b.description(), "");
	assert_eq!(b.input_name("x"), "x");
	assert_eq!(b.input_name("enabled_in"), "Enabled");
	assert_eq!(b.ignore_inputs_for_rendering(), &[] as &[String]);
	assert!(b
		.active_elements_at_time("in", Rational::new(0, 1))
		.is_empty());
	assert_eq!(b.video_cache_range(&core), TimeRange::default());
	assert_eq!(b.audio_cache_range(&core), TimeRange::default());
	assert!(b.value_hint_for_input("in").is_none());
	assert_eq!(b.connected_render_output(&core, "in", -1), None);
	let tr = TimeRange::new(Rational::new(0, 1), Rational::new(5, 1));
	assert_eq!(b.input_time_adjustment(&core, "in", -1, tr, true), tr);
	assert_eq!(b.output_time_adjustment(&core, "in", -1, tr, false), tr);

	// value / process_samples / generate_frame no-ops.
	let mut table = NodeValueTable::default();
	let mut row = std::collections::BTreeMap::new();
	b.value(&core, &row, Rational::new(0, 1), &mut table);
	assert!(table.is_empty());
	let mut samples = SampleBuffer::default();
	b.process_samples(&core, &row, tr, &mut samples);
	assert!(samples.data.is_empty());
	b.generate_frame(&core, &mut CHandle::null(), Rational::new(0, 1));
	assert!(b.shader_code("any").is_none());

	// gizmo / event defaults are inert.
	b.gizmo_update(&core, &row);
	let mut core_mut = NodeCore::new();
	b.gizmo_drag(&mut core_mut, true, 1.0, 2.0, 0);
	b.input_value_changed(&mut core_mut, "in", -1);
	b.input_connected(&mut core_mut, "in", -1, NodeId::from_identity(1).unwrap());
	b.input_disconnected(&mut core_mut, "in", -1, NodeId::from_identity(1).unwrap());
	b.output_connected(&mut core_mut, NodeId::from_identity(2).unwrap(), "in", -1);
	b.output_disconnected(&mut core_mut, NodeId::from_identity(2).unwrap(), "in", -1);
	b.connected_to_preview(&mut core_mut);
	b.added_to_graph(&mut core_mut);
	b.removed_from_graph(&mut core_mut);
	b.link_changed(&mut core_mut);
	assert!(b.duplicate(&core).is_some());

	// load_custom/save_custom/post_load/legacy id mapping.
	assert!(b.load_custom(&mut core_mut, &mut NoopReader));
	b.save_custom(&core, &mut NoopWriter);
	b.post_load(&mut core_mut);
	assert_eq!(b.map_legacy_input_id("old"), "old");
}

/// A reader/writer pair for the serializer-default coverage (the real
/// XML adapter lands with the serializer milestone).
struct NoopReader;
impl oak_node::serializer::XmlRead for NoopReader {
	fn next_start_element(&mut self) -> bool {
		false
	}
	fn name(&self) -> &str {
		""
	}
	fn attribute(&self, _name: &str) -> Option<String> {
		None
	}
	fn read_element_text(&mut self) -> String {
		String::new()
	}
	fn skip_current_element(&mut self) {}
}

struct NoopWriter;
impl oak_node::serializer::XmlWrite for NoopWriter {
	fn start_element(&mut self, _name: &str) {}
	fn end_element(&mut self) {}
	fn attribute(&mut self, _name: &str, _value: &str) {}
	fn text_element(&mut self, _name: &str, _text: &str) {}
}

/// node.rs: NodeCaches default + clone + enabled-in default value.
#[test]
fn node_caches_and_defaults() {
	let caches = oak_node::node::NodeCaches::default();
	assert!(caches.video.ctx.is_null());
	assert!(caches.thumbnail.ctx.is_null());
	assert!(caches.audio.ctx.is_null());
	assert!(caches.waveform.ctx.is_null());

	let core = NodeCore::new();
	assert_eq!(
		core.standard_value("enabled_in", -1),
		NodeValue::Boolean(true),
		"enabled defaults to true"
	);
}

/// folder.rs: FolderBehavior surface.
#[test]
fn folder_behavior_surface() {
	let mut folder = oak_node::folder::FolderBehavior::new("My Folder");
	assert_eq!(folder.name, "My Folder");
	assert!(folder.children.is_empty());
	folder.children.push(NodeId::from_identity(1).unwrap());

	let b: Box<dyn NodeBehavior> = Box::new(oak_node::folder::FolderBehavior::new("X"));
	assert_eq!(b.name(), "X");
	assert_eq!(b.type_id(), "org.olivevideoeditor.Olive.folder");
	assert_eq!(b.categories(), &[Category::Timeline]);
	assert!(b.duplicate(&NodeCore::new()).is_some());

	// create() builds a folder node (bare core, no enabled_in).
	let (core, behavior) = oak_node::folder::create("Root");
	assert!(!core.has_input("enabled_in"));
	assert_eq!(behavior.name(), "Root");

	// register() adds a folder entry to the registry table.
	let mut meta = Vec::new();
	oak_node::folder::register(&mut meta);
	assert_eq!(meta.len(), 1);
	assert_eq!(meta[0].type_id, "org.olivevideoeditor.Olive.folder");
	let (_, b2) = (meta[0].create)();
	assert_eq!(b2.name(), "Folder");
}

/// ops.rs: category names and copy_inputs.
#[test]
fn ops_category_and_copy_inputs() {
	use oak_node::graph::Graph;
	use oak_node::ops;

	assert_eq!(ops::category_name(Category::Output), "Output");
	assert_eq!(ops::category_name(Category::Effect), "Effect");
	assert_eq!(ops::category_name(Category::Generator), "Generator");
	assert_eq!(ops::category_name(Category::Input), "Input");
	assert_eq!(ops::category_name(Category::Math), "Math");
	assert_eq!(ops::category_name(Category::Color), "Color");
	assert_eq!(ops::category_name(Category::Distort), "Distort");
	assert_eq!(ops::category_name(Category::Filter), "Filter");
	assert_eq!(ops::category_name(Category::Keying), "Keying");
	assert_eq!(ops::category_name(Category::OpenFx), "OpenFX");
	assert_eq!(ops::category_name(Category::Timeline), "Timeline");
	assert_eq!(ops::category_name(Category::Group), "Group");

	// copy_inputs copies standard values (and connections when asked).
	let mut g = Graph::new();
	let mk = |g: &mut Graph, id: &str| {
		let mut core = NodeCore::new();
		core.add_input(Input::new(id, ValueType::Float, float(0.0)));
		g.add_node(core, Box::new(oak_node::nodes::EmptyBehavior))
	};
	let src = mk(&mut g, "val_in");
	let dst = mk(&mut g, "val_in");
	let other = mk(&mut g, "other_in");
	g.connect(src, other, "other_in", -1).unwrap();

	// Without connections: values copy, edges do not.
	g.get_mut(src)
		.unwrap()
		.core
		.set_standard_value("val_in", -1, float(42.0));
	ops::copy_inputs(&mut g, src, dst, false).unwrap();
	assert_eq!(
		g.get(dst).unwrap().core.standard_value("val_in", -1),
		float(42.0)
	);
	assert!(!g.is_input_connected(dst, "val_in", -1));

	// With connections: dst's matching input reconnects to src's sources.
	let up = mk(&mut g, "val_in");
	g.connect(up, src, "val_in", -1).unwrap();
	let dst2 = mk(&mut g, "val_in");
	ops::copy_inputs(&mut g, src, dst2, true).unwrap();
	assert_eq!(g.connected_output(dst2, "val_in", -1), Some(up));

	// Missing source/dest -> E_NOT_FOUND.
	assert!(ops::copy_inputs(&mut g, NodeId::INVALID, dst, false).is_err());
	assert!(ops::copy_inputs(&mut g, src, NodeId::INVALID, false).is_err());
}

/// handle.rs: null/is_null + owned-box refcount discipline (the
/// facade-facing surface; the guard* wrappers and make_borrowed were
/// removed with the crate's C exports).
#[test]
fn handle_boxing_discipline() {
	let null = CHandle::null();
	assert!(null.is_null());
	assert!(unsafe { handle::get::<u32>(&null) }.is_none());

	// make_owned / make_owned_with round-trip.
	let owned = handle::make_owned(7u32);
	let rb = owned.ctx as *const RefBox<u32>;
	unsafe {
		assert_eq!((*rb).refs.load(std::sync::atomic::Ordering::Relaxed), 1);
	}
	assert_eq!(unsafe { handle::get::<u32>(&owned) }, Some(&7u32));

	// make_owned_with uses a custom release.
	unsafe extern "C" fn custom_release(ctx: *mut std::ffi::c_void) {
		unsafe {
			let rb = ctx as *mut RefBox<String>;
			if (*rb).refs.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
				drop(Box::from_raw(rb));
			}
		}
	}
	let custom = handle::make_owned_with("hello".to_string(), custom_release);
	assert_eq!(
		unsafe { handle::get::<String>(&custom) },
		Some(&"hello".to_string())
	);

	// Release everything (single release each).
	for h in [owned, custom] {
		unsafe { (h.release.unwrap())(h.ctx) };
	}
}

/// error.rs: code mapping for every variant.
#[test]
fn error_codes_map() {
	assert_eq!(Error::Invalid.code(), OAKNODE_E_INVALID);
	assert_eq!(Error::State.code(), oak_node::error::OAKNODE_E_STATE);
	assert_eq!(
		Error::Failed("x".to_string()).code(),
		oak_node::error::OAKNODE_E_FAILED
	);
	assert_eq!(Error::NotFound.code(), oak_node::error::OAKNODE_E_NOT_FOUND);
	assert_eq!(Error::NoMem.code(), oak_node::error::OAKNODE_E_NOMEM);
	assert_eq!(oak_node::error::OAKNODE_OK, 0);
}

/// id.rs: identity packing and INVALID sentinel.
#[test]
fn node_id_identity_packing() {
	// from_identity(5) = index 5, generation 0.
	let id = NodeId::from_identity(5).unwrap();
	assert_eq!(id.index(), 5);
	assert_eq!(id.generation(), 0);
	assert_eq!(id.identity(), 5);
	let gen = NodeId::from_identity((3u64 << 32) | 5).unwrap();
	assert_eq!(gen.generation(), 3);
	assert_eq!(gen.index(), 5);
	assert_eq!(NodeId::from_identity(gen.identity()), Some(gen));
	assert!(NodeId::from_identity(0xdead).is_some());
	assert!(
		NodeId::from_identity(u32::MAX as u64).is_none(),
		"invalid index rejected"
	);
	assert!(!NodeId::INVALID.valid());
	assert!(id.valid());
}

/// value.rs: NodeValueTable row helpers (count/is_empty/clear/get).
#[test]
fn value_table_rows() {
	let mut t = NodeValueTable::default();
	assert!(t.is_empty());
	t.push(ValueType::Int, NodeValue::Int(1), None);
	t.push(ValueType::Float, float(2.0), Some("tag".to_string()));
	assert_eq!(t.count(), 2);
	assert!(!t.is_empty());
	assert_eq!(t.get(ValueType::Float), Some(&float(2.0)));
	assert_eq!(t.get(ValueType::Combo), None);
	t.clear();
	assert!(t.is_empty());
	assert_eq!(t.count(), 0);
}

/// TimeRange sanity (used by caches) — a smoke through oakcore-rs.
#[test]
fn time_range_smoke() {
	let r = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
	assert_eq!(r.length(), Rational::new(10, 1));
	assert!(r.contains(Rational::new(5, 1)));
	assert!(!r.contains(Rational::new(10, 1)));
}
