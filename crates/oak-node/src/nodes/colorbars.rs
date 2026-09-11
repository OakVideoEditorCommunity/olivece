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

//! SMPTE color bars generator: clean-room reimplementation of the
//! OpenFX-Misc `ColorBars` plugin's parameter semantics (upstream
//! github.com/cgvirus/OpenFX-Misc, GPL2; read for behavior only, no
//! upstream code copied).
//!
//! Deviations from the upstream plugin: it lays the pattern out on an
//! IRE-scale grid driven by `barIntensity` (0..100 IRE, default 75) and
//! `outputIRE`, with the bar heights in IRE units; this node renders the
//! standard SMPTE ECR layout instead — a 75% 7-bar top field (two thirds
//! of the frame), the mid strip of blue/black/magenta/black/cyan/black/
//! white, and the reverse-grayscale bottom field with the PLUGE on the
//! right — and exposes the upstream intensity choice as a two-way
//! `standard_in` combo (75% / 100%).

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};

/// Base texture input id (the shared generator-with-merge base). Type:
/// texture; flags: not-keyframable; this is the generator's effect input.
pub const BASE_INPUT: &str = super::generatorwithmerge::BASE_INPUT;

/// Standard input id (upstream `kParamBarIntensity` "Bar intensity",
/// default 75 IRE). Type: combo; default 0; combo strings: 0 = "SMPTE
/// 75%", 1 = "Full 100%". The 100% setting scales the chromatic bars and
/// the reverse-grayscale ramp to full amplitude.
pub const STANDARD_INPUT: &str = "standard_in";

/// SMPTE color bars generator node.
pub struct ColorBarsNode;

/// Fragment shader for the `"colorbars"` shader id.
///
/// The frame is split in normalized coordinates: the top field (`v <
/// 2/3`) holds the seven bars (white, yellow, cyan, green, magenta, red,
/// blue), the mid strip (`v < 3/4`) the blue/black/magenta/black/cyan/
/// black/white row, and the bottom field the reverse-grayscale ramp
/// (left-to-right decreasing) ending in the three-step PLUGE (below
/// black / black / above black). `ove_texcoord.y` runs downward — frame
/// row 0 is the top of the image — so the first branch is the top field.
///
/// `level` is the bar amplitude: 0.75 for the 75% standard, 1.0 for the
/// 100% one. The branch chain is spelled with if/else (a `switch` would
/// be rejected by the shader translator).
const SHADER_FRAG: &str = r#"uniform vec2 resolution_in;
uniform int standard_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    float level = 0.75;
    if (standard_in == 1) {
        level = 1.0;
    }

    float column = floor(ove_texcoord.x * 7.0);
    vec3 col = vec3(0.0);

    if (ove_texcoord.y < 2.0 / 3.0) {
        // Top field: white, yellow, cyan, green, magenta, red, blue.
        if (column < 0.5) {
            col = vec3(level);
        } else if (column < 1.5) {
            col = vec3(level, level, 0.0);
        } else if (column < 2.5) {
            col = vec3(0.0, level, level);
        } else if (column < 3.5) {
            col = vec3(0.0, level, 0.0);
        } else if (column < 4.5) {
            col = vec3(level, 0.0, level);
        } else if (column < 5.5) {
            col = vec3(level, 0.0, 0.0);
        } else {
            col = vec3(0.0, 0.0, level);
        }
    } else if (ove_texcoord.y < 3.0 / 4.0) {
        // Mid strip: blue, black, magenta, black, cyan, black, white.
        if (column < 0.5) {
            col = vec3(0.0, 0.0, level);
        } else if (column < 1.5) {
            col = vec3(0.0);
        } else if (column < 2.5) {
            col = vec3(level, 0.0, level);
        } else if (column < 3.5) {
            col = vec3(0.0);
        } else if (column < 4.5) {
            col = vec3(0.0, level, level);
        } else if (column < 5.5) {
            col = vec3(0.0);
        } else {
            col = vec3(level);
        }
    } else {
        // Bottom field: reverse grayscale ramp, then the PLUGE steps
        // (below black / black / above black) in the last bar.
        float g = level * (5.0 - column) / 5.0;
        if (column > 5.5) {
            float sub = floor((ove_texcoord.x * 7.0 - 6.0) * 3.0);
            if (sub < 0.5) {
                g = -0.04;
            } else if (sub < 1.5) {
                g = 0.0;
            } else {
                g = 0.04;
            }
        }
        col = vec3(g);
    }

    frag_color = vec4(col, 1.0);
}
"#;

impl ColorBarsNode {
	/// Fragment shader for the `"colorbars"` request.
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}
}

impl NodeBehavior for ColorBarsNode {
	/// Human-readable name.
	fn name(&self) -> &str {
		"Color Bars"
	}

