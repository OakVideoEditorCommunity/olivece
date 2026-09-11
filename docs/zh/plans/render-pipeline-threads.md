# 渲染管线改造：单解码线程 + 单渲染线程 + 队列流水线 + GPU 零拷贝

> 面向实现者的任务书（2026-09-11）。本文只描述方案与工作项，不含已执行的代码修改。
>
> 用户要求（原文要点）：
> 1. 解码一个单独的线程执行——线程，不是进程
> 2. 渲染一个单独的线程执行——同样线程
> 3. 主进程负责显示上屏
> 4. OpenFX 隔离在单独的进程，只有一个进程，崩了重新拉起来
> 5. 使用队列提交渲染任务
> 6. 流水线式作业：解码完成立刻进渲染队列，渲染完成进上屏队列
> 7. GPU 内存零拷贝——内置效果全是 GPU 效果，除非遇见 CPU OpenFX 特效，不回读；
>    哪怕为此要写平台特定代码（分支实现 CUDA/Vulkan/OpenGL/DirectX 通用）
> 8. 参考原版 Olive（github.com/olive-editor/olive）的渲染管线
> 9. 参考原版 Olive 重写 resolve：match + Job 枚举（crates/oak-node/src/jobs.rs）
>    单次循环完成，而不是四次扫描
>
> 配套调研：上游 Olive 已浅克隆到 `.cache/olive-upstream`（gitignore 内），本文
> 引用其文件时写作 `upstream/app/...`。

## 1. 背景：为什么改

当前渲染在**多进程**里跑（M15 进程隔离战役引入）：主进程经 NDJSON+shm 向 N 个
`oak-worker` 子进程派发 ticket，每个子进程单线程同步渲染。这个模型的前提（GPU
工作可以靠多进程并行、进程隔离保护一切）对**内置效果全 GPU 化之后的 Oak 不再
成立**：

- **GPU 是单队列**。wgpu 设备只有一个提交队列，N 个进程各自建 GPU 上下文并不能
  并行 GPU 工作，反而为每帧付出 GPU→CPU 回读 + shm 搬运 + 上屏前 CPU→GPU 重传
  的三次额外拷贝。
- **每个 worker 重复持有**解码器会话、OCIO 处理器缓存、着色器缓存与 GPU 上下文，
  内存与启动成本都按进程数翻倍。
- 进程间只能传 CPU 像素（shm 槽），GPU 纹理无法跨进程直接流动（除非上平台句柄
  外联，成本高于收益）。

线程模型则完全够用：崩溃隔离只需要覆盖**真正会崩的东西——第三方 OpenFX 插件**，
而插件恰好本就是 CPU 边界（绝大多数 OFX 插件读写 CPU 图像），天然适合单独进程。
内置 GPU 效果不会崩出进程外（wgpu 的错误模型是校验+设备丢失，不是段错误），
不需要进程隔离。

## 2. 现状（关键事实，均带坐标）

### 2.1 进程池渲染后端

- `crates/oak-render/src/procpool.rs:1-40`：TicketArena → ProcessDispatcher →
  N 个 WorkerHandle，stdio NDJSON 控制面 + shm FrameSlotPool 数据面；worker 崩溃
  后已认领帧重新入队并重生进程（有界重启）。
- `crates/oak-worker/src/worker.rs`：子进程是单线程 NDJSON 循环，
  `handle_render_batch_stream` 在循环线程上同步渲染整批 ticket——worker 内部
  **没有**任何渲染线程，并行只来自进程池。
- `crates/oak-render/src/scheduler.rs`：预览帧调度（交错分片认领）；自动缓存由
  `autocacher.rs` 驱动。

### 2.2 帧传输是 CPU/shm，显示前再上传 GPU

- `crates/oak-app/src/oakui/renderops.rs:550-555`：`RenderedFrame` 只有
  `CpuF32 { .. }` 与 `Shm(ShmFrameRef)` 两种——无论渲染在不在 GPU 上完成，
  到上屏路径的都是 CPU 像素，显示时再经 staging 上传（M15 S2 已确认播放路径
  主堆拷贝为 0，但 GPU↔CPU 的两次搬运仍在）。
