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

//! Command-line option parser, mirroring
//! `src/common/src/commandlineparser.h` and
//! `include/common/commandlineparser.h`.
//!
//! A parser owns a list of registered options and positional arguments;
//! option/argument handles returned at registration time are owned by the
//! parser and stay valid until it is destroyed.

use std::ffi::CString;
use std::io::{self, Write};

use crate::error::{Error, Result};

/// `olive::CommandLineParser` — owns registered options and positional
/// arguments.
pub struct CommandLineParser {
	/// Application name shown by `print_help`.
	app_name: String,
	/// Application version shown by `print_help`.
	app_version: String,
	/// Registered named options.
	///
	/// Boxed so that the option objects keep a stable address when the
	/// vector grows; option handles borrowed by the C ABI point at the
	/// boxed value. // CPP-PARITY: the C++ code heap-allocates each
	/// `Option` (`new Option()`); `Vec<Box<..>>` reproduces that stability
	/// against reallocation.
	options: Vec<Box<CommandLineOption>>,
	/// Registered positional arguments (see `options` for the boxing note).
	positionals: Vec<Box<CommandLinePositionalArgument>>,
}

impl CommandLineParser {
	/// New empty parser.
	pub fn new() -> Self {
		// CPP-PARITY: `app_name_` defaults to "oak" in the C++ header
		// (`std::string app_name_ = "oak"`), `app_version_` to "".
		Self {
			app_name: "oak".to_string(),
			app_version: String::new(),
			options: Vec::new(),
			positionals: Vec::new(),
		}
	}

	/// Set the application name/version shown by `print_help`.
	pub fn set_app_info(&mut self, name: &str, version: &str) {
		self.app_name = name.to_string();
		self.app_version = version.to_string();
	}

	/// Register an option with one or more name strings.
	///
	/// Returns the option, which remains owned by the parser.
	pub fn add_option(
		&mut self,
		names: &[CString],
		description: &str,
		takes_arg: bool,
		arg_placeholder: &str,
		hidden: bool,
	) -> Result<()> {
		let mut strings = Vec::with_capacity(names.len());
		for n in names {
			// CPP-PARITY: C++ stores raw byte strings and accepts any bytes;
			// Rust `String` is UTF-8, so a non-UTF-8 option name is rejected
			// here with E_INVALID. The C ABI `add_option` already rejects
			// null/empty names before reaching this point.
			strings.push(n.to_str().map_err(|_| Error::Invalid)?.to_string());
		}

		self.options.push(Box::new(CommandLineOption {
			names: strings,
			description: description.to_string(),
			takes_arg,
			arg_placeholder: arg_placeholder.to_string(),
			hidden,
			is_set: false,
			setting: None,
		}));
		Ok(())
	}

	/// Register a positional argument.
	pub fn add_positional_argument(
		&mut self,
		name: &str,
		description: &str,
		required: bool,
	) -> Result<()> {
		self.positionals
			.push(Box::new(CommandLinePositionalArgument {
				name: name.to_string(),
				description: description.to_string(),
				required,
				setting: None,
			}));
		Ok(())
	}

