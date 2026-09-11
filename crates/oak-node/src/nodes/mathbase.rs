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

//! Shared base for binary math nodes (C++
//! `src/node/src/math/math/mathbase.{h,cpp}`, `olive::MathNodeBase`).
//! Not an instantiable node — modeled as a helper module: the C++
//! class carries no data members, only the operation/pairing enums,
//! the pairing heuristic, and the static eval/shader helpers shared
//! by `MathNode` (and conceptually other binary math nodes).

use crate::jobs::{Job, ShaderJobPayload};
use crate::node::NodeCore;
use crate::value::{NodeValue, NodeValueRow, NodeValueTable, ValueType};

/// Binary operation (C++ `MathNodeBase::Operation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
	/// Addition (C++ `k_op_add`).
	Add,
	/// Subtraction (C++ `k_op_subtract`).
	Subtract,
	/// Multiplication (C++ `k_op_multiply`).
	Multiply,
	/// Division (C++ `k_op_divide`).
	Divide,
	/// Exponentiation (C++ `k_op_power`).
	Power,
}

/// Operand type pairing (C++ `MathNodeBase::Pairing`; discriminants
/// match the C++ enum because they are serialized into shader ids as
/// `"<op>.<pairing>.<type_a>.<type_b>"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
	/// No valid pairing (C++ `k_pair_none = -1`).
	None = -1,
	/// number op number.
	NumberNumber,
	/// vec op vec.
	VecVec,
	/// matrix op matrix.
	MatrixMatrix,
	/// color op color.
	ColorColor,
	/// texture op texture.
	TextureTexture,
	/// vec op number.
	VecNumber,
	/// matrix op vec.
	MatrixVec,
	/// number op color.
	NumberColor,
	/// texture op number.
	TextureNumber,
	/// texture op color.
	TextureColor,
	/// texture op matrix.
	TextureMatrix,
	/// samples op samples.
	SampleSample,
	/// samples op number.
	SampleNumber,
}

impl Pairing {
	/// `k_pair_count` (C++ sentinel one past the last pairing).
	const COUNT: i32 = Pairing::SampleNumber as i32 + 1;
}

/// Pairing heuristic (C++ `MathNodeBase::PairingCalculator`):
/// inspects the two operand value tables and picks the most likely
/// [`Pairing`] plus the concrete values to feed the operation.
pub struct PairingCalculator {
	/// Chosen pairing (C++ `most_likely_pairing_`).
	pub most_likely_pairing: Pairing,
	/// Chosen operand A (C++ `most_likely_value_a_`).
	pub most_likely_value_a: NodeValue,
	/// Chosen operand B (C++ `most_likely_value_b_`).
	pub most_likely_value_b: NodeValue,
}

impl PairingCalculator {
	/// Build from the two operand tables (C++ `PairingCalculator()`
	/// constructor): per-table `get_pair_likelihood` weight vectors are
	/// combined (longer table gets a weight bonus), and the highest
	/// non-negative combined likelihood wins; the winning operands are
	/// copied out of the tables.
	pub fn new(table_a: &NodeValueTable, table_b: &NodeValueTable) -> Self {
		let likelihood_a = get_pair_likelihood(table_a);
		let likelihood_b = get_pair_likelihood(table_b);

		let weight_a = (table_b.count() as i64 - table_a.count() as i64).max(0);
		let weight_b = (table_a.count() as i64 - table_b.count() as i64).max(0);

		let mut likelihoods = vec![-1i64; Pairing::COUNT as usize];
		for i in 0..Pairing::COUNT as usize {
			if likelihood_a[i] == -1 || likelihood_b[i] == -1 {
				likelihoods[i] = -1;
			} else {
				likelihoods[i] = likelihood_a[i] + weight_a + likelihood_b[i] + weight_b;
			}
		}

		// Pick the highest strictly-non-negative likelihood (first wins
		// ties, matching the C++ `>` comparison from `k_pair_none`).
		let mut most_likely: Pairing = Pairing::None;
		let mut best = -1i64;
		for i in 0..Pairing::COUNT as usize {
			if likelihoods[i] > best {
				best = likelihoods[i];
				most_likely = cpp_pairing(i as i32);
			}
		}

		let mut value_a = NodeValue::None;
		let mut value_b = NodeValue::None;
		if most_likely != Pairing::None && best >= 0 {
			// The winning likelihood's row index is the table index that
			// produced it: the C++ stores it in the likelihood vector and
			// re-reads `table_a.at(...)`. Recompute the same index here.
			let idx_a = likelihood_a[most_likely as i32 as usize];
			let idx_b = likelihood_b[most_likely as i32 as usize];
			if idx_a >= 0 && (idx_a as usize) < table_a.count() {
				value_a = table_a.rows()[idx_a as usize].1.clone();
			}
			if idx_b >= 0 && (idx_b as usize) < table_b.count() {
				value_b = table_b.rows()[idx_b as usize].1.clone();
			}
		}

		PairingCalculator {
			most_likely_pairing: most_likely,
			most_likely_value_a: value_a,
			most_likely_value_b: value_b,
		}
	}

	/// Whether a pairing was found (C++ `found_most_likely_pairing()`).
	pub fn found_most_likely_pairing(&self) -> bool {
		(self.most_likely_pairing as i32) > Pairing::None as i32
			&& (self.most_likely_pairing as i32) < Pairing::COUNT
	}
}

/// Per-table likelihood vector (C++
/// `PairingCalculator::get_pair_likelihood`): row index `i` becomes the
/// weight for every pairing the row's value type can serve; -1 = not
/// eligible. `k_pair_count` entries.
fn get_pair_likelihood(table: &NodeValueTable) -> Vec<i64> {
	let mut likelihood = vec![-1i64; Pairing::COUNT as usize];
	for (i, (ty, _, _)) in table.rows().iter().enumerate() {
		let weight = i as i64;
		let set = |p: Pairing, likelihood: &mut Vec<i64>| {
			likelihood[p as i32 as usize] = weight;
		};
		match ty {
			ValueType::Vec2 | ValueType::Vec3 | ValueType::Vec4 => {
				set(Pairing::VecVec, &mut likelihood);
				set(Pairing::VecNumber, &mut likelihood);
				set(Pairing::MatrixVec, &mut likelihood);
			}
			ValueType::Matrix => {
				set(Pairing::MatrixMatrix, &mut likelihood);
				set(Pairing::MatrixVec, &mut likelihood);
				set(Pairing::TextureMatrix, &mut likelihood);
			}
			ValueType::Color => {
				set(Pairing::ColorColor, &mut likelihood);
				set(Pairing::NumberColor, &mut likelihood);
				set(Pairing::TextureColor, &mut likelihood);
			}
			ValueType::Int | ValueType::Float | ValueType::Rational => {
				set(Pairing::NumberNumber, &mut likelihood);
				set(Pairing::VecNumber, &mut likelihood);
				set(Pairing::NumberColor, &mut likelihood);
				set(Pairing::TextureNumber, &mut likelihood);
				set(Pairing::SampleNumber, &mut likelihood);
			}
			ValueType::Samples => {
				set(Pairing::SampleSample, &mut likelihood);
				set(Pairing::SampleNumber, &mut likelihood);
			}
			ValueType::Texture => {
				set(Pairing::TextureTexture, &mut likelihood);
				set(Pairing::TextureNumber, &mut likelihood);
				set(Pairing::TextureColor, &mut likelihood);
				set(Pairing::TextureMatrix, &mut likelihood);
			}
			_ => {}
		}
	}
	likelihood
}

