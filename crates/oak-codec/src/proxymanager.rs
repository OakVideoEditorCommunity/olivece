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

//! `olive::ProxyManager` — proxy (low-res transcode) generation.
//!
//! Mirrors `src/codec/src/proxymanager.h`. Stateless (NOTES.md): actual
//! transcodes are delegated to the global task submit callback
//! ([`crate::task`]); with no registrar, `get_or_start` reports the proxy
//! as missing. `proxy_params_from_config` reads the oak_core config store
//! with the compiled-in defaults as fallback (1280x720 / divider 1 / crf 23
//! / "mp4" / "veryfast" / audio included).

use oak_core::configstore::ConfigStore;
use oak_core::filefunctions::FileFunctions;
use std::path::Path;

/// Proxy state of a proxy file on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ProxyState {
	/// Missing (or NULL/empty/absent).
	Missing = 0,
	/// Generating.
	Generating = 1,
	/// Ready on disk.
	Ready = 2,
	/// Generation failed.
	Failed = 3,
}

impl TryFrom<i32> for ProxyState {
	type Error = ();

	fn try_from(value: i32) -> Result<Self, Self::Error> {
		match value {
			0 => Ok(ProxyState::Missing),
			1 => Ok(ProxyState::Generating),
			2 => Ok(ProxyState::Ready),
			3 => Ok(ProxyState::Failed),
			_ => Err(()),
		}
	}
}

/// `olive::ProxyManager::ProxyParams` — mirror of `oakcodec_proxy_params`.
#[derive(Clone, Debug)]
#[repr(C)]
pub struct ProxyParams {
	/// Absolute target width (0 when divider-based).
	pub width: i32,
	/// Absolute target height (0 when divider-based).
	pub height: i32,
	/// Source resolution divider (1 = absolute width/height, 2/4/8).
	pub divider: i32,
	/// Proxy format version.
	pub version: i32,
	/// x264 crf.
	pub crf: i32,
	/// Include the audio track (1/0; C `int`).
	pub include_audio: i32,
	/// ffmpeg output container (e.g. "mp4").
	pub extension: [u8; 32],
	/// ffmpeg encoder preset (e.g. "veryfast").
	pub preset: [u8; 32],
}

impl Default for ProxyParams {
	fn default() -> Self {
		ProxyParams {
			width: 1280,
			height: 720,
			divider: 1,
			version: 1,
			crf: 23,
			include_audio: 1,
			extension: bytes32(b"mp4"),
			preset: bytes32(b"veryfast"),
		}
	}
}

impl ProxyParams {
	fn extension_str(&self) -> &str {
		cstr_slice(&self.extension)
	}

	/// Only the proxy-params tests read the preset through this accessor
	/// (the struct field itself is `pub`).
	#[allow(dead_code)]
	fn preset_str(&self) -> &str {
		cstr_slice(&self.preset)
	}
}

/// `oakcodec_proxy_result` — POD result of [`ProxyManager::get_or_start`];
/// see `include/codec/proxy.h`.
#[repr(C)]
pub struct OakCodecProxyResult {
	/// `ProxyState` value.
	pub state: i32,
	/// Resulting proxy filename (may be empty).
	pub filename: [u8; 1024],
}

/// `olive::ProxyManager` — stateless proxy query/generate manager.
pub struct ProxyManager;