- `crates/oak-core/src/texture.rs:183-202`：`Texture` 已有 `Gpu { token, backend,
  ctx, .. }` / `Cpu(Frame)` 双变体，GPU 纹理自持 `Arc<dyn GpuContextLike>`——
  **零拷贝改造的数据类型基础已经存在**，缺的是"全链路不把 GPU 纹理下载回来"
  的管线。

### 2.3 解码在 worker 内随叫随做，无流水线

- `crates/oak-render/src/eval.rs:988-990`：解码器会话本就进程级共享
  （按 (filename, stream) 键控、内部互斥），但解码动作发生在每个 worker 的
  ticket 处理里：一帧的"解码→渲染→回读"是串行的，帧与帧之间没有重叠。
- `render_footage_frame`（eval.rs，FootageJob 的落点）同步解码一帧并缩放到
  目标尺寸。

### 2.4 resolve 四次扫描表

- `crates/oak-render/src/eval.rs:919-930`：`RenderEvalHooks::resolve` 依次调用
  `resolve_plugin_jobs` → `resolve_footage_jobs` → `resolve_shader_jobs` →
  `resolve_color_transform_jobs`（eval.rs:406/480/520/561）。每个函数各自
  `for (_, value, _) in table.rows_mut()` 全表扫描，逐行用
  `handle::get_checked::<T>` 探测载荷类型——一张表被遍历 4 遍，每行最多被
  错误类型探测 3 次。
- `crates/oak-node/src/jobs.rs:35-40`：`Job` 枚举已存在（Footage/Shader/Plugin/
  ColorTransform），但**节点并没有把 Job 枚举塞进表里**——表里塞的是裸载荷
  box，靠 resolve 端逐类型猜测。注释自述"the graph-shaped job model lands in
  v0.6"（jobs.rs:31-33）。
- `CacheJob` 缺失：`process_video_cache_job`（eval.rs:391-399）直接返回
  "deferred" 错误，磁盘帧缓存加载没有 Job 表达。

### 2.5 OpenFX 在每个 worker 进程里渲染

- `crates/oak-worker/src/worker.rs:100-129`：OFX 渲染发生在 worker 进程内
  （注释自述"crash isolation"），进度经 `plugin_progress` NDJSON 事件回报，
  取消经主进程广播 `plugin_cancel`。N 个 worker = N 份插件实例宿主，插件崩溃
  杀掉的是某一个 worker（procpool 重生），语义正确但宿主数量与位置都不是
  设计出来的，是进程池的副产品。
- 插件失败时回退紫帧（eval.rs:386 `purple_frame`）的约定已存在。

### 2.6 原版 Olive 的参考点（upstream/app/...）

- `render/rendermanager.h:38-63`：`RenderThread`（QThread）内部一个
  `std::list<RenderTicketPtr> queue_` + `QWaitCondition`；RenderManager 持有
  **一个** video 线程、**一个** audio 线程、波形线程池、一个 dry-run 线程
  （rendermanager.cpp:123-150 按类型派发）。
- `render/renderprocessor.cpp:137` `Run()`：一个 ticket 的处理全流程
  （取参数 → GenerateTexture → 隔行处理 → 按需 GenerateFrame/写缓存 →
  Finish）。
- `node/traverser.cpp:351` `ResolveJobs`：**单函数 job 分发**——先递归
  resolve 子 job（job 的参数里嵌的 job），再按类型（CacheJob/
  ColorTransformJob/ShaderJob/GenerateJob/FootageJob）调对应 Process*，
  结果写回 value。这就是用户要求的"match + Job 枚举、单次循环"的范式。
- `render/job/*.h`：AcceleratedJob 基类 + 各 Job 的字段集（与我们 jobs.rs
  的载荷一一对应，可对照补全）。

## 3. 目标架构

