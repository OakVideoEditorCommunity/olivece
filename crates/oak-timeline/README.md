# oaktimeline Rust crate (declaration draft, for review)

> Status: **declaration draft**. Signatures + doc comments are the
> spec; every body is `todo!()`. Not wired into any build.
> The crate template (FFI discipline, testing layers) follows
> `crates/oakplugin/README.md` and `crates/oaknode/README.md`.

## Scope

Replaces the C++ oaktimeline module (`src/timeline/src`): timeline
markers and work areas, the timeline undo-command family (add/remove
tracks, place/trim/split blocks, ripple edits, slide, gap insertion),
and the shared `Timeline` namespace / utility helpers.

Public contract: `include/timeline/*.h` (`error.h`, `displaymode.h`,
`marker.h`, `workarea.h`, `edit.h`) — frozen, implemented verbatim by
`src/ffi.rs`. The timeline value handles (`OakTimelineMarkerList`,
`OakTimelineWorkArea`) and every edit command are exported through this
ABI only; consumers (the facade/app, the oaknode crate) never see the
internal Rust types.

## Key architectural decisions (C++ → Rust mapping)

1. **Domain modules mirror the C++ header files.** The C++ module is a
   flat set of headers, not a deep class hierarchy, so the Rust crate
   keeps one module per C++ header family (`common`, `marker`,
   `workarea`, `undocommon`, `undotrack`, `undogeneral`, `undopointer`,
   `undoripple`, `undosplit`, `util`). `COVERAGE.md` maps every C++ type
   to its Rust home; review that first.

2. **No C++ `UndoCommand` subclass hierarchy.** Following the oaknode
   crate decision (#4), each undo command is a plain Rust struct that
   exposes `prepare()` / `redo()` / `undo()` (all `todo!()` here) and
   is surfaced to the world through the oakundo C ABI vtable
   (`bridge::undo::oakundo_command_init`, Rust callbacks as `userdata`).
   Every command struct carries a `to_command()` factory that wraps it
   into an `oakundo::undocommand::UndoCommand` value for the undo stack.

3. **Value types come from `oakcore-rs`.** Markers and work areas are
   built on `Rational`/`TimeRange`, so the crate depends on
   `oakcore-rs` (`crates/oakcore`) exactly like oaknode does; no
   pixel/sample formats are involved here.

4. **All cross-module access goes through the C ABI.** Per the project
   rule (no cross-module C++ member calls), timeline commands touch the
   node graph exclusively through the oaknode C ABI
   (`bridge::node`), undo through the oakundo C ABI (`bridge::undo`),
   and XML/config through the oak_core C ABI (`bridge::common`). The
   C++ internal helpers `oakundo_capi::make_command_handle` /
   `oaknode_c_api::to_native` are **not** replicated in Rust — their
   role is subsumed by vtable commands and by value handles treated as
   opaque.

5. **Handles.** `handle.rs` keeps the `RefBox`/`CHandle` scaffolding
   for the oakengine facade boundary (the C ABI export layer is the only
   place left that talks handles). Every `make_owned` value handle boxes
   an `Arc<Mutex<T>>` — the same pattern as the oaknode project handles —
   so the crate's commands hold the shared marker list / work area as a
   plain Rust `Arc<Mutex<…>>` and the facade entries convert the handle
   back to the `Arc` at the boundary.

## Layout

```
src/
  lib.rs         crate doc + module map
  error.rs       error codes (mirrors include/timeline/error.h)
  handle.rs      refcounted-handle scaffolding (same pattern as node)
  common.rs      Timeline namespace (MovementMode/ThumbnailMode/
                 WaveformMode, EditToInfo) — timelinecommon.h
  marker.rs      TimelineMarker/MarkerList + 5 marker commands
  workarea.rs    TimelineWorkArea + 2 workarea commands
  undocommon.rs  node/block remove helpers + MultiUndoCommand
  undotrack.rs   track ripple/prepend/insert-after/replace commands
  undogeneral.rs resize/media-in/add/remove-track/transition/gap/
                 enable-disable/insert-gaps/default-transition commands
  undopointer.rs BlockTrimCommand/TrackSlideCommand/TrackPlaceBlockCommand
  undoripple.rs  ripple remove-area / ripple-tool / delete-gaps commands
  undosplit.rs   BlockSplitCommand/BlockSplitPreservingLinksCommand/
                 TrackSplitAtTimeCommand
  util.rs        timelineutil.h inline helpers (rat_nd, same_*,
                 free_detached_handle, block/track queries)
  bridge/        C ABI imports: node.rs, undo.rs, common.rs
  ffi.rs         include/timeline/*.h export layer
tests/           contract + golden tests (see README test section)
```

## Hard rules for the implementer

1. Every `extern "C"` body goes through `handle::guard*`; no panic
   crosses FFI.
2. Timeline objects never outlive their owning node; borrowed handles
   are created under a guard that owns the node reference.
3. Behavior parity with C++ is proven by the C ABI test-suite
   (`src/timeline/tests`, unchanged) plus the golden tests in `tests/`
   (XML save/load formats captured verbatim from
   `src/timeline/src/timelinemarker.cpp` / `timelineworkarea.cpp`).
4. Where C++ behavior is genuinely load-bearing but ugly (e.g. marker
   list kept sorted by time, ripple's compensation gap rules), port the
   behavior, not the aesthetics; leave a `// CPP-PARITY:` comment with
   the C++ file:line.

## Dependency policy

Prefer mature third-party crates (MIT/Apache-2.0/BSD, GPL-compatible)
over hand-rolling; register each addition (name + reason) here. Large
existing C++ libraries (OTIO, OCIO, OIIO, FFmpeg) are NEVER rewritten
— they are consumed through their C ABI / bridge layers.
