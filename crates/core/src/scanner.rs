use crate::{
    DuplicateFile, DuplicateFileId, DuplicateGroup, GroupId, ScanConfig, ScanResult, ScanSummary,
    SelectionPolicy,
};
use fclones::{
    FileLen,
    config::GroupConfig,
    group_files,
    log::{Log, LogLevel, ProgressBarLength},
    progress::ProgressTracker,
};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::Sender,
    },
};

#[derive(Clone, Debug)]
pub enum ScanEvent {
    Started,
    PhaseStarted { name: String, total: Option<u64> },
    Progress { processed: u64, total: Option<u64> },
    FilesDiscovered(u64),
    GroupFound(DuplicateGroup),
    Finished(ScanSummary),
    Failed(String),
    Cancelled,
}
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("at least one scan path is required")]
    NoPaths,
    #[error("minimum size exceeds maximum size")]
    InvalidSizeRange,
    #[error("scan cancelled")]
    Cancelled,
    #[error("fclones scan failed: {0}")]
    Fclones(String),
}

/// A cloneable cancellation flag. fclones 0.35 does not expose an interruptible
/// grouping API; cancellation is therefore observed before and after its single
/// `group_files` call, while progress remains streamed during that call.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release)
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
pub trait DuplicateScanner {
    fn scan(
        &self,
        config: ScanConfig,
        events: Sender<ScanEvent>,
        cancellation: CancellationToken,
    ) -> Result<ScanResult, ScanError>;
}
#[derive(Default)]
pub struct FclonesScanner;
impl DuplicateScanner for FclonesScanner {
    fn scan(
        &self,
        config: ScanConfig,
        events: Sender<ScanEvent>,
        cancellation: CancellationToken,
    ) -> Result<ScanResult, ScanError> {
        if config.paths.is_empty() {
            return Err(ScanError::NoPaths);
        };
        if config
            .min_size
            .zip(config.max_size)
            .is_some_and(|(min, max)| min > max)
        {
            return Err(ScanError::InvalidSizeRange);
        };
        if cancellation.is_cancelled() {
            let _ = events.send(ScanEvent::Cancelled);
            return Err(ScanError::Cancelled);
        };
        let _ = events.send(ScanEvent::Started);
        let fc = GroupConfig {
            paths: config
                .paths
                .iter()
                .map(|p| fclones::Path::from(&p.path))
                .collect(),
            min_size: FileLen(config.min_size.unwrap_or(1)),
            max_size: config.max_size.map(FileLen),
            cache: config.cache,
            ..GroupConfig::default()
        };
        let log = EventLog {
            events: events.clone(),
            processed: Arc::new(AtomicU64::new(0)),
        };
        let raw = group_files(&fc, &log).map_err(|e| {
            let message = e.to_string();
            let _ = events.send(ScanEvent::Failed(message.clone()));
            ScanError::Fclones(message)
        })?;
        if cancellation.is_cancelled() {
            let _ = events.send(ScanEvent::Cancelled);
            return Err(ScanError::Cancelled);
        };
        let preferred: Vec<_> = config
            .paths
            .iter()
            .filter(|p| p.preferred)
            .map(|p| p.path.clone())
            .collect();
        let mut next_id = 1;
        let groups: Vec<DuplicateGroup> = raw
            .into_iter()
            .enumerate()
            .filter_map(|(group_idx, g)| {
                let files: Vec<_> = g
                    .files
                    .into_iter()
                    .map(|f| {
                        let path = f.path.to_path_buf();
                        let modified = fs::metadata(&path).ok().and_then(|m| m.modified().ok());
                        let id = DuplicateFileId(next_id);
                        next_id += 1;
                        DuplicateFile {
                            id,
                            path,
                            size: g.file_len.0,
                            modified,
                        }
                    })
                    .collect();
                let mut group =
                    DuplicateGroup::new(GroupId(group_idx as u64 + 1), g.file_len.0, files).ok()?;
                group.apply_selection(SelectionPolicy::PreferPreferredDirectories, &preferred);
                Some(group)
            })
            .collect();
        let discovered = groups.iter().map(|g| g.files.len() as u64).sum();
        let _ = events.send(ScanEvent::FilesDiscovered(discovered));
        // Results are returned atomically with `Finished`; sending a cloned
        // group for every match only builds an unbounded queue while the UI is
        // busy scanning. In a large scan that can mean hundreds of thousands
        // of needless allocations, and no current consumer uses those events.
        let result = ScanResult::from_groups(groups);
        let _ = events.send(ScanEvent::Finished(result.summary.clone()));
        Ok(result)
    }
}
struct EventLog {
    events: Sender<ScanEvent>,
    processed: Arc<AtomicU64>,
}
impl Log for EventLog {
    fn progress_bar(&self, msg: &str, len: ProgressBarLength) -> Arc<dyn ProgressTracker> {
        let total = match len {
            ProgressBarLength::Items(n) | ProgressBarLength::Bytes(n) => Some(n),
            ProgressBarLength::Unknown => None,
        };
        self.processed.store(0, Ordering::Release);
        let _ = self.events.send(ScanEvent::PhaseStarted {
            name: msg.to_owned(),
            total,
        });
        Arc::new(EventProgress {
            events: self.events.clone(),
            processed: self.processed.clone(),
            total,
            last_emitted: AtomicU64::new(0),
            minimum_step: total.map(|n| (n / 200).max(1)).unwrap_or(1_024),
        })
    }
    fn log(&self, _: LogLevel, msg: String) {
        let _ = self.events.send(ScanEvent::PhaseStarted {
            name: msg,
            total: None,
        });
    }
}
struct EventProgress {
    events: Sender<ScanEvent>,
    processed: Arc<AtomicU64>,
    total: Option<u64>,
    last_emitted: AtomicU64,
    minimum_step: u64,
}
impl ProgressTracker for EventProgress {
    fn inc(&self, delta: u64) {
        let processed = self.processed.fetch_add(delta, Ordering::AcqRel) + delta;
        let last = self.last_emitted.load(Ordering::Acquire);
        if processed != self.total.unwrap_or(u64::MAX)
            && processed.saturating_sub(last) < self.minimum_step
        {
            return;
        }
        // Only the caller that advances the watermark emits. Concurrent
        // workers may skip an intermediate update, but never fabricate one.
        if self
            .last_emitted
            .compare_exchange(last, processed, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let _ = self.events.send(ScanEvent::Progress {
            processed,
            total: self.total,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tempfile::tempdir;
    #[test]
    fn rejects_bad_config_before_starting_worker() {
        let (_tx, rx) = mpsc::channel();
        assert!(matches!(
            FclonesScanner.scan(ScanConfig::default(), _tx, CancellationToken::default()),
            Err(ScanError::NoPaths)
        ));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn cancelled_before_start_emits_cancelled() {
        let (t, r) = mpsc::channel();
        let c = CancellationToken::default();
        c.cancel();
        assert!(matches!(
            FclonesScanner.scan(
                ScanConfig {
                    paths: vec![crate::ScanPath {
                        path: ".".into(),
                        preferred: false
                    }],
                    ..Default::default()
                },
                t,
                c
            ),
            Err(ScanError::Cancelled)
        ));
        assert!(matches!(r.recv().unwrap(), ScanEvent::Cancelled));
    }

    #[test]
    fn progress_events_are_coalesced_but_finish_at_the_reported_total() {
        let (sender, receiver) = mpsc::channel();
        let progress = EventProgress {
            events: sender,
            processed: Arc::new(AtomicU64::new(0)),
            total: Some(1_000),
            last_emitted: AtomicU64::new(0),
            minimum_step: 100,
        };

        for _ in 0..1_000 {
            progress.inc(1);
        }

        let updates = receiver.try_iter().collect::<Vec<_>>();
        assert!(updates.len() <= 10);
        assert!(matches!(
            updates.last(),
            Some(ScanEvent::Progress {
                processed: 1_000,
                total: Some(1_000)
            })
        ));
    }
    #[test]
    fn finds_duplicates_and_initially_keeps_preferred_directory() {
        let d = tempdir().unwrap();
        let regular = d.path().join("regular");
        let preferred = d.path().join("preferred");
        fs::create_dir(&regular).unwrap();
        fs::create_dir(&preferred).unwrap();
        fs::write(regular.join("a"), b"identical content").unwrap();
        fs::write(preferred.join("b"), b"identical content").unwrap();
        let (t, _r) = mpsc::channel();
        let result = FclonesScanner
            .scan(
                ScanConfig {
                    paths: vec![
                        crate::ScanPath {
                            path: regular.clone(),
                            preferred: false,
                        },
                        crate::ScanPath {
                            path: preferred.clone(),
                            preferred: true,
                        },
                    ],
                    min_size: Some(1),
                    max_size: None,
                    cache: false,
                },
                t,
                CancellationToken::default(),
            )
            .unwrap();
        assert_eq!(result.groups.len(), 1);
        let group = &result.groups[0];
        assert_eq!(group.selected_ids().len(), 1);
        assert!(group.selected_files().all(|f| f.path.starts_with(&regular)));
        assert_eq!(result.selected_bytes(), b"identical content".len() as u64);
    }
}
