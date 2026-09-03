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

//! Process-wide configuration store, mirroring
//! `src/common/src/configstore.h` and `include/common/config.h`.
//!
//! Per the config-wave ruling this is a process-wide singleton, NOT a
//! refcounted handle (same precedent as `OakCurrent`). Keys are typed
//! (string / int64 / double / bool); typed getters return a caller-supplied
//! fallback when the key is absent or of a different type. Persistence is
//! a TOML file at `<config_location>/config.toml`; a legacy
//! `<config_location>/config.ini` (written by old C++/Rust builds) is
//! read once for migration and left in place.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::error::{Error, Result};

/// Error-handler callback for user-visible config errors
/// (`oak_coreConfigErrorHandler`). Called with title, message, and the
/// registered userdata.
pub type ErrorHandler = Option<unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void)>;

/// Entry types (`oak_coreConfigEntryType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
	/// No entry / null type.
	None,
	/// String.
	String,
	/// Integer.
	Int,
	/// Double.
	Double,
	/// Boolean.
	Bool,
}

/// A single typed configuration value (`ConfigStore::Entry` in the C++).
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
	/// String.
	String(String),
	/// Integer.
	Int(i64),
	/// Double.
	Double(f64),
	/// Boolean.
	Bool(bool),
}

/// Process-wide singleton store. `AtomicPtr`-based error handler plus a
/// `Mutex`-guarded key/value map.
pub struct ConfigStore {
	/// Registered error handler (may be null).
	error_handler: AtomicPtr<c_void>,
	/// Userdata passed to the error handler.
	error_userdata: AtomicPtr<c_void>,
	/// `group/key` -> typed value.
	entries: Mutex<HashMap<String, ConfigValue>>,
}