	/// Stable type id.
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.colorbars"
	}

	/// Categories.
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description.
	fn description(&self) -> &str {
		"Generate SMPTE color bars."
	}

	/// Localized input names: the merge base's `base_in` -> "Base" plus
	/// `standard_in` -> "Standard".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			BASE_INPUT => "Base",
			STANDARD_INPUT => "Standard",
			_ => id,
		}
	}

	/// Combo input option labels: `standard_in` -> "SMPTE 75%",
	/// "Full 100%".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			STANDARD_INPUT => vec!["SMPTE 75%", "Full 100%"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs: a `"colorbars"` shader job over the value row,
	/// pushed through `push_mergable_job` — merged alpha-over `base_in`
	/// when a base texture is connected, pushed bare otherwise.
	///
	/// The params row carries `standard_in`; `resolution_in` is filled by
	/// the runner from the render target size, so it is not part of the
	/// params here.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let mut params = inputs.clone();
		// The input is part of the row in the traverser flow (the bare key
		// is always inserted for an unconnected input); fall back to the
		// node's own value for direct `value()` calls.
		if !params.contains_key(STANDARD_INPUT) {
			params.insert(
				STANDARD_INPUT.to_string(),
				core.value_at_time(STANDARD_INPUT, -1, time),
			);
		}

		let job = crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations: 1,
			type_id: self.type_id().to_string(),
			shader_id: "colorbars".to_string(),
			effect_input: core.effect_input.clone(),
			params,
			iterative_input: String::new(),
		}));
		super::generatorwithmerge::GeneratorWithMerge::push_mergable_job(inputs, job, table);
	}

	/// Shader code request: `"colorbars"` returns this node's shader; the
	/// `"mrg"` request returns the shared alpha-over merge shader;
	/// anything else is unsupported.
	fn shader_code(&self, request: &str) -> Option<String> {
		match request {
			"colorbars" => Some(Self::shader_frag().to_string()),
			"mrg" => Some(super::generatorwithmerge::merge_shader_frag().to_string()),
			_ => None,
		}
	}

	/// Deep copy.
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ColorBarsNode))
	}
}

/// Constructor: adds `base_in` (not-keyframable) and `standard_in` with
/// the defaults documented on the constants, sets the video-effect flag
/// and makes `base_in` the effect input (the `GeneratorWithMerge`
/// constructor side effects).
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut base = crate::input::Input::new(
		BASE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);

	core.add_input(crate::input::Input::new(
		STANDARD_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(0),
	));

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = BASE_INPUT.to_string();

	(core, Box::new(ColorBarsNode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
    fn input_names_and_combo_strings() {
        let n = ColorBarsNode;
        assert_eq!(n.input_name(STANDARD_INPUT), "Standard");
        assert_eq!(
            n.input_name(super::super::generatorwithmerge::BASE_INPUT),
            "Base"
        );
        assert_eq!(n.input_combo_strings(STANDARD_INPUT), vec!["SMPTE 75%", "Full 100%"]);
        assert!(n.input_combo_strings("other_in").is_empty());
    }

    #[test]
    fn create_wires_inputs_and_flags() {
        let (core, behavior) = create();
        assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.colorbars");
        assert_eq!(
            core.get_input(STANDARD_INPUT).unwrap().default,
            NodeValue::Combo(0)
        );
        assert_eq!(behavior.input_combo_strings(STANDARD_INPUT).len(), 2);
        assert_eq!(core.effect_input, BASE_INPUT);
        assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
    }

    #[test]
    fn value_pushes_generator_job() {
        let (core, behavior) = create();
        let mut table = NodeValueTable::default();
        behavior.value(
            &core,
            &NodeValueRow::default(),
            Rational::new(0, 1),
            &mut table,
        );
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::jobs::shader_job(&handle) }
            .expect("shader job payload expected");
        assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.colorbars");
        assert_eq!(payload.shader_id, "colorbars");
        assert_eq!(payload.iterations, 1);
        assert_eq!(payload.effect_input, BASE_INPUT);
        assert_eq!(payload.iterative_input, "");
        assert_eq!(
            payload.params.get(STANDARD_INPUT),
            Some(&NodeValue::Combo(0))
        );
    }

    #[test]
    fn value_row_standard_wins_over_the_node_default() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([(STANDARD_INPUT.to_string(), NodeValue::Combo(1))]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let payload = unsafe { crate::jobs::shader_job(&handle) }
            .expect("shader job payload expected");
        assert_eq!(
            payload.params.get(STANDARD_INPUT),
            Some(&NodeValue::Combo(1))
        );
    }

    #[test]
    fn value_with_base_merges_nested_job() {
        let (core, behavior) = create();
        let inputs = NodeValueRow::from([(
            BASE_INPUT.to_string(),
            NodeValue::Texture(crate::handle::CHandle::null()),
        )]);
        let mut table = NodeValueTable::default();
        behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
        let handle = match table.get(ValueType::Texture) {
            Some(NodeValue::Texture(h)) => *h,
            _ => panic!("texture expected"),
        };
        let merge = unsafe { crate::jobs::shader_job(&handle) }
            .expect("merge job payload expected");
        assert_eq!(merge.shader_id, "mrg");
        assert_eq!(merge.effect_input, BASE_INPUT);
        match merge.params.get(crate::nodes::merge::BLEND_INPUT) {
            Some(NodeValue::Texture(blend)) => {
                let nested = unsafe { crate::jobs::shader_job(blend) }
                    .expect("nested job payload boxed");
                assert_eq!(nested.shader_id, "colorbars");
            }
            _ => panic!("nested blend job expected"),
        }
    }

    #[test]
    fn shader_code_dispatches() {
        let n = ColorBarsNode;
        let code = n.shader_code("colorbars").unwrap();
        assert!(code.contains("uniform int standard_in;"));
        assert!(code.contains("uniform vec2 resolution_in;"));
        assert!(code.contains("float level = 0.75;"));
        assert!(!code.contains("switch"));
        assert!(n
            .shader_code("mrg")
            .unwrap()
            .contains("base_col *= 1.0 - blend_col.a;"));
        assert!(n.shader_code("other").is_none());
    }

    #[test]
    fn duplicate_clones() {
        let (core, behavior) = create();
        let dup = behavior.duplicate(&core).unwrap();
        assert_eq!(dup.name(), "Color Bars");
    }
}

/// Register this node type.
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.colorbars",
		name: "Color Bars",
		categories: &[Category::Generator],
		create,
	});
}
