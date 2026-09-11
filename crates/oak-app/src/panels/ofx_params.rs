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

//! The inspector's OFX parameter view (stage 6b): auto-generated controls
//! for the effect node's inputs.
//!
//! The [`EffectStackView`](gpui::effect_stack::EffectStackView) invokes a
//! params renderer inside each expanded effect card. For OFX plugin nodes
//! this view reads the effect's parameter snapshot from the engine
//! ([`AppEngine::effect_params`]) and renders one control per input:
//!
//! - int / float → [`Slider`] (double-click for direct entry)
//! - boolean → [`CheckBox`]
//! - combo → [`ComboBox`] fed from the repeated `("combo_option", _)`
//!   properties; string-combo values come from `("combo_value", _)`
//! - text → [`EditableTextState`] (`text_input`; the `("multiline", true)`
//!   property that [`super::effectchain::effect_params`] adds to the v3
//!   text node's text inputs builds a `text_area` instead)
//! - vec2 / vec3 → one [`SpinBox`] per component
//! - color → a swatch + deferred popup picker ([`OfxColorPicker`]:
//!   R/G/B/A sliders, live preview, hex input, Cancel/OK)
//! - push button → a clickable button (`AppEngine::effect_push_button`)
//!
//! Secret (HIDDEN) inputs never reach the snapshot the facade builds, and
//! the view skips them again on its own (a plugin or mock engine can hand
//! one over; the inspector must not show a secret input even then).
//! `ui_group` / `ui_page` become section titles. Every edit is routed
//! through [`AppEngine::set_effect_param`] (undoable).
//!
//! The control set is built once per expanded card — the stack view caches
//! the params view per effect (recreating it per render would kill
//! in-progress slider drags); the view observes the engine and re-syncs
//! the widget values from the engine snapshot on every render.

use std::sync::Arc;
use crate::oakui::component::text_input;

use gpui::effect_stack::EffectId;
use gpui::colors::DefaultColors;
use gpui::{
	div, prelude::*, px, rgb, size, point, ClickEvent, Context, Entity, EventEmitter, Focusable,
	Render, SharedString, Window,
};
use gpui::{
	Anchor, App, Bounds, ElementId, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseUpEvent,
	Point, Pixels, Rgba, anchored, canvas, deferred, fill,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage};
use crate::oakui::component::controls::{CheckBox, CheckBoxEvent, CheckState};
use crate::oakui::component::controls::{ComboBox, ComboBoxEvent, ComboBoxOption};
use crate::oakui::component::controls::{Slider, SliderEvent, SliderModel, SliderValue, ValueKind};
use crate::oakui::component::controls::{SpinBox, SpinBoxEvent};

use oak_node::value::{NodeValue, ValueType};

use crate::oakui::{AppEngine, EffectParam};

/// One editable parameter row of the view.
struct ParamControl {
	/// The input id (the OFX param name).
	input_id: String,
	/// The display name (the OFX param label).
	display_name: String,
	/// The OFX ui_group / ui_page section header, if any.
	section: Option<(String, String)>,
	/// The control(s) for this parameter.
	kind: ControlKind,
	/// Parametric-curve normalization: (key lo, key hi, value min, value
	/// max) used to map between the engine's real coordinates and the
	/// editor's normalized 0..1 space.
	curve_domain: Option<(f64, f64, f64, f64)>,
}

/// The concrete control(s) for one parameter.
enum ControlKind {
	/// A slider (int / float).
	Slider(Entity<Slider>),
	/// A checkbox (boolean).
	CheckBox(Entity<CheckBox>),
	/// A combo box (combo / string combo).
	Combo(Entity<ComboBox>),
	/// One spinbox per component (vec2 / vec3); the usize is the
	/// component index within the value.
	Spin(Vec<(Entity<SpinBox>, usize)>),
	/// A colour swatch + popup picker (color).
	Color(Entity<OfxColorPicker>),
	/// A text field: single-line by default, multi-line when the param
	/// carries the `("multiline", true)` property (the v3 text node's text
	/// inputs; the inspector then builds a `text_area`).
	Text {
		editor: Entity<EditableTextState>,
		multiline: bool,
	},
	/// One curve editor per dimension (parametric parameter).
	Curve(Vec<Entity<gpui_widgets::curve_editor::CurveEditor>>),
	/// A push button (rendered inline, no entity).
	PushButton,
	/// A read-only value line (no editable control; e.g. custom/binary).
	ReadOnly(SharedString),
}

/// The inspector's parameter view for one expanded effect card.
pub struct OfxParamsView<E: AppEngine> {
	engine: Entity<E>,
	effect: EffectId,
	controls: Vec<ParamControl>,
}

impl<E: AppEngine> OfxParamsView<E> {
	/// Builds the control set from the engine's current parameter snapshot.
	pub fn new(
		effect: EffectId,
		engine: Entity<E>,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let params = engine.read(cx).effect_params(effect).unwrap_or_default();
		let mut control_id = 0usize;
		let controls = params
			.iter()
			// The built-in facade already drops hidden inputs, but a plugin
			// or mock engine's snapshot can carry them; a secret input must
			// never reach the inspector, so filter here as well (the params
			// view is the last stop before a control is built).
			.filter(|param| param.flags & oak_node::input::flags::HIDDEN == 0)
			.map(|param| build_control(param, &mut control_id, window, cx))
			.collect();

		let this = Self {
			engine,
			effect,
			controls,
		};
		wire_controls(&this, cx);
		// The stack view caches one params view per effect, so the view
		// lives across edits: re-render (and thereby `sync_values`, which
		// silently reapplies the engine snapshot) whenever the engine
		// changes — undo/redo, external edits, plugin-side updates.
		cx.observe(&this.engine, |_this, _engine, cx| cx.notify())
			.detach();
		this
	}

	/// Applies the engine's current values to every control (called each
	/// render so external edits / undo / redo land on the controls).
	fn sync_values(&mut self, window: &Window, cx: &mut Context<Self>) {
		let params = self.engine.read(cx).effect_params(self.effect).unwrap_or_default();
		for control in &self.controls {
			let Some(param) = params.iter().find(|p| p.input_id == control.input_id) else {
				continue;
			};
			match &control.kind {
				ControlKind::Slider(slider) => {
					let sv = slider_value(param);
					let slider = slider.clone();
					slider.update(cx, |slider, _| slider.set_value(sv));
				}
				ControlKind::CheckBox(check) => {
					let state = match param.value {
						NodeValue::Boolean(true) => CheckState::Checked,
						_ => CheckState::Unchecked,
					};
					let check = check.clone();
					check.update(cx, |check, cx| check.set_state(state, cx));
				}
				ControlKind::Combo(combo) => {
					let index = combo_index_for(param);
					let combo = combo.clone();
					combo.update(cx, |combo, cx| {
						combo.set_selected(Some(index), cx);
					});
				}
				ControlKind::Spin(spins) => {
					let components = value_components(&param.value);
					for (spin, channel) in spins {
						if let Some(value) = components.get(*channel) {
							let spin = spin.clone();
							let sv = SliderValue::Float(*value);
							spin.update(cx, |spin, cx| spin.set_value(sv, cx));
						}
					}
				}
				ControlKind::Color(picker) => {
					if let NodeValue::Color(v) = param.value {
						let color = Rgba {
							r: v[0] as f32,
							g: v[1] as f32,
							b: v[2] as f32,
							a: v[3] as f32,
						};
						let picker = picker.clone();
						picker.update(cx, |picker, cx| picker.set_committed(color, cx));
					}
					// Drain a viewer eyedropper pick into the draft. The result
					// is taken (not peeked) so an armed-but-unpicked picker
					// keeps the previous value once the picker disarms.
					if let Some(color) = self
						.engine
						.update(cx, |engine, cx| engine.take_eyedropper_result(cx))
					{
						let picker = picker.clone();
						picker.update(cx, |picker, cx| picker.apply_viewer_pick(color, cx));
					}
				}
				ControlKind::Text { editor, .. } => {
					let text = match &param.value {
						NodeValue::Text(s) => s.clone(),
						NodeValue::StrCombo(s) => s.clone(),
						_ => continue,
					};
					// Never re-sync a field the user is editing: the
					// params view re-renders on every engine tick, and
					// reapplying the engine snapshot mid-edit wipes the
					// in-progress text (the "cannot type into the text
					// field" report). The field re-syncs on blur, and
					// the row's explicit commit writes the edit back.
					if editor.read(cx).focus_handle(cx).is_focused(window) {
						continue;
					}
					let editor = editor.clone();
					editor.update(cx, |editor, cx| {
						if editor.as_str() != text {
							editor.emplace(&text, cx);
						}
					});
				}
				ControlKind::Curve(editors) => {
					// Re-seed from the engine's JSON mirror, but never
					// mid-drag (that would steal the gesture) and never on
					// an identical curve (sync_values runs per render).
					let curves = match &param.value {
						NodeValue::Text(json) => {
							oak_plugin::param_curve::curves_from_json(json).unwrap_or_default()
						}
						_ => Vec::new(),
					};
					let domain = control.curve_domain.unwrap_or((0.0, 1.0, 0.0, 1.0));
					for (editor, curve) in editors.iter().zip(curves.iter()) {
						let fresh: Vec<_> = curve
							.points
							.iter()
							.map(|p| curve_point_to_editor(p, &curve.points, domain))
							.collect();
						let (current, dragging) = {
							let e = editor.read(cx);
							(e.points().to_vec(), e.is_dragging())
						};
						if dragging || curve_points_close(&current, &fresh) {
							continue;
						}
						editor.update(cx, |editor, cx| editor.set_points(fresh, cx));
					}
				}
				ControlKind::PushButton | ControlKind::ReadOnly(_) => {}
			}
		}
	}
}

/// The (key lo, key hi, value min, value max) normalization domain of a
/// parametric parameter: the key range from the `parametric_range`
/// property (default 0..1); the value domain is 0..1 when everything fits
/// (LUT-style params), else the data extent with a 10% pad.
fn curve_domain(
	param: &EffectParam,
	curves: &[oak_plugin::param_curve::Curve],
) -> (f64, f64, f64, f64) {
	let (mut lo, mut hi) = (0.0, 1.0);
	if let Some((_, oak_node::value::NodeValue::Vec2(v))) = param
		.properties
		.iter()
		.find(|(k, _)| k == "parametric_range")
	{
		lo = v[0];
		hi = v[1];
	}
	if hi <= lo {
		hi = lo + 1.0;
	}
	let (mut vmin, mut vmax) = (0.0f64, 1.0f64);
	let all_unit = curves
		.iter()
		.flat_map(|c| c.points.iter())
		.all(|p| (0.0..=1.0).contains(&p.value));
	if !all_unit {
		vmin = curves
			.iter()
			.flat_map(|c| c.points.iter())
			.map(|p| p.value)
			.fold(f64::INFINITY, f64::min);
		vmax = curves
			.iter()
			.flat_map(|c| c.points.iter())
			.map(|p| p.value)
			.fold(f64::NEG_INFINITY, f64::max);
		if vmax - vmin < 1e-6 {
			vmax = vmin + 1.0;
		}
		let pad = (vmax - vmin) * 0.1;
		vmin -= pad;
		vmax += pad;
	}
	(lo, hi, vmin, vmax)
}

/// Real curve point → normalized editor point (slopes become bezier
/// handle offsets; a point without an explicit slope edits gets linear
/// handles).
fn curve_point_to_editor(
	p: &oak_plugin::param_curve::ControlPoint,
	points: &[oak_plugin::param_curve::ControlPoint],
	domain: (f64, f64, f64, f64),
) -> gpui_widgets::curve_editor::CurvePoint {
	use gpui_widgets::curve_editor::{CurvePoint, CurveVec2};
	let (lo, hi, vmin, vmax) = domain;
	let (sx, sy) = (hi - lo, vmax - vmin);
	let index = points.iter().position(|q| q.key == p.key).unwrap_or(0);
	let x = (p.key - lo) / sx;
	let y = (p.value - vmin) / sy;
	// Hermite slope (real) -> normalized slope: m_norm = m_real * sx / sy.
	let m_norm = p.slope * sx / sy;
	let handle_out = points.get(index + 1).map(|next| {
		let h = (next.key - p.key) / sx;
		CurveVec2::new(h / 3.0, m_norm * h / 3.0)
	});
	let handle_in = index.checked_sub(1).and_then(|pi| points.get(pi)).map(|prev| {
		let h = (p.key - prev.key) / sx;
		CurveVec2::new(-h / 3.0, -m_norm * h / 3.0)
	});
	CurvePoint {
		x,
		y,
		handle_in,
		handle_out,
	}
}

/// Normalized editor points → real curve points (slopes recovered from
/// the bezier handles; points without handles get the centered-difference
/// auto slope).
fn curve_from_editor(
	points: &[gpui_widgets::curve_editor::CurvePoint],
	domain: (f64, f64, f64, f64),
) -> oak_plugin::param_curve::Curve {
	let (lo, hi, vmin, vmax) = domain;
	let (sx, sy) = (hi - lo, vmax - vmin);
	let n = points.len();
	let mut out = oak_plugin::param_curve::Curve::empty();
	for (i, p) in points.iter().enumerate() {
		let key = lo + p.x * sx;
		let value = vmin + p.y * sy;
		// Slope from the out handle (preferred) or the in handle.
		let m_norm = if let (Some(h), Some(next)) = (p.handle_out, points.get(i + 1)) {
			let h_seg = next.x - p.x;
			if h_seg.abs() > 1e-9 {
				Some(3.0 * h.y / h_seg)
			} else {
				None
			}
		} else if let (Some(h), Some(prev)) = (p.handle_in, i.checked_sub(1).map(|pi| &points[pi]))
		{
			let h_seg = p.x - prev.x;
			if h_seg.abs() > 1e-9 {
				Some(-3.0 * h.y / h_seg)
			} else {
				None
			}
		} else {
			None
		};
		let m_norm = m_norm.unwrap_or_else(|| {
			// Centered-difference auto slope (same rule as the host model).
			match (i.checked_sub(1), points.get(i + 1)) {
				(Some(pi), Some(next)) => {
					let prev = &points[pi];
					let dx = next.x - prev.x;
					if dx.abs() > 1e-9 {
						(next.y - prev.y) / dx
					} else {
						0.0
					}
				}
				(Some(pi), None) if n > 1 => {
					let prev = &points[pi];
					let dx = p.x - prev.x;
					if dx.abs() > 1e-9 { (p.y - prev.y) / dx } else { 0.0 }
				}
				(None, Some(next)) if n > 1 => {
					let dx = next.x - p.x;
					if dx.abs() > 1e-9 { (next.y - p.y) / dx } else { 0.0 }
				}
				_ => 0.0,
			}
		});
		out.points.push(oak_plugin::param_curve::ControlPoint {
			key,
			value,
			slope: m_norm * sy / sx,
		});
	}
	out
}

