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

//! The mock engine: a gpui entity that implements every data-source trait
//! the widgets read, and feeds them demo data.
//!
//! # One entity, many roles
//!
//! [`MockEngine`] is the single source of truth for the demo project and
//! implements, all on the same entity:
//!
//! * [`EngineGateway`] — the app's transport/project seam (see the
//!   [module docs](super::engine));
//! * [`TimelineDataSource`] — the sequence model behind the timeline widget;
//! * [`EffectStackDataSource`] — the 媒体 → 变换 → OCIO LUT → 输出 stack;
//! * [`ProjectDataSource`] — the material bin tree;
//! * [`AudioMeterDataSource`] — the two program-master channels.
//!
//! Transport state lives in two [`MockClock`] entities (one per
//! [`Monitor`]); the engine owns them and advances them on every
//! [`EngineGateway::tick`].
//!
//! The real engine will later implement the same gateway over the
//! `liboakengine` C ABI; only the wiring in [`crate::app`] changes.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::effect_stack::{
    EffectCardKind, EffectData, EffectId, EffectStackDataSource, EffectStackEvent,
};
use gpui::node_graph::{
    EdgeData, EdgeId, NodeData, NodeGraphDataSource, NodeGraphEvent, NodeId, PortData,
    PortDataType, PortId, PortKind,
};
use gpui::timeline::{
    ClipData, ClipId, Frame, FrameRange, FrameRate, Marker, TimelineDataSource, TimelineEvent,
    TrackData, TrackKind, TrimEdge,
};
use gpui::{
    hsla, point, prelude::*, px, App, Context, Entity, Hsla, Pixels, Point, RenderImage,
    SharedString,
};
use gpui_widgets::audio_meter::AudioMeterDataSource;
use gpui_widgets::project_explorer::{ProjectDataSource, ProjectEntry};
use gpui_widgets::viewer::PlaybackClock;

use oak_core::Rational;
use oak_node::block::clip_input;
use oak_node::track::TrackType;
use oak_timeline::util::{block_clip_create, track_append_block};

use super::engine::{
    AppEngine, EngineGateway, ExportEvent, ExportSession, LibraryProject, Monitor, MulticamState,
    Project, ScopeData, Sequence, VideoFormat, WizardFootage, WizardSyncOffset,
};
use super::graphops;
use super::transport::TransportState;

/// The demo sequence length: 00:04:18:18 at 25 fps.
const SEQUENCE_LENGTH: i64 = 6468;

/// The current unix time in seconds (0 on clock failure).
fn now_unix() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs() as i64)
		.unwrap_or(0)
}

/// The demo library rows the project manager opens with (M13 D4): three
/// projects with plausible stats, most recently modified first.
fn demo_library() -> Vec<LibraryProject> {
	let now = now_unix();
	vec![
		LibraryProject {
			uuid: "mock-1".into(),
			name: "第一稿".into(),
			created_at: now - 86400,
			modified_at: now - 300,
			duration_ms: SEQUENCE_LENGTH * 1000 / 25,
			track_count: 4,
			clip_count: 5,
			footage_count: 3,
		},
		LibraryProject {
			uuid: "mock-2".into(),
			name: "宣传片 v3".into(),
			created_at: now - 3 * 86400,
			modified_at: now - 86400,
			duration_ms: 95_000,
			track_count: 6,
			clip_count: 14,
			footage_count: 8,
		},
		LibraryProject {
			uuid: "mock-3".into(),
			name: "采访粗剪".into(),
			created_at: now - 9 * 86400,
			modified_at: now - 7 * 86400,
			duration_ms: 612_000,
			track_count: 3,
			clip_count: 22,
			footage_count: 5,
		},
	]
}

// ---------------------------------------------------------------------------
// Clocks
// ---------------------------------------------------------------------------

/// A transport clock: the playhead plus the wall-clock anchor used while
/// playing. This is the object the viewer widgets poll through
/// [`PlaybackClock`].
pub struct MockClock {
	/// The transport state (play/pause, playhead, loop range).
	pub transport: TransportState,
	/// The clock's frame rate.
	pub rate: FrameRate,
	/// Wall-clock anchor `(started_at, anchored_frame)` while playing.
	started: Option<(Instant, Frame)>,
}

impl MockClock {
	/// A stopped clock at frame zero running at `rate`.
	pub fn new(rate: FrameRate) -> Self {
		Self {
			transport: TransportState::new(),
			rate,
			started: None,
		}
	}

	/// Starts playback from the current playhead.
	pub fn play(&mut self) {
		self.transport.play();
		self.started = Some((Instant::now(), self.transport.frame()));
	}

	/// Pauses playback, keeping the playhead.
	pub fn pause(&mut self) {
		self.transport.pause();
		self.started = None;
	}

	/// Advances the playhead from the wall clock while playing, looping at
	/// `length` — or pausing on the last frame when `stop_on_last` is set.
	/// No-op when stopped.
	pub fn tick(&mut self, length: Frame, stop_on_last: bool) {
		let Some((started, anchored)) = self.started else {
			return;
		};
		let elapsed = started.elapsed();
		let mut frame = anchored
			+ Frame(
				(elapsed.as_secs_f64() * self.rate.num as f64 / self.rate.den as f64).round()
					as i64,
			);
		if length.0 > 0 && frame.0 >= length.0 {
			if stop_on_last {
				// Stop on the last frame and pause playback (the C++ viewer's
				// Stop on Last option): the playhead never wraps.
				self.transport.pause();
				self.started = None;
				frame = Frame(length.0 - 1);
				self.transport.seek(frame, length);
				return;
			}
			// Loop back to the start of the sequence for the demo.
			frame = Frame(frame.0 % length.0);
		}
		self.transport.seek(frame, length);
	}
}

impl PlaybackClock for MockClock {
	fn current_frame(&self) -> Frame {
		self.transport.frame()
	}

	fn is_playing(&self) -> bool {
		self.transport.is_playing()
	}

	fn frame_rate(&self) -> FrameRate {
		self.rate
	}
}

// ---------------------------------------------------------------------------
// Timeline model
// ---------------------------------------------------------------------------

/// A recorded timeline drop of a footage entry (mock state; the mock has no
/// media pipeline, so it records the request and places a demo clip).
#[derive(Debug, Clone, PartialEq)]
pub struct MockFootageDrop {
	/// The dropped project-explorer entry id.
	pub id: u64,
	/// The track kind the clip landed on.
	pub track_kind: TrackKind,
	/// The display track index the clip landed on.
	pub track_index: usize,
	/// The clip's start frame.
	pub time: Frame,
}

/// A clip on the demo timeline.
#[derive(Debug, Clone)]
pub struct MockClip {
	id: ClipId,
	range: FrameRange,
	media_in: Frame,
	label: SharedString,
	color: Hsla,
}

impl ClipData for MockClip {
	fn id(&self) -> ClipId {
		self.id
	}

	fn range(&self) -> FrameRange {
		self.range
	}

	fn media_in(&self) -> Frame {
		self.media_in
	}

	fn label(&self) -> SharedString {
		self.label.clone()
	}

	fn color(&self) -> Option<Hsla> {
		Some(self.color)
	}
}

/// A track on the demo timeline (snapshot handed to the timeline widget).
#[derive(Debug, Clone)]
pub struct MockTrack {
	kind: TrackKind,
	name: SharedString,
	height: Pixels,
	locked: bool,
	muted: bool,
	solo: bool,
	visible: bool,
	clips: Vec<MockClip>,
}

impl TrackData for MockTrack {
	type Clip = MockClip;

	fn kind(&self) -> TrackKind {
		self.kind
	}

	fn name(&self) -> SharedString {
		self.name.clone()
	}

	fn is_locked(&self) -> bool {
		self.locked
	}

	fn is_muted(&self) -> bool {
		self.muted
	}

	fn is_solo(&self) -> bool {
		self.solo
	}

	fn is_visible(&self) -> bool {
		self.visible
	}

	fn height(&self) -> Pixels {
		self.height
	}

	fn clips(&self) -> &[Self::Clip] {
		&self.clips
	}
}

/// An effect card in the demo stack.
#[derive(Debug, Clone)]
pub struct MockEffect {
	id: EffectId,
	kind: EffectCardKind,
	title: SharedString,
	subtitle: Option<SharedString>,
	enabled: bool,
	expanded: bool,
	badge: Option<usize>,
}

impl EffectData for MockEffect {
	fn id(&self) -> EffectId {
		self.id
	}

	fn kind(&self) -> EffectCardKind {
		self.kind
	}

	fn title(&self) -> SharedString {
		self.title.clone()
	}

	fn subtitle(&self) -> Option<SharedString> {
		self.subtitle.clone()
	}

	fn is_enabled(&self) -> bool {
		self.enabled
	}

	fn is_expanded(&self) -> bool {
		self.expanded
	}

	fn badge_count(&self) -> Option<usize> {
		self.badge
	}
}

// ---------------------------------------------------------------------------
// Node graph model
// ---------------------------------------------------------------------------

/// A port on a mock node. `connected` is a model-side cache refreshed by
/// [`MockEngine::refresh_port_connectivity`] after every graph mutation.
#[derive(Debug, Clone)]
pub struct MockPort {
	id: PortId,
	kind: PortKind,
	label: SharedString,
	data_type: PortDataType,
	connected: bool,
}

impl PortData for MockPort {
	fn id(&self) -> PortId {
		self.id
	}

	fn kind(&self) -> PortKind {
		self.kind
	}

	fn label(&self) -> SharedString {
		self.label.clone()
	}

	fn data_type(&self) -> PortDataType {
		self.data_type.clone()
	}

	fn is_connected(&self) -> bool {
		self.connected
	}
}

/// A node in the mock graph (媒体 → 变换 → … → 输出).
#[derive(Debug, Clone)]
pub struct MockNode {
	id: NodeId,
	title: SharedString,
	position: Point<Pixels>,
	inputs: Vec<MockPort>,
	outputs: Vec<MockPort>,
	header_color: Option<Hsla>,
	enabled: bool,
	collapsed: bool,
}

impl NodeData for MockNode {
	type Port = MockPort;

	fn id(&self) -> NodeId {
		self.id
	}

	fn title(&self) -> SharedString {
		self.title.clone()
	}

	fn position(&self) -> Point<Pixels> {
		self.position
	}

	fn inputs(&self) -> Vec<Self::Port> {
		self.inputs.clone()
	}

	fn outputs(&self) -> Vec<Self::Port> {
		self.outputs.clone()
	}

	fn header_color(&self) -> Option<Hsla> {
		self.header_color
	}

	fn is_collapsed(&self) -> bool {
		self.collapsed
	}

	fn is_enabled(&self) -> bool {
		self.enabled
	}
}

/// An edge in the mock graph: a connection from an output port to an input
/// port.
#[derive(Debug, Clone)]
pub struct MockEdge {
	id: EdgeId,
	from_node: NodeId,
	from_port: PortId,
	to_node: NodeId,
	to_port: PortId,
}

impl EdgeData for MockEdge {
	fn id(&self) -> EdgeId {
		self.id
	}

	fn from_node(&self) -> NodeId {
		self.from_node
	}

	fn from_port(&self) -> PortId {
		self.from_port
	}

	fn to_node(&self) -> NodeId {
		self.to_node
	}

