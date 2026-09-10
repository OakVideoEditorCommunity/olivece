# 文本素材化与结构化文本编辑器改造计划

> 面向实现者的任务书（2026-09-10）。本文只描述方案与工作项，不含已执行的代码修改。
> 提出背景（用户原话）："不能让用户手工输入 HTML；输入框输入不了内容（已修复，见 §0）；
> 应该让用户手工编辑文本、手工设置字体、字号、位置、字体颜色、轮廓、发光；文字应该是一个
> 单独的素材而不是一个特效——文字作为 clip 被拖动到时间轴上，而不是作为特效被拖动到检查器；
> 文字不应该被放在特效那里，应该在项目的'新建序列'旁边添加一个'添加文本素材'。"

## 0. 已先行修复（不在本计划范围）

- 输入框无法输入：参数视图每个引擎 tick 都把引擎值重刷进输入框，击键下一帧即被清掉。
  已改为聚焦期间跳过重同步（`crates/oak-app/src/panels/ofx_params.rs` sync_values 的
  Text 分支，与曲线编辑器拖拽保护同款），含回归测试
  `text_field_keeps_in_progress_edits_while_focused`。**已提交**（`5f8db8e31`）。

## 1. 现状

### 1.1 节点层

- `crates/oak-node/src/nodes/textv3.rs`（type id `org.olivevideoeditor.Olive.text3`）：
  当前"Text"特效。输入仅 `text_in`（**HTML 原文**，默认值是
  `<p style='font-size: 72pt; color: white;'>Sample Text</p>`）、`valign_in`、
  `use_args_in`、`args_in`，外加 ShapeNodeBase 继承的 `pos_in`/`size_in`/`color_in`。
  字体、字号、颜色全部编码在 HTML 里；轮廓、发光**根本不存在**。
  `create()` 设 `VIDEO_EFFECT` 标志 → 出现在特效库/检查器"添加特效"菜单
  （`crates/oak-app/src/oakui/effectchain.rs::addable_effects`，内置特效取
  `VIDEO_EFFECT && !DONT_SHOW_IN_CREATE_MENU`）。
- `textv1.rs`（`textgenerator`）：旧版，已带 `DONT_SHOW_IN_CREATE_MENU`。
- `textbackend.rs`：**只有 hook 层**。`TextLayoutRequest{text, mode, font_family,
  font_size_pt, dots_per_meter, wrap_width, center_horizontally}` +
  `set_text_backends(measure, render)` 两个函数指针。**没有任何已安装的文本引擎**，
  即当前 text3 根本无法真正出字（后端决策被刻意推迟到 facade 层，见模块文档）。
- 字体引擎可用性：workspace lockfile 已有 `cosmic-text 0.19.0`（gpui 文本系统在用）与
  `swash`。无需新增重量级依赖即可实现后端；GPU 侧轮廓/发光可作为覆盖率纹理的后处理
  pass 实现，不走字体引擎。

### 1.2 素材/时间轴层

- bin 条目 = 项目根文件夹 `FolderBehavior.children`（`crates/oak-app/src/oakui/projectbrowser.rs`，
  `roots()/children()`，条目 id = 节点 identity，名称 = `core.label`）。
- 时间轴接受 bin 拖放：footage 走 `engine.drop_footage_at` →
  `graphops::place_footage_clip`（footage 节点连到 clip `tex_in`）。**没有**非 footage
  条目的拖放路径。
- clip 由生成器喂入的机制已存在：clip 的 `tex_in` 可以接任意节点输出（多机位、
  效果链都是这么接的，见 `effectchain::insert` 的连线模式）。
- 项目面板"新建序列"按钮：`crates/oak-app/src/panels/project_explorer.rs:246`，
  emit `NewSequenceRequested` → app.rs 订阅处理。

### 1.3 渲染层

- text3 的 `value()` 产出 shader job（`ShaderJobPayload`），渲染端
  `process_shader_job` 编译执行。文本栅格化在节点侧经 textbackend 完成
  （当前无后端 → 空）。轮廓/发光若做 GPU 后处理，可复用同一个 job 管线
  （多级 `iterations` 或嵌套 payload 都已支持）。

## 2. 目标

1. 用户永远看不到、也输不了 HTML。文本内容、字体、字号、位置、颜色、轮廓、发光
   全部是结构化字段。
2. 文字是**素材**：项目面板"新建序列"旁出现"添加文本素材"，创建后出现在 bin 里，
   可拖到时间轴成为 clip；选中文本 clip 在检查器里编辑上述字段。
