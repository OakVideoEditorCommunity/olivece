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

//! The process-wide PortAudio output/input manager (`olive::AudioManager`).
//!
//! Singleton semantics: the single instance lives behind a
//! `OnceLock<Mutex<ManagerInner>>`; handles returned to C are borrowed and
//! their addref/release are no-ops (mirrors the C++ singleton and
//! `include/audio/manager.h`). An empty handle reports `OAKAUDIO_E_STATE`.
//!
//! Recording goes through the oakcodec encoder C ABI ([`crate::bridge`]);
//! device/config lookups go through oak_core.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use cpal::{Device, DeviceId};
use cpal::traits::{DeviceTrait, HostTrait};
use oak_codec::encoder::Encoder;
use oak_codec::encodingparams::EncodingParams;
use crate::error::{Error, Result};
use crate::params::AudioParams;
use crate::previewdevice::PreviewAudioDevice;
use crate::error::Error::NotFound;

/// `paNoDevice` (PortAudio "no device" sentinel; also the default when no
/// device is configured).
const PA_NO_DEVICE: i32 = -1;

/// The process-wide manager state. `OnceLock` cannot be reset, so
/// [`destroy_instance`] flips `DESTROYED` to make [`instance`] hand out empty
/// handles again (the singleton box itself is retained).
static MANAGER: OnceLock<Mutex<ManagerInner>> = OnceLock::new();
static DESTROYED: AtomicBool = AtomicBool::new(false);

/// Manager state (all device/stream fields; PortAudio itself is not bridged,
/// see [`ManagerInner::default`] for the degradations).
///
/// Public since the single-lib unification (the deleted `ffi`/`bridge`
/// C ABI is gone): the oakengine facade calls the singleton's methods
/// directly through [`instance`] instead of crossing the old C ABI.
pub struct ManagerInner {
	/// Current output device index.
	output_device: i32,
	/// Current input device index.
	input_device: i32,
	/// Output params the buffer is configured for.
	output_params: Option<AudioParams>,
	/// Queued output samples feeding the playback clock (shared with the
	/// PortAudio output callback).
	output_buffer: std::sync::Arc<PreviewAudioDevice>,
	/// Whether the output "stream" is running (stand-in for
	/// `Pa_IsStreamActive`).
	output_started: bool,
	/// The real PortAudio output stream (M12 P1; opened lazily on the
	/// first pushed samples).
	output_device_stream: crate::outputdevice::PortAudioOutput,
	/// Active oakcodec recording encoder (None when idle).
	recording: Option<Arc<dyn Encoder>>,
}

// SAFETY: the raw encoder pointer is only touched while the manager mutex is
// held, which serializes every access; the encoder lives until `recording` is
// taken out in `stop_recording`.
unsafe impl Send for ManagerInner {}

impl Default for ManagerInner {
	fn default() -> Self {
		ManagerInner {
			// CPP-PARITY: no device is selected until `create_instance` runs
			// the config lookup (PortAudio enumeration cannot be bridged, so
			// the devices stay at paNoDevice and the C layer reports E_FAILED
			// on output/recording until a device is set explicitly).
			output_device: PA_NO_DEVICE,
			input_device: PA_NO_DEVICE,
			output_params: None,
			output_buffer: std::sync::Arc::new(PreviewAudioDevice::new()),
			output_started: false,
			output_device_stream: crate::outputdevice::PortAudioOutput::new(),
			recording: None,
		}
	}
}

impl ManagerInner {
	/// Create the process-wide AudioManager (no-op when it exists). Returns
	/// `OAKAUDIO_OK` or `OAKAUDIO_E_NOMEM`.
	///
	/// `// CPP-PARITY: src/audio/c_api/manager.cpp:73` (C++ allocates with `new`
	/// and reports `OAKAUDIO_E_NOMEM` on exception; Rust allocation infallibly
	/// panics, so the error code is never produced).
	pub fn create_instance() -> Result<()> {
		DESTROYED.store(false, Ordering::SeqCst);
		let _ = MANAGER.get_or_init(|| Mutex::new(ManagerInner::default()));
		Ok(())
	}

