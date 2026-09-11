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

//! Legacy text generator v2 (C++
//! `src/node/src/generator/text/textv2.{h,cpp}`,
//! `olive::TextGeneratorV2`, derives from `ShapeNodeBase`).
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

use super::textbackend::{TextLayoutMode, TextLayoutRequest, TextLayoutSize, TextRenderTransform};

/// Text input id (C++ `k_text_input`). Type: text; default
/// `"Sample Text"`.
pub const TEXT_INPUT: &str = "text_in";

/// HTML toggle input id (C++ `k_html_input`). Type: boolean; default
/// `false`.
pub const HTML_INPUT: &str = "html_in";

/// Vertical alignment input id (C++ `k_v_align_input`). Type: combo;
/// default `0` (C++ `k_vertical_align_top`); combo strings: "Top",
/// "Center", "Bottom".
pub const V_ALIGN_INPUT: &str = "valign_in";

/// Font family input id (C++ `k_font_input`). Type: font (C++
/// `NodeValue::k_font` — no dedicated [`crate::value::ValueType`]
/// variant, travels as text); no default.
pub const FONT_INPUT: &str = "font_in";

/// Font size input id (C++ `k_font_size_input`). Type: float; default
/// `72.0`.
pub const FONT_SIZE_INPUT: &str = "font_size_in";

/// Legacy text generator v2. The C++ class has no own private members
/// — it inherits position/size/color inputs and the polygon gizmo from
/// the shape base (C++ `ShapeNodeBase`, modelled in
/// [`super::shapenodebase`]) and everything else lives in [`NodeCore`] —
/// so this is a unit struct.
pub struct TextGeneratorV2;

/// `Variant::to_string()` for the text/font inputs (Text payload, with a
/// numeric fallback for mis-typed connections).
fn to_text(v: &NodeValue) -> String {
	match v {
		NodeValue::Text(s) => s.clone(),
		other => other.to_double().to_string(),
	}
}

/// `Variant::to_vec2()` for the inherited position/size inputs.
#[allow(dead_code)] // C++ parity accessor used by the tests.
fn to_vec2(v: &NodeValue) -> [f64; 2] {
	match v {
		NodeValue::Vec2(a) => *a,
		other => [other.to_double(), 0.0],
	}
}

impl TextGeneratorV2 {
	/// Build the C++ `TextLayoutRequest` (textv2.cpp `generate_frame()`):
	/// the text and font from the input row, HTML mode with newlines
	/// replaced by `<br>` when `html_in` is set, 72 DPI
	/// (`dots_per_meter = 2835`), and the wrap width at the shape
	/// size X.
	#[allow(dead_code)]
	pub fn layout_request(row: &NodeValueRow) -> TextLayoutRequest {
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
		let size = row
			.get(crate::nodes::shapenodebase::SIZE_INPUT)
			.map(to_vec2)
			.unwrap_or([0.0, 0.0]);

		TextLayoutRequest {
			text,
			mode,
			font_family: row.get(FONT_INPUT).map(to_text).unwrap_or_else(String::new),
			font_size_pt: row
				.get(FONT_SIZE_INPUT)
				.map(NodeValue::to_double)
				.unwrap_or(0.0),
			dots_per_meter: 2835,
			wrap_width: size[0],
			center_horizontally: false,
		}
	}

	/// The C++ base offset (textv2.cpp `generate_frame()`): the shape
	/// position re-centered into frame space —
	/// `pos - size/2 + frame/2` (the frame halves are integer division in
	/// C++).
	#[allow(dead_code)]
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

