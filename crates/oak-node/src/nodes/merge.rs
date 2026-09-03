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

//! Merge node (C++ `src/node/src/math/merge/merge.{h,cpp}`,
//! `olive::MergeNode`): alpha-over composites two textures.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Base (background) texture input id (C++ `k_base_in`). Type:
/// texture; flags: not-keyframable.
pub const BASE_INPUT: &str = "base_in";

/// Blend (foreground) texture input id (C++ `k_blend_in`). Type:
/// texture; flags: not-keyframable.
pub const BLEND_INPUT: &str = "blend_in";

/// Merge node. Unit-like: the C++ class's only members are
/// `base_in_`/`blend_in_` `NodeInput*` back-pointers, which live in
/// [`NodeCore`] here, so there is nothing to duplicate.
pub struct MergeNode;

/// Fragment shader (C++ `get_shader_code()` loads the
/// `:/shaders/alphaover.frag` resource). Text copied verbatim from
/// `engine/shaders/alphaover.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D base_in;
uniform sampler2D blend_in;
uniform bool base_in_enabled;
uniform bool blend_in_enabled;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec4 base_col = texture(base_in, ove_texcoord);
    vec4 blend_col = texture(blend_in, ove_texcoord);

    if (!base_in_enabled && !blend_in_enabled) {
        frag_color = vec4(0.0);
        return;
    }

    if (!base_in_enabled) {
        frag_color = blend_col;
        return;
    }

    if (!blend_in_enabled) {
        frag_color = base_col;
        return;
    }

    base_col *= 1.0 - blend_col.a;
    base_col += blend_col;

    frag_color = base_col;
}
"#;

impl MergeNode {
	/// Fragment shader (C++ `get_shader_code()`; the request id is
	/// ignored — the same alpha-over shader serves every request).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for MergeNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Merge"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.merge"
	}

	/// Categories (C++ `category()`; filed under math even though it
	/// composites textures).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Merge two textures together."
	}

	/// Localized input names (C++ `retranslate()`): `base_in` ->
	/// "Base", `blend_in` -> "Blend".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			BASE_INPUT => "Base",
			BLEND_INPUT => "Blend",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): if only the blend texture is
	/// present, or the blend texture has fewer than RGBA channels (no
	/// alpha to over with), push the blend input as-is; if only the
	/// base texture is present, push the base input as-is; if both are
	/// present, push a shader job over the base texture with the whole
	/// input row as job values; if neither, push nothing.
	///
	/// The both-present case boxes a [`ShaderJobPayload`] that the
	/// renderer's resolve hook executes and replaces with the result
	/// texture; the params row carries both input textures, keyed by
	/// their input ids. The C++ `MergeNode` constructor never sets an
	/// effect input, so `effect_input` is empty and the runner has no
	/// main texture to bind — binding `base_in`/`blend_in` explicitly is
	/// a renderer TODO. The "blend has fewer than 4 channels" check
	/// needs the texture's channel count, which the Rust texture handle
	/// does not carry, so the alpha-less blend case is only
	/// distinguishable by presence here (`// CPP-PARITY: merge.cpp`
	/// `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let base = inputs.get(BASE_INPUT);
		let blend = inputs.get(BLEND_INPUT);

		match (base, blend) {
			(
				Some(b @ crate::value::NodeValue::Texture(_)),
				Some(bl @ crate::value::NodeValue::Texture(_)),
			) => {
				// Both present: alpha-over shader job (C++
				// `base_tex->toJob(ShaderJob(value))`). The C++ checks the
				// blend channel count here (RGBA required for an alpha to over
				// with) and pushes the blend as-is when it has no alpha
				// channel — not representable without the texture params
				// (`// CPP-PARITY: merge.cpp`).
				let _ = (b, bl);
				table.push(
					crate::value::ValueType::Texture,
					crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
						node_id: crate::id::NodeId::INVALID,
						time,
						iterations: 1,
						type_id: self.type_id().to_string(),
						shader_id: String::new(),
						effect_input: core.effect_input.clone(),
						params: inputs.clone(),
						iterative_input: String::new(),
					})),
					None,
				);
			}
			(Some(b @ crate::value::NodeValue::Texture(_)), None) => {
				table.push(crate::value::ValueType::Texture, b.clone(), None);
			}
			(None, Some(bl @ crate::value::NodeValue::Texture(_))) => {
				table.push(crate::value::ValueType::Texture, bl.clone(), None);
			}
			_ => {}
		}
	}

	/// Shader code request (C++ `get_shader_code()`): always returns
	/// the alpha-over fragment shader regardless of the request id.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MergeNode))
	}
}

/// Constructor (C++ `MergeNode::MergeNode()`): adds `base_in` and
/// `blend_in` as not-keyframable texture inputs and sets the
/// dont-show-in-param-view flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut base = crate::input::Input::new(
		BASE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);

	let mut blend = crate::input::Input::new(
		BLEND_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	blend.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(blend);

	core.flags |= crate::node::flags::DONT_SHOW_IN_PARAM_VIEW;

	(core, Box::new(MergeNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	#[test]
	fn input_names() {
		let n = MergeNode;
		assert_eq!(n.input_name(BASE_INPUT), "Base");
		assert_eq!(n.input_name(BLEND_INPUT), "Blend");
	}

	#[test]
	fn create_wires_inputs_and_flag() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.merge");
		assert!(core.get_input(BASE_INPUT).is_some());
		assert!(core.get_input(BLEND_INPUT).is_some());
		assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_PARAM_VIEW, 0);
	}

	#[test]
	fn value_neither_pushes_nothing() {
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
	fn value_base_only_pushes_base() {
		let (core, behavior) = create();
		let base = tex();
		let inputs = crate::value::NodeValueRow::from([(BASE_INPUT.to_string(), base.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&base));
	}

	#[test]
	fn value_blend_only_pushes_blend() {
		let (core, behavior) = create();
		let blend = tex();
		let inputs = crate::value::NodeValueRow::from([(BLEND_INPUT.to_string(), blend.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&blend));
	}

	#[test]
	fn value_both_pushes_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(BASE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => {
				let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
					.expect("shader job payload boxed");
				assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.merge");
				assert_eq!(payload.shader_id, "");
				assert_eq!(payload.iterations, 1);
				assert_eq!(payload.iterative_input, "");
				assert!(payload.params.contains_key(BASE_INPUT));
				assert!(payload.params.contains_key(BLEND_INPUT));
			}
			_ => panic!("texture expected"),
		}
	}

	#[test]
	fn shader_code_always_alphaover() {
		let n = MergeNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("base_col *= 1.0 - blend_col.a;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Merge");
	}
}

/// Register this node type (C++ `k_merge_node` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.merge",
		name: "Merge",
		categories: &[Category::Math],
		create,
	});
}
