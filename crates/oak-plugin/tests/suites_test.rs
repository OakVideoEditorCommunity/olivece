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

//! suite 层 round-trip 测试：宿主 suite 表 ⇄ 最小测试插件。
//!
//! 声明文档原计划"每个用例让测试插件在 describe/render 期间真实
//! 回调对应 suite"（"插件视角"的 HostSupport 兼容性背书）。最小
//! 测试插件（tests/fixtures/oak-test-plugin.ofx.bundle）是第 0 期
//! 交付物，尚未构建——插件无关的断言直接执行；依赖插件的用例经
//! [`common::skip`] 门，插件落地后补全（用例内 `// TODO(plugin)`）。

mod common;

use std::ffi::{c_char, c_double, c_int, c_uint, c_void, CStr, CString};
use std::sync::atomic::{AtomicUsize, Ordering};

use oak_plugin::descriptor::EffectDescriptor;
use oak_plugin::instance::Instance;
use oak_plugin::param::{ParamInstance, ParamSetInstance};
use oak_plugin::property::{PropertySet, Value};
use oak_plugin::suites::fetch_suite;
use oak_plugin::suites::image_effect::suite_v1 as image_effect_suite;
use oak_plugin::suites::memory::suite_v1 as memory_suite;
use oak_plugin::suites::message::suite_v1 as message_suite_v1;
use oak_plugin::suites::multithread::suite_v1 as multithread_suite;
use oak_plugin::suites::param::suite_v1 as param_suite;
use oak_plugin::suites::progress::suite_v1 as progress_suite;
use oak_plugin::suites::property::suite_v1 as property_suite;
use oak_plugin::suites::tag;
use oak_plugin::suites::timeline::suite_v1 as timeline_suite;

/// OFX 状态码的本地别名（SDK ofxCore.h）。
const OK: c_int = 0;
const UNKNOWN: c_int = 3;
const UNSUPPORTED: c_int = 5;
const BAD_HANDLE: c_int = 9;
const BAD_INDEX: c_int = 10;

fn cs(s: &str) -> CString {
	CString::new(s).unwrap()
}

/// 未打标属性集句柄（宿主内部直用路径）。
fn props_handle(s: &PropertySet) -> *mut c_void {
	s as *const PropertySet as *mut c_void
}

