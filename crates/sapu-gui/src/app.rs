use crate::theme::{self, glass, pal, serif};
use crate::{fmt, treemap};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Id, Key, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
use sapu_core::cleaner::Expect;
use sapu_core::journal::{self, ItemState, Journal};
use sapu_core::{
    advise_with, cleaner, icloud, ml, safety, Advice, Category, Judge, Judgement, Kind, Node, Progress, Recommendation,
    ScanOptions, Tree, Verdict,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

fn verdict_color(v: Verdict) -> Color32 {
    match v {
        Verdict::Safe => pal().ok,
        Verdict::Likely => pal().likely,
        Verdict::Review => pal().warn,
        Verdict::Keep => pal().keep,
    }
}

const ROW_H: f32 = 30.0;

/// Room for the macOS traffic-light buttons (content runs under the title bar).
const TITLEBAR_INSET: i8 = if cfg!(target_os = "macos") { 80 } else { 16 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Size,
    Name,
    Items,
}

impl SortBy {
    fn label(self) -> &'static str {
        match self {
            SortBy::Size => "Size",
            SortBy::Name => "Name",
            SortBy::Items => "Items",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    Advice,
    Cloud,
    Safety,
}

/// A clean running on a background thread (so the window stays responsive
/// and it can be cancelled between items).
struct Job {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: usize,
    current: Arc<Mutex<String>>,
    handle: Option<JoinHandle<Vec<JobResult>>>,
}

struct JobResult {
    rec: Recommendation,
    outcome: Result<cleaner::Applied, String>,
    /// Fresh scan of a folder whose contents were cleared.
    fresh: Option<Node>,
}

/// Batch waiting for confirmation.
struct BatchConfirm {
    n: usize,
    bytes: u64,
    /// Big or includes Review/AI items: needs an extra tick to proceed.
    risky: Option<String>,
    ack: bool,
}

type ScanResult = sapu_core::Result<(Tree, Advice, Option<ml::Model>)>;

enum Phase {
    Scanning { progress: Arc<Progress>, handle: Option<JoinHandle<ScanResult>>, started: Instant },
    Ready(Box<Browser>),
    Failed(String),
}

struct Browser {
    tree: Tree,
    /// Child indices from the root to the folder being shown.
    path: Vec<usize>,
    /// Selected child (index into the current folder's children).
    selected: Option<usize>,
    /// Remembered selection for each ancestor, restored when going back.
    history: Vec<Option<usize>>,
    /// Current folder's children in display order.
    view: Vec<usize>,
    sort: SortBy,
    /// Scroll the list so the selection is visible on the next frame.
    scroll_to_selection: bool,
    /// When the current folder was opened (drives the grow-in animation).
    anim_at: Instant,
    judge: Judge,
    /// Judgement for each child of the current folder (by child index).
    verdicts: Vec<Option<Judgement>>,
    advice: Advice,
    /// Ticked recommendations (parallel to `advice.items`).
    checked: Vec<bool>,
    mode: Mode,
    /// Sapu AI, trained right after the scan.
    model: Option<ml::Model>,
    /// iCloud numbers for the ☁ tab (computed when opened).
    cloud: Option<icloud::Summary>,
    /// Showing a cached scan from this time (seconds since 1970) while a fresh
    /// one runs. Nothing can be cleaned until it's `None`.
    cached_at: Option<u64>,
    icloud_roots: Vec<PathBuf>,
}

impl Browser {
    fn new(tree: Tree, advice: Advice, model: Option<ml::Model>) -> Self {
        let mut b = Self {
            tree,
            path: Vec::new(),
            selected: None,
            history: Vec::new(),
            view: Vec::new(),
            sort: SortBy::Size,
            scroll_to_selection: false,
            anim_at: Instant::now(),
            judge: Judge::new(),
            verdicts: Vec::new(),
            advice: Advice::default(),
            checked: Vec::new(),
            mode: Mode::Browse,
            model,
            cloud: None,
            cached_at: None,
            icloud_roots: dirs::home_dir().map(|h| icloud::roots(&h)).unwrap_or_default(),
        };
        b.set_advice(advice);
        b.rebuild_view();
        b.selected = b.view.first().copied();
        b
    }

    fn set_advice(&mut self, advice: Advice) {
        // Pre-tick what's automatic and Safe/Likely; Review stays unticked.
        self.checked = advice
            .items
            .iter()
            .map(|r| {
                r.is_automatic()
                    && !r.judgement.is_ai()
                    && r.judgement.category != Category::ICloud // keeping files offline is your call
                    && r.judgement.verdict <= Verdict::Likely
            })
            .collect();
        self.advice = advice;
    }

    fn refresh_advice(&mut self) {
        let advice = advise_with(&self.tree, &self.judge, self.model.as_ref());
        self.set_advice(advice);
    }

    fn selection_total(&self) -> (usize, u64) {
        self.advice
            .items
            .iter()
            .zip(&self.checked)
            .filter(|(r, c)| **c && r.is_automatic())
            .fold((0, 0), |(n, b), (r, _)| (n + 1, b + r.size))
    }

    /// Jump the browser to a recommendation.
    fn show_path(&mut self, idx_path: &[usize]) {
        let Some((&last, parents)) = idx_path.split_last() else { return };
        self.path = parents.to_vec();
        self.anim_at = Instant::now();
        self.history = parents.iter().map(|&i| Some(i)).collect();
        self.selected = Some(last);
        self.mode = Mode::Browse;
        self.rebuild_view();
        self.scroll_to_selection = true;
    }

    fn current(&self) -> &Node {
        self.tree.root.get(&self.path).unwrap_or(&self.tree.root)
    }

    fn rebuild_view(&mut self) {
        let node = self.tree.root.get(&self.path).unwrap_or(&self.tree.root);
        let mut view: Vec<usize> = (0..node.children.len()).collect();
        match self.sort {
            // Sizes can change after cleaning, so sort live rather than trusting stored order.
            SortBy::Size => view.sort_by_key(|&i| std::cmp::Reverse(node.children[i].size)),
            SortBy::Name => view.sort_by_cached_key(|&i| node.children[i].name.to_lowercase()),
            SortBy::Items => view.sort_by_key(|&i| std::cmp::Reverse(node.children[i].files)),
        }
        self.view = view;
        let dir = self.tree.path_of(&self.path);
        self.verdicts = node
            .children
            .iter()
            .map(|c| ml::judge_with(&self.judge, self.model.as_ref(), &dir.join(&*c.name), c))
            .collect();
        if self.selected.is_some_and(|s| s >= node.children.len()) {
            self.selected = self.view.first().copied();
        }
    }

    fn enter(&mut self, child: usize) {
        if !self.current().children.get(child).is_some_and(|c| c.is_dir()) {
            return;
        }
        self.history.push(Some(child));
        self.path.push(child);
        self.anim_at = Instant::now();
        self.selected = None;
        self.rebuild_view();
        self.selected = self.view.first().copied();
        self.scroll_to_selection = true;
    }

    fn back(&mut self) {
        if self.path.pop().is_some() {
            self.anim_at = Instant::now();
            self.selected = self.history.pop().flatten();
            self.rebuild_view();
            self.scroll_to_selection = true;
        }
    }

    /// Jump to an ancestor (breadcrumb click). `depth` 0 = scan root.
    fn go_to_depth(&mut self, depth: usize) {
        while self.path.len() > depth {
            self.back();
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let pos = self
            .selected
            .and_then(|s| self.view.iter().position(|&v| v == s))
            .map(|p| p as isize + delta)
            .unwrap_or(0)
            .clamp(0, self.view.len() as isize - 1);
        self.selected = Some(self.view[pos as usize]);
        self.scroll_to_selection = true;
    }

    fn idx_path(&self, child: usize) -> Vec<usize> {
        let mut p = self.path.clone();
        p.push(child);
        p
    }
}

/// Things the UI asks for; applied after drawing so borrows stay simple.
/// Every action is logged (debug level) before it runs.
#[derive(Debug)]
enum Action {
    ToggleLogs,
    Select(usize),
    Enter(usize),
    Back,
    GoToDepth(usize),
    AskTrash(usize),
    Reveal(usize),
    SetSort(SortBy),
    Rescan,
    ChooseFolder,
    StopScan,
    SetMode(Mode),
    ToggleCheck(usize),
    /// Tick every automatic item up to this verdict (None = untick all).
    CheckUpTo(Option<Verdict>),
    ShowRec(usize),
    AskBatch,
    /// "Not this one": remove a recommendation and teach the AI to keep it.
    RejectRec(usize),
    /// Browser: mark a child of the current folder as Keep (teaches the AI).
    MarkKeep(usize),
    /// Browser: add to your never-touch list.
    Protect(usize),
    Unprotect(PathBuf),
    Undo(u64),
    CancelJob,
    /// iCloud: remove the local download (index path in the tree).
    Evict(Vec<usize>),
    /// iCloud: download now.
    Download(Vec<usize>),
    /// Jump to any item in the browser.
    ShowIdx(Vec<usize>),
    /// Ask to trash any item by index path (used by the iCloud tab).
    AskTrashIdx(Vec<usize>),
    /// Delete Sapu's own scan cache.
    ClearCache,
    /// Show one of Sapu's folders in Finder.
    RevealPath(PathBuf),
}

/// Build the confirmation for trashing an item, flagging iCloud items.
fn confirm_for(b: &Browser, idx_path: Vec<usize>) -> Option<Confirm> {
    let node = b.tree.root.get(&idx_path)?;
    let path = b.tree.path_of(&idx_path);
    let icloud = icloud::in_icloud(&path, &b.icloud_roots);
    if icloud {
        info!(path = %path.display(), "asking for iCloud delete consent");
    }
    Some(Confirm { size: node.size.max(node.logical), is_dir: node.is_dir(), icloud, consent: false, path, idx_path })
}

struct Confirm {
    path: PathBuf,
    idx_path: Vec<usize>,
    size: u64,
    is_dir: bool,
    /// Lives in iCloud: deleting it removes it from every device.
    icloud: bool,
    /// The person ticked "I understand…" (required for iCloud items).
    consent: bool,
}

enum Status {
    Ok(String),
    Err(String),
}

pub struct SapuApp {
    root: PathBuf,
    opts: ScanOptions,
    phase: Phase,
    confirm: Option<Confirm>,
    confirm_batch: Option<BatchConfirm>,
    job: Option<Job>,
    history: Vec<journal::Batch>,
    protected: Vec<PathBuf>,
    status: Option<(Status, Instant)>,
    log_file: Option<PathBuf>,
    logs: crate::logview::LogViewer,
    /// A fresh scan running in the background while results are on screen.
    refresh: Option<(Arc<Progress>, JoinHandle<ScanResult>, Instant)>,
    /// Sizes of Sapu's own folders (Safety tab).
    data_usage: Vec<sapu_core::paths::Usage>,
    /// Last theme seen, to log Light/Dark switches.
    last_dark: Option<bool>,
}

impl SapuApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        root: PathBuf,
        log_file: Option<PathBuf>,
        buffer: crate::logview::LogBuffer,
    ) -> Self {
        theme::apply(&cc.egui_ctx);
        // macOS blur behind the window ("liquid glass"), opt-in with SAPU_GLASS=1.
        // Without it (or if it fails) everything uses solid colours.
        #[cfg(target_os = "macos")]
        if crate::glass::requested() {
            match crate::glass::install(cc) {
                Ok(()) => {
                    theme::set_glass(true);
                    info!("glass: blur installed behind content");
                }
                Err(e) => warn!(error = %e, "glass: couldn't install blur, using solid colours"),
            }
        }
        if !crate::glass::requested() {
            info!("glass: off (start with SAPU_GLASS=1 to try the blur)");
        }
        let mut app = Self {
            root,
            opts: ScanOptions::default(),
            phase: Phase::Failed(String::new()),
            confirm: None,
            confirm_batch: None,
            job: None,
            history: Vec::new(),
            protected: Vec::new(),
            status: None,
            logs: crate::logview::LogViewer::new(buffer, log_file.clone()),
            log_file,
            last_dark: None,
            refresh: None,
            data_usage: Vec::new(),
        };
        app.open_root();
        app
    }

    /// Scan + train + advise on a worker thread; the result is also cached.
    fn spawn_scan(&self) -> (Arc<Progress>, JoinHandle<ScanResult>) {
        let progress = Arc::new(Progress::default());
        let (root, opts, p) = (self.root.clone(), self.opts.clone(), progress.clone());
        info!(root = %root.display(), "starting scan");
        let handle = std::thread::spawn(move || {
            let tree = sapu_core::scan(&root, &opts, &p)?;
            // Save for an instant start next time (in the background, it's only a cache).
            let copy = tree.clone();
            std::thread::spawn(move || {
                if let Err(e) = sapu_core::cache::save(&copy) {
                    warn!(error = %e, "couldn't save scan cache");
                }
            });
            let judge = Judge::new();
            let model = ml::train(&tree, &judge);
            let advice = advise_with(&tree, &judge, Some(&model));
            info!(recommendations = advice.items.len(), "advice ready");
            Ok((tree, advice, Some(model)))
        });
        (progress, handle)
    }

    /// Full-screen scan (nothing to show yet).
    fn start_scan(&mut self) {
        if matches!(self.phase, Phase::Ready(_)) {
            return self.start_refresh();
        }
        let (progress, handle) = self.spawn_scan();
        self.phase = Phase::Scanning { progress, handle: Some(handle), started: Instant::now() };
    }

    /// Fresh scan in the background; results stay on screen meanwhile.
    fn start_refresh(&mut self) {
        if self.refresh.is_some() {
            return;
        }
        let (progress, handle) = self.spawn_scan();
        self.refresh = Some((progress, handle, Instant::now()));
    }

    /// Show the last scan of `root` instantly if there is one, then refresh it.
    fn open_root(&mut self) {
        self.refresh = None;
        if let Some(tree) = sapu_core::cache::load(&self.root) {
            let judge = Judge::new();
            let model = ml::load();
            let advice = advise_with(&tree, &judge, model.as_ref());
            let at = tree.stats.started_at;
            let mut b = Browser::new(tree, advice, model);
            b.cached_at = Some(at);
            self.phase = Phase::Ready(Box::new(b));
            self.start_refresh();
        } else {
            self.phase = Phase::Failed(String::new());
            self.start_scan();
        }
    }

    fn poll_refresh(&mut self) {
        let Some((_, handle, _)) = &self.refresh else { return };
        if !handle.is_finished() {
            return;
        }
        let (_, handle, _) = self.refresh.take().expect("checked");
        match handle.join() {
            Ok(Ok((tree, advice, model))) => {
                let st = tree.stats;
                info!(files = st.files, ms = st.elapsed.as_millis() as u64, "fresh scan replaces the shown one");
                let mode = match &self.phase {
                    Phase::Ready(b) => b.mode,
                    _ => Mode::Browse,
                };
                let mut b = Browser::new(tree, advice, model);
                b.mode = mode;
                if mode == Mode::Cloud {
                    b.cloud = dirs::home_dir().map(|h| icloud::summarize(&b.tree, &h));
                }
                self.phase = Phase::Ready(Box::new(b));
                self.set_status(Status::Ok(format!(
                    "Up to date: {} files scanned in {:.1}s",
                    fmt::count(st.files),
                    st.elapsed.as_secs_f64()
                )));
            }
            Ok(Err(e)) => {
                error!(error = %e, "refresh failed");
                self.set_status(Status::Err(format!("Couldn't refresh the scan: {e}")));
            }
            Err(_) => self.set_status(Status::Err("The scanner crashed while refreshing.".into())),
        }
    }

    /// Cleaning on a cached (possibly outdated) scan isn't allowed.
    fn blocked_while_cached(&self, a: &Action) -> bool {
        let Phase::Ready(b) = &self.phase else { return false };
        b.cached_at.is_some()
            && matches!(
                a,
                Action::AskTrash(_)
                    | Action::AskTrashIdx(_)
                    | Action::AskBatch
                    | Action::Evict(_)
                    | Action::Download(_)
            )
    }

    fn set_status(&mut self, s: Status) {
        match &s {
            Status::Ok(m) => debug!(message = %m, "status"),
            Status::Err(m) => warn!(message = %m, "status"),
        }
        self.status = Some((s, Instant::now()));
    }

    fn poll_scan(&mut self) {
        let Phase::Scanning { handle, .. } = &mut self.phase else { return };
        if !handle.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        let result = handle.take().expect("checked above").join();
        self.phase = match result {
            Ok(Ok((tree, advice, model))) => {
                let st = tree.stats;
                info!(files = st.files, bytes = st.bytes, ms = st.elapsed.as_millis() as u64, "scan done");
                if st.errors > 0 {
                    self.set_status(Status::Err(format!(
                        "{} items couldn't be read. Give Sapu (or your terminal) Full Disk Access for a complete scan.",
                        fmt::count(st.errors)
                    )));
                }
                Phase::Ready(Box::new(Browser::new(tree, advice, model)))
            }
            Ok(Err(e)) => {
                error!(error = %e, "scan failed");
                Phase::Failed(e.to_string())
            }
            Err(_) => Phase::Failed("The scanner crashed.".into()),
        };
    }

    fn apply(&mut self, action: Action) {
        debug!(?action, "ui action");
        if self.blocked_while_cached(&action) {
            info!(?action, "blocked: still showing the cached scan");
            self.set_status(Status::Err(
                "One moment: you're looking at your last scan. Sapu changes nothing until the fresh scan finishes."
                    .into(),
            ));
            return;
        }
        match action {
            Action::ToggleLogs => {
                self.logs.open = !self.logs.open;
                info!(open = self.logs.open, "log window");
            }
            Action::Rescan => {
                info!(root = %self.root.display(), "rescan requested");
                self.start_scan()
            }
            Action::ChooseFolder => {
                if let Some(dir) = rfd::FileDialog::new().set_directory(&self.root).pick_folder() {
                    info!(folder = %dir.display(), "folder chosen");
                    self.root = dir;
                    self.open_root();
                } else {
                    debug!("folder picker cancelled");
                }
            }
            Action::ClearCache => match sapu_core::cache::clear() {
                Ok(()) => {
                    self.refresh_safety();
                    self.set_status(Status::Ok("Scan cache cleared. The next start does a full scan.".into()));
                }
                Err(e) => self.set_status(Status::Err(format!("Couldn't clear the cache: {e}"))),
            },
            Action::RevealPath(p) => {
                let _ = std::fs::create_dir_all(&p);
                reveal_path(&p);
            }
            Action::Undo(id) => {
                info!(batch = id, "undo requested");
                let report = Journal::open().undo(id);
                info!(batch = id, restored = report.restored, failed = report.failed.len(), "undo finished");
                let msg = format!("Restored {} items from the Trash", report.restored);
                if report.failed.is_empty() {
                    self.set_status(Status::Ok(format!("{msg}. Rescanning…")));
                } else {
                    for (p, e) in &report.failed {
                        warn!(path = %p.display(), error = %e, "undo failed");
                    }
                    self.set_status(Status::Err(format!(
                        "{msg}; {} couldn't be restored automatically (use Finder → Trash → Put Back)",
                        report.failed.len()
                    )));
                }
                self.refresh_safety();
                if report.restored > 0 {
                    self.start_scan();
                }
            }
            Action::Unprotect(p) => {
                info!(path = %p.display(), "unprotect");
                let _ = safety::unprotect(&p);
                self.refresh_safety();
                self.set_status(Status::Ok(format!("{} is no longer protected", fmt::tilde(&p))));
            }
            Action::CancelJob => {
                info!("clean: stop requested");
                if let Some(j) = &self.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::StopScan => {
                if let Phase::Scanning { progress, .. } = &self.phase {
                    info!("scan stopped by user");
                    progress.cancel();
                }
            }
            other => {
                let Phase::Ready(b) = &mut self.phase else { return };
                match other {
                    Action::Select(i) => {
                        b.selected = Some(i);
                    }
                    Action::Enter(i) => b.enter(i),
                    Action::Back => b.back(),
                    Action::GoToDepth(d) => b.go_to_depth(d),
                    Action::SetSort(s) => {
                        b.sort = s;
                        b.rebuild_view();
                    }
                    Action::Reveal(i) => reveal(&b.tree.path_of(&b.idx_path(i))),
                    Action::SetMode(m) => {
                        info!(tab = ?m, "switched tab");
                        b.mode = m;
                        if m == Mode::Cloud {
                            b.cloud = dirs::home_dir().map(|h| icloud::summarize(&b.tree, &h));
                        }
                        if m == Mode::Safety {
                            self.history = Journal::open().history();
                            self.protected = safety::user_protected();
                            self.data_usage = sapu_core::paths::usage();
                        }
                    }
                    Action::Evict(idx) => {
                        let path = b.tree.path_of(&idx);
                        info!(path = %path.display(), "iCloud: remove download");
                        match icloud::evict(&path) {
                            Ok(()) => {
                                let mut freed = 0;
                                if let Some(n) = b.tree.root.get(&idx) {
                                    freed = n.size;
                                    let e = n.evicted();
                                    b.tree.root.replace(&idx, e);
                                }
                                b.rebuild_view();
                                b.refresh_advice();
                                b.cloud = dirs::home_dir().map(|h| icloud::summarize(&b.tree, &h));
                                self.set_status(Status::Ok(format!(
                                    "Removed the download of {}, freed {}. It's still in iCloud.",
                                    fmt::tilde(&path),
                                    fmt::size(freed)
                                )));
                            }
                            Err(e) => self.set_status(Status::Err(e.to_string())),
                        }
                    }
                    Action::Download(idx) => {
                        let path = b.tree.path_of(&idx);
                        info!(path = %path.display(), "iCloud: download");
                        match icloud::download(&path) {
                            Ok(()) => self.set_status(Status::Ok(format!(
                                "Downloading {} from iCloud. Rescan later to see it here.",
                                fmt::tilde(&path)
                            ))),
                            Err(e) => self.set_status(Status::Err(e.to_string())),
                        }
                    }
                    Action::ShowIdx(idx) => b.show_path(&idx),
                    Action::Protect(ci) => {
                        let path = b.tree.path_of(&b.idx_path(ci));
                        info!(path = %path.display(), "protect");
                        match safety::protect(&path) {
                            Ok(()) => {
                                b.advice.items.retain(|r| {
                                    safety::touches_protected(&r.path, std::slice::from_ref(&path)).is_none()
                                });
                                let advice = std::mem::take(&mut b.advice);
                                b.set_advice(advice);
                                b.rebuild_view();
                                self.set_status(Status::Ok(format!(
                                    "Protected {}. Sapu will never touch it or anything containing it.",
                                    fmt::tilde(&path)
                                )));
                            }
                            Err(e) => self.set_status(Status::Err(format!("Couldn't protect: {e}"))),
                        }
                    }
                    Action::ToggleCheck(i) => {
                        if let Some(c) = b.checked.get_mut(i) {
                            *c = !*c;
                        }
                    }
                    Action::CheckUpTo(limit) => {
                        for (r, c) in b.advice.items.iter().zip(b.checked.iter_mut()) {
                            *c = r.is_automatic() && limit.is_some_and(|l| r.judgement.verdict <= l);
                        }
                    }
                    Action::ShowRec(i) => {
                        if let Some(idx) = b.advice.items.get(i).map(|r| r.idx_path.clone()) {
                            b.show_path(&idx);
                        }
                    }
                    Action::RejectRec(i) => {
                        if i < b.advice.items.len() {
                            let rec = b.advice.items.remove(i);
                            info!(path = %rec.path.display(), verdict = ?rec.judgement.verdict, ai = rec.judgement.is_ai(), "recommendation rejected (Keep)");
                            b.checked.remove(i);
                            if let Some(node) = b.tree.root.get(&rec.idx_path) {
                                let _ = ml::record_feedback(&rec.path, node, Verdict::Keep, &b.judge);
                            }
                            self.status = Some((
                                Status::Ok(format!(
                                    "Got it, keeping {}. Sapu AI learns this on the next scan.",
                                    fmt::tilde(&rec.path)
                                )),
                                Instant::now(),
                            ));
                        }
                    }
                    Action::MarkKeep(ci) => {
                        let idx = b.idx_path(ci);
                        let path = b.tree.path_of(&idx);
                        info!(path = %path.display(), "marked Keep (AI feedback)");
                        if let Some(node) = b.tree.root.get(&idx) {
                            let _ = ml::record_feedback(&path, node, Verdict::Keep, &b.judge);
                        }
                        if let Some(v) = b.verdicts.get_mut(ci) {
                            *v = None;
                        }
                        b.advice.items.retain(|r| r.path != path);
                        let advice = std::mem::take(&mut b.advice);
                        b.set_advice(advice);
                        self.status = Some((
                            Status::Ok(format!(
                                "Marked {} as Keep. Sapu AI learns this on the next scan.",
                                fmt::tilde(&path)
                            )),
                            Instant::now(),
                        ));
                    }
                    Action::AskBatch => {
                        let (n, bytes) = b.selection_total();
                        if n > 0 {
                            let picked =
                                || b.advice.items.iter().zip(&b.checked).filter(|(r, c)| **c && r.is_automatic());
                            let reviewish = picked()
                                .filter(|(r, _)| r.judgement.is_ai() || r.judgement.verdict == Verdict::Review)
                                .count();
                            let risky = if reviewish > 0 {
                                Some(format!(
                                    "{reviewish} of these are Review items or AI guesses. I've looked at each one."
                                ))
                            } else if bytes > 20_000_000_000 {
                                Some(format!("This moves {} at once. I've checked the list.", fmt::size(bytes)))
                            } else if n > 100 {
                                Some(format!("This moves {n} items at once. I've checked the list."))
                            } else {
                                None
                            };
                            info!(
                                items = n,
                                bytes,
                                risky = risky.is_some(),
                                "clean requested, asking for confirmation"
                            );
                            self.confirm_batch = Some(BatchConfirm { n, bytes, risky, ack: false });
                        }
                    }
                    Action::AskTrash(i) => {
                        let idx_path = b.idx_path(i);
                        self.confirm = confirm_for(b, idx_path);
                    }
                    Action::AskTrashIdx(idx_path) => {
                        self.confirm = confirm_for(b, idx_path);
                    }
                    _ => {}
                }
            }
        }
    }

    fn do_trash(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        info!(path = %c.path.display(), bytes = c.size, "move to Trash confirmed");
        let Phase::Ready(b) = &mut self.phase else { return };
        let journal = Journal::open();
        let (label, note, consent) = if c.icloud {
            (format!("Deleted {} from iCloud", fmt::tilde(&c.path)), "iCloud delete, consent given", c.consent)
        } else {
            (format!("Moved {} to Trash", fmt::tilde(&c.path)), "picked by hand", false)
        };
        let icloud_ok = if consent { safety::ICloud::ConsentGiven } else { safety::ICloud::Refuse };
        let batch = journal.begin(&label);
        match cleaner::trash_item(&c.path, &journal, batch, c.size, note, icloud_ok) {
            Ok(_) => {
                if let Some(node) = b.tree.root.get(&c.idx_path) {
                    // You chose to delete it: a "probably safe" example for the AI.
                    let _ = ml::record_feedback(&c.path, node, Verdict::Likely, &b.judge);
                }
                b.tree.root.remove(&c.idx_path);
                b.selected = None;
                b.rebuild_view();
                b.selected = b.view.first().copied();
                b.refresh_advice();
                if b.mode == Mode::Cloud {
                    b.cloud = dirs::home_dir().map(|h| icloud::summarize(&b.tree, &h));
                }
                let msg = if c.icloud {
                    format!("Deleted {} from iCloud (it's in the Trash; Undo in 🛡 Safety)", fmt::tilde(&c.path))
                } else {
                    format!("Moved {} to the Trash, freed {}", fmt::tilde(&c.path), fmt::size(c.size))
                };
                self.set_status(Status::Ok(msg));
            }
            Err(e) => {
                warn!(path = %c.path.display(), error = %e, "trash failed");
                self.set_status(Status::Err(format!("Couldn't move to Trash: {e}")));
            }
        }
    }

    fn refresh_safety(&mut self) {
        self.history = Journal::open().history();
        self.protected = safety::user_protected();
        self.data_usage = sapu_core::paths::usage();
    }

    /// Start cleaning every ticked recommendation on a background thread.
    fn start_batch(&mut self) {
        self.confirm_batch = None;
        let Phase::Ready(b) = &mut self.phase else { return };
        let scanned_at = b.tree.stats.started_at;
        let items: Vec<(Recommendation, Expect)> = b
            .advice
            .items
            .iter()
            .zip(&b.checked)
            .filter(|(r, c)| **c && r.is_automatic())
            .filter_map(|(r, _)| {
                let node = b.tree.root.get(&r.idx_path)?;
                // Caches change all the time by design; everything else must be untouched since the scan.
                let check_mtime = r.judgement.action != sapu_core::judge::Action::TrashContents;
                Some((r.clone(), Expect::from_node(node, scanned_at, check_mtime)))
            })
            .collect();

        // Accepting an AI guess is a training example.
        for (r, _) in &items {
            if r.judgement.is_ai() {
                if let Some(node) = b.tree.root.get(&r.idx_path) {
                    let _ = ml::record_feedback(&r.path, node, r.judgement.verdict, &b.judge);
                }
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(Mutex::new(String::new()));
        let total = items.len();
        info!(items = total, "clean started");
        for (r, _) in &items {
            debug!(path = %r.path.display(), verdict = ?r.judgement.verdict, action = ?r.judgement.action, bytes = r.size, "queued");
        }
        let (c2, d2, cur2) = (cancel.clone(), done.clone(), current.clone());
        let handle = std::thread::spawn(move || {
            let journal = Journal::open();
            let batch = journal.begin(&format!("Cleaned {} recommendations", items.len()));
            let mut results = Vec::new();
            for (rec, expect) in items {
                if c2.load(Ordering::Relaxed) {
                    info!("clean cancelled by user");
                    break;
                }
                if let Ok(mut c) = cur2.lock() {
                    *c = fmt::tilde(&rec.path);
                }
                let outcome = cleaner::apply(&rec.path, &rec.judgement, Some(&expect), &journal, batch)
                    .map_err(|e| e.to_string());
                let fresh = match (&outcome, rec.judgement.action) {
                    (Ok(_), sapu_core::judge::Action::TrashContents) => {
                        sapu_core::scan(&rec.path, &ScanOptions::default(), &Progress::default()).ok().map(|t| t.root)
                    }
                    _ => None,
                };
                if let Err(e) = &outcome {
                    warn!(path = %rec.path.display(), error = %e, "recommendation skipped");
                }
                results.push(JobResult { rec, outcome, fresh });
                d2.fetch_add(1, Ordering::Relaxed);
            }
            results
        });
        self.job = Some(Job { cancel, done, total, current, handle: Some(handle) });
    }

    /// Background clean finished: update the tree in place.
    fn finish_batch(&mut self, results: Vec<JobResult>, cancelled: bool, total: usize) {
        let Phase::Ready(b) = &mut self.phase else { return };
        let mut removals: Vec<Vec<usize>> = Vec::new();
        let (mut done, mut freed, mut skipped) = (0usize, 0u64, 0usize);
        let mut first_skip: Option<String> = None;

        for r in results {
            match r.outcome {
                Ok(res) => {
                    skipped += res.failed.len();
                    if r.rec.judgement.action == sapu_core::judge::Action::Evict {
                        // Still in iCloud, just not on disk any more.
                        if let Some(n) = b.tree.root.get(&r.rec.idx_path) {
                            freed += n.size;
                            let e = n.evicted();
                            b.tree.root.replace(&r.rec.idx_path, e);
                        }
                    } else if let Some(fresh) = r.fresh {
                        freed += r.rec.size.saturating_sub(fresh.size);
                        b.tree.root.replace(&r.rec.idx_path, fresh);
                    } else {
                        freed += r.rec.size;
                        removals.push(r.rec.idx_path.clone());
                    }
                    done += 1;
                }
                Err(e) => {
                    skipped += 1;
                    first_skip.get_or_insert(format!("{}: {e}", fmt::tilde(&r.rec.path)));
                }
            }
        }
        // Remove from the back so earlier index paths stay valid.
        removals.sort_unstable_by(|a, b| b.cmp(a));
        for p in &removals {
            b.tree.root.remove(p);
        }

        b.path.clear();
        b.history.clear();
        b.selected = None;
        b.rebuild_view();
        b.selected = b.view.first().copied();
        b.refresh_advice();

        info!(done, freed, skipped, cancelled, "batch clean finished");
        let mut msg = format!("Cleaned {done} items, freed {}", fmt::size(freed));
        if cancelled {
            msg.push_str(&format!(" (stopped; {} not started)", total.saturating_sub(done + skipped)));
        }
        if skipped > 0 {
            msg.push_str(&format!(" · {skipped} skipped"));
            if let Some(f) = first_skip {
                msg.push_str(&format!(", e.g. {f}"));
            }
        }
        msg.push_str(" · Undo in 🛡 Safety");
        self.set_status(if skipped > 0 && done == 0 { Status::Err(msg) } else { Status::Ok(msg) });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        if !job.handle.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        let results = job.handle.take().and_then(|h| h.join().ok()).unwrap_or_default();
        let cancelled = job.cancel.load(Ordering::Relaxed);
        let total = job.total;
        self.job = None;
        self.finish_batch(results, cancelled, total);
    }

    fn keyboard(&self, ctx: &egui::Context) -> Vec<Action> {
        let mut out = Vec::new();
        if self.confirm.is_some() || self.job.is_some() {
            return out;
        }
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            if cmd && i.key_pressed(Key::R) {
                out.push(Action::Rescan);
            }
            if cmd && i.key_pressed(Key::O) {
                out.push(Action::ChooseFolder);
            }
            if cmd && i.key_pressed(Key::L) {
                out.push(Action::ToggleLogs);
            }
            if i.key_pressed(Key::Escape) {
                out.push(Action::StopScan);
            }
        });
        // Up/Down/PageUp/PageDown need the browser mutably; handled in `ui`.
        let Phase::Ready(b) = &self.phase else { return out };
        if b.mode != Mode::Browse || self.confirm_batch.is_some() {
            return out;
        }
        ctx.input(|i| {
            if let Some(sel) = b.selected {
                if i.key_pressed(Key::Enter) || i.key_pressed(Key::ArrowRight) {
                    out.push(Action::Enter(sel));
                }
                if i.key_pressed(Key::Delete) || (i.modifiers.command && i.key_pressed(Key::Backspace)) {
                    out.push(Action::AskTrash(sel));
                }
                if i.modifiers.command && i.key_pressed(Key::Enter) {
                    out.push(Action::Reveal(sel));
                }
            }
            if !i.modifiers.command && (i.key_pressed(Key::Backspace) || i.key_pressed(Key::ArrowLeft)) {
                out.push(Action::Back);
            }
        });
        out
    }
}

impl eframe::App for SapuApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if theme::glass_active() {
            [0.0; 4] // let the macOS blur show through
        } else {
            pal().bg.to_normalized_gamma_f32()
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let dark = ctx.theme() == egui::Theme::Dark;
        theme::set_dark(dark);
        if self.last_dark != Some(dark) {
            info!(theme = if dark { "dark" } else { "light" }, glass = theme::glass_active(), "appearance");
            self.last_dark = Some(dark);
        }
        self.poll_scan();
        self.poll_refresh();
        self.poll_job();
        if matches!(self.phase, Phase::Scanning { .. }) || self.job.is_some() || self.refresh.is_some() {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
        if self.status.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(8)) {
            self.status = None;
        }

        let mut actions = self.keyboard(ctx);
        if let Phase::Ready(b) = &mut self.phase {
            if self.confirm.is_none() && self.confirm_batch.is_none() && b.mode == Mode::Browse {
                ctx.input(|i| {
                    if i.key_pressed(Key::ArrowDown) {
                        b.move_selection(1);
                    }
                    if i.key_pressed(Key::ArrowUp) {
                        b.move_selection(-1);
                    }
                    if i.key_pressed(Key::PageDown) {
                        b.move_selection(15);
                    }
                    if i.key_pressed(Key::PageUp) {
                        b.move_selection(-15);
                    }
                });
            }
        }

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(pal().panel).inner_margin(egui::Margin {
                left: TITLEBAR_INSET,
                right: 16,
                top: 10,
                bottom: 10,
            }))
            .show(ui, |ui| top_bar(ui, &self.root, &self.phase, self.logs.counts(), &mut actions));

        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(pal().panel).inner_margin(egui::Margin::symmetric(16, 7)))
            .show(ui, |ui| {
                let refreshing = self.refresh.as_ref().map(|(p, _, _)| {
                    let cached = match &self.phase {
                        Phase::Ready(b) => b.cached_at.map(fmt::ago),
                        _ => None,
                    };
                    let s = p.snapshot();
                    match cached {
                        Some(ago) => format!(
                            "Showing your last scan ({ago}). Refreshing… {} files so far. Cleaning unlocks when it's done.",
                            fmt::count(s.files)
                        ),
                        None => format!("Refreshing… {} files so far", fmt::count(s.files)),
                    }
                });
                status_bar(ui, &self.phase, self.status.as_ref(), refreshing, self.log_file.as_deref())
            });

        match &mut self.phase {
            Phase::Scanning { progress, started, .. } => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(pal().bg))
                    .show(ui, |ui| scanning_view(ui, &self.root, progress, *started, &mut actions));
            }
            Phase::Failed(msg) => {
                let msg = msg.clone();
                egui::CentralPanel::default().frame(egui::Frame::new().fill(pal().bg)).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.35);
                        ui.label(serif("Scan failed", 24.0).color(pal().bad));
                        ui.label(RichText::new(msg).color(pal().dim));
                        ui.add_space(10.0);
                        if ui.button("Try again").clicked() {
                            actions.push(Action::Rescan);
                        }
                    });
                });
            }
            Phase::Ready(b) if b.mode == Mode::Cloud => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::symmetric(18, 14)))
                    .show(ui, |ui| cloud_view(ui, b, &mut actions));
            }
            Phase::Ready(b) if b.mode == Mode::Safety => {
                let (history, protected) = (&self.history, &self.protected);
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::symmetric(16, 12)))
                    .show(ui, |ui| safety_view(ui, history, protected, &self.data_usage, &mut actions));
            }
            Phase::Ready(b) if b.mode == Mode::Advice => {
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::symmetric(16, 12)))
                    .show(ui, |ui| advice_view(ui, b, &mut actions));
            }
            Phase::Ready(b) => {
                egui::Panel::left("list")
                    .resizable(true)
                    .default_size(560.0)
                    .size_range(380.0..=900.0)
                    .frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::same(8)))
                    .show(ui, |ui| list_view(ui, b, &mut actions));
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(pal().bg).inner_margin(egui::Margin::same(8)))
                    .show(ui, |ui| map_view(ui, b, &mut actions));
            }
        }

        if let Some(c) = &mut self.confirm {
            match confirm_dialog(ctx, c) {
                Some(true) => self.do_trash(),
                Some(false) => self.confirm = None,
                None => {}
            }
        }

        if let Some(bc) = &mut self.confirm_batch {
            match batch_dialog(ctx, bc) {
                Some(true) => self.start_batch(),
                Some(false) => self.confirm_batch = None,
                None => {}
            }
        }
        if let Some(job) = &self.job {
            job_dialog(ctx, job, &mut actions);
        }

        self.logs.show(ctx);

        for a in actions {
            self.apply(a);
        }
    }
}

