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

//! 参数体系：13 种参数实例 + param ↔ oaknode 桥。
//!
//! 对应 C++ 的 `ParamInstance`/`OliveParamInstance`。桥的语义
//! （M9 已定）：节点输入值变化 → 写回 OFX 参数；OFX 参数被插件
//! 改动 → 经 oaknode 回写节点（undoable 经
//! `oak_undo::undocommand::UndoCommand`）。
//!
//! 句柄约定（[`crate::suites::tag`]）：`ParamDef`/`ParamInstance`
//! `#[repr(C)]` 且 `props` 在偏移 0；元素装箱（`Vec<Box<..>>`）保证
//! Vec 重分配不移动对象、句柄不悬垂。
//!
//! 节点值布局：随 [`crate::node`] 冻结（`include/node/node.h` 的
//! `oaknode_value` POD）；单库化后 oaknode 的身份注册表随其 ffi
//! 删除，回写路径是保留失败路径的本地桩（见 [`crate::node`]）。

use std::ffi::CString;

use crate::property::{PropertySet, Value};

// ---- OFX 参数类型字符串（ofxParam.h）----

/// kOfxParamTypeInteger。
pub const TYPE_INTEGER: &str = "OfxParamTypeInteger";
/// kOfxParamTypeInteger2D。
pub const TYPE_INTEGER2D: &str = "OfxParamTypeInteger2D";
/// kOfxParamTypeInteger3D。
pub const TYPE_INTEGER3D: &str = "OfxParamTypeInteger3D";
/// kOfxParamTypeDouble。
pub const TYPE_DOUBLE: &str = "OfxParamTypeDouble";
/// kOfxParamTypeDouble2D。
pub const TYPE_DOUBLE2D: &str = "OfxParamTypeDouble2D";
/// kOfxParamTypeDouble3D。
pub const TYPE_DOUBLE3D: &str = "OfxParamTypeDouble3D";
/// kOfxParamTypeBoolean。
pub const TYPE_BOOLEAN: &str = "OfxParamTypeBoolean";
/// kOfxParamTypeChoice。
pub const TYPE_CHOICE: &str = "OfxParamTypeChoice";
/// kOfxParamTypeString。
pub const TYPE_STRING: &str = "OfxParamTypeString";
/// kOfxParamTypeStrChoice。
pub const TYPE_STRCHOICE: &str = "OfxParamTypeStrChoice";
/// kOfxParamTypeRGB。
pub const TYPE_RGB: &str = "OfxParamTypeRGB";
/// kOfxParamTypeRGBA。
pub const TYPE_RGBA: &str = "OfxParamTypeRGBA";
/// kOfxParamTypeBytes。
pub const TYPE_BYTES: &str = "OfxParamTypeBytes";
/// kOfxParamTypeCustom。
pub const TYPE_CUSTOM: &str = "OfxParamTypeCustom";
/// kOfxParamTypePushButton。
pub const TYPE_PUSHBUTTON: &str = "OfxParamTypePushButton";
/// kOfxParamTypeGroup。
pub const TYPE_GROUP: &str = "OfxParamTypeGroup";
/// kOfxParamTypePage。
pub const TYPE_PAGE: &str = "OfxParamTypePage";
/// kOfxParamTypeParametric（值 = 曲线列表，经
/// OfxParametricParameterSuite 读写，见
/// [`crate::suites::parametric`] 与 [`crate::param_curve`]）。
pub const TYPE_PARAMETRIC: &str = "OfxParamTypeParametric";

// ---- 属性名（ofxParam.h / ofxCore.h）----

