//! `/api/tasks` ハンドラ。
//!
//! Phase 1:
//!   * T3: `GET   /api/tasks`           (list + filters)
//!   * T4: `GET   /api/tasks/{n}`       (single fetch, 存在しなければ 404)
//!   * T5: `POST  /api/tasks`           (create — サーバーが rowid = task_number を採番)
//!   * T6: `PATCH /api/tasks/{n}`       (partial update, 存在しなければ 404)

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::AppState;
use crate::error::Error;
use crate::model::{TaskCreateDto, TaskDto, TaskListResponse, TaskResponse};
use crate::sqlite::{self, TaskRow};

const ALLOWED_STATUSES: &[&str] = &["open", "done", "closed"];
/// `limit` 未指定時にかぶせるデフォルト上限。意図的に全件欲しい場合は
/// クライアント側で `?limit=<大きめの数>` を指定する。
const DEFAULT_LIMIT: u32 = 500;

/// `GET /api/tasks` のクエリパラメータ。どれも optional。
#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    pub status: Option<String>,
    pub since: Option<String>,
    pub project: Option<String>,
    pub limit: Option<u32>,
}

pub async fn list_tasks(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<TaskListResponse>, Error> {
    // status は SQL に渡す前に許容値チェック (CHECK 制約まかせにすると
    // 5xx になり、user 入力の誤りに対して 400 を返せないため)。
    if let Some(s) = params.status.as_deref() {
        if !ALLOWED_STATUSES.contains(&s) {
            return Err(Error::BadRequest(format!(
                "invalid status {s:?}: expected one of {ALLOWED_STATUSES:?}"
            )));
        }
    }

    // since は RFC 3339 datetime (= レスポンスの `serverTime` と同形式)。
    // クライアントが前回の `serverTime` をそのまま投げ戻せることを優先して
    // `YYYY-MM-DD` 単独は受け付けない (フォーマット分岐を増やさない)。
    // SQLite の `updated` は日単位なので、内部では UTC の日付に truncate。
    let since = match params.since.as_deref() {
        Some(s) => Some(
            DateTime::parse_from_rfc3339(s)
                .map_err(|_| {
                    Error::BadRequest(format!(
                        "invalid since {s:?}: expected RFC 3339 datetime \
                         (e.g. 2026-04-18T13:00:00Z)"
                    ))
                })?
                .with_timezone(&Utc)
                .date_naive(),
        ),
        None => None,
    };

    // limit 未指定は DEFAULT_LIMIT で蓋をする (DoS 回避)。
    let limit = Some(params.limit.unwrap_or(DEFAULT_LIMIT));

    // Mutex を短時間 (SQL 2 本分) だけ保持し、`.await` はまたがない。
    let (tasks, reminds_map) = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tasks = sqlite::read_tasks_filtered(
            &conn,
            params.status.as_deref(),
            since,
            params.project.as_deref(),
            limit,
        )?;
        let ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
        let reminds = sqlite::read_reminds_for_tasks(&conn, &ids)?;
        (tasks, reminds)
    };

    let dtos: Vec<TaskDto> = tasks
        .into_iter()
        .map(|t| {
            let reminds = reminds_map.get(&t.id).cloned().unwrap_or_default();
            TaskDto::from_task(t, reminds)
        })
        .collect();

    Ok(Json(TaskListResponse {
        tasks: dtos,
        server_time: Utc::now(),
    }))
}

// ----------------------------------------------------------------------
// GET /api/tasks/{task_number} (T4)
// ----------------------------------------------------------------------

/// 単一 task の取得。存在しなければ 404。
///
/// パスの非数値は axum の `Path<i64>` 抽出器がパース失敗で 400 を返す
/// (`PATCH` と同じ経路)。
pub async fn get_task(
    State(state): State<AppState>,
    Path(task_number): Path<i64>,
) -> Result<Json<TaskResponse>, Error> {
    let (task, reminds) = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let task = sqlite::read_task_by_id(&conn, task_number)?.ok_or(Error::NotFound)?;
        let reminds_map = sqlite::read_reminds_for_tasks(&conn, &[task_number])?;
        let reminds = reminds_map.get(&task_number).cloned().unwrap_or_default();
        (task, reminds)
    };

    Ok(Json(TaskResponse {
        task: TaskDto::from_task(task, reminds),
        server_time: Utc::now(),
    }))
}

// ----------------------------------------------------------------------
// POST /api/tasks (T5)
// ----------------------------------------------------------------------

