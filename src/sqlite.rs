//! my-task SQLite read/write helpers.
//!
//! my-task が所有する `tasks` / `projects` / `task_reminds` テーブルを
//! 前提とする (CREATE はしない)。WAL モードで運用される前提なので、
//! 書き込み競合に備えて `busy_timeout` を設定して接続を返す。

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::{params, params_from_iter, Connection, Row};

use crate::error::Error;
use crate::model::{Status, Task};

const BUSY_TIMEOUT_MS: u64 = 5000;
const DATE_FMT: &str = "%Y-%m-%d";

/// Open a connection to the given SQLite file.
///
/// `path` の親ディレクトリが無ければ作成 (sync daemon 起動直後の自然な
/// 失敗回避)。`busy_timeout` を設定する。schema 作成はしない。
pub fn open(path: &Path) -> Result<Connection, Error> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            // 親ディレクトリの作成失敗は I/O エラーとして伝播 (黙って続けない)。
            std::fs::create_dir_all(dir)?;
        }
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    Ok(conn)
}

// ------------------------------------------------------------------
// projects
// ------------------------------------------------------------------

/// Get an existing project id by name, or insert and return the new id.
pub fn resolve_project(conn: &Connection, name: &str) -> Result<i64, Error> {
    conn.execute(
        "INSERT OR IGNORE INTO projects (name) VALUES (?1)",
        params![name],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM projects WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(id)
}

// ------------------------------------------------------------------
// tasks: insert / update
// ------------------------------------------------------------------

/// Borrow-friendly view of a tasks row used by the insert/update helpers.
///
/// SQLite に書き込む値の集合を 1 箇所に集約する。`Task` は SQLite から
/// 読み出した行 (`id` 確定済み) を表すドメイン型なので、UnsyncedTask /
/// ChangedTask など別 DTO から書き込む経路でも `Task` を作らずにここを
/// 経由できるようにしている。
pub struct TaskRow<'a> {
    pub title: &'a str,
    /// `tasks.status` の生値。CHECK 制約 (`open` / `done` / `closed`) で
    /// 不正な値は SQLite が拒否する (Fail Fast)。
    pub status: &'a str,
    pub source: &'a str,
    pub project: Option<&'a str>,
    pub due: Option<NaiveDate>,
    pub done_at: Option<NaiveDate>,
    pub created: NaiveDate,
    pub updated: NaiveDate,
    pub important: bool,
}

