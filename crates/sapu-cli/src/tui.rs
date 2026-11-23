//! Interactive disk explorer (ratatui).
//!
//! The scan runs on a background thread while a live progress screen shows
//! counters. Once done, the whole tree is in memory, so browsing is instant.

use crate::format::{self, tilde, truncate_left};
use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
    DefaultTerminal, Frame,
};
use sapu_core::{cleaner, Kind, Node, Progress, ScanOptions, Tree};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

const ACCENT: Color = Color::Rgb(255, 170, 60);
const DIM: Color = Color::DarkGray;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn run(root: PathBuf, opts: ScanOptions, log_file: Option<PathBuf>) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = App::new(root, opts, log_file).run(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Size,
    Name,
    Files,
}

impl SortBy {
    fn next(self) -> Self {
        match self {
            SortBy::Size => SortBy::Name,
            SortBy::Name => SortBy::Files,
            SortBy::Files => SortBy::Size,
        }
    }
    fn label(self) -> &'static str {
        match self {
            SortBy::Size => "size",
            SortBy::Name => "name",
            SortBy::Files => "items",
        }
    }
}

enum Phase {
    Scanning { progress: Arc<Progress>, handle: Option<JoinHandle<sapu_core::Result<Tree>>>, started: Instant },
    Ready(Box<Browser>),
    Failed(String),
}

struct Browser {
    tree: Tree,
    /// Child indices from the root to the folder being shown.
    path: Vec<usize>,
    /// Selected row in each ancestor, restored when going back.
    history: Vec<usize>,
    /// Current folder's children, as indices, in display order.
    view: Vec<usize>,
    table: TableState,
    sort: SortBy,
}

impl Browser {
    fn new(tree: Tree) -> Self {
        let mut b = Self {
            tree,
            path: Vec::new(),
            history: Vec::new(),
            view: Vec::new(),
            table: TableState::default().with_selected(Some(0)),
            sort: SortBy::Size,
        };
        b.rebuild_view();
        b
    }

    fn current(&self) -> &Node {
        self.tree.root.get(&self.path).unwrap_or(&self.tree.root)
    }

    fn rebuild_view(&mut self) {
        let node = self.tree.root.get(&self.path).unwrap_or(&self.tree.root);
        let mut view: Vec<usize> = (0..node.children.len()).collect();
        match self.sort {
            SortBy::Size => {} // children are already largest-first
            SortBy::Name => view.sort_by_cached_key(|&i| node.children[i].name.to_lowercase()),
            SortBy::Files => view.sort_by_key(|&i| std::cmp::Reverse(node.children[i].files)),
        }
        self.view = view;
        let max = self.view.len().saturating_sub(1);
        let sel = self.table.selected().unwrap_or(0).min(max);
        self.table.select(Some(sel));
    }

    fn selected_index(&self) -> Option<usize> {
        self.table.selected().and_then(|r| self.view.get(r).copied())
    }

    fn selected_node(&self) -> Option<&Node> {
        self.selected_index().map(|i| &self.current().children[i])
    }

    fn selected_idx_path(&self) -> Option<Vec<usize>> {
        let i = self.selected_index()?;
        let mut p = self.path.clone();
        p.push(i);
        Some(p)
    }

    fn enter(&mut self) {
        let Some(i) = self.selected_index() else { return };
        if !self.current().children[i].is_dir() {
            return;
        }
        self.history.push(self.table.selected().unwrap_or(0));
        self.path.push(i);
        self.table.select(Some(0));
        self.rebuild_view();
    }

    fn back(&mut self) {
        if self.path.pop().is_some() {
            let row = self.history.pop().unwrap_or(0);
            self.table.select(Some(row));
            self.rebuild_view();
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let max = self.view.len() as isize - 1;
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.table.select(Some((cur + delta).clamp(0, max) as usize));
    }
}

struct Confirm {
    path: PathBuf,
    idx_path: Vec<usize>,
    size: u64,
}

enum Status {
    Info(String),
    Error(String),
}

struct App {
    root: PathBuf,
    opts: ScanOptions,
    phase: Phase,
    confirm: Option<Confirm>,
    status: Option<(Status, Instant)>,
    show_help: bool,
    log_file: Option<PathBuf>,
    quit: bool,
}

impl App {
    fn new(root: PathBuf, opts: ScanOptions, log_file: Option<PathBuf>) -> Self {
        let mut app = Self {
            phase: Phase::Failed(String::new()),
            root,
            opts,
            confirm: None,
            status: None,
            show_help: false,
            log_file,
            quit: false,
        };
        app.start_scan();
        app
    }

