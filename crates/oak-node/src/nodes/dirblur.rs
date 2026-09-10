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

//! Directional blur filter (clean-room reimplementation of the
//! OpenFX-Misc `DirBlurOFX` / `net.sf.openfx.DirBlur` effect; `ofx-misc`
//! used for parameter semantics only, no code copied).
//!
//! Upstream blurs by concatenating a per-frame transform, i.e. by
//! resampling the image along the motion direction with an
//! `amount`-pixel smear and a `centered`/`fading` shaping of the tap
//! weights. The same picture is produced here by one fragment pass that
//! averages a fixed number of taps along the direction vector, centered
//! on the pixel (a box-shaped, non-fading smear — the `centered = true`,
//! `fading = 0` case).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Blur radius input id (upstream `dirBlurAmount`, "Amount"). Type:
/// float; default `0.0`; properties: `min = 0.0`. The half-length of
/// the smear in sequence pixels: each tap lies within `amount_in`
/// pixels of the pixel along the direction vector.
pub const AMOUNT_INPUT: &str = "amount_in";

/// Blur direction input id (upstream `dirBlurAngle`/"Angle" on top of
/// the transform concatenation). Type: float; default `0.0`; degrees
/// counter-clockwise from the +x axis in the un-flipped image frame (as
/// in [`crate::nodes::blur`]'s `directional_degrees_in`, y grows down the
/// frame, so a growing angle rotates the smear clockwise on screen).
pub const ANGLE_INPUT: &str = "angle_in";

/// Directional blur node. Averages 16 taps evenly spaced along the
/// direction vector, centered on the pixel.
pub struct DirBlurNode;

/// Number of taps averaged per pixel. Fixed (upstream has no tap-count
/// knob either): a power-of-two constant keeps the division exact and
/// the shader otherwise branch-free.
const TAP_COUNT: i32 = 16;

/// Fragment shader: one direction vector from `angle_in`, then a
/// constant-count loop averaging the taps. The tap spacing collapses to
/// zero when `amount_in` is zero, so a zero amount is an exact
/// identity without a branch. Offsets are pixels (`resolution_in`,
/// auto-filled by the renderer from the effect input's size, or from
/// the sequence square resolution on the real render path — the amount
/// is a sequence-pixel value, matching the blur node's radius); the
/// sampler clamps to the border, so taps past the frame edge repeat the
/// edge pixel. `switch` is deliberately not used (naga rejects it).
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float amount_in;
uniform float angle_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

// Taps averaged per pixel (keep in sync with TAP_COUNT).
#define TAP_COUNT 16

// M_PI mirrors the blur node's directional-blur shader (degrees ->
// radians without relying on built-in constants).
#define M_PI 3.1415926535897932384626433832795

void main() {
  float angle = (angle_in * M_PI) / 180.0;
  vec2 direction = vec2(cos(angle), sin(angle));

  // Distance between neighboring taps: TAP_COUNT taps evenly spread
  // over the 2 * amount_in pixel-long segment centered on the pixel.
  vec2 step = direction * (2.0 * amount_in) / (resolution_in * float(TAP_COUNT - 1));

  vec4 sum = vec4(0.0);
  for (int i = 0; i < TAP_COUNT; ++i) {
    // Tap offset in [-amount_in, +amount_in] pixels, centered on 0.
    float t = float(i) - float(TAP_COUNT - 1) * 0.5;
    sum += texture(tex_in, ove_texcoord + step * t);
  }

  frag_color = sum / float(TAP_COUNT);
}
"#;

impl DirBlurNode {
	/// Fragment shader for any request (this node has a single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for DirBlurNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Directional Blur"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.dirblur"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description.
	fn description(&self) -> &str {
		"Blurs an image along a direction."
	}

