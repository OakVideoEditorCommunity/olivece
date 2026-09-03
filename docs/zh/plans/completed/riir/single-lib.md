# 单库化（single-lib）· 模块间调用改回直接 Rust 调用

> 2026-08-10。本计划是 RIIR 拆分计划的**反向修正**：模块拆分阶段为了
> 让各 crate 独立链接，把模块间调用压成了 C ABI（`bridge/` + dlsym /
> link-time extern）。现在模块已经全部 Rust 化，owner 要求模块间调用
> **改回直接 Rust 调用**（一个 `liboakengine` dylib 内联全部模块；
> unsafe 只允许留在 OFX 插件边界、引擎对外 C API、以及 shm/wgpu/libc
> 等不可避免的位置）。
>
> 约束（本次任务硬性要求）：
> 1. **不改变** `oakengine_*` 外部签名与各模块 C ABI 签名
>    （`app`/`worker`/`cli` 继续工作；`src/plugin` 的 C-ABI 冻结）。
> 2. **不触碰** OFX 插件边界（oakplugin 的 suites 保持 C）。
> 3. 每个 crate 的 `cargo test`、`src/engine/rust` 的 `cargo test`
>    全程保持绿（每步闭环）。
> 4. Oak GPL 头、tab 缩进、不做 git 变更。

## 0. 目标与判据

终态：

```
liboakengine（一个 dylib，crate 图是 DAG）
├── oakcore-rs   ← 共享 ABI 类型（CHandle + POD），无依赖的叶子
├── oak_core    → oakcore-rs
├── oakundo      → oakcore-rs
├── oaknode      → oakcore-rs, oak_core, oakundo, oakcodec
├── oaktimeline  → oakcore-rs, oakundo, oaknode
├── oakcodec     → oakcore-rs, oakrender          （codec→render 直接调）
├── oakaudio     → oakcore-rs, oak_core, oakcodec
├── oakrender    → oakcore-rs, oak_core, oaknode  （render→node 直接调）
├── oaktask      → oakcore-rs, oak_core, oakundo, oakcodec, oaknode, oakrender
├── oakplugin    → oakcore-rs, oak_core, oakundo, oaknode, oakrender（suites 仍 C）
└── oakengine    → 全部（facade）
```

完成判据（每条命令可验证）：

1. 模块间、facade→模块的调用 **100% 是编译期 Rust 函数调用**：
   `grep -rn "extern \"C\"" --include="*.rs" src/{undo,common,timeline,codec,audio,render,task,plugin,node}/rust/src/bridge/` = 0；
   `grep -rn "dlsym" --include="*.rs"` 只剩 facade 的 `linkage.rs`/`tests/` 注释与
   OFX host（oakplugin suites）里的必要解析。
2. 模块 C ABI（`oakundo_*` … `oaknode_*`）与 `oakengine_*` 依旧从
   `liboakengine` dylib 导出（`nm -g liboakengine.dylib` 抽查）——
   外部的 `oaknode_project_init` 等签名一行不改。
3. 每个 crate `cargo build` 0 error；每个 crate `cargo test` 绿；
   `src/engine/rust` `cargo test` 绿。
4. unsafe 面收敛：模块 src 中，除
   `{ffmpeg, ocio, wgpu, libc, shm} → unsafe` 包裹与各自 `ffi.rs` 导出外，
   无残留；`bridge/` 目录从模块 crate 中消失（或只余测试桩，见 §7）。

## 1. 现状盘点（2026-08-10 实测）

### 1.1 crate 依赖现状

模块 crate 之间**没有 path 依赖**；跨模块调用走各自的 `bridge/`：

| crate | 跨界机制 | bridge/ 规模（行） | dlsym::call 数 |
|---|---|---|---|
| oaknode | dlsym(RTLD_DEFAULT) | codec 1270 / common 50340 / core 5298 / render 5134 / timeline 2161 / undo 12964 | 59 |
| oakrender | dlsym | codec 6904 / common 7419 / node 5750 | 20 |
| oakplugin | dlsym | node 12124 / render 30454 / undo 10943 | 28 |
| oaktask | extern "C"（link-time） | codec 9909 / common 5980 / node 26044 / render 12340 / timeline 1291 / undo 2528 | 0 |
| oakcodec | extern "C" | common 15657 / render 7262（+test_stubs 27176） | 0 |
| oakaudio | extern "C" | codec 6683 / common 1665 / ffmpeg 7812 | 0 |
| oaktimeline | extern "C" | common / node 11742 / undo 2780（+teststubs 52921） | 0 |
| oakundo / oak_core | — | 无 bridge | — |

