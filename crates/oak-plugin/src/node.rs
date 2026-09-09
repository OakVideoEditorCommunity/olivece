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

//! oaknode 桥（single-lib unification）：节点值 POD、身份注册表与
//! undoable 参数回写。
//!
//! ## Value 布局冻结
//!
//! [`Value`] 即 include/node/node.h:93 的 `oaknode_value` POD，字段
//! 逐字一致（type/num/den/f[4]；`type` 取值见 [`node_value_type`]）。
//! 字符串族输入（k_file/k_text/k_font/k_str_combo，node.h:48-52）没有
//! POD 表示——走 `*_input_string_*` 专用函数（本桥的
//! [`set_input_string_undoable`]）；`OAKNODE_VALUE_STRING` 的 POD 里
//! 不携带字符串数据。
//!
//! ## 身份注册表（单库化重建）
//!
//! oaknode 的 C ABI 已删除（单库化）：节点对象是
//! `oak_node::project::Project`（Arc<Mutex<Project>>）里的
//! `oak_node::graph::Graph` 条目，按 [`oak_node::id::NodeId`] 定位。
//! 本 crate 的 [`NodeRef`] 即旧句柄盒
//! `(Arc<Mutex<Project>>, NodeId)` 的值型复刻；进程级注册表
//! [`register_node`]/[`node_from_identity`] 按
//! [`oak_node::id::NodeId::identity`] 的打包身份（u64）做弱引用
//! 映射（project 释放后条目自然失效，升级失败 → None）——
//! 对应 M9 C++ 版 `oaknode_node_identity()` 注册表的地址语义。
//! facade 装配期调 [`register_node`] 登记节点，把返回身份写进
//! [`crate::instance::Instance::bind_node`]（`node_identity`）。
//!
//! ## undoable 回写
//!
//! [`set_input_undoable`]/[`set_input_string_undoable`] 按
//! `oak_node::ops::set_value_at_time_command` 的同一 closure
//! 模式（`command_from_closures`）构造未执行的
//! `oak_undo::undocommand::UndoCommand`：redo 闭包锁 project、
//! `graph.get_mut(id)`、`NodeCore::set_standard_value` 写标准值；
//! undo 闭包回放创建期快照的旧值。命令由调用方
//! （[`crate::param::notify_instance_changed`] →
//! [`crate::instance::Instance::submit_undo_command`]）提交。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use oak_undo::undocommand::UndoCommand;

/// oaknode 节点引用（single-lib）：旧 C ABI 句柄盒
/// `(Arc<Mutex<Project>>, NodeId)` 的值型复刻。
#[derive(Clone)]
pub struct NodeRef {
	/// 所属 project（节点生命周期随 project）。
	pub project: Arc<Mutex<oak_node::project::Project>>,
	/// 节点在 project.graph 里的 id。
	pub id: oak_node::id::NodeId,
}

/// oaknode_value_type 的取值（node.h:74）。
pub mod node_value_type {
	/// OAKNODE_VALUE_NONE。
	pub const NONE: i32 = 0;
	/// OAKNODE_VALUE_INT。
	pub const INT: i32 = 1;
	/// OAKNODE_VALUE_FLOAT。
	pub const FLOAT: i32 = 2;
	/// OAKNODE_VALUE_BOOL。
	pub const BOOL: i32 = 3;
	/// OAKNODE_VALUE_RATIONAL。
	pub const RATIONAL: i32 = 4;
	/// OAKNODE_VALUE_COLOR。
	pub const COLOR: i32 = 5;
	/// OAKNODE_VALUE_VEC2。
	pub const VEC2: i32 = 6;
	/// OAKNODE_VALUE_VEC3。
	pub const VEC3: i32 = 7;
	/// OAKNODE_VALUE_VEC4。
	pub const VEC4: i32 = 8;
	/// OAKNODE_VALUE_COMBO。
	pub const COMBO: i32 = 9;
	/// OAKNODE_VALUE_STRING。
	pub const STRING: i32 = 10;
}

