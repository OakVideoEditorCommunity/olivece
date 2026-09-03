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

//! Shared generator-with-merge base (C++
//! `src/node/src/generator/shape/generatorwithmerge.{h,cpp}`,
//! `olive::GeneratorWithMerge`).
//!
//! Abstract C++ base for generators that can merge their output over a
//! base texture. Not instantiable, so this is a helper module, not a
//! [`NodeBehavior`] implementation.

use crate::jobs::ShaderJobPayload;
use crate::value::NodeValue;

/// Base texture input id (C++ `k_base_input`). Type: texture; flags:
/// not-keyframable; the base constructor makes it the effect input and
/// sets the video-effect flag.
pub const BASE_INPUT: &str = "base_in";

/// Merge fragment shader (C++ `get_shader_code()` `"mrg"` branch loads
/// `:/shaders/alphaover.frag`). Text copied verbatim from
/// `engine/shaders/alphaover.frag`.
const MERGE_SHADER_FRAG: &str = r#"uniform sampler2D base_in;
uniform sampler2D blend_in;
uniform bool base_in_enabled;
uniform bool blend_in_enabled;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec4 base_col = texture(base_in, ove_texcoord);
    vec4 blend_col = texture(blend_in, ove_texcoord);

    if (!base_in_enabled && !blend_in_enabled) {
        frag_color = vec4(0.0);
        return;
    }

    if (!base_in_enabled) {
        frag_color = blend_col;
        return;
    }

    if (!blend_in_enabled) {
        frag_color = base_col;
        return;
    }

    base_col *= 1.0 - blend_col.a;
    base_col += blend_col;

    frag_color = base_col;
}
"#;

/// Merge fragment shader for the `"mrg"` shader request (C++
/// `GeneratorWithMerge::get_shader_code()` `"mrg"` branch; any other
/// request returns an empty `ShaderCode`).
pub fn merge_shader_frag() -> &'static str {
	MERGE_SHADER_FRAG
}

/// Helper mirroring the C++ `GeneratorWithMerge` base. The base has no
/// own member fields; its constructor adds [`BASE_INPUT`], makes it the
/// effect input and sets the video-effect flag, and its `retranslate()`
/// names `base_in` "Base".
pub struct GeneratorWithMerge;