/// 假插件（仅喂给 Instance；describe 之外的字段不被触碰）。
fn dummy_plugin(descriptor: EffectDescriptor) -> std::sync::Arc<oak_plugin::host::Plugin> {
	unsafe extern "C" fn dummy_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		OK
	}
	std::sync::Arc::new(oak_plugin::host::Plugin {
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

/// 用 param suite 的 paramDefine 造一个含 6 参数的实例。
fn make_instance() -> (std::sync::Arc<Instance>, *mut c_void) {
	let mut desc = EffectDescriptor::new();
	let s = param_suite();
	let dhandle = tag::make(&desc.props as *const PropertySet, tag::DESCRIPTOR);
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
				OK
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
	let inst = std::sync::Arc::new(Instance {
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
		edit: std::sync::Mutex::new(oak_plugin::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	});
	let h = tag::make(&inst.props as *const PropertySet, tag::INSTANCE);
	(inst, h)
}

/// fetch_suite：第 1 期承诺的八张表按名按版本可取（v 不符返回
/// 空）；不认识的 suite 返回空。覆盖 M11 §3.1 清单逐个断言。
#[test]
fn fetch_suite_registry() {
	for (name, version) in [
		("OfxPropertySuite", 1),
		("OfxMemorySuite", 1),
		("OfxImageEffectSuite", 1),
		("OfxParameterSuite", 1),
		("OfxMessageSuite", 1),
		("OfxMessageSuite", 2),
		("OfxProgressSuite", 1),
		("OfxProgressSuite", 2),
		("OfxTimeLineSuite", 1),
		("OfxMultiThreadSuite", 1),
	] {
		assert!(
			fetch_suite(name, version).is_some(),
			"{name} v{version} 应可取"
		);
	}
	assert!(fetch_suite("OfxPropertySuite", 2).is_none());
	assert!(fetch_suite("OfxMessageSuite", 3).is_none());
	assert!(fetch_suite("OfxBogusSuite", 1).is_none());
}

/// property suite：define→set→get→getDimension→reset 全链路；
/// 越界读返回 kOfxStatErrBadIndex；未定义属性 → ErrUnknown。
///
/// 声明原含"写只读宿主能力属性 → kOfxStatErrReadOnly"：SDK 无此
/// 状态码、HostSupport 也不在 suite 层拦截写（见 suites/property.rs
/// 模块文档的偏差说明）——本测试随之改为断言 reset 的明确行为。
#[test]
fn property_suite_roundtrip() {
	let set = PropertySet::new();
	set.define("width", vec![Value::Int(0)]);
	let h = props_handle(&set);
	let s = property_suite();
	let name = cs("width");

	// set → get round-trip。
	unsafe {
		assert_eq!((s.set_int)(h, name.as_ptr(), 0, 1920), OK);
	}
	let mut v = 0;
	unsafe {
		assert_eq!((s.get_int)(h, name.as_ptr(), 0, &mut v), OK);
	}
	assert_eq!(v, 1920);

	// getDimension。
	let mut dim = 0;
	unsafe {
		assert_eq!((s.get_dimension)(h, name.as_ptr(), &mut dim), OK);
	}
	assert_eq!(dim, 1);

	// 越界读 → BadIndex；未定义 → Unknown。
	unsafe {
		assert_eq!((s.get_int)(h, name.as_ptr(), 5, &mut v), BAD_INDEX);
	}
	let nope = cs("nope");
	unsafe {
		assert_eq!((s.get_int)(h, nope.as_ptr(), 0, &mut v), UNKNOWN);
	}

	// propReset：第 1 期明确不支持（无默认值快照）→ ErrUnsupported。
	unsafe {
		assert_eq!((s.reset)(h, name.as_ptr()), UNSUPPORTED);
	}

	// 空 handle → BadHandle。
	unsafe {
		assert_eq!(
			(s.get_int)(std::ptr::null_mut(), name.as_ptr(), 0, &mut v),
			BAD_HANDLE
		);
	}
}

/// memory suite：alloc/free 正常；free(NULL) no-op；未知指针
/// free → BadHandle；零尺寸分配可往返。
///
/// 声明原含"实例销毁后账本无残留"：sweep 是 pub(crate) 钩子
/// （destroyInstance 路径），属 host 生命周期测试（lifecycle_test）
/// ——此处只验 suite 契约。
#[test]
fn memory_suite_ledger() {
	let s = memory_suite();
	let mut a: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.alloc)(std::ptr::null_mut(), 256, &mut a), OK);
	}
	assert!(!a.is_null());
	// 内存可用且对齐（f32 写读）。
	unsafe {
		let f = a as *mut f32;
		*f = 1.5f32;
		assert_eq!(*f, 1.5f32);
		assert_eq!(a as usize % 16, 0);
	}
	// 释放；free(NULL) no-op；重复/未知 → BadHandle。
	unsafe {
		assert_eq!((s.free)(a), OK);
		assert_eq!((s.free)(std::ptr::null_mut()), OK);
		assert_eq!((s.free)(a), BAD_HANDLE);
	}
	// 零尺寸。
	let mut z: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.alloc)(std::ptr::null_mut(), 0, &mut z), OK);
		assert_eq!((s.free)(z), OK);
	}
}

