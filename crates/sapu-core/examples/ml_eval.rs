//! Offline evaluation of Sapu AI on a simulated Mac.
//!
//! `cargo run --release -p sapu-core --example ml_eval`
//!
//! 1. Builds a realistic home folder (caches, logs, projects, documents,
//!    downloads, app data) and lets the rules label it, exactly like a scan.
//! 2. Trains the model on seed + that scan, as `ml::train` does.
//! 3. Scores it on `ml_eval.tsv`: hand-labelled items the rules don't cover,
//!    from apps that are not in the seed. These are never trained on.
//! 4. Also runs 5-fold cross-validation over the seed, grouped by app, so the
//!    same app never sits on both sides of a split.

use sapu_core::ml::features::{self, Ctx};
use sapu_core::ml::model::{self, argmax, class_index, Example, Model, CLASSES};
use sapu_core::ml::{self, dataset};
use sapu_core::{Judge, Kind, Node, ScanStats, Tree, Verdict};
use std::path::{Path, PathBuf};

const NOW: u64 = 1_780_000_000;
const DAY: u64 = 86_400;
const MB: u64 = 1_000_000;
const SEED: &str = include_str!("../src/ml/seed.tsv");
const EVAL: &str = include_str!("ml_eval.tsv");

fn home() -> PathBuf {
    PathBuf::from("/Users/demo")
}

// ── tiny deterministic RNG ─────────────────────────────────────────────
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.below(v.len() as u64) as usize]
    }
}

// ── simulated scan ─────────────────────────────────────────────────────
fn leaf(name: &str, kind: Kind, mb: u64, days: u64, files: u64) -> Node {
    Node {
        name: name.into(),
        kind,
        size: mb * MB + 8192,
        files,
        incomplete: false,
        modified: NOW - days * DAY,
        logical: mb * MB + 8192,
        children: Vec::new(),
    }
}

fn insert(root: &mut Node, rel: &str, item: Node) {
    let comps: Vec<&str> = rel.split('/').collect();
    let mut cur = root;
    for c in &comps[..comps.len() - 1] {
        let pos = match cur.children.iter().position(|n| &*n.name == *c) {
            Some(p) => p,
            None => {
                cur.children.push(leaf(c, Kind::Dir, 0, 10_000, 0));
                cur.children.len() - 1
            }
        };
        cur = &mut cur.children[pos];
    }
    cur.children.push(item);
}

fn total(n: &mut Node) {
    if n.children.is_empty() {
        return;
    }
    let (mut size, mut files, mut modified) = (0, 0, 0);
    for c in &mut n.children {
        total(c);
        size += c.size;
        files += c.files;
        modified = modified.max(c.modified);
    }
    n.size = size;
    n.logical = size;
    n.files = files;
    n.modified = modified;
    n.children.sort_by(|a, b| b.size.cmp(&a.size));
}

/// Apps installed on the simulated Mac.
pub const INSTALLED: &[&str] = &[
    "Slack",
    "Discord",
    "Visual Studio Code",
    "Google Chrome",
    "Spotify",
    "Notion",
    "Microsoft Teams",
    "zoom.us",
    "Steam",
    "Figma",
    "Arc",
    "Brave Browser",
    "Firefox",
    "Obsidian",
    "1Password",
    "Telegram",
    "WhatsApp",
    "Microsoft Edge",
    "Microsoft Word",
    "Postman",
    "Cursor",
    "Signal",
    "Docker",
    "Xcode",
    "IntelliJ IDEA",
    "Adobe Photoshop 2025",
    "Anki",
    "Minecraft",
    "Claude",
    "Parallels Desktop",
    "Safari",
    "Mail",
    "Music",
    "Notes",
];

