//! Swift bindings for `sapu-core`, generated with UniFFI.
//!
//! This is what the SwiftUI shell in `macos/SapuShell` talks to. It is
//! **read-only on purpose**: it scans, judges and recommends, but exposes
//! nothing that moves, deletes or evicts files. Cleaning stays in Sapu.app,
//! where every removal goes through re-check → safety → journal → Trash.

use sapu_core::{judge::Source, ml, paths, Judge, Judgement, Node, Progress, ScanOptions, Tree, Verdict};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

uniffi::setup_scaffolding!();

/// Sapu's version, e.g. "0.10.0".
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The user's home folder, the usual place to start a scan.
#[uniffi::export]
pub fn home_folder() -> String {
    sapu_core::cleaner::home().map(|p| p.display().to_string()).unwrap_or_else(|_| "/".into())
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SapuError {
    #[error("{message}")]
    Scan { message: String },
    #[error("{message}")]
    NotFound { message: String },
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

/// Plain words for a verdict, the same ones Sapu.app uses.
#[uniffi::export]
pub fn verdict_label(verdict: VerdictKind) -> String {
    let v = match verdict {
        VerdictKind::Safe => Verdict::Safe,
        VerdictKind::Likely => Verdict::Likely,
        VerdictKind::Review => Verdict::Review,
        VerdictKind::Keep => Verdict::Keep,
    };
    v.label().to_string()
}

/// A verdict on one item, and why.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Opinion {
    pub verdict: VerdictKind,
    pub category: String,
    pub reason: String,
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
            from_ai: j.is_ai(),
            confidence,
        }
    }
}

/// One file or folder in the scanned tree.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Entry {
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
    pub opinion: Option<Opinion>,
}

/// A recommendation: something worth cleaning.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Suggestion {
    pub path: String,
    pub idx_path: Vec<u32>,
    pub size: u64,
    pub opinion: Opinion,
    /// Sapu.app pre-ticks these (Safe, by a rule). AI and Review items never are.
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

/// Where Sapu keeps its data on this Mac.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DataLocation {
    pub name: String,
    pub path: String,
    pub note: String,
}

#[uniffi::export]
pub fn data_locations() -> Vec<DataLocation> {
    let loc = |name: &str, path: PathBuf, note: &str| DataLocation {
        name: name.into(),
        path: path.display().to_string(),
        note: note.into(),
    };
    vec![
        loc("Your data", paths::data_dir(), "Sapu AI, your protected list and the Undo history. Keep."),
        loc("Cache", paths::cache_dir(), "Saved scans for an instant start. Safe to delete."),
        loc("Logs", paths::logs_dir(), "What Sapu did. Rotated at 10 MB."),
    ]
}

/// Runs one scan at a time. Ask for progress or cancel from any thread
/// while `scan` runs on a background thread.
#[derive(uniffi::Object)]
pub struct Scanner {
    progress: Mutex<Arc<Progress>>,
}

#[uniffi::export]
impl Scanner {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Scanner { progress: Mutex::new(Arc::new(Progress::default())) })
    }

    /// Scan `path` (blocking). With `use_ai`, Sapu AI is retrained on the
    /// result, exactly like a scan in Sapu.app.
    pub fn scan(&self, path: String, use_ai: bool) -> Result<Arc<ScanResult>, SapuError> {
        let progress = Arc::new(Progress::default());
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = progress.clone();
        let tree = sapu_core::scan(Path::new(&path), &ScanOptions::default(), &progress)
            .map_err(|e| SapuError::Scan { message: e.to_string() })?;
        Ok(Arc::new(ScanResult::new(tree, use_ai)))
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
        self.progress.lock().unwrap_or_else(|e| e.into_inner()).cancel();
    }
}

/// A finished scan, judged.
#[derive(uniffi::Object)]
pub struct ScanResult {
    tree: Tree,
    judge: Judge,
    model: Option<ml::Model>,
}

impl ScanResult {
    fn new(tree: Tree, use_ai: bool) -> Self {
        let judge = Judge::new();
        let model = use_ai.then(|| ml::train(&tree, &judge));
        ScanResult { tree, judge, model }
    }

    fn entry(&self, node: &Node, idx: Vec<usize>) -> Entry {
        let path = self.tree.path_of(&idx);
        let opinion = ml::judge_with(&self.judge, self.model.as_ref(), &path, node).as_ref().map(Opinion::from);
        Entry {
            name: node.name.to_string(),
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
    pub fn root(&self) -> Entry {
        self.entry(&self.tree.root, Vec::new())
    }

    /// What's inside a folder, largest first. `idx_path` is empty for the root.
    pub fn children(&self, idx_path: Vec<u32>) -> Result<Vec<Entry>, SapuError> {
        let idx: Vec<usize> = idx_path.iter().map(|&i| i as usize).collect();
        let node = self.tree.root.get(&idx).ok_or_else(|| SapuError::NotFound {
            message: "That item isn't in this scan any more. Rescan to refresh.".into(),
        })?;
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
        sapu_core::advise_with(&self.tree, &self.judge, self.model.as_ref())
            .items
            .iter()
            .map(|r| Suggestion {
                path: r.path.display().to_string(),
                idx_path: to_u32(&r.idx_path),
                size: r.size,
                opinion: Opinion::from(&r.judgement),
                preselected: r.is_automatic() && r.judgement.verdict == Verdict::Safe && !r.judgement.is_ai(),
            })
            .collect()
    }

    /// The largest files anywhere in the scan.
    pub fn largest_files(&self, count: u32) -> Vec<Entry> {
        self.tree
            .largest_files(count as usize)
            .into_iter()
            .map(|(path, size)| Entry {
                name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
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
}

fn to_u32(idx: &[usize]) -> Vec<u32> {
    idx.iter().map(|&i| i as u32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sapu-ffi-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("big")).unwrap();
        fs::create_dir_all(dir.join("small")).unwrap();
        fs::write(dir.join("big/a.bin"), vec![1u8; 300_000]).unwrap();
        fs::write(dir.join("small/b.txt"), b"hello").unwrap();
        dir
    }

    #[test]
    fn scans_and_browses_without_ai() {
        let dir = fixture();
        let scanner = Scanner::new();
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
    fn labels_and_locations() {
        assert_eq!(verdict_label(VerdictKind::Safe), "Safe to delete");
        assert_eq!(data_locations().len(), 3);
        assert!(!version().is_empty());
    }
}
