# M10 · oakstorage 拆分手册（工程持久化，数据库替换预备）

> 内容：工程文件的读写——`engine/node/project/serializer/` 的**落盘路径**
> （Load/Save/SaveData/LoadData 的文件分支 + 版本化 serializerXXXXXX 族）
> 与 `engine/task/project/{load,save,loadotio,saveotio}` 的**文件 IO 部分**。
> **不含**剪贴板序列化（copy/paste 的节点图 XML 留在 oaknode 的
> serializer 族，见 M3）。
> 依赖：oaknode（project/root/序列化建图取图）、oak_core。
> 被依赖：oaktask（load/save 任务委托）、facade。
> 拆分顺序：M3a（oaknode 之后、oakserialize 同批）。
>
> **本模块存在的理由**：把"工程从哪来、存到哪去"收敛成唯一模块、唯一
> 接口，当前后端是 XML .ove 文件，**未来整体替换为数据库**。替换时新增
> 一个后端实现并注册即可，oaktask / facade / app 零改动。因此本模块的
> 接口按"存储后端无关"设计：URI 寻址、手工虚函数表、字节流 + 句柄，
> 接口里**不出现任何文件路径特有的概念以外的存储语义**（事务、连接串
> 等都藏在后端内部）。

## 1. 目标形态

```
oakstorage/
  include/oakstorage/{storage.h, backend.h, types.h, export.h}
  src/
    capi_storage.cpp          # storage.h 实现：URI 分发 + 会话
    backends/ove_xml/         # 内建后端：XML .ove（现有 serializer 落盘路径迁入）
    backends/...              # 未来：oakdb（数据库后端）
  tests/                      # oakstorage_gtest
```

## 2. 冻结 C API

遵守 01 §0 铁律：纯 C、有类型不透明句柄、init/free、self 首参、
多态用手工虚函数表。

### 2.1 `oakstorage/types.h`

```c
/* 错误码（0=OK，负值错误；版本/格式探测用正值信息码） */
#define OAKSTORAGE_OK                 0
#define OAKSTORAGE_TOO_OLD            1   /* 工程版本过旧（信息码） */
#define OAKSTORAGE_TOO_NEW            2   /* 工程版本过新 */
#define OAKSTORAGE_UNKNOWN_VERSION    3
#define OAKSTORAGE_E_INVALID         -1
#define OAKSTORAGE_E_STATE           -2
#define OAKSTORAGE_E_NOT_FOUND       -3
#define OAKSTORAGE_E_FAILED          -4
#define OAKSTORAGE_E_NO_BACKEND      -5   /* 无后端认领该 URI */
#define OAKSTORAGE_E_FORMAT          -6   /* 解析失败（XML/DB 约束） */
#define OAKSTORAGE_E_IO              -7   /* 读写失败 */
```

### 2.2 `oakstorage/storage.h`（消费侧主接口）

```c
/* 打开的工程会话：包裹一个已加载（或待保存）的 oakmodel Project。
 * owned 句柄，配对 oakstorage_project_free。 */
typedef struct OakStorageProject OakStorageProject;

/* --- 静态函数（无 self，对应"类方法"） ---
 * 全部是同步命令：成败与结果全在返回值/出参里，调用方知道影响，
 * 由调用方（oaktask/facade）负责对外通知；本模块无任何回调/事件。 */

/* 探测 URI：返回认领该 URI 的后端名（buf/size，先 NULL 查长度），
 * 或负值（OAKSTORAGE_E_NO_BACKEND）。不写盘、不建会话。 */
OAKSTORAGE_API int oakstorage_probe(const char *uri, char *buf, int buf_size);

/* 打开工程（load）。URI scheme 选后端：
 *   file:///path/to/proj.ove  → ove-xml 后端
 *   file:///…/proj.otio       → otio 后端（import 语义）
 *   oakdb://…                 → 未来数据库后端
 * 失败返回 NULL，细节经 oakstorage_last_error。 */
OAKSTORAGE_API OakStorageProject *oakstorage_open(
	const char *uri, int *result_code);

/* 把 project 保存到 URI（save / save-as）。
 * `options`：位掩码，OAKSTORAGE_SAVE_COMPRESS 等；后端忽略不识别的位。 */
#define OAKSTORAGE_SAVE_COMPRESS 0x1
OAKSTORAGE_API int oakstorage_save(OakNodeProject *project,
	const char *uri, unsigned options);

/* --- 成员函数（self 首参） --- */

OAKSTORAGE_API void oakstorage_project_free(OakStorageProject *self);
/* 取出工程句柄：所有权转移给调用方（此后 self 为空壳，仍须 free）。
 * 对应 oakengine/task.h 的 take_project 语义。 */
OAKSTORAGE_API OakNodeProject *oakstorage_project_take_project(
	OakStorageProject *self);
/* borrowed：不转移所有权 */
OAKSTORAGE_API OakNodeProject *oakstorage_project_project(
	const OakStorageProject *self);
/* 会话来源 URI（buf/size） */
OAKSTORAGE_API int oakstorage_project_uri(const OakStorageProject *self,
	char *buf, int buf_size);

OAKSTORAGE_API int oakstorage_last_error(char *buf, int buf_size);
OAKSTORAGE_API int oakstorage_debug_alive_count(void);  /* 测试专用 */
```

