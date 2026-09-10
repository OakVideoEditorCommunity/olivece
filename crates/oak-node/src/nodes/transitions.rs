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

//! Transition node: blends the two textures a timeline transition block
//! sits between, driven by a `progress_in` factor.
//!
//! One behavior carries all four transition styles
//! ([`TYPE_NAMES`]); the `type_in` combo picks the fragment shader
//! through its shader id ([`SHADER_IDS`]) — the same
//! `node->get_shader_code(shader_id)` dispatch the math nodes use. The
//! two texture inputs are named `tex_in` ("from", the clip before the
//! cut) and `blend_in` ("to", the clip after it), matching the timeline
//! transition block's `out_block_in`/`in_block_in` wiring; `progress_in`
//! is the block's position across its own span, `0.0` at the cut's
//! outgoing side and `1.0` at its incoming side.
//!
//! Like [`crate::nodes::dissolve`], the node never sets a
//! `core.effect_input`: the both-present case boxes one
//! [`ShaderJobPayload`] whose param row carries both textures, and the
//! renderer binds `tex_in`/`blend_in` by name.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Outgoing ("from") texture input id. Type: texture; flags:
/// not-keyframable.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Incoming ("to") texture input id. Type: texture; flags:
/// not-keyframable.
pub const BLEND_INPUT: &str = "blend_in";

/// Transition progress input id. Type: float; default `0.0`;
/// properties: `min = 0.0`, `max = 1.0`, `view = percentage`. `0.0`
/// yields `tex_in`, `1.0` yields `blend_in`.
pub const PROGRESS_INPUT: &str = "progress_in";

/// Transition style input id (the combo the block and the inspector
/// both edit). Type: combo; flags: not-connectable, not-keyframable;
/// properties: `combobox_strings` = [`TYPE_NAMES`].
pub const TYPE_INPUT: &str = "type_in";

/// Localized style names, in combo-index order.
pub const TYPE_NAMES: [&str; 4] = ["Cross Dissolve", "Fade", "Wipe", "Slide"];

/// Shader ids for [`TYPE_NAMES`], by combo index. These are the strings
/// the timeline transition block stores so the composite driver can ask
/// the factory for the matching fragment source without instantiating
/// this node in the project graph.
pub const SHADER_IDS: [&str; 4] = ["crossdissolve", "fade", "wipe", "slide"];

/// The shader id for a combo index, clamping out-of-range values onto
/// cross dissolve (index `0`), the C++ default transition.
pub fn shader_id_for(index: i64) -> &'static str {
	SHADER_IDS[index.clamp(0, SHADER_IDS.len() as i64 - 1) as usize]
}

/// Cross dissolve: a straight mix, `50/50` at the midpoint.
const SHADER_CROSSDISSOLVE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;

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

    frag_color = mix(tex_col, blend_col, progress_in);
}
"#;

/// Fade: a dip through the fade color (black), outgoing image down over
/// the first half, incoming image up over the second — the classic
/// "fade to color" cut. The other end of the dip (white) is not exposed
/// yet: W5 exposes no fade-color parameter.
const SHADER_FADE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;

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
    vec4 fade_col = vec4(0.0, 0.0, 0.0, 1.0);

    if (progress_in < 0.5) {
        frag_color = mix(tex_col, fade_col, progress_in * 2.0);
    } else {
        frag_color = mix(fade_col, blend_col, (progress_in - 0.5) * 2.0);
    }
}
"#;

/// Wipe: a soft-edged boundary sweeps left to right, the incoming image
/// following behind it.
const SHADER_WIPE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;

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

    float soft = 0.02;
    float boundary = progress_in;
    float mask = smoothstep(boundary - soft, boundary + soft, ove_texcoord.x);

    frag_color = mix(blend_col, tex_col, mask);
}
"#;

/// Slide: the outgoing image is pushed off to the left while the
/// incoming image slides in from the right (the C++ "push" direction).
const SHADER_SLIDE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;

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

    vec2 from_uv = ove_texcoord + vec2(progress_in, 0.0);
    vec2 to_uv = ove_texcoord - vec2(1.0 - progress_in, 0.0);

    vec4 tex_col = texture(tex_in, from_uv);
    vec4 blend_col = texture(blend_in, to_uv);

    if (ove_texcoord.x < 1.0 - progress_in) {
        frag_color = tex_col;
    } else {
        frag_color = blend_col;
    }
}
"#;

/// Transition node. Unit-like: the style rides in `type_in`, so there is
/// no per-instance state.
pub struct TransitionNode;

