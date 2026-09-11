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
//
//! The disk frame-cache container: a minimal self-describing raw-file
//! format used by the disk frame cache until the liboakoiio/EXR path
//! lands (oakcodec's `oiio.rs` is a stub, so the payload I/O is
//! implemented here).
//!
//! The file layout is: 9-byte magic `OAKCACHE1`, `u32` LE container
//! version (1), `i32` LE width/height/format/channels, `i64` LE
//! timestamp numerator/denominator, `u64` LE payload length, then the
//! tightly packed pixel payload (the frame's `data` bytes verbatim).
//! All multi-byte integers are little-endian.
//!
//! The video metadata beyond dims/format is not persisted: loaded frames
//! carry default [`VideoParamsPod`] values with width/height/format
//! mirrored from the header (cache frames are full-resolution internal
//! F32 RGBA, which is all the payload needs).

use crate::error::{Error, Result};
use oak_core::frame::VideoParamsPod;
use oak_core::texture::Frame;
use oak_core::{PixelFormat, Rational};

/// Container magic (`OAKCACHE1`, one digit per container revision).
const MAGIC: &[u8; 9] = b"OAKCACHE1";

/// Container version written by this implementation.
const VERSION: u32 = 1;

/// Fixed header length in bytes (magic + version + w/h + format +
/// channels + timestamp + payload length).
const HEADER_LEN: usize = MAGIC.len() + 4 + 4 + 4 + 4 + 4 + 8 + 8 + 8;

/// Serialize `frame` to `path` (atomically: a `.tmp` sibling is written
/// then renamed over the destination).
///
/// Fails with [`Error::Failed`] when the parent directory does not exist
/// or the write/rename fails.
pub fn save_cache_frame(path: &str, frame: &Frame) -> Result<()> {
	let mut buf = Vec::with_capacity(HEADER_LEN + frame.data.len());
	buf.extend_from_slice(MAGIC);
	buf.extend_from_slice(&VERSION.to_le_bytes());
	buf.extend_from_slice(&frame.width.to_le_bytes());
	buf.extend_from_slice(&frame.height.to_le_bytes());
	buf.extend_from_slice(&(frame.format as i32).to_le_bytes());
	buf.extend_from_slice(&frame.channels.to_le_bytes());
	buf.extend_from_slice(&frame.timestamp.numerator().to_le_bytes());
	buf.extend_from_slice(&frame.timestamp.denominator().to_le_bytes());
	buf.extend_from_slice(&(frame.data.len() as u64).to_le_bytes());
	buf.extend_from_slice(&frame.data);

	let tmp = format!("{path}.tmp");
	std::fs::write(&tmp, &buf)
		.map_err(|e| Error::Failed(format!("frame cache write failed: {tmp}: {e}")))?;
	std::fs::rename(&tmp, path)
		.map_err(|e| Error::Failed(format!("frame cache rename failed: {tmp}: {e}")))?;
	Ok(())
}

/// Deserialize a frame saved by [`save_cache_frame`].
///
/// Fails with [`Error::NotFound`] when the file does not exist and
/// [`Error::Failed`] when it is too short, has the wrong magic/version,
/// carries invalid metadata or a payload length that does not match
/// `height × linesize`.
pub fn load_cache_frame(path: &str) -> Result<Frame> {
	let bytes = std::fs::read(path).map_err(|e| {
		if e.kind() == std::io::ErrorKind::NotFound {
			Error::NotFound
		} else {
			Error::Failed(format!("frame cache read failed: {path}: {e}"))
		}
	})?;
	if bytes.len() < HEADER_LEN {
		return Err(Error::Failed(format!(
			"frame cache truncated header: {path} ({} bytes)",
			bytes.len()
		)));
	}
	if &bytes[0..MAGIC.len()] != MAGIC {
		return Err(Error::Failed(format!("frame cache bad magic: {path}")));
	}
	let mut off = MAGIC.len();
	let version = read_u32(&bytes, &mut off);
	if version != VERSION {
		return Err(Error::Failed(format!(
			"frame cache unsupported version {version}: {path}"
		)));
	}
	let width = read_i32(&bytes, &mut off);
	let height = read_i32(&bytes, &mut off);
	let format = match read_i32(&bytes, &mut off) {
		f if f == PixelFormat::U8 as i32 => PixelFormat::U8,
		f if f == PixelFormat::U10 as i32 => PixelFormat::U10,
		f if f == PixelFormat::U16 as i32 => PixelFormat::U16,
		f if f == PixelFormat::F16 as i32 => PixelFormat::F16,
		f if f == PixelFormat::F32 as i32 => PixelFormat::F32,
		f => return Err(Error::Failed(format!("frame cache bad format {f}: {path}"))),
	};
	let channels = read_i32(&bytes, &mut off);
	let ts_num = read_i64(&bytes, &mut off);
	let ts_den = read_i64(&bytes, &mut off);
	let payload_len = read_u64(&bytes, &mut off);

	if width < 0 || height < 0 {
		return Err(Error::Failed(format!(
			"frame cache negative dimensions {width}x{height}: {path}"
		)));
	}
	if channels <= 0 {
		return Err(Error::Failed(format!(
			"frame cache bad channel count {channels}: {path}"
		)));
	}
	let linesize = (width as usize)
		.checked_mul(channels as usize)
		.and_then(|n| n.checked_mul(format.bytes_per_channel()))
		.ok_or_else(|| Error::Failed(format!("frame cache linesize overflow: {path}")))?;
	let expected = (height as usize)
		.checked_mul(linesize)
		.ok_or_else(|| Error::Failed(format!("frame cache size overflow: {path}")))?;
	if payload_len != expected as u64 {
		return Err(Error::Failed(format!(
			"frame cache payload length mismatch: {path} \
			 (header {payload_len}, expected {expected})"
		)));
	}
	let payload = &bytes[off..];
	if payload.len() != expected {
		return Err(Error::Failed(format!(
			"frame cache truncated payload: {path} ({} bytes, expected {expected})",
			payload.len()
		)));
	}

	Ok(Frame {
		width,
		height,
		format,
		channels,
		timestamp: Rational::new(ts_num, ts_den),
		data: payload.to_vec(),
		params: VideoParamsPod {
			width,
			height,
			format: format as i32,
			..VideoParamsPod::default()
		},
	})
}

