//! Small formatting helpers shared by the CLI and the TUI.

use humansize::{format_size, DECIMAL};
use std::path::Path;

/// Sizes in decimal units, same as Finder (1 GB = 1,000,000,000 bytes).
pub fn size(bytes: u64) -> String {
    format_size(bytes, DECIMAL)
}

/// 1234567 -> "1,234,567"
pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Smooth horizontal bar using eighth-block characters.
pub fn bar(fraction: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let fraction = fraction.clamp(0.0, 1.0);
    let eighths = (fraction * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let mut s = "█".repeat(full);
    if full < width {
        s.push(PARTS[eighths % 8]);
        s.push_str(&" ".repeat(width - full - 1));
    }
    s
}

pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Show `~/…` instead of the full home path.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        // Show iCloud paths the way Finder does.
        let md = home.join("Library/Mobile Documents");
        if let Ok(rest) = path.strip_prefix(md.join("com~apple~CloudDocs")) {
            let r = rest.display().to_string();
            return if r.is_empty() { "iCloud Drive".into() } else { format!("iCloud Drive/{r}") };
        }
        if let Ok(rest) = path.strip_prefix(&md) {
            let mut parts = rest.components();
            if let Some(app) = parts.next() {
                let app = app.as_os_str().to_string_lossy();
                let app = app.rsplit('~').next().unwrap_or(&app).to_string();
                let r = parts.as_path().display().to_string();
                return if r.is_empty() { format!("iCloud ({app})") } else { format!("iCloud ({app})/{r}") };
            }
        }
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
        }
    }
    path.display().to_string()
}

/// Keep the end of a long string: "…/deep/folder/name".
pub fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn bars_have_fixed_width() {
        for f in [0.0, 0.13, 0.5, 0.99, 1.0] {
            assert_eq!(bar(f, 20).chars().count(), 20);
        }
    }
}