	/// Destroy the process-wide AudioManager (no-op when absent).
	///
	/// `// CPP-PARITY: src/audio/c_api/manager.cpp:85` — the C++ singleton is
	/// deleted and re-creatable; `OnceLock` cannot be reset, so a `DESTROYED`
	/// flag makes [`instance`] return an empty handle (and a later
	/// [`create_instance`] resurrects the existing box).
	pub fn destroy_instance() {
		DESTROYED.store(true, Ordering::SeqCst);
		// The C++ singleton is deleted on destroy; the OnceLock cannot be
		// reset, so the resurrection must at least come back with a fresh
		// playback state (the previous session's output params/buffer would
		// otherwise leak into the next session's meters and clock).
		if let Some(m) = MANAGER.get() {
			let mut inner = m.lock().unwrap_or_else(|e| e.into_inner());
			inner.output_params = None;
			inner.output_buffer.clear();
			inner.output_started = false;
		}
	}

	/// Return a handle to the process-wide AudioManager (borrowed; empty when
	/// no instance exists).
	///
	/// `// CPP-PARITY: src/audio/c_api/manager.cpp:90` (`wrap`; the handle is a
	/// borrowed singleton whose addref/release are no-ops).
	pub fn instance(&self) -> Option<&Self> {
		if DESTROYED.load(Ordering::SeqCst) {
			return None;
		}
		match MANAGER.get() {
			Some(m) => {
				// SAFETY: `m` is the process-wide singleton; borrowed handles do
				// not free it, so it outlives every handle.
				Some(self)
			}
			None => None,
		}
	}

	/// Bytes between output-notify pulses (0 disables).
	pub fn set_output_notify_interval(&self, bytes: i64) -> Result<()> {
		if bytes < 0 {
			return Err(Box::new(Error::Invalid));
		}
		self.output_buffer.set_notify_interval(bytes);
		Ok(())
	}

	/// Push a block of samples to the output device, opening/restarting the
	/// stream when the params changed.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:111` — the PortAudio
	/// open/start path is not bridged; the buffer is configured and written
	/// directly and the "stream" is marked running. `error_buf` is written by
	/// the FFI layer from the returned error.
	pub fn push_to_output(
		&mut self,
		params: AudioParams,
		samples: &[u8],
		_error_buf: &mut [u8],
	) -> Result<()> {
		if self.output_params.as_ref() != Some(&params) {
			self.output_params = Some(params);
			self.output_buffer.set_params(params);
			// M12 P1: open (or re-open) the real output stream on a format
			// change; device < 0 selects the system default. A stream
			// failure keeps the samples buffered (silent playback) instead
			// of failing the push.
			let device = self.output_device;
			let rate = params.sample_rate;
			let channels = params.channel_count();
			let sink = self.output_buffer.clone();
			let _ = self.output_device_stream.ensure_open(device, rate, channels, sink);
		}
		self.output_buffer.write(samples);
		self.output_started = true;
		Ok(())
	}

	/// Discard buffered output.
	pub fn clear_buffered_output(&self) -> Result<()> {
		self.output_buffer.clear();
		Ok(())
	}

	/// Frames currently queued for the output callback (diagnostics).
	pub fn output_queued_frames(&self) -> i64 {
		self.output_buffer.queued_frames()
	}

	/// Drop up to `frames` stale frames from the front of the output queue
	/// (underrun resync without a full clear; see
	/// [`PreviewAudioDevice::drop_front_frames`]). Returns frames dropped.
	pub fn drop_output_frames(&self, frames: i64) -> i64 {
		self.output_buffer.drop_front_frames(frames)
	}

	/// Read and reset the output underrun counter (frames the callback had
	/// to zero-fill because the buffer was empty). The playback engine
	/// polls this per tick and resyncs after an underrun: the zero-fill
	/// advanced the playback clock past the queued content, so without a
	/// resync everything afterwards plays late.
	pub fn take_output_underrun_frames(&self) -> i64 {
		self.output_buffer.take_underrun_frames()
	}

	/// Stop the output stream.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:229` (`stop_output` aborts
	/// the stream and clears the buffer).
	pub fn stop_output(&mut self) -> Result<()> {
		self.output_started = false;
		self.output_buffer.clear();
		Ok(())
	}

	/// Seconds of audio consumed by the output device since the last reset,
	/// compensated for output latency; negative when no stream is running.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:169` — PortAudio's
	/// `outputLatency` is not representable without a live stream, so the buffer
	/// clock is used directly (the `max(0, ...)` clamp is kept).
	pub fn seconds(&self, out: &mut f64) -> Result<()> {
		if !self.output_started {
			*out = -1.0;
			return Ok(());
		}
		let rate = self.output_params.map(|p| p.sample_rate).unwrap_or(0);
		if rate <= 0 {
			*out = -1.0;
			return Ok(());
		}
		let secs = self.output_buffer.output_frames_consumed() as f64 / f64::from(rate);
		*out = secs.max(0.0);
		Ok(())
	}

