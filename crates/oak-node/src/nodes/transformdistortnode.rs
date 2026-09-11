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

//! Transform distort effect (C++
//! `src/node/src/distort/transform/transformdistortnode.{h,cpp}`,
//! `olive::TransformDistortNode`). In C++ this derives from
//! `MatrixGenerator` (`super::matrix`), which provides the
//! `pos_in`/`rot_in`/`scale_in`/`uniform_scale_in`/`anchor_in` inputs
//! and the `generate_matrix` helper.

/// The transform fragment shader: samples the input through the inverse
/// of the node's pixel-space transform (`transform_in` carries the
/// CPU-inverted matrix; `resolution_in` is auto-filled by the runner).
/// Pixel space is CENTER-origin (C++ `transform.vert` transforms the
/// quad in the frame-centered projection): position (0,0) is the frame
/// center and rotation/scale pivot around the anchor — NOT around the
/// top-left corner, which is what a top-left-origin pixel space would
/// pivot on (a corner pivot swings the image off-frame as it rotates,
/// reading exactly like an unwanted zoom). The fixed fullscreen vertex
/// stage stays unchanged.
const TRANSFORM_FRAG: &str = r#"uniform sampler2D tex_in;
uniform mat4 transform_in;
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main(void) {
    vec2 half_res = resolution_in * 0.5;
    vec2 px = ove_texcoord * resolution_in - half_res;
    vec2 src = (transform_in * vec4(px, 0.0, 1.0)).xy + half_res;
    vec2 uv = src / resolution_in;
    // A transform that pushes content off-frame leaves the vacated
    // region TRANSPARENT (the alternative — the sampler's edge clamp —
    // smears the border pixels across it).
    vec4 col = texture(tex_in, uv);
    float inside = step(0.0, uv.x) * step(0.0, uv.y) * step(uv.x, 1.0) * step(uv.y, 1.0);
    frag_color = col * inside;
}
"#;

use crate::factory::NodeMeta;

use crate::jobs::{Job, ShaderJobPayload};
use crate::node::{Category, Gizmo, NodeBehavior, NodeCore};

/// Parent matrix input id (C++ `k_parent_input`). Type: matrix; no
/// default (identity when unconnected).
pub const PARENT_INPUT: &str = "parent_in";

/// Texture input id (C++ `k_texture_input`). Type: texture; flags:
/// not-keyframable; this is the node's effect input. Note: C++
/// `prepend_input`s it so it appears before the inherited matrix
/// inputs.
pub const TEXTURE_INPUT: &str = "tex_in";

/// Auto-scale combo input id (C++ `k_autoscale_input`). Type: combo;
/// default `0` (`AutoScaleType::None`); combo strings: "None", "Fit",
/// "Fill", "Stretch".
pub const AUTOSCALE_INPUT: &str = "autoscale_in";

/// Interpolation combo input id (C++ `k_interpolation_input`). Type:
/// combo; default `2` (mipmapped bilinear); combo strings: "Nearest
/// Neighbor", "Bilinear", "Mipmapped Bilinear".
pub const INTERPOLATION_INPUT: &str = "interpolation_in";

/// Number of scale point gizmos (C++ `k_gizmo_scale_count` from
/// `node.h`: top-left, top-center, top-right, bottom-left,
/// bottom-center, bottom-right, center-left, center-right).
pub const GIZMO_SCALE_COUNT: usize = 8;

/// Auto-scale mode (C++ `AutoScaleType`); values match the
/// `autoscale_in` combo indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// C++ parity value set (`AutoScaleType`); only `None` is constructed at
// runtime (the combo indices are still resolved through this enum).
#[allow(dead_code)]
pub enum AutoScaleType {
	/// No auto-scaling.
	None = 0,
	/// Fit the texture inside the sequence frame (letterbox).
	Fit = 1,
	/// Fill the sequence frame (crop overflow).
	Fill = 2,
	/// Stretch the texture to the sequence frame ignoring aspect.
	Stretch = 3,
}

/// Rotation direction for wrap-around detection (C++ private enum
/// `RotationDirection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // C++ parity value set; see `AutoScaleType`.
enum RotationDirection {
	/// No direction established yet.
	None,
	/// Clockwise (C++ `k_direction_positive`).
	Positive,
	/// Counter-clockwise (C++ `k_direction_negative`).
	Negative,
}

/// Which axes a scale gizmo drags (C++ private enum `GizmoScaleType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // C++ parity value set; see `AutoScaleType`.
enum GizmoScaleType {
	/// Horizontal center handles (C++ `k_gizmo_scale_x_only`).
	XOnly,
	/// Vertical center handles (C++ `k_gizmo_scale_y_only`).
	YOnly,
	/// Corner handles (C++ `k_gizmo_scale_both`).
	Both,
}

