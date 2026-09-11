# Build Guide

This document describes how to build the Oak Video Editor from source on
macOS, Linux, and Windows. For the Chinese version see
[`zh/build.md`](zh/build.md).

> **2026 note:** Oak is a pure Rust workspace. `cargo build` at the
> repository root produces the app (`oak-editor`), the CLI (`oak-cli`)
> and the render worker (`oak-worker`). The old C++/CMake tree lives on
> the `cpp-legacy` branch; nothing in this guide uses it.

---

## Prerequisites (all platforms)

- **git** — clone with submodules (`gpui/` is a submodule):
  ```sh
  git clone --recursive https://github.com/OakVideoEditorCommunity/oak.git
  cd oak
  # or, on an existing clone: git submodule update --init --recursive
  ```
- **Rust stable** (via [rustup](https://rustup.rs/); on Windows use the
  MSYS2 toolchain instead — see the Windows section).
- **C toolchain + cmake + pkg-config + nasm** — cmake and a C++ compiler
  are needed by the vendored OpenColorIO build (Linux/macOS), nasm by
  the FFmpeg assembly.
- **FFmpeg 8.1, built by the project script.** Distro packages are too
  old for `ffmpeg-next` 9 and are deliberately not used:
  ```sh
  tooling/install-deps.sh        # codec/filter libraries + build tools
  tooling/ffmpeg/build-ffmpeg.sh # clones release/8.1, installs into .cache/ffmpeg
  ```
  `FFMPEG_DIR` does not need exporting: the committed
  `.cargo/config.toml` sets it relative to the workspace root
  (`ffmpeg-sys-next` cannot read `.env` at build-script time — the
  config entry is the only machine-agnostic way). The `oak-ffmpeg-link`
  build script panics without it; run `build-ffmpeg.sh` once before the
  first `cargo build`.
- **CI/CD uses the vcpkg manifest instead of `build-ffmpeg.sh`.** Every
  desktop job runs `vcpkg install --triplet <x64-windows | x64-linux |
  arm64-osx | arm64-linux>` against the repo-root `vcpkg.json`, which
  pins FFmpeg to **9.0.1#1** (`overrides`) and every other port through
  `builtin-baseline`; the tree lands in `vcpkg_installed/<triplet>` and
  jobs point `FFMPEG_DIR`/`PKG_CONFIG_PATH` at it. The C/C++ libraries
  outside that set (OCIO is vendored, the rest come from the system
  package manager) are unchanged. The local-build path above remains
  valid and is what the docs below describe.

## Quick start (macOS / Linux)

```sh
tooling/install-deps.sh         # Homebrew / apt / dnf / pacman
tooling/ffmpeg/build-ffmpeg.sh  # ~10–20 min, once
cargo build --workspace
cargo test  --workspace         # Linux: see "headless tests" below
```

---

## macOS

- macOS 12+, Xcode Command Line Tools (`xcode-select --install`), Homebrew.
- ```sh
  brew install cmake pkg-config
  tooling/install-deps.sh
  tooling/ffmpeg/build-ffmpeg.sh
  cargo build --workspace
  cargo test  --workspace
  ```
- OpenColorIO is compiled from the vendored 2.5.2 sources and linked
  statically — no `brew install opencolorio` needed (cmake is required).
- The GPU-gated tests (OFX GL overlay, hardware decode) run only with
  `OAK_GPU_TESTS=1`.

## Linux

- Debian/Ubuntu, Fedora and Arch are supported by
  `tooling/install-deps.sh`. Additionally install:
  ```sh
  # Debian/Ubuntu
  sudo apt-get install -y cmake \
    libpipewire-0.3-dev libspa-0.2-dev libjack-jackd2-dev \
    libasound2-dev libpulse-dev libsndfile1-dev \
    libgl1-mesa-dev libvulkan-dev libxkbcommon-dev libxkbcommon-x11-dev
  ```
  (the PipeWire/JACK/ALSA/PulseAudio/sndfile dev packages are cpal's
  audio backends; GL/Vulkan/XKB are the wgpu windowing stack).
- **Headless tests:** several gpui/UI tests open real windows through
  wgpu on Mesa's software Vulkan. Under a display-less session run:
  ```sh
  sudo apt-get install -y xvfb mesa-vulkan-drivers
  xvfb-run -a -s "-screen 0 1920x1080x24" cargo test --workspace
  ```
- OpenColorIO is the vendored static build, as on macOS.

## Windows (MSYS2 UCRT64)

> **CI/CD note**: the GitHub Windows CI/CD no longer uses this path — it
> builds MSVC-ABI on `warp-windows-2025-vs2026-x64-16x` with vcpkg
> manifest mode (`vcpkg.json` at the repo root: FFmpeg 9.0.1 with every
> free codec + hwaccel, pkgconf, librsvg, all pinned by
> `overrides`/`builtin-baseline`) and the vendored static OCIO. The
> MSYS2 flow below remains the documented local-build alternative.

The Windows build targets **x86_64-pc-windows-gnu** with MSYS2's own
Rust; the MSVC toolchain is not supported (the build scripts emit
Unix-style link args the MSVC linker rejects).

1. Install [MSYS2](https://www.msys2.org/) and open the **UCRT64** shell.
2. ```sh
   pacman -Syu
   pacman -S --needed mingw-w64-ucrt-x86_64-rust \
     mingw-w64-ucrt-x86_64-cmake mingw-w64-ucrt-x86_64-opencolorio
   tooling/install-deps.sh        # must run inside the UCRT64 shell
   tooling/ffmpeg/build-ffmpeg.sh
   ```
3. Environment (put in your shell rc or export per session):
   ```sh
   # The vendored OCIO sources contain MSVC-only constructs, so Windows
   # links MSYS2's OpenColorIO 2.5.2 dynamically instead:
   export OCIO_RS_ENABLE_REAL=1 OCIO_INSTALL_DIR=/ucrt64 OCIO_RS_LINK=dynamic
   # mingw-w64 (Nov 2025+) forwards _assert to __msvcrt_assert inside
   # libmingwex.a while rustc's link order leaves -lmingwex last; the
   # trailing -lmsvcrt re-scans the CRT import library afterwards
   # (otherwise: undefined _fileno/_setmode/__imp___msvcrt_assert).
   export RUSTFLAGS="-C link-args=-lmsvcrt"
   # If your shell inherits the MSVC INCLUDE/LIB (some CI runners inject
   # them into every step), clear them — they poison the MinGW compiles:
   unset INCLUDE LIB
   ```
4. ```sh
   cargo build --workspace
   cargo test  --workspace
   ```

---

## OpenColorIO summary

| Platform | Source | Linkage | Notes |
|----------|--------|---------|-------|
| Linux / macOS | vendored 2.5.2 (`ocio-sys` `bundled` feature, on by default) | static | needs cmake + C++ compiler |
| Windows | MSYS2 `mingw-w64-ucrt-x86_64-opencolorio` | dynamic | set `OCIO_INSTALL_DIR=/ucrt64`, `OCIO_RS_LINK=dynamic` |

Without the `bundled` feature and without `OCIO_RS_ENABLE_REAL=1`,
`ocio-sys` builds a stub and every colour test early-returns. The
bundled feature is enabled unconditionally by `oak-render`, so a plain
`cargo build` always gets the real thing on Linux/macOS.

## Packaging

Distribution packages are built in containers (no host dependencies
beyond Docker/Podman):

```sh
tooling/package/build-deb.sh  # Debian 12  → .deb
tooling/package/build-rpm.sh  # Fedora 41  → .rpm
tooling/package/build-pkg.sh  # Arch Linux → .pkg.tar.zst
```

All runtime dependencies are declared in the respective package metadata
(`packaging/`, `tooling/package/oak.spec`, `tooling/package/PKGBUILD`) —
the packages are self-contained except for the documented system
libraries; see `docs/project-storage.md` for what lands where.

## Troubleshooting

- **`oak-ffmpeg-link` panics about `FFMPEG_DIR`** — run
  `tooling/ffmpeg/build-ffmpeg.sh` once; it installs into
  `.cache/ffmpeg`, which `.cargo/config.toml` points at.
- **IDE builds fail (RustRover etc.)** — IDEs that cannot inject
  environment variables into cargo can read a git-ignored `.env` at the
  workspace root with `FFMPEG_DIR=...` (and `PKG_CONFIG_PATH=...` if
  your codec libraries live in a custom prefix).
- **pacman stalls with "Operation too slow"** — MSYS2 mirrors hiccup;
  `install-deps.sh` retries three times, re-running it resumes via
  `--needed`.
- **Windows: `undefined reference to _fileno/_setmode/__imp___msvcrt_assert`**
  — set `RUSTFLAGS="-C link-args=-lmsvcrt"` (see the Windows section).
- **Windows: `AddInstanceForFactory: No factory registered` / MSVC-flavoured
  errors** — you are on the MSVC Rust toolchain; switch to MSYS2's Rust
  (`x86_64-pc-windows-gnu`).
- **Empty `gpui/` directory** — `git submodule update --init --recursive`.
- **Linux tests open windows and hang/fail** — use the `xvfb-run` line
  from the Linux section.