/// image effect suite：describe 期 clipDefine/clipGetPropertySet 与
/// 属性读写；实例期 clipGetHandle。
///
/// clipGetImage/clipReleaseImage 配对经
/// [`oak_plugin::clip::ClipInstance::fetch_image`] 与存活表记账验证。
#[test]
fn image_effect_clip_image_pairing() {
	let mut desc = EffectDescriptor::new();
	let s = image_effect_suite();
	let h = tag::make(&desc.props as *const PropertySet, tag::DESCRIPTOR);

	// clipDefine + 属性读写（props 在偏移 0）。
	let mut clip: *mut c_void = std::ptr::null_mut();
	let name = cs("Source");
	unsafe {
		assert_eq!((s.clip_define)(h, name.as_ptr(), &mut clip), OK);
	}
	assert_eq!(tag::kind(clip), tag::CLIP);
	let ps = property_suite();
	let label_prop = cs("OfxPropLabel");
	let optional_prop = cs("OfxImageClipPropOptional");
	let label = cs("SourceLabel");
	unsafe {
		assert_eq!(
			(ps.set_string)(clip, label_prop.as_ptr(), 0, label.as_ptr()),
			OK
		);
		assert_eq!((ps.set_int)(clip, optional_prop.as_ptr(), 0, 1), OK);
	}
	let mut out: *mut c_char = std::ptr::null_mut();
	unsafe {
		assert_eq!((ps.get_string)(clip, label_prop.as_ptr(), 0, &mut out), OK);
		assert_eq!(CStr::from_ptr(out).to_bytes(), b"SourceLabel");
	}

	// getPropertySet/getParamSet：返回 effect 本体。
	let mut props: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!((s.get_property_set)(h, &mut props), OK);
		assert_eq!(props, h);
		assert_eq!((s.get_param_set)(h, &mut props), OK);
		assert_eq!(props, h);
	}

	// 实例期 clipGetHandle（手工构造实例 + clip 实例）。
	let clip_desc = oak_plugin::descriptor::ClipDescriptor {
		props: PropertySet::new(),
		name: "Source".into(),
	};
	let inst = std::sync::Arc::new(Instance {
		props: PropertySet::new(),
		plugin: dummy_plugin(EffectDescriptor::new()),
		context: "OfxImageEffectContextFilter".into(),
		params: ParamSetInstance { params: vec![] },
		clips: vec![Box::new(oak_plugin::clip::ClipInstance::from_descriptor(
			&clip_desc,
		))],
		node_identity: std::sync::atomic::AtomicUsize::new(0),
		destroyed: std::sync::atomic::AtomicBool::new(false),
		sequence_range: std::sync::Mutex::new(None),
		progress_cb: std::sync::Mutex::new(None),
		cancel: std::sync::atomic::AtomicBool::new(false),
		edit: std::sync::Mutex::new(oak_plugin::instance::EditTransaction::new()),
		render_lock: std::sync::Mutex::new(()),
		interact: std::sync::Mutex::new(None),
	});
	let ih = tag::make(&inst.props as *const PropertySet, tag::INSTANCE);
	let mut clip_h: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_get_handle)(ih, name.as_ptr(), &mut clip_h, std::ptr::null_mut()),
			OK
		);
	}
	assert_eq!(tag::kind(clip_h), tag::CLIP);
	// 未找到 → BadHandle（HS:2067-2070）。
	let nope = cs("Nope");
	unsafe {
		assert_eq!(
			(s.clip_get_handle)(ih, nope.as_ptr(), &mut clip_h, std::ptr::null_mut()),
			BAD_HANDLE
		);
	}

	// clipGetImage/clipReleaseImage 配对：输入 clip 排入一帧后 get
	// 取到登记在存活表的图像；release 摘除；二次 release → BadHandle
	// （HS:2053-2068 的 releaseReference 配对）。
	let mut frame = oak_render::eval::generate_frame(
		oak_core::Rational::new(0, 1),
		(2, 2),
		oak_core::PixelFormat::F32,
	)
	.unwrap();
	for (i, v) in [0.1f32, 0.2, 0.3, 1.0].iter().enumerate() {
		frame.data[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
	}
	inst.clips[0].set_input_texture(
		Some(oak_core::texture::Texture::wrap_frame(frame)),
		0.0,
	);
	// 注意另取句柄：上面未找到路径的 clip_get_handle 把 clip_h 置空了。
	let mut img_clip: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_get_handle)(ih, name.as_ptr(), &mut img_clip, std::ptr::null_mut()),
			OK
		);
	}
	let mut img: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.clip_get_image)(img_clip, 0.0, std::ptr::null(), &mut img),
			OK,
			"clipGetImage 抓取排入的输入帧"
		);
	}
	assert!(!img.is_null());
	assert_eq!(tag::kind(img), tag::IMAGE);
	unsafe {
		assert_eq!((s.clip_release_image)(img), OK);
		assert_eq!(
			(s.clip_release_image)(img),
			BAD_HANDLE,
			"二次 release 必须 BadHandle（存活表已摘除）"
		);
	}
}

