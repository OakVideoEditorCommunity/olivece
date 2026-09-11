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

//! Rich text generator v3 (C++
//! `src/node/src/generator/text/textv3.{h,cpp}`,
//! `olive::TextGeneratorV3`, derives from `ShapeNodeBase`).
//!
//! FONT/RASTER BACKEND DEPENDENCY — DELIBERATELY UNDECIDED:
//! The C++ does NOT link any font/raster library directly (no freetype,
//! stb, harfbuzz, etc. anywhere in the tree). Text layout/rasterization
//! was historically Qt's rich text stack (`QTextDocument` fed through
//! Olive's `Html::html_to_doc()` + `QPainter` directly over an
//! RGBA8888-premultiplied buffer); it now runs behind the
//! facade-installed hooks in [`super::textbackend`]. No Rust font crate
//! is chosen here on purpose.
//!
//! REDESIGN (Rust-only, no C++ counterpart): on top of the ported v3
//! node this adds a structured, user-facing input set — `plain_text_in`,
//! `font_family_in`, `font_size_in` and the outline/glow enable/color/
//! size inputs — while the legacy `text_in` HTML input stays as the
//! serialized compatibility carrier (now hidden) and as the migration
//! source for pre-redesign projects ([`migrate_legacy_html`]). The
//! outline/glow inputs drive a GPU post-process chain in [`value`]: the
//! rasterized text coverage is dilated (square structuring element,
//! `outline_width_in` clamped to 0..7) and colorized into a stroke,
//! and/or blurred once per axis (two passes, `glow_radius_in` clamped
//! to 0..64) and colorized into a glow; the results are alpha-over'd
//! beneath the text. With both enabled the outline runs first and the
//! glow samples the stroke. With both disabled (or no render backend
//! installed) the node keeps the pre-redesign deferred null-job
//! behavior.

use crate::factory::NodeMeta;
use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};
use oak_core::Rational;

use super::textbackend::{TextLayoutMode, TextLayoutRequest, TextLayoutSize, TextRenderTransform};

/// Text input id (C++ `k_text_input`). Type: text; default
/// `LEGACY_DEFAULT_TEXT_HTML`; properties: `vieweronly = true`; flags:
/// hidden (REDESIGN: carried internally and kept for the serialized
/// compatibility of pre-redesign projects; the user-facing text is
/// [`PLAIN_TEXT_INPUT`]).
pub const TEXT_INPUT: &str = "text_in";

/// Vertical alignment input id (C++ `k_vertical_alignment_input`).
/// Type: combo; no default; flags: hidden | static; combo strings:
/// "Top", "Middle", "Bottom".
pub const VERTICAL_ALIGNMENT_INPUT: &str = "valign_in";

/// Args enable toggle input id (C++ `k_use_args_input`). Type: boolean;
/// default `true`; flags: hidden | static.
pub const USE_ARGS_INPUT: &str = "use_args_in";

/// Format arguments array input id (C++ `k_args_input`). Type: text;
/// flags: array; properties: `arraystart = 1`.
pub const ARGS_INPUT: &str = "args_in";

/// Plain text input id (REDESIGN addition, no C++ counterpart). Type:
/// text; default [`DEFAULT_PLAIN_TEXT`]. The editable user-facing text
/// and — when a text layout backend is installed — the text the
/// generator lays out; the legacy (hidden) [`TEXT_INPUT`] HTML stays as
/// the compatibility carrier.
pub const PLAIN_TEXT_INPUT: &str = "plain_text_in";

/// Font family input id (REDESIGN addition, no C++ counterpart). Type:
/// str-combo; default empty (the backend's default font). The option
/// list is injected by the backend layer (a `combo_option` property),
/// so this node has no [`TextGeneratorV3::input_combo_strings`] entry
/// for it; free-form entry is allowed.
pub const FONT_FAMILY_INPUT: &str = "font_family_in";

/// Font size input id (REDESIGN addition, no C++ counterpart). Type:
/// float; default `72.0`; properties: `min = 1.0`.
pub const FONT_SIZE_INPUT: &str = "font_size_in";

/// Outline enable toggle input id (REDESIGN addition, no C++
/// counterpart). Type: boolean; default `false`.
pub const OUTLINE_ENABLED_INPUT: &str = "outline_enabled_in";

/// Outline color input id (REDESIGN addition, no C++ counterpart).
/// Type: color; default opaque black.
pub const OUTLINE_COLOR_INPUT: &str = "outline_color_in";

/// Outline width input id (REDESIGN addition, no C++ counterpart).
/// Type: float; default `2.0`; properties: `min = 0.0`.
pub const OUTLINE_WIDTH_INPUT: &str = "outline_width_in";

/// Glow enable toggle input id (REDESIGN addition, no C++
/// counterpart). Type: boolean; default `false`.
pub const GLOW_ENABLED_INPUT: &str = "glow_enabled_in";

/// Glow color input id (REDESIGN addition, no C++ counterpart). Type:
/// color; default opaque yellow.
pub const GLOW_COLOR_INPUT: &str = "glow_color_in";

/// Glow radius input id (REDESIGN addition, no C++ counterpart). Type:
/// float; default `8.0`; properties: `min = 0.0`.
pub const GLOW_RADIUS_INPUT: &str = "glow_radius_in";

/// Font color input id (REDESIGN addition, no C++ counterpart). Type:
/// color; default opaque white (the backend rasterizes in white, so the
/// default leaves the raster unchanged). The glyphs are tinted with it
/// during rasterization, premultiplied — the same treatment the outline
/// and glow colorize passes give their own layers.
pub const COLOR_INPUT: &str = "color_in";

/// Default of the legacy [`TEXT_INPUT`] HTML payload (the C++
/// `k_text_input` default, verbatim). Used by [`create`] and by
/// [`migrate_legacy_html`] to recognize an untouched legacy value.
pub const LEGACY_DEFAULT_TEXT_HTML: &str =
	"<p style='font-size: 72pt; color: white;'>Sample Text</p>";

/// Default of [`PLAIN_TEXT_INPUT`] (REDESIGN addition): the localized
/// placeholder text new text nodes start with.
pub const DEFAULT_PLAIN_TEXT: &str = "文本";

/// Vertical alignment (C++ `TextGeneratorV3::VerticalAlignment`, values
/// `k_v_align_top = 0`, `k_v_align_middle = 1`, `k_v_align_bottom = 2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerticalAlignment {
	/// Align to the top of the shape rect (C++ `k_v_align_top`).
	Top,
	/// Vertically center in the shape rect (C++ `k_v_align_middle`).
	Middle,
	/// Align to the bottom of the shape rect (C++ `k_v_align_bottom`).
	Bottom,
}

impl VerticalAlignment {
	/// From a combo index (C++ `static_cast<VerticalAlignment>`); unknown
	/// values map to [`VerticalAlignment::Top`] like the C++ cast's
	/// callers assume.
	fn from_int(v: i32) -> VerticalAlignment {
		match v {
			1 => VerticalAlignment::Middle,
			2 => VerticalAlignment::Bottom,
			_ => VerticalAlignment::Top,
		}
	}
}

/// The C++ `k_input_flag_static` mask: not-connectable +
/// not-keyframable.
const STATIC_FLAGS: u32 =
	crate::input::flags::NOT_CONNECTABLE | crate::input::flags::NOT_KEYFRAMABLE;

/// Rich text generator v3 (the current "Text" node). Inherits
/// position/size/color inputs and the polygon gizmo from the shape base
/// (C++ `ShapeNodeBase`, modelled in [`super::shapenodebase`]).
///
/// The C++ also owns a `TextGizmo *text_gizmo_` — a GUI-layer viewport
/// gizmo with no Rust equivalent in this crate; it is omitted here and
/// will be re-attached by the facade/gizmo wave (gizmos themselves live
/// in [`NodeCore::gizmos`]).
pub struct TextGeneratorV3 {
	/// Suppresses re-emitting the vertical alignment to the gizmo
	/// while it is being driven by the gizmo (C++ `dont_emit_valign_`).
	dont_emit_valign: bool,
}

/// `Variant::to_string()` for the text inputs (Text and string-combo
/// payloads — the latter for [`FONT_FAMILY_INPUT`] — with a numeric
/// fallback for mis-typed connections).
fn to_text(v: &NodeValue) -> String {
	match v {
		NodeValue::Text(s) | NodeValue::StrCombo(s) => s.clone(),
		other => other.to_double().to_string(),
	}
}

/// `Variant::to_bool()` for the args toggle.
fn to_bool(v: &NodeValue) -> bool {
	match v {
		NodeValue::Boolean(b) => *b,
		other => other.to_double() != 0.0,
	}
}

/// `Variant::to_vec2()` for the inherited position/size inputs.
fn to_vec2(v: &NodeValue) -> [f64; 2] {
	match v {
		NodeValue::Vec2(a) => *a,
		other => [other.to_double(), 0.0],
	}
}

/// Largest rasterization dimension accepted from `size_in`. Both the
/// CPU staging buffer and the intermediate GPU textures are `w * h * 4`
/// bytes, so an absurd shape size is clamped rather than allocated.
const MAX_RASTER_SIZE: i32 = 8192;

/// Shader id of the outline dilation pass: a square dilation (the
/// algorithm of [`super::dilate`], with the radius read from
/// [`OUTLINE_WIDTH_INPUT`]).
pub const OUTLINE_DILATE_SHADER_ID: &str = "outline_dilate";

/// Shader id of the outline colorize pass: multiplies the dilated
/// coverage by [`OUTLINE_COLOR_INPUT`].
pub const OUTLINE_COLORIZE_SHADER_ID: &str = "outline_colorize";

/// Shader id of the glow blur pass. The job runs one iteration per axis
/// (horizontal, then vertical).
pub const GLOW_BLUR_SHADER_ID: &str = "glow_blur";

/// Shader id of the glow colorize pass: multiplies the blurred coverage
/// by [`GLOW_COLOR_INPUT`].
pub const GLOW_COLORIZE_SHADER_ID: &str = "glow_colorize";

/// Effect-texture input id shared by the four post-process shaders: the
/// coverage texture produced by the preceding pass.
const POST_TEXTURE_INPUT: &str = "tex_in";

/// Resolution input id. The raster resolution is inserted explicitly so
/// the evaluation pass cannot pre-fill it with the sequence resolution.
const RESOLUTION_INPUT: &str = "resolution_in";

/// Fragment shader of the outline dilation pass: the square
/// `(2r+1)^2` max filter of [`super::dilate`], with the radius taken
/// from `outline_width_in` (rounded and clamped to `0..=7`) instead of
/// the dilate node's own input and the rest copied verbatim.
const OUTLINE_DILATE_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float outline_width_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  int radius = int(clamp(outline_width_in, 0.0, 7.0) + 0.5);
  vec2 texel = vec2(1.0) / resolution_in;

  vec4 acc = texture(tex_in, ove_texcoord);
  for (int dy = -radius; dy <= radius; ++dy) {
    for (int dx = -radius; dx <= radius; ++dx) {
      vec2 uv = ove_texcoord + vec2(float(dx), float(dy)) * texel;
      acc = max(acc, texture(tex_in, uv));
    }
  }

  frag_color = acc;
}
"#;

