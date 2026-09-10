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

//! Localization: UI strings live in YAML language packs discovered at
//! runtime (one `<lang>.yaml` per language — the file stem IS the
//! language code, e.g. `en-US.yaml`, `zh-CN.yaml`). Nothing is compiled in: adding a language means
//! dropping one more `.yaml` into any searched directory — no rebuild,
//! no code change. Each pack carries its own display name in the
//! `language.name` key (the endonym, e.g. "Français"), which the
//! language pickers show.
//!
//! # Lookup order
//!
//! 1. The user pack directory (`~/.oak/i18n/`);
//! 2. The bundled packs (next to the executable, the app bundle's
//!    `Resources/i18n/`, or the system install's `<exe>/../share/oak/i18n/`);
//! 3. The dev checkout's `assets/i18n/` (running from the repo).
//!
//! Later sources override earlier ones per key. `tr` falls back from
//! the active language to `en-US` and then to the key itself, so a
//! missing key can never panic — it degrades to a visible key string.
//!
//! # The language setting
//!
//! The active language code is a process-global string (behind an
//! `RwLock` — any thread can read it). At startup [`init`] loads the
//! persisted value from the config key `Language`; unknown codes fall
//! back to `en-US`. [`set_language_code`] flips the global and persists.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// The fallback language code: the reference pack every other pack is
/// key-compared against, the config default, and `tr`'s last resort
/// before the raw key.
pub const FALLBACK_CODE: &str = "en-US";

/// One language's key → value table (values are leaked once at load —
/// the tables live for the whole process).
type Table = HashMap<String, &'static str>;

/// All discovered packs: language code → table.
static TABLES: OnceLock<RwLock<HashMap<String, Table>>> = OnceLock::new();

/// The active language code. Empty means "not yet initialized" and reads
/// as [`FALLBACK_CODE`].
static CURRENT_CODE: RwLock<String> = RwLock::new(String::new());

/// The active language code (a discovered pack's code, or `en-US`).
pub fn language_code() -> String {
	let code = CURRENT_CODE.read().unwrap_or_else(|e| e.into_inner());
	if code.is_empty() {
		FALLBACK_CODE.to_string()
	} else {
		code.clone()
	}
}

/// Switches the active language to any discovered pack's code and
/// persists the choice. Unknown codes fall back to en-US.
pub fn set_language_code(code: &str) {
	let exists = tables()
		.read()
		.unwrap_or_else(|e| e.into_inner())
		.contains_key(code);
	let code = if exists { code } else { FALLBACK_CODE };
	*CURRENT_CODE.write().unwrap_or_else(|e| e.into_inner()) = code.to_string();
	persist_language(code);
	sync_widgets();
}

/// A pack's own display name — its `language.name` value (the endonym,
/// e.g. "Français") — which the language pickers show, so a newly
/// dropped-in pack needs no code change. Falls back to the code.
pub fn pack_native_name(code: &str) -> String {
	tables()
		.read()
		.unwrap_or_else(|e| e.into_inner())
		.get(code)
		.and_then(|t| t.get("language.name"))
		.map(|s| s.to_string())
		.unwrap_or_else(|| code.to_string())
}

/// The language codes with a discovered pack: en-US first, zh-CN
/// second, the rest alphabetically by code.
pub fn available_languages() -> Vec<String> {
	let tables = tables().read().unwrap_or_else(|e| e.into_inner());
	let mut codes: Vec<String> = tables.keys().cloned().collect();
	codes.sort();
	let mut out = Vec::with_capacity(codes.len());
	for preferred in [FALLBACK_CODE, "zh-CN"] {
		if let Some(pos) = codes.iter().position(|c| c == preferred) {
			out.push(codes.remove(pos));
		}
	}
	out.extend(codes);
	out
}

/// Loads the persisted language from the config. Called once at startup.
/// Never fails: a missing key keeps the default (en-US).
pub fn init() {
	let code = crate::oakui::real::config_get_string("Language");
	if code.is_empty() {
		sync_widgets();
		return;
	}
	set_language_code(code.trim());
}

/// Writes the code back to the config `Language` key.
fn persist_language(code: &str) {
	crate::oakui::real::config_set_string("Language", code);
}

