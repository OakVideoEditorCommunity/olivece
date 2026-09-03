# M4 · oaktimeline 拆分手册

> 内容：`engine/timeline/`（TimelineMarker/TimelineMarkerList/
> TimelineWorkArea/timelinecommon/timeline undo 命令族：
> timelineundogeneral/timelineundopointer/timelineundoripple/
> timelineundosplit/timelineundotrack/timelineundoworkarea）。
> 依赖：node 32（重）、common 4、config 2、undo 1、core 3。
> 拆分顺序第 4 位。

## 1. 目标形态

```
oaktimeline/
  include/oaktimeline/{marker.h, workarea.h, edit.h, types.h, export.h}
  src/
  tests/  # oaktimeline_gtest
```

timeline 的 undo 命令类**不出模块**（01 §5）：消费侧经
`edit.h` 的语义函数或 `_command` 工厂拿基类 `OakUndoCommand *`
（oakundo 句柄）组进自己的 MultiUndoCommand。

## 2. 冻结 C API

### 2.1 `oaktimeline/marker.h`

```c
typedef struct OakTimelineMarkerList OakTimelineMarkerList;
/* list 句柄为 borrowed（属 Sequence/Clip 所有），不需要 free */
OAKTL_API OakTimelineMarkerList *oaktimeline_marker_list_of(
	OakNodeNode *owner /* sequence 或 clip 句柄 */);

OAKTL_API int oaktimeline_marker_count(const OakTimelineMarkerList *l);
OAKTL_API int oaktimeline_marker_at(const OakTimelineMarkerList *l, int i,
	int64_t *in_ts, int64_t *out_ts, int *color, char *name_buf, int n);
OAKTL_API int oaktimeline_marker_add(OakTimelineMarkerList *l,
	int64_t in_ts, int64_t out_ts, const char *name, int color,
	OakUndoCommand *command);    /* command=NULL 时自行入栈（2026-08：void* → 有类型句柄） */
OAKTL_API int oaktimeline_marker_remove_at(OakTimelineMarkerList *l,
	int i, OakUndoCommand *command);
OAKTL_API int oaktimeline_marker_set_time(OakTimelineMarkerList *l,
	int i, int64_t in_ts, int64_t out_ts, OakUndoCommand *command);
OAKTL_API int oaktimeline_marker_set_props(OakTimelineMarkerList *l,
	int i, int color, const char *name, OakUndoCommand *command);
/* 无事件接口（2026-08 修订，04 §3）：marker 的增删改都是调用方发的
 * 命令，MARKER_ADDED/REMOVED/MODIFIED 通知由调用方所在层（facade，
 * id 沿用 oakengine events 段）在命令后发出。 */
```

### 2.2 `oaktimeline/workarea.h`

```c
typedef struct OakTimelineWorkarea OakTimelineWorkarea;
OAKTL_API int oaktimeline_workarea_get(const OakTimelineWorkarea *w,
	int64_t *in_ts, int64_t *out_ts, int *enabled);
OAKTL_API int oaktimeline_workarea_set_range(OakTimelineWorkarea *w,
	int64_t in_ts, int64_t out_ts);                    /* live */
OAKTL_API int oaktimeline_workarea_set_range_undoable(
	OakTimelineWorkarea *w, int64_t in_ts, int64_t out_ts,
	int64_t old_in_ts, int64_t old_out_ts, OakUndoCommand *command);
OAKTL_API int oaktimeline_workarea_set_enabled_undoable(
	OakTimelineWorkarea *w, int enabled, OakUndoCommand *command);
OAKTL_API void oaktimeline_workarea_reset(int64_t *in_ts, int64_t *out_ts);
/* load/save 经 oak_core_xml 句柄在 oaknode 序列化路径调用 */
OAKTL_API int oaktimeline_workarea_load(OakTimelineWorkarea *w,
	oak_coreXmlReader *r);
OAKTL_API int oaktimeline_workarea_save(const OakTimelineWorkarea *w,
	oak_coreXmlWriter *x);
```

### 2.3 `oaktimeline/edit.h`（timeline undo 命令族的语义入口）

