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

//! OpenFX plugin node (C++ `engine/node/plugins/plugin.{h,cpp}`,
//! `olive::plugin::PluginNode`).
//!
//! Thin wrapper over an OFX plugin instance that lives in the
//! `oakplugin` crate; no OFX types (`OFX::Host::ImageEffect::Instance`,
//! `kOfxParam*`, ...) are declared here because oaknode sits below
//! oakplugin in the dependency graph. The instance is represented as
//! the opaque [`PluginInstanceHandle`] — an identity key into the
//! oakplugin crate's instance registry.
//!
//! All per-plugin data (inputs, defaults, properties, labels) is
//! discovered at runtime from the plugin descriptor by the oakplugin
//! discovery pass (`oak_plugin::node_factory`), which builds the
//! [`NodeCore`] inputs, constructs [`PluginNode`]s and registers one
//! factory entry per discovered plugin (the C++
//! `factory.cpp::register_plugin_nodes` + `PluginNode::PluginNode`
//! split across the dependency seam).

use std::sync::{Arc, Mutex, OnceLock};

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::{Rational, TimeRange};

/// Texture input id (C++ `plugin::k_texture_input`). Type: texture;
/// no default. Only synthesized when the plugin declares clip inputs
/// but none of them is the simple-source clip (see the discovery
/// pass); then it becomes the node's effect input with display name
/// "Texture".
pub const TEXTURE_INPUT: &str = "tex_in";

/// OFX simple-source clip name (C++ `kOfxImageEffectSimpleSourceClipName`):
/// the canonical first texture clip of a filter plugin.
pub const SOURCE_CLIP: &str = "Source";

/// Opaque handle to an OFX plugin instance owned by the `oakplugin`
/// crate (identity key into its instance registry; C++
/// `OFX::Host::ImageEffect::Instance *`, member `plugin_instance_`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PluginInstanceHandle(pub u64);

impl PluginInstanceHandle {
	/// The null/absent instance handle (no plugin instance behind the
	/// node).
	pub fn null() -> Self {
		PluginInstanceHandle(0)
	}

	/// True when no instance is behind this handle.
	pub fn is_null(&self) -> bool {
		self.0 == 0
	}
}

/// Plugin job payload (C++ `plugin::PluginJob`): everything the render
/// seam needs to run the instance against the input texture. Boxed
/// into a texture-typed [`NodeValue`] by [`PluginNode::value`] and
/// resolved by the oakrender evaluation seam (C++
/// `RenderProcessor::process_plugin_job`), which executes it through
/// the oakplugin render driver.
#[derive(Clone, Debug)]
pub struct PluginJobPayload {
	/// The instance identity (oakplugin registry key).
	pub instance: PluginInstanceHandle,
	/// The request time (C++ `globals.time().in()`).
	pub time: Rational,
	/// The effect input id the main source texture arrives on (C++
	/// `node->get_effect_input_id()`).
	pub effect_input_id: String,
	/// Snapshot of the full input row (C++ `PluginJob` holds the
	/// `NodeValueRow`: the tagged non-texture values become param
	/// overrides, the texture values feed the clip inputs).
	pub values: NodeValueRow,
}

/// OFX plugin node. Wraps one plugin instance; inputs mirror the
/// plugin's declared params and clips.
///
/// The C++ class has a single own member besides the instance
/// pointer, `sub_category_`. Because `name()`/`id()`/`description()`
/// read the plugin descriptor at call time in C++ and the Rust trait
/// returns `&str`, this port caches those strings on the struct
/// (populated from the descriptor at construction).
pub struct PluginNode {
	/// The wrapped plugin instance (C++ `plugin_instance_`; an
	/// oakplugin registry identity here).
	instance: PluginInstanceHandle,
	/// Sub-category derived from the OFX context (C++
	/// `sub_category_`): "Filter", "Generator", "Transition", or
	/// "General".
	sub_category: String,
	/// Cached display name (C++ `name()` reads the descriptor's
	/// `kOfxPropLabel` on every call).
	name: String,
	/// Cached type id (C++ `id()` = the plugin identifier).
	type_id: String,
	/// Cached description (C++ `description()` reads the
	/// descriptor's `kOfxPropPluginDescription`).
	description: String,
}

