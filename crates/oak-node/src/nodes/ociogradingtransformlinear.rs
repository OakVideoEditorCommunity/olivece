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

//! OCIO linear color grading node (C++
//! `src/node/src/color/ociogradingtransformlinear/ociogradingtransformlinear.{h,cpp}`,
//! `olive::OCIOGradingTransformLinearNode`).
//!
//! Note: OpenColorIO itself is never linked here; it is reached through
//! the color manager (`crate::colormanager`) and the oakrender bridge
//! (oakrender, opaque handles), like the C++ node's
//! `oakrender_color_processor_create_grading_primary` call.

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};

use crate::nodes::ociobase::OcioBase;

/// Contrast input id (C++ `k_contrast_input`). Type: vec4 (x = master,
/// y/z/w = R/G/B); default `{1, 1, 1, 1}`; properties: `min =
/// {0.01, 0.01, 0.01, 0.01}` (per `ocio::GradingPrimary::validate`),
/// `base = 0.01`, component colors `color0 = #c0c0c0`, `color1 =
/// #ff0000`, `color2 = #00ff00`, `color3 = #0000ff`.
pub const CONTRAST_INPUT: &str = "ocio_grading_primary_contrast";

/// Offset input id (C++ `k_offset_input`). Type: vec4 (x = master);
/// default `{0, 0, 0, 0}`; properties: `base = 0.01` and the component
/// colors documented on [`CONTRAST_INPUT`].
pub const OFFSET_INPUT: &str = "ocio_grading_primary_offset";

/// Exposure input id (C++ `k_exposure_input`). Type: vec4 (x = master,
/// in stops); default `{0, 0, 0, 0}`; properties: `base = 0.01`, the
/// component colors documented on [`CONTRAST_INPUT`], and (set in
/// `retranslate`) `tooltip = "Exposure increments in stops."`.
pub const EXPOSURE_INPUT: &str = "ocio_grading_primary_exposure";

/// Saturation input id (C++ `k_saturation_input`). Type: float; default
/// `1.0`; properties: `view = percentage`, `min = 0.0`.
pub const SATURATION_INPUT: &str = "ocio_grading_primary_saturation";

/// Pivot input id (C++ `k_pivot_input`). Type: float; default `0.18`
/// (default listed in `ocio::GradingPrimary`); properties: `base =
/// 0.01`.
pub const PIVOT_INPUT: &str = "ocio_grading_primary_pivot";

/// Black-clamp enable input id (C++ `k_clamp_black_enable_input`).
/// Type: boolean; default `false`.
pub const CLAMP_BLACK_ENABLE_INPUT: &str = "clamp_black_enable_in";

/// Black clamp input id (C++ `k_clamp_black_input`). Type: float;
/// default `0.0`; properties: `enabled` = current value of
/// [`CLAMP_BLACK_ENABLE_INPUT`], `base = 0.01`.
pub const CLAMP_BLACK_INPUT: &str = "ocio_grading_primary_clampBlack";

/// White-clamp enable input id (C++ `k_clamp_white_enable_input`).
/// Type: boolean; default `false`.
pub const CLAMP_WHITE_ENABLE_INPUT: &str = "clamp_white_enable_in";

/// White clamp input id (C++ `k_clamp_white_input`). Type: float;
/// default `1.0`; properties: `enabled` = current value of
/// [`CLAMP_WHITE_ENABLE_INPUT`], `base = 0.01`, and `min` = black clamp
/// + 0.000001 while the black clamp is static.
pub const CLAMP_WHITE_INPUT: &str = "ocio_grading_primary_clampWhite";

/// OCIO linear grading node. Simple linear color grading using
/// OpenColorIO (`ocio::GRADING_LIN`). Owns no members beyond the
/// embedded OCIO base state (C++ has no own private members).
pub struct OCIOGradingTransformLinearNode {
	/// Shared OCIO base state (C++ base class `OCIOBaseNode`).
	base: OcioBase,
}

/// Fragment shader. The `%1` marker is replaced at request time with the
/// OCIO-generated grading-primary GLSL stub for the linear style (the
/// renderer's OCIO_GRADING_STUBS entry; the stub declares
/// the `ocio_grading_primary_*` uniforms and `ove_grading_primary`), and
/// the body applies it to the sampled input. The uniform names match the
/// grading input ids, so the rewritten params row binds by name — the
/// C++ job's `values` assigned to the GPU uniforms by name-match the same
/// way.
const SHADER_FRAG: &str = r#"// Main texture input
uniform sampler2D tex_in;

