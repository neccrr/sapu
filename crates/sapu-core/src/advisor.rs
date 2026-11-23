//! Cleanup recommendations: walk a scanned tree, judge every item, and
//! collect what's worth removing.
//!
//! The walk stops at the first judged item on each branch: once
//! `node_modules` is recommended, nothing inside it is listed separately,
//! and nothing inside a **Keep** item is ever suggested. Folders under
//! 1 MB are never entered.

use crate::judge::{Judge, Judgement, Verdict};
use crate::ml::{self, Model};
use crate::tree::{Node, Tree};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    /// Child indices from the tree root (valid until the tree changes).
    pub idx_path: Vec<usize>,
    pub path: PathBuf,
    pub size: u64,
    pub judgement: Judgement,
}

impl Recommendation {
    pub fn is_automatic(&self) -> bool {
        self.judgement.action.is_automatic()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Advice {
    /// Safe first, then Likely, then Review; largest first within each.
    pub items: Vec<Recommendation>,
}

impl Advice {
    /// Total size per verdict (Safe, Likely, Review).
    pub fn total(&self, verdict: Verdict) -> u64 {
        self.items.iter().filter(|r| r.judgement.verdict == verdict).map(|r| r.size).sum()
    }
}

const MB: u64 = 1_000_000;
const MAX_ITEMS: usize = 300;
/// At most this many AI guesses (largest first) in one list.
const MAX_AI_ITEMS: usize = 40;

fn min_size(v: Verdict) -> u64 {
    match v {
        Verdict::Safe => MB,
        Verdict::Likely => 5 * MB,
        Verdict::Review => 50 * MB,
        Verdict::Keep => u64::MAX,
    }
}

/// Rules only.
pub fn advise(tree: &Tree, judge: &Judge) -> Advice {
    advise_with(tree, judge, None)
}

/// Rules first, then Sapu's model for anything the rules don't recognise.
pub fn advise_with(tree: &Tree, judge: &Judge, model: Option<&Model>) -> Advice {
    let mut items = Vec::new();
    let mut path = tree.root_path.clone();
    let ctx = Walk { judge, model, protected: crate::safety::user_protected() };
    walk(&tree.root, &mut path, &mut Vec::new(), &ctx, &mut items);

    items.sort_by(|a: &Recommendation, b| a.judgement.verdict.cmp(&b.judgement.verdict).then(b.size.cmp(&a.size)));
    // AI guesses are suggestions, not a to-do list: keep only the biggest few.
    let mut ai_seen = 0;
    items.retain(|r| {
        if r.judgement.is_ai() {
            ai_seen += 1;
            ai_seen <= MAX_AI_ITEMS
        } else {
            true
        }
    });
    items.truncate(MAX_ITEMS);
    let advice = Advice { items };
    tracing::info!(
        items = advice.items.len(),
        safe_bytes = advice.total(Verdict::Safe),
        likely_bytes = advice.total(Verdict::Likely),
        review_bytes = advice.total(Verdict::Review),
        ai_guesses = advice.items.iter().filter(|r| r.judgement.is_ai()).count(),
        "recommendations ready"
    );
    advice
}

struct Walk<'a> {
    judge: &'a Judge,
    model: Option<&'a Model>,
    protected: Vec<PathBuf>,
}

/// Something irreplaceable buried inside a folder we're about to suggest.
fn risky_contents(node: &Node, depth: usize) -> Option<&'static str> {
    if depth > 6 {
        return None;
    }
    for c in &node.children {
        if c.is_dir() {
            if &*c.name == ".git" {
                return Some("a git repository (your code history)");
            }
            if c.name.ends_with(".photoslibrary") {
                return Some("a Photos library");
            }
            if let Some(r) = risky_contents(c, depth + 1) {
                return Some(r);
            }
        }
    }
    None
}

fn walk(node: &Node, path: &mut PathBuf, idx: &mut Vec<usize>, ctx: &Walk, out: &mut Vec<Recommendation>) {
    let (judge, model) = (ctx.judge, ctx.model);
    for (i, child) in node.children.iter().enumerate() {
        if child.size < MB {
            // Too small to suggest, and so is everything inside it. (Not `break`:
            // after a clean, totals shrink without re-sorting siblings.)
            continue;
        }
        path.push(&*child.name);
        idx.push(i);
        // Your protected list: never suggest it, never suggest a folder containing it.
        if let Some(p) = crate::safety::touches_protected(path, &ctx.protected) {
            if child.is_dir() && p != *path && p.starts_with(&*path) {
                walk(child, path, idx, ctx, out); // only an ancestor: look inside
            }
            idx.pop();
            path.pop();
            continue;
        }
        match ml::judge_with(judge, model, path, child) {
            Some(j) if j.verdict == Verdict::Keep => {
                // Keep it, but its contents can still hold leftovers
                // (e.g. node_modules inside ~/Documents/projects).
                if child.is_dir() && j.look_inside {
                    walk(child, path, idx, ctx, out);
                }
            }
            Some(mut j) => {
                // "Old & large" and AI guesses are broad: make sure nothing
                // irreplaceable is hiding inside before suggesting them.
                if child.is_dir()
                    && matches!(j.category, crate::judge::Category::OldFile | crate::judge::Category::AiGuess)
                {
                    if let Some(what) = risky_contents(child, 0) {
                        j.verdict = Verdict::Review;
                        j.reason = format!("{} ⚠ Contains {what}. Check before deleting.", j.reason);
                    }
                }
                if child.size >= min_size(j.verdict) {
                    out.push(Recommendation {
                        idx_path: idx.clone(),
                        path: path.clone(),
                        size: child.size,
                        judgement: j,
                    });
                }
            }
            None => {
                if child.is_dir() {
                    walk(child, path, idx, ctx, out);
                }
            }
        }
        idx.pop();
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scan, Progress, ScanOptions};
    use std::fs;

    #[test]
    fn finds_leftovers_inside_personal_folders() {
        let home = std::env::temp_dir().join(format!("sapu-adv-{}", std::process::id()));
        let proj = home.join("Documents/proj");
        fs::create_dir_all(proj.join("node_modules/pkg")).unwrap();
        fs::write(proj.join("package.json"), "{}").unwrap();
        fs::write(proj.join("node_modules/pkg/big.js"), vec![0u8; 3_000_000]).unwrap();
        fs::create_dir_all(home.join("Downloads")).unwrap();
        fs::write(home.join("Downloads/App.dmg"), vec![0u8; 6_000_000]).unwrap();

        let tree = scan(&home, &ScanOptions::default(), &Progress::default()).unwrap();
        let judge = Judge::with(home.clone(), u64::MAX / 4);
        let advice = advise(&tree, &judge);
        let names: Vec<_> =
            advice.items.iter().map(|r| r.path.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert!(names.contains(&"node_modules".to_string()), "{names:?}");
        assert!(names.contains(&"App.dmg".to_string()), "{names:?}");
        assert_eq!(advice.items[0].judgement.verdict, Verdict::Safe, "safe items first");

        let _ = fs::remove_dir_all(&home);
    }
}