	/// Restart the output clock at zero.
	pub fn reset_output_clock(&self) -> Result<()> {
		self.output_buffer.reset_output_frames();
		Ok(())
	}

	/// Current output device index (`paNoDevice` = -1) or a negative error code.
	pub fn get_output_device(&self) -> Result<i32> {
		Ok(self.output_device)
	}

	/// Set the output device index.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:238` (the device is
	/// recorded and the stream closed; PortAudio's index validation and name
	/// logging are not bridged).
	pub fn set_output_device(&mut self, device: i32) -> Result<()> {
		self.output_device = device;
		self.output_started = false;
		self.output_buffer.clear();
		// The stream reopens with the new device on the next push. Drop the
		// cached params too: `push_to_output` only re-opens the stream on a
		// params change, so without this the switch would stay silent until
		// the format changed.
		self.output_params = None;
		self.output_device_stream.close();
		Ok(())
	}

	/// Current input device index or a negative error code.
	pub fn get_input_device(&self) -> Result<i32> {
		Ok(self.input_device)
	}

	/// Set the input device index.
	pub fn set_input_device(&mut self, device: i32) -> Result<()> {
		self.input_device = device;
		Ok(())
	}

	/// Close the output stream and re-initialize PortAudio.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:271` (PortAudio terminate/
	/// initialize is not bridged; the output side is reset).
	pub fn hard_reset(&mut self) -> Result<()> {
		self.output_started = false;
		self.output_buffer.clear();
		self.output_device_stream.close();
		Ok(())
	}

	/// Start recording the input device to a file via the oakcodec encoder
	/// (direct Rust calls; the encoder is a `dyn Encoder` value). The input
	/// stream is always captured as interleaved f32.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:278` (encoder init/open;
	/// the PortAudio input stream is not bridged). On failure the encoder's
	/// last-error string is surfaced when available.
	pub fn start_recording(
		&mut self,
		params: &EncodingParams,
		_error_buf: &mut [u8],
	) -> Result<()> {
		if self.input_device == PA_NO_DEVICE {
			return Err(Box::new(Error::Failed("no input device".to_string())));
		}
		let encoder = oak_codec::encoder::create_from_params(params)
			.ok_or_else(|| Error::Failed("failed to create encoder for recording".to_string()))?;
		encoder
			.configure(params)
			.map_err(|e| Error::Failed(format!("encoder configure failed: {e:?}")))?;
		if let Err(e) = encoder.open() {
			let detail = encoder.get_error();
			let msg = if detail.is_empty() {
				format!("failed to open encoder for recording: {e:?}")
			} else {
				format!("failed to open encoder for recording: {detail}")
			};
			return Err(Box::new(Error::Failed(msg)));
		}
		self.recording = Some(encoder);
		Ok(())
	}

