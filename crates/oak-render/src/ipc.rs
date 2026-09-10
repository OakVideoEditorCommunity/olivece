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

//! Render-worker IPC: the control-plane NDJSON protocol and the
//! shared-memory frame-slot transport. The transport is the Rust port of
//! `engine/render/ipc/` + `ipcmessage.cpp`.
//!
//! Ownership moved to oakrender in M15 S1: both ends of the pipe now
//! link this single copy (the main-process [`crate::procpool`]
//! dispatcher creates the segments and speaks the protocol; the
//! oak-worker binary re-exports this module from its `crate::ipc`
//! shim). Before M15 the module lived in the oak-worker binary (M14
//! R2); the facade still keeps its own copy for the frozen
//! `oakengine_ipc_*` C ABI.
//!
//! Two halves:
//!
//!   - **Control plane.** One compact JSON object per line on the stdio
//!     pipes (worker.cpp / ipcmessage.cpp `write_message`/`read_message`).
//!     Every message carries a `"type"` string; the field names below are
//!     the ones the C++ serializers actually emit
//!     (`engine/render/ipc/ipcmessage.cpp`): note `ticket` / `node` /
//!     `channels` / `slot` — the longer names (`ticket_id`, `node_uuid`,
//!     `channel_count`, `output_slot`) exist only on the C POD structs in
//!     `ipc.h`. [`write_message`]/[`error_message`] build the wire lines.
//!   - **Data plane.** Named shared memory holding the frame-slot pools —
//!     the port of `engine/render/ipc/` (`sharedmemoryregion.cpp`,
//!     `frameslotpool.cpp`): [`SharedMemoryRegion`] maps a named segment —
//!     POSIX `shm_open` + `mmap` on Unix (`munmap` + `shm_unlink` on
//!     close), `CreateFileMappingW`/`OpenFileMappingW` + `MapViewOfFile`
//!     on Windows (`UnmapViewOfFile` + `CloseHandle` on close) — and
//!     [`FrameSlotPool`] lays out a fixed pool of equal-sized frame
//!     slots inside it with lock-free hand-off through two
//!     [`SpscRingBuffer`]s of slot indices (free + ready). Each ring is a
//!     single-producer/single-consumer structure; the filler owns
//!     `free.pop` + `ready.push`, the drainer owns `ready.pop` +
//!     `free.push`, so no mutex is ever taken.
//!
//! **The in-memory layout is the version-1 wire protocol** the app and the
//! render worker share, and it never changes: the byte offsets below are
//! copied field-for-field from the C++ implementation (64-byte cache-line
//! alignment, the `Header`/`SpscRingBuffer`/`oak_frame_slot_meta` POD
//! structs). A segment written by the C++ side attaches here and vice
//! versa.
//!
//! This module is deliberately unsafe-heavy and self-contained: it touches
//! raw shared memory and raw POSIX syscalls, and everything else in the
//! crate reaches it through the safe wrapper methods.
//!
//! Message types (M = main/editor, W = worker):
//!   handshake      M<->W  negotiate protocol version + announce shm geometry
//!   load_graph     M ->W  path to a temp file holding the serialized graph
//!   render_frame   M ->W  request a frame render (ticket, node, time, params)
//!   frame_ready    W ->M  a rendered frame is published (slot + ticket)
//!   cancel         M ->W  abandon an in-flight ticket
//!   graph_update   M ->W  reserved (no payload struct yet)
//!   shutdown       M ->W  finish current work and exit cleanly
//!   error          W ->M  worker-side failure report ("message" field)
//!
//! Protocol v2 additions (M15 S1; backward compatible — v1 message names
//! and wire shapes are unchanged, v2 only adds new message types):
//!   hello_caps     W ->M  worker capabilities after a successful shm attach
//!                         (supported output formats, max slot size)
//!   render_batch   M ->W  a batch of frame tickets with main-assigned slots
//!   batch_accepted W ->M  the worker claimed the batch (explicit claim)
//!   frame_failed   W ->M  one ticket failed to render (error string)
//!
//! Slot addressing: `render_batch` tickets carry the destination `slot`
//! chosen by the main process; the worker never picks its own slots in
//! batch mode (main-side addressing lets the preview cache live directly
//! on slots). The v1 `render_frame` path keeps worker-side slot acquire.

#![allow(dead_code)]

#[cfg(windows)]
use std::ffi::c_void;
use std::ffi::{c_char, c_int};
use std::io::{self, Write};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};


/// `"handshake"`.
pub const TYPE_HANDSHAKE: &str = "handshake";
/// `"load_graph"`.
pub const TYPE_LOAD_GRAPH: &str = "load_graph";
/// `"render_frame"`.
pub const TYPE_RENDER_FRAME: &str = "render_frame";
/// `"frame_ready"`.
pub const TYPE_FRAME_READY: &str = "frame_ready";
/// `"cancel"`.
pub const TYPE_CANCEL: &str = "cancel";
/// `"graph_update"`.
pub const TYPE_GRAPH_UPDATE: &str = "graph_update";
/// `"shutdown"`.
pub const TYPE_SHUTDOWN: &str = "shutdown";
/// `"error"`.
pub const TYPE_ERROR: &str = "error";
/// `"hello_caps"` (protocol v2).
pub const TYPE_HELLO_CAPS: &str = "hello_caps";
/// `"render_batch"` (protocol v2).
pub const TYPE_RENDER_BATCH: &str = "render_batch";
/// `"batch_accepted"` (protocol v2).
pub const TYPE_BATCH_ACCEPTED: &str = "batch_accepted";
/// `"frame_failed"` (protocol v2).
pub const TYPE_FRAME_FAILED: &str = "frame_failed";
/// `"render_audio_batch"` (protocol v2, M15 S3): a batch of audio range
/// pulls rendered into the same shm slot transport as video frames.
pub const TYPE_RENDER_AUDIO_BATCH: &str = "render_audio_batch";
/// `"plugin_progress"` (protocol v2): worker->main — one OFX plugin
/// progress event (progressStart/Update/End forwarded over the control
/// plane). The main process drains it into the plugin-progress dialog.
pub const TYPE_PLUGIN_PROGRESS: &str = "plugin_progress";
/// `"plugin_cancel"` (protocol v2): main->worker — the user cancelled the
/// plugin render. The worker sets its sticky cancel flag; every live
/// progress reporter then answers false (the plugin aborts at its next
/// progressUpdate).
pub const TYPE_PLUGIN_CANCEL: &str = "plugin_cancel";

/// Wire-format slot format for 8-bit BGRA frames (M15 S1). The viewer
/// preview path requests BGRA8 so the worker converts its F32 pipeline
/// output at the end of the render (conversion, not an extra copy) and
/// the main process can feed the slot straight to the GPU queue. The
/// value lives outside the `PixelFormat` enum range on purpose: it is a
/// slot wire format, not a pipeline format.
pub const SLOT_FORMAT_BGRA8: i32 = 100;

/// Wire-format slot format for interleaved f32 audio samples (M15 S3).
/// An audio slot reuses [`FrameSlotMeta`]: `format` is this marker,
/// `channel_count` is the channel count, `linesize` is
/// `channels * 4` (bytes per sample frame), `data_size` is the total
/// sample bytes and `width` carries the output sample rate (Hz). The
/// value lives outside the `PixelFormat` enum range like
/// [`SLOT_FORMAT_BGRA8`].
pub const SLOT_FORMAT_AUDIO_F32: i32 = 101;

/// `handshake` — field-for-field equivalent of `oak_ipc_handshake`
/// (ipc.h). Wire field names match the C++ serializer.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct HandshakeMsg {
	/// Protocol version.
	pub protocol_version: i32,
	/// Worker->main output shared-memory segment key.
	pub shm_key: String,
	/// Main->worker input shared-memory segment key (optional).
	pub input_shm_key: String,
	/// Number of main->worker input frame slots.
	pub input_slots: i32,
	/// Number of worker->main output frame slots.
	pub output_slots: i32,
	/// Per-output-slot pixel block size.
	pub slot_data_bytes: i64,
	/// Per-input-slot pixel block size.
	pub input_slot_data_bytes: i64,
}

impl HandshakeMsg {
	/// The worker's startup handshake (`worker.cpp startup_handshake()`).
	pub fn to_json(&self) -> Value {
		json!({
			"type": TYPE_HANDSHAKE,
			"protocol_version": self.protocol_version,
			"shm_key": self.shm_key,
			"input_shm_key": self.input_shm_key,
			"input_slots": self.input_slots,
			"output_slots": self.output_slots,
			"slot_data_bytes": self.slot_data_bytes,
			"input_slot_data_bytes": self.input_slot_data_bytes,
		})
	}
}

/// `render_frame` — request a frame render. Wire names per ipcmessage.cpp:
/// `ticket`, `node`, `channels` (not the ipc.h POD names).
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct RenderFrameMsg {
	/// Correlates with the eventual frame_ready.
	pub ticket: i64,
	/// Viewer node stable uuid in the loaded graph.
	pub node: String,
	/// Frame timestamp numerator.
	pub time_num: i64,
	/// Frame timestamp denominator.
	pub time_den: i64,
	/// Forced output size (0 = graph default).
	pub width: i32,
	/// Forced output height (0 = graph default).
	pub height: i32,
	/// Forced PixelFormat (-1 = default).
	pub format: i32,
	/// Channel count (0 = default).
	pub channels: i32,
	/// RenderMode.
	pub mode: i32,
	/// Optional decoded input slot (-1 = none).
	pub input_slot: i32,
	/// Ordered decoded input slots.
	pub input_slots: Vec<i32>,
	/// Output color transform present?
	pub has_color_transform: bool,
	/// Color transform targets the display space.
	pub color_is_display: bool,
	/// Output color space name.
	pub color_output: String,
	/// Output color view name.
	pub color_view: String,
	/// Output color look name.
	pub color_look: String,
}

/// `frame_ready` — a rendered frame is published (wire names `ticket`/
/// `slot`).
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct FrameReadyMsg {
	/// Correlates with the render_frame request.
	pub ticket: i64,
	/// Index into the worker->main output FrameSlotPool.
	pub slot: i32,
}

/// `cancel` — abandon an in-flight ticket by id.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct CancelMsg {
	/// The in-flight ticket id to abandon.
	pub ticket: i64,
}

/// `load_graph` — path to a temporary file holding the serialized graph.
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
#[serde(default)]
pub struct LoadGraphMsg {
	/// Path to the temporary file holding the serialized graph.
	pub path: String,
}

// ---------------------------------------------------------------------------
// Protocol v2 messages (M15 S1; additive, v1 wire shapes unchanged)
// ---------------------------------------------------------------------------

/// `hello_caps` (worker->main) — sent right after a successful handshake
/// shm attach: the output formats the worker can write into slots and the
/// maximum slot size it accepts (main uses both to negotiate geometry).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct HelloCapsMsg {
	/// Protocol version the worker speaks (1; v2 is additive).
	pub protocol_version: i32,
	/// Slot output formats the worker supports (`PixelFormat` ints plus
	/// [`SLOT_FORMAT_BGRA8`]).
	pub formats: Vec<i32>,
	/// Largest slot data block the worker will render into (bytes).
	pub max_slot_bytes: i64,
}

/// A node value on the wire (protocol v2, montage effect parameters).
/// `oak_node::value::NodeValue` itself is not serde-able (texture/sample
/// payloads, handles); this enum covers the plain-data variants an effect
/// parameter can carry. Connection/handle variants (texture, samples,
/// node refs, video/audio params, push buttons) have no wire
/// representation and are dropped by [`WireNodeValue::from_node_value`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum WireNodeValue {
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
	/// Rational (num, den).
	Rational(i64, i64),
	/// Vec2.
	Vec2([f64; 2]),
	/// Vec3.
	Vec3([f64; 3]),
	/// Vec4.
	Vec4([f64; 4]),
	/// 4x4 matrix, row-major.
	Matrix([f64; 16]),
	/// Combo index.
	Combo(i64),
	/// String combo.
	StrCombo(String),
	/// Opaque bytes.
	Binary(Vec<u8>),
}

impl Default for WireNodeValue {
	fn default() -> Self {
		WireNodeValue::None
	}
}

impl WireNodeValue {
	/// The wire form of a node value, or `None` when the variant has no
	/// wire representation (the parameter is then not carried).
	pub fn from_node_value(v: &oak_node::value::NodeValue) -> Option<WireNodeValue> {
		use oak_node::value::NodeValue as NV;
		Some(match v {
			NV::None => WireNodeValue::None,
			NV::Int(i) => WireNodeValue::Int(*i),
			NV::Float(f) => WireNodeValue::Float(*f),
			NV::Color(c) => WireNodeValue::Color(*c),
			NV::Text(s) => WireNodeValue::Text(s.clone()),
			NV::Boolean(b) => WireNodeValue::Boolean(*b),
			NV::Rational(r) => WireNodeValue::Rational(r.numerator(), r.denominator()),
			NV::Vec2(v2) => WireNodeValue::Vec2(*v2),
			NV::Vec3(v3) => WireNodeValue::Vec3(*v3),
			NV::Vec4(v4) => WireNodeValue::Vec4(*v4),
			NV::Matrix(m) => WireNodeValue::Matrix(*m),
			NV::Combo(i) => WireNodeValue::Combo(*i),
			NV::StrCombo(s) => WireNodeValue::StrCombo(s.clone()),
			NV::Binary(b) => WireNodeValue::Binary(b.clone()),
			_ => return None,
		})
	}

