//! The air/OS side of [`crate::viewports`]: native windows are `host.cpp`'s `BDirectWindow`s,
//! frames are drawn with Vulkan (`gpu`, one presenter per window) or by the software rasteriser
//! (one canvas per window, the textures shared), and the clipboard, cursors and links are the
//! Be API's.
//!
//! The GPU device is the editor's too (its canvas compositor), and so is its health: a fault the
//! editor or a window's frame reports switches the editor's canvas and every window to software
//! at the start of the next frame ([`crate::shell`]), as the one-window host did.

use std::collections::HashMap;
use std::time::Instant;

use photocraft_ui_egui::gpu_canvas::{DeviceHealth, Fault};

use crate::host;
use crate::raster::{Canvas, Rasterizer};
use crate::services;
use crate::viewports::{NativeCommand, Platform, WindowId, WindowInfo, WindowSpec};

const LOG: &str = "photocraft_haiku";

/// What is drawn for one window.
#[derive(Default)]
struct Surface {
    canvas: Canvas,
    gpu: Option<crate::gpu::Surface>,
}

pub struct Haiku {
    raster: Rasterizer,
    /// The GPU renderer; `None` draws with `raster` (no hardware Vulkan adapter,
    /// `PHOTOCRAFT_RENDERER=cpu`, or the device failed). The rasteriser keeps egui's textures
    /// either way, so it can take over between two frames.
    gpu: Option<crate::gpu::Gpu>,
    /// The device's health, which the editor's GPU canvas watches too.
    health: Option<DeviceHealth>,
    surfaces: HashMap<WindowId, Surface>,
}

impl Haiku {
    pub fn new(gpu: Option<crate::gpu::Gpu>, health: Option<DeviceHealth>) -> Self {
        Self { raster: Rasterizer::new(), gpu, health, surfaces: HashMap::new() }
    }

    /// Back to the software rasteriser for good. The next frames draw every window with it.
    pub fn drop_gpu(&mut self) {
        for surface in self.surfaces.values_mut() {
            // The presenters let go of the windows before the device goes.
            surface.gpu = None;
            surface.canvas.invalidate();
        }
        self.gpu = None;
        self.health = None;
    }

    fn faulted(&self) -> bool {
        self.health.as_ref().is_some_and(|h| h.fault().is_some())
    }

    /// A frame the GPU couldn't draw: the editor and the windows switch at the next frame.
    fn fail(&mut self, e: String) {
        log::error!(target: LOG, "{e}");
        match &self.health {
            Some(health) => health.mark(Fault::Error(e)),
            None => self.drop_gpu(),
        }
    }
}

impl Drop for Haiku {
    fn drop(&mut self) {
        // The presenters let go of their windows (and the frame buffer) before the device goes.
        self.surfaces.clear();
    }
}

/// egui's cursor as one of Haiku's `BCursorID`s.
fn cursor_id(icon: egui::CursorIcon) -> i32 {
    use egui::CursorIcon as C;
    match icon {
        C::None => 12,
        C::ContextMenu => 3,
        C::Help => 9,
        C::PointingHand => 6,
        C::Progress | C::Wait => 14,
        C::Cell | C::Crosshair => 5,
        C::Text => 2,
        C::VerticalText => 10,
        C::Alias => 29,
        C::Copy => 4,
        C::Move | C::AllScroll => 11,
        C::NoDrop | C::NotAllowed => 13,
        C::Grab => 7,
        C::Grabbing => 8,
        C::ResizeHorizontal | C::ResizeColumn => 24,
        C::ResizeVertical | C::ResizeRow => 23,
        C::ResizeNeSw => 25,
        C::ResizeNwSe => 26,
        C::ResizeEast => 16,
        C::ResizeSouthEast => 21,
        C::ResizeSouth => 17,
        C::ResizeSouthWest => 22,
        C::ResizeWest => 18,
        C::ResizeNorthWest => 20,
        C::ResizeNorth => 15,
        C::ResizeNorthEast => 19,
        C::ZoomIn => 27,
        C::ZoomOut => 28,
        _ => 1,
    }
}

/// egui's image copy, as PNG.
fn copy_image(img: &egui::ColorImage) {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    let mut png = Vec::new();
    let encoded = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| "bad size".to_string())
        .and_then(|i| i.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string()));
    match encoded {
        Ok(()) => {
            if !host::clipboard_set_picture("image/png", &png) {
                log::warn!(target: LOG, "clipboard: couldn't copy the picture");
            }
        }
        Err(e) => log::warn!(target: LOG, "clipboard picture: {e}"),
    }
}

impl Platform for Haiku {
    fn waker(&self) -> fn() {
        host::wake
    }

    fn open(&mut self, spec: &WindowSpec) -> Option<WindowId> {
        let window = host::open_window(spec);
        log::debug!(target: LOG, "window {window:?} opened: {spec:?}");
        window
    }

    fn close(&mut self, window: WindowId) {
        // The presenter stops listening to the window's direct connection first.
        self.surfaces.remove(&window);
        host::close_window(window);
        log::debug!(target: LOG, "window {window} closed");
    }

    fn command(&mut self, window: WindowId, command: NativeCommand) {
        log::debug!(target: LOG, "window {window}: {command:?}");
        host::window_command(window, &command);
    }

