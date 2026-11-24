//! Swift bindings for `sapu-core`, generated with UniFFI.
//!
//! This is what the SwiftUI shell in `macos/SapuShell` talks to. Everything
//! that changes files goes through the same core calls Sapu.app uses, with the
//! same rules:
//!
//! - everything goes to the Trash, logged in the journal first, and can be undone;
//! - each item is re-checked against the scan right before it's touched;
//! - the hard safety rules and your protected list always apply;
//! - AI guesses, Review items and iCloud items are never pre-selected;
//! - risky batches (Review/AI items, over 20 GB, over 100 items) need an explicit "I've checked";
//! - deleting from iCloud needs consent for that one item, and whole containers are refused;
//! - every action is logged to `~/Library/Logs/sapu/sapu-shell.log`.

use sapu_core::{
    cleaner::{self, Expect},
    icloud,
    journal::ItemState,
    judge::{Action, Category, Source},
    ml, paths, safety, Advice, Journal, Judge, Judgement, Node, Progress, Recommendation, ScanOptions, Tree, Verdict,
};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Once};
use tracing::{info, warn};

uniffi::setup_scaffolding!();

const RISKY_BYTES: u64 = 20_000_000_000;
const RISKY_COUNT: usize = 100;

// ── Setup ───────────────────────────────────────────────────────────

static LOGGING: Once = Once::new();

/// Start logging to `~/Library/Logs/sapu/sapu-shell.log` (rotated at 10 MB).
/// Safe to call more than once. `SAPU_LOG` changes the level, as in Sapu.app.
#[uniffi::export]
pub fn start_logging() {
    LOGGING.call_once(|| {
        use tracing_subscriber::{prelude::*, EnvFilter};
        let path = log_file();
        paths::rotate_log(&path, 10 * 1024 * 1024);
        let file = path
            .parent()
            .and_then(|d| fs::create_dir_all(d).ok())
            .and_then(|_| OpenOptions::new().create(true).append(true).open(&path).ok());
        let Some(file) = file else { return };
        let filter =
            EnvFilter::try_from_env("SAPU_LOG").unwrap_or_else(|_| EnvFilter::new("warn,sapu_ffi=info,sapu_core=info"));
        let layer = tracing_subscriber::fmt::layer().with_writer(Mutex::new(file)).with_ansi(false).with_filter(filter);
        if tracing_subscriber::registry().with(layer).try_init().is_ok() {
            info!(version = env!("CARGO_PKG_VERSION"), arch = std::env::consts::ARCH, "sapu-shell starting");
        }
    });
}

fn log_file() -> PathBuf {
    paths::logs_dir().join("sapu-shell.log")
}

/// Where the shell's log is.
#[uniffi::export]
pub fn log_path() -> String {
    log_file().display().to_string()
}

/// Log something the window did (a click, a dialog) next to the engine's own lines.
#[uniffi::export]
pub fn log_event(message: String) {
    info!(target: "sapu_ffi::ui", "{message}");
}

/// The last `lines` lines of the log, oldest first.
#[uniffi::export]
pub fn recent_log(lines: u32) -> Vec<String> {
    let text = fs::read_to_string(log_file()).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines as usize);
    all[start..].iter().map(|s| s.to_string()).collect()
}

/// Sapu's version, e.g. "0.10.0".
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The user's home folder, the usual place to start a scan.
#[uniffi::export]
pub fn home_folder() -> String {
    cleaner::home().map(|p| p.display().to_string()).unwrap_or_else(|_| "/".into())
}

/// Whether this app can read folders macOS protects (Mail, Safari, the Trash).
#[uniffi::export]
pub fn has_full_disk_access() -> bool {
    dirs::home_dir().map(|h| fs::read_dir(h.join("Library/Safari")).is_ok()).unwrap_or(false)
}

// ── Types ───────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SapuError {
    #[error("{message}")]
    Failed { message: String },
    #[error("{message}")]
    NotFound { message: String },
    /// The batch needs an explicit "I've checked" first. `message` says why.
    #[error("{message}")]
    NeedsConfirmation { message: String },
}