/// Insert one row into `tasks`.
///
/// `explicit_id` を `Some(n)` にすると `id` 列を明示して INSERT (Neon →
/// SQLite で `task_number` を保つ用途)。`None` のときは AUTOINCREMENT に
/// 任せ、採番された rowid を返す。
pub fn insert_task_row(
    conn: &Connection,
    row: &TaskRow<'_>,
    explicit_id: Option<i64>,
) -> Result<i64, Error> {
    let project_id = match row.project {
        Some(name) => Some(resolve_project(conn, name)?),
        None => None,
    };
    let due_str = row.due.map(format_date);
    let done_at_str = row.done_at.map(format_date);
    let created_str = format_date(row.created);
    let updated_str = format_date(row.updated);
    let important_int = bool_to_int(row.important);

    if let Some(id) = explicit_id {
        conn.execute(
            "INSERT INTO tasks
                (id, title, status, source, project_id,
                 due, done_at, created, updated, important)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                row.title,
                row.status,
                row.source,
                project_id,
                due_str,
                done_at_str,
                created_str,
                updated_str,
                important_int,
            ],
        )?;
        Ok(id)
    } else {
        conn.execute(
            "INSERT INTO tasks
                (title, status, source, project_id,
                 due, done_at, created, updated, important)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.title,
                row.status,
                row.source,
                project_id,
                due_str,
                done_at_str,
                created_str,
                updated_str,
                important_int,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

/// Overwrite an existing tasks row identified by `id`.
pub fn update_task_row(conn: &Connection, id: i64, row: &TaskRow<'_>) -> Result<(), Error> {
    let project_id = match row.project {
        Some(name) => Some(resolve_project(conn, name)?),
        None => None,
    };
    conn.execute(
        "UPDATE tasks SET
            title = ?1,
            status = ?2,
            source = ?3,
            project_id = ?4,
            due = ?5,
            done_at = ?6,
            created = ?7,
            updated = ?8,
            important = ?9
         WHERE id = ?10",
        params![
            row.title,
            row.status,
            row.source,
            project_id,
            row.due.map(format_date),
            row.done_at.map(format_date),
            format_date(row.created),
            format_date(row.updated),
            bool_to_int(row.important),
            id,
        ],
    )?;
    Ok(())
}

/// Insert a new task and return the assigned rowid.
///
/// `task.id` は無視される (SQLite が AUTOINCREMENT で採番)。`task.project`
/// が `Some` のときは `resolve_project` で id を解決する。
pub fn insert_task(conn: &Connection, task: &Task) -> Result<i64, Error> {
    insert_task_row(conn, &task_to_row(task), None)
}

/// Overwrite an existing task identified by `task.id`.
pub fn update_task(conn: &Connection, task: &Task) -> Result<(), Error> {
    update_task_row(conn, task.id, &task_to_row(task))
}

fn task_to_row(task: &Task) -> TaskRow<'_> {
    TaskRow {
        title: &task.title,
        status: task.status.as_str(),
        source: &task.source,
        project: task.project.as_deref(),
        due: task.due,
        done_at: task.done_at,
        created: task.created,
        updated: task.updated,
        important: task.important,
    }
}

// ------------------------------------------------------------------
// tasks: read
// ------------------------------------------------------------------

/// Read every task whose `updated` is strictly greater than `since`.
///
/// `since` は `YYYY-MM-DD` (or empty string for "all"). 比較は SQLite の
/// 文字列辞書比較で行うため、空文字を渡すと全件返る。
pub fn read_tasks_since(conn: &Connection, since: &str) -> Result<Vec<Task>, Error> {
    let mut stmt = conn.prepare(
        "SELECT
             t.id, t.title, t.status, t.source,
             p.name AS project_name,
             t.due, t.done_at, t.created, t.updated, t.important
         FROM tasks t
         LEFT JOIN projects p ON t.project_id = p.id
         WHERE t.updated > ?1
         ORDER BY t.id",
    )?;
    let rows = stmt.query_map(params![since], row_to_task)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Read every task (project name JOINed). Equivalent to `read_tasks_since(_, "")`.
pub fn read_all_tasks(conn: &Connection) -> Result<Vec<Task>, Error> {
    read_tasks_since(conn, "")
}

// ------------------------------------------------------------------
// task_reminds
// ------------------------------------------------------------------

/// Group reminds by `task_id` for the given task ids.
///
/// `task_ids` が空のときは空の `HashMap` を返す (空 IN 句で SQL を発行
/// せず、構文エラーにもしない)。
pub fn read_reminds_for_tasks(
    conn: &Connection,
    task_ids: &[i64],
) -> Result<HashMap<i64, Vec<NaiveDate>>, Error> {
    let mut map: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
    if task_ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?"; task_ids.len()].join(",");
    let sql = format!(
        "SELECT task_id, remind_at
           FROM task_reminds
          WHERE task_id IN ({placeholders})
          ORDER BY task_id, remind_at"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(task_ids.iter()), |row| {
        let tid: i64 = row.get(0)?;
        let date_str: String = row.get(1)?;
        Ok((tid, date_str))
    })?;
    for r in rows {
        let (tid, date_str) = r?;
        let date = parse_date(&date_str)?;
        map.entry(tid).or_default().push(date);
    }
    Ok(map)
}

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

fn row_to_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    let id: i64 = row.get(0)?;
    let title: String = row.get(1)?;
    let status_str: String = row.get(2)?;
    let source: String = row.get(3)?;
    let project: Option<String> = row.get(4)?;
    let due_str: Option<String> = row.get(5)?;
    let done_at_str: Option<String> = row.get(6)?;
    let created_str: String = row.get(7)?;
    let updated_str: String = row.get(8)?;
    let important_int: i64 = row.get(9)?;

    let due = match due_str {
        Some(s) => Some(date_from_sql(&s, 5)?),
        None => None,
    };
    let done_at = match done_at_str {
        Some(s) => Some(date_from_sql(&s, 6)?),
        None => None,
    };
    let created = date_from_sql(&created_str, 7)?;
    let updated = date_from_sql(&updated_str, 8)?;

    Ok(Task {
        id,
        title,
        status: Status::from_db_str(&status_str),
        source,
        project,
        due,
        done_at,
        created,
        updated,
        important: important_int != 0,
    })
}

fn date_from_sql(s: &str, column_idx: usize) -> rusqlite::Result<NaiveDate> {
    NaiveDate::parse_from_str(s, DATE_FMT).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            column_idx,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })
}

fn parse_date(s: &str) -> Result<NaiveDate, Error> {
    NaiveDate::parse_from_str(s, DATE_FMT)
        .map_err(|e| Error::Config(format!("invalid date {s:?} in SQLite: {e}")))
}

fn format_date(d: NaiveDate) -> String {
    d.format(DATE_FMT).to_string()
}

fn bool_to_int(b: bool) -> i64 {
    if b {
        1
    } else {
        0
    }
}
