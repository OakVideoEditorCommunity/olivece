# 记录项

## common

CancelableObject只在render和task处有🚰，移入对应模块。

## 从 oak_core（src/common）移除的类（2026-08-05）

判据：不严重依赖 common 其他类，且只有一个非 common 模块使用它
（tests/gtest 不计入使用方）。以下类已从 `src/common/` 移除，
拆分时应放到对应模块：

| 类 | 唯一使用方 | 应放到 |
|---|---|---|
| `Html`（html.h/.cpp） | engine/node | oaknode（M3）。注意它依赖 common 的 xmlutils，迁移时需连同 XML 辅助或改为调用 oak_core C API |
| `JobTime`（jobtime.h/.cpp） | engine/render | oakrender（M7） |
| `OTIOUtils`（otioutils.h） | engine/task | oaktask（M8） |
| `PlaybackAudioClock`（playbackaudioclock.h） | engine/audio | oakaudio（M6） |
| `to_hex`（tohex.h） | engine/node | oaknode（M3） |
| `mid()`（util.h） | engine/node | oaknode（M3） |
| `AVFramePtr`（avframeptr.h） | engine/render | oakrender（M7） |
| `CrashpadInterface`（crashpadinterface.h/.cpp） | app/main.cpp | app（crashhandler 相关） |
| crashpadutils.h | app/crashhandler | app/crashhandler |
| `AutoScroll`（autoscroll.h，oakutil shim） | app/dialog、app/widget | app |
| `digit.h`（oakutil shim） | app/dialog | app |
| `range.h`（oakutil shim） | app/widget | app |

另：`power.h`、`memorypool.h`、`threadsafemap.h` 当前没有任何
common 模块外的使用方（零用户），不满足移除判据，暂保留在
oak_core；后续若确认无用途可直接删除。

## oak_core 去Qt化的删除与语义变更（2026-08-05）

去Qt化过程中以下函数被删除或语义变化，迁移调用方时需注意：

- `CommandLineParser::print_help()`：不再自动读取
  `QCoreApplication::applicationName()/applicationVersion()`，需先调
  `set_app_info()`（C API：`oak_core_commandlineparser_set_app_info`），
  否则打印默认 `"oak"` + 空版本。
- `FileFunctions::get_unique_file_identifier`：哈希由 SHA-1 改为
  FNV-1a 64-bit，旧缓存 key 全部失效（需一次重建）。
  `get_configuration_location`/`get_application_path`/
  `get_temp_file_path`/`get_auto_recovery_root` 的标准路径改为手写
  平台实现，路径与原 Qt 版不同。`read_file_as_string(":/...")` 的
  qrc 资源路径不再可用，shader 等资源调用方需改为磁盘路径或内嵌。
- xmlutils：`xml_read_next_start_element(reader, CancelAtom*)`
  重载与 cancel_atom 参数删除（依赖 render/cancelatom.h），取消语义
  由调用方在循环外自行检查；`XMLAttributeLoop` 宏删除，由
  `XmlStreamReader::attributes()` + range-for 替代；
  `QXmlStreamReader/Writer` 由基于 expat 的
  `olive::XmlStreamReader/Writer` 替代（不支持 XML 命名空间）。
- `OIIOUtils::frame_to_buffer/buffer_to_frame`：未进 C API，C++ 侧签名
  拍平为 `(const void *data, int64_t linesize_bytes, OIIO::ImageBuf *)`，
  engine/codec/frame.cpp 迁移时传成员即可。`oak_corePixelFormat` 枚举
  目前定义在 include/common/ocioutils.h，oiioutils 复用，后续可抽成
  独立的 include/common/pixelformat.h。
- `MemoryPool`：删除 QTimer 每 5 秒自动回收空 arena，改为公有
  `clear_empty_arenas()` 由调用方周期调用；不再继承 QObject。
  memorypool/threadsafemap 零使用方且为模板/多态基类，未包 C API。
- `debug_handler`：丢失 `QMessageLogContext`（文件/行号）参数（原实现
  本就未用）；`qInstallMessageHandler` 无对应物。
- `Current::set_current_video_params/audio_params`：由按值拷贝改为持有
  `std::shared_ptr<void>`（类型擦除），调用方需自行管理生命周期并用
  `static_pointer_cast` 还原（调用点：olivehost.cpp、
  timebasedwidget.cpp、plugin.cpp 等 6 处）。
- qtutils：engine 未使用的纯 UI 函数（`q_font_metrics_width`、
  `create_horizontal/vertical_line`、`set_combo_box_data`、
  `word_wrap_string`、`flip_control_and_shift_modifiers`、
  `get_formatted_date_time`、`to_q_color`、`core::qHash`、
  `Q_DECLARE_METATYPE` 宏组）未迁入 oak_core，app 层继续用 Qt 版。
  `ptr_to_value/value_to_ptr` 载体由 QVariant 改为 `uintptr_t`，
  迁移时调用点需同步改（renderprocessor.cpp、rendermanager.cpp、
  previewautocacher.cpp、src/capi/worker.cpp）。`get_parent_of_type`
  依赖 C++ 模板+RTTI，未进 C API。
- define.h：删除 `MACRO_NAME_AS_STR`/`MACRO_VAL_AS_STR`/
  `OLIVE_NS_CONST_ARG`/`OLIVE_NS_ARG`/`OLIVE_NS_RETURN_ARG`
  （依赖 Qt QArgument，全仓无使用）。