/// C++ `Pairing` discriminant -> [`Pairing`] (the enum mirrors the C++
/// discriminants, so this is a plain cast).
fn cpp_pairing(v: i32) -> Pairing {
	// The Rust enum discriminants are identical to the C++ enum; this is
	// only reachable with `0..=12`, which all map to real variants.
	match v {
		0 => Pairing::NumberNumber,
		1 => Pairing::VecVec,
		2 => Pairing::MatrixMatrix,
		3 => Pairing::ColorColor,
		4 => Pairing::TextureTexture,
		5 => Pairing::VecNumber,
		6 => Pairing::MatrixVec,
		7 => Pairing::NumberColor,
		8 => Pairing::TextureNumber,
		9 => Pairing::TextureColor,
		10 => Pairing::TextureMatrix,
		11 => Pairing::SampleSample,
		_ => Pairing::SampleNumber,
	}
}

/// C++ `NodeValue::Type` discriminant -> [`ValueType`] for the pairings
/// the shader path handles.
fn cpp_type(v: i32) -> ValueType {
	match v {
		1 => ValueType::Int,
		2 => ValueType::Float,
		3 => ValueType::Rational,
		5 => ValueType::Color,
		6 => ValueType::Matrix,
		10 => ValueType::Texture,
		11 => ValueType::Samples,
		12 => ValueType::Vec2,
		13 => ValueType::Vec3,
		_ => ValueType::Vec4,
	}
}

/// Stateless helper namespace for the C++ `MathNodeBase` static
/// methods (unit-like: the C++ base class has no data members).
pub struct MathNodeBase;

