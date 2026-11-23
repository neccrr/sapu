use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// Live counters shared between the scanner threads and a UI thread.
/// Lock-free except `current`, which is only updated with `try_lock`.
#[derive(Debug, Default)]
pub struct Progress {
    pub(crate) files: AtomicU64,
    pub(crate) dirs: AtomicU64,
    pub(crate) bytes: AtomicU64,
    pub(crate) errors: AtomicU64,
    pub(crate) current: Mutex<PathBuf>,
    cancel: AtomicBool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
}

impl Progress {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            files: self.files.load(Relaxed),
            dirs: self.dirs.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            errors: self.errors.load(Relaxed),
        }
    }

    /// The folder a scanner thread is in right now (approximate).
    pub fn current_path(&self) -> PathBuf {
        self.current.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Ask the scan to stop early; it returns a partial tree.
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }
}
