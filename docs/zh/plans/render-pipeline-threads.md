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
> 补充硬性要求（2026-09-11，用户追加）：
> **解码必须尽可能 GPU，并与渲染共用同一片 GPU 内存——解码这一步也是零拷贝；
> CPU 解码后上传只作为 fallback。解码实现优先 FFmpeg（硬解），次选手写 GPU
> 解码代码。** 本文 §2.7/§3.1/§3.4/§3.6/§4/§6 均按此修订。
>
> 补充硬性要求二（2026-09-11，用户追加）：
> **Job 图做成货真价实的图存储结构**（不是线性表，也不是二维数组），允许非
> 全连通；每个项目图固定一对**虚拟输入节点 / 虚拟输出节点**（默认相连、不可
> 删除、要在节点编辑器里显示出来）；resolve 从输入节点开始**广度优先搜索**，
> 逐个 match 处理 Job，直到全部分支汇聚于输出节点。见 §3.8。
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

### 2.3 解码在 worker 内随叫随做，硬解帧全部回读 CPU，无流水线

- `crates/oak-render/src/eval.rs:988-990`：解码器会话本就进程级共享
  （按 (filename, stream) 键控、内部互斥），但解码动作发生在每个 worker 的
  ticket 处理里：一帧的"解码→渲染→回读"是串行的，帧与帧之间没有重叠。
- `render_footage_frame`（eval.rs，FootageJob 的落点）同步解码一帧并缩放到
  目标尺寸。
- **硬件解码已经存在，但硬件帧全部下载回 CPU**（详见 §2.7）——GPU 解码
  这块砖已经有了，缺的是"不把砖搬回 CPU"的下半截。

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

### 2.7 硬解已接通但逐帧下载（解码零拷贝的现状缺口）

- `crates/oak-codec/src/hwdecode.rs:17-42`：FFmpeg 8 的 hwaccel 模型已落地
  ——建平台硬件设备上下文（`av_hwdevice_ctx_create`）挂到软解码器上，
  VideoToolbox（macOS）/ CUDA(NVDEC)→VAAPI（Linux）/ D3D11VA→CUDA
  （Windows）按候选顺序尝试，软解兜底；`OAK_HWACCEL=0` 与配置项
  `HardwareDecoding` 双开关；`HW_TRANSFERS` 计数器证明 hwaccel 真生效。
- **但每帧硬件表面都用 `av_hwframe_transfer_data` 下载到系统内存**再走
  swscale（hwdecode.rs:41-42 自述）——这就是"解码零拷贝"要消灭的那次
  下载。项目 FFmpeg 构建已带齐平台硬解
  （`--enable-nvdec/vaapi/vdpau/d3d11va/dxva2/videotoolbox`，
  tooling/ffmpeg/build-ffmpeg.sh）。
- 上游 Olive **没有硬解**（`.cache/olive-upstream/app/codec/decoder.cpp`
  全文无 hw_device/hwaccel 引用，纯软解+上传）——本计划的 GPU 解码超出
  Olive 的范围，参考对象改为 FFmpeg hwaccel API 与本仓库 hwdecode.rs 已有
  的设备模型；FFmpeg 覆盖不到的路径才手写 GPU 解码（Vulkan Video /
  直接 NVDEC 绑定，见 §3.6）。

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
│                                       │ 持有全部解码器会话+硬解设备     │
│                                       │ 硬解→GPU 导入(零拷贝,§3.6)      │
│                                       │ 软解→staging 上传(仅 fallback)  │
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
   decode 队列取 FootageJob 解码，**产出直接落在 GPU 上、与渲染共用同一
   片 GPU 内存**：硬解帧（NV12/P010 等 YUV 表面）经平台互操作原样导入为
   GPU 纹理（§3.6，零拷贝），**不执行** `av_hwframe_transfer_data`；
   CPU 解码 + staging 上传只作为硬解不可用时的 fallback（沿用
   `OAK_HWACCEL` / `HardwareDecoding` 开关，hwdecode.rs:56/102）。
   硬解表面到工作色彩空间 RGBA 的转换不再走 CPU swscale，而是渲染线程
   上的一个内置 "YUV→RGB" GPU pass（色度上采样 + 色彩矩阵着色器，
   与 §3.5 的全 GPU 中间纹理一致）。连续播放时由调度层预取（§3.4）。
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
- **硬解帧的流水走向**：解码线程产出的硬件表面（YUV）以平台句柄形态
  （DMA-BUF fd / D3D11 纹理 / CVPixelBuffer / CUDA 数组）经 §3.6 的互操作
  导入为 GPU 纹理后投 render 队列；帧的生命周期由"持有 AVHWFramesContext
  引用的 GPU 纹理包装"管理，渲染线程消费完（YUV→RGB pass 采样结束）即
  释放回硬解帧池。全路径无 `av_hwframe_transfer_data`。
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

