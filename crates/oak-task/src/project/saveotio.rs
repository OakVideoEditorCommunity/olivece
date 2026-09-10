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

//! `SaveOTIOTask`, mirroring `src/task/src/project/saveotio/saveotio.h`.
//!
//! Serializes a borrowed project to an OpenTimelineIO (`.otio`) or FCPXML
//! (`.fcpxml`) file through the pure-Rust `oakotio` binding (see `README`
//! decision #6): the project's sequences become `OTIO::Timeline`s
//! (`serialize_timeline` / `serialize_track` / `serialize_track_list`),
//! exactly like the C++ `serialize_*` helpers. The format is dispatched
//! from the filename extension (see [`crate::project::format`]) at the
//! final write only — the serialization itself is shared.
//!
//! **Single-lib note**: the project graph is read through the direct
//! oaknode domain operations in [`crate::nodeops`] (folder children,
//! sequence track lists, track blocks, footage parameters) instead of the
//! deleted oaknode C ABI stubs; the project is an
//! `Arc<Mutex<oak_node::project::Project>>` instead of a borrowed `CHandle`.
//!
//! CPP-PARITY: src/task/src/project/saveotio/saveotio.cpp

use oak_core::Rational;
use oak_node::id::NodeId;
use oak_node::track::TrackType;
use oak_otio::{
	Clip, Composable, ExternalReference, Gap, MediaReference, RationalTime, Serializable,
	SerializableCollection, TimeRange, Timeline, Track, Transition,
};

use crate::error::{Error, Result};
use crate::nodeops::{self, ProjectRef};
use crate::project::format::InterchangeFormat;
use crate::task::{Task, TaskBehavior};

/// `Rational(INT_MIN)` — the initial "no track yet" maximum length
/// (`RATIONAL_MIN` in saveotio.cpp).
fn rational_min() -> Rational {
	Rational::new(-2147483647, 1)
}

/// An OTIO project-save task. Borrows its project.
pub struct SaveOTIOTask {
	/// The shared task base.
	pub base: Task,
	/// Borrowed project to save (domain project).
	pub project: ProjectRef,
	/// Output OTIO filename.
	pub filename: String,
}

impl SaveOTIOTask {
	/// Serialize one sequence into an `OTIO::Timeline`; `None` when the
	/// sequence has no usable frame rate or a track fails to serialize.
	///
	/// CPP-PARITY: saveotio.cpp (SaveOTIOTask::serialize_timeline)
	fn serialize_timeline(project: &ProjectRef, sequence: NodeId) -> Option<Timeline> {
		let mut otio_timeline =
			Timeline::new(nodeops::node_label(project, sequence));

		// Direct video-params value type (single-lib unification); absent
		// params leave rate at 0.0 → the documented `None` path.
		let (num, den) = nodeops::sequence_video_params(project, sequence, 0)
			.map(|vp| vp.frame_rate())
			.unwrap_or((0, 1));
		let mut rate = 0.0f64;
		if den != 0 {
			rate = num as f64 / den as f64;
		}
		if rate.is_nan() || rate <= 0.0 {
			return None;
		}

		let video_list = nodeops::sequence_track_list(project, sequence, TrackType::Video);
		let audio_list = nodeops::sequence_track_list(project, sequence, TrackType::Audio);

		if let Some(list) = video_list {
			if !Self::serialize_track_list(project, list, &mut otio_timeline, rate) {
				return None;
			}
		}
		if let Some(list) = audio_list {
			if !Self::serialize_track_list(project, list, &mut otio_timeline, rate) {
				return None;
			}
		}

		Some(otio_timeline)
	}

