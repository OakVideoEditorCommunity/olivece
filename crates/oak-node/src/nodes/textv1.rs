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

//! Legacy text generator v1 (C++
//! `src/node/src/generator/text/textv1.{h,cpp}`,
//! `olive::TextGeneratorV1`).
//!
//! FONT/RASTER BACKEND DEPENDENCY — DELIBERATELY UNDECIDED:
//! The C++ does NOT link any font/raster library directly (no freetype,
//! stb, harfbuzz, etc. anywhere in the tree). Text layout/rasterization
//! was historically Qt's rich text stack (`QTextDocument` +
//! `QAbstractTextDocumentLayout` + `QPainter` into a
//! `QImage::Format_Grayscale8` coverage buffer); it now runs behind the
//! facade-installed hooks in [`super::textbackend`]. No Rust font crate
//! is chosen here on purpose.

use crate::factory::NodeMeta;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{NodeValue, NodeValueRow, NodeValueTable};
use oak_core::Rational;

use super::textbackend::{TextLayoutMode, TextLayoutRequest, TextLayoutSize};

/// Text input id (C++ `k_text_input`). Type: text; default
/// `"Sample Text"`.
pub const TEXT_INPUT: &str = "text_in";

/// HTML toggle input id (C++ `k_html_input`). Type: boolean; default
/// `false`.
pub const HTML_INPUT: &str = "html_in";

/// Text color input id (C++ `k_color_input`). Type: color; default
/// `Color(1.0, 1.0, 1.0)` (white).
pub const COLOR_INPUT: &str = "color_in";

/// Vertical alignment input id (C++ `k_v_align_input`). Type: combo;
/// default `1` (center); combo strings: "Top", "Center", "Bottom".
pub const V_ALIGN_INPUT: &str = "valign_in";

/// Font family input id (C++ `k_font_input`). Type: font (C++
/// `NodeValue::k_font` — no dedicated [`crate::value::ValueType`]
/// variant, travels as text); no default.
pub const FONT_INPUT: &str = "font_in";

/// Font size input id (C++ `k_font_size_input`). Type: float; default
/// `72.0`.
pub const FONT_SIZE_INPUT: &str = "font_size_in";

/// Legacy text generator v1. The C++ class has no own private members
/// (inputs/caches live in [`NodeCore`]), so this is a unit struct.
pub struct TextGeneratorV1;

/// `Variant::to_string()` for the text/font inputs (Text payload, with a
/// numeric fallback for mis-typed connections).
fn to_text(v: &NodeValue) -> String {
	match v {
		NodeValue::Text(s) => s.clone(),
		other => other.to_double().to_string(),
	}
}

impl TextGeneratorV1 {
	/// Build the C++ `TextLayoutRequest` (textv1.cpp `generate_frame()`):
	/// the text and font from the input row, HTML mode with newlines
	/// replaced by `<br>` when `html_in` is set, horizontal centering on,
	/// and the wrap width at 80% of the frame width (the "title safe"
	/// area — `(width / 10) * 8`). The measure backend is not consulted
	/// here.
	#[allow(dead_code)] // C++ parity helper; the tests drive it.
	pub fn layout_request(row: &NodeValueRow, frame_width: i32) -> TextLayoutRequest {
		let text = row
			.get(TEXT_INPUT)
			.map(to_text)
			.unwrap_or_else(|| String::new());
		let html = matches!(row.get(HTML_INPUT), Some(NodeValue::Boolean(true)));
		let mut mode = TextLayoutMode::PlainText;
		let text = if html {
			// QTextDocument::setHtml() doesn't translate newlines, so they
			// were replaced with <br> tags first.
			mode = TextLayoutMode::Html;
			text.replace('\n', "<br>")
		} else {
			text
		};

		let tenth_of_width = frame_width / 10;
		TextLayoutRequest {
			text,
			mode,
			font_family: row.get(FONT_INPUT).map(to_text).unwrap_or_else(String::new),
			font_size_pt: row
				.get(FONT_SIZE_INPUT)
				.map(NodeValue::to_double)
				.unwrap_or(0.0),
			dots_per_meter: 0,
			wrap_width: (tenth_of_width * 8) as f64,
			center_horizontally: true,
		}
	}