impl MathNodeBase {
	/// Display name for an operation (C++ `get_operation_name()`):
	/// Add/Subtract/Multiply/Divide/Power.
	pub fn operation_name(op: Operation) -> &'static str {
		match op {
			Operation::Add => "Add",
			Operation::Subtract => "Subtract",
			Operation::Multiply => "Multiply",
			Operation::Divide => "Divide",
			Operation::Power => "Power",
		}
	}

	/// Whether applying `op` with scalar `number` is an identity
	/// (C++ `number_is_no_op()`): 0 for add/subtract, fuzzy-1.0 for
	/// multiply/divide/power (`// CPP-PARITY: mathbase.cpp`
	/// `qFuzzyCompare(number, 1.0f)` — factor 1e5, float overload).
	pub fn number_is_no_op(op: Operation, number: f32) -> bool {
		match op {
			Operation::Add | Operation::Subtract => number == 0.0,
			Operation::Multiply | Operation::Divide | Operation::Power => {
				(number - 1.0f32).abs() * 100000.0f32 <= number.abs().min(1.0f32)
			}
		}
	}

	/// Shared shader generator (C++ `get_shader_code_internal()`):
	/// parses the `"<op>.<pairing>.<type_a>.<type_b>"` shader id,
	/// builds a fragment shader declaring both operands as uniforms and
	/// applying the GLSL operator (`pow()` for power, with a vec4-wrapped
	/// exponent for texture-number); the texture*matrix multiply case
	/// instead emits a no-op fragment plus a vertex shader that
	/// multiplies `gl_Position` by the matrix uniform. Returns
	/// `(frag, vert)`.
	pub fn shader_code_internal(
		shader_id: &str,
		param_a_in: &str,
		param_b_in: &str,
	) -> (String, String) {
		let parts: Vec<i32> = shader_id
			.split('.')
			.map(|p| p.parse().unwrap_or(0))
			.collect();
		if parts.len() < 4 {
			return (String::new(), String::new());
		}
		let op = match parts[0] {
			0 => Operation::Add,
			1 => Operation::Subtract,
			2 => Operation::Multiply,
			3 => Operation::Divide,
			_ => Operation::Power,
		};
		let pairing = cpp_pairing(parts[1]);
		let type_a = cpp_type(parts[2]);
		let type_b = cpp_type(parts[3]);

		let (operation, vert) = if pairing == Pairing::TextureMatrix && op == Operation::Multiply {
			// Override the operation for this operation since we multiply
			// texture COORDS by the matrix rather than the color.
			let (tex_in, mat_in) = if type_a == ValueType::Texture {
				(param_a_in, param_b_in)
			} else {
				(param_b_in, param_a_in)
			};

			let operation = format!("texture({}, ove_texcoord)", tex_in);
			let vert = format!(
				"uniform mat4 {};\n\
				 \n\
				 in vec4 a_position;\n\
				 in vec2 a_texcoord;\n\
				 \n\
				 out vec2 ove_texcoord;\n\
				 \n\
				 void main() {{\n\
				     gl_Position = {} * a_position;\n\
				     ove_texcoord = a_texcoord;\n\
				 }}\n",
				mat_in, mat_in
			);
			(operation, vert)
		} else {
			let var_a = shader_variable_call(param_a_in, type_a);
			let var_b = shader_variable_call(param_b_in, type_b);
			let operation = match op {
				Operation::Add => format!("{} + {}", var_a, var_b),
				Operation::Subtract => format!("{} - {}", var_a, var_b),
				Operation::Multiply => format!("{} * {}", var_a, var_b),
				Operation::Divide => format!("{} / {}", var_a, var_b),
				Operation::Power => {
					if pairing == Pairing::TextureNumber {
						// The "number" in this operation has to be declared a vec4
						if type_a.is_numeric() {
							format!("pow({}, vec4({}))", var_b, var_a)
						} else {
							format!("pow({}, vec4({}))", var_a, var_b)
						}
					} else {
						format!("pow({}, {})", var_a, var_b)
					}
				}
			};
			(operation, String::new())
		};

		let frag = format!(
			"uniform {} {};\n\
			 uniform {} {};\n\
			 \n\
			 in vec2 ove_texcoord;\n\
			 out vec4 frag_color;\n\
			 \n\
			 void main(void) {{\n\
			     vec4 c = {};\n\
			     c.a = clamp(c.a, 0.0, 1.0);\n\
			     frag_color = c;\n\
			 }}\n",
			shader_uniform_type(type_a),
			param_a_in,
			shader_uniform_type(type_b),
			param_b_in,
			operation
		);

		(frag, vert)
	}

	/// Shared value evaluation (C++ `value_internal()`): big switch on
	/// the pairing — number/number (preserving rationals unless power),
	/// vec/vec (zero-padding divide guard), matrix*vec, vec/number,
	/// matrix/matrix, color+/-color, color*number, sample buffers
	/// (elementwise, longer tail memcpy'd), texture pairings (shader job
	/// payload boxed in the texture value with `"op.pairing.ta.tb"` id;
	/// no-op push-through when the texture is null, the number is
	/// identity, or the matrix is identity), and sample*number (static:
	/// in-place SIMD loop; dynamic: sample job).
	///
	/// `core`/`inputs` carry the node's input state so the sample*number
	/// branch can tell a static number (in-place transform) from a
	/// dynamic one (deferred sample job), mirroring the C++ `this`
	/// member access in `is_input_static(number_param)`. `time` is the
	/// job's request timestamp and `type_id` the emitting behavior's
	/// type id — the caller (e.g. `MathNode::value`) forwards its `time`
	/// argument and `self.type_id()`.
	pub fn value_internal(
		operation: Operation,
		pairing: Pairing,
		param_a_in: &str,
		val_a: &NodeValue,
		param_b_in: &str,
		val_b: &NodeValue,
		time: oak_core::Rational,
		type_id: &str,
		core: &NodeCore,
		inputs: &NodeValueRow,
		output: &mut NodeValueTable,
	) {
		match pairing {
			Pairing::NumberNumber => {
				if let (NodeValue::Rational(a), NodeValue::Rational(b)) = (val_a, val_b) {
					if operation != Operation::Power {
						// Preserve rationals.
						let r = add_sub_mult_div_rational(operation, *a, *b);
						output.push(ValueType::Rational, NodeValue::Rational(r), None);
						return;
					}
				}
				let n = perform_all_f32(
					operation,
					Self::retrieve_number(val_a),
					Self::retrieve_number(val_b),
				);
				output.push(ValueType::Float, NodeValue::Float(n as f64), None);
			}

			Pairing::VecVec => {
				let mut vec_a = retrieve_vector(val_a);
				let mut vec_b = retrieve_vector(val_b);

				if operation == Operation::Divide {
					// Lower-dimensional vectors are padded with zeros;
					// dividing the padding components would be 0/0 (NaN).
					// Force them to 0/1 so the result is a well-defined
					// zero, which is discarded by push_vector anyway.
					let max_type = vec_max_type(val_a, val_b);
					if max_type == ValueType::Vec2 {
						vec_a[2] = 0.0;
						vec_a[3] = 0.0;
						vec_b[2] = 1.0;
						vec_b[3] = 1.0;
					} else if max_type == ValueType::Vec3 {
						vec_a[3] = 0.0;
						vec_b[3] = 1.0;
					}
				}

				let result = add_sub_mult_div_vec(operation, vec_a, vec_b);
				push_vector(output, vec_max_type(val_a, val_b), result);
			}

			Pairing::MatrixVec => {
				let (matrix, vec) = if val_a.value_type() == ValueType::Matrix {
					(matrix_value(val_a), retrieve_vector(val_b))
				} else {
					(matrix_value(val_b), retrieve_vector(val_a))
				};

				// Only valid operation is multiply (`v * m`).
				let result = if operation == Operation::Multiply {
					vec_mul_matrix(vec, matrix)
				} else {
					vec
				};
				push_vector(output, vec_max_type(val_a, val_b), result);
			}

			Pairing::VecNumber => {
				let (vec, number) = if val_a.value_type().is_vector() {
					(retrieve_vector(val_a), Self::retrieve_number(val_b))
				} else {
					(retrieve_vector(val_b), Self::retrieve_number(val_a))
				};
				let ty = if val_a.value_type().is_vector() {
					val_a.value_type()
				} else {
					val_b.value_type()
				};

				// Only multiply and divide are valid operations.
				let result = mult_div_vec_number(operation, vec, number);
				push_vector(output, ty, result);
			}

			Pairing::MatrixMatrix => {
				let mat_a = matrix_value(val_a);
				let mat_b = matrix_value(val_b);
				let result = add_sub_mult_matrix(operation, mat_a, mat_b);
				output.push(ValueType::Matrix, NodeValue::Matrix(result), None);
			}

			Pairing::ColorColor => {
				let col_a = match val_a {
					NodeValue::Color(c) => *c,
					_ => [0.0; 4],
				};
				let col_b = match val_b {
					NodeValue::Color(c) => *c,
					_ => [0.0; 4],
				};
				// Only add and subtract are valid operations.
				let result = add_sub_color(operation, col_a, col_b);
				output.push(ValueType::Color, NodeValue::Color(result), None);
			}

			Pairing::NumberColor => {
				let (col, num) = if val_a.value_type() == ValueType::Color {
					(
						match val_a {
							NodeValue::Color(c) => *c,
							_ => [0.0; 4],
						},
						val_b.to_double(),
					)
				} else {
					(
						match val_b {
							NodeValue::Color(c) => *c,
							_ => [0.0; 4],
						},
						val_a.to_double(),
					)
				};
				// Only multiply and divide are valid operations.
				let result = mult_color_number(operation, col, num);
				output.push(ValueType::Color, NodeValue::Color(result), None);
			}

			Pairing::SampleSample => {
				let (samples_a, samples_b) = match (val_a, val_b) {
					(NodeValue::Samples(a), NodeValue::Samples(b)) => (a, b),
					_ => return,
				};

				let max_samples = samples_a.sample_count.max(samples_b.sample_count);
				let min_samples = samples_a.sample_count.min(samples_b.sample_count);
				let channels = samples_a.channels;

				let mut mixed = crate::value::SampleBuffer {
					format: samples_a.format,
					channels,
					sample_count: max_samples,
					data: vec![
						0u8;
						max_samples * channels * samples_a.format.bytes_per_sample().max(1)
					],
				};

				for c in 0..channels {
					for j in 0..min_samples {
						let v = perform_all_f32(
							operation,
							samples_a.sample_value(c, j) as f32,
							samples_b.sample_value(c, j) as f32,
						);
						mixed.set_sample_value(c, j, v as f64);
					}
				}

				if max_samples > min_samples {
					// Fill in the remainder space from the larger buffer.
					let larger = if max_samples == samples_a.sample_count {
						samples_a
					} else {
						samples_b
					};
					for c in 0..channels {
						for j in min_samples..max_samples {
							mixed.set_sample_value(c, j, larger.sample_value(c, j));
						}
					}
				}

				output.push(ValueType::Samples, NodeValue::Samples(mixed), None);
			}

			Pairing::TextureColor
			| Pairing::TextureNumber
			| Pairing::TextureTexture
			| Pairing::TextureMatrix => {
				let (number_val, texture_val) = if val_a.value_type() == ValueType::Texture {
					(val_b, val_a)
				} else {
					(val_a, val_b)
				};
				let texture = match texture_val {
					NodeValue::Texture(h) if !h.is_null() => Some(h),
					_ => None,
				};

				let mut operation_is_noop = false;

				if texture.is_none() {
					operation_is_noop = true;
				} else if pairing == Pairing::TextureNumber {
					if Self::number_is_no_op(operation, Self::retrieve_number(number_val)) {
						operation_is_noop = true;
					}
				} else if pairing == Pairing::TextureMatrix {
					// Only allow matrix multiplication. The C++ also
					// remaps the matrix from texture space into sequence
					// space via `TransformDistortNode::adjust_matrix_by_resolutions`
					// before the identity check; that needs the sequence
					// video params, which this crate's value() trait does
					// not carry, so the raw matrix is checked here.
					// `// CPP-PARITY: mathbase.cpp` texture_matrix branch.
					let matrix = matrix_value(number_val);
					if operation != Operation::Multiply || matrix_is_identity(matrix) {
						operation_is_noop = true;
					}
				}

				if operation_is_noop {
					// Just push texture as-is.
					output.push(ValueType::Texture, texture_val.clone(), None);
				} else {
					// Push a texture value boxing the deferred shader job
					// (C++ `Texture::job(ShaderJob)`), which the renderer
					// seam (`traverser::RenderHooks::resolve`) downcasts
					// and resolves via `get_shader_code`. The id encodes
					// the operation, the pairing, and the actual operand
					// types; the params row carries the two operands under
					// their input ids, mirroring C++ `job.Insert(param_a_in,
					// val_a); job.Insert(param_b_in, val_b)`.
					// `// CPP-PARITY: mathbase.cpp` texture pairings.
					let shader_id = format!(
						"{}.{}.{}.{}",
						operation as i32,
						pairing as i32,
						val_a.value_type().to_cpp_discriminant(),
						val_b.value_type().to_cpp_discriminant()
					);
					output.push(
						ValueType::Texture,
						NodeValue::Texture(crate::handle::make_owned(Job::ShaderJob(ShaderJobPayload {
							node_id: crate::id::NodeId::INVALID,
							time,
							iterations: 1,
							type_id: type_id.to_string(),
							shader_id,
							effect_input: core.effect_input.clone(),
							params: NodeValueRow::from([
								(param_a_in.to_string(), val_a.clone()),
								(param_b_in.to_string(), val_b.clone()),
							]),
							iterative_input: String::new(),
						}))),
						None,
					);
				}
			}

			Pairing::SampleNumber => {
				let (samples_val, number_param, number) =
					if val_a.value_type() == ValueType::Samples {
						(val_a, param_b_in, Self::retrieve_number(val_b))
					} else {
						(val_b, param_a_in, Self::retrieve_number(val_a))
					};

				let buffer = match samples_val {
					NodeValue::Samples(b) => b.clone(),
					_ => return,
				};

				if buffer.is_allocated() {
					if core.is_input_static(inputs, number_param, -1) {
						if !Self::number_is_no_op(operation, number) {
							// In-place transform of every sample.
							let mut transformed = buffer;
							for c in 0..transformed.channels {
								for i in 0..transformed.sample_count {
									let v = perform_all_f32(
										operation,
										transformed.sample_value(c, i) as f32,
										number,
									);
									transformed.set_sample_value(c, i, v as f64);
								}
							}
							output.push(ValueType::Samples, NodeValue::Samples(transformed), None);
						} else {
							output.push(ValueType::Samples, NodeValue::Samples(buffer), None);
						}
					} else {
						// Dynamic number: the C++ queues a `SampleJob`
						// deferred to the audio renderer (which applies the
						// per-index value via `process_samples`); here the
						// samples flow through unchanged for the renderer
						// seam to process.
						// `// CPP-PARITY: mathbase.cpp` sample_number branch.
						output.push(ValueType::Samples, NodeValue::Samples(buffer), None);
					}
				}
			}

			Pairing::None => {}
		}
	}

	/// Shared per-sample processing for the sample*number pairing
	/// (C++ `process_samples_internal()`): reads the scalar from
	/// `param_a_in` (falling back to `param_b_in`) and applies the
	/// operation to the input sample at `index` on every channel.
	pub fn process_samples_internal(
		values: &NodeValueRow,
		operation: Operation,
		param_a_in: &str,
		param_b_in: &str,
		input: &crate::value::SampleBuffer,
		output: &mut crate::value::SampleBuffer,
		index: usize,
	) {
		let mut number_val = values.get(param_a_in);
		if !is_scalar(number_val) {
			number_val = values.get(param_b_in);
			if !is_scalar(number_val) {
				return;
			}
		}
		let number_flt = Self::retrieve_number(number_val.unwrap());

		for i in 0..output.channels {
			let v = perform_all_f32(operation, input.sample_value(i, index) as f32, number_flt);
			output.set_sample_value(i, index, v as f64);
		}
	}

	/// Extract a number from any numeric/rational value (C++
	/// `retrieve_number()`): rationals convert through
	/// `Rational::to_double`, everything else through
	/// [`NodeValue::to_double`].
	pub fn retrieve_number(val: &NodeValue) -> f32 {
		match val {
			NodeValue::Rational(r) => r.to_f64() as f32,
			_ => val.to_double() as f32,
		}
	}
}

