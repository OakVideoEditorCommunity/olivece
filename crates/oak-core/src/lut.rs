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

//! CPU-built 3D LUTs for GPU color management (M2).
//!
//! The display pipeline's per-pixel math (working space → project output
//! spec → display device via ICC) is evaluated once per settings change
//! with the exact CPU reference implementation
//! ([`crate::colormath`] + [`crate::color::ColorProcessor`]) and baked
//! into a 3D LUT. The GPU present pass then samples it with manual
//! trilinear interpolation, so the pixels that reach the swapchain are
//! color-managed on the GPU without a CPU readback — the display
//! transform runs at full precision against the same code the CPU path
//! has always used.
//!
//! The LUT is stored as tightly packed f32 RGB, red varying fastest, and
//! covers the cube `[lo, hi]³`; samples outside the domain clamp to the
//! boundary. Alpha is never part of the transform.

/// A 3D RGB lookup table over `[lo, hi]³`, `edge³` samples, red fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
	/// Samples per axis (≥ 2).
	pub edge: u32,
	/// Domain low corner.
	pub lo: [f32; 3],
	/// Domain high corner.
	pub hi: [f32; 3],
	/// `edge³ * 3` tightly packed RGB values.
	pub data: Vec<f32>,
}

impl Lut3d {
	/// The standard display-transform edge: 65³ samples keep trilinear
	/// error far below the 10-bit display quantization step for the
	/// analytic output-node/ICC chains.
	pub const DISPLAY_EDGE: u32 = 65;

	/// The display LUT's input domain: working-space scene-linear values
	/// can exceed 1 (HDR) and dip negative from gamut matrices. The CPU
	/// output node clamps after its gamut matrix, so values outside this
	/// range are already clipped by the baked transform.
	pub const DISPLAY_LO: [f32; 3] = [-0.25, -0.25, -0.25];
	/// See [`Lut3d::DISPLAY_LO`].
	pub const DISPLAY_HI: [f32; 3] = [4.0, 4.0, 4.0];

	/// Build a LUT by evaluating `f` on the `edge³` grid.
	pub fn build<F: FnMut([f32; 3]) -> [f32; 3]>(
		edge: u32,
		lo: [f32; 3],
		hi: [f32; 3],
		mut f: F,
	) -> Self {
		let edge = edge.max(2);
		let n = edge as usize;
		let mut data = Vec::with_capacity(n * n * n * 3);
		let step = |i: usize, axis: usize| -> f32 {
			let t = i as f32 / (edge - 1) as f32;
			lo[axis] + (hi[axis] - lo[axis]) * t
		};
		for b in 0..n {
			for g in 0..n {
				for r in 0..n {
					let out = f([step(r, 0), step(g, 1), step(b, 2)]);
					data.extend_from_slice(&out);
				}
			}
		}
		Self { edge, lo, hi, data }
	}

	/// The identity LUT (useful for pass-through/legacy display paths).
	pub fn identity(edge: u32) -> Self {
		Self::build(edge, [0.0; 3], [1.0; 3], |c| c)
	}

	/// Index of the sample `(r, g, b)` in [`Lut3d::data`].
	fn index(&self, r: u32, g: u32, b: u32) -> usize {
		(((b * self.edge + g) * self.edge + r) * 3) as usize
	}

	/// One sample (panics if the LUT data is malformed).
	pub fn sample(&self, r: u32, g: u32, b: u32) -> [f32; 3] {
		let i = self.index(r, g, b);
		[self.data[i], self.data[i + 1], self.data[i + 2]]
	}

	/// CPU trilinear evaluation (the reference for tests; the GPU pass
	/// implements the same interpolation in WGSL).
	pub fn eval(&self, rgb: [f32; 3]) -> [f32; 3] {
		let last = self.edge - 1;
		let mut p = [0.0f32; 3];
		for a in 0..3 {
			let span = self.hi[a] - self.lo[a];
			let t = if span.abs() > f32::EPSILON {
				(rgb[a] - self.lo[a]) / span
			} else {
				0.0
			}
			.clamp(0.0, 1.0);
			p[a] = t * last as f32;
		}
		let i0 = [
			p[0].floor() as u32,
			p[1].floor() as u32,
			p[2].floor() as u32,
		];
		let f = [
			p[0] - i0[0] as f32,
			p[1] - i0[1] as f32,
			p[2] - i0[2] as f32,
		];
		let i1 = [
			(i0[0] + 1).min(last),
			(i0[1] + 1).min(last),
			(i0[2] + 1).min(last),
		];
		std::array::from_fn(|a| {
			let c00 = lerp(
				self.sample(i0[0], i0[1], i0[2])[a],
				self.sample(i1[0], i0[1], i0[2])[a],
				f[0],
			);
			let c10 = lerp(
				self.sample(i0[0], i1[1], i0[2])[a],
				self.sample(i1[0], i1[1], i0[2])[a],
				f[0],
			);
			let c01 = lerp(
				self.sample(i0[0], i0[1], i1[2])[a],
				self.sample(i1[0], i0[1], i1[2])[a],
				f[0],
			);
			let c11 = lerp(
				self.sample(i0[0], i1[1], i1[2])[a],
				self.sample(i1[0], i1[1], i1[2])[a],
				f[0],
			);
			let c0 = lerp(c00, c10, f[1]);
			let c1 = lerp(c01, c11, f[1]);
			lerp(c0, c1, f[2])
		})
	}

	/// The LUT domain as `(lo, hi)`.
	pub fn domain(&self) -> ([f32; 3], [f32; 3]) {
		(self.lo, self.hi)
	}
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
	a + (b - a) * t
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn identity_lut_interpolates_linearly() {
		let lut = Lut3d::identity(3);
		assert_eq!(lut.edge, 3);
		let c = lut.eval([0.25, 0.5, 0.75]);
		assert!((c[0] - 0.25).abs() < 1e-6);
		assert!((c[1] - 0.5).abs() < 1e-6);
		assert!((c[2] - 0.75).abs() < 1e-6);
	}

	#[test]
	fn build_covers_the_domain_corners() {
		let lut = Lut3d::build(2, [-1.0; 3], [2.0; 3], |c| [c[0] * 2.0, c[1], c[2]]);
		assert_eq!(lut.sample(0, 0, 0), [-2.0, -1.0, -1.0]);
		assert_eq!(lut.sample(1, 1, 1), [4.0, 2.0, 2.0]);
		// Out-of-domain samples clamp to the nearest corner.
		assert_eq!(lut.eval([-100.0, -100.0, -100.0]), [-2.0, -1.0, -1.0]);
		assert_eq!(lut.eval([100.0, 100.0, 100.0]), [4.0, 2.0, 2.0]);
	}

	#[test]
	fn trilinear_matches_manual_blend() {
		// A linear LUT is reproduced exactly by trilinear interpolation.
		let lut = Lut3d::build(5, [0.0; 3], [1.0; 3], |c| {
			[c[0] * 0.5 + c[1] * 0.25 + c[2] * 0.25, c[1], c[2]]
		});
		for probe in [[0.1, 0.2, 0.3], [0.9, 0.4, 0.7], [0.0, 1.0, 0.5]] {
			let out = lut.eval(probe);
			let expect = [
				probe[0] * 0.5 + probe[1] * 0.25 + probe[2] * 0.25,
				probe[1],
				probe[2],
			];
			for a in 0..3 {
				assert!(
					(out[a] - expect[a]).abs() < 1e-5,
					"axis {a}: {} vs {}",
					out[a],
					expect[a]
				);
			}
		}
	}
}
