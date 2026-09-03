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

//! Color processing: OCIO processors and the process-wide default config
//! (C++ `ColorProcessor` + `ColorManager` statics).
//!
//! Implemented over the `ocio-rs` crate (safe Rust bindings for
//! OpenColorIO v2.5.2, bundled real-OCIO build). OCIO is never rewritten —
//! this module maps the C++ call surface onto ocio-rs.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::PixelFormat;

use crate::error::{Error, Result};
use crate::texture::Frame;

/// A color processor (wraps an OCIO processor). A processor whose OCIO
/// lookup failed holds `None` — conversions then pass through, mirroring
/// the C++ non-fatal creation behavior.
pub struct ColorProcessor {
	/// The OCIO processor (None = pass-through).
	inner: Option<ocio_rs::Processor>,
	/// The cached CPU processor used for pixel conversions (C++
	/// `cpu_processor_`).
	cpu: Option<ocio_rs::CPUProcessor>,
}

/// The R/B channel swap as a 4x4 matrix transform (baked around the
/// display chain so BGRA buffers can be transformed through OCIO's RGBA
/// entry points).
fn rb_swap_matrix() -> Option<ocio_rs::transform::MatrixTransform> {
	let m = ocio_rs::transform::MatrixTransform::create().ok()?;
	m.set_matrix(&[
		0.0, 0.0, 1.0, 0.0, //
		0.0, 1.0, 0.0, 0.0, //
		1.0, 0.0, 0.0, 0.0, //
		0.0, 0.0, 0.0, 1.0,
	])
	.ok()?;
	Some(m)
}

/// Processor direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
	/// src -> dst.
	Normal,
	/// dst -> src.
	Inverse,
}

impl Direction {
	/// The ocio-rs transform direction for the swap already applied at the
	/// call site.
	fn to_ocio(self) -> ocio_rs::TransformDirection {
		match self {
			Direction::Normal => ocio_rs::TransformDirection::Forward,
			Direction::Inverse => ocio_rs::TransformDirection::Inverse,
		}
	}
}

// OCIO Config/Processor are refcounted handles over C++ objects that are
// thread-safe for CPU processing (the C++ ColorProcessor is shared across
// render threads the same way); the safe wrapper is Send+Sync.
unsafe impl Send for ColorProcessor {}
unsafe impl Sync for ColorProcessor {}

