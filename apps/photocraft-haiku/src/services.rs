//! Native air/OS platform services; file and recovery logic follows the desktop host. Printing
//! goes to the print kit (`src/print.rs`).
use crate::{host, print};
use photocraft_codecs::{ChannelLayout, EncodeOptions, Image};
use photocraft_doc::Document;
use photocraft_format::RecoveryStore;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogReply, FileDialogRequest, Recoverable, Services, screen_picker};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub const SIGNATURE: &str = "application/x-vnd.storytold-PhotoCraft";
/// `B_CAPS_LOCK` (`<InterfaceDefs.h>`).
const B_CAPS_LOCK: u32 = 0x8;
static PENDING: Mutex<Option<(i32, bool, bool, FileDialogReply)>> = Mutex::new(None);
static NEXT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(1);
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("PHOTOCRAFT_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/boot/home".into())).join("config/settings/PhotoCraft")))
}
pub fn prefs_file() -> Option<PathBuf> {
    config_dir().map(|p| p.join("preferences.json"))
}
fn recovery_dir() -> Option<PathBuf> {
    if std::env::var_os("PHOTOCRAFT_NO_RECOVERY").is_some() { None } else { config_dir().map(|p| p.join("Recovery")) }
}
pub fn claim_document_types() {
    host::claim_types(SIGNATURE, &["image/vnd.adobe.photoshop", "image/png", "image/jpeg", "application/x-photocraft"]);
}
pub fn clipboard_text() -> Option<String> {
    host::clipboard_get("text/plain").map(|v| String::from_utf8_lossy(&v).trim_end_matches('\0').to_string())
}
fn show_file_dialog(request: FileDialogRequest, _parent: Option<&photocraft_ui_egui::integration::Frame>, reply: FileDialogReply) {
    let (mode, name, extensions) = match request {
        FileDialogRequest::Open { multiple, initial_dir, extensions } => {
            (if multiple { 3 } else { 0 }, initial_dir.unwrap_or_default(), extensions.unwrap_or_default())
        }
        FileDialogRequest::Folder { initial_dir } => (2, initial_dir.unwrap_or_default(), Vec::new()),
        FileDialogRequest::Save { suggested } => (1, suggested, Vec::new()),
    };
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut pending) = PENDING.lock() {
        if pending.is_some() {
            reply.send(Some(FileDialogAnswer::Unavailable("A file panel is already open".into())));
            return;
        }
        *pending = Some((id, mode == 1, mode == 2, reply));
        drop(pending);
        host::file_panel(id, mode, &name, &extensions.iter().map(String::as_str).collect::<Vec<_>>(), "PhotoCraft");
    }
}
pub fn answer(id: i32, picked: bool, text: &str) {
    if let Ok(mut pending) = PENDING.lock()
        && pending.as_ref().is_some_and(|p| p.0 == id)
        && let Some((_, save, folder, reply)) = pending.take()
    {
        let paths: Vec<String> = text.lines().filter(|s| !s.is_empty()).map(str::to_owned).collect();
        let answer = if !picked || paths.is_empty() {
            None
        } else if save {
            paths.first().cloned().map(FileDialogAnswer::SaveTo)
        } else if folder {
            paths.first().cloned().map(FileDialogAnswer::FolderTo)
        } else {
            Some(FileDialogAnswer::Paths(paths))
        };
        reply.send(answer);
    }
}
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    photocraft_format::atomic_write(path, bytes).map_err(|e| e.to_string())?;
    host::type_file(path);
    Ok(())
}

/// [`write_atomic`] reporting the fraction written (a background save's progress, #3389).
fn write_atomic_progress(path: &Path, bytes: &[u8], progress: &dyn Fn(f32)) -> Result<(), String> {
    photocraft_format::atomic_write_progress(path, bytes, progress).map_err(|e| e.to_string())?;
    host::type_file(path);
    Ok(())
}

fn export_options(settings: &photocraft_ui_egui::ExportSettings) -> photocraft_io::ExportOptions {
    settings.io_options()
}

/// The share of a save's progress bar that encoding fills; writing the file fills the rest. A
/// 24 MP, 5-layer PSD (572 MB) takes about 3 s to encode and 0.5 s or more to write and flush.
const ENCODE_SHARE: f32 = 0.85;

