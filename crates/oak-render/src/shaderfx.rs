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

//! Effect shader translation: the nodes' embedded GLSL fragment shaders
//! (the C++ `:/shaders/*.frag` corpus, kept verbatim in oak-node) are
//! converted to WGSL at runtime through naga and run as wgpu fullscreen
//! passes.
//!
//! The conversion mirrors the C++ Vulkan backend's mechanical rewrite
//! (`vulkanrenderer.cpp` `ConvertGlslToVulkan` + `ExtractUniforms`):
//!
//! - a `#version 450 core` prelude is prepended;
//! - legacy `texture2D(`/`texture3D(` calls are renamed to `texture(`;
//! - the pipeline I/O globals get explicit locations
//!   (`layout(location = 0) in vec2 ove_texcoord;`,
//!   `layout(location = 0) out vec4 frag_color;`);
//! - loose `uniform <type> <name>;` declarations are extracted: samplers
//!   get explicit `set`/`binding` qualifiers, and value uniforms are
//!   collected into one anonymous `std140` uniform block (GLSL 450
//!   anonymous block members stay accessible by their bare names, so the
//!   shader body needs no rewriting).
//!
//! Uniform values are packed by the caller following std140 rules
//! (float/int/bool 4/4, vec2 8/8, vec3 12/16, vec4 16/16, mat4 64/16 —
//! the same table the C++ `GetStd140Size/Alignment` used).

use crate::error::{Error, Result};

/// A value uniform's GLSL type (std140 packing + `NodeValue` mapping).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UniformType {
	/// `float`.
	Float,
	/// `int`.
	Int,
	/// `bool` (stored as `int` in the block; WGSL has no shareable bool).
	Bool,
	/// `vec2`.
	Vec2,
	/// `vec3`.
	Vec3,
	/// `vec4`.
	Vec4,
	/// `mat4`.
	Mat4,
	/// `vec4[N]` (an array uniform; the polygon generator's bezier point
	/// table is the only consumer, indexed by `[i]` in the shader body).
	Vec4Array(usize),
}

impl UniformType {
	/// The GLSL type keyword, or `None` when it is not a value uniform
	/// (samplers, arrays and unknown types are not packable).
	fn from_keyword(kw: &str) -> Option<UniformType> {
		Some(match kw {
			"float" => UniformType::Float,
			"int" => UniformType::Int,
			"bool" => UniformType::Bool,
			"vec2" => UniformType::Vec2,
			"vec3" => UniformType::Vec3,
			"vec4" => UniformType::Vec4,
			"mat4" => UniformType::Mat4,
			_ => return None,
		})
	}

	/// The GLSL keyword back (block re-emission). Array types have their
	/// count appended (`vec4{name}[{count}]`), matching C++ uniform-array
	/// declarations.
	fn keyword(self) -> &'static str {
		match self {
			UniformType::Float => "float",
			UniformType::Int => "int",
			UniformType::Bool => "bool",
			UniformType::Vec2 => "vec2",
			UniformType::Vec3 => "vec3",
			UniformType::Vec4 => "vec4",
			UniformType::Mat4 => "mat4",
			UniformType::Vec4Array(_) => "vec4",
		}
	}

	/// std140 base alignment in bytes (C++ `GetStd140Alignment`).
	pub fn align(self) -> usize {
		match self {
			UniformType::Float | UniformType::Int | UniformType::Bool => 4,
			UniformType::Vec2 => 8,
			UniformType::Vec3 | UniformType::Vec4 | UniformType::Mat4 => 16,
			UniformType::Vec4Array(_) => 16,
		}
	}

	/// std140 storage size in bytes (C++ `GetStd140Size`).
	pub fn size(self) -> usize {
		match self {
			UniformType::Float | UniformType::Int | UniformType::Bool => 4,
			UniformType::Vec2 => 8,
			UniformType::Vec3 => 12,
			UniformType::Vec4 => 16,
			UniformType::Mat4 => 64,
			UniformType::Vec4Array(n) => 16 * n.max(1),
		}
	}
}

/// A value uniform (std140-packed into the uniform block).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniformDecl {
	/// Uniform name (= the node input id, the Olive convention).
	pub name: String,
	/// Its type.
	pub ty: UniformType,
	/// Byte offset in the packed block (assigned by [`translate`]).
	pub offset: usize,
}

/// The result of translating one effect fragment shader.
#[derive(Clone, Debug)]
pub struct TranslatedShader {
	/// The WGSL fragment module (entry point `main`).
	pub wgsl: String,
	/// Value uniforms in block order (offsets assigned, block tail-padded
	/// to 16).
	pub uniforms: Vec<UniformDecl>,
	/// The packed uniform block size in bytes (0 = no value uniforms).
	pub uniform_block_bytes: usize,
	/// Texture input names in binding order (combined `sampler2D` etc.;
	/// the first one is the effect's main input by Olive convention).
	pub textures: Vec<String>,
	/// Fragment input varyings in location order (`ove_texcoord` first,
	/// then any effect-specific varyings like cornerpin's perspective
	/// helpers). The runner's vertex stage must produce all of them.
	pub varyings: Vec<String>,
}

// ---------------------------------------------------------------------------
// The effect runner
// ---------------------------------------------------------------------------

/// A compiled effect: the translated shader plus its cached pipeline.
pub struct CompiledEffect {
	/// The translation result (uniform layout + texture bindings).
	pub translated: TranslatedShader,
	/// The compiled pipeline in the context cache.
	pub program: std::sync::Arc<crate::backend::ShaderProgram>,
}

/// Translate `glsl` and compile the pipeline on `ctx`. `key` is the
/// pipeline cache key (the effect type id plus any shader-variant id).
pub fn compile_effect(
	ctx: &crate::backend::GpuContext,
	key: &str,
	glsl: &str,
	filtering: bool,
) -> Result<CompiledEffect> {
	let translated = translate(glsl)?;
	let program = ctx.compile_shader_pass(
		key,
		&translated.wgsl,
		translated.textures.len() as u32,
		!translated.uniforms.is_empty(),
		filtering,
	)?;
	Ok(CompiledEffect { translated, program })
}

