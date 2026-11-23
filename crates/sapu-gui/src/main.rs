//! Sapu window app. `cargo run --release` (from the workspace root) opens it.
//! Optional first argument: folder to scan (default: home).

mod app;
mod fmt;
mod glass;
mod logview;
mod theme;
mod treemap;

use eframe::egui;
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result {
    let (log_file, buffer) = init_logging();
    install_panic_hook();
    log_startup(log_file.as_deref());

    let root =
        std::env::args_os().nth(1).map(PathBuf::from).or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("/"));
    tracing::info!(root = %root.display(), "scan root");

    let options = eframe::NativeOptions {
        viewport: {
            let v = egui::ViewportBuilder::default()
                .with_title("Sapu")
                .with_inner_size([1180.0, 760.0])
                .with_min_inner_size([760.0, 460.0]);
            // macOS: translucent window, content runs under a hidden title bar
            // (the traffic lights float over Sapu's own top bar).
            if cfg!(target_os = "macos") {
                // Only see-through when the blur is requested; otherwise solid.
                v.with_transparent(glass::requested())
                    .with_fullsize_content_view(true)
                    .with_title_shown(false)
                    .with_titlebar_shown(false)
            } else {
                v
            }
        },
        ..Default::default()
    };

    let result = eframe::run_native(
        "Sapu",
        options,
        Box::new(move |cc| Ok(Box::new(app::SapuApp::new(cc, root, log_file, buffer)))),
    );
    match &result {
        Ok(()) => tracing::info!("sapu-gui closed"),
        Err(e) => tracing::error!(error = %e, "window system error"),
    }
    result
}

/// Two destinations:
/// - the log file `~/Library/Logs/sapu/sapu-gui.log` (info and up; `SAPU_LOG` to change),
/// - an in-memory buffer for the 📜 Logs window (debug and up from Sapu).
fn init_logging() -> (Option<PathBuf>, logview::LogBuffer) {
    use tracing::Level;
    use tracing_subscriber::{filter::Targets, prelude::*};

    let buffer = logview::LogBuffer::new();
    let path = sapu_core::paths::logs_dir().join("sapu-gui.log");
    sapu_core::paths::rotate_log(&path, 10 * 1024 * 1024);
    let file = path
        .parent()
        .and_then(|d| fs::create_dir_all(d).ok())
        .and_then(|_| OpenOptions::new().create(true).append(true).open(&path).ok());

    let file_filter =
        EnvFilter::try_from_env("SAPU_LOG").unwrap_or_else(|_| EnvFilter::new("warn,sapu_gui=info,sapu_core=info"));
    let file_layer = file
        .map(|f| tracing_subscriber::fmt::layer().with_writer(Mutex::new(f)).with_ansi(false).with_filter(file_filter));
    let memory = Targets::new()
        .with_default(Level::WARN)
        .with_target("sapu_gui", Level::TRACE)
        .with_target("sapu_core", Level::DEBUG);

    tracing_subscriber::registry().with(file_layer).with(buffer.clone().with_filter(memory)).init();
    let has_file = path.exists().then_some(path);
    (has_file, buffer)
}

/// Crashes end up in the log (and the Logs window) with where they happened.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        tracing::error!(%location, %message, "PANIC");
        default(info);
    }));
}

/// Facts that help when something goes wrong.
fn log_startup(log_file: Option<&std::path::Path>) {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        "sapu-gui starting"
    );
    if let Some(f) = log_file {
        tracing::info!(file = %f.display(), "logging to file");
    }
    // Full Disk Access check: protected folders can only be listed with it.
    if let Some(home) = dirs::home_dir() {
        if cfg!(target_os = "macos") {
            let probe = home.join("Library/Safari");
            let fda = std::fs::read_dir(&probe).is_ok();
            if fda {
                tracing::info!("Full Disk Access: yes");
            } else {
                tracing::warn!(
                    "Full Disk Access: no. Some folders (Mail, Messages, Safari, Trash) can't be scanned or undone"
                );
            }
        }
        let icloud = sapu_core::icloud::available(&home);
        tracing::info!(
            icloud_drive = icloud,
            desktop_documents_in_icloud = sapu_core::icloud::desktop_documents_synced(&home),
            "iCloud"
        );
    }
    sapu_core::paths::migrate();
    tracing::info!(
        data = %sapu_core::paths::data_dir().display(),
        cache = %sapu_core::paths::cache_dir().display(),
        logs = %sapu_core::paths::logs_dir().display(),
        "Sapu's folders"
    );
}