/// Cheap curve equality for the per-render re-sync (epsilon on
/// coordinates; handles compared too).
fn curve_points_close(
	a: &[gpui_widgets::curve_editor::CurvePoint],
	b: &[gpui_widgets::curve_editor::CurvePoint],
) -> bool {
	use gpui_widgets::curve_editor::CurveVec2;
	let vec_close = |a: &CurveVec2, b: &CurveVec2| {
		(a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6
	};
	let handle_close = |a: &Option<CurveVec2>, b: &Option<CurveVec2>| match (a, b) {
		(None, None) => true,
		(Some(a), Some(b)) => vec_close(a, b),
		_ => false,
	};
	a.len() == b.len()
		&& a.iter().zip(b.iter()).all(|(a, b)| {
			vec_close(
				&CurveVec2::new(a.x, a.y),
				&CurveVec2::new(b.x, b.y),
			) && handle_close(&a.handle_in, &b.handle_in)
				&& handle_close(&a.handle_out, &b.handle_out)
		})
}

/// Whether a parameter asks for a multi-line text field (the
/// `("multiline", true)` property the facade adds to the v3 text node's
/// text inputs).
fn is_multiline(param: &EffectParam) -> bool {
	param
		.properties
		.iter()
		.any(|(k, v)| k == "multiline" && matches!(v, NodeValue::Boolean(true)))
}

/// The SliderValue for a param's current value (int → Integer, float →
/// Float).
fn slider_value(param: &EffectParam) -> SliderValue {
	match param.value {
		NodeValue::Int(v) => SliderValue::Integer(v),
		NodeValue::Float(v) => SliderValue::Float(v),
		_ => SliderValue::Float(param.value.to_double()),
	}
}

/// The option list of a combo/string-combo parameter: the
/// `("combo_value", _)` strings when it has any, else the
/// `("combo_option", _)` labels. A string combo whose current value is not
/// in the list (a font family saved on another machine, say) keeps its
/// value: it goes in front, so the combo shows what the node actually
/// holds instead of silently falling back to the first option.
fn combo_haystack(param: &EffectParam) -> Vec<String> {
	let mut haystack = if param.value_type == ValueType::StrCombo {
		let values = crate::oakui::effectchain::combo_values(param);
		if values.is_empty() {
			crate::oakui::effectchain::combo_options(param)
		} else {
			values
		}
	} else {
		crate::oakui::effectchain::combo_options(param)
	};
	if param.value_type == ValueType::StrCombo {
		if let NodeValue::StrCombo(s) | NodeValue::Text(s) = &param.value {
			if !s.is_empty() && !haystack.iter().any(|v| v == s) {
				haystack.insert(0, s.clone());
			}
		}
	}
	haystack
}

/// The selected option index of a combo/string-combo parameter. Integer
/// combos carry the index directly; string combos are matched against the
/// [`combo_haystack`] list by value.
fn combo_index_for(param: &EffectParam) -> usize {
	if param.value_type == ValueType::StrCombo {
		let haystack = combo_haystack(param);
		match &param.value {
			NodeValue::StrCombo(s) | NodeValue::Text(s) => {
				haystack.iter().position(|v| v == s).unwrap_or(0)
			}
			_ => 0,
		}
	} else {
		param.value.to_double().max(0.0) as usize
	}
}

/// The component list of a vec/color value (empty for other types).
fn value_components(value: &NodeValue) -> Vec<f64> {
	match value {
		NodeValue::Vec2(v) => vec![v[0], v[1]],
		NodeValue::Vec3(v) => vec![v[0], v[1], v[2]],
		NodeValue::Vec4(v) => vec![v[0], v[1], v[2], v[3]],
		NodeValue::Color(v) => vec![v[0], v[1], v[2], v[3]],
		_ => Vec::new(),
	}
}

/// The default numeric range when the parameter carries no min/max
/// properties (the OFX translation only attaches min/max to colour
/// inputs). Wide ranges keep every value reachable; the slider's
/// double-click entry allows exact typing.
fn default_range(value_type: ValueType) -> (f64, f64) {
	match value_type {
		ValueType::Int => (-100000.0, 100000.0),
		_ => (-10000.0, 10000.0),
	}
}

/// The min/max from the parameter's `("min", _)` / `("max", _)`
/// properties, falling back to [`default_range`].
fn numeric_range(param: &EffectParam) -> (f64, f64) {
	let (dmin, dmax) = default_range(param.value_type);
	(
		range_property(param, "min").unwrap_or(dmin),
		range_property(param, "max").unwrap_or(dmax),
	)
}

/// The `("min", _)` / `("max", _)` property of a parameter as a finite
/// number: a NaN or infinite bound would make the slider's arithmetic
/// meaningless, so it is ignored.
fn range_property(param: &EffectParam, key: &str) -> Option<f64> {
	param
		.properties
		.iter()
		.find(|(k, _)| k == key)
		.and_then(|(_, v)| match v {
			NodeValue::Float(f) => Some(*f),
			NodeValue::Int(i) => Some(*i as f64),
			_ => None,
		})
		.filter(|v| v.is_finite())
}

/// The slider range and step of an int/float parameter: `(min, max, step)`.
///
/// The OFX translation only attaches min/max to colour inputs, but the
/// built-in nodes attach `("min", _)` to the parameters whose domain has a
/// floor and no ceiling (font size, outline width, glow radius). A flat
/// default range then snaps the value onto a coarse grid nowhere near it —
/// `font_size_in` at 72 showed as 50.995 over a 1..10000 range. Instead the
/// value itself defines the grid: a "nice" step that divides the value's
/// distance from its bound, over a range holding 200 of them, so a
/// min-only parameter slides up from its floor with the handle exactly on
/// the value. The slider's double-click entry types exact values, so the
/// range never has to cover everything.
fn slider_range_and_step(param: &EffectParam) -> (f64, f64, f64) {
	let value = param.value.to_double();
	if param.value_type == ValueType::Int {
		// Integer parameters step by one; only the bounds need ordering
		// (an inverted range would panic the slider's clamp).
		let (min, max) = numeric_range(param);
		let (mut lo, mut hi) = (min.min(max), min.max(max));
		if value.is_finite() {
			lo = lo.min(value);
			hi = hi.max(value);
		}
		if hi <= lo {
			hi = lo + 200.0;
		}
		return (lo, hi, 1.0);
	}
	match (range_property(param, "min"), range_property(param, "max")) {
		// A floor but no ceiling.
		(Some(min), None) => {
			let v = if value.is_finite() { value.max(min) } else { min };
			let span = v - min;
			let step = if span > 0.0 {
				nice_grid_step(span)
			} else {
				1.0
			};
			(min, min + 200.0 * step, step)
		}
		// A ceiling but no floor: the mirror image.
		(None, Some(max)) => {
			let v = if value.is_finite() { value.min(max) } else { max };
			let span = max - v;
			let step = if span > 0.0 {
				nice_grid_step(span)
			} else {
				1.0
			};
			(max - 200.0 * step, max, step)
		}
		// Both bounds, or neither (the wide default range): one step over
		// the whole range, rounded so the value lands on the grid.
		(bound_min, bound_max) => {
			let (dmin, dmax) = default_range(param.value_type);
			let min = bound_min.unwrap_or(dmin);
			let max = bound_max.unwrap_or(dmax);
			let (lo, hi) = (min.min(max), min.max(max));
			let step0 = ((hi - lo) / 200.0).max(0.001);
			let span = if value.is_finite() {
				value.clamp(lo, hi) - lo
			} else {
				0.0
			};
			let step = if span > 0.0 {
				span / (span / step0).round().max(1.0)
			} else {
				step0
			};
			(lo, hi, step)
		}
	}
}

/// A "nice" step for a slider whose value sits `span` above its bound: one
/// of `1/2/5 × 10^k`, close to a twenty-fifth of the span (a slider wants a
/// few dozen steps to feel controllable) and dividing the span into a whole
/// number of them, so the value itself is on the grid.
fn nice_grid_step(span: f64) -> f64 {
	let target = span / 25.0;
	if !(target.is_finite() && target > 0.0) {
		return 1.0;
	}
	let exp = target.log10().floor() as i32;
	let mut best: Option<f64> = None;
	for k in -2..=2 {
		for m in [1.0, 2.0, 5.0] {
			let step = scale_by_pow10(m, exp + k);
			if !(step.is_finite() && step > 0.0) || step < span / 100.0 || step > span / 8.0 {
				continue;
			}
			let steps = span / step;
			if steps < 1.0 || (steps - steps.round()).abs() > 1e-9 * steps.abs().max(1.0) {
				continue;
			}
			let better = match best {
				Some(b) => (step / target).ln().abs() < (b / target).ln().abs(),
				None => true,
			};
			if better {
				best = Some(step);
			}
		}
	}
	best.unwrap_or_else(|| {
		let steps = (span / target).round().max(1.0);
		span / steps
	})
}

/// `m × 10^exp`, by repeated multiplication rather than `powf`: the
/// result is then bit-identical to the literal grid values (0.1, 0.01, …)
/// that a slider step of that size is expected to land on.
fn scale_by_pow10(m: f64, exp: i32) -> f64 {
	let mut v = m;
	if exp >= 0 {
		for _ in 0..exp {
			v *= 10.0;
		}
	} else {
		for _ in 0..-exp {
			v /= 10.0;
		}
	}
	v
}

/// Builds one [`ParamControl`] for `param`, creating the control entities
/// (each consuming one control id from `next_id`).
fn build_control<E: AppEngine>(
	param: &EffectParam,
	next_id: &mut usize,
	window: &mut Window,
	cx: &mut Context<OfxParamsView<E>>,
) -> ParamControl {
	let kind = match param.value_type {
		ValueType::Int | ValueType::Float => {
			let (min, max, step) = slider_range_and_step(param);
			let kind = if param.value_type == ValueType::Int {
				ValueKind::Integer
			} else {
				ValueKind::Float
			};
			let default_raw = param.value.to_double().clamp(min, max);
			let model = SliderModel::new(kind, min, max, step, default_raw);
			let slider = cx.new(|cx| Slider::new(*next_id, model, window, cx));
			*next_id += 1;
			ControlKind::Slider(slider)
		}
		ValueType::Boolean => {
			let state = match param.value {
				NodeValue::Boolean(true) => CheckState::Checked,
				_ => CheckState::Unchecked,
			};
			let check = cx.new(|cx| CheckBox::new(*next_id, state, window, cx));
			*next_id += 1;
			ControlKind::CheckBox(check)
		}
		ValueType::Combo | ValueType::StrCombo => {
			let options = combo_haystack(param);
			if options.is_empty() {
				// No option list: show the raw value read-only.
				let text = if param.value_type == ValueType::Combo {
					format!("{}", param.value.to_double() as i64)
				} else {
					match &param.value {
						NodeValue::StrCombo(s) | NodeValue::Text(s) => s.clone(),
						_ => String::new(),
					}
				};
				ControlKind::ReadOnly(text.into())
			} else {
				let options = options
					.iter()
					.enumerate()
					.map(|(i, label)| ComboBoxOption::new(i, label.clone()))
					.collect();
				let combo = cx.new(|cx| ComboBox::new(*next_id, options, window, cx));
				let index = combo_index_for(param);
				combo.update(cx, |combo, cx| combo.set_selected(Some(index), cx));
				*next_id += 1;
				ControlKind::Combo(combo)
			}
		}
		ValueType::Text => {
			let text = match &param.value {
				NodeValue::Text(s) => s.clone(),
				_ => String::new(),
			};
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			editor.update(cx, |editor, cx| editor.emplace(&text, cx));
			*next_id += 1;
			ControlKind::Text {
				editor,
				multiline: is_multiline(param),
			}
		}
		ValueType::Vec2 | ValueType::Vec3 => {
			let components = value_components(&param.value);
			let count = if param.value_type == ValueType::Vec2 {
				2
			} else {
				3
			};
			let (min, max) = default_range(param.value_type);
			let mut spins = Vec::new();
			for channel in 0..count {
				let value = components.get(channel).copied().unwrap_or(0.0).clamp(min, max);
				let model = SliderModel::new(ValueKind::Float, min, max, 0.001, value);
				let spin = cx.new(|cx| SpinBox::new(*next_id, model, window, cx));
				*next_id += 1;
				spins.push((spin, channel));
			}
			ControlKind::Spin(spins)
		}
		ValueType::Color => {
			// Colour params get the swatch + popup picker (channels are
			// always 0..1 regardless of any attached min/max).
			let components = value_components(&param.value);
			let color = Rgba {
				r: components.get(0).copied().unwrap_or(0.0) as f32,
				g: components.get(1).copied().unwrap_or(0.0) as f32,
				b: components.get(2).copied().unwrap_or(0.0) as f32,
				a: components.get(3).copied().unwrap_or(1.0) as f32,
			};
			let picker = cx.new(|cx| OfxColorPicker::new(*next_id, color, window, cx));
			*next_id += 1;
			ControlKind::Color(picker)
		}
		ValueType::PushButton => ControlKind::PushButton,
		ValueType::Parametric => {
			// One curve editor per dimension; points are seeded from the
			// JSON mirror of the curves (the input's Text value).
			let curves = match &param.value {
				NodeValue::Text(json) => oak_plugin::param_curve::curves_from_json(json)
					.unwrap_or_default(),
				_ => Vec::new(),
			};
			let domain = curve_domain(param, &curves);
			let mut editors = Vec::new();
			for curve in &curves {
				let points = curve
					.points
					.iter()
					.map(|p| curve_point_to_editor(p, &curve.points, domain))
					.collect::<Vec<_>>();
				let editor = cx.new(|cx| {
					gpui_widgets::curve_editor::CurveEditor::new(*next_id, points, window, cx)
				});
				*next_id += 1;
				editors.push(editor);
			}
			return ParamControl {
				input_id: param.input_id.clone(),
				display_name: param.display_name.clone(),
				section: crate::oakui::effectchain::ui_section_of(param),
				kind: ControlKind::Curve(editors),
				curve_domain: Some(domain),
			};
		}
		// Custom / binary and anything without an editable control: a
		// read-only line (or nothing).
		_ => ControlKind::ReadOnly(SharedString::new("")),
	};

	ParamControl {
		input_id: param.input_id.clone(),
		display_name: param.display_name.clone(),
		section: crate::oakui::effectchain::ui_section_of(param),
		kind,
		curve_domain: None,
	}
}

/// Wires every control's events to the engine's `set_effect_param` /
/// `effect_push_button`.
fn wire_controls<E: AppEngine>(view: &OfxParamsView<E>, cx: &mut Context<OfxParamsView<E>>) {
	for control in &view.controls {
		let input_id = control.input_id.clone();
		let effect = view.effect;
		let engine = view.engine.clone();
		match &control.kind {
			ControlKind::Slider(slider) => {
				let slider = slider.clone();
				cx.subscribe(&slider, move |_, _, event: &crate::oakui::component::controls::SliderEvent, cx| {
					if let crate::oakui::component::controls::SliderEvent::ValueChanged { value, .. } = event {
						let nv = match value {
							SliderValue::Integer(v) => NodeValue::Int(*v),
							_ => NodeValue::Float(value.to_f64()),
						};
						engine.update(cx, |engine, cx| {
							let _ = engine.set_effect_param(effect, &input_id, nv, cx);
						});
					}
				})
				.detach();
			}
			ControlKind::CheckBox(check) => {
				let check = check.clone();
				cx.subscribe(&check, move |_, _, event: &CheckBoxEvent, cx| {
					let CheckBoxEvent::Toggled { state, .. } = event;
					let nv = NodeValue::Boolean(*state == CheckState::Checked);
					engine.update(cx, |engine, cx| {
						let _ = engine.set_effect_param(effect, &input_id, nv, cx);
					});
				})
				.detach();
			}
			ControlKind::Combo(combo) => {
				let combo = combo.clone();
				cx.subscribe(&combo, move |_, _, event: &ComboBoxEvent, cx| {
					let ComboBoxEvent::Selected { value, .. } = event;
					// Integer combos carry the index; string combos map
					// the picked option back to its string value.
					let param = engine.update(cx, |engine, _cx| {
						engine
							.effect_params(effect)
							.unwrap_or_default()
							.into_iter()
							.find(|p| p.input_id == input_id)
					});
					let nv = match &param {
						Some(p) if p.value_type == ValueType::StrCombo => {
							let haystack = combo_haystack(p);
							NodeValue::StrCombo(
								haystack.get(*value).cloned().unwrap_or_default(),
							)
						}
						_ => NodeValue::Combo(*value as i64),
					};
					engine.update(cx, |engine, cx| {
						let _ = engine.set_effect_param(effect, &input_id, nv, cx);
					});
				})
				.detach();
			}
			ControlKind::Spin(spins) => {
				let spins = spins.clone();
				for (spin, channel) in spins {
					let spin = spin.clone();
					let channel = channel;
					// Clone per iteration: each spinbox's closure owns its
					// own engine / input id.
					let engine = engine.clone();
					let input_id = input_id.clone();
					cx.subscribe(&spin, move |_, _, event: &SpinBoxEvent, cx| {
						if let SpinBoxEvent::ValueChanged { value, .. } = event {
							// Re-read the current value, patch the changed
							// component, and write the whole value back.
							let patched = engine.update(cx, |engine, cx| {
								let params = engine.effect_params(effect).unwrap_or_default();
								let current = params
									.iter()
									.find(|p| p.input_id == input_id)
									.map(|p| p.value.clone())
									.unwrap_or(NodeValue::None);
								let patched = patch_component(&current, channel, value.to_f64());
								engine.set_effect_param(effect, &input_id, patched, cx).is_ok()
							});
							let _ = patched;
						}
					})
					.detach();
				}
			}
			ControlKind::Color(picker) => {
				let picker = picker.clone();
				cx.subscribe(&picker, move |_, _, event: &OfxColorEvent, cx| match event {
					// Only OK commits; slider drags update the draft inside
					// the picker, so a drag session is one undo row.
					OfxColorEvent::Committed(color) => {
						let nv = NodeValue::Color([
							color.r as f64,
							color.g as f64,
							color.b as f64,
							color.a as f64,
						]);
						engine.update(cx, |engine, cx| {
							if let Err(err) = engine.set_effect_param(effect, &input_id, nv, cx) {
								// A failed set is the only path that snaps the
								// swatch back to the old engine value (the value
								// sync re-reads it every frame), so log it rather
								// than swallowing it.
								println!("[ofx params] set colour param {input_id:?} failed: {err}");
							}
						});
					}
					// Arming the picker points the program viewer's cursor at
					// the frame; the next click there lands in the draft.
					OfxColorEvent::PickViewerToggle { armed } => {
						engine.update(cx, |engine, cx| {
							engine.set_eyedropper_armed(*armed, cx);
						});
					}
					// Open/close are purely local to the popup.
					OfxColorEvent::Opened | OfxColorEvent::Cancelled => {}
				})
				.detach();
			}
			ControlKind::Text { .. } => {
				// The text field commits explicitly (the commit button in the
				// row). No event subscription here: the params view is rebuilt
				// on every card render, so committing on TextChanged would
				// re-enter the engine update on the same frame the value is
				// re-synced (an endless re-render loop).
			}
			ControlKind::Curve(editors) => {
				// Any point/handle edit on any dimension's editor rebuilds
				// the whole curve set and commits it as the JSON mirror
				// (undoable via the engine's set_effect_param).
				for editor in editors.iter() {
					let editor = editor.clone();
					let editors_all = editors.clone();
					let domain = control.curve_domain.unwrap_or((0.0, 1.0, 0.0, 1.0));
					let input_id = input_id.clone();
					let engine = engine.clone();
					cx.subscribe(&editor, move |_, _, event: &gpui_widgets::curve_editor::CurveEditorEvent, cx| {
						use gpui_widgets::curve_editor::CurveEditorEvent as E;
						match event {
							E::PointMoved { .. } | E::HandleMoved { .. } | E::PointAdded { .. } => {}
						}
						let curves: Vec<oak_plugin::param_curve::Curve> = editors_all
							.iter()
							.map(|e| {
								let points = e.read(cx).points().to_vec();
								curve_from_editor(&points, domain)
							})
							.collect();
						let json = oak_plugin::param_curve::curves_to_json(&curves);
						engine.update(cx, |engine, cx| {
							let _ = engine.set_effect_param(
								effect,
								&input_id,
								NodeValue::Text(json),
								cx,
							);
						});
					})
					.detach();
				}
			}
			ControlKind::PushButton | ControlKind::ReadOnly(_) => {}
		}
	}
}

/// Returns `value` with the component at `channel` replaced by `component`.
fn patch_component(value: &NodeValue, channel: usize, component: f64) -> NodeValue {
	let mut out = value.clone();
	match &mut out {
		NodeValue::Vec2(v) if channel < 2 => v[channel] = component,
		NodeValue::Vec3(v) if channel < 3 => v[channel] = component,
		NodeValue::Vec4(v) if channel < 4 => v[channel] = component,
		NodeValue::Color(v) if channel < 4 => v[channel] = component,
		_ => {}
	}
	out
}

impl<E: AppEngine> Render for OfxParamsView<E> {
	fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		self.sync_values(window, cx);

		let mut body = div().flex().flex_col().gap_1().p_2();
		let mut last_section: Option<(String, String)> = None;
		for control in &self.controls {
			if control.section != last_section {
				last_section = control.section.clone();
				if let Some((group, page)) = &last_section {
					let title = if page.is_empty() {
						group.clone()
					} else if group.is_empty() {
						page.clone()
					} else {
						format!("{group} · {page}")
					};
					body = body.child(
						div()
							.py_1()
							.text_xs()
							.font_weight(gpui::FontWeight(600.0))
							.text_color(colors.selected)
							.child(title),
					);
				}
			}

			// A push button renders full-width with its own label (the OFX
			// param label IS the button text); every other control gets the
			// label column + control layout.
			let row_element: gpui::AnyElement = if matches!(control.kind, ControlKind::PushButton) {
				let engine = self.engine.clone();
				let effect = self.effect;
				let input_id = control.input_id.clone();
				let button_label = control.display_name.clone();
				div()
					.id(SharedString::from(format!("ofx-push-{}", control.input_id)))
					.flex_1()
					.cursor_pointer()
					.rounded_sm()
					.border_1()
					.border_color(colors.border)
					.bg(colors.selected)
					.text_sm()
					.text_color(colors.text)
					.text_center()
					.py_1()
					.child(button_label)
					.on_click(move |_event: &ClickEvent, _window, cx| {
						engine.update(cx, |engine, cx| {
							let _ = engine.effect_push_button(effect, &input_id, cx);
						});
					})
					.into_any_element()
			} else {
				let label = div()
					.flex_shrink_0()
					.w(px(110.0))
					.text_sm()
					.text_color(colors.text)
					.child(control.display_name.clone());
				let widget = match &control.kind {
					ControlKind::Slider(slider) => div().flex_1().child(slider.clone()).into_any_element(),
					ControlKind::CheckBox(check) => div().flex_1().child(check.clone()).into_any_element(),
					ControlKind::Combo(combo) => div().flex_1().child(combo.clone()).into_any_element(),
					ControlKind::Spin(spins) => {
						let mut row = div().flex_1().flex().gap_1();
						for (spin, _) in spins {
							row = row.child(div().flex_1().child(spin.clone()));
						}
						row.into_any_element()
					}
					ControlKind::Color(picker) => {
						div().flex_1().child(picker.clone()).into_any_element()
					}
					ControlKind::Text { editor, multiline } => {
						let weak = editor.downgrade();
						let engine = self.engine.clone();
						let effect = self.effect;
						let input_id = control.input_id.clone();
						let editor_commit = editor.clone();
						let field = if *multiline {
							// A multi-line field. `text_input` above is
							// single-line only (and keeps its theme colors
							// to itself), so the element is built here with
							// the same colors the component would use, in a
							// fixed-height box the text scrolls inside.
							gpui_elements::editable_text::text_area(format!(
								"ofx-param-{}",
								control.input_id
							))
							.state(weak)
							.accepts_input(true)
							.h(px(96.0))
							.text_color(colors.text)
							.placeholder_color(colors.disabled.into())
							.selection_color(colors.selected.into())
							.caret_color(colors.text.into())
							.marked_color(colors.text.into())
							.into_any_element()
						} else {
							text_input(format!("ofx-param-{}", control.input_id), cx)
								.state(weak)
								.accepts_input(true)
								.into_any_element()
						};
						div()
							.flex_1()
							.flex()
							.gap_1()
							.child(
								div()
									.flex_1()
									.rounded_md()
									.border_1()
									.border_color(colors.border)
									.bg(colors.background)
									.px_2()
									.py_1()
									.child(field),
							)
							.child(
								// Explicit commit: reads the field and pushes the
								// string to the engine (avoids the re-render loop of
								// committing on every keystroke).
								div()
									.id(SharedString::from(format!("ofx-commit-{}", control.input_id)))
									.cursor_pointer()
									.rounded_sm()
									.border_1()
									.border_color(colors.border)
									.bg(colors.selected)
									.text_sm()
									.text_color(colors.text)
									.px_2()
									.py_1()
									.child("✓")
									.on_click(move |_event: &ClickEvent, _window, cx| {
										let text = editor_commit.read(cx).as_str().to_string();
										engine.update(cx, |engine, cx| {
											let _ = engine
												.set_effect_param(effect, &input_id, NodeValue::Text(text), cx);
										});
									}),
							)
							.into_any_element()
					}
					ControlKind::ReadOnly(text) => div()
						.flex_1()
						.text_sm()
						.text_color(colors.disabled)
						.child(text.clone())
						.into_any_element(),
					ControlKind::Curve(editors) => {
						// One curve editor per dimension, stacked; each is a
						// fixed-height canvas.
						let mut col = div().flex_1().flex().flex_col().gap_1();
						for editor in editors {
							col = col.child(
								div()
									.h_24()
									.rounded_md()
									.border_1()
									.border_color(colors.border)
									.bg(colors.background)
									.child(editor.clone()),
							);
						}
						col.into_any_element()
					}
					ControlKind::PushButton => unreachable!("handled above"),
				};
				div()
					.id(SharedString::from(format!("ofx-param-{}", control.input_id)))
					.flex()
					.items_center()
					.gap_2()
					.child(label)
					.child(widget)
					.into_any_element()
			};
			body = body.child(row_element);
		}
		if self.controls.is_empty() {
			body = body.child(
				div()
					.text_sm()
					.text_color(colors.disabled)
					.child(crate::i18n::tr("inspector.params")),
			);
		}
		body
	}
}

