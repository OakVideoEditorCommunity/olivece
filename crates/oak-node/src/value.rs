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

//! The node value system: replaces `olive::Variant` + `NodeValue`
//! (C++ type-erasure) with a closed enum.
//!
//! Boundary note: cross-module payloads (textures, sample buffers)
//! arrive as refcounted C handles — the enum stores those handles by
//! value and releases on drop, which keeps the ownership chain inside
//! the refcount discipline instead of the C++ shared_ptr-in-Variant
//! model (the one documented exception of the C++ tree; it does not
//! exist here). Textures specifically are oakrender objects, and
//! oakrender depends on oaknode, so the payload must stay an opaque
//! [`crate::handle::CHandle`] at this boundary.

use std::ffi::c_int;

use oak_core::{Rational, SampleFormat};

/// `oaknode_value_type` discriminants (include/node/node.h), used by the
/// ffi layer to marshal [`NodeValue`]s across the C boundary.
pub mod oak {
	use std::ffi::c_int;

	/// `OAKNODE_VALUE_NONE` (types without a POD representation).
	pub const NONE: c_int = 0;
	/// `OAKNODE_VALUE_INT`.
	pub const INT: c_int = 1;
	/// `OAKNODE_VALUE_FLOAT`.
	pub const FLOAT: c_int = 2;
	/// `OAKNODE_VALUE_BOOL`.
	pub const BOOL: c_int = 3;
	/// `OAKNODE_VALUE_RATIONAL`.
	pub const RATIONAL: c_int = 4;
	/// `OAKNODE_VALUE_COLOR`.
	pub const COLOR: c_int = 5;
	/// `OAKNODE_VALUE_VEC2`.
	pub const VEC2: c_int = 6;
	/// `OAKNODE_VALUE_VEC3`.
	pub const VEC3: c_int = 7;
	/// `OAKNODE_VALUE_VEC4`.
	pub const VEC4: c_int = 8;
	/// `OAKNODE_VALUE_COMBO`.
	pub const COMBO: c_int = 9;
	/// `OAKNODE_VALUE_STRING` (string-family inputs; string APIs only).
	pub const STRING: c_int = 10;
}

/// Value type tag (mirrors C++ `NodeValue::Type`; the C ABI marshals
/// these as ints in `ffi.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
	/// No value.
	None,
	/// Integer.
	Int,
	/// Float (f64).
	Float,
	/// Color RGBA.
	Color,
	/// Text.
	Text,
	/// Boolean.
	Boolean,
	/// Texture handle (oakrender).
	Texture,
	/// Sample buffer (owned Rust buffer, F32 planar/packed).
	Samples,
	/// Rational time.
	Rational,
	/// Vec2/Vec3/Vec4.
	Vec2,
	/// Vec3.
	Vec3,
	/// Vec4.
	Vec4,
	/// 4x4 matrix (C++ `k_matrix`; row-major, 16 elements).
	Matrix,
	/// Combo index.
	Combo,
	/// String combo value.
	StrCombo,
	/// Video params.
	VideoParams,
	/// Audio params.
	AudioParams,
	/// Binary blob.
	Binary,
	/// Node reference (for node-typed inputs).
	NodeRef,
	/// Push button (no payload).
	PushButton,
	/// Parametric curve (OpenFX parametric param; the value payload is a
	/// [`NodeValue::Text`] JSON document, see
	/// `oak_plugin::param_curve::curves_to_json` — string-carried like
	/// [`ValueType::Text`]).
	Parametric,
}

