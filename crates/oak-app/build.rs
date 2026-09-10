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

//! Build-time link configuration for the `oakapp` crate.
//!
//! M14 R3: the app links the oak* module crates as plain rlibs — there is
//! no `liboakengine` dylib to locate anymore. The only remaining link
//! concern is gpui's macOS backend: gpui_macos reaches the IOSurface API
//! through the `core-video` crate, which depends on `io-surface` with
//! `default-features = false` — that disables io-surface's `link` feature,
//! so nothing adds the IOSurface.framework to the final link and the
//! binary fails with undefined `_IOSurface*` symbols. The app's build
//! script is the single place that configures the macOS link, so link the
//! framework here.

fn main() {
	let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
	if os == "macos" {
		println!("cargo:rustc-link-lib=framework=IOSurface");
		// The static FFmpeg's external codec libraries pull in `-lz`, and
		// on some machines that resolves to a package-manager copy whose
		// install name is @rpath/libz.1.dylib; without an LC_RPATH entry
		// the binary dies at launch ("Library not loaded"). Map the rpath
		// at the real system library.
		println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib");
	}
	if os == "windows" {
		// The window/taskbar icon on Windows comes from the exe's icon
		// resource (ID 1 — gpui_windows' `load_icon`); compile it in. The
		// same resource is the exe's Explorer icon.
		println!("cargo:rerun-if-changed=resources/windows/oak.rc");
		println!("cargo:rerun-if-changed=../../assets/appicon/icon.ico");
		embed_resource::compile("resources/windows/oak.rc", embed_resource::NONE)
			.manifest_optional()
			.unwrap();
	}
	// --- OFX interact end-to-end test plugin ---------------------------------
	// The app-side interact tests (src/oakui/ofx.rs) drive the *real*
	// minimal test plugin (../../crates/oak-plugin/cbits/oak_test_plugin.c) through
	// the app's interact wiring. oakplugin compiles the same C file into its
	// own OUT_DIR, but build-script env vars do not cross crates, so compile
	// it here too — the test assembles a plugin bundle from the app's
	// OUT_DIR.
	println!("cargo:rerun-if-changed=../../crates/oak-plugin/cbits/oak_test_plugin.c");
	build_test_plugin(&os);
}

/// Compiles the minimal OFX test plugin as a shared library into
/// `$OUT_DIR/oak_test_plugin.{dylib,so}` (same recipe as oakplugin's
/// build.rs). Only the interact branch of the plugin is exercised by the
/// app-side tests; the GL parts are macOS-gated inside the C source.
fn build_test_plugin(os: &str) {
	use std::process::Command;
	let out = std::env::var("OUT_DIR").expect("OUT_DIR");
	let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
	let (link_flag, ext) = if os == "macos" {
		("-dynamiclib", "dylib")
	} else {
		("-shared", "so")
	};
	let mut args = vec![
		"-fPIC".to_string(),
		"-I../../crates/oak-plugin/ofx".to_string(),
		"../../crates/oak-plugin/cbits/oak_test_plugin.c".to_string(),
		link_flag.to_string(),
		"-o".to_string(),
		format!("{out}/oak_test_plugin.{ext}"),
	];
	if os == "macos" {
		args.push("-framework".into());
		args.push("OpenGL".into());
	}
	let status = Command::new(&cc)
		.args(&args)
		.status()
		.expect("compile OFX test plugin failed");
	assert!(status.success(), "OFX test plugin compile failed");
}
