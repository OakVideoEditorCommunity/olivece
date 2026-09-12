//! GPU pixel tests for the Tier-1 merge nodes (skip without GPU).
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

const DISSOLVE: &str = "org.olivevideoeditor.Olive.dissolve";
const KEYMIX: &str = "org.olivevideoeditor.Olive.keymix";
const PREMULT: &str = "org.olivevideoeditor.Olive.premult";
const UNPREMULT: &str = "org.olivevideoeditor.Olive.unpremult";

/// Assert the four channels of a pixel against the expected values.
fn assert_pixel(px: [f32; 4], want: [f32; 4], what: &str) {
    for c in 0..4 {
        assert!(
            (px[c] - want[c]).abs() < 1e-3,
            "{what}: channel {c}: got {px:?}, want {want:?}"
        );
    }
}

/// A row with the two picture inputs (and the mask for keymix) painted
/// over the whole 8x8 frame.
fn merge_row(tex: [f32; 4], blend: [f32; 4]) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), tex)));
    row.insert("blend_in".to_string(), texture_value(filled_frame((8, 8), blend)));
    row
}

/// A row with just the effect input, plus the channel combo.
fn channel_row(rgba: [f32; 4], channel_input: &str, channel: i64) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), rgba)));
    row.insert(channel_input.to_string(), NodeValue::Combo(channel));
    row
}

/// Dissolve at mix 0.5 lerps halfway between red and green; the alpha
/// rides the same lerp (both inputs are opaque here).
#[test]
fn dissolve_half_mix_blends_inputs() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = merge_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
    row.insert("mix_in".to_string(), NodeValue::Float(0.5));
    let frame = eval_node_row(DISSOLVE, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [0.5, 0.5, 0.0, 1.0],
        "dissolve mix 0.5",
    );
}

/// Mix 0 must leave the first input untouched.
#[test]
fn dissolve_zero_mix_keeps_first_input() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = merge_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
    row.insert("mix_in".to_string(), NodeValue::Float(0.0));
    let frame = eval_node_row(DISSOLVE, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [1.0, 0.0, 0.0, 1.0],
        "dissolve mix 0",
    );
}

/// KeyMix keys on the mask's alpha: a fully transparent mask keeps the
/// input everywhere.
#[test]
fn keymix_transparent_mask_keeps_input() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = merge_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
    row.insert(
        "mask_in".to_string(),
        texture_value(filled_frame((8, 8), [0.0, 0.0, 0.0, 0.0])),
    );
    let frame = eval_node_row(KEYMIX, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [1.0, 0.0, 0.0, 1.0],
        "keymix zero mask",
    );
}

/// A fully opaque mask selects the blend everywhere.
#[test]
fn keymix_opaque_mask_takes_blend() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = merge_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
    row.insert(
        "mask_in".to_string(),
        texture_value(filled_frame((8, 8), [1.0, 1.0, 1.0, 1.0])),
    );
    let frame = eval_node_row(KEYMIX, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [0.0, 1.0, 0.0, 1.0],
        "keymix opaque mask",
    );
}

/// Premultiply scales RGB by the alpha channel; alpha is untouched.
#[test]
fn premult_scales_rgb_by_alpha() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let row = channel_row([1.0, 0.5, 0.25, 0.5], "premult_channel_in", 4);
    let frame = eval_node_row(PREMULT, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [0.5, 0.25, 0.125, 0.5],
        "premult alpha",
    );
}

/// Unpremultiply is the inverse of premultiply within tolerance.
#[test]
fn unpremult_inverts_premult() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let row = channel_row([0.5, 0.25, 0.125, 0.5], "unpremult_channel_in", 4);
    let frame = eval_node_row(UNPREMULT, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [1.0, 0.5, 0.25, 0.5],
        "unpremult alpha",
    );
}

/// A zero divisor is guarded: the pixel is passed through unchanged
/// rather than blowing up to infinity.
#[test]
fn unpremult_zero_alpha_passes_through() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let row = channel_row([0.5, 0.25, 0.125, 0.0], "unpremult_channel_in", 4);
    let frame = eval_node_row(UNPREMULT, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 2, 2),
        [0.5, 0.25, 0.125, 0.0],
        "unpremult zero guard",
    );
}
