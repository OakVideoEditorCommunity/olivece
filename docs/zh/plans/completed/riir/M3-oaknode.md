# M3 · oaknode 拆分手册

> 内容：`engine/node/`（Node/NodeGroup/NodeKeyframe/NodeInput/
> NodeValue/NodeFactory/NodeTraverser/NodeGroup/Project/Folder/
> Sequence/Track/TrackList/Block/ClipBlock/Footage/Serializer/
> ColorManager/各类节点实现）。
> 被依赖：timeline 32、task 38、render 38、plugin 6、codec 3、capi 110。
> 拆分顺序第 3 位（最大的模块；**链接级独立推迟到 M7 复核**，裁决
> 见 02 §4）。

## 1. 目标形态

```
oaknode/
  include/oaknode/   # node.h, project.h, track.h, block.h, footage.h,
                     # keyframe.h, factory.h, traverser.h, serializer.h,
                     # colormanager.h, types.h, export.h
  src/               # 现有 node/ 全量迁入（含全部具体节点实现子目录）
  tests/             # oaknode_gtest
```

## 2. 冻结 C API（跨界类清单）

跨界类 = 被其他模块引用的类（来源：02 依赖矩阵的热门头统计）。
每类按 01 §1 生成 `init/free/func`，此处冻结**函数族与特殊约定**
（逐函数签名照 oakengine 现有 facade 同族对齐——oakengine/node.h、
project.h、timeline.h 的对应函数就是模板，参数命名前缀换
`oaknode_`）：

| 类 | 主要消费方 | 函数族（每族含 init/free） |
|---|---|---|
| Node | 全部 | label/color/enabled 存取、input 列举与存取（`oak_node_value`）、connect/disconnect、output_connections 枚举、links、context positions、id() 字符串 |
| NodeGroup | nodeparamview 链 | passthrough add/remove/列举、resolve_input |
| NodeKeyframe | curve/keyframe | time/value/type/bezier 存取（live + undoable 变体） |
| NodeFactory | factorymenu、构造点 | id_count/id_at/name_from_id/create_from_id/node_at |
| NodeTraverser | nodevaluetree 等 | traverse db 创建/行枚举/释放 |
| Project | task/render | root、name/filename/cache_path、modified、is_modified、add/remove_node |
| Folder | projectexplorer | child 增删/列举/move_children |
| Sequence | timeline/render | track_list、workarea、markers、video/audio params、playhead |
| Track / TrackList | timeline | height/mute/lock/index/type、block 增删、split/ripple 原语 |
| Block / ClipBlock / GapBlock / TransitionBlock | timeline | in/out/length/media_in、speed/reverse/loop、links |
| Footage | task | filename、streams、proxy、duration |
| ProjectSerializer | task / oakstorage | **剪贴板** copy/paste + 节点图 XML 的内存形态（SaveData/LoadData，照 oakengine/serializer.h 模板）；**落盘 save/load 迁 oakstorage（M10），不在本模块** |
| ColorManager | render | config、default config、display transform |

**特殊约定**：
1. undoable 变体与 live 变体成对（`_live` 后缀或
   `, OakUndoCommand *command` 尾参——有类型句柄，禁 void*），
   与 oakengine 现状一致。
2. 无事件订阅接口（2026-08 修订，04 §3）：oaknode 的所有修改都经
   命令函数完成，调用方知道影响；Node 族的变更通知（label/input/
   keyframe/context 等）由 facade 在执行命令后经既有 `oakengine_event`
   通道发出（事件 id 沿用 `oakengine/events.h` 70-95 段，值不变），
   oaknode 自身不持有任何上层回调。
3. `Node *`、`Project *` 等句柄即 `OakNodeNode *`/`OakNodeProject *`，
   不透明。
4. 虚函数不出模块（01 §5）；具体节点类型经
   `oaknode_factory_create_from_id` 构造，消费侧不碰子类。

## 3. 切割点（node 的对下引用）

