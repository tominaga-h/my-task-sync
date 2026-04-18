//! Domain types and API DTOs.
//!
//! ドメイン型 (`Task` / `Status`) は SQLite I/O 内部用で、JSON 面には露出
//! しない。API DTO (`TaskDto` / `TaskListResponse`) は my-own から呼ばれる
//! HTTP エンドポイントの shape を表し、camelCase でシリアライズする。
//!
//! v1 にあった `SyncTask` / `UnsyncedTask` / `ChangedTask` / `ChangesResponse`
//! / `PushResponse` / `PushAction` / `PatchNumberBody` は polling daemon
//! 時代の push/pull API 向けで、v2 の逆方向 REST では不要 — T3 で削除。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------
// Domain types (SQLite-side)
// ------------------------------------------------------------------

/// my-task の `tasks.status` 列 (`open` / `done` / `closed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Open,
    Done,
    Closed,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Done => "done",
            Status::Closed => "closed",
        }
    }

    /// 不明な値は `Open` にフォールバック (my-task `model.rs` と同じ挙動)。
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "done" => Status::Done,
            "closed" => Status::Closed,
            _ => Status::Open,
        }
    }
}

/// SQLite から読み出した `tasks` 行。reminds は別テーブルなので別途
/// `sqlite::read_reminds_for_tasks` で取得する。
#[derive(Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub status: Status,
    pub source: String,
    pub project: Option<String>,
    pub due: Option<NaiveDate>,
    pub done_at: Option<NaiveDate>,
    pub created: NaiveDate,
    pub updated: NaiveDate,
    pub important: bool,
}

// ------------------------------------------------------------------
// API DTOs (camelCase JSON)
// ------------------------------------------------------------------

/// HTTP 境界における task 表現。`task_number` は SQLite の rowid に等しい。
///
/// `createdAt` / `updatedAt` は ISO 8601 文字列 (`DateTime<Utc>`) として
/// 返す。SQLite 側は `NaiveDate` (日単位) なので、変換時は 00:00:00 UTC を
/// 割り当てる。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDto {
    pub task_number: i64,
    pub title: String,
    pub status: String,
    pub source: String,
    pub project_name: Option<String>,
    pub due: Option<NaiveDate>,
    pub done_at: Option<NaiveDate>,
    pub important: bool,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub reminds: Vec<NaiveDate>,
}

impl TaskDto {
    /// SQLite から読み出した `Task` + 関連 reminds を HTTP DTO に変換。
    pub fn from_task(task: Task, reminds: Vec<NaiveDate>) -> Self {
        Self {
            task_number: task.id,
            title: task.title,
            status: task.status.as_str().to_string(),
            source: task.source,
            project_name: task.project,
            due: task.due,
            done_at: task.done_at,
            important: task.important,
            updated_at: date_to_dt(task.updated),
            created_at: date_to_dt(task.created),
            reminds,
        }
    }
}

/// `GET /api/tasks` のレスポンス。`serverTime` はクライアントが次回の
/// `since` として使えるタイムスタンプ。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListResponse {
    pub tasks: Vec<TaskDto>,
    pub server_time: DateTime<Utc>,
}

/// `POST /api/tasks` body / `PATCH /api/tasks/:n` body (後者は T6)。
/// `taskNumber` は含めない — サーバーが SQLite rowid を採番する約束。
/// body に `taskNumber` が含まれていたら 400 (ハンドラ側で早期検出)。
///
/// `important` / `reminds` は省略可 (デフォルト false / 空配列)。
/// 未定義フィールドは `deny_unknown_fields` でパースエラー (→ 400) に
/// する。クライアントのタイポ (例: `reminders`) がサイレントに
/// 捨てられて空データで INSERT されるのを防ぐため。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskCreateDto {
    pub title: String,
    pub status: String,
    pub source: String,
    pub project_name: Option<String>,
    pub due: Option<NaiveDate>,
    pub done_at: Option<NaiveDate>,
    #[serde(default)]
    pub important: bool,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub reminds: Vec<NaiveDate>,
}

/// `GET /:n` / `POST` / `PATCH` のレスポンス共通形。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResponse {
    pub task: TaskDto,
    pub server_time: DateTime<Utc>,
}

/// `GET /api/projects` 要素。`id` / `name` のどちらも単語 1 つなので
/// camelCase rename は不要 (出ても同一形)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
}

/// `GET /api/projects` のレスポンス。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListResponse {
    pub projects: Vec<Project>,
    pub server_time: DateTime<Utc>,
}

fn date_to_dt(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0)
        .expect("00:00:00 is a valid wall time")
        .and_utc()
}