/// Translates `key` in the active language. Never panics: unknown keys
/// fall back to en-US and then to the key itself.
pub fn tr(key: &'static str) -> &'static str {
	let tables = tables().read().unwrap_or_else(|e| e.into_inner());
	let current = language_code();
	if let Some(value) = tables.get(&current).and_then(|t| t.get(key)) {
		return value;
	}
	if let Some(value) = tables.get(FALLBACK_CODE).and_then(|t| t.get(key)) {
		return value;
	}
	key
}

/// Translates a key built at runtime — the built-in effect and parameter
/// names, whose keys are `node.<type-id suffix>.name` and
/// `node.<type-id suffix>.input.<input id>` (see
/// [`oakui::effectchain::addable_effects`](crate::oakui::effectchain::addable_effects)).
///
/// Same lookup as [`tr`] — the active language, then `en-US` — with the
/// node layer's own English name as the last resort: `tr` takes
/// `&'static str` keys and returns the key itself on a miss, which for a
/// formatted key would put a raw `node.…` identifier in the UI.
pub fn tr_or(key: &str, fallback: &str) -> String {
	let tables = tables().read().unwrap_or_else(|e| e.into_inner());
	let current = language_code();
	if let Some(value) = tables.get(&current).and_then(|t| t.get(key)) {
		return value.to_string();
	}
	if let Some(value) = tables.get(FALLBACK_CODE).and_then(|t| t.get(key)) {
		return value.to_string();
	}
	fallback.to_string()
}

/// The keys the gpui widget crates localize through
/// `gpui_widgets::i18n` (viewer transport labels, effect-stack empty state).
pub const WIDGET_KEYS: &[&str] = &[
	"viewer.safe_frames",
	"viewer.zoom",
	"viewer.no_frame_source",
	"viewer.in_point",
	"viewer.step_back",
	"viewer.play",
	"viewer.pause",
	"viewer.step_forward",
	"viewer.out_point",
	"viewer.clear_range",
	"effect_stack.empty",
	"effect_stack.add",
	"explorer.tree",
	"explorer.icons",
];

/// Installs the active language's widget strings into the
/// `gpui_widgets::i18n` string-table hook.
pub fn sync_widgets() {
	let mut table = gpui_widgets::i18n::StringTable::new();
	for key in WIDGET_KEYS {
		table.insert((*key).to_string(), tr(key).to_string());
	}
	gpui_widgets::i18n::set_table(table);
}

// ---------------------------------------------------------------------------
// Pack loading
// ---------------------------------------------------------------------------

/// The loaded packs (lazily parsed on first use).
fn tables() -> &'static RwLock<HashMap<String, Table>> {
	TABLES.get_or_init(|| RwLock::new(load_packs()))
}

/// The directories searched for packs, lowest precedence first; each
/// directory's packs override the previous source per key.
fn pack_dirs() -> Vec<std::path::PathBuf> {
	let mut dirs = Vec::new();
	// Dev checkout: the repo-root assets/i18n (the app crate now lives in
	// crates/oak-app, so CARGO_MANIFEST_DIR is no longer the repo root),
	// plus a CWD-relative fallback for running the binary from the root.
	dirs.push(
		std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("../../assets/i18n"),
	);
	dirs.push(std::path::PathBuf::from("assets/i18n"));
	if let Ok(exe) = std::env::current_exe() {
		if let Some(dir) = exe.parent() {
			// Portable layout: <exe>/i18n.
			dirs.push(dir.join("i18n"));
			if let Some(contents) = dir.parent() {
				// macOS bundle: <exe>/../Resources/i18n.
				dirs.push(contents.join("Resources/i18n"));
				// System install (deb/rpm/pkg): <exe>/../share/oak/i18n.
				dirs.push(contents.join("share/oak/i18n"));
			}
		}
	}
	// User language packs (add or override languages without a rebuild).
	if let Some(home) = std::env::var_os("HOME") {
		dirs.push(std::path::Path::new(&home).join(".oak/i18n"));
	}
	dirs
}

