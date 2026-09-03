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

//! 阶段 6a 端到端：OFX 插件 → 节点工厂 → 节点图 → 渲染出帧。
//!
//! 链路：`scan_path`（最小测试插件，cbits/oak_test_plugin.c）→
//! `node_factory::register_plugin_nodes`（动态注册）→
//! `Factory::create_any`（参数翻译的输入表）→ Graph 连接常量纹理
//! 源 → `Traverser::evaluate` + `RenderEvalHooks`（解 PluginJobPayload
//! 经 render_driver 出帧）→ 像素断言。
//!
//! 测试插件未构建时全部 skip（common 约定）。宿主单例经
//! `common::with_host` 串行化。

mod common;

use oak_core::{PixelFormat, Rational};
use oak_node::factory::Factory;
use oak_node::graph::Graph;
use oak_node::node::{NodeBehavior, NodeCore};
use oak_node::traverser::{EvalRequest, Traverser};
use oak_node::value::{NodeValue, ValueType};
use oak_plugin::host::Host;
use oak_core::texture::Texture;

const PLUGIN_ID: &str = "org.oak.test-plugin";
const IDENTITY_ID: &str = "org.oak.test-plugin.identity";

/// 常量纹理源节点：推一张填充实色的 F32 帧（测试专用行为）。
struct ConstSource {
	rgba: [f32; 4],
	size: (i32, i32),
}

impl NodeBehavior for ConstSource {
	fn name(&self) -> &str {
		"ConstSource"
	}

	fn type_id(&self) -> &str {
		"test.const-source"
	}

	fn duplicate(&self, _core: &NodeCore) -> Option<Box<dyn NodeBehavior>> {
		Some(Box::new(ConstSource {
			rgba: self.rgba,
			size: self.size,
		}))
	}