	/// C++ draw offsets (textv1.cpp `generate_frame()`): x is pushed 10%
	/// inwards for the title-safe area; the vertical offset depends on the
	/// valign combo (top: 10% top margin; center: frame center; bottom:
	/// 10% bottom margin). The C++ math is integer (`width()/10`,
	/// `height()/2 - doc_height/2`, ...), mirrored here.
	#[allow(dead_code)]
	pub fn draw_offsets(
		valign: i32,
		frame_width: i32,
		frame_height: i32,
		doc_height: i32,
	) -> (f64, f64) {
		let tenth_of_width = frame_width / 10;
		let offset_x = tenth_of_width as f64;
		let offset_y = match valign {
			// k_vertical_align_top: push 10% inwards for the title-safe area.
			0 => (frame_height / 10) as f64,
			// k_vertical_align_center.
			1 => (frame_height / 2 - doc_height / 2) as f64,
			// k_vertical_align_bottom: 10% bottom margin.
			2 => (frame_height - doc_height - frame_height / 10) as f64,
			_ => 0.0,
		};
		(offset_x, offset_y)
	}

	/// The layout/measure control flow of the C++ `generate_frame()` with
	/// the backend hooks: build the request, measure via the installed
	/// measure backend (zero size when none is installed — the documented
	/// no-backend fallback), and compute the draw offsets for the given
	/// frame size.
	///
	/// The render step and the alpha transplant need the frame's pixel
	/// buffer, which the Rust frame handle does not expose; they are not
	/// representable here (`// CPP-PARITY: textv1.cpp` `generate_frame`).
	#[allow(dead_code)]
	pub fn measure_and_layout(
		row: &NodeValueRow,
		frame_width: i32,
		frame_height: i32,
	) -> (TextLayoutRequest, TextLayoutSize, (f64, f64)) {
		let req = Self::layout_request(row, frame_width);
		let doc = match super::textbackend::text_measure_backend() {
			Some(measure) => measure(&req),
			None => TextLayoutSize::default(),
		};
		let valign = row
			.get(V_ALIGN_INPUT)
			.map(NodeValue::to_double)
			.unwrap_or(0.0) as i32;
		let offsets = Self::draw_offsets(valign, frame_width, frame_height, doc.height as i32);
		(req, doc, offsets)
	}
}

impl NodeBehavior for TextGeneratorV1 {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Text (Legacy)"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.textgenerator"
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
	/// "Text", `html_in` -> "Enable HTML", `font_in` -> "Font",
	/// `font_size_in` -> "Font Size", `color_in` -> "Color",
	/// `valign_in` -> "Vertical Align" (combo strings Top/Center/
	/// Bottom — set by the C++ `set_combo_box_strings`, which has no
	/// trait surface here).
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXT_INPUT => "Text",
			HTML_INPUT => "Enable HTML",
			COLOR_INPUT => "Color",
			V_ALIGN_INPUT => "Vertical Align",
			FONT_INPUT => "Font",
			FONT_SIZE_INPUT => "Font Size",
			_ => id,
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `valign_in` -> "Top", "Center",
	/// "Bottom".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			V_ALIGN_INPUT => vec!["Top", "Center", "Bottom"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): if the text input is
	/// non-empty, push a texture generate job at the global video
	/// params; otherwise push nothing.
	///
	/// The Rust model has no generate-job payload: the job case pushes a
	/// null texture handle marking a renderer-deferred generate job
	/// resolved via [`NodeBehavior::generate_frame`]
	/// (`// CPP-PARITY: textv1.cpp` `value()`).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &NodeValueRow,
		time: Rational,
		table: &mut NodeValueTable,
	) {
		let text = inputs.get(TEXT_INPUT).map(to_text).unwrap_or_else(|| {
			core.value_at_time(TEXT_INPUT, -1, time)
				.to_double()
				.to_string()
		});
		if !text.is_empty() {
			table.push(
				crate::value::ValueType::Texture,
				NodeValue::Texture(crate::handle::CHandle::null()),
				None,
			);
		}
	}

	/// Direct frame generation (C++ `generate_frame()`): rasterizes
	/// the text into a grayscale coverage buffer (32-bit-aligned
	/// scanlines) via the installed text backend — HTML mode replaces
	/// newlines with `<br>`, wrap width is 80% of frame width ("title
	/// safe"), horizontal centering is on, vertical offset depends on
	/// the valign combo (top: 10% margin; center: frame center; bottom:
	/// 10% bottom margin) — then transplants the coverage as alpha
	/// into the float frame multiplied by the color input. With no
	/// backend installed, warns once and leaves the frame empty.
	///
	/// The Rust frame is an opaque [`crate::handle::CHandle`]
	/// whose pixels cannot be read or written from this crate, so the
	/// body is a documented no-op; the layout/measure/offset control flow
	/// is ported in [`Self::layout_request`], [`Self::draw_offsets`] and
	/// [`Self::measure_and_layout`], and exercised by the tests.
	fn generate_frame(
		&self,
		core: &NodeCore,
		frame: &mut crate::handle::CHandle,
		time: Rational,
	) {
		let _ = (core, frame, time);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TextGeneratorV1))
	}
}

