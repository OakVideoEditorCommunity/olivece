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

//! Mask distort effect (C++ `src/node/src/distort/mask/mask.{h,cpp}`,
//! `olive::MaskDistortNode`). In C++ this derives from
//! `PolygonGenerator` (which derives from `GeneratorWithMerge`), so the
//! polygon point editing, the `base_in` merge input and the
//! `points_in`/`color_in` inputs are inherited; the base's Rust home is
//! `crate::nodes::polygon`.

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Invert input id (C++ `k_invert_input`). Type: bool; default `false`.
pub const INVERT_INPUT: &str = "invert_in";

/// Feather input id (C++ `k_feather_input`). Type: float; default
/// `0.0`; properties: `min = 0.0`.
pub const FEATHER_INPUT: &str = "feather_in";

/// Mask distort node. Renders the inherited polygon as a white mask and
/// multiplies it over the base texture, with optional invert and
/// feather (gaussian blur of the matte). Has no own member fields in
/// C++ beyond the inherited `PolygonGenerator` state.
pub struct MaskDistortNode {
	/// Inherited polygon-generator state (C++ base class
	/// `PolygonGenerator`; provides `points_in`, `color_in`, the base
	/// merge input `base_in` and the polygon shape shader).
	pub polygon: crate::nodes::polygon::PolygonGenerator,
}

/// Merge fragment shader for the `"mrg"` shader id (C++ loads
/// `:/shaders/multiply.frag`). Text copied verbatim from
/// `engine/shaders/multiply.frag`.
const SHADER_MRG_FRAG: &str = r#"// Input texture
uniform sampler2D tex_a;
uniform sampler2D tex_b;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    frag_color = texture(tex_a, ove_texcoord) * texture(tex_b, ove_texcoord);
}
"#;

/// Invert fragment shader for the `"invert"` shader id (C++ loads
/// `:/shaders/invertrgba.frag`). Text copied verbatim from
/// `engine/shaders/invertrgba.frag`.
const SHADER_INVERT_FRAG: &str = r#"// Input texture
uniform sampler2D tex_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    vec4 color = texture(tex_in, ove_texcoord);
    color = 1.0 - color;
    frag_color = color;
}
"#;

/// Feather (gaussian blur) fragment shader for the `"feather"` shader
/// id (C++ loads `:/shaders/blur.frag`). Text copied verbatim from
/// `engine/shaders/blur.frag`.
const SHADER_FEATHER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform int method_in;
uniform float radius_in;
uniform bool horiz_in;
uniform bool vert_in;
uniform bool repeat_edge_pixels_in;
uniform vec2 resolution_in;

// Directional
uniform float directional_degrees_in;

// Radial
uniform vec2 radial_center_in;

uniform int ove_iteration;

in vec2 ove_texcoord;
out vec4 frag_color;

// Gaussian function uses PI
#define M_PI 3.1415926535897932384626433832795

// Methods
#define METHOD_BOX_BLUR 0
#define METHOD_GAUSSIAN_BLUR 1
#define METHOD_DIRECTIONAL_BLUR 2
#define METHOD_RADIAL_BLUR 3

// Mode
#define MODE_NONE 0
#define MODE_HORIZONTAL 1
#define MODE_VERTICAL 2

// Single gaussian formula (unused, mainly here for documentation/just in case)
//float gaussian(float x, float sigma) {
//    return (1.0/(sigma*sqrt(2.0*M_PI)))*exp(-0.5*pow(x/sigma, 2.0));
//}

// Double gaussian formula, actually used in the code below
// Should be faster than the single gaussian above since it doesn't need sqrt()
float gaussian2(float x, float y, float sigma) {
    return (1.0/((sigma*sigma)*2.0*M_PI))*exp(-0.5*(((x*x) + (y*y))/(sigma*sigma)));
}

int determine_mode() {
    if (radius_in == 0.0) {
        return MODE_NONE;
    }

    if (!horiz_in && !vert_in) {
        return MODE_NONE;
    }

    if (horiz_in && !vert_in) {
        return MODE_HORIZONTAL;
    }

    if (vert_in && !horiz_in) {
        return MODE_VERTICAL;
    }

    if (ove_iteration == 0) {
        return MODE_HORIZONTAL;
    }

    if (ove_iteration == 1) {
        return MODE_VERTICAL;
    }
}

