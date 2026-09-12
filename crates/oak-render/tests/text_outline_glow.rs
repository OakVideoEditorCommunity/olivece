//! GPU pixel tests for the text3 outline/glow post-process (skip when no
//! GPU adapter).
use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

use oak_node::nodes::textbackend::{
    set_text_backends, TextLayoutRequest, TextLayoutSize, TextRenderTarget, TextRenderTransform,
};

#[allow(dead_code)]
fn texture_value(t: Texture) -> NodeValue { NodeValue::Texture(oak_node::handle::make_owned(t)) }
fn gpu() -> bool { oak_core::backend::shared_gpu_or_skip("a text outline/glow GPU test").is_some() }
#[allow(dead_code)]
fn filled_frame(size: (i32, i32), rgba: [f32; 4]) -> Texture {
    let mut f = oak_render::eval::generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
    for px in f.data.chunks_exact_mut(16) { for (c, v) in px.chunks_exact_mut(4).zip(rgba) { c.copy_from_slice(&v.to_le_bytes()); } }
    Texture::wrap_frame(f)
}
fn pixel_at(frame: &oak_core::texture::Frame, x: usize, y: usize) -> [f32; 4] {
    let stride = frame.linesize_bytes() as usize;
    let at = y * stride + x * 16;
    let mut out = [0f32; 4];
    for c in 0..4 { out[c] = f32::from_le_bytes(frame.data[at + c*4..at + c*4 + 4].try_into().unwrap()); }
    out
}
fn eval_node_row(type_id: &str, inputs: NodeValueRow, frame_size: Option<(i32, i32)>) -> oak_core::texture::Frame {
    use oak_node::traverser::RenderHooks;
    let (core, behavior) = oak_node::factory::Factory::global().create_any(type_id).expect("node type registered");
    let mut table = NodeValueTable::default();
    behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
    let mut hooks = oak_render::eval::RenderEvalHooks::new();
    hooks.frame_size = frame_size;
    hooks.resolve(oak_node::id::NodeId::INVALID, &inputs, &mut table);
    let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else { panic!("{type_id}: no texture produced") };
    if handle.ctx.is_null() { panic!("{type_id}: null texture produced"); }
    let tex = unsafe { oak_node::handle::get_checked::<Texture>(handle) }.expect("resolved texture");
    assert!(matches!(tex, Texture::Gpu { .. }), "{type_id}: must render on the GPU");
    tex.to_frame().expect("readback")
}

const TEXT3: &str = "org.olivevideoeditor.Olive.text3";

/// The text backends are process globals: keep the tests that install
/// them off each other.
static BACKENDS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Measure stub: a 16x16 document, so the raster is 16x16 whatever the
/// font engine would have laid out.
fn block_measure(_req: &TextLayoutRequest) -> TextLayoutSize { TextLayoutSize { width: 16.0, height: 16.0 } }

/// Render stub: paint white premultiplied coverage over the middle half
/// of the target (pixels 4..12 on both axes).
fn block_render(_req: &TextLayoutRequest, _transform: &TextRenderTransform, target: TextRenderTarget) {
    if target.channel_count != 4 { return; }
    let stride = target.linesize_bytes as usize;
    let (w, h) = (target.width as usize, target.height as usize);
    for y in h / 4..3 * h / 4 {
        for x in w / 4..3 * w / 4 {
            let at = y * stride + x * 4;
            target.data[at..at + 4].copy_from_slice(&[255; 4]);
        }
    }
}

/// Insert boolean parameters (input id -> value).
fn set_bools(row: &mut NodeValueRow, params: &[(&str, bool)]) {
    for (id, v) in params {
        row.insert((*id).to_string(), NodeValue::Boolean(*v));
    }
}

/// Insert float parameters (input id -> value).
fn set_floats(row: &mut NodeValueRow, params: &[(&str, f64)]) {
    for (id, v) in params {
        row.insert((*id).to_string(), NodeValue::Float(*v));
    }
}

/// The evaluation row of the three tests: a 16x16 raster painted by
/// [`block_render`], with a 2px outline and/or a 4px glow. The outline
/// and glow colors are left at the node defaults (opaque black, opaque
/// yellow).
fn text_row(outline: bool, glow: bool) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("plain_text_in".to_string(), NodeValue::Text("X".to_string()));
    set_bools(
        &mut row,
        &[
            ("use_args_in", false),
            ("outline_enabled_in", outline),
            ("glow_enabled_in", glow),
        ],
    );
    set_floats(&mut row, &[("outline_width_in", 2.0), ("glow_radius_in", 4.0)]);
    row.insert("size_in".to_string(), NodeValue::Vec2([16.0, 16.0]));
    row
}

