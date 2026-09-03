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

//! Playback caches: validated/requested range bookkeeping and the
//! on-disk frame hash cache (C++ `PlaybackCache`/`FrameHashCache`/
//! `AudioPlaybackCache`/`AudioWaveformCache`/`ThumbnailCache`).
//!
//! CPP-PARITY notes:
//! - The on-disk state file layout matches `BinaryStreamReader/Writer`
//!   (src/render/transition/binarystream.h) byte for byte: big-endian
//!   u32/i32 fields, 16 raw UUID bytes — C++ and Rust builds share the
//!   same `<cache_dir>/<uuid>/state` files.
//! - The frame filename scheme matches `FrameHashCache::cache_path_name`:
//!   `<cache_dir>/<uuid>/<timestamp>` with the timestamp computed via
//!   `Timecode::time_to_timestamp(…, k_round)`.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use oak_core::{Rational, TimeRange, TimeRangeList};

use crate::error::{Error, Result};

/// Opaque identity of the owning node (the oaknode NodeId identity
/// integer; the cache never calls back into node code — the C++
/// `parent_` back-pointer is reduced to this identity plus explicit
/// C ABI calls where unavoidable).
pub type OwnerIdentity = u64;

/// Cache flavor (the C++ subclasses become one type + kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
	/// Video frame hash cache (disk-backed).
	VideoFrame,
	/// Thumbnail cache (disk-backed, frame-hash family).
	Thumbnail,
	/// Audio playback cache.
	AudioPlayback,
	/// Audio waveform cache.
	AudioWaveform,
}

impl CacheKind {
	/// True for the frame-hash flavors (disk-backed, carry a timebase and
	/// frame filenames).
	pub fn is_frame_hash(self) -> bool {
		matches!(self, CacheKind::VideoFrame | CacheKind::Thumbnail)
	}
}

// ---- canonical UUID text ("{8-4-4-4-12}", lowercase) ----------------------

struct UuidRng(u64);

fn uuid_rng() -> std::sync::MutexGuard<'static, UuidRng> {
	static RNG: LazyLock<Mutex<UuidRng>> = LazyLock::new(|| {
		let seed = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos() as u64)
			.unwrap_or(0x9E3779B97F4A7C15)
			| 1;
		Mutex::new(UuidRng(seed ^ 0x9E3779B97F4A7C15))
	});
	RNG.lock().unwrap_or_else(|e| e.into_inner())
}

impl UuidRng {
	fn next_u64(&mut self) -> u64 {
		// xorshift64*
		let mut x = self.0;
		x ^= x >> 12;
		x ^= x << 25;
		x ^= x >> 27;
		self.0 = x;
		x.wrapping_mul(0x2545F4914F6CDD1D)
	}
}

/// C++ `create_uuid_text()`: canonical "{8-4-4-4-12}" lowercase text with
/// version-4 and RFC-4122 variant bits.
pub fn create_uuid_text() -> String {
	let mut bytes = [0u8; 16];
	{
		let mut rng = uuid_rng();
		let hi = rng.next_u64();
		let lo = rng.next_u64();
		for i in 0..8 {
			bytes[i] = (hi >> (i * 8)) as u8;
		}
		for i in 0..8 {
			bytes[8 + i] = (lo >> (i * 8)) as u8;
		}
	}
	bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
	bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 1

	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut out = String::with_capacity(38);
	out.push('{');
	for i in 0..16 {
		if i == 4 || i == 6 || i == 8 || i == 10 {
			out.push('-');
		}
		out.push(HEX[(bytes[i] >> 4) as usize] as char);
		out.push(HEX[(bytes[i] & 0xF) as usize] as char);
	}
	out.push('}');
	out
}

// ---- binary stream helpers (QDataStream big-endian subset) -----------------

fn write_be_u32(out: &mut Vec<u8>, v: u32) {
	out.extend_from_slice(&v.to_be_bytes());
}

fn write_be_i32(out: &mut Vec<u8>, v: i32) {
	out.extend_from_slice(&v.to_be_bytes());
}