照 `oakengine/timeline.h` 的 timeline 编辑原语逐一对齐（那本来就是
这一族的 facade 版，签名模板直接搬）：

```c
OAKTL_API void *oaktimeline_add_track_command(OakNodeTrackList *list);
OAKTL_API void *oaktimeline_remove_track_command(OakNodeTrackList *list,
	int index);
OAKTL_API void *oaktimeline_place_block_command(OakNodeTrackList *list,
	int track_index, OakNodeBlock *block, int64_t in_ts);
OAKTL_API void *oaktimeline_replace_block_with_gap_command(
	OakNodeTrack *track, OakNodeBlock *block, int64_t in_ts);
OAKTL_API void *oaktimeline_trim_command(OakNodeTrack *track,
	OakNodeBlock *block, int64_t point_ts, int trim_in);
OAKTL_API void *oaktimeline_split_command(OakNodeBlock *const *blocks,
	int count, int64_t point_ts);   /* preserving links 变体加 _links */
OAKTL_API void *oaktimeline_ripple_delete_gaps_command(
	OakNodeSequence *seq, const int64_t *in_ts, const int64_t *out_ts,
	const int *track_types, const int *track_indexes, int range_count);
OAKTL_API void *oaktimeline_slide_command(OakNodeTrack *track,
	OakNodeBlock *block, int track_delta, int64_t time_delta_ts);
OAKTL_API int64_t oaktimeline_nearest_block_ts(OakNodeTrack *track,
	int64_t ts, int direction /* -1/0/1 */);
```

## 3. 切割点（timeline → node 32 次）

全部经 oaknode C ABI + 适配类（01 §2）：

- `node/output/track/track.h`（5）、`tracklist.h`（3）：timeline 内部
  对 Track 的操作换 `OakNodeTrack*` 适配。
- `node/block/gap/gap.h`（4）、`transition.h`（4）：Gap/Transition 的
  构造换 `oaknode_factory_create_from_id`。
- `node/project.h`（3）：Project 适配类。

## 4. 测试（映射 03 §2/§3）

- marker：增删改查、undo 往返（push 后 undo 恢复）；每命令后读
  count/at 断言生效（无事件——通知在 facade 层测）。
- workarea：set/get、undoable 旧值恢复、reset 哨兵、xml 往返。
- edit：每个 `_command` 工厂 1 个"构造→入栈→undo 还原"用例
  （track 增删、place/replace/trim/split/ripple/slide）。
- `oaktimeline_debug_alive_count()` 泄漏断言。

## 实施现状（2026-08-05）

- 目录结构：`src/timeline/{src,c_api,tests,standalone}` +
  `include/timeline/{error,marker,workarea,edit}.h`。
- 构建测试：`cmake -S src/timeline/standalone -B build-oaktimeline &&
  cmake --build build-oaktimeline -j && ctest --test-dir
  build-oaktimeline`——117/117（本模块 21 用例 + oaknode 回归 96）。
  全量回归：oak_core 193、oaknode 96、oakrender 42、oakcodec 18、
  oakaudio 36 全绿。
- C API 与 §2 冻结表的差异：marker/workarea 的增删改统一为
  `_command` 工厂形态（返回 OakUndoCommand，调用方 redo/push），
  workarea set_range_command 需要调用方提供旧值（facade 知情原则）；
  edit.h 的 `_command` 工厂按 §2.3 落地并补 insert_gaps/
  ripple_remove_area；`oaktimeline_marker_list_of/workarea_of` 经
  oaknode 新增的借用出口（oaknode_node_get_markers/get_work_area）
  实现。marker list 无 free（borrowed，M4 §2.1 既定）。
- 与计划的差异：undo 命令类未走"语义函数 + 适配类"，而是命令类整体
  留在模块内、成员全部换成 oaknode C 句柄直调 C ABI（01 §0 铁律 6
  的直接执行）；UndoCommand 跨模块继承保留为例外（notes.md 有记录）。
- oaknode 侧适配：transition/timeline/* 由 stub 改为桥接真身头；
  viewer 的 workarea/markers 改 unique_ptr；4 个 serializer 的 marker
  构造点适配显式 add_marker。
- 已知问题：无（de-Qt 断链的 block→track 长度通知已在本轮修复）。
