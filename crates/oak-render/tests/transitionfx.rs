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

//! GPU pixel tests for the layer-form transition node
//! (`org.olivevideoeditor.Olive.transitionfx`): cross dissolve, fade, wipe
//! and slide over `tex_in` + `blend_in`, with the progress either set on
//! the row or auto-filled by the renderer from the adjustment layer's own
//! span. Skips without a GPU adapter.

use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};
use oak_node::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

const TRANSITIONFX: &str = "org.olivevideoeditor.Olive.transitionfx";

fn texture_value(t: Texture) -> NodeValue { NodeValue::Texture(oak_node::handle::make_owned(t)) }
fn gpu() -> bool { oak_core::backend::GpuContext::shared().is_some() }
fn filled_frame(size: (i32, i32), rgba: [f32; 4]) -> Texture {
	let mut f = oak_render::eval::generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	for px in f.data.chunks_exact_mut(16) { for (c, v) in px.chunks_exact_mut(4).zip(rgba) { c.copy_from_slice(&v.to_le_bytes()); } }
	Texture::wrap_frame(f)
}
/// An 8x8 frame whose red channel carries each column's centre `u`
/// (`(x + 0.5) / width`), so a sample taken at `u` reads back as exactly
/// `u`: the slide tests use it to tell which part of the picture landed
/// where.
fn gradient_frame_x(size: (i32, i32)) -> Texture {
	let mut f = oak_render::eval::generate_frame(Rational::new(0, 1), size, PixelFormat::F32).unwrap();
	let w = size.0 as usize;
	for (i, px) in f.data.chunks_exact_mut(16).enumerate() {
		let u = ((i % w) as f32 + 0.5) / w as f32;
		for (c, v) in px.chunks_exact_mut(4).zip([u, 0.0, 0.0, 1.0]) { c.copy_from_slice(&v.to_le_bytes()); }
	}
	Texture::wrap_frame(f)
}
fn pixel_at(frame: &oak_core::texture::Frame, x: usize, y: usize) -> [f32; 4] {
	let stride = frame.linesize_bytes() as usize;
	let at = y * stride + x * 16;
	let mut out = [0f32; 4];
	for c in 0..4 { out[c] = f32::from_le_bytes(frame.data[at + c*4..at + c*4 + 4].try_into().unwrap()); }
	out
}
/// Evaluate one node row. `layer_progress` is the adjustment-layer sweep
/// position the graph driver records before walking an effect chain; the
/// shader job picks it up for `progress_in` when the node left the input
/// out of its params (the auto-fill path).
fn eval_node_row(
	type_id: &str,
	inputs: NodeValueRow,
	frame_size: Option<(i32, i32)>,
	layer_progress: Option<f64>,
) -> oak_core::texture::Frame {
	use oak_node::traverser::RenderHooks;
	let (core, behavior) = oak_node::factory::Factory::global().create_any(type_id).expect("node type registered");
	let mut table = NodeValueTable::default();
	behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
	let mut hooks = oak_render::eval::RenderEvalHooks::new();
	hooks.frame_size = frame_size;
	hooks.layer_progress = layer_progress;
	hooks.resolve(oak_node::id::NodeId::INVALID, &inputs, &mut table);
	let Some(NodeValue::Texture(handle)) = table.get(ValueType::Texture) else { panic!("{type_id}: no texture produced") };
	if handle.ctx.is_null() { panic!("{type_id}: null texture produced"); }
	let tex = unsafe { oak_node::handle::get_checked::<Texture>(handle) }.expect("resolved texture");
	assert!(matches!(tex, Texture::Gpu { .. }), "{type_id}: must render on the GPU");
	tex.to_frame().expect("readback")
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

/// The two-picture row the shader job carries: `tex_in` (From) and
/// `blend_in` (To), each painted over the whole 8x8 frame.
fn transition_row(tex: [f32; 4], blend: [f32; 4]) -> NodeValueRow {
	let mut row = NodeValueRow::new();
	row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), tex)));
	row.insert("blend_in".to_string(), texture_value(filled_frame((8, 8), blend)));
	row
}

/// Style combo index for a row (cross dissolve is 0, the default).
fn style(row: &mut NodeValueRow, index: i64) {
	row.insert("type_in".to_string(), NodeValue::Combo(index));
}

/// Halfway cross dissolve lerps red and green to olive.
#[test]
fn crossdissolve_half_progress_blends() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let mut row = transition_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
	row.insert("progress_in".to_string(), NodeValue::Float(0.5));
	let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), None);
	assert_pixel(pixel_at(&frame, 2, 2), [0.5, 0.5, 0.0, 1.0], "cross dissolve 0.5");
}

/// Progress 0 and 1 are the two pictures untouched.
#[test]
fn crossdissolve_ends_are_the_inputs() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	for (progress, want, what) in [
		(0.0, [1.0, 0.0, 0.0, 1.0], "cross dissolve 0"),
		(1.0, [0.0, 1.0, 0.0, 1.0], "cross dissolve 1"),
	] {
		let mut row = transition_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
		row.insert("progress_in".to_string(), NodeValue::Float(progress));
		let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), None);
		assert_pixel(pixel_at(&frame, 4, 4), want, what);
	}
}

