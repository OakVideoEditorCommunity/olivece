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

//! `olive::ConformManager` — pcm waveform cache files for fast scrubbing.
//!
//! Mirrors `src/codec/src/conformmanager.h`. Stateless (NOTES.md): actual
//! conform work is delegated to the global task submit callback
//! ([`crate::task`]); with no registrar the state queries report
//! `Unavailable`. Deterministic per-channel filenames derive from the
//! source + target audio params.

use oak_core::filefunctions::FileFunctions;
use std::path::Path;

/// Conform state of one audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ConformState {
	/// Conform files exist.
	Exists = 0,
	/// Conform is being generated.
	Generating = 1,
	/// No task registrar; conform unavailable.
	Unavailable = 2,
}

/// `olive::ConformManager` — stateless conform query/produce manager.
pub struct ConformManager;

impl ConformManager {
	/// The process-wide ConformManager singleton.
	pub fn instance() -> &'static ConformManager {
		static INSTANCE: ConformManager = ConformManager;
		&INSTANCE
	}

	/// Query (and when possible start) the conform of one audio stream.
	///
	/// `wait != 0` treats a post-submit miss as `Unavailable`; `wait == 0`
	/// reports it as `Generating`. Without a task registrar the result is
	/// always `Unavailable`.
	pub fn get_conform_state(
		&self,
		cache_path: &str,
		source_filename: &str,
		stream_index: i32,
		sample_rate: i32,
		channel_layout: u64,
		sample_format: i32,
		wait: bool,
	) -> crate::error::Result<ConformState> {
		let filenames = conform_filenames(
			cache_path,
			source_filename,
			stream_index,
			sample_rate,
			sample_format,
			channel_layout,
		);

		// Return existing conform if it exists.
		if all_conforms_exist(&filenames) {
			return Ok(ConformState::Exists);
		}

		// Interim state (pre-M8): no task system, conform cannot be generated.
		if !crate::task::task_submit_is_registered() {
			return Ok(ConformState::Unavailable);
		}

		// The task owns the ".working" temporary names and the rename to the
		// final per-channel filenames on success; output_filename carries the
		// first channel's final path and the task derives the siblings.
		let req = crate::task::TaskRequest {
			kind: crate::task::TaskKind::Conform,
			input_filename: source_filename,
			output_filename: filenames.first().map(String::as_str).unwrap_or(""),
			stream_index,
			sample_rate,
			channel_layout,
			sample_format,
			proxy_width: 0,
			proxy_height: 0,
		};

		// Interim simplification: submission is synchronous — we always wait
		// for the submit to return, regardless of `wait`.
		if crate::task::submit_task(&req).is_err() {
			return Ok(ConformState::Unavailable);
		}

		if all_conforms_exist(&filenames) {
			return Ok(ConformState::Exists);
		}

		if wait {
			// Synchronous wait already happened and the conform still does not
			// exist: report the wait as failed.
			return Ok(ConformState::Unavailable);
		}

		Ok(ConformState::Generating)
	}

	/// Number of conform (pcm) files for the given stream/params — one per
	/// channel; 0 on invalid arguments.
	pub fn get_conform_filename_count(
		&self,
		_cache_path: &str,
		_source_filename: &str,
		_stream_index: i32,
		_sample_rate: i32,
		channel_layout: u64,
		_sample_format: i32,
	) -> usize {
		channel_layout.count_ones() as usize
	}

	/// The `index`-th conform filename.
	pub fn get_conform_filename(
		&self,
		cache_path: &str,
		source_filename: &str,
		stream_index: i32,
		sample_rate: i32,
		channel_layout: u64,
		sample_format: i32,
		index: usize,
	) -> crate::error::Result<String> {
		let filenames = conform_filenames(
			cache_path,
			source_filename,
			stream_index,
			sample_rate,
			sample_format,
			channel_layout,
		);
		filenames
			.get(index)
			.cloned()
			.ok_or(crate::error::Error::NotFound)
	}
}

/// Deterministic conform base name plus per-channel pcm filenames, mirroring
/// `ConformManager::get_conformed_filename`: one file per channel under
/// `cache_path`, named `<identifier>-<stream>.<rate>.<format>.<layout>.<i>.pcm`.
fn conform_filenames(
	cache_path: &str,
	source_filename: &str,
	stream_index: i32,
	sample_rate: i32,
	sample_format: i32,
	channel_layout: u64,
) -> Vec<String> {
	let count = channel_layout.count_ones() as usize;
	let base = format!(
		"{}-{}.{}.{}.{}",
		unique_file_identifier(source_filename),
		stream_index,
		sample_rate,
		sample_format,
		channel_layout,
	);

	let mut out = Vec::with_capacity(count);
	for i in 0..count {
		let p = Path::new(cache_path).join(format!("{}.{}.pcm", base, i));
		out.push(p.to_string_lossy().into_owned());
	}
	out
}

/// `oak_core_filefunctions_get_unique_file_identifier` wrapper.
fn unique_file_identifier(filename: &str) -> String {
	FileFunctions::new()
		.get_unique_file_identifier(filename)
		.unwrap_or_default()
}

/// True when every conform filename already exists on disk.
fn all_conforms_exist(filenames: &[String]) -> bool {
	filenames.iter().all(|f| Path::new(f).exists())
}