/// A node value. `Texture` stores an oakrender handle; dropping the
/// value releases one reference, and cloning addrefs it (the C++
/// shared_ptr-in-`Variant` model, kept inside the refcount discipline —
/// a plain bitwise clone would double-release on drop).
#[derive(Debug)]
pub enum NodeValue {
	/// No value.
	None,
	/// Integer.
	Int(i64),
	/// Float.
	Float(f64),
	/// RGBA color.
	Color([f64; 4]),
	/// Text.
	Text(String),
	/// Boolean.
	Boolean(bool),
	/// Texture handle (owned reference).
	Texture(crate::handle::CHandle),
	/// Interleaved/planar sample payload + format.
	Samples(SampleBuffer),
	/// Rational.
	Rational(Rational),
	/// Vec2.
	Vec2([f64; 2]),
	/// Vec3.
	Vec3([f64; 3]),
	/// Vec4.
	Vec4([f64; 4]),
	/// 4x4 matrix, row-major 16 elements (C++ `k_matrix`).
	Matrix([f64; 16]),
	/// Array of `vec4` values (C++ bezier `points_in` array input; the
	/// polygon generator's point table, packed as `vec4[i]` uniforms).
	Vec4Array(Vec<[f64; 4]>),
	/// Combo index.
	Combo(i64),
	/// String combo.
	StrCombo(String),
	/// Video parameters (frame size/format/rate; plain data).
	VideoParams(VideoParams),
	/// Audio parameters (plain data).
	AudioParams(AudioParams),
	/// Opaque bytes.
	Binary(Vec<u8>),
	/// Reference to another node (identity + generation checked).
	NodeRef(crate::id::NodeId),
	/// Push button.
	PushButton,
}

/// Audio sample payload (owned).
#[derive(Clone, Debug)]
pub struct SampleBuffer {
	/// Format of `data`.
	pub format: SampleFormat,
	/// Channel count.
	pub channels: usize,
	/// Samples per channel.
	pub sample_count: usize,
	/// Raw payload (layout per `format`).
	pub data: Vec<u8>,
}

impl Default for SampleBuffer {
	/// Empty buffer (format `Invalid`, no channels/samples/data).
	fn default() -> Self {
		SampleBuffer {
			format: SampleFormat::Invalid,
			channels: 0,
			sample_count: 0,
			data: Vec::new(),
		}
	}
}

impl SampleBuffer {
	/// True when the payload is allocated (C++ `SampleBuffer::is_allocated`).
	pub fn is_allocated(&self) -> bool {
		!self.data.is_empty()
	}

	/// Byte offset of sample `index` of `channel` in `data`, per the
	/// format's layout: planar formats store channel-major planes of
	/// `sample_count` samples; packed formats interleave per frame.
	fn sample_offset(&self, channel: usize, index: usize) -> Option<usize> {
		if self.format == SampleFormat::Invalid
			|| channel >= self.channels
			|| index >= self.sample_count
		{
			return None;
		}
		let bps = self.format.bytes_per_sample();
		let stride = if self.format.is_planar() {
			self.sample_count
		} else {
			self.channels
		};
		let pos = if self.format.is_planar() {
			channel * self.sample_count + index
		} else {
			index * self.channels + channel
		};
		let byte = pos * bps;
		if byte + bps > self.data.len() {
			return None;
		}
		Some(byte)
	}

	/// Read one sample as `f64` (C++ `SampleBuffer::data(channel)[index]`,
	/// float pipeline). Out-of-range reads yield 0.0.
	pub fn sample_value(&self, channel: usize, index: usize) -> f64 {
		let byte = match self.sample_offset(channel, index) {
			Some(b) => b,
			None => return 0.0,
		};
		let bps = self.format.bytes_per_sample();
		let raw = &self.data[byte..byte + bps];
		match self.format {
			SampleFormat::U8Planar | SampleFormat::U8 => raw[0] as f64,
			SampleFormat::S16Planar | SampleFormat::S16 => {
				i16::from_le_bytes([raw[0], raw[1]]) as f64
			}
			SampleFormat::S32Planar
			| SampleFormat::S32
			| SampleFormat::F32Planar
			| SampleFormat::F32 => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
			SampleFormat::S64Planar | SampleFormat::S64 => {
				i64::from_le_bytes(raw.try_into().unwrap_or([0; 8])) as f64
			}
			SampleFormat::F64Planar | SampleFormat::F64 => {
				f64::from_le_bytes(raw.try_into().unwrap_or([0; 8]))
			}
			SampleFormat::Invalid => 0.0,
		}
	}

