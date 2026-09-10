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

//! Erode filter (clean-room reimplementation of the CImg `Erode`/
//! `net.sf.cimg.CImgErode` effect; `ofx-misc` used for parameter
//! semantics only, no code copied).

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Radius input id. Type: float; default `1.0`; properties: `min = 0.0`,
/// `max = 7.0`. The structuring element is a square of `2 * radius + 1`
/// pixels on a side, in pixels.
pub const RADIUS_INPUT: &str = "radius_in";

/// Largest supported radius. The shader clamps to this; the neighborhood
/// is `15 x 15` pixels, sampled in a single pass (see [`SHADER_FRAG`]).
pub const MAX_RADIUS: f64 = 7.0;

/// Erode filter node. Morphological minimum over a square neighborhood.
pub struct ErodeNode;

/// Fragment shader: one pass sampling the full `(2r+1)^2` neighborhood
/// and taking the per-channel minimum. `iterations` stays 1 — a single
/// wide pass is mathematically identical to `r` 3x3 iterations for a
/// square structuring element (min is associative and idempotent), and
/// avoids the ping-pong feedback textures entirely. `radius_in` is
/// rounded to the nearest integer and clamped to `0..=7`; offsets are
/// pixel-space texel steps (`resolution_in`, texel centers), with
/// edge-of-frame taps clamping to the border pixel.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float radius_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  int radius = int(clamp(radius_in, 0.0, 7.0) + 0.5);
  vec2 texel = vec2(1.0) / resolution_in;

  vec4 acc = texture(tex_in, ove_texcoord);
  for (int dy = -radius; dy <= radius; ++dy) {
    for (int dx = -radius; dx <= radius; ++dx) {
      vec2 uv = ove_texcoord + vec2(float(dx), float(dy)) * texel;
      acc = min(acc, texture(tex_in, uv));
    }
  }

  frag_color = acc;
}
"#;

impl ErodeNode {
	/// Fragment shader for any request (this node has a single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for ErodeNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Erode"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.erode"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description.
	fn description(&self) -> &str {
		"Shrink bright areas by taking the minimum over a square neighborhood."
	}

	/// Localized input names: `tex_in` -> "Input", `radius_in` ->
	/// "Radius".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			RADIUS_INPUT => "Radius",
			_ => id,
		}
	}

	/// Evaluate outputs: no texture -> push nothing; radius <= 0 -> push
	/// the input texture unchanged (the same no-work shortcut as
	/// `blur.rs`'s `can_push_job`); otherwise push the single-pass
	/// neighborhood shader job.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		use crate::value::{NodeValue, ValueType};

		let tex = match inputs.get(TEXTURE_INPUT) {
			Some(tex @ NodeValue::Texture(_)) => tex.clone(),
			_ => return,
		};

		let radius = match inputs.get(RADIUS_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(RADIUS_INPUT, -1, time).to_double(),
		};

		if radius <= 0.0 {
			table.push(ValueType::Texture, tex, None);
			return;
		}

		let mut params = inputs.clone();
		params.insert(RADIUS_INPUT.to_string(), NodeValue::Float(radius));

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
		Some(Box::new(ErodeNode))
	}
}

/// Constructor: adds `tex_in` and `radius_in` with the defaults and
/// properties documented on the constants, sets the video-effect flag and
/// the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut radius = crate::input::Input::new(
		RADIUS_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	radius.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
		("max".to_string(), crate::value::NodeValue::Float(MAX_RADIUS)),
	];
	core.add_input(radius);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(ErodeNode))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::value::{NodeValue, NodeValueTable, ValueType};
	use oak_core::Rational;

	fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	#[test]
	fn input_names() {
		let n = ErodeNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(RADIUS_INPUT), "Radius");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.erode");
		assert_eq!(behavior.categories(), &[Category::Filter]);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		let radius = core.get_input(RADIUS_INPUT).expect("radius input");
		assert_eq!(radius.default, NodeValue::Float(1.0));
		assert!(radius
			.properties
			.iter()
			.any(|(k, v)| k == "min" && *v == NodeValue::Float(0.0)));
		assert!(radius
			.properties
			.iter()
			.any(|(k, v)| k == "max" && *v == NodeValue::Float(MAX_RADIUS)));
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
	fn value_zero_radius_passes_through() {
		let (core, behavior) = create();
		let tex = tex();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex.clone()),
			(RADIUS_INPUT.to_string(), NodeValue::Float(0.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert_eq!(table.get(ValueType::Texture), Some(&tex));
	}

	#[test]
	fn value_pushes_single_pass_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(RADIUS_INPUT.to_string(), NodeValue::Float(2.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
			.expect("shader job payload boxed");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.erode");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.iterative_input, "");
		assert_eq!(
			payload.params.get(RADIUS_INPUT),
			Some(&NodeValue::Float(2.0))
		);
	}

	#[test]
	fn value_default_radius_pushes_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		let payload = unsafe { crate::handle::get_checked::<ShaderJobPayload>(h) }
			.expect("shader job payload boxed");
		assert_eq!(
			payload.params.get(RADIUS_INPUT),
			Some(&NodeValue::Float(1.0))
		);
	}

	#[test]
	fn shader_code_is_single_pass_min_without_switch() {
		let n = ErodeNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform vec2 resolution_in;"));
		assert!(glsl.contains("acc = min(acc, texture(tex_in, uv));"));
		assert!(glsl.contains("int(clamp(radius_in, 0.0, 7.0) + 0.5)"));
		assert!(!glsl.contains("switch"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Erode");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.erode",
		name: "Erode",
		categories: &[Category::Filter],
		create,
	});
}
