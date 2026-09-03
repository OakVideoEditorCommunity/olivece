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

//! Miscellaneous helpers, folding together several small `include/common`
//! headers that share no handle: `miscutils.h` (decibel/lerp),
//! `loopmode.h`, `dropworkflowbehavior.h`, `power.h`, `current.h`. Each
//! public header still gets its own submodule in `crate::ffi` for C ABI
//! completeness; this file holds all their domain types.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::error::Result;

/// Minimum decibel value used by the editor (`-200.0` dB).
pub const DECIBEL_MINIMUM: f64 = -200.0;

/// Constant `-ln(0.01)` (`lo_g100`), shared by the logarithmic slider
/// conversions in `olive::Decibel` (`src/common/src/decibel.h`).
// CPP-PARITY: value copied verbatim from `olive::Decibel::lo_g100`.
const DECIBEL_LO_G100: f64 = 4.60517018599;

/// Convert a linear amplitude to decibels (0.0 or infinite results yield
/// [`DECIBEL_MINIMUM`]).
pub fn decibel_from_linear(linear: f64) -> Result<f64> {
	// CPP-PARITY: `20.0 * std::log10(linear)`, returning `minimum` when the
	// result is infinite. The C++ never fails, so this always yields Ok.
	let v = 20.0 * linear.log10();
	if v.is_infinite() {
		Ok(DECIBEL_MINIMUM)
	} else {
		Ok(v)
	}
}

/// Convert decibels to a linear amplitude (results below `1e-6` clamp to
/// 0.0).
pub fn decibel_to_linear(db: f64) -> Result<f64> {
	// CPP-PARITY: `std::pow(10.0, db / 20.0)`, clamping < 1e-6 to 0.
	let v = 10.0_f64.powf(db / 20.0);
	if v < 0.000001 {
		Ok(0.0)
	} else {
		Ok(v)
	}
}

/// Convert a logarithmic slider position (0..1) to decibels.
pub fn decibel_from_logarithmic(logarithmic: f64) -> Result<f64> {
	// CPP-PARITY: matches `olive::Decibel::from_logarithmic` (branch
	// thresholds and `20.0*log10(-log(1-x)/lo_g100)`).
	if logarithmic < 0.001 {
		Ok(DECIBEL_MINIMUM)
	} else if logarithmic > 0.99 {
		Ok(0.0)
	} else {
		Ok(20.0 * (-(1.0 - logarithmic).ln() / DECIBEL_LO_G100).log10())
	}
}

/// Convert decibels to a logarithmic slider position (0..1).
pub fn decibel_to_logarithmic(db: f64) -> Result<f64> {
	// CPP-PARITY: `1 - exp(-pow(10, db/20) * lo_g100)`, short-circuiting
	// `|db| <= 1e-12` to 1.
	if db.abs() <= 1e-12 {
		Ok(1.0)
	} else {
		Ok(1.0 - (-(10.0_f64.powf(db / 20.0)) * DECIBEL_LO_G100).exp())
	}
}

/// Convert a linear amplitude directly to a logarithmic position.
pub fn decibel_linear_to_logarithmic(linear: f64) -> Result<f64> {
	// CPP-PARITY: `1 - exp(-linear * lo_g100)`.
	Ok(1.0 - (-linear * DECIBEL_LO_G100).exp())
}

/// Convert a logarithmic position directly to a linear amplitude.
pub fn decibel_logarithmic_to_linear(logarithmic: f64) -> Result<f64> {
	// CPP-PARITY: `> 0.99 -> 1`, else `-log(1-x)/lo_g100`.
	if logarithmic > 0.99 {
		Ok(1.0)
	} else {
		Ok(-(1.0 - logarithmic).ln() / DECIBEL_LO_G100)
	}
}

/// Linearly interpolate between `a` and `b` using `t` (`0.0` -> `a`,
/// `1.0` -> `b`).
pub fn lerp(a: f64, b: f64, t: f64) -> Result<f64> {
	// CPP-PARITY: `lerp` template `(a*(1.0 - t)) + (b*t)`.
	Ok(a * (1.0 - t) + b * t)
}

/// Playback loop mode (`OakLoopMode`), mirroring `olive::LoopMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
	/// Looping disabled.
	Off = 0,
	/// Loop playback.
	Loop = 1,
	/// Clamp at the end.
	Clamp = 2,
}

