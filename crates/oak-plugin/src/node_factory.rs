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

//! OFX 插件 → 节点工厂接线（阶段 6a）。
//!
//! 对应 C++ `NodeFactory::register_plugin_nodes`
//! （factory.cpp:148-186）+ `PluginNode::PluginNode(Instance*)` 的
//! 参数翻译构造函数（engine/node/plugins/plugin.cpp:274-514）。
//! OFX 类型只存在于本 crate，故翻译放这里；oaknode 侧只承载行为
//! （[`oak_node::nodes::plugin::PluginNode`]）——这是依赖方向
//! （oakplugin → oakrender → oaknode）强加的拆分。
//!
//! 职责：
//! - **实例注册表**：节点持 [`oak_node::nodes::plugin::PluginInstanceHandle`]
//!   （u64 键），这里持 `Arc<RefBox<Instance>>`（C++ 的工厂实例在库
//!   条目内存活；Rust 侧等价于注册表进程级存活）。
//! - **参数翻译**：15 类 OFX 参数 → oaknode 输入（类型表逐字对齐
//!   plugin.cpp:340-378），默认值缓存、颜色语义启发式、combo 排序、
//!   secret→hidden、ui_group/ui_page、clip→纹理输入、effect_input
//!   选择（plugin.cpp:494-514）。
//! - **渲染执行器**：[`install_render_executor`] 把 render_driver 装
//!   进 oakrender 的 executor 槽（依赖反转；oakrender 看不见本 crate）
//!   与 oaknode 的 duplicator 槽。
//!
//! app 注入点另见 [`crate::progress::set_reporter_factory`]（进度
//! UI）与 [`crate::suites::timeline::set_active_viewer_provider`]
//! （timeline suite 回退时间源）。

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use oak_node::factory::{DynNodeConstructor, DynamicNodeMeta};
use oak_node::input::{flags as input_flags, Input};
use oak_node::node::{Category, NodeBehavior, NodeCore};
use oak_node::nodes::plugin::{
    PluginInstanceHandle, PluginNode, SOURCE_CLIP, TEXTURE_INPUT,
};
use oak_node::value::{NodeValue, ValueType};

use crate::handle::RefBox;
use crate::host::Host;
use crate::instance::Instance;
use crate::param::{self as ofx, ParamDef, ParamValue};
use crate::property::{PropertySet, Value as PropValue};

/// kOfxPropPluginDescription（描述符根属性）。
const PROP_PLUGIN_DESCRIPTION: &str = "OfxPropPluginDescription";

// ---------------------------------------------------------------------------
// 实例注册表
// ---------------------------------------------------------------------------

static INSTANCES: OnceLock<Mutex<HashMap<u64, Arc<RefBox<Instance>>>>> = OnceLock::new();
static NEXT_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

fn instances() -> &'static Mutex<HashMap<u64, Arc<RefBox<Instance>>>> {
	INSTANCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记一个实例，返回非 0 句柄键（C++ 的 `Instance*` 指针身份）。
/// 实例进程级存活（对齐 C++ 工厂持有；Drop 才发 destroyInstance）。
pub fn register_instance(inst: Arc<RefBox<Instance>>) -> u64 {
	let id = NEXT_INSTANCE_ID.fetch_add(1, Ordering::Relaxed);
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(id, inst);
	id
}

/// 句柄键 → 实例（查无返回 None）。
pub fn instance_from_id(id: u64) -> Option<Arc<RefBox<Instance>>> {
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(&id)
		.cloned()
}

/// 摘除登记（节点销毁路径；未登记的键 no-op）。
pub fn unregister_instance(id: u64) {
	instances()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&id);
}

/// 当前登记数（测试/诊断）。
pub fn registered_instance_count() -> usize {
	instances().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Triggers a push-button parameter (the Rust counterpart of the C++
/// `oakengine_plugin_node_push_button_clicked`; called by the
/// inspector's button widget).
///
/// OFX push buttons carry no value: the host's press signal is a single
/// set on the parameter, then the press is routed to the plugin as
/// `kOfxActionInstanceChanged` (UserEdited) so the plugin reacts in its
/// own instanceChanged action (the OFX contract, ofxCore.h:405-435;
/// real plugins such as CImg depend on that action). Locates the
/// instance and parameter, type-checks, marks the value, then dispatches
/// the action. Returns false when the instance/parameter is unknown or
/// the parameter is not a push button; an instanceChanged failure is
/// logged and still reported as a successful press (the value was set).
pub fn push_button_clicked(instance: u64, param_name: &str) -> bool {
	let Some(inst) = instance_from_id(instance) else {
		return false;
	};
	let Some(p) = inst.value.params.find(param_name) else {
		return false;
	};
	if p.def.ofx_type != ofx::TYPE_PUSHBUTTON {
		return false;
	}
	p.set_ofx(ParamValue::PushButton);
	// The current render context supplies the time / render scale when the
	// press happens during a render; outside one, time 0 and scale 1:1.
	let (time, scale) = crate::suites::render_ctx()
		.map(|ctx| (ctx.time, ctx.scale))
		.unwrap_or((0.0, crate::instance::RenderScale { x: 1.0, y: 1.0 }));
	if let Err(e) = inst.value.instance_changed(
		param_name,
		crate::param::ChangeReason::UserEdited,
		time,
		scale,
	) {
		eprintln!("push_button_clicked: instanceChanged failed for \"{param_name}\": {e}");
	}
	true
}

// ---------------------------------------------------------------------------
// 项目幅面（normalised 坐标默认值 → canonical 的换算基准）
// ---------------------------------------------------------------------------
//
// C++ get_project_extent 读 Current::current_video_params
// （plugin.cpp:45-50）；crate 侧无项目上下文，app 经
// set_project_extent 注入，未注入用 HD 缺省。

static PROJECT_EXTENT: OnceLock<Mutex<(f64, f64)>> = OnceLock::new();

/// 注入项目幅面（宽、高；normalised 坐标默认值换算用）。
pub fn set_project_extent(width: f64, height: f64) {
	*project_extent_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner()) = (width, height);
}

fn project_extent_slot() -> &'static Mutex<(f64, f64)> {
	PROJECT_EXTENT.get_or_init(|| Mutex::new((1920.0, 1080.0)))
}

fn project_extent() -> (f64, f64) {
	*project_extent_slot().lock().unwrap_or_else(|e| e.into_inner())
}

/// C++ to_canonical（plugin.cpp:52-55）。
fn to_canonical(normalised: f64, extent: f64) -> f64 {
	if extent > 0.0 {
		normalised * extent
	} else {
		normalised
	}
}

// ---------------------------------------------------------------------------
// 属性读取助手
// ---------------------------------------------------------------------------

fn prop_str(props: &PropertySet, name: &str, index: usize) -> String {
	match props.get(name, index) {
		Some(PropValue::String(s)) => s.to_string_lossy().into_owned(),
		_ => String::new(),
	}
}

