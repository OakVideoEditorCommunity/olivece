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

//! The content views of the app's modal dialogs: preferences (the settings
//! panel mirroring the C++ tabbed preferences: general / rendering / cache
//! / proxy / project / audio) and export (format + output path).
//!
//! Each view owns its widgets and emits nothing itself — the host
//! (`crate::app::OakApp`) reads the state (format / path) when a dialog
//! button is clicked, and the preferences view writes its choices straight
//! into the oak_core config store on selection. Theme/language changes
//! additionally emit a [`PreferencesEvent`] so the host can re-apply the
//! shell chrome immediately.

use crate::oakui::component::controls::SliderModel;
use crate::oakui::component::controls::SliderValue;
use crate::oakui::component::controls::ValueKind;
use crate::oakui::component::controls::{CheckBox, CheckBoxEvent, CheckState};
use crate::oakui::component::controls::{ComboBox, ComboBoxEvent, ComboBoxOption};
use crate::oakui::component::controls::{SpinBox, SpinBoxEvent};
use crate::oakui::component::text_input;
use gpui::colors::DefaultColors;
use gpui::prelude::*;
use gpui::timeline::FrameRate;
use gpui::{
    div, px, App, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, Keystroke, PathPromptOptions, Render, SharedString, Window,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage, TextChanged};

use crate::actions::ActionId;
use crate::i18n;
use crate::oakui::real::{
    audio_input_device, audio_input_devices, audio_output_device, audio_output_devices,
    config_get_bool, config_get_int, config_get_string, config_set_bool, config_set_int,
    config_set_string, encoding_formats, proxy_dividers, renderer_backends, set_audio_input_device,
    set_audio_output_device, set_theme_dark, theme_is_dark, CONFIG_KEY_DEFAULT_TRANSITION_SEC,
    CONFIG_KEY_DISK_CACHE_PATH, CONFIG_KEY_FFMPEG_PATH, CONFIG_KEY_PG_URL,
    CONFIG_KEY_PREVIEW_WINDOW, CONFIG_KEY_PROXY_DIVIDER, CONFIG_KEY_RENDERER_BACKEND,
    CONFIG_KEY_SNAPSHOT_INTERVAL_SEC, CONFIG_KEY_STORAGE_BACKEND, CONFIG_KEY_USE_PROXY,
    DEFAULT_PREVIEW_WINDOW_FORWARD, DEFAULT_SNAPSHOT_INTERVAL_SEC, DEFAULT_TRANSITION_SEC,
    EXPORT_FORMAT_MP4,
};
// The `DisplayBitDepth` config key lives with the format mapping it
// drives (oak-render's backend); the preferences dropdown and the
// window-layer consumer share the same key.
use oak_core::backend::CONFIG_KEY_DISPLAY_BIT_DEPTH;

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

/// A request the preferences dialog emits for the host shell (the settings
/// themselves are written into the config store directly; these need
/// shell chrome — the menu bar / theme / key map — to re-render).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferencesEvent {
	/// The theme dropdown changed (the payload is the new dark flag).
	ThemeChanged(bool),
	/// The language dropdown changed (already applied to the i18n global).
	LanguageChanged,
	/// The custom shortcut overrides changed (the Keyboard tab); the host
	/// must re-bind the global key map and rebuild the menu bar so the new
	/// keys take effect immediately.
	ShortcutsChanged,
	/// The display color-management mode changed (the host re-evaluates the
	/// platform policy and retags the windows immediately — no restart).
	DisplayColorChanged,
}

impl gpui::EventEmitter<PreferencesEvent> for PreferencesContent {}

/// The preferences dialog content, mirroring the C++ tabbed preferences as
/// one grouped panel:
///
/// * **常规 General** — language, theme.
/// * **渲染 Rendering** — the renderer backend.
/// * **缓存 Cache** — the disk cache directory (`DiskCachePath`) and the
///   playback pre-render window (`PlaybackPreRenderFrames`, the "cache
///   ahead" frames the preview scheduler fills during playback).
/// * **存储 Storage** — the write-through library backend
///   (`Storage/Backend`, SQLite / PostgreSQL) and the PostgreSQL
///   connection string (`Storage/PgUrl`).
/// * **代理 Proxy** — use proxy media (`UseProxyMedia`), proxy resolution
///   divider (`ProxyDivider`).
/// * **项目 Project** — the snapshot interval (`Storage/SnapshotIntervalSec`,
///   the write-through era's auto-save interval) and the default transition
///   length (`DefaultTransitionLength`).
/// * **音频 Audio** — the output / input devices (`AudioOutput` /
///   `AudioInput`, applied live through the oakaudio manager).
///
/// Every row writes into the config store on selection, so the choices
/// survive restarts (the app loads the config at startup and saves it on
/// exit).
pub struct PreferencesContent {
	backend: Entity<ComboBox>,
	/// The on-screen display bit depth (10-bit default, 8-bit fallback).
	/// The swapchain format is fixed at surface creation, so a change
	/// takes effect after a restart.
	display_bit_depth: Entity<ComboBox>,
	language: Entity<ComboBox>,
	theme: Entity<ComboBox>,
	cache_dir: Entity<PathField>,
	cache_ahead: Entity<SpinBox>,
	use_proxy: Entity<CheckBox>,
	hw_decode: Entity<CheckBox>,
	proxy_divider: Entity<ComboBox>,
	display_icc: Entity<CheckBox>,
	display_icc_path: Entity<PathField>,
	snapshot_interval: Entity<SpinBox>,
	transition_length: Entity<SpinBox>,
	audio_output: Entity<ComboBox>,
	audio_input: Entity<ComboBox>,
	storage_backend: Entity<ComboBox>,
	storage_pg_url: Entity<PathField>,
	/// Whether the write-through library uses PostgreSQL (drives the
	/// visibility of the connection-string field).
	storage_is_pg: bool,
	/// The backend options, in display order.
	backends: Vec<&'static str>,
	/// The proxy divider options, in display order (1 = full resolution).
	dividers: Vec<i64>,
	/// The output device names (dropdown order; index 0 is system default).
	output_devices: Vec<String>,
	/// The input device names (dropdown order; index 0 is system default).
	input_devices: Vec<String>,
}

impl PreferencesContent {
	/// Builds the content: reads the current config values and seeds every
	/// widget.
	pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
		// --- 渲染 Rendering: the renderer backend --------------------------
		let backends = renderer_backends();
		let current_backend = config_get_string(CONFIG_KEY_RENDERER_BACKEND);
		let backend_selected = backends
			.iter()
			.position(|b| b.eq_ignore_ascii_case(&current_backend))
			.unwrap_or(0);
		let backend_options = backends
			.iter()
			.enumerate()
			.map(|(i, name)| ComboBoxOption::new(i, backend_label(name)))
			.collect();
		let backend = cx.new(|cx| {
			ComboBox::new(1, backend_options, window, cx)
				.with_placeholder(i18n::tr("preferences.backend.placeholder"))
		});
		cx.subscribe(&backend, |this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				if let Some(name) = this.backends.get(*value) {
					config_set_string(CONFIG_KEY_RENDERER_BACKEND, name);
					println!("[preferences] renderer backend → {name}");
				}
			}
			let _ = cx;
		})
		.detach();
		backend.update(cx, |combo, cx| {
			combo.set_selected(Some(backend_selected), cx)
		});

		// --- 渲染 Rendering: on-screen display bit depth ---------------------
		// 10-bit is the default; 8-bit is the compatibility fallback. The
		// swapchain format is chosen once at window/surface creation, so a
		// change takes effect after a restart (see oak-render's
		// `DisplayBitDepth::present_formats`).
		let display_bit_depth = cx.new(|cx| {
			let options = vec![
				ComboBoxOption::new(0, i18n::tr("preferences.bit_depth.10bit")),
				ComboBoxOption::new(1, i18n::tr("preferences.bit_depth.8bit")),
			];
			ComboBox::new(12, options, window, cx)
		});
		cx.subscribe(&display_bit_depth, |_this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				let depth = if *value == 1 { "8" } else { "10" };
				config_set_string(CONFIG_KEY_DISPLAY_BIT_DEPTH, depth);
				println!("[preferences] display bit depth → {depth}");
			}
			let _ = cx;
		})
		.detach();
		let bit_depth_selected =
			if config_get_string(CONFIG_KEY_DISPLAY_BIT_DEPTH) == "8" {
				1
			} else {
				0
			};
		display_bit_depth.update(cx, |combo, cx| {
			combo.set_selected(Some(bit_depth_selected), cx)
		});

		// --- 常规 General: language + theme --------------------------------
		// Data-driven: one option per discovered language pack, labelled
		// with the pack's own `language.name` — a newly dropped-in pack
		// appears here with no code change.
		let languages = i18n::available_languages();
		let language_options: Vec<_> = languages
			.iter()
			.enumerate()
			.map(|(index, code)| {
				ComboBoxOption::new(index, format!("{} ({code})", i18n::pack_native_name(code)))
			})
			.collect();
		let language = cx.new(|cx| {
			ComboBox::new(2, language_options, window, cx)
				.with_placeholder(i18n::tr("preferences.language.placeholder"))
		});
		let language_selected = languages
			.iter()
			.position(|code| *code == i18n::language_code())
			.unwrap_or(0);
		cx.subscribe(
			&language,
			move |_this, _combo, event: &ComboBoxEvent, cx| {
				if let ComboBoxEvent::Selected { value, .. } = event {
					if let Some(code) = languages.get(*value) {
						crate::i18n::set_language_code(code);
						cx.emit(PreferencesEvent::LanguageChanged);
					}
				}
			},
		)
		.detach();
		language.update(cx, |combo, cx| {
			combo.set_selected(Some(language_selected), cx)
		});

		let theme_options = vec![
			ComboBoxOption::new(0, i18n::tr("preferences.theme.dark")),
			ComboBoxOption::new(1, i18n::tr("preferences.theme.light")),
		];
		let theme = cx.new(|cx| ComboBox::new(3, theme_options, window, cx));
		cx.subscribe(&theme, |_this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				let dark = *value == 0;
				set_theme_dark(dark);
				cx.emit(PreferencesEvent::ThemeChanged(dark));
			}
		})
		.detach();
		theme.update(cx, |combo, cx| {
			combo.set_selected(Some(if theme_is_dark() { 0 } else { 1 }), cx)
		});

		// --- 缓存 Cache: the disk cache directory --------------------------
		let cache_dir = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});
		let configured_cache = config_get_string(CONFIG_KEY_DISK_CACHE_PATH);
		cache_dir.update(cx, |field, cx| field.set_path(configured_cache, cx));

		// --- 缓存 Cache: the playback pre-render window (cache ahead) ------
		// The number of frames the preview scheduler fills ahead of the
		// playhead during playback (`PlaybackPreRenderFrames`, consumed by
		// `update_preview_window` every playback tick — no restart needed).
		let cache_ahead = cx.new(|cx| {
			let current = config_get_int(
				CONFIG_KEY_PREVIEW_WINDOW,
				DEFAULT_PREVIEW_WINDOW_FORWARD,
			)
			.clamp(8, 1200);
			SpinBox::new(
				10,
				SliderModel::new(ValueKind::Integer, 8.0, 1200.0, 8.0, current as f64),
				window,
				cx,
			)
		});
		cx.subscribe(&cache_ahead, |_this, _spin, event: &SpinBoxEvent, cx| {
			let value = match event {
				SpinBoxEvent::ValueChanged { value, .. }
				| SpinBoxEvent::EditCommitted { value, .. } => value.to_f64() as i64,
			};
			config_set_int(CONFIG_KEY_PREVIEW_WINDOW, value);
			let _ = cx;
		})
		.detach();

		// --- 代理 Proxy -----------------------------------------------------
		let use_proxy = cx.new(|cx| {
			CheckBox::new(
				7,
				if config_get_bool(CONFIG_KEY_USE_PROXY, true) {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("preferences.proxy.enable"))
		});
		cx.subscribe(&use_proxy, |_this, check, event: &CheckBoxEvent, cx| {
			if let CheckBoxEvent::Toggled { state, .. } = event {
				let enabled = *state == CheckState::Checked;
				config_set_bool(CONFIG_KEY_USE_PROXY, enabled);
				check.update(cx, |check, cx| check.set_state(*state, cx));
			}
		})
		.detach();

		let dividers = proxy_dividers();
		let divider_options = dividers
			.iter()
			.enumerate()
			.map(|(i, d)| {
				if *d <= 1 {
					ComboBoxOption::new(i, i18n::tr("preferences.proxy.full"))
				} else {
					ComboBoxOption::new(i, format!("1/{d}"))
				}
			})
			.collect();
		let proxy_divider = cx.new(|cx| ComboBox::new(4, divider_options, window, cx));
		cx.subscribe(&proxy_divider, |this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				if let Some(divider) = this.dividers.get(*value) {
					config_set_int(CONFIG_KEY_PROXY_DIVIDER, *divider);
				}
			}
			let _ = cx;
		})
		.detach();
		let current_divider = config_get_int(CONFIG_KEY_PROXY_DIVIDER, 1);
		let divider_selected = dividers
			.iter()
			.position(|d| *d == current_divider)
			.unwrap_or(0);
		proxy_divider.update(cx, |combo, cx| {
			combo.set_selected(Some(divider_selected), cx)
		});

		// --- 渲染 Rendering: hardware decoding switch -------------------
		// Default ON (user mandate); off forces software decoding.
		let hw_decode = cx.new(|cx| {
			CheckBox::new(
				9,
				if config_get_bool("HardwareDecoding", true) {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("preferences.hwdecode.enable"))
		});
		cx.subscribe(&hw_decode, |_this, check, event: &CheckBoxEvent, cx| {
			if let CheckBoxEvent::Toggled { state, .. } = event {
				let enabled = *state == CheckState::Checked;
				config_set_bool("HardwareDecoding", enabled);
				check.update(cx, |check, cx| check.set_state(*state, cx));
			}
		})
		.detach();

        // --- 色彩 Color: display ICC color management -----------------------
        // On by default: the viewer frames are transformed through the
        // display's ICC profile (system profile, or a custom file below).
        // A mode change re-evaluates the platform display policy and
        // retags the windows immediately (no restart).
        use crate::oakui::displaycolor::{CONFIG_KEY_COLOR_MODE, CONFIG_KEY_CUSTOM_ICC};
        let display_icc = cx.new(|cx| {
			let mode = config_get_string(CONFIG_KEY_COLOR_MODE);
			CheckBox::new(
				13,
				if mode != "off" {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("preferences.color.enable"))
		});
		cx.subscribe(&display_icc, |_this, check, event: &CheckBoxEvent, cx| {
			if let CheckBoxEvent::Toggled { state, .. } = event {
				let enabled = *state == CheckState::Checked;
				config_set_string(CONFIG_KEY_COLOR_MODE, if enabled { "icc" } else { "off" });
				check.update(cx, |check, cx| check.set_state(*state, cx));
				// Drop the cached processors and tell the host to retag the
				// windows for the new policy.
				crate::oakui::displaycolor::invalidate();
				cx.emit(PreferencesEvent::DisplayColorChanged);
			}
		})
		.detach();
		let display_icc_path = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});
		let configured_icc = config_get_string(CONFIG_KEY_CUSTOM_ICC);
		display_icc_path.update(cx, |field, cx| field.set_path(configured_icc, cx));

		// --- 项目 Project: snapshot interval + default transition ----------
		let snapshot_interval = cx.new(|cx| {
			let current = config_get_int(
				CONFIG_KEY_SNAPSHOT_INTERVAL_SEC,
				DEFAULT_SNAPSHOT_INTERVAL_SEC,
			);
			SpinBox::new(
				8,
				SliderModel::new(ValueKind::Integer, 0.0, 86400.0, 10.0, current as f64),
				window,
				cx,
			)
		});
		cx.subscribe(
			&snapshot_interval,
			|_this, _spin, event: &SpinBoxEvent, cx| {
				let value = match event {
					SpinBoxEvent::ValueChanged { value, .. }
					| SpinBoxEvent::EditCommitted { value, .. } => value.to_f64() as i64,
					_ => return,
				};
				config_set_int(CONFIG_KEY_SNAPSHOT_INTERVAL_SEC, value);
				let _ = cx;
			},
		)
		.detach();

		let transition_length = cx.new(|cx| {
			let current = config_get_string(CONFIG_KEY_DEFAULT_TRANSITION_SEC)
				.parse::<f64>()
				.ok()
				.filter(|v| *v >= 0.0)
				.unwrap_or_else(|| DEFAULT_TRANSITION_SEC.parse().unwrap());
			SpinBox::new(
				9,
				SliderModel::new(ValueKind::Float, 0.0, 60.0, 0.5, current),
				window,
				cx,
			)
		});
		cx.subscribe(
			&transition_length,
			|_this, _spin, event: &SpinBoxEvent, cx| {
				let value = match event {
					SpinBoxEvent::ValueChanged { value, .. }
					| SpinBoxEvent::EditCommitted { value, .. } => value.to_f64(),
					_ => return,
				};
				config_set_string(CONFIG_KEY_DEFAULT_TRANSITION_SEC, &format!("{value}"));
				let _ = cx;
			},
		)
		.detach();

		// --- 音频 Audio: output / input devices -----------------------------
		// The enumeration reads the oakaudio manager even on the mock
		// engine; the config choice applies the moment the dropdown changes.
		let (audio_output, output_devices) = device_combo(5, true, window, cx);
		let (audio_input, input_devices) = device_combo(6, false, window, cx);
		cx.subscribe(&audio_output, |this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				// Option 0 is the system default; the devices start at 1.
				let name = value
					.checked_sub(1)
					.and_then(|i| this.output_devices.get(i))
					.cloned()
					.unwrap_or_default();
				set_audio_output_device(&name);
			}
			let _ = cx;
		})
		.detach();
		cx.subscribe(&audio_input, |this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value, .. } = event {
				let name = value
					.checked_sub(1)
					.and_then(|i| this.input_devices.get(i))
					.cloned()
					.unwrap_or_default();
				set_audio_input_device(&name);
			}
			let _ = cx;
		})
		.detach();

		// --- 存储 Storage: the write-through library backend ----------------
		// `Storage/Backend` + `Storage/PgUrl` are read by oakstorage's
		// write-through when a project binds (per-project library session),
		// so a change applies to projects opened afterwards.
		let storage_is_pg = config_get_string(CONFIG_KEY_STORAGE_BACKEND) == "pg";
		let storage_backend = cx.new(|cx| {
			let options = vec![
				ComboBoxOption::new(0, "SQLite"),
				ComboBoxOption::new(1, "PostgreSQL"),
			];
			ComboBox::new(11, options, window, cx)
		});
		cx.subscribe(&storage_backend, |this, _combo, event: &ComboBoxEvent, cx| {
			match *event {
				ComboBoxEvent::Selected { value } => {
					let backend = if value == 1 { "pg" } else { "sqlite" };
					this.storage_is_pg = backend == "pg";
					config_set_string(CONFIG_KEY_STORAGE_BACKEND, backend);
					// Re-render so the connection-string row shows/hides with
					// the selection.
					cx.notify();
				}
			}
		})
		.detach();
		storage_backend.update(cx, |combo, cx| {
			combo.set_selected(Some(if storage_is_pg { 1 } else { 0 }), cx)
		});
		let storage_pg_url = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});
		let configured_pg_url = config_get_string(CONFIG_KEY_PG_URL);
		storage_pg_url.update(cx, |field, cx| field.set_path(configured_pg_url, cx));

		Self {
			backend,
			display_bit_depth,
			language,
			theme,
			cache_dir,
			cache_ahead,
			use_proxy,
			hw_decode,
			proxy_divider,
			display_icc,
			display_icc_path,
			snapshot_interval,
			transition_length,
			audio_output,
			audio_input,
			storage_backend,
			storage_pg_url,
			storage_is_pg,
			backends,
			dividers,
			output_devices,
			input_devices,
		}
	}

	/// Commits the custom ICC path field to the config (called by the host
	/// when the dialog closes, like the cache directory).
	pub fn commit_display_icc_path(&self, cx: &App) {
		let path = self.display_icc_path.read(cx).path(cx).trim().to_string();
		config_set_string(crate::oakui::displaycolor::CONFIG_KEY_CUSTOM_ICC, &path);
	}

	/// Commits the PostgreSQL connection-string field to the config (called
	/// by the host when the dialog closes, like the cache directory).
	pub fn commit_storage_pg_url(&self, cx: &App) {
		let url = self.storage_pg_url.read(cx).path(cx).trim().to_string();
		config_set_string(CONFIG_KEY_PG_URL, &url);
	}

	/// Opens the platform file picker for a custom ICC profile.
	fn browse_display_icc(&mut self, cx: &mut Context<Self>) {
		let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
			files: true,
			directories: false,
			multiple: false,
			prompt: Some(i18n::tr("preferences.color.browse").into()),
			allowed_extensions: Vec::new(),
		});
		cx.spawn(async move |this, cx| {
			let Ok(Ok(Some(paths))) = receiver.await else {
				return;
			};
			let Some(path) = paths.first() else {
				return;
			};
			this.update(cx, |this, cx| {
				this.display_icc_path.update(cx, |field, cx| {
					field.set_path(path.to_string_lossy().into_owned(), cx)
				});
				cx.notify();
			});
		})
		.detach();
	}

	/// The cache directory currently entered.
	pub fn cache_dir(&self, cx: &App) -> SharedString {
		self.cache_dir.read(cx).path(cx)
	}

	/// Commits the cache directory field to the config (called by the host
	/// when the dialog closes, so a typed-but-unbrowsed path still lands).
	pub fn commit_cache_dir(&self, cx: &App) {
		let path = self.cache_dir(cx).trim().to_string();
		config_set_string(CONFIG_KEY_DISK_CACHE_PATH, &path);
	}

	/// Opens the platform directory picker and lands the choice in the cache
	/// directory field (committed with the field, on dialog close).
	fn browse_cache_dir(&mut self, cx: &mut Context<Self>) {
		let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
			files: false,
			directories: true,
			multiple: false,
			prompt: Some(i18n::tr("preferences.cache.browse").into()),
			allowed_extensions: Vec::new(),
		});
		cx.spawn(async move |this, cx| {
			let Ok(Ok(Some(paths))) = receiver.await else {
				return;
			};
			let Some(path) = paths.first() else {
				return;
			};
			this.update(cx, |this, cx| {
				this.cache_dir.update(cx, |field, cx| {
					field.set_path(path.to_string_lossy().into_owned(), cx)
				});
				cx.notify();
			});
		})
		.detach();
	}
}

