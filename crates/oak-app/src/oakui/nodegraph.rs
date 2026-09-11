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

//! M12 P2: the real node-graph surface.
//!
//! Builds the gpui node-graph data (`RealNode` / `RealPort` / `RealEdge`)
//! from the CURRENT SEQUENCE's graph (M14 R3: the direct oaknode graph walk
//! — the app links the module rlibs and reads the project graph itself):
//!
//! - the sequence node becomes the output card (rightmost);
//! - every clip block becomes a card titled with its label (or "Clip");
//! - effects (everything else in the sequence graph) sit between;
//! - footage nodes (the media) feed the clip's `tex_in` from the left;
//! - declared inputs become input ports (id string as the label); every
//!   node exposes one "out" output port (the module declares no outputs;
//!   edges are enumerated from `Graph::output_connections`);
//! - REAL edges connect the source node's main output to the target
//!   node's input port; a synthesized "clip → output" wire per clip
//!   connects the clip's main output to the sequence's `tex_in` (the
//!   module's place-block flow never wires blocks to the sequence node —
//!   the C++ reaches it through track nodes, which do not exist here);
//! - positions come from the sequence node's context position map, with a
//!   deterministic role-grid fallback layout (footage | effects | clips |
//!   output) when a node has no entry yet — the first drag persists
//!   through the undoable position setter.
//!
//! Identity mapping: `NodeId` = the module node identity (stable across
//! frames). `PortId` packs `(node identity, kind, index)` — input port
//! `(id << 4) | (index << 1)`, output port `(id << 4) | 1`. Identities
//! are arena slots (low index bits, zero generation for most nodes), so
//! the shifts are injective.
//!
//! Structural timeline plumbing (track lists, tracks, gaps, transitions)
//! is NOT displayed: those nodes carry no graph edges in the module world
//! and would only add empty cards; the displayed graph is the media chain
//! clip → effects → output the C++ node editor centers on.

use std::collections::HashMap;

use gpui::node_graph::{
	EdgeData, EdgeId, NodeData, NodeId, PortData, PortId, PortDataType, PortKind,
};
use gpui::{hsla, point, px, Hsla, Pixels, Point, SharedString};

use oak_node::id::NodeId as DomainNodeId;

use crate::oakui::graphops::{self, ProjectRef};

/// The sequence node type id (the graph's output card).
const TYPE_ID_SEQUENCE: &str = "org.olivevideoeditor.Olive.sequence";
/// Clip block type id (the graph's clip cards).
const TYPE_ID_CLIP_BLOCK: &str = "org.olivevideoeditor.Olive.clipblock";
/// Footage node type id (the graph's media cards).
const TYPE_ID_FOOTAGE: &str = "org.olivevideoeditor.Olive.footage";
/// Structural timeline nodes never shown in the node editor.
const TYPE_ID_TRACK: &str = "org.olivevideoeditor.Olive.track";
const TYPE_ID_TRACK_LIST: &str = "org.olivevideoeditor.Olive.tracklist";
const TYPE_ID_GAP_BLOCK: &str = "org.olivevideoeditor.Olive.gapblock";
const TYPE_ID_TRANSITION_BLOCK: &str = "org.olivevideoeditor.Olive.transitionblock";

/// The "video" wire type used by the real graph (the module exposes no
/// per-input type names; all node ports are treated as video).
fn video_type() -> PortDataType {
	PortDataType::new("video", hsla(0.55, 0.75, 0.6, 1.0))
}

/// The "output" wire type for the module's implicit outputs.
fn out_type() -> PortDataType {
	PortDataType::new("video", hsla(0.08, 0.7, 0.55, 1.0))
}

/// Pack a node identity + kind + index into a global `PortId`.
fn port_id(node: u64, kind: PortKind, index: u32) -> PortId {
	match kind {
		PortKind::Input => PortId((node << 4) | ((index as u64) << 1)),
		PortKind::Output => PortId((node << 4) | 1),
	}
}

/// Unpack a `PortId` into `(node identity, kind, index)`.
fn unpack_port(id: PortId) -> (u64, PortKind, u32) {
	let raw = id.0;
	let node = raw >> 4;
	if raw & 1 == 1 {
		(node, PortKind::Output, 0)
	} else {
		(node, PortKind::Input, ((raw >> 1) & 0x7) as u32)
	}
}