/// Fragment shader of the outline colorize pass: tint the coverage with
/// [`OUTLINE_COLOR_INPUT`] and emit it premultiplied, so it composites
/// as `color * alpha` over whatever is beneath it.
const OUTLINE_COLORIZE_FRAG: &str = r#"uniform sampler2D tex_in;
uniform vec4 outline_color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 coverage = texture(tex_in, ove_texcoord);
  float alpha = coverage.a * outline_color_in.a;

  frag_color = vec4(outline_color_in.rgb * alpha, alpha);
}
"#;

/// Fragment shader of the glow blur pass: a box blur over the axis
/// selected by `ove_iteration` (0 = horizontal, 1 = vertical), taps one
/// texel apart and averaged over `2r + 1`. `glow_radius_in` is rounded
/// and clamped to `0..=64`; a sub-pixel radius passes the texture
/// through.
const GLOW_BLUR_FRAG: &str = r#"uniform sampler2D tex_in;
uniform float glow_radius_in;
uniform vec2 resolution_in;

uniform int ove_iteration;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  int radius = int(clamp(glow_radius_in, 0.0, 64.0) + 0.5);
  if (radius < 1) {
    frag_color = texture(tex_in, ove_texcoord);
    return;
  }

  vec4 composite = vec4(0.0);
  for (int i = -radius; i <= radius; ++i) {
    vec2 uv = ove_texcoord;
    if (ove_iteration == 0) {
      uv.x += float(i) / resolution_in.x;
    } else {
      uv.y += float(i) / resolution_in.y;
    }
    composite += texture(tex_in, uv);
  }

  frag_color = composite / float(radius * 2 + 1);
}
"#;

/// Fragment shader of the glow colorize pass: tint the blurred coverage
/// with [`GLOW_COLOR_INPUT`] and emit it premultiplied.
const GLOW_COLORIZE_FRAG: &str = r#"uniform sampler2D tex_in;
uniform vec4 glow_color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
  vec4 coverage = texture(tex_in, ove_texcoord);
  float alpha = coverage.a * glow_color_in.a;

  frag_color = vec4(glow_color_in.rgb * alpha, alpha);
}
"#;

impl TextGeneratorV3 {
	/// Map our alignment to the gizmo's alignment int (C++
	/// `get_qt_alignment_from_ours()`): Top -> `TextGizmo::k_align_top`,
	/// Middle -> `TextGizmo::k_align_vcenter`, Bottom ->
	/// `TextGizmo::k_align_bottom` (0 = top, 1 = bottom, 2 = vcenter in
	/// the gizmo's numbering).
	pub fn get_gizmo_alignment_from_ours(v: VerticalAlignment) -> i32 {
		match v {
			VerticalAlignment::Top => 0,
			VerticalAlignment::Middle => 2,
			VerticalAlignment::Bottom => 1,
		}
	}

	/// Map the gizmo's alignment int back to ours (C++
	/// `get_our_alignment_from_qts()`); unknown values map to
	/// [`VerticalAlignment::Top`].
	pub fn get_our_alignment_from_gizmos(v: i32) -> VerticalAlignment {
		match v {
			1 => VerticalAlignment::Bottom,
			2 => VerticalAlignment::Middle,
			_ => VerticalAlignment::Top,
		}
	}

	/// The current alignment (C++ `get_vertical_alignment()`): the
	/// `valign_in` standard value as a [`VerticalAlignment`].
	pub fn vertical_alignment(core: &NodeCore) -> VerticalAlignment {
		VerticalAlignment::from_int(
			core.standard_value(VERTICAL_ALIGNMENT_INPUT, -1)
				.to_double() as i32,
		)
	}

	/// Expand `%N` placeholders with args (C++ `format_string()`):
	/// `%%` yields a literal `%`; `%` followed by digits parses an int
	/// (out-of-int-range parses fail to 0, making the index -1) and
	/// substitutes `args[index - 1]` when in range, otherwise expands
	/// to nothing; a lone `%` before a non-digit/non-`%` is copied
	/// verbatim.
	pub fn format_string(input: &str, args: &[String]) -> String {
		let bytes = input.as_bytes();
		let mut output = String::new();
		let mut i = 0;
		while i < bytes.len() {
			let c = bytes[i] as char;
			if i + 1 < bytes.len() && c == '%' {
				let next = bytes[i + 1] as char;
				if next == '%' {
					// Double percent, append a single percent.
					output.push('%');
					i += 1;
				} else if next.is_ascii_digit() {
					// Find the length of the number (QString::toInt()
					// semantics: out-of-int-range parses fail and yield 0,
					// making the index -1).
					let mut num = String::new();
					i += 1;
					while i < bytes.len() && bytes[i].is_ascii_digit() {
						num.push(bytes[i] as char);
						i += 1;
					}
					i -= 1;
					let n: i64 = num.parse().unwrap_or(0);
					let index = if n > i32::MAX as i64 || n < i32::MIN as i64 {
						-1
					} else {
						(n as i32) - 1
					};
					if index >= 0 && (index as usize) < args.len() {
						output.push_str(&args[index as usize]);
					}
				} else {
					output.push(c);
				}
			} else {
				output.push(c);
			}
			i += 1;
		}
		output
	}

	/// Gizmo activated callback (C++ `gizmo_activated()`): sets
	/// `use_args_in` to `false` and `dont_emit_valign_ = true`.
	fn gizmo_activated(&mut self, core: &mut NodeCore) {
		core.set_standard_value(USE_ARGS_INPUT, -1, NodeValue::Boolean(false));
		self.dont_emit_valign = true;
	}

	/// Gizmo deactivated callback (C++ `gizmo_deactivated()`): sets
	/// `use_args_in` to `true` and `dont_emit_valign_ = true`.
	fn gizmo_deactivated(&mut self, core: &mut NodeCore) {
		core.set_standard_value(USE_ARGS_INPUT, -1, NodeValue::Boolean(true));
		self.dont_emit_valign = true;
	}

	/// Set the vertical alignment through the undo system (C++
	/// `set_vertical_alignment_undoable()`, formerly a
	/// `NodeParamSetStandardValueCommand` on the undo stack). The undo
	/// command stack is not part of this crate, so only the resulting
	/// standard-value write is performed (`// CPP-PARITY: textv3.cpp`
	/// `set_vertical_alignment_undoable`).
	fn set_vertical_alignment_undoable(&mut self, core: &mut NodeCore, a: i32) {
		core.set_standard_value(
			VERTICAL_ALIGNMENT_INPUT,
			-1,
			NodeValue::Combo(Self::get_our_alignment_from_gizmos(a) as i64),
		);
	}
}