impl ColorProcessor {
	/// Create from two colorspace names on the default config
	/// (C++ `ColorProcessor::create(config, input, dest)`).
	///
	/// Role names (e.g. "scene_linear") are resolved to canonical names.
	/// OCIO failures are non-fatal: a processor with `inner == None` is
	/// returned and conversions pass through.
	pub fn create(src_space: &str, dst_transform: &str, dir: Direction) -> Option<Self> {
		let config = default_config()?;
		let src = if config.has_role(src_space) {
			config
				.canonical_name(src_space)
				.unwrap_or_else(|| src_space.to_string())
		} else {
			src_space.to_string()
		};

		let processor = match dir {
			Direction::Normal => config.processor(&src, dst_transform),
			Direction::Inverse => config.processor(dst_transform, &src),
		}
		.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());

		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create from a LUT file (C++ `create_lut` semantics): an OCIO
	/// FileTransform with linear interpolation on the default config.
	pub fn create_lut(path: &str, dir: Direction) -> Option<Self> {
		let config = default_config()?;
		let transform = ocio_rs::transform::FileTransform::create().ok()?;
		transform.set_src(path).ok()?;
		transform.set_interpolation(ocio_rs::Interpolation::Linear);
		transform.set_direction(dir.to_ocio());
		let processor = config
			.processor_from_transform(&transform, dir.to_ocio())
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create from a LUT file on a specific config (C++
	/// `ColorProcessor::create_lut` family with a ColorManager config).
	pub fn create_lut_on(config: &ocio_rs::Config, path: &str, dir: Direction) -> Option<Self> {
		let transform = ocio_rs::transform::FileTransform::create().ok()?;
		transform.set_src(path).ok()?;
		transform.set_interpolation(ocio_rs::Interpolation::Linear);
		transform.set_direction(dir.to_ocio());
		let processor = config
			.processor_from_transform(&transform, dir.to_ocio())
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create a dynamic grading-primary processor on the default config
	/// (C++ `ColorProcessor` grading-primary family).
	pub fn create_grading_primary(style: GradingStyle) -> Option<Self> {
		let config = default_config()?;
		let transform =
			ocio_rs::transform::GradingPrimaryTransform::create(style.to_ocio()).ok()?;
		transform.make_dynamic();
		transform.set_direction(ocio_rs::TransformDirection::Forward);
		let processor = config
			.processor_from_transform(&transform, ocio_rs::TransformDirection::Forward)
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create the display-output processor from a display ICC profile
	/// (macOS ColorSync / Windows ICM / Linux colord or `_ICC_PROFILE`).
	///
	/// OCIO's ICC reader (FileFormatICC, display-class profiles) builds the
	/// FileTransform's forward direction as "CIE XYZ (D65-adapted PCS) →
	/// device code values", so the chain is `<src_space> → linear Rec.709 →
	/// CIE XYZ D65 → ICC forward`. `src_space` is a colorspace or role of
	/// the default config (the pipeline reference, e.g. "scene_linear").
	pub fn create_display_icc(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::create_display_icc_impl(src_space, icc_path, false)
	}

	/// Create the display-output processor for BGRA8 buffers (the viewer's
	/// wire format): the [`create_display_icc`](Self::create_display_icc)
	/// chain with R/B-swapping matrices baked around it, so BGRA bytes are
	/// transformed in place through OCIO's RGBA entry points (swap∘chain∘swap
	/// is the identity-wrapped chain evaluated on swapped channels).
	pub fn create_display_icc_bgra8(src_space: &str, icc_path: &str) -> Option<Self> {
		Self::create_display_icc_impl(src_space, icc_path, true)
	}

	/// Create the display-output processor for content the caller has already
	/// converted to CIE XYZ (D65, unit luminance) — the non-sRGB project
	/// output gamut path. [`create_display_icc`](Self::create_display_icc)
	/// starts from an OCIO named space (sRGB and friends); P3/BT.2020
	/// targets have no named space in the builtin configs, so the caller
	/// linearizes and gamut-maps to XYZ itself
	/// (`crate::colormath::output_spec_to_xyz_d65`) and this chain only
	/// needs the ICC half: it runs the existing builder with the config's
	/// `cie_xyz_d65_interchange` role as the source space (XYZ → linear
	/// Rec.709, whose inverse the builder's leg 2 immediately undoes — a
	/// no-op round trip, leaving the ICC FileTransform to map the XYZ PCS
	/// to device values).
	///
	/// Returns `None` (not a pass-through) when the chain cannot build — e.g.
	/// the config rejects the interchange role — so callers can fall back to
	/// the sRGB chain instead of silently mapping wrong content.
	pub fn create_display_icc_xyz(icc_path: &str) -> Option<Self> {
		Self::create_display_icc_xyz_impl(icc_path, false)
	}

	/// The [`create_display_icc_xyz`](Self::create_display_icc_xyz) chain
	/// with R/B-swapping matrices baked around it for BGRA8 buffers (see
	/// [`create_display_icc_bgra8`](Self::create_display_icc_bgra8)).
	pub fn create_display_icc_xyz_bgra8(icc_path: &str) -> Option<Self> {
		Self::create_display_icc_xyz_impl(icc_path, true)
	}

	fn create_display_icc_xyz_impl(icc_path: &str, bgra: bool) -> Option<Self> {
		let p = Self::create_display_icc_impl("cie_xyz_d65_interchange", icc_path, bgra)?;
		// A failed OCIO lookup yields a pass-through processor (the
		// non-fatal convention above); for the XYZ chain that must surface as
		// `None` so the caller can fall back to the sRGB degradation instead
		// of feeding XYZ values through a no-op.
		p.is_valid().then_some(p)
	}

	/// Shared builder: `bgra` wraps the chain in R/B swap matrices.
	fn create_display_icc_impl(src_space: &str, icc_path: &str, bgra: bool) -> Option<Self> {
		let config = default_config()?;
		// Leg 1: pipeline space -> linear Rec.709 (sRGB primaries, D65).
		let to_lin709 = ocio_rs::transform::ColorSpaceTransform::create().ok()?;
		to_lin709.set_src(src_space).ok()?;
		to_lin709.set_dst("Linear Rec.709 (sRGB)").ok()?;
		// Leg 2: linear Rec.709 -> CIE XYZ D65 (the ICC connection space as
		// OCIO's ICC reader adapts it, D50->D65 Bradford baked in). The
		// builtin configs carry no XYZ colorspace, so the conversion is an
		// explicit matrix (sRGB/Rec.709 primaries -> XYZ D65).
		let to_xyz = ocio_rs::transform::MatrixTransform::create().ok()?;
		to_xyz
			.set_matrix(&[
				0.4123908, 0.3575843, 0.1804808, 0.0, //
				0.2126390, 0.7151687, 0.0721923, 0.0, //
				0.0193308, 0.1191948, 0.9505322, 0.0, //
				0.0, 0.0, 0.0, 1.0,
			])
			.ok()?;
		// Leg 3: XYZ D65 -> device, per the ICC profile (OCIO's
		// FileFormatICC forward direction).
		let icc = ocio_rs::transform::FileTransform::create().ok()?;
		icc.set_src(icc_path).ok()?;
		icc.set_interpolation(ocio_rs::Interpolation::Linear);
		icc.set_direction(ocio_rs::TransformDirection::Forward);
		let group = ocio_rs::transform::GroupTransform::create().ok()?;
		if bgra {
			group.append_transform(&rb_swap_matrix()?).ok()?;
		}
		group.append_transform(&to_lin709).ok()?;
		group.append_transform(&to_xyz).ok()?;
		group.append_transform(&icc).ok()?;
		if bgra {
			group.append_transform(&rb_swap_matrix()?).ok()?;
		}
		let processor = config
			.processor_from_transform(&group, ocio_rs::TransformDirection::Forward)
			.ok();
		let cpu = processor
			.as_ref()
			.and_then(|p| p.default_cpu_processor().ok());
		Some(Self {
			inner: processor,
			cpu,
		})
	}

	/// Create from an explicit OCIO processor (C++
	/// `ColorProcessor::create(ConstProcessorRcPtr)`).
	pub fn from_processor(processor: ocio_rs::Processor) -> Self {
		let cpu = processor.default_cpu_processor().ok();
		Self {
			inner: Some(processor),
			cpu,
		}
	}

	/// Create a pass-through processor (invalid OCIO processor).
	pub fn pass_through() -> Self {
		Self {
			inner: None,
			cpu: None,
		}
	}

	/// The underlying OCIO processor handle, when valid.
	pub fn processor(&self) -> Option<&ocio_rs::Processor> {
		self.inner.as_ref()
	}

	/// Convert one RGBA color (C++ `convert_color`).
	pub fn convert_color(&self, rgba: [f64; 4]) -> [f64; 4] {
		match &self.cpu {
			Some(cpu) => {
				let mut c = [
					rgba[0] as f32,
					rgba[1] as f32,
					rgba[2] as f32,
					rgba[3] as f32,
				];
				cpu.apply_rgba(&mut c);
				[c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64]
			}
			None => rgba,
		}
	}

	/// Convert a whole F32 frame in place (row-major RGBA).
	pub fn convert_frame(&self, frame: &mut Frame) -> Result<()> {		let Some(cpu) = &self.cpu else {
			return Ok(()); // pass-through
		};
		if frame.format != PixelFormat::F32 {
			return Err(Error::Invalid);
		}
		// Tightly packed RGBA: stride is 4 f32 elements per pixel
		// (ocio-rs counts elements, not bytes).
		let pixels = frame.pixel_count() as i64;
		let buf: &mut [f32] = bytemuck_f32_slice(&mut frame.data)
			.ok_or_else(|| Error::Failed("pixel buffer not f32 aligned".to_string()))?;
		cpu.apply_rgba_pixels(buf, pixels, 4);
		Ok(())
	}

	/// True when the underlying OCIO processor is valid (C++
	/// `get_processor() != null`).
	pub fn is_valid(&self) -> bool {
		self.inner.is_some()
	}

	/// The OCIO processor cache id (C++ `ColorProcessor::id()`), or the
	/// empty string for a pass-through processor.
	pub fn cache_id(&self) -> String {
		match &self.inner {
			Some(p) => p.cache_id().unwrap_or_default(),
			None => String::new(),
		}
	}

	/// Convert a packed BGRA8 buffer in place (the viewer wire format).
	/// The processor must have been built with
	/// [`create_display_icc_bgra8`](Self::create_display_icc_bgra8) — the
	/// R/B swizzle is baked into the chain, so the bytes go through OCIO's
	/// RGBA entry point unchanged. A pass-through processor is a no-op.
	///
	/// The conversion detours through F32: the default CPU processor is
	/// F32-finalized and rejects packed u8 buffers with a bit-depth
	/// mismatch, and the ocio-rs binding exposes no Uint8-finalized CPU
	/// processor. The round-trip cost is one small staging buffer.
	pub fn convert_bgra8(&self, data: &mut [u8], pixels: i64) -> Result<()> {
		let Some(cpu) = &self.cpu else {
			return Ok(());
		};
		let count = (pixels.max(0) as usize) * 4;
		if data.len() < count {
			return Err(Error::Invalid);
		}
		let mut f32s: Vec<f32> =
			data[..count].iter().map(|&v| v as f32 / 255.0).collect();
		cpu.try_apply_rgba_pixels(&mut f32s, pixels, 4)
			.map_err(|e| Error::Failed(format!("OCIO f32 apply (bgra8 detour): {e}")))?;
		for (dst, v) in data[..count].iter_mut().zip(f32s) {
			*dst = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
		}
		Ok(())
	}

	/// Convert an F32 RGBA buffer in place (tightly packed, 4 floats per
	/// pixel). A pass-through processor is a no-op.
	pub fn convert_f32_rgba(&self, samples: &mut [f32], pixels: i64) -> Result<()> {
		let Some(cpu) = &self.cpu else {
			return Ok(());
		};
		cpu.try_apply_rgba_pixels(samples, pixels, 4)
			.map_err(|e| Error::Failed(format!("OCIO f32 apply: {e}")))
	}
}

/// Grading-primary transform style (mirrors the C++ enum values: LIN=0,
/// LOG=1 in `include/render/color.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradingStyle {
	/// OCIO GRADING_LIN.
	Lin,
	/// OCIO GRADING_LOG.
	Log,
}

impl GradingStyle {
	fn to_ocio(self) -> ocio_rs::GradingStyle {
		match self {
			GradingStyle::Lin => ocio_rs::GradingStyle::Lin,
			GradingStyle::Log => ocio_rs::GradingStyle::Log,
		}
	}
}

/// Reinterpret a byte buffer as f32 when its length is a multiple of 4
/// (the pipeline guarantees F32 RGBA frames, so alignment is exact).
fn bytemuck_f32_slice(data: &mut [u8]) -> Option<&mut [f32]> {
	if data.len() % 4 != 0 {
		return None;
	}
	// SAFETY: length is a multiple of 4 and `data` is byte-aligned; the
	// cast is valid for any alignment (f32 has no stricter requirement
	// than u8 for mutable slice casts at this length).
	Some(unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut f32, data.len() / 4) })
}

// ---- process-wide default config (C++ ColorManager statics) ----------------

/// The process-wide pipeline color settings (the project properties that
/// drive the ACEScg + F32 pipeline): the working colorspace and the
/// output/delivery spec. Defaults to ACEScg working + sRGB output; the app
/// updates this from the open project's properties (and again on every
/// project-properties commit). Render and export paths read it — a single
/// project is open at a time, so a process global is the same shape as the
/// OCIO default config above.
static PIPELINE_COLOR: LazyLock<Mutex<(crate::colormath::WorkingColorSpace, crate::colormath::OutputColorSpec)>> =
	LazyLock::new(|| Mutex::new((
		crate::colormath::WorkingColorSpace::default(),
		crate::colormath::OutputColorSpec::default(),
	)));

/// Set the pipeline color settings (working space + output spec).
pub fn set_pipeline_color_settings(
	working: crate::colormath::WorkingColorSpace,
	output: crate::colormath::OutputColorSpec,
) {
	*PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()) = (working, output);
}

