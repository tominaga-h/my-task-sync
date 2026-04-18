//! my-task-sync library crate.
//!
//! v2 backend server: axum HTTP server backed by the my-task SQLite.
//! See `docs/SERVER_DESIGN.md` for the full design.

pub mod config;
pub mod error;
pub mod http;
pub mod model;
pub mod sqlite;