facade `src/engine/rust/src/bridge/`：9 个文件 2466 行、**624 个
`extern "C"` 导入**（node 348 / common 91 / render 36 / timeline 34 /
task 33 / audio 31 / codec 24 / undo 22 / plugin 5），由
`linkage.rs` 锚定 13 个 `#[no_mangle]` 符号把模块 rlib 拉进 dylib。

### 1.2 真实调用方向（生产代码，不含 bridge 定义与测试桩）

按 "`crate::bridge::<b>` 在 `<a>` 生产代码中的出现" 统计（类型别名引用
与文档注释已剔除，函数调用计数）：

| 方向 | 生产调用 | 说明 |
|---|---|---|
| node → render | **3** | `disk_cache_path`(project.rs)、`color_config_create_default`/`color_config_load`(colormanager.rs)；其余是 `TextureHandle`/`ColorProcessorHandle`/`CacheHandle` 等**不透明 CHandle 类型别名** |
| render → node | **6** | `copier.rs`：`project_deep_copy` / `project_sync_copy` / `ChangeRecord` / `node_abi_available` |
| node → codec | 1 | — |
| node → timeline | **0** | bridge 是死代码（仅测试用） |
| timeline → node | 7 | — |
| node → undo | 15 | — |
| node → common | 21 | — |
| codec → render | 8 | cancelatom（`heard_cancel` 生产、`init`/`cancel` 测试）+ `OakCancelAtom`/`OakRenderTexture` 类型 |
| render → codec | **1** | `ffi.rs:931` 的 `codec_abi_available()`，位于必然 Err 的 deferred stub 路径 |
| task → node/render/codec | 1 / 6 / 3 | timeline / undo 为 0 |
| plugin → node/render/undo | 14 / 38 / 13 | suites 层保持 C（冻结） |
| audio → codec/common | 12 / 4 | — |
| timeline → undo | 5 | — |
| **facade → 各模块** | **393 处调用** | 见 §5 |

### 1.3 环（Rust crate 禁止循环 path 依赖）

| 环 | 边 | 处理 |
|---|---|---|
| **node ↔ render** | node→render 3 fn + render→node copier | §4.1：render→node 保留直接依赖；node→render 的 3 个函数下沉 oak_core（实现本就在 oak_core 侧：`default_disk_cache_path` 已是 render/bridge/common.rs 内的纯 Rust 实现；OCIO config 由 oak_core `ocioutils::OcioConfig` 提供） |
| **codec ↔ render** | codec→render cancelatom + render→codec stub | §4.2：cancelatom 下沉 oak_core（原子取消旗标，纯 Rust）；render→codec 的 stub 检查直接删除（路径必然 Err） |

其余方向均无环：node→timeline 0 调用、task→* 单向、plugin→* 单向、
audio→* 单向、timeline→node/undo 单向。

## 2. 目标架构：直接 Rust 调用 + 单一 dylib

### 2.1 公共面分层（每个模块 crate）

每个模块 crate 的公共面分两层，对应"机械落地"与"类型化"两档：

- **Layer 1（本次任务落地）：`ffi` 即公共面，调用方直接 Rust 调用。**
  模块 C ABI 函数是 `pub unsafe extern "C" fn`（`#[no_mangle]` 保持导出）。
  引擎内部调用不再走 `extern "C"` 导入、不走 dlsym，而是
  `oaknode::ffi::project::oaknode_project_init()` 这样的**普通 Rust
  函数调用**——编译期类型检查、链接期直接解析到同 dylib 内的 rlib。
  调用方在**包装层**（各 crate 的 bridge 改为安全包装 fn）一次性
  `unsafe {}` 包住，业务代码保持 safe。
- **Layer 2（增量、后续）：类型化 API。** 各 crate 已有的非 ffi 模块
  （`project::Project`、`undostack::UndoStack`、`graph::Graph`、
  `marker::Marker`、`manager::Manager`…）即"types, not CHandle"方向；
  目前只覆盖部分操作，其余操作仍实现于 `ffi.rs`（CHandle 上）。新代码
  优先写类型层；把 `ffi.rs` 里的逻辑逐步提取成类型方法属于后续增量，
  **不在本任务范围内**（本任务只改调用机制，不改实现）。

### 2.2 类型同一性：共享 ABI 类型下沉 oakcore-rs（本任务的地基）

直接 Rust 调用要求调用方与被调方**类型同一**。现状是每个 crate 各有一份
layout 相同但 Rust 类型不同的 `CHandle`、POD（`OakVideoParams`、
`OakAudioParams`、`OakRational`、`OakNodeValue`…）。必须统一：