	/// Write one sample from an `f64` (C++
	/// `SampleBuffer::data(channel)[index] = value`, float pipeline).
	/// Out-of-range writes are ignored.
	pub fn set_sample_value(&mut self, channel: usize, index: usize, value: f64) {
		let byte = match self.sample_offset(channel, index) {
			Some(b) => b,
			None => return,
		};
		let bps = self.format.bytes_per_sample();
		let raw = &mut self.data[byte..byte + bps];
		match self.format {
			SampleFormat::U8Planar | SampleFormat::U8 => raw[0] = value as u8,
			SampleFormat::S16Planar | SampleFormat::S16 => {
				raw.copy_from_slice(&(value as i16).to_le_bytes())
			}
			SampleFormat::S32Planar | SampleFormat::S32 => {
				raw.copy_from_slice(&(value as i32).to_le_bytes())
			}
			SampleFormat::F32Planar | SampleFormat::F32 => {
				raw.copy_from_slice(&(value as f32).to_le_bytes())
			}
			SampleFormat::S64Planar | SampleFormat::S64 => {
				raw.copy_from_slice(&(value as i64).to_le_bytes())
			}
			SampleFormat::F64Planar | SampleFormat::F64 => {
				raw.copy_from_slice(&value.to_le_bytes())
			}
			SampleFormat::Invalid => {}
		}
	}

	/// Multiply every sample by `volume` in place (C++
	/// `SampleBuffer::transform_volume`).
	pub fn transform_volume(&mut self, volume: f64) {
		for c in 0..self.channels {
			for i in 0..self.sample_count {
				let v = self.sample_value(c, i);
				self.set_sample_value(c, i, v * volume);
			}
		}
	}

	/// Multiply one channel's samples by `volume` in place (C++
	/// `SampleBuffer::transform_volume_for_channel`).
	pub fn transform_volume_for_channel(&mut self, channel: usize, volume: f64) {
		if channel >= self.channels {
			return;
		}
		for i in 0..self.sample_count {
			let v = self.sample_value(channel, i);
			self.set_sample_value(channel, i, v * volume);
		}
	}
}

impl ValueType {
	/// Pinned mapping to `oaknode_value_type` (`// CPP-PARITY:
	/// src/node/c_api/valueconvert.h` `value_type_to_oak`). Types without a
	/// POD representation map to [`oak::NONE`].
	pub fn to_oak(self) -> c_int {
		match self {
			ValueType::Int => oak::INT,
			ValueType::Float => oak::FLOAT,
			ValueType::Boolean => oak::BOOL,
			ValueType::Rational => oak::RATIONAL,
			ValueType::Color => oak::COLOR,
			ValueType::Vec2 => oak::VEC2,
			ValueType::Vec3 => oak::VEC3,
			ValueType::Vec4 => oak::VEC4,
			ValueType::Combo => oak::COMBO,
			// String-carried types (k_file/k_text/k_font/k_str_combo;
			// parametric carries a JSON text).
			ValueType::Text | ValueType::StrCombo | ValueType::Parametric => oak::STRING,
			_ => oak::NONE,
		}
	}

	/// True for string-carried types (no POD representation; handled by
	/// the dedicated string getters/setters, `// CPP-PARITY: valueconvert.h`
	/// `value_type_is_string`).
	pub fn is_string(self) -> bool {
		matches!(
			self,
			ValueType::Text | ValueType::StrCombo | ValueType::Parametric
		)
	}

	/// Number of keyframe tracks the type splits into (C++
	/// `NodeValue::get_number_of_keyframe_tracks`).
	pub fn keyframe_track_count(self) -> usize {
		match self {
			ValueType::Vec2 => 2,
			ValueType::Vec3 => 3,
			ValueType::Vec4 | ValueType::Color => 4,
			_ => 1,
		}
	}

