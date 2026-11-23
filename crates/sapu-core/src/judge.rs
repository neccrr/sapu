//! The judgement system: decides, for any file or folder, whether it's
//! safe to remove and why.
//!
//! Rules are checked in priority order:
//! 1. **Keep** — macOS, your personal folders, keys, git history.
//! 2. **Known locations** — caches, logs, Xcode data, package-manager stores.
//! 3. **Project leftovers** — `node_modules`, Rust `target/`, Python venvs…
//!    (confirmed by a marker file next to them, e.g. `Cargo.toml`).
//! 4. **File types** — installers (`.dmg`, `.pkg`), old archives in Downloads.
//! 5. **Age + size** — big things nobody has touched in a long time.
//!
//! Anything no rule recognises gets no judgement (shown as neutral).

use crate::tree::Node;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Verdict {
    /// Rebuilt automatically or re-downloaded on demand. Nothing is lost.
    Safe,
    /// Almost certainly not needed (installers, old archives, venvs).
    Likely,
    /// Could be worth removing, but only you can tell.
    Review,
    /// Don't touch.
    Keep,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Safe => "Safe to delete",
            Verdict::Likely => "Probably safe",
            Verdict::Review => "Review",
            Verdict::Keep => "Keep",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Verdict::Safe => "Safe",
            Verdict::Likely => "Likely",
            Verdict::Review => "Review",
            Verdict::Keep => "Keep",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Category {
    Cache,
    Logs,
    Build,
    Dependencies,
    DevTools,
    Installer,
    Archive,
    Trash,
    Backup,
    App,
    OldFile,
    Personal,
    System,
    /// Judged by Sapu's model rather than a rule.
    AiGuess,
    /// Lives in iCloud.
    ICloud,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Cache => "Cache",
            Category::Logs => "Logs",
            Category::Build => "Build output",
            Category::Dependencies => "Dependencies",
            Category::DevTools => "Developer tools",
            Category::Installer => "Installer",
            Category::Archive => "Archive",
            Category::Trash => "Trash",
            Category::Backup => "Backup",
            Category::App => "App",
            Category::OldFile => "Old & large",
            Category::Personal => "Personal",
            Category::System => "System",
            Category::AiGuess => "AI guess",
            Category::ICloud => "iCloud",
        }
    }
}

/// What cleaning this item means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Action {
    /// Move the item itself to the Trash.
    TrashItem,
    /// Keep the folder, move everything inside it to the Trash.
    TrashContents,
    /// Already in the Trash: the user empties it in Finder.
    EmptyTrash,
    /// Sapu won't do it; the reason says how to do it properly.
    Manual,
    /// iCloud: remove the local download, keep it in iCloud. Nothing is deleted.
    Evict,
}

impl Action {
    pub fn is_automatic(self) -> bool {
        matches!(self, Action::TrashItem | Action::TrashContents | Action::Evict)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Judgement {
    pub verdict: Verdict,
    pub category: Category,
    pub reason: String,
    pub action: Action,
    /// For Keep items: still worth looking inside for leftovers
    /// (e.g. `node_modules` somewhere under ~/Documents).
    pub look_inside: bool,
    pub source: Source,
}

/// Who made the call.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum Source {
    /// A hand-written rule in this file. Certain.
    Rule,
    /// Sapu's trained model, with its confidence (0–1).
    Model { confidence: f32 },
}

impl Judgement {
    pub fn is_ai(&self) -> bool {
        matches!(self.source, Source::Model { .. })
    }
}

fn j(verdict: Verdict, category: Category, action: Action, reason: impl Into<String>) -> Option<Judgement> {
    Some(Judgement { verdict, category, reason: reason.into(), action, look_inside: false, source: Source::Rule })
}

/// Keep the folder itself, but recommendations may come from inside it.
fn keep_container(category: Category, reason: &str) -> Option<Judgement> {
    Some(Judgement {
        verdict: Verdict::Keep,
        category,
        reason: reason.into(),
        action: Action::Manual,
        look_inside: true,
        source: Source::Rule,
    })
}

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

pub struct Judge {
    home: PathBuf,
    now: u64,
    /// Folders synced with iCloud (inside them: never trash, only "remove download").
    icloud: Vec<PathBuf>,
}

impl Default for Judge {
    fn default() -> Self {
        Self::new()
    }
}

impl Judge {
    pub fn new() -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Self::with(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/nonexistent")), now)
    }

