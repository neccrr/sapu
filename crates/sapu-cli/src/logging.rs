//! Logging setup.
//!
//! - Terminal commands: colored, leveled lines on stderr. Lines are printed
//!   *above* any active progress bar instead of tearing through it.
//! - TUI: logs go to a file (the screen belongs to the UI).
//!
//! Verbosity: `-q` errors only, default info, `-v` debug, `-vv` trace.
//! `SAPU_LOG` overrides everything, e.g. `SAPU_LOG=sapu_core=trace`.

use indicatif::MultiProgress;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tracing_subscriber::{fmt, EnvFilter};

static BARS: OnceLock<MultiProgress> = OnceLock::new();

/// Shared progress-bar container; log lines are routed through it.
pub fn bars() -> &'static MultiProgress {
    BARS.get_or_init(MultiProgress::new)
}

pub enum Target {
    Terminal,
    File,
}

fn filter(verbosity: i8) -> EnvFilter {
    let level = match verbosity {
        i8::MIN..=-1 => "error",
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    EnvFilter::try_from_env("SAPU_LOG")
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,sapu={level},sapu_core={level}")))
}

/// Returns the log file path when logging to a file.
pub fn init(verbosity: i8, target: Target) -> anyhow::Result<Option<PathBuf>> {
    match target {
        Target::Terminal => {
            fmt()
                .with_env_filter(filter(verbosity))
                .with_writer(TermWriter::default)
                .with_ansi(io::stderr().is_terminal())
                .with_target(verbosity > 0)
                .with_timer(fmt::time::uptime())
                .compact()
                .init();
            Ok(None)
        }
        Target::File => {
            let path = log_file_path();
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            fmt()
                .with_env_filter(filter(verbosity.max(1))) // TUI: debug by default, it's a file
                .with_writer(Mutex::new(file))
                .with_ansi(false)
                .init();
            Ok(Some(path))
        }
    }
}

pub fn log_file_path() -> PathBuf {
    let p = sapu_core::paths::logs_dir().join("sapu.log");
    sapu_core::paths::rotate_log(&p, 10 * 1024 * 1024);
    p
}

/// Buffers one log event, then prints it above any progress bars.
#[derive(Default)]
struct TermWriter(Vec<u8>);

impl Write for TermWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for TermWriter {
    fn drop(&mut self) {
        if self.0.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.0);
        let line = text.trim_end_matches('\n');
        if io::stderr().is_terminal() && bars().println(line).is_ok() {
            return;
        }
        let _ = writeln!(io::stderr(), "{line}");
    }
}
