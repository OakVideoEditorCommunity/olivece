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

//! `ProjectImportTask`, mirroring `src/task/src/project/import/import.h`.
//!
//! Imports media files into a folder of a project. Produces an undoable
//! [`oak_undo::undocommand::UndoCommand`] (taken via `take_command()`),
//! tracks per-file import failures, and supports an optional image-sequence
//! confirmation callback.
//!
//! **Single-lib note**: the node-graph manipulation went through the
//! deleted oaknode C ABI; it now goes through the direct oaknode domain
//! operations in [`crate::nodeops`] (folder/footage creation, probing via
//! the oakcodec decoder registry, undo command construction). The folder
//! and project are domain references (`Arc<Mutex<Project>>` + `NodeId`)
//! instead of borrowed `CHandle`s.
//!
//! CPP-PARITY: src/task/src/project/import/import.h

use std::path::Path;

use oak_core::configstore::ConfigStore;
use oak_core::videoparams::VideoType;
use oak_undo::undocommand::UndoCommand;

use crate::error::{Error, Result};
use crate::nodeops::{self, NodeRef, ProjectRef};
use crate::task::{Task, TaskBehavior};

/// Callback used to confirm whether a detected image sequence should be
/// imported as a sequence rather than individual frames, mirroring
/// `ImageSequenceConfirmFn` in import.h.
pub type ImageSequenceConfirmFn = Box<dyn FnMut(&str, &str) -> bool + Send>;

/// A project-import task. Borrows the destination folder and project.
pub struct ProjectImportTask {
	/// The shared task base.
	pub base: Task,
	/// Destination folder (project + folder node id).
	pub folder: NodeRef,
	/// Destination project.
	pub project: ProjectRef,
	/// Media filenames to import.
	pub filenames: Vec<String>,
	/// The undo command produced by the task, taken via `take_command`.
	command: Option<UndoCommand>,
	/// Files that failed to import.
	invalid_files: Vec<String>,
	/// Imported footage, taken via `get_imported_footage`.
	imported_footage: Vec<NodeRef>,
	/// Optional image-sequence confirmation callback.
	image_sequence_confirm: Option<ImageSequenceConfirmFn>,
	/// Total number of files to import (directories counted recursively).
	file_count: usize,
	/// Image-sequence files already decided not to be imported as sequences.
	image_sequence_ignore_files: Vec<String>,
}

impl ProjectImportTask {
	/// Create an import task for the given folder/project and filenames.
	/// The old `folder: CHandle` / `project: CHandle` signature is replaced
	/// by the domain [`NodeRef`] folder and [`ProjectRef`] project
	/// (single-lib unification).
	pub fn new(
		base: Task,
		folder: NodeRef,
		project: ProjectRef,
		filenames: Vec<String>,
		image_sequence_confirm: Option<ImageSequenceConfirmFn>,
		file_count: usize,
	) -> ProjectImportTask {
		ProjectImportTask {
			base,
			folder,
			project,
			filenames,
			command: None,
			invalid_files: Vec::new(),
			imported_footage: Vec::new(),
			image_sequence_confirm,
			file_count,
			image_sequence_ignore_files: Vec::new(),
		}
	}

	/// Take ownership of the produced undo command; `Err(Error::State)` if
	/// the task has not run yet.
	pub fn take_command(&mut self) -> Result<UndoCommand> {
		self.command.take().ok_or(Error::State)
	}

	/// Number of files that failed to import.
	pub fn get_invalid_file_count(&self) -> usize {
		self.invalid_files.len()
	}

	/// Whether any file failed to import.
	pub fn has_invalid_files(&self) -> bool {
		!self.invalid_files.is_empty()
	}

	/// The imported footage at `index` as a domain [`NodeRef`];
	/// `Err(Error::NotFound)` if out of range. (The deleted C ABI handed
	/// out addref'd handles; the domain reference is a plain clone.)
	pub fn get_imported_footage(&self, index: usize) -> Result<NodeRef> {
		self.imported_footage.get(index).cloned().ok_or(Error::NotFound)
	}

	/// Total number of imported footage entries.
	pub fn get_file_count(&self) -> usize {
		self.imported_footage.len()
	}

	/// Set the image-sequence confirmation callback.
	pub fn set_image_sequence_confirm_callback(&mut self, cb: ImageSequenceConfirmFn) {
		self.image_sequence_confirm = Some(cb);
	}

	/// The number of files counted at construction (progress denominator).
	pub fn file_count(&self) -> usize {
		self.file_count
	}

