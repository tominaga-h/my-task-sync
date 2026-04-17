//! HTTP client for the my-own `/api/sync/tasks/*` endpoints.
//!
//! [`SyncApi`] is the trait that `sync_engine` consumes. The production
//! implementation is [`HttpApiClient`], which uses `reqwest` and applies
//! a bounded retry policy with exponential backoff for transport / 5xx
//! errors. Tests substitute their own `MockApi` against the same trait.
//!
//! 4xx responses are returned immediately (no retry); 5xx responses and
//! transient transport errors (`reqwest::Error::is_timeout()` or
//! `is_connect()`) retry up to `MAX_ATTEMPTS - 1` times.

use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;

use crate::error::Error;
use crate::model::{ChangesResponse, PatchNumberBody, PushResponse, SyncTask, UnsyncedTask};

/// Total attempts (1 initial + 3 retries).
const MAX_ATTEMPTS: u32 = 4;
/// Backoff delays between retries — `OVERVIEW.md` § リトライ戦略 (1s → 2s → 4s).
const RETRY_BACKOFFS_SEC: [u64; 3] = [1, 2, 4];
/// Per-request HTTP timeout.
const REQUEST_TIMEOUT_SECS: u64 = 30;

// ------------------------------------------------------------------
// Trait
// ------------------------------------------------------------------

/// The four operations sync_engine performs against my-own.
///
/// Native `async fn in trait` (Rust 1.75+) — no `async-trait` crate.
pub trait SyncApi {
    async fn push_tasks(&self, tasks: Vec<SyncTask>) -> Result<PushResponse, Error>;
    async fn get_unsynced(&self) -> Result<Vec<UnsyncedTask>, Error>;
    async fn patch_task_number(&self, neon_id: i64, task_number: i64) -> Result<(), Error>;
    async fn get_changes(&self, since: Option<&str>) -> Result<ChangesResponse, Error>;
}

// ------------------------------------------------------------------
// HttpApiClient
// ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HttpApiClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl HttpApiClient {
    pub fn new(base_url: String, api_key: String) -> Result<Self, Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()?;
        Ok(Self {
            client,
            base_url,
            api_key,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    fn request<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> RequestBuilder {
        let url = self.url(path);
        let mut req = self
            .client
            .request(method, url)
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(ACCEPT, "application/json");
        if let Some(b) = body {
            req = req
                .header(CONTENT_TYPE, "application/json")
                .json(b);
        }
        req
    }
}

#[derive(Serialize)]
struct PushBody<'a> {
    tasks: &'a [SyncTask],
}

impl SyncApi for HttpApiClient {
    async fn push_tasks(&self, tasks: Vec<SyncTask>) -> Result<PushResponse, Error> {
        let body = PushBody { tasks: &tasks };
        let builder = self.request(Method::POST, "/api/sync/tasks/push", Some(&body));
        let resp = send_with_retry(builder).await?;
        let parsed: PushResponse = resp.json().await?;
        Ok(parsed)
    }

    async fn get_unsynced(&self) -> Result<Vec<UnsyncedTask>, Error> {
        let builder = self.request::<()>(Method::GET, "/api/sync/tasks/unsynced", None);
        let resp = send_with_retry(builder).await?;
        // The server returns `{ "tasks": [...] }`. Decode via a small wrapper
        // so we can keep `Vec<UnsyncedTask>` as the public return type.
        #[derive(serde::Deserialize)]
        struct Wrapper {
            tasks: Vec<UnsyncedTask>,
        }
        let wrapped: Wrapper = resp.json().await?;
        Ok(wrapped.tasks)
    }

    async fn patch_task_number(&self, neon_id: i64, task_number: i64) -> Result<(), Error> {
        let body = PatchNumberBody { task_number };
        let path = format!("/api/sync/tasks/{neon_id}/number");
        let builder = self.request(Method::PATCH, &path, Some(&body));
        let _ = send_with_retry(builder).await?;
        Ok(())
    }

    async fn get_changes(&self, since: Option<&str>) -> Result<ChangesResponse, Error> {
        let builder = match since {
            Some(s) => self
                .request::<()>(Method::GET, "/api/sync/tasks/changes", None)
                .query(&[("since", s)]),
            None => self.request::<()>(Method::GET, "/api/sync/tasks/changes", None),
        };
        let resp = send_with_retry(builder).await?;
        let parsed: ChangesResponse = resp.json().await?;
        Ok(parsed)
    }
}

// ------------------------------------------------------------------
// Retry helper
// ------------------------------------------------------------------

/// Send the request with retries on transport errors and 5xx responses.
///
/// 4xx is returned immediately as `Error::Api { status, body }` so we
/// don't waste time retrying authentication / validation failures.
async fn send_with_retry(builder: RequestBuilder) -> Result<Response, Error> {
    let mut last_err: Option<Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let req = builder.try_clone().ok_or_else(|| {
            Error::Config(
                "request body is not cloneable; cannot retry (streaming bodies unsupported)"
                    .into(),
            )
        })?;
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                let code = status.as_u16();
                let body = resp.text().await.unwrap_or_default();
                if (400..500).contains(&code) {
                    // 4xx: never retry. 401 surfaces "認証情報を確認" via the message.
                    return Err(Error::Api { status: code, body });
                }
                last_err = Some(Error::Api { status: code, body });
            }
            Err(e) => {
                // Only transient transport failures are retried; everything
                // else (e.g. redirect policy, body decode) fails immediately.
                if !(e.is_timeout() || e.is_connect()) {
                    return Err(Error::Reqwest(e));
                }
                last_err = Some(Error::Reqwest(e));
            }
        }

        let next = attempt as usize;
        if next < RETRY_BACKOFFS_SEC.len() {
            tokio::time::sleep(Duration::from_secs(RETRY_BACKOFFS_SEC[next])).await;
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Config("retry loop exited without an error".into())))
}