/// Builds a device dropdown for the output (`output = true`) or input side:
/// option 0 is the system default, the rest are the enumerated devices, and
/// the current config value (validated against the enumeration) is
/// preselected. Returns the combo and the option→device-name list.
fn device_combo(
	control: usize,
	output: bool,
	window: &mut Window,
	cx: &mut Context<PreferencesContent>,
) -> (Entity<ComboBox>, Vec<String>) {
	let devices = if output {
		audio_output_devices()
	} else {
		audio_input_devices()
	};
	let current = if output {
		audio_output_device()
	} else {
		audio_input_device()
	};
	let mut options = vec![ComboBoxOption::new(
		0,
		i18n::tr("preferences.audio.default"),
	)];
	for (i, name) in devices.iter().enumerate() {
		options.push(ComboBoxOption::new(i + 1, name.clone()));
	}
	let selected = devices
		.iter()
		.position(|n| *n == current)
		.map(|i| i + 1)
		.unwrap_or(0);
	let placeholder = if output {
		i18n::tr("preferences.audio.output.placeholder")
	} else {
		i18n::tr("preferences.audio.input.placeholder")
	};
	let combo =
		cx.new(|cx| ComboBox::new(control, options, window, cx).with_placeholder(placeholder));
	combo.update(cx, |combo, cx| combo.set_selected(Some(selected), cx));
	(combo, devices)
}

/// A display label for a renderer backend id.
fn backend_label(name: &str) -> String {
	match name {
		"opengl" => "OpenGL",
		"metal" => "Metal",
		"vulkan" => "Vulkan",
		"none" => "None (off)",
		other => other,
	}
	.to_string()
}

/// A labeled form row: a small caption above the widget.
fn form_row(
	colors: &gpui::colors::Colors,
	label: SharedString,
	widget: impl IntoElement,
) -> gpui::Div {
	div()
		.flex()
		.flex_col()
		.gap_1()
		.child(div().text_color(colors.text).child(label))
		.child(widget)
}

/// A group header separating the preference sections.
fn section_header(colors: &gpui::colors::Colors, label: SharedString) -> gpui::Div {
	div()
		.pt_2()
		.text_color(colors.disabled)
		.text_xs()
		.child(label)
}

impl Render for PreferencesContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		// The full settings list is taller than a 900px window at the design
		// density, so the content scrolls inside the modal card.
		div()
			.id("preferences-content")
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.max_h(px(720.0))
			.overflow_y_scroll()
			// 常规 General
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.general").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.language").into(),
				self.language.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.theme").into(),
				self.theme.clone(),
			))
			// 渲染 Rendering
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.render").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.backend").into(),
				self.backend.clone(),
			))
			.child(self.hw_decode.clone())
			// 缓存 Cache
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.cache").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.cache.dir").into(),
				div()
					.flex()
					.gap_2()
					.child(div().flex_1().child(self.cache_dir.clone()))
					.child(
						div()
							.id("preferences-cache-browse")
							.px_3()
							.py_1()
							.rounded_md()
							.bg(colors.background)
							.border_1()
							.border_color(colors.border)
							.text_color(colors.text)
							.cursor_pointer()
							.child(i18n::tr("preferences.cache.browse"))
							.on_click(cx.listener(|this, _event, _window, cx| {
								this.browse_cache_dir(cx);
							})),
					),
			))
			// 缓存 Cache: cache ahead (playback pre-render window)
			.child(form_row(
				&colors,
				i18n::tr("preferences.cache.ahead").into(),
				div()
					.debug_selector(|| "preferences-cache-ahead".into())
					.child(self.cache_ahead.clone()),
			))
			// 存储 Storage
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.storage").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.storage.backend").into(),
				div()
					.debug_selector(|| "preferences-storage-backend".into())
					.child(self.storage_backend.clone()),
			))
			.child(if self.storage_is_pg {
				form_row(
					&colors,
					i18n::tr("preferences.storage.pg_url").into(),
					div()
						.debug_selector(|| "preferences-storage-pg-url".into())
						.child(self.storage_pg_url.clone()),
				)
			} else {
				div()
			})
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("preferences.storage.restart_hint")),
			)
			// 代理 Proxy
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.proxy").into(),
			))
			.child(self.use_proxy.clone())
			.child(form_row(
				&colors,
				i18n::tr("preferences.proxy.resolution").into(),
				self.proxy_divider.clone(),
			))
			// 色彩 Color
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.color").into(),
			))
			.child(self.display_icc.clone())
			.child(form_row(
				&colors,
				i18n::tr("preferences.color.custom").into(),
				div()
					.flex()
					.gap_2()
					.child(div().flex_1().child(self.display_icc_path.clone()))
					.child(
						div()
							.id("preferences-icc-browse")
							.px_3()
							.py_1()
							.rounded_md()
							.bg(colors.background)
							.border_1()
							.border_color(colors.border)
							.text_color(colors.text)
							.cursor_pointer()
							.child(i18n::tr("preferences.color.browse"))
							.on_click(cx.listener(|this, _event, _window, cx| {
								this.browse_display_icc(cx);
							})),
					),
			))
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("preferences.color.restart_hint")),
			)
			// 项目 Project
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.project").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.snapshot.interval").into(),
				self.snapshot_interval.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.transition.default").into(),
				self.transition_length.clone(),
			))
			// 音频 Audio
			.child(section_header(
				&colors,
				i18n::tr("preferences.section.audio").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.audio.output").into(),
				self.audio_output.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("preferences.audio.input").into(),
				self.audio_input.clone(),
			))
			// 渲染 Rendering: display bit depth (kept at the bottom of the list
			// so the rows above stay inside the card's scroll viewport).
			.child(form_row(
				&colors,
				i18n::tr("preferences.bit_depth.label").into(),
				self.display_bit_depth.clone(),
			))
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("preferences.bit_depth.restart_hint")),
			)
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("preferences.hint")),
			)
	}
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// A text field with the same shape as the file dialog's path field.
pub struct PathField {
	editor: Entity<EditableTextState>,
	/// Whether the field accepts input (disabled fields dim and drop the
	/// text-input handler, like the checkbox disabled state).
	enabled: bool,
}

impl PathField {
	/// The path currently entered.
	pub fn path(&self, app: &App) -> SharedString {
		self.editor.read(app).as_str().into()
	}

	/// Replaces the path shown in the field.
	pub fn set_path(&mut self, path: impl Into<SharedString>, cx: &mut Context<Self>) {
		let path = path.into();
		self.editor.update(cx, |editor, cx| {
			editor.emplace(path.as_ref(), cx);
		});
		cx.notify();
	}

	/// Whether the field accepts input (disabled fields are read-only and
	/// render dimmed).
	pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
		if self.enabled == enabled {
			return;
		}
		self.enabled = enabled;
		cx.notify();
	}

	/// The input state the field starts in (chainable after construction).
	pub fn with_enabled(mut self, enabled: bool) -> Self {
		self.enabled = enabled;
		self
	}
}

impl Render for PathField {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let weak = self.editor.downgrade();
		let enabled = self.enabled;
		div()
			.rounded_md()
			.border_1()
			.border_color(colors.border)
			.bg(colors.background)
			.px_2()
			.py_1()
			.opacity(if enabled { 1.0 } else { 0.45 })
			.child(
				text_input("gpui-widgets-export-path", cx)
					.state(weak)
					.accepts_input(enabled),
			)
	}
}

/// The export dialog content: the container-format dropdown and the output
/// path field.
pub struct ExportDialogContent {
	sequence: Entity<ComboBox>,
	format: Entity<ComboBox>,
	video_codec: Entity<ComboBox>,
	audio_codec: Entity<ComboBox>,
	color: Entity<ComboBox>,
	range: Entity<ComboBox>,
	resolution_w: Entity<SpinBox>,
	resolution_h: Entity<SpinBox>,
	frame_rate: Entity<SpinBox>,
	bitrate: Entity<SpinBox>,
	path: Entity<PathField>,
	/// (format id, display name, extension) in dropdown order.
	formats: Vec<(i32, String, String)>,
	/// The container format for the current codec lists.
	active_format: i32,
	/// (entry id, display name) of the pickable sequences, in dropdown
	/// order; populated by the host from the open project.
	sequences: Vec<(u64, SharedString)>,
}

impl ExportDialogContent {
	/// Builds the content: the format list comes from the oakcodec encoding
	/// enumeration (MP4 default), the codec lists follow the container's
	/// compatible tables (switching container rebuilds them — an
	/// unsupported codec is never offered), the color defaults to SDR
	/// Rec.709 8-bit, the range to the whole sequence.
	pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
		let formats: Vec<(i32, String, String)> = encoding_formats()
			.into_iter()
			.filter(|(_, _, ext)| !ext.is_empty())
			.collect();
		let options = formats
			.iter()
			.enumerate()
			.map(|(i, (_, name, ext))| ComboBoxOption::new(i, format!("{name} (.{ext})")))
			.collect();
		let format = cx.new(|cx| {
			ComboBox::new(3, options, window, cx)
				.with_placeholder(i18n::tr("export.format.placeholder"))
		});
		let mp4_index = formats
			.iter()
			.position(|(id, _, _)| *id == EXPORT_FORMAT_MP4)
			.unwrap_or(0);
		format.update(cx, |combo, cx| combo.set_selected(Some(mp4_index), cx));

		let active_format = formats
			.get(mp4_index)
			.map(|(id, _, _)| *id)
			.unwrap_or(EXPORT_FORMAT_MP4);

		// Video / audio codec combo boxes, populated by the container.
		// The lists come straight from `Format::get_video_codecs` /
		// `get_audio_codecs`: only codecs the container supports appear,
		// so an incompatible codec is unselectable by construction.
		let codec_options = |codes: Vec<i32>| {
			codes
				.iter()
				.enumerate()
				.map(|(i, code)| {
					ComboBoxOption::new(
						i,
						oak_codec::exportcodec::Codec::get_codec_name(
							oak_codec::exportcodec::Codec::from_i32(*code)
								.unwrap_or(oak_codec::exportcodec::Codec::H264),
						),
					)
				})
				.collect::<Vec<_>>()
		};
		let video_codecs = compatible_video_codecs(active_format);
		let audio_codecs = compatible_audio_codecs(active_format);
		let video_codec = cx.new(|cx| {
			ComboBox::new(5, codec_options(video_codecs), window, cx)
				.with_placeholder(i18n::tr("export.video_codec"))
		});
		video_codec.update(cx, |combo, cx| combo.set_selected(Some(0), cx));
		let audio_codec = cx.new(|cx| {
			ComboBox::new(6, codec_options(audio_codecs), window, cx)
				.with_placeholder(i18n::tr("export.audio_codec"))
		});
		audio_codec.update(cx, |combo, cx| combo.set_selected(Some(0), cx));

