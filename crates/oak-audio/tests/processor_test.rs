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

//! AudioProcessor contract tests (processor.rs), calling the public Rust
//! API. The conversion runs a real FFmpeg filter graph
//! (aresample/aformat/atempo), so resampling and time-stretch have filter
//! latency: the exact frame counts are drained after `flush`, while
//! identity conversion is an immediate passthrough.

use oak_audio::error::{Error, OAKAUDIO_E_INVALID, OAKAUDIO_E_STATE, OAKAUDIO_OK};
use oak_audio::params::{AudioParams, SampleFormat};
use oak_audio::processor::Processor;

/// Stereo f32_p planes of `frames` ramp samples.
fn ramp_planes(frames: usize) -> Vec<Vec<f32>> {
	vec![
		(0..frames).map(|i| i as f32 * 0.01).collect(),
		(0..frames).map(|i| -(i as f32) * 0.01).collect(),
	]
}

/// Stereo planar-f32 params at `rate`.
fn params(rate: i32) -> AudioParams {
	AudioParams {
		sample_rate: rate,
		channel_layout: 0x3,
		format: SampleFormat::F32Planar,
	}
}

fn open_identity(p: &Processor) -> Result<(), Box<dyn std::error::Error>> {
	p.open(params(48000), params(48000), 1.0)
}

/// Pointer arrays for the convert call.
fn plane_ptrs(planes: &[Vec<f32>]) -> Vec<*const f32> {
	planes.iter().map(|p| p.as_ptr()).collect()
}

fn plane_mut_ptrs(planes: &mut [Vec<f32>]) -> Vec<*mut f32> {
	planes.iter_mut().map(|p| p.as_mut_ptr()).collect()
}

/// The crate error code of a failed call (the API surfaces
/// `Box<dyn std::error::Error>`).
fn code(err: Box<dyn std::error::Error>) -> i32 {
	err.downcast_ref::<oak_audio::error::Error>()
		.map(|e| e.code())
		.unwrap_or(-1)
}

/// init yields a closed processor; is_open is false before open and true
/// after; close returns it to closed without error.
#[test]
fn processor_open_isopen_close() {
	let p = Processor::init();
	assert_eq!(p.is_open().unwrap(), false);
	open_identity(&p).unwrap();
	assert_eq!(p.is_open().unwrap(), true);
	p.close().unwrap();
	assert_eq!(p.is_open().unwrap(), false);
}

/// open with matching in/out rate and format is an identity passthrough:
/// convert returns the same frame count and samples within 1e-6.
#[test]
fn identity_convert_passthrough() {
	let p = Processor::init();
	open_identity(&p).unwrap();

	let planes = ramp_planes(32);
	let in_ptrs = plane_ptrs(&planes);
	let mut out = vec![vec![0f32; 32]; 2];
	let out_ptrs = plane_mut_ptrs(&mut out);

	let n = p.convert(in_ptrs.as_ptr(), 32, out_ptrs.as_ptr(), 32).unwrap();
	assert_eq!(n, 32);
	for ch in 0..2 {
		for i in 0..32 {
			assert!(
				(out[ch][i] - planes[ch][i]).abs() < 1e-6,
				"ch{ch}[{i}]: {} vs {}",
				out[ch][i],
				planes[ch][i]
			);
		}
	}
}

/// convert with an output capacity smaller than the produced frames returns
/// the produced count clamped to capacity and fills up to capacity.
#[test]
fn convert_capacity_truncation() {
	let p = Processor::init();
	open_identity(&p).unwrap();

	let planes = ramp_planes(32);
	let in_ptrs = plane_ptrs(&planes);
	let mut out = vec![vec![9.9f32; 10]; 2];
	let out_ptrs = plane_mut_ptrs(&mut out);

	let n = p.convert(in_ptrs.as_ptr(), 32, out_ptrs.as_ptr(), 10).unwrap();
	assert_eq!(n, 10);
	for ch in 0..2 {
		for i in 0..10 {
			assert_eq!(out[ch][i], planes[ch][i]);
		}
	}

	// The graph has already drained; nothing further to pull.
	let mut out2 = vec![vec![0f32; 32]; 2];
	let out2_ptrs = plane_mut_ptrs(&mut out2);
	let n = p.convert(in_ptrs.as_ptr(), 0, out2_ptrs.as_ptr(), 32).unwrap();
	assert_eq!(n, 0);
}