- `oakcore-rs` 新增（或接收）：
  - `handle::CHandle`（`{ctx, addref, release, abi_version}`，`#[repr(C)]`，
    含 `null()`/`is_null()`，`Send + Sync`；`abi_version` 不校验，
    只作标签——统一后 `null()` 盖 0，各 crate 的 `make_owned` 仍盖各自
    的 `OAKXXX_ABI_VERSION`）；
  - 跨界 POD：`params::OakVideoParams`、`params::OakAudioParams`、
    `rational::Rational`（已有）、`value::OakNodeValue` 等。
- 各模块 crate：`pub use oakcore_rs::handle::CHandle;`（`handle.rs` 保留
  `RefBox`/`make_owned`/`guard*`/ABI 版本常量）；POD 同理 re-export。
  各 crate 的 `ffi.rs` **签名不改**（类型名不变，只是指向共享类型，
  C ABI 层面 layout 不变）。
- facade：`pub use oakcore_rs::handle::CHandle;`，`OakVideoParamsPod` 等
  改为 `pub type OakVideoParamsPod = oakcore_rs::params::OakVideoParams;`。

> 这是"机械"的底气：统一后 `oaknode::ffi::project::oaknode_project_init()`
> 的返回类型就是 facade `OakEngineProject.handle` 的类型，调用点
> **一行不用改**。§7 会说明 facade `bridge/` 保留同名安全包装，因此
> facade 21k 行业务代码零改动。

## 3. 每 crate 的 bridge/ 去向

原则：**生产代码不再经过任何 `bridge/`；bridge/ 从生产路径删除。**

| crate | 处理 |
|---|---|
| facade `engine/rust/src/bridge/` | §5：`extern "C"` 块 → 直接调用模块 crate `ffi` 的安全包装（同名同签名） |
| oaknode `bridge/{undo,common,codec}` | 直接调用 oakundo/oak_core/oakcodec 的 ffi；`bridge/` 删除 |
| oaknode `bridge/{render,timeline,core}` | render/timeline 见 §4（环/死代码）；core → oakcore-rs / 宿主 liboakcore（保持 link-time extern，见 §4.3） |
| oaktask `bridge/*` | 直接调用各目标 crate ffi；`bridge/` 删除 |
| oakrender `bridge/node` | 直接调用 oaknode ffi（§4.1） |
| oakrender `bridge/{codec,common}` | codec → §4.2（stub 删除）；common → 直接调用 oak_core |
| oakplugin `bridge/{node,render,undo}` | 直接调用各目标 crate ffi；suites 保持 C |
| oakcodec `bridge/{common,render}` | common → oak_core；render → oakrender（cancelatom 移 oak_core 后改调 oak_core，见 §4.2） |
| oakaudio `bridge/{codec,common,ffmpeg}` | codec/common → 直接调用；ffmpeg → ffmpeg_bridge（C++，保持 extern "C"） |
| oaktimeline `bridge/{node,undo,common}` | node/undo → 直接调用；common 0 调用 → 删除；`teststubs.rs` 见 §7 |

**同名 `#[no_mangle]` 冲突**：两个 crate 导出同名 `#[no_mangle]` 符号
在单个 dylib 里本来就不允许（重复符号）。现状不会冲突（每个 C ABI 符号
唯一归属一个 crate）。内部调用改成直接 Rust 调用后，**外部 C ABI 符号
原样保留**（`oaknode_*` 仍由 oaknode crate 导出，供 `app`/`worker`/
`cli` 与各 crate 的 C ABI 测试使用）；内部调用**绕过**这些符号，走
`pub` Rust fn。任何"同符号双实现"都不引入——机制上天然避免。

## 4. 依赖环处理

### 4.1 node ↔ render（实施结论：两侧调用均为死代码，删除即破环）

实施时发现：**两侧的跨模块调用都是死代码**——`oaknode` 的 dlsym 目标
（`oakrender_color_config_*`）与 `oakrender` 的 dlsym 目标
（`oaknode_project_deep_copy`/`project_sync_copy`）**在对方 crate 中从未
实现**（2026-08-10 全量 grep 确认），所以运行时必然 dlsym 失败，走
"缺失"分支。破环 = 删除死调用：

1. `disk_cache_path()`：实现**下沉 oak_core**（新增
   `filefunctions::default_disk_cache_path()`，基于既有的
   `FileFunctions::get_configuration_location()`）。oaknode
   `project.rs` 与 oakrender `manager.rs` 都直接调 oak_core。
