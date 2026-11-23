//! Where Sapu keeps its own files. Everything is local to this Mac and
//! nothing is ever stored inside Sapu.app, so updating or re-downloading
//! the app keeps your data.
//!
//! ```text
//! ~/Library/Application Support/sapu/   things you'd miss: keep
//!   ai/model.bin          Sapu AI (retrained on every scan)
//!   ai/feedback.tsv       your Keep / trash choices (teaches the AI)
//!   safety/protected.txt  your "never touch" list
//!   safety/journal.jsonl  everything moved to the Trash (powers Undo)
//! ~/Library/Caches/sapu/                safe to delete anytime
//!   scans/<id>.bin        last scan of each folder (instant start)
//! ~/Library/Logs/sapu/                  logs, rotated at 10 MB
//!   sapu-gui.log, sapu.log
//! ```
//!
//! On Linux the same layout lives under the XDG data/cache/state folders.

use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

fn mac() -> bool {
    cfg!(target_os = "macos")
}

/// `~/Library/Application Support/sapu`
pub fn data_dir() -> PathBuf {
    if mac() {
        home().join("Library/Application Support/sapu")
    } else {
        dirs::data_dir().unwrap_or_else(home).join("sapu")
    }
}

pub fn ai_dir() -> PathBuf {
    data_dir().join("ai")
}
pub fn model_file() -> PathBuf {
    ai_dir().join("model.bin")
}
pub fn feedback_file() -> PathBuf {
    ai_dir().join("feedback.tsv")
}

pub fn safety_dir() -> PathBuf {
    data_dir().join("safety")
}
pub fn protected_file() -> PathBuf {
    safety_dir().join("protected.txt")
}
pub fn journal_file() -> PathBuf {
    safety_dir().join("journal.jsonl")
}

/// `~/Library/Caches/sapu`: nothing here is precious.
pub fn cache_dir() -> PathBuf {
    if mac() {
        home().join("Library/Caches/sapu")
    } else {
        dirs::cache_dir().unwrap_or_else(home).join("sapu")
    }
}
pub fn scans_dir() -> PathBuf {
    cache_dir().join("scans")
}

/// `~/Library/Logs/sapu`
pub fn logs_dir() -> PathBuf {
    if mac() {
        home().join("Library/Logs/sapu")
    } else {
        dirs::state_dir().unwrap_or_else(home).join("sapu")
    }
}

/// Move files from the old flat layout (≤ 0.9) into the new folders. Safe to call every start.
pub fn migrate() {
    let old = data_dir();
    for (name, new) in [
        ("model.bin", model_file()),
        ("feedback.tsv", feedback_file()),
        ("protected.txt", protected_file()),
        ("journal.jsonl", journal_file()),
    ] {
        let from = old.join(name);
        if from.is_file() && !new.exists() {
            let moved = new.parent().map(fs::create_dir_all).transpose().and_then(|_| fs::rename(&from, &new));
            match moved {
                Ok(_) => info!(from = %from.display(), to = %new.display(), "moved data file to new layout"),
                Err(e) => warn!(file = %from.display(), error = %e, "couldn't move data file"),
            }
        }
    }
}

/// Keep a log file from growing forever: past `max` bytes it becomes `<name>.1`.
pub fn rotate_log(path: &Path, max: u64) {
    if fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let old = path.with_extension("log.1");
        let _ = fs::rename(path, old);
    }
}

#[derive(Debug, Clone)]
pub struct Usage {
    pub label: &'static str,
    pub note: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    /// Safe to delete without losing anything you'd miss.
    pub clearable: bool,
}

/// How much space each of Sapu's own folders uses.
pub fn usage() -> Vec<Usage> {
    let size = |p: &Path| if p.exists() { crate::scanner::dir_size(p) } else { 0 };
    vec![
        Usage {
            label: "Sapu AI",
            note: "model + your feedback",
            path: ai_dir(),
            bytes: size(&ai_dir()),
            clearable: false,
        },
        Usage {
            label: "Safety",
            note: "protected list + Undo journal",
            path: safety_dir(),
            bytes: size(&safety_dir()),
            clearable: false,
        },
        Usage {
            label: "Scan cache",
            note: "last scans, for an instant start",
            path: cache_dir(),
            bytes: size(&cache_dir()),
            clearable: true,
        },
        Usage { label: "Logs", note: "rotated at 10 MB", path: logs_dir(), bytes: size(&logs_dir()), clearable: false },
    ]
}