	fn import(
		&mut self,
		task: &mut Task,
		folder: NodeRef,
		entries: &mut Vec<String>,
		counter: &mut usize,
		parent_command: &mut UndoCommand,
	) {
		let atom = task.get_cancel_atom();
		let mut i = 0;
		while i < entries.len() {
			if task.is_cancelled() {
				break;
			}

			let file_path = &entries[i];

			if Path::new(file_path).is_dir() {
				// Create a folder corresponding to the directory and recurse.
				let entry_list: Vec<String> = match std::fs::read_dir(file_path) {
					Ok(rd) => rd
						.filter_map(|e| e.ok())
						.map(|e| e.path().to_string_lossy().into_owned())
						.collect(),
					Err(_) => Vec::new(),
				};

				if !entry_list.is_empty() {
					if let Some(folder_id) = nodeops::folder_create(&self.project) {
						nodeops::set_node_label(&self.project, folder_id, &basename_of(file_path));
						self.add_item_to_folder(
							folder.clone(),
							(self.project.clone(), folder_id),
							parent_command,
						);
						let mut sub_entries = entry_list;
						self.import(
							task,
							(self.project.clone(), folder_id),
							&mut sub_entries,
							counter,
							parent_command,
						);
					}
				}
			} else {
				let Some(footage) = nodeops::footage_create(&self.project, None) else {
					i += 1;
					continue;
				};

				// Mirror the C++ cancel-atom dance around the probe: the
				// footage behavior's own cancellation flag tracks the
				// task's atom during the probe and is cleared afterwards.
				nodeops::footage_set_cancelled(&self.project, footage, true);
				nodeops::footage_set_cancelled(&self.project, footage, atom.is_cancelled());
				let ok = nodeops::footage_set_filename(&self.project, footage, file_path);
				nodeops::footage_set_cancelled(&self.project, footage, false);

				if ok && nodeops::footage_is_valid(&self.project, footage) {
					nodeops::set_node_label(&self.project, footage, &basename_of(file_path));

					// See if this footage is an image sequence.
					self.validate_image_sequence(
						task,
						(self.project.clone(), footage),
						entries,
						i,
					);

					// Create the undoable command that adds the item.
					self.add_item_to_folder(
						folder.clone(),
						(self.project.clone(), footage),
						parent_command,
					);

					self.imported_footage.push((self.project.clone(), footage));
				} else {
					self.invalid_files.push(file_path.clone());

					// Remove the invalid footage from the graph; the remove
					// command takes ownership on redo and deletes the node
					// when the command is destroyed.
					let mut remove =
						nodeops::remove_node_command(self.project.clone(), footage);
					remove.redo_now();
				}

				*counter += 1;
				task.emit_progress(*counter as f64 / self.file_count.max(1) as f64);
			}
			i += 1;
		}
	}

	fn add_item_to_folder(&self, folder: NodeRef, item: NodeRef, command: &mut UndoCommand) {
		let child = nodeops::folder_add_child_command(folder, item);
		command.multi_add_child(child);
	}

	fn validate_image_sequence(
		&mut self,
		task: &mut Task,
		footage: NodeRef,
		info_list: &mut Vec<String>,
		index: usize,
	) {
		let filename = nodeops::footage_filename(&footage.0, footage.1);
		if filename.is_empty() {
			return;
		}

		// Direct oakcodec calls (single-lib unification).
		let digit_count = oak_codec::decoder::get_image_sequence_digit_count(&filename);
		if digit_count <= 0 {
			return;
		}

		if self
			.image_sequence_ignore_files
			.iter()
			.any(|f| f == &filename)
		{
			return;
		}

		if !self.item_is_still_image_footage_only(&footage) {
			return;
		}

		let Some(mut video_stream) = nodeops::footage_video_params(&footage.0, footage.1, 0)
		else {
			return;
		};

		let width = video_stream.width();
		let height = video_stream.height();

		// Direct oakcodec call (single-lib unification).
		let seq_index = oak_codec::decoder::get_image_sequence_index(&filename);

		let prev_fn = transform_sequence_filename(&filename, seq_index - 1, digit_count);
		let next_fn = transform_sequence_filename(&filename, seq_index + 1, digit_count);

		let previous_file = nodeops::footage_create(&self.project, Some(&prev_fn));
		let next_file = nodeops::footage_create(&self.project, Some(&next_fn));

		let prev_matches = previous_file.is_some_and(|f| {
			nodeops::footage_set_filename(&self.project, f, &prev_fn)
				&& self.compare_still_image_size(&(self.project.clone(), f), width, height)
		});
		let next_matches = next_file.is_some_and(|f| {
			nodeops::footage_set_filename(&self.project, f, &next_fn)
				&& self.compare_still_image_size(&(self.project.clone(), f), width, height)
		});

		if prev_matches || next_matches {
			// Ask the user whether this is really a sequence (default: no).
			let is_sequence = match self.image_sequence_confirm.as_mut() {
				Some(cb) => cb(&filename, &filename),
				None => false,
			};

			let start_index = get_image_sequence_limit(&filename, seq_index, false, digit_count);
			let end_index = get_image_sequence_limit(&filename, seq_index, true, digit_count);

			for j in start_index..=end_index {
				let entry_fn = transform_sequence_filename(&filename, j, digit_count);

				if is_sequence {
					// Remove later entries of this sequence from the import
					// list (they are imported as one footage). The C++
					// `info_list.size()` is re-evaluated after each erase.
					let mut k = index + 1;
					while k < info_list.len() {
						if info_list[k] == entry_fn {
							info_list.remove(k);
							break;
						}
						k += 1;
					}
				} else {
					self.image_sequence_ignore_files.push(entry_fn);
				}
			}

			if is_sequence {
				video_stream.set_video_type(VideoType::ImageSequence);

				// Direct config read (single-lib unification).
				if let Ok(rate) = ConfigStore::instance().get(None, "DefaultSequenceFrameRate") {
					if let Some((num, den)) = parse_rational(&rate) {
						if den != 0 {
							video_stream.set_time_base(num, den);
							video_stream.set_frame_rate(den, num);
						}
					}
				}

				video_stream.set_start_time(start_index);
				video_stream.set_duration(end_index - start_index + 1);
				nodeops::footage_set_video_params(&footage.0, footage.1, 0, &video_stream);
			}
		}

		// The probe footage was only created for comparison; remove it.
		for probe in [previous_file, next_file].into_iter().flatten() {
			let mut remove = nodeops::remove_node_command(self.project.clone(), probe);
			remove.redo_now();
		}

		let _ = task;
	}