// ───────────────────────────── views ─────────────────────────────

/// Segmented control with a glass "pill" that slides to the selected tab.
fn tabs(ui: &mut Ui, b: &Browser, actions: &mut Vec<Action>) {
    let p = pal();
    let reclaim = b.advice.total(Verdict::Safe) + b.advice.total(Verdict::Likely);
    let items: Vec<(Mode, String)> = vec![
        (Mode::Browse, "Browse".into()),
        (Mode::Advice, format!("✨ Recommendations · {}", fmt::size(reclaim))),
        (Mode::Cloud, "☁ iCloud".into()),
        (Mode::Safety, "🛡 Safety".into()),
    ];
    let font = FontId::proportional(13.0);
    let pad = Vec2::new(14.0, 6.0);
    let galleys: Vec<_> =
        items.iter().map(|(_, t)| ui.painter().layout_no_wrap(t.clone(), font.clone(), p.text)).collect();
    let total_w: f32 = galleys.iter().map(|g| g.size().x + pad.x * 2.0).sum::<f32>() + 6.0;
    let h = 30.0;
    let (outer, _) = ui.allocate_exact_size(Vec2::new(total_w, h), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(outer, CornerRadius::same(15), p.track);

    let mut x = outer.left() + 3.0;
    let mut rects = Vec::new();
    for g in &galleys {
        let w = g.size().x + pad.x * 2.0;
        rects.push(Rect::from_min_size(egui::pos2(x, outer.top() + 3.0), Vec2::new(w, h - 6.0)));
        x += w;
    }
    let sel = items.iter().position(|(m, _)| *m == b.mode).unwrap_or(0);
    let id = ui.make_persistent_id("tab-pill");
    let ax = ui.ctx().animate_value_with_time(id.with("x"), rects[sel].left(), 0.28);
    let aw = ui.ctx().animate_value_with_time(id.with("w"), rects[sel].width(), 0.28);
    let pill = Rect::from_min_size(egui::pos2(ax, rects[sel].top()), Vec2::new(aw, h - 6.0));
    let shadow = egui::epaint::Shadow { offset: [0, 2], blur: 8, spread: 0, color: p.shadow };
    painter.add(shadow.as_shape(pill, CornerRadius::same(12)));
    painter.rect_filled(pill, CornerRadius::same(12), p.surface_hi);
    painter.add(theme::sheen_shape(pill, 12.0, p.sheen, p.rim));

    for (i, ((mode, _), g)) in items.iter().zip(galleys).enumerate() {
        let r = rects[i];
        let resp = ui.interact(r, id.with(i), Sense::click());
        let col = if i == sel {
            p.text
        } else if resp.hovered() {
            p.text2
        } else if *mode == Mode::Advice {
            p.ok
        } else {
            p.dim
        };
        let pos = r.center() - g.size() / 2.0;
        ui.painter().galley_with_override_text_color(pos, g, col);
        if resp.clicked() {
            actions.push(Action::SetMode(*mode));
        }
    }
}

/// "📜 Logs" button; turns amber/red when warnings/errors were logged.
fn logs_button(ui: &mut Ui, counts: (usize, usize), actions: &mut Vec<Action>) {
    let (errors, warnings) = counts;
    let (label, color) = if errors > 0 {
        (format!("📜 Logs · {errors}"), pal().bad)
    } else if warnings > 0 {
        (format!("📜 Logs · {warnings}"), pal().warn)
    } else {
        ("📜 Logs".to_string(), pal().text)
    };
    if ui
        .button(RichText::new(label).color(color))
        .on_hover_text(format!("Open the log window (⌘L) · {errors} errors, {warnings} warnings"))
        .clicked()
    {
        actions.push(Action::ToggleLogs);
    }
}

fn top_bar(ui: &mut Ui, root: &Path, phase: &Phase, log_counts: (usize, usize), actions: &mut Vec<Action>) {
    // The bar doubles as the window's title bar: drag it to move the window.
    let drag = ui.interact(ui.max_rect(), ui.make_persistent_id("drag-window"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }

    ui.horizontal(|ui| {
        ui.label(serif("Sapu", 21.0).color(pal().accent));
        ui.add_space(14.0);
        if let Phase::Ready(b) = phase {
            tabs(ui, b, actions);
            ui.add_space(12.0);
            if b.mode != Mode::Browse {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("⟳  Rescan").on_hover_text("⌘R · also retrains Sapu AI").clicked() {
                        actions.push(Action::Rescan);
                    }
                    logs_button(ui, log_counts, actions);
                });
                return;
            }
        }

        if let Phase::Ready(b) = phase {
            let back = ui.add_enabled(!b.path.is_empty(), egui::Button::new("⬅"));
            if back.on_hover_text("Back (⌫)").clicked() {
                actions.push(Action::Back);
            }
            // Breadcrumb
            let root_label = fmt::tilde(&b.tree.root_path);
            if ui.add(egui::Button::new(RichText::new(root_label).strong()).frame(false)).clicked() {
                actions.push(Action::GoToDepth(0));
            }
            let mut node = &b.tree.root;
            for (depth, &i) in b.path.iter().enumerate() {
                node = &node.children[i];
                ui.label(RichText::new("›").color(pal().dim));
                let last = depth + 1 == b.path.len();
                let text = RichText::new(&*node.name);
                let text = if last { text.strong().color(pal().accent) } else { text };
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    actions.push(Action::GoToDepth(depth + 1));
                }
            }
        } else {
            ui.label(RichText::new(fmt::tilde(root)).strong());
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            logs_button(ui, log_counts, actions);
            if ui.button("⟳ Rescan").on_hover_text("⌘R").clicked() {
                actions.push(Action::Rescan);
            }
            if ui.button("📂 Open Folder…").on_hover_text("⌘O").clicked() {
                actions.push(Action::ChooseFolder);
            }
            if let Phase::Ready(b) = phase {
                ui.add_space(8.0);
                let mut sort = b.sort;
                egui::ComboBox::from_id_salt("sort").selected_text(format!("Sort: {}", sort.label())).show_ui(
                    ui,
                    |ui| {
                        for s in [SortBy::Size, SortBy::Name, SortBy::Items] {
                            ui.selectable_value(&mut sort, s, s.label());
                        }
                    },
                );
                if sort != b.sort {
                    actions.push(Action::SetSort(sort));
                }
            }
        });
    });

    if let Phase::Ready(b) = phase {
        if b.mode != Mode::Browse {
            return;
        }
        let node = b.current();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(serif(fmt::size(node.size), 30.0));
            ui.label(RichText::new(format!("{} items in this folder", fmt::count(node.files))).color(pal().dim));
            if node.incomplete {
                ui.label(RichText::new("⚠ some items unreadable").color(pal().warn));
            }
            if b.tree.stats.cancelled {
                ui.label(RichText::new("⏸ partial scan").color(pal().warn));
            }
        });
    }
}

