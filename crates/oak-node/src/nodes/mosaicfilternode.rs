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

//! Mosaic filter (C++
//! `src/node/src/filter/mosaic/mosaicfilternode.{h,cpp}`,
//! `olive::MosaicFilterNode`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Horizontal block count input id (C++ `k_horiz_input`). Type: float;
/// default `32.0`; properties: `min = 1.0`.
pub const HORIZ_INPUT: &str = "horiz_in";

/// Vertical block count input id (C++ `k_vert_input`). Type: float;
/// default `18.0`; properties: `min = 1.0`.
pub const VERT_INPUT: &str = "vert_in";

/// Mosaic filter node. Pixelates the image into a grid of blocks. The
/// C++ class declares no own member fields.
pub struct MosaicFilterNode;

/// Fragment shader (C++ `get_shader_code()` loads
/// `:/shaders/mosaic.frag` via FileFunctions for any request). Text
/// copied verbatim from `engine/shaders/mosaic.frag`.
const SHADER_FRAG: &str = r#"// Input texture
uniform sampler2D tex_in;

uniform float horiz_in;
uniform float vert_in;

// Input texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  float x;
  float y;

  if (horiz_in > 0.0) {
    x = floor(ove_texcoord.x * horiz_in) / horiz_in;
  } else {
    x = ove_texcoord.x;
  }

  if (vert_in > 0.0) {
    y = floor(ove_texcoord.y * vert_in) / vert_in;
  } else {
    y = ove_texcoord.y;
  }

  vec4 color = texture(tex_in, vec2(x, y));
  frag_color = color;
}
"#;

impl MosaicFilterNode {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns `mosaic.frag`).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for MosaicFilterNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Mosaic"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.mosaicfilter"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Filter]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Apply a pixelated mosaic filter to video."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` ->
	/// "Texture", `horiz_in` -> "Horizontal", `vert_in` -> "Vertical".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Texture",
			HORIZ_INPUT => "Horizontal",
			VERT_INPUT => "Vertical",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// otherwise queue the input texture as a shader job.
	///
	/// The C++ pass-through optimization — when the block counts already
	/// equal the texture's pixel dimensions, push the input texture
	/// unchanged — compares the input values against the texture's
	/// width/height, which the Rust texture handle does not carry, so it
	/// is not representable and a shader job is always queued when a
	/// texture is present (`// CPP-PARITY: mosaicfilternode.cpp` value()).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		if !matches!(
			inputs.get(TEXTURE_INPUT),
			Some(crate::value::NodeValue::Texture(_))
		) {
			return;
		}

		// `// CPP-PARITY: mosaicfilternode.cpp` `value()` — the C++ pushes
		// `tex->to_job(job)` when the block counts differ from the texture's
		// pixel dimensions and passes the texture through when they match;
		// the Rust texture handle carries no width/height, so the
		// pass-through branch is not representable and a shader job is
		// always queued here. The C++ also forces bilinear interpolation on
		// `tex_in` (`job.SetInterpolation(tex_in, kLinear)`) so mipmapping
		// does not smear block colors; [`ShaderJobPayload`] has no
		// interpolation field and the renderer does not support it yet
		// (TODO: carry interpolation on the payload and apply it in the
		// renderer). The job is boxed here as a [`ShaderJobPayload`] that
		// the renderer's resolve hook executes and replaces with the result
		// texture; the params row is the whole input row.
		let params = inputs.clone();
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params,
				iterative_input: String::new(),
			}))),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns the mosaic fragment shader.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(MosaicFilterNode))
	}
}

/// Constructor (C++ `MosaicFilterNode::MosaicFilterNode()`): adds
/// `tex_in`, `horiz_in`, `vert_in` with the defaults and properties
/// documented on the constants, then sets the video-effect flag and the
/// effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut horiz = crate::input::Input::new(
		HORIZ_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(32.0),
	);
	horiz.properties = vec![("min".to_string(), crate::value::NodeValue::Float(1.0))];
	core.add_input(horiz);

	let mut vert = crate::input::Input::new(
		VERT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(18.0),
	);
	vert.properties = vec![("min".to_string(), crate::value::NodeValue::Float(1.0))];
	core.add_input(vert);

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(MosaicFilterNode))
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
		let n = MosaicFilterNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(HORIZ_INPUT), "Horizontal");
		assert_eq!(n.input_name(VERT_INPUT), "Vertical");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.mosaicfilter"
		);
		assert_eq!(
			core.get_input(HORIZ_INPUT).unwrap().default,
			NodeValue::Float(32.0)
		);
		assert_eq!(
			core.get_input(VERT_INPUT).unwrap().default,
			NodeValue::Float(18.0)
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
	fn value_with_texture_pushes_shader_job_payload() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(TEXTURE_INPUT.to_string(), tex()),
			(HORIZ_INPUT.to_string(), NodeValue::Float(32.0)),
			(VERT_INPUT.to_string(), NodeValue::Float(18.0)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			panic!("expected a texture-typed value");
		};
		let payload =
			unsafe { crate::jobs::shader_job(handle) }
				.expect("payload boxed behind the handle");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.mosaicfilter");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		// The whole input row travels as the params; horiz/vert stay put
		// under their input ids.
		assert_eq!(
			payload.params.get(HORIZ_INPUT),
			Some(&NodeValue::Float(32.0))
		);
		assert_eq!(
			payload.params.get(VERT_INPUT),
			Some(&NodeValue::Float(18.0))
		);
	}

	#[test]
	fn shader_code_returns_mosaic_shader() {
		let n = MosaicFilterNode;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform float horiz_in;"));
		assert!(code.contains("floor(ove_texcoord.x * horiz_in)"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Mosaic");
	}
}

/// Register this node type (C++ `k_mosaic_filter` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.mosaicfilter",
		name: "Mosaic",
		categories: &[Category::Filter],
		create,
	});
}