    fn start_scan(&mut self) {
        let progress = Arc::new(Progress::default());
        let (root, opts, p) = (self.root.clone(), self.opts.clone(), progress.clone());
        let handle = std::thread::spawn(move || sapu_core::scan(&root, &opts, &p));
        self.phase = Phase::Scanning { progress, handle: Some(handle), started: Instant::now() };
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            self.tick();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(80))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key);
                    }
                }
            }
        }
        if let Phase::Scanning { progress, handle, .. } = &mut self.phase {
            progress.cancel();
            if let Some(h) = handle.take() {
                let _ = h.join();
            }
        }
        Ok(())
    }

    fn tick(&mut self) {
        if let Phase::Scanning { handle, .. } = &mut self.phase {
            if handle.as_ref().is_some_and(|h| h.is_finished()) {
                let result = handle.take().unwrap().join();
                self.phase = match result {
                    Ok(Ok(tree)) => {
                        if tree.stats.errors > 0 {
                            self.set_status(Status::Error(format!(
                                "{} items unreadable. Grant Full Disk Access to your terminal for a full scan.",
                                format::count(tree.stats.errors)
                            )));
                        }
                        Phase::Ready(Box::new(Browser::new(tree)))
                    }
                    Ok(Err(e)) => {
                        error!(error = %e, "scan failed");
                        Phase::Failed(e.to_string())
                    }
                    Err(_) => Phase::Failed("scanner thread panicked".into()),
                };
            }
        }
        if let Some((_, at)) = &self.status {
            if at.elapsed() > Duration::from_secs(6) {
                self.status = None;
            }
        }
    }

    fn set_status(&mut self, s: Status) {
        self.status = Some((s, Instant::now()));
    }

    // ───────────────────────── input ─────────────────────────

    fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.do_trash(),
                _ => self.confirm = None,
            }
            return;
        }

        match &mut self.phase {
            Phase::Scanning { progress, .. } => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    info!("scan cancelled by user");
                    progress.cancel(); // tick() picks up the partial tree
                }
            }
            Phase::Failed(_) => match key.code {
                KeyCode::Char('r') => self.start_scan(),
                _ => self.quit = true,
            },
            Phase::Ready(b) => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Esc if b.path.is_empty() => self.quit = true,
                KeyCode::Down | KeyCode::Char('j') => b.move_by(1),
                KeyCode::Up | KeyCode::Char('k') => b.move_by(-1),
                KeyCode::PageDown => b.move_by(15),
                KeyCode::PageUp => b.move_by(-15),
                KeyCode::Home | KeyCode::Char('g') => b.table.select(Some(0)),
                KeyCode::End | KeyCode::Char('G') => b.move_by(isize::MAX / 2),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => b.enter(),
                KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Esc => b.back(),
                KeyCode::Char('s') => {
                    b.sort = b.sort.next();
                    b.rebuild_view();
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    if let (Some(n), Some(idx)) = (b.selected_node(), b.selected_idx_path()) {
                        self.confirm = Some(Confirm { path: b.tree.path_of(&idx), size: n.size, idx_path: idx });
                    }
                }
                KeyCode::Char('o') => {
                    if let Some(idx) = b.selected_idx_path() {
                        let path = b.tree.path_of(&idx);
                        reveal(&path);
                    }
                }
                KeyCode::Char('r') => self.start_scan(),
                KeyCode::Char('?') => self.show_help = true,
                _ => {}
            },
        }
    }

    fn do_trash(&mut self) {
        let Some(c) = self.confirm.take() else { return };
        let Phase::Ready(b) = &mut self.phase else { return };
        let journal = sapu_core::Journal::open();
        let batch = journal.begin(&format!("Moved {} to Trash", tilde(&c.path)));
        match cleaner::trash_item(&c.path, &journal, batch, c.size, "picked by hand", sapu_core::safety::ICloud::Refuse)
        {
            Ok(_) => {
                b.tree.root.remove(&c.idx_path);
                b.rebuild_view();
                let msg = format!("Moved {} to Trash, freed {}", tilde(&c.path), format::size(c.size));
                self.set_status(Status::Info(msg));
            }
            Err(e) => {
                warn!(path = %c.path.display(), error = %e, "trash failed");
                self.set_status(Status::Error(format!("Could not trash: {e}")));
            }
        }
    }

    // ───────────────────────── drawing ─────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        match &mut self.phase {
            Phase::Scanning { progress, started, .. } => draw_scanning(f, area, &self.root, progress, *started),
            Phase::Failed(msg) => draw_failed(f, area, msg),
            Phase::Ready(b) => {
                let [header, body, footer] =
                    Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(1)]).areas(area);
                draw_header(f, header, b);
                draw_table(f, body, b);
                draw_footer(f, footer, self.status.as_ref().map(|(s, _)| s));
            }
        }
        if let Some(c) = &self.confirm {
            draw_confirm(f, area, c);
        }
        if self.show_help {
            draw_help(f, area, self.log_file.as_deref());
        }
    }
}