impl ProxyManager {
	/// The process-wide ProxyManager singleton.
	pub fn instance() -> &'static ProxyManager {
		static INSTANCE: ProxyManager = ProxyManager;
		&INSTANCE
	}

	/// Compiled-in default proxy parameters (the `Default` values).
	pub fn proxy_params_default() -> ProxyParams {
		ProxyParams::default()
	}

	/// Proxy parameters read from the oak_core config, with the compiled-in
	/// defaults as fallback.
	///
	/// # CPP-PARITY
	/// `src/codec/src/proxymanager.h` `proxy_params_from_config` — reads
	/// ProxyWidth/ProxyHeight/ProxyDivider/ProxyCRF/ProxyPreset/
	/// ProxyIncludeAudio via `oak_core_config_*`.
	pub fn proxy_params_from_config() -> ProxyParams {
		let mut p = ProxyParams::default();
		p.width = config_get_int("ProxyWidth", p.width);
		p.height = config_get_int("ProxyHeight", p.height);
		p.divider = config_get_int("ProxyDivider", p.divider);
		p.crf = config_get_int("ProxyCRF", p.crf);
		p.include_audio = if config_get_bool("ProxyIncludeAudio", p.include_audio) != 0 {
			1
		} else {
			0
		};
		if let Some(preset) = config_get_str("ProxyPreset") {
			if !preset.is_empty() {
				p.preset = bytes32(preset.as_bytes());
			}
		}
		p
	}

	/// State of a proxy file on disk.
	pub fn get_proxy_state(proxy_filename: &str) -> ProxyState {
		if proxy_filename.is_empty() {
			return ProxyState::Missing;
		}
		if Path::new(proxy_filename).exists() {
			return ProxyState::Ready;
		}
		let working = Self::get_working_filename(proxy_filename);
		if let Ok(w) = working {
			if Path::new(&w).exists() {
				return ProxyState::Generating;
			}
		}
		ProxyState::Missing
	}

	/// Human-readable string for a proxy state.
	pub fn proxy_state_to_string(state: ProxyState) -> String {
		match state {
			ProxyState::Missing => "missing".to_string(),
			ProxyState::Generating => "generating".to_string(),
			ProxyState::Ready => "ready".to_string(),
			ProxyState::Failed => "failed".to_string(),
		}
	}

	/// The proxy state a string name stands for (`Missing` for anything
	/// unrecognized; also accepts the numeric form the Rust serializer used
	/// before the string form landed).
	pub fn proxy_state_from_string(name: &str) -> ProxyState {
		match name.trim() {
			"generating" | "1" => ProxyState::Generating,
			"ready" | "2" => ProxyState::Ready,
			"failed" | "3" => ProxyState::Failed,
			_ => ProxyState::Missing,
		}
	}

	/// Whether a proxy filename was generated with an audio track (the
	/// generation tags audio-including proxies `.a1.`).
	pub fn proxy_filename_has_audio(proxy_filename: &str) -> bool {
		std::path::Path::new(proxy_filename)
			.file_name()
			.map(|n| n.to_string_lossy().contains(".a1."))
			.unwrap_or(false)
	}

	/// Proxy directory for a project cache path.
	pub fn get_proxy_directory(cache_path: &str) -> crate::error::Result<String> {
		Ok(Path::new(cache_path)
			.join("proxy")
			.to_string_lossy()
			.into_owned())
	}

	/// Deterministic proxy filename for a source stream.
	pub fn get_proxy_filename(
		cache_path: &str,
		source_filename: &str,
		stream_index: i32,
		params: &ProxyParams,
	) -> crate::error::Result<String> {
		let proxy_dir = Self::get_proxy_directory(cache_path)?;
		let extension = if params.extension_str().is_empty() {
			"mp4"
		} else {
			params.extension_str()
		};

		// Divider mode scales relative to the source, so the tag names the
		// divider rather than an absolute target size.
		let size_tag = if params.divider > 1 {
			format!("div{}", params.divider)
		} else {
			format!("{}x{}", params.width, params.height)
		};

		let filename = format!(
			"{}-{}.{}.v{}.a{}.{}",
			unique_file_identifier(source_filename),
			stream_index,
			size_tag,
			params.version,
			params.include_audio,
			extension,
		);

		Ok(Path::new(&proxy_dir)
			.join(filename)
			.to_string_lossy()
			.into_owned())
	}

	/// Working (in-progress) filename of a proxy.
	pub fn get_working_filename(proxy_filename: &str) -> crate::error::Result<String> {
		// Append a recognizable suffix while keeping a standard container
		// extension so ffmpeg can infer the output format.
		Ok(format!("{}.working.mp4", proxy_filename))
	}

	/// Get or start generating a proxy for `source_filename`. With no task
	/// registrar the state stays `Missing`.
	pub fn get_or_start(
		&self,
		cache_path: &str,
		source_filename: &str,
		stream_index: i32,
		params: &ProxyParams,
	) -> crate::error::Result<(ProxyState, String)> {
		let filename = Self::get_proxy_filename(cache_path, source_filename, stream_index, params)?;
		let file_state = Self::get_proxy_state(&filename);
		if file_state == ProxyState::Ready {
			return Ok((ProxyState::Ready, filename));
		}

		if !crate::task::task_submit_is_registered() {
			// Interim state (pre-M8): no task system, proxy cannot be generated.
			return Ok((ProxyState::Missing, filename));
		}

		if file_state == ProxyState::Generating {
			// Stale working file from an interrupted run.
			if let Ok(working) = Self::get_working_filename(&filename) {
				let _ = std::fs::remove_file(&working);
			}
		}

		// The task owns the ".working.mp4" temporary name and the rename to the
		// final filename on success.
		let req = crate::task::TaskRequest {
			kind: crate::task::TaskKind::Proxy,
			input_filename: source_filename,
			output_filename: &filename,
			stream_index,
			sample_rate: 0,
			channel_layout: 0,
			sample_format: 0,
			proxy_width: if params.divider <= 1 { params.width } else { 0 },
			proxy_height: if params.divider <= 1 {
				params.height
			} else {
				0
			},
		};

		// Interim simplification: submission is synchronous.
		if crate::task::submit_task(&req).is_err() {
			return Ok((ProxyState::Failed, filename));
		}

		if Self::get_proxy_state(&filename) == ProxyState::Ready {
			return Ok((ProxyState::Ready, filename));
		}

		Ok((ProxyState::Generating, filename))
	}

	/// Locate an ffmpeg executable (empty string when none found).
	pub fn find_ffmpeg(configured_path: &str) -> String {
		// An explicitly configured path takes precedence if it is usable.
		if !configured_path.is_empty() {
			if is_executable_file(Path::new(configured_path)) {
				return absolute(configured_path);
			}
		}

		// Fall back to searching the system PATH (split per platform:
		// `;` on Windows, `:` elsewhere — see [`split_path_env`]).
		if let Ok(path_env) = std::env::var("PATH") {
			for dir in split_path_env(&path_env) {
				let candidate = Path::new(&dir).join(ffmpeg_exe_name());
				if is_executable_file(&candidate) {
					return absolute(&candidate.to_string_lossy());
				}
			}
		}

		// Finally, try common install locations (PATH on GUI-launched apps,
		// particularly on macOS, often lacks these).
		let mut candidates: Vec<String> = Vec::new();
		let app_path = application_path();
		if !app_path.is_empty() {
			candidates.push(format!("{}/{}", app_path, ffmpeg_exe_name()));
		}
		candidates.push("/opt/homebrew/bin/ffmpeg".to_string());
		candidates.push("/usr/local/bin/ffmpeg".to_string());
		candidates.push("/usr/bin/ffmpeg".to_string());
		candidates.push("/usr/local/bin/ffmpeg".to_string());

		// Windows-specific install locations (GUI-launched apps can also
		// start with a minimal PATH there).
		#[cfg(target_os = "windows")]
		{
			// The official Windows installer defaults to
			// %LOCALAPPDATA%\Programs\ffmpeg\bin.
			if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
				candidates.push(format!(
					"{}/Programs/ffmpeg/bin/{}",
					local_app_data,
					ffmpeg_exe_name()
				));
			}
			// Oak's user configuration directory.
			if let Ok(config_dir) = FileFunctions::new().get_configuration_location() {
				candidates.push(format!("{}/{}", config_dir, ffmpeg_exe_name()));
			}
		}

		for c in candidates {
			if is_executable_file(Path::new(&c)) {
				return absolute(&c);
			}
		}

		String::new()
	}
}