vec4 add_to_composite(vec4 composite, vec2 pixel_coord, float weight)
{
  if (repeat_edge_pixels_in
      || (pixel_coord.x >= 0.0
          && pixel_coord.x < 1.0
          && pixel_coord.y >= 0.0
          && pixel_coord.y < 1.0)) {
      composite += texture(tex_in, pixel_coord) * weight;
  }

  return composite;
}

void main(void) {
    int mode = determine_mode();

    if (mode == MODE_NONE) {
        frag_color = texture(tex_in, ove_texcoord);
        return;
    }

    // We only sample on hard pixels, so we don't accept decimal radii
    float real_radius = ceil(radius_in);

    vec4 composite = vec4(0.0);

    float divider, sigma;

    if (method_in == METHOD_DIRECTIONAL_BLUR || method_in == METHOD_RADIAL_BLUR) {
      // Despite similar math, these are lighter methods perceptually, so we double the radius to
      // better match box/gaussian
      real_radius *= 2.0;
    }

    if (method_in == METHOD_BOX_BLUR || method_in == METHOD_DIRECTIONAL_BLUR) {

        // Calculate the weight of each pixel based on the radius
        divider = 1.0 / real_radius;

    } else if (method_in == METHOD_GAUSSIAN_BLUR) {

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

    if (method_in == METHOD_BOX_BLUR || method_in == METHOD_GAUSSIAN_BLUR) {
        for (float i = -real_radius + 0.5; i <= real_radius; i += 2.0) {
            float weight;

            if (method_in == METHOD_BOX_BLUR) {
                weight = divider;
            } else if (method_in == METHOD_GAUSSIAN_BLUR) {
                weight = gaussian2(i, 0.0, sigma) / divider;
            }

            vec2 pixel_coord = ove_texcoord;
            if (mode == MODE_HORIZONTAL) {
                pixel_coord.x += i / resolution_in.x;
            } else if (mode == MODE_VERTICAL) {
                pixel_coord.y += i / resolution_in.y;
            }

            composite = add_to_composite(composite, pixel_coord, weight);
        }
    } else if (method_in == METHOD_DIRECTIONAL_BLUR || method_in == METHOD_RADIAL_BLUR) {
        float angle;

        if (method_in == METHOD_DIRECTIONAL_BLUR) {
          // Convert directional degrees to radians
          angle = (directional_degrees_in*M_PI)/180.0;
        } else {
          // Calculate angle from distance of center to current coordinate
          vec2 distance = (ove_texcoord - 0.5) * (resolution_in) - radial_center_in;
          angle = atan(distance.y/distance.x);

          float multiplier = length(distance) / resolution_in.y * 2.0;

          real_radius = ceil(radius_in * multiplier);
          divider = 1.0 / real_radius;
        }

        // Get angles
        float sin_angle = sin(angle);
        float cos_angle = cos(angle);

        for (float i = -real_radius + 0.5; i <= real_radius; i += 2.0) {
          vec2 pixel_coord = ove_texcoord;

          pixel_coord.y += sin_angle * i / resolution_in.y;
          pixel_coord.x += cos_angle * i / resolution_in.x;

          composite = add_to_composite(composite, pixel_coord, divider);
        }
    }

    frag_color = composite;
}
"#;

impl MaskDistortNode {
	/// Merge fragment shader (C++ `get_shader_code()` `"mrg"` branch).
	fn shader_mrg_frag() -> &'static str {
		SHADER_MRG_FRAG
	}

	/// Invert fragment shader (C++ `get_shader_code()` `"invert"`
	/// branch).
	fn shader_invert_frag() -> &'static str {
		SHADER_INVERT_FRAG
	}

	/// Feather blur fragment shader (C++ `get_shader_code()`
	/// `"feather"` branch).
	fn shader_feather_frag() -> &'static str {
		SHADER_FEATHER_FRAG
	}
}

