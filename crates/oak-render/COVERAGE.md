# oakrender 类完整覆盖映射表（C++ `src/render/src` → oakrender Rust crate）

> **实现状态（2026-08）**：本表全部落点已在 `src/render/rust/` 落地；
> `cargo test` 全绿（130 passed / 6 ignored）。原落点 `backend = 留在
> C++ 后端插件` 已过时 —— Rust 侧用 wgpu 直连（backend.rs），无
> liboakgl2/liboakvulkan。延后项见 README "Deferred"：
> oakcodec 帧载荷（bridge/codec）、oaknode 深拷贝（bridge/node）、
> 进程隔离 worker（oakengine_ipc）、颜色管理 GPU blit（OCIO→WGSL）。
>
> 逐类盘点 C++ oakrender 的全部公开/内部类。落点标注：`crate 模块`
> = src/render/rust/src/ 下的文件；`bridge` = 经 C ABI 出模块；
> `plugin` = 归 M11 oakplugin crate；`drop` = 刻意不迁移（附理由）。

## 1. RenderManager / RenderThread（rendermanager.h）

| C++ | Rust 落点 |
|---|---|
| `create_instance` / `destroy_instance` / `instance` | `manager::RenderManager::init/shutdown/global` |
| `render_frame` / `render_audio`（RenderVideoParams/RenderAudioParams） | `manager` + `ticket::TicketArena::submit_video/submit_audio`（params 结构在 ticket.rs  marshalling） |
| `RenderThread`（start/add_ticket/remove_ticket/quit/wait/run） | 已删除（M15 S2 删线程池）；视频经 `procpool::ProcessDispatcher`，单测/音频经 `worker::InlineDispatcher` |
| `backend()` / `requested_backend` / `backend_from_string` / `backend_to_string` | `backend::BackendKind` + 字符串互转 |
| `get_cacher` | `manager.autocacher` |
| `set_project` | `manager`（持有项目身份，不持指针） |
| `set_aggressive_garbage_collection` / `clear_old_decoders` / `decoder_clear_loop` | `manager.set_aggressive_gc`；decoder 清理由 `bridge::codec` 的缓存管理替代（见 §9 决策注记） |
| `create_thread` | `worker.rs` 内部 |

## 2. RenderTicket / RenderTicketWatcher（renderticket.h）

| C++ | Rust 落点 |
|---|---|
| `start` / `finish(_internal)` / `is_running` / `get_finish_count` / `has_result` / `get` / `wait_for_finished` / `lock` | `ticket.rs` 内部状态机（watcher 不再存在——回调 directly on ticket；`wait_for_finished` 为阻塞 API） |
| `set_property` / `property`（Variant 属性包） | `ticket::TicketMeta`（限 string/enum 已知键，不再用 Variant） |
| `set_finished_callback` | `ticket::Completion`（FnOnce，恰好一次） |
| Watcher 全类（get_ticket/set_ticket/cancel/ticket_finished…） | 合入 ticket：`TicketId` + `cancel` + `result`；watcher 是 Qt 信号时代的转发器，不需要 |

## 3. RenderWorkerPool / workerprocess / workerjson（进程隔离）

| C++ | Rust 落点 |
|---|---|
| `start` / `submit_frame` / `remove_ticket` / `shutdown` / `worker_loop` / `process_job(_attempt)` | `procpool::ProcessDispatcher`（oak-worker 子进程，M15 默认后端；`worker::WorkerPool` 已删除） |
| `PooledWorker` / `acquire_worker` / `return_worker` / `shutdown_worker` / `clear_graph_cache` | `procpool::ProcessDispatcher` + `WorkerHandle`（spawn/handshake/崩溃重启；pre-M15 `worker::ProcessPool` 桩已删除） |
| `write_graph_snapshot` / `cleanup_graph_file` / `add/release_graph_path_ref(_locked)` / `set_graph_path_cached(_locked)` | `worker::GraphSnapshotStore`（图快照文件的引用计数缓存） |
| `is_supported` / `prepare_job` / `finish_with_frame` / `cancel_active_process` / `set/clear_active_worker` | `worker` 内部 |
| **决策注记**：进程隔离 worker 是唯一视频后端（M15 S2 用户要求删线程池）。`manager::RenderBackendChoice::{Threads,Processes}`：`Threads` = 测试专用同步 inline；`Processes` = 默认。`worker::InlineDispatcher`（sync=音频/测试，queued=测试确定性）。 | |

## 4. Renderer 抽象（renderer.h，后端接口）

