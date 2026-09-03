# M1 · oak_core 拆分手册

> 内容：`engine/common/`（41 文件通用工具集）+ `engine/config/`。
> 依赖：oakcore（2）。被依赖：几乎全员（node 31、render 24、
> codec 12、plugin 6、audio 5、task 4、timeline 4、undo 2）。
> 拆分顺序第 1 位（叶子）。

## 1. 目标形态

```
oak_core/
  include/oak_core/   # 公共头（C ABI + 允许直引的纯头工具）
  src/                 # 现有 .cpp 原样迁入
  tests/               # oak_core_gtest
```

- **纯头工具**（lerp.h、define.h、decibel.h、tohex.h、digit.h、
  memorypool.h、threadsafemap.h、range.h 等无 .cpp 的）：作为
  oak_core 公共头直接提供给其他模块 include——拆分阶段允许
  （不产生链接依赖）。RIIR 阶段这些会改写成各语言自有实现。
- **config/config.h**：单头配置存取，被全模块引用（10+）。
  它是 Qt 依赖（QSettings 包装），按 §2 冻结 C ABI。

## 2. 冻结 C API（有 .cpp 实现的函数族）

命名前缀 `oak_core_`。以下按头分组（签名机械规则见 01 §3，
此处冻结函数清单与特殊约定）：

### 2.1 `oak_core/config.h`（对应 config/config.h）

```c
void oak_core_config_set(const char *group, const char *key,
                          const char *value_utf8);
int  oak_core_config_get(const char *group, const char *key,
                          char *buf, int buf_size);          /* 两段式 */
int  oak_core_config_get_int(const char *group, const char *key,
                              int fallback);
double oak_core_config_get_double(const char *group, const char *key,
                                   double fallback);
void oak_core_config_set_int(const char *group, const char *key, int v);
```

### 2.2 `oak_core/xml.h`（对应 common/xmlutils.h，8 次被引）

```c
/* XMLAttributeLoop/xml_read_next_start_element 的 C 化：
 * 以迭代器句柄包装 QXmlStreamReader */
typedef struct oak_coreXmlReader oak_coreXmlReader;
oak_coreXmlReader *oak_core_xml_reader_init(const char *utf8, int len);
void oak_core_xml_reader_free(oak_coreXmlReader *r);
int  oak_core_xml_read_next_start_element(oak_coreXmlReader *r);
int  oak_core_xml_reader_name(oak_coreXmlReader *r, char *buf, int n);
int  oak_core_xml_reader_attr(oak_coreXmlReader *r, const char *attr,
                               char *buf, int n);
int  oak_core_xml_reader_read_element_text(oak_coreXmlReader *r,
                                            char *buf, int n);
void oak_core_xml_reader_skip_current(oak_coreXmlReader *r);
```

写出侧 `oak_core_xml_writer_*`（init_to_string/write_attribute/
write_text_element/free 得字符串，两段式）。

### 2.3 `oak_core/files.h`（common/filefunctions.h，render 7 次）

```c
int  oak_core_file_exists(const char *path);                 /* 1/0 */
int  oak_core_file_size(const char *path);                   /* -1 失败 */
int  oak_core_file_read_all(const char *path, char *buf, int n); /* 两段式 */
int  oak_core_file_write_all(const char *path, const char *data, int n);
int  oak_core_dir_mkpath(const char *path);
int  oak_core_get_config_path(char *buf, int n);
int  oak_core_get_temp_path(char *buf, int n);
```

### 2.4 `oak_core/ocio.h`、`oak_core/oii.h`、`oak_core/ffmpeg.h`

（ocioutils/oiioutils/ffmpegutils，按 01 §3 机械 POD 化；
OAK/OIIO/FFmpeg 类型全部句柄化或拍平字段。）

### 2.5 `oak_core/misc.h`

`jobtime`（`double oak_core_jobtime_now(void)`）、`current`
（`oak_core_current_get/set` 线程局部当前对象句柄）。

## 3. 切割点（common 的 12 次反向 include，逐条）

| 现状 | 处理 |
|---|---|
| common → render/ 7 次（播放钟/自动滚动等引 render 类型） | 涉及文件（playbackaudioclock/autoscroll 等）**上移出 oak_core**：它们不是底层工具，归 oakrender（M7） |
| common → node/ 3 次 | 同上，归 oaknode（M3） |
| common → undo/ 1 次 | 同上，归 oakundo（M2） |
| common → codec/ 1 次 | 同上，归 oakcodec（M5） |
| common → pluginSupport/ 1 次 | 同上，归 oakplugin（M9） |

