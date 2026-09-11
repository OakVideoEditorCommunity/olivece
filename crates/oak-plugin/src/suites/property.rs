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

//! OfxPropertySuite v1：属性读写。
//!
//! 语义逐条对照 HS: ofxhPropertySuite.cpp：
//! - 读未定义属性 / 类型不符 → kOfxStatErrUnknown（HS: propGet 的
//!   `fetchTypedProperty` 失败路径，ofxhPropertySuite.cpp:787-794）；
//! - 越界读写 → kOfxStatErrBadIndex（HS: `getValueRaw`/
//!   `setValue`，ofxhPropertySuite.cpp:257/284）；
//! - propGetN 拷贝 min(count, dimension) 个元素，不报错
//!   （HS: `getValueNRaw`，ofxhPropertySuite.cpp:271-280）；
//! - propSetN 维度变化时整体替换：本 crate 的属性维度始终跟随数组
//!   长度（HS 对固定维度属性 count > dimension 会返回
//!   kOfxStatErrBadIndex，ofxhPropertySuite.cpp:299——本 crate 不区分
//!   固定/可变维度，如实更宽容；三个系统 bundle 均以正确 count 调用，
//!   无行为分歧）；
//! - 只读属性：SDK 无 kOfxStatErrReadOnly，HostSupport 也不在 suite
//!   层拦截写（`_pluginReadOnly` 仅供宿主内部逻辑查询）——本实现
//!   如实不拦截；若日后需要，在 PropertySet 增加只读标记再评。
//! - propReset：HostSupport 恢复到 define 时默认值
//!   （ofxhPropertySuite.cpp:330）；本 crate 不保存默认值快照，
//!   第 1 期明确返回 kOfxStatErrUnsupported（真话比静默 no-op 安全）。

use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};

use crate::property::{Property, PropertySet, Value};
use crate::suites::status;

/// 函数表布局（与 SDK `OfxPropertySuiteV1` 逐字段一致）。
#[repr(C)]
pub struct PropertySuiteV1 {
	/// propSetPointer
	pub set_pointer: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int,
	/// propSetString
	pub set_string: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_char) -> c_int,
	/// propSetDouble
	pub set_double: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_double) -> c_int,
	/// propSetInt
	pub set_int: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, c_int) -> c_int,
	/// propSetPointerN
	pub set_pointer_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const *mut c_void) -> c_int,
	/// propSetStringN
	pub set_string_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const *const c_char) -> c_int,
	/// propSetDoubleN
	pub set_double_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_double) -> c_int,
	/// propSetIntN
	pub set_int_n: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *const c_int) -> c_int,
	/// propGetPointer
	pub get_pointer:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void) -> c_int,
	/// propGetString
	pub get_string:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_char) -> c_int,
	/// propGetDouble
	pub get_double: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_double) -> c_int,
	/// propGetInt
	pub get_int: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int,
	/// propGetPointerN
	pub get_pointer_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void) -> c_int,
	/// propGetStringN
	pub get_string_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_char) -> c_int,
	/// propGetDoubleN
	pub get_double_n:
		unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_double) -> c_int,
	/// propGetIntN
	pub get_int_n: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int,
	/// propReset
	pub reset: unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int,
	/// propGetDimension
	pub get_dimension: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int) -> c_int,
}

/// 属性值类别（suite 层的类型检查；对应 HS 的 `Property::TypeEnum`）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	Int,
	Double,
	Str,
	Pointer,
}

impl Kind {
	fn of(v: &Value) -> Kind {
		match v {
			Value::Int(_) => Kind::Int,
			Value::Double(_) => Kind::Double,
			Value::String(_) => Kind::Str,
			Value::Pointer(_) => Kind::Pointer,
		}
	}
}