/// Shared test support: serializes access to the global task-submit registry
/// (unit tests run in parallel and would otherwise clear each other's
/// registration) and provides a callback that accepts any task.
#[cfg(test)]
pub(crate) mod test_util {
	use crate::error::OAKCODEC_OK;
	use crate::task::OakCodecTaskRequest;

	/// Serializes every test that mutates the task-submit registry.
	pub static REG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	/// A task-submit callback that accepts every request (no-op).
	pub unsafe extern "C" fn accept_cb(
		_req: *const OakCodecTaskRequest,
		_ud: *mut std::ffi::c_void,
	) -> i32 {
		OAKCODEC_OK
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn temp_subdir(name: &str) -> String {
		let dir =
			std::env::temp_dir().join(format!("oakcodec_conform_{}_{}", name, std::process::id()));
		let _ = std::fs::create_dir_all(&dir);
		dir.to_string_lossy().into_owned()
	}

	#[test]
	fn unique_identifier_uses_file_metadata() {
		// The identifier is the 16-digit FNV-1a hex of (absolute path +
		// mtime); a missing file has no identifier.
		assert_eq!(unique_file_identifier("no-such-file.mp4"), "");
		let dir = temp_subdir("id");
		let file = std::path::Path::new(&dir).join("media.mp4");
		std::fs::write(&file, b"x").unwrap();
		let id = unique_file_identifier(&file.to_string_lossy());
		assert_eq!(id.len(), 16);
		assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
		// Deterministic: same input, same id.
		assert_eq!(id, unique_file_identifier(&file.to_string_lossy()));
		let other = std::path::Path::new(&dir).join("other.mp4");
		std::fs::write(&other, b"y").unwrap();
		// Different input, different id.
		assert_ne!(id, unique_file_identifier(&other.to_string_lossy()));
	}

	#[test]
	fn filename_count_from_channel_layout() {
		let m = ConformManager::instance();
		assert_eq!(m.get_conform_filename_count("c", "s", 0, 48000, 0x3, 0), 2); // stereo
		assert_eq!(m.get_conform_filename_count("c", "s", 0, 48000, 0x4, 0), 1); // mono
		assert_eq!(m.get_conform_filename_count("c", "s", 0, 48000, 0, 0), 0); // invalid
		assert_eq!(
			m.get_conform_filename_count("c", "s", 0, 48000, 0x60F, 0),
			6
		); // 5.1
	}

	#[test]
	fn conform_filename_derivation_and_range() {
		let m = ConformManager::instance();
		let src_dir = temp_subdir("names");
		let src = std::path::Path::new(&src_dir).join("media.mp4");
		std::fs::write(&src, b"x").unwrap();
		let cache = temp_subdir("names");
		let id = unique_file_identifier(&src.to_string_lossy());
		let base = format!("{}-0.48000.0.3", id);
		let f0 = m
			.get_conform_filename(&cache, &src.to_string_lossy(), 0, 48000, 0x3, 0, 0)
			.unwrap();
		let f1 = m
			.get_conform_filename(&cache, &src.to_string_lossy(), 0, 48000, 0x3, 0, 1)
			.unwrap();
		assert_eq!(
			f0,
			std::path::Path::new(&cache)
				.join(format!("{base}.0.pcm"))
				.to_string_lossy()
		);
		assert_eq!(
			f1,
			std::path::Path::new(&cache)
				.join(format!("{base}.1.pcm"))
				.to_string_lossy()
		);
		// Out of range.
		assert!(matches!(
			m.get_conform_filename(&cache, &src.to_string_lossy(), 0, 48000, 0x3, 0, 5),
			Err(crate::error::Error::NotFound)
		));
	}

	#[test]
	fn get_conform_state_unavailable_without_registrar() {
		let _g = super::test_util::REG_LOCK.lock().unwrap();
		// Ensure no registrar is left over.
		crate::task::set_task_submit_cb_extern(None, std::ptr::null_mut());
		let cache = temp_subdir("unavail");
		let s = ConformManager::instance()
			.get_conform_state(&cache, "missing.mp4", 0, 48000, 0x3, 0, false)
			.unwrap();
		assert_eq!(s, ConformState::Unavailable);
	}

	#[test]
	fn get_conform_state_exists_when_files_present() {
		let cache = temp_subdir("exists");
		let m = ConformManager::instance();
		for i in 0..2 {
			let f = m
				.get_conform_filename(&cache, "media.mp4", 0, 48000, 0x3, 0, i)
				.unwrap();
			std::fs::write(&f, b"pcm").unwrap();
		}
		let s = m
			.get_conform_state(&cache, "media.mp4", 0, 48000, 0x3, 0, false)
			.unwrap();
		assert_eq!(s, ConformState::Exists);
	}

	#[test]
	fn get_conform_state_generating_when_registered() {
		let _g = super::test_util::REG_LOCK.lock().unwrap();
		crate::task::set_task_submit_cb_extern(
			Some(super::test_util::accept_cb),
			std::ptr::null_mut(),
		);
		let cache = temp_subdir("generating");
		let s = ConformManager::instance()
			.get_conform_state(&cache, "missing.mp4", 0, 48000, 0x3, 0, false)
			.unwrap();
		crate::task::set_task_submit_cb_extern(None, std::ptr::null_mut());
		assert_eq!(s, ConformState::Generating);
	}
}