	/// The C++ draw offset (textv2.cpp `generate_frame()`): the base
	/// offset plus the vertical alignment delta — top: none; center:
	/// `size.y/2 - doc_height/2` (the halving is integer on the
	/// `int(doc.height)`); bottom: `size.y - doc_height`.
	#[allow(dead_code)]
	pub fn draw_offset(
		valign: i32,
		base: (f64, f64),
		size: [f64; 2],
		doc_height: i32,
	) -> (f64, f64) {
		let (dx, mut dy) = base;
		match valign {
			// k_vertical_align_top: do nothing.
			0 => {}
			// k_vertical_align_center.
			1 => dy += size[1] / 2.0 - (doc_height / 2) as f64,
			// k_vertical_align_bottom.
			2 => dy += size[1] - doc_height as f64,
			_ => {}
		}
		(dx, dy)
	}

	/// The C++ `TextRenderTransform` (textv2.cpp `generate_frame()`):
	/// scale, the draw offset, and the clip rect at the base offset
	/// covering the shape size (set before the vertical-alignment
	/// translate in the C++).
	#[allow(dead_code)]
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
	/// the backend hooks: build the request, measure via the installed
	/// measure backend (zero size when none is installed — the documented
	/// no-backend fallback), and compute the base/draw offsets for the
	/// given frame size.
	///
	/// The render step and the alpha transplant need the frame's pixel
	/// buffer, which the Rust frame handle does not expose; they are not
	/// representable here (`// CPP-PARITY: textv2.cpp` `generate_frame`).
	#[allow(dead_code)]
	pub fn measure_and_layout(
		row: &NodeValueRow,
		frame_width: i32,
		frame_height: i32,
	) -> (TextLayoutRequest, TextLayoutSize, (f64, f64), (f64, f64)) {
		let req = Self::layout_request(row);
		let doc = match super::textbackend::text_measure_backend() {
			Some(measure) => measure(&req),
			None => TextLayoutSize::default(),
		};
		let size = row
			.get(crate::nodes::shapenodebase::SIZE_INPUT)
			.map(to_vec2)
			.unwrap_or([0.0, 0.0]);
		let pos = row
			.get(crate::nodes::shapenodebase::POSITION_INPUT)
			.map(to_vec2)
			.unwrap_or([0.0, 0.0]);
		let base = Self::base_offset(pos, size, frame_width, frame_height);
		let valign = row
			.get(V_ALIGN_INPUT)
			.map(NodeValue::to_double)
			.unwrap_or(0.0) as i32;
		let draw = Self::draw_offset(valign, base, size, doc.height as i32);
		(req, doc, base, draw)
	}
}

impl NodeBehavior for TextGeneratorV2 {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Text (Legacy)"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.text2"
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
	/// `font_size_in` -> "Font Size", `valign_in` -> "Vertical Align"
	/// (combo strings Top/Center/Bottom — set by the C++
	/// `set_combo_box_strings`, which has no trait surface here); the
	/// base class retranslate covers the inherited shape inputs and
	/// `base_in` ("Base").
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			TEXT_INPUT => "Text",
			HTML_INPUT => "Enable HTML",
			V_ALIGN_INPUT => "Vertical Align",
			FONT_INPUT => "Font",
			FONT_SIZE_INPUT => "Font Size",
			crate::nodes::generatorwithmerge::BASE_INPUT => "Base",
			_ => crate::nodes::shapenodebase::ShapeNodeBase::input_name(id),
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
	/// params forced to `PixelFormat::f32`; otherwise push nothing.
	///
	/// The Rust model has no generate-job payload: the job case pushes a
	/// null texture handle marking a renderer-deferred generate job
	/// resolved via [`NodeBehavior::generate_frame`]; the f32 forcing is
	/// renderer-side and has no representation here
	/// (`// CPP-PARITY: textv2.cpp` `value()`).
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
	/// the text into a grayscale coverage buffer via the installed
	/// text backend at 72 DPI (2835 dots/meter) — HTML mode replaces
	/// newlines with `<br>`, wrap width is the shape size X, base
	/// offset is the shape position re-centered into frame space,
	/// vertical draw offset depends on the valign combo (top: none;
	/// center: `size.y/2 - doc.height/2`; bottom: `size.y -
	/// doc.height`), and the clip rect is the shape rect at the base
	/// offset — then writes the coverage multiplied by the RGBA color
	/// into the float frame (SIMD path in C++, scalar fallback
	/// identical). With no backend installed, warns once and leaves
	/// the frame empty.
	///
	/// The Rust frame is an opaque [`crate::handle::CHandle`]
	/// whose pixels cannot be read or written from this crate, so the
	/// body is a documented no-op; the layout/measure/offset control flow
	/// is ported in [`Self::layout_request`], [`Self::base_offset`],
	/// [`Self::draw_offset`], [`Self::render_transform`] and
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
		Some(Box::new(TextGeneratorV2))
	}
}

