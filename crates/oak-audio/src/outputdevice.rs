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

//! M12 P1: the real audio output device (cpal, direct crate call).
//!
//! The playback stream's callback pulls interleaved bytes from the
//! shared [`PreviewAudioDevice`] and advances the output clock; underrun
//! (empty buffer) writes silence. The stream is opened lazily on the
//! first pushed samples and re-opened when the format or device changes.
//!
//! The callback never allocates or locks anything but the preview
//! device's mutex (a short critical section; the buffer is drained in
//! whole frames).

use std::cell::RefCell;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
	BufferSize, Device, Host, SampleFormat, Stream, StreamConfig,
	SupportedBufferSize,
};
use crate::previewdevice::PreviewAudioDevice;

/// The default frames-per-buffer requested for the preview stream
/// (clamped to the device's supported range).
const FRAMES_PER_BUFFER: u32 = 512;

/// OAK_DEBUG_AUDIO-gated diagnostics for the output stream.
fn stream_dbg_enabled() -> bool {
	static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	*ENABLED.get_or_init(|| std::env::var_os("OAK_DEBUG_AUDIO").is_some())
}

/// An open (or openable) cpal output stream.
pub struct PortAudioOutput {
	/// Audio host/session (created lazily; `None` when unavailable).
	host: Option<Host>,
	/// The live stream (None when closed).
	stream: Option<Stream>,
	/// The (device, rate, channels) the stream was opened with.
	opened: Option<(i32, i32, i32)>,
}

/// Backwards-compatible name for [`PortAudioOutput`].
pub type AudioOutput = PortAudioOutput;

impl PortAudioOutput {
	/// Create the device (the host is created lazily on first use).
	pub fn new() -> PortAudioOutput {
		PortAudioOutput {
			host: Some(cpal::default_host()),
			stream: None,
			opened: None,
		}
	}

	/// Whether an output stream is currently running.
	pub fn is_running(&self) -> bool {
		self.stream.is_some()
	}

	/// Close the stream (pause + drop).
	pub fn close(&mut self) {
		if let Some(stream) = self.stream.take() {
			let _ = stream.pause();
		}
		self.opened = None;
	}

	/// Ensure an output stream is open for `(device, rate, channels)` and
	/// pulling from `sink`. `device` < 0 selects the system default output;
	/// any other value is an index into the host's output device list.
	/// Failures leave the output silent (samples still buffer).
	pub fn ensure_open(
		&mut self,
		device: i32,
		rate: i32,
		channels: i32,
		sink: Arc<PreviewAudioDevice>,
	) -> Result<(), String> {
		if rate <= 0 || channels <= 0 {
			return Err("invalid output format".into());
		}
		if self.opened == Some((device, rate, channels)) && self.stream.is_some() {
			return Ok(());
		}
		self.close();

		if self.host.is_none(){
			self.host = Some(cpal::default_host())
		}

		let host = self.host.as_ref().unwrap();

		let output_device = resolve_device(&host, device)
			.ok_or_else(|| "no output device available".to_string())?;
		let config = pick_config(&output_device, rate, channels)?;
		if stream_dbg_enabled() {
			eprintln!(
				"[audio-stream] config: {} Hz, {} ch, buffer {:?}",
				u32::from(config.sample_rate),
				config.channels,
				config.buffer_size
			);
		}

		// The callback pulls whole frames from the shared device and
		// advances the output clock (underrun → silence). `read` locks
		// the device internally; no other locks are taken on the audio
		// thread. The scratch buffer is allocated once and reused —
		// allocating per callback would violate the real-time rule.
		let channels_usize = channels.max(1) as usize;
		let sink_cb = sink.clone();
		let scratch = RefCell::new(Vec::<u8>::new());
		// OAK_DEBUG_AUDIO: true device-side request rate (callbacks/s and
		// frames/s), aggregated once per second — this is the ground truth
		// for production/consumption mismatch hunts.
		let dbg_stats = RefCell::new((0u64, 0u64, std::time::Instant::now()));
		let callback = move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
			let total = out.len();
			if stream_dbg_enabled() {
				let mut st = dbg_stats.borrow_mut();
				st.0 += total as u64;
				st.1 += 1;
				let el = st.2.elapsed();
				if el >= std::time::Duration::from_secs(1) {
					let secs = el.as_secs_f64();
					eprintln!(
						"[audio-stream] callback: {:.1} calls/s, {:.0} samples/s ({} ch)",
						st.1 as f64 / secs,
						st.0 as f64 / secs,
						channels_usize
					);
					st.0 = 0;
					st.1 = 0;
					st.2 = std::time::Instant::now();
				}
			}
			let mut scratch = scratch.borrow_mut();
			scratch.resize(total * 4, 0);
			let got = sink_cb.read(scratch.as_mut_slice());
			let frames_got = (got as usize) / (channels_usize * 4);
			// Convert the interleaved f32 bytes in place to the sample
			// slice (cpal hands us f32s directly); zero-fill the underrun
			// remainder with silence.
			let n_samples = frames_got * channels_usize;
			for (i, s) in out.iter_mut().enumerate() {
				if i < n_samples {
					let b = &scratch[i * 4..i * 4 + 4];
					*s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
				} else {
					*s = 0.0;
				}
			}
			sink_cb.add_output_frames((total / channels_usize) as i64);
			// Account the zero-filled tail separately so the engine can
			// resync after an underrun instead of drifting out of sync.
			sink_cb.add_underrun_frames((total / channels_usize - frames_got) as i64);
		};
		let err_callback = |err: cpal::Error| {
			eprintln!("output stream error: {err}");
		};

