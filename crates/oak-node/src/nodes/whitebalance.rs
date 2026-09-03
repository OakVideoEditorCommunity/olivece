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

//! White balance node (C++
//! `src/node/src/color/whitebalance/whitebalance.{h,cpp}`,
//! `olive::WhiteBalanceNode`).
//!
//! White balance correction by color temperature and tint: converts a
//! scene illuminant temperature (Kelvin) into per-channel RGB gains
//! using the Tanner Helland blackbody approximation, normalized so the
//! green channel is preserved (no exposure shift); tint shifts along
//! the green-magenta axis.

use crate::factory::NodeMeta;
use crate::jobs::ShaderJobPayload;
use crate::node::{Category, NodeBehavior, NodeCore};

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Temperature input id (C++ `k_temperature_input`). Type: float;
/// default `6500.0` (Kelvin); properties: `min = 1000.0`, `max =
/// 40000.0`, `view = normal slider`.
pub const TEMPERATURE_INPUT: &str = "temperature_in";

/// Tint input id (C++ `k_tint_input`). Type: float; default `0.0`;
/// properties: `min = -1.0`, `max = 1.0`, `base = 0.01`.
pub const TINT_INPUT: &str = "tint_in";

/// Gain uniform id (C++ `k_gain_input`). Not a declared node input —
/// the C++ never calls `add_input` for it; it is the shader uniform
/// name fed per frame in `value()` with the RGB gain computed by
/// [`WhiteBalanceNode::gain_for_temperature`]. Type: vec3.
pub const GAIN_INPUT: &str = "wb_gain_in";

/// White balance node. Adjusts white balance by color temperature and
/// tint. The C++ class has no own private members, so this is a
/// unit-like struct (caches/inputs live in `NodeCore`).
pub struct WhiteBalanceNode;

/// Fragment shader (C++ `get_shader_code` loads the
/// `:/shaders/whitebalance.frag` resource). Text copied verbatim from
/// `engine/shaders/whitebalance.frag`.
const SHADER_FRAG: &str = r#"uniform sampler2D tex_in;

uniform vec3 wb_gain_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void)
{
    vec4 source = texture(tex_in, ove_texcoord);

    // Deliberately not clamped: white balance must also work on HDR/linear
    // footage with values above 1.0
    frag_color = vec4(source.rgb * wb_gain_in, source.a);
}
"#;

impl WhiteBalanceNode {
	/// Fragment shader for any request (C++ `get_shader_code()` ignores
	/// the request id and always returns this shader).
	fn shader_frag() -> &'static str {
		SHADER_FRAG
	}

	/// RGB gains for a given illuminant temperature and tint (C++
	/// `get_gain_for_temperature()`, extracted for testability). Kelvin
	/// is clamped to [1000, 40000]; the Tanner Helland blackbody
	/// approximation gives 0-255 per channel (red: 255 below 6600K, else
	/// `329.698727446 * (t - 60)^-0.1332047592`; green: logarithmic
	/// below 6600K, power-law above; blue: 255 above 6600K, 0 below
	/// 1900K, logarithmic between). The result is normalized so the
	/// green channel gain is 1.0 at tint 0, then tint scales the green
	/// channel by `clamp(1.0 + tint, 0.0, 2.0)` (green-magenta axis).
	pub fn gain_for_temperature(kelvin: f64, tint: f64) -> [f64; 3] {
		let kelvin = kelvin.clamp(1000.0, 40000.0);
		let t = kelvin / 100.0;

		let red = if t <= 66.0 {
			255.0
		} else {
			329.698727446 * (t - 60.0).powf(-0.1332047592)
		};

		let green = if t <= 66.0 {
			99.4708025861 * t.ln() - 161.1195681661
		} else {
			288.1221695283 * (t - 60.0).powf(-0.0755148492)
		};

		let blue = if t >= 66.0 {
			255.0
		} else if t <= 19.0 {
			0.0
		} else {
			138.5177312231 * (t - 10.0).ln() - 305.0447927307
		};

		// Normalize to the green channel so temperature shifts do not change
		// exposure, then let tint move along the green-magenta axis.
		let tint_gain = (1.0 + tint).clamp(0.0, 2.0);

		[red / green, green / green * tint_gain, blue / green]
	}
}

