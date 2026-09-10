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

//! Interchange-export correctness (the "DaVinci Resolve imports nothing"
//! report): the OTIO JSON must declare a usable `available_range` for
//! every clip (a zero duration rejects the whole file), gap ranges must
//! stay in the sequence's time base, an edited lone sequence exports as
//! a bare `Timeline` root (empty sequences are skipped), and the FCPXML
//! assets of a file used on both video and audio tracks declare both
//! stream flags.

use std::sync::{Arc, Mutex};

use oak_core::{Rational, TimeRange};
use oak_node::block::ClipBlockBehavior;
use oak_node::footage::FootageBehavior;
use oak_node::id::NodeId;
use oak_node::node::NodeCore;
use oak_node::project::Project;
use oak_node::sequence::SequenceBehavior;
use oak_node::track::{TrackBehavior, TrackListBehavior, TrackType};
use oak_otio::{MediaReference, Serializable};

/// The sequence's frame rate (the `set_default_parameters` NTSC
/// default) every timeline-range must share.
fn sequence_rate() -> f64 {
	30000.0 / 1001.0
}

fn clip_path(tag: &str, ext: &str) -> std::path::PathBuf {
	std::env::temp_dir().join(format!("oaktask_otio_{tag}_{}.{ext}", std::process::id()))
}

/// Add a track of `kind` carrying one clip of `footage` covering
/// `[in, out)` (media seconds) to `list` — the graph plumbing shared by
/// the two sequences built below.
fn add_track_with_clip(
	p: &mut Project,
	footage: NodeId,
	kind: TrackType,
	in_: Rational,
	out: Rational,
) -> NodeId {
	let (tcore, tbehavior) = TrackBehavior::create();
	let track = p.graph.add_node(tcore, tbehavior);
	p.graph
		.get_mut(track)
		.unwrap()
		.behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<TrackBehavior>()
		.unwrap()
		.kind = kind;

	let (ccore, cbehavior) = oak_node::block::clip_create();
	let clip = p.graph.add_node(ccore, cbehavior);
	p.graph
		.connect(footage, clip, oak_node::block::clip_input::TEXTURE_INPUT, -1)
		.expect("connect footage to clip");
	p.graph
		.get_mut(clip)
		.unwrap()
		.behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<ClipBlockBehavior>()
		.expect("clip block")
		.core
		.range = TimeRange::new(in_, out);
	p.graph
		.get_mut(track)
		.unwrap()
		.behavior
		.as_any_mut()
		.unwrap()
		.downcast_mut::<TrackBehavior>()
		.unwrap()
		.append_block(clip);
	track
}

/// A project with two sequences: "Edited" (a 1s video clip on one video
/// track, a 0.5s video clip on a second — leaving a trailing gap — and
/// the same media on an audio track) and "Empty" (no tracks). Both sit
/// in the root folder, like the app's project bin.
fn build_project(media: &str) -> Arc<Mutex<Project>> {
	let project = Project::new();
	{
		let mut p = project.lock().unwrap();
		p.initialize().expect("root folder");

		let footage = {
			let mut f = FootageBehavior::new(media);
			f.probe().expect("probe the generated clip");
			p.graph.add_node(NodeCore::new(), Box::new(f))
		};

		// "Edited": video list (two tracks) + audio list (one track).
		let (score, sbehavior) = SequenceBehavior::create();
		let edited = p.graph.add_node(score, sbehavior);
		{
			let video_track_a = add_track_with_clip(
				&mut p,
				footage,
				TrackType::Video,
				Rational::new(0, 1),
				Rational::new(1, 1),
			);
			let video_track_b = add_track_with_clip(
				&mut p,
				footage,
				TrackType::Video,
				Rational::new(0, 1),
				Rational::new(1, 2),
			);
			let audio_track = add_track_with_clip(
				&mut p,
				footage,
				TrackType::Audio,
				Rational::new(0, 1),
				Rational::new(1, 1),
			);
			for (kind, tracks) in [
				(TrackType::Video, vec![video_track_a, video_track_b]),
				(TrackType::Audio, vec![audio_track]),
			] {
				let (lcore, lbehavior) = TrackListBehavior::create();
				let list = p.graph.add_node(lcore, lbehavior);
				{
					let list_b = p
						.graph
						.get_mut(list)
						.unwrap()
						.behavior
						.as_any_mut()
						.unwrap()
						.downcast_mut::<TrackListBehavior>()
						.unwrap();
					list_b.kind = kind;
					list_b.tracks = tracks;
				}
				p.graph
					.get_mut(edited)
					.unwrap()
					.behavior
					.as_any_mut()
					.unwrap()
					.downcast_mut::<SequenceBehavior>()
					.unwrap()
					.track_lists
					.push(list);
			}
			p.graph
				.get_mut(edited)
				.unwrap()
				.core
				.label = "Edited".to_string();
		}

		// "Empty": a sequence with no track lists at all.
		let (ecore, ebehavior) = SequenceBehavior::create();
		let empty = p.graph.add_node(ecore, ebehavior);
		p.graph.get_mut(empty).unwrap().core.label = "Empty".to_string();

		// Both live in the root folder (the save task enumerates it).
		let root = p.root;
		p.graph
			.get_mut(root)
			.unwrap()
			.behavior
			.as_any_mut()
			.unwrap()
			.downcast_mut::<oak_node::folder::FolderBehavior>()
			.expect("root folder")
			.children
			.extend([edited, empty]);
	}
	project
}

