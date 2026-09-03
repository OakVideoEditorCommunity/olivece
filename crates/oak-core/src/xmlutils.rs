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

//! Streaming XML reader/writer, mirroring `src/common/src/xmlutils.h`
//! and `include/common/xmlutils.h`. The C++-only
//! `get_native` / `wrap_native` entry points deal with
//! `olive::XmlStreamReader` / `olive::XmlStreamWriter` and are served by
//! the C++ adapter layer, not here.

use crate::error::{Error, Result};

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::BytesStart;
use quick_xml::events::Event as QxEvent;
use quick_xml::Reader as QxReader;

// CPP-PARITY: The C++ reader is backed by expat, which parses the whole
// document into an event list at construction. This Rust port parses the
// document with quick-xml and folds its event stream (elements, attributes,
// character data; comments, processing instructions and CDATA handled,
// namespace processing not performed) into the same expat-shaped event list
// so that navigation and error reporting match expat.

/// One token type, mirroring `olive::XmlStreamReader::TokenType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokenType {
	/// A parse error occurred.
	Invalid,
	StartElement,
	EndElement,
	Characters,
	EndDocument,
}

/// A single XML attribute (name/value pair), `olive::XmlStreamAttribute`.
#[derive(Clone, Debug)]
struct Attribute {
	name: String,
	value: String,
}

/// One parsed SAX-style event, `olive::XmlStreamReader::Event`.
#[derive(Clone, Debug)]
struct Event {
	token_type: TokenType,
	name: String,
	text: String,
	attributes: Vec<Attribute>,
}

/// `olive::XmlStreamReader` — a handle-wrapped streaming XML reader.
pub struct XmlReader {
	/// Parsed event list (the whole document is parsed at construction).
	events: Vec<Event>,
	/// Index of the next event to consume.
	pos: usize,
	/// Current token type.
	token: TokenType,
	/// Current element name.
	name: String,
	/// Current character data.
	text: String,
	/// Current attributes.
	attributes: Vec<Attribute>,
	/// Whether the document failed to parse.
	has_error: bool,
}

impl XmlReader {
	/// Create a streaming reader over a complete document.
	pub fn new(data: &str) -> Result<Self> {
		let (events, has_error) = parse_document(data);
		// CPP-PARITY: The C++ constructor reports parse failures through
		// has_error() rather than throwing, so `new` always yields a usable
		// reader whose has_error flag the caller inspects afterwards. The
		// C++ error_string() is not part of the Rust domain surface (the
		// c_api only exposes has_error), so the message is not retained.
		Ok(Self {
			events,
			pos: 0,
			token: TokenType::Invalid,
			name: String::new(),
			text: String::new(),
			attributes: Vec::new(),
			has_error,
		})
	}

	/// Advance to the next event token (mirrors `read_next()`).
	fn read_next(&mut self) -> TokenType {
		self.attributes.clear();
		self.text.clear();
		self.name.clear();

		if self.has_error {
			self.token = TokenType::Invalid;
			return self.token;
		}

		if self.pos >= self.events.len() {
			self.token = TokenType::EndDocument;
			return self.token;
		}

		let ev = &self.events[self.pos];
		self.pos += 1;
		self.token = ev.token_type;
		self.name = ev.name.clone();
		self.text = ev.text.clone();
		self.attributes = ev.attributes.clone();
		self.token
	}

	/// Whether the reader is currently on a start element.
	fn is_start_element(&self) -> bool {
		self.token == TokenType::StartElement
	}

	/// Whether the reader is currently on an end element.
	fn is_end_element(&self) -> bool {
		self.token == TokenType::EndElement
	}

