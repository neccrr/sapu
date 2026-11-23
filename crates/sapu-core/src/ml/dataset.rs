//! Where training examples come from.
//!
//! 1. **Seed** (`seed.tsv`): hand-labelled macOS paths the rules don't cover,
//!    e.g. Electron apps hiding caches in Application Support.
//! 2. **Rules** (distillation): every item the rule-based judge recognises
//!    in *your* scan becomes an example, so the model learns the patterns
//!    behind the rules and can apply them to things the rules never named.
//! 3. **Your feedback** (`feedback.tsv`): what you trashed or marked Keep.
//!    Weighted ×5, so the model adapts to you quickly.

use super::features::{extract, Ctx};
use super::model::{class_index, Example};
use crate::judge::{Action, Judge, Verdict};
use crate::tree::{Kind, Node, Tree};
use std::io::Write;
use std::path::{Path, PathBuf};

const SEED: &str = include_str!("seed.tsv");
const FEEDBACK_WEIGHT: f32 = 5.0;
const SEED_WEIGHT: f32 = 2.0;
/// Per-class cap on rule-derived examples, so one huge cache folder
/// doesn't dominate training.
const MAX_PER_CLASS: usize = 15_000;

/// Kept for compatibility; see [`crate::paths`].
pub fn data_dir() -> PathBuf {
    crate::paths::data_dir()
}

pub fn model_path() -> PathBuf {
    crate::paths::model_file()
}

pub fn feedback_path() -> PathBuf {
    crate::paths::feedback_file()
}

fn parse_verdict(s: &str) -> Option<Verdict> {
    Some(match s {
        "Safe" => Verdict::Safe,
        "Likely" => Verdict::Likely,
        "Review" => Verdict::Review,
        "Keep" => Verdict::Keep,
        _ => return None,
    })
}

/// Build a stand-in node for a labelled path (used by seed and feedback).
fn synthetic(rel: &str, kind: &str, size_mb: f64, age_days: u64, now: u64) -> (PathBuf, Node) {
    let name = Path::new(rel).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let is_dir = kind == "dir";
    let node = Node {
        name: name.into(),
        kind: if is_dir { Kind::Dir } else { Kind::File },
        size: (size_mb * 1e6) as u64,
        files: if is_dir { 120 } else { 1 },
        incomplete: false,
        modified: now.saturating_sub(age_days * 86_400),
        logical: (size_mb * 1e6) as u64,
        children: Vec::new(),
    };
    (PathBuf::from(rel), node)
}

fn parse_tsv(text: &str, home: &Path, ctx: &Ctx, weight: f32) -> Vec<Example> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let cols: Vec<&str> = l.split('\t').collect();
            if cols.len() != 5 {
                return None;
            }
            let verdict = parse_verdict(cols[0])?;
            let (rel, node) = synthetic(cols[2], cols[1], cols[3].parse().ok()?, cols[4].parse().ok()?, ctx.now);
            let path = if rel.is_absolute() { rel } else { home.join(rel) };
            Some(Example { features: extract(&path, &node, ctx), label: class_index(verdict), weight })
        })
        .collect()
}

pub fn seed_examples(ctx: &Ctx) -> Vec<Example> {
    parse_tsv(SEED, ctx.home, ctx, SEED_WEIGHT)
}

pub fn feedback_examples(ctx: &Ctx) -> Vec<Example> {
    std::fs::read_to_string(feedback_path()).map(|t| parse_tsv(&t, ctx.home, ctx, FEEDBACK_WEIGHT)).unwrap_or_default()
}

/// Remember a decision you made, for the next training run.
pub fn record_feedback(path: &Path, node: &Node, verdict: Verdict, judge: &Judge) -> std::io::Result<()> {
    let rel = path.strip_prefix(judge.home()).unwrap_or(path);
    let age = judge.now().saturating_sub(node.modified) / 86_400;
    let line = format!(
        "{:?}\t{}\t{}\t{:.1}\t{}\n",
        verdict,
        if node.is_dir() { "dir" } else { "file" },
        rel.display(),
        node.size as f64 / 1e6,
        age
    );
    std::fs::create_dir_all(crate::paths::ai_dir())?;
    std::fs::OpenOptions::new().create(true).append(true).open(feedback_path())?.write_all(line.as_bytes())
}

/// Label your scan with the rule-based judge.
pub fn distill(tree: &Tree, judge: &Judge, ctx: &Ctx) -> Vec<Example> {
    let mut buckets: [Vec<Example>; 4] = Default::default();
    let mut path = tree.root_path.clone();
    walk(&tree.root, &mut path, judge, ctx, &mut buckets);

    // Even out: keep at most MAX_PER_CLASS per class, sampled evenly.
    let mut out = Vec::new();
    for mut b in buckets {
        if b.len() > MAX_PER_CLASS {
            let step = b.len() as f64 / MAX_PER_CLASS as f64;
            b = (0..MAX_PER_CLASS).map(|i| b[(i as f64 * step) as usize].clone()).collect();
        }
        out.extend(b);
    }
    out
}

fn walk(node: &Node, path: &mut PathBuf, judge: &Judge, ctx: &Ctx, out: &mut [Vec<Example>; 4]) {
    let personal = ["Documents", "Desktop", "Pictures", "Movies", "Music"];
    for child in &node.children {
        if child.size < 4_096 {
            continue;
        }
        path.push(&*child.name);
        match judge.judge(path, child) {
            Some(j) => {
                let label = class_index(j.verdict);
                out[label].push(Example { features: extract(path, child, ctx), label, weight: 1.0 });
                // Look inside containers whose contents are judged too (Caches, Logs…).
                let descend = j.look_inside || j.action == Action::TrashContents;
                if child.is_dir() && descend {
                    walk(child, path, judge, ctx, out);
                }
            }
            None => {
                // Recent personal files are a reasonable "keep" signal.
                let rel = path.strip_prefix(ctx.home).ok();
                let in_personal = rel.is_some_and(|r| personal.iter().any(|p| r.starts_with(p)));
                let days = ctx.now.saturating_sub(child.modified) / 86_400;
                if in_personal && days < 365 && !child.is_dir() {
                    let label = class_index(Verdict::Keep);
                    out[label].push(Example { features: extract(path, child, ctx), label, weight: 0.5 });
                }
                if child.is_dir() {
                    walk(child, path, judge, ctx, out);
                }
            }
        }
        path.pop();
    }
}
