//! `/api/projects` エンドポイント結合テスト。
//!
//! * T7:  GET  /api/projects
//! * #100/v0.2.0:
//!   * POST   /api/projects
//!   * PATCH  /api/projects/{id}
//!   * DELETE /api/projects/{id}
//!
//! `common/mod.rs` の in-memory モック SQLite を state に渡して、
//! Router::oneshot でエンドポイント挙動を検証する。

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{insert_raw_task, make_my_task_db};
use rusqlite::Connection;
use serde_json::{json, Value};
use tower::ServiceExt;

use my_task_sync::http::{router, AppState};
use my_task_sync::sqlite;

const API_KEY: &str = "test-key";

// ---------- helpers ----------

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("parse json body")
}

fn authed_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {API_KEY}"))
        .body(Body::empty())
        .expect("build request")
}

/// 認証ヘッダ付きで任意 method / body を組み立てるヘルパ。
/// `body` が None のときは Content-Type を付けない (DELETE などで空 body)。
fn authed_request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {API_KEY}"));
    match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            builder
                .body(Body::from(serde_json::to_vec(&v).unwrap()))
                .expect("build request")
        }
        None => builder.body(Body::empty()).expect("build request"),
    }
}

fn app_with(conn: Connection) -> axum::Router {
    router(AppState::new(conn, API_KEY.into(), ":memory:".into(), None))
}

// ======================================================================
// GET /api/projects (既存 T7 テスト)
// ======================================================================