- decibel.h：删除 `ALLOW_RETURNING_INFINITY` 编译开关分支（代码库
  未定义该宏，行为不变）。
- `FFmpegUtils`：新增类内常量 `k_rgb_channel_count/k_rgba_channel_count`
  （3/4）取代对 `VideoParams` 同名常量的引用，消除 common→render
  反向依赖。

## oakundo 去Qt化的删除与语义变更（2026-08-05）

`src/undo/`（olive::UndoCommand / MultiUndoCommand / UndoStack）去Qt化
过程中以下删除与语义变化，迁移调用方时需注意：

- `UndoStack` 不再继承 `QAbstractItemModel`（删除 `Q_OBJECT`、
  `columnCount/data/index/parent/rowCount/headerData/hasChildren` 全部
  model overrides 及 `begin/end*Rows` 通知）。历史面板（QTreeView 等）
  属 app UI 层，迁移时应以 facade 的行式查询
  （command_count/done_count/command_name/command_is_done）自行实现
  model。
- `UndoStack::GetUndoAction/GetRedoAction/update_actions`（QAction 成员
  及其文本/使能维护）删除——QAction 属 app 菜单层，app 应自行创建
  action 并连接 undo/redo；`index_changed(int)` signal 改为 C++ 侧
  `std::function<void(int)>`（`set_index_changed_callback`），C ABI
  不暴露事件，调用方在变更命令后读 `oakundo_undostack_index`。
- `UndoCommand` 与 `olive::Project` 解耦：删除纯虚
  `get_relevant_project()`（含 MultiUndoCommand/EmptyCommand 的
  override）与 `project_` 成员；`redo_and_set_modified/
  undo_and_set_modified` 的修改标记语义改为可选回调对
  `set_modified_callbacks(is_modified, set_modified)`：redo 时记录当前
  修改标记并置 true，undo 时恢复记录值；不挂回调则退化为纯
  redo/undo。读写顺序与 Qt 版一致（redo_now() 之后读取并置位），
  行为对齐。Project 侧迁移时在适配层把 `Project::is_modified/
  set_modified` 绑到这对回调即可。
- `UndoStack` 析构不再调用 `clear()`（Qt 版在析构里 clear 会再 push
  一个 EmptyCommand 造成泄漏），改为直接删除所有持有命令。
- 顺带修复三处 engine 原版就存在的缺陷（迁移调用方无需适配，但
  行为与旧版不同，特此记录）：
  - `jump(0)` 死循环：底部 EmptyCommand 不可 undo，`jump` 现以
    `can_undo()/can_redo()` 为守卫，跳到 0 时停在 index 1；
  - `MultiUndoCommand` 原来不拥有子命令（析构泄漏），现析构删除
    全部 children；
  - `push_pre_executed` 原来不置 `done_` 标记，导致入栈后 undo
    为空操作（facade undo-group 的组撤销实际不生效），现入栈前
    `set_done(true)`（UndoCommand 新增 `set_done()`）。
- `clear()` 推入的占位命令名固定为字面量 "New/Open Project"（原
  `tr()` 翻译移除，翻译由 app 层负责）。
- `push/push_pre_executed` 的命令名由 `QString` 改为 `std::string`；
  C ABI 中 NULL name 视同空串。
- C ABI 句柄语义：`oakundo_command_init/init_multi` 返回的句柄拥有
  底层命令；push 或 `oakundo_command_multi_add_child` 成功后所有权
  转移、wrapper 被消费（不可再用/再 free）；
  `oakundo_command_multi_child` 返回 borrowed wrapper（free 只释放
  wrapper）。

## oaknode 去Qt化的删除与语义变更（2026-08-05）

`src/node/`（Node 族、Project 族、Sequence/Track/Block 族、serializer、
ColorManager）去Qt化过程中的删除与语义变化，迁移调用方时需注意
（详细约定见 `src/node/DEQT.md` §4/§7）：

- 信号槽整组删除：Node 的 28 个信号（label_changed、input_connected、
  keyframe_added、added_to_graph 等）、NodeKeyframe 的 5 个、
  Project/Folder/Sequence/Footage 的全部信号（name_changed、
  modified_changed、track_added、begin/end_insert_item 等），变更通知
  统一由 facade 经 `oakengine_event` 发出，oaknode 不持有上层回调。
- QObject 父子生命周期改显式所有权：Project 持有全部节点
  （`add_node`/`remove_node`，`clear()`/析构删除）；nodeundo 用
  `std::unique_ptr<Node>` 替代 `memory_manager_`；keyframe 经
  `Node::add_keyframe()/remove_keyframe()` 显式注册（自动 set_parent），
  gizmo 经 `Node::add_gizmo()/remove_gizmo()` 显式注册（构造不再自
  注册）；`NodeInputImmediate::delete_all_keyframes(reclaimed)` 替代
  "reparent 续命"。
- polygon/text 的 `generate_frame()` 光栅化（QPainter/QTextDocument）
  改后端钩子：`PathFillBackend`（geometry.h）、`TextMeasureBackend`/
  `TextRenderBackend`（textbackend.h），facade 未安装前输出空白
  （被迫行为差异）。