fn prop_double(props: &PropertySet, name: &str, index: usize) -> f64 {
	match props.get(name, index) {
		Some(PropValue::Double(v)) => v,
		Some(PropValue::Int(v)) => v as f64,
		_ => 0.0,
	}
}

fn prop_int(props: &PropertySet, name: &str, index: usize) -> i32 {
	match props.get(name, index) {
		Some(PropValue::Int(v)) => v,
		Some(PropValue::Double(v)) => v as i32,
		_ => 0,
	}
}

fn is_normalised_coord_system(def: &ParamDef) -> bool {
	prop_str(&def.props, ofx::P_DEFAULT_COORD_SYS, 0) == ofx::V_COORD_NORMALISED
}

// ---------------------------------------------------------------------------
// 默认值（plugin.cpp default_value_for_param，:57-131）
// ---------------------------------------------------------------------------

/// 单个参数的节点默认值（无值类返回 None；对齐 C++ 返回 invalid
/// QVariant 的分支）。
fn default_value_for_param(def: &ParamDef) -> Option<NodeValue> {
	let props = &def.props;
	match def.ofx_type.as_str() {
		ofx::TYPE_INTEGER => Some(NodeValue::Int(prop_int(props, ofx::P_DEFAULT, 0) as i64)),
		ofx::TYPE_CHOICE => Some(NodeValue::Combo(prop_int(props, ofx::P_DEFAULT, 0) as i64)),
		ofx::TYPE_BOOLEAN => Some(NodeValue::Boolean(prop_int(props, ofx::P_DEFAULT, 0) != 0)),
		ofx::TYPE_DOUBLE => {
			let mut val = prop_double(props, ofx::P_DEFAULT, 0);
			if is_normalised_coord_system(def) {
				let (x_size, _) = project_extent();
				val = to_canonical(val, x_size);
			}
			Some(NodeValue::Float(val))
		}
		ofx::TYPE_STRING | ofx::TYPE_STRCHOICE => {
			Some(NodeValue::Text(prop_str(props, ofx::P_DEFAULT, 0)))
		}
		ofx::TYPE_CUSTOM => {
			// C++ 亦按字符串读默认（plugin.cpp:83-87）；节点输入是
			// binary，按字节保留。
			Some(NodeValue::Binary(
				prop_str(props, ofx::P_DEFAULT, 0).into_bytes(),
			))
		}
		ofx::TYPE_RGB | ofx::TYPE_RGBA => {
			let count = if def.ofx_type == ofx::TYPE_RGBA { 4 } else { 3 };
			let mut values = [0.0, 0.0, 0.0, 1.0];
			for i in 0..count {
				values[i] = prop_double(props, ofx::P_DEFAULT, i);
			}
			let alpha = if count == 4 { values[3] } else { 1.0 };
			Some(NodeValue::Color([values[0], values[1], values[2], alpha]))
		}
		ofx::TYPE_DOUBLE2D | ofx::TYPE_DOUBLE3D | ofx::TYPE_INTEGER2D | ofx::TYPE_INTEGER3D => {
			let is_double = matches!(def.ofx_type.as_str(), ofx::TYPE_DOUBLE2D | ofx::TYPE_DOUBLE3D);
			let count = if matches!(def.ofx_type.as_str(), ofx::TYPE_DOUBLE2D | ofx::TYPE_INTEGER2D) {
				2
			} else {
				3
			};
			let mut values = [0.0f64; 3];
			if is_double {
				for i in 0..count {
					values[i] = prop_double(props, ofx::P_DEFAULT, i);
				}
				if is_normalised_coord_system(def) {
					let (x_size, y_size) = project_extent();
					values[0] = to_canonical(values[0], x_size);
					values[1] = to_canonical(values[1], y_size);
					if count == 3 {
						values[2] = to_canonical(values[2], x_size);
					}
				}
			} else {
				for i in 0..count {
					values[i] = prop_int(props, ofx::P_DEFAULT, i) as f64;
				}
			}
			if count == 2 {
				Some(NodeValue::Vec2([values[0], values[1]]))
			} else {
				Some(NodeValue::Vec3([values[0], values[1], values[2]]))
			}
		}
		ofx::TYPE_BYTES => Some(NodeValue::Binary(Vec::new())),
		// parametric：值 = 默认曲线的确定性 JSON（节点输入无曲线模型，
		// 经 Text 承载；撤销/工程序列化随标准值免费获得）。默认曲线
		// 来自 def.default（describe 期插件经 parametric suite 改过
		// 的默认），不是 P_DEFAULT 属性（parametric 无标量默认）。
		ofx::TYPE_PARAMETRIC => match &def.default {
			ParamValue::Parametric(curves) => {
				Some(NodeValue::Text(crate::param_curve::curves_to_json(curves)))
			}
			// 防御：type_default 恒产出 Parametric，此处只兜底。
			_ => None,
		},
		// PushButton/Group/Page/未知 → invalid（C++ 末尾
		// return QVariant()）。
		_ => None,
	}
}

/// 每插件的默认值缓存（C++ g_plugin_param_defaults，plugin.cpp:37）。
static DEFAULTS: OnceLock<Mutex<HashMap<String, HashMap<String, NodeValue>>>> = OnceLock::new();

fn defaults_slot() -> &'static Mutex<HashMap<String, HashMap<String, NodeValue>>> {
	DEFAULTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取（或首访构建）某插件的参数默认值表（build_default_values，
/// plugin.cpp:224-246）。
fn cached_defaults(
	plugin_id: &str,
	params: &crate::param::ParamSetInstance,
) -> HashMap<String, NodeValue> {
	{
		let cache = defaults_slot().lock().unwrap_or_else(|e| e.into_inner());
		if let Some(hit) = cache.get(plugin_id) {
			return hit.clone();
		}
	}
	let mut defaults = HashMap::new();
	for p in &params.params {
		let ofx_type = p.def.ofx_type.as_str();
		if ofx_type == ofx::TYPE_GROUP || ofx_type == ofx::TYPE_PAGE || ofx_type == ofx::TYPE_PUSHBUTTON {
			continue;
		}
		if p.def.name.is_empty() {
			continue;
		}
		if let Some(v) = default_value_for_param(&p.def) {
			defaults.insert(p.def.name.clone(), v);
		}
	}
	defaults_slot()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(plugin_id.to_string(), defaults.clone());
	defaults
}

// ---------------------------------------------------------------------------
// 颜色语义启发式（plugin.cpp deduce_color_semantic，:133-222）
// ---------------------------------------------------------------------------

