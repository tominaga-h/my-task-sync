//! HTTP layer — axum router, shared state, ミドルウェア。
//!
//! Phase 1 / 2 のルート構成:
//!   * `/healthz`      — 認証なし (運用者が token 無しで死活確認できるように)
//!   * `/api/status`   — 認証なし (T11 / Phase 2)。public URL 出る前に叩きたい
//!     ので Bearer を要求しない
//!   * `/api/*` (他)    — Bearer 認証 middleware 必須 (T2)
//!
//! `AppState` は `Router::with_state` で全ハンドラ / middleware に共有され、
//! `State<AppState>` で抽出できる。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{middleware, Router};
use rusqlite::Connection;
use tower_http::trace::TraceLayer;

pub mod auth;
pub mod projects;
pub mod status;
pub mod tasks;

/// Shared state handed to every handler via `axum::extract::State`.
///
/// `rusqlite::Connection` は `!Sync` なので `Mutex` 必須。ハンドラは
/// `.lock()` を await をまたがずに短時間だけ保持する運用 (std::sync::Mutex
/// で OK)。`api_key` は `Arc<String>` で包み、`AppState::clone()` を安価に
/// 保つ。T11 で `started_at` / `sqlite_path` / `ngrok_domain` を追加 —
/// `/api/status` のレスポンス組み立てに使う。
#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub api_key: Arc<String>,
    /// サーバー起動時刻。uptime 算出用。
    pub started_at: Instant,
    /// 開いている SQLite のパス。`/api/status` の sqlite.path 表示用。
    pub sqlite_path: Arc<String>,
    /// ngrok `domain` 設定 (config.toml or env から resolve した結果)。
    /// `None` なら ngrok 無効 → `/api/status` は `{ "enabled": false }` を返す。
    pub ngrok_domain: Option<Arc<String>>,
}

impl AppState {
    pub fn new(
        conn: Connection,
        api_key: String,
        sqlite_path: String,
        ngrok_domain: Option<String>,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            api_key: Arc::new(api_key),
            started_at: Instant::now(),
            sqlite_path: Arc::new(sqlite_path),
            ngrok_domain: ngrok_domain.map(Arc::new),
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
        .route("/tasks", get(tasks::list_tasks).post(tasks::create_task))
        .route(
            "/tasks/{task_number}",
            get(tasks::get_task).patch(tasks::patch_task),
        )
        .route(
            "/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route(
            "/projects/{id}",
            patch(projects::update_project).delete(projects::delete_project),
        )
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        // `/api/status` は認証 middleware の **外側** に置く。
        // axum のルート優先順位で exact-match (`/api/status`) が
        // `.nest("/api", ...)` より勝つので、認証なしで到達できる。
        .route("/api/status", get(status::get_status))
        .nest("/api", api)
        // `TraceLayer` はすべてのリクエストに DEBUG 以上で request/response
        // のログを出す。5xx 時の tracing::error と組み合わせて "どのパス・
        // どのメソッドで落ちたか" を追えるようにする。
        .layer(TraceLayer::new_for_http())
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
        AppState::new(
            Connection::open_in_memory().unwrap(),
            TEST_API_KEY.into(),
            ":memory:".into(),
            None,
        )
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