| 现状（次数） | 处理 |
|---|---|
| node → render/ 47（colorprocessor 8、videoparams 5、footagejob 4、rendermanager 3、pluginjob 3 等） | videoparams/colortransform 随 M3.5 下沉 oak_core；其余 **M7 时**改经 oakrender C ABI（02 §4 裁决 A：M3 暂不断链，禁止新增） |
| node → codec/ 8（decoder 4、frame 2、encoder 1、proxymanager 1） | M5 时改经 oakcodec C ABI（M5 手册已含 frame/decoder 家族） |
| node → timeline/ 5（timelinecommon 2、marker 1、workarea 1、timelineundogeneral 1） | timelinecommon 的枚举/常量下沉 oak_core/types.h；marker/workarea 引用（均在 node/project/ 序列化路径）M4 时改经 oaktimeline C ABI |
| node → audio/ 4 | M6 时改经 oakaudio C ABI |
| node → undo/ 4（undocommand.h） | M2 后改 include oakundo 公共头 + 适配类 |

M3 阶段判据（放宽版）：oaknode 目录就位、C API 实现、oaknode_gtest
全绿、对上述各向**无新增引用**（grep 快照对比）。M7 后复核
"oaknode 只经 C ABI 调下"。

## 4. 测试（映射 03 §2/§3）

- 重点：Node 增删连边、Project/Folder 层级、Track 属性、
  keyframe live/undoable 对称、serializer 剪贴板往返、
  factory 枚举与创建；每个变更命令执行后直接读状态断言生效
  （无事件可断言——通知在 facade 层测）。
- 枚举序数：NodeValue::Type ⇄ oak_node_value_type 映射表（已在
  nodevaluehandle.h 钉过一次，oaknode 测试再钉一次，防两侧漂移）。
- `oaknode_debug_alive_count()` 泄漏断言。

## 实施现状（2026-08-05）

M3 已落地并可独立构建、测试全绿（96 个用例全部通过，无 skip）。
以下为与上文计划的实际差异。

### 最终目录结构

- `src/node/src/` — 去 Qt 化 C++ 实现（`olive::` 命名空间），target
  `oaknode`（SHARED）；平铺结构，`src/node/src` 为 include 根，
  模块内 include 无前缀（`"value.h"`、`"block/block.h"`）。
- `src/node/c_api/` — 纯 C ABI 包装（node/group/keyframe/factory/
  traverser/project/folder/footage/serializer/block/track/sequence/
  colormanager 共 13 个 .cpp + 内部头 `valueconvert.h`/`alivecount.h`），
  通过 `target_sources` 合并进 `oaknode`，不单独成库。
- `src/node/tests/` — gtest，单一 target `oaknode-gtest`（13 个
  _test.cpp + 共享夹具 `testnode.h`），`gtest_discover_tests`
  （`DISCOVERY_MODE PRE_TEST`）。
- `include/node/`（仓库根）— 公共 C 头：`error.h` + node/group/
  keyframe/factory/traverser/project/folder/footage/serializer/block/
  track/sequence/colormanager.h。
- `src/node/standalone/CMakeLists.txt` — 独立构建 driver（见下）。
- `src/node/DEQT.md` — 去Qt化替换约定与逐波次裁决记录。
- `src/node/transition/` — 过渡 stub 头（见「实际依赖」）。

### 独立构建与测试

```sh
cmake -S src/node/standalone -B build-oaknode
cmake --build build-oaknode -j
ctest --test-dir build-oaknode --output-on-failure
```

driver 照 src/common/standalone 模式：EXPAT/OpenColorIO/OpenImageIO
用 Homebrew 的 config 包（`find_package(... CONFIG)`）并映射到
`${OCIO_LIBRARIES}` 等变量；`add_subdirectory` 引入真实 in-repo
target（core→olivecore、ffmpeg_bridge、src/undo→oakundo、
src/common→oak_core，各自 BUILD_TESTS 关闭），不再链接预构建
dylib；禁用 OpenTimelineIO（`/opt/otio` 的 `@loader_path` 问题，
oaknode 不需要）。