// ---------------------------------------------------------------------------
// OfxColorPicker — colour swatch + deferred popup picker
// ---------------------------------------------------------------------------

/// A request emitted by an [`OfxColorPicker`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OfxColorEvent {
	/// The popup was opened (draft reset to the committed colour).
	Opened,
	/// The popup was dismissed without committing.
	Cancelled,
	/// OK pressed: the caller should commit this colour (undoable).
	Committed(Rgba),
	/// The viewer eyedropper was armed (`true`) or disarmed (`false`); the
	/// params view routes it to the engine, which mirrors it into the
	/// program viewer.
	PickViewerToggle { armed: bool },
}

/// A colour swatch with a deferred popup picker, used for OFX colour
/// parameters (the replacement for the old per-channel spinboxes).
///
/// - The swatch shows the **committed** colour (the engine value) over a
///   two-tone checkerboard so alpha is visible.
/// - Clicking opens a deferred popup with a Photoshop-style palette (an
///   S/V square + hue bar, switchable to RGB sliders), a live preview
///   swatch, a hex field (`#RRGGBB` / `#RRGGBBAA`, validated) and
///   Cancel / OK buttons.
/// - Slider drags only mutate the **draft**; only OK emits
///   [`OfxColorEvent::Committed`], which the params view routes through
///   [`AppEngine::set_effect_param`] (undoable). A drag session is
///   therefore a single undo row. Cancel / Escape / outside click discard
///   the draft.
///
/// The picker is a child entity of the params view and carries its own
/// state across frames; [`OfxColorPicker::set_committed`] re-syncs it from
/// the engine each frame (undo / redo / external edits land on the swatch).
pub struct OfxColorPicker {
	/// Stable control id (element ids / slider ids).
	control: usize,
	/// Whether the popup is open.
	open: bool,
	/// Popup anchor position (window space, set when opening).
	position: Point<Pixels>,
	/// The committed colour (what the swatch shows).
	committed: Rgba,
	/// The draft colour while the popup is open (what OK commits).
	draft: Rgba,
	/// Whether the last hex parse failed (error hint in the popup).
	hex_error: bool,
	/// Whether the popup was open when the swatch was pressed (a second
	/// click closes it).
	was_open_at_down: bool,
	/// The editing mode of the primary sliders (RGB channels or HSV).
	mode: ColorMode,
	/// Primary channel sliders (R/G/B or H/S/V, re-ranged on mode switch).
	c0: Entity<Slider>,
	c1: Entity<Slider>,
	c2: Entity<Slider>,
	/// The alpha slider (always 0..1).
	a: Entity<Slider>,
	/// The hex editor (`#RRGGBB` / `#RRGGBBAA`).
	hex: Entity<EditableTextState>,
	/// Whether the viewer eyedropper is armed (a click on the program
	/// viewer samples a pixel back into the draft).
	picking: bool,
	/// The mouse-up that ends a viewer-pick click must not count as an
	/// outside-click dismissal: the pick lands (disarming the eyedropper)
	/// between the button going down on the viewer and coming up, so the
	/// `picking` guard alone cannot tell the pick's own click apart from
	/// a later genuine outside click. Set by [`Self::apply_viewer_pick`],
	/// consumed by the next `on_mouse_up_out`.
	swallow_next_outside_up: bool,
}