/// Behavior when media is dropped onto a timeline without a sequence
/// (`OakDropWorkflowBehavior`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropWorkflowBehavior {
	/// Ask the user every time.
	Ask = 0,
	/// Automatically create a sequence.
	Auto = 1,
	/// Never create; import manually.
	Manual = 2,
	/// Disable dropping entirely.
	Disable = 3,
}

impl DropWorkflowBehavior {
	/// Whether `value` is a valid behavior.
	pub fn is_valid(value: i32) -> bool {
		// CPP-PARITY: the C++ switch matches the four enumerators.
		matches!(value, 0..=3)
	}

	/// Printable name ("UNKNOWN" for invalid values).
	pub fn name(value: i32) -> &'static str {
		// CPP-PARITY: exact strings from the c_api `behavior_name()`.
		match value {
			0 => "ASK",
			1 => "AUTO",
			2 => "MANUAL",
			3 => "DISABLE",
			_ => "UNKNOWN",
		}
	}
}

/// Round `value` up to the next power of two.
pub fn power_ceil_to_power_of_2(value: u32) -> Result<u32> {
	// CPP-PARITY: bit-blast from `olive::ceil_to_power_of_2`. Uses wrapping
	// arithmetic so the decrement of 0 wraps to `u32::MAX` (and the final
	// increment of `u32::MAX` wraps to 0), exactly like C++ unsigned wraps.
	let mut v = value.wrapping_sub(1);
	v |= v >> 1;
	v |= v >> 2;
	v |= v >> 4;
	v |= v >> 8;
	v |= v >> 16;
	Ok(v.wrapping_add(1))
}

/// Round `value` down to the nearest power of two.
pub fn power_floor_to_power_of_2(value: u32) -> Result<u32> {
	// CPP-PARITY: bit-blast from `olive::floor_to_power_of_2`.
	let mut x = value;
	x = x | (x >> 1);
	x = x | (x >> 2);
	x = x | (x >> 4);
	x = x | (x >> 8);
	x = x | (x >> 16);
	Ok(x - (x >> 1))
}

/// Destructor callback for objects handed to Current slots.
pub type DestroyFn = Option<unsafe extern "C" fn(*mut c_void)>;

/// One opaque slot value plus its destructor, mirroring a C++
/// `std::shared_ptr<void>` held by `Current`.
struct Slot {
	/// Opaque external object pointer (may be null = empty slot).
	ptr: *mut c_void,
	/// Optional destructor invoked when the slot is replaced or cleared.
	destroy: DestroyFn,
}

impl Slot {
	/// An empty slot.
	fn empty() -> Self {
		Self {
			ptr: std::ptr::null_mut(),
			destroy: None,
		}
	}
}

// `*mut c_void` is neither Send nor Sync; the singleton serialises all
// slot access behind a `Mutex`, so promising Send+Sync for the guarded
// `Slot` is sound.
// CPP-PARITY: the C++ `Current` uses `std::shared_ptr` (which is
// thread-safe) but does not itself lock; Rust guards each slot with a
// `Mutex` for sound `Send`/`Sync`.
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

/// The process-wide `Current` singleton (see `include/common/current.h`).
/// `ctx` points to a statically allocated object that lives until process
/// exit; addref/release are no-ops. Slots hold opaque external objects with
/// optional destructors.
pub struct Current {
	/// Video params slot.
	video_params: Mutex<Slot>,
	/// Audio params slot.
	audio_params: Mutex<Slot>,
	/// Plugin host slot.
	plugin_host: Mutex<Slot>,
	/// Plugin cache slot.
	plugin_cache: Mutex<Slot>,
	/// Whether the session is interactive.
	is_interactive: bool,
}