	/// Back to a node value (worker side).
	pub fn to_node_value(&self) -> oak_node::value::NodeValue {
		use oak_node::value::NodeValue as NV;
		match self {
			WireNodeValue::None => NV::None,
			WireNodeValue::Int(i) => NV::Int(*i),
			WireNodeValue::Float(f) => NV::Float(*f),
			WireNodeValue::Color(c) => NV::Color(*c),
			WireNodeValue::Text(s) => NV::Text(s.clone()),
			WireNodeValue::Boolean(b) => NV::Boolean(*b),
			WireNodeValue::Rational(n, d) => NV::Rational(oak_core::Rational::new(*n, *d)),
			WireNodeValue::Vec2(v2) => NV::Vec2(*v2),
			WireNodeValue::Vec3(v3) => NV::Vec3(*v3),
			WireNodeValue::Vec4(v4) => NV::Vec4(*v4),
			WireNodeValue::Matrix(m) => NV::Matrix(*m),
			WireNodeValue::Combo(i) => NV::Combo(*i),
			WireNodeValue::StrCombo(s) => NV::StrCombo(s.clone()),
			WireNodeValue::Binary(b) => NV::Binary(b.clone()),
		}
	}
}

/// One effect parameter on the wire (input id + value).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct WireEffectParam {
	/// Node input id.
	pub input: String,
	/// Parameter value.
	pub value: WireNodeValue,
}

/// One effect of a montage clip's effect stack on the wire (protocol v2
/// additive field of [`WireMontageClip`]; source-first order).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct WireMontageEffect {
	/// Built-in node type id or OFX plugin identifier.
	pub type_id: String,
	/// Enabled flag (disabled effects are bypassed).
	pub enabled: bool,
	/// Effect input (clip) name; "" = none.
	pub effect_input_id: String,
	/// Parameter values.
	pub params: Vec<WireEffectParam>,
}

/// One montage clip on the wire (rationals flattened to num/den pairs).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct WireMontageClip {
	/// Footage filename.
	pub filename: String,
	/// Media stream index.
	pub stream_index: i32,
	/// Clip in point numerator (sequence time).
	pub in_num: i64,
	/// Clip in point denominator.
	pub in_den: i64,
	/// Clip out point numerator (sequence time).
	pub out_num: i64,
	/// Clip out point denominator.
	pub out_den: i64,
	/// Media in point numerator.
	pub media_in_num: i64,
	/// Media in point denominator.
	pub media_in_den: i64,
	/// Playback gain (1.0 = unity).
	pub gain: f32,
	/// The clip's effect stack (protocol v2 additive: older peers omit the
	/// field and it defaults to an empty stack).
	pub effects: Vec<WireMontageEffect>,
}

/// Map a ticket-side montage effect to its wire form (main process;
/// parameters without a wire representation are dropped).
pub fn wire_effect_from(effect: &crate::ticket::MontageEffect) -> WireMontageEffect {
	WireMontageEffect {
		type_id: effect.type_id.clone(),
		enabled: effect.enabled,
		effect_input_id: effect.effect_input_id.clone().unwrap_or_default(),
		params: effect
			.params
			.iter()
			.filter_map(|(input, value)| {
				WireNodeValue::from_node_value(value).map(|value| WireEffectParam {
					input: input.clone(),
					value,
				})
			})
			.collect(),
	}
}

/// Map a wire effect back to the ticket-side form (worker).
pub fn montage_effect_from(wire: &WireMontageEffect) -> crate::ticket::MontageEffect {
	crate::ticket::MontageEffect {
		type_id: wire.type_id.clone(),
		enabled: wire.enabled,
		effect_input_id: if wire.effect_input_id.is_empty() {
			None
		} else {
			Some(wire.effect_input_id.clone())
		},
		params: wire
			.params
			.iter()
			.map(|p| (p.input.clone(), p.value.to_node_value()))
			.collect(),
	}
}

/// One adjustment layer of a montage on the wire (protocol v2 additive
/// field of [`BatchTicketSpec`]; rationals flattened to num/den pairs).
/// `track_index` is the montage-list boundary of
/// [`crate::ticket::AdjustmentSpan`], not a track number.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct WireAdjustmentSpan {
	/// Adjustment in point numerator (sequence time).
	pub in_num: i64,
	/// Adjustment in point denominator.
	pub in_den: i64,
	/// Adjustment out point numerator (sequence time).
	pub out_num: i64,
	/// Adjustment out point denominator.
	pub out_den: i64,
	/// Montage-list boundary (clips composited below this layer).
	pub track_index: usize,
	/// The adjustment's effect stack (source-first).
	pub effects: Vec<WireMontageEffect>,
}

/// Map a ticket-side adjustment span to its wire form (main process).
pub fn wire_adjustment_from(span: &crate::ticket::AdjustmentSpan) -> WireAdjustmentSpan {
	WireAdjustmentSpan {
		in_num: span.in_time.numerator(),
		in_den: span.in_time.denominator(),
		out_num: span.out_time.numerator(),
		out_den: span.out_time.denominator(),
		track_index: span.track_index,
		effects: span.effects.iter().map(wire_effect_from).collect(),
	}
}

/// Map a wire adjustment span back to the ticket-side form (worker).
pub fn adjustment_from_wire(wire: &WireAdjustmentSpan) -> crate::ticket::AdjustmentSpan {
	crate::ticket::AdjustmentSpan {
		in_time: oak_core::Rational::new(wire.in_num, wire.in_den),
		out_time: oak_core::Rational::new(wire.out_num, wire.out_den),
		track_index: wire.track_index,
		effects: wire.effects.iter().map(montage_effect_from).collect(),
	}
}

/// One frame ticket inside a [`RenderBatchMsg`] — the main process
/// assigns the destination `slot`.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BatchTicketSpec {
	/// Ticket id (correlates with frame_ready / frame_failed).
	pub ticket: i64,
	/// Destination slot index in the worker->main output pool.
	pub slot: i32,
	/// Frame timestamp numerator.
	pub time_num: i64,
	/// Frame timestamp denominator.
	pub time_den: i64,
	/// Output width.
	pub width: i32,
	/// Output height.
	pub height: i32,
	/// Slot output format (`PixelFormat` int or [`SLOT_FORMAT_BGRA8`]).
	pub format: i32,
	/// Channel count (4 on the video pipeline).
	pub channels: i32,
	/// Single-footage decode filename ("" = none).
	pub footage_file: String,
	/// Single-footage stream index.
	pub footage_stream: i32,
	/// Sequence montage (ordered topmost-last; empty = none).
	pub montage: Vec<WireMontageClip>,
	/// Adjustment layers over the montage (protocol v2 additive: older peers
	/// omit the field and it defaults to no adjustment layers).
	pub adjustments: Vec<WireAdjustmentSpan>,
	/// Sequence viewer node identity (0 = montage mode; nonzero = render
	/// the viewer's graph frame from the worker's loaded snapshot).
	pub viewer_node: u64,
	/// The owning project's uuid (M16 S1): the worker renders the viewer's
	/// graph frame only when this matches the loaded snapshot's project
	/// ("" = no graph mode).
	pub project_key: String,
}

/// `render_batch` (main->worker) — a batch of frame tickets with
/// main-assigned slots, rendered in order.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct RenderBatchMsg {
	/// Batch id (correlates with batch_accepted).
	pub batch_id: i64,
	/// The tickets, rendered in order.
	pub tickets: Vec<BatchTicketSpec>,
}

/// `batch_accepted` (worker->main) — explicit claim confirmation for a
/// [`RenderBatchMsg`] (the batch's frames are owned by this worker; no
/// work stealing).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BatchAcceptedMsg {
	/// The accepted batch id.
	pub batch_id: i64,
	/// The ticket ids accepted (same order as the batch).
	pub tickets: Vec<i64>,
}

/// One audio range pull inside a [`RenderAudioBatchMsg`] (M15 S3) — the
/// audio counterpart of [`BatchTicketSpec`]: the main process assigns the
/// destination `slot`, the worker mixes the montage over `[time,
/// time + duration)` at `sample_rate`/`channel_layout` into interleaved
/// f32 and writes it into the slot (wire format
/// [`SLOT_FORMAT_AUDIO_F32`]).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct AudioTicketSpec {
	/// Ticket id (correlates with frame_ready / frame_failed).
	pub ticket: i64,
	/// Destination slot index in the worker->main output pool.
	pub slot: i32,
	/// Range start numerator.
	pub time_num: i64,
	/// Range start denominator.
	pub time_den: i64,
	/// Range length numerator.
	pub duration_num: i64,
	/// Range length denominator.
	pub duration_den: i64,
	/// Output sample rate (Hz).
	pub sample_rate: i32,
	/// Output channel layout mask.
	pub channel_layout: u64,
	/// Channel count (derived from the layout; written into the slot meta).
	pub channels: i32,
	/// Sequence montage (ordered topmost-last; empty = silence).
	pub montage: Vec<WireMontageClip>,
}

/// `render_audio_batch` (main->worker, M15 S3) — a batch of audio range
/// pulls rendered in order, sharing the claim/credit/frame_ready flow of
/// [`RenderBatchMsg`].
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct RenderAudioBatchMsg {
	/// Batch id (correlates with batch_accepted).
	pub batch_id: i64,
	/// The audio tickets, rendered in order.
	pub tickets: Vec<AudioTicketSpec>,
}

/// `frame_failed` (worker->main) — one ticket failed to render; the main
/// process falls back (purple frame) and owns the slot again.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct FrameFailedMsg {
	/// The failed ticket id.
	pub ticket: i64,
	/// Human-readable failure reason.
	pub error: String,
}

/// `plugin_progress` (worker->main) — one OFX plugin progress event
/// forwarded over the control plane.
///
/// The oakplugin progress suite runs in the worker process (plugin
/// rendering is process-isolated); the worker installs a progress reporter
/// factory whose reporters push these messages to stdout. Wire shape is
/// intentionally the same as the main-process `PluginProgressEvent`:
/// progressStart arrives with fraction 0 and label/message set,
/// progressUpdate with the fraction, progressEnd with fraction 1.0 (the
/// app closes the dialog on >= 1.0, mirroring the main-process reporter —
/// the `UiProgressReporter` trait has no end hook, so completion is
/// inferred from the fraction there too).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct PluginProgressMsg {
	/// The plugin's progressStart label.
	pub label: String,
	/// The plugin's progressStart message.
	pub message: String,
	/// Progress fraction in 0.0..=1.0.
	pub fraction: f64,
}

impl PluginProgressMsg {
	/// Build the wire `plugin_progress` value.
	pub fn to_json(&self) -> Value {
		json!({
			"type": TYPE_PLUGIN_PROGRESS,
			"label": self.label,
			"message": self.message,
			"fraction": self.fraction,
		})
	}
}

/// The wire `plugin_cancel` message (main->worker; no payload).
pub fn plugin_cancel_json() -> Value {
	json!({ "type": TYPE_PLUGIN_CANCEL })
}

/// Build a worker-side error report, mirroring `error_message()` in
/// worker.cpp: `{"type":"error","message":...}` plus `"ticket"` when
/// non-zero.
pub fn error_message(message: &str, ticket: Option<i64>) -> Value {
	match ticket.filter(|t| *t != 0) {
		Some(t) => json!({ "type": TYPE_ERROR, "message": message, "ticket": t }),
		None => json!({ "type": TYPE_ERROR, "message": message }),
	}
}

/// Write one NDJSON message line (compact JSON + `\n`), the Rust port of
/// `ipcmessage.cpp write_message()`.
pub fn write_message(w: &mut impl Write, msg: &Value) -> io::Result<()> {
	let line =
		serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
	w.write_all(line.as_bytes())?;
	w.write_all(b"\n")
}

// ---------------------------------------------------------------------------
// Shared-memory frame-slot transport
// ---------------------------------------------------------------------------

/// `OAK_IPC_SHM_KEY_CAP` — capacity of shm key strings (ipc.h), incl. NUL.
pub const OAK_IPC_SHM_KEY_CAP: usize = 128;
/// `OAK_IPC_COLORSPACE_CAP` — capacity of `oak_frame_slot_meta::colorspace`.
pub const OAK_IPC_COLORSPACE_CAP: usize = 128;

/// Byte alignment of every sub-region of a frame slot pool (the C++
/// `k_align = 64`; cache-line alignment).
const K_ALIGN: usize = 64;

/// `k_magic = 0x4F4B5350` ("OKSP") — the frame slot pool header magic.
pub const FRAMEPOOL_MAGIC: u32 = 0x4F4B5350;

/// Round `value` up to the next multiple of `align` (power of two).
const fn align_up(value: usize, align: usize) -> usize {
	(value + (align - 1)) & !(align - 1)
}

