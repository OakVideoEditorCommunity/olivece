# RIIR 模块拆分计划 · 00 总览

> 本目录是 liboakengine 的**模块拆分**执行计划：把单个
> liboakengine.so 拆成一组小库，模块之间只经 C ABI 调用。
> **只拆分，不重写**——这是 riir.md 绞杀者路线的第一阶段，拆分完成
> 并稳定后，才逐模块用 Rust 重写（届时模块的 C ABI 原样保留，Rust
> 实现替换 C++ 实现对调用方透明）。
>
> 阅读顺序：`00`（本文）→ `01-adapter-pattern.md`（适配器规范 + §0
> 接口铁律，所有模块共用）→ `02-modules-and-order.md`（模块清单、依赖矩阵、
> 拆分顺序）→ `03-testing.md`（测试规范）→ `04-interfaces.md`（模块间
> 接口 provides/consumes 全表）→ `M1`…`M10`（逐模块执行
> 手册，**C API 已在各手册中冻结**）。

## 目标与判据

终态：

```
oakcore（已有，不动）
oak_core ─ oakundo ─ oaknode ─ oaktimeline ─ oakcodec ─ oakrender ─ oaktask ─ oakplugin
                          │                                             │
                          └────────────── oakstorage（工程持久化，      ┘
                                           后端可插拔：文件→数据库）
                                                  oakaudio ─────────────┐
                                                                        ▼
                                              liboakengine（= facade + coreengine，纯装配层）
```

完成判据（每条都可命令验证）：

1. 每个模块是独立 CMake 目标（`add_library(oak<mod> STATIC|SHARED ...)`），
   有自己的 `include/` 公共头目录；模块 A 链接模块 B 时**只包含** B 的
   `include/oak<mod>/` 下的头，不包含 B 的私有头。
2. 模块间调用 100% 经 C ABI（`extern "C"`，见 01）。验证：
   `nm -D --defined-only liboak<mod>.so | grep -c " T _Z"` = 0（不导出
   C++ 符号），且消费方 `nm -D | grep " U _ZN5olive"` = 0。
3. 全量构建 0 error；全量 ctest 绿（45+，随模块测试增加只增不减）。
4. 每个模块的每个 C API 有 Google Test 覆盖（规范见 03）。

## 铁律

1. **只拆不写**：除边界适配器（01）和必要的反向依赖切割外，不改任何
   函数实现、不改行为。每个模块拆完，ctest 必须保持全绿。
2. **C API 先冻结后实现**：每个模块手册（M1-M9）里的 C API 表就是
   契约，实现不得偏离；执行中确需调整的，先改手册再改代码，并在手册
   里标注修订记录。
3. **qmake 式渐进**：一次只拆一个模块，闭环（构建+ctest+nm）后提交，
   再开下一个。顺序见 02，按依赖叶先根后。
4. 三条红线沿用（禁 inline 化、禁 stub、禁 dlsym）。
5. Qt 依赖：模块可继续用 Qt（拆分不是去 Qt），但 C ABI 边界上不许
   出现 Qt 类型（QString/QVariant/QList/信号槽），全部 POD 化 + 回调
   （见 01 §4）。

## 术语

- **提供侧（callee）**：被调用的模块，按 01 §1 实现 C API。
- **消费侧（caller）**：调用方模块，按 01 §2 用同名 C++ 适配类包住
  C API，使本模块内原有调用点代码**零改动**。
- **跨界类**：被其他模块消费的类（进入该模块的 C API 表面）。
- **切割点**：阻碍模块独立的反向/环依赖 include，逐条在模块手册里
  列出并给出处理方式。