impl Current {
	/// The process-wide singleton.
	pub fn instance() -> &'static Current {
		// CPP-PARITY: mirrors `Current::get_instance()` returning a
		// process-wide static. `OnceLock` gives the same lazy,
		// thread-safe single-instance guarantee.
		static INSTANCE: OnceLock<Current> = OnceLock::new();
		INSTANCE.get_or_init(|| Current {
			video_params: Mutex::new(Slot::empty()),
			audio_params: Mutex::new(Slot::empty()),
			plugin_host: Mutex::new(Slot::empty()),
			plugin_cache: Mutex::new(Slot::empty()),
			is_interactive: true,
		})
	}

	/// Replace a slot's occupant, destroying the previous one if it had a
	/// destructor.
	fn set_slot(slot: &Mutex<Slot>, obj: *mut c_void, destroy: DestroyFn) -> Result<()> {
		// Recover the guard if a previous panic poisoned the mutex; the
		// slot contents remain valid.
		let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
		let old = std::mem::replace(&mut *s, Slot { ptr: obj, destroy });
		// CPP-PARITY: the old `shared_ptr`'s refcount drops to zero when it
		// is replaced, invoking its deleter (the stored `destroy`). A
		// previously stored NULL destroy was a no-op deleter, so nothing
		// runs then either.
		if !old.ptr.is_null() {
			if let Some(d) = old.destroy {
				// Safety: the destructor was supplied by the caller of the
				// matching `set_*` and owns the pointer it is given.
				unsafe { d(old.ptr) };
			}
		}
		Ok(())
	}

	/// Fetch a slot's raw occupant pointer (borrowed).
	fn get_slot(slot: &Mutex<Slot>) -> Result<*mut c_void> {
		let s = slot.lock().unwrap_or_else(|e| e.into_inner());
		Ok(s.ptr)
	}

	/// Store a pointer in the video-params slot, taking over destruction.
	pub fn set_video_params(&self, obj: *mut c_void, destroy: DestroyFn) -> Result<()> {
		Self::set_slot(&self.video_params, obj, destroy)
	}

	/// Store a pointer in the audio-params slot.
	pub fn set_audio_params(&self, obj: *mut c_void, destroy: DestroyFn) -> Result<()> {
		Self::set_slot(&self.audio_params, obj, destroy)
	}

	/// Store a pointer in the plugin-host slot.
	pub fn set_plugin_host(&self, obj: *mut c_void, destroy: DestroyFn) -> Result<()> {
		Self::set_slot(&self.plugin_host, obj, destroy)
	}

	/// Store a pointer in the plugin-cache slot.
	pub fn set_plugin_cache(&self, obj: *mut c_void, destroy: DestroyFn) -> Result<()> {
		Self::set_slot(&self.plugin_cache, obj, destroy)
	}

	/// Fetch the video-params slot.
	pub fn get_video_params(&self) -> Result<*mut c_void> {
		Self::get_slot(&self.video_params)
	}

	/// Fetch the audio-params slot.
	pub fn get_audio_params(&self) -> Result<*mut c_void> {
		Self::get_slot(&self.audio_params)
	}

	/// Fetch the plugin-host slot.
	pub fn get_plugin_host(&self) -> Result<*mut c_void> {
		Self::get_slot(&self.plugin_host)
	}

	/// Fetch the plugin-cache slot.
	pub fn get_plugin_cache(&self) -> Result<*mut c_void> {
		Self::get_slot(&self.plugin_cache)
	}

	/// Whether the session is interactive.
	pub fn is_interactive(&self) -> Result<bool> {
		// CPP-PARITY: `Current::interactive()` is hardcoded to `true` and
		// is never mutated, so the stored flag stays true.
		Ok(self.is_interactive)
	}
}

#[cfg(test)]
mod tests {
	use std::ffi::c_void;
	use std::sync::atomic::AtomicUsize;
	use std::sync::atomic::Ordering;

	use super::*;

	#[test]
	fn decibel_from_linear_known_values() {
		assert_eq!(decibel_from_linear(1.0).unwrap(), 0.0);
		assert!((decibel_from_linear(10.0).unwrap() - 20.0).abs() < 1e-12);
		assert!((decibel_from_linear(100.0).unwrap() - 40.0).abs() < 1e-12);
		// Zero / negative-infinite log10 clamps to minimum.
		assert_eq!(decibel_from_linear(0.0).unwrap(), DECIBEL_MINIMUM);
		// Negative input yields NaN in both C++ and Rust (no clamp).
		assert!(decibel_from_linear(-1.0).unwrap().is_nan());
	}

	#[test]
	fn decibel_to_linear_known_values() {
		assert_eq!(decibel_to_linear(0.0).unwrap(), 1.0);
		assert!((decibel_to_linear(20.0).unwrap() - 10.0).abs() < 1e-12);
		// Well below -120 dB clamps to 0; exactly 1e-6 does not.
		assert_eq!(decibel_to_linear(-200.0).unwrap(), 0.0);
		assert_eq!(decibel_to_linear(-120.0).unwrap(), 0.000001);
	}

