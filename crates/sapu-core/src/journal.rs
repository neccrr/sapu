//! Write-ahead journal of everything Sapu moves to the Trash, plus Undo.
//!
//! Before an item is touched, an `intent` line is written; afterwards a
//! `done` (with where it landed in the Trash) or `failed` line. Because the
//! intent comes first, even a crash mid-clean leaves a record of what was
//! being done. Undo moves items from the Trash back to where they were.
//!
//! File: `~/Library/Application Support/sapu/journal.jsonl` (one JSON per line).

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Event {
    Batch,
    Intent,
    Done,
    Failed,
    Restored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Line {
    batch: u64,
    time: u64,
    event: Event,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trashed_to: Option<PathBuf>,
    #[serde(default)]
    bytes: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ItemState {
    /// Intent recorded, no outcome (interrupted).
    Unknown,
    Trashed,
    Failed,
    Restored,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub path: PathBuf,
    pub trashed_to: Option<PathBuf>,
    pub bytes: u64,
    pub state: ItemState,
    pub note: String,
}

impl Item {
    /// Can Undo bring it back right now?
    pub fn restorable(&self) -> bool {
        self.state == ItemState::Trashed
            && self.trashed_to.as_ref().is_some_and(|t| t.symlink_metadata().is_ok())
            && self.path.symlink_metadata().is_err()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Batch {
    pub id: u64,
    pub time: u64,
    pub label: String,
    pub items: Vec<Item>,
}

impl Batch {
    pub fn bytes_trashed(&self) -> u64 {
        self.items.iter().filter(|i| i.state == ItemState::Trashed).map(|i| i.bytes).sum()
    }
    pub fn count(&self, s: ItemState) -> usize {
        self.items.iter().filter(|i| i.state == s).count()
    }
    pub fn restorable(&self) -> usize {
        self.items.iter().filter(|i| i.restorable()).count()
    }
}

#[derive(Debug, Default)]
pub struct UndoReport {
    pub restored: usize,
    pub failed: Vec<(PathBuf, String)>,
}

pub struct Journal {
    path: PathBuf,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Default for Journal {
    fn default() -> Self {
        Self::open()
    }
}

impl Journal {
    pub fn open() -> Self {
        Self { path: crate::paths::journal_file() }
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, line: &Line) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
            let mut s = serde_json::to_string(line).map_err(std::io::Error::other)?;
            s.push('\n');
            f.write_all(s.as_bytes())?;
            f.sync_data()
        })();
        if let Err(e) = result {
            warn!(error = %e, "could not write journal");
        }
    }

    /// Start a new batch (one user action). Returns its id.
    pub fn begin(&self, label: &str) -> u64 {
        let id = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        self.write(&Line {
            batch: id,
            time: now_secs(),
            event: Event::Batch,
            path: None,
            trashed_to: None,
            bytes: 0,
            note: label.into(),
        });
        info!(batch = id, label, "journal: new batch");
        id
    }

    pub fn intent(&self, batch: u64, path: &Path, bytes: u64, note: &str) {
        self.write(&Line {
            batch,
            time: now_secs(),
            event: Event::Intent,
            path: Some(path.into()),
            trashed_to: None,
            bytes,
            note: note.into(),
        });
    }

    pub fn done(&self, batch: u64, path: &Path, trashed_to: Option<PathBuf>, bytes: u64) {
        self.write(&Line {
            batch,
            time: now_secs(),
            event: Event::Done,
            path: Some(path.into()),
            trashed_to,
            bytes,
            note: String::new(),
        });
    }

    pub fn failed(&self, batch: u64, path: &Path, why: &str) {
        self.write(&Line {
            batch,
            time: now_secs(),
            event: Event::Failed,
            path: Some(path.into()),
            trashed_to: None,
            bytes: 0,
            note: why.into(),
        });
    }

    fn restored(&self, batch: u64, path: &Path) {
        self.write(&Line {
            batch,
            time: now_secs(),
            event: Event::Restored,
            path: Some(path.into()),
            trashed_to: None,
            bytes: 0,
            note: String::new(),
        });
    }

    /// All batches, newest first.
    pub fn history(&self) -> Vec<Batch> {
        let Ok(text) = fs::read_to_string(&self.path) else { return Vec::new() };
        let mut batches: BTreeMap<u64, Batch> = BTreeMap::new();
        for line in text.lines().filter_map(|l| serde_json::from_str::<Line>(l).ok()) {
            let b = batches.entry(line.batch).or_insert_with(|| Batch {
                id: line.batch,
                time: line.time,
                label: String::new(),
                items: Vec::new(),
            });
            match line.event {
                Event::Batch => b.label = line.note,
                Event::Intent => {
                    if let Some(p) = line.path {
                        b.items.push(Item {
                            path: p,
                            trashed_to: None,
                            bytes: line.bytes,
                            state: ItemState::Unknown,
                            note: line.note,
                        });
                    }
                }
                other => {
                    let Some(p) = line.path else { continue };
                    if let Some(item) = b.items.iter_mut().rev().find(|i| i.path == p) {
                        match other {
                            Event::Done => {
                                item.state = ItemState::Trashed;
                                item.trashed_to = line.trashed_to;
                                if line.bytes > 0 {
                                    item.bytes = line.bytes;
                                }
                            }
                            Event::Failed => {
                                item.state = ItemState::Failed;
                                item.note = line.note;
                            }
                            Event::Restored => item.state = ItemState::Restored,
                            _ => {}
                        }
                    }
                }
            }
        }
        let mut out: Vec<Batch> = batches.into_values().filter(|b| !b.items.is_empty()).collect();
        out.reverse();
        out
    }

    /// Put everything from a batch back where it was.
    pub fn undo(&self, batch_id: u64) -> UndoReport {
        let mut report = UndoReport::default();
        let Some(batch) = self.history().into_iter().find(|b| b.id == batch_id) else { return report };
        for item in batch.items.iter().filter(|i| i.state == ItemState::Trashed) {
            let Some(from) = &item.trashed_to else {
                report.failed.push((item.path.clone(), "location in the Trash unknown; use Finder → Put Back".into()));
                continue;
            };
            if item.path.symlink_metadata().is_ok() {
                report.failed.push((item.path.clone(), "something new already exists at that path".into()));
                continue;
            }
            let result = (|| -> std::io::Result<()> {
                if let Some(parent) = item.path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(from, &item.path)
            })();
            match result {
                Ok(()) => {
                    remove_trash_info(from);
                    self.restored(batch_id, &item.path);
                    info!(path = %item.path.display(), "restored from Trash");
                    report.restored += 1;
                }
                Err(e) => report.failed.push((item.path.clone(), e.to_string())),
            }
        }
        report
    }
}

/// Folders where the system Trash keeps files for this user.
fn trash_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    vec![home.join(".Trash"), home.join(".local/share/Trash/files")]
}

/// Find where an item just landed in the Trash. Finder renames clashes
/// ("photos.zip" → "photos 2.zip"), so match on the name stem and pick
/// the most recently changed candidate.
pub fn locate_in_trash(original: &Path, since: SystemTime) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let name = original.file_name()?.to_string_lossy().to_string();
    let p = Path::new(&name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let since = since.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) - 2;

    trash_dirs()
        .into_iter()
        .filter_map(|d| fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n == name || (n.starts_with(&format!("{stem} ")) && n.ends_with(&ext))
        })
        .filter_map(|e| e.metadata().ok().map(|m| (e.path(), m.ctime())))
        .filter(|(_, ctime)| *ctime >= since)
        .max_by_key(|(_, ctime)| *ctime)
        .map(|(p, _)| p)
}

