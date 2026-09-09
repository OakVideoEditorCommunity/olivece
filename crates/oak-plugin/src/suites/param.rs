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

//! OfxParameterSuite v1：参数定义与读写。
//!
//! 语义对照 HS: ofxhParam.cpp：
//! - paramDefine：describe 期把参数定义挂到效果描述符（未定义类型 →
//!   kOfxStatErrUnsupported，HS:1665-1710；parametric 类型放行，其
//!   曲线读写走 OfxParametricParameterSuite，见 suites/parametric.rs）；
//! - paramGetHandle：按名查（未找到 → kOfxStatErrUnknown，HS:1758）；
//! - paramGetValue/paramSetValue 等变长入口在 C shim
//!   （cbits/ofx_param_shim.c）：按 [`crate::param::ParamKind`] 解析
//!   va_args 后转发到本模块的 `ofx_param_*_impl`；
//! - paramSetValue 成功后触发 instanceChanged（kOfxChangePluginEdited，
//!   HS:1982-1994 的 `paramChangedByPlugin`），经
//!   [`crate::param::notify_instance_changed`] 回写绑定节点；
//! - 第 1 期无动画：AtTime == 当前值；keys 恒 0；derivative/integral
//!   → kOfxStatErrMissingHostFeature；editBegin/End 为编辑事务括号
//!   （undo 分组：事务内回写并入一条 multi 命令，见
//!   [`crate::instance::Instance::edit_begin`]）。

use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::Mutex;

use crate::descriptor::EffectDescriptor;
use crate::instance::Instance;
use crate::param::{ChangeReason, ParamDef, ParamInstance, ParamKind, ParamValue};
use crate::suites::status;
use crate::suites::tag;

/// 函数表布局（与 SDK `OfxParameterSuiteV1` 一致；字段序以
/// SDK 为准）。
#[repr(C)]
pub struct ParameterSuiteV1 {
	/// paramDefine（describe 期间）：定义参数。
	pub param_define:
		unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut *mut c_void) -> c_int,
	/// paramGetHandle
	pub param_get_handle: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*mut *mut c_void,
		*mut *mut c_void,
	) -> c_int,
	/// paramSetGetPropertySet
	pub param_set_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// paramGetPropertySet
	pub param_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// paramGetValue（变长出口参数按类型解释；入口在 C shim）
	pub param_get_value: unsafe extern "C" fn(*mut c_void, ...) -> c_int,
	/// paramGetValueAtTime
	pub param_get_value_at_time: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetDerivative
	pub param_get_derivative: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetIntegral
	pub param_get_integral: unsafe extern "C" fn(*mut c_void, c_double, c_double, ...) -> c_int,
	/// paramSetValue（触发 instanceChanged）
	pub param_set_value: unsafe extern "C" fn(*mut c_void, ...) -> c_int,
	/// paramSetValueAtTime
	pub param_set_value_at_time: unsafe extern "C" fn(*mut c_void, c_double, ...) -> c_int,
	/// paramGetNumKeys
	pub param_get_num_keys: unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int,
	/// paramGetKeyTime
	pub param_get_key_time: unsafe extern "C" fn(*mut c_void, c_int, *mut c_double) -> c_int,
	/// paramGetKeyIndex
	pub param_get_key_index:
		unsafe extern "C" fn(*mut c_void, c_double, c_int, *mut c_int) -> c_int,
	/// paramDeleteKey
	pub param_delete_key: unsafe extern "C" fn(*mut c_void, c_double) -> c_int,
	/// paramDeleteAllKeys
	pub param_delete_all_keys: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// paramCopy
	pub param_copy:
		unsafe extern "C" fn(*mut c_void, *mut c_void, c_double, c_double, *const c_void) -> c_int,
	/// paramEditBegin / paramEditEnd（编辑事务括号）
	pub param_edit_begin: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// paramEditEnd
	pub param_edit_end: unsafe extern "C" fn(*mut c_void) -> c_int,
}

// ---- 句柄解析 -----------------------------------------------------------