/// The pipeline working colorspace.
pub fn pipeline_working_space() -> crate::colormath::WorkingColorSpace {
	PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()).0
}

/// The pipeline output/delivery spec.
pub fn pipeline_output_spec() -> crate::colormath::OutputColorSpec {
	PIPELINE_COLOR.lock().unwrap_or_else(|e| e.into_inner()).1
}

/// The pipeline working colorspace as an OFX colorspace name (the value
/// written to `kOfxImageClipPropColourspace` so plugins are told the
/// true space of the pixels they receive — ACEScg in the default pipeline,
/// sRGB in the legacy pass-through mode).
pub fn pipeline_working_ofx_name() -> &'static str {
	match pipeline_working_space() {
		crate::colormath::WorkingColorSpace::AcesCg => "ACEScg",
		crate::colormath::WorkingColorSpace::SrgbLegacy => "sRGB",
	}
}

/// Send+Sync wrapper around `ocio_rs::Config` (a `NonNull`-based handle;
/// the underlying OCIO config is a shared pointer safe for concurrent
/// reads).
pub struct SafeConfig(ocio_rs::Config);
unsafe impl Send for SafeConfig {}
unsafe impl Sync for SafeConfig {}

impl std::ops::Deref for SafeConfig {
	type Target = ocio_rs::Config;
	fn deref(&self) -> &ocio_rs::Config {
		&self.0
	}
}

