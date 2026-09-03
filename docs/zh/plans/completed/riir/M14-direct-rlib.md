 # M14：前端绕过 facade 直链 rlib（纯 Rust ABI）

> 前置：单库化（single-lib.md）+ 模块 bridge/ffi 清除已完成；
> liboakengine.dylib 的 C ABI 冻结，专供插件/外部消费者。
> 本计划把三个第一方前端（app / oak-cli / oak-worker）从 dylib + C ABI
> 切换为直接静态链接各模块 rlib、纯 Rust 调用，根除 C ABI 边界的
> 内存安全问题（P5a 双释放/悬空两次真实崩溃均发生在该边界）。

## 0. 形态决策（2026-08，已定）

- **A 方案（用户拍板）**：app / oak-cli / oak-worker 只依赖
  oak_core / oakundo / oaknode / oaktimeline / oakcodec / oakaudio /
  oakrender / oaktask / oakplugin / oakstorage 的 rlib；不链接
  liboakengine（连 rlib 形态都不用）。
- **oakengine 保留为纯 cdylib**：只给 OFX 插件和未来的外部消费者
  用；它继续依赖同一批模块，C ABI 冻结不变（只增不改+大版本）。
- **facade 里的胶水逻辑下沉**（不复制）：
  1. **全局 undo 栈 + 分组**（oakengine undo.rs）→ oakundo 新增
     进程全局栈模块（stack 已是 oakundo 的，facade 只是加了全局
     单例 + 组 + capi 兼容动作）。
  2. **写穿绑定**（oakengine storage.rs：project↔库会话绑定、undo
     路径挂钩、快照线程、退出 flush）→ oakstorage 的会话管理器 +
     oakundo 全局栈的命令通知钩子。
  3. **任务编排**（facade task.rs 的 meta 簿记/进度订阅）→ oaktask
     原生（订阅/take 已有，facade 只是包装）。
  4. **库管理**（oakengine library.rs）→ oakstorage 直接面。
  5. **效果链便捷函数 / sequence 编辑组合**（facade node.rs /
     timeline.rs 的 effect_*、split/trim/ripple 组合）→ 多数已在
     oaktimeline/oaknode 有命令级原语；确属组合的，收到 oaknode
     的 ops.rs（它已是这个职责）。
  6. **编码参数助手 / exporter 家族** → oakcodec::encodingparams +
     oaktask::export（ExportTask 直接驱动）。
  7. **渲染管理器 init/可用性** → oakrender::manager 直接。
  8. **testmedia / 测试媒体** → oakcodec::testmedia（已在）。
- **unsafe 边界收敛**：前端代码零 unsafe（gpui 必要的除外）；
  模块内部 unsafe 只在插件边界（OFX C ABI）和底层库绑定
  （ffmpeg-next/ocio-sys）处。
- **内存安全验收线**：前端不出现任何裸指针/CHandle；模块间对象
  引用一律 Arc/引用（M13 的 CHandle 内部清除随本计划逐模块完成）。

## 1. 分期

| 期 | 内容 | 完成判据 |
|----|------|----------|
| R1 | 胶水下沉：oakundo 全局栈+通知、oakstorage 会话管理器、oaktask 任务面补齐、oaknode ops 组合函数 | 各模块自带测试绿；facade 改为转发下沉版（行为不变，全量绿） |
| R2 | oak-cli + oak-worker 切换（小，先蹚路） | 两 crate 零 oakengine 依赖，测试绿 |
| R3 | app 切换：src/oakui/ffi.rs、host_syms.rs 删除，real.rs 全量改 Rust 调用；AppEngine trait 不动（Mock 保留） | `cargo test`（根 crate）全绿；真机冒烟（导入/播放/编辑/导出） |
| R4 | oakengine 转纯 cdylib（workspace default-members 移除 rlib 使用点清零）、根 build.rs 链接逻辑删除、CD 去掉 app 内嵌 dylib（**已完成，2026-08**：crate-type 已是 cdylib-only、零 Rust 依赖方、cd.yml 去掉 embed 步骤与 `-p oakengine` 预构建） | CI 绿；dmg 体积显著缩小（不再嵌 37-56MB dylib） |
| R5 | M13 遗留的 CHandle 内部清除逐模块完成（bridge 已删，剩 handle.rs 与模块内 CHandle 传参） | 模块内部无 CHandle 传参；C ABI 导出层（oakengine）独占 CHandle |

依赖：R1→R2→R3→R4；R5 与 R2-R4 可并行。

## 2. 风险

| 风险 | 对策 |
|------|------|
| facade 胶水下沉破坏现有行为 | R1 先下沉+facade 转发，全量测试绿才进 R2 |
| app 调用面大（170 个 facade 函数） | real.rs 按面板/功能域分片迁移，每片绿后提交 |
| 模块 pub API 缺口 | 按需把内部函数升 pub（crate 内 #[doc] 注释） |
| 插件依赖 facade 路径行为差异 | oakengine 测试（it_* 全套）保留并继续绿 |
| CD 打包依赖 dylib 内嵌 | R4 更新 cd.yml（去掉 embed 步骤） |

## 3. 不在本期

- 插件 ABI 改版（OFX 仍 C ABI）；oakengine C ABI 任何签名变更；
  Windows 打包启用（等 Windows 链接实测）。