fn read_i32(bytes: &[u8], off: &mut usize) -> i32 {
	let v = i32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap());
	*off += 4;
	v
}

fn read_u32(bytes: &[u8], off: &mut usize) -> u32 {
	let v = u32::from_le_bytes(bytes[*off..*off + 4].try_into().unwrap());
	*off += 4;
	v
}

fn read_i64(bytes: &[u8], off: &mut usize) -> i64 {
	let v = i64::from_le_bytes(bytes[*off..*off + 8].try_into().unwrap());
	*off += 8;
	v
}

fn read_u64(bytes: &[u8], off: &mut usize) -> u64 {
	let v = u64::from_le_bytes(bytes[*off..*off + 8].try_into().unwrap());
	*off += 8;
	v
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;
	use std::sync::atomic::{AtomicU32, Ordering};

	static COUNTER: AtomicU32 = AtomicU32::new(0);

	fn temp_path(tag: &str) -> String {
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		std::env::temp_dir()
			.join(format!("oakcache_io_{}_{n}_{tag}.bin", std::process::id()))
			.to_string_lossy()
			.into_owned()
	}

	fn sample_frame(width: i32, height: i32) -> Frame {
		let format = PixelFormat::F32;
		let channels = VideoParamsPod::INTERNAL_CHANNEL_COUNT;
		let size =
			(width as usize) * (height as usize) * (channels as usize) * format.bytes_per_channel();
		let data = (0..size).map(|i| (i * 7 % 251) as u8).collect();
		Frame {
			width,
			height,
			format,
			channels,
			timestamp: Rational::new(5, 2),
			data,
			params: VideoParamsPod {
				width,
				height,
				format: format as i32,
				..VideoParamsPod::default()
			},
		}
	}

	fn cleanup(path: &str) {
		let _ = std::fs::remove_file(path);
		let _ = std::fs::remove_file(format!("{path}.tmp"));
	}

	#[test]
	fn save_load_round_trips_every_field() {
		let path = temp_path("roundtrip");
		let frame = sample_frame(4, 3);
		save_cache_frame(&path, &frame).expect("save");
		let loaded = load_cache_frame(&path).expect("load");
		assert_eq!(loaded, frame);
		assert_eq!(loaded.data, frame.data);
		cleanup(&path);
	}

	#[test]
	fn load_missing_file_is_err() {
		let path = temp_path("missing");
		assert!(load_cache_frame(&path).is_err());
		assert!(!Path::new(&path).exists());
	}

	#[test]
	fn load_truncated_payload_is_err() {
		let path = temp_path("truncated");
		let frame = sample_frame(4, 3);
		save_cache_frame(&path, &frame).expect("save");
		let full = std::fs::read(&path).expect("read back");
		std::fs::write(&path, &full[..full.len() - 8]).expect("truncate");
		assert!(load_cache_frame(&path).is_err());
		cleanup(&path);
	}

	#[test]
	fn load_bad_magic_is_err() {
		let path = temp_path("badmagic");
		std::fs::write(&path, b"NOTACACHE.................").expect("write");
		assert!(load_cache_frame(&path).is_err());
		cleanup(&path);
	}
}