/// Combined mask fragment shader for the `"mask"` shader id, replacing
/// the C++ chain — matte rasterize -> optional invert -> optional 2-pass
/// feather blur -> multiply over base — with a single GPU pass: the base
/// texture is multiplied by the polygon matte (odd-even fill of the
/// closed point loop, same transform as the polygon generator), the
/// matte is optionally inverted and optionally softened with a 2D
/// gaussian (one pass; the C++ `blur.frag` horizontal+vertical
/// iterations, evaluated as the separable product — sigma = radius/2,
/// matching the C++ gaussian2 call). Feather radius is capped at 16 px
/// for pass cost; the C++ cap is the full blur shader.
const SHADER_MASK_FRAG: &str = r#"// Input texture
uniform sampler2D base_in;
uniform int point_count;
uniform vec4 points_in[64];
uniform float feather_in;
uniform bool invert_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec2 pixel = ove_texcoord * resolution_in;
    vec4 base = texture(base_in, ove_texcoord);
    float matte = 0.0;
    float cx = resolution_in.x * 0.5;
    float cy = resolution_in.y * 0.5;

    if (point_count >= 3) {
        int crossings = 0;
        for (int i = 0; i < point_count; i++) {
            vec2 a = vec2(points_in[i].x + cx, cy - points_in[i].y);
            vec2 b = vec2(points_in[(i + 1) % point_count].x + cx, cy - points_in[(i + 1) % point_count].y);
            if ((a.y <= pixel.y && b.y > pixel.y) || (b.y <= pixel.y && a.y > pixel.y)) {
                float x_cross = a.x + (pixel.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if (x_cross > pixel.x) {
                    crossings++;
                }
            }
        }
        matte = (crossings % 2 == 1) ? 1.0 : 0.0;
    }

    if (feather_in > 0.0) {
        float r = min(feather_in, 16.0);
        float sigma = r * 0.5;
        float wsum = 0.0;
        float acc = 0.0;
        for (int y = -int(ceil(r)); y <= int(ceil(r)); y++) {
            for (int x = -int(ceil(r)); x <= int(ceil(r)); x++) {
                vec2 p = clamp(ove_texcoord + vec2(float(x), float(y)) / resolution_in, 0.0, 1.0);
                vec2 pp = p * resolution_in;
                int cs = 0;
                for (int i = 0; i < point_count; i++) {
                    vec2 a = vec2(points_in[i].x + cx, cy - points_in[i].y);
                    vec2 b = vec2(points_in[(i + 1) % point_count].x + cx, cy - points_in[(i + 1) % point_count].y);
                    if ((a.y <= pp.y && b.y > pp.y) || (b.y <= pp.y && a.y > pp.y)) {
                        float x_cross = a.x + (pp.y - a.y) / (b.y - a.y) * (b.x - a.x);
                        if (x_cross > pp.x) {
                            cs++;
                        }
                    }
                }
                float inside = (cs % 2 == 1) ? 1.0 : 0.0;
                float w = exp(-0.5 * ((float(x) * float(x)) + (float(y) * float(y))) / (sigma * sigma));
                acc += w * inside;
                wsum += w;
            }
        }
        matte = acc / max(wsum, 1e-6);
    }

    if (invert_in) {
        matte = 1.0 - matte;
    }
    frag_color = vec4(base.rgb * matte, base.a * matte);
}
"#;