fn status_bar(
    ui: &mut Ui,
    phase: &Phase,
    status: Option<&(Status, Instant)>,
    refreshing: Option<String>,
    log: Option<&Path>,
) {
    if let (Some(msg), None) = (&refreshing, status) {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0).color(pal().accent));
            ui.label(RichText::new(msg).small().color(pal().text2));
        });
        return;
    }
    // Messages slide in and fade out.
    let fade = status.map(|(_, at)| {
        let age = at.elapsed().as_secs_f32();
        let fin = theme::ease_out(age / 0.3);
        let fout = 1.0 - ((age - 7.4) / 0.6).clamp(0.0, 1.0);
        if fin < 1.0 || fout < 1.0 {
            ui.ctx().request_repaint();
        }
        (fin * fout, (1.0 - fin) * 10.0)
    });
    ui.horizontal(|ui| {
        match status {
            Some((Status::Ok(m), _)) => {
                let (a, dx) = fade.unwrap_or((1.0, 0.0));
                ui.add_space(dx);
                ui.label(RichText::new(format!("✓ {m}")).color(pal().ok.gamma_multiply(a)));
            }
            Some((Status::Err(m), _)) => {
                let (a, dx) = fade.unwrap_or((1.0, 0.0));
                ui.add_space(dx);
                ui.label(RichText::new(format!("⚠ {m}")).color(pal().warn.gamma_multiply(a)));
            }
            None => {
                let hint =
                    "Double-click to open · ⌫ back · ⌘⌫ move to Trash · ⌘⏎ reveal in Finder · right-click for more";
                ui.label(RichText::new(hint).color(pal().dim).small());
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Phase::Ready(b) = phase {
                let st = &b.tree.stats;
                ui.label(
                    RichText::new(format!(
                        "{} files · {} folders · {:.1}s",
                        fmt::count(st.files),
                        fmt::count(st.dirs),
                        st.elapsed.as_secs_f64()
                    ))
                    .color(pal().dim)
                    .small(),
                );
            }
            if let Some(p) = log {
                ui.label(RichText::new("log").color(pal().dim).small()).on_hover_text(fmt::tilde(p));
            }
        });
    });
}

