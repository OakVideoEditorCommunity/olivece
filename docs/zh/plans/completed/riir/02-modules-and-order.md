# 02 · 模块清单、依赖矩阵与拆分顺序

> 本文基于 2026-07-26 对 engine/ 全量 490 个源文件的 include 扫描
> （方法：按 include 目标首段归类计数）。每个模块手册（M1-M9）里的
> 切割点清单都出自这张表。

## 1. 依赖矩阵（include 次数，空格=0）

| from\to | audio | codec | common | config | node | plugin | render | task | timeline | undo | core |
|---|---|---|---|---|---|---|---|---|---|---|---|
| audio | | 1 | 5 | 2 | | | 2 | | | | 5 |
| codec | | | 12 | 1 | 3 | | 11 | 5 | | | 4 |
| common | | 1 | | 1 | 3 | 1 | 7 | | | 1 | 2 |
| config | | 1 | 4 | | 1 | | | | 1 | | |
| node | 4 | 8 | 31 | 10 | | 3 | 47 | | 5 | 4 | 4 |
| pluginSupport | | | 6 | | 6 | | 6 | | | 2 | 2 |
| render | 3 | 9 | 24 | 6 | 38 | 5 | | 2 | | 1 | 8 |
| task | | 4 | 4 | 2 | 38 | | 4 | | 1 | | |
| timeline | | | 4 | 2 | 32 | | | | | 1 | 3 |
| undo | | | 2 | | 1 | | | | | | |
| src(capi) | 9 | 13 | 1 | 3 | 110 | 3 | 48 | 12 | 16 | 14 | 3 |

已知分层违规 1 处：`render/` 引用了 `src/capi/displayinternal.h`
（R7-A 重做 display.h 时一并消除）。

> **2026-08 增补（oakstorage 拆出）**：矩阵扫描早于 oakstorage 单列。
> 其依赖关系为：oakstorage → node（serializer 落盘路径，含版本化
> serializerXXXXXX 族）+ common；oaktask → oakstorage（load/save/otio
> 委托）；facade → oakstorage。详细契约见 04 §2.10 与 M10。

## 2. 模块定义与拆分顺序

顺序原则：叶子先、根后；每步只引入"已拆模块的 C ABI"，不引入
"未拆模块的 C++ 头"。

| 序 | 模块 | 内容（engine/ 下目录） | 主要切割点 |
|---|---|---|---|
| M0 | oakcore | `core/`（已完成，不动） | — |
| M1 | oak_core | `common/`（41 文件工具集）+ `config/` | common→render/node/undo/codec/plugin 的 12 次反向 include（清单见 M1 §3） |
| M2 | oakundo | `undo/`（undocommand/undostack） | undo→node/project.h 1 处（M2 §3） |
| M3 | oaknode | `node/`（图、工厂、keyframe、nodeundo、traverser） | node→render 47、node→codec 8、node→timeline 5、node→audio 4、node→undo 4（M3 §3，最大的活） |
| M3a | **oakstorage** | `node/project/serializer` 落盘路径 + `task/project/{load,save,loadotio,saveotio}` 文件 IO（**工程持久化单列**，后端可插拔：当前 XML 文件，未来数据库） | storage→node（序列化建图取图，经 oaknode C ABI）；手册 M10 |
| M4 | oaktimeline | `timeline/`（marker/workarea/timeline undo 命令族/timelinecommon） | timeline→node 32（经 oaknode C ABI + 适配类） |
| M5 | oakcodec | `codec/`（decoder/encoder/frame/proxy/conform） | codec→render 11（videoparams 等随 M3.5 下沉）、codec→task 5、codec→node 3 |
| M6 | oakaudio | `audio/`（AudioManager/AudioProcessor/输出） | audio→render 2、audio→codec 1 |
| M7 | oakrender | `render/`（Renderer/PlaybackCache/ColorManager/帧缓存/job） | render→node 38、render→codec 9、render→task 2、render→undo 1、render→src 1（违规） |
| M8 | oaktask | `task/`（Task/TaskManager/项目任务编排/cache 任务；**工程文件 IO 已划给 oakstorage，M3a**） | task→node 38、task→codec 4、task→render 4、task→storage（load/save 委托） |
| M9 | oakplugin | `pluginSupport/`（OpenFX host） | plugin→node 6、plugin→render 6、plugin→undo 2、plugin→coreengine 2 |
| M11 | oakplugin Rust 重写 | 整个模块换 Rust（含自研 OFX host 取代 HostSupport；0-2 期已立项：golden master、crate 落地、GL+ofxColour+render 驱动收编） | 手册 M11；项目 Rust 化滩头模块，完成后 node/render→plugin C++ 引用归零 |
| M12 | **app 重写（gpui）** | Qt app → gpui 桌面壳；只经冻结 C ABI 用 liboakengine，与 CLI/worker 并列为 engine 三前端 | 手册 M12；含现状盘点、P0-P5 分期与判据 |
| — | liboakengine | `src/capi` + `coreengine` + `tool/` + `ui/` 残余 | 纯装配层：facade 内部调用改经各模块 C ABI（或保持现状直接链，见 M9 §4 裁决） |