```text
┌──────────────────────────── 主进程 ────────────────────────────┐
│                                                                │
│  UI 线程（gpui）          调度层（manager/scheduler/autocacher） │
│   │ 消费上屏队列            │ 把播放/导出/缓存请求编成 ticket      │
│   │                       │ 投到 decode/render 队列               │
│   ▼                       ▼                                     │
│ 上屏队列 ◄── present ── 渲染线程（唯一） ── render 队列 ◄──┐      │
│ （GPU 纹理）              │ 持有唯一 GpuContext             │      │
│                           │ 图求值 + 全部 GPU pass           │      │
│                           ▼                                 │      │
│                        decode 队列 ◄── 解码线程（唯一）─────────┘      │
│                                       │ 持有全部解码器会话            │
│                                       │ 解码→上传 GPU→投 render 队列  │
│                                                                │
│        ┌─── 仅当链上出现 CPU OpenFX 特效时 ───┐                 │
│        ▼                                      │                 │
│   OFX 主机进程（唯一，oak-ofx-host）            │                 │
│   NDJSON 控制面 + shm/（后续）GPU 句柄数据面     │                 │
│   崩溃→有界重生+在途 job 重投                    │                 │
└────────────────────────────────────────────────┘
```

### 3.1 三个执行角色

1. **解码线程（唯一）**：独占全部解码器会话（eval.rs:988 的进程级会话表
   顺理成章地归它所有——会话本就互斥，集中后连互斥都可以去掉）。从
   decode 队列取 FootageJob，解码、按需要缩放/色彩预变换、**上传为 GPU
   纹理**，把结果投到 render 队列。连续播放时由调度层预取（下一条 §3.4）。
2. **渲染线程（唯一）**：独占进程唯一 `GpuContext`（`GpuContext::shared()`
   之外的第二条获取路径收编到这里；texture.rs:178-181 的 Arc 自持模型
   不变）。从 render 队列取"图快照 + 时刻"作业，跑图求值与全部 GPU
   pass，产出 `Texture::Gpu` 投到上屏队列。
3. **主进程（上屏）**：UI 线程从上屏队列取 GPU 纹理直接显示（§3.5 的上屏
   互操作）。导出/磁盘缓存等 CPU 消费者在队列出口处显式回读——回读只
   发生在这些边界。

### 3.2 OpenFX 隔离进程（唯一）

- 新建 `oak-ofx-host` 可执行（或 oak-worker 的 `--ofx-host` 模式）：进程内
  加载全部 OFX 插件实例，渲染线程经 IPC 把 PluginJob 发给它，输入纹理
  回读成 CPU 帧经 shm 传入，输出经 shm 传回后再上传 GPU。**这是全链路唯一
  的计划内回读点**（仅当链上真有 CPU OFX 插件）。
- 崩溃处理复用 procpool 已验证的机制（procpool.rs:1-40：EOF/非零退出→
  在途帧重新入队、进程重生、有界重启），但池大小恒为 1；重复失败回退
  紫帧（沿用 eval.rs:386 约定）。
- `plugin_progress` / `plugin_cancel` 协议（worker.rs:100-129）原样搬运。
- GPU 型 OFX 插件（OpenGL/CUDA 上下文的）本期不支持直通，按 CPU 插件处理
  （文档明示；真有需要时按 §3.6 的平台分支再开 GPU 句柄通道）。

### 3.3 队列与 ticket

- 对上层（oak-app/oak-cli）**ticket API 不变**：RenderManager 仍是唯一入口，
  完成仍走 exactly-once 的 TicketPayload。
- 对内，ticket 变成队列项：三条 SPSC/MPSC 环（decode/render/present），
  复用 `crates/oak-render/src/ipc.rs` 已有的无锁环实现；取消仍走
  `cancelatom`（Olive 的 CancelAtom 对应物）。
- 优先级：交互（seek/单帧刷新）> 播放预取 > 导出 > 自动缓存。调度层
  （scheduler.rs）从"分片给 N 个进程"改为"按优先级与依赖关系投队列"。

### 3.4 流水线作业

- 调度层维护播放方向的预取窗口（autocacher 已有近似逻辑）：帧 N 在渲染
  线程上跑时，帧 N+1..N+k 的 FootageJob 已在解码线程上解码。