| C++ | Rust 落点 |
|---|---|
| `init` / `post_init` / `destroy` / `post_destroy` / `destroy_internal` | `backend::Backend` trait（load/unload） |
| `create_native_texture` / `destroy_native_texture` / `create_texture(_from_native_handle)` / `destroy_texture` / `clear_old_textures` | `backend::Backend::create_texture/destroy_texture` + `texture::Texture` |
| `upload_to_texture` / `download_from_texture` / `flush` | `backend::Backend::upload/download/flush` |
| `blit` / `blit_to_texture` / `blit_color_managed`（3 重载） | `backend::Backend::blit(_color_managed)` |
| `create_native_shader` / `destroy_native_shader` / `get_default_shader` | `backend::Backend::shader_*` |
| `interlace_texture` | `backend::Backend::interlace` |
| `clear_destination` / `attach_output_texture` / `detach_output_texture` / `get_pixel_from_texture` | `backend::Backend` |
| `is_open_gl` / `is_vulkan` / `get_lifetime` / `is_renderer_alive` | `backend::BackendKind` + 存活由所有权表达（drop 即死） |
| `set_owner_thread_to_current` / `clear_owner_thread` / `called_on_owner_thread` | **类型化消除**：GL 上下文当前性由 `backend::ContextGuard`（RAII，Send 约束）表达，不做运行期断言 |

## 5. Texture / TextureHandle（texture.h/texturehandle.h）

| C++ | Rust 落点 |
|---|---|
| 构造族 / `~Texture` / `id()` / `params()` | `texture::Texture`（Gpu/Cpu 两态，已在底稿） |
| `upload` / `download` / `handle_frame` / `frame` | `Texture::to_frame` / `backend.upload` |
| `is_dummy` / `width` / `height` / `format` / `channel_count` / `divider` / `pixel_aspect_ratio` / `virtual_resolution` | `Texture` 查询方法 |
| `job` / `to_job` / `is_job` | **设计变更**：纹理即值；job 关联经 eval.rs 的作业记录，Texture 不再携带 job |
| `renderer()` / `is_renderer_alive` | `Texture::Gpu.backend`（种类；存活见上） |
| `TextureHandle`（句柄包装类） | 废弃——C ABI 句柄即真相 |

## 6. 缓存族（playbackcache/framehashcache/audio(waveform)cache/rendercache/colorprocessorcache/renderjobtracker）

| C++ | Rust 落点 |
|---|---|
| `PlaybackCache` 全表（uuid/invalidated ranges/invalidate/validate/request/callbacks/passthrough/load_save_state/mutex/indicator_height） | `cache::PlaybackCache`（已声明；callbacks → `cache::EventSink` trait，facade/autocacher 实现） |
| `FrameHashCache`（timebase/validate_timestamp/is_frame_cached/valid_cache_filename/save_load_cache_frame/hash_deleted/to_time/to_timestamp/cache_path_name） | `cache.rs` 的 frame-hash 子面（kind=VideoFrame/Thumbnail；`save/load_cache_frame` 经 bridge::codec 帧载荷） |
| `AudioPlaybackCache`（get/set_parameters） | `cache.rs`（AudioPlayback kind + params 字段） |
| `AudioWaveformCache`（InvalidateEvent 覆写等） | `cache.rs`（kind 差异内聚；事件经 EventSink） |
| `ThumbnailCache` | `cache.rs`（Thumbnail kind） |
| `RenderCache<T>`（值缓存模板） | `cache::ValueCache`（泛型） |
| `ColorProcessorCache` | `color::ProcessorCache` |
| `RenderJobTracker` | `autocacher` 内部（作业代际戳） |

## 7. 色彩（colorprocessor/managedcolor/lutlibrary/colortransformjob）

| C++ | Rust 落点 |
|---|---|
| `ColorProcessor`（create 两族/convert_frame 两族/convert_color/id/get_processor） | `color::ColorProcessor`（OCIO 薄封装） |
| `ColorProcessor::Direction` | `color::Direction` |
| `ManagedColor` / `ColorTransformJob` / `ShaderCode` / `ShaderJob` / `GenerateJob` / `CacheJob` / `FootageJob` / `SampleJob` / `AcceleratedJob`（job 族） | `oak_node::jobs::Job`（enum 闭合：Footage/Shader/Plugin/ColorTransform/Cache；节点在求值期把 payload 装箱推进 value table）；`eval::RenderEvalHooks::resolve` 单趟遍历输出表、先递归解析嵌套 job 再分发执行；`eval::JobSpec` 只保留执行器面（Shader/ColorTransform/Generate/Footage/Sample/Plugin） |
| `LUTLibrary`（supported_extensions/is_supported_extension） | `color::lut` |
| `ColorManager` 静态面（default config/display/view/reference） | **归 oaknode crate 的 colormanager.rs**（所有者）；render 只保留 `color::default_config` 客户端查询（bridge::node） |

## 8. PreviewAutoCacher（previewautocacher.h，44 方法）