fn failed(e: impl ToString) -> SapuError {
    SapuError::Failed { message: e.to_string() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VerdictKind {
    Safe,
    Likely,
    Review,
    Keep,
}

impl From<Verdict> for VerdictKind {
    fn from(v: Verdict) -> Self {
        match v {
            Verdict::Safe => VerdictKind::Safe,
            Verdict::Likely => VerdictKind::Likely,
            Verdict::Review => VerdictKind::Review,
            Verdict::Keep => VerdictKind::Keep,
        }
    }
}

impl From<VerdictKind> for Verdict {
    fn from(v: VerdictKind) -> Self {
        match v {
            VerdictKind::Safe => Verdict::Safe,
            VerdictKind::Likely => Verdict::Likely,
            VerdictKind::Review => Verdict::Review,
            VerdictKind::Keep => Verdict::Keep,
        }
    }
}

/// What cleaning an item would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CleanAction {
    /// Move the item to the Trash.
    Trash,
    /// Move what's inside to the Trash; the folder stays.
    TrashContents,
    /// Remove the download; it stays in iCloud.
    RemoveDownload,
    /// Sapu can't do this one for you; the reason says what to do.
    Manual,
}

impl From<Action> for CleanAction {
    fn from(a: Action) -> Self {
        match a {
            Action::TrashItem => CleanAction::Trash,
            Action::TrashContents => CleanAction::TrashContents,
            Action::Evict => CleanAction::RemoveDownload,
            Action::EmptyTrash | Action::Manual => CleanAction::Manual,
        }
    }
}

/// Plain words for a verdict, the same ones Sapu.app uses.
#[uniffi::export]
pub fn verdict_label(verdict: VerdictKind) -> String {
    Verdict::from(verdict).label().to_string()
}

/// A verdict on one item, and why.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Opinion {
    pub verdict: VerdictKind,
    pub category: String,
    pub reason: String,
    pub action: CleanAction,
    /// Guessed by Sapu AI rather than a rule. Never pre-selected.
    pub from_ai: bool,
    /// AI confidence, 0–1 (1 for rules).
    pub confidence: f32,
}

impl From<&Judgement> for Opinion {
    fn from(j: &Judgement) -> Self {
        let confidence = match j.source {
            Source::Rule => 1.0,
            Source::Model { confidence } => confidence,
        };
        Opinion {
            verdict: j.verdict.into(),
            category: j.category.label().to_string(),
            reason: j.reason.clone(),
            action: j.action.into(),
            from_ai: j.is_ai(),
            confidence,
        }
    }
}

/// One file or folder in the scanned tree.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    /// Child indices from the root. Pass back to [`ScanResult::children`].
    pub idx_path: Vec<u32>,
    /// Bytes on disk.
    pub size: u64,
    /// Bytes as the file reports them (bigger than `size` when it's only in iCloud).
    pub logical: u64,
    pub files: u64,
    pub is_dir: bool,
    pub has_children: bool,
    /// Seconds since 1970.
    pub modified: u64,
    /// Something inside couldn't be read (usually missing Full Disk Access).
    pub incomplete: bool,
    /// Lives in iCloud Drive or an app's iCloud folder.
    pub in_icloud: bool,
    pub opinion: Option<Opinion>,
}