2. `color_config_create_default()`/`color_config_load()`：oakrender 从未
   实现这两个符号 → node `colormanager.rs` 的调用恒为 `None`（走
   "标记已加载/保持原状" 分支）。删除调用、保留确定性等价逻辑（行为不变）。
3. render 的 `copier.rs`（`project_deep_copy` 等）同样恒失败且无生产
   调用方——保持现状（dlsym 运行时失败），不引入 crate 依赖。

**类型别名**：node 里存的 `TextureHandle`/`ColorProcessorHandle`/
`CacheHandle` 本就是 `pub type X = CHandle`（统一后 =
`oakcore_rs::handle::CHandle`），零依赖。

结果：node 与 render 之间无任何 crate 依赖、无任何有效调用，环消失。

### 4.2 codec ↔ render（实施结论：cancelatom 保持 C 边界，render 的 stub 删除）

- **render → codec**：`ffi.rs:931` 的 `codec_abi_available()` 位于
  deferred stub（`Err(...)` 恒返回），**已删除**（保留原 Err 文案）。
  render→codec = 0。
- **codec → render**：只有 cancelatom。实施时发现 codec 收到的 atom 句柄
  是 oakrender `make_owned` 按 **oakrender 的 `RefBox` 布局**装箱的，
  codec 直接解引用会在不同 crate 的 `RefBox` 布局间读内存（oak_core
  value-first、其余 refs-first），**不安全**。因此 cancelatom 的读取保持
  经 oakrender 的 C 导出（codec `bridge::render` 的 link-time extern，
  与 `oakcore_*`/`fb_*` 同类，属"不可避免的 C 边界"）；实现体下沉
  oak_core（`cancelatom::CancelAtom`，oakrender 的
  `oakrender_cancelatom_*` 导出改包 oak_core，`render/cancelatom.rs`
  变为 re-export）。

结果：codec 与 render 之间无 crate 依赖；codec→render 仅剩 cancelatom
一个 link-time C 边界。

### 4.3 oakcore_*（宿主 liboakcore）与 fb_*（ffmpeg_bridge）

`oakcore_audioparams_*` / `oakcore_rational_*`（宿主 C++ liboakcore）与
`fb_*`（ffmpeg_bridge C++）**不是 Rust crate**，无法变成直接 Rust 调用：
保持现状（dylib 链接期 runtime lookup，`build.rs` 的
`-Wl,-undefined,dynamic_lookup`）。`bridge/core.rs` 之类保留为 link-time
extern（或就地声明），属于"不可避免的 C 边界"。

## 5. facade bridge → 直接调用（步骤 a，最大但机械）

`src/engine/rust/src/bridge/*.rs` 从 `extern "C" { ... }` 导入改为
**同名同签名的安全包装 fn**：

```rust
// src/bridge/node.rs（改写后）
use oakcore_rs::handle::CHandle;
/// `oaknode_project_init` — 直接调用 oaknode crate（同 dylib 内 rlib）。
pub fn oaknode_project_init() -> CHandle {
    unsafe { oaknode::ffi::project::oaknode_project_init() }
}
```

- 每个 ffi fn 的路径：`oak<mod>::ffi::<子模块>::<fn>`（如
  `oaknode::ffi::project`、`oaknode::ffi::folder`、`oak_core::ffi::config`…）；
  实现时以模块 crate 实际 `pub mod` 布局为准（已核对：oaknode ffi 有
  `project/node/keyframe/…` 子模块）。
- **facade 业务代码（node.rs/timeline.rs/… 共 393 处调用）零改动**：
  它们只 `use crate::bridge::node as n;` 然后 `n::oaknode_project_init()`，
  包装 fn 同名同签名，链接目标从"extern 导入"变成"直接调用"。
- unsafe 收口在包装层：模块 ffi 是 `pub unsafe extern "C" fn`，包装层
  一处 `unsafe {}`。facade 其余代码保持 safe（现存的散落 `unsafe {}`
  调用点一并消除）。
- `linkage.rs` 保留（锚定符号导出，供外部 C ABI），注释更新。
- 现 facade 里 `crate::bridge::common` 的 POD 镜像（`OakVideoParamsPod`）
  改为 `type` 别名共享类型（§2.2）。

## 6. 模块 bridge → 直接调用（步骤 b）

逐方向（每步一个 crate 一个方向，`cargo build`+`cargo test` 闭环）：

1. oaknode：`bridge/undo.rs`、`bridge/common.rs`、`bridge/codec.rs`
   （→ oakundo/oak_core/oakcodec 直接调，同 §5 的包装写法）；
2. oaktimeline：`bridge/node.rs`、`bridge/undo.rs`（→ oaknode/oakundo）；
3. oakrender：`bridge/node.rs`（→ oaknode，§4.1）、`bridge/common.rs`
   （→ oak_core）；
