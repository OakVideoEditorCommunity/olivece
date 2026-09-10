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

//! Layer transition node: the adjustment-layer form of a transition
//! (`docs/zh/plans/adjustment-layers-and-transitions.md` §4.2), the
//! counterpart of the timeline transition *block* whose node lives in
//! [`crate::nodes::transitions`].
//!
//! The node sits in the effect chain of an adjustment layer covering the
//! seam. `tex_in` is the chain input (`core.effect_input`), so the render
//! driver feeds it the composited lower layers; `blend_in` is the second
//! texture — the pre-seam block's output by convention — bound by name
//! from the job row, the same dual-texture mode [`crate::nodes::merge`]
//! uses. [`crate::nodes::transitions`] boxes its job the same way; the
//! difference is the chain input, which the timeline transition block
//! does not have (its two sides arrive as a pair).
//!
//! `progress_in` is the layer's position across the adjustment block's
//! own span. The renderer fills it in for the duration of an adjustment
//! sweep (`oakrender::eval`'s `hooks.layer_progress`, which pre-fills the
//! row of any job whose shader declares the uniform and whose params do
//! not already carry it), so the node writes `progress_in` into the job
//! row **only when the user set it explicitly** — a row carrying the
//! default `0.0` would otherwise pin the node to the very start of the
//! span. [`TransitionFxNode::value`] documents the "explicit" test.
//!
//! One behavior carries all four styles ([`TYPE_NAMES`]), dispatched by
//! the `type_in` combo through its shader id ([`SHADER_IDS`]) — the
//! `node->get_shader_code(shader_id)` path the math nodes and the
//! timeline transition node use. The fragment sources are the timeline
//! node's, adapted to the one-picture chain form: the fade mixes against
//! its own `color_in` (the layer form has a single picture, so there is
//! no dip between two of them), and the wipe/slide styles take a
//! `direction_in` sweep direction.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Chain texture input id. Type: texture; flags: not-keyframable. This
/// is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Second texture input id (the pre-seam picture by convention). Type:
/// texture; flags: not-keyframable.
pub const BLEND_INPUT: &str = "blend_in";

/// Transition progress input id. Type: float; default `0.0`;
/// properties: `min = 0.0`, `max = 1.0`, `view = percentage`. `0.0`
/// yields `tex_in`, `1.0` yields `blend_in`. Left out of the job row
/// unless the user set it, so the renderer can fill in the adjustment
/// layer's progress instead.
pub const PROGRESS_INPUT: &str = "progress_in";

/// Fade target color input id. Type: color; default opaque black. Only
/// the fade style samples it (`progress_in = 0` is all `color_in`,
/// `1` is the input untouched).
pub const COLOR_INPUT: &str = "color_in";

/// Wipe/slide sweep direction input id. Type: combo; flags:
/// not-connectable, not-keyframable; properties: `combobox_strings` =
/// [`DIRECTION_NAMES`].
pub const DIRECTION_INPUT: &str = "direction_in";

/// Transition style input id. Type: combo; flags: not-connectable,
/// not-keyframable; properties: `combobox_strings` = [`TYPE_NAMES`].
pub const TYPE_INPUT: &str = "type_in";

/// Localized style names, in combo-index order.
pub const TYPE_NAMES: [&str; 4] = ["Cross Dissolve", "Fade", "Wipe", "Slide"];

/// Shader ids for [`TYPE_NAMES`], by combo index. The strings match the
/// timeline transition block's (`crate::nodes::transitions::SHADER_IDS`),
/// so both transition forms ask the factory for the same style.
pub const SHADER_IDS: [&str; 4] = ["crossdissolve", "fade", "wipe", "slide"];

/// Sweep direction names for [`DIRECTION_INPUT`], in combo-index order:
/// the side the boundary sweeps towards. The incoming picture is
/// revealed from the opposite edge (`Left to Right` wipes the `blend_in`
/// picture in from the left, `blend_in` replacing `tex_in` behind the
/// boundary).
pub const DIRECTION_NAMES: [&str; 4] = [
	"Left to Right",
	"Right to Left",
	"Top to Bottom",
	"Bottom to Top",
];

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