	fn to_port(&self) -> PortId {
		self.to_port
	}
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

/// The mock engine implementing every data-source trait over demo data.
pub struct MockEngine {
	project: Project,
	sequence: Sequence,
	/// The source monitor's clock.
	pub source_clock: Entity<MockClock>,
	/// The program monitor's clock.
	pub program_clock: Entity<MockClock>,
	/// The timeline tracks.
	tracks: Vec<MockTrack>,
	/// The effect stack cards.
	effects: Vec<MockEffect>,
	/// Id allocator for effects added at runtime.
	next_effect_id: u64,
	/// Whether the program monitor is playing (mirrors the clock; kept here
	/// because the audio-meter data source has no `App` to read the clock).
	program_playing: bool,
	/// The selected item in the material bin (demo state).
	selected_item: Option<u64>,
	/// Phase counter driving the demo audio levels.
	meter_phase: u32,
	/// The demo node graph shown in the node editor.
	nodes: Vec<MockNode>,
	/// The demo node-graph edges.
	edges: Vec<MockEdge>,
	/// Id allocator for edges added at runtime.
	next_edge_id: u64,
	/// Id allocator for nodes added at runtime (the node editor's Add menu).
	next_node_id: u64,
	/// Id allocator for the ports of nodes added at runtime.
	next_port_id: u64,
	/// The node selection, kept in sync with the node editor (and, later, the
	/// effect stack) so both views share one selection.
	node_selection: BTreeSet<NodeId>,
	/// Cache of the synthetic CPU frames handed to the viewers, keyed by
	/// monitor. Entries are the playhead frame that produced the image plus
	/// the scope samples analyzed in the same pass, so a paused viewer never
	/// regenerates its picture (or its scopes).
	cpu_frame_cache: Mutex<HashMap<Monitor, (i64, Arc<RenderImage>, ScopeData)>>,
	/// Paths handed to [`AppEngine::import_footage`] since creation; the mock
	/// has no media pipeline, so it just records them (drives app-level tests
	/// of the import flow).
	imported_footage: Vec<PathBuf>,
	/// Footage entries dropped onto the timeline via
	/// [`AppEngine::drop_footage`] since creation (mock state; each entry is
	/// the applied (id, track kind, track index, start frame) — drives
	/// app-level tests of the explorer→timeline drag).
	footage_drops: Vec<MockFootageDrop>,
	/// The fake project library the project manager browses (M13 D4): an
	/// in-memory row set the library trait methods operate on, so the app
	/// flow (list / open / create / rename / duplicate / delete / import /	/// export) is testable without a database.
	library: Vec<LibraryProject>,
	/// Id allocator for library rows created at runtime.
	next_library_id: u64,
	/// The uuids handed to [`AppEngine::library_open_project`] (test
	/// observability).
	library_opened: Vec<String>,
	/// (uuid, path) pairs handed to [`AppEngine::library_export_project`]
	/// (test observability).
	library_exported: Vec<(String, PathBuf)>,
	/// The demo sequence markers (M12 P4): shown on the timeline ruler and
	/// driven by the 序列 → 添加/清除标记 menu actions.
	markers: Vec<Marker>,
	/// The enabled work area (render/export in/out range) of the demo
	/// sequence, in sequence frames (M12 P4). `None` = disabled.
	workarea: Option<(Frame, Frame)>,
	/// Undo/redo call counts (test observability; the mock keeps no undo
	/// stack, so the counters are the only way to see the dispatch landed).
	undo_calls: u64,
	/// See [`MockEngine::undo_calls`].
	redo_calls: u64,
	/// Per-footage proxy lifecycle state (the demo proxy pipeline; ids are
	/// the explorer entry ids, `Missing` when absent).
	proxy_states: HashMap<u64, crate::oakui::engine::ProxyMediaState>,
	/// Per-footage proxy-use flag (the demo's per-footage switch).
	proxy_enabled: HashMap<u64, bool>,
	/// Per-footage custom proxy generation params (the proxy dialog's
	/// custom checkbox path).
	proxy_custom: HashMap<u64, crate::oakui::engine::ProxyParamsUi>,
	/// The demo's global "Use Proxy Media" switch.
	use_proxy: bool,
	/// The demo project's OCIO config override (the 项目属性 color tab).
	ocio_config: String,
	/// The demo project's disk-cache location (setting, custom path).
	cache_location: (i32, String),
	/// The demo clip link pairs (normalized `(min, max)` id pairs; the
	/// 链接/重新链接 toggle edits them).
	clip_links: std::collections::HashSet<(u64, u64)>,
	/// The demo multicam graph: a real oaknode project whose source
	/// sequence's video tracks are the angles. Created lazily so the demo
	/// panel shows a genuine graph behind its synthetic frames — and the
	/// switch / enable / disable commands run on the real command path
	/// (`oak_timeline::multicam` + the global undo stack).
	multicam_graph: Mutex<Option<DemoMulticamGraph>>,
	/// The demo multicam angle-frame cache: source → (playhead, image), so
	/// a paused cell never regenerates its picture.
	multicam_frames: Mutex<HashMap<i32, (i64, Arc<RenderImage>)>>,
}

impl MockEngine {
	/// Builds the demo project: 第一稿.ove, one HD sequence, four tracks.
	pub fn demo(cx: &mut Context<Self>) -> Self {
		let rate = VideoFormat::hd_1080p25().rate;
		let clip =
			|id: u64, start: i64, end: i64, media_in: i64, label: &str, color: Hsla| MockClip {
				id: ClipId(id),
				range: FrameRange::new(Frame(start), Frame(end)),
				media_in: Frame(media_in),
				label: label.into(),
				color,
			};
		// Clip colors follow the design's timeline: video and audio clips are
		// green bars (#48a26d), slightly lighter for audio. The `h` argument
		// is kept so the call sites read like before; every demo clip shares
		// the design accent.
		let video = |_h: f32| Hsla {
			h: 0.402,
			s: 0.385,
			l: 0.459,
			a: 1.0,
		};
		let audio = |_h: f32| Hsla {
			h: 0.402,
			s: 0.32,
			l: 0.54,
			a: 1.0,
		};
		let node_color = |h: f32| Hsla {
			h,
			s: 0.55,
			l: 0.5,
			a: 1.0,
		};

		// The demo node graph: two clips, one through the transform, one
		// through the blur, merged in the mixer and LUT'd to the viewer.
		// Port ids are globally unique.
		let video_type = PortDataType::new("video", hsla(0.55, 0.75, 0.6, 1.0));
		let audio_type = PortDataType::new("audio", hsla(0.1, 0.7, 0.55, 1.0));
		let port = |id: u64, kind: PortKind, label: &str, data_type: &PortDataType| MockPort {
			id: PortId(id),
			kind,
			label: label.into(),
			data_type: data_type.clone(),
			connected: false,
		};
		let node = |id: u64,
		            title: &str,
		            position: (f32, f32),
		            color: f32,
		            inputs: Vec<MockPort>,
		            outputs: Vec<MockPort>| MockNode {
			id: NodeId(id),
			title: title.into(),
			position: point(px(position.0), px(position.1)),
			inputs,
			outputs,
			header_color: Some(node_color(color)),
			enabled: true,
			collapsed: false,
		};
		let nodes = vec![
			node(
				0,
				"第一稿.mp4",
				(40.0, 40.0),
				0.55,
				vec![],
				vec![port(1, PortKind::Output, "video", &video_type)],
			),
			node(
				1,
				"B-roll.mp4",
				(40.0, 230.0),
				0.60,
				vec![],
				vec![
					port(3, PortKind::Output, "video", &video_type),
					port(4, PortKind::Output, "audio", &audio_type),
				],
			),
			node(
				2,
				"变换",
				(320.0, 40.0),
				0.35,
				vec![
					port(20, PortKind::Input, "in", &video_type),
					port(22, PortKind::Input, "mask", &video_type),
				],
				vec![port(21, PortKind::Output, "out", &video_type)],
			),
			node(
				3,
				"模糊",
				(320.0, 230.0),
				0.78,
				vec![port(30, PortKind::Input, "in", &video_type)],
				vec![port(31, PortKind::Output, "out", &video_type)],
			),
			node(
				4,
				"混合",
				(600.0, 130.0),
				0.08,
				vec![
					port(40, PortKind::Input, "A", &video_type),
					port(41, PortKind::Input, "B", &video_type),
				],
				vec![port(42, PortKind::Output, "out", &video_type)],
			),
			node(
				5,
				"输出",
				(880.0, 130.0),
				0.0,
				vec![port(50, PortKind::Input, "in", &video_type)],
				vec![],
			),
		];
		let edge = |id: u64, from_node: u64, from_port: u64, to_node: u64, to_port: u64| MockEdge {
			id: EdgeId(id),
			from_node: NodeId(from_node),
			from_port: PortId(from_port),
			to_node: NodeId(to_node),
			to_port: PortId(to_port),
		};
		let edges = vec![
			edge(1, 0, 1, 2, 20),
			edge(2, 1, 3, 3, 30),
			edge(3, 2, 21, 4, 40),
			edge(4, 3, 31, 4, 41),
			edge(5, 4, 42, 5, 50),
		];

		let mut this = Self {
			project: Project {
				name: "第一稿".into(),
				path: PathBuf::from("/home/mikesolar/Videos/aaa.ove"),
			},
			sequence: Sequence {
				name: "第一稿".into(),
				format: VideoFormat::hd_1080p25(),
				length: Frame(SEQUENCE_LENGTH),
			},
			source_clock: cx.new(|_cx| MockClock::new(rate)),
			program_clock: cx.new(|_cx| MockClock::new(rate)),
			tracks: vec![
				MockTrack {
					kind: TrackKind::Video,
					name: "V2 视频轨道1".into(),
					height: px(64.0),
					locked: false,
					muted: false,
					solo: false,
					visible: true,
					clips: vec![clip(10, 120, 300, 0, "标题.mov", video(0.55))],
				},
				MockTrack {
					kind: TrackKind::Video,
					name: "V1 视频轨道0".into(),
					height: px(64.0),
					locked: false,
					muted: false,
					solo: false,
					visible: true,
					clips: vec![
						clip(11, 0, 240, 0, "开场.mov", video(0.60)),
						clip(12, 240, 600, 100, "B-roll.mp4", video(0.50)),
					],
				},
				MockTrack {
					kind: TrackKind::Audio,
					name: "A1 音频轨道0".into(),
					height: px(48.0),
					locked: false,
					muted: false,
					solo: false,
					visible: true,
					clips: vec![clip(13, 0, 600, 0, "对白.wav", audio(0.05))],
				},
				MockTrack {
					kind: TrackKind::Audio,
					name: "A2 音频轨道1".into(),
					height: px(48.0),
					locked: false,
					muted: false,
					solo: false,
					visible: true,
					clips: vec![clip(14, 0, 480, 0, "配乐.flac", audio(0.62))],
				},
			],
			effects: vec![
				MockEffect {
					id: EffectId(0),
					kind: EffectCardKind::Source,
					title: "媒体".into(),
					subtitle: Some("第一稿.mp4".into()),
					enabled: true,
					expanded: false,
					badge: None,
				},
				MockEffect {
					id: EffectId(1),
					kind: EffectCardKind::Effect,
					title: "变换".into(),
					subtitle: Some("缩放 100% · 旋转 0°".into()),
					enabled: true,
					expanded: true,
					badge: Some(2),
				},
				MockEffect {
					id: EffectId(2),
					kind: EffectCardKind::Effect,
					title: "OCIO LUT".into(),
					subtitle: Some("filmic_to_display.cube".into()),
					enabled: true,
					expanded: false,
					badge: None,
				},
				MockEffect {
					id: EffectId(3),
					kind: EffectCardKind::Output,
					title: "输出".into(),
					subtitle: None,
					enabled: true,
					expanded: false,
					badge: None,
				},
			],
			next_effect_id: 4,
			program_playing: false,
			selected_item: None,
			meter_phase: 0,
			nodes,
			edges,
			next_edge_id: 6,
			next_node_id: 100,
			next_port_id: 1000,
			node_selection: BTreeSet::new(),
			cpu_frame_cache: Mutex::new(HashMap::new()),
			imported_footage: Vec::new(),
			footage_drops: Vec::new(),
			library: demo_library(),
			next_library_id: 100,
			library_opened: Vec::new(),
			library_exported: Vec::new(),
			markers: Vec::new(),
			workarea: None,
			undo_calls: 0,
			redo_calls: 0,
			proxy_states: HashMap::new(),
			proxy_enabled: HashMap::new(),
			proxy_custom: HashMap::new(),
			use_proxy: true,
			ocio_config: String::new(),
			cache_location: (0, String::new()),
			clip_links: std::collections::HashSet::new(),
			multicam_graph: Mutex::new(None),
			multicam_frames: Mutex::new(HashMap::new()),
		};
		// The demo graph is born connected: derive every port's `connected`
		// flag from the edge list.
		this.refresh_port_connectivity();
		this
	}

	/// The current sequence length (also used by the gateway).
	fn sequence_length(&self) -> Frame {
		self.sequence.length
	}

	/// Resolves the clock entity for a monitor.
	fn clock(&self, monitor: Monitor) -> &Entity<MockClock> {
		match monitor {
			Monitor::Source => &self.source_clock,
			Monitor::Program => &self.program_clock,
		}
	}

	/// Applies an edit request from the effect stack to the model.
	pub fn apply_effect_event(&mut self, event: &EffectStackEvent, cx: &mut Context<Self>) {
		match event {
			EffectStackEvent::EnableToggled { effect, enabled } => {
				if let Some(effect) = self.effects.iter_mut().find(|e| e.id == *effect) {
					effect.enabled = *enabled;
				}
			}
			EffectStackEvent::ExpansionToggled { effect, expanded } => {
				if let Some(effect) = self.effects.iter_mut().find(|e| e.id == *effect) {
					effect.expanded = *expanded;
				}
			}
			EffectStackEvent::RemoveRequested(id) => {
				if let Some(index) = self
					.effects
					.iter()
					.position(|e| e.id == *id && e.is_removable())
				{
					self.effects.remove(index);
				}
			}
			EffectStackEvent::ReorderRequested { effect, new_index } => {
				let Some(from) = self.effects.iter().position(|e| e.id == *effect) else {
					return;
				};
				// `new_index` is an insertion index in the post-removal list.
				let card = self.effects.remove(from);
				let to = (*new_index).min(self.effects.len());
				self.effects.insert(to, card);
			}
			EffectStackEvent::AddRequested { index } => {
				let id = EffectId(self.next_effect_id);
				self.next_effect_id += 1;
				let card = MockEffect {
					id,
					kind: EffectCardKind::Effect,
					title: "新效果".into(),
					subtitle: None,
					enabled: true,
					expanded: false,
					badge: None,
				};
				let index = (*index).min(self.effects.len());
				self.effects.insert(index, card);
			}
			// A drag-and-drop add from the effect library: same insert, with
			// the dropped type's name on the card.
			EffectStackEvent::AddTypeRequested { index, type_id } => {
				let id = EffectId(self.next_effect_id);
				self.next_effect_id += 1;
				let card = MockEffect {
					id,
					kind: EffectCardKind::Effect,
					title: type_id.clone(),
					subtitle: None,
					enabled: true,
					expanded: false,
					badge: None,
				};
				let index = (*index).min(self.effects.len());
				self.effects.insert(index, card);
			}
			EffectStackEvent::CardSelected { effect } => {
				// The inspector card click highlights the matching node in
				// the node editor (the bidirectional node↔inspector link).
				// The demo links cards and nodes by shared title.
				if let Some(node) = self.node_for_effect(*effect) {
					self.node_selection = BTreeSet::from([node]);
				}
			}
			// The app owns the context menu; the mock ignores it.
			EffectStackEvent::ContextMenuRequested { .. }
			| EffectStackEvent::ParameterChanged { .. } => {}
		}
		cx.notify();
	}

