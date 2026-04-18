//! Crate-wide error type.
//!
//! 既存設計 (`OVERVIEW.md` § 依存クレート) に従い `thiserror` を使わず、
//! `Display` / `std::error::Error` / `From` を手書きしている。HTTP レスポンス
//! への変換 (`axum::response::IntoResponse`) もここに実装する — ハンドラが
//! `Result<T, Error>` を返せば自動で適切な status + JSON body にマップされる。

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum Error {
    /// Configuration / CLI parsing problem (Fail Fast — never silently default).
    Config(String),
    /// Client supplied invalid input (400). Handler が user 入力の検証に失敗
    /// したときに返す。`msg` は原則そのままクライアントに返すので、内部情報
    /// (ファイルパス等) を載せないこと。
    BadRequest(String),
    /// SQLite operation failed.
    Sqlite(rusqlite::Error),
    /// HTTP transport failed (connection, TLS, body decode, …).
    Reqwest(reqwest::Error),
    /// Filesystem I/O failed (config read, state.db parent dir, …).
    Io(std::io::Error),
    /// TOML parse failed.
    Toml(toml::de::Error),
    /// JSON (de)serialisation failed.
    Json(serde_json::Error),
    /// Remote API returned a non-2xx response.
    Api { status: u16, body: String },
    /// Bearer 認証に失敗 (ヘッダ欠損 / 形式不正 / トークン不一致)。
    Unauthorized,
    /// リソースが見つからない (PATCH / GET by id で task_number 不在など)。
    NotFound,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(msg) => write!(f, "config error: {msg}"),
            Error::BadRequest(msg) => write!(f, "bad request: {msg}"),
            Error::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Error::Reqwest(e) => write!(f, "http error: {e}"),
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Toml(e) => write!(f, "toml error: {e}"),
            Error::Json(e) => write!(f, "json error: {e}"),
            Error::Api { status, body } => write!(f, "api error {status}: {body}"),
            Error::Unauthorized => write!(f, "unauthorized"),
            Error::NotFound => write!(f, "not found"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Sqlite(e) => Some(e),
            Error::Reqwest(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Toml(e) => Some(e),
            Error::Json(e) => Some(e),
            Error::Config(_)
            | Error::BadRequest(_)
            | Error::Api { .. }
            | Error::Unauthorized
            | Error::NotFound => None,
        }
    }
}

/// `Error` を HTTP レスポンスに落とす変換。4xx 系はクライアント向けに
/// 簡潔なメッセージを返し、5xx 系は詳細を server ログにだけ残して
/// クライアントには "internal error" だけ伝える (情報漏洩を避ける)。
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, message): (StatusCode, String) = match &self {
            Error::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            Error::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            Error::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            _ => {
                tracing::error!(error = %self, "server error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Reqwest(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Error::Toml(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