/// 属性 suite 公共入口模板：解句柄 + panic 兜底。
///
/// 句柄按 [`crate::suites::tag`] 约定：低 3 位是对象种类标签，地址
/// 即对象 props 字段（偏移 0）；剥标签后可直接当 PropertySet 用。
/// 裸 PropertySet 指针（标签 0，宿主内部/测试直传）原样通过。
///
/// `# Safety`：`handle` 必须指向活的 `PropertySet` 或已注册对象
/// （suite 生命周期契约：宿主对象先于 suite 调用创建，后于全部调用
/// 销毁）。
#[track_caller]
unsafe fn caught(handle: *mut c_void, f: impl FnOnce(&PropertySet) -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		if handle.is_null() {
			return status::ERR_BAD_HANDLE;
		}
		let set = unsafe { &*crate::suites::tag::strip(handle) };
		f(set).map_or_else(|code| code, |()| status::OK)
	}))
	.unwrap_or(status::FAILED);
	if code != status::OK && std::env::var_os("OAK_OFX_TRACE").is_some() {
		let tag = crate::suites::tag::kind(handle);
		eprintln!("[ofx] property suite error {code} at {caller} (handle {handle:p} tag {tag})");
	}
	code
}

/// 属性名：空指针 / 非 UTF-8 → kOfxStatErrValue（防御性；HostSupport
/// 直接解引用会崩）。
///
/// `# Safety`：`name` 必须是有效的 NUL 结尾 C 字符串（插件契约）。
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// 越界防护：`index` 转 usize（负数 → 越界错误）。
fn idx(index: c_int) -> Result<usize, c_int> {
	usize::try_from(index).map_err(|_| status::ERR_BAD_INDEX)
}

/// 先类型后索引的取值（HS 顺序：`fetchTypedProperty` 在
/// `getValueRaw` 之前，ofxhPropertySuite.cpp:787/794/257）。
fn get_value<'a>(
	props: &'a [Property],
	name: &str,
	index: c_int,
	kind: Kind,
) -> Result<&'a Value, c_int> {
	let result = get_value_inner(props, name, index, kind);
	if std::env::var_os("OAK_OFX_TRACE").is_some() {
		match &result {
			Ok(_) => eprintln!("[ofx] propGet({name}[{index}]) -> ok"),
			Err(c) => eprintln!("[ofx] propGet({name}[{index}]) -> {c}"),
		}
	}
	result
}

fn get_value_inner<'a>(
	props: &'a [Property],
	name: &str,
	index: c_int,
	kind: Kind,
) -> Result<&'a Value, c_int> {
	let p = props
		.iter()
		.find(|p| p.name == name)
		.ok_or(status::ERR_UNKNOWN)?;
	// 首元素代理整条属性的类型（宿主只定义同构数组；HS 是定义期
	// 固定类型，等价）。空数组没有类型探针，跳过类型检查，越界
	// 由下面的索引访问报 BadIndex（HS getValueRaw 语义）。
	if let Some(probe) = p.values.first() {
		// Int <-> Double 数值协随（与 get_n 同款宽容宿主语义）。
		let numeric = matches!(Kind::of(probe), Kind::Int | Kind::Double)
			&& matches!(kind, Kind::Int | Kind::Double);
		if Kind::of(probe) != kind && !numeric {
			if std::env::var_os("OAK_OFX_TRACE").is_some() {
				eprintln!("[ofx] property kind mismatch: {name}");
			}
			return Err(status::ERR_UNKNOWN);
		}
	}
	p.values.get(idx(index)?).ok_or(status::ERR_BAD_INDEX)
}

// ---- propSet（单元素）-----------------------------------------------------

