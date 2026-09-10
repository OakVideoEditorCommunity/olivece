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

//! KeyMix effect (clean-room reimplementation of the OpenFX-Misc
//! `KeyMixOFX` / `net.sf.openfx.KeyMix`; the `KeyMix` reference tree is
//! used for parameter semantics only, no code copied).
//!
//! Upstream copies the foreground over the background wherever the mask
//! is opaque (`ofxsMaskMixPix` with its fixed `mix = 1`): a connected
//! mask paints the foreground through its alpha, an unconnected mask is
//! treated as white and the foreground wins everywhere. This node
//! reduces that to the mask's alpha threshold — `mask.a > 0` takes the
//! blend, else the input — which is the mask-binding piece of the
//! Tier-1 merge group (the mask input mirrors the chroma-key/despill
//! texture binding: a plain not-keyframable texture input).
//!
//! Like [`crate::nodes::merge`], the node never sets a
//! `core.effect_input`; the both-present case boxes one
//! [`ShaderJobPayload`] whose params row carries all three textures, and
//! the renderer binds `tex_in`/`blend_in`/`mask_in` by name (the pass
//! size follows the first bound texture — `blend_in`, the
//! alphabetically first key of the job's `BTreeMap` row).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Background texture input id. Type: texture; flags: not-keyframable.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Foreground texture input id. Type: texture; flags: not-keyframable.
pub const BLEND_INPUT: &str = "blend_in";

/// Mask texture input id (upstream's mask input, "Mask"). Type:
/// texture; flags: not-keyframable. Its **alpha** selects per pixel
/// (`> 0` keeps the foreground); an unconnected mask selects it
/// everywhere.
pub const MASK_INPUT: &str = "mask_in";

/// KeyMix node. Unit-like: there is no per-instance state.
pub struct KeyMixNode;

/// Fragment shader: copy the blend over the input through the mask's
/// alpha. The `_enabled` flags mirror the alpha-over shader's convention
/// for unbound samplers; the mask one carries upstream's "no mask means
/// white" rule. `switch` is deliberately not used (naga rejects it).
///
/// The ternary is kept off the `texture()` call on purpose (naga's GLSL
/// front end is happiest with the plain-branch spelling), and the
/// `value()` pass-through means the "input missing" branches are only
/// reachable through a direct renderer call.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform sampler2D mask_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform bool mask_in_enabled;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    if (!tex_in_enabled && !blend_in_enabled) {
        frag_color = vec4(0.0);
        return;
    }

    if (!tex_in_enabled) {
        frag_color = texture(blend_in, ove_texcoord);
        return;
    }

    if (!blend_in_enabled) {
        frag_color = texture(tex_in, ove_texcoord);
        return;
    }

    vec4 tex_col = texture(tex_in, ove_texcoord);
    vec4 blend_col = texture(blend_in, ove_texcoord);

    // No mask connected: upstream's mask-mix helper sees no mask image
    // and uses an opaque one, so the foreground wins everywhere.
    vec4 mask_col = vec4(1.0);
    if (mask_in_enabled) {
        mask_col = texture(mask_in, ove_texcoord);
    }

    frag_color = mask_col.a > 0.0 ? blend_col : tex_col;
}
"#;

impl KeyMixNode {
	/// Fragment shader (single variant, so the request id is ignored).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for KeyMixNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"KeyMix"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.keymix"
	}

	/// Categories. Filed under math like the alpha-over
	/// [`crate::nodes::merge`] (the `Category` enum has no merge group).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description.
	fn description(&self) -> &str {
		"Mix two textures by a mask's alpha."
	}

	/// Localized input names: `tex_in` -> "Input", `blend_in` ->
	/// "Blend", `mask_in` -> "Mask".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			BLEND_INPUT => "Blend",
			MASK_INPUT => "Mask",
			_ => id,
		}
	}

	/// Evaluate outputs: if only one of the two picture textures is
	/// present, push it as-is (no mask can conjure the other side);
	/// if both are present, push one shader job over the whole input
	/// row — the mask rides in the row and may be absent, which the
	/// shader reads as fully opaque. If neither picture is present,
	/// push nothing.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		use crate::value::{NodeValue, ValueType};

		let tex = inputs.get(TEXTURE_INPUT);
		let blend = inputs.get(BLEND_INPUT);

		match (tex, blend) {
			(Some(NodeValue::Texture(_)), Some(NodeValue::Texture(_))) => {
				table.push(
					ValueType::Texture,
					NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
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
			(Some(t @ NodeValue::Texture(_)), _) => {
				table.push(ValueType::Texture, t.clone(), None);
			}
			(_, Some(b @ NodeValue::Texture(_))) => {
				table.push(ValueType::Texture, b.clone(), None);
			}
			_ => {}
		}
	}

	/// Shader code request: always the mask-copy fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(KeyMixNode))
	}
}

/// Constructor: adds `tex_in`, `blend_in` and `mask_in` as
/// not-keyframable texture inputs and sets the video-effect flag (no
/// effect input: every texture binds by name).
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	for id in [TEXTURE_INPUT, BLEND_INPUT, MASK_INPUT] {
		let mut input = crate::input::Input::new(
			id,
			crate::value::ValueType::Texture,
			crate::value::NodeValue::None,
		);
		input.flags |= crate::input::flags::NOT_KEYFRAMABLE;
		core.add_input(input);
	}

	core.flags |= crate::node::flags::VIDEO_EFFECT;

	(core, Box::new(KeyMixNode))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn run(core: &NodeCore, inputs: &crate::value::NodeValueRow) -> ShaderJobPayload {
		let behavior = KeyMixNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	#[test]
	fn input_names() {
		let n = KeyMixNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(BLEND_INPUT), "Blend");
		assert_eq!(n.input_name(MASK_INPUT), "Mask");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.keymix");
		assert_eq!(behavior.name(), "KeyMix");
		assert_eq!(behavior.categories(), &[Category::Math]);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.effect_input, "");

		for id in [TEXTURE_INPUT, BLEND_INPUT, MASK_INPUT] {
			let input = core.get_input(id).expect("texture input");
			assert_eq!(input.value_type, ValueType::Texture);
			assert_ne!(input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		}
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
	fn value_tex_only_pushes_tex() {
		let (core, behavior) = create();
		let input = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), input.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&input));
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
	fn value_mask_only_pushes_nothing() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(MASK_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn value_both_pushes_job_payload_with_mask() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
			(MASK_INPUT.to_string(), tex()),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.keymix");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert!(payload.params.contains_key(TEXTURE_INPUT));
		assert!(payload.params.contains_key(BLEND_INPUT));
		assert!(payload.params.contains_key(MASK_INPUT));
	}

	#[test]
	fn value_both_without_mask_still_pushes_job() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
		]);
		let payload = run(&core, &inputs);
		assert!(!payload.params.contains_key(MASK_INPUT));
	}

	#[test]
	fn shader_code_declares_inputs_without_switch() {
		let n = KeyMixNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform sampler2D tex_in;"));
		assert!(glsl.contains("uniform sampler2D blend_in;"));
		assert!(glsl.contains("uniform sampler2D mask_in;"));
		assert!(glsl.contains("uniform bool mask_in_enabled;"));
		assert!(glsl.contains("mask_col = vec4(1.0);"));
		assert!(glsl.contains("frag_color = mask_col.a > 0.0 ? blend_col : tex_col;"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "KeyMix");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.keymix",
		name: "KeyMix",
		categories: &[Category::Math],
		create,
	});
}
