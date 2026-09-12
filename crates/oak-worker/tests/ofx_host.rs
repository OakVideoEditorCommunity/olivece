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

//! M3: the single OpenFX host process (`oak-worker --ofx-host`).
//!
//! These tests spawn the real worker binary in host mode against the
//! crate's bundled test plugin and assert the milestone's acceptance
//! points: a plugin job renders and its progress events reach the app
//! callback, a crashed host is respawned and the in-flight job is
//! re-posted, and three consecutive crashes exhaust the budget (the
//! evaluator's purple fallback is asserted by the eval unit test).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oak_core::texture::Texture;
use oak_core::{PixelFormat, Rational};
use oak_render::eval::{generate_frame, JobSpec};
use oak_render::ofxhost::{OfxHost, OfxHostConfig};

/// The plugin runtime and the progress callback are process-wide.
static LOCK: Mutex<()> = Mutex::new(());

/// Assemble the bundled test plugin into an OFX bundle and return the
/// directory that contains it (`OFX_PLUGIN_PATH`). `None` when the test
/// plugin was not built (release builds).
fn bundle_parent(tag: &str) -> Option<PathBuf> {
	let lib = oak_plugin::bundled_test_plugin()?;
	let root = std::env::temp_dir().join(format!(
		"oak-ofx-host-{tag}-{}",
		std::process::id()
	));
	let bundle = root.join("OakTest.ofx.bundle");
	let platform = if cfg!(target_os = "macos") {
		"MacOS"
	} else if cfg!(target_os = "windows") {
		"Win64"
	} else if cfg!(target_arch = "aarch64") {
		"Linux-aarch64"
	} else {
		"Linux-x86-64"
	};
	let dest = bundle.join("Contents").join(platform);
	std::fs::create_dir_all(&dest).ok()?;
	let name = lib.file_name()?;
	std::fs::copy(&lib, dest.join(name)).ok()?;
	Some(root)
}

fn host(plugin_dir: &Path, args: Vec<String>, max_failures: u32) -> Arc<OfxHost> {
	OfxHost::new(OfxHostConfig {
		host_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_oak-worker"))),
		max_failures,
		host_args: args,
		env: vec![(
			"OFX_PLUGIN_PATH".to_string(),
			plugin_dir.to_string_lossy().into_owned(),
		)],
		..Default::default()
	})
	.expect("host client")
}

fn source_texture(w: i32, h: i32) -> Texture {
	let mut frame = generate_frame(Rational::new(0, 1), (w, h), PixelFormat::F32).unwrap();
	for px in frame.data.chunks_exact_mut(16) {
		for (c, v) in px.chunks_exact_mut(4).zip([0.1f32, 0.2, 0.3, 1.0]) {
			c.copy_from_slice(&v.to_le_bytes());
		}
	}
	Texture::wrap_frame(frame)
}

/// The bundled test plugin's filter (`org.oak.test-plugin`) paints a
/// constant 0.5 grey with alpha 1.
fn plugin_spec(src: &Texture) -> JobSpec {
	JobSpec::Plugin {
		instance: 0,
		type_id: "org.oak.test-plugin".to_string(),
		time: 0.0,
		effect_input_id: Some("Source".to_string()),
		inputs: vec![("Source".to_string(), src.clone())],
		values: Vec::new(),
	}
}