static DEFAULT_CONFIG: LazyLock<Mutex<Option<std::sync::Arc<SafeConfig>>>> =
	LazyLock::new(|| Mutex::new(None));

/// Borrow the default config (C++ `ColorManager::get_default_config()`).
/// The `Arc` keeps the config alive across threads.
pub fn default_config() -> Option<std::sync::Arc<SafeConfig>> {
	DEFAULT_CONFIG
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.clone()
}

/// Load the process-wide default config from $OCIO or the bundled config
/// (C++ `ColorManager::SetUpDefaultConfig`).
pub fn set_up_default_config() -> Result<()> {
	let env = std::env::var("OCIO").ok().filter(|p| !p.is_empty());
	set_up_default_config_from(env.as_deref())
}

/// Load the process-wide default config from an explicit path (`None` =
/// the bundled default config). The project-properties OCIO override and
/// the `$OCIO` startup path both land here.
pub fn set_up_default_config_from(path: Option<&str>) -> Result<()> {
	let config = match path {
		Some(path) => ocio_rs::Config::from_file(path)
			.map_err(|e| Error::Failed(format!("load OCIO config \"{path}\": {e}")))?,
		None => ocio_rs::Config::create_from_builtin_config("default")
			.or_else(|_| ocio_rs::Config::create_from_builtin_config("ocio-2.2-default"))
			.map_err(|e| Error::Failed(format!("load bundled config: {e}")))?,
	};
	*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) =
		Some(std::sync::Arc::new(SafeConfig(config)));
	Ok(())
}

/// The active default config's display transform cache id for
/// (display, view) — computed from the config's reference colorspace
/// (C++ `ColorManager::display_transform` cache-id semantics).
///
/// `Err(Error::State)` when no default config exists; `Ok(None)` when the
/// display or view is unknown.
pub fn display_transform_result(
	display: &str,
	view: &str,
) -> std::result::Result<Option<String>, Error> {
	let config = default_config().ok_or(Error::State)?;

	let mut display_found = false;
	for i in 0..config.num_displays_all() {
		if config.display_all(i).as_deref() == Some(display) {
			display_found = true;
			break;
		}
	}
	if !display_found {
		return Ok(None);
	}

	let mut view_found = false;
	let views =
		config.num_views_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, display);
	for i in 0..views {
		if config
			.view_by_reference_space(ocio_rs::SearchReferenceSpaceType::Scene, display, i)
			.as_deref()
			== Some(view)
		{
			view_found = true;
			break;
		}
	}
	if !view_found {
		return Ok(None);
	}

	// OCIO's ROLE_REFERENCE role name ("reference"): `get_color_space`
	// resolves name-or-role (C++ `getColorSpace(ROLE_REFERENCE)`); the
	// role-name lookups are the fallback for configs that do not bind it.
	let src = config
		.get_color_space("reference")
		.and_then(|cs| cs.name())
		.filter(|s| !s.is_empty())
		.or_else(|| {
			config
				.role_color_space("reference")
				.filter(|s| !s.is_empty())
		})
		.or_else(|| {
			config
				.role_color_space("aces_interchange")
				.filter(|s| !s.is_empty())
		})
		.or_else(|| {
			config
				.role_color_space("scene_linear")
				.filter(|s| !s.is_empty())
		})
		.ok_or(Error::State)?;
	let processor = config
		.processor_display(src, display, view, ocio_rs::TransformDirection::Forward)
		.map_err(|_| Error::NotFound)?;
	Ok(processor.cache_id())
}

/// [`display_transform_result`] as a plain option (unknown → None).
pub fn display_transform(display: &str, view: &str) -> Option<String> {
	display_transform_result(display, view).ok().flatten()
}

/// The $OCIO path when set, otherwise the extracted default config path
/// (C++ `ColorManager::get_config_path`; the actual file extraction is
/// deferred to the app layer — the Rust default config stays in memory).
pub fn config_path() -> Option<String> {
	if let Ok(path) = std::env::var("OCIO") {
		if !path.is_empty() {
			return Some(path);
		}
	}
	if default_config().is_none() {
		return None;
	}
	Some(format!(
		"{}/ocioconf/config.ocio",
		crate::commonutil::configuration_location()
	))
}

// ---- OCIO GPU function shaders (C++ colormanagement.cpp GetColorContext) --

/// Cache of generated GLSL stubs, keyed by function name + color spaces +
/// config cache id (so a swapped test config re-generates). `None` entries
/// are cached too — the lookup result is deterministic per key.
static OCIO_STUB_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// The GLSL ES 3.0 shader text implementing the OCIO function `fn_name`
/// between `from_space` and `to_space` (both resolved as color-space names
/// or roles, exactly like the C++ `getProcessor(src, dst)` calls).
///
/// This mirrors the C++ OCIO-node shader path: the node's
/// `GenerateProcessor` builds a `ColorTransform` between two spaces and
/// `GetShaderCode` receives the auto-generated stub from
/// `Renderer::GetColorContext` (colormanagement.cpp), which the node then
/// splices into its fragment shader at the `%1` marker.
///
/// `None` when no default config exists or the processor is LUT-based:
/// `extractGpuShaderInfo` reports lookup textures that the C++ renderer
/// uploads per LUT (the loops right after `GetColorContext`), but the Rust
/// renderer has no such upload path, so those transforms are skipped and
/// the node passes through.
pub fn ocio_function_shader(fn_name: &str, from_space: &str, to_space: &str) -> Option<String> {
	let config = default_config()?;
	let cache_key = format!(
		"{}:{}:{}:{}",
		fn_name,
		from_space,
		to_space,
		config.cache_id().unwrap_or_default()
	);
	if let Some(hit) = OCIO_STUB_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&cache_key)
	{
		return hit.clone();
	}
	let stub = build_ocio_function_shader(&config, fn_name, from_space, to_space);
	OCIO_STUB_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(cache_key, stub.clone());
	stub
}