/// A recommendation: something worth cleaning.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Suggestion {
    pub path: String,
    pub idx_path: Vec<u32>,
    pub size: u64,
    pub opinion: Opinion,
    /// Sapu can do this itself (Trash, Trash contents or Remove download).
    pub automatic: bool,
    /// Ticked to begin with, as in Sapu.app: rule-based Safe and Likely items only.
    /// AI guesses, Review items and iCloud items never are.
    pub preselected: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ScanProgress {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current_path: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ScanSummary {
    pub root_path: String,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Items that couldn't be read.
    pub errors: u64,
    pub seconds: f64,
    pub cancelled: bool,
}

/// What a clean did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CleanReport {
    /// Items moved to the Trash (or downloads removed).
    pub done: u32,
    pub freed_bytes: u64,
    /// Items skipped, with why (changed since the scan, protected, …).
    pub skipped: Vec<Skipped>,
    /// The History entry, for Undo.
    pub batch_id: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

/// One entry in History.
#[derive(Debug, Clone, uniffi::Record)]
pub struct HistoryBatch {
    pub id: u64,
    /// Seconds since 1970.
    pub time: u64,
    pub label: String,
    pub items: u32,
    pub bytes: u64,
    /// How many items Undo can still put back.
    pub restorable: u32,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UndoReport {
    pub restored: u32,
    pub failed: Vec<Skipped>,
}

/// Where Sapu keeps its data on this Mac.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DataLocation {
    pub name: String,
    pub note: String,
    pub path: String,
    pub bytes: u64,
    /// Safe to delete without losing anything you'd miss.
    pub clearable: bool,
}

#[uniffi::export]
pub fn data_locations() -> Vec<DataLocation> {
    paths::usage()
        .into_iter()
        .map(|u| DataLocation {
            name: u.label.into(),
            note: u.note.into(),
            path: u.path.display().to_string(),
            bytes: u.bytes,
            clearable: u.clearable,
        })
        .collect()
}

/// Empty the scan cache (`~/Library/Caches/sapu/scans`). Nothing you'd miss.
#[uniffi::export]
pub fn clear_cache() -> Result<(), SapuError> {
    sapu_core::cache::clear().map_err(failed)?;
    info!("scan cache cleared");
    Ok(())
}

// ── Safety tab: protected list, History, Undo ───────────────────────

#[uniffi::export]
pub fn protected_paths() -> Vec<String> {
    safety::user_protected().iter().map(|p| p.display().to_string()).collect()
}

/// Never touch this path (or any folder containing it).
#[uniffi::export]
pub fn protect(path: String) -> Result<(), SapuError> {
    safety::protect(Path::new(&path)).map_err(failed)
}

#[uniffi::export]
pub fn unprotect(path: String) -> Result<bool, SapuError> {
    safety::unprotect(Path::new(&path)).map_err(failed)
}

/// Everything Sapu (any of its apps) has moved to the Trash, newest first.
#[uniffi::export]
pub fn history() -> Vec<HistoryBatch> {
    Journal::open()
        .history()
        .into_iter()
        .map(|b| HistoryBatch {
            id: b.id,
            time: b.time,
            items: b.count(ItemState::Trashed) as u32,
            bytes: b.bytes_trashed(),
            restorable: b.restorable() as u32,
            paths: b.items.iter().map(|i| i.path.display().to_string()).collect(),
            label: b.label,
        })
        .collect()
}

/// Put a whole batch back from the Trash.
#[uniffi::export]
pub fn undo(batch_id: u64) -> UndoReport {
    info!(batch = batch_id, "undo requested");
    let report = Journal::open().undo(batch_id);
    info!(batch = batch_id, restored = report.restored, failed = report.failed.len(), "undo finished");
    UndoReport {
        restored: report.restored as u32,
        failed: report
            .failed
            .into_iter()
            .map(|(p, e)| {
                warn!(path = %p.display(), error = %e, "undo failed");
                Skipped { path: p.display().to_string(), reason: e }
            })
            .collect(),
    }
}

// ── Scanning ────────────────────────────────────────────────────────

/// Runs one scan at a time. Ask for progress or cancel from any thread
/// while `scan` runs on a background thread.
#[derive(uniffi::Object)]
pub struct DiskScanner {
    progress: Mutex<Arc<Progress>>,
}

#[uniffi::export]
impl DiskScanner {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(DiskScanner { progress: Mutex::new(Arc::new(Progress::default())) })
    }

    /// Scan `path` (blocking). With `use_ai`, Sapu AI is retrained on the
    /// result, exactly like a scan in Sapu.app.
    pub fn scan(&self, path: String, use_ai: bool) -> Result<Arc<ScanResult>, SapuError> {
        let progress = Arc::new(Progress::default());
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = progress.clone();
        info!(path = %path, use_ai, "scan started");
        let tree = sapu_core::scan(Path::new(&path), &ScanOptions::default(), &progress).map_err(|e| {
            warn!(path = %path, error = %e, "scan failed");
            failed(e)
        })?;
        let (files, bytes, errors, cancelled) =
            (tree.stats.files, tree.root.size, tree.stats.errors, tree.stats.cancelled);
        let walk_ms = tree.stats.elapsed.as_millis() as u64;
        let started = std::time::Instant::now();
        // Judging, recommendations and Sapu AI training happen here. "scan done" is
        // logged after them, as Sapu.app does, so scripts/perf-macos.sh compares like with like.
        let result = ScanResult::new(tree, use_ai);
        let judge_ms = started.elapsed().as_millis() as u64;
        info!(files, bytes, errors, cancelled, ms = walk_ms, judge_ms, "scan done");
        Ok(Arc::new(result))
    }

    pub fn progress(&self) -> ScanProgress {
        let p = self.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let s = p.snapshot();
        ScanProgress {
            files: s.files,
            dirs: s.dirs,
            bytes: s.bytes,
            errors: s.errors,
            current_path: p.current_path().display().to_string(),
        }
    }

    /// Stop early. `scan` then returns what it found so far.
    pub fn cancel(&self) {
        info!("scan cancel requested");
        self.progress.lock().unwrap_or_else(|e| e.into_inner()).cancel();
    }
}

