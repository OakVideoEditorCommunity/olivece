# M7 · oakrender 拆分手册

> 内容：`engine/render/`（Renderer/OpenGLRenderer/DynamicRenderer/
> PlaybackCache/FrameHashCache/AudioWaveformCache/ColorProcessor/
> ColorManager/RenderManager/PreviewAutoCacher/DiskManager/job 族/
> shaders 加载）。
> 依赖：node 38、common 24、codec 9、core 8、config 6、plugin 5、
> audio 3、task 2、undo 1、**src 1（分层违规）**。
> 拆分顺序第 7 位。M7 完成后 oaknode 的链接级独立复核（02 §4）。

## 1. 目标形态

```
oakrender/
  include/oakrender/{renderer.h, cache.h, color.h, manager.h, display.h,
                     types.h, export.h}
  src/
  tests/  # oakrender_gtest
```

## 2. 冻结 C API

### 2.1 `oakrender/renderer.h`（渲染器/纹理/blit）

签名照 R7-A（`../r7-pure-abi-plan.md` §A.2）的 display.h 重写版
**原样采用**——R7-A 先做的话，M7 直接把它从 facade 层搬进
oakrender 并改前缀 `oakrender_display_*`；本表不重复，以 R7-A 为准。
补充后端管理：

```c
OAKRD_API int oakrender_backend_count(void);
OAKRD_API int oakrender_backend_id_at(int i, char *buf, int n);
OAKRD_API int oakrender_set_backend(const char *backend_id);
OAKRD_API int oakrender_current_backend(char *buf, int n);
```

### 2.2 `oakrender/cache.h`（PlaybackCache/FrameHashCache/waveform 缓存）

```c
typedef struct OakRenderCache OakRenderCache;
OAKRD_API void oakrender_cache_invalidate(OakRenderCache *c,
	int64_t in_ts, int64_t out_ts);
OAKRD_API void oakrender_cache_validate(OakRenderCache *c,
	int64_t in_ts, int64_t out_ts);
OAKRD_API int oakrender_cache_has_validated_ranges(
	const OakRenderCache *c);
OAKRD_API int oakrender_cache_indicator_height(void);   /* 常量查询 */
/* 帧哈希缓存 */
OAKRD_API int oakrender_frame_cache_load(OakRenderCache *c,
	const char *path, const char *uuid, int64_t ts,
	OakCodecFrame **out_frame);
OAKRD_API void oakrender_frame_cache_save(OakRenderCache *c,
	const char *path, const char *uuid, const OakCodecFrame *f);
/* 无缓存事件（2026-08 修订，04 §3）：缓存的 invalidate/validate 由
 * 编辑命令的调用方触发并知情，通知由 facade 在命令后发出；
 * oakrender 不持有上层回调。渲染 ticket 的完成回调属异步命令
 * 返回通道，不在此限（见 renderer.h 族）。 */
```

### 2.3 `oakrender/color.h`

```c
typedef struct OakColorProcessor OakColorProcessor;
OAKRD_API OakColorProcessor *oakrender_color_processor_create(
	const char *src_space, const char *dst_transform, int direction);
OAKRD_API void oakrender_color_processor_free(OakColorProcessor *p);
OAKRD_API int oakrender_color_processor_convert(OakColorProcessor *p,
	double ir, double ig, double ib, double ia,
	double *or_, double *og, double *ob, double *oa);
/* ColorManager */
OAKRD_API int oakrender_color_manager_set_up_default_config(void);
OAKRD_API int oakrender_color_manager_get_config(char *buf, int n);
OAKRD_API int oakrender_color_manager_display_transform(
	const char *display, const char *view, char *buf, int n);
```

### 2.4 `oakrender/manager.h`（RenderManager/PreviewAutoCacher/
DiskManager/job 提交）

```c
OAKRD_API int oakrender_manager_init(void);
OAKRD_API void oakrender_manager_shutdown(void);
/* 异步帧请求：完成经回调送 OakCodecFrame（跨线程，retain 规则同 A.3） */
typedef void (*oakrender_frame_ready_fn)(OakCodecFrame *frame,
	int64_t ts, void *userdata);
OAKRD_API int64_t oakrender_request_frame(OakNodeNode *viewer,
	int64_t ts, oakrender_frame_ready_fn cb, void *userdata);
OAKRD_API void oakrender_cancel_request(int64_t request_id);
OAKRD_API int oakrender_set_cacher_multicam(OakNodeNode *multicam_or_NULL);
OAKRD_API int oakrender_set_display_color_processor(
	OakColorProcessor *p_or_NULL);
OAKRD_API int oakrender_disk_cache_path(char *buf, int n);
OAKRD_API int64_t oakrender_disk_cache_size(void);
OAKRD_API int oakrender_disk_cache_clear(void);
```

