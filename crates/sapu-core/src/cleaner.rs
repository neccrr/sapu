//! Everything that removes files goes through here.
//!
//! Order of checks for every item:
//! 1. **Re-check** it still matches the scan ([`revalidate`]).
//! 2. **Safety check** ([`safety`]): system paths, home, keychains, your protected list.
//! 3. **Journal intent** (write-ahead), then **move to Trash**, then journal the outcome.

use crate::{
    error::{Error, Result},
    journal::{self, Journal},
    judge::{Action, Judgement},
    rules::CleanupRule,
    safety, scanner,
    tree::Node,
};
use rayon::prelude::*;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub rule_id: &'static str,
    pub rule_name: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Report only; touch nothing.
    DryRun,
    /// Move contents to the Trash (recoverable). Never hard-delete.
    Trash,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub trashed: u64,
    pub freed_bytes: u64,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or(Error::NoHome)
}

// ── Re-check before deleting ────────────────────────────────────────

/// What the scan saw. If the item no longer matches, it's skipped.
#[derive(Debug, Clone, Copy)]
pub struct Expect {
    pub bytes: u64,
    pub is_dir: bool,
    pub scanned_at: u64,
    /// Skip if modified after the scan. Off for caches, which change constantly by design.
    pub check_mtime: bool,
}

impl Expect {
    pub fn from_node(node: &Node, scanned_at: u64, check_mtime: bool) -> Self {
        Self { bytes: node.size, is_dir: node.is_dir(), scanned_at, check_mtime }
    }
}

/// Make sure the item is still what the scan saw.
pub fn revalidate(path: &Path, e: &Expect) -> Result<()> {
    let m = std::fs::symlink_metadata(path).map_err(|_| Error::Changed("it no longer exists".into()))?;
    if m.is_dir() != e.is_dir {
        return Err(Error::Changed("it's a different kind of item now".into()));
    }
    if e.check_mtime && e.scanned_at > 0 && (m.mtime().max(0) as u64) > e.scanned_at + 1 {
        return Err(Error::Changed("it was modified after the scan".into()));
    }
    if !m.is_dir() {
        let now = m.blocks() * 512;
        if e.bytes > 0 && now > e.bytes.saturating_mul(2) + 1_000_000 {
            return Err(Error::Changed("it has grown a lot since the scan".into()));
        }
    }
    Ok(())
}

// ── Rule-based `clean` command ──────────────────────────────────────

/// Measure every rule target that exists and passes the safety check.
pub fn plan(rules: &[CleanupRule]) -> Result<Vec<PlanItem>> {
    let home = home()?;
    let mut items: Vec<PlanItem> = rules
        .par_iter()
        .filter_map(|rule| {
            let target = rule.resolve(&home);
            if !target.is_dir() {
                debug!(rule = rule.id, path = %target.display(), "target missing, skipped");
                return None;
            }
            let target = match safety::check_cleanup(&target, &home) {
                Ok(t) => t,
                Err(e) => {
                    warn!(rule = rule.id, error = %e, "target failed safety check");
                    return None;
                }
            };
            let bytes = scanner::dir_size(&target);
            Some(PlanItem { rule_id: rule.id, rule_name: rule.name, path: target, bytes })
        })
        .collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.bytes));
    Ok(items)
}

/// Clear the *contents* of each target (the folder itself stays).
pub fn execute(items: &[PlanItem], mode: Mode) -> Result<Outcome> {
    let home = home()?;
    let mut out = Outcome::default();
    if mode == Mode::DryRun {
        out.freed_bytes = items.iter().map(|i| i.bytes).sum();
        return Ok(out);
    }
    let journal = Journal::open();
    let batch = journal.begin("Clean (built-in rules)");
    for item in items {
        let target = safety::check_cleanup(&item.path, &home)?;
        info!(rule = item.rule_id, path = %target.display(), "cleaning");
        for child in std::fs::read_dir(&target)? {
            let child = child?.path();
            let size = item_size(&child);
            let result = safety::check_cleanup(&child, &home)
                .and_then(|c| journal::trash_logged(&journal, batch, &c, size, item.rule_name));
            match result {
                Ok(()) => {
                    out.trashed += 1;
                    out.freed_bytes += size;
                }
                Err(e) => {
                    warn!(path = %child.display(), error = %e, "skipped");
                    out.failed.push((child, e.to_string()));
                }
            }
        }
    }
    Ok(out)
}

