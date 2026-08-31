//! SQLite-backed scan history and application persistence.
//!
//! Paths are deliberately stored as BLOBs.  On Unix this is the byte sequence
//! of the `OsStr`, so a database round-trip does not lose non-UTF-8 file names.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub use dupekit_core::{
    CleanupOutcome, DuplicateFile, DuplicateFileId, DuplicateGroup, GroupId, ScanPath, ScanSummary,
};
use thiserror::Error;

pub type ScanId = i64;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("scan {0} does not exist")]
    ScanNotFound(ScanId),
    #[error("file {0:?} does not exist")]
    FileNotFound(DuplicateFileId),
    #[error("selecting file {file_id:?} would select every copy in its duplicate group")]
    LastCopySelected { file_id: DuplicateFileId },
    #[error("system time is outside SQLite's supported range")]
    InvalidTime,
    #[error("path data was not encoded for this operating system")]
    IncompatiblePath,
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}
impl ScanStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: String) -> std::result::Result<Self, rusqlite::Error> {
        match value.as_str() {
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(rusqlite::Error::InvalidColumnType(
                0,
                "status".into(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewScan {
    pub name: Option<String>,
    pub started_at: SystemTime,
    pub paths: Vec<ScanPath>,
    pub settings: ScanSettings,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSettings {
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub cache: bool,
}
impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            min_size: Some(1_048_576),
            max_size: None,
            cache: true,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub id: ScanId,
    pub name: Option<String>,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
    pub status: ScanStatus,
    pub summary: Option<ScanSummary>,
    pub paths: Vec<ScanPath>,
    pub settings: ScanSettings,
    pub settings_recorded: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupAction {
    pub id: i64,
    pub scan_id: ScanId,
    pub created_at: SystemTime,
    pub action: String,
    pub affected_files: u64,
    pub recovered_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCleanupAction {
    pub created_at: SystemTime,
    pub action: String,
    pub affected_files: u64,
    pub recovered_bytes: u64,
}
/// A cleanup audit entry with its outcome.  Failed targets are retained rather
/// than being folded into a misleading "success" count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupHistoryEntry {
    pub action: CleanupAction,
    pub attempted_files: u64,
    pub failures: Vec<CleanupFailureRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupFailureRecord {
    pub path: PathBuf,
    pub message: String,
}

mod codec;
mod database;
mod queries;

pub use database::Database;

#[cfg(test)]
mod tests;