impl TransitionNode {
	/// The fragment source for a shader id; an unknown id falls back to
	/// cross dissolve (the only style the timeline creates by default).
	fn shader_frag(shader_id: &str) -> &'static str {
		match shader_id {
			"fade" => SHADER_FADE,
			"wipe" => SHADER_WIPE,
			"slide" => SHADER_SLIDE,
			_ => SHADER_CROSSDISSOLVE,
		}
	}
}

impl NodeBehavior for TransitionNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Transition"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.transition"
	}

	/// Categories. Filed under timeline (the `Category` enum has no
	/// transition group; the transition *block* sits in the same one).
	fn categories(&self) -> &[Category] {
		&[Category::Timeline]
	}

	/// Description.
	fn description(&self) -> &str {
		"Blend the two sides of a cut with a transition style."
	}

	/// Localized input names: `tex_in` -> "From", `blend_in` -> "To",
	/// `progress_in` -> "Progress", `type_in` -> "Type" (the ids the
	/// transition block's `out_block_in`/`in_block_in` display names
	/// mirror).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "From",
			BLEND_INPUT => "To",
			PROGRESS_INPUT => "Progress",
			TYPE_INPUT => "Type",
			_ => id,
		}
	}

	/// Evaluate outputs: if only one texture is present, push it as-is
	/// (a transition against nothing is the input itself); if both are
	/// present, push one shader job over the whole input row; if
	/// neither, push nothing.
	///
	/// The resolved progress and the shader id the `type_in` combo
	/// selects are written into the job row, so the `progress_in`
	/// uniform is always present (the composite driver relies on that,
	/// and `run_effect`'s row is otherwise only as complete as the
	/// caller's).
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
				let progress = match inputs.get(PROGRESS_INPUT) {
					Some(v) => v.to_double(),
					None => core.value_at_time(PROGRESS_INPUT, -1, time).to_double(),
				};
				let ty = match inputs.get(TYPE_INPUT) {
					Some(v) => v.to_double() as i64,
					None => core.value_at_time(TYPE_INPUT, -1, time).to_double() as i64,
				};

				let mut params = inputs.clone();
				params.insert(PROGRESS_INPUT.to_string(), NodeValue::Float(progress));
				params.insert(TYPE_INPUT.to_string(), NodeValue::Combo(ty));

				table.push(
					ValueType::Texture,
					NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
						node_id: crate::id::NodeId::INVALID,
						time,
						iterations: 1,
						type_id: self.type_id().to_string(),
						shader_id: shader_id_for(ty).to_string(),
						effect_input: core.effect_input.clone(),
						params,
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

	/// Shader code request: the fragment source for the requested style
	/// id (see [`SHADER_IDS`]).
	fn shader_code(&self, request: &str) -> Option<String> {
		Some(Self::shader_frag(request).to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TransitionNode))
	}
}