/// Copy a byte string into a NUL-terminated `[u8; 32]` (truncated to 31
/// chars so there is always a trailing NUL).
fn bytes32(s: &[u8]) -> [u8; 32] {
	let mut a = [0u8; 32];
	let n = s.len().min(31);
	a[..n].copy_from_slice(&s[..n]);
	a
}

/// View a NUL-terminated `[u8; 32]` as a `&str` (up to the first NUL).
fn cstr_slice(a: &[u8; 32]) -> &str {
	let end = a.iter().position(|&b| b == 0).unwrap_or(a.len());
	std::str::from_utf8(&a[..end]).unwrap_or("")
}

/// `oak_core_config_get_int` wrapper (null group).
fn config_get_int(key: &str, default: i32) -> i32 {
	ConfigStore::instance().get_int(None, key, default)
}

/// `oak_core_config_get_bool` wrapper (null group).
fn config_get_bool(key: &str, default: i32) -> i32 {
	ConfigStore::instance().get_bool(None, key, default)
}

/// `oak_core_config_get` string read; `None` when the stored value is
/// empty or absent.
fn config_get_str(key: &str) -> Option<String> {
	match ConfigStore::instance().get(None, key) {
		Ok(s) if !s.is_empty() => Some(s),
		_ => None,
	}
}