	/// Applies a node-editor request to the mock graph (the "edits are
	/// requests" loop: the view emits, the engine applies and notifies).
	pub fn apply_node_graph_event(&mut self, event: &NodeGraphEvent, cx: &mut Context<Self>) {
		match event {
			NodeGraphEvent::NodeMovePreview { .. }
			| NodeGraphEvent::ViewChanged { .. }
			| NodeGraphEvent::NodeContextMenuRequested { .. } => {}
			NodeGraphEvent::NodeMoveRequested { nodes, delta } => {
				for id in nodes {
					if let Some(node) = self.nodes.iter_mut().find(|n| n.id() == *id) {
						node.position = node.position + *delta;
					}
				}
			}
			NodeGraphEvent::ConnectionRequested { from, to } => {
				if self.can_connect(*from, *to) {
					let from_node = self.node_with_port(*from).map(|n| n.id());
					let to_node = self.node_with_port(*to).map(|n| n.id());
					if let (Some(from_node), Some(to_node)) = (from_node, to_node) {
						self.edges.push(MockEdge {
							id: EdgeId(self.next_edge_id),
							from_node,
							from_port: *from,
							to_node,
							to_port: *to,
						});
						self.next_edge_id += 1;
						self.refresh_port_connectivity();
					}
				}
			}
			NodeGraphEvent::DisconnectionRequested { edge } => {
				if let Some(index) = self.edges.iter().position(|e| e.id() == *edge) {
					self.edges.remove(index);
					self.refresh_port_connectivity();
				}
			}
			NodeGraphEvent::DeleteRequested { nodes, edges } => {
				self.edges.retain(|edge| {
					!edges.contains(&edge.id())
						&& !nodes.contains(&edge.from_node())
						&& !nodes.contains(&edge.to_node())
				});
				self.nodes.retain(|node| !nodes.contains(&node.id()));
				self.node_selection.clear();
				self.refresh_port_connectivity();
			}
			NodeGraphEvent::SelectionChanged { nodes } => {
				// Only auto-expand when the selection actually CHANGED. The
				// card-header click selects the card's node (CardSelected →
				// node_selection) and the panel mirrors that into the graph
				// widget, whose SelectionChanged echo must not fight the
				// expansion toggle the same click requested.
				let changed = self.node_selection != *nodes;
				self.node_selection = nodes.clone();
				if changed && nodes.len() == 1 {
					let node = *nodes.iter().next().expect("a one-element set");
					if let Some(effect) = self.effect_for_node(node) {
						if let Some(card) = self.effects.iter_mut().find(|e| e.id == effect) {
							card.expanded = true;
						}
					}
				}
			}
			NodeGraphEvent::BackgroundClicked { position } => {
				// The real app opens an "add node" menu here; the mock logs it.
				println!("[node editor] add-node menu at graph {position:?} (mock: ignored)");
			}
		}
		cx.notify();
	}

	/// The node currently under the cursor selection (demo state; the effect
	/// stack will sync to this once it drives the same selection).
	pub fn selected_node_ids(&self) -> &BTreeSet<NodeId> {
		&self.node_selection
	}

	/// The demo node↔effect-card link: a card and a graph node share a
	/// selection when their titles match (the demo graph's "变换" / "输出"
	/// cards map 1:1 to the same-named graph nodes). The inspector card
	/// click and the node-graph click route through this mapping.
	fn node_for_effect(&self, effect: EffectId) -> Option<NodeId> {
		let title = self.effects.iter().find(|e| e.id == effect)?.title.clone();
		self.nodes
			.iter()
			.find(|n| n.title() == title)
			.map(|n| n.id())
	}

	/// The inverse of [`node_for_effect`](Self::node_for_effect): the card
	/// matching a selected graph node.
	fn effect_for_node(&self, node: NodeId) -> Option<EffectId> {
		let title = self.nodes.iter().find(|n| n.id() == node)?.title();
		self.effects.iter().find(|e| e.title == title).map(|e| e.id)
	}

	/// Looks up a node by id (test helper).
	#[cfg(test)]
	fn node(&self, id: NodeId) -> Option<&MockNode> {
		self.nodes.iter().find(|n| n.id() == id)
	}

	/// Looks up a port by its globally unique id.
	fn port(&self, id: PortId) -> Option<&MockPort> {
		self.nodes
			.iter()
			.flat_map(|n| n.inputs.iter().chain(n.outputs.iter()))
			.find(|p| p.id() == id)
	}

	/// Returns the node that owns `port`.
	fn node_with_port(&self, port: PortId) -> Option<&MockNode> {
		self.nodes.iter().find(|n| {
			n.inputs.iter().any(|p| p.id() == port) || n.outputs.iter().any(|p| p.id() == port)
		})
	}

	/// Whether an edge with the same endpoints already exists.
	fn edge_exists(&self, from: PortId, to: PortId) -> bool {
		self.edges
			.iter()
			.any(|e| e.from_port() == from && e.to_port() == to)
	}

	/// Whether there is a directed path from node `from` to node `to`
	/// following the existing edges (DFS). `from == to` counts as a path.
	fn reaches(&self, from: NodeId, to: NodeId) -> bool {
		if from == to {
			return true;
		}
		let mut visited = std::collections::HashSet::new();
		let mut stack = vec![from];
		while let Some(current) = stack.pop() {
			if !visited.insert(current) {
				continue;
			}
			for edge in &self.edges {
				if edge.from_node() == current {
					let next = edge.to_node();
					if next == to {
						return true;
					}
					stack.push(next);
				}
			}
		}
		false
	}

	/// Recomputed every port's `connected` flag from the current edge list.
	fn refresh_port_connectivity(&mut self) {
		for node in &mut self.nodes {
			for port in node.inputs.iter_mut().chain(node.outputs.iter_mut()) {
				port.connected = self
					.edges
					.iter()
					.any(|e| e.from_port() == port.id || e.to_port() == port.id);
			}
		}
	}

	/// Sets the row height of every timeline track (demo toolbar).
	pub fn set_track_height(&mut self, height: Pixels, cx: &mut Context<Self>) {
		for track in &mut self.tracks {
			track.height = height;
		}
		cx.notify();
	}

	/// Adds a new empty track of the given kind (demo "序列" menu).
	pub fn add_track(&mut self, kind: TrackKind, cx: &mut Context<Self>) {
		let index = self.tracks.len();
		let (name, height) = match kind {
			TrackKind::Video => (format!("V{} 视频轨道{}", index / 2 + 1, index), px(64.0)),
			TrackKind::Audio => (format!("A{} 音频轨道{}", index / 2 + 1, index), px(48.0)),
			TrackKind::Subtitle => (format!("S{} 字幕轨道", index + 1), px(32.0)),
		};
		self.tracks.push(MockTrack {
			kind,
			name: name.into(),
			height,
			locked: false,
			muted: false,
			solo: false,
			visible: true,
			clips: Vec::new(),
		});
		cx.notify();
	}

	/// Finds the (track, clip) position of `clip` in the display list.
	fn mock_clip_position(&self, clip: ClipId) -> Option<(usize, usize)> {
		self.tracks
			.iter()
			.enumerate()
			.find_map(|(track_index, track)| {
				track
					.clips
					.iter()
					.position(|c| c.id() == clip)
					.map(|clip_index| (track_index, clip_index))
			})
	}

	/// Demo synchronization: moves every selected clip so its in point
	/// lines up with the earliest selected in point (the real engine's
	/// source-timecode alignment, approximated on the mock's flat track
	/// list). Locked tracks are skipped; clips keep their length.
	fn mock_sync_clips(&mut self, clips: &[ClipId]) {
		let mut positions: Vec<(usize, usize, i64, i64)> = Vec::new();
		for clip in clips {
			if let Some((track, index)) = self.mock_clip_position(*clip) {
				if self.tracks[track].locked {
					continue;
				}
				let range = self.tracks[track].clips[index].range;
				positions.push((track, index, range.start.0, range.end.0));
			}
		}
		if positions.len() < 2 {
			return;
		}
		let anchor = positions.iter().map(|p| p.2).min().unwrap_or(0);
		for (track, index, start, end) in positions {
			if start == anchor {
				continue;
			}
			let length = end - start;
			self.tracks[track].clips[index].range =
				FrameRange::new(Frame(anchor), Frame(anchor + length));
		}
	}

	/// A clip id larger than every existing one (for splits).
	fn next_mock_clip_id(&self) -> u64 {
		self.tracks
			.iter()
			.flat_map(|t| t.clips.iter())
			.map(|c| c.id().0)
			.max()
			.map(|max| max + 1)
			.unwrap_or(1)
	}

	/// Whether two demo clips are linked (the 链接/重新链接 toggle's state;
	/// the app tests assert through this).
	pub fn clips_linked(&self, a: u64, b: u64) -> bool {
		let pair = if a < b { (a, b) } else { (b, a) };
		self.clip_links.contains(&pair)
	}

	/// Splits the clip at (track, clip) position into two at `time`
	/// (mock-apply, not undoable).
	fn split_mock_clip(&mut self, track: usize, index: usize, time: Frame) {
		let source = &self.tracks[track].clips[index];
		if time.0 <= source.range.start.0 || time.0 >= source.range.end.0 {
			return;
		}
		let new_id = ClipId(self.next_mock_clip_id());
		let second = MockClip {
			id: new_id,
			range: FrameRange::new(time, source.range.end),
			media_in: Frame(source.media_in.0 + (time.0 - source.range.start.0)),
			label: source.label.clone(),
			color: source.color,
		};
		let mut first = self.tracks[track].clips[index].clone();
		first.range = FrameRange::new(source.range.start, time);
		self.tracks[track].clips[index] = first;
		self.tracks[track].clips.insert(index + 1, second);
	}

	/// The demo audio levels: animated while the program monitor plays.
	fn meter_levels(&self) -> Vec<f32> {
		if !self.program_playing {
			// Idle rest level: low but visible, so the program viewer's level
			// strip reads as alive (the design shows a lit green strip).
			return vec![0.42, 0.35];
		}
		let t = (self.meter_phase % 120) as f32 / 120.0 * std::f32::consts::TAU;
		let level = 0.25 + 0.55 * (t.sin() * 0.6 + (2.0 * t).sin() * 0.4).abs();
		vec![level, level * 0.8]
	}
}

// ---------------------------------------------------------------------------
// EngineGateway
// ---------------------------------------------------------------------------

impl EngineGateway for MockEngine {
	fn project(&self) -> Option<&Project> {
		Some(&self.project)
	}

	fn current_sequence(&self) -> Option<&Sequence> {
		Some(&self.sequence)
	}

	fn source_media_name(&self) -> String {
		// The demo "source" media shown in the source viewer: the first
		// footage item of the mock project.
		"第一稿.mp4".into()
	}

	fn open_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
		self.project.path = path;
		if let Some(name) = self.project.path.file_stem() {
			self.project.name = name.to_string_lossy().into_owned();
		}
		cx.notify();
	}

	fn request_frame(&mut self, monitor: Monitor, frame: Frame, cx: &mut Context<Self>) {
		let length = self.sequence_length();
		let clock = self.clock(monitor).clone();
		clock.update(cx, |clock, cx| {
			clock.transport.seek(frame, length);
			// Re-anchor so resuming continues from the new position.
			if clock.transport.is_playing() {
				clock.play();
			}
			cx.notify();
		});
		cx.notify();
	}

	fn play(&mut self, monitor: Monitor, cx: &mut Context<Self>) {
		if monitor == Monitor::Program {
			self.program_playing = true;
		}
		let clock = self.clock(monitor).clone();
		clock.update(cx, |clock, cx| {
			clock.play();
			cx.notify();
		});
		cx.notify();
	}

	fn pause(&mut self, monitor: Monitor, cx: &mut Context<Self>) {
		if monitor == Monitor::Program {
			self.program_playing = false;
		}
		let clock = self.clock(monitor).clone();
		clock.update(cx, |clock, cx| {
			clock.pause();
			cx.notify();
		});
		cx.notify();
	}

	fn step(&mut self, monitor: Monitor, delta: i64, cx: &mut Context<Self>) {
		let length = self.sequence_length();
		let clock = self.clock(monitor).clone();
		clock.update(cx, |clock, cx| {
			clock.transport.step(delta, length);
			if clock.transport.is_playing() {
				clock.play();
			}
			cx.notify();
		});
		cx.notify();
	}

	fn tick(&mut self, cx: &mut Context<Self>) {
		let length = self.sequence_length();
		let stop_on_last = self.stop_on_last();
		for clock in [&self.source_clock, &self.program_clock] {
			let clock = clock.clone();
			clock.update(cx, |clock, cx| {
				clock.tick(length, stop_on_last);
				cx.notify();
			});
		}
		self.meter_phase = self.meter_phase.wrapping_add(1);
		cx.notify();
	}
}

// ---------------------------------------------------------------------------
// AppEngine
// ---------------------------------------------------------------------------

impl AppEngine for MockEngine {
	type Clock = MockClock;

	fn create(cx: &mut Context<Self>) -> Self {
		Self::demo(cx)
	}

	fn source_clock(&self) -> &Entity<Self::Clock> {
		&self.source_clock
	}

	fn program_clock(&self) -> &Entity<Self::Clock> {
		&self.program_clock
	}

	fn clock_frame(&self, monitor: Monitor, cx: &App) -> Frame {
		self.clock_frame(monitor, cx)
	}

	fn cpu_frame(&self, monitor: Monitor, cx: &App) -> Arc<RenderImage> {
		self.cpu_frame(monitor, cx)
	}

	fn scope_data(&self, monitor: Monitor, cx: &App) -> ScopeData {
		self.scope_data(monitor, cx)
	}

	fn add_track(&mut self, kind: TrackKind, cx: &mut Context<Self>) {
		self.add_track(kind, cx);
	}

	fn remove_track(&mut self, index: usize, cx: &mut Context<Self>) {
		if index < self.tracks.len() {
			self.tracks.remove(index);
		}
		cx.notify();
	}

	fn set_track_height(&mut self, height: Pixels, cx: &mut Context<Self>) {
		self.set_track_height(height, cx);
	}

	fn select_item(&mut self, id: u64, cx: &mut Context<Self>) {
		self.select_item(id, cx);
	}

	fn apply_effect_event(&mut self, event: &EffectStackEvent, cx: &mut Context<Self>) {
		self.apply_effect_event(event, cx);
	}

	fn selected_graph_node(&self) -> Option<u64> {
		if self.node_selection.len() != 1 {
			return None;
		}
		let node = *self.node_selection.iter().next()?;
		Some(node.0)
	}