// Main texture coordinate
in vec2 ove_texcoord;
out vec4 frag_color;

// Program will replace this with OCIO's auto-generated shader code
%1

void main() {
  vec4 col = texture(tex_in, ove_texcoord);
  frag_color = ove_grading_primary(col);
}
"#;

/// Row value for `id` (the traverser convention), else the node's own
/// standard/keyframe value (C++ `GetValueAtTime` parity for direct
/// calls).
fn row_or_standard(
	core: &NodeCore,
	inputs: &crate::value::NodeValueRow,
	id: &str,
	time: oak_core::Rational,
) -> crate::value::NodeValue {
	match inputs.get(id) {
		Some(v) => v.clone(),
		None => core.value_at_time(id, -1, time),
	}
}

/// A vec4 input value as `[x, y, z, w]` (x = master per the RGBM
/// convention), collecting a generic numeric value into a padded array.
fn to_vec4(v: crate::value::NodeValue) -> [f64; 4] {
	match v {
		crate::value::NodeValue::Vec4(v) => v,
		crate::value::NodeValue::Vec3(v) => [v[0], v[1], v[2], 0.0],
		crate::value::NodeValue::Vec2(v) => [v[0], v[1], 0.0, 0.0],
		crate::value::NodeValue::Float(v) => [v, v, v, v],
		_ => [0.0; 4],
	}
}

/// The per-channel vec3 the OCIO GPU uniforms expect: `rgb[i] =
/// f(channel[i], master)`.
fn channel_merge(v: [f64; 4], f: impl Fn(f64, f64) -> f64) -> crate::value::NodeValue {
	let m = v[0];
	crate::value::NodeValue::Vec3([
		f(v[1], m),
		f(v[2], m),
		f(v[3], m),
	])
}

/// Set or replace an input property (C++ `set_input_property`).
fn set_input_property(core: &mut NodeCore, input: &str, key: &str, value: crate::value::NodeValue) {
	if let Some(input) = core.get_input_mut(input) {
		if let Some(slot) = input.properties.iter_mut().find(|(k, _)| k == key) {
			slot.1 = value;
		} else {
			input.properties.push((key.to_string(), value));
		}
	}
}

impl OCIOGradingTransformLinearNode {
	/// Set the per-component widget colors of a vec4 input (C++
	/// `set_vec4_input_colors()`): master `#c0c0c0`, R `#ff0000`, G
	/// `#00ff00`, B `#0000ff`.
	fn set_vec4_input_colors(core: &mut NodeCore, input: &str) {
		set_input_property(
			core,
			input,
			"color0",
			crate::value::NodeValue::Text("#c0c0c0".into()),
		);
		set_input_property(
			core,
			input,
			"color1",
			crate::value::NodeValue::Text("#ff0000".into()),
		);
		set_input_property(
			core,
			input,
			"color2",
			crate::value::NodeValue::Text("#00ff00".into()),
		);
		set_input_property(
			core,
			input,
			"color3",
			crate::value::NodeValue::Text("#0000ff".into()),
		);
	}

	/// Constrain the white clamp UI minimum to just above the black
	/// clamp (C++ `update_clamp_white_minimum()`), as required by
	/// `ocio::GradingPrimary::validate`. No-op while the black clamp is
	/// keyframed or connected — a static UI minimum cannot follow an
	/// animated value, so the invariant is enforced per frame in
	/// `value()` instead.
	fn update_clamp_white_minimum(&mut self, core: &mut NodeCore) {
		let _ = self;
		// The C++ also returns while the black clamp is *connected*
		// (`is_input_connected`); edge state is not carried by NodeCore
		// (the C++ InputConnectedEvent/InputDisconnectedEvent call sites
		// re-run this), so only the keyframing half of the guard is
		// representable here — a connected black clamp would update the
		// static minimum where C++ would not.
		// `// CPP-PARITY: ociogradingtransformlinear.cpp`
		// update_clamp_white_minimum.
		if core.is_input_keyframing(CLAMP_BLACK_INPUT, -1) {
			return;
		}
		let min = core.standard_value(CLAMP_BLACK_INPUT, -1).to_double() + 0.000001;
		set_input_property(
			core,
			CLAMP_WHITE_INPUT,
			"min",
			crate::value::NodeValue::Float(min),
		);
	}