/// propSet 通用实现：属性未定义时按 OFX 语义**隐式创建**（ofxProperty.h：
/// "If the property does not exist it is created"，HS setValue 同）——
/// 插件 describe 期写的大量描述符属性（grouping、各 capability 开关）
/// 并未由宿主预定义，拒绝创建会让 describe 直接失败。已定义但类型
/// 不符 → Unknown；越界 → BadIndex。
fn set_value(set: &PropertySet, name: &str, index: c_int, value: Value) -> Result<(), c_int> {
	set.with_locked(|props| {
		let idx = idx(index)?;
		let Some(p) = props.iter_mut().find(|p| p.name == name) else {
			// 隐式创建：维度 index+1，前导槽位以同值填充（HS 的新属性
			// 默认值语义——新建属性先有一个默认元素再逐位写）。
			let mut values = vec![value.clone(); idx];
			values.push(value);
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// 空属性（宿主预定义的空数组，如 OfxImageEffectPropSupportedContexts）
		// 没有类型探针：按 HS 的 index == size 追加语义直接 push。
		if p.values.is_empty() {
			if idx != 0 {
				if std::env::var_os("OAK_OFX_TRACE").is_some() {
					eprintln!("[ofx] propSet into empty predefined array: {name}[{idx}]");
				}
				return Err(status::ERR_BAD_INDEX);
			}
			p.values.push(value);
			return Ok(());
		}
		let probe = &p.values[0];
		if Kind::of(probe) != Kind::of(&value) {
			return Err(status::ERR_UNKNOWN);
		}
		// HS `setValue` 允许 index == size 时追加（ofxhPropertySuite.cpp:284）
		// ——addSupportedContext 等协商属性正是这样逐位增长的。
		if idx == p.values.len() {
			p.values.push(value);
			return Ok(());
		}
		if idx >= p.values.len() && std::env::var_os("OAK_OFX_TRACE").is_some() {
			eprintln!("[ofx] propSet out of range: {name}[{idx}] (dim {})", p.values.len());
		}
		let slot = p.values.get_mut(idx).ok_or(status::ERR_BAD_INDEX)?;
		*slot = value;
		Ok(())
	})
}

unsafe extern "C" fn prop_set_pointer(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Pointer(value))
		})
	}
}

unsafe extern "C" fn prop_set_string(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: *const c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if value.is_null() {
				return Err(status::ERR_VALUE);
			}
			// C 语义：截断到首个 NUL；CString::new 不会失败
			//（from_ptr 已保证 NUL 结尾）。
			let s = CString::new(CStr::from_ptr(value).to_bytes()).unwrap();
			set_value(set, name, index, Value::String(s))
		})
	}
}

unsafe extern "C" fn prop_set_double(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Double(value))
		})
	}
}

unsafe extern "C" fn prop_set_int(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	value: c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			set_value(set, name, index, Value::Int(value))
		})
	}
}

// ---- propSetN（批量）------------------------------------------------------

/// 从 C 数组读出 `count` 个元素并整体写入；count != 现有维度时
/// 整体替换（HS `setValueN` 的 resize 语义，ofxhPropertySuite.cpp:299-311）。
fn set_values(
	set: &PropertySet,
	name: &str,
	count: c_int,
	values: Vec<Value>,
) -> Result<(), c_int> {
	set.with_locked(|props| {
		let Some(p) = props.iter_mut().find(|p| p.name == name) else {
			// 与单元素 set_value 一致：未定义属性按 OFX 语义隐式创建。
			props.push(crate::property::Property {
				name: name.to_string(),
				values,
			});
			return Ok(());
		};
		// count == 0 时无类型可探（HS 仍会做 fetchTypedProperty）。
		if let Some(first) = p.values.first() {
			if let Some(v) = values.first() {
				if Kind::of(first) != Kind::of(v) {
					return Err(status::ERR_UNKNOWN);
				}
			}
		}
		let count = idx(count)?;
		if count != p.values.len() {
			// 维度变化：整体替换（HS 是 resize + 逐位写，等价）。
			p.values = values;
		} else {
			for (i, v) in values.into_iter().enumerate() {
				p.values[i] = v;
			}
		}
		Ok(())
	})
}

unsafe extern "C" fn prop_set_pointer_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Pointer(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

unsafe extern "C" fn prop_set_string_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const *const c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let mut vals = Vec::with_capacity(count.max(0) as usize);
			for i in 0..idx(count)? {
				let v = *values.add(i);
				if v.is_null() {
					return Err(status::ERR_VALUE);
				}
				vals.push(Value::String(
					CString::new(CStr::from_ptr(v).to_bytes()).unwrap(),
				));
			}
			set_values(set, name, count, vals)
		})
	}
}

unsafe extern "C" fn prop_set_double_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Double(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

unsafe extern "C" fn prop_set_int_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	values: *const c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && values.is_null() {
				return Err(status::ERR_VALUE);
			}
			let vals = (0..idx(count)?)
				.map(|i| Value::Int(*values.add(i)))
				.collect();
			set_values(set, name, count, vals)
		})
	}
}

// ---- propGet（单元素）-----------------------------------------------------