/// 16 raw bytes in RFC 4122 order from the canonical text form.
fn uuid_text_to_bytes(uuid: &str) -> [u8; 16] {
	let mut bytes = [0u8; 16];
	let mut nibble = 0usize;
	for c in uuid.chars() {
		if c == '{' || c == '}' || c == '-' {
			continue;
		}
		if nibble >= 32 {
			break;
		}
		let d = c.to_digit(16).unwrap_or(0) as u8;
		if nibble % 2 == 0 {
			bytes[nibble / 2] = d << 4;
		} else {
			bytes[nibble / 2] |= d;
		}
		nibble += 1;
	}
	bytes
}

/// Canonical "{8-4-4-4-12}" lowercase text from 16 raw bytes.
fn bytes_to_uuid_text(bytes: &[u8; 16]) -> String {
	const HEX: &[u8; 16] = b"0123456789abcdef";
	let mut out = String::with_capacity(38);
	out.push('{');
	for i in 0..16 {
		if i == 4 || i == 6 || i == 8 || i == 10 {
			out.push('-');
		}
		out.push(HEX[(bytes[i] >> 4) as usize] as char);
		out.push(HEX[(bytes[i] & 0xF) as usize] as char);
	}
	out.push('}');
	out
}

/// Reader over a byte slice; reads past the end yield zeroed values
/// (QDataStream ReadPastEnd semantics).
struct ByteReader<'a> {
	data: &'a [u8],
	pos: usize,
}

impl<'a> ByteReader<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, pos: 0 }
	}

	fn read_u32(&mut self) -> u32 {
		let v = self.read_be(4);
		v as u32
	}

	fn read_i32(&mut self) -> i32 {
		let v = self.read_be(4);
		v as i32
	}

	fn read_uuid(&mut self) -> [u8; 16] {
		let mut b = [0u8; 16];
		let n = self.take(&mut b);
		let _ = n;
		b
	}

	fn take(&mut self, out: &mut [u8]) -> usize {
		let n = (self.data.len() - self.pos).min(out.len());
		out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
		self.pos += n;
		n
	}

	fn read_be(&mut self, bytes: usize) -> u64 {
		let mut b = [0u8; 8];
		let n = (self.data.len() - self.pos).min(bytes);
		b[8 - bytes..8 - bytes + n].copy_from_slice(&self.data[self.pos..self.pos + n]);
		self.pos += n;
		let mut v: u64 = 0;
		for i in 0..bytes {
			v = (v << 8) | b[8 - bytes + i] as u64;
		}
		v
	}
}

/// Modification time in milliseconds since the epoch (C++
/// `modification_time_msecs`; 0 when unknown).
fn modification_time_msecs(path: &std::path::Path) -> i64 {
	std::fs::metadata(path)
		.and_then(|m| m.modified())
		.ok()
		.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
		.map(|d| d.as_millis() as i64)
		.unwrap_or(0)
}

/// The unified cache.
pub struct PlaybackCache {
	/// Flavor.
	pub kind: CacheKind,
	/// Owner identity.
	pub owner: OwnerIdentity,
	/// UUID (canonical text; project-file compatible).
	pub uuid: String,
	/// Frame timebase (frame-hash flavors only).
	pub timebase: Option<Rational>,
	/// Validated ranges.
	validated: TimeRangeList,
	/// Requested-but-not-yet-validated ranges.
	requested: TimeRangeList,
	/// Passthrough target uuids with their ranges.
	passthroughs: Vec<(TimeRange, String)>,
	/// Persist toggle.
	saving_enabled: bool,
	/// Root disk cache directory (defaults to the process-wide default;
	/// project-owned caches re-point it through the node bridge later).
	disk_dir: String,
	/// mtime of the last loaded state file (skip reloads of unchanged files).
	last_loaded_state: i64,
	/// External mutex exposed to the C ABI `oakrender_cache_lock/unlock`
	/// (C++ `PlaybackCache::mutex()`; always pair the calls).
	pub lock: Mutex<()>,
}

impl PlaybackCache {
	/// New cache for `owner` (C++ `PlaybackCache(parent)`).
	pub fn new(kind: CacheKind, owner: OwnerIdentity) -> Self {
		let disk_dir = oak_core::commonutil::default_disk_cache_path();
		Self {
			kind,
			owner,
			uuid: create_uuid_text(),
			timebase: None,
			validated: TimeRangeList::new(),
			requested: TimeRangeList::new(),
			passthroughs: Vec::new(),
			saving_enabled: true,
			disk_dir,
			last_loaded_state: 0,
			lock: Mutex::new(()),
		}
	}

	/// The cache UUID text.
	pub fn uuid(&self) -> &str {
		&self.uuid
	}