> **M2 攻关结论（2026-09-11 回填）**：共享 device 路线可行且已落地，前提是
> **引擎与 gpui 统一到同一个 wgpu 大版本**。攻关发现：
>
> 1. gpui 的 `Window::gpu_context()`（Linux/FreeBSD）确实暴露窗口的
>    `(Arc<wgpu::Device>, Arc<wgpu::Queue>)`，但 vendored gpui_wgpu 用的是
>    **wgpu 29**，而引擎此前是 **wgpu 25**——两个大版本的 `wgpu::Texture`
>    是不同类型，纹理无法跨越。M2 把 `oak-core`/`oak-render` 升到
>    **wgpu 29 + naga 29**（`backend.rs` 的 8 处破坏性 API 改动；其余代码
>    只经 `GpuContext`），版本鸿沟消除。
> 2. `GpuContext::adopt(device, queue, kind)` 采用宿主 device；
>    `register_context`（`oak-app/src/oakui/gpu.rs`）在窗口建立时调用
>    `install_shared` 把它装进进程级 shared 槽，渲染线程因此在预览器同一
>    device 上出帧。`GpuContext::texture_handle` 把引擎纹理的
>    `Arc<wgpu::Texture>` 交给 gpui 的 `SurfaceSource::Texture`，上屏零拷贝。
> 3. **色彩管理必须留在链上**：GPU 路径不能跳过 output node 与显示器 ICC。
>    做法是把「工作空间 → 输出规格（`colormath::working_to_display_target`）
>    → 显示器 ICC（`displaycolor::apply_f32_rgba`）」在 CPU 上用**原有精确
>    实现**烘焙成 65³ 3D LUT（`oak-core::lut::Lut3d`，域
>    `[-0.25, 4]³`），经 `GpuContext::set_display_lut` 上传为 GPU 3D 纹理，
>    由 `present_texture` 的 WGSL pass 做手工三线性插值（不依赖
>    `FLOAT32_FILTERABLE`）。设置/显示器/ICC 变化（`displaycolor::generation`
>    或项目色彩设置）时重建。CPU 路径的 `apply_f32_rgba` 一行未动，GPU 与
>    CPU 逐点一致（测试对拍，f16 输出量化内）。同一套 LUT 机制也用于图内
>    `ColorTransformJob`：`process_color_transform_job` 对 GPU 纹理把 OCIO
>    processor 经 CPU 参考烘焙成 3D LUT，用 `GpuContext::apply_color_lut`
>    在 GPU 上应用（`color_transform_lut` 按 processor cache id 缓存），不再
>    pass-through；只有无法烘焙时才走一次显式回读。
> 4. **平台边界**：macOS/Windows 的 gpui 暂不暴露 device（macOS 走
>    `oak_bridge` IOSurface 的未来路线，见 acescg 计划 P2），此时
>    `present_gpu_frame` 返回 `None`，`to_display` 走**单点显式回读**
>    （唯一一次 download，之后仍 CPU 上传到 gpui）。Linux 上若引擎
>    device 不是 adopted（例如进程池 worker 的独立 device），同样回退这条
>    单点路径。CPU OFX、导出编码器、磁盘缓存三处边界显式回读不变。
>    `install_shared` 对「引擎已创建但尚未创建任何 GPU 资源」的上下文
>    允许被 UI 设备**替换**（时序守卫：开窗前任何 `shared()` 触碰都不会
>    静默丢掉零拷贝上屏），用过的上下文拒绝替换并记一条错误日志。
> 5. 进程池后端（默认）仍在 worker 内完成图求值后**显式回读**成 shm 槽
>    （oak-worker/worker.rs 的 graph 分支），这是进程模型的必然；M4 通过
>    验收前进程池仍是默认，线程管线（`OAK_PIPELINE=threads`）才走上述
>    零拷贝上屏。
> 6. 已知取舍：GPU 帧没有 `Frame::timestamp`（GPU 路径不需要）；scopes/
>    取色器的 CPU 兜底图是 1×1 占位（需要时可作为显式回读点按需填充）；
>    上屏每帧新建一张 `Rgba16Float` 目标纹理（与 gpui 的取帧生命周期一致，
>    后续可做纹理环）。

### 3.6 平台互操作分支（解码零拷贝与上屏，用户硬性要求）

