//! `/api/tasks` ハンドラ。
//!
//! Phase 1 T3 ではリスト取得 (`GET /api/tasks`) だけを実装する。
//! `/:task_number` (T4) / `POST` (T5) / `PATCH` (T6) は後続タスクで追加。

use axum::extract::{Query, State};
use axum::Json;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;

use super::AppState;
use crate::error::Error;
use crate::model::{TaskDto, TaskListResponse};
use crate::sqlite;

const DATE_FMT: &str = "%Y-%m-%d";
const ALLOWED_STATUSES: &[&str] = &["open", "done", "closed"];

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

    // since は YYYY-MM-DD の日付にパース。parse 失敗は 400。
    let since =
        match params.since.as_deref() {
            Some(s) => Some(NaiveDate::parse_from_str(s, DATE_FMT).map_err(|_| {
                Error::BadRequest(format!("invalid since {s:?}: expected YYYY-MM-DD"))
            })?),
            None => None,
        };

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
            params.limit,
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
