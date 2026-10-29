# PhotoCraft on air/OS

This independent fork of PhotoCraft 0.17.0 (upstream `69b8fd9c14b2`) runs the full
Rust editor on air/OS / Haiku x86_64. It retains the document engine, layers,
brushes, masks, adjustments, codecs, colour management, 8/16/32-bit documents,
recovery, preferences and authenticated automation. The native Be API host,
input mapping and software rasterizer reuse the existing *craft ports.

## Rendering

The native host uses wgpu/Vulkan for both egui and PhotoCraft's existing tiled
GPU canvas/compositor. Frames are drawn in BGRA (the screen's and a `B_RGB32`
bitmap's byte order, so nothing is swizzled on the processor) into the
presenter's frame texture (`apps/photocraft-haiku/src/present.rs`), which gets
them onto the window. `PHOTOCRAFT_PRESENT` picks how:

- `readback` (the default): a compute pass compares the frame with the one
  app_server already has, 64 px tile by tile. Only the changed tiles, joined
  into a few rectangles (`damage.rs`), are copied back and drawn into the
  window's `BBitmap`; a frame identical to the last one copies nothing.
- `full`: the whole frame is read back and drawn every time (the first host).
- `direct` (experimental): the window is a `BDirectWindow` with air/OS's
  `B_DIRECT_DEVICE_PIXELS`, the screen's frame buffer is imported into Vulkan
  with the drivers' private `VkImportScanoutMemoryHAIKU` (`scanout.rs`, as
  Summit does), and the GPU copies each frame into the visible parts of the
  window: nothing is read back and the processor does not wait. app_server's
  own copy of the screen catches up through a readback a quarter of a second
  after the screen changed. The window's direct daemon thread waits for the
  GPU's copies before app_server moves or covers the window, and anything the
  direct path cannot do (another driver, a resize in progress, an error) is read
  back instead. `PRESENT_VERIFY=1` reads the frame buffer back after each direct
  frame and logs how many visible pixels differ from the frame and how many
  covered ones equal it. It is not the default: on the Framework 13 (RADV,
  amdgpu_drm) the Vulkan device is lost a few seconds after the frame buffer is
  imported, even when nothing is copied into it (the copies themselves were
  pixel-exact while it lasted), and it has not been tried on NVK.

Measured on the Framework 13 (Radeon 760M, 175 %, a 1000×700 window = 1750×1225
device pixels; `tools/haiku/present-bench.py`, 40 samples each, against the
0.17.0~airos1-1 build): app_server's processor time 0.589 → 0.208 s,
PhotoCraft's 3.17 → 1.01 s, a hover's request-to-frame latency 21.7 → 15.6 ms
(median), the whole-canvas change about the same (21.2 → 20.7 ms); the median
frame copies nothing back. Per-frame present costs are logged with
`RUST_LOG=warn,photocraft_haiku=debug,present=debug`.

`PHOTOCRAFT_RENDERER=auto|cpu|vulkan` selects the host renderer. Auto is the default
and falls back to software if a hardware adapter cannot initialize. Saved CPU
compatibility preferences select software at the next launch. Vulkan mode
requires an integrated or discrete hardware adapter; it rejects software Vulkan
adapters. At runtime, GPU loss or an uncaptured error switches both the document
canvas and the window renderer to CPU, preserving the open documents.
PhotoCraft's normal per-operation CPU fallbacks remain available.

A working Vulkan loader and driver are prerequisites for acceleration; the app
package does not install or replace graphics drivers. AMD was tested with RADV.
NVIDIA was tested with the air/OS NVK/NVRM runtime. If necessary, choose a matching
ICD for this process with `VK_ICD_FILENAMES=/path/to/icd.json`.

On the validation X399, the system manifests pointed to AMD despite its NVIDIA
card. Validation used a private NVK runtime under
`/boot/home/PhotoCraft-validation/runtime` and the `nvk.json` manifest there.
The installed `photocraft-nvidia` command in
`/boot/home/config/non-packaged/bin` launches PhotoCraft with that private ICD;
it forwards file arguments. No system-wide Vulkan configuration was changed.
Its library SHA-256 is
`00aeaee9cb7c9f8aaf481433fce4fefb96f4e6ad7425f010aed9a7a95855b09d`.

## Build and package

```sh
JOBS=12 bash tools/haiku/build.sh
bash tools/haiku/package.sh --no-build
```

