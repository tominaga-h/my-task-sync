# my-task-sync

macOS local HTTP server that exposes the **my-task** SQLite over REST so that
**my-own** (the Next.js Web app) can read and write tasks against it. Runs
under `launchctl` on a loopback port (default `:3333`) with Bearer token
authentication.

Designed for a single user on a single machine — not a multi-tenant service.

- Design spec: [`docs/SERVER_DESIGN.md`](docs/SERVER_DESIGN.md)
- HTTP API reference: [`docs/API.md`](docs/API.md)
- Migration plan & progress: [`tasks/plan.md`](tasks/plan.md), [`tasks/todo.md`](tasks/todo.md)

> **Phase 1** — loopback-only access from my-own running locally. Phase 2 will
> add an ngrok subprocess so the server is reachable from Vercel-hosted my-own
> via a stable public URL.

## Requirements

- macOS (uses `launchctl` LaunchAgents)
- Rust 1.75+ (uses native `async fn` in trait)
- A running [`my-task`](https://github.com/mad-tmng/my-task) install (provides
  the SQLite schema this server reads/writes)
- An API key — a shared secret between my-task-sync and whoever calls it

## Build

```bash
cargo build --release           # release binary → target/release/my-task-sync
make check                      # fmt + check + clippy + test (pre-push gate)
```

## Configure

```bash
mkdir -p ~/.config/my-task-sync
cp config.example.toml ~/.config/my-task-sync/config.toml
$EDITOR ~/.config/my-task-sync/config.toml     # set api_key
```

`config.toml` shape:

```toml
[sqlite]
# path = "/custom/path/tasks.db"    # default: ~/Library/Application Support/my-task/tasks.db

[server]
port    = 3333
api_key = "your-api-key-here"
```

Environment variables override file values:

| Variable                  | Overrides           |
|---------------------------|---------------------|
| `MY_TASK_SYNC_API_KEY`    | `[server].api_key`  |
| `MY_TASK_SYNC_PORT`       | `[server].port`     |
| `MY_TASK_DATA_FILE`       | `[sqlite].path`     |
| `RUST_LOG`                | tracing filter      |

The server refuses to start if `api_key` is not resolved (no silent defaults).

## Install as a LaunchAgent

```bash
cp target/release/my-task-sync /usr/local/bin/
cp com.my-task-sync.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.my-task-sync.plist
```

## Manage

```bash
# Status
launchctl list | grep my-task-sync

# Stop
launchctl unload ~/Library/LaunchAgents/com.my-task-sync.plist

# Logs
tail -f /tmp/my-task-sync.out.log
tail -f /tmp/my-task-sync.err.log
```

Graceful shutdown waits up to 10 s for in-flight HTTP requests before
force-exiting, so `launchctl unload` / restart cycles don't stall.

## Quick API check

```bash
# Liveness (no auth)
curl localhost:3333/healthz
# → 200 OK, body "ok"

# List tasks (auth required)
curl -H "Authorization: Bearer $MY_TASK_SYNC_API_KEY" localhost:3333/api/tasks

# Create a task
curl -X POST \
  -H "Authorization: Bearer $MY_TASK_SYNC_API_KEY" \
  -H "content-type: application/json" \
  -d '{"title":"buy milk","status":"open","source":"web","createdAt":"2026-04-18T10:00:00Z","updatedAt":"2026-04-18T10:00:00Z"}' \
  localhost:3333/api/tasks
```

Full endpoint reference with request/response shapes and error codes is in
[`docs/API.md`](docs/API.md).

## Known limitations

- `updated` is day-precision (inherited from `my-task`'s `NaiveDate` schema).
  The `?since=<RFC 3339 datetime>` filter is truncated to UTC date and compared
  inclusive (`>=`), so same-day updates may appear in multiple incremental
  fetches — clients should dedup by `taskNumber`.
- The server binds to `127.0.0.1` only. Phase 2's ngrok subprocess provides
  public access.