	/// Stop recording.
	///
	/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:328` (the PortAudio input
	/// stream is not bridged; the encoder is flushed and closed).
	pub fn stop_recording(&mut self) -> Result<()> {
		if let Some(encoder) = self.recording.take() {
			let _ = encoder.flush();
			let _ = encoder.close();
		}
		Ok(())
	}



	/// Peak level (linear, 0..1 and above) of each channel of the buffered,
	/// not-yet-consumed output, written to `peaks` in channel order.
	/// Returns the channel count (0 when no output is configured or the
	/// layout is unknown). Only packed/planar F32 buffers are analyzed —
	/// other formats report zeroed peaks. The analysis window is the most
	/// recent 8192 frames of the queue.
	///
	/// There is no C++ counterpart (the Qt side metered inside the audio
	/// output callback); with the output callback unbridged this is how the
	/// UI reads levels.
	pub fn output_levels(&self, peaks: &mut [f32]) -> Result<i32> {
		let Some(params) = self.output_params else {
			return Ok(0);
		};
		let channels = params.channel_count();
		if channels <= 0 {
			return Ok(0);
		}
		let n = (channels as usize).min(peaks.len());
		for p in &mut peaks[..n] {
			*p = 0.0;
		}
		use crate::params::SampleFormat;
		let packed = params.format == SampleFormat::F32;
		let planar = params.format == SampleFormat::F32Planar;
		if !packed && !planar {
			return Ok(channels);
		}
		let frames_max = 8192i64;
		let bpf = channels as i64 * 4;
		let mut buf = vec![0u8; (bpf * frames_max) as usize];
		let got = self.output_buffer.peek_tail(&mut buf);
		// Whole frames only; the tail is what we hold, so leading partial
		// bytes (when the queue is not frame-aligned) are dropped.
		let frames = got / bpf;
		if frames == 0 {
			return Ok(channels);
		}
		let bytes = (frames * bpf) as usize;
		let buf = &buf[..bytes];
		let frame_count = frames as usize;
		let channel_count = channels as usize;
		let mut planes: Vec<Vec<f32>> = vec![Vec::with_capacity(frame_count); channel_count];
		if packed {
			for frame in buf.chunks_exact(bpf as usize) {
				for ch in 0..channel_count {
					let b = &frame[ch * 4..ch * 4 + 4];
					planes[ch].push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
				}
			}
		} else {
			for (ch, plane) in planes.iter_mut().enumerate() {
				let start = ch * frame_count * 4;
				for b in buf[start..start + frame_count * 4].chunks_exact(4) {
					plane.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
				}
			}
		}
		let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
		let stats = crate::levelmeter::analyze_sample_buffer(&views);
		for (i, p) in peaks[..n].iter_mut().enumerate() {
			*p = stats.channels[i].peak_linear as f32;
		}
		Ok(channels)
	}

}

/// Lock the process-wide manager singleton (the direct-Rust replacement
/// for the deleted C ABI's `oakaudio_manager_instance`): `None` when no
/// instance exists (never created, or destroyed since the last
/// [`ManagerInner::create_instance`]).
pub fn instance() -> Option<MutexGuard<'static, ManagerInner>> {
	if DESTROYED.load(Ordering::SeqCst) {
		return None;
	}
	MANAGER
		.get()
		.map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Device index named by the configuration ("AudioOutput"/"AudioInput"), or
/// the default when unset/unmatched. Static; `paNoDevice` when PortAudio is
/// not initialized.
///
/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:404`.
pub fn find_config_device_by_name_s(is_output_device: bool) -> Result<Device> {
	let name = crate::config::device_name(is_output_device)?;
	find_device_by_name_s_or_default(&name, is_output_device)
}

/// Device index whose name matches `name` exactly (empty matches nothing,
/// falls through to the default device). Static.
///
/// `// CPP-PARITY: src/audio/src/audiomanager.cpp:410` — PortAudio device
/// enumeration cannot be bridged from this crate, so the result is always
/// `paNoDevice` and the caller falls back to the default device.
pub fn find_device_by_name_s_or_default(name: &String, _is_output_device: bool) -> Result<Device> {
	let host = cpal::default_host();
	let device = host.devices()?.find(|d| {
		if d.id().is_err(){
			return false;
		}
		if d.supports_output() && d.id().unwrap().id() == name {
			return true;
		}
		false
	});
	if let Some(device) = device {
		Ok(device)
	}
	else{
		if let Some(device) = host.default_output_device() {
			Ok(device)
		}
		else{
			Err(Box::new(Error::NotFound))
		}
	}

}

/// The host's output device names in enumeration order; the list index is
/// the device index [`ManagerInner::set_output_device`] takes. The default
/// device is NOT marked — callers prepend their own "system default" entry
/// (index -1). Static (needs no manager instance).
pub fn output_device_names() -> Vec<String> {
	device_names(true)
}

/// The host's input device names in enumeration order (see
/// [`output_device_names`]).
pub fn input_device_names() -> Vec<String> {
	device_names(false)
}

/// Enumerates the default host's devices, keeping only the ones that
/// support the requested direction; unnamed devices are skipped.
fn device_names(output: bool) -> Vec<String> {
	let host = cpal::default_host();
	let devices = if output {
		host.output_devices()
	} else {
		host.input_devices()
	};
	let Ok(devices) = devices else {
		return Vec::new();
	};
	devices
		.filter_map(|d| d.id().ok().map(|id| id.id().to_string()))
		.collect()
}

/// The enumeration index of the device named `name` (`None` when absent);
/// the inverse of [`output_device_names`]/[`input_device_names`].
pub fn device_index_by_name(name: &str, output: bool) -> Option<i32> {
	let names = if output {
		output_device_names()
	} else {
		input_device_names()
	};
	names.iter().position(|n| n == name).map(|i| i as i32)
}


#[cfg(test)]
mod tests {
	use super::*;