/// Run an effect: shade `dst` from the input textures with `params` as
/// the uniform values.
///
/// - `inputs` maps the shader's texture names to context texture tokens;
///   declared textures without an input bind the context's 1×1
///   placeholder and get `<name>_enabled = 0` (C++ Blit's texture
///   binding + enable-flag convention). The first declared texture is
///   the main input — and the iterative one when `iterations` > 1
///   (C++ `ShaderJob::SetIterations` with `tex_in`).
/// - `iterations` runs the shader that many times, feeding each pass's
///   output back as the main input (C++ `OpenGLRenderer::Blit`'s
///   ping-pong; the `ove_iteration` uniform tracks the pass index).
/// - Well-known uniforms are auto-filled when declared but absent from
///   `params`: `resolution_in` (the frame size), `ove_iteration`,
///   `ove_mvpmat` (identity).
pub fn run_effect(
	ctx: &crate::backend::GpuContext,
	effect: &CompiledEffect,
	params: &oak_node::value::NodeValueRow,
	inputs: &[(String, u64)],
	dst: u64,
	size: (i32, i32),
	iterations: u32,
) -> Result<()> {
	use oak_node::value::NodeValue;

	let declares = |name: &str| effect.translated.uniforms.iter().any(|u| u.name == name);

	// Resolve every declared texture to a token (placeholder when the
	// effect's input is unconnected).
	let mut tokens: Vec<u64> = Vec::with_capacity(effect.translated.textures.len());
	let mut row = params.clone();
	for (i, name) in effect.translated.textures.iter().enumerate() {
		let token = inputs
			.iter()
			.find(|(n, _)| n == name)
			.map(|(_, t)| *t)
			.or_else(|| if i == 0 { inputs.first().map(|(_, t)| *t) } else { None });
		match token {
			Some(t) => {
				tokens.push(t);
				let flag = format!("{name}_enabled");
				if declares(&flag) && !row.contains_key(&flag) {
					row.insert(flag, NodeValue::Boolean(true));
				}
			}
			None => tokens.push(ctx.placeholder_texture()?),
		}
	}

	// Well-known uniforms (C++ inserts resolution_in at job-build time;
	// ove_mvpmat defaults to identity in Blit).
	if declares("resolution_in") && !row.contains_key("resolution_in") {
		row.insert(
			"resolution_in".to_string(),
			NodeValue::Vec2([size.0 as f64, size.1 as f64]),
		);
	}
	if declares("ove_mvpmat") && !row.contains_key("ove_mvpmat") {
		let mut m = [0.0f64; 16];
		for i in 0..4 {
			m[i * 4 + i] = 1.0;
		}
		row.insert("ove_mvpmat".to_string(), NodeValue::Matrix(m));
	}

	let iterations = iterations.max(1);
	if iterations == 1 {
		let uniforms = pack_uniforms(&effect.translated, &row);
		return ctx.run_shader_pass(&effect.program, &uniforms, &tokens, dst);
	}

	// Ping-pong (C++ Blit): one scratch texture for two passes, two for
	// longer chains; the last pass always lands in `dst`.
	let scratch_a = ctx.create_texture(size.0, size.1)?;
	let scratch_b = if iterations > 2 {
		Some(ctx.create_texture(size.0, size.1)?)
	} else {
		None
	};
	let result = (|| -> Result<()> {
		let mut input_tokens = tokens.clone();
		for i in 0..iterations {
			let mut pass_row = row.clone();
			if declares("ove_iteration") {
				pass_row.insert("ove_iteration".to_string(), NodeValue::Int(i as i64));
			}
			let target = if i == iterations - 1 {
				dst
			} else if i % 2 == 0 {
				scratch_a
			} else {
				scratch_b.unwrap()
			};
			let uniforms = pack_uniforms(&effect.translated, &pass_row);
			ctx.run_shader_pass(&effect.program, &uniforms, &input_tokens, target)?;
			input_tokens[0] = target;
		}
		Ok(())
	})();
	ctx.destroy_texture(scratch_a);
	if let Some(b) = scratch_b {
		ctx.destroy_texture(b);
	}
	result
}


/// Replace whole-word occurrences of `name` in `s` with `replacement`
/// (identifier boundaries: alphanumerics and `_`). The node-shader
/// corpus uses plain identifiers, so this simple scan suffices — no
/// regex dependency.
fn replace_ident(s: &str, name: &str, replacement: &str) -> String {
	fn is_ident_char(c: u8) -> bool {
		c.is_ascii_alphanumeric() || c == b'_'
	}
	let bytes = s.as_bytes();
	let name = name.as_bytes();
	let mut out = String::with_capacity(s.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i..].starts_with(name)
			&& (i == 0 || !is_ident_char(bytes[i - 1]))
			&& !bytes
				.get(i + name.len())
				.is_some_and(|&c| is_ident_char(c))
		{
			out.push_str(replacement);
			i += name.len();
		} else {
			out.push(bytes[i] as char);
			i += 1;
		}
	}
	out
}

/// Parse a standalone `in`/`out` varying declaration line body (after
/// the direction keyword), e.g. `vec2 ove_texcoord;` → `(name, type)`.
/// Function parameter lists and anything else are rejected.
fn parse_plain_global(rest: &str) -> Option<(String, String)> {
	let rest = rest.trim().strip_suffix(';')?.trim();
	let (ty, name) = rest.split_once(char::is_whitespace)?;
	let name = name.trim();
	if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
		return None;
	}
	Some((name.to_string(), ty.to_string()))
}

/// Binding 0 is the uniform block; textures/samplers follow.
const UNIFORM_BLOCK_BINDING: u32 = 0;