/// RGB/RGBA 参数是取色器（"color"）还是逐通道标量组（"scalar"）。
fn deduce_color_semantic(def: &ParamDef, group_labels: &HashMap<String, String>) -> &'static str {
	const COLOR_KEYWORDS: &[&str] = &["color", "colour", "fill", "tint", "key"];
	const SCALAR_KEYWORDS: &[&str] = &[
		"gamma",
		"contrast",
		"gain",
		"offset",
		"saturation",
		"exposure",
		"brightness",
		"lift",
		"multiply",
		"scale",
		"pivot",
	];

	if def.ofx_type != ofx::TYPE_RGB && def.ofx_type != ofx::TYPE_RGBA {
		return "color";
	}

	let label = prop_str(&def.props, ofx::PROP_LABEL, 0).to_lowercase();
	let hint = prop_str(&def.props, ofx::P_HINT, 0).to_lowercase();
	let name = def.name.to_lowercase();

	// 规则 1：显式颜色关键词 → color。
	for kw in COLOR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "color";
		}
	}

	// 规则 2：显式标量/调色关键词 → scalar。
	for kw in SCALAR_KEYWORDS {
		if label.contains(kw) || hint.contains(kw) || name.contains(kw) {
			return "scalar";
		}
	}

	// 规则 3：display 范围显著越出 [0,1] → scalar。
	let dim = if def.ofx_type == ofx::TYPE_RGBA { 4 } else { 3 };
	for i in 0..dim {
		let dmin = prop_double(&def.props, ofx::P_DISPLAY_MIN, i);
		let dmax = prop_double(&def.props, ofx::P_DISPLAY_MAX, i);
		if dmin < -0.01 || dmax > 1.01 {
			return "scalar";
		}
	}

	// 规则 4：默认值全相等 → scalar（lean）。
	let mut defs = [0.0f64; 4];
	for i in 0..dim {
		defs[i] = prop_double(&def.props, ofx::P_DEFAULT, i);
	}
	let all_equal = (1..dim).all(|i| defs[i] == defs[0]);
	if all_equal {
		return "scalar";
	}

	// 规则 5：父 group 名含标量关键词 → scalar。
	let parent = prop_str(&def.props, ofx::P_PARENT, 0).to_lowercase();
	if !parent.is_empty() {
		let group_label = group_labels
			.get(&prop_str(&def.props, ofx::P_PARENT, 0))
			.map(|s| s.to_lowercase())
			.unwrap_or_default();
		for kw in SCALAR_KEYWORDS {
			if parent.contains(kw) || group_label.contains(kw) {
				return "scalar";
			}
		}
	}

	// 兜底。
	"color"
}

// ---------------------------------------------------------------------------
// clip 显示名（plugin.cpp clip_label_for_name，:248-270）
// ---------------------------------------------------------------------------

fn clip_label_for_name(name: &str, clip_props: Option<&PropertySet>) -> String {
	// 过渡上下文 clip 名（ofxImageEffect.h:1435-1441）。
	if name == SOURCE_CLIP {
		return "Source".to_string();
	}
	if name == "SourceFrom" {
		return "From".to_string();
	}
	if name == "SourceTo" {
		return "To".to_string();
	}
	if let Some(props) = clip_props {
		let label = prop_str(props, ofx::PROP_LABEL, 0);
		if !label.is_empty() {
			return label;
		}
	}
	name.to_string()
}

// ---------------------------------------------------------------------------
// 参数翻译：OFX 参数 → 节点输入（plugin.cpp:331-490）
// ---------------------------------------------------------------------------

/// OFX 类型 → 节点输入值类型（plugin.cpp:340-378 的类型表；
/// Group/Page 与未知类型（k_none）均无输入 → None，跳过）。
fn input_type_for(ofx_type: &str) -> Option<ValueType> {
	Some(match ofx_type {
		ofx::TYPE_INTEGER => ValueType::Int,
		ofx::TYPE_DOUBLE => ValueType::Float,
		ofx::TYPE_BOOLEAN => ValueType::Boolean,
		ofx::TYPE_STRING => ValueType::Text,
		ofx::TYPE_RGB | ofx::TYPE_RGBA => ValueType::Color,
		ofx::TYPE_CHOICE => ValueType::Combo,
		ofx::TYPE_DOUBLE2D | ofx::TYPE_INTEGER2D => ValueType::Vec2,
		ofx::TYPE_DOUBLE3D | ofx::TYPE_INTEGER3D => ValueType::Vec3,
		ofx::TYPE_STRCHOICE => ValueType::StrCombo,
		ofx::TYPE_BYTES | ofx::TYPE_CUSTOM => ValueType::Binary,
		ofx::TYPE_PUSHBUTTON => ValueType::PushButton,
		// parametric：值 = 默认曲线的 JSON 文本（见
		// [`crate::param_curve::curves_to_json`]）。
		ofx::TYPE_PARAMETRIC => ValueType::Parametric,
		_ => return None,
	})
}

