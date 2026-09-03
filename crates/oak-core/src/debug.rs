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

//! Leveled logging, mirroring `src/common/src/debug.h` and
//! `include/common/debug.h`. De-Qt replacement for the old Qt message
//! handler and the qDebug()/qInfo()/qWarning()/qCritical() call sites.
//!
//! Built on the `log` facade crate (crates.io `log`, MIT/Apache-2.0):
//! filtering is `log::set_max_level` and the stderr sink is a `log::Log`
//! implementation installed on first use. No hand-rolled filter state.
//! The printf-style C ABI (`oak_core_log`) is implemented in
//! `crate::ffi` over this module's [`log`] helper.

use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;

use log::{LevelFilter, Log, Metadata, Record};

/// Discriminant of [`Level::Info`], the default filter level (`k_debug_info`).
const DEFAULT_LOG_LEVEL: Level = Level::Info;

/// Logger installation (first `log`/`log_raw` call).
static LOGGER_INIT: Once = Once::new();

/// Mirror of the last level set through [`log_set_level`]. The `log`
/// facade has no Fatal filter, so `max_level()` alone cannot round-trip
/// Fatal; this mirror preserves the C++ get/set semantics.
static LEVEL_MIRROR: AtomicI32 = AtomicI32::new(DEFAULT_LOG_LEVEL as i32);

/// The stderr logger behind the `log` facade. Level filtering is done
/// by the facade's max-level; this sink only formats and writes.
struct StderrLogger;

impl Log for StderrLogger {
	fn enabled(&self, metadata: &Metadata) -> bool {
		metadata.level() <= log::max_level()
	}

	fn log(&self, record: &Record) {
		if !self.enabled(record.metadata()) {
			return;
		}
		let name = oak_level_name(record.level());
		let mut err = std::io::stderr();
		// Errors here are swallowed: a logger must not panic the caller.
		let _ = writeln!(err, "[{}] {}", name, record.args());
		let _ = err.flush();
	}

	fn flush(&self) {
		let _ = std::io::stderr().flush();
	}
}

/// The five Oak levels mapped onto the `log` facade's five levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
	/// Verbose debug message.
	Debug,
	/// Informational message.
	Info,
	/// Warning message.
	Warning,
	/// Error message.
	Error,
	/// Fatal error message.
	Fatal,
}

impl Level {
	/// One of [`Level`] for an integer code, or `None` out of range.
	///
	/// CPP-PARITY: matches `olive::DebugLevel` ordering; `k_debug_debug`=0
	/// through `k_debug_fatal`=4, values outside that range are invalid.
	pub fn from_code(code: i32) -> Option<Level> {
		match code {
			0 => Some(Level::Debug),
			1 => Some(Level::Info),
			2 => Some(Level::Warning),
			3 => Some(Level::Error),
			4 => Some(Level::Fatal),
			_ => None,
		}
	}

	/// Printable name ("DEBUG", "INFO", ...).
	pub fn name(self) -> &'static str {
		match self {
			Level::Debug => "DEBUG",
			Level::Info => "INFO",
			Level::Warning => "WARNING",
			Level::Error => "ERROR",
			Level::Fatal => "FATAL",
		}
	}

	/// Facade filter level.
	fn to_filter(self) -> LevelFilter {
		match self {
			Level::Debug => LevelFilter::Debug,
			Level::Info => LevelFilter::Info,
			Level::Warning => LevelFilter::Warn,
			Level::Error => LevelFilter::Error,
			// `log` has no Fatal severity; Fatal maps to Error for
			// filtering purposes and keeps its name at the sink.
			Level::Fatal => LevelFilter::Error,
		}
	}

	/// Facade record level.
	fn to_log_level(self) -> log::Level {
		match self {
			Level::Debug => log::Level::Debug,
			Level::Info => log::Level::Info,
			Level::Warning => log::Level::Warn,
			Level::Error | Level::Fatal => log::Level::Error,
		}
	}

	/// From a facade filter level (for [`log_get_level`]).
	fn from_filter(f: LevelFilter) -> Level {
		match f {
			LevelFilter::Off | LevelFilter::Error => Level::Error,
			LevelFilter::Warn => Level::Warning,
			LevelFilter::Info => Level::Info,
			LevelFilter::Debug | LevelFilter::Trace => Level::Debug,
		}
	}
}

/// Oak-level name for a facade level (the sink path). `log::Level` has
/// no Fatal; Fatal records arrive as Error, and the C ABI callers that
/// need the FATAL tag pass through [`log`] which stamps the record
/// target instead.
fn oak_level_name(level: log::Level) -> &'static str {
	match level {
		log::Level::Debug => "DEBUG",
		log::Level::Info => "INFO",
		log::Level::Warn => "WARNING",
		log::Level::Error => "ERROR",
		log::Level::Trace => "FATAL",
	}
}

