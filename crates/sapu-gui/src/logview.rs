//! Live log capture + the "📜 Logs" window.
//!
//! A tracing layer copies every event (debug and up from Sapu, warnings from
//! libraries) into a ring buffer in memory. The log window reads it live.
//! Everything is also written to the log file (see `main.rs`).

use crate::fmt;
use crate::theme::{self, pal, serif};
use eframe::egui::{self, Align, Color32, FontId, Key, Layout, RichText, Ui, ViewportBuilder, ViewportId};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

const CAPACITY: usize = 5_000;

#[derive(Debug, Clone)]
pub struct LogLine {
    /// Seconds since Sapu started.
    pub t: f32,
    pub level: Level,
    pub target: String,
    pub message: String,
    /// `key=value` pairs attached to the event.
    pub fields: String,
}

#[derive(Clone)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
    started: Instant,
    /// Total lines ever captured (so the viewer can tell when something new arrived).
    count: Arc<std::sync::atomic::AtomicUsize>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(CAPACITY))),
            started: Instant::now(),
            count: Arc::default(),
        }
    }

    pub fn total(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        self.lines.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut l) = self.lines.lock() {
            l.clear();
        }
    }

    pub fn counts(&self) -> (usize, usize) {
        let l = self.lines.lock();
        l.map(|l| {
            // Only Sapu's own problems light up the button (library chatter doesn't).
            let ours = |x: &&LogLine| x.target.starts_with("sapu");
            (
                l.iter().filter(ours).filter(|x| x.level == Level::ERROR).count(),
                l.iter().filter(ours).filter(|x| x.level == Level::WARN).count(),
            )
        })
        .unwrap_or((0, 0))
    }
}

#[derive(Default)]
struct Collect {
    message: String,
    fields: Vec<String>,
}

impl Visit for Collect {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={:?}", field.name(), value));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut c = Collect::default();
        event.record(&mut c);
        let line = LogLine {
            t: self.started.elapsed().as_secs_f32(),
            level: *meta.level(),
            target: meta.target().to_string(),
            message: c.message,
            fields: c.fields.join("  "),
        };
        if let Ok(mut l) = self.lines.lock() {
            if l.len() >= CAPACITY {
                l.pop_front();
            }
            l.push_back(line);
        }
        self.count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// ── The window ──────────────────────────────────────────────────────

pub struct LogViewer {
    pub open: bool,
    buffer: LogBuffer,
    file: Option<PathBuf>,
    show_trace: bool,
    show_debug: bool,
    show_info: bool,
    show_warn: bool,
    show_error: bool,
    search: String,
    follow: bool,
    seen: usize,
}

impl LogViewer {
    pub fn new(buffer: LogBuffer, file: Option<PathBuf>) -> Self {
        Self {
            open: false,
            buffer,
            file,
            show_trace: false,
            show_debug: true,
            show_info: true,
            show_warn: true,
            show_error: true,
            search: String::new(),
            follow: true,
            seen: 0,
        }
    }

    /// (errors, warnings) currently in the buffer, for the button badge.
    pub fn counts(&self) -> (usize, usize) {
        self.buffer.counts()
    }

    fn wants(&self, l: &Level) -> bool {
        match *l {
            Level::TRACE => self.show_trace,
            Level::DEBUG => self.show_debug,
            Level::INFO => self.show_info,
            Level::WARN => self.show_warn,
            _ => self.show_error,
        }
    }