/// Run the save task for `project` to `filename`.
fn save(project: &Arc<Mutex<Project>>, filename: String) {
	let mut driver = oak_task::task::Task::new("Saving project...", None);
	driver.set_behavior(Box::new(oak_task::project::saveotio::SaveOTIOTask {
		base: oak_task::task::Task::new("Saving project...", None),
		project: project.clone(),
		filename: filename.clone(),
	}));
	if let Err(e) = driver.start() {
		panic!(
			"save to {filename} failed: {e:?} / {}",
			driver.error().unwrap_or("unknown error")
		);
	}
}

/// OTIO JSON: a lone edited sequence exports as a bare `Timeline` root;
/// every clip declares a usable available range; every gap range is in
/// the sequence's time base.
#[test]
fn otio_export_is_importable() {
	let media = clip_path("src_otio", "mp4");
	oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("clip generation");
	let project = build_project(&media.to_string_lossy());
	let out = clip_path("out", "otio");
	let _ = std::fs::remove_file(&out);
	save(&project, out.to_string_lossy().into_owned());

	let doc = oak_otio::from_json_file(&out).expect("the export parses as OTIO");
	let Serializable::Timeline(timeline) = doc else {
		panic!("one edited sequence must export as a bare Timeline root, not a collection");
	};
	assert_eq!(timeline.name(), "Edited");

	let mut clips = 0;
	let mut gaps = 0;
	for child in timeline.tracks().children() {
		let track = child.as_track().expect("track");
		for block in track.children() {
			if let Some(clip) = block.as_clip() {
				clips += 1;
				let range = clip
					.media_reference()
					.and_then(MediaReference::as_external_reference)
					.expect("external reference")
					.available_range()
					.expect("available_range");
				assert!(
					range.duration().value() > 0.0,
					"a zero available range rejects the file everywhere"
				);
				assert!(range.duration().rate() > 0.0);
				if track.kind() == "Audio" {
					assert_eq!(
						range.duration().rate(),
						48000.0,
						"an audio clip's available range is in the media's sample rate"
					);
				}
			}
			if let Some(gap) = block.as_gap() {
				gaps += 1;
				let range = gap.source_range().expect("gap range");
				assert!(
					(range.duration().rate() - sequence_rate()).abs() < 1e-6,
					"gap duration rate {} is not the sequence rate",
					range.duration().rate()
				);
				assert!(
					(range.start_time().rate() - sequence_rate()).abs() < 1e-6,
					"gap start rate {} is not the sequence rate",
					range.start_time().rate()
				);
			}
		}
	}
	assert_eq!(clips, 3, "two video clips + one audio clip");
	assert!(gaps >= 1, "the half-length video track gains a trailing gap");

	let _ = std::fs::remove_file(&media);
	let _ = std::fs::remove_file(&out);
}

/// FCPXML: the shared media's single asset declares hasVideo AND
/// hasAudio, and no asset carries a zero duration.
#[test]
fn fcpxml_export_declares_both_streams_and_real_durations() {
	let media = clip_path("src_fcpxml", "mp4");
	oak_codec::testmedia::write_test_clip(&media, 64, 64, 10, 10).expect("clip generation");
	let project = build_project(&media.to_string_lossy());
	let out = clip_path("out", "fcpxml");
	let _ = std::fs::remove_file(&out);
	save(&project, out.to_string_lossy().into_owned());

	let xml = std::fs::read_to_string(&out).expect("read the fcpxml export");
	assert!(
		xml.contains("hasVideo=\"1\""),
		"the shared media's asset declares video: {xml}"
	);
	assert!(
		xml.contains("hasAudio=\"1\""),
		"the shared media's asset declares audio: {xml}"
	);
	assert!(
		!xml.contains("duration=\"0/1s\""),
		"no asset carries a zero duration: {xml}"
	);

	let _ = std::fs::remove_file(&media);
	let _ = std::fs::remove_file(&out);
}
