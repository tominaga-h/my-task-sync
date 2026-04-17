//! my-task-sync library crate.
//!
//! This crate is consumed by both the `my-task-sync` binary and the
//! integration tests under `tests/`. The module layout matches
//! `docs/OVERVIEW.md` § リポジトリ構成.

#![allow(async_fn_in_trait)]

pub mod api_client;
pub mod config;
pub mod error;
pub mod model;
pub mod sqlite;
pub mod sync_engine;
pub mod sync_state;
