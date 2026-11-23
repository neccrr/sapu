//! iCloud Drive integration.
//!
//! The big idea: a file in iCloud doesn't need to be *downloaded* on this Mac.
//! "Remove download" (eviction) frees its space here while it stays safely in
//! iCloud. It re-downloads automatically when you open it. Nothing is deleted,
//! so Sapu never offers to *trash* anything inside iCloud: that would delete
//! it on every device.
//!
//! Detection: an iCloud file that isn't downloaded has a logical size but
//! (almost) no blocks on disk, which Sapu already measures for every file.
//! Actions use Apple's own tools: `brctl` (iCloud Drive), falling back to
//! `fileproviderctl` on newer macOS.

use crate::error::{Error, Result};
use crate::tree::{Node, Tree};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

/// `~/Library/Mobile Documents`: iCloud Drive and every app's iCloud folder.
pub fn mobile_documents(home: &Path) -> PathBuf {
    home.join("Library/Mobile Documents")
}

/// The "iCloud Drive" folder you see in Finder.
pub fn drive(home: &Path) -> PathBuf {
    mobile_documents(home).join("com~apple~CloudDocs")
}

/// Is iCloud Drive set up on this Mac at all?
pub fn available(home: &Path) -> bool {
    drive(home).is_dir()
}

/// Are Desktop & Documents synced with iCloud (System Settings → iCloud Drive)?
pub fn desktop_documents_synced(home: &Path) -> bool {
    let d = drive(home);
    d.join("Desktop").is_dir() && d.join("Documents").is_dir()
}

/// Every place whose contents live in iCloud.
pub fn roots(home: &Path) -> Vec<PathBuf> {
    let mut r = vec![mobile_documents(home)];
    if desktop_documents_synced(home) {
        r.push(home.join("Desktop"));
        r.push(home.join("Documents"));
    }
    r
}

pub fn in_icloud(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| path.starts_with(r))
}

/// Not downloaded: it has a size, but (almost) nothing on disk.
pub fn is_cloud_only(node: &Node) -> bool {
    node.logical > 64 * 1024 && node.size.saturating_mul(20) < node.logical
}

/// Mostly downloaded (so removing the download would free real space).
pub fn is_downloaded(node: &Node) -> bool {
    node.size > 0 && node.size.saturating_mul(2) >= node.logical
}

fn tool() -> String {
    std::env::var("SAPU_BRCTL").unwrap_or_else(|_| "brctl".into())
}

fn run(args: &[&str], path: &Path) -> std::result::Result<(), String> {
    let out = Command::new(args[0]).args(&args[1..]).arg(path).output().map_err(|e| format!("{}: {e}", args[0]))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn check_inside(path: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or(Error::NoHome)?;
    let path = path.canonicalize()?;
    if !in_icloud(&path, &roots(&home)) {
        return Err(Error::Manual(format!("{} isn't in iCloud, so there's no download to remove.", path.display())));
    }
    Ok(path)
}

/// Remove the local download; the item stays in iCloud.
pub fn evict(path: &Path) -> Result<()> {
    let path = check_inside(path)?;
    let brctl = tool();
    let result = run(&[&brctl, "evict"], &path)
        .or_else(|e1| run(&["fileproviderctl", "evict"], &path).map_err(|e2| format!("{e1}; {e2}")));
    match result {
        Ok(()) => {
            info!(path = %path.display(), "removed download (still in iCloud)");
            Ok(())
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "evict failed");
            Err(Error::Manual(format!(
                "macOS refused to remove the download ({e}). In Finder: right-click → Remove Download."
            )))
        }
    }
}

