//! GPU pixel tests for the Tier-1 geometry/generator nodes (skip without GPU).
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

const POSITION: &str = "org.olivevideoeditor.Olive.position";
const MIRROR: &str = "org.olivevideoeditor.Olive.mirror";
const CHECKERBOARD: &str = "org.olivevideoeditor.Olive.checkerboard";
const COLORBARS: &str = "org.olivevideoeditor.Olive.colorbars";
const RAMP: &str = "org.olivevideoeditor.Olive.ramp";

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// Paint one RGBA pixel into a CPU frame (these F32 frames are 16 bytes a
/// pixel).
fn paint(frame: &mut oak_core::texture::Frame, x: usize, y: usize, rgba: [f32; 4]) {
    let at = y * frame.linesize_bytes() as usize + x * 16;
    for (c, v) in rgba.iter().enumerate() {
        frame.data[at + c * 4..at + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Opaque black with a single white pixel at `(x, y)`.
fn white_pixel_frame(size: (i32, i32), x: usize, y: usize) -> Texture {
    let mut frame = filled_frame(size, BLACK).to_frame().expect("cpu frame");
    paint(&mut frame, x, y, WHITE);
    Texture::wrap_frame(frame)
}

/// Opaque black with the left half (`x < size.0 / 2`) painted white.
fn left_white_frame(size: (i32, i32)) -> Texture {
    let mut frame = filled_frame(size, BLACK).to_frame().expect("cpu frame");
    for y in 0..size.1 as usize {
        for x in 0..(size.0 / 2) as usize {
            paint(&mut frame, x, y, WHITE);
        }
    }
    Texture::wrap_frame(frame)
}

/// Assert the four channels of a pixel within the tests' 0.02 tolerance.
fn assert_pixel(px: [f32; 4], want: [f32; 4], what: &str) {
    for c in 0..4 {
        assert!(
            (px[c] - want[c]).abs() < 0.02,
            "{what}: channel {c}: got {px:?}, want {want:?}"
        );
    }
}

/// Position with a whole-pixel `offset_in`: the white pixel at (2, 3)
/// lands at (5, 5) (frame rows run downward, so the offset moves the image
/// right and down), and the source pixel it vacated reads black.
#[test]
fn position_shifts_a_white_pixel_by_whole_pixels() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = NodeValueRow::new();
    row.insert(
        "tex_in".to_string(),
        texture_value(white_pixel_frame((8, 8), 2, 3)),
    );
    row.insert("offset_in".to_string(), NodeValue::Vec2([3.0, 2.0]));
    let frame = eval_node_row(POSITION, row, None);
    assert_pixel(pixel_at(&frame, 5, 5), WHITE, "position moved pixel");
    // Off-frame reads are transparent (never the clamped edge pixels):
    // the composite below shows through instead.
    assert_pixel(
        pixel_at(&frame, 2, 3),
        [0.0, 0.0, 0.0, 0.0],
        "position vacated pixel",
    );
    assert_pixel(
        pixel_at(&frame, 0, 0),
        [0.0, 0.0, 0.0, 0.0],
        "position untouched corner",
    );
}

/// Mirror with `horizontal_in` on flips a one-sided white block about the
/// frame center: the left-half white block moves to the right half.
/// `vertical_in` stays at its default (off), so rows are untouched.
#[test]
fn mirror_horizontal_flips_the_white_block_to_the_other_side() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = NodeValueRow::new();
    row.insert(
        "tex_in".to_string(),
        texture_value(left_white_frame((8, 8))),
    );
    row.insert("horizontal_in".to_string(), NodeValue::Boolean(true));
    let frame = eval_node_row(MIRROR, row, None);
    assert_pixel(pixel_at(&frame, 5, 3), WHITE, "mirror moved block");
    assert_pixel(pixel_at(&frame, 2, 3), BLACK, "mirror vacated block");
    assert_pixel(pixel_at(&frame, 7, 7), WHITE, "mirror bottom-right");
    assert_pixel(pixel_at(&frame, 0, 7), BLACK, "mirror bottom-left");
}

/// Checkerboard with 4px boxes on an 8x8 frame: the parity of the cell
/// index sum picks the color, with `color1_in` (red) in the cells reaching
/// (0, 0), (3, 3) and (7, 7) and `color2_in` (green) in (7, 0), (0, 7),
/// (0, 4) and (4, 0).
#[test]
fn checkerboard_alternates_colors_by_cell_parity() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = NodeValueRow::new();
    row.insert("size_in".to_string(), NodeValue::Vec2([4.0, 4.0]));
    row.insert(
        "color1_in".to_string(),
        NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
    );
    row.insert(
        "color2_in".to_string(),
        NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
    );
    let frame = eval_node_row(CHECKERBOARD, row, Some((8, 8)));
    for (x, y) in [(0, 0), (3, 3), (7, 7)] {
        assert_pixel(
            pixel_at(&frame, x, y),
            [1.0, 0.0, 0.0, 1.0],
            &format!("checkerboard color1 ({x},{y})"),
        );
    }
    for (x, y) in [(7, 0), (0, 7), (0, 4), (4, 0)] {
        assert_pixel(
            pixel_at(&frame, x, y),
            [0.0, 1.0, 0.0, 1.0],
            &format!("checkerboard color2 ({x},{y})"),
        );
    }
}

