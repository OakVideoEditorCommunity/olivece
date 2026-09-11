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

//! OfxParametricParameterSuite v1：参数化曲线参数（调色插件的
//! 曲线/LUT 类参数）。
//!
//! 语义对照 ofxParametricParam.h（属性表另见 HS ofxhParam.cpp:285-330）：
//! - 曲线 = 每维一条 [`crate::param_curve::Curve`]，存于参数值
//!   [`crate::param::ParamValue::Parametric`]（def 默认 / instance 当前
//!   值，`ParamInstance::from_def` 复制默认）；
//! - 维度与定义域取参数属性 [`crate::param::P_PARAMETRIC_DIMENSION`]
//!   （int，默认 1）与 [`crate::param::P_PARAMETRIC_RANGE`]
//!   （double×2，默认 (0,1)）——paramDefine 预置，插件 describe 期可改；
//! - 求值 = 三次 Hermite（slope 自动差分），越界 parametricPosition
//!   钳制到端点值，见 [`crate::param_curve::Curve::evaluate`]；
//! - describe 期（Def）的 Set/Add/Delete 改**默认值**（HS："If a
//!   plugin wishes to set a different default value for a curve, it can
//!   use the suite to set key/value pairs on the descriptor. When a new
//!   instance is made, it will have these curve values as a default"）；
//! - 第 1 期无参数动画（宿主
//!   `kOfxParamHostPropSupportsParametricAnimation = 0`）：`time` 恒
//!   忽略（曲线是实例级静态的），`addAnimationKey` 同理忽略；
//! - 未配置的曲线（维度内但从未写入）按恒等默认求值/读数——与
//!   "default default 是恒等查找" 一致；任何写入路径先把缺失曲线
//!   补成恒等默认再落库；
//! - 错误码：坏句柄 → BadHandle，非 parametric 参数 → BadHandle，
//!   curveIndex / nth 越界 → BadIndex（头文件 GetNth/Set/Add 文档
//!   里写的 Unknown 是类型未知的泛指，本实现按任务约定统一
//!   BadHandle/BadIndex）。

use std::borrow::Cow;
use std::ffi::{c_double, c_int, c_void};

use crate::param::{ParamDef, ParamInstance, ParamValue};
use crate::param_curve::Curve;
use crate::property::{PropertySet, Value};
use crate::suites::{status, tag};

/// 函数表布局（与 SDK `OfxParametricParameterSuiteV1` 一致；字段序
/// 以 ofxParametricParam.h 为准）。
#[repr(C)]
pub struct ParametricParameterSuiteV1 {
	/// parametricParamGetValue：求值曲线（越界位置钳制到端点值）。
	pub parametric_param_get_value: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_double,
		*mut c_double,
	) -> c_int,
	/// parametricParamGetNControlPoints：控制点数。
	pub parametric_param_get_n_control_points:
		unsafe extern "C" fn(*mut c_void, c_int, c_double, *mut c_int) -> c_int,
	/// parametricParamGetNthControlPoint：第 nth 个控制点的 key/value。
	pub parametric_param_get_nth_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_int,
		*mut c_double,
		*mut c_double,
	) -> c_int,
	/// parametricParamSetNthControlPoint：改第 nth 个控制点
	/// （key 变化后保持有序，slope 重算）。
	pub parametric_param_set_nth_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_int,
		c_double,
		c_double,
		c_int,
	) -> c_int,
	/// parametricParamAddControlPoint：加入/覆盖控制点（保持有序）。
	pub parametric_param_add_control_point: unsafe extern "C" fn(
		*mut c_void,
		c_int,
		c_double,
		c_double,
		c_double,
		c_int,
	) -> c_int,
	/// parametricParamDeleteControlPoint：删第 nth 个控制点。
	pub parametric_param_delete_control_point:
		unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
	/// parametricParamDeleteAllControlPoints：清空某维曲线。
	pub parametric_param_delete_all_control_points:
		unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
}

// ---- 句柄解析 -----------------------------------------------------------