fn scanning_view(ui: &mut Ui, root: &Path, p: &Progress, started: Instant, actions: &mut Vec<Action>) {
    let s = p.snapshot();
    let secs = started.elapsed().as_secs_f64().max(0.001);
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(20.0));
        glass(ui, 22, egui::Margin::same(30), None, |ui| {
            ui.set_width(460.0);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(22.0).color(pal().accent));
                    ui.label(serif(format!("Scanning {}", fmt::tilde(root)), 20.0));
                });
                ui.add_space(14.0);
                egui::Grid::new("scan-stats").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
                    let row = |ui: &mut Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).color(pal().dim));
                        ui.label(RichText::new(v).strong());
                        ui.end_row();
                    };
                    row(ui, "Found", fmt::size(s.bytes));
                    row(ui, "Files", fmt::count(s.files));
                    row(ui, "Folders", fmt::count(s.dirs));
                    row(ui, "Speed", format!("{} files/s", fmt::count((s.files as f64 / secs) as u64)));
                    row(ui, "Time", format!("{secs:.1}s"));
                });
                ui.add_space(10.0);
                let cur = fmt::truncate_left(&fmt::tilde(&p.current_path()), 58);
                ui.add(egui::Label::new(RichText::new(cur).color(pal().dim).small().monospace()).truncate());
                ui.add_space(14.0);
                if ui.button("Stop and show results").on_hover_text("Esc").clicked() {
                    actions.push(Action::StopScan);
                }
            });
        });
    });
}