impl NodeBehavior for TextGeneratorV3 {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Text"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.text3"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Generator]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Generate rich text."
	}

	/// Localized input names (C++ `retranslate()`): `text_in` ->
	/// "Text", `valign_in` -> "Vertical Alignment" (combo strings
	/// Top/Middle/Bottom), `args_in` -> "Arguments"; the base class
	/// retranslate covers the inherited shape inputs and `base_in`
	/// ("Base"). The redesign inputs are named here too ("Text" for
	/// `plain_text_in`, "Font Family", "Font Size", "Outline"/"Outline
	/// Color"/"Outline Width", "Glow"/"Glow Color"/"Glow Radius").
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXT_INPUT | PLAIN_TEXT_INPUT => "Text",
			FONT_FAMILY_INPUT => "Font Family",
			FONT_SIZE_INPUT => "Font Size",
			OUTLINE_ENABLED_INPUT => "Outline",
			OUTLINE_COLOR_INPUT => "Outline Color",
			OUTLINE_WIDTH_INPUT => "Outline Width",
			GLOW_ENABLED_INPUT => "Glow",
			GLOW_COLOR_INPUT => "Glow Color",
			GLOW_RADIUS_INPUT => "Glow Radius",
			COLOR_INPUT => "Color",
			VERTICAL_ALIGNMENT_INPUT => "Vertical Alignment",
			ARGS_INPUT => "Arguments",
			crate::nodes::generatorwithmerge::BASE_INPUT => "Base",
			_ => crate::nodes::shapenodebase::ShapeNodeBase::input_name(id),
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `valign_in` -> "Top", "Middle",
	/// "Bottom". The redesign's `font_family_in` is a str-combo whose
	/// option list is injected by the backend layer, so it has no
	/// static labels here.
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			VERTICAL_ALIGNMENT_INPUT => vec!["Top", "Middle", "Bottom"],
			_ => Vec::new(),
		}
	}

	/// Shader code request: the merged generate shader is served under
	/// the `"mrg"` request (shared with
	/// [`crate::nodes::generatorwithmerge`]), the four post-process passes
	/// (REDESIGN, no C++ counterpart) under their shader ids. Every other
	/// request is unhandled.
	fn shader_code(&self, request: &str) -> Option<String> {
		match request {
			"mrg" => Some(crate::nodes::generatorwithmerge::merge_shader_frag().to_string()),
			OUTLINE_DILATE_SHADER_ID => Some(OUTLINE_DILATE_FRAG.to_string()),
			OUTLINE_COLORIZE_SHADER_ID => Some(OUTLINE_COLORIZE_FRAG.to_string()),
			GLOW_BLUR_SHADER_ID => Some(GLOW_BLUR_FRAG.to_string()),
			GLOW_COLORIZE_SHADER_ID => Some(GLOW_COLORIZE_FRAG.to_string()),
			_ => None,
		}
	}

	/// Evaluate outputs (C++ `value()`): if `use_args_in` is set and
	/// the args array is non-empty, expand `%N` placeholders in the
	/// text via [`Self::format_string`]; if the resulting text is
	/// non-empty, push a merged texture generate job (params from the
	/// incoming base texture when present, else the global video
	/// params, forced to `PixelFormat::u8` and the project's default
	/// input color space, with the expanded text inserted back into
	/// the job); otherwise pass the base input texture through
	/// unchanged.
	///
	/// REDESIGN: the text evaluated comes from [`PLAIN_TEXT_INPUT`] when
	/// a text layout backend is installed (the structured path) and from
	/// the legacy [`TEXT_INPUT`] HTML otherwise, so a backend-less build
	/// keeps the pre-redesign behavior exactly. The C++ builds the layout
	/// request here (`Texture::job(text_params, job)` carries the
	/// laid-out document); the Rust job has no payload, so the request is
	/// built by [`Self::layout_request`] instead and this method only
	/// decides which text the job describes.
	///
	/// REDESIGN (wave 2): with an outline and/or glow pass enabled,
	/// [`Self::build_post_job`] rasterizes the evaluated text and the
	/// pushed handle carries the post-process chain instead of the plain
	/// generate job. Either way the job goes through
	/// [`crate::nodes::generatorwithmerge::GeneratorWithMerge::push_mergable_job`]
	/// (with a null handle — the renderer-deferred generate job — when no
	/// pass is enabled, which is the pre-redesign behavior); the args
	/// array resolves to the single row value when present (a per-element
	/// array model is deferred), so `%N` expansion is exercised directly
	/// via [`Self::format_string`] (`// CPP-PARITY: textv3.cpp` `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		let mut text = Self::job_text(Self::plain_text_path(), core, inputs, time);

		let use_args_val = inputs
			.get(USE_ARGS_INPUT)
			.cloned()
			.unwrap_or_else(|| core.value_at_time(USE_ARGS_INPUT, -1, time));
		if to_bool(&use_args_val) {
			let args: Vec<String> = match inputs.get(ARGS_INPUT) {
				Some(NodeValue::Text(s)) => vec![s.clone()],
				_ => Vec::new(),
			};
			if !args.is_empty() {
				text = Self::format_string(&text, &args);
			}
		}

		if !text.is_empty() {
			// C++ `push_mergable_job(value, Texture::job(text_params, job),
			// table)` — merged over base_in when connected, else pushed
			// directly. An enabled outline/glow pass boxes the post-process
			// chain (REDESIGN wave 2); with both passes off the plain
			// raster is the output — the pre-backend deferred null job only
			// applies when no render backend is installed.
			let job = match self.build_post_job(core, inputs, &text, time) {
				Some(job) => job,
				None => {
					let size = Self::raster_size(core, inputs, time);
					let align = Self::alignment_arg(core, inputs);
					let font_color = Self::color_arg(core, inputs, COLOR_INPUT, time);
					match Self::rasterize_text(inputs, &text, size, align, font_color) {
						// The addref runs while the value owns its handle
						// reference (NodeValue::drop releases it) — taking
						// the bare handle out first would dangle it.
						Some(NodeValue::Texture(handle)) => unsafe { handle.addref() },
						_ => crate::handle::CHandle::null(),
					}
				}
			};
			crate::nodes::generatorwithmerge::GeneratorWithMerge::push_mergable_job(
				inputs, job, table,
			);
		} else if let Some(base @ NodeValue::Texture(_)) =
			inputs.get(crate::nodes::generatorwithmerge::BASE_INPUT)
		{
			table.push(base.value_type(), base.clone(), None);
		}
	}

	/// Direct frame generation (C++ `generate_frame()`): clears the
	/// RGBA8888-premultiplied frame to transparent, then (only when a
	/// measure backend is installed) lays out the text as Olive HTML at
	/// 96 DPI (3780 dots/meter) wrapped to the shape size X, computes
	/// the base offset from the shape position re-centered into frame
	/// space, applies the vertical alignment to the draw offset (top:
	/// none; middle: `size.y/2 - doc.height/2`; bottom: `size.y -
	/// doc.height`), clips to the shape rect at the base offset, and
	/// renders over the buffer via the render backend. With no measure
	/// backend installed, warns once and leaves the cleared frame
	/// untouched.
	///
	/// The Rust frame is an opaque [`crate::handle::CHandle`]
	/// whose pixels cannot be read or written from this crate, so the
	/// body is a documented no-op; the layout/measure/offset control flow
	/// is ported in [`Self::layout_request`], [`Self::base_offset`] and
	/// [`Self::draw_offset`], and exercised by the tests.
	fn generate_frame(
		&self,
		core: &NodeCore,
		frame: &mut crate::handle::CHandle,
		time: Rational,
	) {
		let _ = (core, frame, time);
	}

	/// Gizmo position update (C++ `update_gizmo_positions()`): after
	/// the base update, sets the text gizmo rect to the bounding rect
	/// of the polygon gizmo's polygon (empty polygon -> zero rect) and
	/// feeds it the current `text_in` HTML.
	///
	/// The polygon/text gizmos live in the GUI layer with no Rust model
	/// in this crate, so this is a documented no-op
	/// (`// CPP-PARITY: textv3.cpp` `update_gizmo_positions`).
	fn gizmo_update(&self, core: &NodeCore, row: &NodeValueRow) {
		let _ = (core, row);
	}

	/// Input value changed (C++ `InputValueChangedEvent()`): when
	/// `valign_in` changes and `dont_emit_valign_` is not set, forwards
	/// the new alignment to the text gizmo; then defers to the base
	/// implementation.
	///
	/// The text gizmo has no Rust model in this crate, so only the
	/// flag check is represented (`// CPP-PARITY: textv3.cpp`
	/// `InputValueChangedEvent`). REDESIGN: a change of the legacy
	/// [`TEXT_INPUT`] HTML also runs the one-shot HTML-to-plain-text
	/// migration ([`migrate_legacy_html`] — one of its three call
	/// sites, see the function).
	fn input_value_changed(&mut self, core: &mut NodeCore, input: &str, element: i32) {
		let _ = element;
		if input == VERTICAL_ALIGNMENT_INPUT && !self.dont_emit_valign {
			// The C++ forwards the new alignment to the text gizmo here.
		}
		if input == TEXT_INPUT {
			migrate_legacy_html(core);
		}
	}

	/// Post-load fixups (C++ `PostLoadEvent`): runs the REDESIGN
	/// HTML-to-plain-text migration ([`migrate_legacy_html`]) for load
	/// pipelines that call this hook after the inputs are applied.
	fn post_load(&mut self, core: &mut NodeCore) {
		migrate_legacy_html(core);
	}

	/// Custom load (C++ `load_custom()`): consume the `<custom>` segment
	/// exactly like the default implementation, then run the REDESIGN
	/// migration. The node-body parser writes the `<input>` values before
	/// the trailing `<custom>` element, so this is the hook that fires
	/// with the legacy [`TEXT_INPUT`] value already loaded.
	fn load_custom(
		&mut self,
		core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		reader.skip_current_element();
		migrate_legacy_html(core);
		true
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TextGeneratorV3 {
			dont_emit_valign: self.dont_emit_valign,
		}))
	}

	/// Downcast to [`Self`] (gizmo-state access).
	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	/// Mutable downcast (see [`NodeBehavior::as_any`]).
	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

impl TextGeneratorV3 {
	/// Whether the structured plain-text path is active (REDESIGN):
	/// `true` when a text layout backend is installed. Without a backend
	/// the node keeps the pre-redesign behavior (the legacy
	/// [`TEXT_INPUT`] HTML is carried).
	pub fn plain_text_path() -> bool {
		super::textbackend::text_measure_backend().is_some()
	}

	/// The text [`Self::value`] evaluates (REDESIGN split of the C++
	/// `value()` text extraction): [`PLAIN_TEXT_INPUT`] on the
	/// `plain_text == true` path, the legacy [`TEXT_INPUT`] HTML
	/// otherwise. Row values win over the core's value at `time`, like
	/// the C++ input evaluation.
	fn job_text(
		plain_text: bool,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
	) -> String {
		let id = if plain_text {
			PLAIN_TEXT_INPUT
		} else {
			TEXT_INPUT
		};
		let val = inputs
			.get(id)
			.cloned()
			.unwrap_or_else(|| core.value_at_time(id, -1, time));
		to_text(&val)
	}

	/// Build the C++ `TextLayoutRequest` (textv3.cpp `generate_frame()`)
	/// for the active text path: with a backend installed, the structured
	/// request — [`PLAIN_TEXT_INPUT`] as [`TextLayoutMode::PlainText`]
	/// with `font_family_in`/`font_size_in` — else the pre-redesign
	/// request, Olive-HTML text from [`TEXT_INPUT`] at 96 DPI (3780
	/// dots/meter) with the font taken from the markup. Both wrap to the
	/// shape size X; the backend defaults are used when font family/size
	/// are empty/zero.
	pub fn layout_request(row: &NodeValueRow) -> TextLayoutRequest {
		Self::layout_request_path(Self::plain_text_path(), row)
	}

	/// [`Self::layout_request`] with the path chosen explicitly: the
	/// backend state is a process-global, so the tests drive both paths
	/// through this parameter instead of installing hooks.
	fn layout_request_path(plain_text: bool, row: &NodeValueRow) -> TextLayoutRequest {
		let size = row
			.get(crate::nodes::shapenodebase::SIZE_INPUT)
			.map(to_vec2)
			.unwrap_or([0.0, 0.0]);
		if plain_text {
			TextLayoutRequest {
				text: row
					.get(PLAIN_TEXT_INPUT)
					.map(to_text)
					.unwrap_or_else(String::new),
				mode: TextLayoutMode::PlainText,
				font_family: row
					.get(FONT_FAMILY_INPUT)
					.map(to_text)
					.unwrap_or_else(String::new),
				font_size_pt: row
					.get(FONT_SIZE_INPUT)
					.map(|v| v.to_double())
					.unwrap_or(0.0),
				dots_per_meter: 3780,
				wrap_width: size[0],
				center_horizontally: false,
			}
		} else {
			TextLayoutRequest {
				text: row.get(TEXT_INPUT).map(to_text).unwrap_or_else(String::new),
				mode: TextLayoutMode::OliveHtml,
				font_family: String::new(),
				font_size_pt: 0.0,
				dots_per_meter: 3780,
				wrap_width: size[0],
				center_horizontally: false,
			}
		}
	}

	/// The C++ base offset (textv3.cpp `generate_frame()`): the shape
	/// position re-centered into frame space — `pos - size/2 + frame/2`
	/// (the frame halves are integer division in C++).
	pub fn base_offset(
		pos: [f64; 2],
		size: [f64; 2],
		frame_width: i32,
		frame_height: i32,
	) -> (f64, f64) {
		(
			pos[0] - size[0] / 2.0 + (frame_width / 2) as f64,
			pos[1] - size[1] / 2.0 + (frame_height / 2) as f64,
		)
	}

	/// The C++ draw offset (textv3.cpp `generate_frame()`): the base
	/// offset plus the vertical-alignment delta — top: none; middle:
	/// `size.y/2 - doc.height/2`; bottom: `size.y - doc.height` (all
	/// double math, unlike the integer halving in v2).
	pub fn draw_offset(
		align: VerticalAlignment,
		base: (f64, f64),
		size: [f64; 2],
		doc_height: f64,
	) -> (f64, f64) {
		let (dx, mut dy) = base;
		match align {
			VerticalAlignment::Top => {}
			VerticalAlignment::Middle => dy += size[1] / 2.0 - doc_height / 2.0,
			VerticalAlignment::Bottom => dy += size[1] - doc_height,
		}
		(dx, dy)
	}

	/// The C++ `TextRenderTransform` (textv3.cpp `generate_frame()`):
	/// scale, the draw offset, and the clip rect at the base offset
	/// covering the shape size (set before the vertical-alignment
	/// translate in the C++).
	pub fn render_transform(
		scale: f64,
		draw: (f64, f64),
		base: (f64, f64),
		size: [f64; 2],
	) -> TextRenderTransform {
		TextRenderTransform {
			scale,
			draw_offset_x: draw.0,
			draw_offset_y: draw.1,
			clip_enabled: true,
			clip_offset_x: base.0,
			clip_offset_y: base.1,
			clip_width: size[0],
			clip_height: size[1],
		}
	}

	/// The layout/measure control flow of the C++ `generate_frame()` with
	/// the backend hooks: build the request and measure via the installed
	/// measure backend (zero size when none is installed — the documented
	/// no-backend fallback; the frame is left cleared).
	///
	/// The render step needs the frame's pixel buffer, which the Rust
	/// frame handle does not expose; it is not representable here
	/// (`// CPP-PARITY: textv3.cpp` `generate_frame`).
	pub fn measure_and_layout(row: &NodeValueRow) -> (TextLayoutRequest, TextLayoutSize) {
		let req = Self::layout_request(row);
		let doc = match super::textbackend::text_measure_backend() {
			Some(measure) => measure(&req),
			None => TextLayoutSize::default(),
		};
		(req, doc)
	}

