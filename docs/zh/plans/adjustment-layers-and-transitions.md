# 调整图层与转场特效改造计划

> 面向实现者的任务书（2026-09-10）。本文只描述方案与工作项，不含已执行的代码修改。
> 用户要求："增加一项新功能：调整图层。可以将特效应用到多个 clip 那种。增加交叉叠化
> 等几种常见的转场特效。我觉得转场特效可以做成应用在调整图层上的节点。注意转场开始
> 和结束点，不能覆盖整个 clip，这要求调整图层也能针对 clip 的一段设置。"
>
> 配套调研：调整图层/转场基建审计（2026-09-10 会话，结论已并入本文各"现状"节）。

## 1. 需求分解

1. **调整图层（Adjustment Layer）**：时间轴上的一种块，位于某条视频轨、占一段时间
   范围，其效果链作用于**它下方所有视频轨在该时刻的合成结果**。一段范围可覆盖
   多个 clip（"将特效应用到多个 clip"），也可只覆盖一个 clip 的一段（"针对 clip
   的一段设置"）。
2. **转场特效**：交叉叠化（Cross Dissolve）等若干常见转场，有明确的**开始和结束
   点**（不能覆盖整个 clip）。
3. 用户倾向的实现形态：转场做成**应用在调整图层上的节点**（"我觉得转场特效可以
   做成应用在调整图层上的节点"）——本文 §4 给出该形态的落地设计，同时指出其与
   一等转场块的关系。

## 2. 现状（关键事实，均带坐标）

### 2.1 转场块模型已存在，渲染/创建/UI 全空

- `crates/oak-node/src/block.rs:129-137`：`TransitionBlockBehavior { core: BlockCore,
  in_offset, out_offset }`，与普通块一样挂在 `TrackBehavior.blocks`。
  构造 `transition_create()`（block.rs:587-608）声明 `out_block_in`/`in_block_in`
  两个 NodeRef 输入（接前后块）。
- `value()` 是空实现（block.rs:501-510，注释明确"渲染成一个洞"）；
  `render_graph_frame`（`crates/oak-render/src/eval.rs:1466-1472`）与
  `video_montage`（`renderops.rs:270`）都只认 `ClipBlockBehavior`，转场块被跳过。
- 创建路径只有 OTIO 导入（`loadotio.rs:249-333`，含连线与 offset 设置）；
  OTIO/FCPXML 导出已支持（saveotio.rs:250-259、fcpxml.rs:596-600）。
- gpui 时间轴有转场楔形绘制与 `TransitionChanged` 事件（timeline/data.rs:122-135、
  clip.rs:346-377、timeline_view.rs:304-316），但 `RealClip` 不喂数据
  （`real.rs:873-897` 用默认 None）、`real.rs:4683` 明确丢弃该事件。
- `oak-timeline` 有 `TransitionRemoveCommand`（undogeneral.rs:543-632，edge/offset
  恢复未建模）。行动注册表有 `Tool::Transition`/`DefaultTransition`(Ctrl+Shift+D)
  但无后端（actions.rs:131/229/733/778-782）。

### 2.2 调整图层：零代码，但模型/渲染/撤销的接入点都已确认

- 块基类 `BlockCore`（block.rs:28-113）：range/media_in/speed/links/enabled/track。
  clip 的效果链挂载点 = `tex_in` 输入 + `core.effect_input`（block.rs:527-534），
  effectchain 的 insert/remove/move/set_enabled **与 clip 无关，任何有 effect
  input 的节点都能挂**。
- 渲染主路径 `render_graph_frame`（eval.rs:1409-1546）：逐 clip traverser 求值 →
  `composite_tracks` 自底向上 alpha-over。**调整图层的挂钩点 = eval.rs:1451-1478
  的轨道循环：遇到调整块时先把已收集的下层帧合成，跑调整块效果链，再往上。**
- 备选渲染路径 montage：`VideoTicketParams.montage`（ticket.rs:67-83 的
  `MontageClip{effects: Vec<MontageEffect>}`），由 `renderops.rs:249-309
  video_montage` 构建。**两条渲染路径语义必须一致**（worker 快照图渲染优先、
  montage 兜底：worker.rs:1253-1299、manager.rs:119-151），调整图层两边都要实现。
- 撤销：`graphops::place_footage_clip`（graphops.rs:1445-1506）=
  非撤销建节点 + 一条 undo 推 `TrackPlaceBlockCommand` + 连线；
  `TrackPlaceBlockCommand`（undopointer.rs:541-710）与块类型无关；
  graphops 的 clip 访问器（clip_ids/clip_range/trim_command 等）目前 downcast 到
  ClipBlockBehavior，需要推广到"任何带 BlockCore 的块"（参照
  `nodeops::block_core_of/set_block_core`，nodeops.rs:99-136 的模式）。