/// Constructor (C++ `TextGeneratorV1::TextGeneratorV1()`): adds
/// `text_in`, `html_in`, `color_in`, `valign_in`, `font_in` and
/// `font_size_in` with the defaults documented on the constants, and
/// sets the `dont_show_in_create_menu` flag.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	let mut core = NodeCore::new();

	core.add_input(crate::input::Input::new(
		TEXT_INPUT,
		crate::value::ValueType::Text,
		NodeValue::Text("Sample Text".to_string()),
	));
	core.add_input(crate::input::Input::new(
		HTML_INPUT,
		crate::value::ValueType::Boolean,
		NodeValue::Boolean(false),
	));
	core.add_input(crate::input::Input::new(
		COLOR_INPUT,
		crate::value::ValueType::Color,
		NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	));
	core.add_input(crate::input::Input::new(
		V_ALIGN_INPUT,
		crate::value::ValueType::Combo,
		NodeValue::Combo(1),
	));
	core.add_input(crate::input::Input::new(
		FONT_INPUT,
		crate::value::ValueType::Text,
		NodeValue::Text(String::new()),
	));
	core.add_input(crate::input::Input::new(
		FONT_SIZE_INPUT,
		crate::value::ValueType::Float,
		NodeValue::Float(72.0),
	));

	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;

	(core, Box::new(TextGeneratorV1))
}