	/// Parse an argv-style argument list (argv[0] is the program name and
	/// is skipped).
	pub fn process(&mut self, argv: &[CString]) -> Result<()> {
		let mut positional_index = 0usize;
		let mut i = 1usize; // CPP-PARITY: argv[0] is the program name, skipped.

		while i < argv.len() {
			let arg_str = match argv[i].to_str() {
				Ok(s) => s.to_string(),
				Err(_) => {
					// CPP-PARITY: C++ compares raw bytes; a non-UTF-8 arg
					// cannot match a known option/positional here, so treat
					// it as an unknown parameter (best effort for the log).
					eprintln!("Unknown parameter: {}", argv[i].to_string_lossy());
					i += 1;
					continue;
				}
			};

			if !arg_str.is_empty() && arg_str.starts_with('-') {
				// Must be an option. Skip past the first dash.
				let arg_basename = &arg_str[1..];

				let mut matched_known = false;
				let mut consume_next = false;

				// CPP-PARITY: the C++ `goto found_flag` is a labelled break
				// out of the double loop; the match is case-insensitive.
				'find: for opt in self.options.iter_mut() {
					for name in &opt.names {
						if name.eq_ignore_ascii_case(arg_basename) {
							opt.is_set = true;
							if opt.takes_arg && i + 1 < argv.len() {
								// CPP-PARITY: the argument value is stored as
								// a byte string in C++; Rust only keeps it when
								// it is valid UTF-8, otherwise it is left unset.
								opt.setting = argv[i + 1].to_str().ok().map(str::to_owned);
								consume_next = true;
							}
							matched_known = true;
							break 'find;
						}
					}
				}

				if !matched_known {
					eprintln!("Unknown parameter: {}", arg_str);
				}
				// CPP-PARITY: when a `takes_arg` option consumes the following
				// argument, C++ advances `i` twice (the inner `i++` plus the
				// loop increment); otherwise once.
				i += if consume_next { 2 } else { 1 };
			} else {
				// Must be a positional argument.
				if positional_index < self.positionals.len() {
					self.positionals[positional_index].setting = Some(arg_str);
					positional_index += 1;
				} else {
					eprintln!("Unknown parameter: {}", arg_str);
				}
				i += 1;
			}
		}

		Ok(())
	}

	/// Print usage/help text to stdout.
	pub fn print_help(&self, filename: &str) -> Result<()> {
		let stdout = io::stdout();
		let mut out = stdout.lock();
		self.write_help(&mut out, filename)
	}

	/// Render the usage/help text into `out`, mirroring
	/// `CommandLineParser::print_help`. Split out so tests can capture the
	/// output in a buffer.
	fn write_help<W: Write>(&self, out: &mut W, filename: &str) -> Result<()> {
		let r = (|| -> io::Result<()> {
			// CPP-PARITY: `printf("%s %s\n")` always emits the separating
			// space, so "oak " + version + "\n" when the version is empty.
			writeln!(out, "{} {}", self.app_name, self.app_version)?;
			writeln!(out, "Copyright (C) 2018-2022 Oak Video Editor Team")?;

			// Build the "[name] [name] ..." positional list.
			let mut positional_args = String::new();
			for (i, p) in self.positionals.iter().enumerate() {
				if i > 0 {
					positional_args.push(' ');
				}
				positional_args.push('[');
				positional_args.push_str(&p.name);
				positional_args.push(']');
			}

			// CPP-PARITY: on POSIX the basename is everything after the last
			// '/'; without a slash the whole string is used.
			let basename = match filename.rfind('/') {
				Some(pos) => &filename[pos + 1..],
				None => filename,
			};

			writeln!(out, "Usage: {} [options] {}\n", basename, positional_args)?;

			for opt in &self.options {
				if opt.hidden {
					continue;
				}

				let mut all_args = String::new();
				for (i, name) in opt.names.iter().enumerate() {
					if i > 0 {
						all_args.push_str(", ");
					}
					all_args.push('-');
					all_args.push_str(name);
				}

				if opt.arg_placeholder.is_empty() {
					writeln!(out, "    {}", all_args)?;
				} else {
					writeln!(out, "    {} <{}>", all_args, opt.arg_placeholder)?;
				}
				writeln!(out, "        {}\n", opt.description)?;
			}

			writeln!(out)?;
			Ok(())
		})();
		r.map_err(|e| Error::Failed(e.to_string()))
	}

	// The six accessors below are `pub(crate)` so the C ABI layer
	// (`crate::ffi::commandlineparser`) can hand out stable borrowed handles
	// to the boxed options/arguments; they are currently unused until that
	// layer is implemented.
	#[allow(dead_code)]
	/// Number of registered options. `pub(crate)`: used by the C ABI layer
	/// to look up the option just appended by [`Self::add_option`].
	pub(crate) fn option_count(&self) -> usize {
		self.options.len()
	}

	/// Borrow a registered option by index. `pub(crate)`: hands the C ABI
	/// layer a stable pointer to the boxed option for a borrowed handle.
	#[allow(dead_code)]
	pub(crate) fn option(&self, index: usize) -> Option<&CommandLineOption> {
		self.options.get(index).map(|b| b.as_ref())
	}

	/// Mutably borrow a registered option by index.
	#[allow(dead_code)]
	pub(crate) fn option_mut(&mut self, index: usize) -> Option<&mut CommandLineOption> {
		self.options.get_mut(index).map(|b| b.as_mut())
	}

	/// Number of registered positional arguments.
	#[allow(dead_code)]
	pub(crate) fn positional_count(&self) -> usize {
		self.positionals.len()
	}

	/// Borrow a registered positional argument by index.
	#[allow(dead_code)]
	pub(crate) fn positional(&self, index: usize) -> Option<&CommandLinePositionalArgument> {
		self.positionals.get(index).map(|b| b.as_ref())
	}

	/// Mutably borrow a registered positional argument by index.
	#[allow(dead_code)]
	pub(crate) fn positional_mut(
		&mut self,
		index: usize,
	) -> Option<&mut CommandLinePositionalArgument> {
		self.positionals.get_mut(index).map(|b| b.as_mut())
	}
}

