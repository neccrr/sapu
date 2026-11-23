mod format;
mod logging;
mod tui;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use console::{style, Style};
use indicatif::{ProgressBar, ProgressStyle};
use sapu_core::{
    cleaner::{self, Mode},
    rules, Judge, Kind, Progress, ScanOptions, Tree, Verdict,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// Sapu: see what's eating your disk, then sweep the safe stuff.
///
/// Run with no command to open the interactive explorer on your home folder.
#[derive(Parser)]
#[command(name = "sapu", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// More logs (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Only show errors
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
}

#[derive(Args, Clone)]
struct ScanArgs {
    /// Folder to scan (default: home)
    path: Option<PathBuf>,
    /// Skip hidden files and folders
    #[arg(long)]
    no_hidden: bool,
    /// Also cross into other volumes (external drives, network mounts)
    #[arg(long)]
    cross_volumes: bool,
}

impl ScanArgs {
    fn root(&self) -> Result<PathBuf> {
        match &self.path {
            Some(p) => Ok(p.clone()),
            None => Ok(cleaner::home()?),
        }
    }
    fn options(&self) -> ScanOptions {
        ScanOptions { include_hidden: !self.no_hidden, one_filesystem: !self.cross_volumes, ..Default::default() }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Level {
    Safe,
    Likely,
}

#[derive(Subcommand)]
enum Cmd {
    /// Interactive explorer (default)
    Ui(ScanArgs),
    /// Print the biggest items inside a folder
    Scan {
        #[command(flatten)]
        args: ScanArgs,
        /// How many items to show
        #[arg(short, long, default_value_t = 20)]
        top: usize,
    },
    /// Print the largest files anywhere under a folder
    Big {
        #[command(flatten)]
        args: ScanArgs,
        /// How many files to show
        #[arg(short = 'n', long, default_value_t = 25)]
        count: usize,
    },
    /// Judge everything and recommend what to clean
    Advise {
        #[command(flatten)]
        args: ScanArgs,
        /// Move recommended items to the Trash: `safe`, or `likely` (safe + probably safe)
        #[arg(long, value_enum)]
        clean: Option<Level>,
        /// Rules only, no Sapu AI
        #[arg(long)]
        no_ai: bool,
    },
    /// Train Sapu's AI on this Mac and show how well it does
    Model {
        #[command(flatten)]
        args: ScanArgs,
    },
    /// iCloud Drive: what's downloaded vs only in iCloud; remove downloads to free space
    Icloud {
        #[command(flatten)]
        args: ScanArgs,
        /// Remove the local download of this iCloud file/folder (it stays in iCloud)
        #[arg(long)]
        evict: Option<PathBuf>,
        /// Download this iCloud file/folder now
        #[arg(long)]
        download: Option<PathBuf>,
        /// Delete this item from iCloud on ALL devices (moves it to the Trash; asks you to type "delete")
        #[arg(long)]
        delete: Option<PathBuf>,
        /// Consent for --delete when not running in a terminal (scripts)
        #[arg(long, requires = "delete")]
        i_understand_this_deletes_from_icloud: bool,
    },
    /// Where Sapu keeps its own data on this Mac, and how big it is
    Data {
        /// Delete the scan cache (safe: it's only used for an instant start)
        #[arg(long)]
        clear_cache: bool,
    },
    /// What Sapu has moved to the Trash (newest first)
    History {
        /// How many batches to show
        #[arg(short = 'n', long, default_value_t = 10)]
        count: usize,
    },
    /// Put a batch back where it was (default: the most recent)
    Undo {
        /// Batch id from `sapu history`
        id: Option<u64>,
    },
    /// Never let Sapu touch a path (or anything containing it). No path: list them.
    Protect {
        path: Option<PathBuf>,
        /// Remove it from the protected list instead
        #[arg(long)]
        remove: bool,
    },
    /// List the built-in cleanup rules
    Rules,
    /// Show what can be cleaned (dry run unless --yes)
    Clean {
        /// Actually move files to the Trash
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    sapu_core::paths::migrate();
    let verbosity: i8 = if cli.quiet { -1 } else { cli.verbose.min(3) as i8 };

    let cmd = cli.cmd.unwrap_or(Cmd::Ui(ScanArgs { path: None, no_hidden: false, cross_volumes: false }));

    if let Cmd::Ui(args) = cmd {
        let log_file = logging::init(verbosity, logging::Target::File)?;
        info!("sapu {} starting TUI", env!("CARGO_PKG_VERSION"));
        return tui::run(args.root()?, args.options(), log_file);
    }

    logging::init(verbosity, logging::Target::Terminal)?;

    match cmd {
        Cmd::Ui(_) => unreachable!(),
        Cmd::Scan { args, top } => {
            let tree = scan_with_spinner(&args)?;
            print_summary(&tree);
            print_children(&tree, top);
        }
        Cmd::Big { args, count } => {
            let tree = scan_with_spinner(&args)?;
            print_summary(&tree);
            print_largest(&tree, count);
        }
        Cmd::Advise { args, clean, no_ai } => advise(&args, clean, no_ai)?,
        Cmd::Model { args } => model_report(&args)?,
        Cmd::History { count } => history(count),
        Cmd::Data { clear_cache } => data_cmd(clear_cache)?,
        Cmd::Icloud { args, evict, download, delete, i_understand_this_deletes_from_icloud } => {
            if let Some(p) = delete {
                icloud_delete(&p, i_understand_this_deletes_from_icloud)?
            } else {
                icloud_cmd(&args, evict, download)?
            }
        }
        Cmd::Undo { id } => undo(id)?,
        Cmd::Protect { path, remove } => protect(path, remove)?,
        Cmd::Rules => print_rules(),
        Cmd::Clean { yes } => clean(yes)?,
    }
    Ok(())
}

/// Run the scan on a worker thread while a spinner shows live counters.
fn scan_with_spinner(args: &ScanArgs) -> Result<Tree> {
    let root = args.root()?;
    let opts = args.options();
    let progress = Arc::new(Progress::default());

    let pb = logging::bars().add(ProgressBar::new_spinner());
    pb.set_style(
        ProgressStyle::with_template("{spinner:.yellow} {prefix:.bold} {msg:.dim}").unwrap().tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_prefix("scanning");
    pb.enable_steady_tick(Duration::from_millis(80));

    let worker = {
        let (root, p) = (root.clone(), progress.clone());
        std::thread::spawn(move || sapu_core::scan(&root, &opts, &p))
    };

    while !worker.is_finished() {
        let s = progress.snapshot();
        let width = console::Term::stderr().size().1 as usize;
        let current = format::truncate_left(&format::tilde(&progress.current_path()), width.saturating_sub(60).max(20));
        pb.set_message(format!(
            "{} · {} files · {} folders · {}",
            format::size(s.bytes),
            format::count(s.files),
            format::count(s.dirs),
            current
        ));
        std::thread::sleep(Duration::from_millis(100));
    }
    pb.finish_and_clear();

    let tree = worker
        .join()
        .map_err(|_| anyhow::anyhow!("scanner thread panicked"))?
        .with_context(|| format!("scanning {}", root.display()))?;

    if tree.stats.errors > 0 {
        warn!(
            unreadable = tree.stats.errors,
            "some items were unreadable — grant Full Disk Access to your terminal for a complete scan (run with -v to list them)"
        );
    }
    Ok(tree)
}

fn print_summary(tree: &Tree) {
    let st = &tree.stats;
    println!();
    println!("  {}", style(format::tilde(&tree.root_path)).bold());
    println!(
        "  {}  {}",
        style(format::size(tree.root.size)).yellow().bold(),
        style(format!(
            "on disk · {} files · {} folders · {:.2}s ({} files/s){}",
            format::count(st.files),
            format::count(st.dirs),
            st.elapsed.as_secs_f64(),
            format::count(st.files_per_sec() as u64),
            if st.hardlinks_skipped > 0 {
                format!(" · {} hard links counted once", format::count(st.hardlinks_skipped))
            } else {
                String::new()
            }
        ))
        .dim()
    );
    println!();
}

fn bar_style(frac: f64) -> Style {
    match frac {
        x if x >= 0.25 => Style::new().red(),
        x if x >= 0.10 => Style::new().color256(214),
        x if x >= 0.02 => Style::new().yellow(),
        _ => Style::new().green(),
    }
}

fn print_children(tree: &Tree, top: usize) {
    let total = tree.root.size;
    println!(
        "  {:>10}  {:<20}  {:>6}  {}",
        style("SIZE").dim().bold(),
        "",
        style("%").dim().bold(),
        style("NAME").dim().bold()
    );
    for c in tree.root.children.iter().take(top) {
        let frac = format::percent(c.size, total);
        let name = match c.kind {
            Kind::Dir => style(format!("{}/", c.name)).blue().bold(),
            Kind::Symlink => style(format!("{} →", c.name)).cyan(),
            _ => style(c.name.to_string()),
        };
        println!(
            "  {:>10}  {}  {:>6}  {}{}",
            style(format::size(c.size)).bold(),
            bar_style(frac).apply_to(format::bar(frac, 20)),
            style(format!("{:.1}%", frac * 100.0)).dim(),
            name,
            if c.incomplete { style(" ⚠").yellow().to_string() } else { String::new() }
        );
    }
    let rest = tree.root.children.len().saturating_sub(top);
    if rest > 0 {
        let rest_size: u64 = tree.root.children.iter().skip(top).map(|c| c.size).sum();
        println!(
            "  {:>10}  {}",
            style(format::size(rest_size)).dim(),
            style(format!("… {} more (use --top)", format::count(rest as u64))).dim()
        );
    }
    println!();
}

fn print_largest(tree: &Tree, n: usize) {
    let files = tree.largest_files(n);
    let max = files.first().map(|f| f.1).unwrap_or(0);
    println!("  {}", style(format!("Largest {} files", files.len())).bold());
    println!();
    for (path, size) in files {
        let frac = format::percent(size, max);
        println!(
            "  {:>10}  {}  {}",
            style(format::size(size)).bold(),
            bar_style(format::percent(size, tree.root.size)).apply_to(format::bar(frac, 12)),
            format::tilde(&path)
        );
    }
    println!();
}

fn print_rules() {
    println!();
    for r in rules::default_rules() {
        println!(
            "  {:<22} {}\n  {:<22} {}\n",
            style(r.id).yellow().bold(),
            style(format!("~/{}", r.rel_path)).bold(),
            "",
            style(r.description).dim()
        );
    }
}

fn clean(yes: bool) -> Result<()> {
    let pb = logging::bars().add(ProgressBar::new_spinner());
    pb.set_style(ProgressStyle::with_template("{spinner:.yellow} {msg}").unwrap());
    pb.set_message("measuring cleanup targets…");
    pb.enable_steady_tick(Duration::from_millis(80));
    let plan = cleaner::plan(&rules::default_rules())?;
    pb.finish_and_clear();

    let total: u64 = plan.iter().map(|i| i.bytes).sum();
    println!();
    for i in &plan {
        println!(
            "  {:>10}  {}  {}",
            style(format::size(i.bytes)).bold(),
            style(format!("{:<22}", i.rule_name)).yellow(),
            style(format::tilde(&i.path)).dim()
        );
    }
    println!("  {}", style("─".repeat(40)).dim());
    println!("  {:>10}  {}", style(format::size(total)).green().bold(), style("reclaimable").bold());
    println!();

    if !yes {
        println!(
            "  {} Nothing was touched. Re-run with {} to move these to the Trash.\n",
            style("dry run").cyan().bold(),
            style("--yes").bold()
        );
        return Ok(());
    }

    if !confirm(&format!("Move the contents of {} folders ({}) to the Trash?", plan.len(), format::size(total)))? {
        println!("  Cancelled. Nothing was touched.\n");
        return Ok(());
    }
    let out = cleaner::execute(&plan, Mode::Trash)?;
    println!(
        "  {} moved {} items to the Trash, freed {}",
        style("✓").green().bold(),
        format::count(out.trashed),
        style(format::size(out.freed_bytes)).green().bold()
    );
    for (p, err) in &out.failed {
        warn!(path = %p.display(), error = %err, "skipped");
    }
    println!();
    Ok(())
}

fn verdict_style(v: Verdict) -> Style {
    match v {
        Verdict::Safe => Style::new().green(),
        Verdict::Likely => Style::new().cyan(),
        Verdict::Review => Style::new().yellow(),
        Verdict::Keep => Style::new().blue(),
    }
}

fn advise(args: &ScanArgs, clean: Option<Level>, no_ai: bool) -> Result<()> {
    let tree = scan_with_spinner(args)?;
    let judge = Judge::new();
    let model = (!no_ai).then(|| sapu_core::ml::train(&tree, &judge));
    let advice = sapu_core::advise_with(&tree, &judge, model.as_ref());

    println!();
    println!("  {} {}", style("✨ Recommendations for").bold(), style(format::tilde(&tree.root_path)).bold());
    println!();
    for v in [Verdict::Safe, Verdict::Likely, Verdict::Review] {
        let n = advice.items.iter().filter(|r| r.judgement.verdict == v).count();
        println!(
            "  {}  {:>10}  {}",
            verdict_style(v).bold().apply_to(format!("{:<15}", v.label())),
            style(format::size(advice.total(v))).bold(),
            style(format!("{n} items")).dim()
        );
    }

    for v in [Verdict::Safe, Verdict::Likely, Verdict::Review] {
        let group: Vec<_> = advice.items.iter().filter(|r| r.judgement.verdict == v).collect();
        if group.is_empty() {
            continue;
        }
        println!();
        println!("  {}", verdict_style(v).bold().apply_to(v.label().to_uppercase()));
        for r in group {
            let jd = &r.judgement;
            let cat = if jd.is_ai() {
                style(format!("{:<14}", "🧠 AI guess")).magenta()
            } else {
                style(format!("{:<14}", jd.category.label())).dim()
            };
            println!("  {:>10}  {}  {}", style(format::size(r.size)).bold(), cat, format::tilde(&r.path));
            let note = if r.is_automatic() { jd.reason.clone() } else { format!("👉 {}", jd.reason) };
            println!("  {:>10}  {:<14}  {}", "", "", style(note).dim());
        }
    }
    println!();

    let Some(level) = clean else {
        if !advice.items.is_empty() {
            println!(
                "  Nothing was touched. Run with {} or {} to move items to the Trash.\n",
                style("--clean safe").bold(),
                style("--clean likely").bold()
            );
        }
        return Ok(());
    };

    let limit = match level {
        Level::Safe => Verdict::Safe,
        Level::Likely => Verdict::Likely,
    };
    // AI guesses are never cleaned automatically: review them in the app.
    let picked: Vec<_> = advice
        .items
        .iter()
        .filter(|r| {
            r.is_automatic()
                && !r.judgement.is_ai()
                && r.judgement.category != sapu_core::Category::ICloud // use `sapu icloud --evict`
                && r.judgement.verdict <= limit
        })
        .collect();
    let total: u64 = picked.iter().map(|r| r.size).sum();
    if picked.is_empty() {
        println!("  Nothing to clean at that level.\n");
        return Ok(());
    }
    if !confirm(&format!("Move {} items ({}) to the Trash?", picked.len(), format::size(total)))? {
        println!("  Cancelled. Nothing was touched.\n");
        return Ok(());
    }

    let journal = sapu_core::Journal::open();
    let batch = journal.begin(&format!("advise --clean ({} items)", picked.len()));
    let (mut done, mut freed) = (0usize, 0u64);
    for r in picked {
        let expect = tree.root.get(&r.idx_path).map(|n| {
            let check_mtime = r.judgement.action != sapu_core::judge::Action::TrashContents;
            cleaner::Expect::from_node(n, tree.stats.started_at, check_mtime)
        });
        match cleaner::apply(&r.path, &r.judgement, expect.as_ref(), &journal, batch) {
            Ok(res) => {
                done += 1;
                freed += r.size;
                info!(path = %r.path.display(), trashed = res.trashed, "cleaned");
                for (p, e) in res.failed {
                    warn!(path = %p.display(), error = %e, "skipped");
                }
            }
            Err(e) => warn!(path = %r.path.display(), error = %e, "skipped"),
        }
    }
    println!(
        "  {} cleaned {} items, about {} moved to the Trash. Undo with `sapu undo`.\n",
        style("✓").green().bold(),
        done,
        style(format::size(freed)).green().bold()
    );
    Ok(())
}

fn model_report(args: &ScanArgs) -> Result<()> {
    use sapu_core::ml::{self, model::CLASSES};
    let tree = scan_with_spinner(args)?;
    let judge = Judge::new();
    let model = ml::train(&tree, &judge);
    let m = &model.metrics;

    println!();
    println!("  {}", style("🧠 Sapu AI — training report").bold());
    println!();
    println!("  {:<22} logistic regression, 65,536 hashed features × 4 classes", style("Model").dim());
    println!(
        "  {:<22} {} ({} seed · {} from rules on your scan · {} your feedback)",
        style("Examples").dim(),
        style(format::count(m.examples as u64)).bold(),
        format::count(m.from_seed as u64),
        format::count(m.from_rules as u64),
        format::count(m.from_feedback as u64)
    );
    println!(
        "  {:<22} {}",
        style("Per class").dim(),
        CLASSES
            .iter()
            .zip(m.per_class)
            .map(|(c, n)| format!("{} {}", c.short(), format::count(n as u64)))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    println!("  {:<22} {} ms", style("Training time").dim(), m.train_ms);
    println!(
        "  {:<22} {}",
        style("Held-out accuracy").dim(),
        style(format!("{:.1}%", m.holdout_accuracy * 100.0)).green().bold()
    );
    println!("  {:<22} {}", style("Saved to").dim(), format::tilde(&ml::model_path()));

    println!();
    println!("  {}", style("Confusion matrix (held-out 10%, rows = true label)").bold());
    print!("  {:>10}", "");
    for c in CLASSES {
        print!("{:>9}", c.short());
    }
    println!();
    for (i, row) in m.confusion.iter().enumerate() {
        print!("  {:>10}", verdict_style(CLASSES[i]).apply_to(CLASSES[i].short()));
        for (j, n) in row.iter().enumerate() {
            let cell = format!("{n:>9}");
            if i == j {
                print!("{}", style(cell).green())
            } else if *n > 0 {
                print!("{}", style(cell).red())
            } else {
                print!("{}", style(cell).dim())
            }
        }
        println!();
    }

    println!();
    println!("  {}", style("What the model learned (strongest signals per class)").bold());
    for (k, c) in CLASSES.iter().enumerate() {
        let top: Vec<String> =
            model.top_features(k, 8).into_iter().map(|(n, _)| ml::features::describe(&n).unwrap_or(n)).collect();
        println!(
            "  {}  {}",
            verdict_style(*c).bold().apply_to(format!("{:<7}", c.short())),
            style(top.join(" · ")).dim()
        );
    }

    // What it says about things the rules don't recognise
    let advice = sapu_core::advise_with(&tree, &judge, Some(&model));
    let ai: Vec<_> = advice.items.iter().filter(|r| r.judgement.is_ai()).take(12).collect();
    println!();
    println!("  {}", style(format!("AI suggestions for items no rule recognised ({})", ai.len())).bold());
    for r in ai {
        println!(
            "  {:>10}  {}  {}",
            style(format::size(r.size)).bold(),
            verdict_style(r.judgement.verdict).apply_to(format!("{:<7}", r.judgement.verdict.short())),
            format::tilde(&r.path)
        );
        println!("  {:>10}  {:<7}  {}", "", "", style(&r.judgement.reason).dim());
    }
    println!();
    Ok(())
}

/// y/N prompt. When not run from a terminal (scripts), `--clean`/`--yes` alone is consent.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Ok(true);
    }
    print!("  {} {} ", style(question).bold(), style("[y/N]").dim());
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn history(count: usize) {
    use sapu_core::journal::ItemState;
    let h = sapu_core::Journal::open().history();
    println!();
    if h.is_empty() {
        println!("  Nothing cleaned yet.\n");
        return;
    }
    for b in h.iter().take(count) {
        let when = chrono_like(b.time);
        println!("  {}  {}  {}", style(format!("#{}", b.id)).dim(), style(&b.label).bold(), style(when).dim());
        println!(
            "      {} items · {} to Trash · {} restored · {} skipped · {} can be undone",
            b.items.len(),
            format::size(b.bytes_trashed()),
            b.count(ItemState::Restored),
            b.count(ItemState::Failed),
            style(b.restorable()).green().bold()
        );
    }
    println!(
        "\n  Undo the latest with {}, or a specific one with {}.\n",
        style("sapu undo").bold(),
        style("sapu undo <id>").bold()
    );
}

fn chrono_like(epoch: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(epoch);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

fn undo(id: Option<u64>) -> Result<()> {
    let journal = sapu_core::Journal::open();
    let history = journal.history();
    let batch = match id {
        Some(id) => history.iter().find(|b| b.id == id),
        None => history.iter().find(|b| b.restorable() > 0),
    };
    let Some(batch) = batch else {
        println!("\n  Nothing to undo.\n");
        return Ok(());
    };
    println!();
    if !confirm(&format!("Put back {} items from \"{}\"?", batch.restorable(), batch.label))? {
        println!("  Cancelled.\n");
        return Ok(());
    }
    let r = journal.undo(batch.id);
    println!("  {} restored {} items", style("↩").green().bold(), r.restored);
    for (p, e) in &r.failed {
        println!("  {} {}: {}", style("⚠").yellow(), format::tilde(p), style(e).dim());
    }
    println!();
    Ok(())
}

fn protect(path: Option<PathBuf>, remove: bool) -> Result<()> {
    use sapu_core::safety;
    println!();
    match path {
        Some(p) if remove => {
            if safety::unprotect(&p)? {
                println!("  {} no longer protected\n", format::tilde(&p));
            } else {
                println!("  {} wasn't on the list\n", format::tilde(&p));
            }
        }
        Some(p) => {
            safety::protect(&p).with_context(|| format!("protecting {}", p.display()))?;
            println!("  🛡 {} is protected. Sapu will never touch it or any folder containing it.\n", format::tilde(&p));
        }
        None => {
            let list = safety::user_protected();
            if list.is_empty() {
                println!("  Nothing protected yet. Add with {}\n", style("sapu protect <path>").bold());
            }
            for p in list {
                println!("  🛡 {}", format::tilde(&p));
            }
            println!("  {}\n", style(format!("(stored in {})", format::tilde(&safety::protected_file()))).dim());
        }
    }
    Ok(())
}

fn icloud_cmd(args: &ScanArgs, evict: Option<PathBuf>, download: Option<PathBuf>) -> Result<()> {
    use sapu_core::icloud;
    println!();
    if let Some(p) = evict {
        icloud::evict(&p)?;
        println!("  ☁ Removed the download of {}. It's still in iCloud.\n", format::tilde(&p));
        return Ok(());
    }
    if let Some(p) = download {
        icloud::download(&p)?;
        println!("  ☁ Downloading {} from iCloud.\n", format::tilde(&p));
        return Ok(());
    }
    let tree = scan_with_spinner(args)?;
    let home = cleaner::home()?;
    let s = icloud::summarize(&tree, &home);
    if !s.available {
        println!("  iCloud Drive isn't set up on this Mac.\n");
        return Ok(());
    }
    if s.not_scanned {
        println!("  This scan doesn't include iCloud. Run `sapu icloud` without a path to scan your home folder.\n");
        return Ok(());
    }
    println!("  {}", style("☁ iCloud Drive").bold());
    println!("  {:<26} {}", style("In iCloud").dim(), style(format::size(s.in_cloud)).bold());
    println!("  {:<26} {}", style("Downloaded on this Mac").dim(), style(format::size(s.on_mac)).yellow().bold());
    println!(
        "  {:<26} {}",
        style("Only in iCloud").dim(),
        style(format::size(s.in_cloud.saturating_sub(s.on_mac))).green().bold()
    );
    if s.desktop_documents_synced {
        println!("  {}", style("Desktop & Documents sync with iCloud.").dim());
    }
    if let Some((_, size)) = s.photos_library.filter(|(_, n)| *n > 2_000_000_000) {
        println!(
            "  {}",
            style(format!(
                "Photos library: {}. Consider Photos → Settings → iCloud → Optimize Mac Storage.",
                format::size(size)
            ))
            .dim()
        );
    }
    if !s.downloaded.is_empty() {
        println!();
        println!("  {}", style("Downloaded, biggest first").bold());
        for i in s.downloaded.iter().take(20) {
            println!("  {:>10}  {}", style(format::size(i.on_mac)).bold(), format::tilde(&i.path));
        }
        println!("\n  Free space without deleting: {}", style("sapu icloud --evict <path>").bold());
    }
    if !s.cloud_only.is_empty() {
        println!();
        println!("  {}", style("Only in iCloud").bold());
        for i in s.cloud_only.iter().take(10) {
            println!("  {:>10}  {}", style(format!("☁ {}", format::size(i.in_cloud))).dim(), format::tilde(&i.path));
        }
    }
    println!();
    Ok(())
}

/// Delete an iCloud item everywhere, only after explicit consent.
fn icloud_delete(path: &std::path::Path, flag_consent: bool) -> Result<()> {
    use sapu_core::safety::ICloud;
    use std::io::{IsTerminal, Write};
    let home = cleaner::home()?;
    let roots = sapu_core::icloud::roots(&home);
    let canon = path.canonicalize().with_context(|| format!("{} not found", path.display()))?;
    if !sapu_core::icloud::in_icloud(&canon, &roots) {
        anyhow::bail!("{} isn't in iCloud. Use the app or `sapu advise` for normal files.", format::tilde(&canon));
    }
    let size = sapu_core::scanner::dir_size(&canon);
    println!();
    println!("  {} {}", style("☁ Delete from iCloud:").red().bold(), format::tilde(&canon));
    println!("  {}", style("This removes it from iCloud on ALL your devices (iPhone, iPad, other Macs).").red());
    println!("  {}", style("It goes to this Mac's Trash first, and `sapu undo` can put it back.").dim());
    println!("  {}", style("To only free space on this Mac, use --evict instead (keeps it in iCloud).").dim());
    println!();

    let consent = if std::io::stdin().is_terminal() {
        print!("  Type {} to confirm: ", style("delete").bold());
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        answer.trim() == "delete"
    } else {
        flag_consent
    };
    if !consent {
        println!("  Cancelled. Nothing was touched.\n");
        info!(path = %canon.display(), "iCloud delete cancelled");
        return Ok(());
    }
    let journal = sapu_core::Journal::open();
    let batch = journal.begin(&format!("Deleted {} from iCloud", format::tilde(&canon)));
    cleaner::trash_item(&canon, &journal, batch, size, "iCloud delete, consent given", ICloud::ConsentGiven)?;
    println!("  {} deleted from iCloud. Undo with `sapu undo`.\n", style("✓").green().bold());
    Ok(())
}

fn data_cmd(clear_cache: bool) -> Result<()> {
    use sapu_core::paths;
    println!();
    if clear_cache {
        sapu_core::cache::clear()?;
        println!("  {} scan cache cleared\n", style("✓").green().bold());
        return Ok(());
    }
    println!("  {}", style("Sapu's data on this Mac (never inside the app, never online)").bold());
    println!();
    for u in paths::usage() {
        println!(
            "  {:<12} {:>10}  {}",
            style(u.label).bold(),
            format::size(u.bytes),
            style(format!("{} · {}", format::tilde(&u.path), u.note)).dim()
        );
    }
    println!("\n  {}\n", style("Clear the scan cache with `sapu data --clear-cache`.").dim());
    Ok(())
}
