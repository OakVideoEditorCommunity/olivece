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

//! OCIO LUT file node (C++ `src/node/src/color/ociolut/ociolut.{h,cpp}`,
//! `olive::OCIOLutNode`).
//!
//! Note: OpenColorIO itself is never linked here; it is reached through
//! the color manager (`crate::colormanager`) and the oakrender bridge
//! (oakrender, opaque handles), like the C++ node's
//! `oakrender_color_processor_create_lut` / `oakrender_lut_*` calls.

use std::sync::Mutex;

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

use crate::nodes::ociobase::OcioBase;

/// LUT file input id (C++ `k_file_input`). Type: file; default: empty
/// string; flags: not-keyframable, not-connectable; properties:
/// `filter = "LUT Files (*.<ext> ...);;All Files (*)"` (extensions from
/// `oakrender_lut_supported_extension_at`, `*.*` fallback),
/// `placeholder = "Select a LUT file"`, `lut_library = true`.
pub const FILE_INPUT: &str = "lut_file_in";

/// Direction combo input id (C++ `k_direction_input`). Type: combo;
/// default `0` (forward); flags: not-keyframable, not-connectable.
/// Combo strings (set in `retranslate`): "Forward", "Inverse".
pub const DIRECTION_INPUT: &str = "lut_dir_in";

/// Deferred processor-generation state (C++ `mutable` members
/// `gen_mutex_`, `processor_dirty_`, `last_path_`, `last_direction_`,
/// `last_processor_`, `last_error_`). Grouped behind one mutex: the C++
/// mutable-in-const-method pattern maps to interior mutability here,
/// and the C++ code already serializes all of these under `gen_mutex_`.
struct ProcessorState {
	/// Regeneration pending (C++ `processor_dirty_`, starts `true`).
	dirty: bool,
	/// Path of the LUT the cached processor was built from (C++
	/// `last_path_`).
	last_path: String,
	/// Direction the cached processor was built for (C++
	/// `last_direction_`, starts `-1`).
	last_direction: i64,
	/// Cached processor for change detection (C++ `last_processor_`);
	/// shared with the base's active processor slot.
	last_processor: Option<std::sync::Arc<oak_core::color::ColorProcessor>>,
	/// Human-readable reason no LUT processor is active (C++
	/// `last_error_`); empty when a valid processor is in use or no LUT
	/// file has been selected yet.
	last_error: String,
}

impl Default for ProcessorState {
	/// Fresh state: dirty, empty path, no cached processor or error.
	fn default() -> Self {
		ProcessorState {
			dirty: true,
			last_path: String::new(),
			last_direction: -1,
			last_processor: None,
			last_error: String::new(),
		}
	}
}

/// OCIO LUT node. Applies a LUT file through OpenColorIO.
pub struct OCIOLutNode {
	/// Shared OCIO base state (C++ base class `OCIOBaseNode`).
	base: OcioBase,
	/// Processor generation cache/lock (C++ `gen_mutex_` + the mutable
	/// `last_*` members).
	state: Mutex<ProcessorState>,
}

impl OCIOLutNode {
	/// Human-readable description of why no LUT processor is active
	/// (C++ `last_error()`); empty when a valid LUT processor is in use
	/// or no LUT file has been selected yet.
	#[allow(dead_code)] // C++ parity accessor; read back by the tests.
	pub fn last_error(&self) -> String {
		self.state.lock().unwrap().last_error.clone()
	}

	/// Record a new error string (C++ `set_last_error()`); no-op when
	/// unchanged. The Qt version surfaced the error on the main-window
	/// status bar; here it is only recorded and read back via
	/// [`Self::last_error`].
	#[allow(dead_code)] // C++ parity setter; the tests feed it through
	// `last_error`.
	fn set_last_error(&self, error: &str) {
		let mut state = self.state.lock().unwrap();
		if state.last_error == error {
			return;
		}
		state.last_error = error.to_string();
	}