/// Fade: the input fades up out of `color_in` (black by default) —
/// `progress_in = 0` is the fade color, `1` the picture untouched. The
/// layer form has one picture (its `blend_in` is the optional pre-seam
/// reference), so the two-sided dip of the timeline block's fade has no
/// pair to dip through here.
const SHADER_FADE: &str = r#"uniform sampler2D tex_in;
uniform bool tex_in_enabled;
uniform vec4 color_in;
uniform float progress_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    if (!tex_in_enabled) {
        frag_color = vec4(0.0);
        return;
    }

    vec4 tex_col = texture(tex_in, ove_texcoord);

    frag_color = mix(color_in, tex_col, progress_in);
}
"#;

/// Wipe: a soft-edged boundary sweeps across the frame, the incoming
/// picture following behind it. The direction combo picks the axis and
/// the sign with two steps and two mixes — no branch on the index, so
/// all four directions are one path.
const SHADER_WIPE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;
uniform float direction_in;

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

    // Direction 0..3: 0/1 are horizontal, 2/3 vertical (the axis), and
    // the odd entries sweep the other way (the sign).
    float d = clamp(direction_in, 0.0, 3.0);
    float horiz = 1.0 - step(1.5, d);
    float neg = step(1.0, mod(d, 2.0));
    float sweep = mix(ove_texcoord.y, ove_texcoord.x, horiz);
    float boundary = mix(sweep, 1.0 - sweep, neg);

    float soft = 0.02;
    float mask = smoothstep(progress_in - soft, progress_in + soft, boundary);

    frag_color = mix(blend_col, tex_col, mask);
}
"#;

/// Slide: the incoming picture pushes the outgoing one off the frame,
/// both travelling the direction the combo names. The offsets mirror
/// each other (`-e * progress` against `+e * (1 - progress)`), so the
/// pair stays adjacent and neither picture is stretched.
const SHADER_SLIDE: &str = r#"uniform sampler2D tex_in;
uniform sampler2D blend_in;
uniform bool tex_in_enabled;
uniform bool blend_in_enabled;
uniform float progress_in;
uniform float direction_in;

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

    // Direction 0..3: 0/1 are horizontal, 2/3 vertical (the axis), and
    // the odd entries travel the other way (the sign).
    float d = clamp(direction_in, 0.0, 3.0);
    float horiz = 1.0 - step(1.5, d);
    float neg = step(1.0, mod(d, 2.0));
    vec2 e = vec2(horiz, 1.0 - horiz) * (1.0 - 2.0 * neg);

    float sweep = mix(ove_texcoord.y, ove_texcoord.x, horiz);
    float boundary = mix(sweep, 1.0 - sweep, neg);

    vec2 from_uv = ove_texcoord - e * progress_in;
    vec2 to_uv = ove_texcoord + e * (1.0 - progress_in);

    vec4 tex_col = texture(tex_in, from_uv);
    vec4 blend_col = texture(blend_in, to_uv);

    if (boundary < progress_in) {
        frag_color = blend_col;
    } else {
        frag_color = tex_col;
    }
}
"#;

/// Layer transition node. Unit-like: the style rides in `type_in`, so
/// there is no per-instance state.
pub struct TransitionFxNode;

impl TransitionFxNode {
	/// The fragment source for a shader id; an unknown id falls back to
	/// cross dissolve.
	fn shader_frag(shader_id: &str) -> &'static str {
		match shader_id {
			"fade" => SHADER_FADE,
			"wipe" => SHADER_WIPE,
			"slide" => SHADER_SLIDE,
			_ => SHADER_CROSSDISSOLVE,
		}
	}
}