/// Fade mixes the picture up out of `color_in`: 0 is the colour, 1 the
/// picture, and a blue `color_in` proves the uniform binds by name (the
/// shader declares no `blend_in` at all). The row always carries the
/// input's default, exactly as the traverser fills every input in — a
/// row without `color_in` would leave the uniform zero-filled, which for
/// a colour is transparent black rather than the input's opaque default.
#[test]
fn fade_dips_through_the_color() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let cases: [(f64, [f64; 4], [f32; 4]); 3] = [
		(0.0, [0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]),
		(0.5, [0.0, 0.0, 1.0, 1.0], [0.5, 0.0, 0.5, 1.0]),
		(1.0, [0.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]),
	];
	for (progress, color, want) in cases {
		let mut row = transition_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
		style(&mut row, 1);
		row.insert("progress_in".to_string(), NodeValue::Float(progress));
		row.insert("color_in".to_string(), NodeValue::Color(color));
		let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), None);
		assert_pixel(pixel_at(&frame, 3, 5), want, "fade");
	}
}

/// Wipe at half progress: the incoming picture covers the half the
/// direction comes from, the outgoing keeps the rest. All four combo
/// entries are the same path (axis and sign are two steps and two mixes).
#[test]
fn wipe_directions_place_the_incoming_picture() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let red = [1.0, 0.0, 0.0, 1.0];
	let green = [0.0, 1.0, 0.0, 1.0];
	// (direction, pixel, expected) — tex_in is red (From), blend_in green (To).
	let cases: [(i64, (usize, usize), [f32; 4]); 8] = [
		(0, (2, 4), green),
		(0, (6, 4), red),
		(1, (2, 4), red),
		(1, (6, 4), green),
		(2, (4, 2), green),
		(2, (4, 6), red),
		(3, (4, 2), red),
		(3, (4, 6), green),
	];
	for (direction, (x, y), want) in cases {
		let mut row = transition_row(red, green);
		style(&mut row, 2);
		row.insert("progress_in".to_string(), NodeValue::Float(0.5));
		row.insert("direction_in".to_string(), NodeValue::Combo(direction));
		let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), None);
		assert_pixel(
			pixel_at(&frame, x, y),
			want,
			&format!("wipe direction {direction} at ({x},{y})"),
		);
	}
}

/// Slide: a gradient stands in for the incoming picture, so the value at
/// a pixel names the column that landed there. At progress 0.25 the
/// incoming enters from the left with its trailing quarter on screen (the
/// frame shows the gradient's `[0.75, 1]` columns), while the outgoing
/// part shows the untouched `tex_in`. The reversed direction mirrors it.
#[test]
fn slide_moves_both_pictures_the_named_way() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let red = [1.0, 0.0, 0.0, 1.0];
	// (direction, pixel, expected red channel) at progress 0.25.
	let cases: [(i64, (usize, usize), f32); 4] = [
		(0, (0, 4), 0.8125),
		(0, (6, 4), 1.0),
		(1, (0, 4), 1.0),
		(1, (7, 4), 0.1875),
	];
	for (direction, (x, y), red_channel) in cases {
		let mut row = NodeValueRow::new();
		row.insert("tex_in".to_string(), texture_value(filled_frame((8, 8), red)));
		row.insert("blend_in".to_string(), texture_value(gradient_frame_x((8, 8))));
		style(&mut row, 3);
		row.insert("progress_in".to_string(), NodeValue::Float(0.25));
		row.insert("direction_in".to_string(), NodeValue::Combo(direction));
		let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), None);
		assert_pixel(
			pixel_at(&frame, x, y),
			[red_channel, 0.0, 0.0, 1.0],
			&format!("slide direction {direction} at ({x},{y})"),
		);
	}
}

/// Auto-fill: the row carries no `progress_in` at all (the node dropped
/// it), so the renderer inserts the adjustment layer's own progress. The
/// same row renders differently for each hook value — without the fill the
/// uniform would stay 0 and both calls would be pure `tex_in`.
#[test]
fn progress_is_filled_from_the_adjustment_layer() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let row = transition_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
	let frame = eval_node_row(TRANSITIONFX, row.clone(), Some((8, 8)), Some(0.5));
	assert_pixel(pixel_at(&frame, 4, 4), [0.5, 0.5, 0.0, 1.0], "auto progress 0.5");
	let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), Some(0.75));
	assert_pixel(pixel_at(&frame, 4, 4), [0.25, 0.75, 0.0, 1.0], "auto progress 0.75");
}

/// An explicit `progress_in` on the row wins over the layer sweep.
#[test]
fn explicit_progress_beats_the_layer_sweep() {
	if !gpu() { eprintln!("no adapter; skipping"); return; }
	let mut row = transition_row([1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]);
	row.insert("progress_in".to_string(), NodeValue::Float(0.25));
	let frame = eval_node_row(TRANSITIONFX, row, Some((8, 8)), Some(0.75));
	assert_pixel(pixel_at(&frame, 4, 4), [0.75, 0.25, 0.0, 1.0], "explicit progress");
}