- gizmo 绘制与 hit-test 归 app 层：各 gizmo 的 `draw(QPainter*)`、
  `PointGizmo::get_clicking_rect()/get_drawing_rect()` 等删除；
  `DraggableGizmo` 的拖拽信号改直调 `parent_node()->gizmo_drag_*()`；
  drag 回调里的 `sender()` 由 `Node::current_gizmo()` 替代。
- footage 离线媒体警示帧：QImage/QPainter 改纯像素循环（深红底+斜纹），
  "Media Offline" 文字叠层与抗锯齿丢失（被迫行为差异）。
- `:/ocioconf` Qt 资源路径不可用：ColorManager 默认配置的 qrc 提取必然
  失败，需 `OCIO` 环境变量指向磁盘 config.ocio（测试用
  `engine/render/ocioconf/config.ocio`）。
- `NodeGroupAddInputPassthrough` 的上游 bug 原样保留（去Qt化不改行为
  逻辑，待单独裁决修复）。
- track.h 的 `QFontMetrics` 13px 常量：轨道默认高度原依赖
  `QFontMetrics(qApp->font()).height()`，去Qt后固化为 13px 字面量，
  换字体/DPI 不再自适应。
- `Footage::check_footage()` 的 QTimer 周期回调删除，函数本体保留为
  public 由 facade 周期调用；`qApp->activeWindow()` 门槛移到 app 层。
- 序列化 XML 元素/属性名与读写顺序逐字节不变；`XmlStreamWriter` 输出
  紧凑 XML（无声明无缩进）；OVEC 压缩段保持 qCompress 兼容
  （zlib + 4 字节大端长度）。
- 修复三个上游真实 bug（行为因此与旧版不同，特此记录）：
  `Project::clear()` 现重置 `root_`（clear 后可再次 initialize）；
  clear 不再先 `set_parent(nullptr)` 再 delete（否则 ~Node 的
  disconnect_edge 触发 parent 不等的 assert）；
  `Sequence` 新增析构删除三个 `TrackList`（原来构造 new 后泄漏）。
- `sender()` 的其他替代：原 keyframe signal→Node 的 5 条失效通知链
  （invalidate_from_keyframe_*）函数本体保留为 public 成员，带
  `NodeKeyframe *key` 显式参数，待 facade 接线。

## 信号与槽的处理策略（2026-08-05）

去Qt化对 Qt 信号槽的统一处理模式，M1–M3 一致执行，后续模块照此：

1. **模块内通知改显式回调/直调（std::function）**：通知双方都在同一
   模块内时，去掉 connect，改为 `std::function` 回调注册或成员直调。
   例：oakundo `UndoStack::index_changed(int)` signal →
   `set_index_changed_callback(std::function<void(int)>)`；oaknode
   `Node::current_gizmo()/set_current_gizmo()` 机制替代 gizmo drag
   回调里的 `sender()`；`DraggableGizmo` 的 handle_start/handle_movement
   信号改直调 `parent_node()->gizmo_drag_start()/gizmo_drag_move()`；
   TrackList 原 track_list_changed/length_changed 发射点直调
   `sequence->update_track_cache()/verify_length()`。
2. **跨层/跨界通知删除，由 facade 经 oakengine_event 统一发出**：
   一切指向 app/UI/其他模块的信号整组删除（Node 28 个、NodeKeyframe
   5 个、Project/Folder/Sequence/Footage 全部），oaknode 自身不持有
   任何上层回调；facade 在执行命令后经既有 `oakengine_event` 通道发
   通知（事件 id 沿用 `oakengine/events.h` 70-95 段，值不变）。依据：
   oaknode 的所有修改都经命令函数完成，调用方知道影响（M3 §2 特殊
   约定 2，04 §3）。
3. **QObject 父子生命周期改显式所有权**：`setParent`/childEvent 机制
   删除，所有权用显式容器与注册函数表达。例：Project 持有节点
   （`add_node`/`remove_node`/`clear()`）；`ColorManager` 由
   `std::unique_ptr` 持有；nodeundo 用 `std::unique_ptr<Node>` 替代
   `memory_manager_`；keyframe/gizmo 分别经 `add_keyframe()`/
   `add_gizmo()` 显式注册；UndoCommand 树由 MultiUndoCommand 析构
   删除 children。
4. **sender() 的替代**：两种形态——拖曳场景用
   `Node::current_gizmo()`（DraggableGizmo 直调回调前后
   `set_current_gizmo(this/nullptr)` 包一层）；通知链场景改显式参数，
   如 `invalidate_from_keyframe_*(NodeKeyframe *key)`。
5. **原 slot 函数本体保留为 public 成员，待 facade 接线**：signal 删了
   但 slot 承载的业务逻辑不删，改为 public 成员函数由上层直调。清单
   见 `src/node/DEQT.md` §7（如 `Sequence::update_track_cache()`、
   `Footage::check_footage()/default_color_space_changed()/
   proxy_ready()/proxy_finished()`、5 条 invalidate_from_keyframe_*）。

## oakrender 去Qt化的删除与语义变更（2026-08-05）

- `RenderTicket::finished` 不再跨线程补发：回调在调用 finish() 的
  线程上触发；未决状态由 facade 自查 `is_running()`。
- PreviewAutoCacher 延迟重排队改显式标志：单次 QTimer 重排队删除，
  `try_render()` 置 `delayed_requeue_pending_`，facade 在
  `requeue_delay_ms()` 后重调 `try_render()`。
