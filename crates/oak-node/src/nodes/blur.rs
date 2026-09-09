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

//! Blur filter (C++ `src/node/src/filter/blur/blur.{h,cpp}`,
//! `olive::BlurFilterNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Blur method input id (C++ `k_method_input`). Type: combo; default
/// `k_gaussian` (1); flags: not-keyframable, not-connectable; combo box
/// strings: "Box", "Gaussian", "Directional", "Radial".
pub const METHOD_INPUT: &str = "method_in";

/// Blur radius input id (C++ `k_radius_input`). Type: float; default
/// `10.0`; properties: `min = 0.0`.
pub const RADIUS_INPUT: &str = "radius_in";

/// Horizontal blur toggle input id (C++ `k_horiz_input`). Type: bool;
/// default `true`; hidden unless the method is box or gaussian.
pub const HORIZ_INPUT: &str = "horiz_in";

/// Vertical blur toggle input id (C++ `k_vert_input`). Type: bool;
/// default `true`; hidden unless the method is box or gaussian.
pub const VERT_INPUT: &str = "vert_in";

/// Repeat-edge-pixels input id (C++ `k_repeat_edge_pixels_input`). Type:
/// bool; default `true`.
pub const REPEAT_EDGE_PIXELS_INPUT: &str = "repeat_edge_pixels_in";

/// Directional angle input id (C++ `k_directional_degrees_input`). Type:
/// float; default `0.0`; hidden unless the method is directional.
pub const DIRECTIONAL_DEGREES_INPUT: &str = "directional_degrees_in";

/// Radial center input id (C++ `k_radial_center_input`). Type: vec2;
/// default `(0.0, 0.0)`; hidden unless the method is radial; property
/// `offset` is set to half the texture resolution at gizmo-update time.
pub const RADIAL_CENTER_INPUT: &str = "radial_center_in";

/// Blur method enum (C++ `BlurFilterNode::Method`).
pub enum Method {
	/// Box blur.
	Box,
	/// Gaussian blur (the default).
	Gaussian,
	/// Directional blur.
	Directional,
	/// Radial blur.
	Radial,
}

/// Blur filter node. Box/gaussian/directional/radial blur, implemented
/// as one iterative shader.
///
/// C++ member `radial_center_gizmo_` is a Qt `PointGizmo` (GUI type);
/// gizmos live in `NodeCore::gizmos`, so no field is kept here.
pub struct BlurFilterNode;

/// Fragment shader (C++ `get_shader_code()` loads
/// `:/shaders/blur.frag` via FileFunctions for any request). Text
/// copied verbatim from `engine/shaders/blur.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
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

    // Unreachable in practice (the branches above are exhaustive), but
    // naga's validator rejects functions with a fallthrough path —
    // deviation from the verbatim C++ shader text.
    return MODE_NONE;
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