fn bar_color(frac: f64) -> Color32 {
    match frac {
        x if x >= 0.25 => pal().bad,
        x if x >= 0.10 => pal().accent,
        x if x >= 0.02 => pal().warn,
        _ => pal().ok,
    }
}

fn icon(n: &Node) -> &'static str {
    match n.kind {
        Kind::Dir => "📁",
        Kind::File => "📄",
        Kind::Symlink => "🔗",
        Kind::Other => "•",
    }
}

fn list_view(ui: &mut Ui, b: &mut Browser, actions: &mut Vec<Action>) {
    let node = b.tree.root.get(&b.path).unwrap_or(&b.tree.root);
    let total = node.size;

    // Column header
    let header_font = FontId::proportional(11.0);
    let (hrect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    let cols = Columns::new(hrect);
    let p = ui.painter();
    p.text(cols.size_right(hrect), Align2::RIGHT_CENTER, "SIZE", header_font.clone(), pal().dim);
    p.text(cols.name_left(hrect), Align2::LEFT_CENTER, "NAME", header_font.clone(), pal().dim);
    p.text(cols.items_right(hrect), Align2::RIGHT_CENTER, "ITEMS", header_font, pal().dim);
    ui.separator();

    if node.children.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(RichText::new("This folder is empty").color(pal().dim));
        });
        return;
    }

    let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
    if b.scroll_to_selection {
        if let Some(pos) = b.selected.and_then(|s| b.view.iter().position(|&v| v == s)) {
            let viewport = ui.available_height();
            let target = (pos as f32 * (ROW_H + ui.spacing().item_spacing.y) - viewport / 2.0).max(0.0);
            scroll = scroll.vertical_scroll_offset(target);
        }
        b.scroll_to_selection = false;
    }

    let view = &b.view;
    let selected = b.selected;
    let b = &*b;
    // Bars grow in when a folder opens.
    let grow = theme::ease_out(b.anim_at.elapsed().as_secs_f32() / 0.5);
    if grow < 1.0 {
        ui.ctx().request_repaint();
    }
    scroll.show_rows(ui, ROW_H, view.len(), |ui, range| {
        for row in range {
            let ci = view[row];
            let c = &node.children[ci];
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
            let p = ui.painter();

            let is_sel = selected == Some(ci);
            // Smooth hover / selection fades.
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.14);
            let sel = ui.ctx().animate_bool_with_time(resp.id.with("sel"), is_sel, 0.18);
            if hov > 0.0 {
                p.rect_filled(rect, CornerRadius::same(9), pal().row_hover.gamma_multiply(hov));
            }
            if sel > 0.0 {
                p.rect_filled(rect, CornerRadius::same(9), pal().row_sel.gamma_multiply(sel));
            }

            let cols = Columns::new(rect);
            let frac = fmt::percent(c.size, total);
            let font = FontId::proportional(13.0);

            if icloud::is_cloud_only(c) {
                // Only in iCloud: show its real size, dimmed, with a cloud.
                p.text(
                    cols.size_right(rect),
                    Align2::RIGHT_CENTER,
                    format!("☁ {}", fmt::size(c.logical)),
                    font.clone(),
                    pal().dim,
                );
            } else {
                p.text(cols.size_right(rect), Align2::RIGHT_CENTER, fmt::size(c.size), font.clone(), pal().text);
            }

            // Usage bar
            let track = cols.bar_rect(rect);
            p.rect_filled(track, CornerRadius::same(4), pal().track);
            let mut fill = track;
            fill.set_width((track.width() * frac as f32 * grow).max(if c.size > 0 { 1.5 } else { 0.0 }));
            p.rect_filled(fill, CornerRadius::same(3), bar_color(frac));
            p.text(
                cols.pct_right(rect),
                Align2::RIGHT_CENTER,
                format!("{:.1}%", frac * 100.0),
                FontId::proportional(11.5),
                pal().dim,
            );

            // Verdict badge
            if let Some(Some(jd)) = b.verdicts.get(ci) {
                let pr = cols.pill_rect(rect);
                paint_pill(p, pr, jd);
                ui.interact(pr, ui.make_persistent_id(("pill", ci)), Sense::hover()).on_hover_text_at_pointer(format!(
                    "{} · {}\n{}",
                    jd.verdict.label(),
                    jd.category.label(),
                    jd.reason
                ));
            }

            // Name (clipped to its column)
            let name_color = match c.kind {
                Kind::Dir => pal().dir,
                Kind::Symlink => pal().link,
                _ => pal().text,
            };
            let warn = if c.incomplete { "  ⚠" } else { "" };
            p.with_clip_rect(cols.name_clip(rect)).text(
                cols.name_left(rect),
                Align2::LEFT_CENTER,
                format!(
                    "{} {}{}{}",
                    icon(c),
                    c.name,
                    warn,
                    if icloud::is_cloud_only(c) { "  ☁ in iCloud" } else { "" }
                ),
                font,
                name_color,
            );

            if c.is_dir() {
                p.text(
                    cols.items_right(rect),
                    Align2::RIGHT_CENTER,
                    fmt::count(c.files),
                    FontId::proportional(11.5),
                    pal().dim,
                );
            }

            if resp.clicked() {
                actions.push(Action::Select(ci));
            }
            if resp.double_clicked() {
                actions.push(Action::Enter(ci));
            }
            resp.context_menu(|ui| {
                if c.is_dir() && ui.button("Open").clicked() {
                    actions.push(Action::Enter(ci));
                    ui.close();
                }
                if ui.button("Reveal in Finder").clicked() {
                    actions.push(Action::Reveal(ci));
                    ui.close();
                }
                let full = b.tree.path_of(&b.idx_path(ci));
                if icloud::in_icloud(&full, &b.icloud_roots) {
                    ui.separator();
                    if c.size > 0 && ui.button("☁ Remove download (keep in iCloud)").clicked() {
                        actions.push(Action::Evict(b.idx_path(ci)));
                        ui.close();
                    }
                    if icloud::is_cloud_only(c) && ui.button("☁ Download now").clicked() {
                        actions.push(Action::Download(b.idx_path(ci)));
                        ui.close();
                    }
                }
                if ui.button("Mark as Keep (teach Sapu AI)").clicked() {
                    actions.push(Action::MarkKeep(ci));
                    ui.close();
                }
                if ui.button("🛡 Protect (never touch)").clicked() {
                    actions.push(Action::Protect(ci));
                    ui.close();
                }
                ui.separator();
                let trash_label = if icloud::in_icloud(&full, &b.icloud_roots) {
                    "Delete from iCloud…"
                } else {
                    "Move to Trash…"
                };
                if ui.button(RichText::new(trash_label).color(pal().bad)).clicked() {
                    actions.push(Action::AskTrash(ci));
                    ui.close();
                }
            });
        }
    });
}