impl NodeBehavior for TransitionFxNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Transition FX"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.transitionfx"
	}

	/// Categories. Filed under effect: the `Category` enum has no
	/// transition group, and this is a video effect on a chain (the
	/// timeline transition block carries `Category::Timeline` instead).
	fn categories(&self) -> &[Category] {
		&[Category::Effect]
	}

	/// Description.
	fn description(&self) -> &str {
		"Blend a picture with a second one across an adjustment layer's span."
	}

	/// Localized input names: `tex_in` -> "From", `blend_in` -> "To",
	/// `progress_in` -> "Progress", `color_in` -> "Color",
	/// `direction_in` -> "Direction", `type_in` -> "Type".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "From",
			BLEND_INPUT => "To",
			PROGRESS_INPUT => "Progress",
			COLOR_INPUT => "Color",
			DIRECTION_INPUT => "Direction",
			TYPE_INPUT => "Type",
			_ => id,
		}
	}

	/// Evaluate outputs: if only one texture is present, push it as-is
	/// (a transition against nothing is the input itself); if both are
	/// present, push one shader job over the whole input row; if
	/// neither, push nothing.
	///
	/// The shader id the `type_in` combo selects is written into the job
	/// row. `progress_in` is deliberately **not** written when the user
	/// never set it: the renderer pre-fills the row from the adjustment
	/// layer's own position (`hooks.layer_progress`) and only a missing
	/// param lets that through. "Set" means the row value differs from
	/// the `0.0` default or the input is keyframed — the standard-value
	/// map cannot answer it, because the project serializer writes a
	/// `<standard>` element for every input, so a reloaded node always
	/// has one.
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
				let ty = match inputs.get(TYPE_INPUT) {
					Some(v) => v.to_double() as i64,
					None => core.value_at_time(TYPE_INPUT, -1, time).to_double() as i64,
				};

				let mut params = inputs.clone();
				params.insert(TYPE_INPUT.to_string(), NodeValue::Combo(ty));

				let progress_set = inputs
					.get(PROGRESS_INPUT)
					.map(|v| v.to_double() != 0.0)
					.unwrap_or(false)
					|| core.is_input_keyframing(PROGRESS_INPUT, -1);
				if !progress_set {
					params.remove(PROGRESS_INPUT);
				}

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
		Some(Box::new(TransitionFxNode))
	}
}

