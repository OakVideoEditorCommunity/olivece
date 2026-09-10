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

//! FCPXML (Final Cut Pro X `.fcpxml`) interchange support for the oakotio
//! binding.
//!
//! This module reads and writes the FCPXML document format produced and
//! consumed by Final Cut Pro (FCP X, version 1.10 of the format) and, with
//! some tolerance, by DaVinci Resolve and other NLEs. It maps the FCPXML
//! structure onto the same [`crate::model`] types used for OpenTimelineIO
//! JSON, so an importer/exporter can offer `.fcpxml` alongside `.otio` with
//! a single object model:
//!
//! | FCPXML                                  | OTIO model              |
//! |-----------------------------------------|-------------------------|
//! | `<resources>`/`<format>`                | frame rate (via `RationalTime::rate`) |
//! | `<resources>`/`<asset>`                 | `ExternalReference` (target_url + available_range) |
//! | `<library>`/`<event>`/`<project>`       | timeline metadata + name |
//! | `<sequence>`                            | `Timeline`              |
//! | `<spine>`                               | `Track` (kind "Video")  |
//! | secondary `<video>` / `<audio>`         | `Track` (kind "Video"/"Audio") |
//! | `<asset-clip>`                          | `Clip`                  |
//! | `<gap>`                                 | `Gap`                   |
//! | `<transition>`                          | `Transition` (in_offset == out_offset == duration/2) |
//!
//! Time values ("100/3000s", "0s", "1001/30000s", ...) are rational
//! seconds; they are converted to/from `RationalTime` through
//! `oak_core::Rational` with exact rational arithmetic, so NTSC rates
//! (30000/1001, 60000/1001, ...) stay frame accurate in both directions.
//!
//! The reader is deliberately lenient: unknown elements and attributes are
//! skipped with a log line (`eprintln!`) instead of failing, so real-world
//! documents from FCP X / DaVinci Resolve import without crashing. Unknown
//! *timed* blocks inside a track are imported as `Gap`s that preserve their
//! timing. Structural errors (non-FCPXML root, unsupported version, missing
//! format resources) are reported as [`FcpxmlError`].
//!
//! The writer emits FCPXML 1.10 with 2-space indentation (the FCP X
//! convention). Time values are written as reduced rational seconds
//! ("48s", "1/30s", "1001/30000s") rather than FCP X's scaled
//! "value/rate*100" spellings; the two forms are semantically identical.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use oak_core::Rational;
use quick_xml::escape;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Reader;
use quick_xml::Writer;

use crate::model::{
	Clip, Composable, ExternalReference, Gap, MediaReference, MissingReference, RationalTime,
	TimeRange, Timeline, Track, Transition,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the FCPXML layer.
#[derive(Debug)]
pub enum FcpxmlError {
	/// The document could not be parsed as XML (malformed markup, encoding
	/// errors, attribute errors).
	Xml(String),
	/// The document is structurally not a usable FCPXML document (wrong
	/// root element, missing attributes, unknown resource references, bad
	/// time values).
	Malformed(String),
	/// The document declares an FCPXML version this crate does not support.
	UnsupportedVersion(String),
	/// The underlying file could not be read or written.
	Io(std::io::Error),
}

impl fmt::Display for FcpxmlError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			FcpxmlError::Xml(e) => write!(f, "FCPXML XML error: {e}"),
			FcpxmlError::Malformed(e) => write!(f, "FCPXML document error: {e}"),
			FcpxmlError::UnsupportedVersion(e) => write!(f, "FCPXML version error: {e}"),
			FcpxmlError::Io(e) => write!(f, "FCPXML file error: {e}"),
		}
	}
}

impl std::error::Error for FcpxmlError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			FcpxmlError::Io(e) => Some(e),
			_ => None,
		}
	}
}

impl From<std::io::Error> for FcpxmlError {
	fn from(e: std::io::Error) -> FcpxmlError {
		FcpxmlError::Io(e)
	}
}

impl From<quick_xml::Error> for FcpxmlError {
	fn from(e: quick_xml::Error) -> FcpxmlError {
		FcpxmlError::Xml(e.to_string())
	}
}

/// Convenience alias used by the FCPXML API.
pub type Result<T> = std::result::Result<T, FcpxmlError>;

// ---------------------------------------------------------------------------
// Time values
// ---------------------------------------------------------------------------

/// Parse an FCPXML time value into a rational number of seconds.
///
/// Accepts the canonical "value/rate s" form ("100/3000s", "1001/30000s"),
/// plain integer seconds ("48s", "0s"), a bare rational without the unit
/// ("1/24"), and decimal seconds ("1.5s"). Frame-count values ("24f")
/// cannot be converted without a rate and are rejected.
fn parse_time(s: &str) -> Result<Rational> {
	let s = s.trim();
	let s = s.strip_suffix('s').map(str::trim).unwrap_or(s);
	if s.ends_with('f') {
		return Err(FcpxmlError::Malformed(format!(
			"frame-count time value \"{s}f\" cannot be converted without a rate"
		)));
	}
	if let Some((num, den)) = s.split_once('/') {
		let num = num
			.trim()
			.parse::<i64>()
			.map_err(|_| FcpxmlError::Malformed(format!("invalid time value \"{s}\"")))?;
		let den = den
			.trim()
			.parse::<i64>()
			.map_err(|_| FcpxmlError::Malformed(format!("invalid time value \"{s}\"")))?;
		if den == 0 {
			return Err(FcpxmlError::Malformed(format!(
				"time value \"{s}\" has a zero denominator"
			)));
		}
		Ok(Rational::new(num, den))
	} else if let Ok(whole) = s.parse::<i64>() {
		Ok(Rational::new(whole, 1))
	} else if let Ok(frac) = s.parse::<f64>() {
		if !frac.is_finite() {
			return Err(FcpxmlError::Malformed(format!(
				"non-finite time value \"{s}\""
			)));
		}
		Ok(Rational::from_double(frac))
	} else {
		Err(FcpxmlError::Malformed(format!(
			"invalid time value \"{s}\""
		)))
	}
}

/// Format a rational number of seconds the way this crate writes FCPXML:
/// "0s", "48s" for whole seconds, "1001/30000s" otherwise (reduced form).
fn format_time(r: Rational) -> String {
	let num = r.numerator();
	let den = r.denominator();
	if num == 0 {
		"0s".to_string()
	} else if den == 1 {
		format!("{num}s")
	} else {
		format!("{num}/{den}s")
	}
}

/// Seconds -> `RationalTime` at a given frame rate, using exact rational
/// arithmetic so frame counts stay integer for NTSC rates.
fn time_at_rate(seconds: Rational, rate: Rational) -> RationalTime {
	let frames = seconds * rate;
	RationalTime::new(frames.to_f64(), rate.to_f64())
}

/// `RationalTime` -> exact rational seconds (`value / rate`), recovered
/// with the continued-fraction `from_double` so "1001/30000s" round-trips.
fn seconds_of(time: RationalTime) -> Rational {
	Rational::from_double(time.value() / time.rate())
}

