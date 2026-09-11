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

//! Project: owns the graph, the folder tree, settings, and the undo
//! stack binding. Mirrors C++ `olive::Project`.

use std::sync::{Arc, Mutex, Weak};

use crate::graph::Graph;
use crate::id::NodeId;

/// A reference to a node inside a project's graph — the value boxed by
/// every public node/folder/sequence handle (`id.rs`: "a handle boxes
/// `(Arc<Mutex<Project>>, NodeId)`"). Node lifetime follows the project;
/// a stale `NodeId` (slot reused, node removed) fails validation instead
/// of aliasing.
#[derive(Clone)]
pub struct NodeRef {
	/// The owning project (or a scratch project for orphaned nodes).
	pub project: Arc<Mutex<Project>>,
	/// The node's id in that project's graph.
	pub id: NodeId,
	/// Shared owned-flag for the ffi alive counter: true while the node
	/// object is separately accounted (factory-created / detached nodes);
	/// flipped to false when the node is adopted by a project graph
	/// (shared across every handle copy — mirrors the C++
	/// `OakNodeBox::owns` flip in `mark_container_owned`).
	pub owned: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl NodeRef {
	/// New reference. `owned` selects whether releasing the last handle
	/// reference accounts the node in the crate's debug alive count.
	pub fn new(project: Arc<Mutex<Project>>, id: NodeId, owned: bool) -> NodeRef {
		NodeRef {
			project,
			id,
			owned: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(owned)),
		}
	}
}

/// The project. Shared ownership via `Arc<Mutex<Project>>` is what the
/// public C ABI handles box.
pub struct Project {
	/// The node graph (all nodes incl. sequences).
	pub graph: Graph,
	/// Root folder of the bin tree ([`NodeId::INVALID`] until
	/// [`Project::initialize`]).
	pub root: NodeId,
	/// Arbitrary project settings (C++ `setting_` map).
	pub settings: std::collections::HashMap<String, String>,
	/// Project file path (empty = unsaved).
	pub filename: String,
	/// Dirty flag.
	pub modified: bool,
	/// Session UUID (cache filename base).
	pub uuid: String,
	/// Cache location setting (0 = default, 1 = alongside project,
	/// 2 = custom path; C++ `CacheSetting`).
	pub cache_location_setting: i32,
	/// Custom cache path (C++ `custom_cache_path_`).
	pub custom_cache_path: String,
}

/// Setting key for the root folder identity (C++
/// `Project::k_root_key`).
pub const SETTING_ROOT: &str = "root";
/// Setting key for the cache location (C++ `k_cache_location_setting_key`).
pub const SETTING_CACHE_LOCATION: &str = "cachesetting";
/// Setting key for the custom cache path (C++ `k_cache_path_key`).
pub const SETTING_CACHE_PATH: &str = "customcachepath";
/// Setting key: the pipeline working colorspace ("acescg" | "srgb_legacy").
/// Absent = the default (ACEScg).
pub const SETTING_WORKING_COLOR_SPACE: &str = "workingcolorspace";
/// Setting key: the output/delivery gamut ("srgb" | "displayp3" | "bt2020").
/// Absent = sRGB.
pub const SETTING_OUTPUT_GAMUT: &str = "outputgamut";
/// Setting key: the output/delivery transfer ("srgb" | "gamma22" | "pq" |
/// "hlg"). Absent = sRGB.
pub const SETTING_OUTPUT_TRANSFER: &str = "outputtransfer";

impl Project {
	/// New empty project (no root folder until [`Project::initialize`]).
	pub fn new() -> Arc<Mutex<Project>> {
		Arc::new(Mutex::new(Project {
			graph: Graph::new(),
			root: NodeId::INVALID,
			settings: std::collections::HashMap::new(),
			filename: String::new(),
			modified: false,
			uuid: generate_uuid(),
			cache_location_setting: 0,
			custom_cache_path: String::new(),
		}))
	}

