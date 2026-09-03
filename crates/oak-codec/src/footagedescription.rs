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

//! `olive::FootageDescription` — codec-internal stream inventory.
//!
//! Mirrors `src/codec/src/footagedescription.h`. A value type describing
//! the streams a `Decoder::probe()` found in a file. Video and subtitle
//! streams are stored as oak_core by-value handles; audio streams as
//! `oak_core::TimeRangeList`/raw audio params. The original's
//! `Track::Type` mapping and XML load/save are intentionally not reproduced
//! (NOTES.md §4) — use [`FootageDescription::stream_is_video`] etc.

use oak_core::subtitleparams::SubtitleParams;
use oak_core::videoparams::VideoParams;
use oak_core::{Rational, TimeRange};

use crate::audioparams::AudioParams;

/// One stream entry in a footage description.
#[derive(Clone, Debug)]
pub enum StreamEntry {
	/// A video stream.
	Video(VideoParams),
	/// An audio stream.
	Audio(AudioParams),
	/// A subtitle stream.
	Subtitle(SubtitleParams),
}

/// `olive::FootageDescription` — the decoder name plus stream inventory.
#[derive(Clone, Debug, Default)]
pub struct FootageDescription {
	/// The decoder id that produced this description.
	decoder: String,
	/// Total number of streams (video + audio + subtitle).
	total_stream_count: usize,
	/// The streams, in probe order.
	streams: Vec<StreamEntry>,
	/// The media has a source start time.
	has_source_start_time: bool,
	/// Source start time (when present).
	source_start_time: Rational,
	/// Total duration across streams (may be empty).
	duration: Option<TimeRange>,
}

impl FootageDescription {
	/// New, empty description with the given decoder id.
	pub fn new(decoder: &str) -> Self {
		Self {
			decoder: decoder.to_string(),
			total_stream_count: 0,
			streams: Vec::new(),
			has_source_start_time: false,
			// C++ default-constructs the Rational member, i.e. the 0/0
			// null sentinel. It is not meaningful until a source start time
			// is set.
			source_start_time: Rational::default(),
			duration: None,
		}
	}

	/// The decoder id.
	pub fn decoder(&self) -> &str {
		&self.decoder
	}

	/// Total stream count.
	pub fn total_stream_count(&self) -> usize {
		self.total_stream_count
	}

	/// Number of video streams.
	pub fn video_stream_count(&self) -> usize {
		self.streams
			.iter()
			.filter(|s| matches!(s, StreamEntry::Video(_)))
			.count()
	}

	/// Number of audio streams.
	pub fn audio_stream_count(&self) -> usize {
		self.streams
			.iter()
			.filter(|s| matches!(s, StreamEntry::Audio(_)))
			.count()
	}

	/// Number of subtitle streams.
	pub fn subtitle_stream_count(&self) -> usize {
		self.streams
			.iter()
			.filter(|s| matches!(s, StreamEntry::Subtitle(_)))
			.count()
	}

	/// Whether the `index`-th stream (in probe order) is a video stream.
	pub fn stream_is_video(&self, index: usize) -> bool {
		self.streams
			.get(index)
			.is_some_and(|s| matches!(s, StreamEntry::Video(_)))
	}

	/// Whether the `index`-th stream (in probe order) is an audio stream.
	pub fn stream_is_audio(&self, index: usize) -> bool {
		self.streams
			.get(index)
			.is_some_and(|s| matches!(s, StreamEntry::Audio(_)))
	}

	/// Whether the `index`-th stream (in probe order) is a subtitle stream.
	pub fn stream_is_subtitle(&self, index: usize) -> bool {
		self.streams
			.get(index)
			.is_some_and(|s| matches!(s, StreamEntry::Subtitle(_)))
	}

	/// The `index`-th video stream's params (by video-stream ordinal).
	pub fn get_video_stream(&self, index: usize) -> Option<&VideoParams> {
		self.streams
			.iter()
			.filter_map(|s| match s {
				StreamEntry::Video(p) => Some(p),
				_ => None,
			})
			.nth(index)
	}

	/// The `index`-th audio stream's params (by audio-stream ordinal).
	pub fn get_audio_stream(&self, index: usize) -> Option<&AudioParams> {
		self.streams
			.iter()
			.filter_map(|s| match s {
				StreamEntry::Audio(p) => Some(p),
				_ => None,
			})
			.nth(index)
	}