impl ConfigStore {
	/// The process-wide singleton.
	///
	/// CPP-PARITY: the C++ constructor runs `set_defaults()` on construction
	/// (`configstore.cpp:41-44`), so the singleton starts pre-loaded with the
	/// compiled-in defaults, exactly as `ConfigStore::current()` would.
	pub fn instance() -> &'static ConfigStore {
		static INSTANCE: OnceLock<ConfigStore> = OnceLock::new();
		INSTANCE.get_or_init(|| {
			let store = ConfigStore {
				error_handler: AtomicPtr::new(std::ptr::null_mut()),
				error_userdata: AtomicPtr::new(std::ptr::null_mut()),
				entries: Mutex::new(HashMap::new()),
			};
			store.set_defaults();
			store
		})
	}

	/// Reset to compiled-in defaults and load `config.toml`, falling back to
	/// the legacy `config.ini` for migration when no TOML file exists yet (a
	/// missing config is not an error). Mirrors `ConfigStore::load()`
	/// (`configstore.cpp:239`).
	pub fn load(&self) -> Result<()> {
		self.set_defaults();

		let toml_path = get_config_file_path();

		let metadata = match std::fs::metadata(&toml_path) {
			Ok(m) => m,
			Err(_) => {
				// No `config.toml` yet: either this is a fresh install
				// (defaults are fine, not an error) or an old `config.ini`
				// remains to be migrated.
				return self.load_legacy_ini();
			}
		};

		// CPP-PARITY: the C++ treats a directory as unreadable (exists() is
		// checked first, then is_regular_file()), reporting the error and
		// returning false (`configstore.cpp:245-258`).
		if !metadata.is_file() {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			return Err(Error::Failed("config.toml is not a regular file".into()));
		}

		let content = std::fs::read_to_string(&toml_path).map_err(|_| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed("config.toml could not be read".into())
		})?;

		// A corrupt TOML file is reported and surfaced, not silently ignored:
		// ignoring it would wipe the user's settings on the next save().
		let parsed: toml::Value = toml::from_str(&content).map_err(|e| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed(format!("config.toml could not be parsed: {}", e))
		})?;

		self.apply_toml(&parsed);
		Ok(())
	}

	/// Legacy migration path: read an old `config.ini` (written by the C++
	/// or pre-TOML Rust builds) with the INI parser, then immediately persist
	/// the loaded values as `config.toml` so the next startup reads TOML. The
	/// INI file is deliberately left in place. A missing INI is not an error.
	fn load_legacy_ini(&self) -> Result<()> {
		let path = get_config_ini_path();

		let metadata = match std::fs::metadata(&path) {
			Ok(m) => m,
			Err(_) => {
				// No saved settings at all: defaults are fine, not an error.
				return Ok(());
			}
		};

		// CPP-PARITY: a directory at the config path is an error (see load()).
		if !metadata.is_file() {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			return Err(Error::Failed("config.ini is not a regular file".into()));
		}

		// CPP-PARITY: invalid UTF-8 maps to the same "unreadable file" failure.
		let content = std::fs::read_to_string(&path).map_err(|_| {
			self.report_error(
				"Error loading settings",
				"Failed to load application settings. This session will use defaults.",
			);
			Error::Failed("config.ini could not be read".into())
		})?;

		self.parse_ini(&content);

		// Write the migrated values as TOML now. A failed write is reported by
		// save() but does not fail load(): the values are already in memory,
		// and the migration is retried on the next startup.
		let _ = self.save();

		Ok(())
	}

	/// Parse legacy INI text into the store (QSettings-style lax parsing).
	/// Kept solely for the migration path in `load_legacy_ini`.
	fn parse_ini(&self, content: &str) {
		let mut group = String::new();
		for raw_line in content.lines() {
			let line = trim(raw_line);
			if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
				continue;
			}

			if line.starts_with('[') && line.ends_with(']') {
				group = trim(&line[1..line.len() - 1]);
				continue;
			}

			let eq = match line.find('=') {
				Some(e) => e,
				None => {
					// Malformed line: skip, keep going (matches QSettings'
					// lax INI parsing).
					continue;
				}
			};

			let key = trim(&line[..eq]);
			let value = trim(&line[eq + 1..]);
			if key.is_empty() {
				continue;
			}
			let full_key = if group.is_empty() {
				key.to_string()
			} else {
				format!("{}/{}", group, key)
			};

			match self.get_entry(&full_key) {
				Some(existing) => {
					// Known key: honor its declared type. An unparseable value
					// keeps the default.
					let ty = to_entry_type(&existing);
					if let Some(parsed) = string_to_value(&value, ty) {
						self.set_entry(full_key, parsed);
					}
				}
				None => {
					// Unknown key: stored as a string.
					self.set_entry(full_key, ConfigValue::String(value));
				}
			}
		}
	}

	/// Apply parsed TOML to the store. Top-level keys become flat entries;
	/// `[group]` tables (including nested dotted tables) flatten back to
	/// `group/sub` keys. Known keys keep their declared type; unknown keys are
	/// stored as strings — the same semantics as the INI loader.
	fn apply_toml(&self, parsed: &toml::Value) {
		if let Some(table) = parsed.as_table() {
			self.flatten_toml("", table);
		}
	}

	/// Flatten a TOML table into `prefix`-joined store keys.
	fn flatten_toml(&self, prefix: &str, table: &toml::map::Map<String, toml::Value>) {
		for (key, value) in table {
			let full_key = if prefix.is_empty() {
				key.clone()
			} else {
				format!("{}/{}", prefix, key)
			};
			match value.as_table() {
				Some(sub) => self.flatten_toml(&full_key, sub),
				None => self.apply_toml_entry(&full_key, value),
			}
		}
	}

	/// Apply a single (already flattened) key/value to the store.
	fn apply_toml_entry(&self, full_key: &str, value: &toml::Value) {
		match self.get_entry(full_key) {
			Some(existing) => {
				// Known key: honor its declared type. An unconvertible value
				// keeps the default.
				let ty = to_entry_type(&existing);
				if let Some(converted) = toml_value_to_typed(value, ty) {
					self.set_entry(full_key.to_string(), converted);
				}
			}
			None => {
				// Unknown key: stored as a string.
				self.set_entry(
					full_key.to_string(),
					ConfigValue::String(toml_value_to_string(value)),
				);
			}
		}
	}

	/// Write the current store to `config.toml` via temp file + rename.
	/// Mirrors `ConfigStore::save()` (`configstore.cpp:316`).
	pub fn save(&self) -> Result<()> {
		let real_filename = get_config_file_path();
		let temp_filename = format!("{}.tmp", real_filename);

		// Flat keys are written as top-level TOML keys; keys containing '/'
		// become `[group]` tables (group = everything before the last '/'),
		// keeping the QSettings INI key shape. Values are written as native
		// TOML int/float/bool/string; non-finite doubles have no TOML literal
		// and are written as strings, which load() restores by declared type.
		let mut sections: BTreeMap<String, BTreeMap<String, toml::Value>> = BTreeMap::new();
		{
			let guard = self.entries.lock().unwrap();
			for (key, value) in guard.iter() {
				let toml_value = config_value_to_toml(value);
				match key.rfind('/') {
					Some(slash) => {
						let group = key[..slash].to_string();
						let sub = key[slash + 1..].to_string();
						sections
							.entry(group)
							.or_default()
							.insert(sub, toml_value);
					}
					None => {
						sections
							.entry(String::new())
							.or_default()
							.insert(key.clone(), toml_value);
					}
				}
			}
		}

		let mut root: toml::map::Map<String, toml::Value> = toml::map::Map::new();
		if let Some(flat) = sections.remove("") {
			for (sub, value) in flat {
				root.insert(sub, value);
			}
		}
		for (group, subs) in sections {
			root.insert(group, toml::Value::Table(subs.into_iter().collect()));
		}

		let out = toml::to_string_pretty(&toml::Value::Table(root)).map_err(|e| {
			self.report_error(
				"Error saving settings",
				"Failed to serialize application settings as TOML.",
			);
			Error::Failed(format!("config.toml could not be serialized: {}", e))
		})?;

		if std::fs::write(&temp_filename, out.as_bytes()).is_err() {
			self.report_error(
				"Error saving settings",
				"Failed to save application settings. The application may lack write \
				 permissions for this location.",
			);
			return Err(Error::Failed(
				"temp config file could not be written".into(),
			));
		}

		// CPP-PARITY: rename temp -> real; on POSIX this overwrites
		// atomically, so the remove+retry fallback only matters on Windows,
		// mirrored for behavioral parity (`configstore.cpp:378-390`).
		if std::fs::rename(&temp_filename, &real_filename).is_err() {
			let _ = std::fs::remove_file(&real_filename);
			if std::fs::rename(&temp_filename, &real_filename).is_err() {
				self.report_error(
					"Error saving settings",
					"Failed to overwrite the application settings file.",
				);
				return Err(Error::Failed(
					"config.toml could not be renamed into place".into(),
				));
			}
		}

		Ok(())
	}

	/// Reset to compiled-in defaults (drop custom keys).
	pub fn reset_defaults(&self) -> Result<()> {
		self.set_defaults();
		Ok(())
	}

	/// Set a string entry. Mirrors `oak_core_config_set` (`config.cpp:102`):
	/// a new key is created as a string; setting an existing typed entry
	/// parses the string into its declared type, and an unparseable value
	/// leaves the entry unchanged.
	pub fn set(&self, group: Option<&str>, key: &str, value: &str) {
		if key.is_empty() {
			return;
		}
		let joined = join_key(group, key);
		match self.get_entry(&joined) {
			Some(ConfigValue::String(_)) | None => {
				self.set_entry(joined, ConfigValue::String(value.to_string()));
			}
			Some(other) => {
				let ty = to_entry_type(&other);
				if let Some(parsed) = string_to_value(value, ty) {
					self.set_entry(joined, parsed);
				}
			}
		}
	}

	/// Read an entry as a string (two-stage getter semantics: formatted
	/// for numeric/bool entries). Mirrors `oak_core_config_get`
	/// (`config.cpp:134`).
	pub fn get(&self, group: Option<&str>, key: &str) -> Result<String> {
		if key.is_empty() {
			return Err(Error::Invalid);
		}
		match self.get_entry(&join_key(group, key)) {
			None => Err(Error::NotFound),
			Some(v) => Ok(value_to_string(&v)),
		}
	}

	/// Read an INT entry; `fallback` when absent or a different type.
	pub fn get_int(&self, group: Option<&str>, key: &str, fallback: i32) -> i32 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Int(v)) => v as i32,
			_ => fallback,
		}
	}

	/// Read a DOUBLE entry; `fallback` when absent or a different type.
	pub fn get_double(&self, group: Option<&str>, key: &str, fallback: f64) -> f64 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Double(v)) => v,
			_ => fallback,
		}
	}

	/// Set an INT entry.
	pub fn set_int(&self, group: Option<&str>, key: &str, v: i32) {
		self.set_int64(group, key, v as i64);
	}

	/// Read an INT entry as i64; `fallback` when absent or a different type.
	pub fn get_int64(&self, group: Option<&str>, key: &str, fallback: i64) -> i64 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Int(v)) => v,
			_ => fallback,
		}
	}

	/// Set an INT entry as i64.
	pub fn set_int64(&self, group: Option<&str>, key: &str, v: i64) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Int(v));
	}

	/// Read a BOOL entry as int 0/1; `fallback` when absent or a different
	/// type.
	pub fn get_bool(&self, group: Option<&str>, key: &str, fallback: i32) -> i32 {
		if key.is_empty() {
			return fallback;
		}
		match self.get_entry(&join_key(group, key)) {
			Some(ConfigValue::Bool(b)) => {
				if b {
					1
				} else {
					0
				}
			}
			_ => fallback,
		}
	}

	/// Set a BOOL entry.
	pub fn set_bool(&self, group: Option<&str>, key: &str, v: i32) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Bool(v != 0));
	}

	/// Set a DOUBLE entry.
	pub fn set_double(&self, group: Option<&str>, key: &str, v: f64) {
		if key.is_empty() {
			return;
		}
		self.set_entry(join_key(group, key), ConfigValue::Double(v));
	}

	/// Entry type of a key, or `NotFound`. Mirrors `oak_core_config_entry_type`
	/// (`config.cpp:270`).
	pub fn entry_type(&self, group: Option<&str>, key: &str) -> Result<EntryType> {
		if key.is_empty() {
			return Err(Error::Invalid);
		}
		match self.get_entry(&join_key(group, key)) {
			None => Err(Error::NotFound),
			Some(v) => Ok(to_entry_type(&v)),
		}
	}

	/// Register (or clear, with a null handler) the error handler. Mirrors
	/// the domain half of `oak_core_config_set_error_handler` (`config.cpp:288`).
	pub fn set_error_handler(&self, handler: ErrorHandler, userdata: *mut c_void) -> Result<()> {
		let ptr = handler.map_or(std::ptr::null_mut(), |h| h as *mut c_void);
		self.error_handler.store(ptr, Ordering::Release);
		self.error_userdata.store(userdata, Ordering::Release);
		Ok(())
	}

	/// Report a user-visible error through the registered handler, or to
	/// stderr when none is registered. Mirrors `ConfigStore::report_error`
	/// (`configstore.cpp:51`).
	fn report_error(&self, title: &str, message: &str) {
		let handler = self.error_handler.load(Ordering::Acquire);
		if handler.is_null() {
			eprintln!("{}: {}", title, message);
			return;
		}
		let userdata = self.error_userdata.load(Ordering::Acquire);
		let t = CString::new(title).unwrap_or_default();
		let m = CString::new(message).unwrap_or_default();
		// The pointer stored by `set_error_handler` is a function pointer
		// cast to `*mut c_void`; recover it for the call.
		let h: unsafe extern "C" fn(*const c_char, *const c_char, *mut c_void) =
			unsafe { std::mem::transmute(handler) };
		unsafe {
			h(t.as_ptr(), m.as_ptr(), userdata);
		}
	}

	/// Reset to compiled-in defaults (drop custom keys). Mirrors
	/// `ConfigStore::set_defaults()` (`configstore.cpp:76`).
	fn set_defaults(&self) {
		let mut guard = self.entries.lock().unwrap();
		guard.clear();

		// Only the keys the de-Qt engine modules (oaknode/oakrender/oakcodec)
		// actually read are registered here; the app-layer keys of the old Qt
		// config arrive with the app/config wave. Enum-valued ints hardcode
		// the numeric values of their (still Qt-based) defining headers:
		//
		//  - Timeline::k_thumbnail_in_out / k_waveforms_enabled = 1
		//    (engine/timeline/timelinecommon.h)
		//  - PixelFormat::f32 = 4 (core/include/olive/core/render/pixelformat.h)
		//  - VideoParams::k_interlace_none = 0 (src/common/src/videoparams.h)
		//  - k_channel_layout_stereo = 3
		//    (core/include/olive/core/render/channellayout.h)
		//  - ColorCoding::k_red..k_navy = 0..11, k_lime = 6
		//    (engine/ui/colorcoding.h)

		guard.insert("TimelineThumbnailMode".into(), ConfigValue::Int(1));
		guard.insert("TimelineWaveformMode".into(), ConfigValue::Int(1));

		guard.insert("DefaultSequenceWidth".into(), ConfigValue::Int(1920));
		guard.insert("DefaultSequenceHeight".into(), ConfigValue::Int(1080));
		// Rational settings are stored as strings in oakcore_rational
		// "num/den" form; this mirrors the old default Rational(1001, 30000).
		guard.insert(
			"DefaultSequenceFrameRate".into(),
			ConfigValue::String("1001/30000".into()),
		);
		guard.insert(
			"DefaultSequencePixelAspect".into(),
			ConfigValue::String("1/1".into()),
		);
		guard.insert("DefaultSequenceInterlacing".into(), ConfigValue::Int(0));
		guard.insert(
			"DefaultSequenceAudioFrequency".into(),
			ConfigValue::Int(48000),
		);
		guard.insert("DefaultSequenceAudioLayout".into(), ConfigValue::Int(3));
		guard.insert("OfflinePixelFormat".into(), ConfigValue::Int(4));

		guard.insert("SplitClipsCopyNodes".into(), ConfigValue::Bool(true));
		guard.insert("UseProxyMedia".into(), ConfigValue::Bool(true));
		guard.insert("UseGLFinish".into(), ConfigValue::Bool(false));
		guard.insert("ReassocLinToNonLin".into(), ConfigValue::Bool(false));

		guard.insert(
			"GraphicsBackend".into(),
			ConfigValue::String("opengl".into()),
		);
		guard.insert("LUTLibraryPaths".into(), ConfigValue::String(String::new()));

		guard.insert("DiskCacheSaveInterval".into(), ConfigValue::Int(10000));
		guard.insert("AutoCacheDelay".into(), ConfigValue::Int(1000));
		guard.insert("DiskCacheBehind".into(), ConfigValue::String("0/1".into()));
		guard.insert("DiskCacheAhead".into(), ConfigValue::String("60/1".into()));

		guard.insert("ProxyWidth".into(), ConfigValue::Int(1280));
		guard.insert("ProxyHeight".into(), ConfigValue::Int(720));
		guard.insert("ProxyDivider".into(), ConfigValue::Int(1));
		guard.insert("ProxyCRF".into(), ConfigValue::Int(23));
		guard.insert("ProxyPreset".into(), ConfigValue::String("veryfast".into()));
		guard.insert("ProxyIncludeAudio".into(), ConfigValue::Bool(true));

		guard.insert("MarkerColor".into(), ConfigValue::Int(6));
		for i in 0..=11 {
			guard.insert(format!("CatColor{}", i), ConfigValue::Int(i));
		}
	}

	/// Copy the entry for a joined key out of the map, or `None` when absent.
	fn get_entry(&self, key: &str) -> Option<ConfigValue> {
		let guard = self.entries.lock().unwrap();
		guard.get(key).cloned()
	}

	/// Insert (or replace) an entry.
	fn set_entry(&self, key: String, value: ConfigValue) {
		let mut guard = self.entries.lock().unwrap();
		guard.insert(key, value);
	}
}