/// A save on a worker thread (`Services::save_file` and `Services::automation_save`, #2017):
/// encode, then write through `write`. Cancelling while encoding leaves the target untouched;
/// once writing starts the save can't be cancelled. The encoder's progress fills the first
/// [`ENCODE_SHARE`] of the job's progress, so the status bar and tab read "Saving NN%" (#3389).
fn save_through(
    doc: &Document,
    path: &str,
    settings: &photocraft_ui_egui::ExportSettings,
    ctl: &photocraft_ui_egui::jobs_ui::SaveCtl,
    write: impl Fn(&str, &[u8], &dyn Fn(f32)) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    ctl.progress(0.0, "Encoding");
    let opts = export_options(settings);
    let cancelled = || ctl.check().is_err();
    let progress = |f: f32| ctl.progress(f * ENCODE_SHARE, "");
    let interrupt = photocraft_raster::Interrupt::new(&cancelled, &progress);
    let r = crate::crash_guard::guard("Save", || photocraft_io::export_with(doc, path, &opts, &interrupt).map_err(|e| e.to_string()))?;
    ctl.commit()?;
    ctl.progress(ENCODE_SHARE, "Writing");
    // The last bit is the flush and rename after the last chunk: 100% only once they're done.
    write(path, &r.bytes, &|f| ctl.progress(ENCODE_SHARE + (0.99 - ENCODE_SHARE) * f, ""))?;
    ctl.progress(1.0, "");
    Ok(r.warnings)
}

fn save_file(
    doc: &Document,
    path: &str,
    settings: &photocraft_ui_egui::ExportSettings,
    ctl: &photocraft_ui_egui::jobs_ui::SaveCtl,
) -> Result<Vec<String>, String> {
    save_through(doc, path, settings, ctl, |p, b, progress| write_atomic_progress(Path::new(p), b, progress))
}

type SharedRecovery = Rc<RefCell<Option<RecoveryStore>>>;

/// Run `f` on the recovery store (`Err` without a config directory). The service closures never
/// call each other, so the store is never borrowed twice.
fn with_store<R>(store: &SharedRecovery, f: impl FnOnce(&mut RecoveryStore) -> R) -> Result<R, String> {
    let mut slot = store.try_borrow_mut().map_err(|_| "crash recovery is busy".to_string())?;
    Ok(f(slot.as_mut().ok_or("no config directory")?))
}

