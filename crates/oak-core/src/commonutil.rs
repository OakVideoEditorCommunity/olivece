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

//! Config / file-function helpers — thin wrappers over this crate's
//! `configstore` and `filefunctions` (the former `bridge/common.rs`,
//! moved here from oak-render with the backend/color merge).

use std::sync::Mutex;

/// Read a config string via the domain store
/// (`ConfigStore::get(group, key)`); `None` when missing or empty.
pub fn config_get_string(group: Option<&str>, key: &str) -> Option<String> {
	crate::configstore::ConfigStore::instance()
		.get(group, key)
		.ok()
		.filter(|s| !s.is_empty())
}

/// `oak_core_config_get_int(group, key, default)`.
pub fn config_get_int(group: Option<&str>, key: &str, default: i32) -> i32 {
	crate::configstore::ConfigStore::instance().get_int(group, key, default)
}

/// The configuration directory — `filefunctions`' implementation
/// (`FileFunctions::get_configuration_location`, honoring `OAK_CONFIG_DIR`
/// and the platform fallbacks).
pub fn configuration_location() -> String {
	crate::filefunctions::FileFunctions::new()
		.get_configuration_location()
		.unwrap_or_default()
}

/// Serializes tests that mutate `OAK_CONFIG_DIR` / `OAK_RENDER_BACKEND`
/// (env is process-global; the manager tests share this lock too).
pub static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

/// The default disk cache directory (C++ `DiskManager::
/// get_default_disk_cache_path`): `<configuration_location>/mediacache`.
pub fn default_disk_cache_path() -> String {
	crate::filefunctions::default_disk_cache_path()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn config_missing_key_falls_back() {
		// The real config store is empty under cargo test: defaults apply.
		assert_eq!(config_get_int(None, "GraphicsBackend", 7), 7);
		assert_eq!(config_get_string(None, "missing"), None);
	}

	#[test]
	fn configuration_location_uses_env_override() {
		let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let dir = std::env::temp_dir().join("oakrender-config-test");
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let loc = configuration_location();
		assert_eq!(loc, dir.to_string_lossy());
		std::env::remove_var("OAK_CONFIG_DIR");
	}

	#[test]
	fn default_disk_cache_path_is_under_config() {
		let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let dir = std::env::temp_dir().join("oakrender-cache-test");
		std::env::set_var("OAK_CONFIG_DIR", &dir);
		let p = default_disk_cache_path();
		assert!(p.ends_with("/mediacache") || p.ends_with("\\mediacache"));
		std::env::remove_var("OAK_CONFIG_DIR");
	}
}