	/// Set the UUID and reload the disk state (C++ `set_uuid`).
	pub fn set_uuid(&mut self, uuid: &str) {
		self.uuid = uuid.to_string();
		let dir = self.disk_dir.clone();
		let _ = self.load_state(&std::path::Path::new(&dir));
	}

	/// Set the frame timebase (frame-hash flavors; C++ `set_timebase`).
	pub fn set_timebase(&mut self, tb: Rational) {
		self.timebase = Some(tb);
	}

	/// The frame timebase (null when unset).
	pub fn timebase(&self) -> Rational {
		self.timebase.unwrap_or(Rational::NULL)
	}

	/// Root disk cache directory.
	pub fn disk_dir(&self) -> &str {
		&self.disk_dir
	}

	/// Re-point the disk cache root (project-owned caches).
	pub fn set_disk_dir(&mut self, dir: &str) {
		self.disk_dir = dir.to_string();
	}

	/// Mark a range invalid (C++ `invalidate`).
	pub fn invalidate(&mut self, range: TimeRange) {
		if range.in_() == range.out() {
			eprintln!("Tried to invalidate zero-length range");
			return;
		}
		self.validated.remove(range);
		self.passthroughs.retain(|(r, _)| !overlaps(*r, range));
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(&std::path::Path::new(&dir));
		}
	}

	/// Mark a range valid (C++ `validate`).
	pub fn validate(&mut self, range: TimeRange) {
		self.validated.insert(range);
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(&std::path::Path::new(&dir));
		}
	}

	/// True when any validated range exists (C++
	/// `has_validated_ranges`).
	pub fn has_validated_ranges(&self) -> bool {
		!self.validated.is_empty()
	}

	/// The validated ranges (C++ `get_validated_ranges`).
	pub fn validated_ranges(&self) -> &TimeRangeList {
		&self.validated
	}

	/// Invalidated sub-ranges of `within` (C++
	/// `get_invalidated_ranges`).
	pub fn invalidated_ranges(&self, within: TimeRange) -> TimeRangeList {
		// Clamp to >= 0 (C++ does this for safety).
		let zero = Rational::new(0, 1);
		let mut in_ = within.in_();
		let mut out = within.out();
		if in_ < zero {
			in_ = zero;
		}
		if out < zero {
			out = zero;
		}
		let intersecting = TimeRange::new(in_, out);

		let mut invalidated = TimeRangeList::new();
		invalidated.insert(intersecting);

		for range in self.validated.ranges() {
			invalidated.remove(*range);
		}
		for (range, _) in &self.passthroughs {
			invalidated.remove(*range);
		}
		invalidated
	}

	/// Record a request (C++ `request`).
	pub fn request(&mut self, range: TimeRange) {
		self.requested.insert(range);
	}

	/// The requested-but-not-yet-validated ranges.
	pub fn requested_ranges(&self) -> &TimeRangeList {
		&self.requested
	}

	/// Clear a requested range (C++ `clear_request_range`).
	pub fn clear_request_range(&mut self, range: TimeRange) {
		self.requested.remove(range);
	}

	/// Passthrough link (C++ `set_passthrough`).
	pub fn set_passthrough(&mut self, other: &PlaybackCache) {
		for range in other.validated.ranges() {
			self.passthroughs.push((*range, other.uuid.clone()));
		}
		for (range, uuid) in &other.passthroughs {
			self.passthroughs.push((*range, uuid.clone()));
		}
		// FrameHashCache::set_passthrough also adopts the source timebase.
		if self.kind.is_frame_hash() {
			if let Some(tb) = other.timebase {
				self.timebase = Some(tb);
			}
		}
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(&std::path::Path::new(&dir));
		}
	}

	/// Passthrough from a snapshot (avoids aliasing when the two handles
	/// may refer to the same cache).
	pub fn set_passthrough_snapshot(&mut self, snapshot: PassthroughSnapshot) {
		for range in snapshot.validated.ranges() {
			self.passthroughs.push((*range, snapshot.uuid.clone()));
		}
		for (range, uuid) in &snapshot.passthroughs {
			self.passthroughs.push((*range, uuid.clone()));
		}
		if self.kind.is_frame_hash() {
			if let Some(tb) = snapshot.timebase {
				self.timebase = Some(tb);
			}
		}
		if self.saving_enabled {
			let dir = self.disk_dir.clone();
			let _ = self.save_state(&std::path::Path::new(&dir));
		}
	}

	/// The passthrough ranges (C++ `get_passthroughs`).
	pub fn passthroughs(&self) -> &[(TimeRange, String)] {
		&self.passthroughs
	}

	/// Persist toggle (C++ `set_saving_enabled`).
	pub fn set_saving_enabled(&mut self, enabled: bool) {
		self.saving_enabled = enabled;
	}

	/// The persist toggle.
	pub fn saving_enabled(&self) -> bool {
		self.saving_enabled
	}

	/// `<cache_dir>/<uuid>` (C++ `get_this_cache_directory`).
	pub fn cache_directory(&self, cache_dir: &std::path::Path) -> std::path::PathBuf {
		cache_dir.join(&self.uuid)
	}

	/// Disk state load (C++ `load_state`); `cache_dir` is the root cache
	/// directory. Missing state clears the ranges (C++ behavior).
	pub fn load_state(&mut self, cache_dir: &std::path::Path) -> Result<()> {
		let state_path = self.cache_directory(cache_dir).join("state");
		if !state_path.exists() {
			self.validated = TimeRangeList::new();
			self.passthroughs.clear();
			return Ok(());
		}

		let file_time = modification_time_msecs(&state_path);
		if file_time <= self.last_loaded_state {
			return Ok(());
		}

		let data = match std::fs::read(&state_path) {
			Ok(d) => d,
			Err(e) => return Err(Error::Failed(format!("read state: {e}"))),
		};
		let mut r = ByteReader::new(&data);

		let version = r.read_u32();
		if self.kind.is_frame_hash() {
			// FrameHashCache::LoadStateEvent
			let event_version = r.read_u32();
			if event_version == 1 {
				let num = r.read_i32();
				let den = r.read_i32();
				if num > 0 && den > 0 {
					self.timebase = Some(Rational::new(num as i64, den as i64));
				}
			}
		}

		if version == 1 {
			let valid_count = r.read_i32();
			for _ in 0..valid_count {
				let in_num = r.read_i32() as i64;
				let in_den = r.read_i32() as i64;
				let out_num = r.read_i32() as i64;
				let out_den = r.read_i32() as i64;
				self.validated.insert(TimeRange::new(
					Rational::new(in_num, in_den),
					Rational::new(out_num, out_den),
				));
			}

			let pass_count = r.read_i32();
			for _ in 0..pass_count {
				let in_num = r.read_i32() as i64;
				let in_den = r.read_i32() as i64;
				let out_num = r.read_i32() as i64;
				let out_den = r.read_i32() as i64;
				let uuid = bytes_to_uuid_text(&r.read_uuid());
				self.passthroughs.push((
					TimeRange::new(
						Rational::new(in_num, in_den),
						Rational::new(out_num, out_den),
					),
					uuid,
				));
			}
		}

		self.last_loaded_state = file_time;
		Ok(())
	}

	/// See [`PlaybackCache::load_state`].
	pub fn save_state(&self, cache_dir: &std::path::Path) -> Result<()> {
		let dir = self.cache_directory(cache_dir);
		let state_path = dir.join("state");

		if self.validated.is_empty() && self.passthroughs.is_empty() {
			let _ = std::fs::remove_file(&state_path);
			return Ok(());
		}

		std::fs::create_dir_all(&dir)
			.map_err(|e| Error::Failed(format!("create cache dir: {e}")))?;

		let mut out = Vec::new();
		write_be_u32(&mut out, 1); // PlaybackCache version
		if self.kind.is_frame_hash() {
			// FrameHashCache::SaveStateEvent
			write_be_u32(&mut out, 1);
			let tb = self.timebase.unwrap_or(Rational::new(1, 1));
			write_be_i32(&mut out, tb.numerator() as i32);
			write_be_i32(&mut out, tb.denominator() as i32);
		}

		write_be_i32(&mut out, self.validated.ranges().len() as i32);
		for range in self.validated.ranges() {
			write_be_i32(&mut out, range.in_().numerator() as i32);
			write_be_i32(&mut out, range.in_().denominator() as i32);
			write_be_i32(&mut out, range.out().numerator() as i32);
			write_be_i32(&mut out, range.out().denominator() as i32);
		}

		write_be_i32(&mut out, self.passthroughs.len() as i32);
		for (range, uuid) in &self.passthroughs {
			write_be_i32(&mut out, range.in_().numerator() as i32);
			write_be_i32(&mut out, range.in_().denominator() as i32);
			write_be_i32(&mut out, range.out().numerator() as i32);
			write_be_i32(&mut out, range.out().denominator() as i32);
			out.extend_from_slice(&uuid_text_to_bytes(uuid));
		}

		let mut file = std::fs::File::create(&state_path)
			.map_err(|e| Error::Failed(format!("create state: {e}")))?;
		file.write_all(&out)
			.map_err(|e| Error::Failed(format!("write state: {e}")))?;
		file.flush()
			.map_err(|e| Error::Failed(format!("flush state: {e}")))?;
		Ok(())
	}

	/// The on-disk filename for a frame time (C++
	/// `FrameHashCache::get_valid_cache_filename`). `None` when the frame is
	/// not cached and no passthrough covers the time.
	pub fn frame_filename(&self, time: Rational) -> Option<String> {
		if !self.kind.is_frame_hash() {
			return None;
		}
		if is_cached_at(&self.validated, time) {
			return Some(self.cache_path_name(time, &self.uuid));
		}
		for (range, uuid) in &self.passthroughs {
			if range.contains(time) {
				return Some(self.cache_path_name(time, uuid));
			}
		}
		None
	}

	/// `cache_path_name(time)`: `<disk_dir>/<uuid>/<timestamp>` where the
	/// timestamp is `time_to_timestamp(time, tb, k_round)`.
	fn cache_path_name(&self, time: Rational, uuid: &str) -> String {
		let timestamp = match self.timebase {
			Some(tb) => tb.time_to_timestamp(time),
			// No valid timebase: whole seconds.
			None => time.to_f64().round() as i64,
		};
		std::path::Path::new(&self.disk_dir)
			.join(uuid)
			.join(timestamp.to_string())
			.to_string_lossy()
			.into_owned()
	}

	/// The static `cache_path_name(cache_path, cache_id, time, tb)`
	/// variant used by the FFI frame-cache load/save exports.
	pub fn frame_cache_path(
		cache_path: &str,
		cache_id: &str,
		time: Rational,
		timebase: Rational,
	) -> String {
		let timestamp = timebase.time_to_timestamp(time);
		std::path::Path::new(cache_path)
			.join(cache_id)
			.join(timestamp.to_string())
			.to_string_lossy()
			.into_owned()
	}
}