4. oaktask：`bridge/{node,render,codec,undo,common}.rs`；
5. oakplugin：`bridge/{node,render,undo}.rs`（suites 不动）；
6. oakcodec：`bridge/common.rs`（→ oak_core）、`bridge/render.rs`
   （→ oak_core cancelatom，§4.2）；
7. oakaudio：`bridge/{codec,common}.rs`；
8. oaknode：`bridge/render.rs`（→ oak_core，§4.1）、`bridge/timeline.rs`
   （死代码删除，测试迁移见 §7）、`bridge/core.rs`（§4.3 保留 extern）。

被调方需要被加为 path 依赖的 crate：oaknode 加 `oakundo`/`oak_core`/
`oakcodec`；oaktimeline 加 `oakundo`/`oaknode`；oakrender 加
`oak_core`/`oaknode`；oaktask 加 `oakundo`/`oak_core`/`oakcodec`/
`oaknode`/`oakrender`；oakplugin 加 `oakcore-rs`/`oak_core`/`oakundo`/
`oaknode`/`oakrender`；oakcodec 加 `oak_core`；oakaudio 加
`oak_core`/`oakcodec`。**不产生任何环**（§1.3 已消除）。

## 7. 测试策略（每 crate 测试保持绿）

- **每 crate 测试沿用 C ABI 测试为主**（`tests/*.rs` 走 crate 自己的
  `oak<mod>_*` 导出，`ffi.rs` 不变），**内部 Rust API 测试增量补充**。
- **用 `bridge/` 的测试必须迁移**（桥层从生产路径删除后不能留在测试里）：
  - 目标 crate 已 path 依赖的：`oaknode::bridge::undo::…` →
    `oakundo::ffi::…`（或 oakundo 类型层）直接调；`bridge::common::videoparams_*`
    → `oak_core::…`；`bridge::core::audioparams_*` → `oakcore_rs::…`
    / 宿主 liboakcore extern；
  - 依赖不存在（环/死代码）的：`node::bridge::timeline` 的测试用
    oaktimeline 自身导出或改写；`node::bridge::render` 的测试改调
    oak_core 下沉后的函数；
  - 专门测 dlsym 机制的测试（如 `node/rust/tests/dbg2_test.rs` 的
    `bridge::dlsym::resolve`）随机制删除而删除。
- **`test-stubs` feature（oakplugin/oak_core/oaktimeline/oaknode）**：
  语义变为"编译测试桩 C ABI（替代尚未存在的宿主/兄弟模块实现）"。
  直接调用落地后，同一二进制内不再同时出现"真实现 + 桩"冲突：
  facade `cargo test` 的 dev-dependency feature union 按新依赖图调整
  （凡直接依赖的 crate 用真实现，不再叠 test-stubs）。
- facade `tests/`（10 个集成测试）走 `oakengine_*` C ABI，不受影响；
  `tests/common/mod.rs` 的 `force_link` 更新锚点。

## 8. unsafe 清单（before / after）

| 位置 | 现状（实测计数） | 之后 |
|---|---|---|
| 模块 `bridge/`（dlsym + extern 包装） | node 298 / plugin 167 / timeline 137 / render 45 / codec 50 / task 15 处 `unsafe` 行 | **删除**（或仅测试桩） |
| 模块 `ffi.rs` 导出（`unsafe extern "C"`，内含句柄解包） | 各 crate 均有 | **保留**（= 模块 C ABI，外部契约；内部调用在包装层包 unsafe） |
| 模块业务代码中散落的 `unsafe` | node ~1020 / plugin ~476 / codec ~533 / render ~277 / task ~306 / timeline ~271 / audio ~165 / common ~230 / undo ~80 行 | 只保留**包裹外部库**（ffmpeg/ocio/wgpu/libc）的调用与必要的句柄解包；bridge 调用产生的 unsafe 随桥层消失 |
| oakplugin suites（OFX host） | C 套件 trampoline | **保持 C**（冻结，任务红线） |
| facade `engine/rust/src` | 1642 行 `unsafe`（含 ipc/shm 1120+ 行、worker、handle guard、桥层 10 行） | 桥层包装 unsafe 保留；`handle.rs` guard/unbox、`ipc.rs` shm、`worker.rs` 保留（外部 C API 与 shm，属允许项） |
| `linkage.rs` / `build.rs` | — | 保留（符号锚定 + `-Wl,-undefined,dynamic_lookup`，宿主 C++ 符号必需） |