/// A stable REAL edge id from `(from node, to node, input id)` (FNV-1a,
/// high bit masked so it can never collide with the synthesized-wire tag
/// [`is_output_wire`] reserves).
fn real_edge_id(from: u64, to: u64, input_id: &str) -> EdgeId {
	let mut h: u64 = 0xcbf29ce484222325;
	for &b in [from.to_le_bytes(), to.to_le_bytes()].concat().iter() {
		h ^= b as u64;
		h = h.wrapping_mul(0x100000001b3);
	}
	for &b in input_id.as_bytes() {
		h ^= b as u64;
		h = h.wrapping_mul(0x100000001b3);
	}
	EdgeId(h & 0x7fff_ffff_ffff_ffff)
}

/// The synthesized "clip → output" wire's id: tagged with the high bit so
/// the app can tell structural wires apart from real edges (whose ids are
/// masked below the tag — see [`real_edge_id`]).
fn output_wire_id(clip: u64) -> EdgeId {
	EdgeId(0x8000_0000_0000_0000 | real_edge_id(clip, clip, "out").0)
}

/// Whether `id` is a synthesized structural wire (never a real edge).
pub fn is_output_wire(id: EdgeId) -> bool {
	id.0 & 0x8000_0000_0000_0000 != 0
}

/// A node card in the real graph.
#[derive(Debug, Clone)]
pub struct RealNode {
	/// The module node identity.
	pub id: NodeId,
	/// Card title.
	pub title: SharedString,
	/// Graph-space position.
	pub position: Point<Pixels>,
	/// Input ports (top to bottom).
	pub inputs: Vec<RealPort>,
	/// The implicit output port (`out`); empty on the output endpoint,
	/// which is a pure sink.
	pub outputs: Vec<RealPort>,
	/// Category accent.
	pub header_color: Option<Hsla>,
	/// Collapsed state.
	pub collapsed: bool,
	/// Enabled state.
	pub enabled: bool,
	/// The virtual endpoint role, when this card is one of the fixed
	/// endpoint pair (`None` for ordinary effect / media cards).
	pub endpoint: Option<GraphEndpoint>,
}