/// param 句柄：describe 期是定义（值=默认值），实例期是实例。
/// Def 持 `&mut`（Set/Add/Delete 直改默认值）；Instance 持 `&`
/// （经 [`ParamInstance::set_ofx`] 的内部 Mutex 改当前值）。
enum ParamRef<'a> {
	Def(&'a mut ParamDef),
	Instance(&'a ParamInstance),
}

/// 解析 param 句柄。空指针/标签不符 → BadHandle。
fn resolve_param(handle: *mut c_void) -> Result<ParamRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::PARAM_DEF => Ok(ParamRef::Def(&mut *(tag::strip(handle) as *mut ParamDef))),
			tag::PARAM_INSTANCE => Ok(ParamRef::Instance(&*(
				tag::strip(handle) as *const ParamInstance
			))),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// 公共入口模板：panic 兜底。
#[track_caller]
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if code != status::OK && std::env::var_os("OAK_OFX_TRACE").is_some() {
		eprintln!("[ofx] parametric suite error {code} at {caller}");
	}
	code
}

// ---- 模型存取 -----------------------------------------------------------

/// 取参数曲线（Def=默认值，Instance=当前值）。值不是 parametric 曲线
/// → BadHandle（非 parametric 参数的 suite 语义；先于索引检查，保证
/// 非 parametric 恒为 BadHandle）。
fn curves_of(r: &ParamRef) -> Result<Vec<Curve>, c_int> {
	match r {
		ParamRef::Def(d) => match &d.default {
			ParamValue::Parametric(c) => Ok(c.clone()),
			_ => Err(status::ERR_BAD_HANDLE),
		},
		ParamRef::Instance(p) => match p.get() {
			ParamValue::Parametric(c) => Ok(c),
			_ => Err(status::ERR_BAD_HANDLE),
		},
	}
}

/// 写回曲线：Def → 默认值；Instance → 当前值 + instanceChanged 通知
/// （复用 param suite 的 [`crate::suites::param::notify_changed`]——
/// 曲线经 [`crate::param::notify_instance_changed`] 序列化为 JSON
/// 文本写回节点输入（undoable，同标量路径），保持宿主侧变更感知的
/// 纪律一致）。
fn write_curves(r: &mut ParamRef, curves: Vec<Curve>) {
	match r {
		ParamRef::Def(d) => d.default = ParamValue::Parametric(curves),
		ParamRef::Instance(p) => {
			let name = p.def.name.clone();
			let addr = &p.props as *const _ as usize;
			p.set_ofx(ParamValue::Parametric(curves));
			crate::suites::param::notify_changed(addr, &name);
		}
	}
}

/// param 的属性集（句柄即 props 地址，偏移 0 句柄约定）。
fn props_of(handle: *mut c_void) -> &'static PropertySet {
	unsafe { &*tag::strip(handle) }
}

/// 维度：`OfxParamPropParametricDimension`（int，默认 1；≤0 防御性
/// 按 1 处理——头文件要求"greater than 0"）。
fn dimension_of(handle: *mut c_void) -> usize {
	match props_of(handle).get(crate::param::P_PARAMETRIC_DIMENSION, 0) {
		Some(Value::Int(d)) => d.max(1) as usize,
		_ => 1,
	}
}

/// 定义域：`OfxParamPropParametricRange`（double×2，默认 (0,1)）——
/// 未配置曲线补恒等默认时的端点。
fn range_of(handle: *mut c_void) -> (f64, f64) {
	let props = props_of(handle);
	let lo = match props.get(crate::param::P_PARAMETRIC_RANGE, 0) {
		Some(Value::Double(d)) => d,
		_ => 0.0,
	};
	let hi = match props.get(crate::param::P_PARAMETRIC_RANGE, 1) {
		Some(Value::Double(d)) => d,
		_ => 1.0,
	};
	(lo, hi)
}

/// curveIndex 越界（相对 dimension 属性）→ BadIndex。
fn check_curve_index(handle: *mut c_void, curve_index: c_int) -> Result<usize, c_int> {
	let ci = usize::try_from(curve_index).map_err(|_| status::ERR_BAD_INDEX)?;
	if ci >= dimension_of(handle) {
		return Err(status::ERR_BAD_INDEX);
	}
	Ok(ci)
}