- DiskCacheFolder 周期存盘由 GUI 线程 QTimer 改后台线程，数据保护
  用 `std::recursive_mutex`（原靠 QObject 线程亲和串行化）。
- PlaybackCache 的 Qt 信号改单回调注册：`set_invalidated_callback`/
  `set_validated_callback`/`set_requested_callback`/
  `set_cancel_all_callback`；跨层 invalidated/validated 通知由 facade
  在命令后重发（M7 §2.2：oakrender 不持有上层回调）。
- `DiskManager::instance()` 惰性自建（原 app 启动时显式
  create_instance；库消费者不经 facade 也能用）。
- FrameHashCache 帧缓存 JPEG 读写 QImage→OIIO/OpenEXR。
- AudioWaveformCache/PlaybackCache 的 QPainter 绘制归 app 层；
  缓存指示条高度固化为常量 4（`oakrender_cache_indicator_height()`，
  原 `QFontMetrics(QFont()).height()/4`）。
- 离屏 GL 上下文共享组需 app 显式传入（原 QOpenGLContext
  globalShareContext 隐式共享）。
- OpenGLContext/Vulkan 上下文抽象层（OpenGLContextProvider 等）替代
  QOpenGLContext 直用。
- WorkerProcess 仅 POSIX 实现（Windows 占位未做）。
- worker IPC 的 NDJSON 协议未变，workerjson 为本地（去 Qt）实现。
- `:/shaders` qrc 资源缺口：着色器源码的 Qt 资源路径在库形态下
  不可用，需 app 侧提供文件映射。
- 删除 ipc/ipcmessage.cpp 等 3 个过期 .cpp（对应头已自包含 inline，
  .cpp 内容失效）。
- displayinternal.h 分层违规（render→src/capi）并入 texturehandle.h。
- Renderer 线程亲和由 QObject::thread() 改显式 owner thread
  （`set_owner_thread_to_current()`/`called_on_owner_thread()`）。

## oaktimeline 去Qt化的删除与语义变更（2026-08-05）

- TimelineMarker/TimelineWorkArea 的信号（time_changed/name_changed/
  color_changed/enabled_changed/range_changed/marker_added/removed/
  modified）全部删除，变更通知由 facade 层在命令后发出；list 的重排由
  TimelineMarker::set_time 直调 parent_->resort(this)。
- TimelineMarker::draw()/get_marker_height()（QPainter/QFontMetrics）
  删除，归 app 层。
- TimelineMarkerList 的 QObject childEvent 自动注册改为显式
  add_marker(std::unique_ptr)/remove_marker 所有权转移；marker 构造
  不再带 parent 参数（oaknode 的 4 个 serializer 调用点已适配）。
- ViewerOutput 的 workarea_/markers_ 由 QObject 父子管理改为
  std::unique_ptr 持有。
- timeline 全部 undo 命令类改为持有 oaknode C 句柄
  （OakNodeTrack/OakNodeBlock/OakNodeSequence），图操作全部经
  oaknode C ABI（01 §0 铁律 6）；为此 oaknode C API 新增：
  copy_in_graph、clip_add_cache_passthrough_from、block_get_kind、
  block_as_node/from_node、track_as_node、nearest_block_before/after_or_at、
  command_create_remove_node/add_node/set_position_recursive、
  node_get_project、input_array_insert/remove、connect/disconnect_element、
  tracklist_get_sequence/track_input_id/array_append/array_remove_last/
  get_array_index_from_cache_index、sequence_as_node、node_get_markers/
  get_work_area（后两个为 oaktimeline 的借用出口）。
- UndoCommand 跨模块继承（oaktimeline 命令类继承 oakundo 的
  olive::UndoCommand）保留——undo 架构的既有跨模块模式，记录为
  01 §5 的明确例外（oaknode 的 nodeundo 同例）。
- **修复 de-Qt 断链**：Block 长度变化不再经信号通知 Track，改为
  Block::InputValueChangedEvent 里对 track_->block_length_changed(this)
  直调（原 Qt 版 connect 链，wave-2 遗留的 facade 接线项提前兑现）。
- 命令创建的 GapBlock 在插入 track 前必须先进项目图
  （block_add_to_graph/block_remove_from_graph helper，对齐原版
  setParent(graph)/setParent(memory) 语义）；孤立句柄用 orphan 标志
  跟踪，析构时 oaknode_block_free。
- oaknode↔oaktimeline 运行期互相解析（add_default_nodes →
  TimelineAddTrackCommand），双方 dylib dynamic_lookup，测试二进制
  必须同时链两个库（各 standalone 驱动已接线）。
- oak_core xml C API 新增 get_native 借用访问器（C++ only），
  oaktimeline 的 load/save 经它取回 XmlStreamReader/Writer。

## oakplugin 去Qt化的删除与语义变更（2026-08-05）

- olivehost/oliveplugininstance/oliveclip/paraminstance/image/
  pluginprogressreporter 六个类去Qt：QMessageBox/QApplication 全部
  删除，宿主消息与 undo 提交改为 facade 回调
  （set_host_message_handler / set_undo_submit_callback /
  ActiveViewerProvider / set_main_thread_id）。
- paraminstance 的参数桥改走 oaknode C API（node_ 为 OakNodeNode
  值句柄）；undo 命令走 oakundo C API；node↔instance 路由用新增的
  oaknode_node_identity() 注册表。