/// 从描述符实例构建节点输入表（PluginNode 构造函数的参数/clip 循环）。
fn build_core(inst: &Instance) -> NodeCore {
	let mut core = NodeCore::new();

	let defaults = cached_defaults(&inst.plugin.identifier, &inst.params);

	// 第 1 遍：group/page 标签与 param→page 映射（plugin.cpp:300-330）。
	let mut group_labels: HashMap<String, String> = HashMap::new();
	let mut page_for_param: HashMap<String, String> = HashMap::new();
	for p in &inst.params.params {
		let def = &p.def;
		match def.ofx_type.as_str() {
			ofx::TYPE_GROUP => {
				let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
				group_labels.insert(
					def.name.clone(),
					if label.is_empty() { def.name.clone() } else { label },
				);
			}
			ofx::TYPE_PAGE => {
				let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
				let page_label = if label.is_empty() { def.name.clone() } else { label };
				let count = def.props.dimension(ofx::P_PAGE_CHILD);
				for i in 0..count {
					let child = prop_str(&def.props, ofx::P_PAGE_CHILD, i);
					if child == ofx::PAGE_SKIP_ROW || child == ofx::PAGE_SKIP_COLUMN {
						continue;
					}
					page_for_param.insert(child, page_label.clone());
				}
			}
			_ => {}
		}
	}

	// 第 2 遍：值参数 → 输入（plugin.cpp:331-490）。
	for p in &inst.params.params {
		let def = &p.def;
		let Some(value_type) = input_type_for(&def.ofx_type) else {
			continue;
		};
		let input_id = def.name.clone();
		if input_id.is_empty() {
			continue;
		}

		let is_secret = prop_int(&def.props, ofx::P_SECRET, 0) != 0;

		// 默认值（缓存表；C++ defaults.value(input_id)）。
		let mut input = match defaults.get(&input_id) {
			Some(default_value) => {
				let input = Input::new(&input_id, value_type, default_value.clone());
				if !matches!(value_type, ValueType::PushButton) {
					core.set_standard_value(&input_id, 0, default_value.clone());
				}
				input
			}
			None => Input::new(&input_id, value_type, NodeValue::None),
		};

		if is_secret {
			input.flags |= input_flags::HIDDEN;
		}
		let label = prop_str(&def.props, ofx::PROP_LABEL, 0);
		input.display_name = if label.is_empty() { input_id.clone() } else { label };

		let parent = prop_str(&def.props, ofx::P_PARENT, 0);
		if !parent.is_empty() {
			let group = group_labels.get(&parent).cloned().unwrap_or(parent.clone());
			input
				.properties
				.push(("ui_group".to_string(), NodeValue::Text(group)));
		}
		if let Some(page) = page_for_param.get(&input_id) {
			input
				.properties
				.push(("ui_page".to_string(), NodeValue::Text(page.clone())));
		}

		// parametric 专属属性（消费方 = 检查器曲线编辑器；值从
		// OfxParametricParameterSuite 的属性表搬运）：
		// - "parametric_dimension"：int，维度数；
		// - "parametric_range"：Vec2，定义域 (lo, hi)；
		// - "parametric_ui_colour"：每维一条（重复键，对齐 combo_option
		//   惯例），Color([r,g,b,1])，取自 double×3N 属性，未配置不携带。
		// 无参数动画（第 1 期）→ 不可键帧。
		if matches!(value_type, ValueType::Parametric) {
			input.flags |= input_flags::NOT_KEYFRAMABLE;
			let dim = prop_int(&def.props, ofx::P_PARAMETRIC_DIMENSION, 0).max(1);
			input.properties.push((
				"parametric_dimension".to_string(),
				NodeValue::Int(dim as i64),
			));
			input.properties.push((
				"parametric_range".to_string(),
				NodeValue::Vec2([
					prop_double(&def.props, ofx::P_PARAMETRIC_RANGE, 0),
					prop_double(&def.props, ofx::P_PARAMETRIC_RANGE, 1),
				]),
			));
			let colour_dim = def.props.dimension(ofx::P_PARAMETRIC_UI_COLOUR);
			for i in 0..dim as usize {
				if i * 3 + 2 >= colour_dim {
					break;
				}
				input.properties.push((
					"parametric_ui_colour".to_string(),
					NodeValue::Color([
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3),
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3 + 1),
						prop_double(&def.props, ofx::P_PARAMETRIC_UI_COLOUR, i * 3 + 2),
						1.0,
					]),
				));
			}
		}

		if matches!(value_type, ValueType::Color) {
			let semantic = deduce_color_semantic(def, &group_labels);
			input.properties.push((
				"color_semantic".to_string(),
				NodeValue::Text(semantic.to_string()),
			));
			// display min/max 取第 0 维（plugin.cpp:418-424）。
			input.properties.push((
				"min".to_string(),
				NodeValue::Float(prop_double(&def.props, ofx::P_DISPLAY_MIN, 0)),
			));
			input.properties.push((
				"max".to_string(),
				NodeValue::Float(prop_double(&def.props, ofx::P_DISPLAY_MAX, 0)),
			));
			let hint = prop_str(&def.props, ofx::P_HINT, 0);
			if !hint.is_empty() {
				input
					.properties
					.push(("tooltip".to_string(), NodeValue::Text(hint)));
			}
		}

		if matches!(value_type, ValueType::Combo | ValueType::StrCombo) {
			let mut option_labels = Vec::new();
			let mut option_values = Vec::new();
			let label_count = def.props.dimension(ofx::P_CHOICE_OPTION);
			let value_count = def.props.dimension(ofx::P_CHOICE_ENUM);
			for i in 0..label_count {
				option_labels.push(prop_str(&def.props, ofx::P_CHOICE_OPTION, i));
			}
			for i in 0..value_count {
				option_values.push(prop_str(&def.props, ofx::P_CHOICE_ENUM, i));
			}
			if option_labels.is_empty() && !option_values.is_empty() {
				option_labels = option_values.clone();
			}
			if option_values.is_empty() && !option_labels.is_empty() {
				option_values = option_labels.clone();
			}

			// ChoiceOrder 稳定排序（plugin.cpp:449-472）。
			let order_count = def.props.dimension(ofx::P_CHOICE_ORDER);
			if order_count == option_labels.len() && option_labels.len() == option_values.len() {
				let mut indices: Vec<usize> = (0..option_labels.len()).collect();
				indices.sort_by_key(|&i| prop_int(&def.props, ofx::P_CHOICE_ORDER, i));
				option_labels = indices.iter().map(|&i| option_labels[i].clone()).collect();
				option_values = indices.iter().map(|&i| option_values[i].clone()).collect();
			}

			// combo 选项经重复键属性携带（NodeValue 无字符串表变体；
			// 消费方按 ("combo_option", _) 全量收集，str_combo 的
			// 值表为 ("combo_value", _)——C++ set_combo_box_strings /
			// "combo_value_str" 的等价物）。
			for label in &option_labels {
				input.properties.push((
					"combo_option".to_string(),
					NodeValue::Text(label.clone()),
				));
			}
			if matches!(value_type, ValueType::StrCombo) {
				for value in &option_values {
					input.properties.push((
						"combo_value".to_string(),
						NodeValue::Text(value.clone()),
					));
				}
			}
		}

		core.add_input(input);
	}

	// clip → 纹理输入（plugin.cpp:492-501）。
	let mut has_texture_input = false;
	for clip in &inst.clips {
		if clip.name == "Output" {
			continue;
		}
		let mut input = Input::new(&clip.name, ValueType::Texture, NodeValue::None);
		input.display_name = clip_label_for_name(&clip.name, Some(&clip.props));
		core.add_input(input);
		has_texture_input = true;
	}

	// effect_input 选择（plugin.cpp:503-514）。
	if core.has_input(SOURCE_CLIP) {
		core.effect_input = SOURCE_CLIP.to_string();
	} else if core.has_input(TEXTURE_INPUT) {
		core.effect_input = TEXTURE_INPUT.to_string();
	} else if has_texture_input {
		let mut input = Input::new(TEXTURE_INPUT, ValueType::Texture, NodeValue::None);
		input.display_name = "Texture".to_string();
		core.add_input(input);
		core.effect_input = TEXTURE_INPUT.to_string();
	}

	core
}

/// 上下文的显示子分类（plugin.cpp:281-290）。
fn sub_category_for(context: &str) -> &'static str {
	match context {
		"OfxImageEffectContextFilter" => "Filter",
		"OfxImageEffectContextGenerator" => "Generator",
		"OfxImageEffectContextTransition" => "Transition",
		_ => "General",
	}
}

/// 插件描述符的显示名（plugin.cpp PluginNode::name，:517-524）。
fn plugin_display_name(inst: &Instance) -> String {
	let label = prop_str(&inst.plugin.descriptor.props, ofx::PROP_LABEL, 0);
	if label.is_empty() {
		inst.plugin.identifier.clone()
	} else {
		label
	}
}

