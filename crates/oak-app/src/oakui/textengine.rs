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

//! cosmic-text backend for the text generator nodes: the facade-layer half
//! of [`oak_node::nodes::textbackend`].
//!
//! `oak-node` deliberately links no font or shaping crate: the text
//! generator nodes describe their job with the
//! [`TextLayoutRequest`](oak_node::nodes::textbackend::TextLayoutRequest) /
//! [`TextRenderTarget`](oak_node::nodes::textbackend::TextRenderTarget) PODs
//! and call the two function-pointer hooks that [`install`] fills in. This
//! module is that backend: layout and rasterization run on `cosmic-text` +
//! `swash`, the same stack the `gpui_wgpu` text system already links, so the
//! app keeps a single font stack and a single `cosmic-text` in the lockfile.
//!
//! [`RealEngine::new`](super::real::RealEngine::new) calls [`install`] once;
//! [`font_families`] is the other entry point, feeding the
//! `font_family_in` combo of the text nodes (see
//! [`super::effectchain::effect_params`]).
//!
//! # Units
//!
//! [`TextLayoutRequest::dots_per_meter`] is the paint device resolution
//! (Qt's `QTextDocument`/`QPainter` device metric); font sizes and geometry
//! in the request are points, and the laid-out document the hooks return and
//! draw in is sized in *device pixels*, exactly like the `QTextDocument`
//! this replaced. The conversion is `px = pt * dots_per_meter * 0.0254 / 72`
//! (3780 dots/meter, the Qt default for 96 DPI, gives the familiar 4/3
//! factor); a zero or negative `dots_per_meter` falls back to 3780 and a
//! non-positive or non-finite `font_size_pt` to 72 pt. Both fallbacks match
//! the text nodes' own defaults (`oak_node::nodes::textv3`'s
//! `font_size_in` default is 72 pt).
//!
//! # Approximations compared to the former Qt implementation
//!
//! * Line spacing is `1.2 * font_size` (Qt's single spacing depends on the
//!   font's own metrics).
//! * `Html` / `OliveHtml` requests are flattened by [`html_to_plain`]
//!   instead of being laid out as rich text: tags carry no formatting here,
//!   and the text is painted white throughout — per-span colors, bold /
//!   italic runs and the rich-text alignment / list layout of the old
//!   `QTextDocument` are not reproduced.
//! * Text decorations (underline / strikethrough) are not painted.
//! * `center_horizontally` maps to [`Align::Center`] over the wrap width;
//!   like the C++ default `QTextOption(Qt::AlignCenter)` it is a no-op when
//!   the request carries no wrap width.
//! * A request whose flattened text is empty measures 0×0 and paints
//!   nothing (an empty `QTextDocument` reports one empty line's height).
//!
//! The layout and rasterization themselves follow the C++ contract to the
//! pixel: a document point `p` lands at
//! `((p.x + draw_offset_x) * scale, (p.y + draw_offset_y) * scale)`, clipped
//! to the scaled clip rect, and the two target formats are the Qt
//! `Format_Grayscale8` coverage buffer (channel count 1) and the
//! `Format_RGBA8888_Premultiplied` buffer (channel count 4).

use std::borrow::Cow;
use std::sync::{Mutex, MutexGuard, Once, OnceLock};

use cosmic_text::{
	Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap,
};
use oak_node::nodes::textbackend::{
	set_text_backends, TextLayoutMode, TextLayoutRequest, TextLayoutSize, TextRenderTarget,
	TextRenderTransform,
};

/// Fallback font size in points (the text nodes' `font_size_in` default).
const DEFAULT_FONT_SIZE_PT: f64 = 72.0;

/// Fallback paint device resolution: 3780 dots/meter ≈ 96 DPI, Qt's default.
const DEFAULT_DOTS_PER_METER: f64 = 3780.0;

/// Fallback font size in device pixels, used only when the point size and
/// the resolution cannot produce a usable pixel size.
const DEFAULT_FONT_PX: f32 = 96.0;

/// Points per inch (the numerator of the point → pixel conversion).
const POINTS_PER_INCH: f64 = 72.0;

/// Meters per inch (the denominator of the point → pixel conversion).
const METERS_PER_INCH: f64 = 0.0254;