	/// The C++ `NodeValue::Type` enum discriminant (`src/node/src/value.h`).
	/// Used to serialize shader ids as `"<op>.<pairing>.<type_a>.<type_b>"`
	/// (`// CPP-PARITY: mathbase.cpp` `value_internal`). Types without a
	/// C++ counterpart (e.g. [`ValueType::VideoParams`]) map to
	/// `k_none = 0`.
	pub fn to_cpp_discriminant(self) -> i32 {
		match self {
			ValueType::None => 0,
			ValueType::Int => 1,
			ValueType::Float => 2,
			ValueType::Rational => 3,
			ValueType::Boolean => 4,
			ValueType::Color => 5,
			ValueType::Matrix => 6,
			ValueType::Text => 7,
			// k_font = 8 / k_file = 9 have no Rust type counterpart.
			ValueType::Texture => 10,
			ValueType::Samples => 11,
			ValueType::Vec2 => 12,
			ValueType::Vec3 => 13,
			ValueType::Vec4 => 14,
			// k_bezier = 15 has no Rust type counterpart.
			ValueType::Combo => 16,
			ValueType::StrCombo => 17,
			ValueType::VideoParams => 18,
			ValueType::AudioParams => 19,
			// k_subtitle_params = 20 has no Rust type counterpart.
			ValueType::Binary => 21,
			ValueType::PushButton => 22,
			// No C++ counterpart (k_none); the parametric curve payload
			// lives in the JSON text (k_bezier = 15 is a different model).
			ValueType::NodeRef | ValueType::Parametric => 0,
		}
	}

	/// Whether values of this type can be interpolated between keyframes
	/// (C++ `NodeValue::type_can_be_interpolated`; bezier is not a Rust
	/// value type).
	pub fn can_interpolate(self) -> bool {
		matches!(
			self,
			ValueType::Float
				| ValueType::Vec2
				| ValueType::Vec3
				| ValueType::Vec4
				| ValueType::Color
				| ValueType::Rational
		)
	}
}

impl NodeValue {
	/// The value's type tag.
	pub fn value_type(&self) -> ValueType {
		match self {
			NodeValue::None => ValueType::None,
			NodeValue::Int(_) => ValueType::Int,
			NodeValue::Float(_) => ValueType::Float,
			NodeValue::Color(_) => ValueType::Color,
			NodeValue::Text(_) => ValueType::Text,
			NodeValue::Boolean(_) => ValueType::Boolean,
			NodeValue::Texture(_) => ValueType::Texture,
			NodeValue::Samples(_) => ValueType::Samples,
			NodeValue::Rational(_) => ValueType::Rational,
			NodeValue::Vec2(_) => ValueType::Vec2,
			NodeValue::Vec3(_) => ValueType::Vec3,
			NodeValue::Vec4(_) => ValueType::Vec4,
			NodeValue::Vec4Array(_) => ValueType::Vec4,
			NodeValue::Matrix(_) => ValueType::Matrix,
			NodeValue::Combo(_) => ValueType::Combo,
			NodeValue::StrCombo(_) => ValueType::StrCombo,
			NodeValue::VideoParams(_) => ValueType::VideoParams,
			NodeValue::AudioParams(_) => ValueType::AudioParams,
			NodeValue::Binary(_) => ValueType::Binary,
			NodeValue::NodeRef(_) => ValueType::NodeRef,
			NodeValue::PushButton => ValueType::PushButton,
		}
	}

	/// Numeric conversion (C++ `Variant::to_double` used by keyframe
	/// interpolation); non-numeric payloads yield 0.0.
	pub fn to_double(&self) -> f64 {
		match self {
			NodeValue::Int(i) => *i as f64,
			NodeValue::Float(f) => *f,
			NodeValue::Color(c) => c[0],
			NodeValue::Boolean(b) => {
				if *b {
					1.0
				} else {
					0.0
				}
			}
			NodeValue::Rational(r) => r.to_f64(),
			NodeValue::Vec2(v) => v[0],
			NodeValue::Vec3(v) => v[0],
			NodeValue::Vec4(v) => v[0],
			NodeValue::Combo(i) => *i as f64,
			_ => 0.0,
		}
	}

	/// Whether this value can be interpolated (type-based).
	pub fn can_interpolate(&self) -> bool {
		self.value_type().can_interpolate()
	}

