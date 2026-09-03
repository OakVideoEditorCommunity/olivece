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

//! OfxImageEffectSuite v1：clip/image 操作。
//!
//! 语义对照 HS: ofxhImageEffect.cpp：
//! - clipGetImage 返回图像 **属性集** handle（HS:2038 `getPropHandle`），
//!   图像存活由宿主表托管（get → 插入强引用，clipReleaseImage → 摘除
//!   释放；对应 HS 的引用计数配对，HS:2044）；
//! - clipGetRegionOfDefinition：读 clip 实例属性里的
//!   kOfxImageEffectPropRegionOfDefinition（协商时宿主写入）；非法
//!   RoD → kOfxStatFailed（HS:2143-2147）；
//! - abort：实例期 → 当前渲染的取消状态（HS:2154-2170；契约是
//!   0 = 继续、非 0 = 中止——REPLY_YES/NO 会让 ofxs 的
//!   `abort(...) != 0` 判断误判中止，像素循环一行不写 → 黑帧）；
//! - imageMemory*：账本同 memory suite（HS 的 lock 是"锁住防重分配"
//!   语义，第 1 期账本不需要 → OK no-op，见 memory.rs 文档）。
//!
//! `// TODO(clip)`：clipGetImage 依赖 [`crate::clip::ClipInstance::fetch_image`]
//! （oakrender 帧访问随单库化改为本地桩，见 [`crate::render`]），代码已齐、运行时待 clip.rs。

use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::clip::ClipInstance;
use crate::descriptor::{ClipDescriptor, EffectDescriptor};
use crate::image::Image;
use crate::instance::{Instance, OfxRectD, RenderScale};
use crate::property::PropertySet;
use crate::suites::{status, tag};

/// 函数表布局（与 SDK `OfxImageEffectSuiteV1` 逐字段一致；常用子集
/// 注释，完整字段以 SDK 为准）。
#[repr(C)]
pub struct ImageEffectSuiteV1 {
	/// getPropertySet：取 effect/clip/image 的属性集。
	pub get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// getParamSet：取实例参数集。
	pub get_param_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// clipDefine（describe 期间）：定义 clip。
	pub clip_define: unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> c_int,
	/// clipGetHandle：按名取 clip。
	pub clip_get_handle: unsafe extern "C" fn(
		*mut c_void,
		*const c_char,
		*mut *mut c_void,
		*mut *mut c_void,
	) -> c_int,
	/// clipGetPropertySet
	pub clip_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// clipGetImage：取图像（host 侧增加引用，必须与
	/// clipReleaseImage 配对）。
	pub clip_get_image:
		unsafe extern "C" fn(*mut c_void, c_double, *const c_void, *mut *mut c_void) -> c_int,
	/// clipReleaseImage
	pub clip_release_image: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// clipGetRegionOfDefinition
	pub clip_get_region_of_definition:
		unsafe extern "C" fn(*mut c_void, c_double, *mut c_void) -> c_int,
	/// abort：查询是否应中止（0 = 继续，非 0 = 中止；取消透传）。
	pub abort: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// imageMemoryAlloc / imageMemoryFree / imageMemoryLock /
	/// imageMemoryUnlock：图像内存管理（账本同 memory suite）。
	pub image_memory_alloc: unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_void) -> c_int,
	/// imageMemoryFree
	pub image_memory_free: unsafe extern "C" fn(*mut c_void) -> c_int,
	/// imageMemoryLock
	pub image_memory_lock: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
	/// imageMemoryUnlock
	pub image_memory_unlock: unsafe extern "C" fn(*mut c_void) -> c_int,
}

/// 存活图像表：clipGetImage 产出（props 地址 → 强引用；唯一持有者，
/// clipReleaseImage 摘除即释放——对应 HS 的 get/release 配对）。
static LIVE_IMAGES: std::sync::LazyLock<Mutex<HashMap<usize, std::sync::Arc<Image>>>> =
	std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 公共入口模板：panic 兜底。
#[track_caller]
fn caught(f: impl FnOnce() -> Result<(), c_int>) -> c_int {
	let caller = std::panic::Location::caller();
	let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_or_else(
		|_| status::FAILED,
		|r| r.map_or_else(|c| c, |()| status::OK),
	);
	if code != status::OK && std::env::var_os("OAK_OFX_TRACE").is_some() {
		eprintln!("[ofx] image-effect suite error {code} at {caller}");
	}
	if std::env::var_os("OAK_OFX_TRACE").is_some() {
		eprintln!("[ofx] image-effect suite call at {caller} -> {code}");
	}
	code
}

