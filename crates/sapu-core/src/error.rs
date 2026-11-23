use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("could not move to Trash: {0}")]
    Trash(#[from] trash::Error),

    #[error("refusing to touch unsafe path: {0}")]
    UnsafePath(PathBuf),

    #[error("this item has to be cleaned by hand: {0}")]
    Manual(String),

    #[error("protected by you: {0}")]
    Protected(PathBuf),

    #[error("skipped, it changed since the scan: {0}")]
    Changed(String),

    #[error(
        "{0} is in iCloud: deleting it removes it from all your devices, so Sapu needs your explicit consent first"
    )]
    ICloudConsent(PathBuf),

    #[error("home directory not found")]
    NoHome,
}

pub type Result<T> = std::result::Result<T, Error>;