/// 取第 `ci` 条曲线；缺失（维内但从未写入）→ 恒等默认（只读路径
/// 不落库：未配置曲线 = 恒等查找）。
fn curve_at<'a>(curves: &'a [Curve], ci: usize, range: (f64, f64)) -> Cow<'a, Curve> {
	match curves.get(ci) {
		Some(c) => Cow::Borrowed(c),
		None => Cow::Owned(Curve::identity(range.0, range.1)),
	}
}

/// 确保第 `ci` 条曲线存在（写入路径：缺失时补恒等默认再落库）。
fn ensure_curve(curves: &mut Vec<Curve>, ci: usize, range: (f64, f64)) -> &mut Curve {
	while curves.len() <= ci {
		curves.push(Curve::identity(range.0, range.1));
	}
	&mut curves[ci]
}

// ---- suite 函数 ---------------------------------------------------------

/// parametricParamGetValue：求值曲线。`time` 忽略（无参数动画）；
/// 越界 parametricPosition 钳制到端点值（[`Curve::evaluate`]）。
unsafe extern "C" fn parametric_param_get_value(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	parametric_position: c_double,
	out: *mut c_double,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *out = 0.0 };
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let range = range_of(param);
		let v = curve_at(&curves, ci, range).evaluate(parametric_position);
		unsafe { *out = v };
		Ok(())
	})
}

/// parametricParamGetNControlPoints：控制点数（未配置曲线 = 恒等默认
/// 的 2 点）。
unsafe extern "C" fn parametric_param_get_n_control_points(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	out: *mut c_int,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *out = 0 };
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let n = curve_at(&curves, ci, range_of(param)).len();
		unsafe { *out = n as c_int };
		Ok(())
	})
}

/// parametricParamGetNthControlPoint：第 nth 个控制点的 key/value
/// （nth 越界 → BadIndex；未配置曲线按恒等默认读数）。
unsafe extern "C" fn parametric_param_get_nth_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	nth: c_int,
	key: *mut c_double,
	value: *mut c_double,
) -> c_int {
	caught(|| {
		if key.is_null() || value.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe {
			*key = 0.0;
			*value = 0.0;
		};
		let r = resolve_param(param)?;
		let curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		let c = curve_at(&curves, ci, range_of(param));
		let p = c.nth(nth).ok_or(status::ERR_BAD_INDEX)?;
		unsafe {
			*key = p.key;
			*value = p.value;
		};
		Ok(())
	})
}

