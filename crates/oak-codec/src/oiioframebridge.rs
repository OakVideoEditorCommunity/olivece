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

//! `oiio_frame_to_buffer` / `oiio_buffer_to_frame` — codec-internal OIIO
//! frame <-> pixel-buffer conversion.
//!
//! Mirrors `src/codec/src/oiioframebridge.{h,cpp}`. These are internal C++
//! functions that moved into codec from oak_core (NOTES.md §oak_core侧修复);
//! oak_core keeps its OIIO mapping functions; the frame conversion itself
//! lives here.
//!
//! The C++ bridge copies pixels through the live OpenImageIO `ImageBuf`
//! (`oiio_frame_to_buffer`/`oiio_buffer_to_frame`). OIIO is not linked into
//! this build, so the port serializes the frame into a self-describing byte
//! buffer instead. The layout is stable (documented in
//! [`OiioBufferHeader`]); it carries the frame's geometry, pixel format,
//! timestamp and time base alongside the raw pixel rows, so a buffer can be
//! turned back into an equivalent [`Frame`] without any external state.

use oak_core::ocioutils::PixelFormat as OakPixelFormat;
use oak_core::videoparams::VideoParams;
use crate::frame::Frame;
use oak_core::Rational;
use std::ffi::c_int;

/// Fixed header size, in bytes, of an OIIO frame buffer.
///
/// The layout is a 64-byte little-endian header followed by the raw pixel
/// data. `OiioBufferHeader::to_bytes` / `from_bytes` are the single writer /
/// reader of this header, so the exact offsets only ever exist in one place.
const HEADER_LEN: usize = 64;

/// Magic bytes identifying an OIIO frame buffer (`"OFMB"`).
const MAGIC: &[u8; 4] = b"OFMB";

/// Current serialization version.
const VERSION: u32 = 1;

/// Decoded OIIO frame-buffer header.
///
/// # Byte layout (little-endian, `HEADER_LEN` = 64 bytes)
///
/// | Offset | Size | Field |
/// |--------|------|-------|
/// | 0 | 4 | Magic bytes `"OFMB"` |
/// | 4 | 4 | Serialization `version` (`u32`, currently 1) |
/// | 8 | 4 | `width` (`i32`) |
/// | 12 | 4 | `height` (`i32`) |
/// | 16 | 4 | `format` (`i32`, an `OakPixelFormat` value) |
/// | 20 | 4 | `linesize_bytes` (`i32`, distance between pixel rows) |
/// | 24 | 8 | `timestamp_num` (`i64`) |
/// | 32 | 8 | `timestamp_den` (`i64`) |
/// | 40 | 8 | `time_base_num` (`i64`) |
/// | 48 | 8 | `time_base_den` (`i64`) |
/// | 56 | 8 | `pixel_len` (`u64`, pixel-data length in bytes) |
/// | 64 | … | raw pixel data (`pixel_len` bytes) |
///
/// `pixel_len` must equal `linesize_bytes * height`; the frame geometry in the
/// header must match the params the frame is reconstructed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OiioBufferHeader {
	version: u32,
	width: i32,
	height: i32,
	format: i32,
	linesize_bytes: i32,
	timestamp_num: i64,
	timestamp_den: i64,
	time_base_num: i64,
	time_base_den: i64,
	pixel_len: u64,
}

impl OiioBufferHeader {
	/// Serialize the header into exactly `HEADER_LEN` bytes (little-endian).
	fn to_bytes(&self) -> [u8; HEADER_LEN] {
		let mut b = [0u8; HEADER_LEN];
		b[0..4].copy_from_slice(MAGIC);
		b[4..8].copy_from_slice(&self.version.to_le_bytes());
		b[8..12].copy_from_slice(&self.width.to_le_bytes());
		b[12..16].copy_from_slice(&self.height.to_le_bytes());
		b[16..20].copy_from_slice(&self.format.to_le_bytes());
		b[20..24].copy_from_slice(&self.linesize_bytes.to_le_bytes());
		b[24..32].copy_from_slice(&self.timestamp_num.to_le_bytes());
		b[32..40].copy_from_slice(&self.timestamp_den.to_le_bytes());
		b[40..48].copy_from_slice(&self.time_base_num.to_le_bytes());
		b[48..56].copy_from_slice(&self.time_base_den.to_le_bytes());
		b[56..64].copy_from_slice(&self.pixel_len.to_le_bytes());
		b
	}