impl GeneratorWithMerge {
	/// Push a generated texture job, merged over the base input when
	/// one is connected (C++ `push_mergable_job()`): with a base
	/// texture, builds a `"mrg"` shader job with the base input as
	/// `MergeNode::k_base_in` and the generated job as
	/// `MergeNode::k_blend_in`, pushing `base->to_job(merge)`; without
	/// a base, pushes the generated job unchanged.
	///
	/// `job` boxes the generated job's [`ShaderJobPayload`]. Without a
	/// base the box is pushed through as-is (addref'd, so the table's
	/// reference outlives the caller's handle). With a base, a new
	/// `"mrg"` payload is boxed whose params row carries the base
	/// texture under [`BASE_INPUT`] and the generated job (the blend
	/// layer) under [`crate::nodes::merge::BLEND_INPUT`], with
	/// `type_id`/`time` taken from the generated job — mirroring the
	/// C++ `merge` shader job. A null or foreign `job` handle (the
	/// legacy deferred model) keeps the old placeholder behavior.
	pub fn push_mergable_job(
		inputs: &crate::value::NodeValueRow,
		job: crate::handle::CHandle,
		table: &mut crate::value::NodeValueTable,
	) {
		match inputs.get(BASE_INPUT) {
			Some(NodeValue::Texture(_)) => {
				// A base is connected: the C++ pushes
				// `base->to_job(ShaderJob("mrg"))` — an alpha-over merge
				// of the generated texture over the base.
				// `// CPP-PARITY: generatorwithmerge.cpp` push_mergable_job.
				match unsafe { crate::handle::get_checked::<ShaderJobPayload>(&job) } {
					Some(gen_job) => {
						let mut params = crate::value::NodeValueRow::new();
						params.insert(
							BASE_INPUT.to_string(),
							inputs
								.get(BASE_INPUT)
								.cloned()
								.expect("base input matched above"),
						);
						params.insert(
							crate::nodes::merge::BLEND_INPUT.to_string(),
							NodeValue::Texture(unsafe { job.addref() }),
						);
						table.push(
							crate::value::ValueType::Texture,
							NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
								node_id: crate::id::NodeId::INVALID,
								time: gen_job.time,
								iterations: 1,
								type_id: gen_job.type_id.clone(),
								shader_id: "mrg".to_string(),
								effect_input: BASE_INPUT.to_string(),
								params,
								iterative_input: String::new(),
							})),
							None,
						);
					}
					None => {
						// Legacy null-handle deferred model: keep the
						// placeholder — TODO: drop once every generator
						// boxes a job payload.
						table.push(
							crate::value::ValueType::Texture,
							NodeValue::Texture(crate::handle::CHandle::null()),
							None,
						);
					}
				}
			}
			_ => {
				table.push(
					crate::value::ValueType::Texture,
					NodeValue::Texture(unsafe { job.addref() }),
					None,
				);
			}
		}
	}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn push_job_without_base_pushes_job_unchanged() {
		let job = crate::handle::make_owned(crate::jobs::ShaderJobPayload {
			type_id: "org.olivevideoeditor.Olive.solidgenerator".to_string(),
			time: Rational::new(2, 1),
			shader_id: "1".to_string(),
			..Default::default()
		});
		let mut table = NodeValueTable::default();
		GeneratorWithMerge::push_mergable_job(
			&crate::value::NodeValueRow::default(),
			job,
			&mut table,
		);
		let handle = match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(&handle)
		}
		.expect("job payload boxed");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.solidgenerator");
		assert_eq!(payload.shader_id, "1");
		assert_eq!(payload.time, Rational::new(2, 1));
	}

	#[test]
	fn push_job_without_base_null_job_pushes_null() {
		let job = crate::handle::CHandle::null();
		let mut table = NodeValueTable::default();
		GeneratorWithMerge::push_mergable_job(
			&crate::value::NodeValueRow::default(),
			job,
			&mut table,
		);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => assert!(h.is_null()),
			_ => panic!("texture expected"),
		}
	}

	#[test]
	fn push_job_with_base_boxes_merge_payload() {
		let gen_job = crate::handle::make_owned(crate::jobs::ShaderJobPayload {
			type_id: "org.olivevideoeditor.Olive.solidgenerator".to_string(),
			time: Rational::new(2, 1),
			shader_id: "1".to_string(),
			..Default::default()
		});
		let base = NodeValue::Texture(crate::handle::make_owned::<u8>(7));
		let inputs = crate::value::NodeValueRow::from([(BASE_INPUT.to_string(), base.clone())]);
		let mut table = NodeValueTable::default();
		GeneratorWithMerge::push_mergable_job(&inputs, gen_job, &mut table);

		let handle = match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => *h,
			_ => panic!("texture expected"),
		};
		let merge = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(&handle)
		}
		.expect("merge job payload boxed");
		assert_eq!(merge.shader_id, "mrg");
		assert_eq!(merge.type_id, "org.olivevideoeditor.Olive.solidgenerator");
		assert_eq!(merge.time, Rational::new(2, 1));
		assert_eq!(merge.iterations, 1);
		assert_eq!(merge.effect_input, BASE_INPUT);
		assert_eq!(merge.params.get(BASE_INPUT), Some(&base));
		let blend = match merge.params.get(crate::nodes::merge::BLEND_INPUT) {
			Some(NodeValue::Texture(h)) => *h,
			_ => panic!("texture expected"),
		};
		let blend_job = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(&blend)
		}
		.expect("blend job payload boxed");
		assert_eq!(blend_job.type_id, "org.olivevideoeditor.Olive.solidgenerator");
		assert_eq!(blend_job.shader_id, "1");
	}

	#[test]
	fn push_job_with_base_null_job_keeps_placeholder() {
		let job = crate::handle::CHandle::null();
		let inputs = crate::value::NodeValueRow::from([(
			BASE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		GeneratorWithMerge::push_mergable_job(&inputs, job, &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(h)) => assert!(h.is_null()),
			_ => panic!("texture expected"),
		}
	}
}