	/// Read an input: the row value when present, else the core's value at
	/// `time` (the row-first lookup [`Self::job_text`] uses).
	fn input_value(core: &NodeCore, inputs: &NodeValueRow, id: &str, time: Rational) -> NodeValue {
		inputs
			.get(id)
			.cloned()
			.unwrap_or_else(|| core.value_at_time(id, -1, time))
	}

	/// Read a float input (REDESIGN post-process inputs).
	fn float_arg(core: &NodeCore, inputs: &NodeValueRow, id: &str, time: Rational) -> f64 {
		Self::input_value(core, inputs, id, time).to_double()
	}

	/// Read a boolean input (REDESIGN post-process inputs).
	fn bool_arg(core: &NodeCore, inputs: &NodeValueRow, id: &str, time: Rational) -> bool {
		let val = Self::input_value(core, inputs, id, time);
		to_bool(&val)
	}

	/// Read a color input; a mis-typed value falls back to opaque black
	/// (the same fallback the C++ `Variant::to_color()` callers get for a
	/// non-color).
	fn color_arg(core: &NodeCore, inputs: &NodeValueRow, id: &str, time: Rational) -> [f64; 4] {
		match Self::input_value(core, inputs, id, time) {
			NodeValue::Color(c) => c,
			_ => [0.0, 0.0, 0.0, 1.0],
		}
	}

	/// The vertical alignment of this evaluation: the row's `valign_in`
	/// when present, else the standard value (the lookup
	/// [`Self::vertical_alignment`] documents).
	fn alignment_arg(core: &NodeCore, inputs: &NodeValueRow) -> VerticalAlignment {
		match inputs.get(VERTICAL_ALIGNMENT_INPUT) {
			Some(v) => VerticalAlignment::from_int(v.to_double() as i32),
			None => Self::vertical_alignment(core),
		}
	}

	/// Clamp one raster dimension into `1..=MAX_RASTER_SIZE`; a
	/// non-finite size falls back to a single pixel.
	fn clamp_raster(v: f64) -> i32 {
		if !v.is_finite() {
			return 1;
		}
		(v.round() as i32).clamp(1, MAX_RASTER_SIZE)
	}

	/// The rasterization size of this evaluation: the shape size
	/// (`size_in`), rounded and clamped per dimension.
	fn raster_size(core: &NodeCore, inputs: &NodeValueRow, time: Rational) -> (i32, i32) {
		let size = to_vec2(&Self::input_value(
			core,
			inputs,
			crate::nodes::shapenodebase::SIZE_INPUT,
			time,
		));
		(Self::clamp_raster(size[0]), Self::clamp_raster(size[1]))
	}

	/// Box one post-process shader job (REDESIGN, no C++ counterpart):
	/// this node's type id (the chain is all ours), an invalid node id
	/// (the jobs are synthetic — no graph node evaluates them) and the
	/// shader id selecting the pass in [`Self::shader_code`].
	fn shader_job(
		time: Rational,
		type_id: &str,
		shader_id: &str,
		effect_input: &str,
		iterations: i32,
		params: NodeValueRow,
	) -> crate::handle::CHandle {
		crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
			node_id: crate::id::NodeId::INVALID,
			time,
			iterations,
			type_id: type_id.to_string(),
			shader_id: shader_id.to_string(),
			effect_input: effect_input.to_string(),
			params,
			iterative_input: String::new(),
		}))
	}

	/// Box a `"mrg"` job drawing `blend` (the top layer) over `base` (the
	/// backdrop): the merge node's premultiplied alpha-over,
	/// `base = base * (1 - blend.a) + blend`.
	fn merge_job(
		time: Rational,
		type_id: &str,
		base: &NodeValue,
		blend: &NodeValue,
	) -> crate::handle::CHandle {
		let mut params = NodeValueRow::new();
		params.insert(crate::nodes::merge::BASE_INPUT.to_string(), base.clone());
		params.insert(crate::nodes::merge::BLEND_INPUT.to_string(), blend.clone());
		Self::shader_job(
			time,
			type_id,
			"mrg",
			crate::nodes::merge::BASE_INPUT,
			1,
			params,
		)
	}

	/// Rasterize the evaluated `text` into an RGBA premultiplied F32
	/// coverage texture (REDESIGN, no C++ counterpart — the C++ renders
	/// straight into the output frame instead): layout the plain-text
	/// request with `text` substituted for [`PLAIN_TEXT_INPUT`], measure
	/// it, render into a `size`-sized staging buffer with the crate's
	/// draw/clip transform, then widen the 8-bit coverage to float.
	///
	/// `None` without a render backend (the documented no-backend
	/// fallback), for an empty raster, or when the staging frame cannot be
	/// allocated. The white raster is tinted by `color` (the font color,
	/// [`COLOR_INPUT`]) while widening to float.
	fn rasterize_text(
		row: &NodeValueRow,
		text: &str,
		size: (i32, i32),
		align: VerticalAlignment,
		color: [f64; 4],
	) -> Option<NodeValue> {
		let render = super::textbackend::text_render_backend()?;
		let (width, height) = size;
		if width <= 0 || height <= 0 {
			return None;
		}

		let mut req_row = row.clone();
		req_row.insert(
			PLAIN_TEXT_INPUT.to_string(),
			NodeValue::Text(text.to_string()),
		);
		let req = Self::layout_request_path(true, &req_row);
		let doc = match super::textbackend::text_measure_backend() {
			Some(measure) => measure(&req),
			None => TextLayoutSize::default(),
		};

		// The backend writes 8-bit premultiplied RGBA over the existing
		// (cleared) rows; the shape-local offsets keep the text rect at
		// the raster origin, with the vertical alignment applied.
		let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
		{
			let target = super::textbackend::TextRenderTarget {
				data: &mut rgba,
				width,
				height,
				linesize_bytes: width * 4,
				channel_count: 4,
			};
			let draw =
				Self::draw_offset(align, (0.0, 0.0), [width as f64, height as f64], doc.height);
			let transform =
				Self::render_transform(1.0, draw, (0.0, 0.0), [width as f64, height as f64]);
			render(&req, &transform, target);
		}

		let mut frame = Frame::new();
		frame.set_video_params(VideoParamsPod {
			width,
			height,
			..Default::default()
		});
		if !frame.allocate() {
			return None;
		}
		for (pixel, coverage) in frame.data.chunks_exact_mut(16).zip(rgba.chunks_exact(4)) {
			// The backend rasterizes in opaque-premultiplied white; tint by
			// [`COLOR_INPUT`] per channel (the alpha scales too — a
			// half-transparent font color stays premultiplied).
			for (c, (channel, byte)) in pixel.chunks_exact_mut(4).zip(coverage).enumerate() {
				let v = f32::from(*byte) / 255.0 * color[c] as f32;
				channel.copy_from_slice(&v.to_le_bytes());
			}
		}

		Some(NodeValue::Texture(crate::handle::make_owned(
			Texture::wrap_frame(frame),
		)))
	}

	/// Build the outline/glow post-process chain (REDESIGN, no C++
	/// counterpart) for the evaluated `text`: rasterize the coverage,
	/// dilate and colorize it into a stroke when the outline is enabled,
	/// blur and colorize it into a glow when the glow is enabled, and
	/// merge the results **beneath** the text (the text stays on top).
	///
	/// With both enabled the outline runs first and the glow samples the
	/// stroke; with both disabled — or without a render backend — `None`,
	/// so the caller keeps the pre-redesign deferred null job.
	fn build_post_job(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		text: &str,
		time: Rational,
	) -> Option<crate::handle::CHandle> {
		let outline = Self::bool_arg(core, inputs, OUTLINE_ENABLED_INPUT, time);
		let glow = Self::bool_arg(core, inputs, GLOW_ENABLED_INPUT, time);
		if !outline && !glow {
			return None;
		}

		let type_id = self.type_id();
		let size = Self::raster_size(core, inputs, time);
		let align = Self::alignment_arg(core, inputs);
		let font_color = Self::color_arg(core, inputs, COLOR_INPUT, time);
		let text_tex = Self::rasterize_text(inputs, text, size, align, font_color)?;
		let resolution = NodeValue::Vec2([size.0 as f64, size.1 as f64]);

		let mut result = text_tex.clone();
		let mut stroke: Option<NodeValue> = None;

		if outline {
			let mut params = NodeValueRow::new();
			params.insert(POST_TEXTURE_INPUT.to_string(), text_tex.clone());
			params.insert(
				OUTLINE_WIDTH_INPUT.to_string(),
				NodeValue::Float(Self::float_arg(core, inputs, OUTLINE_WIDTH_INPUT, time)),
			);
			params.insert(RESOLUTION_INPUT.to_string(), resolution.clone());
			let dilated = NodeValue::Texture(Self::shader_job(
				time,
				type_id,
				OUTLINE_DILATE_SHADER_ID,
				POST_TEXTURE_INPUT,
				1,
				params,
			));

			let mut params = NodeValueRow::new();
			params.insert(POST_TEXTURE_INPUT.to_string(), dilated);
			params.insert(
				OUTLINE_COLOR_INPUT.to_string(),
				NodeValue::Color(Self::color_arg(core, inputs, OUTLINE_COLOR_INPUT, time)),
			);
			params.insert(RESOLUTION_INPUT.to_string(), resolution.clone());
			let colorized = NodeValue::Texture(Self::shader_job(
				time,
				type_id,
				OUTLINE_COLORIZE_SHADER_ID,
				POST_TEXTURE_INPUT,
				1,
				params,
			));

			// The stroke is the widened, colorized coverage drawn over the
			// text itself, so the glyphs stay on top of their outline.
			let stroke_tex =
				NodeValue::Texture(Self::merge_job(time, type_id, &colorized, &text_tex));
			stroke = Some(stroke_tex.clone());
			result = stroke_tex;
		}

		if glow {
			let source = stroke.clone().unwrap_or_else(|| text_tex.clone());
			let mut params = NodeValueRow::new();
			params.insert(POST_TEXTURE_INPUT.to_string(), source);
			params.insert(
				GLOW_RADIUS_INPUT.to_string(),
				NodeValue::Float(Self::float_arg(core, inputs, GLOW_RADIUS_INPUT, time)),
			);
			params.insert(RESOLUTION_INPUT.to_string(), resolution.clone());
			// Two iterations, one per axis (the shader picks the axis).
			let blurred = NodeValue::Texture(Self::shader_job(
				time,
				type_id,
				GLOW_BLUR_SHADER_ID,
				POST_TEXTURE_INPUT,
				2,
				params,
			));

			let mut params = NodeValueRow::new();
			params.insert(POST_TEXTURE_INPUT.to_string(), blurred);
			params.insert(
				GLOW_COLOR_INPUT.to_string(),
				NodeValue::Color(Self::color_arg(core, inputs, GLOW_COLOR_INPUT, time)),
			);
			params.insert(RESOLUTION_INPUT.to_string(), resolution.clone());
			let glow_tex = NodeValue::Texture(Self::shader_job(
				time,
				type_id,
				GLOW_COLORIZE_SHADER_ID,
				POST_TEXTURE_INPUT,
				1,
				params,
			));

			// The glow is drawn over the stroke (or the bare text when the
			// outline is off), which is drawn over the text.
			let blend = stroke.clone().unwrap_or_else(|| text_tex.clone());
			result = NodeValue::Texture(Self::merge_job(time, type_id, &glow_tex, &blend));
		}

		let NodeValue::Texture(handle) = &result else {
			return None;
		};
		Some(unsafe { handle.addref() })
	}
}

