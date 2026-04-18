//! my-task SQLite 読み書きの単体テスト。
//!
//! 対象: `my_task_sync::sqlite` の公開関数
//!   - resolve_project
//!   - insert_task / update_task
//!   - read_tasks_since / read_all_tasks
//!   - read_reminds_for_tasks
//!
//! my-task 本体とスキーマが一致することは前提。テストは in-memory SQLite で
//! `common::make_my_task_db()` がスキーマを作成した上で各関数を呼び出す。

mod common;

use chrono::NaiveDate;
use rusqlite::params;

use common::{insert_raw_task, make_my_task_db};
use my_task_sync::model::{Status, Task};
use my_task_sync::sqlite;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("valid date")
}

// ---------- resolve_project ----------

#[test]
fn resolve_project_creates_new_row_and_returns_id() {
    // Given
    let conn = make_my_task_db();

    // When
    let id = sqlite::resolve_project(&conn, "personal").expect("resolve new project");

    // Then: projects に 1 行あり、返された id はその行の id
    let (found_id, name): (i64, String) = conn
        .query_row("SELECT id, name FROM projects", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("row exists");
    assert_eq!(found_id, id);
    assert_eq!(name, "personal");
}

#[test]
fn resolve_project_returns_existing_id_when_name_already_present() {
    // Given: 既存プロジェクトがある
    let conn = make_my_task_db();
    conn.execute("INSERT INTO projects (name) VALUES (?1)", params!["work"])
        .unwrap();
    let existing_id: i64 = conn
        .query_row("SELECT id FROM projects WHERE name = 'work'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // When
    let id = sqlite::resolve_project(&conn, "work").expect("resolve existing");

    // Then: 同じ id が返り、projects は 1 行のまま (INSERT OR IGNORE)
    assert_eq!(id, existing_id);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "no duplicate inserted");
}

// ---------- insert_task / update_task ----------

#[test]
fn insert_task_persists_row_and_returns_sqlite_id() {
    // Given: project 名を渡す (insert_task が内部で resolve_project を呼ぶ想定)
    let conn = make_my_task_db();

    let task = Task {
        id: 0, // new task — let SQLite assign
        title: "買い物".into(),
        status: Status::Open,
        source: "web".into(),
        project: Some("personal".into()),
        due: Some(d(2026, 4, 15)),
        done_at: None,
        created: d(2026, 4, 12),
        updated: d(2026, 4, 12),
        important: false,
    };

    // When
    let new_id = sqlite::insert_task(&conn, &task).expect("insert_task");

    // Then
    assert!(new_id > 0, "sqlite rowid should be positive");
    let (title, status, project_name): (String, String, Option<String>) = conn
        .query_row(
            "SELECT t.title, t.status, p.name
             FROM tasks t LEFT JOIN projects p ON t.project_id = p.id
             WHERE t.id = ?1",
            params![new_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "買い物");
    assert_eq!(status, "open");
    assert_eq!(project_name.as_deref(), Some("personal"));
}

#[test]
fn update_task_overwrites_fields_of_existing_row() {
    // Given: 既存タスク (updated=2026-04-10)
    let conn = make_my_task_db();
    let id = insert_raw_task(&conn, "original", "open", None, "2026-04-10", "2026-04-10");

    let task = Task {
        id,
        title: "renamed".into(),
        status: Status::Done,
        source: "cli".into(),
        project: None,
        due: None,
        done_at: Some(d(2026, 4, 12)),
        created: d(2026, 4, 10),
        updated: d(2026, 4, 12),
        important: true,
    };

    // When
    sqlite::update_task(&conn, &task).expect("update_task");

    // Then: 行が書き換わっている
    let (title, status, done_at, important, updated): (
        String,
        String,
        Option<String>,
        i64,
        String,
    ) = conn
        .query_row(
            "SELECT title, status, done_at, important, updated FROM tasks WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(title, "renamed");
    assert_eq!(status, "done");
    assert_eq!(done_at.as_deref(), Some("2026-04-12"));
    assert_eq!(important, 1);
    assert_eq!(updated, "2026-04-12");
}

// ---------- read_tasks_since / read_all_tasks ----------

#[test]
fn read_tasks_since_excludes_rows_at_or_before_given_date() {
    // Given: 3 行。updated が 04-10 / 04-11 / 04-12
    let conn = make_my_task_db();
    insert_raw_task(&conn, "old", "open", None, "2026-04-10", "2026-04-10");
    insert_raw_task(&conn, "mid", "open", None, "2026-04-11", "2026-04-11");
    insert_raw_task(&conn, "new", "open", None, "2026-04-12", "2026-04-12");

    // When: 04-11 より後 (strict) を取得
    let tasks = sqlite::read_tasks_since(&conn, "2026-04-11").expect("read_tasks_since");

    // Then: 04-12 の 1 件のみ
    assert_eq!(tasks.len(), 1, "only strictly newer row returned");
    assert_eq!(tasks[0].title, "new");
}

#[test]
fn read_tasks_since_empty_date_returns_all() {
    // Given: 2 行
    let conn = make_my_task_db();
    insert_raw_task(&conn, "a", "open", None, "2026-04-10", "2026-04-10");
    insert_raw_task(&conn, "b", "open", None, "2026-04-12", "2026-04-12");

    // When: 初回起動 (空文字 or 非常に古い日付) は全件返す想定
    let tasks = sqlite::read_tasks_since(&conn, "").expect("read_tasks_since empty");

    // Then
    assert_eq!(tasks.len(), 2);
}

#[test]
fn read_all_tasks_returns_every_row_with_project_name_joined() {
    // Given: project が紐づくタスクと紐づかないタスク
    let conn = make_my_task_db();
    let pid = sqlite::resolve_project(&conn, "work").unwrap();
    insert_raw_task(
        &conn,
        "with-project",
        "open",
        Some(pid),
        "2026-04-12",
        "2026-04-12",
    );
    insert_raw_task(
        &conn,
        "no-project",
        "open",
        None,
        "2026-04-12",
        "2026-04-12",
    );

    // When
    let tasks = sqlite::read_all_tasks(&conn).expect("read_all_tasks");

    // Then: 2 件返り、project 名が正しく JOIN されている
    assert_eq!(tasks.len(), 2);
    let with = tasks.iter().find(|t| t.title == "with-project").unwrap();
    assert_eq!(with.project.as_deref(), Some("work"));
    let without = tasks.iter().find(|t| t.title == "no-project").unwrap();
    assert!(without.project.is_none());
}

// ---------- read_reminds_for_tasks ----------

#[test]
fn read_reminds_for_tasks_groups_by_task_id_only_for_requested() {
    // Given: 3 タスクに複数の reminds
    let conn = make_my_task_db();
    let t1 = insert_raw_task(&conn, "t1", "open", None, "2026-04-12", "2026-04-12");
    let t2 = insert_raw_task(&conn, "t2", "open", None, "2026-04-12", "2026-04-12");
    let t3 = insert_raw_task(&conn, "t3", "open", None, "2026-04-12", "2026-04-12");

    for &(tid, date) in &[
        (t1, "2026-04-14"),
        (t1, "2026-04-15"),
        (t2, "2026-04-20"),
        (t3, "2026-04-30"), // 未要求の ID
    ] {
        conn.execute(
            "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, ?2)",
            params![tid, date],
        )
        .unwrap();
    }

    // When: t1, t2 のみを要求
    let map = sqlite::read_reminds_for_tasks(&conn, &[t1, t2]).expect("read_reminds_for_tasks");

    // Then
    assert_eq!(map.len(), 2, "only requested ids appear in map");
    assert_eq!(map.get(&t1).map(Vec::len), Some(2));
    assert_eq!(map.get(&t2).map(Vec::len), Some(1));
    assert!(!map.contains_key(&t3), "t3 must not be included");
}

#[test]
fn read_reminds_for_tasks_returns_empty_map_when_no_reminds() {
    // Given
    let conn = make_my_task_db();
    let t1 = insert_raw_task(&conn, "t1", "open", None, "2026-04-12", "2026-04-12");

    // When
    let map = sqlite::read_reminds_for_tasks(&conn, &[t1]).unwrap();

    // Then: key が存在しない or 空 Vec — どちらも可だが、要求 id の扱いは一貫している
    let reminds = map.get(&t1).cloned().unwrap_or_default();
    assert!(reminds.is_empty());
}

#[test]
fn read_reminds_for_tasks_handles_empty_input_slice() {
    // Given
    let conn = make_my_task_db();

    // When: 要求 ID なし
    let map = sqlite::read_reminds_for_tasks(&conn, &[]).unwrap();

    // Then: SQL を発行しても空の map を返す (空 IN 句でクラッシュしないこと)
    assert!(map.is_empty());
}

// ---------- open: busy_timeout ----------

#[test]
fn open_connection_sets_busy_timeout() {
    // Given: ファイル DB を一時ディレクトリに作成 (WAL 動作確認はせず、timeout 設定のみ確認)
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("tasks.db");

    // スキーマだけ先に作る (my-task 本体が作る前提)
    let bootstrap = rusqlite::Connection::open(&path).unwrap();
    bootstrap
        .execute_batch(
            "CREATE TABLE tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT, status TEXT, source TEXT, project_id INTEGER,
                due TEXT, done_at TEXT, created TEXT, updated TEXT, important INTEGER
            );",
        )
        .unwrap();
    drop(bootstrap);

    // When
    let conn = sqlite::open(&path).expect("open my-task sqlite");

    // Then: busy_timeout が 0 でない (設計値は 3000ms だがテストは「設定されている」ことのみ確認)
    let timeout_ms: i64 = conn
        .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
        .unwrap();
    assert!(
        timeout_ms > 0,
        "busy_timeout must be set (got {timeout_ms})"
    );
}