	/// (Re)build the color processor (C++ `generate_processor()`):
	/// creates a grading-primary processor of style
	/// `OAKRENDER_GRADING_PRIMARY_LIN` through the color manager and
	/// stores it with [`OcioBase::set_processor`] when creation
	/// succeeds.
	fn generate_processor(&mut self, core: &mut NodeCore) {
		let _ = core;
		// The C++ wraps the color manager and creates a grading-primary
		// processor of style `OAKRENDER_GRADING_PRIMARY_LIN` via
		// `oakrender_color_processor_create_grading_primary`, storing it
		// with OcioBase::set_processor when `processor.ctx` is non-null.
		// Without a manager (the Rust model reaches the manager through
		// the oakrender bridge, absent here) the C++ guard
		// `if (manager())` fails, so this is a no-op and the processor
		// stays empty — `value()` then pushes nothing.
		// `// CPP-PARITY: ociogradingtransformlinear.cpp`
		// generate_processor.
	}

	/// OCIO config change hook (C++ `config_changed()` override):
	/// regenerates the processor.
	fn config_changed(&mut self, core: &mut NodeCore) {
		self.generate_processor(core);
	}
}

impl NodeBehavior for OCIOGradingTransformLinearNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"OCIO Color Grading (Linear)"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.ociogradingtransformlinear"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Simple linear color grading using OpenColorIO."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// contrast -> "Contrast", offset -> "Offset", exposure ->
	/// "Exposure" (plus its stops tooltip), saturation -> "Saturation",
	/// pivot -> "Pivot", clamp enables -> "Enable Black/White Clamp",
	/// clamps -> "Black Clamp"/"White Clamp".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			crate::nodes::ociobase::TEXTURE_INPUT => "Input",
			CONTRAST_INPUT => "Contrast",
			OFFSET_INPUT => "Offset",
			EXPOSURE_INPUT => "Exposure",
			SATURATION_INPUT => "Saturation",
			PIVOT_INPUT => "Pivot",
			CLAMP_BLACK_ENABLE_INPUT => "Enable Black Clamp",
			CLAMP_BLACK_INPUT => "Black Clamp",
			CLAMP_WHITE_ENABLE_INPUT => "Enable White Clamp",
			CLAMP_WHITE_INPUT => "White Clamp",
			_ => id,
		}
	}

	/// Input value changed (C++ `InputValueChangedEvent`): toggling a
	/// clamp-enable input mirrors it into the clamp input's `enabled`
	/// property; a black-clamp change re-constrains the white clamp
	/// minimum; any change regenerates the processor.
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		let _ = element;
		if input == CLAMP_WHITE_ENABLE_INPUT {
			set_input_property(
				core,
				CLAMP_WHITE_INPUT,
				"enabled",
				crate::value::NodeValue::Boolean(
					core.standard_value(CLAMP_WHITE_ENABLE_INPUT, -1)
						.to_double() != 0.0,
				),
			);
		} else if input == CLAMP_BLACK_ENABLE_INPUT {
			set_input_property(
				core,
				CLAMP_BLACK_INPUT,
				"enabled",
				crate::value::NodeValue::Boolean(
					core.standard_value(CLAMP_BLACK_ENABLE_INPUT, -1)
						.to_double() != 0.0,
				),
			);
		} else if input == CLAMP_BLACK_INPUT {
			// Ensure the white clamp is always greater than the black
			// clamp as per ocio::GradingPrimary::validate.
			self.update_clamp_white_minimum(core);
		}

		self.generate_processor(core);
	}

	/// Edge connected (C++ `InputConnectedEvent`): forwards to the base
	/// class and, for the black clamp input, re-constrains the white
	/// clamp minimum.
	fn input_connected(
		&mut self,
		core: &mut NodeCore,
		input: &str,
		element: i32,
		source: crate::id::NodeId,
	) {
		let _ = (element, source);
		// C++ forwards to the base class first; OCIOBaseNode does not
		// override the event, so that half is a no-op here.
		if input == CLAMP_BLACK_INPUT {
			self.update_clamp_white_minimum(core);
		}
	}

	/// Edge disconnected (C++ `InputDisconnectedEvent`): forwards to the
	/// base class and, for the black clamp input, re-constrains the
	/// white clamp minimum.
	fn input_disconnected(
		&mut self,
		core: &mut NodeCore,
		input: &str,
		element: i32,
		source: crate::id::NodeId,
	) {
		let _ = (element, source);
		// See [`NodeBehavior::input_connected`].
		if input == CLAMP_BLACK_INPUT {
			self.update_clamp_white_minimum(core);
		}
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// otherwise pushes a REAL [`ShaderJobPayload`] whose params carry the
	/// input row rewritten into the vec3 form the OCIO-generated GPU
	/// uniforms expect — the C++ job's `values` rewrite, applied to the
	/// uniform names the renderer's grading stub declares:
	/// `ocio_grading_primary_contrast` RGB = channel * master, `offset`
	/// RGB = channel + master, `exposure` RGB = 2^(channel + master).
	/// Disabled clamps are pushed as `GradingPrimary::NoClampBlack()`
	/// (-1.0) / `NoClampWhite()` (2.0), and when both clamps are enabled
	/// the white clamp is raised to black + 0.000001 per frame if
	/// keyframed/connected values violate white > black. The transform
	/// itself is the OCIO shader the renderer splices in — the processor
	/// is generated at render time from the default config, so the
	/// evaluation-time guard (C++ processor gate) is replaced by the
	/// node's own presence check on the input texture.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let _ = self;
		if !matches!(
			inputs.get(crate::nodes::ociobase::TEXTURE_INPUT),
			Some(crate::value::NodeValue::Texture(_))
		) {
			return;
		}

		let mut params = inputs.clone();

		// vec4 (RGBM: x = master) inputs -> the vec3 GPU uniform form.
		let contrast = to_vec4(row_or_standard(core, inputs, CONTRAST_INPUT, time));
		let offset = to_vec4(row_or_standard(core, inputs, OFFSET_INPUT, time));
		let exposure = to_vec4(row_or_standard(core, inputs, EXPOSURE_INPUT, time));
		params.insert(
			CONTRAST_INPUT.to_string(),
			channel_merge(contrast, |c, m| c * m),
		);
		params.insert(
			OFFSET_INPUT.to_string(),
			channel_merge(offset, |c, m| c + m),
		);
		params.insert(
			EXPOSURE_INPUT.to_string(),
			channel_merge(exposure, |c, m| 2f64.powf(c + m)),
		);

		// Scalar uniforms (the IDs are the generated uniform names).
		params.insert(
			SATURATION_INPUT.to_string(),
			row_or_standard(core, inputs, SATURATION_INPUT, time),
		);
		params.insert(
			PIVOT_INPUT.to_string(),
			row_or_standard(core, inputs, PIVOT_INPUT, time),
		);

		// Clamps: enabled -> the value, disabled -> the OCIO sentinels
		// (NoClampBlack -1 / NoClampWhite 2 keep the GPU clamp a no-op).
		let black_enabled = row_or_standard(core, inputs, CLAMP_BLACK_ENABLE_INPUT, time)
			.to_double() != 0.0;
		let white_enabled = row_or_standard(core, inputs, CLAMP_WHITE_ENABLE_INPUT, time)
			.to_double() != 0.0;
		let mut black =
			row_or_standard(core, inputs, CLAMP_BLACK_INPUT, time).to_double();
		let mut white =
			row_or_standard(core, inputs, CLAMP_WHITE_INPUT, time).to_double();
		if black_enabled && white_enabled {
			// ocio::GradingPrimary::validate: white > black (per frame,
			// when the static UI minimum cannot follow animated values).
			white = white.max(black + 0.000001);
		}
		params.insert(
			CLAMP_BLACK_INPUT.to_string(),
			crate::value::NodeValue::Float(if black_enabled { black } else { -1.0 }),
		);
		params.insert(
			CLAMP_WHITE_INPUT.to_string(),
			crate::value::NodeValue::Float(if white_enabled { white } else { 2.0 }),
		);
		params.insert(
			"ocio_grading_primary_localBypass".to_string(),
			crate::value::NodeValue::Boolean(false),
		);

		let job = crate::handle::make_owned(crate::jobs::ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations: 1,
			type_id: self.type_id().to_string(),
			shader_id: "rgb".to_string(),
			effect_input: crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
			params,
			iterative_input: String::new(),
		});
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(job),
			None,
		);
	}

	/// Shader code request (C++ `get_shader_code()`): reads the fragment
	/// shader and replaces every `%1` marker with `request` — the OCIO
	/// auto-generated grading-primary shader text (C++ `GetShaderCode`
	/// receives the stub from the renderer's color context; the Rust
	/// renderer resolves it from the `OCIO_GRADING_STUBS` table before
	/// calling, see [`crate::nodes::ociogradingtransformlog`]'s sibling
	/// wiring).
	fn shader_code(&self, request: &str) -> Option<String> {
		Some(SHADER_FRAG.replace("%1", request))
	}

	/// Added to a graph (C++ base `AddedToGraphEvent`): captures the
	/// project's color manager and runs `config_changed()` via
	/// [`OcioBase::added_to_graph`].
	fn added_to_graph(&mut self, core: &mut NodeCore) {
		self.base.added_to_graph(core);
		self.config_changed(core);
	}

	/// Removed from a graph (C++ base `RemovedFromGraphEvent`): clears
	/// the color manager pointer via [`OcioBase::removed_from_graph`].
	fn removed_from_graph(&mut self, core: &mut NodeCore) {
		self.base.removed_from_graph(core);
	}

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		// The C++ copy constructor copies the embedded OCIO base state;
		// a fresh base with no processor is the safe Rust port (the
		// processor is never populated without the render bridge).
		Some(Box::new(OCIOGradingTransformLinearNode {
			base: OcioBase::new(),
		}))
	}
}