/// How the primary channel sliders present the colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
	/// Red / Green / Blue, each 0..1.
	Rgb,
	/// Hue (0..360°) / Saturation / Value (0..1).
	Hsv,
}

impl OfxColorPicker {
	/// Create a picker for `control` showing `color`.
	pub(crate) fn new(
		control: usize,
		color: Rgba,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let picker = Self {
			control,
			open: false,
			position: Point::default(),
			committed: color,
			draft: color,
			hex_error: false,
			was_open_at_down: false,
			mode: ColorMode::Rgb,
			c0: Self::channel_slider(cx, window, 0, 0.0, 1.0, 0.01, color.r as f64),
			c1: Self::channel_slider(cx, window, 1, 0.0, 1.0, 0.01, color.g as f64),
			c2: Self::channel_slider(cx, window, 2, 0.0, 1.0, 0.01, color.b as f64),
			a: Self::channel_slider(cx, window, 3, 0.0, 1.0, 0.01, color.a as f64),
			hex: cx.new(|cx| EditableTextState::new(StringStorage::default(), cx)),
			picking: false,
			swallow_next_outside_up: false,
		};
		let hex_text = format_hex(color);
		picker.hex.update(cx, |hex, cx| hex.emplace(&hex_text, cx));
		picker
	}

	/// Build a float slider over `min..=max` with `step` for one channel and
	/// subscribe its edits to [`Self::on_slider`].
	fn channel_slider(
		cx: &mut Context<Self>,
		window: &mut Window,
		channel: usize,
		min: f64,
		max: f64,
		step: f64,
		value: f64,
	) -> Entity<Slider> {
		let model = SliderModel::new(ValueKind::Float, min, max, step, value.clamp(min, max));
		let slider = cx.new(|cx| Slider::new(channel * 1000 + 100, model, window, cx));
		cx.subscribe(
			&slider,
			move |this: &mut Self, _: Entity<Slider>, event: &SliderEvent, cx| {
				this.on_slider(channel, event, cx);
			},
		)
		.detach();
		slider
	}

	/// A slider changed: update the draft channel and re-format the hex
	/// field (no commit — the value only lands on OK). In HSV mode the
	/// primary channels edit hue/saturation/value and write the derived RGB
	/// back into the draft.
	fn on_slider(&mut self, channel: usize, event: &SliderEvent, cx: &mut Context<Self>) {
		if let SliderEvent::ValueChanged { value, .. } = event {
			match self.mode {
				ColorMode::Rgb => {
					let v = value.to_f64().clamp(0.0, 1.0) as f32;
					match channel {
						0 => self.apply_draft_rgb(v, self.draft.g, self.draft.b, cx),
						1 => self.apply_draft_rgb(self.draft.r, v, self.draft.b, cx),
						2 => self.apply_draft_rgb(self.draft.r, self.draft.g, v, cx),
						3 => self.apply_draft_alpha(v, cx),
						_ => return,
					}
				}
				ColorMode::Hsv => {
					let (mut h, mut s, mut v) =
						rgb_to_hsv(self.draft.r, self.draft.g, self.draft.b);
					match channel {
						0 => h = value.to_f64().clamp(0.0, 360.0) as f32,
						1 => s = value.to_f64().clamp(0.0, 1.0) as f32,
						2 => v = value.to_f64().clamp(0.0, 1.0) as f32,
						3 => {
							self.apply_draft_alpha(value.to_f64().clamp(0.0, 1.0) as f32, cx);
							return;
						}
						_ => return,
					}
					let rgb = hsv_to_rgb(h, s, v);
					self.apply_draft_rgb(rgb.r, rgb.g, rgb.b, cx);
				}
			}
		}
	}

	/// Set the draft RGB from a computed colour and refresh the hex field.
	fn apply_draft_rgb(&mut self, r: f32, g: f32, b: f32, cx: &mut Context<Self>) {
		self.draft.r = r;
		self.draft.g = g;
		self.draft.b = b;
		self.refresh_hex(cx);
	}

	/// Set the draft alpha and refresh the hex field.
	fn apply_draft_alpha(&mut self, a: f32, cx: &mut Context<Self>) {
		self.draft.a = a;
		self.refresh_hex(cx);
	}

	/// Clear the hex error, re-format the hex field from the draft and
	/// repaint (shared by every draft mutation).
	fn refresh_hex(&mut self, cx: &mut Context<Self>) {
		self.hex_error = false;
		let hex_entity = self.hex.clone();
		let hex_text = format_hex(self.draft);
		hex_entity.update(cx, |hex, cx| {
			if hex.as_str() != hex_text {
				hex.emplace(&hex_text, cx);
			}
		});
		cx.notify();
	}

	/// S/V palette click / drag: keep the draft's hue, take the picked
	/// saturation / value, write the result back into the draft.
	fn on_palette(&mut self, s: f32, v: f32, cx: &mut Context<Self>) {
		let (h, _, _) = rgb_to_hsv(self.draft.r, self.draft.g, self.draft.b);
		let rgb = hsv_to_rgb(h, s, v);
		self.apply_draft_rgb(rgb.r, rgb.g, rgb.b, cx);
	}

	/// Hue bar click / drag: keep the draft's saturation / value, take the
	/// picked hue, write the result back into the draft.
	fn on_hue(&mut self, h: f32, cx: &mut Context<Self>) {
		let (_, s, v) = rgb_to_hsv(self.draft.r, self.draft.g, self.draft.b);
		let rgb = hsv_to_rgb(h, s, v);
		self.apply_draft_rgb(rgb.r, rgb.g, rgb.b, cx);
	}

	/// Re-sync the sliders and the hex field from the current draft (no
	/// events emitted; used when the draft is reset or committed). In HSV
	/// mode the primary sliders show the hue/saturation/value of the draft.
	fn sync_from_draft(&self, cx: &mut Context<Self>) {
		let (a, b, c) = match self.mode {
			ColorMode::Rgb => (self.draft.r, self.draft.g, self.draft.b),
			ColorMode::Hsv => {
				let (h, s, v) = rgb_to_hsv(self.draft.r, self.draft.g, self.draft.b);
				(h, s, v)
			}
		};
		let values = [a as f64, b as f64, c as f64, self.draft.a as f64];
		let sliders = [&self.c0, &self.c1, &self.c2, &self.a];
		for (slider, value) in sliders.iter().zip(values.iter()) {
			let slider = *slider;
			slider.update(cx, |slider, _| {
				slider.set_value(SliderValue::Float(*value));
			});
		}
		let hex_entity = self.hex.clone();
		let hex_text = format_hex(self.draft);
		hex_entity.update(cx, |hex, cx| {
			if hex.as_str() != hex_text {
				hex.emplace(&hex_text, cx);
			}
		});
	}

	/// Switch the primary sliders between RGB and HSV editing. The draft is
	/// preserved: the other mode's channels are derived from it.
	fn set_mode(&mut self, mode: ColorMode, cx: &mut Context<Self>) {
		if self.mode == mode {
			return;
		}
		self.mode = mode;
		self.rebuild_primary_sliders(cx);
		self.sync_from_draft(cx);
		cx.notify();
	}

	/// Re-range the three primary sliders for the current mode (RGB 0..1 /
	/// HSV 0..360/0..1/0..1) without rebuilding the entities, so in-flight
	/// subscriptions stay intact.
	fn rebuild_primary_sliders(&mut self, cx: &mut Context<Self>) {
		let (ranges, vals) = match self.mode {
			ColorMode::Rgb => (
				[(0.0, 1.0, 0.01); 3],
				[
					self.draft.r as f64,
					self.draft.g as f64,
					self.draft.b as f64,
				],
			),
			ColorMode::Hsv => {
				let (h, s, v) = rgb_to_hsv(self.draft.r, self.draft.g, self.draft.b);
				(
					[(0.0, 360.0, 1.0), (0.0, 1.0, 0.01), (0.0, 1.0, 0.01)],
					[h as f64, s as f64, v as f64],
				)
			}
		};
		let sliders = [&self.c0, &self.c1, &self.c2];
		for (slider, (range, value)) in sliders
			.into_iter()
			.zip(ranges.iter().zip(vals.iter()))
		{
			let (min, max, step) = *range;
			let value = *value;
			let slider = slider.clone();
			slider.update(cx, |slider, _| {
				slider.set_model(SliderModel::new(ValueKind::Float, min, max, step, value));
			});
		}
	}

	/// Apply a committed colour from the engine (called every frame from
	/// the params view's value sync). While the popup is open the draft is
	/// left alone so an in-progress edit is not clobbered by re-syncs.
	pub(crate) fn set_committed(&mut self, color: Rgba, cx: &mut Context<Self>) {
		if self.committed == color {
			return;
		}
		self.committed = color;
		if !self.open {
			self.draft = color;
			self.sync_from_draft(cx);
		}
		cx.notify();
	}

	/// Toggle the viewer eyedropper. While armed the program viewer samples
	/// the pixel under the cursor on click; the params view routes the
	/// toggle to the engine and polls [`AppEngine::take_eyedropper_result`]
	/// every frame, so this picker stays in sync with the armed state.
	fn toggle_viewer_pick(&mut self, cx: &mut Context<Self>) {
		self.picking = !self.picking;
		cx.emit(OfxColorEvent::PickViewerToggle { armed: self.picking });
		cx.notify();
	}

	/// Apply a colour sampled from the program viewer into the draft and
	/// disarm the eyedropper (the engine already cleared its armed flag).
	pub(crate) fn apply_viewer_pick(&mut self, color: Rgba, cx: &mut Context<Self>) {
		if self.picking {
			self.draft = color;
			self.picking = false;
			self.hex_error = false;
			// The pick's own click is still in flight (the sampled pixel
			// lands before the button comes back up on the viewer): its
			// mouse-up must not dismiss the popup as an outside click.
			self.swallow_next_outside_up = true;
			self.sync_from_draft(cx);
			cx.notify();
		}
	}

