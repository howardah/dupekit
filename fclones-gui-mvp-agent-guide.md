# fclones Desktop GUI — MVP Agent Guide

## Goal

Build a small, polished, cross-platform desktop GUI for [fclones](https://github.com/pkolaczk/fclones), focused on safe and fast duplicate-file cleanup.

The core duplicate detection should use the **fclones Rust crate directly**, not invoke the fclones CLI as a subprocess.

The application should be native Rust for the MVP:

- **UI:** Iced
- **Core:** Rust
- **Duplicate detection:** fclones crate
- **Persistence:** SQLite via rusqlite
- **Database ownership:** application-owned SQLite database, preferably with rusqlite's `bundled` feature

The architecture must keep the UI, fclones integration, and persistence sufficiently separated that a Tauri frontend could be added later without rewriting the application core.

---

# 1. Product principles

The application should prioritize:

1. **Safety**
   - Never accidentally delete the only remaining copy of a file.
   - Make destructive operations explicit.
   - Prefer moving files to trash over permanent deletion.
   - Clearly show how much data will be affected.

2. **Speed**
   - fclones is specifically chosen because it is extremely fast.
   - Do not introduce unnecessary copying, serialization, or duplicate hashing.
   - Preserve and use fclones' own cache rather than implementing a second hash cache.

3. **Clarity**
   - A user should be able to select directories, start a scan, understand its progress, review duplicate groups, and clean them up without needing to understand fclones internals.

4. **Native Rust**
   - Avoid unnecessary web technologies.
   - Keep filesystem paths as `PathBuf`/`OsString` internally rather than assuming UTF-8.

5. **Small MVP**
   - Do not attempt to expose every fclones feature.
   - Prefer a robust subset over a feature-heavy but fragile application.

---

# 2. Before coding

First inspect the current fclones project and API:

- https://github.com/pkolaczk/fclones
- https://docs.rs/fclones/
- https://github.com/pkolaczk/fclones-gui

The existing `fclones-gui` project is especially useful as a reference for:

- fclones integration
- progress reporting
- large result sets
- duplicate selection
- filesystem operations
- GTK UX decisions

Do not blindly copy its architecture. Use it to understand existing solutions and avoid reinventing problems that have already been solved.

Check the current versions and APIs rather than assuming API details from this specification.

---

# 3. Recommended architecture

Use a Cargo workspace:

```text
fclones-desktop/
├── Cargo.toml
├── crates/
│   ├── core/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── models.rs
│   │   │   ├── scanner.rs
│   │   │   └── cleanup.rs
│   │   └── Cargo.toml
│   │
│   ├── storage/
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── database.rs
│   │   │   └── migrations.rs
│   │   └── Cargo.toml
│   │
│   └── app/
│       ├── src/
│       │   ├── main.rs
│       │   ├── app.rs
│       │   ├── message.rs
│       │   └── views/
│       │       ├── home.rs
│       │       ├── scan.rs
│       │       └── results.rs
│       └── Cargo.toml
```

The exact organization can change if there is a compelling reason, but preserve the conceptual separation:

```text
Iced UI
   ↓
Application/core layer
   ↓
fclones + filesystem
   ↓
SQLite persistence
```

The UI should not directly manipulate fclones internals or SQL queries.

---

# 4. Core API boundary

Create an application-level abstraction around fclones.

For example:

```rust
pub trait DuplicateScanner {
    fn scan(
        &self,
        config: ScanConfig,
        events: Sender<ScanEvent>,
    ) -> Result<ScanResult>;
}
```

The exact API should be adapted to the actual fclones APIs and Iced concurrency model.

The important requirement is:

> fclones-specific types should not leak throughout the entire application.

Create application-level types such as:

```rust
pub struct ScanConfig {
    pub paths: Vec<ScanPath>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub cache: bool,
}

pub struct ScanPath {
    pub path: PathBuf,
    pub preferred: bool,
}

pub struct DuplicateGroup {
    pub id: GroupId,
    pub file_size: u64,
    pub files: Vec<DuplicateFile>,
}

pub struct DuplicateFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}
```

These are illustrative. Adjust them to the actual requirements and fclones API.

---

# 5. Scan events and progress

The scanner must not block the Iced UI.

Use a worker task/thread and communicate progress back to the application.

Conceptually:

```text
Iced UI
   │
   │ StartScan
   ▼
worker
   │
   ├── fclones
   │
   └── ScanEvent
          │
          ▼
       Iced state
```

Events should cover at least:

```rust
enum ScanEvent {
    Started,
    FilesDiscovered(u64),
    Progress {
        processed: u64,
        total: Option<u64>,
    },
    GroupFound(DuplicateGroup),
    Finished(ScanSummary),
    Failed(String),
    Cancelled,
}
```

Use the actual fclones progress mechanisms where possible rather than estimating progress independently.

Cancellation must be supported.

---

# 6. fclones cache

Do **not** implement a duplicate hash cache in SQLite.

fclones already has a cache mechanism for hashes and filesystem metadata.

The application should configure/use fclones' cache appropriately and allow the user to enable/disable it.

The database should store scan history and application data, not duplicate fclones' internal cache.

---

# 7. SQLite persistence

Use SQLite from the beginning.

Recommended dependency:

```toml
rusqlite = { version = "...", features = ["bundled"] }
```

Use the current compatible rusqlite version rather than hard-coding an obsolete version.

The database should contain application-level data such as:

- scans
- scan paths
- duplicate groups
- duplicate files
- selection state
- cleanup history

It should **not** attempt to reproduce fclones' hashing/cache implementation.

Initial schema can be approximately:

```sql
CREATE TABLE scans (
    id              INTEGER PRIMARY KEY,
    name            TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    status          TEXT NOT NULL,
    duplicate_bytes INTEGER,
    duplicate_files INTEGER
);

CREATE TABLE scan_paths (
    id          INTEGER PRIMARY KEY,
    scan_id     INTEGER NOT NULL,
    path        TEXT NOT NULL,
    preferred   INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(scan_id) REFERENCES scans(id)
);

CREATE TABLE duplicate_groups (
    id        INTEGER PRIMARY KEY,
    scan_id   INTEGER NOT NULL,
    file_size INTEGER NOT NULL,
    FOREIGN KEY(scan_id) REFERENCES scans(id)
);

CREATE TABLE duplicate_files (
    id        INTEGER PRIMARY KEY,
    group_id  INTEGER NOT NULL,
    path      BLOB NOT NULL,
    selected  INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(group_id) REFERENCES duplicate_groups(id)
);

CREATE TABLE cleanup_actions (
    id              INTEGER PRIMARY KEY,
    scan_id         INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    action          TEXT NOT NULL,
    affected_files  INTEGER NOT NULL,
    recovered_bytes INTEGER NOT NULL
);
```

This schema is a starting point, not a rigid requirement.

Important: filesystem paths may not be UTF-8 on Unix. Do not force all internal paths through `String`. Preserve paths as `PathBuf`/`OsString`, and choose an appropriate SQLite representation for lossless persistence.

---

# 8. MVP screens

## 8.1 Home / New Scan

The initial screen should allow:

- adding directories
- removing directories
- marking directories as preferred
- setting minimum file size
- optionally setting maximum file size
- enabling/disabling fclones cache
- starting a scan

Conceptual layout:

```text
┌──────────────────────────────────────────────────────┐
│ Duplicate Finder                                    │
├──────────────────────────────────────────────────────┤
│ Directories                                          │
│                                                      │
│  /mnt/photos                                  [ × ]  │
│  /mnt/backup                                 ★ [ × ] │
│                                                      │
│  [+ Add directory]                                   │
│                                                      │
│ Options                                              │
│                                                      │
│  Minimum file size       [ 1 KB              ]       │
│  Maximum file size       [ —                 ]       │
│  ☑ Use hash cache                                    │
│                                                      │
│                           [ Find duplicates ]         │
└──────────────────────────────────────────────────────┘
```

Do not expose every fclones option in the MVP.

Advanced options can come later.

---

# 9. Preferred/reference directories

Support a concept called:

**Preferred directory**

This is analogous to Krokiet's reference-directory behavior.

Example:

```text
/mnt/photos
/mnt/backup       ★ Preferred
```

The scanner still scans both directories.

The preference affects **selection**, not duplicate detection.

If:

```text
/mnt/photos/a.jpg
/mnt/backup/a.jpg
```

are duplicates, the application should initially prefer keeping:

```text
/mnt/backup/a.jpg
```

Do not make this a destructive scanner rule.

Implement it as an application-level selection policy.

---

# 10. Scan screen

Show clear progress.

Conceptually:

```text
Scanning /mnt/photos

Discovering files        ✓
Grouping by size         ✓
Partial hashing          ██████████████░░ 82%
Full hashing             ███████░░░░░░░░░ 43%

1,824,231 files scanned
438.2 GB processed
18.3 GB duplicates found

                    [ Cancel ]
```

The UI must remain responsive throughout the scan.

Do not attempt to render every discovered file individually during the scan.

If fclones produces enormous numbers of intermediate results, batch updates where appropriate.

---

# 11. Results screen

Duplicate groups are the primary unit of presentation.

Example:

```text
12.4 GB potentially recoverable
4,391 duplicate groups

─────────────────────────────────────────────────────

2 copies · 428 MB

☐ /photos/2024/video.mov
☑ /backup/photos/2024/video.mov

─────────────────────────────────────────────────────

3 copies · 84 MB

☐ /photos/a.raw
☑ /old/photos/a.raw
☑ /backup/a.raw
```

Each file should show at least:

- path
- size
- modification time

The UI should show:

- total duplicate groups
- total duplicate files
- potentially recoverable bytes
- selected files
- selected bytes

---

# 12. Large result sets

This is an important technical requirement.

A duplicate scan may produce:

- tens of thousands of duplicate groups
- hundreds of thousands of files

Do not construct an enormous naïve Iced widget tree if the framework/component being used cannot handle it efficiently.

Before building the full UI, create a prototype that renders roughly:

```text
250,000 rows
```

with:

- scrolling
- selection
- checkbox interaction

Measure memory usage and responsiveness.

Use virtualization/lazy rendering where appropriate.

If Iced's available list approach performs poorly, investigate a better native strategy before proceeding. Do not assume that a normal `Column` containing hundreds of thousands of widgets is acceptable.

This is the highest-risk UI part of the MVP.

---

# 13. Selection

Individual files must be selectable.

The application must maintain a critical invariant:

> At least one file in every duplicate group must remain unselected.

The UI must not merely discourage deleting all copies. The core/application layer should make it impossible to create an invalid destructive operation.

Useful bulk-selection policies:

```text
Select duplicates…

• Keep first
• Keep newest
• Keep oldest
• Prefer preferred directories
• Clear selection
```

The exact labels can be improved during implementation.

Selection should be previewable before deletion.

---

# 14. Cleanup

MVP cleanup actions:

1. Move selected files to trash
2. Permanently delete selected files

Moving to trash should be the primary/default action.

Permanent deletion should require a stronger confirmation.

Before performing cleanup, show:

```text
Delete 8,392 files?

This will affect approximately 118.4 GB.

[ Cancel ]              [ Move to Trash ]
```

After cleanup, show:

- number of files affected
- bytes recovered
- failures, if any

Record cleanup actions in SQLite.

If possible, use dry-run/preflight behavior before actual deletion.

Never assume all files can be safely trashed on every filesystem/platform.

Handle partial failures explicitly.

---

# 15. Out of scope for MVP

Do not implement these unless required to make the MVP work:

- similar-image detection
- image previews
- video previews
- hard-link conversion
- symlink conversion
- reflinks
- custom hashing algorithms
- regex/glob-heavy filtering UI
- advanced thread tuning
- network/cloud-specific behavior
- automatic updates
- authentication/accounts
- cloud synchronization
- Windows-specific optimizations
- macOS-specific optimizations
- a web frontend
- plugin architecture

The goal is a focused duplicate-file cleanup application.

---

# 16. Application state

Iced application state can conceptually look like:

```rust
enum AppState {
    Home,
    Configuring(ScanConfig),
    Scanning(ActiveScan),
    Results(ScanResults),
}
```

Messages can conceptually include:

```rust
enum Message {
    AddDirectory,
    DirectoryAdded(PathBuf),
    RemoveDirectory(PathBuf),
    TogglePreferred(PathBuf),

    StartScan,
    ScanEvent(ScanEvent),
    CancelScan,

    ToggleFile(FileId),
    ApplySelectionRule(SelectionRule),

    MoveSelectedToTrash,
    DeleteSelected,
}
```

Adapt these to the actual Iced version/API.

Keep business logic out of `view()` functions.

---

# 17. Filesystem safety

Treat filesystem operations as high-risk application logic.

Requirements:

- Never delete the final remaining copy in a duplicate group.
- Check that the target still exists immediately before cleanup.
- Account for files changing between scan and cleanup.
- Handle permissions errors.
- Handle files being moved/renamed after the scan.
- Handle paths disappearing.
- Do not follow symlinks unexpectedly.
- Avoid path-string assumptions.
- Do not assume all filesystems provide a trash facility.
- Clearly report partial failures.

The cleanup operation should operate on the actual paths selected by the user, not on stale assumptions from the database.

If a file has changed since scanning, prefer refusing the destructive operation for that file rather than blindly deleting it.

---

# 18. Error handling

Do not use `unwrap()`/`expect()` in normal application paths.

Errors should be represented explicitly and displayed to the user in useful terms.

Examples:

```text
Could not scan /mnt/photos:
Permission denied
```

rather than exposing raw internal errors as the primary UX.

Log detailed diagnostics for developers.

---

# 19. Configuration vs database

Keep lightweight application preferences separate from scan history.

For example:

```text
config.toml
```

may contain:

- theme
- default minimum size
- confirmation preferences
- last-used settings

SQLite contains:

- scan history
- scan paths
- results
- selection state
- cleanup history

Do not store fclones' hash cache in either one if fclones already manages it.

---

# 20. Testing requirements

Write tests for the dangerous logic before polishing the UI.

At minimum:

### Duplicate selection

Test:

- two copies → one can be selected
- three copies → one or two can be selected
- attempting to select all → rejected/prevented
- preferred directory is retained
- newest/oldest policies behave correctly

### Cleanup

Test with temporary directories:

- selected file is moved/deleted
- unselected file survives
- all files cannot be selected
- missing file is handled
- changed file is handled
- permission failure is handled
- partial cleanup is reported correctly

### Persistence

Test:

- scan creation
- result persistence
- reopening a scan
- selection persistence
- cleanup history

### Paths

Test paths containing:

- spaces
- Unicode
- unusual characters
- very long names

Where supported by the platform, test non-UTF-8 Unix paths.

---

# 21. Performance requirements

Do not optimize prematurely, but avoid obvious performance mistakes.

Important rules:

- Do not hash files independently from fclones.
- Do not read entire files into memory.
- Do not duplicate fclones' cache.
- Do not serialize massive result sets unnecessarily.
- Do not perform filesystem scanning on the UI thread.
- Do not render hundreds of thousands of widgets at once.
- Prefer streaming/batched result delivery.
- Avoid cloning large `PathBuf` collections unnecessarily.

The application should feel responsive even while scanning a multi-terabyte disk.

---

# 22. Packaging

The MVP should eventually produce a normal desktop executable.

For Linux, investigate:

- standalone binary
- `.desktop` launcher
- AppImage or another appropriate distribution format

Do not make packaging the first milestone.

The application should work from:

```bash
cargo run --release
```

before distribution packaging is attempted.

---

# 23. Development milestones

Implement in this order.

## Milestone 1 — fclones spike

Create a minimal Rust program that:

1. accepts a directory
2. invokes fclones through its Rust API
3. prints duplicate groups
4. demonstrates progress
5. demonstrates cancellation

Do not build the GUI yet.

Goal: verify the current fclones crate API and understand how it exposes progress/results.

---

## Milestone 2 — core model

Implement:

- ScanConfig
- DuplicateGroup
- DuplicateFile
- selection policies
- preferred-directory logic
- safe deletion validation

Write tests.

---

## Milestone 3 — SQLite

Implement:

- migrations
- scan history
- scan paths
- duplicate groups
- duplicate files
- cleanup history

Do not store fclones' internal hash cache.

---

## Milestone 4 — Iced shell

Implement:

- home screen
- directory picker
- scan configuration
- scan state
- progress screen

---

## Milestone 5 — result-list performance spike

Before implementing the polished results page:

- generate a large fake result set
- render ~250k file rows
- scroll rapidly
- toggle selections
- monitor memory/CPU

Resolve virtualization/performance issues here.

---

## Milestone 6 — results

Implement:

- grouped duplicate results
- individual selection
- preferred-directory selection
- bulk selection
- recoverable-byte calculations

---

## Milestone 7 — cleanup

Implement:

- move to trash
- permanent delete
- confirmations
- safety checks
- error reporting
- cleanup history

---

## Milestone 8 — scan history

Implement:

- previous scans
- scan summaries
- reopen results
- delete old scan records

---

## Milestone 9 — polish

Only after the core is reliable:

- keyboard shortcuts
- better empty states
- icons
- responsive layout
- dark/light/system theme
- accessibility
- packaging

---

# 24. Definition of done

The MVP is complete when a user can:

1. Launch the application.
2. Select one or more directories.
3. Mark one or more directories as preferred.
4. Start a duplicate scan.
5. See meaningful progress.
6. Cancel a scan.
7. Wait for the scan to finish.
8. Browse duplicate groups.
9. See file paths, sizes, and modification dates.
10. Apply a bulk selection policy.
11. Manually change selections.
12. See total selected files and bytes.
13. Be prevented from selecting every copy of a duplicate.
14. Move selected files to trash.
15. Permanently delete files with explicit confirmation.
16. See cleanup results/errors.
17. Reopen previous scan results.
18. Benefit from fclones' existing hash cache on subsequent scans.

---

# 25. Important implementation philosophy

Do not turn this into a clone of every feature in fclones.

The value of this application is the combination of:

```text
fclones' excellent performance
        +
a safe, understandable GUI
        +
persistent scan history
        +
smart selection policies
```

The MVP should feel like a **native, fast, trustworthy duplicate-file cleaner**, not like a GUI wrapper around every possible fclones option.

When choosing between two implementations, prefer the one that:

1. keeps filesystem operations safe,
2. preserves fclones' performance,
3. keeps the application architecture simple,
4. keeps core logic independent of Iced,
5. is easy to test.

Do not add features merely because fclones supports them.