/// Column geometry for list rows.
struct Columns {
    left: f32,
    right: f32,
}

impl Columns {
    const SIZE_W: f32 = 84.0;
    const BAR_W: f32 = 90.0;
    const PCT_W: f32 = 46.0;
    const ITEMS_W: f32 = 70.0;
    const PILL_W: f32 = 54.0;
    const GAP: f32 = 10.0;

    fn new(rect: Rect) -> Self {
        Self { left: rect.left() + 8.0, right: rect.right() - 10.0 }
    }
    fn size_right(&self, r: Rect) -> egui::Pos2 {
        egui::pos2(self.left + Self::SIZE_W, r.center().y)
    }
    fn bar_rect(&self, r: Rect) -> Rect {
        let x = self.left + Self::SIZE_W + Self::GAP;
        Rect::from_min_size(egui::pos2(x, r.center().y - 4.0), Vec2::new(Self::BAR_W, 8.0))
    }
    fn pct_right(&self, r: Rect) -> egui::Pos2 {
        egui::pos2(self.left + Self::SIZE_W + Self::GAP + Self::BAR_W + Self::GAP + Self::PCT_W, r.center().y)
    }
    fn pill_rect(&self, r: Rect) -> Rect {
        let x = self.pct_right(r).x + Self::GAP;
        Rect::from_min_size(egui::pos2(x, r.center().y - 9.0), Vec2::new(Self::PILL_W, 18.0))
    }
    fn name_left(&self, r: Rect) -> egui::Pos2 {
        egui::pos2(self.pct_right(r).x + Self::GAP + Self::PILL_W + Self::GAP, r.center().y)
    }
    fn name_clip(&self, r: Rect) -> Rect {
        Rect::from_min_max(
            egui::pos2(self.name_left(r).x, r.top()),
            egui::pos2(self.right - Self::ITEMS_W - Self::GAP, r.bottom()),
        )
    }
    fn items_right(&self, r: Rect) -> egui::Pos2 {
        egui::pos2(self.right, r.center().y)
    }
}

/// One tile to draw: either a real child or the "everything small" bucket.
struct Tile {
    child: Option<usize>,
    size: u64,
}

/// Pick children big enough to see; lump the rest together.
fn tiles_for(node: &Node, max: usize) -> Vec<Tile> {
    let min = (node.size as f64 * 0.002) as u64;
    let mut tiles = Vec::new();
    let mut rest = 0u64;
    for (i, c) in node.children.iter().enumerate() {
        if c.size == 0 {
            continue;
        }
        if tiles.len() < max && c.size >= min {
            tiles.push(Tile { child: Some(i), size: c.size });
        } else {
            rest += c.size;
        }
    }
    if rest > 0 {
        tiles.push(Tile { child: None, size: rest });
        // keep largest-first for the layout
        tiles.sort_by_key(|t| std::cmp::Reverse(t.size));
    }
    tiles
}

fn map_view(ui: &mut Ui, b: &Browser, actions: &mut Vec<Action>) {
    let area = ui.available_rect_before_wrap();
    map_tiles(ui, b, actions);
    // Tiles fade in when a folder opens.
    let t = theme::ease_out(b.anim_at.elapsed().as_secs_f32() / 0.45);
    if t < 1.0 {
        ui.painter().rect_filled(area.expand(1.0), CornerRadius::ZERO, pal().bg.gamma_multiply(1.0 - t));
        ui.ctx().request_repaint();
    }
}

fn map_tiles(ui: &mut Ui, b: &Browser, actions: &mut Vec<Action>) {
    let node = b.current();
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    if node.size == 0 {
        return;
    }

    let tiles = tiles_for(node, 250);
    let values: Vec<f64> = tiles.iter().map(|t| t.size as f64).collect();
    let rects = treemap::squarify(&values, area);

    for (k, (tile, rect)) in tiles.iter().zip(rects).enumerate() {
        let rect = rect.shrink(1.5);
        if rect.width() < 1.0 || rect.height() < 1.0 {
            continue;
        }
        let Some(ci) = tile.child else {
            ui.painter().rect_filled(rect, CornerRadius::same(6), pal().other_tile);
            let resp = ui.interact(rect, ui.make_persistent_id(("other", k)), Sense::hover());
            resp.on_hover_text_at_pointer(format!("Smaller items\n{}", fmt::size(tile.size)));
            continue;
        };

        let c = &node.children[ci];
        let base = if c.is_dir() { pal().tiles[k % pal().tiles.len()] } else { pal().file_tile };
        let p = ui.painter().clone();
        let resp = ui.interact(rect, ui.make_persistent_id(("tile", ci)), Sense::click());

        // Folder tiles show one level of their contents inside.
        let header_h = 18.0;
        let nested = c.is_dir() && rect.width() > 70.0 && rect.height() > 50.0 && !c.children.is_empty();
        if nested {
            p.rect_filled(rect, CornerRadius::same(8), base.gamma_multiply(0.55));
            let inner = Rect::from_min_max(rect.min + Vec2::new(2.0, header_h), rect.max - Vec2::new(2.0, 2.0));
            let sub = tiles_for(c, 60);
            let sub_vals: Vec<f64> = sub.iter().map(|t| t.size as f64).collect();
            for (j, (st, sr)) in sub.iter().zip(treemap::squarify(&sub_vals, inner)).enumerate() {
                let sr = sr.shrink(0.5);
                let col = match st.child {
                    None => pal().other_tile,
                    Some(_) if j % 2 == 0 => base,
                    Some(_) => base.gamma_multiply(0.85),
                };
                p.rect_filled(sr, CornerRadius::same(2), col);
                if let Some(si) = st.child {
                    let sc = &c.children[si];
                    if sr.width() > 64.0 && sr.height() > 18.0 {
                        tile_label(&p, sr, sc, false);
                    }
                    if sr.contains(ui.ctx().pointer_hover_pos().unwrap_or_default()) {
                        p.rect_stroke(sr, CornerRadius::same(2), Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
                    }
                }
            }
            // Header with the folder name
            let hdr = Rect::from_min_size(rect.min, Vec2::new(rect.width(), header_h));
            p.with_clip_rect(hdr.shrink2(Vec2::new(5.0, 0.0))).text(
                hdr.left_center() + Vec2::new(5.0, 0.0),
                Align2::LEFT_CENTER,
                format!("{}  {}", c.name, fmt::size(c.size)),
                FontId::proportional(11.5),
                Color32::WHITE,
            );
        } else {
            p.rect_filled(rect, CornerRadius::same(8), base);
            p.add(theme::sheen_shape(rect, 8.0, Color32::from_white_alpha(40), Color32::from_white_alpha(70)));
            if rect.width() > 56.0 && rect.height() > 18.0 {
                tile_label(&p, rect, c, true);
            }
        }

        if b.selected == Some(ci) {
            p.rect_stroke(rect, CornerRadius::same(8), Stroke::new(2.5, Color32::WHITE), StrokeKind::Inside);
        } else if resp.hovered() {
            p.rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.5, Color32::from_white_alpha(180)),
                StrokeKind::Inside,
            );
        }

        let pct = fmt::percent(c.size, node.size) * 100.0;
        let verdict = match b.verdicts.get(ci) {
            Some(Some(jd)) => format!("\n{}: {}", jd.verdict.label(), jd.reason),
            _ => String::new(),
        };
        let resp = resp.on_hover_text_at_pointer(format!(
            "{} {}\n{} · {:.1}%{}{}",
            icon(c),
            c.name,
            fmt::size(c.size),
            pct,
            if c.is_dir() { format!("\n{} items · double-click to open", fmt::count(c.files)) } else { String::new() },
            verdict
        ));
        if resp.clicked() {
            actions.push(Action::Select(ci));
        }
        if resp.double_clicked() {
            actions.push(Action::Enter(ci));
        }
        resp.context_menu(|ui| {
            if c.is_dir() && ui.button("Open").clicked() {
                actions.push(Action::Enter(ci));
                ui.close();
            }
            if ui.button("Reveal in Finder").clicked() {
                actions.push(Action::Reveal(ci));
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Move to Trash…").color(pal().bad)).clicked() {
                actions.push(Action::AskTrash(ci));
                ui.close();
            }
        });
    }
}

