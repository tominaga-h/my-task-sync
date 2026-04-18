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
/// 読み出した行 (`id` 確定済み) を表すドメイン型なので、write 経路の別
/// DTO (例: T5 の POST body, T6 の PATCH) からも `Task` を経由せずに
/// ここを通せるようにしている。
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

/// Read a single task by its `id` (= SQLite rowid = `task_number`).
///
/// 存在しなければ `Ok(None)`。POST (T5) / PATCH (T6) のレスポンスで
/// "書き込み直後に最新の値を読み戻して返す" のと、GET `/:n` (T4) で使う。
pub fn read_task_by_id(conn: &Connection, id: i64) -> Result<Option<Task>, Error> {
    let result = conn.query_row(
        "SELECT
             t.id, t.title, t.status, t.source,
             p.name AS project_name,
             t.due, t.done_at, t.created, t.updated, t.important
         FROM tasks t
         LEFT JOIN projects p ON t.project_id = p.id
         WHERE t.id = ?1",
        params![id],
        row_to_task,
    );
    match result {
        Ok(task) => Ok(Some(task)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::Sqlite(e)),
    }
}

/// Read tasks with optional filters for `GET /api/tasks`.
///
/// * `status`: exact match on `tasks.status` (`open` / `done` / `closed`).
///   validation はハンドラ側で行う想定 — ここは SQL に渡すだけ。
/// * `since`: `tasks.updated >= since` を満たす行のみ (inclusive 比較)。
///   SQLite 側の `updated` は `YYYY-MM-DD` (日単位) なので、ハンドラは
///   DateTime<Utc> を受け取ってから `date_naive()` で truncate してここへ
///   渡す想定。同日の重複受信はクライアント側で `task_number` dedup。
/// * `project`: JOIN 後の `projects.name` に完全一致。`None` のときは
///   project フィルタを掛けず、project 無しのタスクも含めて返す。
/// * `limit`: 行数上限。`None` は無制限 (SQLite の `LIMIT -1`)。
///
/// どのフィルタも `None` のときは全件返す。
pub fn read_tasks_filtered(
    conn: &Connection,
    status: Option<&str>,
    since: Option<NaiveDate>,
    project: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<Task>, Error> {
    let since_str = since.map(format_date);
    // SQLite は `LIMIT -1` を "無制限" として解釈する (公式文書)。
    let limit_val: i64 = limit.map(i64::from).unwrap_or(-1);
    let mut stmt = conn.prepare(
        "SELECT
             t.id, t.title, t.status, t.source,
             p.name AS project_name,
             t.due, t.done_at, t.created, t.updated, t.important
         FROM tasks t
         LEFT JOIN projects p ON t.project_id = p.id
         WHERE (?1 IS NULL OR t.status = ?1)
           AND (?2 IS NULL OR t.updated >= ?2)
           AND (?3 IS NULL OR p.name = ?3)
         ORDER BY t.id
         LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![status, since_str, project, limit_val], row_to_task)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// ------------------------------------------------------------------
// task_reminds
// ------------------------------------------------------------------

/// Replace all `task_reminds` rows for `task_id` with the given `dates`.
///
/// DELETE → INSERT を 1 セットで行う。POST (T5) は既存行 0 から INSERT、
/// PATCH (T6) は既存行を差し替える用途。呼び出し側は transaction 内で
/// 使うことを推奨 (部分失敗で中間状態が残らないように)。
pub fn replace_reminds(conn: &Connection, task_id: i64, dates: &[NaiveDate]) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM task_reminds WHERE task_id = ?1",
        params![task_id],
    )?;
    for d in dates {
        conn.execute(
            "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, ?2)",
            params![task_id, format_date(*d)],
        )?;
    }
    Ok(())
}

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