/// parametricParamSetNthControlPoint：改第 nth 个控制点为 (key,
/// value)。key 变化可能破坏有序性（头文件明确提醒）——模型侧先移除
/// 再按 key 插入、撞 key 按覆盖处理、slope 重算（[`Curve::set_nth`]）。
/// `addAnimationKey` 忽略（宿主不支持参数动画）。
unsafe extern "C" fn parametric_param_set_nth_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	nth: c_int,
	key: c_double,
	value: c_double,
	_add_animation_key: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		ensure_curve(&mut curves, ci, range_of(param))
			.set_nth(nth, key, value)
			.map_err(|()| status::ERR_BAD_INDEX)?;
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// parametricParamAddControlPoint：加入控制点；同 key 已存在 → 覆盖
/// （头文件 "If a key exists sufficiently close to 'key', then it will
/// be set to the indicated control point"；本实现取精确同 key）。
/// `addAnimationKey` 忽略（无参数动画）。
unsafe extern "C" fn parametric_param_add_control_point(
	param: *mut c_void,
	curve_index: c_int,
	_time: c_double,
	key: c_double,
	value: c_double,
	_add_animation_key: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		ensure_curve(&mut curves, ci, range_of(param)).upsert(key, value);
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// parametricParamDeleteControlPoint：删第 nth 个控制点（越界 →
/// BadIndex）。
unsafe extern "C" fn parametric_param_delete_control_point(
	param: *mut c_void,
	curve_index: c_int,
	nth: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		let nth = usize::try_from(nth).map_err(|_| status::ERR_BAD_INDEX)?;
		ensure_curve(&mut curves, ci, range_of(param))
			.delete_nth(nth)
			.map_err(|()| status::ERR_BAD_INDEX)?;
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// parametricParamDeleteAllControlPoints：清空某维曲线（清空后求值
/// 退化为恒等，[`Curve::evaluate`] 的空曲线分支）。
unsafe extern "C" fn parametric_param_delete_all_control_points(
	param: *mut c_void,
	curve_index: c_int,
) -> c_int {
	caught(|| {
		let mut r = resolve_param(param)?;
		let mut curves = curves_of(&r)?;
		let ci = check_curve_index(param, curve_index)?;
		ensure_curve(&mut curves, ci, range_of(param)).clear();
		write_curves(&mut r, curves);
		Ok(())
	})
}

/// 静态函数表实例。
pub fn suite_v1() -> &'static ParametricParameterSuiteV1 {
	static SUITE: std::sync::OnceLock<ParametricParameterSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ParametricParameterSuiteV1 {
		parametric_param_get_value: parametric_param_get_value,
		parametric_param_get_n_control_points: parametric_param_get_n_control_points,
		parametric_param_get_nth_control_point: parametric_param_get_nth_control_point,
		parametric_param_set_nth_control_point: parametric_param_set_nth_control_point,
		parametric_param_add_control_point: parametric_param_add_control_point,
		parametric_param_delete_control_point: parametric_param_delete_control_point,
		parametric_param_delete_all_control_points: parametric_param_delete_all_control_points,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::{c_char, CString};
	use std::sync::Arc;

	use crate::descriptor::EffectDescriptor;
	use crate::host::Plugin;
	use crate::instance::Instance;
	use crate::param::{ParamInstance, ParamSetInstance};
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(&d.props as *const PropertySet, tag::DESCRIPTOR)
	}

	/// 假插件（host::Plugin 的构造只为拿 Arc 喂给 Instance；describe
	/// 之外的字段不被本测试触碰）。
	fn dummy_plugin(descriptor: EffectDescriptor) -> Arc<Plugin> {
		unsafe extern "C" fn dummy_entry(
			_: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> c_int {
			status::OK
		}
		Arc::new(Plugin {
			identifier: "test.plugin".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor,
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		})
	}

	fn instance_handle(i: &Instance) -> *mut c_void {
		tag::make(&i.props as *const PropertySet, tag::INSTANCE)
	}

	/// describe 期建一个 parametric 参数 "curve"（默认维度 1），返回
	/// 描述符与其 param 句柄。
	fn define_parametric() -> (EffectDescriptor, *mut c_void) {
		let desc = EffectDescriptor::new();
		let s = crate::suites::param::suite_v1();
		let dhandle = descriptor_handle(&desc);
		let t = cs(crate::param::TYPE_PARAMETRIC);
		let n = cs("curve");
		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph), 0);
		}
		assert_eq!(tag::kind(ph), tag::PARAM_DEF);
		(desc, ph)
	}

	/// 把 describe 产物实例化（default → 实例值复制），返回
	/// (实例 Arc, 实例 handle, 实例 param 句柄)。
	fn instantiate(desc: EffectDescriptor) -> (Arc<Instance>, *mut c_void, *mut c_void) {
		let params = ParamSetInstance {
			params: desc
				.params
				.iter()
				.map(|d| Box::new(ParamInstance::from_def((**d).clone())))
				.collect(),
		};
		let plugin = dummy_plugin(desc);
		let inst = Arc::new(Instance {
			props: PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: vec![],
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		});
		let ih = instance_handle(&inst);
		let s = crate::suites::param::suite_v1();
		let n = cs("curve");
		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_get_handle)(ih, n.as_ptr(), &mut ph, std::ptr::null_mut()), 0);
		}
		assert_eq!(tag::kind(ph), tag::PARAM_INSTANCE);
		(inst, ih, ph)
	}

	/// describe 定义 + 实例化一步到位（默认维度 1）。
	fn make_instance() -> (Arc<Instance>, *mut c_void, *mut c_void) {
		let (desc, _dph) = define_parametric();
		instantiate(desc)
	}

	fn ps() -> &'static ParametricParameterSuiteV1 {
		suite_v1()
	}

	/// describe 期默认值：2 个恒等控制点 (0,0)/(1,1)，求值 = 恒等。
	#[test]
	fn describe_defaults_are_identity() {
		let (_desc, ph) = define_parametric();
		let s = ps();
		let mut n = -1;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 0, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.0, 0.0));
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (1.0, 1.0));
		// 恒等求值（越界钳制端点值）。
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
		}
		assert_eq!(out, 0.5);
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, -3.0, &mut out), 0);
			assert_eq!(out, 0.0);
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 7.0, &mut out), 0);
			assert_eq!(out, 1.0);
		}
	}

	/// describe 期 Set/Add/Delete 改**默认值**，实例化时复制到实例。
	#[test]
	fn descriptor_edits_become_instance_defaults() {
		let (desc, ph) = define_parametric();
		let s = ps();
		// describe 期改默认：加两个点，改第 1 个点的值。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.25, 0.5, 0), 0);
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.75, 0.9, 0), 0);
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.25, 0.6, 0), 0);
		}
		// 默认值本身生效。
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.6);
		// 实例化：默认曲线随 from_def 复制。
		let (_inst, _ih, iph) = instantiate(desc);
		unsafe {
			assert_eq!((s.parametric_param_get_value)(iph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.6);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(iph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 4); // (0,0) (0.25,0.6) (0.75,0.9) (1,1)
	}

	/// 实例期全链路：Add → Set → Delete → DeleteAll，求值随模型变化。
	#[test]
	fn instance_add_set_delete_eval_chain() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();

		// Add 中点 (0.3, 0.7)：插值性质 + 有序。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.3, 0.7, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.3, &mut out), 0);
		}
		assert_eq!(out, 0.7);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3);

		// SetNth 改 (0.3,0.7) → (0.4,0.6)。
		unsafe {
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.4, 0.6, 0), 0);
		}
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.4, 0.6));
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.4, &mut out), 0);
		}
		assert_eq!(out, 0.6);

		// Delete 第 1 点 → 回到双点。
		unsafe {
			assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
		}
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);

		// DeleteAll → 0 点，求值退化为恒等。
		unsafe {
			assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.5, &mut out), 0);
		}
		assert_eq!(n, 0);
		assert_eq!(out, 0.5);
	}

	/// SetNth 改 key 破坏有序性 → 自动重排（头文件提醒的语义）。
	#[test]
	fn set_nth_reorders_on_key_change() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.5, 0), 0);
			// 把第 0 点 (0,0) 挪到 0.75 → 序列变 (0.5) (0.75) (1)。
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 0, 0.75, 0.75, 0), 0);
		}
		let (mut k, mut v) = (-1.0, -1.0);
		for (i, (ek, ev)) in [(0.5, 0.5), (0.75, 0.75), (1.0, 1.0)].iter().enumerate() {
			unsafe {
				assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, i as c_int, &mut k, &mut v), 0);
			}
			assert_eq!((k, v), (*ek, *ev), "nth {i}");
		}
	}

	/// 同 key Add → 覆盖（不增点）。
	#[test]
	fn add_overwrites_same_key() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.4, 0), 0);
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.5, 0.9, 0), 0);
		}
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3); // (0,0) (0.5,0.9) (1,1)
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (0.5, 0.9));
	}

	/// 错误码：坏句柄 / 非 parametric / curveIndex 越界 / nth 越界 /
	/// 空 out。
	#[test]
	fn error_codes() {
		let s = ps();
		let mut out = 0.0;
		// 空句柄 → BadHandle。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(std::ptr::null_mut(), 0, 0.0, 0.5, &mut out),
				status::ERR_BAD_HANDLE
			);
		}
		// 空 out → ErrValue。
		let (_inst, _ih, ph) = make_instance();
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 0, 0.0, 0.5, std::ptr::null_mut()),
				status::ERR_VALUE
			);
		}
		// curveIndex 越界（维度 1）→ BadIndex。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 1, 0.0, 0.5, &mut out),
				status::ERR_BAD_INDEX
			);
		}
		// nth 越界（2 点曲线）→ BadIndex。
		let mut k = 0.0;
		unsafe {
			assert_eq!(
				(s.parametric_param_get_nth_control_point)(ph, 0, 0.0, 5, &mut k, &mut out),
				status::ERR_BAD_INDEX
			);
		}
		// 非 parametric 参数 → BadHandle（Integer 参数的句柄）。
		let desc = EffectDescriptor::new();
		let psuite = crate::suites::param::suite_v1();
		let dhandle = descriptor_handle(&desc);
		let t = cs("OfxParamTypeInteger");
		let n = cs("gain");
		let mut gph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut gph), 0);
			// parametric 也定义一个，便于实例化后拿两个 handle。
			let pt = cs(crate::param::TYPE_PARAMETRIC);
			let pn = cs("curve");
			let mut pph: *mut c_void = std::ptr::null_mut();
			assert_eq!((psuite.param_define)(dhandle, pt.as_ptr(), pn.as_ptr(), &mut pph), 0);
			let params = ParamSetInstance {
				params: desc
					.params
					.iter()
					.map(|d| Box::new(ParamInstance::from_def((**d).clone())))
					.collect(),
			};
			let plugin = dummy_plugin(desc);
			let inst = Arc::new(Instance {
				props: PropertySet::new(),
				plugin,
				context: "OfxImageEffectContextFilter".into(),
				params,
				clips: vec![],
				node_identity: std::sync::atomic::AtomicUsize::new(0),
				destroyed: std::sync::atomic::AtomicBool::new(false),
				sequence_range: std::sync::Mutex::new(None),
				progress_cb: std::sync::Mutex::new(None),
				cancel: std::sync::atomic::AtomicBool::new(false),
				edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
				render_lock: std::sync::Mutex::new(()),
				interact: std::sync::Mutex::new(None),
			});
			let ih = instance_handle(&inst);
			let mut g: *mut c_void = std::ptr::null_mut();
			let gn = cs("gain");
			assert_eq!((psuite.param_get_handle)(ih, gn.as_ptr(), &mut g, std::ptr::null_mut()), 0);
			assert_eq!(
				(s.parametric_param_get_value)(g, 0, 0.0, 0.5, &mut out),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.parametric_param_add_control_point)(g, 0, 0.0, 0.5, 0.5, 0),
				status::ERR_BAD_HANDLE
			);
		}
	}

	/// 维度属性驱动：插件设 dimension=2 后第二维可用、第三维 BadIndex；
	/// 未配置曲线按恒等默认读数。
	#[test]
	fn dimension_prop_drives_curves() {
		let (desc, ph) = define_parametric();
		// 插件经属性 suite 把维度改成 2。
		let psuite = crate::suites::property::suite_v1();
		let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
		unsafe {
			assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
		}
		let s = ps();
		// 第二维：Add 后求值可用。
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 1, 0.0, 0.25, 0.5, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 1, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.5);
		// 第一维未配置 → 恒等默认。
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.25, &mut out), 0);
		}
		assert_eq!(out, 0.25);
		let mut n = 0;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(ph, 0, 0.0, &mut n), 0);
		}
		assert_eq!(n, 2);
		// 第三维 → BadIndex。
		unsafe {
			assert_eq!(
				(s.parametric_param_get_value)(ph, 2, 0.0, 0.5, &mut out),
				status::ERR_BAD_INDEX
			);
			assert_eq!(
				(s.parametric_param_add_control_point)(ph, 2, 0.0, 0.5, 0.5, 0),
				status::ERR_BAD_INDEX
			);
		}
		// 维度属性随 from_def 复制到实例（实例句柄上同样生效）。
		let (_inst, _ih, iph) = instantiate(desc);
		let mut n = -1;
		unsafe {
			assert_eq!((s.parametric_param_get_n_control_points)(iph, 1, 0.0, &mut n), 0);
		}
		assert_eq!(n, 3); // (0,0) (0.25,0.5) (1,1)
	}

	/// range 属性参与未配置曲线的恒等默认（端点 = range 值）。
	#[test]
	fn range_prop_shapes_implicit_identity() {
		let (_desc, ph) = define_parametric();
		let psuite = crate::suites::property::suite_v1();
		let dim = cs(crate::param::P_PARAMETRIC_DIMENSION);
		let range = cs(crate::param::P_PARAMETRIC_RANGE);
		unsafe {
			assert_eq!((psuite.set_int)(ph, dim.as_ptr(), 0, 2), 0);
			// range = (0, 255)。
			assert_eq!((psuite.set_double)(ph, range.as_ptr(), 0, 0.0), 0);
			assert_eq!((psuite.set_double)(ph, range.as_ptr(), 1, 255.0), 0);
		}
		let s = ps();
		// 未配置的第二维：恒等默认端点 = range。
		let (mut k, mut v) = (-1.0, -1.0);
		unsafe {
			assert_eq!((s.parametric_param_get_nth_control_point)(ph, 1, 0.0, 1, &mut k, &mut v), 0);
		}
		assert_eq!((k, v), (255.0, 255.0));
	}

	/// UI 属性（UIColour double×3N / InteractBackground 指针）经通用
	/// 属性 suite set/get，并随 from_def 复制到实例。
	#[test]
	fn ui_props_set_get_and_copy() {
		let (desc, ph) = define_parametric();
		let psuite = crate::suites::property::suite_v1();
		// UIColour：3 个 double（维度 1 → 1 个 RGB 三元组）。
		let colour = cs(crate::param::P_PARAMETRIC_UI_COLOUR);
		let rgb: [f64; 3] = [1.0, 0.0, 0.5];
		unsafe {
			assert_eq!((psuite.set_double_n)(ph, colour.as_ptr(), 3, rgb.as_ptr()), 0);
		}
		// InteractBackground：任意指针。
		let bg = cs(crate::param::P_PARAMETRIC_INTERACT_BG);
		let sentinel = 0x1234usize as *mut c_void;
		unsafe {
			assert_eq!((psuite.set_pointer)(ph, bg.as_ptr(), 0, sentinel), 0);
		}
		// 读回。
		let mut got = [0.0; 3];
		unsafe {
			assert_eq!((psuite.get_double_n)(ph, colour.as_ptr(), 3, got.as_mut_ptr()), 0);
		}
		assert_eq!(got, rgb);
		let mut gotp: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.get_pointer)(ph, bg.as_ptr(), 0, &mut gotp), 0);
		}
		assert_eq!(gotp, sentinel);
		// 实例 props 深拷贝。
		let (_inst, _ih, iph) = instantiate(desc);
		let mut got2 = [0.0; 3];
		unsafe {
			assert_eq!((psuite.get_double_n)(iph, colour.as_ptr(), 3, got2.as_mut_ptr()), 0);
		}
		assert_eq!(got2, rgb);
		let mut gotp2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((psuite.get_pointer)(iph, bg.as_ptr(), 0, &mut gotp2), 0);
		}
		assert_eq!(gotp2, sentinel);
	}

	/// 实例编辑走 instance-changed 通知路径（未绑定节点 → no-op，
	/// 不 panic 即通过；登记路径的节流语义由 param suite 测试覆盖）。
	#[test]
	fn instance_edits_go_through_notify_path() {
		let (_inst, _ih, ph) = make_instance();
		let s = ps();
		unsafe {
			assert_eq!((s.parametric_param_add_control_point)(ph, 0, 0.0, 0.2, 0.8, 0), 0);
			assert_eq!((s.parametric_param_set_nth_control_point)(ph, 0, 0.0, 1, 0.2, 0.9, 0), 0);
			assert_eq!((s.parametric_param_delete_control_point)(ph, 0, 1), 0);
			assert_eq!((s.parametric_param_delete_all_control_points)(ph, 0), 0);
		}
		let mut out = 0.0;
		unsafe {
			assert_eq!((s.parametric_param_get_value)(ph, 0, 0.0, 0.2, &mut out), 0);
		}
		assert_eq!(out, 0.2);
	}
}