	#[test]
	fn decibel_logarithmic_branch_thresholds() {
		// Very small positions clamp to minimum; >0.99 clamp to 0 dB.
		assert_eq!(decibel_from_logarithmic(0.0).unwrap(), DECIBEL_MINIMUM);
		assert_eq!(decibel_from_logarithmic(1.0).unwrap(), 0.0);
		// Mid-range is a real value.
		let db = decibel_from_logarithmic(0.5).unwrap();
		assert!(db.is_finite());
		// to_logarithmic clamps |db| <= 1e-12 to 1.0.
		assert_eq!(decibel_to_logarithmic(0.0).unwrap(), 1.0);
	}

	#[test]
	fn decibel_round_trips() {
		// linear <-> logarithmic round trip (within float tolerance).
		for linear in [0.001, 0.01, 0.1, 0.5, 0.9, 0.99] {
			let log = decibel_linear_to_logarithmic(linear).unwrap();
			let back = decibel_logarithmic_to_linear(log).unwrap();
			assert!(
				(back - linear).abs() < 1e-6,
				"linear {} -> {} -> {}",
				linear,
				log,
				back
			);
		}
		// db <-> logarithmic round trip. Only non-positive db are reversible:
		// a positive db pushes the logarithmic position past 0.99, which the
		// C++ `from_logarithmic` intentionally clamps back to 0 dB.
		for db in [-60.0, -30.0, -12.0, -6.0, -3.0, -1.0] {
			let log = decibel_to_logarithmic(db).unwrap();
			let back = decibel_from_logarithmic(log).unwrap();
			assert!((back - db).abs() < 1e-3, "db {} -> {} -> {}", db, log, back);
		}
		// A positive db saturates the logarithmic position > 0.99 and comes
		// back as 0 dB (faithful to the C++ clamp).
		let log6 = decibel_to_logarithmic(6.0).unwrap();
		assert!(log6 > 0.99);
		assert_eq!(decibel_from_logarithmic(log6).unwrap(), 0.0);
		// logarithmic_to_linear clamps >0.99 to 1.
		assert_eq!(decibel_logarithmic_to_linear(0.999).unwrap(), 1.0);
	}

	#[test]
	fn lerp_matches_cpp() {
		assert_eq!(lerp(0.0, 10.0, 0.0).unwrap(), 0.0);
		assert_eq!(lerp(0.0, 10.0, 1.0).unwrap(), 10.0);
		assert_eq!(lerp(0.0, 10.0, 0.5).unwrap(), 5.0);
		assert_eq!(lerp(2.0, 4.0, 0.25).unwrap(), 2.5);
	}

	#[test]
	fn loop_mode_discriminants_match_cpp() {
		// Values are load-bearing across the C ABI (include/common/loopmode.h
		// and src/common/src/loopmode.h).
		assert_eq!(LoopMode::Off as i32, 0);
		assert_eq!(LoopMode::Loop as i32, 1);
		assert_eq!(LoopMode::Clamp as i32, 2);
		// Copy/Clone/Eq semantics of a plain enum.
		let a = LoopMode::Loop;
		let b = a;
		assert_eq!(a, b);
		assert_ne!(LoopMode::Off, LoopMode::Clamp);
	}

	#[test]
	fn drop_workflow_behavior_discriminants_match_cpp() {
		// The config layer persists these as ints (enum OakDropWorkflowBehavior).
		assert_eq!(DropWorkflowBehavior::Ask as i32, 0);
		assert_eq!(DropWorkflowBehavior::Auto as i32, 1);
		assert_eq!(DropWorkflowBehavior::Manual as i32, 2);
		assert_eq!(DropWorkflowBehavior::Disable as i32, 3);
	}

	#[test]
	fn drop_workflow_behavior() {
		for (v, expected_valid) in [(0, true), (1, true), (2, true), (3, true)] {
			assert_eq!(DropWorkflowBehavior::is_valid(v), expected_valid);
		}
		assert!(!DropWorkflowBehavior::is_valid(-1));
		assert!(!DropWorkflowBehavior::is_valid(4));

		assert_eq!(DropWorkflowBehavior::name(0), "ASK");
		assert_eq!(DropWorkflowBehavior::name(1), "AUTO");
		assert_eq!(DropWorkflowBehavior::name(2), "MANUAL");
		assert_eq!(DropWorkflowBehavior::name(3), "DISABLE");
		assert_eq!(DropWorkflowBehavior::name(99), "UNKNOWN");
		assert_eq!(DropWorkflowBehavior::name(-5), "UNKNOWN");
	}