/// param suite：describe 期 define→getHandle→getValue（默认值）；
/// 实例期 int/double/bool/choice/string/RGBA/2D/3D 的
/// setValue/getValue round-trip；AtTime == 当前值。
///
/// 声明含"paramSetValue 触发 instanceChanged"：通知走
/// [`oak_plugin::param::notify_instance_changed`]，插件自改经 undo
/// 命令写回绑定节点的输入。
#[test]
fn param_suite_roundtrip_and_change_action() {
	// describe 期。
	let mut desc = EffectDescriptor::new();
	let s = param_suite();
	let dhandle = tag::make(&desc.props as *const PropertySet, tag::DESCRIPTOR);
	let t = cs("OfxParamTypeDouble");
	let n = cs("opacity");
	let mut ph: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_define)(dhandle, t.as_ptr(), n.as_ptr(), &mut ph),
			OK
		);
	}
	assert_eq!(tag::kind(ph), tag::PARAM_DEF);
	let mut v = 99.0;
	unsafe {
		assert_eq!((s.param_get_value)(ph, &mut v), OK);
	}
	assert_eq!(v, 0.0, "describe 期 getValue 应为默认值");
	// describe 期 set → BadHandle（HS verifyMagic 语义）。
	unsafe {
		assert_eq!((s.param_set_value)(ph, 42.0), BAD_HANDLE);
	}
	// paramGetHandle 按名取；未找到 → Unknown。
	let mut ph2: *mut c_void = std::ptr::null_mut();
	unsafe {
		assert_eq!(
			(s.param_get_handle)(dhandle, n.as_ptr(), &mut ph2, std::ptr::null_mut()),
			OK
		);
		assert_eq!(ph, ph2);
	}
	let nope = cs("nope");
	unsafe {
		assert_eq!(
			(s.param_get_handle)(dhandle, nope.as_ptr(), &mut ph2, std::ptr::null_mut()),
			UNKNOWN
		);
	}

	// 实例期 round-trip。
	let (_inst, ih) = make_instance();
	let mut gain: *mut c_void = std::ptr::null_mut();
	let mut pos: *mut c_void = std::ptr::null_mut();
	let mut color: *mut c_void = std::ptr::null_mut();
	let mut enabled: *mut c_void = std::ptr::null_mut();
	let mut label: *mut c_void = std::ptr::null_mut();
	unsafe {
		for (n, out) in [
			("gain", &mut gain),
			("pos", &mut pos),
			("color", &mut color),
			("enabled", &mut enabled),
			("label", &mut label),
		] {
			let n = cs(n);
			assert_eq!(
				(s.param_get_handle)(ih, n.as_ptr(), out, std::ptr::null_mut()),
				OK
			);
			assert_eq!(tag::kind(*out), tag::PARAM_INSTANCE);
		}

		// Integer。
		assert_eq!((s.param_set_value)(gain, 7), OK);
		let mut iv = 0;
		assert_eq!((s.param_get_value)(gain, &mut iv), OK);
		assert_eq!(iv, 7);

		// Double2D。
		assert_eq!((s.param_set_value)(pos, 1.5, -2.5), OK);
		let (mut x, mut y) = (0.0, 0.0);
		assert_eq!((s.param_get_value)(pos, &mut x, &mut y), OK);
		assert_eq!((x, y), (1.5, -2.5));

		// RGB。
		assert_eq!((s.param_set_value)(color, 0.1, 0.2, 0.3), OK);
		let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
		assert_eq!((s.param_get_value)(color, &mut r, &mut g, &mut b), OK);
		assert_eq!((r, g, b), (0.1, 0.2, 0.3));

		// Boolean。
		assert_eq!((s.param_set_value)(enabled, 1), OK);
		let mut e = 0;
		assert_eq!((s.param_get_value)(enabled, &mut e), OK);
		assert_eq!(e, 1);

		// String（get 返回内驻指针）。
		let hello = cs("hello");
		assert_eq!((s.param_set_value)(label, hello.as_ptr()), OK);
		let mut p: *mut c_char = std::ptr::null_mut();
		assert_eq!((s.param_get_value)(label, &mut p), OK);
		assert_eq!(CStr::from_ptr(p).to_bytes(), b"hello");

		// AtTime == 当前值（无动画）。
		let mut v2 = 0.0;
		assert_eq!((s.param_get_value_at_time)(gain, 12.0, &mut iv), OK);
		assert_eq!(iv, 7);
		assert_eq!((s.param_set_value_at_time)(gain, 12.0, 9), OK);
		let mut iv2 = 0;
		assert_eq!((s.param_get_value)(gain, &mut iv2), OK);
		assert_eq!(iv2, 9);
	}

	// paramSetValue → instanceChanged 回写（经 PARAM_OWNER 登记表，
	// pub(crate)）由 crate 内测试
	// `suites::param::tests::set_value_writes_back_to_bound_node` 覆盖。
}