/// A finished scan, judged. Cleaning works from this snapshot and re-checks
/// each item on disk first; rescan afterwards to see the new totals.
#[derive(uniffi::Object)]
pub struct ScanResult {
    tree: Tree,
    judge: Judge,
    model: Option<ml::Model>,
    advice: Advice,
    icloud_roots: Vec<PathBuf>,
}

impl ScanResult {
    fn new(tree: Tree, use_ai: bool) -> Self {
        let judge = Judge::new();
        let model = use_ai.then(|| ml::train(&tree, &judge));
        let advice = sapu_core::advise_with(&tree, &judge, model.as_ref());
        let icloud_roots = dirs::home_dir().map(|h| icloud::roots(&h)).unwrap_or_default();
        ScanResult { tree, judge, model, advice, icloud_roots }
    }

    fn entry(&self, node: &Node, idx: Vec<usize>) -> FileEntry {
        let path = self.tree.path_of(&idx);
        let opinion = ml::judge_with(&self.judge, self.model.as_ref(), &path, node).as_ref().map(Opinion::from);
        FileEntry {
            name: node.name.to_string(),
            in_icloud: icloud::in_icloud(&path, &self.icloud_roots),
            path: path.display().to_string(),
            idx_path: to_u32(&idx),
            size: node.size,
            logical: node.logical,
            files: node.files,
            is_dir: node.is_dir(),
            has_children: !node.children.is_empty(),
            modified: node.modified,
            incomplete: node.incomplete,
            opinion,
        }
    }

    fn node(&self, idx_path: &[u32]) -> Result<(Vec<usize>, &Node), SapuError> {
        let idx: Vec<usize> = idx_path.iter().map(|&i| i as usize).collect();
        let node = self.tree.root.get(&idx).ok_or_else(|| SapuError::NotFound {
            message: "That item isn't in this scan any more. Rescan to refresh.".into(),
        })?;
        Ok((idx, node))
    }

    fn picked(&self, paths: &[String]) -> Vec<&Recommendation> {
        self.advice.items.iter().filter(|r| r.is_automatic() && paths.iter().any(|p| Path::new(p) == r.path)).collect()
    }
}

/// The same rule Sapu.app uses for what's ticked to begin with.
fn preselected(r: &Recommendation) -> bool {
    r.is_automatic()
        && !r.judgement.is_ai()
        && r.judgement.category != Category::ICloud
        && r.judgement.verdict <= Verdict::Likely
}