		let stream = output_device
			.build_output_stream(config, callback, err_callback, None)
			.map_err(|e| format!("output stream open failed: {e}"))?;
		stream
			.play()
			.map_err(|e| format!("output stream start failed: {e}"))?;
		self.stream = Some(stream);
		self.opened = Some((device, rate, channels));
		Ok(())
	}
}

impl Default for PortAudioOutput {
	fn default() -> Self {
		PortAudioOutput::new()
	}
}

/// Resolve a PortAudio-style device index to a cpal device: an index >= 0
/// picks the Nth output device of the host; anything else (notably -1,
/// `paNoDevice`) falls back to the host's default output device.
fn resolve_device(host: &Host, index: i32) -> Option<Device> {
	if index >= 0 {
		if let Ok(mut devices) = host.output_devices() {
			if let Some(device) = devices.nth(index as usize) {
				return Some(device);
			}
		}
	}
	host.default_output_device()
}

/// Pick an F32 stream config for `(rate, channels)`. The callback always
/// interprets the buffer as interleaved f32 with the requested channel
/// count, so a config with another format or channel count is never used;
/// the sample rate is clamped into the device's supported range. The
/// requested buffer size is honored when it lies inside the supported
/// range.
fn pick_config(device: &Device, rate: i32, channels: i32) -> Result<StreamConfig, String> {
	let want_rate = rate as u32;
	let want_channels = channels as u16;

	if let Ok(mut configs) = device.supported_output_configs() {
		while let Some(c) = configs.next() {
			if c.sample_format() != SampleFormat::F32 || c.channels() != want_channels {
				continue;
			}
			let rate = want_rate.clamp(c.min_sample_rate(), c.max_sample_rate());
			let buffer_size = match c.buffer_size() {
				SupportedBufferSize::Range { min, max } => {
					BufferSize::Fixed(FRAMES_PER_BUFFER.clamp(*min, *max))
				}
				SupportedBufferSize::Unknown => BufferSize::Default,
			};
			return Ok(StreamConfig {
				channels: want_channels,
				sample_rate: rate,
				buffer_size,
			});
		}
	}

	let fallback = device
		.default_output_config()
		.map_err(|e| format!("output device config unavailable: {e}"))?;
	if fallback.sample_format() != SampleFormat::F32 || fallback.channels() != want_channels {
		return Err(format!(
			"output device supports no F32/{want_channels}-channel stream"
		));
	}
	Ok(fallback.config())
}
