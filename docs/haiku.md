# PhotoCraft on air/OS

This independent fork of PhotoCraft 0.10.0 (upstream `8d2e615ab93d`) runs the full
Rust editor on air/OS / Haiku x86_64. It retains the document engine, layers,
brushes, masks, adjustments, codecs, colour management, 8/16/32-bit documents,
recovery, preferences and authenticated automation. The native Be API host,
input mapping and software rasterizer reuse the existing *craft ports.

## Rendering

The native host uses wgpu/Vulkan for both egui and PhotoCraft's existing tiled
GPU canvas/compositor. It renders to a reusable offscreen texture, reads the
completed frame into a reusable buffer, and presents a native `BBitmap`.
This avoids a software triangle rasterizer in the accelerated path. There is
still a full-window readback: this is not a direct Vulkan swapchain or zero-copy
presentation. GPU submission and readback timings are logged separately with
`RUST_LOG=warn,photocraft*=info,photocraft_haiku=debug`.

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

Output: `artifacts/haiku/photocraft-0.10.0~airos1-2-x86_64.hpkg`. Install on the
target with `pkgman install /path/to/photocraft-0.10.0~airos1-2-x86_64.hpkg`.
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
