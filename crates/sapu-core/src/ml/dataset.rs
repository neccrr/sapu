//! Where training examples come from.
//!
//! 1. **Seed** (`seed.tsv`): hand-labelled macOS paths the rules don't cover,
//!    e.g. Electron apps hiding caches in Application Support.
//! 2. **Rules** (distillation): every item the rule-based judge recognises
//!    in *your* scan becomes an example, so the model learns the patterns
//!    behind the rules and can apply them to things the rules never named.
//! 3. **Your feedback** (`feedback.tsv`): what you trashed or marked Keep.
//!    Weighted ×5, so the model adapts to you quickly. Only your latest
//!    choice per path counts, and accepting one of the AI's own guesses
//!    counts less (×2), so it can't talk itself into things.
//!
//! Rows are `verdict, kind, path, size_mb, age_days[, flags]`, where flags
//! is a comma list: `gone` / `installed` (the app that made it), `ai`
//! (an accepted AI guess), `files=N`.

use super::apps::{AppStatus, Apps};
use super::features::{extract, set_app_status, Ctx};
use super::model::{class_index, group_of, Example};
use crate::judge::{Action, Judge, Verdict};
use crate::tree::{Kind, Node, Tree};
use std::io::Write;
use std::path::{Path, PathBuf};

const SEED: &str = include_str!("seed.tsv");
const FEEDBACK_WEIGHT: f32 = 5.0;
/// Accepting the AI's own guess confirms less than a choice you made yourself.
const ACCEPTED_GUESS_WEIGHT: f32 = 2.0;
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
fn synthetic(rel: &str, kind: &str, size_mb: f64, age_days: u64, files: u64, now: u64) -> (PathBuf, Node) {
    let name = Path::new(rel).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let is_dir = kind == "dir";
    let node = Node {
        name: name.into(),
        kind: if is_dir { Kind::Dir } else { Kind::File },
        size: (size_mb * 1e6) as u64,
        // 0 = unknown for folders (older rows didn't record it).
        files: if is_dir { files } else { 1 },
        incomplete: false,
        modified: now.saturating_sub(age_days * 86_400),
        logical: (size_mb * 1e6) as u64,
        children: Vec::new(),
    };
    (PathBuf::from(rel), node)
}

struct Row<'a> {
    verdict: Verdict,
    kind: &'a str,
    rel: &'a str,
    size_mb: f64,
    age_days: u64,
    flags: Vec<&'a str>,
}

fn rows(text: &str) -> impl Iterator<Item = Row<'_>> {
    text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).filter_map(|l| {
        let cols: Vec<&str> = l.split('\t').collect();
        if !(5..=6).contains(&cols.len()) {
            return None;
        }
        Some(Row {
            verdict: parse_verdict(cols[0])?,
            kind: cols[1],
            rel: cols[2],
            size_mb: cols[3].parse().ok()?,
            age_days: cols[4].parse().ok()?,
            flags: cols.get(5).map(|f| f.split(',').map(str::trim).collect()).unwrap_or_default(),
        })
    })
}

fn example(row: &Row, home: &Path, ctx: &Ctx, weight: f32) -> Example {
    let files = row.flags.iter().find_map(|f| f.strip_prefix("files=")?.parse().ok()).unwrap_or(0);
    let (rel, node) = synthetic(row.rel, row.kind, row.size_mb, row.age_days, files, ctx.now);
    let path = if rel.is_absolute() { rel } else { home.join(rel) };
    let mut features = extract(&path, &node, ctx);
    if row.flags.contains(&"gone") {
        set_app_status(&mut features, Some(AppStatus::Gone));
    } else if row.flags.contains(&"installed") {
        set_app_status(&mut features, Some(AppStatus::Installed));
    }
    let weight = if row.flags.contains(&"ai") { weight.min(ACCEPTED_GUESS_WEIGHT) } else { weight };
    Example { features, label: class_index(row.verdict), weight, group: group_of(&path), hand: true }
}

/// Parse labelled rows. Exposed for the evaluation harness.
pub fn parse_tsv(text: &str, home: &Path, ctx: &Ctx, weight: f32) -> Vec<Example> {
    rows(text).map(|r| example(&r, home, ctx, weight)).collect()
}

