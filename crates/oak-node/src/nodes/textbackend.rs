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

//! Shared text layout/rasterization backend hooks (C++
//! `src/node/src/generator/text/textbackend.h`, header-only — no class,
//! just POD structs and global function-pointer hooks in `olive::`).
//!
//! FONT/RASTER BACKEND DEPENDENCY — DELIBERATELY UNDECIDED:
//! The C++ text nodes do NOT link any font or raster library directly.
//! Text layout and rasterization were historically done with Qt's rich
//! text stack (`QTextDocument` / `QTextOption` /
//! `QAbstractTextDocumentLayout`) rasterized via `QPainter` into a
//! `QImage` (`Format_Grayscale8` for v1/v2 coverage buffers,
//! `Format_RGBA8888_Premultiplied` for v3). That Qt dependency has
//! already been removed from oaknode: the C++ now carries only the POD
//! job descriptions below plus two installable backend hooks
//! (`g_text_measure_backend` / `g_text_render_backend`) that the
//! facade/app layer fills in at runtime. No Rust font/shaping/raster
//! crate (freetype-rs, rusttype, cosmic-text, swash, etc.) is chosen
//! here on purpose; the backend decision belongs to the facade layer
//! that will install these hooks.

/// POD description of a text layout/rasterization job (C++
/// `TextLayoutRequest`). Replaces the `QTextDocument`/`QTextOption`
/// usage in the text generator nodes; this module carries only the
/// data.
pub struct TextLayoutRequest {
	/// Plain text or markup, per `mode`.
	pub text: String,
	/// Layout mode (C++ `TextLayoutRequest::Mode`).
	pub mode: TextLayoutMode,
	/// Default font family (empty = backend default).
	pub font_family: String,
	/// Default font size in points (0 = default).
	pub font_size_pt: f64,
	/// Paint device resolution (0 = backend default).
	pub dots_per_meter: i32,
	/// Wrap width (C++ `QTextDocument::setTextWidth()`).
	pub wrap_width: f64,
	/// Center horizontally by default (C++ default
	/// `QTextOption(Qt::AlignCenter)`).
	pub center_horizontally: bool,
	// Note: the backends always paint with white as the default text
	// color (formerly PaintContext palette QPalette::Text = Qt::white);
	// HTML markup may override it per span.
}

/// Layout mode (C++ `TextLayoutRequest::Mode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextLayoutMode {
	/// Plain text (C++ `QTextDocument::setPlainText()`).
	PlainText,
	/// HTML subset (C++ `QTextDocument::setHtml()`).
	Html,
	/// Olive's `Html::html_to_doc()` rich text.
	OliveHtml,
}

/// Laid-out document size (C++ `TextLayoutSize`, formerly
/// `QTextDocument::size()`).
#[derive(Clone, Copy, Debug, Default)]
pub struct TextLayoutSize {
	/// Document width in pixels.
	pub width: f64,
	/// Document height in pixels.
	pub height: f64,
}

/// Target pixel buffer for text rasterization (C++
/// `TextRenderTarget`). `channel_count == 1` means 8-bit grayscale
/// coverage (formerly `QImage::Format_Grayscale8`, the node tints it
/// afterwards); `channel_count == 4` means 8-bit RGBA premultiplied
/// drawn over the existing (cleared) buffer content (formerly
/// `QImage::Format_RGBA8888_Premultiplied`).
pub struct TextRenderTarget<'a> {
	/// Pixel buffer, `linesize_bytes * height` bytes.
	pub data: &'a mut [u8],
	/// Buffer width in pixels.
	pub width: i32,
	/// Buffer height in pixels.
	pub height: i32,
	/// Bytes per scanline.
	pub linesize_bytes: i32,
	/// Channel count: 1 = grayscale coverage, 4 = RGBA premultiplied.
	pub channel_count: i32,
}

/// Draw transform, mirroring the original `QPainter` calls (C++
/// `TextRenderTransform`). The painter first applied
/// `scale(scale, scale)`, then translated to the base offset, set the
/// clip rect `(0, 0, clip_width, clip_height)` at that base offset, and
/// finally translated by the remaining draw offset before drawing. A
/// point `p` of the laid-out document therefore lands at
/// `((p.x + draw_offset_x) * scale, (p.y + draw_offset_y) * scale)`,
/// clipped to `((clip_offset_*) * scale, (clip_size) * scale)` relative
/// to the same origin. `clip_enabled == false` means no clip.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextRenderTransform {
	/// Uniform scale (C++ `QPainter::scale(scale, scale)`).
	pub scale: f64,
	/// Draw offset X, in pre-scale document coordinates.
	pub draw_offset_x: f64,
	/// Draw offset Y, in pre-scale document coordinates.
	pub draw_offset_y: f64,
	/// Whether the clip rect is active.
	pub clip_enabled: bool,
	/// Clip rect offset X, in pre-scale document coordinates.
	pub clip_offset_x: f64,
	/// Clip rect offset Y, in pre-scale document coordinates.
	pub clip_offset_y: f64,
	/// Clip rect width, in pre-scale document coordinates.
	pub clip_width: f64,
	/// Clip rect height, in pre-scale document coordinates.
	pub clip_height: f64,
}