3. 特效库不再出现 text3（迁移完成后隐藏；迁移期不破坏旧项目）。

## 3. 方案

### 3.1 textv3 节点结构化输入（节点层）

给 `textv3.rs` 增加输入（保留 `text_in` 作内部合成载体与旧项目兼容）：

| 新输入 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `plain_text_in` | Text | `"文本"` | 纯文本内容（多行） |
| `font_family_in` | Combo(StrCombo) | 空=后端默认 | 字体族（选项由后端枚举注入；手输亦可） |
| `font_size_in` | Float | 72.0 | 字号 pt，min 1 |
| `text_color_in` | Color | (1,1,1,1) | 字体颜色（替代 HTML color） |
| `outline_enabled_in` | Boolean | false | 轮廓开关 |
| `outline_color_in` | Color | (0,0,0,1) | 轮廓颜色 |
| `outline_width_in` | Float | 2.0 | 轮廓宽度 px，min 0 |
| `glow_enabled_in` | Boolean | false | 发光开关 |
| `glow_color_in` | Color | (1,1,0,1) | 发光颜色 |
| `glow_radius_in` | Float | 8.0 | 发光半径 px，min 0 |

- `value()` 变更：不再把用户文本当 HTML。若装了后端，用 `TextLayoutMode::PlainText`
  + `font_family_in`/`font_size_in` 布局；颜色经 `color_in`（已有的 shape 基类输入，
  改名为 UI 上呈现为"字体颜色"还是保留 `color_in` 复用——**实现时选复用 `color_in`**，
  少一个冗余输入；`text_color_in` 不建）。轮廓/发光：
  - 首选 **GPU 后处理**：文本覆盖率纹理 → 轮廓 = 覆盖率膨胀(dilate)+底色垫底合成；
    发光 = 覆盖率高斯模糊+加色合成。两者都是现成模糊/合成 shader 的组合，
    作为 text3 `value()` 内的嵌套 payload 链（eval 已支持嵌套递归，深度上限 8）。
  - 轮廓膨胀/模糊模糊 kernel 复用 `blur.rs` 的 box blur 迭代模式即可（视觉可接受，
    避免新写高斯）。
- `text_in` 保留但改为**内部输入**（UI 隐藏，`input::flags::HIDDEN`）：
  旧项目文件里它是 HTML，载入时若 `plain_text_in` 为空而 `text_in` 非空，
  `Retranslate`/加载钩子里做一次 HTML→纯文本剥离（简单正则去标签即可，
  写 `strip_html_to_plain()` 单测覆盖）。
- `valign_in`/`use_args_in`/`args_in` 保留原样（已是 HIDDEN|STATIC 或正常输入）。

### 3.2 文本后端（cosmic-text）

- 新增 `crates/oak-app/src/oakui/textengine.rs`（app 层安装 hook，oak-node 不加依赖）：
  - 用 lockfile 已有的 `cosmic-text`（在 oak-app 的 Cargo.toml 提升为直接依赖，
    版本与 gpui 一致 0.19，避免双版本）。
  - 实现 `measure`/`render` 两个 `fn`，在 `RealEngine::create`（或 app 启动）
    调 `oak_node::nodes::textbackend::set_text_backends(Some(..), Some(..))`。
    （注意 textbackend 模块在 oak-node 是私有 mod 还是 pub——实现时若私有需改
    `pub mod textbackend`；hook 函数签名是 plain `fn`，跨 crate 直接传。）
  - `render` 输出 RGBA premultiplied（channel_count=4，白字默认色——节点侧
    `color_in` 着色在 shader 里做，与 v1/v3 的 C++ 语义一致）。
  - 字体枚举：`cosmic_text::fontdb` 系统字体库 → `font_family_in` 的 combo 选项
    由引擎 `effect_params` 组装时注入（`combo_option` 属性）。
- 风险：cosmic-text 的 CJK 字体回退（fontdb 自带 fallback 链，Linux 上
  Noto Sans CJK 通常可用）；多行/换行由 wrap_width + 文本含 `\n` 覆盖。

### 3.3 素材化（bin + 时间轴）

- **创建入口**：项目面板标题栏"新建序列"按钮旁加"添加文本素材"按钮
  （`project_explorer.rs` header，新 emit `NewTextFootageRequested`；app.rs 订阅）。
  行为：在项目根文件夹创建一个 text3 生成器节点（`core.label = "文本"`），
  作为 bin 条目出现。引擎方法 `AppEngine::create_text_footage(cx) -> Result<u64, String>`
  （real 实现：graphops 建节点 + 挂到 root folder children + undoable；
  mock 实现：记一条假条目）。
