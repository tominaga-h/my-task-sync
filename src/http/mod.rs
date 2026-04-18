//! HTTP layer — axum router and shared state.
//!
//! Phase 1 exposes only `/healthz`. Auth middleware (T2) and `/api/*`
//! handlers (T3〜T7) will extend this module without touching callers.

use std::sync::{Arc, Mutex};

use axum::{routing::get, Router};
use rusqlite::Connection;

/// Shared state handed to every handler via `axum::extract::State`.
///
/// `rusqlite::Connection` is `!Sync`, so the `Mutex` is required even
/// though daemon usage is effectively single-writer. `std::sync::Mutex`
/// is fine here because handlers do not await while holding the lock.
#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
}

impl AppState {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }
}

/// Build the top-level router. `/healthz` is unauthenticated so it can
/// be used as a smoke test without the operator needing a bearer token.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState::new(Connection::open_in_memory().unwrap())
    }

    #[tokio::test]
    async fn healthz_returns_200_ok() {
        let app = router(test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }
}
