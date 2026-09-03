# 04 · 模块间接口设计（provides / consumes 全表）

> 本文是各模块手册（M1–M10）C API 的**汇总视图**：每个模块对外提供哪些
> 接口族、消费哪些模块的哪些接口族、边界上流什么数据。逐函数签名以各
> M 手册的"冻结 C API"节为准；本文管"模块与模块之间的契约关系"，管不到
> 逐函数细节。
>
> 全部接口遵守 `01-adapter-pattern.md` §0 的铁律：**纯 C、面向对象**——
> 有类型的不透明句柄（禁止 `void *` 当对象）、`init_*`/`free_*` 构造析构、
> 首参 self 的普通函数当成员函数、多态用手工函数指针表、C++ 对象与成员
> 函数一律不跨动态库边界。

## 1. 接口关系总表

行 = 消费方，列 = 提供方；单元格 = 消费的接口族（详见各提供方手册）。

| 消费 ↓ \ 提供 → | oak_core | oakundo | oaknode | oaktimeline | oakcodec | oakaudio | oakrender | oakstorage | oaktask | oakplugin | oakcore |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **facade**（src/capi） | 工具/类型 | undo 全族 | node/project/footage/serializer(clipboard) 族 | timeline 全族 | decoder/frame/proxy 族 | audio 全族 | renderer/playback/preview 族 | open/save/probe 族 | task/manager 全族 | plugin 全族 | rational/timecode |
| **oaktask** | 工具 | command 句柄 | footage/project/sequence/folder 族 | — | conform/proxy 族 | — | 导出用 render 族 | **load/save/otio 委托** | — | — | — |
| **oakrender** | videoparams/colortransform（M3.5 下沉） | command 句柄 | node 遍历/取值、project、colormanager | — | decoder/frame 族 | 音频参数 | — | — | — | plugin 实例句柄 | — |
| **oakplugin** | 工具 | command 句柄 | node 参数读写 | — | — | — | 帧缓冲/纹理句柄 | — | — | — | — |
| **oakaudio** | 工具 | — | — | — | frame/解码（1 处） | — | videoparams（下沉后消失） | — | — | — | samplebuffer |
| **oakcodec** | videoparams/subtitleparams（下沉） | — | footage 流信息（3 处） | — | — | — | — | — | — | — | rational |
| **oaktimeline** | timelinecommon 枚举（下沉） | command 句柄 | track/block/sequence 族（32 处） | — | — | — | — | — | — | — | — |
| **oaknode** | 枚举/常量/工具 | undocommand.h（4 处） | — | marker/workarea（2 处，M4 反向） | decoder/frame/proxy（8 处，M5 反向） | audio 参数（4 处，M6 反向） | colorprocessor/rendermanager/job（M7 反向，02 §4 裁决 A） | — | — | — | rational/bezier |
| **oakundo** | 工具 | — | — | — | — | — | — | — | — | — | — |
| **oakstorage** | 工具 | — | **project/root/序列化建图取图** | — | — | — | — | — | — | — | — |
| **oak_core** | — | — | — | — | — | — | — | — | — | — | — |

（空格 = 无依赖。"N 处"数据来自 02 的 include 扫描。oakcore 与
ffmpeg_bridge 为现成独立库，不参与拆分顺序。）

## 2. 逐模块接口契约

### 2.1 oak_core（M1）— 纯下沉，无业务对象

- **提供**：`include/oak_core/types.h` 的全模块共用 POD（时间戳/区间/
  枚举常量，含 M3.5 下沉的 `OakVideoParams`/`OakSubtitleParams`/
  `OakColorTransform`）；工具函数（全 `_s` 静态式，无句柄）。
- **消费**：无（叶子）。
- **边界数据**：纯 POD 值，无所有权问题。

### 2.2 oakundo（M2）— 手工虚表的第一个用户

- **提供**：`OakUndoCommand` / `OakUndoStack` 两类句柄。命令的多态
  （redo/undo 行为）经 **回调函数指针** 实现
  （`oakundo_command_create(name, redo, undo, free_fn, userdata)`），
  即铁律 §0.4 的手工虚表；消费侧**不构造 C++ 子类**。
- **消费**：oak_core。
- **边界数据**：命令句柄（owned）。无事件——push/undo/redo 的调用方
  知道栈索引变化，通知由调用方（facade 适配层）发出（见 §3）。

### 2.3 oaknode（M3）— 最大提供方

- **提供**：`OakNodeNode/NodeGroup/NodeKeyframe/NodeFactory/NodeTraverser/
  Project/Folder/Sequence/Track/TrackList/Block/Footage/ColorManager`
  句柄族。逐族清单见 M3 §2。无订阅接口——所有修改经命令函数完成，
  调用方知道影响（§3）。
- **消费**：oak_core、oakundo；对 render/codec/audio/timeline 的引用按
  02 §3/§4 的反向切割表在各模块就位后改经其 C ABI。
- **边界数据**：节点句柄（borrowed 为主，工程拥有节点）、
  `oak_node_value` POD、id 字符串（buf/size）。