/// 属性名（空指针/非 UTF-8 → ErrValue）。
unsafe fn c_name<'a>(name: *const c_char) -> Result<&'a str, c_int> {
	if name.is_null() {
		return Err(status::ERR_VALUE);
	}
	unsafe { CStr::from_ptr(name) }
		.to_str()
		.map_err(|_| status::ERR_VALUE)
}

/// effect 句柄解析（describe 期 → 描述符；实例期 → 实例）。
enum EffectRef<'a> {
	Descriptor(&'a mut EffectDescriptor),
	Instance(&'a Instance),
}

fn resolve_effect(handle: *mut c_void) -> Result<EffectRef<'static>, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	// 两者 props 均在偏移 0（句柄约定）。
	unsafe {
		match tag::kind(handle) {
			tag::DESCRIPTOR => Ok(EffectRef::Descriptor(
				&mut *(tag::strip(handle) as *mut EffectDescriptor),
			)),
			tag::INSTANCE => Ok(EffectRef::Instance(
				&*(tag::strip(handle) as *const Instance),
			)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// clip 句柄解析（实例期；describe 期 ClipDescriptor 只在属性
/// suite 里用，不走这里）。
fn resolve_clip(handle: *mut c_void) -> Result<&'static ClipInstance, c_int> {
	if handle.is_null() {
		return Err(status::ERR_BAD_HANDLE);
	}
	unsafe {
		match tag::kind(handle) {
			tag::CLIP => Ok(&*(tag::strip(handle) as *const ClipInstance)),
			_ => Err(status::ERR_BAD_HANDLE),
		}
	}
}

/// getPropertySet：effect 属性集 = handle 本体（props 在偏移 0；
/// HS:1886-1898 返回 `getProps().getHandle()`，同址）。
unsafe extern "C" fn get_property_set(effect: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_effect(effect)?;
		unsafe { *out = effect };
		Ok(())
	})
}

/// getParamSet：param-set 即 effect 本体（HS:1901-1931 描述符/实例
/// 各自返回其 param set；本设计两者合一）。
unsafe extern "C" fn get_param_set(effect: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_effect(effect)?;
		unsafe { *out = effect };
		Ok(())
	})
}

/// clipDefine（describe 期）：定义 clip 并返回其属性集 handle。
/// 重复名 → 整体替换（HS: `defineClip` 的 map 覆盖语义，
/// ofxhImageEffect.cpp:265-271；旧句柄随之失效，与 HS 一致）。
unsafe extern "C" fn clip_define(
	effect: *mut c_void,
	name: *const c_char,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *out = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let desc = match resolve_effect(effect)? {
			EffectRef::Descriptor(d) => d,
			EffectRef::Instance(_) => return Err(status::ERR_BAD_HANDLE),
		};
		let clip = ClipDescriptor::new(n);
		// 重复定义：替换原 Box（HS map 覆盖语义）。
		if let Some(existing) = desc.clips.iter_mut().find(|c| c.name == n) {
			*existing = Box::new(clip);
		} else {
			desc.clips.push(Box::new(clip));
		}
		// 地址取自已入盒的对象（栈上临时变量在移动后失效）。
		let clip = desc
			.clips
			.iter()
			.find(|c| c.name == n)
			.expect("just stored");
		let addr = &clip.props as *const _ as usize;
		unsafe { *out = tag::make(addr as *const PropertySet, tag::CLIP) };
		Ok(())
	})
}

/// clipGetHandle（实例期）：按名取 clip 及其属性集。未找到 →
/// BadHandle（HS:2067-2070）。
unsafe extern "C" fn clip_get_handle(
	effect: *mut c_void,
	name: *const c_char,
	clip: *mut *mut c_void,
	property_set: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if clip.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *clip = std::ptr::null_mut() };
		let n = unsafe { c_name(name)? };
		let handle = match resolve_effect(effect)? {
			EffectRef::Instance(i) => {
				let c = i
					.clips
					.iter()
					.find(|c| c.name == n)
					.ok_or(status::ERR_BAD_HANDLE)?;
				let addr = &c.props as *const _ as usize;
				tag::make(addr as *const PropertySet, tag::CLIP)
			}
			EffectRef::Descriptor(_) => return Err(status::ERR_BAD_HANDLE),
		};
		unsafe { *clip = handle };
		if !property_set.is_null() {
			// props 在偏移 0：clip handle 与 props handle 同值。
			unsafe { *property_set = handle };
		}
		Ok(())
	})
}

/// clipGetPropertySet：clip 属性集 = clip handle 本体。
unsafe extern "C" fn clip_get_property_set(clip: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let _ = resolve_clip(clip)?;
		unsafe { *out = clip };
		Ok(())
	})
}