/// kOfxParamPropSecret。
pub(crate) const P_SECRET: &str = "OfxParamPropSecret";
/// kOfxParamPropHint。
pub(crate) const P_HINT: &str = "OfxParamPropHint";
/// kOfxParamPropScriptName。
pub(crate) const P_SCRIPT_NAME: &str = "OfxParamPropScriptName";
/// kOfxParamPropParent。
pub(crate) const P_PARENT: &str = "OfxParamPropParent";
/// kOfxParamPropEnabled。
pub(crate) const P_ENABLED: &str = "OfxParamPropEnabled";
/// kOfxParamPropDataPtr。
pub(crate) const P_DATA_PTR: &str = "OfxParamPropDataPtr";
/// kOfxParamPropType。
pub(crate) const P_TYPE: &str = "OfxParamPropType";
/// kOfxParamPropIsAnimating。
pub(crate) const P_IS_ANIMATING: &str = "OfxParamPropIsAnimating";
/// kOfxParamPropIsAutoKeying。
pub(crate) const P_IS_AUTO_KEYING: &str = "OfxParamPropIsAutoKeying";
/// kOfxParamPropPersistant。
pub(crate) const P_PERSISTANT: &str = "OfxParamPropPersistant";
/// kOfxParamPropEvaluateOnChange。
pub(crate) const P_EVALUATE_ON_CHANGE: &str = "OfxParamPropEvaluateOnChange";
/// kOfxParamPropCanUndo。
pub(crate) const P_CAN_UNDO: &str = "OfxParamPropCanUndo";
/// kOfxParamPropCacheInvalidation。
pub(crate) const P_CACHE_INVALIDATION: &str = "OfxParamPropCacheInvalidation";
/// kOfxParamInvalidateValueChange（P_CACHE_INVALIDATION 的默认值）。
pub(crate) const V_INVALIDATE_VALUE_CHANGE: &str = "OfxParamInvalidateValueChange";
/// kOfxParamPropAnimates。
pub(crate) const P_ANIMATES: &str = "OfxParamPropAnimates";
/// kOfxParamPropDefault。
pub(crate) const P_DEFAULT: &str = "OfxParamPropDefault";
/// kOfxParamPropDisplayMin。
pub(crate) const P_DISPLAY_MIN: &str = "OfxParamPropDisplayMin";
/// kOfxParamPropDisplayMax。
pub(crate) const P_DISPLAY_MAX: &str = "OfxParamPropDisplayMax";
/// kOfxParamPropMin。
pub(crate) const P_MIN: &str = "OfxParamPropMin";
/// kOfxParamPropMax。
pub(crate) const P_MAX: &str = "OfxParamPropMax";
/// kOfxParamPropIncrement。
pub(crate) const P_INCREMENT: &str = "OfxParamPropIncrement";
/// kOfxParamPropDigits。
pub(crate) const P_DIGITS: &str = "OfxParamPropDigits";
/// kOfxParamPropDoubleType。
pub(crate) const P_DOUBLE_TYPE: &str = "OfxParamPropDoubleType";
/// kOfxParamDoubleTypePlain。
pub(crate) const V_DOUBLE_TYPE_PLAIN: &str = "OfxParamDoubleTypePlain";
/// kOfxParamPropDefaultCoordinateSystem。
pub(crate) const P_DEFAULT_COORD_SYS: &str = "OfxParamPropDefaultCoordinateSystem";
/// kOfxParamCoordinatesCanonical。
pub(crate) const V_COORD_CANONICAL: &str = "OfxParamCoordinatesCanonical";
/// kOfxParamCoordinatesNormalised（ofxParam.h:514）。
pub(crate) const V_COORD_NORMALISED: &str = "OfxParamCoordinatesNormalised";
/// kOfxParamPropShowTimeMarker。
pub(crate) const P_SHOW_TIME_MARKER: &str = "OfxParamPropShowTimeMarker";
/// kOfxParamPropDimensionLabel。
pub(crate) const P_DIMENSION_LABEL: &str = "OfxParamPropDimensionLabel";
/// kOfxParamPropStringMode。
pub(crate) const P_STRING_MODE: &str = "OfxParamPropStringMode";
/// kOfxParamStringIsSingleLine。
pub(crate) const V_STRING_SINGLE_LINE: &str = "OfxParamStringIsSingleLine";
/// kOfxParamPropStringFilePathExists。
pub(crate) const P_STRING_FILE_EXISTS: &str = "OfxParamPropStringFilePathExists";
/// kOfxParamPropChoiceOption。
pub(crate) const P_CHOICE_OPTION: &str = "OfxParamPropChoiceOption";
/// kOfxParamPropChoiceOrder。
pub(crate) const P_CHOICE_ORDER: &str = "OfxParamPropChoiceOrder";
/// kOfxParamPropChoiceEnum。
pub(crate) const P_CHOICE_ENUM: &str = "OfxParamPropChoiceEnum";
/// kOfxParamPageSkipRow（page 子项哨兵，ofxParam.h:178）。
pub(crate) const PAGE_SKIP_ROW: &str = "OfxParamPageSkipRow";
/// kOfxParamPageSkipColumn（page 子项哨兵，ofxParam.h:186）。
pub(crate) const PAGE_SKIP_COLUMN: &str = "OfxParamPageSkipColumn";
/// kOfxParamPropCustomInterpCallbackV1。
pub(crate) const P_CUSTOM_INTERP: &str = "OfxParamPropCustomCallbackV1";
/// kOfxParamPropParametricDimension（int×1，默认 1）。
pub(crate) const P_PARAMETRIC_DIMENSION: &str = "OfxParamPropParametricDimension";
/// kOfxParamPropParametricRange（double×2，默认 (0,1)）。
pub(crate) const P_PARAMETRIC_RANGE: &str = "OfxParamPropParametricRange";
/// kOfxParamPropParametricUIColour（double×3N，默认未设）。
pub(crate) const P_PARAMETRIC_UI_COLOUR: &str = "OfxParamPropParametricUIColour";
/// kOfxParamPropParametricInteractBackground（pointer×1，默认 NULL）。
pub(crate) const P_PARAMETRIC_INTERACT_BG: &str = "OfxParamPropParametricInteractBackground";
/// kOfxParamPropPageChild。
pub(crate) const P_PAGE_CHILD: &str = "OfxParamPropPageChild";
/// kOfxParamPropGroupOpen。
pub(crate) const P_GROUP_OPEN: &str = "OfxParamPropGroupOpen";
/// kOfxParamPropInteractV1。
pub(crate) const P_INTERACT_V1: &str = "OfxParamPropInteractV1";
/// kOfxParamPropInteractSize。
pub(crate) const P_INTERACT_SIZE: &str = "OfxParamPropInteractSize";
/// kOfxParamPropInteractSizeAspect。
pub(crate) const P_INTERACT_ASPECT: &str = "OfxParamPropInteractSizeAspect";
/// kOfxParamPropInteractMinimumSize。
pub(crate) const P_INTERACT_MIN_SIZE: &str = "OfxParamPropInteractMinimumSize";
/// kOfxParamPropInteractPreferedSize。
pub(crate) const P_INTERACT_PREF_SIZE: &str = "OfxParamPropInteractPreferedSize";
/// kOfxPropType。
pub(crate) const PROP_TYPE: &str = "OfxPropType";
/// kOfxPropName。
pub(crate) const PROP_NAME: &str = "OfxPropName";
/// kOfxPropLabel。
pub(crate) const PROP_LABEL: &str = "OfxPropLabel";
/// kOfxPropShortLabel。
pub(crate) const PROP_SHORT_LABEL: &str = "OfxPropShortLabel";
/// kOfxPropLongLabel。
pub(crate) const PROP_LONG_LABEL: &str = "OfxPropLongLabel";
/// kOfxPropIcon。
pub(crate) const PROP_ICON: &str = "OfxPropIcon";
/// kOfxTypeParameter。
pub(crate) const TYPE_PARAMETER: &str = "OfxTypeParameter";