	/// Serialize all tracks of one track list (video or audio) into the
	/// timeline. Each track is padded to the list's maximum track length.
	///
	/// CPP-PARITY: saveotio.cpp (SaveOTIOTask::serialize_track_list)
	fn serialize_track_list(
		project: &ProjectRef,
		list: NodeId,
		otio_timeline: &mut Timeline,
		sequence_rate: f64,
	) -> bool {
		let mut max_track_length = rational_min();

		let track_count = nodeops::tracklist_track_count(project, list);

		for i in 0..track_count {
			if let Some(track) = nodeops::tracklist_track_at(project, list, i) {
				let length = nodeops::track_length(project, track);
				if length > max_track_length {
					max_track_length = length;
				}
			}
		}

		for i in 0..track_count {
			let Some(track) = nodeops::tracklist_track_at(project, list, i) else {
				continue;
			};

			let Some(otio_track) =
				Self::serialize_track(project, track, sequence_rate, max_track_length)
			else {
				return false;
			};

			otio_timeline
				.tracks_mut()
				.append_child(Composable::Track(otio_track));
		}

		true
	}

	/// Serialize one track: a `Track` whose kind matches the native track
	/// type, one OTIO block per native block, plus a trailing `Gap` when the
	/// track is shorter than `max_track_length`.
	///
	/// CPP-PARITY: saveotio.cpp (SaveOTIOTask::serialize_track)
	fn serialize_track(
		project: &ProjectRef,
		track: NodeId,
		sequence_rate: f64,
		max_track_length: Rational,
	) -> Option<Track> {
		let track_type = nodeops::track_type(project, track)?;

		let kind = match track_type {
			TrackType::Video => "Video",
			TrackType::Audio => "Audio",
			TrackType::Subtitle => {
				eprintln!(
					"Don't know OTIO track kind for native type {}",
					track_type.to_c()
				);
				return None;
			}
		};
		let mut otio_track = Track::new(kind);

		let block_count = nodeops::track_block_count(project, track);

		for i in 0..block_count {
			let Some(block) = nodeops::track_block_at(project, track, i) else {
				continue;
			};

			let kind = nodeops::block_kind(project, block);

			let otio_block: Option<Composable> = match kind {
				nodeops::BlockKind::Clip => {
					let mut otio_clip = Clip::new(nodeops::node_label(project, block));

					otio_clip.set_source_range(TimeRange::new(
						RationalTime::from_rational(block_in_of(project, block), sequence_rate),
						RationalTime::from_rational(block_length_of(project, block), sequence_rate),
					));

					let media = nodeops::node_find_input_footage(project, block);
					if let Some(media) = media {
						// The available range covers the WHOLE media: start
						// 0, duration = the media length in the reference's
						// own time base (the video rate for video clips, the
						// sample rate for audio clips — OTIO's External-
						// Reference convention). A zero duration declares
						// the media unusable and DaVinci Resolve rejects
						// every clip whose source range falls outside it.
						let duration_seconds = nodeops::node_length(project, media).to_f64();
						let available_range = if track_type == TrackType::Video {
							let source_frame_rate = nodeops::footage_video_params(project, media, 0)
								.map(|vp| {
									let (num, den) = vp.frame_rate();
									if den != 0 {
										num as f64 / den as f64
									} else {
										0.0
									}
								})
								.filter(|rate| *rate > 0.0)
								.unwrap_or(sequence_rate);
							TimeRange::new(
								RationalTime::new(0.0, source_frame_rate),
								RationalTime::new(
									(duration_seconds * source_frame_rate).round(),
									source_frame_rate,
								),
							)
						} else {
							let sample_rate = nodeops::footage_audio_sample_rate(project, media)
								.unwrap_or(48000) as f64;
							TimeRange::new(
								RationalTime::new(0.0, sample_rate),
								RationalTime::new(
									(duration_seconds * sample_rate).round(),
									sample_rate,
								),
							)
						};

						let media_url = nodeops::footage_filename(project, media);
						if !media_url.is_empty() {
							otio_clip.set_media_reference(MediaReference::ExternalReference(
								ExternalReference::new(media_url, Some(available_range)),
							));
						}
					}

					Some(Composable::Clip(otio_clip))
				}
				nodeops::BlockKind::Gap => Some(Composable::Gap(Gap::new(
					// Gaps live in timeline time: the SEQUENCE rate, for both
					// fields — a TimeRange mixing bases (a 24 fps start with
					// a 1.0 duration) is unreadable to strict importers.
					TimeRange::new(
						RationalTime::from_rational(block_in_of(project, block), sequence_rate),
						RationalTime::from_rational(block_length_of(project, block), sequence_rate),
					),
					nodeops::node_label(project, block),
				))),
				nodeops::BlockKind::Transition => {
					let mut otio_transition =
						Transition::new(nodeops::node_label(project, block));

					let n = transition_offset_of(project, block, true);
					otio_transition.set_in_offset(RationalTime::from_rational(n, 24.0));
					let n = transition_offset_of(project, block, false);
					otio_transition.set_out_offset(RationalTime::from_rational(n, 24.0));

					Some(Composable::Transition(otio_transition))
				}
				nodeops::BlockKind::Other => None,
			};

			let Some(otio_block) = otio_block else {
				// We shouldn't ever get here, but catch without crashing if
				// we ever do.
				return None;
			};

			otio_track.append_child(otio_block);
		}

		// All OTIO tracks must have the same duration so we add a Gap to
		// fill the remaining time. Like the gap blocks above, the range
		// stays in the sequence's time base (duration in whole frames).
		let duration = track_duration(&otio_track);
		let duration_seconds = duration.clone().to_seconds();
		if duration_seconds < max_track_length.to_f64() {
			let time_left = max_track_length.to_f64() - duration_seconds;

			let gap = Gap::new(
				TimeRange::new(
					RationalTime::new(0.0, sequence_rate),
					RationalTime::new((time_left * sequence_rate).round(), sequence_rate),
				),
				"",
			);
			otio_track.append_child(Composable::Gap(gap));
		}

		Some(otio_track)
	}
}