/// The hand-labelled seed. It has no Mac behind it, so apps count as
/// installed unless a row says `gone`.
pub fn seed_examples(ctx: &Ctx) -> Vec<Example> {
    let all = Apps::all_installed();
    let seed_ctx = Ctx { home: ctx.home, now: ctx.now, apps: &all };
    parse_tsv(SEED, ctx.home, &seed_ctx, SEED_WEIGHT)
}

/// Only the latest line per path counts (you may change your mind).
fn latest_per_path(text: &str) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter(|l| seen.insert(l.split('\t').nth(2).unwrap_or(l)))
        .collect();
    out.reverse();
    out
}

pub fn feedback_from(text: &str, ctx: &Ctx) -> Vec<Example> {
    let text = latest_per_path(text).join("\n");
    parse_tsv(&text, ctx.home, ctx, FEEDBACK_WEIGHT)
}

pub fn feedback_examples(ctx: &Ctx) -> Vec<Example> {
    std::fs::read_to_string(feedback_path()).map(|t| feedback_from(&t, ctx)).unwrap_or_default()
}

/// Rewrite feedback.tsv keeping only the latest line per path, once it's
/// mostly repeats. Safe to call any time; does nothing if there's no file.
pub fn compact_feedback() -> std::io::Result<()> {
    let path = feedback_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(()) };
    let total = text.lines().filter(|l| !l.trim().is_empty()).count();
    let keep = latest_per_path(&text);
    if total < 100 || keep.len() * 2 > total {
        return Ok(());
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, keep.join("\n") + "\n")?;
    std::fs::rename(tmp, path)
}

fn feedback_line(path: &Path, node: &Node, verdict: Verdict, judge: &Judge, ai: bool) -> String {
    let rel = path.strip_prefix(judge.home()).unwrap_or(path);
    let age = judge.now().saturating_sub(node.modified) / 86_400;
    let mut flags = vec![format!("files={}", node.files)];
    if ai {
        flags.push("ai".into());
    }
    format!(
        "{:?}\t{}\t{}\t{:.1}\t{}\t{}\n",
        verdict,
        if node.is_dir() { "dir" } else { "file" },
        rel.display(),
        node.size as f64 / 1e6,
        age,
        flags.join(",")
    )
}

fn append_feedback(line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::paths::ai_dir())?;
    std::fs::OpenOptions::new().create(true).append(true).open(feedback_path())?.write_all(line.as_bytes())
}

/// Remember a decision you made, for the next training run.
pub fn record_feedback(path: &Path, node: &Node, verdict: Verdict, judge: &Judge) -> std::io::Result<()> {
    append_feedback(&feedback_line(path, node, verdict, judge, false))
}

/// Remember that you accepted one of Sapu AI's own guesses. Counts less than
/// a choice you made yourself, so the model can't reinforce its own mistakes.
pub fn record_accepted_guess(path: &Path, node: &Node, verdict: Verdict, judge: &Judge) -> std::io::Result<()> {
    append_feedback(&feedback_line(path, node, verdict, judge, true))
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
                let ex = Example { group: group_of(path), ..Example::new(extract(path, child, ctx), label, 1.0) };
                out[label].push(ex);
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
                    let ex = Example { group: group_of(path), ..Example::new(extract(path, child, ctx), label, 0.5) };
                    out[label].push(ex);
                }
                if child.is_dir() {
                    walk(child, path, judge, ctx, out);
                }
            }
        }
        path.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_latest_wins_and_guesses_count_less() {
        let home = Path::new("/Users/t");
        let apps = Apps::unknown();
        let ctx = Ctx { home, now: 2_000_000_000, apps: &apps };
        let text = "Likely\tdir\tDownloads/a\t10\t5\n\
                    Keep\tdir\tDownloads/a\t10\t5\tfiles=3\n\
                    Safe\tdir\tLibrary/Application Support/X/Cache\t10\t5\tfiles=40,ai\n\
                    Safe\tdir\tLibrary/Application Support/X/Cache\t10\t5\tfiles=40,ai\n";
        let ex = feedback_from(text, &ctx);
        assert_eq!(ex.len(), 2);
        assert_eq!(ex[0].label, class_index(Verdict::Keep));
        assert_eq!(ex[0].weight, FEEDBACK_WEIGHT);
        assert_eq!(ex[1].weight, ACCEPTED_GUESS_WEIGHT);
        assert!(ex[1].features.iter().any(|f| f.name == "files:10-99"));
    }
}