/// Joins group and key into the stored "group/key" form. Mirrors
/// `ConfigStore::join_key` (`configstore.cpp:68`).
pub(crate) fn join_key(group: Option<&str>, key: &str) -> String {
	match group {
		Some(g) if !g.is_empty() => format!("{}/{}", g, key),
		_ => key.to_string(),
	}
}

/// Serializes a value for the string getter. Mirrors
/// `ConfigStore::value_to_string` (`configstore.cpp:154`).
fn value_to_string(value: &ConfigValue) -> String {
	match value {
		ConfigValue::String(s) => s.clone(),
		ConfigValue::Int(i) => i.to_string(),
		ConfigValue::Double(d) => format_g(*d),
		ConfigValue::Bool(b) => {
			if *b {
				"true".to_string()
			} else {
				"false".to_string()
			}
		}
	}
}

/// Converts a stored value to its TOML representation. Non-finite doubles
/// have no TOML literal, so they are written as strings ("nan"/"inf"/"-inf")
/// and restored by `toml_value_to_typed` via the declared-type string
/// conversion.
fn config_value_to_toml(value: &ConfigValue) -> toml::Value {
	match value {
		ConfigValue::String(s) => toml::Value::String(s.clone()),
		ConfigValue::Int(i) => toml::Value::Integer(*i),
		ConfigValue::Double(d) if d.is_finite() => toml::Value::Float(*d),
		ConfigValue::Double(_) => toml::Value::String(value_to_string(value)),
		ConfigValue::Bool(b) => toml::Value::Boolean(*b),
	}
}

/// Converts a TOML value to the declared entry type, mirroring the INI
/// loader's known-key conversion: a value whose TOML type does not match the
/// declared type is parsed from its textual form (so `"640"` still loads an
/// Int key), and an unparseable value keeps the default.
fn toml_value_to_typed(value: &toml::Value, ty: EntryType) -> Option<ConfigValue> {
	match ty {
		EntryType::Int => match value {
			toml::Value::Integer(i) => Some(ConfigValue::Int(*i)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Int),
		},
		EntryType::Double => match value {
			toml::Value::Float(f) => Some(ConfigValue::Double(*f)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Double),
		},
		EntryType::Bool => match value {
			toml::Value::Boolean(b) => Some(ConfigValue::Bool(*b)),
			_ => string_to_value(&toml_value_to_text(value), EntryType::Bool),
		},
		EntryType::String => Some(ConfigValue::String(toml_value_to_text(value))),
		EntryType::None => None,
	}
}

/// The textual form of a TOML value, used for declared-type coercion and
/// known String keys: the raw string for strings, the TOML literal otherwise.
fn toml_value_to_text(value: &toml::Value) -> String {
	match value {
		toml::Value::String(s) => s.clone(),
		_ => value.to_string(),
	}
}

/// The INI-parity string form for an unknown key read from TOML: matches what
/// the legacy INI writer would have persisted for the same value, so unknown
/// keys behave identically across formats.
fn toml_value_to_string(value: &toml::Value) -> String {
	match value {
		toml::Value::String(s) => s.clone(),
		toml::Value::Integer(i) => i.to_string(),
		toml::Value::Boolean(b) => b.to_string(),
		toml::Value::Float(f) => format_g(*f),
		_ => value.to_string(),
	}
}

/// Parses text into a value of the given type; `None` when it cannot be
/// parsed as the requested type (strings always parse). Mirrors
/// `ConfigStore::string_to_value` (`configstore.cpp:173`).
fn string_to_value(text: &str, ty: EntryType) -> Option<ConfigValue> {
	match ty {
		EntryType::String => Some(ConfigValue::String(text.to_string())),
		EntryType::Int => {
			// CPP-PARITY: std::stoll skips leading whitespace but requires the
			// whole remaining string to be consumed; `trim_start().parse()`
			// reproduces that (trailing whitespace/junk fails the parse).
			text.trim_start().parse::<i64>().ok().map(ConfigValue::Int)
		}
		EntryType::Double => {
			// CPP-PARITY: std::stod additionally accepts inf/infinity/nan
			// (case-insensitive); handled explicitly since Rust's f64 parse
			// rejects them.
			let t = text.trim_start();
			let parsed = if t.eq_ignore_ascii_case("inf")
				|| t.eq_ignore_ascii_case("+inf")
				|| t.eq_ignore_ascii_case("infinity")
				|| t.eq_ignore_ascii_case("+infinity")
			{
				Some(f64::INFINITY)
			} else if t.eq_ignore_ascii_case("-inf") || t.eq_ignore_ascii_case("-infinity") {
				Some(f64::NEG_INFINITY)
			} else if t.eq_ignore_ascii_case("nan")
				|| t.eq_ignore_ascii_case("+nan")
				|| t.eq_ignore_ascii_case("-nan")
			{
				Some(f64::NAN)
			} else {
				t.parse::<f64>().ok()
			};
			parsed.map(ConfigValue::Double)
		}
		EntryType::Bool => {
			if text == "true" || text == "1" {
				Some(ConfigValue::Bool(true))
			} else if text == "false" || text == "0" {
				Some(ConfigValue::Bool(false))
			} else {
				None
			}
		}
		EntryType::None => None,
	}
}