/// Strip an HTML fragment to plain text (REDESIGN helper, no C++
/// counterpart): every `<...>` tag is dropped, then the entities
/// `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&#39;` and `&nbsp;` are decoded
/// (the latter to a plain space — a non-breaking space is not
/// representable in the plain-text input, a documented simplification).
/// Unknown entities and a bare `&` are copied verbatim; an unterminated
/// `<` swallows the rest of the input.
///
/// This is a simple stripper, not a conforming HTML parser: tags are
/// dropped first and entities decoded afterwards in a single pass (so
/// `&lt;p&gt;` stays the literal text `<p>`), and whitespace is neither
/// collapsed nor trimmed (so `<p>a</p><p>b</p>` becomes `ab`). It only
/// exists to migrate the legacy [`TEXT_INPUT`] payload into
/// [`PLAIN_TEXT_INPUT`].
pub fn strip_html_to_plain(html: &str) -> String {
	/// Whether `chars` starts with the (ASCII) `entity` text.
	fn starts_with(chars: &[char], entity: &str) -> bool {
		let mut it = chars.iter();
		entity.chars().all(|c| it.next() == Some(&c))
	}

	const ENTITIES: [(&str, &str); 6] = [
		("&amp;", "&"),
		("&lt;", "<"),
		("&gt;", ">"),
		("&quot;", "\""),
		("&#39;", "'"),
		("&nbsp;", " "),
	];

	let chars: Vec<char> = html.chars().collect();
	let mut out = String::with_capacity(html.len());
	let mut i = 0;
	while i < chars.len() {
		match chars[i] {
			'<' => {
				// Drop up to and including the tag's closing '>'; an
				// unterminated tag drops the remainder. The tag text is
				// discarded, never rescanned, so a decoded `&lt;p&gt;`
				// cannot turn into a tag afterwards.
				i += 1;
				while i < chars.len() && chars[i] != '>' {
					i += 1;
				}
				i += 1;
			}
			'&' => {
				let decoded = ENTITIES
					.iter()
					.find(|(entity, _)| starts_with(&chars[i..], entity));
				match decoded {
					Some((entity, replacement)) => {
						out.push_str(replacement);
						i += entity.chars().count();
					}
					None => {
						// Unknown entity (or a bare '&'): keep it as-is.
						out.push('&');
						i += 1;
					}
				}
			}
			c => {
				out.push(c);
				i += 1;
			}
		}
	}
	out
}

/// One-shot migration of a pre-redesign project's legacy [`TEXT_INPUT`]
/// HTML into [`PLAIN_TEXT_INPUT`] (REDESIGN helper, no C++
/// counterpart): when the plain text is still untouched (empty or the
/// [`DEFAULT_PLAIN_TEXT`] default) and the legacy input holds a
/// non-empty, non-default HTML value, the stripped plain text is written
/// to `plain_text_in` and `true` is returned. The legacy value is never
/// modified, so an old project can still be saved in its original form;
/// after a successful migration the plain text is no longer the default
/// and further calls are no-ops (idempotent).
///
/// A project created after the redesign serializes `plain_text_in` at
/// the same default, indistinguishable through the standard values from
/// a legacy node with an untouched default HTML payload; that case is
/// left alone (the default HTML is not migrated) rather than replacing
/// the redesign default with the legacy "Sample Text".
///
/// Call sites: [`NodeBehavior::load_custom`] (fires in the node-body
/// parser after the `<input>` elements — the hook the real load path
/// reaches), [`NodeBehavior::post_load`] (for load pipelines that call
/// it after the inputs are applied) and [`NodeBehavior::input_value_changed`]
/// for the legacy input. The migration only takes effect in the facade
/// once a loader calls one of them.
pub fn migrate_legacy_html(core: &mut NodeCore) -> bool {
	if core.get_input(PLAIN_TEXT_INPUT).is_none() {
		return false;
	}
	let plain_untouched = matches!(
		&core.standard_value(PLAIN_TEXT_INPUT, -1),
		NodeValue::Text(s) if s.is_empty() || s == DEFAULT_PLAIN_TEXT
	);
	if !plain_untouched {
		return false;
	}
	let plain = match &core.standard_value(TEXT_INPUT, -1) {
		NodeValue::Text(t) if !t.is_empty() && t != LEGACY_DEFAULT_TEXT_HTML => {
			strip_html_to_plain(t)
		}
		_ => return false,
	};
	if plain.is_empty() {
		return false;
	}
	core.set_standard_value(PLAIN_TEXT_INPUT, -1, NodeValue::Text(plain));
	true
}

/// Constructor (C++ `TextGeneratorV3::TextGeneratorV3()`): builds the
/// shape base without its own gizmo behavior (`ShapeNodeBase(false)`),
/// adds `text_in` (hidden, REDESIGN), the structured redesign inputs
/// (`plain_text_in`, `font_family_in`, `font_size_in`, `outline_*`,
/// `glow_*`), `valign_in`, `use_args_in` and `args_in` with the
/// defaults, flags and properties documented on the constants, sets the
/// inherited `size_in` standard value to `(400, 300)`, creates the
/// `TextGizmo` bound to `text_in`, and initializes
/// `dont_emit_valign_ = false`.
///
/// The `TextGizmo` is a GUI-layer gizmo with no Rust model (see the
/// struct doc); the inherited inputs (`base_in` from the merge base,
/// `pos_in`/`size_in` from the shape base without its color input) are
/// wired here, mirroring the C++ constructor chain `Node ->
/// GeneratorWithMerge -> ShapeNodeBase(false) -> TextGeneratorV3`.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	// GeneratorWithMerge base: base_in texture effect input.
	let mut base = crate::input::Input::new(
		crate::nodes::generatorwithmerge::BASE_INPUT,
		crate::value::ValueType::Texture,
		NodeValue::None,
	);
	base.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	core.add_input(base);
	core.effect_input = crate::nodes::generatorwithmerge::BASE_INPUT.to_string();
	core.flags |= crate::node::flags::VIDEO_EFFECT;
	// REDESIGN (W6): text is a footage entry (the project panel's "add text
	// footage" button) and a timeline clip, no longer an effect the user
	// adds to a chain. The flag hides it from the effect library / add
	// menus only — `VIDEO_EFFECT` stays so text3 nodes already in a project
	// keep evaluating, and the factory keeps registering the type (the
	// footage path and old project files create it directly).
	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;

	// ShapeNodeBase(false): pos/size, no color input.
	core.add_input(crate::input::Input::new(
		crate::nodes::shapenodebase::POSITION_INPUT,
		crate::value::ValueType::Vec2,
		NodeValue::Vec2([0.0, 0.0]),
	));
	let mut size = crate::input::Input::new(
		crate::nodes::shapenodebase::SIZE_INPUT,
		crate::value::ValueType::Vec2,
		NodeValue::Vec2([100.0, 100.0]),
	);
	size.properties = vec![("min".to_string(), NodeValue::Vec2([0.0, 0.0]))];
	core.add_input(size);

	// Own inputs.
	// REDESIGN: the structured user-facing inputs. `plain_text_in` is the
	// text laid out when a backend is installed; the legacy `text_in`
	// stays as the hidden compatibility carrier (see the constants).
	core.add_input(crate::input::Input::new(
		PLAIN_TEXT_INPUT,
		crate::value::ValueType::Text,
		NodeValue::Text(DEFAULT_PLAIN_TEXT.to_string()),
	));

	let mut text = crate::input::Input::new(
		TEXT_INPUT,
		crate::value::ValueType::Text,
		NodeValue::Text(LEGACY_DEFAULT_TEXT_HTML.to_string()),
	);
	text.flags |= crate::input::flags::HIDDEN;
	text.properties = vec![("vieweronly".to_string(), NodeValue::Boolean(true))];
	core.add_input(text);

	core.add_input(crate::input::Input::new(
		FONT_FAMILY_INPUT,
		crate::value::ValueType::StrCombo,
		NodeValue::StrCombo(String::new()),
	));

	let mut font_size = crate::input::Input::new(
		FONT_SIZE_INPUT,
		crate::value::ValueType::Float,
		NodeValue::Float(72.0),
	);
	font_size.properties = vec![("min".to_string(), NodeValue::Float(1.0))];
	core.add_input(font_size);

	core.add_input(crate::input::Input::new(
		OUTLINE_ENABLED_INPUT,
		crate::value::ValueType::Boolean,
		NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		OUTLINE_COLOR_INPUT,
		crate::value::ValueType::Color,
		NodeValue::Color([0.0, 0.0, 0.0, 1.0]),
	));
	let mut outline_width = crate::input::Input::new(
		OUTLINE_WIDTH_INPUT,
		crate::value::ValueType::Float,
		NodeValue::Float(2.0),
	);
	outline_width.properties = vec![("min".to_string(), NodeValue::Float(0.0))];
	core.add_input(outline_width);

	core.add_input(crate::input::Input::new(
		GLOW_ENABLED_INPUT,
		crate::value::ValueType::Boolean,
		NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		GLOW_COLOR_INPUT,
		crate::value::ValueType::Color,
		NodeValue::Color([1.0, 1.0, 0.0, 1.0]),
	));
	let mut glow_radius = crate::input::Input::new(
		GLOW_RADIUS_INPUT,
		crate::value::ValueType::Float,
		NodeValue::Float(8.0),
	);
	glow_radius.properties = vec![("min".to_string(), NodeValue::Float(0.0))];
	core.add_input(glow_radius);

	core.add_input(crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Color,
		NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	));

	// Hidden alignment / args inputs, unchanged by the redesign.
	let mut valign = crate::input::Input::new(
		VERTICAL_ALIGNMENT_INPUT,
		crate::value::ValueType::Combo,
		NodeValue::Combo(0),
	);
	valign.flags |= STATIC_FLAGS | crate::input::flags::HIDDEN;
	core.add_input(valign);

	let mut use_args = crate::input::Input::new(
		USE_ARGS_INPUT,
		crate::value::ValueType::Boolean,
		NodeValue::Boolean(true),
	);
	use_args.flags |= STATIC_FLAGS | crate::input::flags::HIDDEN;
	core.add_input(use_args);

	let mut args = crate::input::Input::new(
		ARGS_INPUT,
		crate::value::ValueType::Text,
		NodeValue::Text(String::new()),
	);
	args.flags |= crate::input::flags::ARRAY;
	args.properties = vec![("arraystart".to_string(), NodeValue::Int(1))];
	core.add_input(args);

	// C++ set_standard_value override.
	core.set_standard_value(
		crate::nodes::shapenodebase::SIZE_INPUT,
		-1,
		NodeValue::Vec2([400.0, 300.0]),
	);

	(
		core,
		Box::new(TextGeneratorV3 {
			dont_emit_valign: false,
		}),
	)
}