fn cs(s: &str) -> CString {
	// 静态 ASCII 常量，无内嵌 NUL。
	CString::new(s).unwrap()
}

/// 参数值类别（param shim 的变长参数分发；与
/// cbits/ofx_param_shim.c 的 KIND_* 枚举逐字对应）。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
	/// Integer。
	Int = 1,
	/// Integer2D。
	Int2 = 2,
	/// Integer3D。
	Int3 = 3,
	/// Double。
	Double = 4,
	/// Double2D。
	Double2 = 5,
	/// Double3D。
	Double3 = 6,
	/// Boolean。
	Bool = 7,
	/// Choice。
	Choice = 8,
	/// RGB。
	Rgb = 9,
	/// RGBA。
	Rgba = 10,
	/// String。
	Str = 11,
	/// StrChoice。
	StrChoice = 12,
}

/// OFX 类型字符串 → 值类别 + 维度（HS: ofxhParam.cpp `findType`）。
/// 无值类（PushButton/Group/Page）与 parametric（曲线值走
/// OfxParametricParameterSuite，无标量变长入口）返回 None。
pub(crate) fn kind_of_type(ofx_type: &str) -> Option<(ParamKind, usize)> {
	match ofx_type {
		TYPE_INTEGER => Some((ParamKind::Int, 1)),
		TYPE_INTEGER2D => Some((ParamKind::Int2, 2)),
		TYPE_INTEGER3D => Some((ParamKind::Int3, 3)),
		TYPE_DOUBLE => Some((ParamKind::Double, 1)),
		TYPE_DOUBLE2D => Some((ParamKind::Double2, 2)),
		TYPE_DOUBLE3D => Some((ParamKind::Double3, 3)),
		TYPE_BOOLEAN => Some((ParamKind::Bool, 1)),
		TYPE_CHOICE => Some((ParamKind::Choice, 1)),
		TYPE_STRING => Some((ParamKind::Str, 1)),
		TYPE_STRCHOICE => Some((ParamKind::StrChoice, 1)),
		TYPE_RGB => Some((ParamKind::Rgb, 3)),
		TYPE_RGBA => Some((ParamKind::Rgba, 4)),
		_ => None,
	}
}

/// 数值类参数（Min/Max/DisplayMin/Max 表格适用；HS
/// `addNumericParamProps` 的 isDoubleParam || isIntParam ||
/// isColourParam）。
fn is_numeric_type(ofx_type: &str) -> bool {
	matches!(
		ofx_type,
		TYPE_INTEGER
			| TYPE_INTEGER2D
			| TYPE_INTEGER3D
			| TYPE_DOUBLE
			| TYPE_DOUBLE2D
			| TYPE_DOUBLE3D
			| TYPE_RGB
			| TYPE_RGBA
	)
}

/// 该类型的默认 ParamValue（describe 期基线；HS 的属性默认在 props
/// 的 kOfxParamPropDefault，值语义等价）。
fn type_default(ofx_type: &str) -> ParamValue {
	match kind_of_type(ofx_type) {
		Some((ParamKind::Int, d)) | Some((ParamKind::Int2, d)) | Some((ParamKind::Int3, d)) => {
			ParamValue::Int([0; 3], d)
		}
		Some((ParamKind::Double, d))
		| Some((ParamKind::Double2, d))
		| Some((ParamKind::Double3, d)) => ParamValue::Double([0.0; 3], d),
		Some((ParamKind::Rgb, d)) | Some((ParamKind::Rgba, d)) => ParamValue::Color([0.0; 4], d),
		Some((ParamKind::Bool, _)) => ParamValue::Bool(false),
		Some((ParamKind::Choice, _)) => ParamValue::Choice(0),
		Some((ParamKind::Str, _)) => ParamValue::String(cs("")),
		Some((ParamKind::StrChoice, _)) => ParamValue::StrChoice(cs("")),
		None => match ofx_type {
			TYPE_BYTES | TYPE_CUSTOM => ParamValue::Bytes(Vec::new()),
			TYPE_PUSHBUTTON => ParamValue::PushButton,
			TYPE_GROUP | TYPE_PAGE => ParamValue::Container,
			// parametric 默认 = 1 条恒等曲线（range 默认 (0,1)）。
			TYPE_PARAMETRIC => ParamValue::Parametric(vec![crate::param_curve::Curve::identity(
				0.0, 1.0,
			)]),
			_ => ParamValue::Container, // 未知类型占位（paramDefine 已拒绝）
		},
	}
}

/// 数值属性表的值（Min/Max/Display 族；HS ofxhParam.cpp:420-449）。
/// `is_max` 选 ±f64::MAX / ±i32::MAX；colour 参数恒 0/1。
fn numeric_value(ofx_type: &str, kind: ParamKind, dim: usize, is_max: bool) -> Vec<Value> {
	if ofx_type == TYPE_RGB || ofx_type == TYPE_RGBA {
		// colour：display 范围 0..1，min/max 同。
		let v = Value::Double(if is_max { 1.0 } else { 0.0 });
		return vec![v; dim];
	}
	match kind {
		ParamKind::Double | ParamKind::Double2 | ParamKind::Double3 => {
			let v = Value::Double(if is_max { f64::MAX } else { f64::MIN });
			vec![v; dim]
		}
		_ => {
			let v = Value::Int(if is_max { i32::MAX } else { i32::MIN });
			vec![v; dim]
		}
	}
}