### 2.3 `oakstorage/backend.h`（手工虚函数表——后端注册接口）

多态按 01 §0.4：纯 C 函数指针表，提供侧（后端）填充，oakstorage 核心
经表调用。**后端实现不进公共头**；数据库后端未来只是多注册一行。

```c
/* 存储后端虚表。所有函数必需；返回码用 OAKSTORAGE_*。
 * 句柄协议：load 成功时 *out_project 收到 owned OakNodeProject*；
 * 后端可在 vtable 之外持有任意私有状态（连接池、事务句柄等）。 */
typedef struct OakStorageBackend {
	const char *name;          /* "ove-xml" / "otio" / "oakdb"（静态字符串） */
	const char *uri_scheme;    /* "file" / "oakdb"；同一 scheme 可注册多个
	                            * 后端，按 can_handle 顺序裁决 */

	/* 是否认领该 URI（后缀、magic bytes、连接可达性等，后端自决） */
	int (*can_handle)(const char *uri);

	/* 加载：URI → owned Project 句柄；*result_code 收 OAKSTORAGE_* */
	OakNodeProject *(*load)(const char *uri, int *result_code,
		char *err_buf, int err_buf_size);

	/* 保存：Project → URI；options 透传 storage.h 的位掩码 */
	int (*save)(OakNodeProject *project, const char *uri, unsigned options,
		char *err_buf, int err_buf_size);
} OakStorageBackend;

/* 注册/注销。oakstorage 核心不拷贝表体——后端必须保证表与 name 字符串
 * 在 unregister 前存活（内建后端为静态存储期，天然满足）。 */
OAKSTORAGE_API int oakstorage_backend_register(const OakStorageBackend *backend);
OAKSTORAGE_API int oakstorage_backend_unregister(const char *name);
```

**数据库替换路径（未来的活，接口已预留）**：
1. 写 `backends/oakdb/`：`can_handle` 认 `oakdb://`，`load/save` 走 SQL，
   建表 schema 是后端私事；
2. `oakstorage_backend_register(&oakdb_backend);` 一行接入；
3. oaktask/facade/app 不动；`file://` 的 .ove 后端继续共存（迁移期
   双后端并存，经 URI 显式选择）。
**反向约束**：任何"必须改本手册 §2.2 才能接数据库"的需求，说明接口
冻结有洞——先改本手册再动手。

### 2.4 与 oaktask 的边界（任务只是壳）

M8 的任务工厂保留原签名，实现改为薄委托：

```c
/* oaktask/project.cpp（概念） */
OakTaskTask *oaktask_create_project_load(const char *filename) {
	/* 任务体内：oakstorage_open(uri) → 完成回调里
	 * oakstorage_project_take_project() */
}
```

import/conform/precache 等非工程 IO 任务不经 oakstorage。

## 3. 切割点

| 现状 | 处理 |
|---|---|
| `node/project/serializer/*` 落盘路径在 oaknode 内 | 文件分支 + serializerXXXXXX 版本族迁入 `oakstorage/backends/ove_xml/`；**节点图 XML 生成/解析（SaveData/LoadData 的内存形态）仍属 oaknode**，ove-xml 后端经 oaknode C ABI（`oaknode_serializer_*` 族）取图/建图。剪贴板分支留 oaknode（M3/M3b） |
| `task/project/load|save` 含文件 IO | IO 部分下沉 oakstorage；task 保留任务编排（进度、取消、事件） |
| `task/project/loadotio|saveotio` | 同上，注册为 "otio" 后端（scheme=file，can_handle 认 .otio） |
| serializer 对 Qt 文件对话框/布局的引用 | 布局信息（SerializedLayoutInfo）随保存走 options 的不透明 blob（buf/size），不进入本手册冻结面 |

## 4. 测试（映射 03 §2/§3）

- **round-trip 字节一致**（金标准）：project_with_footage.ove →
  `oakstorage_open` → `oakstorage_save` 到临时 URI → 两文件字节一致；
  再 load 后 `oakstorage_project_project()` 非空、root 非空。
- probe：.ove（压缩/未压缩各一）、.otio、未知 scheme →
  E_NO_BACKEND。
- 错误路径：不存在文件 open → NULL + last_error 非空；TOO_NEW 版本
  头 → result_code = OAKSTORAGE_TOO_NEW。
- 后端虚表：注册一个内存 mock 后端（`mem://`，load/save 记日志），
  断言 open/save 全走虚表、unregister 后 probe 报 E_NO_BACKEND——
  **这条用例就是"数据库可插拔"的接口验证**。
- `oakstorage_debug_alive_count()`：open/take/free 配对无泄漏。