/// Linux keeps a .trashinfo file per item; tidy it after a restore.
fn remove_trash_info(trashed: &Path) {
    let Some(name) = trashed.file_name() else { return };
    if let Some(files_dir) = trashed.parent() {
        if files_dir.ends_with("Trash/files") {
            if let Some(trash) = files_dir.parent() {
                let info = trash.join("info").join(format!("{}.trashinfo", name.to_string_lossy()));
                let _ = fs::remove_file(info);
            }
        }
    }
}

/// Write-ahead + trash + record, for one item that already passed the safety check.
pub fn trash_logged(journal: &Journal, batch: u64, path: &Path, bytes: u64, note: &str) -> Result<()> {
    journal.intent(batch, path, bytes, note);
    let before = SystemTime::now();
    match trash::delete(path) {
        Ok(()) => {
            let landed = locate_in_trash(path, before);
            if landed.is_none() {
                warn!(path = %path.display(), "moved to Trash, but couldn't find where (Undo will need Finder's Put Back)");
            }
            tracing::debug!(path = %path.display(), bytes, trashed_to = ?landed, "trashed");
            journal.done(batch, path, landed, bytes);
            Ok(())
        }
        Err(e) => {
            journal.failed(batch, path, &e.to_string());
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_folds_events() {
        let dir = std::env::temp_dir().join(format!("sapu-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let j = Journal::at(dir.join("journal.jsonl"));
        let b = j.begin("test clean");
        j.intent(b, Path::new("/x/a"), 10, "cache");
        j.done(b, Path::new("/x/a"), Some(PathBuf::from("/t/a")), 10);
        j.intent(b, Path::new("/x/b"), 5, "logs");
        j.failed(b, Path::new("/x/b"), "denied");
        j.intent(b, Path::new("/x/c"), 7, "interrupted");

        let h = j.history();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].label, "test clean");
        assert_eq!(h[0].count(ItemState::Trashed), 1);
        assert_eq!(h[0].count(ItemState::Failed), 1);
        assert_eq!(h[0].count(ItemState::Unknown), 1);
        assert_eq!(h[0].bytes_trashed(), 10);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_moves_back() {
        let dir = std::env::temp_dir().join(format!("sapu-undo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("trash")).unwrap();
        fs::create_dir_all(dir.join("home/proj")).unwrap();
        fs::write(dir.join("trash/node_modules"), "x").unwrap();
        let j = Journal::at(dir.join("journal.jsonl"));
        let b = j.begin("t");
        let orig = dir.join("home/proj/node_modules");
        j.intent(b, &orig, 1, "");
        j.done(b, &orig, Some(dir.join("trash/node_modules")), 1);

        assert_eq!(j.history()[0].restorable(), 1);
        let r = j.undo(b);
        assert_eq!(r.restored, 1, "{:?}", r.failed);
        assert!(orig.exists());
        assert_eq!(j.history()[0].count(ItemState::Restored), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