/// Measure hook (C++ `TextMeasureBackend`): lays out the request and
/// returns the document size (formerly `QTextDocument::size()`).
pub type TextMeasureBackend = fn(&TextLayoutRequest) -> TextLayoutSize;

/// Render hook (C++ `TextRenderBackend`): draws the request into the
/// target buffer (formerly `QAbstractTextDocumentLayout::draw()`).
pub type TextRenderBackend =
	for<'a> fn(&TextLayoutRequest, &TextRenderTransform, TextRenderTarget<'a>);

/// Install the backend hooks (C++ `set_text_backends()`). When a hook
/// is `None` the caller falls back to a zero size / leaves the (already
/// cleared) buffer untouched — a documented behavior gap until the
/// facade installs a text engine. Setting a hook to `None` uninstalls
/// it (the C++ global is a plain function pointer, assignable any
/// number of times; a `Mutex` keeps the tests able to reset it).
pub fn set_text_backends(measure: Option<TextMeasureBackend>, render: Option<TextRenderBackend>) {
	*MEASURE.lock().unwrap() = measure;
	*RENDER.lock().unwrap() = render;
}

/// Currently installed measure hook (C++ `text_measure_backend()`).
pub fn text_measure_backend() -> Option<TextMeasureBackend> {
	*MEASURE.lock().unwrap()
}

/// Currently installed render hook (C++ `text_render_backend()`).
pub fn text_render_backend() -> Option<TextRenderBackend> {
	*RENDER.lock().unwrap()
}

/// Installed measure hook (C++ global `g_text_measure_backend`).
static MEASURE: std::sync::Mutex<Option<TextMeasureBackend>> = std::sync::Mutex::new(None);

/// Installed render hook (C++ global `g_text_render_backend`).
static RENDER: std::sync::Mutex<Option<TextRenderBackend>> = std::sync::Mutex::new(None);

/// Serializes every test that installs the process-global text backends:
/// the tests below and `textv3`'s installer tests share it, so none of
/// them observes another's install.
#[cfg(test)]
pub(crate) static TEST_BACKEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
	use super::*;

	// The two tests below share the process-global backend statics; the
	// shared test lock also serializes them against `textv3`'s installer
	// tests, so `backend_hooks_default_none` cannot observe the hooks
	// installed by `backend_hooks_install_and_query`.

	#[test]
	fn backend_hooks_default_none() {
		let _guard = TEST_BACKEND_LOCK.lock().unwrap();
		set_text_backends(None, None);
		assert!(text_measure_backend().is_none());
		assert!(text_render_backend().is_none());
	}

	#[test]
	fn backend_hooks_install_and_query() {
		let _guard = TEST_BACKEND_LOCK.lock().unwrap();
		fn measure(_r: &TextLayoutRequest) -> TextLayoutSize {
			TextLayoutSize {
				width: 12.0,
				height: 34.0,
			}
		}
		fn render(_r: &TextLayoutRequest, _t: &TextRenderTransform, target: TextRenderTarget) {
			for b in target.data.iter_mut() {
				*b = 255;
			}
		}
		set_text_backends(Some(measure), Some(render));
		assert_eq!(
			text_measure_backend().unwrap()(&TextLayoutRequest {
				text: String::new(),
				mode: TextLayoutMode::PlainText,
				font_family: String::new(),
				font_size_pt: 0.0,
				dots_per_meter: 0,
				wrap_width: 0.0,
				center_horizontally: false,
			})
			.width,
			12.0
		);
		assert_eq!(
			text_measure_backend().unwrap()(&TextLayoutRequest {
				text: String::new(),
				mode: TextLayoutMode::Html,
				font_family: String::new(),
				font_size_pt: 0.0,
				dots_per_meter: 0,
				wrap_width: 0.0,
				center_horizontally: false,
			})
			.height,
			34.0
		);
		let mut buf = vec![0u8; 4];
		text_render_backend().unwrap()(
			&TextLayoutRequest {
				text: String::new(),
				mode: TextLayoutMode::PlainText,
				font_family: String::new(),
				font_size_pt: 0.0,
				dots_per_meter: 0,
				wrap_width: 0.0,
				center_horizontally: false,
			},
			&TextRenderTransform::default(),
			TextRenderTarget {
				data: &mut buf,
				width: 2,
				height: 2,
				linesize_bytes: 2,
				channel_count: 1,
			},
		);
		assert_eq!(buf, vec![255u8; 4]);
		// Restore the uninstalled state so other tests observe the
		// no-backend fallback.
		set_text_backends(None, None);
	}
}