	/// Create the root folder and default state (C++ `initialize()`).
	pub fn initialize(&mut self) -> crate::error::Result<()> {
		use crate::error::Error;
		if self.root.valid() {
			return Err(Error::State);
		}
		let (core, behavior) = crate::folder::create("Root");
		let id = self.graph.add_node(core, behavior);
		self.root = id;
		self.settings
			.insert(SETTING_ROOT.to_string(), id.identity().to_string());
		// Every project graph carries the virtual endpoint pair (see
		// docs/zh/plans/render-pipeline-threads.md §3.8); created last so
		// the root folder keeps the first arena slot.
		self.graph.ensure_endpoints();
		Ok(())
	}

	/// Remove all nodes and reset to a blank project (C++ `clear()`).
	pub fn clear(&mut self) -> crate::error::Result<()> {
		// Detach every node from its folder membership, then drop the
		// arena (the `// CPP-PARITY: project.cpp:90` clear() loop).
		let ids = self.graph.node_ids();
		for id in ids {
			if let Some(entry) = self.graph.get_mut(id) {
				entry.core.bin_folder = None;
				entry.core.links.clear();
			}
		}
		self.graph = Graph::new();
		self.root = NodeId::INVALID;
		Ok(())
	}

	/// The pipeline working colorspace (project property; ACEScg when the
	/// setting is absent).
	pub fn working_color_space(&self) -> oak_core::colormath::WorkingColorSpace {
		oak_core::colormath::WorkingColorSpace::from_setting(
			self.settings.get(SETTING_WORKING_COLOR_SPACE).map(String::as_str).unwrap_or(""),
		)
	}

	/// The output/delivery colorspace (project property; sRGB when the
	/// settings are absent).
	pub fn output_color_spec(&self) -> oak_core::colormath::OutputColorSpec {
		oak_core::colormath::OutputColorSpec::from_settings(
			self.settings.get(SETTING_OUTPUT_GAMUT).map(String::as_str).unwrap_or(""),
			self.settings.get(SETTING_OUTPUT_TRANSFER).map(String::as_str).unwrap_or(""),
		)
	}

	/// Set the pipeline working colorspace property.
	pub fn set_working_color_space(&mut self, space: oak_core::colormath::WorkingColorSpace) {
		self.settings
			.insert(SETTING_WORKING_COLOR_SPACE.to_string(), space.as_setting().to_string());
	}

	/// Set the output/delivery colorspace properties.
	pub fn set_output_color_spec(&mut self, spec: oak_core::colormath::OutputColorSpec) {
		self.settings
			.insert(SETTING_OUTPUT_GAMUT.to_string(), spec.gamut.as_setting().to_string());
		self.settings
			.insert(SETTING_OUTPUT_TRANSFER.to_string(), spec.transfer.as_setting().to_string());
	}

	/// Deep-copy the whole project for background render isolation
	/// (replaces oakrender's C++ ProjectCopier: the copy happens here,
	/// inside the module that owns the data — see M-series note on
	/// render→node decoupling).
	pub fn deep_copy(&self) -> crate::error::Result<Arc<Mutex<Project>>> {
		use crate::error::Error;
		let copy = Project::new();
		let mut guard = copy.lock().map_err(|_| Error::State)?;

		// Map original id -> copied id, preserving identities where the
		// copy's arena slots are free (the copy starts empty, so every
		// node keeps its identity).
		let mut id_map: std::collections::HashMap<NodeId, NodeId> =
			std::collections::HashMap::new();
		for id in self.graph.node_ids() {
			let entry = self.graph.get(id).ok_or(Error::NotFound)?;
			let (core, behavior) = clone_entry(entry);
			let new_id = guard.graph.add_node(core, behavior);
			id_map.insert(id, new_id);
		}

		// Copy edges (source ids remapped).
		let edges: Vec<_> = self
			.graph
			.output_connections_all()
			.into_iter()
			.map(|(from, to, input, element)| {
				(
					*id_map.get(&from).unwrap_or(&from),
					*id_map.get(&to).unwrap_or(&to),
					input,
					element,
				)
			})
			.collect();
		for (from, to, input, element) in edges {
			guard.graph.connect(from, to, &input, element).ok();
		}

		// Project state.
		guard.root = id_map.get(&self.root).copied().unwrap_or(NodeId::INVALID);
		guard.settings = self.settings.clone();
		guard.filename = self.filename.clone();
		guard.modified = self.modified;
		guard.uuid = self.uuid.clone();
		guard.cache_location_setting = self.cache_location_setting;
		guard.custom_cache_path = self.custom_cache_path.clone();
		drop(guard);
		Ok(copy)
	}