#[uniffi::export]
impl ScanResult {
    pub fn summary(&self) -> ScanSummary {
        let s = &self.tree.stats;
        ScanSummary {
            root_path: self.tree.root_path.display().to_string(),
            bytes: self.tree.root.size,
            files: s.files,
            dirs: s.dirs,
            errors: s.errors,
            seconds: s.elapsed.as_secs_f64(),
            cancelled: s.cancelled,
        }
    }

    /// The scanned folder itself.
    pub fn root(&self) -> FileEntry {
        self.entry(&self.tree.root, Vec::new())
    }

    /// What's inside a folder, largest first. `idx_path` is empty for the root.
    pub fn children(&self, idx_path: Vec<u32>) -> Result<Vec<FileEntry>, SapuError> {
        let (idx, node) = self.node(&idx_path)?;
        let mut order: Vec<usize> = (0..node.children.len()).collect();
        order.sort_by(|&a, &b| node.children[b].size.cmp(&node.children[a].size));
        Ok(order
            .into_iter()
            .map(|i| {
                let mut child = idx.clone();
                child.push(i);
                self.entry(&node.children[i], child)
            })
            .collect())
    }

    /// Everything worth cleaning: Safe first, then Likely, then Review.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        self.advice
            .items
            .iter()
            .map(|r| Suggestion {
                path: r.path.display().to_string(),
                idx_path: to_u32(&r.idx_path),
                size: r.size,
                opinion: Opinion::from(&r.judgement),
                automatic: r.is_automatic(),
                preselected: preselected(r),
            })
            .collect()
    }

    /// The largest files anywhere in the scan.
    pub fn largest_files(&self, count: u32) -> Vec<FileEntry> {
        self.tree
            .largest_files(count as usize)
            .into_iter()
            .map(|(path, size)| FileEntry {
                name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                in_icloud: icloud::in_icloud(&path, &self.icloud_roots),
                path: path.display().to_string(),
                idx_path: Vec::new(),
                size,
                logical: size,
                files: 1,
                is_dir: false,
                has_children: false,
                modified: 0,
                incomplete: false,
                opinion: None,
            })
            .collect()
    }

    /// If cleaning these needs an explicit "I've checked", the sentence to show
    /// next to that tick box. `None` means a plain confirmation is enough.
    pub fn confirmation_needed(&self, paths: Vec<String>) -> Option<String> {
        let picked = self.picked(&paths);
        let n = picked.len();
        let bytes: u64 = picked.iter().map(|r| r.size).sum();
        let reviewish = picked.iter().filter(|r| r.judgement.is_ai() || r.judgement.verdict == Verdict::Review).count();
        if reviewish > 0 {
            Some(format!("{reviewish} of these are Review items or AI guesses. I've looked at each one."))
        } else if bytes > RISKY_BYTES {
            Some(format!("This moves {} at once. I've checked the list.", sapu_core::judge::human_size(bytes)))
        } else if n > RISKY_COUNT {
            Some(format!("This moves {n} items at once. I've checked the list."))
        } else {
            None
        }
    }

    /// Clean the chosen recommendations (blocking; call it off the main thread).
    /// Each item is re-checked against this scan and skipped if it changed.
    pub fn clean(&self, paths: Vec<String>, checked: bool) -> Result<CleanReport, SapuError> {
        if let Some(why) = self.confirmation_needed(paths.clone()) {
            if !checked {
                return Err(SapuError::NeedsConfirmation { message: why });
            }
        }
        let picked = self.picked(&paths);
        if picked.is_empty() {
            return Err(failed("Nothing to clean: none of these are recommendations Sapu can clean itself."));
        }
        let scanned_at = self.tree.stats.started_at;
        info!(items = picked.len(), checked, "clean started");

        // Accepting an AI guess is a training example.
        for r in &picked {
            if r.judgement.is_ai() {
                if let Some(node) = self.tree.root.get(&r.idx_path) {
                    let _ = ml::record_feedback(&r.path, node, r.judgement.verdict, &self.judge);
                }
            }
        }

        let journal = Journal::open();
        let batch = journal.begin("Cleaned recommendations");
        let mut report = CleanReport { done: 0, freed_bytes: 0, skipped: Vec::new(), batch_id: batch };
        for r in picked {
            let Some(node) = self.tree.root.get(&r.idx_path) else { continue };
            // Caches change all the time by design; everything else must be untouched since the scan.
            let check_mtime = r.judgement.action != Action::TrashContents;
            let expect = Expect::from_node(node, scanned_at, check_mtime);
            match cleaner::apply(&r.path, &r.judgement, Some(&expect), &journal, batch) {
                Ok(applied) => {
                    report.done += applied.trashed.max(1) as u32;
                    report.freed_bytes += r.size;
                    for (p, e) in applied.failed {
                        report.skipped.push(Skipped { path: p.display().to_string(), reason: e });
                    }
                }
                Err(e) => {
                    warn!(path = %r.path.display(), error = %e, "recommendation skipped");
                    report.skipped.push(Skipped { path: r.path.display().to_string(), reason: e.to_string() });
                }
            }
        }
        info!(done = report.done, freed = report.freed_bytes, skipped = report.skipped.len(), "clean finished");
        Ok(report)
    }

    /// Move one hand-picked item to the Trash, after it's re-checked.
    /// Items in iCloud are refused here; see [`ScanResult::delete_from_icloud`].
    pub fn move_to_trash(&self, idx_path: Vec<u32>) -> Result<CleanReport, SapuError> {
        self.trash_one(&idx_path, safety::ICloud::Refuse)
    }

    /// Delete one iCloud item from **all your devices** (it goes to this Mac's
    /// Trash, so Undo still works here). Refused unless `i_understand` is true,
    /// which the window only sets after the person agrees for this one item.
    /// Whole iCloud folders are always refused.
    pub fn delete_from_icloud(&self, idx_path: Vec<u32>, i_understand: bool) -> Result<CleanReport, SapuError> {
        if !i_understand {
            return Err(SapuError::NeedsConfirmation {
                message: "Deleting from iCloud removes it from all your devices. Confirm for this item first.".into(),
            });
        }
        self.trash_one(&idx_path, safety::ICloud::ConsentGiven)
    }

    /// Remove the local download of an iCloud item. Nothing is deleted.
    pub fn remove_download(&self, idx_path: Vec<u32>) -> Result<(), SapuError> {
        let (idx, _) = self.node(&idx_path)?;
        icloud::evict(&self.tree.path_of(&idx)).map_err(failed)
    }

    /// Download an iCloud item to this Mac now.
    pub fn download(&self, idx_path: Vec<u32>) -> Result<(), SapuError> {
        let (idx, _) = self.node(&idx_path)?;
        icloud::download(&self.tree.path_of(&idx)).map_err(failed)
    }

    /// Mark an item as Keep. Sapu AI learns from it on the next scan.
    pub fn mark_keep(&self, idx_path: Vec<u32>) -> Result<(), SapuError> {
        let (idx, node) = self.node(&idx_path)?;
        let path = self.tree.path_of(&idx);
        ml::record_feedback(&path, node, Verdict::Keep, &self.judge).map_err(failed)?;
        info!(path = %path.display(), "marked Keep");
        Ok(())
    }
}