fn draw_scanning(f: &mut Frame, area: Rect, root: &std::path::Path, p: &Progress, started: Instant) {
    let s = p.snapshot();
    let secs = started.elapsed().as_secs_f64();
    let frame = SPINNER[(started.elapsed().as_millis() / 80) as usize % SPINNER.len()];
    let rate = if secs > 0.0 { s.files as f64 / secs } else { 0.0 };

    let popup = centered(area, 70, 11);
    let width = popup.width.saturating_sub(16) as usize;
    let current = truncate_left(&tilde(&p.current_path()), width);

    let row =
        |k: &str, v: String| Line::from(vec![Span::styled(format!("  {k:<10}"), Style::new().fg(DIM)), Span::raw(v)]);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("  {frame} "), Style::new().fg(ACCENT)),
            Span::styled(format!("Scanning {}", tilde(root)), Style::new().bold()),
        ]),
        Line::raw(""),
        row("Size", format::size(s.bytes)),
        row("Files", format::count(s.files)),
        row("Folders", format::count(s.dirs)),
        row("Speed", format!("{} files/s · {:.1}s", format::count(rate as u64), secs)),
        row("In", current),
        Line::raw(""),
        Line::styled("  q / Esc to stop and browse what's scanned so far", Style::new().fg(DIM)),
    ];
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(panel(" sapu ")), popup);
}

fn draw_failed(f: &mut Frame, area: Rect, msg: &str) {
    let popup = centered(area, 70, 7);
    let text = vec![
        Line::styled("  Scan failed", Style::new().fg(Color::Red).bold()),
        Line::raw(""),
        Line::raw(format!("  {msg}")),
        Line::raw(""),
        Line::styled("  r retry · any other key quits", Style::new().fg(DIM)),
    ];
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(text).block(panel(" sapu ")), popup);
}