| C++ | Rust 落点 |
|---|---|
| `set_project` / `project_destroyed` / `conform_finished` | `autocacher.attach/detach` |
| `get_single_frame`（2 重载）/ `clear_single_frame_renders(_that_arent_running)` / `cancel_queued_single_frame_render` | `autocacher.single_frame`（一次性 ticket） |
| `get_range_of_audio` / `render_frame` / `render_audio` | `autocacher` 提交 ticket（经 `ticket.rs`） |
| `force_cache_range` / `is_rendering_custom_range` | `autocacher.force_range`（已在底稿） |
| `set_playhead` / `cancel_video_tasks` / `cancel_audio_tasks` / `set_renders_paused` / `set_thumbnails_paused` / `set_multicam_node` / `set_ignore_cache_requests` / `set_display_color_processor` / `set_cache_progress_callback` / `set_stop_cache_proxy_tasks_callback` | `autocacher` 字段/方法（callbacks → EventSink） |
| `audio_rendered` / `video_rendered`（ticket 回调） | 内部完成处理 |
| `try_render` / `delayed_requeue_pending` / `cancel_delayed_requeue` / `requeue_delay_ms` | 内部调度 |
| `connect_to_node_cache` / `disconnect_from_node_cache` / `start_caching_*_range` / `*_invalidated_from_*` / `cancel_for_cache` / `cache_proxy_task_cancelled` | 内部（事件源是 cache.rs 的 EventSink 注册，不再有 C++ 回调指针） |

## 9. RenderProcessor（renderprocessor.h，NodeTraverser 子类）

| C++ | Rust 落点 |
|---|---|
| `generate_database` / `run` / `process` (static) | `eval.rs`：oaknode traverser 引擎 + `RenderEvalHooks` |
| `process_video_footage` / `process_audio_footage` / `process_shader` / `process_samples` / `process_color_transform` / `process_frame_generation` / `process_plugin_job` / `process_video_cache_job` | `eval::RenderEvalHooks` 的各 hook 方法（plugin job 转发给 oakplugin crate C ABI——render 不再认识 OFX；cache job 真读盘：`frameio` 自描述容器，缺失/损坏回退到节点输入） |
| `create_texture` / `create_sample_buffer` / `generate_texture` / `generate_frame` / `convert_to_reference_space` / `resolve_decoder_from_input` / `use_cache` | `eval.rs` + `texture.rs` + `bridge::codec` |

## 10. ProjectCopier（projectcopier.h，33 方法）

**整体反转为客户端**（C++ 里它是 render→node 耦合的最大来源）：

| C++ | Rust 落点 |
|---|---|
| `set_project` / `get_copied_project` / `get_copy<T>` / `get_original<T>` / `get_node_map` | `copier::ProjectCopy`（句柄身份映射表在 oaknode 侧维护，render 只存 identity 对） |
| `queue_*`（node_add/remove、edge_add/remove、value_change、value_hint_change、project_setting_change、footage_proxy）+ `do_*` | oaknode `ChangeRecord` 序列 + `oaknode_project_sync_copy`（bridge::node） |
| `process_update_queue` / `has_updates_in_queue` / `get_graph_change_time` / `get_last_update_time` | `copier::ProjectCopy::sync` + 代际戳字段 |
| `set_added/removed_node_handler` | `copier` 注册回调（复制后接线用） |
| `insert_into_copy_map` / `update_graph_change_value` / `update_last_synced_value` | oaknode 内部（render 不可见） |

## 11. 其余小件

| C++ | Rust 落点 |
|---|---|
| `CancelAtom` | `ticket::CancelToken`（共享原子取消标志） |
| `DiskManager`（默认缓存路径/大小/清理） | `manager::disk_cache_*`（已在 C ABI） |
| `PreviewAudioDevice` | facade/app 职责（音频输出设备绑定）→ `drop`（注释说明） |
| `paths.h` / `configaccessor.h` / `alphaassoc.h` / `rendermodes.h` | bridge::common / 常量枚举 |
| `ShaderCode`/`ShaderRequest`（节点 shader 请求） | eval 期记录（见 §7 job 族） |
| **pluginrenderer.cpp / PluginJob** | **plugin（M11 crate，2 期收编）** |

## 12. 刻意不迁移（drop）

| C++ | 理由 |
|---|---|
| Qt 信号残余 / `QObject` 父子 | Rust 所有权原生表达 |
| `Variant` 属性包（ticket params） | 闭合键集（`TicketMeta`），不需要类型擦除 |
| `Texture::to_job` 的 job 内嵌 | 求值期关联在 eval.rs，纹理保持纯值 |
| `called_on_owner_thread` 运行期断言 | `ContextGuard` 类型化替代 |
| `PreviewAudioDevice` | 设备绑定属 facade/app（M 手册边界） |
