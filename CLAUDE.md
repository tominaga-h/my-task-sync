# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

macOS local **HTTP server** that exposes the my-task SQLite over REST for **my-own** (Next.js + Neon Postgres Web app) to read and write. Built with axum, runs under `launchctl` on a loopback port (default `:3333`), authenticated with a static Bearer token. Single-user, single-machine.

In Phase 1 only localhost access is supported. Phase 2 adds an `ngrok` subprocess so Vercel-hosted my-own can reach this daemon via a public URL.

The authoritative design spec is `docs/SERVER_DESIGN.md`. The public HTTP API is documented in `docs/API.md`. Phased migration progress lives in `tasks/plan.md` and `tasks/todo.md`. When the code and those docs disagree, surface the discrepancy rather than silently picking one.

v1 (polling daemon) is fully removed. If you encounter references to `sync_engine`, `api_client`, `state.db`, `last_push_at`, `--once`, or `--dry-run`, that's stale documentation — fix it.

## Commands

```bash
make check                                     # fmt + check + clippy + test (mirrors pre-push hook / CI)
cargo build --release                          # release binary → target/release/my-task-sync
cargo test                                     # all unit + integration tests
cargo test --test http_tasks_test              # one integration test file
cargo test --test http_tasks_test -- post_with_all_optional_fields_round_trips  # one test
cargo test --lib                               # library unit tests only

./target/debug/my-task-sync                    # run server; reads ~/.config/my-task-sync/config.toml
./target/debug/my-task-sync --config ./my.toml # explicit config path
MY_TASK_SYNC_API_KEY=... MY_TASK_SYNC_PORT=13333 MY_TASK_DATA_FILE=/tmp/x.db \
  ./target/debug/my-task-sync                  # env-driven config (useful for smoke tests)
RUST_LOG=my_task_sync=debug,tower_http=debug ./target/debug/my-task-sync
```