/// Line height as a multiple of the font size.
const LINE_HEIGHT_SCALE: f32 = 1.2;

/// Upper bound on the rasterized font size. A project file can carry an
/// arbitrary `font_size_pt` / `dots_per_meter`; without a ceiling the swash
/// bitmap cache would happily try to allocate gigabytes for a glyph.
const MAX_FONT_PX: f32 = 8192.0;

/// Upper bound on the wrap width, keeping the layout solver's inputs sane
/// for a corrupt `wrap_width`.
const MAX_WRAP_PX: f32 = 1.0e6;

/// Glyph rasterization state shared by every request.
///
/// [`FontSystem`] scans the system font directories on construction, so it
/// is built once (lazily, on the first non-empty request) and reused; a
/// [`Buffer`] is cheap and stays per-request.
struct TextSystem {
	font_system: FontSystem,
	swash_cache: SwashCache,
}

/// The process-wide text system.
fn system() -> &'static Mutex<TextSystem> {
	static SYSTEM: OnceLock<Mutex<TextSystem>> = OnceLock::new();
	SYSTEM.get_or_init(|| {
		Mutex::new(TextSystem {
			font_system: FontSystem::new(),
			swash_cache: SwashCache::new(),
		})
	})
}

/// Locks the text system, recovering from a poisoned lock: a panic while
/// shaping one text node must not take the whole app's text rendering down.
fn lock_system() -> MutexGuard<'static, TextSystem> {
	system().lock().unwrap_or_else(|e| e.into_inner())
}

/// Installs this module's hooks as the process-wide text backends (C++
/// `set_text_backends()` at app startup).
///
/// Idempotent: only the first call installs, so a second engine instance
/// cannot swap the hooks out from under a layout in flight.
pub fn install() {
	static INSTALL: Once = Once::new();
	INSTALL.call_once(|| {
		set_text_backends(Some(measure), Some(render));
	});
}

/// Measure hook: lays the request out and returns the document size in
/// device pixels (C++ `QTextDocument::size()`).
pub fn measure(req: &TextLayoutRequest) -> TextLayoutSize {
	let text = request_text(req);
	if text.is_empty() {
		return TextLayoutSize::default();
	}
	let mut sys = lock_system();
	let buffer = layout(&mut sys.font_system, req, &text);
	let mut width = 0.0f32;
	let mut height = 0.0f32;
	for run in buffer.layout_runs() {
		width = width.max(run.line_w);
		height = height.max(run.line_top + run.line_height);
	}
	TextLayoutSize {
		width: width as f64,
		height: height as f64,
	}
}

/// Render hook: paints the request into `target` (C++
/// `QAbstractTextDocumentLayout::draw()`).
pub fn render(req: &TextLayoutRequest, transform: &TextRenderTransform, target: TextRenderTarget) {
	let TextRenderTarget {
		data,
		width,
		height,
		linesize_bytes,
		channel_count,
	} = target;
	if width <= 0 || height <= 0 || linesize_bytes <= 0 || !matches!(channel_count, 1 | 4) {
		return;
	}
	// `QPainter::scale(0, 0)` (degenerate transform in the stored
	// parameters) paints nothing rather than collapsing to a matrix.
	let scale = transform.scale;
	if !scale.is_finite() || scale <= 0.0 {
		return;
	}
	let text = request_text(req);
	if text.is_empty() {
		return;
	}
	let (clip_left, clip_top, clip_right, clip_bottom) =
		clip_rect(transform, width, height, scale);
	if !(clip_left < clip_right && clip_top < clip_bottom) {
		return;
	}

	let mut sys = lock_system();
	let mut buffer = layout(&mut sys.font_system, req, &text);
	let TextSystem {
		font_system,
		swash_cache,
	} = &mut *sys;
	let offset_x = transform.draw_offset_x * scale;
	for run in buffer.layout_runs() {
		let offset_y = (run.line_y as f64 + transform.draw_offset_y) * scale;
		for glyph in run.glyphs {
			let physical = glyph.physical((offset_x as f32, offset_y as f32), scale as f32);
			// The raster extends about one em around the glyph origin in
			// both axes; skip the glyphs that cannot touch the clip rect
			// instead of letting swash rasterize (and cache) them.
			let em = glyph.font_size as f64 * scale;
			let gx = physical.x as f64;
			let gy = physical.y as f64;
			if gx + em < clip_left
				|| gx - em > clip_right
				|| gy + em < clip_top
				|| gy - em > clip_bottom
			{
				continue;
			}
			let base = glyph.color_opt.unwrap_or(WHITE);
			swash_cache.with_pixels(font_system, physical.cache_key, base, |px, py, color| {
				let x = physical.x + px;
				let y = physical.y + py;
				if (x as f64) < clip_left
					|| (x as f64) >= clip_right
					|| (y as f64) < clip_top
					|| (y as f64) >= clip_bottom
				{
					return;
				}
				blend(data, linesize_bytes, channel_count, x, y, color);
			});
		}
	}
}