	/// Split a whole value into per-track components (C++
	/// `NodeValue::split_normal_value_into_track_values`). Scalar types
	/// split into a single element holding the whole value.
	pub fn split_into_tracks(&self, declared: ValueType) -> Vec<NodeValue> {
		let count = declared.keyframe_track_count();
		let mut vals = vec![NodeValue::None; count];
		match self {
			NodeValue::Vec2(v) => {
				vals[0] = NodeValue::Float(v[0]);
				vals[1] = NodeValue::Float(v[1]);
			}
			NodeValue::Vec3(v) => {
				vals[0] = NodeValue::Float(v[0]);
				vals[1] = NodeValue::Float(v[1]);
				vals[2] = NodeValue::Float(v[2]);
			}
			NodeValue::Vec4(v) => {
				vals[0] = NodeValue::Float(v[0]);
				vals[1] = NodeValue::Float(v[1]);
				vals[2] = NodeValue::Float(v[2]);
				vals[3] = NodeValue::Float(v[3]);
			}
			NodeValue::Color(c) => {
				vals[0] = NodeValue::Float(c[0]);
				vals[1] = NodeValue::Float(c[1]);
				vals[2] = NodeValue::Float(c[2]);
				vals[3] = NodeValue::Float(c[3]);
			}
			_ => {
				vals[0] = self.clone();
			}
		}
		vals
	}

	/// Recombine per-track components into a whole value (C++
	/// `NodeValue::combine_track_values_into_normal_value`). An empty
	/// slice yields [`NodeValue::None`].
	pub fn combine_tracks(tracks: &[NodeValue], declared: ValueType) -> NodeValue {
		if tracks.is_empty() {
			return NodeValue::None;
		}
		let comp = |i: usize| tracks.get(i).map(NodeValue::to_double).unwrap_or(0.0);
		match declared {
			ValueType::Vec2 => NodeValue::Vec2([comp(0), comp(1)]),
			ValueType::Vec3 => NodeValue::Vec3([comp(0), comp(1), comp(2)]),
			ValueType::Vec4 => NodeValue::Vec4([comp(0), comp(1), comp(2), comp(3)]),
			ValueType::Color => NodeValue::Color([comp(0), comp(1), comp(2), comp(3)]),
			_ => tracks[0].clone(),
		}
	}

	/// Interpolate between two values at `t` in [0, 1] (C++
	/// `get_split_value_at_time_on_track` linear path, `lerp(a,b,t) =
	/// a*(1-t)+b*t`; rational values re-quantize through
	/// `Rational::from_double`). Non-interpolable types snap to `self`.
	pub fn lerp(&self, other: &NodeValue, t: f64) -> NodeValue {
		match (self, other) {
			(NodeValue::Float(a), NodeValue::Float(b)) => NodeValue::Float(lerp_f(a, b, t)),
			(NodeValue::Color(a), NodeValue::Color(b)) => NodeValue::Color(lerp_arr4(a, b, t)),
			(NodeValue::Vec2(a), NodeValue::Vec2(b)) => NodeValue::Vec2(lerp_arr2(a, b, t)),
			(NodeValue::Vec3(a), NodeValue::Vec3(b)) => NodeValue::Vec3(lerp_arr3(a, b, t)),
			(NodeValue::Vec4(a), NodeValue::Vec4(b)) => NodeValue::Vec4(lerp_arr4(a, b, t)),
			(NodeValue::Rational(_), _) | (_, NodeValue::Rational(_)) => {
				let a = self.to_double();
				let b = other.to_double();
				NodeValue::Rational(Rational::from_double(lerp_f(&a, &b, t)))
			}
			_ => self.clone(),
		}
	}

	/// Rebuild a value of `declared` type carrying `scalar` as its single
	/// numeric payload (used by the per-track bezier evaluation, where the
	/// whole scalar value interpolates along the curve). Non-numeric
	/// declared types fall back to the scalar's numeric conversion.
	pub fn with_scalar(&self, declared: ValueType, scalar: f64) -> NodeValue {
		match declared {
			ValueType::Int => NodeValue::Int(scalar as i64),
			ValueType::Float => NodeValue::Float(scalar),
			ValueType::Boolean => NodeValue::Boolean(scalar != 0.0),
			ValueType::Combo => NodeValue::Combo(scalar as i64),
			ValueType::Color => NodeValue::Color([scalar, 0.0, 0.0, 0.0]),
			ValueType::Vec2 => NodeValue::Vec2([scalar, 0.0]),
			ValueType::Vec3 => NodeValue::Vec3([scalar, 0.0, 0.0]),
			ValueType::Vec4 => NodeValue::Vec4([scalar, 0.0, 0.0, 0.0]),
			_ => self.clone(),
		}
	}
}