/// param-set handle：describe 期是效果描述符，实例期是实例
/// （getParamSet 返回与 effect 相同的 handle，见 image_effect suite）。
enum ParamSetRef<'a> {
	Descriptor(&'a mut EffectDescriptor),
	Instance(&'a Instance),
}

/// param handle：describe 期是定义（值=默认值），实例期是实例。
enum ParamRef<'a> {
	Def(&'a ParamDef),
	Instance(&'a ParamInstance),
}

/// 解析 param-set（effect）句柄。空指针/标签不符 → BadHandle。
fn resolve_param_set(handle: *mut c_void) -> Result<ParamSetRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	// 两种对象 props 均在偏移 0（句柄约定）。
	unsafe {
		match tag::kind(handle) {
			tag::DESCRIPTOR => Ok(ParamSetRef::Descriptor(
				&mut *(tag::strip(handle) as *mut EffectDescriptor),
			)),
			tag::INSTANCE => Ok(ParamSetRef::Instance(
				&*(tag::strip(handle) as *const Instance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// 解析 param 句柄。
fn resolve_param(handle: *mut c_void) -> Result<ParamRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::PARAM_DEF => Ok(ParamRef::Def(&*(tag::strip(handle) as *const ParamDef))),
			tag::PARAM_INSTANCE => Ok(ParamRef::Instance(
				&*(tag::strip(handle) as *const ParamInstance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// 属性名（空指针/非 UTF-8 → ErrValue；防御性）。
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// 公共入口模板：panic 兜底。
#[track_caller]
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if std::env::var_os("OAK_OFX_TRACE").is_some() {
		if code != status::OK {
			eprintln!("[ofx] param suite error {code} at {caller}");
		}
	}
	code
}

// ---- 变长参数实现（C shim 转发）----------------------------------------

impl ParamKind {
	fn from_i32(v: c_int) -> Option<ParamKind> {
		Some(match v {
			1 => ParamKind::Int,
			2 => ParamKind::Int2,
			3 => ParamKind::Int3,
			4 => ParamKind::Double,
			5 => ParamKind::Double2,
			6 => ParamKind::Double3,
			7 => ParamKind::Bool,
			8 => ParamKind::Choice,
			9 => ParamKind::Rgb,
			10 => ParamKind::Rgba,
			11 => ParamKind::Str,
			12 => ParamKind::StrChoice,
			_ => return None,
		})
	}
}

/// 按 kind 把 ParamValue 写进类型化 out（数值类）。
fn write_value(v: &ParamValue, kind: ParamKind, out: *mut c_void) -> Result<(), c_int> {
	let err = || Err(status::FAILED);
	unsafe {
		match kind {
			ParamKind::Int => match v {
				ParamValue::Int(a, _) => {
					*(out as *mut c_int) = a[0];
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Int2 | ParamKind::Int3 => match v {
				ParamValue::Int(a, dim) => {
					let n = if kind == ParamKind::Int2 { 2 } else { 3 };
					let dst = std::slice::from_raw_parts_mut(out as *mut c_int, n);
					for (d, s) in dst.iter_mut().zip(a.iter()).take(n) {
						*d = *s;
					}
					let _ = dim;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Double => match v {
				ParamValue::Double(d, _) => {
					*(out as *mut f64) = d[0];
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Double2 | ParamKind::Double3 => match v {
				ParamValue::Double(d, _) => {
					let n = if kind == ParamKind::Double2 { 2 } else { 3 };
					let dst = std::slice::from_raw_parts_mut(out as *mut f64, n);
					for (dd, s) in dst.iter_mut().zip(d.iter()).take(n) {
						*dd = *s;
					}
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Bool => match v {
				ParamValue::Bool(b) => {
					*(out as *mut c_int) = *b as c_int;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Choice => match v {
				ParamValue::Choice(c) => {
					*(out as *mut c_int) = *c;
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Rgb | ParamKind::Rgba => match v {
				ParamValue::Color(c, _) => {
					let n = if kind == ParamKind::Rgb { 3 } else { 4 };
					let dst = std::slice::from_raw_parts_mut(out as *mut f64, n);
					for (dd, s) in dst.iter_mut().zip(c.iter()).take(n) {
						*dd = *s;
					}
					Ok(())
				}
				_ => err(),
			},
			ParamKind::Str | ParamKind::StrChoice => {
				// 字符串走 ofx_param_get_string_impl（内驻指针）。
				err()
			}
		}
	}
}

/// 按 kind 从类型化 input 构造 ParamValue（数值类）。
fn read_value(kind: ParamKind, input: *const c_void) -> Option<ParamValue> {
	unsafe {
		match kind {
			ParamKind::Int => Some(ParamValue::Int([*(input as *const c_int), 0, 0], 1)),
			ParamKind::Int2 => {
				let a = *(input as *const [c_int; 2]);
				Some(ParamValue::Int([a[0], a[1], 0], 2))
			}
			ParamKind::Int3 => {
				let a = *(input as *const [c_int; 3]);
				Some(ParamValue::Int([a[0], a[1], a[2]], 3))
			}
			ParamKind::Double => Some(ParamValue::Double([*(input as *const f64), 0.0, 0.0], 1)),
			ParamKind::Double2 => {
				let a = *(input as *const [f64; 2]);
				Some(ParamValue::Double([a[0], a[1], 0.0], 2))
			}
			ParamKind::Double3 => {
				let a = *(input as *const [f64; 3]);
				Some(ParamValue::Double([a[0], a[1], a[2]], 3))
			}
			ParamKind::Bool => Some(ParamValue::Bool(*(input as *const c_int) != 0)),
			ParamKind::Choice => Some(ParamValue::Choice(*(input as *const c_int))),
			ParamKind::Rgb => {
				let a = *(input as *const [f64; 3]);
				Some(ParamValue::Color([a[0], a[1], a[2], 0.0], 3))
			}
			ParamKind::Rgba => {
				let a = *(input as *const [f64; 4]);
				Some(ParamValue::Color([a[0], a[1], a[2], a[3]], 4))
			}
			ParamKind::Str | ParamKind::StrChoice => None,
		}
	}
}

/// 取"当前值"（实例期=当前值，describe 期=默认值）。
fn value_of(r: &ParamRef) -> ParamValue {
	match r {
		ParamRef::Instance(p) => p.get(),
		ParamRef::Def(d) => d.default.clone(),
	}
}

/// param → 所属 instance 的回写定位表（instanceChanged 用）。
/// `// TODO(instance)`：createInstance 后由宿主登记，destroy 时摘除。
static PARAM_OWNER: std::sync::LazyLock<Mutex<HashMap<usize, usize>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 登记参数 → 实例（实例化路径调用）。
pub(crate) fn register_param_owner(param_props: usize, instance_props: usize) {
	PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(param_props, instance_props);
}

/// 摘除某实例的全部参数登记（destroy 路径调用）。
pub(crate) fn unregister_params_of(instance_props: usize) {
	PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.retain(|_, owner| *owner != instance_props);
}

/// paramSetValue 成功后的 instanceChanged 触发（HS:
/// ofxhParam.cpp:1991-1994 `paramChangedByPlugin`）。未登记实例时
/// no-op（未绑定节点的场景本就 no-op）。parametric suite 的
/// Set/Add/Delete 复用此路径（pub(crate)：suites/parametric.rs）。
pub(crate) fn notify_changed(param_props: usize, name: &str) {
	let owner = PARAM_OWNER
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&param_props)
		.copied();
	if let Some(inst_addr) = owner {
		// instance 的 props 在偏移 0（句柄约定）。
		let inst = unsafe { &*(inst_addr as *const Instance) };
		crate::param::notify_instance_changed(inst, name, ChangeReason::PluginEdited);
	}
}

/// 参数类型查询（C shim 调用）：未知/非法句柄 → 0（dispatch 失败）。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_kind_of(param: *mut c_void) -> c_int {
	std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		match resolve_param(param) {
			Ok(r) => match &r {
				ParamRef::Instance(p) => p.def.kind().map_or(0, |k| k as c_int),
				ParamRef::Def(d) => d.kind().map_or(0, |k| k as c_int),
			},
			Err(_) => 0,
		}
	}))
	.unwrap_or(0)
}

/// 数值类 get（C shim 转发）：`out` 为按 kind 的类型化数组。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_get_impl(
	param: *mut c_void,
	kind: c_int,
	out: *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let r = resolve_param(param)?;
		write_value(&value_of(&r), k, out)?;
		Ok(())
	})
}

/// 字符串 get（内驻指针：指向实例存储 / 定义默认值；下次 set 前
/// 有效——与属性 suite 的字符串指针契约一致）。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_get_string_impl(
	param: *mut c_void,
	out: *mut *mut c_char,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		let ptr: *const c_char = match resolve_param(param)? {
			ParamRef::Instance(p) => p.string_ptr().ok_or(status::FAILED)?,
			ParamRef::Def(d) => match &d.default {
				ParamValue::String(s) | ParamValue::StrChoice(s) => s.as_ptr(),
				_ => return Err(status::FAILED),
			},
		};
		unsafe { *out = ptr as *mut c_char };
		Ok(())
	})
}

/// 数值类 set（C shim 转发）：`input` 为按 kind 的类型化数组。
/// describe 期（ParamDef）→ BadHandle（HS: paramSetValue 的
/// verifyMagic 对 descriptor 失败）。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_set_impl(
	param: *mut c_void,
	kind: c_int,
	input: *const c_void,
) -> c_int {
	caught(|| {
		if input.is_null() {
			return Err(status::ERR_VALUE);
		}
		let k = ParamKind::from_i32(kind).ok_or(status::ERR_UNKNOWN)?;
		let value = read_value(k, input).ok_or(status::FAILED)?;
		let (name, props_addr) = match resolve_param(param)? {
			ParamRef::Instance(p) => {
				p.set_ofx(value);
				(p.def.name.clone(), &p.props as *const _ as usize)
			}
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		notify_changed(props_addr, &name);
		Ok(())
	})
}

/// 字符串 set。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_set_string_impl(
	param: *mut c_void,
	input: *const c_char,
) -> c_int {
	caught(|| {
		if input.is_null() {
			return Err(status::ERR_VALUE);
		}
		let s = CString::new(unsafe { CStr::from_ptr(input) }.to_bytes()).unwrap();
		let (name, props_addr) = match resolve_param(param)? {
			ParamRef::Instance(p) => {
				p.set_ofx(ParamValue::String(s));
				(p.def.name.clone(), &p.props as *const _ as usize)
			}
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		notify_changed(props_addr, &name);
		Ok(())
	})
}

/// derivative/integral：第 1 期无动画 → MissingHostFeature
/// （C shim 不消费 va_args 直接转发）。
#[no_mangle]
pub unsafe extern "C" fn ofx_param_missing_feature_impl(param: *mut c_void) -> c_int {
	caught(|| {
		// handle 合法性仍检查（空/坏句柄 → BadHandle）。
		let _ = resolve_param(param)?;
		Err(status::ERR_MISSING_HOST_FEATURE)
	})
}

// ---- 非变长入口 ----------------------------------------------------------

/// paramDefine：describe 期定义参数（未定义类型 → Unsupported，
/// HS:1704；重复名 → HS 允许，原样再建一条——宿主以首个为准，与
/// HS 的 map 覆盖语义一致）。
unsafe extern "C" fn param_define(
	param_set: *mut c_void,
	param_type: *const c_char,
	name: *const c_char,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *property_set = std::ptr::null_mut() };
		let t = unsafe { c_name(param_type)? };
		let n = unsafe { c_name(name)? };
		// 仅 describe 期（HS: SetDescriptor 专属，实例期 dynamic_cast
		// 失败 → BadHandle）。
		let desc = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => d,
			ParamSetRef::Instance(_) => return Err(status::ERR_BAD_HANDLE),
		};
		// 类型合法性（kind_of_type 之外还有无值类与 parametric；
		// parametric 的曲线值走 OfxParametricParameterSuite）。
		let valid = crate::param::kind_of_type(t).is_some()
			|| matches!(
				t,
				crate::param::TYPE_BYTES
					| crate::param::TYPE_CUSTOM
					| crate::param::TYPE_PUSHBUTTON
					| crate::param::TYPE_GROUP
					| crate::param::TYPE_PAGE
					| crate::param::TYPE_PARAMETRIC
			);
		if !valid {
			return Err(status::ERR_UNSUPPORTED);
		}
		let def = ParamDef::new(n, t);
		desc.params.push(Box::new(def));
		// 地址必须取自已入盒的对象（栈上临时变量在 push 后失效，
		// 句柄会指向死内存——Box 负载地址在 Vec 重分配时稳定）。
		let def = desc.params.last().expect("just pushed");
		let addr = &def.props as *const _ as usize;
		unsafe {
			*property_set = tag::make(addr as *const crate::property::PropertySet, tag::PARAM_DEF)
		};
		Ok(())
	})
}

/// paramGetHandle：按名查（未找到 → ErrUnknown，HS:1758）。
unsafe extern "C" fn param_get_handle(
	param_set: *mut c_void,
	name: *const c_char,
	param: *mut *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if param.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *param = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let handle = match resolve_param_set(param_set)? {
			ParamSetRef::Descriptor(d) => {
				let def = d.param(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &def.props as *const _ as usize;
				tag::make(addr as *const crate::property::PropertySet, tag::PARAM_DEF)
			}
			ParamSetRef::Instance(i) => {
				let p = i.params.find(n).ok_or(status::ERR_UNKNOWN)?;
				let addr = &p.props as *const _ as usize;
				tag::make(
					addr as *const crate::property::PropertySet,
					tag::PARAM_INSTANCE,
				)
			}
		};
		unsafe { *param = handle };
		if !property_set.is_null() {
			// props 在偏移 0：param handle 与 props handle 同值。
			unsafe { *property_set = handle };
		}
		Ok(())
	})
}

/// paramSetGetPropertySet：param-set 的属性集（= effect handle 本体）。
unsafe extern "C" fn param_set_get_property_set(
	param_set: *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_param_set(param_set)?;
		unsafe { *property_set = param_set };
		Ok(())
	})
}

/// paramGetPropertySet：param 的属性集（= param handle 本体）。
unsafe extern "C" fn param_get_property_set(
	param: *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if property_set.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_param(param)?;
		unsafe { *property_set = param };
		Ok(())
	})
}

/// paramGetNumKeys：第 1 期无动画 → 恒 0。
unsafe extern "C" fn param_get_num_keys(param: *mut c_void, count: *mut c_int) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		if count.is_null() {
			return Err(status::ERR_VALUE);
		}
		unsafe { *count = 0 };
		Ok(())
	})
}