impl ScanResult {
    fn trash_one(&self, idx_path: &[u32], consent: safety::ICloud) -> Result<CleanReport, SapuError> {
        let (idx, node) = self.node(idx_path)?;
        let path = self.tree.path_of(&idx);
        let expect = Expect::from_node(node, self.tree.stats.started_at, true);
        cleaner::revalidate(&path, &expect).map_err(|e| {
            info!(path = %path.display(), reason = %e, "skipped by re-check");
            failed(format!("Skipped: {e}. Rescan and try again."))
        })?;
        let journal = Journal::open();
        let icloud_delete = consent == safety::ICloud::ConsentGiven;
        let label = if icloud_delete {
            format!("Deleted {} from iCloud", path.display())
        } else {
            format!("Moved {} to Trash", path.display())
        };
        let note = if icloud_delete { "iCloud delete, consent given" } else { "picked by hand" };
        let batch = journal.begin(&label);
        cleaner::trash_item(&path, &journal, batch, node.size, note, consent).map_err(failed)?;
        // You chose to delete it: a "probably safe" example for the AI.
        let _ = ml::record_feedback(&path, node, Verdict::Likely, &self.judge);
        Ok(CleanReport { done: 1, freed_bytes: node.size, skipped: Vec::new(), batch_id: batch })
    }
}