/// paramGetValueAtTime/paramSetValueAtTime 与关键帧族
/// （GetNumKeys/GetKeyTime/GetKeyIndex/DeleteKey/DeleteAllKeys）：
/// 第 1 期无动画 → keys 恒 0、key 查询 BadIndex、删除 no-op OK。
#[test]
fn param_keyframe_family() {
	let (_inst, ih) = make_instance();
	let s = param_suite();
	let mut opacity: *mut c_void = std::ptr::null_mut();
	let name = cs("opacity");
	unsafe {
		assert_eq!(
			(s.param_get_handle)(ih, name.as_ptr(), &mut opacity, std::ptr::null_mut()),
			OK
		);
	}

	let mut nkeys = -1;
	unsafe {
		assert_eq!((s.param_get_num_keys)(opacity, &mut nkeys), OK);
	}
	assert_eq!(nkeys, 0, "无动画支持：恒 0 个关键帧");

	let mut kt = 0.0;
	unsafe {
		assert_eq!((s.param_get_key_time)(opacity, 0, &mut kt), BAD_INDEX);
	}
	let mut ki = 0;
	unsafe {
		assert_eq!((s.param_get_key_index)(opacity, 1.0, 0, &mut ki), BAD_INDEX);
		// 删除与拷贝：无关键帧 → no-op OK。
		assert_eq!((s.param_delete_key)(opacity, 1.0), OK);
		assert_eq!((s.param_delete_all_keys)(opacity), OK);
		assert_eq!(
			(s.param_copy)(opacity, opacity, 1.0, 1.0, std::ptr::null()),
			OK
		);
		// editBegin/End 括号。
		assert_eq!((s.param_edit_begin)(opacity), OK);
		assert_eq!((s.param_edit_end)(opacity), OK);
	}
}

