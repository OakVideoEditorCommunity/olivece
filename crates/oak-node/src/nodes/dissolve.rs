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

//! Dissolve effect (clean-room reimplementation of the OpenFX-Misc
//! `DissolveOFX` / `net.sf.openfx.DissolvePlugin`; the `Dissolve`
//! reference tree is used for parameter semantics only, no code copied).
//!
//! Upstream cross-fades a stack of inputs with a single `which` mix
//! factor — Natron folds a many-input dissolve into one effect, and for
//! two inputs that per-pixel result is the weighted average
//! `(1 - mix) * a + mix * b`. This node implements exactly that: two
//! texture inputs and a `mix_in` factor, so it is the atomic piece a
//! transition would be built from.
//!
//! Like [`crate::nodes::merge`], the node never sets a `core.effect_input`;
//! the both-present case boxes one [`ShaderJobPayload`] whose params row
//! carries both textures, and the renderer binds `tex_in`/`blend_in` by
//! name (the pass size follows the first bound texture — `blend_in`, the
//! alphabetically first key of the job's `BTreeMap` row).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// First ("from") texture input id. Type: texture; flags:
/// not-keyframable.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Second ("to") texture input id. Type: texture; flags:
/// not-keyframable.
pub const BLEND_INPUT: &str = "blend_in";

/// Mix factor input id (upstream `which`, "Which"). Type: float; default
/// `0.5`; properties: `min = 0.0`, `max = 1.0`, `view = percentage`.
/// `0.0` yields `tex_in`, `1.0` yields `blend_in`. Upstream leaves its
/// default at OFX's `0.0` because `which` is an input *index* there;
/// with two inputs a neutral 50/50 dissolve is the more useful default
/// (`// CPP-PARITY: Dissolve.cpp` `describeInContext`).
pub const MIX_INPUT: &str = "mix_in";

/// Dissolve node. Unit-like: there is no per-instance state.
pub struct DissolveNode;

/// Fragment shader: cross-fade the two inputs by `mix_in`. The
/// `_enabled` flags mirror the alpha-over shader's convention for
/// unbound samplers (the renderer fills them in from the job's bound
/// textures; `run_effect`'s first-sampler fallback cannot misfire here
/// because `value()` passes the missing input through on the CPU side
/// instead of pushing a job). `switch` is deliberately not used (naga
/// rejects it).
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float mix_in;

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

    frag_color = mix(tex_col, blend_col, mix_in);
}
"#;

impl DissolveNode {
	/// Fragment shader (single variant, so the request id is ignored).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for DissolveNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Dissolve"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.dissolve"
	}

	/// Categories. Filed under math like the alpha-over
	/// [`crate::nodes::merge`] (the `Category` enum has no merge group).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description.
	fn description(&self) -> &str {
		"Dissolve between two textures by a mix factor."
	}

	/// Localized input names: `tex_in` -> "Input", `blend_in` ->
	/// "Blend", `mix_in` -> "Mix".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			BLEND_INPUT => "Blend",
			MIX_INPUT => "Mix",
			_ => id,
		}
	}

	/// Evaluate outputs: if only one texture is present, push it as-is
	/// (a dissolve against nothing is the input itself — pushing a job
	/// would also let `run_effect`'s first-sampler fallback bind the
	/// lone texture to the missing one); if both are present, push one
	/// shader job over the whole input row; if neither, push nothing.
	///
	/// The resolved mix factor is written into the job row so the
	/// `mix_in` uniform is always present, even when the incoming row
	/// only carries the textures (the row/standard-value split mirrors
	/// the directional-blur node).
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
				let mix = match inputs.get(MIX_INPUT) {
					Some(v) => v.to_double(),
					None => core.value_at_time(MIX_INPUT, -1, time).to_double(),
				};

				let mut params = inputs.clone();
				params.insert(MIX_INPUT.to_string(), NodeValue::Float(mix));

				table.push(
					ValueType::Texture,
					NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
						node_id: crate::id::NodeId::INVALID,
						time,
						iterations: 1,
						type_id: self.type_id().to_string(),
						shader_id: String::new(),
						effect_input: core.effect_input.clone(),
						params,
						iterative_input: String::new(),
					}))),
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

	/// Shader code request: always the cross-fade fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(DissolveNode))
	}
}

/// Constructor: adds `tex_in`, `blend_in` and `mix_in` with the defaults
/// and properties documented on the constants and sets the video-effect
/// flag (no effect input: both textures bind by name).
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut blend = crate::input::Input::new(
		BLEND_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	blend.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(blend);

	let mut mix = crate::input::Input::new(
		MIX_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.5),
	);
	mix.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("max".to_string(), crate::value::NodeValue::Float(1.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	core.add_input(mix);

	core.flags |= crate::node::flags::VIDEO_EFFECT;

	(core, Box::new(DissolveNode))
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
		let behavior = DissolveNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	#[test]
	fn input_names() {
		let n = DissolveNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(BLEND_INPUT), "Blend");
		assert_eq!(n.input_name(MIX_INPUT), "Mix");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.dissolve");
		assert_eq!(behavior.name(), "Dissolve");
		assert_eq!(behavior.categories(), &[Category::Math]);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.effect_input, "");

		for id in [TEXTURE_INPUT, BLEND_INPUT] {
			let input = core.get_input(id).expect("texture input");
			assert_ne!(input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		}

		let mix = core.get_input(MIX_INPUT).expect("mix input");
		assert_eq!(mix.default, NodeValue::Float(0.5));
		assert!(mix
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
		assert!(mix
			.properties
			.iter()
			.any(|(k, v)| k == "max" && *v == NodeValue::Float(1.0)));
		assert!(mix
			.properties
			.iter()
			.any(|(k, v)| k == "view" && *v == NodeValue::Text("percentage".into())));
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
	fn value_both_pushes_job_payload_with_both_textures() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.dissolve");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert!(payload.params.contains_key(TEXTURE_INPUT));
		assert!(payload.params.contains_key(BLEND_INPUT));
		assert_eq!(
			payload.params.get(MIX_INPUT),
			Some(&NodeValue::Float(0.5)),
			"the mix factor rides in the job params"
		);
	}

	#[test]
	fn value_takes_mix_from_row() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
			(MIX_INPUT.to_string(), NodeValue::Float(1.0)),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.params.get(MIX_INPUT), Some(&NodeValue::Float(1.0)));
	}

	#[test]
	fn value_takes_mix_from_core() {
		let (mut core, _) = create();
		core.set_standard_value(MIX_INPUT, -1, NodeValue::Float(0.25));
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.params.get(MIX_INPUT), Some(&NodeValue::Float(0.25)));
	}

	#[test]
	fn shader_code_declares_inputs_without_switch() {
		let n = DissolveNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform sampler2D tex_in;"));
		assert!(glsl.contains("uniform sampler2D blend_in;"));
		assert!(glsl.contains("uniform bool tex_in_enabled;"));
		assert!(glsl.contains("uniform bool blend_in_enabled;"));
		assert!(glsl.contains("uniform float mix_in;"));
		assert!(glsl.contains("frag_color = mix(tex_col, blend_col, mix_in);"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Dissolve");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.dissolve",
		name: "Dissolve",
		categories: &[Category::Math],
		create,
	});
}