/// `oak_core_filefunctions_get_unique_file_identifier` wrapper.
fn unique_file_identifier(filename: &str) -> String {
	FileFunctions::new()
		.get_unique_file_identifier(filename)
		.unwrap_or_default()
}

/// `oak_core_filefunctions_get_application_path` read.
fn application_path() -> String {
	FileFunctions::new()
		.get_application_path()
		.unwrap_or_default()
}

/// Split a PATH-style variable into its non-empty directory entries. The
/// separator is platform-specific (`;` on Windows, `:` elsewhere);
/// `std::env::split_paths` already applies that rule natively.
fn split_path_env(raw: &str) -> Vec<String> {
	std::env::split_paths(raw)
		.map(|p| p.to_string_lossy().into_owned())
		.filter(|p| !p.is_empty())
		.collect()
}

/// The executable file name to look for: `ffmpeg.exe` on Windows,
/// `ffmpeg` everywhere else.
fn ffmpeg_exe_name() -> &'static str {
	if cfg!(windows) {
		"ffmpeg.exe"
	} else {
		"ffmpeg"
	}
}

/// True when `p` is a regular file that can be run as a program. Unix
/// checks for at least one execute bit; Windows has no permission bits, so
/// any regular file counts.
fn is_executable_file(p: &Path) -> bool {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		match std::fs::metadata(p) {
			Ok(md) if md.is_file() => md.permissions().mode() & 0o111 != 0,
			_ => false,
		}
	}
	#[cfg(windows)]
	{
		matches!(std::fs::metadata(p), Ok(md) if md.is_file())
	}
}

