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

//! clip 实例：clip ↔ oakrender 纹理桥。
//!
//! 对应 C++ 的 `OliveClipInstance`。纹理数据经
//! [`crate::render`]（oakrender 值类型：`Texture`/`Frame`）流动；
//! OFX 侧只看到 [`crate::image::Image`]（CPU 路径）。
//!
//! `#[repr(C)]` + props 在偏移 0（句柄约定，见 [`crate::suites::tag`]；
//! clip handle 即 `&props`）。
//!
//! 单库化后 oakrender 的 ffi 已删除：帧访问走
//! [`oak_core::texture::Texture::to_frame`] 值路径（GPU 纹理经后端
//! 下载、CPU 纹理克隆），帧释放随值 drop 自动发生（原
//! `texture_get_frame`/`frame_free` 句柄调用面随桩删除）。

use crate::instance::{OfxRangeD, OfxRectD, RenderScale};
use crate::property::PropertySet;

/// clip 实例。
#[repr(C)]
pub struct ClipInstance {
	/// 实例级 clip 属性（当前分量/位深/像素比，协商结果写入；
	/// 偏移 0，句柄约定）。
	pub props: PropertySet,
	/// clip 名。
	pub name: String,
	/// 当前输入纹理（oakrender 值；输出 clip 为 None）。
	input_texture: std::sync::Mutex<Option<crate::render::Texture>>,
	/// 当前输出纹理（C++ `output_textures_` 的 phase 1 单槽；
	/// [`store_output_image`](Self::store_output_image) 的回写目标；
	/// 输入 clip 为 None）。
	output_texture: std::sync::Mutex<Option<crate::render::Texture>>,
}

/// 从 clip 属性读协商分量（getClipPreferences 写入）。
fn components_from_props(props: &PropertySet) -> Option<crate::image::Components> {
	use crate::property::Value;
	match props.get(crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS, 0)? {
		Value::String(s) => match s.to_string_lossy().as_ref() {
			"OfxImageComponentRGBA" => Some(crate::image::Components::Rgba),
			"OfxImageComponentRGB" => Some(crate::image::Components::Rgb),
			"OfxImageComponentAlpha" => Some(crate::image::Components::Alpha),
			_ => None,
		},
		_ => None,
	}
}

/// IEEE 754 半精度 → 单精度（[`crate::image::f16_to_f32`] 的本地别名，
/// 保持调用点可读）。
fn f16_to_f32(bits: u16) -> f32 {
	crate::image::f16_to_f32(bits)
}

impl ClipInstance {
	/// 按描述符实例化（createInstance 路径调用；公开：宿主与测试
	/// 都需要构造 clip 实例）。实例 props 是描述符 props 的深拷贝
	/// （HS: ClipBase 的实例构造，ofxhClip.cpp:57-70——插件在实例期
	/// 读 supported components 等）。
	///
	/// ofxColour（M11 §4）：输入 clip 的 kOfxImageClipPropColourspace
	/// 由宿主写为工作空间（ACEScg，ofxColour.h "Hosts should set this
	/// property to the colourspace of the input clip. Typically it will
	/// be set to the working colourspace"）；输出 clip 由
	/// GetOutputColourspace action 后写。
	pub fn from_descriptor(desc: &crate::descriptor::ClipDescriptor) -> Self {
		let props = desc.props.clone();
		let name = desc.name.clone();
		if name != "Output" {
			// The working colorspace follows the pipeline setting (project
			// property): ACEScg in the default pipeline, sRGB in the legacy
			// pass-through mode — plugins must be told the true space of the
			// pixels they receive.
			props.set_one(
				crate::host::PROP_CLIP_COLOURSPACE,
				crate::property::Value::String(
                    std::ffi::CString::new(oak_core::color::pipeline_working_ofx_name()).unwrap(),
				),
			);
		}
		// OfxImageClipPropConnected（ofxsImageEffect.cpp:1106 的
		// `Clip::isConnected()` 是无默认值强读——缺这个属性时，CImg 这类
		// 带可选 mask clip 的插件直接抛
		// PropertyUnknownToHost → MissingHostFeature 紫帧）。默认 0
		// （未连接），挂接输入纹理时由 set_input_texture 置 1。
		props.set_one(
			crate::host::PROP_CLIP_CONNECTED,
			crate::property::Value::Int(0),
		);
		Self {
			props,
			name,
			input_texture: std::sync::Mutex::new(None),
			output_texture: std::sync::Mutex::new(None),
		}
	}

