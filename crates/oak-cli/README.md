# oak-cli (Rust)

Headless command-line consumer of the oak editor module crates — the Rust
rewrite of `cli/main.cpp` (which stays in the tree until cutover). Same
subcommands, same output format, same exit codes:

| exit | meaning |
|---|---|
| 0 | success |
| 1 | general error (bad project/media file, no sequence, I/O failure) |
| 2 | rendering unavailable or failed (e.g. no render backend) |
| 64 | usage error |

## Build and test

```sh
cargo build --release      # binary: target/release/oak-cli
cargo test                 # unit + integration tests
```

The crate is **self-contained** (M14 R2): it links the oak* module rlibs
directly (`oaknode`, `oaktimeline`, `oakcodec`, `oakrender`, `oaktask`,
`oak_core`) — no `liboakengine` dylib, no C ABI, no build.rs link step.
`cargo test -p oak-cli` stands alone.

## Subcommands

Every subcommand of the C++ original is implemented:

```
oak-cli info <project.ove> <start> <end> <out_dir>   project name/sequences/footage
oak-cli render <project.ove> <start_seconds> <end_seconds> <out_dir>
oak-cli probe <mediafile>
oak-cli transcode <input_media> <out> [width] [--format ppm|mp4]
```

Argument validation is faithful to the C++ (`invalid start seconds`,
`invalid width`, `unknown --format` … all exit 64). The output formatters
(`src/fmt.rs`) reproduce the C++ `printf` output byte for byte and are
golden-tested against the output captured from the C++ binary on the test
fixtures (`tests/project_with_footage.ove`, `tests/demo.mp4`); the PPM and
WAV writers (`src/ppm.rs`, `src/wav.rs`) are the exact ports of the C++
`write_ppm`/`write_wav` and are unit-tested.

## Layout

```
src/
  main.rs     clap surface, --help/-h + unknown-command handling, dispatch
  engine.rs   module-native assembly layer (M14 R2): project load/create,
              footage probe, sequence + clip assembly, montage resolution,
              ticket rendering, synchronous export
  fmt.rs      golden output formatters (info/probe)
  ppm.rs      P6 PPM writer (f32/u8 frames)
  wav.rs      PCM s16 WAV writer (interleaved float samples)
  cmd/        per-subcommand validation + module-crate calls
tests/cli.rs  binary-level tests (exit codes, messages, usage errors)
```