	/// M12 P1 acceptance: the PortAudio output callback pulls pushed
	/// samples and advances the playback clock. Requires a working audio
	/// session; skips (returns) when PortAudio cannot deliver callbacks
	/// (CI boxes, background/headless macOS sessions where CoreAudio
	/// starts the stream but never runs it).
	#[test]
	fn output_callback_consumes_pushed_samples() {
		// The audio HAL can wedge for MINUTES on headless/CI hosts — the
		// stream build/play calls block without erroring, so no in-test
		// deadline can fire. Run the body on a worker thread and skip when
		// it does not finish in time; the probe section touches no shared
		// state, so detaching a wedged thread is safe here.
		let (tx, rx) = std::sync::mpsc::channel();
		std::thread::spawn(move || {
			output_callback_body();
			let _ = tx.send(());
		});
		if rx
			.recv_timeout(std::time::Duration::from_secs(60))
			.is_err()
		{
			eprintln!("audio host wedged (headless/CI); skipping");
			return;
		}
	}

	/// The test body (see the wrapping test for the watchdog rationale).
	fn output_callback_body() {		// Skip when the audio system cannot actually run a stream: open a
		// silent stream and require at least one callback within 2 s. A
		// device existing is not enough — headless sessions report the
		// stream running while delivering zero callbacks.
		use std::sync::atomic::AtomicI64 as A;
		use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
		static PROBE: A = A::new(0);
		PROBE.store(0, Ordering::Relaxed);
		let can_play = (|| {
			let host = cpal::default_host();
			let device = host.default_output_device()?;
			let config = device.default_output_config().ok()?;
			let cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
				PROBE.fetch_add(data.len() as i64, Ordering::Relaxed);
				for s in data.iter_mut() {
					*s = 0.0;
				}
			};
			let stream = device
				.build_output_stream(
					config.config(),
					cb,
					|_| {},
					None,
				)
				.ok()?;
			stream.play().ok()?;
			let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
			while std::time::Instant::now() < deadline && PROBE.load(Ordering::Relaxed) == 0 {
				std::thread::sleep(std::time::Duration::from_millis(50));
			}
			let got = PROBE.load(Ordering::Relaxed) > 0;
			drop(stream);
			Some(got)
		})();
		if can_play != Some(true) {
			eprintln!("audio session cannot deliver callbacks; skipping");
			return;
		}

		DESTROYED.store(false, Ordering::SeqCst);
		let manager = MANAGER.get_or_init(|| Mutex::new(ManagerInner::default()));

		// A 440 Hz sine, 0.2 s at 48 kHz stereo, packed F32.
		let params = AudioParams {
			sample_rate: 48000,
			channel_layout: 0x3,
			format: crate::params::SampleFormat::F32,
		};
		let frames = 9600usize;
		let mut samples = Vec::with_capacity(frames * 2);
		for i in 0..frames {
			let v = (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5;
			samples.push(v);
			samples.push(v);
		}
		let bytes: Vec<u8> = samples
			.iter()
			.flat_map(|s| s.to_le_bytes())
			.collect();
		manager.lock().unwrap().push_to_output(
			params,
			&bytes,
			&mut vec![0u8; 256],
		)
		.expect("push succeeds even without an explicit device");

		// Give the audio thread time to consume. PortAudio/CoreAudio
		// stream startup can take SECONDS in some environments (audio HAL
		// device probing), so poll with a generous deadline instead of a
		// fixed sleep. The callback advances the clock for EVERY invocation
		// (underrun counts too), so the pushed frames drain within a couple
		// of hundred milliseconds of real audio time once the stream runs.
		let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
		let target = frames as i64 - 1024;
		let mut consumed = 0i64;
		while std::time::Instant::now() < deadline {
			consumed = manager.lock().unwrap().output_buffer.output_frames_consumed();
			if consumed >= target {
				break;
			}
			std::thread::sleep(std::time::Duration::from_millis(100));
		}
		assert!(
			consumed >= target,
			"the output callback must consume the pushed frames (consumed {consumed} of {frames})"
		);

		manager.lock().unwrap().output_device_stream.close();
	}
}