/// Constructor (C++ `TextGeneratorV2::TextGeneratorV2()`): adds
/// `text_in`, `html_in`, `valign_in`, `font_in` and `font_size_in` with
/// the defaults documented on the constants, sets the inherited shape
/// base `color_in` standard value to white and `size_in` to
/// `(400, 300)`, and sets the `dont_show_in_create_menu` flag.
///
/// The inherited inputs (`base_in` from the merge base, `pos_in`/
/// `size_in`/`color_in` from the shape base) are wired here, mirroring
/// the C++ constructor chain `Node -> GeneratorWithMerge ->
/// ShapeNodeBase -> TextGeneratorV2`.
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

	// ShapeNodeBase (create_color_input = true): pos/size/color.
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
	core.add_input(crate::input::Input::new(
		crate::nodes::shapenodebase::COLOR_INPUT,
		crate::value::ValueType::Color,
		NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
	));

	// Own inputs.
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
		V_ALIGN_INPUT,
		crate::value::ValueType::Combo,
		NodeValue::Combo(0),
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

	// C++ set_standard_value overrides.
	core.set_standard_value(
		crate::nodes::shapenodebase::COLOR_INPUT,
		-1,
		NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
	);
	core.set_standard_value(
		crate::nodes::shapenodebase::SIZE_INPUT,
		-1,
		NodeValue::Vec2([400.0, 300.0]),
	);

	core.flags |= crate::node::flags::DONT_SHOW_IN_CREATE_MENU;

	(core, Box::new(TextGeneratorV2))
}