fn to_u32(idx: &[usize]) -> Vec<u32> {
    idx.iter().map(|&i| i as u32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sapu-ffi-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("big")).unwrap();
        fs::create_dir_all(dir.join("small")).unwrap();
        fs::write(dir.join("big/a.bin"), vec![1u8; 300_000]).unwrap();
        fs::write(dir.join("small/b.txt"), b"hello").unwrap();
        dir
    }

    #[test]
    fn scans_and_browses_without_ai() {
        let dir = fixture("browse");
        let scanner = DiskScanner::new();
        let result = scanner.scan(dir.display().to_string(), false).unwrap();

        let summary = result.summary();
        assert_eq!(summary.files, 2);
        assert!(!summary.cancelled);

        let top = result.children(Vec::new()).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].name, "big", "largest first");
        assert!(top[0].is_dir && top[0].has_children);

        let inside = result.children(top[0].idx_path.clone()).unwrap();
        assert_eq!(inside[0].name, "a.bin");
        assert!(inside[0].path.ends_with("big/a.bin"));

        assert!(result.children(vec![99]).is_err());
        assert_eq!(result.largest_files(1)[0].size, inside[0].size);
        assert!(scanner.progress().files >= 2);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_unsafe_and_unconfirmed_actions() {
        let dir = fixture("guard");
        let result = DiskScanner::new().scan(dir.display().to_string(), false).unwrap();

        // iCloud delete without consent is refused before anything is touched.
        let top = result.children(Vec::new()).unwrap();
        let err = result.delete_from_icloud(top[0].idx_path.clone(), false).unwrap_err();
        assert!(matches!(err, SapuError::NeedsConfirmation { .. }));
        assert!(dir.join("big/a.bin").exists());

        // Nothing here is a recommendation, so a clean does nothing.
        assert!(result.clean(vec![dir.join("big").display().to_string()], true).is_err());
        assert!(dir.join("big").exists());

        // An item that changed since the scan is skipped by the re-check.
        fs::remove_file(dir.join("small/b.txt")).unwrap();
        let small = top.iter().find(|e| e.name == "small").unwrap();
        let inside = result.children(small.idx_path.clone()).unwrap();
        assert!(result.move_to_trash(inside[0].idx_path.clone()).is_err());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preselection_matches_sapu_app() {
        let rec = |verdict, action, source, category| Recommendation {
            idx_path: vec![],
            path: PathBuf::from("/x"),
            size: 1,
            judgement: Judgement { verdict, category, reason: String::new(), action, look_inside: false, source },
        };
        let ai = Source::Model { confidence: 0.9 };
        assert!(preselected(&rec(Verdict::Safe, Action::TrashItem, Source::Rule, Category::Cache)));
        assert!(preselected(&rec(Verdict::Likely, Action::TrashItem, Source::Rule, Category::Installer)));
        assert!(!preselected(&rec(Verdict::Review, Action::TrashItem, Source::Rule, Category::OldFile)));
        assert!(!preselected(&rec(Verdict::Safe, Action::TrashItem, ai, Category::AiGuess)));
        assert!(!preselected(&rec(Verdict::Safe, Action::Evict, Source::Rule, Category::ICloud)));
        assert!(!preselected(&rec(Verdict::Safe, Action::Manual, Source::Rule, Category::Cache)));
    }

    #[test]
    fn labels_and_locations() {
        assert_eq!(verdict_label(VerdictKind::Safe), "Safe to delete");
        assert!(!data_locations().is_empty());
        assert!(!version().is_empty());
    }
}