/// open with a zero/negative rate or a wrong output format is rejected and
/// leaves the processor closed; convert before open is a state error.
#[test]
fn open_invalid_params() {
	let p = Processor::init();
	assert_eq!(
		code(p.open(params(0), params(48000), 1.0).unwrap_err()),
		OAKAUDIO_E_INVALID
	);
	assert_eq!(p.is_open().unwrap(), false);
	// The output format is forced to planar f32 by the processor contract.
	let mut wrong_out = params(48000);
	wrong_out.format = SampleFormat::F32;
	assert_eq!(
		code(p.open(params(48000), wrong_out, 1.0).unwrap_err()),
		OAKAUDIO_E_INVALID
	);
	assert_eq!(p.is_open().unwrap(), false);

	// convert before open is a state error; a non-positive speed is
	// rejected on open.
	assert_eq!(
		code(p.convert(std::ptr::null(), 0, std::ptr::null(), 0).unwrap_err()),
		OAKAUDIO_E_STATE
	);
	assert_eq!(
		code(p.open(params(48000), params(48000), 0.0).unwrap_err()),
		OAKAUDIO_E_INVALID
	);
}

/// Resampling to half rate halves the frame count (44100 -> 22050). The
/// real resampler holds samples back (filter delay), so the frames are
/// drained after `flush`; flush then keeps the processor open.
#[test]
fn resample_and_flush() {
	let p = Processor::init();
	p.open(params(44100), params(22050), 1.0).unwrap();

	// 1 second of input keeps the resampler delay well below the signal.
	let frames = 44100;
	let planes = ramp_planes(frames);
	let in_ptrs = plane_ptrs(&planes);
	let mut out = vec![vec![0f32; frames]; 2];
	let out_ptrs = plane_mut_ptrs(&mut out);

	let mut total = p
		.convert(in_ptrs.as_ptr(), frames as i32, out_ptrs.as_ptr(), frames as i32)
		.unwrap();
	p.flush().unwrap();
	// Drain the resampler delay after end-of-input.
	while total < frames as i32 {
		let n = p.convert(std::ptr::null(), 0, out_ptrs.as_ptr(), frames as i32).unwrap();
		if n == 0 {
			break;
		}
		total += n;
	}
	assert!(
		(total - 22050).abs() <= 2,
		"half-rate output must halve the frame count (got {total})"
	);
	assert_eq!(p.is_open().unwrap(), true);
}

/// A tempo factor != 1.0 time-stretches: tempo 2.0 halves the frame count
/// (drained after `flush`; atempo needs a full analysis window before it
/// produces output) and the processor stays open.
#[test]
fn tempo_stretch() {
	let p = Processor::init();
	p.open(params(48000), params(48000), 2.0).unwrap();

	// 1 second of input: many atempo windows (1024 samples at 48 kHz).
	let frames = 48000;
	let planes = ramp_planes(frames);
	let in_ptrs = plane_ptrs(&planes);
	let mut out = vec![vec![0f32; frames]; 2];
	let out_ptrs = plane_mut_ptrs(&mut out);

	let mut total = p
		.convert(in_ptrs.as_ptr(), frames as i32, out_ptrs.as_ptr(), frames as i32)
		.unwrap();
	p.flush().unwrap();
	while total < frames as i32 {
		let n = p.convert(std::ptr::null(), 0, out_ptrs.as_ptr(), frames as i32).unwrap();
		if n == 0 {
			break;
		}
		total += n;
	}
	assert!(
		(total - 24000).abs() <= 2400,
		"tempo 2.0 must halve the frame count (got {total})"
	);
	assert_eq!(p.is_open().unwrap(), true);
	p.close().unwrap();

	// Re-opening a closed processor works; opening an open one is a state
	// error.
	open_identity(&p).unwrap();
	assert_eq!(code(open_identity(&p).unwrap_err()), OAKAUDIO_E_STATE);
}

/// The error mapping is intact.
#[test]
fn error_codes() {
	assert_eq!(Error::Invalid.code(), OAKAUDIO_E_INVALID);
	assert_eq!(Error::State.code(), OAKAUDIO_E_STATE);
	assert_eq!(Error::Failed("x".to_string()).code(), -60003);
	assert_eq!(oak_audio::error::OAKAUDIO_OK, OAKAUDIO_OK);
}