- **拖放到时间轴**：时间轴 drop 目前只认 footage。扩展 `drop_footage_at`
  （或新增 `drop_generator_at`）：若拖入的 bin 条目是 text3 节点，则
  `block_clip_create` + 把 text3 节点连到 clip `tex_in` + 放置到轨道
  （undoable，一条 undo）。clip 时长默认 5 秒（可拖长）。判定"条目是 text3"：
  `graphops` 按 identity 取节点比较 type_id。
- **bin 删除**：复用现有 `delete_entry`（从文件夹移除 + 断开图连接，已 undoable）。
  文本 clip 删除走现有 clip 删除路径。
- **检查器编辑**：选中文本 clip 时，检查器显示其 **生成器节点**的参数
  （现在选中 clip 显示效果链；对 generator-fed clip，链头即 text3——
  检查器需要一个小改动：当 clip 的 `tex_in` 直连一个 generator 节点时，
  把该生成器的参数也列出（或直接把选择路由到生成器节点）。
  **实现时确定**：倾向"generator 节点作为链的第一张卡展示"——
  `selected_effect_cards` 已经遍历 chain，chain() 目前把喂入节点（footage/
  生成器）都算作链尾一张卡（见 effectchain.rs chain() 的已知怪癖），
  text3 参数会自然出现；需要的是 text3 的参数在 build_control 下呈现为
  结构化字段（文本=多行输入、字体=combo、颜色=颜色选择器、数值=spin）。
- **特效库隐藏 text3**：`textv3.rs::create()` 的 flags 增加
  `DONT_SHOW_IN_CREATE_MENU`。旧项目里已存在的 text3 特效链节点**不受影响**
  （只是不能再新增）。此项放在最后做，确认素材路径可用后再隐藏。

### 3.4 UI 文案（i18n）

8 个语言文件新增：`project.add_text_footage`（添加文本素材）、
`text.font_family`（字体）、`text.font_size`（字号）、`text.outline`（轮廓）、
`text.outline_width`（轮廓宽度）、`text.glow`（发光）、`text.glow_radius`（发光半径）、
`text.content`（文本内容）。zh-CN/en-US 翻译，其余语言给英文。

## 4. 工作项（可分配给子代理的最小单元）

1. **W1 节点输入扩展**：textv3 新输入 + `value()` 纯文本路径 + `text_in` 隐藏 +
   HTML 剥离迁移 + 节点单测（输入存在/默认值/隐藏标志/纯文本 job 参数）。
2. **W2 轮廓/发光 GPU 后处理**：text3 `value()` 嵌套 payload（膨胀/模糊/合成），
   eval.rs GPU 像素测试（白字黑轮廓边缘检测、发光半径扩散检测）。
3. **W3 cosmic-text 后端**：textengine.rs + hook 安装 + 字体枚举注入 +
   集成测试（装后端后 text3 渲染出非空纹理，GPU 测试）。
4. **W4 素材创建+拖放**：面板按钮 + `create_text_footage` + drop 扩展 +
   i18n + app 层测试（mock：按钮 emit；real：创建后 bin 有条目、拖放后轨道有 clip
   且 tex_in 连到 text3）。
5. **W5 检查器结构化呈现**：确认 text3 参数以结构化字段出现在文本 clip 的检查器
   （含聚焦保护已修的多行文本输入）；颜色走 OfxColorPicker。
6. **W6 特效库隐藏 + 收尾**：DONT_SHOW_IN_CREATE_MENU、全量测试、文档更新
   （docs/zh 如有特效清单）。

依赖顺序：W1→W2/W3（可并行）→W4→W5→W6。W2 与 W3 独立。
建议 W1+W2 一个子代理、W3 一个、W4+W5 一个、W6 收尾由主代理审查后执行。

## 5. 验收标准

1. 项目面板点"添加文本素材"→ bin 出现"文本"条目；拖到时间轴 → 出现文本 clip。
2. 选中文本 clip，检查器可编辑：内容（多行）、字体、字号、位置、颜色、轮廓
   （开关/颜色/宽度）、发光（开关/颜色/半径）；全程无 HTML 可见。
3. 编辑任一字段，暂停的画面立即更新（依赖已提交的暂停刷新修复）。
4. 特效库/添加特效菜单中不再出现 Text/text3。
5. 含旧 text3 特效的项目能打开、能渲染（HTML 自动剥成纯文本进 `plain_text_in`）。
6. `cargo test --workspace` 全绿，新增 GPU 测试在无 GPU 环境跳过。