	/// Direction combo value with legacy fallback (C++ file-static
	/// `read_direction_input()`): integer combo value, or — for old
	/// serializers that stored the combo as a string — case-insensitive
	/// "forward"/"0" -> 0 and "inverse"/"1" -> 1, defaulting to 0 with a
	/// stderr warning for anything else.
	fn read_direction_input(core: &NodeCore) -> i64 {
		match &core.standard_value(DIRECTION_INPUT, -1) {
			crate::value::NodeValue::Combo(i) => *i,
			crate::value::NodeValue::Int(i) => *i,
			crate::value::NodeValue::Text(s) => {
				let lower = s.to_lowercase();
				if lower == "forward" || lower == "0" {
					0
				} else if lower == "inverse" || lower == "1" {
					1
				} else {
					eprintln!("OCIOLutNode: unexpected direction value {}", s);
					0
				}
			}
			other => {
				eprintln!(
					"OCIOLutNode: unexpected direction value {}",
					other.to_double()
				);
				0
			}
		}
	}

	/// Whether this is the main GUI process (C++ file-static
	/// `is_main_process()`): true when a render manager exists (the
	/// render worker never creates one).
	#[allow(dead_code)] // C++ parity probe; constants-only in the Rust model.
	// The tests assert the worker answer directly.
	fn is_main_process() -> bool {
		// The C++ probes `oakrender_manager_available()`, which the
		// oakrender bridge does not expose. Without a render manager (the
		// oakrender-free test environment) the probe would report the
		// worker process, so the worker branch — defer processor
		// generation to value() time — always applies here.
		// `// CPP-PARITY: ociolut.cpp` is_main_process.
		false
	}

	/// (Re)generate the processor now (C++ `generate_processor()`):
	/// ensures the processor is current, then — in the main process
	/// only — invalidates the texture-input cache and cancels background
	/// video cache tasks so in-flight renders cannot write stale frames.
	#[allow(dead_code)] // C++ parity entry point; the tests drive it.
	fn generate_processor(&mut self, core: &mut NodeCore) {
		self.ensure_processor(core);
		// The C++ main-process half (`invalidate_all(k_texture_input)`
		// + `oakrender_cancel_video_tasks(0)`) refreshes the viewer after
		// a processor change; it needs the render manager seam that the
		// Rust model does not expose, and `is_main_process()` is false
		// here, so it is skipped (`// CPP-PARITY: ociolut.cpp`
		// generate_processor).
	}

	/// Ensure the processor matches the current inputs (C++
	/// `ensure_processor()`): under the generation mutex, returns early
	/// when not dirty, a cached processor exists, and path/direction are
	/// unchanged; otherwise rebuilds from the inputs.
	fn ensure_processor(&self, core: &NodeCore) {
		{
			let state = self.state.lock().unwrap();
			if !state.dirty
				&& state.last_processor.is_some()
				&& Self::file_path(core) == state.last_path
				&& Self::read_direction_input(core) == state.last_direction
			{
				return;
			}
		}
		self.create_processor_from_inputs(core);
	}

