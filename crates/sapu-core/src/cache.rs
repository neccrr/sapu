//! Scan cache: the last scan of each folder, saved so Sapu can show it
//! instantly on the next start while a fresh scan runs.
//!
//! Lives in `~/Library/Caches/sapu/scans/` and is always safe to delete.
//! Cached results are display-only: nothing can be cleaned until the fresh
//! scan has finished, and every item is re-checked before it's touched anyway.
//!
//! Format: a small custom binary (magic, version, stats, then the tree
//! depth-first), so no extra dependencies and ~0.3 s for 700k files.

use crate::paths;
use crate::tree::{Kind, Node, ScanStats, Tree};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info, warn};

const MAGIC: &[u8; 8] = b"SAPUSCN1";

fn key(root: &Path) -> String {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub fn file_for(root: &Path) -> PathBuf {
    paths::scans_dir().join(format!("{}.bin", key(root)))
}

// ── writing ──

fn w_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}
fn w_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    w_u64(w, s.len() as u64)?;
    w.write_all(s.as_bytes())
}
fn w_node(w: &mut impl Write, n: &Node) -> io::Result<()> {
    w_str(w, &n.name)?;
    let kind = match n.kind {
        Kind::Dir => 0u8,
        Kind::File => 1,
        Kind::Symlink => 2,
        Kind::Other => 3,
    };
    w.write_all(&[kind, n.incomplete as u8])?;
    for v in [n.size, n.files, n.modified, n.logical, n.children.len() as u64] {
        w_u64(w, v)?;
    }
    n.children.iter().try_for_each(|c| w_node(w, c))
}

/// Save a finished scan (atomically: write to a temp file, then rename).
pub fn save(tree: &Tree) -> io::Result<()> {
    if tree.stats.cancelled {
        return Ok(()); // partial scans aren't worth caching
    }
    let started = std::time::Instant::now();
    let path = file_for(&tree.root_path);
    fs::create_dir_all(paths::scans_dir())?;
    let tmp = path.with_extension("tmp");
    {
        let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w_str(&mut w, &tree.root_path.to_string_lossy())?;
        let s = &tree.stats;
        for v in [s.files, s.dirs, s.bytes, s.errors, s.hardlinks_skipped, s.elapsed.as_millis() as u64, s.started_at] {
            w_u64(&mut w, v)?;
        }
        w_node(&mut w, &tree.root)?;
        w.flush()?;
    }
    fs::rename(&tmp, &path)?;
    info!(file = %path.display(), ms = started.elapsed().as_millis() as u64, "scan cached");
    Ok(())
}

// ── reading ──

fn r_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn r_str(r: &mut impl Read) -> io::Result<String> {
    let n = r_u64(r)? as usize;
    if n > 1 << 20 {
        return Err(io::Error::other("corrupt cache (name too long)"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}
fn r_node(r: &mut impl Read, depth: usize) -> io::Result<Node> {
    if depth > 4096 {
        return Err(io::Error::other("corrupt cache (too deep)"));
    }
    let name = r_str(r)?;
    let mut flags = [0u8; 2];
    r.read_exact(&mut flags)?;
    let kind = match flags[0] {
        0 => Kind::Dir,
        1 => Kind::File,
        2 => Kind::Symlink,
        _ => Kind::Other,
    };
    let (size, files, modified, logical, count) = (r_u64(r)?, r_u64(r)?, r_u64(r)?, r_u64(r)?, r_u64(r)?);
    if count > 50_000_000 {
        return Err(io::Error::other("corrupt cache (too many children)"));
    }
    let mut children = Vec::with_capacity(count as usize);
    for _ in 0..count {
        children.push(r_node(r, depth + 1)?);
    }
    Ok(Node { name: name.into(), kind, size, files, incomplete: flags[1] != 0, modified, logical, children })
}

/// The last scan of `root`, if there is a readable one.
pub fn load(root: &Path) -> Option<Tree> {
    let path = file_for(root);
    let started = std::time::Instant::now();
    let result = (|| -> io::Result<Tree> {
        let mut r = BufReader::with_capacity(1 << 20, File::open(&path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::other("not a Sapu scan cache"));
        }
        let root_path = PathBuf::from(r_str(&mut r)?);
        let mut v = [0u64; 7];
        for x in &mut v {
            *x = r_u64(&mut r)?;
        }
        let stats = ScanStats {
            files: v[0],
            dirs: v[1],
            bytes: v[2],
            errors: v[3],
            hardlinks_skipped: v[4],
            elapsed: Duration::from_millis(v[5]),
            cancelled: false,
            started_at: v[6],
        };
        let root = r_node(&mut r, 0)?;
        Ok(Tree { root_path, root, stats })
    })();
    match result {
        Ok(t) => {
            info!(file = %path.display(), files = t.stats.files, ms = started.elapsed().as_millis() as u64, "loaded cached scan");
            Some(t)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!("no cached scan yet");
            None
        }
        Err(e) => {
            warn!(file = %path.display(), error = %e, "cached scan unreadable, ignoring it");
            let _ = fs::remove_file(&path);
            None
        }
    }
}

/// Delete all cached scans (Sapu's own cache, never your files).
pub fn clear() -> io::Result<()> {
    let dir = paths::scans_dir();
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    info!("scan cache cleared");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan, Progress, ScanOptions};

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("sapu-cache-{}", std::process::id()));
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::write(dir.join("a/b/f.bin"), vec![1u8; 20_000]).unwrap();
        fs::write(dir.join("g.txt"), "hello").unwrap();
        let tree = scan(&dir, &ScanOptions::default(), &Progress::default()).unwrap();

        let mut buf = Vec::new();
        w_node(&mut buf, &tree.root).unwrap();
        let back = r_node(&mut buf.as_slice(), 0).unwrap();
        assert_eq!(back.size, tree.root.size);
        assert_eq!(back.files, tree.root.files);
        assert_eq!(back.logical, tree.root.logical);
        assert_eq!(back.children.len(), tree.root.children.len());
        assert_eq!(&*back.children[0].name, &*tree.root.children[0].name);

        assert!(r_node(&mut &buf[..buf.len() / 2], 0).is_err(), "truncated cache is rejected");
        let _ = fs::remove_dir_all(&dir);
    }
}
