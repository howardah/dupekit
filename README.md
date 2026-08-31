# Dupekit

Dupekit is a native Rust desktop application for finding and safely cleaning up duplicate files. It uses the `fclones` crate directly for scanning, Iced for the UI, and an application-owned SQLite database for scan history.

## Alpha status

Dupekit is alpha software. It's largely stable because of its reliance on a the `fclones` crate, but use with caution. Features, interfaces, and the database schema may change between releases. Back up important files before using cleanup actions.

## Run

```sh
cargo run --release -p dupekit
```

Dupekit stores its SQLite scan history database in a `dupekit` folder in your operating system's local data directory. fclones owns and manages hash caching. Dupekit does not duplicate that cache in SQLite.

## Safety model

- Selection logic prevents selecting the final copy in any duplicate group.
- Cleanup checks file size and modification time immediately before acting.
- Moving to the operating system trash is the primary cleanup action.
- Permanent deletion requires a separate explicit confirmation.
- Missing, changed, and failed files are reported instead of silently ignored.

## Workspace

- `crates/core`: domain models, direct fclones integration, selection policies, and cleanup safety
- `crates/storage`: SQLite migrations and persistent scan/cleanup history
- `crates/app`: Iced desktop interface and application orchestration

## Verify

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --no-deps -- -D warnings
```

## Known MVP limitation

The fclones 0.35 public `group_files` API is a single synchronous operation without a cancellation hook. Dupekit immediately returns the UI to a safe state and discards a cancelled scan's eventual result, but the underlying worker can continue until fclones returns.