**M3.5（伴随 M3 的类型下沉）**：`render/videoparams.h`、
`render/subtitleparams.h`、`render/colortransform.h` 是纯数据类型，
codec/node 都重度引用——下沉到 **oak_core**（或独立 oakmedia 目录，
执行时二选一，默认并 oak_core），切断 codec→render 的大头。

## 3. 依赖环处理总表

| 环 | 数据 | 处理 |
|---|---|---|
| node ↔ render | 47/38 | M3 时 node 侧 47 次引用经 oakrender **尚未存在**——因此 M3 拆分时 node→render 的引用先经"前向 C ABI"处理：把 node 用到的 render 类（ColorProcessor/RenderManager/footagejob/pluginjob/videoparams）的 C API 定义在 **oaknode 手册里但由 M7 实现**？**否**——正确顺序见 §4 说明 |
| node ↔ timeline | 5/32 | node→timeline 5 次（timelinecommon×2、marker/workarea/timelineundogeneral 各1）：枚举/常量头下沉 oak_core，其余经 M4 反向 C ABI |
| node ↔ codec | 8/3 | node→codec 8（decoder/frame/encoder/proxymanager）：M5 反向 C ABI |
| node ↔ audio | 4/4 | M6 反向 C ABI |
| render ↔ codec | 9/11 | M3.5 类型下沉后剩 ~3（renderer.h/framemanager.h），M5 时处理 |
| codec ↔ task | 5/4 | codec→task 5（taskmanager/conform/proxy 的编排引用）：重排归属（proxy/conform 的 task 依赖上移 oaktask），M5/M8 处理 |
| common ↔ 各 | 12 | M1 §3 逐条 |

## 4. 关键顺序裁决：node ↔ render 怎么破

node→render 的 47 次引用（ColorProcessor 8、videoparams 5、
footagejob 4、rendermanager 3、pluginjob 3 等）在 M3 时 oakrender
还不存在。两条路：

- **A（选定）**：M3 阶段不追求 oaknode 立即独立链接，先把 oaknode
  的**公共 C ABI 头**（node/project/viewer/track/block/footage 等
  跨界类的 init/free/func）定义并实现出来；node→render 的引用在
  **M7（oakrender 拆分）时**统一改成经 oakrender C ABI。
  即：M3 只要求"oaknode 有自己的 include/ + C API + 测试"，链接
  验证推迟到 M7 闭环。
- B（否决）：先把 render 里被 node 引用的类全搬到下层——伤筋动骨，
  违反"只拆不写"。

M3 的完成判据因此放宽为：oaknode C API 实现 + 测试绿 + oaknode 内
不再新增对 render 的引用；**链接级独立**在 M7 复核。

## 5. 每模块通用落地步骤（M 手册都按此节奏）

1. 建目录：`oak<mod>/include/oak<mod>/`、`oak<mod>/src/`（先软链接/
   移动源文件，CMake 独立目标）。
2. 按手册 C API 表写 `include/oak<mod>/*.h` + `src/capi_*.cpp` 实现
   （01 §1）。
3. 消费侧逐个换：include 换适配头（01 §2），反向切割点逐条处理。
4. 按 03 写测试（该模块每个 C API 至少 1 个 TEST）。
5. 全量构建 + 全量 ctest + nm 审计（00 §判据 2）+ 提交。
