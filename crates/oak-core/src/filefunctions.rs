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

//! File/directory helpers, mirroring `src/common/src/filefunctions.h`
//! and `include/common/filefunctions.h`. Mirrors the filefunctions C++
//! namespace as a handle-bearing family for C ABI shape uniformity; the
//! underlying operations are stateless.
//!
//! Two-stage string getters here return their result via `buf`/`buf_size`
//! (see `crate::ffi`); the domain functions below return the owned string.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// FNV-1a 64-bit hash of `data`, returned as lowercase hex (`%016llx`).
///
/// Mirrors the anonymous `fnv1a_hex` helper in `src/common/src/filefunctions.cpp`.
/// `pub(crate)` since `displayicc` reuses it to name ICC cache files.
pub(crate) fn fnv1a_hex(data: &[u8]) -> String {
	let mut hash: u64 = 14695981039346656037;
	for &c in data {
		hash ^= u64::from(c);
		hash = hash.wrapping_mul(1099511628211);
	}
	format!("{:016x}", hash)
}

/// Case-insensitive check whether `s` ends with `suffix`.
///
/// Mirrors `ends_with_case_insensitive` in `filefunctions.cpp`; only `A-Z`
/// are folded to lower case (like the C++ `a >= 'A' && a <= 'Z'` test).
fn ends_with_case_insensitive(s: &str, suffix: &str) -> bool {
	if suffix.len() > s.len() {
		return false;
	}
	let offset = s.len() - suffix.len();
	s.as_bytes()[offset..]
		.iter()
		.zip(suffix.as_bytes().iter())
		.all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

/// The filefunctions family (stateless; the handle only exists for C ABI
/// uniformity).
pub struct FileFunctions;

impl FileFunctions {
	/// Creates the filefunctions family object.
	pub fn new() -> Self {
		Self
	}

	/// Deterministic identifier string for a file (empty if it does not
	/// exist).
	pub fn get_unique_file_identifier(&self, filename: &str) -> Result<String> {
		// `std::path::absolute` is the std analog of `fs::absolute`: it
		// yields an absolute path without resolving symlinks or normalizing.
		let abs = match std::path::absolute(filename) {
			Ok(p) => p,
			Err(_) => return Ok(String::new()),
		};
		if !abs.exists() {
			return Ok(String::new());
		}

		let metadata = match std::fs::metadata(&abs) {
			Ok(m) => m,
			Err(_) => return Ok(String::new()),
		};
		let mtime = match metadata.modified() {
			Ok(t) => t,
			Err(_) => return Ok(String::new()),
		};

		// C++ appends `to_string(mtime.time_since_epoch().count())`; on
		// macOS `file_time_type` has nanosecond precision, so `.as_nanos()`
		// is the closest equivalent (used only as a cache-key salt).
		// CPP-PARITY: `time_since_epoch().count()` on a pre-epoch timestamp
		// is negative; Rust's `duration_since` returns an error which we fold
		// into the negated magnitude.
		let count: i128 = mtime
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos() as i128)
			.unwrap_or_else(|e| -(e.duration().as_nanos() as i128));

		let mut hash_input = abs.to_string_lossy().into_owned();
		hash_input.push_str(&count.to_string());

		Ok(fnv1a_hex(hash_input.as_bytes()))
	}

	/// Configuration directory.
	pub fn get_configuration_location(&self) -> Result<String> {
		// Tests and tooling can redirect the configuration (and, since most
		// locations derive from it, the cache/data) root.
		if let Ok(dir) = std::env::var("OAK_CONFIG_DIR") {
			if !dir.is_empty() {
				let _ = std::fs::create_dir_all(&dir);
				return Ok(dir);
			}
		}

		if Self::is_portable() {
			return Ok(Self::application_path());
		}

		// CPP-PARITY: the C++ `#ifdef __APPLE__` picks `~/Library/Application
		// Support`; the non-Apple branch prefers `$XDG_CONFIG_HOME` then
		// `~/.config`. The empty-root fallback is the temp directory.
		#[cfg(target_os = "macos")]
		let config_root = match std::env::var("HOME") {
			Ok(h) if !h.is_empty() => PathBuf::from(h).join("Library").join("Application Support"),
			_ => PathBuf::new(),
		};
		#[cfg(not(target_os = "macos"))]
		let config_root = match std::env::var("XDG_CONFIG_HOME") {
			Ok(x) if !x.is_empty() => PathBuf::from(x),
			_ => match std::env::var("HOME") {
				Ok(h) if !h.is_empty() => PathBuf::from(h).join(".config"),
				_ => PathBuf::new(),
			},
		};

		let config_root = if config_root.as_os_str().is_empty() {
			std::env::temp_dir()
		} else {
			config_root
		};

		let config_dir = config_root.join("oak");
		let _ = std::fs::create_dir_all(&config_dir);
		Ok(config_dir.to_string_lossy().into_owned())
	}

	/// Application path.
	pub fn get_application_path(&self) -> Result<String> {
		Ok(Self::application_path())
	}

	/// Whether the application is running in portable mode (a `portable` file
	/// sits next to the application executable). Mirrors
	/// `FileFunctions::is_portable`.
	fn is_portable() -> bool {
		let app = Self::application_path();
		!app.is_empty() && Path::new(&app).join("portable").exists()
	}

	/// Application path, without the `Result` wrapper (used internally).
	fn application_path() -> String {
		#[cfg(target_os = "macos")]
		{
			// CPP-PARITY: C++ uses `_NSGetExecutablePath` + `weakly_canonical`;
			// `std::env::current_exe` + `canonicalize` is the portable
			// equivalent and yields the same parent directory.
			if let Ok(exe) = std::env::current_exe() {
				let canonical = exe.canonicalize().unwrap_or(exe);
				if let Some(parent) = canonical.parent() {
					return parent.to_string_lossy().into_owned();
				}
			}
		}
		#[cfg(all(unix, not(target_os = "macos")))]
		{
			if let Ok(target) = std::fs::read_link("/proc/self/exe") {
				if let Some(parent) = target.parent() {
					return parent.to_string_lossy().into_owned();
				}
			}
		}

		// Fallback: current working directory
		std::env::current_dir()
			.map(|c| c.to_string_lossy().into_owned())
			.unwrap_or_default()
	}

	/// Temporary file path.
	pub fn get_temp_file_path(&self) -> Result<String> {
		// CPP-PARITY: `std::env::temp_dir()` never fails the way
		// `fs::temp_directory_path(ec)` can, so the `"." / "oak-temp"`
		// fallback is unreachable here and omitted.
		let temp_path = std::env::temp_dir().join("oak");
		let _ = std::fs::create_dir_all(&temp_path);
		Ok(temp_path.to_string_lossy().into_owned())
	}

	/// Auto-recovery root directory.
	pub fn get_auto_recovery_root(&self) -> Result<String> {
		let config = self.get_configuration_location()?;
		Ok(PathBuf::from(config)
			.join("autorecovery")
			.to_string_lossy()
			.into_owned())
	}

	/// Whether `source` can be copied to `dest` without overwriting.
	pub fn can_copy_directory_without_overwriting(&self, source: &str, dest: &str) -> bool {
		// CPP-PARITY: a failed `directory_iterator` (e.g. `source` missing)
		// yields an empty iteration, so the function returns `true`.
		let Ok(entries) = std::fs::read_dir(source) else {
			return true;
		};
		for entry in entries.flatten() {
			let dest_equivalent = PathBuf::from(dest).join(entry.file_name());
			// `entry.is_directory(ec)` failing (ec set) falls through to the
			// "else exists" branch in C++; `unwrap_or(false)` mirrors that.
			if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
				if !self.can_copy_directory_without_overwriting(
					&entry.path().to_string_lossy(),
					&dest_equivalent.to_string_lossy(),
				) {
					return false;
				}
			} else if dest_equivalent.exists() {
				return false;
			}
		}
		true
	}

	/// Recursively copy a directory, optionally overwriting.
	pub fn copy_directory(&self, source: &str, dest: &str, overwrite: bool) -> Result<()> {
		if !Path::new(source).is_dir() {
			eprintln!("Failed to copy directory, source {} didn't exist", source);
			return Ok(());
		}

		if std::fs::create_dir_all(dest).is_err() {
			eprintln!("Failed to create destination directory {}", dest);
			return Ok(());
		}

		// CPP-PARITY: a failed `directory_iterator` on `source` aborts the
		// copy silently (C++ prints nothing); skipping matches that.
		let Ok(entries) = std::fs::read_dir(source) else {
			return Ok(());
		};
		for entry in entries.flatten() {
			let entry_path = entry.path();
			let dest_file_path = PathBuf::from(dest).join(entry.file_name());

			if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
				// Copy dir
				self.copy_directory(
					&entry_path.to_string_lossy(),
					&dest_file_path.to_string_lossy(),
					overwrite,
				)?;
			} else {
				// Copy file
				if overwrite {
					// C++ adds owner/group/others write permission then
					// removes the destination (so read-only files can be
					// replaced). CPP-PARITY: gated to `unix` because Rust's
					// `PermissionsExt` is Unix-only; on Windows the write
					// permissions already allow removal.
					#[cfg(unix)]
					{
						use std::os::unix::fs::PermissionsExt;
						if let Ok(meta) = std::fs::metadata(&dest_file_path) {
							let mut perms = meta.permissions();
							// owner_write|group_write|others_write
							perms.set_mode(perms.mode() | 0o200 | 0o020 | 0o002);
							let _ = std::fs::set_permissions(&dest_file_path, perms);
						}
					}
					let _ = std::fs::remove_file(&dest_file_path);
				}

				// CPP-PARITY: C++ `fs::copy_file` with `copy_options::none`
				// FAILS when the destination exists, but Rust's
				// `std::fs::copy` would silently overwrite it — so the
				// no-overwrite case must be guarded explicitly.
				if !overwrite && dest_file_path.exists() {
					eprintln!(
						"Failed to copy file {} to {}: File exists",
						entry_path.to_string_lossy(),
						dest_file_path.to_string_lossy()
					);
					continue;
				}

				if let Err(e) = std::fs::copy(&entry_path, &dest_file_path) {
					eprintln!(
						"Failed to copy file {} to {}: {}",
						entry_path.to_string_lossy(),
						dest_file_path.to_string_lossy(),
						e
					);
				}
			}
		}
		Ok(())
	}

	/// Whether a directory exists, optionally creating it.
	pub fn directory_is_valid(&self, dir: &str, try_to_create: bool) -> bool {
		// Return whether the directory exists, or whether it could be created
		// if it doesn't
		if Path::new(dir).is_dir() {
			return true;
		}
		try_to_create && std::fs::create_dir_all(dir).is_ok()
	}

	/// Filename with a (dotless) extension ensured.
	pub fn ensure_filename_extension(&self, filename: &str, extension: &str) -> Result<String> {
		let mut fn_str = filename.to_string();
		// No-op if either input is empty
		if !fn_str.is_empty() && !extension.is_empty() {
			let extension_with_dot = format!(".{}", extension);
			if !ends_with_case_insensitive(&fn_str, &extension_with_dot) {
				fn_str.push_str(&extension_with_dot);
			}
		}
		Ok(fn_str)
	}

	/// The entire file read into a string (empty if it cannot be read).
	pub fn read_file_as_string(&self, filename: &str) -> Result<String> {
		// CPP-PARITY: the C++ returns the raw byte buffer as a `std::string`;
		// Rust `String` must be valid UTF-8, so invalid bytes are replaced
		// (lossy) rather than preserved. Project/XML files are UTF-8, so this
		// matches in practice.
		match std::fs::read(filename) {
			Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
			Err(_) => Ok(String::new()),
		}
	}

	/// A non-existing temporary variant of `original`.
	pub fn get_safe_temporary_filename(&self, original: &str) -> Result<String> {
		let mut counter: i32 = 0;

		let original_path = PathBuf::from(original);
		let dir = original_path
			.parent()
			.map(Path::to_path_buf)
			.unwrap_or_default();
		let filename = original_path
			.file_name()
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_default();

		// Split off the complete suffix (everything from the first dot), like
		// QFileInfo::completeSuffix()
		let mut basename = filename.clone();
		let mut complete_suffix = String::new();
		if let Some(first_dot) = filename.find('.') {
			basename = filename[..first_dot].to_string();
			complete_suffix = filename[first_dot..].to_string();
		}

		let mut temp_abs_path;
		loop {
			temp_abs_path = dir.join(format!("{}.tmp{}{}", basename, counter, complete_suffix));
			counter += 1;
			if !temp_abs_path.exists() {
				break;
			}
		}

		Ok(temp_abs_path.to_string_lossy().into_owned())
	}

	/// Rename `from` to `to`, deleting `to` first if it exists.
	pub fn rename_file_allow_overwrite(&self, from: &str, to: &str) -> bool {
		if Path::new(to).exists() {
			// CPP-PARITY: C++ `fs::remove` handles both files and dirs; Rust
			// has no single "remove" that does both, and the use case here is
			// file renames, so `remove_file` is used.
			if std::fs::remove_file(to).is_err() {
				eprintln!("Couldn't remove existing file {} for overwrite", to);
				return false;
			}
		}

		// By this point, we can assume `to` either never existed or has now
		// been deleted
		if std::fs::rename(from, to).is_err() {
			eprintln!("Failed to rename file {} to {}", from, to);
			return false;
		}

		true
	}

	/// Append the platform executable suffix (".exe" on Windows).
	pub fn get_formatted_executable_for_platform(&self, unformatted: &str) -> Result<String> {
		#[cfg(target_os = "windows")]
		{
			Ok(format!("{}.exe", unformatted))
		}
		#[cfg(not(target_os = "windows"))]
		{
			Ok(unformatted.to_string())
		}
	}
}

