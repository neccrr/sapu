//! Parallel tree scanner.
//!
//! Each folder is read once with `read_dir` + `lstat`; subfolders are then
//! walked in parallel on rayon's work-stealing pool, so wide trees (like
//! `~/Library`) use every core. Sizes are summed bottom-up as the recursion
//! unwinds, so the finished tree is fully aggregated and sorted with no
//! second pass.

use crate::{
    error::Result,
    progress::Progress,
    tree::{Kind, Node, ScanStats, Tree},
};
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs::{self, FileType, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;
use tracing::{debug, trace};

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub include_hidden: bool,
    /// Stay on the root's volume (skip external drives, network mounts).
    pub one_filesystem: bool,
    /// Absolute paths never entered.
    pub skip: Vec<PathBuf>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { include_hidden: true, one_filesystem: true, skip: Vec::new() }
    }
}

struct Ctx<'a> {
    opts: &'a ScanOptions,
    devices: Vec<u64>,
    skip: Vec<PathBuf>,
    /// (device, inode) of multiply-linked files already counted.
    seen_links: Mutex<HashSet<(u64, u64)>>,
    links_skipped: AtomicU64,
    progress: &'a Progress,
}

pub fn scan(root: &Path, opts: &ScanOptions, progress: &Progress) -> Result<Tree> {
    let started = Instant::now();
    let started_at =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let root = root.canonicalize()?;
    let meta = fs::symlink_metadata(&root)?;

    let mut devices = vec![meta.dev()];
    let mut skip = opts.skip.clone();
    if root == Path::new("/") {
        // macOS: user data lives on a separate APFS volume reached through
        // firmlinks (/Users, /Applications…). Allow that volume, and skip
        // the raw mount points so nothing is counted twice.
        if let Ok(m) = fs::metadata("/System/Volumes/Data") {
            devices.push(m.dev());
        }
        skip.extend(["/System/Volumes", "/Volumes", "/dev", "/private/var/vm"].map(PathBuf::from));
    }

    debug!(root = %root.display(), hidden = opts.include_hidden, "scan started");

    let ctx =
        Ctx { opts, devices, skip, seen_links: Mutex::new(HashSet::new()), links_skipped: AtomicU64::new(0), progress };

    let name: Box<str> = root.file_name().map(|n| n.to_string_lossy().into()).unwrap_or_else(|| "/".into());

    let node = if meta.is_dir() {
        walk_dir(&root, name, alloc_size(&meta), mtime(&meta), &ctx)
    } else {
        Node {
            name,
            kind: kind_of(&meta.file_type()),
            size: alloc_size(&meta),
            files: 1,
            incomplete: false,
            modified: mtime(&meta),
            logical: meta.len(),
            children: Vec::new(),
        }
    };

    let snap = progress.snapshot();
    let stats = ScanStats {
        files: snap.files,
        dirs: snap.dirs,
        bytes: node.size,
        errors: snap.errors,
        hardlinks_skipped: ctx.links_skipped.load(Relaxed),
        elapsed: started.elapsed(),
        cancelled: progress.is_cancelled(),
        started_at,
    };

    debug!(
        files = stats.files,
        dirs = stats.dirs,
        bytes = stats.bytes,
        errors = stats.errors,
        hardlinks_skipped = stats.hardlinks_skipped,
        elapsed_ms = stats.elapsed.as_millis() as u64,
        files_per_sec = stats.files_per_sec() as u64,
        cancelled = stats.cancelled,
        "scan finished"
    );

    Ok(Tree { root_path: root, root: node, stats })
}

/// Convenience: total on-disk size of one folder.
pub fn dir_size(path: &Path) -> u64 {
    scan(path, &ScanOptions::default(), &Progress::default()).map(|t| t.root.size).unwrap_or(0)
}