/// The default text color: the backends always paint white unless the
/// markup overrides it (C++ `QPalette::Text` = `Qt::white`).
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

/// The sorted, de-duplicated font families of the system font database.
///
/// Feeds the text nodes' `font_family_in` combo (the `combo_option`
/// injection in [`super::effectchain::effect_params`]). Names are the
/// English family names, so they match what a project stores; an empty
/// database yields an empty list and the combo keeps free-form entry.
///
/// The list is snapshotted on first use: `effect_params` rebuilds the
/// inspector's parameters on every engine change, and the font database
/// only changes when fonts are installed (an app restart).
pub fn font_families() -> Vec<String> {
	static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
	FAMILIES
		.get_or_init(|| {
			let mut names: Vec<String> = {
				let sys = lock_system();
				sys.font_system
					.db()
					.faces()
					.filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
					.collect()
			};
			names.sort();
			names.dedup();
			names
		})
		.clone()
}

/// The pixel-space clip rectangle of a render, already intersected with the
/// target buffer. An empty (or inverted) rectangle means nothing is drawn;
/// a non-finite rect from a corrupt transform collapses to empty too.
fn clip_rect(
	transform: &TextRenderTransform,
	width: i32,
	height: i32,
	scale: f64,
) -> (f64, f64, f64, f64) {
	let mut left = 0.0f64;
	let mut top = 0.0f64;
	let mut right = width as f64;
	let mut bottom = height as f64;
	if transform.clip_enabled {
		let x = transform.clip_offset_x * scale;
		let y = transform.clip_offset_y * scale;
		let w = transform.clip_width * scale;
		let h = transform.clip_height * scale;
		if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
			return (0.0, 0.0, 0.0, 0.0);
		}
		left = left.max(x);
		top = top.max(y);
		right = right.min(x + w.max(0.0));
		bottom = bottom.min(y + h.max(0.0));
	}
	(left, top, right, bottom)
}

/// Composites one source pixel over the target.
///
/// `channel_count == 1` is the grayscale coverage buffer the v1/v2 nodes
/// tint afterwards: the glyph contributes its alpha as coverage. Channel
/// count 4 is the premultiplied RGBA buffer of v3, so the source is
/// premultiplied before the over-blend (the hook's colors are straight —
/// swash's mask pixels carry the (white) base color plus coverage-as-alpha,
/// and its color bitmaps are straight RGBA).
fn blend(
	data: &mut [u8],
	linesize_bytes: i32,
	channel_count: i32,
	x: i32,
	y: i32,
	color: Color,
) {
	if x < 0 || y < 0 {
		return;
	}
	let alpha = color.a();
	if alpha == 0 {
		return;
	}
	let offset = y as usize * linesize_bytes as usize + x as usize * channel_count as usize;
	if channel_count == 1 {
		let Some(dst) = data.get_mut(offset) else {
			return;
		};
		let src = alpha as u32;
		let out = src + (*dst as u32 * (255 - src)) / 255;
		*dst = out.min(255) as u8;
		return;
	}
	let Some(pixel) = data.get_mut(offset..offset + 4) else {
		return;
	};
	let src_alpha = alpha as u32;
	let inverse = 255 - src_alpha;
	let src = [
		(color.r() as u32 * src_alpha + 127) / 255,
		(color.g() as u32 * src_alpha + 127) / 255,
		(color.b() as u32 * src_alpha + 127) / 255,
		src_alpha,
	];
	for (dst, value) in pixel.iter_mut().zip(src) {
		*dst = (value + *dst as u32 * inverse / 255).min(255) as u8;
	}
}

