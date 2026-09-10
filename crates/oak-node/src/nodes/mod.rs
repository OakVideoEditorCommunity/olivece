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

//! Built-in node types. One file per C++ node class, named after it
//! (e.g. `transformdistortnode.rs`, `viewer` stays in
//! [`crate::sequence`]). Each registers with the factory via
//! [`register_all`].

pub mod blur;
mod chromakey;
mod colordifferencekey;
pub mod compositesource;
mod cornerpindistortnode;
mod cropdistortnode;
mod despill;
mod displaytransform;
mod dropshadowfilter;
mod flipdistortnode;
mod generatorwithmerge;
pub mod group;
mod mask;
mod math;
mod mathbase;
mod matrix;
mod merge;
mod mosaicfilternode;
pub mod multicamnode;
mod noise;
mod ociobase;
mod ociogradingtransformlinear;
mod ociogradingtransformlog;
mod ociolut;
pub mod opacity;
mod pan;
pub mod plugin;
mod polygon;
mod rippledistortnode;
mod shapenode;
mod shapenodebase;
mod solid;
mod stroke;
mod swirldistortnode;
pub mod textbackend;
mod textv1;
mod textv2;
mod textv3;
mod threewaycolor;
mod tiledistortnode;
mod timeformat;
mod timeinput;
mod timeoffsetnode;
mod timeremap;
pub mod transitionfx;
pub mod transitions;
mod transformdistortnode;
mod trigonometry;
mod valuenode;
mod volume;
mod wavedistortnode;
mod whitebalance;
mod colormatrix;
mod dilate;
mod edgedetect;
mod erode;
mod colorcorrect;
mod gamma;
mod saturation;
mod invert;
mod clamp;
mod grade;
mod dirblur;
mod sharpen;
mod dissolve;
mod keymix;
mod premult;
mod unpremult;
mod position;
mod mirror;
mod checkerboard;
mod colorbars;
mod ramp;

use crate::node::{NodeBehavior, NodeCore};

/// A no-op behavior for vacant arena slots (graph internal; never
/// observable through the public API — a vacant slot is only reachable
/// by a stale id, which `Graph::get` rejects).
pub struct EmptyBehavior;

impl NodeBehavior for EmptyBehavior {
	fn name(&self) -> &str {
		""
	}

	fn type_id(&self) -> &str {
		""
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(EmptyBehavior))
	}
}

/// Register every built-in node type with the global factory.
/// Registration order matches the C++ menu order
/// (`// CPP-PARITY: factory.cpp`).
pub fn register_all() {
	let mut meta = Vec::new();

	polygon::register(&mut meta);
	matrix::register(&mut meta);
	transformdistortnode::register(&mut meta);
	volume::register(&mut meta);
	pan::register(&mut meta);
	math::register(&mut meta);
	timeinput::register(&mut meta);
	trigonometry::register(&mut meta);
	blur::register(&mut meta);
	solid::register(&mut meta);
	merge::register(&mut meta);
	stroke::register(&mut meta);
	textv1::register(&mut meta);
	textv2::register(&mut meta);
	textv3::register(&mut meta);
	mosaicfilternode::register(&mut meta);
	cropdistortnode::register(&mut meta);
	valuenode::register(&mut meta);
	timeremap::register(&mut meta);
	shapenode::register(&mut meta);
	colordifferencekey::register(&mut meta);
	despill::register(&mut meta);
	group::register(&mut meta);
	opacity::register(&mut meta);
	flipdistortnode::register(&mut meta);
	noise::register(&mut meta);
	timeoffsetnode::register(&mut meta);
	transitions::register(&mut meta);
	transitionfx::register(&mut meta);
	cornerpindistortnode::register(&mut meta);
	displaytransform::register(&mut meta);
	ociogradingtransformlinear::register(&mut meta);
	ociogradingtransformlog::register(&mut meta);
	whitebalance::register(&mut meta);
	ociolut::register(&mut meta);
	threewaycolor::register(&mut meta);
	chromakey::register(&mut meta);
	mask::register(&mut meta);
	dropshadowfilter::register(&mut meta);
	timeformat::register(&mut meta);
	wavedistortnode::register(&mut meta);
	tiledistortnode::register(&mut meta);
	swirldistortnode::register(&mut meta);
	rippledistortnode::register(&mut meta);
	colormatrix::register(&mut meta);
	dilate::register(&mut meta);
	edgedetect::register(&mut meta);
	erode::register(&mut meta);
	// The OpenFX-Misc cleanroom GPU ports (see
	// docs/zh/plans/ofx-misc-gpu-cleanroom.md), registered after the C++
	// menu order ends and before multicam.
	colorcorrect::register(&mut meta);
	gamma::register(&mut meta);
	saturation::register(&mut meta);
	invert::register(&mut meta);
	clamp::register(&mut meta);
	grade::register(&mut meta);
	dirblur::register(&mut meta);
	sharpen::register(&mut meta);
	dissolve::register(&mut meta);
	keymix::register(&mut meta);
	premult::register(&mut meta);
	unpremult::register(&mut meta);
	position::register(&mut meta);
	mirror::register(&mut meta);
	checkerboard::register(&mut meta);
	colorbars::register(&mut meta);
	ramp::register(&mut meta);
	multicamnode::register(&mut meta);

	// OpenFX plugins have no static type ids (C++
	// `factory.cpp::register_plugin_nodes`); the registration call is a
	// no-op placeholder for the oakplugin bridge's runtime discovery.
	plugin::register(&mut meta);

	crate::factory::install_entries(meta);
}