/// Location query helper reserved for the two-stage C getters; returns a
/// path suitable for `std::fs`.
pub(crate) fn config_location_path() -> Result<PathBuf> {
	FileFunctions::new()
		.get_configuration_location()
		.map(PathBuf::from)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, MutexGuard};

	// The configuration-location tests mutate the process-wide `OAK_CONFIG_DIR`
	// env var, which is also read by `get_auto_recovery_root` and
	// `config_location_path`. Rust runs tests in parallel, so serialize those
	// env-dependent tests with a shared lock. We use the crate-wide test lock so
	// `configstore` tests (which also mutate `OAK_CONFIG_DIR`) serialize on the
	// SAME mutex.
	fn config_lock() -> &'static Mutex<()> {
		crate::test_support::env_lock()
	}

	fn unique_temp_dir(tag: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!(
			"oak-filefunctions-test-{}-{}-{}",
			tag,
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	#[test]
	fn fnv1a_known_vectors() {
		// Offset basis as lowercase hex, and the canonical FNV-1a-64 of "a".
		assert_eq!(fnv1a_hex(b""), "cbf29ce484222325");
		assert_eq!(fnv1a_hex(b"a"), "af63dc4c8601ec8c");
	}

	#[test]
	fn ensure_extension_appends_and_is_case_insensitive() {
		let f = FileFunctions::new();
		assert_eq!(
			f.ensure_filename_extension("foo", "ove").unwrap(),
			"foo.ove"
		);
		// Already present (case-insensitive): untouched.
		assert_eq!(
			f.ensure_filename_extension("foo.OVE", "ove").unwrap(),
			"foo.OVE"
		);
		assert_eq!(
			f.ensure_filename_extension("foo.ove", "Ove").unwrap(),
			"foo.ove"
		);
		// The "." in the suffix must actually be present.
		assert_eq!(
			f.ensure_filename_extension("fooove", "ove").unwrap(),
			"fooove.ove"
		);
		// Empty inputs are no-ops.
		assert_eq!(f.ensure_filename_extension("", "ove").unwrap(), "");
		assert_eq!(f.ensure_filename_extension("foo", "").unwrap(), "foo");
	}

	#[test]
	fn formatted_executable_for_platform() {
		let f = FileFunctions::new();
		let v = f.get_formatted_executable_for_platform("myapp").unwrap();
		#[cfg(target_os = "windows")]
		assert_eq!(v, "myapp.exe");
		#[cfg(not(target_os = "windows"))]
		assert_eq!(v, "myapp");
	}

	#[test]
	fn unique_identifier_existing_and_missing() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("id");
		let file = dir.join("data.txt");
		std::fs::write(&file, b"hello").unwrap();

		let id = f
			.get_unique_file_identifier(&file.to_string_lossy())
			.unwrap();
		assert_eq!(id.len(), 16);
		assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

		// Deterministic for the same file.
		let id2 = f
			.get_unique_file_identifier(&file.to_string_lossy())
			.unwrap();
		assert_eq!(id, id2);

		let missing = f
			.get_unique_file_identifier(&dir.join("nope.txt").to_string_lossy())
			.unwrap();
		assert_eq!(missing, "");
	}

	#[test]
	fn read_file_as_string_roundtrip() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("read");
		let file = dir.join("x.txt");
		std::fs::write(&file, b"hello world").unwrap();
		assert_eq!(
			f.read_file_as_string(&file.to_string_lossy()).unwrap(),
			"hello world"
		);
		// Missing file -> empty string.
		assert_eq!(
			f.read_file_as_string(&dir.join("missing.txt").to_string_lossy())
				.unwrap(),
			""
		);
	}

	#[test]
	fn directory_is_valid_create_semantics() {
		let f = FileFunctions::new();
		let parent = unique_temp_dir("dirval");
		let dir = parent.join("created");
		assert!(!dir.exists());
		assert!(f.directory_is_valid(&dir.to_string_lossy(), true));
		assert!(dir.is_dir());
		// Now it exists, so even without create it is valid.
		assert!(f.directory_is_valid(&dir.to_string_lossy(), false));

		let missing = parent.join("never");
		assert!(!missing.exists());
		assert!(!f.directory_is_valid(&missing.to_string_lossy(), false));
	}

	#[test]
	fn safe_temporary_filename_skips_existing() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("safetmp");
		let original = dir.join("video.mp4");
		// Block the first candidate so the counter must advance.
		std::fs::write(dir.join("video.tmp0.mp4"), b"x").unwrap();

		let result = f
			.get_safe_temporary_filename(&original.to_string_lossy())
			.unwrap();
		assert!(result.ends_with(".mp4"), "suffix preserved: {}", result);
		assert!(
			result.contains(".tmp1.mp4"),
			"should skip existing .tmp0, got: {}",
			result
		);
		assert!(!Path::new(&result).exists());
	}

	#[test]
	fn rename_allow_overwrite() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("rename");
		let from = dir.join("from.txt");
		let to = dir.join("to.txt");
		std::fs::write(&from, b"new").unwrap();
		std::fs::write(&to, b"old").unwrap();

		assert!(f.rename_file_allow_overwrite(&from.to_string_lossy(), &to.to_string_lossy()));
		assert!(!from.exists());
		assert_eq!(std::fs::read(&to).unwrap(), b"new");

		// Renaming a missing source fails.
		let missing = dir.join("missing.txt");
		assert!(!f.rename_file_allow_overwrite(
			&missing.to_string_lossy(),
			&dir.join("z.txt").to_string_lossy()
		));
	}

	#[test]
	fn copy_directory_recursive_and_overwrite_check() {
		let f = FileFunctions::new();
		let src = unique_temp_dir("cp-src");
		let nested = src.join("sub");
		std::fs::create_dir_all(&nested).unwrap();
		std::fs::write(src.join("a.txt"), b"a").unwrap();
		std::fs::write(nested.join("b.txt"), b"b").unwrap();

		let dest = unique_temp_dir("cp-dst");
		// Nothing in dest yet: safe to copy without overwriting.
		assert!(f.can_copy_directory_without_overwriting(
			&src.to_string_lossy(),
			&dest.to_string_lossy()
		));
		f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), false)
			.unwrap();
		assert!(dest.join("a.txt").exists());
		assert!(dest.join("sub").join("b.txt").exists());
		assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");

		// Copying again would overwrite a.txt.
		assert!(!f.can_copy_directory_without_overwriting(
			&src.to_string_lossy(),
			&dest.to_string_lossy()
		));
		// With overwrite it succeeds and refreshes content.
		f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), true)
			.unwrap();
		assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");

		// A missing source is a silent no-op (returns Ok).
		f.copy_directory(
			&dest.join("nope").to_string_lossy(),
			&unique_temp_dir("cp-missing").to_string_lossy(),
			false,
		)
		.unwrap();
	}

	#[test]
	fn application_and_temp_paths() {
		let f = FileFunctions::new();
		let app = f.get_application_path().unwrap();
		assert!(!app.is_empty());
		assert!(Path::new(&app).is_dir());

		let temp = f.get_temp_file_path().unwrap();
		assert!(!temp.is_empty());
		assert!(Path::new(&temp).is_dir());
	}

	#[test]
	fn configuration_location_obeys_env_override() {
		let _guard: MutexGuard<()> = config_lock().lock().unwrap();
		let f = FileFunctions::new();
		let dir = unique_temp_dir("config");
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let loc = f.get_configuration_location().unwrap();
		std::env::remove_var("OAK_CONFIG_DIR");
		assert_eq!(PathBuf::from(&loc), dir);
		assert!(dir.is_dir());
	}

	#[test]
	fn auto_recovery_root_derives_from_config() {
		let _guard: MutexGuard<()> = config_lock().lock().unwrap();
		let f = FileFunctions::new();
		let dir = unique_temp_dir("config2");
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let root = f.get_auto_recovery_root().unwrap();
		std::env::remove_var("OAK_CONFIG_DIR");
		assert!(root.ends_with("autorecovery"));
		assert!(PathBuf::from(&root).starts_with(&dir));
	}

	#[test]
	fn config_location_path_helper() {
		let _guard: MutexGuard<()> = config_lock().lock().unwrap();
		let dir = unique_temp_dir("clp");
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let p = config_location_path().unwrap();
		std::env::remove_var("OAK_CONFIG_DIR");
		assert_eq!(p, dir);
	}

	#[test]
	fn ends_with_case_insensitive_matrix() {
		assert!(ends_with_case_insensitive("foo.OVE", ".ove"));
		assert!(ends_with_case_insensitive("foo.ove", ".OVE"));
		assert!(ends_with_case_insensitive("x", "x"));
		assert!(ends_with_case_insensitive("x", ""));
		// Suffix longer than the string never matches.
		assert!(!ends_with_case_insensitive("ove", ".ove"));
		assert!(!ends_with_case_insensitive("", "a"));
		// Only ASCII A-Z fold (like the C++ range test); other bytes compare
		// exactly.
		assert!(ends_with_case_insensitive("a[", "["));
		assert!(!ends_with_case_insensitive("a[", "{"));
		assert!(ends_with_case_insensitive("vidéo.MP4", ".mp4"));
	}

	#[test]
	fn ensure_extension_dotfile_multi_ext_and_unicode() {
		let f = FileFunctions::new();
		// Dotfiles and multi-extension names just get the suffix appended.
		assert_eq!(
			f.ensure_filename_extension(".hidden", "txt").unwrap(),
			".hidden.txt"
		);
		assert_eq!(
			f.ensure_filename_extension("archive.tar", "gz").unwrap(),
			"archive.tar.gz"
		);
		assert_eq!(
			f.ensure_filename_extension("archive.tar.gz", "gz").unwrap(),
			"archive.tar.gz"
		);
		// Unicode names pass through unchanged apart from the suffix.
		assert_eq!(
			f.ensure_filename_extension("動画ファイル", "ove").unwrap(),
			"動画ファイル.ove"
		);
		// A bare "." suffix (empty extension) is a no-op per the C++.
		assert_eq!(f.ensure_filename_extension("foo.", "").unwrap(), "foo.");
	}

	#[test]
	fn safe_temporary_filename_naming_variants() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("safetmp2");

		// Counter 0 is used when nothing blocks it.
		let r0 = f
			.get_safe_temporary_filename(&dir.join("clip.mov").to_string_lossy())
			.unwrap();
		assert!(r0.ends_with("clip.tmp0.mov"), "got: {}", r0);

		// Complete suffix = everything from the FIRST dot (QFileInfo
		// completeSuffix semantics, `filefunctions.cpp:318-326`).
		let r1 = f
			.get_safe_temporary_filename(&dir.join("a.tar.gz").to_string_lossy())
			.unwrap();
		assert!(r1.ends_with("a.tmp0.tar.gz"), "got: {}", r1);

		// No extension at all.
		let r2 = f
			.get_safe_temporary_filename(&dir.join("README").to_string_lossy())
			.unwrap();
		assert!(r2.ends_with("README.tmp0"), "got: {}", r2);

		// Dotfile: basename is empty, complete suffix is the whole name.
		let r3 = f
			.get_safe_temporary_filename(&dir.join(".hidden").to_string_lossy())
			.unwrap();
		assert!(r3.ends_with(".tmp0.hidden"), "got: {}", r3);

		// No parent directory: the candidate is relative ("name.tmp0.ext").
		let r4 = f.get_safe_temporary_filename("video.mp4").unwrap();
		assert_eq!(r4, "video.tmp0.mp4");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn unique_identifier_differs_by_path() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("id2");
		let a = dir.join("a.txt");
		let b = dir.join("b.txt");
		std::fs::write(&a, b"same").unwrap();
		std::fs::write(&b, b"same").unwrap();
		// The absolute path is part of the hash input
		// (`filefunctions.cpp:100-103`), so identical content in different
		// paths yields different identifiers.
		let ida = f.get_unique_file_identifier(&a.to_string_lossy()).unwrap();
		let idb = f.get_unique_file_identifier(&b.to_string_lossy()).unwrap();
		assert_ne!(ida, idb);

		// A directory also gets an identifier (C++ only checks exists()).
		let idd = f
			.get_unique_file_identifier(&dir.to_string_lossy())
			.unwrap();
		assert_eq!(idd.len(), 16);

		// Empty filename -> absolute() of "" is the CWD which exists; but a
		// definitely-bogus relative name yields "".
		let bogus = f
			.get_unique_file_identifier("definitely-not-here.xyz")
			.unwrap();
		assert_eq!(bogus, "");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn read_file_as_string_binary_and_lossy() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("read2");
		// Invalid UTF-8 is replaced (lossy), not an error.
		let bad = dir.join("bad.bin");
		std::fs::write(&bad, b"a\xff\xfeb").unwrap();
		let s = f.read_file_as_string(&bad.to_string_lossy()).unwrap();
		assert_eq!(s, "a\u{FFFD}\u{FFFD}b");

		// Empty file reads as empty, indistinguishable from a missing file.
		let empty = dir.join("empty.txt");
		std::fs::write(&empty, b"").unwrap();
		assert_eq!(f.read_file_as_string(&empty.to_string_lossy()).unwrap(), "");

		// A directory cannot be read as a file -> empty.
		assert_eq!(f.read_file_as_string(&dir.to_string_lossy()).unwrap(), "");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn directory_is_valid_blocked_by_file() {
		let f = FileFunctions::new();
		let parent = unique_temp_dir("dirval2");
		// A regular file blocks directory creation at the same path.
		let blocker = parent.join("blocked");
		std::fs::write(&blocker, b"x").unwrap();
		assert!(!f.directory_is_valid(&blocker.to_string_lossy(), true));
		// ...and so does a file in the MIDDLE of the path to create.
		let nested = blocker.join("child");
		assert!(!f.directory_is_valid(&nested.to_string_lossy(), true));
		// Nested creation works when nothing blocks it.
		let deep = parent.join("x").join("y").join("z");
		assert!(f.directory_is_valid(&deep.to_string_lossy(), true));
		assert!(deep.is_dir());

		let _ = std::fs::remove_dir_all(&parent);
	}

	#[test]
	fn copy_directory_no_overwrite_preserves_existing() {
		let f = FileFunctions::new();
		let src = unique_temp_dir("cp2-src");
		std::fs::write(src.join("a.txt"), b"new").unwrap();
		let dest = unique_temp_dir("cp2-dst");
		std::fs::write(dest.join("a.txt"), b"old").unwrap();

		// would-overwrite detection fires on the direct conflict...
		assert!(!f.can_copy_directory_without_overwriting(
			&src.to_string_lossy(),
			&dest.to_string_lossy()
		));
		// ...and a missing source trivially reports "safe" (empty iteration,
		// `filefunctions.cpp:202`).
		assert!(f.can_copy_directory_without_overwriting(
			&src.join("nope").to_string_lossy(),
			&dest.to_string_lossy()
		));

		// overwrite=false: the copy of the conflicting file fails (logged)
		// and the destination keeps its old content; other files still copy.
		std::fs::write(src.join("b.txt"), b"b").unwrap();
		f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), false)
			.unwrap();
		assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
		assert_eq!(std::fs::read(dest.join("b.txt")).unwrap(), b"b");

		// overwrite=true replaces the conflicting file.
		f.copy_directory(&src.to_string_lossy(), &dest.to_string_lossy(), true)
			.unwrap();
		assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"new");

		let _ = std::fs::remove_dir_all(&src);
		let _ = std::fs::remove_dir_all(&dest);
	}

	#[test]
	fn can_copy_detects_nested_conflict() {
		let f = FileFunctions::new();
		let src = unique_temp_dir("cp3-src");
		std::fs::create_dir_all(src.join("sub")).unwrap();
		std::fs::write(src.join("sub").join("deep.txt"), b"x").unwrap();
		let dest = unique_temp_dir("cp3-dst");
		std::fs::create_dir_all(dest.join("sub")).unwrap();
		std::fs::write(dest.join("sub").join("deep.txt"), b"y").unwrap();
		// The conflict is two levels down: recursion must find it.
		assert!(!f.can_copy_directory_without_overwriting(
			&src.to_string_lossy(),
			&dest.to_string_lossy()
		));

		let _ = std::fs::remove_dir_all(&src);
		let _ = std::fs::remove_dir_all(&dest);
	}

	#[test]
	fn rename_to_fresh_destination() {
		let f = FileFunctions::new();
		let dir = unique_temp_dir("rename2");
		let from = dir.join("only.txt");
		let to = dir.join("fresh.txt");
		std::fs::write(&from, b"data").unwrap();
		assert!(f.rename_file_allow_overwrite(&from.to_string_lossy(), &to.to_string_lossy()));
		assert_eq!(std::fs::read(&to).unwrap(), b"data");

		// Destination existing as a DIRECTORY cannot be removed by
		// remove_file, so the rename fails (documented CPP-PARITY
		// divergence from C++ fs::remove, which would remove an empty dir).
		let from2 = dir.join("only2.txt");
		std::fs::write(&from2, b"z").unwrap();
		let todir = dir.join("destdir");
		std::fs::create_dir(&todir).unwrap();
		assert!(!f.rename_file_allow_overwrite(&from2.to_string_lossy(), &todir.to_string_lossy()));

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn temp_file_path_under_system_temp() {
		let f = FileFunctions::new();
		let temp = f.get_temp_file_path().unwrap();
		// `<temp>/oak`, created on demand (`filefunctions.cpp:184-196`).
		assert_eq!(PathBuf::from(&temp), std::env::temp_dir().join("oak"));
		assert!(Path::new(&temp).is_dir());
	}

	#[test]
	fn unique_temp_dirs_are_actually_unique() {
		let a = unique_temp_dir("uniq");
		let b = unique_temp_dir("uniq");
		assert_ne!(a, b);
		let _ = std::fs::remove_dir_all(&a);
		let _ = std::fs::remove_dir_all(&b);
	}
}

/// The default disk cache directory (C++ `DiskManager::
/// get_default_disk_cache_path`): `<configuration location>/mediacache`.
///
/// The `DiskCachePath` config key overrides the location when set (the
/// preferences dialog's cache-directory setting); an empty/absent value
/// keeps the default.
///
/// Single-lib unification: this used to live in the oakrender crate's
/// `bridge::common` fallback (see `docs/zh/plans/riir/single-lib.md`);
/// oaknode and oakrender both call it directly now.
pub fn default_disk_cache_path() -> String {
	// A configured override wins (whitespace-only counts as absent).
	if let Ok(custom) = crate::configstore::ConfigStore::instance().get(None, "DiskCachePath") {
		if !custom.trim().is_empty() {
			return custom;
		}
	}
	Path::new(
		&FileFunctions::new()
			.get_configuration_location()
			.unwrap_or_default(),
	)
	.join("mediacache")
	.to_string_lossy()
	.into_owned()
}