/// Download it from iCloud now.
pub fn download(path: &Path) -> Result<()> {
    let path = check_inside(path)?;
    let brctl = tool();
    let result = run(&[&brctl, "download"], &path)
        .or_else(|e1| run(&["fileproviderctl", "materialize"], &path).map_err(|e2| format!("{e1}; {e2}")));
    result.map_err(|e| {
        Error::Manual(format!("macOS couldn't start the download ({e}). In Finder: right-click → Download Now."))
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudItem {
    pub path: PathBuf,
    pub idx_path: Vec<usize>,
    pub on_mac: u64,
    pub in_cloud: u64,
    pub modified: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub available: bool,
    pub desktop_documents_synced: bool,
    /// Total size of everything in iCloud that this scan covered.
    pub in_cloud: u64,
    /// How much of that is downloaded on this Mac.
    pub on_mac: u64,
    /// Largest downloaded files (candidates for "Remove download").
    pub downloaded: Vec<CloudItem>,
    /// Largest items that are only in iCloud.
    pub cloud_only: Vec<CloudItem>,
    /// Photos library size, for the "Optimize Mac Storage" tip.
    pub photos_library: Option<(PathBuf, u64)>,
    /// The scan didn't include iCloud (e.g. a single folder was scanned).
    pub not_scanned: bool,
}

/// Collect iCloud numbers from a scan.
pub fn summarize(tree: &Tree, home: &Path) -> Summary {
    let mut s = Summary {
        available: available(home),
        desktop_documents_synced: desktop_documents_synced(home),
        ..Default::default()
    };
    let roots = roots(home);
    let covered = roots.iter().any(|r| r.starts_with(&tree.root_path) || tree.root_path.starts_with(r));
    if !covered {
        s.not_scanned = true;
        return s;
    }
    let mut path = tree.root_path.clone();
    walk(&tree.root, &mut path, &mut Vec::new(), &roots, false, &mut s);
    s.downloaded.sort_by_key(|i| std::cmp::Reverse(i.on_mac));
    s.downloaded.truncate(60);
    s.cloud_only.sort_by_key(|i| std::cmp::Reverse(i.in_cloud));
    s.cloud_only.truncate(40);
    info!(
        in_cloud = s.in_cloud,
        on_mac = s.on_mac,
        downloaded = s.downloaded.len(),
        cloud_only = s.cloud_only.len(),
        "iCloud summary"
    );
    s
}

fn walk(node: &Node, path: &mut PathBuf, idx: &mut Vec<usize>, roots: &[PathBuf], inside: bool, s: &mut Summary) {
    for (i, c) in node.children.iter().enumerate() {
        path.push(&*c.name);
        idx.push(i);
        let now_inside = inside || roots.iter().any(|r| r == path);
        if now_inside && !inside {
            s.in_cloud += c.logical;
            s.on_mac += c.size;
        }
        if c.name.ends_with(".photoslibrary") && c.size > s.photos_library.as_ref().map_or(0, |p| p.1) {
            s.photos_library = Some((path.clone(), c.size));
        }
        let item = || CloudItem {
            path: path.clone(),
            idx_path: idx.clone(),
            on_mac: c.size,
            in_cloud: c.logical,
            modified: c.modified,
            is_dir: c.is_dir(),
        };
        if now_inside && !c.is_dir() {
            if is_cloud_only(c) && c.logical >= 5_000_000 {
                s.cloud_only.push(item());
            } else if is_downloaded(c) && c.size >= 5_000_000 {
                s.downloaded.push(item());
            }
        }
        // Only descend where iCloud content can be.
        let may_contain = now_inside || roots.iter().any(|r| r.starts_with(&*path));
        if c.is_dir() && may_contain {
            walk(c, path, idx, roots, now_inside, s);
        }
        idx.pop();
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;

    fn file(size: u64, logical: u64) -> Node {
        Node {
            name: "f".into(),
            kind: Kind::File,
            size,
            files: 1,
            incomplete: false,
            modified: 0,
            logical,
            children: vec![],
        }
    }

    #[test]
    fn states() {
        assert!(is_cloud_only(&file(0, 50_000_000)));
        assert!(!is_cloud_only(&file(50_000_000, 50_000_000)));
        assert!(is_downloaded(&file(50_003_968, 50_000_000)));
        assert!(!is_downloaded(&file(0, 50_000_000)));
    }
}
