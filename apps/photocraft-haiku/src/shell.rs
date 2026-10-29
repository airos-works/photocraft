//! The interface thread's event loop: Haiku's input goes to the window it happened in
//! (`viewports`, which gives every egui viewport a window of its own: the floating panels', the
//! documents' extra windows, Camera Raw), PhotocraftApp runs its frame in the root viewport's
//! pass, the main window's, and egui's output goes back to the windows and the system.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui::{ViewportCommand, ViewportId, pos2};
use photocraft_ui_egui::PhotocraftApp;

use crate::host::{self, RawEvent, kind};
use crate::platform::Haiku;
use crate::services;
use crate::viewports::{Viewports, WindowInput};

const LOG: &str = "photocraft_haiku";

pub struct Shell {
    pub app: PhotocraftApp,
    ctx: egui::Context,
    viewports: Viewports<Haiku>,
    /// The main window (the root viewport's), with the menu bar.
    root: i32,
    /// The editor's GPU device health while the windows draw with Vulkan.
    gpu_health: Option<photocraft_ui_egui::gpu_canvas::DeviceHealth>,
    frame_info: photocraft_ui_egui::integration::Frame,
    /// The interface scale (the system font against 12 pt) and device pixels per logical pixel
    /// ([`host::device_scale`]), and when the latter was last asked.
    ui_scale: f32,
    dev: f32,
    dev_checked: Instant,
    quit: bool,
    /// Native menu clicks for its backend (`native_menu.rs`).
    menu_clicks: crate::native_menu::Clicks,
    /// Set by `Services::quit` once leaving is settled.
    quit_asked: Rc<Cell<bool>>,
    /// [`Self::dev`] for `Services::cursor_pos`.
    dev_shared: Rc<Cell<f32>>,
    /// Where files were dropped on the main window (logical pixels), for `Services::cursor_pos`
    /// in the frame that opens them.
    drop_at: Rc<Cell<Option<(f32, f32)>>>,
    /// The last mouse event was a pen's.
    pen: bool,
}

