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

//! Color matrix filter (clean-room reimplementation of OpenFX
//! `net.sf.openfx.ColorMatrixPlugin`; `ofx-misc` used for parameter
//! semantics only, no code copied).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id. Type: texture; flags: not-keyframable; this is the
/// node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// The sixteen matrix input ids (C++ OFX exposes one matrix parameter;
/// Olive has no matrix input widget, so the 4x4 matrix arrives as
/// sixteen scalar inputs). Row-major: `m0..m3` are the weights feeding
/// the output red channel from the input R, G, B, A; `m4..m7` the green
/// channel; `m8..m11` blue; `m12..m15` alpha. Typed as pairs of indices
/// by [`MATRIX_LABELS`].
pub const MATRIX_INPUTS: [&str; 16] = [
	"m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9", "m10", "m11", "m12", "m13",
	"m14", "m15",
];

/// Human-readable label per [`MATRIX_INPUTS`] entry: the output channel
/// before the arrow, the input channel after it. The diagonal (m0, m5,
/// m10, m15) is `1.0`, every other element `0.0` — the identity matrix.
pub const MATRIX_LABELS: [&str; 16] = [
	"R <- R", "R <- G", "R <- B", "R <- A", "G <- R", "G <- G", "G <- B", "G <- A", "B <- R",
	"B <- G", "B <- B", "B <- A", "A <- R", "A <- G", "A <- B", "A <- A",
];

/// Matrix uniform name. Not a node input: the `value()` hook inserts the
/// packed matrix into the job params row under this key (the
/// `transform_in` precedent in `transformdistortnode.rs`). `resolution_in`
/// is unused — the filter is coordinate-independent.
pub const MATRIX_UNIFORM: &str = "matrix_in";

/// Color matrix filter node. Multiplies the RGBA vector by a 4x4 matrix.
pub struct ColorMatrixNode;

/// Fragment shader: `NodeValue::Matrix` is row-major and the runner
/// transposes it into GLSL's column-major layout, so `matrix_in * color`
/// is the row-major matrix times the column vector — output channel `c`
/// is `sum_j m[c * 4 + j] * color[j]`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;
uniform mat4 matrix_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  frag_color = matrix_in * texture(tex_in, ove_texcoord);
}
"#;

impl ColorMatrixNode {
	/// Fragment shader for any request (this node has a single variant).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for ColorMatrixNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Color Matrix"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.colormatrix"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description.
	fn description(&self) -> &str {
		"Multiply a video's RGBA channels by a 4x4 matrix."
	}

	/// Localized input names: `tex_in` -> "Input", `mN` ->
	/// "OUT <- IN" (see [`MATRIX_LABELS`]).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		if id == TEXTURE_INPUT {
			return "Input";
		}
		MATRIX_INPUTS
			.iter()
			.position(|m| *m == id)
			.map(|i| MATRIX_LABELS[i])
			.unwrap_or(id)
	}

	/// Evaluate outputs: no texture -> push nothing; otherwise push a
	/// shader job carrying the matrix as the `matrix_in` uniform. Unlike
	/// the identity cases elsewhere there is no pass-through shortcut:
	/// the identity matrix is what the shader computes anyway, and a
	/// shortcut would skip the (tested) GPU pass.
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

		let mut matrix = [0.0f64; 16];
		for (i, id) in MATRIX_INPUTS.iter().enumerate() {
			matrix[i] = match inputs.get(*id) {
				Some(v) => v.to_double(),
				None => core.value_at_time(id, -1, time).to_double(),
			};
		}

		let mut params = inputs.clone();
		params.insert(MATRIX_UNIFORM.to_string(), NodeValue::Matrix(matrix));

		table.push(
			ValueType::Texture,
			NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
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

	/// Shader code request: always the one fragment source.
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ColorMatrixNode))
	}
}

/// Constructor: adds `tex_in` plus the sixteen matrix inputs (identity
/// defaults), sets the video-effect flag and the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	for (i, id) in MATRIX_INPUTS.iter().enumerate() {
		// The identity diagonal: m0, m5, m10, m15.
		let default = if i % 5 == 0 { 1.0 } else { 0.0 };
		let mut input = crate::input::Input::new(
			id,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(default),
		);
		input.flags |= crate::input::flags::NOT_KEYFRAMABLE;
		core.add_input(input);
	}

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(core, Box::new(ColorMatrixNode))
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
		let n = ColorMatrixNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name("m0"), "R <- R");
		assert_eq!(n.input_name("m15"), "A <- A");
		assert_eq!(n.input_name("other"), "other");
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.colormatrix");
		assert_eq!(behavior.categories(), &[Category::Color]);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_eq!(core.get_input(TEXTURE_INPUT).unwrap().value_type, ValueType::Texture);
		for (i, id) in MATRIX_INPUTS.iter().enumerate() {
			let input = core.get_input(id).expect("matrix input registered");
			assert_eq!(input.value_type, ValueType::Float);
			let expected = if i % 5 == 0 { 1.0 } else { 0.0 };
			assert_eq!(input.default, NodeValue::Float(expected), "{id} default");
		}
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
	fn value_pushes_identity_matrix_by_default() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed");
		let mut identity = [0.0f64; 16];
		for i in 0..4 {
			identity[i * 5] = 1.0;
		}
		assert_eq!(
			payload.params.get(MATRIX_UNIFORM),
			Some(&NodeValue::Matrix(identity))
		);
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.colormatrix");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.iterations, 1);
		assert!(payload.params.contains_key(TEXTURE_INPUT));
	}

	#[test]
	fn value_packs_row_major_matrix_from_row() {
		let (core, behavior) = create();
		// Red -> green, green -> red, blue and alpha held.
		let swap = [0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
			1.0];
		let mut inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		for (i, id) in MATRIX_INPUTS.iter().enumerate() {
			inputs.insert(id.to_string(), NodeValue::Float(swap[i]));
		}
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed");
		assert_eq!(
			payload.params.get(MATRIX_UNIFORM),
			Some(&NodeValue::Matrix(swap))
		);
	}

	#[test]
	fn value_reads_matrix_from_core_when_absent_from_row() {
		let (mut core, behavior) = create();
		core.set_standard_value("m1", -1, NodeValue::Float(0.25));
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let Some(NodeValue::Texture(h)) = table.get(ValueType::Texture) else {
			panic!("shader job expected");
		};
		let payload = unsafe { crate::jobs::shader_job(h) }
			.expect("shader job payload boxed");
		let Some(NodeValue::Matrix(m)) = payload.params.get(MATRIX_UNIFORM) else {
			panic!("matrix uniform expected");
		};
		assert_eq!(m[1], 0.25);
		assert_eq!(m[0], 1.0);
	}

	#[test]
	fn shader_code_declares_matrix_uniform() {
		let n = ColorMatrixNode;
		let glsl = n.shader_code("").unwrap();
		assert!(glsl.contains("uniform mat4 matrix_in;"));
		assert!(glsl.contains("frag_color = matrix_in * texture(tex_in, ove_texcoord);"));
	}

	#[test]
	fn duplicate_clones_behavior() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Color Matrix");
	}
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.colormatrix",
		name: "Color Matrix",
		categories: &[Category::Color],
		create,
	});
}
