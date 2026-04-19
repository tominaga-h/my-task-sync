//! `GET /api/status` 結合テスト (T11)。
//!
//! Router::oneshot で /api/status を叩き、3 状態のうち以下の 2 つを pin:
//! - ngrok disabled (domain = None) → `{ "enabled": false }`
//! - ngrok enabled but unreachable (localhost:4040 に誰もいない) →
//!   `{ "enabled": true, "reachable": false, "error": "..." }`
//!
//! "up" 状態 (ngrok が実際に動いていて tunnel が張れてる) のテストは
//! 外部プロセスに依存するため integration 外で行う (手動 smoke test か
//! T13 の CP6)。parse_tunnel_status 単体のテストは src/http/status.rs
//! inline でカバー済み。

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::make_my_task_db;
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

use my_task_sync::http::{router, AppState};

const API_KEY: &str = "test-key";

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("parse json body")
}

fn get_status_request() -> Request<Body> {
    // 認証ヘッダなしで叩けることを意図的に pin (運用確認用エンドポイント)
    Request::builder()
        .uri("/api/status")
        .body(Body::empty())
        .expect("build request")
}

fn app_with_ngrok(conn: Connection, ngrok_domain: Option<String>) -> axum::Router {
    router(AppState::new(
        conn,
        API_KEY.into(),
        "/tmp/status-test.db".into(),
        ngrok_domain,
    ))
}

// ---------- server section ----------

#[tokio::test]
async fn status_returns_200_without_auth() {
    // Bearer token なしでも 200 — `/api/status` は認証 middleware の外側。
    let conn = make_my_task_db();
    let app = app_with_ngrok(conn, None);
    let resp = app.oneshot(get_status_request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn status_server_section_contains_version_uptime_sqlite() {
    let conn = make_my_task_db();
    let app = app_with_ngrok(conn, None);
    let body = body_json(app.oneshot(get_status_request()).await.unwrap()).await;

    let server = &body["server"];
    assert!(server["version"].is_string(), "version missing");
    // Cargo.toml の version (0.1.0) が反映されているはず
    assert_eq!(server["version"], env!("CARGO_PKG_VERSION"));

    // uptime は u64。起動直後なので 0 or 1
    assert!(
        server["uptimeSeconds"].as_u64().is_some(),
        "uptimeSeconds not a u64"
    );

    // SQLite ヘルス — make_my_task_db が開けたので ok=true、path は渡した値
    assert_eq!(server["sqlite"]["path"], "/tmp/status-test.db");
    assert_eq!(server["sqlite"]["ok"], true);
}

// ---------- ngrok section ----------

#[tokio::test]
async fn status_ngrok_disabled_when_no_domain() {
    // domain = None → `{ "enabled": false }` のみ (他フィールドは省略)
    let conn = make_my_task_db();
    let app = app_with_ngrok(conn, None);
    let body = body_json(app.oneshot(get_status_request()).await.unwrap()).await;

    let ngrok = &body["ngrok"];
    assert_eq!(ngrok["enabled"], false);
    // skip_serializing_if で消えているはず
    assert!(ngrok.get("reachable").is_none());
    assert!(ngrok.get("publicUrl").is_none());
    assert!(ngrok.get("error").is_none());
}

#[tokio::test]
async fn status_ngrok_unreachable_when_admin_api_not_running() {
    // domain 設定あり + localhost:4040 に誰もいない (CI / 通常 test 環境)。
    // 理論上 test 実行中に 4040 を掴んでる別プロセスがあれば false positive
    // になるが、そのときは up として返るのも許容するように "reachable が
    // bool を返す" ことだけ pin する。
    let conn = make_my_task_db();
    let app = app_with_ngrok(conn, Some("x.ngrok-free.dev".into()));
    let body = body_json(app.oneshot(get_status_request()).await.unwrap()).await;

    let ngrok = &body["ngrok"];
    assert_eq!(ngrok["enabled"], true);
    // reachable フィールドは true / false のどちらかが返る
    assert!(
        ngrok["reachable"].is_boolean(),
        "reachable must be bool, got {:?}",
        ngrok["reachable"]
    );
    // 到達不能なら error メッセージが入る (テスト実行環境で 4040 が空なら
    // この分岐に落ちる)
    if ngrok["reachable"] == false {
        assert!(
            ngrok["error"].is_string(),
            "unreachable path must have error string"
        );
    }
}
