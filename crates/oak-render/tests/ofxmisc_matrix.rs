//! GPU pixel tests for the Tier-1 matrix/edge/morphology nodes (skip without GPU).
use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

fn texture_value(t: Texture) -> NodeValue { NodeValue::Texture(oak_node::handle::make_owned(t)) }
fn gpu() -> bool { oak_core::backend::shared_gpu_or_skip("an OFX-Misc GPU test").is_some() }
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

const COLORMATRIX: &str = "org.olivevideoeditor.Olive.colormatrix";
const EDGEDETECT: &str = "org.olivevideoeditor.Olive.edgedetect";
const DILATE: &str = "org.olivevideoeditor.Olive.dilate";
const ERODE: &str = "org.olivevideoeditor.Olive.erode";

/// A 16x16 F32 frame painted pixel by pixel from `paint(x, y)`.
fn painted_frame(paint: impl Fn(usize, usize) -> [f32; 4]) -> Texture {
    let mut f = oak_render::eval::generate_frame(Rational::new(0, 1), (16, 16), PixelFormat::F32).unwrap();
    let stride = f.linesize_bytes() as usize;
    for y in 0..16 {
        for x in 0..16 {
            let at = y * stride + x * 16;
            for (c, v) in paint(x, y).iter().enumerate() {
                f.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    Texture::wrap_frame(f)
}

/// A row carrying just the texture effect input.
fn row_with(texture: Texture) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(texture));
    row
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

/// Identity matrix (the `m0..m15` input defaults): the color passes
/// through unchanged.
#[test]
fn colormatrix_identity_keeps_color() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let rgba = [0.25, 0.5, 0.75, 1.0];
    let out = eval_node_row(COLORMATRIX, row_with(filled_frame((16, 16), rgba)), None);
    assert_pixel(pixel_at(&out, 8, 8), rgba, "identity center");
    assert_pixel(pixel_at(&out, 0, 0), rgba, "identity corner");
}

/// Red/green swap matrix (`m0..m15` row-major: R <- G and G <- R, the
/// rest identity) maps (1, 0, 0, 1) to (0, 1, 0, 1).
#[test]
fn colormatrix_swap_red_green() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let mut row = row_with(filled_frame((16, 16), [1.0, 0.0, 0.0, 1.0]));
    let swap = [
        0.0, 1.0, 0.0, 0.0, // R <- (R, G, B, A)
        1.0, 0.0, 0.0, 0.0, // G <- (R, G, B, A)
        0.0, 0.0, 1.0, 0.0, // B <- (R, G, B, A)
        0.0, 0.0, 0.0, 1.0, // A <- (R, G, B, A)
    ];
    for (i, v) in swap.iter().enumerate() {
        row.insert(format!("m{i}"), NodeValue::Float(*v));
    }
    let out = eval_node_row(COLORMATRIX, row, None);
    assert_pixel(pixel_at(&out, 8, 8), [0.0, 1.0, 0.0, 1.0], "swap");
}

/// The Sobel magnitude of a black/white step is 1 + 2 + 1 = 4.0 on the
/// two columns either side of the split (7 and 8) and 0.0 elsewhere in
/// RGB; the alpha channel passes through. A threshold above the
/// magnitude zeroes the whole frame.
#[test]
fn edgedetect_half_black_white_lights_boundary_columns() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let split = painted_frame(|x, _| {
        if x < 8 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }
    });
    let mut row = row_with(split);
    row.insert("threshold_in".to_string(), NodeValue::Float(0.5));
    let out = eval_node_row(EDGEDETECT, row, None);
    for y in 0..16 {
        for x in 0..16 {
            let px = pixel_at(&out, x, y);
            let rgb = if x == 7 || x == 8 { 4.0 } else { 0.0 };
            assert_pixel(px, [rgb, rgb, rgb, 1.0], &format!("edgedetect ({x},{y})"));
        }
    }

    // Threshold above 4.0: the step drops every magnitude.
    let mut row = row_with(painted_frame(|x, _| {
        if x < 8 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }
    }));
    row.insert("threshold_in".to_string(), NodeValue::Float(10.0));
    let out = eval_node_row(EDGEDETECT, row, None);
    assert_pixel(pixel_at(&out, 7, 8), [0.0, 0.0, 0.0, 1.0], "edgedetect thresholded");
}

/// A single white pixel grows to its full 3x3 neighborhood at radius 1;
/// the rest of the frame stays black.
#[test]
fn dilate_single_pixel_grows_to_3x3() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let white = [1.0, 1.0, 1.0, 1.0];
    let black = [0.0, 0.0, 0.0, 1.0];
    let mut row = row_with(painted_frame(|x, y| if (x, y) == (8, 8) { white } else { black }));
    row.insert("radius_in".to_string(), NodeValue::Float(1.0));
    let out = eval_node_row(DILATE, row, None);
    for y in 0..16 {
        for x in 0..16 {
            let want = if (7..=9).contains(&x) && (7..=9).contains(&y) { white } else { black };
            assert_pixel(pixel_at(&out, x, y), want, &format!("dilate ({x},{y})"));
        }
    }
}

/// A 3x3 white block shrinks to its center pixel at radius 1; the rest
/// of the frame stays black.
#[test]
fn erode_white_block_shrinks_to_center() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let white = [1.0, 1.0, 1.0, 1.0];
    let black = [0.0, 0.0, 0.0, 1.0];
    let mut row = row_with(painted_frame(|x, y| {
        if (7..=9).contains(&x) && (7..=9).contains(&y) { white } else { black }
    }));
    row.insert("radius_in".to_string(), NodeValue::Float(1.0));
    let out = eval_node_row(ERODE, row, None);
    for y in 0..16 {
        for x in 0..16 {
            let want = if (x, y) == (8, 8) { white } else { black };
            assert_pixel(pixel_at(&out, x, y), want, &format!("erode ({x},{y})"));
        }
    }
}