/// `OAK_IPC_SHM_MODE_CREATE` / `OAK_IPC_SHM_MODE_ATTACH` (ipc.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShmMode {
	/// Create (and own) the segment. Fails if it already exists; the owner
	/// unlinks it on close.
	Create,
	/// Attach to a segment created by the peer. Does not unlink on close.
	Attach,
}

impl ShmMode {
	/// Map the C ABI mode integer (`OAK_IPC_SHM_MODE_CREATE` = 0,
	/// `OAK_IPC_SHM_MODE_ATTACH` = 1) back to the enum.
	fn from_c(v: c_int) -> ShmMode {
		match v {
			0 => ShmMode::Create,
			_ => ShmMode::Attach,
		}
	}
}

/// Per-slot metadata describing the frame currently occupying a slot —
/// field-for-field `oak_frame_slot_meta` from `engine/include/oakengine/ipc.h`.
///
/// This POD lives in shared memory alongside the pixel data and is part of
/// the version-1 wire protocol; `#[repr(C)]` keeps the C ABI layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSlotMeta {
	/// Caller-defined tag (ticket id, or footage stream hash).
	pub id: i64,
	/// Frame timestamp numerator.
	pub time_num: i64,
	/// Frame timestamp denominator.
	pub time_den: i64,
	/// Frame width.
	pub width: i32,
	/// Frame height.
	pub height: i32,
	/// `PixelFormat::Format` value.
	pub format: i32,
	/// Channel count.
	pub channel_count: i32,
	/// Bytes per scanline (stride).
	pub linesize: i32,
	/// Valid bytes written into the slot's data block.
	pub data_size: i32,
	/// Input colorspace name.
	pub colorspace: [c_char; OAK_IPC_COLORSPACE_CAP],
}

impl Default for FrameSlotMeta {
	fn default() -> Self {
		FrameSlotMeta {
			id: 0,
			time_num: 0,
			time_den: 0,
			width: 0,
			height: 0,
			format: 0,
			channel_count: 0,
			linesize: 0,
			data_size: 0,
			colorspace: [0; OAK_IPC_COLORSPACE_CAP],
		}
	}
}

/// `sizeof(oak_frame_slot_meta)` (8+8+8 + 4*6 + 128).
const FRAME_SLOT_META_SIZE: usize = 176;

// ---------------------------------------------------------------------------
// SpscRingBuffer
// ---------------------------------------------------------------------------

/// A lock-free single-producer / single-consumer ring buffer of `u32`
/// indices, living in shared memory — the port of
/// `engine/include/oakengine/spscringbuffer.h`.
///
/// Layout (offsets from the buffer base, matching the C++ class):
///
/// ```text
/// 0   head_     u32   producer cursor (relaxed read, release write)
/// 4   tail_     u32   consumer cursor (relaxed read, release write)
/// 8   capacity_ u32   slot count (written once by create())
/// 12  slots     u32[capacity]
/// ```
///
/// One slot is always left empty to disambiguate full and empty, so a
/// buffer with `capacity` slots holds at most `capacity - 1` live entries.
/// The payload is a `u32` slot index — never a pointer.
///
/// `SpscRingBuffer` is a thin view over a raw pointer; it is `Copy` and
/// owns nothing. All methods are `unsafe` because they read and write the
/// shared segment concurrently with a peer process.
#[derive(Clone, Copy)]
pub struct SpscRingBuffer {
	/// Base of the ring header (`head_` at offset 0).
	base: *mut u8,
}

// The shared memory the ring lives in is usable from any thread of the
// local process; synchronization with the peer is the ring's own atomics.
unsafe impl Send for SpscRingBuffer {}
unsafe impl Sync for SpscRingBuffer {}

impl SpscRingBuffer {
	/// `sizeof(SpscRingBuffer)` — header bytes before the slot array.
	pub const HEADER_BYTES: usize = 12;

	/// Total bytes required for the header plus `capacity` index slots
	/// (`SpscRingBuffer::bytes_needed`).
	pub fn bytes_needed(capacity: u32) -> usize {
		Self::HEADER_BYTES + capacity as usize * 4
	}

	/// In-place construct a ring header at `mem` with `capacity` index
	/// slots. `mem` must provide at least [`Self::bytes_needed`] bytes and
	/// be suitably aligned (mmap-backed segments are). Done exactly once by
	/// whichever process owns the segment's creation; the peer uses
	/// [`Self::attach`] instead.
	///
	/// # Safety
	/// `mem` must be a valid, writable, aligned buffer of at least
	/// [`Self::bytes_needed`] bytes, and must not be concurrently written
	/// during this call.
	pub unsafe fn create(mem: *mut u8, capacity: u32) -> SpscRingBuffer {
		let ring = SpscRingBuffer { base: mem };
		unsafe {
			ring.store_capacity(capacity);
			ring.head().store(0, Ordering::Relaxed);
			ring.tail().store(0, Ordering::Relaxed);
			for i in 0..capacity as usize {
				*ring.slot_ptr(i) = 0;
			}
		}
		ring
	}

	/// Re-interpret already-initialized shared memory as a ring buffer
	/// (peer-process side). No writes are performed.
	///
	/// # Safety
	/// `mem` must point to a buffer previously initialized by
	/// [`Self::create`] (or an ABI-identical C++ side) that stays mapped
	/// for as long as this view is used.
	pub unsafe fn attach(mem: *mut u8) -> SpscRingBuffer {
		SpscRingBuffer { base: mem }
	}

	/// The ring's capacity (slot count).
	///
	/// # Safety
	/// `self` must point at a live ring (created or attached).
	pub unsafe fn capacity(&self) -> u32 {
		unsafe { (self.base.add(8) as *const u32).read() }
	}

	/// Producer side: enqueue an index. Returns false if the buffer is full.
	///
	/// # Safety
	/// Exactly one producer may call this concurrently with exactly one
	/// consumer calling [`Self::pop`]; the ring must be live.
	pub unsafe fn push(&self, value: u32) -> bool {
		unsafe {
			let head = self.head().load(Ordering::Relaxed);
			let next = self.increment(head);
			if next == self.tail().load(Ordering::Acquire) {
				return false;
			}
			*self.slot_ptr(head as usize) = value;
			self.head().store(next, Ordering::Release);
		}
		true
	}

	/// Consumer side: dequeue an index into `out`. Returns false if the
	/// buffer is empty.
	///
	/// # Safety
	/// Exactly one consumer may call this concurrently with exactly one
	/// producer calling [`Self::push`]; the ring must be live.
	pub unsafe fn pop(&self, out: &mut u32) -> bool {
		unsafe {
			let tail = self.tail().load(Ordering::Relaxed);
			if tail == self.head().load(Ordering::Acquire) {
				return false;
			}
			*out = *self.slot_ptr(tail as usize);
			self.tail().store(self.increment(tail), Ordering::Release);
		}
		true
	}

	/// Approximate number of entries currently queued; may be stale the
	/// instant it returns. For metrics/backpressure, not correctness.
	///
	/// # Safety
	/// The ring must be live.
	pub unsafe fn size_approx(&self) -> u32 {
		unsafe {
			let head = self.head().load(Ordering::Acquire);
			let tail = self.tail().load(Ordering::Acquire);
			let cap = self.capacity();
			(head + cap - tail) % cap
		}
	}

	/// Approximate empty check (see [`Self::size_approx`]).
	///
	/// # Safety
	/// The ring must be live.
	pub unsafe fn is_empty_approx(&self) -> bool {
		unsafe { self.head().load(Ordering::Acquire) == self.tail().load(Ordering::Acquire) }
	}

	#[inline]
	fn increment(&self, index: u32) -> u32 {
		// `capacity_` is small; this avoids requiring a power-of-two capacity.
		unsafe { (index + 1) % self.capacity() }
	}

	#[inline]
	unsafe fn head(&self) -> &AtomicU32 {
		unsafe { &*(self.base as *const AtomicU32) }
	}

	#[inline]
	unsafe fn tail(&self) -> &AtomicU32 {
		unsafe { &*(self.base.add(4) as *const AtomicU32) }
	}

	#[inline]
	unsafe fn store_capacity(&self, capacity: u32) {
		unsafe { *(self.base.add(8) as *mut u32) = capacity };
	}

	#[inline]
	unsafe fn slot_ptr(&self, index: usize) -> *mut u32 {
		unsafe { self.base.add(Self::HEADER_BYTES + index * 4) as *mut u32 }
	}
}

// ---------------------------------------------------------------------------
// FrameSlotPool
// ---------------------------------------------------------------------------

/// Pool header written by create() and read back by attach(). Field-for-
/// field the C++ `FrameSlotPool::Header` (offsets: 0,4,8,16,24,32,40;
/// 48 bytes total).
#[repr(C)]
struct PoolHeader {
	magic: u32,
	slot_count: u32,
	slot_data_bytes: u64,
	free_ring_offset: u64,
	ready_ring_offset: u64,
	meta_offset: u64,
	data_offset: u64,
}

const POOL_HEADER_SIZE: usize = 48;

/// A fixed-size pool of equal-sized frame slots in shared memory with
/// lock-free hand-off — the port of the C++ `FrameSlotPool`
/// (`engine/src/oliveimpl/render/ipc/frameslotpool.{h,cpp}`).
///
/// One pool models a single direction of frame flow. It does NOT own the
/// memory; it is a view over a mapped [`SharedMemoryRegion`] (or any
/// ABI-identical segment). Lifecycle: the filler `acquire`s a free slot,
/// writes meta + pixels, then `publish`es it; the drainer `consume`s the
/// next ready slot, reads it, and `release`s it back to the free ring.
///
/// [`FrameSlotPool`] is `Clone` — the clone is another view of the same
/// segment (the C++ `copy()`), useful to hand both sides a handle without
/// owning the mapping twice.
pub struct FrameSlotPool {
	/// Segment base.
	base: *mut u8,
	/// The pool header at `base + 0`.
	header: *mut PoolHeader,
	/// Free-ring view (filler pops, drainer pushes).
	free_ring: SpscRingBuffer,
	/// Ready-ring view (filler pushes, drainer pops).
	ready_ring: SpscRingBuffer,
	/// Metadata array at `base + meta_offset`.
	meta: *mut FrameSlotMeta,
	/// Pixel data blocks at `base + data_offset`.
	data: *mut u8,
}

// Views into shared memory are safe to share within the process; the rings
// carry their own synchronization.
unsafe impl Send for FrameSlotPool {}
unsafe impl Sync for FrameSlotPool {}

impl Clone for FrameSlotPool {
	fn clone(&self) -> FrameSlotPool {
		FrameSlotPool {
			base: self.base,
			header: self.header,
			free_ring: self.free_ring,
			ready_ring: self.ready_ring,
			meta: self.meta,
			data: self.data,
		}
	}
}

impl FrameSlotPool {
	/// Total bytes a region must provide to back a pool of
	/// `slot_count` x `slot_data_bytes`
	/// (`FrameSlotPool::bytes_needed`).
	pub fn bytes_needed(slot_count: u32, slot_data_bytes: usize) -> usize {
		let ring_cap = slot_count + 1;
		let mut total = align_up(POOL_HEADER_SIZE, K_ALIGN);
		let ring_bytes = align_up(SpscRingBuffer::bytes_needed(ring_cap), K_ALIGN);
		total += ring_bytes; // free ring
		total += ring_bytes; // ready ring
		total += align_up(FRAME_SLOT_META_SIZE * slot_count as usize, K_ALIGN); // metadata
		total += align_up(slot_data_bytes, K_ALIGN) * slot_count as usize; // pixel data
		total
	}

	/// Lay out and initialize a brand-new pool over `mem` (owner side, once).
	///
	/// Writes the header, initializes both rings, seeds the free ring with
	/// every slot index and zeroes the metadata. `mem` must provide at
	/// least [`Self::bytes_needed`] bytes of writable, aligned memory (an
	/// mmap-backed segment) and must outlive the returned pool.
	///
	/// # Safety
	/// `mem` must be a valid, writable, aligned buffer of at least
	/// [`Self::bytes_needed`] bytes, not concurrently written during this
	/// call.
	pub unsafe fn create(mem: *mut u8, slot_count: u32, slot_data_bytes: usize) -> FrameSlotPool {
		let ring_cap = slot_count + 1;
		let free_off = align_up(POOL_HEADER_SIZE, K_ALIGN);
		let ready_off = free_off + align_up(SpscRingBuffer::bytes_needed(ring_cap), K_ALIGN);
		let meta_off = ready_off + align_up(SpscRingBuffer::bytes_needed(ring_cap), K_ALIGN);
		let data_off = meta_off + align_up(FRAME_SLOT_META_SIZE * slot_count as usize, K_ALIGN);

		let pool = unsafe {
			FrameSlotPool {
				base: mem,
				header: mem as *mut PoolHeader,
				free_ring: SpscRingBuffer::create(mem.add(free_off), ring_cap),
				ready_ring: SpscRingBuffer::create(mem.add(ready_off), ring_cap),
				meta: mem.add(meta_off) as *mut FrameSlotMeta,
				data: mem.add(data_off),
			}
		};
		unsafe {
			(*pool.header).magic = FRAMEPOOL_MAGIC;
			(*pool.header).slot_count = slot_count;
			(*pool.header).slot_data_bytes = slot_data_bytes as u64;
			(*pool.header).free_ring_offset = free_off as u64;
			(*pool.header).ready_ring_offset = ready_off as u64;
			(*pool.header).meta_offset = meta_off as u64;
			(*pool.header).data_offset = data_off as u64;
		}
		// `ptr::write_bytes` counts in elements of T, so cast to bytes.
		unsafe {
			ptr::write_bytes(
				pool.meta as *mut u8,
				0,
				slot_count as usize * std::mem::size_of::<FrameSlotMeta>(),
			);
		}
		// Seed the free ring with every slot index so the filler can
		// acquire() immediately.
		for i in 0..slot_count {
			unsafe { pool.free_ring.push(i) };
		}
		pool
	}