/// The plugin-node duplicator (installed by the oakplugin crate): old
/// instance identity -> fresh instance identity (the C++
/// `PluginNode::copy()` creates a fresh instance — filter context
/// preferred, else the plugin's first declared context). `None` when
/// instance creation fails.
type PluginDuplicator = dyn Fn(PluginInstanceHandle) -> Option<PluginInstanceHandle> + Send + Sync;

static DUPLICATOR: OnceLock<Mutex<Option<Arc<PluginDuplicator>>>> = OnceLock::new();

fn duplicator_slot() -> &'static Mutex<Option<Arc<PluginDuplicator>>> {
	DUPLICATOR.get_or_init(|| Mutex::new(None))
}

/// Install the plugin-node duplicator (oakplugin discovery path;
/// `None` clears it). Without it [`PluginNode::duplicate`] returns
/// `None`.
pub fn set_plugin_duplicator(dup: Option<Arc<PluginDuplicator>>) {
	*duplicator_slot().lock().unwrap_or_else(|e| e.into_inner()) = dup;
}

impl PluginNode {
	/// Constructor for the oakplugin discovery path (the C++
	/// `PluginNode::PluginNode(instance)`; the input walk happens in
	/// the oakplugin translation pass, which owns the OFX types).
	pub fn new(
		instance: PluginInstanceHandle,
		name: String,
		type_id: String,
		description: String,
		sub_category: String,
	) -> Self {
		PluginNode {
			instance,
			sub_category,
			name,
			type_id,
			description,
		}
	}

	/// The wrapped instance identity (oakplugin registry key).
	pub fn instance_handle(&self) -> PluginInstanceHandle {
		self.instance
	}

	/// Forward a push-button param activation to the plugin (C++
	/// `push_button_clicked()`; currently a no-op upstream).
	pub fn push_button_clicked(&mut self, core: &mut NodeCore, name: String) {
		let _ = (core, name);
	}
}

impl NodeBehavior for PluginNode {
	/// Human-readable name (C++ `name()` = the plugin descriptor's
	/// `kOfxPropLabel`; served from the cached copy).
	fn name(&self) -> &str {
		&self.name
	}

	/// Stable type id (C++ `id()` = the plugin identifier; served
	/// from the cached copy).
	fn type_id(&self) -> &str {
		&self.type_id
	}

	/// Categories (C++ `category()`; always OpenFx).
	fn categories(&self) -> &[Category] {
		&[Category::OpenFx]
	}

	/// Sub-category (C++ `sub_category()` = the context string
	/// captured at construction).
	fn sub_category(&self) -> &str {
		&self.sub_category
	}

	/// Description (C++ `description()` = the descriptor's
	/// `kOfxPropPluginDescription`; served from the cached copy).
	fn description(&self) -> &str {
		&self.description
	}

	/// Evaluate outputs (C++ `value()`): re-pushes every non-texture,
	/// non-none input value tagged with its input id (the tag routes
	/// the value to the matching OFX param); then resolves the input
	/// texture — simple-source clip first, then `tex_in`, then the
	/// first texture-typed input — and, when both texture and plugin
	/// instance exist, pushes a [`PluginJobPayload`] boxed into a
	/// texture-typed value (the C++ `table->push(k_texture,
	/// tex->to_job(job), this)`; the render seam executes the job).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		// Re-push every non-texture, non-none input value, tagged with
		// its input id.
		for (id, v) in inputs.iter() {
			if matches!(v, NodeValue::Texture(_) | NodeValue::None) {
				continue;
			}
			table.push(v.value_type(), v.clone(), Some(id.clone()));
		}

		// Resolve the input texture: simple-source clip, then tex_in,
		// then the first texture-typed input.
		let tex = inputs
			.get(SOURCE_CLIP)
			.filter(|v| matches!(v, NodeValue::Texture(_)))
			.or_else(|| {
				inputs
					.get(TEXTURE_INPUT)
					.filter(|v| matches!(v, NodeValue::Texture(_)))
			})
			.or_else(|| inputs.values().find(|v| matches!(v, NodeValue::Texture(_))));

