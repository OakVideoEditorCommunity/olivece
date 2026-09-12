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

//! The single OpenFX host client (M3, design §3.2).
//!
//! One `oak-worker --ofx-host` child process hosts every OFX plugin
//! instance; the render eval thread submits [`JobSpec::Plugin`] jobs to it
//! over the NDJSON control plane and the shm frame-slot data plane:
//!
//! - Input textures are read back to CPU and written into the host's
//!   **input pool** (the explicit CPU boundary of the plugin path);
//!   the host renders and publishes its output frame into the **output
//!   pool**, which this client copies out and re-uploads into the
//!   evaluator's texture value.
//! - `plugin_progress` lines from the host are forwarded to the
//!   process-wide progress callback (`procpool::set_plugin_progress_cb`);
//!   `plugin_cancel` is sent on [`OfxHost::cancel`].
//! - A dying host (EOF on stdout) fails the in-flight submit, which
//!   respawns the host and **re-posts the same job**; after
//!   [`OfxHostConfig::max_failures`] consecutive crashes the client stays
//!   permanently dead and the evaluator falls back to a purple frame.
//!
//! The client is single-slot by design (design §3.2): submissions are
//! synchronous, one job in flight, because the evaluator itself is
//! synchronous. The process-wide client is installed by the render
//! manager when the thread pipeline is selected ([`install_client`]);
//! with no client installed the evaluator keeps using the in-process
//! oakplugin executor.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use oak_core::frame::VideoParamsPod;
use oak_core::texture::{Frame, Texture};
use oak_core::PixelFormat;

use crate::error::{Error, Result};
use crate::eval::JobSpec;
use crate::ipc::{
	plugin_cancel_json, write_message, HandshakeMsg, OfxInputRef, OfxJobMsg, OfxResultMsg,
	PluginProgressMsg, WireEffectParam, WireNodeValue, TYPE_ERROR, TYPE_OFX_RESULT,
	TYPE_PLUGIN_PROGRESS, TYPE_SHUTDOWN,
};
use crate::procpool::{plugin_progress_cb, ShmRegionView};

/// Slots per direction (input/output pools). Two slots per direction is
/// the floor; the client restarts the host with a bigger pool when a job
/// needs more (plugins with many clip inputs), so the idle cost stays
/// small.
pub const HOST_SLOTS: u32 = 2;

/// Initial per-slot capacity. Plugin frames are render-size F32; a 1080p
/// frame is ~8 MiB, and the pool grows (with a host restart) when a job
/// needs more. Small enough that an idle host costs little memory.
pub const HOST_SLOT_BYTES: usize = 8 * 1024 * 1024;

/// Consecutive crash budget before the client is permanently dead
/// (acceptance: three consecutive crashes fall back to purple frames).
pub const HOST_MAX_FAILURES: u32 = 3;

/// Configuration for [`OfxHost`].
#[derive(Clone, Debug)]
pub struct OfxHostConfig {
	/// Host executable; `None` resolves `OAK_WORKER_BIN` / the
	/// `oak-worker` binary next to the current executable.
	pub host_bin: Option<PathBuf>,
	/// Slots per direction.
	pub slots: u32,
	/// Initial per-slot capacity in bytes.
	pub slot_bytes: usize,
	/// Consecutive crash budget (a successful job resets it).
	pub max_failures: u32,
	/// Extra host argv (test crash hooks).
	pub host_args: Vec<String>,
	/// Extra host environment (test plugin discovery).
	pub env: Vec<(String, String)>,
}