/// Transform distort node. Applies a 2D position/rotation/scale/anchor
/// transform to a texture, equivalent to multiplying by an orthographic
/// matrix.
///
/// The C++ class also keeps a `Matrix4x4 gizmo_inverted_transform_`
/// drag-state member; `Matrix4x4` has no Rust equivalent in this crate
/// yet, so the field is omitted here (it is recomputed at drag start
/// and consumed within the drag).
pub struct TransformDistortNode {
	/// Inherited matrix-generator state (C++ base class
	/// `MatrixGenerator`; provides the transform inputs and
	/// `generate_matrix`).
	pub matrix: super::matrix::MatrixGenerator,
	/// Angle between the drag-start mouse position and the anchor
	/// (C++ `double gizmo_start_angle_`).
	gizmo_start_angle: f64,
	/// Anchor point in screen space captured at drag start (C++
	/// `PointF gizmo_anchor_pt_`).
	gizmo_anchor_pt: (f64, f64),
	/// Whether uniform scale was on at scale-drag start (C++
	/// `bool gizmo_scale_uniform_`).
	gizmo_scale_uniform: bool,
	/// Previous-frame raw rotation angle (C++ `double
	/// gizmo_last_angle_`).
	gizmo_last_angle: f64,
	/// Previous-frame perpendicular rotation angle used to disambiguate
	/// wrap-around (C++ `double gizmo_last_alt_angle_`).
	gizmo_last_alt_angle: f64,
	/// Number of full revolutions accumulated while rotation dragging
	/// (C++ `int gizmo_rotate_wrap_`).
	gizmo_rotate_wrap: i32,
	/// Last raw rotation direction (C++ `gizmo_rotate_last_dir_`).
	gizmo_rotate_last_dir: RotationDirection,
	/// Last perpendicular rotation direction (C++
	/// `gizmo_rotate_last_alt_dir_`).
	gizmo_rotate_last_alt_dir: RotationDirection,
	/// Axes dragged by the current scale gizmo (C++
	/// `gizmo_scale_axes_`).
	gizmo_scale_axes: GizmoScaleType,
	/// Scale reference point captured at drag start (texture-space
	/// anchor, flipped for right/bottom handles; C++ `Vector2D
	/// gizmo_scale_anchor_`).
	gizmo_scale_anchor: (f64, f64),
	/// Scale drag handles in `k_gizmo_scale_*` order (C++
	/// `PointGizmo *point_gizmo_[k_gizmo_scale_count]`; absolute drag
	/// value behavior, bound to both tracks of the inherited
	/// `scale_in`).
	point_gizmo: [Gizmo; GIZMO_SCALE_COUNT],
	/// Anchor point handle (C++ `PointGizmo *anchor_gizmo_`; anchor
	/// shape, drags the inherited `anchor_in` and `pos_in` tracks).
	anchor_gizmo: Gizmo,
	/// Frame outline handle (C++ `PolygonGizmo *poly_gizmo_`; drags the
	/// inherited `pos_in` tracks).
	poly_gizmo: Gizmo,
	/// Rotation ring handle (C++ `ScreenGizmo *rotation_gizmo_`;
	/// absolute drag value behavior, bound to the inherited `rot_in`).
	rotation_gizmo: Gizmo,
}

impl TransformDistortNode {
	/// Static matrix composition (C++
	/// `adjust_matrix_by_resolutions()`): scale to a 2x2 square in
	/// sequence space, apply the texture offset and the generated
	/// matrix, scale back out to texture size, then apply the
	/// auto-scale mode (stretch: per-axis; fit/fill: uniform by width
	/// or height depending on the aspect-ratio comparison).
	///
	/// Also covers the C++ private helpers `create_scale_point` and
	/// `get_direction_from_angles` (below). The remaining helpers are not
	/// representable: `generate_auto_scaled_matrix` needs the texture
	/// params from the C++ `VideoParams` (the Rust texture handle carries
	/// no params) and `is_a_scale_gizmo` compares gizmo pointers.
	#[allow(dead_code)] // C++ parity helpers; the pure ones are covered by
	// the tests, the rest are not representable in the Rust model.
	fn adjust_matrix_by_resolutions(
		mat: [f64; 16],
		sequence_res: (f64, f64),
		texture_res: (f64, f64),
		offset: (f64, f64),
		autoscale_type: AutoScaleType,
	) -> [f64; 16] {
		// First, create an identity matrix.
		let mut adjusted_matrix = super::mathbase::identity_matrix();

		// Scale it to a square based on the sequence's resolution.
		adjusted_matrix = super::matrix::matrix_scale(
			adjusted_matrix,
			2.0 / sequence_res.0,
			2.0 / sequence_res.1,
			1.0,
		);

		// Apply offset if applicable.
		adjusted_matrix = super::matrix::matrix_translate(adjusted_matrix, offset.0, offset.1);

		// Adjust by the matrix we generated earlier.
		adjusted_matrix = super::matrix::matrix_mul(adjusted_matrix, mat);

		// Scale back out to texture size (adjusted by pixel aspect).
		adjusted_matrix = super::matrix::matrix_scale(
			adjusted_matrix,
			texture_res.0 * 0.5,
			texture_res.1 * 0.5,
			1.0,
		);

		// If auto-scale is enabled, fit the texture to the sequence
		// (without cropping).
		if autoscale_type != AutoScaleType::None {
			if autoscale_type == AutoScaleType::Stretch {
				adjusted_matrix = super::matrix::matrix_scale(
					adjusted_matrix,
					sequence_res.0 / texture_res.0,
					sequence_res.1 / texture_res.1,
					1.0,
				);
			} else {
				let footage_real_ar = texture_res.0 / texture_res.1;
				let sequence_real_ar = sequence_res.0 / sequence_res.1;

				let scale_by_x = sequence_res.0 / texture_res.0;
				let scale_by_y = sequence_res.1 / texture_res.1;
				let autoscale_val;

				if (autoscale_type == AutoScaleType::Fit) == (sequence_real_ar > footage_real_ar) {
					// Scale by height. Either the sequence is wider than
					// the footage or we're using fill and cutting off the
					// sides.
					autoscale_val = scale_by_y;
				} else {
					// Scale by width. Either the footage is wider than the
					// sequence or we're using fill and cutting off the top
					// and bottom.
					autoscale_val = scale_by_x;
				}

				adjusted_matrix =
					super::matrix::matrix_scale(adjusted_matrix, autoscale_val, autoscale_val, 1.0);
			}
		}

		adjusted_matrix
	}

