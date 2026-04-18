//! `/api/projects` ハンドラ (T7)。
//!
//! プロジェクト一覧を返すだけの薄いエンドポイント。新規作成エンドポイント
//! は意図的に持たない — プロジェクトは `POST /api/tasks` の `projectName`
//! で透過的に登録される (`sqlite::resolve_project` が `INSERT OR IGNORE`
//! する) ため、独立した create 経路は要らない。

use axum::extract::State;
use axum::Json;
use chrono::Utc;

use super::AppState;
use crate::error::Error;
use crate::model::ProjectListResponse;
use crate::sqlite;

pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<ProjectListResponse>, Error> {
    let projects = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        sqlite::read_projects(&conn)?
    };

    Ok(Json(ProjectListResponse {
        projects,
        server_time: Utc::now(),
    }))
}