impl Default for OfxHostConfig {
	fn default() -> Self {
		Self {
			host_bin: None,
			slots: HOST_SLOTS,
			slot_bytes: HOST_SLOT_BYTES,
			max_failures: HOST_MAX_FAILURES,
			host_args: Vec::new(),
			env: Vec::new(),
		}
	}
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Why a submit did not produce a frame: the host died (retryable) or it
/// reported a render failure (not a crash).
enum SubmitError {
	/// EOF/pipe error: the host is gone and the job can be re-posted.
	Died(String),
	/// A real plugin/render failure (the evaluator falls back).
	Failed(String),
}

struct HostInner {
	child: Option<Child>,
	stdin: Option<ChildStdin>,
	input: Option<Arc<ShmRegionView>>,
	output: Option<Arc<ShmRegionView>>,
	/// Current per-slot capacity (grows on demand).
	slot_bytes: usize,
	/// Current slots per direction (grows on demand).
	slots: u32,
	generation: u64,
	consecutive_failures: u32,
	permanently_dead: bool,
	reader: Option<JoinHandle<()>>,
	/// Total host respawns (tests/observability).
	restarts: u64,
}

struct WaitState {
	results: HashMap<u64, std::result::Result<Frame, String>>,
	dead: bool,
}

/// The single-host client.
pub struct OfxHost {
	config: OfxHostConfig,
	inner: Mutex<HostInner>,
	wait: Arc<(Mutex<WaitState>, Condvar)>,
	next_job: AtomicU64,
	/// Serializes submissions: the one-job-in-flight contract is enforced
	/// here, not left to the render thread being the only caller (a
	/// concurrent second submit would interleave input-pool writes).
	submit_lock: Mutex<()>,
}

/// One job's pre-read frames (read once, re-posted unchanged across
/// crashes).
struct HostJobFrames {
	type_id: String,
	time: f64,
	effect_input_id: String,
	inputs: Vec<(String, Frame)>,
	/// The main source frame (sent separately from the named inputs; the
	/// evaluator's `src` may not correspond to any named clip).
	src: Option<Frame>,
	values: Vec<WireEffectParam>,
}

impl HostJobFrames {
	fn max_bytes(&self) -> usize {
		self.inputs
			.iter()
			.map(|(_, f)| f.data.len())
			.chain(self.src.iter().map(|f| f.data.len()))
			.max()
			.unwrap_or(0)
	}

	/// Input-pool slots the job needs (named clips + the source).
	fn input_slots(&self) -> u32 {
		self.inputs.len() as u32 + u32::from(self.src.is_some())
	}
}

impl OfxHost {
	/// A client for `config`; the host process starts lazily on the first
	/// submit (or via [`OfxHost::start`]).
	pub fn new(config: OfxHostConfig) -> Result<Arc<Self>> {
		Ok(Arc::new(Self {
			config,
			inner: Mutex::new(HostInner {
				child: None,
				stdin: None,
				input: None,
				output: None,
				slot_bytes: 0,
				slots: 0,
				generation: 0,
				consecutive_failures: 0,
				permanently_dead: false,
				reader: None,
				restarts: 0,
			}),
			wait: Arc::new((
				Mutex::new(WaitState {
					results: HashMap::new(),
					dead: false,
				}),
				Condvar::new(),
			)),
			next_job: AtomicU64::new(1),
			submit_lock: Mutex::new(()),
		}))
	}

	/// Start (or restart) the host now instead of on the first submit.
	pub fn start(&self) -> Result<()> {
		let mut inner = lock(&self.inner);
		Self::ensure_started(&mut inner, &self.config, &self.wait)
	}

	/// True once the crash budget is exhausted.
	pub fn is_permanently_dead(&self) -> bool {
		lock(&self.inner).permanently_dead
	}

	/// Consecutive crashes since the last successful job.
	pub fn failures(&self) -> u32 {
		lock(&self.inner).consecutive_failures
	}

	/// Total host respawns (tests).
	pub fn restarts(&self) -> u64 {
		lock(&self.inner).restarts
	}

	/// Whether a host process is currently running.
	pub fn is_running(&self) -> bool {
		lock(&self.inner).child.is_some()
	}

	/// Submit one plugin job for `spec` (the evaluator's
	/// [`JobSpec::Plugin`]) against `src`, returning the rendered frame.
	/// Transparently re-posts the job across host crashes until the
	/// crash budget is exhausted.
	pub fn submit(&self, spec: &JobSpec, src: &Texture) -> Result<Frame> {
		let _submit = lock(&self.submit_lock);
		let frames = Self::read_job_frames(spec, src)?;
		let job = self.next_job.fetch_add(1, Ordering::Relaxed);
		loop {
			match self.attempt(job, &frames) {
				Ok(frame) => {
					lock(&self.inner).consecutive_failures = 0;
					return Ok(frame);
				}
				Err(SubmitError::Failed(err)) => return Err(Error::Failed(err)),
				Err(SubmitError::Died(why)) => {
					let mut inner = lock(&self.inner);
					// Reap immediately: the crash budget may be exhausted
					// here, and a dead child must not linger (is_running
					// stays truthful) until shutdown.
					Self::reap(&mut inner);
					inner.consecutive_failures += 1;
					if inner.consecutive_failures >= self.config.max_failures {
						inner.permanently_dead = true;
						return Err(Error::Failed(format!(
							"OFX host crashed {} times in a row ({why}); giving up",
							inner.consecutive_failures
						)));
					}
					lock(&self.wait.0).dead = false;
					// Loop: ensure_started spawns a fresh host and the job
					// is sent again unchanged.
				}
			}
		}
	}