/// 参数值（与 OFX 参数类型一一对应）。
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
	/// kOfxParamTypeInteger / Integer2D / Integer3D。
	Int([i32; 3], usize),
	/// kOfxParamTypeDouble / Double2D / Double3D。
	Double([f64; 3], usize),
	/// kOfxParamTypeBoolean。
	Bool(bool),
	/// kOfxParamTypeChoice（选项索引）。
	Choice(i32),
	/// kOfxParamTypeString。
	String(std::ffi::CString),
	/// kOfxParamTypeRGB / RGBA。
	Color([f64; 4], usize),
	/// kOfxParamTypeStrChoice。
	StrChoice(std::ffi::CString),
	/// kOfxParamTypeBytes / Custom（不透明字节串）。
	Bytes(Vec<u8>),
	/// kOfxParamTypePushButton（无值，仅触发）。
	PushButton,
	/// kOfxParamTypeGroup / Page（容器，无值）。
	Container,
	/// kOfxParamTypeParametric：每维一条曲线（曲线模型见
	/// [`crate::param_curve`]；默认 = 1 条恒等曲线，维度由属性
	/// `OfxParamPropParametricDimension` 决定，见
	/// [`ParamDef::new`]）。
	Parametric(Vec<crate::param_curve::Curve>),
}

/// 参数定义（describe 产物，见 [`crate::descriptor::EffectDescriptor`]）。
/// `#[repr(C)]` + props 在偏移 0（句柄约定，见 [`crate::suites::tag`]）。
#[derive(Clone)]
#[repr(C)]
pub struct ParamDef {
	/// 定义属性（label/hint/parent/coordinate-system/secret/
	/// display min-max/choice 选项与排序等；describe 期预置
	/// HostSupport 同款表格）。
	pub props: PropertySet,
	/// 参数名（OFX 标识）。
	pub name: String,
	/// OFX 类型字符串（kOfxParamType*）。
	pub ofx_type: String,
	/// 默认值（describe 后读取）。
	pub default: ParamValue,
}

impl ParamDef {
	/// 按 OFX 类型构建定义（镜像 HS ofxhParam.cpp:224-352：
	/// universalProps + addStandardParamProps 的分类型属性表）。
	pub(crate) fn new(name: &str, ofx_type: &str) -> Self {
		let props = PropertySet::new();
		let uname = name.to_string();
		let utype = ofx_type.to_string();

		// universalProps（HS ofxhParam.cpp:229-246）。
		props.set_one(PROP_TYPE, Value::String(cs(TYPE_PARAMETER)));
		props.set_one(P_SECRET, Value::Int(0));
		props.set_one(P_HINT, Value::String(cs("")));
		props.set_one(P_SCRIPT_NAME, Value::String(cs(&uname)));
		props.set_one(P_PARENT, Value::String(cs("")));
		props.set_one(P_ENABLED, Value::Int(1));
		props.set_one(P_DATA_PTR, Value::Pointer(std::ptr::null_mut()));
		props.set_one(P_TYPE, Value::String(cs(&utype)));
		props.set_one(PROP_NAME, Value::String(cs(&uname)));
		props.set_one(PROP_LABEL, Value::String(cs(&uname)));
		props.set_one(PROP_SHORT_LABEL, Value::String(cs(&uname)));
		props.set_one(PROP_LONG_LABEL, Value::String(cs(&uname)));
		props.define(
			PROP_ICON,
			vec![Value::String(cs("")), Value::String(cs(""))],
		);

		// 值类参数（HS addValueParamProps，ofxhParam.cpp:352-380）。
		if let Some((kind, dim)) = kind_of_type(ofx_type) {
			props.set_one(P_IS_ANIMATING, Value::Int(0));
			props.set_one(P_IS_AUTO_KEYING, Value::Int(0));
			props.set_one(P_PERSISTANT, Value::Int(1));
			props.set_one(P_EVALUATE_ON_CHANGE, Value::Int(1));
			props.set_one(P_CAN_UNDO, Value::Int(1));
			props.set_one(
				P_CACHE_INVALIDATION,
				Value::String(cs(V_INVALIDATE_VALUE_CHANGE)),
			);
			// 可动画性（HS ofxhParam.cpp:367-372：custom/string/
			// boolean/choice 默认不可动画）。
			let animates = ofx_type != TYPE_CUSTOM
				&& ofx_type != TYPE_STRING
				&& ofx_type != TYPE_BOOLEAN
				&& ofx_type != TYPE_CHOICE;
			props.set_one(P_ANIMATES, Value::Int(animates as i32));
			props.define(P_DEFAULT, default_values(kind, dim));

			// 数值类（HS addNumericParamProps，ofxhParam.cpp:391-455）。
			if is_numeric_type(ofx_type) {
				props.define(P_DISPLAY_MIN, numeric_value(ofx_type, kind, dim, false));
				props.define(P_DISPLAY_MAX, numeric_value(ofx_type, kind, dim, true));
				props.define(P_MIN, numeric_value(ofx_type, kind, dim, false));
				props.define(P_MAX, numeric_value(ofx_type, kind, dim, true));
				if matches!(
					kind,
					ParamKind::Double | ParamKind::Double2 | ParamKind::Double3
				) {
					props.set_one(P_INCREMENT, Value::Double(1.0));
					props.set_one(P_DIGITS, Value::Int(2));
				}
				if ofx_type == TYPE_DOUBLE || ofx_type == TYPE_DOUBLE2D || ofx_type == TYPE_DOUBLE3D
				{
					props.set_one(P_DOUBLE_TYPE, Value::String(cs(V_DOUBLE_TYPE_PLAIN)));
					props.set_one(P_DEFAULT_COORD_SYS, Value::String(cs(V_COORD_CANONICAL)));
					if dim == 1 {
						props.set_one(P_SHOW_TIME_MARKER, Value::Int(0));
					}
				}
				if dim == 2 || dim == 3 {
					let labels: Vec<Value> = ["x", "y", "z"][..dim]
						.iter()
						.map(|l| Value::String(cs(l)))
						.collect();
					props.define(P_DIMENSION_LABEL, labels);
				}
			}
		}

		// 分类型附加（HS addStandardParamProps，ofxhParam.cpp:248-301）。
		match ofx_type {
			TYPE_STRING => {
				props.set_one(P_STRING_MODE, Value::String(cs(V_STRING_SINGLE_LINE)));
				props.set_one(P_STRING_FILE_EXISTS, Value::Int(1));
			}
			TYPE_CHOICE => {
				// 维度 0：选项数由插件 SetN 决定。ChoiceEnum（枚举值映射）
				// 与 ChoiceLabelOption（OFX 1.5 按值标签）同样预定义空
				// 数组——新支持库读它们时不是"未知属性"（其强读会抛成
				// PropertyUnknownToHost → MissingHostFeature）。
				props.define(P_CHOICE_OPTION, vec![]);
				props.define("OfxParamPropChoiceEnum", vec![]);
				props.define("OfxParamPropChoiceLabelOption", vec![]);
			}
			TYPE_CUSTOM => {
				props.set_one(P_CUSTOM_INTERP, Value::Pointer(std::ptr::null_mut()));
			}
			TYPE_PAGE => {
				props.define(P_PAGE_CHILD, vec![]);
			}
			TYPE_GROUP => {
				props.set_one(P_GROUP_OPEN, Value::Int(1));
			}
			TYPE_PARAMETRIC => {
				// parametric（HS addValueParamProps(eDouble, 0) 的
				// invariantProps + allParametric，ofxhParam.cpp:285-330）：
				// 值类 invariant 属性照旧；Animates=1（HS 的 animates 计算
				// 对 parametric 为真）；另加 4 个 parametric 专属属性。
				// 数值属性表（Min/Max/Display）HS 也加（变量维 double），
				// 但插件侧从不读 parametric 的数值属性（ofxsPropertyValidation
				// 只校验本块 4 个 + 通用属性），本 crate 遵循
				// is_numeric_type 的门控，不为 parametric 预置。
				props.set_one(P_IS_ANIMATING, Value::Int(0));
				props.set_one(P_IS_AUTO_KEYING, Value::Int(0));
				props.set_one(P_PERSISTANT, Value::Int(1));
				props.set_one(P_EVALUATE_ON_CHANGE, Value::Int(1));
				props.set_one(P_CAN_UNDO, Value::Int(1));
				props.set_one(
					P_CACHE_INVALIDATION,
					Value::String(cs(V_INVALIDATE_VALUE_CHANGE)),
				);
				props.set_one(P_ANIMATES, Value::Int(1));
				props.set_one(P_PARAMETRIC_DIMENSION, Value::Int(1));
				// UIColour 默认未设：不预定义（插件首写时按 OFX 语义
				// 隐式创建并允许逐位追加；预定义空数组会让 index >= 1
				// 的写入炸 BadIndex——ColorLookup/HueCorrect 的
				// describeInContext 失败根因）。
				props.set_one(P_PARAMETRIC_INTERACT_BG, Value::Pointer(std::ptr::null_mut()));
				props.define(
					P_PARAMETRIC_RANGE,
					vec![Value::Double(0.0), Value::Double(1.0)],
				);
			}
			_ => {}
		}

		// 交互属性（HS addInteractParamProps，ofxhParam.cpp:305-317；
		// group/page 除外）。
		if ofx_type != TYPE_GROUP && ofx_type != TYPE_PAGE {
			props.set_one(P_INTERACT_V1, Value::Pointer(std::ptr::null_mut()));
			props.define(
				P_INTERACT_SIZE,
				vec![Value::Double(0.0), Value::Double(0.0)],
			);
			props.set_one(P_INTERACT_ASPECT, Value::Double(1.0));
			props.define(
				P_INTERACT_MIN_SIZE,
				vec![Value::Double(10.0), Value::Double(10.0)],
			);
			props.define(P_INTERACT_PREF_SIZE, vec![Value::Int(10), Value::Int(10)]);
		}

		Self {
			props,
			name: uname,
			ofx_type: utype,
			default: type_default(ofx_type),
		}
	}