/// Pack the uniform block for `shader` from `params` (the effect's
/// parameter row; uniform names are the node input ids by Olive
/// convention). Packing follows the declared uniform types, converting
/// from whatever `NodeValue` shape arrived (the C++ Blit dispatched on
/// the value type with GL's implicit conversions; here the declared
/// type wins). Undeclared params are skipped; missing values stay zero.
pub fn pack_uniforms(
	shader: &TranslatedShader,
	params: &oak_node::value::NodeValueRow,
) -> Vec<u8> {
	use oak_node::value::NodeValue;
	let mut buf = vec![0u8; shader.uniform_block_bytes];
	for decl in &shader.uniforms {
		let Some(value) = params.get(&decl.name) else {
			continue;
		};
		let f32s: Vec<f32> = match (decl.ty, value) {
			(UniformType::Float, NodeValue::Float(v)) => vec![*v as f32],
			(UniformType::Float, NodeValue::Int(v) | NodeValue::Combo(v)) => vec![*v as f32],
			(UniformType::Float, NodeValue::Boolean(v)) => vec![f32::from(u8::from(*v))],
			(UniformType::Int, NodeValue::Int(v) | NodeValue::Combo(v)) => {
				write_i32(&mut buf, decl.offset, *v as i32);
				continue;
			}
			(UniformType::Int, NodeValue::Float(v)) => {
				write_i32(&mut buf, decl.offset, *v as i32);
				continue;
			}
			(UniformType::Bool, NodeValue::Boolean(v)) => {
				write_i32(&mut buf, decl.offset, i32::from(*v));
				continue;
			}
			(UniformType::Bool, NodeValue::Int(v) | NodeValue::Combo(v)) => {
				write_i32(&mut buf, decl.offset, i32::from(*v != 0));
				continue;
			}
			(UniformType::Vec2, NodeValue::Vec2(v)) => v.iter().map(|x| *x as f32).collect(),
			(UniformType::Vec3, NodeValue::Vec3(v)) => v.iter().map(|x| *x as f32).collect(),
			// A color packs into a vec3 slot as its RGB (C++ Color →
			// glUniform4f only for vec4; a vec3 target takes rgb).
			(UniformType::Vec3, NodeValue::Color(v)) => {
				v[..3].iter().map(|x| *x as f32).collect()
			}
			(UniformType::Vec4, NodeValue::Vec4(v) | NodeValue::Color(v)) => {
				v.iter().map(|x| *x as f32).collect()
			}
			(UniformType::Vec4Array(n), NodeValue::Vec4Array(v)) => {
				let mut out = Vec::with_capacity(n * 4);
				for el in v.iter().take(n) {
					out.extend(el.iter().map(|x| *x as f32));
				}
				// Pad to the declared count (each element is 16 bytes; the
				// buffer is sized n*16 regardless).
				let have = out.len();
				out.resize((n * 4).max(have), 0.0f32);
				out
			}
			// Matrices: GLSL mat4 is column-major; the NodeValue comment
			// marks the layout row-major, so transpose on the way in.
			(UniformType::Mat4, NodeValue::Matrix(m)) => {
				let mut cols = Vec::with_capacity(16);
				for c in 0..4 {
					for r in 0..4 {
						cols.push(m[r * 4 + c] as f32);
					}
				}
				cols
			}
			_ => continue,
		};
		for (i, v) in f32s.iter().enumerate() {
			let at = decl.offset + i * 4;
			if at + 4 <= buf.len() {
				buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
	}
	buf
}

fn write_i32(buf: &mut [u8], offset: usize, v: i32) {
	if offset + 4 <= buf.len() {
		buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
	}
}

/// Whether a GLSL type keyword is a combined sampler (C++
/// `IsSamplerType`: sampler\*D / samplerCube / sampler2DArray).
fn is_sampler_type(kw: &str) -> bool {
	kw.starts_with("sampler")
}

/// Parse a `uniform <type> <name>;` declaration line (the constrained
/// style of the node shader corpus: one declaration per line, no layout
/// qualifiers, no initializers). Arrays ARE supported:
/// `uniform <type> <name>[<count>];` (the polygon generator's point
/// table). `None` for any other line (samplers are handled by the
/// caller). Returns `(base keyword, name, array count)`.
fn parse_uniform_line(line: &str) -> Option<(&str, &str, usize)> {
	let t = line.trim_start();
	let rest = t.strip_prefix("uniform")?;
	if !rest.starts_with(char::is_whitespace) {
		return None;
	}
	let rest = rest.trim_start();
	let (ty, rest) = rest.split_once(char::is_whitespace)?;
	let rest = rest.trim_start().trim_end_matches(';');
	let (name, count) = if let Some(idx) = rest.find('[') {
		let name = rest[..idx].trim();
		let end = rest[idx..].find(']')?;
		let count: usize = rest[idx + 1..idx + end].trim().parse().ok()?;
		(name, count.max(1))
	} else {
		(rest.trim(), 1)
	};
	if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
		return None;
	}
	Some((ty, name, count))
}

/// Translate one GLSL fragment shader to WGSL (naga glsl-in → wgsl-out).
/// The source keeps the Olive node-shader conventions; see the module
/// docs for the rewrite steps.
pub fn translate(glsl: &str) -> Result<TranslatedShader> {
	let mut body_lines: Vec<String> = Vec::new();
	let mut uniforms: Vec<(UniformType, String, usize)> = Vec::new();
	let mut textures: Vec<String> = Vec::new();

	for line in glsl.lines() {
		if let Some((ty, name, count)) = parse_uniform_line(line) {
			if is_sampler_type(ty) {
				for _ in 0..count {
					textures.push(name.to_string());
				}
				continue;
			}
			let base = UniformType::from_keyword(ty);
			let uniform = match (base, count) {
				(Some(UniformType::Vec4), n) if n > 1 => UniformType::Vec4Array(n),
				(Some(t), n) if n == 1 => t,
				_ => {
					return Err(Error::Failed(format!(
						"unsupported array uniform type in shader: {ty} {name}[{count}]"
					)));
				}
			};
			uniforms.push((uniform, name.to_string(), count));
			continue;
		}
		body_lines.push(line.to_string());
	}

	let mut src = String::from("#version 450 core\n");
	let mut in_loc = 0u32;
	let mut out_loc = 0u32;
	let mut varyings: Vec<String> = Vec::new();

	// Re-emit the extracted uniforms BEFORE the body (GLSL requires
	// declarations to precede use): the value block first (binding 0),
	// then the samplers. The block is anonymous — GLSL 450 anonymous
	// block members stay accessible by their bare names, so the shader
	// body needs no rewriting.
	if !uniforms.is_empty() {
		src.push_str("layout(std140, set = 0, binding = ");
		src.push_str(&UNIFORM_BLOCK_BINDING.to_string());
		src.push_str(") uniform OakParams {\n");
		for (ty, name, count) in &uniforms {
			// bools are declared as int (WGSL has no host-shareable
			// bool); the body's uses were rewritten to `bool(x)`.
			let kw = if *ty == UniformType::Bool {
				"int"
			} else {
				ty.keyword()
			};
			if *count > 1 {
				src.push_str(&format!("    {kw} {name}[{}];\n", count));
			} else {
				src.push_str(&format!("    {kw} {name};\n"));
			}
		}
		src.push_str("};\n");
	}
	for (i, name) in textures.iter().enumerate() {
		// Split the combined sampler2D: texture at an odd binding, its
		// sampler right after (the binding map is reported through
		// [`TranslatedShader::textures`] in declaration order).
		src.push_str(&format!(
			"layout(set = 0, binding = {}) uniform texture2D {};\n\
			 layout(set = 0, binding = {}) uniform sampler {}_s;\n",
			1 + 2 * i,
			name,
			2 + 2 * i,
			name
		));
	}

	for line in &body_lines {
		let mut l = line.clone();
		// Strip a pre-existing #version (the prelude pins 450 core).
		if l.trim_start().starts_with("#version") {
			continue;
		}
		// Legacy sampling entry points.
		l = l.replace("texture2D(", "texture(");
		l = l.replace("texture3D(", "texture(");
		l = l.replace("textureCube(", "texture(");
		// naga's GLSL frontend has no combined sampler2D uniforms: split
		// each into (texture2D, sampler) and combine at the call site
		// (`texture(sampler2D(tex, tex_s), uv)` — the same style naga's
		// own GLSL tests use).
		for name in &textures {
			l = l.replace(
				&format!("texture({name},"),
				&format!("texture(sampler2D({name}, {name}_s),"),
			);
		}
		// WGSL has no host-shareable bool: bool uniforms live in the
		// block as `int`, so their uses become `bool(x)` (nonzero test).
		for (ty, name, _count) in &uniforms {
			if *ty == UniformType::Bool {
				l = replace_ident(&l, name, &format!("bool({name})"));
			}
		}
		// Explicit interface locations (C++ ConvertGlslToVulkan did this
		// for the two known globals; shaders with extra varyings — e.g.
		// cornerpin's perspective helpers — get sequential locations so
		// nothing collides at location 0).
		let trimmed = l.trim_start();
		if let Some(rest) = trimmed.strip_prefix("in ") {
			if let Some((name, _ty)) = parse_plain_global(rest) {
				let indent = &l[..l.len() - trimmed.len()];
				l = format!("{indent}layout(location = {in_loc}) in {}", rest.trim());
				varyings.push(name);
				in_loc += 1;
			}
		} else if let Some(rest) = trimmed.strip_prefix("out ") {
			if parse_plain_global(rest).is_some() {
				let indent = &l[..l.len() - trimmed.len()];
				l = format!("{indent}layout(location = {out_loc}) out {}", rest.trim());
				out_loc += 1;
			}
		}
		src.push_str(&l);
		src.push('\n');
	}

	let module = naga::front::glsl::Frontend::default()
		.parse(&naga::front::glsl::Options::from(naga::ShaderStage::Fragment), &src)
		.map_err(|e| Error::Failed(format!("GLSL parse failed: {e:?}")))?;
	let info = naga::valid::Validator::new(
		naga::valid::ValidationFlags::all(),
		naga::valid::Capabilities::all(),
	)
	.validate(&module)
	.map_err(|e| Error::Failed(format!("translated shader failed validation: {e:?}")))?;
	let wgsl = naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
		.map_err(|e| Error::Failed(format!("WGSL emission failed: {e:?}")))?;

	// std140 offsets (declaration order; the block tail pads to 16).
	let mut offset = 0usize;
	let mut decls = Vec::with_capacity(uniforms.len());
	for (ty, name, _count) in uniforms {
		let align = ty.align();
		offset = offset.next_multiple_of(align);
		decls.push(UniformDecl { name, ty, offset });
		offset += ty.size();
	}
	let uniform_block_bytes = if decls.is_empty() {
		0
	} else {
		offset.next_multiple_of(16)
	};

	Ok(TranslatedShader {
		wgsl,
		uniforms: decls,
		uniform_block_bytes,
		textures,
		varyings,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The minimal node-shader shape: texture input + one float uniform.
	#[test]
	fn translates_minimal_effect_shader() {
		let glsl = r#"
uniform sampler2D tex_in;
uniform float gain_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    frag_color = texture(tex_in, ove_texcoord) * gain_in;
}
"#;
		let out = translate(glsl).expect("translate");
		assert_eq!(out.textures, vec!["tex_in"]);
		assert_eq!(out.uniforms.len(), 1);
		assert_eq!(out.uniforms[0].name, "gain_in");
		assert_eq!(out.uniforms[0].ty, UniformType::Float);
		assert_eq!(out.uniforms[0].offset, 0);
		assert_eq!(out.uniform_block_bytes, 16);
		assert!(out.wgsl.contains("fn main"), "WGSL entry point: {}", out.wgsl);
	}

	/// bool/int/vec/color-shaped uniforms pack with std140 offsets.
	#[test]
	fn std140_offsets_match_the_cpp_table() {
		let glsl = r#"
uniform sampler2D tex_in;
uniform bool flag_in;
uniform vec2 center_in;
uniform float radius_in;
uniform vec4 color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() {
    vec4 c = texture(tex_in, ove_texcoord);
    frag_color = flag_in ? color_in * radius_in : vec4(c.xy + center_in, c.zw);
}
"#;
		let out = translate(glsl).expect("translate");
		let offsets: Vec<(&str, usize)> = out
			.uniforms
			.iter()
			.map(|u| (u.name.as_str(), u.offset))
			.collect();
		// bool 4/4 @0; vec2 align 8 @8; float 4 @16; vec4 align 16 @32.
		assert_eq!(
			offsets,
			vec![
				("flag_in", 0),
				("center_in", 8),
				("radius_in", 16),
				("color_in", 32)
			]
		);
		assert_eq!(out.uniform_block_bytes, 48);
	}

	/// Array uniforms translate into the block as packed std140 arrays
	/// (polygon's `points_in[64]`; the declaration order determines the
	/// offsets: `int` @0, the vec4 array aligned to 16 @16, the trailing
	/// vec2 aligned to 8).
	#[test]
	fn array_uniforms_translate_and_pack() {
		use oak_node::value::{NodeValue, NodeValueRow};
		let glsl = r#"
uniform int point_count;
uniform vec4 points_in[64];
uniform vec2 resolution_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() { frag_color = vec4(0.0); }
"#;
		let out = translate(glsl).expect("translate array uniforms");
		let decls: Vec<(&str, &UniformType, usize)> = out
			.uniforms
			.iter()
			.map(|u| (u.name.as_str(), &u.ty, u.offset))
			.collect();
		assert_eq!(
			decls,
			vec![
				("point_count", &UniformType::Int, 0),
				("points_in", &UniformType::Vec4Array(64), 16),
				("resolution_in", &UniformType::Vec2, 16 + 64 * 16),
			]
		);
		assert_eq!(
			out.uniform_block_bytes,
			(16 + 64 * 16 + 8usize).next_multiple_of(16)
		);

		let mut row = NodeValueRow::new();
		row.insert("point_count".into(), NodeValue::Int(3));
		row.insert(
			"points_in".into(),
			NodeValue::Vec4Array(vec![[1.0, 2.0, 0.0, 0.0], [3.0, 4.0, 0.0, 0.0]]),
		);
		let buf = pack_uniforms(&out, &row);
		assert_eq!(buf.len(), out.uniform_block_bytes);
		let count = out.uniforms.iter().find(|u| u.name == "point_count").unwrap();
		assert_eq!(
			i32::from_le_bytes(buf[count.offset..count.offset + 4].try_into().unwrap()),
			3
		);
		let points = out.uniforms.iter().find(|u| u.name == "points_in").unwrap();
		let at = |i: usize, c: usize| points.offset + i * 16 + c * 4;
		assert_eq!(f32::from_le_bytes(buf[at(0, 0)..at(0, 1)].try_into().unwrap()), 1.0);
		assert_eq!(f32::from_le_bytes(buf[at(0, 1)..at(0, 2)].try_into().unwrap()), 2.0);
		assert_eq!(f32::from_le_bytes(buf[at(1, 0)..at(1, 1)].try_into().unwrap()), 3.0);
		// Short arrays pad the remaining slots to zero.
		assert_eq!(
			f32::from_le_bytes(buf[at(63, 0)..at(63, 1)].try_into().unwrap()),
			0.0
		);
	}

	/// Uniform packing follows the declared types and std140 offsets.
	#[test]
	fn pack_uniforms_maps_node_values() {
		use oak_node::value::{NodeValue, NodeValueRow};
		let glsl = r#"
uniform sampler2D tex_in;
uniform float gain_in;
uniform bool flag_in;
uniform vec4 color_in;

in vec2 ove_texcoord;
out vec4 frag_color;

void main() { frag_color = texture(tex_in, ove_texcoord); }
"#;
		let out = translate(glsl).unwrap();
		let mut row = NodeValueRow::new();
		row.insert("gain_in".into(), NodeValue::Float(0.5));
		row.insert("flag_in".into(), NodeValue::Boolean(true));
		row.insert("color_in".into(), NodeValue::Color([0.1, 0.2, 0.3, 0.4]));
		let buf = pack_uniforms(&out, &row);
		assert_eq!(buf.len(), out.uniform_block_bytes);
		let gain = out.uniforms.iter().find(|u| u.name == "gain_in").unwrap();
		assert_eq!(
			f32::from_le_bytes(buf[gain.offset..gain.offset + 4].try_into().unwrap()),
			0.5
		);
		let flag = out.uniforms.iter().find(|u| u.name == "flag_in").unwrap();
		assert_eq!(
			i32::from_le_bytes(buf[flag.offset..flag.offset + 4].try_into().unwrap()),
			1
		);
		let color = out.uniforms.iter().find(|u| u.name == "color_in").unwrap();
		for (i, want) in [0.1f32, 0.2, 0.3, 0.4].iter().enumerate() {
			let at = color.offset + i * 4;
			assert_eq!(f32::from_le_bytes(buf[at..at + 4].try_into().unwrap()), *want);
		}
	}

	// ---- GPU runner tests (skipped without an adapter) -------------------

	fn gpu() -> Option<std::sync::Arc<crate::backend::GpuContext>> {
		crate::backend::GpuContext::create(crate::backend::BackendKind::Auto)
	}

	fn f32_frame(w: i32, h: i32, fill: impl Fn(usize) -> [f32; 4]) -> crate::texture::Frame {
		use crate::texture::Frame;
		let mut frame = Frame::new();
		let mut pod = crate::frame::VideoParamsPod::default();
		pod.width = w;
		pod.height = h;
		pod.format = oak_core::PixelFormat::F32 as i32;
		frame.set_video_params(pod);
		frame.allocate();
		for px in 0..(w * h) as usize {
			let rgba = fill(px);
			for (c, v) in rgba.iter().enumerate() {
				frame.data[(px * 4 + c) * 4..(px * 4 + c) * 4 + 4]
					.copy_from_slice(&v.to_le_bytes());
			}
		}
		frame
	}

	fn pixel(out: &crate::texture::Frame, x: usize) -> [f32; 4] {
		let mut rgba = [0.0f32; 4];
		for (c, v) in rgba.iter_mut().enumerate() {
			*v = f32::from_le_bytes(out.data[(x * 4 + c) * 4..(x * 4 + c) * 4 + 4].try_into().unwrap());
		}
		rgba
	}

	/// The real opacity shader through the full runner: pixels are
	/// multiplied by the factor (and the UV convention is identity —
	/// a flip would move the non-uniform pixels around).
	#[test]
	fn gpu_opacity_effect_scales_pixels() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.opacity")
			.unwrap();
		let glsl = behavior.shader_code("").unwrap();
		let effect = compile_effect(&ctx, "test/opacity", &glsl, false).unwrap();

		let frame = f32_frame(16, 4, |px| {
			[0.2 + 0.01 * px as f32, 0.4, 0.6, 1.0]
		});
		let src = ctx.create_texture(16, 4).unwrap();
		ctx.upload(src, &frame).unwrap();
		let dst = ctx.create_texture(16, 4).unwrap();

		let mut row = oak_node::value::NodeValueRow::new();
		row.insert("opacity_in".into(), oak_node::value::NodeValue::Float(0.5));
		run_effect(&ctx, &effect, &row, &[("tex_in".to_string(), src)], dst, (16, 4), 1).unwrap();

		let out = ctx.download(dst).unwrap();
		for px in 0..16usize {
			let want = (0.2 + 0.01 * px as f32) * 0.5;
			let got = pixel(&out, px)[0];
			assert!((got - want).abs() < 1e-4, "px {px}: got {got}, want {want}");
		}
		ctx.destroy_texture(src);
		ctx.destroy_texture(dst);
	}

	/// The real blur shader: radius 0 is an exact passthrough (the
	/// shader's MODE_NONE branch), and a 2px horizontal box blur on a
	/// step edge lands exactly half-and-half at the boundary pixels.
	#[test]
	fn gpu_blur_effect_passthrough_and_step() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.blur")
			.unwrap();
		let glsl = behavior.shader_code("").unwrap();
		let effect = compile_effect(&ctx, "test/blur", &glsl, false).unwrap();

		let src = ctx.create_texture(16, 1).unwrap();
		let dst = ctx.create_texture(16, 1).unwrap();
		let step = f32_frame(16, 1, |px| {
			if px < 8 {
				[0.0, 0.0, 0.0, 1.0]
			} else {
				[1.0, 1.0, 1.0, 1.0]
			}
		});
		ctx.upload(src, &step).unwrap();

		// radius 0: passthrough.
		let mut row = oak_node::value::NodeValueRow::new();
		row.insert("method_in".into(), oak_node::value::NodeValue::Combo(0));
		row.insert("radius_in".into(), oak_node::value::NodeValue::Float(0.0));
		row.insert("horiz_in".into(), oak_node::value::NodeValue::Boolean(true));
		row.insert("vert_in".into(), oak_node::value::NodeValue::Boolean(false));
		run_effect(&ctx, &effect, &row, &[("tex_in".to_string(), src)], dst, (16, 1), 1).unwrap();
		let out = ctx.download(dst).unwrap();
		assert_eq!(out.data, step.data, "radius 0 is a passthrough");

		// radius 2 horizontal box: out(x) = 0.5 * (in[x-1] + in[x+1]).
		row.insert("radius_in".into(), oak_node::value::NodeValue::Float(2.0));
		run_effect(&ctx, &effect, &row, &[("tex_in".to_string(), src)], dst, (16, 1), 1).unwrap();
		let out = ctx.download(dst).unwrap();
		for x in 0..16usize {
			let got = pixel(&out, x)[0];
			let want = match x {
				0 => 0.0,   // first tap out of bounds (repeat_edge off)
				7 | 8 => 0.5,
				15 => 0.5,  // second tap out of bounds
				_ if x < 7 => 0.0,
				_ => 1.0,
			};
			assert!(
				(got - want).abs() < 1e-4,
				"px {x}: got {got}, want {want}"
			);
		}
		ctx.destroy_texture(src);
		ctx.destroy_texture(dst);
	}
	/// The polygon generator's shader rasterizes the default pentagon on
	/// the GPU: the center texel inside the closed point loop is opaque
	/// white, the frame corner is transparent, and the 1-point fallback
	/// renders nothing.
	#[test]
	fn gpu_polygon_rasterizes_pentagon() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.polygon")
			.unwrap();
		let glsl = behavior.shader_code("rgb").unwrap();
		let effect = compile_effect(&ctx, "test/polygon", &glsl, false).unwrap();

		let dst = ctx.create_texture(512, 512).unwrap();
		let mut row = oak_node::value::NodeValueRow::new();
		row.insert(
			"points_in".into(),
			oak_node::value::NodeValue::Vec4Array(vec![
				[0.0, -135.0, 0.0, 0.0],
				[135.0, -45.0, 0.0, 0.0],
				[90.0, 120.0, 0.0, 0.0],
				[-90.0, 120.0, 0.0, 0.0],
				[-135.0, -45.0, 0.0, 0.0],
			]),
		);
		row.insert("point_count".into(), oak_node::value::NodeValue::Int(5));
		row.insert(
			"color_in".into(),
			oak_node::value::NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
		);
		run_effect(&ctx, &effect, &row, &[], dst, (512, 512), 1).unwrap();
		let out = ctx.download(dst).unwrap();

		let center = pixel(&out, 256 * 512 + 256);
		assert_eq!(center, [1.0, 1.0, 1.0, 1.0], "center is inside the pentagon");
		let corner = pixel(&out, 0);
		assert_eq!(corner, [0.0, 0.0, 0.0, 0.0], "corner is outside");

		// Degenerate: a single point draws nothing.
		row.insert("point_count".into(), oak_node::value::NodeValue::Int(1));
		run_effect(&ctx, &effect, &row, &[], dst, (512, 512), 1).unwrap();
		let out = ctx.download(dst).unwrap();
		assert_eq!(
			pixel(&out, 256 * 512 + 256),
			[0.0, 0.0, 0.0, 0.0],
			"single point draws nothing"
		);

		ctx.destroy_texture(dst);
	}

	/// The mask effect's shader multiplies the base texture by the
	/// pentagon matte on the GPU: the center texel keeps the base value,
	/// the corner is cleared, and with `invert_in` the result flips.
	#[test]
	fn gpu_mask_multiplies_base_by_pentagon() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.mask")
			.unwrap();
		let glsl = behavior.shader_code("mask").unwrap();
		let effect = compile_effect(&ctx, "test/mask", &glsl, false).unwrap();

		let src = ctx.create_texture(512, 512).unwrap();
		let dst = ctx.create_texture(512, 512).unwrap();
		let gray = crate::shaderfx::tests::f32_frame(512, 512, |_| {
			[0.5, 0.5, 0.5, 1.0]
		});
		ctx.upload(src, &gray).unwrap();

		let mut row = oak_node::value::NodeValueRow::new();
		row.insert(
			"points_in".into(),
			oak_node::value::NodeValue::Vec4Array(vec![
				[0.0, -135.0, 0.0, 0.0],
				[135.0, -45.0, 0.0, 0.0],
				[90.0, 120.0, 0.0, 0.0],
				[-90.0, 120.0, 0.0, 0.0],
				[-135.0, -45.0, 0.0, 0.0],
			]),
		);
		row.insert("point_count".into(), oak_node::value::NodeValue::Int(5));
		row.insert("feather_in".into(), oak_node::value::NodeValue::Float(0.0));
		row.insert("invert_in".into(), oak_node::value::NodeValue::Boolean(false));
		run_effect(
			&ctx,
			&effect,
			&row,
			&[("base_in".to_string(), src)],
			dst,
			(512, 512),
			1,
		)
		.unwrap();
		let out = ctx.download(dst).unwrap();
		let center = pixel(&out, 256 * 512 + 256);
		assert_eq!(center, [0.5, 0.5, 0.5, 1.0], "center keeps the base");
		assert_eq!(pixel(&out, 0), [0.0, 0.0, 0.0, 0.0], "corner is masked out");

		// Inverted: the corner keeps the base, the center is cleared.
		row.insert("invert_in".into(), oak_node::value::NodeValue::Boolean(true));
		run_effect(
			&ctx,
			&effect,
			&row,
			&[("base_in".to_string(), src)],
			dst,
			(512, 512),
			1,
		)
		.unwrap();
		let out = ctx.download(dst).unwrap();
		assert_eq!(pixel(&out, 0), [0.5, 0.5, 0.5, 1.0], "inverted corner keeps base");
		assert_eq!(
			pixel(&out, 256 * 512 + 256),
			[0.0, 0.0, 0.0, 0.0],
			"inverted center cleared"
		);

		// Feather: a square polygon with radius 4 softens the edge — the
		// pixel right outside the crisp edge becomes partially visible.
		row.insert("invert_in".into(), oak_node::value::NodeValue::Boolean(false));
		row.insert("feather_in".into(), oak_node::value::NodeValue::Float(4.0));
		row.insert(
			"points_in".into(),
			oak_node::value::NodeValue::Vec4Array(vec![
				[-150.0, -150.0, 0.0, 0.0],
				[150.0, -150.0, 0.0, 0.0],
				[150.0, 150.0, 0.0, 0.0],
				[-150.0, 150.0, 0.0, 0.0],
			]),
		);
		row.insert("point_count".into(), oak_node::value::NodeValue::Int(4));
		run_effect(
			&ctx,
			&effect,
			&row,
			&[("base_in".to_string(), src)],
			dst,
			(512, 512),
			1,
		)
		.unwrap();
		let out = ctx.download(dst).unwrap();
		let soft = pixel(&out, 408 * 512 + 256)[0];
		assert!(
			soft > 0.02 && soft < 0.6,
			"just outside the feathered edge is partially visible: {soft}"
		);

		ctx.destroy_texture(src);
		ctx.destroy_texture(dst);
	}

	/// The linear grading node's OCIO-spliced shader doubles gray under
	/// +1 stop of master exposure (2^1 * 0.2 = 0.4) and gates the
	/// transform behind the localBypass uniform off.
	#[test]
	fn gpu_grading_linear_applies_exposure() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let Some(stub) = crate::eval::grading_stub_for(
			"org.olivevideoeditor.Olive.ociogradingtransformlinear",
		) else {
			eprintln!("no OCIO config; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.ociogradingtransformlinear")
			.unwrap();
		let glsl = behavior.shader_code(&stub).unwrap();
		let effect = compile_effect(&ctx, "test/grading-lin", &glsl, false).unwrap();

		let src = ctx.create_texture(8, 4).unwrap();
		let dst = ctx.create_texture(8, 4).unwrap();
		let gray = crate::shaderfx::tests::f32_frame(8, 4, |_| [0.2, 0.2, 0.2, 1.0]);
		ctx.upload(src, &gray).unwrap();

		let mut row = oak_node::value::NodeValueRow::new();
		row.insert(
			"ocio_grading_primary_exposure".into(),
			oak_node::value::NodeValue::Vec3([2.0, 2.0, 2.0]),
		);
		row.insert(
			"ocio_grading_primary_contrast".into(),
			oak_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
		);
		row.insert(
			"ocio_grading_primary_offset".into(),
			oak_node::value::NodeValue::Vec3([0.0, 0.0, 0.0]),
		);
		row.insert(
			"ocio_grading_primary_saturation".into(),
			oak_node::value::NodeValue::Float(1.0),
		);
		row.insert(
			"ocio_grading_primary_pivot".into(),
			oak_node::value::NodeValue::Float(0.18),
		);
		row.insert(
			"ocio_grading_primary_clampBlack".into(),
			oak_node::value::NodeValue::Float(-1.0),
		);
		row.insert(
			"ocio_grading_primary_clampWhite".into(),
			oak_node::value::NodeValue::Float(2.0),
		);
		row.insert(
			"ocio_grading_primary_localBypass".into(),
			oak_node::value::NodeValue::Boolean(false),
		);
		run_effect(
			&ctx,
			&effect,
			&row,
			&[("tex_in".to_string(), src)],
			dst,
			(8, 4),
			1,
		)
		.unwrap();
		let out = ctx.download(dst).unwrap();
		for px in 0..8usize {
			let got = pixel(&out, px);
			let want = [0.4f32, 0.4, 0.4, 1.0];
			assert!(
				(got[0] - want[0]).abs() < 1e-3,
				"px {px}: exposure must double gray, got {got:?}"
			);
		}

		ctx.destroy_texture(src);
		ctx.destroy_texture(dst);
	}

	/// The log grading node's OCIO-spliced shader: brightness (lift) of
	/// +0.1 shifts 0.2 gray to 0.3 with the identity gain/gamma/pivot
	/// defaults.
	#[test]
	fn gpu_grading_log_applies_lift() {
		let Some(ctx) = gpu() else {
			eprintln!("no adapter; skipping");
			return;
		};
		let Some(stub) = crate::eval::grading_stub_for(
			"org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog",
		) else {
			eprintln!("no OCIO config; skipping");
			return;
		};
		let (_core, behavior) = oak_node::factory::Factory::global()
			.create_any("org.olivevideoeditor.Olive.OCIO_NAMESPACEgradingtransformlog")
			.unwrap();
		let glsl = behavior.shader_code(&stub).unwrap();
		let effect = compile_effect(&ctx, "test/grading-log", &glsl, false).unwrap();

		let src = ctx.create_texture(8, 4).unwrap();
		let dst = ctx.create_texture(8, 4).unwrap();
		let gray = crate::shaderfx::tests::f32_frame(8, 4, |_| [0.2, 0.2, 0.2, 1.0]);
		ctx.upload(src, &gray).unwrap();

		let mut row = oak_node::value::NodeValueRow::new();
		row.insert(
			"ocio_grading_primary_brightness".into(),
			oak_node::value::NodeValue::Vec3([0.1, 0.1, 0.1]),
		);
		row.insert(
			"ocio_grading_primary_contrast".into(),
			oak_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
		);
		row.insert(
			"ocio_grading_primary_gamma".into(),
			oak_node::value::NodeValue::Vec3([1.0, 1.0, 1.0]),
		);
		row.insert(
			"ocio_grading_primary_saturation".into(),
			oak_node::value::NodeValue::Float(1.0),
		);
		row.insert(
			"ocio_grading_primary_pivot".into(),
			oak_node::value::NodeValue::Float(-0.2),
		);
		row.insert(
			"ocio_grading_primary_pivotBlack".into(),
			oak_node::value::NodeValue::Float(0.0),
		);
		row.insert(
			"ocio_grading_primary_pivotWhite".into(),
			oak_node::value::NodeValue::Float(1.0),
		);
		row.insert(
			"ocio_grading_primary_clampBlack".into(),
			oak_node::value::NodeValue::Float(-1.0),
		);
		row.insert(
			"ocio_grading_primary_clampWhite".into(),
			oak_node::value::NodeValue::Float(2.0),
		);
		row.insert(
			"ocio_grading_primary_localBypass".into(),
			oak_node::value::NodeValue::Boolean(false),
		);
		run_effect(
			&ctx,
			&effect,
			&row,
			&[("tex_in".to_string(), src)],
			dst,
			(8, 4),
			1,
		)
		.unwrap();
		let out = ctx.download(dst).unwrap();
		for px in 0..8usize {
			let got = pixel(&out, px);
			assert!(
				(got[0] - 0.3).abs() < 1e-3,
				"px {px}: lift must shift gray, got {got:?}"
			);
		}

		ctx.destroy_texture(src);
		ctx.destroy_texture(dst);
	}

	/// Every registered node type that ships a shader must translate (the
	/// all-shaders sweep). OCIO-stubbed shaders (`%1` markers needing the
	/// OCIO-generated function text) retry with the real OCIO stub first;
	/// only nodes whose stub is unavailable *and* unknown fail outright.
	#[test]
	fn all_registered_shaders_translate() {
		let mut ok = Vec::new();
		let mut ocio_stubbed = Vec::new();
		let mut failed = Vec::new();
		for meta in oak_node::factory::Factory::global().entries() {
			let (_core, behavior) = (meta.create)();
			let Some(glsl) = behavior.shader_code("") else {
				continue;
			};
			match translate(&glsl) {
				Ok(_) => ok.push(meta.type_id),
				Err(e) => {
					let msg = format!("{e:?}");
					// The unresolved OCIO stub is the one sanctioned
					// failure mode (chromakey & co. call into
					// OCIO-generated functions). Retry with the real OCIO
					// stub: the wiring must make these translate. When no
					// OCIO config is available (stub build), fall back to
					// a pass-through function so the sweep still covers
					// the node's own shader body.
				let retried = crate::eval::ocio_stub_for(meta.type_id)
					.or_else(|| {
						crate::eval::OCIO_SHADER_STUBS
							.iter()
							.find(|(id, ..)| *id == meta.type_id)
							.map(|(_, fn_name, ..)| {
								format!("vec4 {fn_name}(vec4 c) {{ return c; }}")
							})
					})
					.or_else(|| crate::eval::grading_stub_for(meta.type_id))
					.or_else(|| {
						crate::eval::OCIO_GRADING_STUBS
							.iter()
							.find(|(id, _)| *id == meta.type_id)
							.map(|_| "vec4 ove_grading_primary(vec4 c) { return c; }".to_string())
					});
				match retried {
					Some(stub) => match translate(&behavior.shader_code(&stub).unwrap()) {
						Ok(_) => ok.push(meta.type_id),
						Err(e) => {
							failed.push((meta.type_id, format!("with OCIO stub: {e:?}")))
						}
					},
					None if msg.contains("SceneLinear")
						|| msg.contains("UnknownFunction")
						|| msg.contains("ove_grading_primary") => {
						// Not in the stub table but still OCIO-shaped:
						// report separately, not as a regression.
						ocio_stubbed.push(meta.type_id);
					}
					None => failed.push((meta.type_id, msg)),
				}
				}
			}
		}
		eprintln!("shader sweep: {} ok, {} ocio-stubbed", ok.len(), ocio_stubbed.len());
		for id in &ocio_stubbed {
			eprintln!("  ocio-stubbed: {id}");
		}
		for (id, msg) in &failed {
			eprintln!("  FAILED: {id}: {}", &msg[..msg.len().min(200)]);
		}
		assert!(failed.is_empty(), "{} shaders failed to translate", failed.len());
		assert!(!ok.is_empty(), "no shaders translated at all");
	}
}