	/// Localized input names: `tex_in` -> "Input", `amount_in` ->
	/// "Amount", `angle_in` -> "Angle".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			AMOUNT_INPUT => "Amount",
			ANGLE_INPUT => "Angle",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; otherwise push the
	/// directional-blur shader job.
	///
	/// A zero `amount_in` still pushes the job (the shader's tap spacing
	/// then collapses to zero and the pass is an exact identity) rather
	/// than passing the input texture through as the blur node does for
	/// a zero radius: the plan's node template reserves the pass-through
	/// for the no-texture case, and feeding the input's CPU texture
	/// downstream here would silently break nodes that expect a GPU
	/// texture. The resolved amount and angle are written into the job
	/// row so the uniforms are always present, even when the incoming
	/// row only carries the effect input.
	///
	/// `resolution_in` is filled by the runner from the effect input's
	/// size (C++ `tex->virtual_resolution()`; see the blur node's
	/// `// CPP-PARITY: blur.cpp` note).
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

		let amount = match inputs.get(AMOUNT_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(AMOUNT_INPUT, -1, time).to_double(),
		};
		let angle = match inputs.get(ANGLE_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(ANGLE_INPUT, -1, time).to_double(),
		};

		let mut params = inputs.clone();
		params.insert(AMOUNT_INPUT.to_string(), NodeValue::Float(amount));
		params.insert(ANGLE_INPUT.to_string(), NodeValue::Float(angle));

		table.push(
			ValueType::Texture,
			NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input: String::new(),
			})),
			None,
		);
	}

	/// Shader code request: always the one fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(DirBlurNode))
	}
}

/// Constructor: adds `tex_in`, `amount_in` and `angle_in` with the
/// defaults and properties documented on the constants, sets the
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

	let mut amount = crate::input::Input::new(
		AMOUNT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	amount.properties = vec![("min".to_string(), crate::value::NodeValue::Float(0.0))];
	core.add_input(amount);

	core.add_input(crate::input::Input::new(
		ANGLE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(DirBlurNode))
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
		let behavior = DirBlurNode;
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
		let n = DirBlurNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(AMOUNT_INPUT), "Amount");
		assert_eq!(n.input_name(ANGLE_INPUT), "Angle");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.dirblur");
		assert_eq!(behavior.name(), "Directional Blur");
		assert_eq!(behavior.categories(), &[Category::Filter]);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);

		let amount = core.get_input(AMOUNT_INPUT).expect("amount input");
		assert_eq!(amount.default, NodeValue::Float(0.0));
		assert!(amount
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));

		let angle = core.get_input(ANGLE_INPUT).expect("angle input");
		assert_eq!(angle.default, NodeValue::Float(0.0));
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
	fn value_zero_amount_still_pushes_job_with_defaults() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.dirblur");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(0.0)),
			"the blur amount rides in the job params"
		);
		assert_eq!(
			payload.params.get(ANGLE_INPUT),
			Some(&NodeValue::Float(0.0))
		);
	}

	#[test]
	fn value_takes_params_from_row() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(AMOUNT_INPUT.to_string(), NodeValue::Float(12.0)),
			(ANGLE_INPUT.to_string(), NodeValue::Float(90.0)),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(12.0))
		);
		assert_eq!(
			payload.params.get(ANGLE_INPUT),
			Some(&NodeValue::Float(90.0))
		);
	}

	#[test]
	fn value_takes_params_from_core() {
		let (mut core, _) = create();
		core.set_standard_value(AMOUNT_INPUT, -1, NodeValue::Float(4.0));
		core.set_standard_value(ANGLE_INPUT, -1, NodeValue::Float(-45.0));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(4.0))
		);
		assert_eq!(
			payload.params.get(ANGLE_INPUT),
			Some(&NodeValue::Float(-45.0))
		);
	}

	#[test]
	fn shader_code_is_centered_tap_average_without_switch() {
		let n = DirBlurNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform float amount_in;"));
		assert!(glsl.contains("uniform float angle_in;"));
		assert!(glsl.contains("uniform vec2 resolution_in;"));
		assert!(glsl.contains("#define TAP_COUNT 16"));
		assert!(glsl.contains("float(TAP_COUNT - 1) * 0.5"));
		assert!(glsl.contains("frag_color = sum / float(TAP_COUNT);"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn tap_count_matches_shader() {
		let glsl = DirBlurNode::shader_frag();
		assert!(
			glsl.contains(&format!("#define TAP_COUNT {TAP_COUNT}")),
			"the shader and the Rust-side tap count must agree"
		);
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Directional Blur");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.dirblur",
		name: "Directional Blur",
		categories: &[Category::Filter],
		create,
	});
}