		if tex.is_some() && !self.instance.is_null() {
			let payload = PluginJobPayload {
				instance: self.instance,
				time,
				effect_input_id: core.effect_input.clone(),
				values: inputs.clone(),
			};
			table.push(
				crate::value::ValueType::Texture,
				NodeValue::Texture(crate::handle::make_owned(payload)),
				None,
			);
		}
	}

	/// Process audio samples (C++ `process_samples()`): passthrough —
	/// silences the output when the input is empty/unallocated,
	/// otherwise reallocates the output to the input's params when
	/// mismatched and copies every channel verbatim.
	///
	/// The C++ receives the input buffer from the audio traversal; the
	/// Rust trait hands only the value row, so the input buffer is
	/// located as the first samples value in the row.
	fn process_samples(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		range: TimeRange,
		output: &mut crate::value::SampleBuffer,
	) {
		let _ = (core, range);

		let input = match inputs.values().find(|v| matches!(v, NodeValue::Samples(_))) {
			Some(NodeValue::Samples(b)) => b,
			_ => {
				// Empty/unallocated input: silence the output (C++
				// `output.silence()`).
				if output.is_allocated() {
					output.data.fill(0);
				}
				return;
			}
		};

		if !input.is_allocated() || input.channels == 0 || input.sample_count == 0 {
			if output.is_allocated() {
				output.data.fill(0);
			}
			return;
		}

		// Reallocate the output to the input's params when mismatched
		// (C++ `set_audio_params` + `set_sample_count` + `allocate`).
		if !output.is_allocated()
			|| output.channels != input.channels
			|| output.sample_count != input.sample_count
		{
			output.format = input.format;
			output.channels = input.channels;
			output.sample_count = input.sample_count;
			let bps = input.format.bytes_per_sample();
			output.data = vec![0u8; input.channels * input.sample_count * bps];
		}

		// Copy every channel verbatim (C++ `output.fast_set(input, c)`).
		for c in 0..input.channels {
			for i in 0..input.sample_count {
				let v = input.sample_value(c, i);
				output.set_sample_value(c, i, v);
			}
		}
	}

	/// Direct frame generation (C++ `generate_frame()`): allocates
	/// the destination frame if needed and zero-fills it (plugins do
	/// their real image work in the plugin job, not here).
	///
	/// The Rust frame is an opaque [`crate::handle::CHandle`]
	/// whose pixels cannot be read or written from this crate, so the
	/// body is a documented no-op (`// CPP-PARITY: plugin.cpp`
	/// `generate_frame`).
	fn generate_frame(
		&self,
		core: &NodeCore,
		frame: &mut crate::handle::CHandle,
		time: Rational,
	) {
		let _ = (core, frame, time);
	}

	/// Deep copy (C++ `copy()`): asks the oakplugin side (through the
	/// installed [`set_plugin_duplicator`] hook) for a fresh plugin
	/// instance — filter context when supported, else the plugin's
	/// first declared context — and wraps it in a new behavior; the
	/// caller clones the core (inputs included), matching the C++
	/// `Node::copy` split. `None` when there is no instance or no
	/// duplicator installed.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		if self.instance.is_null() {
			return None;
		}
		let dup = duplicator_slot()
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()?;
		let new_handle = dup(self.instance)?;
		Some(Box::new(PluginNode {
			instance: new_handle,
			sub_category: self.sub_category.clone(),
			name: self.name.clone(),
			type_id: self.type_id.clone(),
			description: self.description.clone(),
		}))
	}

	/// Downcast to [`Self`] (instance/metadata access).
	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	/// Mutable downcast (see [`NodeBehavior::as_any`]).
	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

/// Constructor (C++ `PluginNode::PluginNode(instance)`): the real
/// construction (instance creation + the OFX param/clip -> input
/// translation) belongs to the oakplugin discovery pass
/// (`oak_plugin::node_factory`), which registers one factory entry per
/// discovered plugin. This placeholder (null instance, empty cached
/// metadata) stays for the static-registration surface and tests.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let core = NodeCore::new();
	let node = PluginNode {
		instance: PluginInstanceHandle::null(),
		sub_category: "General".to_string(),
		name: String::new(),
		type_id: String::new(),
		description: String::new(),
	};
	(core, Box::new(node))
}

