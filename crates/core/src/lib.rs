//! Application-level duplicate-file domain logic.
//!
//! This crate deliberately keeps `fclones` and operating-system details at its
//! boundary: callers work with `PathBuf` based models and cannot construct an
//! invalid cleanup selection through the public selection API.

mod cleanup;
mod models;
mod scanner;

pub use cleanup::{
    CleanupAction, CleanupError, CleanupFailure, CleanupOutcome, CleanupPlan, CleanupPreflight,
    CleanupProgressPhase, CleanupProgressUpdate, CleanupService, FileSnapshot,
};
pub use models::{
    DuplicateFile, DuplicateFileId, DuplicateGroup, GroupId, ScanConfig, ScanPath, ScanResult,
    ScanSummary, SelectionError, SelectionPolicy,
};
pub use scanner::{CancellationToken, DuplicateScanner, FclonesScanner, ScanError, ScanEvent};