判据：切完后 `grep -rn '#include "' oak_core/src | grep -vE
'"(oak_core|olive/core)'` 为空（只剩 oakcore 与 Qt/系统头）。

## 4. 测试（映射 03 §2）

- config：set/get 往返、int/double fallback、两段式 buf。
- xml：reader 解析样例串、attr/text 读取、skip、writer 产出解析回读。
- files：临时目录建/写/读/尺寸/删除。
- 每函数 1 正常 + 1 错误路径；`oak_core_debug_alive_count()` 泄漏断言。

## 实施现状（2026-08-05）

M1 已落地并可独立构建、测试全绿（127 个用例：126 通过，1 个
`GTEST_SKIP`）。以下为与上文计划的实际差异。

### 最终目录结构

- `src/common/src/` — 去 Qt 化 C++ 实现（`olive::` 命名空间），target
  `oak_core`（SHARED）。
- `src/common/c_api/` — 纯 C ABI 包装，通过 `target_sources` 合并进
  `oak_core`，不单独成库。
- `src/common/tests/` — gtest，target `oak_core-gtest`，
  `gtest_discover_tests`。
- `include/common/`（仓库根）— 公共 C 头：`commandlineparser.h`、
  `current.h`、`debug.h`、`dropworkflowbehavior.h`、`error.h`、
  `ffmpegutils.h`、`filefunctions.h`、`miscutils.h`、`ocioutils.h`、
  `oiioutils.h`、`power.h`、`qtutils.h`、`xmlutils.h`。
- `src/common/standalone/CMakeLists.txt` — 独立构建 driver（见下）。

### 独立构建与测试

```sh
cmake -S src/common/standalone -B build-oak_core
cmake --build build-oak_core -j
ctest --test-dir build-oak_core --output-on-failure
```

driver 通过 `find_package(... CONFIG)` 使用 Homebrew 的 OCIO/OIIO/GTest
（顶层 `cmake/FindOpenColorIO.cmake`/`FindOpenImageIO.cmake` 面向
`.so`，macOS 下不适用），并把 config target 映射到 oak_core
CMakeLists 消费的 `${OCIO_LIBRARIES}` 等变量。driver 中额外处理了两点：
给 `olivecore` 补 `third_party/openfx/include` 头路径（顶层靠全局
include）；禁用 OpenTimelineIO（`/opt/otio` 的 dylib 用 `@loader_path`
安装名，构建树内无法加载，且 oak_core 不需要 OTIO）。

### 实际依赖

- 第三方：EXPAT（XML 解析）、OpenColorIO、OpenImageIO、FFmpeg（经
  ffmpeg_bridge 间接）、GTest（仅测试）。
- Oak 内部库：**仍链接 `olivecore` 与 `ffmpeg_bridge`**，二者均为真实
  符号依赖而非纯头文件：
  - `olive::core::Rational`（`core/include/olive/core/util/rational.h`）
    是对 `oakcore_rational_*` C ABI 的包装，构造/析构/运算都需要
    olivecore 的符号；
  - `FFmpegUtils::get_compatible_bridge_pixel_format` 调用
    `fb_find_best_pix_fmt_of_list`（ffmpeg_bridge 导出符号）。
  - `pixelformat.h`/`sampleformat.h` 本身是 header-only，只需头路径
    （`core/include`、`ffmpeg_bridge/include`，因这两个 target 的
    include 目录是 PRIVATE，在 oak_core 里显式补为 PUBLIC）。
- 未按计划只链 oakcore 头；OLIVECORE_BUILD_TESTS 在独立构建中关闭。

### 与计划的主要差异

- 接口未按 §2 冻结清单逐条实现，而是按实际使用面包装：C ABI 头放在
  仓库根 `include/common/`，命名 `oak_core_<模块>_<动词>`，两段式
  buffer（先查询尺寸再拷入）约定统一。
- `xmlutils` 用 expat 实现（事件预先解析成队列）；C API 的
  `oak_core_xml_reader_read_element_text` 因底层是消费型读取，在
  handle 内缓存最近一次文本以兼容两段式调用。
- 被移除/上移的类（播放钟、autoscroll 等反向 include 涉及项）见
  `notes.md`。
- 计划中提到的 `switch(PixelFormat)` 编译问题实际不存在：
  `olive::core::PixelFormat` 提供 `operator Format()`，可直接 switch。