	fn value(
		&self,
		_core: &NodeCore,
		_inputs: &oak_node::value::NodeValueRow,
		time: Rational,
		table: &mut oak_node::value::NodeValueTable,
	) {
		let mut frame =
			oak_render::eval::generate_frame(time, self.size, PixelFormat::F32).unwrap();
		for pixel in frame.data.chunks_exact_mut(16) {
			for (i, v) in self.rgba.iter().enumerate() {
				pixel[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
			}
		}
		table.push(
			ValueType::Texture,
			NodeValue::Texture(oak_node::handle::make_owned(Texture::wrap_frame(frame))),
			None,
		);
	}
}

/// 扫描 + 注册（幂等；宿主不可用返回 false = skip）。
fn scan_and_register() -> bool {
	let Some(dir) = common::test_plugin_scan_dir() else {
		common::skip("最小测试插件未构建");
		return false;
	};
	if Host::global().cache.scan_path(&dir).is_err() {
		common::skip("测试插件扫描失败");
		return false;
	}
	oak_plugin::node_factory::register_plugin_nodes();
	true
}

/// 取输出表的渲染纹理（resolve 后的真纹理盒）。
fn rendered_texture(
	table: &oak_node::value::NodeValueTable,
) -> Texture {
	let NodeValue::Texture(handle) = table
		.get(ValueType::Texture)
		.expect("根输出应有纹理")
	else {
		panic!("纹理槽不是 Texture 值");
	};
	unsafe { oak_node::handle::get_checked::<Texture>(handle) }
		.cloned()
		.expect("纹理盒必须是渲染产物（PluginJobPayload 已 resolve）")
}

fn first_pixel(texture: &Texture) -> [f32; 4] {
	let Texture::Cpu(frame) = texture else {
		panic!("期望 CPU 帧");
	};
	let mut out = [0f32; 4];
	for i in 0..4 {
		out[i] = f32::from_le_bytes(frame.data[i * 4..i * 4 + 4].try_into().unwrap());
	}
	out
}

/// 注册 + 参数翻译：动态条目、输入类型/默认值/显示名/隐藏标记/
/// combo 选项/effect_input（对齐 plugin.cpp 构造函数）。
#[test]
fn plugin_nodes_register_with_translated_inputs() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}

		let entries = Factory::global().dynamic_entries();
		assert!(
			entries.iter().any(|m| m.type_id == PLUGIN_ID),
			"CPU 变体应注册为动态节点"
		);
		assert!(
			entries.iter().any(|m| m.type_id == IDENTITY_ID),
			"identity 变体应注册为动态节点"
		);
		let meta = entries.iter().find(|m| m.type_id == PLUGIN_ID).unwrap();
		assert_eq!(meta.sub_category, "Filter");
		assert_eq!(
			meta.categories,
			vec![oak_node::node::Category::OpenFx]
		);

		let (core, behavior) = Factory::global()
			.create_any(PLUGIN_ID)
			.expect("create_any 应建出插件节点");

		// gain：Double → Float，默认 0.0，显示名 Gain，带 display
		// min/max 属性（-2/2）。
		let gain = core.get_input("gain").expect("gain 输入");
		assert_eq!(gain.value_type, ValueType::Float);
		assert_eq!(gain.default, NodeValue::Float(0.0));
		assert_eq!(gain.display_name, "Gain");
		// min/max/tooltip 属性 C++ 只对颜色输入设置
		// （plugin.cpp:407-430 的 k_color 分支）；Double 参数无。
		assert!(gain
			.properties
			.iter()
			.all(|(k, _)| k != "min" && k != "max"));

		// mode：Choice → Combo，两个选项 Fast/High。
		let mode = core.get_input("mode").expect("mode 输入");
		assert_eq!(mode.value_type, ValueType::Combo);
		let options: Vec<String> = mode
			.properties
			.iter()
			.filter(|(k, _)| k == "combo_option")
			.map(|(_, v)| match v {
				NodeValue::Text(s) => s.clone(),
				_ => panic!("combo_option 应是 Text"),
			})
			.collect();
		assert_eq!(options, vec!["Fast".to_string(), "High".to_string()]);

		// debug：secret → hidden。
		let debug = core.get_input("debug").expect("debug 输入");
		assert!(
			debug.flags & oak_node::input::flags::HIDDEN != 0,
			"secret 参数应隐藏"
		);

		// label：String → Text。
		let label = core.get_input("label").expect("label 输入");
		assert_eq!(label.value_type, ValueType::Text);

		// Source clip → 纹理输入；effect_input 选中 Source。
		let source = core.get_input("Source").expect("Source 输入");
		assert_eq!(source.value_type, ValueType::Texture);
		assert_eq!(core.effect_input, "Source");

		// 行为是持真实实例句柄的 PluginNode。
		let plugin = behavior
			.as_any()
			.and_then(|a| a.downcast_ref::<oak_node::nodes::plugin::PluginNode>())
			.expect("行为应是 PluginNode");
		assert!(!plugin.instance_handle().is_null());

		Host::global().shutdown();
	});
}

/// CPU 端到端：常量源 → 插件节点（render 填常量 0.5/alpha 1）→
/// 输出帧像素断言。
#[test]
fn plugin_renders_constant_frame_end_to_end() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let (core, behavior) = Factory::global()
			.create_any(PLUGIN_ID)
			.expect("create_any");

		let mut graph = Graph::new();
		let src_id = graph.add_node(
			NodeCore::new(),
			Box::new(ConstSource {
				rgba: [0.2, 0.4, 0.6, 1.0],
				size: (4, 4),
			}),
		);
		let plug_id = graph.add_node(core, behavior);
		graph
			.connect(src_id, plug_id, "Source", -1)
			.expect("Source 连接");

		let mut traverser = Traverser::new();
		let mut hooks = oak_render::eval::RenderEvalHooks::new();
		let table = traverser
			.evaluate(
				&graph,
				&EvalRequest::new(plug_id, Rational::new(0, 1)),
				&mut hooks,
			)
			.expect("evaluate 应成功");

		let texture = rendered_texture(&table);
		assert_eq!(texture.size(), (4, 4));
		// 测试插件 render 无视输入，填常量 0.5（alpha=1）。
		assert_eq!(first_pixel(&texture), [0.5, 0.5, 0.5, 1.0]);

		Host::global().shutdown();
	});
}

