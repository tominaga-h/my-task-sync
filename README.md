# my-task-sync

macOS local daemon that keeps **my-task** (local SQLite CLI) and **my-own**
(Neon-backed Web app) in sync. Polls every 30 seconds (configurable),
runs under `launchctl`, and uses the SQLite rowid as the canonical
`task_number`.

Designed for a single user, single machine; not a general multi-tenant
sync service.

## Requirements

- macOS (uses `launchctl` LaunchAgents)
- Rust 1.75+ (uses native `async fn` in trait)
- A running my-task install (provides the SQLite schema this daemon reads)
- An API key for the my-own `/api/sync/tasks/*` endpoints

## Build

```bash
cargo build --release
```

The release binary lands at `target/release/my-task-sync`.

## Configure

```bash
mkdir -p ~/.config/my-task-sync
cp config.example.toml ~/.config/my-task-sync/config.toml
$EDITOR ~/.config/my-task-sync/config.toml      # set api_key
```

Settings can be overridden via environment variables:

| Variable                  | Overrides           |
|---------------------------|---------------------|
| `MY_TASK_SYNC_API_KEY`    | `[api].api_key`     |
| `MY_TASK_SYNC_BASE_URL`   | `[api].base_url`    |
| `MY_TASK_DATA_FILE`       | `[sqlite].path`     |
| `RUST_LOG`                | tracing filter      |

The daemon refuses to start if `api_key` or `base_url` cannot be resolved
(no silent defaults).

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

# Run a single cycle manually
my-task-sync --once

# Inspect what would be sent without touching the API or state
my-task-sync --once --dry-run
```

## What it does each cycle

1. **Push** — read SQLite tasks updated after `last_push_at`, send them
   to `POST /api/sync/tasks/push`.
2. **Pull unsynced** — fetch Neon tasks with `task_number IS NULL`,
   INSERT each into SQLite, then PATCH the assigned rowid back to Neon
   via `/api/sync/tasks/:id/number`.
3. **Pull updates** — fetch Neon tasks changed after `last_pull_at`
   from `/api/sync/tasks/changes`, applying row-level Last-Write-Wins
   based on the `updated` date. `task_reminds` are replaced wholesale on
   any update.

State (`last_push_at`, `last_pull_at`) lives in
`~/.config/my-task-sync/state.db` — separate from `tasks.db` so resetting
the daemon never risks the user's task data.

## Known limitations

- `updated` precision is one day (matches `my-task`'s `NaiveDate` schema).
  If both sides update the same task on the same day the LWW outcome is
  whichever side runs the later push, which is usually fine for a single
  human user but is documented in `docs/OVERVIEW.md`.
- HTTP retries are limited to 3 retries with `1s → 2s → 4s` backoff;
  4xx responses are surfaced immediately.
