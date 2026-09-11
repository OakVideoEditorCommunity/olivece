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

//! Opacity effect (C++ `src/node/src/effect/opacity/opacityeffect.{h,cpp}`,
//! `olive::OpacityEffect`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Opacity input id (C++ `k_value_input`). Type: float; default `1.0`;
/// properties: `view = percentage`, `min = 0.0`, `max = 1.0`.
pub const VALUE_INPUT: &str = "opacity_in";

/// Opacity effect node. Multiplies a texture's alpha by a 0..1 factor.
pub struct OpacityEffect {
	/// Owned child math node configured to `multiply` (C++ `math_`,
	/// formerly a QObject child).
	math: Box<super::math::MathNode>,
}

/// Fragment shader for the plain opacity path (C++ loads the
/// `:/shaders/opacity.frag` resource in `get_shader_code`).
/// Text copied verbatim from `engine/shaders/opacity.frag`.
const SHADER_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;
uniform float opacity_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  frag_color = texture(tex_in, ove_texcoord) * opacity_in;
}
"#;

/// Fragment shader for the `rgbmult` shader id (C++
/// `:/shaders/opacity_rgb.frag`), used when the opacity input is
/// itself a texture. Text copied verbatim from
/// `engine/shaders/opacity_rgb.frag`.
const SHADER_RGB_MULT_FRAG: &str = r#"// Inputs
uniform sampler2D tex_in;
uniform sampler2D opacity_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

vec3 rgb2hsv(vec3 c)
{
  vec4 K = vec4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
  vec4 p = mix(vec4(c.bg, K.wz), vec4(c.gb, K.xy), step(c.b, c.g));
  vec4 q = mix(vec4(p.xyw, c.r), vec4(c.r, p.yzx), step(p.x, c.r));

  float d = q.x - min(q.w, q.y);
  float e = 1.0e-10;
  return vec3(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

void main() {
  vec4 value = texture(opacity_in, ove_texcoord);
  float v = rgb2hsv(value.rgb).b;

  vec4 c = texture(tex_in, ove_texcoord);
  c *= v;
  frag_color = c;
}
"#;

impl OpacityEffect {
	/// Fragment shader for the plain opacity path (C++
	/// `get_shader_code()` default branch).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}

	/// Fragment shader for the `rgbmult` request (C++
	/// `get_shader_code()` `"rgbmult"` branch).
	fn shader_rgb_mult_frag() -> &'static str {
		SHADER_RGB_MULT_FRAG
	}
}

impl NodeBehavior for OpacityEffect {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Opacity"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.opacity"
	}

	/// Categories (C++ `category()`; note: C++ files Opacity under
	/// `k_category_filter` even though the sources live in `effect/`).
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Alter a video's opacity.\n\nThis is equivalent to multiplying a video by a number between 0.0 and 1.0."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Texture", `opacity_in` -> "Opacity".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			VALUE_INPUT => "Opacity",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// texture opacity input -> `rgbmult` shader job; scalar opacity
	/// != 1.0 -> plain shader job; opacity == 1.0 -> pass-through push
	/// of the input texture unchanged.
	///
	/// The job cases box a [`ShaderJobPayload`] that the renderer's
	/// resolve hook executes and replaces with the result texture; the
	/// params row carries the input texture and uniforms, keyed by the
	/// effect input (`// CPP-PARITY: opacityeffect.cpp` `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let tex = match inputs.get(TEXTURE_INPUT) {
			Some(tex @ crate::value::NodeValue::Texture(_)) => tex.clone(),
			_ => return,
		};

		// The shader-job box (C++ ShaderJob): the behavior's type id plus
		// the shader-variant id select the fragment source, and the effect
		// input key locates the main texture inside the params row. `time`
		// is diagnostics-only (C++ jobs keep the request timestamp).
		let job = |shader_id: &str| -> crate::value::NodeValue {
			crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: shader_id.to_string(),
				effect_input: core.effect_input.clone(),
				params: inputs.clone(),
				iterative_input: String::new(),
			})))
		};

		match inputs.get(VALUE_INPUT) {
			Some(crate::value::NodeValue::Texture(_)) => {
				// Texture opacity input: rgbmult shader job.
				table.push(crate::value::ValueType::Texture, job("rgbmult"), None);
			}
			Some(v) => {
				let opacity = v.to_double();
				// Same semantics as `!qFuzzyCompare(opacity, 1.0)`
				// (double overload).
				if (opacity - 1.0).abs() * 1e12 > opacity.abs().min(1.0) {
					table.push(crate::value::ValueType::Texture, job(""), None);
				} else {
					table.push(crate::value::ValueType::Texture, tex, None);
				}
			}
			None => {
				let opacity = core.value_at_time(VALUE_INPUT, -1, time).to_double();
				if (opacity - 1.0).abs() * 1e12 > opacity.abs().min(1.0) {
					table.push(crate::value::ValueType::Texture, job(""), None);
				} else {
					table.push(crate::value::ValueType::Texture, tex, None);
				}
			}
		}
	}

	/// Shader code request (C++ `get_shader_code()`): dispatch on
	/// request id (`"rgbmult"` vs default) between the two fragment
	/// shaders above.
	fn shader_code(&self, request: &str) -> Option<String> {
		if request == "rgbmult" {
			Some(SHADER_RGB_MULT_FRAG.to_string())
		} else {
			Some(SHADER_FRAG.to_string())
		}
	}

	/// Deep copy (C++ `copy()`); clones the owned child math node too.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(OpacityEffect {
			math: super::math::MathNode::new(),
		}))
	}
}