/// 非字符串标量读取（克隆值写出；字符串走内驻指针路径）。
fn get_scalar(set: &PropertySet, name: &str, index: c_int, kind: Kind) -> Result<Value, c_int> {
	set.with_locked(|props| get_value(props, name, index, kind).cloned())
}

unsafe extern "C" fn prop_get_pointer(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Pointer)?;
			match v {
				Value::Pointer(p) => {
					if std::env::var_os("OAK_OFX_TRACE").is_some() {
						eprintln!("[ofx] propGetPointer(h={handle:p}, {name}[{index}]) -> {p:p}");
					}
					*out = p;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
		})
	}
}

/// propGetString：写**内驻** C 串指针（OFX 契约：指向宿主内部存储，
/// 属性被下次修改前有效；克隆的 CString 指针会悬垂，绝不能给插件）。
fn get_string(set: &PropertySet, name: &str, index: c_int) -> Result<*mut c_char, c_int> {
	set.with_locked(|props| match get_value(props, name, index, Kind::Str)? {
		Value::String(s) => Ok(s.as_ptr() as *mut c_char),
		_ => Err(status::FAILED),
	})
}

unsafe extern "C" fn prop_get_string(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut *mut c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let r = get_string(set, name, index);
			if std::env::var_os("OAK_OFX_TRACE").is_some() {
				let rendered = match &r {
					Ok(p) => format!("0 <- {:?}", std::ffi::CStr::from_ptr(*p)),
					Err(c) => format!("{c}"),
				};
				eprintln!("[ofx] propGetString(h={handle:p}, {name}[{index}]) -> {rendered}");
			}
			*out = r?;
			Ok(())
		})
	}
}

unsafe extern "C" fn prop_get_double(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Double)?;
			match v {
				Value::Double(d) => {
					*out = d;
					Ok(())
				}
				// Int <-> Double 数值协随（宽容宿主语义，同 get_n）。
				Value::Int(i) => {
					*out = i as f64;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
		})
	}
}

unsafe extern "C" fn prop_get_int(
	handle: *mut c_void,
	name: *const c_char,
	index: c_int,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			let v = get_scalar(set, name, index, Kind::Int)?;
			match v {
				Value::Int(i) => {
					*out = i;
					Ok(())
				}
				// Int <-> Double 数值协随（宽容宿主语义，同 get_n）。
				Value::Double(d) => {
					*out = d as i32;
					Ok(())
				}
				_ => Err(status::FAILED),
			}
			.map_err(|c| {
				if std::env::var_os("OAK_OFX_TRACE").is_some() {
					eprintln!("[ofx] propGetInt({name}) -> {c}");
				}
				c
			})
		})
	}
}

// ---- propGetN（批量）------------------------------------------------------

/// 批量读取：拷贝 min(count, dimension) 个（HS `getValueNRaw`，
/// ofxhPropertySuite.cpp:271-280，不报错）。
fn get_n(
	set: &PropertySet,
	name: &str,
	count: c_int,
	kind: Kind,
	write: impl Fn(&Value, usize),
) -> Result<(), c_int> {
	set.with_locked(|props| {
		let p = props
			.iter()
			.find(|p| p.name == name)
			.ok_or(status::ERR_UNKNOWN)?;
		let probe = p.values.first().ok_or(status::ERR_UNKNOWN)?;
		// Int <-> Double coercion (forgiving-host semantics): the CImg
		// framework reads the DOUBLE render window with propGetIntN and
		// must still get the values; strict kind checks turn that into a
		// bogus MissingHostFeature at render time.
		let numeric = matches!(Kind::of(probe), Kind::Int | Kind::Double)
			&& matches!(kind, Kind::Int | Kind::Double);
		if Kind::of(probe) != kind && !numeric {
			return Err(status::ERR_UNKNOWN);
		}
		let n = idx(count)?.min(p.values.len());
		for (i, v) in p.values.iter().take(n).enumerate() {
			write(v, i);
		}
		if std::env::var_os("OAK_OFX_TRACE").is_some() {
			let dump: Vec<String> = p
				.values
				.iter()
				.take(n)
				.map(|v| match v {
					Value::Int(i) => i.to_string(),
					Value::Double(d) => d.to_string(),
					_ => "?".to_string(),
				})
				.collect();
			eprintln!("[ofx] propGetN({name} x{n}) -> ok [{dump:?}]");
		}
		Ok(())
	})
	.map_err(|code| {
		// The property the plugin asked for and we did not have — the
		// single most useful line when a plugin reports
		// MissingHostFeature (trace-gated, like fetchSuite misses).
		if std::env::var_os("OAK_OFX_TRACE").is_some() {
			eprintln!("[ofx] property miss: {name} (code {code})");
		}
		code
	})
}