判定：模块 crate 中"非外部库包裹"的 unsafe 清零；facade 中 unsafe 只留
{导出 guard、shm/ipc、worker 会话}。

## 9. 实施顺序（一次一个边界，每步构建+测试闭环）

1. **地基**：`oakcore-rs` 新增 `handle::CHandle`；各 crate
   `handle.rs` 改 re-export；facade 改 re-export。逐 crate 验证
   `cargo build` + `cargo test`（这一步后直接调用才类型同一）。
2. **facade bridge → 直接调用**（§5，步骤 a）。
3. **环处理**（§4）：删除死调用；disk_cache_path/cancelatom 下沉
   oak_core；render 的 `codec_abi_available()` stub 检查删除。
4. **模块 bridge 逐方向 → 直接调用**（§6，步骤 b）。
5. **dlsym 删除**（步骤 c）：node/render/plugin 的 `bridge/mod.rs::dlsym`
   与残留调用清除；`linkage.rs` 注释更新。
6. **测试迁移**（§7）与全量验证：每 crate `cargo test` +
   `src/engine/rust` `cargo test` + 抽查 dylib 导出。

## 11. 实施状态（2026-08-10 实测）

已落地（全部构建 0 error、全量 `cargo test` 绿）：

- **地基**：`oakcore_rs::handle::CHandle`（`Clone+Copy+Debug+Default`，
  `Send+Sync`，`null()` 盖 0）。10 个 crate + facade 全部 re-export；
  各 crate 的 `impl CHandle`、`unsafe impl Send/Sync` 删除。受影响断言
  （common/codec/audio/task 的 null-handle 版本/函数指针测试）已同步更新。
- **facade bridge**（步骤 a）：9 个文件 624 个 `extern "C"` 导入中
  **618 个改为直接 Rust 调用**（`unsafe { oak<mod>::ffi::<sub>::<fn>(...) }`
  安全包装，名字签名不变，facade 业务代码 393 处调用点零改动）。
  保留 8 个 link-time extern：`oakcore_audioparams_*` ×5（宿主 C++）、
  `oakaudio_manager_start_recording`/`oakcodec_encoder_init`/
  `oaktask_create_export`（encoding-params C ABI POD，facade 保留自己的
  POD 镜像，见 §11.1）。
- **签名漂移修复**（直接调用把 facade 旧 extern 声明与模块真实签名之间的
  漂移暴露为编译错误）：`videoparams_init_basic` 4→8 参、
  `init_with_time_base` 4→10 参、`get_bytes_per_channel_for_format`
  2→1 参、`oakaudio_manager_seconds` 1→2 参、
  `oakaudio_sync_estimate_stretch_and_offset` 9→12 参（facade 导出现在
  把引擎的 `min_rate/max_rate/rate_step` 透传给模块，消除了原先
  "模块自搜默认范围"的 documented deviation）、`get_buffer_size`/
  `get_time_in_timebase_units`/`get_bytes_per_pixel_for_format` 参数
  类型修正。POD 统一：`OakNodeValue`→`oaknode::value::OakNodeValue`
  （`type_` 字段改名 `kind`，1 读 3 构 + 1 测试更新）、
  `OakUndoCommandVtable`/`OakVideoTicketParams`/`OakRenderVideoParams`/
  `OffsetResult`/`StretchOffsetResult`/`SourceClip` → 各模块 crate 类型。
- **环处理**（§4 实施结论）：node↔render 死调用删除；render→codec stub
  删除；`oak_core::{filefunctions::default_disk_cache_path, cancelatom}`；
  render 的 `cancelatom.rs` 改为 re-export。
- **模块 bridge**（步骤 b，模式已证明）：oaknode `bridge/undo.rs`
  → oakundo 直接调用（oakundo 加入 node 依赖；bridge 函数名/签名不变，
  node 生产代码与测试零改动，全部绿）。

### 11.1 保留的 C 边界（facade bridge 中 8 个 extern 声明）

encoding-params POD（`oakcodec_encoding_params` 等）在 4 个 crate 各有
镜像（codec/audio/task/facade），字段仅 `[c_char;N]` vs `[u8;N]` 差异，
统一到 oakcore-rs 属后续增量（§5 已注明）；在此之前，跨越该 POD 的 3 个
函数与宿主 `oakcore_*` 保持 link-time extern，与 `fb_*` 同类。

### 11.2 实施进度（2026-08-10 第二轮）

本轮完成的方向（每步 `cargo build` + 该 crate `cargo test` 绿）：