fn simulated_home() -> Tree {
    let mut rng = Rng(7);
    let mut root = leaf("demo", Kind::Dir, 0, 0, 0);
    let mut add = |rel: String, kind: Kind, mb: u64, days: u64, files: u64| {
        let name = rel.rsplit('/').next().unwrap().to_string();
        insert(&mut root, &rel, leaf(&name, kind, mb, days, files));
    };
    let vendors = ["apple", "google", "microsoft", "jetbrains", "spotify", "tinyspeck", "figma", "adobe", "mozilla"];
    let apps = ["Safari", "Chrome", "VSCode", "Teams", "client", "slackmacgap", "Desktop", "Photoshop", "helper"];
    let plain = [
        "Homebrew",
        "pip",
        "Yarn",
        "ms-playwright",
        "node-gyp",
        "Google",
        "Firefox",
        "SiriTTS",
        "GeoServices",
        "CloudKit",
        "typescript",
        "pnpm",
        "go-build",
        "JetBrains",
        "Cypress",
        "electron",
        "colima",
        "lima",
        "deno",
    ];
    for i in 0..120 {
        let name = if i % 3 == 0 {
            format!("{}-{i}", rng.pick(&plain))
        } else {
            format!("com.{}.{}{}", rng.pick(&vendors), rng.pick(&apps), i)
        };
        let (mb, days) = (1 + rng.below(1500), rng.below(200));
        add(format!("Library/Caches/{name}"), Kind::Dir, mb, days, 20 + rng.below(4000));
    }
    for i in 0..30 {
        let name = format!("{}{i}", rng.pick(&["DiagnosticReports", "JetBrains", "Homebrew", "com.apple.xpc", "zoom"]));
        add(format!("Library/Logs/{name}"), Kind::Dir, 1 + rng.below(200), rng.below(300), 5 + rng.below(200));
    }
    for i in 0..15 {
        add(
            format!("Library/Developer/Xcode/DerivedData/App{i}-abcdefgh{i}"),
            Kind::Dir,
            100 + rng.below(3000),
            rng.below(200),
            5000,
        );
    }
    for app in ["Slack", "discord", "Code", "Spotify", "Notion", "Figma", "Postman", "Cursor", "Signal"] {
        for (sub, mb) in [("Cache", 200), ("Code Cache", 80), ("GPUCache", 10), ("Local Storage", 5), ("IndexedDB", 40)] {
            add(format!("Library/Application Support/{app}/{sub}"), Kind::Dir, mb, rng.below(5), 300);
        }
    }
    let projects = ["web", "api", "site", "bot", "dash", "lab", "game", "cli", "app", "shop", "blog", "tools"];
    for (i, p) in projects.iter().enumerate() {
        add(format!("Documents/code/{p}/node_modules"), Kind::Dir, 100 + rng.below(800), rng.below(300), 30_000);
        if i % 2 == 0 {
            add(format!("Documents/code/{p}/__pycache__"), Kind::Dir, 1 + rng.below(20), rng.below(100), 40);
            add(format!("Documents/code/{p}/.pytest_cache"), Kind::Dir, 1, rng.below(100), 10);
        }
        for f in ["main.rs", "index.ts", "README.md", "app.py", "styles.css", "package.json"] {
            add(format!("Documents/code/{p}/src/{f}"), Kind::File, 1, rng.below(400), 1);
        }
    }
    let docs = ["pdf", "docx", "xlsx", "pptx", "key", "pages", "txt", "md", "numbers", "csv"];
    let words = ["Report", "Notes", "Invoice", "Lecture", "Plan", "Budget", "Essay", "Letter", "Summary", "Draft"];
    for i in 0..80 {
        let name = format!("{} {i}.{}", rng.pick(&words), rng.pick(&docs));
        add(format!("Documents/{name}"), Kind::File, 1 + rng.below(30), rng.below(800), 1);
    }
    for i in 0..25 {
        let name = format!("Screenshot 2025-0{}-1{} at 10.0{}.0{}.png", 1 + i % 9, i % 10, i % 6, i % 10);
        add(format!("Desktop/{name}"), Kind::File, 1 + rng.below(4), rng.below(700), 1);
    }
    for i in 0..40 {
        add(format!("Pictures/IMG_{}.HEIC", 1000 + i), Kind::File, 2 + rng.below(5), rng.below(900), 1);
    }
    for i in 0..10 {
        add(format!("Movies/clip-{i}.mov"), Kind::File, 100 + rng.below(3000), rng.below(900), 1);
        add(format!("Music/track-{i}.m4a"), Kind::File, 5 + rng.below(10), rng.below(900), 1);
    }
    let dl = ["dmg", "pkg", "zip", "tar.gz", "pdf", "png", "mp4", "csv", "iso", "log"];
    for i in 0..60 {
        let name = format!("{}-{i}.{}", rng.pick(&["setup", "invoice", "photos", "data", "app", "file"]), rng.pick(&dl));
        add(format!("Downloads/{name}"), Kind::File, 1 + rng.below(900), rng.below(500), 1);
    }
    for r in [".npm/_cacache/content-v2", ".cargo/registry/cache/index", ".gradle/caches/modules-2", ".Trash/old"] {
        add(r.into(), Kind::Dir, 200 + rng.below(2000), rng.below(60), 2000);
    }
    total(&mut root);
    Tree { root_path: home(), root, stats: ScanStats::default() }
}

// ── labelled rows ──────────────────────────────────────────────────────
#[derive(Clone)]
struct Row {
    verdict: Verdict,
    path: PathBuf,
    node: Node,
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

fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            let verdict = parse_verdict(c.first()?)?;
            let kind = if *c.get(1)? == "dir" { Kind::Dir } else { Kind::File };
            let rel = *c.get(2)?;
            let mb: f64 = c.get(3)?.parse().ok()?;
            let days: u64 = c.get(4)?.parse().ok()?;
            let name = rel.rsplit('/').next()?;
            let mut node = leaf(name, kind, 0, days, if kind == Kind::Dir { 120 } else { 1 });
            node.size = (mb * 1e6) as u64;
            node.logical = node.size;
            Some(Row { verdict, path: home().join(rel), node })
        })
        .collect()
}

/// Which app (or top folder) a row belongs to, for grouped cross-validation.
fn group(path: &Path) -> String {
    let rel = path.strip_prefix(home()).unwrap_or(path);
    let c: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
    let n = if c.first().is_some_and(|s| s == "library") { 3 } else { 2 };
    c.iter().take(n.min(c.len().saturating_sub(1)).max(1)).cloned().collect::<Vec<_>>().join("/")
}