/// oaknode_value（include/node/node.h:93，字段逐字一致）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Value {
	/// 类型（[`node_value_type`]）。
	pub r#type: i32,
	/// INT/COMBO 值、BOOL 0/1、RATIONAL 分子。
	pub num: i64,
	/// RATIONAL 分母。
	pub den: i64,
	/// FLOAT f[0]；VEC2/3/4 f[0..n-1]；COLOR r,g,b,a。
	pub f: [f64; 4],
}

impl Value {
	/// 类型化构造：整数 / choice 索引。
	pub const fn int(v: i64) -> Self {
		Self {
			r#type: node_value_type::INT,
			num: v,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// 类型化构造：浮点。
	pub const fn float(v: f64) -> Self {
		Self {
			r#type: node_value_type::FLOAT,
			num: 0,
			den: 0,
			f: [v, 0.0, 0.0, 0.0],
		}
	}

	/// 类型化构造：布尔。
	pub const fn bool_(v: bool) -> Self {
		Self {
			r#type: node_value_type::BOOL,
			num: v as i64,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// 类型化构造：choice（COMBO）。
	pub const fn combo(v: i64) -> Self {
		Self {
			r#type: node_value_type::COMBO,
			num: v,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// 类型化构造：颜色。
	pub const fn color(r: f64, g: f64, b: f64, a: f64) -> Self {
		Self {
			r#type: node_value_type::COLOR,
			num: 0,
			den: 0,
			f: [r, g, b, a],
		}
	}

	/// 类型化构造：vec2/3/4（长度按 f 数组尾部 0 判定）。
	pub const fn vec(v: &[f64]) -> Self {
		let t = match v.len() {
			2 => node_value_type::VEC2,
			3 => node_value_type::VEC3,
			_ => node_value_type::VEC4,
		};
		let mut f = [0.0; 4];
		let mut i = 0;
		while i < v.len() && i < 4 {
			f[i] = v[i];
			i += 1;
		}
		Self {
			r#type: t,
			num: 0,
			den: 0,
			f,
		}
	}

	/// 类型化构造：字符串族（POD 不携带数据；值经
	/// [`set_input_string_undoable`] 传递）。
	pub const fn string() -> Self {
		Self {
			r#type: node_value_type::STRING,
			num: 0,
			den: 0,
			f: [0.0; 4],
		}
	}

	/// POD → [`oak_node::value::NodeValue`]，按 POD kind 映射（与
	/// `oak_node::value::OakNodeValue::to_node_value` 的 kind 分支一致，
	/// 不按声明类型重量化）。`STRING` 族 POD 不携带数据 → `None`；
	/// `NONE` → [`oak_node::value::NodeValue::None`]。
	pub fn to_node_value(&self) -> Option<oak_node::value::NodeValue> {
		use oak_node::value::NodeValue as NV;
		match self.r#type {
			node_value_type::NONE => Some(NV::None),
			node_value_type::INT => Some(NV::Int(self.num)),
			node_value_type::FLOAT => Some(NV::Float(self.f[0])),
			node_value_type::BOOL => Some(NV::Boolean(self.num != 0)),
			node_value_type::RATIONAL => Some(NV::Rational(oak_core::Rational::new(
				self.num, self.den,
			))),
			node_value_type::COLOR => Some(NV::Color(self.f)),
			node_value_type::VEC2 => Some(NV::Vec2([self.f[0], self.f[1]])),
			node_value_type::VEC3 => Some(NV::Vec3([self.f[0], self.f[1], self.f[2]])),
			node_value_type::VEC4 => Some(NV::Vec4(self.f)),
			node_value_type::COMBO => Some(NV::Combo(self.num)),
			_ => None,
		}
	}

	/// [`oak_node::value::NodeValue`] → POD（`ValueType::to_oak` 的逆）。
	/// 无 POD 表达的类型（字符串族/纹理/采样/矩阵等）→ `None`。
	pub fn from_node_value(v: &oak_node::value::NodeValue) -> Option<Value> {
		use oak_node::value::NodeValue as NV;
		Some(match v {
			NV::None => Value {
				r#type: node_value_type::NONE,
				num: 0,
				den: 0,
				f: [0.0; 4],
			},
			NV::Int(i) => Value::int(*i),
			NV::Float(f) => Value::float(*f),
			NV::Boolean(b) => Value::bool_(*b),
			NV::Rational(r) => Value {
				r#type: node_value_type::RATIONAL,
				num: r.numerator(),
				den: r.denominator(),
				f: [0.0; 4],
			},
			NV::Color(c) => Value::color(c[0], c[1], c[2], c[3]),
			NV::Vec2(a) => Value::vec(&[a[0], a[1]]),
			NV::Vec3(a) => Value::vec(&[a[0], a[1], a[2]]),
			NV::Vec4(a) => Value::vec(&[a[0], a[1], a[2], a[3]]),
			NV::Combo(i) => Value::combo(*i),
			_ => return None,
		})
	}
}

// ---- 身份注册表 -----------------------------------------------------------

/// 注册表条目：弱 project 引用（project 释放后升级失败，条目变死）
/// + 节点 id。
struct RegistryEntry {
	/// 所属 project（弱引用；强引用由调用方/节点持有）。
	project: Weak<Mutex<oak_node::project::Project>>,
	/// 节点 id。
	id: oak_node::id::NodeId,
}

/// 进程级身份注册表（OnceLock 惰性初始化；测试进程内可重复 register）。
static NODE_REGISTRY: OnceLock<Mutex<HashMap<u64, RegistryEntry>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<u64, RegistryEntry>> {
	NODE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 登记 oaknode 节点（facade 装配期调用；对应 M9 C++ 版
/// `oaknode_node_identity()` 注册表的登记侧）。返回的注册表键是打包
/// 身份 +1（[`oak_node::id::NodeId::identity`]）：身份 0 是合法的首
/// 节点，而 `Instance::node_identity` 以 0 为未绑定哨兵，移位后二者
/// 永不冲突。键写入 [`crate::instance::Instance::bind_node`]，摘除与
/// 查找（[`node_from_identity`]）用同一键。同一身份重复登记覆盖旧
/// 条目（重绑定）。
pub fn register_node(
	project: Arc<Mutex<oak_node::project::Project>>,
	id: oak_node::id::NodeId,
) -> u64 {
	let identity = id
		.identity()
		.checked_add(1)
		.expect("register_node: the invalid node id has no registry key");
	let entry = RegistryEntry {
		project: Arc::downgrade(&project),
		id,
	};
	registry()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.insert(identity, entry);
	identity
}

/// 摘除身份（节点销毁路径调用；未知身份 no-op）。
pub fn unregister_node(identity: u64) {
	registry()
		.lock()
		.unwrap_or_else(|e| e.into_inner())
		.remove(&identity);
}

/// 身份 → 节点引用（param 回写入口）。
///
/// `None`：身份未登记，或 project 已释放（弱引用升级失败——旧 C
/// ABI 对悬垂句柄的可解释失败路径）。调用方
/// （[`crate::param::notify_instance_changed`]）按 "未绑定节点 →
/// no-op" 处理。
pub fn node_from_identity(identity: usize) -> Option<NodeRef> {
	let (project, id) = {
		let reg = registry()
			.lock()
			.unwrap_or_else(|e| e.into_inner());
		let entry = reg.get(&(identity as u64))?;
		(entry.project.clone(), entry.id)
	};
	let project = project.upgrade()?;
	Some(NodeRef { project, id })
}

// ---- 桥调用面（undoable 回写）---------------------------------------------

/// oaknode 桥错误（set_input* 的失败原因；调用方按失败跳过提交）。
#[derive(Debug, thiserror::Error)]
pub enum NodeBridgeError {
	/// 节点 id 已失效（图内无此节点）。
	#[error("node id is stale or not present in the graph")]
	StaleNode,
	/// 节点上无该输入。
	#[error("node has no input '{0}'")]
	UnknownInput(String),
	/// 值 POD 无法转换（STRING 族 POD 不携带数据；数值族恒可转换）。
	#[error("value POD of type {0} cannot be converted to a node value")]
	InvalidValue(i32),
	/// 输入不是字符串族类型（string setter 限定 Text/StrCombo）。
	#[error("input '{0}' is not a string-family input")]
	NotStringInput(String),
}

/// 取锁（毒锁接管，一次 panic 不级联）。
fn lock_any<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
	m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 从 redo/undo 闭包构造未执行的 [`UndoCommand`]（closure 模式，与
/// `oak_node::ops::command_from_closures` 同构）。
fn command_from_closures(
	redo: impl FnMut() + Send + 'static,
	undo: impl FnMut() + Send + 'static,
) -> UndoCommand {
	UndoCommand::from_closures(redo, undo)
}

/// 以 undoable 方式设置节点的标准输入值（POD 路径；数值族输入）。
///
/// 返回**未执行**的 [`UndoCommand`]（调用方
/// [`crate::instance::Instance::submit_undo_command`] 提交）：redo
/// 锁 project、`graph.get_mut(id)`、`NodeCore::set_standard_value`
/// 写新值；undo 回放创建期快照的旧值（与
/// `oak_node::ops::set_value_at_time_command` 的 standard-value 分支
/// 同构，element = -1 整值）。节点/输入失效时返回
/// [`NodeBridgeError`]（不产出命令）。
pub fn set_input_undoable(
	node: &NodeRef,
	input: &str,
	value: &Value,
) -> Result<UndoCommand, NodeBridgeError> {
	let nv = value
		.to_node_value()
		.ok_or(NodeBridgeError::InvalidValue(value.r#type))?;
	// 校验节点与输入存在性 + 旧值快照（创建期状态，redo/undo 据此
	// 回放；不校验声明类型——按 POD kind 存原样，与 ops.rs 一致）。
	let old = {
		let mut guard = lock_any(&node.project);
		let entry = guard
			.graph
			.get_mut(node.id)
			.ok_or(NodeBridgeError::StaleNode)?;
		if entry.core.input_data_type(input).is_none() {
			return Err(NodeBridgeError::UnknownInput(input.to_string()));
		}
		entry.core.standard_value(input, -1)
	};

	let project = node.project.clone();
	let id = node.id;
	let input_redo = input.to_string();
	let input_undo = input.to_string();
	let value_redo = nv.clone();
	let project_redo = project.clone();
	let project_undo = project.clone();
	Ok(command_from_closures(
		move || {
			let mut guard = lock_any(&project_redo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_redo, -1, value_redo.clone());
			}
		},
		move || {
			let mut guard = lock_any(&project_undo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_undo, -1, old.clone());
			}
		},
	))
}

/// 以 undoable 方式设置节点的标准输入值（字符串族输入：Text /
/// StrCombo / Parametric——POD 不携带字符串数据）。
///
/// 按输入的声明类型（[`oak_node::value::ValueType`]）选存储变体：
/// `StrCombo` → [`oak_node::value::NodeValue::StrCombo`]，
/// `Text` / `Parametric`（值 = 曲线 JSON 文本）→
/// [`oak_node::value::NodeValue::Text`]。声明类型不是字符串族 →
/// [`NodeBridgeError::NotStringInput`]。命令语义与
/// [`set_input_undoable`] 相同。
pub fn set_input_string_undoable(
	node: &NodeRef,
	input: &str,
	value: &str,
) -> Result<UndoCommand, NodeBridgeError> {
	let (old, declared) = {
		let guard = lock_any(&node.project);
		let entry = guard
			.graph
			.get(node.id)
			.ok_or(NodeBridgeError::StaleNode)?;
		let declared = entry
			.core
			.input_data_type(input)
			.ok_or_else(|| NodeBridgeError::UnknownInput(input.to_string()))?;
		(entry.core.standard_value(input, -1), declared)
	};
	let nv = match declared {
		oak_node::value::ValueType::StrCombo => oak_node::value::NodeValue::StrCombo(value.to_string()),
		oak_node::value::ValueType::Text | oak_node::value::ValueType::Parametric => {
			oak_node::value::NodeValue::Text(value.to_string())
		}
		_ => return Err(NodeBridgeError::NotStringInput(input.to_string())),
	};

	let project = node.project.clone();
	let id = node.id;
	let input_redo = input.to_string();
	let input_undo = input.to_string();
	let value_redo = nv.clone();
	let project_redo = project.clone();
	let project_undo = project.clone();
	Ok(command_from_closures(
		move || {
			let mut guard = lock_any(&project_redo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_redo, -1, value_redo.clone());
			}
		},
		move || {
			let mut guard = lock_any(&project_undo);
			if let Some(entry) = guard.graph.get_mut(id) {
				entry
					.core
					.set_standard_value(&input_undo, -1, old.clone());
			}
		},
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Serializes the identity-registry tests: every `test_project()` node
	/// gets the same packed `NodeId::identity` (a fresh graph's first
	/// node), so two tests running in parallel collide in the process-wide
	/// registry and one resolves the other's live entry.
	fn identity_lock() -> &'static std::sync::Mutex<()> {
		static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
		&LOCK
	}

	/// 建一个含 Text/StrCombo/Float 输入的测试节点。
	fn test_project() -> (Arc<Mutex<oak_node::project::Project>>, oak_node::id::NodeId) {
		let project = oak_node::project::Project::new();
		let mut guard = project.lock().unwrap();
		let id = guard.graph.add_node(
			oak_node::node::NodeCore::empty(),
			Box::new(oak_node::nodes::EmptyBehavior),
		);
		// NodeCore::empty 无输入；手动声明三种输入。
		use oak_node::input::Input;
		let core = guard.graph.get_mut(id).unwrap();
		core.core.add_input(Input::new(
			"label",
			oak_node::value::ValueType::Text,
			oak_node::value::NodeValue::Text(String::new()),
		));
		core.core.add_input(Input::new(
			"choice",
			oak_node::value::ValueType::StrCombo,
			oak_node::value::NodeValue::StrCombo(String::new()),
		));
		core.core.add_input(Input::new(
			"opacity",
			oak_node::value::ValueType::Float,
			oak_node::value::NodeValue::Float(1.0),
		));
		drop(guard);
		(project, id)
	}

	/// 注册表：register → node_from_identity → unregister 全链。
	#[test]
	fn identity_register_roundtrip() {
		let _guard = identity_lock().lock().unwrap_or_else(|e| e.into_inner());
		let (project, id) = test_project();
		let identity = register_node(project.clone(), id);
		let node = node_from_identity(identity as usize).expect("已登记身份应可解析");
		assert_eq!(node.id, id);
		let found = node.project.lock().unwrap().graph.get(id).is_some();
		assert!(found);

		// 未登记身份 → None。
		assert!(node_from_identity(0xfeed_face).is_none());
		unregister_node(identity);
		assert!(node_from_identity(identity as usize).is_none());
	}

	/// project 释放后弱条目升级失败 → None（悬垂语义）；重复登记
	/// 覆盖死条目后重新解析到新 project。
	#[test]
	fn identity_project_dropped() {
		let _guard = identity_lock().lock().unwrap_or_else(|e| e.into_inner());
		let (project, id) = test_project();
		let identity = register_node(project.clone(), id);
		drop(project);
		assert!(node_from_identity(identity as usize).is_none());
		// 新 project 同槽位身份重登记（覆盖死条目）。
		let (project2, id2) = test_project();
		register_node(project2.clone(), id2);
		let node = node_from_identity(identity as usize).expect("重登记后应可解析");
		assert_eq!(node.id, id2);
		let alive = node.project.lock().unwrap().graph.get(id2).is_some();
		assert!(alive);
	}

	/// Value POD ↔ NodeValue 全类型映射；STRING 族 POD 无数据。
	#[test]
	fn pod_node_value_roundtrip() {
		use oak_node::value::NodeValue as NV;
		let cases: Vec<(Value, NV)> = vec![
			(Value::int(7), NV::Int(7)),
			(Value::float(1.5), NV::Float(1.5)),
			(Value::bool_(true), NV::Boolean(true)),
			(Value::combo(3), NV::Combo(3)),
			(
				Value {
					r#type: node_value_type::RATIONAL,
					num: 3,
					den: 2,
					f: [0.0; 4],
				},
				NV::Rational(oak_core::Rational::new(3, 2)),
			),
			(Value::color(0.1, 0.2, 0.3, 0.4), NV::Color([0.1, 0.2, 0.3, 0.4])),
			(Value::vec(&[1.0, 2.0]), NV::Vec2([1.0, 2.0])),
			(Value::vec(&[1.0, 2.0, 3.0]), NV::Vec3([1.0, 2.0, 3.0])),
			(
				Value::vec(&[1.0, 2.0, 3.0, 4.0]),
				NV::Vec4([1.0, 2.0, 3.0, 4.0]),
			),
		];
		for (pod, nv) in cases {
			let to = pod.to_node_value().expect("POD 应可转换");
			assert_eq!(to, nv, "POD → NodeValue");
			assert_eq!(Value::from_node_value(&nv), Some(pod), "NodeValue → POD");
		}
		// STRING POD 无数据。
		assert!(Value::string().to_node_value().is_none());
		// 无 POD 表达的类型。
		assert_eq!(Value::from_node_value(&NV::Text("x".into())), None);
		assert_eq!(Value::from_node_value(&NV::Matrix([0.0; 16])), None);
	}

	/// set_input_undoable：redo 写标准值、undo 回放旧值；错误路径。
	#[test]
	fn set_input_undoable_applies_and_undoes() {
		let (project, id) = test_project();
		let node = NodeRef {
			project: project.clone(),
			id,
		};
		let mut cmd = set_input_undoable(&node, "opacity", &Value::float(0.25)).unwrap();
		// 命令未执行时值未变。
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			oak_node::value::NodeValue::Float(1.0)
		);
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			oak_node::value::NodeValue::Float(0.25)
		);
		cmd.undo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("opacity", -1),
			oak_node::value::NodeValue::Float(1.0)
		);

		// 未知输入 / STRING POD / 失效节点。
		assert!(matches!(
			set_input_undoable(&node, "nope", &Value::float(1.0)),
			Err(NodeBridgeError::UnknownInput(_))
		));
		assert!(matches!(
			set_input_undoable(&node, "opacity", &Value::string()),
			Err(NodeBridgeError::InvalidValue(_))
		));
		let stale = NodeRef {
			project,
			id: oak_node::id::NodeId::from_identity(999).unwrap(),
		};
		assert!(matches!(
			set_input_undoable(&stale, "opacity", &Value::float(1.0)),
			Err(NodeBridgeError::StaleNode)
		));
	}

	/// set_input_string_undoable：Text/StrCombo 变体选择与回放。
	#[test]
	fn set_input_string_undoable_variants() {
		let (project, id) = test_project();
		let node = NodeRef {
			project: project.clone(),
			id,
		};
		let mut cmd = set_input_string_undoable(&node, "label", "hello").unwrap();
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("label", -1),
			oak_node::value::NodeValue::Text("hello".to_string())
		);
		cmd.undo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("label", -1),
			oak_node::value::NodeValue::Text(String::new())
		);

		let mut cmd = set_input_string_undoable(&node, "choice", "low").unwrap();
		cmd.redo_now();
		assert_eq!(
			project
				.lock()
				.unwrap()
				.graph
				.get(id)
				.unwrap()
				.core
				.standard_value("choice", -1),
			oak_node::value::NodeValue::StrCombo("low".to_string())
		);

		// 数值输入走字符串 setter → NotStringInput。
		assert!(matches!(
			set_input_string_undoable(&node, "opacity", "1.0"),
			Err(NodeBridgeError::NotStringInput(_))
		));
	}
}