	/// Incremental sync of a deep copy after edits (C++
	/// ProjectCopier::queue_update semantics): applies the recorded
	/// change set to `copy`.
	pub fn sync_copy(
		&self,
		copy: &mut Project,
		changes: &[ChangeRecord],
	) -> crate::error::Result<()> {
		use crate::error::Error;
		// Rebuild the id mapping from the copy (identities are stable
		// across the deep-copy, so original id -> copy id is identity).
		let _ = Error::NotFound;
		for change in changes {
			match change {
				ChangeRecord::NodeAdded(id) => {
					let entry = self.graph.get(*id).ok_or(Error::NotFound)?;
					let (core, behavior) = clone_entry(entry);
					copy.graph.add_entry(
						crate::graph::NodeEntry {
							core,
							behavior,
							generation: id.generation(),
							vacant: false,
						},
						*id,
					);
				}
				ChangeRecord::NodeRemoved(_id) => {
					// Ids are stable, so the copy-side id equals the
					// original; drop it from the copy.
					copy.graph.remove_node(*_id);
				}
				ChangeRecord::EdgeChanged {
					from,
					to,
					input,
					element,
					connected,
				} => {
					if *connected {
						copy.graph.connect(*from, *to, input, *element)?;
					} else {
						copy.graph.disconnect(*from, *to, input, *element);
					}
				}
				ChangeRecord::ValueChanged {
					node,
					input,
					element,
				} => {
					if let (Some(src), Some(dst)) =
						(self.graph.get(*node), copy.graph.get_mut(*node))
					{
						let v = src.core.standard_value(input, *element);
						dst.core.set_standard_value(input, *element, v);
					}
				}
			}
		}
		Ok(())
	}

	/// Display name (C++ `Project::name()`): filename base or
	/// "(untitled)".
	pub fn name(&self) -> String {
		if self.filename.is_empty() {
			return "(untitled)".to_string();
		}
		let base = std::path::Path::new(&self.filename)
			.file_name()
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_default();
		match base.find('.') {
			Some(dot) => base[..dot].to_string(),
			None => base,
		}
	}

	/// Full filename or "" (C++ `Project::filename()`).
	pub fn filename(&self) -> &str {
		&self.filename
	}

	/// Window-title name (C++ `Project::pretty_filename()`).
	pub fn pretty_filename(&self) -> &str {
		if self.filename.is_empty() {
			"(untitled)"
		} else {
			&self.filename
		}
	}

	/// Set the filename (C++ `Project::set_filename()`).
	pub fn set_filename(&mut self, filename: &str) {
		self.filename = filename.to_string();
	}

	/// 1 when the project has unsaved changes (C++ `is_modified()`).
	pub fn is_modified(&self) -> bool {
		self.modified
	}

	/// Set the modified flag (C++ `set_modified()`).
	pub fn set_modified(&mut self, modified: bool) {
		self.modified = modified;
	}

	/// 1 when the project is new (untitled and unmodified; C++
	/// `is_new()`).
	pub fn is_new(&self) -> bool {
		!self.modified && self.filename.is_empty()
	}