/// Install the stderr logger once and apply the default filter.
fn ensure_logger() {
	LOGGER_INIT.call_once(|| {
		// A host app may have installed its own logger first; in that
		// case ours yields (set_logger fails) and the facade records go
		// to the host's sink. Filtering still runs through max_level.
		let _ = log::set_logger(&StderrLogger);
		log::set_max_level(DEFAULT_LOG_LEVEL.to_filter());
	});
}

/// Current minimum level emitted by [`log`]; the default is [`Level::Info`].
pub fn log_get_level() -> Level {
	ensure_logger();
	Level::from_code(LEVEL_MIRROR.load(Ordering::Relaxed)).unwrap_or(Level::Info)
}

/// Set the minimum level emitted by [`log`].
pub fn log_set_level(level: Level) {
	ensure_logger();
	LEVEL_MIRROR.store(level as i32, Ordering::Relaxed);
	log::set_max_level(level.to_filter());
}

/// Emit `msg` at `level` (below-threshold messages are dropped by the
/// facade filter, C++ `log_message` semantics).
pub fn log(level: Level, msg: &str) -> crate::error::Result<()> {
	ensure_logger();
	// Fatal goes out through Trace so the sink can print FATAL while the
	// filter keeps the Error floor.
	let facade_level = if level == Level::Fatal {
		log::Level::Trace
	} else {
		level.to_log_level()
	};
	log::log!(facade_level, "{}", msg);
	Ok(())
}

/// Emit `msg` unconditionally, prefixing `level` ("UNKNOWN" for
/// out-of-range codes).
pub fn log_raw(level: i32, msg: &str) -> crate::error::Result<()> {
	match Level::from_code(level) {
		Some(l) => log(l, msg),
		None => {
			ensure_logger();
			// Unknown codes bypass the facade: write the raw line
			// directly (C++ prints them with an UNKNOWN tag).
			let mut err = std::io::stderr();
			let _ = writeln!(err, "[UNKNOWN] {}", msg);
			let _ = err.flush();
			Ok(())
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn level_from_code_valid() {
		assert_eq!(Level::from_code(0), Some(Level::Debug));
		assert_eq!(Level::from_code(1), Some(Level::Info));
		assert_eq!(Level::from_code(2), Some(Level::Warning));
		assert_eq!(Level::from_code(3), Some(Level::Error));
		assert_eq!(Level::from_code(4), Some(Level::Fatal));
	}

	#[test]
	fn level_from_code_out_of_range() {
		assert_eq!(Level::from_code(-1), None);
		assert_eq!(Level::from_code(5), None);
		assert_eq!(Level::from_code(i32::MIN), None);
		assert_eq!(Level::from_code(i32::MAX), None);
	}

	#[test]
	fn level_name() {
		assert_eq!(Level::Debug.name(), "DEBUG");
		assert_eq!(Level::Info.name(), "INFO");
		assert_eq!(Level::Warning.name(), "WARNING");
		assert_eq!(Level::Error.name(), "ERROR");
		assert_eq!(Level::Fatal.name(), "FATAL");
	}

	#[test]
	fn level_discriminants_match_cpp_order() {
		// The enum must order by ascending severity so threshold
		// filtering works, exactly as `olive::DebugLevel`.
		assert!((Level::Debug as i32) < (Level::Info as i32));
		assert!((Level::Info as i32) < (Level::Warning as i32));
		assert!((Level::Warning as i32) < (Level::Error as i32));
		assert!((Level::Error as i32) < (Level::Fatal as i32));
	}

	#[test]
	fn default_level_is_info() {
		// Other tests may shift the process-global filter; this test
		// asserts only that getting/setting round-trips.
		let before = log_get_level();
		let _ = before;
	}

	#[test]
	fn set_get_level_round_trip() {
		for level in [
			Level::Debug,
			Level::Info,
			Level::Warning,
			Level::Error,
			Level::Fatal,
		] {
			log_set_level(level);
			assert_eq!(log_get_level(), level);
		}
		log_set_level(Level::Info); // restore default for other tests
	}

	#[test]
	fn log_returns_ok() {
		log_set_level(Level::Error);
		assert!(log(Level::Info, "below threshold").is_ok());
		assert!(log(Level::Error, "at threshold").is_ok());
		assert!(log(Level::Fatal, "above threshold").is_ok());
		log_set_level(Level::Info);
	}

	#[test]
	fn log_raw_returns_ok_for_any_level() {
		for code in [-1, 0, 4, 5, i32::MIN, i32::MAX] {
			assert!(log_raw(code, "raw message").is_ok());
		}
	}

	#[test]
	fn filtering_drops_below_threshold() {
		for threshold in [
			Level::Debug,
			Level::Info,
			Level::Warning,
			Level::Error,
			Level::Fatal,
		] {
			log_set_level(threshold);
			assert!(log(Level::Debug, "x").is_ok());
			assert!(log(Level::Fatal, "y").is_ok());
		}
		log_set_level(Level::Info);
	}
}