impl NodeBehavior for MaskDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Mask"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.mask"
	}

	/// Categories (C++ `category()`; note: C++ files Mask under
	/// distort even though it derives from a generator base).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Apply a polygonal mask."
	}

	/// Localized input names (C++ `retranslate()`): the inherited
	/// `base_in` -> "Texture", `invert_in` -> "Invert", `feather_in` ->
	/// "Feather" (plus the `PolygonGenerator` names via its own
	/// retranslate).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			crate::nodes::generatorwithmerge::BASE_INPUT => "Texture",
			INVERT_INPUT => "Invert",
			FEATHER_INPUT => "Feather",
			crate::nodes::polygon::POINTS_INPUT => "Points",
			crate::nodes::polygon::COLOR_INPUT => "Color",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): pushes a REAL single
	/// [`ShaderJobPayload`] whose `"mask"` fragment shader does the whole
	/// C++ chain on the GPU — yes. One pass multiplies the base texture
	/// by the polygon matte (odd-even fill), optionally invert, and the
	/// optional feather gaussian softens the matte during sampling. The
	/// payload's params carry `points_in` (the inherited array, collected
	/// as in the polygon generator), `point_count`, `invert_in` and
	/// `feather_in`, with the base texture under the effect input.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let points = crate::nodes::polygon::point_array(core, inputs, time);
		let point_count = points.len();
		let mut params = inputs.clone();
		params.insert(
			crate::nodes::polygon::POINTS_INPUT.to_string(),
			crate::value::NodeValue::Vec4Array(points),
		);
		params.insert(
			"point_count".to_string(),
			crate::value::NodeValue::Int(point_count as i64),
		);
		// The row carries invert/feather in the traverser flow; fall back
		// to the node's own values for direct `value()` calls.
		for id in [INVERT_INPUT, FEATHER_INPUT] {
			if !params.contains_key(id) {
				params.insert(id.to_string(), core.value_at_time(id, -1, time));
			}
		}
		let job = crate::handle::make_owned(crate::jobs::ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations: 1,
			type_id: self.type_id().to_string(),
			shader_id: "mask".to_string(),
			effect_input: crate::nodes::generatorwithmerge::BASE_INPUT.to_string(),
			params,
			iterative_input: String::new(),
		});
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(job),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): dispatches on the
	/// request id — `"mrg"` -> multiply merge shader, `"feather"` ->
	/// blur shader, `"invert"` -> invert RGBA shader, anything else ->
	/// the inherited `PolygonGenerator` shader.
	fn shader_code(&self, request: &str) -> Option<String> {
		match request {
			"mrg" => Some(SHADER_MRG_FRAG.to_string()),
			"feather" => Some(SHADER_FEATHER_FRAG.to_string()),
			"invert" => Some(SHADER_INVERT_FRAG.to_string()),
			"mask" => Some(SHADER_MASK_FRAG.to_string()),
			_ => self.polygon.shader_code(request),
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MaskDistortNode {
			polygon: crate::nodes::polygon::PolygonGenerator,
		}))
	}
}