- "给效果链喂一张外来纹理"目前**没有**现成 API：链首效果的 `tex_in` 未连接时
  取不到值（traverser.rs:247-251）。需要 §4.2 的"纹理源"机制。
- UI：块绘制按 `ClipData.color/label` 即可区分外观（如需斜纹装饰才动
  gpui/crates/gpui/src/timeline/clip.rs）；移动/修剪/分割/删除对画成 RealClip 的
  新块**免费**（只要 §2.2 的 graphops 访问器推广到位）。空区右键菜单在
  panels/timeline.rs:1268（`empty_area_menu`，知道轨道+帧）。

## 3. 设计 A：调整图层

### 3.1 节点模型

- 新行为 `AdjustmentBlockBehavior { core: BlockCore }`（oak-node/src/block.rs）+
  构造 `adjustment_create()`：只声明一个可连接、不可打关键帧的 `tex_in` 纹理输入，
  `core.effect_input = "tex_in"`；不需要 footage 链接与 media/speed/reverse 输入。
  在 `nodes/mod.rs` 注册表注册（type id `org.olivevideoeditor.Olive.adjustment`）。
- 范围 = `core.range`（与其他块完全同构）——**天然满足"针对 clip 的一段"**：
  范围想拉多长拉多长，可跨多个 clip，可只盖一个 clip 的中段。
- 效果链：与 clip 完全同构（effectchain 不变）。检查器选中调整块时，
  `selected_clip_node` 等 graphops 访问器推广到"任何带 BlockCore 的块"
  （详见 §3.4），效果栈/参数 UI 原样工作。
- 序列化：`save_custom`/`load_custom` 走 block.rs:165-186 的扩展机制
  （旧读者跳过新元素，docs/project-storage.md 已为此预留）；OTIO/FCPXML 导出
  第一版跳过调整块（记 TODO；OTIO 无调整图层概念，需要时映射为带效果的 Gap）。

### 3.2 渲染：图路径（render_graph_frame）

在 eval.rs 的轨道循环（1451-1478）中：

```
对每条视频轨（顶层→底层顺序）:
    若该轨在 time 有启用的调整块:
        1. 把已收集的下层 frames 先经 composite_tracks 合成出"下层合成图"
        2. 用 §3.3 的机制对下层合成图跑调整块的效果链 → 一张纹理
        3. 把该纹理作为唯一一帧压入 frames，继续向上收集
    否则: 照常收集该轨覆盖 time 的 clip
最后: composite_tracks(frames)
```

- 多个调整图层自然嵌套（每条含调整块的轨道触发一次上述 flush）。
- 中间合成用现有 GPU 合成 helper（composite_tracks 的 GPU 变体），避免回读；
  调整链的输出再作为后续合成的输入纹理。
- 调整块自己的 `value()`：直通 `tex_in`（与 clip 的直通同形）；
  真正喂纹理由渲染驱动完成（§3.3）。

### 3.3 渲染：给效果链喂"下层合成图"（纹理源）

新增轻量"纹理源"节点（oak-node，`org.olivevideoeditor.Olive.composite_source`）：
`value()` 把渲染驱动预置的一张纹理推进输出表（形状仿 footage 节点的
FootageJobPayload，但零开销直接装箱 `NodeValue::Texture`）。渲染驱动在每次
flush 时：

1. 图内临时建（或复用一个每帧更新的）纹理源节点，预置下层合成图；
2. 把它连到调整链首效果的 `tex_in`（临时边，求值后拆除，或每帧重连）；
3. `traverser.evaluate(EvalRequest::new(调整块, time))` —— 效果链、嵌套
   shader job、OFX 插件全部照常工作（无需任何特殊分支）。

（调研的备选方案——在 hooks 里替换链首 payload 的 tex_in 参数——魔法且脆，
不采用；montage 机制（§3.4）覆盖不了内置 shader 特效，也不够。）

### 3.4 渲染：montage 路径（必须与图路径语义一致）

- `VideoTicketParams` 增加
  `adjustments: Vec<AdjustmentSpan { in_time, out_time, track_index, effects:
  Vec<MontageEffect> }>`；`video_montage` 识别调整块并用现成的 `clip_effects()`
  （renderops.rs:222-241，对任意宿主块可用）构建其效果快照。
- `render_montage_frame_into` 在按轨道累积合成时，越过 `track_index` 边界就把
  对应 span 的 effects 用现有 `apply_clip_effects/apply_montage_effect`
  （eval.rs:1901-1999）应用到累积图上。
