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

//! Display color management: the display mapping applied to viewer frames
//! at present time, and — critically — WHO performs it.
//!
//! ## The single-mapping rule
//!
//! The frame content leaves the pipeline in a colorimetrically defined
//! space (the project's output target; sRGB by default). The final mapping
//! to the physical display must happen EXACTLY ONCE — either the OS does it
//! (color-managed compositors / ColorSync / DWM-ACM) or the app does it
//! (an OCIO chain through the display ICC). Both at once doubles the
//! correction; neither leaves wide-gamut displays wrong.
//!
//! [`display_policy`] makes that choice per platform:
//!
//! * **Wayland** — always [`DisplayPolicy::OsManaged`]. A Wayland
//!   compositor owns the display mapping (color-management-v1 declares the
//!   content as sRGB; compositors without the protocol assume sRGB, which
//!   the content is). Self-applying the display ICC here would double-map
//!   on every color-managed compositor (KWin 6.2+, GNOME 50+…).
//! * **Windows** — [`DisplayPolicy::OsManaged`] when Windows 11 Auto Color
//!   Management is active (it maps the sRGB-declared swapchain to the
//!   display), otherwise [`DisplayPolicy::SelfManaged`]: older Windows has
//!   no per-app OS mapping, so the app applies the ICM profile itself.
//! * **macOS** — [`DisplayPolicy::OsManaged`] by default: ColorSync maps
//!   the content (named via the layer's content colorspace) to the
//!   display. An explicit `DisplayColorMode=icc` preference hands the
//!   mapping to the app (the layer colorspace tag then makes the OS path
//!   a pass-through).
//! * **X11** — the user's preference ([`CONFIG_KEY_COLOR_MODE`]): X11 has
//!   no compositor mapping, so self-management is the only way to honor
//!   wide-gamut displays there (default self-managed).
//!
//! `OAK_DISPLAY_POLICY=self|os` overrides the platform decision for
//! debugging.
//!
//! ## The content space
//!
//! The chain starts from the project's output spec
//! ([`oak_core::color::pipeline_output_spec`]): sRGB content runs
//! through the named sRGB/Rec.709 space of the active OCIO config, and
//! non-sRGB content (P3/BT.2020 gamuts, PQ/HLG transfers) is converted to
//! CIE XYZ (D65, unit luminance) first and flows through the ICC's
//! connection space (the `cie_xyz_d65_interchange` chain of
//! [`ColorProcessor::create_display_icc_xyz`]). A non-empty
//! [`CONFIG_KEY_CONTENT_SPACE`] overrides the spec with an explicit OCIO
//! colorspace name.
//!
//! When the policy is OS-managed this module transforms nothing and the
//! platform layer declares the content colorspace instead (macOS layer
//! colorspace, Windows `SetColorSpace1`, Wayland color-management-v1),
//! re-declared whenever the project output spec changes.
//!
//! ## Multi-monitor tracking
//!
//! The display ICC depends on WHICH physical monitor the window is on, so
//! the effective key additionally carries the current monitor fingerprint.
//! [`poll_monitor`] runs on the app's tick loop and records the fingerprint
//! (throttled to one probe every 2s); [`note_monitor`] sets it directly
//! (tests, other callers). Fingerprints are opaque strings —
//! `"mac:<CGDirectDisplayID>"`, `"win:<device name>"`, `"x11:<RandR
//! output>"` — and the empty string means "no known monitor", which falls
//! back to the main-display profile. The `SelfManaged` policy keys the
//! transform chain per fingerprint, so dragging the window onto another
//! display re-resolves its ICC without touching the OS-managed paths.

use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use oak_core::colormath::{output_spec_to_xyz_d65, OutputColorSpec, OutputGamut, OutputTransfer};
use oak_core::configstore::ConfigStore;
use oak_core::color::{pipeline_output_spec, ColorProcessor};