fn draw_header(f: &mut Frame, area: Rect, b: &Browser) {
    let node = b.current();
    let path = b.tree.path_of(&b.path);
    let st = &b.tree.stats;

    let mut crumbs = vec![
        Span::styled(" sapu ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw("  "),
        Span::styled(tilde(&path), Style::new().bold()),
    ];
    if node.incomplete {
        crumbs.push(Span::styled("  ⚠ partly unreadable", Style::new().fg(Color::Yellow)));
    }
    if st.cancelled {
        crumbs.push(Span::styled("  ⏸ partial scan", Style::new().fg(Color::Yellow)));
    }

    let info = Line::from(vec![
        Span::raw("  "),
        Span::styled(format::size(node.size), Style::new().fg(ACCENT).bold()),
        Span::styled(
            format!(
                "  ·  {} items  ·  sorted by {}  ·  scanned {} files in {:.1}s",
                format::count(node.files),
                b.sort.label(),
                format::count(st.files),
                st.elapsed.as_secs_f64()
            ),
            Style::new().fg(DIM),
        ),
    ]);

    let block = Block::new().borders(Borders::BOTTOM).border_style(Style::new().fg(DIM));
    f.render_widget(Paragraph::new(vec![Line::raw(""), Line::from(crumbs), info]).block(block), area);
}

fn draw_table(f: &mut Frame, area: Rect, b: &mut Browser) {
    let node = b.tree.root.get(&b.path).unwrap_or(&b.tree.root);
    if node.children.is_empty() {
        let msg = Paragraph::new("\n(empty folder)").alignment(Alignment::Center).fg(DIM);
        f.render_widget(msg, area);
        return;
    }

    const BAR_W: usize = 20;
    let total = node.size;
    let rows = b.view.iter().map(|&i| {
        let c = &node.children[i];
        let frac = format::percent(c.size, total);
        let bar_color = match frac {
            x if x >= 0.25 => Color::Red,
            x if x >= 0.10 => ACCENT,
            x if x >= 0.02 => Color::Yellow,
            _ => Color::Green,
        };
        let (name, name_style) = match c.kind {
            Kind::Dir => (format!("{}/", c.name), Style::new().fg(Color::LightBlue).bold()),
            Kind::Symlink => (format!("{} →", c.name), Style::new().fg(Color::Cyan).italic()),
            Kind::Other => (c.name.to_string(), Style::new().fg(DIM)),
            Kind::File => (c.name.to_string(), Style::new()),
        };
        let mut name_spans = vec![Span::styled(name, name_style)];
        if c.incomplete {
            name_spans.push(Span::styled(" ⚠", Style::new().fg(Color::Yellow)));
        }
        let items = if c.is_dir() { format::count(c.files) } else { String::new() };

        Row::new(vec![
            Cell::from(Line::from(format::size(c.size)).alignment(Alignment::Right)),
            Cell::from(Span::styled(format::bar(frac, BAR_W), Style::new().fg(bar_color))),
            Cell::from(Line::from(format!("{:>5.1}%", frac * 100.0)).fg(DIM)),
            Cell::from(Line::from(name_spans)),
            Cell::from(Line::from(items).alignment(Alignment::Right).fg(DIM)),
        ])
    });

    let header = Row::new(["SIZE", "", "%", "NAME", "ITEMS"].map(|h| {
        let cell = Line::from(h).style(Style::new().fg(DIM).add_modifier(Modifier::BOLD));
        if h == "SIZE" || h == "ITEMS" {
            Cell::from(cell.alignment(Alignment::Right))
        } else {
            Cell::from(cell)
        }
    }))
    .bottom_margin(0);

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(BAR_W as u16),
            Constraint::Length(6),
            Constraint::Fill(1),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .column_spacing(2)
    .row_highlight_style(Style::new().bg(Color::Rgb(45, 45, 55)).add_modifier(Modifier::BOLD))
    .highlight_symbol(Span::styled("▌", Style::new().fg(ACCENT)));

    f.render_stateful_widget(table, area, &mut b.table);
}

fn draw_footer(f: &mut Frame, area: Rect, status: Option<&Status>) {
    let line = match status {
        Some(Status::Info(m)) => Line::from(Span::styled(format!(" ✓ {m}"), Style::new().fg(Color::Green))),
        Some(Status::Error(m)) => Line::from(Span::styled(format!(" ✗ {m}"), Style::new().fg(Color::Red))),
        None => {
            let keys = [
                ("↑↓", "move"),
                ("⏎", "open"),
                ("⌫", "back"),
                ("s", "sort"),
                ("d", "trash"),
                ("o", "reveal"),
                ("r", "rescan"),
                ("?", "help"),
                ("q", "quit"),
            ];
            let mut spans = vec![Span::raw(" ")];
            for (k, label) in keys {
                spans.push(Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(DIM)));
                spans.push(Span::styled(format!(" {label}  "), Style::new().fg(DIM)));
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_confirm(f: &mut Frame, area: Rect, c: &Confirm) {
    let popup = centered(area, 72, 8);
    let w = popup.width.saturating_sub(6) as usize;
    let text = vec![
        Line::raw(""),
        Line::from(vec![Span::raw("  "), Span::styled(truncate_left(&tilde(&c.path), w), Style::new().bold())]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format::size(c.size), Style::new().fg(ACCENT).bold()),
            Span::styled(" will be moved to the Trash (recoverable)", Style::new().fg(DIM)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(" y ", Style::new().fg(Color::Black).bg(Color::Red).bold()),
            Span::raw(" move to Trash    "),
            Span::styled(" n ", Style::new().fg(Color::Black).bg(DIM)),
            Span::raw(" cancel"),
        ]),
    ];
    let block = panel(" Move to Trash? ").border_style(Style::new().fg(Color::Red));
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(text).block(block), popup);
}

fn draw_help(f: &mut Frame, area: Rect, log_file: Option<&std::path::Path>) {
    let popup = centered(area, 60, 17);
    let k = |key: &str, what: &str| {
        Line::from(vec![Span::styled(format!("  {key:<14}"), Style::new().fg(ACCENT)), Span::raw(what.to_string())])
    };
    let mut lines = vec![
        Line::raw(""),
        k("↑ ↓  j k", "move"),
        k("PgUp PgDn", "move faster"),
        k("g / G", "top / bottom"),
        k("⏎  →  l", "open folder"),
        k("⌫  ←  h  Esc", "go back"),
        k("s", "sort: size → name → items"),
        k("d  Del", "move to Trash (asks first)"),
        k("o", "reveal in Finder"),
        k("r", "rescan"),
        k("q  Ctrl-C", "quit"),
        Line::raw(""),
    ];
    if let Some(p) = log_file {
        lines.push(Line::styled(format!("  log: {}", tilde(p)), Style::new().fg(DIM)));
    }
    lines.push(Line::styled("  any key to close", Style::new().fg(DIM)));
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(panel(" Keys ")), popup);
}

fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(title, Style::new().fg(ACCENT).bold()))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(height.min(area.height))]).flex(Flex::Center).areas(area);
    let [h] = Layout::horizontal([Constraint::Length(width.min(area.width))]).flex(Flex::Center).areas(v);
    h
}

fn reveal(path: &std::path::Path) {
    let result = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(path).spawn()
    } else {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open").arg(dir).spawn()
    };
    if let Err(e) = result {
        warn!(path = %path.display(), error = %e, "could not reveal");
    }
}