	/// Send `plugin_cancel` to the host (sticky flag; the host's progress
	/// reporter then answers false at the plugin's next progressUpdate).
	pub fn cancel(&self) {
		let mut inner = lock(&self.inner);
		if let Some(stdin) = inner.stdin.as_mut() {
			let _ = write_message(stdin, &plugin_cancel_json());
			let _ = stdin.flush();
		}
	}

	/// Stop the host (idempotent). The client is permanently dead
	/// afterwards, mirroring the evaluation fallback.
	pub fn shutdown(&self) {
		let mut inner = lock(&self.inner);
		if let Some(stdin) = inner.stdin.as_mut() {
			let _ = write_message(stdin, &serde_json::json!({ "type": TYPE_SHUTDOWN }));
			let _ = stdin.flush();
		}
		Self::reap(&mut inner);
		inner.permanently_dead = true;
	}

	/// One attempt: ensure a live host, write the inputs, send the job and
	/// wait for its result (or the host's death).
	fn attempt(&self, job: u64, frames: &HostJobFrames) -> std::result::Result<Frame, SubmitError> {
		let mut inner = lock(&self.inner);
		if inner.permanently_dead {
			return Err(SubmitError::Failed(
				"OFX host is permanently dead".to_string(),
			));
		}
		// Size the pools before the first spawn (and restart the host when
		// a job outgrows them; no in-flight job exists — submits are
		// synchronous).
		let needed_bytes = frames.max_bytes();
		let needed_slots = frames.input_slots().max(1);
		if needed_bytes > inner.slot_bytes || needed_slots > inner.slots {
			if inner.child.is_some() {
				Self::reap(&mut inner);
			}
			inner.slot_bytes = needed_bytes.next_power_of_two().max(self.config.slot_bytes);
			inner.slots = needed_slots.next_power_of_two().max(self.config.slots);
		}
		if let Err(err) = Self::ensure_started(&mut inner, &self.config, &self.wait) {
			return Err(SubmitError::Died(err.to_string()));
		}
		let input = inner
			.input
			.clone()
			.ok_or_else(|| SubmitError::Died("OFX host has no input pool".to_string()))?;
		let stdin = inner
			.stdin
			.as_mut()
			.ok_or_else(|| SubmitError::Died("OFX host has no stdin".to_string()))?;

		let mut inputs = Vec::with_capacity(frames.inputs.len());
		for (name, frame) in &frames.inputs {
			let slot = match write_input_frame(&input, frame) {
				Ok(slot) => slot,
				Err(err) => return Err(SubmitError::Failed(err)),
			};
			inputs.push(OfxInputRef {
				name: name.clone(),
				slot,
			});
		}
		let src_slot = match &frames.src {
			Some(frame) => match write_input_frame(&input, frame) {
				Ok(slot) => Some(slot),
				Err(err) => return Err(SubmitError::Failed(err)),
			},
			None => None,
		};
		let msg = OfxJobMsg {
			job,
			type_id: frames.type_id.clone(),
			time: frames.time,
			effect_input_id: frames.effect_input_id.clone(),
			inputs,
			values: frames.values.clone(),
			src_slot,
		};
		if let Err(err) = write_message(stdin, &msg.to_json()).and_then(|_| stdin.flush()) {
			return Err(SubmitError::Died(format!("OFX host stdin: {err}")));
		}
		drop(inner);

		let (wait_lock, cv) = &*self.wait;
		let mut wait = lock(wait_lock);
		loop {
			if let Some(result) = wait.results.remove(&job) {
				return result.map_err(SubmitError::Failed);
			}
			if wait.dead {
				return Err(SubmitError::Died("OFX host exited".to_string()));
			}
			wait = cv.wait(wait).unwrap_or_else(|e| e.into_inner());
		}
	}

