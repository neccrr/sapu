//! Which apps are installed, so the model can spot data left behind by apps
//! you removed (`~/Library/Application Support/Evernote` with no Evernote).
//!
//! Matching is by name words: an app-data folder belongs to an installed app
//! when one of its distinctive words ("evernote", "slackmacgap") matches a
//! word from an installed app's name or bundle id. Apple's own folders and
//! anything we can't tell are treated as "unknown", never as leftovers.

use super::features::tokens;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Where app data lives, relative to home.
const DATA_ROOTS: [&str; 3] = ["Application Support", "Containers", "Group Containers"];

/// Words that say nothing about which app a folder belongs to.
const GENERIC: &[&str] = &[
    "com",
    "net",
    "org",
    "io",
    "co",
    "app",
    "apps",
    "group",
    "desktop",
    "mac",
    "macos",
    "osx",
    "inc",
    "ltd",
    "the",
    "helper",
    "data",
    "shared",
    "stable",
    "user",
    "beta",
    "pro",
    "free",
    "lite",
    "plus",
    "client",
    "launcher",
    "agent",
    "service",
    "services",
    "support",
    "for",
    "and",
    "google",
    "microsoft",
    "apple",
    "adobe",
    "jetbrains",
    "mozilla",
    "caches",
    "cache",
    "crash",
    "reporter",
    "updater",
    "update",
    "dev",
    "studio",
];

/// App Support folders macOS itself makes (they have no matching .app).
const SYSTEM: &[&str] = &[
    "addressbook",
    "callhistorydb",
    "callhistorytransactions",
    "cloudocs",
    "clouddocs",
    "crashreporter",
    "dock",
    "fileprovider",
    "icloud",
    "knowledge",
    "mobilesync",
    "spotlight",
    "syncservices",
    "caches",
    "accounts",
    "animoji",
    "contextstoreagent",
    "dmd",
    "familycircle",
    "sapu",
];

/// Apps that ship as a suite under one shared folder name.
const SUITES: &[(&str, &[&str])] = &[("office", &["word", "excel", "powerpoint", "outlook", "onenote"])];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStatus {
    Installed,
    Gone,
}

#[derive(Debug, Clone, Default)]
pub struct Apps {
    words: HashSet<String>,
    /// false: we don't know what's installed, so nothing counts as gone.
    known: bool,
    /// true: treat every app as installed (for the hand-labelled seed).
    all_installed: bool,
}

fn distinctive(s: &str) -> Vec<String> {
    tokens(s).into_iter().filter(|t| t.len() >= 3 && !GENERIC.contains(&t.as_str())).collect()
}

/// Drop a leading team id ("UBF8T346G9.Office" → "Office").
fn strip_team_id(name: &str) -> &str {
    match name.split_once('.') {
        Some((id, rest)) if id.len() == 10 && id.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) => rest,
        _ => name,
    }
}

impl Apps {
    /// Nothing known: never reports a leftover.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Every app counts as installed (hand-labelled data with no machine behind it).
    pub fn all_installed() -> Self {
        Self { all_installed: true, ..Self::default() }
    }

    /// From app names and bundle ids ("Slack", "com.tinyspeck.slackmacgap").
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Self {
        let mut words = HashSet::new();
        for n in names {
            words.extend(distinctive(n.as_ref()));
        }
        Self { known: !words.is_empty(), words, all_installed: false }
    }

    /// Look in the usual app folders. Cheap: one directory listing each,
    /// plus each app's Info.plist for its bundle id when it's plain XML.
    pub fn scan(home: &Path) -> Self {
        let dirs = [
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
            home.join("Applications"),
            PathBuf::from("/Applications/Setapp"),
        ];
        let mut names = Vec::new();
        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Some(stem) = name.strip_suffix(".app") else { continue };
                names.push(stem.to_string());
                if let Some(id) = bundle_id(&e.path()) {
                    names.push(id);
                }
            }
        }
        Self::from_names(&names)
    }

    /// For an item inside an app's data folder, whether that app is installed.
    /// `None` when it's not in an app data folder or we can't tell.
    pub fn status(&self, path: &Path, home: &Path) -> Option<AppStatus> {
        let rel = path.strip_prefix(home).ok()?;
        let mut comps = rel.components().map(|c| c.as_os_str().to_string_lossy());
        if comps.next()? != "Library" || !DATA_ROOTS.contains(&comps.next()?.as_ref()) {
            return None;
        }
        let folder = comps.next()?;
        if self.all_installed {
            return Some(AppStatus::Installed);
        }
        if !self.known {
            return None;
        }
        let folder = strip_team_id(&folder);
        let lower = folder.to_lowercase();
        if lower.starts_with("com.apple.") || lower.starts_with("group.com.apple.") || SYSTEM.contains(&&*lower) {
            return None;
        }
        let words = distinctive(folder);
        if words.is_empty() {
            return None;
        }
        let installed = words.iter().any(|w| {
            self.words.iter().any(|a| a == w || (a.len() >= 4 && w.len() >= 4 && (a.contains(w) || w.contains(a))))
                || SUITES.iter().any(|(s, members)| s == w && members.iter().any(|m| self.words.contains(*m)))
        });
        Some(if installed { AppStatus::Installed } else { AppStatus::Gone })
    }
}

/// `CFBundleIdentifier` from an app's Info.plist, if it's XML (binary plists are skipped).
fn bundle_id(app: &Path) -> Option<String> {
    let text = std::fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    let after = text.split("<key>CFBundleIdentifier</key>").nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_leftovers() {
        let home = Path::new("/Users/t");
        let apps = Apps::from_names(&["Slack", "com.tinyspeck.slackmacgap", "Microsoft Word", "Visual Studio Code"]);
        let s = |p: &str| apps.status(&home.join(p), home);
        assert_eq!(s("Library/Application Support/Slack/Cache"), Some(AppStatus::Installed));
        assert_eq!(s("Library/Containers/com.tinyspeck.slackmacgap"), Some(AppStatus::Installed));
        assert_eq!(s("Library/Group Containers/UBF8T346G9.Office"), Some(AppStatus::Installed));
        assert_eq!(s("Library/Application Support/Code"), Some(AppStatus::Installed));
        assert_eq!(s("Library/Application Support/Evernote"), Some(AppStatus::Gone));
        assert_eq!(s("Library/Containers/com.skype.skype"), Some(AppStatus::Gone));
        // Apple's own and system folders are never leftovers.
        assert_eq!(s("Library/Containers/com.apple.mail"), None);
        assert_eq!(s("Library/Application Support/CrashReporter"), None);
        // Not app data at all.
        assert_eq!(s("Documents/Evernote"), None);
        // Nothing known about installed apps: never a leftover.
        assert_eq!(Apps::unknown().status(&home.join("Library/Application Support/Evernote"), home), None);
    }
}