- 依赖表达：FootageJob 先进 decode 队列；渲染线程遇到"输入纹理是未决
  FootageJob"时不是同步解码，而是把该渲染作业挂起到依赖完成（依赖计数
  到位后自动入 render 队列）——Olive 是同步解码（renderprocessor.cpp:325
  ProcessVideoFootage 在渲染线程内解码），我们按用户要求做真流水线。
- 背压：三条队列都有界；上屏消费慢（暂停、窗口最小化）时 render 队列满 →
  解码暂停预取；导出时上屏队列直通导出消费者，不存在"没人收"的积压。

### 3.5 GPU 零拷贝与上屏

- 内置效果全 GPU：图求值产生的中间纹理全部是 `Texture::Gpu`，整个 resolve
  过程不分配 CPU 帧；`generate_frame`（eval.rs:941）这类 CPU 帧生产点改为
  GPU 清屏。
- 上屏：gpui 自身用 wgpu 渲染。**首选**让预览画布与渲染线程共用同一
  wgpu device（gpui 暴露 device/queue 的途径需调研 gpui 侧 API；若不可行，
  次选方案是渲染线程把帧 blit 进一块与画布共享的纹理，或退一步用现有的
  staging 上传但只此一处）。这一条的落地细节作为 M2 的第一个技术攻关点，
  攻关结论回填本小节。
- CPU 边界的显式回读点只有三处：CPU OFX 插件（§3.2）、导出编码器输入、
  磁盘帧缓存写入（FrameHashCache::SaveCacheFrame 对应物）。

### 3.6 平台互操作分支（为 7 预留）

解码上传与上屏共用一层 `gpuinteop` 抽象，按后端分支实现：

| 平台/后端 | 解码→GPU | 上屏/外部共享 |
|---|---|---|
| Linux Vulkan | 软解→staging 上传；后续 VAAPI→DMA-BUF→Vulkan 外部内存 | wgpu 同源纹理直通；跨进程必要时 DMA-BUF fd |
| Windows | 软解→staging；后续 D3D11VA/NVDEC→D3D11 纹理 | D3D11 shared HANDLE / DXGI |
| CUDA（可选加速） | NVDEC→cuArray→CUDA-Vulkan 互操作（cuImportExternalMemory 系） | 仅导出/插件边界使用 |
| macOS | 软解→staging；后续 VideoToolbox→IOSurface→Metal | IOSurface 共享 |
| OpenGL（遗留/调试用） | 软解→glTexSubImage2D | 仅调试后端 |

实施顺序：staging 上传先行（正确性），DMA-BUF/D3D11/IOSurface 零拷贝上传
作为后续优化项逐个落地（每一项独立可测、可回退）。

### 3.7 resolve 重写（M0，独立先行）

对照 upstream `node/traverser.cpp:351 ResolveJobs`：

1. **节点往表里塞 `Job` 枚举**，不再塞裸载荷：jobs.rs 的 `Job` 补全为
   `Footage / Shader / Plugin / ColorTransform / Cache`（CacheJob 载荷：
   缓存路径 + 时刻 + 回退纹理，对应 eval.rs:391-399 目前 deferred 的
   `process_video_cache_job`；Generate/Sample 暂不立 Job——生成器走
   ShaderJob 变体、音频走 SampleBuffer 直通，评审时如需再补）。
2. `RenderEvalHooks::resolve` 改为**单次循环**：逐行取载荷→一次
   `match` 分发到 `process_footage_job / process_shader_job /
   process_plugin_job / process_color_transform_job / process_cache_job`；
   每个 process_* 在消耗自己的子值前**递归 resolve 子 job**（Olive 的
   `for subval: ResolveJobs(subval)` 范式，traverser.cpp:362-366）——
   例如 ShaderJob 的 effect 输入里嵌着 FootageJob 时先解码再跑 pass。
3. 类型探测从"每行最多 4 次 get_checked 试错"降为"一次枚举判别"。
4. 现有四个 `resolve_*_jobs` 函数的逐类型逻辑原样搬进对应 process_*，
   行为不变（本步是纯结构改造，现有测试全部应无修改通过；新增
   CacheJob 的磁盘加载测试）。

