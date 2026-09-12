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

//! `oak-worker --ofx-host`: the single OpenFX host process (M3, design
//! §3.2).
//!
//! The host loads every OFX plugin once (the worker binary's normal plugin
//! runtime) and then serves `ofx_job` messages from the main process over
//! NDJSON, with frames moving through the input/output
//! [`FrameSlotPool`]s announced by the handshake. Each job is resolved to
//! a host-local instance by the plugin **identifier** (the cross-process
//! stable key) and rendered through the same in-process executor the
//! workers used to install ([`oak_plugin::node_factory::install_render_executor`]).
//!
//! Progress is flushed to stdout immediately (the main process's reader
//! forwards it to the plugin-progress dialog), and `plugin_cancel` sets
//! the same sticky flag protocol the worker used: the next `progressStart`
//! resets it and every `progressUpdate` after a cancel answers false, so
//! the plugin aborts at its next progress call.
//!
//! The host is deliberately single-threaded and synchronous, matching the
//! worker model; crash isolation comes from the parent respawning it.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};

use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};
use oak_core::PixelFormat;
use oak_render::eval::{self, JobSpec, PluginJobRequest};
use oak_render::ipc::{
	error_message, write_message, FrameSlotPool, HandshakeMsg, OfxJobMsg, OfxResultMsg,
	PluginProgressMsg, SharedMemoryRegion, ShmMode, TYPE_HANDSHAKE, TYPE_OFX_JOB,
	TYPE_PLUGIN_CANCEL, TYPE_SHUTDOWN,
};

/// Sticky plugin cancel (protocol parity with `worker.rs`): set by
/// `plugin_cancel`, reset by the next `progressStart`.
static OFX_CANCEL: AtomicBool = AtomicBool::new(false);

/// stdout is shared by progress events (emitted from inside the plugin
/// render) and control responses.
static OUT_LOCK: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write one NDJSON line to stdout, flushed (progress must stream while
/// the job renders). No-op under unit tests (they exercise the reporter
/// return values, not the pipe).
fn emit(value: &serde_json::Value) {
	if cfg!(test) {
		return;
	}
	let _guard = lock(&OUT_LOCK);
	let stdout = std::io::stdout();
	let mut out = stdout.lock();
	let _ = write_message(&mut out, value);
	let _ = out.flush();
}

/// The host's progress reporter: emits `plugin_progress` immediately and
/// reports cancellation to the plugin.
struct HostProgressReporter {
	label: String,
	message: String,
}

impl oak_plugin::progress::UiProgressReporter for HostProgressReporter {
	fn update(&mut self, progress: f64) -> bool {
		emit(
			&PluginProgressMsg {
				label: self.label.clone(),
				message: self.message.clone(),
				fraction: progress.clamp(0.0, 1.0),
			}
			.to_json(),
		);
		!OFX_CANCEL.load(Ordering::Relaxed)
	}

	fn end(&mut self) {
		emit(
			&PluginProgressMsg {
				label: self.label.clone(),
				message: self.message.clone(),
				fraction: 1.0,
			}
			.to_json(),
		);
	}
}

/// Build one progress reporter for `progressStart`: clears the sticky
/// cancel (the protocol's "a fresh render starts uncancelled") and emits
/// the fraction-0 start event. The reporter factory installed with the
/// progress suite calls this; tests call it directly.
fn host_progress_reporter(label: &str, message: &str) -> Box<dyn oak_plugin::progress::UiProgressReporter> {
	OFX_CANCEL.store(false, Ordering::Relaxed);
	emit(
		&PluginProgressMsg {
			label: label.to_string(),
			message: message.to_string(),
			fraction: 0.0,
		}
		.to_json(),
	);
	Box::new(HostProgressReporter {
		label: label.to_string(),
		message: message.to_string(),
	})
}

/// Install the reporter factory (`progressStart` → [`host_progress_reporter`]).
fn install_progress_factory() {
	oak_plugin::progress::set_reporter_factory(Some(Arc::new(host_progress_reporter)));
}

/// Read stdin on its own thread. `plugin_cancel` is handled inline (sets
/// the sticky flag) so a cancel is observed while a plugin render is in
/// flight; every other line goes to the main loop through the channel.
/// Dropping the sender on EOF closes the channel and ends the loop.
fn spawn_stdin_reader() -> mpsc::Receiver<String> {
	let (tx, rx) = mpsc::channel();
	std::thread::Builder::new()
		.name("oak-ofx-host-stdin".into())
		.spawn(move || {
			let stdin = std::io::stdin();
			let mut reader = BufReader::new(stdin.lock());
			let mut line = String::new();
			loop {
				line.clear();
				match reader.read_line(&mut line) {
					Ok(0) | Err(_) => break,
					Ok(_) => {}
				}
				if line.trim().is_empty() {
					continue;
				}
				if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
					if value.get("type").and_then(|t| t.as_str()) == Some(TYPE_PLUGIN_CANCEL) {
						OFX_CANCEL.store(true, Ordering::Relaxed);
						continue;
					}
				}
				if tx.send(line.clone()).is_err() {
					break;
				}
			}
		})
		.expect("spawn OFX host stdin reader");
	rx
}

