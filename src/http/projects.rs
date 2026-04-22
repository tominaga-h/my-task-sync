//! `/api/projects` ハンドラ。
//!
//! Phase 1 (T7):
//!   * `GET  /api/projects`           — list (一覧)
//!
//! Phase 1.2 (#100 / v0.2.0):
//!   * `POST   /api/projects`          — create (名前指定で新規作成)
//!   * `PATCH  /api/projects/{id}`     — rename (名前変更)
//!   * `DELETE /api/projects/{id}`     — delete (紐づくタスク 0 件のときのみ)
//!
//! 重複名は 409 Conflict、名称長 1〜200、trim 後保存。削除時にタスクが
//! 紐づいていれば 409 Conflict で `"project has N tasks"` を返す (UI 側で
//! 正規表現パースするのでメッセージ文言は変更禁止)。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use serde_json::Value;

use super::AppState;
use crate::error::Error;
use crate::model::{ProjectListResponse, ProjectResponse};
use crate::sqlite;

/// `tasks.title` と揃えるほど厳しくはないが、UI 表示を想定して上限 200 文字。
const MAX_NAME_LEN: usize = 200;

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

// ----------------------------------------------------------------------
// POST /api/projects
// ----------------------------------------------------------------------

/// 新規プロジェクトを作成する。body は `{"name": "..."}` のみ。
///
/// `name` は trim して長さ 1〜200 を要求。空文字・空白のみ・超過は 400。
/// 重複名 (大文字小文字区別あり — SQLite のデフォルト `TEXT` 比較に従う) は
/// 409 Conflict。未知フィールドや非オブジェクト body も 400。
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<ProjectResponse>), Error> {
    let name = parse_name_from_body(&body)?;

    // INSERT + 重複判定 + read-back を 1 トランザクションにまとめる。
    // 事前の `read_project_by_name` → INSERT の 2 段にすると、2 本の POST が
    // "両方 None を見てから両方 INSERT" という race を踏み得る。
    // トランザクション内で事前チェック→INSERT し、それでもなお同時 INSERT で
    // UNIQUE 違反が出た場合は catch して 409 に落とす (concurrency 対策)。
    let project = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tx = conn.unchecked_transaction()?;

        if sqlite::read_project_by_name(&tx, &name)?.is_some() {
            return Err(Error::Conflict("project name already exists".into()));
        }

        let project = match sqlite::insert_project(&tx, &name) {
            Ok(p) => p,
            Err(e) => return Err(map_unique_to_conflict(e)),
        };

        // 念のため read-back (UNIQUE 制約の通過と rowid 確定を確認)。
        let fresh = sqlite::read_project_by_id(&tx, project.id)?
            .ok_or_else(|| Error::Config("just-inserted project disappeared from SQLite".into()))?;

        tx.commit()?;
        fresh
    };

    Ok((
        StatusCode::CREATED,
        Json(ProjectResponse {
            project,
            server_time: Utc::now(),
        }),
    ))
}

// ----------------------------------------------------------------------
// PATCH /api/projects/{id}
// ----------------------------------------------------------------------

/// プロジェクトをリネームする。`id` のプロジェクトが無ければ 404、
/// 新名称が別 id と衝突すれば 409、body が不正なら 400。
/// 現在と同じ名称を送ってきた場合は no-op (200 + 現状を返す)。
pub async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Result<Json<ProjectResponse>, Error> {
    let new_name = parse_name_from_body(&body)?;

    let project = {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tx = conn.unchecked_transaction()?;

        let existing = sqlite::read_project_by_id(&tx, id)?.ok_or(Error::NotFound)?;

        // 現名称と同じなら no-op。trim 後の new_name で比較する (body に
        // 空白付きで送られたときに「同じ扱い」にしたい)。
        if existing.name == new_name {
            // tx は read-only で終わるので commit は無くても良いが、明示する。
            tx.commit()?;
            return Ok(Json(ProjectResponse {
                project: existing,
                server_time: Utc::now(),
            }));
        }

        // 別 id が同名を既に使っていれば 409。race 対策に UNIQUE 違反も
        // 最後に catch する。
        if let Some(other) = sqlite::read_project_by_name(&tx, &new_name)? {
            if other.id != id {
                return Err(Error::Conflict("project name already exists".into()));
            }
        }

        if let Err(e) = sqlite::update_project_name(&tx, id, &new_name) {
            return Err(map_unique_to_conflict(e));
        }

        let fresh = sqlite::read_project_by_id(&tx, id)?
            .ok_or_else(|| Error::Config("project disappeared after UPDATE".into()))?;

        tx.commit()?;
        fresh
    };

    Ok(Json(ProjectResponse {
        project,
        server_time: Utc::now(),
    }))
}