impl Shell {
    /// The shell around `app`, whose main window (the root viewport's) is native window `root`.
    pub fn new(mut app: PhotocraftApp, ctx: egui::Context, root: i32, ui_scale: f32) -> Result<Self, String> {
        let mode = std::env::var("PHOTOCRAFT_RENDERER").unwrap_or_else(|_| {
            if app.session.prefs().performance.effective_rendering_mode() == photocraft_engine::prefs::RenderingMode::Cpu { "cpu" } else { "auto" }.into()
        });
        app.perf.gpu_info.selected = mode.clone();
        app.perf.gpu_info.preference = "vulkan".into();
        app.perf.gpu_info.canvas = "cpu".into();
        if !matches!(mode.as_str(), "auto" | "cpu" | "vulkan") {
            return Err("PHOTOCRAFT_RENDERER must be auto, cpu or vulkan".into());
        }
        let gpu = if mode == "cpu" {
            None
        } else {
            match crate::crash_guard::guard("GPU startup", crate::gpu::Gpu::new) {
                Ok(gpu) => {
                    app.set_wgpu(gpu.state.clone());
                    // A device fault falls back to the software rasterizer (`frame`), so the
                    // windows never become undrawable: don't let the editor close the app for it.
                    app.forget_window_device();
                    if app.gpu_active() {
                        Some(gpu)
                    } else if mode == "vulkan" {
                        return Err(app.perf.gpu_info.fallback.clone().unwrap_or_else(|| "GPU canvas initialization failed".into()));
                    } else {
                        None
                    }
                }
                Err(e) if mode == "vulkan" => return Err(e),
                Err(e) => {
                    log::warn!(target: LOG, "GPU startup: {e}; using CPU rendering");
                    app.perf.gpu_info.fallback = Some(e);
                    None
                }
            }
        };
        let gpu_health = gpu.as_ref().and_then(|_| app.gpu_health());
        if gpu.is_none() {
            app.perf.gpu_info.backend = "software".into();
            app.perf.gpu_info.device_type = "cpu".into();
        }
        app.perf.gpu_info.present = gpu.as_ref().map_or("native software bitmap", |g| g.present_mode().name()).into();
        log::info!(target: LOG, "air/OS renderer: {}", if gpu.is_some() { "Vulkan" } else { "CPU" });
        let dev = host::device_scale();
        let viewports = Viewports::new(&ctx, Haiku::new(gpu, gpu_health.clone()), root, ui_scale * dev, dev);
        // The services that need the window: where the pointer is, leaving, the menu bar.
        let dev_shared = Rc::new(Cell::new(dev));
        let drop_at: Rc<Cell<Option<(f32, f32)>>> = Rc::default();
        let (d, at) = (dev_shared.clone(), drop_at.clone());
        app.services.cursor_pos = Some(Box::new(move |ctx: &egui::Context| {
            let (x, y) = at.get().or_else(host::cursor)?;
            let k = d.get() / ctx.pixels_per_point();
            Some(pos2(x * k, y * k))
        }));
        let quit_asked: Rc<Cell<bool>> = Rc::default();
        let asked = quit_asked.clone();
        app.services.quit = Some(Box::new(move || {
            asked.set(true);
            host::wake();
        }));
        let menu_clicks = crate::native_menu::Clicks::default();
        if std::env::var("PHOTOCRAFT_NATIVE_MENU").as_deref() != Ok("0") {
            let backend = crate::native_menu::HaikuMenu::new(crate::native_menu::HostSink, menu_clicks.clone());
            app.services.native_menu = Some(photocraft_ui_egui::native_menu::NativeMenu::new(Box::new(backend)));
        }
        // The monitor profiles, read before the first frame if that's quick (as the desktop app
        // does), else applied when they arrive.
        let displays = services::read_displays();
        match displays.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => photocraft_ui_egui::monitor_status::apply(&mut app, r),
            Err(_) => photocraft_ui_egui::monitor_status::pending(&mut app, displays),
        }
        Ok(Self {
            app,
            ctx,
            viewports,
            root,
            gpu_health,
            frame_info: Default::default(),
            ui_scale,
            dev,
            dev_checked: Instant::now(),
            quit: false,
            menu_clicks,
            quit_asked,
            dev_shared,
            drop_at,
            pen: false,
        })
    }

    /// Runs until the app quits.
    pub fn run(&mut self) {
        while !self.quit {
            host::wait(self.viewports.timeout());
            self.drain();
            if self.dev_checked.elapsed() > Duration::from_secs(2) {
                self.check_density();
            }
            for viewport in self.viewports.take_due() {
                if viewport == ViewportId::ROOT {
                    self.frame();
                } else if self.viewports.run_deferred(viewport).close_root {
                    self.quit = true;
                }
            }
            self.viewports.chores();
        }
    }

    /// Asks for the desktop's density again (the monitors may have changed).
    fn check_density(&mut self) {
        self.dev_checked = Instant::now();
        let dev = host::device_scale();
        if (dev - self.dev).abs() > f32::EPSILON {
            log::info!(target: LOG, "display density {} -> {dev}", self.dev);
            self.dev = dev;
            self.dev_shared.set(dev);
            self.viewports.set_density(self.ui_scale * dev, dev);
        }
    }

    /// A pen tablet's sample with a mouse event goes to the canvas's stylus feed before the
    /// pointer move it belongs to (see `tablet.rs`). The feed is the app's, as on the other
    /// desktops (AppKit's event monitor, the X11 reader): the canvas of whichever window the
    /// pointer moves in reads it.
    fn tablet(&mut self, ev: &RawEvent) {
        let raw = crate::tablet::Raw {
            tablet: ev.tablet,
            pressure: ev.pressure,
            tilt: (ev.has_tilt != 0).then_some((ev.tilt_x, ev.tilt_y)),
            eraser: ev.eraser != 0,
            subtype: ev.subtype,
        };
        match crate::tablet::update(raw) {
            crate::tablet::Update::Pen(sample) => {
                self.pen = true;
                self.app.stylus.feed.set(Some(sample));
            }
            crate::tablet::Update::Keep => {
                if ev.kind == kind::MOUSE_DOWN
                    && let Some(touch) = crate::tablet::contact(self.app.stylus.feed.get())
                {
                    self.app.stylus.feed.set(Some(touch));
                }
            }
            crate::tablet::Update::Mouse => {
                if std::mem::take(&mut self.pen) {
                    self.app.stylus.feed.set(None);
                }
            }
        }
    }

    /// Hands the queued native events to their windows; the application's own are handled here.
    fn drain(&mut self) {
        while let Some((ev, text)) = host::next_event() {
            if matches!(ev.kind, kind::MOUSE_MOVED | kind::MOUSE_DOWN | kind::MOUSE_UP) {
                self.tablet(&ev);
            }
            if ev.kind == kind::DROPPED_FILES && ev.window == self.root {
                self.drop_at.set(Some((ev.x, ev.y)));
            }
            if let Some(input) = window_input(&ev, &text) {
                if !self.viewports.input(ev.window, input) {
                    log::debug!(target: LOG, "event {} for window {}, which is gone", ev.kind, ev.window);
                }
                continue;
            }
            match ev.kind {
                // Quit (Command+Q, the Deskbar's Quit) closes the main window, which asks about
                // unsaved documents first.
                kind::QUIT_REQUESTED => {
                    self.viewports.input(self.root, WindowInput::Close);
                }
                // Tracker's Open, Open With and drops on the icon, and the documents of a later
                // launch (one copy runs: B_SINGLE_LAUNCH): they open as documents, as File › Open
                // opens them.
                kind::OPEN_FILES => {
                    for path in text.lines().filter(|l| !l.is_empty()) {
                        if let Err(e) = self.app.open_path(path) {
                            self.app.open_failed(path, &e);
                        }
                    }
                    host::window_command(self.root, &host::NativeCommand::Activate);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::MENU => {
                    self.menu_clicks.borrow_mut().push(ev.id);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::SCREEN_PICK => {
                    services::pick_answer(ev.id, ev.x != 0.0, ev.buttons as u32, ev.dx, ev.dy);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::DIALOG_ANSWER => {
                    services::answer(ev.id, ev.x != 0.0, &text);
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                kind::COLORS => {
                    self.check_density();
                    self.viewports.request(ViewportId::ROOT, Duration::ZERO);
                }
                // A panel's answer after its caller stopped waiting has nobody to go to.
                _ => {}
            }
        }
    }

    /// Back to software for good: the editor's canvas and every window. A device fault (the
    /// editor's canvas or a window's frame reported it) waits for this, the start of a frame:
    /// the last frame's canvas callbacks were GPU work the rasteriser can't draw.
    fn check_gpu(&mut self) {
        let Some(fault) = self.gpu_health.as_ref().and_then(|h| h.fault()) else { return };
        log::warn!(target: LOG, "{fault}; switching native host to CPU");
        self.app.check_gpu(&self.ctx);
        self.viewports.platform(Haiku::drop_gpu);
        self.gpu_health = None;
        self.app.perf.gpu_info.present = "native software bitmap".into();
    }

    /// The root viewport's pass: the editor's frame (the desktop app's `logic` and `ui`), which
    /// draws the other windows' viewports on the way.
    fn frame(&mut self) {
        self.check_gpu();
        let Some(mut raw) = self.viewports.root_input() else { return };
        self.app.frame_input(&self.ctx, &mut raw);
        let ctx = self.ctx.clone();
        let t0 = Instant::now();
        let mut closing = false;
        let out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            closing = ctx.input(|i| i.viewport().close_requested());
            self.app.frame_logic(&ctx, &mut self.frame_info);
            self.app.frame_ui(ui, &mut self.frame_info);
        });
        let t1 = Instant::now();
        self.frame_info.cpu_usage = Some((t1 - t0).as_secs_f32());
        self.drop_at.set(None);
        services::check_pick();
        // The main window's close button, Close Window or Quit: the editor asks about unsaved
        // changes and cancels the close while it does (`CancelClose`), as eframe honours.
        let cancelled = out.viewport_output.get(&ViewportId::ROOT).is_some_and(|vp| vp.commands.iter().any(|c| matches!(c, ViewportCommand::CancelClose)));
        if closing && !cancelled {
            self.quit = true;
        }
        if self.viewports.finish(ViewportId::ROOT, out).close_root || self.quit_asked.get() {
            self.quit = true;
        }
        if self.gpu_health.as_ref().is_some_and(|h| h.fault().is_some()) {
            // The next frame draws in software.
            self.viewports.request(ViewportId::ROOT, Duration::ZERO);
        }
        log::debug!(target: LOG, "frame: interface {:.1} ms, output and drawing {:.1} ms", (t1 - t0).as_secs_f64() * 1e3, t1.elapsed().as_secs_f64() * 1e3);
    }

    /// What quitting does besides closing the windows (`on_exit` in the desktop app).
    pub fn finish(&mut self) {
        self.app.frame_exit();
    }
}

/// A native event that happened in a window, as that window's input.
fn window_input<'a>(ev: &RawEvent, text: &'a str) -> Option<WindowInput<'a>> {
    Some(match ev.kind {
        kind::MOUSE_MOVED | kind::MOUSE_DOWN | kind::MOUSE_UP => WindowInput::Pointer { x: ev.x, y: ev.y, buttons: ev.buttons, modifiers: ev.modifiers },
        kind::POINTER_GONE => WindowInput::PointerGone,
        // A wheel's whole steps, or (`repeat` 1) a touchpad's scroll gesture with its unrounded
        // steps and its phase in `clicks` (`host.cpp`).
        kind::WHEEL => WindowInput::Wheel { dx: ev.dx, dy: ev.dy, modifiers: ev.modifiers, gesture: (ev.repeat != 0).then_some(ev.clicks) },
        kind::KEY_DOWN | kind::KEY_UP => {
            WindowInput::Key { pressed: ev.kind == kind::KEY_DOWN, code: ev.key, raw_char: ev.raw_char, modifiers: ev.modifiers, text }
        }
        kind::MODIFIERS => WindowInput::Modifiers(host::modifiers()),
        kind::FOCUS => WindowInput::Focus(ev.x != 0.0),
        kind::CLOSE => WindowInput::Close,
        // Files dropped on a window go to the interface as the desktop app's do: on the canvas
        // they are placed as layers, elsewhere opened (where: `Services::cursor_pos`); on a
        // floating panel's window, its dock area takes them.
        kind::DROPPED_FILES => WindowInput::Dropped { x: ev.x, y: ev.y, paths: text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect() },
        // The next frame picks up the new size, position or state.
        kind::RESIZED | kind::MOVED | kind::MINIMIZED => WindowInput::Changed,
        _ => return None,
    })
}