- oliveclip 的纹理改 OakRenderTexture 值句柄；为此 oakrender C API
  新增 texture is_dummy/get_frame/width/height/fb_format/
  wrap_native 与 copier/ticket 族。
- oaknode C API 新增 get_input_at_time/set_input_at_time_undoable
  (_into)/node_identity/sequence_from_node/
  sequence_set_default_parameters/find_input_footage。
- avframeptr.h 从 src/codec/src/ffmpeg/ 上移到 src/common/src/
  （codec 与 render 共用），transition 里的旧拷贝已删。
- 新 C ABI：include/plugin/{error,host,instance}.h，
  OakPluginInstance 为引用计数值句柄。
- oaknode 的 transition stub（src/node/transition/pluginSupport/
  {oliveplugininstance,olivehost}.h）改为桥接 oakplugin 真身头，
  liboaknode 因此引用 oakplugin 符号；凡构建 oaknode 的 standalone
  树（codec/audio/task/render/timeline）都必须
  add_subdirectory(src/plugin)，且测试二进制要链接 oakplugin，
  否则 dynamic_lookup 悬空符号 jump-to-0（EXC_BAD_ACCESS
  address=0x0）。
- 修复 ffmpegdecoder.cpp 被 3d004c081 误删出
  src/codec/src/ffmpeg/CMakeLists.txt 的回归：FFmpegDecoder 构造
  函数在 flat namespace 悬空，decoder probe/decode/encoder
  roundtrip 三个测试 jump-to-0。

## oaknode→oaktimeline 切到 C ABI（2026-08-07）

- ViewerOutput 不再持有 TimelineWorkArea/TimelineMarkerList 的 C++
  对象，改持 OakTimelineWorkArea/OakTimelineMarkerList 拥有型值句柄
  （oaktimeline_workarea_create/marker_list_create，析构时 free）；
  workarea_handle()/markers_handle() 返回借用副本（调用方不得
  free）。
- oaktimeline C API 新增：marker_list_create/marker_add/
  workarea_create/workarea_set_enabled；句柄 box 增加可选 deleter
  区分借用/拥有（timelinehandle.h）。
- oaknode_node_get_markers/get_work_area 改为经 out 参数返回
  addref 后的 OakTimeline* 句柄（include/node/node.h 只做具名
  struct 前置声明，公开 C 头互相 include 构成环，前置声明规避）；
  timeline/marker.h、timeline/workarea.h 的 typedef 改为具名
  struct。OakNodeMarkerList/OakNodeWorkArea 不透明指针类型删除。
- 4 个旧项目序列化器（210528/210907/211228/220403）的
  load_work_area/load_marker_list 改走 oaktimeline C API；
  ProjectSerializer::SerializedMarker POD 取代
  std::vector<TimelineMarker*>（LoadData::markers 与
  only_serialize_markers_），serializer230220 的 marker 读写直接
  展开为 XML 属性读写。**行为变化**：app 层 paste/markers 接口
  签名改变（app 尚未拆分，编译点留待 facade 阶段）。
- Sequence::add_default_nodes 的 TimelineAddTrackCommand 改为
  oaktimeline_add_track_command + oakundo_capi::to_command/
  mark_container_owned 转入 MultiUndoCommand。
- clip.cpp 的 Timeline::ThumbnailMode/WaveformMode 枚举改自新中立
  头 include/timeline/displaymode.h（OAK_TIMELINE_THUMBNAIL_*/
  OAK_TIMELINE_WAVEFORMS_*，值与 olive::Timeline 枚举保持兼容）。
- tracklist.h/block.h 里 timelinecommon.h 的死引用删除；
  src/node/transition/timeline/ 四个 stub 头删除。
- oak_core xml C API 新增 oak_core_xml_reader_wrap_native/
  oak_core_xml_writer_wrap_native（C++ only 借用包装），
  XmlReaderState/XmlWriterState 支持 owning/borrowed 双模式。
- liboaknode 对 liboaktimeline 的 C++ 符号引用降为 0（nm 验证）。

## oaknode→oakrender 切到 C ABI（2026-08-07）

- oakrender cache C API 大扩（include/render/cache.h）：
  cache_create_for_node(parent, kind)（四种缓存：视频帧/缩略图/音频
  播放/波形，parent 的原生回指针留在 oakrender 内部）、get_uuid
  （两段式）、request、load/save_state、set_saving_enabled、
  set_passthrough、get_passthroughs、get_valid_cache_filename、
  get_timebase、lock/unlock（替代直交 std::mutex）、
  invalidate_range（有理数版，原有 invalidate 是时间戳制）、
  get_native（C++ only，供 oakrender 内部的 PreviewAutoCacher 用）。
- Node 的四个缓存成员（video_cache_/thumbnail_cache_/audio_cache_/
  waveform_cache_）从原生指针改为 OakRenderCache 拥有型值句柄，
  访问器返回借用副本；构造/析构/拷贝 UUID/加载保存全部走 C ABI。
- ClipBlock 的 connected_video_cache()/thumbnails()/waveform() 等
  返回借用句柄；request/invalidate/passthrough 流程走 C ABI；
  passthrough 列表查询只传 range（原 Passthrough::cache 文本不出界）。