**解码必须尽可能 GPU，并与渲染共用同一片 GPU 内存；CPU 解码后上传只作为
fallback。** 解码上传与上屏共用一层 `gpuinteop` 抽象，按后端分支实现。
**解码实现优先 FFmpeg 硬解**（hwdecode.rs 的设备模型 + 下表的导入路径），
**FFmpeg 覆盖不到的才手写 GPU 解码**（Vulkan Video 扩展 / 直接 NVDEC
绑定——仅当某种编码或平台组合 FFmpeg 没有 hwaccel 时立项，不提前写）：

| 平台/后端 | 硬解（FFmpeg hwaccel）→ GPU 导入（零拷贝） | fallback | 上屏/外部共享 |
|---|---|---|---|
| Linux + NVIDIA | NVDEC（CUDA 设备）→ cuArray → CUDA-Vulkan 外部内存互操作（`cuImportExternalMemory` 系）导入 wgpu 纹理 | 软解→staging 上传 | wgpu 同源纹理直通 |
| Linux + AMD/Intel | VAAPI → VASurface → DMA-BUF fd → Vulkan 外部内存（`VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier`） | 软解→staging 上传 | 同源直通；跨进程必要时 DMA-BUF fd |
| Windows | D3D11VA → D3D11 纹理 → DXGI shared HANDLE（NT 句柄）导入 wgpu（DX12/Vulkan 后端均可）；NVDEC 候选经 CUDA 互操作 | 软解→staging 上传 | D3D11 shared HANDLE / DXGI |
| macOS | VideoToolbox → CVPixelBuffer → IOSurface → Metal 纹理（wgpu Metal 后端直接包裹） | 软解→staging 上传 | IOSurface 共享 |
| OpenGL（遗留/调试用） | 不做硬解导入 | 软解→glTexSubImage2D | 仅调试后端 |

要点：

- 硬解表面通常是 YUV（NV12/P010），**导入后不是 RGB**——YUV→RGB（色度
  上采样 + BT.601/709/2020 矩阵 + 全/窄范围）是渲染线程上的内置 GPU
  pass（着色器），替代今天的 CPU swscale；HDR 元数据随纹理流转。
- 导入失败（驱动缺 modifier、句柄类型不支持等）按帧回退到
  `av_hwframe_transfer_data` + staging 上传，单帧失败不污染后续帧。
- 实施顺序：staging fallback 先行（正确性基线，任何平台都可跑），然后按
  Linux(NVDEC/VAAPI) → Windows(D3D11VA) → macOS(VideoToolbox) 的顺序
  落地零拷贝导入，每行一个独立 PR、独立开关、可单独回退。

### 3.7 resolve 重写（M0a，独立先行）

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

### 3.8 Job 图：虚拟输入/输出节点与广度优先求值（用户硬性要求）

节点图的求值不再把"每个节点往一张线性表里 push 值、resolve 扫表找活干"
当作模型（jobs.rs:31-33 自述的临时形态、§2.4 的四次扫描即其症状），而是
**Job 图即图**：

1. **图存储是货真价实的邻接结构**：求值以 `oak_node::graph::Graph` 的
   节点/连接为骨架，节点把 `Job` 枚举（§3.7-1 补全后的）挂在自己的输出
   上；没有"全图拍平成表"的中间形态，也没有二维数组。允许**非全连通**：
   与输出无关的分支不参与成帧（下述活集规则）。
2. **每个项目图固定一对虚拟节点**：
   - `GraphInput`（虚拟输入节点）：BFS 的唯一**起点**。它自身不产生
     像素——它是"图的数据入口"语义（单帧单 clip 时其后挂素材/生成器；
     序列合成时由轨道合成结构向其馈入）。只有输出端口。
   - `GraphOutput`（虚拟输出节点）：所有终末分支的**汇聚点**，它的值
     就是这一帧。只有输入端口。
   - 两节点**默认相连、不可删除、不可复制**（图模型层强约束：新建图
     自带这对节点；`remove_node`/`duplicate` 对它们拒绝；序列化把它们
     作为图的固定端点写入/读出）。
   - **要在节点编辑器里显示出来**（`crates/oak-app/src/panels/node_editor.rs`）：
     与普通节点同等的渲染与连线交互，但禁删、禁复制、禁改名；样式上
     与真节点区分（固定标题/图标），连线规则校验（输入节点不接受入线、
     输出节点不接受出线）。