/// `perform_all` scalar operation (C++ template instantiated for
/// `float`): add/sub/mul/div and `pow`.
fn perform_all_f32(operation: Operation, a: f32, b: f32) -> f32 {
	match operation {
		Operation::Add => a + b,
		Operation::Subtract => a - b,
		Operation::Multiply => a * b,
		Operation::Divide => a / b,
		Operation::Power => a.powf(b),
	}
}

/// `perform_add_sub_mult_div<Rational, Rational>` — add/sub/mul/div on
/// rationals; power is unsupported and returns `a` unchanged.
fn add_sub_mult_div_rational(
	operation: Operation,
	a: oak_core::Rational,
	b: oak_core::Rational,
) -> oak_core::Rational {
	match operation {
		Operation::Add => a + b,
		Operation::Subtract => a - b,
		Operation::Multiply => a * b,
		Operation::Divide => a / b,
		Operation::Power => a,
	}
}

/// C++ `retrieve_vector`: widen any vector into a `[f32; 4]`
/// (Vec2/3 zero-padded; Vec4 and everything else pass through).
fn retrieve_vector(val: &NodeValue) -> [f32; 4] {
	match val {
		NodeValue::Vec2(v) => [v[0] as f32, v[1] as f32, 0.0, 0.0],
		NodeValue::Vec3(v) => [v[0] as f32, v[1] as f32, v[2] as f32, 0.0],
		NodeValue::Vec4(v) => [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32],
		_ => [0.0; 4],
	}
}

/// C++ `push_vector`: narrow a `[f32; 4]` back into the target vec type.
fn push_vector(output: &mut NodeValueTable, ty: ValueType, vec: [f32; 4]) {
	match ty {
		ValueType::Vec2 => output.push(
			ValueType::Vec2,
			NodeValue::Vec2([vec[0] as f64, vec[1] as f64]),
			None,
		),
		ValueType::Vec3 => output.push(
			ValueType::Vec3,
			NodeValue::Vec3([vec[0] as f64, vec[1] as f64, vec[2] as f64]),
			None,
		),
		ValueType::Vec4 => output.push(
			ValueType::Vec4,
			NodeValue::Vec4([vec[0] as f64, vec[1] as f64, vec[2] as f64, vec[3] as f64]),
			None,
		),
		_ => {}
	}
}

/// The larger of two vec value types by C++ enum discriminant
/// (Vec2 < Vec3 < Vec4; used for the vec pairing output type).
fn vec_max_type(a: &NodeValue, b: &NodeValue) -> ValueType {
	if a.value_type().to_cpp_discriminant() >= b.value_type().to_cpp_discriminant() {
		a.value_type()
	} else {
		b.value_type()
	}
}