/// Parses one YAML pack into a table (duplicate keys are an error).
fn parse_pack(code: &str, source: &str) -> Table {
	let mut table = Table::new();
	let parsed: HashMap<String, String> = match serde_yaml::from_str(source) {
		Ok(map) => map,
		Err(err) => {
			eprintln!("[i18n] failed to parse the {code} pack: {err}");
			return table;
		}
	};
	for (key, value) in parsed {
		let leaked: &'static str = Box::leak(value.into_boxed_str());
		table.insert(key, leaked);
	}
	table
}

/// Loads every pack file found in [`pack_dirs`] (later sources override
/// earlier ones per key; a file named `<code>.yaml` registers or extends
/// language `<code>`).
fn load_packs() -> HashMap<String, Table> {
	let mut packs: HashMap<String, Table> = HashMap::new();
	for dir in pack_dirs() {
		let Ok(read_dir) = std::fs::read_dir(&dir) else {
			continue;
		};
		let mut files: Vec<_> = read_dir
			.filter_map(|entry| entry.ok())
			.map(|entry| entry.path())
			.filter(|path| {
				path.extension().and_then(|e| e.to_str()) == Some("yaml")
					|| path.extension().and_then(|e| e.to_str()) == Some("yml")
			})
			.collect();
		files.sort();
		for path in files {
			let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
				continue;
			};
			let Ok(source) = std::fs::read_to_string(&path) else {
				continue;
			};
			let table = parse_pack(code, &source);
			packs
				.entry(code.to_string())
				.or_default()
				.extend(table);
		}
	}
	if packs.is_empty() {
		eprintln!(
			"[i18n] no language packs found in {:?}; UI strings degrade to raw keys",
			pack_dirs()
		);
	}
	packs
}