/// paramGetKeyTime：无关键帧 → BadIndex。
unsafe extern "C" fn param_get_key_time(
	param: *mut c_void,
	_index: c_int,
	_time: *mut c_double,
) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Err(status::ERR_BAD_INDEX)
	})
}

/// paramGetKeyIndex：无关键帧 → BadIndex。
unsafe extern "C" fn param_get_key_index(
	param: *mut c_void,
	_time: c_double,
	_direction: c_int,
	_index: *mut c_int,
) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Err(status::ERR_BAD_INDEX)
	})
}

/// paramDeleteKey / paramDeleteAllKeys：无关键帧 → OK no-op。
unsafe extern "C" fn param_delete_key(param: *mut c_void, _time: c_double) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Ok(())
	})
}

unsafe extern "C" fn param_delete_all_keys(param: *mut c_void) -> c_int {
	caught(|| {
		let _ = resolve_param(param)?;
		Ok(())
	})
}

/// paramCopy：第 1 期无关键帧可拷 → OK no-op。
unsafe extern "C" fn param_copy(
	dst: *mut c_void,
	src: *mut c_void,
	_dst_time: c_double,
	_src_time: c_double,
	_key_range: *const c_void,
) -> c_int {
	caught(|| {
		let _ = resolve_param(dst)?;
		let _ = resolve_param(src)?;
		Ok(())
	})
}