3. **resolve = 从输入节点出发的广度优先搜索**：每个项目/序列的求值以
   `GraphInput` 为根做 BFS，出队一个节点就用一次 `match`（§3.7-2 的
   五个 process_*）处理它挂出的 Job，**直到全部分支汇聚于 `GraphOutput`
   为止**。遍历规则：
   - **多输入汇合**：入度到零才出队（Kahn 形态的 BFS）——一个节点的
     全部输入都 resolve 完毕它才进入处理队列；这保证汇合节点拿到的
     每一路输入都是成品纹理。
   - **多输出分叉**：一个节点的输出可以喂多个后继，沿邻接边自然扇出；
     后继各自按自己的入度等待。
   - **顺序语义**：出队序即 Job 处理序，且对同一图是确定性的（邻接按
     连接建立序迭代）——用户明确要求"节点被应用的先后顺序可能影响
     最终画面"，该顺序由图结构显式表达，而非由表扫描次序隐含。
   - **环**：visited 集防死循环；发现回边时报错并断开该分支（图模型
     现有约束本就不鼓励环，此处把行为写成明文）。
   - **活集**：正向（从 GraphInput）可达 ∩ 反向（从 GraphOutput）可达
     的节点才是活集；不可达节点从不入队（非全连通图的天然剪枝），
     正向可达但到不了输出的分支是可选的二次剪枝（先求正确，再求省）。
4. **与 resolve 重写的合流**：§3.7 的单循环 match 是本 BFS 的"出队即
   处理"循环体；M0 分两步走——先在现有线性表上完成枚举化与单循环
   （M0a，纯结构改造、独立可验收），再把表升级为图、引入虚拟端点与
   BFS（M0b，见 §4）。上游 Olive 没有虚拟端点概念（其遍历从 viewer
   输出节点倒推），这一对端点是 Oak 自己的设计，语义对齐用户的
   "从输入开始、汇聚于输出"。

## 4. 里程碑

| 里程碑 | 内容 | 验收 |
|---|---|---|
| **M0a Job 枚举化 + 单循环 resolve** | §3.7 全量：Job 枚举补全（含 CacheJob）并挂进输出表、resolve 单循环 match、子 job 递归 resolve | 全 workspace 测试绿；新增 CacheJob 磁盘缓存往返测试；`resolve_*_jobs` 四函数删除 |
| **M0b Job 图 + 虚拟端点 + BFS** | §3.8 全量：图固定 GraphInput/GraphOutput 虚拟节点（默认相连、禁删禁复制、序列化往返）、节点编辑器显示两节点、resolve 改为从输入节点的 Kahn 形态 BFS | 新增测试：多输入汇合等齐全部输入、多输出分叉各自成帧、非全连通图不可达节点不执行、环报错断支、虚拟节点删除/复制被拒、序列化往返后端点仍在；节点编辑器 UI 测试（端点可见、入线/出线规则）；既有测试全绿 |
| **M1 线程管线骨架** | 解码/渲染/上屏三线程+三队列进 oak-render（`pipeline` 模块）；RenderManager 增加线程后端，进程池后端保留，`OAK_PIPELINE=processes` 可回退 | 同一套渲染测试在两个后端下都绿（测试矩阵化）；播放/seek/导出 smoke 等价 |
| **M2 GPU 零拷贝** | 图内全程 `Texture::Gpu`（合成/转场/调整层不再逐帧回读）；wgpu 29 统一 + 采用 gpui device（§3.5 攻关已回填）；GPU 色彩管理（工作空间→输出规格→显示器 ICC 烘焙 3D LUT，GPU 执行）；导出/缓存/OFX 三处边界显式回读；内置 YUV→RGB GPU pass（M5 解码导入的依赖项，解码接线随 M5） | 图播放路径 **GPU→CPU 回读为 0**（`oak_core::backend::gpu_transfer_counters` 计数断言，M1 帧缓存范式）；`RenderedFrame::Gpu` + `to_display` 上屏在 adopted device 上零拷贝（app 测试）；YUV→RGB pass 与 `colormath::yuv444p16_to_rgb_f32` 对拍；全 workspace 测试绿 |
| **M3 OFX 独立进程** | oak-ofx-host 单进程宿主；PluginJob 经 IPC；崩溃重生+紫帧回退；进度/取消协议搬运 | 杀掉 ofx-host 进程 → 在途 job 重投成功；连续三次崩溃 → 紫帧；进度条/取消行为与现状一致 |
| **M4 流水线预取** | 调度层按 §3.4 投依赖窗口；背压策略 | 1080p 播放 CPU 占用不升、fps 不低于进程池后端；首帧延迟不劣化（基准对比留档） |
| **M5 GPU 解码零拷贝** | §3.6 表逐行落地：staging fallback 基线 → Linux NVDEC/VAAPI 导入 → Windows D3D11VA 导入 → macOS VideoToolbox 导入；FFmpeg 无 hwaccel 的组合才评估手写 GPU 解码 | 硬解路径 `HW_TRANSFERS` 计数归零（不再下载）；逐平台导入开/关对比测试；每行独立 PR 可回退 |

