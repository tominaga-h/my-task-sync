//! テスト共通ヘルパ。
//!
//! my-task の SQLite スキーマを in-memory DB に作成する。
//! my-task-sync 本体は既存テーブル前提で CREATE しないため、
//! テスト側でスキーマを用意する必要がある。
//!
//! スキーマは `~/lab/rust/my-task/src/db.rs` L14〜38 と完全一致させる。
//!
//! `#[allow(dead_code)]`: 各 integration test file は別 crate としてビルド
//! されるため、使わないテストからは "unused" 警告が出る。共通ヘルパ側で
//! 抑制する (個別ファイルで `#[allow]` を重ねるより一括処理)。

#![allow(dead_code)]

use rusqlite::Connection;

/// my-task 互換のスキーマを作成した in-memory 接続を返す。
pub fn make_my_task_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    conn.execute_batch(
        "
        CREATE TABLE projects (
            id    INTEGER PRIMARY KEY AUTOINCREMENT,
            name  TEXT    NOT NULL UNIQUE
        );
        CREATE TABLE tasks (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            title      TEXT    NOT NULL,
            status     TEXT    NOT NULL DEFAULT 'open'
                       CHECK(status IN ('open', 'done', 'closed')),
            source     TEXT    NOT NULL DEFAULT 'private',
            project_id INTEGER REFERENCES projects(id),
            due        TEXT,
            done_at    TEXT,
            created    TEXT    NOT NULL,
            updated    TEXT    NOT NULL,
            important  INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE task_reminds (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id   INTEGER NOT NULL REFERENCES tasks(id),
            remind_at TEXT NOT NULL
        );
        ",
    )
    .expect("create my-task schema");
    conn
}

/// 任意の updated 日付で 1 行 INSERT し、sqlite_id を返す。
pub fn insert_raw_task(
    conn: &Connection,
    title: &str,
    status: &str,
    project_id: Option<i64>,
    updated: &str,
    created: &str,
) -> i64 {
    conn.execute(
        "INSERT INTO tasks (title, status, source, project_id, due, done_at, created, updated, important)
         VALUES (?1, ?2, 'cli', ?3, NULL, NULL, ?4, ?5, 0)",
        rusqlite::params![title, status, project_id, created, updated],
    )
    .expect("insert raw task");
    conn.last_insert_rowid()
}