impl NodeData for RealNode {
	type Port = RealPort;

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

/// A port on a real node.
#[derive(Debug, Clone)]
pub struct RealPort {
	/// Global port id.
	pub id: PortId,
	/// Direction.
	pub kind: PortKind,
	/// The node input id this port fronts ("" for the implicit output) —
	/// edges resolve to ports by this, not by the display label.
	pub input_id: SharedString,
	/// Port label.
	pub label: SharedString,
	/// Wire type.
	pub data_type: PortDataType,
	/// Whether an edge is attached.
	pub connected: bool,
}

impl PortData for RealPort {
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

/// A wire between two real nodes.
#[derive(Debug, Clone)]
pub struct RealEdge {
	/// Stable edge id.
	pub id: EdgeId,
	/// Source node.
	pub from_node: NodeId,
	/// Source output port.
	pub from_port: PortId,
	/// Target node.
	pub to_node: NodeId,
	/// Target input port.
	pub to_port: PortId,
}

impl EdgeData for RealEdge {
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

/// Deterministic header accents per node identity.
fn node_color(ident: u64) -> Hsla {
	let hues = [0.55f32, 0.6, 0.08, 0.3, 0.78, 0.45, 0.9, 0.15];
	hsla(hues[(ident as usize) % hues.len()], 0.5, 0.35, 1.0)
}

/// Whether a sequence-graph node should be shown in the node editor: the
/// media chain (sequence output, clip blocks, footage, effects) only;
/// structural timeline plumbing (tracks, track lists, gaps, transitions)
/// is hidden.
fn is_displayed(type_id: &str) -> bool {
	!matches!(
		type_id,
		TYPE_ID_TRACK | TYPE_ID_TRACK_LIST | TYPE_ID_GAP_BLOCK | TYPE_ID_TRANSITION_BLOCK
	)
}

/// The virtual endpoint pair every project graph carries: `GraphInput`
/// (the evaluation walk's root) and `GraphOutput` (where the frame is
/// read). Endpoints are shown as fixed cards — the panel hides their edit
/// entries and filters delete requests naming them, and the graph model
/// itself refuses to remove or copy the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEndpoint {
	/// `GraphInput`: data enters through `feed_in`, leaves through `tex_out`.
	Input,
	/// `GraphOutput`: every branch converges on `tex_in`; a pure sink.
	Output,
}

/// Whether `type_id` names one of the virtual endpoints, and which.
pub fn endpoint_of_type_id(type_id: &str) -> Option<GraphEndpoint> {
	use oak_node::nodes::graphendpoints::{GRAPH_INPUT_TYPE_ID, GRAPH_OUTPUT_TYPE_ID};
	if type_id == GRAPH_INPUT_TYPE_ID {
		Some(GraphEndpoint::Input)
	} else if type_id == GRAPH_OUTPUT_TYPE_ID {
		Some(GraphEndpoint::Output)
	} else {
		None
	}
}

/// The fixed header accent of an endpoint card: the pair reads as
/// structural furniture, not as one more effect (deliberately outside the
/// [`node_color`] palette — and the demo graph's hand-picked hues).
fn endpoint_color(endpoint: GraphEndpoint) -> Hsla {
	match endpoint {
		GraphEndpoint::Input => hsla(0.40, 0.55, 0.35, 1.0),
		GraphEndpoint::Output => hsla(0.85, 0.65, 0.45, 1.0),
	}
}

/// A displayed node: its domain id plus its type id.
struct TypedNode {
	/// The node's domain id.
	id: DomainNodeId,
	/// The node's identity.
	ident: u64,
	/// The node's type id.
	type_id: String,
}

/// The displayable nodes of ONE clip's context chain — the per-clip node
/// view the editor shows while that clip is selected: the clip block node
/// plus its effect chain in signal order. The chain (from
/// [`effectchain::chain`]) walks the `tex_in`/effect-input link all the way
/// to the media source, so it already contains the footage node feeding the
/// clip (`[footage, effect1, effect2, ...]`); concatenating the clip yields
/// the full `footage → effects → clip` chain. The sequence node is NOT part
/// of a clip's context — the chain is self-contained, so the filtered view
/// shows no output card and no synthesized clip→output wires.
fn clip_context_nodes(g: &oak_node::graph::Graph, clip: DomainNodeId) -> Vec<TypedNode> {
	let mut out: Vec<TypedNode> = Vec::new();
	// The clip block card (the chain's output end).
	out.push(TypedNode {
		id: clip,
		ident: clip.identity(),
		type_id: TYPE_ID_CLIP_BLOCK.into(),
	});
	// The media → effects chain, closest-to-source first (the same order
	// the effect stack lists cards in).
	for node in crate::oakui::effectchain::chain(g, clip) {
		out.push(TypedNode {
			id: node,
			ident: node.identity(),
			type_id: graphops::node_type_id(g, node),
		});
	}
	out
}

/// The displayable nodes of the sequence's graph, in identity order (the
/// sequence node itself exactly once).
fn graph_nodes(g: &oak_node::graph::Graph, seq: DomainNodeId) -> Vec<TypedNode> {
	let mut out: Vec<TypedNode> = g
		.node_ids()
		.into_iter()
		.filter_map(|id| {
			let type_id = graphops::node_type_id(g, id);
			if !is_displayed(&type_id) {
				return None;
			}
			Some(TypedNode {
				id,
				ident: id.identity(),
				type_id,
			})
		})
		.filter(|n| n.id != seq)
		.collect();
	// The sequence node itself: exactly one output card.
	out.push(TypedNode {
		id: seq,
		ident: seq.identity(),
		type_id: TYPE_ID_SEQUENCE.into(),
	});
	out.sort_by_key(|n| n.ident);
	out
}

/// The context position of `node` in the sequence's map, or the (0,0)
/// sentinel the fallback layout replaces.
fn context_position(
	g: &oak_node::graph::Graph,
	seq: DomainNodeId,
	node: DomainNodeId,
) -> Point<Pixels> {
	let placed = g.get(node).and_then(|e| {
		e.core
			.context_positions
			.iter()
			.find(|(c, _, _)| *c == seq)
			.map(|(_, pos, _)| *pos)
	});
	match placed {
		Some((x, y)) if x != 0.0 || y != 0.0 => point(px(x as f32), px(y as f32)),
		_ => point(px(0.0), px(0.0)),
	}
}

/// Build the (nodes, edges) snapshot of the sequence's node graph, or of a
/// single clip's context chain when `clip` is `Some` (the per-clip view the
/// editor shows while that clip is selected).
fn build_graph_impl(
	project: &ProjectRef,
	seq: DomainNodeId,
	clip: Option<DomainNodeId>,
) -> (Vec<RealNode>, Vec<RealEdge>) {
	let g = graphops::lock(project);
	let g = &g.graph;
	let mut nodes = Vec::new();
	let mut edges = Vec::new();
	if !g.is_valid(seq) {
		return (nodes, edges);
	}
	if let Some(clip) = clip {
		if !g.is_valid(clip) {
			return (nodes, edges);
		}
	}
	let seq_ident = seq.identity();
	let seq_label = graphops::node_label(g, seq);
	let seq_name = g
		.get(seq)
		.map(|e| e.behavior.name().to_string())
		.unwrap_or_default();

	// The displayable node set: the whole sequence graph, or one clip's
	// context chain (footage → effects → clip).
	let all = match clip {
		Some(clip) => clip_context_nodes(g, clip),
		None => graph_nodes(g, seq),
	};
	// The synthesized "clip → output" wire only exists in the full-sequence
	// view: a per-clip context has no output card to wire into.
	let with_output_wires = clip.is_none();

	// Build every card's ports first (inputs, the implicit output), so
	// real edges can resolve their target port index by matching the
	// input id against the already-built cards.
	let mut built: Vec<(TypedNode, RealNode)> = Vec::new();
	for typed in all {
		let ident = typed.ident;
		let title = if typed.type_id == TYPE_ID_SEQUENCE {
			if seq_label.is_empty() {
				seq_name.clone()
			} else {
				seq_label.clone()
			}
		} else {
			let label = graphops::node_label(g, typed.id);
			let name = g
				.get(typed.id)
				.map(|e| e.behavior.name().to_string())
				.unwrap_or_default();
			if label.is_empty() {
				name
			} else {
				label
			}
		};
		let endpoint = endpoint_of_type_id(&typed.type_id);

		// Inputs: only stream inputs (texture / samples) become ports — an
		// OFX plugin declares every PARAMETER as an input too, and those
		// belong to the params panel, not the card (the garbled wall of
		// "NatronOfxParamProcess*" rows). Secret/internal inputs are hidden
		// outright. The port label is the human-readable display name (the
		// OFX param label), the wire lookup uses the id.
		let input_decls: Vec<(usize, String, String)> = g
			.get(typed.id)
			.map(|e| {
				e.core
					.inputs
					.iter()
					.enumerate()
					.filter(|(_, i)| {
						matches!(
							i.value_type,
							oak_node::value::ValueType::Texture
								| oak_node::value::ValueType::Samples
						) && i.flags & oak_node::input::flags::HIDDEN == 0
					})
					.map(|(idx, i)| {
						let label = if i.display_name.is_empty() {
							i.id.clone()
						} else {
							i.display_name.clone()
						};
						(idx, i.id.clone(), label)
					})
					.collect()
			})
			.unwrap_or_default();
		let mut inputs = Vec::with_capacity(input_decls.len());
		for (idx, id_str, label) in &input_decls {
			if id_str.is_empty() {
				continue;
			}
			inputs.push(RealPort {
				id: port_id(ident, PortKind::Input, *idx as u32),
				kind: PortKind::Input,
				input_id: id_str.clone().into(),
				label: label.clone().into(),
				data_type: video_type(),
				connected: g.is_input_connected(typed.id, id_str, -1),
			});
		}

		// The implicit output port. The output endpoint is a sink — nothing
		// is wired out of it, so it gets no port; the input endpoint keeps
		// its `tex_out` (the walk's root output).
		let outputs = if endpoint == Some(GraphEndpoint::Output) {
			Vec::new()
		} else {
			let out_count = g.output_connections(typed.id).len();
			vec![RealPort {
				id: port_id(ident, PortKind::Output, 0),
				kind: PortKind::Output,
				input_id: SharedString::new_static(""),
				label: SharedString::new_static("out"),
				data_type: out_type(),
				connected: out_count > 0,
			}]
		};

		let position = context_position(g, seq, typed.id);
		built.push((
			typed,
			RealNode {
				id: NodeId(ident),
				title: title.into(),
				position,
				inputs,
				outputs,
				header_color: Some(match endpoint {
					Some(endpoint) => endpoint_color(endpoint),
					None => node_color(ident),
				}),
				collapsed: false,
				enabled: true,
				endpoint,
			},
		));
	}

	// Outgoing REAL edges: every source node's output connections, with
	// the target port resolved to the index of the input whose id matches
	// (the module stores edges by input id; the index may differ from 0 —
	// e.g. a clip's `tex_in` sits after `enabled_in`). Edges whose target
	// is not part of THIS view are skipped: a per-clip context ends at the
	// clip, so its outgoing sequence/track edges are not part of the chain.
	let mut node_edges: Vec<(u64, Vec<RealEdge>)> = Vec::new();
	for (typed, _) in &built {
		let mut edges_of = Vec::new();
		for (to, conn_id, _element) in g.output_connections(typed.id) {
			if !g.is_valid(to) || conn_id.is_empty() {
				continue;
			}
			let to_ident = to.identity();
			if !built.iter().any(|(t, _)| t.ident == to_ident) {
				continue;
			}
			// The port id packs the DECLARATION index of the input (stable
			// across the param filtering), so resolve the edge to the
			// port's own id, not a recomputed index.
			let to_port = built
				.iter()
				.find(|(t, _)| t.ident == to_ident)
				.and_then(|(_, n)| n.inputs.iter().find(|p| p.input_id.as_ref() == conn_id))
				.map(|p| p.id)
				.unwrap_or_else(|| port_id(to_ident, PortKind::Input, 0));
			edges_of.push(RealEdge {
				id: real_edge_id(typed.ident, to_ident, &conn_id),
				from_node: NodeId(typed.ident),
				from_port: port_id(typed.ident, PortKind::Output, 0),
				to_node: NodeId(to_ident),
				to_port,
			});
		}
		node_edges.push((typed.ident, edges_of));
	}

	// Fallback layout: nodes without a persisted context position get a
	// deterministic role grid — footage | effects | clips | output as
	// columns, a per-role row counter as the row (the same grid the
	// C++-era node editor lays chains out on). Nodes the user has
	// already dragged keep their persisted position. `fallback_base`
	// mirrors this grid so the first drag moves from the displayed
	// position rather than the origin.
	let mut row_at_role: HashMap<u32, u32> = HashMap::new();
	for (typed, node) in built.iter_mut() {
		if node.position != point(px(0.0), px(0.0)) {
			continue;
		}
		let role = role_of(&typed.type_id, typed.ident == seq_ident);
		let row = row_at_role.entry(role).or_insert(0);
		let x = 40.0 + (role as f32) * 260.0;
		let y = 40.0 + (*row as f32) * 180.0;
		*row += 1;
		node.position = point(px(x), px(y));
	}

	// Assemble: every built card + its real edges, plus the synthesized
	// "clip → output" wires (each clip's main output into the sequence's
	// `tex_in`, its first declared input) — full-sequence view only.
	let clip_input = port_id(seq_ident, PortKind::Input, 0);
	for (typed, node) in built {
		if with_output_wires && typed.type_id == TYPE_ID_CLIP_BLOCK {
			edges.push(RealEdge {
				id: output_wire_id(typed.ident),
				from_node: node.id,
				from_port: port_id(typed.ident, PortKind::Output, 0),
				to_node: NodeId(seq_ident),
				to_port: clip_input,
			});
		}
		if let Some((_, real)) = node_edges.iter().find(|(id, _)| *id == typed.ident) {
			edges.extend(real.iter().cloned());
		}
		nodes.push(node);
	}
	(nodes, edges)
}

/// Build the (nodes, edges) snapshot of the sequence's node graph.
pub fn build_graph(project: &ProjectRef, seq: DomainNodeId) -> (Vec<RealNode>, Vec<RealEdge>) {
	build_graph_impl(project, seq, None)
}

/// Build the (nodes, edges) snapshot of ONE clip's context chain — the
/// per-clip node view the editor shows while that clip is selected. The
/// chain is footage → effects → clip; the sequence output card and the
/// synthesized wires are not part of a clip's context. An invalid clip
/// identity yields an empty graph.
pub fn build_graph_for_clip(
	project: &ProjectRef,
	seq: DomainNodeId,
	clip_ident: u64,
) -> (Vec<RealNode>, Vec<RealEdge>) {
	let Some(clip) = graphops::id_of(clip_ident) else {
		return (Vec::new(), Vec::new());
	};
	build_graph_impl(project, seq, Some(clip))
}

/// The role column of a displayed node (footage | effects | clips |
/// output); drives the fallback grid's x position.
fn role_of(type_id: &str, is_output: bool) -> u32 {
	if is_output {
		3
	} else if type_id == TYPE_ID_FOOTAGE {
		0
	} else if type_id == TYPE_ID_CLIP_BLOCK {
		2
	} else {
		1 // effects
	}
}

/// The provisional position the builder assigns to an unplaced node: the
/// role column and the per-role row among the OTHER unplaced nodes in
/// sorted display order (mirrors the fallback grid in `build_graph`).
/// `apply_edit` uses it so the first drag release writes
/// `(displayed base + delta)` instead of `(origin + delta)`.
fn fallback_base(g: &oak_node::graph::Graph, seq: DomainNodeId, ident: u64) -> (f64, f64) {
	let seq_ident = seq.identity();
	let all = graph_nodes(g, seq);
	let target = all
		.iter()
		.find(|n| n.ident == ident)
		.expect("the moved node is part of the displayed graph");
	let target_role = role_of(&target.type_id, ident == seq_ident);
	let mut row: u32 = 0;
	for n in &all {
		if n.ident == ident {
			break;
		}
		if role_of(&n.type_id, n.ident == seq_ident) != target_role {
			continue;
		}
		// Placed nodes do not consume a row.
		let placed = g
			.get(n.id)
			.and_then(|e| {
				e.core
					.context_positions
					.iter()
					.find(|(c, _, _)| *c == seq)
					.map(|(_, pos, _)| *pos)
			})
			.map(|(x, y)| x != 0.0 || y != 0.0)
			.unwrap_or(false);
		if !placed {
			row += 1;
		}
	}
	(
		40.0 + f64::from(target_role * 260),
		40.0 + f64::from(row * 180),
	)
}

/// Resolve a domain node id from a widget identity, validating it against
/// the graph.
fn find_node(g: &oak_node::graph::Graph, ident: u64) -> Result<DomainNodeId, String> {
	let Some(id) = graphops::id_of(ident) else {
		return Err(format!("node {ident} not found"));
	};
	if !g.is_valid(id) {
		return Err(format!("node {ident} not found"));
	}
	Ok(id)
}

// ---------------------------------------------------------------------------
// Connection rules and edit application
// ---------------------------------------------------------------------------

/// Whether connecting output port `from` to input port `to` is valid in
/// the graph: output → input, distinct nodes, and the target input must
/// exist and be free. The sequence node's inputs are connectable like any
/// other (its `tex_in` starts unconnected; a user wire replaces the
/// synthesized one once the graph grows real edges).
pub fn can_connect(project: &ProjectRef, from: PortId, to: PortId) -> bool {
	let (from_node, from_kind, _) = unpack_port(from);
	let (to_node, to_kind, to_index) = unpack_port(to);
	if from_kind != PortKind::Output || to_kind != PortKind::Input {
		return false;
	}
	if from_node == to_node {
		return false;
	}
	let guard = graphops::lock(project);
	let g = &guard.graph;
	let Ok(node) = find_node(g, to_node) else {
		return false;
	};
	let Some(entry) = g.get(node) else {
		return false;
	};
	let Some(id_str) = entry.core.inputs.get(to_index as usize).map(|i| i.id.clone()) else {
		return false;
	};
	if id_str.is_empty() {
		return false;
	}
	!g.is_input_connected(node, &id_str, -1)
}

/// Apply a node-graph edit to the sequence's graph (undoable through the
/// global undo stack).
pub fn apply_edit(
	project: &ProjectRef,
	seq: DomainNodeId,
	edit: &gpui::node_graph::NodeGraphEvent,
) -> Result<(), String> {
	use gpui::node_graph::NodeGraphEvent;
	match edit {
		NodeGraphEvent::ConnectionRequested { from, to } => {
			let (from_node, from_kind, _) = unpack_port(*from);
			let (to_node, to_kind, to_index) = unpack_port(*to);
			if from_kind != PortKind::Output || to_kind != PortKind::Input {
				return Err("connection endpoints must be output → input".into());
			}
			let (src, dst, id_str) = {
				let guard = graphops::lock(project);
				let g = &guard.graph;
				let src = find_node(g, from_node)?;
				let dst = find_node(g, to_node)?;
				let id_str = g
					.get(dst)
					.and_then(|e| e.core.inputs.get(to_index as usize))
					.map(|i| i.id.clone())
					.filter(|s| !s.is_empty());
				let Some(id_str) = id_str else {
					return Err("target input missing".into());
				};
				(src, dst, id_str)
			};
			let cmd = graphops::connect_command(project, src, dst, &id_str)?;
			graphops::push_command(cmd, "Connect Nodes")
		}
		NodeGraphEvent::DisconnectionRequested { edge } => {
			if is_output_wire(*edge) {
				// The synthesized clip → output wire is structural; the
				// graph has no such edge to remove.
				return Ok(());
			}
			let (dst, conn_id) = {
				let guard = graphops::lock(project);
				let g = &guard.graph;
				let mut found = None;
				'outer: for (typed_from, to, conn_id, _) in g.output_connections_all() {
					if real_edge_id(typed_from.identity(), to.identity(), &conn_id).0 == edge.0 {
						found = Some((to, conn_id));
						break 'outer;
					}
				}
				found.ok_or_else(|| format!("edge {} not found", edge.0))?
			};
			let cmd = graphops::disconnect_command(project, dst, &conn_id)?;
			graphops::push_command(cmd, "Disconnect Nodes")
		}
		NodeGraphEvent::NodeMoveRequested { nodes, delta } => {
			for node in nodes {
				let (id, base) = {
					let guard = graphops::lock(project);
					let g = &guard.graph;
					let id = find_node(g, node.0)?;
					let placed = g
						.get(id)
						.and_then(|e| {
							e.core
								.context_positions
								.iter()
								.find(|(c, _, _)| *c == seq)
								.map(|(_, pos, _)| *pos)
						})
						.filter(|(x, y)| *x != 0.0 || *y != 0.0);
					let base = match placed {
						Some(pos) => pos,
						None => fallback_base(g, seq, node.0),
					};
					(id, base)
				};
				let cmd = graphops::set_context_position_command(
					project,
					id,
					seq,
					base.0 + f64::from(delta.x.as_f32()),
					base.1 + f64::from(delta.y.as_f32()),
				)?;
				graphops::push_command(cmd, "Set Position")?;
			}
			Ok(())
		}
		NodeGraphEvent::DeleteRequested { nodes, .. } => {
			for node in nodes {
				// The output node is the graph's context; never deleted.
				if node.0 == seq.identity() {
					continue;
				}
				let id = {
					let guard = graphops::lock(project);
					find_node(&guard.graph, node.0)?
				};
				graphops::remove_node(project, id)?;
			}
			Ok(())
		}
		_ => Ok(()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::oakui::effectchain;
	use crate::oakui::graphops;

	/// Serializes with the other app test modules (the process-global undo
	/// stack).
	fn stack_lock() -> std::sync::MutexGuard<'static, ()> {
		crate::oakui::graphops::test_lock()
	}

	/// A project with a sequence and a clip whose chain runs footage → one
	/// effect → clip: `(project, seq, clip, effect, footage)`.
	fn project_with_chained_clip() -> (
		ProjectRef,
		DomainNodeId,
		DomainNodeId,
		DomainNodeId,
		DomainNodeId,
	) {
		let project = graphops::create_project();
		let seq = graphops::create_sequence(&project, "Chain");
		let clip = {
			let mut g = graphops::lock(&project);
			let (core, behavior) = oak_node::block::clip_create();
			g.graph.add_node(core, behavior)
		};
		// Insert the effect on the bare clip first (the chain is empty, so
		// the insert rewires nothing and connects the effect to the clip's
		// `tex_in`).
		let ty = effectchain::addable_effects()
			.into_iter()
			.next()
			.expect("the factory registers at least one video effect")
			.type_id;
		let effect = effectchain::insert(&project, clip, 0, &ty).expect("chain the effect");
		// Then feed the effect's media input from a footage node: the chain
		// becomes footage → effect → clip.
		let effect_input = graphops::lock(&project)
			.graph
			.get(effect)
			.map(|e| e.core.effect_input.clone())
			.unwrap_or_default();
		let footage = {
			let mut g = graphops::lock(&project);
			let (core, behavior) = oak_node::footage::FootageBehavior::create();
			g.graph.add_node(core, behavior)
		};
		{
			let mut g = graphops::lock(&project);
			g.graph
				.connect(footage, effect, &effect_input, -1)
				.expect("wire the footage into the effect");
		}
		(project, seq, clip, effect, footage)
	}

	/// The per-clip builder yields exactly the clip's context chain
	/// (footage → effect → clip): no sequence output card, no synthesized
	/// wires, and the chain's real edges connect footage → effect → clip.
	/// The full-sequence builder still shows the output card and wires.
	#[test]
	fn clip_context_build_is_the_clip_chain_only() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, seq, clip, effect, footage) = project_with_chained_clip();

