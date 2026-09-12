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

//! # oakplugin — Oak 的插件模块（自研 OFX 宿主）
//!
//! 本 crate 是 oakplugin 模块的全部实现（M11）：
//!
//! - **OFX 宿主**：扫描 bundle、加载插件、实现八张 suite
//!   （property/memory/image_effect/param/message/progress/timeline/
//!   multithread），驱动 describe/createInstance/render 等 action。
//! - **桥**：把 OFX 实例的参数接到 oaknode（[`node`]，节点值 POD +
//!   身份注册表与 undoable 回写）、把 clip 的输入输出接到 oakrender
//!   纹理（[`render`]，纹理/帧/渲染器值类型）、把参数修改包成 undo
//!   命令（直接经 `oak_undo::undocommand::UndoCommand`，见
//!   [`instance::Instance::submit_undo_command`]）。
//!
//! ## 单库化（single-lib unification）
//!
//! 本 crate 的 C ABI 出口（原 [`ffi`]）与模块桥（原 [`bridge`]）已
//! 删除：oaknode/oakrender/oakundo 均以 path 依赖直接链接。桥以
//! 直接 Rust 类型重建（[`node`] 的身份注册表与
//! `set_input_*_undoable`；[`render`] 的 `Texture`/`Frame`/`Renderer`
//! 值类型）。GL 纹理名（旧 `oakrender_texture_id` 的 GL 命名空间）
//! 由 [`gl_bridge`]（方案 B：离屏 CGL 上下文 + 回读）提供；
//! [`render::texture_id`] 对 oakrender 纹理保持恒 0（wgpu/Metal 无
//! GL 命名空间），use_opengl 决策与 GL suite 的 OpenGLTextureIndex
//! 改从 [`gl_bridge`] 取真实名。
//!
//! ## 句柄纪律（全 crate 最高优先级约定）
//!
//! 1. 插件可在任意自起线程回调 suite（multithread suite 存活期
//!    内）；一切共享状态走 `Mutex`。
//! 2. OFX 语义以 openfx HostSupport 为参照系；协商与时序实现点必须
//!    注释对应 HostSupport 文件与行号（格式：`// HS: ofxhImageEffect.cpp:2776`）。
//!
//! ## 第 1 期范围
//!
//! filter/generator/transition 上下文；CPU 渲染路径。GL 纹理 suite
//! 与 ofxColour 为第 2 期（[`clip`]/[`image`] 内以 `// [P2]` 标记
//! 预留点）。

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod clip;
pub mod descriptor;
pub mod error;
pub mod gl_bridge;
pub mod handle;
pub mod host;
pub mod image;
pub mod instance;
pub mod node;
pub mod node_factory;
pub mod param;
pub mod param_curve;
pub mod progress;
pub mod property;
pub mod render;
pub mod render_driver;
pub mod suites;

/// The minimal test plugin shared library built by this crate's build
/// script (`$OUT_DIR/oak_test_plugin.{so,dylib}`), for integration tests
/// that exercise the real plugin path — including the single OFX host
/// process (M3) — without a system plugin. The build script always
/// compiles it, so this is `None` only if the build directory was
/// tampered with (tests treat that as a hard failure, not a skip).
pub fn bundled_test_plugin() -> Option<std::path::PathBuf> {
	let dir = std::path::Path::new(env!("OUT_DIR"));
	for name in [
		"oak_test_plugin.dylib",
		"oak_test_plugin.so",
		"oak_test_plugin.dll",
	] {
		let path = dir.join(name);
		if path.exists() {
			return Some(path);
		}
	}
	None
}
