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

//! Unpremultiply effect (clean-room reimplementation of the OpenFX-Misc
//! `UnpremultOFX` / `net.sf.openfx.PremultPlugin` in its unpremultiply
//! mode; the `Premult` reference tree is used for parameter semantics
//! only, no code copied).
//!
//! The inverse of [`crate::nodes::premult`]: each RGB channel is divided
//! by the channel picked in [`CHANNEL_INPUT`] (default
//! [`Channel::Alpha`]) instead of multiplied by it, and the source alpha
//! rides through untouched. Upstream guards the division —
//! `!process_c || alpha <= FLT_EPSILON` keeps the source value
//! (`// CPP-PARITY: Premult.cpp`) — and the shader keeps the same guard.
//!
//! Single texture input, so `core.effect_input` is set to
//! [`TEXTURE_INPUT`] (the despill node's shape) and the job binds that
//! one sampler by name. `None` (or an out-of-range selector) divides by
//! one, leaving the image unchanged.

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Channel selector input id (upstream's "premult channel" choice, used
/// here as the divisor). Type: combo; default [`Channel::Alpha`]
/// (`Combo(4)`); flags: not-connectable, not-keyframable. Combo strings:
/// "None", "R", "G", "B", "A".
pub const CHANNEL_INPUT: &str = "unpremult_channel_in";

/// Channel selector values for [`CHANNEL_INPUT`], in combo order. The
/// numeric values are the shader's channel indices: `0` selects nothing
/// (the image passes through — a divisor of 1) and `1..=4` select
/// `r`/`g`/`b`/`a`. Only `Alpha` is constructed today (the runtime value
/// is the raw combo index); the full upstream set is kept (with an
/// `allow`) for parity.
#[allow(dead_code)]
#[repr(i64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
	/// No channel: the image is left unchanged.
	None = 0,
	/// Red channel.
	Red = 1,
	/// Green channel.
	Green = 2,
	/// Blue channel.
	Blue = 3,
	/// Alpha channel (the default divisor).
	Alpha = 4,
}

/// Unpremultiply node. Unit-like: there is no per-instance state.
pub struct UnpremultiplyNode;

/// Fragment shader: `rgb /= selected_channel` with the upstream
/// near-zero guard. The channel dispatch is an if/else chain and not a
/// `switch` (naga rejects the latter); an out-of-range selector falls
/// back to a divisor of 1, the "None" combo entry. No `tex_in_enabled`
/// flag is declared: with a single texture input the sampler is either
/// bound or the job never runs (`value()` requires the texture), and the
/// despill node's shader has the same shape.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform int unpremult_channel_in;

in vec2 ove_texcoord;
out vec4 frag_color;

float selected_channel(vec4 col, int channel) {
    if (channel == 1) {
        return col.r;
    } else if (channel == 2) {
        return col.g;
    } else if (channel == 3) {
        return col.b;
    } else if (channel == 4) {
        return col.a;
    }
    return 1.0;
}

void main(void) {
    vec4 col = texture(tex_in, ove_texcoord);
    float factor = selected_channel(col, unpremult_channel_in);

    // Upstream keeps the source value when the divisor is (near) zero
    // (`alpha <= FLT_EPSILON` in Premult.cpp), which is also what saves
    // an unpainted alpha of 0 from blowing the RGB channels up.
    if (factor <= 0.000001) {
        frag_color = col;
        return;
    }

    frag_color = vec4(col.rgb / factor, col.a);
}
"#;