/// `perform_add_sub_mult_div<Vector4D, Vector4D>` — element-wise.
fn add_sub_mult_div_vec(operation: Operation, a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
	let mut out = a;
	for i in 0..4 {
		out[i] = match operation {
			Operation::Add => a[i] + b[i],
			Operation::Subtract => a[i] - b[i],
			Operation::Multiply => a[i] * b[i],
			Operation::Divide => a[i] / b[i],
			Operation::Power => a[i],
		};
	}
	out
}

/// `perform_mult_div<Vector4D, float>` — multiply/divide only; other
/// operations return the vector unchanged.
fn mult_div_vec_number(operation: Operation, a: [f32; 4], b: f32) -> [f32; 4] {
	let mut out = a;
	for i in 0..4 {
		out[i] = match operation {
			Operation::Multiply => a[i] * b,
			Operation::Divide => a[i] / b,
			_ => a[i],
		};
	}
	out
}

/// `perform_add_sub<Color, Color>` — add/subtract only.
fn add_sub_color(operation: Operation, a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
	match operation {
		Operation::Add => [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]],
		Operation::Subtract => [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]],
		_ => a,
	}
}

/// `perform_mult<Color, float>` — multiply/divide only.
fn mult_color_number(operation: Operation, a: [f64; 4], b: f64) -> [f64; 4] {
	match operation {
		Operation::Multiply => [a[0] * b, a[1] * b, a[2] * b, a[3] * b],
		Operation::Divide => [a[0] / b, a[1] / b, a[2] / b, a[3] / b],
		_ => a,
	}
}

/// Matrix operand -> row-major `[f64; 16]` (identity default).
fn matrix_value(val: &NodeValue) -> [f64; 16] {
	match val {
		NodeValue::Matrix(m) => *m,
		_ => identity_matrix(),
	}
}

/// The 4x4 identity matrix (C++ `Matrix4x4()` default).
pub fn identity_matrix() -> [f64; 16] {
	let mut m = [0.0f64; 16];
	m[0] = 1.0;
	m[5] = 1.0;
	m[10] = 1.0;
	m[15] = 1.0;
	m
}

/// Whether a row-major matrix equals the identity (C++
/// `Matrix4x4::is_identity`, which compares against a fresh identity
/// matrix).
pub fn matrix_is_identity(m: [f64; 16]) -> bool {
	m == identity_matrix()
}

/// `perform_add_sub_mult<Matrix4x4, Matrix4x4>` — add/subtract
/// element-wise, multiply as a matrix product.
fn add_sub_mult_matrix(operation: Operation, a: [f64; 16], b: [f64; 16]) -> [f64; 16] {
	match operation {
		Operation::Add => {
			let mut out = [0.0; 16];
			for i in 0..16 {
				out[i] = a[i] + b[i];
			}
			out
		}
		Operation::Subtract => {
			let mut out = [0.0; 16];
			for i in 0..16 {
				out[i] = a[i] - b[i];
			}
			out
		}
		Operation::Multiply => {
			let mut out = [0.0; 16];
			for r in 0..4 {
				for c in 0..4 {
					let mut acc = 0.0;
					for k in 0..4 {
						acc += a[r * 4 + k] * b[k * 4 + c];
					}
					out[r * 4 + c] = acc;
				}
			}
			out
		}
		_ => a,
	}
}

/// `perform_mult<Vector4D, Matrix4x4>` (multiply only) — `v * m`
/// (C++ `mathtypes.h` `operator*(const Vector4D &, const Matrix4x4 &)`):
/// `result[i] = sum_j v[j] * m[j][i]` with row-major storage.
pub fn vec_mul_matrix(v: [f32; 4], m: [f64; 16]) -> [f32; 4] {
	let mut out = [0.0f32; 4];
	for i in 0..4 {
		let mut acc = 0.0f64;
		for j in 0..4 {
			acc += v[j] as f64 * m[j * 4 + i];
		}
		out[i] = acc as f32;
	}
	out
}

/// Whether a row value can serve as the process_samples scalar (C++
/// checks `type() != k_none`; a connected samples operand on
/// `param_a_in` — as VolumeNode passes `samples_in` first — is not a
/// scalar, so we fall through to the number operand.
/// `// CPP-PARITY: mathbase.cpp` process_samples_internal).
fn is_scalar(val: Option<&NodeValue>) -> bool {
	match val {
		None => false,
		Some(NodeValue::None) => false,
		Some(v) => v.value_type().is_numeric(),
	}
}

/// C++ `get_shader_uniform_type`.
fn shader_uniform_type(ty: ValueType) -> &'static str {
	match ty {
		ValueType::Texture => "sampler2D",
		ValueType::Color => "vec4",
		ValueType::Matrix => "mat4",
		_ => "float",
	}
}

/// C++ `get_shader_variable_call`: textures sample at the texcoord.
fn shader_variable_call(input_id: &str, ty: ValueType) -> String {
	if ty == ValueType::Texture {
		format!("texture({}, ove_texcoord)", input_id)
	} else {
		input_id.to_string()
	}
}

impl ValueType {
	/// Whether the type is a vector (C++ `NodeValue::type_is_vector`).
	pub fn is_vector(self) -> bool {
		matches!(self, ValueType::Vec2 | ValueType::Vec3 | ValueType::Vec4)
	}

	/// Whether the type is numeric (C++ `NodeValue::type_is_numeric`).
	pub fn is_numeric(self) -> bool {
		matches!(
			self,
			ValueType::Float | ValueType::Int | ValueType::Rational
		)
	}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{NodeValueTable, SampleBuffer};
    use oak_core::{Rational, SampleFormat};

    fn table(values: Vec<NodeValue>) -> NodeValueTable {
		let mut t = NodeValueTable::default();
		for v in values {
			let ty = v.value_type();
			t.push(ty, v, None);
		}
		t
	}

	#[test]
	fn operation_names() {
		assert_eq!(MathNodeBase::operation_name(Operation::Add), "Add");
		assert_eq!(
			MathNodeBase::operation_name(Operation::Subtract),
			"Subtract"
		);
		assert_eq!(
			MathNodeBase::operation_name(Operation::Multiply),
			"Multiply"
		);
		assert_eq!(MathNodeBase::operation_name(Operation::Divide), "Divide");
		assert_eq!(MathNodeBase::operation_name(Operation::Power), "Power");
	}

	#[test]
	fn number_is_no_op_semantics() {
		// Add/subtract: exactly 0.0.
		assert!(MathNodeBase::number_is_no_op(Operation::Add, 0.0));
		assert!(MathNodeBase::number_is_no_op(Operation::Subtract, 0.0));
		assert!(!MathNodeBase::number_is_no_op(Operation::Add, 0.5));
		// Multiply/divide/power: fuzzy 1.0 (float factor 1e5).
		assert!(MathNodeBase::number_is_no_op(Operation::Multiply, 1.0));
		assert!(MathNodeBase::number_is_no_op(Operation::Divide, 1.0));
		assert!(MathNodeBase::number_is_no_op(Operation::Power, 1.0));
		assert!(MathNodeBase::number_is_no_op(
			Operation::Multiply,
			1.0 + 0.0000001
		));
		assert!(!MathNodeBase::number_is_no_op(Operation::Multiply, 2.0));
		assert!(!MathNodeBase::number_is_no_op(Operation::Divide, 0.0));
	}