	fn open_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
		if !self.open {
			self.open = true;
			self.position = position;
			self.swallow_next_outside_up = false;
			// Start from the committed colour.
			self.draft = self.committed;
			self.hex_error = false;
			self.sync_from_draft(cx);
			cx.emit(OfxColorEvent::Opened);
			cx.notify();
		}
	}

	/// Cancel: discard the draft, keep the committed colour. Also disarms a
	/// viewer eyedropper that was left armed.
	fn close_menu(&mut self, cx: &mut Context<Self>) {
		if self.picking {
			self.picking = false;
			cx.emit(OfxColorEvent::PickViewerToggle { armed: false });
		}
		if self.open {
			self.open = false;
			self.hex_error = false;
			// Discard any in-progress draft edits so the sliders / swatch
			// settle back on the committed colour.
			self.draft = self.committed;
			self.sync_from_draft(cx);
			cx.emit(OfxColorEvent::Cancelled);
			cx.notify();
		}
	}

	/// OK: validate a hand-typed hex edit, then commit the draft. Also
	/// disarms a viewer eyedropper that was left armed.
	fn commit(&mut self, cx: &mut Context<Self>) {
		if self.picking {
			self.picking = false;
			cx.emit(OfxColorEvent::PickViewerToggle { armed: false });
		}
		let text = self.hex.read(cx).as_str().trim().to_string();
		if !text.is_empty() {
			match parse_hex(&text) {
				Some(color) => self.draft = color,
				None => {
					self.hex_error = true;
					cx.notify();
					return;
				}
			}
		}
		self.hex_error = false;
		let color = self.draft;
		self.open = false;
		self.committed = color;
		self.sync_from_draft(cx);
		cx.emit(OfxColorEvent::Committed(color));
		cx.notify();
	}

	/// The popup's anchored subtree (deferred, above the card).
	fn popup_anchored(
		&self,
		cx: &mut Context<Self>,
		colors: &Arc<gpui::colors::Colors>,
	) -> gpui::Deferred {
		let control = self.control;
		let draft = self.draft;
		let hex_weak = self.hex.downgrade();
		let hex_error = self.hex_error;
		let mode = self.mode;

		// RGB / HSV mode tabs.
		let tab = |this_mode: ColorMode| {
			div()
				.id(SharedString::from(format!(
					"ofx-color-mode-{control}-{}",
					if this_mode == ColorMode::Rgb { "rgb" } else { "hsv" }
				)))
				.debug_selector(move || {
					format!(
						"ofx-color-mode-{control}-{}",
						if this_mode == ColorMode::Rgb { "rgb" } else { "hsv" }
					)
					.into()
				})
				.cursor_pointer()
				.flex_1()
				.rounded_sm()
				.border_1()
				.border_color(colors.border)
				.bg(if mode == this_mode {
					colors.selected
				} else {
					colors.background
				})
				.text_sm()
				.text_color(colors.text)
				.flex()
				.items_center()
				.justify_center()
				.py_1()
				.child(crate::i18n::tr(if this_mode == ColorMode::Rgb {
					"ofx.color.mode_rgb"
				} else {
					"ofx.color.mode_hsv"
				}))
				.on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
					this.set_mode(this_mode, cx);
				}))
		};
		let tab_row = div().flex().gap_1().child(tab(ColorMode::Rgb)).child(tab(ColorMode::Hsv));

		// Photoshop-style S/V palette + hue bar. Both record their layout
		// bounds each frame (an invisible canvas) so a click / drag can map
		// the cursor to a value.
		let sv_bounds = std::sync::Arc::new(std::sync::RwLock::new(None::<gpui::Bounds<gpui::Pixels>>));
		let sv_drag = std::sync::Arc::new(std::sync::RwLock::new(SvPaletteDrag));
		let (hue, _, _) = rgb_to_hsv(draft.r, draft.g, draft.b);
		let record_sv = sv_bounds.clone();
		let sv_canvas = canvas(
			move |b, _window, _cx| {
				*record_sv.write().unwrap() = Some(b);
				b
			},
			move |b, _content, window, cx| paint_sv_palette(b, hue, window, cx),
		)
		.size_full();
		let sv_panel = div()
			.id(ElementId::named_usize("ofx-color-sv-palette", control))
			.debug_selector(|| "ofx-color-sv-palette".into())
			.w(px(180.0))
			.h(px(180.0))
			.relative()
			.rounded_md()
			.border_1()
			.border_color(colors.border)
			.overflow_hidden()
			.cursor_pointer()
			.on_mouse_down(
				MouseButton::Left,
				{
					let sv_bounds = sv_bounds.clone();
					cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
						let Some(bounds) = sv_bounds.read().unwrap().as_ref().copied() else {
							return;
						};
						let (s, v) = sv_from_point(bounds, event.position);
						this.on_palette(s, v, cx);
					})
				},
			)
			.on_drag(sv_drag.clone(), |_payload, _offset, _window, cx| {
				cx.new(|_| SvPaletteDragGhost)
			})
			.on_drag_move(
				{
					let sv_bounds = sv_bounds.clone();
					cx.listener(
						move |this,
						      event: &gpui::DragMoveEvent<std::sync::Arc<std::sync::RwLock<SvPaletteDrag>>>,
						      _window,
						      cx| {
							let Some(bounds) = sv_bounds.read().unwrap().as_ref().copied() else {
								return;
							};
							let (s, v) = sv_from_point(bounds, event.event.position);
							this.on_palette(s, v, cx);
						},
					)
				},
			)
			.child(sv_canvas);
		let hue_bounds = std::sync::Arc::new(std::sync::RwLock::new(None::<gpui::Bounds<gpui::Pixels>>));
		let hue_drag = std::sync::Arc::new(std::sync::RwLock::new(HueBarDrag));
		let record_hue = hue_bounds.clone();
		let hue_canvas = canvas(
			move |b, _window, _cx| {
				*record_hue.write().unwrap() = Some(b);
				b
			},
			|b, _content, window, cx| paint_hue_bar(b, window, cx),
		)
		.size_full();
		let hue_bar = div()
			.id(ElementId::named_usize("ofx-color-hue-bar", control))
			.debug_selector(|| "ofx-color-hue-bar".into())
			.w(px(18.0))
			.h(px(180.0))
			.relative()
			.rounded_md()
			.border_1()
			.border_color(colors.border)
			.overflow_hidden()
			.cursor_pointer()
			.on_mouse_down(
				MouseButton::Left,
				{
					let hue_bounds = hue_bounds.clone();
					cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
						let Some(bounds) = hue_bounds.read().unwrap().as_ref().copied() else {
							return;
						};
						this.on_hue(hue_from_point(bounds, event.position), cx);
					})
				},
			)
			.on_drag(hue_drag.clone(), |_payload, _offset, _window, cx| {
				cx.new(|_| HueBarDragGhost)
			})
			.on_drag_move(
				{
					let hue_bounds = hue_bounds.clone();
					cx.listener(
						move |this,
						      event: &gpui::DragMoveEvent<std::sync::Arc<std::sync::RwLock<HueBarDrag>>>,
						      _window,
						      cx| {
							let Some(bounds) = hue_bounds.read().unwrap().as_ref().copied() else {
								return;
							};
							this.on_hue(hue_from_point(bounds, event.event.position), cx);
						},
					)
				},
			)
			.child(hue_canvas);
		let palette_row = div().flex().gap_1().child(sv_panel).child(hue_bar);

		// Four labelled channel sliders — R/G/B/A or H/S/V/A depending on
		// the mode.
		let labels: [&str; 4] = match mode {
			ColorMode::Rgb => ["R", "G", "B", "A"],
			ColorMode::Hsv => ["H", "S", "V", "A"],
		};
		let mut slider_rows = div().flex().flex_col().gap_1();
		for (label, slider) in labels.iter().zip([&self.c0, &self.c1, &self.c2, &self.a]) {
			slider_rows = slider_rows.child(
				div()
					.flex()
					.items_center()
					.gap_1()
					.child(
						div()
							.w(px(12.0))
							.text_sm()
							.text_color(colors.text)
							.child(*label),
					)
					.child(div().flex_1().child(slider.clone())),
			);
		}

		// Live preview swatch (checkerboard + draft) next to the hex field.
		let preview_canvas = canvas(
			|bounds, _window, _cx| bounds,
			move |bounds, _content, window, cx| {
				paint_checker_swatch(bounds, draft, window, cx);
			},
		)
		.size_full();
		let preview = div()
			.w(px(36.0))
			.h(px(24.0))
			.rounded_sm()
			.border_1()
			.border_color(colors.border)
			.overflow_hidden()
			.child(preview_canvas);
		let hex_row = div()
			.flex()
			.items_center()
			.gap_1()
			.child(preview)
			.child(
				div()
					.flex_1()
					.rounded_md()
					.border_1()
					.border_color(colors.border)
					.bg(colors.background)
					.px_2()
					.py_1()
					.child(
						text_input(format!("ofx-color-hex-{control}"), cx)
							.state(hex_weak)
							.accepts_input(true),
					),
			);

		// Hex parse error hint.
		let error_hint = if hex_error {
			div()
				.text_xs()
				.text_color(gpui::rgba(0xff5555))
				.child(crate::i18n::tr("ofx.color.invalid"))
				.into_any_element()
		} else {
			div().into_any_element()
		};

		// Cancel / OK.
		let cancel = div()
			.id(SharedString::from(format!("ofx-color-cancel-{control}")))
			.cursor_pointer()
			.rounded_sm()
			.border_1()
			.border_color(colors.border)
			.bg(colors.background)
			.text_sm()
			.text_color(colors.text)
			.px_2()
			.py_1()
			.child(crate::i18n::tr("ofx.color.cancel"))
			.on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
				this.close_menu(cx);
			}));
		let ok = div()
			.id(SharedString::from(format!("ofx-color-ok-{control}")))
			.cursor_pointer()
			.rounded_sm()
			.border_1()
			.border_color(colors.border)
			.bg(colors.selected)
			.text_sm()
			.text_color(colors.text)
			.px_2()
			.py_1()
			.child(crate::i18n::tr("ofx.color.ok"))
			.on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
				this.commit(cx);
			}));
		let buttons = div().flex().justify_between().gap_1().child(cancel).child(ok);

		// "Pick from viewer": arms the eyedropper in the program viewer; the
		// next click on the frame samples the pixel under the cursor.
		let pick_viewer = div()
			.id(SharedString::from(format!("ofx-color-pick-viewer-{control}")))
			.debug_selector(move || format!("ofx-color-pick-viewer-{control}").into())
			.cursor_pointer()
			.rounded_sm()
			.border_1()
			.border_color(colors.border)
			.bg(if self.picking { colors.selected } else { colors.background })
			.text_sm()
			.text_color(colors.text)
			.px_2()
			.py_1()
			.child(crate::i18n::tr("ofx.color.pick_viewer"))
			.on_click(cx.listener(|this, _event: &ClickEvent, _window, cx| {
				this.toggle_viewer_pick(cx);
			}));

		deferred(
			anchored()
				.position(self.position)
				.anchor(Anchor::TopLeft)
				.offset(point(px(0.0), px(36.0)))
				.snap_to_window_with_margin(px(8.0))
				.child(
				div()
					.w(px(300.0))
					.p_2()
					.rounded_lg()
					.border_1()
					.border_color(colors.border)
					.bg(colors.container)
					.flex()
					.flex_col()
					.gap_1()
					.debug_selector(|| "ofx-color-popup".into())
					.on_mouse_up_out(
						MouseButton::Left,
						cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
							// While the viewer eyedropper is armed, the click
							// that follows is a *pick* on the program viewer,
							// not an outside-click dismissal: the popup must
							// survive it so the sampled colour lands in the
							// draft. The pick lands mid-click (before the
							// button comes up) and disarms the eyedropper, so
							// its mouse-up is swallowed once explicitly.
							if this.swallow_next_outside_up {
								this.swallow_next_outside_up = false;
							} else if !this.picking {
								this.close_menu(cx);
							}
						}),
					)
					.on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
						if event.keystroke.key == "escape" {
							if this.picking {
								this.toggle_viewer_pick(cx);
							} else {
								this.close_menu(cx);
							}
						}
					}))
					.child(tab_row)
					.child(palette_row)
					.child(slider_rows)
					.child(hex_row)
					.child(error_hint)
					.child(buttons)
					.child(pick_viewer),
				),
			)
			.with_priority(1)
	}
}

impl EventEmitter<OfxColorEvent> for OfxColorPicker {}

impl Render for OfxColorPicker {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let control = self.control;
		// While the popup is open the swatch follows the draft live (the
		// user's in-progress edit), otherwise it shows the committed value.
		let swatch_color = if self.open { self.draft } else { self.committed };

		let swatch = div()
			.id(ElementId::named_usize("ofx-color-swatch", control))
			.w(px(28.0))
			.h(px(28.0))
			.rounded_md()
			.border_1()
			.border_color(if self.open {
				colors.selected
			} else {
				colors.border
			})
			.cursor_pointer()
			.overflow_hidden()
			.debug_selector(|| "ofx-color-swatch".into())
			.on_mouse_down(
				MouseButton::Left,
				cx.listener(|this, _event: &MouseDownEvent, _window, _cx| {
					this.was_open_at_down = this.open;
				}),
			)
			.on_click(cx.listener(|this, event: &ClickEvent, _window, cx| {
				if this.was_open_at_down {
					this.close_menu(cx);
				} else {
					this.open_menu(event.position(), cx);
				}
				cx.stop_propagation();
			}))
			.child(canvas(
				|bounds, _window, _cx| bounds,
				move |bounds, _content, window, cx| {
					paint_checker_swatch(bounds, swatch_color, window, cx);
				},
			)
			.size_full());

		let popup = if self.open {
			self.popup_anchored(cx, &colors)
		} else {
			deferred(div())
		};

		div().relative().child(swatch).child(popup)
	}
}