impl NodeBehavior for WhiteBalanceNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"White Balance"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.whitebalance"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Color]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Adjust white balance by color temperature and tint."
	}

	/// Localized input names (C++ `retranslate()`): `tex_in` -> "Input",
	/// `temperature_in` -> "Temperature (K)", `tint_in` -> "Tint".
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXTURE_INPUT => "Input",
			TEMPERATURE_INPUT => "Temperature (K)",
			TINT_INPUT => "Tint",
			_ => id,
		}
	}

	/// Shader code request (C++ `get_shader_code()`): the request id is
	/// ignored; always returns [`SHADER_FRAG`].
	fn shader_code(&self, _request: &str) -> Option<String> {
		Some(SHADER_FRAG.to_string())
	}

	/// Evaluate outputs (C++ `value()`): no texture -> push nothing;
	/// otherwise builds a `ShaderJob` from the whole input row, inserts
	/// `wb_gain_in` as a vec3 computed by
	/// [`Self::gain_for_temperature`] from the temperature and tint
	/// inputs, and pushes the texture as that job.
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		match inputs.get(TEXTURE_INPUT) {
			Some(crate::value::NodeValue::Texture(_)) => {}
			_ => return,
		}

		let temperature = match inputs.get(TEMPERATURE_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(TEMPERATURE_INPUT, -1, time).to_double(),
		};
		let tint = match inputs.get(TINT_INPUT) {
			Some(v) => v.to_double(),
			None => core.value_at_time(TINT_INPUT, -1, time).to_double(),
		};
		let gain = Self::gain_for_temperature(temperature, tint);

		// `// CPP-PARITY: whitebalance.cpp` `value()` — the C++ builds a
		// ShaderJob from the whole input row, inserts `wb_gain_in` as the
		// per-frame vec3 gain, and pushes `tex->to_job(job)`. The job is
		// boxed here as a [`ShaderJobPayload`] that the renderer's resolve
		// hook executes and replaces with the result texture; the params
		// row carries the computed gain under the shader uniform name.
		let mut params = inputs.clone();
		params.insert(GAIN_INPUT.to_string(), crate::value::NodeValue::Vec3(gain));
		table.push(
			crate::value::ValueType::Texture,
			crate::value::NodeValue::Texture(crate::handle::make_owned(ShaderJobPayload {
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

	/// Deep copy (C++ `copy()` via `NODE_DEFAULT_FUNCTIONS`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(WhiteBalanceNode))
	}
}

/// Constructor (C++ `WhiteBalanceNode::WhiteBalanceNode()`): adds
/// `tex_in` (texture, effect input), `temperature_in` and `tint_in`
/// with the defaults and properties documented on the constants, and
/// sets the video-effect flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(tex);

	let mut temperature = crate::input::Input::new(
		TEMPERATURE_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(6500.0),
	);
	temperature.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(1000.0)),
		("max".to_string(), crate::value::NodeValue::Float(40000.0)),
		(
			"view".to_string(),
			crate::value::NodeValue::Text("normal".into()),
		),
	];
	core.add_input(temperature);

	let mut tint = crate::input::Input::new(
		TINT_INPUT,
		crate::value::ValueType::Float,
		crate::value::NodeValue::Float(0.0),
	);
	tint.properties = vec![
		("min".to_string(), crate::value::NodeValue::Float(-1.0)),
		("max".to_string(), crate::value::NodeValue::Float(1.0)),
		("base".to_string(), crate::value::NodeValue::Float(0.01)),
	];
	core.add_input(tint);

	core.effect_input = TEXTURE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;

	(core, Box::new(WhiteBalanceNode))
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.whitebalance`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.whitebalance",
		name: "White Balance",
		categories: &[Category::Color],
		create,
	});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    #[test]
	fn input_names() {
		let n = WhiteBalanceNode;
		assert_eq!(n.input_name(TEXTURE_INPUT), "Input");
		assert_eq!(n.input_name(TEMPERATURE_INPUT), "Temperature (K)");
		assert_eq!(n.input_name(TINT_INPUT), "Tint");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs_flags_and_properties() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.whitebalance"
		);
		let tex = core.get_input(TEXTURE_INPUT).unwrap();
		assert_ne!(tex.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		assert_eq!(
			core.get_input(TEMPERATURE_INPUT).unwrap().default,
			NodeValue::Float(6500.0)
		);
		assert_eq!(
			core.get_input(TINT_INPUT).unwrap().default,
			NodeValue::Float(0.0)
		);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn gain_matches_documented_blackbody_formula() {
		let (kelvin, tint): (f64, f64) = (5600.0, 0.25);
		let t = kelvin / 100.0;
		let red = 255.0; // t = 56 <= 66
		let green = 99.4708025861 * t.ln() - 161.1195681661;
		let blue = 138.5177312231 * (t - 10.0).ln() - 305.0447927307;
		let tint_gain = (1.0 + tint).clamp(0.0, 2.0);
		let expected = [red / green, green / green * tint_gain, blue / green];
		let got = WhiteBalanceNode::gain_for_temperature(kelvin, tint);
		for (g, e) in got.iter().zip(expected.iter()) {
			assert!((g - e).abs() < 1e-9, "got {}, expected {}", g, e);
		}
	}

	#[test]
	fn gain_normalizes_green_to_one() {
		// The green channel gain is always 1.0 at tint 0, so temperature
		// shifts never change exposure.
		for kelvin in [1000.0, 1900.0, 5600.0, 6500.0, 10000.0, 40000.0] {
			let gain = WhiteBalanceNode::gain_for_temperature(kelvin, 0.0);
			assert_eq!(gain[1], 1.0, "kelvin {}", kelvin);
		}
	}

	#[test]
	fn gain_blue_black_below_1900k() {
		// t <= 19 => blue channel gain is 0.
		let gain = WhiteBalanceNode::gain_for_temperature(1000.0, 0.0);
		assert_eq!(gain[2], 0.0);
	}

	#[test]
	fn gain_clamps_kelvin_range() {
		// Below 1000 and above 40000 Kelvin are clamped.
		let low = WhiteBalanceNode::gain_for_temperature(500.0, 0.0);
		let at_min = WhiteBalanceNode::gain_for_temperature(1000.0, 0.0);
		assert_eq!(low, at_min);
		let high = WhiteBalanceNode::gain_for_temperature(50000.0, 0.0);
		let at_max = WhiteBalanceNode::gain_for_temperature(40000.0, 0.0);
		assert_eq!(high, at_max);
	}

	#[test]
	fn gain_tint_scales_green_axis_clamped() {
		assert_eq!(WhiteBalanceNode::gain_for_temperature(6500.0, 0.0)[1], 1.0);
		assert_eq!(WhiteBalanceNode::gain_for_temperature(6500.0, 1.0)[1], 2.0);
		assert_eq!(WhiteBalanceNode::gain_for_temperature(6500.0, -1.0)[1], 0.0);
		assert_eq!(WhiteBalanceNode::gain_for_temperature(6500.0, 10.0)[1], 2.0);
		assert_eq!(
			WhiteBalanceNode::gain_for_temperature(6500.0, -10.0)[1],
			0.0
		);
	}

	#[test]
	fn shader_code_returns_whitebalance_frag() {
		let code = WhiteBalanceNode.shader_code("anything").unwrap();
		assert!(code.contains("source.rgb * wb_gain_in"));
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
		let inputs = crate::value::NodeValueRow::from([(
			TEXTURE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		)]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		let NodeValue::Texture(handle) = table.get(ValueType::Texture).unwrap() else {
			panic!("expected a texture-typed value");
		};
		let payload =
			unsafe { crate::handle::get_checked::<crate::jobs::ShaderJobPayload>(handle) }
				.expect("payload boxed behind the handle");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.whitebalance");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		// The computed per-frame gain is injected under the shader uniform
		// name, overriding any row value (C++ `job.Insert(k_gain_input, ...)`).
		let gain = payload.params.get(GAIN_INPUT).unwrap();
		let NodeValue::Vec3(g) = gain else {
			panic!("expected a vec3 gain");
		};
		assert_eq!(g, &WhiteBalanceNode::gain_for_temperature(6500.0, 0.0));
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "White Balance");
	}
}