/// Constructor: adds `tex_in`, `blend_in`, `progress_in` and `type_in`
/// with the defaults and properties documented on the constants and sets
/// the video-effect flag (no effect input: both textures bind by name).
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

	let mut progress = crate::input::Input::new(
		PROGRESS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	progress.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("max".to_string(), crate::value::NodeValue::Float(1.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	core.add_input(progress);

	let mut ty = crate::input::Input::new(
		TYPE_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	);
	ty.flags |= crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;
	ty.properties = vec![(
		"combobox_strings".to_string(),
		crate::value::NodeValue::Binary(TYPE_NAMES.join(",").into_bytes()),
	)];
	core.add_input(ty);

	core.flags |= crate::node::flags::VIDEO_EFFECT;

	(core, Box::new(TransitionNode))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn run(core: &NodeCore, inputs: &NodeValueRow) -> ShaderJobPayload {
		let behavior = TransitionNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	fn both() -> NodeValueRow {
		NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
		])
	}

	#[test]
	fn input_names() {
		let n = TransitionNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "From");
		assert_eq!(n.input_name(BLEND_INPUT), "To");
		assert_eq!(n.input_name(PROGRESS_INPUT), "Progress");
		assert_eq!(n.input_name(TYPE_INPUT), "Type");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.transition");
		assert_eq!(behavior.name(), "Transition");
		assert_eq!(behavior.categories(), &[Category::Timeline]);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.effect_input, "");

		for id in [TEXTURE_INPUT, BLEND_INPUT] {
			let input = core.get_input(id).expect("texture input");
			assert_ne!(input.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		}

		let progress = core.get_input(PROGRESS_INPUT).expect("progress input");
		assert_eq!(progress.default, NodeValue::Float(0.0));
		assert!(progress
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
		assert!(progress
			.properties
			.iter()
			.any(|(k, v)| k == "max" && *v == NodeValue::Float(1.0)));

		let ty = core.get_input(TYPE_INPUT).expect("type input");
		assert_eq!(ty.default, NodeValue::Combo(0));
		assert_ne!(ty.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_eq!(
			ty.properties.first(),
			Some(&(
				"combobox_strings".to_string(),
				NodeValue::Binary("Cross Dissolve,Fade,Wipe,Slide".as_bytes().to_vec())
			))
		);
	}

	#[test]
	fn shader_ids_cover_every_type_name() {
		assert_eq!(TYPE_NAMES.len(), SHADER_IDS.len());
		assert_eq!(shader_id_for(0), "crossdissolve");
		assert_eq!(shader_id_for(3), "slide");
		assert_eq!(shader_id_for(-1), "crossdissolve");
		assert_eq!(shader_id_for(7), "slide");
	}

	#[test]
	fn value_no_texture_pushes_nothing() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(&core, &NodeValueRow::default(), Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn value_tex_only_pushes_tex() {
		let (core, behavior) = create();
		let input = tex();
		let inputs = NodeValueRow::from([(TEXTURE_INPUT.to_string(), input.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&input));
	}

	#[test]
	fn value_blend_only_pushes_blend() {
		let (core, behavior) = create();
		let blend = tex();
		let inputs = NodeValueRow::from([(BLEND_INPUT.to_string(), blend.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&blend));
	}

	#[test]
	fn value_both_pushes_job_payload_with_both_textures() {
		let (core, _) = create();
		let payload = run(&core, &both());
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.transition");
		assert_eq!(payload.shader_id, "crossdissolve", "combo default is index 0");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert!(payload.params.contains_key(TEXTURE_INPUT));
		assert!(payload.params.contains_key(BLEND_INPUT));
		assert_eq!(
			payload.params.get(PROGRESS_INPUT),
			Some(&NodeValue::Float(0.0)),
			"the progress factor rides in the job params"
		);
	}

	#[test]
	fn value_takes_progress_and_type_from_row() {
		let (core, _) = create();
		let mut inputs = both();
		inputs.insert(PROGRESS_INPUT.to_string(), NodeValue::Float(0.75));
		inputs.insert(TYPE_INPUT.to_string(), NodeValue::Combo(2));
		let payload = run(&core, &inputs);
		assert_eq!(payload.params.get(PROGRESS_INPUT), Some(&NodeValue::Float(0.75)));
		assert_eq!(payload.shader_id, "wipe");
	}

	#[test]
	fn value_takes_type_from_core() {
		let (mut core, _) = create();
		core.set_standard_value(TYPE_INPUT, -1, NodeValue::Combo(3));
		let payload = run(&core, &both());
		assert_eq!(payload.shader_id, "slide");
	}

	#[test]
	fn shader_code_selects_variant_without_switch() {
		let n = TransitionNode;
		let sources: Vec<String> = SHADER_IDS
			.iter()
			.map(|id| n.shader_code(id).expect("shader source"))
			.collect();
		for (i, glsl) in sources.iter().enumerate() {
			assert!(glsl.contains("uniform sampler2D tex_in;"), "{i} binds tex_in");
			assert!(
				glsl.contains("uniform sampler2D blend_in;"),
				"{i} binds blend_in"
			);
			assert!(glsl.contains("uniform bool tex_in_enabled;"));
			assert!(glsl.contains("uniform bool blend_in_enabled;"));
			assert!(glsl.contains("uniform float progress_in;"));
			assert!(!glsl.contains("switch"));
		}
		for i in 0..sources.len() {
			for j in (i + 1)..sources.len() {
				assert_ne!(sources[i], sources[j], "types {i} and {j} differ");
			}
		}
		assert!(sources[0].contains("mix(tex_col, blend_col, progress_in)"));
		assert!(sources[3].contains("1.0 - progress_in"));
	}

	#[test]
	fn shader_code_unknown_id_falls_back_to_cross_dissolve() {
		let n = TransitionNode;
		assert_eq!(
			n.shader_code("nonsense"),
			n.shader_code(SHADER_IDS[0]),
			"an unknown style id renders as cross dissolve"
		);
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Transition");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.transition",
		name: "Transition",
		categories: &[Category::Timeline],
		create,
	});
}
