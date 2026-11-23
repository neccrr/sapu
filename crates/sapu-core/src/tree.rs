use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// One file or folder. Folders hold their whole subtree, so drilling
/// down in a UI never needs another disk scan.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: Box<str>,
    pub kind: Kind,
    /// Allocated bytes on disk (for folders: the whole subtree).
    pub size: u64,
    /// Files in the subtree (1 for a file).
    pub files: u64,
    /// Something inside couldn't be read (usually missing Full Disk Access).
    pub incomplete: bool,
    /// Last modification, seconds since 1970 (for folders: the newest thing inside).
    pub modified: u64,
    /// Logical size in bytes (what the file "is"). For iCloud files that
    /// aren't downloaded this is much larger than `size` (what's on disk).
    pub logical: u64,
    /// Always sorted largest-first.
    pub children: Vec<Node>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub(crate) fn sort_children(&mut self) {
        self.children.sort_unstable_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    }

    /// Follow child indices from this node.
    pub fn get(&self, path: &[usize]) -> Option<&Node> {
        path.iter().try_fold(self, |n, &i| n.children.get(i))
    }

    pub fn get_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        path.iter().try_fold(self, |n, &i| n.children.get_mut(i))
    }

    /// What this node looks like after its iCloud download is removed:
    /// same files, nothing on disk.
    pub fn evicted(&self) -> Node {
        let mut n = self.clone();
        fn clear(n: &mut Node) {
            if n.is_dir() {
                n.size = 0;
                for c in &mut n.children {
                    clear(c);
                    n.size += c.size;
                }
            } else {
                n.size = 0;
            }
        }
        clear(&mut n);
        n
    }

    /// Swap a descendant for a freshly scanned version and fix ancestor totals.
    /// Sibling order is left alone, so index paths held elsewhere stay valid.
    pub fn replace(&mut self, path: &[usize], new: Node) -> Option<Node> {
        let target = self.get_mut(path)?;
        let old = std::mem::replace(target, new);
        let (new_size, new_files, new_logical) = {
            let n = self.get(path)?;
            (n.size, n.files, n.logical)
        };
        let adjust = |n: &mut Node| {
            n.size = (n.size + new_size).saturating_sub(old.size);
            n.files = (n.files + new_files).saturating_sub(old.files);
            n.logical = (n.logical + new_logical).saturating_sub(old.logical);
        };
        if let Some((_, parents)) = path.split_last() {
            let mut node = &mut *self;
            adjust(node);
            for &i in parents {
                node = &mut node.children[i];
                adjust(node);
            }
        }
        Some(old)
    }

    /// Remove a descendant and fix the totals of every ancestor.
    /// Sibling order stays largest-first, so no re-sort is needed.
    pub fn remove(&mut self, path: &[usize]) -> Option<Node> {
        let (&last, parent_path) = path.split_last()?;
        let parent = self.get_mut(parent_path)?;
        if last >= parent.children.len() {
            return None;
        }
        let removed = parent.children.remove(last);

        let mut node = self;
        node.size = node.size.saturating_sub(removed.size);
        node.files = node.files.saturating_sub(removed.files);
        node.logical = node.logical.saturating_sub(removed.logical);
        for &i in parent_path {
            node = &mut node.children[i];
            node.size = node.size.saturating_sub(removed.size);
            node.files = node.files.saturating_sub(removed.files);
            node.logical = node.logical.saturating_sub(removed.logical);
        }
        Some(removed)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Entries that couldn't be read.
    pub errors: u64,
    /// Extra hard links that were not double-counted.
    pub hardlinks_skipped: u64,
    pub elapsed: Duration,
    pub cancelled: bool,
    /// When the scan started (seconds since 1970). Used to spot changes before deleting.
    pub started_at: u64,
}