/// Build (but do not cache) the GLSL stub for `fn_name` between the two
/// spaces (C++ `GpuShaderDesc::CreateShaderDesc` + `setLanguage` +
/// `extractGpuShaderInfo`, colormanagement.cpp `GetColorContext`).
fn build_ocio_function_shader(
	config: &SafeConfig,
	fn_name: &str,
	from_space: &str,
	to_space: &str,
) -> Option<String> {
	let processor = config.processor(from_space, to_space).ok()?;
	let gpu = processor.default_gpu_processor().ok()?;
	let mut desc = ocio_rs::GpuShaderDesc::create().ok()?;
	desc.set_language(ocio_rs::GpuLanguage::GlslEs3_0).ok()?;
	desc.set_function_name(fn_name).ok()?;
	desc.set_resource_prefix("ocio_").ok()?;
	gpu.try_extract_shader_info(&mut desc).ok()?;
	// LUT-based processors need their 1D/3D textures uploaded (the C++
	// `GetColorContext` caller loops over `getNum3DTextures`/`getNumTextures`
	// right after extraction); without a LUT upload path these cannot render.
	if desc.num_textures() > 0 || desc.num_3d_textures() > 0 {
		return None;
	}
	desc.shader_text()
}

/// Cache of generated grading GLSL stubs, keyed by style + config cache id
/// (same shape as [`OCIO_STUB_CACHE`]; grading is analytic so the result is
/// deterministic per key).
static OCIO_GRADING_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
	LazyLock::new(|| Mutex::new(HashMap::new()));

/// The GLSL text of the dynamic grading-primary processor for `style`
/// (C++ oakrender creates the same processor via
/// `oakrender_color_processor_create_grading_primary` and the renderer
/// applies it between passes; the Rust equivalent generates the OCIO GPU
/// shader the node splices at its `%1` marker). `None` when no default
/// config exists or the processor is LUT-based (the same guard as
/// [`ocio_function_shader`] — grading is analytic, so this is purely a
/// config-availability check).
pub fn grading_primary_function_shader(style: GradingStyle) -> Option<String> {
	let config = default_config()?;
	let style_id = match style {
		GradingStyle::Lin => "lin",
		GradingStyle::Log => "log",
	};
	let cache_key = format!("{style_id}:{}", config.cache_id().unwrap_or_default());
	if let Some(hit) = OCIO_GRADING_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&cache_key)
	{
		return hit.clone();
	}
	let stub = build_ocio_grading_shader(&config, style);
	OCIO_GRADING_CACHE
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(cache_key, stub.clone());
	stub
}

/// Build (but do not cache) the grading GLSL stub: a dynamic
/// grading-primary transform on `config`, extracted exactly like
/// [`build_ocio_function_shader`].
fn build_ocio_grading_shader(config: &SafeConfig, style: GradingStyle) -> Option<String> {
	let transform = ocio_rs::transform::GradingPrimaryTransform::create(style.to_ocio()).ok()?;
	transform.make_dynamic();
	transform.set_direction(ocio_rs::TransformDirection::Forward);
	let processor = config
		.processor_from_transform(&transform, ocio_rs::TransformDirection::Forward)
		.ok()?;
	let gpu = processor.default_gpu_processor().ok()?;
	let mut desc = ocio_rs::GpuShaderDesc::create().ok()?;
	desc.set_language(ocio_rs::GpuLanguage::GlslEs3_0).ok()?;
	desc.set_function_name("ove_grading_primary").ok()?;
	desc.set_resource_prefix("ocio_").ok()?;
	gpu.try_extract_shader_info(&mut desc).ok()?;
	if desc.num_textures() > 0 || desc.num_3d_textures() > 0 {
		return None;
	}
	desc.shader_text()
}

// ---- LUT library (C++ LUTLibrary) ------------------------------------------

/// Supported LUT extensions (C++ `LUTLibrary::supported_extensions()`).
pub const SUPPORTED_LUT_EXTENSIONS: [&str; 9] = [
	"cube", "3dl", "spi1d", "spi3d", "spimtx", "csp", "clf", "ctf", "cub",
];

