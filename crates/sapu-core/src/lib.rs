//! sapu-core: the engine. UI-agnostic so the CLI, the TUI,
//! and a future SwiftUI app can all share it.

pub mod advisor;
pub mod cache;
pub mod cleaner;
pub mod error;
pub mod icloud;
pub mod journal;
pub mod judge;
pub mod ml;
pub mod paths;
pub mod progress;
pub mod rules;
pub mod safety;
pub mod scanner;
pub mod tree;

pub use advisor::{advise, advise_with, Advice, Recommendation};
pub use error::{Error, Result};
pub use journal::Journal;
pub use judge::{Category, Judge, Judgement, Source, Verdict};
pub use progress::{Progress, Snapshot};
pub use scanner::{scan, ScanOptions};
pub use tree::{Kind, Node, ScanStats, Tree};