		let (nodes, edges) = build_graph_for_clip(&project, seq, clip.identity());
		let ids: Vec<u64> = nodes.iter().map(|n| n.id.0).collect();
		assert_eq!(ids.len(), 3, "clip + effect + footage (got {ids:?})");
		assert!(ids.contains(&clip.identity()), "the clip node is present");
		assert!(ids.contains(&effect.identity()), "the effect node is present");
		assert!(ids.contains(&footage.identity()), "the footage node is present");
		assert!(
			!ids.contains(&seq.identity()),
			"the sequence output card is not part of a clip's context"
		);

		// A per-clip view carries no synthesized clip→output wires.
		assert!(
			edges.iter().all(|e| !is_output_wire(e.id)),
			"the per-clip view has no synthesized output wires"
		);
		// The chain's real edges: footage feeds the effect, which feeds the
		// clip.
		assert!(
			edges.iter().any(|e| e.from_node == NodeId(footage.identity())
				&& e.to_node == NodeId(effect.identity())),
			"footage feeds the effect"
		);
		assert!(
			edges.iter().any(|e| e.from_node == NodeId(effect.identity())
				&& e.to_node == NodeId(clip.identity())),
			"the effect feeds the clip"
		);

		// The full-sequence view keeps the output card and the synthesized
		// clip→output wire.
		let (full, full_edges) = build_graph(&project, seq);
		assert!(
			full.iter().any(|n| n.id == NodeId(seq.identity())),
			"the full graph shows the sequence output card"
		);
		assert!(
			full_edges.iter().any(|e| is_output_wire(e.id)),
			"the full graph synthesizes the clip→output wire"
		);