	fn addable_effects(&self) -> Vec<crate::oakui::engine::EffectEntry> {
		// The demo list is the real factory's effect table (built-ins plus
		// any registered OpenFX plugins), so the effect library shows the
		// same entries the real engine would.
		crate::oakui::effectchain::addable_effects()
	}

	fn add_effect(
		&mut self,
		index: usize,
		type_id: &str,
		cx: &mut Context<Self>,
	) -> Result<(), String> {
		let Some((_, name)) = crate::oakui::effectchain::addable_effects()
			.into_iter()
			.find(|entry| entry.type_id == type_id)
			.map(|entry| (entry.type_id, entry.name))
		else {
			return Err(format!("unknown effect \"{type_id}\""));
		};
		let id = EffectId(self.next_effect_id);
		self.next_effect_id += 1;
		let card = MockEffect {
			id,
			kind: EffectCardKind::Effect,
			title: name.into(),
			subtitle: None,
			enabled: true,
			expanded: false,
			badge: None,
		};
		let index = index.min(self.effects.len());
		self.effects.insert(index, card);
		cx.notify();
		Ok(())
	}

	fn add_node_at(
		&mut self,
		type_id: &str,
		position: Point<Pixels>,
		cx: &mut Context<Self>,
	) -> Result<(), String> {
		// The demo graph is display-only; a created node gets one video
		// input and one video output, titled after the factory entry.
		let library = self.node_library();
		let Some(entry) = library.iter().find(|entry| entry.type_id == type_id) else {
			return Err(format!("unknown node type \"{type_id}\""));
		};
		let video_type = PortDataType::new("video", hsla(0.55, 0.75, 0.6, 1.0));
		let in_id = PortId(self.next_port_id);
		let out_id = PortId(self.next_port_id + 1);
		self.next_port_id += 2;
		let node = MockNode {
			id: NodeId(self.next_node_id),
			title: entry.name.clone().into(),
			position,
			inputs: vec![MockPort {
				id: in_id,
				kind: PortKind::Input,
				label: "in".into(),
				data_type: video_type.clone(),
				connected: false,
			}],
			outputs: vec![MockPort {
				id: out_id,
				kind: PortKind::Output,
				label: "out".into(),
				data_type: video_type,
				connected: false,
			}],
			header_color: None,
			enabled: true,
			collapsed: false,
		};
		self.next_node_id += 1;
		self.nodes.push(node);
		cx.notify();
		Ok(())
	}

	fn apply_node_graph_event(&mut self, event: &NodeGraphEvent, cx: &mut Context<Self>) {
		self.apply_node_graph_event(event, cx);
	}

	fn apply_timeline_event(&mut self, event: &TimelineEvent, cx: &mut Context<Self>) {
		match event {
			TimelineEvent::PlayheadChanged(frame) => {
				let current = self.clock_frame(Monitor::Program, cx);
				if *frame != current {
					self.request_frame(Monitor::Program, *frame, cx);
				}
			}
			TimelineEvent::ClipTrimRequested {
				clip,
				edge,
				new_frame,
			} => {
				if let Some((track, index)) = self.mock_clip_position(*clip) {
					if self.tracks[track].locked {
						return;
					}
					let clip = &mut self.tracks[track].clips[index];
					match edge {
						gpui::timeline::TrimEdge::Start => {
							let delta = new_frame.0 - clip.range.start.0;
							clip.range.start = *new_frame;
							clip.media_in = Frame(clip.media_in.0 + delta);
						}
						gpui::timeline::TrimEdge::End => {
							clip.range.end = *new_frame;
						}
					}
				}
				cx.notify();
			}
			TimelineEvent::ClipSplitRequested { clip, time } => {
				self.split_clip(*clip, *time, cx);
			}
			TimelineEvent::ClipRippleTrimRequested {
				clip,
				edge,
				new_frame,
			} => {
				if let Some((track, index)) = self.mock_clip_position(*clip) {
					if self.tracks[track].locked {
						return;
					}
					// Trim the clip itself (same as a plain trim), then shift
					// every later clip on the track by the same delta — the
					// mock's stand-in for the real ripple-close of the gap.
					let delta = {
						let clip = &self.tracks[track].clips[index];
						match edge {
							gpui::timeline::TrimEdge::Start => new_frame.0 - clip.range.start.0,
							gpui::timeline::TrimEdge::End => new_frame.0 - clip.range.end.0,
						}
					};
					{
						let clip = &mut self.tracks[track].clips[index];
						match edge {
							gpui::timeline::TrimEdge::Start => {
								clip.range.start = *new_frame;
								clip.media_in = Frame(clip.media_in.0 + delta);
							}
							gpui::timeline::TrimEdge::End => {
								clip.range.end = *new_frame;
							}
						}
					}
					for later in self.tracks[track].clips.iter_mut().skip(index + 1) {
						later.range = FrameRange::new(
							Frame(later.range.start.0 + delta),
							Frame(later.range.end.0 + delta),
						);
					}
				}
				cx.notify();
			}
			TimelineEvent::ClipRollRequested {
				clip_a,
				clip_b,
				new_frame,
			} => {
				let (Some((ta, ia)), Some((tb, ib))) = (
					self.mock_clip_position(*clip_a),
					self.mock_clip_position(*clip_b),
				) else {
					return;
				};
				if ta != tb || self.tracks[ta].locked {
					return;
				}
				// The boundary must stay inside the two clips' combined span.
				let (a_start, b_end) = {
					let a = &self.tracks[ta].clips[ia];
					let b = &self.tracks[ta].clips[ib];
					(a.range.start.0, b.range.end.0)
				};
				if new_frame.0 <= a_start || new_frame.0 >= b_end {
					return;
				}
				let (a_idx, b_idx) = if ia < ib { (ia, ib) } else { (ib, ia) };
				self.tracks[ta].clips[a_idx].range.end = *new_frame;
				self.tracks[ta].clips[b_idx].range.start = *new_frame;
				cx.notify();
			}
			TimelineEvent::ClipSlideRequested { clip, new_start } => {
				if let Some((track, index)) = self.mock_clip_position(*clip) {
					if self.tracks[track].locked {
						return;
					}
					// The clip slides in time; its media offset is unchanged.
					let length = {
						let clip = &self.tracks[track].clips[index];
						clip.range.end.0 - clip.range.start.0
					};
					let clip = &mut self.tracks[track].clips[index];
					clip.range = FrameRange::new(*new_start, Frame(new_start.0 + length));
				}
				cx.notify();
			}
			TimelineEvent::ClipSlipRequested { clip, new_media_in } => {
				if let Some((track, index)) = self.mock_clip_position(*clip) {
					if self.tracks[track].locked {
						return;
					}
					// The clip's position in time is fixed; only the media
					// offset under it changes.
					self.tracks[track].clips[index].media_in = *new_media_in;
				}
				cx.notify();
			}
			TimelineEvent::ClipMoveRequested {
				clip,
				new_track,
				new_start,
			} => {
				let Some((track, index)) = self.mock_clip_position(*clip) else {
					return;
				};
				if self.tracks[track].locked
					|| self
						.tracks
						.get(*new_track)
						.map(|t| t.locked)
						.unwrap_or(true)
				{
					return;
				}
				let mut clip = self.tracks[track].clips.remove(index);
				let length = clip.range.end.0 - clip.range.start.0;
				clip.range = FrameRange::new(*new_start, Frame(new_start.0 + length));
				if *new_track < self.tracks.len() {
					self.tracks[*new_track].clips.push(clip);
				}
				cx.notify();
			}
			TimelineEvent::TrackHeightChanged { track, height } => {
				if let Some(track) = self.tracks.get_mut(*track) {
					track.height = *height;
				}
				cx.notify();
			}
			TimelineEvent::SelectionChanged
			| TimelineEvent::TrackSelected { .. }
			| TimelineEvent::TransitionChanged { .. }
			| TimelineEvent::ContextMenuRequested { .. }
			| TimelineEvent::ZoomChanged(_) => {}
			TimelineEvent::TrackToggleRequested { track, toggle } => {
				// The demo model applies the toggles directly (no undo in
				// mock mode); the muted flag doubles as video visibility,
				// mirroring the real engine.
				let Some(track) = self.tracks.get_mut(*track) else {
					return;
				};
				match toggle {
					gpui::timeline::TrackHeaderEvent::ToggleLock => {
						track.locked = !track.locked;
					}
					gpui::timeline::TrackHeaderEvent::ToggleMute => {
						track.muted = !track.muted;
					}
					gpui::timeline::TrackHeaderEvent::ToggleSolo => {
						track.solo = !track.solo;
					}
					gpui::timeline::TrackHeaderEvent::ToggleVisibility => {
						track.visible = !track.visible;
						track.muted = !track.visible;
					}
				}
				cx.notify();
			}
			TimelineEvent::WorkAreaPreview { start, end } => {
				self.set_workarea_preview(*start, *end, cx);
			}
			TimelineEvent::WorkAreaCommitted {
				start,
				end,
				old_start,
				old_end,
			} => {
				self.commit_workarea(*old_start, *old_end, *start, *end, cx);
			}
		}
	}

	fn split_clip(&mut self, clip: ClipId, time: Frame, cx: &mut Context<Self>) {
		let Some((track, index)) = self.mock_clip_position(clip) else {
			return;
		};
		if self.tracks[track].locked {
			return;
		}
		self.split_mock_clip(track, index, time);
		cx.notify();
	}

	fn split_at_playhead(&mut self, cx: &mut Context<Self>) {
		let frame = self.clock_frame(Monitor::Program, cx);
		let targets: Vec<(usize, usize)> = self
			.tracks
			.iter()
			.enumerate()
			.filter(|(_, track)| !track.locked)
			.flat_map(|(track_index, track)| {
				track
					.clips
					.iter()
					.enumerate()
					.filter(|(_, clip)| clip.range.start.0 < frame.0 && frame.0 < clip.range.end.0)
					.map(|(clip_index, _)| (track_index, clip_index))
					.collect::<Vec<_>>()
			})
			.collect();
		for (track, index) in targets {
			self.split_mock_clip(track, index, frame);
		}
		cx.notify();
	}

	fn delete_clip(&mut self, clip: ClipId, ripple: bool, cx: &mut Context<Self>) {
		let Some((track, index)) = self.mock_clip_position(clip) else {
			return;
		};
		if self.tracks[track].locked {
			return;
		}
		let removed = self.tracks[track].clips.remove(index);
		if ripple {
			// Shift the following clips on the same track left by the removed
			// clip's length (the mock ripples one track, not the whole
			// sequence).
			let shift = removed.range.end.0 - removed.range.start.0;
			for later in &mut self.tracks[track].clips[index..] {
				later.range = FrameRange::new(
					Frame(later.range.start.0 - shift),
					Frame(later.range.end.0 - shift),
				);
			}
		}
		cx.notify();
	}

	fn can_undo(&self) -> bool {
		// The mock keeps no undo stack; the real engine's facade stack drives
		// the Edit menu in real mode.
		false
	}

	fn can_redo(&self) -> bool {
		false
	}

	fn undo(&mut self, cx: &mut Context<Self>) {
		println!("[mock engine] undo: no undo stack in mock mode");
		self.undo_calls += 1;
		cx.notify();
	}

	fn redo(&mut self, cx: &mut Context<Self>) {
		println!("[mock engine] redo: no undo stack in mock mode");
		self.redo_calls += 1;
		cx.notify();
	}

	fn workarea(&self) -> Option<(Frame, Frame)> {
		self.workarea
	}