依赖关系：M0a 独立；M0b 依赖 M0a；M1 依赖 M0a+M0b；M2 依赖 M1；M3 依赖
M1（与 M2 可并行）；M4 依赖 M2；M5 依赖 M2（YUV→RGB pass 与互操作抽象），
可拆成并行子项。

**全程验收（用户要求）**：M5 完成、整个任务收官后，跑一轮**分支覆盖率**
（branch coverage）测量并留档，作为管线改造整体的质量闸门。

## 5. 不变量与边界

- **不动**：ticket API（oak-app/oak-cli 无感）、撤销/重做、缓存磁盘格式、
  OFX 插件 ABI、CI 的 OFX fixture 探针（ci.yml 的
  scan_probe 流程）。**例外（M0b 明确改变的两处）**：图模型新增
  GraphInput/GraphOutput 固定端点，序列化格式随之携带这对端点（旧工程
  载入时自动补挂，等价于一次无损迁移，配套序列化往返测试）。
- **回退开关**：线程后端落地期间进程池后端完整保留，`OAK_PIPELINE` 环境
  变量 + 配置项双开关；M4 验收通过前进程池仍是默认。硬解导入逐平台
  独立开关（§3.6），`OAK_HWACCEL=0` 一键回到全软解。
- **线程亲和**：wgpu device 可在专用线程独占使用（Device/Queue 均 Send）；
  渲染线程是唯一触摸 `GpuContext` 的线程；硬解帧池与硬件设备上下文
  （AVHWDeviceContext）归解码线程所有，硬件表面经互操作导入后才交给
  渲染线程消费（§3.4）。
- **音频**：本计划只覆盖视频管线；音频采样链（SampleJob 对应路径）维持
  现状，如需统一另立计划。

## 6. 风险与对策

1. **上屏互操作不确定**（gpui 的 wgpu device 能否共享）：**M2 已攻关并落地**
   （结论见 §3.5 回填）：把引擎从 wgpu 25 升到 29 后，`register_context`
   采用 gpui 的 device，`Texture::Gpu` 的原始纹理经
   `SurfaceSource::Texture` 直通 gpui，上屏零拷贝；颜色由 CPU 烘焙的 3D LUT
   在 GPU 应用，不跳过色彩管理。macOS/Windows 的 gpui 暂不暴露 device，
   自动回退到单点 staging（仍只此一处）。
2. **解码器线程安全性**：oak-codec 会话当前按进程级互斥共享，集中到一个
   线程后语义更简单，但 hwaccel 解码上下文可能有线程亲和（VAAPI/NVDEC），
   M1 先做软解路径，hwaccel 随 M5 逐项验证。
3. **OFX 插件的 GPU 直通**：本期明确不支持，文档化；插件渲染结果统一按
   CPU 帧回传（§3.2）。
4. **测试环境无 GPU**：CI 的 lavapipe/xvfb 路径已在跑 wgpu（ci.yml 的
   Test 步骤），线程后端必须在该环境下同样可用；`GpuContext::create`
   的 CPU 回退（backend.rs:1422 测试所示）保持可用。
5. **BFS 求值的兼容性**（M0b）：虚拟端点改变了图模型与求值顺序的显性
   语义，是全部里程碑里对既有行为扰动最大的一步。对策：M0a 先把
   枚举化与单循环做掉（行为不变、纯结构），M0b 单独成 PR、配全套
   §4 列举的行为测试；旧工程载入自动补挂端点。
6. **GPU 解码零拷贝的平台碎片化**（M5）：各平台导入路径（DMA-BUF
   modifier 协商、DXGI NT 句柄、IOSurface、CUDA-Vulkan 互操作）都有
   驱动/版本坑，且 wgpu 对外部内存导入的公开 API 覆盖有限，可能要
   落到 wgpu-hal 原生句柄层写 unsafe 胶水。对策：staging fallback 是
   永久基线（任何导入失败按帧回退）；按 §3.6 顺序逐平台落地，每平台
   独立开关；CUDA-Vulkan 互操作作为 NVDEC 路径的最后手段（先试
   更通用的 DMA-BUF——Linux 上 NVIDIA 也经 VAAPI 可达时优先 VAAPI）。
7. **范围蔓延**：手写 GPU 解码严格限于"FFmpeg 没有对应 hwaccel"的
   组合（用户原话"次选"），不提前立项；Job 图的序列化格式版本化
   不在本期（沿用工程文件现有版本策略）。
