# oaktask coverage map

Maps every C++ task class/method in `src/task/src` to its Rust home in this
crate. The C++ code remains the parity source of truth; `// CPP-PARITY:`
markers in the Rust files point at the exact C++ file. Review this map before
touching a task class so you rewrite the right module.

| C++ class / file (`src/task/src/...`)      | Rust home                                   |
|--------------------------------------------|---------------------------------------------|
| `task.h` — `olive::Task` base              | `src/task.rs`                               |
| `taskmanager.h` — `TaskManager`            | `src/manager.rs`                            |
| `codecbridge.h` — submitter registration   | `src/codecbridge.rs` + `src/bridge/codec.rs`|
| `conform/conform.h` — `ConformTask`        | `src/conform.rs`                            |
| `customcache/customcachetask.h`            | `src/customcache.rs`                        |
| `export/export.h` — `ExportTask`           | `src/export.rs`                             |
| `precache/precachetask.h` — `PreCacheTask` | `src/precache.rs`                           |
| `proxy/proxy.h` — `ProxyTask`              | `src/proxy.rs`                              |
| `render/render.h` — `RenderTask` base      | `src/render.rs`                             |
| `project/import/import.h`                  | `src/project/import.rs`                     |
| `project/load/load.h`                      | `src/project/load.rs`                       |
| interchange-format dispatch (task-side)    | `src/project/format.rs`                     |
| `project/loadotio/loadotio.h`              | `src/project/loadotio.rs`                   |
| `project/save/save.h`                      | `src/project/save.rs`                       |
| `project/saveotio/saveotio.h`              | `src/project/saveotio.rs`                   |

## Parity / golden tests

| Golden source                                 | Rust test                                  |
|-----------------------------------------------|--------------------------------------------|
| `ConformTask::derive_filenames` (conform.h)    | `tests/parity_test.rs::conform_derive_filenames` |
| `ProxyTask::build_arguments` (proxy.h)         | `tests/parity_test.rs::proxy_build_arguments` |
| `ProxyTask::parse_progress` (proxy.h)          | `tests/parity_test.rs::proxy_parse_progress` |
| `src/task/tests/task_test.cpp` (manager/task)  | `tests/manager_test.rs`, `tests/ffi_contract_test.rs` |
| `include/task/*.h` (C ABI contract)            | `tests/ffi_contract_test.rs`               |
| `loadotio.cpp` (OTIO -> project)           | `tests/otio_test.rs::otio_load_*`, `fcpxml_load_*` (synthetic documents built with `oakotio`) |
| `saveotio.cpp` (project -> OTIO)           | `tests/otio_test.rs::otio_save_*`, `fcpxml_save_*` (exports re-parsed with `oakotio`) |
| extension dispatch (`.otio`/`.fcpxml`)     | `tests/otio_test.rs::otio_load_extension_dispatch_matrix` (+ unknown-extension failures on both tasks) |

## Couplings handled through other modules' C ABIs

The task module never reimplements another oak module. Every cross-module
call is a declared `extern "C"` import in `src/bridge/`, mirroring the frozen
headers verbatim:

- **oakrender**: `bridge/render.rs` — cancelatom (`OakCancelAtom`), tickets
  (`OakRenderTicket`, `oakrender_ticket_*`), project copier
  (`OakRenderProjectCopier`), color processor (`OakColorProcessor`), frame
  cache (`OakRenderCache`).
- **oakcodec**: `bridge/codec.rs` — encoder/decoder (`OakEncoder`/`OakDecoder`),
  frames (`OakFrame`), task submitter (`oakcodec_set_task_submit_cb`,
  `OakCodecTaskRequest`), proxy params (`oakcodec_proxy_params`).
- **oaknode**: `bridge/node.rs` — project/footage/folder/sequence/colormanager/
  node handles for the project tasks.
- **oakundo**: `bridge/undo.rs` — `OakUndoCommand`.
- **oak_core / oakcore**: `bridge/common.rs` — `OakVideoParams`,
  `OakColorTransform`, `OakAudioParams`.