/// 新規 task を作成し、採番された `taskNumber` 入りの完全な DTO を返す。
///
/// body の `taskNumber` はサーバー採番の約束 (docs/SERVER_DESIGN.md) を
/// 守るため明示的に拒否する (400)。parse 時に deny_unknown_fields を
/// 使わないのは、error response を 400 にしたいためで、axum の
/// `Json<T>` 抽出器が返す 422 では仕様と合わない。Value で受けてから
/// 手動 validate → TaskCreateDto にマッピングする。
pub async fn create_task(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<TaskResponse>), Error> {
    if body.get("taskNumber").is_some() {
        return Err(Error::BadRequest(
            "taskNumber must not be in request body (server assigns SQLite rowid)".into(),
        ));
    }

    let dto: TaskCreateDto = serde_json::from_value(body)
        .map_err(|e| Error::BadRequest(format!("invalid task body: {e}")))?;

    if !ALLOWED_STATUSES.contains(&dto.status.as_str()) {
        return Err(Error::BadRequest(format!(
            "invalid status {:?}: expected one of {:?}",
            dto.status, ALLOWED_STATUSES
        )));
    }

    // INSERT + reminds + read-back を 1 トランザクションにまとめる。reminds
    // の途中で失敗したら task 本体も rollback される。読み戻しも tx 内で行う
    // ことで、commit 後に別プロセス (my-task CLI) が書き換えた状態を返す余地
    // を消し、read-after-write 一貫性を保証する。
    let (task, reminds) = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tx = conn.unchecked_transaction()?;
        let row = TaskRow {
            title: &dto.title,
            status: &dto.status,
            source: &dto.source,
            project: dto.project_name.as_deref(),
            due: dto.due,
            done_at: dto.done_at,
            created: dto.created_at.date_naive(),
            updated: dto.updated_at.date_naive(),
            important: dto.important,
        };
        let id = sqlite::insert_task_row(&tx, &row, None)?;
        sqlite::replace_reminds(&tx, id, &dto.reminds)?;

        // tx 内で読み戻し (project 透過作成や status の正規化が反映された
        // 状態でクライアントに渡すため + read-after-write 保証)。
        let task = sqlite::read_task_by_id(&tx, id)?
            .ok_or_else(|| Error::Config("just-inserted task disappeared from SQLite".into()))?;
        let reminds_map = sqlite::read_reminds_for_tasks(&tx, &[id])?;
        let reminds = reminds_map.get(&id).cloned().unwrap_or_default();

        tx.commit()?;
        (task, reminds)
    };

    Ok((
        StatusCode::CREATED,
        Json(TaskResponse {
            task: TaskDto::from_task(task, reminds),
            server_time: Utc::now(),
        }),
    ))
}

// ----------------------------------------------------------------------
// PATCH /api/tasks/{task_number} (T6)
// ----------------------------------------------------------------------

/// PATCH body で認識するキー。allowlist 外のキーは 400 (クライアントの
/// タイポを silently 捨てないため)。
const PATCH_ALLOWED_KEYS: &[&str] = &[
    "title",
    "status",
    "source",
    "projectName",
    "due",
    "doneAt",
    "important",
    "updatedAt",
    "createdAt",
    "reminds",
];

/// 部分更新ハンドラ。送られてきたフィールドだけ上書きし、未送信フィールドは
/// 既存値を維持する。nullable フィールド (`projectName` / `due` / `doneAt`)
/// は `null` 送信で明示的にクリアできる。`reminds` は配列送信で全置換、
/// 未送信なら既存を保持。
///
/// `updatedAt` だけは例外的に、未送信のとき **`Utc::now()` で auto-bump する**。
/// 編集操作があれば更新日時が進む、という意味論を my-task-sync 側で担保する。
/// クライアントが明示的に `updatedAt` を送った場合はその値で上書き。
///
/// body に `taskNumber` が含まれていれば 400 (URL 側が唯一の権威)。
/// タスクが存在しなければ 404。
pub async fn patch_task(
    State(state): State<AppState>,
    Path(task_number): Path<i64>,
    Json(body): Json<Value>,
) -> Result<Json<TaskResponse>, Error> {
    // ---- body 形式 / 未知キー / taskNumber の事前検証 ----
    let obj = body
        .as_object()
        .ok_or_else(|| Error::BadRequest("body must be a JSON object".into()))?;
    if obj.contains_key("taskNumber") {
        return Err(Error::BadRequest(
            "taskNumber must not be in request body (URL path is authoritative)".into(),
        ));
    }
    for key in obj.keys() {
        if !PATCH_ALLOWED_KEYS.contains(&key.as_str()) {
            return Err(Error::BadRequest(format!("unknown field: {key:?}")));
        }
    }

    // status は早めに validate (後で再検査しないで済むように)。
    if let Some(v) = obj.get("status") {
        let s = v
            .as_str()
            .ok_or_else(|| Error::BadRequest("status must be a string".into()))?;
        if !ALLOWED_STATUSES.contains(&s) {
            return Err(Error::BadRequest(format!(
                "invalid status {s:?}: expected one of {ALLOWED_STATUSES:?}"
            )));
        }
    }

    // ---- 短時間ロックで read → merge → write → read-back ----
    let (task, reminds) = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tx = conn.unchecked_transaction()?;

        let existing = sqlite::read_task_by_id(&tx, task_number)?.ok_or(Error::NotFound)?;

        // フィールドごとに merge。nullable は「未送信 / null / 値」の 3 状態。
        let title = patch_required_string(&body, "title", &existing.title)?;
        let status = patch_required_string(&body, "status", existing.status.as_str())?;
        let source = patch_required_string(&body, "source", &existing.source)?;
        let project = patch_nullable_string(&body, "projectName", existing.project.clone())?;
        let due = patch_nullable_date(&body, "due", existing.due)?;
        let done_at = patch_nullable_date(&body, "doneAt", existing.done_at)?;
        let important = patch_required_bool(&body, "important", existing.important)?;
        let created = patch_required_date_from_datetime(&body, "createdAt", existing.created)?;
        // updatedAt は特別扱い: 未送信なら now() で auto-bump。
        let updated = match body.get("updatedAt") {
            None => Utc::now().date_naive(),
            Some(v) => parse_datetime_to_date(v, "updatedAt")?,
        };

        let row = TaskRow {
            title: &title,
            status: &status,
            source: &source,
            project: project.as_deref(),
            due,
            done_at,
            created,
            updated,
            important,
        };
        sqlite::update_task_row(&tx, task_number, &row)?;

        // reminds は送られたときだけ全置換。null は 400。
        if let Some(v) = body.get("reminds") {
            let new_reminds = parse_reminds_array(v)?;
            sqlite::replace_reminds(&tx, task_number, &new_reminds)?;
        }

        // tx 内で読み戻し (read-after-write 保証 — commit 後に別プロセスが
        // 書き換えた状態をレスポンスに混ぜない)。
        let task = sqlite::read_task_by_id(&tx, task_number)?
            .ok_or_else(|| Error::Config("task disappeared after UPDATE".into()))?;
        let reminds_map = sqlite::read_reminds_for_tasks(&tx, &[task_number])?;
        let reminds = reminds_map.get(&task_number).cloned().unwrap_or_default();

        tx.commit()?;
        (task, reminds)
    };

    Ok(Json(TaskResponse {
        task: TaskDto::from_task(task, reminds),
        server_time: Utc::now(),
    }))
}