/// 1 when `extension` (dot optional, case-insensitive) is supported.
pub fn is_supported_lut_extension(extension: &str) -> bool {
	let ext = extension.strip_prefix('.').unwrap_or(extension);
	SUPPORTED_LUT_EXTENSIONS
		.iter()
		.any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::{Mutex, MutexGuard};

	/// Serializes config-dependent color tests (the process-wide default
	/// config is global).
	static CONFIG_TEST_LOCK: Mutex<()> = Mutex::new(());

	fn config_lock() -> MutexGuard<'static, ()> {
		CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
	}

	#[test]
	fn lut_extensions_case_insensitive_and_dot_optional() {
		assert!(is_supported_lut_extension("cube"));
		assert!(is_supported_lut_extension(".CUBE"));
		assert!(is_supported_lut_extension("clf"));
		assert!(!is_supported_lut_extension("exr"));
		assert!(!is_supported_lut_extension(""));
		assert_eq!(SUPPORTED_LUT_EXTENSIONS.len(), 9);
	}

	#[test]
	fn pass_through_processor_identity() {
		let p = ColorProcessor::pass_through();
		assert!(!p.is_valid());
		assert_eq!(
			p.convert_color([0.25, 0.5, 0.75, 1.0]),
			[0.25, 0.5, 0.75, 1.0]
		);
		let mut f = Frame::dummy();
		assert!(p.convert_frame(&mut f).is_ok());
	}

	#[test]
	fn convert_frame_rejects_non_f32() {
		// A pass-through processor accepts anything (no conversion needed).
		let p = ColorProcessor::pass_through();
		let mut f = Frame::new();
		f.format = PixelFormat::U8;
		assert!(
			p.convert_frame(&mut f).is_ok(),
			"pass-through converts nothing"
		);
		// A *valid* processor requires the F32 pipeline format.
		if set_up_default_config().is_err() {
			return;
		}
		let valid = ColorProcessor::create("scene_linear", "sdr-video", Direction::Normal)
			.and_then(|p| p.is_valid().then_some(p));
		if let Some(valid) = valid {
			let mut f = Frame::new();
			f.format = PixelFormat::U8;
			assert_eq!(
				valid.convert_frame(&mut f).unwrap_err().code(),
				crate::error::OAKCORE_E_INVALID
			);
		}
	}

	#[test]
	fn create_without_config_yields_none() {
		let _lock = config_lock();
		// When no default config has been set up, creation is None.
		let saved = default_config();
		*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = None;
		assert!(ColorProcessor::create("scene_linear", "sdr-video", Direction::Normal).is_none());
		*DEFAULT_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = saved;
	}

	#[test]
	fn default_config_setup_and_role_resolution() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			// Bundled OCIO missing (e.g. stub build): skip.
			return;
		}
		let config = default_config().expect("config set up");
		// The built-in default config carries a scene_linear role.
		assert!(config.has_role("scene_linear") || config.has_role("aces_interchange"));
	}

	#[test]
	fn processor_from_builtin_converts() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}		// Create via the built-in config; a valid processor must exist for
		// the ACES scene→display-encoded pairing and must preserve alpha.
		let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal);
		let p = p.expect("processor handle always returned");
		assert!(
			p.is_valid(),
			"ACEScg→sRGB Encoded must be a valid processor"
		);
		let out = p.convert_color([0.18, 0.18, 0.18, 1.0]);
		assert!((out[3] - 1.0).abs() < 1e-6, "alpha passes through");
		assert!(out[0].is_finite() && out[1].is_finite() && out[2].is_finite());
		assert!(
			(out[0] - 0.18).abs() > 1e-3,
			"the transform must change the value"
		);
	}

	#[test]
	fn unknown_colorspaces_yield_invalid_processor() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let p = ColorProcessor::create("not_a_real_colorspace", "also_not_real", Direction::Normal)
			.unwrap();
		assert!(!p.is_valid(), "lookup failure is non-fatal (pass-through)");
		assert_eq!(
			p.convert_color([1.0, 0.5, 0.25, 0.0]),
			[1.0, 0.5, 0.25, 0.0]
		);
	}

	#[test]
	fn display_transform_queries() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let config = default_config().unwrap();
		if config.get_num_displays_all() <= 0 {
			return;
		}
		let display = config.get_display_all(0).unwrap();
		let n = config.get_num_views_v2(ocio_rs::SearchReferenceSpaceType::Scene, &display);
		assert!(n >= 0);
		if n > 0 {
			let view = config
				.get_view_v2(ocio_rs::SearchReferenceSpaceType::Scene, &display, 0)
				.unwrap();
			let id = display_transform(&display, &view);
			assert!(id.is_some());
			assert!(!id.unwrap().is_empty());
		}
		assert!(display_transform("no-such-display", "x").is_none());
	}

	#[test]
	fn convert_frame_with_valid_processor_changes_pixels() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let p = ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
			.expect("handle always returned");
		if !p.is_valid() {
			return;
		}
		let mut f = Frame::new();
		let mut pod = crate::frame::VideoParamsPod::default();
		pod.width = 4;
		pod.height = 2;
		f.set_video_params(pod);
		f.allocate();
		// Fill with 0.18 grey via f32 view.
		let f32s: &mut [f32] = unsafe {
			std::slice::from_raw_parts_mut(f.data.as_mut_ptr() as *mut f32, f.pixel_count() * 4)
		};
		for px in f32s.chunks_exact_mut(4) {
			px[0] = 0.18;
			px[1] = 0.18;
			px[2] = 0.18;
			px[3] = 1.0;
		}
		p.convert_frame(&mut f).unwrap();
		let out: &[f32] = unsafe {
			std::slice::from_raw_parts(f.data.as_ptr() as *const f32, f.pixel_count() * 4)
		};
		assert!(
			(out[0] - 0.18).abs() > 1e-4,
			"scene_linear→sdr-video must change 0.18 grey (got {})",
			out[0]
		);
		assert!((out[3] - 1.0).abs() < 1e-5, "alpha preserved");
	}

	#[test]
	fn display_icc_processor_applies_srgb_profile() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		// A display-class ICC is required; the macOS system profiles always
		// have one, CI Linux/Windows runners may not — skip then.
		let icc = [
			"/System/Library/ColorSync/Profiles/sRGB Profile.icc",
			"/System/Library/ColorSync/Profiles/Display P3.icc",
		]
		.into_iter()
		.find(|p| std::path::Path::new(p).exists());
		let Some(icc) = icc else {
			eprintln!("no system ICC profile; skipping");
			return;
		};
		let p = ColorProcessor::create_display_icc("scene_linear", icc)
			.expect("handle always returned");
		assert!(p.is_valid(), "ICC processor builds from {icc}");
		// 0.18 scene-linear grey -> ~0.5 sRGB device grey (the sRGB system
		// profile's device space is sRGB-encoded).
		let out = p.convert_color([0.18, 0.18, 0.18, 1.0]);
		assert!(
			(out[0] - 0.5).abs() < 0.08,
			"0.18 linear grey should land near 0.5 sRGB (got {})",
			out[0]
		);
		assert!((out[0] - out[1]).abs() < 1e-3 && (out[1] - out[2]).abs() < 1e-3,
			"grey stays grey: {out:?}");
		assert!((out[3] - 1.0).abs() < 1e-5, "alpha preserved");
	}

	/// The non-sRGB project output gamut display path: content converted to
	/// CIE XYZ (D65, unit luminance) by
	/// `crate::colormath::output_spec_to_xyz_d65` must flow through the
	/// display ICC. Builds by running the classic builder with the config's
	/// `cie_xyz_d65_interchange` role as the source space — the feasibility
	/// question this test answers is whether OCIO accepts the role name as a
	/// `ColorSpaceTransform` source (it is a role, not a bare colorspace, in
	/// the OCIO 2.2+ builtin configs).
	#[test]
	fn display_icc_xyz_accepts_interchange_role() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		// Any display-class ICC; probe the usual macOS + Linux system profile
		// locations (CI runners may have none — skip then).
		let icc = [
			"/System/Library/ColorSync/Profiles/sRGB Profile.icc",
			"/System/Library/ColorSync/Profiles/Display P3.icc",
			"/usr/share/color/icc/colord/sRGB.icc",
			"/usr/share/color/icc/ghostscript/srgb.icc",
			"/usr/local/share/color/icc/colord/sRGB.icc",
		]
		.into_iter()
		.find(|p| std::path::Path::new(p).exists());
		let Some(icc) = icc else {
			eprintln!("no system ICC profile; skipping");
			return;
		};
		let p = ColorProcessor::create_display_icc_xyz(icc)
			.expect("handle always returned or explicit None");
		assert!(
			p.is_valid(),
			"the cie_xyz_d65_interchange role must build the XYZ→ICC chain from {icc}"
		);
		// Numeric sanity: the XYZ chain fed with `output_spec_to_xyz_d65` of
		// an sRGB-encoded mid-grey must match the classic chain applied to
		// the same encoded values — legs 1+2 (XYZ→lin709→XYZ) are the inverse
		// round trip of the classic chain's lin709→XYZ leg, so both must land
		// on the same device values.
		let spec = crate::colormath::OutputColorSpec::default();
		let encoded = [0.5f32, 0.5, 0.5, 1.0];
		let mut xyz_in = encoded;
		crate::colormath::output_spec_to_xyz_d65(&mut xyz_in, spec);
		let mut via_xyz = xyz_in;
		let _ = p.convert_f32_rgba(&mut via_xyz, 1);
		let srgb = ColorProcessor::create_display_icc("sRGB Encoded Rec.709 (sRGB)", icc)
			.expect("handle always returned");
		let mut via_srgb = encoded;
		let _ = srgb.convert_f32_rgba(&mut via_srgb, 1);
		for c in 0..3 {
			assert!(
				(via_xyz[c] - via_srgb[c]).abs() < 0.02,
				"channel {c}: XYZ chain {} vs sRGB chain {} (round trip must be identity)",
				via_xyz[c],
				via_srgb[c]
			);
		}
		// Grey stays grey; alpha preserved.
		assert!(
			(via_xyz[0] - via_xyz[1]).abs() < 1e-3 && (via_xyz[1] - via_xyz[2]).abs() < 1e-3,
			"grey stays grey: {via_xyz:?}"
		);
		assert!((via_xyz[3] - 1.0).abs() < 1e-5, "alpha preserved");
	}

	/// The exact chain the viewers use (BGRA8, display-class ICC from
	/// `OAK_DISPLAY_ICC`): a mid-grey frame must NOT collapse to black —
	/// the viewer-black-screen regression guard. Skipped without the env
	/// var (point it at the display profile under investigation); an empty
	/// value is treated as unset, same as `displayicc::env_override_icc`.
	#[test]
	fn display_icc_bgra8_never_outputs_black() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let icc = std::env::var("OAK_DISPLAY_ICC").unwrap_or_default();
		if icc.is_empty() {
			eprintln!("OAK_DISPLAY_ICC unset or empty; skipping");
			return;
		}
		let p = ColorProcessor::create_display_icc_bgra8("sRGB Encoded Rec.709 (sRGB)", &icc)
			.expect("handle always returned");
		assert!(p.is_valid(), "BGRA8 ICC processor builds from {icc}");
		// BGRA bytes: 0.5 grey, 0.75 red, 0.25 green, 0.6 blue.
		let mut data: Vec<u8> = vec![
			128, 128, 128, 255, // grey
			0, 0, 191, 255, // red
			0, 64, 0, 255, // green
			153, 0, 0, 255, // blue
		];
		let bgra_result = p.convert_bgra8(&mut data, 4);
		eprintln!("bgra8 convert: {bgra_result:?} -> {data:?}");
		// The F32 RGBA leg (the CpuF32 display path) must not crush to
		// black either — and unlike the u8 leg it must not even fail.
		let p32 = ColorProcessor::create_display_icc("sRGB Encoded Rec.709 (sRGB)", &icc)
			.expect("handle always returned");
		assert!(p32.is_valid(), "F32 ICC processor builds from {icc}");
		let mut f32s: Vec<f32> = vec![
			0.5, 0.5, 0.5, 1.0, // grey
			0.75, 0.0, 0.0, 1.0, // red
			0.0, 0.25, 0.0, 1.0, // green
			0.0, 0.0, 0.6, 1.0, // blue
		];
		let f32_result = p32.convert_f32_rgba(&mut f32s, 4);
		eprintln!("f32 convert: {f32_result:?} -> {f32s:?}");
		bgra_result.expect("convert");
		for (i, px) in data.chunks_exact(4).enumerate() {
			assert_eq!(px[3], 255, "pixel {i}: alpha preserved");
			let rgb: u32 = px[0] as u32 + px[1] as u32 + px[2] as u32;
			assert!(rgb > 0, "pixel {i} must not be crushed to black: {px:?}");
		}
		// Grey stays greyish (a display profile must not tint wildly).
		let (b, g, r) = (data[0] as i32, data[1] as i32, data[2] as i32);
		assert!(
			(b - g).abs() < 24 && (g - r).abs() < 24,
			"grey stays grey through the display ICC: {b} {g} {r}"
		);
		f32_result.expect("convert f32");
		for (i, px) in f32s.chunks_exact(4).enumerate() {
			assert!((px[3] - 1.0).abs() < 1e-3, "pixel {i}: alpha preserved");
			let rgb = px[0] + px[1] + px[2];
			assert!(rgb > 0.0, "pixel {i} must not be crushed to black: {px:?}");
		}
	}

	#[test]
	fn inverse_direction_reverses() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let fwd =
			ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Normal)
				.expect("handle");
		let inv =
			ColorProcessor::create("ACEScg", "sRGB Encoded Rec.709 (sRGB)", Direction::Inverse)
				.expect("handle");
		if !fwd.is_valid() || !inv.is_valid() {
			return;
		}
		let x = fwd.convert_color([0.18, 0.18, 0.18, 1.0]);
		let back = inv.convert_color(x);
		assert!(
			(back[0] - 0.18).abs() < 0.05,
			"inverse undoes forward ({} -> {})",
			x[0],
			back[0]
		);
	}

	#[test]
	fn create_lut_from_real_file() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		// A tiny 1D .cube LUT (identity-ish with a slight lift).
		let lut = "TITLE oakrender test\nLUT_1D_SIZE 2\n0.0 0.0 0.0\n1.0 1.0 1.0\n";
		let path = std::env::temp_dir().join("oakrender-test-lut.cube");
		std::fs::write(&path, lut).unwrap();
		let p = ColorProcessor::create_lut(path.to_string_lossy().as_ref(), Direction::Normal)
			.expect("processor handle always returned");
		assert!(
			p.is_valid(),
			"a readable .cube must produce a valid processor"
		);
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn grading_primary_and_from_processor() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		if let Some(p) = ColorProcessor::create_grading_primary(GradingStyle::Log) {
			// A grading-primary processor is valid and converts.
			if p.is_valid() {
				let out = p.convert_color([0.18, 0.5, 0.7, 1.0]);
				assert!(out.iter().all(|v| v.is_finite()));
			}
		}
		if let Some(config) = default_config() {
			if let Ok(proc) = config.processor("ACEScg", "sRGB Encoded Rec.709 (sRGB)") {
				let p = ColorProcessor::from_processor(proc);
				assert!(p.is_valid());
				assert!(!p.cache_id().is_empty(), "OCIO cache id present");
			}
		}
	}

	#[test]
	fn grading_primary_shader_generates_analytic_glsl() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return; // Bundled OCIO missing (e.g. stub build): skip.
		}
		for style in [GradingStyle::Lin, GradingStyle::Log] {
			let stub = grading_primary_function_shader(style)
				.expect("default config generates an analytic grading shader");
			assert!(
				stub.contains("ove_grading_primary"),
				"function name present"
			);
			assert!(!stub.contains("sampler"), "no LUT upload expected for grading");
			// Cache hit: a repeated call returns the same text.
			let again = grading_primary_function_shader(style).unwrap();
			assert_eq!(stub, again);
		}
	}

	#[test]
	fn create_lut_missing_file_is_invalid() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return;
		}
		let p = ColorProcessor::create_lut("/nonexistent/never.cube", Direction::Normal).unwrap();
		assert!(!p.is_valid(), "unreadable LUT → pass-through processor");
	}

	#[test]
	fn ocio_function_shader_generates_glsl_for_chromakey() {
		let _lock = config_lock();
		if set_up_default_config().is_err() {
			return; // Bundled OCIO missing (e.g. stub build): skip.
		}
		let stub = ocio_function_shader(
			"SceneLinearToCIEXYZ_d65",
			"scene_linear",
			"cie_xyz_d65_interchange",
		)
		.expect("default config generates an analytic shader");
		assert!(stub.contains("SceneLinearToCIEXYZ_d65"), "function name present");
		assert!(!stub.contains("sampler"), "no LUT upload expected in the default config");
		// Cache hit: a repeated call returns the same text.
		let again = ocio_function_shader(
			"SceneLinearToCIEXYZ_d65",
			"scene_linear",
			"cie_xyz_d65_interchange",
		)
		.unwrap();
		assert_eq!(stub, again);
	}

	#[test]
	fn ocio_function_shader_rejects_lut_processors() {
		let _lock = config_lock();
		// studio-config's Rec.709 display is a CLF LUT chain (unlike the
		// analytic sRGB one); without a LUT upload path it must be refused.
		let Ok(cfg) = ocio_rs::Config::create_from_builtin_config(
			"studio-config-v2.1.0_aces-v1.3_ocio-v2.3",
		) else {
			return;
		};
		let cfg = SafeConfig(cfg);
		assert!(
			build_ocio_function_shader(&cfg, "probe", "ACEScg", "Rec.709 - Display").is_none(),
			"LUT-based processor must be refused"
		);
		// A pure-matrix pairing on the same config still generates.
		assert!(
			build_ocio_function_shader(&cfg, "probe", "ACEScg", "ACES2065-1").is_some(),
			"analytic processor still generates"
		);
	}
}