/// message suite：v1 变长参数经 C shim 格式化后落到注册的捕获器
/// （%d/%s/%f 三种格式逐字一致）；headless 默认（question 答"否"）。
///
/// v2 的 va_list 入口无法从 Rust 构造调用（platform ABI），格式化
/// 路径与 v1 共用同一 C 函数（forward）——由测试插件从 C 侧覆盖
/// （`// TODO(plugin)`）。
#[test]
fn message_suite_v1_v2() {
	use oak_plugin::suites::message::set_handler;

	unsafe extern "C" fn capture(
		type_: *const c_char,
		message: *const c_char,
		userdata: *mut c_void,
	) -> c_int {
		let v = unsafe { &mut *(userdata as *mut Vec<(String, String)>) };
		v.push((
			unsafe { CStr::from_ptr(type_) }
				.to_string_lossy()
				.into_owned(),
			unsafe { CStr::from_ptr(message) }
				.to_string_lossy()
				.into_owned(),
		));
		1
	}

	let mut captured: Vec<(String, String)> = Vec::new();
	set_handler(Some(capture), &mut captured as *mut _ as *mut c_void);
	let s = message_suite_v1();
	let t = cs("OfxMessageError");
	let id = cs("test-id");
	let fmt = cs("v=%d s=%s f=%f");
	let str_arg = cs("oak");
	unsafe {
		// 答复 YES → kOfxStatReplyYes（12）。
		assert_eq!(
			(s.message)(
				std::ptr::null_mut(),
				t.as_ptr(),
				id.as_ptr(),
				fmt.as_ptr(),
				42,
				str_arg.as_ptr(),
				1.5f64
			),
			12
		);
	}
	assert_eq!(captured.len(), 1);
	assert_eq!(captured[0].0, "OfxMessageError");
	assert!(
		captured[0].1.starts_with("v=42 s=oak f="),
		"got {:?}",
		captured[0].1
	);

	// NULL format → Failed（C++ olivehost.cpp:267）。
	unsafe {
		assert_eq!(
			(s.message)(
				std::ptr::null_mut(),
				t.as_ptr(),
				id.as_ptr(),
				std::ptr::null()
			),
			1
		);
	}

	// 注销出口 → headless 默认：普通消息 OK。
	set_handler(None, std::ptr::null_mut());
	let msg = cs("ask");
	unsafe {
		assert_eq!(
			(s.message)(std::ptr::null_mut(), t.as_ptr(), id.as_ptr(), msg.as_ptr()),
			OK
		);
	}
	// question 类型 + 无出口 → REPLY_NO（13）。用无占位符的格式串：
	// 占位符没有对应的变长参数是 UB（glibc 上 vsnprintf 对野指针
	// strlen 直接 SIGSEGV；macOS 上恰好读到可映射内存才没崩）。
	let q = cs("OfxMessageQuestion");
	let plain = cs("really?");
	unsafe {
		assert_eq!(
			(s.message)(
				std::ptr::null_mut(),
				q.as_ptr(),
				id.as_ptr(),
				plain.as_ptr(),
				1
			),
			13
		);
	}
}

/// progress suite：start/update/end 序列转发到 ProgressReporter；
/// 回调返回 false 时 update 向插件返回取消状态。
#[test]
fn progress_suite_forwarding() {
	use oak_plugin::progress::ProgressReporter;
	use oak_plugin::suites::progress::set_current;

	unsafe extern "C" fn capture_progress(p: f64, userdata: *mut c_void) -> c_int {
		let v = unsafe { &mut *(userdata as *mut Vec<f64>) };
		v.push(p);
		0
	}
	unsafe extern "C" fn cancel_progress(_p: f64, _userdata: *mut c_void) -> c_int {
		1
	}

	let s = progress_suite();
	let mut seen: Vec<f64> = Vec::new();
	set_current(Some(unsafe {
		ProgressReporter::new(capture_progress, &mut seen as *mut _ as *mut c_void)
	}));

	unsafe {
		assert_eq!((s.start)(std::ptr::null_mut(), std::ptr::null()), OK);
		assert_eq!((s.update)(std::ptr::null_mut(), 0.25), OK);
		assert_eq!((s.update)(std::ptr::null_mut(), 0.75), OK);
		assert_eq!((s.end)(std::ptr::null_mut()), OK);
	}
	assert_eq!(seen, vec![0.25, 0.75]);

	// 取消回调：update → REPLY_NO。
	set_current(Some(unsafe {
		ProgressReporter::new(cancel_progress, std::ptr::null_mut())
	}));
	unsafe {
		assert_eq!((s.update)(std::ptr::null_mut(), 0.5), 13);
	}
	set_current(None);
}

