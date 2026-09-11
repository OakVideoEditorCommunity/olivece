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

//! The `audio_config` namespace from `src/audio/src/configbridge.*`:
//! audio-specific configuration read through the oak_core C ABI.

use std::error::Error;
use std::str::FromStr;
use oak_core::configstore::*;
/// PortAudio output buffer size in frames; 0 = let PortAudio choose.
///
/// `// CPP-PARITY: src/audio/src/configbridge.cpp:30`
/// (`audio_config::output_buffer_size`).
pub fn output_buffer_size() -> i32 {
	ConfigStore::instance().get_int(None, "AudioOutputBufferSize", 0)
}

/// Name of the configured audio device for `is_output_device`
/// (key "AudioOutput" / "AudioInput"); empty when absent.
///
/// `// CPP-PARITY: src/audio/src/configbridge.cpp:36`
/// (`audio_config::device_name`): two-stage size query; `size <= 1`
/// (absent or empty string) yields the empty string.
pub fn device_name(is_output_device: bool) -> Result<String, Box<dyn Error>> {
	let key = if is_output_device {
		"AudioOutput"
	} else {
		"AudioInput"
	};
	let store = ConfigStore::instance();
	let size = i32::from_str(store.get(None, key)?.as_str())?;
	if size <= 1 {
		// Absent (oak_core_E_NOT_FOUND) or empty
		return Err(Box::new(crate::error::Error::NotFound));
	}
	let name = store.get(None, key)?;
	Ok(name)
}
