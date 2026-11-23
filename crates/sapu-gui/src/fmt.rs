use humansize::{format_size, DECIMAL};
use std::path::Path;

/// Decimal units, same as Finder.
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

pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

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

pub fn truncate_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 2 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

/// "5 min ago", "3 h ago", "2 days ago".
pub fn ago(epoch_secs: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let d = now.saturating_sub(epoch_secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}