	/// Map an existing, already-initialized pool (peer side).
	///
	/// Reads the geometry from the in-memory header written by
	/// [`Self::create`]; the returned pool reports `is_valid() == false`
	/// when the magic does not match.
	///
	/// # Safety
	/// `mem` must point to a mapped segment that either contains a pool
	/// initialized by [`Self::create`] (or an ABI-identical C++ side) or is
	/// an arbitrary buffer whose first 4 bytes we must be able to read.
	pub unsafe fn attach(mem: *mut u8) -> FrameSlotPool {
		if mem.is_null() {
			return FrameSlotPool::invalid();
		}
		let header = mem as *mut PoolHeader;
		// SAFETY: `mem` is a live mapping of at least the header size.
		if unsafe { (*header).magic } != FRAMEPOOL_MAGIC {
			return FrameSlotPool::invalid();
		}
		let pool = unsafe {
			FrameSlotPool {
				base: mem,
				header,
				free_ring: SpscRingBuffer::attach(mem.add((*header).free_ring_offset as usize)),
				ready_ring: SpscRingBuffer::attach(mem.add((*header).ready_ring_offset as usize)),
				meta: mem.add((*header).meta_offset as usize) as *mut FrameSlotMeta,
				data: mem.add((*header).data_offset as usize),
			}
		};
		pool
	}

	/// An invalid pool (attach on a non-pool segment).
	fn invalid() -> FrameSlotPool {
		FrameSlotPool {
			base: ptr::null_mut(),
			header: ptr::null_mut(),
			free_ring: SpscRingBuffer {
				base: ptr::null_mut(),
			},
			ready_ring: SpscRingBuffer {
				base: ptr::null_mut(),
			},
			meta: ptr::null_mut(),
			data: ptr::null_mut(),
		}
	}

	/// True when the pool was attached to a segment containing a valid pool
	/// header.
	pub fn is_valid(&self) -> bool {
		!self.header.is_null()
	}

	/// Number of slots in the pool (0 for an invalid pool).
	pub fn slot_count(&self) -> u32 {
		if self.is_valid() {
			unsafe { (*self.header).slot_count }
		} else {
			0
		}
	}

	/// Bytes available in every slot's pixel-data block (0 for invalid).
	pub fn slot_data_bytes(&self) -> usize {
		if self.is_valid() {
			unsafe { (*self.header).slot_data_bytes as usize }
		} else {
			0
		}
	}

	/// Byte stride between consecutive slot data blocks.
	fn slot_stride(&self) -> usize {
		align_up(self.slot_data_bytes(), K_ALIGN)
	}

	// ---- Filler side ----

	/// Take ownership of a free slot. Returns false (leaving `index`
	/// untouched) if none is free.
	///
	/// # Safety
	/// The pool must be a valid view of a live segment.
	pub unsafe fn acquire(&self, index: &mut u32) -> bool {
		unsafe { self.free_ring.pop(index) }
	}

	/// Pointer to a slot's pixel data block (`slot_data_bytes` available).
	///
	/// # Safety
	/// `index` must be in `0..slot_count`; the pool must be a valid view of
	/// a live segment.
	pub unsafe fn slot_data(&self, index: u32) -> *mut u8 {
		unsafe { self.data.add(index as usize * self.slot_stride()) }
	}

	/// Mutable metadata for a slot. The filler writes this before
	/// [`Self::publish`]. The returned pointer addresses shared memory; it
	/// is borrowed, not owned.
	///
	/// # Safety
	/// `index` must be in `0..slot_count`; the pool must be a valid view of
	/// a live segment.
	pub unsafe fn meta(&self, index: u32) -> *mut FrameSlotMeta {
		unsafe { self.meta.add(index as usize) }
	}

	/// Publish a filled slot to the drainer. Must follow a successful
	/// [`Self::acquire`] of `index`. Returns false if the ready ring is
	/// full (the filler must then release the slot and retry later).
	///
	/// # Safety
	/// `index` must be a slot previously acquired and not yet released.
	pub unsafe fn publish(&self, index: u32) -> bool {
		unsafe { self.ready_ring.push(index) }
	}

	// ---- Drainer side ----

	/// Take the next published slot. Returns false if nothing is ready.
	///
	/// # Safety
	/// The pool must be a valid view of a live segment.
	pub unsafe fn consume(&self, index: &mut u32) -> bool {
		unsafe { self.ready_ring.pop(index) }
	}

	/// Return a consumed slot to the free pool for reuse. Must follow a
	/// successful [`Self::consume`] of `index`. Returns false if the free
	/// ring is full (the drainer must not release the slot yet).
	///
	/// # Safety
	/// `index` must be a slot previously consumed and not yet re-acquired.
	pub unsafe fn release(&self, index: u32) -> bool {
		unsafe { self.free_ring.push(index) }
	}

	/// Immutable metadata for a slot (drainer side).
	///
	/// # Safety
	/// `index` must be in `0..slot_count`; the pool must be a valid view of
	/// a live segment.
	pub unsafe fn meta_const(&self, index: u32) -> *const FrameSlotMeta {
		unsafe { self.meta.add(index as usize) }
	}

	/// Immutable pixel data for a slot.
	///
	/// # Safety
	/// `index` must be in `0..slot_count`; the pool must be a valid view of
	/// a live segment.
	pub unsafe fn slot_data_const(&self, index: u32) -> *const u8 {
		unsafe { self.data.add(index as usize * self.slot_stride()) }
	}
}

// ---------------------------------------------------------------------------
// SharedMemoryRegion
// ---------------------------------------------------------------------------

/// A named, fixed-size shared-memory segment mapped into the process
/// address space — the port of the C++ `SharedMemoryRegion`
/// (`engine/render/ipc/sharedmemoryregion.cpp`).
///
/// One process opens the segment in [`ShmMode::Create`] (owner: fails if
/// the name already exists, zeroes the mapping, unlinks on close); the
/// peer opens the same key in [`ShmMode::Attach`]. The mapping is a raw
/// contiguous byte range; the ring buffers and frame slot pools are laid
/// out inside it. Nothing here is locked — synchronization is entirely the
/// caller's responsibility via the lock-free structures placed in the
/// mapping.
///
/// Two backends: POSIX `shm_open`/`mmap`/`munmap`/`shm_unlink` on Unix,
/// and the Win32 file-mapping API on Windows
/// (`CreateFileMappingW`/`OpenFileMappingW` + `MapViewOfFile`).
///
/// **M15 S1 shm spike (macOS):** POSIX `shm_open` + `ftruncate` was
/// verified to back single segments of at least 512 MiB (spiked to 1 GiB)
/// on macOS — the SysV `kern.sysv.shmmax` sysctl (4 MiB default) does NOT
/// constrain POSIX shm, and no `kern.posix.shm.*` size cap applied. The
/// designed 8-slot x 8.3 MiB (BGRA8 1080p) per-worker segments therefore
/// need no temp-file+`mmap(MAP_SHARED)` fallback backend on macOS; the
/// `region_shm_spike_512mb_segment` test below is the regression gate
/// (Linux POSIX shm is unconstrained the same way).
pub struct SharedMemoryRegion {
	/// The key the region was opened with (no leading slash).
	key: String,
	/// Requested mapping size in bytes.
	size: usize,
	/// The mapped data pointer; null when invalid.
	data: *mut u8,
	/// File descriptor from `shm_open` (-1 when invalid). Unix only.
	#[cfg(unix)]
	fd: i32,
	/// File-mapping handle from `CreateFileMappingW`/`OpenFileMappingW`
	/// (null when invalid). Windows only.
	#[cfg(windows)]
	mapping: *mut c_void,
	/// Open mode.
	mode: ShmMode,
	/// Human-readable reason of the last failed open.
	error: String,
	/// The platform-prefixed name actually passed to `shm_open`. Unix only.
	#[cfg(unix)]
	shm_name: String,
}

/// Owned segment keys that are still mapped when the process exits. Test
/// binaries finish via `std::process::exit` (libtest), which skips Rust
/// static destructors — the process-wide render-manager singleton never
/// runs `Drop`, its `shm_unlink` never fires, and every test run leaks one
/// ~66 MiB segment per worker until `/dev/shm` fills up.
/// `libc::atexit` handlers DO run under `process::exit`, so each `Create`
/// registers its key here and [`SharedMemoryRegion::atexit_cleanup_owned_shm`]
/// unlinks them all at exit. Unlinking while a peer still maps the segment
/// is safe — POSIX only removes the name; the mapping lives until the last
/// `munmap` (the workers attach without owning, so they never register).
/// A SIGKILL'd process skips even atexit; those orphans are swept by
/// [`SharedMemoryRegion::cleanup_stale_owned_segments`] at the next create,
/// and the eager `posix_fallocate` reservation turns quota exhaustion into
/// a graceful open failure instead of a SIGBUS at first touch.
#[cfg(unix)]
static OWNED_SHM_KEYS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
#[cfg(unix)]
static ATEXIT_REGISTERED: std::sync::Once = std::sync::Once::new();

impl SharedMemoryRegion {
	/// An empty (invalid) region.
	pub fn new() -> SharedMemoryRegion {
		SharedMemoryRegion {
			key: String::new(),
			size: 0,
			data: ptr::null_mut(),
			#[cfg(unix)]
			fd: -1,
			#[cfg(windows)]
			mapping: ptr::null_mut(),
			mode: ShmMode::Attach,
			error: String::new(),
			#[cfg(unix)]
			shm_name: String::new(),
		}
	}

	/// Build a unique segment key for a worker, e.g.
	/// "olive-rw-<pid>-<index>" (`SharedMemoryRegion::make_key`).
	/// Centralized so the owner and the spawned worker agree on the same
	/// name.
	pub fn make_key(owner_pid: i64, worker_index: i32) -> String {
		format!("olive-rw-{owner_pid}-{worker_index}")
	}

	// -------------------------------------------------------------------
	// Unix backend (POSIX shm_open/mmap/munmap/shm_unlink)
	// -------------------------------------------------------------------

	/// Best-effort unlink of the segment named by `key` (the same naming
	/// as [`Self::open`]). Used by the creator side to clear a stale
	/// segment left behind by a crashed previous owner before re-creating
	/// it (M15 crash restart). The mapping of a peer still holding the
	/// segment stays valid — POSIX unlinks only remove the name.
	#[cfg(unix)]
	pub fn unlink_key(key: &str) {
		let shm_name = format!("/{}", key.replace('/', "_"));
		if let Ok(name_c) = std::ffi::CString::new(shm_name) {
			// SAFETY: a NUL-terminated name; unlink is safe whether or not
			// the segment exists (ENOENT is ignored by the caller).
			unsafe { libc::shm_unlink(name_c.as_ptr()) };
		}
	}

	/// Sweep owned segments (`olive-rw-<pid>-…`) whose owner pid no longer
	/// exists and unlink them. A SIGKILL'd / aborted owner never runs its
	/// atexit unlink, and because the key embeds the dead pid nobody else
	/// ever reuses the name — on a quota'd `/dev/shm` the accumulation
	/// eventually turns the next create into ENOSPC/EDQUOT. Runs once per
	/// process, before the first create. Unlinking only removes the name:
	/// a peer still mapping the segment keeps its memory until munmap.
	#[cfg(target_os = "linux")]
	pub fn cleanup_stale_owned_segments() {
		static ONCE: std::sync::Once = std::sync::Once::new();
		ONCE.call_once(|| {
			let Ok(entries) = std::fs::read_dir("/dev/shm") else {
				return;
			};
			for entry in entries.flatten() {
				let Some(name) = entry.file_name().to_str().map(str::to_string) else {
					continue;
				};
				let Some(rest) = name.strip_prefix("olive-rw-") else {
					continue;
				};
				let pid_digits: String =
					rest.chars().take_while(|c| c.is_ascii_digit()).collect();
				if pid_digits.is_empty() {
					continue;
				}
				// A live owner (or a pid reuse) means the segment is owned;
				// only dead owners are swept.
				if std::path::Path::new(&format!("/proc/{pid_digits}")).exists() {
					continue;
				}
				Self::unlink_key(&name);
			}
		});
	}

	/// No-op outside Linux: POSIX shm segments are not visible as files on
	/// every unix (macOS keeps them in a kernel namespace), so there is no
	/// directory to sweep.
	#[cfg(all(unix, not(target_os = "linux")))]
	pub fn cleanup_stale_owned_segments() {}