		// Container change → rebuild the codec lists (and re-select the
		// first compatible entry) so a stale incompatible codec can never
		// survive a format switch.
		cx.subscribe(&format, |this, _format, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value } = event {
				let fmt = this
					.formats
					.get(*value)
					.map(|(id, _, _)| *id)
					.unwrap_or(EXPORT_FORMAT_MP4);
				this.apply_format(fmt, cx);
			}
		});

		let color = cx.new(|cx| {
			let options = vec![
				ComboBoxOption::new(0, i18n::tr("export.color.sdr")),
				ComboBoxOption::new(1, i18n::tr("export.color.hdr")),
			];
			ComboBox::new(7, options, window, cx)
				.with_placeholder(i18n::tr("export.color"))
		});
		color.update(cx, |combo, cx| combo.set_selected(Some(0), cx));

		let range = cx.new(|cx| {
			let options = vec![
				ComboBoxOption::new(0, i18n::tr("export.range.all")),
				ComboBoxOption::new(1, i18n::tr("export.range.inout")),
			];
			ComboBox::new(8, options, window, cx)
				.with_placeholder(i18n::tr("export.range"))
		});
		range.update(cx, |combo, cx| combo.set_selected(Some(0), cx));

		// Resolution / frame rate / bitrate SPIN boxes: 0 = keep the
		// sequence's / the codec's default (the engine treats 0 that way),
		// any typed value is honored (the backend accepts arbitrary
		// width/height/rate/bitrate).
		let resolution_w = cx.new(|cx| {
			SpinBox::new(
				9,
				SliderModel::new(ValueKind::Integer, 0.0, 16384.0, 4.0, 0.0),
				window,
				cx,
			)
		});
		let resolution_h = cx.new(|cx| {
			SpinBox::new(
				12,
				SliderModel::new(ValueKind::Integer, 0.0, 16384.0, 4.0, 0.0),
				window,
				cx,
			)
		});
		let frame_rate = cx.new(|cx| {
			SpinBox::new(
				10,
				SliderModel::new(ValueKind::Float, 0.0, 240.0, 0.1, 0.0),
				window,
				cx,
			)
		});
		let bitrate = cx.new(|cx| {
			SpinBox::new(
				11,
				SliderModel::new(ValueKind::Integer, 0.0, 500_000_000.0, 1_000_000.0, 0.0),
				window,
				cx,
			)
		});

		let path = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});

		// The sequence picker is filled by the host (it owns the project);
		// an empty list disables the dialog's OK via the picker staying
		// unselected.
		let sequence = cx.new(|cx| {
			ComboBox::new(13, Vec::new(), window, cx)
				.with_placeholder(i18n::tr("export.sequence"))
		});

		Self {
			sequence,
			format,
			video_codec,
			audio_codec,
			color,
			range,
			resolution_w,
			resolution_h,
			frame_rate,
			bitrate,
			path,
			formats,
			active_format,
			sequences: Vec::new(),
		}
	}

	/// Populates the sequence picker from the open project and preselects
	/// `selected` (falling back to the first entry — the dialog always has
	/// a target to export).
	pub fn set_sequences(
		&mut self,
		sequences: Vec<(u64, SharedString)>,
		selected: Option<u64>,
		cx: &mut Context<Self>,
	) {
		let preselect = selected
			.and_then(|id| sequences.iter().position(|(entry, _)| *entry == id))
			.or(if sequences.is_empty() { None } else { Some(0) });
		let options = sequences
			.iter()
			.enumerate()
			.map(|(i, (_, name))| ComboBoxOption::new(i, name.clone()))
			.collect::<Vec<_>>();
		self.sequence.update(cx, |combo, cx| combo.set_options(options, cx));
		self.sequence
			.update(cx, |combo, cx| combo.set_selected(preselect, cx));
		self.sequences = sequences;
		cx.notify();
	}

	/// The picked sequence's project-entry id (`None` only when the
	/// project has no sequences — the host refuses to open the dialog
	/// then).
	pub fn selected_sequence(&self, cx: &App) -> Option<u64> {
		self.sequence
			.read(cx)
			.selected()
			.and_then(|index| self.sequences.get(index).map(|(id, _)| *id))
	}

	/// Rebuilds the codec lists for `fmt` and re-selects the first entry.
	fn apply_format(&mut self, fmt: i32, cx: &mut Context<Self>) {
		let video_codes = compatible_video_codecs(fmt);
		let audio_codes = compatible_audio_codecs(fmt);
		let v_opts = video_codes
			.iter()
			.enumerate()
			.map(|(i, code)| {
				ComboBoxOption::new(
					i,
					oak_codec::exportcodec::Codec::get_codec_name(
						oak_codec::exportcodec::Codec::from_i32(*code)
							.unwrap_or(oak_codec::exportcodec::Codec::H264),
					),
				)
			})
			.collect::<Vec<_>>();
		let a_opts = audio_codes
			.iter()
			.enumerate()
			.map(|(i, code)| {
				ComboBoxOption::new(
					i,
					oak_codec::exportcodec::Codec::get_codec_name(
						oak_codec::exportcodec::Codec::from_i32(*code)
							.unwrap_or(oak_codec::exportcodec::Codec::AAC),
					),
				)
			})
			.collect::<Vec<_>>();
		self.video_codec
			.update(cx, |combo, cx| combo.set_options(v_opts, cx));
		self.video_codec
			.update(cx, |combo, cx| combo.set_selected(Some(0), cx));
		self.audio_codec
			.update(cx, |combo, cx| combo.set_options(a_opts, cx));
		self.audio_codec
			.update(cx, |combo, cx| combo.set_selected(Some(0), cx));
		self.active_format = fmt;
	}

	/// The selected format id.
	pub fn format(&self, cx: &App) -> i32 {
		let Some(selected) = self.format.read(cx).selected() else {
			return EXPORT_FORMAT_MP4;
		};
		self.formats
			.get(selected)
			.map(|(id, _, _)| *id)
			.unwrap_or(EXPORT_FORMAT_MP4)
	}

	/// The selected format's file extension (without the dot).
	pub fn extension(&self, cx: &App) -> String {
		let Some(selected) = self.format.read(cx).selected() else {
			return "mp4".to_string();
		};
		self.formats
			.get(selected)
			.map(|(_, _, ext)| ext.clone())
			.unwrap_or_else(|| "mp4".to_string())
	}

	/// The output path currently entered.
	pub fn path(&self, cx: &App) -> SharedString {
		self.path.read(cx).path(cx)
	}

	/// Pre-fills the output path.
	pub fn set_path(&mut self, path: impl Into<SharedString>, cx: &mut Context<Self>) {
		let path = path.into();
		self.path
			.update(cx, |content, cx| content.set_path(path, cx));
		cx.notify();
	}

	/// The full export settings (codec/color/bit-depth/range/size/fps/
	/// bitrate) the engine consumes. The codec combos expose only the
	/// container's compatible codes, so the picked ids are always valid
	/// for `format`.
	pub fn settings(&self, cx: &App) -> crate::oakui::engine::ExportSettings {
		let video_codec = compatible_video_codecs(self.active_format)
			.get(self.video_codec.read(cx).selected().unwrap_or(0))
			.copied()
			.unwrap_or(1);
		let audio_codec = compatible_audio_codecs(self.active_format)
			.get(self.audio_codec.read(cx).selected().unwrap_or(0))
			.copied()
			.unwrap_or(12);
		let hdr = self.color.read(cx).selected() == Some(1);
		let in_out = self.range.read(cx).selected() == Some(1);
		let w = self.resolution_w.read(cx).value().to_f64().round() as i32;
		let h = self.resolution_h.read(cx).value().to_f64().round() as i32;
		let size = if w > 0 && h > 0 { (w, h) } else { (0, 0) };
		let frame_rate = self.frame_rate.read(cx).value().to_f64();
		let video_bitrate = self.bitrate.read(cx).value().to_f64().round() as i64;
		crate::oakui::engine::ExportSettings {
			format: self.active_format,
			video_codec,
			audio_codec,
			size,
			frame_rate,
			video_bitrate,
			audio_bitrate: 0,
			bit_depth: if hdr { 10 } else { 8 },
			color_primaries: if hdr { 9 } else { 1 },
			color_transfer: if hdr { 16 } else { 1 },
			color_space: if hdr { 9 } else { 1 },
			range: if in_out { Some((0.0, 0.0)) } else { None },
		}
	}
}

/// The video codecs the container `format` accepts (the compatibility
/// table; the dialog only offers these).
pub fn compatible_video_codecs(format: i32) -> Vec<i32> {
	let Some(container) = oak_codec::exportformat::Format::from_i32(format) else {
		return vec![1]; // H.264 fallback
	};
	oak_codec::exportformat::Format::get_video_codecs(container)
		.iter()
		.map(|c| *c as i32)
		.collect()
}

/// The audio codecs the container `format` accepts.
pub fn compatible_audio_codecs(format: i32) -> Vec<i32> {
	let Some(container) = oak_codec::exportformat::Format::from_i32(format) else {
		return vec![12]; // AAC fallback
	};
	oak_codec::exportformat::Format::get_audio_codecs(container)
		.iter()
		.map(|c| *c as i32)
		.collect()
}

impl Render for ExportDialogContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("export.sequence").into(),
				self.sequence.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.format").into(),
				self.format.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.video_codec").into(),
				self.video_codec.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.audio_codec").into(),
				self.audio_codec.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.color").into(),
				self.color.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.range").into(),
				self.range.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.resolution").into(),
				div()
					.flex()
					.gap_2()
					.child(self.resolution_w.clone())
					.child(div()
						.text_color(colors.disabled)
						.text_xs()
						.child("×"))
					.child(self.resolution_h.clone()),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.framerate").into(),
				self.frame_rate.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.bitrate").into(),
				self.bitrate.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("export.path").into(),
				self.path.clone(),
			))
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("export.hint")),
			)
	}
}

// ---------------------------------------------------------------------------
// Proxy settings (the C++ Tools > Proxy Settings dialog)
// ---------------------------------------------------------------------------

/// The ffmpeg encoder presets the proxy dialog offers (the C++
/// `ProxyDialog` preset combo, same order).
pub const PROXY_PRESETS: &[&str] = &[
	"ultrafast",
	"superfast",
	"veryfast",
	"faster",
	"fast",
	"medium",
	"slow",
	"slower",
	"veryslow",
];

/// The proxy settings dialog content: the global generation settings
/// plus the per-footage proxy list (the gpui port of the C++
/// `ProxyDialog`). All footage of the open project is listed — the gpui
/// shell opens the dialog from the Tools menu without a footage
/// selection, so the C++ "selected footage" group becomes "footage".
pub struct ProxyDialogContent<E: crate::oakui::engine::AppEngine> {
	engine: Entity<E>,
	divider: Entity<ComboBox>,
	width: Entity<SpinBox>,
	height: Entity<SpinBox>,
	crf: Entity<SpinBox>,
	preset: Entity<ComboBox>,
	include_audio: Entity<CheckBox>,
	max_concurrent: Entity<SpinBox>,
	ffmpeg_path: Entity<PathField>,
	custom_params: Entity<CheckBox>,
	/// Snapshot of the footage rows (refreshed after generate / delete).
	rows: Vec<crate::oakui::engine::ProxyFootageRow>,
	/// The divider values in dropdown order (1 = custom size).
	dividers: Vec<i32>,
}

impl<E: crate::oakui::engine::AppEngine> ProxyDialogContent<E> {
	/// Builds the content seeded from the global config params (the C++
	/// `oakengine_proxy_params_from_config` defaults).
	pub fn new(engine: Entity<E>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let params = crate::oakui::engine::proxy_params_from_config();

		let dividers: Vec<i32> = vec![1, 2, 4, 8];
		let divider_options = dividers
			.iter()
			.enumerate()
			.map(|(i, d)| ComboBoxOption::new(i, divider_label(*d)))
			.collect();
		let divider = cx.new(|cx| ComboBox::new(21, divider_options, window, cx));
		let divider_selected = dividers
			.iter()
			.position(|d| *d == params.divider)
			.unwrap_or(0);
		divider.update(cx, |combo, cx| {
			combo.set_selected(Some(divider_selected), cx)
		});

		let width = cx.new(|cx| {
			SpinBox::new(
				22,
				SliderModel::new(
					ValueKind::Integer,
					160.0,
					4096.0,
					16.0,
					f64::from(params.width),
				),
				window,
				cx,
			)
		});
		let height = cx.new(|cx| {
			SpinBox::new(
				23,
				SliderModel::new(
					ValueKind::Integer,
					120.0,
					2160.0,
					8.0,
					f64::from(params.height),
				),
				window,
				cx,
			)
		});
		let crf = cx.new(|cx| {
			SpinBox::new(
				24,
				SliderModel::new(ValueKind::Integer, 0.0, 51.0, 1.0, f64::from(params.crf)),
				window,
				cx,
			)
		});

		let preset_options = PROXY_PRESETS
			.iter()
			.enumerate()
			.map(|(i, name)| ComboBoxOption::new(i, *name))
			.collect();
		let preset = cx.new(|cx| ComboBox::new(25, preset_options, window, cx));
		let preset_selected = PROXY_PRESETS
			.iter()
			.position(|name| *name == params.preset)
			.unwrap_or(2);
		preset.update(cx, |combo, cx| {
			combo.set_selected(Some(preset_selected), cx)
		});

		let include_audio = cx.new(|cx| {
			CheckBox::new(
				26,
				if params.include_audio {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("proxydialog.include_audio"))
		});

		let max_concurrent = cx.new(|cx| {
			SpinBox::new(
				28,
				SliderModel::new(
					ValueKind::Integer,
					1.0,
					16.0,
					1.0,
					config_get_int("ProxyMaxConcurrent", 1).clamp(1, 16) as f64,
				),
				window,
				cx,
			)
		});

		let ffmpeg_path = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});
		ffmpeg_path.update(cx, |field, cx| {
			field.set_path(config_get_string(CONFIG_KEY_FFMPEG_PATH), cx)
		});

		let rows = engine.read(cx).proxy_rows();
		let any_custom = rows.iter().any(|row| row.has_custom);
		let custom_params = cx.new(|cx| {
			CheckBox::new(
				27,
				if any_custom {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("proxydialog.custom"))
		});

		Self {
			engine,
			divider,
			width,
			height,
			crf,
			preset,
			include_audio,
			max_concurrent,
			ffmpeg_path,
			custom_params,
			rows,
			dividers,
		}
	}

	/// The generation params currently edited in the dialog (the C++
	/// `current_params`).
	pub fn current_params(&self, cx: &App) -> crate::oakui::engine::ProxyParamsUi {
		let divider = self
			.divider
			.read(cx)
			.selected()
			.and_then(|i| self.dividers.get(i))
			.copied()
			.unwrap_or(1);
		let preset = self
			.preset
			.read(cx)
			.selected()
			.and_then(|i| PROXY_PRESETS.get(i))
			.unwrap_or(&"veryfast")
			.to_string();
		crate::oakui::engine::ProxyParamsUi {
			width: self.width.read(cx).value().to_f64() as i32,
			height: self.height.read(cx).value().to_f64() as i32,
			divider,
			crf: self.crf.read(cx).value().to_f64() as i32,
			preset,
			include_audio: self.include_audio.read(cx).state() == CheckState::Checked,
		}
	}

	/// Writes the global settings into the config store (the C++
	/// `save_global_settings`).
	pub fn save_global_settings(&self, cx: &App) {
		let params = self.current_params(cx);
		config_set_int("ProxyWidth", i64::from(params.width));
		config_set_int("ProxyHeight", i64::from(params.height));
		config_set_int("ProxyDivider", i64::from(params.divider));
		config_set_int("ProxyCRF", i64::from(params.crf));
		config_set_string("ProxyPreset", &params.preset);
		config_set_bool("ProxyIncludeAudio", params.include_audio);
		config_set_int(
			"ProxyMaxConcurrent",
			self.max_concurrent.read(cx).value().to_f64().clamp(1.0, 16.0) as i64,
		);
		config_set_string(
			CONFIG_KEY_FFMPEG_PATH,
			self.ffmpeg_path.read(cx).path(cx).trim(),
		);
	}

	/// Generates proxies for every footage row with a video stream (the
	/// C++ Generate Proxies button): with the custom checkbox set, the
	/// edited params become each footage's custom params first.
	pub fn generate(&mut self, cx: &mut Context<Self>) {
		let custom = self.custom_params.read(cx).state() == CheckState::Checked;
		let params = self.current_params(cx);
		let ids: Vec<(u64, bool)> = self
			.rows
			.iter()
			.map(|row| (row.id, row.can_generate))
			.collect();
		for (id, can_generate) in ids {
			if !can_generate {
				continue;
			}
			if custom {
				self.engine.update(cx, |engine, cx| {
					engine.proxy_set_custom_params(id, params.clone(), cx)
				});
			}
			if let Err(err) = self
				.engine
				.update(cx, |engine, cx| engine.proxy_generate(id, cx))
			{
				println!("[proxy] generate failed for {id}: {err}");
			}
		}
		self.refresh(cx);
	}

	/// Deletes every footage row's proxy (the C++ Delete Proxies button).
	pub fn delete(&mut self, cx: &mut Context<Self>) {
		let ids: Vec<u64> = self.rows.iter().map(|row| row.id).collect();
		for id in ids {
			self.engine
				.update(cx, |engine, cx| engine.proxy_delete(id, cx));
		}
		self.refresh(cx);
	}

	/// Applies the dialog (the C++ `accept`): saves the global settings
	/// and sets or clears each footage's custom params per the checkbox.
	pub fn accept(&mut self, cx: &mut Context<Self>) {
		self.save_global_settings(cx);
		let custom = self.custom_params.read(cx).state() == CheckState::Checked;
		let params = self.current_params(cx);
		let ids: Vec<u64> = self.rows.iter().map(|row| row.id).collect();
		for id in ids {
			if custom {
				self.engine.update(cx, |engine, cx| {
					engine.proxy_set_custom_params(id, params.clone(), cx)
				});
			} else {
				self.engine
					.update(cx, |engine, cx| engine.proxy_clear_custom_params(id, cx));
			}
		}
		self.refresh(cx);
	}

