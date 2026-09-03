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

//! `olive::ColorTransform` — a color transform description. Mirrors
//! `src/common/src/colortransform.h` and `include/common/colortransform.h`.
//!
//! A transform is either an output-colorspace transform or a
//! display/view/look transform. The C++ object is passed through the C ABI
//! as a refcounted handle ([`OakColorTransform`] in `crate::ffi`); this
//! module owns the plain-data description behind the handle.
//!
//! The C++-only functions `oak_core_colortransform_init_from_native` /
//! `get_native` take or return `olive::ColorTransform` and cannot be
//! expressed from Rust; they are served by the C++ adapter layer, not here.

/// `olive::ColorTransform` — the plain-data description behind the handle.
pub struct ColorTransform {
	/// Output colorspace name (empty when this is a display transform).
	output: String,
	/// Display name (empty when this is an output-colorspace transform).
	display: String,
	/// Whether this is a display/view/look transform.
	// CPP-PARITY: the C++ class tracks this with an explicit `is_display_`
	// member (`src/common/src/colortransform.h`). We keep it as a separate
	// bool as well so an empty display name on a display transform is still
	// recognized as a display transform.
	is_display: bool,
	/// View name (display transforms only).
	view: String,
	/// Look name (display transforms only).
	look: String,
}

impl ColorTransform {
	/// New output-colorspace transform.
	pub fn new_output(output: &str) -> Self {
		Self {
			output: output.to_string(),
			display: String::new(),
			is_display: false,
			view: String::new(),
			look: String::new(),
		}
	}

	/// New display/view/look transform.
	pub fn new_display(display: &str, view: &str, look: &str) -> Self {
		// CPP-PARITY: the C++ display constructor stores the display name in
		// the shared `output_` member and sets `is_display_ = true`; here the
		// display name lives in its own `display` field (the skeleton's public
		// `display()` / `output()` accessors read the respective field).
		Self {
			output: String::new(),
			display: display.to_string(),
			is_display: true,
			view: view.to_string(),
			look: look.to_string(),
		}
	}

	/// Whether this is a display/view/look transform.
	pub fn is_display(&self) -> bool {
		self.is_display
	}

	/// Display name (empty if this is an output transform).
	pub fn display(&self) -> &str {
		&self.display
	}

	/// Output colorspace name (empty if this is a display transform).
	pub fn output(&self) -> &str {
		&self.output
	}

	/// View name.
	pub fn view(&self) -> &str {
		&self.view
	}

	/// Look name.
	pub fn look(&self) -> &str {
		&self.look
	}
}

#[cfg(test)]
mod tests {
	use super::ColorTransform;

	#[test]
	fn output_transform_defaults() {
		let t = ColorTransform::new_output("sRGB");
		assert!(!t.is_display());
		assert_eq!(t.output(), "sRGB");
		assert_eq!(t.display(), "");
		assert_eq!(t.view(), "");
		assert_eq!(t.look(), "");
	}

	#[test]
	fn display_transform_defaults() {
		let t = ColorTransform::new_display("DCI-P3", "standard", "soft");
		assert!(t.is_display());
		assert_eq!(t.display(), "DCI-P3");
		assert_eq!(t.view(), "standard");
		assert_eq!(t.look(), "soft");
		assert_eq!(t.output(), "");
	}

	#[test]
	fn output_transform_empty_strings() {
		// Mirrors the C++ default constructor: an output transform built from
		// an empty string still has `is_display == false`.
		let t = ColorTransform::new_output("");
		assert!(!t.is_display());
		assert_eq!(t.output(), "");
	}

	#[test]
	fn display_transform_recognized_even_with_empty_display_name() {
		// The explicit is_display flag (vs. deriving from a non-empty display
		// name) keeps an empty-named display transform identifiable.
		let t = ColorTransform::new_display("", "view", "");
		assert!(t.is_display());
		assert_eq!(t.display(), "");
		assert_eq!(t.view(), "view");
	}

	#[test]
	fn accessors_return_borrowed_slices() {
		let t = ColorTransform::new_display("Display", "View", "Look");
		let d: &str = t.display();
		let v: &str = t.view();
		let l: &str = t.look();
		let o: &str = t.output();
		assert_eq!(d, "Display");
		assert_eq!(v, "View");
		assert_eq!(l, "Look");
		assert_eq!(o, "");
	}

	#[test]
	fn c_api_string_sizes() {
		// The c_api string getters return the required size including the NUL
		// (value.size() + 1) and, being NON-TRUNCATING, only copy when the
		// buffer is large enough. Verify the values the domain layer exposes
		// line up with those sizes.
		let t = ColorTransform::new_display("RGB", "ACES", "none");
		assert_eq!(t.display().len() + 1, 4);
		assert_eq!(t.view().len() + 1, 5);
		assert_eq!(t.look().len() + 1, 5);
	}
}