	#[test]
	fn pairing_calculator_picks_number_number() {
		let a = table(vec![NodeValue::Float(2.0)]);
		let b = table(vec![NodeValue::Float(3.0)]);
		let p = PairingCalculator::new(&a, &b);
		assert!(p.found_most_likely_pairing());
		assert_eq!(p.most_likely_pairing, Pairing::NumberNumber);
		assert_eq!(p.most_likely_value_a, NodeValue::Float(2.0));
		assert_eq!(p.most_likely_value_b, NodeValue::Float(3.0));
	}

	#[test]
	fn pairing_calculator_empty_tables_find_nothing() {
		let a = NodeValueTable::default();
		let b = NodeValueTable::default();
		let p = PairingCalculator::new(&a, &b);
		assert!(!p.found_most_likely_pairing());
		assert_eq!(p.most_likely_pairing, Pairing::None);
		assert_eq!(p.most_likely_value_a, NodeValue::None);
	}

	#[test]
	fn pairing_calculator_vector_pairing() {
		let a = table(vec![NodeValue::Vec2([1.0, 2.0])]);
		let b = table(vec![NodeValue::Vec2([3.0, 4.0])]);
		let p = PairingCalculator::new(&a, &b);
		assert_eq!(p.most_likely_pairing, Pairing::VecVec);
	}

	#[test]
	fn pairing_calculator_length_weight_bonus() {
		// Two rows vs one: the two-row table wins the tie for a pairing
		// both can serve.
		let a = table(vec![NodeValue::Float(1.0), NodeValue::Float(2.0)]);
		let b = table(vec![NodeValue::Float(3.0)]);
		let p = PairingCalculator::new(&a, &b);
		assert_eq!(p.most_likely_pairing, Pairing::NumberNumber);
	}

	#[test]
	fn value_internal_number_number_float() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::NumberNumber,
			"a",
			&NodeValue::Float(2.0),
			"b",
			&NodeValue::Float(3.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(out.get(ValueType::Float), Some(&NodeValue::Float(5.0)));
	}