// ── Single items and recommendations ────────────────────────────────

/// Move one hand-picked item to the Trash (safety check + journal).
/// Items in iCloud are refused unless `icloud` is [`safety::ICloud::ConsentGiven`].
pub fn trash_item(
    path: &Path,
    journal: &Journal,
    batch: u64,
    bytes: u64,
    note: &str,
    icloud: safety::ICloud,
) -> Result<PathBuf> {
    let home = home()?;
    let path = safety::check_manual_icloud(path, &home, icloud)
        .inspect_err(|e| warn!(path = %path.display(), error = %e, "blocked by safety check"))?;
    journal::trash_logged(journal, batch, &path, bytes, note)?;
    info!(path = %path.display(), "moved to Trash");
    Ok(path)
}

/// Result of cleaning one recommendation.
#[derive(Debug, Default)]
pub struct Applied {
    pub trashed: u64,
    pub failed: Vec<(PathBuf, String)>,
}

/// Carry out a judged action on `path` (always via the Trash).
pub fn apply(
    path: &Path,
    judgement: &Judgement,
    expect: Option<&Expect>,
    journal: &Journal,
    batch: u64,
) -> Result<Applied> {
    if let Some(e) = expect {
        revalidate(path, e).inspect_err(|err| info!(path = %path.display(), reason = %err, "skipped by re-check"))?;
    }
    debug!(path = %path.display(), action = ?judgement.action, "applying");
    let mut out = Applied::default();
    match judgement.action {
        Action::TrashItem => {
            // Recommendations never carry iCloud consent.
            trash_item(
                path,
                journal,
                batch,
                expect.map(|e| e.bytes).unwrap_or(0),
                &judgement.reason,
                safety::ICloud::Refuse,
            )?;
            out.trashed = 1;
        }
        Action::TrashContents => {
            let home = home()?;
            let dir = safety::check_manual(path, &home)?;
            info!(path = %dir.display(), "clearing contents");
            for child in std::fs::read_dir(&dir)? {
                let child = child?.path();
                let size = item_size(&child);
                let result = safety::check_manual(&child, &home)
                    .and_then(|c| journal::trash_logged(journal, batch, &c, size, &judgement.reason));
                match result {
                    Ok(()) => out.trashed += 1,
                    Err(e) => {
                        warn!(path = %child.display(), error = %e, "skipped");
                        out.failed.push((child, e.to_string()));
                    }
                }
            }
        }
        Action::Evict => {
            // Not a deletion: the item stays in iCloud.
            crate::icloud::evict(path)?;
        }
        Action::EmptyTrash | Action::Manual => return Err(Error::Manual(judgement.reason.clone())),
    }
    Ok(out)
}

fn item_size(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => scanner::dir_size(p),
        Ok(m) => m.blocks() * 512,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn revalidate_catches_changes() {
        let dir = std::env::temp_dir().join(format!("sapu-reval-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.bin");
        fs::write(&f, vec![0u8; 10_000]).unwrap();
        let e = Expect { bytes: 12_288, is_dir: false, scanned_at: 1, check_mtime: true };
        assert!(matches!(revalidate(&f, &e), Err(Error::Changed(_))), "modified after scan_at=1");
        let e = Expect { scanned_at: u64::MAX / 2, ..e };
        assert!(revalidate(&f, &e).is_ok());
        assert!(revalidate(&dir.join("gone"), &e).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