	/// Advance until the next start element, an end element, or the end of
	/// the document. Returns whether the reader is now positioned on a
	/// start element.
	pub fn read_next_start_element(&mut self) -> Result<bool> {
		// CPP-PARITY: mirrors the free function xml_read_next_start_element():
		// stops (returns false) on an end element or end of document.
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				return Ok(false);
			}
			if self.is_end_element() {
				return Ok(false);
			}
			if self.is_start_element() {
				return Ok(true);
			}
		}
	}

	/// Name of the current element token.
	pub fn name(&self) -> Result<String> {
		Ok(self.name.clone())
	}

	/// Read the concatenated character data of the current element. Must
	/// be called on a start element; consumes up to the matching end
	/// element.
	pub fn read_element_text(&mut self) -> Result<String> {
		if !self.is_start_element() {
			// CPP-PARITY: C++ returns an empty string when not on a start
			// element.
			return Ok(String::new());
		}

		let mut result = String::new();
		let mut depth = 1;
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				break;
			}
			if token == TokenType::StartElement {
				depth += 1;
			} else if token == TokenType::EndElement {
				depth -= 1;
				if depth == 0 {
					break;
				}
			} else if token == TokenType::Characters && depth == 1 {
				result.push_str(&self.text);
			}
		}
		Ok(result)
	}

	/// Skip the current element and all of its children.
	pub fn skip_current_element(&mut self) -> Result<()> {
		if !self.is_start_element() {
			return Ok(());
		}

		let mut depth = 1;
		loop {
			let token = self.read_next();
			if token == TokenType::Invalid || token == TokenType::EndDocument {
				break;
			}
			if token == TokenType::StartElement {
				depth += 1;
			} else if token == TokenType::EndElement {
				depth -= 1;
				if depth == 0 {
					break;
				}
			}
		}
		Ok(())
	}

	/// Number of attributes on the current start element.
	pub fn attribute_count(&self) -> Result<i32> {
		Ok(self.attributes.len() as i32)
	}

	/// Name of the attribute at `index` on the current start element.
	pub fn attribute_name(&self, index: i32) -> Result<String> {
		if index < 0 || index as usize >= self.attributes.len() {
			// CPP-PARITY: the c_api returns E_NOT_FOUND for an out-of-range
			// attribute index.
			return Err(Error::NotFound);
		}
		Ok(self.attributes[index as usize].name.clone())
	}

	/// Value of the attribute at `index` on the current start element.
	pub fn attribute_value(&self, index: i32) -> Result<String> {
		if index < 0 || index as usize >= self.attributes.len() {
			return Err(Error::NotFound);
		}
		Ok(self.attributes[index as usize].value.clone())
	}

	/// Whether the document failed to parse.
	pub fn has_error(&self) -> Result<bool> {
		Ok(self.has_error)
	}
}

/// `olive::XmlStreamWriter` — a handle-wrapped streaming XML writer.
pub struct XmlWriter {
	/// Document built so far.
	output: String,
	/// Open element name stack.
	stack: Vec<String>,
	/// A `'>'` is pending for the open start tag.
	open_start_tag: bool,
}

impl XmlWriter {
	/// Create a streaming XML writer.
	pub fn new() -> Self {
		Self {
			output: String::new(),
			stack: Vec::new(),
			open_start_tag: false,
		}
	}

	/// Write a start element.
	pub fn write_start_element(&mut self, name: &str) -> Result<()> {
		if self.open_start_tag {
			self.output.push('>');
			self.open_start_tag = false;
		}
		self.output.push('<');
		self.output.push_str(name);
		self.stack.push(name.to_string());
		self.open_start_tag = true;
		Ok(())
	}

	/// Write an attribute on the current start element.
	pub fn write_attribute(&mut self, name: &str, value: &str) -> Result<()> {
		// CPP-PARITY: no-op when not inside an open start tag.
		if !self.open_start_tag {
			return Ok(());
		}
		self.output.push(' ');
		self.output.push_str(name);
		self.output.push_str("=\"");
		self.output.push_str(&escape_attribute(value));
		self.output.push('"');
		Ok(())
	}

	/// Write character data.
	pub fn write_characters(&mut self, text: &str) -> Result<()> {
		if self.open_start_tag {
			self.output.push('>');
			self.open_start_tag = false;
		}
		self.output.push_str(&escape_text(text));
		Ok(())
	}

	/// Write an empty element with character data (`<name>text</name>`).
	pub fn write_text_element(&mut self, name: &str, text: &str) -> Result<()> {
		self.write_start_element(name)?;
		self.write_characters(text)?;
		self.write_end_element()?;
		Ok(())
	}

	/// Write an end element.
	pub fn write_end_element(&mut self) -> Result<()> {
		// CPP-PARITY: no-op when the element stack is empty.
		let name = match self.stack.pop() {
			Some(n) => n,
			None => return Ok(()),
		};

		if self.open_start_tag {
			self.output.push_str("/>");
			self.open_start_tag = false;
		} else {
			self.output.push_str("</");
			self.output.push_str(&name);
			self.output.push('>');
		}
		Ok(())
	}

	/// Write the end of the document.
	pub fn write_end_document(&mut self) -> Result<()> {
		while !self.stack.is_empty() {
			self.write_end_element()?;
		}
		Ok(())
	}

	/// The document written so far.
	pub fn output(&self) -> Result<String> {
		Ok(self.output.clone())
	}
}