/// Register this node type (C++ `k_text_generator_v2` in
/// `factory.cpp::create_from_factory_index`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.text2",
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
		let n = TextGeneratorV2;
		assert_eq!(n.input_name(TEXT_INPUT), "Text");
		assert_eq!(n.input_name(HTML_INPUT), "Enable HTML");
		assert_eq!(n.input_name(V_ALIGN_INPUT), "Vertical Align");
		assert_eq!(n.input_name(FONT_INPUT), "Font");
		assert_eq!(n.input_name(FONT_SIZE_INPUT), "Font Size");
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
		assert_eq!(
			n.input_name(crate::nodes::shapenodebase::COLOR_INPUT),
			"Color"
		);
		assert_eq!(n.input_name("other_in"), "other_in");
	}

	#[test]
	fn create_wires_inherited_and_own_inputs() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.text2");
		assert_eq!(
			core.get_input(TEXT_INPUT).unwrap().value_type,
			ValueType::Text
		);
		assert_eq!(
			core.get_input(V_ALIGN_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(
			core.effect_input,
			crate::nodes::generatorwithmerge::BASE_INPUT
		);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
		assert_ne!(core.flags & crate::node::flags::DONT_SHOW_IN_CREATE_MENU, 0);
		// Inherited standard-value overrides.
		assert_eq!(
			core.standard_value(crate::nodes::shapenodebase::COLOR_INPUT, -1),
			NodeValue::Color([1.0, 1.0, 1.0, 1.0])
		);
		assert_eq!(
			core.standard_value(crate::nodes::shapenodebase::SIZE_INPUT, -1),
			NodeValue::Vec2([400.0, 300.0])
		);
	}

	#[test]
	fn layout_request_uses_shape_size_and_72dpi() {
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
		row.insert(FONT_SIZE_INPUT.to_string(), NodeValue::Float(36.0));
		row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(false));
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([400.0, 300.0]),
		);
		let req = TextGeneratorV2::layout_request(&row);
		assert_eq!(req.text, "Hi");
		assert_eq!(req.font_size_pt, 36.0);
		assert_eq!(req.dots_per_meter, 2835);
		assert_eq!(req.wrap_width, 400.0);
		assert!(!req.center_horizontally);
	}

	#[test]
	fn layout_request_html_translates_newlines() {
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("a\nb".to_string()));
		row.insert(HTML_INPUT.to_string(), NodeValue::Boolean(true));
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([100.0, 100.0]),
		);
		let req = TextGeneratorV2::layout_request(&row);
		assert_eq!(req.text, "a<br>b");
		assert_eq!(req.mode, TextLayoutMode::Html);
	}

	#[test]
	fn base_and_draw_offsets() {
		let size = [400.0, 300.0];
		let pos = [0.0, 0.0];
		let base = TextGeneratorV2::base_offset(pos, size, 1920, 1080);
		assert_eq!(base, (0.0 - 200.0 + 960.0, 0.0 - 150.0 + 540.0));
		// Top: no vertical delta.
		assert_eq!(TextGeneratorV2::draw_offset(0, base, size, 100), base);
		// Center: size.y/2 - doc_height/2 (integer halving of doc height).
		assert_eq!(
			TextGeneratorV2::draw_offset(1, base, size, 100),
			(base.0, base.1 + 150.0 - 50.0)
		);
		// Bottom: size.y - doc_height.
		assert_eq!(
			TextGeneratorV2::draw_offset(2, base, size, 100),
			(base.0, base.1 + 300.0 - 100.0)
		);
	}

	#[test]
	fn render_transform_clips_to_shape_rect() {
		let size = [400.0, 300.0];
		let base = (100.0, 200.0);
		let t = TextGeneratorV2::render_transform(0.5, base, base, size);
		assert_eq!(t.scale, 0.5);
		assert_eq!(t.draw_offset_x, 100.0);
		assert_eq!(t.draw_offset_y, 200.0);
		assert!(t.clip_enabled);
		assert_eq!(t.clip_offset_x, 100.0);
		assert_eq!(t.clip_offset_y, 200.0);
		assert_eq!(t.clip_width, 400.0);
		assert_eq!(t.clip_height, 300.0);
	}

	#[test]
	fn measure_without_backend_returns_zero_size() {
		crate::nodes::textbackend::set_text_backends(None, None);
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
		row.insert(
			crate::nodes::shapenodebase::POSITION_INPUT.to_string(),
			NodeValue::Vec2([0.0, 0.0]),
		);
		row.insert(
			crate::nodes::shapenodebase::SIZE_INPUT.to_string(),
			NodeValue::Vec2([400.0, 300.0]),
		);
		row.insert(V_ALIGN_INPUT.to_string(), NodeValue::Combo(1));
		let (_req, doc, base, draw) = TextGeneratorV2::measure_and_layout(&row, 1920, 1080);
		assert_eq!(doc.width, 0.0);
		assert_eq!(doc.height, 0.0);
		assert_eq!(base, (760.0, 390.0));
		assert_eq!(draw, (760.0, 390.0 + 150.0));
	}

	#[test]
	fn value_pushes_job_when_text_nonempty() {
		let (core, behavior) = create();
		let mut row = NodeValueRow::default();
		row.insert(TEXT_INPUT.to_string(), NodeValue::Text("Hi".to_string()));
		let mut table = NodeValueTable::default();
		behavior.value(&core, &row, Rational::new(0, 1), &mut table);
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
		assert!(frame.is_null());
	}

	#[test]
	fn duplicate_copies_node() {
		let (_core, behavior) = create();
		let copy = behavior.duplicate(&_core).unwrap();
		assert_eq!(copy.type_id(), "org.olivevideoeditor.Olive.text2");
		assert_eq!(copy.name(), "Text (Legacy)");
	}
}
