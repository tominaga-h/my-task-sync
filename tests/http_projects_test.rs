//! `GET /api/projects` 結合テスト (T7)。
//!
//! `common/mod.rs` の in-memory モック SQLite を state に渡して、
//! Router::oneshot でエンドポイント挙動を検証する。

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::make_my_task_db;
use rusqlite::Connection;
use serde_json::Value;
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

fn app_with(conn: Connection) -> axum::Router {
    router(AppState::new(conn, API_KEY.into()))
}

// ---------- tests ----------

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
    // resolve_project を 3 つ順に呼ぶ → id=1,2,3 で挿入される。
    // レスポンス順は id ASC (= 挿入順)。
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
    // クロスエンドポイント: POST /api/tasks で projectName=new-proj を
    // 送ると projects に透過的に INSERT される。続けて /api/projects を
    // 叩くと new-proj が見える。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let create_req = Request::builder()
        .method(axum::http::Method::POST)
        .uri("/api/tasks")
        .header("Authorization", format!("Bearer {API_KEY}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "title": "t",
                "status": "open",
                "source": "web",
                "projectName": "new-proj",
                "createdAt": "2026-04-18T10:00:00Z",
                "updatedAt": "2026-04-18T10:00:00Z",
            }))
            .unwrap(),
        ))
        .unwrap();
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
