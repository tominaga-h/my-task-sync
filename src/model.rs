//! Domain types and API DTOs.
//!
//! ドメイン型 (`Task` / `Status`) は SQLite モジュール内部用で、JSON には
//! 露出しない。API DTO 群 (`SyncTask` / `UnsyncedTask` / `ChangedTask`
//! / `ChangesResponse` / `PushResponse` / `PushResultRow` / `PushAction`)
//! は my-own の `/api/sync/tasks/*` 仕様に従い camelCase でシリアライズ/
//! デシリアライズする。
//!
//! NOTE: 仕様の一次情報は `docs/OVERVIEW.md` と本リポジトリのタスク指示書
//! Open Question #1 を起点とした推論で確定した形状。my-own 側の実装が
//! 入った時点で再検証すること。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------------
// Domain types (SQLite 側)
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

/// SQLite から読み出した `tasks` 行。
///
/// `id` は SQLite の rowid (= `task_number`)。新規 INSERT 時は `0` を渡し、
/// `sqlite::insert_task` が autoincrement 採番する。reminds は別テーブル
/// (`task_reminds`) なので `Task` に含めず、必要な箇所で
/// `sqlite::read_reminds_for_tasks` を別取得する。
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
// API DTO (camelCase)
// ------------------------------------------------------------------

/// `POST /api/sync/tasks/push` リクエスト body の要素。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncTask {
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

/// `GET /api/sync/tasks/unsynced` の要素 (`task_number = NULL` のタスク)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnsyncedTask {
    pub neon_id: i64,
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

/// `GET /api/sync/tasks/changes` の `tasks` 配列要素 (`task_number IS NOT NULL`)。
///
/// 行の同定は `task_number` (= SQLite の rowid) で行うので、`neon_id` は
/// このサイクルでは不要。サーバ側のレスポンスに `neonId` キーが含まれて
/// いても serde はデフォルトで未知フィールドを無視する。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedTask {
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

/// `GET /api/sync/tasks/changes` のレスポンス全体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResponse {
    pub tasks: Vec<ChangedTask>,
    pub server_time: DateTime<Utc>,
}

/// `POST /api/sync/tasks/push` のレスポンス。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<PushResultRow>,
}

/// `PushResponse.results` の要素。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResultRow {
    pub task_number: i64,
    pub action: PushAction,
    pub neon_id: i64,
}

/// `PushResultRow.action` の取りうる値 (`created` / `updated` / `skipped_newer`)。
///
/// 未知の値は `serde` がエラーとする (Fail Fast)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushAction {
    Created,
    Updated,
    SkippedNewer,
}

/// task_number 書き戻し用の小さな DTO (`PATCH /api/sync/tasks/:id/number`)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchNumberBody {
    pub task_number: i64,
}
