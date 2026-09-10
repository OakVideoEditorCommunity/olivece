//! GPU pixel tests for the Tier-1 blur/sharpen nodes (skip without GPU).
use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

fn texture_value(t: Texture) -> NodeValue { NodeValue::Texture(oak_node::handle::make_owned(t)) }
fn gpu() -> bool { oak_core::backend::GpuContext::shared().is_some() }
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

const DIRBLUR: &str = "org.olivevideoeditor.Olive.dirblur";
const SHARPEN: &str = "org.olivevideoeditor.Olive.sharpen";

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

/// A row carrying the effect input plus any explicitly set float inputs.
fn row_with(texture: Texture, scalars: &[(&str, f64)]) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(texture));
    for (name, v) in scalars {
        row.insert((*name).to_string(), NodeValue::Float(*v));
    }
    row
}

/// Assert every channel of `px` is within `tol` of `want`.
fn assert_close(px: [f32; 4], want: [f32; 4], tol: f32, what: &str) {
    for c in 0..4 {
        assert!(
            (px[c] - want[c]).abs() <= tol,
            "{what}: channel {c}: got {px:?}, want {want:?} (tol {tol})"
        );
    }
}

/// Zero amount (the input default) collapses the tap spacing to zero, so
/// the directional blur is an exact identity anchored to the sampled
/// coordinate; it still renders on the GPU.
#[test]
fn dirblur_zero_amount_is_identity() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let src = painted_frame(|x, y| [x as f32 / 15.0, y as f32 / 15.0, 0.25, 1.0]);
    let out = eval_node_row(DIRBLUR, row_with(src, &[]), None);
    for (x, y) in [(0, 0), (5, 3), (8, 8), (14, 12), (15, 15)] {
        assert_close(
            pixel_at(&out, x, y),
            [x as f32 / 15.0, y as f32 / 15.0, 0.25, 1.0],
            1e-3,
            &format!("dirblur zero amount ({x},{y})"),
        );
    }
}

/// A one-pixel-wide white vertical line smears along +x at angle 0:
/// pixels either side pick up the line, the line itself dims, and
/// nothing leaks along y.
#[test]
fn dirblur_smears_horizontally_without_vertical_spread() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let src = painted_frame(|x, y| {
        if x == 8 && (4..12).contains(&y) { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }
    });
    let out = eval_node_row(
        DIRBLUR,
        row_with(src, &[("amount_in", 2.0), ("angle_in", 0.0)]),
        None,
    );

    let spread_l = pixel_at(&out, 6, 8)[0];
    let spread_r = pixel_at(&out, 10, 8)[0];
    assert!((0.05..0.3).contains(&spread_l), "pixel left of the line picks up the smear: {spread_l}");
    assert!((spread_l - spread_r).abs() < 0.02, "smear is symmetric about the line: {spread_l} vs {spread_r}");

    // The line dims, but not to nothing: the 16 taps sit 2*2/15 px apart,
    // so they land at 8 + t*4/15 for t = -7.5..7.5 and the sampler's
    // 1 px-wide linear reconstruction gives them 13/15, 9/15, 5/15 and
    // 1/15 of the line each, twice over, leaving the center pixel at
    // 2*(13+9+5+1)/(15*16) = 7/30.
    let on_line = pixel_at(&out, 8, 8)[0];
    assert!(
        (on_line - 7.0 / 30.0).abs() < 0.02,
        "a one-pixel line dims to ~7/30 of its brightness: {on_line}"
    );

    // The smear redistributes the line's brightness without adding or
    // losing any: the linear-filter triangle has unit area, so the 16
    // output pixels (each the average of 16 unit-spaced samples) still
    // sum to the whole line. This holds whatever the filter mode.
    let row_energy: f32 = (0..16).map(|x| pixel_at(&out, x, 8)[0]).sum();
    assert!(
        (row_energy - 1.0).abs() < 0.02,
        "the row keeps the line's total brightness: {row_energy}"
    );

    assert!(pixel_at(&out, 5, 8)[0] < 0.01, "two pixels out stays black");
    assert!(pixel_at(&out, 8, 3)[0] < 0.01, "no spread above the line");
    assert!(pixel_at(&out, 8, 12)[0] < 0.01, "no spread below the line");
    let alpha = pixel_at(&out, 6, 8)[3];
    assert!((alpha - 1.0).abs() < 1e-3, "alpha passes through: {alpha}");
}