/// isIdentity 透传：identity 变体声明恒透传 Source → 输出应等于
/// 输入帧（render_driver 的 passthrough 短路）。
#[test]
fn identity_variant_passes_source_through() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let (core, behavior) = Factory::global()
			.create_any(IDENTITY_ID)
			.expect("create_any(identity)");

		let mut graph = Graph::new();
		let src_id = graph.add_node(
			NodeCore::new(),
			Box::new(ConstSource {
				rgba: [0.25, 0.75, 0.5, 1.0],
				size: (2, 2),
			}),
		);
		let plug_id = graph.add_node(core, behavior);
		graph
			.connect(src_id, plug_id, "Source", -1)
			.expect("Source 连接");

		let mut traverser = Traverser::new();
		let mut hooks = oak_render::eval::RenderEvalHooks::new();
		let table = traverser
			.evaluate(
				&graph,
				&EvalRequest::new(plug_id, Rational::new(0, 1)),
				&mut hooks,
			)
			.expect("evaluate 应成功");

		let texture = rendered_texture(&table);
		assert_eq!(first_pixel(&texture), [0.25, 0.75, 0.5, 1.0]);

		Host::global().shutdown();
	});
}

/// 参数覆盖路径：Text/StrCombo 走 set_ofx，数值走 POD——经 set 后
/// 实例参数值可读回（翻译注入的回归保护）。
#[test]
fn param_overrides_reach_instance() {
	common::with_host(|| {
		if !scan_and_register() {
			return;
		}
		let inst = Host::global()
			.create_instance(PLUGIN_ID, None)
			.expect("实例");
		let id = oak_plugin::node_factory::register_instance(inst.clone());

		// 数值覆盖（gain = 1.25）。
		let job_values = vec![(
			"gain".to_string(),
			oak_node::value::NodeValue::Float(1.25),
		)];
		let pod: Vec<(String, oak_plugin::node::Value)> = job_values
			.iter()
			.filter_map(|(k, v)| {
				oak_plugin::node::Value::from_node_value(v).map(|p| (k.clone(), p))
			})
			.collect();
		let dst = oak_render::eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32)
			.unwrap();
		let src = oak_render::eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32)
			.unwrap();
		let job = oak_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: pod,
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		oak_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("render_frame 应成功");
		let gain = inst.value.params.find("gain").unwrap().get();
		assert_eq!(
			gain,
			oak_plugin::param::ParamValue::Double([1.25, 0.0, 0.0], 1)
		);

		// NaN 覆盖回退默认（gain 默认 0.0）。
		let pod_nan = vec![(
			"gain".to_string(),
			oak_plugin::node::Value::float(f64::NAN),
		)];
		let dst = oak_render::eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32)
			.unwrap();
		let src = oak_render::eval::generate_frame(Rational::new(0, 1), (2, 2), PixelFormat::F32)
			.unwrap();
		let job = oak_plugin::render_driver::RenderJob {
			time: 0.0,
			dst: Texture::wrap_frame(dst),
			src: Some(Texture::wrap_frame(src)),
			effect_input_id: Some("Source".into()),
			inputs: Vec::new(),
			values: pod_nan,
			renderer: None,
			clear_destination: false,
			interactive: false,
		};
		oak_plugin::render_driver::render_frame(&inst.value, &job)
			.expect("NaN 覆盖不应失败");
		assert_eq!(
			inst.value.params.find("gain").unwrap().get(),
			oak_plugin::param::ParamValue::Double([0.0, 0.0, 0.0], 1)
		);

		oak_plugin::node_factory::unregister_instance(id);
		Host::global().shutdown();
	});
}