    pub fn with(home: PathBuf, now: u64) -> Self {
        let home = home.canonicalize().unwrap_or(home);
        let icloud = crate::icloud::roots(&home);
        Self { home, now, icloud }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    fn age_days(&self, node: &Node) -> u64 {
        if node.modified == 0 {
            return 0;
        }
        self.now.saturating_sub(node.modified) / 86_400
    }

    /// Judge one item. `path` is its full path, `node` its scan data.
    pub fn judge(&self, path: &Path, node: &Node) -> Option<Judgement> {
        use Action::*;
        use Category::*;
        use Verdict::*;

        let name = &*node.name;
        let lower = name.to_lowercase();
        let is_dir = node.is_dir();
        let age = self.age_days(node);

        // ── Outside home ────────────────────────────────────────────
        let Ok(rel) = path.strip_prefix(&self.home) else {
            if ["/System", "/usr", "/bin", "/sbin", "/private", "/Library", "/opt", "/cores", "/etc", "/var"]
                .iter()
                .any(|p| path.starts_with(p))
            {
                return j(Keep, System, Manual, "Part of macOS. Managed by the system.");
            }
            if path.starts_with("/Applications") && lower.ends_with(".app") {
                return j(Review, App, TrashItem, "App. Remove it if you no longer use it.");
            }
            return None;
        };
        let r = rel.to_string_lossy();
        if r.is_empty() {
            return None;
        }

        // ── 1. Keep ─────────────────────────────────────────────────
        if r.starts_with("Library/Keychains") || r.starts_with(".ssh") || r.starts_with(".gnupg") {
            return j(Keep, System, Manual, "Passwords and keys.");
        }
        if lower.ends_with(".photoslibrary") {
            return j(Keep, Personal, Manual, "Photos library. Manage it inside the Photos app.");
        }
        if name == ".git" {
            return j(Keep, Personal, Manual, "A project's version history.");
        }

        // ── iCloud: deleting here deletes everywhere, so only offer to remove downloads ──
        if crate::icloud::in_icloud(path, &self.icloud) {
            if self.icloud.iter().any(|root| root == path) {
                return keep_container(ICloud, "Synced with iCloud. Recommendations here only remove downloads; deleting needs your explicit consent.");
            }
            if node.size >= 50 * MB && age >= 30 && crate::icloud::is_downloaded(node) {
                return j(
                    Safe,
                    ICloud,
                    Evict,
                    format!(
                        "In iCloud and downloaded here, not opened for {}. Remove the download to free {} on this Mac; it stays in iCloud and comes back when you open it.",
                        human_age(age),
                        human_size(node.size)
                    ),
                );
            }
            let reason =
                "Synced with iCloud. Deleting it removes it from all your devices, so Sapu asks for consent first.";
            return if is_dir { keep_container(ICloud, reason) } else { j(Keep, ICloud, Manual, reason) };
        }

        // ── 2. Known locations (exact paths in home) ────────────────
        match r.as_ref() {
            "Library" => return keep_container(System, "Settings and data for all your apps."),
            "Desktop" | "Documents" | "Downloads" | "Pictures" | "Movies" | "Music" => {
                return keep_container(Personal, "Your personal files. Sapu never cleans this on its own.")
            }
            "Library/Caches" => return j(Safe, Cache, TrashContents, "App caches. Apps rebuild them when needed."),
            "Library/Logs" => return j(Safe, Logs, TrashContents, "Old app logs and crash reports."),
            "Library/Developer/Xcode/DerivedData" => {
                return j(Safe, Build, TrashContents, "Xcode build files. Rebuilt on the next build.")
            }
            "Library/Developer/CoreSimulator/Caches" => {
                return j(Safe, Cache, TrashContents, "Simulator caches. Rebuilt when needed.")
            }
            "Library/Developer/Xcode/iOS DeviceSupport" | "Library/Developer/Xcode/watchOS DeviceSupport" => {
                return j(
                    Likely,
                    DevTools,
                    TrashContents,
                    "Debug files for older iOS versions. Downloaded again when you plug in a device.",
                )
            }
            "Library/Developer/Xcode/Archives" => {
                return j(Review, DevTools, TrashContents, "Old app builds. Keep any you might re-submit.")
            }
            "Library/Developer/CoreSimulator/Devices" => {
                return j(
                    Review,
                    DevTools,
                    Manual,
                    "iOS simulators. Remove unused ones with `xcrun simctl delete unavailable`.",
                )
            }
            "Library/Application Support/MobileSync/Backup" => {
                return j(
                    Review,
                    Backup,
                    Manual,
                    "iPhone/iPad backups. Delete old ones in Finder → your device → Manage Backups.",
                )
            }
            "Library/Containers/com.docker.docker" => {
                return j(Review, DevTools, Manual, "Docker's disk image. Free space with `docker system prune`.")
            }
            ".rustup/toolchains" => {
                return j(
                    Review,
                    DevTools,
                    Manual,
                    "Rust toolchains. Remove old ones with `rustup toolchain uninstall`.",
                )
            }
            ".Trash" | ".local/share/Trash" => {
                return j(Safe, Trash, EmptyTrash, "Already in the Trash. Empty it in Finder to free the space.")
            }
            ".npm/_cacache"
            | ".cargo/registry/cache"
            | ".cargo/registry/src"
            | ".cache"
            | ".gradle/caches"
            | ".pnpm-store"
            | "Library/pnpm/store"
            | ".bun/install/cache"
            | ".yarn/cache"
            | ".cocoapods/repos"
            | ".android/cache"
            | ".expo" => {
                return j(Safe, Cache, TrashContents, "Download cache for developer tools. Re-downloaded when needed.")
            }
            ".m2/repository" => {
                return j(Likely, Cache, TrashContents, "Maven downloads. Re-downloaded on the next build.")
            }
            _ => {}
        }

        // Inside known cache/log folders (when browsing into them).
        if r.starts_with("Library/Caches/") {
            return j(Safe, Cache, TrashItem, format!("Cache for {name}. Rebuilt when needed."));
        }
        if r.starts_with("Library/Logs/") {
            return j(Safe, Logs, TrashItem, "Old logs.");
        }
        if r.starts_with("Library/Developer/Xcode/DerivedData/") {
            return j(Safe, Build, TrashItem, "Xcode build files for one project.");
        }

        // ── 3. Project leftovers (folders) ──────────────────────────
        if is_dir {
            let sibling = |f: &str| path.parent().is_some_and(|p| p.join(f).exists());
            match name {
                "node_modules" => {
                    return j(Safe, Dependencies, TrashItem, "JavaScript packages. Reinstall with `npm install`.")
                }
                "target" if sibling("Cargo.toml") => {
                    return j(Safe, Build, TrashItem, "Rust build output. Rebuilt by `cargo build`.")
                }
                "build" | ".gradle"
                    if sibling("build.gradle") || sibling("build.gradle.kts") || sibling("settings.gradle") =>
                {
                    return j(Safe, Build, TrashItem, "Gradle build output. Rebuilt on the next build.")
                }
                "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" => {
                    return j(Safe, Build, TrashItem, "Python cache. Rebuilt automatically.")
                }
                ".venv" | "venv" | "env" if path.join("pyvenv.cfg").exists() => {
                    return j(
                        Likely,
                        Dependencies,
                        TrashItem,
                        "Python virtual environment. Recreate with `python -m venv` and `pip install`.",
                    )
                }
                "Pods" if sibling("Podfile") => {
                    return j(Safe, Dependencies, TrashItem, "CocoaPods. Reinstall with `pod install`.")
                }
                ".next" | ".nuxt" | ".turbo" | ".parcel-cache" | ".svelte-kit" | ".angular" | ".vercel"
                    if sibling("package.json") =>
                {
                    return j(Safe, Build, TrashItem, "Web framework build cache. Rebuilt automatically.")
                }
                _ => {}
            }
            if lower.ends_with(".app") {
                return j(Review, App, TrashItem, "App. Remove it if you no longer use it.");
            }
        }

        // ── 4. File types ───────────────────────────────────────────
        if !is_dir {
            let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let in_downloads = r.starts_with("Downloads/");
            match ext.as_str() {
                "dmg" | "pkg" | "mpkg" | "iso" | "xip" => {
                    let when =
                        if age == 0 { "downloaded today".to_string() } else { format!("{} old", human_age(age)) };
                    return j(
                        Likely,
                        Installer,
                        TrashItem,
                        format!("Installer, {when}. Not needed once the app is installed."),
                    );
                }
                "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" | "bz2" | "xz" if in_downloads && age >= 30 => {
                    return j(
                        Likely,
                        Archive,
                        TrashItem,
                        format!("Downloaded archive, {} old. Not needed if you already unpacked it.", human_age(age)),
                    )
                }
                "log" => return j(Likely, Logs, TrashItem, "Log file."),
                _ => {}
            }
        }

        // ── 5. Age + size ───────────────────────────────────────────
        let in_library = r.starts_with("Library/");
        let top_of_downloads = rel.parent().is_some_and(|p| p == Path::new("Downloads"));
        if top_of_downloads && age >= 90 {
            return j(Review, OldFile, TrashItem, format!("In Downloads and untouched for {}.", human_age(age)));
        }
        if !in_library && node.size >= GB && age >= 365 {
            return j(
                Review,
                OldFile,
                TrashItem,
                format!("{} that nobody has touched in {}.", human_size(node.size), human_age(age)),
            );
        }
        if !in_library && !is_dir && node.size >= 500 * MB && age >= 180 {
            return j(Review, OldFile, TrashItem, format!("Large file, untouched for {}.", human_age(age)));
        }
        None
    }
}

pub fn human_age(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day".into(),
        2..=44 => format!("{days} days"),
        45..=364 => format!("{} months", (days as f64 / 30.4).round() as u64),
        _ => {
            let y = days as f64 / 365.0;
            if y < 1.5 {
                "1 year".into()
            } else {
                format!("{:.0} years", y)
            }
        }
    }
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::Kind;

