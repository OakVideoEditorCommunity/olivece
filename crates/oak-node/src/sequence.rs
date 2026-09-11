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

//! Sequence: the C++ `ViewerOutput`/`Sequence` pair — a node that owns
//! tracks, markers, work area, and playback caches.
//! `// CPP-PARITY: src/node/src/output/viewer/viewer.{h,cpp}`,
//! `// CPP-PARITY: src/node/src/project/sequence/sequence.{h,cpp}`.

use crate::id::NodeId;
use crate::input::Input;
use crate::node::{Category, NodeBehavior, NodeCore};
use crate::value::{AudioParams, NodeValue, ValueType, VideoParams};

/// Sequence texture input id (C++ `ViewerOutput::k_texture_input`).
pub const TEXTURE_INPUT: &str = "tex_in";
/// Sequence audio samples input id (C++ `ViewerOutput::k_samples_input`).
pub const SAMPLES_INPUT: &str = "samples_in";
/// Sequence per-track input id format (C++ `Sequence::k_track_input_format`).
pub const TRACK_INPUT_FORMAT: &str = "track_in_%1";

/// Sequence behavior (viewer node).
pub struct SequenceBehavior {
	/// Track list node ids (video then audio, C++ order).
	pub track_lists: Vec<NodeId>,
	/// Timeline markers handle (oaktimeline, owned; created lazily by
	/// the facade through the C ABI).
	pub markers: crate::handle::CHandle,
	/// Work area handle (oaktimeline, owned; created lazily by the
	/// facade through the C ABI).
	pub workarea: crate::handle::CHandle,
	/// Length cache (C++ last_length_).
	pub last_length: oak_core::Rational,
	/// Autocache toggles.
	pub autocache_video: bool,
	/// Audio autocache toggle.
	pub autocache_audio: bool,
	/// Playhead position (C++ ViewerOutput::playhead_).
	pub playhead: oak_core::Rational,
	/// Video parameter streams.
	pub video_params: Vec<VideoParams>,
	/// Audio parameter streams.
	pub audio_params: Vec<AudioParams>,
}

impl SequenceBehavior {
	/// Empty sequence with zero tracks and no parameters.
	pub fn new() -> Self {
		SequenceBehavior {
			track_lists: Vec::new(),
			markers: crate::handle::CHandle::null(),
			workarea: crate::handle::CHandle::null(),
			last_length: oak_core::Rational::new(0, 1),
			autocache_video: false,
			autocache_audio: false,
			playhead: oak_core::Rational::new(0, 1),
			video_params: Vec::new(),
			audio_params: Vec::new(),
		}
	}

	/// Constructor: the C++ `Sequence` input surface with default
	/// parameters but no track lists (`// CPP-PARITY: sequence.cpp:36`,
	/// `viewer.cpp:84`). Used by the serializer to rebuild a sequence
	/// from a file — the track lists arrive as separate nodes.
	pub fn create() -> (NodeCore, Box<dyn NodeBehavior>) {
		let mut core = NodeCore::new();
		// Bin item (C++ `sequence.cpp:37` `set_flag(k_is_item)`): nested
		// sequences are shared, never cloned, by dependency-graph copies.
		core.flags |= crate::node::flags::IS_ITEM;
		// Viewer parameter streams (C++ ViewerOutput::kVideoParamsInput /
		// kAudioParamsInput / kSubtitleParamsInput arrays).
		for (id, ty) in [
			("video_param_in", ValueType::VideoParams),
			("audio_param_in", ValueType::AudioParams),
			("subtitle_param_in", ValueType::None),
		] {
			let mut input = Input::new(id, ty, NodeValue::None);
			input.flags |= crate::input::flags::NOT_CONNECTABLE
				| crate::input::flags::NOT_KEYFRAMABLE
				| crate::input::flags::ARRAY
				| crate::input::flags::HIDDEN;
			core.add_input(input);
		}
		core.add_input(Input::new(
			TEXTURE_INPUT,
			ValueType::Texture,
			NodeValue::None,
		));
		core.add_input(Input::new(
			SAMPLES_INPUT,
			ValueType::Samples,
			NodeValue::None,
		));
		// One array input per track list (`track_in_%1`; the video list
		// owns track_in_0, audio track_in_1, subtitle track_in_2 —
		// `// CPP-PARITY: sequence.h`).
		for base in 0..3 {
			let mut track_input = Input::new(
				&TRACK_INPUT_FORMAT.replace("%1", &base.to_string()),
				ValueType::None,
				NodeValue::None,
			);
			track_input.flags |= crate::input::flags::ARRAY;
			core.add_input(track_input);
		}
		let mut behavior = SequenceBehavior::new();
		behavior.set_default_parameters();
		(core, Box::new(behavior))
	}