// ----- PATCH helpers --------------------------------------------------
//
// どれも共通して「未送信 → 既存値をそのまま返す」を基本動作にする。
// 型ごとに null をどう解釈するかだけ違う:
//   * required_*: null は許さない (400)
//   * nullable_*: null を「クリア」と解釈して None を返す

fn patch_required_string(body: &Value, key: &str, existing: &str) -> Result<String, Error> {
    match body.get(key) {
        None => Ok(existing.to_string()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(Error::BadRequest(format!("{key} must be a string"))),
    }
}

fn patch_required_bool(body: &Value, key: &str, existing: bool) -> Result<bool, Error> {
    match body.get(key) {
        None => Ok(existing),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(Error::BadRequest(format!("{key} must be a boolean"))),
    }
}

fn patch_nullable_string(
    body: &Value,
    key: &str,
    existing: Option<String>,
) -> Result<Option<String>, Error> {
    match body.get(key) {
        None => Ok(existing),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(Error::BadRequest(format!("{key} must be a string or null"))),
    }
}

fn patch_nullable_date(
    body: &Value,
    key: &str,
    existing: Option<NaiveDate>,
) -> Result<Option<NaiveDate>, Error> {
    match body.get(key) {
        None => Ok(existing),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| Error::BadRequest(format!("{key} must be YYYY-MM-DD or null"))),
        Some(_) => Err(Error::BadRequest(format!(
            "{key} must be a date string or null"
        ))),
    }
}

fn patch_required_date_from_datetime(
    body: &Value,
    key: &str,
    existing: NaiveDate,
) -> Result<NaiveDate, Error> {
    match body.get(key) {
        None => Ok(existing),
        Some(v) => parse_datetime_to_date(v, key),
    }
}

/// RFC 3339 datetime を UTC 日付に truncate。SQLite 側の粒度 (日単位) に
/// 合わせる。non-string / parse 失敗 / null はすべて 400。
fn parse_datetime_to_date(v: &Value, key: &str) -> Result<NaiveDate, Error> {
    let s = v
        .as_str()
        .ok_or_else(|| Error::BadRequest(format!("{key} must be an RFC 3339 datetime string")))?;
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc).date_naive())
        .map_err(|_| {
            Error::BadRequest(format!(
                "{key} must be an RFC 3339 datetime (e.g. 2026-04-18T13:00:00Z)"
            ))
        })
}

fn parse_reminds_array(v: &Value) -> Result<Vec<NaiveDate>, Error> {
    let arr = v
        .as_array()
        .ok_or_else(|| Error::BadRequest("reminds must be an array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| Error::BadRequest("reminds items must be YYYY-MM-DD strings".into()))?;
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| Error::BadRequest(format!("invalid remind date: {s:?}")))?;
        out.push(d);
    }
    Ok(out)
}