	/// Effective cache directory (C++ `Project::cache_path()`); the
	/// default-location branch consults the oakrender disk-cache path
	/// through the bridge when the setting is not custom/alongside.
	pub fn cache_path(&self) -> String {
		match self.cache_location_setting {
			2 => {
				if !self.custom_cache_path.is_empty() {
					return self.custom_cache_path.clone();
				}
			}
			1 => {
				if !self.filename.is_empty() {
					let dir = std::path::Path::new(&self.filename)
						.parent()
						.map(|p| p.to_string_lossy().into_owned())
						.unwrap_or_default();
					if !dir.is_empty() {
						return format!("{}/cache", dir);
					}
				}
			}
			_ => {}
		}
		// Default location: the shared disk-cache directory (single-lib:
		// lives in oak_core, used by oaknode and oakrender alike).
		oak_core::filefunctions::default_disk_cache_path()
	}

	/// Copy all settings from `src` into `self` (C++
	/// `Project::copy_settings`).
	pub fn copy_settings_from(&mut self, src: &Project) {
		self.settings = src.settings.clone();
		self.cache_location_setting = src.cache_location_setting;
		self.custom_cache_path = src.custom_cache_path.clone();
	}
}

/// Clone a node entry into independently-owned parts (deep copy of the
/// core data; the behavior is re-created via [`NodeBehavior::duplicate`]).
fn clone_entry(
	entry: &crate::graph::NodeEntry,
) -> (crate::node::NodeCore, Box<dyn crate::node::NodeBehavior>) {
	let core = entry.core.clone();
	let behavior = entry
		.behavior
		.duplicate(&core)
		.unwrap_or_else(|| Box::new(crate::nodes::EmptyBehavior));
	(core, behavior)
}

/// UUID v4 in C++ `QUuid::createUuid().toString()` text format
/// (`// CPP-PARITY: project.cpp:483` `regenerate_uuid`).
fn generate_uuid() -> String {
	use std::time::{SystemTime, UNIX_EPOCH};
	// Randomness source: splitmix64 seeded from the clock (test-friendly;
	// real sessions use a stronger seed — this is not a security boundary).
	let nanos = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|d| d.as_nanos() as u64)
		.unwrap_or(0);
	let mut seed = nanos ^ 0x9E3779B97F4A7C15;
	let mut next = move || {
		seed = seed.wrapping_add(0x9E3779B97F4A7C15);
		let mut z = seed;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
		z ^ (z >> 31)
	};
	let mut b = [0u8; 16];
	for chunk in b.chunks_mut(8) {
		let r = next().to_le_bytes();
		chunk.copy_from_slice(&r);
	}
	b[6] = (b[6] & 0x0F) | 0x40; // version 4
	b[8] = (b[8] & 0x3F) | 0x80; // variant 1
	format!(
		"{{{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
		b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
		b[14], b[15]
	)
}

/// A recorded structural change for incremental copy sync (replaces
/// the C++ signal-driven copier updates).
#[derive(Clone, Debug)]
pub enum ChangeRecord {
	/// A node was added.
	NodeAdded(NodeId),
	/// A node was removed (id of the copy-side node).
	NodeRemoved(NodeId),
	/// An edge change.
	EdgeChanged {
		/// Source.
		from: NodeId,
		/// Destination.
		to: NodeId,
		/// Input id.
		input: String,
		/// Element.
		element: i32,
		/// Connected or disconnected.
		connected: bool,
	},
	/// A parameter value changed.
	ValueChanged {
		/// Node.
		node: NodeId,
		/// Input id.
		input: String,
		/// Element.
		element: i32,
	},
}

/// Weak-project handle used by the identity registry (node_from_identity
/// upgrades it; a freed project leaves a dead weak entry that upgrades
/// to `None`).
pub(crate) type WeakProject = Weak<Mutex<Project>>;