/// 插件描述符的描述文本（plugin.cpp PluginNode::description，
/// :531-538）。
fn plugin_description(inst: &Instance) -> String {
	prop_str(&inst.plugin.descriptor.props, PROP_PLUGIN_DESCRIPTION, 0)
}

// ---------------------------------------------------------------------------
// 节点构造（每次建图都新建实例——C++ 库条目共享一个实例是 Qt 父子
// 所有权模型；Rust 侧每节点独占实例才能支持 duplicate 与并发渲染）
// ---------------------------------------------------------------------------

/// 为 (identifier, context) 建一个插件节点（新实例 + 注册表登记）。
fn create_plugin_node(
	identifier: &str,
	context: &str,
) -> Option<(NodeCore, Box<dyn NodeBehavior>)> {
	let inst = Host::global()
		.create_instance(identifier, Some(context))
		.ok()?;
	let core = build_core(&inst.value);
	let name = plugin_display_name(&inst.value);
	let description = plugin_description(&inst.value);
	let sub_category = sub_category_for(context).to_string();
	let id = register_instance(inst);
	let node = PluginNode::new(
		PluginInstanceHandle(id),
		name,
		identifier.to_string(),
		description,
		sub_category,
	);
	Some((core, Box::new(node)))
}

/// 扫描宿主插件缓存并向节点工厂注册动态条目（C++
/// `NodeFactory::register_plugin_nodes`，factory.cpp:148-186）。
/// 返回新注册的 type id 列表；已存在的 id 跳过（register_dynamic
/// 去重，对齐 C++ existing_ids 检查）。
pub fn register_plugin_nodes() -> Vec<String> {
	install_render_executor();

	let host = Host::global();
	let mut registered = Vec::new();
	for i in 0..host.cache.count() {
		let Some(plugin) = host.cache.at(i) else {
			continue;
		};

		// 元数据实例（name/description；建完即弃）。上下文选择：
		// filter 优先、实例化失败按序回退（TrackerPM 这类插件的
		// createInstance 只在 tracker/general/paint 下能跑）。所有
		// 上下文都失败的插件无法实例化（常见于只支持 Vegas 立体声
		// 等厂商套件的插件）——记录原因，不静默跳过。
		let (context, inst) = match host.create_instance_preferred(&plugin.identifier) {
			Ok(pair) => pair,
			Err(e) => {
				eprintln!("[ofx] {}: instance creation failed: {e}", plugin.identifier);
				continue;
			}
		};
		let name = plugin_display_name(&inst.value);
		let description = plugin_description(&inst.value);
		let sub_category = sub_category_for(&context).to_string();
		drop(inst);

		let identifier = plugin.identifier.clone();
		let create: DynNodeConstructor = Arc::new(move || {
			create_plugin_node(&identifier, &context)
				.unwrap_or_else(oak_node::nodes::plugin::create)
		});

		let meta = DynamicNodeMeta {
			type_id: plugin.identifier.clone(),
			name,
			categories: vec![Category::OpenFx],
			sub_category,
			description,
			create,
		};
		if oak_node::factory::Factory::global().register_dynamic(meta) {
			registered.push(plugin.identifier.clone());
		}
	}
	registered
}

// ---------------------------------------------------------------------------
// 渲染执行器 + duplicator（依赖反转的 oakplugin 侧半环）
// ---------------------------------------------------------------------------

/// 文本族参数注入（POD 无字符串表达；直接 set_ofx）。按参数的 OFX
/// 类型分发：String → String、StrChoice → StrChoice；Parametric →
/// 解析 JSON 曲线集（[`crate::param_curve::curves_from_json`]，宿主侧
/// [`crate::param_curve::curves_to_json`] 的格式）写回
/// `ParamValue::Parametric`——即"节点输入值变化 → 写回实例"的
/// parametric 路径（渲染期参数覆盖的分支，见
/// [`execute_plugin_job`]）。JSON 解析失败 / 类型不符 → 忽略（与
/// 字符串族类型不匹配静默一致）。
fn set_text_param(inst: &Instance, key: &str, text: &str) {
	let Some(p) = inst.params.find(key) else {
		return;
	};
	match p.def.ofx_type.as_str() {
		ofx::TYPE_STRING => {
			let Ok(cs) = CString::new(text) else {
				return;
			};
			p.set_ofx(ParamValue::String(cs));
		}
		ofx::TYPE_STRCHOICE => {
			let Ok(cs) = CString::new(text) else {
				return;
			};
			p.set_ofx(ParamValue::StrChoice(cs));
		}
		ofx::TYPE_PARAMETRIC => {
			if let Some(curves) = crate::param_curve::curves_from_json(text) {
				p.set_ofx(ParamValue::Parametric(curves));
			}
		}
		_ => {}
	}
}

/// executor 槽实现：JobSpec::Plugin → render_driver::render_frame。
fn execute_plugin_job(
	req: &oak_render::eval::PluginJobRequest<'_>,
) -> oak_render::error::Result<oak_core::texture::Texture> {
    use oak_render::error::Error;

    let oak_render::eval::JobSpec::Plugin {
		instance,
		time,
		effect_input_id,
		inputs,
		values,
	} = req.spec
	else {
		return Err(Error::Invalid);
	};

	let inst = instance_from_id(*instance).ok_or_else(|| {
		Error::Failed(format!("插件实例 {instance} 未登记（实例已释放？）"))
	})?;

	if req.src.is_dummy() {
		return Err(Error::Failed("plugin job 无可用输入纹理".into()));
	}

	// 参数注入：数值族走 render_driver 的 POD 覆盖；字符串/曲线族
	// POD 无表达，这里直接 set_ofx（对齐 pluginrenderer.cpp 的
	// StringInstance::set 分支 + parametric 的 JSON 写回）。
	let mut pod_values = Vec::new();
	for (key, nv) in values {
		match nv {
			NodeValue::Text(s) | NodeValue::StrCombo(s) => {
				set_text_param(&inst.value, key, s)
			}
			NodeValue::PushButton | NodeValue::None => {}
			other => {
				if let Some(v) = crate::node::Value::from_node_value(other) {
					pod_values.push((key.clone(), v));
				}
			}
		}
	}

	// 输出纹理：与输入同尺寸的 F32 帧（render_driver 校验 F32）。
	let dst_frame = oak_render::eval::generate_frame(
		oak_core::Rational::from_double(*time),
		req.src.size(),
		oak_core::PixelFormat::F32,
	)?;
	let job = crate::render_driver::RenderJob {
		time: *time,
		dst: oak_core::texture::Texture::wrap_frame(dst_frame),
		src: Some(req.src.clone()),
		effect_input_id: effect_input_id.clone(),
		inputs: inputs.clone(),
		values: pod_values,
		// GL 渲染路径：OpenGL-only 插件（CImg 套件）在 CPU 路径的
		// isIdentity 直接回 kOfxStatErrMissingHostFeature（紫帧根因）。
		// 标记上下文只服务于 use_opengl 的 kind 判定；真正的 GL 渲染走
		// gl_bridge 的离屏上下文（无头的 oak-worker 进程同样可用）。
		renderer: Some(std::sync::Arc::new(crate::render::GlKindMarker)),
		clear_destination: false,
		interactive: false,
	};
	let (out, _rois) = crate::render_driver::render_frame(&inst.value, &job)
		.map_err(|e| Error::Failed(format!("插件渲染失败：{e:?}")))?;
	Ok(out)
}