/// Color bars at the default SMPTE 75% standard: the top-left pixel is the
/// 75% white bar, the pixel in the second bar is 75% yellow, and the
/// top-left of the mid strip is the 75% blue bar.
#[test]
fn colorbars_75_percent_white_yellow_and_blue_bars() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let frame = eval_node_row(COLORBARS, NodeValueRow::new(), Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 0, 0),
        [0.75, 0.75, 0.75, 1.0],
        "colorbars 75% white bar",
    );
    assert_pixel(
        pixel_at(&frame, 1, 0),
        [0.75, 0.75, 0.0, 1.0],
        "colorbars yellow bar",
    );
    assert_pixel(
        pixel_at(&frame, 0, 5),
        [0.0, 0.0, 0.75, 1.0],
        "colorbars mid-strip blue bar",
    );
}

/// Ramp from (default) black at `point0_in` to (default) white at
/// `point1_in`: the gradient is the projection onto the p0->p1 axis, so an
/// axis spanning (-4.5, 0) -> (3.5, 0) gives 0.5 at the pixel whose center
/// is the midpoint, 0.125 one eighth of the way in, and 1.0 at the end.
#[test]
fn ramp_from_black_to_white_is_half_at_the_midpoint() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = NodeValueRow::new();
    row.insert("point0_in".to_string(), NodeValue::Vec2([-4.5, 0.0]));
    row.insert("point1_in".to_string(), NodeValue::Vec2([3.5, 0.0]));
    let frame = eval_node_row(RAMP, row, Some((8, 8)));
    assert_pixel(
        pixel_at(&frame, 3, 3),
        [0.5, 0.5, 0.5, 1.0],
        "ramp midpoint",
    );
    assert_pixel(
        pixel_at(&frame, 0, 3),
        [0.125, 0.125, 0.125, 1.0],
        "ramp near point0",
    );
    assert_pixel(pixel_at(&frame, 7, 3), WHITE, "ramp at point1");
}

/// Whole-pixel translation past the frame edge leaves transparent
/// pixels (never the clamped edge column): +100px empties the frame.
#[test]
fn position_off_frame_is_transparent() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), WHITE)));
    row.insert("offset_in".to_string(), NodeValue::Vec2([100.0, 0.0]));
    let frame = eval_node_row(POSITION, row, None);
    for y in 0..8usize {
        for x in 0..8usize {
            assert_pixel(
                pixel_at(&frame, x, y),
                [0.0, 0.0, 0.0, 0.0],
                "off-frame content must be transparent",
            );
        }
    }
}