	fn add_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
		let frame = self.clock_frame(Monitor::Program, cx);
		if !self.markers.iter().any(|m| m.frame == frame) {
			self.markers.push(Marker {
				frame,
				label: SharedString::new_static(""),
				color: None,
			});
		}
		cx.notify();
	}

	fn remove_marker_at_playhead(&mut self, cx: &mut Context<Self>) {
		let frame = self.clock_frame(Monitor::Program, cx);
		self.markers.retain(|m| m.frame != frame);
		cx.notify();
	}

	fn set_workarea_preview(&mut self, start: Frame, end: Frame, cx: &mut Context<Self>) {
		self.workarea = Some((start, end));
		cx.notify();
	}

	fn commit_workarea(
		&mut self,
		_old_start: Frame,
		_old_end: Frame,
		start: Frame,
		end: Frame,
		cx: &mut Context<Self>,
	) {
		self.workarea = Some((start, end));
		cx.notify();
	}

	fn clear_workarea(&mut self, cx: &mut Context<Self>) {
		self.workarea = None;
		cx.notify();
	}

	fn new_project(&mut self, cx: &mut Context<Self>) {
		println!("[mock engine] new project: demo data stays (mock mode)");
		cx.notify();
	}

	fn open_project_path(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Result<(), String> {
		self.open_project(path, cx);
		Ok(())
	}

	fn import_footage(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Result<(), String> {
		self.imported_footage.push(path);
		cx.notify();
		Ok(())
	}

	fn drop_footage(
		&mut self,
		id: u64,
		track_kind: TrackKind,
		track_index: usize,
		time: Frame,
		cx: &mut Context<Self>,
	) {
		// The footage's media type, inferred from its entry name (the mock
		// never probes media). Entries the explorer does not list are
		// rejected.
		let Some(name) = self.footage_entry_name(id) else {
			println!("[mock engine] drop footage: entry {id} not in the project");
			cx.notify();
			return;
		};
		let footage_kind = if crate::oakui::filename_is_audio(&name) {
			TrackKind::Audio
		} else {
			TrackKind::Video
		};
		// Track policy (mirrors the facade semantics, see `AppEngine`: the
		// pointed track is used when it matches the footage's kind; a
		// mismatch auto-selects the topmost track of the footage's kind; no
		// matching track rejects the drop).
		let target = if self.tracks.get(track_index).map(|t| t.kind) == Some(footage_kind) {
			track_index
		} else if let Some(index) = self.tracks.iter().position(|t| t.kind == footage_kind) {
			index
		} else {
			println!(
				"[mock engine] drop footage: no {:?} track for {:?} media \"{name}\"",
				footage_kind, track_kind
			);
			cx.notify();
			return;
		};
		// A 10-second demo clip (the mock has no media durations).
		let fps = self.frame_rate();
		let length = Frame(
			(10.0 * fps.num as f64 / fps.den.max(1) as f64)
				.round()
				.max(1.0) as i64,
		);
		let clip = MockClip {
			id: ClipId(self.next_mock_clip_id()),
			range: FrameRange::new(Frame(time.0.max(0)), Frame(time.0.max(0) + length.0)),
			media_in: Frame::ZERO,
			label: name.clone().into(),
			color: if footage_kind == TrackKind::Audio {
				Hsla {
					h: 0.402,
					s: 0.32,
					l: 0.54,
					a: 1.0,
				}
			} else {
				Hsla {
					h: 0.402,
					s: 0.385,
					l: 0.459,
					a: 1.0,
				}
			},
		};
		// Keep the track's clips in ascending frame order (a data-source
		// consistency requirement of the timeline widget).
		let track = &mut self.tracks[target];
		let position = track
			.clips
			.iter()
			.position(|c| c.range.start.0 > time.0)
			.unwrap_or(track.clips.len());
		track.clips.insert(position, clip);
		self.footage_drops.push(MockFootageDrop {
			id,
			track_kind: footage_kind,
			track_index: target,
			time: Frame(time.0.max(0)),
		});
		cx.notify();
	}

	/// The mock's drop ghost extent: the 10-second demo clip length used
	/// by `drop_footage`.
	fn footage_length_frames(&self, id: u64) -> Option<i64> {
		self.footage_entry_name(id)?;
		let fps = self.frame_rate();
		Some(
			(10.0 * fps.num as f64 / fps.den.max(1) as f64)
				.round()
				.max(1.0) as i64,
		)
	}

	fn export_project_path(
		&mut self,
		_path: PathBuf,
		cx: &mut Context<Self>,
	) -> Result<(), String> {
		println!("[mock engine] export: no persistence in mock mode");
		cx.notify();
		Ok(())
	}

	fn close_project(&mut self, cx: &mut Context<Self>) {
		println!("[mock engine] close project: demo data stays (mock mode)");
		cx.notify();
	}

	// --- project library (M13 D4, in-memory fake) -------------------------

	fn storage_bound(&self) -> bool {
		// The mock pretends the demo project lives in the library.
		true
	}

	fn library_projects(&self) -> Result<Vec<LibraryProject>, String> {
		let mut rows = self.library.clone();
		rows.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
		Ok(rows)
	}

	fn library_create_project(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
		let uuid = format!("mock-{}", self.next_library_id);
		self.next_library_id += 1;
		let now = now_unix();
		self.library.push(LibraryProject {
			uuid,
			name: name.to_string(),
			created_at: now,
			modified_at: now,
			duration_ms: 0,
			track_count: 0,
			clip_count: 0,
			footage_count: 0,
		});
		self.project.name = name.to_string();
		self.project.path = PathBuf::new();
		cx.notify();
		Ok(())
	}

	fn library_open_project(&mut self, uuid: &str, cx: &mut Context<Self>) -> Result<(), String> {
		let Some(row) = self.library.iter().find(|row| row.uuid == uuid) else {
			return Err(format!("library project {uuid} not found"));
		};
		self.project.name = row.name.clone();
		self.project.path = PathBuf::new();
		self.library_opened.push(uuid.to_string());
		cx.notify();
		Ok(())
	}

	fn library_delete_project(&mut self, uuid: &str) -> Result<(), String> {
		let before = self.library.len();
		self.library.retain(|row| row.uuid != uuid);
		if self.library.len() == before {
			return Err(format!("library project {uuid} not found"));
		}
		Ok(())
	}

	fn library_rename_project(&mut self, uuid: &str, name: &str) -> Result<(), String> {
		let Some(row) = self.library.iter_mut().find(|row| row.uuid == uuid) else {
			return Err(format!("library project {uuid} not found"));
		};
		row.name = name.to_string();
		row.modified_at = now_unix();
		Ok(())
	}

	fn library_duplicate_project(&mut self, uuid: &str) -> Result<(), String> {
		let Some(row) = self.library.iter().find(|row| row.uuid == uuid).cloned() else {
			return Err(format!("library project {uuid} not found"));
		};
		let now = now_unix();
		self.library.push(LibraryProject {
			uuid: format!("mock-{}", self.next_library_id),
			name: format!("{} (copy)", row.name),
			created_at: now,
			modified_at: now,
			..row
		});
		self.next_library_id += 1;
		Ok(())
	}

	fn library_import_project(&mut self, path: PathBuf) -> Result<String, String> {
		let name = path
			.file_stem()
			.map(|s| s.to_string_lossy().into_owned())
			.filter(|s| !s.is_empty())
			.ok_or_else(|| format!("invalid project file \"{}\"", path.display()))?;
		let uuid = format!("mock-{}", self.next_library_id);
		self.next_library_id += 1;
		let now = now_unix();
		self.library.push(LibraryProject {
			uuid: uuid.clone(),
			name,
			created_at: now,
			modified_at: now,
			duration_ms: 0,
			track_count: 0,
			clip_count: 0,
			footage_count: 0,
		});
		Ok(uuid)
	}

	fn library_export_project(&mut self, uuid: &str, path: PathBuf) -> Result<(), String> {
		if !self.library.iter().any(|row| row.uuid == uuid) {
			return Err(format!("library project {uuid} not found"));
		}
		self.library_exported.push((uuid.to_string(), path));
		Ok(())
	}

	fn start_export(&mut self, _format: i32, _path: PathBuf) -> Result<ExportSession, String> {
		self.start_export_with(&crate::oakui::engine::ExportSettings::default(), _path)
	}

	fn start_export_with(
		&mut self,
		_settings: &crate::oakui::engine::ExportSettings,
		_path: PathBuf,
	) -> Result<ExportSession, String> {
		// Mock export: fake progress on a background thread, no file.
		let (tx, rx) = mpsc::channel::<ExportEvent>();
		std::thread::spawn(move || {
			let _ = tx.send(ExportEvent::Started);
			for (delay_ms, fraction) in [(150u64, 0.33), (300, 0.66), (450, 1.0)] {
				std::thread::sleep(std::time::Duration::from_millis(delay_ms));
				let _ = tx.send(ExportEvent::Progress(fraction));
			}
			let _ = tx.send(ExportEvent::Finished(true, String::new()));
		});
		Ok(ExportSession {
			events: rx,
			cancel: Box::new(|| {}),
		})
	}

	fn sequence_entries(&self) -> Vec<(u64, SharedString)> {
		// The mock bin's one sequence entry (id 4 = "aaa.ove").
		vec![(4, self.sequence.name.clone().into())]
	}

	fn current_sequence_id(&self) -> Option<u64> {
		Some(4)
	}

	fn start_export_of(
		&mut self,
		_id: u64,
		settings: &crate::oakui::engine::ExportSettings,
		path: PathBuf,
	) -> Result<ExportSession, String> {
		self.start_export_with(settings, path)
	}

	fn use_proxy_media(&self) -> bool {
		self.use_proxy
	}

	fn set_use_proxy_media(&mut self, enabled: bool, cx: &mut Context<Self>) {
		self.use_proxy = enabled;
		// The mock's viewer frames are synthetic; still drop the cache so
		// the toggle behaves like the real engine.
		self.cpu_frame_cache.lock().unwrap().clear();
		cx.notify();
	}

	fn project_ocio_config(&self) -> String {
		self.ocio_config.clone()
	}

	fn set_project_ocio_config(
		&mut self,
		path: String,
		cx: &mut Context<Self>,
	) -> Result<(), String> {
		let trimmed = path.trim().to_string();
		// Validate like the real engine (a bogus path keeps the dialog open).
		if !trimmed.is_empty() {
			oak_core::color::set_up_default_config_from(Some(&trimmed))
				.map_err(|e| e.to_string())?;
		}
		self.ocio_config = trimmed;
		cx.notify();
		Ok(())
	}

	fn project_cache_location(&self) -> (i32, String) {
		self.cache_location.clone()
	}

	fn set_project_cache_location(
		&mut self,
		setting: i32,
		custom_path: String,
		cx: &mut Context<Self>,
	) {
		let setting = setting.clamp(0, 2);
		self.cache_location = (
			setting,
			if setting == 2 {
				custom_path.trim().to_string()
			} else {
				String::new()
			},
		);
		cx.notify();
	}

	/// 链接/重新链接 the demo clips: a fully linked selection unlinks,
	/// otherwise the set links together (matching the real engine's rule).
	/// The demo stores the pairs (normalized min-max ids).
	fn toggle_clip_links(&mut self, clips: Vec<ClipId>, cx: &mut Context<Self>) {
		if clips.len() < 2 {
			return;
		}
		let pair = |a: u64, b: u64| if a < b { (a, b) } else { (b, a) };
		let all_linked = clips.iter().enumerate().all(|(i, a)| {
			clips[i + 1..]
				.iter()
				.all(|b| self.clip_links.contains(&pair(a.0, b.0)))
		});
		for (i, a) in clips.iter().enumerate() {
			for b in &clips[i + 1..] {
				if all_linked {
					self.clip_links.remove(&pair(a.0, b.0));
				} else {
					self.clip_links.insert(pair(a.0, b.0));
				}
			}
		}
		cx.notify();
	}

	fn proxy_rows(&self) -> Vec<crate::oakui::engine::ProxyFootageRow> {
		// Every explorer footage entry gets a row; audio-only entries keep
		// `can_generate` off (the proxy pipeline is video-only).
		let mut rows = Vec::new();
		for root in self.roots() {
			for entry in std::iter::once(root.clone()).chain(self.children(root.id)) {
				if entry.is_dir {
					continue;
				}
				let is_audio = crate::oakui::filename_is_audio(&entry.name.to_string());
				let state = self
					.proxy_states
					.get(&entry.id)
					.copied()
					.unwrap_or(crate::oakui::engine::ProxyMediaState::Missing);
				rows.push(crate::oakui::engine::ProxyFootageRow {
					id: entry.id,
					name: entry.name.to_string(),
					state,
					enabled: self.proxy_enabled.get(&entry.id).copied().unwrap_or(false),
					has_custom: self.proxy_custom.contains_key(&entry.id),
					can_generate: !is_audio,
					has_proxy: state != crate::oakui::engine::ProxyMediaState::Missing,
				});
			}
		}
		rows
	}

	fn proxy_state(&self, id: u64) -> Option<crate::oakui::engine::ProxyMediaState> {
		self.footage_entry_name(id)?;
		Some(
			self.proxy_states
				.get(&id)
				.copied()
				.unwrap_or(crate::oakui::engine::ProxyMediaState::Missing),
		)
	}

	fn proxy_row(&self, id: u64) -> Option<crate::oakui::engine::ProxyFootageRow> {
		self.proxy_rows().into_iter().find(|row| row.id == id)
	}

	fn proxy_generate(&mut self, id: u64, cx: &mut Context<Self>) -> Result<(), String> {
		let Some(name) = self.footage_entry_name(id) else {
			return Err("entry is not footage".into());
		};
		if crate::oakui::filename_is_audio(&name) {
			return Err("the footage has no video stream".into());
		}
		// The mock has no ffmpeg pipeline: the proxy is instantly ready.
		self.proxy_states
			.insert(id, crate::oakui::engine::ProxyMediaState::Ready);
		self.proxy_enabled.insert(id, true);
		self.cpu_frame_cache.lock().unwrap().clear();
		cx.notify();
		Ok(())
	}

	fn proxy_delete(&mut self, id: u64, cx: &mut Context<Self>) {
		self.proxy_states.remove(&id);
		self.proxy_enabled.remove(&id);
		self.cpu_frame_cache.lock().unwrap().clear();
		cx.notify();
	}

	fn proxy_set_enabled(&mut self, id: u64, enabled: bool, cx: &mut Context<Self>) {
		self.proxy_enabled.insert(id, enabled);
		self.cpu_frame_cache.lock().unwrap().clear();
		cx.notify();
	}

	fn proxy_reveal(&self, id: u64) {
		println!("[mock engine] reveal proxy for entry {id} (no files in mock mode)");
	}

	fn proxy_set_custom_params(
		&mut self,
		id: u64,
		params: crate::oakui::engine::ProxyParamsUi,
		cx: &mut Context<Self>,
	) {
		self.proxy_custom.insert(id, params);
		cx.notify();
	}

	fn proxy_clear_custom_params(&mut self, id: u64, cx: &mut Context<Self>) {
		self.proxy_custom.remove(&id);
		cx.notify();
	}

	fn proxy_custom_params(&self, id: u64) -> Option<crate::oakui::engine::ProxyParamsUi> {
		self.proxy_custom.get(&id).cloned()
	}

	fn sync_eligibility(&self, clips: &[ClipId]) -> crate::oakui::engine::SyncEligibility {
		// Demo semantics: every selected video clip carries a source start
		// timecode; the mock keeps no waveform cache, so waveform sync stays
		// unavailable.
		let mut eligibility = crate::oakui::engine::SyncEligibility::default();
		for clip in clips {
			let label = self.tracks.iter().find_map(|track| {
				track
					.clips
					.iter()
					.find(|c| c.id() == *clip)
					.map(|c| c.label.clone())
			});
			match label {
				Some(label) if !crate::oakui::filename_is_audio(&label) => {
					eligibility.source_time += 1;
				}
				_ => {}
			}
		}
		eligibility
	}

	fn sync_clips_by_source_time(&mut self, clips: Vec<ClipId>, cx: &mut Context<Self>) {
		self.mock_sync_clips(&clips);
		cx.notify();
	}

	fn sync_clips_by_waveform(
		&mut self,
		clips: Vec<ClipId>,
		_adjust_speed: bool,
		cx: &mut Context<Self>,
	) {
		// The mock has no waveform data: align by in point like the source
		// timecode path (demo approximation).
		self.mock_sync_clips(&clips);
		cx.notify();
	}

	fn clip_footage_entries(&self, clips: &[ClipId]) -> Vec<crate::oakui::engine::ProxyFootageRow> {
		// The demo's clip ids ARE the explorer entry ids.
		let rows = self.proxy_rows();
		clips
			.iter()
			.filter_map(|clip| rows.iter().find(|row| row.id == clip.0).cloned())
			.collect()
	}

	fn multicam_state(&self) -> Option<MulticamState> {
		self.mock_multicam_state()
	}

	fn multicam_angle_frame(
		&mut self,
		source: i32,
		cx: &mut Context<Self>,
	) -> Option<Arc<RenderImage>> {
		let playhead = self.clock_frame(Monitor::Program, cx).0;
		self.mock_multicam_angle_frame(source, playhead)
	}

	fn multicam_eligible(&self, _clips: &[ClipId]) -> bool {
		// The demo always exposes a multicam setup, so the timeline menu
		// item is enabled (the mock has no clip→viewer wiring to judge).
		self.mock_multicam_state().is_some()
	}

	fn multicam_enabled_on_selection(&self, _clips: &[ClipId]) -> bool {
		self.mock_multicam_state().is_some()
	}

	fn multicam_enable_selected(
		&mut self,
		_clips: Vec<ClipId>,
		enabled: bool,
		cx: &mut Context<Self>,
	) {
		// Run the real enable/disable commands on the demo graph (one undo
		// entry each, like the real engine).
		let mut guard = self.ensure_demo_multicam();
		let Some(demo) = guard.as_mut() else {
			return;
		};
		let clip = demo.clip.clone();
		if enabled {
			if demo.multicam.is_some() {
				return; // The demo starts enabled; enabling again is a no-op.
			}
			let sequence = demo.sequence.clone();
			let cmd = oak_timeline::multicam::multicam_enable(vec![clip], sequence);
			let label = oak_timeline::multicam::enable_label(1);
			if let Err(e) = super::graphops::push_command(cmd, &label) {
				println!("[mock] multicam enable failed: {e}");
			}
		} else {
			let cmd = oak_timeline::multicam::multicam_disable(vec![clip]);
			let label = oak_timeline::multicam::disable_label(1);
			if let Err(e) = super::graphops::push_command(cmd, &label) {
				println!("[mock] multicam disable failed: {e}");
			}
		}
		// Re-resolve the multicam node after the command.
		demo.multicam = oak_timeline::multicam::clip_find_multicam(&demo.clip);
		self.multicam_frames.lock().unwrap().clear();
		cx.notify();
	}

	fn multicam_switch_to(&mut self, source: i32, split_clip: bool, cx: &mut Context<Self>) {
		let guard = self.ensure_demo_multicam();
		let Some(demo) = guard.as_ref() else {
			return;
		};
		let Some(state) =
			crate::oakui::multicam::multicam_state_for_clip(&demo.project, demo.clip.id)
		else {
			return;
		};
		if source < 0 || source >= state.source_count {
			return;
		}
		let playhead_frame = self.clock_frame(Monitor::Program, cx).0;
		let playhead = Rational::new(playhead_frame.max(0), 25);
		let cmd = oak_timeline::multicam::multicam_switch(
			demo.clip.clone(),
			source,
			split_clip,
			playhead,
		);
		if let Err(e) = super::graphops::push_command(cmd, oak_timeline::multicam::SWITCH_LABEL) {
			println!("[mock] multicam switch failed: {e}");
		}
		drop(guard);
		self.multicam_frames.lock().unwrap().clear();
		cx.notify();
	}

	// --- multicam wizard (mock: the demo footage entries) ---------------

	fn multicam_wizard_footage(&self) -> Option<Vec<WizardFootage>> {
		Some(vec![
			WizardFootage {
				id: 3,
				name: "第一稿.mp4".into(),
				source_timecode: Some(0),
				duration_s: Some(30.0),
				has_audio: Some(true),
			},
			WizardFootage {
				id: 10,
				name: "intro.mov".into(),
				source_timecode: Some(1000),
				duration_s: Some(45.0),
				has_audio: Some(true),
			},
			WizardFootage {
				id: 11,
				name: "b-roll.mp4".into(),
				source_timecode: Some(2000),
				duration_s: Some(60.0),
				has_audio: Some(true),
			},
			WizardFootage {
				id: 12,
				name: "interview.mov".into(),
				source_timecode: Some(500),
				duration_s: Some(90.0),
				has_audio: Some(true),
			},
		])
	}

	fn multicam_wizard_sync_offsets(
		&self,
		selected: &[WizardFootage],
	) -> Result<Vec<WizardSyncOffset>, String> {
		// The mock's correlation is a demo: each angle's offset is derived
		// from its source timecode relative to the reference (angle 0).
		let reference_tc = selected
			.first()
			.and_then(|f| f.source_timecode)
			.unwrap_or(0) as f64;
		Ok(selected
			.iter()
			.map(|f| WizardSyncOffset {
				footage: f.id,
				offset_s: f.source_timecode.map(|t| t as f64 / 1000.0).unwrap_or(0.0)
					- reference_tc / 1000.0,
				confidence: f.has_audio.unwrap_or(false).then_some(0.95),
			})
			.collect())
	}

	fn multicam_create_sequence(
		&mut self,
		selected: Vec<WizardFootage>,
		offsets: Vec<f64>,
		name: String,
		cx: &mut Context<Self>,
	) -> Result<u64, String> {
		let _ = (&selected, offsets);
		// The mock has no project graph for a NEW sequence outside its
		// demo: report the creation by switching the demo's name and
		// returning a stable identity (the panel already shows the demo
		// multicam — the wizard's create is a verbose success).
		if self.sequence.name.is_empty() {
			self.sequence.name = name;
		}
		cx.notify();
		Ok(1)
	}

	fn backend_name(&self) -> &'static str {
		"mock"
	}
}

