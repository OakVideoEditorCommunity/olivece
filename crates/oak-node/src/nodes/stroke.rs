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

//! Stroke filter (C++ `src/node/src/filter/stroke/stroke.{h,cpp}`,
//! `olive::StrokeFilterNode`).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Stroke color input id (C++ `k_color_input`). Type: color; default
/// opaque white `(1.0, 1.0, 1.0, 1.0)`.
pub const COLOR_INPUT: &str = "color_in";

/// Stroke radius input id (C++ `k_radius_input`). Type: float; default
/// `10.0`; properties: `min = 0.0`.
pub const RADIUS_INPUT: &str = "radius_in";

/// Stroke opacity input id (C++ `k_opacity_input`). Type: float;
/// default `1.0`; properties: `view = percentage`, `min = 0.0`,
/// `max = 1.0`.
pub const OPACITY_INPUT: &str = "opacity_in";

/// Inner-stroke toggle input id (C++ `k_inner_input`). Type: bool;
/// default `false`.
pub const INNER_INPUT: &str = "inner_in";

/// Stroke filter node. Draws a colored outline around the opaque
/// regions of the input. The C++ class declares no own member fields.
pub struct StrokeFilterNode;

/// Fragment shader (C++ `get_shader_code()` loads
/// `:/shaders/stroke.frag` via FileFunctions for any request). Text
/// copied verbatim from `engine/shaders/stroke.frag`.
const SHADER_FRAG: &str = r#"// Node parameter inputs
uniform sampler2D tex_in;
uniform vec4 color_in;
uniform float radius_in;
uniform float opacity_in;
uniform bool inner_in;
uniform vec2 resolution_in;

// Standard inputs
uniform int ove_iteration;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec4 pixel_here = texture(tex_in, ove_texcoord);

    // Detect no-op situations
    if (radius_in == 0.0
        || opacity_in == 0.0
        || (inner_in && pixel_here.a == 0.0)
        || (!inner_in && pixel_here.a == 1.0)) {
        // No-op, do nothing
        frag_color = pixel_here;
        return;
    }

    float radius = ceil(radius_in);

    float stroke_weight = 0.0;

    // Loop over box
    for (float i=-radius + 0.5; i<=radius; i += 2.0) {
        float x_coord = i / resolution_in.x;

        for (float j=-radius + 0.5; j<=radius; j += 2.0) {
            float y_coord = j / resolution_in.y;

            if (abs(length(vec2(i, j))) < radius) {
                // Get pixel here
                float alpha = texture(tex_in, ove_texcoord + vec2(x_coord, y_coord)).a;

                if (inner_in) {
                    alpha = 1.0 - alpha;
                }

                stroke_weight += alpha;

                if (stroke_weight >= 1.0) {
                    break;
                }
            }
        }

        if (stroke_weight >= 1.0) {
            stroke_weight = 1.0;
            break;
        }
    }

    stroke_weight *= opacity_in;

    if (inner_in) {
        stroke_weight *= pixel_here.a;
    }

    // Make RGBA color
    vec4 stroke_col = color_in * stroke_weight;

    if (inner_in) {
        // Alpha over the stroke over the texture
        stroke_col = pixel_here * (1.0 - stroke_col.a) + stroke_col;
    } else {
        // Alpha over the texture over the stroke
        stroke_col = stroke_col * (1.0 - pixel_here.a) + pixel_here;
    }

    frag_color = stroke_col;
}
"#;

impl StrokeFilterNode {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns `stroke.frag`).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for StrokeFilterNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Stroke"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.stroke"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Creates a stroke outline around an image."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `color_in` -> "Color", `radius_in` -> "Radius", `opacity_in` ->
	/// "Opacity", `inner_in` -> "Inner".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			COLOR_INPUT => "Color",
			RADIUS_INPUT => "Radius",
			OPACITY_INPUT => "Opacity",
			INNER_INPUT => "Inner",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// radius <= 0.0 or opacity <= 0.0 -> pass-through push of the input
	/// texture; otherwise push a shader job with `resolution_in` set to
	/// the texture's virtual resolution.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; `resolution_in`
	/// is filled by the runner from the frame size, matching the C++
	/// `tex->virtual_resolution()` (`// CPP-PARITY: stroke.cpp` `value()`).
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

		let radius = match inputs.get(RADIUS_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(RADIUS_INPUT, -1, time).to_double(),
		};
		let opacity = match inputs.get(OPACITY_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(OPACITY_INPUT, -1, time).to_double(),
		};

		if radius > 0.0 && opacity > 0.0 {
			// The shader-job box (C++ ShaderJob): the behavior's type id
			// selects the fragment source; the effect input key locates the
			// main texture inside the params row.
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
		} else {
			table.push(crate::value::ValueType::Texture, tex, None);
		}
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns the stroke fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(StrokeFilterNode))
	}
}

/// Constructor (C++ `StrokeFilterNode::StrokeFilterNode()`): adds
/// `tex_in`, `color_in`, `radius_in`, `opacity_in`, `inner_in` with the
/// defaults and properties documented on the constants, then sets the
/// video-effect flag and the effect input.
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
		crate::value::NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	);
	core.add_input(color);

	let mut radius = crate::input::Input::new(
		RADIUS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(10.0),
	);
	radius.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(radius);

	let mut opacity = crate::input::Input::new(
		OPACITY_INPUT,
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

	core.add_input(crate::input::Input::new(
		INNER_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(StrokeFilterNode))
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
		let n = StrokeFilterNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
		assert_eq!(n.input_name(RADIUS_INPUT), "Radius");
		assert_eq!(n.input_name(OPACITY_INPUT), "Opacity");
		assert_eq!(n.input_name(INNER_INPUT), "Inner");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.stroke");
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Color([1.0, 1.0, 1.0, 1.0])
		);
		assert_eq!(
			core.get_input(RADIUS_INPUT).unwrap().default,
			NodeValue::Float(10.0)
		);
		assert_eq!(
			core.get_input(OPACITY_INPUT).unwrap().default,
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
	fn value_zero_radius_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(0.0));
		core.set_standard_value(OPACITY_INPUT, -1, NodeValue::Float(1.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_zero_opacity_passes_texture_through() {
		let (mut core, behavior) = create();
		core.set_standard_value(RADIUS_INPUT, -1, NodeValue::Float(10.0));
		core.set_standard_value(OPACITY_INPUT, -1, NodeValue::Float(0.0));
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex.clone())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_positive_radius_and_opacity_pushes_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(RADIUS_INPUT.to_string(), NodeValue::Float(5.0)),
			(OPACITY_INPUT.to_string(), NodeValue::Float(0.5)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => {
				let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
					.expect("shader job payload boxed");
				assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.stroke");
				assert_eq!(payload.shader_id, "");
				assert_eq!(payload.iterations, 1);
				assert_eq!(payload.effect_input, TEXTURE_INPUT);
				assert_eq!(payload.iterative_input, "");
				assert!(payload.params.contains_key(RADIUS_INPUT));
			}
			_ => panic!("texture expected"),
		}
	}

	#[test]
	fn shader_code_returns_stroke_shader() {
		let n = StrokeFilterNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform float radius_in;"));
		assert!(code.contains("stroke_weight *= opacity_in;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Stroke");
	}
}

/// Register this node type (C++ `k_stroke_filter` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.stroke",
		name: "Stroke",
		categories: &[Category::Filter],
		create,
	});
}