- oaknode_node_get_video_frame_cache 改为返回 addref 后的
  OakRenderCache（out 参数，node.h 前置声明 struct OakRenderCache）；
  OakNodeFrameCache 不透明指针类型删除；oaktask 的 precachetask
  直接收句柄，不再 wrap_borrowed；RenderTask::render/
  start_video_ticket 的 cache 参数与 oakrender_video_ticket_params.cache
  改为 OakRenderCache 值（借用语义，空 ctx=无）。
- 色彩：OCIOBaseNode 的 processor_ 改 OakColorProcessor 拥有型句柄
  （新增析构释放；OCIOLutNode 同理并补了 ~OCIOLutNode）；
  job.set_color_processor 经 C++-only
  oakrender_color_processor_get_native 取 shared_ptr。
  oakrender 新增 color_processor_create_transform/create_lut/
  create_grading_primary/get_native 与 lut_is_supported_extension/
  supported_extensions_count/at；oaknode 新增
  colormanager_wrap_borrowed/get_native（C++ only）。
  OCIO 的 FileTransform/GradingPrimaryTransform 构造全部内收到
  oakrender（grading log/linear、LUT 加载）。
- RenderManager::instance() 空检查 → oakrender_manager_available()；
  PreviewAutoCacher::cancel_video_tasks → oakrender_cancel_video_tasks；
  DiskManager 默认缓存路径 → 已有 oakrender_disk_cache_path。
- PreviewAutoCacher（oakrender 内部）改用 oakrender_cache_get_native
  解包节点缓存，回调接线保持模块内 C++。
- **例外（记录为 01 §5 例外）**：NodeValue 的纹理载荷
  TexturePtr=shared_ptr<olive::Texture> 经 Variant 类型擦除流经
  oaknode 的 35 个 TU，析构引用 olive::Texture::~Texture——值系统
  载荷问题，与 UndoCommand 跨模块继承同类，留待值系统重做。
  除此之外 liboaknode→liboakrender 的 C++ 符号引用为 0。

## ③ C ABI 接线冻结线（2026-08-07）

③（模块间调用切 C ABI）完成两整块后冻结，剩余部分改由各模块的
Rust 重写驱动（RIIR 后调用方只能走 C ABI，接线自动发生且被类型
强制做对）：

- 已切：oaknode→oaktimeline（81431d180）、oaknode→oakrender
  （5a564f30c，缓存体系/色彩/单例全部 C ABI 化）。
- olive::Variant 从 oaknode 下沉到 oak_core（src/common/src/
  variant.{h,cpp}）——它本是跨模块值类型。
- 冻结时点的残留（nm 可查）：
  - render→node 41 个 C++ 符号：ProjectCopier 深拷贝族、
    NodeTraverser（RenderProcessor 跨模块继承）、ColorManager、
    Footage/MultiCam/ViewerOutput 常量、oaknode_c_api::alive_*。
    → 在 oaknode/oakrender Rust 重写中消解（架构底稿：
    src/node/rust、src/render/rust；ProjectCopier 反转为
    oaknode_project_deep_copy/sync_copy，traverser 改 hook 制）。
  - node/render→plugin 的 OFX C++ 符号：随 M11（DeepSeek 实现中）
    落地消解。
  - 各模块→oak_core 的 XmlStreamReader/FileFunctions/VideoParams
    C++ 调用：随 oak_core Rust 化消解。
  - 文档化例外：Texture 的 Variant 载荷（Rust 侧不存在此问题）、
    UndoCommand 跨模块继承、oakgl2/oakvulkan 后端插件接口。
- ④（隐藏 C++ API）同步搁置：模块 Rust 化后 C++ 符号自然消失。

## oak_core Rust 测试：ffmpeg_bridge 符号依赖（2026-08-08）

`oak_core_ffmpegutils_get_compatible_bridge_pixel_format` 在非测试构建
中会经 `find_best_pix_fmt_of_list` 引用 ffmpeg_bridge 的
`fb_find_best_pix_fmt_of_list` 符号。集成测试二进制不链接 libffmpeg_bridge，
实验证实直接调用该 FFI 导出会在链接期报 `_fb_find_best_pix_fmt_of_list`
undefined symbol（cargo test --test link_exp 复现，随后已删）。

因此该函数不能走普通集成测试。处理方式（与 oakplugin/oaktimeline 的
test-stubs 约定一致）：

- `src/common/rust/Cargo.toml` 新增 `test-stubs` feature；
- `find_best_pix_fmt_of_list` 的 extern 声明/调用改为
  `#[cfg(all(not(test), not(feature = "test-stubs")))]`，桩改为
  `#[cfg(any(test, feature = "test-stubs"))]`——`cargo test --lib`
  无需 flag 仍走桩，集成测试 `cargo test --features test-stubs` 也可链接；
- `tests/ffi_ffmpegutils.rs` 覆盖 ffmpegutils 全部 6 个 FFI 导出
  （成功路径 + null out-param 的 E_INVALID 路径），需带
  `--features test-stubs` 运行；不带 flag 时该文件整体为空（cfg 门控）。

## oak_core Rust：ocioutils/oiioutils 吸收 oakoci（2026-08-08 接手笔记）

### 基线

`src/common/rust` 全量测试：`cargo test --features test-stubs` **497 个全部通过**
（314 lib + 12 contract + 8 ffi_colortransform + 61 ffi_commandlineparser +
24 ffi_config + 8 ffi_ffmpegutils + 21 ffi_misc + 12 ffi_subtitleparams +
15 ffi_videoparams + 22 ffi_xmlutils + 集成 real_ocio 等）。TODO 中旧快照
"476"已过时；后续验收以"保持 497+ 全绿"为准。