/// clipGetImage：抓取输入图像并登记到存活表，返回图像属性集 handle
/// （HS:2003-2049；`getImage` 失败 → Failed）。
///
/// `// TODO(clip)`：fetch_image 待 clip 迁移到
/// `oak_core::texture::Texture` 值模型（当前帧访问为本地桩）。
unsafe extern "C" fn clip_get_image(
	clip: *mut c_void,
	time: c_double,
	region: *const c_void,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		unsafe { *out = std::ptr::null_mut() };
		let c = resolve_clip(clip)?;
		// Output clip：返回当前渲染的输出图像（render 驱动经 TLS
		// 设置；对应 HS 渲染期输出图像挂在 Output clip 上）。
		if c.name == "Output" {
			if let Some(image) = crate::suites::current_output() {
				let addr = &image.props as *const _ as usize;
				LIVE_IMAGES
					.lock()
					.unwrap_or_else(|e| e.into_inner())
					.insert(addr, image);
				unsafe { *out = tag::make(addr as *const PropertySet, tag::IMAGE) };
				return Ok(());
			}
			return Err(status::FAILED);
		}
		// 区域（可空）：像素坐标的可选 bounds。
		let region = if region.is_null() {
			None
		} else {
			Some(unsafe { *(region as *const OfxRectD) })
		};
		// 渲染比例取自当前渲染上下文（HS 的 clip 在 render 期从
		// in_args 拿到 scale；本设计经 TLS，见 suites::RenderCtx）。
		let scale = crate::suites::render_ctx()
			.map(|ctx| ctx.scale)
			.unwrap_or(RenderScale { x: 1.0, y: 1.0 });
		let image = c
			.fetch_image(time, scale, region)
			.map_err(|_| status::FAILED)?;
		if std::env::var_os("OAK_OFX_TRACE").is_some() {
			let p = image.pixels();
			let dump: Vec<f32> = p
				.chunks_exact(4)
				.take(4)
				.map(|c| f32::from_le_bytes(c.try_into().unwrap()))
				.collect();
			eprintln!("[ofx] clipGetImage({}) fetched pixels[0..4] = {dump:?}", c.name);
		}
		let image = std::sync::Arc::new(image);
		let addr = &image.props as *const _ as usize;
		LIVE_IMAGES
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(addr, image);
		unsafe { *out = tag::make(addr as *const PropertySet, tag::IMAGE) };
		Ok(())
	})
}

/// clipReleaseImage：摘除存活表强引用（图像随之释放；HS:2053-2068
/// 的 releaseReference 配对）。
unsafe extern "C" fn clip_release_image(image: *mut c_void) -> c_int {
	caught(|| {
		if image.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		if tag::kind(image) != tag::IMAGE {
			return Err(status::ERR_BAD_HANDLE);
		}
		let addr = tag::strip(image) as usize;
		let mut live = LIVE_IMAGES.lock().unwrap_or_else(|e| e.into_inner());
		match live.remove(&addr) {
			Some(_) => Ok(()),
			None => Err(status::ERR_BAD_HANDLE),
		}
	})
}

/// clipGetRegionOfDefinition：读 clip 实例属性的协商 RoD；缺失/非法
/// → Failed（HS:2111-2150；非法判断 x2<x1 || y2<y1 → Failed）。
unsafe extern "C" fn clip_get_region_of_definition(
	clip: *mut c_void,
	_time: c_double,
	bounds: *mut c_void,
) -> c_int {
	caught(|| {
		if bounds.is_null() {
			return Err(status::ERR_BAD_HANDLE);
		}
		let c = resolve_clip(clip)?;
		let rod = read_rod(&c.props).ok_or(status::FAILED)?;
		if rod.x2 < rod.x1 || rod.y2 < rod.y1 {
			return Err(status::FAILED);
		}
		unsafe { *(bounds as *mut OfxRectD) = rod };
		Ok(())
	})
}

/// 从 clip 属性读协商 RoD（kOfxImageEffectPropRegionOfDefinition，
/// Double×4；协商时宿主写入）。
fn read_rod(props: &PropertySet) -> Option<OfxRectD> {
	let v = props.get("OfxImageEffectPropRegionOfDefinition", 0)?;
	let Value::Double(x1) = v else { return None };
	let Value::Double(y1) = props.get("OfxImageEffectPropRegionOfDefinition", 1)? else {
		return None;
	};
	let Value::Double(x2) = props.get("OfxImageEffectPropRegionOfDefinition", 2)? else {
		return None;
	};
	let Value::Double(y2) = props.get("OfxImageEffectPropRegionOfDefinition", 3)? else {
		return None;
	};
	Some(OfxRectD { x1, y1, x2, y2 })
}