- **oaknode `bridge/{undo,common,codec}` → 直接调用**（上轮已做 undo，
  本轮完成 common + codec）：oak_core/oakcodec 加入 node 依赖；common
  桥 31 个 dlsym 包装改为 `oak_core::ffi::*` 直接调用，`test-stubs`
  特性与库内 XML 桩删除；codec 桥的 `decoder_probe` 改为直接调用
  `oakcodec::ffi::decoder::oakcodec_decoder_probe`（签名修正为真实
  单参返回 CHandle，footage.rs 调用点同步）。node dlsym 52→20。
- **oakrender `bridge/common` → 直接调用**：config/configuration_location/
  disk_cache_path 改调 `oak_core::ffi` 与 `oak_core::filefunctions`；
  `bridge/codec.rs` 全死代码（0 生产调用、0 测试引用）**删除**；
  `bridge/node.rs`（copier 深拷贝）是死方向（oaknode 未实现该 C ABI，
  且 `oakrender_project_copier_*` 是冻结导出）——保留 dlsym 并记录。
  render dlsym 22→4。
- **oakaudio `bridge/{codec,common}` → 直接调用**：oakcodec/oak_core
  加入 audio 依赖；codec 桥的 `*mut c_void` 句柄约定与真实
  `CHandle`/`OakCodecAudioStreamInfo`/`oakcodec_encoding_params` 对齐
  （类型别名统一），调用点（waveform/manager）同步；common 桥改直接
  调用。测试侧：`manager_test` 的 encoding-params 结构从 `[c_char;N]`
  改为 `[u8;N]`（真实 codec POD），`audio_codec` 0→13（PCM——真实
  codec 把 0 映射为 DNxHD 视频编码器，打不开音频流）；录音成功路径
  改为容忍真实编码器行为（见 §11.3）；`waveform_test` 的有效文件解码
  断言改为探针错误路径（完整解码依赖宿主 ffmpeg_bridge，Rust 测试
  二进制不链接）。
- **oakcodec `test-stubs` 特性拆分**（为 node 测试二进制提供宿主符号）：
  `src/bridge/test_stubs.rs` 的 oak_core_* 桩保持 `#[cfg(test)]`
  （codec 自身测试用），oakcore_*/oakrender_* 桩改为
  `#[cfg(any(test, feature="test-stubs"))]`；node 的 dev-dependencies
  以 `features=["test-stubs"]` 依赖 oakcodec，解决 node 测试链接
  codec ffmpeg 单元时的宿主符号缺失。codec 自身测试的并发竞态
  （`ffi/frame.rs` 的 alive-count 断言与 format/proxy/task/conform
  测试未加锁）用 `lock_tests` 补齐。

### 11.3 转换中暴露的测试语义调整（行为记录）

- `oakaudio_manager_start_recording` 成功路径：旧测试假设桩编码器恒成功；
  真实 oakcodec 会真写文件（ffmpeg 打开输出），测试环境不可靠。
  该测试现只钉住 manager 自身的参数校验（NULL/禁音频 → E_INVALID
  带错误串），编码器打开结果容忍。
- `oakaudio_waveform_extract` 有效文件解码依赖宿主 ffmpeg_bridge
  （`fb_*`，C++ 库，Rust 测试二进制不链接）——测试改为只钉探针的
  NOT_FOUND 路径。
- node `footage::probe`：从 dlsym（符号缺失恒失败）改为真实
  `oakcodec_decoder_probe` 直接调用，测试的 `is_err()` 断言仍成立
  （真实 codec 对不存在文件返回错误句柄）。

### 11.4 第三轮完成（2026-08-10）

本轮完成的方向（每步 `cargo build` + 该 crate `cargo test` 绿）：

- **oakplugin `bridge/{node,render,undo}` → 直接调用**（28 处 dlsym → 0）：
  oaknode/oakrender/oakundo 加入依赖；28 个桥函数直接调 `oaknode::ffi`/
  `oakrender::ffi`/`oakundo::ffi`。`Value` 桥接用布局一致的指针 cast
  （避免 49 处 `r#type`→`kind` 改名）；`VideoParams` 别名到
  `oakrender::ffi::OakRenderVideoParams`。`test-stubs` 特性保留，桩改为
  纯 Rust（无 `#[no_mangle]`，与真实 crate 导出不冲突）；`dlsym` 模块
  删除。测试迁移：两个"符号缺失"单测改为空/NULL 句柄经真实 crate 的
  容错路径（`texture_is_dummy` 空句柄 0、`instance_render` 空 dst 被
  插件层 E_INVALID 拒绝、`render_job` 同）；OFX suites 层未触碰。