	/// 值类别（变长参数分发用；无值类返回 None）。
	pub(crate) fn kind(&self) -> Option<ParamKind> {
		kind_of_type(&self.ofx_type).map(|(k, _)| k)
	}
}

/// kOfxParamPropDefault 的属性值（HS ofxhParam.cpp:376-377）。
fn default_values(kind: ParamKind, dim: usize) -> Vec<Value> {
	match kind {
		ParamKind::Int
		| ParamKind::Int2
		| ParamKind::Int3
		| ParamKind::Bool
		| ParamKind::Choice => {
			vec![Value::Int(0); dim.max(1)]
		}
		ParamKind::Double
		| ParamKind::Double2
		| ParamKind::Double3
		| ParamKind::Rgb
		| ParamKind::Rgba => vec![Value::Double(0.0); dim.max(1)],
		ParamKind::Str | ParamKind::StrChoice => vec![Value::String(cs(""))],
	}
}

/// 参数实例（createInstance 后由 [`ParamDef`] 实例化）。
/// `#[repr(C)]` + props 在偏移 0（句柄约定）。
#[repr(C)]
pub struct ParamInstance {
	/// 实例级属性（当前值镜像等）。
	pub props: PropertySet,
	/// 对应定义。
	pub def: ParamDef,
	/// 当前值（节点桥与 OFX 侧的同步点）。
	value: std::sync::Mutex<ParamValue>,
}

impl ParamInstance {
	/// 从定义实例化（createInstance 路径调用）。实例 props 是定义
	/// props 的深拷贝（HS: SetInstance::makeParam 的 descriptor 属性
	/// 复制语义——插件在实例期读 label/min/max 等）。
	pub fn from_def(def: ParamDef) -> Self {
		let value = def.default.clone();
		let props = def.props.clone();
		Self {
			props,
			def,
			value: std::sync::Mutex::new(value),
		}
	}