/// Half-open overlap (C++ `TimeRange::overlaps_with`, default inclusivity).
fn overlaps(a: TimeRange, b: TimeRange) -> bool {
	!(b.out() <= a.in_() || b.in_() >= a.out())
}

/// An owned snapshot of another cache's passthrough-relevant data
/// (validated ranges, passthroughs, timebase, uuid) so `set_passthrough`
/// cannot alias.
#[derive(Clone, Debug)]
pub struct PassthroughSnapshot {
	/// The source's validated ranges.
	pub validated: TimeRangeList,
	/// The source's passthroughs.
	pub passthroughs: Vec<(TimeRange, String)>,
	/// The source's timebase.
	pub timebase: Option<Rational>,
	/// The source's uuid.
	pub uuid: String,
}

/// True when `t` lies in any range (C++ `TimeRangeList::contains(Rational)`).
fn is_cached_at(list: &TimeRangeList, t: Rational) -> bool {
	list.ranges().iter().any(|r| r.contains(t))
}

/// Monotonic identity counter for caches without a real node identity.
static NEXT_CACHE_ID: AtomicU64 = AtomicU64::new(1);

/// A synthetic owner identity for detached caches (never collides with
/// node identities, which are pointer values).
pub fn next_owner_identity() -> OwnerIdentity {
	NEXT_CACHE_ID.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tb_cache() -> PlaybackCache {
		let mut c = PlaybackCache::new(CacheKind::VideoFrame, 1);
		c.set_timebase(Rational::new(1, 30));
		c.set_saving_enabled(false);
		c
	}

	#[test]
	fn uuid_is_canonical_v4() {
		let u = create_uuid_text();
		assert_eq!(u.len(), 38);
		assert!(u.starts_with('{') && u.ends_with('}'));
		assert_eq!(u.chars().filter(|&c| c == '-').count(), 4);
		// version nibble at position 15 ("-4...").
		let b = u.as_bytes();
		assert_eq!(b[15], b'4');
		// variant nibble at position 20.
		let v = (b[20] as char).to_digit(16).unwrap();
		assert!(v == 8 || v == 9 || v == 10 || v == 11);
	}

	#[test]
	fn uuid_text_bytes_roundtrip() {
		let u = create_uuid_text();
		let b = uuid_text_to_bytes(&u);
		assert_eq!(bytes_to_uuid_text(&b), u);
	}

	#[test]
	fn invalidate_validate_roundtrip() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(10, 1));
		c.validate(r);
		assert!(c.has_validated_ranges());
		assert_eq!(c.invalidated_ranges(r).ranges().len(), 0);
		c.invalidate(TimeRange::new(Rational::new(4, 1), Rational::new(6, 1)));
		// C++ semantics: invalidated = intersecting − validated − passthrough.
		let inv = c.invalidated_ranges(r);
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(4, 1));
		assert_eq!(inv.ranges()[0].out(), Rational::new(6, 1));
		// The validated list still covers the two surviving sub-ranges.
		assert_eq!(c.validated_ranges().ranges().len(), 2);
	}

	#[test]
	fn zero_length_invalidate_is_rejected() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(5, 1));
		c.validate(r);
		c.invalidate(TimeRange::new(Rational::new(2, 1), Rational::new(2, 1)));
		assert!(
			c.has_validated_ranges(),
			"zero-length invalidate is a no-op"
		);
	}

	#[test]
	fn invalidated_ranges_clamps_below_zero() {
		let mut c = tb_cache();
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));
		let inv = c.invalidated_ranges(TimeRange::new(Rational::new(-5, 1), Rational::new(10, 1)));
		// Only [5,10) remains after the clamp + validation removal.
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(5, 1));
		assert_eq!(inv.ranges()[0].out(), Rational::new(10, 1));
	}

	#[test]
	fn passthrough_excludes_ranges() {
		let mut a = tb_cache();
		let mut b = PlaybackCache::new(CacheKind::VideoFrame, 2);
		b.set_timebase(Rational::new(1, 30));
		b.set_saving_enabled(false);
		b.validate(TimeRange::new(Rational::new(0, 1), Rational::new(5, 1)));

		a.set_passthrough(&b);
		assert_eq!(a.passthroughs().len(), 1);
		assert_eq!(a.passthroughs()[0].1, b.uuid);

		let inv = a.invalidated_ranges(TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)));
		assert_eq!(inv.ranges().len(), 1);
		assert_eq!(inv.ranges()[0].in_(), Rational::new(5, 1));

		// Invalidate a range overlapping the passthrough: it is unlinked.
		a.invalidate(TimeRange::new(Rational::new(2, 1), Rational::new(8, 1)));
		assert!(a.passthroughs().is_empty());
	}

	#[test]
	fn frame_filename_parity_scheme() {
		let mut c = tb_cache();
		c.set_uuid("{01234567-89ab-cdef-0123-456789abcdef}");
		let time = Rational::new(1, 2); // 0.5 s at 30fps → frame 15
		c.validate(TimeRange::new(time, time + Rational::new(1, 30)));
		let dir = std::env::temp_dir();
		c.set_disk_dir(&dir.to_string_lossy());
		let name = c.frame_filename(time).unwrap();
		assert_eq!(
			name,
			dir.join("{01234567-89ab-cdef-0123-456789abcdef}")
				.join("15")
				.to_string_lossy()
		);
	}

	#[test]
	fn frame_filename_none_when_not_cached() {
		let c = tb_cache();
		assert!(c.frame_filename(Rational::new(1, 30)).is_none());
	}

	#[test]
	fn audio_kind_has_no_frame_filename() {
		let c = PlaybackCache::new(CacheKind::AudioPlayback, 1);
		assert!(c.frame_filename(Rational::new(1, 1)).is_none());
	}

	#[test]
	fn request_ranges_and_clear() {
		let mut c = tb_cache();
		let r = TimeRange::new(Rational::new(0, 1), Rational::new(5, 1));
		c.request(r);
		assert_eq!(c.requested_ranges().ranges().len(), 1);
		c.request(TimeRange::new(Rational::new(3, 1), Rational::new(8, 1)));
		assert_eq!(c.requested_ranges().ranges().len(), 1, "merges on insert");
		assert_eq!(c.requested_ranges().ranges()[0].in_(), Rational::new(0, 1));
		c.clear_request_range(TimeRange::new(Rational::new(2, 1), Rational::new(3, 1)));
		assert_eq!(c.requested_ranges().ranges().len(), 2, "splits on clear");
	}

	#[test]
	fn audio_cache_disk_state_uses_base_format() {
		let dir =
			std::env::temp_dir().join(format!("oakrender-audio-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();
		let mut c = PlaybackCache::new(CacheKind::AudioPlayback, 1);
		c.set_saving_enabled(false);
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(2, 1)));
		c.save_state(&dir).unwrap();
		// Base format: no timebase block (audio has no frame-hash event).
		let bytes = std::fs::read(dir.join(&c.uuid).join("state")).unwrap();
		let mut expect = Vec::new();
		write_be_u32(&mut expect, 1); // version
		write_be_i32(&mut expect, 1); // valid_count
		write_be_i32(&mut expect, 0);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 2);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 0); // pass_count
		assert_eq!(bytes, expect);
		// frame_filename is not available for audio kinds.
		assert!(c.frame_filename(Rational::new(0, 1)).is_none());
		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn cache_directory_helper() {
		let c = tb_cache();
		let dir = std::path::Path::new("/tmp/cache");
		assert_eq!(c.cache_directory(dir), dir.join(&c.uuid));
	}

	#[test]
	fn disk_state_roundtrip_binary_parity() {
		let dir = std::env::temp_dir().join(format!("oakrender-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();

		let mut c = tb_cache();
		c.set_timebase(Rational::new(1, 30));
		c.set_saving_enabled(true);
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(10, 1)));
		c.validate(TimeRange::new(Rational::new(20, 1), Rational::new(30, 1)));
		c.save_state(&dir).unwrap();

		// Byte layout: ver=1, ver2=1, tb=1/30, count=2, 2×4×i32, count=0.
		let bytes = std::fs::read(dir.join(&c.uuid).join("state")).unwrap();
		let mut expect = Vec::new();
		write_be_u32(&mut expect, 1);
		write_be_u32(&mut expect, 1);
		write_be_i32(&mut expect, 1);
		write_be_i32(&mut expect, 30);
		write_be_i32(&mut expect, 2);
		for (i, o) in [(0i32, 10i32), (20, 30)] {
			write_be_i32(&mut expect, i);
			write_be_i32(&mut expect, 1);
			write_be_i32(&mut expect, o);
			write_be_i32(&mut expect, 1);
		}
		write_be_i32(&mut expect, 0);
		assert_eq!(bytes, expect, "C++ binary state layout parity");

		let mut c2 = PlaybackCache::new(CacheKind::VideoFrame, 99);
		c2.set_uuid(&c.uuid.clone());
		c2.set_timebase(Rational::new(1, 30));
		c2.set_saving_enabled(false);
		c2.load_state(&dir).unwrap();
		assert_eq!(c2.validated.ranges().len(), 2);
		assert_eq!(
			c2.validated.ranges()[0],
			TimeRange::new(Rational::new(0, 1), Rational::new(10, 1))
		);
		assert_eq!(
			c2.validated.ranges()[1],
			TimeRange::new(Rational::new(20, 1), Rational::new(30, 1))
		);

		// Re-save from the loaded cache → identical bytes.
		c2.set_saving_enabled(true);
		c2.save_state(&dir).unwrap();
		let bytes2 = std::fs::read(dir.join(&c2.uuid).join("state")).unwrap();
		assert_eq!(bytes, bytes2);

		std::fs::remove_dir_all(&dir).ok();
	}

	#[test]
	fn save_state_removes_file_when_empty() {
		let dir = std::env::temp_dir().join(format!("oakrender-test-{}", next_owner_identity()));
		std::fs::create_dir_all(&dir).unwrap();
		let mut c = tb_cache();
		c.set_saving_enabled(true);
		c.set_disk_dir(&dir.to_string_lossy());
		c.validate(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		c.save_state(&dir).unwrap();
		assert!(dir.join(&c.uuid).join("state").exists());
		c.invalidate(TimeRange::new(Rational::new(0, 1), Rational::new(1, 1)));
		assert!(!dir.join(&c.uuid).join("state").exists());
		std::fs::remove_dir_all(&dir).ok();
	}
}