/// Register this node type (C++ `k_text_generator_v1` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.textgenerator",
		name: "Text (Legacy)",
		categories: &[Category::Generator],
		create,
	});
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::NodeBehavior;
	use crate::value::{NodeValueTable, ValueType};
	use oak_core::Rational;

	#[test]
	fn input_names() {
		let n = TextGeneratorV1;
		assert_eq!(n.input_name(TEXT_INPUT), "Text");
		assert_eq!(n.input_name(HTML_INPUT), "Enable HTML");
		assert_eq!(n.input_name(COLOR_INPUT), "Color");
		assert_eq!(n.input_name(V_ALIGN_INPUT), "Vertical Align");
		assert_eq!(n.input_name(FONT_INPUT), "Font");
		assert_eq!(n.input_name(FONT_SIZE_INPUT), "Font Size");
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inputs() {
		let (core, behavior) = create();
		assert_eq!(
			behavior.type_id(),
			"org.olivevideoeditor.Olive.textgenerator"
		);
		assert_eq!(
			core.get_input(TEXT_INPUT).unwrap().value_type,
			ValueType::Text
		);
		assert_eq!(
			core.get_input(TEXT_INPUT).unwrap().default,
			NodeValue::Text("Sample Text".to_string())
		);
		assert_eq!(
			core.get_input(HTML_INPUT).unwrap().value_type,
			ValueType::Boolean
		);
		assert_eq!(
			core.get_input(COLOR_INPUT).unwrap().default,
			NodeValue::Color([1.0, 1.0, 1.0, 1.0])
		);
		assert_eq!(
			core.get_input(V_ALIGN_INPUT).unwrap().default,
			NodeValue::Combo(1)
		);
		assert_eq!(
			core.get_input(FONT_SIZE_INPUT).unwrap().default,
			NodeValue::Float(72.0)
		);
		assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
	}

	#[test]
	fn layout_request_plain_text() {
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hello".to_string()));
		row.insert(FONT_INPUT.to_string(), NodeValue::Text("Arial".to_string()));
		row.insert(FONT_SIZE_INPUT.to_string(), NodeValue::Float(48.0));
		row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(false));
		let req = TextGeneratorV1::layout_request(&row, 1920);
		assert_eq!(req.text, "Hello");
		assert_eq!(req.mode, TextLayoutMode::PlainText);
		assert_eq!(req.font_family, "Arial");
		assert_eq!(req.font_size_pt, 48.0);
		assert!(req.center_horizontally);
		// 1920 / 10 * 8 = 1536.
		assert_eq!(req.wrap_width, 1536.0);
	}

	#[test]
	fn layout_request_html_translates_newlines() {
		let mut row = NodeValueRow::default();
		row.insert(
			TEXT_INPUT.to_string(),
			NodeValue::Text("line1\nline2".to_string()),
		);
		row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(true));
		let req = TextGeneratorV1::layout_request(&row, 1280);
		assert_eq!(req.text, "line1<br>line2");
		assert_eq!(req.mode, TextLayoutMode::Html);
		// 1280 / 10 * 8 = 1024.
		assert_eq!(req.wrap_width, 1024.0);
	}

	#[test]
	fn draw_offsets_per_valign() {
		// Center (default of the v1 node): x = width/10, y = height/2.
		let (x, y) = TextGeneratorV1::draw_offsets(1, 1920, 1080, 100);
		assert_eq!(x, 192.0);
		assert_eq!(y, 540.0 - 50.0);
		// Top: y = height/10.
		let (x, y) = TextGeneratorV1::draw_offsets(0, 1920, 1080, 100);
		assert_eq!(x, 192.0);
		assert_eq!(y, 108.0);
		// Bottom: y = height - doc_height - height/10.
		let (_, y) = TextGeneratorV1::draw_offsets(2, 1920, 1080, 100);
		assert_eq!(y, 1080.0 - 100.0 - 108.0);
	}

	#[test]
	fn measure_without_backend_returns_zero_size() {
		crate::nodes::textbackend::set_text_backends(None, None);
		// No backend is installed in cargo test; the fallback is a zero
		// document size.
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hello".to_string()));
		row.insert(V_ALIGN_INPUT.to_string(), NodeValue::Combo(1));
		let (req, doc, offsets) = TextGeneratorV1::measure_and_layout(&row, 1920, 1080);
		assert_eq!(doc.width, 0.0);
		assert_eq!(doc.height, 0.0);
		assert_eq!(req.wrap_width, 1536.0);
		assert_eq!(offsets.0, 192.0);
		assert_eq!(offsets.1, 540.0);
	}

	#[test]
	fn value_pushes_job_when_text_nonempty() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		// The C++ pushes a deferred generate job; the Rust model pushes a
		// null texture handle marking that job.
		assert!(matches!(
			table.get(ValueType::Texture),
			Some(NodeValue::Texture(h)) if h.is_null()
		));
	}

	#[test]
	fn value_pushes_nothing_when_text_empty() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text(String::new()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
		assert!(table.is_empty());
	}

	#[test]
	fn generate_frame_is_documented_noop() {
		let (core, behavior) = create();
		let mut frame = crate::handle::CHandle::null();
		behavior.generate_frame(&core, &mut frame, Rational::new(0, 1));
		// The frame handle cannot be touched; it stays untouched (null).
		assert!(frame.is_null());
	}

	#[test]
	fn duplicate_copies_node() {
		let (_core, behavior) = create();
		let copy = behavior.duplicate(&_core).unwrap();
		assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.textgenerator");
		assert_eq!(copy.name(), "Text (Legacy)");
	}
}
