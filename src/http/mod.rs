//! HTTP layer — axum router, shared state, ミドルウェア。
//!
//! Phase 1 のルート構成:
//!   * `/healthz` — 認証なし (運用者が token 無しで死活確認できるように)
//!   * `/api/*`   — Bearer 認証 middleware 必須 (T2)。中身のハンドラは T3〜T7
//!
//! `AppState` は `Router::with_state` で全ハンドラ / middleware に共有され、
//! `State<AppState>` で抽出できる。

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::{middleware, routing::get, Router};
use rusqlite::Connection;

pub mod auth;
pub mod tasks;

/// Shared state handed to every handler via `axum::extract::State`.
///
/// `rusqlite::Connection` は `!Sync` なので `Mutex` 必須。ハンドラは
/// `.lock()` を await をまたがずに短時間だけ保持する運用 (std::sync::Mutex
/// で OK)。`api_key` は `Arc<String>` で包み、`AppState::clone()` を安価に
/// 保つ。
#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub api_key: Arc<String>,
}

impl AppState {
    pub fn new(conn: Connection, api_key: String) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            api_key: Arc::new(api_key),
        }
    }
}

/// Build the top-level router.
///
/// `/api/*` 配下は認証 middleware を `.layer` で被せる。axum 0.8 では
/// routes が空 (T3 未着手のいま) の Router に `.layer` しても middleware が
/// 発火しないため、`.fallback(api_not_found)` で必ず "中身" を持たせる —
/// 404 を返す責任を middleware の後ろに繋ぐことで、無認証リクエストが
/// 401 より先に 404 として漏れるのを防ぐ。
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/tasks", get(tasks::list_tasks))
        // T4〜T7 のハンドラは順次ここに追加される。
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/api", api)
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const TEST_API_KEY: &str = "test-secret";

    fn test_state() -> AppState {
        AppState::new(Connection::open_in_memory().unwrap(), TEST_API_KEY.into())
    }

    async fn oneshot(app: Router, req: Request<Body>) -> axum::response::Response {
        app.oneshot(req).await.unwrap()
    }

    // ---------- /healthz (auth なし) ----------

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn healthz_is_accessible_without_bearer_token() {
        // 認証ヘッダなしでも 200。smoke test 用に意図的。
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ---------- /api/* (auth middleware) ----------

    #[tokio::test]
    async fn api_without_authorization_header_returns_401() {
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/api/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_with_wrong_bearer_returns_401() {
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/api/foo")
                .header("Authorization", "Bearer wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_with_malformed_authorization_returns_401() {
        // "Bearer " プレフィックスなしは形式違反として 401。
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/api/foo")
                .header("Authorization", TEST_API_KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn api_with_correct_bearer_falls_through_to_404() {
        // 認証は通るが /api/foo は T3 以降で追加される。現時点は 404。
        let app = router(test_state());
        let resp = oneshot(
            app,
            Request::builder()
                .uri("/api/foo")
                .header("Authorization", format!("Bearer {TEST_API_KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
