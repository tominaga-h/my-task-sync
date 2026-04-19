# my-task-sync

macOS local HTTP server that exposes the **my-task** SQLite over REST so that
**my-own** (the Next.js Web app) can read and write tasks against it. Runs
under `launchctl` on a loopback port (default `:3333`) with Bearer token
authentication.

Designed for a single user on a single machine — not a multi-tenant service.

> 日本語版: [`docs/README_ja.md`](docs/README_ja.md)

- Design spec: [`docs/SERVER_DESIGN.md`](docs/SERVER_DESIGN.md)
- HTTP API reference: [`docs/API.md`](docs/API.md)
- Migration plan & progress: [`tasks/plan.md`](tasks/plan.md), [`tasks/todo.md`](tasks/todo.md)

> **Phase 2 complete** — when `[ngrok].domain` is configured, my-task-sync
> spawns `ngrok` as a subprocess on startup, exposing the loopback server
> via a stable public URL for Vercel-hosted my-own.

## Requirements

- macOS (uses `launchctl` LaunchAgents)
- Rust 1.75+ (uses native `async fn` in trait)
- A running [`my-task`](https://github.com/mad-tmng/my-task) install (provides
  the SQLite schema this server reads/writes)
- An API key — a shared secret between my-task-sync and whoever calls it
- (optional) [`ngrok`](https://ngrok.com/) binary + authtoken, if you want the
  server reachable from the internet (e.g. for my-own on Vercel)

## ngrok setup (optional)

Only needed if you want a public URL. Skip this section for local-only use.

```bash
brew install ngrok
ngrok config add-authtoken <your-authtoken>
ngrok config check          # should print "Valid configuration file at ..."
```

Then reserve a free domain from https://dashboard.ngrok.com/cloud-edge/domains
(e.g. `unedified-carrie-example.ngrok-free.dev`) and set it in your config
(see below).

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

# [ngrok] — optional. If `domain` is set, my-task-sync launches `ngrok http`
# as a subprocess on startup and exposes the loopback port publicly. If
# omitted, the server is reachable only on localhost.
# [ngrok]
# domain = "unedified-carrie-example.ngrok-free.dev"
```

Environment variables override file values:

| Variable                         | Overrides           |
|----------------------------------|---------------------|
| `MY_TASK_SYNC_API_KEY`           | `[server].api_key`  |
| `MY_TASK_SYNC_PORT`              | `[server].port`     |
| `MY_TASK_DATA_FILE`              | `[sqlite].path`     |
| `MY_TASK_SYNC_NGROK_DOMAIN`      | `[ngrok].domain`    |
| `RUST_LOG`                       | tracing filter      |

The server refuses to start if `api_key` is not resolved (no silent defaults).
If `[ngrok].domain` is configured but the `ngrok` binary isn't in `PATH`, the
server also refuses to start (fail-fast with an error pointing to the install
steps above).

## Install as a LaunchAgent

```bash
cp target/release/my-task-sync /usr/local/bin/
cp com.my-task-sync.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.my-task-sync.plist
```

If you use ngrok, set the domain via the plist's `EnvironmentVariables` or via
the `config.toml` loaded by the agent; either works.

## Manage

```bash
# Status
launchctl list | grep my-task-sync

# Stop
launchctl unload ~/Library/LaunchAgents/com.my-task-sync.plist

# Logs (my-task-sync itself)
tail -f /tmp/my-task-sync.out.log
tail -f /tmp/my-task-sync.err.log

# Logs (ngrok subprocess, if enabled)
tail -f /tmp/my-task-sync-ngrok.out.log
tail -f /tmp/my-task-sync-ngrok.err.log

# Verify public URL is up (no auth required)
curl -sS localhost:3333/api/status | jq
```

Graceful shutdown waits up to 10 s for in-flight HTTP requests before
force-exiting, so `launchctl unload` / restart cycles don't stall. The ngrok
subprocess is killed via `killpg` (process-group-wide SIGKILL) as part of the
shutdown flow.

### Log hygiene

The ngrok log files at `/tmp/my-task-sync-ngrok.{out,err}.log` are opened in
**append** mode (so prior crash dumps aren't lost on restart). They are not
rotated automatically; if they grow too large, delete them manually — the
next startup will recreate them.

## Quick API check

```bash
# Liveness (no auth)
curl localhost:3333/healthz
# → 200 OK, body "ok"

# Server + ngrok status (no auth — the ngrok URL is the secret gate)
curl -sS localhost:3333/api/status | jq

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
- When running without ngrok, the server binds to `127.0.0.1` only. For
  internet reachability, configure `[ngrok].domain` (see above).
- `/api/status` is intentionally unauthenticated. Treat the ngrok URL itself
  as a soft secret — if it leaks, rotate the reserved domain.