### oakoci shim crate 的拆解去向

`src/bindings/oakoci/`（untracked，纯 Rust shim，曾用于给
oakrender Rust 提供 OCIO/图像能力）内容已全部内收进
`src/common/rust`，然后整目录删除：

| oakoci 内容 | 去向 | 落点 |
|---|---|---|
| `oci.rs` OcioConfig / OcioProcessor（包 `ocio_rs::Config`/`CPUProcessor`） | 重写为 `ocioutils.rs` 的 `OcioConfig`/`OcioProcessor`，错误从消息包装 `Error` 改为 `crate::error::Error::Failed`（新增 `From<ocio_rs::OcioError>`） | `src/common/rust/src/ocioutils.rs` |
| `image.rs` F32Image + read/write_image_f32（image 0.25 tiff） | 原样并入 `oiioutils.rs`，错误统一走 `Error::Failed` | `src/common/rust/src/oiioutils.rs` |
| `error.rs` 消息包装 Error | 并入 `error.rs`（`Error::new` 便捷构造） | `src/common/rust/src/error.rs` |
| `tests/smoke.rs`（5 OCIO + 1 TIF round-trip） | 移植为 `tests/real_ocio.rs`，config 用 `env!("CARGO_MANIFEST_DIR")/../../../engine/render/ocioconf/config.ocio`，断言改为 `Error::Failed(_)` | `src/common/rust/tests/real_ocio.rs` |

### 位深判别值（已验证）

`ocio_rs::BitDepth` 是 `#[repr(i32)]`，判别值即 OCIO 码：Unknown=0,
Uint8=1, Uint10=2, Uint16=5, F16=7, F32=8。`ocioutils.rs` 直接
`depth as i32`，单测锁死 1/2/5/7/8/0，与 C++ 的
`OCIOUtils::get_ocio_bit_depth_from_pixel_format`（CPP-PARITY）一致。

### 决策：不引入 ffmpeg-next（偏差，已落定）

TODO 原计划用 `ffmpeg-next` 的 `av_d2q` 做 aspect-ratio 换算。复查发现
C++ 侧 `Rational::from_double`（`core/src/util/rational.cpp:39`）本身就是
**av_d2q 的移植**（文件内注释 "ported from FFmpeg's av_d2q"），oakcore-rs
的 `Rational::from_double` 与之对等，且现有 12 个单测已锁死行为
（NaN/超界→(0,0)、0.0→(0,1)、约分）。因此**保留手写移植，走
`oakcore_rs::Rational::from_double`**，不把 ffmpeg-sys-next（bindgen +
系统 FFmpeg）拖进叶子 crate——守住 README 的 "narrow extern C" 纪律。
`oiioutils.rs` 的实现已如此；文档残留的 "via ffmpeg-next" 说法待清。

### OCIO 环境依赖

`ocio-sys` 不自探测系统 OCIO：无配置时构建的是 stub bridge（调用全失败）。
`src/common/rust/.cargo/config.toml`（从 oakoci 抄入）固定三个变量：
`OCIO_RS_ENABLE_REAL=1`、`OCIO_INSTALL_DIR=/opt/homebrew`、
`OCIO_RS_LINK=dynamic`。机器上 OpenColorIO 2.5.2（Homebrew）、
libavutil 60.26.102、~/.cargo/registry 已缓存 ocio-rs/ocio-sys/
image/ffmpeg-next 全套。

### 待办（已全部结清，2026-08-10 核查）

1-4 均已完成：common 497+ 绿（89.8% 覆盖）、oiioutils 文档已改为
oakcore-rs 手写移植、src/bindings/oakoci 已删除、tarpaulin ≥80% 达标。

## 技术债登记（2026-08-09）

- **oaktask 渲染循环同步化 ✅（2026-08-09 修复）**：src/task/rust/src/render.rs
  的同步循环（"一帧一 ticket、wait 到底"）已改为并发循环，与 C++
  原版一致：最多 `max_inflight`（默认 `available_parallelism`，照 C++
  `max(1, hardware_concurrency)`）个 ticket 在飞，ticket 完成回调
  （`oakrender_ticket_finished_fn`）把完成的 ticket 推进完成队列并唤醒
  渲染线程（队列 + condvar）；乱序完成经 reorder buffer 按时间戳序
  （音频先、帧按时间升序）交付给 behavior 钩子；取消/钩子错误会
  cancel+wait 全部在飞 ticket（其完成回调仍然恰好触发一次）后再返回。
  可观察契约不变。配套：tests/common/mod.rs 的 ticket stub 升级为
  模拟 ticket arena（按提交序分配 id、可脚本化乱序完成 `stub_complete`、
  DEFER 模式、取消原子会完成在飞 ticket）；新增
  tests/render_loop_test.rs（乱序交付、音频优先、取消 drain、进度单调、
  错误停止派发、窗口化）。bridge/render.rs 的
  `oakrender_ticket_finished_fn` 由 3 参修正为 2 参
  `(ticket, userdata)`，与 include/render/ticket.h 及 oakrender 实现一致
  （原 3 参镜像与实际 ABI 不符）。
  遗留注意：ticket.h 规定回调收到的 ticket 是"提交者句柄的借用副本，
  提交者持有并释放"，因此渲染循环只释放 submit 函数返回的那一份句柄、
  不释放队列里的借用副本（C++ 原版对两者都调 free，若真实 oakrender 的
  句柄副本不各自计数，则 C++ 路径存在双释放风险，Rust 侧按头文件契约
  规避）；ticket.h 无 poll/try_wait 查询，等待完全走完成回调 + condvar。