impl Default for CommandLineParser {
	fn default() -> Self {
		Self::new()
	}
}

/// `olive::CommandLineOption` — a registered named option.
pub struct CommandLineOption {
	/// Option name strings (without leading dash).
	names: Vec<String>,
	/// Help text.
	description: String,
	/// Whether the option consumes the following argument.
	takes_arg: bool,
	/// Placeholder shown in help when `takes_arg`.
	arg_placeholder: String,
	/// Whether to omit from help output.
	hidden: bool,
	/// Whether the option was present on the command line.
	is_set: bool,
	/// The argument value (present only when set).
	setting: Option<String>,
}

impl CommandLineOption {
	/// Whether the option was present on the command line.
	pub fn is_set(&self) -> bool {
		self.is_set
	}

	/// The option's argument value, if one was set.
	pub fn get_setting(&self) -> Result<&str> {
		match &self.setting {
			Some(v) => Ok(v.as_str()),
			// CPP-PARITY: C++ `get_setting()` returns the (possibly empty)
			// stored string and never fails; the "was it supplied" question
			// is answered by [`Self::is_set`]. Rust mirrors this by returning
			// an empty string when nothing was stored.
			None => Ok(""),
		}
	}

	/// Set the option's argument value.
	pub fn set_setting(&mut self, value: &str) -> Result<()> {
		self.setting = Some(value.to_string());
		Ok(())
	}
}

/// `olive::CommandLinePositionalArgument` — a registered positional
/// argument.
pub struct CommandLinePositionalArgument {
	/// Argument name.
	name: String,
	/// Help text.
	#[allow(dead_code)] // CPP-PARITY: stored but never read in C++ print_help/process.
	description: String,
	/// Whether the argument is required.
	#[allow(dead_code)] // CPP-PARITY: stored but never consumed in the C++ source.
	required: bool,
	/// The argument value (present only when parsed).
	setting: Option<String>,
}

impl CommandLinePositionalArgument {
	/// The argument's value, if one was parsed.
	pub fn get_setting(&self) -> Result<&str> {
		match &self.setting {
			Some(v) => Ok(v.as_str()),
			None => Ok(""),
		}
	}

