//! GPU pixel tests for the Tier-1 color nodes (skip when no GPU adapter).
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

const COLORCORRECT: &str = "org.olivevideoeditor.Olive.colorcorrect";
const GAMMA: &str = "org.olivevideoeditor.Olive.gamma";
const SATURATION: &str = "org.olivevideoeditor.Olive.saturation";
const INVERT: &str = "org.olivevideoeditor.Olive.invert";
const CLAMP: &str = "org.olivevideoeditor.Olive.clamp";
const GRADE: &str = "org.olivevideoeditor.Olive.grade";

/// A row carrying just the effect input, with `rgba` painted over the
/// whole 8x8 frame.
fn row_with(rgba: [f32; 4]) -> NodeValueRow {
    let mut row = NodeValueRow::new();
    row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), rgba)));
    row
}

/// Insert float parameters (input id -> value).
fn set_floats(row: &mut NodeValueRow, params: &[(&str, f64)]) {
    for (id, v) in params {
        row.insert((*id).to_string(), NodeValue::Float(*v));
    }
}

/// Insert boolean parameters (input id -> value).
fn set_bools(row: &mut NodeValueRow, params: &[(&str, bool)]) {
    for (id, v) in params {
        row.insert((*id).to_string(), NodeValue::Boolean(*v));
    }
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

/// ColorCorrect on mid gray: the saturation lerp is an identity on gray,
/// so the chain reduces to the contrast/gamma/gain/offset passes.
/// `0.5 -> pow(0.5/0.18, 1.2)*0.18 = 0.6133516 -> ^(1/0.8) = 0.5427963
/// -> *1.1 = 0.5970759 -> +0.02 = 0.6170759`.
#[test]
fn colorcorrect_mid_gray_control_chain() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(
        &mut row,
        &[
            ("saturation_in", 0.5),
            ("contrast_in", 1.2),
            ("gamma_in", 0.8),
            ("gain_in", 1.1),
            ("offset_in", 0.02),
        ],
    );
    let frame = eval_node_row(COLORCORRECT, row, None);
    for (x, y) in [(0, 0), (4, 4), (7, 7)] {
        assert_pixel(
            pixel_at(&frame, x, y),
            [0.6170759, 0.6170759, 0.6170759, 1.0],
            &format!("colorcorrect ({x},{y})"),
        );
    }
}

/// Gamma 2.0 is the square root: `pow(0.5, 1/2) = 0.7071068`. Alpha is
/// untouched.
#[test]
fn gamma_mid_gray_square_root() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(&mut row, &[("gamma_in", 2.0)]);
    let frame = eval_node_row(GAMMA, row, None);
    assert_pixel(
        pixel_at(&frame, 4, 4),
        [0.70710677, 0.70710677, 0.70710677, 1.0],
        "gamma",
    );
}

/// Saturation 0.5 on a non-gray pixel lerps halfway to its Rec. 709
/// luma (`0.2126*0.6 + 0.7152*0.4 + 0.0722*0.2 = 0.42808`), and is an
/// identity on gray. Alpha is untouched.
#[test]
fn saturation_half_lerps_to_luma() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = row_with([0.6, 0.4, 0.2, 1.0]);
    set_floats(&mut row, &[("saturation_in", 0.5)]);
    let frame = eval_node_row(SATURATION, row, None);
    assert_pixel(
        pixel_at(&frame, 4, 4),
        [0.51404, 0.41404, 0.31404, 1.0],
        "saturation on color",
    );

    let mut gray = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(&mut gray, &[("saturation_in", 0.5)]);
    let gray_frame = eval_node_row(SATURATION, gray, None);
    assert_pixel(
        pixel_at(&gray_frame, 4, 4),
        [0.5, 0.5, 0.5, 1.0],
        "saturation on gray",
    );
}

/// Invert defaults to all four toggles on: mid gray 0.5 becomes 0.5 in
/// RGB but the alpha flips to 0.0. With the alpha toggle off, an
/// asymmetric pixel flips only its color channels.
#[test]
fn invert_per_channel_toggles() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    // All four toggles default to on.
    let frame = eval_node_row(INVERT, row_with([0.5, 0.5, 0.5, 1.0]), None);
    assert_pixel(pixel_at(&frame, 4, 4), [0.5, 0.5, 0.5, 0.0], "invert defaults");

    // Alpha toggle off: only the color channels invert.
    let mut row = row_with([0.25, 0.75, 0.5, 1.0]);
    set_bools(
        &mut row,
        &[
            ("invert_r_in", true),
            ("invert_g_in", true),
            ("invert_b_in", true),
            ("invert_a_in", false),
        ],
    );
    let frame = eval_node_row(INVERT, row, None);
    assert_pixel(
        pixel_at(&frame, 4, 4),
        [0.75, 0.25, 0.5, 1.0],
        "invert without alpha",
    );
}

/// Clamp raises mid gray to the 0.6 lower bound (alpha is clamped too,
/// matching the reference's `processA` default), and a 0.4 upper bound
/// pulls every channel down to 0.4.
#[test]
fn clamp_bounds_every_channel() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(&mut row, &[("min_in", 0.6), ("max_in", 1.0)]);
    let frame = eval_node_row(CLAMP, row, None);
    assert_pixel(pixel_at(&frame, 4, 4), [0.6, 0.6, 0.6, 1.0], "clamp to min");

    let mut low = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(&mut low, &[("min_in", 0.0), ("max_in", 0.4)]);
    let low_frame = eval_node_row(CLAMP, low, None);
    assert_pixel(
        pixel_at(&low_frame, 4, 4),
        [0.4, 0.4, 0.4, 0.4],
        "clamp to max",
    );
}

/// Grade stretch + gamma: with the black point at 0.1 and the white
/// point at 0.6 the slope is `(1-0)/(0.6-0.1) = 2` and the offset
/// `-0.2`, so 0.5 maps to 0.8, then the gamma 2.0 pass gives
/// `pow(0.8, 0.5) = 0.8944272`. The defaults are an identity.
#[test]
fn grade_stretch_and_gamma() {
    if !gpu() {
        eprintln!("no adapter; skipping");
        return;
    }
    let mut row = row_with([0.5, 0.5, 0.5, 1.0]);
    set_floats(
        &mut row,
        &[
            ("blackpoint_in", 0.1),
            ("whitepoint_in", 0.6),
            ("black_in", 0.0),
            ("white_in", 1.0),
            ("gamma_in", 2.0),
        ],
    );
    let frame = eval_node_row(GRADE, row, None);
    assert_pixel(
        pixel_at(&frame, 4, 4),
        [0.8944272, 0.8944272, 0.8944272, 1.0],
        "grade stretch",
    );

    // Default points and gamma: identity.
    let frame = eval_node_row(GRADE, row_with([0.5, 0.5, 0.5, 1.0]), None);
    assert_pixel(pixel_at(&frame, 4, 4), [0.5, 0.5, 0.5, 1.0], "grade identity");
}
