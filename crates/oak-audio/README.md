# oakaudio Rust crate

> Status: **implemented**. The C ABI (`include/audio/*.h`) is implemented
> by `src/ffi.rs`; the contract suite lives in `tests/` (all green,
> ~88% line coverage under tarpaulin). The architecture below mirrors the
> oaknode/oakrender crate template (FFI discipline, testing layers) from
> `crates/oaknode/README.md` and `crates/oakrender/README.md`.

## Scope

Replaces the C++ oakaudio module (`src/audio/src`, ~50k lines): the
PortAudio output/input manager (`AudioManager`), the real-time
resampler/format converter (`AudioProcessor`), timeline synchronization
helpers (`AudioSynchronizer`, `AudioWaveformSync`), the level meter
(`AudioLevelMeter`), the visual waveform store (`AudioVisualWaveform`),
the header-only pull buffer (`PreviewAudioDevice`), and the config
bridge (`audio_config` namespace).

Public contract: `include/audio/*.h` (5 headers plus `error.h`, ~45
functions) — frozen, implemented verbatim by `src/ffi.rs`.

## Key architectural decisions (C++ → Rust mapping)

1. **Singleton manager.** `AudioManager` is a process-wide PortAudio
   singleton. Rust keeps the singleton behind a `OnceLock<Mutex<...>>`
   with borrow-only handles: `addref`/`release` are no-ops exactly as on
   the C++ side, and an empty handle reports `OAKAUDIO_E_STATE`. No
   destruction ever happens through the handle.
2. **Processor is the only heavy FFI consumer.** `AudioProcessor` wraps
   the ffmpeg_bridge audio filter graph (`fb_audio_graph_*`,
   `fb_frame_*`); every call funnels through `bridge::ffmpeg`. The
   resampler/format-conversion semantics and the always-planar-f32
   output (`OAKAUDIO_PROCESSOR_OUTPUT_FORMAT = 4`) are preserved.
3. **Sync helpers are stateless.** `AudioSynchronizer` and
   `AudioWaveformSync` have only static methods in C++; they become
   plain functions in `synchronizer.rs` / `waveformsync.rs`. No handles
   are involved on the sync headers except by-value arguments.
4. **Value types are local.** `params.rs` defines `AudioParams` (a
   plain POD) and a **planar-first** `SampleFormat` enum mirroring
   `olive::core::SampleFormat::Format` exactly, because these values
   cross the C ABI as `int`. See the note in `params.rs` about why the
   crate does not reuse `oakcore-rs`'s `SampleFormat`.
5. **Rational reuses oakcore-rs.** `core::Rational` (used by
   synchronizer and waveform) comes from `oakcore-rs`; there is no
   local copy.
6. **Record path through oakcodec.** `AudioManager` records through the
   oakcodec encoder C ABI (`bridge::codec`) and waveform extraction
   decodes through the oakcodec decoder C ABI — exactly as the C++
   does. No direct ffmpeg_bridge use in the record path.
7. **Config via oak_core.** Device names and the output buffer size
   read through `bridge::common` (`oak_core_config_*`), preserving the
   `audio_config` namespace semantics as a `config.rs` free-function
   module.

## Layout

`COVERAGE.md` maps every C++ audio class/method to its Rust home.
Review that first.

```
src/
  lib.rs         crate doc + module map
  error.rs       error codes (mirrors include/audio/error.h)
  handle.rs      refcounted-handle scaffolding (same pattern as node)
  params.rs      AudioParams + planar-first SampleFormat value types
  config.rs      audio_config namespace (bridge::common)
  manager.rs     AudioManager singleton (PortAudio I/O, recording)
  processor.rs   AudioProcessor (resampler/converter, bridge::ffmpeg)
  synchronizer.rs AudioSynchronizer placement helpers
  levelmeter.rs  AudioLevelMeter peak/RMS/VU/LUFS analysis
  waveform.rs    AudioVisualWaveform mipmapped store + extraction
  waveformsync.rs AudioWaveformSync envelope offset estimation
  previewdevice.rs PreviewAudioDevice pull buffer
  bridge/        C ABI imports: common.rs, codec.rs, ffmpeg.rs
  ffi.rs         include/audio/*.h export layer
tests/           contract + golden tests (see README test section)
```

## Hard rules for the implementer

1. Every `extern "C"` body goes through `handle::guard*`; no panic
   crosses FFI. The manager's borrow-only singleton is the one place
   `guard_handle`/`guard_void` are used with no refcount semantics.
2. `SampleFormat` and `AudioParams` integer values MUST match the C++
   enums bit-for-bit; `// CPP-PARITY:` comments mark every load-bearing
   layout decision.
3. Behavior parity with C++ is proven by the C ABI test-suite
   (`src/audio/tests`, unchanged) plus the golden tests in `tests/`
   (waveform mipmap/channel-interleaved layout, RMS/LUFS thresholds,
   sync placement).
4. Where C++ behavior is genuinely load-bearing but ugly, port the
   behavior, not the aesthetics; leave a `// CPP-PARITY:` comment with
   the C++ file:line.
5. `src/plugin/` is frozen and out of scope; no oakaudio code reaches
   into it.

## Dependency policy

Prefer mature third-party crates (MIT/Apache-2.0/BSD, GPL-compatible)
over hand-rolling; register each addition (name + reason) here. Large
existing C++ libraries (OTIO, OCIO, OIIO, FFmpeg) are NEVER rewritten
— they are consumed through their C ABI / bridge layers.
