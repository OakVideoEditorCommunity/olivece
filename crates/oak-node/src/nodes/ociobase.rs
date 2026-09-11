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

//! Shared base for the OCIO-backed color nodes (C++
//! `src/node/src/color/ociobase/ociobase.{h,cpp}`, `olive::OCIOBaseNode`).
//!
//! This is NOT a [`NodeBehavior`] implementation — the C++ class is an
//! abstract base (its `config_changed()` is pure virtual), so it is
//! modeled as a helper struct embedded by the OCIO grading/LUT/display
//! nodes (`super::displaytransform`, `super::ociolut`,
//! `super::ociogradingtransformlinear`, `super::ociogradingtransformlog`).
//!
//! Note: OpenColorIO itself is never linked here. All OCIO work goes
//! through the color manager (`crate::colormanager`) and the oakrender
//! bridge (opaque oakrender handles), mirroring how the C++ node calls
//! `oakrender_color_processor_*` / `oaknode_colormanager_*` C functions
//! instead of using OCIO directly.

use crate::node::NodeCore;
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};

/// Texture input id shared by all OCIO nodes (C++
/// `OCIOBaseNode::k_texture_input`). Type: texture; flags:
/// not-keyframable; declared in the base constructor, which also makes
/// it the node's effect input and sets the video-effect flag.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Shared state and behavior of the C++ `OCIOBaseNode` base class.
///
/// The C++ class also stores `manager_`, a borrowed
/// `olive::ColorManager*` captured in `AddedToGraphEvent` and cleared in
/// `RemovedFromGraphEvent`. There is no Rust-owned equivalent for that
/// borrowed pointer (the color manager lives per-project behind the
/// bridge), so the field is omitted here: `added_to_graph` /
/// `removed_from_graph` document the capture/clear, and the processor
/// generation helpers reach the manager through
/// `crate::colormanager`/oakrender at call time.
pub struct OcioBase {
	/// Owned color processor (C++ `processor_`, an `OakColorProcessor`),
	/// shared by `Arc` because the [`crate::jobs::ColorTransformJobPayload`]
	/// emitted at evaluation time carries the same immutable processor.
	/// `None` while no valid processor has been generated. Released with
	/// the node (C++ destructor calls `oakrender_color_processor_free`).
	///
	/// Behind a mutex because the C++ regenerates the processor from
	/// `value()`-time paths (`ensure_processor()`'s mutable-in-const-
	/// method pattern), so interior mutability is required.
	processor: std::sync::Mutex<Option<std::sync::Arc<oak_core::color::ColorProcessor>>>,
}

impl OcioBase {
	/// Construct the shared base state (C++ `OCIOBaseNode::OCIOBaseNode()`):
	/// the processor starts empty; the constructor side that adds
	/// [`TEXTURE_INPUT`], marks it the effect input and sets the
	/// video-effect flag happens in each node's `create()`.
	pub fn new() -> Self {
		OcioBase {
			processor: std::sync::Mutex::new(None),
		}
	}

	/// Shared reference to the owned processor (C++
	/// `OCIOBaseNode::processor()`; callers must NOT free it).
	pub fn processor(&self) -> Option<std::sync::Arc<oak_core::color::ColorProcessor>> {
		self.processor
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
	}

	/// Take ownership of a new processor, releasing the old one
	/// (C++ `OCIOBaseNode::set_processor()`, which frees the previous
	/// `OakColorProcessor` before storing the new one).
	pub fn set_processor(
		&self,
		processor: Option<std::sync::Arc<oak_core::color::ColorProcessor>>,
	) {
		// The C++ frees the previous processor via
		// `oakrender_color_processor_free`; the Rust processor is an
		// `Arc` released on drop, so replacing the field drops the old
		// one automatically once any in-flight jobs release it.
		*self.processor.lock().unwrap_or_else(|e| e.into_inner()) = processor;
	}

