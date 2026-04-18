//! `/api/tasks` ハンドラ。
//!
//! Phase 1:
//!   * T3: `GET  /api/tasks` (list + filters)
//!   * T5: `POST /api/tasks` (create — サーバーが rowid = task_number を採番)
//!
//! T4 `GET /:n` / T6 `PATCH /:n` は後続タスクで追加。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;

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

    // INSERT + reminds を 1 トランザクションにまとめる。reminds の途中で
    // 失敗したら task 本体も rollback されるので、DB に中間状態が残らない。
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
        tx.commit()?;

        // 書き戻し直後の行を読み直して返す (project 透過作成や status の
        // 正規化が反映された状態でクライアントに渡すため)。
        let task = sqlite::read_task_by_id(&conn, id)?
            .ok_or_else(|| Error::Config("just-inserted task disappeared from SQLite".into()))?;
        let reminds_map = sqlite::read_reminds_for_tasks(&conn, &[id])?;
        let reminds = reminds_map.get(&id).cloned().unwrap_or_default();
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