/// Register this node type. NOTE: unlike built-in nodes, plugin nodes
/// have no static type id — the oakplugin discovery pass appends one
/// factory entry per discovered OFX plugin at runtime (C++
/// `factory.cpp::register_plugin_nodes`) through
/// [`crate::factory::Factory::register_dynamic`], keyed by plugin
/// identifier, so there is no fixed `NodeMeta` literal to push. This
/// function is a no-op placeholder kept for the static table.
pub fn register(meta: &mut Vec<NodeMeta>) {
	let _ = meta;
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValueTable, ValueType};
	use oak_core::{Rational, TimeRange};

	/// A plugin node with a non-null instance and cached metadata.
	fn node() -> PluginNode {
		PluginNode {
			instance: PluginInstanceHandle(1),
			sub_category: "Filter".to_string(),
			name: "Test Plugin".to_string(),
			type_id: "org.example.testplugin".to_string(),
			description: "A test plugin".to_string(),
		}
	}

	#[test]
	fn metadata_served_from_cache() {
		let n = node();
		assert_eq!(n.name(), "Test Plugin");
		assert_eq!(n.type_id(), "org.example.testplugin");
		assert_eq!(n.description(), "A test plugin");
		assert_eq!(n.sub_category(), "Filter");
		assert_eq!(n.categories(), &[Category::OpenFx]);
	}

	#[test]
	fn instance_handle_null_semantics() {
		assert!(PluginInstanceHandle::null().is_null());
		assert!(!PluginInstanceHandle(1).is_null());
	}

	#[test]
	fn value_tags_non_texture_inputs() {
		let n = node();
		let core = NodeCore::new();
		let mut row = NodeValueRow::default();
		row.insert("opacity".to_string(), NodeValue::Float(0.5));
		row.insert("mode".to_string(), NodeValue::Combo(2));
		let mut table = NodeValueTable::default();
		n.value(&core, &row, Rational::new(0, 1), &mut table);
		// Both values pushed, tagged with their input ids, plus the
		// plugin job texture (no texture input in the row -> no job).
		let tagged: Vec<(&str, &NodeValue)> = table
			.rows()
			.iter()
			.filter(|(_, _, t)| t.is_some())
			.map(|(_, v, t)| (t.as_deref().unwrap(), v))
			.collect();
		assert_eq!(tagged.len(), 2);
		assert!(tagged
			.iter()
			.any(|(id, v)| *id == "opacity" && *v == &NodeValue::Float(0.5)));
		assert!(tagged
			.iter()
			.any(|(id, v)| *id == "mode" && *v == &NodeValue::Combo(2)));
		// No texture pushed (no texture input in the row).
		assert!(table.get(ValueType::Texture).is_none());
	}

	#[test]
	fn value_pushes_job_payload_for_texture_input() {
		let n = node();
		let mut core = NodeCore::new();
		core.effect_input = TEXTURE_INPUT.to_string();
		let mut row = NodeValueRow::default();
		row.insert(
			"tex_in".to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		);
		row.insert("gain".to_string(), NodeValue::Float(0.25));
		row.insert("none_in".to_string(), NodeValue::None);
		let mut table = NodeValueTable::default();
		let time = Rational::new(3, 2);
		n.value(&core, &row, time, &mut table);
		// The texture is consumed as the job source: one tagged param
		// value + the boxed plugin job payload.
		assert_eq!(table.count(), 2);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("expected the plugin job texture");
		};
		// SAFETY: the handle was created by value() boxing a
		// PluginJobPayload.
		let payload = unsafe { crate::handle::get::<PluginJobPayload>(h) }
			.expect("texture handle must box a PluginJobPayload");
		assert_eq!(payload.instance, PluginInstanceHandle(1));
		assert_eq!(payload.time, time);
		assert_eq!(payload.effect_input_id, TEXTURE_INPUT);
		assert_eq!(payload.values.len(), 3);
		assert_eq!(
			payload.values.get("gain"),
			Some(&NodeValue::Float(0.25))
		);
	}

	#[test]
	fn value_pushes_job_for_source_clip_only_input() {
		// Gate: a plugin with only the simple-source clip connected still
		// emits its job (the C++ resolution tries `Source` before
		// `tex_in`). With no texture at all, nothing is pushed.
		let n = node();
		let core = NodeCore::new();

		let mut row = NodeValueRow::default();
		row.insert(
			SOURCE_CLIP.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		);
		let mut table = NodeValueTable::default();
		n.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(
			matches!(table.get(ValueType::Texture), Some(NodeValue::Texture(h)) if !h.is_null()),
			"a source-clip-only row must still produce the plugin job"
		);

		let mut table = NodeValueTable::default();
		n.value(&core, &NodeValueRow::default(), Rational::new(0, 1), &mut table);
		assert!(
			table.get(ValueType::Texture).is_none(),
			"no texture anywhere -> no job"
		);
	}

	#[test]
	fn value_pushes_nothing_without_instance() {
		let n = PluginNode {
			instance: PluginInstanceHandle::null(),
			..node()
		};
		let core = NodeCore::new();
		let mut row = NodeValueRow::default();
		row.insert(
			TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		);
		let mut table = NodeValueTable::default();
		n.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn process_samples_copies_verbatim() {
		let n = node();
		let core = NodeCore::new();
		let input = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32Planar,
			channels: 2,
			sample_count: 3,
			// Planar layout: channel 0 plane [1, 2, 3], channel 1 plane
			// [4, 5, 6].
			data: vec![1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0]
				.iter()
				.flat_map(|f| f.to_le_bytes())
				.collect(),
		};
		let mut row = NodeValueRow::default();
		row.insert("samples_in".to_string(), NodeValue::Samples(input));
		let mut output = crate::value::SampleBuffer::default();
		n.process_samples(
			&core,
			&row,
			TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
			&mut output,
		);
		assert!(output.is_allocated());
		assert_eq!(output.channels, 2);
		assert_eq!(output.sample_count, 3);
		assert_eq!(output.format, oak_core::SampleFormat::F32Planar);
		for c in 0..2 {
			for i in 0..3 {
				let expected = 1.0 + (c * 3 + i) as f64;
				assert_eq!(output.sample_value(c, i), expected);
			}
		}
	}

	#[test]
	fn process_samples_silences_output_on_missing_input() {
		let n = node();
		let core = NodeCore::new();
		let mut output = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32,
			channels: 1,
			sample_count: 2,
			data: vec![1.0f32, 2.0]
				.iter()
				.flat_map(|f| f.to_le_bytes())
				.collect(),
		};
		n.process_samples(
			&core,
			&NodeValueRow::default(),
			TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)),
			&mut output,
		);
		assert_eq!(output.sample_value(0, 0), 0.0);
		assert_eq!(output.sample_value(0, 1), 0.0);
	}

	#[test]
	fn generate_frame_is_documented_noop() {
		let n = node();
		let core = NodeCore::new();
		let mut frame = crate::handle::CHandle::null();
		n.generate_frame(&core, &mut frame, Rational::new(0, 1));
		assert!(frame.is_null());
	}

	#[test]
	fn duplicate_uses_installed_duplicator() {
		// No duplicator installed -> None.
		set_plugin_duplicator(None);
		let n = node();
		assert!(n.duplicate(&NodeCore::new()).is_none());

		// Installed duplicator: fresh handle, metadata copied.
		set_plugin_duplicator(Some(Arc::new(|h: PluginInstanceHandle| {
			Some(PluginInstanceHandle(h.0 + 100))
		})));
		let dup = n.duplicate(&NodeCore::new()).expect("duplicator installed");
		let dup = dup.as_any().unwrap().downcast_ref::<PluginNode>().unwrap();
		assert_eq!(dup.instance_handle(), PluginInstanceHandle(101));
		assert_eq!(dup.name(), "Test Plugin");
		assert_eq!(dup.sub_category(), "Filter");

		// Duplicator failure -> None.
		set_plugin_duplicator(Some(Arc::new(|_: PluginInstanceHandle| None)));
		assert!(n.duplicate(&NodeCore::new()).is_none());
		set_plugin_duplicator(None);
	}

	#[test]
	fn push_button_clicked_is_noop() {
		let mut n = node();
		let mut core = NodeCore::new();
		n.push_button_clicked(&mut core, "button".to_string());
	}

	#[test]
	fn create_returns_placeholder_node() {
		let (core, behavior) = create();
		let n = behavior
			.as_any()
			.unwrap()
			.downcast_ref::<PluginNode>()
			.unwrap();
		assert!(n.instance.is_null());
		assert_eq!(n.sub_category(), "General");
		assert_eq!(behavior.name(), "");
		assert_eq!(behavior.type_id(), "");
		// No inputs beyond the standard enabled input.
		assert_eq!(core.inputs.len(), 1);
	}
}