	/// Remember `key` so it is unlinked at process exit (see
	/// [`OWNED_SHM_KEYS`]). Safe to call from any thread; duplicate keys
	/// are harmless (the unlink is idempotent).
	#[cfg(unix)]
	fn track_owned_key(key: &str) {
		ATEXIT_REGISTERED.call_once(|| {
			// SAFETY: `atexit_cleanup_owned_shm` is a plain extern "C" fn
			// that is valid for the whole process lifetime, which is what
			// libc::atexit requires.
			unsafe {
				libc::atexit(Self::atexit_cleanup_owned_shm);
			}
		});
		if let Ok(mut keys) = OWNED_SHM_KEYS.lock() {
			keys.get_or_insert_with(Vec::new).push(key.to_string());
		}
	}

	/// Unlink every segment this process created, at process exit — runs
	/// even when the exit path is `std::process::exit` (test binaries).
	#[cfg(unix)]
	extern "C" fn atexit_cleanup_owned_shm() {
		let keys = OWNED_SHM_KEYS.lock().ok().and_then(|mut keys| keys.take());
		if let Some(keys) = keys {
			for key in keys {
				Self::unlink_key(&key);
			}
		}
	}

	/// Open the segment identified by `key` with the given `size` in bytes.
	///
	/// `key` is a short identifier (no leading slash needed; the platform
	/// prefix is added internally). Returns true on success; on failure
	/// [`Self::error`] carries a human-readable reason. An existing region
	/// is closed first.
	#[cfg(unix)]
	pub fn open(&mut self, key: &str, size: usize, mode: ShmMode) -> bool {
		self.close();
		self.key = key.to_string();
		self.size = size;
		self.mode = mode;

		if mode == ShmMode::Create {
			// A crashed owner leaks its segments (the name embeds the dead
			// pid, so nobody ever reuses or unlinks them); on a quota'd
			// tmpfs the accumulation eventually kills the next create.
			// Sweep the orphans of dead owners before creating anything.
			Self::cleanup_stale_owned_segments();
		}

		// POSIX shared-memory names must start with a single slash and
		// contain no others.
		let shm_name = format!("/{}", key.replace('/', "_"));
		let name_c = match std::ffi::CString::new(shm_name.clone()) {
			Ok(c) => c,
			Err(_) => {
				self.error = format!("invalid shm key {key:?} (contains NUL)");
				return false;
			}
		};
		self.shm_name = shm_name;

		let mut oflag = libc::O_RDWR;
		if mode == ShmMode::Create {
			oflag |= libc::O_CREAT | libc::O_EXCL;
			// Clear any stale segment left by a crashed previous run with
			// the same name.
			unsafe { libc::shm_unlink(name_c.as_ptr()) };
		}

		let fd = unsafe { libc::shm_open(name_c.as_ptr(), oflag, 0o600) };
		if fd < 0 {
			self.error = format!(
				"shm_open({}) failed: {}",
				self.shm_name,
				std::io::Error::last_os_error()
			);
			return false;
		}
		self.fd = fd;

		if mode == ShmMode::Create {
			if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
				self.error = format!("ftruncate failed: {}", std::io::Error::last_os_error());
				self.close();
				return false;
			}
		} else {
			// mmap() succeeds even beyond the real segment size and only
			// faults (SIGBUS) on access, so verify the segment is large
			// enough up front.
			let mut st: libc::stat = unsafe { std::mem::zeroed() };
			if unsafe { libc::fstat(fd, &mut st) } != 0 {
				self.error = format!("fstat failed: {}", std::io::Error::last_os_error());
				self.close();
				return false;
			}
			if (st.st_size as usize) < size {
				self.error = format!(
					"shared memory segment is {} bytes, smaller than the requested {}",
					st.st_size, size
				);
				self.close();
				return false;
			}
		}

		let data = unsafe {
			libc::mmap(
				ptr::null_mut(),
				size,
				libc::PROT_READ | libc::PROT_WRITE,
				libc::MAP_SHARED,
				fd,
				0,
			)
		};
		if data == libc::MAP_FAILED {
			self.error = format!("mmap failed: {}", std::io::Error::last_os_error());
			self.close();
			return false;
		}
		self.data = data as *mut u8;
		self.error.clear();

		if mode == ShmMode::Create {
			Self::track_owned_key(&self.key);
			// Reserve (and zero) the whole segment up front. `ftruncate`
			// alone does not reserve on tmpfs: the pages fault in on first
			// touch, and when the tmpfs is full / the user quota is
			// exhausted that touch is a SIGBUS that kills the process.
			// `posix_fallocate` performs the reservation eagerly and reports
			// ENOSPC/EDQUOT as a return value, so quota exhaustion degrades
			// to a render-manager fallback instead of a crash. The fresh
			// segment is already zero-filled (O_EXCL + stale unlink above),
			// so no separate memset pass is needed on success.
			let rc = unsafe { libc::posix_fallocate(fd, 0, size as libc::off_t) };
			if rc != 0 && rc != libc::EOPNOTSUPP && rc != libc::ENOSYS {
				self.error = format!(
					"reserving {} bytes of shared memory failed: {}",
					size,
					std::io::Error::from_raw_os_error(rc)
				);
				self.close();
				return false;
			}
			if rc != 0 {
				// Filesystem without fallocate support: fall back to
				// touching every page now (still better than faulting
				// lazily mid-render).
				unsafe { ptr::write_bytes(self.data, 0, size) };
			}
		}
		true
	}

	/// Unmap and (if owner) unlink the segment. Also called by `Drop`.
	#[cfg(unix)]
	pub fn close(&mut self) {
		if !self.data.is_null() {
			unsafe { libc::munmap(self.data as *mut std::ffi::c_void, self.size) };
			self.data = ptr::null_mut();
		}
		if self.fd >= 0 {
			unsafe { libc::close(self.fd) };
			self.fd = -1;
		}
		if self.mode == ShmMode::Create && !self.shm_name.is_empty() {
			// Only the owner unlinks, so the name is freed once both sides
			// have unmapped.
			if let Ok(c) = std::ffi::CString::new(self.shm_name.clone()) {
				unsafe { libc::shm_unlink(c.as_ptr()) };
			}
			self.shm_name.clear();
		}
		self.size = 0;
	}

	/// True when the region holds a live mapping.
	pub fn is_valid(&self) -> bool {
		!self.data.is_null()
	}

	/// The mapped data pointer (null when invalid).
	pub fn data(&self) -> *mut u8 {
		self.data
	}

	/// The mapping size in bytes.
	pub fn size(&self) -> usize {
		self.size
	}

	/// The key the region was opened with.
	pub fn key(&self) -> &str {
		&self.key
	}

	/// Human-readable reason of the last failed open.
	pub fn error(&self) -> &str {
		&self.error
	}
}

// ---------------------------------------------------------------------------
// Windows backend (named file-mapping objects)
// ---------------------------------------------------------------------------
//
// The Win32 file-mapping API (`CreateFileMappingW`/`OpenFileMappingW` +
// `MapViewOfFile`) is the direct analog of POSIX shm: a named section
// object shared between processes, mapped into each process's address
// space. Handles are `*mut c_void`; the mapped view is what callers read
// and write. The one semantic difference from POSIX is that a section
// object has no unlink step — the OS destroys it when the last handle to
// it closes (a crashed owner's handles are reclaimed automatically), which
// is also why a Create on a live existing name must fail like O_EXCL (see
// `open`).

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "C" {
	/// `CreateFileMappingW` — create a pagefile-backed section object
	/// (INVALID_HANDLE_VALUE as `h_file`), or return a handle to the
	/// existing object when `lp_name` is already in use.
	fn CreateFileMappingW(
		h_file: *mut c_void,
		lp_file_mapping_attributes: *mut c_void,
		fl_protect: u32,
		dw_maximum_size_high: u32,
		dw_maximum_size_low: u32,
		lp_name: *const u16,
	) -> *mut c_void;
	/// `OpenFileMappingW` — open an existing section object by name.
	fn OpenFileMappingW(
		dw_desired_access: u32,
		b_inherit_handle: i32,
		lp_name: *const u16,
	) -> *mut c_void;
	/// `MapViewOfFile` — map (part of) a section object into the process
	/// address space.
	fn MapViewOfFile(
		h_file_mapping_object: *mut c_void,
		dw_desired_access: u32,
		dw_file_offset_high: u32,
		dw_file_offset_low: u32,
		dw_number_of_bytes_to_map: usize,
	) -> *mut c_void;
	/// `UnmapViewOfFile` — release a mapped view.
	fn UnmapViewOfFile(lp_base_address: *mut c_void) -> i32;
	/// `CloseHandle` — close a kernel-object handle.
	fn CloseHandle(h_object: *mut c_void) -> i32;
	/// `GetLastError` — the last-error code set by a Win32 call.
	fn GetLastError() -> u32;
	/// `SetLastError` — set the last-error code (used to clear it before
	/// the ERROR_ALREADY_EXISTS probe, which must not see a stale value).
	fn SetLastError(dw_err_code: u32);
	/// `VirtualQuery` — describe the mapped region containing
	/// `lp_address`; used to size-check an attached segment.
	fn VirtualQuery(
		lp_address: *const c_void,
		lp_buffer: *mut MemoryBasicInformation,
		dw_length: usize,
	) -> usize;
}

/// `INVALID_HANDLE_VALUE` (winbase.h) — the `h_file` sentinel that makes
/// `CreateFileMappingW` back the section with the system page file.
#[cfg(windows)]
const INVALID_HANDLE_VALUE: *mut c_void = (-1isize) as *mut c_void;
/// `PAGE_READWRITE` (winnt.h) — read/write protection for the section.
#[cfg(windows)]
const PAGE_READWRITE: u32 = 0x04;
/// `FILE_MAP_ALL_ACCESS` (= `SECTION_ALL_ACCESS`, winnt.h) — full access
/// to the section, used for both open and map.
#[cfg(windows)]
const FILE_MAP_ALL_ACCESS: u32 = 0x000F_001F;
/// `ERROR_ALREADY_EXISTS` (winerror.h) — reported by `CreateFileMappingW`
/// when the named object already exists.
#[cfg(windows)]
const ERROR_ALREADY_EXISTS: u32 = 183;

/// `MEMORY_BASIC_INFORMATION` (winnt.h) — the layout `VirtualQuery`
/// writes. Field-for-field, x64: 8+8+4+2+(pad)+8+4+4+4 = 48 bytes.
#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryBasicInformation {
	base_address: *mut c_void,
	allocation_base: *mut c_void,
	allocation_protect: u32,
	partition_id: u16,
	region_size: usize,
	state: u32,
	protect: u32,
	type_: u32,
}

/// Encode `s` as a NUL-terminated UTF-16 buffer for the `...W` Win32
/// entry points. Returns None when `s` contains a NUL byte (the name
/// would be truncated).
#[cfg(windows)]
fn to_wide(s: &str) -> Option<Vec<u16>> {
	if s.contains('\0') {
		return None;
	}
	let mut wide: Vec<u16> = s.encode_utf16().collect();
	wide.push(0);
	Some(wide)
}

#[cfg(windows)]
impl SharedMemoryRegion {
	/// Best-effort unlink of the segment named by `key` (the same naming
	/// as [`Self::open`]).
	///
	/// Windows no-op: section objects have no name-unlink step. The OS
	/// destroys the object automatically when the last handle to it is
	/// closed (a crashed owner's handles are reclaimed by the kernel), so
	/// there is nothing for the caller to clean up here.
	pub fn unlink_key(_key: &str) {}

	/// Open the segment identified by `key` with the given `size` in bytes.
	///
	/// Windows: `CreateFileMappingW` (Create) or `OpenFileMappingW`
	/// (Attach) for a pagefile-backed section named `Local\OakShm<key>`,
	/// then `MapViewOfFile`. Returns true on success; on failure
	/// [`Self::error`] carries a human-readable reason. An existing region
	/// is closed first.
	pub fn open(&mut self, key: &str, size: usize, mode: ShmMode) -> bool {
		self.close();
		self.key = key.to_string();
		self.size = size;
		self.mode = mode;

		// Win32 object names are case-sensitive and cannot contain '/' or
		// '\'; the "Local\" namespace scopes the object to the terminal
		// session (the "Global\" namespace would require the
		// SeCreateGlobalPrivilege right). The `OakShm` prefix keeps the
		// name clear of user objects and makes keys recognizable in e.g.
		// Process Explorer.
		let map_name = format!("Local\\OakShm{}", key.replace(['/', '\\'], "_"));
		let name_wide = match to_wide(&map_name) {
			Some(wide) => wide,
			None => {
				self.error = format!("invalid shm key {key:?} (contains NUL)");
				return false;
			}
		};

		let size_hi = ((size as u64) >> 32) as u32;
		let size_lo = (size as u64 & 0xFFFF_FFFF) as u32;
		let (handle, api) = unsafe {
			if mode == ShmMode::Create {
				// Clear the last-error code so the already-exists probe
				// below only fires on a real collision.
				SetLastError(0);
				(
					CreateFileMappingW(
						INVALID_HANDLE_VALUE,
						ptr::null_mut(),
						PAGE_READWRITE,
						size_hi,
						size_lo,
						name_wide.as_ptr(),
					),
					"CreateFileMappingW",
				)
			} else {
				(
					OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name_wide.as_ptr()),
					"OpenFileMappingW",
				)
			}
		};
		if handle.is_null() {
			self.error = format!(
				"{api}({map_name}) failed: {}",
				std::io::Error::last_os_error()
			);
			return false;
		}
		self.mapping = handle;