	/// C++ `Matrix4x4::map(PointF)` equivalent: maps `p` through the
	/// row-major matrix treating it as `(x, y, 0, 1)`, dividing by the
	/// resulting `w` whenever it is not exactly 1.
	#[allow(dead_code)]
	fn map_point(mat: [f64; 16], p: (f64, f64)) -> (f64, f64) {
		let x = p.0 * mat[0] + p.1 * mat[1] + mat[3];
		let y = p.0 * mat[4] + p.1 * mat[5] + mat[7];
		let w = p.0 * mat[12] + p.1 * mat[13] + mat[15];
		if w == 1.0 {
			(x, y)
		} else {
			(x / w, y / w)
		}
	}

	/// Scale-point gizmo placement (C++ `create_scale_point()`): maps the
	/// unit-square position through `mat` and adds the sequence half
	/// resolution.
	#[allow(dead_code)]
	fn create_scale_point(x: f64, y: f64, half_res: (f64, f64), mat: [f64; 16]) -> (f64, f64) {
		let p = Self::map_point(mat, (x, y));
		(p.0 + half_res.0, p.1 + half_res.1)
	}

	/// Rotation direction of a mouse angle step (C++
	/// `get_direction_from_angles()`): positive when `current` is greater
	/// than `last`, negative otherwise.
	#[allow(dead_code)]
	fn get_direction_from_angles(last: f64, current: f64) -> RotationDirection {
		if current > last {
			RotationDirection::Positive
		} else {
			RotationDirection::Negative
		}
	}
}

impl NodeBehavior for TransformDistortNode {
	/// Human-readable name (C++ `name()`).
	fn name(&self) -> &str {
		"Transform"
	}

	/// Short menu name (C++ `short_name()`; overrides the
	/// `MatrixGenerator` short name "Ortho" to just return `name()`).
	fn short_name(&self) -> &str {
		"Transform"
	}