/// Crash recovery: background incremental .pcraft autosaves into `dir` (`None`: no config
/// directory, so autosaves fail and nothing is recovered). Recovered documents keep their entries
/// until a newer autosave replaces them or they're saved or closed (see [`RecoveryStore`]).
fn recovery_services(dir: Option<PathBuf>) -> Services {
    let recover_dir = dir.clone();
    let store: SharedRecovery = Rc::new(RefCell::new(dir.map(RecoveryStore::new)));
    let (s1, s2, s4, s5) = (store.clone(), store.clone(), store.clone(), store.clone());
    Services {
        autosave: Some(Box::new(move |doc: &Arc<Document>, revision: u64, path: Option<&str>| {
            with_store(&s1, |s| s.autosave_checked(doc, revision, path.map(str::to_string)))?
        })),
        autosave_results: Some(Box::new(move || with_store(&s4, |s| s.take_completed()).unwrap_or_default())),
        // For a session that ends without asking (upstream: the window's GPU device was lost).
        flush_autosaves: Some(Box::new(move || with_store(&s5, RecoveryStore::flush).unwrap_or_default())),
        discard_autosave: Some(Box::new(move |id: u64| {
            let _ = with_store(&s2, |s| s.discard(id));
        })),
        recover: Some(Box::new(move || {
            let Some(dir) = recover_dir.as_ref() else { return Vec::new() };
            photocraft_format::list_recovery(dir)
                .into_iter()
                .map(|e| Recoverable {
                    key: e.info.key.clone(),
                    name: e.info.document_name.clone(),
                    path: e.info.original_path.clone(),
                    load: Box::new(move || photocraft_format::recover(&e).map_err(|e| e.to_string())),
                })
                .collect()
        })),
        adopt_autosave: Some(Box::new(move |id: u64, key: &str| {
            let _ = with_store(&store, |s| s.adopt(id, key));
        })),
        ..Default::default()
    }
}
pub fn native(automation: Option<photocraft_automation::AuthorizedWorkspace>) -> Services {
    let automation_read = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let bytes = workspace.read(path).map_err(|error| error.to_string())?;
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            Ok((name, bytes))
        }) as photocraft_ui_egui::AutomationReadFn
    });
    let automation_write = automation.clone().map(|workspace| {
        Box::new(move |path: &str, bytes: &[u8]| workspace.write(path, bytes).map_err(|error| error.to_string())) as photocraft_ui_egui::AutomationWriteFn
    });
    let automation_open_pcraft = automation.clone().map(|workspace| {
        Box::new(move |path: &str| {
            let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_string();
            let opened = crate::crash_guard::guard("Open", || workspace.open_pcraft(path).map_err(|e| e.to_string()))?;
            Ok(opened.map(|doc| (name, doc)))
        }) as photocraft_ui_egui::AutomationOpenPcraftFn
    });
    let automation_save = automation.clone().map(|workspace| {
        std::sync::Arc::new(move |doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings, ctl: &photocraft_ui_egui::jobs_ui::SaveCtl| {
            save_through(doc, path, settings, ctl, |p, b, _| workspace.write(p, b).map_err(|e| e.to_string()))
        }) as photocraft_ui_egui::SaveFileFn
    });
    let step: fn(&str, &serde_json::Value) -> photocraft_engine::Result<()> = photocraft_automation::workspace::authorize_desktop_engine_step;
    let automation_authorize = automation.is_some().then_some(step);
    let automation_command = automation.map(|_| {
        Box::new(|id: &str, params: &serde_json::Value| {
            photocraft_automation::workspace::authorize_desktop_engine_command(id, params).map_err(|error| error.to_string())
        }) as photocraft_ui_egui::AutomationCommandFn
    });
    Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], opts: &photocraft_io::ImportOptions| {
            crate::crash_guard::guard("Open", || {
                photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
            })
        })),
        export: Some(Box::new(|doc: &Document, path: &str, settings: &photocraft_ui_egui::ExportSettings| {
            let opts = export_options(settings);
            crate::crash_guard::guard("Export", || photocraft_io::export(doc, path, &opts).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string()))
        })),
        save_file: Some(Arc::new(save_file)),
        file_dialog: Some(Box::new(show_file_dialog)),
        write: Some(Box::new(|path: &str, bytes: &[u8]| write_atomic(Path::new(path), bytes))),
        file_stamp: Some(Box::new(|path: &str| {
            // Follows a symlink, as Save writes through it to the file it points at.
            let meta = std::fs::metadata(path).ok()?;
            Some(photocraft_ui_egui::FileStamp { len: meta.len(), modified: meta.modified().ok() })
        })),
        automation_read,
        automation_write,
        automation_open_pcraft,
        automation_save,
        automation_command,
        automation_authorize,
        encode_png: Some(Box::new(|w, h, rgba| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        inbox: None,
        open_url: Some(Box::new(|url: &str| host::open_url(url).then_some(()).ok_or_else(|| "Could not open URL".into()))),
        clipboard_set_image: Some(Box::new(|w, h, px| {
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, px.to_vec()).map_err(|e| e.to_string())?;
            let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())?;
            host::clipboard_set_picture("image/png", &png).then_some(()).ok_or_else(|| "Clipboard unavailable".into())
        })),
        // No PNG on the clipboard is no image; one that doesn't decode is an error.
        clipboard_get_image: Some(Box::new(|| {
            let Some(bytes) = host::clipboard_get("image/png") else { return Ok(None) };
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
            Ok(Some((img.width(), img.height(), img.into_raw())))
        })),
        load_prefs: Some(Box::new(|| std::fs::read_to_string(prefs_file()?).ok())),
        save_prefs: Some(Box::new(|text: &str| write_atomic(&prefs_file().ok_or("no config directory")?, text.as_bytes()))),
        append_text: Some(Box::new(|path: &str, text: &str| {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| e.to_string())?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())
        })),
        // Preferences › General › Beep When Done: the "Beep" sound of the Sounds preferences.
        beep: Some(host::beep),
        // Caps Lock on: the painting tools show their precise cursor.
        caps_lock: Some(Box::new(|| host::modifiers() & B_CAPS_LOCK != 0)),
        // Appearance › Auto: the panel colour of the Appearance preferences, read at each frame
        // (a change sends B_COLORS_UPDATED, which wakes the interface).
        system_theme: Some(Box::new(|_| Some(if host::dark() { egui::Theme::Dark } else { egui::Theme::Light }))),
        read_displays: Some(Arc::new(|| Some(read_displays()))),
        screen_pick: Some(Box::new(screen_pick)),
        // Native refs arrive through the host event queue.
        os_events: None,
        // Set by main, which starts loading the store before the window opens.
        preset_store: None,
        is_wayland: false,
        ..recovery_services(recovery_dir())
    }
}

/// Reads the displays and their profiles on a thread of its own (`Services::read_displays`).
pub fn read_displays() -> std::sync::mpsc::Receiver<photocraft_ui_egui::monitor_status::Detection> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("display profiles".into()).spawn(move || {
        let dir = crate::displays::profiles_dir();
        // Made at the first read, so the user finds where profiles go.
        if let Some(d) = &dir
            && let Err(e) = std::fs::create_dir_all(d)
        {
            log::debug!(target: "photocraft_haiku", "{}: {e}", d.display());
        }
        let _ = tx.send(crate::displays::read(&host::displays(), dir.as_deref(), host::ui_scale()));
    });
    if let Err(e) = spawned {
        log::warn!(target: "photocraft_haiku", "display profiles: {e}");
    }
    rx
}