/// Constructor: adds the two textures, `progress_in`, `color_in`,
/// `direction_in` and `type_in` with the defaults and properties
/// documented on the constants, and sets the video-effect flag with
/// `tex_in` as the effect input (the chain hangs off it; `blend_in`
/// binds by name alongside).
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

	let color = crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([0.0, 0.0, 0.0, 1.0]),
	);
	core.add_input(color);

	let mut direction = crate::input::Input::new(
		DIRECTION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	);
	direction.flags |= crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;
	direction.properties = vec![(
		"combobox_strings".to_string(),
		crate::value::NodeValue::Binary(DIRECTION_NAMES.join(",").into_bytes()),
	)];
	core.add_input(direction);

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
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(TransitionFxNode))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::keyframe::{Interpolation, Keyframe};
	use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn run(core: &NodeCore, inputs: &NodeValueRow) -> ShaderJobPayload {
		let behavior = TransitionFxNode;
		let mut table = NodeValueTable::default();
		behavior.value(core, inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
			.expect("shader job payload boxed")
			.clone()
	}

	/// The row the graph traverser builds for an unconnected node with
	/// both textures: every input is present, `progress_in` carrying the
	/// input default.
	fn both() -> NodeValueRow {
		NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(BLEND_INPUT.to_string(), tex()),
			(PROGRESS_INPUT.to_string(), NodeValue::Float(0.0)),
			(COLOR_INPUT.to_string(), NodeValue::Color([0.0, 0.0, 0.0, 1.0])),
			(DIRECTION_INPUT.to_string(), NodeValue::Combo(0)),
		])
	}

	#[test]
	fn input_names() {
		let n = TransitionFxNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "From");
		assert_eq!(n.input_name(BLEND_INPUT), "To");
		assert_eq!(n.input_name(PROGRESS_INPUT), "Progress");
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
		assert_eq!(n.input_name(DIRECTION_INPUT), "Direction");
		assert_eq!(n.input_name(TYPE_INPUT), "Type");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.transitionfx"
		);
		assert_eq!(behavior.name(), "Transition FX");
		assert_eq!(behavior.categories(), &[Category::Effect]);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.effect_input, TEXTURE_INPUT, "the chain hangs off tex_in");
		assert_eq!(
			core.get_input(TEXTURE_INPUT).map(|i| i.value_type),
			Some(ValueType::Texture)
		);

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

		let color = core.get_input(COLOR_INPUT).expect("color input");
		assert_eq!(color.default, NodeValue::Color([0.0, 0.0, 0.0, 1.0]));

		let direction = core.get_input(DIRECTION_INPUT).expect("direction input");
		assert_eq!(direction.default, NodeValue::Combo(0));
		assert_ne!(direction.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_eq!(
			direction.properties.first(),
			Some(&(
				"combobox_strings".to_string(),
				NodeValue::Binary(DIRECTION_NAMES.join(",").into_bytes())
			))
		);

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
		assert_eq!(shader_id_for(1), "fade");
		assert_eq!(shader_id_for(2), "wipe");
		assert_eq!(shader_id_for(3), "slide");
		assert_eq!(shader_id_for(-1), "crossdissolve");
		assert_eq!(shader_id_for(7), "slide");
	}

	#[test]
	fn direction_names_cover_every_index() {
		assert_eq!(DIRECTION_NAMES.len(), 4);
		for (i, name) in DIRECTION_NAMES.iter().enumerate() {
			assert!(!name.is_empty(), "{i} has a label");
		}
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
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.transitionfx");
		assert_eq!(payload.shader_id, "crossdissolve", "combo default is index 0");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert!(payload.params.contains_key(TEXTURE_INPUT));
		assert!(payload.params.contains_key(BLEND_INPUT));
		assert!(
			!payload.params.contains_key(PROGRESS_INPUT),
			"the default progress stays out of the row so the layer can fill it"
		);
		assert_eq!(
			payload.params.get(COLOR_INPUT),
			Some(&NodeValue::Color([0.0, 0.0, 0.0, 1.0])),
			"the style parameters ride along"
		);
	}

	#[test]
	fn value_takes_type_from_row_and_core() {
		let (mut core, _) = create();
		let mut inputs = both();
		inputs.insert(TYPE_INPUT.to_string(), NodeValue::Combo(2));
		assert_eq!(run(&core, &inputs).shader_id, "wipe");

		core.set_standard_value(TYPE_INPUT, -1, NodeValue::Combo(3));
		assert_eq!(run(&core, &both()).shader_id, "slide");
	}

	#[test]
	fn value_explicit_progress_stays_in_the_job() {
		let (core, _) = create();
		let mut inputs = both();
		inputs.insert(PROGRESS_INPUT.to_string(), NodeValue::Float(0.75));
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(PROGRESS_INPUT),
			Some(&NodeValue::Float(0.75)),
			"a user-set progress overrides the layer sweep"
		);
	}

	#[test]
	fn value_keyframed_progress_stays_in_the_job() {
		let (mut core, _) = create();
		core.keyframe_track_mut(PROGRESS_INPUT, -1)
			.set_key(Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(0.0),
				interpolation: Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		let mut inputs = both();
		inputs.insert(
			PROGRESS_INPUT.to_string(),
			core.value_at_time(PROGRESS_INPUT, -1, Rational::new(0, 1)),
		);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(PROGRESS_INPUT),
			Some(&NodeValue::Float(0.0)),
			"a keyframed progress is authored, even when its value is 0"
		);
	}

	#[test]
	fn shader_code_selects_variant_without_switch() {
		let n = TransitionFxNode;
		let sources: Vec<String> = SHADER_IDS
			.iter()
			.map(|id| n.shader_code(id).expect("shader source"))
			.collect();
		for (i, glsl) in sources.iter().enumerate() {
			assert!(glsl.contains("uniform sampler2D tex_in;"), "{i} binds tex_in");
			assert!(glsl.contains("uniform bool tex_in_enabled;"), "{i}");
			assert!(glsl.contains("uniform float progress_in;"), "{i}");
			assert!(!glsl.contains("switch"));
		}
		for i in [0usize, 2, 3] {
			assert!(
				sources[i].contains("uniform sampler2D blend_in;"),
				"{i} binds the second picture"
			);
			assert!(sources[i].contains("uniform bool blend_in_enabled;"), "{i}");
		}
		assert!(sources[1].contains("uniform vec4 color_in;"), "fade fades");
		for i in [2usize, 3] {
			assert!(
				sources[i].contains("uniform float direction_in;"),
				"{i} sweeps"
			);
		}
		for i in 0..sources.len() {
			for j in (i + 1)..sources.len() {
				assert_ne!(sources[i], sources[j], "types {i} and {j} differ");
			}
		}
		assert!(sources[0].contains("mix(tex_col, blend_col, progress_in)"));
		assert!(sources[1].contains("mix(color_in, tex_col, progress_in)"));
	}

	#[test]
	fn shader_code_unknown_id_falls_back_to_cross_dissolve() {
		let n = TransitionFxNode;
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
		assert_eq!(dup.name(), "Transition FX");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.transitionfx",
		name: "Transition FX",
		categories: &[Category::Effect],
		create,
	});
}