/// Frames per second from a format's `frameDuration` ("1001/30000s" ->
/// 30000/1001 fps). `None` for a zero or null frame duration.
fn rate_of(frame_duration: Rational) -> Option<Rational> {
	if frame_duration.numerator() == 0 || frame_duration.denominator() == 0 {
		None
	} else {
		Some(Rational::new(
			frame_duration.denominator(),
			frame_duration.numerator(),
		))
	}
}

/// Frames per second -> `frameDuration` seconds (the inverse of
/// [`rate_of`]).
fn frame_duration_of(rate: Rational) -> Rational {
	Rational::from_double(1.0 / rate.to_f64())
}

/// The FCPXML version this crate writes. FCP X reads it and it supports
/// the features used (a `name` attribute on `<sequence>`, `format`
/// attributes on `<asset-clip>`).
const FCPXML_VERSION: &str = "1.10";

/// The highest FCPXML version this crate reads.
const FCPXML_MAX_MINOR: u32 = 11;

/// Reject documents declaring a version this crate cannot read. The
/// version is parsed as "major.minor" (1.0 - 1.11); a plain float
/// comparison would misjudge "1.9" against "1.11".
fn check_version(version: &str) -> Result<()> {
	let parts: Vec<&str> = version.trim().split('.').collect();
	let parse = |s: &&str| s.parse::<u32>().ok();
	let (Some(major), Some(minor)) = (parts.first().and_then(parse), parts.get(1).and_then(parse))
	else {
		return Err(FcpxmlError::UnsupportedVersion(format!(
			"unparseable FCPXML version \"{version}\""
		)));
	};
	if parts.len() > 2 || major != 1 || minor > FCPXML_MAX_MINOR {
		return Err(FcpxmlError::UnsupportedVersion(format!(
			"unsupported FCPXML version \"{version}\" (supported: 1.0 - 1.11)"
		)));
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Minimal XML tree
// ---------------------------------------------------------------------------

/// A lightweight XML tree built from the quick-xml event stream. Only the
/// structure FCPXML needs is kept: element name, attributes, child
/// elements and (trimmed) text content.
#[derive(Clone, Debug, Default)]
struct Node {
	name: String,
	attrs: Vec<(String, String)>,
	children: Vec<Node>,
	text: String,
}

impl Node {
	/// The first attribute with the given name.
	fn attr(&self, key: &str) -> Option<&str> {
		self.attrs
			.iter()
			.find(|(k, _)| k == key)
			.map(|(_, v)| v.as_str())
	}

	/// The first child element with the given (local) name.
	fn child(&self, name: &str) -> Option<&Node> {
		self.children.iter().find(|c| c.name == name)
	}
}

/// Build a [`Node`] from a start/empty event (local name, unescaped
/// attributes).
fn node_from_start(e: &BytesStart) -> Result<Node> {
	let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
	let mut attrs = Vec::new();
	for attr in e.attributes() {
		let attr =
			attr.map_err(|err| FcpxmlError::Xml(format!("invalid attribute in <{name}>: {err}")))?;
		let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
		let value = attr
			.normalized_value(quick_xml::XmlVersion::Implicit1_0)
			.map_err(|err| FcpxmlError::Xml(format!("invalid value for attribute {key}: {err}")))?
			.into_owned();
		attrs.push((key, value));
	}
	Ok(Node {
		name,
		attrs,
		children: Vec::new(),
		text: String::new(),
	})
}

/// Attach `node` as a child of the current top of `stack` (no-op for the
/// document root).
fn attach(stack: &mut [Node], node: Node) {
	if let Some(parent) = stack.last_mut() {
		parent.children.push(node);
	}
}

/// Parse `text` into a single XML tree (the document root). Comments,
/// processing instructions, the XML declaration and the DOCTYPE are
/// skipped; mismatched or unclosed tags are errors.
fn parse_xml(text: &str) -> Result<Node> {
	let mut reader = Reader::from_str(text);
	reader.config_mut().trim_text(true);
	let mut stack: Vec<Node> = Vec::new();
	let mut root: Option<Node> = None;
	loop {
		match reader.read_event()? {
			Event::Eof => break,
			Event::Start(e) => stack.push(node_from_start(&e)?),
			Event::Empty(e) => {
				let node = node_from_start(&e)?;
				attach(&mut stack, node);
			}
			Event::End(e) => {
				let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
				let node = stack.pop().ok_or_else(|| {
					FcpxmlError::Malformed(format!("unexpected closing tag </{name}>"))
				})?;
				if node.name != name {
					return Err(FcpxmlError::Malformed(format!(
						"mismatched closing tag: expected </{}>, found </{name}>",
						node.name
					)));
				}
				if stack.is_empty() {
					// The document root.
					root = Some(node);
				} else {
					attach(&mut stack, node);
				}
			}
			Event::Text(t) => {
				// The reader resolves predefined entities; decode the
				// (UTF-8 or declared-encoding) text content.
				let text = t
					.decode()
					.map_err(|e| FcpxmlError::Xml(format!("invalid text content: {e}")))?;
				if let Some(top) = stack.last_mut() {
					top.text.push_str(&text);
				}
			}
			Event::CData(t) => {
				let text = String::from_utf8_lossy(t.as_ref()).into_owned();
				if let Some(top) = stack.last_mut() {
					top.text.push_str(&text);
				}
			}
			_ => {}
		}
	}
	match root {
		Some(root) if stack.is_empty() => Ok(root),
		_ => Err(FcpxmlError::Malformed(format!(
			"document ended with {} unclosed element(s)",
			stack.len()
		))),
	}
}

// ---------------------------------------------------------------------------
// Reader: resources
// ---------------------------------------------------------------------------

/// A parsed `<format>` resource.
#[derive(Clone, Debug)]
struct FormatInfo {
	/// `frameDuration` in seconds ("1001/30000s").
	frame_duration: Rational,
}

/// A parsed `<asset>` resource.
#[derive(Clone, Debug)]
struct AssetInfo {
	/// The asset `src` (media URL).
	src: String,
	/// The referenced format id, if any.
	format: Option<String>,
	/// The asset `duration` in seconds.
	duration: Rational,
}

/// Parse a `<format>` resource. Bad formats are logged and skipped so one
/// broken resource cannot sink the whole document.
fn parse_format(node: &Node, formats: &mut HashMap<String, FormatInfo>) {
	let Some(id) = node.attr("id") else {
		eprintln!("oakotio fcpxml: skipping <format> without an id");
		return;
	};
	let frame_duration = match node.attr("frameDuration") {
		Some(fd) => match parse_time(fd) {
			Ok(fd) => fd,
			Err(e) => {
				eprintln!("oakotio fcpxml: skipping <format id=\"{id}\">: {e}");
				return;
			}
		},
		None => {
			eprintln!("oakotio fcpxml: skipping <format id=\"{id}\"> without frameDuration");
			return;
		}
	};
	formats.insert(id.to_string(), FormatInfo { frame_duration });
}

/// Parse an `<asset>` resource. Unreadable durations fall back to 0s.
fn parse_asset(node: &Node, assets: &mut HashMap<String, AssetInfo>) {
	let Some(id) = node.attr("id") else {
		eprintln!("oakotio fcpxml: skipping <asset> without an id");
		return;
	};
	let duration = match node.attr("duration") {
		Some(d) => parse_time(d).unwrap_or_else(|e| {
			eprintln!("oakotio fcpxml: <asset id=\"{id}\"> {e}; using 0s");
			Rational::new(0, 1)
		}),
		None => Rational::new(0, 1),
	};
	assets.insert(
		id.to_string(),
		AssetInfo {
			src: node.attr("src").unwrap_or("").to_string(),
			format: node.attr("format").map(str::to_string),
			duration,
		},
	);
}

// ---------------------------------------------------------------------------
// Reader: sequences and blocks
// ---------------------------------------------------------------------------

/// The rate of an `<asset-clip>`: its own `format` attribute, else the
/// referenced asset's format, else the sequence rate.
fn clip_rate_of(
	node: &Node,
	formats: &HashMap<String, FormatInfo>,
	assets: &HashMap<String, AssetInfo>,
	fallback: Rational,
) -> Rational {
	if let Some(fmt_id) = node.attr("format") {
		if let Some(fmt) = formats.get(fmt_id) {
			if let Some(rate) = rate_of(fmt.frame_duration) {
				return rate;
			}
			eprintln!("oakotio fcpxml: <asset-clip> format \"{fmt_id}\" has a zero frame duration");
		} else {
			eprintln!("oakotio fcpxml: <asset-clip> references unknown format \"{fmt_id}\"");
		}
	}
	if let Some(ref_id) = node.attr("ref") {
		if let Some(asset) = assets.get(ref_id) {
			if let Some(fmt_id) = asset.format.as_deref() {
				if let Some(fmt) = formats.get(fmt_id) {
					if let Some(rate) = rate_of(fmt.frame_duration) {
						return rate;
					}
				}
			}
		}
	}
	fallback
}

/// The rate of an `<asset>` (its format resource), falling back to the
/// sequence rate.
fn asset_rate_of(
	asset: &AssetInfo,
	formats: &HashMap<String, FormatInfo>,
	fallback: Rational,
) -> Rational {
	if let Some(fmt_id) = asset.format.as_deref() {
		if let Some(fmt) = formats.get(fmt_id) {
			if let Some(rate) = rate_of(fmt.frame_duration) {
				return rate;
			}
			eprintln!("oakotio fcpxml: asset format \"{fmt_id}\" has a zero frame duration");
		} else {
			eprintln!("oakotio fcpxml: asset references unknown format \"{fmt_id}\"");
		}
	}
	fallback
}

/// Parse one timed block inside a track container (spine / video / audio).
///
/// `pos` is the accumulated end of the previous blocks, used as the
/// element's offset when the element carries no explicit `offset`
/// attribute. Returns `None` for elements that should be skipped without
/// occupying timeline space.
fn parse_block(
	node: &Node,
	name: &str,
	seq_rate: Rational,
	formats: &HashMap<String, FormatInfo>,
	assets: &HashMap<String, AssetInfo>,
	pos: &mut Rational,
) -> Result<Option<Composable>> {
	let offset = match node.attr("offset") {
		Some(s) => parse_time(s)?,
		None => *pos,
	};
	match name {
		"asset-clip" | "clip" => {
			let duration = match node.attr("duration") {
				Some(d) => parse_time(d)?,
				None => {
					return Err(FcpxmlError::Malformed(format!(
						"<{name}> is missing its duration attribute"
					)));
				}
			};
			let start = match node.attr("start") {
				Some(s) => parse_time(s)?,
				None => Rational::new(0, 1),
			};
			let clip_rate = clip_rate_of(node, formats, assets, seq_rate);

			let mut clip = Clip::new(node.attr("name").unwrap_or(""));
			clip.set_source_range(TimeRange::new(
				time_at_rate(start, clip_rate),
				time_at_rate(duration, clip_rate),
			));
			if let Some(enabled) = node.attr("enabled") {
				clip.set_enabled(enabled != "0");
			}

			let reference = match node.attr("ref") {
				Some(ref_id) => match assets.get(ref_id) {
					Some(asset) => {
						let asset_rate = asset_rate_of(asset, formats, seq_rate);
						MediaReference::ExternalReference(ExternalReference::new(
							asset.src.clone(),
							Some(TimeRange::new(
								time_at_rate(Rational::new(0, 1), asset_rate),
								time_at_rate(asset.duration, asset_rate),
							)),
						))
					}
					None => {
						eprintln!(
							"oakotio fcpxml: <{name}> references unknown asset \"{ref_id}\"; using a MissingReference"
						);
						MediaReference::MissingReference(MissingReference::new())
					}
				},
				None => {
					eprintln!("oakotio fcpxml: <{name}> has no ref; using a MissingReference");
					MediaReference::MissingReference(MissingReference::new())
				}
			};
			clip.set_media_reference(reference);

			*pos = offset + duration;
			Ok(Some(Composable::Clip(clip)))
		}
		"gap" => {
			let duration = match node.attr("duration") {
				Some(d) => parse_time(d)?,
				None => {
					eprintln!("oakotio fcpxml: <gap> without duration treated as empty");
					Rational::new(0, 1)
				}
			};
			let range = TimeRange::new(
				time_at_rate(offset, seq_rate),
				time_at_rate(duration, seq_rate),
			);
			*pos = offset + duration;
			Ok(Some(Composable::Gap(Gap::new(
				range,
				node.attr("name").unwrap_or(""),
			))))
		}
		"transition" => {
			let duration = match node.attr("duration") {
				Some(d) => parse_time(d)?,
				None => {
					return Err(FcpxmlError::Malformed(
						"<transition> is missing its duration attribute".to_string(),
					));
				}
			};
			// FCP X centers its transitions: each half of the duration
			// reaches into the adjacent clip, so in_offset == out_offset
			// == duration / 2.
			let half = Rational::new(duration.numerator(), duration.denominator() * 2);
			let mut transition = Transition::new(node.attr("name").unwrap_or(""));
			transition.set_in_offset(time_at_rate(half, seq_rate));
			transition.set_out_offset(time_at_rate(half, seq_rate));
			*pos = offset + duration;
			Ok(Some(Composable::Transition(transition)))
		}
		// FCP X containers this crate does not model. Their timing is
		// preserved as a gap so the timeline layout stays intact.
		"sync-clip" | "title" | "generator" | "caption" | "multicam" | "titles" => {
			let duration = match node.attr("duration") {
				Some(d) => parse_time(d)?,
				None => Rational::new(0, 1),
			};
			eprintln!(
				"oakotio fcpxml: <{name}> is not mapped to an OTIO block; importing its timing as a gap"
			);
			let range = TimeRange::new(
				time_at_rate(offset, seq_rate),
				time_at_rate(duration, seq_rate),
			);
			*pos = offset + duration;
			Ok(Some(Composable::Gap(Gap::new(range, ""))))
		}
		// Metadata-ish children that occasionally sit inside a track
		// container: nothing to do.
		"marker" | "metadata" | "param" | "note" | "analysis" | "media" | "text" | "text-style" => {
			Ok(None)
		}
		other => {
			eprintln!("oakotio fcpxml: ignoring unknown track element <{other}>");
			Ok(None)
		}
	}
}

/// Parse a track container (`spine`, `<video>` or `<audio>`) into a new
/// [`Track`] pushed onto `tracks`. Nested `<video>`/`<audio>` elements
/// become additional tracks, matching FCP X's multi-lane layout.
fn parse_container(
	kind: &str,
	node: &Node,
	seq_rate: Rational,
	formats: &HashMap<String, FormatInfo>,
	assets: &HashMap<String, AssetInfo>,
	tracks: &mut Vec<Track>,
) -> Result<()> {
	let mut track = Track::new(kind);
	let mut pos = Rational::new(0, 1);
	for child in &node.children {
		match child.name.as_str() {
			"video" => parse_container("Video", child, seq_rate, formats, assets, tracks)?,
			"audio" => parse_container("Audio", child, seq_rate, formats, assets, tracks)?,
			other => {
				if let Some(block) = parse_block(child, other, seq_rate, formats, assets, &mut pos)?
				{
					track.append_child(block);
				}
			}
		}
	}
	tracks.push(track);
	Ok(())
}

/// Store interchange hints (source version, tcFormat, event/project
/// names, audio layout/rate) under the nested "fcpxml" metadata key so a
/// later export can reproduce them.
fn set_fcpxml_metadata(
	timeline: &mut Timeline,
	sequence: &Node,
	event: &Node,
	project: &Node,
	version: &str,
) {
	let mut fcpx = serde_json::Map::new();
	fcpx.insert("version".into(), serde_json::Value::String(version.into()));
	if let Some(tc) = sequence.attr("tcFormat") {
		fcpx.insert("tcFormat".into(), serde_json::Value::String(tc.into()));
	}
	if let Some(layout) = sequence.attr("audioLayout") {
		fcpx.insert(
			"audioLayout".into(),
			serde_json::Value::String(layout.into()),
		);
	}
	if let Some(rate) = sequence.attr("audioRate") {
		fcpx.insert("audioRate".into(), serde_json::Value::String(rate.into()));
	}
	if let Some(e) = event.attr("name") {
		fcpx.insert("event".into(), serde_json::Value::String(e.into()));
	}
	if let Some(p) = project.attr("name") {
		fcpx.insert("project".into(), serde_json::Value::String(p.into()));
	}
	timeline
		.metadata_mut()
		.insert("fcpxml".into(), serde_json::Value::Object(fcpx));
}

/// Parse a `<sequence>` element into a [`Timeline`].
fn parse_sequence(
	node: &Node,
	event: &Node,
	project: &Node,
	version: &str,
	formats: &HashMap<String, FormatInfo>,
	assets: &HashMap<String, AssetInfo>,
) -> Result<Timeline> {
	let format_id = node.attr("format").ok_or_else(|| {
		FcpxmlError::Malformed("<sequence> is missing its format reference".into())
	})?;
	let seq_format = formats.get(format_id).ok_or_else(|| {
		FcpxmlError::Malformed(format!(
			"sequence references unknown format resource \"{format_id}\""
		))
	})?;
	let rate = rate_of(seq_format.frame_duration).ok_or_else(|| {
		FcpxmlError::Malformed(format!(
			"format resource \"{format_id}\" has a zero frame duration"
		))
	})?;

	let name = node
		.attr("name")
		.filter(|s| !s.is_empty())
		.or_else(|| project.attr("name").filter(|s| !s.is_empty()))
		.or_else(|| event.attr("name").filter(|s| !s.is_empty()))
		.unwrap_or("")
		.to_string();
	let mut timeline = Timeline::new(name);

	if let Some(tc_start) = node.attr("tcStart") {
		let seconds = parse_time(tc_start)?;
		timeline.set_global_start_time(time_at_rate(seconds, rate));
	}

	set_fcpxml_metadata(&mut timeline, node, event, project, version);

	let mut tracks = Vec::new();
	for child in &node.children {
		match child.name.as_str() {
			"spine" => parse_container("Video", child, rate, formats, assets, &mut tracks)?,
			"video" => parse_container("Video", child, rate, formats, assets, &mut tracks)?,
			"audio" => parse_container("Audio", child, rate, formats, assets, &mut tracks)?,
			other => eprintln!("oakotio fcpxml: ignoring unknown <sequence> child <{other}>"),
		}
	}
	for track in tracks {
		timeline.tracks_mut().append_child(Composable::Track(track));
	}
	Ok(timeline)
}

/// Parse an FCPXML document into one [`Timeline`] per `<sequence>`.
///
/// # Errors
///
/// - `FcpxmlError::Xml` for malformed XML markup.
/// - `FcpxmlError::Malformed` when the root is not `<fcpxml>`, a required
///   attribute is missing, or a sequence references a format resource that
///   does not exist.
/// - `FcpxmlError::UnsupportedVersion` for a missing or out-of-range
///   `version` attribute.
pub fn from_fcpxml_string(text: &str) -> Result<Vec<Timeline>> {
	let root = parse_xml(text)?;
	if root.name != "fcpxml" {
		return Err(FcpxmlError::Malformed(format!(
			"document root is <{}>, expected <fcpxml>",
			root.name
		)));
	}
	let version = root.attr("version").ok_or_else(|| {
		FcpxmlError::UnsupportedVersion("document has no version attribute".into())
	})?;
	check_version(version)?;

	let mut formats = HashMap::new();
	let mut assets = HashMap::new();
	if let Some(resources) = root.child("resources") {
		for child in &resources.children {
			match child.name.as_str() {
				"format" => parse_format(child, &mut formats),
				"asset" => parse_asset(child, &mut assets),
				other => eprintln!("oakotio fcpxml: ignoring unknown resource element <{other}>"),
			}
		}
	}

	let mut timelines = Vec::new();
	if let Some(library) = root.child("library") {
		for event in library.children.iter().filter(|c| c.name == "event") {
			for project in event.children.iter().filter(|c| c.name == "project") {
				for sequence in project.children.iter().filter(|c| c.name == "sequence") {
					timelines.push(parse_sequence(
						sequence, event, project, version, &formats, &assets,
					)?);
				}
			}
		}
	}
	Ok(timelines)
}

/// Read and parse an FCPXML document from a file.
pub fn from_fcpxml_file(path: impl AsRef<Path>) -> Result<Vec<Timeline>> {
	from_fcpxml_string(&std::fs::read_to_string(path)?)
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Default frame size used for `<format>` resources (the OTIO model does
/// not carry video dimensions; see README "Deviations").
const DEFAULT_FORMAT_WIDTH: &str = "1920";
const DEFAULT_FORMAT_HEIGHT: &str = "1080";

/// A `<format>` resource ready to be written.
struct FormatEntry {
	id: String,
	name: String,
	frame_duration: Rational,
	width: String,
	height: String,
}

/// An `<asset>` resource ready to be written.
struct AssetEntry {
	id: String,
	name: String,
	src: String,
	format_id: String,
	duration: Rational,
	has_video: bool,
	has_audio: bool,
}

/// Resources accumulated while walking the timelines being exported.
/// Formats are deduplicated by frame duration, assets by `src`.
#[derive(Default)]
struct ResourceSet {
	formats: Vec<FormatEntry>,
	assets: Vec<AssetEntry>,
	format_ids: HashMap<(i64, i64), String>,
	asset_ids: HashMap<String, String>,
	next_id: usize,
}

impl ResourceSet {
	/// The `<format>` id for a frame duration, creating the resource (and
	/// assigning an id) if it is not present yet.
	fn format_id_for(&mut self, frame_duration: Rational) -> String {
		let key = (frame_duration.numerator(), frame_duration.denominator());
		if let Some(id) = self.format_ids.get(&key) {
			return id.clone();
		}
		self.next_id += 1;
		let id = format!("r{}", self.next_id);
		let name = format_name(frame_duration);
		self.formats.push(FormatEntry {
			id: id.clone(),
			name,
			frame_duration,
			width: DEFAULT_FORMAT_WIDTH.to_string(),
			height: DEFAULT_FORMAT_HEIGHT.to_string(),
		});
		self.format_ids.insert(key, id.clone());
		id
	}

	/// The `<asset>` id for a media URL, creating the resource if it is
	/// not present yet.
	fn asset_id_for(
		&mut self,
		src: &str,
		format_id: &str,
		duration: Rational,
		has_video: bool,
		has_audio: bool,
	) -> String {
		if let Some(id) = self.asset_ids.get(src) {
			return id.clone();
		}
		self.next_id += 1;
		let id = format!("r{}", self.next_id);
		let name = Path::new(src)
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_else(|| src.to_string());
		self.assets.push(AssetEntry {
			id: id.clone(),
			name,
			src: src.to_string(),
			format_id: format_id.to_string(),
			duration,
			has_video,
			has_audio,
		});
		self.asset_ids.insert(src.to_string(), id.clone());
		id
	}

	/// Walk one timeline and register every format and asset it needs.
	/// Formats are collected in a first pass so they are written before
	/// assets (the FCP X resources ordering), with asset ids following.
	fn collect_from(&mut self, timeline: &Timeline) {
		let seq_rate = sequence_rate_of(timeline);
		self.format_id_for(frame_duration_of(seq_rate));
		for child in timeline.tracks().children() {
			let Some(track) = child.as_track() else {
				continue;
			};
			for block in track.children() {
				let Some(clip) = block.as_clip() else {
					continue;
				};
				if let Some(range) = clip.source_range() {
					self.format_id_for(frame_duration_of(Rational::from_double(
						range.duration().rate(),
					)));
				}
				if let Some(external) = clip
					.media_reference()
					.and_then(MediaReference::as_external_reference)
				{
					if let Some(range) = external.available_range() {
						self.format_id_for(frame_duration_of(Rational::from_double(
							range.duration().rate(),
						)));
					}
				}
			}
		}

		for child in timeline.tracks().children() {
			let Some(track) = child.as_track() else {
				continue;
			};
			let is_video = track.kind() == "Video";
			let is_audio = track.kind() == "Audio";
			for block in track.children() {
				let Some(clip) = block.as_clip() else {
					continue;
				};
				let clip_rate = clip
					.source_range()
					.map(|r| Rational::from_double(r.duration().rate()))
					.unwrap_or(seq_rate);
				let clip_format_id = self.format_id_for(frame_duration_of(clip_rate));
				let Some(external) = clip
					.media_reference()
					.and_then(MediaReference::as_external_reference)
				else {
					continue;
				};
				let src = external.target_url();
				if self.asset_ids.contains_key(src) {
					// Already registered from another track: union the
					// stream flags so a file used on both a video and an
					// audio track declares hasVideo AND hasAudio (the
					// first registration's track kind must not hide the
					// other stream from the importer).
					if let Some(existing) = self.assets.iter_mut().find(|a| a.src == src) {
						existing.has_video |= is_video;
						existing.has_audio |= is_audio;
					}
					continue;
				}
				let (asset_format_id, asset_duration) = match external.available_range() {
					Some(range) => {
						let rate = Rational::from_double(range.duration().rate());
						(
							self.format_id_for(frame_duration_of(rate)),
							seconds_of(range.duration()),
						)
					}
					None => {
						let duration = clip
							.source_range()
							.map(|r| seconds_of(r.duration()))
							.unwrap_or(Rational::new(0, 1));
						(clip_format_id, duration)
					}
				};
				self.asset_id_for(src, &asset_format_id, asset_duration, is_video, is_audio);
			}
		}
	}
}

/// FCP X-style format name, e.g. "FFVideoFormat1920x1080p30" or
/// "...p29_97". The name is informational; importers derive the rate from
/// `frameDuration`.
fn format_name(frame_duration: Rational) -> String {
	let rate = rate_of(frame_duration).map(Rational::to_f64).unwrap_or(0.0);
	let rate_str = if (rate - rate.round()).abs() < 1e-6 {
		format!("{}", rate.round() as i64)
	} else {
		format!("{rate:.2}").replace('.', "_")
	};
	format!(
		"FFVideoFormat{}x{}p{rate_str}",
		DEFAULT_FORMAT_WIDTH, DEFAULT_FORMAT_HEIGHT
	)
}

/// The timeline rate: the rate of the first block carrying a source
/// range, else 24 fps.
fn sequence_rate_of(timeline: &Timeline) -> Rational {
	for child in timeline.tracks().children() {
		if let Some(track) = child.as_track() {
			for block in track.children() {
				if let Some(range) = block.source_range() {
					return Rational::from_double(range.duration().rate());
				}
			}
		}
	}
	Rational::new(24, 1)
}

/// The duration of one block in seconds (transitions contribute
/// in_offset + out_offset).
fn block_duration(block: &Composable) -> Rational {
	match block {
		Composable::Clip(c) => c
			.source_range()
			.map(|r| seconds_of(r.duration()))
			.unwrap_or(Rational::new(0, 1)),
		Composable::Gap(g) => g
			.source_range()
			.map(|r| seconds_of(r.duration()))
			.unwrap_or(Rational::new(0, 1)),
		Composable::Transition(t) => seconds_of(t.in_offset()) + seconds_of(t.out_offset()),
		_ => Rational::new(0, 1),
	}
}

/// The end of one track, in seconds, walking the blocks the same way the
/// writer places them. Transitions overlap the tail of the previous block,
/// so they advance the position by their `out_offset` only (the `in_offset`
/// lies inside the previous block).
fn track_end(track: &Track) -> Rational {
	let mut pos = Rational::new(0, 1);
	for block in track.children() {
		match &**block {
			Composable::Transition(t) => pos = pos + seconds_of(t.out_offset()),
			other => pos = pos + block_duration(other),
		}
	}
	pos
}

/// The end of the longest track, in seconds (the `<sequence>`
/// `duration`).
fn timeline_duration(timeline: &Timeline) -> Rational {
	let mut max_end = Rational::new(0, 1);
	for child in timeline.tracks().children() {
		if let Some(track) = child.as_track() {
			let end = track_end(track);
			if end > max_end {
				max_end = end;
			}
		}
	}
	max_end
}

/// Push an (escaped) attribute onto a start element. quick-xml does not
/// escape attribute values itself, so the value is escaped here.
fn push_attr(start: &mut BytesStart, key: &str, value: &str) {
	let escaped = escape::escape(value).into_owned();
	start.push_attribute((key, escaped.as_str()));
}

fn write_event(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<()> {
	writer.write_event(event)?;
	Ok(())
}

/// Write the `<resources>` element.
fn write_resources(writer: &mut Writer<Vec<u8>>, resources: &ResourceSet) -> Result<()> {
	if resources.formats.is_empty() && resources.assets.is_empty() {
		return Ok(());
	}
	write_event(writer, Event::Start(BytesStart::new("resources")))?;
	for format in &resources.formats {
		let mut el = BytesStart::new("format");
		push_attr(&mut el, "id", &format.id);
		push_attr(&mut el, "name", &format.name);
		push_attr(
			&mut el,
			"frameDuration",
			&format_time(format.frame_duration),
		);
		push_attr(&mut el, "width", &format.width);
		push_attr(&mut el, "height", &format.height);
		write_event(writer, Event::Empty(el))?;
	}
	for asset in &resources.assets {
		let mut el = BytesStart::new("asset");
		push_attr(&mut el, "id", &asset.id);
		push_attr(&mut el, "name", &asset.name);
		push_attr(&mut el, "src", &asset.src);
		push_attr(&mut el, "format", &asset.format_id);
		push_attr(&mut el, "duration", &format_time(asset.duration));
		push_attr(&mut el, "hasVideo", if asset.has_video { "1" } else { "0" });
		push_attr(&mut el, "hasAudio", if asset.has_audio { "1" } else { "0" });
		write_event(writer, Event::Empty(el))?;
	}
	write_event(writer, Event::End(BytesEnd::new("resources")))?;
	Ok(())
}

/// Read a string out of the "fcpxml" metadata object.
fn metadata_string(timeline: &Timeline, key: &str) -> Option<String> {
	timeline
		.metadata()
		.get("fcpxml")
		.and_then(|m| m.get(key))
		.and_then(|v| v.as_str())
		.map(str::to_string)
}

/// Write the blocks of one track into its FCPXML container.
fn write_blocks(
	writer: &mut Writer<Vec<u8>>,
	track: &Track,
	seq_rate: Rational,
	resources: &mut ResourceSet,
	pos: &mut Rational,
) -> Result<()> {
	for block in track.children() {
		match &**block {
			Composable::Clip(clip) => {
				let (start, duration, clip_rate) = match clip.source_range() {
					Some(range) => (
						seconds_of(range.start_time()),
						seconds_of(range.duration()),
						Rational::from_double(range.duration().rate()),
					),
					None => (Rational::new(0, 1), Rational::new(0, 1), seq_rate),
				};
				let mut el = BytesStart::new("asset-clip");
				push_attr(&mut el, "name", clip.name());
				match clip.media_reference() {
					Some(MediaReference::ExternalReference(external)) => {
						let src = external.target_url();
						match resources.asset_ids.get(src) {
							Some(asset_id) => push_attr(&mut el, "ref", asset_id),
							None => eprintln!(
								"oakotio fcpxml: no asset registered for \"{src}\"; writing clip \"{}\" without a ref",
								clip.name()
							),
						}
					}
					_ => eprintln!(
						"oakotio fcpxml: clip \"{}\" has no external media reference; writing it without a ref",
						clip.name()
					),
				}
				push_attr(&mut el, "offset", &format_time(*pos));
				push_attr(&mut el, "duration", &format_time(duration));
				push_attr(&mut el, "start", &format_time(start));
				if clip_rate != seq_rate {
					let format_id = resources.format_id_for(frame_duration_of(clip_rate));
					push_attr(&mut el, "format", &format_id);
				}
				if !clip.enabled() {
					push_attr(&mut el, "enabled", "0");
				}
				write_event(writer, Event::Empty(el))?;
				*pos = *pos + duration;
			}
			Composable::Gap(gap) => {
				let duration = gap
					.source_range()
					.map(|r| seconds_of(r.duration()))
					.unwrap_or(Rational::new(0, 1));
				let mut el = BytesStart::new("gap");
				push_attr(&mut el, "name", gap.name());
				push_attr(&mut el, "offset", &format_time(*pos));
				push_attr(&mut el, "duration", &format_time(duration));
				write_event(writer, Event::Empty(el))?;
				*pos = *pos + duration;
			}
			Composable::Transition(transition) => {
				let in_s = seconds_of(transition.in_offset());
				let out_s = seconds_of(transition.out_offset());
				let duration = in_s + out_s;
				// The transition starts in_offset before the end of the
				// previous block (FCP X's centered transition layout).
				let offset = *pos - in_s;
				let mut el = BytesStart::new("transition");
				push_attr(&mut el, "name", transition.name());
				push_attr(&mut el, "offset", &format_time(offset));
				push_attr(&mut el, "duration", &format_time(duration));
				write_event(writer, Event::Empty(el))?;
				*pos = offset + duration;
			}
			other => eprintln!(
				"oakotio fcpxml: skipping nested {} block on track \"{}\"",
				other.schema_name(),
				track.kind()
			),
		}
	}
	Ok(())
}

/// Write one `<sequence>` element (and its tracks) for a timeline.
fn write_sequence(
	writer: &mut Writer<Vec<u8>>,
	timeline: &Timeline,
	resources: &mut ResourceSet,
) -> Result<()> {
	let mut video_tracks = Vec::new();
	let mut audio_tracks = Vec::new();
	for child in timeline.tracks().children() {
		if let Some(track) = child.as_track() {
			match track.kind() {
				"Video" => video_tracks.push(track),
				"Audio" => audio_tracks.push(track),
				other => eprintln!("oakotio fcpxml: skipping track of unknown kind \"{other}\""),
			}
		}
	}

	// The first video track (or first audio track when there is no video)
	// becomes the primary storyline (<spine>).
	let spine = video_tracks
		.first()
		.copied()
		.or_else(|| audio_tracks.first().copied());

	let seq_rate = sequence_rate_of(timeline);
	let duration = timeline_duration(timeline);
	let tc_start = match timeline.global_start_time() {
		Some(rt) => seconds_of(rt),
		None => Rational::new(0, 1),
	};
	let tc_format = metadata_string(timeline, "tcFormat").unwrap_or_else(|| "NDF".to_string());

	let mut seq = BytesStart::new("sequence");
	let seq_format_id = resources.format_id_for(frame_duration_of(seq_rate));
	push_attr(&mut seq, "format", &seq_format_id);
	push_attr(&mut seq, "tcStart", &format_time(tc_start));
	push_attr(&mut seq, "tcFormat", &tc_format);
	push_attr(&mut seq, "duration", &format_time(duration));
	if let Some(layout) = metadata_string(timeline, "audioLayout") {
		push_attr(&mut seq, "audioLayout", &layout);
	}
	if let Some(rate) = metadata_string(timeline, "audioRate") {
		push_attr(&mut seq, "audioRate", &rate);
	}
	push_attr(&mut seq, "name", timeline.name());
	write_event(writer, Event::Start(seq))?;

	if let Some(spine_track) = spine {
		write_event(writer, Event::Start(BytesStart::new("spine")))?;
		let mut pos = Rational::new(0, 1);
		write_blocks(writer, spine_track, seq_rate, resources, &mut pos)?;
		write_event(writer, Event::End(BytesEnd::new("spine")))?;
	}

	let mut spine_was_video = false;
	if let Some(spine_track) = spine {
		spine_was_video = spine_track.kind() == "Video";
	}
	for track in video_tracks.iter().skip(usize::from(spine_was_video)) {
		write_event(writer, Event::Start(BytesStart::new("video")))?;
		let mut pos = Rational::new(0, 1);
		write_blocks(writer, track, seq_rate, resources, &mut pos)?;
		write_event(writer, Event::End(BytesEnd::new("video")))?;
	}
	let spine_was_audio = !spine_was_video && spine.is_some();
	for track in audio_tracks.iter().skip(usize::from(spine_was_audio)) {
		write_event(writer, Event::Start(BytesStart::new("audio")))?;
		let mut pos = Rational::new(0, 1);
		write_blocks(writer, track, seq_rate, resources, &mut pos)?;
		write_event(writer, Event::End(BytesEnd::new("audio")))?;
	}

	write_event(writer, Event::End(BytesEnd::new("sequence")))?;
	Ok(())
}

/// Write one `<event>`/`<project>`/`<sequence>` group for a timeline.
fn write_project(
	writer: &mut Writer<Vec<u8>>,
	timeline: &Timeline,
	resources: &mut ResourceSet,
) -> Result<()> {
	let event_name = metadata_string(timeline, "event").unwrap_or_else(|| "Event".to_string());
	let project_name = metadata_string(timeline, "project")
		.filter(|s| !s.is_empty())
		.unwrap_or_else(|| timeline.name().to_string());

	let mut event_el = BytesStart::new("event");
	push_attr(&mut event_el, "name", &event_name);
	write_event(writer, Event::Start(event_el))?;
	let mut project_el = BytesStart::new("project");
	push_attr(&mut project_el, "name", &project_name);
	write_event(writer, Event::Start(project_el))?;
	write_sequence(writer, timeline, resources)?;
	write_event(writer, Event::End(BytesEnd::new("project")))?;
	write_event(writer, Event::End(BytesEnd::new("event")))?;
	Ok(())
}

/// Serialize timelines to an FCPXML 1.10 document string.
///
/// Each timeline becomes one `<project>`/`<sequence>` pair inside a single
/// `<library>`; formats and assets are collected and deduplicated across
/// all timelines.
pub fn to_fcpxml_string(timelines: &[Timeline]) -> Result<String> {
	let mut resources = ResourceSet::default();
	for timeline in timelines {
		resources.collect_from(timeline);
	}

	let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
	write_event(
		&mut writer,
		Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)),
	)?;
	write_event(&mut writer, Event::DocType(BytesText::new("fcpxml")))?;

	let mut root = BytesStart::new("fcpxml");
	push_attr(&mut root, "version", FCPXML_VERSION);
	write_event(&mut writer, Event::Start(root))?;

	write_resources(&mut writer, &resources)?;

	if !timelines.is_empty() {
		write_event(&mut writer, Event::Start(BytesStart::new("library")))?;
		for timeline in timelines {
			write_project(&mut writer, timeline, &mut resources)?;
		}
		write_event(&mut writer, Event::End(BytesEnd::new("library")))?;
	}

	write_event(&mut writer, Event::End(BytesEnd::new("fcpxml")))?;

	let bytes = writer.into_inner();
	String::from_utf8(bytes)
		.map_err(|e| FcpxmlError::Xml(format!("writer produced non-UTF-8 output: {e}")))
}

/// Serialize timelines to an FCPXML 1.10 document file.
pub fn to_fcpxml_file(timelines: &[Timeline], path: impl AsRef<Path>) -> Result<()> {
	std::fs::write(path, to_fcpxml_string(timelines)?)?;
	Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use std::error::Error as _;

	#[test]
	fn parse_time_forms() {
		assert_eq!(parse_time("0s").unwrap(), Rational::new(0, 1));
		assert_eq!(parse_time("48s").unwrap(), Rational::new(48, 1));
		assert_eq!(parse_time("100/3000s").unwrap(), Rational::new(1, 30));
		assert_eq!(
			parse_time("1001/30000s").unwrap(),
			Rational::new(1001, 30000)
		);
		assert_eq!(parse_time("1/24").unwrap(), Rational::new(1, 24));
		assert_eq!(parse_time("0").unwrap(), Rational::new(0, 1));
		assert_eq!(parse_time("1.5s").unwrap(), Rational::new(3, 2));
		assert!(parse_time("abc").is_err());
		assert!(parse_time("1/0s").is_err());
		assert!(parse_time("24f").is_err());
		assert!(parse_time("1/2/3s").is_err());
	}

	#[test]
	fn format_time_forms() {
		assert_eq!(format_time(Rational::new(0, 1)), "0s");
		assert_eq!(format_time(Rational::new(48, 1)), "48s");
		assert_eq!(format_time(Rational::new(1001, 30000)), "1001/30000s");
		assert_eq!(format_time(Rational::new(2, 5)), "2/5s");
	}

	#[test]
	fn ntsc_rate_round_trip() {
		// 1 frame at 29.97fps: frameDuration 1001/30000s -> 30000/1001 fps.
		let rate = rate_of(Rational::new(1001, 30000)).unwrap();
		assert_eq!(rate, Rational::new(30000, 1001));
		// 3 frames come back as an exact integer at that rate.
		let t = time_at_rate(Rational::new(3003, 30000), rate);
		assert!((t.value() - 3.0).abs() < 1e-9, "value = {}", t.value());
		// ... and the seconds round-trip to the same rational.
		assert_eq!(seconds_of(t), Rational::new(1001, 10000));
		assert_eq!(frame_duration_of(rate), Rational::new(1001, 30000));
		// 23.976 material (24000/1001) behaves the same.
		let rate23976 = rate_of(Rational::new(1001, 24000)).unwrap();
		assert_eq!(rate23976, Rational::new(24000, 1001));
		assert_eq!(frame_duration_of(rate23976), Rational::new(1001, 24000));
	}

	#[test]
	fn version_gates() {
		assert!(check_version("1.0").is_ok());
		assert!(check_version("1.9").is_ok());
		assert!(check_version("1.10").is_ok());
		assert!(check_version("1.11").is_ok());
		assert!(check_version("2.0").is_err());
		assert!(check_version("0.9").is_err());
		assert!(check_version("1.12").is_err());
		assert!(check_version("bogus").is_err());
	}

	#[test]
	fn format_names() {
		assert_eq!(
			format_name(Rational::new(1, 30)),
			"FFVideoFormat1920x1080p30"
		);
		assert_eq!(
			format_name(Rational::new(1001, 30000)),
			"FFVideoFormat1920x1080p29_97"
		);
	}

	#[test]
	fn block_durations() {
		let mut clip = Clip::new("c");
		clip.set_source_range(TimeRange::new(
			RationalTime::new(0.0, 24.0),
			RationalTime::new(48.0, 24.0),
		));
		assert_eq!(block_duration(&Composable::Clip(clip)), Rational::new(2, 1));
		let mut t = Transition::new("t");
		t.set_in_offset(RationalTime::new(12.0, 24.0));
		t.set_out_offset(RationalTime::new(12.0, 24.0));
		assert_eq!(
			block_duration(&Composable::Transition(t)),
			Rational::new(1, 1)
		);
	}

	#[test]
	fn error_display_and_source() {
		assert!(FcpxmlError::Xml("x".into())
			.to_string()
			.contains("XML error"));
		assert!(FcpxmlError::Malformed("x".into())
			.to_string()
			.contains("document error"));
		assert!(FcpxmlError::UnsupportedVersion("x".into())
			.to_string()
			.contains("version error"));
		let io = FcpxmlError::Io(std::io::Error::new(std::io::ErrorKind::Other, "io"));
		assert!(io.to_string().contains("file error"));
		assert!(io.source().is_some());
		assert!(FcpxmlError::Xml("x".into()).source().is_none());
		assert!(FcpxmlError::Malformed("x".into()).source().is_none());
	}

	#[test]
	fn negative_and_decimal_time_values() {
		assert_eq!(parse_time("-3/2s").unwrap(), Rational::new(-3, 2));
		assert_eq!(parse_time("-0.5s").unwrap(), Rational::new(-1, 2));
		assert_eq!(format_time(Rational::new(-3, 2)), "-3/2s");
	}

	#[test]
	fn resource_error_branches_are_lenient() {
		let doc = r#"<fcpxml version="1.10">
	<resources>
		<format frameDuration="100/3000s"/>
		<format id="f2" frameDuration="bogus"/>
		<format id="f3" frameDuration="1/24s"/>
		<asset duration="junk"/>
		<asset id="a2"/>
	</resources>
	<library><event name="e"><project name="p">
		<sequence format="f3"><spine/></sequence>
	</project></event></library>
</fcpxml>"#;
		let timelines = from_fcpxml_string(doc).expect("broken resources are skipped");
		assert_eq!(timelines.len(), 1);
		assert_eq!(timelines[0].name(), "p");
	}

	#[test]
	fn zero_frame_duration_is_an_error() {
		let doc = r#"<fcpxml version="1.10">
	<resources><format id="f1" frameDuration="0/3000s"/></resources>
	<library><event name="e"><project name="p">
		<sequence format="f1"><spine/></sequence>
	</project></event></library>
</fcpxml>"#;
		assert!(matches!(
			from_fcpxml_string(doc),
			Err(FcpxmlError::Malformed(_))
		));
	}

	#[test]
	fn unknown_asset_format_falls_back_to_sequence_rate() {
		let doc = r#"<fcpxml version="1.10">
	<resources>
		<format id="f1" frameDuration="100/3000s"/>
		<asset id="a1" src="file:///x.mov" format="ghost" duration="10s"/>
	</resources>
	<library><event name="e"><project name="p">
		<sequence format="f1"><spine>
			<asset-clip name="c" ref="a1" offset="0s" duration="300/3000s" start="0s"/>
		</spine></sequence>
	</project></event></library>
</fcpxml>"#;
		let timelines = from_fcpxml_string(doc).expect("falls back to the sequence rate");
		let clip = timelines[0].tracks().children()[0]
			.as_track()
			.unwrap()
			.children()[0]
			.as_clip()
			.unwrap();
		let range = clip.source_range().unwrap();
		assert_eq!(range.duration().rate(), 30.0);
		// The asset reference still resolves (src is kept).
		let external = clip
			.media_reference()
			.unwrap()
			.as_external_reference()
			.unwrap();
		assert_eq!(external.target_url(), "file:///x.mov");
	}

	#[test]
	fn nested_track_block_and_unknown_kind_are_skipped() {
		let mut timeline = Timeline::new("nested");
		let mut track = Track::new("Video");
		track.append_child(Composable::Track(Track::new("Video")));
		timeline.tracks_mut().append_child(Composable::Track(track));
		let xml = to_fcpxml_string(&[timeline]).expect("nested track block is skipped");
		assert!(xml.contains("<spine>"), "{xml}");
		assert!(xml.contains("</spine>"), "{xml}");

		let mut timeline = Timeline::new("kinds");
		let mut subtitles = Track::new("Subtitles");
		subtitles.append_child(Composable::Clip(Clip::new("c")));
		timeline
			.tracks_mut()
			.append_child(Composable::Track(subtitles));
		let xml = to_fcpxml_string(&[timeline]).expect("unknown track kind is skipped");
		assert!(!xml.contains("<spine>"), "{xml}");
		assert!(xml.contains("<sequence"), "{xml}");
	}

	#[test]
	fn clip_without_media_reference_writes_without_ref() {
		let mut timeline = Timeline::new("t");
		let mut track = Track::new("Video");
		let mut clip = Clip::new("orphan");
		clip.set_source_range(TimeRange::new(
			RationalTime::new(0.0, 24.0),
			RationalTime::new(24.0, 24.0),
		));
		track.append_child(Composable::Clip(clip));
		timeline.tracks_mut().append_child(Composable::Track(track));
		let xml = to_fcpxml_string(&[timeline]).expect("export");
		assert!(
			xml.contains(
				"<asset-clip name=\"orphan\" offset=\"0s\" duration=\"1s\" start=\"0s\"/>"
			),
			"{xml}"
		);
	}

	#[test]
	fn file_io_errors() {
		assert!(from_fcpxml_file("/nonexistent/oakotio-test.fcpxml").is_err());
		assert!(to_fcpxml_file(&[], &std::env::temp_dir()).is_err());
	}
}