	/// Re-reads the footage rows (after generate / delete / accept).
	pub fn refresh(&mut self, cx: &mut Context<Self>) {
		self.rows = self.engine.read(cx).proxy_rows();
		cx.notify();
	}
}

/// The dropdown label of a resolution divider (the C++ combo labels).
fn divider_label(divider: i32) -> String {
	match divider {
		1 => i18n::tr("proxydialog.resolution.custom").into(),
		2 => i18n::tr("proxydialog.resolution.half").into(),
		4 => i18n::tr("proxydialog.resolution.quarter").into(),
		8 => i18n::tr("proxydialog.resolution.eighth").into(),
		other => format!("1/{other}"),
	}
}

/// The display string of a proxy lifecycle state.
fn proxy_state_label(state: crate::oakui::engine::ProxyMediaState) -> String {
    use crate::oakui::engine::ProxyMediaState;
    match state {
		ProxyMediaState::Missing => i18n::tr("proxydialog.state.missing"),
		ProxyMediaState::Generating => i18n::tr("proxydialog.state.generating"),
		ProxyMediaState::Ready => i18n::tr("proxydialog.state.ready"),
		ProxyMediaState::Failed => i18n::tr("proxydialog.state.failed"),
	}
	.into()
}

impl<E: crate::oakui::engine::AppEngine> Render for ProxyDialogContent<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();

		let footage_list = div().flex().flex_col().gap_1().children(
			self.rows
				.iter()
				.map(|row| {
					let mut state = proxy_state_label(row.state);
					if row.has_custom {
						state.push_str(&i18n::tr("proxydialog.custom_suffix"));
					}
					let enabled =
						row.enabled && row.state == crate::oakui::engine::ProxyMediaState::Ready;
					let dot = if enabled {
						colors.selected
					} else {
						colors.disabled
					};
					div()
						.flex()
						.items_center()
						.gap_2()
						.child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(dot))
						.child(
							div()
								.flex_1()
								.overflow_hidden()
								.text_ellipsis()
								.text_color(colors.text)
								.child(row.name.clone()),
						)
						.child(div().text_color(colors.disabled).text_xs().child(state))
				})
				.collect::<Vec<_>>(),
		);

		let footage_group = div()
			.flex()
			.flex_col()
			.gap_2()
			.child(section_header(
				&colors,
				i18n::tr("proxydialog.footage_group").into(),
			))
			.child(
				div()
					.id("proxy-footage-list")
					.max_h(px(160.0))
					.overflow_y_scroll()
					.child(if self.rows.is_empty() {
						div()
							.text_color(colors.disabled)
							.text_xs()
							.child(i18n::tr("proxydialog.no_footage"))
					} else {
						footage_list
					}),
			)
			.child(self.custom_params.clone());

		let settings_group = div()
			.flex()
			.flex_col()
			.gap_2()
			.child(section_header(
				&colors,
				i18n::tr("proxydialog.global").into(),
			))
			.child(form_row(
				&colors,
				i18n::tr("proxydialog.resolution").into(),
				self.divider.clone(),
			))
			.child(
				div()
					.flex()
					.gap_3()
					.child(form_row(
						&colors,
						i18n::tr("proxydialog.width").into(),
						self.width.clone(),
					))
					.child(form_row(
						&colors,
						i18n::tr("proxydialog.height").into(),
						self.height.clone(),
					)),
			)
			.child(form_row(
				&colors,
				i18n::tr("proxydialog.max_concurrent").into(),
				self.max_concurrent.clone(),
			))
			.child(
				div()
					.flex()
					.gap_3()
					.child(form_row(
						&colors,
						i18n::tr("proxydialog.crf").into(),
						self.crf.clone(),
					))
					.child(form_row(
						&colors,
						i18n::tr("proxydialog.preset").into(),
						self.preset.clone(),
					)),
			)
			.child(self.include_audio.clone())
			.child(form_row(
				&colors,
				i18n::tr("proxydialog.ffmpeg").into(),
				self.ffmpeg_path.clone(),
			));

		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(footage_group)
			.child(settings_group)
	}
}

// ---------------------------------------------------------------------------
// Project properties (the C++ File > Project Properties dialog)
// ---------------------------------------------------------------------------

/// The project-properties dialog content (the C++ `ProjectPropertiesDialog`):
/// the read-only project name, the per-project OCIO config override and the
/// disk-cache location. Apply happens through [`Self::commit`], which the
/// host runs on the OK button — an invalid OCIO config keeps the dialog open
/// with the error shown under the OCIO row.
pub struct ProjectPropertiesContent<E: crate::oakui::engine::AppEngine> {
	engine: Entity<E>,
	ocio_config: Entity<PathField>,
	cache_location: Entity<ComboBox>,
	custom_cache_path: Entity<PathField>,
	/// The cache location selected in the combo (0 = default location,
	/// 1 = alongside the project, 2 = custom path; see
	/// [`crate::oakui::engine::AppEngine::project_cache_location`]).
	cache_setting: i32,
	/// The pipeline working colorspace combo (ACEScg / sRGB legacy).
	working_space: Entity<ComboBox>,
	/// The delivery output gamut combo (sRGB / Display P3 / BT.2020).
	output_gamut: Entity<ComboBox>,
	/// The delivery output transfer combo (sRGB / gamma 2.2 / PQ / HLG).
	output_transfer: Entity<ComboBox>,
	/// The commit error shown under the OCIO row (an invalid config keeps
	/// the dialog open, like the C++ accept()).
	error: Option<String>,
}

impl<E: crate::oakui::engine::AppEngine> ProjectPropertiesContent<E> {
	/// Builds the content seeded from the engine's current project state.
	pub fn new(engine: Entity<E>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let ocio_config = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: true,
			}
		});
		let configured_ocio = engine.read(cx).project_ocio_config();
		ocio_config.update(cx, |field, cx| field.set_path(configured_ocio, cx));

		let cache_options = vec![
			ComboBoxOption::new(0, i18n::tr("projprops.cache.default")),
			ComboBoxOption::new(1, i18n::tr("projprops.cache.alongside")),
			ComboBoxOption::new(2, i18n::tr("projprops.cache.custom")),
		];
		let cache_location = cx.new(|cx| ComboBox::new(30, cache_options, window, cx));
		let (cache_setting, custom_path) = engine.read(cx).project_cache_location();
		cache_location.update(cx, |combo, cx| {
			combo.set_selected(Some(cache_setting as usize), cx)
		});
		// The custom-path field follows the combo selection live.
		cx.subscribe(
			&cache_location,
			|this, _combo, event: &ComboBoxEvent, cx| {
				let ComboBoxEvent::Selected { value } = event;
				let setting = *value as i32;
				this.cache_setting = setting;
				this.custom_cache_path
					.update(cx, |field, cx| field.set_enabled(setting == 2, cx));
				cx.notify();
			},
		)
		.detach();

		let custom_cache_path = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			PathField {
				editor,
				enabled: cache_setting == 2,
			}
		});
		custom_cache_path.update(cx, |field, cx| field.set_path(custom_path, cx));

		// --- Color pipeline: working colorspace + delivery output --------
		let working_options = vec![
			ComboBoxOption::new(0, i18n::tr("projprops.color.working.acescg")),
			ComboBoxOption::new(1, i18n::tr("projprops.color.working.srgb")),
		];
		let working_space = cx.new(|cx| ComboBox::new(30, working_options, window, cx));
		let gamut_options = vec![
			ComboBoxOption::new(0, i18n::tr("projprops.color.gamut.srgb")),
			ComboBoxOption::new(1, i18n::tr("projprops.color.gamut.p3")),
			ComboBoxOption::new(2, i18n::tr("projprops.color.gamut.bt2020")),
		];
		let output_gamut = cx.new(|cx| ComboBox::new(30, gamut_options, window, cx));
		let transfer_options = vec![
			ComboBoxOption::new(0, i18n::tr("projprops.color.transfer.srgb")),
			ComboBoxOption::new(1, i18n::tr("projprops.color.transfer.gamma22")),
			ComboBoxOption::new(2, i18n::tr("projprops.color.transfer.pq")),
			ComboBoxOption::new(3, i18n::tr("projprops.color.transfer.hlg")),
		];
		let output_transfer = cx.new(|cx| ComboBox::new(30, transfer_options, window, cx));
		let (working, gamut, transfer) = engine.read(cx).project_color_settings();
		working_space.update(cx, |combo, cx| {
			combo.set_selected(
                Some(oak_core::colormath::WorkingColorSpace::from_setting(&working) as usize),
                cx,
			)
		});
		output_gamut.update(cx, |combo, cx| {
			combo.set_selected(
                Some(oak_core::colormath::OutputGamut::from_setting(&gamut) as usize),
                cx,
			)
		});
		output_transfer.update(cx, |combo, cx| {
			combo.set_selected(
                Some(oak_core::colormath::OutputTransfer::from_setting(&transfer) as usize),
                cx,
			)
		});

		Self {
			engine,
			ocio_config,
			cache_location,
			custom_cache_path,
			cache_setting,
			working_space,
			output_gamut,
			output_transfer,
			error: None,
		}
	}

	/// The OCIO config path currently entered.
	pub fn ocio_config_path(&self, cx: &App) -> SharedString {
		self.ocio_config.read(cx).path(cx)
	}

	/// Replaces the OCIO config path (the 浏览… picker and tests).
	pub fn set_ocio_config_path(&mut self, path: impl Into<SharedString>, cx: &mut Context<Self>) {
		let path = path.into();
		self.ocio_config
			.update(cx, |field, cx| field.set_path(path, cx));
		cx.notify();
	}

	/// The custom disk-cache path currently entered.
	pub fn custom_cache_path(&self, cx: &App) -> SharedString {
		self.custom_cache_path.read(cx).path(cx)
	}

	/// Replaces the custom disk-cache path.
	pub fn set_custom_cache_path(&mut self, path: impl Into<SharedString>, cx: &mut Context<Self>) {
		let path = path.into();
		self.custom_cache_path
			.update(cx, |field, cx| field.set_path(path, cx));
		cx.notify();
	}

	/// Selects the cache location option (0 = default, 1 = alongside,
	/// 2 = custom) and enables the custom-path field accordingly — the
	/// combo's own event path, exposed for tests.
	pub fn select_cache_setting(&mut self, setting: i32, cx: &mut Context<Self>) {
		let setting = setting.clamp(0, 2);
		self.cache_setting = setting;
		self.cache_location.update(cx, |combo, cx| {
			combo.set_selected(Some(setting as usize), cx)
		});
		self.custom_cache_path
			.update(cx, |field, cx| field.set_enabled(setting == 2, cx));
		cx.notify();
	}

	/// Applies the edited settings (the C++ `accept()`): validates and
	/// applies the OCIO config override first — an invalid config keeps the
	/// dialog open — then the disk-cache location and the color pipeline
	/// settings. Ok clears the error row.
	pub fn commit(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
		let ocio = self.ocio_config_path(cx).to_string();
		self.engine
			.update(cx, |engine, cx| engine.set_project_ocio_config(ocio, cx))?;
		let custom = self.custom_cache_path(cx).to_string();
		let setting = self.cache_setting;
		let path = if setting == 2 { custom } else { String::new() };
		self.engine.update(cx, |engine, cx| {
			engine.set_project_cache_location(setting, path, cx)
		});
		// The color pipeline settings: combo index → canonical setting
		// string via the colormath enums (single source of truth).
		let (working, gamut, transfer) = self.color_settings(cx);
		self.engine.update(cx, |engine, cx| {
			engine.set_project_color_settings(working, gamut, transfer, cx)
		});
		self.set_error(None, cx);
		Ok(())
	}

	/// The color pipeline settings currently selected in the combos, as
	/// the canonical persisted strings.
	fn color_settings(&self, cx: &App) -> (String, String, String) {
        use oak_core::colormath::{OutputGamut, OutputTransfer, WorkingColorSpace};
        let working = self
			.working_space
			.read(cx)
			.selected()
			.map(|i| match i {
				1 => WorkingColorSpace::SrgbLegacy,
				_ => WorkingColorSpace::AcesCg,
			})
			.unwrap_or_default();
		let gamut = self
			.output_gamut
			.read(cx)
			.selected()
			.map(|i| match i {
				1 => OutputGamut::DisplayP3,
				2 => OutputGamut::Bt2020,
				_ => OutputGamut::Srgb,
			})
			.unwrap_or_default();
		let transfer = self
			.output_transfer
			.read(cx)
			.selected()
			.map(|i| match i {
				1 => OutputTransfer::Gamma22,
				2 => OutputTransfer::Pq,
				3 => OutputTransfer::Hlg,
				_ => OutputTransfer::Srgb,
			})
			.unwrap_or_default();
		(
			working.as_setting().to_string(),
			gamut.as_setting().to_string(),
			transfer.as_setting().to_string(),
		)
	}

	/// The error shown under the OCIO row after a rejected commit.
	pub fn set_error(&mut self, msg: Option<String>, cx: &mut Context<Self>) {
		self.error = msg;
		cx.notify();
	}

	/// The commit error currently shown (`None` while the last commit
	/// applied cleanly).
	pub fn error(&self) -> Option<&String> {
		self.error.as_ref()
	}
}

impl<E: crate::oakui::engine::AppEngine> Render for ProjectPropertiesContent<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let project_name = self
			.engine
			.read(cx)
			.project()
			.map(|p| p.name.clone())
			.unwrap_or_default();

		// The 浏览… button picks an OCIO config through the platform file
		// dialog and fills the path field (the resolve is async, so the
		// picker's receiver is drained in a spawned task).
		let ocio_field = self.ocio_config.clone();
		let browse = div()
			.id("projprops-browse")
			.px_3()
			.py_1()
			.rounded_md()
			.bg(colors.background)
			.border_1()
			.border_color(colors.border)
			.text_color(colors.text)
			.cursor_pointer()
			.on_click(
				cx.listener(move |_this, _event: &gpui::ClickEvent, _window, cx| {
					let receiver = cx.prompt_for_paths(PathPromptOptions {
						files: true,
						directories: false,
						multiple: false,
						prompt: None,
				allowed_extensions: Vec::new(),
					});
					cx.spawn(async move |this, cx| {
						if let Ok(Ok(Some(paths))) = receiver.await {
							if let Some(path) = paths.first() {
								let path = path.to_string_lossy().into_owned();
								let _ =
									this.update(cx, |this, cx| this.set_ocio_config_path(path, cx));
							}
						}
					})
					.detach();
				}),
			)
			.child(i18n::tr("projprops.browse"));

		let ocio_row = div().flex().gap_2().child(ocio_field).child(browse);

		let custom_row = form_row(
			&colors,
			i18n::tr("projprops.cache.custom").into(),
			self.custom_cache_path.clone(),
		);

		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("projprops.name").into(),
				div().text_color(colors.text).child(project_name),
			))
			.child(
				form_row(&colors, i18n::tr("projprops.ocio_config").into(), ocio_row).child(
					if let Some(error) = &self.error {
						div()
							.debug_selector(|| "projprops-error".into())
							.text_color(gpui::rgb(0xe5484d))
							.text_xs()
							.child(error.clone())
					} else {
						div()
					},
				),
			)
			.child(form_row(
				&colors,
				i18n::tr("projprops.cache.location").into(),
				self.cache_location.clone(),
			))
			.child(custom_row)
			.child(form_row(
				&colors,
				i18n::tr("projprops.color.working").into(),
				self.working_space.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("projprops.color.gamut").into(),
				self.output_gamut.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("projprops.color.transfer").into(),
				self.output_transfer.clone(),
			))
	}
}

// ---------------------------------------------------------------------------
// Preferences: the tabbed host (General + Keyboard)
// ---------------------------------------------------------------------------

/// The preferences dialog tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreferencesTab {
	/// The grouped settings (language / theme / backend / cache / …).
	General,
	/// The custom-shortcuts editor.
	Keyboard,
}

/// A fully transparent color (for un-selected rows / tabs).
fn transparent() -> gpui::Rgba {
	gpui::Rgba {
		r: 0.0,
		g: 0.0,
		b: 0.0,
		a: 0.0,
	}
}

/// The tabbed preferences dialog content: the existing grouped settings plus
/// the Keyboard tab — the Rust counterpart of the C++ `PreferencesDialog`
/// hosting the `PreferencesKeyboardTab` (`preferenceskeyboardtab.cpp`).
///
/// The host re-emits the general tab's [`PreferencesEvent`]s (theme /
/// language) and turns the keyboard tab's [`KeyboardEvent::Changed`] into
/// [`PreferencesEvent::ShortcutsChanged`], so the app shell only subscribes to
/// this one content entity.
pub struct PreferencesDialogContent {
	active: PreferencesTab,
	general: Entity<PreferencesContent>,
	keyboard: Entity<KeyboardTabContent>,
}