/// The screen colour pick in progress: the answer's way back, and whether the interface stopped
/// waiting for it.
struct Pick {
    id: i32,
    tx: std::sync::mpsc::Sender<Result<screen_picker::Capture, String>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    ctx: egui::Context,
}
static PICK: Mutex<Option<Pick>> = Mutex::new(None);

/// `Services::screen_pick`: the native picker (`PickerWindow` in host.cpp) over a snapshot of the
/// whole screen. The interface's own picker shows its snapshots in viewports of their own, which
/// this host embeds in the window; the native one covers every monitor and answers one colour.
fn screen_pick(ctx: &egui::Context) -> screen_picker::Pending {
    let (tx, rx) = std::sync::mpsc::channel();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = photocraft_ui_egui::i18n::t;
    if host::screen_pick(id, t("Click to pick · Esc to cancel"), t("Scroll to zoom")) {
        if let Ok(mut pick) = PICK.lock() {
            *pick = Some(Pick { id, tx, cancelled: cancelled.clone(), ctx: ctx.clone() });
        }
    } else {
        let _ = tx.send(Err("The screen couldn't be read".into()));
    }
    screen_picker::Pending { receiver: rx, cancelled }
}

/// The picker's answer (a [`host::kind::SCREEN_PICK`] event): the pixel `rgb` (0xRRGGBB) at the
/// screen point (`x`, `y`), converted to sRGB through its display's profile, or no colour.
pub fn pick_answer(id: i32, picked: bool, rgb: u32, x: f32, y: f32) {
    let Some(pick) = PICK.lock().ok().and_then(|mut p| p.take_if(|p| p.id == id)) else { return };
    let answer = if picked {
        let [_, r, g, b] = rgb.to_be_bytes();
        let profile = crate::displays::profile_at(&host::displays(), crate::displays::profiles_dir().as_deref(), f64::from(x), f64::from(y));
        crate::displays::to_srgb([r, g, b], profile.as_deref()).map(|c| screen_picker::Capture::Color(Some(c)))
    } else {
        Ok(screen_picker::Capture::Color(None))
    };
    let _ = pick.tx.send(answer);
    pick.ctx.request_repaint();
}

/// Closes the picker when the interface stopped waiting for its pick (another document became
/// active, a dialog opened). Called every frame.
pub fn check_pick() {
    let Some(id) = PICK.lock().ok().and_then(|mut p| p.take_if(|p| p.cancelled.load(std::sync::atomic::Ordering::Relaxed))).map(|p| p.id) else {
        return;
    };
    host::screen_pick_cancel(id);
}

/// File › Print's route to the printer on air/OS: the print-ready PDF the editor makes (its
/// page is the paper, the image placed on it with the marks) drawn into a `BPrintJob` for the
/// chosen printer (`src/print.rs`), on the print command's worker. It is preferred, so jobs
/// don't look for CUPS. Shift held at Print (or `PHOTOCRAFT_PRINT_SETUP=1`) opens the printer
/// driver's page and job setup first; their answers are kept for that printer in
/// `~/config/settings/PhotoCraft/printers` and used by every later job on it.
pub fn print_spooler() -> photocraft_engine::print_cmds::Spooler {
    use photocraft_engine::print_cmds::{PrintJob, Spooler};
    Spooler {
        name: "print kit",
        preferred: true,
        run: Arc::new(|job: &PrintJob| {
            let options = print::Options {
                title: job.title.clone(),
                copies: job.copies,
                // A photo prints in colour: libprint's drivers start in monochrome, and their
                // job setup's choice (Shift+Print) stays once made.
                color: true,
                // The page is laid out on the paper in the orientation the dialog chose.
                landscape: None,
                setup: print::shift_down() || std::env::var_os("PHOTOCRAFT_PRINT_SETUP").is_some_and(|v| v != "0"),
                ..print::Options::default()
            };
            let store = print::Store::new(config_dir().map(|d| d.join("printers")));
            let started = std::time::Instant::now();
            // The dialog's "default printer" is the print server's; without one, the system's
            // print setup asks which.
            let printer = job.printer.clone().or_else(|| print::printers().into_iter().find(|p| p.default).map(|p| p.name));
            let printer = printer.as_deref();
            let result = print::print_pdf(printer, job.pdf.clone(), &options, &store).map_err(|e| e.to_string());
            match &result {
                Ok(receipt) => log::info!(target: "photocraft_haiku", "printed on {printer:?} in {:.1} s: {receipt}", started.elapsed().as_secs_f32()),
                Err(e) => log::warn!(target: "photocraft_haiku", "printing on {printer:?}: {e}"),
            }
            result
        }),
    }
}

/// The print server's printers for File › Print, the default first.
pub fn printer_source() -> photocraft_engine::print_cmds::PrinterSource {
    Arc::new(|| {
        print::printers().into_iter().map(|p| craft_print::Printer { name: p.name, description: String::new(), default: p.default, two_sided: None }).collect()
    })
}