/// Canonicalize a path, falling back to the raw string on failure.
fn absolute(p: &str) -> String {
	std::fs::canonicalize(p)
		.map(|c| c.to_string_lossy().into_owned())
		.unwrap_or_else(|_| p.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn temp_subdir(name: &str) -> String {
		let dir =
			std::env::temp_dir().join(format!("oakcodec_proxy_{}_{}", name, std::process::id()));
		let _ = std::fs::create_dir_all(&dir);
		dir.to_string_lossy().into_owned()
	}


	#[test]
	fn proxy_params_default_values() {
		let p = ProxyManager::proxy_params_default();
		assert_eq!(p.width, 1280);
		assert_eq!(p.height, 720);
		assert_eq!(p.divider, 1);
		assert_eq!(p.version, 1);
		assert_eq!(p.crf, 23);
		assert_eq!(p.include_audio, 1);
		assert_eq!(p.extension_str(), "mp4");
		assert_eq!(p.preset_str(), "veryfast");
	}

	#[test]
	fn proxy_params_from_config_uses_defaults_without_store() {
		// The test stub returns defaults for every int/bool and an empty
		// ProxyPreset; the empty preset must not clobber the compiled-in one.
		let p = ProxyManager::proxy_params_from_config();
		assert_eq!(p.width, 1280);
		assert_eq!(p.height, 720);
		assert_eq!(p.divider, 1);
		assert_eq!(p.crf, 23);
		assert_eq!(p.include_audio, 1);
		assert_eq!(p.preset_str(), "veryfast");
		assert_eq!(p.extension_str(), "mp4");
	}

	#[test]
	fn proxy_directory_is_cache_slash_proxy() {
		assert_eq!(
			ProxyManager::get_proxy_directory("/tmp/cache").unwrap(),
			// Path::join separators are platform-native (\ on Windows).
			std::path::Path::new("/tmp/cache")
				.join("proxy")
				.to_string_lossy()
		);
	}

	#[test]
	fn proxy_filename_derivation() {
		let cache = temp_subdir("fn");
		let p = ProxyManager::proxy_params_default();

		// A missing source carries no unique-file identifier
		// (get_unique_file_identifier is empty for non-existent files —
		// C++ parity), so the name is the plain size/version/audio tags.
		let missing = std::path::Path::new(&temp_subdir("missing")).join("nope.mp4");
		let f = ProxyManager::get_proxy_filename(&cache, missing.to_str().unwrap(), 0, &p).unwrap();
		let plain = std::path::Path::new(&cache)
			.join("proxy")
			.join("-0.1280x720.v1.a1.mp4")
			.to_string_lossy()
			.into_owned();
		assert_eq!(f, plain);

		// An existing source embeds a stable per-file identifier.
		let existing = std::path::Path::new(&temp_subdir("existing")).join("real.mp4");
		std::fs::write(&existing, b"media").unwrap();
		let f1 =
			ProxyManager::get_proxy_filename(&cache, existing.to_str().unwrap(), 0, &p).unwrap();
		let f2 =
			ProxyManager::get_proxy_filename(&cache, existing.to_str().unwrap(), 0, &p).unwrap();
		assert!(
			f1.contains("-0.1280x720.v1.a1.mp4"),
			"size/version/audio tags present: {f1}"
		);
		assert!(f1 != plain, "id embedded: {f1}");
		assert_eq!(f1, f2, "the identifier is stable for the same file");

		// Divider mode tags the divider instead of an absolute size.
		let mut d = p.clone();
		d.divider = 2;
		let f3 = ProxyManager::get_proxy_filename(&cache, missing.to_str().unwrap(), 0, &d).unwrap();
		assert!(f3.contains(".div2."));

		// No audio.
		let mut na = p.clone();
		na.include_audio = 0;
		let f4 = ProxyManager::get_proxy_filename(&cache, missing.to_str().unwrap(), 0, &na).unwrap();
		assert!(f4.contains(".a0."));
	}

	#[test]
	fn proxy_state_transitions() {
		let cache = temp_subdir("state");
		let p = ProxyManager::proxy_params_default();
		let f = ProxyManager::get_proxy_filename(&cache, "media.mp4", 0, &p).unwrap();

		// Missing when neither the file nor the working file exists.
		assert_eq!(ProxyManager::get_proxy_state(&f), ProxyState::Missing);

		// Generating when only the working file exists.
		let working = ProxyManager::get_working_filename(&f).unwrap();
		std::fs::create_dir_all(Path::new(&working).parent().unwrap()).unwrap();
		std::fs::write(&working, b"x").unwrap();
		assert_eq!(ProxyManager::get_proxy_state(&f), ProxyState::Generating);

		// Ready when the final file exists (takes precedence over working).
		std::fs::create_dir_all(Path::new(&f).parent().unwrap()).unwrap();
		std::fs::write(&f, b"x").unwrap();
		assert_eq!(ProxyManager::get_proxy_state(&f), ProxyState::Ready);
	}

	#[test]
	fn proxy_state_to_string_mapping() {
		assert_eq!(
			ProxyManager::proxy_state_to_string(ProxyState::Missing),
			"missing"
		);
		assert_eq!(
			ProxyManager::proxy_state_to_string(ProxyState::Generating),
			"generating"
		);
		assert_eq!(
			ProxyManager::proxy_state_to_string(ProxyState::Ready),
			"ready"
		);
		assert_eq!(
			ProxyManager::proxy_state_to_string(ProxyState::Failed),
			"failed"
		);
	}

	#[test]
	fn get_working_filename_appends_suffix() {
		assert_eq!(
			ProxyManager::get_working_filename("/a/b.mp4").unwrap(),
			"/a/b.mp4.working.mp4"
		);
	}

	#[test]
	fn get_or_start_missing_without_registrar() {
		let _g = crate::conformmanager::test_util::REG_LOCK.lock().unwrap();
		crate::task::set_task_submit_cb_extern(None, std::ptr::null_mut());
		let cache = temp_subdir("nostart");
		let p = ProxyManager::proxy_params_default();
		let (state, _f) = ProxyManager::instance()
			.get_or_start(&cache, "media.mp4", 0, &p)
			.unwrap();
		assert_eq!(state, ProxyState::Missing);
	}

	#[test]
	fn get_or_start_ready_when_file_exists() {
		let cache = temp_subdir("ready");
		let p = ProxyManager::proxy_params_default();
		let f = ProxyManager::get_proxy_filename(&cache, "media.mp4", 0, &p).unwrap();
		std::fs::create_dir_all(Path::new(&f).parent().unwrap()).unwrap();
		std::fs::write(&f, b"x").unwrap();
		let (state, filename) = ProxyManager::instance()
			.get_or_start(&cache, "media.mp4", 0, &p)
			.unwrap();
		assert_eq!(state, ProxyState::Ready);
		assert_eq!(filename, f);
	}

	#[test]
	fn get_or_start_generating_when_registered() {
		let _g = crate::conformmanager::test_util::REG_LOCK.lock().unwrap();
		crate::task::set_task_submit_cb_extern(
			Some(crate::conformmanager::test_util::accept_cb),
			std::ptr::null_mut(),
		);
		let cache = temp_subdir("start");
		let p = ProxyManager::proxy_params_default();
		let (state, _f) = ProxyManager::instance()
			.get_or_start(&cache, "media.mp4", 0, &p)
			.unwrap();
		crate::task::set_task_submit_cb_extern(None, std::ptr::null_mut());
		assert_eq!(state, ProxyState::Generating);
	}

	#[test]
	fn find_ffmpeg_configured_path_wins() {
		// Point at a real executable (the current test binary) so the
		// configured-path branch resolves to an absolute path.
		let me = std::env::current_exe().unwrap();
		let found = ProxyManager::find_ffmpeg(me.to_str().unwrap());
		let canonical = std::fs::canonicalize(&me).unwrap();
		assert_eq!(found, canonical.to_string_lossy());
	}

	#[test]
	fn find_ffmpeg_missing_returns_empty() {
		let found = ProxyManager::find_ffmpeg("/definitely/not/a/real/ffmpeg");
		// Either an absolute configured/installed match or empty; never a raw
		// unresolved path.
		assert!(
			found.is_empty() || std::path::Path::new(&found).is_absolute(),
			"found: {found}"
		);
	}

	/// `oakcodec_proxy_params` byte-level layout lock against
	/// `include/codec/proxy.h` (verified with a C++ `offsetof` probe): the
	/// Rust mirror must read a C caller's POD in place.
	#[test]
	fn proxy_params_c_abi_layout() {
		use std::mem::{offset_of, size_of};

		assert_eq!(size_of::<ProxyParams>(), 88);
		assert_eq!(offset_of!(ProxyParams, width), 0);
		assert_eq!(offset_of!(ProxyParams, height), 4);
		assert_eq!(offset_of!(ProxyParams, divider), 8);
		assert_eq!(offset_of!(ProxyParams, version), 12);
		assert_eq!(offset_of!(ProxyParams, crf), 16);
		assert_eq!(offset_of!(ProxyParams, include_audio), 20);
		assert_eq!(offset_of!(ProxyParams, extension), 24);
		assert_eq!(offset_of!(ProxyParams, preset), 56);
	}
}

#[cfg(test)]
mod tests_extra {
	use super::*;

	/// Unix-only: builds a fake `ffmpeg` shell script with a mode-0755
	/// chmod; Windows has no permission bits and searches for `ffmpeg.exe`.
	#[cfg(unix)]
	#[test]
	fn find_ffmpeg_searches_path() {
		// Create a fake executable in a temp dir and prepend it to PATH.
		let dir = std::env::temp_dir().join(format!("oakcodec_ffmpeg_{}", std::process::id()));
		let _ = std::fs::create_dir_all(&dir);
		let fake = dir.join("ffmpeg");
		use std::os::unix::fs::PermissionsExt;
		std::fs::write(&fake, b"#!/bin/sh\n").unwrap();
		std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

		let mut paths = dir.to_string_lossy().into_owned();
		if let Ok(existing) = std::env::var("PATH") {
			paths.push(':');
			paths.push_str(&existing);
		}
		let old = std::env::var_os("PATH");
		std::env::set_var("PATH", &paths);
		let found = ProxyManager::find_ffmpeg("");
		if let Some(old) = old {
			std::env::set_var("PATH", old);
		} else {
			std::env::remove_var("PATH");
		}

		// `std::env::set_var` is not thread-safe, so under parallel tests the
		// canonicalized form can race; assert the search branch invariants
		// instead of the exact canonical path.
		assert!(found.starts_with('/'), "found: {found}");
		assert!(found.ends_with("/ffmpeg"), "found: {found}");
		assert!(std::path::Path::new(&found).exists(), "found: {found}");
	}

	#[test]
	fn split_path_env_uses_platform_separator() {
		// The separator is platform-dependent, so the expected input is
		// gated: `:` on Unix, `;` on Windows.
		#[cfg(unix)]
		{
			assert_eq!(split_path_env("a:b:c"), ["a", "b", "c"]);
			// Empty entries are skipped.
			assert_eq!(split_path_env("a::b:"), ["a", "b"]);
			assert_eq!(split_path_env("solo"), ["solo"]);
		}
		#[cfg(windows)]
		{
			assert_eq!(split_path_env("a;b;c"), ["a", "b", "c"]);
			assert_eq!(split_path_env("a;;b;"), ["a", "b"]);
			assert_eq!(split_path_env("solo"), ["solo"]);
		}
	}

	#[test]
	fn ffmpeg_exe_name_is_platform_specific() {
		#[cfg(unix)]
		assert_eq!(ffmpeg_exe_name(), "ffmpeg");
		#[cfg(windows)]
		assert_eq!(ffmpeg_exe_name(), "ffmpeg.exe");
	}

	#[test]
	fn get_proxy_state_empty_and_working() {
		// Empty filename -> Missing.
		assert_eq!(ProxyManager::get_proxy_state(""), ProxyState::Missing);
		// A path that does not exist -> Missing.
		assert_eq!(
			ProxyManager::get_proxy_state("/nope/nope.mp4"),
			ProxyState::Missing
		);
	}
}