### 2.4 oaktimeline（M4）

- **提供**：marker/workarea/timeline 编辑原语句柄族（`OakTimelineMarker`
  等），timeline 专用 undo 命令**经 oakundo 的回调式命令**注册，不自带
  命令子类。
- **消费**：oaknode（32 处，全部经句柄族）、oakundo、oak_core。

### 2.5 oakcodec（M5）

- **提供**：`OakCodecDecoder/Encoder/Frame/ProxyManager` 句柄族；
  帧以 `OakCodecFrame *` 不透明句柄跨边界（owned，配对 free），
  像素数据经 `oakcodec_frame_data(frame, plane, &linesize)` 取出指针
  （borrowed，生命周期随 frame）。
- **消费**：oak_core、oaknode（footage 流信息）、oakcore、ffmpeg_bridge。

### 2.6 oakaudio（M6）

- **提供**：`OakAudioManager/Processor/Synchronizer` 句柄族；波形/电平
  数据以 POD 数组 + count 出参。
- **消费**：oakcore（samplebuffer）、oakcodec（1 处）。

### 2.7 oakrender（M7）

- **提供**：`OakRenderRenderer/Ticket/Cache/ColorProcessor` 句柄族；
  渲染结果帧为 owned 句柄；渲染 ticket 是**异步命令**（后台线程），
  进度/完成回调是它的返回通道——这是 §3 允许回调的唯一情形
  （线程语义按 riir.md §6.2 钉死）。
- **消费**：oaknode、oakcodec、oak_core、oakundo（1 处）、oakbackend
  （GPU 插件，经 `renderbackend_c.h` 手工虚表——现有先例）。

### 2.8 oaktask（M8）— 编排者

- **提供**：`OakTaskTask` 句柄 + 任务工厂族 + TaskManager 查询函数。
  任务是**异步命令**：进度/完成回调即其返回通道（§3 唯一例外），
  无其他事件。load/save/import/otio 任务工厂保留，但**实现改为委托
  oakstorage**（见 M10 §4）；任务结果（Project、Footage 列表）以
  **有类型句柄**返回（`oaktask_import_take_command` 的 `void *` 返回
  按铁律 §0.1 改为 `OakUndoCommand *`）。
- **消费**：oakstorage、oaknode、oakcodec、oakrender、oaktimeline。

### 2.9 oakplugin（M9）

- **提供**：OFX 插件加载/实例句柄族（`OakPluginHost/Instance`）。
- **消费**：oaknode、oakrender、oakundo。

### 2.10 oakstorage（M10，新拆）— 工程持久化

- **提供**：`OakStorageProject`（打开的工程会话）、`OakStorageBackend`
  （手工虚函数表，后端注册用）两类句柄 + `oakstorage_open/save/probe`
  静态函数。**URI 寻址**：`file://…/*.ove` 走内建 ove-xml 后端；未来
  `oakdb://` 走数据库后端——替换数据库 = 新增一个后端实现并注册，
  消费侧零改动。
- **消费**：oaknode（反序列化建图 / 序列化取图）、oak_core。
- **边界数据**：工程句柄（owned）、XML 字节流（buf/size）、后端表。
  无事件——open/save 是同步命令，成败与结果全在返回值里，调用方
  （oaktask/facade）知道影响，由它发通知（§3）。
- 详见 `M10-oakstorage.md`。

## 3. 通知的统一约定（2026-08 修订：上层对下层只有命令）

**铁律：上层调下层只发命令，不调订阅。** 上层调用下层时必须知道该调用的
影响——改了什么、结果是什么，全部由返回值/出参告知，调用方自己决定后续
动作。因此：

- **模块间 C ABI 一律不提供 subscribe/unsubscribe 类接口。** 下层不持有
  上层的函数指针，不反向通知。各模块手册里原有的 `oak<mod>_subscribe`
  设计全部作废（M2/M3/M10 已改）。
- **变更通知由命令发起方负责。** 例：app 经 facade 调 `undo` 命令后，
  facade 知道栈索引变了，由 facade 经既有 `oakengine_event` 通道通知
  app 侧——通知的起点永远是最靠近调用者的那一层。
- **唯一例外：异步任务。** 后台执行的单元（oaktask 的任务、oakrender 的
  渲染 ticket）本质上是"提交时拿不到结果"的命令，允许进度/完成回调——
  回调即该命令的返回通道，线程语义按 01 §4 / riir.md §6.2 钉死。
  同步接口不得配回调。

## 4. 错误与所有权（横向统一）

- 返回码：`0 = OK`，负值 `OAK<MOD>_E_*`（值与 oakengine 现有对齐）；
  细节经 `oak<mod>_last_error(buf, size)`（线程局部）。
- 所有权注释三档：`/* owned */`（init/take 返回，必须配对 free）、
  `/* borrowed */`（访问器返回，禁 free）、`/* retained */`（register
  类，配对 unregister）。句柄默认 owned。
- 每个模块提供 `oak<mod>_debug_alive_count()`（测试专用，钉死泄漏）。