/// Parse `#RRGGBB` or `#RRGGBBAA` into an [`Rgba`] (0..1 components).
/// Rejects a missing `#`, wrong lengths and non-hex digits.
fn parse_hex(input: &str) -> Option<Rgba> {
	let s = input.trim();
	let s = s.strip_prefix('#')?;
	if s.len() != 6 && s.len() != 8 {
		return None;
	}
	let mut bytes = [0u8; 4];
	for i in 0..s.len() / 2 {
		bytes[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
	}
	let (r, g, b, a) = if s.len() == 8 {
		(bytes[0], bytes[1], bytes[2], bytes[3])
	} else {
		(bytes[0], bytes[1], bytes[2], 255)
	};
	Some(Rgba {
		r: r as f32 / 255.0,
		g: g as f32 / 255.0,
		b: b as f32 / 255.0,
		a: a as f32 / 255.0,
	})
}

/// Format an [`Rgba`] as `#RRGGBB` (opaque alpha) or `#RRGGBBAA`.
fn format_hex(color: Rgba) -> String {
	let to = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
	let (r, g, b, a) = (to(color.r), to(color.g), to(color.b), to(color.a));
	if a == 255 {
		format!("#{r:02X}{g:02X}{b:02X}")
	} else {
		format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
	}
}

/// Paint a two-tone checkerboard (8 px cells) with `color` over it — the
/// alpha channel reads through the checkerboard.
fn paint_checker_swatch(
	bounds: Bounds<Pixels>,
	color: Rgba,
	window: &mut Window,
	_cx: &mut App,
) {
	const CELL: f32 = 8.0;
	let width = f32::from(bounds.size.width);
	let height = f32::from(bounds.size.height);
	let cols = (width / CELL).ceil() as i32;
	let rows = (height / CELL).ceil() as i32;
	let light = Hsla::from(rgb(0xe8e8e8));
	let dark = Hsla::from(rgb(0xc0c0c0));
	for y in 0..rows {
		for x in 0..cols {
			let cell = Bounds::new(
				point(
					bounds.left() + px(x as f32 * CELL),
					bounds.top() + px(y as f32 * CELL),
				),
				size(px(CELL), px(CELL)),
			);
			let shade = if (x + y) % 2 == 0 { light } else { dark };
			window.paint_quad(fill(cell, shade));
		}
	}
	// The colour on top; a transparent alpha blends over the checkerboard.
	window.paint_quad(fill(bounds, Hsla::from(color)));
}

/// Drag payload for the S/V palette. The palette reads its own layout
/// bounds from the recorded canvas, so the payload itself is empty.
struct SvPaletteDrag;

/// The invisible ghost that accompanies an S/V palette drag (gpui requires
/// one for `on_drag`).
struct SvPaletteDragGhost;

impl Render for SvPaletteDragGhost {
	fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
		div()
	}
}

/// Drag payload for the hue bar.
struct HueBarDrag;

/// The invisible ghost that accompanies a hue-bar drag.
struct HueBarDragGhost;

impl Render for HueBarDragGhost {
	fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
		div()
	}
}

/// Convert RGB (0..1 components) to HSV: hue in degrees (0..360, 0 when
/// the colour is achromatic), saturation 0..1, value 0..1.
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
	let max = r.max(g).max(b);
	let min = r.min(g).min(b);
	let d = max - min;
	let s = if max > 0.0 { d / max } else { 0.0 };
	let h = if d <= 0.0 {
		0.0
	} else if max == r {
		((g - b) / d).rem_euclid(6.0) * 60.0
	} else if max == g {
		((b - r) / d + 2.0) * 60.0
	} else {
		((r - g) / d + 4.0) * 60.0
	};
	(h, s, max)
}

/// Convert HSV (hue in degrees) back to an opaque RGB [`Rgba`].
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgba {
	let h = h.rem_euclid(360.0);
	let c = v * s;
	let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
	let m = v - c;
	let (r, g, b) = match (h / 60.0) as u32 {
		0 => (c, x, 0.0),
		1 => (x, c, 0.0),
		2 => (0.0, c, x),
		3 => (0.0, x, c),
		4 => (x, 0.0, c),
		_ => (c, 0.0, x),
	};
	Rgba {
		r: r + m,
		g: g + m,
		b: b + m,
		a: 1.0,
	}
}

/// Map a cursor position to (saturation, value) in 0..1 — x rightwards, y
/// upwards (clamped to the palette bounds).
fn sv_from_point(bounds: Bounds<Pixels>, pos: Point<Pixels>) -> (f32, f32) {
	let width = f32::from(bounds.size.width).max(1.0);
	let height = f32::from(bounds.size.height).max(1.0);
	let s = ((f32::from(pos.x) - f32::from(bounds.left())) / width).clamp(0.0, 1.0);
	let v = 1.0 - ((f32::from(pos.y) - f32::from(bounds.top())) / height).clamp(0.0, 1.0);
	(s, v)
}

/// Inverse of [`sv_from_point`] — the palette position of a (s, v) pair.
// Only the unit tests below call this helper (the lib build sees it as dead
// code), so it is kept with an explicit allow.
#[allow(dead_code)]
fn point_from_sv(bounds: Bounds<Pixels>, s: f32, v: f32) -> Point<Pixels> {
	let width = f32::from(bounds.size.width).max(1.0);
	let height = f32::from(bounds.size.height).max(1.0);
	point(
		bounds.left() + px(s.clamp(0.0, 1.0) * width),
		bounds.top() + px((1.0 - v.clamp(0.0, 1.0)) * height),
	)
}

/// Map a cursor position on the hue bar to a hue in degrees (0..360, top
/// to bottom, clamped).
fn hue_from_point(bounds: Bounds<Pixels>, pos: Point<Pixels>) -> f32 {
	let height = f32::from(bounds.size.height).max(1.0);
	let t = ((f32::from(pos.y) - f32::from(bounds.top())) / height).clamp(0.0, 1.0);
	t * 360.0
}

/// Paint the S/V palette: a 24×24 grid whose base colour is the hue at
/// full value, overlaid per row with a black fade (value 1.0 → 0.0).
fn paint_sv_palette(bounds: Bounds<Pixels>, hue: f32, window: &mut Window, _cx: &mut App) {
	const GRID: u32 = 24;
	let width = f32::from(bounds.size.width);
	let height = f32::from(bounds.size.height);
	let cw = width / GRID as f32;
	let ch = height / GRID as f32;
	for row in 0..GRID {
		for col in 0..GRID {
			let s = col as f32 / (GRID - 1) as f32;
			let color = hsv_to_rgb(hue, s, 1.0);
			let cell = Bounds::new(
				point(
					bounds.left() + px(col as f32 * cw),
					bounds.top() + px(row as f32 * ch),
				),
				size(px(cw + 1.0), px(ch + 1.0)),
			);
			window.paint_quad(fill(cell, Hsla::from(color)));
		}
		// Black overlay fades the row's value from 1.0 down to 0.0.
		let v = 1.0 - row as f32 / (GRID - 1) as f32;
		let overlay = Rgba {
			r: 0.0,
			g: 0.0,
			b: 0.0,
			a: 1.0 - v,
		};
		let bar = Bounds::new(
			point(bounds.left(), bounds.top() + px(row as f32 * ch)),
			size(px(width), px(ch + 1.0)),
		);
		window.paint_quad(fill(bar, Hsla::from(overlay)));
	}
}