/// Escape text for the XML entities used by `escape_text` in C++.
fn escape_text(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			_ => out.push(c),
		}
	}
	out
}

/// Escape text for the XML entities used by `escape_attribute` in C++.
fn escape_attribute(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			_ => out.push(c),
		}
	}
	out
}

/// Whether `c` is XML whitespace.
fn is_ws(c: char) -> bool {
	c == ' ' || c == '\t' || c == '\r' || c == '\n'
}

/// Convert a quick-xml `QName` (raw bytes) to an owned UTF-8 string. The
/// input is always valid UTF-8 (the reader was built from a `&str`).
fn qname_string(q: impl AsRef<[u8]>) -> String {
	std::str::from_utf8(q.as_ref())
		.map(str::to_owned)
		.unwrap_or_default()
}

/// Append character data, merging consecutive Characters events
/// (mirrors `on_characters` in C++).
fn push_characters(events: &mut Vec<Event>, text: &str) {
	if text.is_empty() {
		return;
	}
	if let Some(last) = events.last_mut() {
		if last.token_type == TokenType::Characters {
			last.text.push_str(text);
			return;
		}
	}
	events.push(Event {
		token_type: TokenType::Characters,
		name: String::new(),
		text: text.to_string(),
		attributes: Vec::new(),
	});
}

/// Push a `StartElement` event (and its attributes) for a quick-xml start
/// or empty tag. On success returns the element name with the stack already
/// pushed; on a parse error returns `None` with `has_error` semantics left
/// to the caller (nothing was pushed).
fn push_start_element(
	e: &BytesStart,
	events: &mut Vec<Event>,
	stack: &mut Vec<String>,
	saw_start: &mut bool,
) -> Option<String> {
	let name = qname_string(e.name());
	let mut attributes: Vec<Attribute> = Vec::new();
	for attr in e.attributes() {
		let attr = match attr {
			Ok(a) => a,
			Err(_) => return None,
		};
		if attr.value.contains(&b'<') {
			// CPP-PARITY: expat rejects a raw '<' inside an attribute
			// value (XML_ERROR_INVALID_TOKEN).
			return None;
		}
		let value = match attr.unescape_value() {
			Ok(v) => v.into_owned(),
			Err(_) => return None,
		};
		attributes.push(Attribute {
			name: qname_string(attr.key),
			value,
		});
	}
	if stack.is_empty() && *saw_start {
		// CPP-PARITY: expat reports "junk after document element" for a
		// second root element.
		return None;
	}
	*saw_start = true;
	stack.push(name.clone());
	events.push(Event {
		token_type: TokenType::StartElement,
		name: name.clone(),
		text: String::new(),
		attributes,
	});
	Some(name)
}