/// The attached pools. The `SharedMemoryRegion`s must outlive the pools
/// (the pool views point into the mappings).
struct HostPools {
	_input_region: SharedMemoryRegion,
	input: FrameSlotPool,
	_output_region: SharedMemoryRegion,
	output: FrameSlotPool,
}

/// Attach the pools announced by a `handshake` (the worker's
/// `handle_handshake`, minus the render session).
fn attach_pools(msg: &serde_json::Value) -> Result<HostPools, String> {
	let hs: HandshakeMsg =
		serde_json::from_value(msg.clone()).map_err(|e| format!("invalid handshake: {e}"))?;
	if hs.shm_key.is_empty() || hs.output_slots <= 0 || hs.slot_data_bytes <= 0 {
		return Err("handshake missing output shared-memory geometry".to_string());
	}
	let output_bytes =
		FrameSlotPool::bytes_needed(hs.output_slots as u32, hs.slot_data_bytes as usize);
	let mut output_region = SharedMemoryRegion::new();
	if !output_region.open(&hs.shm_key, output_bytes, ShmMode::Attach) {
		return Err(format!(
			"failed to attach output shared memory: {}",
			output_region.error()
		));
	}
	// SAFETY: the mapping is live and sized for the pool.
	let output = unsafe { FrameSlotPool::attach(output_region.data()) };
	if !output.is_valid() {
		return Err("output shared memory does not contain a frame slot pool".to_string());
	}

	if hs.input_shm_key.is_empty() || hs.input_slots <= 0 || hs.input_slot_data_bytes <= 0 {
		return Err("handshake missing input shared-memory geometry".to_string());
	}
	let input_bytes =
		FrameSlotPool::bytes_needed(hs.input_slots as u32, hs.input_slot_data_bytes as usize);
	let mut input_region = SharedMemoryRegion::new();
	if !input_region.open(&hs.input_shm_key, input_bytes, ShmMode::Attach) {
		return Err(format!(
			"failed to attach input shared memory: {}",
			input_region.error()
		));
	}
	// SAFETY: the mapping is live and sized for the pool.
	let input = unsafe { FrameSlotPool::attach(input_region.data()) };
	if !input.is_valid() {
		return Err("input shared memory does not contain a frame slot pool".to_string());
	}

	Ok(HostPools {
		_input_region: input_region,
		input,
		_output_region: output_region,
		output,
	})
}

/// Consume one input frame (the producer is the main process).
fn read_input_frame(pool: &FrameSlotPool, slot: u32) -> Result<Frame, String> {
	let mut consumed = 0u32;
	// SAFETY: consumer side of the SPSC protocol, single-threaded host.
	if !unsafe { pool.consume(&mut consumed) } {
		return Err("input slot missing".to_string());
	}
	if consumed != slot {
		unsafe {
			pool.release(consumed);
		}
		return Err(format!(
			"input slot mismatch: expected {slot}, got {consumed}"
		));
	}
	// SAFETY: `slot` was just consumed; the meta POD is initialized by the
	// producer before publish.
	let meta = unsafe { &*pool.meta_const(slot) };
	if meta.width <= 0 || meta.height <= 0 || meta.data_size < 0 {
		unsafe {
			pool.release(slot);
		}
		return Err("input frame has invalid metadata".to_string());
	}
	let len = (meta.data_size as usize).min(pool.slot_data_bytes());
	// SAFETY: `len` is within the slot block.
	let data = unsafe { std::slice::from_raw_parts(pool.slot_data_const(slot), len).to_vec() };
	unsafe {
		pool.release(slot);
	}
	let mut frame = Frame::new();
	let mut pod = VideoParamsPod::default();
	pod.width = meta.width;
	pod.height = meta.height;
	pod.format = PixelFormat::F32 as i32;
	frame.set_video_params(pod);
	frame.data = data;
	Ok(frame)
}