	/// Rebuild the processor from the LUT path and direction (C++
	/// `create_processor_from_inputs()`): empty path, non-regular file,
	/// or a failed OCIO FileTransform -> clear both processor slots,
	/// reset the cache markers, record the error, and return false;
	/// otherwise create the LUT processor via
	/// [`oak_core::color::ColorProcessor::create_lut`] on the process-wide
	/// default config (direction 0 = forward), update the cache markers
	/// and both processor slots, and return true. A missing default
	/// config yields `None` from `create_lut`, landing in the error
	/// branch (the node then passes its input through unchanged).
	/// `// CPP-PARITY: ociolut.cpp` `create_processor_from_inputs()`.
	fn create_processor_from_inputs(&self, core: &NodeCore) -> bool {
		let path = Self::file_path(core);
		let direction = Self::read_direction_input(core);

		// Reset both processor slots and the cache markers, recording
		// `error` (empty = no error: no LUT selected yet).
		let clear = |node: &Self, error: String| {
			let mut state = node.state.lock().unwrap();
			state.last_processor = None;
			state.last_path.clear();
			state.last_direction = -1;
			state.dirty = false;
			state.last_error = error;
			node.base.set_processor(None);
			false
		};

		if path.is_empty() {
			return clear(self, String::new());
		}
		if !std::path::Path::new(&path).is_file() {
			return clear(self, format!("LUT file not found: {path}"));
		}
		let dir = if direction == 1 {
			oak_core::color::Direction::Inverse
		} else {
			oak_core::color::Direction::Normal
		};
		match oak_core::color::ColorProcessor::create_lut(&path, dir) {
			Some(processor) => {
				let processor = std::sync::Arc::new(processor);
				{
					let mut state = self.state.lock().unwrap();
					state.last_processor = Some(processor.clone());
					state.last_path = path;
					state.last_direction = direction;
					state.dirty = false;
					state.last_error.clear();
				}
				self.base.set_processor(Some(processor));
				true
			}
			None => clear(
				self,
				format!("Failed to create a LUT processor from {path}"),
			),
		}
	}

	/// OCIO config change hook (C++ `config_changed()` override):
	/// regenerates immediately in the main process, or just marks the
	/// processor dirty in the render worker (deferred to render time).
	fn config_changed(&mut self, core: &mut NodeCore) {
		let _ = core;
		// C++: main process -> generate_processor(); render worker ->
		// mark dirty. `is_main_process()` is false here, so the worker
		// branch applies and the processor is rebuilt at value() time.
		self.state.lock().unwrap().dirty = true;
	}

	/// The standard value of [`FILE_INPUT`] as a path string.
	fn file_path(core: &NodeCore) -> String {
		match &core.standard_value(FILE_INPUT, -1) {
			crate::value::NodeValue::Text(s) => s.clone(),
			_ => String::new(),
		}
	}
}

impl NodeBehavior for OCIOLutNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"OCIO LUT"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ociolut"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Applies a LUT file through OpenColorIO."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `lut_file_in` -> "LUT File", `lut_dir_in` -> "Direction" (also
	/// sets the direction combo strings "Forward"/"Inverse").
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			crate::nodes::ociobase::TEXTURE_INPUT => "Input",
			FILE_INPUT => "LUT File",
			DIRECTION_INPUT => "Direction",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `lut_dir_in` -> "Forward", "Inverse".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			DIRECTION_INPUT => vec!["Forward", "Inverse"],
			_ => Vec::new(),
		}
	}

	/// Input value changed (C++ `InputValueChangedEvent`): for
	/// `lut_file_in` or `lut_dir_in`, regenerates the processor
	/// immediately in the main process; in the render worker (where
	/// generation can be slow and the main process may be blocked
	/// waiting on LoadGraph) only marks the processor dirty so it is
	/// rebuilt at render time.
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		let _ = element;
		if input == FILE_INPUT || input == DIRECTION_INPUT {
			// C++: main process -> generate_processor(); render worker ->
			// mark dirty. `is_main_process()` is false here (no render
			// manager), so the worker branch applies.
			self.state.lock().unwrap().dirty = true;
			let _ = core;
		}
	}

	/// Evaluate outputs (C++ `value()`): first `ensure_processor()` so
	/// the processor is up to date before the base class emits the color
	/// transform job (essential in the render worker, where creation is
	/// deferred until the first render), then delegates to
	/// [`OcioBase::value`].
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		self.ensure_processor(core);
		self.base.value(core, inputs, time, table);
	}

	/// Added to a graph (C++ base `AddedToGraphEvent`): captures the
	/// project's color manager and runs `config_changed()` via
	/// [`OcioBase::added_to_graph`].
	fn added_to_graph(&mut self, core: &mut NodeCore) {
		self.base.added_to_graph(core);
		self.config_changed(core);
	}

	/// Removed from a graph (C++ base `RemovedFromGraphEvent`): clears
	/// the color manager pointer via [`OcioBase::removed_from_graph`].
	fn removed_from_graph(&mut self, core: &mut NodeCore) {
		self.base.removed_from_graph(core);
	}

	/// Deep copy (C++ `copy()` via `NODE_COPY_FUNCTION`; the destructor
	/// additionally disconnects all signals and frees the cached
	/// processor).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		// The C++ copy constructor carries over the processor-cache
		// members; the refcounted `last_processor_` cannot be shared
		// safely by value (the destructor frees it), so a fresh
		// processor state (dirty, no cache) plus a fresh empty OcioBase
		// is the safe port — the processor is never populated without
		// the render bridge anyway.
		Some(Box::new(OCIOLutNode {
			base: OcioBase::new(),
			state: Mutex::new(ProcessorState::default()),
		}))
	}
}