/// abort：查询是否应中止渲染。OFX 契约（ofxImageEffect.h）与
/// ofxs 实现（ofxsImageEffect.cpp `abort(...) != 0`）都是
/// **0 = 继续、非 0 = 中止**——不能回 REPLY_YES/NO（13 会被
/// openfx-misc 理解成"立即中止"，像素循环一行都不写 → 黑帧）。
/// 取消状态是成功应答而非错误码，不能走 caught 的 Err 通道。
unsafe extern "C" fn abort(effect: *mut c_void) -> c_int {
	catch_unwind(AssertUnwindSafe(|| {
		let inst = match resolve_effect(effect)? {
			EffectRef::Instance(i) => i,
			EffectRef::Descriptor(_) => return Err(status::ERR_BAD_HANDLE),
		};
		Ok(inst)
	}))
	.map_or_else(
		|_| status::FAILED,
		|r| match r {
			Ok(inst) => {
				let cancelled = inst.cancel.load(std::sync::atomic::Ordering::Relaxed)
					|| crate::suites::progress::is_cancelled();
				if cancelled { 1 } else { 0 }
			}
			Err(c) => c,
		},
	)
}

/// imageMemoryAlloc：账本同 memory suite（HS:2174-2212；失败 →
/// ErrMemory）。`handle` 忽略。
unsafe extern "C" fn image_memory_alloc(
	_handle: *mut c_void,
	byte_size: c_int,
	out: *mut *mut c_void,
) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		match crate::suites::memory::alloc(byte_size as usize) {
			Some(ptr) => {
				unsafe { *out = ptr as *mut c_void };
				Ok(())
			}
			None => Err(status::ERR_MEMORY),
		}
	})
}

/// imageMemoryFree：账本销账；未知指针 → BadHandle（SDK 契约）。
unsafe extern "C" fn image_memory_free(memory: *mut c_void) -> c_int {
	caught(|| {
		if crate::suites::memory::free(memory as *mut u8) {
			Ok(())
		} else {
			Err(status::ERR_BAD_HANDLE)
		}
	})
}

/// imageMemoryLock/Unlock：HS 语义是"锁住防重分配"（ofxhMemory.cpp
/// lock/unlock 计数），第 1 期账本不建模 → OK no-op（文档见
/// memory.rs）。
unsafe extern "C" fn image_memory_lock(memory: *mut c_void, out: *mut *mut c_void) -> c_int {
	caught(|| {
		if out.is_null() {
			return Err(status::ERR_VALUE);
		}
		// 账本里的地址即数据指针（分配即就绪，无 HS 的延迟分配）。
		unsafe { *out = memory };
		Ok(())
	})
}

unsafe extern "C" fn image_memory_unlock(_memory: *mut c_void) -> c_int {
	caught(|| Ok(()))
}

use crate::property::Value;