	/// Apply the default video/audio parameters (C++
	/// `ViewerOutput::set_default_parameters()`; the config lookups read
	/// the oak_core config store directly).
	pub fn set_default_parameters(&mut self) {
		let config = oak_core::configstore::ConfigStore::instance();
		let width = config.get_int(None, "DefaultSequenceWidth", 1920);
		let height = config.get_int(None, "DefaultSequenceHeight", 1080);
		let sample_rate = config.get_int(None, "DefaultSequenceAudioFrequency", 48000);
		// The canonical setting is the "num/den" string
		// `DefaultSequenceFrameRate` (a frame DURATION; the rate is
		// den/num, see oak-task's footage import). The `...Num`/`...Den`
		// keys this used to read never existed anywhere in the config, so
		// every sequence silently fell back to 30/1 — while the app-side
		// playback clock defaulted to 25, and the audio chunks (sized by
		// the sequence time base) chronically underfed the output device.
		let (tb_num, tb_den) = config
			.get(None, "DefaultSequenceFrameRate")
			.ok()
			.and_then(|s| {
				let (n, d) = s.split_once('/')?;
				Some((n.trim().parse::<i64>().ok()?, d.trim().parse::<i64>().ok()?))
			})
			.filter(|(n, d)| *n > 0 && *d > 0)
			.unwrap_or((1001, 30000));

		self.video_params = vec![VideoParams {
			width,
			height,
			frame_rate: oak_core::Rational::new(tb_den, tb_num),
			pixel_format: 4, // f32
			channels: 4,
			interlaced: false,
		}];
		self.audio_params = vec![AudioParams {
			sample_rate,
			channel_layout: 0x3, // stereo
			format: 4,           // f32
		}];
	}

	/// Recompute the cached lengths from the track lists (C++
	/// `ViewerOutput::verify_length()`).
	pub fn verify_length(
		&mut self,
		lengths: (
			oak_core::Rational,
			oak_core::Rational,
			oak_core::Rational,
		),
	) {
		let (video, audio, overall) = lengths;
		self.last_length = overall;
		let _ = (video, audio);
	}

	/// Total stream counts.
	pub fn video_stream_count(&self) -> usize {
		self.video_params.len()
	}

	/// Audio stream count.
	pub fn audio_stream_count(&self) -> usize {
		self.audio_params.len()
	}
}

impl NodeBehavior for SequenceBehavior {
	fn name(&self) -> &str {
		"Sequence"
	}

	fn type_id(&self) -> &str {
		"org.olivevideoeditor.Olive.sequence"
	}

	fn categories(&self) -> &[Category] {
		&[Category::Output]
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(SequenceBehavior::new()))
	}

	/// Custom project save (C++ `Sequence::SaveCustom` writes the
	/// workarea and markers; those live behind opaque oaktimeline
	/// handles in Rust, so the track-list references are all that
	/// persists).
	fn save_custom(&self, core: &NodeCore, writer: &mut dyn crate::serializer::XmlWrite) {
		let _ = core;
		if !self.track_lists.is_empty() {
			writer.start_element("tracklists");
			for t in &self.track_lists {
				writer.text_element("tracklist", &t.identity().to_string());
			}
			writer.end_element(); // tracklists
		}
	}

	/// Custom project load (C++ `Sequence::LoadCustom`): the track-list
	/// references are collected here and resolved to live ids by the
	/// serializer's post-load pass; the C++ workarea/markers segments
	/// are skipped (opaque handles).
	fn load_custom(
		&mut self,
		_core: &mut NodeCore,
		reader: &mut dyn crate::serializer::XmlRead,
	) -> bool {
		while reader.next_start_element() {
			match reader.name() {
				"tracklists" => {
					while reader.next_start_element() {
						if reader.name() == "tracklist" {
							if let Some(id) =
								crate::serializer::parse_node_ref(&reader.read_element_text())
							{
								self.track_lists.push(id);
							}
						} else {
							reader.skip_current_element();
						}
					}
				}
				_ => reader.skip_current_element(),
			}
		}
		true
	}

	fn as_any(&self) -> Option<&dyn std::any::Any> {
		Some(self)
	}

	fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
		Some(self)
	}
}

impl Default for SequenceBehavior {
	fn default() -> Self {
		SequenceBehavior::new()
	}
}