/// `lerp` from the C++ `lerp.h` template: `a*(1.0 - t) + b*t`.
fn lerp_f(a: &f64, b: &f64, t: f64) -> f64 {
	(a * (1.0 - t)) + (b * t)
}

fn lerp_arr2(a: &[f64; 2], b: &[f64; 2], t: f64) -> [f64; 2] {
	[lerp_f(&a[0], &b[0], t), lerp_f(&a[1], &b[1], t)]
}

fn lerp_arr3(a: &[f64; 3], b: &[f64; 3], t: f64) -> [f64; 3] {
	[
		lerp_f(&a[0], &b[0], t),
		lerp_f(&a[1], &b[1], t),
		lerp_f(&a[2], &b[2], t),
	]
}

fn lerp_arr4(a: &[f64; 4], b: &[f64; 4], t: f64) -> [f64; 4] {
	[
		lerp_f(&a[0], &b[0], t),
		lerp_f(&a[1], &b[1], t),
		lerp_f(&a[2], &b[2], t),
		lerp_f(&a[3], &b[3], t),
	]
}

/// Video parameters (plain data; mirrors oak_core `VideoParams` C++
/// fields — the C ABI marshals field-by-field).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VideoParams {
	/// Width.
	pub width: i32,
	/// Height.
	pub height: i32,
	/// Frame rate.
	pub frame_rate: Rational,
	/// Pixel format as oakcore-rs enum discriminant.
	pub pixel_format: i32,
	/// Channel count.
	pub channels: i32,
	/// Whether the stream is interlaced.
	pub interlaced: bool,
}

/// Audio parameters (plain data).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioParams {
	/// Sample rate.
	pub sample_rate: i32,
	/// Channel layout mask.
	pub channel_layout: u64,
	/// Sample format discriminant.
	pub format: i32,
}

/// One row of evaluated inputs: input id -> value at a time.
pub type NodeValueRow = std::collections::BTreeMap<String, NodeValue>;

/// Evaluation output table (C++ `NodeValueTable`): ordered pushes with
/// optional source tags; `get` returns the last push of a type.
#[derive(Default, Debug)]
pub struct NodeValueTable {
	rows: Vec<(ValueType, NodeValue, Option<String>)>,
}

impl NodeValueTable {
	/// Push a value with an optional tag (C++ `push`).
	pub fn push(&mut self, ty: ValueType, value: NodeValue, tag: Option<String>) {
		self.rows.push((ty, value, tag));
	}

	/// Last pushed value of `ty` (C++ `get` semantics).
	pub fn get(&self, ty: ValueType) -> Option<&NodeValue> {
		self.rows
			.iter()
			.rev()
			.find(|(t, _, _)| *t == ty)
			.map(|(_, v, _)| v)
	}

	/// Number of pushed rows (C++ `count()`).
	pub fn count(&self) -> usize {
		self.rows.len()
	}

	/// True when the table holds no rows (C++ `is_empty()`).
	pub fn is_empty(&self) -> bool {
		self.rows.is_empty()
	}

	/// Clear all rows (C++ `clear()`).
	pub fn clear(&mut self) {
		self.rows.clear();
	}

	/// All rows `(type, value, tag)` in push order.
	pub fn rows(&self) -> &[(ValueType, NodeValue, Option<String>)] {
		&self.rows
	}

	/// Mutable row access (the render seam resolves job payloads into
	/// finished textures in place).
	pub fn rows_mut(&mut self) -> &mut Vec<(ValueType, NodeValue, Option<String>)> {
		&mut self.rows
	}
}