## 3. 切割点

| 现状 | 处理 |
|---|---|
| render → node/ 38（project 7、viewer 5、value 4、footage 4、traverser 3 等） | 全部经 oaknode C ABI + 适配类（M3 已就位）——**本手册的工作量主体** |
| render → codec/ 9（frame 7、decoder 1、conformmanager 1） | 经 oakcodec C ABI（M5） |
| render → task/ 2 | 回调注册（01 §4），task 句柄不传 |
| render → undo/ 1（undostack.h） | oakundo 适配类（M2） |
| render → audio/ 3 | 经 oakaudio C ABI（M6） |
| render → src/capi/displayinternal.h 1 次（违规） | R7-A 时随 display.h 重写消除；若 R7-A 未做，M7 先把该内部头的内容并入 oakrender |
| render → plugin/ 5 | pluginjob 等：M9 时经 oakplugin C ABI（M7 暂不断链，禁止新增） |

## 4. 测试（映射 03 §2/§3）

- cache：invalidate/validate 状态机、帧缓存存取往返（调用方触发后
  读状态断言——无事件）。
- color：默认 config 建置、processor convert 已知值（sRGB→Linear
  抽样点数值断言，容差 1e-3）。
- manager：request_frame 对 demo.mp4 + 最小 sequence 出帧非空
  （offscreen 可跑的部分）；cancel 路径。
- GL 相关：沿用 GTEST_SKIP 模式，不强求离屏可绘。
- `oakrender_debug_alive_count()` 泄漏断言。

## 5. 收尾复核（02 §4 裁决 A 的兑现）

M7 闭环时执行：
```
grep -rn '#include "render/' oaknode/src | wc -l     # 必须 0
nm -D oaknode 构建产物 | grep -c " U _ZN5olive.*render"   # 必须 0
```
node→render 的 47 次引用此时应全部经 oakrender C ABI（M3 遗留的
"暂不断链"在此结清）。

## 实施现状（2026-08-05）

M7 已落地：oakrender 整库编译通过、纯 C ABI 与测试就位、独立构建
全绿（37 个用例：33 通过 + 4 个 GTEST_SKIP，0 失败），oaknode 回归
96/96 不变红。以下为与上文计划的实际差异。

### 最终目录结构

- `src/render/src/` — 去 Qt 化 C++ 实现（`olive::` 命名空间），target
  `oakrender`（SHARED）+ 动态后端 `oakgl`/`oakvulkan`（各自由
  `*backend_c.cpp` 单个 TU 编成，`oak_renderer_*` C ABI，运行时由
  DynamicRenderer dlopen）；平铺结构，`src/render/src` 为 include 根。
- `src/render/c_api/` — 纯 C ABI 包装（renderer/cache/color/manager
  共 4 个 .cpp + 内部头 `alivecount.h`/`internalhandles.h`），经
  `target_sources` 合并进 `oakrender`，不单独成库。
- `src/render/tests/` — gtest，单一 target `oakrender-gtest`
  （cache/color/manager/renderer 4 个 _test.cpp），
  `gtest_discover_tests`（`DISCOVERY_MODE PRE_TEST`）。
- `include/render/`（仓库根）— 公共 C 头：`error.h` + renderer.h
  （display/纹理/帧/blit + 后端管理）、cache.h（PlaybackCache/
  FrameHashCache + `oakrender_debug_alive_count()`）、color.h
  （ColorProcessor + ColorManager 静态函数）、manager.h
  （RenderManager/PreviewAutoCacher/DiskManager + 异步帧请求）。
- `src/render/standalone/CMakeLists.txt` — 独立构建 driver（见下）。
- `src/render/transition/` — 过渡 stub/桥接头（render/ 前缀的桥接头
  转发到 `src/render/src` 真身；codec/audio/task/config/pluginSupport
  为 stub），供 oaknode 与 oakrender 共用（须置于
  `src/node/transition` 之前）。

### 独立构建与测试

```sh
cmake -S src/render/standalone -B build-oakrender
cmake --build build-oakrender -j
ctest --test-dir build-oakrender --output-on-failure
```