/// At angle 90 the smear runs along +y: the same one-pixel line laid
/// horizontally bleeds into the rows above and below it, and not along
/// its own axis.
#[test]
fn dirblur_angle_90_smears_vertically() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let src = painted_frame(|x, y| {
        if y == 8 && (4..12).contains(&x) { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }
    });
    let out = eval_node_row(
        DIRBLUR,
        row_with(src, &[("amount_in", 2.0), ("angle_in", 90.0)]),
        None,
    );

    let above = pixel_at(&out, 8, 6)[0];
    let below = pixel_at(&out, 8, 10)[0];
    assert!((0.05..0.3).contains(&above), "row above the line picks up the smear: {above}");
    assert!((above - below).abs() < 0.02, "smear is symmetric about the line: {above} vs {below}");

    // Same arithmetic as the horizontal case, rotated: 7/30 of the line.
    let on_line = pixel_at(&out, 8, 8)[0];
    assert!(
        (on_line - 7.0 / 30.0).abs() < 0.02,
        "a one-pixel line dims to ~7/30 of its brightness: {on_line}"
    );

    let column_energy: f32 = (0..16).map(|y| pixel_at(&out, 8, y)[0]).sum();
    assert!(
        (column_energy - 1.0).abs() < 0.02,
        "the column keeps the line's total brightness: {column_energy}"
    );

    assert!(pixel_at(&out, 8, 5)[0] < 0.01, "two rows out stays black");
    assert!(pixel_at(&out, 3, 6)[0] < 0.01, "no spread left of the line");
    assert!(pixel_at(&out, 1, 8)[0] < 0.01, "no spread along the line's axis");
}

/// A flat region is untouched by the unsharp mask whatever the amount
/// (the default 1.0 included), and an explicit zero amount is an exact
/// identity on a gradient.
#[test]
fn sharpen_flat_and_zero_amount_pass_through() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let rgba = [0.4, 0.6, 0.8, 1.0];
    let out = eval_node_row(SHARPEN, row_with(filled_frame((16, 16), rgba), &[]), None);
    assert_close(pixel_at(&out, 8, 8), rgba, 1e-3, "sharpen default amount on a flat frame");

    let src = painted_frame(|x, y| [x as f32 / 15.0, y as f32 / 15.0, 0.25, 1.0]);
    let out = eval_node_row(SHARPEN, row_with(src, &[("amount_in", 0.0)]), None);
    for (x, y) in [(0, 0), (5, 3), (8, 8), (14, 12), (15, 15)] {
        assert_close(
            pixel_at(&out, x, y),
            [x as f32 / 15.0, y as f32 / 15.0, 0.25, 1.0],
            1e-3,
            &format!("sharpen zero amount ({x},{y})"),
        );
    }
}

/// At a black/white step the 3x3 box blur is 2/3 white on the bright
/// side (overshoot to 4/3) and 1/3 white on the dark side (undershoot to
/// -1/3); flat columns pass through and the result is not clamped.
#[test]
fn sharpen_overshoots_and_undershoots_a_step_edge() {
    if !gpu() { eprintln!("no adapter; skipping"); return; }
    let src = painted_frame(|x, _| {
        if x < 8 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }
    });
    let out = eval_node_row(SHARPEN, row_with(src, &[("amount_in", 1.0)]), None);

    let bright = pixel_at(&out, 8, 8);
    assert!((bright[0] - 4.0 / 3.0).abs() < 0.01, "overshoot on the bright side: {bright:?}");
    assert!(bright[0] > 1.0, "overshoot is not clamped: {bright:?}");
    assert!((bright[3] - 1.0).abs() < 1e-3, "alpha passes through: {bright:?}");

    let dark = pixel_at(&out, 7, 8);
    assert!((dark[0] + 1.0 / 3.0).abs() < 0.01, "undershoot on the dark side: {dark:?}");
    assert!(dark[0] < 0.0, "undershoot is not clamped: {dark:?}");
    assert!((dark[3] - 1.0).abs() < 1e-3, "alpha passes through: {dark:?}");

    assert_close(pixel_at(&out, 6, 8), [0.0, 0.0, 0.0, 1.0], 1e-3, "flat black column");
    assert_close(pixel_at(&out, 9, 8), [1.0, 1.0, 1.0, 1.0], 1e-3, "flat white column");
}