/// Paint the hue bar: 36 vertical stripes spanning the full hue circle.
fn paint_hue_bar(bounds: Bounds<Pixels>, window: &mut Window, _cx: &mut App) {
	const STRIPES: u32 = 36;
	let width = f32::from(bounds.size.width);
	let height = f32::from(bounds.size.height);
	let ch = height / STRIPES as f32;
	for i in 0..STRIPES {
		let hue = i as f32 / STRIPES as f32 * 360.0;
		let color = hsv_to_rgb(hue, 1.0, 1.0);
		let cell = Bounds::new(
			point(bounds.left(), bounds.top() + px(i as f32 * ch)),
			size(px(width), px(ch + 1.0)),
		);
		window.paint_quad(fill(cell, Hsla::from(color)));
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use gpui::{Modifiers, Subscription, TestAppContext};
	use std::sync::Mutex;

	#[test]
	fn hex_parsing_and_formatting() {
		// #RRGGBB parses with opaque alpha; #RRGGBBAA keeps the alpha.
		let c = parse_hex("#1A80E6").expect("6-digit hex parses");
		assert!((c.r - 0x1A as f32 / 255.0).abs() < 1e-6);
		assert!((c.g - 0x80 as f32 / 255.0).abs() < 1e-6);
		assert!((c.b - 0xE6 as f32 / 255.0).abs() < 1e-6);
		assert!((c.a - 1.0).abs() < 1e-6);
		let c = parse_hex("#1A80E6FF").expect("8-digit opaque hex parses");
		assert!((c.a - 1.0).abs() < 1e-6);
		let c = parse_hex("#1A80E67F").expect("8-digit alpha hex parses");
		assert!((c.a - 0x7F as f32 / 255.0).abs() < 1e-6);

		// format -> parse round-trips exactly (both sides are 8-bit).
		for hex in ["#102030", "#0A0B0C", "#11223344", "#FF000080"] {
			let parsed = parse_hex(hex).expect("round-trip source parses");
			assert_eq!(format_hex(parsed), hex.to_ascii_uppercase());
		}

		// Opaque colours format to 6 digits, translucent to 8.
		assert_eq!(
			format_hex(Rgba {
				r: 0.0,
				g: 0.0,
				b: 0.0,
				a: 1.0,
			}),
			"#000000"
		);
		assert_eq!(
			format_hex(Rgba {
				r: 1.0,
				g: 1.0,
				b: 1.0,
				a: 0.5,
			}),
			"#FFFFFF80"
		);
	}

	#[test]
	fn hex_parse_rejects_malformed() {
		for bad in [
			"",            // empty
			"102030",      // missing '#'
			"#12345",      // too short
			"#1234567",    // 7 digits
			"#GGHHII",     // non-hex digits
			"#123456789",  // too long
			"# 123456",    // whitespace inside
		] {
			assert!(parse_hex(bad).is_none(), "expected {bad:?} to be rejected");
		}
	}

	/// The colour picker (and its four sliders + hex editor) constructs
	/// without panicking and paints a swatch.
	#[gpui::test]
	async fn color_picker_constructs_without_panicking(cx: &mut TestAppContext) {
		struct Host {
			picker: Entity<OfxColorPicker>,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.picker.clone())
			}
		}

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(200.0)), |window, cx| {
			let picker = cx.new(|cx| {
				OfxColorPicker::new(
					1,
					Rgba {
						r: 0.4,
						g: 0.2,
						b: 0.8,
						a: 0.5,
					},
					window,
					cx,
				)
			});
			Host { picker }
		});
		cx.run_until_parked();

		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});
		assert!(
			visual.debug_bounds("ofx-color-swatch").is_some(),
			"the colour swatch should be painted"
		);
	}

	/// The palette (SV square) mapping works from inside the popup: the
	/// canvas records its layout bounds each frame and the click maps them
	/// to (s, v). Regression test for the bare canvases — without
	/// `.size_full()` the canvas leaf collapses to zero height in the
	/// block layout, the recorded bounds are 0 tall, and a click at the
	/// palette centre maps to v ≈ 0 (black) instead of v ≈ 0.5: the popup
	/// opens but shows/behaves as an empty box (the reported "色板没显示
	/// 出来").
	#[gpui::test]
	async fn palette_click_maps_center_to_mid_saturation_value(cx: &mut TestAppContext) {
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(560.0)), |window, cx| {
			OfxColorPicker::new(
				1,
				Rgba {
					r: 0.4,
					g: 0.2,
					b: 0.8,
					a: 0.5,
				},
				window,
				cx,
			)
		});
		cx.run_until_parked();

		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Open the popup by clicking the swatch, then re-draw so the popup's
		// deferred layer is laid out (and the palette canvas records its
		// bounds for the click mapping).
		let swatch = visual.debug_bounds("ofx-color-swatch").expect("swatch painted");
		let swatch_center = Point::new(
			swatch.origin.x + swatch.size.width * 0.5,
			swatch.origin.y + swatch.size.height * 0.5,
		);
		visual.simulate_click(swatch_center, Modifiers::default());
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Click the centre of the SV palette.
		let sv = visual
			.debug_bounds("ofx-color-sv-palette")
			.expect("sv palette bounds");
		assert!(
			f32::from(sv.size.height) >= 170.0,
			"sv palette height collapsed to {}",
			f32::from(sv.size.height)
		);
		let sv_center = Point::new(
			sv.origin.x + sv.size.width * 0.5,
			sv.origin.y + sv.size.height * 0.5,
		);
		visual.simulate_click(sv_center, Modifiers::default());
		cx.run_until_parked();

		// The centre of the palette maps to ~(0.5, 0.5); the hue bar still
		// shows the initial colour's hue is kept (reading the draft's s/v).
		let picker = window.root(cx).expect("picker root");
		let draft = cx.read(|cx| picker.read(cx).draft);
		let (_, s, v) = rgb_to_hsv(draft.r, draft.g, draft.b);
		assert!(
			(s - 0.5).abs() < 0.05,
			"centre click maps to mid saturation (got {s})"
		);
		assert!(
			(v - 0.5).abs() < 0.05,
			"centre click maps to mid value, not the collapsed-canvas 0 (got {v})"
		);
	}

	/// The "pick from viewer" button toggles the eyedropper and emits
	/// `PickViewerToggle` events that the params view routes to the engine.
	#[gpui::test]
	async fn pick_viewer_button_toggles_armed(cx: &mut TestAppContext) {
		struct Host {
			picker: Entity<OfxColorPicker>,
			events: Arc<Mutex<Vec<OfxColorEvent>>>,
			_subscription: Subscription,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.picker.clone())
			}
		}

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(560.0)), |window, cx| {
			let picker = cx.new(|cx| {
				OfxColorPicker::new(
					1,
					Rgba {
						r: 0.4,
						g: 0.2,
						b: 0.8,
						a: 0.5,
					},
					window,
					cx,
				)
			});
			let events = Arc::new(Mutex::new(Vec::new()));
			let events_sub = events.clone();
			let _subscription = cx.subscribe(&picker, move |_this, _emitter, event: &OfxColorEvent, _cx| {
				events_sub.lock().unwrap().push(*event);
			});
			Host {
				picker,
				events,
				_subscription,
			}
		});
		cx.run_until_parked();

		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Open the popup by clicking the swatch, then re-draw so the popup's
		// deferred layer is laid out.
		let swatch = visual.debug_bounds("ofx-color-swatch").expect("swatch painted");
		let swatch_center = Point::new(
			swatch.origin.x + swatch.size.width * 0.5,
			swatch.origin.y + swatch.size.height * 0.5,
		);
		visual.simulate_click(swatch_center, Modifiers::default());
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Clicking "pick from viewer" arms the eyedropper...
		let button = visual
			.debug_bounds("ofx-color-pick-viewer-1")
			.expect("pick button painted");
		let button_center = Point::new(
			button.origin.x + button.size.width * 0.5,
			button.origin.y + button.size.height * 0.5,
		);
		visual.simulate_click(button_center, Modifiers::default());
		cx.run_until_parked();
		let host = window.root(cx).expect("host root");
		let events = cx.read(|cx| host.read(cx).events.lock().unwrap().clone());
		assert!(
			matches!(events.last(), Some(OfxColorEvent::PickViewerToggle { armed: true })),
			"clicking pick should arm the eyedropper, got {events:?}"
		);

		// ...and a second click disarms it again.
		visual.simulate_click(button_center, Modifiers::default());
		cx.run_until_parked();
		let events = cx.read(|cx| host.read(cx).events.lock().unwrap().clone());
		assert!(
			matches!(events.last(), Some(OfxColorEvent::PickViewerToggle { armed: false })),
			"second click should disarm the eyedropper, got {events:?}"
		);
	}

	/// While the viewer eyedropper is armed, a click outside the popup is a
	/// *pick* on the program viewer, not a dismissal: the popup must survive
	/// it (no `Cancelled` emitted, the arm state kept), or the sampled colour
	/// would be dropped by `apply_viewer_pick`'s `picking` guard. Once the
	/// eyedropper is disarmed, an outside click dismisses as usual.
	#[gpui::test]
	async fn outside_click_while_picking_keeps_popup_open(cx: &mut TestAppContext) {
		struct Host {
			picker: Entity<OfxColorPicker>,
			events: Arc<Mutex<Vec<OfxColorEvent>>>,
			_subscription: Subscription,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.picker.clone())
			}
		}

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(560.0)), |window, cx| {
			let picker = cx.new(|cx| {
				OfxColorPicker::new(
					1,
					Rgba {
						r: 0.4,
						g: 0.2,
						b: 0.8,
						a: 0.5,
					},
					window,
					cx,
				)
			});
			let events = Arc::new(Mutex::new(Vec::new()));
			let events_sub = events.clone();
			let _subscription = cx.subscribe(&picker, move |_this, _emitter, event: &OfxColorEvent, _cx| {
				events_sub.lock().unwrap().push(*event);
			});
			Host {
				picker,
				events,
				_subscription,
			}
		});
		cx.run_until_parked();

		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Open the popup by clicking the swatch, then re-draw so the popup's
		// deferred layer is laid out.
		let swatch = visual.debug_bounds("ofx-color-swatch").expect("swatch painted");
		let swatch_center = Point::new(
			swatch.origin.x + swatch.size.width * 0.5,
			swatch.origin.y + swatch.size.height * 0.5,
		);
		visual.simulate_click(swatch_center, Modifiers::default());
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Arm the eyedropper.
		let button = visual
			.debug_bounds("ofx-color-pick-viewer-1")
			.expect("pick button painted");
		let button_center = Point::new(
			button.origin.x + button.size.width * 0.5,
			button.origin.y + button.size.height * 0.5,
		);
		visual.simulate_click(button_center, Modifiers::default());
		cx.run_until_parked();

		// A click outside the popup (clear of its right/bottom edges, inside
		// the window) while armed: the popup survives, the arm is kept, and no
		// dismissal is emitted.
		let popup = visual
			.debug_bounds("ofx-color-popup")
			.expect("popup painted");
		let outside = Point::new(
			px((f32::from(popup.origin.x) + f32::from(popup.size.width) + 10.0).min(318.0)),
			px((f32::from(popup.origin.y) + f32::from(popup.size.height) + 10.0).min(399.0)),
		);
		visual.simulate_click(outside, Modifiers::default());
		cx.run_until_parked();
		let host = window.root(cx).expect("host root");
		let (open, picking, cancelled) = cx.read(|cx| {
			let host = host.read(cx);
			(
				host.picker.read(cx).open,
				host.picker.read(cx).picking,
				host.events
					.lock()
					.unwrap()
					.iter()
					.any(|e| matches!(e, OfxColorEvent::Cancelled)),
			)
		});
		assert!(open, "the popup must survive an outside click while picking");
		assert!(picking, "the eyedropper stays armed through the pick");
		assert!(!cancelled, "no dismissal may be emitted while picking");

		// Once disarmed (the pick landed / the button toggled again), an
		// outside click dismisses the popup as usual.
		visual.simulate_click(button_center, Modifiers::default());
		cx.run_until_parked();
		visual.simulate_click(outside, Modifiers::default());
		cx.run_until_parked();
		let (open, cancelled) = cx.read(|cx| {
			let host = host.read(cx);
			(
				host.picker.read(cx).open,
				host.events
					.lock()
					.unwrap()
					.iter()
					.any(|e| matches!(e, OfxColorEvent::Cancelled)),
			)
		});
		assert!(!open, "outside click dismisses the popup once disarmed");
		assert!(cancelled, "the dismissal emits Cancelled");
	}

	/// Applying a viewer pick lands the sampled colour in the draft and
	/// disarms the eyedropper.
	#[gpui::test]
	async fn apply_viewer_pick_updates_draft(cx: &mut TestAppContext) {
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(200.0)), |window, cx| {
			OfxColorPicker::new(
				1,
				Rgba {
					r: 0.0,
					g: 0.0,
					b: 0.0,
					a: 1.0,
				},
				window,
				cx,
			)
		});
		cx.run_until_parked();

		let picker = window.root(cx).expect("picker root");
		let red = Rgba {
			r: 1.0,
			g: 0.0,
			b: 0.0,
			a: 1.0,
		};
		cx.update(|cx| {
			picker.update(cx, |picker, cx| {
				picker.toggle_viewer_pick(cx);
				picker.apply_viewer_pick(red, cx);
			});
		});

		let (draft, picking) = cx.read(|cx| {
			let picker = picker.read(cx);
			(picker.draft, picker.picking)
		});
		assert_eq!(draft, red, "viewer pick should land in the draft");
		assert!(!picking, "a viewer pick disarms the eyedropper");
	}

	/// The pick's own click must not close the popup: the sampled colour
	/// lands (disarming the eyedropper) between the button going down on
	/// the viewer and coming up, so the trailing mouse-up arrives with
	/// `picking` already false — without the one-shot swallow it reads as
	/// an outside click, closes the popup and discards the picked colour
	/// (the user can never press OK).
	#[gpui::test]
	async fn pick_click_mouse_up_does_not_dismiss_popup(cx: &mut TestAppContext) {
		struct Host {
			picker: Entity<OfxColorPicker>,
			events: Arc<Mutex<Vec<OfxColorEvent>>>,
			_subscription: Subscription,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.picker.clone())
			}
		}

		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(320.0), px(560.0)), |window, cx| {
			let picker = cx.new(|cx| {
				OfxColorPicker::new(
					1,
					Rgba {
						r: 0.4,
						g: 0.2,
						b: 0.8,
						a: 0.5,
					},
					window,
					cx,
				)
			});
			let events = Arc::new(Mutex::new(Vec::new()));
			let events_sub = events.clone();
			let _subscription = cx.subscribe(&picker, move |_this, _emitter, event: &OfxColorEvent, _cx| {
				events_sub.lock().unwrap().push(*event);
			});
			Host {
				picker,
				events,
				_subscription,
			}
		});
		cx.run_until_parked();

		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		// Open the popup, re-draw for the deferred layer, arm the eyedropper.
		let swatch = visual.debug_bounds("ofx-color-swatch").expect("swatch painted");
		visual.simulate_click(
			Point::new(
				swatch.origin.x + swatch.size.width * 0.5,
				swatch.origin.y + swatch.size.height * 0.5,
			),
			Modifiers::default(),
		);
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});
		let button = visual
			.debug_bounds("ofx-color-pick-viewer-1")
			.expect("pick button painted");
		visual.simulate_click(
			Point::new(
				button.origin.x + button.size.width * 0.5,
				button.origin.y + button.size.height * 0.5,
			),
			Modifiers::default(),
		);
		cx.run_until_parked();

		// The pick lands mid-click (the sampled colour arrives while the
		// button is still down on the viewer), then the button comes up
		// outside the popup.
		let red = Rgba {
			r: 1.0,
			g: 0.0,
			b: 0.0,
			a: 1.0,
		};
		let host = window.root(cx).expect("host root");
		visual.update(|_window, cx| {
			host.update(cx, |host, cx| {
				host.picker.update(cx, |picker, cx| picker.apply_viewer_pick(red, cx));
			});
		});
		let popup = visual
			.debug_bounds("ofx-color-popup")
			.expect("popup painted");
		let outside = Point::new(
			px((f32::from(popup.origin.x) + f32::from(popup.size.width) + 10.0).min(318.0)),
			px((f32::from(popup.origin.y) + f32::from(popup.size.height) + 10.0).min(399.0)),
		);
		visual.simulate_click(outside, Modifiers::default());
		cx.run_until_parked();

		let (open, draft, cancelled) = cx.read(|cx| {
			let host = host.read(cx);
			(
				host.picker.read(cx).open,
				host.picker.read(cx).draft,
				host.events
					.lock()
					.unwrap()
					.iter()
					.any(|e| matches!(e, OfxColorEvent::Cancelled)),
			)
		});
		assert!(open, "the pick's own mouse-up must not dismiss the popup");
		assert_eq!(draft, red, "the picked colour survives in the draft");
		assert!(!cancelled, "no dismissal may be emitted for the pick click");

		// A later genuine outside click still dismisses as usual.
		visual.simulate_click(outside, Modifiers::default());
		cx.run_until_parked();
		let (open, cancelled) = cx.read(|cx| {
			let host = host.read(cx);
			(
				host.picker.read(cx).open,
				host.events
					.lock()
					.unwrap()
					.iter()
					.any(|e| matches!(e, OfxColorEvent::Cancelled)),
			)
		});
		assert!(!open, "a later outside click dismisses the popup");
		assert!(cancelled, "the later dismissal emits Cancelled");
	}

	/// Known-value checks + round-trips for the RGB↔HSV conversion.
	#[test]
	fn hsv_rgb_known_values() {
		let hsv = |c: Rgba| rgb_to_hsv(c.r, c.g, c.b);
		let close = |a: f32, b: f32| (a - b).abs() < 1e-4;

		// The six canonical corners of the RGB cube.
		assert_eq!(hsv(Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }), (0.0, 1.0, 1.0));
		assert_eq!(hsv(Rgba { r: 0.0, g: 1.0, b: 0.0, a: 1.0 }), (120.0, 1.0, 1.0));
		assert_eq!(hsv(Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }), (240.0, 1.0, 1.0));
		assert_eq!(hsv(Rgba { r: 1.0, g: 1.0, b: 0.0, a: 1.0 }), (60.0, 1.0, 1.0));
		assert_eq!(hsv(Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }), (0.0, 0.0, 1.0));
		assert_eq!(hsv(Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }), (0.0, 0.0, 0.0));

		// The same corners back to RGB.
		let rgb = |h: f32, s: f32, v: f32| hsv_to_rgb(h, s, v);
		assert_eq!(rgb(0.0, 1.0, 1.0), Rgba { r: 1.0, g: 0.0, b: 0.0, a: 1.0 });
		assert_eq!(rgb(120.0, 1.0, 1.0), Rgba { r: 0.0, g: 1.0, b: 0.0, a: 1.0 });
		assert_eq!(rgb(240.0, 1.0, 1.0), Rgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0 });

		// Arbitrary RGB round-trips.
		for (r, g, b) in [(0.5, 0.25, 0.75), (0.1, 0.9, 0.2), (0.33, 0.67, 0.44)] {
			let (h, s, v) = rgb_to_hsv(r, g, b);
			let back = hsv_to_rgb(h, s, v);
			assert!(
				close(back.r, r) && close(back.g, g) && close(back.b, b),
				"RGB round-trip failed for ({r}, {g}, {b}) -> {back:?}"
			);
		}

		// Arbitrary HSV round-trips (the hue may land on the other side of
		// 0° / 360°, hence the looser tolerance).
		for (h, s, v) in [(30.0, 0.5, 0.5), (200.0, 0.3, 0.8), (345.0, 1.0, 0.2)] {
			let rgb = hsv_to_rgb(h, s, v);
			let (h2, s2, v2) = rgb_to_hsv(rgb.r, rgb.g, rgb.b);
			assert!(
				(h2 - h).abs() < 1e-3 && (s2 - s).abs() < 1e-4 && (v2 - v).abs() < 1e-4,
				"HSV round-trip failed for ({h}, {s}, {v}) -> ({h2}, {s2}, {v2})"
			);
		}
	}

	/// The palette ↔ cursor mapping: corners, centre and out-of-bounds
	/// clamping.
	#[test]
	fn sv_point_mapping() {
		let bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(50.0)));
		let at = |x: f32, y: f32| point(px(x), px(y));

		assert_eq!(sv_from_point(bounds, at(10.0, 20.0)), (0.0, 1.0));
		assert_eq!(sv_from_point(bounds, at(110.0, 70.0)), (1.0, 0.0));
		assert_eq!(sv_from_point(bounds, at(60.0, 45.0)), (0.5, 0.5));
		// Out of bounds clamps to the edges.
		assert_eq!(sv_from_point(bounds, at(0.0, 0.0)), (0.0, 1.0));
		assert_eq!(sv_from_point(bounds, at(500.0, 500.0)), (1.0, 0.0));

		assert_eq!(point_from_sv(bounds, 0.0, 1.0), at(10.0, 20.0));
		assert_eq!(point_from_sv(bounds, 1.0, 0.0), at(110.0, 70.0));
		assert_eq!(point_from_sv(bounds, 0.5, 0.5), at(60.0, 45.0));
		assert_eq!(point_from_sv(bounds, -1.0, 2.0), at(10.0, 20.0));
	}
	/// A text field the user is editing is never re-synced mid-edit: the
	/// params view re-renders on every engine change, and reapplying the
	/// engine snapshot would wipe the in-progress text (the "cannot type
	/// into the text field" report). On blur the field re-syncs to the
	/// engine value.
	#[gpui::test]
	async fn text_field_keeps_in_progress_edits_while_focused(cx: &mut TestAppContext) {
		use crate::oakui::mock::MockEngine;
		struct Host {
			view: Entity<OfxParamsView<MockEngine>>,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.view.clone())
			}
		}
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(400.0), px(200.0)), |window, cx| {
			let engine = cx.new(|cx| MockEngine::create(cx));
			let view = cx.new(|cx| OfxParamsView::<MockEngine>::new(EffectId(900), engine, window, cx));
			Host { view }
		});
		cx.run_until_parked();
		let host = window.root(cx).expect("host root");
		let view = cx.read(|cx| host.read(cx).view.clone());
		// Two inputs are text fields: the single-line `args_in` and the
		// multi-line `plain_text_in`. This test types into the latter.
		let editor = cx.read(|cx| {
			view.read(cx)
				.controls
				.iter()
				.find(|c| c.input_id == "plain_text_in")
				.map(|c| match &c.kind {
					ControlKind::Text { editor, .. } => editor.clone(),
					_ => panic!("plain_text_in should build a text control"),
				})
				.expect("the plain_text_in control")
		});
		assert_eq!(
			cx.read(|cx| editor.read(cx).as_str().to_string()),
			"文本\nsecond line"
		);

		// Focus the field and type: the in-progress text must survive the
		// engine-notify re-sync (the tick-rate repaint).
		window
			.update(cx, |_root, window, cx| {
				let handle = editor.read(cx).focus_handle(cx);
				window.focus(&handle, cx);
			})
			.unwrap();
		cx.update(|cx| {
			editor.update(cx, |editor, cx| editor.emplace("user typed", cx));
		});
		cx.update(|cx| {
			view.update(cx, |view, cx| {
				let engine = view.engine.clone();
				engine.update(cx, |_engine, cx| cx.notify());
			});
		});
		cx.run_until_parked();
		assert_eq!(
			cx.read(|cx| editor.read(cx).as_str().to_string()),
			"user typed",
			"the in-progress text survives the re-sync while focused"
		);

		// On blur the field re-syncs to the engine value.
		window
			.update(cx, |_root, window, _cx| {
				window.blur();
			})
			.unwrap();
		cx.update(|cx| {
			view.update(cx, |view, cx| {
				let engine = view.engine.clone();
				engine.update(cx, |_engine, cx| cx.notify());
			});
		});
		cx.run_until_parked();
		assert_eq!(
			cx.read(|cx| editor.read(cx).as_str().to_string()),
			"文本\nsecond line",
			"the field re-syncs to the engine value on blur"
		);
	}

	/// The v3 text node's parameter set renders as one structured control per
	/// visible input — a multi-line text field, a font-family combo, colour
	/// pickers, sliders, checkboxes, vec2 spinboxes — and the three hidden
	/// inputs (the legacy text, the vertical align, the use-args toggle)
	/// render nothing at all.
	#[gpui::test]
	async fn text3_params_render_as_structured_controls(cx: &mut TestAppContext) {
		use crate::oakui::mock::MockEngine;
		struct Host {
			view: Entity<OfxParamsView<MockEngine>>,
		}
		impl Render for Host {
			fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
				div().size_full().child(self.view.clone())
			}
		}
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(size(px(400.0), px(600.0)), |window, cx| {
			let engine = cx.new(|cx| MockEngine::create(cx));
			let view = cx.new(|cx| OfxParamsView::<MockEngine>::new(EffectId(900), engine, window, cx));
			Host { view }
		});
		cx.run_until_parked();
		let host = window.root(cx).expect("host root");
		let view = cx.read(|cx| host.read(cx).view.clone());

		// The mock hands the hidden inputs over (a plugin engine's snapshot
		// can carry them too); the view must drop them before any control is
		// built.
		let raw_ids: Vec<String> = cx.read(|cx| {
			view.read(cx)
				.engine
				.read(cx)
				.effect_params(EffectId(900))
				.expect("the mock carries the text3 parameter set")
				.into_iter()
				.map(|p| p.input_id)
				.collect()
		});
		for hidden in ["text_in", "valign_in", "use_args_in"] {
			assert!(
				raw_ids.iter().any(|id| id == hidden),
				"the mock snapshot carries {hidden} ({raw_ids:?})"
			);
		}

		// Draw once: the render pass is where `sync_values` reapplies the
		// engine snapshot to the widgets (the sliders snap to their grid).
		let visual = gpui::VisualTestContext::from_window(window.into(), cx).into_mut();
		visual.update(|window, cx| {
			window.draw(cx).clear();
		});

		/// One plain-data snapshot of the control set (the kinds carry
		/// entities, so they cannot be compared directly).
		struct Snapshot {
			kinds: Vec<(String, &'static str)>,
			sliders: Vec<(String, f64)>,
			texts: Vec<(String, String, bool)>,
			combos: Vec<(String, Option<usize>)>,
			spins: Vec<(String, Vec<f64>)>,
			colors: Vec<String>,
		}
		let snap = visual.read(|cx| {
			let view = view.read(cx);
			let mut snap = Snapshot {
				kinds: Vec::new(),
				sliders: Vec::new(),
				texts: Vec::new(),
				combos: Vec::new(),
				spins: Vec::new(),
				colors: Vec::new(),
			};
			for control in &view.controls {
				let kind = match &control.kind {
					ControlKind::Slider(slider) => {
						snap.sliders
							.push((control.input_id.clone(), slider.read(cx).value().to_f64()));
						"slider"
					}
					ControlKind::CheckBox(_) => "checkbox",
					ControlKind::Combo(combo) => {
						snap.combos.push((control.input_id.clone(), combo.read(cx).selected()));
						"combo"
					}
					ControlKind::Spin(spins) => {
						snap.spins.push((
							control.input_id.clone(),
							spins.iter().map(|(spin, _)| spin.read(cx).value().to_f64()).collect(),
						));
						"spin"
					}
					ControlKind::Color(_) => {
						snap.colors.push(control.input_id.clone());
						"color"
					}
					ControlKind::Text { editor, multiline } => {
						snap.texts.push((
							control.input_id.clone(),
							editor.read(cx).as_str().to_string(),
							*multiline,
						));
						if *multiline {
							"text-area"
						} else {
							"text"
						}
					}
					ControlKind::Curve(_) => "curve",
					ControlKind::PushButton => "button",
					ControlKind::ReadOnly(_) => "readonly",
				};
				snap.kinds.push((control.input_id.clone(), kind));
			}
			snap
		});

		let kinds: Vec<(&str, &str)> = snap
			.kinds
			.iter()
			.map(|(id, kind)| (id.as_str(), *kind))
			.collect();
		assert_eq!(
			kinds,
			vec![
				("pos_in", "spin"),
				("size_in", "spin"),
				("plain_text_in", "text-area"),
				("font_family_in", "combo"),
				("font_size_in", "slider"),
				("outline_enabled_in", "checkbox"),
				("outline_color_in", "color"),
				("outline_width_in", "slider"),
				("glow_enabled_in", "checkbox"),
				("glow_color_in", "color"),
				("glow_radius_in", "slider"),
				("args_in", "text"),
			],
			"one control per visible text3 input, hidden inputs and secret textures skipped"
		);

		// Each numeric widget shows the engine value (not a grid neighbour).
		let slider_values: Vec<(&str, f64)> = snap
			.sliders
			.iter()
			.map(|(id, value)| (id.as_str(), *value))
			.collect();
		for (id, want) in [
			("font_size_in", 72.0),
			("outline_width_in", 2.0),
			("glow_radius_in", 8.0),
		] {
			let got = slider_values
				.iter()
				.find(|(input_id, _)| *input_id == id)
				.unwrap_or_else(|| panic!("{id} should be a slider: {slider_values:?}"));
			assert!(
				(got.1 - want).abs() <= 1e-9 * want.abs().max(1.0),
				"{id} shows {} but the engine holds {want}",
				got.1
			);
		}

		assert_eq!(
			snap.texts
				.iter()
				.map(|(id, text, multiline)| (id.as_str(), text.as_str(), *multiline))
				.collect::<Vec<_>>(),
			vec![
				("plain_text_in", "文本\nsecond line", true),
				("args_in", "", false),
			],
			"the editable text is multi-line and holds the node's text verbatim"
		);
		assert_eq!(
			snap.combos,
			vec![("font_family_in".to_string(), Some(0))],
			"the font family is a (string) combo"
		);
		assert_eq!(
			snap.colors,
			vec!["outline_color_in".to_string(), "glow_color_in".to_string()],
			"both colours get a picker"
		);
		assert_eq!(
			snap.spins,
			vec![
				("pos_in".to_string(), vec![0.0, 0.0]),
				("size_in".to_string(), vec![400.0, 300.0]),
			],
			"the vec2 params get one spinbox per component"
		);
	}

	/// Every numeric parameter seeds its slider on the step grid, inside the
	/// range, and a re-sync does not move it: `font_size_in` (72 with a floor
	/// of 1) used to show 50.995, because the range came from the type
	/// default and the value snapped onto a coarse grid nowhere near it.
	#[test]
	fn numeric_params_stay_on_their_slider_grid() {
		fn param(
			value_type: ValueType,
			value: NodeValue,
			properties: Vec<(&str, NodeValue)>,
		) -> EffectParam {
			EffectParam {
				input_id: "test_in".to_string(),
				display_name: "Test".to_string(),
				value_type,
				value,
				flags: 0,
				properties: properties
					.into_iter()
					.map(|(k, v)| (k.to_string(), v))
					.collect(),
			}
		}
		fn close(got: f64, want: f64) -> bool {
			(got - want).abs() <= 1e-9 * want.abs().max(1.0)
		}

		struct Case {
			label: &'static str,
			param: EffectParam,
			want_value: f64,
			want_range: (f64, f64),
		}
		let cases = vec![
			Case {
				label: "font size 72 over a floor of 1",
				param: param(
					ValueType::Float,
					NodeValue::Float(72.0),
					vec![("min", NodeValue::Float(1.0))],
				),
				want_value: 72.0,
				want_range: (1.0, 201.0),
			},
			Case {
				label: "outline width 2 over a floor of 0",
				param: param(
					ValueType::Float,
					NodeValue::Float(2.0),
					vec![("min", NodeValue::Float(0.0))],
				),
				want_value: 2.0,
				want_range: (0.0, 20.0),
			},
			Case {
				label: "glow radius 8 over a floor of 0",
				param: param(
					ValueType::Float,
					NodeValue::Float(8.0),
					vec![("min", NodeValue::Float(0.0))],
				),
				want_value: 8.0,
				want_range: (0.0, 100.0),
			},
			Case {
				label: "a value sitting on its floor",
				param: param(
					ValueType::Float,
					NodeValue::Float(1.0),
					vec![("min", NodeValue::Float(1.0))],
				),
				want_value: 1.0,
				want_range: (1.0, 201.0),
			},
			Case {
				label: "a bounded 0..1 float",
				param: param(
					ValueType::Float,
					NodeValue::Float(0.5),
					vec![("min", NodeValue::Float(0.0)), ("max", NodeValue::Float(1.0))],
				),
				want_value: 0.5,
				want_range: (0.0, 1.0),
			},
			Case {
				label: "an unbounded float (the wide default range)",
				param: param(ValueType::Float, NodeValue::Float(0.7234), Vec::new()),
				want_value: 0.7234,
				want_range: (-10000.0, 10000.0),
			},
			Case {
				label: "an int over 0..10",
				param: param(
					ValueType::Int,
					NodeValue::Int(5),
					vec![("min", NodeValue::Int(0)), ("max", NodeValue::Int(10))],
				),
				want_value: 5.0,
				want_range: (0.0, 10.0),
			},
		];

		for case in cases {
			let label = case.label;
			let (min, max, step) = slider_range_and_step(&case.param);
			assert!(
				min.is_finite() && max.is_finite() && step.is_finite() && step > 0.0,
				"{label}: bad slider geometry ({min}, {max}) step {step}"
			);
			assert_eq!((min, max), case.want_range, "{label}: the slider range");
			let kind = if case.param.value_type == ValueType::Int {
				ValueKind::Integer
			} else {
				ValueKind::Float
			};
			let engine_value = case.param.value.to_double();
			let mut model = SliderModel::new(kind, min, max, step, engine_value.clamp(min, max));
			assert!(
				close(model.raw, case.want_value),
				"{label}: seeded at {} instead of {}",
				model.raw,
				case.want_value
			);
			// `sync_values` re-applies the engine value on every render: the
			// snapshot must land back on the same position.
			model.set_value(SliderValue::Float(engine_value));
			assert!(
				close(model.raw, case.want_value),
				"{label}: a re-sync moved the value to {} (want {})",
				model.raw,
				case.want_value
			);
			assert!(
				min <= model.raw && model.raw <= max,
				"{label}: {} is outside {min}..{max}",
				model.raw
			);
		}
	}
}

