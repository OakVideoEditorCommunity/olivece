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

//! Golden master：描述符快照 diff 与渲染帧比对（M11 §2.2/§2.3）。
//!
//! 快照在 0 期由**现行 C++ 实现**抓取入库（tests/ofx/snapshots/ 与
//! tests/ofx/frames/）；本文件的测试断言 Rust 实现与之逐字段/逐像素
//! 一致。**0 期基建尚未落地**（tests/ofx/ 目录为空）——依赖快照/
//! 帧库的用例一律 `#[ignore]`，落地后摘掉并补全断言（见
//! [`descriptor_snapshots_match`] 的说明）。无真实 bundle 的环境
//! （CI）整文件 skip。
//!
//! 本文件当前没有实际执行的用例：渲染/GL/色彩链路本身已实现并经
//! `gl_render_test.rs`/`colour_test.rs` 覆盖，这里缺的只是 golden
//! 参照物。

mod common;

/// 描述符快照：对快照清单中的每个插件，字段级 diff——标识、版本、
/// 上下文集合、参数矩阵（名称/类型/默认值/标签/hint/父组/secret/
/// display range/choice labels+values+排序结果）、clip 表。任何 diff
/// 打印成 unified diff 供定位。
///
/// # ignore 原因（M11 0 期基建缺失）
///
/// `tests/ofx/snapshots/`（C++ 实现抓取的描述符 JSON 库）尚未生成。
/// 快照落地后摘掉本注解并实现：扫描快照清单 → 每插件 describe →
/// 与 JSON 逐字段比对。当前无快照可比对，硬跑必假红。
#[test]
#[ignore = "M11 0 期快照库（tests/ofx/snapshots/）尚未生成；落地后实现并摘除"]
fn descriptor_snapshots_match() {
	todo!("0 期快照落地后实现：扫描快照清单 → describe → 逐字段 diff")
}

/// clip 协商快照：getClipPreferences 之后的输出分量/位深/像素比/
/// 帧率/field 与快照一致（隐式行为的主要回归防线）。
///
/// # ignore 原因
///
/// 同 [`descriptor_snapshots_match`]：协商快照（含 getClipPreferences
/// 协商后结果）在 `tests/ofx/snapshots/`，尚未生成。
#[test]
#[ignore = "M11 0 期协商快照尚未生成；落地后实现并摘除"]
fn negotiated_clip_preferences_match() {
	todo!("0 期协商快照落地后实现")
}

/// CImg 全量插件冒烟：每个插件 describe + 协商不崩、不泄漏
/// （不比对快照，只验健壮性——覆盖快照集之外的怪癖）。
///
/// # ignore 原因（本机 CImg bundle 的宿主兼容缺口）
///
/// 本机安装的 CImg.ofx.bundle 是 Natron 分支构建：describe 期间写
/// 专属属性 `NatronOfxImageEffectPropDeprecated`，且其 OFX support
/// 的 `Property::Set` 句柄语义与本 crate 的打标句柄约定不兼容
/// （插件侧属性写入在进入本宿主属性套件前失败 → describe 返回
/// kOfxStatErrMissingHostFeature）。真实插件的端到端加载/建实例/
/// 协商路径由 Misc.ofx.bundle（标准 openfx-misc 构建）冒烟覆盖。
/// CImg 兼容属 M11 §3.5「CImg 全量 describe/协商冒烟」的宿主保真
/// 缺口——属性套件适配插件侧 `Property::Set` 句柄后摘除本注解。
#[test]
#[ignore = "本机 CImg.ofx.bundle 为 Natron 分支构建，属性套件句柄语义不兼容（describe 返回 MissingHostFeature）；Misc 系统冒烟已覆盖真实插件路径"]
fn cimg_full_describe_smoke() {
	todo!("属性套件适配 Natron 分支的 Property::Set 句柄后实现：全量 describe + 协商不崩、不泄漏")
}

/// CPU 渲染 golden：代表性效果集逐帧 SHA256 一致（bit 级）。
/// 输入帧为合成渐变+色块（确定性生成器在 common 里）。
///
/// # ignore 原因
///
/// `tests/ofx/frames/`（C++ 实现抓取的 EXR + SHA256 参照库）尚未生成
/// ——渲染链路本身已是真实现（render_driver + oak-render eval），
/// 缺的只是 golden 参照物。参照库落地后实现并摘除。
#[test]
#[ignore = "M11 0 期渲染 golden 参照库（tests/ofx/frames/）尚未生成；落地后实现并摘除"]
fn render_golden_cpu_bitexact() {
	todo!("0 期渲染 golden 落地后实现")
}

/// GL 渲染 golden：1e-4 容差（驱动差异）；无 GPU 环境 skip。
///
/// # ignore 原因
///
/// GL 渲染路径已实现（suites/gl_render.rs，macOS CGL + CPU 回退），
/// 缺的只是 golden 帧库（tests/ofx/frames/ 未生成）。帧库落地后实现
/// 并摘除。
#[test]
#[ignore = "golden 帧库（tests/ofx/frames/）未生成；落地后实现并摘除"]
fn render_golden_gl_tolerant() {
	todo!("M11 第 2 期 GL 路径落地后实现")
}

/// F32+ACEScg 链路断言：golden 帧的像素格式为 F32，且经 OCIO
/// display transform 后与参考值在容差内（色彩链路没被偷换成 8-bit
/// 的回归防线）。
///
/// # ignore 原因
///
/// 同 [`render_golden_cpu_bitexact`]：只缺 golden 参照帧库——F32
/// 管线和 OCIO/ofxColour 链路均已实现（colour_test.rs 覆盖）。
#[test]
#[ignore = "golden 帧库（tests/ofx/frames/）未生成；落地后实现并摘除"]
fn pipeline_is_f32_acescg() {
	todo!("帧库与 OCIO 链路落地后实现")
}