	/// The `index`-th subtitle stream's params (by subtitle-stream ordinal).
	pub fn get_subtitle_stream(&self, index: usize) -> Option<&SubtitleParams> {
		self.streams
			.iter()
			.filter_map(|s| match s {
				StreamEntry::Subtitle(p) => Some(p),
				_ => None,
			})
			.nth(index)
	}

	/// Whether the media has a source start time.
	pub fn has_source_start_time(&self) -> bool {
		self.has_source_start_time
	}

	/// Source start time.
	pub fn source_start_time(&self) -> Rational {
		self.source_start_time
	}

	/// Total duration across streams.
	pub fn duration(&self) -> Option<TimeRange> {
		self.duration
	}

	/// Append one stream entry, mirroring the C++ `add_*_stream` family.
	///
	/// Test/extension support: the ffi probe tests build
	/// `FootageDescription`s through a fake decoder and need a way to
	/// populate them. Hidden from docs; never called by production code.
	#[doc(hidden)]
	pub fn push_stream(&mut self, entry: StreamEntry) {
		self.total_stream_count += 1;
		self.streams.push(entry);
	}

	/// Set the total stream count, mirroring the C++ `set_stream_count`
	/// (which records every stream in the container, including ones the
	/// probe could not describe and therefore did not push).
	///
	/// Hidden from docs; probe/extension support only.
	#[doc(hidden)]
	pub fn set_stream_count(&mut self, count: usize) {
		self.total_stream_count = count;
	}

	/// Set the source start time, mirroring the C++ `set_source_start_time`.
	/// `source` is the raw metadata source kind (unused by the Rust port).
	///
	/// Hidden from docs; probe/extension support only.
	#[doc(hidden)]
	pub fn set_source_start_time(&mut self, time: Rational, _source: i32) {
		self.has_source_start_time = true;
		self.source_start_time = time;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn video_params(index: i32) -> VideoParams {
		let mut vp = VideoParams::new_basic(
            1920,
            1080,
            oak_core::ocioutils::PixelFormat::from_code(0),
            4,
            1,
            1,
            0,
            1,
		);
		vp.set_stream_index(index);
		vp
	}

	fn audio_params() -> AudioParams {
		AudioParams {
			sample_rate: 48000,
			channel_layout: 0x3,
			format: 0,
			stream_index: 1,
			duration: 0,
			time_base: (1, 48000),
		}
	}

	fn subtitle_params() -> SubtitleParams {
		SubtitleParams::new()
	}

	#[test]
	fn new_description_is_empty() {
		let d = FootageDescription::new("ffmpeg");
		assert_eq!(d.decoder(), "ffmpeg");
		assert_eq!(d.total_stream_count(), 0);
		assert_eq!(d.video_stream_count(), 0);
		assert_eq!(d.audio_stream_count(), 0);
		assert_eq!(d.subtitle_stream_count(), 0);
		assert!(!d.has_source_start_time());
		assert!(d.duration().is_none());
	}

	#[test]
	fn push_stream_updates_counts_and_queries() {
		let mut d = FootageDescription::new("mock");
		d.push_stream(StreamEntry::Video(video_params(0)));
		d.push_stream(StreamEntry::Audio(audio_params()));
		d.push_stream(StreamEntry::Subtitle(subtitle_params()));
		d.push_stream(StreamEntry::Video(video_params(1)));

		assert_eq!(d.total_stream_count(), 4);
		assert_eq!(d.video_stream_count(), 2);
		assert_eq!(d.audio_stream_count(), 1);
		assert_eq!(d.subtitle_stream_count(), 1);

		// stream_is_* by probe order.
		assert!(d.stream_is_video(0));
		assert!(d.stream_is_audio(1));
		assert!(d.stream_is_subtitle(2));
		assert!(d.stream_is_video(3));

		// Ordinal getters.
		assert!(d.get_video_stream(0).is_some());
		assert!(d.get_video_stream(1).is_some());
		assert!(d.get_video_stream(2).is_none());
		assert!(d.get_audio_stream(0).is_some());
		assert!(d.get_audio_stream(1).is_none());
		assert!(d.get_subtitle_stream(0).is_some());
		assert!(d.get_subtitle_stream(1).is_none());
	}
}