fn walk_dir(path: &Path, name: Box<str>, own_size: u64, own_mtime: u64, ctx: &Ctx) -> Node {
    let p = ctx.progress;
    p.dirs.fetch_add(1, Relaxed);
    p.bytes.fetch_add(own_size, Relaxed);
    if let Ok(mut cur) = p.current.try_lock() {
        cur.clear();
        cur.push(path);
    }

    let mut node = Node {
        name,
        kind: Kind::Dir,
        size: own_size,
        files: 0,
        incomplete: false,
        modified: own_mtime,
        logical: 0,
        children: Vec::new(),
    };

    if p.is_cancelled() {
        node.incomplete = true;
        return node;
    }

    let entries = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            p.errors.fetch_add(1, Relaxed);
            debug!(path = %path.display(), error = %e, "cannot read folder");
            node.incomplete = true;
            return node;
        }
    };

    let mut subdirs: Vec<(PathBuf, Box<str>, u64, u64)> = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                p.errors.fetch_add(1, Relaxed);
                debug!(path = %path.display(), error = %e, "bad folder entry");
                node.incomplete = true;
                continue;
            }
        };
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if !ctx.opts.include_hidden && file_name.starts_with('.') {
            continue;
        }

        // On Unix, DirEntry::metadata is an lstat: symlinks are not followed.
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                p.errors.fetch_add(1, Relaxed);
                debug!(path = %entry.path().display(), error = %e, "cannot stat");
                node.incomplete = true;
                continue;
            }
        };
        let ft = meta.file_type();

        if ft.is_dir() {
            let child = entry.path();
            if ctx.opts.one_filesystem && !ctx.devices.contains(&meta.dev()) {
                trace!(path = %child.display(), "skipping other volume");
                continue;
            }
            if ctx.skip.iter().any(|s| s == &child) {
                trace!(path = %child.display(), "skipping excluded path");
                continue;
            }
            subdirs.push((child, file_name.into(), alloc_size(&meta), mtime(&meta)));
            continue;
        }

        if meta.nlink() > 1 && ft.is_file() {
            let first_time = ctx.seen_links.lock().map(|mut s| s.insert((meta.dev(), meta.ino()))).unwrap_or(true);
            if !first_time {
                ctx.links_skipped.fetch_add(1, Relaxed);
                continue;
            }
        }

        let size = alloc_size(&meta);
        p.files.fetch_add(1, Relaxed);
        p.bytes.fetch_add(size, Relaxed);
        node.children.push(Node {
            name: file_name.into(),
            kind: kind_of(&ft),
            size,
            files: 1,
            incomplete: false,
            modified: mtime(&meta),
            logical: if ft.is_file() { meta.len() } else { size },
            children: Vec::new(),
        });
    }

    let dirs: Vec<Node> =
        subdirs.into_par_iter().map(|(path, name, size, mt)| walk_dir(&path, name, size, mt, ctx)).collect();
    node.children.extend(dirs);

    for c in &node.children {
        node.size += c.size;
        node.files += c.files;
        node.incomplete |= c.incomplete;
        node.modified = node.modified.max(c.modified);
        node.logical += c.logical;
    }
    node.sort_children();
    node
}

/// Bytes actually allocated on disk (what you get back by deleting),
/// not the logical length. Sparse files and APFS compression are
/// reflected correctly this way.
fn alloc_size(meta: &Metadata) -> u64 {
    meta.blocks() * 512
}

fn mtime(meta: &Metadata) -> u64 {
    meta.mtime().max(0) as u64
}

fn kind_of(ft: &FileType) -> Kind {
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_temp_tree() {
        let dir = std::env::temp_dir().join(format!("sapu-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        fs::write(dir.join("a.bin"), vec![1u8; 10_000]).unwrap();
        fs::write(dir.join("sub/deeper/b.bin"), vec![1u8; 50_000]).unwrap();
        fs::hard_link(dir.join("a.bin"), dir.join("sub/a-link.bin")).unwrap();

        let tree = scan(&dir, &ScanOptions::default(), &Progress::default()).unwrap();
        assert_eq!(tree.root.files, 2, "hard link counted once");
        assert_eq!(tree.stats.hardlinks_skipped, 1);
        assert_eq!(&*tree.root.children[0].name, "sub", "largest first");
        assert!(tree.root.size >= 60_000);

        let _ = fs::remove_dir_all(&dir);
    }
}
