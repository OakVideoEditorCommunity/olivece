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

//! Shared helpers for the oakaudio contract suite.
//!
//! The former bridge stubs are gone with the deleted C ABI: every test
//! now calls the crate's public Rust API directly (manager singleton,
//! processor, levelmeter, waveform, synchronizer). Decoding in
//! `waveform::extract` goes through oakcodec's in-process FFmpeg
//! decoder; the processor drives a real FFmpeg filter graph.

// Every integration-test binary in this crate compiles this module on its
// own and uses only the helpers it needs, so items unused in one binary are
// still part of the shared fixture API.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Mutex;

/// Serializes tests that mutate process-wide state (the manager singleton)
/// within one test binary.
pub static MANAGER_LOCK: Mutex<()> = Mutex::new(());

/// Lock the manager singleton for a test. The manager state persists across
/// tests (the `OnceLock` cannot be reset), so a panicked test must not
/// poison the lock for the rest of the binary.
pub fn lock_manager() -> std::sync::MutexGuard<'static, ()> {
	MANAGER_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Build a planar f32 buffer with `channel_count` channels of `frame_count`
/// frames from a single-channel `source` (replicated per channel).
pub fn planar_from(source: &[f32], channel_count: usize) -> Vec<Vec<f32>> {
	(0..channel_count).map(|_| source.to_vec()).collect()
}

/// A silence buffer: every sample is exactly `0.0`.
pub fn silence_planar(channel_count: usize, frame_count: usize) -> Vec<Vec<f32>> {
	vec![vec![0.0; frame_count]; channel_count]
}

// ---- Minimal WAV fixture helpers -------------------------------------------

/// Write a 16-bit PCM WAV file (`fmt` chunk + `data` chunk, standard 44-byte
/// header). Decoded by oakcodec's FFmpeg decoder in `waveform::extract`.
pub fn write_wav(path: &Path, channels: u16, rate: u32, samples: &[i16]) -> std::io::Result<()> {
	let block_align = channels * 2;
	let byte_rate = rate * u32::from(block_align);
	let data_size = (samples.len() * 2) as u32;

	let mut out = Vec::with_capacity(44 + data_size as usize);
	out.extend_from_slice(b"RIFF");
	out.extend_from_slice(&(36 + data_size).to_le_bytes());
	out.extend_from_slice(b"WAVE");
	out.extend_from_slice(b"fmt ");
	out.extend_from_slice(&16u32.to_le_bytes());
	out.extend_from_slice(&1u16.to_le_bytes()); // PCM
	out.extend_from_slice(&channels.to_le_bytes());
	out.extend_from_slice(&rate.to_le_bytes());
	out.extend_from_slice(&byte_rate.to_le_bytes());
	out.extend_from_slice(&block_align.to_le_bytes());
	out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
	out.extend_from_slice(b"data");
	out.extend_from_slice(&data_size.to_le_bytes());
	for s in samples {
		out.extend_from_slice(&s.to_le_bytes());
	}
	std::fs::write(path, out)
}

/// Write a WAV header only (no `data` payload) with arbitrary channel and
/// rate claims — used to exercise extraction validation (e.g. the channel
/// cap) without decoding real audio.
pub fn write_wav_header_only(path: &Path, channels: u16, rate: u32) -> std::io::Result<()> {
	let block_align = channels * 2;
	let byte_rate = rate * u32::from(block_align);

	let mut out = Vec::with_capacity(44);
	out.extend_from_slice(b"RIFF");
	out.extend_from_slice(&36u32.to_le_bytes());
	out.extend_from_slice(b"WAVE");
	out.extend_from_slice(b"fmt ");
	out.extend_from_slice(&16u32.to_le_bytes());
	out.extend_from_slice(&1u16.to_le_bytes());
	out.extend_from_slice(&channels.to_le_bytes());
	out.extend_from_slice(&rate.to_le_bytes());
	out.extend_from_slice(&byte_rate.to_le_bytes());
	out.extend_from_slice(&block_align.to_le_bytes());
	out.extend_from_slice(&16u16.to_le_bytes());
	out.extend_from_slice(b"data");
	out.extend_from_slice(&0u32.to_le_bytes());
	std::fs::write(path, out)
}