unsafe extern "C" fn prop_get_pointer_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut *mut c_void,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Pointer, |v, i| match v {
				Value::Pointer(p) => *out.add(i) = *p,
				_ => {}
			})
		})
	}
}

unsafe extern "C" fn prop_get_string_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut *mut c_char,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Str, |v, i| match v {
				Value::String(s) => *out.add(i) = s.as_ptr() as *mut c_char,
				_ => {}
			})
		})
	}
}

unsafe extern "C" fn prop_get_double_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut c_double,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Double, |v, i| match v {
				Value::Double(d) => *out.add(i) = *d,
				Value::Int(d) => *out.add(i) = *d as f64,
				_ => {}
			})
		})
	}
}

unsafe extern "C" fn prop_get_int_n(
	handle: *mut c_void,
	name: *const c_char,
	count: c_int,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if count > 0 && out.is_null() {
				return Err(status::ERR_VALUE);
			}
			get_n(set, name, count, Kind::Int, |v, i| match v {
				Value::Int(d) => *out.add(i) = *d,
				Value::Double(d) => *out.add(i) = *d as i32,
				_ => {}
			})
		})
	}
}

// ---- propReset / propGetDimension -----------------------------------------

unsafe extern "C" fn prop_reset(handle: *mut c_void, name: *const c_char) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			// HS `reset()` 恢复到 define 时的默认值
			// （ofxhPropertySuite.cpp:330）；本 crate 不保存默认值快照，
			// 第 1 期明确不支持（见模块文档）。
			let _ = (set, name);
			Err(status::ERR_UNSUPPORTED)
		})
	}
}

unsafe extern "C" fn prop_get_dimension(
	handle: *mut c_void,
	name: *const c_char,
	out: *mut c_int,
) -> c_int {
	unsafe {
		caught(handle, |set| {
			let name = c_name(name)?;
			if out.is_null() {
				return Err(status::ERR_VALUE);
			}
			// HS: fetchProperty 失败（未定义）→ Unknown（ofxhPropertySuite.cpp:992）；
			// 已定义的空数组（如协商属性的初始状态）是合法维度 0，不是错误。
			let exists = set.with_locked(|props| props.iter().any(|p| p.name == name));
			if !exists {
				if std::env::var_os("OAK_OFX_TRACE").is_some() {
					let tag = crate::suites::tag::kind(handle);
					eprintln!("[ofx] property miss (dimension): {name} (handle {handle:p} tag {tag})");
				}
				return Err(status::ERR_UNKNOWN);
			}
			*out = set.dimension(name) as c_int;
			Ok(())
		})
	}
}

/// 静态函数表实例（fetch_suite 返回其地址）。
pub fn suite_v1() -> &'static PropertySuiteV1 {
	static SUITE: std::sync::OnceLock<PropertySuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| PropertySuiteV1 {
		set_pointer: prop_set_pointer,
		set_string: prop_set_string,
		set_double: prop_set_double,
		set_int: prop_set_int,
		set_pointer_n: prop_set_pointer_n,
		set_string_n: prop_set_string_n,
		set_double_n: prop_set_double_n,
		set_int_n: prop_set_int_n,
		get_pointer: prop_get_pointer,
		get_string: prop_get_string,
		get_double: prop_get_double,
		get_int: prop_get_int,
		get_pointer_n: prop_get_pointer_n,
		get_string_n: prop_get_string_n,
		get_double_n: prop_get_double_n,
		get_int_n: prop_get_int_n,
		reset: prop_reset,
		get_dimension: prop_get_dimension,
	})
}