impl TaskBehavior for SaveOTIOTask {
	/// Serialize the project to OTIO/FCPXML via `oakotio` and write the file.
	fn run(&mut self, task: &mut Task) -> Result<()> {
		// Dispatch the interchange format from the filename extension.
		// Format handling ends at the write below — the serialization
		// produces `oak_otio::Timeline`s and is shared between `.otio` and
		// `.fcpxml` (C++ parity for the serialize_* helpers).
		let format = match InterchangeFormat::of(&self.filename) {
			Ok(f) => f,
			Err(e) => {
				if let Error::Failed(msg) = &e {
					task.set_error(msg);
				}
				return Err(e);
			}
		};

		// Collect sequences from the root folder (non-recursive, matching
		// the original list_children_of_type behavior closely enough for
		// OTIO).
		let root = {
			let guard = self.project.lock().unwrap_or_else(|e| e.into_inner());
			guard.root
		};
		if !root.valid() {
			task.set_error("Project contains no sequences to export.");
			return Err(Error::Failed(
				"Project contains no sequences to export.".to_string(),
			));
		}

		let mut sequences: Vec<NodeId> = Vec::new();
		{
			let guard = self.project.lock().unwrap_or_else(|e| e.into_inner());
			let Some(entry) = guard.graph.get(root) else {
				task.set_error("Project contains no sequences to export.");
				return Err(Error::Failed(
					"Project contains no sequences to export.".to_string(),
				));
			};
			let Some(folder) = entry
				.behavior
				.as_any()
				.and_then(|a| a.downcast_ref::<oak_node::folder::FolderBehavior>())
			else {
				task.set_error("Project contains no sequences to export.");
				return Err(Error::Failed(
					"Project contains no sequences to export.".to_string(),
				));
			};
			for child in &folder.children {
				if let Some(child_entry) = guard.graph.get(*child) {
					if child_entry.behavior.type_id() == nodeops::SEQUENCE_TYPE_ID {
						sequences.push(*child);
					}
				}
			}
		}

		if sequences.is_empty() {
			task.set_error("Project contains no sequences to export.");
			return Err(Error::Failed(
				"Project contains no sequences to export.".to_string(),
			));
		}

		let mut serialized: Vec<Timeline> = Vec::with_capacity(sequences.len());
		for sequence in &sequences {
			match Self::serialize_timeline(&self.project, *sequence) {
				Some(timeline) => serialized.push(timeline),
				None => {
					task.set_error(&format!(
						"Failed to serialize sequence \"{}\"",
						nodeops::node_label(&self.project, *sequence)
					));
					return Err(Error::Failed("Failed to serialize sequence".to_string()));
				}
			}
		}

		// Drop timelines with no clips at all (an empty sequence the user
		// never touched): DaVinci Resolve's OTIO import expects a single
		// Timeline root and chokes on a SerializableCollection — skipping
		// the empty ones lets a lone edited sequence export as a bare
		// Timeline. FCPXML benefits the same way (no empty <project>s).
		serialized.retain(|timeline| {
			timeline.tracks().children().iter().any(|child| {
				child
					.as_track()
					.is_some_and(|track| track.children().iter().any(|b| b.as_clip().is_some()))
			})
		});
		if serialized.is_empty() {
			task.set_error("Project contains no edited sequences to export.");
			return Err(Error::Failed(
				"Project contains no edited sequences to export.".to_string(),
			));
		}

		// Write the serialized timelines, dispatching on the format: OTIO
		// JSON writes a `Timeline` root for one sequence or a
		// `SerializableCollection` for several; FCPXML writes all timelines
		// into one document (one `<project>` per timeline).
		let result: std::result::Result<(), String> = match format {
			InterchangeFormat::OtioJson => {
				if serialized.len() == 1 {
					serialized[0]
						.to_json_file(&self.filename)
						.map_err(|e| e.to_string())
				} else {
					// Serialize all into a SerializableCollection.
					let collection = SerializableCollection::new(
						"Sequences",
						serialized.into_iter().map(Serializable::Timeline).collect(),
					);
					Serializable::SerializableCollection(collection)
						.to_json_file(&self.filename)
						.map_err(|e| e.to_string())
				}
			}
			InterchangeFormat::Fcpxml => {
				oak_otio::to_fcpxml_file(&serialized, &self.filename).map_err(|e| e.to_string())
			}
		};

		result.map_err(|e| {
			let (label, what) = match format {
				InterchangeFormat::OtioJson => (
					"Failed to save OpenTimelineIO to file",
					"Failed to save OpenTimelineIO",
				),
				InterchangeFormat::Fcpxml => {
					("Failed to save FCPXML to file", "Failed to save FCPXML")
				}
			};
			task.set_error(&format!("{label} \"{}\": {e}", self.filename));
			Error::Failed(what.to_string())
		})
	}
}