fn tile_label(p: &egui::Painter, rect: Rect, n: &Node, big: bool) {
    let clip = p.with_clip_rect(rect.shrink(3.0));
    let ink = pal().tile_ink;
    let name_font = FontId::proportional(if big { 13.0 } else { 11.0 });
    clip.text(rect.left_top() + Vec2::new(5.0, 4.0), Align2::LEFT_TOP, &*n.name, name_font, ink);
    if rect.height() > 36.0 {
        clip.text(
            rect.left_top() + Vec2::new(5.0, if big { 20.0 } else { 17.0 }),
            Align2::LEFT_TOP,
            fmt::size(n.size),
            FontId::proportional(11.0),
            ink.gamma_multiply(0.8),
        );
    }
}

/// Verdict badge. AI guesses get a purple outline and an "AI" prefix.
fn paint_pill(p: &egui::Painter, rect: Rect, jd: &Judgement) {
    let c = verdict_color(jd.verdict);
    let border = if jd.is_ai() { pal().ai } else { c.gamma_multiply(0.7) };
    p.rect_filled(rect, CornerRadius::same(9), c.gamma_multiply(0.22));
    p.rect_stroke(rect, CornerRadius::same(9), Stroke::new(1.0, border), StrokeKind::Inside);
    let text = if jd.is_ai() { format!("AI·{}", jd.verdict.short()) } else { jd.verdict.short().to_string() };
    let size = if jd.is_ai() { 10.0 } else { 11.0 };
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(size), c);
}

fn verdict_blurb(v: Verdict) -> &'static str {
    match v {
        Verdict::Safe => "Rebuilt or re-downloaded automatically. Nothing is lost.",
        Verdict::Likely => "Almost certainly not needed any more.",
        Verdict::Review => "Could free a lot of space, but only you can decide. Not ticked by default.",
        Verdict::Keep => "",
    }
}

fn advice_view(ui: &mut Ui, b: &Browser, actions: &mut Vec<Action>) {
    let adv = &b.advice;

    // Summary cards
    ui.horizontal(|ui| {
        for v in [Verdict::Safe, Verdict::Likely, Verdict::Review] {
            let n = adv.items.iter().filter(|r| r.judgement.verdict == v).count();
            glass(ui, 18, egui::Margin::symmetric(18, 14), None, |ui| {
                ui.set_min_width(200.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new(v.label()).color(verdict_color(v)).strong());
                    ui.label(serif(fmt::size(adv.total(v)), 28.0));
                    ui.label(RichText::new(format!("{n} items")).color(pal().dim).small());
                });
            });
        }
    });
    ui.add_space(8.0);
    match &b.model {
        Some(m) => {
            let mt = &m.metrics;
            let ai_n = adv.items.iter().filter(|r| r.judgement.is_ai()).count();
            ui.label(
                RichText::new(format!(
                    "🧠 Sapu AI · trained on this Mac in {} ms · {} examples ({} from your choices) · {:.0}% right on items it hadn't seen · {} guesses below, never ticked automatically",
                    mt.train_ms,
                    fmt::count(mt.examples as u64),
                    mt.from_feedback,
                    mt.holdout_accuracy * 100.0,
                    ai_n
                ))
                .small()
                .color(pal().ai),
            );
        }
        None => {
            ui.label(RichText::new("🧠 Sapu AI is off").small().color(pal().dim));
        }
    }
    ui.add_space(8.0);

    // Selection bar
    let (n, bytes) = b.selection_total();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{n} selected · {}", fmt::size(bytes))).strong());
        ui.add_space(8.0);
        if ui.small_button("Select safe").clicked() {
            actions.push(Action::CheckUpTo(Some(Verdict::Safe)));
        }
        if ui.small_button("Safe + likely").clicked() {
            actions.push(Action::CheckUpTo(Some(Verdict::Likely)));
        }
        if ui.small_button("None").clicked() {
            actions.push(Action::CheckUpTo(None));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let btn = egui::Button::new(RichText::new(format!("Move {n} to Trash")).color(Color32::WHITE).strong())
                .fill(pal().bad);
            let fresh = b.cached_at.is_none();
            if ui
                .add_enabled(n > 0 && fresh, btn)
                .on_disabled_hover_text(if fresh {
                    "Tick something first"
                } else {
                    "Waiting for the fresh scan to finish"
                })
                .clicked()
            {
                actions.push(Action::AskBatch);
            }
        });
    });
    ui.add_space(6.0);
    ui.separator();

    if adv.items.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(RichText::new("Nothing to clean here. Nice and tidy ✨").size(16.0).color(pal().dim));
        });
        return;
    }

    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for v in [Verdict::Safe, Verdict::Likely, Verdict::Review] {
            let group: Vec<usize> = (0..adv.items.len()).filter(|&i| adv.items[i].judgement.verdict == v).collect();
            if group.is_empty() {
                continue;
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(serif(v.label(), 19.0).color(verdict_color(v)));
                ui.label(RichText::new(verdict_blurb(v)).color(pal().dim));
            });
            ui.add_space(4.0);
            for i in group {
                rec_row(ui, b, i, actions);
            }
        }
        ui.add_space(20.0);
    });
}

fn rec_row(ui: &mut Ui, b: &Browser, i: usize, actions: &mut Vec<Action>) {
    let rec = &b.advice.items[i];
    let jd = &rec.judgement;
    let auto = rec.is_automatic();
    let tint = (b.checked[i] && auto).then(|| pal().row_sel);
    glass(ui, 14, egui::Margin::symmetric(12, 9), tint, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let mut ticked = b.checked[i] && auto;
            let cb = ui.add_enabled(auto, egui::Checkbox::without_text(&mut ticked));
            if cb.changed() {
                actions.push(Action::ToggleCheck(i));
            }
            if !auto {
                cb.on_disabled_hover_text("Sapu can't do this one for you. See the note.");
            }

            let (pr, _) = ui.allocate_exact_size(Vec2::new(54.0, 18.0), Sense::hover());
            paint_pill(ui.painter(), pr, jd);

            ui.vertical(|ui| {
                let name = rec.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).strong());
                    if jd.is_ai() {
                        ui.label(RichText::new("🧠 AI guess").small().color(pal().ai));
                    } else {
                        ui.label(RichText::new(jd.category.label()).small().color(pal().dim));
                    }
                });
                let parent = rec.path.parent().map(fmt::tilde).unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(parent).size(11.0).color(pal().dim)).truncate());
                let note = if auto { jd.reason.clone() } else { format!("👉 {}", jd.reason) };
                ui.add(egui::Label::new(RichText::new(note).small().color(pal().text2)).wrap());
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(serif(fmt::size(rec.size), 17.0));
                ui.add_space(6.0);
                if ui.small_button("Show").on_hover_text("Find it in the browser").clicked() {
                    actions.push(Action::ShowRec(i));
                }
                if ui.small_button("Keep").on_hover_text("Not this one. Hides it and teaches Sapu AI.").clicked() {
                    actions.push(Action::RejectRec(i));
                }
            });
        });
    });
    ui.add_space(4.0);
}

fn batch_dialog(ctx: &egui::Context, bc: &mut BatchConfirm) -> Option<bool> {
    let mut result = None;
    let modal = egui::Modal::new(Id::unique("confirm-batch")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(serif("Clean selected items?", 20.0));
        ui.add_space(8.0);
        ui.label(format!("{} items, {} in total, will be moved to the Trash.", bc.n, fmt::size(bc.bytes)));
        ui.label(
            RichText::new(
                "Before each one, Sapu re-checks it hasn't changed since the scan and skips it if it has. \
                 Folders like Caches are emptied, not removed. Everything can be undone from 🛡 Safety.",
            )
            .color(pal().dim),
        );
        if let Some(why) = &bc.risky {
            ui.add_space(8.0);
            ui.checkbox(&mut bc.ack, RichText::new(why).color(pal().warn));
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let ok = bc.risky.is_none() || bc.ack;
            let go = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE).strong()).fill(pal().bad);
            if ui.add_enabled(ok, go).clicked() {
                result = Some(true);
            }
            if ui.button("Cancel").clicked() {
                result = Some(false);
            }
        });
    });
    if modal.should_close() && result.is_none() {
        result = Some(false);
    }
    result
}

fn stat_card(ui: &mut Ui, label: &str, value: u64, color: Color32, note: &str) {
    glass(ui, 18, egui::Margin::symmetric(18, 14), None, |ui| {
        ui.set_min_width(210.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(label).color(color).strong());
            ui.label(serif(fmt::size(value), 28.0));
            ui.label(RichText::new(note).color(pal().dim).small());
        });
    });
}

fn cloud_view(ui: &mut Ui, b: &Browser, actions: &mut Vec<Action>) {
    let Some(s) = &b.cloud else { return };
    let p = pal();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.label(serif("iCloud Drive", 24.0));
        ui.label(
            RichText::new(
                "Files in iCloud don't have to take space on this Mac. Removing a download keeps the file safe in iCloud; \
                 it comes back automatically when you open it. Sapu only deletes from iCloud when you explicitly agree, item by item.",
            )
            .color(p.text2),
        );
        ui.add_space(12.0);

        if !s.available {
            glass(ui, 16, egui::Margin::same(18), None, |ui| {
                ui.label(RichText::new("iCloud Drive isn't set up on this Mac.").strong());
                ui.label(RichText::new("Turn it on in System Settings → your name → iCloud → iCloud Drive.").color(p.dim));
            });
            return;
        }
        if s.not_scanned {
            glass(ui, 16, egui::Margin::same(18), None, |ui| {
                ui.label(RichText::new("This scan doesn't include iCloud.").strong());
                ui.label(RichText::new("Open your home folder (⌘O) to see your iCloud Drive here.").color(p.dim));
            });
            return;
        }

        ui.horizontal(|ui| {
            stat_card(ui, "In iCloud", s.in_cloud, p.link, "everything Sapu could see");
            stat_card(ui, "Downloaded on this Mac", s.on_mac, p.accent, "space iCloud uses here");
            stat_card(ui, "Only in iCloud", s.in_cloud.saturating_sub(s.on_mac), p.ok, "not taking space here");
        });
        ui.add_space(8.0);
        let mut notes = Vec::new();
        if s.desktop_documents_synced {
            notes.push("Your Desktop and Documents folders sync with iCloud, so they're treated as iCloud too.".to_string());
        }
        if let Some((_, size)) = &s.photos_library {
            if *size > 2_000_000_000 {
                notes.push(format!(
                    "Your Photos library uses {} here. If you use iCloud Photos, turn on Photos → Settings → iCloud → Optimize Mac Storage.",
                    fmt::size(*size)
                ));
            }
        }
        for n in notes {
            ui.label(RichText::new(format!("ℹ  {n}")).color(p.text2));
        }

        ui.add_space(14.0);
        ui.label(serif("Downloaded, biggest first", 19.0));
        if s.downloaded.is_empty() {
            ui.label(RichText::new("Nothing large is downloaded.").color(p.dim));
        }
        for item in &s.downloaded {
            cloud_row(ui, item, true, actions);
        }

        ui.add_space(14.0);
        ui.label(serif("Only in iCloud", 19.0));
        if s.cloud_only.is_empty() {
            ui.label(RichText::new("Nothing large is cloud-only.").color(p.dim));
        }
        for item in &s.cloud_only {
            cloud_row(ui, item, false, actions);
        }
        ui.add_space(20.0);
    });
}