The scripts build the tier-3 Rust target `x86_64-unknown-haiku`, including its
standard library, and cross-compile the small C++ Be API boundary. Panic
unwinding stays enabled. `tools/haiku/env.sh` supplies the workspace defaults for
`RUSTUP_HOME`, `CARGO_HOME`, `HAIKU_SYSROOT`, `HAIKU_CROSS` and `CARGO_TARGET_DIR`;
these can be overridden. The packaging script also accepts `HAIKU_HOST_TOOLS`
for Haiku's Linux-hosted resource compiler and package tools.

Output: `artifacts/haiku/photocraft-0.17.0~airos1-1-x86_64.hpkg`. Install on the
target with `pkgman install /path/to/photocraft-0.17.0~airos1-1-x86_64.hpkg`.
PhotoCraft appears in Deskbar's Applications menu and at
`/boot/system/apps/PhotoCraft`. Its native icon and executable MIME attributes
are embedded in the package. Existing preferred applications are preserved.

The vendor patches enable Haiku's existing Vulkan backend in wgpu 30.0.1 and
adapt cap-primitives 4.0.3 to Haiku's filesystem metadata and flags. See
[`vendor/README.md`](../vendor/README.md). No automation capability checks are
removed. The dependency's regular-file `st_rdev = -1` sentinel has a regression
test because the original unsigned conversion panicked on `.pcraft` opens.

## Native integration and automation

The host supplies native windows, keyboard/mouse input, display-density changes,
asynchronous Open/Save/folder panels, Tracker document refs, text clipboard and
PNG plus Haiku `image/bitmap` clipboard exchange. Saved files receive MIME types.
Native close/quit requests respect the editor's unsaved-changes dialog.

Settings live in `~/config/settings/PhotoCraft`. `PHOTOCRAFT_CONFIG_DIR` selects
an isolated directory, and `PHOTOCRAFT_NO_RECOVERY=1` disables recovery for tests.
Native tablet pressure and automatic monitor-profile discovery are not wired
in this initial host; the editor's explicit colour-profile controls remain.

No control listener is opened by default. For an isolated test instance:

```sh
PHOTOCRAFT_CONFIG_DIR=/boot/home/PhotoCraft-validation/settings \
PHOTOCRAFT_RENDERER=vulkan /boot/system/apps/PhotoCraft \
  --control 7878 \
  --control-token-file /boot/home/PhotoCraft-validation/control.token \
  --automation-read-root /boot/home/PhotoCraft-validation \
  --automation-write-root /boot/home/PhotoCraft-validation
```

The server creates its token file with mode 0600. `tools/haiku/control.py` reads
that file locally and sends JSON requests from stdin. Paths in `app.open`,
`app.save` and screenshots are relative to the granted roots; absolute paths,
parent traversal and escaping symlinks are refused. Use the upstream
[control protocol](control-protocol.md) for editing commands.

`tools/haiku/validate.py --token-file FILE --report REPORT.json` runs on the
target against that test instance. It creates a synthetic 6000×4000 document,
benchmarks repeated opacity changes, simulates GPU loss, checks full software
fallback, and saves afterward. It changes the test instance's GPU preference.
The native clipboard/key probes under `tools/haiku/` exercise Be API integration.

## Hardware results — 18 October 2026

These results, and [`haiku-validation.json`](haiku-validation.json), are for PhotoCraft
0.10.0 (`0.10.0~airos1-2`). They have not been repeated since the 0.17.0 merge below.

Both GPUs rendered the full editor at 1.75× density. The layered 640×480 fixture
contains a soft brush stroke, a Multiply layer and a brightness/contrast
adjustment with its mask. Its GPU canvas pixels matched the CPU export exactly
at 8 bits; at 16 and 32 bits, maximum displayed-channel error was 1/255.
`.pcraft` save/reopen exports were pixel-identical at all three depths. PSD
round trips preserved all four layers and depth, with maximum export error 1/255.
The final installed package (`0.10.0~airos1-2`, source `cbe6d62e6`) passed
GPU rendering and layered open/save checks on both devices. Cold native
document-ref launches opened the PSD on both, with the expected window title.

Native bitmap clipboard paste was pixel-exact, and copied images advertised both
PNG and native bitmap formats. Both devices passed authentication, path-escape,
unsaved-quit cancellation, and GPU-loss/edit/save checks. With a missing Vulkan
ICD, auto mode also opened, edited, saved and captured a frame in software on
both devices.