/// Register this node type (C++ `k_text_generator_v3` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.text3",
		name: "Text",
		categories: &[Category::Generator],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::handle::CHandle;
	use crate::node::NodeBehavior;
	use crate::nodes::textbackend::TextRenderTarget;
	use crate::value::{NodeValueTable, ValueType};
	use oak_core::Rational;

	#[test]
	fn input_names() {
		let n = TextGeneratorV3 {
			dont_emit_valign: false,
		};
		assert_eq!(n.input_name(TEXT_INPUT), "Text");
		// REDESIGN: the plain-text input shares the "Text" display name.
		assert_eq!(n.input_name(PLAIN_TEXT_INPUT), "Text");
		assert_eq!(n.input_name(FONT_FAMILY_INPUT), "Font Family");
		assert_eq!(n.input_name(FONT_SIZE_INPUT), "Font Size");
		assert_eq!(n.input_name(OUTLINE_ENABLED_INPUT), "Outline");
		assert_eq!(n.input_name(OUTLINE_COLOR_INPUT), "Outline Color");
		assert_eq!(n.input_name(OUTLINE_WIDTH_INPUT), "Outline Width");
		assert_eq!(n.input_name(GLOW_ENABLED_INPUT), "Glow");
		assert_eq!(n.input_name(GLOW_COLOR_INPUT), "Glow Color");
		assert_eq!(n.input_name(GLOW_RADIUS_INPUT), "Glow Radius");
		assert_eq!(n.input_name(VERTICAL_ALIGNMENT_INPUT), "Vertical Alignment");
		assert_eq!(n.input_name(ARGS_INPUT), "Arguments");
		assert_eq!(
			n.input_name(crate::nodes::generatorwithmerge::BASE_INPUT),
			"Base"
		);
		assert_eq!(
			n.input_name(crate::nodes::shapenodebase::POSITION_INPUT),
			"Position"
		);
		assert_eq!(
			n.input_name(crate::nodes::shapenodebase::SIZE_INPUT),
			"Size"
		);
		// The hidden use_args_in input has no display name override.
		assert_eq!(n.input_name(USE_ARGS_INPUT), USE_ARGS_INPUT);
	}

	#[test]
	fn create_wires_inherited_and_own_inputs() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text3");
		assert_eq!(
			core.get_input(TEXT_INPUT).unwrap().value_type,
			ValueType::Text
		);
		assert!(core
			.get_input(TEXT_INPUT)
			.unwrap()
			.properties
			.iter()
			.any(|(k, v)| k == "vieweronly" && v == &NodeValue::Boolean(true)));
		let valign = core.get_input(VERTICAL_ALIGNMENT_INPUT).unwrap();
		assert_ne!(valign.flags & crate::input::flags::HIDDEN, 0);
		assert_ne!(valign.flags & crate::input::flags::NOT_CONNECTABLE, 0);
		assert_ne!(valign.flags & crate::input::flags::NOT_KEYFRAMABLE, 0);
		let use_args = core.get_input(USE_ARGS_INPUT).unwrap();
		assert_eq!(use_args.default, NodeValue::Boolean(true));
		let args = core.get_input(ARGS_INPUT).unwrap();
		assert_ne!(args.flags & crate::input::flags::ARRAY, 0);
		assert!(args
			.properties
			.iter()
			.any(|(k, v)| k == "arraystart" && v == &NodeValue::Int(1)));
		// The base has no color input of its own (ShapeNodeBase(false));
		// the font color input (REDESIGN wave 3) takes that exact slot.
		assert_eq!(
			core.get_input(crate::nodes::shapenodebase::COLOR_INPUT)
				.unwrap()
				.default,
			NodeValue::Color([1.0, 1.0, 1.0, 1.0])
		);
		assert_eq!(
			core.standard_value(crate::nodes::shapenodebase::SIZE_INPUT, -1),
			NodeValue::Vec2([400.0, 300.0])
		);
		assert_eq!(
			core.effect_input,
			crate::nodes::generatorwithmerge::BASE_INPUT
		);
		// REDESIGN: v3 left the create menu (text is a footage/clip now), so
		// this test flipped from the pre-redesign expectation (`== 0`).
		assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
		// It stays a video effect: legacy chains must keep evaluating.
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	/// The W6 contract: hidden from the add menus, still a working effect
	/// node for the chains (and the footage path) that create it directly.
	#[test]
	fn hidden_from_create_menu_but_still_a_video_effect() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text3");
		assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn alignment_round_trip() {
		for v in [
			VerticalAlignment::Top,
			VerticalAlignment::Middle,
			VerticalAlignment::Bottom,
		] {
			let gizmo = TextGeneratorV3::get_gizmo_alignment_from_ours(v);
			assert_eq!(TextGeneratorV3::get_our_alignment_from_gizmos(gizmo), v);
		}
		assert_eq!(
			TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Top),
			0
		);
		assert_eq!(
			TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Middle),
			2
		);
		assert_eq!(
			TextGeneratorV3::get_gizmo_alignment_from_ours(VerticalAlignment::Bottom),
			1
		);
		// Unknown gizmo values map to Top.
		assert_eq!(
			TextGeneratorV3::get_our_alignment_from_gizmos(99),
			VerticalAlignment::Top
		);
	}

	#[test]
	fn format_string_expands_args() {
		let args = vec!["foo".to_string(), "bar".to_string()];
		assert_eq!(
			TextGeneratorV3::format_string("hello %1", &args),
			"hello foo"
		);
		assert_eq!(TextGeneratorV3::format_string("%2 %1", &args), "bar foo");
		// Out of range expands to nothing.
		assert_eq!(TextGeneratorV3::format_string("[%3]", &args), "[]");
		assert_eq!(TextGeneratorV3::format_string("[%0]", &args), "[]");
	}

	#[test]
	fn format_string_percent_escapes() {
		let args = vec!["foo".to_string()];
		assert_eq!(TextGeneratorV3::format_string("100%%", &args), "100%");
		assert_eq!(TextGeneratorV3::format_string("%%1", &args), "%1");
		// Lone % before non-digit/non-% is copied verbatim.
		assert_eq!(TextGeneratorV3::format_string("%x %", &args), "%x %");
		// Trailing % is copied verbatim.
		assert_eq!(TextGeneratorV3::format_string("end%", &args), "end%");
	}

	#[test]
	fn format_string_out_of_int_range_fails_to_zero() {
		let args = vec!["foo".to_string()];
		assert_eq!(
			TextGeneratorV3::format_string("%99999999999999999999", &args),
			""
		);
		assert_eq!(TextGeneratorV3::format_string("%2147483648", &args), "");
		assert_eq!(TextGeneratorV3::format_string("%2147483647", &args), "");
	}

	#[test]
	fn format_string_multidigit_and_reuse() {
		let args = vec!["a".to_string(), "b".to_string(), "c".to_string()];
		// %10 parses as index 10 (out of range with 3 args) -> empty.
		assert_eq!(TextGeneratorV3::format_string("%10", &args), "");
		assert_eq!(TextGeneratorV3::format_string("%2%2%2", &args), "bbb");
	}

	#[test]
	fn layout_request_uses_olive_html_and_96dpi() {
		let mut row = NodeValueRow::default();
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("<p>Hi</p>".to_string()),
		);
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([400.0, 300.0]),
		);
		// The legacy path is driven explicitly: the backend state is a
		// process-global that other tests install hooks into.
		let req = TextGeneratorV3::layout_request_path(false, &row);
		assert_eq!(req.text, "<p>Hi</p>");
		assert_eq!(req.mode, TextLayoutMode::OliveHtml);
		assert_eq!(req.dots_per_meter, 3780);
		assert_eq!(req.wrap_width, 400.0);
	}

	#[test]
	fn layout_request_plain_text_path_uses_structured_inputs() {
		let mut row = NodeValueRow::default();
		row.insert(
			PLAIN_TEXT_INPUT.to_string(),
			NodeValue::Text("hello".to_string()),
		);
		row.insert(
			FONT_FAMILY_INPUT.to_string(),
			NodeValue::StrCombo("Noto Sans".to_string()),
		);
		row.insert(FONT_SIZE_INPUT.to_string(), NodeValue::Float(48.0));
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([400.0, 300.0]),
		);
		// The row also carries legacy HTML: the plain path must ignore it.
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("<p>legacy</p>".to_string()),
		);
		let req = TextGeneratorV3::layout_request_path(true, &row);
		assert_eq!(req.text, "hello");
		assert_eq!(req.mode, TextLayoutMode::PlainText);
		assert_eq!(req.font_family, "Noto Sans");
		assert_eq!(req.font_size_pt, 48.0);
		assert_eq!(req.dots_per_meter, 3780);
		assert_eq!(req.wrap_width, 400.0);
	}

	#[test]
	fn job_text_prefers_the_row_value() {
		let (core, _behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(
			PLAIN_TEXT_INPUT.to_string(),
			NodeValue::Text("row plain".to_string()),
		);
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("row html".to_string()),
		);
		assert_eq!(
			TextGeneratorV3::job_text(true, &core, &row, Rational::new(0, 1)),
			"row plain"
		);
		assert_eq!(
			TextGeneratorV3::job_text(false, &core, &row, Rational::new(0, 1)),
			"row html"
		);
	}

	#[test]
	fn job_text_falls_back_to_the_core_value() {
		let (mut core, _behavior) = create();
		core.set_standard_value(
			PLAIN_TEXT_INPUT,
			-1,
			NodeValue::Text("core plain".to_string()),
		);
		core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("core html".to_string()));
		let row = NodeValueRow::default();
		assert_eq!(
			TextGeneratorV3::job_text(true, &core, &row, Rational::new(0, 1)),
			"core plain"
		);
		assert_eq!(
			TextGeneratorV3::job_text(false, &core, &row, Rational::new(0, 1)),
			"core html"
		);
	}

	#[test]
	fn base_and_draw_offsets() {
		let size = [400.0, 300.0];
		let base = TextGeneratorV3::base_offset([0.0, 0.0], size, 1920, 1080);
		assert_eq!(base, (760.0, 390.0));
		// Top: no delta; middle/bottom use double math on doc.height.
		assert_eq!(
			TextGeneratorV3::draw_offset(VerticalAlignment::Top, base, size, 100.0),
			base
		);
		assert_eq!(
			TextGeneratorV3::draw_offset(VerticalAlignment::Middle, base, size, 100.0),
			(base.0, base.1 + 150.0 - 50.0)
		);
		assert_eq!(
			TextGeneratorV3::draw_offset(VerticalAlignment::Bottom, base, size, 100.0),
			(base.0, base.1 + 300.0 - 100.0)
		);
	}

	/// Serializes the tests that install a process-global text backend;
	/// shared with the `textbackend` module's own tests, so no test
	/// observes another module's install.
	use crate::nodes::textbackend::TEST_BACKEND_LOCK as BACKEND_LOCK;

	/// Measure hook for tests that only need "a backend is installed".
	fn noop_measure(_req: &TextLayoutRequest) -> TextLayoutSize {
		TextLayoutSize::default()
	}

	#[test]
	fn measure_without_backend_returns_zero_size() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(None, None);
		let mut row = NodeValueRow::default();
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("<p>Hi</p>".to_string()),
		);
		let (_req, doc) = TextGeneratorV3::measure_and_layout(&row);
		assert_eq!(doc.width, 0.0);
		assert_eq!(doc.height, 0.0);
	}

	#[test]
	fn value_uses_plain_text_when_a_backend_is_installed() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), None);
		assert!(TextGeneratorV3::plain_text_path());

		// The row carries an empty legacy HTML only: the non-empty plain
		// text default is what makes the job push, so this fails on the
		// legacy path (empty text, no base -> empty table).
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(h)) if h.is_null()
		));

		// The public layout request follows the installed backend.
		let mut req_row = NodeValueRow::default();
		req_row.insert(
			PLAIN_TEXT_INPUT.to_string(),
			NodeValue::Text("hi".to_string()),
		);
		assert_eq!(
			TextGeneratorV3::layout_request(&req_row).mode,
			TextLayoutMode::PlainText
		);

		crate::nodes::textbackend::set_text_backends(None, None);
		assert!(!TextGeneratorV3::plain_text_path());
	}

	#[test]
	fn value_pushes_job_when_text_nonempty() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(None, None);
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("<p>Hi</p>".to_string()),
		);
		row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(false));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(h)) if h.is_null()
		));
	}

	#[test]
	fn value_expands_args_from_row() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(None, None);
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("Hello %1".to_string()),
		);
		row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(true));
		row.insert(ARGS_INPUT.to_string(), NodeValue::Text("World".to_string()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(h)) if h.is_null()
		));
		// The expanded text is carried by the (deferred) job, which has no
		// payload here; the expansion math itself is covered by
		// format_string tests.
	}

	#[test]
	fn value_passes_base_through_when_text_empty() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
		row.insert(
			crate::nodes::generatorwithmerge::BASE_INPUT.to_string(),
			NodeValue::Texture(crate::handle::CHandle::null()),
		);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(_))
		));
	}

	#[test]
	fn value_pushes_nothing_when_text_empty_and_no_base() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		// Both text inputs are emptied explicitly: which one `value` reads
		// depends on the process-global backend state, which the installer
		// tests change and restore concurrently.
		row.insert(PLAIN_TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn gizmo_activation_toggles_use_args() {
		let (mut core, mut behavior) = create();
		let node = behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TextGeneratorV3>()
			.unwrap();
		node.gizmo_activated(&mut core);
		assert_eq!(
			core.standard_value(USE_ARGS_INPUT, -1),
			NodeValue::Boolean(false)
		);
		assert!(node.dont_emit_valign);
		node.gizmo_deactivated(&mut core);
		assert_eq!(
			core.standard_value(USE_ARGS_INPUT, -1),
			NodeValue::Boolean(true)
		);
		assert!(node.dont_emit_valign);
	}

	#[test]
	fn set_vertical_alignment_undoable_maps_through_gizmo_alignment() {
		let (mut core, mut behavior) = create();
		let node = behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<TextGeneratorV3>()
			.unwrap();
		// Gizmo vcenter (2) maps back to Middle (1).
		node.set_vertical_alignment_undoable(&mut core, 2);
		assert_eq!(
			core.standard_value(VERTICAL_ALIGNMENT_INPUT, -1),
			NodeValue::Combo(1)
		);
	}

	#[test]
	fn generate_frame_is_documented_noop() {
		let (core, behavior) = create();
		let mut frame = crate::handle::CHandle::null();
		behavior.generate_frame(&core, &mut frame, Rational::new(0, 1));
		assert!(frame.is_null());
	}

	#[test]
	fn duplicate_copies_node() {
		let (_core, behavior) = create();
		let copy = behavior.duplicate(&_core).unwrap();
		assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.text3");
		assert_eq!(copy.name(), "Text");
	}

	#[test]
	fn redesign_inputs_have_defaults_and_flags() {
		let (core, _behavior) = create();

		let plain = core.get_input(PLAIN_TEXT_INPUT).unwrap();
		assert_eq!(plain.value_type, ValueType::Text);
		assert_eq!(
			plain.default,
			NodeValue::Text(DEFAULT_PLAIN_TEXT.to_string())
		);

		let family = core.get_input(FONT_FAMILY_INPUT).unwrap();
		assert_eq!(family.value_type, ValueType::StrCombo);
		assert_eq!(family.default, NodeValue::StrCombo(String::new()));

		let size = core.get_input(FONT_SIZE_INPUT).unwrap();
		assert_eq!(size.value_type, ValueType::Float);
		assert_eq!(size.default, NodeValue::Float(72.0));
		assert!(size
			.properties
			.iter()
			.any(|(k, v)| k == "min" && v == &NodeValue::Float(1.0)));

		let outline = core.get_input(OUTLINE_ENABLED_INPUT).unwrap();
		assert_eq!(outline.value_type, ValueType::Boolean);
		assert_eq!(outline.default, NodeValue::Boolean(false));

		let outline_color = core.get_input(OUTLINE_COLOR_INPUT).unwrap();
		assert_eq!(outline_color.value_type, ValueType::Color);
		assert_eq!(
			outline_color.default,
			NodeValue::Color([0.0, 0.0, 0.0, 1.0])
		);

		let outline_width = core.get_input(OUTLINE_WIDTH_INPUT).unwrap();
		assert_eq!(outline_width.value_type, ValueType::Float);
		assert_eq!(outline_width.default, NodeValue::Float(2.0));
		assert!(outline_width
			.properties
			.iter()
			.any(|(k, v)| k == "min" && v == &NodeValue::Float(0.0)));

		let glow = core.get_input(GLOW_ENABLED_INPUT).unwrap();
		assert_eq!(glow.value_type, ValueType::Boolean);
		assert_eq!(glow.default, NodeValue::Boolean(false));

		let glow_color = core.get_input(GLOW_COLOR_INPUT).unwrap();
		assert_eq!(glow_color.value_type, ValueType::Color);
		assert_eq!(glow_color.default, NodeValue::Color([1.0, 1.0, 0.0, 1.0]));

		let glow_radius = core.get_input(GLOW_RADIUS_INPUT).unwrap();
		assert_eq!(glow_radius.value_type, ValueType::Float);
		assert_eq!(glow_radius.default, NodeValue::Float(8.0));
		assert!(glow_radius
			.properties
			.iter()
			.any(|(k, v)| k == "min" && v == &NodeValue::Float(0.0)));

		// Every redesign input is user-facing (none hidden).
		for id in [
			PLAIN_TEXT_INPUT,
			FONT_FAMILY_INPUT,
			FONT_SIZE_INPUT,
			OUTLINE_ENABLED_INPUT,
			OUTLINE_COLOR_INPUT,
			OUTLINE_WIDTH_INPUT,
			GLOW_ENABLED_INPUT,
			GLOW_COLOR_INPUT,
			GLOW_RADIUS_INPUT,
		] {
			assert_eq!(
				core.get_input(id).unwrap().flags & crate::input::flags::HIDDEN,
				0
			);
		}

		// The legacy input keeps its default and vieweronly property, and
		// is now hidden.
		let legacy = core.get_input(TEXT_INPUT).unwrap();
		assert_eq!(
			legacy.default,
			NodeValue::Text(LEGACY_DEFAULT_TEXT_HTML.to_string())
		);
		assert_ne!(legacy.flags & crate::input::flags::HIDDEN, 0);
		assert!(legacy
			.properties
			.iter()
			.any(|(k, v)| k == "vieweronly" && v == &NodeValue::Boolean(true)));

		// The shape base still has no color input of its own
		// (ShapeNodeBase(false)); the REDESIGN wave-3 font color input
		// takes that exact slot (white default).
		assert_eq!(
			core.get_input(crate::nodes::shapenodebase::COLOR_INPUT)
				.unwrap()
				.default,
			NodeValue::Color([1.0, 1.0, 1.0, 1.0])
		);
	}

	#[test]
	fn strip_html_to_plain_drops_tags() {
		assert_eq!(strip_html_to_plain("<p>a</p><p>b</p>"), "ab");
		assert_eq!(strip_html_to_plain("<br>"), "");
		assert_eq!(strip_html_to_plain("<p></p>"), "");
		assert_eq!(strip_html_to_plain("a<br>b"), "ab");
		// Whitespace is neither collapsed nor trimmed.
		assert_eq!(strip_html_to_plain("<p>a b</p>"), "a b");
		// A complete tag drops only itself; the text around it stays.
		assert_eq!(strip_html_to_plain("<p>abc"), "abc");
		// An unterminated tag swallows the remainder.
		assert_eq!(strip_html_to_plain("a<b"), "a");
		assert_eq!(strip_html_to_plain("<p"), "");
	}

	#[test]
	fn strip_html_to_plain_decodes_entities() {
		assert_eq!(
			strip_html_to_plain("a &amp;&lt;&gt;&quot;&#39;&nbsp;b"),
			"a &<>\"' b"
		);
		// A bare '&' and unknown entities are kept verbatim, one pass only.
		assert_eq!(strip_html_to_plain("a & b &fake; c"), "a & b &fake; c");
		assert_eq!(strip_html_to_plain("&amp;amp;"), "&amp;");
		// Entities are decoded after tags are dropped: an encoded tag stays
		// literal text.
		assert_eq!(strip_html_to_plain("&lt;p&gt;"), "<p>");
	}

	#[test]
	fn migrate_legacy_html_moves_stripped_text_once() {
		let (mut core, _behavior) = create();
		core.set_standard_value(
			TEXT_INPUT,
			-1,
			NodeValue::Text("<p>Hello <b>World</b></p>".to_string()),
		);
		assert!(migrate_legacy_html(&mut core));
		assert_eq!(
			core.standard_value(PLAIN_TEXT_INPUT, -1),
			NodeValue::Text("Hello World".to_string())
		);
		// The legacy value is left untouched and further calls are no-ops.
		assert_eq!(
			core.standard_value(TEXT_INPUT, -1),
			NodeValue::Text("<p>Hello <b>World</b></p>".to_string())
		);
		assert!(!migrate_legacy_html(&mut core));
		assert_eq!(
			core.standard_value(PLAIN_TEXT_INPUT, -1),
			NodeValue::Text("Hello World".to_string())
		);
	}

	#[test]
	fn migrate_legacy_html_leaves_defaults_and_edits_alone() {
		let (mut core, _behavior) = create();
		// A node at its defaults: the legacy default HTML is not migrated
		// (a new node must keep the redesign default across a save/load).
		assert!(!migrate_legacy_html(&mut core));
		assert_eq!(
			core.standard_value(PLAIN_TEXT_INPUT, -1),
			NodeValue::Text(DEFAULT_PLAIN_TEXT.to_string())
		);

		// An edited plain text is never overwritten.
		core.set_standard_value(PLAIN_TEXT_INPUT, -1, NodeValue::Text("mine".to_string()));
		core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("<p>legacy</p>".to_string()));
		assert!(!migrate_legacy_html(&mut core));
		assert_eq!(
			core.standard_value(PLAIN_TEXT_INPUT, -1),
			NodeValue::Text("mine".to_string())
		);

		// A legacy HTML with no text content migrates nothing.
		core.set_standard_value(PLAIN_TEXT_INPUT, -1, NodeValue::Text(String::new()));
		core.set_standard_value(TEXT_INPUT, -1, NodeValue::Text("<p></p>".to_string()));
		assert!(!migrate_legacy_html(&mut core));
		assert_eq!(
			core.standard_value(PLAIN_TEXT_INPUT, -1),
			NodeValue::Text(String::new())
		);
	}

	#[test]
	fn migrate_legacy_html_noop_without_plain_text_input() {
		// A pre-redesign core (no plain_text_in at all) must not panic.
		let mut core = NodeCore::new();
		core.add_input(crate::input::Input::new(
			TEXT_INPUT,
			ValueType::Text,
			NodeValue::Text("<p>x</p>".to_string()),
		));
		assert!(!migrate_legacy_html(&mut core));
	}

	/// Render hook for the post-process tests: paints the middle half of
	/// the target (`x`, `y` in `[dim / 4, 3 * dim / 4)`) solid white — a
	/// coverage block whose dilation and box blur are exactly computable.
	fn solid_render(
		_req: &TextLayoutRequest,
		_transform: &TextRenderTransform,
		target: TextRenderTarget,
	) {
		if target.channel_count != 4 {
			return;
		}
		let stride = target.linesize_bytes as usize;
		let (w, h) = (target.width as usize, target.height as usize);
		for y in h / 4..3 * h / 4 {
			for x in w / 4..3 * w / 4 {
				let at = y * stride + x * 4;
				target.data[at..at + 4].copy_from_slice(&[255; 4]);
			}
		}
	}

	/// The evaluation row of the post-process tests: a 16x16 raster with a
	/// 2-pixel black outline and/or a 4-pixel yellow glow.
	fn post_row(outline: bool, glow: bool) -> NodeValueRow {
		let mut row = NodeValueRow::new();
		row.insert(
			PLAIN_TEXT_INPUT.to_string(),
			NodeValue::Text("X".to_string()),
		);
		row.insert(USE_ARGS_INPUT.to_string(), NodeValue::Boolean(false));
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([16.0, 16.0]),
		);
		row.insert(
			OUTLINE_ENABLED_INPUT.to_string(),
			NodeValue::Boolean(outline),
		);
		row.insert(
			OUTLINE_COLOR_INPUT.to_string(),
			NodeValue::Color([0.0, 0.0, 0.0, 1.0]),
		);
		row.insert(OUTLINE_WIDTH_INPUT.to_string(), NodeValue::Float(2.0));
		row.insert(GLOW_ENABLED_INPUT.to_string(), NodeValue::Boolean(glow));
		row.insert(
			GLOW_COLOR_INPUT.to_string(),
			NodeValue::Color([1.0, 1.0, 0.0, 1.0]),
		);
		row.insert(GLOW_RADIUS_INPUT.to_string(), NodeValue::Float(4.0));
		row
	}

	/// Evaluate [`TextGeneratorV3::value`] and return the texture handle it
	/// pushed.
	fn push_value(core: &NodeCore, behavior: &dyn NodeBehavior, row: &NodeValueRow) -> CHandle {
		let mut table = NodeValueTable::default();
		behavior.value(core, row, Rational::new(0, 1), &mut table);
		match table.get(ValueType::Texture) {
			Some(NodeValue::Texture(handle)) => *handle,
			other => panic!("expected a texture row, got {other:?}"),
		}
	}

	/// The job payload boxed by a deferred texture handle.
	fn job_of(handle: &CHandle) -> &ShaderJobPayload {
		unsafe { crate::jobs::shader_job(handle) }
			.expect("handle carries a ShaderJobPayload")
	}

	/// The shader id of the pass a deferred texture handle runs.
	fn shader_id_of(handle: &CHandle) -> &str {
		&job_of(handle).shader_id
	}

	/// A texture-typed job param (the effect input or a merge layer).
	fn param_texture<'a>(handle: &'a CHandle, input: &str) -> &'a CHandle {
		match job_of(handle).params.get(input) {
			Some(NodeValue::Texture(tex)) => tex,
			other => panic!("param {input:?} is not a texture: {other:?}"),
		}
	}

	/// A float job param.
	fn param_float(handle: &CHandle, input: &str) -> f64 {
		match job_of(handle).params.get(input) {
			Some(NodeValue::Float(f)) => *f,
			other => panic!("param {input:?} is not a float: {other:?}"),
		}
	}

	/// A color job param.
	fn param_color(handle: &CHandle, input: &str) -> [f64; 4] {
		match job_of(handle).params.get(input) {
			Some(NodeValue::Color(c)) => *c,
			other => panic!("param {input:?} is not a color: {other:?}"),
		}
	}

	/// A vec2 job param.
	fn param_vec2(handle: &CHandle, input: &str) -> [f64; 2] {
		match job_of(handle).params.get(input) {
			Some(NodeValue::Vec2(v)) => *v,
			other => panic!("param {input:?} is not a vec2: {other:?}"),
		}
	}

	/// Identity of the refcounted box behind a handle: every clone of a
	/// job param addrefs the same box, so equal pointers mean "the same
	/// texture was fed to both passes".
	fn job_ptr(handle: &CHandle) -> usize {
		handle.ctx as usize
	}

	/// Assert `handle` boxes the 16x16 CPU coverage frame the rasterizer
	/// staging-allocates (not a shader job).
	fn assert_cpu_texture(handle: &CHandle) {
		match unsafe { crate::handle::get_checked::<Texture>(handle) } {
			Some(Texture::Cpu(frame)) => assert_eq!((frame.width, frame.height), (16, 16)),
			other => panic!("expected a CPU coverage frame, got {other:?}"),
		}
	}

	/// One pixel of a CPU coverage frame's F32 RGBA data.
	fn pixel_of(handle: &CHandle, x: usize, y: usize) -> [f32; 4] {
		let Some(Texture::Cpu(frame)) = (unsafe { crate::handle::get_checked::<Texture>(handle) })
		else {
			panic!("expected a CPU coverage frame");
		};
		let stride = frame.linesize_bytes() as usize;
		let at = y * stride + x * 16;
		let mut out = [0f32; 4];
		for (c, v) in out.iter_mut().enumerate() {
			*v = f32::from_le_bytes(frame.data[at + c * 4..at + c * 4 + 4].try_into().unwrap());
		}
		out
	}

	#[test]
	fn post_job_off_still_rasterizes_the_plain_text() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
		let (core, behavior) = create();
		let row = post_row(false, false);
		let handle = push_value(&core, behavior.as_ref(), &row);
		crate::nodes::textbackend::set_text_backends(None, None);
		// Both passes off with a backend installed: the plain (tinted)
		// raster, not the pre-backend deferred null job.
		assert_cpu_texture(&handle);
		assert_eq!(pixel_of(&handle, 8, 8), [1.0, 1.0, 1.0, 1.0]);
		assert_eq!(pixel_of(&handle, 0, 0), [0.0, 0.0, 0.0, 0.0]);
	}

	#[test]
	fn font_color_tints_the_raster_premultiplied() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
		let (core, behavior) = create();
		let mut row = post_row(false, false);
		row.insert(
			COLOR_INPUT.to_string(),
			NodeValue::Color([1.0, 0.0, 0.0, 0.5]),
		);
		let handle = push_value(&core, behavior.as_ref(), &row);
		crate::nodes::textbackend::set_text_backends(None, None);
		assert_cpu_texture(&handle);
		// The white coverage scales per channel, alpha included.
		assert_eq!(pixel_of(&handle, 8, 8), [1.0, 0.0, 0.0, 0.5]);
		assert_eq!(pixel_of(&handle, 0, 0), [0.0, 0.0, 0.0, 0.0]);
	}

	#[test]
	fn outline_chain_dilates_then_colorizes_over_the_text() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
		let (core, behavior) = create();
		let row = post_row(true, false);
		let handle = push_value(&core, behavior.as_ref(), &row);
		crate::nodes::textbackend::set_text_backends(None, None);

		// The pushed chain is the stroke merged with the text on top: the
		// text is the merge's top (`blend_in`) layer, its CPU coverage
		// raster the head of that branch.
		assert_eq!(shader_id_of(&handle), "mrg");
		let stroke = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
		let text = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
		assert_cpu_texture(text);

		// The stroke is the colorized dilation of the raster.
		assert_eq!(shader_id_of(stroke), OUTLINE_COLORIZE_SHADER_ID);
		assert_eq!(
			param_color(stroke, OUTLINE_COLOR_INPUT),
			[0.0, 0.0, 0.0, 1.0]
		);
		assert_eq!(param_vec2(stroke, RESOLUTION_INPUT), [16.0, 16.0]);
		let dilated = param_texture(stroke, POST_TEXTURE_INPUT);
		assert_eq!(shader_id_of(dilated), OUTLINE_DILATE_SHADER_ID);
		assert_eq!(job_of(dilated).iterations, 1);
		assert_eq!(param_float(dilated, OUTLINE_WIDTH_INPUT), 2.0);
		assert_eq!(param_vec2(dilated, RESOLUTION_INPUT), [16.0, 16.0]);
		assert_eq!(
			job_ptr(param_texture(dilated, POST_TEXTURE_INPUT)),
			job_ptr(text)
		);
	}

	#[test]
	fn glow_chain_blurs_once_per_axis_then_colorizes() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
		let (core, behavior) = create();
		let row = post_row(false, true);
		let handle = push_value(&core, behavior.as_ref(), &row);
		crate::nodes::textbackend::set_text_backends(None, None);

		// Glow only: the glow is merged beneath the bare text.
		assert_eq!(shader_id_of(&handle), "mrg");
		let glow = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
		let text = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
		assert_cpu_texture(text);

		assert_eq!(shader_id_of(glow), GLOW_COLORIZE_SHADER_ID);
		assert_eq!(param_color(glow, GLOW_COLOR_INPUT), [1.0, 1.0, 0.0, 1.0]);
		assert_eq!(param_vec2(glow, RESOLUTION_INPUT), [16.0, 16.0]);
		let blurred = param_texture(glow, POST_TEXTURE_INPUT);
		assert_eq!(shader_id_of(blurred), GLOW_BLUR_SHADER_ID);
		// Two iterations: one per axis (horizontal, then vertical).
		assert_eq!(job_of(blurred).iterations, 2);
		assert_eq!(param_float(blurred, GLOW_RADIUS_INPUT), 4.0);
		assert_eq!(param_vec2(blurred, RESOLUTION_INPUT), [16.0, 16.0]);
		assert_eq!(
			job_ptr(param_texture(blurred, POST_TEXTURE_INPUT)),
			job_ptr(text)
		);
	}

	#[test]
	fn outline_and_glow_glow_the_stroke() {
		let _guard = BACKEND_LOCK.lock().unwrap();
		crate::nodes::textbackend::set_text_backends(Some(noop_measure), Some(solid_render));
		let (core, behavior) = create();
		let row = post_row(true, true);
		let handle = push_value(&core, behavior.as_ref(), &row);
		crate::nodes::textbackend::set_text_backends(None, None);

		// Both on: the glow is drawn over the stroke (which is drawn over
		// the text), and it samples the stroke itself — the blur's input
		// is the stroke's merge job, not the bare coverage raster.
		assert_eq!(shader_id_of(&handle), "mrg");
		let glow = param_texture(&handle, crate::nodes::merge::BASE_INPUT);
		let stroke = param_texture(&handle, crate::nodes::merge::BLEND_INPUT);
		assert_eq!(shader_id_of(stroke), "mrg");

		let stroke_colorized = param_texture(stroke, crate::nodes::merge::BASE_INPUT);
		assert_eq!(shader_id_of(stroke_colorized), OUTLINE_COLORIZE_SHADER_ID);
		assert_cpu_texture(param_texture(stroke, crate::nodes::merge::BLEND_INPUT));

		let blurred = param_texture(glow, POST_TEXTURE_INPUT);
		assert_eq!(shader_id_of(blurred), GLOW_BLUR_SHADER_ID);
		assert_eq!(
			job_ptr(param_texture(blurred, POST_TEXTURE_INPUT)),
			job_ptr(stroke)
		);
	}
}