	/// 写协商后的像素格式（C++ `OliveClipInstance::setParams` 的 props
	/// 侧：setPixelDepth/setComponents，oliveclip.cpp:700-709）。
	/// `format` 为 olive::PixelFormat::Format（0=u8, 2=u16, 3=f16,
	/// 4=f32）；`channels` 为分量数。
	pub fn set_video_params(&self, format: i32, channels: i32) {
		let depth = match format {
			0 => "OfxBitDepthByte",
			2 => "OfxBitDepthShort",
			3 => "OfxBitDepthHalf",
			_ => "OfxBitDepthFloat",
		};
		let comps = match channels {
			1 => "OfxImageComponentAlpha",
			3 => "OfxImageComponentRGB",
			_ => "OfxImageComponentRGBA",
		};
		self.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH,
			crate::property::Value::String(std::ffi::CString::new(depth).unwrap()),
		);
		self.props.set_one(
			crate::image::K_IMAGE_EFFECT_PROP_COMPONENTS,
			crate::property::Value::String(std::ffi::CString::new(comps).unwrap()),
		);
	}

	/// 写协商 RoD（C++ `setRegionOfDefinition` 的单槽版，
	/// oliveclip.cpp:674-678——C++ 按 time 存 map，本驱动一帧一槽；
	/// 落点与 image effect suite 的 clipGetRegionOfDefinition 读取处
	/// 一致）。
	pub fn set_region_of_definition(&self, rod: OfxRectD, _time: f64) {
		use crate::property::Value;
		self.props.define(
			"OfxImageEffectPropRegionOfDefinition",
			vec![
				Value::Double(rod.x1),
				Value::Double(rod.y1),
				Value::Double(rod.x2),
				Value::Double(rod.y2),
			],
		);
	}

	/// 挂接输入纹理（oaknode 侧 clip 输入值变化时由 param/render 桥
	/// 调用）。`time` 用于多帧纹理选择。None 断开。
	pub fn set_input_texture(&self, texture: Option<crate::render::Texture>, _time: f64) {
		// The connection state follows the texture hand-off (the render
		// driver only feeds clips that have input; optional mask clips
		// stay 0, so `Clip::isConnected()` answers false for them).
		let connected = i32::from(texture.is_some());
		self.props.set_one(
			crate::host::PROP_CLIP_CONNECTED,
			crate::property::Value::Int(connected),
		);
		*self.input_texture.lock().unwrap_or_else(|e| e.into_inner()) = texture;
	}

	/// 挂接输出纹理（render 驱动创建并经值传入；C++
	/// `setOutputTexture` 的 phase 1 单槽版）。`time` 用于多帧纹理
	/// 选择（`// [P2]`）。None 断开。
	pub fn set_output_texture(&self, texture: Option<crate::render::Texture>, _time: f64) {
		*self
			.output_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner()) = texture;
	}

	/// 抓取本 clip 在 `time` 的图像（OFX clipGetImage 的宿主侧）。
	/// CPU 路径：把 oakrender 纹理 readback 成 [`crate::image::Image`]
	/// （像素格式按协商结果，全链路 F32）。
	/// `// [P2]` GL 路径：clipLoadTexture 语义在此扩展。
	///
	/// 输入帧支持 U8/U16/F16/F32：非 F32 归一化转换为 F32（对齐
	/// oliveclip.cpp setInputTexture 的格式转换路径）；转换中的
	/// NaN/Inf 清洗为 0（oliveclip.cpp copy_pixels 的 scrub）。
	/// `region` 只支持 None（整帧）——子区域随 renderer 桥落地。
	pub fn fetch_image(
		&self,
		time: f64,
		scale: RenderScale,
		region: Option<OfxRectD>,
	) -> crate::error::Result<crate::image::Image> {
		use crate::error::Error;
		use crate::render::PIXEL_FORMAT_F32;

		let _ = (time, scale);
		if region.is_some() {
			return Err(Error::Failed("fetch_image 子区域第 1 期不支持".into()));
		}
		let texture = self
			.input_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.ok_or(Error::NotFound)?;
		// 占位纹理（dummy）：视作无输入。
		if texture.is_dummy() {
			return Err(Error::NotFound);
		}
		// 纹理 → CPU 帧（GPU 纹理后端下载；帧随 drop 释放）。
		let frame = crate::render::texture_get_frame(&texture)?;
		let params = frame.video_params();
		let format = params.format;
		if format != PIXEL_FORMAT_F32
			&& format != crate::render::PIXEL_FORMAT_U8
			&& format != oak_core::PixelFormat::U16 as i32
			&& format != oak_core::PixelFormat::F16 as i32
		{
			return Err(Error::Failed(format!(
				"输入帧格式 {format} 不支持（仅 U8/U16/F16/F32）"
			)));
		}
		let (w, h) = (params.width as f64, params.height as f64);
		// 分量按协商结果（getClipPreferences 已写入 clip.props）。
		let components =
			components_from_props(&self.props).unwrap_or(crate::image::Components::Rgba);

		let mut image = crate::image::Image::allocate(
			crate::image::BitDepth::Float,
			components,
			OfxRectD {
				x1: 0.0,
				y1: 0.0,
				x2: w,
				y2: h,
			},
		);
		let src = frame.data();
		if src.is_null() {
			return Err(Error::Failed("帧无数据".into()));
		}
		// 行优先 + 格式转换（帧行跨度经 linesize 读取——真实
		// oakrender 帧可有行填充；目标 Image 恒紧凑 F32）。
		// U8/U16/F16 输入归一化到 [0,1] 浮点（对齐 oliveclip.cpp
		// setInputTexture 的 swscale 转换路径：插件侧永远见到协商位
		// 深）；F32/转换结果中的 NaN/Inf 清洗为 0（oliveclip.cpp
		// copy_pixels 的 scrub——CImg 对 NaN 未定义行为）。
		let channels = components.channel_count();
		let samples_per_row = (w as usize) * channels;
		let src_bpc = match format {
			f if f == crate::render::PIXEL_FORMAT_U8 => 1,
			f if f == oak_core::PixelFormat::U16 as i32 => 2,
			f if f == oak_core::PixelFormat::F16 as i32 => 2,
			_ => 4,
		};
		let tight_src = samples_per_row * src_bpc;
		let row = frame.linesize_bytes();
		let row = if row > 0 { row } else { tight_src };
		let src_bytes = unsafe { std::slice::from_raw_parts(src, row * h as usize) };
		let dst = image.pixels_mut();
		let mut scrubbed = false;
		for y in 0..h as usize {
			let s = y * row;
			for i in 0..samples_per_row {
				let v = match format {
					f if f == crate::render::PIXEL_FORMAT_U8 => {
						src_bytes[s + i] as f32 / 255.0
					}
					f if f == oak_core::PixelFormat::U16 as i32 => {
						let off = s + i * 2;
						let bits = u16::from_le_bytes([src_bytes[off], src_bytes[off + 1]]);
						bits as f32 / 65535.0
					}
					f if f == oak_core::PixelFormat::F16 as i32 => {
						let off = s + i * 2;
						let bits = u16::from_le_bytes([src_bytes[off], src_bytes[off + 1]]);
						let v = f16_to_f32(bits);
						if v.is_nan() || v.is_infinite() {
							scrubbed = true;
							0.0
						} else {
							v
						}
					}
					_ => {
						let off = s + i * 4;
						let v = f32::from_le_bytes([
							src_bytes[off],
							src_bytes[off + 1],
							src_bytes[off + 2],
							src_bytes[off + 3],
						]);
						if v.is_nan() || v.is_infinite() {
							scrubbed = true;
							0.0
						} else {
							v
						}
					}
				};
				let d = (y * samples_per_row + i) * 4;
				dst[d..d + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		if scrubbed {
			eprintln!("[PLUGIN] NaN/Inf scrubbed from input frame data during fetch");
		}
		// 位深协商（设计约束：管线全链路 ACEScg + F32；插件不支持
		// F32 时输入图像转成协商位深——输出端在 render 驱动转回
		// F32）。clip props 的 PixelDepth 由协商流程
		// （set_video_params）写入；缺省 = F32。
		let negotiated = self
			.props
			.get(crate::image::K_IMAGE_EFFECT_PROP_PIXEL_DEPTH, 0)
			.and_then(|v| match v {
				crate::property::Value::String(s) => {
					crate::image::BitDepth::from_ofx(&s.to_string_lossy())
				}
				_ => None,
			})
			.unwrap_or(crate::image::BitDepth::Float);
		if negotiated != crate::image::BitDepth::Float {
			image = image.convert_depth(negotiated);
		}
		Ok(image)
	}

	/// 把输出图像回写为 oakrender 纹理（render 完成后由
	/// [`crate::instance::Instance::render`] 的调用方使用）。
	///
	/// 输出纹理由 oakrender 侧创建并经 [`Self::set_output_texture`]
	/// 挂入——本函数取该纹理的 CPU 帧（GPU 纹理经后端下载，写回后
	/// 对 `Texture::Gpu` 再经
	/// [`oak_core::backend::GpuContextLike::upload`] 上传），按帧
	/// 参数校验 F32 与尺寸后整帧拷贝图像像素（全链路 F32；C++
	/// pluginrenderer 的 `readback/wrap` 路径第 1 期以 CPU 拷贝表达，
	/// GL 走 [`crate::render`] 的 `// [P2]`）。未挂输出纹理
	/// 或纹理为占位（dummy）→ [`crate::error::Error::NotFound`]。
	/// 成功返回纹理值（克隆，随 drop 释放）。
	pub fn store_output_image(
		&self,
		image: &crate::image::Image,
	) -> crate::error::Result<crate::render::Texture> {
		use crate::error::Error;
		use crate::render::{texture_get_frame, PIXEL_FORMAT_F32};

		let texture = self
			.output_texture
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.clone()
			.ok_or(Error::NotFound)?;
		if texture.is_dummy() {
			return Err(Error::NotFound);
		}
		let mut frame = texture_get_frame(&texture)?;
		let params = frame.video_params();
		if params.format != PIXEL_FORMAT_F32 {
			return Err(Error::Failed(format!(
				"输出帧格式 {} 非 F32（第 1 期约束）",
				params.format
			)));
		}
		let (w, h) = (params.width as usize, params.height as usize);
		// 图像与帧必须同尺寸（全链路 F32；宽高/行宽/总长逐项校验）。
		let tight = w * image.components().channel_count() * 4;
		if tight != image.row_bytes() || tight * h != image.pixels().len() {
			return Err(Error::Failed("图像尺寸与输出帧不一致".into()));
		}
		let dst = frame.data_mut();
		if dst.is_null() {
			return Err(Error::Failed("输出帧无数据".into()));
		}
		// 行优先拷贝（目标帧行跨度经 linesize 读取——真实 oakrender
		// 帧可有行填充；M11 §4 修复同 fetch_image）。
		let row = frame.linesize_bytes();
		let row = if row > 0 { row } else { tight };
		let dst_bytes = unsafe { std::slice::from_raw_parts_mut(dst, row * h) };
		let pixels = image.pixels();
		for y in 0..h {
			let d = y * row;
			let s = y * tight;
			dst_bytes[d..d + tight].copy_from_slice(&pixels[s..s + tight]);
		}
		// GPU 目标纹理：拷贝只落在下载帧上，经后端 upload 回写
		// （CPU 纹理无需上传）。
		if let crate::render::Texture::Gpu { token, ctx, .. } = &texture {
			ctx.upload(*token, &frame)
				.map_err(|e| Error::Failed(format!("输出纹理上传失败：{e}")))?;
		}
		Ok(texture)
	}

	/// 本 clip 的时间域（clipGetFrameRange）。
	///
	/// `// TODO(value-model)`：输入范围经 oakrender 帧的时间基推导
	/// （time_base）——随 clip 迁移到 `oak_core::texture::Texture`
	/// 值模型落地。
	pub fn frame_range(&self) -> crate::error::Result<OfxRangeD> {
		let _ = OfxRangeD::default();
		Err(crate::error::Error::Failed(
			"frame_range 待 renderer 桥".into(),
		))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn f16_to_f32_covers_special_values() {
		// 常规值：1.0 = 0x3C00，-2.0 = 0xC000，0.5 = 0x3800。
		assert_eq!(f16_to_f32(0x3C00), 1.0);
		assert_eq!(f16_to_f32(0xC000), -2.0);
		assert_eq!(f16_to_f32(0x3800), 0.5);
		// 零与负零。
		assert_eq!(f16_to_f32(0x0000), 0.0);
		assert_eq!(f16_to_f32(0x8000).to_bits(), (0.0f32).to_bits() | (1 << 31));
		// 非规格数：最小正规格数 2^-14 ≈ 0.00006104；2^-24 是最小非
		// 规格数之一。
		assert!((f16_to_f32(0x0400) - 2f32.powi(-14)).abs() < 1e-12);
		assert!((f16_to_f32(0x0001) - 2f32.powi(-24)).abs() < 1e-12);
		// Inf/NaN。
		assert!(f16_to_f32(0x7C00).is_infinite() && f16_to_f32(0x7C00) > 0.0);
		assert!(f16_to_f32(0xFC00).is_infinite() && f16_to_f32(0xFC00) < 0.0);
		assert!(f16_to_f32(0x7E00).is_nan());
	}
}
