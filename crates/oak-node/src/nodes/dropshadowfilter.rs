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

//! Drop shadow filter (C++
//! `src/node/src/filter/dropshadow/dropshadowfilter.{h,cpp}`,
//! `olive::DropShadowFilter`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Shadow color input id (C++ `k_color_input`). Type: color; default
/// black `(0.0, 0.0, 0.0)`.
pub const COLOR_INPUT: &str = "color_in";

/// Shadow distance input id (C++ `k_distance_input`). Type: float;
/// default `10.0`.
pub const DISTANCE_INPUT: &str = "distance_in";

/// Shadow angle input id (C++ `k_angle_input`). Type: float; default
/// `135.0`.
pub const ANGLE_INPUT: &str = "angle_in";

/// Shadow softness input id (C++ `k_softness_input`). Type: float;
/// default `10.0`; properties: `min = 0.0`.
pub const SOFTNESS_INPUT: &str = "radius_in";

/// Shadow opacity input id (C++ `k_opacity_input`). Type: float;
/// default `1.0`; properties: `min = 0.0`, `view = percentage`.
pub const OPACITY_INPUT: &str = "opacity_in";

/// Fast/low-quality toggle input id (C++ `k_fast_input`). Type: bool;
/// default `false`.
pub const FAST_INPUT: &str = "fast_in";

/// Iterative-input texture id (C++ ShaderJob param
/// `previous_iteration_in`). Not a real node input: the C++ `value()`
/// inserts it into the job params directly, so it only exists here as
/// the feedback slot between the two blur passes and the merge step.
pub const ITERATIVE_INPUT: &str = "previous_iteration_in";

/// Drop shadow filter node. Adds a colored, blurred, offset copy of the
/// input's alpha behind the image. The C++ class declares no own member
/// fields.
pub struct DropShadowFilter;

/// Fragment shader (C++ `get_shader_code()` loads
/// `:/shaders/dropshadow.frag` via FileFunctions for any request). Text
/// copied verbatim from `engine/shaders/dropshadow.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform vec4 color_in;
uniform float distance_in;
uniform float angle_in;
uniform float radius_in;
uniform float opacity_in;
uniform vec2 resolution_in;
uniform sampler2D previous_iteration_in;
uniform bool fast_in;

uniform int ove_iteration;

in vec2 ove_texcoord;
out vec4 frag_color;

// Gaussian function uses PI
#define M_PI 3.1415926535897932384626433832795

// Single gaussian formula (unused, mainly here for documentation/just in case)
//float gaussian(float x, float sigma) {
//    return (1.0/(sigma*sqrt(2.0*M_PI)))*exp(-0.5*pow(x/sigma, 2.0));
//}

// Double gaussian formula, actually used in the code below
// Should be faster than the single gaussian above since it doesn't need sqrt()
float gaussian2(float x, float y, float sigma) {
  return (1.0/((sigma*sigma)*2.0*M_PI))*exp(-0.5*(((x*x) + (y*y))/(sigma*sigma)));
}

void main(void) {
  if (ove_iteration == 2 || radius_in == 0.0) {

    // Merge step
    vec4 composite = texture(tex_in, ove_texcoord);
    if (composite.a < 1.0) {
      // Convert degrees to radians
      float shadow_angle = ((angle_in + 90.0)*M_PI)/180.0;

      vec2 shadow_offset = vec2(cos(shadow_angle) * distance_in, sin(shadow_angle) * distance_in);
      shadow_offset /= resolution_in;
      shadow_offset += ove_texcoord;

      vec4 shadow_color = texture(previous_iteration_in, shadow_offset);

      shadow_color.rgb = color_in.rgb * shadow_color.a;
      shadow_color *= 1.0 - composite.a;
      shadow_color *= opacity_in;

      composite += shadow_color;
    }
    frag_color = composite;

  } else {
    // We only sample on hard pixels, so we don't accept decimal radii
    float real_radius = ceil(radius_in);

    vec4 composite = vec4(0.0);

    float divider, sigma;

    if (fast_in) {

      // Calculate the weight of each pixel based on the radius
      divider = 1.0 / real_radius;

    } else {

      // Using (radius = 3 * sigma) because 3 standard deviations covers 97% of the blur according to this document:
      // http://chemaguerra.com/gaussian-filter-radius/
      sigma = real_radius;
      real_radius *= 3.0;

      // Use gaussian formula to calculate the weight of all pixels
      divider = 0.0;
      for (float i = -real_radius + 0.5; i <= real_radius; i += 2.0) {
        divider += gaussian2(i, 0.0, sigma);
      }

    }

    for (float i = -real_radius + 0.5; i <= real_radius; i += 2.0) {
      float weight;

      if (fast_in) {
        weight = divider;
      } else {
        weight = gaussian2(i, 0.0, sigma) / divider;
      }

      vec2 pixel_coord = ove_texcoord;
      vec4 tex_col;
      if (ove_iteration == 0) {
        pixel_coord.x += i / resolution_in.x;

        // Pull from main texture
        tex_col = texture(tex_in, pixel_coord);
      } else if (ove_iteration == 1) {
        pixel_coord.y += i / resolution_in.y;

        // Pull from previous iteration
        tex_col = texture(previous_iteration_in, pixel_coord);
      }

      composite += tex_col * weight;
    }

    frag_color = composite;
  }

}
"#;