    /// Call every frame from the main window.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let builder = ViewportBuilder::default()
            .with_title("Sapu — Logs")
            .with_inner_size([960.0, 600.0])
            .with_min_inner_size([520.0, 300.0]);
        ctx.show_viewport_immediate(ViewportId::from_hash_of("sapu-logs"), builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested() || (i.modifiers.command && i.key_pressed(Key::W))) {
                self.open = false;
                return;
            }
            self.ui(ui);
        });
    }

    fn ui(&mut self, ui: &mut Ui) {
        let p = pal();
        // Solid background: this window isn't translucent.
        let solid = if ui.ctx().theme() == egui::Theme::Dark { theme::DARK_P.bg } else { theme::LIGHT.bg };
        let lines = self.buffer.snapshot();
        let total = self.buffer.total();
        let new_arrived = total != self.seen;
        self.seen = total;

        egui::Panel::top("log-top")
            .frame(egui::Frame::new().fill(solid).inner_margin(egui::Margin::symmetric(14, 10)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(serif("Logs", 20.0).color(p.accent));
                    ui.add_space(10.0);
                    for (on, name, col) in [
                        (&mut self.show_error, "Error", p.bad),
                        (&mut self.show_warn, "Warn", p.warn),
                        (&mut self.show_info, "Info", p.ok),
                        (&mut self.show_debug, "Debug", p.link),
                        (&mut self.show_trace, "Trace", p.dim),
                    ] {
                        let text = RichText::new(name).color(if *on { col } else { p.dim });
                        if ui.selectable_label(*on, text).clicked() {
                            *on = !*on;
                        }
                    }
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("🔍 search").desired_width(200.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            self.buffer.clear();
                        }
                        if ui.button("Copy").on_hover_text("Copy the visible lines").clicked() {
                            let text: String = lines
                                .iter()
                                .filter(|l| self.matches(l))
                                .map(|l| {
                                    format!("{:>9.3}s {:<5} {} {} {}\n", l.t, l.level, l.target, l.message, l.fields)
                                })
                                .collect();
                            ui.ctx().copy_text(text);
                        }
                        if let Some(f) = &self.file {
                            if ui.button("Show log file").on_hover_text(fmt::tilde(f)).clicked() {
                                crate::app::reveal_path(f);
                            }
                        }
                        ui.checkbox(&mut self.follow, "Follow");
                    });
                });
            });

        let shown: Vec<&LogLine> = lines.iter().filter(|l| self.matches(l)).collect();
        egui::Panel::bottom("log-bottom")
            .frame(egui::Frame::new().fill(solid).inner_margin(egui::Margin::symmetric(14, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} of {} lines (last {CAPACITY} kept in memory)",
                            shown.len(),
                            lines.len()
                        ))
                        .small()
                        .color(p.dim),
                    );
                    if let Some(f) = &self.file {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("full log: {}", fmt::tilde(f))).small().color(p.dim));
                        });
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(solid).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                let row_h = 18.0;
                let area = egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(self.follow);
                if new_arrived {
                    ui.ctx().request_repaint();
                }
                area.show_rows(ui, row_h, shown.len(), |ui, range| {
                    let end = range.end.min(shown.len());
                    let start = range.start.min(end);
                    for l in &shown[start..end] {
                        let (col, tag) = match l.level {
                            Level::ERROR => (p.bad, "ERROR"),
                            Level::WARN => (p.warn, "WARN "),
                            Level::INFO => (p.ok, "INFO "),
                            Level::DEBUG => (p.link, "DEBUG"),
                            _ => (p.dim, "TRACE"),
                        };
                        let font = FontId::monospace(12.0);
                        let mut job = egui::text::LayoutJob::default();
                        let mut add = |s: &str, c: Color32| {
                            job.append(
                                s,
                                0.0,
                                egui::TextFormat { font_id: font.clone(), color: c, ..Default::default() },
                            );
                        };
                        add(&format!("{:>9.3}s ", l.t), p.dim);
                        add(&format!("{tag} "), col);
                        add(&format!("{:<22} ", short_target(&l.target)), p.dim);
                        add(&l.message, p.text);
                        if !l.fields.is_empty() {
                            add(&format!("  {}", l.fields), p.text2);
                        }
                        ui.add(egui::Label::new(job).truncate());
                    }
                });
            });

        // Keep refreshing while open so new lines appear.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }

    fn matches(&self, l: &LogLine) -> bool {
        if !self.wants(&l.level) {
            return false;
        }
        if self.search.is_empty() {
            return true;
        }
        let q = self.search.to_lowercase();
        l.message.to_lowercase().contains(&q)
            || l.fields.to_lowercase().contains(&q)
            || l.target.to_lowercase().contains(&q)
    }
}

fn short_target(t: &str) -> &str {
    t.strip_prefix("sapu_").unwrap_or(t)
}