/// Reloads every pack (tests).
#[cfg(test)]
fn reload_for_test() {
	let mut tables = tables().write().unwrap_or_else(|e| e.into_inner());
	*tables = load_packs();
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every shipped pack (the YAML files in `assets/i18n/`) parses and
	/// covers exactly the en-US key set, and carries its `language.name`
	/// display name. User packs from `$HOME/.oak/i18n` are deliberately
	/// not held to the key set (a partial override pack is legitimate).
	#[test]
	fn every_shipped_pack_matches_en_keys() {
		let tables = tables().read().unwrap_or_else(|e| e.into_inner());
		let en = tables.get("en-US").expect("en-US pack");
		assert!(!en.is_empty(), "en-US pack parses to a non-empty table");
		for entry in std::fs::read_dir(
		std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/i18n"),
	)
	.expect("dev checkout packs")
{
			let path = entry.unwrap().path();
			if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
				continue;
			}
			let code = path.file_stem().unwrap().to_str().unwrap().to_string();
			let table = tables
				.get(&code)
				.unwrap_or_else(|| panic!("{code} pack"));
			assert!(!table.is_empty(), "{code} pack parses to a non-empty table");
			assert!(
				table.contains_key("language.name"),
				"{code} pack has no language.name display name"
			);
			for key in en.keys() {
				assert!(table.contains_key(key), "key {key} missing from the {code} pack");
			}
			for key in table.keys() {
				assert!(en.contains_key(key), "key {key} missing from the en-US pack");
			}
		}
	}

	/// No value is left as the raw key, and the two languages differ
	/// except for an explicit shared list (brand names, units).
	#[test]
	fn no_untranslated_values() {
		let shared = [
			"Olive Dark",
			"Olive Light",
			"English",
			"简体中文",
			"en-US",
			"zh-CN",
			"OpenGL",
			"Metal",
			"Vulkan",
			"MP4",
			"OpenFX",
			"RGB",
			"HSV",
			"1/2",
			"1/4",
			"1/8",
			// Standard colorimetry names — identical across languages.
			"sRGB / Rec.709",
			"Display P3",
			"Rec.2020",
			"sRGB",
			"Gamma 2.2",
			"PQ (ST 2084)",
			"HLG",
		];
		let tables = tables().read().unwrap_or_else(|e| e.into_inner());
		let en = tables.get("en-US").unwrap();
		let zh = tables.get("zh-CN").unwrap();
		for (key, en_value) in en {
			assert_ne!(*en_value, key.as_str(), "en-US value for {key} is the raw key");
			let zh_value = zh.get(key).unwrap();
			assert_ne!(*zh_value, key.as_str(), "zh-CN value for {key} is the raw key");
			if en_value == zh_value && !shared.contains(en_value) {
				panic!("key {key} has identical en-US and zh-CN values ({en_value:?})");
			}
		}
	}

	/// Every built-in node type the factory registers carries a
	/// `node.<type-id suffix>.name` entry in the reference pack *and* in
	/// the translation: the effect library and the inspector build that key
	/// at runtime, so a node without an entry would show its English name
	/// in every language — and one whose zh value repeats the English name
	/// was never translated at all. The matching parameter keys are held by
	/// `effectchain`'s own test against the live parameter list.
	#[test]
	fn every_builtin_node_name_is_translated() {
		let tables = tables().read().unwrap_or_else(|e| e.into_inner());
		let en = tables.get("en-US").expect("en-US pack");
		let zh = tables.get("zh-CN").expect("zh-CN pack");
		let mut nodes = 0usize;
		for meta in oak_node::factory::Factory::global().entries() {
			let suffix = meta.type_id.rsplit('.').next().unwrap_or(meta.type_id);
			let key = format!("node.{suffix}.name");
			let en_value = en
				.get(&key)
				.unwrap_or_else(|| panic!("{key} missing from the en-US pack"));
			assert_eq!(
				*en_value, meta.name,
				"{key} does not carry the node's English name"
			);
			let zh_value = zh
				.get(&key)
				.unwrap_or_else(|| panic!("{key} missing from the zh-CN pack"));
			assert_ne!(zh_value, en_value, "{key} is not translated ({en_value:?})");
			nodes += 1;
		}
		assert!(
			nodes >= 60,
			"the factory registers the built-in node types ({nodes})"
		);
	}

	/// `tr` falls back from the active language to English and then to
	/// the key itself.
	#[test]
	fn tr_falls_back_to_english_then_the_key() {
		let _guard = crate::i18n::test_lock();
		set_language_code("zh-CN");
		assert_eq!(tr("menu.file"), "文件(F)");
		set_language_code("en-US");
		assert_eq!(tr("menu.file"), "File(F)");
		assert_eq!(tr("no.such.key"), "no.such.key");
	}

	/// A custom pack on disk overrides built-in values and registers a
	/// new language. The active language is pinned to en-US: the user
	/// pack overrides the built-in en-US table, so the assertions below
	/// must not inherit another test's active language.
	#[test]
	fn user_pack_overrides_and_extends() {
		let _guard = crate::i18n::test_lock();
		set_language_code("en-US");
		let dir = std::env::temp_dir().join(format!("oak_i18n_pack_{}", std::process::id()));
		let home = dir.join("home");
		let pack_dir = home.join(".oak/i18n");
		std::fs::create_dir_all(&pack_dir).unwrap();
		std::fs::write(pack_dir.join("pirate.yaml"), "\"menu.file\": \"Arrr\"\n").unwrap();
		std::fs::write(pack_dir.join("en-US.yaml"), "\"menu.file\": \"Yarr File\"\n").unwrap();
		unsafe { std::env::set_var("HOME", &home) };
		reload_for_test();
		assert_eq!(tr("menu.file"), "Yarr File", "the user pack overrides the built-in");
		assert!(available_languages().iter().any(|c| c == "pirate"));
		set_language_code("pirate");
		assert_eq!(tr("menu.file"), "Arrr");
		set_language_code("zh-CN");
		unsafe { std::env::remove_var("HOME") };
		reload_for_test();
		let _ = std::fs::remove_dir_all(&dir);
		// Leave the process language at the default for the next test.
		set_language_code("en-US");
	}
}

/// Serializes tests that mutate the language global / HOME.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
	lang_test_lock().lock().unwrap_or_else(|e| e.into_inner())
}

/// Compatibility alias used by the app/actions test modules. Both entry
/// points MUST share the one static mutex: with two separate mutexes the
/// i18n tests and the app/actions tests do not exclude each other, and
/// the language global races (observed as a Windows-only CI failure
/// where `tr` returned the English value mid-test).
#[cfg(test)]
pub(crate) fn lang_test_lock() -> &'static std::sync::Mutex<()> {
	static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
	&LOCK
}