fn cloud_row(ui: &mut Ui, item: &icloud::CloudItem, downloaded: bool, actions: &mut Vec<Action>) {
    let p = pal();
    glass(ui, 14, egui::Margin::symmetric(14, 9), None, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(if downloaded { "⬇" } else { "☁" }).size(16.0).color(if downloaded {
                p.accent
            } else {
                p.link
            }));
            ui.vertical(|ui| {
                let name = item.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                ui.label(RichText::new(name).strong());
                let parent = item.path.parent().map(fmt::tilde).unwrap_or_default();
                let age = sapu_core::judge::human_age(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                        .saturating_sub(item.modified)
                        / 86_400,
                );
                let when = if age == "today" { "changed today".to_string() } else { format!("changed {age} ago") };
                ui.add(
                    egui::Label::new(RichText::new(format!("{parent} · {when}")).size(11.0).color(p.dim)).truncate(),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let size = if downloaded { item.on_mac } else { item.in_cloud };
                ui.label(serif(fmt::size(size), 17.0));
                ui.add_space(6.0);
                if downloaded {
                    if ui.button("Remove download").on_hover_text("Frees the space here. Stays in iCloud.").clicked() {
                        actions.push(Action::Evict(item.idx_path.clone()));
                    }
                } else if ui.button("Download").clicked() {
                    actions.push(Action::Download(item.idx_path.clone()));
                }
                if ui.small_button("Show").clicked() {
                    actions.push(Action::ShowIdx(item.idx_path.clone()));
                }
                if ui
                    .small_button(RichText::new("Delete…").color(pal().bad))
                    .on_hover_text("Delete from iCloud on all devices (asks for consent first)")
                    .clicked()
                {
                    actions.push(Action::AskTrashIdx(item.idx_path.clone()));
                }
            });
        });
    });
    ui.add_space(4.0);
}

fn job_dialog(ctx: &egui::Context, job: &Job, actions: &mut Vec<Action>) {
    egui::Modal::new(Id::unique("clean-job")).show(ctx, |ui| {
        ui.set_width(440.0);
        let done = job.done.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(pal().accent));
            ui.label(serif(format!("Cleaning… {done} / {}", job.total), 18.0));
        });
        ui.add_space(8.0);
        ui.add(egui::ProgressBar::new(done as f32 / job.total.max(1) as f32).fill(pal().accent));
        let cur = job.current.lock().map(|c| c.clone()).unwrap_or_default();
        ui.add(egui::Label::new(RichText::new(fmt::truncate_left(&cur, 60)).small().color(pal().dim)).truncate());
        ui.add_space(10.0);
        let stopping = job.cancel.load(Ordering::Relaxed);
        if ui
            .add_enabled(!stopping, egui::Button::new(if stopping { "Stopping after this item…" } else { "Stop" }))
            .clicked()
        {
            actions.push(Action::CancelJob);
        }
    });
}

fn safety_view(
    ui: &mut Ui,
    history: &[journal::Batch],
    protected: &[PathBuf],
    usage: &[sapu_core::paths::Usage],
    actions: &mut Vec<Action>,
) {
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.label(serif("How Sapu keeps you safe", 22.0));
        ui.add_space(4.0);
        for line in [
            "Everything goes to the Trash first. Nothing is ever deleted outright.",
            "Every action is written to a journal before it happens, and can be undone below.",
            "Each item is re-checked just before cleaning and skipped if it changed since the scan.",
            "macOS, your home folder, ~/Library, keychains and .ssh can never be removed.",
            "Your protected items below, and any folder containing them, are never touched.",
            "AI guesses and Review items are never ticked for you.",
            "Nothing in iCloud is deleted without your explicit consent, item by item.",
        ] {
            ui.label(RichText::new(format!("✓  {line}")).color(pal().text2));
        }

        ui.add_space(16.0);
        ui.label(serif("Protected by you", 19.0));
        if protected.is_empty() {
            ui.label(RichText::new("Nothing yet. Right-click anything in Browse → 🛡 Protect.").color(pal().dim));
        }
        for p in protected {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("🛡 {}", fmt::tilde(p))).color(pal().keep));
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Unprotect(p.clone()));
                }
            });
        }

        ui.add_space(16.0);
        ui.label(serif("Data on this Mac", 19.0));
        ui.label(
            RichText::new("Sapu keeps everything locally, never inside the app and never online. Updating Sapu keeps it.")
                .color(pal().text2),
        );
        ui.add_space(4.0);
        for u in usage {
            ui.horizontal(|ui| {
                ui.label(RichText::new(u.label).strong());
                ui.label(RichText::new(format!("{} · {}", fmt::size(u.bytes), u.note)).color(pal().dim));
                ui.label(RichText::new(fmt::tilde(&u.path)).small().color(pal().dim).monospace());
                if ui.small_button("Show").clicked() {
                    actions.push(Action::RevealPath(u.path.clone()));
                }
                if u.clearable && u.bytes > 0 && ui.small_button("Clear").on_hover_text("Safe: it's only a cache").clicked() {
                    actions.push(Action::ClearCache);
                }
            });
        }

        ui.add_space(16.0);
        ui.label(serif("History", 19.0));
        if history.is_empty() {
            ui.label(RichText::new("Nothing cleaned yet.").color(pal().dim));
        }
        for b in history.iter().take(50) {
            glass(ui, 14, egui::Margin::symmetric(14, 10), None, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&b.label).strong());
                            let failed = b.count(ItemState::Failed);
                            let restored = b.count(ItemState::Restored);
                            let unknown = b.count(ItemState::Unknown);
                            let mut parts = vec![
                                fmt::ago(b.time),
                                format!("{} items", b.items.len()),
                                format!("{} to Trash", fmt::size(b.bytes_trashed())),
                            ];
                            if failed > 0 {
                                parts.push(format!("{failed} skipped"));
                            }
                            if restored > 0 {
                                parts.push(format!("{restored} restored"));
                            }
                            if unknown > 0 {
                                parts.push(format!("{unknown} interrupted"));
                            }
                            ui.label(RichText::new(parts.join(" · ")).small().color(pal().dim));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let n = b.restorable();
                            let btn = egui::Button::new(format!("↩ Undo ({n})"));
                            if ui
                                .add_enabled(n > 0, btn)
                                .on_disabled_hover_text("Nothing left to restore from the Trash (already restored, emptied, or location unknown: use Finder → Put Back)")
                                .clicked()
                            {
                                actions.push(Action::Undo(b.id));
                            }
                        });
                    });
                    egui::CollapsingHeader::new(RichText::new("Details").small())
                        .id_salt(("batch", b.id))
                        .show(ui, |ui| {
                            for it in b.items.iter().take(200) {
                                let (icon, col) = match it.state {
                                    ItemState::Trashed => ("🗑", pal().dim),
                                    ItemState::Restored => ("↩", pal().ok),
                                    ItemState::Failed => ("⚠", pal().warn),
                                    ItemState::Unknown => ("?", pal().warn),
                                };
                                let mut t = format!("{icon} {}  {}", fmt::size(it.bytes), fmt::tilde(&it.path));
                                if it.state == ItemState::Failed && !it.note.is_empty() {
                                    t.push_str(&format!(" ({})", it.note));
                                }
                                ui.label(RichText::new(t).small().color(col));
                            }
                        });
                });
            ui.add_space(4.0);
        }
    });
}

/// Some(true) = confirmed, Some(false) = cancelled, None = still open.
/// iCloud items need an explicit "I understand" tick before the button works.
fn confirm_dialog(ctx: &egui::Context, c: &mut Confirm) -> Option<bool> {
    let p = pal();
    let mut result = None;
    let modal = egui::Modal::new(Id::unique("confirm-trash")).show(ctx, |ui| {
        ui.set_width(470.0);
        let title = if c.icloud { "Delete from iCloud?" } else { "Move to Trash?" };
        ui.label(serif(title, 20.0).color(if c.icloud { p.bad } else { p.text }));
        ui.add_space(8.0);
        ui.label(RichText::new(fmt::truncate_left(&fmt::tilde(&c.path), 64)).monospace());
        ui.add_space(4.0);
        let kind = if c.is_dir { "folder" } else { "file" };
        if c.icloud {
            glass(ui, 12, egui::Margin::same(12), Some(p.bad.gamma_multiply(0.14)), |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("☁ This {kind} ({}) is in iCloud.", fmt::size(c.size))).strong().color(p.bad));
                for line in [
                    "Deleting it removes it from iCloud on all your devices: iPhone, iPad and other Macs.",
                    "It goes to this Mac's Trash first. iCloud.com can also recover deleted files for about 30 days.",
                    "If you only want to free space on this Mac, use “Remove download” instead. That keeps it in iCloud.",
                ] {
                    ui.label(RichText::new(format!("•  {line}")).color(p.text2));
                }
            });
            ui.add_space(10.0);
            ui.checkbox(&mut c.consent, RichText::new("I understand. Delete it from iCloud everywhere.").strong());
        } else {
            ui.label(
                RichText::new(format!("{} {kind} will be moved to the Trash. You can put it back from there.", fmt::size(c.size)))
                    .color(p.dim),
            );
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let allowed = !c.icloud || c.consent;
            let label = if c.icloud { "Delete from iCloud" } else { "Move to Trash" };
            let go = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(p.bad);
            let enter = !c.icloud && ui.input(|i| i.key_pressed(Key::Enter)); // no keyboard shortcut for iCloud deletes
            if (ui.add_enabled(allowed, go).clicked() || enter) && allowed {
                result = Some(true);
            }
            if ui.button("Cancel").clicked() {
                result = Some(false);
            }
        });
    });
    if modal.should_close() && result.is_none() {
        result = Some(false);
    }
    if result == Some(false) && c.icloud {
        info!(path = %c.path.display(), "iCloud delete cancelled");
    }
    result
}

pub fn reveal_path(path: &Path) {
    reveal(path)
}

fn reveal(path: &Path) {
    info!(path = %path.display(), "reveal in Finder");
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn()
    };
    if let Err(e) = result {
        warn!(path = %path.display(), error = %e, "could not reveal");
    }
}