/// Config key: the display color management mode ("icc" / "off"). The
/// preference only applies where the platform policy allows self-management
/// (macOS, X11, non-ACM Windows).
pub const CONFIG_KEY_COLOR_MODE: &str = "DisplayColorMode";
/// Config key: a custom ICC profile path (empty = the system display
/// profile).
pub const CONFIG_KEY_CUSTOM_ICC: &str = "DisplayColorCustomIcc";
/// Config key: the content colorspace the display chain starts from (an
/// OCIO colorspace name of the active config). Empty (the default) =
/// follow the project's output spec.
pub const CONFIG_KEY_CONTENT_SPACE: &str = "DisplayColorContentSpace";

/// The default content space (OCIO 2.2 builtin config name for
/// gamma-encoded Rec.709/sRGB display-referred content).
const DEFAULT_CONTENT_SPACE: &str = "sRGB Encoded Rec.709 (sRGB)";

/// Who performs the final mapping to the physical display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayPolicy {
	/// The app applies the display transform itself (the display ICC chain);
	/// the platform layer must be tagged so the OS passes pixels through.
	SelfManaged,
	/// The OS maps the (sRGB-declared) content to the display; the app must
	/// not apply any display transform.
	OsManaged,
}

/// The cached processor set for one (policy, mode, icc, content, monitor)
/// key: the sRGB-named chain for F32 samples, the XYZ (PCS) chain for
/// non-sRGB project output, and the BGRA8 variant of the sRGB chain — the
/// viewer's 8-bit slot always runs the sRGB chain, a degradation for
/// non-sRGB specs whose encoding 8-bit cannot honor (PQ/HLG highlights
/// clamp).
struct State {
	key: (DisplayPolicy, String, String, String, String),
	f32: Option<Arc<ColorProcessor>>,
	xyz: Option<Arc<ColorProcessor>>,
	bgra: Option<Arc<ColorProcessor>>,
}

static STATE: LazyLock<Mutex<Option<State>>> = LazyLock::new(|| Mutex::new(None));

/// Bumped every time the effective key changes (policy / mode / ICC path /
/// content space / monitor): the engine's frame caches compare against it
/// and drop images produced with a stale transform.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The current monitor fingerprint (see [`poll_monitor`]): `None` (the
/// default) means the window's monitor is unknown and the main-display
/// profile is used. Updated only on a successful probe — a failed probe
/// keeps the last known monitor rather than degrading mid-session.
static CURRENT_MONITOR: Mutex<Option<String>> = Mutex::new(None);

/// When [`poll_monitor`] last probed the monitor; the first call always
/// probes immediately, later ones at most every [`MONITOR_PROBE_INTERVAL`].
static LAST_MONITOR_PROBE: Mutex<Option<Instant>> = Mutex::new(None);