	/// Shared output evaluation (C++ `OCIOBaseNode::value()`): no texture
	/// on [`TEXTURE_INPUT`] -> push nothing; texture present and
	/// processor ready -> push a boxed
	/// [`crate::jobs::ColorTransformJobPayload`] built from the processor
	/// and the input texture (the C++ `t->to_job(ColorTransformJob)`,
	/// resolved by the render seam via the processor); texture present
	/// but processor not ready (e.g. still being generated
	/// asynchronously) -> pass the input texture through unchanged.
	/// `// CPP-PARITY: ociobase.cpp` `value()`.
	pub fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: oak_core::Rational,
		table: &mut NodeValueTable,
	) {
		let _ = core;
		let Some(tex @ NodeValue::Texture(_)) = inputs.get(TEXTURE_INPUT) else {
			return;
		};
		match self.processor() {
			Some(processor) => table.push(
				crate::value::ValueType::Texture,
				NodeValue::Texture(crate::handle::make_owned(crate::jobs::Job::ColorTransformJob(
					crate::jobs::ColorTransformJobPayload {
						color_processor: processor,
						input: tex.clone(),
						time,
					},
				))),
				None,
			),
			None => table.push(crate::value::ValueType::Texture, tex.clone(), None),
		}
	}

	/// Graph-entry hook (C++ `OCIOBaseNode::AddedToGraphEvent`):
	/// captures the project's color manager, then invokes the node's
	/// `config_changed()` (a per-subclass method here, since the C++
	/// pure virtual has no trait home). Subclasses call this from their
	/// [`NodeBehavior::added_to_graph`] override.
	///
	/// The Rust model has no borrowed-manager field (see the type doc),
	/// so this only forwards the subclass hook call sites.
	pub fn added_to_graph(&mut self, core: &mut NodeCore) {
		let _ = (self, core);
	}

	/// Graph-exit hook (C++ `OCIOBaseNode::RemovedFromGraphEvent`):
	/// clears the captured color manager pointer. Subclasses call this
	/// from their [`NodeBehavior::removed_from_graph`] override.
	pub fn removed_from_graph(&mut self, core: &mut NodeCore) {
		let _ = (self, core);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeCore;
	use crate::value::{NodeValueRow, NodeValueTable, ValueType};

	#[test]
	fn processor_state_transitions() {
		let base = OcioBase::new();
		assert!(base.processor().is_none());
		base.set_processor(Some(std::sync::Arc::new(
			oak_core::color::ColorProcessor::pass_through(),
		)));
		assert!(base.processor().is_some());
		base.set_processor(None);
		assert!(base.processor().is_none());
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
		let base = OcioBase::new();
		let mut table = NodeValueTable::default();
		base.value(
			&NodeCore::new(),
			&NodeValueRow::default(),
			oak_core::Rational::new(0, 1),
			&mut table,
		);
		assert!(table.is_empty());
	}

	#[test]
	fn value_passes_through_without_processor() {
		let base = OcioBase::new();
		let mut table = NodeValueTable::default();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		base.value(
			&NodeCore::new(),
			&inputs,
			oak_core::Rational::new(0, 1),
			&mut table,
		);
		// Pass-through keeps the input texture value.
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_pushes_color_transform_job_with_processor() {
		let base = OcioBase::new();
		base.set_processor(Some(std::sync::Arc::new(
			oak_core::color::ColorProcessor::pass_through(),
		)));
		let mut table = NodeValueTable::default();
		let tex = NodeValue::Texture(crate::handle::make_owned(1u8));
		let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		base.value(
			&NodeCore::new(),
			&inputs,
			oak_core::Rational::new(0, 1),
			&mut table,
		);
		// The ready case pushes the boxed ColorTransformJobPayload (the
		// C++ `t->to_job(ColorTransformJob)`).
		let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else {
			panic!("job row expected");
		};
		let payload = unsafe {
			crate::jobs::color_transform_job(handle)
		}
		.expect("a boxed ColorTransformJobPayload");
		assert!(std::sync::Arc::ptr_eq(
			&payload.color_processor,
			&base.processor().unwrap()
		));
		assert_eq!(payload.input, tex);
		assert_eq!(payload.time, oak_core::Rational::new(0, 1));
	}

	#[test]
	fn graph_hooks_are_noops() {
		let mut base = OcioBase::new();
		let mut core = NodeCore::new();
		base.added_to_graph(&mut core);
		base.removed_from_graph(&mut core);
		assert!(base.processor().is_none());
	}
}
