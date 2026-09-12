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

//! Build-time link configuration for the `oakffmpeg-link` crate.
//!
//! ffmpeg-sys-next only emits the link directives for FFmpeg's own
//! libraries. When FFmpeg is linked STATICALLY (the project FFmpeg built
//! by tooling/ffmpeg/build-ffmpeg.sh, pointed at via `FFMPEG_DIR`), its
//! archives reference every external codec library they were configured
//! with (x264, x265, dav1d, ..., plus system frameworks on macOS), and
//! the final link fails with undefined symbols unless those transitive
//! dependencies are linked too. FFmpeg's own `.pc` files carry the exact
//! list in `Libs.private`, so this script asks pkg-config for the static
//! link line and forwards it to cargo.
//!
//! The project FFmpeg (built by tooling/ffmpeg/build-ffmpeg.sh, pointed
//! at via `FFMPEG_DIR`) is MANDATORY: failing loudly beats silently
//! binding a system pkg-config FFmpeg whose .pc may reference stale
//! paths (observed: a Homebrew upgrade of dav1d left ffmpeg's own .pc
//! pointing at a deleted Cellar dir, breaking the link).

use std::path::PathBuf;
use std::process::Command;

fn main() {
	let dir = match env_or_dotenv("FFMPEG_DIR") {
		Some(dir) => dir,
		None => panic!(
			"FFMPEG_DIR is not set. Oak links FFmpeg statically; build the project FFmpeg \
			 first:\n  tooling/install-deps.sh\n  tooling/ffmpeg/build-ffmpeg.sh\n  export \
			 FFMPEG_DIR=$(pwd)/.cache/ffmpeg\n(see docs/build.md). IDEs that cannot inject \
			 environment variables into cargo (e.g. RustRover) can use a .env file at the \
			 workspace root with FFMPEG_DIR=<absolute path>."
		),
	};
	let pc_dir = PathBuf::from(&dir).join("lib").join("pkgconfig");
	assert!(
		pc_dir.exists(),
		"FFMPEG_DIR={dir} has no lib/pkgconfig — point it at a full install prefix \
		 (the output of tooling/ffmpeg/build-ffmpeg.sh)"
	);
	println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
	let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
	// Emitting rerun-if-changed for a MISSING file makes cargo re-run this
	// build script on every single build (the "missing" state never
	// stabilizes into a fingerprint), cascading rebuilds through every
	// dependent crate. Only track the file once it actually exists.
	let dotenv = manifest.join("../..").join(".env");
	if dotenv.exists() {
		println!("cargo:rerun-if-changed={}", dotenv.display());
	}

	let pkg_path = env_or_dotenv("PKG_CONFIG_PATH").unwrap_or_default();
	// The pkg-config child must see the FFMPEG_DIR .pc files. Join with the
	// platform separator (`:` on Unix, `;` on Windows): formatting a `:`
	// into a Windows `C:\...` path list makes pkgconf split the drive
	// letter off.
	let joined_path = {
		let mut paths = vec![pc_dir.clone()];
		if !pkg_path.is_empty() {
			paths.extend(std::env::split_paths(&pkg_path));
		}
		std::env::join_paths(paths).unwrap_or_default()
	};
	const FFMPEG_LIBS: [&str; 7] = [
		"libavformat",
		"libavcodec",
		"libavfilter",
		"libavdevice",
		"libavutil",
		"libswscale",
		"libswresample",
	];
	let run = |program: &str| {
		Command::new(program)
			.arg("--static")
			.arg("--libs")
			.args(FFMPEG_LIBS)
			.env("PKG_CONFIG_PATH", &joined_path)
			.output()
	};
	// vcpkg ships `pkgconf` on Windows without a `pkg-config` shim (macOS
	// Homebrew ships both); fall back on the standard name first, then
	// pkgconf. Only a missing program falls back — a pkg-config that runs
	// and fails is a real error (bad .pc, missing libs) and must surface.
	let output = match env_or_dotenv("PKG_CONFIG").filter(|p| !p.trim().is_empty()) {
		Some(custom) => run(&custom).expect("PKG_CONFIG points at a program that failed to run"),
		None => match run("pkg-config") {
			Ok(output) => output,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				run("pkgconf").expect("pkg-config/pkgconf is required when FFMPEG_DIR is set")
			}
			Err(e) => panic!("failed to run pkg-config: {e}"),
		},
	};
	if !output.status.success() {
		panic!(
			"pkg-config --static --libs failed for the FFMPEG_DIR install: {}",
			String::from_utf8_lossy(&output.stderr)
		);
	}

	// System libraries (z, m, bz2, iconv) must come from the OS, not from
	// a package manager's keg: Homebrew's zlib carries an @rpath install
	// name, and linking it without an rpath entry breaks the binary at
	// launch (dyld: Library not loaded: @rpath/libz.1.dylib). Put /usr/lib
	// first in the search order so -lz resolves to the system copy.
	#[cfg(target_os = "macos")]
	println!("cargo:rustc-link-search=native=/usr/lib");

	for token in String::from_utf8_lossy(&output.stdout).split_whitespace() {
		if let Some(path) = token.strip_prefix("-L") {
			println!("cargo:rustc-link-search=native={path}");
		} else if let Some(lib) = token.strip_prefix("-l") {
			// MinGW has no libdl: FFmpeg's .pc files can still list it via
			// an external dep's Requires.private; the Unix dlopen surface
			// the FFmpeg build uses has no Windows references to satisfy.
			#[cfg(target_os = "windows")]
			if lib == "dl" {
				continue;
			}
			// System libs (m, z, bz2, iconv, ...) and externals alike; the
			// linker picks .a or .dylib per -L search order.
			println!("cargo:rustc-link-lib={lib}");
		} else if token == "-framework" {
			// Handled on the next token (see below).
		} else if let Some(framework) = token.strip_prefix("-framework=") {
			println!("cargo:rustc-link-lib=framework={framework}");
		}
		// Bare tokens after "-framework" are handled by the stateful pass
		// below; pkg-config prints "-framework Foo" as two tokens.
	}
	// Second pass for the two-token "-framework Foo" form.
	let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
	let tokens: Vec<&str> = stdout.split_whitespace().collect();
	for pair in tokens.windows(2) {
		if pair[0] == "-framework" {
			println!("cargo:rustc-link-lib=framework={}", pair[1]);
		}
	}

	// Some codec libraries are C++ (svt-av1's JsonHelper, ...); their
	// archives reference the C++ standard library, which pkg-config's
	// Libs.private does not list. Link it explicitly.
	if cfg!(target_os = "macos") {
		println!("cargo:rustc-link-lib=c++");
	} else if cfg!(all(target_os = "linux", target_env = "gnu")) {
		println!("cargo:rustc-link-lib=stdc++");
	}

	// Some externals (Homebrew lame, snappy) reach the FFmpeg archives via
	// the configure-time --extra-ldflags instead of Requires.private, so
	// their -L never shows up above; add the Homebrew lib dir as a
	// fallback search path.
	#[cfg(target_os = "macos")]
	for prefix in ["/opt/homebrew/lib", "/usr/local/lib"] {
		if std::path::Path::new(prefix).exists() {
			println!("cargo:rustc-link-search=native={prefix}");
		}
	}
}

/// Reads `key` from the process environment, falling back to the `.env`
/// file at the workspace root (this crate lives in crates/oakffmpeg-link,
/// so the root is two levels up). The .env syntax is the common one:
/// `KEY=value` lines, optional `export ` prefix, `#` comments, optional
/// matching single/double quotes around the value.
///
/// IDEs that cannot inject environment variables into the cargo
/// invocation (RustRover) use the .env fallback for `FFMPEG_DIR`.
fn env_or_dotenv(key: &str) -> Option<String> {
	if let Ok(v) = std::env::var(key) {
		return Some(v);
	}
	let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
	let content = std::fs::read_to_string(root.join(".env")).ok()?;
	for line in content.lines() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let line = line.strip_prefix("export ").unwrap_or(line);
		let Some((k, v)) = line.split_once('=') else {
			continue;
		};
		if k.trim() == key {
			let v = v.trim();
			let v = v
				.strip_prefix('"')
				.and_then(|s| s.strip_suffix('"'))
				.or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
				.unwrap_or(v);
			return Some(v.to_string());
		}
	}
	None
}
