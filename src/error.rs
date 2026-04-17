//! Crate-wide error type.
//!
//! `OVERVIEW.md` § 依存クレート does not include `thiserror`, so the enum
//! is hand-written with `Display` / `std::error::Error` / `From` impls
//! for the underlying errors we propagate.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Configuration / CLI parsing problem (Fail Fast — never silently default).
    Config(String),
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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(msg) => write!(f, "config error: {msg}"),
            Error::Sqlite(e) => write!(f, "sqlite error: {e}"),
            Error::Reqwest(e) => write!(f, "http error: {e}"),
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Toml(e) => write!(f, "toml error: {e}"),
            Error::Json(e) => write!(f, "json error: {e}"),
            Error::Api { status, body } => write!(f, "api error {status}: {body}"),
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
            Error::Config(_) | Error::Api { .. } => None,
        }
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