/// The text a request lays out: the request's own text, or the flattened
/// markup for the two HTML modes.
fn request_text(req: &TextLayoutRequest) -> Cow<'_, str> {
	match req.mode {
		TextLayoutMode::PlainText => Cow::Borrowed(req.text.as_str()),
		TextLayoutMode::Html | TextLayoutMode::OliveHtml => Cow::Owned(html_to_plain(&req.text)),
	}
}

/// Lays a request out into a shaped [`Buffer`].
fn layout(font_system: &mut FontSystem, req: &TextLayoutRequest, text: &str) -> Buffer {
	let metrics = Metrics::relative(font_px(req), LINE_HEIGHT_SCALE);
	let mut buffer = Buffer::new(font_system, metrics);
	let mut attrs = Attrs::new();
	if !req.font_family.is_empty() {
		attrs = attrs.family(Family::Name(req.font_family.as_str()));
	}
	let alignment = if req.center_horizontally {
		Some(Align::Center)
	} else {
		None
	};
	buffer.set_text(text, &attrs, Shaping::Advanced, alignment);
	if req.wrap_width.is_finite() && req.wrap_width > 0.0 {
		buffer.set_size(Some(req.wrap_width.min(MAX_WRAP_PX as f64) as f32), None);
	}
	// CJK text has no spaces to break at, so word wrapping alone would
	// overflow the wrap width; `WordOrGlyph` keeps the C++ behavior of
	// wrapping inside a run of CJK.
	buffer.set_wrap(Wrap::WordOrGlyph);
	buffer.shape_until_scroll(font_system, false);
	buffer
}

/// The font size of a request in device pixels, with the documented
/// fallbacks and a sanity ceiling.
fn font_px(req: &TextLayoutRequest) -> f32 {
	let pt = if req.font_size_pt.is_finite() && req.font_size_pt > 0.0 {
		req.font_size_pt
	} else {
		DEFAULT_FONT_SIZE_PT
	};
	let dots_per_meter = if req.dots_per_meter > 0 {
		req.dots_per_meter as f64
	} else {
		DEFAULT_DOTS_PER_METER
	};
	let px = pt * dots_per_meter * (METERS_PER_INCH / POINTS_PER_INCH);
	if !px.is_finite() || px <= 0.0 {
		return DEFAULT_FONT_PX;
	}
	(px as f32).min(MAX_FONT_PX)
}