`make check` is stricter than `cargo test` alone — it runs `cargo fmt`, `cargo check --all-targets -Dwarnings`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`. Same gate as the git pre-push hook.

The integration test in `tests/cli_integration_test.rs` reads `CARGO_BIN_EXE_my-task-sync` **at runtime**, which cargo only injects at compile time. `.cargo/config.toml` propagates the variable to test child processes — if you rename the binary or move it out of `target/debug/`, update that file too.

## Architecture

### Request flow

1. axum `Router` on `127.0.0.1:<port>` (loopback only; external ingress is ngrok's job in Phase 2).
2. `TraceLayer::new_for_http()` wraps everything for request/response logging.
3. `/healthz` is open for smoke testing. Everything under `/api/*` passes through the Bearer auth middleware first.
4. Handlers take `State(AppState)` + `Query<T>` / `Json<T>`, validate input, lock the SQLite mutex briefly, build a DTO, return `Result<Json<T>, Error>`.
5. `Error` implements `IntoResponse`: `401 unauthorized` / `400 bad request` pass a client-facing message through; `500 internal error` hides details and logs via `tracing::error!`.

### Key invariants — do not change without discussion

- **SQLite rowid is the canonical `task_number`.** The server assigns it on `POST /api/tasks`. Requests that include `taskNumber` in the body get `400`. `insert_task_row(..., explicit_id=Some(n))` is kept for future restore/replay scenarios but is not exercised in Phase 1 handlers.
- **`updated` is day-precision** (`NaiveDate`, `YYYY-MM-DD`) because my-task's schema stores only dates. The `since` query parameter is parsed as RFC 3339 datetime (so it round-trips with the `serverTime` field), then truncated to UTC date and compared with `updated >= date(since)`. Same-day updates will re-appear on subsequent fetches — clients must dedup by `taskNumber`.
- **Fail-fast config.** Missing `[server].api_key` is a hard error (`Error::Config`) at startup. No silent defaults. CLI rejects unknown flags for the same reason; `--once` / `--dry-run` from v1 are intentionally un-accepted so stale launchctl plists fail loudly.
- **`AppState` uses `std::sync::Mutex<Connection>`.** Handlers acquire it, run SQL, release — **never holding the lock across `.await`**. Consistent reads that need two statements (e.g. `list_tasks` fetches tasks then reminds) stay inside one lock block. For T5 POST the INSERT + reminds are wrapped in `unchecked_transaction` so a mid-flight failure rolls back cleanly.
- **`/healthz` is intentionally unauthenticated.** Operators can probe it without a token. The Bearer middleware is scoped to the nested `/api` router.
- **Graceful shutdown has a 10 s deadline** (`GRACEFUL_SHUTDOWN_SECS` in `main.rs`). SIGINT/SIGTERM trigger axum drain; if in-flight requests don't complete within the window, the server force-exits so `launchctl KeepAlive` restart cycles don't stall.

### SQLite schema dependency

`my-task-sync` **does not create** the `tasks` / `projects` / `task_reminds` tables — `my-task` owns the schema. If that schema changes, this daemon's reads/writes break. The test helper `tests/common/mod.rs::make_my_task_db` must be kept byte-identical with `~/lab/rust/my-task/src/db.rs` L14–38; mismatches cause tests to pass while production fails.

**Tests use an in-memory mock SQLite** (`make_my_task_db()`), never the user's real `tasks.db`. `tests/http_tasks_test.rs` constructs a fresh in-memory DB per test and wires it into the real `router()`.

### Module layout

- `src/main.rs` — CLI parsing, tracing init, axum boot, SIGINT/SIGTERM handling with graceful shutdown deadline.
- `src/config.rs` — TOML + env resolution. Order: env > TOML > default. Default config path is `$HOME/.config/my-task-sync/config.toml` (explicit override of `dirs::config_dir()` — which would pick macOS `Library/Application Support`).
- `src/sqlite.rs` — all `rusqlite` I/O. Public helpers: `open`, `resolve_project`, `insert_task_row` / `update_task_row` (write through `TaskRow<'_>`), `read_tasks_since` / `read_all_tasks` (legacy, retained for tests), `read_tasks_filtered` (the T3 filter SQL), `read_task_by_id`, `replace_reminds`, `read_reminds_for_tasks`.
- `src/http/mod.rs` — `Router` assembly, `AppState { conn, api_key }`, `TraceLayer`, `/api` nest with Bearer layer + `api_not_found` fallback.
- `src/http/auth.rs` — `require_bearer` middleware, constant-time token compare.
- `src/http/tasks.rs` — `list_tasks` (T3), `create_task` (T5). T4 `get_task` / T6 `patch_task` land here.
- `src/model.rs` — domain types (`Task`, `Status`) and API DTOs (`TaskDto`, `TaskCreateDto`, `TaskListResponse`, `TaskResponse`). All DTOs serialize camelCase; `TaskCreateDto` has `deny_unknown_fields` so client typos fail loudly.
- `src/error.rs` — hand-written `Error` enum (no `thiserror`), with `Display`, `std::error::Error`, `From<..>` for transport errors, and `IntoResponse` for HTTP mapping.

## Editing guidance

- **Rust edition 2021, MSRV 1.75** (uses native `async fn in trait`; don't add the `async-trait` crate).
- **Run `make check` before committing.** Same gate as the pre-push hook.
- **Adding an endpoint**: mirror the pattern in `list_tasks` / `create_task` — validate input in the handler (400 paths), acquire the mutex for the minimum window, call `sqlite::*` helpers, build the DTO response. For writes, use `unchecked_transaction` around the INSERT + related rows.
- **Adding SQLite writes**: route through `TaskRow` + `insert_task_row` / `update_task_row` rather than inlining SQL — keeps the schema-dependent column list in one place.
- **Adding a new config field**: extend both `FileConfig` (TOML shape) and `ResolvedConfig` (validated shape) in `src/config.rs`; add env-override logic in `resolve()` if it's sensitive; update `config.example.toml` and the README env table.
- **Adding a DTO field**: camelCase on the wire, snake_case in Rust (rely on `#[serde(rename_all = "camelCase")]`). For request bodies, keep `deny_unknown_fields`. If the new field is optional at the HTTP boundary but stored, decide whether `#[serde(default)]` is appropriate.
- **Before deleting code that seems unused**: check `git log -p` for the original commit and the plan. v1 removal happened in T1; most "dead" pre-Phase-1 code is already gone. What remains is kept on purpose (e.g. `insert_task_row` with `explicit_id` for future replay).
