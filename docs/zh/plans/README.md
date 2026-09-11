# 长期计划（plans）

本目录收纳 Oak 的**中长期规划文档与并行执行计划**。已完成的战役
文档归档在 [`completed/`](completed/)；进行中的计划在下方目录。

## 目录

| 文档 | 内容 | 启动前提 |
|---|---|---|
| [`ai-agent-design.md`](ai-agent-design.md) | **AI Agent 插件设计**（已按 OPP/1 重写）：多模态 LLM 作为外部插件经策展工具面自动剪辑，`render.*` 取帧回喂形成"编辑→看图→再编辑"视觉闭环；事务化编辑、双层确认、声明式 AI 面板、Mock LLM/回放夹具测试、A1–A5 里程碑 | external-plugin-system P1–P3 完成（面板需 P4） |
| [`external-plugin-system.md`](external-plugin-system.md) | **外部功能插件系统**：插件=独立进程（非库加载），JSON-RPC over stdio 控制面 + shm 数据面（泛化 M15 render-worker 传输）；策展宿主 API（事务化可撤销编辑、取帧回喂 AI）、声明式/像素面双 UI 路径、能力位与确认模式；含与 ai-agent-design.md 的关系与 P1–P6 里程碑 | 已解锁（RIIR + M15 完成），随时启动 |
| [`external-plugin-protocol.md`](external-plugin-protocol.md) | **插件协议规范 OPP/1**：NDJSON 分帧 + JSON-RPC 2.0 双向信封、握手/心跳/关闭、全量方法/事件/错误码、编辑事务协议、shm 数据面（无头部无锁）、声明式与像素面 UI 协议、限流配额、版本演进规则、AI 粗剪报文示例 | 随 external-plugin-system 启动 |
| [`render-pipeline-threads.md`](render-pipeline-threads.md) | **渲染管线改造**：单解码线程 + 单渲染线程 + 主进程上屏（GPU 单队列，多进程无意义）、OpenFX 收编进唯一隔离进程（崩溃重生）、decode→render→present 三队列流水线、内置效果全 GPU 零拷贝（CPU OFX 才回读，平台分支 CUDA/Vulkan/OpenGL/DirectX）、resolve 重写为 match+Job 单循环（含 CacheJob）；M0–M5 里程碑 | 已解锁，随时启动（M0 独立先行） |


## 其他参考

- 构建：`docs/zh/build.md`、`docs/zh/build_macos-zh.md`
- 工程文件：`docs/zh/project-file-reference.md`
- 代码风格与 Google Test 要求：`CONTRIBUTING.md`（仓库根）