	/// Read the job's textures back to CPU frames once (the explicit OFX
	/// boundary) and convert the scalar params to their wire form. The
	/// `src` frame is kept separate: plugin jobs may name no clip for it
	/// (montage) or the evaluator's `src` may be a clone the host cannot
	/// re-identify by pointer.
	fn read_job_frames(spec: &JobSpec, src: &Texture) -> Result<HostJobFrames> {
		let JobSpec::Plugin {
			type_id,
			time,
			effect_input_id,
			inputs,
			values,
			..
		} = spec
		else {
			return Err(Error::Invalid);
		};
		let frames = inputs
			.iter()
			.map(|(name, texture)| Ok((name.clone(), texture.to_frame()?)))
			.collect::<Result<Vec<_>>>()?;
		// A dummy source mirrors the in-process executor's rejection (the
		// host receives no src and fails the job the same way).
		let src = if src.is_dummy() {
			None
		} else {
			Some(src.to_frame()?)
		};
		let values = values
			.iter()
			.filter_map(|(input, value)| {
				WireNodeValue::from_node_value(value).map(|value| WireEffectParam {
					input: input.clone(),
					value,
				})
			})
			.collect();
		Ok(HostJobFrames {
			type_id: type_id.clone(),
			time: *time,
			effect_input_id: effect_input_id.clone().unwrap_or_default(),
			inputs: frames,
			src,
			values,
		})
	}

	/// Create the pools and spawn the host. No-op when it is already up.
	fn ensure_started(
		inner: &mut HostInner,
		config: &OfxHostConfig,
		wait: &Arc<(Mutex<WaitState>, Condvar)>,
	) -> Result<()> {
		if inner.permanently_dead {
			return Err(Error::Failed("OFX host is permanently dead".into()));
		}
		if inner.child.is_some() {
			return Ok(());
		}
		if inner.slot_bytes == 0 {
			inner.slot_bytes = config.slot_bytes;
		}
		if inner.slots == 0 {
			inner.slots = config.slots.max(1);
		}
		inner.generation += 1;
		let generation = inner.generation;
		let slots = inner.slots.max(1);
		let input = ShmRegionView::create(
			&format!("oak-ofx-in-{}-{generation}", std::process::id()),
			slots,
			inner.slot_bytes,
		)?;
		let output = ShmRegionView::create(
			&format!("oak-ofx-out-{}-{generation}", std::process::id()),
			slots,
			inner.slot_bytes,
		)?;
		let bin = resolve_host_bin(config)?;
		let mut command = Command::new(&bin);
		command
			.arg("--ofx-host")
			.args(&config.host_args)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit());
		for (key, value) in &config.env {
			command.env(key, value);
		}
		let mut child = command
			.spawn()
			.map_err(|e| Error::Failed(format!("spawn OFX host {}: {e}", bin.display())))?;
		let mut stdin = child
			.stdin
			.take()
			.ok_or_else(|| Error::Failed("OFX host stdin not piped".into()))?;
		let stdout = child
			.stdout
			.take()
			.ok_or_else(|| Error::Failed("OFX host stdout not piped".into()))?;
		let handshake = HandshakeMsg {
			protocol_version: 1,
			shm_key: output.key().to_string(),
			input_shm_key: input.key().to_string(),
			input_slots: slots as i32,
			output_slots: slots as i32,
			slot_data_bytes: inner.slot_bytes as i64,
			input_slot_data_bytes: inner.slot_bytes as i64,
		};
		write_message(&mut stdin, &handshake.to_json())
			.and_then(|_| stdin.flush())
			.map_err(|e| Error::Failed(format!("OFX host handshake: {e}")))?;
		let reader = spawn_reader(stdout, output.clone(), wait.clone());
		inner.child = Some(child);
		inner.stdin = Some(stdin);
		inner.input = Some(input);
		inner.output = Some(output);
		inner.reader = Some(reader);
		Ok(())
	}

	/// Kill/join the current generation and drop its pools.
	fn reap(inner: &mut HostInner) {
		if let Some(mut child) = inner.child.take() {
			let _ = child.kill();
			let _ = child.wait();
		}
		inner.stdin = None;
		if let Some(reader) = inner.reader.take() {
			let _ = reader.join();
		}
		inner.input = None;
		inner.output = None;
		inner.restarts += 1;
	}
}

// ---------------------------------------------------------------------------
// Process-wide client slot
// ---------------------------------------------------------------------------

static CLIENT: OnceLock<Mutex<Option<Arc<OfxHost>>>> = OnceLock::new();

fn client_slot() -> &'static Mutex<Option<Arc<OfxHost>>> {
	CLIENT.get_or_init(|| Mutex::new(None))
}

/// Install (or clear) the process-wide host client. Installed by the
/// render manager when the thread pipeline is selected; the evaluator
/// prefers the client over the in-process plugin executor while it is
/// installed.
pub fn install_client(client: Option<Arc<OfxHost>>) {
	*lock(client_slot()) = client;
}