/// Flattens the HTML the text nodes may carry into plain text.
///
/// This mirrors `oak_node::nodes::textv3`'s legacy-HTML stripper (which
/// is crate-private): tags are dropped without being rescanned, the
/// entities it knows are decoded, unknown entities and bare `&` are kept
/// as-is, and whitespace is neither collapsed nor trimmed. On top of that,
/// `<br>` and the block-level end tags (`</p>`, `</div>`, `</li>`, the
/// headings, table rows, …) become line breaks so the paragraph structure
/// of `Html` / `OliveHtml` text survives flattening; the breaks a trailing
/// block end tag would add are dropped.
fn html_to_plain(html: &str) -> String {
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

	/// Block-level end tags that start a new line, plus `<br>` itself.
	const LINE_BREAK_TAGS: [&str; 21] = [
		"br",
		"/p",
		"/div",
		"/li",
		"/tr",
		"/h1",
		"/h2",
		"/h3",
		"/h4",
		"/h5",
		"/h6",
		"/blockquote",
		"/pre",
		"/table",
		"/ul",
		"/ol",
		"/dl",
		"/dt",
		"/dd",
		"/section",
		"/figure",
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
				let start = i + 1;
				let mut end = start;
				while end < chars.len() && chars[end] != '>' {
					end += 1;
				}
				let tag: String = chars[start..end].iter().collect::<String>().to_lowercase();
				let trimmed = tag.trim_start();
				let (closing, rest) = match trimmed.strip_prefix('/') {
					Some(rest) => (true, rest.trim_start()),
					None => (false, trimmed),
				};
				let name: String = rest
					.chars()
					.take_while(|c| c.is_ascii_alphanumeric())
					.collect();
				let name = if closing { format!("/{name}") } else { name };
				if !out.is_empty() && LINE_BREAK_TAGS.contains(&name.as_str()) {
					out.push('\n');
				}
				i = end + 1;
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
	while out.ends_with('\n') {
		out.pop();
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Mutex as StdMutex;

	/// The hook statics are process-global; the one test that installs them
	/// holds this so it cannot race a future one.
	static HOOK_LOCK: StdMutex<()> = StdMutex::new(());

	/// A plain-text request with the test defaults (72 pt at 3780
	/// dots/meter, no wrap, no centering, backend default family).
	fn plain(text: &str, font_size_pt: f64, wrap_width: f64) -> TextLayoutRequest {
		TextLayoutRequest {
			text: text.to_string(),
			mode: TextLayoutMode::PlainText,
			font_family: String::new(),
			font_size_pt,
			dots_per_meter: 3780,
			wrap_width,
			center_horizontally: false,
		}
	}

	/// A transform with only the scale set (the rest neutral).
	fn scaled(scale: f64) -> TextRenderTransform {
		TextRenderTransform {
			scale,
			..TextRenderTransform::default()
		}
	}

	/// Renders a request into a zeroed buffer and returns it.
	fn render_into(
		req: &TextLayoutRequest,
		transform: &TextRenderTransform,
		width: i32,
		height: i32,
		channel_count: i32,
	) -> Vec<u8> {
		let mut data = vec![0u8; (width * height * channel_count) as usize];
		render(
			req,
			transform,
			TextRenderTarget {
				data: &mut data,
				width,
				height,
				linesize_bytes: width * channel_count,
				channel_count,
			},
		);
		data
	}

	/// Bounding box (`min_x`, `min_y`, `max_x`, `max_y`) of the nonzero
	/// pixels of a grayscale buffer; `None` when nothing was painted.
	fn ink_bounds(data: &[u8], width: i32, height: i32) -> Option<(i32, i32, i32, i32)> {
		let mut bounds: Option<(i32, i32, i32, i32)> = None;
		for y in 0..height {
			for x in 0..width {
				if data[(y * width + x) as usize] != 0 {
					bounds = Some(match bounds {
						None => (x, y, x, y),
						Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
					});
				}
			}
		}
		bounds
	}

	fn has_ink(data: &[u8]) -> bool {
		data.iter().any(|b| *b != 0)
	}

	#[test]
	fn plain_text_measures_nonzero() {
		let size = measure(&plain("Hello", 72.0, 0.0));
		assert!(size.width > 0.0, "width = {}", size.width);
		assert!(size.height > 0.0, "height = {}", size.height);
	}

	#[test]
	fn measure_scales_with_font_size() {
		let small = measure(&plain("Hello", 36.0, 0.0));
		let large = measure(&plain("Hello", 72.0, 0.0));
		assert!(large.width > small.width * 1.5);
		assert!(large.height > small.height * 1.5);
	}

	#[test]
	fn cjk_text_measures_nonzero() {
		// No explicit family: the default fallback chain has to resolve the
		// glyphs through fontconfig.
		let size = measure(&plain("中文字体测试", 72.0, 0.0));
		assert!(size.width > 0.0, "width = {}", size.width);
		assert!(size.height > 0.0, "height = {}", size.height);
	}

	#[test]
	fn empty_text_measures_zero() {
		let size = measure(&plain("", 72.0, 0.0));
		assert_eq!(size.width, 0.0);
		assert_eq!(size.height, 0.0);
	}

	#[test]
	fn multi_line_is_taller_than_single_line() {
		let one = measure(&plain("Hello", 72.0, 0.0));
		let two = measure(&plain("Hello\nWorld", 72.0, 0.0));
		assert!(two.height > one.height);
	}

	#[test]
	fn wrap_width_wraps() {
		let unwrapped = measure(&plain("中文字体测试", 72.0, 0.0));
		let wrapped = measure(&plain("中文字体测试", 72.0, 200.0));
		assert!(
			wrapped.width < unwrapped.width,
			"wrapped {} >= unwrapped {}",
			wrapped.width,
			unwrapped.width
		);
		assert!(wrapped.width <= 200.0, "wrapped width {}", wrapped.width);
		assert!(wrapped.height > unwrapped.height);
	}

	#[test]
	fn html_mode_flattens_tags() {
		let plain_req = plain("Hello", 72.0, 0.0);
		let html_req = TextLayoutRequest {
			text: "<b>Hello</b>".to_string(),
			mode: TextLayoutMode::Html,
			..plain("Hello", 72.0, 0.0)
		};
		assert_eq!(measure(&html_req).width, measure(&plain_req).width);
		assert_eq!(measure(&html_req).height, measure(&plain_req).height);
	}

	#[test]
	fn html_to_plain_decodes_and_breaks() {
		assert_eq!(html_to_plain("<b>a</b>"), "a");
		assert_eq!(html_to_plain("a<br>b"), "a\nb");
		assert_eq!(html_to_plain("a<br/>b"), "a\nb");
		assert_eq!(html_to_plain("<p>a</p><p>b</p>"), "a\nb");
		assert_eq!(html_to_plain("a</DIV>b"), "a\nb");
		assert_eq!(html_to_plain("&amp;&lt;&gt;&quot;&#39;&nbsp;"), "&<>\"' ");
		// Unknown entities and bare `&` survive untouched.
		assert_eq!(html_to_plain("a &b"), "a &b");
		// A decoded `&lt;p&gt;` is text, never rescanned into a tag.
		assert_eq!(html_to_plain("&lt;p&gt;"), "<p>");
		// Unterminated tags drop the remainder; trailing breaks are dropped.
		assert_eq!(html_to_plain("<p>a"), "a");
		assert_eq!(html_to_plain("a</p>"), "a");
	}

	#[test]
	fn font_families_are_sorted_and_unique() {
		let families = font_families();
		assert!(
			!families.is_empty(),
			"the system font database has no family"
		);
		let mut sorted = families.clone();
		sorted.sort();
		sorted.dedup();
		assert_eq!(families, sorted);
		// Cached: the second call hands out the same list.
		assert_eq!(font_families(), families);
	}

	#[test]
	fn font_pixel_size_fallbacks() {
		let base = plain("x", 72.0, 0.0);
		// 72 pt at 3780 dots/meter == 96 DPI == 96.012 px.
		assert!((font_px(&base) as f64 - 96.012).abs() < 0.01);
		// 0 / non-finite point sizes fall back to 72 pt.
		assert_eq!(font_px(&plain("x", 0.0, 0.0)), font_px(&base));
		assert_eq!(font_px(&plain("x", f64::NAN, 0.0)), font_px(&base));
		// Non-positive dots-per-meter falls back to 3780.
		let mut req = plain("x", 72.0, 0.0);
		req.dots_per_meter = 0;
		assert_eq!(font_px(&req), font_px(&base));
		req.dots_per_meter = -3780;
		assert_eq!(font_px(&req), font_px(&base));
		// Absurd sizes clamp instead of asking swash for a huge bitmap.
		assert_eq!(font_px(&plain("x", 1.0e9, 0.0)), MAX_FONT_PX);
	}

	#[test]
	fn render_writes_premultiplied_white_rgba() {
		let data = render_into(&plain("H", 72.0, 0.0), &scaled(1.0), 160, 160, 4);
		let mut ink = 0usize;
		for px in data.chunks_exact(4) {
			let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
			if a == 0 && r == 0 && g == 0 && b == 0 {
				continue;
			}
			ink += 1;
			assert_eq!((r, g, b), (r, r, r), "non-white pixel {px:?}");
			assert!(r <= a, "not premultiplied: {px:?}");
		}
		assert!(ink > 0, "the render painted nothing");
	}

	#[test]
	fn render_writes_grayscale_coverage() {
		let req = plain("H", 72.0, 0.0);
		let gray = render_into(&req, &scaled(1.0), 160, 160, 1);
		assert!(has_ink(&gray), "the render painted nothing");
		// The RGBA alpha is that same coverage (both blend over a zeroed
		// buffer), so the two formats have to agree pixel for pixel.
		let rgba = render_into(&req, &scaled(1.0), 160, 160, 4);
		for (i, alpha) in rgba.chunks_exact(4).map(|px| px[3]).enumerate() {
			assert_eq!(gray[i], alpha, "coverage mismatch at pixel {i}");
		}
	}

	#[test]
	fn render_scales_the_glyph() {
		let req = plain("H", 72.0, 0.0);
		let one = render_into(&req, &scaled(1.0), 256, 256, 1);
		let two = render_into(&req, &scaled(2.0), 256, 256, 1);
		let (x0, y0, x1, y1) = ink_bounds(&one, 256, 256).expect("scale 1 ink");
		let (u0, v0, u1, v1) = ink_bounds(&two, 256, 256).expect("scale 2 ink");
		assert!((u1 - u0) > (x1 - x0) * 3 / 2, "width did not scale");
		assert!((v1 - v0) > (y1 - y0) * 3 / 2, "height did not scale");
	}

	#[test]
	fn render_skips_degenerate_scale() {
		for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
			let data = render_into(&plain("H", 72.0, 0.0), &scaled(scale), 64, 64, 1);
			assert!(!has_ink(&data), "scale {scale} painted something");
		}
	}

	#[test]
	fn render_clips_to_the_clip_rect() {
		let req = plain("H", 72.0, 0.0);
		let full = render_into(&req, &scaled(1.0), 160, 160, 1);
		let (min_x, _, _, _) = ink_bounds(&full, 160, 160).expect("unclipped ink");
		assert!(min_x < 40, "the unclipped ink starts at {min_x}");

		// A clip that keeps only the right half of the glyph.
		let clipped = render_into(
			&req,
			&TextRenderTransform {
				clip_enabled: true,
				clip_offset_x: 40.0,
				clip_width: 200.0,
				clip_height: 200.0,
				..scaled(1.0)
			},
			160,
			160,
			1,
		);
		let (cmin_x, _, cmax_x, cmax_y) =
			ink_bounds(&clipped, 160, 160).expect("ink inside the clip");
		assert!(cmin_x >= 40, "ink left of the clip at {cmin_x}");
		assert!(cmin_x > min_x, "the clip removed nothing");
		assert!(cmax_x < 160 && cmax_y < 160);

		// An empty clip rect and a non-finite one paint nothing.
		let empty = render_into(
			&req,
			&TextRenderTransform {
				clip_enabled: true,
				clip_width: 0.0,
				clip_height: 0.0,
				..scaled(1.0)
			},
			160,
			160,
			1,
		);
		assert!(!has_ink(&empty));
		let nan = render_into(
			&req,
			&TextRenderTransform {
				clip_enabled: true,
				clip_offset_x: f64::NAN,
				clip_width: 64.0,
				clip_height: 64.0,
				..scaled(1.0)
			},
			160,
			160,
			1,
		);
		assert!(!has_ink(&nan));
	}

	#[test]
	fn render_honors_draw_offset() {
		let req = plain("H", 72.0, 0.0);
		let base = render_into(&req, &scaled(1.0), 256, 256, 1);
		let shifted = render_into(
			&req,
			&TextRenderTransform {
				draw_offset_x: 100.0,
				draw_offset_y: 100.0,
				..scaled(1.0)
			},
			256,
			256,
			1,
		);
		let (bx, by, _, _) = ink_bounds(&base, 256, 256).expect("unshifted ink");
		let (sx, sy, _, _) = ink_bounds(&shifted, 256, 256).expect("shifted ink");
		// The offset is applied in device pixels after the scale, so the
		// corners move by exactly the offset.
		assert_eq!((sx, sy), (bx + 100, by + 100));
	}

	#[test]
	fn hooks_install_and_drive_the_nodes() {
		let _guard = HOOK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		use oak_node::nodes::textbackend::{text_measure_backend, text_render_backend};

		install();
		let measure_fn = text_measure_backend().expect("install() sets the measure hook");
		let render_fn = text_render_backend().expect("install() sets the render hook");

		let req = plain("Hello", 72.0, 0.0);
		let size = measure_fn(&req);
		assert!(size.width > 0.0 && size.height > 0.0);

		let mut data = vec![0u8; 192 * 192];
		render_fn(
			&req,
			&scaled(1.0),
			TextRenderTarget {
				data: &mut data,
				width: 192,
				height: 192,
				linesize_bytes: 192,
				channel_count: 1,
			},
		);
		assert!(has_ink(&data), "the render hook painted nothing");

		// Leave the process hooks uninstalled like `textbackend`'s own tests
		// do, so no later test depends on this module's state.
		set_text_backends(None, None);
	}
}
