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

//! 测试公共件：最小测试插件定位、快照/golden 文件路径、夹具。
//!
//! 最小测试插件（cbits/oak_test_plugin.c，build.rs 编译为共享库，
//! 运行时装配成 oak-test-plugin.ofx.bundle）：filter 上下文、
//! Double 参数 gain、双 clip（Source/Output）。插件未构建时相关
//! 用例经 [`skip`] 提前返回。
//! 单库化后像素路径经 oakrender 值模型（`oak_core::texture::Texture`）
//! 驱动；渲染 goldens 待该迁移落地。

// 每个集成测试二进制都会独立编译本模块，只用到自己需要的那部分辅助件，
// 因此某个二进制里未用到的条目仍属于共享夹具 API。
#![allow(dead_code)]

use std::path::PathBuf;

/// 构建系统注入插件路径的环境变量名。
pub const TEST_PLUGIN_ENV: &str = "OAK_TEST_PLUGIN_DIR";

/// 测试插件 bundle 的绝对路径（由构建系统经环境变量注入；
/// 未注入时用 build.rs 编的共享库现场装配 bundle 目录；均不可用
/// 时返回 None，调用方 skip）。
pub fn test_plugin_dir() -> Option<PathBuf> {
	if let Some(p) = std::env::var_os(TEST_PLUGIN_ENV) {
		return Some(PathBuf::from(p));
	}
	static BUNDLE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
	BUNDLE
		.get_or_init(|| {
			let out = PathBuf::from(env!("OUT_DIR"));
			let lib = if cfg!(target_os = "macos") {
				out.join("oak_test_plugin.dylib")
			} else {
				out.join("oak_test_plugin.so")
			};
			if !lib.is_file() {
				return None;
			}
			let bundle = std::env::temp_dir()
				.join(format!("oak-test-plugin-{}", std::process::id()))
				.join("oak-test-plugin.ofx.bundle");
			let platform = if cfg!(target_os = "macos") {
				"MacOS"
			} else if cfg!(target_os = "windows") {
				"Win64"
			} else {
				"Linux-x86-64"
			};
			let bin_dir = bundle.join("Contents").join(platform);
			std::fs::create_dir_all(&bin_dir).ok()?;
			// Windows 上必须有 .dll 扩展名：LoadLibrary 会对无扩展名的
			// 模块名自动追加 ".dll"，名为 "plugin" 的文件将加载失败。
			let target = bin_dir.join(if cfg!(target_os = "windows") {
				"plugin.dll"
			} else {
				"plugin"
			});
			if !target.exists() {
				std::fs::copy(&lib, &target).ok()?;
			}
			Some(bundle)
		})
		.clone()
}

/// 测试插件 bundle 的父目录（host_scan 的入参）。
pub fn test_plugin_scan_dir() -> Option<PathBuf> {
	let bundle = test_plugin_dir()?;
	let parent = bundle.parent()?;
	// scan_path 会 canonicalize；bundle 在临时目录下，直接给父目录。
	Some(parent.to_path_buf())
}

/// golden master 目录（tests/ofx/）。描述符快照 JSON 与 EXR 帧都在这。
pub fn golden_dir() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/ofx")
}

/// 当前平台是否有 GPU（GL golden 用例的门：无 GPU 一律 skip）。
///
/// 第 1 期无 GL 用例：显式环境变量开启（GL 验收需人工本机确认，
/// CI 一律跳过）。
pub fn gpu_available() -> bool {
	std::env::var_os("OAK_GPU_TESTS").is_some()
}

/// 通用 skip 宏的函数形态：前置条件不满足时打印原因并提前返回。
/// （cargo 没有 GTEST_SKIP，约定为 `return`，并在输出里打印 SKIP。）
pub fn skip(reason: &str) {
	println!("SKIP: {reason}");
}

/// 宿主单例测试串行化：同一二进制的测试并行跑会互相踩
/// init/shutdown/scan（进程单例无锁）；所有触碰宿主面的用例经它。
pub fn with_host(f: impl FnOnce()) {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	f();
}