/// 静态函数表实例。
pub fn suite_v1() -> &'static ImageEffectSuiteV1 {
	static SUITE: std::sync::OnceLock<ImageEffectSuiteV1> = std::sync::OnceLock::new();
	SUITE.get_or_init(|| ImageEffectSuiteV1 {
		get_property_set: get_property_set,
		get_param_set: get_param_set,
		clip_define: clip_define,
		clip_get_handle: clip_get_handle,
		clip_get_property_set: clip_get_property_set,
		clip_get_image: clip_get_image,
		clip_release_image: clip_release_image,
		clip_get_region_of_definition: clip_get_region_of_definition,
		abort: abort,
		image_memory_alloc: image_memory_alloc,
		image_memory_free: image_memory_free,
		image_memory_lock: image_memory_lock,
		image_memory_unlock: image_memory_unlock,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;

	use crate::descriptor::EffectDescriptor;
	use crate::property::PropertySet;

	fn cs(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	fn descriptor_handle(d: &EffectDescriptor) -> *mut c_void {
		tag::make(&d.props as *const PropertySet, tag::DESCRIPTOR)
	}

	/// describe 期：clipDefine 建 clip、属性 suite 读写其属性、
	/// 重复定义整体替换（HS map 覆盖）。
	#[test]
	fn describe_clip_define_and_props() {
		let mut desc = EffectDescriptor::new();
		let s = suite_v1();
		let h = descriptor_handle(&desc);

		let mut clip: *mut c_void = std::ptr::null_mut();
		let name = cs("Source");
		unsafe {
			assert_eq!((s.clip_define)(h, name.as_ptr(), &mut clip), 0);
		}
		assert_eq!(tag::kind(clip), tag::CLIP);

		// 属性 suite 直接读写 clip handle（props 在偏移 0）。
		let ps = crate::suites::property::suite_v1();
		let label_prop = cs(crate::param::PROP_LABEL);
		let optional_prop = cs(crate::descriptor::CLIP_OPTIONAL);
		let label = cs("SourceLabel");
		unsafe {
			assert_eq!(
				(ps.set_string)(clip, label_prop.as_ptr(), 0, label.as_ptr()),
				0
			);
		}
		let mut out: *mut c_char = std::ptr::null_mut();
		unsafe {
			assert_eq!((ps.get_string)(clip, label_prop.as_ptr(), 0, &mut out), 0);
			assert_eq!(CStr::from_ptr(out).to_bytes(), b"SourceLabel");
			// 可选性标记：插件写 Optional=1。
			assert_eq!((ps.set_int)(clip, optional_prop.as_ptr(), 0, 1), 0);
		}

		// getPropertySet / getParamSet：返回 effect 本体 handle。
		let mut props: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.get_property_set)(h, &mut props), 0);
			assert_eq!(props, h);
			assert_eq!((s.get_param_set)(h, &mut props), 0);
			assert_eq!(props, h);
		}

		// 重复 clipDefine：替换（旧句柄失效，HS 一致）。
		let mut clip2: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!((s.clip_define)(h, name.as_ptr(), &mut clip2), 0);
		}
		assert_ne!(clip, clip2);
		assert_eq!(desc.clips.len(), 1);

		// 空 out / 空 handle → BadHandle。
		unsafe {
			assert_eq!(
				(s.clip_define)(h, name.as_ptr(), std::ptr::null_mut()),
				status::ERR_BAD_HANDLE
			);
			assert_eq!(
				(s.clip_define)(std::ptr::null_mut(), name.as_ptr(), &mut clip2),
				status::ERR_BAD_HANDLE
			);
		}
	}

	/// abort：describe 期 → BadHandle；实例期 → 0（未取消）。
	#[test]
	fn abort_requires_instance() {
		let desc = EffectDescriptor::new();
		let s = suite_v1();
		let dh = descriptor_handle(&desc);
		unsafe {
			assert_eq!((s.abort)(dh), status::ERR_BAD_HANDLE);
			assert_eq!((s.abort)(std::ptr::null_mut()), status::ERR_BAD_HANDLE);
		}
		// 实例期：未取消 → 0（OFX 契约：0 = 继续，非 0 = 中止）。
		let inst = std::sync::Arc::new(crate::instance::Instance {
			props: PropertySet::new(),
			plugin: std::sync::Arc::new(crate::host::Plugin {
				identifier: "t".into(),
				version: (1, 0),
				bundle_path: std::path::PathBuf::new(),
				contexts: vec![],
				descriptor: EffectDescriptor::new(),
				lib: std::ptr::null_mut(),
				entry: dummy_entry,
				ofx_plugin: std::ptr::null_mut(),
				unloaded: std::sync::atomic::AtomicBool::new(false),
			}),
			context: "OfxImageEffectContextFilter".into(),
			params: crate::param::ParamSetInstance { params: vec![] },
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
		let ih = tag::make(&inst.props as *const PropertySet, tag::INSTANCE);
		unsafe {
			assert_eq!((s.abort)(ih), 0);
		}
	}

	unsafe extern "C" fn dummy_entry(
		_: *const c_char,
		_: *const c_void,
		_: *mut c_void,
		_: *mut c_void,
	) -> c_int {
		status::OK
	}

	/// imageMemoryAlloc/Free 走 memory 账本。
	#[test]
	fn image_memory_ledger() {
		let s = suite_v1();
		let mut mem: *mut c_void = std::ptr::null_mut();
		unsafe {
			assert_eq!(
				(s.image_memory_alloc)(std::ptr::null_mut(), 1024, &mut mem),
				0
			);
			assert!(!mem.is_null());
			// lock 返回数据指针（第 1 期 = 句柄本身）。
			let mut ptr: *mut c_void = std::ptr::null_mut();
			assert_eq!((s.image_memory_lock)(mem, &mut ptr), 0);
			assert_eq!(ptr, mem);
			assert_eq!((s.image_memory_unlock)(mem), 0);
			// 未知指针 free → BadHandle。
			assert_eq!(
				(s.image_memory_free)(0xdeadbeef as *mut c_void),
				status::ERR_BAD_HANDLE
			);
			assert_eq!((s.image_memory_free)(mem), 0);
		}
	}
}