impl ScanStats {
    pub fn files_per_sec(&self) -> f64 {
        let s = self.elapsed.as_secs_f64();
        if s > 0.0 {
            self.files as f64 / s
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Tree {
    pub root_path: PathBuf,
    pub root: Node,
    pub stats: ScanStats,
}

impl Tree {
    /// Turn a child-index path into a real filesystem path.
    pub fn path_of(&self, idx_path: &[usize]) -> PathBuf {
        let mut out = self.root_path.clone();
        let mut node = &self.root;
        for &i in idx_path {
            node = &node.children[i];
            out.push(&*node.name);
        }
        out
    }

    /// The `n` largest files anywhere in the tree.
    ///
    /// Uses a size-`n` min-heap plus pruning: children are sorted
    /// largest-first, so once a child is no bigger than the smallest
    /// file we're keeping, it and every later sibling can be skipped.
    pub fn largest_files(&self, n: usize) -> Vec<(PathBuf, u64)> {
        if n == 0 {
            return Vec::new();
        }
        let mut heap: BinaryHeap<Reverse<(u64, Vec<usize>)>> = BinaryHeap::with_capacity(n + 1);
        collect_largest(&self.root, &mut Vec::new(), n, &mut heap);

        let mut out: Vec<_> = heap.into_iter().map(|Reverse((size, p))| (self.path_of(&p), size)).collect();
        out.sort_unstable_by_key(|f| Reverse(f.1));
        out
    }
}

fn collect_largest(node: &Node, path: &mut Vec<usize>, n: usize, heap: &mut BinaryHeap<Reverse<(u64, Vec<usize>)>>) {
    for (i, child) in node.children.iter().enumerate() {
        if heap.len() == n {
            if let Some(Reverse((min, _))) = heap.peek() {
                if child.size <= *min {
                    break; // pruned: every later sibling is smaller still
                }
            }
        }
        path.push(i);
        if child.is_dir() {
            collect_largest(child, path, n, heap);
        } else {
            heap.push(Reverse((child.size, path.clone())));
            if heap.len() > n {
                heap.pop();
            }
        }
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64) -> Node {
        Node {
            name: name.into(),
            kind: Kind::File,
            size,
            files: 1,
            incomplete: false,
            modified: 0,
            logical: size,
            children: vec![],
        }
    }
    fn dir(name: &str, mut children: Vec<Node>) -> Node {
        let mut d = Node {
            name: name.into(),
            kind: Kind::Dir,
            size: 0,
            files: 0,
            incomplete: false,
            modified: 0,
            logical: 0,
            children: vec![],
        };
        d.size = children.iter().map(|c| c.size).sum();
        d.files = children.iter().map(|c| c.files).sum();
        d.children.append(&mut children);
        d.sort_children();
        d
    }
    fn sample() -> Tree {
        let root = dir(
            "r",
            vec![
                dir("a", vec![file("a1", 50), file("a2", 5)]),
                dir("b", vec![dir("bb", vec![file("b1", 80)])]),
                file("c", 10),
            ],
        );
        Tree { root_path: "/r".into(), root, stats: ScanStats::default() }
    }

    #[test]
    fn largest_files_finds_top_n() {
        let t = sample();
        let top = t.largest_files(2);
        assert_eq!(top, vec![(PathBuf::from("/r/b/bb/b1"), 80), (PathBuf::from("/r/a/a1"), 50)]);
    }

    #[test]
    fn remove_updates_ancestors() {
        let mut t = sample();
        // root children sorted: b(80), a(55), c(10); b -> bb -> b1
        let removed = t.root.remove(&[0, 0, 0]).unwrap();
        assert_eq!(&*removed.name, "b1");
        assert_eq!(t.root.size, 65);
        assert_eq!(t.root.children[0].size, 0);
        assert_eq!(t.root.files, 3);
    }

    #[test]
    fn replace_adjusts_ancestors() {
        let mut t = sample();
        // b(80) -> bb(80); replace bb with an empty folder
        t.root.replace(&[0, 0], dir("bb", vec![])).unwrap();
        assert_eq!(t.root.children[0].size, 0);
        assert_eq!(t.root.size, 65);
        assert_eq!(t.root.files, 3);
    }
}
