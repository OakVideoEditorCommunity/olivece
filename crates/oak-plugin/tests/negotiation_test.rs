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

//! 协商语义专项：clip 偏好协商矩阵、RoD/RoI、isIdentity、field
//! 透传。当年调试重灾区，每条对照 HostSupport 行为（注释标行号）。
//!
//! 最小测试插件固定声明 RGBA/F32、实现 RoD（1920×1080）与 RoI
//! （回写 region）、isIdentity 恒非透传、无 field——需要插件变体
//! 的矩阵案（回退链、身份透传、field）标记 `// TODO(plugin)`，
//! 随 M11 §2.4 测试插件的变体落地。
//!
//! 单库化后经 Rust API 直连（`Host::global().create_instance`），不再
//! 经已删除的 `oakplugin_host_init`/`oakplugin_instance_create` C ABI。

mod common;

use std::sync::Arc;

use oak_plugin::handle::RefBox;
use oak_plugin::host::Host;
use oak_plugin::instance::Instance;

const TEST_PLUGIN_ID: &str = "org.oak.test-plugin";

/// 扫描并创建实例；不可用返回 None（skip）。
fn create_instance() -> Option<Arc<RefBox<Instance>>> {
	if common::test_plugin_scan_dir().is_none() {
		common::skip("最小测试插件未构建");
		return None;
	}
	let dir = common::test_plugin_scan_dir().unwrap();
	let host = Host::global();
	let _ = host.cache.scan();
	host.cache.scan_path(&dir).ok()?;
	host.create_instance(TEST_PLUGIN_ID, None).ok()
}

/// 协商顺序：测试插件声明 RGBA/F32、帧率 24——协商结果必须
/// 原样采纳（HS: ofxhImageEffect.cpp:1686-1740 的回灌路径）。
/// 全组合矩阵（RGBA/RGB/Alpha × Float/Half/Byte）需要插件变体：
/// `// TODO(plugin)`。
#[test]
fn clip_preferences_component_depth_matrix() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().expect("协商应成功");
		assert_eq!(prefs.output_components, "OfxImageComponentRGBA");
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		assert_eq!(prefs.frame_rate, 24.0);
		// 回灌：clip 实例属性带上了协商结果（clipGetPropertySet 读）。
		let output = inst
			.value
			.clips
			.iter()
			.find(|c| c.name == "Output")
			.unwrap();
		assert_eq!(
			output
				.props
				.get("OfxImageEffectPropComponents", 0)
				.map(|v| format!("{v:?}")),
			Some("String(\"OfxImageComponentRGBA\")".into())
		);
		Host::global().shutdown();
	});
}

/// 位深回退链（Half→Byte）：宿主当前只宣告 F32 支持、插件声明
/// F32——直通。回退链的插件侧变体 `// TODO(plugin)`。
#[test]
fn bit_depth_fallback_chain() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().unwrap();
		assert_eq!(prefs.output_bit_depth, "OfxBitDepthFloat");
		Host::global().shutdown();
	});
}

/// RoD：插件实现 getRegionOfDefinition → 用插件值（1920×1080，
/// HS: ofxhImageEffect.cpp:1087-1130 的 out args 读取）。
/// "未实现时用输入 RoD 并集"的默认语义需要插件变体：`// TODO(plugin)`。
#[test]
fn region_of_definition_default_and_override() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let rod = inst
			.value
			.get_region_of_definition(0.0, oak_plugin::instance::RenderScale { x: 1.0, y: 1.0 })
			.expect("getRoD 应成功");
		assert_eq!((rod.x1, rod.y1, rod.x2, rod.y2), (0.0, 0.0, 1920.0, 1080.0));
		Host::global().shutdown();
	});
}

/// RoI：插件把 region 原样写回 Source 的 per-clip 属性
/// （"OfxImageEffectPropRegionOfInterest_Source"）；返回值与
/// 请求 region 一致（HS: getRegionsOfInterest 的 per-clip 前缀）。
#[test]
fn regions_of_interest_writeback() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let region = oak_plugin::instance::OfxRectD {
			x1: 10.0,
			y1: 20.0,
			x2: 100.0,
			y2: 200.0,
		};
		let rois = inst
			.value
			.get_regions_of_interest(
				0.0,
				oak_plugin::instance::RenderScale { x: 1.0, y: 1.0 },
				region,
			)
			.expect("getRoI 应成功");
		assert_eq!(rois.len(), 2, "clips 数与 RoI 数一致");
		// RoI 只对输入 clip 有意义：Source 回写 region；Output 是
		// 宿主预定义的默认（零），渲染驱动只用输入条目。
		let source = rois
			.get(
				inst.value
					.clips
					.iter()
					.position(|c| c.name == "Source")
					.unwrap(),
			)
			.unwrap();
		assert_eq!(
			(source.x1, source.y1, source.x2, source.y2),
			(10.0, 20.0, 100.0, 200.0)
		);
		Host::global().shutdown();
	});
}

/// isIdentity：测试插件恒非透传 → None（render 必须真正执行）。
/// 透传短路（Some）需要插件变体：`// TODO(plugin)`。
#[test]
fn is_identity_shortcircuit() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let identity = inst.value.is_identity(1.5).expect("isIdentity 应成功");
		assert!(identity.is_none(), "测试插件恒非透传：{identity:?}");
		Host::global().shutdown();
	});
}

/// field 透传：插件声明的 field order（OfxFieldNone）原样透传到
/// 协商结果（宿主只透传不处理）。多值矩阵 `// TODO(plugin)`。
#[test]
fn field_passthrough() {
	common::with_host(|| {
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let prefs = inst.value.get_clip_preferences().unwrap();
		assert_eq!(prefs.field, "OfxFieldNone", "插件声明的 field 应透传");
		Host::global().shutdown();
	});
}

/// begin/endSequenceRender：配对调用成功；begin 后 timeline
/// getTimeBounds 返回该序列范围（sequence_range 接线）。
#[test]
fn sequence_render_brackets() {
	common::with_host(|| {
		use oak_plugin::instance::OfxRangeD;
		let Some(inst) = create_instance() else {
			Host::global().shutdown();
			return;
		};
		let range = OfxRangeD {
			min: 10.0,
			max: 200.0,
		};
		assert!(inst.value.begin_sequence_render(range).is_ok());
		// begin 后实例记录该序列范围（timeline 上下文的 getTimeBounds
		// 读它）。
		let stored = *inst.value.sequence_range.lock().unwrap();
		let stored = stored.expect("begin_sequence_render 必须登记范围");
		assert_eq!((stored.min, stored.max), (10.0, 200.0));
		assert!(inst.value.end_sequence_render(range).is_ok());
		// end 后清除。
		assert!(inst.value.sequence_range.lock().unwrap().is_none());

		Host::global().shutdown();
	});
}