/// Constructor (C++
/// `OCIOGradingTransformLinearNode::OCIOGradingTransformLinearNode()`):
/// builds the base (`tex_in` texture input, effect input, video-effect
/// flag), adds the contrast/offset/exposure/saturation/pivot and
/// clamp-enable/clamp inputs with the defaults, flags and properties
/// documented on the constants, and applies the initial white-clamp
/// minimum constraint.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	// OCIOBaseNode base constructor.
	let mut tex = crate::input::Input::new(
		crate::nodes::ociobase::TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);
	core.effect_input = crate::nodes::ociobase::TEXTURE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	let component_colors = vec![
		(
			"color0".to_string(),
			crate::value::NodeValue::Text("#c0c0c0".into()),
		),
		(
			"color1".to_string(),
			crate::value::NodeValue::Text("#ff0000".into()),
		),
		(
			"color2".to_string(),
			crate::value::NodeValue::Text("#00ff00".into()),
		),
		(
			"color3".to_string(),
			crate::value::NodeValue::Text("#0000ff".into()),
		),
	];

	let mut contrast = crate::input::Input::new(
		CONTRAST_INPUT,
		crate::value::ValueType::Vec4,
		crate::value::NodeValue::Vec4([1.0, 1.0, 1.0, 1.0]),
	);
	contrast.properties = vec![
		(
			"min".to_string(),
			crate::value::NodeValue::Vec4([0.01, 0.01, 0.01, 0.01]),
		),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	contrast.properties.extend(component_colors.clone());
	core.add_input(contrast);

	let mut offset = crate::input::Input::new(
		OFFSET_INPUT,
		crate::value::ValueType::Vec4,
		crate::value::NodeValue::Vec4([0.0, 0.0, 0.0, 0.0]),
	);
	offset.properties = vec![("base".to_string(), crate::value::NodeValue::Float(0.01))];
	offset.properties.extend(component_colors.clone());
	core.add_input(offset);

	let mut exposure = crate::input::Input::new(
		EXPOSURE_INPUT,
		crate::value::ValueType::Vec4,
		crate::value::NodeValue::Vec4([0.0, 0.0, 0.0, 0.0]),
	);
	exposure.properties = vec![("base".to_string(), crate::value::NodeValue::Float(0.01))];
	exposure.properties.extend(component_colors);
	core.add_input(exposure);

	let mut saturation = crate::input::Input::new(
		SATURATION_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	saturation.properties = vec![
		(
			"view".to_string(),
			crate::value::NodeValue::Text("percentage".into()),
		),
		("min".to_string(), crate::value::NodeValue::Float(0.0)),
	];
	core.add_input(saturation);

	let mut pivot = crate::input::Input::new(
		PIVOT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.18),
	);
	pivot.properties = vec![("base".to_string(), crate::value::NodeValue::Float(0.01))];
	core.add_input(pivot);

	core.add_input(crate::input::Input::new(
		CLAMP_BLACK_ENABLE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	let mut clamp_black = crate::input::Input::new(
		CLAMP_BLACK_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	clamp_black.properties = vec![
		(
			"enabled".to_string(),
			crate::value::NodeValue::Boolean(
				core.standard_value(CLAMP_BLACK_ENABLE_INPUT, -1)
					.to_double() != 0.0,
			),
		),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	core.add_input(clamp_black);

	core.add_input(crate::input::Input::new(
		CLAMP_WHITE_ENABLE_INPUT,
		crate::value::ValueType::Boolean,
		crate::value::NodeValue::Boolean(false),
	));
	let mut clamp_white = crate::input::Input::new(
		CLAMP_WHITE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(1.0),
	);
	clamp_white.properties = vec![
		(
			"enabled".to_string(),
			crate::value::NodeValue::Boolean(
				core.standard_value(CLAMP_WHITE_ENABLE_INPUT, -1)
					.to_double() != 0.0,
			),
		),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	core.add_input(clamp_white);

	// Constrain the white clamp minimum to just above the (static) black
	// clamp as per ocio::GradingPrimary::validate. When the black clamp
	// is keyframed or connected, Value() enforces the invariant per frame
	// instead.
	let mut node = OCIOGradingTransformLinearNode {
		base: OcioBase::new(),
	};
	node.update_clamp_white_minimum(&mut core);

	(core, Box::new(node))
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.ociogradingtransformlinear`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.ociogradingtransformlinear",
		name: "OCIO Color Grading (Linear)",
		categories: &[Category::Color],
		create,
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::{Interpolation, Keyframe};
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn node() -> OCIOGradingTransformLinearNode {
		OCIOGradingTransformLinearNode {
			base: OcioBase::new(),
		}
	}

	/// Property value lookup helper for tests.
	fn property(core: &NodeCore, input: &str, key: &str) -> Option<NodeValue> {
		core.get_input(input).and_then(|i| {
			i.properties
				.iter()
				.find(|(k, _)| k == key)
				.map(|(_, v)| v.clone())
		})
	}

	#[test]
	fn input_names() {
		let n = node();
		assert_eq!(n.input_name(crate::nodes::ociobase::TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(CONTRAST_INPUT), "Contrast");
		assert_eq!(n.input_name(OFFSET_INPUT), "Offset");
		assert_eq!(n.input_name(EXPOSURE_INPUT), "Exposure");
		assert_eq!(n.input_name(SATURATION_INPUT), "Saturation");
		assert_eq!(n.input_name(PIVOT_INPUT), "Pivot");
		assert_eq!(n.input_name(CLAMP_BLACK_ENABLE_INPUT), "Enable Black Clamp");
		assert_eq!(n.input_name(CLAMP_BLACK_INPUT), "Black Clamp");
		assert_eq!(n.input_name(CLAMP_WHITE_ENABLE_INPUT), "Enable White Clamp");
		assert_eq!(n.input_name(CLAMP_WHITE_INPUT), "White Clamp");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.ociogradingtransformlinear"
		);
		assert_ne!(
			core.get_input(crate::nodes::ociobase::TEXTURE_INPUT)
				.unwrap()
				.flags & crate::input::flags::NOT_KEYFRAMABLE,
			0
		);
		assert_eq!(
			core.get_input(CONTRAST_INPUT).unwrap().default,
			NodeValue::Vec4([1.0, 1.0, 1.0, 1.0])
		);
		assert_eq!(
			core.get_input(OFFSET_INPUT).unwrap().default,
			NodeValue::Vec4([0.0; 4])
		);
		assert_eq!(
			core.get_input(EXPOSURE_INPUT).unwrap().default,
			NodeValue::Vec4([0.0; 4])
		);
		assert_eq!(
			core.get_input(SATURATION_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		assert_eq!(
			core.get_input(PIVOT_INPUT).unwrap().default,
			NodeValue::Float(0.18)
		);
		assert_eq!(
			core.get_input(CLAMP_BLACK_ENABLE_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(
			core.get_input(CLAMP_BLACK_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		assert_eq!(
			core.get_input(CLAMP_WHITE_ENABLE_INPUT).unwrap().default,
			NodeValue::Boolean(false)
		);
		assert_eq!(
			core.get_input(CLAMP_WHITE_INPUT).unwrap().default,
			NodeValue::Float(1.0)
		);
		// Component colors on every vec4 grading input.
		for id in [CONTRAST_INPUT, OFFSET_INPUT, EXPOSURE_INPUT] {
			let input = core.get_input(id).unwrap();
			assert!(input
				.properties
				.iter()
				.any(|(k, v)| k == "color0" && *v == NodeValue::Text("#c0c0c0".into())));
			assert!(input
				.properties
				.iter()
				.any(|(k, v)| k == "color3" && *v == NodeValue::Text("#0000ff".into())));
		}
		// Initial white-clamp minimum constraint: black (0.0) + 0.000001.
		assert_eq!(
			property(&core, CLAMP_WHITE_INPUT, "min"),
			Some(NodeValue::Float(0.000001))
		);
		// Clamp enabled properties mirror the enable inputs (false at
		// construction).
		assert_eq!(
			property(&core, CLAMP_BLACK_INPUT, "enabled"),
			Some(NodeValue::Boolean(false))
		);
		assert_eq!(
			property(&core, CLAMP_WHITE_INPUT, "enabled"),
			Some(NodeValue::Boolean(false))
		);
		assert_eq!(core.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn update_clamp_white_minimum_tracks_black_clamp() {
		let mut core = NodeCore::new();
		// Provide the clamp inputs so the property write lands.
		core.add_input(crate::input::Input::new(
			CLAMP_WHITE_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(1.0),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_BLACK_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(0.0),
		));
		core.set_standard_value(CLAMP_BLACK_INPUT, -1, NodeValue::Float(0.5));
		let mut n = node();
		n.update_clamp_white_minimum(&mut core);
		assert_eq!(
			property(&core, CLAMP_WHITE_INPUT, "min"),
			Some(NodeValue::Float(0.500001))
		);
	}

	#[test]
	fn update_clamp_white_minimum_skips_keyframed_black_clamp() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			CLAMP_WHITE_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(1.0),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_BLACK_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(0.0),
		));
		core.keyframe_track_mut(CLAMP_BLACK_INPUT, -1)
			.set_key(Keyframe {
				time: Rational::new(0, 1),
				value: NodeValue::Float(0.5),
				interpolation: Interpolation::Hold,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		let mut n = node();
		n.update_clamp_white_minimum(&mut core);
		// Keyframed: the static minimum is not updated.
		assert_eq!(property(&core, CLAMP_WHITE_INPUT, "min"), None);
	}

	#[test]
	fn input_value_changed_mirrors_enable_toggles() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			CLAMP_BLACK_ENABLE_INPUT,
			crate::value::ValueType::Boolean,
			crate::value::NodeValue::Boolean(false),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_BLACK_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(0.0),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_WHITE_ENABLE_INPUT,
			crate::value::ValueType::Boolean,
			crate::value::NodeValue::Boolean(false),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_WHITE_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(1.0),
		));
		core.set_standard_value(CLAMP_BLACK_ENABLE_INPUT, -1, NodeValue::Boolean(true));
		let mut n = node();
		n.input_value_changed(&mut core, CLAMP_BLACK_ENABLE_INPUT, 0);
		assert_eq!(
			property(&core, CLAMP_BLACK_INPUT, "enabled"),
			Some(NodeValue::Boolean(true))
		);
		// White enable mirrors too.
		core.set_standard_value(CLAMP_WHITE_ENABLE_INPUT, -1, NodeValue::Boolean(true));
		n.input_value_changed(&mut core, CLAMP_WHITE_ENABLE_INPUT, 0);
		assert_eq!(
			property(&core, CLAMP_WHITE_INPUT, "enabled"),
			Some(NodeValue::Boolean(true))
		);
	}

	#[test]
	fn input_value_changed_black_clamp_reconstrains_white_minimum() {
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			CLAMP_BLACK_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(0.0),
		));
		core.add_input(crate::input::Input::new(
			CLAMP_WHITE_INPUT,
			crate::value::ValueType::Float,
			crate::value::NodeValue::Float(1.0),
		));
		core.set_standard_value(CLAMP_BLACK_INPUT, -1, NodeValue::Float(0.2));
		let mut n = node();
		n.input_value_changed(&mut core, CLAMP_BLACK_INPUT, 0);
		assert_eq!(
			property(&core, CLAMP_WHITE_INPUT, "min"),
			Some(NodeValue::Float(0.200001))
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
	fn value_texture_pushes_real_grading_job() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(
				crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
			(CONTRAST_INPUT.to_string(), NodeValue::Vec4([0.5, 0.25, 0.5, 0.75])),
			(OFFSET_INPUT.to_string(), NodeValue::Vec4([0.25, -0.125, 0.0, 0.125])),
			(EXPOSURE_INPUT.to_string(), NodeValue::Vec4([1.0, 0.0, 0.0, 0.0])),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let tex = table.get(ValueType::Texture).expect("texture pushed");
		let NodeValue::Texture(handle) = tex else {
			panic!("pushed value is not a texture handle");
		};
		let job = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle)
		}
		.expect("real shader job");
		assert_eq!(job.shader_id, "rgb");
		assert_eq!(
			job.type_id,
			"org.olivevideoeditor.Olive.ociogradingtransformlinear"
		);
		assert_eq!(job.effect_input, crate::nodes::ociobase::TEXTURE_INPUT);
		// vec4 (RGBM: x = master) rewrites: contrast RGB = c*m, offset
		// RGB = c+m, exposure RGB = 2^(c+m).
		assert_eq!(
			job.params.get(CONTRAST_INPUT),
			Some(&NodeValue::Vec3([0.125, 0.25, 0.375]))
		);
		assert_eq!(
			job.params.get(OFFSET_INPUT),
			Some(&NodeValue::Vec3([0.125, 0.25, 0.375]))
		);
		assert_eq!(
			job.params.get(EXPOSURE_INPUT),
			Some(&NodeValue::Vec3([2.0, 2.0, 2.0]))
		);
		// Scalar defaults from the node's standard values.
		assert_eq!(
			job.params.get(SATURATION_INPUT),
			Some(&NodeValue::Float(1.0))
		);
		assert_eq!(job.params.get(PIVOT_INPUT), Some(&NodeValue::Float(0.18)));
		// Clamps disabled -> OCIO sentinels (NoClampBlack/NoClampWhite).
		assert_eq!(
			job.params.get(CLAMP_BLACK_INPUT),
			Some(&NodeValue::Float(-1.0))
		);
		assert_eq!(
			job.params.get(CLAMP_WHITE_INPUT),
			Some(&NodeValue::Float(2.0))
		);
		assert_eq!(
			job.params.get("ocio_grading_primary_localBypass"),
			Some(&NodeValue::Boolean(false))
		);
	}

	#[test]
	fn value_enables_clamp_when_flagged_of_white_and_black() {
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([
			(
				crate::nodes::ociobase::TEXTURE_INPUT.to_string(),
				NodeValue::Texture(crate::handle::CHandle::null()),
			),
			(
				CLAMP_BLACK_ENABLE_INPUT.to_string(),
				NodeValue::Boolean(true),
			),
			(
				CLAMP_WHITE_ENABLE_INPUT.to_string(),
				NodeValue::Boolean(true),
			),
			(CLAMP_BLACK_INPUT.to_string(), NodeValue::Float(0.5)),
			(CLAMP_WHITE_INPUT.to_string(), NodeValue::Float(0.4)),
		]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let tex = table.get(ValueType::Texture).expect("texture pushed");
		let NodeValue::Texture(handle) = tex else {
			panic!("pushed value is not a texture handle");
		};
		let job = unsafe {
			crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle)
		}
		.expect("real shader job");
		// White <= black is raised to black + 0.000001.
		assert_eq!(
			job.params.get(CLAMP_BLACK_INPUT),
			Some(&NodeValue::Float(0.5))
		);
		assert_eq!(
			job.params.get(CLAMP_WHITE_INPUT),
			Some(&NodeValue::Float(0.500001))
		);
	}

	#[test]
	fn shader_code_splices_stub_marker() {
		let n = node();
		let stub = "vec4 ove_grading_primary(vec4 c) { return c; }";
		let code = n.shader_code(stub).unwrap();
		assert!(!code.contains("%1"));
		assert!(code.contains("ove_grading_primary(col)"));
		assert!(code.contains(stub));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "OCIO Color Grading (Linear)");
	}
}