/// duplicator 槽实现：duplicate() 经注册表换新实例。
fn duplicate_instance(old: PluginInstanceHandle) -> Option<PluginInstanceHandle> {
	let inst = instance_from_id(old.0)?;
	let identifier = inst.value.plugin.identifier.clone();
	let context = inst.value.context.clone();
	let new_inst = Host::global()
		.create_instance(&identifier, Some(&context))
		.ok()?;
	Some(PluginInstanceHandle(register_instance(new_inst)))
}

static EXECUTOR_INSTALLED: OnceLock<()> = OnceLock::new();

/// 把渲染执行器装进 oakrender 的 executor 槽、duplicator 装进
/// oaknode（幂等）。[`register_plugin_nodes`] 已内含；app 侧单独
/// 初始化渲染管线时也可直接调。
pub fn install_render_executor() {
	EXECUTOR_INSTALLED.get_or_init(|| {
		oak_render::eval::set_plugin_executor(Some(Arc::new(execute_plugin_job)));
		oak_render::eval::set_plugin_instance_factory(Some(Arc::new(shared_plugin_instance)));
		oak_node::nodes::plugin::set_plugin_duplicator(Some(Arc::new(duplicate_instance)));
	});
}

// ---------------------------------------------------------------------------
// montage 渲染路径的共享实例（渲染进程按插件标识惰性创建 + 进程级缓存）
// ---------------------------------------------------------------------------