	/// Parse a header from the start of `bytes`, validating the magic and
	/// version and requiring at least `HEADER_LEN` bytes. Returns the decoded
	/// header and the number of bytes consumed (`HEADER_LEN`).
	fn from_bytes(bytes: &[u8]) -> crate::error::Result<(Self, usize)> {
		if bytes.len() < HEADER_LEN {
			return Err(crate::error::Error::Invalid);
		}
		if &bytes[0..4] != MAGIC {
			return Err(crate::error::Error::Invalid);
		}
		let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
		if version != VERSION {
			return Err(crate::error::Error::Invalid);
		}
		let header = OiioBufferHeader {
			version,
			width: i32::from_le_bytes(bytes[8..12].try_into().unwrap()),
			height: i32::from_le_bytes(bytes[12..16].try_into().unwrap()),
			format: i32::from_le_bytes(bytes[16..20].try_into().unwrap()),
			linesize_bytes: i32::from_le_bytes(bytes[20..24].try_into().unwrap()),
			timestamp_num: i64::from_le_bytes(bytes[24..32].try_into().unwrap()),
			timestamp_den: i64::from_le_bytes(bytes[32..40].try_into().unwrap()),
			time_base_num: i64::from_le_bytes(bytes[40..48].try_into().unwrap()),
			time_base_den: i64::from_le_bytes(bytes[48..56].try_into().unwrap()),
			pixel_len: u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
		};
		Ok((header, HEADER_LEN))
	}
}

/// Convert an OIIO-backed `Frame` into a raw pixel buffer.
///
/// # CPP-PARITY
/// `src/codec/src/oiioframebridge.cpp` `oiio_frame_to_buffer` —
/// allocates the destination and copies the OIIO pixel data out.
///
/// The buffer is a 64-byte [`OiioBufferHeader`] followed by the frame's pixel
/// rows (see the header docs for the exact layout). The frame must already be
/// allocated ([`Frame::allocate`]); otherwise this returns
/// [`crate::error::Error::State`].
pub fn oiio_frame_to_buffer(frame: &Frame) -> crate::error::Result<Vec<u8>> {
	let data = frame.data().ok_or(crate::error::Error::State)?;
	let width = frame.width();
	let height = frame.height();
	let format = frame.format() as i32;
	let linesize_bytes = frame.linesize_bytes();
	let timestamp = frame.timestamp();

	let (time_base_num, time_base_den) = match frame.params() {
		Some(p) => {
			let (num, den) = p.time_base();
			(i64::from(num), i64::from(den))
		}
		None => (0, 0),
	};

	let header = OiioBufferHeader {
		version: VERSION,
		width,
		height,
		format,
		linesize_bytes,
		timestamp_num: timestamp.numerator(),
		timestamp_den: timestamp.denominator(),
		time_base_num,
		time_base_den,
		pixel_len: data.len() as u64,
	};

	let mut out = Vec::with_capacity(HEADER_LEN + data.len());
	out.extend_from_slice(&header.to_bytes());
	out.extend_from_slice(data);
	Ok(out)
}