/// Structural equality: `Texture` compares by handle address, `Samples`
/// by payload. Mirrors the C++ `NodeValue::operator==` (type + tag +
/// data) for the types the crate supports; `None` equals only `None`.
impl PartialEq for NodeValue {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(NodeValue::None, NodeValue::None) => true,
			(NodeValue::Int(a), NodeValue::Int(b)) => a == b,
			(NodeValue::Float(a), NodeValue::Float(b)) => a == b,
			(NodeValue::Color(a), NodeValue::Color(b)) => a == b,
			(NodeValue::Text(a), NodeValue::Text(b)) => a == b,
			(NodeValue::Boolean(a), NodeValue::Boolean(b)) => a == b,
			(NodeValue::Texture(a), NodeValue::Texture(b)) => a.ctx == b.ctx,
			(NodeValue::Samples(a), NodeValue::Samples(b)) => {
				a.format == b.format
					&& a.channels == b.channels
					&& a.sample_count == b.sample_count
					&& a.data == b.data
			}
			(NodeValue::Rational(a), NodeValue::Rational(b)) => a == b,
			(NodeValue::Vec2(a), NodeValue::Vec2(b)) => a == b,
			(NodeValue::Vec3(a), NodeValue::Vec3(b)) => a == b,
			(NodeValue::Vec4(a), NodeValue::Vec4(b)) => a == b,
			(NodeValue::Matrix(a), NodeValue::Matrix(b)) => a == b,
			(NodeValue::Combo(a), NodeValue::Combo(b)) => a == b,
			(NodeValue::StrCombo(a), NodeValue::StrCombo(b)) => a == b,
			(NodeValue::VideoParams(a), NodeValue::VideoParams(b)) => a == b,
			(NodeValue::AudioParams(a), NodeValue::AudioParams(b)) => a == b,
			(NodeValue::Binary(a), NodeValue::Binary(b)) => a == b,
			(NodeValue::NodeRef(a), NodeValue::NodeRef(b)) => a == b,
			(NodeValue::PushButton, NodeValue::PushButton) => true,
			_ => false,
		}
	}
}

impl Clone for NodeValue {
	/// Clone with C++ `shared_ptr` semantics for [`NodeValue::Texture`]:
	/// the handle is copied and addref'd, so each clone owns one
	/// reference released on drop (a plain bitwise copy would
	/// double-release). All other variants are bitwise-copied.
	fn clone(&self) -> Self {
		match self {
			NodeValue::Texture(h) => {
				let mut h2 = h.clone();
				if let Some(f) = h2.addref {
					// Safety: `h2` is a valid handle; addref only touches
					// the refcount.
					unsafe { f(h2.ctx) };
				}
				NodeValue::Texture(h2)
			}
			NodeValue::None => NodeValue::None,
			NodeValue::Int(v) => NodeValue::Int(*v),
			NodeValue::Float(v) => NodeValue::Float(*v),
			NodeValue::Color(v) => NodeValue::Color(*v),
			NodeValue::Text(v) => NodeValue::Text(v.clone()),
			NodeValue::Boolean(v) => NodeValue::Boolean(*v),
			NodeValue::Samples(v) => NodeValue::Samples(v.clone()),
			NodeValue::Rational(v) => NodeValue::Rational(*v),
			NodeValue::Vec2(v) => NodeValue::Vec2(*v),
			NodeValue::Vec3(v) => NodeValue::Vec3(*v),
			NodeValue::Vec4(v) => NodeValue::Vec4(*v),
			NodeValue::Vec4Array(v) => NodeValue::Vec4Array(v.clone()),
			NodeValue::Matrix(v) => NodeValue::Matrix(*v),
			NodeValue::Combo(v) => NodeValue::Combo(*v),
			NodeValue::StrCombo(v) => NodeValue::StrCombo(v.clone()),
			NodeValue::VideoParams(v) => NodeValue::VideoParams(*v),
			NodeValue::AudioParams(v) => NodeValue::AudioParams(*v),
			NodeValue::Binary(v) => NodeValue::Binary(v.clone()),
			NodeValue::NodeRef(v) => NodeValue::NodeRef(*v),
			NodeValue::PushButton => NodeValue::PushButton,
		}
	}
}

impl Drop for NodeValue {
	/// `Texture` payloads own one handle reference: dropping the value
	/// releases it (the documented boundary rule — cross-module payloads
	/// stay inside the refcount discipline).
	fn drop(&mut self) {
		if let NodeValue::Texture(h) = self {
			if let Some(f) = h.release {
				unsafe { f(h.ctx) };
			}
		}
	}
}