/// Publish one output frame (the consumer is the main process).
fn write_output_frame(pool: &FrameSlotPool, frame: &Frame) -> Result<i32, String> {
	let mut slot = 0u32;
	// SAFETY: producer side of the SPSC protocol, single-threaded host.
	if !unsafe { pool.acquire(&mut slot) } {
		return Err("output pool is full".to_string());
	}
	let bytes = frame.data.len();
	if bytes > pool.slot_data_bytes() {
		unsafe {
			pool.release(slot);
		}
		return Err(format!(
			"output frame is {bytes} bytes, larger than the output slot ({})",
			pool.slot_data_bytes()
		));
	}
	// SAFETY: `slot` was just acquired; fill then publish.
	unsafe {
		std::ptr::copy_nonoverlapping(frame.data.as_ptr(), pool.slot_data(slot), bytes);
		let meta = &mut *pool.meta(slot);
		*meta = Default::default();
		meta.width = frame.width;
		meta.height = frame.height;
		meta.format = PixelFormat::F32 as i32;
		meta.channel_count = 4;
		meta.linesize = frame.linesize_bytes() as i32;
		meta.data_size = bytes as i32;
		meta.time_num = frame.timestamp.numerator();
		meta.time_den = frame.timestamp.denominator().max(1);
		if !pool.publish(slot) {
			pool.release(slot);
			return Err("output publish failed".to_string());
		}
	}
	Ok(slot as i32)
}

/// Render one `ofx_job` and return its `ofx_result` response.
fn handle_job(msg: serde_json::Value, pools: &HostPools) -> serde_json::Value {
	let job: OfxJobMsg = match serde_json::from_value(msg) {
		Ok(job) => job,
		Err(err) => return error_message(&format!("invalid ofx_job: {err}"), None),
	};
	let fail = |message: String| {
		OfxResultMsg {
			job: job.job,
			slot: -1,
			error: message,
		}
		.to_json()
	};

	let mut inputs = Vec::with_capacity(job.inputs.len());
	for input in &job.inputs {
		match read_input_frame(&pools.input, input.slot) {
			Ok(frame) => inputs.push((input.name.clone(), Texture::wrap_frame(frame))),
			Err(err) => return fail(err),
		}
	}
	let src = match job.src_slot {
		Some(slot) => match read_input_frame(&pools.input, slot) {
			Ok(frame) => Texture::wrap_frame(frame),
			Err(err) => return fail(err),
		},
		None => {
			// No explicit source: mirror the evaluator's fallback (the
			// declared effect input, else the first clip, else dummy).
			let named = inputs
				.iter()
				.find(|(name, _)| name == &job.effect_input_id)
				.or_else(|| inputs.first())
				.map(|(_, texture)| texture.clone());
			named.unwrap_or_else(Texture::dummy)
		}
	};

	let Some(factory) = eval::plugin_instance_factory() else {
		return fail("no plugin instance factory installed in the OFX host".to_string());
	};
	let Some(instance) = factory(&job.type_id) else {
		return fail(format!("unknown or unavailable OFX plugin: {}", job.type_id));
	};
	let Some(executor) = eval::plugin_executor() else {
		return fail("no plugin executor installed in the OFX host".to_string());
	};
	let spec = JobSpec::Plugin {
		instance,
		type_id: job.type_id.clone(),
		time: job.time,
		effect_input_id: if job.effect_input_id.is_empty() {
			None
		} else {
			Some(job.effect_input_id.clone())
		},
		inputs,
		values: job
			.values
			.iter()
			.map(|param| (param.input.clone(), param.value.to_node_value()))
			.collect(),
	};
	match executor(&PluginJobRequest { spec: &spec, src }) {
		Ok(texture) => match texture.to_frame() {
			Ok(frame) => match write_output_frame(&pools.output, &frame) {
				Ok(slot) => OfxResultMsg {
					job: job.job,
					slot,
					error: String::new(),
				}
				.to_json(),
				Err(err) => fail(err),
			},
			Err(err) => fail(format!("plugin output readback failed: {err:?}")),
		},
		Err(err) => fail(format!("plugin render failed: {err:?}")),
	}
}