/// Constructor (C++ `OpacityEffect::OpacityEffect()`): builds the child
/// math node, adds `tex_in`/`opacity_in` with the defaults and
/// properties documented on the constants, sets the video-effect flag
/// and the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut opacity = crate::input::Input::new(
		VALUE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	opacity.properties = vec![
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("max".to_string(), crate::value::NodeValue::Float(1.0)),
	];
	core.add_input(opacity);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(
		core,
		Box::new(OpacityEffect {
			math: super::math::MathNode::new(),
		}),
	)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = OpacityEffect {
			math: super::super::math::MathNode::new(),
		};
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(VALUE_INPUT), "Opacity");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.opacity");
		assert_eq!(
			core.get_input(VALUE_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
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
	fn value_opacity_unity_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(VALUE_INPUT, -1, NodeValue::Float(1.0));
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_opacity_scaled_pushes_shader_job() {
		let (mut core, behavior) = create();
		core.set_standard_value(VALUE_INPUT, -1, NodeValue::Float(0.5));
		let inputs = crate::value::NodeValueRow::from([(
			TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		// The scaled-opacity path pushes the default-variant shader job
		// (a pass-through would push the input's null handle).
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("texture expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job pushed");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.opacity");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
	}

	#[test]
	fn value_opacity_in_row_scaled_pushes_shader_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(
				TEXTURE_INPUT.to_string(),
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
			(VALUE_INPUT.to_string(), NodeValue::Float(0.5)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("texture expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job pushed");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.opacity");
		assert_eq!(
			payload.params.get(VALUE_INPUT),
			Some(&NodeValue::Float(0.5)),
			"the opacity value rides in the job params"
		);
	}

	#[test]
	fn value_opacity_in_row_unity_passes_through() {
		let (core, behavior) = create();
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex.clone()),
			(VALUE_INPUT.to_string(), NodeValue::Float(1.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_texture_opacity_pushes_rgbmult_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(
				TEXTURE_INPUT.to_string(),
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
			(
				VALUE_INPUT.to_string(),
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		// A texture opacity input selects the rgbmult shader variant.
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("texture expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job pushed");
		assert_eq!(payload.shader_id, "rgbmult");
	}

	#[test]
	fn shader_code_dispatches() {
		let n = OpacityEffect {
			math: super::super::math::MathNode::new(),
		};
		assert!(n.shader_code("rgbmult").unwrap().contains("rgb2hsv"));
		assert!(n.shader_code("other").unwrap().contains("opacity_in"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Opacity");
	}
}

/// Register this node type (C++ `k_opacity_effect` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.opacity",
		name: "Opacity",
		categories: &[Category::Filter],
		create,
	});
}
