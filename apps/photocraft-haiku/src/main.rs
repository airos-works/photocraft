//! PhotoCraft's full editor hosted by the native Be API.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(not(target_os = "haiku"), allow(dead_code))]
#[cfg(target_os = "haiku")]
#[path = "../../photocraft/src/control_server.rs"]
mod control_server;
#[cfg(target_os = "haiku")]
#[path = "../../photocraft/src/crash_guard.rs"]
mod crash_guard;
mod damage;
mod displays;
#[cfg(target_os = "haiku")]
mod gpu;
#[cfg(target_os = "haiku")]
mod host;
mod keys;
#[cfg(target_os = "haiku")]
#[path = "../../photocraft/src/logging.rs"]
mod logging;
mod native_menu;
#[cfg(target_os = "haiku")]
mod present;
mod raster;
#[cfg(target_os = "haiku")]
mod scanout;
#[cfg(target_os = "haiku")]
mod services;
#[cfg(target_os = "haiku")]
mod shell;
mod tablet;

#[cfg(not(target_os = "haiku"))]
fn main() {
    eprintln!("PhotoCraft for air/OS requires Haiku; run `photocraft` on other platforms.");
    std::process::exit(1);
}
#[cfg(target_os = "haiku")]
fn main() {
    if let Err(e) = native_main() {
        eprintln!("PhotoCraft: {e}");
        std::process::exit(1);
    }
}
#[cfg(target_os = "haiku")]
fn native_main() -> Result<(), String> {
    use photocraft_ui_egui::PhotocraftApp;
    host::export_locale();
    let logger = logging::install();
    crash_guard::install_hook();
    let mut files = Vec::new();
    let mut control = None;
    let mut token_file = None;
    let mut read_root = None;
    let mut write_root = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!("PhotoCraft {} for air/OS", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!("PhotoCraft [--control PORT --control-token-file FILE] [--automation-read-root DIR] [--automation-write-root DIR] [FILE…]");
                return Ok(());
            }
            "--control" => {
                control = Some(args.next().and_then(|s| s.parse::<std::num::NonZeroU16>().ok()).ok_or("--control requires a port from 1 to 65535")?.get());
            }
            "--control-token-file" => {
                token_file = Some(std::path::PathBuf::from(args.next().ok_or("missing token file")?));
            }
            "--automation-read-root" => read_root = Some(std::path::PathBuf::from(args.next().ok_or("missing read root")?)),
            "--automation-write-root" => write_root = Some(std::path::PathBuf::from(args.next().ok_or("missing write root")?)),
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}")),
            _ => files.push(arg),
        }
    }
    let automation = if control.is_some() {
        Some(photocraft_automation::AuthorizedWorkspace::new(read_root.as_deref(), write_root.as_deref()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let token = if control.is_some() {
        let path = token_file.as_deref().ok_or("--control requires --control-token-file FILE")?;
        Some(photocraft_automation::security::server_token(None, Some(path)).map_err(|e| e.to_string())?)
    } else {
        None
    };
    if let Some(logger) = logger
        && let Some(dir) = services::config_dir()
    {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        if let Err(e) = logger.attach_dir(&dir.join("logs")) {
            log::warn!(target: "photocraft_haiku", "Log file: {e}");
        }
    }
    host::init(services::SIGNATURE)?;
    services::claim_document_types();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(1, 8));
    if let Err(e) = rayon::ThreadPoolBuilder::new().num_threads(workers).build_global() {
        log::warn!(target: "photocraft_haiku", "Worker pool: {e}");
    }
    let scale = host::ui_scale();
    host::open_window(&host::WindowSpec {
        title: "PhotoCraft".into(),
        position: None,
        size: [1440.0 * scale, 900.0 * scale],
        min_size: [640.0 * scale, 420.0 * scale],
        zoomed: false,
    });
    let worker = std::thread::Builder::new()
        .name("PhotoCraft interface".into())
        .stack_size(16 << 20)
        .spawn(move || {
            let result = crash_guard::guard("Interface", || {
                let ctx = egui::Context::default();
                ctx.set_embed_viewports(true);
                let mut services = services::native(automation);
                services.preset_store = services::config_dir().map(|d| photocraft_engine::preset_store::open_dir_async(d.join("Presets")));
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
                app.custom_titlebar = false;
                app.integrated_titlebar = false;
                app.background_jobs = true;
                if let (Some(port), Some(token)) = (control, token) {
                    app = app.with_control(control_server::start(port, token, ctx.clone()));
                }
                for path in files {
                    if let Err(e) = app.open_path(&path) {
                        app.open_failed(&path, &e);
                    }
                }
                let mut shell = shell::Shell::new(app, ctx, scale)?;
                shell.run();
                shell.finish();
                Ok(())
            });
            if let Err(e) = &result {
                log::error!(target: "photocraft_haiku", "{e}");
                host::alert("PhotoCraft", e);
            }
            host::quit();
            result
        })
        .map_err(|e| e.to_string())?;
    host::run();
    worker.join().map_err(|_| "Interface thread stopped unexpectedly".to_string())?
}