/// `#[repr(C)]` mirror of the C `oaknode_value` POD (include/node/node.h),
/// used by the ffi layer for value-carrying exports (keyframe/dragger).
/// Only the fields meaningful for the value's `kind` are used; the layout
/// (int + 4-byte pad + two i64 + [f64; 4]) matches the C struct exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OakNodeValue {
	/// `oaknode_value_type` discriminant (0 = NONE ... 9 = STRING).
	pub kind: c_int,
	/// INT/COMBO value, BOOL 0/1, RATIONAL numerator.
	pub num: i64,
	/// RATIONAL denominator.
	pub den: i64,
	/// FLOAT f[0]; VEC2/3/4 f[0..n-1]; COLOR r,g,b,a.
	pub f: [f64; 4],
}

impl OakNodeValue {
	/// The zeroed POD (type `NONE`).
	pub fn none() -> Self {
		OakNodeValue {
			kind: 0,
			num: 0,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// Map an oaknode_value POD into a [`NodeValue`] of the input's
	/// declared type (C++ `variant_from_value`). `OAKNODE_VALUE_STRING`
	/// and unknown kinds are rejected with [`Error::Invalid`].
	pub fn to_node_value(self, declared: ValueType) -> crate::error::Result<NodeValue> {
		use crate::error::Error;
		match self.kind {
			oak::INT | oak::COMBO => Ok(NodeValue::Int(self.num)),
			oak::FLOAT => Ok(NodeValue::Float(self.f[0])),
			oak::BOOL => Ok(NodeValue::Boolean(self.num != 0)),
			oak::RATIONAL => Ok(NodeValue::Rational(Rational::new(self.num, self.den))),
			oak::COLOR => Ok(NodeValue::Color(self.f)),
			oak::VEC2 => Ok(NodeValue::Vec2([self.f[0], self.f[1]])),
			oak::VEC3 => Ok(NodeValue::Vec3([self.f[0], self.f[1], self.f[2]])),
			oak::VEC4 => Ok(NodeValue::Vec4(self.f)),
			_ => Err(Error::Invalid),
		}
	}

	/// Map a [`NodeValue`] of the input's declared type into the POD
	/// (C++ `value_from_variant`). String-carried declared types fail with
	/// [`Error::Invalid`]; types without a POD representation fail with
	/// [`Error::Failed`].
	pub fn from_node_value(
		declared: ValueType,
		v: &NodeValue,
	) -> crate::error::Result<OakNodeValue> {
		use crate::error::Error;
		if declared.is_string() {
			return Err(Error::Invalid);
		}
		let mut out = OakNodeValue::none();
		out.kind = declared.to_oak();
		match declared {
			ValueType::None => Ok(out),
			ValueType::Int | ValueType::Combo => {
				out.num = v.to_double() as i64;
				Ok(out)
			}
			ValueType::Float => {
				out.f[0] = v.to_double();
				Ok(out)
			}
			ValueType::Boolean => {
				out.num = v.to_double() as i64;
				Ok(out)
			}
			ValueType::Rational => match v {
				NodeValue::Rational(r) => {
					out.num = r.numerator();
					out.den = r.denominator();
					Ok(out)
				}
				_ => {
					out.num = v.to_double() as i64;
					out.den = 1;
					Ok(out)
				}
			},
			ValueType::Color => {
				let c = match v {
					NodeValue::Color(c) => *c,
					_ => return Err(Error::Failed("type has no POD representation".to_string())),
				};
				out.f = c;
				Ok(out)
			}
			ValueType::Vec2 => {
				let a = match v {
					NodeValue::Vec2(a) => *a,
					_ => return Err(Error::Failed("type has no POD representation".to_string())),
				};
				out.f = [a[0], a[1], 0.0, 0.0];
				Ok(out)
			}
			ValueType::Vec3 => {
				let a = match v {
					NodeValue::Vec3(a) => *a,
					_ => return Err(Error::Failed("type has no POD representation".to_string())),
				};
				out.f = [a[0], a[1], a[2], 0.0];
				Ok(out)
			}
			ValueType::Vec4 => {
				let a = match v {
					NodeValue::Vec4(a) => *a,
					_ => return Err(Error::Failed("type has no POD representation".to_string())),
				};
				out.f = a;
				Ok(out)
			}
			_ => {
				out.kind = oak::NONE;
				Err(Error::Failed("type has no POD representation".to_string()))
			}
		}
	}
}