impl BlurFilterNode {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns `blur.frag`).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for BlurFilterNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Blur"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.blur"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Blurs an image."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `method_in` -> "Method" (combo strings "Box", "Gaussian",
	/// "Directional", "Radial"), `radius_in` -> "Radius", `horiz_in` ->
	/// "Horizontal", `vert_in` -> "Vertical", `repeat_edge_pixels_in` ->
	/// "Repeat Edge Pixels", `directional_degrees_in` -> "Direction",
	/// `radial_center_in` -> "Center".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			// The `method_in` combo strings "Box"/"Gaussian"/"Directional"/
			// "Radial" are a UI-level property (C++ `set_combo_box_strings`).
			METHOD_INPUT => "Method",
			RADIUS_INPUT => "Radius",
			HORIZ_INPUT => "Horizontal",
			VERT_INPUT => "Vertical",
			REPEAT_EDGE_PIXELS_INPUT => "Repeat Edge Pixels",
			DIRECTIONAL_DEGREES_INPUT => "Direction",
			RADIAL_CENTER_INPUT => "Center",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `method_in` -> "Box", "Gaussian",
	/// "Directional", "Radial".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			METHOD_INPUT => vec!["Box", "Gaussian", "Directional", "Radial"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// radius <= 0.0, or box/gaussian with both horiz and vert unchecked
	/// -> pass-through push of the input texture; otherwise push a shader
	/// job with `resolution_in` set to the texture's virtual resolution,
	/// running 2 iterations for box/gaussian when both horiz and vert are
	/// checked (1 otherwise).
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; `resolution_in`
	/// is filled by the runner from the input texture's size, matching the
	/// C++ `tex->virtual_resolution()` (`// CPP-PARITY: blur.cpp` `value()`).
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

		let method = match inputs.get(METHOD_INPUT) {
			Some(v) => v.to_double() as i64,
			None => core.value_at_time(METHOD_INPUT, -1, time).to_double() as i64,
		};
		let radius = match inputs.get(RADIUS_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(RADIUS_INPUT, -1, time).to_double(),
		};
		let horiz = match inputs.get(HORIZ_INPUT) {
			Some(v) => v.to_double() != 0.0,
			None => core.value_at_time(HORIZ_INPUT, -1, time).to_double() != 0.0,
		};
		let vert = match inputs.get(VERT_INPUT) {
			Some(v) => v.to_double() != 0.0,
			None => core.value_at_time(VERT_INPUT, -1, time).to_double() != 0.0,
		};

		let mut can_push_job = true;
		if radius > 0.0 {
			// Method-specific considerations.
			if method == Method::Box as i64 || method == Method::Gaussian as i64 {
				if !horiz && !vert {
					// Disable the job if both directions are unchecked.
					can_push_job = false;
				}
			}
		} else {
			can_push_job = false;
		}

		// Iterate twice for the two-pass box/gaussian blur (once per axis);
		// all other methods are single-pass (C++ `iterations = 2` only for
		// the double-pass case).
		let mut iterations = 1;
		if method == Method::Box as i64 || method == Method::Gaussian as i64 {
			if horiz && vert {
				iterations = 2;
			}
		}

		if can_push_job {
			// The shader-job box (C++ ShaderJob): the behavior's type id
			// selects the fragment source; the effect input key locates
			// the main texture inside the params row.
			table.push(
				crate::value::ValueType::Texture,
				crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
					node_id: crate::id::NodeId::INVALID,
					time,
					iterations,
					type_id: self.type_id().to_string(),
					shader_id: String::new(),
					effect_input: core.effect_input.clone(),
					params: inputs.clone(),
					iterative_input: String::new(),
				})),
				None,
			);
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns the blur fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Gizmo positions (C++ `update_gizmo_positions()`): when the method
	/// is radial and a texture is present, show the radial-center gizmo
	/// at half the texture resolution plus the center input, and set the
	/// input's `offset` property to half the resolution; otherwise hide
	/// the gizmo.
	///
	/// The placement and the `offset` property need the texture's virtual
	/// resolution (the Rust texture handle carries no params), and the
	/// gizmo visibility has no storage in [`Gizmo`] — not representable
	/// here (`// CPP-PARITY: blur.cpp` `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag (C++ `gizmo_drag_move()`): when the current gizmo is
	/// the radial-center gizmo, drag its x/y input draggers by the drag
	/// delta.
	///
	/// The draggers hold per-drag start values and write keyframe tracks,
	/// neither of which the Rust data model carries — not representable
	/// here (`// CPP-PARITY: blur.cpp` `gizmo_drag_move`).
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Input value changed (C++ `InputValueChangedEvent()`): on
	/// `method_in` changes, re-run the hidden-flag update (`horiz_in` /
	/// `vert_in` shown only for box/gaussian, `directional_degrees_in`
	/// only for directional, `radial_center_in` only for radial), then
	/// defer to the base implementation.
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		if input == METHOD_INPUT && element == -1 {
			let method = core.standard_value(METHOD_INPUT, -1).to_double() as i64;
			Self::update_inputs(core, method);
		}
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(BlurFilterNode))
	}
}

impl BlurFilterNode {
	/// Hidden-flag update for the method-specific inputs (C++
	/// `update_inputs(Method)`): `horiz_in`/`vert_in` are shown only for
	/// box/gaussian, `directional_degrees_in` only for directional, and
	/// `radial_center_in` only for radial.
	fn update_inputs(core: &mut NodeCore, method: i64) {
		set_hidden(
			core,
			HORIZ_INPUT,
			!(method == Method::Box as i64 || method == Method::Gaussian as i64),
		);
		set_hidden(
			core,
			VERT_INPUT,
			!(method == Method::Box as i64 || method == Method::Gaussian as i64),
		);
		set_hidden(
			core,
			DIRECTIONAL_DEGREES_INPUT,
			method != Method::Directional as i64,
		);
		set_hidden(core, RADIAL_CENTER_INPUT, method != Method::Radial as i64);
	}
}

/// Set or clear the hidden input flag on `id` (C++
/// `set_input_flag(id, k_input_flag_hidden, hidden)`).
fn set_hidden(core: &mut NodeCore, id: &str, hidden: bool) {
	if let Some(input) = core.get_input_mut(id) {
		if hidden {
			input.flags |= crate::input::flags::HIDDEN;
		} else {
			input.flags &= !crate::input::flags::HIDDEN;
		}
	}
}