impl UnpremultiplyNode {
	/// Fragment shader (single variant, so the request id is ignored).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for UnpremultiplyNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Unpremultiply"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.unpremult"
	}

	/// Categories. Filed under math like the alpha-over
	/// [`crate::nodes::merge`] (the `Category` enum has no merge group).
	fn categories(&self) -> &[Category] {
		&[Category::Math]
	}

	/// Description.
	fn description(&self) -> &str {
		"Divide the RGB channels by a selected channel (zero guarded)."
	}

	/// Localized input names: `tex_in` -> "Input", `unpremult_channel_in`
	/// -> "Channel".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			CHANNEL_INPUT => "Channel",
			_ => id,
		}
	}

	/// Combo input option labels: `unpremult_channel_in` -> "None", "R",
	/// "G", "B", "A" (the [`Channel`] order).
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			CHANNEL_INPUT => vec!["None", "R", "G", "B", "A"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs: with no texture there is nothing to divide;
	/// otherwise push one shader job over the whole input row, with the
	/// resolved channel selector written in so the uniform is always
	/// present even when the incoming row only carries the texture (the
	/// row/standard-value split mirrors the directional-blur node).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		use crate::value::{NodeValue, ValueType};

		match inputs.get(TEXTURE_INPUT) {
			Some(NodeValue::Texture(_)) => {}
			_ => return,
		}

		let channel = match inputs.get(CHANNEL_INPUT) {
			Some(v) => v.clone(),
			None => core.value_at_time(CHANNEL_INPUT, -1, time),
		};

		let mut params = inputs.clone();
		params.insert(CHANNEL_INPUT.to_string(), channel);

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

	/// Shader code request: always the unpremultiply fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(UnpremultiplyNode))
	}
}

/// Constructor: adds `tex_in` (not-keyframable) and
/// `unpremult_channel_in` (combo, default alpha), sets the video-effect
/// flag and makes `tex_in` the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut channel = crate::input::Input::new(
		CHANNEL_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(Channel::Alpha as i64),
	);
	channel.flags |= crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(channel);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(UnpremultiplyNode))
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
		let behavior = UnpremultiplyNode;
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
		let n = UnpremultiplyNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(CHANNEL_INPUT), "Channel");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn combo_strings_match_channel_values() {
		let n = UnpremultiplyNode;
		let strings = n.input_combo_strings(CHANNEL_INPUT);
		assert_eq!(strings, vec!["None", "R", "G", "B", "A"]);
		assert_eq!(Channel::None as i64, 0);
		assert_eq!(Channel::Red as i64, 1);
		assert_eq!(Channel::Green as i64, 2);
		assert_eq!(Channel::Blue as i64, 3);
		assert_eq!(Channel::Alpha as i64, 4);
		assert!(n.input_combo_strings("other").is_empty());
	}

	#[test]
	fn create_wires_inputs_flags_and_defaults() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.unpremult");
		assert_eq!(behavior.name(), "Unpremultiply");
		assert_eq!(behavior.categories(), &[Category::Math]);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.effect_input, TEXTURE_INPUT);

		let tex_input = core.get_input(TEXTURE_INPUT).expect("texture input");
		assert_eq!(tex_input.value_type, ValueType::Texture);
		assert_ne!(tex_input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);

		let channel = core.get_input(CHANNEL_INPUT).expect("channel input");
		assert_eq!(channel.value_type, ValueType::Combo);
		assert_eq!(channel.default, NodeValue::Combo(Channel::Alpha as i64));
		assert_ne!(channel.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_ne!(channel.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
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
	fn value_with_texture_defaults_channel_to_alpha() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.unpremult");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(
			payload.params.get(CHANNEL_INPUT),
			Some(&NodeValue::Combo(Channel::Alpha as i64)),
			"the default channel rides in the job params"
		);
	}

	#[test]
	fn value_takes_channel_from_row() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(
				CHANNEL_INPUT.to_string(),
				NodeValue::Combo(Channel::Red as i64),
			),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(CHANNEL_INPUT),
			Some(&NodeValue::Combo(Channel::Red as i64))
		);
	}

	#[test]
	fn shader_code_declares_inputs_and_zero_guard() {
		let n = UnpremultiplyNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform sampler2D tex_in;"));
		assert!(glsl.contains("uniform int unpremult_channel_in;"));
		assert!(glsl.contains("if (factor <= 0.000001) {"));
		assert!(glsl.contains("frag_color = vec4(col.rgb / factor, col.a);"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Unpremultiply");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.unpremult",
		name: "Unpremultiply",
		categories: &[Category::Math],
		create,
	});
}
