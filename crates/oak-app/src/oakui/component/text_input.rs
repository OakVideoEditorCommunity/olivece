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

//! The app's text input component.
//!
//! Wraps the gpui_elements editing engine (IME composition, caret,
//! selection, undo history) with the app's own presentation rules:
//!
//! - **Theme colors**: text, caret, selection, placeholder and the IME
//!   marked underline all come from `App::default_colors` (the app theme)
//!   instead of the engine's hard-coded defaults. The field's text color
//!   is applied through a wrapping `div().text_color(…)` refinement so
//!   the engine's run layout (which reads `Window::text_style`) picks it
//!   up.
//! - **Editing key bindings**: [`install_text_input_bindings`] installs
//!   the engine's Backspace/Delete/arrow/Home/End/select-all bindings
//!   into the app keymap, scoped to the `EditableText` key context. The
//!   app previously never installed them, so every field accepted IME
//!   text but every editing key was a no-op.
//!
//! IME text insertion itself flows through the engine's
//! `EntityInputHandler` (registered by the element via
//! `Window::handle_input`), so composing text, marked ranges and the
//! candidate-window position all work once the field is focused.

use std::sync::Arc;

use gpui::{
	colors::{Colors, DefaultColors},
	div, App, Div, ElementId, Hsla, IntoElement, ParentElement, Styled, WeakEntity,
};
use gpui_elements::editable_text::actions::{default_bindings, DEFAULT_INPUT_CONTEXT};
use gpui_elements::editable_text::{EditableTextElement, EditableTextState};

/// Install the text-input editing key bindings into the app keymap.
///
/// The editing keys (Backspace, Delete, arrows, Home/End, select-all,
/// …) are gpui actions bound to the `EditableText` key context; without
/// them the fields accept IME text but every editing key is a no-op.
/// Call once at app startup, alongside the registry key bindings.
pub fn install_text_input_bindings(cx: &mut App) {
	cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
}

/// The app's text input component.
///
/// A `div` that carries the theme text color refinement around the
/// gpui_elements editing element, with the theme's selection / caret /
/// placeholder / IME-marked colors applied directly. Build with
/// [`text_input`], then chain the engine's element options (`.state`,
/// `.accepts_input`, …).
pub struct TextInput {
	element: EditableTextElement,
	text_color: Hsla,
	selection: Hsla,
	caret: Hsla,
	placeholder: Hsla,
}

impl TextInput {
	/// Bind the field to an existing editing state entity (the caller
	/// reads the value / subscribes to changes through it).
	pub fn state(mut self, state: WeakEntity<EditableTextState>) -> Self {
		self.element = self.element.state(state);
		self
	}

	/// Whether the field accepts input at all (disabled fields render
	/// without the input handler attached).
	pub fn accepts_input(mut self, enabled: bool) -> Self {
		self.element = self.element.accepts_input(enabled);
		self
	}

	/// Placeholder shown while the field is empty.
	pub fn placeholder(mut self, text: impl Into<gpui::SharedString>) -> Self {
		self.element = self.element.placeholder(text);
		self
	}
}

impl IntoElement for TextInput {
	type Element = Div;

	fn into_element(self) -> Div {
		let Self {
			element,
			text_color,
			selection,
			caret,
			placeholder,
		} = self;
		div()
			// The text_color refinement is pushed onto the window text
			// style during paint, so the engine's run layout (which reads
			// `Window::text_style().color`) renders in the theme text
			// color — the field reads like every other label in the app.
			.text_color(text_color)
			.child(
				element
					.placeholder_color(placeholder)
					.selection_color(selection)
					.caret_color(caret)
					.marked_color(text_color),
			)
	}
}

/// Build the app's text input with the current theme's colors.
///
/// `cx` is used for the theme colors; the returned component needs no
/// further styling to match the rest of the UI.
pub fn text_input(id: impl Into<ElementId>, cx: &App) -> TextInput {
	let colors = theme_colors(cx);
	TextInput {
		element: gpui_elements::editable_text::text_input(id),
		text_color: colors.text.into(),
		selection: colors.selected.into(),
		caret: colors.text.into(),
		placeholder: colors.disabled.into(),
	}
}

/// The theme colors for the app (falls back to the defaults when no
/// theme has been applied yet).
fn theme_colors(cx: &App) -> Arc<Colors> {
	cx.default_colors().clone()
}
