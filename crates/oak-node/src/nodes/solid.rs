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

//! Solid color generator (C++ `src/node/src/generator/solid/solid.{h,cpp}`,
//! `olive::SolidGenerator`).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Color input id (C++ `k_color_input`). Type: color; default
/// `(1.0, 0.0, 0.0, 1.0)` (red — "a color that isn't black").
pub const COLOR_INPUT: &str = "color_in";

/// Solid color generator node. Has no own member fields in C++.
pub struct SolidGenerator;

/// Fragment shader (C++ loads the `:/shaders/solid.frag` resource in
/// `get_shader_code`). Text copied verbatim from
/// `engine/shaders/solid.frag`.
const SHADER_FRAG: &str = r#"uniform vec4 color_in;

out vec4 frag_color;

void main(void) {
    frag_color = color_in;
}
"#;

impl SolidGenerator {
	/// Fragment shader for all shader requests (C++
	/// `get_shader_code()` ignores the request id).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for SolidGenerator {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Solid"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.solidgenerator"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Generate a solid color."
	}

	/// Localized input names (C++ `retranslate()`): `color_in` ->
	/// "Color".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			COLOR_INPUT => "Color",
			_ => id,
		}
	}

	/// Evaluate outputs (C++ `value()`): always pushes a shader job
	/// built from the whole input row, run at the sequence video params
	/// (the generator has no texture input to source params from).
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params
	/// row carries the color uniform keyed by `color_in`
	/// (`// CPP-PARITY: solid.cpp` `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
				node_id: crate::id::NodeId::INVALID,
				time,
				iterations: 1,
				type_id: self.type_id().to_string(),
				shader_id: String::new(),
				effect_input: core.effect_input.clone(),
				params: inputs.clone(),
				iterative_input: String::new(),
			}))),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): returns the solid
	/// fragment shader for any request id.
	fn shader_code(&self, request: &str) -> Option<String> {
		let _ = request;
		Some(Self::shader_frag().to_string())
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(SolidGenerator))
	}
}

/// Constructor (C++ `SolidGenerator::SolidGenerator()`): adds
/// `color_in` with the default documented on the constant.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();
	let mut color = crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Color,
		crate::value::NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
	);
	color.properties = vec![(
		"view".to_string(),
		crate::value::NodeValue::Text("color".into()),
	)];
	core.add_input(color);
	(core, Box::new(SolidGenerator))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = SolidGenerator;
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.solidgenerator"
		);
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Color([1.0, 0.0, 0.0, 1.0])
		);
	}

	#[test]
	fn value_pushes_shader_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(
			COLOR_INPUT.to_string(),
			NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(3, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			unreachable!()
		};
		let job = unsafe {
			crate::jobs::shader_job(handle)
		}
		.expect("solid output boxes a ShaderJobPayload");
		assert_eq!(
			job.type_id,
			"org.olivevideoeditor.Olive.solidgenerator"
		);
		assert_eq!(job.shader_id, "");
		assert_eq!(job.iterations, 1);
		assert_eq!(job.effect_input, "");
		assert_eq!(
			job.params.get(COLOR_INPUT).map(|v| v.to_double()),
			Some(0.0)
		);
	}

	#[test]
	fn shader_code_returns_solid_shader() {
		let n = SolidGenerator;
		let code = n.shader_code("anything").unwrap();
		assert!(code.contains("uniform vec4 color_in;"));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Solid");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.solidgenerator`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.solidgenerator",
		name: "Solid",
		categories: &[Category::Generator],
		create,
	});
}