- **oaktask ↔ 真实 oakrender ticket ABI 接线 ✅（2026-08-09）**：
  oaktask 的 `bridge/render.rs` 保持 link-time `extern "C"`（与
  `bridge/codec.rs` 同模式）：`cargo test` 由 tests/common/mod.rs 的
  `#[no_mangle]` stub 满足链接，真实 `liboakrender` 在场时（app 链接模块
  dylib）解析到真实导出。新增 `--features real-oakrender` +
  `--test render_real_integration_test`：把 oakrender crate 作为可选 path
  dep 链接进同一测试二进制（feature 关闭时完全不编译），驱动
  `RenderTask::render` 走真实 arena 的 CPU 路径（`eval::render_produced_frame`
  生成 F32 帧、无 GPU），帧按时间戳序交付（64×64、0/1→1/1→2/1），并断言
  结束后 `oakrender::handle::alive_count()` 回到基线（ticket/帧/取消原子的
  句柄全部释放）。feature 开启时 tests/common/mod.rs 的 render stub 整段
  `#[cfg(not(feature = "real-oakrender"))]` 编译掉（与真实导出的
  `#[no_mangle]` 符号会冲突，故必须带 `--test` 过滤单独构建）。
  **句柄契约裁定**：真实 oakrender 的句柄副本**不各自计数**——`
  oakrender_ticket_render_frame` 只 `make_owned` 一份 `TicketBox`（refs=1），
  回调闭包捕获的副本与 submit 返回值共享同一 RefBox/同一计数，与头文件
  "回调收到借用副本、提交者持有并释放" 的契约一致；oaktask 渲染循环只释放
  submit 返回的那一份（且 `wait_idle()` 保证所有回调先触发完再释放，队列
  里的借用副本从不释放、也从不 deref 到已释放的 box），无双释放/悬垂，
  **无需修改任务侧**。C++ 原版"两者都 free"的双释放风险随 render 侧
  Rust 化不复存在。另修复 oakrender ffi.rs 一处潜在竞态：`TicketBox.id`
  原先在 `submit_video` 之后才写回，快 worker 可能抢先完成 ticket 并回调
  （回调收到的句柄 id 仍为占位 0，`classify_ticket` 会报 unexpected
  timestamp）；现改为 `arena.next_id()` 预分配 + `submit_video_with_id`
  在 post 前盖好 id（ticket.rs 新增 `next_id`/`submit_video_with_id`/
  `submit_audio_with_id`，`submit_video`/`submit_audio` 签名不变）。
  另修正 bridge/render.rs 的 `oakrender_color_processor_create` 声明：
  5 参（旧镜像）→ 3 参 `(src_space, dst_transform, direction)`，与
  include/render/color.h 及 oakrender 导出一致。
- **oaktask 导出缺口（仍开放）**：临时文件重命名（失败不留半成品）与
  sidecar 字幕编码器未实现；precache 缺项目深拷贝。
- **oaknode GPU 求值接缝（仍开放）**：43 个节点行为的 GPU 路径
  （shader/generate/color-transform job）目前推空纹理占位，
  待 traverser::RenderHooks::resolve 接 oakrender 后变真；
  OCIO 节点依赖 oakrender 色彩处理器桥；text 节点的字体后端未定。
- **ocio-sys 构建依赖 GitHub**：bundled 模式要拉 sse2neon，网络差时
  构建失败（重试可过）；上游构建脚本成功后 cargo 会缓存。

## engine/ vs src/ 模块覆盖比对（2026-08-09）

对照 engine/include/oakengine/*.h（facade 头）与 include/<mod>/*.h
（模块 C ABI）的缺口：

**模块侧真实缺口（要补）**：
- oaknode：NodeInputDragger 族（oakengine_dragger_*，6 函数）未迁；
  keyframe 辅助 4 个（compute_paste_value/get_valid_bezier_point/
  has_sibling_at_time/opposing_bezier_type）未迁；MultiCam 族
  （oakengine_multicam_*，9 函数）未迁（oakrender 只有
  set_cacher_multicam 一个入口）。
- oakcodec：格式/编解码器枚举族（oakengine_encoding_format_* /
  codec_*，12 函数）未迁——oakcodec 只有
  export_format_get_extension 一个。
- oaktimeline/oaknode：engine/timeline.h 的 sequence/clip 便捷层
  （add_footage_clip/ripple_delete_*/move_clip/trim 等 ~90 函数）
  大部分是 facade 便捷封装，底层原语（edit 命令族）已齐，facade
  包装时按需下沉或留在 facade。

**属于 facade 层（不进模块，属预期）**：viewer/playback/preview/
display/gizmo/app/worker/ipc/config/disk/lut/events/exporter ——
即 M9 §4 裁决的 liboakengine 装配层职责。

**已核对无缺口**：undo（39 vs 26 系 facade 组合函数）、audio、
group passthrough、keyframe 主体、project/footage 主体。
