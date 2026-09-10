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

//! Sharpen filter (clean-room reimplementation of the CImg `Sharpen`
//! / `net.sf.cimg.CImgSharpen` effect; `ofx-misc` used for parameter
//! semantics only, no code copied).
//!
//! Upstream blurs a copy of the image and mixes it back as
//! `input * (1 + amount) - blurred * amount`, i.e. an unsharp mask
//! `out = x + amount * (x - blur(x))` with its CImg blur kernel
//! (`amount` defaults to 1, negative values soften). The blur is a
//! 3x3 box average computed inline in the same pass — upstream's
//! separable, user-sized CImg blur would need a multi-pass job, which
//! this node trades for a single-pass small-kernel mask.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Sharpening amount input id (upstream `amount`, "Amount"). Type:
/// float; default `1.0` (upstream default); no range restriction —
/// upstream's range is unbounded, so a negative amount softens
/// (`out = x + amount * (x - blur(x))` turns into a blend toward the
/// blurred image). `0.0` is an exact identity.
pub const AMOUNT_INPUT: &str = "amount_in";

/// Sharpen filter node. Unsharp mask against an inline 3x3 box blur.
pub struct SharpenNode;

/// Fragment shader: 3x3 box blur of the pixel neighborhood, then the
/// unsharp mask `x + amount * (x - blur(x))` on all four channels.
/// Offsets are one texel (`resolution_in`, auto-filled by the renderer);
/// edge-of-frame taps clamp to the border pixel. The result is not
/// clamped: F32 output, and the overshoot on either side of an edge is
/// the point of the effect. `switch` is deliberately not used (naga
/// rejects it).
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float amount_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

// Blur kernel edge (3x3 box, i.e. 9 taps).
#define KERNEL_RADIUS 1

void main() {
  vec2 texel = vec2(1.0) / resolution_in;

  vec4 center = texture(tex_in, ove_texcoord);

  // 3x3 box blur, computed inline: the unsharp mask only needs a small
  // neighborhood, so no second pass (and no nested job) is involved.
  vec4 blurred = vec4(0.0);
  for (int y = -KERNEL_RADIUS; y <= KERNEL_RADIUS; ++y) {
    for (int x = -KERNEL_RADIUS; x <= KERNEL_RADIUS; ++x) {
      blurred += texture(tex_in, ove_texcoord + vec2(float(x), float(y)) * texel);
    }
  }
  float kernel_area = float((2 * KERNEL_RADIUS + 1) * (2 * KERNEL_RADIUS + 1));
  blurred /= kernel_area;

  frag_color = center + amount_in * (center - blurred);
}
"#;

impl SharpenNode {
	/// Fragment shader for any request (this node has a single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for SharpenNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Sharpen"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.sharpen"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description.
	fn description(&self) -> &str {
		"Sharpens an image with an unsharp mask."
	}

	/// Localized input names: `tex_in` -> "Input", `amount_in` ->
	/// "Amount".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			AMOUNT_INPUT => "Amount",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; otherwise push the
	/// unsharp-mask shader job.
	///
	/// A zero `amount_in` still pushes the job (the mask then reduces to
	/// the input pixel exactly) rather than passing the input texture
	/// through as the blur node does for a zero radius: the plan's node
	/// template reserves the pass-through for the no-texture case, and
	/// feeding the input's CPU texture downstream here would silently
	/// break nodes that expect a GPU texture. The resolved amount is
	/// written into the job row so the uniform is always present, even
	/// when the incoming row only carries the effect input.
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

		let mut params = inputs.clone();
		params.insert(AMOUNT_INPUT.to_string(), NodeValue::Float(amount));

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
		Some(Box::new(SharpenNode))
	}
}

/// Constructor: adds `tex_in` and `amount_in` with the defaults and
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

	core.add_input(crate::input::Input::new(
		AMOUNT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(SharpenNode))
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
		let behavior = SharpenNode;
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
		let n = SharpenNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(AMOUNT_INPUT), "Amount");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.sharpen");
		assert_eq!(behavior.name(), "Sharpen");
		assert_eq!(behavior.categories(), &[Category::Filter]);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(
			core.get_input(AMOUNT_INPUT).expect("amount input").default,
			NodeValue::Float(1.0)
		);
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
	fn value_pushes_job_with_default_amount() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.sharpen");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(1.0)),
			"the sharpening amount rides in the job params"
		);
	}

	#[test]
	fn value_takes_amount_from_row() {
		let (core, _) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(AMOUNT_INPUT.to_string(), NodeValue::Float(0.0)),
		]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(0.0))
		);
	}

	#[test]
	fn value_takes_amount_from_core() {
		let (mut core, _) = create();
		core.set_standard_value(AMOUNT_INPUT, -1, NodeValue::Float(2.5));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let payload = run(&core, &inputs);
		assert_eq!(
			payload.params.get(AMOUNT_INPUT),
			Some(&NodeValue::Float(2.5))
		);
	}

	#[test]
	fn shader_code_is_inline_unsharp_mask_without_switch() {
		let n = SharpenNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform float amount_in;"));
		assert!(glsl.contains("uniform vec2 resolution_in;"));
		assert!(glsl.contains("#define KERNEL_RADIUS 1"));
		assert!(glsl.contains("frag_color = center + amount_in * (center - blurred);"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Sharpen");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.sharpen",
		name: "Sharpen",
		categories: &[Category::Filter],
		create,
	});
}