    fn info(&self, window: WindowId) -> WindowInfo {
        host::window_info(window)
    }

    fn set_cursor(&mut self, window: WindowId, cursor: egui::CursorIcon) {
        host::set_cursor(window, cursor_id(cursor));
    }

    fn output(&mut self, command: &egui::OutputCommand) {
        log::debug!(target: LOG, "output {command:?}");
        match command {
            egui::OutputCommand::CopyText(t) => {
                if !host::clipboard_set(t, None) {
                    log::warn!(target: LOG, "clipboard: couldn't copy");
                }
            }
            egui::OutputCommand::CopyImage(img) => copy_image(img),
            // Haiku has no PRIMARY selection (X11/Wayland middle-click paste).
            egui::OutputCommand::TextSelectionSettled(_) => {}
            egui::OutputCommand::OpenUrl(u) => {
                if !host::open_url(&u.url) {
                    log::warn!(target: LOG, "couldn't open {}", u.url);
                }
            }
        }
    }

    fn clipboard_text(&self) -> Option<String> {
        services::clipboard_text()
    }

    fn dark(&self) -> bool {
        host::dark()
    }

    fn max_texture_side(&self) -> usize {
        self.gpu.as_ref().map_or(8192, |g| g.max_texture_side())
    }

    fn set_textures(&mut self, delta: &egui::TexturesDelta) {
        // The rasteriser keeps them too, to take over from a failed GPU.
        self.raster.set_textures(delta);
        if let Some(gpu) = &mut self.gpu
            && let Err(e) = crate::crash_guard::guard("GPU textures", || {
                gpu.set_textures(delta);
                Ok::<(), String>(())
            })
        {
            self.fail(e);
        }
    }

    fn free_textures(&mut self, ids: &[egui::TextureId]) {
        self.raster.free_textures(ids);
        if let Some(gpu) = &mut self.gpu {
            gpu.free_textures(ids);
        }
    }

    fn paint(&mut self, window: WindowId, size: [u32; 2], primitives: &[egui::ClippedPrimitive], ppp: f32, clear: egui::Color32, scale: f32) {
        if self.gpu.is_some() {
            // A fault waits for the next frame, which the editor and every window draw in
            // software: this frame's canvas callbacks are GPU work the rasteriser can't draw.
            if self.faulted() {
                return;
            }
            let t0 = Instant::now();
            let gpu = &self.gpu;
            let surface = self.surfaces.entry(window).or_default();
            if surface.gpu.is_none()
                && let Some(gpu) = gpu
            {
                surface.gpu = Some(gpu.surface(window));
            }
            let result = match (&mut self.gpu, &mut surface.gpu) {
                (Some(gpu), Some(surface)) => crate::crash_guard::guard("GPU frame", || gpu.render(surface, size, primitives, ppp, clear, scale)),
                _ => return,
            };
            match result {
                Ok(_) => log::debug!(target: LOG, "window {window} frame {}x{}: drawn in {:.1} ms", size[0], size[1], t0.elapsed().as_secs_f64() * 1e3),
                Err(e) => self.fail(e),
            }
            return;
        }
        let t0 = Instant::now();
        let (w, h) = (size[0] as usize, size[1] as usize);
        let surface = self.surfaces.entry(window).or_default();
        let dirty = self.raster.render(&mut surface.canvas, [w, h], primitives, ppp, clear);
        let t1 = Instant::now();
        if let Some(dirty) = dirty {
            host::present(window, surface.canvas.pixels(), w, h, dirty, scale);
        }
        log::debug!(target: LOG,
            "window {window} frame {w}x{h}: raster {:.1} ms, present {:.1} ms, dirty {dirty:?}",
            (t1 - t0).as_secs_f64() * 1e3,
            t1.elapsed().as_secs_f64() * 1e3
        );
    }

    fn screenshot(&mut self, window: WindowId) -> egui::ColorImage {
        let Some(surface) = self.surfaces.get_mut(&window) else { return egui::ColorImage::default() };
        match &mut surface.gpu {
            Some(gpu) => gpu.image().unwrap_or_else(|e| {
                log::warn!(target: LOG, "screenshot: {e}");
                egui::ColorImage::default()
            }),
            None => surface.canvas.image(),
        }
    }

    fn chores_due(&self) -> Option<Instant> {
        self.surfaces.values().filter_map(|s| s.gpu.as_ref()).filter_map(|g| if g.wants_present() { Some(Instant::now()) } else { g.sync_due() }).min()
    }

    /// The last frame again where a window moved on the screen, and app_server's copy catching
    /// up with frames that went straight to the screen.
    fn chores(&mut self, scale: f32) {
        let now = Instant::now();
        let mut failed = None;
        for surface in self.surfaces.values_mut() {
            let Some(gpu) = &mut surface.gpu else { continue };
            let result = if gpu.wants_present() {
                crate::crash_guard::guard("GPU present", || gpu.present_again(scale))
            } else if gpu.sync_due().is_some_and(|at| at <= now) {
                crate::crash_guard::guard("GPU sync", || gpu.sync(scale))
            } else {
                continue;
            };
            if let Err(e) = result {
                failed = Some(e);
                break;
            }
        }
        if let Some(e) = failed {
            self.fail(e);
        }
    }
}