// ── the version under test ─────────────────────────────────────────────
fn ctx(home: &Path) -> Ctx<'_> {
    Ctx { home, now: NOW }
}

fn to_example(r: &Row, c: &Ctx) -> Example {
    Example { features: features::extract(&r.path, &r.node, c), label: class_index(r.verdict), weight: 2.0 }
}

fn train(seed: &[Row], tree: &Tree, judge: &Judge) -> Model {
    let h = home();
    let c = ctx(&h);
    let mut all: Vec<Example> = seed.iter().map(|r| to_example(r, &c)).collect();
    all.extend(dataset::distill(tree, judge, &c));
    Model::train(&all, &model::TrainConfig::default())
}

fn probs(m: &Model, r: &Row) -> [f32; 4] {
    let h = home();
    m.predict(&features::extract(&r.path, &r.node, &ctx(&h)))
}

// ── scoring ────────────────────────────────────────────────────────────
#[derive(Default)]
struct Score {
    n: usize,
    removable: usize,
    keep: usize,
    exact: usize,
    suggested: usize,
    suggested_right: usize,
    found: usize,
    harmful: usize,
    overconfident: usize,
    nll: f64,
    bins: [(f64, f64, usize); 10],
}

impl Score {
    fn add(&mut self, m: &Model, judge: &Judge, r: &Row) {
        let p = probs(m, r);
        let truth = class_index(r.verdict);
        let k = argmax(&p);
        self.n += 1;
        self.exact += (k == truth) as usize;
        self.nll -= (p[truth].max(1e-6) as f64).ln();
        let b = ((p[k] * 10.0) as usize).min(9);
        self.bins[b].0 += p[k] as f64;
        self.bins[b].1 += (k == truth) as usize as f64;
        self.bins[b].2 += 1;
        if r.verdict == Verdict::Keep {
            self.keep += 1;
        } else {
            self.removable += 1;
        }
        if let Some(j) = ml::predict(m, judge, &r.path, &r.node) {
            self.suggested += 1;
            if j.verdict == r.verdict {
                self.suggested_right += 1;
            }
            if r.verdict == Verdict::Keep {
                self.harmful += 1;
            } else {
                self.found += 1;
                if r.verdict == Verdict::Review && j.verdict != Verdict::Review {
                    self.overconfident += 1;
                }
            }
        }
    }

    fn ece(&self) -> f64 {
        self.bins.iter().map(|(c, a, n)| if *n == 0 { 0.0 } else { (c - a).abs() }).sum::<f64>() / self.n as f64
    }

    fn print(&self, title: &str) {
        let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
        println!("{title}");
        println!("  items                        {} ({} removable, {} keep)", self.n, self.removable, self.keep);
        println!("  exact accuracy (4 classes)   {:.1}%", pct(self.exact, self.n));
        println!("  suggestions shown            {}", self.suggested);
        println!("    right verdict              {:.1}%", pct(self.suggested_right, self.suggested));
        println!("  removable items found        {:.1}%", pct(self.found, self.removable));
        println!("  Keep items suggested (bad)   {} of {}", self.harmful, self.keep);
        println!("  Review items shown as Safe/Likely  {}", self.overconfident);
        println!("  log loss                     {:.3}", self.nll / self.n as f64);
        println!("  calibration error (ECE)      {:.1} pts", 100.0 * self.ece());
    }
}

fn main() {
    let judge = Judge::with(home(), NOW);
    let tree = simulated_home();
    let seed = rows(SEED);
    let eval: Vec<Row> = rows(EVAL)
        .into_iter()
        .filter(|r| {
            let gap = judge.judge(&r.path, &r.node).is_none();
            if !gap {
                eprintln!("skipping (a rule covers it): {}", r.path.display());
            }
            gap
        })
        .collect();

    let model = train(&seed, &tree, &judge);
    println!("Training: {} examples, built-in holdout accuracy {:.1}%", model.metrics.examples, {
        100.0 * model.metrics.holdout_accuracy
    });
    println!();
    let mut s = Score::default();
    for r in &eval {
        s.add(&model, &judge, r);
    }
    s.print("Held-out test set (ml_eval.tsv)");

    // Grouped 5-fold CV over the seed.
    let mut groups: Vec<String> = seed.iter().map(|r| group(&r.path)).collect();
    groups.sort();
    groups.dedup();
    let mut cv = Score::default();
    for fold in 0..5 {
        let in_fold = |r: &Row| {
            let g = group(&r.path);
            groups.iter().position(|x| *x == g).unwrap() % 5 == fold
        };
        let train_rows: Vec<Row> = seed.iter().filter(|r| !in_fold(r)).cloned().collect();
        let m = train(&train_rows, &tree, &judge);
        for r in seed.iter().filter(|r| in_fold(r)) {
            if judge.judge(&r.path, &r.node).is_none() {
                cv.add(&m, &judge, r);
            }
        }
    }
    println!();
    cv.print("Seed, 5-fold cross-validation grouped by app");
    let _ = CLASSES;
}
