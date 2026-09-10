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

//! Built-in node registration smoke tests: `nodes::register_all()`
//! builds the factory entry table in C++ registration order
//! (`factory.cpp::create_from_factory_index` — the switch order, NOT
//! the `factory.h` InternalID enum order: `initialize()` iterates the
//! enum indices but the switch reorders log/white-balance after
//! linear, and tile/swirl/ripple after wave).

use oak_node::factory::Factory;
use oak_node::node::Category;

/// Expected `type_id` sequence, in C++ registration order
/// (`factory.cpp::create_from_factory_index`; the plugin nodes that
/// follow are registered at runtime by the oakplugin bridge and are not
/// part of this table).
const EXPECTED_ORDER: &[&str] = &[
	"org.olivevideoeditor.Olive.polygon",
	"org.olivevideoeditor.Olive.ortho",
	"org.olivevideoeditor.Olive.transform",
	"org.olivevideoeditor.Olive.volume",
	"org.olivevideoeditor.Olive.pan",
	"org.olivevideoeditor.Olive.math",
	"org.olivevideoeditor.Olive.time",
	"org.olivevideoeditor.Olive.trigonometry",
	"org.olivevideoeditor.Olive.blur",
	"org.olivevideoeditor.Olive.solidgenerator",
	"org.olivevideoeditor.Olive.merge",
	"org.olivevideoeditor.Olive.stroke",
	"org.olivevideoeditor.Olive.textgenerator",
	"org.olivevideoeditor.Olive.text2",
	"org.olivevideoeditor.Olive.text3",
	"org.olivevideoeditor.Olive.mosaicfilter",
	"org.olivevideoeditor.Olive.crop",
	"org.olivevideoeditor.Olive.value",
	"org.olivevideoeditor.Olive.timeremap",
	"org.olivevideoeditor.Olive.shape",
	"org.olivevideoeditor.Olive.colordifferencekey",
	"org.olivevideoeditor.Olive.despill",
	"org.olivevideoeditor.Olive.group",
	"org.olivevideoeditor.Olive.opacity",
	"org.olivevideoeditor.Olive.flip",
	"org.olivevideoeditor.Olive.noise",
	"org.olivevideoeditor.Olive.timeoffset",
	"org.olivevideoeditor.Olive.transition",
	"org.olivevideoeditor.Olive.transitionfx",
	"org.olivevideoeditor.Olive.cornerpin",
	"org.olivevideoeditor.Olive.displaytransform",
	"org.olivevideoeditor.Olive.ociogradingtransformlinear",
	"org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog",
	"org.olivevideoeditor.Olive.whitebalance",
	"org.olivevideoeditor.Olive.ociolut",
	"org.olivevideoeditor.Olive.threewaycolor",
	"org.olivevideoeditor.Olive.chromakey",
	"org.olivevideoeditor.Olive.mask",
	"org.olivevideoeditor.Olive.dropshadow",
	"org.olivevideoeditor.Olive.timeformat",
	"org.olivevideoeditor.Olive.wave",
	"org.olivevideoeditor.Olive.tile",
	"org.olivevideoeditor.Olive.swirl",
	"org.olivevideoeditor.Olive.ripple",
	"org.olivevideoeditor.Olive.colormatrix",
	"org.olivevideoeditor.Olive.dilate",
	"org.olivevideoeditor.Olive.edgedetect",
	"org.olivevideoeditor.Olive.erode",
	// The OpenFX-Misc cleanroom GPU ports, registered between the C++
	// menu order and multicam (see `nodes::register_all`).
	"org.olivevideoeditor.Olive.colorcorrect",
	"org.olivevideoeditor.Olive.gamma",
	"org.olivevideoeditor.Olive.saturation",
	"org.olivevideoeditor.Olive.invert",
	"org.olivevideoeditor.Olive.clamp",
	"org.olivevideoeditor.Olive.grade",
	"org.olivevideoeditor.Olive.dirblur",
	"org.olivevideoeditor.Olive.sharpen",
	"org.olivevideoeditor.Olive.dissolve",
	"org.olivevideoeditor.Olive.keymix",
	"org.olivevideoeditor.Olive.premult",
	"org.olivevideoeditor.Olive.unpremult",
	"org.olivevideoeditor.Olive.position",
	"org.olivevideoeditor.Olive.mirror",
	"org.olivevideoeditor.Olive.checkerboard",
	"org.olivevideoeditor.Olive.colorbars",
	"org.olivevideoeditor.Olive.ramp",
	"org.olivevideoeditor.Olive.multicam",
];

/// `register_all()` installs every built-in node, exactly once, in C++
/// factory order.
#[test]
fn registered_entries_match_cpp_order() {
	let entries = Factory::global().entries();
	assert_eq!(entries.len(), EXPECTED_ORDER.len());
	for (i, (entry, expected)) in entries.iter().zip(EXPECTED_ORDER.iter()).enumerate() {
		assert_eq!(entry.type_id, *expected, "mismatch at entry {i}");
	}
}

/// First and last entries are polygon and multi-cam respectively
/// (multi-cam is the last built-in in `factory.cpp` switch order).
#[test]
fn first_and_last_entries() {
	let entries = Factory::global().entries();
	assert_eq!(
		entries.first().unwrap().type_id,
		"org.olivevideoeditor.Olive.polygon"
	);
	assert_eq!(entries.first().unwrap().name, "Polygon");
	assert_eq!(
		entries.last().unwrap().type_id,
		"org.olivevideoeditor.Olive.multicam"
	);
	assert_eq!(entries.last().unwrap().name, "Multi-Cam");
}

/// `find()` resolves a type id to its metadata (pan, index 4 in the
/// table).
#[test]
fn find_pan_entry() {
	let meta = Factory::global()
		.find("org.olivevideoeditor.Olive.pan")
		.expect("pan is a built-in node");
	assert_eq!(meta.name, "Pan");
	assert_eq!(meta.categories, &[Category::Filter]);
}
