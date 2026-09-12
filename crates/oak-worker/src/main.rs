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

//! oak-worker: headless render worker process (Rust).
//!
//! The whole runtime lives in this binary (M14 R2): render backend
//! selection (dynamic -> OpenGL fallback through the oakrender crate's
//! direct Rust API), the startup handshake and the NDJSON control loop
//! ([`worker`], the port of `engine/src/capi/worker.cpp`), plus the
//! shared-memory frame-slot transport ([`ipc`], the port of
//! `engine/render/ipc/`). No `liboakengine` dylib is linked; the facade
//! keeps its own copies of both modules for the frozen
//! `oakengine_worker_*` / `oakengine_ipc_*` C ABI (external consumers).
//!
//! See README.md for the full status.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod framecache;
mod ipc;
mod ofx_host;
mod worker;

use std::process::exit;

/// Protocol version announced in the startup handshake
/// (`k_protocol_version` in worker.cpp).
pub const PROTOCOL_VERSION: i32 = 1;

/// Log a worker-side message to stderr, mirroring worker.cpp `log_error()`
/// (the `worker: ` prefix).
pub fn log_error(message: &str) {
	eprintln!("worker: {message}");
}

/// Scan argv for `--backend <name>` (worker.cpp `oakengine_worker_main`).
/// The default is `"opengl"`; the value is lowercased; the last flag wins.
fn parse_backend(args: &[String]) -> String {
	let mut backend = "opengl".to_string();
	let mut i = 1usize;
	while i < args.len() {
		if args[i] == "--backend" && i + 1 < args.len() {
			backend = args[i + 1].to_ascii_lowercase();
			i += 2;
		} else {
			i += 1;
		}
	}
	backend
}

fn main() {
	let args: Vec<String> = std::env::args().collect();
	// M3: `--ofx-host` runs the single OpenFX host loop instead of the
	// ticket worker (same binary, no extra target to package).
	if args.iter().any(|arg| arg == "--ofx-host") {
		exit(ofx_host::ofx_host_main(&args));
	}
	let backend = parse_backend(&args);
	exit(worker::worker_main(&backend));
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn protocol_version_is_one() {
		assert_eq!(PROTOCOL_VERSION, 1);
	}

	#[test]
	fn backend_parsing_matches_engine_main() {
		// Default is opengl.
		assert_eq!(parse_backend(&["oak-worker".to_string()]), "opengl");
		// Value lowercased.
		assert_eq!(
			parse_backend(&[
				"oak-worker".to_string(),
				"--backend".to_string(),
				"Vulkan".to_string()
			]),
			"vulkan"
		);
		// "none" skips renderer creation.
		assert_eq!(
			parse_backend(&[
				"oak-worker".to_string(),
				"--backend".to_string(),
				"none".to_string()
			]),
			"none"
		);
		// Last flag wins.
		assert_eq!(
			parse_backend(&[
				"oak-worker".to_string(),
				"--backend".to_string(),
				"vulkan".to_string(),
				"--backend".to_string(),
				"opengl".to_string()
			]),
			"opengl"
		);
		// Missing value leaves the default.
		assert_eq!(
			parse_backend(&["oak-worker".to_string(), "--backend".to_string()]),
			"opengl"
		);
	}
}