		oak_undo::global::clear().unwrap();
	}

	/// Every project graph carries the virtual endpoint pair: the full view
	/// shows both as marked cards (the output endpoint as a pure sink), and
	/// a clip's context chain shows neither.
	#[test]
	fn endpoints_build_as_marked_cards() {
		let _g = stack_lock();
		oak_undo::global::clear().unwrap();
		let (project, seq, clip, _, _) = project_with_chained_clip();

		let (nodes, _edges) = build_graph(&project, seq);
		assert_eq!(
			nodes.iter().filter(|n| n.endpoint.is_some()).count(),
			2,
			"exactly one endpoint pair is displayed"
		);
		let input = nodes
			.iter()
			.find(|n| n.endpoint == Some(GraphEndpoint::Input))
			.expect("the input endpoint card");
		let output = nodes
			.iter()
			.find(|n| n.endpoint == Some(GraphEndpoint::Output))
			.expect("the output endpoint card");
		assert!(
			!input.outputs.is_empty(),
			"the input endpoint keeps its `tex_out`"
		);
		assert!(
			output.outputs.is_empty(),
			"the output endpoint is a sink and has no output port"
		);
		assert!(
			!input.title.is_empty() && !output.title.is_empty(),
			"endpoint cards carry their behavior name as the title"
		);

		// The per-clip chain view contains no endpoint cards.
		let (chain, _) = build_graph_for_clip(&project, seq, clip.identity());
		assert!(
			chain.iter().all(|n| n.endpoint.is_none()),
			"a clip's context chain contains no endpoints"
		);

		oak_undo::global::clear().unwrap();
	}
}