// ----------------------------------------------------------------------
// DELETE /api/projects/{id}
// ----------------------------------------------------------------------

/// プロジェクトを削除する。
///
/// 存在しなければ 404、紐づくタスクが 1 件でも残っていれば 409
/// (`"project has N tasks"`)。それ以外は 204 No Content を返す。
///
/// `count → delete` は **必ずトランザクション内で** 行う。別プロセスが
/// 同時にそのプロジェクトへタスクを紐付けるのを読み飛ばさないため。
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, Error> {
    {
        let conn = state
            .conn
            .lock()
            .map_err(|_| Error::Config("sqlite mutex poisoned".into()))?;
        let tx = conn.unchecked_transaction()?;

        // 404: 存在チェック。count → delete の直前に行うので 2 本読み。
        if sqlite::read_project_by_id(&tx, id)?.is_none() {
            return Err(Error::NotFound);
        }

        let n = sqlite::count_tasks_for_project(&tx, id)?;
        if n > 0 {
            // `"project has N tasks"` — UI 側で `/project has (\d+) tasks/`
            // を正規表現パースするので、文言変更禁止。
            return Err(Error::Conflict(format!("project has {n} tasks")));
        }

        sqlite::delete_project(&tx, id)?;
        tx.commit()?;
    }

    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------------
// helpers
// ----------------------------------------------------------------------

/// body を `{"name": string}` として検証し、trim 済み name を返す。
///
/// 仕様 (受け入れ基準):
///   * body が非 object → 400
///   * `name` 以外のキーを含む → 400 (unknown field)
///   * `name` 欠落 / 非 string → 400
///   * trim 後に 0 文字 → 400
///   * trim 後に 201 文字以上 (> 200) → 400
fn parse_name_from_body(body: &Value) -> Result<String, Error> {
    let obj = body
        .as_object()
        .ok_or_else(|| Error::BadRequest("body must be a JSON object".into()))?;
    for key in obj.keys() {
        if key != "name" {
            return Err(Error::BadRequest(format!("unknown field: {key:?}")));
        }
    }
    let raw = obj
        .get("name")
        .ok_or_else(|| Error::BadRequest("name is required".into()))?
        .as_str()
        .ok_or_else(|| Error::BadRequest("name must be a string".into()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::BadRequest("name must not be empty".into()));
    }
    // 文字数は char 単位でカウント (バイト長ではなく)。日本語を含む名称で
    // 直感的な上限にするため。`chars().count()` は O(n) だが 200 上限なので可。
    if trimmed.chars().count() > MAX_NAME_LEN {
        return Err(Error::BadRequest(format!(
            "name must be at most {MAX_NAME_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

/// SQLite の UNIQUE 制約違反 (`projects.name`) を `Error::Conflict` にマップ。
/// それ以外のエラーはそのまま返す。
fn map_unique_to_conflict(e: Error) -> Error {
    if let Error::Sqlite(rusqlite::Error::SqliteFailure(code, _)) = &e {
        if code.code == rusqlite::ErrorCode::ConstraintViolation {
            return Error::Conflict("project name already exists".into());
        }
    }
    e
}