/// Evaluate `TEXT3` with the raster backends installed. Callers hold
/// [`BACKENDS`].
fn eval_text(row: NodeValueRow) -> oak_core::texture::Frame {
    set_text_backends(Some(block_measure), Some(block_render));
    let frame = eval_node_row(TEXT3, row, None);
    set_text_backends(None, None);
    frame
}

/// Assert the four channels of a pixel against the expected values.
fn assert_pixel(px: [f32; 4], want: [f32; 4], what: &str) {
    for c in 0..4 {
        assert!(
            (px[c] - want[c]).abs() < 1e-3,
            "{what}: channel {c}: got {px:?}, want {want:?}"
        );
    }
}

/// Outline only: the coverage is dilated by 2 pixels and tinted opaque
/// black under the text, so the glyph pixel stays white, the 2px band
/// around it is `[0,0,0,1]` and everything further out is transparent.
#[test]
fn outline_dilates_and_strokes_under_the_text() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let _guard = BACKENDS.lock().unwrap();
    set_text_backends(None, None);

    let frame = eval_text(text_row(true, false));
    assert_eq!((frame.width, frame.height), (16, 16), "raster size");

    // The glyph is drawn over its own outline.
    assert_pixel(pixel_at(&frame, 8, 8), [1.0, 1.0, 1.0, 1.0], "text over stroke");

    // Inside the 2px dilate band (coverage 4..12 widened to 2..14).
    for (x, y) in [(2, 8), (13, 8), (8, 2), (8, 13), (2, 2), (13, 13)] {
        assert_pixel(pixel_at(&frame, x, y), [0.0, 0.0, 0.0, 1.0], &format!("stroke ({x},{y})"));
    }

    // One pixel outside the band.
    for (x, y) in [(1, 8), (14, 8), (8, 1), (8, 14), (0, 0), (15, 15)] {
        assert_pixel(pixel_at(&frame, x, y), [0.0, 0.0, 0.0, 0.0], &format!("outside ({x},{y})"));
    }
}

/// Glow only: the coverage is box-blurred with radius 4 (a 9-tap pass
/// per axis) and tinted yellow under the text. The blurred alpha is
/// `(horizontal taps / 9) * (vertical taps / 9)`, e.g. 32/81 at (3,8)
/// and 1/81 at the clamped corners.
#[test]
fn glow_blurs_the_coverage_under_the_text() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let _guard = BACKENDS.lock().unwrap();
    set_text_backends(None, None);

    let frame = eval_text(text_row(false, true));
    assert_eq!((frame.width, frame.height), (16, 16), "raster size");

    // The glyph is drawn over the glow.
    assert_pixel(pixel_at(&frame, 8, 8), [1.0, 1.0, 1.0, 1.0], "text over glow");

    // Yellow is premultiplied: [a, a, 0, a].
    let a = |v: f32| [v, v, 0.0, v];
    assert_pixel(pixel_at(&frame, 3, 8), a(32.0 / 81.0), "glow 4px left of the block");
    assert_pixel(pixel_at(&frame, 2, 8), a(8.0 / 27.0), "glow 2px left of the block");
    assert_pixel(pixel_at(&frame, 1, 8), a(16.0 / 81.0), "glow 3px left of the block");
    assert_pixel(pixel_at(&frame, 2, 2), a(1.0 / 9.0), "glow on the block corner");
    // The clamped corners mirror each other.
    assert_pixel(pixel_at(&frame, 0, 0), a(1.0 / 81.0), "glow top-left corner");
    assert_pixel(pixel_at(&frame, 15, 15), a(1.0 / 81.0), "glow bottom-right corner");
}

/// Outline and glow: the glow blurs the *stroke* and is drawn beneath
/// it, so the opaque stroke hides it inside the band while it still
/// bleeds the 4px radius past the stroke edge.
#[test]
fn outline_and_glow_glow_the_stroke() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let _guard = BACKENDS.lock().unwrap();
    set_text_backends(None, None);

    let frame = eval_text(text_row(true, true));
    assert_eq!((frame.width, frame.height), (16, 16), "raster size");

    assert_pixel(pixel_at(&frame, 8, 8), [1.0, 1.0, 1.0, 1.0], "text over stroke and glow");
    assert_pixel(pixel_at(&frame, 2, 8), [0.0, 0.0, 0.0, 1.0], "stroke hides the glow");

    // Past the stroke edge the glow shows again, dimmer the wider the
    // blur source (the stroke) is than the bare coverage.
    let a = |v: f32| [v, v, 0.0, v];
    assert_pixel(pixel_at(&frame, 1, 8), a(4.0 / 9.0), "glow just past the stroke");
    assert_pixel(pixel_at(&frame, 15, 8), a(1.0 / 3.0), "glow at the right edge");
    assert_pixel(pixel_at(&frame, 0, 0), a(1.0 / 9.0), "glow top-left corner");
    assert_pixel(pixel_at(&frame, 14, 14), a(16.0 / 81.0), "glow bottom-right of the stroke");
}
