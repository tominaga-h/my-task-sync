//! Persistent key/value state for the sync daemon.
//!
//! `~/.config/my-task-sync/state.db` に置く小さな SQLite ファイル。
//! my-task の `tasks.db` とは別物で、`last_push_at` / `last_pull_at` のみ
//! を保持する (`OVERVIEW.md` § state.db)。
//!
//! 親ディレクトリは `open()` が自動作成する (my-task `db::open` と同じ
//! パターン) — 起動時に config dir が無いケースでも自然に動くため。

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use crate::error::Error;

/// Thread-safe wrapper around the state SQLite connection.
pub struct SyncState {
    conn: Mutex<Connection>,
}

impl SyncState {
    /// Open (or create) the state.db at `path`. Parent dir is created.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_state (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             )",
            [],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Return the value for `key`, or `None` if no row exists.
    pub fn get(&self, key: &str) -> Result<Option<String>, Error> {
        let conn = self.lock_conn()?;
        match conn.query_row(
            "SELECT value FROM sync_state WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert or update `value` for `key`.
    pub fn set(&self, key: &str, value: &str) -> Result<(), Error> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, Error> {
        self.conn
            .lock()
            .map_err(|e| Error::Config(format!("state.db mutex poisoned: {e}")))
    }
}