	/// Stable type id (C++ `id()`).
	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.transform"
	}

	/// Categories (C++ `category()`).
	fn categories(&self) -> &[Category] {
		&[Category::Distort]
	}

	/// Description (C++ `description()`).
	fn description(&self) -> &str {
		"Transform an image in 2D space. Equivalent to multiplying by an orthographic matrix."
	}

	/// Localized input names (C++ `retranslate()`): `parent_in` ->
	/// "Parent", `autoscale_in` -> "Auto-Scale" (combo strings "None",
	/// "Fit", "Fill", "Stretch"), `tex_in` -> "Texture",
	/// `interpolation_in` -> "Interpolation" (combo strings "Nearest
	/// Neighbor", "Bilinear", "Mipmapped Bilinear"), plus the inherited
	/// `MatrixGenerator` input names via its retranslate.
	fn input_name<'a>(&self, id: &'a str) -> &'a str {
		match id {
			PARENT_INPUT => "Parent",
			AUTOSCALE_INPUT => "Auto-Scale",
			TEXTURE_INPUT => "Texture",
			INTERPOLATION_INPUT => "Interpolation",
			// The inherited MatrixGenerator input names (the combo strings
			// above are UI-level properties, C++ `set_combo_box_strings`).
			_ => self.matrix.input_name(id),
		}
	}

	/// Combo input option labels (C++ `retranslate()` /
	/// `set_combo_box_strings`): `autoscale_in` -> "None", "Fit", "Fill",
	/// "Stretch"; `interpolation_in` -> "Nearest Neighbor", "Bilinear",
	/// "Mipmapped Bilinear".
	fn input_combo_strings(&self, id: &str) -> Vec<&'static str> {
		match id {
			AUTOSCALE_INPUT => vec!["None", "Fit", "Fill", "Stretch"],
			INTERPOLATION_INPUT => vec!["Nearest Neighbor", "Bilinear", "Mipmapped Bilinear"],
			_ => Vec::new(),
		}
	}

	/// Evaluate outputs (C++ `value()`): generates the matrix from the
	/// inherited transform inputs (position/rotation/scale/anchor,
	/// folded with `parent_in`) and always pushes it as a `k_matrix`
	/// value. With a texture: builds the auto-scaled real matrix via
	/// `adjust_matrix_by_resolutions`; if it is not identity, renders a
	/// shader job at the GLOBAL video params (the transform may change
	/// the size) binding `ove_maintex` and `ove_mvpmat` with the
	/// interpolation selected by `interpolation_in`, and pushes that;
	/// identity matrix (or no texture) -> pass-through push of the
	/// input texture value.
	///
	/// The job boxes a [`ShaderJobPayload`] that the renderer's resolve
	/// hook executes and replaces with the result texture; the params row
	/// carries the input texture and uniforms, keyed by the effect input.
	/// The real matrix needs the texture's params and the sequence
	/// resolution (the Rust texture handle carries no params and the
	/// value() signature no globals), so the identity check — and thus
	/// the pass-through-vs-job decision — is not representable here: with
	/// a texture the job is always queued (`// CPP-PARITY:
	/// transformdistortnode.cpp` value()).
	fn value(
		&self,
		core: &NodeCore,
		inputs: &crate::value::NodeValueRow,
		time: oak_core::Rational,
		table: &mut crate::value::NodeValueTable,
	) {
		let parent = match inputs.get(PARENT_INPUT) {
			Some(crate::value::NodeValue::Matrix(m)) => *m,
			_ => match core.value_at_time(PARENT_INPUT, -1, time) {
				crate::value::NodeValue::Matrix(m) => m,
				_ => super::mathbase::identity_matrix(),
			},
		};

		// Generate matrix.
		let generated_matrix = super::matrix::MatrixGenerator::generate_matrix(
			inputs, core, time, false, false, false, parent,
		);
		table.push(
			crate::value::ValueType::Matrix,
			crate::value::NodeValue::Matrix(generated_matrix),
			None,
		);

		match inputs.get(TEXTURE_INPUT) {
			Some(tex @ crate::value::NodeValue::Texture(_)) => {
				// The identity transform passes the texture through (C++
				// skips the job when the real matrix is identity).
				let identity = super::mathbase::identity_matrix();
				let is_identity = generated_matrix
					.iter()
					.zip(identity)
					.all(|(a, b)| (*a - b).abs() <= 1e-9);
				if is_identity {
					table.push(crate::value::ValueType::Texture, tex.clone(), None);
					return;
				}

				// The job carries the inverse transform in pixel space
				// (C++ inserts `ove_mvpmat` — the forward matrix for the
				// vertex stage; the fragment-side implementation samples
				// through the inverse, see TRANSFORM_FRAG). A singular
				// matrix (zero scale) passes the input through, matching
				// the C++ inverted()-fails fallback. The auto-scaled
				// variant stays unrepresentable: it needs the texture's
				// params and the sequence resolution at job-build time
				// (`// CPP-PARITY: transformdistortnode.cpp` value()).
				let Some(inverse) = super::matrix::matrix_invert_2d(generated_matrix) else {
					table.push(crate::value::ValueType::Texture, tex.clone(), None);
					return;
				};
				let mut params = inputs.clone();
				params.insert(
					"transform_in".to_string(),
					crate::value::NodeValue::Matrix(inverse),
				);
				table.push(
					crate::value::ValueType::Texture,
					crate::value::NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
						node_id: crate::id::NodeId::INVALID,
						time,
						iterations: 1,
						type_id: self.type_id().to_string(),
						shader_id: String::new(),
						effect_input: core.effect_input.clone(),
						params,
						iterative_input: TEXTURE_INPUT.to_string(),
					}))),
					None,
				);
			}
			_ => {
				// No texture: C++ re-pushes the input value (k_none),
				// which is a no-op here.
			}
		}
	}

	/// Shader code request (C++ `get_shader_code()`): ignores the
	/// request id and returns the transform fragment shader — the
	/// inverse-sampling equivalent of the C++ default vertex-shader
	/// transform (see [`TRANSFORM_FRAG`]).
	fn shader_code(&self, request: &str) -> Option<String> {
		let _ = request;
		Some(TRANSFORM_FRAG.to_string())
	}

	/// Gizmo transform/positions (C++ `update_gizmo_positions()` and
	/// `gizmo_transformation()`). Positions: with a texture, builds the
	/// rectangle matrix (sequence half-res scale folded with the
	/// auto-scaled generated matrix), maps the unit square through it
	/// for the polygon gizmo, places the anchor gizmo at the mapped
	/// origin, the eight scale handles at the mapped unit-square
	/// corners/edge midpoints, and sets the `offset` input property of
	/// `pos_in` (half sequence res + texture offset) and `anchor_in`
	/// (half texture size). Transformation: with a texture returns the
	/// auto-scaled generated matrix (generated with an identity parent);
	/// without one falls back to the base `MatrixGenerator`
	/// transformation.
	///
	/// Every placement needs the texture's params and the sequence
	/// resolution (neither available here), and the gizmo point
	/// positions have no storage in [`Gizmo`] — not representable
	/// (`// CPP-PARITY: transformdistortnode.cpp`
	/// `update_gizmo_positions` / `gizmo_transformation`).
	fn gizmo_update(&self, core: &NodeCore, row: &crate::value::NodeValueRow) {
		let _ = (core, row);
	}

	/// Gizmo drag callbacks (C++ `gizmo_drag_start()` /
	/// `gizmo_drag_move()`). Start: anchor gizmo -> stores the inverted
	/// anchor-aware generated matrix; scale handle -> requires a
	/// texture, records uniform-scale state, screen-space anchor point,
	/// per-handle axes (corners both, left/right center X, top/bottom
	/// center Y), the texture-space scale anchor flipped for
	/// right/bottom handles, and the inverted fully-generated matrix;
	/// rotation gizmo -> records the anchor point, start/last angles
	/// (raw and perpendicular) and resets the wrap counter and
	/// direction. Move: polygon gizmo -> drags the position X/Y tracks
	/// by the mouse delta; anchor gizmo -> drags the anchor tracks by
	/// the inverse-matrix-mapped delta and the position tracks by the
	/// raw delta; rotation gizmo -> accumulates wrap-aware angle
	/// (detecting direction reversal across ±pi using the perpendicular
	/// angle) and drags the rotation track by the degree difference
	/// from the start angle; scale handle -> maps the mouse delta into
	/// inverted-matrix space relative to the anchor, then drags the
	/// scale tracks by the normalized magnitude per the handle's axes,
	/// collapsing to a single uniform value (diagonal ratio for corner
	/// handles) when uniform scale is on.
	///
	/// The drags write keyframe tracks through `NodeInputDragger`s with
	/// per-drag start values, which the Rust data model does not carry —
	/// not representable here (`// CPP-PARITY: transformdistortnode.cpp`
	/// `gizmo_drag_start` / `gizmo_drag_move`). The drag-state fields
	/// above mirror the C++ members but are never written.
	fn gizmo_drag(&mut self, core: &mut NodeCore, start: bool, x: f64, y: f64, modifiers: u32) {
		let _ = (core, start, x, y, modifiers);
	}

	/// Deep copy (C++ `copy()`).
	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(TransformDistortNode {
			matrix: super::matrix::MatrixGenerator,
			gizmo_start_angle: self.gizmo_start_angle,
			gizmo_anchor_pt: self.gizmo_anchor_pt,
			gizmo_scale_uniform: self.gizmo_scale_uniform,
			gizmo_last_angle: self.gizmo_last_angle,
			gizmo_last_alt_angle: self.gizmo_last_alt_angle,
			gizmo_rotate_wrap: self.gizmo_rotate_wrap,
			gizmo_rotate_last_dir: self.gizmo_rotate_last_dir,
			gizmo_rotate_last_alt_dir: self.gizmo_rotate_last_alt_dir,
			gizmo_scale_axes: self.gizmo_scale_axes,
			gizmo_scale_anchor: self.gizmo_scale_anchor,
			point_gizmo: self.point_gizmo.clone(),
			anchor_gizmo: self.anchor_gizmo.clone(),
			poly_gizmo: self.poly_gizmo.clone(),
			rotation_gizmo: self.rotation_gizmo.clone(),
		}))
	}
}

