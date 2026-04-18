//! `GET /api/tasks` 結合テスト。
//!
//! axum Router を `oneshot` で叩き、in-memory SQLite を state に渡して
//! response の status + JSON body を検証する。認証は Bearer token 込み
//! (T2 middleware が通ること前提)。
//!
//! `common/mod.rs` は my-task 本物スキーマの in-memory DB を提供する。

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{insert_raw_task, make_my_task_db};
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

/// 3 件の multipurpose seed:
/// | id | status | project | updated    | remind     |
/// |----|--------|---------|------------|------------|
/// | 1  | open   | home    | 2026-04-10 | 2026-04-20 |
/// | 2  | done   | (null)  | 2026-04-12 | 2026-04-21 |
/// | 3  | closed | home    | 2026-04-14 | (none)     |
fn seed_three_tasks(conn: &Connection) -> (i64, i64, i64) {
    let pid_home = sqlite::resolve_project(conn, "home").expect("create project home");
    let t1 = insert_raw_task(
        conn,
        "t1",
        "open",
        Some(pid_home),
        "2026-04-10",
        "2026-04-01",
    );
    let t2 = insert_raw_task(conn, "t2", "done", None, "2026-04-12", "2026-04-02");
    let t3 = insert_raw_task(
        conn,
        "t3",
        "closed",
        Some(pid_home),
        "2026-04-14",
        "2026-04-03",
    );
    conn.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, '2026-04-20')",
        rusqlite::params![t1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, '2026-04-21')",
        rusqlite::params![t2],
    )
    .unwrap();
    (t1, t2, t3)
}

// ---------- tests ----------

#[tokio::test]
async fn returns_200_and_empty_array_when_db_is_empty() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app.oneshot(authed_get("/api/tasks")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["tasks"].as_array().unwrap().len(), 0);
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn returns_all_rows_with_project_name_and_reminds_joined() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(app.oneshot(authed_get("/api/tasks")).await.unwrap()).await;

    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);

    // t1: project=home, remind=1 件
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[0]["taskNumber"], 1);
    assert_eq!(tasks[0]["projectName"], "home");
    assert_eq!(tasks[0]["reminds"].as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["reminds"][0], "2026-04-20");

    // t2: project=null
    assert_eq!(tasks[1]["title"], "t2");
    assert!(tasks[1]["projectName"].is_null());
    assert_eq!(tasks[1]["reminds"][0], "2026-04-21");

    // t3: remind なし
    assert_eq!(tasks[2]["title"], "t3");
    assert_eq!(tasks[2]["reminds"].as_array().unwrap().len(), 0);
}

// ---------- filters ----------

#[tokio::test]
async fn status_filter_returns_only_matching_rows() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?status=done"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["status"], "done");
    assert_eq!(tasks[0]["title"], "t2");
}

#[tokio::test]
async fn since_filter_is_strict_greater_than() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    // t1.updated = 2026-04-10 → since=2026-04-10 では除外 (strict)
    // t2.updated = 2026-04-12 → 含まれる
    // t3.updated = 2026-04-14 → 含まれる
    let body = body_json(
        app.oneshot(authed_get("/api/tasks?since=2026-04-10"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t2");
    assert_eq!(tasks[1]["title"], "t3");
}

#[tokio::test]
async fn project_filter_returns_only_matching_rows() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?project=home"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[1]["title"], "t3");
}

#[tokio::test]
async fn limit_caps_the_row_count() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(app.oneshot(authed_get("/api/tasks?limit=2")).await.unwrap()).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[1]["title"], "t2");
}

#[tokio::test]
async fn filters_can_combine() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    // status=closed かつ project=home は t3 のみ
    let body = body_json(
        app.oneshot(authed_get("/api/tasks?status=closed&project=home"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["title"], "t3");
}

// ---------- 400 paths ----------

#[tokio::test]
async fn invalid_status_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?status=bogus"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("status"));
}

#[tokio::test]
async fn invalid_since_date_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?since=not-a-date"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("since"));
}

#[tokio::test]
async fn invalid_limit_type_returns_400_via_query_extractor() {
    // axum の Query 抽出が u32 parse に失敗すると 400 を自動で返す。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?limit=-1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