impl EventEmitter<PreferencesEvent> for PreferencesDialogContent {}

impl PreferencesDialogContent {
	/// Builds both tabs (the general tab keeps its existing behavior; the
	/// keyboard tab lists the current menu-bar actions).
	pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
		let general = cx.new(|cx| PreferencesContent::new(window, cx));
		let keyboard = cx.new(|cx| KeyboardTabContent::new(window, cx));
		cx.subscribe(
			&general,
			|_this, _general, event: &PreferencesEvent, cx| match event {
				PreferencesEvent::ThemeChanged(dark) => {
					cx.emit(PreferencesEvent::ThemeChanged(*dark));
				}
				PreferencesEvent::LanguageChanged => cx.emit(PreferencesEvent::LanguageChanged),
				PreferencesEvent::ShortcutsChanged => {}
				PreferencesEvent::DisplayColorChanged => {
					cx.emit(PreferencesEvent::DisplayColorChanged);
				}
			},
		)
		.detach();
		cx.subscribe(&keyboard, |_this, _keyboard, event: &KeyboardEvent, cx| {
			if matches!(event, KeyboardEvent::Changed) {
				cx.emit(PreferencesEvent::ShortcutsChanged);
			}
		})
		.detach();
		Self {
			active: PreferencesTab::General,
			general,
			keyboard,
		}
	}

	/// Commits the general tab's free-text fields (the cache directory, the
	/// custom ICC path, the PostgreSQL connection string), for the host when
	/// the dialog closes.
	pub fn commit_cache_dir(&self, cx: &App) {
		let general = self.general.read(cx);
		general.commit_cache_dir(cx);
		general.commit_display_icc_path(cx);
		general.commit_storage_pg_url(cx);
	}

	/// The keyboard tab's action-row count (tests).
	pub fn keyboard_tab_row_count(&self, cx: &App) -> usize {
		self.keyboard.read(cx).row_count()
	}
}

impl Render for PreferencesDialogContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let content = match self.active {
			PreferencesTab::General => self.general.clone().into_any_element(),
			PreferencesTab::Keyboard => self.keyboard.clone().into_any_element(),
		};
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(
				div()
					.flex()
					.gap_1()
					.child(tab_button(
						PreferencesTab::General,
						self.active,
						&colors,
						cx,
					))
					.child(tab_button(
						PreferencesTab::Keyboard,
						self.active,
						&colors,
						cx,
					)),
			)
			.child(content)
	}
}

/// One tab switcher button of the preferences dialog.
fn tab_button(
	tab: PreferencesTab,
	active: PreferencesTab,
	colors: &gpui::colors::Colors,
	cx: &mut Context<PreferencesDialogContent>,
) -> gpui::Stateful<gpui::Div> {
	let selected = tab == active;
	let label = match tab {
		PreferencesTab::General => i18n::tr("preferences.tab.general"),
		PreferencesTab::Keyboard => i18n::tr("preferences.tab.keyboard"),
	};
	div()
		.id(match tab {
			PreferencesTab::General => "prefs-tab-general",
			PreferencesTab::Keyboard => "prefs-tab-keyboard",
		})
		.debug_selector(move || match tab {
			PreferencesTab::General => "prefs-tab-general".into(),
			PreferencesTab::Keyboard => "prefs-tab-keyboard".into(),
		})
		.px_3()
		.py_1()
		.rounded_md()
		.cursor_pointer()
		.bg(if selected {
			colors.selected
		} else {
			transparent()
		})
		.text_color(if selected {
			colors.selected_text
		} else {
			colors.text
		})
		.on_click(cx.listener(move |this, _event, _window, cx| {
			this.active = tab;
			cx.notify();
		}))
		.child(label)
}

/// A small pill-shaped text button (the keyboard tab's footer buttons). The
/// caller chains the click handler on the returned element.
fn pill_button(
	id: &'static str,
	label: impl Into<SharedString>,
	bg: gpui::Rgba,
	fg: gpui::Rgba,
) -> gpui::Stateful<gpui::Div> {
	let label: SharedString = label.into();
	div()
		.id(id)
		.px_3()
		.py_1()
		.rounded_md()
		.cursor_pointer()
		.bg(bg)
		.text_color(fg)
		.child(label)
}

// ---------------------------------------------------------------------------
// Preferences → Keyboard tab
// ---------------------------------------------------------------------------

/// A request the keyboard tab emits; the tabbed host re-emits it as
/// [`PreferencesEvent::ShortcutsChanged`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardEvent {
	/// The shortcut overrides changed (a key was assigned / cleared / reset /
	/// imported). The host must re-bind the key map and rebuild the menu bar.
	Changed,
}

impl EventEmitter<KeyboardEvent> for KeyboardTabContent {}

/// One row of the keyboard tab: a menu-bar action with its hierarchy.
struct KeyboardRow {
	action: ActionId,
	/// The top-level menu title (the section header), localized.
	section: String,
	/// The full "Menu > Submenu > …" path, localized.
	path: String,
	/// The capture field's focus handle.
	focus: FocusHandle,
}

/// The Preferences → Keyboard tab: a searchable, section-grouped list of every
/// menu-bar action with a click-to-capture shortcut editor, plus Reset
/// Selected / Reset All and Import / Export — the Rust counterpart of the C++
/// `PreferencesKeyboardTab`.
///
/// # Capture
///
/// Clicking a row's shortcut field enters capture mode: a process-wide
/// [`gpui::App::intercept_keystrokes`] subscription suppresses the global key
/// map for as long as the capture is active, so the field's `on_key_down`
/// sees *every* key — including the letters, digits and arrows that the
/// registry binds (the shell's action listeners would otherwise swallow them
/// before the widget-level handlers run, the same problem Qt solves with
/// `QKeySequenceEdit`'s `ShortcutOverride`). The capture field then decides:
///
/// * any real key (plus modifiers) becomes the action's new binding;
/// * Backspace / Delete unbind the action (back to "None");
/// * Escape cancels the capture.
///
/// Every commit writes the override diff to `<config>/shortcuts` and emits
/// [`KeyboardEvent::Changed`], so the change is live immediately (no restart).
///
/// # Conflicts
///
/// Assigning a key that another action already binds *moves* the binding: the
/// displaced action loses the key (falling back to its remaining keys, or to
/// none) and the status line says so. Simple and explicit — the alternative
/// (refusing the assignment) would leave the user stuck when two popular
/// defaults collide.
pub struct KeyboardTabContent {
	query: Entity<EditableTextState>,
	rows: Vec<KeyboardRow>,
	filter: String,
	capturing: Option<usize>,
	selected: Option<usize>,
	confirm_reset_all: bool,
	status: Option<String>,
	/// The keystroke interceptor that routes every key to the capture logic
	/// while a row is capturing (see the capture notes above); it lives for
	/// the tab's whole lifetime and only acts while `capturing` is set.
	#[allow(dead_code)] // kept alive: dropping it unregisters the keystroke interceptor
	interceptor: Option<gpui::Subscription>,
}

impl KeyboardTabContent {
	/// Builds the tab, seeding one row per menu-bar action (the C++
	/// `setup_kbd_shortcuts` enumeration).
	pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
		let query = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
		cx.subscribe(&query, |this, _query, _event: &TextChanged, cx| {
			this.filter = this.query.read(cx).as_str().to_string();
			this.selected = None;
			cx.notify();
		})
		.detach();
		let rows = crate::app::menu_action_paths()
			.into_iter()
			.map(|(action, path)| {
				let section = path.split(" > ").next().unwrap_or_default().to_string();
				KeyboardRow {
					action,
					section,
					path,
					focus: cx.focus_handle(),
				}
			})
			.collect();
		let weak = cx.weak_entity();
		let interceptor = cx.intercept_keystrokes(move |event, _window, app| {
			// While any row is capturing, handle the key here — before the
			// shell's global key bindings (which would otherwise swallow it) —
			// and stop the event so it can neither reach an app action nor
			// bubble to the modal (Escape must cancel the capture, not close
			// the dialog).
			let Some(this) = weak.upgrade() else {
				return;
			};
			if this.read(app).capturing.is_some() {
				this.update(app, |this, cx| {
					this.handle_capture_key(&event.keystroke, cx);
				});
			}
		});
		Self {
			query,
			rows,
			filter: String::new(),
			capturing: None,
			selected: None,
			confirm_reset_all: false,
			status: None,
			interceptor: Some(interceptor),
		}
	}

	/// The number of menu-bar actions listed (tests).
	pub fn row_count(&self) -> usize {
		self.rows.len()
	}

	/// Enters capture mode for `index`: remembers the row and highlights it.
	/// The keystroke interceptor installed at construction does the rest — it
	/// sees every key before the shell's global bindings do, so the capture
	/// works regardless of which element currently has focus.
	fn begin_capture(&mut self, index: usize, cx: &mut Context<Self>) {
		self.capturing = Some(index);
		self.selected = Some(index);
		self.confirm_reset_all = false;
		cx.notify();
	}

	/// Leaves capture mode.
	fn end_capture(&mut self, cx: &mut Context<Self>) {
		self.capturing = None;
		cx.notify();
	}

	/// Handles one captured key (called from the keystroke interceptor, so it
	/// runs for every key while a row is capturing).
	fn handle_capture_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
		let Some(index) = self.capturing else {
			return;
		};
		match capture_decision(keystroke) {
			CaptureDecision::Ignore => cx.stop_propagation(),
			CaptureDecision::Cancel => {
				self.end_capture(cx);
				cx.stop_propagation();
			}
			CaptureDecision::Clear => {
				let action = self.rows[index].action;
				crate::actions::set_custom_shortcut(action.entry().cpp_id, Vec::new());
				let _ = crate::actions::save_custom_shortcuts();
				self.status = Some(i18n::tr("preferences.keyboard.cleared").to_string());
				self.end_capture(cx);
				cx.emit(KeyboardEvent::Changed);
				cx.stop_propagation();
			}
			CaptureDecision::Assign(canon) => {
				let action = self.rows[index].action;
				let stolen = crate::actions::steal_shortcut_for(action.entry(), &canon);
				crate::actions::set_custom_shortcut(action.entry().cpp_id, vec![canon]);
				let _ = crate::actions::save_custom_shortcuts();
				self.status = stolen.map(|previous| {
					i18n::tr("preferences.keyboard.conflict")
						.replace("{action}", i18n::tr(previous.entry().i18n_key))
				});
				self.end_capture(cx);
				cx.emit(KeyboardEvent::Changed);
				cx.stop_propagation();
			}
		}
	}

	/// Reset Selected: the selected row's action falls back to its registry
	/// default keys.
	fn reset_selected(&mut self, cx: &mut Context<Self>) {
		let Some(index) = self.selected else {
			return;
		};
		let action = self.rows[index].action;
		crate::actions::reset_custom_shortcut(action.entry().cpp_id);
		let _ = crate::actions::save_custom_shortcuts();
		self.status = Some(i18n::tr("preferences.keyboard.reset").to_string());
		cx.emit(KeyboardEvent::Changed);
		cx.notify();
	}

	/// Reset All: the first click arms an inline confirmation (the C++
	/// `QMessageBox` equivalent, kept inside the tab so the host modal
	/// machinery stays untouched); the second applies it.
	fn reset_all(&mut self, cx: &mut Context<Self>) {
		if !self.confirm_reset_all {
			self.confirm_reset_all = true;
			self.capturing = None;
			cx.notify();
			return;
		}
		crate::actions::reset_all_custom_shortcuts();
		let _ = crate::actions::save_custom_shortcuts();
		self.confirm_reset_all = false;
		self.status = Some(i18n::tr("preferences.keyboard.reset_all_done").to_string());
		cx.emit(KeyboardEvent::Changed);
		cx.notify();
	}

	/// Import: pick a `shortcuts` file, replace the overrides with its
	/// contents (anything unlisted falls back to default, like the C++ field
	/// walk), then save the effective state back to the configured location.
	fn import_shortcuts(&mut self, cx: &mut Context<Self>) {
		let receiver = cx.prompt_for_paths(PathPromptOptions {
			files: true,
			directories: false,
			multiple: false,
			prompt: Some(i18n::tr("preferences.keyboard.import").into()),
			allowed_extensions: Vec::new(),
		});
		cx.spawn(async move |this, cx| {
			let Ok(Ok(Some(paths))) = receiver.await else {
				return;
			};
			let Some(path) = paths.first() else {
				return;
			};
			let result = crate::actions::load_custom_shortcuts_from(&path.to_string_lossy());
			this.update(cx, |this, cx| {
				match result {
					Ok(_) => {
						this.status = Some(i18n::tr("preferences.keyboard.imported").to_string());
						let _ = crate::actions::save_custom_shortcuts();
					}
					Err(_) => {
						this.status =
							Some(i18n::tr("preferences.keyboard.import_failed").to_string())
					}
				}
				this.capturing = None;
				this.confirm_reset_all = false;
				cx.emit(KeyboardEvent::Changed);
				cx.notify();
			});
		})
		.detach();
	}

	/// Export: write the current override diff to a picked file.
	fn export_shortcuts(&mut self, cx: &mut Context<Self>) {
		let receiver = cx.prompt_for_new_path(&std::path::PathBuf::from("."), Some("shortcuts"));
		cx.spawn(async move |this, cx| {
			let Ok(Ok(Some(path))) = receiver.await else {
				return;
			};
			let result = crate::actions::save_custom_shortcuts_to(&path.to_string_lossy());
			this.update(cx, |this, cx| {
				this.status = Some(match result {
					Ok(_) => i18n::tr("preferences.keyboard.exported").to_string(),
					Err(_) => i18n::tr("preferences.keyboard.export_failed").to_string(),
				});
				cx.notify();
			});
		})
		.detach();
	}
}

impl Render for KeyboardTabContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let weak = self.query.downgrade();
		let capturing = self.capturing;
		let selected = self.selected;

		// The search box.
		let search = div()
			.rounded_md()
			.border_1()
			.border_color(colors.border)
			.bg(colors.background)
			.px_2()
			.py_1()
			.child(
				text_input("keyboard-search-input", cx)
					.state(weak)
					.accepts_input(true),
			);

		// The grouped, filtered action list.
		let mut list = div()
			.id("keyboard-shortcut-list")
			.flex()
			.flex_col()
			.max_h(px(340.0))
			.overflow_y_scroll();
		let mut shown_section: Option<String> = None;
		for (index, row) in self.rows.iter().enumerate() {
			let label = i18n::tr(row.action.entry().i18n_key);
			let shortcut = crate::actions::display_shortcut(row.action);
			if !keyboard_filter_matches(label, &row.path, shortcut.as_deref(), &self.filter) {
				continue;
			}
			if shown_section.as_deref() != Some(row.section.as_str()) {
				list = list.child(section_header(&colors, row.section.clone().into()));
				shown_section = Some(row.section.clone());
			}
			let row_selected = selected == Some(index);
			let is_capturing = capturing == Some(index);
			let field_label: SharedString = if is_capturing {
				i18n::tr("preferences.keyboard.capturing").into()
			} else {
				shortcut
					.map(SharedString::from)
					.unwrap_or_else(|| i18n::tr("preferences.keyboard.unbound").into())
			};
			let row_path = row.path.clone();
			let focus = row.focus.clone();
			list = list.child(
				div()
					.id(ElementId::named_usize("keyboard-shortcut-row", index))
					.flex()
					.items_center()
					.gap_2()
					.px_1()
					.py_0p5()
					.rounded_md()
					.bg(if row_selected {
						colors.selected
					} else {
						transparent()
					})
					.on_click(cx.listener(move |this, _event, _window, cx| {
						this.selected = Some(index);
						cx.notify();
					}))
					.child(
						div()
							.flex_1()
							.flex_col()
							.child(div().text_color(colors.text).child(label))
							.child(div().text_color(colors.disabled).text_xs().child(row_path)),
					)
					.child(
						div()
							.id(ElementId::named_usize("keyboard-shortcut-capture", index))
							.debug_selector(move || format!("keyboard-capture-{index}").into())
							.min_w(px(150.0))
							.px_2()
							.py_0p5()
							.rounded_md()
							.border_1()
							.border_color(if is_capturing {
								colors.selected
							} else {
								colors.border
							})
							.bg(colors.background)
							.text_color(colors.text)
							.cursor_pointer()
							.track_focus(&focus)
							.on_click(cx.listener(move |this, _event, _window, cx| {
								if this.capturing != Some(index) {
									this.begin_capture(index, cx);
								}
								cx.stop_propagation();
							}))
							.child(field_label),
					),
			);
		}

		// The footer: Import/Export on the left, Reset Selected/All on the
		// right (the inline Reset-All confirmation replaces them when armed).
		let footer = if self.confirm_reset_all {
			div()
				.flex()
				.items_center()
				.gap_2()
				.child(
					div()
						.flex_1()
						.text_color(colors.text)
						.child(i18n::tr("preferences.keyboard.reset_all.confirm")),
				)
				.child(
					pill_button(
						"prefs-keyboard-confirm-reset",
						i18n::tr("preferences.keyboard.reset_all"),
						colors.selected,
						colors.selected_text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| this.reset_all(cx))),
				)
				.child(
					pill_button(
						"prefs-keyboard-cancel-reset",
						i18n::tr("dialog.cancel"),
						colors.background,
						colors.text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| {
						this.confirm_reset_all = false;
						cx.notify();
					})),
				)
		} else {
			div()
				.flex()
				.items_center()
				.gap_2()
				.child(
					pill_button(
						"prefs-keyboard-import",
						i18n::tr("preferences.keyboard.import"),
						colors.background,
						colors.text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| this.import_shortcuts(cx))),
				)
				.child(
					pill_button(
						"prefs-keyboard-export",
						i18n::tr("preferences.keyboard.export"),
						colors.background,
						colors.text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| this.export_shortcuts(cx))),
				)
				.child(div().flex_1())
				.child(
					pill_button(
						"prefs-keyboard-reset-selected",
						i18n::tr("preferences.keyboard.reset_selected"),
						colors.background,
						colors.text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| this.reset_selected(cx))),
				)
				.child(
					pill_button(
						"prefs-keyboard-reset-all",
						i18n::tr("preferences.keyboard.reset_all"),
						colors.background,
						colors.text,
					)
					.on_click(cx.listener(|this, _event, _window, cx| this.reset_all(cx))),
				)
		};

		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(search)
			.child(
				div()
					.flex()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("preferences.keyboard.action"))
					.child(div().flex_1())
					.child(i18n::tr("preferences.keyboard.shortcut")),
			)
			.child(list)
			.child(footer)
			.child(if let Some(status) = &self.status {
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(status.clone())
			} else {
				div()
			})
	}
}