/// Constructor (C++ `TransformDistortNode::TransformDistortNode()`):
/// adds `parent_in` (matrix), `autoscale_in` (combo, default 0) and
/// `interpolation_in` (combo, default 2), and PREPENDS `tex_in`
/// (texture, not-keyframable) ahead of the inherited `MatrixGenerator`
/// inputs; creates the rotation screen gizmo (absolute drag behavior,
/// bound to `rot_in`), the frame polygon gizmo (bound to `pos_in`), the
/// anchor point gizmo (anchor shape, bound to `anchor_in` and `pos_in`)
/// and the eight scale point gizmos (absolute drag behavior, bound to
/// `scale_in`); sets the video-effect flag and the effect input.
pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
	// Inherit the MatrixGenerator inputs (pos/rot/scale/uniform/anchor).
	let (mut core, _) = super::matrix::create();

	let mut tex = crate::input::Input::new(
		TEXTURE_INPUT,
		crate::value::ValueType::Texture,
		crate::value::NodeValue::None,
	);
	tex.flags |= crate::input::flags::NOT_KEYFRAMABLE;
	let parent_input = crate::input::Input::new(
		PARENT_INPUT,
		crate::value::ValueType::Matrix,
		crate::value::NodeValue::None,
	);
	let autoscale_input = crate::input::Input::new(
		AUTOSCALE_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(AutoScaleType::None as i64),
	);
	let interpolation_input = crate::input::Input::new(
		INTERPOLATION_INPUT,
		crate::value::ValueType::Combo,
		crate::value::NodeValue::Combo(2),
	);

	// The C++ input order is `enabled_in`, `tex_in` (prepended), the
	// node's own inputs, then the inherited matrix inputs. `tex_in` goes
	// right after `enabled_in`; `parent_in`/`autoscale_in`/
	// `interpolation_in` go between it and the matrix inputs (inserted
	// in reverse at index 2 to keep that order).
	core.inputs.insert(1, tex);
	core.inputs.insert(2, interpolation_input);
	core.inputs.insert(2, autoscale_input);
	core.inputs.insert(2, parent_input);

	// Gizmos, in C++ add order: the rotation screen gizmo (absolute drag
	// behavior, bound to `rot_in`), the frame polygon gizmo (bound to
	// `pos_in`), the anchor point gizmo (bound to `anchor_in` and
	// `pos_in`), and the eight scale point gizmos (absolute drag
	// behavior, bound to `scale_in`) in `k_gizmo_scale_*` order.
	let rotation_gizmo = Gizmo {
		position_inputs: vec![(super::matrix::ROTATION_INPUT.to_string(), -1, 0)],
		drag_point: (0.0, 0.0),
	};
	let poly_gizmo = Gizmo {
		position_inputs: vec![
			(super::matrix::POSITION_INPUT.to_string(), -1, 0),
			(super::matrix::POSITION_INPUT.to_string(), -1, 1),
		],
		drag_point: (0.0, 0.0),
	};
	let anchor_gizmo = Gizmo {
		position_inputs: vec![
			(super::matrix::ANCHOR_INPUT.to_string(), -1, 0),
			(super::matrix::ANCHOR_INPUT.to_string(), -1, 1),
			(super::matrix::POSITION_INPUT.to_string(), -1, 0),
			(super::matrix::POSITION_INPUT.to_string(), -1, 1),
		],
		drag_point: (0.0, 0.0),
	};
	let scale_gizmo = |_i: usize| Gizmo {
		position_inputs: vec![
			(super::matrix::SCALE_INPUT.to_string(), -1, 0),
			(super::matrix::SCALE_INPUT.to_string(), -1, 1),
		],
		drag_point: (0.0, 0.0),
	};
	let point_gizmo: [Gizmo; GIZMO_SCALE_COUNT] = [
		scale_gizmo(0),
		scale_gizmo(1),
		scale_gizmo(2),
		scale_gizmo(3),
		scale_gizmo(4),
		scale_gizmo(5),
		scale_gizmo(6),
		scale_gizmo(7),
	];

	core.gizmos = vec![
		rotation_gizmo.clone(),
		poly_gizmo.clone(),
		anchor_gizmo.clone(),
		point_gizmo[0].clone(),
		point_gizmo[1].clone(),
		point_gizmo[2].clone(),
		point_gizmo[3].clone(),
		point_gizmo[4].clone(),
		point_gizmo[5].clone(),
		point_gizmo[6].clone(),
		point_gizmo[7].clone(),
	];

	core.flags |= crate::node::flags::VIDEO_EFFECT;
	core.effect_input = TEXTURE_INPUT.to_string();

	(
		core,
		Box::new(TransformDistortNode {
			matrix: super::matrix::MatrixGenerator,
			gizmo_start_angle: 0.0,
			gizmo_anchor_pt: (0.0, 0.0),
			gizmo_scale_uniform: false,
			gizmo_last_angle: 0.0,
			gizmo_last_alt_angle: 0.0,
			gizmo_rotate_wrap: 0,
			gizmo_rotate_last_dir: RotationDirection::None,
			gizmo_rotate_last_alt_dir: RotationDirection::None,
			gizmo_scale_axes: GizmoScaleType::Both,
			gizmo_scale_anchor: (0.0, 0.0),
			point_gizmo,
			anchor_gizmo,
			poly_gizmo,
			rotation_gizmo,
		}),
	)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeBehavior;
    use crate::value::{NodeValue, NodeValueTable, ValueType};
    use oak_core::Rational;

    fn tex() -> NodeValue {
		NodeValue::Texture(crate::handle::CHandle::null())
	}

	fn empty_gizmo() -> Gizmo {
		Gizmo {
			position_inputs: vec![],
			drag_point: (0.0, 0.0),
		}
	}

	fn empty_node() -> TransformDistortNode {
		TransformDistortNode {
			matrix: super::super::matrix::MatrixGenerator,
			gizmo_start_angle: 0.0,
			gizmo_anchor_pt: (0.0, 0.0),
			gizmo_scale_uniform: false,
			gizmo_last_angle: 0.0,
			gizmo_last_alt_angle: 0.0,
			gizmo_rotate_wrap: 0,
			gizmo_rotate_last_dir: RotationDirection::None,
			gizmo_rotate_last_alt_dir: RotationDirection::None,
			gizmo_scale_axes: GizmoScaleType::Both,
			gizmo_scale_anchor: (0.0, 0.0),
			point_gizmo: std::array::from_fn(|_| empty_gizmo()),
			anchor_gizmo: empty_gizmo(),
			poly_gizmo: empty_gizmo(),
			rotation_gizmo: empty_gizmo(),
		}
	}

	#[test]
	fn input_names() {
		let n = empty_node();
		assert_eq!(n.input_name(PARENT_INPUT), "Parent");
		assert_eq!(n.input_name(AUTOSCALE_INPUT), "Auto-Scale");
		assert_eq!(n.input_name(TEXTURE_INPUT), "Texture");
		assert_eq!(n.input_name(INTERPOLATION_INPUT), "Interpolation");
		// Inherited matrix-generator names.
		assert_eq!(
			n.input_name(super::super::matrix::POSITION_INPUT),
			"Position"
		);
		assert_eq!(
			n.input_name(super::super::matrix::ROTATION_INPUT),
			"Rotation"
		);
		assert_eq!(n.input_name(super::super::matrix::SCALE_INPUT), "Scale");
		assert_eq!(
			n.input_name(super::super::matrix::UNIFORM_SCALE_INPUT),
			"Uniform Scale"
		);
		assert_eq!(
			n.input_name(super::super::matrix::ANCHOR_INPUT),
			"Anchor Point"
		);
	}

	#[test]
	fn create_wires_inputs_and_flags() {
		let (core, behavior) = create();
		assert_eq!(behavior.type_id(), "org.olivevideoeditor.Olive.transform");
		// C++ input order: enabled_in, tex_in (prepended), parent_in,
		// autoscale_in, interpolation_in, then the inherited matrix inputs.
		let ids: Vec<&str> = core.inputs.iter().map(|i| i.id.as_str()).collect();
		assert_eq!(
			ids,
			vec![
				crate::node::ENABLED_INPUT,
				TEXTURE_INPUT,
				PARENT_INPUT,
				AUTOSCALE_INPUT,
				INTERPOLATION_INPUT,
				super::super::matrix::POSITION_INPUT,
				super::super::matrix::ROTATION_INPUT,
				super::super::matrix::SCALE_INPUT,
				super::super::matrix::UNIFORM_SCALE_INPUT,
				super::super::matrix::ANCHOR_INPUT,
			]
		);
		assert_eq!(
			core.get_input(AUTOSCALE_INPUT).unwrap().default,
			NodeValue::Combo(0)
		);
		assert_eq!(
			core.get_input(INTERPOLATION_INPUT).unwrap().default,
			NodeValue::Combo(2)
		);
		// Eleven gizmos: rotation + polygon + anchor + eight scale points.
		assert_eq!(core.gizmos.len(), 11);
		assert_eq!(core.effect_input, TEXTURE_INPUT);
		assert_ne!(core.flags & crate::node::flags::VIDEO_EFFECT, 0);
	}

	#[test]
	fn value_identity_transform_passes_texture_through() {
		// Default inputs (position 0, rotation 0, scale 1) generate the
		// identity matrix: the texture passes through without a job (C++
		// skips the job when the real matrix is identity).
		let (core, behavior) = create();
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		assert!(table.get(ValueType::Matrix).is_some());
		assert_eq!(table.get(ValueType::Texture), Some(&tex()));
	}

	#[test]
	fn value_pushes_matrix_and_job_with_texture() {
		let (mut core, behavior) = create();
		// A real transform (position +50 in x) generates a non-identity
		// matrix, so the shader job is queued.
		core.set_standard_value(
			super::super::matrix::POSITION_INPUT,
			-1,
			NodeValue::Vec2([50.0, 0.0]),
		);
		let inputs = crate::value::NodeValueRow::from([(TEXTURE_INPUT.to_string(), tex())]);
		let mut table = NodeValueTable::default();
		behavior.value(&core, &inputs, Rational::new(0, 1), &mut table);
		// Matrix output always pushed; texture job queued for the seam.
		assert!(table.get(ValueType::Matrix).is_some());
		let handle = match table.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe { crate::jobs::shader_job(&handle) }
			.expect("shader job payload expected");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.transform");
		assert_eq!(payload.shader_id, "");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, TEXTURE_INPUT);
		assert_eq!(payload.time, Rational::new(0, 1));
		assert!(payload.params.contains_key(TEXTURE_INPUT));
		// The job carries the inverse of the +50px translation
		// (fragment-side inverse sampling).
		match payload.params.get("transform_in") {
			Some(NodeValue::Matrix(m)) => assert_eq!(m[3], -50.0),
			other => panic!("transform_in must carry the inverse matrix: {other:?}"),
		}
	}

	#[test]
	fn value_without_texture_pushes_matrix_only() {
		let (core, behavior) = create();
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		assert!(table.get(ValueType::Matrix).is_some());
		assert!(table.get(ValueType::Texture).is_none());
	}

	#[test]
	fn value_folds_parent_matrix() {
		let (mut core, behavior) = create();
		let mut parent = super::super::mathbase::identity_matrix();
		parent[3] = 50.0;
		core.set_standard_value(PARENT_INPUT, -1, NodeValue::Matrix(parent));
		let mut table = NodeValueTable::default();
		behavior.value(
			&core,
			&crate::value::NodeValueRow::default(),
			Rational::new(0, 1),
			&mut table,
		);
		let m = match table.get(ValueType::Matrix).unwrap() {
			NodeValue::Matrix(m) => *m,
			_ => panic!("matrix expected"),
		};
		assert_eq!(m[3], 50.0, "parent translation folds through");
	}

	#[test]
	fn shader_code_returns_inverse_sampling_frag() {
		let n = empty_node();
		let frag = n.shader_code("anything").expect("real transform shader");
		assert!(frag.contains("transform_in"));
		assert!(frag.contains("resolution_in"));
	}

	#[test]
	fn adjust_matrix_identity_when_matching_resolutions() {
		// sequence == texture at a power-of-two resolution: the
		// scale(2/res) * scale(res/2) composition is exactly identity.
		let m = TransformDistortNode::adjust_matrix_by_resolutions(
			super::super::mathbase::identity_matrix(),
			(640.0, 360.0),
			(640.0, 360.0),
			(0.0, 0.0),
			AutoScaleType::None,
		);
		assert!(super::super::mathbase::matrix_is_identity(m));
	}

	#[test]
	fn adjust_matrix_translation_and_scale() {
		// sequence (640, 360), texture (320, 180), offset (10, 20):
		// scale(2/640) * translate(10, 20) * scale(160, 90) = diag(0.5)
		// with m[0][3] = 10*2/640 and m[1][3] = 20*2/360.
		let m = TransformDistortNode::adjust_matrix_by_resolutions(
			super::super::mathbase::identity_matrix(),
			(640.0, 360.0),
			(320.0, 180.0),
			(10.0, 20.0),
			AutoScaleType::None,
		);
		assert!((m[0] - 0.5).abs() < 1e-12);
		assert!((m[5] - 0.5).abs() < 1e-12);
		assert!((m[3] - 10.0 * 2.0 / 640.0).abs() < 1e-12);
		assert!((m[7] - 20.0 * 2.0 / 360.0).abs() < 1e-12);
	}

	#[test]
	fn adjust_matrix_stretch_autoscale() {
		// Stretch scales per-axis to the sequence size on top of the
		// base composition: with sequence == texture the base is already
		// identity, and stretch adds scale(seq/tex) = 1 — identity.
		let m = TransformDistortNode::adjust_matrix_by_resolutions(
			super::super::mathbase::identity_matrix(),
			(640.0, 360.0),
			(640.0, 360.0),
			(0.0, 0.0),
			AutoScaleType::Stretch,
		);
		assert!(super::super::mathbase::matrix_is_identity(m));

		// Half-size texture with a position offset: the base composition
		// halves the texture (0.5) and stretch's (2, 2) brings it back to
		// unit scale. The offset lives in the pre-scale matrix, so the
		// sequence-space scale (2/640) applies to it: 100 * 2/640 = 0.3125
		// (post-multiplied scales preserve the translation column).
		let mat = super::super::matrix::matrix_translate(
			super::super::mathbase::identity_matrix(),
			100.0,
			0.0,
		);
		let m = TransformDistortNode::adjust_matrix_by_resolutions(
			mat,
			(640.0, 360.0),
			(320.0, 180.0),
			(0.0, 0.0),
			AutoScaleType::Stretch,
		);
		assert!((m[0] - 1.0).abs() < 1e-12);
		assert!((m[5] - 1.0).abs() < 1e-12);
		assert!(
			(m[3] - 100.0 * 2.0 / 640.0).abs() < 1e-12,
			"offset scaled into sequence units"
		);
	}

	#[test]
	fn create_scale_point_maps_and_offsets() {
		let m = super::super::mathbase::identity_matrix();
		let pt = TransformDistortNode::create_scale_point(1.0, -1.0, (320.0, 180.0), m);
		assert_eq!(pt, (321.0, 179.0));
	}

	#[test]
	fn get_direction_from_angles() {
		assert_eq!(
			TransformDistortNode::get_direction_from_angles(0.0, 1.0),
			RotationDirection::Positive
		);
		assert_eq!(
			TransformDistortNode::get_direction_from_angles(1.0, 0.0),
			RotationDirection::Negative
		);
	}

	#[test]
	fn duplicate_clones() {
		let (core, behavior) = create();
		let dup = behavior.duplicate(&core).unwrap();
		assert_eq!(dup.name(), "Transform");
		assert_eq!(dup.short_name(), "Transform");
	}
}

/// Register this node type (C++ factory entry for
/// `org.olivevideoeditor.Olive.transform`).
pub fn register(meta: &mut Vec<NodeMeta>) {
	meta.push(NodeMeta {
		type_id: "org.olivevideoeditor.Olive.transform",
		name: "Transform",
		categories: &[Category::Distort],
		create,
	});
}