/// Constructor (C++ `BlurFilterNode::BlurFilterNode()`): adds `tex_in`,
/// `method_in` (default gaussian), `radius_in`, `horiz_in`/`vert_in`,
/// `directional_degrees_in`, `radial_center_in` with the defaults and
/// properties documented on the constants, hides the method-specific
/// inputs for the default method, adds `repeat_edge_pixels_in`, sets
/// the video-effect flag and the effect input, and adds a draggable
/// anchor-point gizmo bound to both tracks of `radial_center_in`.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut method = crate::input::Input::new(
		METHOD_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(Method::Gaussian as i64),
	);
	method.flags |= crate::input::flags::NOT_KEYFRAMABLE | crate::input::flags::NOT_CONNECTABLE;
	core.add_input(method);

	let mut radius = crate::input::Input::new(
		RADIUS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	);
	radius.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(radius);

	// Box and gaussian only.
	core.add_input(crate::input::Input::new(
		HORIZ_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	));
	core.add_input(crate::input::Input::new(
		VERT_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	));

	// Directional only.
	core.add_input(crate::input::Input::new(
		DIRECTIONAL_DEGREES_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));

	// Radial only.
	core.add_input(crate::input::Input::new(
		RADIAL_CENTER_INPUT,
		crate::value::ValueType::Vec2,
		crate::value::NodeValue::Vec2([0.0, 0.0]),
	));

	// Hide the method-specific inputs for the default (gaussian) method.
	BlurFilterNode::update_inputs(&mut core, Method::Gaussian as i64);

	core.add_input(crate::input::Input::new(
		REPEAT_EDGE_PIXELS_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(true),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	// Anchor-shaped radial-center point gizmo dragging both tracks.
	let gizmo = Gizmo {
		position_inputs: vec![
			(RADIAL_CENTER_INPUT.to_string(), -1, 0),
			(RADIAL_CENTER_INPUT.to_string(), -1, 1),
		],
		drag_point: (0.0, 0.0),
	};
	core.gizmos = vec![gizmo];

	(core, Box::new(BlurFilterNode))
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

	fn is_hidden(core: &NodeCore, id: &str) -> bool {
		core.get_input(id).unwrap().flags & crate::input::flags::HIDDEN != 0
	}

	#[test]
	fn input_names() {
		let n = BlurFilterNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(METHOD_INPUT), "Method");
		assert_eq!(n.input_name(RADIUS_INPUT), "Radius");
		assert_eq!(n.input_name(HORIZ_INPUT), "Horizontal");
		assert_eq!(n.input_name(VERT_INPUT), "Vertical");
		assert_eq!(n.input_name(REPEAT_EDGE_PIXELS_INPUT), "Repeat Edge Pixels");
		assert_eq!(n.input_name(DIRECTIONAL_DEGREES_INPUT), "Direction");
		assert_eq!(n.input_name(RADIAL_CENTER_INPUT), "Center");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.blur");
		assert_eq!(
			core.get_input(METHOD_INPUT).unwrap().default,
			NodeValue::Combo(1)
		);
		assert_eq!(
			core.get_input(RADIUS_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(
			core.get_input(REPEAT_EDGE_PIXELS_INPUT).unwrap().default,
			NodeValue::Boolean(true)
		);
		// Default method (gaussian): directional/radial inputs hidden.
		assert!(!is_hidden(&core, HORIZ_INPUT));
		assert!(!is_hidden(&core, VERT_INPUT));
		assert!(is_hidden(&core, DIRECTIONAL_DEGREES_INPUT));
		assert!(is_hidden(&core, RADIAL_CENTER_INPUT));
		// One radial-center gizmo bound to both tracks.
		assert_eq!(core.gizmos.len(), 1);
		assert_eq!(core.gizmos[0].position_inputs.len(), 2);
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
	fn value_zero_radius_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_box_no_directions_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(0));
		core.set_standard_value(HORIZ_INPUT, -1, NodeValue::Boolean(false));
		core.set_standard_value(VERT_INPUT, -1, NodeValue::Boolean(false));
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(10.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_gaussian_pushes_shader_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(RADIUS_INPUT.to_string(), NodeValue::Float(10.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("texture expected");
		};
		let payload = unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(h) }
			.expect("shader job pushed");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.blur");
		assert_eq!(
			payload.params.get(RADIUS_INPUT),
			Some(&NodeValue::Float(10.0)),
			"the blur radius rides in the job params"
		);
	}

	#[test]
	fn input_value_changed_toggles_method_inputs() {
		let (mut core, behavior) = create();
		let mut b = behavior;

		// Directional: horiz/vert hidden, directional shown.
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(2));
		b.input_value_changed(&mut core, METHOD_INPUT, -1);
		assert!(is_hidden(&core, HORIZ_INPUT));
		assert!(is_hidden(&core, VERT_INPUT));
		assert!(!is_hidden(&core, DIRECTIONAL_DEGREES_INPUT));
		assert!(is_hidden(&core, RADIAL_CENTER_INPUT));

		// Radial: radial shown.
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(3));
		b.input_value_changed(&mut core, METHOD_INPUT, -1);
		assert!(!is_hidden(&core, RADIAL_CENTER_INPUT));

		// Back to box: horiz/vert shown again.
		core.set_standard_value(METHOD_INPUT, -1, NodeValue::Combo(0));
		b.input_value_changed(&mut core, METHOD_INPUT, -1);
		assert!(!is_hidden(&core, HORIZ_INPUT));
		assert!(!is_hidden(&core, VERT_INPUT));
		assert!(is_hidden(&core, DIRECTIONAL_DEGREES_INPUT));
		assert!(is_hidden(&core, RADIAL_CENTER_INPUT));
	}

	#[test]
	fn shader_code_returns_blur_shader() {
		let n = BlurFilterNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform int method_in;"));
		assert!(code.contains("METHOD_GAUSSIAN_BLUR 1"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Blur");
	}
}

/// Register this node type (C++ `k_blur_filter` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.blur",
		name: "Blur",
		categories: &[Category::Filter],
		create,
	});
}