/// The outcome of one captured keystroke.
#[derive(Debug)]
enum CaptureDecision {
	/// A modifier-only key — keep capturing, ignore it.
	Ignore,
	/// Escape — cancel the capture without changing anything.
	Cancel,
	/// Backspace / Delete — clear the binding (unbind).
	Clear,
	/// A real key — the new binding, in canonical gpui keystroke form.
	Assign(String),
}

/// Decides what a capture field should do with a keystroke. Modifier-only
/// keys (a bare Shift / Ctrl / …) never bind; Backspace and Delete clear;
/// Escape cancels; anything else (with or without modifiers) becomes the new
/// binding.
fn capture_decision(keystroke: &Keystroke) -> CaptureDecision {
	match keystroke.key.as_str() {
		"escape" => CaptureDecision::Cancel,
		"backspace" | "delete" => CaptureDecision::Clear,
		// Modifier-only key presses (the parser represents a bare modifier as
		// the modifier's own key name).
		"shift" | "control" | "alt" | "cmd" | "super" | "win" | "fn" | "function" | "secondary"
		| "platform" => CaptureDecision::Ignore,
		"" => CaptureDecision::Ignore,
		_ => CaptureDecision::Assign(keystroke.unparse()),
	}
}

/// Whether an action row survives the keyboard tab's search query:
/// case-insensitive match against the action label, the localized menu path,
/// or the effective shortcut label.
fn keyboard_filter_matches(label: &str, path: &str, shortcut: Option<&str>, query: &str) -> bool {
	let query = query.trim();
	if query.is_empty() {
		return true;
	}
	let query = query.to_lowercase();
	let label_lower = label.to_lowercase();
	let full_path = format!("{path} > {label}").to_lowercase();
	label_lower.contains(&query)
		|| full_path.contains(&query)
		|| shortcut.is_some_and(|s| s.to_lowercase().contains(&query))
}

// ---------------------------------------------------------------------------
// Action search (Help > Search Actions…, the `/` key)
// ---------------------------------------------------------------------------

/// One item of the action search list.
struct ActionSearchItem {
	action: ActionId,
	/// The localized "Menu > Submenu > …" path.
	path: String,
}

/// The action search dialog content (the C++ `ActionSearch`): a search field
/// over every menu-bar action, live filtering, arrow-key selection and
/// Enter / double-click execution through the same dispatch path the menu
/// clicks take. Panel-context hotkeys (the `HIDDEN_MENU_ID` items) never
/// appear — they have no menu item, exactly like the C++ "only the menu bar"
/// enumeration.
pub struct ActionSearchContent {
	query: Entity<EditableTextState>,
	items: Vec<ActionSearchItem>,
	filter: String,
	selection: Option<usize>,
	/// The keystroke interceptor that routes Up/Down/Enter to the list while
	/// the dialog is open (see [`ActionSearchContent::new`]).
	#[allow(dead_code)] // kept alive: dropping it unregisters the keystroke interceptor
	interceptor: Option<gpui::Subscription>,
}

/// A request the action search dialog emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionSearchEvent {
	/// The user activated `action`; the host dispatches it (the same path the
	/// menu clicks take) and closes the dialog.
	Execute(ActionId),
}

impl EventEmitter<ActionSearchEvent> for ActionSearchContent {}

impl ActionSearchContent {
	/// Builds the dialog with every menu-bar action, subscribes to the search
	/// field so filtering re-runs on every keystroke, and installs a keystroke
	/// interceptor that routes Up / Down / Enter to the list.
	///
	/// The interceptor is needed because the shell's global key bindings run
	/// *before* the widget-level key handlers and would swallow Up/Down (they
	/// are the GoToPrevCut/GoToNextCut keys) and Enter — the same reason the
	/// Keyboard tab captures its keys through an interceptor. The dialog is
	/// modal, so the only text input alive while the interceptor is active is
	/// the search field; every other keystroke passes through untouched (in
	/// the real app the IME delivers text to the focused input independently
	/// of the key map, so typing keeps working).
	pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
		let query = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
		cx.subscribe(&query, |this, _query, _event: &TextChanged, cx| {
			this.filter = this.query.read(cx).as_str().to_string();
			let visible: Vec<usize> = this
				.items
				.iter()
				.enumerate()
				.filter(|(_, item)| search_filter_matches(item.action, &item.path, &this.filter))
				.map(|(index, _)| index)
				.collect();
			this.selection = selection_step(&visible, None, 1);
			cx.notify();
		})
		.detach();
		let items = crate::app::menu_action_paths()
			.into_iter()
			.map(|(action, path)| ActionSearchItem { action, path })
			.collect();
		let weak = cx.weak_entity();
		let interceptor = cx.intercept_keystrokes(move |event, _window, app| {
			// Only act on the keys the dialog owns; everything else (text
			// for the search field, Escape for the modal, …) passes through.
			if !matches!(event.keystroke.key.as_str(), "up" | "down" | "enter") {
				return;
			}
			let Some(this) = weak.upgrade() else {
				return;
			};
			let mut stop = false;
			this.update(app, |this, cx| match event.keystroke.key.as_str() {
				"up" => {
					this.move_selection(-1, cx);
					stop = true;
				}
				"down" => {
					this.move_selection(1, cx);
					stop = true;
				}
				"enter" => {
					this.execute(cx);
					stop = true;
				}
				// Escape deliberately falls through to the modal's own handler
				// (which closes the dialog); every other key passes to the
				// search field.
				_ => {}
			});
			if stop {
				app.stop_propagation();
			}
		});
		Self {
			query,
			items,
			filter: String::new(),
			selection: None,
			interceptor: Some(interceptor),
		}
	}

	/// The search field's focus handle (the host focuses it when the dialog
	/// opens, so the search is keyboard-first from the start).
	pub fn search_focus(&self, cx: &App) -> FocusHandle {
		self.query.read(cx).focus_handle(cx)
	}

	fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
		let visible: Vec<usize> = self
			.items
			.iter()
			.enumerate()
			.filter(|(_, item)| search_filter_matches(item.action, &item.path, &self.filter))
			.map(|(index, _)| index)
			.collect();
		self.selection = selection_step(&visible, self.selection, delta);
		cx.notify();
	}

	/// The currently selected action (tests).
	pub fn selected_action(&self) -> Option<ActionId> {
		self.selection
			.and_then(|index| self.items.get(index))
			.map(|item| item.action)
	}

	/// The current search filter (tests).
	pub fn filter(&self) -> &str {
		&self.filter
	}

	fn execute(&mut self, cx: &mut Context<Self>) {
		if let Some(index) = self.selection {
			if let Some(item) = self.items.get(index) {
				cx.emit(ActionSearchEvent::Execute(item.action));
			}
		}
	}
}

impl Render for ActionSearchContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let weak = self.query.downgrade();
		let selection = self.selection;
		let visible: Vec<usize> = self
			.items
			.iter()
			.enumerate()
			.filter(|(_, item)| search_filter_matches(item.action, &item.path, &self.filter))
			.map(|(index, _)| index)
			.collect();

		let list = if visible.is_empty() {
			div()
				.id("action-search-list")
				.flex()
				.flex_col()
				.max_h(px(360.0))
				.overflow_y_scroll()
				.child(div().text_color(colors.disabled).text_xs().child(
					if self.items.is_empty() {
						i18n::tr("actionsearch.no_actions")
					} else {
						i18n::tr("actionsearch.empty")
					},
				))
		} else {
			div()
				.id("action-search-list")
				.flex()
				.flex_col()
				.max_h(px(360.0))
				.overflow_y_scroll()
				.children(visible.iter().map(|&index| {
					let item = &self.items[index];
					let label = i18n::tr(item.action.entry().i18n_key);
					let path = item.path.clone();
					let row_selected = selection == Some(index);
					div()
						.id(ElementId::named_usize("action-search-item", index))
						.flex()
						.items_center()
						.gap_2()
						.px_2()
						.py_0p5()
						.rounded_md()
						.bg(if row_selected {
							colors.selected
						} else {
							transparent()
						})
						.text_color(if row_selected {
							colors.selected_text
						} else {
							colors.text
						})
						.cursor_pointer()
						.on_click(cx.listener(move |this, _event, _window, cx| {
							this.selection = Some(index);
							cx.notify();
						}))
						.on_mouse_down(
							gpui::MouseButton::Left,
							cx.listener(move |this, event: &gpui::MouseDownEvent, _window, cx| {
								// Double-click executes.
								if event.click_count >= 2 {
									this.selection = Some(index);
									this.execute(cx);
								}
							}),
						)
						.child(
							div().flex_1().flex_col().child(div().child(label)).child(
								div()
									.text_xs()
									.text_color(colors.disabled)
									.child(format!("({path})")),
							),
						)
				}))
		};

		div()
			.id("action-search-root")
			.flex()
			.flex_col()
			.gap_2()
			.w_full()
			.child(
				div()
					.rounded_md()
					.border_1()
					.border_color(colors.border)
					.bg(colors.background)
					.px_2()
					.py_1()
					.child(
						text_input("action-search-input", cx)
							.state(weak)
							.accepts_input(true),
					),
			)
			.child(list)
	}
}

/// Whether an action-search item survives the query: case-insensitive match
/// against the action label or the full "Menu > Submenu > action" path.
fn search_filter_matches(action: ActionId, path: &str, query: &str) -> bool {
	let query = query.trim();
	if query.is_empty() {
		return true;
	}
	let query = query.to_lowercase();
	let label = i18n::tr(action.entry().i18n_key);
	label.to_lowercase().contains(&query)
		|| format!("{path} > {label}").to_lowercase().contains(&query)
}

/// The next selected index when moving `delta` (±1) through `visible` (the
/// indices of the currently visible rows), wrapping around. `None` returns
/// the first (for a downward move) / last (for an upward move).
fn selection_step(visible: &[usize], current: Option<usize>, delta: i32) -> Option<usize> {
	if visible.is_empty() {
		return None;
	}
	let position = current.and_then(|c| visible.iter().position(|&v| v == c));
	let next = match position {
		Some(pos) => (pos as i64 + delta as i64).rem_euclid(visible.len() as i64) as usize,
		None if delta > 0 => 0,
		None => visible.len() - 1,
	};
	Some(visible[next])
}

// ---------------------------------------------------------------------------
// About (帮助 → 关于 Oak)
// ---------------------------------------------------------------------------

/// The About dialog's content: the app name + version, the GPL-3.0 license
/// line and the Olive fork notice (the C++ AboutDialog's text block; the
/// patrons scroller is not ported).
pub struct AboutContent;

impl AboutContent {
	/// Builds the static about text.
	pub fn new() -> Self {
		Self
	}
}

impl Render for AboutContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		div()
			.flex()
			.flex_col()
			.gap_2()
			.child(
				div()
					.text_color(colors.text)
					.font_weight(gpui::FontWeight::BOLD)
					.child(format!("Oak Video Editor {}", env!("CARGO_PKG_VERSION"))),
			)
			.child(
				div()
					.text_color(colors.text)
					.child(i18n::tr("about.description")),
			)
			.child(
				div()
					.text_color(colors.text)
					.child(i18n::tr("about.thanks")),
			)
			.child(
				div()
					.text_color(colors.disabled)
					.text_xs()
					.child(i18n::tr("about.fork_notice")),
			)
	}
}

// ---------------------------------------------------------------------------
// Sequence: 新建序列 (New Sequence) + 序列属性 (Sequence Properties)
// ---------------------------------------------------------------------------

/// The frame-rate choices offered for a sequence, as rational pairs in
/// dropdown order (matching [`SEQUENCE_RATE_OPTIONS`]).
const SEQUENCE_RATES: &[(u32, u32)] = &[
	(24000, 1001), // 23.98
	(24, 1),       // 24
	(25, 1),       // 25
	(30000, 1001), // 29.97
	(30, 1),       // 30
	(50, 1),       // 50
	(60000, 1001), // 59.94
	(60, 1),       // 60
];

/// The frame-rate labels, in the same order as [`SEQUENCE_RATES`].
const SEQUENCE_RATE_OPTIONS: &[&str] = &["23.98", "24", "25", "29.97", "30", "50", "59.94", "60"];

/// The number of preset entries (indices 1..=COUNT; 0 is the custom entry,
/// which leaves the width / height / frame-rate fields free).
const SEQUENCE_PRESET_COUNT: usize = 6;

/// The preset formats offered for a sequence.
fn sequence_preset_options() -> Vec<ComboBoxOption> {
	vec![
		ComboBoxOption::new(0, i18n::tr("seqprops.preset.custom")),
		ComboBoxOption::new(1, i18n::tr("seqprops.preset.pal")),
		ComboBoxOption::new(2, i18n::tr("seqprops.preset.ntsc")),
		ComboBoxOption::new(3, i18n::tr("seqprops.preset.hd_1080_25")),
		ComboBoxOption::new(4, i18n::tr("seqprops.preset.hd_1080_30")),
		ComboBoxOption::new(5, i18n::tr("seqprops.preset.uhd_4k")),
		ComboBoxOption::new(6, i18n::tr("seqprops.preset.dci_4k")),
	]
}

/// The `(width, height, rate-num, rate-den)` of a preset entry, or `None`
/// for the custom entry.
fn sequence_preset_format(index: usize) -> Option<(u32, u32, u32, u32)> {
	match index {
		1 => Some((720, 576, 25, 1)),       // PAL
		2 => Some((720, 480, 30000, 1001)), // NTSC
		3 => Some((1920, 1080, 25, 1)),     // HD 1080p25
		4 => Some((1920, 1080, 30, 1)),     // HD 1080p30
		5 => Some((3840, 2160, 25, 1)),     // 4K UHD
		6 => Some((4096, 2160, 24, 1)),     // 4K DCI
		_ => None,
	}
}

/// The frame-rate choices for the sequence dialogs.
fn sequence_rate_options() -> Vec<ComboBoxOption> {
	SEQUENCE_RATE_OPTIONS
		.iter()
		.enumerate()
		.map(|(i, label)| ComboBoxOption::new(i, *label))
		.collect()
}

/// A text field for the sequence name, shaped like the export path field
/// (its own editor so the name can be replaced without retyping).
pub struct TextValue {
	editor: Entity<EditableTextState>,
}

impl TextValue {
	/// The name currently entered.
	pub fn value(&self, app: &App) -> SharedString {
		self.editor.read(app).as_str().into()
	}

	/// Replaces the name shown in the field.
	pub fn set_value(&mut self, value: impl Into<SharedString>, cx: &mut Context<Self>) {
		let value = value.into();
		self.editor.update(cx, |editor, cx| {
			editor.emplace(value.as_ref(), cx);
		});
		cx.notify();
	}
}

impl Render for TextValue {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let weak = self.editor.downgrade();
		div()
			.rounded_md()
			.border_1()
			.border_color(colors.border)
			.bg(colors.background)
			.px_2()
			.py_1()
			.child(text_input("oak-seq-name", cx).state(weak).accepts_input(true))
	}
}

/// The initial state of the sequence format fields.
pub struct SequenceFormatSeed {
	/// The selected preset index (0 = custom).
	pub preset: usize,
	/// The width shown in the spin box (presets override it).
	pub width: u32,
	/// The height shown in the spin box.
	pub height: u32,
	/// The selected frame-rate index, or `None` for the custom default.
	pub rate: Option<usize>,
	/// Whether the interlaced checkbox is checked.
	pub interlaced: bool,
}