	/// Set the argument's value.
	pub fn set_setting(&mut self, value: &str) -> Result<()> {
		self.setting = Some(value.to_string());
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn cstr(s: &str) -> CString {
		CString::new(s).unwrap()
	}

	/// `new()` reproduces the C++ defaults: app name "oak", empty version,
	/// no registered options/arguments.
	#[test]
	fn defaults() {
		let p = CommandLineParser::new();
		assert_eq!(p.app_name, "oak");
		assert_eq!(p.app_version, "");
		assert_eq!(p.option_count(), 0);
		assert_eq!(p.positional_count(), 0);
	}

	/// `set_app_info` overwrites the name and version.
	#[test]
	fn set_app_info() {
		let mut p = CommandLineParser::new();
		p.set_app_info("myapp", "2.5");
		assert_eq!(p.app_name, "myapp");
		assert_eq!(p.app_version, "2.5");
	}

	/// Adding options/arguments increments the counts and they are
	/// retrievable by index.
	#[test]
	fn add_registers() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("h"), cstr("help")], "show help", false, "", false)
			.unwrap();
		p.add_option(&[cstr("o")], "output", true, "FILE", false)
			.unwrap();
		p.add_option(&[cstr("secret")], "hidden opt", false, "", true)
			.unwrap();
		p.add_positional_argument("input", "input file", true)
			.unwrap();
		p.add_positional_argument("output", "output file", false)
			.unwrap();

		assert_eq!(p.option_count(), 3);
		assert_eq!(p.positional_count(), 2);

		let opt = p.option(0).unwrap();
		assert_eq!(opt.names, vec!["h".to_string(), "help".to_string()]);
		assert!(!opt.takes_arg);
		assert!(!opt.hidden);
		assert!(!opt.is_set());
		assert_eq!(opt.get_setting().unwrap(), "");

		let opt = p.option(1).unwrap();
		assert!(opt.takes_arg);
		assert_eq!(opt.arg_placeholder, "FILE");

		let opt = p.option(2).unwrap();
		assert!(opt.hidden);

		let pos = p.positional(0).unwrap();
		assert_eq!(pos.name, "input");
		assert!(pos.required);
		assert_eq!(pos.get_setting().unwrap(), "");
	}

	/// A non-UTF-8 option name is rejected with E_INVALID.
	#[test]
	fn add_option_rejects_non_utf8_name() {
		let mut p = CommandLineParser::new();
		let bad = CString::new(vec![b'x', 0xFF, 0xFE]).unwrap();
		assert!(matches!(
			p.add_option(&[bad], "", false, "", false),
			Err(Error::Invalid)
		));
		assert_eq!(p.option_count(), 0);
	}

	/// get/set_setting round-trips on an option.
	#[test]
	fn option_setting_roundtrip() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		let mut opt = p.option_mut(0).unwrap();
		assert_eq!(opt.get_setting().unwrap(), "");
		opt.set_setting("value").unwrap();
		assert_eq!(opt.get_setting().unwrap(), "value");
	}

	/// get/set_setting round-trips on a positional argument.
	#[test]
	fn positional_setting_roundtrip() {
		let mut p = CommandLineParser::new();
		p.add_positional_argument("in", "", true).unwrap();
		let mut pos = p.positional_mut(0).unwrap();
		assert_eq!(pos.get_setting().unwrap(), "");
		pos.set_setting("file.mp4").unwrap();
		assert_eq!(pos.get_setting().unwrap(), "file.mp4");
	}

	/// process(): simple flags, case-insensitive matching, value-consuming
	/// options, and positional assignment.
	#[test]
	fn process_flags_and_values() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("h"), cstr("help")], "", false, "", false)
			.unwrap();
		p.add_option(&[cstr("o")], "", true, "FILE", false).unwrap();
		p.add_option(&[cstr("V")], "", false, "", false).unwrap();
		p.add_positional_argument("input", "", true).unwrap();

		let argv = [
			cstr("prog"),
			cstr("-help"),
			cstr("-o"),
			cstr("out.mov"),
			cstr("-v"),
			cstr("in.mp4"),
		];
		p.process(&argv).unwrap();

		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "");

		// `-o` consumed `out.mov` as its argument.
		assert!(p.option(1).unwrap().is_set());
		assert_eq!(p.option(1).unwrap().get_setting().unwrap(), "out.mov");

		// `-v` matched `-V` case-insensitively.
		assert!(p.option(2).unwrap().is_set());

		// `in.mp4` filled the first positional.
		assert_eq!(p.positional(0).unwrap().get_setting().unwrap(), "in.mp4");
	}

	/// process(): argv[0] is skipped; a leading empty string before the
	/// first arg does not disturb parsing.
	#[test]
	fn process_skips_program_name() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("x")], "", false, "", false).unwrap();
		let argv = [cstr("prog"), cstr("-x")];
		p.process(&argv).unwrap();
		assert!(p.option(0).unwrap().is_set());

		// A bare program name with no args is fine.
		let argv = [cstr("prog")];
		p.process(&argv).unwrap();
	}

	/// process(): a `takes_arg` option at the end of argv consumes nothing
	/// and stays set without a value.
	#[test]
	fn process_takes_arg_at_end() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "FILE", false).unwrap();
		let argv = [cstr("prog"), cstr("-o")];
		p.process(&argv).unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "");
	}

	/// process(): C++ `process` only ever sets state, never resets it, so a
	/// second call that does not mention an option leaves its previous
	/// `is_set`/setting intact.
	#[test]
	fn process_accumulates_state() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.process(&[cstr("p"), cstr("-o"), cstr("a")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "a");

		// CPP-PARITY: no reset between calls.
		p.process(&[cstr("p")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "a");
	}

	/// process(): only a single leading dash is stripped, so `--o` is a
	/// distinct (unknown) argument rather than matching option `o`.
	#[test]
	fn process_strips_single_dash() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.process(&[cstr("p"), cstr("--o"), cstr("v")]).unwrap();
		assert!(!p.option(0).unwrap().is_set());
		// `--o` was unknown, so `v` fell through to be ignored (no
		// positionals registered).
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "");
	}

	/// print_help() renders the exact text the C++ produces.
	#[test]
	fn print_help_exact() {
		let mut p = CommandLineParser::new();
		p.set_app_info("oak", "1.2.3");
		p.add_option(
			&[cstr("h"), cstr("help")],
			"Show this help message.",
			false,
			"",
			false,
		)
		.unwrap();
		p.add_option(&[cstr("o")], "Output file.", true, "FILE", true)
			.unwrap(); // hidden, omitted
		p.add_option(&[cstr("t")], "Time.", true, "SEC", false)
			.unwrap();
		p.add_positional_argument("input", "Input file", true)
			.unwrap();

		let mut buf = Vec::new();
		p.write_help(&mut buf, "/usr/local/bin/oak").unwrap();
		let text = String::from_utf8(buf).unwrap();

		// CPP-PARITY: C++ emits "        %s\n\n" for the last option and then
		// a final "\n", so the text ends with three newlines after "Time.".
		let expected = "\
oak 1.2.3
Copyright (C) 2018-2022 Oak Video Editor Team
Usage: oak [options] [input]

    -h, -help
        Show this help message.

    -t <SEC>
        Time.


";
		assert_eq!(text, expected);
	}

	/// print_help() with a bare filename (no slash) uses it as-is; the
	/// default empty version yields a trailing-space header line.
	#[test]
	fn print_help_bare_filename_and_default_version() {
		let p = CommandLineParser::new();
		let mut buf = Vec::new();
		p.write_help(&mut buf, "oak").unwrap();
		let text = String::from_utf8(buf).unwrap();
		assert!(text.starts_with("oak \n"), "header was {:?}", &text[..12]);
		assert!(text.contains("Usage: oak [options] \n"));
	}

	/// An option/argument handle stays valid (stable address) while more
	/// options are registered, because options are boxed.
	#[test]
	fn option_address_is_stable() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("a")], "", false, "", false).unwrap();
		let first = p.option(0).unwrap() as *const CommandLineOption;

		// Push enough options to force reallocation.
		for i in 0..100 {
			p.add_option(&[cstr(&format!("x{}", i))], "", false, "", false)
				.unwrap();
		}
		assert_eq!(first, p.option(0).unwrap() as *const CommandLineOption);
	}

	/// An option is matched by any of its registered names, not just the
	/// first.
	#[test]
	fn process_matches_any_registered_name() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("h"), cstr("help"), cstr("?")], "", false, "", false)
			.unwrap();

		p.process(&[cstr("p"), cstr("-?")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
	}

	/// Matching is case-insensitive over the whole name (C++
	/// `string_equals_case_insensitive` applies tolower to every byte).
	#[test]
	fn process_case_insensitive_full_name() {
		for arg in ["-fullscreen", "-FULLSCREEN", "-Fullscreen"] {
			let mut q = CommandLineParser::new();
			q.add_option(&[cstr("FullScreen")], "", false, "", false)
				.unwrap();
			q.process(&[cstr("p"), cstr(arg)]).unwrap();
			assert!(q.option(0).unwrap().is_set(), "arg {:?} did not match", arg);
		}
	}

	/// A `takes_arg` option consumes the following argument verbatim, even
	/// if it starts with a dash (C++ takes argv[i+1] unconditionally).
	#[test]
	fn process_takes_arg_consumes_dash_value() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.add_option(&[cstr("h")], "", false, "", false).unwrap();

		p.process(&[cstr("p"), cstr("-o"), cstr("-h")]).unwrap();
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "-h");
		// `-h` was eaten as the value, never parsed as a flag.
		assert!(!p.option(1).unwrap().is_set());
	}

	/// A non-`takes_arg` option does not consume the next argument; it
	/// falls through to the positional arguments.
	#[test]
	fn process_flag_does_not_consume_next() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("f")], "", false, "", false).unwrap();
		p.add_positional_argument("input", "", true).unwrap();

		p.process(&[cstr("p"), cstr("-f"), cstr("in.mp4")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.positional(0).unwrap().get_setting().unwrap(), "in.mp4");
	}

	/// Repeating a `takes_arg` option overwrites the previous value; the
	/// option stays set.
	#[test]
	fn process_duplicate_option_last_value_wins() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.process(&[cstr("p"), cstr("-o"), cstr("a"), cstr("-o"), cstr("b")])
			.unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "b");
	}

	/// Positional arguments fill in order; extras are reported as unknown
	/// and parsing continues.
	#[test]
	fn process_positional_overflow_is_unknown() {
		let mut p = CommandLineParser::new();
		p.add_positional_argument("first", "", true).unwrap();
		p.add_positional_argument("second", "", false).unwrap();

		// "third" exceeds the registered positionals: C++ prints
		// "Unknown parameter" to stderr and moves on.
		p.process(&[cstr("p"), cstr("1"), cstr("2"), cstr("3"), cstr("-x")])
			.unwrap();
		assert_eq!(p.positional(0).unwrap().get_setting().unwrap(), "1");
		assert_eq!(p.positional(1).unwrap().get_setting().unwrap(), "2");
	}

	/// An empty-string argument does not start with '-', so it is treated
	/// as a positional value (C++ checks `!argv[i].empty()` first).
	#[test]
	fn process_empty_string_is_positional() {
		let mut p = CommandLineParser::new();
		p.add_positional_argument("input", "", false).unwrap();
		p.process(&[cstr("p"), cstr("")]).unwrap();
		assert_eq!(p.positional(0).unwrap().get_setting().unwrap(), "");
	}

	/// A bare "-" strips to an empty basename, matches nothing, and is
	/// reported unknown.
	#[test]
	fn process_bare_dash_is_unknown() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", false, "", false).unwrap();
		p.add_positional_argument("input", "", false).unwrap();
		p.process(&[cstr("p"), cstr("-")]).unwrap();
		assert!(!p.option(0).unwrap().is_set());
		assert_eq!(p.positional(0).unwrap().get_setting().unwrap(), "");
	}

	/// There is no `--` terminator and no `--opt=val` syntax in the C++
	/// parser: `--opt=val` strips one dash and fails to match `opt`.
	#[test]
	fn process_no_double_dash_or_equals_syntax() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.process(&[cstr("p"), cstr("--o=v")]).unwrap();
		assert!(!p.option(0).unwrap().is_set());

		let mut q = CommandLineParser::new();
		q.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		q.add_positional_argument("in", "", false).unwrap();
		// `--` is just an unknown option, not a terminator.
		q.process(&[cstr("p"), cstr("--"), cstr("x")]).unwrap();
		assert_eq!(q.positional(0).unwrap().get_setting().unwrap(), "x");
	}

	/// When two registered options share a name, the first registered one
	/// wins (C++ iterates `options_` in order and `goto found_flag` stops
	/// at the first match).
	#[test]
	fn process_first_matching_option_wins() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("x")], "", false, "", false).unwrap();
		p.add_option(&[cstr("x")], "", false, "", false).unwrap();
		p.process(&[cstr("p"), cstr("-x")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert!(!p.option(1).unwrap().is_set());
	}

	/// is_set is false until the option appears; get_setting on an unset
	/// option returns "" rather than an error (C++ returns the stored
	/// string, which is default-constructed empty).
	#[test]
	fn unset_option_state() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		let opt = p.option(0).unwrap();
		assert!(!opt.is_set());
		assert_eq!(opt.get_setting().unwrap(), "");
	}

	/// set_setting on an option does NOT set is_set (C++ Option::set and
	/// set_setting are independent).
	#[test]
	fn set_setting_does_not_mark_is_set() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		{
			let mut opt = p.option_mut(0).unwrap();
			opt.set_setting("v").unwrap();
		}
		let opt = p.option(0).unwrap();
		assert!(!opt.is_set());
		assert_eq!(opt.get_setting().unwrap(), "v");
	}

	/// A `takes_arg` option set via process has is_set true AND a value;
	/// a non-takes_arg option has is_set true and an empty value.
	#[test]
	fn process_is_set_and_setting_combinations() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("f")], "", false, "", false).unwrap();
		p.add_option(&[cstr("o")], "", true, "F", false).unwrap();
		p.process(&[cstr("p"), cstr("-f"), cstr("-o"), cstr("v")])
			.unwrap();
		assert!(p.option(0).unwrap().is_set());
		assert_eq!(p.option(0).unwrap().get_setting().unwrap(), "");
		assert!(p.option(1).unwrap().is_set());
		assert_eq!(p.option(1).unwrap().get_setting().unwrap(), "v");
	}

	/// The public print_help writes to stdout without error (smoke test;
	/// exact output is covered by write_help tests).
	#[test]
	fn print_help_smoke() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("h")], "help", false, "", false)
			.unwrap();
		p.print_help("oak").unwrap();
	}

	/// Help output lists multiple option names joined with ", " and shows
	/// the `<placeholder>` only when non-empty, regardless of takes_arg.
	#[test]
	fn print_help_placeholder_rules() {
		let mut p = CommandLineParser::new();
		// takes_arg but no placeholder: no <...> shown.
		p.add_option(&[cstr("a")], "arg without placeholder", true, "", false)
			.unwrap();
		// placeholder but not takes_arg: <...> still shown (C++ keys on
		// arg_placeholder.empty(), not takes_arg).
		p.add_option(&[cstr("b")], "placeholder without arg", false, "P", false)
			.unwrap();

		let mut buf = Vec::new();
		p.write_help(&mut buf, "oak").unwrap();
		let text = String::from_utf8(buf).unwrap();
		assert!(text.contains("    -a\n"), "{}", text);
		assert!(text.contains("    -b <P>\n"), "{}", text);
	}

	/// Multiple positionals render as "[a] [b]" in the usage line.
	#[test]
	fn print_help_multiple_positionals() {
		let mut p = CommandLineParser::new();
		p.add_positional_argument("in", "", true).unwrap();
		p.add_positional_argument("out", "", false).unwrap();
		let mut buf = Vec::new();
		p.write_help(&mut buf, "oak").unwrap();
		let text = String::from_utf8(buf).unwrap();
		assert!(
			text.contains("Usage: oak [options] [in] [out]\n"),
			"{}",
			text
		);
	}

	/// Hidden options are omitted from help but still parse.
	#[test]
	fn hidden_option_parses_but_hidden_from_help() {
		let mut p = CommandLineParser::new();
		p.add_option(&[cstr("secret")], "shh", false, "", true)
			.unwrap();
		let mut buf = Vec::new();
		p.write_help(&mut buf, "oak").unwrap();
		let text = String::from_utf8(buf).unwrap();
		assert!(!text.contains("secret"), "{}", text);

		p.process(&[cstr("p"), cstr("-secret")]).unwrap();
		assert!(p.option(0).unwrap().is_set());
	}

	/// Out-of-range index accessors return None.
	#[test]
	fn index_accessors_out_of_range() {
		let p = CommandLineParser::new();
		assert!(p.option(0).is_none());
		assert!(p.positional(0).is_none());
	}

	/// add_option with zero names registers an option that can never
	/// match (the C++ ABI layer rejects name_count == 0 before reaching
	/// the domain type).
	#[test]
	fn add_option_empty_names_never_matches() {
		let mut p = CommandLineParser::new();
		p.add_option(&[], "no names", false, "", false).unwrap();
		assert_eq!(p.option_count(), 1);
		p.process(&[cstr("p"), cstr("-anything")]).unwrap();
		assert!(!p.option(0).unwrap().is_set());
	}
}