		if mode == ShmMode::Create {
			// CreateFileMappingW on an existing name does not fail — it
			// returns a handle to the existing object with GetLastError ==
			// ERROR_ALREADY_EXISTS. That is the O_EXCL case, and unlike
			// POSIX there is no way to unlink a live section object, so
			// Create fails on an existing segment. (A segment left by a
			// crashed owner is already gone: the OS destroyed it when its
			// last handle closed.)
			if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
				unsafe { CloseHandle(handle) };
				self.mapping = ptr::null_mut();
				self.error = format!("segment {key:?} already exists");
				return false;
			}
		}

		// dwNumberOfBytesToMap = 0 maps the whole object. The POSIX path
		// mmap()s exactly `size` and size-checks first (fstat) because a
		// too-short mapping only faults on access; the equivalent check
		// runs below after mapping.
		let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
		if view.is_null() {
			self.error = format!(
				"MapViewOfFile({map_name}) failed: {}",
				std::io::Error::last_os_error()
			);
			self.close();
			return false;
		}
		self.data = view as *mut u8;

		if mode == ShmMode::Attach {
			// The fstat analog: verify the existing segment is at least
			// `size` bytes. VirtualQuery reports the region size rounded
			// up to a page boundary, so sub-page over-requests are not
			// detected (Windows stores section sizes page-granular anyway).
			let mut mbi = unsafe { std::mem::zeroed::<MemoryBasicInformation>() };
			if unsafe {
				VirtualQuery(
					view,
					&mut mbi,
					std::mem::size_of::<MemoryBasicInformation>(),
				)
			} == 0
			{
				self.error = format!("VirtualQuery failed: {}", std::io::Error::last_os_error());
				self.close();
				return false;
			}
			if mbi.region_size < size {
				self.error = format!(
					"shared memory segment is {} bytes, smaller than the requested {}",
					mbi.region_size, size
				);
				self.close();
				return false;
			}
		}
		self.error.clear();
		true
	}

	/// Unmap and close the segment. Also called by `Drop`.
	///
	/// There is no owner-only unlink step: Windows destroys the object
	/// when the last handle closes (the owner's `CloseHandle` here, after
	/// every peer has unmapped) — the automatic equivalent of the POSIX
	/// `shm_unlink`, so no further cleanup is needed.
	pub fn close(&mut self) {
		if !self.data.is_null() {
			unsafe { UnmapViewOfFile(self.data as *mut c_void) };
			self.data = ptr::null_mut();
		}
		if !self.mapping.is_null() {
			unsafe { CloseHandle(self.mapping) };
			self.mapping = ptr::null_mut();
		}
		self.size = 0;
	}
}

impl Default for SharedMemoryRegion {
	fn default() -> Self {
		SharedMemoryRegion::new()
	}
}