fn first_pixel(frame: &oak_core::texture::Frame) -> [f32; 4] {
	let mut out = [0f32; 4];
	for (i, value) in out.iter_mut().enumerate() {
		*value = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

#[test]
fn host_renders_plugin_job_and_forwards_progress() {
	let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let dir = bundle_parent("render")
		.expect("the bundled test plugin must exist (build.rs compiles it)");
	let events: Arc<Mutex<Vec<(String, String, f64)>>> = Arc::new(Mutex::new(Vec::new()));
	let sink = events.clone();
	oak_render::procpool::set_plugin_progress_cb(Some(Arc::new(move |label, message, fraction| {
		sink.lock()
			.unwrap_or_else(|e| e.into_inner())
			.push((label, message, fraction));
	})));

	let client = host(&dir, Vec::new(), 3);
	let src = source_texture(16, 16);
	let out = client
		.submit(&plugin_spec(&src), &src)
		.expect("the host renders the plugin job");
	assert_eq!((out.width, out.height), (16, 16));
	assert_eq!(out.format, PixelFormat::F32);
	let px = first_pixel(&out);
	assert!(
		(px[0] - 0.5).abs() < 1e-3 && (px[3] - 1.0).abs() < 1e-6,
		"the test plugin's constant grey must come back: {px:?}"
	);

	let seen = events.lock().unwrap_or_else(|e| e.into_inner()).clone();
	assert!(
		seen.iter()
			.any(|(label, _, fraction)| label == "render" && (*fraction - 0.5).abs() < 1e-6),
		"the plugin's progressUpdate must reach the app callback: {seen:?}"
	);
	assert!(
		seen.iter().any(|(_, _, fraction)| (*fraction - 1.0).abs() < 1e-6),
		"progressEnd closes the dialog: {seen:?}"
	);

	oak_render::procpool::set_plugin_progress_cb(None);
	client.shutdown();
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_respawns_and_reposts_after_a_crash() {
	let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let dir = bundle_parent("respawn")
		.expect("the bundled test plugin must exist (build.rs compiles it)");
	let marker = dir.join("crash-once.marker");
	let client = host(
		&dir,
		vec![
			"--ofx-crash-once".to_string(),
			marker.to_string_lossy().into_owned(),
		],
		3,
	);
	let src = source_texture(16, 16);
	let out = client
		.submit(&plugin_spec(&src), &src)
		.expect("the in-flight job is re-posted to the respawned host");
	assert_eq!(out.width, 16);
	assert!(client.restarts() >= 1, "the crashed host was respawned");
	assert_eq!(client.failures(), 0, "a successful re-post clears the crash count");
	assert_eq!(first_pixel(&out)[0], 0.5, "the re-posted job still renders");

	client.shutdown();
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_cancels_an_in_flight_slow_render() {
	let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let dir = bundle_parent("cancel").expect("the bundled test plugin must exist");
	let client = host(&dir, Vec::new(), 3);
	let src = source_texture(16, 16);
	let spec = JobSpec::Plugin {
		instance: 0,
		type_id: "org.oak.test-plugin.slow".to_string(),
		time: 0.0,
		effect_input_id: Some("Source".to_string()),
		inputs: vec![("Source".to_string(), src.clone())],
		values: Vec::new(),
	};
	let submitter = client.clone();
	let src_for_job = src.clone();
	let job = std::thread::spawn(move || submitter.submit(&spec, &src_for_job));
	// The slow plugin renders for ~2 s in 20 ms progress steps; cancel
	// lands while it is in flight and the next progressUpdate aborts it.
	std::thread::sleep(Duration::from_millis(200));
	client.cancel();
	let result = job.join().expect("the submit thread must not panic");
	assert!(
		result.is_err(),
		"a cancelled slow render must fail instead of returning a frame"
	);
	assert_eq!(
		client.failures(),
		0,
		"a cancel is not a crash; the budget stays untouched"
	);

	client.shutdown();
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_serializes_concurrent_submits() {
	let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let dir = bundle_parent("concurrent").expect("the bundled test plugin must exist");
	let client = host(&dir, Vec::new(), 3);
	let src = source_texture(16, 16);

	let a = client.clone();
	let spec_a = plugin_spec(&src);
	let src_a = src.clone();
	let first = std::thread::spawn(move || a.submit(&spec_a, &src_a));
	let b = client.clone();
	let spec_b = plugin_spec(&src);
	let src_b = src.clone();
	let second = std::thread::spawn(move || b.submit(&spec_b, &src_b));

	// The submit lock serializes the jobs (each writes the shared input
	// pool); both must come back with the plugin's constant grey.
	for handle in [first, second] {
		let frame = handle.join().expect("submit thread").expect("job renders");
		assert_eq!(first_pixel(&frame)[0], 0.5);
	}

	client.shutdown();
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn host_gives_up_after_three_consecutive_crashes() {
	let _lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
	let dir = bundle_parent("giveup")
		.expect("the bundled test plugin must exist (build.rs compiles it)");
	let client = host(&dir, vec!["--ofx-crash-always".to_string()], 3);
	let src = source_texture(8, 8);
	let err = client
		.submit(&plugin_spec(&src), &src)
		.expect_err("three consecutive crashes must give up");
	let _ = err;
	assert_eq!(client.failures(), 3);
	assert!(client.is_permanently_dead());
	// Fast-fail afterwards: no more spawns.
	let restarts = client.restarts();
	assert!(client.submit(&plugin_spec(&src), &src).is_err());
	assert_eq!(client.restarts(), restarts);

	client.shutdown();
	let _ = std::fs::remove_dir_all(&dir);
}