	#[test]
	fn power_of_two() {
		// ceil
		assert_eq!(power_ceil_to_power_of_2(1).unwrap(), 1);
		assert_eq!(power_ceil_to_power_of_2(2).unwrap(), 2);
		assert_eq!(power_ceil_to_power_of_2(3).unwrap(), 4);
		assert_eq!(power_ceil_to_power_of_2(5).unwrap(), 8);
		assert_eq!(power_ceil_to_power_of_2(8).unwrap(), 8);
		assert_eq!(power_ceil_to_power_of_2(0).unwrap(), 0);
		// floor
		assert_eq!(power_floor_to_power_of_2(1).unwrap(), 1);
		assert_eq!(power_floor_to_power_of_2(2).unwrap(), 2);
		assert_eq!(power_floor_to_power_of_2(5).unwrap(), 4);
		assert_eq!(power_floor_to_power_of_2(9).unwrap(), 8);
		assert_eq!(power_floor_to_power_of_2(8).unwrap(), 8);
		assert_eq!(power_floor_to_power_of_2(0).unwrap(), 0);
		// large value: 0x8000_0001 saturates the bit-blast to u32::MAX, then
		// the final increment wraps to 0 (matching C++ unsigned overflow).
		assert_eq!(power_ceil_to_power_of_2(0x8000_0001).unwrap(), 0u32);
		assert_eq!(power_floor_to_power_of_2(0x8000_0000).unwrap(), 0x8000_0000);
	}

	/// Per-process destroy counter used by the Current singleton test.
	static DESTROY_COUNT: AtomicUsize = AtomicUsize::new(0);

	/// Serialises tests that touch the process-wide `Current` singleton
	/// (cargo runs tests on threads).
	static CURRENT_LOCK: Mutex<()> = Mutex::new(());

	/// A `DestroyFn` that bumps [`DESTROY_COUNT`].
	unsafe extern "C" fn count_destroy(_p: *mut c_void) {
		DESTROY_COUNT.fetch_add(1, Ordering::SeqCst);
	}

	#[test]
	fn current_slot_semantics() {
		let _guard = CURRENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		// Only the video-params slot is used here; the singleton is shared
		// across tests, so keep each test on its own slot to avoid races.
		let cur = Current::instance();
		assert!(cur.is_interactive().unwrap());

		DESTROY_COUNT.store(0, Ordering::SeqCst);

		// Empty initially.
		assert!(cur.get_video_params().unwrap().is_null());

		// Store with a destructor.
		let p1 = 0x1 as *mut c_void;
		assert!(cur.set_video_params(p1, Some(count_destroy)).is_ok());
		assert_eq!(cur.get_video_params().unwrap(), p1);

		// Replacing destroys the previous occupant.
		let p2 = 0x2 as *mut c_void;
		assert!(cur.set_video_params(p2, Some(count_destroy)).is_ok());
		assert_eq!(cur.get_video_params().unwrap(), p2);
		assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 1);

		// Storing NULL clears and destroys the prior occupant.
		assert!(cur.set_video_params(std::ptr::null_mut(), None).is_ok());
		assert!(cur.get_video_params().unwrap().is_null());
		assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 2);

		// Clearing when the slot is already empty does nothing.
		assert!(cur.set_video_params(std::ptr::null_mut(), None).is_ok());
		assert_eq!(DESTROY_COUNT.load(Ordering::SeqCst), 2);
	}

	#[test]
	fn current_slots_independent() {
		let _guard = CURRENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
		let cur = Current::instance();
		let pa = 0x10 as *mut c_void;
		let ph = 0x20 as *mut c_void;
		let pc = 0x30 as *mut c_void;
		// Each slot is independent; no cross-slot interference.
		assert!(cur.set_audio_params(pa, None).is_ok());
		assert!(cur.set_plugin_host(ph, None).is_ok());
		assert!(cur.set_plugin_cache(pc, None).is_ok());
		assert_eq!(cur.get_audio_params().unwrap(), pa);
		assert_eq!(cur.get_plugin_host().unwrap(), ph);
		assert_eq!(cur.get_plugin_cache().unwrap(), pc);
	}
}