// ---------------------------------------------------------------------------
// Data-source traits
// ---------------------------------------------------------------------------

impl TimelineDataSource for MockEngine {
	type Track = MockTrack;

	fn frame_rate(&self) -> FrameRate {
		self.sequence.format.rate
	}

	fn sequence_length(&self) -> Frame {
		self.sequence.length
	}

	fn track_count(&self) -> usize {
		self.tracks.len()
	}

	fn track(&self, index: usize) -> Option<Self::Track> {
		self.tracks.get(index).cloned()
	}

	fn markers(&self) -> Vec<Marker> {
		self.markers.clone()
	}
}

impl EffectStackDataSource for MockEngine {
	fn effects(&self) -> Vec<Arc<dyn EffectData>> {
		self.effects
			.iter()
			.map(|effect| {
				Arc::new(MockEffect {
					id: effect.id,
					kind: effect.kind,
					title: effect.title.clone(),
					subtitle: effect.subtitle.clone(),
					enabled: effect.enabled,
					expanded: effect.expanded,
					badge: effect.badge,
				}) as Arc<dyn EffectData>
			})
			.collect()
	}

	fn target_label(&self) -> Option<SharedString> {
		Some("第一稿.mp4 · 00:00:00:00–00:04:18:18".into())
	}

	fn selected_effect(&self) -> Option<EffectId> {
		if self.node_selection.len() != 1 {
			return None;
		}
		let node = *self.node_selection.iter().next()?;
		self.effect_for_node(node)
	}
}

impl NodeGraphDataSource for MockEngine {
	type Node = MockNode;
	type Edge = MockEdge;

	fn nodes(&self) -> Vec<Self::Node> {
		self.nodes.clone()
	}

	fn edges(&self) -> Vec<Self::Edge> {
		self.edges.clone()
	}

	fn can_connect(&self, from: PortId, to: PortId) -> bool {
		let (Some(from_node), Some(to_node)) = (self.node_with_port(from), self.node_with_port(to))
		else {
			return false;
		};
		if from_node.id() == to_node.id() {
			return false;
		}
		let (Some(from_port), Some(to_port)) = (self.port(from), self.port(to)) else {
			return false;
		};
		if from_port.kind() != PortKind::Output || to_port.kind() != PortKind::Input {
			return false;
		}
		if from_port.data_type() != to_port.data_type() {
			return false;
		}
		if self.edge_exists(from, to) {
			return false;
		}
		// Connecting from_node → to_node would create a cycle iff there is
		// already a path from to_node back to from_node.
		!self.reaches(to_node.id(), from_node.id())
	}
}

impl ProjectDataSource for MockEngine {
	fn roots(&self) -> Vec<ProjectEntry> {
		vec![
			ProjectEntry::new(1, crate::i18n::tr("bin.footage"), true),
			ProjectEntry::new(2, crate::i18n::tr("bin.music"), true),
			ProjectEntry::new(3, "第一稿.mp4", false),
			ProjectEntry::new(4, "aaa.ove", false),
		]
	}

	fn children(&self, parent_id: u64) -> Vec<ProjectEntry> {
		match parent_id {
			1 => vec![
				ProjectEntry::new(10, "intro.mov", false),
				ProjectEntry::new(11, "b-roll.mp4", false),
				ProjectEntry::new(12, "interview.mov", false),
			],
			2 => vec![
				ProjectEntry::new(20, "track-01.wav", false),
				ProjectEntry::new(21, "track-02.wav", false),
			],
			_ => Vec::new(),
		}
	}
}

impl AudioMeterDataSource for MockEngine {
	fn levels(&self) -> Vec<f32> {
		self.meter_levels()
	}
}

// ---------------------------------------------------------------------------
// Demo multicam (the mock's multicam panel grid)
// ---------------------------------------------------------------------------

/// The mock's demo multicam: a real oaknode graph whose source sequence's
/// video tracks are the angles. The panel's frames are synthetic colored
/// cells, but the switch / enable / disable commands run on the REAL command
/// path (`oak_timeline::multicam` + the global undo stack), so the demo
/// exercises the same machinery the real engine uses.
struct DemoMulticamGraph {
	/// The project holding the graph.
	project: graphops::ProjectRef,
	/// The clip whose texture input the multicam feeds.
	clip: oak_timeline::util::NodeRef,
	/// The source sequence (its video tracks are the angles).
	sequence: oak_timeline::util::NodeRef,
	/// The multicam node (present while enabled).
	multicam: Option<oak_timeline::util::NodeRef>,
}

impl DemoMulticamGraph {
	/// Builds the demo graph: a sequence with four video tracks, one clip on
	/// the top track fed by the sequence, multicam already enabled. The
	/// tracks are built directly in the graph (no `Add Track` undo entries —
	/// the demo's initial state is not a user edit).
	fn build() -> Self {
        use oak_node::node::NodeCore;
        use oak_node::sequence::SequenceBehavior;
        use oak_node::track::{TrackBehavior, TrackListBehavior};

        let project = graphops::create_project();
		let sequence = graphops::create_sequence(&project, "Multicam Demo");
		// A video track list with four tracks, wired into the sequence.
		{
			let mut g = graphops::lock(&project);
			let (core, behavior) = TrackListBehavior::create();
			let mut behavior = behavior;
			let list = behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<TrackListBehavior>()
				.unwrap();
			list.kind = TrackType::Video;
			list.sequence = Some(sequence);
			let list_id = g.graph.add_node(core, behavior);
			for _ in 0..4 {
				let (core, behavior) = (
					NodeCore::new(),
					Box::new(TrackBehavior::new(TrackType::Video)),
				);
				let track_id = g.graph.add_node(core, behavior);
				let t = g
					.graph
					.get_mut(track_id)
					.unwrap()
					.behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<TrackBehavior>()
					.unwrap();
				t.kind = TrackType::Video;
				t.track_list = Some(list_id);
				let l = g
					.graph
					.get_mut(list_id)
					.unwrap()
					.behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<TrackListBehavior>()
					.unwrap();
				l.tracks.push(track_id);
			}
			let s = g
				.graph
				.get_mut(sequence)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<SequenceBehavior>()
				.unwrap();
			s.track_lists.push(list_id);
		}
		let clip = block_clip_create(&project);
		{
			let mut g = graphops::lock(&project);
			let c = g
				.graph
				.get_mut(clip.id)
				.unwrap()
				.behavior
				.as_any_mut()
				.unwrap()
				.downcast_mut::<oak_node::block::ClipBlockBehavior>()
				.unwrap();
			c.core.range = oak_core::TimeRange::new(Rational::new(0, 1), Rational::new(200, 1));
			c.core.media_in = Rational::new(0, 1);
		}
		let track0 = {
			let g = graphops::lock(&project);
			graphops::track_ids(&g.graph, sequence, TrackType::Video)[0]
		};
		let track0 = oak_timeline::util::NodeRef::new(project.clone(), track0);
		track_append_block(&track0, &clip);
		{
			let mut g = graphops::lock(&project);
			g.graph
				.connect(sequence, clip.id, clip_input::TEXTURE_INPUT, -1)
				.unwrap();
		}
		// Enable multicam through the real command (kept out of the undo
		// stack — it is the demo's initial state, not a user edit).
		let mut enable = oak_timeline::multicam::MultiCamEnableCommand::new(
			vec![clip.clone()],
			oak_timeline::util::NodeRef::new(project.clone(), sequence),
		);
		enable.redo();
		let multicam = oak_timeline::multicam::clip_find_multicam(&clip);
		let sequence = oak_timeline::util::NodeRef::new(project.clone(), sequence);
		DemoMulticamGraph {
			project,
			clip,
			sequence,
			multicam,
		}
	}
}