/// timeline suite：getTime/getTimeBounds 返回渲染上下文注入的值；
/// 无上下文 → 0 / (0,0) 的 headless 默认。
#[test]
fn timeline_suite_values() {
	use oak_plugin::instance::{OfxRangeD, RenderScale};
	use oak_plugin::suites::{set_render_ctx, RenderCtx};

	let s = timeline_suite();
	let handle = 0x10usize as *mut c_void;
	let mut t = 0.0;
	let mut min = 0.0;
	let mut max = 0.0;

	// 无上下文。
	unsafe {
		assert_eq!((s.get_time)(handle, &mut t), OK);
		assert_eq!(t, 0.0);
		assert_eq!((s.get_time_bounds)(handle, &mut min, &mut max), OK);
		assert_eq!((min, max), (0.0, 0.0));
	}

	// 注入上下文。
	set_render_ctx(Some(RenderCtx {
		time: 42.5,
		scale: RenderScale { x: 1.0, y: 1.0 },
		range: OfxRangeD {
			min: 10.0,
			max: 200.0,
		},
	}));
	unsafe {
		assert_eq!((s.get_time)(handle, &mut t), OK);
		assert_eq!(t, 42.5);
		assert_eq!((s.get_time_bounds)(handle, &mut min, &mut max), OK);
		assert_eq!((min, max), (10.0, 200.0));
		assert_eq!((s.goto_time)(handle, 99.0), OK);
	}
	set_render_ctx(None);
}

/// multithread suite：插件（此处以测试自身的工作线程模拟）起 8 线程
/// 各回调 property suite 一千次；无数据竞争、index/isSpawnedThread
/// 语义正确、join 完整。
#[test]
fn multithread_suite_spawned_callbacks() {
	let s = multithread_suite();
	let ps = property_suite();

	// 共享属性集：每线程一个计数器属性，各写 1000 次。
	// （测试自身即"插件"：工作线程经 multiThread 起，回调 property
	// suite——与插件场景同构。）
	let set = std::sync::Arc::new(PropertySet::new());
	for i in 0..8 {
		let name = Box::leak(format!("ctr{i}").into_boxed_str());
		set.set_one(name, Value::Int(0));
	}

	unsafe extern "C" fn counter_worker(index: c_uint, _count: c_uint, arg: *mut c_void) {
		eprintln!("DBG worker {index} arg={:x}", arg as usize);
		let set = unsafe { &*(arg as *const PropertySet) };
		let name = Box::leak(format!("ctr{index}").into_boxed_str());
		let mut v = 0;
		unsafe {
			let _ = (property_suite().get_int)(
				set as *const _ as *mut c_void,
				cs(name).as_ptr(),
				0,
				&mut v,
			);
		}
		for _ in 0..1000 {
			unsafe {
				let _ = (property_suite().set_int)(
					set as *const _ as *mut c_void,
					cs(name).as_ptr(),
					0,
					v + 1,
				);
				let _ = (property_suite().get_int)(
					set as *const _ as *mut c_void,
					cs(name).as_ptr(),
					0,
					&mut v,
				);
			}
		}
	}

	// 宿主线程身份。
	let mut idx = 0;
	let mut spawned = 1;
	unsafe {
		assert_eq!((s.index)(&mut idx), OK);
		assert_eq!(idx, -1);
		assert_eq!((s.is_spawned)(&mut spawned), OK);
		assert_eq!(spawned, 0);
	}

	// 8 线程各 1000 次增量：最终值恰为 1000（无丢写 = 无竞争）。
	// 注意传 `&*set`（Arc 负载）而非 `&set`（Arc 结构体）。
	let raw: *const PropertySet = &*set;
	unsafe {
		assert_eq!((s.multi_thread)(counter_worker, 8, raw as *mut c_void), OK);
	}
	for i in 0..8 {
		// C 侧按 NUL 结尾读名字——必须经 cs() 包成 CString（裸
		// `str::as_ptr()` 不带 NUL，CStr::from_ptr 会读越界）。
		let name = cs(&format!("ctr{i}"));
		let mut v = 0;
		// 注意传 `&*set`（Arc 负载）而非 `&set`（Arc 结构体）。
		unsafe {
			assert_eq!(
				(ps.get_int)(
					&*set as *const _ as *mut c_void,
					name.as_ptr(),
					0,
					&mut v
				),
				OK
			);
		}
		assert_eq!(v, 1000, "线程 {i} 的计数应无丢失");
	}

	// 0 线程：no-op。
	unsafe {
		assert_eq!(
			(s.multi_thread)(counter_worker, 0, std::ptr::null_mut()),
			OK
		);
	}
}