/// montage 效果栈携带插件标识而非实例 id（montage 在主进程从时间线
/// 解析，实例活在渲染进程）。渲染进程经此工厂按需创建实例并缓存：
/// 参数每次渲染前由 montage 注入（[`execute_plugin_job`] 的 values
/// 覆盖），worker 单线程顺序渲染，共享实例无跨帧状态冲突。
fn shared_plugin_instance(identifier: &str) -> Option<u64> {
	static SHARED: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
	let cache = SHARED.get_or_init(|| Mutex::new(HashMap::new()));
	if let Some(&id) = cache
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.get(identifier)
	{
		return Some(id);
	}
	let host = Host::global();
	if host.cache.find(identifier).is_none() {
		return None;
	}
	// 上下文选择：filter 优先、实例化失败按序回退（与
	// [`register_plugin_nodes`] 同一策略，见
	// [`Host::create_instance_preferred`]）。
	let (_context, inst) = host.create_instance_preferred(identifier).ok()?;
	let id = register_instance(inst);
	cache
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(identifier.to_string(), id);
	Some(id)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the mock plugin entry saw, so the push-button test can
	/// assert the kOfxActionInstanceChanged routing and its inArgs.
	static PUSH_ENTRY_CALLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

	#[test]
	fn input_type_table_covers_all_ofx_kinds() {
		assert_eq!(
			input_type_for(ofx::TYPE_INTEGER),
			Some(ValueType::Int)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_DOUBLE),
			Some(ValueType::Float)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_BOOLEAN),
			Some(ValueType::Boolean)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_STRING),
			Some(ValueType::Text)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_RGB),
			Some(ValueType::Color)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_RGBA),
			Some(ValueType::Color)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_CHOICE),
			Some(ValueType::Combo)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_DOUBLE2D),
			Some(ValueType::Vec2)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_INTEGER2D),
			Some(ValueType::Vec2)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_DOUBLE3D),
			Some(ValueType::Vec3)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_INTEGER3D),
			Some(ValueType::Vec3)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_STRCHOICE),
			Some(ValueType::StrCombo)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_BYTES),
			Some(ValueType::Binary)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_CUSTOM),
			Some(ValueType::Binary)
		);
		assert_eq!(
			input_type_for(ofx::TYPE_PUSHBUTTON),
			Some(ValueType::PushButton)
		);
		// 容器与未知类型：跳过。
		assert_eq!(input_type_for(ofx::TYPE_GROUP), None);
		assert_eq!(input_type_for(ofx::TYPE_PAGE), None);
		assert_eq!(input_type_for("OfxParamTypeBogus"), None);
		// parametric → Parametric（值 = 曲线 JSON 文本）。
		assert_eq!(
			input_type_for(ofx::TYPE_PARAMETRIC),
			Some(ValueType::Parametric)
		);
	}

	#[test]
	fn color_semantic_rules() {
		fn def_with(label: &str, ofx_type: &str) -> ParamDef {
			let def = ParamDef {
				props: PropertySet::new(),
				name: "p".into(),
				ofx_type: ofx_type.into(),
				default: ParamValue::Container,
			};
			def.props.set_one(
				ofx::PROP_LABEL,
				PropValue::String(CString::new(label).unwrap()),
			);
			def
		}
		let groups = HashMap::new();
		// 非颜色类型恒 "color"。
		assert_eq!(
			deduce_color_semantic(&def_with("Anything", ofx::TYPE_DOUBLE), &groups),
			"color"
		);
		// 规则 1：颜色关键词。
		assert_eq!(
			deduce_color_semantic(&def_with("Tint Color", ofx::TYPE_RGB), &groups),
			"color"
		);
		// 规则 2：标量关键词。
		assert_eq!(
			deduce_color_semantic(&def_with("Gamma Adjust", ofx::TYPE_RGBA), &groups),
			"scalar"
		);
	}

	#[test]
	fn color_semantic_display_range_rule() {
		let def = ParamDef {
			props: PropertySet::new(),
			name: "rgb".into(),
			ofx_type: ofx::TYPE_RGB.into(),
			default: ParamValue::Container,
		};
		// 默认 (0,0,0) 全相等前先被规则 3 拦截：display max 越界。
		def.props
			.set_one(ofx::P_DISPLAY_MAX, PropValue::Double(2.0));
		let groups = HashMap::new();
		assert_eq!(deduce_color_semantic(&def, &groups), "scalar");
	}

	#[test]
	fn clip_labels_special_case_names() {
		assert_eq!(clip_label_for_name("Source", None), "Source");
		assert_eq!(clip_label_for_name("SourceFrom", None), "From");
		assert_eq!(clip_label_for_name("SourceTo", None), "To");
		assert_eq!(clip_label_for_name("Overlay", None), "Overlay");
		let props = PropertySet::new();
		props.set_one(
			ofx::PROP_LABEL,
			PropValue::String(CString::new("Matte").unwrap()),
		);
		assert_eq!(clip_label_for_name("Overlay", Some(&props)), "Matte");
	}

	#[test]
	fn instance_registry_roundtrip() {
		// 注册表对不存在的键返回 None；摘除未登记键 no-op。
		assert!(instance_from_id(u64::MAX).is_none());
		unregister_instance(u64::MAX);
	}

	#[test]
	fn shared_instance_unknown_identifier_yields_none() {
		// 缓存里查无此标识 → None（montage 路径据此按"无求值器"
		// 告警并直通）。
		assert!(shared_plugin_instance("com.example.definitely-missing").is_none());
	}

	/// 最简单的真实插件（AddOFX：每个像素加一个常量）经 executor
	/// 路径渲染成功——区分"系统性宿主问题"与"单插件问题"（机器上
	/// 没装该插件则跳过）。
	#[test]
	fn add_plugin_renders_through_the_executor() {
		render_real_plugin_smoke("net.sf.openfx.AddPlugin");
	}

	/// 真实 GL-only 插件（CImg 的 ChromaKeyerOFX）经 executor 路径渲染
	/// 成功——GL-kind 标记必须把它引上可用的 GL 路径，而不是
	/// MissingHostFeature 紫帧（机器上没装该插件则跳过）。
	#[test]
	fn cimg_chromakeyer_renders_through_the_executor() {
		render_real_plugin_smoke("net.sf.openfx.ChromaKeyerPlugin");
	}

	fn render_real_plugin_smoke(identifier: &str) {
		let host = crate::host::Host::global();
		let _ = host.cache.scan();
		if host.cache.find(identifier).is_none() {
			eprintln!("{identifier} not installed; skipping");
			return;
		}
		install_render_executor();
		let instance = shared_plugin_instance(identifier)
			.expect("the shared instance factory resolves the plugin");
		if std::env::var_os("OAK_OFX_TRACE").is_some() {
			let inst = instance_from_id(instance).expect("registered");
			let props = &inst.value.plugin.descriptor.props;
			props.with_locked(|all| {
				for p in all.iter().filter(|p| p.name.contains("GL") || p.name.contains("OpenGL")) {
					eprintln!("[probe] desc prop {} = {:?}", p.name, p.values);
				}
			});
		}

		// 8x8 不透明绿帧（典型 keyer 输入）。
		let mut frame = oak_render::eval::generate_frame(
			oak_core::Rational::new(0, 1),
			(8, 8),
			oak_core::PixelFormat::F32,
		)
		.unwrap();
		for px in frame.data.chunks_exact_mut(16) {
			for (c, v) in px.chunks_exact_mut(4).zip([0.1f32, 0.9, 0.1, 1.0]) {
				c.copy_from_slice(&v.to_le_bytes());
			}
		}
		let spec = oak_render::eval::JobSpec::Plugin {
			instance,
			time: 0.0,
			effect_input_id: Some("Source".to_string()),
			inputs: Vec::new(),
			values: Vec::new(),
		};
		let out = execute_plugin_job(&oak_render::eval::PluginJobRequest {
			spec: &spec,
			src: oak_core::texture::Texture::wrap_frame(frame),
		})
		.expect("the GL-capable render succeeds (no MissingHostFeature purple)");
		let oak_core::texture::Texture::Cpu(out_frame) = &out else {
			panic!("a CPU frame comes back");
		};
		let mut px = [0f32; 4];
		for i in 0..4 {
			px[i] = f32::from_le_bytes(out_frame.data[i * 4..i * 4 + 4].try_into().unwrap());
		}
		eprintln!("[smoke] {identifier} first pixel out = {px:?}");
		assert_ne!(px, [1.0, 0.0, 1.0, 1.0], "must not be the purple failure frame");
		assert_ne!(px, [0.0, 0.0, 0.0, 0.0], "must not be a black empty frame");
	}

	/// 构造一个只含 push-button 参数的最小实例（直接登记进注册表）。
	fn instance_with_push_button() -> u64 {
        use crate::descriptor::EffectDescriptor;
        use crate::handle::RefBox;
        use crate::host::Plugin;
        use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
        use std::ffi::{c_char, c_void};
        use std::sync::atomic::AtomicU32;

        unsafe extern "C" fn dummy_entry(
			action: *const c_char,
			_: *const c_void,
			in_args: *mut c_void,
			_: *mut c_void,
		) -> i32 {
			if !action.is_null() {
				let action = unsafe {
					std::ffi::CStr::from_ptr(action).to_string_lossy().into_owned()
				};
				if action == crate::host::ACTION_INSTANCE_CHANGED {
					let mut calls = PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner());
					calls.push(action);
					// The instanceChanged inArgs contract (ofxCore.h:405-435):
					// kOfxPropChangeReason = kOfxChangeUserEdited.
					let props = unsafe { &*(in_args as *const crate::property::PropertySet) };
					let reason = props.get(crate::host::PROP_CHANGE_REASON, 0);
					let reason = match reason {
						Some(crate::property::Value::String(s)) => {
							s.to_string_lossy().into_owned()
						}
						_ => String::new(),
					};
					calls.push(reason);
				}
			}
			0
		}
		let plugin = Arc::new(Plugin {
			identifier: "test.plugin".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		});
		let mut params = ParamSetInstance { params: Vec::new() };
		params.params.push(Box::new(ParamInstance::from_def(ParamDef::new(
			"button",
			ofx::TYPE_PUSHBUTTON,
		))));
		params.params.push(Box::new(ParamInstance::from_def(ParamDef::new(
			"gain",
			ofx::TYPE_DOUBLE,
		))));
		let inst = crate::instance::Instance {
			props: crate::property::PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: Vec::new(),
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		register_instance(Arc::new(RefBox {
			refs: AtomicU32::new(1),
			value: inst,
		}))
	}

	/// push_button_clicked：实例/参数查无与类型不匹配 → false；命中 →
	/// true、置 PushButton 值并把按下路由为 kOfxActionInstanceChanged
	/// （UserEdited，inArgs 带 change reason / name / type / time /
	/// render scale）。
	#[test]
	fn push_button_clicked_routes_instance_changed() {
		*PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()) = Vec::new();
		let id = instance_with_push_button();
		assert!(push_button_clicked(id, "button"));
		// 类型不匹配 / 查无参数 / 查无实例：不触发 entry。
		assert!(!push_button_clicked(id, "gain"));
		assert!(!push_button_clicked(id, "nope"));
		assert!(!push_button_clicked(u64::MAX, "button"));
		// 只 "button" 那次按下进了插件 entry，且 reason 是 UserEdited。
		let calls = PUSH_ENTRY_CALLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
		assert_eq!(calls, vec!["OfxActionInstanceChanged", "OfxChangeUserEdited"]);
		unregister_instance(id);
	}

	/// 构造一个带 parametric 参数（维度 2、自定义 range、双维 UI 颜色
	/// 已配置）与一个 String 参数的实例（直接登记进注册表）。
	fn instance_with_parametric() -> u64 {
        use crate::descriptor::EffectDescriptor;
        use crate::handle::RefBox;
        use crate::host::Plugin;
        use crate::param::{ParamDef, ParamInstance, ParamSetInstance};
        use std::ffi::{c_char, c_void};
        use std::sync::atomic::AtomicU32;

        unsafe extern "C" fn dummy_entry(
			_: *const c_char,
			_: *const c_void,
			_: *mut c_void,
			_: *mut c_void,
		) -> i32 {
			0
		}
		let def = ParamDef::new("curve", ofx::TYPE_PARAMETRIC);
		def.props
			.set_one(ofx::PROP_LABEL, PropValue::String(CString::new("Curve").unwrap()));
		def.props
			.set_one(ofx::P_PARAMETRIC_DIMENSION, PropValue::Int(2));
		def.props.define(
			ofx::P_PARAMETRIC_RANGE,
			vec![PropValue::Double(0.0), PropValue::Double(1.0)],
		);
		def.props.define(
			ofx::P_PARAMETRIC_UI_COLOUR,
			vec![
				PropValue::Double(1.0),
				PropValue::Double(0.0),
				PropValue::Double(0.0),
				PropValue::Double(0.0),
				PropValue::Double(1.0),
				PropValue::Double(0.0),
			],
		);
		let mut params = ParamSetInstance { params: Vec::new() };
		params
			.params
			.push(Box::new(ParamInstance::from_def(def)));
		params.params.push(Box::new(ParamInstance::from_def(
			ParamDef::new("title", ofx::TYPE_STRING),
		)));
		let plugin = Arc::new(Plugin {
			identifier: "test.parametric".into(),
			version: (1, 0),
			bundle_path: std::path::PathBuf::new(),
			contexts: vec![],
			descriptor: EffectDescriptor::new(),
			lib: std::ptr::null_mut(),
			entry: dummy_entry,
			ofx_plugin: std::ptr::null_mut(),
			unloaded: std::sync::atomic::AtomicBool::new(false),
		});
		let inst = crate::instance::Instance {
			props: crate::property::PropertySet::new(),
			plugin,
			context: "OfxImageEffectContextFilter".into(),
			params,
			clips: Vec::new(),
			node_identity: std::sync::atomic::AtomicUsize::new(0),
			destroyed: std::sync::atomic::AtomicBool::new(false),
			sequence_range: std::sync::Mutex::new(None),
			progress_cb: std::sync::Mutex::new(None),
			cancel: std::sync::atomic::AtomicBool::new(false),
			edit: std::sync::Mutex::new(crate::instance::EditTransaction::new()),
			render_lock: std::sync::Mutex::new(()),
			interact: std::sync::Mutex::new(None),
		};
		register_instance(Arc::new(RefBox {
			refs: AtomicU32::new(1),
			value: inst,
		}))
	}

	/// 翻译 pass 产出 Parametric 输入：value_type、默认值（= 默认曲线
	/// 的 JSON）、display_name（= label）、不可键帧 + 专属属性
	/// （dimension / range / 每维一条 UI 颜色）。
	#[test]
	fn build_core_produces_parametric_input() {
		let id = instance_with_parametric();
		let inst = instance_from_id(id).expect("已登记");
		let core = build_core(&inst.value);

		let input = core.get_input("curve").expect("parametric 输入应存在");
		assert_eq!(input.value_type, ValueType::Parametric);
		assert_eq!(input.display_name, "Curve");
		assert_ne!(input.flags & input_flags::NOT_KEYFRAMABLE, 0);

		// 默认值 = def.default 曲线集的 JSON（type_default 产出 1 条
		// 恒等曲线；dimension 属性只做索引门控，未配置曲线按恒等读，
		// 不扩充默认值）。
		let expected = crate::param_curve::curves_to_json(&[crate::param_curve::Curve::identity(
			0.0, 1.0,
		)]);
		match &input.default {
			NodeValue::Text(s) => assert_eq!(s, &expected, "默认值应为曲线 JSON"),
			other => panic!("预期 Text(JSON)，实际 {other:?}"),
		}
		// 标准值 = 默认（工程序列化从这里走）。
		assert_eq!(core.standard_value("curve", -1), input.default);

		// 专属属性。
		let props = |name: &str| {
			input
				.properties
				.iter()
				.filter(|(k, _)| k == name)
				.map(|(_, v)| v.clone())
				.collect::<Vec<_>>()
		};
		assert_eq!(props("parametric_dimension"), vec![NodeValue::Int(2)]);
		assert_eq!(
			props("parametric_range"),
			vec![NodeValue::Vec2([0.0, 1.0])]
		);
		// 双维 → 两条颜色（重复键）。
		assert_eq!(
			props("parametric_ui_colour"),
			vec![
				NodeValue::Color([1.0, 0.0, 0.0, 1.0]),
				NodeValue::Color([0.0, 1.0, 0.0, 1.0]),
			]
		);
		unregister_instance(id);
	}

	/// parametric 输入（Text JSON）→ 实例写回：有效 JSON 解析并
	/// `set_ofx(Parametric)`；坏 JSON / 类型不符 → 静默忽略（保持现值）。
	#[test]
	fn parametric_input_writes_back_to_instance() {
		let id = instance_with_parametric();
		let inst = instance_from_id(id).expect("已登记");

		// 有效 JSON（双维，第二维单点）→ 曲线写回，求值即实例曲线。
		let json = r#"{"curves":[[{"key":0,"value":0,"slope":1},{"key":0.5,"value":0.7,"slope":1}],[{"key":0,"value":0,"slope":1}]]}"#;
		set_text_param(&inst.value, "curve", json);
		let curves = match inst.value.params.find("curve").unwrap().get() {
			ParamValue::Parametric(c) => c,
			other => panic!("应写回 Parametric，实际 {other:?}"),
		};
		assert_eq!(curves[0].evaluate(0.5), 0.7);
		assert_eq!(curves[1].len(), 1);

		// 坏 JSON → 不改值（与字符串族类型不匹配静默一致）。
		let before = inst.value.params.find("curve").unwrap().get();
		set_text_param(&inst.value, "curve", "{broken");
		assert_eq!(inst.value.params.find("curve").unwrap().get(), before);

		// 非 parametric 参数（String）遇任意文本 → 字符串值路径。
		set_text_param(&inst.value, "title", "hello");
		assert!(matches!(
			inst.value.params.find("title").unwrap().get(),
			ParamValue::String(_)
		));
		unregister_instance(id);
	}
}
