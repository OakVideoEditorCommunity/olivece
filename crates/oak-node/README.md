# oaknode Rust crate (implementation)

> Status: **all FFI headers implemented**. Phase 1 (core engine: graph
> arena, values, keyframes, project, factory, ~55 FFI exports), Phase 2
> (sequence/track/block/footage/colormanager + traverser + serializer,
> the folder/group/keyframe/dragger FFI families, the undo/XML bridges
> with test stubs, and the contract tests) and Phase 3 (the multicam
> grid family and the deferred bridge exports: markers/work-area/frame
> cache accessors, viewer params, sequence/footage stream params via
> the videoparams/audioparams C ABIs, and the colormanager compliant
> transform) are complete; `cargo test --features test-stubs` is green
> (84 tests, 1 ignored byte-exact golden). The remaining `todo!()`s are
> the concrete node-type behaviors under `src/nodes/` (registered in
> the factory, bodies deferred) — the multicam node behavior and the
> effect/generator nodes. The crate template (FFI discipline, testing
> layers) follows `crates/oakplugin/README.md`.

## Scope

Replaces the C++ oaknode module (`src/node/src`, ~40k lines):
the node graph engine, project/folder/sequence/track/block hierarchy,
footage, color manager, keyframes, evaluation (traverser), project
serialization, and the undo bridge.

Public contract: `include/node/*.h` (14 headers, ~280 functions) —
frozen, implemented verbatim by `src/ffi.rs`.

## Key architectural decisions (C++ → Rust mapping)

1. **Inheritance → arena + trait objects.** The C++ design is deep
   inheritance (`Node` → `ViewerOutput`/`Track`/`Block`/… and ~50
   effect nodes). Rust: a slab-allocated `Graph` arena of
   `NodeEntry { core: NodeCore, behavior: Box<dyn NodeBehavior> }`,
   addressed by generational `NodeId`. No reference cycles exist by
   construction (edges are IDs, not pointers).
2. **Cross-module inheritance disappears.** C++ `RenderProcessor :
   NodeTraverser` (render subclassing a node class) becomes a plain
   evaluation API: `traverser::evaluate(...) -> NodeValueTable` is a
   function, and oakrender supplies backend hooks via a trait
   (`RenderHooks`) instead of overriding virtuals.
3. **Value system.** `olive::Variant`/type-erasure becomes a closed
   `NodeValue` enum (`value.rs`). C ABI marshalling lives only in
   `ffi.rs`.
4. **Undo.** Commands are created through the oakundo C ABI
   (`bridge::undo`); the C++ `UndoCommand` subclass hierarchy becomes
   vtable commands whose userdata is a Rust closure.
5. **Serialization.** XML read/write goes through the oak_core C ABI
   (`bridge::common`) until oak_core itself is rewritten.
6. **Threading.** The C++ code relied on Qt's event thread +
   `called_on_owner_thread()` assertions. Rust replaces this with
   `Mutex<Graph>` interior mutability plus explicit
   `&mut Graph` phases for structural edits; the threading contract is
   documented per function.

## Layout

`COVERAGE.md` maps every method of the C++ `olive::Node` (260
declaration lines, ~150 unique methods) to its Rust home — trait /
core / graph / ops / bridge / drop-with-reason. Review that first.

```
src/
  lib.rs         crate doc + module map
  error.rs       error codes (mirrors include/node/error.h)
  handle.rs      refcounted-handle scaffolding (same pattern as plugin)
  value.rs       NodeValue / NodeValueTable / ValueHint
  id.rs          NodeId, generational arena ids
  node.rs        NodeCore + NodeBehavior trait (the virtual surface)
  graph.rs       Graph arena, edges, topological order
  input.rs       Input descriptors, flags, array inputs, hints
  keyframe.rs    NodeKeyframe + track interpolation
  project.rs     Project, settings, folder tree
  sequence.rs    Sequence (ViewerOutput equivalent)
  track.rs       Track, TrackList
  block.rs       Block/ClipBlock/GapBlock/TransitionBlock
  footage.rs     Footage (probe via oakcodec C ABI)
  colormanager.rs ColorManager (OCIO via oakrender C ABI for now)
  traverser.rs   Evaluation engine (iterative, hook-based)
  serializer.rs  XML project load/save (bridge::common)
  factory.rs     Node type registry (id -> constructor)
  nodes/         The concrete built-in node types
  bridge/        C ABI imports: common.rs, undo.rs, render.rs, codec.rs
  ffi.rs         include/node/*.h export layer
tests/           contract + golden tests (see README test section)
```

## Hard rules for the implementer

1. Every `extern "C"` body goes through `handle::guard*`; no panic
   crosses FFI.
2. `Graph` is the only owner of nodes; the public API never hands out
   references into the arena, only `NodeId`-carrying handles.
3. Behavior parity with C++ is proven by the C ABI test-suite
   (`src/node/tests`, unchanged) plus the golden tests in `tests/`.
4. Where C++ behavior is genuinely load-bearing but ugly (e.g.
   `Block` length-change side effects on `Track`), port the behavior,
   not the aesthetics; leave a `// CPP-PARITY:` comment with the C++
   file:line.

## Dependency policy

Prefer mature third-party crates (MIT/Apache-2.0/BSD, GPL-compatible)
over hand-rolling; register each addition (name + reason) here. Large
existing C++ libraries (OTIO, OCIO, OIIO, FFmpeg) are NEVER rewritten
— they are consumed through their C ABI / bridge layers.