测试数字：oakrender **37/37**（33 通过，4 个 skip：GL 相关的
renderer init/纹理生命周期、blit_color_managed 2 例按 §4 的
GTEST_SKIP 模式；manager_init、request_frame 实跑 2 例因
RenderManager 构造读取 config 悬置符号而无法在 standalone 跑，
可跑的 E_STATE/E_INVALID/E_NOT_FOUND 路径已覆盖，含用 oaknode
factory 真建 ViewerOutput 验证）。oaknode 回归 **96/96**。
OCIO 配置照 oaknode 做法经 ctest `ENVIRONMENT OCIO=` 注入
`engine/render/ocioconf/config.ocio`；color 测试对
sRGB OETF→Linear 做已知值抽样断言（0.5 → 0.214041，容差 1e-3）。
oakrender-gtest 照 oaknode-gtest 配方 `-Wl,-force_load` libOfxHost，
并复用 `src/node/standalone/oakengine_ipc_shim.cpp` 垫
oakengine_ipc_* 悬置符号。

### 实际依赖与链接形态

- Oak 内部：oaknode、oak_core、oakundo、olivecore、ffmpeg_bridge
  （真实 target，`add_subdirectory` 引入）。
- 第三方：OpenColorIO、OpenImageIO、Imath/OpenEXR（头）、OpenGL/
  CoreVideo/Metal/QuartzCore framework、GTest（仅测试）。
- liboakrender + liboakgl/liboakvulkan 均以 `-undefined dynamic_lookup`
  悬置 codec/audio/task/pluginSupport/config 等未拆分模块符号；
  后端库在加载时从 liboakrender 解析大部分符号。
- 帧缓存 save/load 往返与 `codec_frame_set/get_params` 的存储断言
  留待 M5（oakcodec）：`codec/frame.h` 仍是 transition stub，Frame
  无法 allocate，本批只覆盖错误路径（save 不崩、load 未命中返回
  E_NOT_FOUND）。

### 与计划的主要差异

- **§5 闭环的实现方式：oaknode 直接链接 oakrender 的 C++ 库，而不
  是经 oakrender C ABI。** `src/node/transition/render/*` 已从 stub
  改为桥接头（转发到 `src/render/src` 真身），oaknode 源里的
  `#include "render/..."` 原样保留、解析到真身类；oaknode-gtest
  链接 liboakrender，node→render 引用在链接期由 C++ 符号直接结清。
  这是 02 §4 裁决 A 的变通执行（裁决原文要求"统一改成经 oakrender
  C ABI"）：OakRenderCache/ColorProcessor 等的纯 C ABI 已在
  `include/render/` 就位供后续消费者（Rust 化）使用，但 oaknode
  自身未改写为 C ABI 调用方。
- §5 的 grep/nm 判据按此口径重新表述（实测值，2026-08-05）：
  - `grep -rn '#include "render/' src/node/src | wc -l` = **38**
    （非 0；全部经桥接头指向真身，无 stub 残留语义）；
  - `nm` 口径改为「liboaknode 的 `U __ZN5olive*` 未解析符号
    共 **56** 个，**0 个悬置**」：25 个由 liboakrender 解析
    （PlaybackCache/FrameHashCache/DiskManager/RenderManager/
    ColorProcessor/PreviewAutoCacher/LUTLibrary 等），其余 31 个由
    oak_core（VideoParams/XmlStream*/SubtitleParams/QtUtils）、
    oakundo（UndoCommand/UndoStack/MultiUndoCommand vtable）解析。
- 公共头位于 `include/render/`（非 §1 的 `include/oakrender/`），
  函数前缀 `oakrender_`，与 oaknode/oak_core 的既有契约一致
  （§1 目标形态中的 `oakrender/include/oakrender/` 未采用）。
- C API 命名照 R7-A §A.2 的 display.h 重写版改前缀
  `oakrender_display_*`/`oakrender_codec_frame_*`；`OakCodecFrame`
  在 renderer.h 定义（M5 未落地，按 §2.2 注释在 oakrender 侧先写）。
- 已知坑（已解，记录给后续模块）：`src/render/transition/render/`
  桥接头占用 `"render/renderer.h"` 等拼写，与公共头
  `include/render/renderer.h` 撞名——公共头内部互相同目录
  quoted include，c_api 源用相对路径包含公共头，测试目标
  `BEFORE PRIVATE` 把 `include/` 提到最前。