/// Test-only crash hooks (deterministic crash/restart acceptance tests).
struct CrashHooks {
	/// Crash on every job.
	always: bool,
	/// Crash on the first job of each process unless the marker file
	/// exists (create it before crashing, so a respawned host renders).
	once_marker: Option<PathBuf>,
}

impl CrashHooks {
	fn from_args(args: &[String]) -> Self {
		let mut hooks = CrashHooks {
			always: false,
			once_marker: None,
		};
		let mut i = 0usize;
		while i < args.len() {
			match args[i].as_str() {
				"--ofx-crash-always" => hooks.always = true,
				"--ofx-crash-once" if i + 1 < args.len() => {
					hooks.once_marker = Some(PathBuf::from(&args[i + 1]));
					i += 1;
				}
				_ => {}
			}
			i += 1;
		}
		hooks
	}

	fn maybe_crash(&self) {
		if self.always {
			std::process::abort();
		}
		if let Some(path) = &self.once_marker {
			if !path.exists() {
				let _ = std::fs::write(path, b"ofx-host-crashed");
				std::process::abort();
			}
		}
	}
}

/// The `--ofx-host` main loop. Returns the process exit code.
pub fn ofx_host_main(args: &[String]) -> i32 {
	// The same plugin runtime the render workers install: the executor
	// (so `plugin_executor` is callable) and the identifier-keyed instance
	// factory (so jobs resolve their own instances).
	oak_plugin::node_factory::install_render_executor();
	if let Err(err) = oak_plugin::host::Host::global().cache.scan() {
		eprintln!("ofx-host: plugin scan failed: {err}");
	}
	install_progress_factory();
	let crash = CrashHooks::from_args(args);

	// stdin runs on its own thread (cancel must be observed mid-render);
	// the main loop consumes the forwarded control messages.
	let control = spawn_stdin_reader();
	let mut pools: Option<HostPools> = None;
	while let Ok(line) = control.recv() {
		let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
			emit(&error_message("invalid JSON message", None));
			continue;
		};
		match msg.get("type").and_then(|t| t.as_str()) {
			Some(TYPE_HANDSHAKE) => match attach_pools(&msg) {
				Ok(attached) => pools = Some(attached),
				Err(err) => emit(&error_message(&err, None)),
			},
			Some(TYPE_OFX_JOB) => match &pools {
				Some(pools) => {
					crash.maybe_crash();
					let response = handle_job(msg, pools);
					emit(&response);
				}
				None => emit(&error_message("ofx_job before handshake", None)),
			},
			Some(TYPE_PLUGIN_CANCEL) => OFX_CANCEL.store(true, Ordering::Relaxed),
			Some(TYPE_SHUTDOWN) => break,
			other => emit(&error_message(
				&format!(
					"unknown message type: {}",
					other.unwrap_or("<missing>")
				),
				None,
			)),
		}
	}
	0
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cancel_flag_is_reset_by_progress_start() {
		// Cancel semantics (protocol parity with the worker): a cancelled
		// reporter tells the plugin to abort at its next progressUpdate;
		// the next progressStart (the factory path below) clears the
		// sticky flag.
		OFX_CANCEL.store(true, Ordering::Relaxed);
		let mut reporter = host_progress_reporter("render", "msg");
		assert!(
			reporter.update(0.5),
			"the progressStart factory path reset the sticky cancel"
		);
		// A cancel after the start makes the next update answer false.
		OFX_CANCEL.store(true, Ordering::Relaxed);
		assert!(!reporter.update(0.6), "a cancelled reporter answers false");
		OFX_CANCEL.store(false, Ordering::Relaxed);
	}

	#[test]
	fn crash_hooks_parse_args() {
		let hooks = CrashHooks::from_args(&[
			"oak-worker".to_string(),
			"--ofx-host".to_string(),
			"--ofx-crash-once".to_string(),
			"/tmp/marker".to_string(),
		]);
		assert!(!hooks.always);
		assert_eq!(hooks.once_marker.as_deref(), Some(std::path::Path::new("/tmp/marker")));

		let hooks = CrashHooks::from_args(&["oak-worker".to_string(), "--ofx-crash-always".to_string()]);
		assert!(hooks.always);
		assert!(hooks.once_marker.is_none());
	}
}