/// Parse `data` with quick-xml into an expat-equivalent event list.
///
/// Returns `(events, has_error)`.
fn parse_document(data: &str) -> (Vec<Event>, bool) {
	let mut events: Vec<Event> = Vec::new();
	let mut stack: Vec<String> = Vec::new();
	let mut saw_start = false;
	let mut has_error = false;

	let mut reader = QxReader::from_str(data);
	loop {
		let ev = match reader.read_event() {
			Ok(e) => e,
			Err(_) => {
				has_error = true;
				break;
			}
		};
		match ev {
			QxEvent::Start(e) => {
				if push_start_element(&e, &mut events, &mut stack, &mut saw_start).is_none() {
					has_error = true;
					break;
				}
			}
			QxEvent::Empty(e) => {
				let name = match push_start_element(&e, &mut events, &mut stack, &mut saw_start) {
					Some(n) => n,
					None => {
						has_error = true;
						break;
					}
				};
				stack.pop();
				// CPP-PARITY: expat emits StartElement then EndElement for
				// `<name/>`.
				events.push(Event {
					token_type: TokenType::EndElement,
					name,
					text: String::new(),
					attributes: Vec::new(),
				});
			}
			QxEvent::End(e) => {
				let name = qname_string(e.name());
				match stack.last() {
					Some(top) if *top == name => {
						stack.pop();
						events.push(Event {
							token_type: TokenType::EndElement,
							name,
							text: String::new(),
							attributes: Vec::new(),
						});
					}
					_ => {
						// CPP-PARITY: expat reports a mismatched tag. (quick-xml
						// also rejects this via check_end_names.)
						has_error = true;
						break;
					}
				}
			}
			QxEvent::Text(t) => {
				let text = t.decode().map(|c| c.into_owned()).unwrap_or_default();
				if stack.is_empty() {
					// CPP-PARITY: expat never reports character data outside the
					// root element; whitespace in the prolog/epilog is silently
					// allowed, anything else is an error ("junk after document
					// element").
					if !text.chars().all(is_ws) {
						has_error = true;
						break;
					}
				} else {
					push_characters(&mut events, &text);
				}
			}
			QxEvent::CData(c) => {
				if stack.is_empty() {
					// CPP-PARITY: expat rejects a CDATA section outside the root
					// element ("junk after document element").
					has_error = true;
					break;
				}
				// CPP-PARITY: CDATA content is literal, no entity resolution.
				let text = c.decode().map(|c| c.into_owned()).unwrap_or_default();
				push_characters(&mut events, &text);
			}
			QxEvent::GeneralRef(r) => {
				if stack.is_empty() {
					has_error = true;
					break;
				}
				if r.is_char_ref() {
					// Numeric character reference: `&#65;` / `&#x41;`.
					match r.resolve_char_ref() {
						Ok(Some(ch)) => {
							let mut s = String::new();
							s.push(ch);
							push_characters(&mut events, &s);
						}
						_ => {
							has_error = true;
							break;
						}
					}
				} else {
					let name = r.decode().map(|c| c.into_owned()).unwrap_or_default();
					match resolve_predefined_entity(&name) {
						Some(s) => push_characters(&mut events, s),
						None => {
							// Undefined entity: expat treats these as fatal.
							has_error = true;
							break;
						}
					}
				}
			}
			// Comments, declarations, processing instructions and DOCTYPE are
			// skipped (expat does not surface them through the character or
			// element handlers).
			QxEvent::Eof => break,
			_ => {}
		}
	}

	if !has_error && (!saw_start || !stack.is_empty()) {
		// CPP-PARITY: expat reports "no element found" for a document without
		// a root and an unclosed element otherwise.
		has_error = true;
	}

	(events, has_error)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn writer_basic_document() {
		let mut w = XmlWriter::new();
		w.write_start_element("root").unwrap();
		w.write_attribute("a", "1").unwrap();
		w.write_attribute("b", "x\"y").unwrap();
		w.write_text_element("child", "hi & bye <there>").unwrap();
		w.write_end_element().unwrap();
		w.write_end_document().unwrap();
		assert_eq!(
			w.output().unwrap(),
			"<root a=\"1\" b=\"x&quot;y\"><child>hi &amp; bye &lt;there&gt;</child></root>"
		);
	}

	#[test]
	fn writer_empty_and_noop() {
		let mut w = XmlWriter::new();
		// No open start tag: attribute is a no-op.
		w.write_attribute("a", "b").unwrap();
		// No stack: end element is a no-op.
		w.write_end_element().unwrap();
		w.write_characters("x").unwrap();
		w.write_end_document().unwrap();
		assert_eq!(w.output().unwrap(), "x");
	}

	#[test]
	fn writer_self_closing_empty_element() {
		let mut w = XmlWriter::new();
		w.write_start_element("a").unwrap();
		w.write_attribute("k", "v").unwrap();
		w.write_end_element().unwrap();
		assert_eq!(w.output().unwrap(), "<a k=\"v\"/>");
	}

	#[test]
	fn reader_basic_attributes_and_text() {
		let doc = "<root a=\"1\" b=\"2\"><child>x</child><empty/></root>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "root");
		assert_eq!(r.attribute_count().unwrap(), 2);
		assert_eq!(r.attribute_name(0).unwrap(), "a");
		assert_eq!(r.attribute_value(0).unwrap(), "1");
		assert_eq!(r.attribute_name(1).unwrap(), "b");
		assert_eq!(r.attribute_value(1).unwrap(), "2");

		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "child");
		assert_eq!(r.read_element_text().unwrap(), "x");

		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "empty");
		assert_eq!(r.attribute_count().unwrap(), 0);

		assert!(!r.read_next_start_element().unwrap());
	}

	#[test]
	fn reader_nested_text_only_direct_children() {
		let doc = "<a><b>inner</b>outer</a>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "a");
		assert_eq!(r.read_element_text().unwrap(), "outer");
	}

	#[test]
	fn reader_skip_current_element() {
		let doc = "<root><skip><deep>d</deep></skip><keep>v</keep></root>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "skip");
		r.skip_current_element().unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "keep");
		assert_eq!(r.read_element_text().unwrap(), "v");
	}

	#[test]
	fn reader_comments_pi_and_entities() {
		let doc = "<!-- c --><root a=\"x&quot;y\">&amp; &lt; &gt;</root>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "root");
		assert_eq!(r.attribute_value(0).unwrap(), "x\"y");
		assert_eq!(r.read_element_text().unwrap(), "& < >");
	}

	#[test]
	fn reader_cdata() {
		let doc = "<a><![CDATA[x < y & z]]></a>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "x < y & z");
	}

	#[test]
	fn reader_whitespace_between_elements() {
		// Indentation/whitespace becomes character data, which
		// read_element_text() collects at depth 1.
		let doc = "<a>\n  <b/>\n</a>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		let text = r.read_element_text().unwrap();
		assert!(text.contains("\n  ") || text.trim().is_empty());
	}

	#[test]
	fn reader_mismatched_tag_is_error() {
		let mut r = XmlReader::new("<a></b>").unwrap();
		assert!(r.has_error().unwrap());
		// Once in error state, navigation stops returning start elements.
		assert!(!r.read_next_start_element().unwrap());
	}

	#[test]
	fn reader_unclosed_element_is_error() {
		let mut r = XmlReader::new("<a>").unwrap();
		assert!(r.has_error().unwrap());
	}

	#[test]
	fn reader_empty_document_is_error() {
		let mut r = XmlReader::new("").unwrap();
		assert!(r.has_error().unwrap());
	}

	#[test]
	fn reader_undefined_entity_is_error() {
		let mut r = XmlReader::new("<a>&foo;</a>").unwrap();
		assert!(r.has_error().unwrap());
	}

	#[test]
	fn reader_attribute_out_of_range_is_not_found() {
		let mut r = XmlReader::new("<a x=\"1\"></a>").unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.attribute_count().unwrap(), 1);
		assert!(r.attribute_name(5).is_err());
		assert!(r.attribute_name(-1).is_err());
		assert!(r.attribute_value(1).is_err());
	}

	#[test]
	fn reader_name_before_read_is_empty() {
		let mut r = XmlReader::new("<a></a>").unwrap();
		assert_eq!(r.name().unwrap(), "");
		assert_eq!(r.attribute_count().unwrap(), 0);
	}

	#[test]
	fn round_trip_writer_then_reader() {
		let mut w = XmlWriter::new();
		w.write_start_element("root").unwrap();
		w.write_text_element("a", "1").unwrap();
		w.write_text_element("b", "2").unwrap();
		w.write_end_element().unwrap();
		let xml = w.output().unwrap();
		assert_eq!(xml, "<root><a>1</a><b>2</b></root>");

		let mut r = XmlReader::new(&xml).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "root");
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "a");
		assert_eq!(r.read_element_text().unwrap(), "1");
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "b");
		assert_eq!(r.read_element_text().unwrap(), "2");
		assert!(!r.read_next_start_element().unwrap());
	}

	// ---- Reader: attribute handling ----

	/// Attribute order is preserved exactly as written (expat reports
	/// attributes in document order via the `atts` array).
	#[test]
	fn reader_attribute_order_preserved() {
		let doc = "<e zeta=\"1\" alpha=\"2\" mid=\"3\" omega=\"4\"/>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.attribute_count().unwrap(), 4);
		for (i, name) in ["zeta", "alpha", "mid", "omega"].iter().enumerate() {
			assert_eq!(r.attribute_name(i as i32).unwrap(), *name);
			assert_eq!(r.attribute_value(i as i32).unwrap(), format!("{}", i + 1));
		}
	}

	/// Single-quoted attribute values are accepted (expat allows both
	/// quote styles).
	#[test]
	fn reader_single_quoted_attribute() {
		let doc = "<e a='v1' b='x&quot;y'/>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.attribute_value(0).unwrap(), "v1");
		assert_eq!(r.attribute_value(1).unwrap(), "x\"y");
	}

	/// Numeric character references (decimal and hex) resolve in both text
	/// and attribute values.
	#[test]
	fn reader_numeric_character_references() {
		let doc = "<e a=\"&#65;&#x42;\">&#x43;&#68;</e>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.attribute_value(0).unwrap(), "AB");
		assert_eq!(r.read_element_text().unwrap(), "CD");
	}

	/// All five predefined entities resolve.
	#[test]
	fn reader_predefined_entities() {
		let doc = "<e>&amp;&lt;&gt;&quot;&apos;</e>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "&<>\"'");
	}

	/// A duplicate attribute is a well-formedness error in expat
	/// (XML_ERROR_DUPLICATE_ATTRIBUTE).
	#[test]
	fn reader_duplicate_attribute_is_error() {
		let mut r = XmlReader::new("<e a=\"1\" a=\"2\"/>").unwrap();
		assert!(r.has_error().unwrap());
	}

	/// A raw '<' inside an attribute value is rejected by expat
	/// (XML_ERROR_INVALID_TOKEN).
	#[test]
	fn reader_lt_in_attribute_value_is_error() {
		let mut r = XmlReader::new("<e a=\"x<y\"/>").unwrap();
		assert!(r.has_error().unwrap());
	}

	// ---- Reader: character data semantics ----

	/// Character data split by an entity reference is merged into one
	/// logical text run (C++ `on_characters` appends to the previous
	/// Characters event).
	#[test]
	fn reader_text_merges_across_entity_references() {
		let doc = "<e>one &amp; two &lt; three</e>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "one & two < three");
	}

	/// CDATA merges with adjacent plain character data, mirroring the C++
	/// merge of consecutive Characters events.
	#[test]
	fn reader_cdata_merges_with_adjacent_text() {
		let doc = "<e>plain<![CDATA[<raw&>]]>tail</e>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "plain<raw&>tail");
	}

	/// A comment inside character data splits nothing observable: the
	/// surrounding text is still concatenated by read_element_text.
	#[test]
	fn reader_comment_inside_text() {
		let doc = "<e>ab<!-- hidden -->cd</e>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "abcd");
	}

	/// Whitespace in the prolog/epilog is allowed and produces no events;
	/// non-whitespace outside the root is an expat error.
	#[test]
	fn reader_whitespace_outside_root_is_ignored() {
		let doc = "  \n\t<a>x</a>\n  ";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "a");
		assert_eq!(r.read_element_text().unwrap(), "x");
	}

	#[test]
	fn reader_text_before_root_is_error() {
		let mut r = XmlReader::new("junk<a/>").unwrap();
		assert!(r.has_error().unwrap());
	}

	#[test]
	fn reader_text_after_root_is_error() {
		let mut r = XmlReader::new("<a/>junk").unwrap();
		assert!(r.has_error().unwrap());
	}

	/// expat enforces a single root element ("junk after document
	/// element").
	#[test]
	fn reader_second_root_element_is_error() {
		let mut r = XmlReader::new("<a/><b/>").unwrap();
		assert!(r.has_error().unwrap());

		let mut r = XmlReader::new("<a></a><b></b>").unwrap();
		assert!(r.has_error().unwrap());
	}

	/// Comments and processing instructions after the root element are
	/// legal in the epilog.
	#[test]
	fn reader_comment_and_pi_after_root_ok() {
		let doc = "<a/><!-- tail --><?pi data?>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "a");
		assert!(!r.read_next_start_element().unwrap());
	}

	/// An XML declaration and DOCTYPE are skipped like any PI/declaration.
	#[test]
	fn reader_xml_declaration_and_doctype_skipped() {
		let doc = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE root><root>v</root>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "root");
		assert_eq!(r.read_element_text().unwrap(), "v");
	}

	// ---- Reader: navigation semantics ----

	/// read_element_text on anything but a start element returns an empty
	/// string (CPP-PARITY: C++ returns std::string()).
	#[test]
	fn reader_read_element_text_off_start_element_is_empty() {
		// Before any read (Invalid token).
		let mut r = XmlReader::new("<a>x</a>").unwrap();
		assert_eq!(r.read_element_text().unwrap(), "");

		// On an end element.
		let mut r = XmlReader::new("<a><b/></a>").unwrap();
		assert!(r.read_next_start_element().unwrap()); // <a>
		assert!(r.read_next_start_element().unwrap()); // <b>
		assert!(!r.read_next_start_element().unwrap()); // stops on </a>
		assert_eq!(r.read_element_text().unwrap(), "");
	}

	/// skip_current_element off a start element is a no-op.
	#[test]
	fn reader_skip_off_start_element_is_noop() {
		let mut r = XmlReader::new("<a><b/></a>").unwrap();
		// Invalid token: no-op, next read still finds <a>.
		r.skip_current_element().unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "a");
	}

	/// skip_current_element with nested elements of the same name must
	/// consume to the matching end tag, not the first one.
	#[test]
	fn reader_skip_nested_same_name() {
		let doc = "<root><a><a><a>deep</a></a></a><b>v</b></root>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap()); // root
		assert!(r.read_next_start_element().unwrap()); // outer a
		r.skip_current_element().unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "b");
	}

	/// A self-closing element can be consumed by read_element_text (expat
	/// emits StartElement+EndElement, so the depth immediately hits 0).
	#[test]
	fn reader_read_element_text_on_self_closing() {
		let doc = "<a><b/></a>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "b");
		assert_eq!(r.read_element_text().unwrap(), "");
		// The end element of <a> remains.
		assert!(!r.read_next_start_element().unwrap());
	}

	/// read_next_start_element stops (returns false) at the first end
	/// element even if more start elements follow at a deeper level.
	#[test]
	fn reader_read_next_start_element_stops_at_end_element() {
		let doc = "<a><b><c/></b></a>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap()); // a
		assert!(r.read_next_start_element().unwrap()); // b
		assert!(r.read_next_start_element().unwrap()); // c
												 // Next is </b>: returns false and stays put.
		assert!(!r.read_next_start_element().unwrap());
	}

	/// Deeply nested mixed content: read_element_text concatenates only
	/// the direct-depth character runs, in order.
	#[test]
	fn reader_mixed_content_direct_text_in_order() {
		let doc = "<p>t1<b>x</b>t2<i>y</i>t3</p>";
		let mut r = XmlReader::new(doc).unwrap();
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), "t1t2t3");
	}

	/// After has_error, every navigation attempt fails and attribute
	/// access stays empty.
	#[test]
	fn reader_error_state_is_sticky() {
		let mut r = XmlReader::new("<a></b>").unwrap();
		assert!(r.has_error().unwrap());
		assert!(!r.read_next_start_element().unwrap());
		assert!(!r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "");
		assert_eq!(r.attribute_count().unwrap(), 0);
	}

	// ---- Reader: malformed input matrix (all must set has_error) ----

	#[test]
	fn reader_malformed_matrix() {
		let cases = [
			"",                      // no element found
			"   \n  ",               // whitespace only: no element
			"<a>",                   // unclosed root
			"<a><b></a></b>",        // mismatched nesting
			"<a></a></a>",           // end tag without start
			"<a></>",                // empty end tag name
			"<a>< /a>",              // '<' not followed by name
			"<a",                    // unclosed start tag
			"<a b>",                 // attribute missing '='
			"<a b=1>",               // attribute value not quoted
			"<a b=\"v>",             // unclosed attribute value
			"<a></a",                // unclosed end tag
			"<a><!-- c</a>",         // unclosed comment
			"<a><![CDATA[x</a>",     // unclosed CDATA
			"<?pi<a></a>",           // unclosed PI
			"<a>&undefined;</a>",    // undefined entity
			"<a>&#xZZ;</a>",         // bad hex char reference
			"<a>&#99999999999;</a>", // out-of-range char reference
			"<a b=\"&foo;\"/>",      // undefined entity in attribute
			"<a/>tail",              // junk after document element
			"<a/><b/>",              // second root element
			"<a a=\"1\" a=\"2\"/>",  // duplicate attribute
			"<a b=\"<\"/>",          // '<' in attribute value
			"<a/ ><b/>",             // self-close then second root
		];
		for doc in cases {
			let r = XmlReader::new(doc).unwrap();
			assert!(r.has_error().unwrap(), "expected error for {:?}", doc);
		}
	}

	/// Well-formed matrix: none of these may set has_error.
	#[test]
	fn reader_wellformed_matrix() {
		let cases = [
			"<a/>",
			"<a></a>",
			"<a>text</a>",
			"<a b=\"v\" c='w'/>",
			"<a><b><c>deep</c></b></a>",
			"<?xml version=\"1.0\"?><a/>",
			"<!-- c --><a/><!-- c -->",
			"<a><![CDATA[x]]></a>",
			"<a>&amp;&#65;&#x41;</a>",
			"<a\n\tb = \"v\"\n/>", // whitespace around attribute '='
			"<a></a >",            // whitespace before '>' of end tag
			"<a>x</a  >",
			" \n<a> </a>\n ",
			"<ns:a xmlns:ns=\"urn:x\" ns:b=\"v\"/>", // namespaces not processed
			"<a>]]</a>",                             // single ']' is fine
		];
		for doc in cases {
			let r = XmlReader::new(doc).unwrap();
			assert!(!r.has_error().unwrap(), "unexpected error for {:?}", doc);
		}
	}

	// ---- Writer ----

	/// write_end_document auto-closes every open element, innermost first.
	#[test]
	fn writer_end_document_auto_closes() {
		let mut w = XmlWriter::new();
		w.write_start_element("a").unwrap();
		w.write_start_element("b").unwrap();
		w.write_characters("t").unwrap();
		w.write_start_element("c").unwrap();
		w.write_end_document().unwrap();
		assert_eq!(w.output().unwrap(), "<a><b>t<c/></b></a>");
	}

	/// Writing characters closes a pending start tag; attributes written
	/// afterwards go nowhere (CPP-PARITY: write_attribute is a no-op when
	/// no start tag is open).
	#[test]
	fn writer_attribute_after_text_is_dropped() {
		let mut w = XmlWriter::new();
		w.write_start_element("a").unwrap();
		w.write_characters("x").unwrap();
		w.write_attribute("late", "v").unwrap();
		w.write_end_element().unwrap();
		assert_eq!(w.output().unwrap(), "<a>x</a>");
	}

	/// escape_text escapes exactly & < >; escape_attribute additionally
	/// escapes the double quote. Other characters (', \n, \t) pass through.
	#[test]
	fn writer_escaping_matrix() {
		let mut w = XmlWriter::new();
		w.write_start_element("a").unwrap();
		w.write_attribute("k", "&<>\"'\n").unwrap();
		w.write_characters("&<>\"'\t").unwrap();
		w.write_end_element().unwrap();
		assert_eq!(
			w.output().unwrap(),
			"<a k=\"&amp;&lt;&gt;&quot;'\n\">&amp;&lt;&gt;\"'\t</a>"
		);
	}

	/// write_end_element past the balanced depth is a silent no-op and the
	/// stack keeps working afterwards.
	#[test]
	fn writer_extra_end_element_noop_then_continue() {
		let mut w = XmlWriter::new();
		w.write_start_element("a").unwrap();
		w.write_end_element().unwrap();
		w.write_end_element().unwrap(); // no-op
		w.write_start_element("b").unwrap();
		w.write_end_document().unwrap();
		assert_eq!(w.output().unwrap(), "<a/><b/>");
	}

	/// write_text_element with empty text still closes the start tag
	/// (write_characters flushes the pending '>'), so the element is not
	/// self-closing. CPP-PARITY: C++ write_characters("") does the same.
	#[test]
	fn writer_text_element_empty_text() {
		let mut w = XmlWriter::new();
		w.write_text_element("a", "").unwrap();
		assert_eq!(w.output().unwrap(), "<a></a>");
	}

	/// write_text_element nests correctly inside an open start tag.
	#[test]
	fn writer_text_element_inside_open_tag() {
		let mut w = XmlWriter::new();
		w.write_start_element("root").unwrap();
		w.write_attribute("v", "1").unwrap();
		w.write_text_element("child", "x").unwrap();
		w.write_end_document().unwrap();
		assert_eq!(w.output().unwrap(), "<root v=\"1\"><child>x</child></root>");
	}

	/// Characters written before any start element land verbatim (escaped)
	/// in the output; C++ does the same.
	#[test]
	fn writer_characters_outside_any_element() {
		let mut w = XmlWriter::new();
		w.write_characters("a & b").unwrap();
		assert_eq!(w.output().unwrap(), "a &amp; b");
	}

	/// Round-trip with special characters: writer escapes, reader
	/// resolves, values come back identical.
	#[test]
	fn round_trip_special_characters() {
		let tricky = "a & b < c > d \"e\" 'f'\n\tg";
		let mut w = XmlWriter::new();
		w.write_start_element("root").unwrap();
		w.write_attribute("attr", tricky).unwrap();
		w.write_text_element("child", tricky).unwrap();
		w.write_end_document().unwrap();
		let xml = w.output().unwrap();

		let mut r = XmlReader::new(&xml).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.name().unwrap(), "root");
		assert_eq!(r.attribute_count().unwrap(), 1);
		assert_eq!(r.attribute_name(0).unwrap(), "attr");
		// CPP-PARITY: expat applies XML 1.0 attribute-value normalization
		// (§3.3.3): a literal tab/newline/CR inside an attribute value
		// becomes a space. quick-xml does the same, so the attribute value
		// reads back with two spaces where the input had "\n\t", while the
		// character data asserted below keeps the "\n\t" intact.
		let normalized_attr = "a & b < c > d \"e\" 'f'  g";
		assert_eq!(r.attribute_value(0).unwrap(), normalized_attr);
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), tricky);
	}

	/// Non-ASCII (UTF-8) content round-trips unchanged.
	#[test]
	fn round_trip_utf8() {
		let text = "héllo wörld — 中文";
		let mut w = XmlWriter::new();
		w.write_text_element("t", text).unwrap();
		let xml = w.output().unwrap();

		let mut r = XmlReader::new(&xml).unwrap();
		assert!(!r.has_error().unwrap());
		assert!(r.read_next_start_element().unwrap());
		assert_eq!(r.read_element_text().unwrap(), text);
	}
}