    fn node(name: &str, kind: Kind, size: u64, age_days: u64, now: u64) -> Node {
        Node {
            name: name.into(),
            kind,
            size,
            files: 1,
            incomplete: false,
            modified: now - age_days * 86_400,
            logical: size,
            children: vec![],
        }
    }

    #[test]
    fn verdicts() {
        let now = 2_000_000_000;
        let home = std::env::temp_dir().join(format!("sapu-judge-{}", std::process::id()));
        std::fs::create_dir_all(home.join("code/app")).unwrap();
        std::fs::write(home.join("code/app/Cargo.toml"), "").unwrap();
        let home = home.canonicalize().unwrap();
        let jd = Judge::with(home.clone(), now);
        let v = |rel: &str, kind, size, age| {
            let n = node(Path::new(rel).file_name().unwrap().to_str().unwrap(), kind, size, age, now);
            jd.judge(&home.join(rel), &n).map(|j| j.verdict)
        };

        assert_eq!(v("Library/Caches", Kind::Dir, MB, 1), Some(Verdict::Safe));
        assert_eq!(v("code/app/target", Kind::Dir, GB, 1), Some(Verdict::Safe));
        assert_eq!(v("code/other/target", Kind::Dir, GB, 1), None, "no Cargo.toml next to it");
        assert_eq!(v("Downloads/Zoom.dmg", Kind::File, 80 * MB, 3), Some(Verdict::Likely));
        assert_eq!(v("Downloads/notes.pdf", Kind::File, MB, 200), Some(Verdict::Review));
        assert_eq!(v("Downloads/notes.pdf", Kind::File, MB, 10), None);
        assert_eq!(v("Documents", Kind::Dir, GB, 1), Some(Verdict::Keep));
        assert_eq!(v("Documents/old-project", Kind::Dir, 3 * GB, 800), Some(Verdict::Review));
        assert_eq!(v("Pictures/Photos Library.photoslibrary", Kind::Dir, 50 * GB, 800), Some(Verdict::Keep));
        assert_eq!(v("x", Kind::File, 10, 1), None);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ages() {
        assert_eq!(human_age(0), "today");
        assert_eq!(human_age(10), "10 days");
        assert_eq!(human_age(91), "3 months");
        assert_eq!(human_age(800), "2 years");
    }
}