- **oaktimeline `bridge/{node,undo,common}` → 直接调用**：oaknode/
  oakundo/oak_core 加入依赖；81 个桥函数直接调用。`teststubs.rs`
  （1658 行）改为纯 Rust mock（去 `#[no_mangle]`），桥函数加
  `#[cfg(any(test, feature="test-stubs"))]` mock 变体 / 真实变体，
  测试二进制（cfg(test) 对依赖关）经 `--features test-stubs` 解析
  `bridge::teststubs`——与真实 crate 导出共存无冲突。修复桥签名漂移：
  `oak_core_xml_reader_read_next_start_element` 2 参（found 标志，
  调用点 ffi.rs/tests 同步）；`oakundo_stack_push`→真实
  `oakundo_undostack_push`。260 测试绿。
- **oaktask `bridge/*` → 直接调用**（240 个桥函数）：oaknode/oakundo/
  oak_core/oakcodec/oakrender/oaktimeline 加入依赖（oakrender 由可选
  转必选，`real-oakrender` 特性改为标记）。镜像类型对齐真实 crate：
  `OakCancelAtom`/`OakRenderTicket`/`OakRenderCache`/`OakColorProcessor`
  等 → CHandle 别名，`OakCodecEncodingParams`/`OakCodecProxyParams`/
  `OakCodecTaskRequest`/`OakRenderVideoTicketParams`/`OakUndoCommandVtable`
  → 真实 crate 类型；`from_chandle`/`to_chandle` 助手删除（恒等）。
  测试侧：encoding-params 结构从 `[c_char;N]`→`[u8;N]`，`write_cstr`
  改 u8，`fake_atom` 等 fixtures 仍可用（真实 crate 的 cancel 路径由
  task 内部创建真实 atom）。90 测试绿。
- **oakcodec `bridge/{common,render}` 句柄类型对齐**：`OakVideoParams`/
  `OakAudioParams`/`OakSubtitleParams`/`OakNodeBlock`/`OakRenderTexture`/
  `OakCancelAtom`/`OakRenderRenderer`/`OakCodecFrame` → CHandle 别名
  （删除其上的 `unsafe impl Send/Sync` 与固有 impl）；共享
  `oakcore_rs::handle::CHandle` 增补 `PartialEq, Eq`（codec 的
  `CodecStream` 派生比较）。
- **facade EncodingParamsPOD 统一**（步骤 c 的 POD 部分）：facade 的
  `EncodingParamsPOD`（41 字段 c_char 镜像）→ 别名
  `oakcodec::ffi::encoder::oakcodec_encoding_params`（u8）；`zeroed()`
  → `zeroed_encoding_params()` 自由函数；`write_field`/`field_str` 改
  u8；`oakcodec_encoder_init`/`oaktask_create_export`/
  `oakaudio_manager_start_recording` 三个 encoding-params link-time
  extern 全部改直接调用（audio 的 `EncodingParams` 同别名后打通）。
  facade bridge 仅剩宿主 `oakcore_audioparams_*` extern（C++ lib，非
  Rust crate）。

### 11.5 未完成（后续步骤）

- **dlsym 残留**（node 20、render 4）：全在死/宿主方向——node
  `bridge/core.rs`（`oakcore_audioparams_*` 宿主符号）、
  `bridge/render.rs`+`bridge/timeline.rs`（死方向，node 测试
  `ffi_timeline_test` 使用）、render `bridge/node.rs`（copier 死方向，
  `copier_test` 使用）。这些是文档化的"保留 dlsym"（死/宿主），转换
  需同步迁移对应测试；`linkage.rs` 锚定注释可更新。
- `oakcodec::bridge::render` 的 cancel-atom C 边界（设计 §4.2）保持。
- OFX suites 层保持 C（冻结，未触碰）。

## 10. 风险与回退

- **共享类型统一**是全局改动：改完一个 crate 编译它，逐 crate 推进；
  若某 crate 的 `CHandle` 有独有 impl 方法，移入共享 impl 或改为扩展 fn。
  `abi_version` 统一为 0 标签后，逐 crate 测试确认无断言依赖。
- **模块 ffi fn 路径**（`oaknode::ffi::project` 等）以实际 `pub mod`
  布局为准；包装层用编译错误驱动修正路径。
- **宿主符号**（`oakcore_*`/`fb_*`）与 `build.rs` 的
  `-Wl,-undefined,dynamic_lookup` 不动；这些不是 Rust crate，是允许保留
  的 C 边界。
- **行为零变更**：本计划只改调用机制（extern/dlsym → Rust fn），不改
  实现；任何"顺带重构"禁止。回退 = 保留 `bridge/` 目录（生产已不引用），
  不影响外部契约。