/// Convert a raw pixel buffer back into a `Frame`.
///
/// # CPP-PARITY
/// `src/codec/src/oiioframebridge.cpp` `oiio_buffer_to_frame` —
/// wraps the buffer bytes in a `Frame` for encoder consumption.
///
/// Parses a [`OiioBufferHeader`] from the front of `buffer`, reconstructs the
/// frame's params (geometry, time base, pixel format) and timestamp, then
/// copies the pixel rows into an allocated frame. The buffer is rejected with
/// [`crate::error::Error::Invalid`] when the magic/version is wrong, the
/// header is truncated, or `pixel_len` is inconsistent with the declared
/// `linesize_bytes * height` / the bytes actually present.
pub fn oiio_buffer_to_frame(buffer: &[u8]) -> crate::error::Result<Frame> {
	let (header, consumed) = OiioBufferHeader::from_bytes(buffer)?;
	let pixels = &buffer[consumed..];

	// `pixel_len` must be consistent with the header geometry and the bytes
	// actually present (checked, so a corrupt header can't panic later).
	if header.linesize_bytes < 0 || header.height < 0 {
		return Err(crate::error::Error::Invalid);
	}
	let expected = (header.linesize_bytes as u64).checked_mul(header.height as u64);
	if expected != Some(header.pixel_len) {
		return Err(crate::error::Error::Invalid);
	}
	if pixels.len() != header.pixel_len as usize {
		return Err(crate::error::Error::Invalid);
	}

	// Build the params from the header, then hand ownership to the frame.
	let mut params = VideoParams::new_with_time_base(
		header.width,
		header.height,
		header.time_base_num as c_int,
		header.time_base_den as c_int,
		OakPixelFormat::from_code(0),
		4,
		1,
		1,
		0,
		1,
	);
	params.set_format(OakPixelFormat::from_code(header.format));
	let mut frame = Frame::with_params(params);
	frame.set_timestamp(Rational::new(header.timestamp_num, header.timestamp_den));
	frame.allocate()?;

	match frame.data_mut() {
		Some(dst) if dst.len() == pixels.len() => {
			dst.copy_from_slice(pixels);
			Ok(frame)
		}
		// Reconstructed params should produce exactly `pixel_len` bytes; if
		// the geometry in the header disagreed with the format's line size,
		// refuse rather than copy a mismatched slice.
		_ => Err(crate::error::Error::Invalid),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn header_round_trip() {
		let header = OiioBufferHeader {
			version: VERSION,
			width: 100,
			height: 50,
			format: 0, // U8
			linesize_bytes: 512,
			timestamp_num: 5,
			timestamp_den: 2,
			time_base_num: 1,
			time_base_den: 30,
			pixel_len: 25600,
		};
		let bytes = header.to_bytes();
		assert_eq!(bytes.len(), HEADER_LEN);
		let (decoded, consumed) = OiioBufferHeader::from_bytes(&bytes).unwrap();
		assert_eq!(consumed, HEADER_LEN);
		assert_eq!(decoded, header);
	}

	#[test]
	fn header_is_little_endian_magic_and_version() {
		let header = OiioBufferHeader {
			version: VERSION,
			width: 1,
			height: 1,
			format: 0,
			linesize_bytes: 4,
			timestamp_num: 0,
			timestamp_den: 1,
			time_base_num: 0,
			time_base_den: 0,
			pixel_len: 4,
		};
		let bytes = header.to_bytes();
		assert_eq!(&bytes[0..4], b"OFMB");
		assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
		// width = 1 -> little-endian 01 00 00 00.
		assert_eq!(&bytes[8..12], &[1, 0, 0, 0]);
	}

	#[test]
	fn from_bytes_rejects_truncated() {
		let header = OiioBufferHeader {
			version: VERSION,
			width: 1,
			height: 1,
			format: 0,
			linesize_bytes: 4,
			timestamp_num: 0,
			timestamp_den: 1,
			time_base_num: 0,
			time_base_den: 0,
			pixel_len: 4,
		};
		let bytes = header.to_bytes();
		assert!(OiioBufferHeader::from_bytes(&bytes[..HEADER_LEN - 1]).is_err());
	}

	#[test]
	fn from_bytes_rejects_bad_magic_and_version() {
		let mut bytes = OiioBufferHeader {
			version: VERSION,
			width: 1,
			height: 1,
			format: 0,
			linesize_bytes: 4,
			timestamp_num: 0,
			timestamp_den: 1,
			time_base_num: 0,
			time_base_den: 0,
			pixel_len: 4,
		}
		.to_bytes();
		bytes[0] = b'X';
		assert!(OiioBufferHeader::from_bytes(&bytes).is_err());

		let mut bytes = OiioBufferHeader {
			version: VERSION,
			width: 1,
			height: 1,
			format: 0,
			linesize_bytes: 4,
			timestamp_num: 0,
			timestamp_den: 1,
			time_base_num: 0,
			time_base_den: 0,
			pixel_len: 4,
		}
		.to_bytes();
		bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
		assert!(OiioBufferHeader::from_bytes(&bytes).is_err());
	}

	/// A small helper to build a fully allocated, filled frame using the same
	/// params pattern as `frame.rs`.
	fn make_frame() -> Frame {
		let mut params = VideoParams::new_with_time_base(
			100,
			50,
			1,
			30,
			OakPixelFormat::from_code(0),
			4,
			1,
			1,
			0,
			1,
		);
		params.set_format(OakPixelFormat::from_code(0)); // U8
		let mut frame = Frame::with_params(params);
		frame.set_timestamp(Rational::new(5, 2));
		frame.allocate().unwrap();
		frame
	}

	#[test]
	fn frame_to_buffer_round_trip() {
		let mut frame = make_frame();
		if let Some(d) = frame.data_mut() {
			d.fill(0xAB);
		}

		let buffer = oiio_frame_to_buffer(&frame).unwrap();
		assert_eq!(buffer.len(), HEADER_LEN + frame.allocated_size());

		let (header, consumed) = OiioBufferHeader::from_bytes(&buffer).unwrap();
		assert_eq!(consumed, HEADER_LEN);
		assert_eq!(header.width, 100);
		assert_eq!(header.height, 50);
		assert_eq!(header.format, 0);
		assert_eq!(header.linesize_bytes, 512);
		assert_eq!(header.timestamp_num, 5);
		assert_eq!(header.timestamp_den, 2);
		assert_eq!(header.time_base_num, 1);
		assert_eq!(header.time_base_den, 30);
		assert_eq!(header.pixel_len, frame.allocated_size() as u64);
	}

	#[test]
	fn buffer_to_frame_round_trip() {
		let mut frame = make_frame();
		let expected_len = frame.allocated_size();
		if let Some(d) = frame.data_mut() {
			for (i, b) in d.iter_mut().enumerate() {
				*b = (i % 256) as u8;
			}
		}
		let buffer = oiio_frame_to_buffer(&frame).unwrap();

		let out = oiio_buffer_to_frame(&buffer).unwrap();
		assert_eq!(out.width(), 100);
		assert_eq!(out.height(), 50);
		assert_eq!(out.format() as i32, 0);
		assert_eq!(out.allocated_size(), expected_len);
		assert_eq!(out.timestamp().numerator(), 5);
		assert_eq!(out.timestamp().denominator(), 2);
		assert_eq!(out.data().unwrap(), frame.data().unwrap());
	}

	#[test]
	fn buffer_to_frame_rejects_corrupt() {
		let frame = make_frame();
		let buffer = oiio_frame_to_buffer(&frame).unwrap();

		// Truncated pixel data.
		assert!(oiio_buffer_to_frame(&buffer[..HEADER_LEN + 1]).is_err());
		// Bad magic.
		let mut bad = buffer.clone();
		bad[0] = 0;
		assert!(oiio_buffer_to_frame(&bad).is_err());
		// pixel_len inconsistent with linesize * height.
		let mut bad = buffer.clone();
		bad[56..64].copy_from_slice(&(999u64).to_le_bytes());
		assert!(oiio_buffer_to_frame(&bad).is_err());
	}
}
