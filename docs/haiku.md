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

For 0.10.0, the X399's system manifests pointed to AMD despite its NVIDIA
card (by the 0.17.0 validation its system NVK worked as is). Validation used a private NVK runtime under
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
standard library, and cross-compile the small C++ Be API boundary.

Like upstream's releases, the build embeds the
[craft-fonts](https://github.com/storytold/craft-fonts) collection (Japanese,
Chinese, Korean, Arabic, Hebrew, Thai, Devanagari, Georgian and symbol faces,
for the interface and the text tool; `crates/text/build.rs`): `build.sh` takes
it from `../craft-fonts` beside this checkout, or from `CRAFT_FONTS_DIR`, and
fails without it; `CRAFT_FONTS_DIR=` (empty) builds without. Check out the
commit upstream's workflows pin (`craft-fonts-ref`, `b076417b5880` for 0.17.0).
The fonts add about 35 MB to the executable; `package.sh` ships their OFL
licences under `licenses/craft-fonts/`. Panic
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

### One running copy

As the desktop app keeps one PhotoCraft per user (`single_instance`), one copy
runs (`B_SINGLE_LAUNCH`): documents opened from Tracker while it runs arrive in
it (`B_REFS_RECEIVED`), and a later launch from Terminal hands its arguments to
it (`B_ARGV_RECEIVED`, relative paths resolved against the launch's folder) and
ends before it touches the log, the preferences or the recovery records. Either
way the documents open as tabs through the editor's own open path, as File ›
Open does, and the window comes to the front (also for a launch without
documents). The system matches a running copy by its executable file, not its
signature, so a test build in another folder runs beside the installed one, each
with its own recovery records. The desktop app gives automation (`--control`) a
process of its own; one executable file can't have that here, so a `--control`
launch while that file's copy runs ends with an error: run the test instance
from a copy of the executable (or with nothing else running from it). A build
without the rdef resources (no `rc`/`xres` step) is multiple-launch.

### Scrolling

A mouse wheel scrolls in whole steps, as before. An air/OS touchpad's scroll
gesture (`B_MOUSE_WHEEL_CHANGED` with `be:device_subtype` 2,
`be:wheel_precise_delta_*` and `be:wheel_phase`, air/OS since 2026-10-27)
reaches the editor as a Mac trackpad's does in the desktop app
(`MouseWheelSource::Trackpad`): its unrounded steps become points (a step is
egui's line, 40 points), with the gesture's start and end. The canvas then pans
(or zooms, with the wheel's zoom modifier) with the fingers and without the
wheel's easing, and egui's lists treat it as a trackpad. The end of a gesture
that arrives with its last movement is held for the next frame (egui drops the
deltas of a gesture that ended). air/OS's touchpad has no pinch gesture, so
Zoom with Trackpad Pinch has nothing to act on: zoom with the wheel's modifier,
the keys or the Zoom tool.

### Updates

PhotoCraft is a package on air/OS, updated by pkgman and SoftwareUpdater, and
`craft-update` has no Haiku platform; the start-up check is off (the host gives
no `check_updates` service). Help › Check for Updates is left out of the menus
and the command search, and the command itself (automation) only opens the
updates window saying that SoftwareUpdater installs newer versions. The
community links (Help › Discord, the Discord button on the start screen and in
About) stay: they open the web browser, and upstream has no switch for the host
to hide them.

### Windows: one `BWindow` per egui viewport

The desktop app opens floating panels, extra document windows (Window › Arrange
› New Window for Document) and Camera Raw in OS windows of their own: each is an
egui viewport (craft-ui's dock area draws a floating panel group with
`show_viewport_immediate`). The host used to embed them all in the main window;
it now gives every viewport a native window, as eframe does:

- A floating panel group (dragged out of the dock, or `window.panel.float`) is
  an undecorated window (`B_NO_BORDER_WINDOW_LOOK`) with craft-ui's own bar.
  Dragging the bar moves it (craft-ui moves the window with `OuterPosition`
  as the pointer goes, as on Windows and X11); its edges and corners resize it
  (craft-ui's resize bands send `BeginResize`, and the window follows the
  pointer while the button stays down); dragging a tab onto the dock docks it
  again; × closes the panel. Panel windows float above every window
  (`B_FLOATING_ALL_WINDOW_FEEL`) while PhotoCraft is active and drop to
  normal windows (`B_NORMAL_WINDOW_FEEL`, behind the active application's) when
  another application is activated, and while a modal dialog is open, as
  craft-ui asks (`WindowLevel::AlwaysOnTop`/`Normal`). The Studio themes'
  Properties card stays inside the main window, as upstream keeps it.
- Saved workspaces put floating panels back in their windows at the next start.
- A document's extra window and Camera Raw are normal titled windows; closing
  one closes that window only. The main window's close button and Quit ask
  about unsaved documents, then close every window.
- The native menu bar, the file panels and the screen colour picker belong to
  the main window. Full Screen modes fill the monitor with the main window
  without its border; it keeps its normal feel, so the floating panels stay in
  front of it (a Deskbar set to Always on Top stays in front too).
- Pen tablets: the stylus samples go to the editor's stylus feed, which is the
  app's, as AppKit's event monitor and the X11 reader feed it in the desktop
  app; the pointer moves they come with go to the window they happened in.

Design (shared with WordCraft, whose host has the same driver): `src/viewports.rs`
is a platform-neutral multi-viewport driver (immediate and deferred viewports,
per-window input, every viewport's `ViewportInfo` in each pass, builder changes
and `ViewportCommand`s as native window commands, windows closing when egui drops
their viewport, repaints by the pass that draws a viewport), tested on Linux
with a fake platform (`src/viewports/tests.rs`). `src/platform.rs` is its air/OS
side: `host.cpp` keeps its `PcWindow`s in a map by id and tags every event with
its window; Vulkan keeps one device and egui renderer (the textures and the
canvas's paint callback resources are the app's) with a presenter per window,
and a window whose frame is unchanged isn't drawn again; the rasteriser keeps
one texture store with a canvas per window. Each window has its own direct
connection (the main window's canvas starts below its menu bar), cursor, full
screen state and pointer drag. A GPU fault, reported by the editor's canvas or
a window's frame, switches the editor and every window to software at the next
frame. The shell runs the editor's frame in the root viewport's pass; a close
request for another window gets one more pass, in which the window closes once
the app has stopped showing it.

`tools/haiku/window-probe.cpp` (built on air/OS with `g++ -o window-probe
window-probe.cpp -lbe`) lists a running copy's windows (frame, look, feel,
active) and sends pointer drags and clicks at screen points the way the input
server does, for testing without someone at the mouse: `window-probe TEAM drag
X0 Y0 X1 Y1`. It can't press the real buttons, so a window activated by it
isn't, and another application taking focus during a drag ends the drag (as it
should).

Tested on the x399 (Vulkan with changed-tile readback, `PHOTOCRAFT_RENDERER=cpu`,
and `PHOTOCRAFT_PRESENT=direct` with `PRESENT_VERIFY=1`: no wrong pixel in the
main window or the panels' windows): floating Layers and History, moving a panel
by its bar (to the point), resizing it from a corner and from its left edge,
docking it again by dragging its tab onto the Color group, clicks in a panel
(History states), its × button, PhotoCraft losing focus to Tracker and getting
it back (panels normal, then floating again), a modal Save Changes prompt
(panels drop behind, come back after Cancel), a pen stroke from
`tablet-probe` in the main window (it tapers as before), a document's extra
window opened and closed, Full Screen Mode With Menu Bar and back with panels
floating, saved floating panels coming back at the next start, and quitting
with panels open and an unsaved document (Don't Save). Camera Raw's own window
wasn't tried.

### System services

The host fills in the editor's platform services (`Services` in
`crates/ui-egui/src/services.rs`) the Haiku way:

- **Menu bar** (`native_menu`, `src/native_menu.rs`): the menus are a native
  `BMenuBar` at the top of the window, and the in-window menus are hidden.
  Upstream lays them out for a Mac (an app menu named PhotoCraft); here About
  goes at the end of Help, Settings (the Preferences pages) at the end of Edit,
  Language and Appearance at the end of Window, and Quit at the end of File.
  Minimize, Zoom and Close Window are the window's own. Keys stay with the
  editor, as upstream's contract asks: the window hands Command keys to the
  canvas before `BWindow` looks for a menu shortcut, so the menus only show
  them (Command shortcuts as Haiku draws them, the others, such as `F1`,
  `Shift+F5` or a tool's letter, as text). Command+Q still reaches `BWindow`
  and runs File › Quit, with the Save Changes prompt.
  `PHOTOCRAFT_NATIVE_MENU=0` keeps the in-window menus. The window grows by
  the bar, so the canvas keeps its size; the interface's viewport, resize
  commands and the direct presentation's frame start below the bar (the direct
  path with the bar is untested, like the direct path on NVK).
- **Pen tablets** (`src/tablet.rs`): Haiku's tablet input add-ons
  (`input_server/devices/tablet` for USB/I²C HID tablets, `wacom`, `easypen`)
  send the pen's state with `B_MOUSE_MOVED`: `be:tablet_pressure` (0..1),
  `be:tablet_eraser` and, when the pen has it, `be:tablet_tilt_x`/`_y`. The
  host passes each sample to the canvas's stylus input (the one the desktop
  app's macOS and X11 readers feed), so pressure, tilt and the eraser end work
  as there (Preferences › Tools › Use Tablet Pressure, the eraser end selecting
  the Eraser). A pen's moves are never merged, and its drags keep the pointer
  history, so a stroke gets every sample. The HID add-on's button messages carry
  no sample (the pen keeps its last one), and its tilt is the HID report's value
  scaled to 0..1 (upright at 0.5), Wacom's -1..1; both become ±64°. Haiku
  reports no barrel rotation. `tools/haiku/tablet-probe.cpp` sends the canvas a
  stroke as the add-ons would, for testing without a tablet.
- **Monitor profiles** (`read_displays`, `src/displays.rs`): neither Haiku nor
  air/OS keeps a colour profile for a screen (app_server and the Screen
  preferences have no colour management). So the profile of a monitor is a file
  in `~/config/settings/ColorProfiles/` (the folder is made at the first launch),
  named after the monitor; the first of these that exists is used, whatever the
  letter case, `.icc` or `.icm`:
  1. `<monitor name> <serial number>.icc`, to tell two identical monitors apart;
  2. `<monitor name>.icc`, the monitor's name in the Screen preferences
     (its EDID name, e.g. `DELL U2720Q.icc`);
  3. `<connector>.icc` (e.g. `DP-2.icc`), for a monitor without EDID;
  4. `default.icc`, for every monitor without one of its own.

  On air/OS each monitor of the display layout is a display of its own, with
  its frame on the desktop; on Haiku the screen, named by
  `BScreen::GetMonitorInfo`. With Edit › Color Settings › Monitor Profile at
  Auto, the canvas is shown in the profile of the monitor most of the window is
  on, and Help › System Info lists each monitor's. A monitor without a profile
  is shown as sRGB, and the editor says so once. The folder is also among the
  installed profiles the colour dialogs list (with
  `~/config/non-packaged/data/color/icc` and `/boot/system/data/color/icc`).
- **Screen colour picker** (`screen_pick`): Pick Screen Color in the colour
  dialogs covers every monitor with a snapshot of the screen
  (`BScreen::GetBitmap`) under a loupe; a click picks the pixel under the
  pointer, Esc or a right click cancels, the wheel zooms. The picked pixel is
  converted to sRGB through the profile of its monitor, as above. (The editor's
  own picker shows its snapshots in viewports of their own, which this host
  embeds in its window, so the picker is native.)
- **Caps Lock** (`caps_lock`) shows the painting tools' precise cursor;
  **Beep When Done** (`beep`) plays the Beep sound of the Sounds preferences;
  Appearance › Auto follows the panel colour of the Appearance preferences
  (`system_theme`, re-read when they change).
- **Drops** (`cursor_pos`): files dropped on the window go to the editor as the
  desktop app's do: on the canvas they are placed as layers, elsewhere opened.
- **Leaving** (`quit`): once the Save Changes prompts are answered, the app
  ends through the window's own close. `flush_autosaves` (finish the queued
  crash-recovery writes and wait for them) is there for upstream's ending
  without asking, when the window's GPU device is lost; this host switches to
  software rendering instead (see Rendering), so nothing calls it yet.

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

## Hardware results

### 0.17.0 on the X399 — 29 October 2026

`tools/haiku/validate.py` against 0.17.0 with the HAIKU5 host and craft-fonts, on
the X399 (GTX 1070, the system's NVK Mesa 25.3.0-devel, 200 %; no private
Vulkan runtime is needed any more), recorded under `revalidation_0_17` in
[`haiku-validation.json`](haiku-validation.json):

| Device / driver | Vulkan median | CPU median | CPU/Vulkan ratio |
|---|---:|---:|---:|
| X399, NVIDIA GTX 1070, NVK Mesa 25.3.0-devel | 47.3 ms | 791.4 ms | 16.7× |

Vulkan is twice as fast as with 0.10.0 (97.3 ms): the changed-tile presentation
and upstream's damage tracking. Simulated GPU loss switched the canvas and the
window to software and editing and saving went on. A native `image/bitmap`
pasted as a layer of its size, and Copy Merged put both PNG and a native bitmap
on the clipboard. The Japanese interface is drawn in full (the build without
craft-fonts shows missing-glyph boxes). The system services above were checked
on the same machine: tablet strokes from `tablet-probe` (no tablet is attached),
a monitor profile in `ColorProfiles`, the screen picker with and without it,
the menu bar, and quitting.

An arm64 build cross-compiles with the airos-ci toolchain (its
`aarch64-unknown-haiku` target and the arm64 SDK); it has not been run on an
arm64 machine yet. The Framework 13 results below are still 0.10.0's.

### 0.10.0 — 18 October 2026

These results are for PhotoCraft 0.10.0 (`0.10.0~airos1-2`).

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
| X399, NVIDIA GTX 1070, NVK Mesa 25.3.0-devel (private runtime) | 97.3 ms | 906.7 ms | 9.3× |

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
  `MouseWheelSource` (a wheel's or a touchpad's: see Scrolling). Import takes
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