/// The block's in point as a `Rational` (default 0/1 when unset).
fn block_in_of(project: &ProjectRef, block: NodeId) -> Rational {
	nodeops::block_in(project, block)
}

/// The block's length as a `Rational` (default 0/1 when unset).
fn block_length_of(project: &ProjectRef, block: NodeId) -> Rational {
	nodeops::block_length(project, block)
}

/// A transition's in (`in_offset == true`) or out offset as a `Rational`.
fn transition_offset_of(project: &ProjectRef, block: NodeId, in_offset: bool) -> Rational {
	if in_offset {
		nodeops::transition_in_offset(project, block)
	} else {
		nodeops::transition_out_offset(project, block)
	}
}

/// The serialized track's duration as an `OTIO::RationalTime` — the sum of
/// its children's source-range durations, keeping the first child's rate
/// (the C++ `otio_track->duration(&es)` result for contiguous children).
fn track_duration(track: &Track) -> RationalTime {
	let mut acc: Option<RationalTime> = None;
	for child in track.children() {
		let duration = match child
			.as_clip()
			.and_then(|c| c.source_range())
			.map(|r| r.duration())
			.or_else(|| {
				child
					.as_gap()
					.and_then(|g| g.source_range())
					.map(|r| r.duration())
			}) {
			Some(d) => d,
			// Transitions occupy no time.
			None => RationalTime::new(0.0, 1.0),
		};
		match acc {
			None => acc = Some(duration),
			Some(a) => {
				let rate = a.rate();
				let value = a.value() + duration.rescaled_to(rate).value();
				acc = Some(RationalTime::new(value, rate));
			}
		}
	}
	acc.unwrap_or_else(RationalTime::invalid_time)
}
