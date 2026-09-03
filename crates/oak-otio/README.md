# oakotio

Pure-Rust OpenTimelineIO JSON binding for the Oak Video Editor's Rust
rewrite. This crate is a self-contained serde model of the OTIO JSON format,
covering exactly the object graph Oak's project load/save tasks use
(`src/task/src/project/loadotio/loadotio.cpp` and
`src/task/src/project/saveotio/saveotio.cpp`): `RationalTime`, `TimeRange`,
`Clip`, `Gap`, `Transition`, `Track`, `Stack`, `Timeline`,
`ExternalReference`, `MissingReference` and `SerializableCollection`.

The writer reproduces the opentimelineio C++ writer's output byte for byte
(4-space indentation, `": "` separators, inline empty objects and arrays,
shortest float representation, no trailing newline); the reader tolerates
hand-written files, preserving unknown fields verbatim across a round-trip
and defaulting missing fields.

The crate also ships an **FCPXML** (Final Cut Pro X `.fcpxml`) interchange
layer (`src/fcpxml.rs`) that maps the FCP X document format onto the *same*
model types, so an importer/exporter can offer `.fcpxml` alongside `.otio`
with a single object graph. See [FCPXML](#fcpxml) below.

Part of the Oak `src/bindings/` family (siblings: `oakaudioout`).
This is an **rlib** — nothing is exported dynamically.

## Structure

```
crates/oakotio/
├── Cargo.toml            # rlib; deps: serde, serde_json, quick-xml (crates.io),
│                         #   oakcore-rs (path); dev-dep: oakcore-rs (path)
├── src/
│   ├── lib.rs            # crate docs + module wiring + re-exports + from_json_* entry points
│   ├── error.rs          # OtioError (Json | Io) + Result<T>
│   ├── model.rs          # serde structs for the 10 OTIO schemas + value types +
│   │                     #   Rational::from_double port + in-crate unit tests
│   └── fcpxml.rs         # FCPXML reader/writer (quick-xml) mapped onto model types +
│                         #   FcpxmlError + in-crate unit tests
└── tests/
    ├── data/             # C++-writer golden files (golden_timeline.json,
    │                     #   golden_collection.json, golden_typed_transition.json,
    │                     #   floatfmt.json)
    ├── parity.rs         # read parity: every golden file parses and round-trips
    ├── semantic.rs       # semantic checks over golden_timeline.json (what the
    │                     #   C++ load task reads back)
    ├── save_parity.rs    # save parity: a built Timeline serializes byte-identical
    │                     #   to golden_timeline.json and re-parses identically
    └── fcpxml.rs         # FCPXML: synthetic-document parse, model round-trip,
                          #   NTSC precision, error paths, leniency
```

## API summary

- `from_json_string(&str) -> Result<Serializable>` /
  `from_json_file(path) -> Result<Serializable>` — parse a document whose root
  is a `Timeline`, a `SerializableCollection`, or an unknown schema kept whole
  as `Serializable::Raw`.
- `RationalTime` — `new(value, rate)` (C++ argument order), `value`, `rate`,
  `to_seconds`, `is_invalid_time`, `invalid_time`, `rescaled_to`,
  `to_rational`, `from_rational`.
- `TimeRange` — `new(start_time, duration)` (C++ argument order), `start_time`,
  `duration`.
- `Clip` — `new(name)`, `name`, `source_range`, `set_source_range`,
  `media_reference`, `media_references`, `set_media_reference`.
- `Gap` — `new(source_range, name)`, `name`, `source_range`.
- `Transition` — `new(name)`, `name`, `in_offset`, `out_offset`,
  `transition_type`, `set_in_offset`, `set_out_offset`.
- `Track` — `new(kind)`, `kind`, `children`, `append_child`.
- `Stack` — `children`, `append_child`.
- `Timeline` — `new(name)`, `name`, `tracks`, `tracks_mut`,
  `global_start_time`, `to_json_string`, `to_json_file`.
- `SerializableCollection` — `new(name, children)`, `name`, `children`,
  `to_json_string`, `to_json_file`.
- `MediaReference` / `Composable` / `Serializable` enums — downcasts
  (`as_clip`, `as_track`, ...) and `schema_name` for dynamic dispatch by
  `OTIO_SCHEMA`.

All fallible operations return `Result<T, OtioError>`.

### FCPXML API (`fcpxml` module)

- `from_fcpxml_string(&str) -> Result<Vec<Timeline>, FcpxmlError>` /
  `from_fcpxml_file(path)` — parse an FCPXML document; one `Timeline` per
  `<sequence>`.
- `to_fcpxml_string(&[Timeline]) -> Result<String, FcpxmlError>` /
  `to_fcpxml_file(&[Timeline], path)` — serialize timelines to an FCPXML
  1.10 document (formats and assets deduplicated across timelines).
- `FcpxmlError` — `Xml` (malformed markup), `Malformed` (wrong root,
  missing attributes, unknown format resources, bad time values),
  `UnsupportedVersion`, `Io`.

The FCPXML layer reads through the same model types as the OTIO layer:
`Timeline`/`Track`/`Clip`/`Gap`/`Transition` with `RationalTime` time
values, so a single object graph feeds both `.otio` and `.fcpxml`
import/export.

## Backend choice: hand-written serde over the `opentimelineio` crate

| Option | Verdict |
| --- | --- |
| crates.io `opentimelineio` | Not viable: the crate is unmaintained, binds the C++ library via FFI (large, ABI-fragile), and does not build a pure-Rust model Oak's load/save tasks can read directly. No actively maintained pure-Rust OTIO implementation exists on crates.io. |
| **This crate: `serde` + `serde_json`** | Pure-Rust (no C++ runtime), fully controllable field order and formatting, preserves unknown fields for forward compatibility, and ports the only piece of C++ numeric behavior Oak needs (`Rational::from_double`) on top of `oakcore_rs::Rational`. |

The C++ side serializes with `opentimelineio::schema::Timeline::to_json_string`
(4-space pretty formatter); this crate reproduces that exact writer with a
`serde_json::PrettyFormatter` (`with_indent(b"    ")`), `preserve_order` maps
so insertion order is kept, and ryu float formatting, which is what the C++
writer (rapidjson) emits. The result is byte-for-byte parity with C++-written
files (verified against the golden files).

## Dependency registry

Runtime dependencies (crates.io):

- `serde` 1 (with `derive`) — (de)serialization for the OTIO schema structs.
- `serde_json` 1 (with `preserve_order`) — JSON codec; `preserve_order` keeps
  map insertion order so metadata and unknown fields round-trip in file
  order.
- `quick-xml` 0.41 — streaming XML codec for the FCPXML layer
  (`src/fcpxml.rs`). Same major version the other Rust modules use
  (`crates/oak_core/Cargo.toml`).
- `oakcore-rs` (path: `../../oakcore-rs`) — shared `Rational` value type
  (used by the `Rational::from_double` port and for exact FCPXML
  rational-time conversion); same path dependency the other bindings use.

Dev-dependencies (tests only):

- `oakcore-rs` (path: `../../oakcore-rs`) — exact `Rational` comparisons in
  `tests/fcpxml.rs`.

Build and test:

```sh
cd crates/oakotio
cargo build
cargo test
```

## C++ parity notes

The C++ anchors this crate reproduces:

- **`Rational::from_double`** (`core/src/util/rational.cpp`) — ported in
  `model.rs` on top of `oakcore_rs::Rational::new` (which applies the exact
  C++ `reduce_fraction(INT_MAX)` reduction). NaN and out-of-range magnitudes
  collapse to the null sentinel `Rational::NULL`; the retry pass against
  `INT64_MAX` fires for tiny magnitudes and is itself reduced back to 0/1 by
  the `INT_MAX` ceiling, matching the C++ result.
- **Writer format** — `opentimelineio::schema::Timeline::to_json_string`:
  4-space indentation, `": "` separators, inline empty `{}`/`[]`, shortest
  float representation (ryu = rapidjson), no trailing newline. Golden files
  written by the C++ writer round-trip byte-identically.
- **Field order** — struct field order matches the C++ writer's output order
  (e.g. `RationalTime`: `rate` then `value`; `TimeRange`: `duration` then
  `start_time`; `Track`: `children` then `kind`).
- **`media_references`** — the C++ `Clip` stores a `std::map<string, ...>`;
  this crate uses `BTreeMap`, which serializes keys in the same sorted order.
- **Missing-reference serialization** — `MissingReference` writes
  `available_range`/`available_image_bounds` as `null` and omits `target_url`,
  exactly like the C++ writer.

## Deviations from the C++ code (deliberate)

- **Unknown fields are kept, not dropped** — the C++ reader discards
  unrecognized JSON fields; this crate preserves them (via `#[serde(flatten)]`
  catch-all maps) so a document written by a newer opentimelineio still
  round-trips. This is a superset of the C++ behavior.
- **Defaults are lenient** — missing fields deserialize to their type's
  default (the C++ `AnyDictionary` fill defaults), so hand-written files
  without optional fields parse cleanly.
- **`RationalTime`/`TimeRange` are `Clone`, not `Copy`** — they carry a
  `String` schema field, so value accessors (`value()`, `rate()`,
  `duration()`, ...) take `&self` and return clones; the C++ value semantics
  (`to_seconds`, `rescaled_to`) are unaffected.

## FCPXML

The `fcpxml` module reads and writes Final Cut Pro X's `.fcpxml`
interchange format (the version-1.x XML documents produced by FCP X and,
with some tolerance, by DaVinci Resolve). It maps the FCPXML structure onto
the same model types as the OTIO layer:

| FCPXML element | Model mapping |
| --- | --- |
| `<fcpxml>` root `version` | validated (1.0 – 1.11); recorded in timeline metadata |
| `<resources>`/`<format>` | frame rate (from `frameDuration`) |
| `<resources>`/`<asset>` | `ExternalReference` (`src` → `target_url`, `duration` → `available_range`) |
| `<library>`/`<event>`/`<project>` | timeline name + `metadata["fcpxml"]` (event/project name, version) |
| `<sequence>` | `Timeline` (`tcStart` → `global_start_time`, `tcFormat`/`audioLayout`/`audioRate` → metadata) |
| `<spine>` | `Track` kind "Video" (the primary storyline) |
| secondary `<video>` / `<audio>` | additional `Track`s (kind "Video"/"Audio", nested lanes flattened) |
| `<asset-clip>` | `Clip` (`offset`+`duration`+`start` → `source_range`, `ref` → media reference, `enabled` preserved) |
| `<gap>` | `Gap` |
| `<transition>` | `Transition` with `in_offset == out_offset == duration/2` (FCP X centers its transitions) |

**Time values.** FCPXML times are rational seconds ("100/3000s",
"0s", "1001/30000s", ...). They are parsed into `oakcore_rs::Rational` and
converted to/from `RationalTime` with exact rational arithmetic, so NTSC
rates (30000/1001, 60000/1001, 24000/1001) stay frame-accurate in both
directions — a 3-frame clip at 29.97fps round-trips as exactly 3 frames.
The reader also tolerates integer seconds ("48s"), bare rationals
("1/24") and decimal seconds ("1.5s").

**Leniency.** Unknown elements and attributes are skipped with a log line
(`eprintln!`) instead of failing, so real-world documents from other NLEs
import without crashing. Unmapped *timed* blocks (`<sync-clip>`,
`<title>`, `<generator>`, ...) are imported as `Gap`s that preserve their
timing; dangling `ref`s become `MissingReference`s. Hard errors are
reserved for genuinely broken documents: non-FCPXML roots, unsupported or
missing `version`, a `<sequence>` referencing an unknown format, and
unparseable time values.

**Writer output.** `to_fcpxml_string` emits FCPXML 1.10 with 2-space
indentation (the FCP X convention): `<?xml?>`, `<!DOCTYPE fcpxml>`,
`<resources>` with `<format>`s first then `<asset>`s (both deduplicated,
formats by frame duration and assets by `src`), one `<event>`/`<project>`/
`<sequence>` group per timeline, the first video track as `<spine>`, the
rest as `<video>`/`<audio>` elements. Interchange hints stored in
`metadata["fcpxml"]` (event/project names, `tcFormat`, `audioLayout`,
`audioRate`) are reproduced on export.

### FCPXML deviations (deliberate)

- **Reduced time spellings** — the writer emits reduced rational seconds
  ("48s", "1/30s", "1001/30000s") instead of FCP X's scaled "value/rate"
  spellings ("1440/30s"). Semantically identical.
- **`<format>` dimensions** — the OTIO model carries no frame size, so
  exported formats use `width="1920" height="1080"` (name
  "FFVideoFormat1920x1080p<rate>"). Importers derive the rate from
  `frameDuration`, which is exact.
- **Audio components** — a clip's embedded audio components are not split
  out; the spine keeps only the video track, audio lives in `<audio>`
  elements. `audioStart`/`audioDuration`/`audioOffset` are not written.
- **`<sync-clip>`/`<title>`/`<generator>`** — imported as timing-preserving
  gaps (their content is not mapped to a model type).
- **Track names** — FCPXML has no track names; imported tracks are unnamed.
- **Transitions** — FCP X transitions are centered, so the importer sets
  `in_offset == out_offset == duration/2`. Asymmetric transitions from
  other tools are approximated this way.
- **Missing media** — clips without an `ExternalReference` are written as
  `asset-clip` elements without a `ref` (technically schema-invalid but
  tolerated by importers; logged).
- **Unknown resource types** (`<effect>`, `<filter>`, ...) are skipped on
  import and not written on export.

## Scope

Covers only what `loadotio.cpp` / `saveotio.cpp` touch, plus the FCPXML
interchange layer. No media-resolution, no `Marker`/`Effect` schemas (kept
as raw `Value` for round-tripping), and no plugin API — `src/plugin/` is
intentionally untouched. OTIO JSON behavior is unchanged by the FCPXML
layer (it only adds public accessors: `Clip::enabled`/`set_enabled`,
`Timeline::metadata`/`metadata_mut`/`set_global_start_time`).