	/// 读当前值。
	pub fn get(&self) -> ParamValue {
		self.value.lock().unwrap_or_else(|e| e.into_inner()).clone()
	}

	/// 写当前值（OFX 语义：paramSetValue action 语义，不打节点桥）。
	pub fn set_ofx(&self, value: ParamValue) {
		*self.value.lock().unwrap_or_else(|e| e.into_inner()) = value;
	}

	/// 字符串值的内驻指针（param suite 的 get_string 用；指向实例
	/// 存储，下次 set 前有效——克隆的 CString 指针会悬垂，不能给）。
	pub(crate) fn string_ptr(&self) -> Option<*const std::ffi::c_char> {
		let v = self.value.lock().unwrap_or_else(|e| e.into_inner());
		match &*v {
			ParamValue::String(s) | ParamValue::StrChoice(s) => Some(s.as_ptr()),
			_ => None,
		}
	}

	/// 从 oaknode 输入值写入（节点→插件方向）。类型映射表逐行对照
	/// C++ 的 `valueconvert`/`paraminstance.h`（`node_get` 的镜像）：
	/// INT→Integer、FLOAT→Double、BOOL→Boolean、COMBO→Choice、
	/// COLOR→RGB/RGBA、VEC2/VEC3→Double(2D/3D)/Integer(2D/3D)。
	/// 维度不齐按 OFX 语义截断/补零（缺失元素按 0）。字符串族
	/// （OAKNODE_VALUE_STRING）的 POD 不携带数据——此路径不改值
	/// （走 facade 的字符串 API，见 `include/plugin/instance.h`）；
	/// parametric 同理（曲线 JSON 文本经渲染期注入
	/// [`crate::node_factory`] 的 set_text_param 写回，见
	/// `crate::node_factory::execute_plugin_job`）。
	/// 类型不匹配 → 忽略（保持现值；C++ `node_get` 失败时参数不回写）。
	pub fn set_from_node(&self, node_value: &crate::node::Value) {
		if let Some(pv) = param_value_from_node(node_value, &self.def.ofx_type) {
			self.set_ofx(pv);
		}
	}
}