## 4. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| **M0 resolve 重写** | §3.7 全量：Job 枚举进表、单循环 match、CacheJob 落地 | 全 workspace 测试绿；新增 CacheJob 磁盘缓存往返测试；`resolve_*_jobs` 四函数删除 |
| **M1 线程管线骨架** | 解码/渲染/上屏三线程+三队列进 oak-render（`pipeline` 模块）；RenderManager 增加线程后端，进程池后端保留，`OAK_PIPELINE=processes` 可回退 | 同一套渲染测试在两个后端下都绿（测试矩阵化）；播放/seek/导出 smoke 等价 |
| **M2 GPU 零拷贝** | 图内全程 `Texture::Gpu`；上屏互操作攻关（§3.5）落地；导出/缓存/OFX 三处边界显式回读 | 播放路径 GPU↔CPU 搬运次数为 0（计数断言，参照 M15 S2 的 `main_heap_frame_copies` 范式）；`to_display` 不再接收 CPU 帧 |
| **M3 OFX 独立进程** | oak-ofx-host 单进程宿主；PluginJob 经 IPC；崩溃重生+紫帧回退；进度/取消协议搬运 | 杀掉 ofx-host 进程 → 在途 job 重投成功；连续三次崩溃 → 紫帧；进度条/取消行为与现状一致 |
| **M4 流水线预取** | 调度层按 §3.4 投依赖窗口；背压策略 | 1080p 播放 CPU 占用不升、fps 不低于进程池后端；首帧延迟不劣化（基准对比留档） |
| **M5 平台互操作** | §3.6 表逐行落地（每行一个独立 PR） | 每平台 CI 绿；零拷贝上传路径有开关可回退 staging |

依赖关系：M0 独立；M1 依赖 M0；M2 依赖 M1；M3 依赖 M1（与 M2 可并行）；
M4 依赖 M2；M5 依赖 M2，可拆成并行子项。

## 5. 不变量与边界

- **不动**：ticket API（oak-app/oak-cli 无感）、撤销/重做、图模型与序列化、
  缓存磁盘格式、OFX 插件 ABI、CI 的 OFX fixture 探针（ci.yml 的
  scan_probe 流程）。
- **回退开关**：线程后端落地期间进程池后端完整保留，`OAK_PIPELINE` 环境
  变量 + 配置项双开关；M4 验收通过前进程池仍是默认。
- **线程亲和**：wgpu device 可在专用线程独占使用（Device/Queue 均 Send）；
  渲染线程是唯一触摸 `GpuContext` 的线程，解码线程只做"CPU 帧→staging"
  的上传请求（经渲染线程代执行或经 device 的线程安全提交，M1 攻关确定）。
- **音频**：本计划只覆盖视频管线；音频采样链（SampleJob 对应路径）维持
  现状，如需统一另立计划。

## 6. 风险与对策

1. **上屏互操作不确定**（gpui 的 wgpu device 能否共享）：M2 第一个工作项
   就是攻关并回填结论；最坏情况退回"渲染线程 blit 到共享纹理"或"单点
   staging"，损失一次拷贝而非架构。
2. **解码器线程安全性**：oak-codec 会话当前按进程级互斥共享，集中到一个
   线程后语义更简单，但 hwaccel 解码上下文可能有线程亲和（VAAPI/NVDEC），
   M1 先做软解路径，hwaccel 随 M5 逐项验证。
3. **OFX 插件的 GPU 直通**：本期明确不支持，文档化；插件渲染结果统一按
   CPU 帧回传（§3.2）。
4. **测试环境无 GPU**：CI 的 lavapipe/xvfb 路径已在跑 wgpu（ci.yml 的
   Test 步骤），线程后端必须在该环境下同样可用；`GpuContext::create`
   的 CPU 回退（backend.rs:1422 测试所示）保持可用。
5. **范围蔓延**：Job 图的 BFS 化（v0.6 议题，用户已叫停过一次）**不在**
   本计划内；M0 只做单循环 match 分发，不改图求值顺序语义。