impl Drop for SharedMemoryRegion {
	fn drop(&mut self) {
		self.close();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// ---- Control-plane protocol ------------------------------------------

	#[test]
	fn handshake_wire_format_matches_cpp_field_names() {
		let hs = HandshakeMsg {
			protocol_version: 1,
			shm_key: "olive-rw-1234-0-out".into(),
			input_shm_key: "".into(),
			input_slots: 0,
			output_slots: 6,
			slot_data_bytes: 4096,
			input_slot_data_bytes: 0,
		};
		let value = hs.to_json();
		// Key order is not part of the contract (JSON objects; the C++
		// QJsonObject is hash-ordered too), but the names must match the
		// C++ serializer exactly.
		assert_eq!(value["type"], "handshake");
		assert_eq!(value["protocol_version"], 1);
		assert_eq!(value["shm_key"], "olive-rw-1234-0-out");
		assert_eq!(value["input_shm_key"], "");
		assert_eq!(value["input_slots"], 0);
		assert_eq!(value["output_slots"], 6);
		assert_eq!(value["slot_data_bytes"], 4096);
		assert_eq!(value["input_slot_data_bytes"], 0);
		// And the serialized line must parse back to the same object.
		let round: serde_json::Value =
			serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
		assert_eq!(round, value);
	}

	#[test]
	fn plugin_progress_message_round_trips() {
		let msg = PluginProgressMsg {
			label: "render".into(),
			message: "pass 1".into(),
			fraction: 0.5,
		};
		let value = msg.to_json();
		assert_eq!(value["type"], TYPE_PLUGIN_PROGRESS);
		assert_eq!(value["label"], "render");
		assert_eq!(value["message"], "pass 1");
		assert_eq!(value["fraction"], 0.5);

		// The NDJSON line parses back to the same payload (the wire
		// contract the worker writes and the dispatcher reads).
		let line = serde_json::to_string(&value).unwrap();
		let parsed: PluginProgressMsg = serde_json::from_str(&line).unwrap();
		assert_eq!(parsed, msg);
	}

	#[test]
	fn plugin_progress_defaults_on_missing_fields() {
		// A minimal message (e.g. progressEnd forwarded as fraction 1.0
		// without touching label/message) deserializes cleanly.
		let m: PluginProgressMsg =
			serde_json::from_str(r#"{"type":"plugin_progress","fraction":1.0}"#).unwrap();
		assert_eq!(m.fraction, 1.0);
		assert!(m.label.is_empty());
		assert!(m.message.is_empty());
	}

	#[test]
	fn plugin_cancel_wire_shape() {
		let value = plugin_cancel_json();
		assert_eq!(value["type"], TYPE_PLUGIN_CANCEL);
	}

	#[test]
	fn render_frame_parse_accepts_cpp_field_names() {
		let json = r#"{"type":"render_frame","ticket":42,"node":"abcd","time_num":1,"time_den":24,"width":1920,"height":1080,"format":-1,"channels":0,"mode":0,"input_slot":-1,"input_slots":[],"has_color_transform":false,"color_output":"","color_view":"","color_look":""}"#;
		let m: RenderFrameMsg = serde_json::from_str(json).unwrap();
		assert_eq!(m.ticket, 42);
		assert_eq!(m.node, "abcd");
		assert_eq!(m.time_num, 1);
		assert_eq!(m.time_den, 24);
		assert_eq!(m.width, 1920);
		assert_eq!(m.input_slot, -1);
	}

	#[test]
	fn render_frame_defaults_on_missing_fields() {
		// The C++ parser defaults missing fields (QJsonValue defaults);
		// serde(default) mirrors that.
		let m: RenderFrameMsg =
			serde_json::from_str(r#"{"type":"render_frame","ticket":7}"#).unwrap();
		assert_eq!(m.ticket, 7);
		assert_eq!(m.time_den, 0);
		assert!(m.node.is_empty());
		assert!(!m.has_color_transform);
	}

	#[test]
	fn error_message_carries_ticket_only_when_nonzero() {
		assert_eq!(
			error_message("boom", None),
			json!({ "type": "error", "message": "boom" })
		);
		assert_eq!(
			error_message("boom", Some(0)),
			json!({ "type": "error", "message": "boom" })
		);
		assert_eq!(
			error_message("boom", Some(9)),
			json!({ "type": "error", "message": "boom", "ticket": 9 })
		);
	}

	#[test]
	fn write_message_emits_one_json_line() {
		let mut buf = Vec::new();
		write_message(&mut buf, &json!({ "type": "shutdown" })).unwrap();
		assert_eq!(String::from_utf8(buf).unwrap(), "{\"type\":\"shutdown\"}\n");
	}

	// ---- Protocol v2 messages (M15 S1) ---------------------------------

	#[test]
	fn hello_caps_wire_roundtrip() {
		let caps = HelloCapsMsg {
			protocol_version: 1,
			formats: vec![4, SLOT_FORMAT_BGRA8],
			max_slot_bytes: 8_294_400,
		};
		let line = json!({
			"type": TYPE_HELLO_CAPS,
			"protocol_version": 1,
			"formats": [4, SLOT_FORMAT_BGRA8],
			"max_slot_bytes": 8_294_400i64,
		});
		// Parse the wire line (the `"type"` field is tolerated by serde's
		// default behavior of ignoring unknown fields).
		let parsed: HelloCapsMsg = serde_json::from_value(line.clone()).unwrap();
		assert_eq!(parsed, caps);
		// And the field names are the canonical wire names.
		let value = serde_json::to_value(&caps).unwrap();
		assert_eq!(value["protocol_version"], 1);
		assert_eq!(value["formats"], json!([4, SLOT_FORMAT_BGRA8]));
		assert_eq!(value["max_slot_bytes"], 8_294_400i64);
	}

	#[test]
	fn render_batch_wire_roundtrip() {
		let batch = RenderBatchMsg {
			batch_id: 7,
			tickets: vec![
				BatchTicketSpec {
					ticket: 41,
					slot: 0,
					time_num: 3,
					time_den: 24,
					width: 320,
					height: 180,
					format: SLOT_FORMAT_BGRA8,
					channels: 4,
					footage_file: "a.mp4".into(),
					footage_stream: 0,
					montage: vec![],
					adjustments: vec![],
					viewer_node: 0,
					project_key: String::new(),
				},
				BatchTicketSpec {
					ticket: 42,
					slot: 1,
					time_num: 4,
					time_den: 24,
					width: 320,
					height: 180,
					format: 4,
					channels: 4,
					footage_file: String::new(),
					footage_stream: 0,
					montage: vec![WireMontageClip {
						filename: "b.mp4".into(),
						stream_index: 1,
						in_num: 0,
						in_den: 24,
						out_num: 48,
						out_den: 24,
						media_in_num: 10,
						media_in_den: 24,
						gain: 0.5,
						effects: vec![WireMontageEffect {
							type_id: "org.olivevideoeditor.Olive.opacity".into(),
							enabled: true,
							effect_input_id: "tex_in".into(),
							params: vec![WireEffectParam {
								input: "opacity_in".into(),
								value: WireNodeValue::Float(0.5),
							}],
						}],
					}],
					adjustments: vec![WireAdjustmentSpan {
						in_num: 0,
						in_den: 24,
						out_num: 48,
						out_den: 24,
						track_index: 1,
						effects: vec![WireMontageEffect {
							type_id: "org.olivevideoeditor.Olive.opacity".into(),
							enabled: true,
							effect_input_id: "tex_in".into(),
							params: vec![WireEffectParam {
								input: "opacity_in".into(),
								value: WireNodeValue::Float(0.5),
							}],
						}],
					}],
					viewer_node: 0,
					project_key: String::new(),
				},
			],
		};
		let value = serde_json::to_value(&batch).unwrap();
		assert_eq!(value["batch_id"], 7);
		assert_eq!(value["tickets"][0]["ticket"], 41);
		assert_eq!(value["tickets"][0]["slot"], 0);
		assert_eq!(value["tickets"][0]["format"], SLOT_FORMAT_BGRA8);
		assert_eq!(value["tickets"][1]["montage"][0]["filename"], "b.mp4");
		assert_eq!(value["tickets"][1]["montage"][0]["media_in_num"], 10);
		// The effect stack rides the clip as an additive v2 field.
		assert_eq!(
			value["tickets"][1]["montage"][0]["effects"][0]["type_id"],
			"org.olivevideoeditor.Olive.opacity"
		);
		assert_eq!(
			value["tickets"][1]["montage"][0]["effects"][0]["params"][0]["value"],
			json!({ "t": "float", "v": 0.5 })
		);
		// The adjustment layer rides the ticket as an additive v2 field: its
		// boundary is a montage-list index, not a track number.
		assert_eq!(value["tickets"][1]["adjustments"][0]["track_index"], 1);
		assert_eq!(value["tickets"][1]["adjustments"][0]["out_num"], 48);
		assert_eq!(
			value["tickets"][1]["adjustments"][0]["effects"][0]["type_id"],
			"org.olivevideoeditor.Olive.opacity"
		);
		// The ticket-side conversions agree with the wire structs.
		let parsed: WireAdjustmentSpan =
			serde_json::from_value(value["tickets"][1]["adjustments"][0].clone()).unwrap();
		let span = adjustment_from_wire(&parsed);
		assert_eq!(span.in_time, oak_core::Rational::new(0, 24));
		assert_eq!(span.out_time, oak_core::Rational::new(48, 24));
		assert_eq!(span.track_index, 1);
		assert_eq!(
			span.effects[0].type_id,
			"org.olivevideoeditor.Olive.opacity"
		);
		assert_eq!(span.effects[0].effect_input_id.as_deref(), Some("tex_in"));
		assert_eq!(
			span.effects[0].params[0].1,
			oak_node::value::NodeValue::Float(0.5)
		);
		// `Rational::new` normalizes, so the wire form of the parsed span
		// collapses 48/24 to 2/1.
		let wire = wire_adjustment_from(&span);
		assert_eq!(
			wire,
			WireAdjustmentSpan {
				in_num: 0,
				in_den: 1,
				out_num: 2,
				out_den: 1,
				track_index: 1,
				effects: parsed.effects.clone(),
			}
		);
		// Round-trip back to the struct.
		let round: RenderBatchMsg = serde_json::from_value(value).unwrap();
		assert_eq!(round, batch);
		// Defaults: a bare ticket parses (missing fields default).
		let bare: BatchTicketSpec =
			serde_json::from_str(r#"{"ticket":1,"slot":2}"#).unwrap();
		assert_eq!(bare.ticket, 1);
		assert_eq!(bare.slot, 2);
		assert_eq!(bare.format, 0);
		assert!(bare.montage.is_empty());
		assert!(
			bare.adjustments.is_empty(),
			"v1 tickets carry no adjustment layers"
		);
	}

	/// Protocol v2 compatibility: a v1-shaped montage clip (no `effects`
	/// field) parses with an empty effect stack, and the node-value wire
	/// enum round-trips every plain-data variant.
	#[test]
	fn montage_clip_effects_are_additive_and_values_roundtrip() {
		let legacy = json!({
			"filename": "a.mp4",
			"stream_index": 0,
			"in_num": 0, "in_den": 24,
			"out_num": 48, "out_den": 24,
			"media_in_num": 0, "media_in_den": 24,
			"gain": 1.0,
		});
		let parsed: WireMontageClip = serde_json::from_value(legacy).unwrap();
		assert!(parsed.effects.is_empty(), "v1 clips carry no effects");

		use oak_node::value::NodeValue as NV;
		let cases = [
			NV::None,
			NV::Int(-3),
			NV::Float(0.25),
			NV::Color([0.1, 0.2, 0.3, 1.0]),
			NV::Text("hello".into()),
			NV::Boolean(true),
			NV::Rational(oak_core::Rational::new(1, 24)),
			NV::Vec2([1.0, 2.0]),
			NV::Vec3([1.0, 2.0, 3.0]),
			NV::Vec4([1.0, 2.0, 3.0, 4.0]),
			NV::Matrix([0.0; 16]),
			NV::Combo(2),
			NV::StrCombo("choice".into()),
			NV::Binary(vec![1, 2, 3]),
		];
		for value in cases {
			let wire = WireNodeValue::from_node_value(&value).expect("plain data has a wire form");
			let json = serde_json::to_string(&wire).unwrap();
			let back: WireNodeValue = serde_json::from_str(&json).unwrap();
			assert_eq!(back.to_node_value(), value, "wire round-trip of {value:?}");
		}
		// Connection/handle variants have no wire form.
		assert!(WireNodeValue::from_node_value(&NV::PushButton).is_none());
	}

	#[test]
	fn batch_accepted_and_frame_failed_wire_roundtrip() {
		let accepted = BatchAcceptedMsg {
			batch_id: 9,
			tickets: vec![1, 2, 3],
		};
		let value = serde_json::to_value(&accepted).unwrap();
		assert_eq!(value["batch_id"], 9);
		assert_eq!(value["tickets"], json!([1, 2, 3]));
		let round: BatchAcceptedMsg = serde_json::from_value(value).unwrap();
		assert_eq!(round, accepted);

		let failed = FrameFailedMsg {
			ticket: 12,
			error: "decode failed".into(),
		};
		let value = serde_json::to_value(&failed).unwrap();
		assert_eq!(value["ticket"], 12);
		assert_eq!(value["error"], "decode failed");
		let round: FrameFailedMsg = serde_json::from_value(value).unwrap();
		assert_eq!(round, failed);
		// Wire line with the type tag parses too.
		let tagged: FrameFailedMsg = serde_json::from_value(json!({
			"type": TYPE_FRAME_FAILED,
			"ticket": 12,
			"error": "decode failed",
		}))
		.unwrap();
		assert_eq!(tagged, failed);
	}

	#[test]
	fn render_audio_batch_wire_roundtrip() {
		// M15 S3: the audio batch message reuses the claim/credit flow of
		// render_batch with a dedicated ticket spec.
		let batch = RenderAudioBatchMsg {
			batch_id: 5,
			tickets: vec![AudioTicketSpec {
				ticket: 41,
				slot: 0,
				time_num: 0,
				time_den: 48000,
				duration_num: 1600,
				duration_den: 48000,
				sample_rate: 48000,
				channel_layout: 0x3,
				channels: 2,
				montage: vec![WireMontageClip {
					filename: "a.mp4".into(),
					stream_index: 0,
					in_num: 0,
					in_den: 24,
					out_num: 48,
					out_den: 24,
					media_in_num: 10,
					media_in_den: 24,
					gain: 0.5,
					effects: vec![],
				}],
			}],
		};
		let value = serde_json::to_value(&batch).unwrap();
		assert_eq!(value["batch_id"], 5);
		assert_eq!(value["tickets"][0]["ticket"], 41);
		assert_eq!(value["tickets"][0]["slot"], 0);
		assert_eq!(value["tickets"][0]["sample_rate"], 48000);
		assert_eq!(value["tickets"][0]["channel_layout"], 3);
		assert_eq!(value["tickets"][0]["channels"], 2);
		assert_eq!(value["tickets"][0]["duration_num"], 1600);
		assert_eq!(value["tickets"][0]["montage"][0]["filename"], "a.mp4");
		let round: RenderAudioBatchMsg = serde_json::from_value(value).unwrap();
		assert_eq!(round, batch);
		// Defaults: a bare audio ticket parses (missing fields default).
		let bare: AudioTicketSpec = serde_json::from_str(r#"{"ticket":1,"slot":2}"#).unwrap();
		assert_eq!(bare.ticket, 1);
		assert_eq!(bare.slot, 2);
		assert_eq!(bare.sample_rate, 0);
		assert!(bare.montage.is_empty());
	}

	#[test]
	fn v2_type_constants_are_stable_wire_names() {
		assert_eq!(TYPE_HELLO_CAPS, "hello_caps");
		assert_eq!(TYPE_RENDER_BATCH, "render_batch");
		assert_eq!(TYPE_BATCH_ACCEPTED, "batch_accepted");
		assert_eq!(TYPE_FRAME_FAILED, "frame_failed");
		assert_eq!(TYPE_RENDER_AUDIO_BATCH, "render_audio_batch");
		assert_eq!(TYPE_SHUTDOWN, "shutdown");
		// BGRA8 slot format stays outside the PixelFormat enum range; the
		// audio slot format follows it (M15 S3).
		assert_eq!(SLOT_FORMAT_BGRA8, 100);
		assert_eq!(SLOT_FORMAT_AUDIO_F32, 101);
	}

	// ---- Shared-memory transport -----------------------------------------

	/// A unique, temporary POSIX segment key for a test (pid + counter), so
	/// parallel test runs never collide.
	fn test_key(name: &str) -> String {
		static COUNTER: AtomicU32 = AtomicU32::new(0);
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		SharedMemoryRegion::make_key(i64::from(std::process::id()), (n & 0x7FFF) as i32)
			+ &format!("-{name}")
	}

	/// Create one segment and map it a second time — the in-process
	/// equivalent of two processes sharing a segment. Returns
	/// `(owner_region, peer_region)`; both must be kept alive for the
	/// whole test (the peer is an attach that does not unlink).
	fn two_mappings(key: &str, size: usize) -> (SharedMemoryRegion, SharedMemoryRegion) {
		let mut owner = SharedMemoryRegion::new();
		assert!(
			owner.open(key, size, ShmMode::Create),
			"create failed: {}",
			owner.error()
		);
		let mut peer = SharedMemoryRegion::new();
		assert!(
			peer.open(key, size, ShmMode::Attach),
			"attach failed: {}",
			peer.error()
		);
		(owner, peer)
	}

	// ---- SpscRingBuffer -------------------------------------------------

	#[test]
	fn ring_bytes_needed_matches_cpp_layout() {
		// 12 header bytes + capacity * 4.
		assert_eq!(SpscRingBuffer::bytes_needed(4), 12 + 16);
		assert_eq!(SpscRingBuffer::bytes_needed(5), 12 + 20);
		assert_eq!(SpscRingBuffer::bytes_needed(0), 12);
	}

	#[test]
	fn ring_empty_full_and_single_entry() {
		let key = test_key("ring-empty");
		let size = SpscRingBuffer::bytes_needed(4);
		let (owner, peer) = two_mappings(&key, size);
		// SAFETY: both mappings are live and at least `size` bytes.
		let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
		let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

		assert!(unsafe { cons.is_empty_approx() });
		let mut v = 99;
		assert!(!unsafe { cons.pop(&mut v) });
		assert_eq!(v, 99);

		assert!(unsafe { prod.push(7) });
		assert!(!unsafe { cons.is_empty_approx() });
		assert_eq!(unsafe { cons.size_approx() }, 1);
		assert!(unsafe { cons.pop(&mut v) });
		assert_eq!(v, 7);
		assert!(unsafe { cons.is_empty_approx() });
	}

	#[test]
	fn ring_capacity_minus_one_live_entries() {
		// A ring of capacity N holds at most N-1 entries (one slot is
		// always left empty to tell full from empty).
		let key = test_key("ring-cap");
		let size = SpscRingBuffer::bytes_needed(4);
		let (owner, peer) = two_mappings(&key, size);
		// SAFETY: live mappings.
		let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
		let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

		for i in 0..3 {
			assert!(unsafe { prod.push(i) });
		}
		// The 4th push must fail: head would collide with tail.
		assert!(!unsafe { prod.push(99) });

		let mut v = 0;
		for expected in 0..3 {
			assert!(unsafe { cons.pop(&mut v) });
			assert_eq!(v, expected);
		}
		assert!(!unsafe { cons.pop(&mut v) });
	}

	#[test]
	fn ring_wraparound_preserves_order() {
		// Fill, drain, then wrap past the end of the slot array: cursors
		// are modulo-capacity, order must be preserved across the wrap.
		let key = test_key("ring-wrap");
		let size = SpscRingBuffer::bytes_needed(4);
		let (owner, peer) = two_mappings(&key, size);
		// SAFETY: live mappings.
		let prod = unsafe { SpscRingBuffer::create(owner.data(), 4) };
		let cons = unsafe { SpscRingBuffer::attach(peer.data()) };

		for i in 0..3 {
			assert!(unsafe { prod.push(i) });
		}
		let mut v = 0;
		for _ in 0..3 {
			assert!(unsafe { cons.pop(&mut v) });
		}
		// Ring is empty again; push past the wrap point.
		for i in 3..6 {
			assert!(unsafe { prod.push(i) });
		}
		for expected in 3..6 {
			assert!(unsafe { cons.pop(&mut v) });
			assert_eq!(v, expected);
		}
	}

	// ---- FrameSlotPool --------------------------------------------------

	#[test]
	fn framepool_bytes_needed_matches_cpp_offsets() {
		// Recompute by hand with the C++ layout: header 64, each ring
		// align_up(12 + 4*(n+1), 64), meta align_up(176*n, 64), data
		// align_up(slot_bytes, 64) * n.
		let check = |n: u32, slot: usize| {
			let ring = align_up(12 + 4 * (n as usize + 1), 64);
			let expected =
				64 + ring + ring + align_up(176 * n as usize, 64) + align_up(slot, 64) * n as usize;
			assert_eq!(FrameSlotPool::bytes_needed(n, slot), expected);
		};
		check(4, 4096);
		check(6, 1_000_000);
		check(1, 64);
		check(3, 100);
	}

	#[test]
	fn framepool_create_attach_two_processes_both_directions() {
		// "Two processes": two mappings of the same segment. Owner creates
		// the pool; the peer attaches. A filler on one side and a drainer
		// on the other exchange slots in both directions.
		let key = test_key("pool-bidi");
		let slots = 4u32;
		let slot_bytes = 64usize;
		let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
		let (owner, peer) = two_mappings(&key, size);

		// SAFETY: both mappings are live and sized by bytes_needed.
		let filler = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
		let drainer = unsafe { FrameSlotPool::attach(peer.data()) };

		assert!(filler.is_valid());
		assert!(drainer.is_valid());
		assert_eq!(drainer.slot_count(), slots);
		assert_eq!(drainer.slot_data_bytes(), slot_bytes);

		// Filler acquires every slot exactly once (seeded free ring), then
		// the free ring is empty.
		let mut got = Vec::new();
		for _ in 0..slots {
			let mut s = 0;
			assert!(unsafe { filler.acquire(&mut s) });
			got.push(s);
		}
		got.sort_unstable();
		assert_eq!(got, vec![0, 1, 2, 3]);
		let mut extra = 0;
		assert!(!unsafe { filler.acquire(&mut extra) });
		// Drainer sees nothing ready yet.
		assert!(!unsafe { drainer.consume(&mut extra) });

		// Filler writes pixels + meta into two slots and publishes them.
		for (i, slot) in [0u32, 2u32].iter().enumerate() {
			// SAFETY: `slot` was acquired above.
			let data = unsafe { filler.slot_data(*slot) };
			unsafe { ptr::write_bytes(data, (i * 40 + 1) as u8, slot_bytes) };
			// SAFETY: slot in range.
			let meta = unsafe { &mut *filler.meta(*slot) };
			meta.id = 100 + *slot as i64;
			meta.width = 8;
			meta.height = 8;
			meta.data_size = slot_bytes as i32;
			assert!(unsafe { filler.publish(*slot) });
		}

		// Drainer consumes them through its own mapping and sees the same
		// payloads and metadata.
		let mut consumed = Vec::new();
		for _ in 0..2 {
			let mut s = 0;
			assert!(unsafe { drainer.consume(&mut s) });
			// SAFETY: s was consumed.
			let data = unsafe { drainer.slot_data_const(s) };
			let meta = unsafe { &*drainer.meta_const(s) };
			assert_eq!(meta.id, 100 + s as i64);
			assert_eq!(meta.width, 8);
			assert_eq!(meta.data_size, slot_bytes as i32);
			// SAFETY: slot_bytes readable in the slot block.
			let first = unsafe { *data };
			assert_eq!(first, ((s as usize / 2) * 40 + 1) as u8);
			consumed.push(s);
		}
		consumed.sort_unstable();
		assert_eq!(consumed, vec![0, 2]);
		assert!(!unsafe { drainer.consume(&mut extra) });

		// Drainer releases the slots back; the filler can acquire them
		// again — the full round trip through both rings.
		for s in consumed {
			assert!(unsafe { drainer.release(s) });
		}
		let mut s = 0;
		assert!(unsafe { filler.acquire(&mut s) });
		assert_eq!(s, 0);
	}

	#[test]
	fn framepool_wraparound_and_full_edges() {
		// Small pool: cycle every slot many times, verifying the rings'
		// modulo behavior end to end.
		let key = test_key("pool-wrap");
		let slots = 3u32;
		let slot_bytes = 32usize;
		let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
		let (owner, peer) = two_mappings(&key, size);

		// SAFETY: live mappings.
		let filler = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
		let drainer = unsafe { FrameSlotPool::attach(peer.data()) };

		for cycle in 0..4u32 {
			let mut published = Vec::new();
			for _ in 0..slots {
				let mut s = 0;
				assert!(unsafe { filler.acquire(&mut s) }, "cycle {cycle}");
				// SAFETY: acquired slot.
				unsafe { ptr::write_bytes(filler.slot_data(s), cycle as u8, slot_bytes) };
				// SAFETY: slot in range.
				let meta = unsafe { &mut *filler.meta(s) };
				meta.id = i64::from(cycle * 100 + s);
				assert!(unsafe { filler.publish(s) });
				published.push(s);
			}
			// Pool is full on the filler side.
			let mut x = 0;
			assert!(!unsafe { filler.acquire(&mut x) });

			// Drain everything on the drainer side.
			let mut consumed = Vec::new();
			for _ in 0..slots {
				let mut s = 0;
				assert!(unsafe { drainer.consume(&mut s) });
				// SAFETY: consumed slot.
				let meta = unsafe { &*drainer.meta_const(s) };
				assert_eq!(meta.id, i64::from(cycle * 100 + s));
				// SAFETY: 1 byte readable.
				assert_eq!(unsafe { *drainer.slot_data_const(s) }, cycle as u8);
				consumed.push(s);
			}
			assert!(!unsafe { drainer.consume(&mut x) });
			consumed.sort_unstable();
			assert_eq!(consumed, vec![0, 1, 2]);

			for s in consumed {
				assert!(unsafe { drainer.release(s) });
			}
		}
	}

	#[test]
	fn framepool_attach_rejects_wrong_magic() {
		let key = test_key("pool-badmagic");
		let size = FrameSlotPool::bytes_needed(2, 16);
		let (owner, _peer) = two_mappings(&key, size);
		// Overwrite the header area with garbage — no pool magic.
		// SAFETY: owner mapping is live.
		unsafe { ptr::write_bytes(owner.data(), 0xAB, 64) };
		// SAFETY: buffer is live.
		let pool = unsafe { FrameSlotPool::attach(owner.data()) };
		assert!(!pool.is_valid());
		assert_eq!(pool.slot_count(), 0);
		assert_eq!(pool.slot_data_bytes(), 0);
	}

	#[test]
	fn framepool_pool_over_reused_segment_is_consistent() {
		// A pool that has been cycled fully and then attached fresh reports
		// the same geometry as bytes_needed computed it.
		let key = test_key("pool-geometry");
		let slots = 5u32;
		let slot_bytes = 1000usize;
		let size = FrameSlotPool::bytes_needed(slots, slot_bytes);
		let (owner, peer) = two_mappings(&key, size);
		// SAFETY: live mappings.
		let _ = unsafe { FrameSlotPool::create(owner.data(), slots, slot_bytes) };
		let attached = unsafe { FrameSlotPool::attach(peer.data()) };
		assert!(attached.is_valid());
		assert_eq!(attached.slot_count(), slots);
		assert_eq!(attached.slot_data_bytes(), slot_bytes);
		// Slot stride is 64-aligned (matches the C++ data layout).
		// SAFETY: valid pool.
		let s0 = unsafe { attached.slot_data(0) };
		let s1 = unsafe { attached.slot_data(1) };
		assert_eq!(s1 as usize - s0 as usize, align_up(slot_bytes, K_ALIGN));
	}

	// ---- SharedMemoryRegion ---------------------------------------------

	#[test]
	fn region_create_attach_write_visibility() {
		let key = test_key("region-vis");
		let size = 4096usize;
		let (mut owner, mut peer) = two_mappings(&key, size);
		assert!(owner.is_valid());
		assert!(peer.is_valid());
		assert_eq!(owner.size(), size);
		assert_eq!(peer.size(), size);
		assert_eq!(owner.key(), key);
		assert_eq!(peer.key(), key);

		// Owner writes; peer sees it through its own mapping.
		// SAFETY: both mappings are live with `size` bytes.
		unsafe {
			let dst = owner.data() as *mut u32;
			*dst = 0xDEADBEEF;
		}
		// SAFETY: peer mapping live.
		let seen = unsafe { *(peer.data() as *const u32) };
		assert_eq!(seen, 0xDEADBEEF);

		// Peer writes back; owner sees it.
		// SAFETY: peer mapping live.
		unsafe {
			let dst = peer.data() as *mut u32;
			*dst = 0x12345678;
		}
		// SAFETY: owner mapping live.
		assert_eq!(unsafe { *(owner.data() as *const u32) }, 0x12345678);

		// Closing the ATTACH side does not unlink: while the owner lives,
		// a third mapping can still open the name.
		peer.close();
		assert!(!peer.is_valid());
		let mut third = SharedMemoryRegion::new();
		assert!(third.open(&key, size, ShmMode::Attach), "{}", third.error());
		assert!(third.is_valid());
		third.close();

		// Closing the OWNER unlinks the segment; further attaches fail.
		owner.close();
		assert!(!owner.is_valid());
		let mut fourth = SharedMemoryRegion::new();
		assert!(!fourth.open(&key, size, ShmMode::Attach));
	}

	#[test]
	#[cfg(windows)]
	fn region_windows_create_attach_roundtrip() {
		// The Windows backend round trip — create -> write -> open -> read —
		// with the same assertions as the platform-neutral
		// `region_create_attach_write_visibility` test above.
		let key = test_key("win-roundtrip");
		let size = 4096usize;
		let (mut owner, mut peer) = two_mappings(&key, size);
		assert!(owner.is_valid());
		assert!(peer.is_valid());
		assert_eq!(owner.size(), size);
		assert_eq!(peer.size(), size);
		assert_eq!(owner.key(), key);
		assert_eq!(peer.key(), key);

		// Owner writes; peer sees it through its own mapping, and vice
		// versa.
		// SAFETY: both mappings are live with `size` bytes.
		unsafe {
			let dst = owner.data() as *mut u32;
			*dst = 0xDEADBEEF;
		}
		// SAFETY: peer mapping live.
		assert_eq!(unsafe { *(peer.data() as *const u32) }, 0xDEADBEEF);
		// SAFETY: peer mapping live.
		unsafe {
			let dst = peer.data() as *mut u32;
			*dst = 0x12345678;
		}
		// SAFETY: owner mapping live.
		assert_eq!(unsafe { *(owner.data() as *const u32) }, 0x12345678);

		// The object dies when the LAST handle closes: closing the attach
		// side leaves the owner's object open (a third attach still
		// works); closing the owner destroys it (further attaches fail).
		peer.close();
		assert!(!peer.is_valid());
		let mut third = SharedMemoryRegion::new();
		assert!(third.open(&key, size, ShmMode::Attach), "{}", third.error());
		assert!(third.is_valid());
		third.close();

		owner.close();
		assert!(!owner.is_valid());
		let mut fourth = SharedMemoryRegion::new();
		assert!(!fourth.open(&key, size, ShmMode::Attach));
	}

	#[test]
	#[cfg(unix)]
	fn region_create_replaces_stale_segment() {
		// Mirrors the C++: Create unlinks any stale segment with the same
		// name first (crash cleanup), so a second Create SUCCEEDS and owns
		// a fresh, zeroed segment.
		let key = test_key("region-exists");
		let size = 128usize;
		let (owner, _peer) = two_mappings(&key, size);
		assert!(owner.is_valid());
		// SAFETY: owner mapping live.
		unsafe { *(owner.data() as *mut u32) = 0xCAFEBABE };

		let mut second = SharedMemoryRegion::new();
		assert!(
			second.open(&key, size, ShmMode::Create),
			"{}",
			second.error()
		);
		assert!(second.is_valid());
		// The replacement segment is fresh (zeroed by create).
		// SAFETY: second mapping live.
		assert_eq!(unsafe { *(second.data() as *const u32) }, 0);
	}

	#[test]
	#[cfg(windows)]
	fn region_create_fails_on_live_existing_segment() {
		// Windows has no way to unlink a live section object (it dies only
		// when its last handle closes), so a second Create while the owner
		// still holds the object fails like O_EXCL — the reverse of the
		// POSIX stale-replace behavior above, which unlinks first.
		let key = test_key("region-exists");
		let size = 128usize;
		let (owner, _peer) = two_mappings(&key, size);
		assert!(owner.is_valid());
		// SAFETY: owner mapping live.
		unsafe { *(owner.data() as *mut u32) = 0xCAFEBABE };

		let mut second = SharedMemoryRegion::new();
		assert!(!second.open(&key, size, ShmMode::Create));
		assert!(!second.is_valid());
		assert!(!second.error().is_empty());
	}

	#[test]
	fn region_attach_fails_when_segment_too_small() {
		// macOS rounds shm segment sizes up to a 16 KiB minimum, so use
		// sizes above that to exercise the size check.
		let key = test_key("region-small");
		let (owner, _peer) = two_mappings(&key, 4096);
		assert!(owner.is_valid());

		// Attaching with a larger size than the segment must fail (the
		// fstat check, mirroring the C++).
		let mut big = SharedMemoryRegion::new();
		assert!(!big.open(&key, 65536, ShmMode::Attach));
		assert!(!big.is_valid());
		assert!(!big.error().is_empty());
	}

	#[test]
	fn region_make_key_format() {
		assert_eq!(SharedMemoryRegion::make_key(4242, 3), "olive-rw-4242-3");
		assert_eq!(SharedMemoryRegion::make_key(1, 0), "olive-rw-1-0");
	}

	#[test]
	fn region_shm_spike_512mb_segment() {
		// M15 S1 shm spike: one POSIX shm segment must ftruncate to at
		// least 512 MiB (the designed per-worker pool is 8 slots x ~8.3 MB
		// BGRA8 1080p ≈ 66 MB, but large F32 4K pools reach the hundreds
		// of MB). Verified on macOS: POSIX shm is NOT bounded by the SysV
		// shmmax sysctl; segments up to 1 GiB spiked fine, so no
		// temp-file+mmap fallback backend is needed. This test is the
		// regression gate; a failure here means the platform needs the
		// fallback backend inside `SharedMemoryRegion::open`.
		let key = test_key("spike-512mb");
		let size = 512usize * 1024 * 1024;
		let mut owner = SharedMemoryRegion::new();
		assert!(
			owner.open(&key, size, ShmMode::Create),
			"512 MiB shm segment must be creatable: {}",
			owner.error()
		);
		assert!(owner.is_valid());
		assert_eq!(owner.size(), size);
		// Touch the first and last byte of the mapping: an overcommitted
		// segment would SIGBUS here.
		// SAFETY: owner mapping is live with `size` bytes.
		unsafe {
			*owner.data() = 0xA5;
			*owner.data().add(size - 1) = 0x5A;
		}
		let mut peer = SharedMemoryRegion::new();
		assert!(
			peer.open(&key, size, ShmMode::Attach),
			"512 MiB shm segment must be attachable: {}",
			peer.error()
		);
		// SAFETY: peer mapping is live with `size` bytes.
		unsafe {
			assert_eq!(*peer.data(), 0xA5);
			assert_eq!(*peer.data().add(size - 1), 0x5A);
		}
		peer.close();
		owner.close();
	}

	#[test]
	fn region_keys_are_isolation_safe() {
		// Keys with slashes are flattened to a single-slash POSIX name.
		let key = "a/b/c";
		let size = 64usize;
		let (owner, peer) = two_mappings(key, size);
		assert!(owner.is_valid());
		assert!(peer.is_valid());
		// The actual POSIX name is "/a_b_c".
		// SAFETY: mapping live.
		unsafe { *(owner.data() as *mut u32) = 7 };
		// SAFETY: peer mapping live.
		assert_eq!(unsafe { *(peer.data() as *const u32) }, 7);
	}
}