impl MockEngine {
	/// The demo multicam graph, built on first access.
	fn ensure_demo_multicam(&self) -> std::sync::MutexGuard<'_, Option<DemoMulticamGraph>> {
		let mut guard = self.multicam_graph.lock().unwrap();
		if guard.is_none() {
			*guard = Some(DemoMulticamGraph::build());
		}
		guard
	}

	/// The demo multicam state (the panel's grid): source count = the demo
	/// sequence's video track count, current source read from the multicam
	/// node.
	fn mock_multicam_state(&self) -> Option<MulticamState> {
		let guard = self.ensure_demo_multicam();
		let demo = guard.as_ref()?;
		let state = crate::oakui::multicam::multicam_state_for_clip(&demo.project, demo.clip.id)?;
		Some(state)
	}

	/// The demo angle frame: a solid colored cell per source with a moving
	/// white stripe (the mock cannot decode media; the cells are synthetic
	/// but the grid geometry and the switch commands are real).
	fn demo_angle_image(source: i32, playhead: i64) -> Option<Arc<RenderImage>> {
		const W: u32 = 160;
		const H: u32 = 90;
		let palette: [(u8, u8, u8); 9] = [
			(255, 0, 0),
			(0, 255, 0),
			(0, 0, 255),
			(255, 255, 0),
			(255, 0, 255),
			(0, 255, 255),
			(255, 128, 0),
			(128, 0, 255),
			(0, 128, 255),
		];
		let (pr, pg, pb) = palette[source.rem_euclid(9) as usize];
		let stripe = (playhead * 6) % W as i64;
		let mut bytes = Vec::with_capacity((W * H * 4) as usize);
		for y in 0..H {
			for x in 0..W {
				let (r, g, b) = if (x as i64 - stripe).abs() < 6 {
					(255, 255, 255)
				} else if (y as i64) < 18 {
					(pr, pg, pb)
				} else {
					// Darken below the "label" band so the cells read as
					// distinct angles.
					(pr / 2, pg / 2, pb / 2)
				};
				// BGRA8 display order.
				bytes.extend_from_slice(&[b, g, r, 255]);
			}
		}
		crate::oakui::frames::bgra_bytes_to_render_image(W, H, &bytes).map(Arc::new)
	}

	/// The demo angle frame for `source` at the current program playhead,
	/// cached per (source, playhead) so a paused cell never regenerates.
	fn mock_multicam_angle_frame(&self, source: i32, playhead: i64) -> Option<Arc<RenderImage>> {
		let mut cache = self.multicam_frames.lock().unwrap();
		if let Some((cached_playhead, image)) = cache.get(&source) {
			if *cached_playhead == playhead {
				return Some(image.clone());
			}
		}
		let image = Self::demo_angle_image(source, playhead)?;
		cache.insert(source, (playhead, image.clone()));
		Some(image)
	}
}

/// Convenience accessors used by panels and the status bar.
impl MockEngine {
	/// The paths imported via [`AppEngine::import_footage`] so far (mock state;
	/// the mock has no media pipeline, it just records the request).
	pub fn imported_footage(&self) -> &[PathBuf] {
		&self.imported_footage
	}

	/// The footage entries dropped onto the timeline so far (mock state; see
	/// [`MockFootageDrop`]).
	pub fn footage_drops(&self) -> &[MockFootageDrop] {
		&self.footage_drops
	}

	/// The display name of the project-explorer entry with `id`, if any.
	fn footage_entry_name(&self, id: u64) -> Option<String> {
		for entry in self.roots() {
			if entry.id == id {
				return Some(entry.name.to_string());
			}
			for child in self.children(entry.id) {
				if child.id == id {
					return Some(child.name.to_string());
				}
			}
		}
		None
	}

	/// The undo/redo call counts (test observability; see the fields).
	pub fn undo_redo_calls(&self) -> (u64, u64) {
		(self.undo_calls, self.redo_calls)
	}

	/// The uuids opened via [`AppEngine::library_open_project`] so far
	/// (mock state; drives app-level tests of the manager's open flow).
	pub fn library_opened(&self) -> &[String] {
		&self.library_opened
	}

	/// The (uuid, path) pairs exported via
	/// [`AppEngine::library_export_project`] so far (mock state).
	pub fn library_exported(&self) -> &[(String, PathBuf)] {
		&self.library_exported
	}

	/// The selected material-bin entry id (demo state).
	pub fn selected_item(&self) -> Option<u64> {
		self.selected_item
	}

	/// Selects a material-bin entry (demo "open" action).
	pub fn select_item(&mut self, id: u64, cx: &mut Context<Self>) {
		self.selected_item = Some(id);
		cx.notify();
	}

	/// Reads the current frame of a monitor's clock.
	pub fn clock_frame(&self, monitor: Monitor, cx: &App) -> Frame {
		self.clock(monitor).read(cx).transport.frame()
	}

	/// The synthetic CPU test frame for `monitor`, cached per playhead frame so
	/// a paused viewer never regenerates its picture. This is the frame the
	/// source/program viewers display through [`ViewerWidget::set_cpu_frame`],
	/// proving the CPU-frame path end to end (the real engine shows the same
	/// pattern until the render-worker frame transport is bound).
	pub fn cpu_frame(&self, monitor: Monitor, cx: &App) -> Arc<RenderImage> {
		let frame = self.clock_frame(monitor, cx);
		let mut cache = self.cpu_frame_cache.lock().unwrap();
		if let Some((cached_frame, image, _)) = cache.get(&monitor) {
			if *cached_frame == frame.0 {
				return image.clone();
			}
		}
		let (width, height, samples) = crate::oakui::frames::synthetic_frame_samples(frame);
		// Analyze the scopes from the same F32 samples the viewer displays.
		let scope = crate::oakui::scopes::analyze_f32_rgba(width, height, &samples);
		let image = Arc::new(crate::oakui::frames::f32_rgba_to_bgra_image(
			width, height, &samples,
		));
		// The 10-bit path: upload the F32 samples as an RGBA16F texture so
		// the viewer samples them straight to the swapchain instead of the
		// BGRA8 image (best-effort — no GPU, no registration, CPU path).
		crate::oakui::gpu::register_display_frame(image.id.0, width, height, &samples);
		cache.insert(monitor, (frame.0, image.clone(), scope));
		image
	}

	/// The scope samples of `monitor`'s current frame, from the same cache
	/// [`MockEngine::cpu_frame`] fills (the analysis runs in the frame
	/// generation pass, so this never re-walks a frame).
	pub fn scope_data(&self, monitor: Monitor, cx: &App) -> ScopeData {
		// Ensure the cache holds the current playhead frame.
		let _ = self.cpu_frame(monitor, cx);
		let cache = self.cpu_frame_cache.lock().unwrap();
		cache
			.get(&monitor)
			.map(|(_, _, scope)| scope.clone())
			.unwrap_or_default()
	}
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn demo_engine(app: &mut gpui::App) -> Entity<MockEngine> {
		app.new(|cx| MockEngine::demo(cx))
	}