/// Constructor (C++ `MaskDistortNode::MaskDistortNode()`): hides the
/// inherited `color_in` input (the mask is always white so the multiply
/// works), then adds `invert_in` and `feather_in` with the defaults and
/// properties documented on the constants. Note: unlike the other
/// distort nodes, the C++ constructor does NOT set the video-effect
/// flag or an effect input here — that state comes from the
/// `PolygonGenerator`/`GeneratorWithMerge` base.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	// Inherited from the `GeneratorWithMerge` / `PolygonGenerator` base
	// chain, mirrored here because the base constructors are not callable
	// in the Rust model (`// CPP-PARITY: generatorwithmerge.cpp` /
	// `polygon.cpp` constructors). `base_in` is the effect input and sets
	// the video-effect flag.
	let mut base = crate::input::Input::new(
		crate::nodes::generatorwithmerge::BASE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);
	core.effect_input = crate::nodes::generatorwithmerge::BASE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	// `points_in` is a bezier array in C++; the Rust value model has no
	// bezier type, so it is declared as a Vec2 array with the default
	// pentagon positions (the bezier control handles are not
	// representable). The array element count matches the C++ default of
	// 5 (`// CPP-PARITY: polygon.cpp` constructor).
	let mut points = crate::input::Input::new(
		crate::nodes::polygon::POINTS_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	);
	points.flags |= crate::input::flags::ARRAY;
	points.array_size = 5;
	core.add_input(points);
	for (i, (x, y)) in [
		(0.0, -135.0),
		(135.0, -45.0),
		(90.0, 120.0),
		(-90.0, 120.0),
		(-135.0, -45.0),
	]
	.iter()
	.enumerate()
	{
		core.set_standard_value(
			crate::nodes::polygon::POINTS_INPUT,
			i as i32,
			crate::value::NodeValue::Vec2([*x, *y]),
		);
	}

	// Mask should always be (1.0, 1.0, 1.0) for multiply to work correctly
	let mut color = crate::input::Input::new(
		crate::nodes::polygon::COLOR_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	);
	color.flags |= crate::input::flags::HIDDEN;
	core.add_input(color);

	core.add_input(crate::input::Input::new(
		INVERT_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	let mut feather = crate::input::Input::new(
		FEATHER_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	feather.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(feather);

	(
		core,
		Box::new(MaskDistortNode {
			polygon: crate::nodes::polygon::PolygonGenerator,
		}),
	)
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.mask`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.mask",
		name: "Mask",
		categories: &[Category::Distort],
		create,
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = MaskDistortNode {
			polygon: crate::nodes::polygon::PolygonGenerator,
		};
		assert_eq!(
			n.input_name(crate::nodes::generatorwithmerge::BASE_INPUT),
			"Texture"
		);
		assert_eq!(n.input_name(INVERT_INPUT), "Invert");
		assert_eq!(n.input_name(FEATHER_INPUT), "Feather");
		assert_eq!(n.input_name(crate::nodes::polygon::POINTS_INPUT), "Points");
		assert_eq!(n.input_name(crate::nodes::polygon::COLOR_INPUT), "Color");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.mask");
		// Inherited base wiring.
		assert_ne!(
			core.get_input(crate::nodes::generatorwithmerge::BASE_INPUT)
				.unwrap()
				.flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		assert_eq!(
			core.effect_input,
			crate::nodes::generatorwithmerge::BASE_INPUT
		);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		// The inherited color input is hidden (mask is always white).
		let color = core.get_input(crate::nodes::polygon::COLOR_INPUT).unwrap();
		assert_ne!(color.flags & crate::input::flags::HIDDEN, 0);
		assert_eq!(color.default, NodeValue::Color([1.0, 1.0, 1.0, 1.0]));
		// points_in: 5-element array with the default pentagon positions.
		let points = core.get_input(crate::nodes::polygon::POINTS_INPUT).unwrap();
		assert_ne!(points.flags & crate::input::flags::ARRAY, 0);
		assert_eq!(points.array_size, 5);
		assert_eq!(
			core.standard_value(crate::nodes::polygon::POINTS_INPUT, 0),
			NodeValue::Vec2([0.0, -135.0])
		);
		assert_eq!(
			core.standard_value(crate::nodes::polygon::POINTS_INPUT, 4),
			NodeValue::Vec2([-135.0, -45.0])
		);
		// Mask-specific inputs.
		assert_eq!(
			core.get_input(INVERT_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		let feather = core.get_input(FEATHER_INPUT).unwrap();
		assert_eq!(feather.default, NodeValue::Float(0.0));
		assert!(feather
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
	}

	#[test]
	fn value_pushes_real_mask_job() {
		let (core, behavior) = create();
		// No inputs at all: the matte job pushes with defaults.
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		let tex = table.get(ValueType::Texture).expect("texture pushed");
		let NodeValue::Texture(handle) = tex else {
			panic!("pushed value is not a texture handle");
		};
		let job = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle)
		}
		.expect("real shader job");
		assert_eq!(job.shader_id, "mask");
		assert_eq!(job.type_id, "org.olivevideoeditor.Olive.mask");
		assert_eq!(
			job.effect_input,
			crate::nodes::generatorwithmerge::BASE_INPUT
		);
		match job.params.get(crate::nodes::polygon::POINTS_INPUT) {
			Some(NodeValue::Vec4Array(points)) => {
				assert_eq!(points.len(), 5, "default pentagon");
				assert_eq!(points[0], [0.0, -135.0, 0.0, 0.0]);
			}
			other => panic!("points_in is not a Vec4Array: {other:?}"),
		}
		assert_eq!(
			job.params.get("point_count"),
			Some(&NodeValue::Int(5))
		);
		assert_eq!(
			job.params.get(INVERT_INPUT),
			Some(&NodeValue::Boolean(false))
		);
		assert_eq!(job.params.get(FEATHER_INPUT), Some(&NodeValue::Float(0.0)));
	}

	#[test]
	fn shader_code_dispatches_on_request() {
		let n = MaskDistortNode {
			polygon: crate::nodes::polygon::PolygonGenerator,
		};
		let mrg = n.shader_code("mrg").unwrap();
		assert!(mrg.contains("texture(tex_a, ove_texcoord) * texture(tex_b, ove_texcoord)"));
		let feather = n.shader_code("feather").unwrap();
		assert!(feather.contains("gaussian2"));
		let invert = n.shader_code("invert").unwrap();
		assert!(invert.contains("color = 1.0 - color;"));
		let mask = n.shader_code("mask").unwrap();
		assert!(mask.contains("points_in[64]"));
		assert!(mask.contains("feather_in"));
		assert!(mask.contains("invert_in"));
		assert!(mask.contains("base.rgb * matte"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Mask");
	}
}