- **已知局限（写进 TODO）**：montage 的效果执行目前只真支持 Opacity + OFX 插件，
  内置 shader 特效会 pass-through（既有问题，非本计划新增）。图路径（worker
  快照优先）始终正确；montage 兜底路径的保真度随 montage 效果执行器的
  补齐而提升——把"montage 效果执行器支持内置 shader 特效"列为后续独立工作项，
  不在本计划范围。

### 3.5 创建/编辑/UI/撤销

- 创建：时间轴空区右键菜单（panels/timeline.rs:1268 `empty_area_menu`）加
  "添加调整图层"：以命中轨道+帧为锚，`adjustment_create()` 建节点 + 一条 undo 推
  `TrackPlaceBlockCommand`（"Add Adjustment Layer"，默认 5 秒或贴近吸附）。
  i18n 8 语言加 `timeline.context.add_adjustment_layer`。
- 外观：RealClip 用区别于普通 clip 的颜色（引擎 `ClipData::color` 覆盖即可，
  不动 gpui）；label 默认"调整图层"。
- 交互：移动/修剪/分割/删除在 graphops 访问器推广后免费获得
  （clip_ids/clip_range/clip_track/trim_command/move_clip_to_track_commands/
  clip_block/delete_clip 全部改为认 BlockCore，而非只认 ClipBlockBehavior——
  参照 nodeops::block_core_of/set_block_core 的多 downcast 模式）。
- 撤销：放置一条 undo（§2.2 模板）；效果编辑走 effectchain 既有 undo；
  删除用 delete_clip 同款（TrackReplaceBlockWithGapCommand + remove_node_command）。

## 4. 设计 B：转场特效

### 4.1 转场块一等化（基础件）

转场块（TransitionBlockBehavior，§2.1）满足"开始和结束点不覆盖整个 clip"——
它自身就是时间轴上一段独立范围的块，骑在两个 clip 的接缝上（in_offset 伸入
前块尾部、out_offset 伸入后块头部）。

1. **创建**：两种方式都实现——
   a. 菜单/快捷键：`DefaultTransition`(Ctrl+Shift+D) 对选中 clip 的接缝插入
      默认转场（交叉叠化，默认 1 秒，可配置）；clip 右键菜单加"添加转场"。
      创建 = transition_create() + 连线前后块 + 设置 offsets + 放置，一条 undo
      （连线模式照 loadotio.rs:307-333）。
   b.（后续）拖 clip 边缘产生重叠自动建转场——**不做**，本计划只做显式创建。
2. **渲染（图路径）**：`render_graph_frame` 在逐 clip 求值阶段识别转场块覆盖
   的 time：对转场区间，同时求值前块与后块两张纹理，按转场类型混合后作为
   该区间输出（两输入混合的 shader 模式参照 merge.rs base/blend；混合 progress
   = (time − transition_in) / length）。**`TransitionBlockBehavior::value()`
   保持不被求值（渲染在合成驱动层完成）**，或按调研备选：把 value() 做成双输入
   shader job 的发出者（前后块纹理经 out_block_in/in_block_in 求值后注入）——
   实现时在两者中选改动更小者（倾向合成驱动层：与调整图层 flush 同层、共享
   中间合成设施）。
3. **渲染（montage 路径）**：montage clip 列表按时间天然把前后 clip 都列出；
   在 `render_montage_frame_into` 对转场区间解码两张并按类型混合
   （与 §3.4 同一累积器阶段）。
4. **转场类型（本批 4 种，均为内置 shader 节点实现，见 §4.2）**：
   交叉叠化（mix 渐变）、淡入淡出（对黑/对白，单边 dissolve）、
   划像（wipe，左/右/上/下方向+柔边）、推移（slide，推出/推入方向）。
5. **UI**：`RealClip` 喂 `ClipData::in_transition/out_transition`
   （查块的前后 TransitionBlockBehavior 邻居，换算楔形长度）；楔形拖拽
   `TransitionChanged` 在 real.rs:4683 落地（undoable 改 offsets，
   复用 transition_set_offsets_and_length 的 undo 模式）。
   转场块自身的移动/删除随 §3.5 的 graphops 推广免费获得；
   `TransitionRemoveCommand` 的 edge/offset 恢复缺陷（undogeneral.rs:580-583）
   顺手修掉（删除时记录并在 undo 恢复 offsets 与连线）。

### 4.2 转场 = 调整图层上的节点（用户提议的形态）

**输入问题的解法**：转场节点需要"另一幅画面"作第二输入。落地设计——

1. 转场节点（内置，`org.olivevideoeditor.Olive.transition.crossdissolve` 等，
   类别 `Category::Transition`）声明两个纹理输入：`tex_in`（链输入）与
   `blend_in`（第二输入，merge.rs 双输入绑定模式）。