	fn item_is_still_image_footage_only(&self, footage: &NodeRef) -> bool {
		// The oaknode domain footage records probed streams; a single
		// stream is the closest domain equivalent of the C++
		// `total_stream_count == 1` check. The oaknode video params carry
		// no `video_type`, so the `kVideoTypeStill` check collapses to
		// "one stream with valid dimensions".
		if nodeops::footage_total_stream_count(&footage.0, footage.1) != 1 {
			return false;
		}

		let Some(vp) = nodeops::footage_video_params(&footage.0, footage.1, 0) else {
			return false;
		};

		vp.is_valid() && vp.video_type() == VideoType::Video
	}

	fn compare_still_image_size(&self, footage: &NodeRef, width: i32, height: i32) -> bool {
		if !self.item_is_still_image_footage_only(footage) {
			return false;
		}

		let Some(stream) = nodeops::footage_video_params(&footage.0, footage.1, 0) else {
			return false;
		};

		stream.width() == width && stream.height() == height
	}
}

impl TaskBehavior for ProjectImportTask {
	/// Import each filename via the direct oaknode domain operations
	/// ([`crate::nodeops`]), collecting an undo command, imported footage,
	/// and per-file failures.
	fn run(&mut self, task: &mut Task) -> Result<()> {
		let mut command = UndoCommand::multi();

		let mut counter = 0;
		let mut entries = self.filenames.clone();
		let folder = (self.folder.0.clone(), self.folder.1);
		self.import(task, folder, &mut entries, &mut counter, &mut command);

		if task.is_cancelled() {
			self.command = None;
			return Err(Error::Cancelled);
		}
		self.command = Some(command);
		Ok(())
	}
}

/// Count files recursively (directories recurse; anything else counts 1),
/// mirroring `count_files_recursive` in import.cpp.
fn basename_of(path: &str) -> String {
	Path::new(path)
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default()
}

/// Substitute `number` into the trailing-digit field of an image-sequence
/// filename (mirrors `oakcodec_decoder_transform_image_sequence_file_name`).
fn transform_sequence_filename(filename: &str, number: i64, digit_count: i32) -> String {
	let width = digit_count.max(0) as usize;
	match filename.rfind('.') {
		Some(dot) => {
			// The digits are the `width` chars immediately before the
			// extension; they are replaced by `number`.
			let digits_start = dot.saturating_sub(width);
			let prefix = &filename[..digits_start];
			format!(
				"{prefix}{:0width$}{}",
				number,
				&filename[dot..],
				width = width
			)
		}
		None => format!("{filename}{:0width$}", number, width = width),
	}
}

/// Walk the index from `start` in the given direction while the transformed
/// filename exists (mirrors `get_image_sequence_limit` in import.cpp).
fn get_image_sequence_limit(start_fn: &str, start: i64, up: bool, digit_count: i32) -> i64 {
	let mut current = start;
	loop {
		let test_index = if up { current + 1 } else { current - 1 };
		let test_filename = transform_sequence_filename(start_fn, test_index, digit_count);
		if !Path::new(&test_filename).exists() {
			break;
		}
		current = test_index;
	}
	current
}

/// Parse a "num/den" frame-rate string.
fn parse_rational(s: &str) -> Option<(i32, i32)> {
	let mut parts = s.trim().split('/');
	let num: i32 = parts.next()?.trim().parse().ok()?;
	let den: i32 = parts.next()?.trim().parse().ok()?;
	Some((num, den))
}