	#[test]
	fn value_internal_number_number_rational_preserved() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::NumberNumber,
			"a",
			&NodeValue::Rational(Rational::new(1, 2)),
			"b",
			&NodeValue::Rational(Rational::new(1, 3)),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Rational),
			Some(&NodeValue::Rational(Rational::new(5, 6)))
		);
		// Power falls through to float.
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Power,
			Pairing::NumberNumber,
			"a",
			&NodeValue::Rational(Rational::new(2, 1)),
			"b",
			&NodeValue::Rational(Rational::new(3, 1)),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(out.get(ValueType::Float), Some(&NodeValue::Float(8.0)));
	}

	#[test]
	fn value_internal_vec_vec_divide_padding_guard() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Divide,
			Pairing::VecVec,
			"a",
			&NodeValue::Vec2([1.0, 4.0]),
			"b",
			&NodeValue::Vec2([2.0, 2.0]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(out.get(ValueType::Vec2), Some(&NodeValue::Vec2([0.5, 2.0])));
	}

	#[test]
	fn value_internal_vec_number_multiply() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::VecNumber,
			"a",
			&NodeValue::Vec3([1.0, 2.0, 3.0]),
			"b",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Vec3),
			Some(&NodeValue::Vec3([2.0, 4.0, 6.0]))
		);
	}

	#[test]
	fn value_internal_matrix_vec() {
		// Translate is stored at row 0, col 3 (C++ `Matrix4x4::translate`).
		// `v * m` computes result[i] = sum_j v[j] * m[j][i], so the
		// translation lands in the w component, not x.
		let mut m = [0.0f64; 16];
		m[0] = 1.0;
		m[5] = 1.0;
		m[10] = 1.0;
		m[15] = 1.0;
		m[3] = 100.0; // m[0][3] translation x
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::MatrixVec,
			"a",
			&NodeValue::Matrix(m),
			"b",
			&NodeValue::Vec2([10.0, 20.0]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		// x' = 10*m[0][0] = 10; y' = 20*m[1][1] = 20.
		assert_eq!(
			out.get(ValueType::Vec2),
			Some(&NodeValue::Vec2([10.0, 20.0]))
		);
	}

	#[test]
	fn value_internal_color_ops() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::ColorColor,
			"a",
			&NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
			"b",
			&NodeValue::Color([0.5, 0.5, 0.0, 0.0]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Color),
			Some(&NodeValue::Color([1.5, 0.5, 0.0, 1.0]))
		);

		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::NumberColor,
			"a",
			&NodeValue::Float(0.5),
			"b",
			&NodeValue::Color([1.0, 1.0, 1.0, 1.0]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Color),
			Some(&NodeValue::Color([0.5, 0.5, 0.5, 0.5]))
		);
	}

	fn f32_planar(channels: usize, count: usize, values: &[f32]) -> SampleBuffer {
		let mut buf = SampleBuffer {
			format: SampleFormat::F32Planar,
			channels,
			sample_count: count,
			data: vec![0u8; channels * count * 4],
		};
		for c in 0..channels {
			for i in 0..count {
				let v = values.get(c * count + i).copied().unwrap_or(0.0);
				buf.set_sample_value(c, i, v as f64);
			}
		}
		buf
	}

	#[test]
	fn value_internal_sample_sample_mix() {
		let a = NodeValue::Samples(f32_planar(1, 4, &[1.0, 2.0, 3.0, 4.0]));
		let b = NodeValue::Samples(f32_planar(1, 2, &[0.5, 0.5]));
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::SampleSample,
			"a",
			&a,
			"b",
			&b,
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		let mixed = match out.get(ValueType::Samples).unwrap() {
			NodeValue::Samples(s) => s,
			_ => panic!("samples expected"),
		};
		assert_eq!(mixed.sample_count, 4);
		assert_eq!(mixed.sample_value(0, 0), 1.5);
		assert_eq!(mixed.sample_value(0, 1), 2.5);
		assert_eq!(mixed.sample_value(0, 2), 3.0, "tail copied from a");
		assert_eq!(mixed.sample_value(0, 3), 4.0);
	}

	#[test]
	fn value_internal_sample_number_static() {
		let mut buf = f32_planar(2, 2, &[1.0, 2.0, 3.0, 4.0]);
		let samples = NodeValue::Samples(buf.clone());
		let mut out = NodeValueTable::default();
		let core = NodeCore::new();
		let inputs = NodeValueRow::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::SampleNumber,
			"samples_in",
			&samples,
			"number_in",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&core,
			&inputs,
			&mut out,
		);
		let out_buf = match out.get(ValueType::Samples).unwrap() {
			NodeValue::Samples(s) => s,
			_ => panic!("samples expected"),
		};
		assert_eq!(out_buf.sample_value(0, 0), 2.0);
		assert_eq!(out_buf.sample_value(0, 1), 4.0);
		assert_eq!(out_buf.sample_value(1, 0), 6.0);
		assert_eq!(out_buf.sample_value(1, 1), 8.0);
		// The static fast path must not mutate the input.
		assert_eq!(buf.sample_value(0, 0), 1.0);
	}

	#[test]
	fn value_internal_sample_number_noop_passes_through() {
		let buf = f32_planar(1, 2, &[1.0, 2.0]);
		let samples = NodeValue::Samples(buf.clone());
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::SampleNumber,
			"samples_in",
			&samples,
			"number_in",
			&NodeValue::Float(1.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(out.get(ValueType::Samples), Some(&NodeValue::Samples(buf)));
	}

	#[test]
	fn shader_code_number_number_add() {
		let (frag, vert) =
			MathNodeBase::shader_code_internal("0.0.2.2", "param_a_in", "param_b_in");
		assert!(vert.is_empty());
		assert!(frag.contains("uniform float param_a_in;"));
		assert!(frag.contains("uniform float param_b_in;"));
		assert!(frag.contains("vec4 c = param_a_in + param_b_in;"));
		assert!(frag.contains("c.a = clamp(c.a, 0.0, 1.0);"));
	}

	#[test]
	fn shader_code_texture_matrix_multiply() {
		let (frag, vert) = MathNodeBase::shader_code_internal("2.10.10.6", "tex_in", "mat_in");
		assert!(frag.contains("texture(tex_in, ove_texcoord)"));
		assert!(vert.contains("uniform mat4 mat_in;"));
		assert!(vert.contains("gl_Position = mat_in * a_position;"));
	}

	#[test]
	fn shader_code_power_texture_number_wraps_vec4() {
		// op=4 (power), pairing=8 (texture_number), type_a=10 (texture),
		// type_b=2 (float): pow(texture, vec4(number)).
		let (frag, _) = MathNodeBase::shader_code_internal("4.8.10.2", "tex_in", "num_in");
		assert!(frag.contains("pow(texture(tex_in, ove_texcoord), vec4(num_in))"));
	}

	#[test]
	fn process_samples_multiply() {
		let input = f32_planar(2, 3, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
		let mut output = SampleBuffer {
			format: SampleFormat::F32Planar,
			channels: 2,
			sample_count: 3,
			data: vec![0u8; 2 * 3 * 4],
		};
		let values = NodeValueRow::from([
			("samples_in".to_string(), NodeValue::Samples(input.clone())),
			("volume_in".to_string(), NodeValue::Float(0.5)),
		]);
		MathNodeBase::process_samples_internal(
			&values,
			Operation::Multiply,
			"samples_in",
			"volume_in",
			&input,
			&mut output,
			1,
		);
		assert_eq!(output.sample_value(0, 1), 1.0, "ch0 index1 = 2 * 0.5");
		assert_eq!(output.sample_value(1, 1), 2.5, "ch1 index1 = 5 * 0.5");
	}

	#[test]
	fn retrieve_number_rational_and_float() {
		assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Float(3.5)), 3.5);
		assert_eq!(
			MathNodeBase::retrieve_number(&NodeValue::Rational(Rational::new(1, 2))),
			0.5
		);
		assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Int(4)), 4.0);
	}

	#[test]
	fn matrix_helpers() {
		assert!(matrix_is_identity(identity_matrix()));

		let mut m = identity_matrix();
		m[3] = 5.0;
		assert!(!matrix_is_identity(m));
		let out = vec_mul_matrix([1.0, 0.0, 0.0, 1.0], m);
		// v * m: m[3] (row 0, col 3) contributes to the w component.
		assert_eq!(out[0], 1.0);
		assert_eq!(out[3], 6.0);

		// matrix * matrix: identity * identity = identity.
		let prod = add_sub_mult_matrix(Operation::Multiply, identity_matrix(), identity_matrix());
		assert!(matrix_is_identity(prod));
	}

	#[test]
	fn value_texture_number_noop_pushes_texture_as_is() {
		let tex = NodeValue::Texture(crate::handle::CHandle::null());
		// A null handle is treated as "no texture" -> noop passthrough.
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::TextureNumber,
			"tex_in",
			&tex,
			"num_in",
			&NodeValue::Float(1.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Texture),
			Some(&tex),
			"null texture passthrough"
		);
	}

	#[test]
	fn value_texture_number_identity_number_pushes_through() {
		// Non-null texture handle + identity number: noop -> passthrough.
		// Use a boxed handle so it is non-null but drops safely.
		let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::TextureNumber,
			"tex_in",
			&tex,
			"num_in",
			&NodeValue::Float(1.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		let pushed = out.get(ValueType::Texture).unwrap();
		match pushed {
			NodeValue::Texture(h) => assert!(!h.is_null()),
			_ => panic!("texture"),
		}
	}

	#[test]
	fn value_texture_number_pushes_job_payload() {
		let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::TextureNumber,
			"tex_in",
			&tex,
			"num_in",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		let handle = match out.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => *h,
			_ => panic!("texture expected"),
		};
		let payload = unsafe {
			crate::jobs::shader_job(&handle)
		}
		.expect("shader job payload boxed in the pushed texture");
		assert_eq!(payload.type_id, "org.olivevideoeditor.Olive.math");
		// op=2 (multiply), pairing=8 (texture_number), a=10 (texture),
		// b=2 (float).
		assert_eq!(payload.shader_id, "2.8.10.2");
		assert_eq!(payload.iterations, 1);
		assert_eq!(payload.effect_input, "");
		assert_eq!(payload.params.get("tex_in"), Some(&tex));
		assert_eq!(payload.params.get("num_in"), Some(&NodeValue::Float(2.0)));
	}

	#[test]
	fn value_texture_matrix_identity_noop() {
		let tex = crate::value::NodeValue::Texture(crate::handle::make_owned::<u8>(7));
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::TextureMatrix,
			"tex_in",
			&tex,
			"mat_in",
			&NodeValue::Matrix(identity_matrix()),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		match out.get(ValueType::Texture).unwrap() {
			NodeValue::Texture(h) => assert!(!h.is_null(), "identity matrix passthrough"),
			_ => panic!("texture"),
		}
	}

	#[test]
	fn value_sample_number_dynamic_passes_through() {
		// A keyframed number operand makes the input non-static.
		let mut core = NodeCore::new();
		core.keyframe_track_mut("num_in", -1)
			.set_key(crate::keyframe::Keyframe {
				time: oak_core::Rational::new(0, 1),
				value: NodeValue::Float(2.0),
				interpolation: crate::keyframe::Interpolation::Linear,
				bezier_in: (0.0, 0.0),
				bezier_out: (0.0, 0.0),
			});
		let buf = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32Planar,
			channels: 1,
			sample_count: 1,
			data: vec![0u8; 4],
		};
		let samples = NodeValue::Samples(buf);
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::SampleNumber,
			"samples_in",
			&samples,
			"num_in",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&core,
			&NodeValueRow::default(),
			&mut out,
		);
		match out.get(ValueType::Samples).unwrap() {
			NodeValue::Samples(s) => assert_eq!(s.sample_value(0, 0), 0.0),
			_ => panic!("samples"),
		}
	}

	#[test]
	fn value_vec_vec_promotes_type() {
		// vec2 + vec3 -> vec3.
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::VecVec,
			"a",
			&NodeValue::Vec2([1.0, 2.0]),
			"b",
			&NodeValue::Vec3([1.0, 1.0, 1.0]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Vec3),
			Some(&NodeValue::Vec3([2.0, 3.0, 1.0]))
		);
	}

	#[test]
	fn value_vec_number_divide() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Divide,
			Pairing::VecNumber,
			"a",
			&NodeValue::Vec2([4.0, 8.0]),
			"b",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(out.get(ValueType::Vec2), Some(&NodeValue::Vec2([2.0, 4.0])));
	}

	#[test]
	fn value_matrix_matrix_ops() {
		// Add and subtract are element-wise; multiply is the product.
		let mut a = identity_matrix();
		a[0] = 2.0;
		let b = identity_matrix();
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::MatrixMatrix,
			"a",
			&NodeValue::Matrix(a),
			"b",
			&NodeValue::Matrix(b),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		match out.get(ValueType::Matrix).unwrap() {
			NodeValue::Matrix(m) => assert_eq!(m[0], 2.0),
			_ => panic!("matrix"),
		}
	}

	#[test]
	fn value_rational_arithmetic_all_ops() {
        use oak_core::Rational;
        for (op, a, b, expect) in [
			(
				Operation::Add,
				Rational::new(1, 2),
				Rational::new(1, 3),
				Rational::new(5, 6),
			),
			(
				Operation::Subtract,
				Rational::new(1, 2),
				Rational::new(1, 3),
				Rational::new(1, 6),
			),
			(
				Operation::Multiply,
				Rational::new(2, 3),
				Rational::new(3, 4),
				Rational::new(1, 2),
			),
			(
				Operation::Divide,
				Rational::new(1, 2),
				Rational::new(1, 4),
				Rational::new(2, 1),
			),
		] {
			let mut out = NodeValueTable::default();
			MathNodeBase::value_internal(
				op,
				Pairing::NumberNumber,
				"a",
				&NodeValue::Rational(a),
				"b",
				&NodeValue::Rational(b),
				Rational::new(0, 1),
				"org.olivevideoeditor.Olive.math",
				&NodeCore::new(),
				&NodeValueRow::default(),
				&mut out,
			);
			assert_eq!(
				out.get(ValueType::Rational),
				Some(&NodeValue::Rational(expect))
			);
		}
	}

	#[test]
	fn pairing_calculator_sample_and_texture() {
		let a = table(vec![NodeValue::Samples(
			crate::value::SampleBuffer::default(),
		)]);
		let b = table(vec![NodeValue::Float(1.0)]);
		let p = PairingCalculator::new(&a, &b);
		assert_eq!(p.most_likely_pairing, Pairing::SampleNumber);

		let a = table(vec![NodeValue::Texture(crate::handle::CHandle::null())]);
		let b = table(vec![NodeValue::Texture(crate::handle::CHandle::null())]);
		let p = PairingCalculator::new(&a, &b);
		assert_eq!(p.most_likely_pairing, Pairing::TextureTexture);
	}

	#[test]
	fn shader_code_more_variants() {
		// Subtract (1) number-number: a - b.
		let (frag, _) = MathNodeBase::shader_code_internal("1.0.2.2", "a", "b");
		assert!(frag.contains("vec4 c = a - b;"));
		// Divide (3) texture-number: texture(a) / number.
		let (frag, _) = MathNodeBase::shader_code_internal("3.8.10.2", "a", "b");
		assert!(frag.contains("vec4 c = texture(a, ove_texcoord) / b;"));
		// Multiply (2) texture-texture: texture * texture.
		let (frag, _) = MathNodeBase::shader_code_internal("2.4.10.10", "a", "b");
		assert!(frag.contains("texture(a, ove_texcoord) * texture(b, ove_texcoord)"));
		// Power (4) number-color: pow(number, color) (number on the left).
		let (frag, _) = MathNodeBase::shader_code_internal("4.7.2.5", "a", "b");
		assert!(frag.contains("pow(a, b)"));
		// Malformed id returns empty strings.
		let (frag, vert) = MathNodeBase::shader_code_internal("not-an-id", "a", "b");
		assert!(frag.is_empty());
		assert!(vert.is_empty());
	}

	#[test]
	fn process_samples_falls_back_when_param_a_not_scalar() {
		let input = crate::value::SampleBuffer {
			format: oak_core::SampleFormat::F32Planar,
			channels: 1,
			sample_count: 1,
			data: vec![0u8; 4],
		};
		let mut output = input.clone();
		let values = NodeValueRow::from([
			("samples_in".to_string(), NodeValue::Samples(input.clone())),
			("volume_in".to_string(), NodeValue::Float(2.0)),
		]);
		// param_a_in is "samples_in" (not a scalar) -> falls back to volume.
		MathNodeBase::process_samples_internal(
			&values,
			Operation::Multiply,
			"samples_in",
			"volume_in",
			&input,
			&mut output,
			0,
		);
		// input value is 0.0 (empty data); no crash is the main check.
		let values2 = NodeValueRow::from([
			("samples_in".to_string(), NodeValue::Samples(input.clone())),
			("volume_in".to_string(), NodeValue::Float(2.0)),
		]);
		let mut input2 = input.clone();
		input2.set_sample_value(0, 0, 3.0);
		let mut output2 = input2.clone();
		MathNodeBase::process_samples_internal(
			&values2,
			Operation::Multiply,
			"samples_in",
			"volume_in",
			&input2,
			&mut output2,
			0,
		);
		assert_eq!(
			output2.sample_value(0, 0),
			6.0,
			"3 * 2 from the fallback scalar"
		);
	}

	#[test]
	fn retrieve_number_combo_and_bool() {
		assert_eq!(MathNodeBase::retrieve_number(&NodeValue::Combo(3)), 3.0);
		assert_eq!(
			MathNodeBase::retrieve_number(&NodeValue::Boolean(true)),
			1.0
		);
		assert_eq!(MathNodeBase::retrieve_number(&NodeValue::None), 0.0);
	}

	#[test]
	fn is_scalar_checks() {
		assert!(is_scalar(Some(&NodeValue::Float(1.0))));
		assert!(is_scalar(Some(&NodeValue::Rational(
			oak_core::Rational::new(1, 2)
		))));
		assert!(!is_scalar(Some(&NodeValue::Samples(
			crate::value::SampleBuffer::default()
		))));
		assert!(!is_scalar(Some(&NodeValue::None)));
		assert!(!is_scalar(None));
	}

	#[test]
	fn color_color_unsupported_op_returns_a() {
		// multiply is not valid for color+color -> returns a unchanged.
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Multiply,
			Pairing::ColorColor,
			"a",
			&NodeValue::Color([1.0, 2.0, 3.0, 4.0]),
			"b",
			&NodeValue::Color([0.5, 0.5, 0.5, 0.5]),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert_eq!(
			out.get(ValueType::Color),
			Some(&NodeValue::Color([1.0, 2.0, 3.0, 4.0]))
		);
	}

	#[test]
	fn value_internal_none_pairing_noop() {
		let mut out = NodeValueTable::default();
		MathNodeBase::value_internal(
			Operation::Add,
			Pairing::None,
			"a",
			&NodeValue::Float(1.0),
			"b",
			&NodeValue::Float(2.0),
			Rational::new(0, 1),
			"org.olivevideoeditor.Olive.math",
			&NodeCore::new(),
			&NodeValueRow::default(),
			&mut out,
		);
		assert!(out.is_empty());
	}
}