#[tokio::test]
async fn returns_empty_array_when_no_projects() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app.oneshot(authed_get("/api/projects")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["projects"].as_array().unwrap().len(), 0);
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn returns_all_projects_ordered_by_insertion() {
    let conn = make_my_task_db();
    sqlite::resolve_project(&conn, "home").unwrap();
    sqlite::resolve_project(&conn, "work").unwrap();
    sqlite::resolve_project(&conn, "hobby").unwrap();

    let app = app_with(conn);
    let body = body_json(app.oneshot(authed_get("/api/projects")).await.unwrap()).await;

    let projects = body["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 3);
    assert_eq!(projects[0]["id"], 1);
    assert_eq!(projects[0]["name"], "home");
    assert_eq!(projects[1]["id"], 2);
    assert_eq!(projects[1]["name"], "work");
    assert_eq!(projects[2]["id"], 3);
    assert_eq!(projects[2]["name"], "hobby");
}

#[tokio::test]
async fn project_transparently_created_via_post_task_is_visible_here() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let create_req = authed_request(
        Method::POST,
        "/api/tasks",
        Some(json!({
            "title": "t",
            "status": "open",
            "source": "web",
            "projectName": "new-proj",
            "createdAt": "2026-04-18T10:00:00Z",
            "updatedAt": "2026-04-18T10:00:00Z",
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);

    let body = body_json(app.oneshot(authed_get("/api/projects")).await.unwrap()).await;
    let projects = body["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["name"], "new-proj");
}

#[tokio::test]
async fn without_auth_returns_401() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = Request::builder()
        .uri("/api/projects")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ======================================================================
// POST /api/projects
// ======================================================================

#[tokio::test]
async fn post_creates_project_and_returns_201() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(
        Method::POST,
        "/api/projects",
        Some(json!({"name": "alpha"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    assert_eq!(body["project"]["name"], "alpha");
    assert!(body["project"]["id"].as_i64().unwrap() > 0);
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn post_trims_surrounding_whitespace_before_insert() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(
        Method::POST,
        "/api/projects",
        Some(json!({"name": "  trimmed  "})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    assert_eq!(body["project"]["name"], "trimmed");
}

#[tokio::test]
async fn post_duplicate_name_returns_409() {
    let conn = make_my_task_db();
    sqlite::insert_project(&conn, "dup").unwrap();
    let app = app_with(conn);

    let req = authed_request(Method::POST, "/api/projects", Some(json!({"name": "dup"})));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("already exists"));
}

#[tokio::test]
async fn post_empty_name_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(Method::POST, "/api/projects", Some(json!({"name": ""})));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_whitespace_only_name_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(
        Method::POST,
        "/api/projects",
        Some(json!({"name": "   \t  "})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_name_too_long_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    // 201 文字 = 上限 200 を超える
    let long = "a".repeat(201);
    let req = authed_request(Method::POST, "/api/projects", Some(json!({"name": long})));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_unknown_field_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(
        Method::POST,
        "/api/projects",
        Some(json!({"name": "ok", "color": "red"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_non_object_body_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    // 配列は object ではない
    let req = authed_request(Method::POST, "/api/projects", Some(json!(["a", "b"])));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_without_auth_returns_401() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/projects")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"x"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ======================================================================
// PATCH /api/projects/{id}
// ======================================================================

#[tokio::test]
async fn patch_renames_project_and_returns_200() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "old").unwrap();
    let app = app_with(conn);

    let req = authed_request(
        Method::PATCH,
        &format!("/api/projects/{}", p.id),
        Some(json!({"name": "new"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["project"]["id"], p.id);
    assert_eq!(body["project"]["name"], "new");
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn patch_same_name_is_noop_200() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "same").unwrap();
    let app = app_with(conn);

    let req = authed_request(
        Method::PATCH,
        &format!("/api/projects/{}", p.id),
        Some(json!({"name": "same"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["project"]["name"], "same");
}

#[tokio::test]
async fn patch_missing_id_returns_404() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(
        Method::PATCH,
        "/api/projects/9999",
        Some(json!({"name": "x"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_duplicate_name_on_another_id_returns_409() {
    let conn = make_my_task_db();
    let _a = sqlite::insert_project(&conn, "alpha").unwrap();
    let b = sqlite::insert_project(&conn, "beta").unwrap();
    let app = app_with(conn);

    let req = authed_request(
        Method::PATCH,
        &format!("/api/projects/{}", b.id),
        Some(json!({"name": "alpha"})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("already exists"));
}

#[tokio::test]
async fn patch_empty_name_returns_400() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "old").unwrap();
    let app = app_with(conn);

    let req = authed_request(
        Method::PATCH,
        &format!("/api/projects/{}", p.id),
        Some(json!({"name": ""})),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_without_auth_returns_401() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "old").unwrap();
    let app = app_with(conn);

    let req = Request::builder()
        .method(Method::PATCH)
        .uri(format!("/api/projects/{}", p.id))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"new"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ======================================================================
// DELETE /api/projects/{id}
// ======================================================================

#[tokio::test]
async fn delete_project_without_tasks_returns_204() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "lonely").unwrap();
    let app = app_with(conn);

    let req = authed_request(Method::DELETE, &format!("/api/projects/{}", p.id), None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // body は空
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 10)
        .await
        .unwrap();
    assert!(bytes.is_empty(), "204 body must be empty");
}

#[tokio::test]
async fn delete_project_with_tasks_returns_409_with_count_message() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "busy").unwrap();
    for i in 0..2 {
        let title = format!("t{i}");
        insert_raw_task(
            &conn,
            &title,
            "open",
            Some(p.id),
            "2026-04-12",
            "2026-04-12",
        );
    }
    let app = app_with(conn);

    let req = authed_request(Method::DELETE, &format!("/api/projects/{}", p.id), None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    let body = body_json(resp).await;
    // UI 側で `/project has (\d+) tasks/` を正規表現パースするので、
    // メッセージは "project has 2 tasks" である必要がある。
    assert_eq!(body["error"], "project has 2 tasks");
}

#[tokio::test]
async fn delete_missing_project_returns_404() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = authed_request(Method::DELETE, "/api/projects/9999", None);
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_without_auth_returns_401() {
    let conn = make_my_task_db();
    let p = sqlite::insert_project(&conn, "lonely").unwrap();
    let app = app_with(conn);

    let req = Request::builder()
        .method(Method::DELETE)
        .uri(format!("/api/projects/{}", p.id))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
