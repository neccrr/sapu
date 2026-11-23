use std::path::{Path, PathBuf};

/// A known-safe place whose *contents* can be cleared.
#[derive(Debug, Clone)]
pub struct CleanupRule {
    pub id: &'static str,
    pub name: &'static str,
    /// Relative to the user's home folder.
    pub rel_path: &'static str,
    pub description: &'static str,
}

impl CleanupRule {
    pub fn resolve(&self, home: &Path) -> PathBuf {
        home.join(self.rel_path)
    }
}

/// Built-in rules. Keep this list conservative: only things apps rebuild on their own.
pub fn default_rules() -> Vec<CleanupRule> {
    vec![
        CleanupRule {
            id: "user-caches",
            name: "User caches",
            rel_path: "Library/Caches",
            description: "App caches; apps recreate them as needed.",
        },
        CleanupRule {
            id: "user-logs",
            name: "User logs",
            rel_path: "Library/Logs",
            description: "Old diagnostic and app logs.",
        },
        CleanupRule {
            id: "xcode-derived-data",
            name: "Xcode DerivedData",
            rel_path: "Library/Developer/Xcode/DerivedData",
            description: "Xcode build products; rebuilt on next build.",
        },
        CleanupRule {
            id: "cargo-registry-cache",
            name: "Cargo download cache",
            rel_path: ".cargo/registry/cache",
            description: "Downloaded crate archives; re-downloaded when needed.",
        },
        CleanupRule {
            id: "npm-cache",
            name: "npm cache",
            rel_path: ".npm/_cacache",
            description: "npm package cache; re-downloaded when needed.",
        },
    ]
}