2. 叠加关系：调整图层盖在接缝上（范围=转场区间），其下层合成图在区间内已经
   包含后块（底层轨上有后块时）——节点混合的是：
   - `tex_in` = 下层合成图（当前时刻）；
   - `blend_in` = 由图连接指定的参考源（默认：接缝前块的输出——创建转场时
     自动连好，节点编辑器里可见可改）。
   实际语义 = "下层画面 ⇄ 前块画面按 progress 混合"，足以表达交叉叠化/
   划像/推移；淡入淡出不需要 blend_in（直接对黑场混合）。
3. **progress 自动喂值**：渲染驱动在跑调整链时知道（layer_in, layer_out, time），
   把 `progress_in = (time − layer_in)/length` 像 `resolution_in` 一样自动填充
   （在 §3.3 的纹理源节点上顺带预置，或 hooks 增加 `layer_progress` 字段由
   process_shader_job 注入——实现时选改动小者，倾向后者：hooks 加
   `pub layer_progress: Option<f64>`，shader 声明 `uniform float progress_in`
   且 job 未显式提供时自动填入）。
4. 这样用户获得两种用法：
   - 简单接缝转场 → §4.1 转场块（一键、楔形可见）；
   - 自定义/复合转场 → 调整图层 + 转场节点（区间即调整块范围，
     "开始/结束点不覆盖整个 clip"由调整块 range 直接保证，节点编辑器里还能
     级联别的效果）。**两种用法渲染同一套 shader。**

### 4.3 验收语义（写进测试）

- 交叉叠化中点：两纯色输入输出各 50% 混合（GPU 像素测试）。
- 转场区间外：画面与无转场完全一致（前后各采一帧断言）。
- 调整图层跨 clip：下层两个不同色 clip，盖一个 Opacity=0.5 的调整层，
  区间两端的帧分别断言；区间外一帧断言不受影响。
- 淡入淡出首尾：progress=0 全黑/全白，progress=1 原图。

## 5. 工作项（可分配给子代理的最小单元）

1. **W1 调整块模型+graphops 推广**：AdjustmentBlockBehavior/adjustment_create/
   注册/序列化；graphops 与 real.rs 的 clip 访问器推广到 BlockCore；
   selected_clip_node 命中调整块。单测：创建/修剪/删除 undo。
2. **W2 调整图层渲染（图路径）**：eval.rs 轨道循环 flush + 纹理源节点 +
   hooks.layer_progress；GPU 测试（§4.3 的跨 clip 用例）。
3. **W3 调整图层渲染（montage 路径）**：VideoTicketParams.adjustments +
   video_montage 构建 + render_montage_frame_into 应用 + 测试。
4. **W4 调整图层 UI**：空区菜单创建、外观、i18n、检查器选中、app 层测试
   （mock+real）。
5. **W5 转场块一等化**：创建（DefaultTransition+右键菜单）、图路径+montage
   渲染（4 类型 shader）、楔形喂数据+TransitionChanged 落地、
   TransitionRemoveCommand 缺陷修复、GPU/集成测试。
6. **W6 转场节点（调整图层形态）**：transition.* 节点族（双输入+progress_in），
   自动连线（创建时把 blend_in 接到接缝前块）、节点编辑器可见性检查、
   GPU 测试（§4.3 中点/边界断言）。

依赖：W1→W2/W3→W4；W5 与 W1-W4 并行可做（只共享 §3.5 的 graphops 推广，
排在 W1 后）；W6 依赖 W2（layer_progress）与 W5（shader 共享）。
建议 W1+W2 一个子代理、W3 一个、W4 一个、W5 一个、W6 一个，主代理统一审查。
**冲突根禁令**：eval.rs 的 render_graph_frame 轨道循环只能由 W2 所有者改，
W3/W5 需要同区改动时通过评审串行合入。

## 6. 验收标准

1. 空区右键能加调整图层；上面加 Blur/Opacity 后，其下方所有视频轨在层覆盖
   范围内的画面整体受影响；范围外不受影响；范围可只盖一个 clip 的一段。
2. 选中接缝 Ctrl+Shift+D 插入 1 秒交叉叠化；播放/暂停/导出三种路径画面一致；
   楔形可见且可拖改长度；转场不覆盖整个 clip（区间外画面逐像素一致）。
3. 调整图层上加交叉叠化节点，blend_in 接到前块后呈现同样叠化；
   progress 沿区间线性推进。
4. 撤销/重做覆盖全部创建/编辑/删除路径。
5. `cargo test --workspace` 全绿；新增 GPU 测试在无 GPU 环境跳过。
