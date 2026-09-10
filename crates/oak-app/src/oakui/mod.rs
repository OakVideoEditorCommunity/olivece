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

//! `oakui` — the app layer's engine seam and view-state logic.
//!
//! This module is what the UI talks to when it needs something from "the
//! engine", plus the pure view-state logic the panels build on:
//!
//! * [`engine`] — the [`EngineGateway`](engine::EngineGateway) trait and the
//!   project/sequence model. The seam itself: panels and the shell hold an
//!   `Entity` whose type implements this trait and never care about the
//!   backend.
//! * [`mock`] — [`MockEngine`](mock::MockEngine) and
//!   [`MockClock`](mock::MockClock), the demo implementation feeding every
//!   widget's data-source trait.
//! * [`real`] — [`RealEngine`](real::RealEngine) and
//!   [`RealClock`](real::RealClock), the real engine. M14 R3: it calls the
//!   oak* module crates' Rust APIs directly (oaknode / oaktimeline /
//!   oakundo / oakrender / oaktask / oakcodec / oakaudio / oak_core /
//!   oakstorage — no `liboakengine` dylib, no C ABI) behind the same
//!   [`EngineGateway`](engine::EngineGateway) seam the mock implements.
//! * [`graphops`] / [`effectchain`] / [`renderops`] — the app's assembly
//!   layer over the module crates: project/timeline/storage helpers, the
//!   effect-chain composition, and montage/render/export drivers.
//! * [`transport`] — the play/pause/step/seek state machine (pure, unit
//!   tested).
//! * [`timecode`] — timecode / duration / fps / resolution formatting (pure,
//!   unit tested).
//! * [`textengine`] — the cosmic-text backend behind the text generator
//!   nodes: the measure/render hooks they call through (`install`), and the
//!   system font list feeding their font-family combo.

pub mod audio_thread;
pub mod component;
pub mod displaycolor;
pub mod effectchain;
pub mod engine;
pub mod frames;
pub mod graphops;
pub mod gpu;
pub mod icons;
pub mod mock;
pub mod multicam;
pub mod nodegraph;
pub mod ofx;
pub mod projectbrowser;
pub mod real;
pub mod renderops;
pub mod scopes;
pub mod textengine;
pub mod timecode;
pub mod transport;
pub mod waveform;
pub mod waveformsync;

pub use engine::{
	AppEngine, EffectEntry, EffectParam, EngineClock, EngineGateway, ExportEvent, ExportSession,
	HistoryEntry, LibraryProject, Monitor, MulticamState, NodeLibraryEntry, Project, ScopeData,
	Sequence, VideoFormat,
};
pub use mock::{MockClock, MockEngine};
pub use real::{RealClock, RealEngine};

/// Whether `name` (a media file name) denotes audio-only media, by
/// extension.
///
/// The module footage is not reliably probed on import, so the timeline
/// drop's track matching falls back to the extension: known audio
/// containers count as audio, everything else as video.
pub fn filename_is_audio(name: &str) -> bool {
	matches!(
		std::path::Path::new(name)
			.extension()
			.and_then(|e| e.to_str())
			.map(|e| e.to_ascii_lowercase())
			.as_deref(),
		Some(
			"wav" | "mp3" | "flac" | "aac" | "ogg" | "oga" | "opus" | "m4a" | "wma" | "aiff"
				| "aif" | "ac3" | "amr" | "ape" | "caf"
		)
	)
}
pub use transport::{PlayState, TransportState};