impl SequenceFormatSeed {
	/// The defaults the new-sequence dialog starts from: HD 1080p25.
	pub fn hd_1080p25() -> Self {
		Self {
			preset: 3,
			width: 1920,
			height: 1080,
			rate: Some(2),
			interlaced: false,
		}
	}

	/// Builds a seed matching a source format (a footage's probed video
	/// stream, or a sequence's current parameters): the matching preset
	/// when the dimensions/rate appear in it, the custom entry otherwise,
	/// and the rate pick's exact match when the rate is one of the common
	/// options (else the custom default 25 must be selected explicitly —
	/// the drop-choice seed follows the probe, so it only seeds what is
	/// known).
	pub fn from_format(format: &crate::oakui::engine::VideoFormat, interlaced: bool) -> Self {
		let preset = (1..=SEQUENCE_PRESET_COUNT)
			.find(|i| {
				sequence_preset_format(*i) == Some((
					format.width,
					format.height,
					format.rate.num,
					format.rate.den,
				))
			})
			.unwrap_or(0);
		let rate = SEQUENCE_RATES
			.iter()
			.position(|r| *r == (format.rate.num, format.rate.den));
		Self {
			preset,
			width: format.width,
			height: format.height,
			rate,
			interlaced,
		}
	}
}

/// The format controls shared by the new-sequence and sequence-properties
/// dialogs: a preset combo that fills the numeric fields, width / height
/// spin boxes, a frame-rate combo and an interlaced checkbox. Picking a
/// preset overwrites the numeric fields; editing any of them snaps the
/// preset back to *custom*.
pub struct SequenceFormatFields {
	preset: Entity<ComboBox>,
	width: Entity<SpinBox>,
	height: Entity<SpinBox>,
	rate: Entity<ComboBox>,
	interlaced: Entity<CheckBox>,
}

impl SequenceFormatFields {
	/// Builds the fields and wires the preset / field cross-updates.
	pub fn build(
		seed: SequenceFormatSeed,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let preset = cx.new(|cx| ComboBox::new(40, sequence_preset_options(), window, cx));
		preset.update(cx, |combo, cx| {
			combo.set_selected(Some(seed.preset), cx)
		});

		let width = cx.new(|cx| {
			SpinBox::new(
				41,
				SliderModel::new(
					ValueKind::Integer,
					16.0,
					8192.0,
					2.0,
					f64::from(seed.width),
				),
				window,
				cx,
			)
		});
		let height = cx.new(|cx| {
			SpinBox::new(
				42,
				SliderModel::new(
					ValueKind::Integer,
					16.0,
					8192.0,
					2.0,
					f64::from(seed.height),
				),
				window,
				cx,
			)
		});

		let rate = cx.new(|cx| ComboBox::new(43, sequence_rate_options(), window, cx));
		rate.update(cx, |combo, cx| combo.set_selected(seed.rate, cx));

		let interlaced = cx.new(|cx| {
			CheckBox::new(
				44,
				if seed.interlaced {
					CheckState::Checked
				} else {
					CheckState::Unchecked
				},
				window,
				cx,
			)
			.with_label(i18n::tr("seqprops.interlaced"))
		});

		// Picking a preset fills the numeric fields. The programmatic
		// set_value/set_selected calls below emit no events, so this never
		// loops back into itself.
		cx.subscribe(&preset, |this, _preset, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { value } = event {
				if let Some((w, h, num, den)) = sequence_preset_format(*value) {
					this.width.update(cx, |spin, cx| {
						spin.set_value(SliderValue::Integer(i64::from(w)), cx)
					});
					this.height.update(cx, |spin, cx| {
						spin.set_value(SliderValue::Integer(i64::from(h)), cx)
					});
					if let Some(index) = SEQUENCE_RATES.iter().position(|r| *r == (num, den)) {
						this.rate
							.update(cx, |combo, cx| combo.set_selected(Some(index), cx));
					}
					cx.notify();
				}
			}
		})
		.detach();

		// Editing a dimension or the frame rate reverts to the custom entry.
		cx.subscribe(&width, |this, _spin, event: &SpinBoxEvent, cx| {
			if let SpinBoxEvent::ValueChanged { .. } = event {
				this.preset.update(cx, |combo, cx| {
					combo.set_selected(Some(0), cx)
				});
				cx.notify();
			}
		})
		.detach();
		cx.subscribe(&height, |this, _spin, event: &SpinBoxEvent, cx| {
			if let SpinBoxEvent::ValueChanged { .. } = event {
				this.preset.update(cx, |combo, cx| {
					combo.set_selected(Some(0), cx)
				});
				cx.notify();
			}
		})
		.detach();
		cx.subscribe(&rate, |this, _combo, event: &ComboBoxEvent, cx| {
			if let ComboBoxEvent::Selected { .. } = event {
				this.preset.update(cx, |combo, cx| {
					combo.set_selected(Some(0), cx)
				});
				cx.notify();
			}
		})
		.detach();

		// The interlaced checkbox is request-only: the host accepts the
		// toggled state back (the standard checkbox pattern).
		cx.subscribe(&interlaced, |_this, check, event: &CheckBoxEvent, cx| {
			if let CheckBoxEvent::Toggled { state, .. } = event {
				check.update(cx, |check, cx| check.set_state(*state, cx));
			}
		})
		.detach();

		Self {
			preset,
			width,
			height,
			rate,
			interlaced,
		}
	}

	/// The video format currently selected in the fields.
	pub fn format(&self, cx: &App) -> crate::oakui::engine::VideoFormat {
		let width = self.width.read(cx).value().to_f64().max(1.0) as u32;
		let height = self.height.read(cx).value().to_f64().max(1.0) as u32;
		let (num, den) = self
			.rate
			.read(cx)
			.selected()
			.and_then(|i| SEQUENCE_RATES.get(i))
			.copied()
			.unwrap_or((25, 1));
		crate::oakui::engine::VideoFormat {
			width,
			height,
			rate: FrameRate::new(num.max(1), den.max(1)),
		}
	}

	/// Whether the interlaced checkbox is checked.
	pub fn interlaced(&self, cx: &App) -> bool {
		self.interlaced.read(cx).state() == CheckState::Checked
	}

	/// The labeled form rows (preset, width/height, frame rate, interlaced).
	pub fn rows(&self, colors: &gpui::colors::Colors) -> gpui::Div {
		let size_row = div()
			.flex()
			.gap_3()
			.child(form_row(
				colors,
				i18n::tr("seqprops.width").into(),
				self.width.clone(),
			))
			.child(form_row(
				colors,
				i18n::tr("seqprops.height").into(),
				self.height.clone(),
			));
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				colors,
				i18n::tr("seqprops.preset").into(),
				self.preset.clone(),
			))
			.child(size_row)
			.child(form_row(
				colors,
				i18n::tr("seqprops.frame_rate").into(),
				self.rate.clone(),
			))
			.child(form_row(
				colors,
				i18n::tr("seqprops.interlaced").into(),
				self.interlaced.clone(),
			))
	}
}

/// The new-sequence dialog content: the sequence name plus the format
/// fields. The host reads the name / format when the OK button is clicked.
pub struct NewSequenceContent<E: crate::oakui::engine::AppEngine> {
	engine: Entity<E>,
	name: Entity<TextValue>,
	format: Entity<SequenceFormatFields>,
	/// The commit error shown under the form (a rejected create keeps the
	/// dialog open).
	error: Option<String>,
}

impl<E: crate::oakui::engine::AppEngine> NewSequenceContent<E> {
	/// Builds the content seeded with the default name and the HD 1080p25
	/// preset.
	pub fn new(engine: Entity<E>, window: &mut Window, cx: &mut Context<Self>) -> Self {
		Self::new_seeded(engine, SequenceFormatSeed::hd_1080p25(), window, cx)
	}

	/// Builds the content seeded with the default name and `seed`'s format
	/// (the footage-probe seed when a drop created the dialog, the HD
	/// 1080p25 defaults otherwise).
	pub fn new_seeded(
		engine: Entity<E>,
		seed: SequenceFormatSeed,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let name = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			TextValue { editor }
		});
		name.update(cx, |field, cx| {
			field.set_value(i18n::tr("seqprops.default_name"), cx)
		});

		let format = cx.new(|cx| SequenceFormatFields::build(seed, window, cx));

		Self {
			engine,
			name,
			format,
			error: None,
		}
	}

	/// The name currently entered.
	pub fn name(&self, cx: &App) -> SharedString {
		self.name.read(cx).value(cx)
	}

	/// The format currently selected.
	pub fn format(&self, cx: &App) -> crate::oakui::engine::VideoFormat {
		self.format.read(cx).format(cx)
	}

	/// Whether the interlaced checkbox is checked.
	pub fn interlaced(&self, cx: &App) -> bool {
		self.format.read(cx).interlaced(cx)
	}

	/// Applies the dialog (the C++ `accept()`): creates the sequence with the
	/// entered name / format and clears the error row.
	pub fn commit(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
		let name = self.name(cx).to_string();
		let format = self.format(cx);
		let interlaced = self.interlaced(cx);
		self.engine.update(cx, |engine, cx| {
			engine.create_sequence_with_params(name, format, interlaced, cx)
		})?;
		self.set_error(None, cx);
		Ok(())
	}

	/// The error shown under the form after a rejected commit.
	pub fn set_error(&mut self, msg: Option<String>, cx: &mut Context<Self>) {
		self.error = msg;
		cx.notify();
	}

	/// The commit error currently shown (`None` while the last commit
	/// applied cleanly).
	pub fn error(&self) -> Option<&String> {
		self.error.as_ref()
	}
}

impl<E: crate::oakui::engine::AppEngine> Render for NewSequenceContent<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let format_rows = self.format.read(cx).rows(&colors);
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("seqprops.name").into(),
				self.name.clone(),
			))
			.child(format_rows)
			.child(
				if let Some(error) = &self.error {
					div()
						.debug_selector(|| "seqprops-error".into())
						.text_color(gpui::rgb(0xe5484d))
						.text_xs()
						.child(error.clone())
				} else {
					div()
				},
			)
	}
}

/// The sequence-properties dialog content: the sequence name plus the
/// format fields, seeded from the sequence's current parameters.
pub struct SequencePropertiesContent<E: crate::oakui::engine::AppEngine> {
	engine: Entity<E>,
	sequence_id: u64,
	name: Entity<TextValue>,
	format: Entity<SequenceFormatFields>,
	/// The commit error shown under the form.
	error: Option<String>,
}

impl<E: crate::oakui::engine::AppEngine> SequencePropertiesContent<E> {
	/// Builds the content seeded from the sequence's current parameters (the
	/// C++ `SequencePropertiesDialog` initializers); a missing sequence
	/// falls back to the HD 1080p25 defaults with a blank name.
	pub fn new(
		engine: Entity<E>,
		sequence_id: u64,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let current = engine.read(cx).sequence_parameters(sequence_id);

		let name = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			TextValue { editor }
		});
		let current_name = current.as_ref().map(|p| p.name.as_str()).unwrap_or("");
		name.update(cx, |field, cx| field.set_value(current_name, cx));

		let seed = match &current {
			Some(params) => {
				let preset = (1..=SEQUENCE_PRESET_COUNT)
					.find(|i| {
						sequence_preset_format(*i)
							== Some((
								params.format.width,
								params.format.height,
								params.format.rate.num,
								params.format.rate.den,
							))
					})
					.unwrap_or(0);
				let rate = SEQUENCE_RATES
					.iter()
					.position(|r| *r == (params.format.rate.num, params.format.rate.den));
				SequenceFormatSeed {
					preset,
					width: params.format.width,
					height: params.format.height,
					rate,
					interlaced: params.interlaced,
				}
			}
			None => SequenceFormatSeed {
				preset: 0,
				width: 1920,
				height: 1080,
				rate: None,
				interlaced: false,
			},
		};
		let format = cx.new(|cx| SequenceFormatFields::build(seed, window, cx));

		Self {
			engine,
			sequence_id,
			name,
			format,
			error: None,
		}
	}

	/// The name currently entered.
	pub fn name(&self, cx: &App) -> SharedString {
		self.name.read(cx).value(cx)
	}

	/// The format currently selected.
	pub fn format(&self, cx: &App) -> crate::oakui::engine::VideoFormat {
		self.format.read(cx).format(cx)
	}

	/// Whether the interlaced checkbox is checked.
	pub fn interlaced(&self, cx: &App) -> bool {
		self.format.read(cx).interlaced(cx)
	}

	/// Applies the edits (the C++ `accept()`): updates the sequence's name /
	/// format and clears the error row.
	pub fn commit(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
		let name = self.name(cx).to_string();
		let format = self.format(cx);
		let interlaced = self.interlaced(cx);
		self.engine.update(cx, |engine, cx| {
			engine.update_sequence_parameters(
				self.sequence_id,
				name,
				format,
				interlaced,
				cx,
			)
		})?;
		self.set_error(None, cx);
		Ok(())
	}

	/// The error shown under the form after a rejected commit.
	pub fn set_error(&mut self, msg: Option<String>, cx: &mut Context<Self>) {
		self.error = msg;
		cx.notify();
	}

	/// The commit error currently shown (`None` while the last commit
	/// applied cleanly).
	pub fn error(&self) -> Option<&String> {
		self.error.as_ref()
	}
}

impl<E: crate::oakui::engine::AppEngine> Render for SequencePropertiesContent<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let format_rows = self.format.read(cx).rows(&colors);
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("seqprops.name").into(),
				self.name.clone(),
			))
			.child(format_rows)
			.child(
				if let Some(error) = &self.error {
					div()
						.debug_selector(|| "seqprops-error".into())
						.text_color(gpui::rgb(0xe5484d))
						.text_xs()
						.child(error.clone())
				} else {
					div()
				},
			)
	}
}

/// The outcome of the drop-onto-empty-timeline choice (拖到空时间轴上的素材
/// 需要先创建一个序列：探测素材参数作为序列参数，还是手工指定参数)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropSequenceChoice {
	/// Create the sequence from the footage's probed video parameters
	/// (the previous auto-create behaviour; the engine's `drop_footage`
	/// sizes the new sequence from the probe).
	UseFootageParams,
	/// Open the new-sequence dialog (seeded with the probed parameters)
	/// and create the sequence with the user's entered settings.
	SpecifyManually,
}

/// The content of the drop-onto-empty-timeline choice dialog: the
/// footage's probed video parameters (or a note when the footage is
/// pure audio). The host routes the modal's buttons to
/// [`DropSequenceChoice`].
pub struct DropSequenceChoiceContent {
	/// The footage's first probed video stream (`width, height, rate,
	/// interlaced`), when the footage carries one.
	probed: Option<(u32, u32, gpui::timeline::FrameRate, bool)>,
}

impl DropSequenceChoiceContent {
	/// Builds the content seeded with the footage's probed video stream.
	pub fn new(probed: Option<(u32, u32, gpui::timeline::FrameRate, bool)>) -> Self {
		Self { probed }
	}

	/// The footage's probed parameters shown in the body.
	pub fn probed(&self) -> Option<(u32, u32, gpui::timeline::FrameRate, bool)> {
		self.probed
	}
}

impl Render for DropSequenceChoiceContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let body = match self.probed {
			Some((width, height, rate, interlaced)) => {
				let base = format!(
					"{} × {}, {}",
					width,
					height,
					crate::oakui::timecode::format_fps(rate),
				);
				if interlaced {
					format!("{base} · {}", i18n::tr("seqprops.interlaced"))
				} else {
					base
				}
			}
			None => i18n::tr("seqprops.drop.no_video").to_string(),
		};
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(
				div()
					.text_color(colors.disabled)
					.child(i18n::tr("seqprops.drop.caption")),
			)
			.child(
				div()
					.debug_selector(|| "drop-choice-params".into())
					.text_color(colors.text)
					.text_sm()
					.child(body),
			)
	}
}

/// The multicam wizard dialog content: pick the angle footage, choose
/// the sync mode, name the multicam sequence. The host reads the inputs
/// through [`Self::selection`] / [`Self::sync_mode`] / [`Self::name`]
/// when the OK button fires.
pub struct MulticamWizardContent<E: crate::oakui::engine::AppEngine> {
	engine: Entity<E>,
	/// The name field (prefilled "Multi-Cam").
	name: Entity<TextValue>,
	/// Sync mode combo (0 = audio waveform, 1 = source timecode, 2 = no
	/// alignment).
	sync: Entity<ComboBox>,
	/// Selection state per footage row: `(entry, checked)`.
	rows: Vec<(crate::oakui::engine::WizardFootage, bool)>,
	/// The wheel to scroll the long angle list.
	scrolled: bool,
}

/// The wizard's sync mode combo values (display order).
const WIZARD_SYNC_MODES: &[&str] = &[
	"multicam.sync.audio",
	"multicam.sync.timecode",
	"multicam.sync.none",
];