## Deliberately out of scope

- **Export/precache render details** — the C++ render loop is a concurrent
  ticket pool; the Rust `render.rs` is a simplified synchronous loop (one
  ticket at a time, `wait()`ed). The per-frame observable contract
  (ordered `frame_downloaded`/`audio_downloaded`, progress, cancellation)
  is preserved. The export task additionally skips the temporary-file
  rename dance and the sidecar subtitle encoder.

`export.cpp.pending` / `export.h.pending` in `src/task/src/export/` are
in-progress variants; `src/export.rs` is mapped to the canonical `export.h`.

OpenTimelineIO was previously out of scope (OTIO parsing stayed in the C++
impl); since `oakotio` landed it is covered: `LoadOTIOTask`/`SaveOTIOTask`
parse/serialize through `oakotio` (see README decision #6) and the project
graph still moves across the oaknode/oaktimeline C ABIs only. The tasks
are format-aware: `src/project/format.rs` dispatches `.otio` (OpenTimelineIO
JSON) vs `.fcpxml` (FCPXML) from the filename extension (case-insensitive)
at the parse/serialize call; the track/clip/footage code is shared, and
the C ABI is unchanged (the format parameter is the filename itself).

## Test strategy

- `tests/common/mod.rs` provides `#[no_mangle]` stubs for every extern C
  symbol the crate imports (test binaries link the rlib without the real
  module DLLs). Each stub family exposes `set_*` controls so tests drive
  both the success and the failure path. The oakrender ticket stubs are
  compiled out when the `real-oakrender` feature is on.
- `tests/manager_test.rs` drives the codec submitter end-to-end: a real
  shell script stands in for ffmpeg (proxy success + 3 failure modes), and
  conform success/failure/cancellation are exercised through real file
  renames.
- `tests/render_real_integration_test.rs` (feature `real-oakrender` +
  `--test` filter; links the real oakrender crate) drives
  `RenderTask::render` against the actual ticket arena — frames arrive in
  timestamp order through the CPU path, no GPU.
- Every exported `oaktask_*` symbol has at least one success and one
  failure-path test (`tests/ffi_contract_test.rs`,
  `tests/project_task_test.rs`).

## Coverage (cargo tarpaulin, 2026-08)

`86.53%` line coverage (1850/2138). Per-file (lines covered/total):

| File                     | Covered |
|--------------------------|---------|
| src/bridge/node.rs       | 4/4     |
| src/bridge/render.rs     | 29/36   |
| src/codecbridge.rs       | 49/49   |
| src/conform.rs           | 72/79   |
| src/customcache.rs       | 43/45   |
| src/error.rs             | 4/8     |
| src/export.rs            | 104/113 |
| src/ffi/manager.rs       | 25/27   |
| src/ffi/project.rs       | 173/180 |
| src/ffi/task.rs          | 94/98   |
| src/ffi/taskhandle.rs    | 44/46   |
| src/handle.rs            | 29/56   |
| src/manager.rs           | 67/89   |
| src/precache.rs          | 33/43   |
| src/project/format.rs    | 8/8     |
| src/project/import.rs    | 184/235 |
| src/project/load.rs      | 36/51   |
| src/project/loadotio.rs  | 201/224 |
| src/project/save.rs      | 28/41   |
| src/project/saveotio.rs  | 196/220 |
| src/proxy.rs             | 114/141 |
| src/render.rs            | 215/242 |
| src/task.rs              | 98/103  |

The remaining gaps are the panic-guard helpers (`handle.rs`, exercised only
through `#[no_mangle]` exports that return codes directly), the
load/save serializer code-map alternatives, and a few import/manager edge
branches — all reachable but not individually asserted.

The concurrent render loop (src/render.rs) is exercised both through the
export run-path tests and the dedicated concurrency suite
(`tests/render_loop_test.rs`: scrambled completion order, audio-first
ordering, cancel drain, progress monotonicity, error stop, windowing).