/// The installed client, if any.
pub fn client() -> Option<Arc<OfxHost>> {
	lock(client_slot()).clone()
}

/// Broadcast `plugin_cancel` to the host (the app's cancel button; the
/// worker pool gets its own broadcast in `procpool`).
pub fn request_cancel_all() {
	if let Some(host) = client() {
		host.cancel();
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the host executable: the configured path, `OAK_WORKER_BIN`, or
/// an `oak-worker` binary next to the current executable (the host is a
/// mode of the worker binary, not a separate target).
fn resolve_host_bin(config: &OfxHostConfig) -> Result<PathBuf> {
	if let Some(path) = &config.host_bin {
		return Ok(path.clone());
	}
	if let Ok(path) = std::env::var("OAK_WORKER_BIN") {
		return Ok(PathBuf::from(path));
	}
	let exe = std::env::current_exe()
		.map_err(|e| Error::Failed(format!("resolve oak-worker: current exe: {e}")))?;
	let candidate = exe
		.parent()
		.ok_or_else(|| Error::Failed("resolve oak-worker: no exe parent".into()))?
		.join(format!("oak-worker{}", std::env::consts::EXE_SUFFIX));
	if candidate.exists() {
		return Ok(candidate);
	}
	Err(Error::Failed(format!(
		"oak-worker binary not found at {}; set OfxHostConfig::host_bin or OAK_WORKER_BIN",
		candidate.display()
	)))
}

/// Write one input frame into the host's input pool, returning its slot.
fn write_input_frame(region: &ShmRegionView, frame: &Frame) -> std::result::Result<u32, String> {
	let pool = region.pool();
	let mut slot = 0u32;
	if !unsafe { pool.acquire(&mut slot) } {
		return Err("OFX host input pool is full".to_string());
	}
	let bytes = frame.data.len();
	if bytes > region.slot_data_bytes() {
		unsafe {
			pool.release(slot);
		}
		return Err(format!(
			"input frame is {bytes} bytes, larger than the OFX host slot ({})",
			region.slot_data_bytes()
		));
	}
	// SAFETY: `slot` was just acquired from this live pool; the copy and
	// meta fill are the producer side of the SPSC protocol.
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
			return Err("OFX host input publish failed".to_string());
		}
	}
	Ok(slot)
}

