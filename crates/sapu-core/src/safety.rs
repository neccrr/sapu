//! Path guards. Two levels:
//! - [`check_cleanup`]: strict, for automatic rule-based cleaning.
//! - [`check_manual`]: for things the user picked by hand in the UI
//!   (still always goes to the Trash).

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Folders inside home that automatic cleaning never touches.
const PROTECTED_IN_HOME: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    "Library/Mail",
    "Library/Messages",
    "Library/Keychains",
    "Library/Mobile Documents", // iCloud Drive
    ".ssh",
];

/// System locations a user can never trash from Sapu.
const SYSTEM: &[&str] = &["/System", "/usr", "/bin", "/sbin", "/private", "/etc", "/var", "/dev", "/cores", "/Library"];

// ── Your own "never touch" list ─────────────────────────────────────
// One path per line in ~/Library/Application Support/sapu/protected.txt
// (`~/` allowed, `#` comments). A protected path can't be removed, and
// neither can any folder that *contains* it.

pub fn protected_file() -> PathBuf {
    crate::paths::protected_file()
}

pub fn user_protected() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    std::fs::read_to_string(protected_file())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(l),
        })
        .collect()
}

pub fn protect(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let path = path.canonicalize()?;
    tracing::info!(path = %path.display(), "added to protected list");
    if user_protected().contains(&path) {
        return Ok(());
    }
    std::fs::create_dir_all(crate::paths::safety_dir())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(protected_file())?;
    writeln!(f, "{}", path.display())
}

pub fn unprotect(path: &Path) -> std::io::Result<bool> {
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let list = user_protected();
    if !list.contains(&target) {
        return Ok(false);
    }
    let kept: Vec<String> = list.iter().filter(|p| **p != target).map(|p| p.display().to_string()).collect();
    std::fs::write(protected_file(), kept.join("\n") + "\n")?;
    Ok(true)
}

/// Is `path` protected, or does it contain something protected?
pub fn touches_protected(path: &Path, protected: &[PathBuf]) -> Option<PathBuf> {
    protected.iter().find(|p| path.starts_with(p) || p.starts_with(path)).cloned()
}

fn check_user_list(path: &Path) -> Result<()> {
    if let Some(p) = touches_protected(path, &user_protected()) {
        return Err(Error::Protected(p));
    }
    Ok(())
}

/// Strictly inside $HOME and outside every protected folder.
pub fn check_cleanup(path: &Path, home: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    let home = home.canonicalize()?;
    if path == home || !path.starts_with(&home) {
        return Err(Error::UnsafePath(path));
    }
    if PROTECTED_IN_HOME.iter().any(|p| path.starts_with(home.join(p))) {
        return Err(Error::UnsafePath(path));
    }
    // Automatic cleaning never touches iCloud (consent is per item, by hand).
    if crate::icloud::in_icloud(&path, &crate::icloud::roots(&home)) {
        return Err(Error::ICloudConsent(path));
    }
    check_user_list(&path)?;
    Ok(path)
}

/// Explicit permission to delete something that lives in iCloud.
///
/// Deleting an iCloud item removes it from every device, so it's never done
/// implicitly: callers must construct this deliberately, after the person has
/// agreed in a consent dialog or typed confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ICloud {
    /// Refuse anything in iCloud (the default everywhere).
    Refuse,
    /// The person explicitly agreed to delete this iCloud item everywhere.
    ConsentGiven,
}

/// For a hand-picked item: block the root, home itself, anything directly
/// in `/`, the standard home folders themselves (their *contents* are fine),
/// `~/Library` itself, Keychains, `.ssh`, and system locations.
/// Anything in iCloud is refused (see [`check_manual_icloud`]).
pub fn check_manual(path: &Path, home: &Path) -> Result<PathBuf> {
    check_manual_icloud(path, home, ICloud::Refuse)
}

/// Like [`check_manual`], with an explicit decision about iCloud items.
pub fn check_manual_icloud(path: &Path, home: &Path, icloud: ICloud) -> Result<PathBuf> {
    let checked = check_manual_inner(path, home)?;
    let roots = crate::icloud::roots(&home.canonicalize()?);
    if crate::icloud::in_icloud(&checked, &roots) {
        // Never whole iCloud containers: Mobile Documents, the iCloud Drive
        // folder, an app's iCloud folder, or synced Desktop/Documents themselves.
        let mobile = crate::icloud::mobile_documents(&home.canonicalize()?);
        let container_level = checked.strip_prefix(&mobile).is_ok_and(|r| r.components().count() <= 1);
        if roots.contains(&checked) || container_level {
            return Err(Error::UnsafePath(checked));
        }
        if icloud != ICloud::ConsentGiven {
            return Err(Error::ICloudConsent(checked));
        }
        tracing::warn!(path = %checked.display(), "iCloud delete: consent given");
    }
    Ok(checked)
}

fn check_manual_inner(path: &Path, home: &Path) -> Result<PathBuf> {
    // Don't canonicalize the item itself: trashing a symlink should trash the link.
    let parent = path.parent().ok_or_else(|| Error::UnsafePath(path.to_path_buf()))?.canonicalize()?;
    let path = parent.join(path.file_name().unwrap_or_default());
    let home = home.canonicalize()?;
    let deny = || Err(Error::UnsafePath(path.clone()));

    if path == Path::new("/") || path == home || parent == Path::new("/") {
        return deny();
    }
    if path.starts_with(&home) {
        let is_standard_folder = PROTECTED_IN_HOME.iter().any(|p| path == home.join(p));
        let is_library = path == home.join("Library");
        let in_secrets = path.starts_with(home.join("Library/Keychains")) || path.starts_with(home.join(".ssh"));
        if is_standard_folder || is_library || in_secrets {
            return deny();
        }
        check_user_list(&path)?;
        return Ok(path);
    }
    if SYSTEM.iter().any(|s| path.starts_with(s)) {
        return deny();
    }
    check_user_list(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_rejects_home_itself_and_outside() {
        let home = dirs::home_dir().unwrap();
        assert!(check_cleanup(&home, &home).is_err());
        assert!(check_cleanup(Path::new("/"), &home).is_err());
    }

    #[test]
    fn protection_covers_ancestors() {
        let list = vec![PathBuf::from("/u/me/Documents/thesis")];
        assert!(touches_protected(Path::new("/u/me/Documents/thesis/ch1.docx"), &list).is_some());
        assert!(touches_protected(Path::new("/u/me/Documents"), &list).is_some(), "folder containing it");
        assert!(touches_protected(Path::new("/u/me/Documents/other"), &list).is_none());
    }

    #[test]
    fn icloud_needs_consent() {
        let home = std::env::temp_dir().join(format!("sapu-icloud-{}", std::process::id()));
        let drive = home.join("Library/Mobile Documents/com~apple~CloudDocs");
        std::fs::create_dir_all(drive.join("Notes")).unwrap();
        let item = drive.join("Notes");
        assert!(matches!(check_manual(&item, &home), Err(Error::ICloudConsent(_))));
        assert!(check_manual_icloud(&item, &home, ICloud::ConsentGiven).is_ok());
        assert!(check_manual_icloud(&drive, &home, ICloud::ConsentGiven).is_err(), "never the Drive itself");
        assert!(check_cleanup(&item, &home).is_err(), "automatic cleaning never");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn manual_rejects_system_and_root_items() {
        let home = dirs::home_dir().unwrap();
        assert!(check_manual(Path::new("/usr/bin"), &home).is_err());
        assert!(check_manual(Path::new("/tmp"), &home).is_err());
        assert!(check_manual(&home, &home).is_err());
    }
}