/// paramEditBegin：进入编辑事务（undo 分组）。解析 param 句柄后经
/// PARAM_OWNER 定位所属实例并递增其事务深度；未登记实例时 no-op
/// （手工构造的实例没有宿主注册，edit 括号退化为无分组）。
unsafe extern "C" fn param_edit_begin(param: *mut c_void) -> c_int {
	caught(|| {
		let addr = match resolve_param(param)? {
			ParamRef::Instance(p) => &p.props as *const _ as usize,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if let Some(owner) = PARAM_OWNER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(&addr)
			.copied()
		{
			let inst = unsafe { &*(owner as *const Instance) };
			inst.edit_begin();
		}
		Ok(())
	})
}

/// paramEditEnd：退出编辑事务；最外层结束时把累积的 multi 命令
/// redo 生效并释放（见 [`crate::instance::Instance::edit_end`]）。
unsafe extern "C" fn param_edit_end(param: *mut c_void) -> c_int {
	caught(|| {
		let addr = match resolve_param(param)? {
			ParamRef::Instance(p) => &p.props as *const _ as usize,
			ParamRef::Def(_) => return Err(status::ERR_BAD_HANDLE),
		};
		if let Some(owner) = PARAM_OWNER
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.get(&addr)
			.copied()
		{
			let inst = unsafe { &*(owner as *const Instance) };
			inst.edit_end();
		}
		Ok(())
	})
}

extern "C" {
	fn ofx_param_get_value_shim(param: *mut c_void, ...) -> c_int;
	fn ofx_param_get_value_at_time_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_set_value_shim(param: *mut c_void, ...) -> c_int;
	fn ofx_param_set_value_at_time_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_get_derivative_shim(param: *mut c_void, time: c_double, ...) -> c_int;
	fn ofx_param_get_integral_shim(
		param: *mut c_void,
		time1: c_double,
		time2: c_double,
		...
	) -> c_int;
}

/// 静态函数表实例。
pub fn suite_v1() -> &'static ParameterSuiteV1 {
	static SUITE: std::sync::OnceLock<ParameterSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ParameterSuiteV1 {
		param_define: param_define,
		param_get_handle: param_get_handle,
		param_set_get_property_set: param_set_get_property_set,
		param_get_property_set: param_get_property_set,
		param_get_value: ofx_param_get_value_shim,
		param_get_value_at_time: ofx_param_get_value_at_time_shim,
		param_get_derivative: ofx_param_get_derivative_shim,
		param_get_integral: ofx_param_get_integral_shim,
		param_set_value: ofx_param_set_value_shim,
		param_set_value_at_time: ofx_param_set_value_at_time_shim,
		param_get_num_keys: param_get_num_keys,
		param_get_key_time: param_get_key_time,
		param_get_key_index: param_get_key_index,
		param_delete_key: param_delete_key,
		param_delete_all_keys: param_delete_all_keys,
		param_copy: param_copy,
		param_edit_begin: param_edit_begin,
		param_edit_end: param_edit_end,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;
	use std::sync::Arc;

	use crate::host::Plugin;
	use crate::param::{ParamInstance, ParamSetInstance};
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(
			&d.props as *const crate::property::PropertySet,
			tag::DESCRIPTOR,
		)
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
		tag::make(
			&i.props as *const crate::property::PropertySet,
			tag::INSTANCE,
		)
	}

	/// 用 paramDefine 建一个实例（describe 产物 → createInstance）。
	fn make_instance() -> (Arc<Instance>, *mut c_void) {
		let mut desc = EffectDescriptor::new();
		let s = suite_v1();
		let dhandle = descriptor_handle(&desc);
		unsafe {
			for (t, n) in [
				("OfxParamTypeInteger", "gain"),
				("OfxParamTypeDouble", "opacity"),
				("OfxParamTypeDouble2D", "pos"),
				("OfxParamTypeRGB", "color"),
				("OfxParamTypeBoolean", "enabled"),
				("OfxParamTypeString", "label"),
			] {
				let t = cs(t);
				let n = cs(n);
				let mut ph: *mut c_void = std::ptr::null_mut();
				assert_eq!(
					(s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph),
					0
				);
			}
		}
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
		(inst.clone(), instance_handle(&inst))
	}

	#[test]
	fn describe_define_and_get() {
		let mut desc = EffectDescriptor::new();
		let s = suite_v1();
		let handle = descriptor_handle(&desc);
		let t = cs("OfxParamTypeDouble");
		let n = cs("opacity");

		let mut ph: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_define)(handle, t.as_ptr(), n.as_ptr(), &mut ph), 0);
		}
		assert!(!ph.is_null());
		assert_eq!(tag::kind(ph), tag::PARAM_DEF);

		// describe 期 paramGetValue：默认值 0.0。
		let mut v = 99.0;
		unsafe {
			assert_eq!((s.param_get_value)(ph, &mut v), 0);
		}
		assert_eq!(v, 0.0);

		// describe 期 set → BadHandle（HS: verifyMagic 对 descriptor 失败）。
		unsafe {
			assert_eq!((s.param_set_value)(ph, 42.0), status::ERR_BAD_HANDLE);
		}

		// paramGetHandle 按名取（props handle 同值）。
		let mut ph2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.param_get_handle)(handle, n.as_ptr(), &mut ph2, std::ptr::null_mut()),
				0
			);
			assert_eq!(ph, ph2);
		}

		// 未找到 → ErrUnknown（HS:1758）。
		let nope = cs("nope");
		unsafe {
			assert_eq!(
				(s.param_get_handle)(handle, nope.as_ptr(), &mut ph2, std::ptr::null_mut()),
				status::ERR_UNKNOWN
			);
		}

		// 未定义类型 → ErrUnsupported（HS:1704）；parametric → 放行
		// （曲线读写走 OfxParametricParameterSuite）。
		let bad = cs("OfxParamTypeBogus");
		let par = cs("OfxParamTypeParametric");
		unsafe {
			assert_eq!(
				(s.param_define)(handle, bad.as_ptr(), n.as_ptr(), &mut ph2),
				status::ERR_UNSUPPORTED
			);
			assert_eq!((s.param_define)(handle, par.as_ptr(), n.as_ptr(), &mut ph2), 0);
		}
		assert_eq!(tag::kind(ph2), tag::PARAM_DEF);

		// 实例期 paramDefine → BadHandle。
		let (_inst, ih) = make_instance();
		unsafe {
			assert_eq!(
				(s.param_define)(ih, t.as_ptr(), n.as_ptr(), &mut ph2),
				status::ERR_BAD_HANDLE
			);
		}
	}

	#[test]
	fn instance_get_set_variadic_roundtrip() {
		let (_inst, ih) = make_instance();
		let s = suite_v1();

		// 取各参数 handle。
		let mut gain: *mut c_void = std::ptr::null_mut();
		let mut opacity: *mut c_void = std::ptr::null_mut();
		let mut pos: *mut c_void = std::ptr::null_mut();
		let mut color: *mut c_void = std::ptr::null_mut();
		let mut enabled: *mut c_void = std::ptr::null_mut();
		let mut label: *mut c_void = std::ptr::null_mut();
		unsafe {
			for (n, out) in [
				("gain", &mut gain),
				("opacity", &mut opacity),
				("pos", &mut pos),
				("color", &mut color),
				("enabled", &mut enabled),
				("label", &mut label),
			] {
				let n = cs(n);
				let r = (s.param_get_handle)(ih, n.as_ptr(), out, std::ptr::null_mut());
				assert_eq!(r, 0, "gethandle {} failed: {}", n.to_str().unwrap(), r);
				assert_eq!(tag::kind(*out), tag::PARAM_INSTANCE);
			}
		}

		// Integer set/get。
		unsafe {
			assert_eq!((s.param_set_value)(gain, 7), 0);
		}
		let mut v = 0;
		unsafe {
			assert_eq!((s.param_get_value)(gain, &mut v), 0);
		}
		assert_eq!(v, 7);

		// Double2D。
		unsafe {
			assert_eq!((s.param_set_value)(pos, 1.5, -2.5), 0);
		}
		let (mut x, mut y) = (0.0, 0.0);
		unsafe {
			assert_eq!((s.param_get_value)(pos, &mut x, &mut y), 0);
		}
		assert_eq!((x, y), (1.5, -2.5));

		// RGB。
		unsafe {
			assert_eq!((s.param_set_value)(color, 0.1, 0.2, 0.3), 0);
		}
		let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
		unsafe {
			assert_eq!((s.param_get_value)(color, &mut r, &mut g, &mut b), 0);
		}
		assert_eq!((r, g, b), (0.1, 0.2, 0.3));

		// Boolean（int 0/1）。
		unsafe {
			assert_eq!((s.param_set_value)(enabled, 1), 0);
		}
		let mut e = 0;
		unsafe {
			assert_eq!((s.param_get_value)(enabled, &mut e), 0);
		}
		assert_eq!(e, 1);

		// String：set 后 get 返回内驻指针。
		let hello = cs("hello");
		unsafe {
			assert_eq!((s.param_set_value)(label, hello.as_ptr()), 0);
		}
		let mut p: *mut c_char = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.param_get_value)(label, &mut p), 0);
			assert_eq!(CStr::from_ptr(p).to_bytes(), b"hello");
		}

		// AtTime == 当前值（无动画）。
		let mut v2 = 0.0;
		unsafe {
			assert_eq!((s.param_get_value_at_time)(opacity, 12.0, &mut v2), 0);
			assert_eq!(v2, 0.0);
			assert_eq!((s.param_set_value_at_time)(opacity, 12.0, 0.75), 0);
		}
		let mut v3 = 0.0;
		unsafe {
			assert_eq!((s.param_get_value)(opacity, &mut v3), 0);
		}
		assert_eq!(v3, 0.75);

		// keys：恒 0 / BadIndex。
		let mut nkeys = -1;
		unsafe {
			assert_eq!((s.param_get_num_keys)(opacity, &mut nkeys), 0);
			assert_eq!(nkeys, 0);
			let mut kt = 0.0;
			assert_eq!(
				(s.param_get_key_time)(opacity, 0, &mut kt),
				status::ERR_BAD_INDEX
			);
		}

		// derivative/integral → MissingHostFeature。
		let mut dv = 0.0;
		unsafe {
			assert_eq!(
				(s.param_get_derivative)(opacity, 1.0, &mut dv),
				status::ERR_MISSING_HOST_FEATURE
			);
		}

		// editBegin/End、deleteKey(s)、copy：no-op OK。
		unsafe {
			assert_eq!((s.param_edit_begin)(opacity), 0);
			assert_eq!((s.param_edit_end)(opacity), 0);
			assert_eq!((s.param_delete_key)(opacity, 1.0), 0);
			assert_eq!((s.param_delete_all_keys)(opacity), 0);
			assert_eq!(
				(s.param_copy)(opacity, opacity, 1.0, 1.0, std::ptr::null()),
				0
			);
		}
	}

	/// paramSetValue → instanceChanged 回写（C++ paramChangedByPlugin）：
	/// 实例绑定节点后，插件侧改值经 PARAM_OWNER 定位实例、
	/// notify_instance_changed 生成 undo 命令并立即 redo——节点输入
	/// 即新值。
	#[test]
	fn set_value_writes_back_to_bound_node() {
		let (inst, ih) = make_instance();
		let s = suite_v1();
		let mut gain: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.param_get_handle)(ih, cs("gain").as_ptr(), &mut gain, std::ptr::null_mut()),
				0
			);
		}

		// 宿主侧登记：param → 实例（生产由 createInstance 路径做），
		// 实例 → oaknode 节点（facade 装配期做）。
		let project = oak_node::project::Project::new();
		let node_id = {
			let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
			let id = guard.graph.add_node(
				oak_node::node::NodeCore::empty(),
				Box::new(oak_node::nodes::EmptyBehavior),
			);
			guard.graph.get_mut(id).unwrap().core.add_input(
				oak_node::input::Input::new(
					"gain",
					oak_node::value::ValueType::Int,
					oak_node::value::NodeValue::Int(0),
				),
			);
			id
		};
		let identity = crate::node::register_node(project.clone(), node_id);
		inst.bind_node(identity as usize);
		let param_addr = tag::strip(gain) as usize;
		register_param_owner(param_addr, &inst.props as *const PropertySet as usize);

		unsafe {
			assert_eq!((s.param_set_value)(gain, 5), 0);
		}
		let stored = project
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.graph
			.get(node_id)
			.unwrap()
			.core
			.standard_value("gain", -1);
		assert_eq!(
			stored,
			oak_node::value::NodeValue::Int(5),
			"插件改值必须经 instanceChanged 写回节点输入"
		);
		unregister_params_of(&inst.props as *const PropertySet as usize);
		crate::node::unregister_node(identity);
	}
}