/// Constructor (C++ `OCIOLutNode::OCIOLutNode()`): builds the base
/// (`tex_in` texture input, effect input, video-effect flag) and adds
/// `lut_file_in` (with the LUT filter/placeholder/lut-library
/// properties) and `lut_dir_in` with the defaults, flags and properties
/// documented on the constants; the processor state starts dirty.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	// OCIOBaseNode base constructor.
	let mut tex = crate::input::Input::new(
		crate::nodes::ociobase::TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);
	core.effect_input = crate::nodes::ociobase::TEXTURE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	// The file input is a string-carried type in Rust (no file value
	// type); the C++ k_file type maps to the same string marshalling.
	let mut file = crate::input::Input::new(
		FILE_INPUT,
		crate::value::ValueType::Text,
		crate::value::NodeValue::Text(String::new()),
	);
	file.flags |= crate::input::flags::NOT_KEYFRAMABLE | crate::input::flags::NOT_CONNECTABLE;
	file.properties = vec![
		// The C++ collects the supported LUT extensions from
		// `oakrender_lut_supported_extension_at`; without the render
		// bridge the list is empty and the `*.*` fallback applies
		// (`// CPP-PARITY: ociolut.cpp` constructor).
		(
			"filter".to_string(),
			crate::value::NodeValue::Text("LUT Files (*.*);;All Files (*)".into()),
		),
		(
			"placeholder".to_string(),
			crate::value::NodeValue::Text("Select a LUT file".into()),
		),
		(
			"lut_library".to_string(),
			crate::value::NodeValue::Boolean(true),
		),
	];
	core.add_input(file);

	let mut direction = crate::input::Input::new(
		DIRECTION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	);
	direction.flags |= crate::input::flags::NOT_KEYFRAMABLE | crate::input::flags::NOT_CONNECTABLE;
	core.add_input(direction);

	(
		core,
		Box::new(OCIOLutNode {
			base: OcioBase::new(),
			state: Mutex::new(ProcessorState::default()),
		}),
	)
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.ociolut`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ociolut",
		name: "OCIO LUT",
		categories: &[Category::Color],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn node() -> OCIOLutNode {
		OCIOLutNode {
			base: OcioBase::new(),
			state: Mutex::new(ProcessorState::default()),
		}
	}

	#[test]
	fn input_names() {
		let n = node();
		assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(FILE_INPUT), "LUT File");
		assert_eq!(n.input_name(DIRECTION_INPUT), "Direction");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.ociolut");
		assert_ne!(
			core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
				.unwrap()
				.flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		let file = core.get_input(FILE_INPUT).unwrap();
		assert_eq!(file.default, NodeValue::Text(String::new()));
		assert_ne!(file.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		assert_ne!(file.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		// `*.*` filter fallback without the render bridge.
		assert!(file.properties.iter().any(|(k, v)| k == "filter"
			&& *v == NodeValue::Text("LUT Files (*.*);;All Files (*)".into())));
		assert!(file
			.properties
			.iter()
			.any(|(k, v)| k == "placeholder" && *v == NodeValue::Text("Select a LUT file".into())));
		assert!(file
			.properties
			.iter()
			.any(|(k, v)| k == "lut_library" && *v == NodeValue::Boolean(true)));
		let dir = core.get_input(DIRECTION_INPUT).unwrap();
		assert_eq!(dir.default, NodeValue::Combo(0));
		assert_ne!(dir.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		assert_ne!(dir.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn read_direction_input_parses_combo_and_legacy_strings() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			DIRECTION_INPUT,
			crate::value::ValueType::Combo,
			crate::value::NodeValue::Combo(0),
		));
		assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
		assert_eq!(OCIOLutNode::read_direction_input(&core), 1);
		// Legacy string storage (old serializers).
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("forward".into()));
		assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("Inverse".into()));
		assert_eq!(OCIOLutNode::read_direction_input(&core), 1);
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Text("0".into()));
		assert_eq!(OCIOLutNode::read_direction_input(&core), 0);
	}

	#[test]
	fn last_error_roundtrip_and_set_once() {
		let n = node();
		assert_eq!(n.last_error(), "");
		n.set_last_error("OCIO LUT: file does not exist: /nope.cube");
		assert_eq!(n.last_error(), "OCIO LUT: file does not exist: /nope.cube");
		// No-op when unchanged.
		n.set_last_error("OCIO LUT: file does not exist: /nope.cube");
		assert_eq!(n.last_error(), "OCIO LUT: file does not exist: /nope.cube");
		n.set_last_error("");
		assert_eq!(n.last_error(), "");
	}

	#[test]
	fn create_processor_from_inputs_resets_markers_without_lut() {
		let n = node();
		let core = NodeCore::new();
		{
			let mut state = n.state.lock().unwrap();
			state.dirty = true;
			state.last_path = "/tmp/foo.cube".to_string();
			state.last_direction = 1;
			state.last_processor = Some(std::sync::Arc::new(
				oak_core::color::ColorProcessor::pass_through(),
			));
			state.last_error = "stale error".to_string();
		}
		// No FILE_INPUT on the core -> empty path branch: markers reset
		// and the stale error clears (no LUT selected is not an error).
		let created = n.create_processor_from_inputs(&core);
		assert!(!created);
		let state = n.state.lock().unwrap();
		assert!(!state.dirty);
		assert_eq!(state.last_path, "");
		assert_eq!(state.last_direction, -1);
		assert!(state.last_processor.is_none());
		assert_eq!(state.last_error, "");
	}

	#[test]
	fn ensure_processor_rebuilds_when_no_cached_processor() {
		let n = node();
		let core = NodeCore::new();
		// Fresh state is dirty -> rebuild (resets markers, returns false).
		n.ensure_processor(&core);
		let state = n.state.lock().unwrap();
		assert!(!state.dirty);
		assert!(state.last_processor.is_none());
	}

	#[test]
	fn input_value_changed_marks_dirty() {
		let mut core = NodeCore::new();
		let mut n = node();
		n.state.lock().unwrap().dirty = false;
		n.input_value_changed(&mut core, FILE_INPUT, 0);
		assert!(n.state.lock().unwrap().dirty);
		n.state.lock().unwrap().dirty = false;
		n.input_value_changed(&mut core, DIRECTION_INPUT, 0);
		assert!(n.state.lock().unwrap().dirty);
		// Unrelated inputs are ignored.
		n.state.lock().unwrap().dirty = false;
		n.input_value_changed(&mut core, crate::nodes::ociobase::TEXTURE_INPUT, 0);
		assert!(!n.state.lock().unwrap().dirty);
	}

	#[test]
	fn config_changed_marks_dirty() {
		let mut core = NodeCore::new();
		let mut n = node();
		n.state.lock().unwrap().dirty = false;
		n.config_changed(&mut core);
		assert!(n.state.lock().unwrap().dirty);
	}

	#[test]
	fn file_path_reads_text_standard_value() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			FILE_INPUT,
			crate::value::ValueType::Text,
			crate::value::NodeValue::Text(String::new()),
		));
		core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
		assert_eq!(OCIOLutNode::file_path(&core), "/tmp/x.cube");
		// Non-text standard value (or missing input) yields empty.
		core.set_standard_value(FILE_INPUT, -1, NodeValue::Float(3.0));
		assert_eq!(OCIOLutNode::file_path(&core), "");
	}

	#[test]
	fn ensure_processor_reuses_cached_when_unchanged() {
		let n = node();
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			FILE_INPUT,
			crate::value::ValueType::Text,
			crate::value::NodeValue::Text(String::new()),
		));
		core.add_input(crate::input::Input::new(
			DIRECTION_INPUT,
			crate::value::ValueType::Combo,
			crate::value::NodeValue::Combo(0),
		));
		core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
		{
			let mut state = n.state.lock().unwrap();
			state.dirty = false;
			state.last_path = "/tmp/x.cube".to_string();
			state.last_direction = 1;
			state.last_processor = Some(std::sync::Arc::new(
				oak_core::color::ColorProcessor::pass_through(),
			));
		}
		// Unchanged path+direction with a live processor: early return,
		// markers are preserved (create_processor_from_inputs would have
		// reset them).
		n.ensure_processor(&core);
		let state = n.state.lock().unwrap();
		assert_eq!(state.last_path, "/tmp/x.cube");
		assert_eq!(state.last_direction, 1);
		assert!(state.last_processor.is_some());
	}

	#[test]
	fn generate_processor_wraps_ensure_processor() {
		let mut n = node();
		let mut core = NodeCore::new();
		n.state.lock().unwrap().dirty = true;
		// Worker branch: generate_processor just ensures; markers reset.
		n.generate_processor(&mut core);
		let state = n.state.lock().unwrap();
		assert!(!state.dirty);
		assert_eq!(state.last_path, "");
		assert_eq!(state.last_direction, -1);
	}

	#[test]
	fn is_main_process_always_false_without_bridge() {
		assert!(!OCIOLutNode::is_main_process());
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty());
	}

	#[test]
	fn value_passes_texture_through_without_processor() {
		// Without the render bridge no LUT processor can be created, so
		// the base value() passes the input texture through unchanged.
		let core = NodeCore::new();
		let n = node();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let inputs = crate::value::NodeValueRow::from([(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			tex.clone(),
		)]);
		let mut table = NodeValueTable::default();
		n.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_pushes_color_transform_job_with_processor() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			FILE_INPUT,
			crate::value::ValueType::Text,
			crate::value::NodeValue::Text(String::new()),
		));
		core.add_input(crate::input::Input::new(
			DIRECTION_INPUT,
			crate::value::ValueType::Combo,
			crate::value::NodeValue::Combo(0),
		));
		core.set_standard_value(FILE_INPUT, -1, NodeValue::Text("/tmp/x.cube".into()));
		core.set_standard_value(DIRECTION_INPUT, -1, NodeValue::Combo(1));
		let n = node();
		let processor = std::sync::Arc::new(oak_core::color::ColorProcessor::pass_through());
		{
			// Prime the cache so value()'s ensure_processor early-returns
			// (unchanged path/direction, live cached processor).
			let mut state = n.state.lock().unwrap();
			state.dirty = false;
			state.last_path = "/tmp/x.cube".to_string();
			state.last_direction = 1;
			state.last_processor = Some(processor.clone());
		}
		n.base.set_processor(Some(processor));
		let inputs = crate::value::NodeValueRow::from([(
			crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		n.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
			panic!("job row expected");
		};
		assert!(unsafe {
			crate::jobs::color_transform_job(handle)
		}
		.is_some());
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "OCIO LUT");
	}
}