/// 节点值 → ParamValue（按参数类型解释；字符串族走专用路径）。
/// 镜像 `set_from_node` 的映射表；render 驱动的参数覆盖
/// （apply_param_overrides）复用同一转换。
pub(crate) fn param_value_from_node(
	v: &crate::node::Value,
	ofx_type: &str,
) -> Option<ParamValue> {
	use crate::node::node_value_type as T;
	match ofx_type {
		TYPE_DOUBLE => (v.r#type == T::FLOAT).then(|| ParamValue::Double([v.f[0], 0.0, 0.0], 1)),
		TYPE_DOUBLE2D => (v.r#type == T::VEC2).then(|| ParamValue::Double([v.f[0], v.f[1], 0.0], 2)),
		TYPE_DOUBLE3D => {
			(v.r#type == T::VEC3).then(|| ParamValue::Double([v.f[0], v.f[1], v.f[2]], 3))
		}
		TYPE_INTEGER => (v.r#type == T::INT).then(|| ParamValue::Int([v.num as i32, 0, 0], 1)),
		TYPE_INTEGER2D | TYPE_INTEGER3D => {
			let dim = if ofx_type == TYPE_INTEGER2D { 2 } else { 3 };
			Some(ParamValue::Int(
				[v.f[0] as i32, v.f[1] as i32, v.f[2] as i32],
				dim,
			))
		}
		TYPE_BOOLEAN => (v.r#type == T::BOOL).then(|| ParamValue::Bool(v.num != 0)),
		TYPE_CHOICE => (v.r#type == T::COMBO).then(|| ParamValue::Choice(v.num as i32)),
		TYPE_RGB => {
			(v.r#type == T::COLOR).then(|| ParamValue::Color([v.f[0], v.f[1], v.f[2], 0.0], 3))
		}
		TYPE_RGBA => (v.r#type == T::COLOR)
			.then(|| ParamValue::Color([v.f[0], v.f[1], v.f[2], v.f[3]], 4)),
		_ => None, // 字符串/无值类：POD 无数据，不改值
	}
}

/// ParamValue → [`crate::node::Value`]（插件→节点方向；
/// 字符串族经 [`crate::node::set_input_string_undoable`]）。
/// 镜像 C++ `paraminstance.h` 的 `value_int`/`value_double`/
/// `value_vec`/`value_color` 构造：RGB 颜色补 alpha=1（C++
/// `RGBInstance::set` 的 `value_color(r,g,b,1.0)`）。无值类与 Bytes
/// 无节点对应 → None。
pub(crate) fn to_node_value(v: &ParamValue) -> Option<crate::node::Value> {
	use crate::node::node_value_type as T;
	let mut out = crate::node::Value::default();
	match v {
		ParamValue::Double(d, 1) => {
			out.r#type = T::FLOAT;
			out.f = [d[0], 0.0, 0.0, 0.0];
		}
		ParamValue::Double(d, 2) => {
			out.r#type = T::VEC2;
			out.f = [d[0], d[1], 0.0, 0.0];
		}
		ParamValue::Double(d, 3) => {
			out.r#type = T::VEC3;
			out.f = [d[0], d[1], d[2], 0.0];
		}
		ParamValue::Double(d, _) => {
			out.r#type = T::FLOAT;
			out.f = [d[0], 0.0, 0.0, 0.0];
		}
		ParamValue::Int(a, 1) => {
			out.r#type = T::INT;
			out.num = a[0] as i64;
		}
		ParamValue::Int(a, 2) => {
			out.r#type = T::VEC2;
			out.f = [a[0] as f64, a[1] as f64, 0.0, 0.0];
		}
		ParamValue::Int(a, 3) => {
			out.r#type = T::VEC3;
			out.f = [a[0] as f64, a[1] as f64, a[2] as f64, 0.0];
		}
		ParamValue::Bool(b) => {
			out.r#type = T::BOOL;
			out.num = *b as i64;
		}
		ParamValue::Choice(c) => {
			out.r#type = T::COMBO;
			out.num = *c as i64;
		}
		ParamValue::Color(c, 3) => {
			out.r#type = T::COLOR;
			out.f = [c[0], c[1], c[2], 1.0];
		}
		ParamValue::Color(c, 4) => {
			out.r#type = T::COLOR;
			out.f = *c;
		}
		_ => return None,
	}
	Some(out)
}

/// 参数实例集（实例级）。
pub struct ParamSetInstance {
	/// 全部参数（定义顺序稳定；Box 保证句柄地址稳定）。
	pub params: Vec<Box<ParamInstance>>,
}

impl ParamSetInstance {
	/// 按名查找。
	pub fn find(&self, name: &str) -> Option<&ParamInstance> {
		self.params
			.iter()
			.find(|p| p.def.name == name)
			.map(|b| b.as_ref())
	}

	/// 快照（内省 C ABI 与快照测试用）。
	///
	/// 声明原为 `Vec<(&str, &ParamValue)>`：值在 Mutex 内，无法返回
	/// 活引用——改为拥有型 `(String, ParamValue)`（内省语义不变）。
	pub fn snapshot(&self) -> Vec<(String, ParamValue)> {
		self.params
			.iter()
			.map(|p| (p.def.name.clone(), p.get()))
			.collect()
	}
}

/// 插件 → 节点方向的回写入口（instanceChanged action 触发）。
/// 经 [`crate::node`] 定位绑定节点（身份注册表），再经
/// `oak_undo::undocommand::UndoCommand` 包成 undoable 修改。未绑定
/// 节点或身份查无时 no-op（单库化后身份注册表重建为
/// [`crate::node::register_node`]/[`crate::node::node_from_identity`]；
/// facade 装配期登记，project 释放后弱条目自然失效）。
///
/// 语义对照 C++ `paraminstance.h` 的 `set()` 路径（`detail::node_set`/
/// `node_set_at`）：
/// - 只回写 [`ChangeReason::PluginEdited`]（插件自改）。UserEdited/
///   TimeChanged 是宿主侧变更，值已由 [`ParamInstance::set_from_node`]
///   同步，不重复写回；
/// - 字符串族经 [`crate::node::set_input_string_undoable`]
///   （POD 不携带字符串数据）；parametric 曲线同样经该字符串 setter
///   以 JSON 文本写回输入（[`crate::param_curve::curves_to_json`]，
///   与标量回写同一 undo 语义——插件在实例上改曲线 → 节点输入更新，
///   工程序列化随标准值保留）；
/// - 无值类（PushButton/Group/Page）与 Bytes 无节点对应 → no-op；
/// - 编辑事务内（[`crate::instance::Instance::in_edit`]）并入 multi
///   命令，否则单命令立即 redo 生效（C++ `submit_undo_command`）。
pub(crate) fn notify_instance_changed(
	instance: &crate::instance::Instance,
	param_name: &str,
	reason: ChangeReason,
) {
	use crate::node;
	use std::sync::atomic::Ordering;

	if !matches!(reason, ChangeReason::PluginEdited) {
		return;
	}
	// 未绑定节点 → no-op（C++：!node_.ctx 时 set 只写本地值）。
	let node_id = instance.node_identity.load(Ordering::Relaxed);
	if node_id == 0 {
		return;
	}
	// 身份查无（未登记 / project 已释放）→ no-op。
	let Some(node_ref) = node::node_from_identity(node_id) else {
		return;
	};
	let Some(param) = instance.params.find(param_name) else {
		return;
	};
	let value = param.get();
	let label = format!("Change {param_name}");
	match to_node_value(&value) {
		Some(nv) => {
			if let Ok(cmd) = node::set_input_undoable(&node_ref, param_name, &nv) {
				instance.submit_undo_command(cmd, &label);
			}
		}
		None => match &value {
			ParamValue::String(s) | ParamValue::StrChoice(s) => {
				let Ok(s) = s.to_str() else {
					return;
				};
				if let Ok(cmd) = node::set_input_string_undoable(&node_ref, param_name, s) {
					instance.submit_undo_command(cmd, &label);
				}
			}
			// parametric：曲线序列化为 JSON 文本写回输入（节点输入是
			// Parametric 声明类型的 Text 值；经字符串 setter 存储）。
			ParamValue::Parametric(curves) => {
				let json = crate::param_curve::curves_to_json(curves);
				if let Ok(cmd) = node::set_input_string_undoable(&node_ref, param_name, &json) {
					instance.submit_undo_command(cmd, &label);
				}
			}
			_ => {} // 无值类 / Bytes：无节点对应
		},
	}
}

/// instanceChanged 的原因（OFX kOfxChange*）。
#[derive(Clone, Copy, Debug)]
pub enum ChangeReason {
	/// 用户编辑。
	UserEdited,
	/// 插件自改。
	PluginEdited,
	/// 时间变化。
	TimeChanged,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::node::node_value_type as T;

	fn param(ofx_type: &str) -> ParamInstance {
		ParamInstance::from_def(ParamDef::new("p", ofx_type))
	}

	/// ParamValue → oaknode Value 的类型映射表（镜像 C++
	/// paraminstance.h 的 value_* 构造）。
	#[test]
	fn to_node_value_mapping() {
		use crate::node::Value;
		let c = |t: i32| Value {
			r#type: t,
			num: 0,
			den: 0,
			f: [0.0; 4],
		};
		// Double 族。
		let v = to_node_value(&ParamValue::Double([1.5, 0.0, 0.0], 1)).unwrap();
		assert_eq!(v, Value::float(1.5));
		let v = to_node_value(&ParamValue::Double([1.0, 2.0, 0.0], 2)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0]));
		let v = to_node_value(&ParamValue::Double([1.0, 2.0, 3.0], 3)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0, 3.0]));
		// Int 族（2D/3D 升格为 VEC2/VEC3 浮点载荷）。
		let v = to_node_value(&ParamValue::Int([7, 0, 0], 1)).unwrap();
		assert_eq!(v, Value::int(7));
		let v = to_node_value(&ParamValue::Int([1, 2, 0], 2)).unwrap();
		assert_eq!(v, Value::vec(&[1.0, 2.0]));
		// Bool / Choice。
		assert_eq!(
			to_node_value(&ParamValue::Bool(true)).unwrap(),
			Value::bool_(true)
		);
		assert_eq!(
			to_node_value(&ParamValue::Choice(3)).unwrap(),
			Value::combo(3)
		);
		// Color：RGB 补 alpha=1（C++ oracle），RGBA 全透传。
		let v = to_node_value(&ParamValue::Color([0.1, 0.2, 0.3, 0.0], 3)).unwrap();
		assert_eq!(v, Value::color(0.1, 0.2, 0.3, 1.0));
		let v = to_node_value(&ParamValue::Color([0.1, 0.2, 0.3, 0.9], 4)).unwrap();
		assert_eq!(v, Value::color(0.1, 0.2, 0.3, 0.9));
		// 无值类 / Bytes → None（无节点对应）。
		assert_eq!(to_node_value(&ParamValue::PushButton), None);
		assert_eq!(to_node_value(&ParamValue::Container), None);
		assert_eq!(to_node_value(&ParamValue::Bytes(vec![1])), None);
		// 字符串族走专用桥（POD 路径 None）。
		assert_eq!(to_node_value(&ParamValue::String(cs(""))), None);
		let _ = c(T::NONE); // 哨兵：类型常量可达
	}

	/// type_default 的无值类/字节族基线（paramDefine 拒绝前的兜底）。
	#[test]
	fn type_default_edge_cases() {
		assert!(matches!(type_default(TYPE_BYTES), ParamValue::Bytes(_)));
		assert!(matches!(type_default(TYPE_CUSTOM), ParamValue::Bytes(_)));
		assert!(matches!(
			type_default(TYPE_PUSHBUTTON),
			ParamValue::PushButton
		));
		assert!(matches!(type_default(TYPE_GROUP), ParamValue::Container));
		assert!(matches!(type_default(TYPE_PAGE), ParamValue::Container));
		// 未知类型占位（paramDefine 已拒绝，此处兜底）。
		assert!(matches!(
			type_default("OfxParamTypeBogus"),
			ParamValue::Container
		));
		// 数值族的默认 0。
		assert!(matches!(
			type_default(TYPE_INTEGER),
			ParamValue::Int([0, 0, 0], 1)
		));
		assert!(matches!(
			type_default(TYPE_DOUBLE2D),
			ParamValue::Double([0.0, 0.0, 0.0], 2)
		));
		assert!(matches!(
			type_default(TYPE_RGBA),
			ParamValue::Color([0.0, 0.0, 0.0, 0.0], 4)
		));
	}

	/// parametric 的默认：1 条恒等曲线；4 个专属属性就位
	/// （dimension=1、range=(0,1)、UIColour 未设、interact 背景 NULL）。
	#[test]
	fn parametric_type_default() {
		assert!(matches!(
			type_default(TYPE_PARAMETRIC),
			ParamValue::Parametric(ref c) if c.len() == 1 && c[0].len() == 2
		));
		let d = ParamDef::new("curve", TYPE_PARAMETRIC);
		assert!(matches!(d.default, ParamValue::Parametric(ref c) if c.len() == 1));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_DIMENSION, 0),
			Some(Value::Int(1))
		));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_RANGE, 0),
			Some(Value::Double(0.0))
		));
		assert!(matches!(
			d.props.get(P_PARAMETRIC_RANGE, 1),
			Some(Value::Double(1.0))
		));
		// UIColour 默认未设：属性不存在（插件首写隐式创建），而不是
		// 预定义的空数组（空数组会把 index >= 1 的写入挡成 BadIndex）。
		assert!(d.props.get(P_PARAMETRIC_UI_COLOUR, 0).is_none());
		assert!(matches!(
			d.props.get(P_PARAMETRIC_INTERACT_BG, 0),
			Some(Value::Pointer(p)) if p.is_null()
		));
		// 默认曲线求值为恒等。
		if let ParamValue::Parametric(curves) = &d.default {
			assert_eq!(curves[0].evaluate(0.5), 0.5);
		}
	}

	/// set_from_node 的维度截断/补零与字符串静默路径。
	#[test]
	fn set_from_node_dimension_rules() {
		// Integer2D 缺第三维补零；浮点截断。
		let p = param(TYPE_INTEGER2D);
		p.set_from_node(&crate::node::Value::vec(&[1.9, 2.9]));
		assert_eq!(p.get(), ParamValue::Int([1, 2, 0], 2));
		// StrChoice 参数遇 STRING 类型：POD 无数据 → 不改值。
		let p = param(TYPE_STRCHOICE);
		p.set_from_node(&crate::node::Value::string());
		assert!(matches!(p.get(), ParamValue::StrChoice(_)));
		// Bytes/Custom 参数：任意节点值都不改（无映射）。
		let p = param(TYPE_CUSTOM);
		p.set_from_node(&crate::node::Value::float(3.0));
		assert!(matches!(p.get(), ParamValue::Bytes(_)));
	}
}
