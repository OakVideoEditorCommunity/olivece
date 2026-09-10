# OpenFX-Misc 洁净室 GPU 重写（内置特效扩充）与特效分类/折叠计划

> 面向实现者的任务书（2026-09-10）。本文只描述方案与工作项，不含已执行的代码修改。
> 用户要求："把 OpenFX-Misc 洁净室重写为 GPU 版本，作为内置特效加入进去，并给内置特效
> 加分类和折叠分类的功能。"参考源码已克隆到本机 `/tmp/ofx-misc`（上游
> `github.com/cgvirus/OpenFX-Misc`，GPL2，86 个插件目录，README 有完整清单）。
>
> 法律/工程边界：**洁净室**指不复制其代码——我们只读其算法描述与参数语义，
> GLSL 与节点代码全部自写。仓内节点实现模式已有 60+ 先例（oak-node/src/nodes/*.rs），
> 本计划实质是"参照 OpenFX-Misc 的特效清单，按仓内既有节点模式补齐内置特效"。

## 1. 现状

### 1.1 节点/渲染管线（完全够用）

- 内置特效 = `oak-node/src/nodes/*.rs` 的 `NodeBehavior` 实现：声明输入（`Input`），
  `value()` 推 `ShaderJobPayload`（`crates/oak-node/src/jobs.rs`），`shader_code()`
  返回 GLSL 片段。渲染端 `crates/oak-render/src/eval.rs::process_shader_job`：
  编译（naga→WGSL，**不支持 GLSL switch**——新 shader 一律 if/else，教训见提交
  `37df1d3d3`）、按名绑定全部纹理参数、嵌套 payload 递归（深度上限 8）、
  `resolution_in` 自动锚定序列分辨率（提交 `fc9060424`）、`iterations` 多轮 +
  `previous_iteration_in` 反馈。GPU 像素测试模式：`eval.rs tests::eval_node_row`。
- 坐标/基准约定：像素空间以**画面中心**为原点（transform 语义，提交 `d028a45ff`）；
  像素尺寸参数（半径/宽度/距离）按序列分辨率解释。
- 已有同类特效（避免重复）：blur、opacity、transform、crop、flip（Distort 系）、
  merge、mrg（生成器 alpha-over）、math、chromakey、colordifferencekey、despill、
  solid、polygon、shape、noise、ociobase/lut/grading、whitebalance、threewaycolor、
  mask、stroke、dropshadow、displaytransform、cornerpin（假实现，另案）、
  tile/swirl/ripple/wave（Distort 系）、trigonometry、volume、pan。

### 1.2 特效库 UI

- `crates/oak-app/src/oakui/effectchain.rs::addable_effects`：内置（`group: None`）
  + OFX 动态条目（`group: Some(子类)`），排序已按组+名字。
- `crates/oak-app/src/panels/effect_library.rs`：渲染时组头已存在
  （`group_header()`，内置统一一个 "Built-in" 头），**不可折叠**；有搜索框。
- 检查器"添加特效"菜单（`panels/inspector.rs:157`）吃同一张 `addable_effects` 表。

## 2. 目标

1. 参照 OpenFX-Misc 清单，按 GPU 版本洁净室重写一批常用特效，作为**内置特效**
   （oak-node 原生节点，非 OFX 运行时）加入。
2. 内置特效按功能分类（Color / Filter / Keying / Distort / Generator / Merge / Time），
   特效库与检查器添加菜单都按分类分组，**分类可折叠**（折叠状态持久化）。

## 3. 特效分批（实现范围）

### Tier 1（本批必做，算法简单、GLSL 直译，全部像素可测）

| 特效（参考） | 分类 | 输入（节点参数） | 算法要点 |
|---|---|---|---|
| ColorCorrectOFX | Color | saturation/contrast/gamma/gain/offset（各 5 组：master/shadows/midtones/highlights）太多了→**简化为全局 5 参数**（saturation/contrast/gamma/gain/offset） | 逐像素 `offset+gain*pow(x,gamma)`，contrast 绕 0.18 灰，saturation 绕 luma |
| GammaOFX | Color | gamma（单值） | `pow(x, 1/g)` |
| SaturationOFX | Color | saturation | luma 插值（与 whitebalance/threeway 不重复：它最简） |
| InvertOFX | Color | channel 开关(RGBA) | `1-x`（按通道掩码） |
| ClampOFX | Color | min/max | clamp 每通道 |
| ColorMatrixOFX | Color | 4x4 矩阵（16 float） | 矩阵×RGBA（uniform mat4 已有先例：transform_in） |
| GradeOFX | Color | blackPoint/whitePoint/blackOut/whiteOut/gamma | 黑白点重映射 |
| DirBlurOFX | Filter | amount/angle | 方向模糊（迭代采样 N=16，角度→方向向量） |
| SharpenCImg | Filter | amount | unsharp mask：x + amount·(x − blur(x))（blur 复用现有迭代模糊，嵌套 payload） |
| EdgeDetectCImg | Filter | threshold/通道 | Sobel 幅值 |
| Dilate/ErodeCImg | Filter | radius/shape(rect) | 3×3~7×7 结构元 max/min（radius 控制迭代轮数） |
| DissolveOFX | Merge | mix(0..1)、第二输入 blend_in | 加权平均（merge.rs 双输入绑定已有先例；转场功能的原子件） |
| KeyMixOFX | Merge | mask_in、blend_in | 按 mask 拷贝（mask 绑定已有先例：chromakey 的 garbage/core matte） |
| PreMult/UnpremultOFX | Merge | channel 选择 | rgb *= a / rgb /= a（0 保护） |
| PositionOFX | Distort | offset xy（整数 px） | 采样偏移（resolution_in 换算） |
| MirrorOFX | Distort | horizontal/vertical | 翻转采样（flip 节点已有？若有重复则跳过——实现时先查 flip.rs 覆盖面） |
| CheckerBoardOFX | Generator | size/color1/color2 | 程序化棋盘格 |
| ColorBarsOFX | Generator | SMPTE/100%/75% | 彩条（分段填色） |
| RampOFX | Generator | point0/point1/color0/color1 | 线性渐变 |
| Rand（噪声已有） | — | — | **跳过**（noise.rs 已覆盖） |
| Constant（solid 已有） | — | — | **跳过** |

合计约 17 个新节点（Mirror 可能合并/跳过）。

### Tier 2（第二批，涉及曲线/对数/卷积/积雨云）

HSVTool（色相替换+keyer 能力）、Quantize（海报化/抖动）、Log2Lin/PLogLin、
ClipTest（斑马纹超范围指示）、Matrix3x3/Matrix5x5（通用卷积）、GodRays（径向
辉光，迭代采样）、ColorLookup（分通道曲线——**复用现有曲线编辑器**
`gpui_widgets::curve_editor` + `oak_plugin::param_curve` 的 JSON 模型，参数为 Text）。

### Tier 3（明确不做，写明理由）

- Roto（要主机遮罩编辑）、TrackerPM（点跟踪，需交互与多帧）、Card3D（3D 投影）、
  STMap/IDistort（位移图输入——其实可做，列 Tier 2 备选）、Shadertoy（沙盒运行时）、
  全部 Views/立体声（无多视图管线）、CImg 重型族（DenoiseSharpen/Smooth* PDE/Inpaint——
  迭代 PDE 不适合实时 GPU 预览）、**全部时间域**（FrameBlend/FrameHold/Retime/
  TimeBlur/SlitScan/TimeOffset/AppendClip——`ShaderJobPayload` 只能采当前时刻纹理，
  多时刻采样需要 job 管线扩展，**单独立案**，不在本计划）。

## 4. 节点实现模板（所有新节点统一）

每个新节点 = `oak-node/src/nodes/` 一个文件，遵循既有模式（参照 `opacity.rs` /
`colorcorrect` 无、参照 `blur.rs`/`math.rs`）：

1. 常量输入 id + `create()`（输入、默认值、min/max、combo 字符串、`VIDEO_EFFECT` 标志、
   `core.effect_input = "tex_in"`；双输入节点参考 merge.rs 的 base/blend）。
2. `value()`：无纹理直通（参考各节点的 `// CPP-PARITY` 注释体例），否则推
   `ShaderJobPayload`（`shader_id: ""`，`iterations: 1`）。
3. `shader_code()`：GLSL 片段（ove_texcoord/frag_color；**禁用 switch**；
   像素尺寸参数用 `resolution_in`；采样偏移用中心原点像素空间与否按特效语义——
   颜色类与坐标无关，几何类参照 transform 的中心原点）。
4. `register()` 进 `nodes/mod.rs` 的注册表。
5. 单元测试（输入默认值/隐藏标志/job 参数）+ **`crates/oak-render/src/eval.rs`
   GPU 像素测试**（eval_node_row 模式，无 GPU 自动跳过）。颜色类用纯色输入断言
   输出值；几何/模糊类用点/块图案断言位移/扩散。

## 5. 分类与折叠（UI）

1. **内置特效分类**：`addable_effects()` 的内置分支改为 `group: Some(分类)`，
   分类取自节点 `categories()` 首个 `Category` 映射：
   `Category::Color→"调色"`（或英文 "Color"，跟 i18n key）、`Filter→"滤镜"`、
   `Distort→"扭曲"`、`Keyer→"键控"`、`Generator→"生成器"`、`Merge→"合成"`、
   `Time→"时间"`、`Math→"数学"`、`Channel→"通道"`。映射函数放
   `effectchain.rs`（`node_category_key` 已有类似物，见 engine.rs:206，
   但该函数是给节点编辑器菜单的 i18n key，特效库分组可直接复用同一 key 体系）。
   i18n：8 语言加 `effect_library.group.<key>`。
2. **折叠**：`effect_library.rs` 组头加点击折叠/展开（箭头 ▶/▼ + 组名）：
   - 面板 struct 增加 `collapsed: std::collections::HashSet<String>`（组 key），
     点击组头切换；渲染时折叠组跳过其子行。
   - 持久化：`oak_core::configstore`（参照现有 `UseProxyMedia` 等键的读写模式），
     键 `EffectLibraryCollapsed`（逗号分隔组 key 列表）。
   - 检查器的添加菜单（inspector.rs:157 的菜单构建）同样按组分组
     （menu.rs 支持子菜单——组做子菜单，比折叠更适合菜单形态；实现时确认
     `MenuItem::with_submenu` 用法，与 proxy_submenu 一致）。
3. 搜索时忽略折叠状态（搜索命中强制展开显示，已在循环内自然满足：
   搜索非空时不跳过子行）。

## 6. 工作项（可分配给子代理的最小单元）

- **W1 Tier1 颜色组（6 节点）**：ColorCorrect/Gamma/Saturation/Invert/Clamp/Grade。
- **W2 Tier1 矩阵+卷积组（4 节点）**：ColorMatrix/EdgeDetect/Dilate/Erode
  （+Tier2 的 Matrix3x3/5x5 若顺利一并）。
- **W3 Tier1 模糊/锐化组（2 节点）**：DirBlur/Sharpen。
- **W4 Tier1 合成组（4 节点）**：Dissolve/KeyMix/PreMult/Unpremult。
- **W5 Tier1 几何+生成器组（4~5 节点）**：Position/Mirror(或跳过)/CheckerBoard/
  ColorBars/Ramp。
- **W6 分类与折叠 UI**：§5 全部（addable_effects 分组 + 特效库折叠 + 持久化 +
  检查器子菜单 + i18n）。
- **W7 Tier2 批**：HSVTool/Quantize/Log2Lin/ClipTest/ColorLookup/GodRays
  （W1-W6 完成并审查后再派）。

W1-W5 互相独立（不同文件），可并行派 5 个子代理；W6 独立；每个子代理须交付：
节点实现 + 单元测试 + GPU 像素测试 + `cargo test -p oak-node -p oak-render` 绿。
**统一禁令**：GLSL 不写 switch；不动 eval.rs/traverser 等管线文件（冲突根）；
遵循 nodes/ 既有文件体例（GPL 头、CPP-PARITY 注释、输入常量文档）。

## 7. 验收标准

1. Tier1 全部节点出现在特效库对应分类下，可加到 clip，画面效果正确（GPU 测试
   逐节点覆盖核心算法）。
2. 特效库分类可折叠，重启 app 折叠状态保留；检查器添加菜单按分类分组。
3. 搜索框在任何折叠状态下都能搜到特效。
4. `cargo test --workspace` 全绿。