/// Maps a value back to its entry type.
fn to_entry_type(value: &ConfigValue) -> EntryType {
	match value {
		ConfigValue::String(_) => EntryType::String,
		ConfigValue::Int(_) => EntryType::Int,
		ConfigValue::Double(_) => EntryType::Double,
		ConfigValue::Bool(_) => EntryType::Bool,
	}
}

/// Trims `" \t\r\n"` from both ends. Mirrors the anonymous `trim()` helper
/// in `configstore.cpp:227`.
fn trim(s: &str) -> String {
	s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
		.to_string()
}

/// The configuration directory. Delegates to
/// `FileFunctions::get_configuration_location()` exactly like the C++ does
/// (`configstore.cpp:61-66` composes the path as
/// `FileFunctions::get_configuration_location() / "config.ini"`), so the
/// `OAK_CONFIG_DIR` override, portable mode, the macOS Application Support
/// default, and the `<root>/oak` suffix all match.
fn configuration_location() -> String {
	crate::filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned())
}

/// `<config_location>/config.toml`.
fn get_config_file_path() -> String {
	format!("{}/config.toml", configuration_location())
}

/// `<config_location>/config.ini` — the legacy format, read once for
/// migration when no `config.toml` exists yet.
fn get_config_ini_path() -> String {
	format!("{}/config.ini", configuration_location())
}

/// Hand-rolled C `%g` formatting with the default precision 6, used by
/// `value_to_string` for doubles. Faithful to `snprintf(buf, ..., "%g", v)`
/// in `configstore.cpp:163`.
///
/// CPP-PARITY: C `%g` rounds to 6 significant digits, drops trailing zeros
/// and a trailing decimal point, and switches to scientific notation when the
/// decimal exponent is < -4 or >= 6. Rust's `{:.*e}` / `{:.*}` both use
/// round-half-to-even like glibc, so results match for ordinary values. The
/// fixed-vs-scientific choice here is driven by the exact decimal exponent
/// (computed from the value), which also handles carry-over rounding (e.g.
/// 999999.5 -> "1000000") the way C does.
fn format_g(v: f64) -> String {
	if v.is_nan() {
		return "nan".to_string();
	}
	if v.is_infinite() {
		return if v.is_sign_negative() { "-inf" } else { "inf" }.to_string();
	}
	if v == 0.0 {
		return if v.is_sign_negative() { "-0" } else { "0" }.to_string();
	}
	let sign = if v.is_sign_negative() { "-" } else { "" };
	let a = v.abs();

	const PRECISION: i32 = 6;
	// Round to PRECISION significant digits via scientific formatting; `%g`
	// decides between fixed and scientific style using the exponent of the
	// ROUNDED value (so e.g. 999999.5 -> "1e+06", not "1000000").
	let sci = format!("{:.*e}", (PRECISION - 1) as usize, a); // "d.ddddd e±N"
	let exp = sci.split('e').nth(1).unwrap().parse::<i32>().unwrap();
	let xr = exp;
	let body = if xr < -4 || xr >= PRECISION {
		// Scientific notation: mantissa is already rounded; strip trailing zeros.
		let mant = sci.split('e').next().unwrap();
		let m = trim_mantissa(mant);
		format!("{}e{}", m, format_exp(&xr.to_string()))
	} else {
		// Fixed notation: (PRECISION - 1 - xr) decimals, trailing zeros removed.
		let decimals = (PRECISION - 1 - xr).max(0) as usize;
		trim_mantissa(&format!("{:.*}", decimals, a))
	};
	format!("{}{}", sign, body)
}

/// Removes trailing zeros after the decimal point and a trailing decimal
/// point, but never truncates integer digits (`%g` behavior).
fn trim_mantissa(m: &str) -> String {
	match m.split_once('.') {
		None => m.to_string(),
		Some((int, frac)) => {
			let frac = frac.trim_end_matches('0');
			if frac.is_empty() {
				int.to_string()
			} else {
				format!("{}.{}", int, frac)
			}
		}
	}
}