The 24 MP benchmark uses three layers, two warmups and eight measured opacity
changes. Each sample measures a command through the next UI frame on the same
machine, including rendering and presentation. These are end-to-end latencies,
not GPU kernel timings or claims about every filter.

| Device / driver | Vulkan median | CPU median | CPU/Vulkan ratio |
|---|---:|---:|---:|
| Laptop, AMD Radeon 760M, RADV Mesa 25.3.6 | 57.6 ms | 467.1 ms | 8.1× |
| X399, NVIDIA GTX 1070, NVK Mesa 25.3.0-devel | 97.3 ms | 906.7 ms | 9.3× |

The native Open panel opened/cancelled and Save As wrote a correctly typed PSD
on the laptop. The metadata dependency's 16 unit tests also passed on both
Haiku devices. Structured rendering/round-trip/benchmark evidence is in
[`haiku-validation.json`](haiku-validation.json).

Local gates passed: 3,144 editor UI unit/integration tests (13 ignored), 160 text
tests (4 ignored), 10 native-host raster/input tests, Linux clippy on the affected crates, native
host clippy, all L0–L6 WebAssembly checks and the 32-crate layering check.
The native clippy invocation uses `--no-deps`; without it Rust 1.99 reports a
false positive in an unchanged dependency's already-const thread-local initializer.
The full corpus gate passed 3,805 tests (27 ignored). All five quick-performance
programs completed successfully on Linux/llvmpipe. Quick mode does not apply
full-size budgets, and the regression comparison was skipped because upstream
records its baseline on a different machine class (Apple M4 Pro). These host
checks are separate from the real AMD/NVIDIA measurements above. Details are in
the fork's
[release validation record](https://github.com/jmgasper/photocraft/blob/main/docs/haiku-validation.json).

## Upstream 0.17.0 merge (2026-10-29)

Upstream `69b8fd9c14b2` was merged (not rebased) into this fork's `main`. Upstream moved the canvas
modules under `crates/ui-egui/src/canvas/`, split the application into `src/app/`, and moved to
its own egui 0.37 and winit forks (`[patch.crates-io]`). The air/OS changes follow it:

- `photocraft-ui-egui` uses `wgpu` and `egui-wgpu` directly and depends on `eframe` only off
  air/OS. `eframe::App` delegates to `frame_exit`, `frame_logic`, `frame_input` and `frame_ui`,
  which the native host calls with `integration::Frame`.
- Upstream still locks wgpu 30.0.1 and cap-primitives 4.0.3, so the vendored copies and their
  patches are unchanged. They sit beside upstream's egui and winit patches.
- Upstream now saves recovery and closes the app when the window's GPU device is lost (#3438).
  The air/OS host keeps its software fallback instead: it calls `forget_window_device` after
  `set_wgpu`, so a fault (including `ui.gpu.simulateLoss`) switches the canvas and window to CPU.
- egui 0.37 delivers screenshots via `ScreenshotCallback`, and wheel events carry a
  `MouseWheelSource` (`Unknown`: Haiku doesn't say what sent them). Import takes
  `ImportOptions`, and the clipboard image reader returns `Result<Option<_>>`.

## Native application icon (2026-10-26)

The air/OS icon is an original landscape print and paintbrush, licensed under
MIT OR Apache-2.0. Its 64-unit vector drawing follows the
[Haiku icon guidelines](https://www.haiku-os.org/development/icon-guidelines):
vertical edges stay upright in perspective, the main light comes from the top
left, the right faces are darker, a floor shadow extends to the right, and the
silhouette has a two-unit black outline (one pixel at 32px). Broad details are
intended to remain legible at 16px and 32px. No fonts or third-party artwork are used.

Rebuild both the native HVIF and its SVG preview from this checkout:

```sh
python3 tools/haiku/make-icon.py
```

`apps/photocraft-haiku/resources/PhotoCraft.hvif` is the packaged icon;
`apps/photocraft-haiku/resources/PhotoCraft.svg` exports the same geometry for inspection.
The existing `.rdef` imports the HVIF, and `tools/haiku/package.sh` embeds it in
the executable's resources. Tracker and Deskbar therefore use the vector icon.
The upstream icons for other platforms remain separate. Native previews can be
rendered on Haiku with `BIconUtils::GetVectorIcon`; review at 16, 32, 64 and 128px
on light and dark backgrounds before changing the drawing.

Package version `0.10.0~airos1-3` includes this icon update.