### 实际依赖

- Oak 内部：oak_core（XML/Current/工具）、oakundo（UndoCommand/
  UndoStack）、olivecore（`olive::core::Rational/Color/Bezier` 等
  C ABI 包装，真实符号）、ffmpeg_bridge（经 oak_core 间接）。
- 第三方：EXPAT、OpenColorIO、OpenImageIO、Imath（头）、FFmpeg
  （经 ffmpeg_bridge 间接）、GTest（仅测试）。
- **transition stub 机制**（裁决 A）：对尚未拆分的
  render/codec/timeline/audio/pluginSupport 模块的引用允许悬空——
  头文件由 `src/node/transition/` 的过渡 stub/转发头提供（engine 头
  仍是 Qt 版），符号经 `-undefined dynamic_lookup`（macOS）留到
  运行时解析。测试进程启动时必须能解析这些符号：oaknode-gtest
  链接真实 target（olivecore/oak_core/oakundo + OCIO/OIIO/Imath
  dylib）并 `-Wl,-force_load` 预构建的
  `build/third_party/openfx/HostSupport/libOfxHost.a`（OFX 符号与
  typeinfo，否则二进制启动即崩，PRE_TEST 发现模式也会挂；路径用
  `find_library` 定位，可由 `OAKNODE_OFX_HOST_ARCHIVE` 覆盖）。
- ColorManager 构造时需要有效 OCIO 配置（`:/ocioconf` qrc 提取是
  Qt 资源遗留、必然失败）：ctest 经 `ENVIRONMENT OCIO=...` 指向
  `engine/render/ocioconf/config.ocio`，colormanager_test 另用
  `OAK_OCIO_TEST_CONFIG` 编译定义兜底。

### 与冻结 C API 的主要差异

- 函数族命名与约定照 oak_core/oakundo 既有契约：`oaknode_<族>_<动词>`，
  int 错误码 + out 参数，字符串两段式 buffer；undoable 变体成对
  （`_undoable` 后缀，部分经 `OakUndoCommand *` 尾参）。
- §2 冻结表中**跳过/未实现**的函数族：
  - Sequence 的 workarea/markers 族（timeline 边界类型
    TimelineMarker/TimelineWorkArea 未拆，留 M4）；
  - ProjectSerializer 的落盘 save/load（按计划迁 oakstorage M10），
    本模块只实现内存形态 `save_to_xml`/`load_from_xml` + SaveData/
    LoadData 句柄；
  - Node 的 `oak_node_value` 句柄族未单独成族，输入值经
    `oaknode_node_get/set_input[_string][_undoable]` 直接收发；
  - 无任何事件订阅接口（与 §2 特殊约定 2 一致）。
- `oaknode_debug_alive_count()` 泄漏断言已实现并在测试中使用。

### 已知问题

- `NodeGroupAddInputPassthrough` 的上游 bug 原样保留（未修，待裁决）。
- polygon/text 光栅化后端钩子（PathFillBackend/TextMeasureBackend/
  TextRenderBackend）未安装前输出空白；footage 离线警示帧丢失文字
  叠层；track 默认高度固化为 13px——均为被迫行为差异，见
  `notes.md`「oaknode 去Qt化的删除与语义变更」。
- 去Qt化顺带修了三个上游 bug（行为与旧版不同）：
  `Project::clear()` 重置 `root_`（可重新 initialize）、clear 删除
  顺序修正（不再触发 disconnect_edge 的 parent assert）、
  `Sequence` 析构删除三个 `TrackList`（原泄漏）。详见 notes.md。
- 库本体对 transition stub 模块的符号悬空在 M7/M9 复核前是预期
  状态；禁止新增对 render/codec/timeline/audio/pluginSupport 的引用。
