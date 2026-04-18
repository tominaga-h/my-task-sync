# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

macOS local daemon that keeps **my-task** (local Rust CLI backed by SQLite at `~/Library/Application Support/my-task/tasks.db`) in sync with **my-own** (Next.js + Neon Postgres at `~/lab/typescript/REACT/my-own`). Polls every 30s under `launchctl`. Single-user, single-machine — not a general sync service.

The one-page spec this codebase implements is `docs/OVERVIEW.md`. Treat it as the source of truth; when code and `OVERVIEW.md` disagree, surface the discrepancy rather than silently picking one.

## Commands

```bash
cargo build --release                          # release binary → target/release/my-task-sync
cargo test                                     # all unit + integration tests
cargo test --test sync_engine_test             # one integration test file
cargo test --test sync_engine_test -- push_sends_only_rows_since_last_push   # one test by name
cargo test --lib                               # library unit tests only
cargo clippy --all-targets -- -D warnings      # lint (no clippy config checked in)

./target/debug/my-task-sync --once --dry-run   # one sync cycle without writes (reads still hit the API)
./target/debug/my-task-sync --config ./my.toml # point at an explicit config
RUST_LOG=my_task_sync=debug ./target/debug/my-task-sync --once
```

The integration test in `tests/cli_integration_test.rs` reads `CARGO_BIN_EXE_my-task-sync` **at runtime**, which cargo only injects at compile time. `.cargo/config.toml` propagates the variable to test child processes — if you rename the binary or move it out of `target/debug/`, update that file too.

## Architecture

### Three-step sync cycle (`src/sync_engine.rs::sync_cycle`)

Each tick runs these three steps sequentially; failure of one step aborts the cycle and is retried on the next tick (state is only advanced on success):

1. **push** — `SELECT * FROM tasks WHERE updated > last_push_at`, `POST /api/sync/tasks/push`, then set `last_push_at = now()`.
2. **pull_unsynced** — `GET /api/sync/tasks/unsynced` (rows the web UI created with `task_number IS NULL`), `INSERT` into SQLite to get an autoincrement rowid, `PATCH /api/sync/tasks/:neon_id/number` with that rowid. The INSERT + reminds + PATCH are wrapped in a single `unchecked_transaction`; if the PATCH fails the SQLite write is rolled back so the next cycle retries cleanly (no double-INSERT, `task_number` stays NULL in Neon).
3. **pull_updates** — `GET /api/sync/tasks/changes?since=last_pull_at`, apply row-level Last-Write-Wins by comparing `updated` dates, then set `last_pull_at = response.server_time`. On UPDATE or INSERT, `task_reminds` for that task is **replaced wholesale**.

### Key invariants — do not change without discussion

- **SQLite rowid is the canonical `task_number`.** Neon never mints numbers. If `task_number` exists on a Neon row, it equals a SQLite rowid; if it doesn't, `pull_unsynced` will assign one next cycle. `insert_task_row(..., explicit_id=Some(n))` is the only way to bypass autoincrement, and it's used exclusively by `pull_updates` to preserve this identity when inserting a Neon-only row into SQLite.
- **State lives in a separate SQLite file.** `~/.config/my-task-sync/state.db` holds only `last_push_at` / `last_pull_at` so resetting the daemon never risks the user's tasks in `tasks.db`. Never write daemon state into `tasks.db`.
- **`updated` is day-precision (`NaiveDate`, `YYYY-MM-DD`).** Inherited from `my-task`'s schema. Same-day conflicts resolve to "whichever side pushed later" — documented as acceptable for single-human use. Do not pretend LWW is sub-day-accurate.
- **Fail-fast config.** Missing `api_key` or `base_url` is a hard error (`Error::Config`). Never silently default either. CLI rejects unknown flags for the same reason.
- **Dry-run suppresses ALL writes.** Both API mutations (`push_tasks`, `patch_task_number`) and any SQLite write that would follow them. Read-only API calls (`get_unsynced`, `get_changes`) still happen so a dry run reflects what *would* be observed. `state.set` is also suppressed.
- **Daemon loop is crash-tolerant.** Inside the main loop, a failed `sync_cycle` is logged and retried on the next tick; only fatal startup errors (config / SQLite / HTTP client construction) exit non-zero. `launchctl KeepAlive` handles restart.

### SQLite schema dependency

`my-task-sync` **does not create** the `tasks` / `projects` / `task_reminds` tables — `my-task` owns the schema. If that schema changes, this daemon's reads/writes break. The test helper `tests/common/mod.rs::make_my_task_db` must be kept byte-identical with `~/lab/rust/my-task/src/db.rs` L14–38; mismatches cause tests to pass while production fails.

### Module layout (matches `OVERVIEW.md` § リポジトリ構成)

- `src/main.rs` — CLI parsing, tracing init, main loop, ctrl-c shutdown.
- `src/config.rs` — TOML + env + CLI resolution. Order: CLI `--config` > default `~/.config/my-task-sync/config.toml` > env overrides per field.
- `src/sqlite.rs` — all `rusqlite` I/O. `TaskRow<'_>` is the borrow-friendly write DTO used by both `insert_task_row` and `update_task_row` so Neon DTOs can write through without constructing `Task`.
- `src/api_client.rs` — `SyncApi` trait + `HttpApiClient`. Retry policy: 1 + 3 attempts with `1s → 2s → 4s` backoff for transport / 5xx only; 4xx returns immediately (authn failures shouldn't retry). Streaming / non-cloneable bodies fail the retry loop.
- `src/sync_engine.rs` — the three steps above.
- `src/sync_state.rs` — thin `Mutex<Connection>` wrapper over `state.db` (`sync_state` key/value table).
- `src/model.rs` — domain types (`Task`, `Status`) internal to SQLite reads; API DTOs (`SyncTask`, `UnsyncedTask`, `ChangedTask`, `PushResponse`, …) serialize camelCase.
- `src/error.rs` — hand-written `Error` enum (no `thiserror` dep).

## Editing guidance

- **Rust edition 2021, MSRV 1.75** (uses native `async fn in trait` — no `async-trait` crate; don't add one).
- When adding an API endpoint: extend the `SyncApi` trait, implement on `HttpApiClient`, and provide a mock impl in whichever test file needs it (`tests/sync_engine_test.rs` already contains a `MockApi` pattern to copy).
- When adding SQLite writes: route through `TaskRow` + `insert_task_row` / `update_task_row` rather than inlining SQL, so the schema-dependent column list stays in one place.
- When adding new config fields: extend both `FileConfig` (TOML shape) and `ResolvedConfig` (validated shape) in `src/config.rs`; add env-override logic in `resolve()` if the field is security-sensitive; extend `config.example.toml` and the README env table.