impl<E: crate::oakui::engine::AppEngine> MulticamWizardContent<E> {
	/// Builds the content seeded with the engine's wizard footage.
	pub fn new(
		engine: Entity<E>,
		window: &mut Window,
		cx: &mut Context<Self>,
	) -> Self {
		let name = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			TextValue { editor }
		});
		name.update(cx, |field, cx| {
			field.set_value(i18n::tr("multicam.wizard.default_name"), cx)
		});
		let sync = cx.new(|cx| {
			let options = WIZARD_SYNC_MODES
				.iter()
				.enumerate()
				.map(|(i, key)| ComboBoxOption::new(i, i18n::tr(key)))
				.collect();
			ComboBox::new(60, options, window, cx)
		});
		sync.update(cx, |combo, cx| combo.set_selected(Some(0), cx));
		let rows = engine
			.read(cx)
			.multicam_wizard_footage()
			.unwrap_or_default()
			.into_iter()
			.map(|entry| (entry, false))
			.collect();
		Self {
			engine,
			name,
			sync,
			rows,
			scrolled: false,
		}
	}

	/// Toggles the checked state of row `index`.
	pub fn toggle_row(&mut self, index: usize, cx: &mut Context<Self>) {
		if let Some(row) = self.rows.get_mut(index) {
			row.1 = !row.1;
			cx.notify();
		}
	}

	/// The angle entries currently checked, in row order.
	pub fn selection(&self) -> Vec<crate::oakui::engine::WizardFootage> {
		self.rows
			.iter()
			.filter(|row| row.1)
			.map(|row| row.0.clone())
			.collect()
	}

	/// The selected sync mode (0 = audio, 1 = timecode, 2 = none).
	pub fn sync_mode(&self, cx: &App) -> usize {
		self.sync.read(cx).selected().unwrap_or(0)
	}

	/// The sequence name entered.
	pub fn name(&self, cx: &App) -> SharedString {
		self.name.read(cx).value(cx)
	}

	/// Whether the engine offers any wizard footage (drives the empty
	/// hint row).
	pub fn has_rows(&self) -> bool {
		!self.rows.is_empty()
	}

	/// The engine entity (the host needs it to run the sync + create).
	pub fn engine(&self) -> &Entity<E> {
		&self.engine
	}
}

impl<E: crate::oakui::engine::AppEngine> Render for MulticamWizardContent<E> {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		let mut rows_div = div().flex().flex_col().gap_1().w_full();
		for (index, (entry, checked)) in self.rows.iter().enumerate() {
			let is_checked = *checked;
			let row = div()
				.id(ElementId::named_usize("multicam-wizard-angle", index))
				.flex()
				.items_center()
				.gap_2()
				.px_1()
				.py_0p5()
				.rounded_sm()
				.hover(|style| style.bg(colors.container))
				.cursor_pointer()
				.on_click(cx.listener(
					move |this: &mut Self, _event: &ClickEvent, _window, cx| {
						this.toggle_row(index, cx);
					},
				))
				.child(
					div()
						.size(px(12.0))
						.flex()
						.items_center()
						.justify_center()
						.bg(if is_checked { colors.selected } else { colors.background })
						.text_color(colors.text)
						.child(if is_checked { "✓" } else { "" }),
				)
				.child(
					div()
						.flex_1()
						.text_color(colors.text)
						.child(entry.name.clone()),
				)
				.child(
					div()
						.text_xs()
						.text_color(colors.disabled)
						.child(entry.duration_s.map(|d| format!("{d:.1}s")).unwrap_or_default()),
				);
			rows_div = rows_div.child(row);
		}
		let angles = div()
			.id("multicam-wizard-angles")
			.max_h(px(220.0))
			.overflow_y_scroll()
			.child(rows_div);
		let stock = if self.rows.is_empty() {
			div()
				.text_color(colors.disabled)
				.text_xs()
				.child(i18n::tr("multicam.wizard.no_footage"))
				.into_any_element()
		} else {
			div().into_any_element()
		};
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("multicam.wizard.name").into(),
				self.name.clone(),
			))
			.child(form_row(
				&colors,
				i18n::tr("multicam.wizard.sync").into(),
				self.sync.clone(),
			))
			.child(
				div()
					.text_sm()
					.text_color(colors.text)
					.child(i18n::tr("multicam.wizard.angles_label")),
			)
			.child(angles)
			.child(stock)
	}
}

/// The rename dialog: a single text field prefilled with the entry's
/// current name (the project-explorer 重命名 context item; the host reads
/// [`Self::value`] on OK and calls the engine's `rename_entry`).
pub struct RenameContent {
	field: Entity<TextValue>,
}

impl RenameContent {
	/// Builds the dialog seeded with the current name.
	pub fn new(current: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
		let field = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			TextValue { editor }
		});
		field.update(cx, |field, cx| field.set_value(current, cx));
		Self { field }
	}

	/// The name to apply.
	pub fn value(&self, cx: &App) -> SharedString {
		self.field.read(cx).value(cx)
	}
}

impl Render for RenameContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("project.context.rename").into(),
				self.field.clone(),
			))
	}
}

/// The project export dialog (文件 → 导出工程文件…): choose the project
/// file format (OTIO / OVE / FCPXML) and the output path. The host reads
/// [`Self::format`] + [`Self::path`] on OK.
pub struct ExportProjectDialogContent {
	format: Entity<ComboBox>,
	/// (format id, display name) in dropdown order.
	formats: Vec<(i32, String)>,
}

/// The project-file format ids (mirror the engine's serializer dispatch;
/// the extension is derived on the host).
pub const PROJECT_FORMAT_OTIO: i32 = 0;
pub const PROJECT_FORMAT_OVE: i32 = 1;
pub const PROJECT_FORMAT_FCPXML: i32 = 2;

impl ExportProjectDialogContent {
	/// Builds the dialog (OTIO default). The output path is NOT entered
	/// here: OK hands off to the platform save dialog (the suggested name
	/// carries the chosen format's extension).
	pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
		let formats: Vec<(i32, String)> = vec![
			(PROJECT_FORMAT_OTIO, "OpenTimelineIO (.otio)".to_string()),
			(PROJECT_FORMAT_OVE, "Oak (.ove)".to_string()),
			(PROJECT_FORMAT_FCPXML, "Final Cut Pro XML (.fcpxml)".to_string()),
		];
		let options = formats
			.iter()
			.enumerate()
			.map(|(i, (_, name))| ComboBoxOption::new(i, name.clone()))
			.collect();
		let format = cx.new(|cx| {
			ComboBox::new(4, options, window, cx)
				.with_placeholder(i18n::tr("project.export.format"))
		});
		format.update(cx, |combo, cx| combo.set_selected(Some(PROJECT_FORMAT_OTIO as usize), cx));
		Self { format, formats }
	}

	/// The selected project format id.
	pub fn format(&self, cx: &App) -> i32 {
		let Some(selected) = self.format.read(cx).selected() else {
			return PROJECT_FORMAT_OTIO;
		};
		self.formats
			.get(selected)
			.map(|(id, _)| *id)
			.unwrap_or(PROJECT_FORMAT_OTIO)
	}

	/// The format's file extension (no dot).
	pub fn extension(&self, cx: &App) -> &'static str {
		match self.format(cx) {
			PROJECT_FORMAT_OVE => "ove",
			PROJECT_FORMAT_FCPXML => "fcpxml",
			_ => "otio",
		}
	}
}

impl Render for ExportProjectDialogContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("project.export.format").into(),
				self.format.clone(),
			))
	}
}

/// The new-project dialog (文件 → 新建项目): project name + the first
/// sequence's format (preset / custom width-height-rate / interlaced).
/// The host reads [`Self::name`] + [`Self::format`]; the engine's
/// library_create_project then seeds the first sequence.
pub struct NewProjectContent {
	name: Entity<TextValue>,
	format: Entity<SequenceFormatFields>,
}

impl NewProjectContent {
	/// Builds the dialog (default name, HD 1080p25).
	pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
		let name = cx.new(|cx| {
			let editor = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
			TextValue { editor }
		});
		name.update(cx, |field, cx| {
			field.set_value(i18n::tr("manager.new.default_name"), cx)
		});
		let format = cx.new(|cx| {
			SequenceFormatFields::build(SequenceFormatSeed::hd_1080p25(), window, cx)
		});
		Self { name, format }
	}

	/// The project name entered.
	pub fn name(&self, cx: &App) -> SharedString {
		self.name.read(cx).value(cx)
	}

	/// The format selected.
	pub fn format(&self, cx: &App) -> crate::oakui::engine::VideoFormat {
		self.format.read(cx).format(cx)
	}

	/// The interlaced flag.
	pub fn interlaced(&self, cx: &App) -> bool {
		self.format.read(cx).interlaced(cx)
	}
}

impl Render for NewProjectContent {
	fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
		let colors = cx.default_colors().clone();
		div()
			.flex()
			.flex_col()
			.gap_3()
			.w_full()
			.child(form_row(
				&colors,
				i18n::tr("manager.new.project_name").into(),
				self.name.clone(),
			))
			.child(self.format.read(cx).rows(&colors))
	}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keystroke(key: &str) -> Keystroke {
		gpui::Keystroke::parse(key).unwrap()
	}

	/// The export dialog's sequence picker: the host populates it from
	/// the project, a known preselection wins, an unknown one falls back
	/// to the first entry, and an empty project leaves nothing to export.
	#[gpui::test]
	async fn export_dialog_sequence_picker(cx: &mut gpui::TestAppContext) {
		cx.update(|cx| cx.init_colors());
		let window = cx.open_window(gpui::size(gpui::px(440.0), gpui::px(400.0)), |window, cx| {
			ExportDialogContent::new(window, cx)
		});
		cx.run_until_parked();
		let content = window.root(cx).expect("dialog content root");

		let sequences = vec![
			(7u64, SharedString::from("Opening")),
			(9u64, SharedString::from("Finale")),
		];
		cx.update(|cx| {
			content.update(cx, |content, cx| {
				content.set_sequences(sequences.clone(), Some(9), cx)
			});
		});
		assert_eq!(
			cx.read(|cx| content.read(cx).selected_sequence(cx)),
			Some(9),
			"the known preselection wins"
		);

		cx.update(|cx| {
			content.update(cx, |content, cx| {
				content.set_sequences(sequences.clone(), Some(42), cx)
			});
		});
		assert_eq!(
			cx.read(|cx| content.read(cx).selected_sequence(cx)),
			Some(7),
			"an unknown preselection falls back to the first entry"
		);

		cx.update(|cx| {
			content.update(cx, |content, cx| content.set_sequences(Vec::new(), None, cx));
		});
		assert_eq!(
			cx.read(|cx| content.read(cx).selected_sequence(cx)),
			None,
			"an empty project leaves nothing to export"
		);
	}

	/// The 4K presets are in the dropdown and resolve to their formats
	/// (4K UHD 3840×2160@25, 4K DCI 4096×2160@24).
	#[test]
	fn four_k_presets_are_offered_and_resolve() {
		let options = sequence_preset_options();
		let labels: Vec<&str> = options.iter().map(|o| o.label.as_ref()).collect();
		assert_eq!(options.len(), 7);
		assert_eq!(sequence_preset_format(5), Some((3840, 2160, 25, 1)));
		assert_eq!(sequence_preset_format(6), Some((4096, 2160, 24, 1)));
		assert_eq!(sequence_preset_format(7), None);
		// The preset labels come from the language packs (default English
		// here): the 4K entries say what they set.
		assert!(labels[5].contains("3840×2160"), "4K UHD label: {}", labels[5]);
		assert!(labels[6].contains("4096×2160"), "4K DCI label: {}", labels[6]);
	}

	/// A probed footage format seeds the new-sequence fields: an exact
	/// preset match selects the preset, otherwise the custom entry with the
	/// probed dimensions/rate already filled in.
	#[test]
	fn seed_from_format_matches_presets_and_falls_back_to_custom() {
		let hd = crate::oakui::engine::VideoFormat {
			width: 1920,
			height: 1080,
			rate: FrameRate::new(25, 1),
		};
		let seed = SequenceFormatSeed::from_format(&hd, false);
		assert_eq!(seed.preset, 3);
		assert_eq!((seed.width, seed.height), (1920, 1080));
		assert_eq!(seed.rate, Some(2));

		let custom = crate::oakui::engine::VideoFormat {
			width: 512,
			height: 512,
			rate: FrameRate::new(1234, 100),
		};
		let seed = SequenceFormatSeed::from_format(&custom, true);
		assert_eq!(seed.preset, 0);
		assert_eq!((seed.width, seed.height), (512, 512));
		assert_eq!(seed.rate, None);
		assert!(seed.interlaced);

		let dci = crate::oakui::engine::VideoFormat {
			width: 4096,
			height: 2160,
			rate: FrameRate::new(24, 1),
		};
		let seed = SequenceFormatSeed::from_format(&dci, false);
		assert_eq!(seed.preset, 6, "4K DCI resolves to its preset");
	}

	/// The sequence properties dialog seeds the preset dropdown correctly for
	/// every preset value, so reopening properties on a 4K sequence keeps
	/// 4K selected (not "custom").
	#[test]
	fn properties_seed_matches_all_presets_including_4k() {
		for index in 1..=SEQUENCE_PRESET_COUNT {
			let (w, h, num, den) = sequence_preset_format(index).unwrap();
			let params = crate::oakui::engine::SequenceParameters {
				name: "4K".into(),
				format: crate::oakui::engine::VideoFormat {
					width: w,
					height: h,
					rate: FrameRate::new(num, den),
				},
				interlaced: false,
			};
			let seed = SequenceFormatSeed::from_format(&params.format, params.interlaced);
			assert_eq!(seed.preset, index, "preset {index} reselects itself");
		}
	}

	#[test]
	fn capture_decision_handles_all_shapes() {
		assert!(matches!(
			capture_decision(&keystroke("escape")),
			CaptureDecision::Cancel
		));
		assert!(matches!(
			capture_decision(&keystroke("backspace")),
			CaptureDecision::Clear
		));
		assert!(matches!(
			capture_decision(&keystroke("delete")),
			CaptureDecision::Clear
		));
		// Bare modifiers never bind.
		assert!(matches!(
			capture_decision(&keystroke("shift")),
			CaptureDecision::Ignore
		));
		assert!(matches!(
			capture_decision(&keystroke("control")),
			CaptureDecision::Ignore
		));
		assert!(matches!(
			capture_decision(&keystroke("alt")),
			CaptureDecision::Ignore
		));
		// A real key (with or without modifiers) becomes the canonical binding.
		match capture_decision(&keystroke("secondary-s")) {
			CaptureDecision::Assign(canon) => {
				assert_eq!(
					canon,
					gpui::Keystroke::parse("secondary-s").unwrap().unparse()
				);
			}
			other => panic!("expected assign, got {other:?}"),
		}
	}

	#[test]
	fn keyboard_filter_matches_name_path_and_shortcut() {
		assert!(keyboard_filter_matches(
			"Save Project",
			"File",
			Some("⌘S"),
			"save"
		));
		assert!(keyboard_filter_matches(
			"Save Project",
			"File",
			Some("⌘S"),
			"file > save"
		));
		// Shortcut matching is case-insensitive.
		assert!(keyboard_filter_matches(
			"Save Project",
			"File",
			Some("⌘S"),
			"⌘s"
		));
		assert!(!keyboard_filter_matches(
			"Save Project",
			"File",
			Some("⌘S"),
			"undo"
		));
		// Empty query matches everything; rows without a shortcut match only
		// by name/path.
		assert!(keyboard_filter_matches("About Oak…", "Help", None, ""));
		assert!(keyboard_filter_matches("About Oak…", "Help", None, "help"));
		assert!(!keyboard_filter_matches("About Oak…", "Help", None, "⌘"));
	}

	#[test]
	fn search_filter_matches_label_or_path() {
		let _guard = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");
		assert!(search_filter_matches(ActionId::NewProject, "File", "new"));
		assert!(search_filter_matches(
			ActionId::NewProject,
			"File",
			"file > new"
		));
		assert!(!search_filter_matches(ActionId::NewProject, "File", "undo"));
		assert!(search_filter_matches(ActionId::NewProject, "File", ""));
	}

	#[test]
	fn selection_step_wraps_and_respects_visibility() {
		assert_eq!(selection_step(&[2, 5, 7], None, 1), Some(2));
		assert_eq!(selection_step(&[2, 5, 7], None, -1), Some(7));
		assert_eq!(selection_step(&[2, 5, 7], Some(2), 1), Some(5));
		assert_eq!(selection_step(&[2, 5, 7], Some(7), 1), Some(2));
		assert_eq!(selection_step(&[2, 5, 7], Some(2), -1), Some(7));
		assert_eq!(selection_step(&[], None, 1), None);
	}

	#[test]
	fn keyboard_rows_cover_the_menu_bar() {
		let _guard = crate::actions::shortcuts_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let _lang = crate::i18n::lang_test_lock()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		crate::i18n::set_language_code("en-US");
		let rows = crate::app::menu_action_paths();
		assert!(!rows.is_empty());
		// Every listed action resolves to a registry entry with a menu item.
		for (action, path) in &rows {
			assert_ne!(action.menu_id(), crate::actions::HIDDEN_MENU_ID);
			assert!(!path.is_empty(), "action {action:?} has an empty path");
		}
	}
}