/// Copy one output frame out of the host's output pool and release the
/// slot (the consumer side of the SPSC protocol).
fn read_output_frame(
	region: &ShmRegionView,
	slot: u32,
) -> std::result::Result<Frame, String> {
	let pool = region.pool();
	let mut consumed = 0u32;
	if !unsafe { pool.consume(&mut consumed) } {
		return Err("OFX host output slot missing".to_string());
	}
	if consumed != slot {
		// Keep the pool consistent: recycle the unexpected entry.
		unsafe {
			pool.release(consumed);
		}
		return Err(format!(
			"OFX host output slot mismatch: expected {slot}, got {consumed}"
		));
	}
	let meta = region.meta_copy(slot);
	if meta.width <= 0 || meta.height <= 0 || meta.data_size < 0 {
		unsafe {
			pool.release(slot);
		}
		return Err("OFX host returned invalid frame metadata".to_string());
	}
	let len = (meta.data_size as usize).min(region.slot_data_bytes());
	let data = region.slot_bytes(slot)[..len].to_vec();
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

/// The host's stdout reader: forwards progress, delivers results and
/// flags death for the blocked submitter.
fn spawn_reader(
	stdout: std::process::ChildStdout,
	output: Arc<ShmRegionView>,
	wait: Arc<(Mutex<WaitState>, Condvar)>,
) -> JoinHandle<()> {
	std::thread::Builder::new()
		.name("oak-ofx-host-reader".into())
		.spawn(move || {
			let mut reader = BufReader::new(stdout);
			let mut line = String::new();
			loop {
				line.clear();
				match reader.read_line(&mut line) {
					Ok(0) | Err(_) => break,
					Ok(_) => {}
				}
				let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
					continue;
				};
				match value.get("type").and_then(|t| t.as_str()) {
					Some(TYPE_OFX_RESULT) => {
						let msg: OfxResultMsg = serde_json::from_value(value).unwrap_or_default();
						let result = if msg.slot >= 0 {
							read_output_frame(&output, msg.slot as u32)
						} else {
							Err(msg.error)
						};
						let (state, cv) = &*wait;
						lock(state).results.insert(msg.job, result);
						cv.notify_all();
					}
					Some(TYPE_PLUGIN_PROGRESS) => {
						let msg: PluginProgressMsg = serde_json::from_value(value).unwrap_or_default();
						if let Some(cb) = plugin_progress_cb() {
							cb(msg.label, msg.message, msg.fraction);
						}
					}
					Some(TYPE_ERROR) => {
						let message = value
							.get("message")
							.and_then(|m| m.as_str())
							.unwrap_or("unknown OFX host error");
						eprintln!("OFX host error: {message}");
					}
					_ => {}
				}
			}
			let (state, cv) = &*wait;
			lock(state).dead = true;
			cv.notify_all();
		})
		.expect("spawn OFX host reader")
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::eval::generate_frame;
	use oak_core::Rational;

	fn plugin_spec(type_id: &str) -> JobSpec {
		JobSpec::Plugin {
			instance: 0,
			type_id: type_id.to_string(),
			time: 0.0,
			effect_input_id: None,
			inputs: Vec::new(),
			values: Vec::new(),
		}
	}

	fn small_texture() -> Texture {
		Texture::wrap_frame(
			generate_frame(Rational::new(0, 1), (4, 4), PixelFormat::F32).unwrap(),
		)
	}

	#[test]
	#[cfg(unix)]
	fn submit_exhausts_the_crash_budget() {
		let host = OfxHost::new(OfxHostConfig {
			host_bin: Some(PathBuf::from("/bin/false")),
			max_failures: 3,
			..Default::default()
		})
		.unwrap();
		let spec = plugin_spec("org.oak.missing");
		let src = small_texture();
		assert!(
			host.submit(&spec, &src).is_err(),
			"a host that never comes up must fail the submit"
		);
		assert_eq!(host.failures(), 3, "three consecutive crashes");
		assert!(host.is_permanently_dead());
		// Further submissions fail fast without spawning again.
		let restarts = host.restarts();
		assert!(host.submit(&spec, &src).is_err());
		assert_eq!(host.restarts(), restarts, "no more spawns after the budget");
	}

	#[test]
	#[cfg(unix)]
	fn shutdown_is_idempotent_and_marks_dead() {
		let host = OfxHost::new(OfxHostConfig {
			host_bin: Some(PathBuf::from("/bin/false")),
			..Default::default()
		})
		.unwrap();
		host.shutdown();
		host.shutdown();
		assert!(host.is_permanently_dead());
		assert!(!host.is_running());
	}

	#[test]
	fn read_job_frames_keeps_the_source_mapping() {
		let mut source = generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32).unwrap();
		source.data[0] = 0x7F;
		let source = Texture::wrap_frame(source);
		// The same texture value appears under the effect input name.
		let spec = JobSpec::Plugin {
			instance: 0,
			type_id: "org.oak.test-plugin".into(),
			time: 1.0,
			effect_input_id: Some("Source".into()),
			inputs: vec![("Source".to_string(), source.clone())],
			values: vec![(
				"gain".to_string(),
				oak_node::value::NodeValue::Float(0.5),
			)],
		};
		let frames = OfxHost::read_job_frames(&spec, &source).unwrap();
		assert!(frames.src.is_some(), "a real source is sent separately");
		assert_eq!(frames.src.as_ref().unwrap().data[0], 0x7F);
		assert_eq!(frames.effect_input_id, "Source");
		assert_eq!(frames.inputs.len(), 1);
		assert_eq!(frames.inputs[0].1.data[0], 0x7F);
		assert_eq!(frames.input_slots(), 2, "named clip + source");
		assert_eq!(
			frames.values,
			vec![WireEffectParam {
				input: "gain".into(),
				value: WireNodeValue::Float(0.5),
			}]
		);
	}

	#[test]
	fn wait_state_wakes_on_result() {
		// The wait protocol itself (independent of a real host).
		let host = OfxHost::new(OfxHostConfig::default()).unwrap();
		let (state, cv) = &*host.wait;
		let mut wait = lock(state);
		// No result yet and not dead: a timed wait returns empty-handed.
		let (guard, timeout) = cv
			.wait_timeout(wait, std::time::Duration::from_millis(10))
			.unwrap();
		wait = guard;
		assert!(timeout.timed_out());
		assert!(!wait.dead);
		// A result wakes a waiter.
		let frame = Frame::dummy();
		wait.results.insert(7, Ok(frame));
		let (guard, _) = cv
			.wait_timeout(wait, std::time::Duration::from_millis(10))
			.unwrap();
		wait = guard;
		assert!(wait.results.remove(&7).is_some());
	}
}
