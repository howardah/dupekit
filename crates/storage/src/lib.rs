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
use rusqlite::{Connection, OptionalExtension, Transaction, params};
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

pub struct Database {
    connection: Connection,
}

impl Database {
    /// Opens (or creates) the application-owned database and applies migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        let db = Self { connection };
        db.migrate()?;
        Ok(db)
    }
    /// Creates an isolated migrated database, useful for tests and previews.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        let db = Self { connection };
        db.migrate()?;
        Ok(db)
    }
    /// Exposes the connection for diagnostics; prefer typed methods for persistence work.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Applies all known schema migrations. Calling this repeatedly is safe.
    pub fn migrate(&self) -> Result<()> {
        self.connection.execute_batch("PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS scans (id INTEGER PRIMARY KEY, name TEXT, started_at INTEGER NOT NULL, finished_at INTEGER, status TEXT NOT NULL CHECK(status IN ('running','completed','failed','cancelled')), duplicate_bytes INTEGER, duplicate_files INTEGER, duplicate_groups INTEGER);
            CREATE TABLE IF NOT EXISTS scan_paths (id INTEGER PRIMARY KEY, scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE, path BLOB NOT NULL, preferred INTEGER NOT NULL DEFAULT 0 CHECK(preferred IN (0,1)));
            CREATE TABLE IF NOT EXISTS duplicate_groups (id INTEGER PRIMARY KEY, scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE, file_size INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS duplicate_files (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL REFERENCES duplicate_groups(id) ON DELETE CASCADE, path BLOB NOT NULL, size INTEGER NOT NULL, modified_at INTEGER, selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0,1)));
            CREATE INDEX IF NOT EXISTS scan_paths_scan_id ON scan_paths(scan_id);
            CREATE INDEX IF NOT EXISTS duplicate_groups_scan_id ON duplicate_groups(scan_id);
            CREATE INDEX IF NOT EXISTS duplicate_files_group_id ON duplicate_files(group_id);
            CREATE TABLE IF NOT EXISTS cleanup_actions (id INTEGER PRIMARY KEY, scan_id INTEGER NOT NULL REFERENCES scans(id) ON DELETE CASCADE, created_at INTEGER NOT NULL, action TEXT NOT NULL, affected_files INTEGER NOT NULL, recovered_bytes INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS cleanup_actions_scan_id ON cleanup_actions(scan_id);
            INSERT OR IGNORE INTO schema_migrations(version) VALUES(2);")?;
        let version: i64 =
            self.connection
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                    row.get(0)
                })?;
        if version < 2 {
            self.connection
                .execute("ALTER TABLE scans ADD COLUMN duplicate_groups INTEGER", [])?;
            self.connection
                .execute("INSERT INTO schema_migrations(version) VALUES(2)", [])?;
        }
        if version < 3 {
            self.connection.execute_batch(
                "ALTER TABLE cleanup_actions ADD COLUMN attempted_files INTEGER NOT NULL DEFAULT 0;
                 CREATE TABLE cleanup_failures (id INTEGER PRIMARY KEY, cleanup_action_id INTEGER NOT NULL REFERENCES cleanup_actions(id) ON DELETE CASCADE, path BLOB NOT NULL, message TEXT NOT NULL);
                 CREATE INDEX cleanup_failures_action_id ON cleanup_failures(cleanup_action_id);
                INSERT INTO schema_migrations(version) VALUES(3);",
            )?;
        }
        if version < 4 {
            self.connection.execute_batch(
                "ALTER TABLE duplicate_files ADD COLUMN modified_at_nanos INTEGER;
                 INSERT INTO schema_migrations(version) VALUES(4);",
            )?;
        }
        if version < 5 {
            self.connection.execute_batch(
                "ALTER TABLE scans ADD COLUMN min_size INTEGER;
                 ALTER TABLE scans ADD COLUMN max_size INTEGER;
                 ALTER TABLE scans ADD COLUMN cache INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE scans ADD COLUMN settings_recorded INTEGER NOT NULL DEFAULT 0;
                 UPDATE scans SET min_size = 1048576 WHERE min_size IS NULL;
                 INSERT INTO schema_migrations(version) VALUES(5);",
            )?;
        }
        Ok(())
    }

    /// Starts a persisted scan and saves its configured scan paths atomically.
    pub fn create_scan(&mut self, scan: &NewScan) -> Result<ScanId> {
        let tx = self.connection.transaction()?;
        let id = insert_scan(&tx, scan)?;
        tx.commit()?;
        Ok(id)
    }
    /// Atomically replaces a scan's groups and marks it completed with its summary.
    /// IDs returned by [`Self::groups`] are database-owned and may be selected later.
    pub fn replace_results(
        &mut self,
        scan_id: ScanId,
        groups: &[DuplicateGroup],
        summary: &ScanSummary,
        finished_at: SystemTime,
    ) -> Result<()> {
        self.replace_results_and_load(scan_id, groups, summary, finished_at)
            .map(|_| ())
    }
    /// Atomically replaces a scan's result set and returns its database-owned
    /// IDs. If loading the replacement cannot complete, the transaction is
    /// rolled back so callers can safely keep displaying the old result set.
    pub fn replace_results_and_load(
        &mut self,
        scan_id: ScanId,
        groups: &[DuplicateGroup],
        summary: &ScanSummary,
        finished_at: SystemTime,
    ) -> Result<Vec<DuplicateGroup>> {
        self.replace_results_and_load_with_settings(scan_id, groups, summary, finished_at, None)
    }
    pub fn replace_results_and_load_with_settings(
        &mut self,
        scan_id: ScanId,
        groups: &[DuplicateGroup],
        summary: &ScanSummary,
        finished_at: SystemTime,
        settings: Option<ScanSettings>,
    ) -> Result<Vec<DuplicateGroup>> {
        let tx = self.connection.transaction()?;
        if tx
            .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(StorageError::ScanNotFound(scan_id));
        }
        tx.execute("DELETE FROM duplicate_groups WHERE scan_id=?1", [scan_id])?;
        for group in groups {
            let group_id = insert_group(&tx, scan_id, group)?;
            for file in &group.files {
                insert_file(&tx, group_id, file, group.is_selected(file.id))?;
            }
        }
        tx.execute("UPDATE scans SET status='completed', finished_at=?2, duplicate_bytes=?3, duplicate_files=?4, duplicate_groups=?5 WHERE id=?1", params![scan_id, time_to_millis(finished_at)?, u64_to_i64(summary.recoverable_bytes)?, u64_to_i64(summary.duplicate_files)?, u64_to_i64(summary.duplicate_groups)?])?;
        if let Some(settings) = settings {
            tx.execute(
                "UPDATE scans SET min_size=?2,max_size=?3,cache=?4,settings_recorded=1 WHERE id=?1",
                params![
                    scan_id,
                    settings.min_size.map(u64_to_i64).transpose()?,
                    settings.max_size.map(u64_to_i64).transpose()?,
                    settings.cache as i64
                ],
            )?;
        }
        let loaded = groups_from_connection(&tx, scan_id)?;
        tx.commit()?;
        Ok(loaded)
    }
    /// Marks a scan as failed or cancelled when it has no completed result set.
    pub fn finish_scan(
        &self,
        scan_id: ScanId,
        status: ScanStatus,
        finished_at: SystemTime,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE scans SET status=?2, finished_at=?3 WHERE id=?1",
            params![scan_id, status.as_str(), time_to_millis(finished_at)?],
        )?;
        if changed == 0 {
            Err(StorageError::ScanNotFound(scan_id))
        } else {
            Ok(())
        }
    }
    /// Lists persisted scans newest first, including their configured paths.
    pub fn scans(&self) -> Result<Vec<Scan>> {
        let mut stmt = self.connection.prepare("SELECT id,name,started_at,finished_at,status,duplicate_bytes,duplicate_files,duplicate_groups,min_size,max_size,cache,settings_recorded FROM scans ORDER BY started_at DESC,id DESC")?;
        let basics = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, ScanId>(0)?,
                    r.get(1)?,
                    millis_to_time(r.get(2)?)?,
                    r.get::<_, Option<i64>>(3)?
                        .map(millis_to_time)
                        .transpose()?,
                    ScanStatus::parse(r.get(4)?)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                    r.get::<_, i64>(10)? != 0,
                    r.get::<_, i64>(11)? != 0,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        basics
            .into_iter()
            .map(
                |(
                    id,
                    name,
                    started_at,
                    finished_at,
                    status,
                    bytes,
                    files,
                    groups,
                    min_size,
                    max_size,
                    cache,
                    settings_recorded,
                )| {
                    let summary = match bytes.zip(files).zip(groups) {
                        Some(((b, f), g)) => Some(ScanSummary {
                            duplicate_groups: i64_to_u64(g)?,
                            recoverable_bytes: i64_to_u64(b)?,
                            duplicate_files: i64_to_u64(f)?,
                        }),
                        None => None,
                    };
                    Ok(Scan {
                        id,
                        name,
                        started_at,
                        finished_at,
                        status,
                        summary,
                        paths: self.scan_paths(id)?,
                        settings: ScanSettings {
                            min_size: min_size.map(i64_to_u64).transpose()?,
                            max_size: max_size.map(i64_to_u64).transpose()?,
                            cache,
                        },
                        settings_recorded,
                    })
                },
            )
            .collect()
    }
    /// Reopens the metadata and configuration for one historic scan.
    pub fn scan(&self, scan_id: ScanId) -> Result<Scan> {
        self.scans()?
            .into_iter()
            .find(|s| s.id == scan_id)
            .ok_or(StorageError::ScanNotFound(scan_id))
    }
    /// Loads all duplicate groups, restoring persisted selection state.
    pub fn groups(&self, scan_id: ScanId) -> Result<Vec<DuplicateGroup>> {
        groups_from_connection(&self.connection, scan_id)
    }
    /// Persists one checkbox change while refusing to select every copy in a group.
    pub fn set_selected(&mut self, file_id: DuplicateFileId, selected: bool) -> Result<()> {
        let tx = self.connection.transaction()?;
        let db_file_id = u64_to_i64(file_id.0)?;
        let group_id: Option<i64> = tx
            .query_row(
                "SELECT group_id FROM duplicate_files WHERE id=?1",
                [db_file_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(group_id) = group_id else {
            return Err(StorageError::FileNotFound(file_id));
        };
        if selected {
            let unselected: i64 = tx.query_row(
                "SELECT COUNT(*) FROM duplicate_files WHERE group_id=?1 AND selected=0",
                [group_id],
                |r| r.get(0),
            )?;
            let already: bool = tx
                .query_row(
                    "SELECT selected FROM duplicate_files WHERE id=?1",
                    [db_file_id],
                    |r| r.get(0),
                )
                .map(|n: i64| n != 0)?;
            if !already && unselected <= 1 {
                return Err(StorageError::LastCopySelected { file_id });
            }
        }
        tx.execute(
            "UPDATE duplicate_files SET selected=?2 WHERE id=?1",
            params![db_file_id, selected as i64],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Appends an audit entry after a cleanup attempt, including partial outcomes.
    pub fn record_cleanup(&self, scan_id: ScanId, action: &NewCleanupAction) -> Result<i64> {
        if self
            .connection
            .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Err(StorageError::ScanNotFound(scan_id));
        }
        self.connection.execute("INSERT INTO cleanup_actions(scan_id,created_at,action,affected_files,recovered_bytes,attempted_files) VALUES(?1,?2,?3,?4,?5,?4)", params![scan_id,time_to_millis(action.created_at)?,action.action,u64_to_i64(action.affected_files)?,u64_to_i64(action.recovered_bytes)?])?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Records every cleanup result, including targets skipped because they
    /// changed after preflight. This is the preferred API for new callers.
    pub fn record_cleanup_outcome(
        &mut self,
        scan_id: ScanId,
        action_name: &str,
        created_at: SystemTime,
        outcome: &CleanupOutcome,
    ) -> Result<i64> {
        let tx = self.connection.transaction()?;
        let exists: Option<i64> = tx
            .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |row| {
                row.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(StorageError::ScanNotFound(scan_id));
        }
        tx.execute(
            "INSERT INTO cleanup_actions(scan_id,created_at,action,affected_files,recovered_bytes,attempted_files) VALUES(?1,?2,?3,?4,?5,?6)",
            params![scan_id, time_to_millis(created_at)?, action_name, u64_to_i64(outcome.removed.len() as u64)?, u64_to_i64(outcome.recovered_bytes)?, u64_to_i64((outcome.removed.len() + outcome.failures.len()) as u64)?],
        )?;
        let action_id = tx.last_insert_rowid();
        for failure in &outcome.failures {
            tx.execute(
                "INSERT INTO cleanup_failures(cleanup_action_id,path,message) VALUES(?1,?2,?3)",
                params![action_id, encode_path(&failure.path), failure.message],
            )?;
        }
        tx.commit()?;
        Ok(action_id)
    }

    /// Removes successfully cleaned files from a stored result set, drops
    /// groups with fewer than two remaining files, and recalculates the scan
    /// summary. Failed or skipped targets remain selectable for a later scan.
    pub fn reconcile_removed_files(&mut self, scan_id: ScanId, paths: &[PathBuf]) -> Result<()> {
        let tx = self.connection.transaction()?;
        let exists: Option<i64> = tx
            .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |row| {
                row.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(StorageError::ScanNotFound(scan_id));
        }
        for path in paths {
            tx.execute(
                "DELETE FROM duplicate_files WHERE id IN (SELECT f.id FROM duplicate_files f JOIN duplicate_groups g ON f.group_id=g.id WHERE g.scan_id=?1 AND f.path=?2)",
                params![scan_id, encode_path(path)],
            )?;
        }
        tx.execute(
            "DELETE FROM duplicate_groups WHERE scan_id=?1 AND id IN (SELECT g.id FROM duplicate_groups g LEFT JOIN duplicate_files f ON f.group_id=g.id WHERE g.scan_id=?1 GROUP BY g.id HAVING COUNT(f.id) < 2)",
            [scan_id],
        )?;
        let (groups, files, bytes): (i64, i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(file_count),0), COALESCE(SUM(file_size * selected_count),0) FROM (SELECT g.id, g.file_size, COUNT(f.id) AS file_count, SUM(f.selected) AS selected_count FROM duplicate_groups g JOIN duplicate_files f ON f.group_id=g.id WHERE g.scan_id=?1 GROUP BY g.id)",
            [scan_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        tx.execute(
            "UPDATE scans SET duplicate_groups=?2, duplicate_files=?3, duplicate_bytes=?4 WHERE id=?1",
            params![scan_id, groups, files, bytes],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Returns a scan's cleanup audit entries, newest first.
    pub fn cleanup_history(&self, scan_id: ScanId) -> Result<Vec<CleanupAction>> {
        let mut stmt=self.connection.prepare("SELECT id,scan_id,created_at,action,affected_files,recovered_bytes FROM cleanup_actions WHERE scan_id=?1 ORDER BY created_at DESC,id DESC")?;
        Ok(stmt
            .query_map([scan_id], |r| {
                Ok(CleanupAction {
                    id: r.get(0)?,
                    scan_id: r.get(1)?,
                    created_at: millis_to_time(r.get(2)?)?,
                    action: r.get(3)?,
                    affected_files: i64_to_u64(r.get(4)?)?,
                    recovered_bytes: i64_to_u64(r.get(5)?)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?)
    }
    /// Returns detailed cleanup history, including individual skipped targets.
    pub fn cleanup_history_details(&self, scan_id: ScanId) -> Result<Vec<CleanupHistoryEntry>> {
        let mut stmt = self.connection.prepare("SELECT id,scan_id,created_at,action,affected_files,recovered_bytes,attempted_files FROM cleanup_actions WHERE scan_id=?1 ORDER BY created_at DESC,id DESC")?;
        let actions = stmt
            .query_map([scan_id], |row| {
                Ok((
                    CleanupAction {
                        id: row.get(0)?,
                        scan_id: row.get(1)?,
                        created_at: millis_to_time(row.get(2)?)?,
                        action: row.get(3)?,
                        affected_files: i64_to_u64(row.get(4)?)?,
                        recovered_bytes: i64_to_u64(row.get(5)?)?,
                    },
                    i64_to_u64(row.get(6)?)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        actions
            .into_iter()
            .map(|(action, attempted_files)| {
                let mut failures = self.connection.prepare(
                    "SELECT path,message FROM cleanup_failures WHERE cleanup_action_id=?1 ORDER BY id",
                )?;
                let failures = failures
                    .query_map([action.id], |row| {
                        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .into_iter()
                    .map(|(path, message)| {
                        Ok(CleanupFailureRecord {
                            path: decode_path(&path)?,
                            message,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(CleanupHistoryEntry {
                    action,
                    attempted_files,
                    failures,
                })
            })
            .collect()
    }
    /// Removes a historic scan and its dependent rows without touching files on disk.
    pub fn delete_scan(&self, scan_id: ScanId) -> Result<()> {
        let n = self
            .connection
            .execute("DELETE FROM scans WHERE id=?1", [scan_id])?;
        if n == 0 {
            Err(StorageError::ScanNotFound(scan_id))
        } else {
            Ok(())
        }
    }
    fn scan_paths(&self, id: ScanId) -> Result<Vec<ScanPath>> {
        let mut stmt = self
            .connection
            .prepare("SELECT path,preferred FROM scan_paths WHERE scan_id=?1 ORDER BY id")?;
        let raw = stmt
            .query_map([id], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)? != 0))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        raw.into_iter()
            .map(|(path, preferred)| {
                Ok(ScanPath {
                    path: decode_path(&path)?,
                    preferred,
                })
            })
            .collect()
    }
}

fn groups_from_connection(connection: &Connection, scan_id: ScanId) -> Result<Vec<DuplicateGroup>> {
    if connection
        .query_row("SELECT 1 FROM scans WHERE id=?1", [scan_id], |_| Ok(()))
        .optional()?
        .is_none()
    {
        return Err(StorageError::ScanNotFound(scan_id));
    }
    let mut stmt = connection
        .prepare("SELECT id,file_size FROM duplicate_groups WHERE scan_id=?1 ORDER BY id")?;
    let raw = stmt
        .query_map([scan_id], |r| {
            Ok((r.get::<_, i64>(0)?, i64_to_u64(r.get(1)?)?))
        })?
        .collect::<std::result::Result<Vec<(i64, u64)>, _>>()?;
    raw.into_iter()
        .map(|(id, file_size)| group_from_connection(connection, id, file_size))
        .collect()
}

fn group_from_connection(
    connection: &Connection,
    id: i64,
    file_size: u64,
) -> Result<DuplicateGroup> {
    let mut stmt=connection.prepare("SELECT id,path,size,modified_at,modified_at_nanos,selected FROM duplicate_files WHERE group_id=?1 ORDER BY id")?;
    let raw = stmt
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)? != 0,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut selected = Vec::new();
    let files = raw
        .into_iter()
        .map(
            |(file_id, path, size, modified_millis, modified_nanos, checked)| {
                if checked {
                    selected.push(DuplicateFileId(i64_to_u64(file_id)?));
                }
                Ok(DuplicateFile {
                    id: DuplicateFileId(i64_to_u64(file_id)?),
                    path: decode_path(&path)?,
                    size: i64_to_u64(size)?,
                    modified: modified_nanos
                        .map(nanos_to_time)
                        .or_else(|| modified_millis.map(millis_to_time))
                        .transpose()?,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut group = DuplicateGroup::new(GroupId(i64_to_u64(id)?), file_size, files)
        .map_err(|_| StorageError::InvalidTime)?;
    for file_id in selected {
        group
            .set_selected(file_id, true)
            .map_err(|_| StorageError::InvalidTime)?;
    }
    Ok(group)
}

fn insert_scan(tx: &Transaction<'_>, scan: &NewScan) -> Result<ScanId> {
    tx.execute(
        "INSERT INTO scans(name,started_at,status,min_size,max_size,cache,settings_recorded) VALUES(?1,?2,'running',?3,?4,?5,1)",
        params![scan.name, time_to_millis(scan.started_at)?, scan.settings.min_size.map(u64_to_i64).transpose()?, scan.settings.max_size.map(u64_to_i64).transpose()?, scan.settings.cache as i64],
    )?;
    let id = tx.last_insert_rowid();
    for path in &scan.paths {
        tx.execute(
            "INSERT INTO scan_paths(scan_id,path,preferred) VALUES(?1,?2,?3)",
            params![id, encode_path(&path.path), path.preferred as i64],
        )?;
    }
    Ok(id)
}
fn insert_group(tx: &Transaction<'_>, scan_id: ScanId, group: &DuplicateGroup) -> Result<i64> {
    tx.execute(
        "INSERT INTO duplicate_groups(scan_id,file_size) VALUES(?1,?2)",
        params![scan_id, u64_to_i64(group.file_size)?],
    )?;
    Ok(tx.last_insert_rowid())
}
fn insert_file(
    tx: &Transaction<'_>,
    group_id: i64,
    file: &DuplicateFile,
    selected: bool,
) -> Result<i64> {
    tx.execute("INSERT INTO duplicate_files(group_id,path,size,modified_at,modified_at_nanos,selected) VALUES(?1,?2,?3,?4,?5,?6)",params![group_id,encode_path(&file.path),u64_to_i64(file.size)?,file.modified.map(time_to_millis).transpose()?,file.modified.map(time_to_nanos).transpose()?,selected as i64])?;
    Ok(tx.last_insert_rowid())
}
fn time_to_millis(time: SystemTime) -> Result<i64> {
    i64::try_from(
        time.duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::InvalidTime)?
            .as_millis(),
    )
    .map_err(|_| StorageError::InvalidTime)
}
fn millis_to_time(value: i64) -> std::result::Result<SystemTime, rusqlite::Error> {
    u64::try_from(value)
        .map(|v| UNIX_EPOCH + std::time::Duration::from_millis(v))
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}
fn time_to_nanos(time: SystemTime) -> Result<i64> {
    i64::try_from(
        time.duration_since(UNIX_EPOCH)
            .map_err(|_| StorageError::InvalidTime)?
            .as_nanos(),
    )
    .map_err(|_| StorageError::InvalidTime)
}
fn nanos_to_time(value: i64) -> std::result::Result<SystemTime, rusqlite::Error> {
    u64::try_from(value)
        .map(|v| UNIX_EPOCH + std::time::Duration::from_nanos(v))
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}
fn u64_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StorageError::InvalidTime)
}
fn i64_to_u64(value: i64) -> std::result::Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = vec![0];
    out.extend_from_slice(path.as_os_str().as_bytes());
    out
}
#[cfg(unix)]
fn decode_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if bytes.first() != Some(&0) {
        return Err(StorageError::IncompatiblePath);
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[1..])))
}
#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = vec![1];
    for x in path.as_os_str().encode_wide() {
        out.extend_from_slice(&x.to_le_bytes())
    }
    out
}
#[cfg(windows)]
fn decode_path(bytes: &[u8]) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.first() != Some(&1) || (bytes.len() - 1) % 2 != 0 {
        return Err(StorageError::IncompatiblePath);
    }
    let w = bytes[1..]
        .chunks_exact(2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]))
        .collect();
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&w)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn db() -> (NamedTempFile, Database) {
        let file = NamedTempFile::new().unwrap();
        let db = Database::open(file.path()).unwrap();
        (file, db)
    }
    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + std::time::Duration::from_millis(ms)
    }
    fn scan(path: PathBuf) -> NewScan {
        NewScan {
            name: Some("Pictures scan".into()),
            started_at: at(1_000),
            paths: vec![ScanPath {
                path,
                preferred: true,
            }],
            settings: ScanSettings::default(),
        }
    }
    fn results() -> Vec<DuplicateGroup> {
        let mut group = DuplicateGroup::new(
            GroupId(1),
            20,
            vec![
                DuplicateFile {
                    id: DuplicateFileId(1),
                    path: PathBuf::from("/photos/été a.jpg"),
                    size: 20,
                    modified: Some(at(2_000)),
                },
                DuplicateFile {
                    id: DuplicateFileId(2),
                    path: PathBuf::from("/backup/été a.jpg"),
                    size: 20,
                    modified: None,
                },
            ],
        )
        .unwrap();
        group.set_selected(DuplicateFileId(2), true).unwrap();
        vec![group]
    }

    #[test]
    fn persists_complete_scan_groups_selection_and_history() {
        let (_file, mut db) = db();
        let id = db
            .create_scan(&scan(PathBuf::from("/with spaces/日本語")))
            .unwrap();
        db.replace_results(
            id,
            &results(),
            &ScanSummary {
                duplicate_groups: 1,
                recoverable_bytes: 20,
                duplicate_files: 2,
            },
            at(3_000),
        )
        .unwrap();
        db.record_cleanup(
            id,
            &NewCleanupAction {
                created_at: at(4_000),
                action: "trash".into(),
                affected_files: 1,
                recovered_bytes: 20,
            },
        )
        .unwrap();

        let stored = db.scan(id).unwrap();
        assert_eq!(stored.status, ScanStatus::Completed);
        assert_eq!(stored.paths[0].path, PathBuf::from("/with spaces/日本語"));
        assert_eq!(
            stored.summary,
            Some(ScanSummary {
                duplicate_groups: 1,
                recoverable_bytes: 20,
                duplicate_files: 2
            })
        );
        let group = db.groups(id).unwrap().pop().unwrap();
        assert_eq!(group.files[0].path, PathBuf::from("/photos/été a.jpg"));
        assert!(group.is_selected(group.files[1].id));
        assert_eq!(db.cleanup_history(id).unwrap()[0].action, "trash");
    }

    #[test]
    fn selection_cannot_remove_all_copies() {
        let (_file, mut db) = db();
        let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
        db.replace_results(
            id,
            &results(),
            &ScanSummary {
                duplicate_groups: 1,
                recoverable_bytes: 20,
                duplicate_files: 2,
            },
            at(3),
        )
        .unwrap();
        let first = db.groups(id).unwrap()[0].files[0].id;
        assert!(matches!(
            db.set_selected(first, true),
            Err(StorageError::LastCopySelected { .. })
        ));
        db.set_selected(first, false).unwrap();
    }

    #[test]
    fn replacement_returns_database_owned_ids_with_selection_intact() {
        let (_file, mut db) = db();
        let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
        let precise_modified = at(2_000) + std::time::Duration::from_nanos(123_456_789);
        db.replace_results(
            id,
            &results(),
            &ScanSummary {
                duplicate_groups: 1,
                duplicate_files: 2,
                recoverable_bytes: 20,
            },
            at(2),
        )
        .unwrap();

        let mut replacement = DuplicateGroup::new(
            GroupId(900),
            20,
            vec![
                DuplicateFile {
                    id: DuplicateFileId(901),
                    path: PathBuf::from("/photos/été a.jpg"),
                    size: 20,
                    modified: Some(precise_modified),
                },
                DuplicateFile {
                    id: DuplicateFileId(902),
                    path: PathBuf::from("/backup/été a.jpg"),
                    size: 20,
                    modified: None,
                },
            ],
        )
        .unwrap();
        replacement
            .set_selected(DuplicateFileId(902), true)
            .unwrap();
        let replacement = vec![replacement];
        let refreshed_settings = ScanSettings {
            min_size: Some(42),
            max_size: Some(4_200),
            cache: false,
        };
        let loaded = db
            .replace_results_and_load_with_settings(
                id,
                &replacement,
                &ScanSummary {
                    duplicate_groups: 1,
                    duplicate_files: 2,
                    recoverable_bytes: 20,
                },
                at(3),
                Some(refreshed_settings),
            )
            .unwrap();

        assert_ne!(loaded[0].id, replacement[0].id);
        assert_ne!(loaded[0].files[0].id, replacement[0].files[0].id);
        assert!(loaded[0].is_selected(loaded[0].files[1].id));
        assert_eq!(loaded[0].files[0].modified, Some(precise_modified));
        assert_eq!(loaded, db.groups(id).unwrap());
        assert_eq!(db.scan(id).unwrap().settings, refreshed_settings);
        assert!(db.scan(id).unwrap().settings_recorded);
    }

    #[test]
    fn legacy_scans_are_marked_when_settings_were_not_recorded() {
        let file = NamedTempFile::new().unwrap();
        {
            let connection = Connection::open(file.path()).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                     INSERT INTO schema_migrations(version) VALUES(4);
                     CREATE TABLE scans (id INTEGER PRIMARY KEY, name TEXT, started_at INTEGER NOT NULL, finished_at INTEGER, status TEXT NOT NULL, duplicate_bytes INTEGER, duplicate_files INTEGER, duplicate_groups INTEGER);
                     INSERT INTO scans(id,name,started_at,status) VALUES(1,'Legacy',0,'completed');
                     CREATE TABLE duplicate_files (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, path BLOB NOT NULL, size INTEGER NOT NULL, modified_at INTEGER, selected INTEGER NOT NULL DEFAULT 0, modified_at_nanos INTEGER);",
                )
                .unwrap();
        }

        let db = Database::open(file.path()).unwrap();
        let scan = db.scan(1).unwrap();
        assert_eq!(scan.settings, ScanSettings::default());
        assert!(!scan.settings_recorded);
    }

    #[test]
    fn deletion_cascades_and_old_scans_can_be_reopened() {
        let (_file, mut db) = db();
        let old = db.create_scan(&scan(PathBuf::from("/old"))).unwrap();
        db.finish_scan(old, ScanStatus::Cancelled, at(2)).unwrap();
        let new = db.create_scan(&scan(PathBuf::from("/new"))).unwrap();
        assert_eq!(
            db.scans().unwrap().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![new, old]
        );
        assert_eq!(db.scan(old).unwrap().status, ScanStatus::Cancelled);
        db.delete_scan(old).unwrap();
        assert!(matches!(db.scan(old), Err(StorageError::ScanNotFound(_))));
    }

    #[test]
    fn detailed_cleanup_history_preserves_partial_failures() {
        let (_file, mut db) = db();
        let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
        let outcome = CleanupOutcome {
            action: dupekit_core::CleanupAction::PermanentDelete,
            removed: vec![PathBuf::from("/a/removed")],
            recovered_bytes: 20,
            failures: vec![dupekit_core::CleanupFailure {
                path: PathBuf::from("/a/changed"),
                message: "file changed since it was scanned".into(),
            }],
        };
        db.record_cleanup_outcome(id, "permanent delete", at(2), &outcome)
            .unwrap();

        let history = db.cleanup_history_details(id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].attempted_files, 2);
        assert_eq!(history[0].action.affected_files, 1);
        assert_eq!(history[0].failures[0].path, PathBuf::from("/a/changed"));
    }

    #[test]
    fn reconciliation_removes_cleaned_files_and_empty_duplicate_groups() {
        let (_file, mut db) = db();
        let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
        db.replace_results(
            id,
            &results(),
            &ScanSummary {
                duplicate_groups: 1,
                duplicate_files: 2,
                recoverable_bytes: 20,
            },
            at(2),
        )
        .unwrap();
        db.reconcile_removed_files(id, &[PathBuf::from("/backup/été a.jpg")])
            .unwrap();

        assert!(db.groups(id).unwrap().is_empty());
        assert_eq!(
            db.scan(id).unwrap().summary,
            Some(ScanSummary {
                duplicate_groups: 0,
                duplicate_files: 0,
                recoverable_bytes: 0,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_unix_paths() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/a bad \xff name"));
        let (_file, mut db) = db();
        let id = db.create_scan(&scan(path.clone())).unwrap();
        let mut group = DuplicateGroup::new(
            GroupId(1),
            1,
            vec![
                DuplicateFile {
                    id: DuplicateFileId(1),
                    path: path.clone(),
                    size: 1,
                    modified: None,
                },
                DuplicateFile {
                    id: DuplicateFileId(2),
                    path: PathBuf::from("/tmp/other"),
                    size: 1,
                    modified: None,
                },
            ],
        )
        .unwrap();
        group.set_selected(DuplicateFileId(2), true).unwrap();
        db.replace_results(
            id,
            &[group],
            &ScanSummary {
                duplicate_groups: 1,
                recoverable_bytes: 1,
                duplicate_files: 2,
            },
            at(2),
        )
        .unwrap();
        assert_eq!(
            db.scan(id).unwrap().paths[0].path.as_os_str().as_bytes(),
            path.as_os_str().as_bytes()
        );
        assert_eq!(
            db.groups(id).unwrap()[0].files[0]
                .path
                .as_os_str()
                .as_bytes(),
            path.as_os_str().as_bytes()
        );
    }
}