impl DropShadowFilter {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns `dropshadow.frag`).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for DropShadowFilter {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Drop Shadow"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.dropshadow"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Adds a drop shadow to an image."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Texture", `color_in` -> "Color", `distance_in` -> "Distance",
	/// `angle_in` -> "Angle", `radius_in` -> "Softness", `opacity_in` ->
	/// "Opacity", `fast_in` -> "Faster (Lower Quality)".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			COLOR_INPUT => "Color",
			DISTANCE_INPUT => "Distance",
			ANGLE_INPUT => "Angle",
			// The C++ softness input id is `radius_in` (k_softness_input).
			SOFTNESS_INPUT => "Softness",
			OPACITY_INPUT => "Opacity",
			FAST_INPUT => "Faster (Lower Quality)",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// otherwise push a shader job with `resolution_in` set to the
	/// texture's virtual resolution and `previous_iteration_in` bound to
	/// the input texture; when softness is non-zero the job runs 3
	/// iterations feeding back through `previous_iteration_in`.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; `resolution_in`
	/// is filled by the runner from the input texture's size. The
	/// feedback is carried both as metadata (`iterations` = 3 and
	/// `iterative_input` = `previous_iteration_in` when softness != 0,
	/// else 1 / empty) and as an unconditional `previous_iteration_in`
	/// entry in `params`, mirroring the C++ unconditional `SetParam`
	/// (`// CPP-PARITY: dropshadowfilter.cpp` `value()`).
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
		let softness = match inputs.get(SOFTNESS_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(SOFTNESS_INPUT, -1, time).to_double(),
		};

		// C++ unconditionally binds the input texture to
		// `previous_iteration_in` (SetParam), so the params row always
		// carries the entry even when the job runs a single iteration.
		let mut params = inputs.clone();
		params.insert(ITERATIVE_INPUT.to_string(), tex);

		// C++ `if (!qIsNull(softness)) job.SetIterations(3)`: the merge
		// step (ove_iteration == 2) needs two blur passes first, so a
		// non-zero softness runs 3 iterations feeding back through
		// `previous_iteration_in`.
		let softness_nonzero = softness != 0.0;
		let iterations = if softness_nonzero { 3 } else { 1 };
		let iterative_input = if softness_nonzero {
			ITERATIVE_INPUT.to_string()
		} else {
			String::new()
		};

		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input,
			})),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns the dropshadow fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(DropShadowFilter))
	}
}

/// Constructor (C++ `DropShadowFilter::DropShadowFilter()`): adds
/// `tex_in`, `color_in`, `distance_in`, `angle_in`, `radius_in`
/// (softness), `opacity_in`, `fast_in` with the defaults and properties
/// documented on the constants, then sets the effect input and the
/// video-effect flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let color = crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([0.0, 0.0, 0.0, 1.0]),
	);
	core.add_input(color);

	core.add_input(crate::input::Input::new(
		DISTANCE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	));
	core.add_input(crate::input::Input::new(
		ANGLE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(135.0),
	));

	let mut softness = crate::input::Input::new(
		SOFTNESS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	);
	softness.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(softness);

	let mut opacity = crate::input::Input::new(
		OPACITY_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	opacity.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
	];
	core.add_input(opacity);

	core.add_input(crate::input::Input::new(
		FAST_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(DropShadowFilter))
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
		let n = DropShadowFilter;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
		assert_eq!(n.input_name(DISTANCE_INPUT), "Distance");
		assert_eq!(n.input_name(ANGLE_INPUT), "Angle");
		assert_eq!(n.input_name(SOFTNESS_INPUT), "Softness");
		assert_eq!(n.input_name(OPACITY_INPUT), "Opacity");
		assert_eq!(n.input_name(FAST_INPUT), "Faster (Lower Quality)");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.dropshadow");
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Color([0.0, 0.0, 0.0, 1.0])
		);
		assert_eq!(
			core.get_input(DISTANCE_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(
			core.get_input(ANGLE_INPUT).unwrap().default,
			NodeValue::Float(135.0)
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
	fn value_with_softness_pushes_three_iteration_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(SOFTNESS_INPUT.to_string(), NodeValue::Float(10.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => {
				let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
					.expect("shader job payload boxed");
				assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.dropshadow");
				assert_eq!(payload.shader_id, "");
				assert_eq!(payload.iterations, 3);
				assert_eq!(payload.effect_input, TEXTURE_INPUT);
				assert_eq!(payload.iterative_input, ITERATIVE_INPUT);
				assert!(payload.params.contains_key(ITERATIVE_INPUT));
			}
			_ => panic!("texture expected"),
		}
	}

	#[test]
	fn value_zero_softness_pushes_single_iteration_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(SOFTNESS_INPUT.to_string(), NodeValue::Float(0.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => {
				let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
					.expect("shader job payload boxed");
				assert_eq!(payload.iterations, 1);
				assert_eq!(payload.iterative_input, "");
				// The params row still carries the unconditional
				// `previous_iteration_in` binding (C++ SetParam).
				assert!(payload.params.contains_key(ITERATIVE_INPUT));
			}
			_ => panic!("texture expected"),
		}
	}

	#[test]
	fn shader_code_returns_dropshadow_shader() {
		let n = DropShadowFilter;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform float distance_in;"));
		assert!(code.contains("shadow_color.rgb = color_in.rgb * shadow_color.a;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Drop Shadow");
	}
}

/// Register this node type (C++ `k_drop_shadow_filter` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.dropshadow",
		name: "Drop Shadow",
		categories: &[Category::Filter],
		create,
	});
}