/// Formats a decimal exponent as C does: a sign always followed by at least
/// two digits (e.g. "e+06", "e-05", "e+100").
fn format_exp(e: &str) -> String {
	let (neg, mag) = match e.strip_prefix('-') {
		Some(rest) => (true, rest),
		None => (false, e.strip_prefix('+').unwrap_or(e)),
	};
	let sign = if neg { "-" } else { "+" };
	if mag.len() < 2 {
		format!("{}{:0>2}", sign, mag)
	} else {
		format!("{}{}", sign, mag)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::c_char;
	use std::path::Path;
	use std::sync::Mutex as StdMutex;

	/// Serializes every test that touches the process-global singleton and
	/// the `OAK_CONFIG_DIR` env override, so parallel tests cannot race.
	///
	/// Uses the crate-wide test lock so `filefunctions` tests (which also
	/// mutate `OAK_CONFIG_DIR`) serialize on the SAME mutex.
	fn test_lock() -> &'static StdMutex<()> {
		crate::test_support::env_lock()
	}

	/// Point `OAK_CONFIG_DIR` at an isolated temp dir, run `f`, then clean up.
	fn with_temp_config<T>(f: impl FnOnce(&Path) -> T) -> T {
		let _guard = test_lock().lock().unwrap();
		let dir =
			std::env::temp_dir().join(format!("oak_core_configstore_test_{}", std::process::id()));
		let _ = std::fs::create_dir_all(&dir);
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let result = f(&dir);
		std::env::remove_var("OAK_CONFIG_DIR");
		let _ = std::fs::remove_dir_all(&dir);
		result
	}

	// ---- Pure logic: %g formatting -------------------------------------

	#[test]
	fn test_format_g_matches_c() {
		assert_eq!(format_g(0.0), "0");
		assert_eq!(format_g(-0.0), "-0");
		assert_eq!(format_g(1920.0), "1920");
		assert_eq!(format_g(100.0), "100");
		assert_eq!(format_g(3.15), "3.15");
		assert_eq!(format_g(1.5), "1.5");
		assert_eq!(format_g(0.1), "0.1");
		assert_eq!(format_g(-3.5), "-3.5");
		assert_eq!(format_g(1.23456789), "1.23457");
		assert_eq!(format_g(1234567.0), "1.23457e+06");
		assert_eq!(format_g(0.00012345), "0.00012345");
		assert_eq!(format_g(0.000012345), "1.2345e-05");
		assert_eq!(format_g(999999.5), "1e+06");
		assert_eq!(format_g(f64::NAN), "nan");
		assert_eq!(format_g(f64::INFINITY), "inf");
		assert_eq!(format_g(f64::NEG_INFINITY), "-inf");
	}

	#[test]
	fn test_join_key() {
		assert_eq!(join_key(None, "k"), "k");
		assert_eq!(join_key(Some(""), "k"), "k");
		assert_eq!(join_key(Some("g"), "k"), "g/k");
	}

	// ---- Defaults ------------------------------------------------------

	#[test]
	fn test_defaults() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
		assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
		assert_eq!(s.get_int(None, "DefaultSequenceInterlacing", -1), 0);
		assert_eq!(s.get_int(None, "DefaultSequenceAudioFrequency", -1), 48000);
		assert_eq!(s.get_int(None, "DefaultSequenceAudioLayout", -1), 3);
		assert_eq!(s.get_int(None, "OfflinePixelFormat", -1), 4);
		assert_eq!(s.get_int(None, "DiskCacheSaveInterval", -1), 10000);
		assert_eq!(s.get_int(None, "AutoCacheDelay", -1), 1000);
		assert_eq!(s.get_int(None, "ProxyWidth", -1), 1280);
		assert_eq!(s.get_int(None, "ProxyHeight", -1), 720);
		assert_eq!(s.get_int(None, "ProxyDivider", -1), 1);
		assert_eq!(s.get_int(None, "ProxyCRF", -1), 23);
		assert_eq!(s.get_int(None, "MarkerColor", -1), 6);
		assert_eq!(s.get_int(None, "CatColor0", -1), 0);
		assert_eq!(s.get_int(None, "CatColor11", -1), 11);

		assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 1);
		assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 1);
		assert_eq!(s.get_bool(None, "UseGLFinish", 1), 0);
		assert_eq!(s.get_bool(None, "ReassocLinToNonLin", 1), 0);
		assert_eq!(s.get_bool(None, "ProxyIncludeAudio", -1), 1);

		assert_eq!(s.get(None, "GraphicsBackend").unwrap(), "opengl");
		assert_eq!(
			s.get(None, "DefaultSequenceFrameRate").unwrap(),
			"1001/30000"
		);
		assert_eq!(s.get(None, "DefaultSequencePixelAspect").unwrap(), "1/1");
		assert_eq!(s.get(None, "LUTLibraryPaths").unwrap(), "");
		assert_eq!(s.get(None, "DiskCacheBehind").unwrap(), "0/1");
		assert_eq!(s.get(None, "DiskCacheAhead").unwrap(), "60/1");
		assert_eq!(s.get(None, "ProxyPreset").unwrap(), "veryfast");
	}

	// ---- Basic getters/setters -----------------------------------------

	#[test]
	fn test_set_get_string() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set(None, "MyString", "hello world");
		assert_eq!(s.get(None, "MyString").unwrap(), "hello world");
		assert_eq!(s.entry_type(None, "MyString").unwrap(), EntryType::String);
	}

	#[test]
	fn test_get_missing_and_invalid() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		assert!(matches!(
			s.get(None, "DefinitelyMissing"),
			Err(Error::NotFound)
		));
		assert!(matches!(
			s.entry_type(None, "DefinitelyMissing"),
			Err(Error::NotFound)
		));
		assert!(matches!(s.get(None, ""), Err(Error::Invalid)));
		assert!(matches!(s.entry_type(None, ""), Err(Error::Invalid)));
	}

	#[test]
	fn test_int_get_set_fallback() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_int(None, "MyInt", 42);
		assert_eq!(s.get_int(None, "MyInt", 0), 42);
		assert_eq!(s.get_int64(None, "MyInt", 0), 42);
		assert_eq!(s.entry_type(None, "MyInt").unwrap(), EntryType::Int);

		let big = i64::from(i32::MAX) + 1;
		s.set_int64(None, "MyBigInt", big);
		assert_eq!(s.get_int64(None, "MyBigInt", 0), big);

		assert_eq!(s.get_int(None, "MissingInt", 7), 7);
		assert_eq!(s.get_int64(None, "MissingInt", 7), 7);

		// Wrong type -> fallback.
		s.set(None, "MyStr", "abc");
		assert_eq!(s.get_int(None, "MyStr", 9), 9);
		assert_eq!(s.get_int64(None, "MyStr", 9), 9);
	}

	#[test]
	fn test_double_get_set_fallback() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_double(None, "MyD", 3.15);
		assert_eq!(s.get_double(None, "MyD", -1.0), 3.15);
		assert_eq!(s.get(None, "MyD").unwrap(), "3.15");
		assert_eq!(s.entry_type(None, "MyD").unwrap(), EntryType::Double);

		assert_eq!(s.get_double(None, "MissingD", 2.5), 2.5);
		s.set(None, "MyStr2", "xyz");
		assert_eq!(s.get_double(None, "MyStr2", 2.5), 2.5);
	}

	#[test]
	fn test_bool_get_set_fallback() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_bool(None, "MyB", 1);
		assert_eq!(s.get_bool(None, "MyB", -1), 1);
		assert_eq!(s.get(None, "MyB").unwrap(), "true");
		assert_eq!(s.entry_type(None, "MyB").unwrap(), EntryType::Bool);

		s.set_bool(None, "MyB2", 0);
		assert_eq!(s.get_bool(None, "MyB2", 1), 0);
		assert_eq!(s.get(None, "MyB2").unwrap(), "false");

		assert_eq!(s.get_bool(None, "MissingB", 5), 5);
		s.set(None, "MyStr3", "xyz");
		assert_eq!(s.get_bool(None, "MyStr3", 5), 5);
	}

	#[test]
	fn test_set_string_parses_into_typed_entry() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		// Create a typed int entry, then set it via the string setter: the
		// string is parsed into the declared type.
		s.set_int(None, "Parsed", 0);
		s.set(None, "Parsed", "123");
		assert_eq!(s.get_int(None, "Parsed", 0), 123);
		assert_eq!(s.entry_type(None, "Parsed").unwrap(), EntryType::Int);

		// Unparseable string leaves the typed entry unchanged.
		s.set(None, "Parsed", "abc");
		assert_eq!(s.get_int(None, "Parsed", 0), 123);

		// Bool path.
		s.set_bool(None, "ParsedB", 0);
		s.set(None, "ParsedB", "true");
		assert_eq!(s.get_bool(None, "ParsedB", -1), 1);
		s.set(None, "ParsedB", "nonsense");
		assert_eq!(s.get_bool(None, "ParsedB", -1), 1);

		// Double path.
		s.set_double(None, "ParsedD", 0.0);
		s.set(None, "ParsedD", "2.5");
		assert_eq!(s.get_double(None, "ParsedD", -1.0), 2.5);
		s.set(None, "ParsedD", "junk");
		assert_eq!(s.get_double(None, "ParsedD", -1.0), 2.5);
	}

	#[test]
	fn test_grouped_keys() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_int(Some("audio"), "sample_rate", 44100);
		assert_eq!(s.get_int(Some("audio"), "sample_rate", 0), 44100);
		assert_eq!(
			s.entry_type(Some("audio"), "sample_rate").unwrap(),
			EntryType::Int
		);

		// Empty group and None are equivalent (flat keys).
		s.set_int(Some(""), "flatkey", 7);
		assert_eq!(s.get_int(None, "flatkey", 0), 7);
		assert_eq!(s.get_int(Some(""), "flatkey", 0), 7);
	}

	// ---- TOML persistence --------------------------------------------------

	#[test]
	fn test_load_missing_file() {
		with_temp_config(|_dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			// Missing file (no config.toml, no config.ini) is not an error;
			// defaults remain.
			s.load().unwrap();
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
		});
	}

	#[test]
	fn test_save_load_roundtrip() {
		with_temp_config(|_dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			s.set(None, "CustomString", "custom value");
			// A custom double is persisted but, not being a registered key, is
			// reloaded as a string (C++ parity: only known keys keep their type).
			s.set_double(Some("render"), "gain", 1.5);
			// Modify a KNOWN typed key so its type survives a reload.
			s.set_int(None, "DefaultSequenceWidth", 640);
			s.set_bool(None, "SplitClipsCopyNodes", 0);
			s.save().unwrap();

			// Wipe back to defaults, dropping custom keys.
			s.reset_defaults().unwrap();
			assert!(matches!(s.get(None, "CustomString"), Err(Error::NotFound)));

			s.load().unwrap();
			// Flat custom string roundtrips.
			assert_eq!(s.get(None, "CustomString").unwrap(), "custom value");
			// Custom typed keys lose their type on reload -> string form.
			assert_eq!(s.get(Some("render"), "gain").unwrap(), "1.5");
			assert_eq!(s.get_double(Some("render"), "gain", -1.0), -1.0);
			// Known typed keys roundtrip with their type preserved.
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 0);

			// Defaults are still present after load.
			assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
		});
	}

	#[test]
	fn test_save_toml_format() {
		with_temp_config(|dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			s.set_int(None, "FlatKey", 1);
			s.set(None, "StrKey", "hello");
			s.set_double(None, "DblKey", 2.5);
			s.set_bool(None, "BoolKey", 1);
			s.set_int(Some("alpha"), "x", 2);
			s.set_int(Some("beta"), "y", 3);
			s.save().unwrap();

			let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
			// Native TOML types: ints/bools/floats unquoted, strings quoted.
			assert!(content.contains("FlatKey = 1"), "content: {}", content);
			assert!(content.contains("StrKey = \"hello\""), "content: {}", content);
			assert!(content.contains("DblKey = 2.5"), "content: {}", content);
			assert!(content.contains("BoolKey = true"), "content: {}", content);
			// Grouped keys land in [group] tables.
			assert!(content.contains("[alpha]"), "content: {}", content);
			assert!(content.contains("x = 2"), "content: {}", content);
			assert!(content.contains("[beta]"), "content: {}", content);
			assert!(content.contains("y = 3"), "content: {}", content);
			// Flat keys are written before any section header.
			let flat_pos = content.find("FlatKey = 1").unwrap();
			let alpha_pos = content.find("[alpha]").unwrap();
			assert!(flat_pos < alpha_pos);
			// The temp file is renamed away and does not linger.
			assert!(!dir.join("config.toml.tmp").exists());
			// save() writes TOML only; no legacy INI is produced.
			assert!(!dir.join("config.ini").exists());
		});
	}

	#[test]
	fn test_migrate_ini_parses_types_and_skips_lines() {
		with_temp_config(|dir| {
			// A legacy config.ini with no config.toml: load() reads it via the
			// migration path.
			let ini = "\
# comment
; another comment

DefaultSequenceWidth=640
UseProxyMedia=false
[section]
UnknownTypedThing=hello
";
			std::fs::write(dir.join("config.ini"), ini).unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
			assert_eq!(
				s.get(Some("section"), "UnknownTypedThing").unwrap(),
				"hello"
			);
			// The migration immediately persisted a config.toml.
			assert!(dir.join("config.toml").is_file());
		});
	}

	#[test]
	fn test_migrate_ini_unparseable_keeps_default() {
		with_temp_config(|dir| {
			std::fs::write(dir.join("config.ini"), "DefaultSequenceWidth=notanumber\n").unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();
			// Unparseable int keeps the compiled-in default.
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1920);
		});
	}

	#[test]
	fn test_migrate_ini_trims_values() {
		with_temp_config(|dir| {
			// parse_ini trims " \t\r\n" around keys and values.
			std::fs::write(dir.join("config.ini"), "  DefaultSequenceWidth = 320  \n").unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 320);
		});
	}

	#[test]
	fn test_toml_roundtrip_all_types() {
		with_temp_config(|dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			// One custom key per ConfigValue type. Known keys (present in the
			// compiled-in defaults) keep their declared type across a reload;
			// unknown keys reload as strings (C++ parity).
			s.set(None, "CustomString", "hello world");
			s.set_int(None, "DefaultSequenceWidth", 640); // known Int
			s.set_bool(None, "SplitClipsCopyNodes", 0); // known Bool
			s.set(None, "ProxyPreset", "medium"); // known String
			s.set_double(Some("render"), "gain", 1.5); // unknown Double
			s.save().unwrap();

			// Wipe back to defaults, dropping custom keys.
			s.reset_defaults().unwrap();
			assert!(matches!(s.get(None, "CustomString"), Err(Error::NotFound)));

			s.load().unwrap();
			assert_eq!(s.get(None, "CustomString").unwrap(), "hello world");
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get_bool(None, "SplitClipsCopyNodes", -1), 0);
			assert_eq!(s.get(None, "ProxyPreset").unwrap(), "medium");
			// Unknown double reloads in string form ("1.5").
			assert_eq!(s.get(Some("render"), "gain").unwrap(), "1.5");
			assert_eq!(s.get_double(Some("render"), "gain", -1.0), -1.0);
			// The file holds native TOML types, not stringified values.
			let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
			assert!(content.contains("DefaultSequenceWidth = 640"), "content: {}", content);
			assert!(content.contains("SplitClipsCopyNodes = false"), "content: {}", content);
			assert!(content.contains("ProxyPreset = \"medium\""), "content: {}", content);
			assert!(content.contains("gain = 1.5"), "content: {}", content);
		});
	}

	#[test]
	fn test_migrate_from_ini_generates_toml() {
		with_temp_config(|dir| {
			// A legacy config.ini with no config.toml: load() must read it and
			// immediately persist the values as TOML, leaving the INI in place.
			let ini = "\
DefaultSequenceWidth=640
UseProxyMedia=false
[section]
CustomKey=hello
";
			std::fs::write(dir.join("config.ini"), ini).unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();

			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
			assert_eq!(s.get(Some("section"), "CustomKey").unwrap(), "hello");

			// config.toml was generated with the migrated values; config.ini
			// is deliberately not deleted.
			assert!(dir.join("config.toml").is_file());
			assert!(dir.join("config.ini").is_file());
			let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
			assert!(content.contains("DefaultSequenceWidth = 640"), "content: {}", content);
			assert!(content.contains("UseProxyMedia = false"), "content: {}", content);
			assert!(content.contains("[section]"), "content: {}", content);
			assert!(content.contains("CustomKey = \"hello\""), "content: {}", content);

			// A second load reads the migrated TOML, not the INI.
			s.reset_defaults().unwrap();
			s.load().unwrap();
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get_bool(None, "UseProxyMedia", -1), 0);
		});
	}

	#[test]
	fn test_toml_preferred_over_ini() {
		with_temp_config(|dir| {
			// Both files present with conflicting values: config.toml wins and
			// the INI is neither consulted nor migrated.
			std::fs::write(
				dir.join("config.ini"),
				"DefaultSequenceWidth=800\nOnlyInIni=legacy\n",
			)
			.unwrap();
			std::fs::write(
				dir.join("config.toml"),
				"DefaultSequenceWidth = 640\nOnlyInToml = \"modern\"\n",
			)
			.unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 640);
			assert_eq!(s.get(None, "OnlyInToml").unwrap(), "modern");
			assert!(matches!(s.get(None, "OnlyInIni"), Err(Error::NotFound)));
		});
	}

	// ---- Error paths ----------------------------------------------------

	static REPORTED: StdMutex<Vec<(String, String)>> = StdMutex::new(Vec::new());

	extern "C" fn record_handler(
		title: *const c_char,
		message: *const c_char,
		_userdata: *mut c_void,
	) {
		unsafe {
			let title = std::ffi::CStr::from_ptr(title)
				.to_string_lossy()
				.into_owned();
			let message = std::ffi::CStr::from_ptr(message)
				.to_string_lossy()
				.into_owned();
			REPORTED.lock().unwrap().push((title, message));
		}
	}

	#[test]
	fn test_load_directory_returns_failed_and_reports() {
		with_temp_config(|dir| {
			// A directory at the config path is not a readable config. With no
			// config.toml present, load() hits the legacy INI fallback, which
			// treats a directory at config.ini the same way.
			std::fs::create_dir(dir.join("config.ini")).unwrap();
			REPORTED.lock().unwrap().clear();
			let s = ConfigStore::instance();
			s.set_error_handler(Some(record_handler), std::ptr::null_mut())
				.unwrap();
			let res = s.load();
			s.set_error_handler(None, std::ptr::null_mut()).unwrap();
			assert!(res.is_err());
			assert_eq!(res.unwrap_err().code(), crate::error::OAKCORE_E_FAILED);

			let reported = REPORTED.lock().unwrap().clone();
			assert_eq!(reported.len(), 1);
			assert_eq!(reported[0].0, "Error loading settings");
			assert!(reported[0]
				.1
				.contains("Failed to load application settings"));
		});
	}

	#[test]
	fn test_error_handler_none_clears() {
		with_temp_config(|dir| {
			std::fs::create_dir(dir.join("config.ini")).unwrap();
			let s = ConfigStore::instance();
			s.set_error_handler(Some(record_handler), std::ptr::null_mut())
				.unwrap();
			s.set_error_handler(None, std::ptr::null_mut()).unwrap();
			REPORTED.lock().unwrap().clear();
			let res = s.load();
			// With no handler, the error goes to stderr instead of the callback.
			assert!(res.is_err());
			assert!(REPORTED.lock().unwrap().is_empty());
		});
	}

	// ---- Pure logic: value conversion -------------------------------------

	#[test]
	fn test_value_to_string_all_types() {
		assert_eq!(value_to_string(&ConfigValue::String("hi".into())), "hi");
		assert_eq!(value_to_string(&ConfigValue::Int(-42)), "-42");
		assert_eq!(
			value_to_string(&ConfigValue::Int(i64::MAX)),
			"9223372036854775807"
		);
		assert_eq!(value_to_string(&ConfigValue::Double(2.5)), "2.5");
		assert_eq!(value_to_string(&ConfigValue::Double(0.0)), "0");
		assert_eq!(value_to_string(&ConfigValue::Bool(true)), "true");
		assert_eq!(value_to_string(&ConfigValue::Bool(false)), "false");
	}

	#[test]
	fn test_to_entry_type() {
		assert_eq!(
			to_entry_type(&ConfigValue::String(String::new())),
			EntryType::String
		);
		assert_eq!(to_entry_type(&ConfigValue::Int(0)), EntryType::Int);
		assert_eq!(to_entry_type(&ConfigValue::Double(0.0)), EntryType::Double);
		assert_eq!(to_entry_type(&ConfigValue::Bool(false)), EntryType::Bool);
	}

	#[test]
	fn test_string_to_value_int() {
		// Mirrors std::stoll: leading whitespace is skipped, the rest must
		// parse fully (`configstore.cpp:183-194`).
		assert_eq!(
			string_to_value("42", EntryType::Int),
			Some(ConfigValue::Int(42))
		);
		assert_eq!(
			string_to_value("  42", EntryType::Int),
			Some(ConfigValue::Int(42))
		);
		assert_eq!(
			string_to_value("-7", EntryType::Int),
			Some(ConfigValue::Int(-7))
		);
		assert_eq!(
			string_to_value("+7", EntryType::Int),
			Some(ConfigValue::Int(7))
		);
		// Trailing junk / whitespace fails (std::stoll pos != size).
		assert!(string_to_value("42 ", EntryType::Int).is_none());
		assert!(string_to_value("42x", EntryType::Int).is_none());
		assert!(string_to_value("0x10", EntryType::Int).is_none());
		assert!(string_to_value("", EntryType::Int).is_none());
		assert!(string_to_value("99999999999999999999999", EntryType::Int).is_none());
	}

	#[test]
	fn test_string_to_value_double() {
		assert_eq!(
			string_to_value("2.5", EntryType::Double),
			Some(ConfigValue::Double(2.5))
		);
		assert_eq!(
			string_to_value(" 2.5", EntryType::Double),
			Some(ConfigValue::Double(2.5))
		);
		assert_eq!(
			string_to_value("1e3", EntryType::Double),
			Some(ConfigValue::Double(1000.0))
		);
		// std::stod accepts inf/infinity/nan (case-insensitive).
		assert_eq!(
			string_to_value("inf", EntryType::Double),
			Some(ConfigValue::Double(f64::INFINITY))
		);
		assert_eq!(
			string_to_value("-Infinity", EntryType::Double),
			Some(ConfigValue::Double(f64::NEG_INFINITY))
		);
		match string_to_value("NaN", EntryType::Double) {
			Some(ConfigValue::Double(d)) => assert!(d.is_nan()),
			other => panic!("expected NaN double, got {:?}", other),
		}
		// Trailing junk fails.
		assert!(string_to_value("2.5 ", EntryType::Double).is_none());
		assert!(string_to_value("2.5x", EntryType::Double).is_none());
		assert!(string_to_value("", EntryType::Double).is_none());
	}

	#[test]
	fn test_string_to_value_bool_and_misc() {
		assert_eq!(
			string_to_value("true", EntryType::Bool),
			Some(ConfigValue::Bool(true))
		);
		assert_eq!(
			string_to_value("1", EntryType::Bool),
			Some(ConfigValue::Bool(true))
		);
		assert_eq!(
			string_to_value("false", EntryType::Bool),
			Some(ConfigValue::Bool(false))
		);
		assert_eq!(
			string_to_value("0", EntryType::Bool),
			Some(ConfigValue::Bool(false))
		);
		// Case-sensitive in the C++ ("TRUE" does not parse).
		assert!(string_to_value("TRUE", EntryType::Bool).is_none());
		assert!(string_to_value("yes", EntryType::Bool).is_none());
		assert!(string_to_value("2", EntryType::Bool).is_none());

		// Strings always parse; None never does.
		assert_eq!(
			string_to_value("anything", EntryType::String),
			Some(ConfigValue::String("anything".into()))
		);
		assert!(string_to_value("anything", EntryType::None).is_none());
	}

	#[test]
	fn test_trim() {
		assert_eq!(trim("  hello \t\r\n"), "hello");
		assert_eq!(trim("no-space"), "no-space");
		assert_eq!(trim("   "), "");
		assert_eq!(trim(""), "");
		assert_eq!(trim(" a b "), "a b");
	}

	#[test]
	fn test_format_g_edge_cases() {
		// %g switches to scientific at exponent >= 6 or < -4.
		assert_eq!(format_g(100000.0), "100000");
		assert_eq!(format_g(1000000.0), "1e+06");
		assert_eq!(format_g(0.0001), "0.0001");
		assert_eq!(format_g(0.00001), "1e-05");
		// Trailing zeros dropped, integer digits never truncated.
		assert_eq!(format_g(1.0), "1");
		assert_eq!(format_g(1.20), "1.2");
		assert_eq!(format_g(-0.00012345), "-0.00012345");
		assert_eq!(format_g(6.5e100), "6.5e+100");
	}

	// ---- Empty-key / fallback semantics -----------------------------------

	#[test]
	fn test_empty_key_is_rejected_everywhere() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		// Setters with an empty key are silent no-ops.
		s.set(None, "", "v");
		s.set(Some("g"), "", "v");
		s.set_int(None, "", 1);
		s.set_int64(None, "", 1);
		s.set_double(None, "", 1.0);
		s.set_bool(None, "", 1);
		assert!(matches!(s.get(None, ""), Err(Error::Invalid)));
		assert!(matches!(s.get(Some("g"), ""), Err(Error::Invalid)));
		assert_eq!(s.get_int(None, "", 7), 7);
		assert_eq!(s.get_int64(None, "", 7), 7);
		assert_eq!(s.get_double(None, "", 7.0), 7.0);
		assert_eq!(s.get_bool(None, "", 7), 7);
		assert!(matches!(s.entry_type(None, ""), Err(Error::Invalid)));
	}

	#[test]
	fn test_typed_getters_cross_type_fallbacks() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_int(None, "OnlyInt", 5);
		// Every other typed getter falls back on the int entry.
		assert_eq!(s.get_double(None, "OnlyInt", 1.5), 1.5);
		assert_eq!(s.get_bool(None, "OnlyInt", 3), 3);
		// ...but the string getter formats any type.
		assert_eq!(s.get(None, "OnlyInt").unwrap(), "5");
		s.set_bool(None, "OnlyBool", 1);
		assert_eq!(s.get_int(None, "OnlyBool", 9), 9);
		assert_eq!(s.get_double(None, "OnlyBool", 9.0), 9.0);
		assert_eq!(s.get(None, "OnlyBool").unwrap(), "true");
	}

	#[test]
	fn test_unicode_keys_and_values() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set(Some("日本語グループ"), "キー", "値🎬");
		assert_eq!(s.get(Some("日本語グループ"), "キー").unwrap(), "値🎬");
		assert_eq!(
			s.entry_type(Some("日本語グループ"), "キー").unwrap(),
			EntryType::String
		);
		// A group containing '/' nests (join is just string concatenation),
		// so ("a/b", "c") and ("a", "b/c") address the SAME entry.
		s.set_int(Some("a/b"), "c", 5);
		assert_eq!(s.get_int(Some("a/b"), "c", 0), 5);
		assert_eq!(s.get_int(Some("a"), "b/c", 0), 5);
	}

	// ---- TOML byte-level format --------------------------------------------

	#[test]
	fn test_save_toml_contents_all_defaults() {
		with_temp_config(|dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			s.save().unwrap();

			let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
			// Re-parse the saved file with the toml crate itself: it must be
			// valid TOML carrying every registered default with its native type.
			let parsed: toml::Value = toml::from_str(&content).expect("saved config.toml parses");
			let table = parsed.as_table().unwrap();
			assert_eq!(table["DefaultSequenceWidth"].as_integer(), Some(1920));
			assert_eq!(table["DefaultSequenceHeight"].as_integer(), Some(1080));
			assert_eq!(table["ProxyPreset"].as_str(), Some("veryfast"));
			assert_eq!(table["SplitClipsCopyNodes"].as_bool(), Some(true));
			assert_eq!(table["UseGLFinish"].as_bool(), Some(false));
			assert_eq!(table["CatColor3"].as_integer(), Some(3));
			assert!(table.contains_key("DiskCacheSaveInterval"));
			assert!(table.contains_key("LUTLibraryPaths"));
			// No group defaults exist; every top-level key is a plain value.
			assert!(table.values().all(|v| !v.is_table()));
			// Serialized output must not contain INI-style "k=v" lines.
			assert!(!content.contains("DefaultSequenceWidth=1920"), "content: {}", content);
		});
	}

	#[test]
	fn test_nested_group_roundtrip() {
		with_temp_config(|dir| {
			let s = ConfigStore::instance();
			s.reset_defaults().unwrap();
			// Group = everything before the LAST '/', so "a/b" + "c" lands in
			// a `["a/b"]` table (the '/' forces a quoted TOML key).
			s.set_int(Some("a/b"), "c", 5);
			s.save().unwrap();
			let content = std::fs::read_to_string(dir.join("config.toml")).unwrap();
			assert!(content.contains("[\"a/b\"]"), "content: {}", content);
			assert!(content.contains("c = 5"), "content: {}", content);

			s.reset_defaults().unwrap();
			s.load().unwrap();
			// Reloaded as an unknown key -> string, addressed as "a/b/c".
			assert_eq!(s.get(Some("a/b"), "c").unwrap(), "5");
			assert_eq!(s.get_int(Some("a/b"), "c", -1), -1);
		});
	}

	// ---- Merge order: defaults < loaded file < runtime sets -----------------

	#[test]
	fn test_merge_order_defaults_file_runtime() {
		with_temp_config(|dir| {
			std::fs::write(
				dir.join("config.ini"),
				"DefaultSequenceWidth=800\nCustomFromFile=yes\n",
			)
			.unwrap();
			let s = ConfigStore::instance();

			// A runtime set made BEFORE load() is wiped: load() resets to
			// defaults first (`configstore.cpp:241`).
			s.reset_defaults().unwrap();
			s.set_int(None, "DefaultSequenceWidth", 640);
			s.set(None, "RuntimeOnly", "r");
			s.load().unwrap();
			// File value wins over both defaults and the pre-load runtime set.
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 800);
			assert!(matches!(s.get(None, "RuntimeOnly"), Err(Error::NotFound)));
			assert_eq!(s.get(None, "CustomFromFile").unwrap(), "yes");

			// A runtime set AFTER load() wins over the file.
			s.set_int(None, "DefaultSequenceWidth", 1024);
			assert_eq!(s.get_int(None, "DefaultSequenceWidth", -1), 1024);
			// Keys not mentioned in the file keep their defaults.
			assert_eq!(s.get_int(None, "DefaultSequenceHeight", -1), 1080);
		});
	}

	// ---- More load parsing edge cases ---------------------------------------

	#[test]
	fn test_load_lax_parsing_matrix() {
		with_temp_config(|dir| {
			// Hand-built INI exercising the C++ load() branches
			// (`configstore.cpp:268-312`).
			let ini = "\
BareLineWithoutEquals
=EmptyKeySkipped
[g]
KeyWithEquals=a=b
[]
FlatAfterEmptySection=ok
";
			std::fs::write(dir.join("config.ini"), ini).unwrap();
			let s = ConfigStore::instance();
			s.load().unwrap();
			// Malformed lines are skipped, not errors.
			assert!(matches!(
				s.get(None, "BareLineWithoutEquals"),
				Err(Error::NotFound)
			));
			// Value keeps everything after the FIRST '='.
			assert_eq!(s.get(Some("g"), "KeyWithEquals").unwrap(), "a=b");
			// "[]" empties the group, so the key is flat.
			assert_eq!(s.get(None, "FlatAfterEmptySection").unwrap(), "ok");
			assert!(matches!(s.get(Some(""), "FlatAfterEmptySection"), Ok(_)));
		});
	}

	#[test]
	fn test_load_invalid_utf8_reports_error() {
		with_temp_config(|dir| {
			// Invalid UTF-8: the C++ reads raw bytes and would muddle
			// through; the Rust port maps this to the same "unreadable file"
			// failure as a directory (documented CPP-PARITY note in load()).
			std::fs::write(dir.join("config.ini"), b"DefaultSequenceWidth=\xff\xfe\n").unwrap();
			REPORTED.lock().unwrap().clear();
			let s = ConfigStore::instance();
			s.set_error_handler(Some(record_handler), std::ptr::null_mut())
				.unwrap();
			let res = s.load();
			s.set_error_handler(None, std::ptr::null_mut()).unwrap();
			assert!(res.is_err());
			assert_eq!(res.unwrap_err().code(), crate::error::OAKCORE_E_FAILED);
			assert_eq!(REPORTED.lock().unwrap().len(), 1);
		});
	}

	#[test]
	fn test_load_invalid_toml_reports_error() {
		with_temp_config(|dir| {
			// A corrupt config.toml must be reported and surfaced as an error,
			// never silently ignored (which would lose the user's settings on
			// the next save).
			std::fs::write(dir.join("config.toml"), "DefaultSequenceWidth = =\n").unwrap();
			REPORTED.lock().unwrap().clear();
			let s = ConfigStore::instance();
			s.set_error_handler(Some(record_handler), std::ptr::null_mut())
				.unwrap();
			let res = s.load();
			s.set_error_handler(None, std::ptr::null_mut()).unwrap();
			assert!(res.is_err());
			assert_eq!(res.unwrap_err().code(), crate::error::OAKCORE_E_FAILED);
			let reported = REPORTED.lock().unwrap().clone();
			assert_eq!(reported.len(), 1);
			assert_eq!(reported[0].0, "Error loading settings");
			assert!(reported[0]
				.1
				.contains("Failed to load application settings"));
		});
	}

	// ---- Save error path -----------------------------------------------------

	#[test]
	fn test_save_failure_reports_error() {
		let _g = test_lock().lock().unwrap();
		let dir =
			std::env::temp_dir().join(format!("oak_core_configstore_test_{}", std::process::id()));
		let _ = std::fs::create_dir_all(&dir);
		// Point OAK_CONFIG_DIR at a regular FILE so writing
		// "<dir>/config.toml.tmp" fails (create_dir_all on it is a silent
		// no-op failure, exactly like the C++ ec-swallowing).
		let blocker = dir.join("not_a_dir");
		std::fs::write(&blocker, b"x").unwrap();
		std::env::set_var("OAK_CONFIG_DIR", &blocker);

		REPORTED.lock().unwrap().clear();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		s.set_error_handler(Some(record_handler), std::ptr::null_mut())
			.unwrap();
		let res = s.save();
		s.set_error_handler(None, std::ptr::null_mut()).unwrap();

		std::env::remove_var("OAK_CONFIG_DIR");
		let _ = std::fs::remove_dir_all(&dir);

		assert!(res.is_err());
		assert_eq!(res.unwrap_err().code(), crate::error::OAKCORE_E_FAILED);
		let reported = REPORTED.lock().unwrap().clone();
		assert_eq!(reported.len(), 1);
		assert_eq!(reported[0].0, "Error saving settings");
	}

	// ---- Error handler userdata ----------------------------------------------

	static USERDATA_HITS: StdMutex<Vec<usize>> = StdMutex::new(Vec::new());

	extern "C" fn userdata_handler(
		_title: *const c_char,
		_message: *const c_char,
		userdata: *mut c_void,
	) {
		USERDATA_HITS.lock().unwrap().push(userdata as usize);
	}

	#[test]
	fn test_error_handler_receives_userdata() {
		with_temp_config(|dir| {
			std::fs::create_dir(dir.join("config.ini")).unwrap();
			USERDATA_HITS.lock().unwrap().clear();
			let s = ConfigStore::instance();
			s.set_error_handler(Some(userdata_handler), 0xDEADusize as *mut c_void)
				.unwrap();
			let _ = s.load();
			s.set_error_handler(None, std::ptr::null_mut()).unwrap();
			assert_eq!(USERDATA_HITS.lock().unwrap().as_slice(), &[0xDEADusize]);
		});
	}

	// ---- Thread-safety smoke test ---------------------------------------------

	#[test]
	fn test_singleton_thread_safety_smoke() {
		let _g = test_lock().lock().unwrap();
		let s = ConfigStore::instance();
		s.reset_defaults().unwrap();
		let mut handles = Vec::new();
		for t in 0..8 {
			handles.push(std::thread::spawn(move || {
				let store = ConfigStore::instance();
				for i in 0..50 {
					let key = format!("thread{}/key{}", t, i % 5);
					store.set_int(None, &key, i);
					let _ = store.get_int(None, &key, -1);
					let _ = store.get(None, &key);
					let _ = store.entry_type(None, &key);
				}
			}));
		}
		for h in handles {
			h.join().unwrap();
		}
		// Same instance across threads, and the last writer won.
		assert!(s.get_int(None, "thread3/key4", -1) >= 0);
		s.reset_defaults().unwrap();
	}
}