/// Minimum time between two monitor probes in [`poll_monitor`].
const MONITOR_PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// The current transform generation (see [`GENERATION`]).
pub fn generation() -> u64 {
	GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// True when the session runs on a Wayland compositor (the same probe the
/// platform backend uses, `gpui::guess_compositor`).
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn on_wayland() -> bool {
	gpui::guess_compositor() == "Wayland"
}

/// The platform policy decision for this session (see the module docs).
/// Stable for the process lifetime except the macOS/X11 preference, which
/// the config key governs at runtime.
pub fn display_policy() -> DisplayPolicy {
	if let Ok(override_mode) = std::env::var("OAK_DISPLAY_POLICY") {
		match override_mode.to_ascii_lowercase().as_str() {
			"os" => return DisplayPolicy::OsManaged,
			"self" => return DisplayPolicy::SelfManaged,
			other => {
				log::warn!(
					"OAK_DISPLAY_POLICY={other:?} ignored (expected \"os\" or \"self\")"
				);
			}
		}
	}

	// Wayland: the compositor owns the display mapping; declaring sRGB
	// content (color-management-v1) is the only correct behavior.
	#[cfg(any(target_os = "linux", target_os = "freebsd"))]
	if on_wayland() {
		return DisplayPolicy::OsManaged;
	}

	// Windows: Auto Color Management (Windows 11) maps the sRGB-declared
	// swapchain for us — self-applying the ICM profile would double-map.
	// Without ACM (Windows 10 / ACM off) no per-app OS mapping exists, so
	// the app applies the profile itself. ACM's swapchain mapping is SDR
	// sRGB; a non-sRGB project output can't be honored through it — warn
	// once so the degradation is visible in the log.
	#[cfg(target_os = "windows")]
	if oak_core::displayicc::windows_acm_active() {
		if pipeline_output_spec() != OutputColorSpec::default() {
			static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
			WARNED.get_or_init(|| {
				log::warn!(
					"Windows ACM: project output {:?} is not sRGB; ACM maps the sRGB swapchain only, \
					 non-sRGB previews will not match the project colorspace",
					pipeline_output_spec()
				);
			});
		}
		return DisplayPolicy::OsManaged;
	}

	// macOS: ColorSync owns the display mapping by default; only an
	// explicit "icc" preference hands it to the app (the layer colorspace
	// tag then turns the OS path into a pass-through). X11 has no OS
	// mapping, so self-management is the only way to honor wide-gamut
	// displays there (the "off" preference opts back into OS-management).
	#[cfg(target_os = "macos")]
	match ConfigStore::instance().get(None, CONFIG_KEY_COLOR_MODE).as_deref() {
		Ok("icc") => DisplayPolicy::SelfManaged,
		_ => DisplayPolicy::OsManaged,
	}
	#[cfg(not(target_os = "macos"))]
	match configured_mode().as_str() {
		"off" => DisplayPolicy::OsManaged,
		_ => DisplayPolicy::SelfManaged,
	}
}

/// The persisted mode preference ("icc" = self-manage, "off" = OS-managed).
fn configured_mode() -> String {
	ConfigStore::instance()
		.get(None, CONFIG_KEY_COLOR_MODE)
		.unwrap_or_else(|_| "icc".to_string())
}

/// Drop the cached processors (call after a preference change).
pub fn invalidate() {
	*STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Record the window's current monitor fingerprint (see [`poll_monitor`]).
/// `None` clears the tracking back to the main-display lookup.
pub fn note_monitor(id: Option<String>) {
	*CURRENT_MONITOR.lock().unwrap_or_else(|e| e.into_inner()) = id;
}

/// Track which physical monitor the window sits on, so the self-managed
/// display transform follows window moves between displays.
///
/// Runs on the app's tick loop. No-op unless the policy is [`DisplayPolicy::SelfManaged`]
/// (an OS-managed session never consults the display ICC). Probes are
/// throttled to one per [`MONITOR_PROBE_INTERVAL`] — the first call probes
/// immediately — and only a successful probe updates the tracked monitor:
/// a transient failure keeps the last known one rather than falling back
/// mid-session.
pub fn poll_monitor(window: &gpui::Window, cx: &gpui::App) {
	if display_policy() != DisplayPolicy::SelfManaged {
		return;
	}
	{
		let mut last = LAST_MONITOR_PROBE.lock().unwrap_or_else(|e| e.into_inner());
		if let Some(stamp) = *last {
			if stamp.elapsed() < MONITOR_PROBE_INTERVAL {
				return;
			}
		}
		*last = Some(Instant::now());
	}
	if let Some(fingerprint) = current_monitor_fingerprint(window, cx) {
		note_monitor(Some(fingerprint));
	}
}

/// The current monitor of `window` as a fingerprint string (see the module
/// docs), or `None` when the platform cannot resolve one. The fingerprint
/// is derived from the window's platform display: macOS and Windows use the
/// gpui display id of the window's current screen (`window.display`), Linux
/// resolves the RandR output whose geometry covers the window center
/// (window bounds are logical pixels there; multiplied by the scale factor
/// they become device pixels).
fn current_monitor_fingerprint(window: &gpui::Window, cx: &gpui::App) -> Option<String> {
	#[cfg(target_os = "macos")]
	{
		let id = u64::from(window.display(cx)?.id());
		return Some(format!("mac:{id}"));
	}
	#[cfg(target_os = "windows")]
	{
		let id = u64::from(window.display(cx)?.id());
		return oak_core::displayicc::windows_monitor_fingerprint(id);
	}
	#[cfg(target_os = "linux")]
	{
		let _ = cx;
		let center = window.bounds().center();
		let x = f64::from(center.x) * f64::from(window.scale_factor());
		let y = f64::from(center.y) * f64::from(window.scale_factor());
		return oak_core::displayicc::x11_monitor_fingerprint_at(x, y);
	}
	#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
	{
		let _ = (window, cx);
		None
	}
}

/// The active (policy, mode, icc, content, monitor) key.
fn current_key() -> (DisplayPolicy, String, String, String, String) {
	let policy = display_policy();
	let custom = ConfigStore::instance()
		.get(None, CONFIG_KEY_CUSTOM_ICC)
		.unwrap_or_default();
	// The content space: a non-empty override (an explicit OCIO colorspace
	// name) wins; otherwise the chain follows the project's output spec —
	// its (gamut, transfer) pair rekeys (and so rebuilds) the chain, so a
	// project settings change flows through the same GENERATION machinery
	// as a preference change.
	let space = ConfigStore::instance()
		.get(None, CONFIG_KEY_CONTENT_SPACE)
		.unwrap_or_default();
	let space = if space.is_empty() {
		let spec = pipeline_output_spec();
		format!("{}:{}", spec.gamut.as_setting(), spec.transfer.as_setting())
	} else {
		format!("override:{space}")
	};
	// The monitor fingerprint the window currently sits on; empty = unknown
	// (falls back to the main display).
	let monitor = CURRENT_MONITOR
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
		.unwrap_or_default();
	(policy, configured_mode(), custom, space, monitor)
}

/// The cached state, (re)built when the key changed.
fn current() -> Option<State> {
	let key = current_key();
	let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
	if let Some(state) = guard.as_ref() {
		if state.key == key {
			return clone_state(state);
		}
	}
	// The key changed: everything rendered with the old transform is
	// stale — bump the generation so frame caches drop their contents.
	// The FIRST build (no prior state) must not bump: no frame can be
	// staler than a transform that did not exist yet, and a spurious
	// bump would drop the frames being cached right now.
	if guard.is_some() {
		GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	}
	let (policy, mode, icc_path, space, monitor) = &key;
	// OS-managed: the compositor/ColorSync/DWM performs the display
	// mapping; applying anything here would be a second correction.
	if *policy == DisplayPolicy::OsManaged || mode != "icc" {
		let state = State {
			key,
			f32: None,
			xyz: None,
			bgra: None,
		};
		let out = clone_state(&state);
		*guard = Some(state);
		return out;
	}
	// Self-managed: content (in `space`) through the display ICC. The
	// custom override wins; empty = the current monitor's profile (a known
	// fingerprint resolves per-monitor, anything else the main display).
	let icc = if icc_path.is_empty() {
		monitor_icc_path(monitor)
	} else {
		Some(icc_path.clone())
	};
	let (f32p, xyzp, bgrap) = match icc {
		// Explicit content-space override: the whole chain starts from
		// that OCIO colorspace name (both slots).
		Some(path) if space.starts_with("override:") => {
			let name = space.trim_start_matches("override:");
			(
				ColorProcessor::create_display_icc(name, &path).map(Arc::new),
				None,
				ColorProcessor::create_display_icc_bgra8(name, &path).map(Arc::new),
			)
		}
		// Project output spec: sRGB content through the named sRGB space;
		// non-sRGB content (P3/BT.2020 gamuts, PQ/HLG transfers) has no
		// named space in the builtin OCIO configs, so the F32 chain
		// converts it to CIE XYZ (PCS) first and runs the ICC half — the
		// sRGB chain stays as the degradation fallback and for the 8-bit
		// slot (whose encoding cannot honor non-sRGB specs).
		Some(path) => {
			let xyz = if pipeline_output_spec() != OutputColorSpec::default() {
				ColorProcessor::create_display_icc_xyz(&path).map(Arc::new)
			} else {
				None
			};
			(
				ColorProcessor::create_display_icc(DEFAULT_CONTENT_SPACE, &path).map(Arc::new),
				xyz,
				ColorProcessor::create_display_icc_bgra8(DEFAULT_CONTENT_SPACE, &path)
					.map(Arc::new),
			)
		}
		None => (None, None, None),
	};
	let state = State {
		key,
		f32: f32p,
		xyz: xyzp,
		bgra: bgrap,
	};
	let out = clone_state(&state);
	*guard = Some(state);
	out
}

/// The display ICC path for the current monitor fingerprint: a known
/// fingerprint resolves per-monitor (with a fallback to the main display
/// inside `system_display_icc_for`); an unknown one (empty or malformed)
/// falls back to the main-display profile — the pre-multi-monitor behavior.
fn monitor_icc_path(monitor: &str) -> Option<String> {
	match oak_core::displayicc::monitor_ref_from_fingerprint(monitor) {
		Some(monitor) => oak_core::displayicc::system_display_icc_for(&monitor),
		None => oak_core::displayicc::system_display_icc(),
	}
}

fn clone_state(state: &State) -> Option<State> {
	Some(State {
		key: state.key.clone(),
		f32: state.f32.clone(),
		xyz: state.xyz.clone(),
		bgra: state.bgra.clone(),
	})
}

/// Whether the app is self-applying a display transform right now (a valid
/// ICC processor under the SelfManaged policy). When false the OS owns the
/// output mapping and the platform layer must declare the content space.
pub fn is_active() -> bool {
	if display_policy() != DisplayPolicy::SelfManaged {
		return false;
	}
	current()
		.map(|s| s.f32.is_some() || s.xyz.is_some() || s.bgra.is_some())
		.unwrap_or(false)
}

/// Declare the current policy to the window's platform layer (macOS: the
/// Metal layer colorspace tag; other platforms handle it at the surface
/// level or ignore it). Call once per window at creation, after any
/// preference change that flips [`is_active`], and after the project
/// output spec changes (which rekeys the content-colorspace declaration).
pub fn apply_to_window(window: &mut gpui::Window) {
	let active = is_active();
	let mode = if active {
		gpui::LayerColorManagement::SelfManaged
	} else {
		gpui::LayerColorManagement::OsManaged
	};
	window.set_layer_color_management(mode);
	// OS-managed: name the content colorspace so the OS maps from it
	// (ColorSync, DWM-ACM, color-management-v1). The self-managed path is
	// tagged pass-through, so the declaration is meaningless there.
	if !active {
		window.set_content_colorspace(content_colorspace());
	}
}

/// The gpui content-colorspace declaration for the current project output
/// spec (only meaningful while the OS performs the display mapping).
fn content_colorspace() -> gpui::WindowContentColorspace {
	let spec = pipeline_output_spec();
	gpui::WindowContentColorspace {
		primaries: match spec.gamut {
			OutputGamut::Srgb => gpui::ContentPrimaries::Srgb,
			OutputGamut::DisplayP3 => gpui::ContentPrimaries::DisplayP3,
			OutputGamut::Bt2020 => gpui::ContentPrimaries::Bt2020,
		},
		transfer: match spec.transfer {
			OutputTransfer::Srgb => gpui::ContentTransfer::Srgb,
			OutputTransfer::Gamma22 => gpui::ContentTransfer::Gamma22,
			OutputTransfer::Pq => gpui::ContentTransfer::Pq,
			OutputTransfer::Hlg => gpui::ContentTransfer::Hlg,
		},
	}
}

/// Apply the display transform to an F32 RGBA buffer in place (no-op
/// when inactive).
pub fn apply_f32_rgba(samples: &mut [f32], pixels: i64) {
	let Some(state) = current() else {
		return;
	};
	if let Some(xyz) = &state.xyz {
		// Non-sRGB project output: the samples are in the output spec's
		// encoded form — linearize the transfer and gamut-map to CIE XYZ
		// (D65, unit luminance) first, then let the ICC chain map the
		// connection space to the display.
		output_spec_to_xyz_d65(samples, pipeline_output_spec());
		let _ = xyz.convert_f32_rgba(samples, pixels);
	} else if let Some(processor) = &state.f32 {
		let _ = processor.convert_f32_rgba(samples, pixels);
	}
}

/// Apply the display transform to a packed BGRA8 buffer in place (no-op
/// when inactive).
pub fn apply_bgra8(data: &mut [u8], pixels: i64) {
	let Some(state) = current() else {
		return;
	};
	if let Some(processor) = &state.bgra {
		let _ = processor.convert_bgra8(data, pixels);
	}
}