	#[gpui::test]
	async fn demo_project_has_a_sequence_and_four_tracks(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			let engine = engine.read(app);
			let sequence = engine.current_sequence().expect("demo sequence");
			assert_eq!(sequence.name, "第一稿");
			assert_eq!(sequence.length, Frame(SEQUENCE_LENGTH));
			assert_eq!(sequence.format.width, 1920);
			assert_eq!(sequence.format.height, 1080);
			assert_eq!(engine.track_count(), 4);
			assert_eq!(engine.track(0).expect("V2").kind(), TrackKind::Video);
			assert_eq!(engine.track(2).expect("A1").kind(), TrackKind::Audio);
		});
	}

	#[gpui::test]
	async fn gateway_play_starts_the_program_clock(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			engine.update(app, |engine, cx| {
				EngineGateway::play(engine, Monitor::Program, cx);
			});
			let clock = engine.read(app).program_clock.read(app);
			assert!(clock.transport.is_playing());
		});
	}

	#[gpui::test]
	async fn gateway_step_moves_the_clock_within_the_sequence(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			engine.update(app, |engine, cx| {
				engine.step(Monitor::Program, 25, cx);
			});
			let frame = engine.read(app).clock_frame(Monitor::Program, app);
			assert_eq!(frame, Frame(25));

			// Stepping far past the end clamps to the last frame.
			engine.update(app, |engine, cx| {
				engine.step(Monitor::Program, 1_000_000, cx);
			});
			let frame = engine.read(app).clock_frame(Monitor::Program, app);
			assert_eq!(frame, Frame(SEQUENCE_LENGTH - 1));
		});
	}

	#[gpui::test]
	async fn gateway_request_frame_seeks_and_pauses_stays(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			engine.update(app, |engine, cx| {
				engine.request_frame(Monitor::Source, Frame(42), cx);
			});
			let frame = engine.read(app).clock_frame(Monitor::Source, app);
			assert_eq!(frame, Frame(42));
		});
	}

	/// The stop-on-last tick pauses on the final frame instead of wrapping.
	#[test]
	fn clock_tick_stops_on_the_last_frame_when_asked() {
        use std::time::{Duration, Instant};
        let mut clock = MockClock::new(FrameRate::new(30, 1));
		clock.play();
		// 10 s at 30 fps = 300 frames into a 5-frame sequence: wrapped 60×.
		clock.started = Some((Instant::now() - Duration::from_secs(10), Frame(0)));

		clock.tick(Frame(5), true);
		assert_eq!(
			clock.transport.frame(),
			Frame(4),
			"pinned to the last frame"
		);
		assert!(!clock.transport.is_playing(), "playback stopped");
		// A later tick is a no-op: the anchor is cleared.
		clock.tick(Frame(5), true);
		assert_eq!(clock.transport.frame(), Frame(4));
	}

	/// Without stop-on-last the tick wraps modulo the sequence length.
	#[test]
	fn clock_tick_loops_when_not_stopping() {
        use std::time::{Duration, Instant};
        let mut clock = MockClock::new(FrameRate::new(30, 1));
		clock.play();
		clock.started = Some((Instant::now() - Duration::from_secs(10), Frame(0)));

		clock.tick(Frame(5), false);
		assert_eq!(clock.transport.frame(), Frame(0), "300 % 5 = 0");
		assert!(clock.transport.is_playing(), "still playing while looping");
	}

	#[gpui::test]
	async fn effect_stack_edit_applies_to_the_model(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			// Remove the OCIO LUT card (id 2).
			engine.update(app, |engine, cx| {
				engine.apply_effect_event(&EffectStackEvent::RemoveRequested(EffectId(2)), cx);
			});
			let stack = engine.read(app).effects();
			assert_eq!(stack.len(), 3);
			let titles: Vec<_> = stack.iter().map(|e| e.title().to_string()).collect();
			assert!(!titles.contains(&"OCIO LUT".to_string()));
			// Source and output cards are pinned and not removable.
			assert_eq!(titles.first().map(String::as_str), Some("媒体"));
			assert_eq!(titles.last().map(String::as_str), Some("输出"));
		});
	}

	/// The card-header click round trip: expansion toggles collapse then
	/// re-expand. The header click emits BOTH `ExpansionToggled` and
	/// `CardSelected`; the card selection mirrors its node into the node
	/// editor, whose `SelectionChanged` echo must not fight the toggle (the
	/// demo's 变换 card maps to graph node 2).
	#[gpui::test]
	async fn effect_card_expansion_survives_the_card_selection_echo(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			let toggle = |app: &mut gpui::App, expanded: bool| {
				engine.update(app, |engine, cx| {
					engine.apply_effect_event(
						&EffectStackEvent::ExpansionToggled {
							effect: EffectId(1),
							expanded,
						},
						cx,
					);
				});
			};
			let is_expanded = |app: &gpui::App| -> bool {
				engine
					.read(app)
					.effects()
					.iter()
					.find(|e| e.id() == EffectId(1))
					.map(|e| e.is_expanded())
					.unwrap_or(false)
			};

			// The demo's 变换 card starts expanded; the collapse click also
			// selects the card (CardSelected → node_selection = {2}), and the
			// node-editor panel then mirrors that into the graph widget, whose
			// SelectionChanged echo arrives through apply_node_graph_event.
			assert!(is_expanded(app), "the demo card starts expanded");
			toggle(app, false);
			engine.update(app, |engine, cx| {
				engine.apply_effect_event(
					&EffectStackEvent::CardSelected {
						effect: EffectId(1),
					},
					cx,
				);
			});
			engine.update(app, |engine, cx| {
				engine.apply_node_graph_event(
					&NodeGraphEvent::SelectionChanged {
						nodes: BTreeSet::from([NodeId(2)]),
					},
					cx,
				);
			});
			assert!(
				!is_expanded(app),
				"the selection echo must not re-expand the collapsed card"
			);

			// Expand → collapse round trip stays clean.
			toggle(app, true);
			assert!(is_expanded(app));
			toggle(app, false);
			assert!(!is_expanded(app));
		});
	}

	#[gpui::test]
	async fn timeline_edits_are_applied_to_the_mock_model(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);

			// Trim the V1 "B-roll.mp4" clip (id 12, 240–600) in by 40 frames.
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::ClipTrimRequested {
						clip: ClipId(12),
						edge: TrimEdge::Start,
						new_frame: Frame(280),
					},
					cx,
				);
			});
			let v1_track = engine.read(app).track(1).expect("V1");
			let b_roll = v1_track
				.clips()
				.iter()
				.find(|c| c.id() == ClipId(12))
				.expect("B-roll clip");
			assert_eq!(b_roll.range(), FrameRange::new(Frame(280), Frame(600)));
			assert_eq!(b_roll.media_in(), Frame(140), "media-in follows the trim");

			// Move the 开场 clip (id 11) onto the V2 track at frame 300.
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::ClipMoveRequested {
						clip: ClipId(11),
						new_track: 0,
						new_start: Frame(300),
					},
					cx,
				);
			});
			let v2 = engine.read(app).track(0).expect("V2");
			assert!(
				v2.clips()
					.iter()
					.any(|c| c.id() == ClipId(11) && c.range().start == Frame(300)),
				"开场 moved to V2 at frame 300"
			);
			assert!(
				!engine
					.read(app)
					.track(1)
					.expect("V1")
					.clips()
					.iter()
					.any(|c| c.id() == ClipId(11)),
				"开场 left V1"
			);
		});
	}

	#[gpui::test]
	async fn track_toggle_requests_flip_the_track_flags(cx: &mut TestAppContext) {
		use gpui::timeline::TrackHeaderEvent;
		cx.update(|app| {
			let engine = demo_engine(app);

			// V1 (display index 1) starts unlocked + visible.
			let v1 = engine.read(app).track(1).expect("V1");
			assert!(!v1.is_locked());
			assert!(v1.is_visible());

			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::TrackToggleRequested {
						track: 1,
						toggle: TrackHeaderEvent::ToggleLock,
					},
					cx,
				);
			});
			assert!(engine.read(app).track(1).expect("V1").is_locked());

			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::TrackToggleRequested {
						track: 1,
						toggle: TrackHeaderEvent::ToggleVisibility,
					},
					cx,
				);
			});
			let v1 = engine.read(app).track(1).expect("V1");
			assert!(!v1.is_visible(), "the visibility toggle hides the track");

			// A1 (display index 2) mutes.
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::TrackToggleRequested {
						track: 2,
						toggle: TrackHeaderEvent::ToggleMute,
					},
					cx,
				);
			});
			assert!(engine.read(app).track(2).expect("A1").is_muted());
		});
	}

	#[gpui::test]
	async fn locked_tracks_reject_clip_edits(cx: &mut TestAppContext) {
		use gpui::timeline::TrackHeaderEvent;
		cx.update(|app| {
			let engine = demo_engine(app);

			// Lock V1 (the B-roll track, display index 1).
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::TrackToggleRequested {
						track: 1,
						toggle: TrackHeaderEvent::ToggleLock,
					},
					cx,
				);
			});

			// Trim, move and delete are all refused.
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::ClipTrimRequested {
						clip: ClipId(12),
						edge: TrimEdge::Start,
						new_frame: Frame(280),
					},
					cx,
				);
				engine.apply_timeline_event(
					&TimelineEvent::ClipMoveRequested {
						clip: ClipId(12),
						new_track: 0,
						new_start: Frame(300),
					},
					cx,
				);
				engine.delete_clip(ClipId(12), false, cx);
			});
			let v1 = engine.read(app).track(1).expect("V1");
			let b_roll = v1
				.clips()
				.iter()
				.find(|c| c.id() == ClipId(12))
				.expect("B-roll survives the rejected delete");
			assert_eq!(
				b_roll.range(),
				FrameRange::new(Frame(240), Frame(600)),
				"trim and move were rejected"
			);
		});
	}

	#[gpui::test]
	async fn add_effect_appends_a_named_card(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			let effects = engine.read(app).addable_effects();
			assert!(!effects.is_empty(), "the demo list is the factory table");
			let first = effects[0].clone();
			let (type_id, name) = (first.type_id, first.name);
			let before = engine.read(app).effects().len();
			engine.update(app, |engine, cx| {
				engine
					.add_effect(usize::MAX, &type_id, cx)
					.expect("add the effect");
			});
			let stack = engine.read(app).effects();
			assert_eq!(stack.len(), before + 1);
			assert_eq!(
				stack.last().map(|e| e.title().to_string()).as_deref(),
				Some(name.as_str()),
				"the card was appended at the chain end"
			);
		});
	}

	#[gpui::test]
	async fn mock_split_at_playhead_and_ripple_delete(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);

			// Park the program playhead inside 开场 (0–240) and split there.
			engine.update(app, |engine, cx| {
				engine.request_frame(Monitor::Program, Frame(120), cx);
				engine.split_at_playhead(cx);
			});
			let v1 = engine.read(app).track(1).expect("V1");
			assert_eq!(v1.clips().len(), 3, "开场 split into two");
			let spanning = v1
				.clips()
				.iter()
				.filter(|c| c.range().start.0 < 120 && 120 < c.range().end.0)
				.count();
			assert_eq!(spanning, 0, "no clip spans the split point");

			// Ripple-delete the second half of 开场 (id 11's split tail).
			let tail_id = v1
				.clips()
				.iter()
				.find(|c| c.range().start == Frame(120))
				.map(|c| c.id())
				.expect("split tail");
			engine.update(app, |engine, cx| {
				engine.delete_clip(tail_id, true, cx);
			});
			let v1 = engine.read(app).track(1).expect("V1");
			assert_eq!(v1.clips().len(), 2);
			// The following B-roll (was 240–600) shifted left by the removed
			// 120-frame tail: now starts at 120.
			let b_roll = v1
				.clips()
				.iter()
				.find(|c| c.id() == ClipId(12))
				.expect("B-roll clip");
			assert_eq!(
				b_roll.range().start,
				Frame(120),
				"ripple shifted B-roll left"
			);
		});
	}

	#[gpui::test]
	async fn demo_graph_has_six_nodes_and_five_edges(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			let engine = engine.read(app);
			let nodes = engine.nodes();
			let edges = engine.edges();

			assert_eq!(nodes.len(), 6, "media, transform, blur, mixer, viewer");
			assert_eq!(edges.len(), 5);

			// The chain ends at the viewer node, fed by the mixer.
			let viewer = nodes
				.iter()
				.find(|n| n.title() == "输出")
				.expect("viewer node");
			assert_eq!(viewer.inputs().len(), 1);
			assert!(viewer.outputs().is_empty());

			// Every edge endpoint references an existing port, and every port
			// connectivity flag matches the edge list.
			for edge in &edges {
				assert!(
					nodes.iter().any(|n| n.id() == edge.from_node()),
					"edge {} from-node exists",
					edge.id().0
				);
				assert!(
					nodes.iter().any(|n| n.id() == edge.to_node()),
					"edge {} to-node exists",
					edge.id().0
				);
			}
			for node in nodes {
				for port in node.inputs().into_iter().chain(node.outputs()) {
					let expected = engine
						.edges()
						.iter()
						.any(|e| e.from_port() == port.id() || e.to_port() == port.id());
					assert_eq!(port.is_connected(), expected, "port {}", port.id().0);
				}
			}
		});
	}

	#[gpui::test]
	async fn can_connect_enforces_direction_type_duplicates_and_cycles(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			let engine = engine.read(app);

			// A valid, still-free connection: Clip1.video → Transform.in (the
			// transform already takes the clip-0 path, so this would be the
			// second input).
			assert!(engine.can_connect(PortId(3), PortId(20)));
			// The existing connection is not offered again.
			assert!(
				!engine.can_connect(PortId(21), PortId(40)),
				"duplicate edge"
			);
			// Input → output is rejected (wrong direction).
			assert!(
				!engine.can_connect(PortId(20), PortId(1)),
				"wrong direction"
			);
			// A port cannot connect to itself.
			assert!(
				!engine.can_connect(PortId(42), PortId(40)),
				"self connection"
			);
			// Type mismatch: the clip's audio output is not a video signal.
			assert!(
				!engine.can_connect(PortId(4), PortId(20)),
				"audio cannot feed a video input"
			);
			// Unknown ports are rejected.
			assert!(!engine.can_connect(PortId(999), PortId(20)));
			assert!(!engine.can_connect(PortId(1), PortId(999)));

			// Cycle rule: the graph flows left-to-right (Clip → Transform →
			// Mixer → Viewer), so no connection can close a loop — but the
			// reachability helper behind the rule is exercised directly.
			assert!(engine.reaches(NodeId(0), NodeId(5)), "main chain path");
			assert!(!engine.reaches(NodeId(5), NodeId(0)), "no backward path");
			assert!(engine.reaches(NodeId(4), NodeId(4)));
		});
	}

	#[gpui::test]
	async fn node_edits_apply_to_the_model(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			// Move the viewer node.
			engine.update(app, |engine, cx| {
				engine.apply_node_graph_event(
					&NodeGraphEvent::NodeMoveRequested {
						nodes: vec![NodeId(5)],
						delta: point(px(100.0), px(-20.0)),
					},
					cx,
				);
			});
			let moved = engine.read(app).node(NodeId(5)).expect("viewer node");
			assert_eq!(moved.position(), point(px(980.0), px(110.0)));

			// Connect the B-roll clip's video to the transform's unused mask
			// input, then disconnect the transform → mixer edge.
			engine.update(app, |engine, cx| {
				engine.apply_node_graph_event(
					&NodeGraphEvent::ConnectionRequested {
						from: PortId(3),
						to: PortId(22),
					},
					cx,
				);
			});
			let engine_read = engine.read(app);
			assert_eq!(engine_read.edges().len(), 6);
			assert!(engine_read
				.port(PortId(22))
				.is_some_and(|p| p.is_connected()));
			let edges = engine_read.edges();
			let extra = edges
				.iter()
				.find(|e| e.to_port() == PortId(22))
				.expect("the new connection");

			engine.update(app, |engine, cx| {
				engine.apply_node_graph_event(
					&NodeGraphEvent::DisconnectionRequested { edge: extra.id() },
					cx,
				);
			});
			assert_eq!(engine.read(app).edges().len(), 5);
			assert!(
				!engine
					.read(app)
					.port(PortId(22))
					.is_some_and(|p| p.is_connected()),
				"mask input freed again"
			);

			// Deleting the blur node takes its incident edges with it.
			engine.update(app, |engine, cx| {
				engine.apply_node_graph_event(
					&NodeGraphEvent::DeleteRequested {
						nodes: vec![NodeId(3)],
						edges: vec![],
					},
					cx,
				);
			});
			let engine_read = engine.read(app);
			assert_eq!(engine_read.nodes().len(), 5);
			assert!(
				!engine_read
					.edges()
					.iter()
					.any(|e| e.to_node() == NodeId(3) || e.from_node() == NodeId(3)),
				"edges incident to the deleted node are removed"
			);
		});
	}

	#[gpui::test]
	async fn cpu_frame_is_cached_per_playhead_and_advances(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);

			// Same playhead → same cached image (Arc identity), so a paused
			// viewer never regenerates its picture.
			let a = engine.read(app).cpu_frame(Monitor::Program, app);
			let b = engine.read(app).cpu_frame(Monitor::Program, app);
			assert!(Arc::ptr_eq(&a, &b), "paused frame must be cached");

			// Advancing the playhead produces a different image: the sweep
			// moved, so playback is visible.
			engine.update(app, |engine, cx| {
				engine.step(Monitor::Program, 25, cx);
			});
			let c = engine.read(app).cpu_frame(Monitor::Program, app);
			assert!(!Arc::ptr_eq(&a, &c), "a new playhead frame regenerates");

			// The frame has the documented proxy size and opaque BGRA8 bytes.
			let size = c.size(0);
			assert_eq!(size.width, crate::oakui::frames::SYNTH_FRAME_WIDTH.into());
			assert_eq!(size.height, crate::oakui::frames::SYNTH_FRAME_HEIGHT.into());
			let bytes = c.as_bytes(0).expect("single frame");
			assert_eq!(
				bytes.len(),
				(crate::oakui::frames::SYNTH_FRAME_WIDTH
					* crate::oakui::frames::SYNTH_FRAME_HEIGHT
					* 4) as usize
			);
			assert!(bytes.chunks_exact(4).all(|px| px[3] == 255), "opaque alpha");
		});
	}

	// -----------------------------------------------------------------------
	// Sequence markers & work area (M12 P4): the mock state the 序列 menu
	// actions and the timeline ruler drag drive.
	// -----------------------------------------------------------------------

	#[gpui::test]
	async fn marker_menu_actions_add_and_remove_at_playhead(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			// Park the playhead at frame 40 (the program monitor's clock).
			engine.update(app, |engine, cx| {
				engine.request_frame(Monitor::Program, Frame(40), cx);
			});

			// 序列 → 添加标记: a marker appears at the playhead.
			engine.update(app, |engine, cx| engine.add_marker_at_playhead(cx));
			let markers = engine.read(app).markers();
			assert_eq!(markers.len(), 1);
			assert_eq!(markers[0].frame, Frame(40));

			// Adding again at the same frame is idempotent for the menu.
			engine.update(app, |engine, cx| engine.add_marker_at_playhead(cx));
			assert_eq!(engine.read(app).markers().len(), 1);

			// 序列 → 清除标记 removes it.
			engine.update(app, |engine, cx| engine.remove_marker_at_playhead(cx));
			assert!(engine.read(app).markers().is_empty());
		});
	}

	#[gpui::test]
	async fn workarea_menu_actions_set_and_clear(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			assert_eq!(engine.read(app).workarea(), None);

			// The menu's commit path (with an explicit old range).
			engine.update(app, |engine, cx| {
				engine.commit_workarea(Frame(0), Frame(100), Frame(20), Frame(80), cx);
			});
			assert_eq!(engine.read(app).workarea(), Some((Frame(20), Frame(80))));

			// The ruler-drag preview path (live, non-undoable).
			engine.update(app, |engine, cx| {
				engine.set_workarea_preview(Frame(25), Frame(75), cx);
			});
			assert_eq!(engine.read(app).workarea(), Some((Frame(25), Frame(75))));

			// 序列 → 清除工作区 disables it.
			engine.update(app, |engine, cx| engine.clear_workarea(cx));
			assert_eq!(engine.read(app).workarea(), None);
		});
	}

	#[gpui::test]
	async fn timeline_workarea_events_drive_the_mock_engine(cx: &mut TestAppContext) {
		cx.update(|app| {
			let engine = demo_engine(app);
			// The timeline widget's events land on the engine through
			// apply_timeline_event (the app shell's subscription).
			engine.update(app, |engine, cx| {
				engine.apply_timeline_event(
					&TimelineEvent::WorkAreaPreview {
						start: Frame(30),
						end: Frame(90),
					},
					cx,
				);
				engine.apply_timeline_event(
					&TimelineEvent::WorkAreaCommitted {
						start: Frame(30),
						end: Frame(90),
						old_start: Frame::ZERO,
						old_end: Frame(100),
					},
					cx,
				);
			});
			assert_eq!(engine.read(app).workarea(), Some((Frame(30), Frame(90))));
		});
	}
}
