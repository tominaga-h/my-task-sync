//! Configuration: TOML file → environment overrides → CLI flags.
//!
//! `OVERVIEW.md` § 設定の解決順序 に従う:
//! 1. `~/.config/my-task-sync/config.toml` (or `--config <path>`)
//! 2. 環境変数 `MY_TASK_SYNC_API_KEY` / `MY_TASK_SYNC_BASE_URL`
//! 3. CLI 引数 `--config`
//!
//! SQLite path resolution:
//! 1. `MY_TASK_DATA_FILE` env
//! 2. `[sqlite].path` from TOML
//! 3. `dirs::data_dir()/my-task/tasks.db`
//!
//! Missing `api_key` / `base_url` is a `ConfigError` — never silently
//! filled in (Fail Fast).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Error;

/// Default sync interval when `[sync].interval_seconds` is not set.
const DEFAULT_INTERVAL_SECONDS: u64 = 30;

// ------------------------------------------------------------------
// CLI
// ------------------------------------------------------------------

/// Parsed CLI flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    pub config_path: Option<PathBuf>,
    pub once: bool,
    pub dry_run: bool,
    pub help: bool,
}

/// Parse `argv` (including the program name as the first element) into a [`Cli`].
///
/// Unknown flags are rejected (Fail Fast) — silently ignoring would let
/// typos go undetected.
pub fn parse_cli_args<I>(args: I) -> Result<Cli, Error>
where
    I: IntoIterator<Item = String>,
{
    let mut iter = args.into_iter();
    // skip the program name; if missing we still parse the empty arg list
    let _ = iter.next();

    let mut cli = Cli::default();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--once" => cli.once = true,
            "--dry-run" => cli.dry_run = true,
            "--help" | "-h" => cli.help = true,
            "--config" => {
                let value = iter
                    .next()
                    .ok_or_else(|| Error::Config("--config requires a path argument".into()))?;
                cli.config_path = Some(PathBuf::from(value));
            }
            other => {
                return Err(Error::Config(format!("unknown CLI flag: {other}")));
            }
        }
    }
    Ok(cli)
}

// ------------------------------------------------------------------
// Resolved config
// ------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub sqlite: SqliteConfig,
    pub api: ApiConfig,
    pub sync: SyncConfig,
    pub once: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct SqliteConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub base_url: String,
    pub api_key: String,
}

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub interval_seconds: u64,
}

// ------------------------------------------------------------------
// File schema
// ------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    sqlite: Option<FileSqlite>,
    #[serde(default)]
    api: Option<FileApi>,
    #[serde(default)]
    sync: Option<FileSync>,
}

#[derive(Debug, Deserialize)]
struct FileSqlite {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileApi {
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileSync {
    interval_seconds: Option<u64>,
}

// ------------------------------------------------------------------
// resolve()
// ------------------------------------------------------------------

/// Resolve the final configuration from CLI + env + config file.
///
/// The CLI struct decides which file to read (explicit `--config <path>`
/// wins; otherwise the well-known location is consulted but missing is
/// not an error). Env then overrides individual fields, and the function
/// validates that required fields are present.
pub fn resolve(cli: Cli) -> Result<ResolvedConfig, Error> {
    let file = load_file_config(cli.config_path.as_deref())?;

    let api_key = std::env::var("MY_TASK_SYNC_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| file.api.as_ref().and_then(|a| a.api_key.clone()))
        .ok_or_else(|| {
            Error::Config(
                "api_key is not set (provide [api].api_key in config or MY_TASK_SYNC_API_KEY env)"
                    .into(),
            )
        })?;

    let base_url = std::env::var("MY_TASK_SYNC_BASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| file.api.as_ref().and_then(|a| a.base_url.clone()))
        .ok_or_else(|| {
            Error::Config(
                "base_url is not set (provide [api].base_url in config or MY_TASK_SYNC_BASE_URL env)"
                    .into(),
            )
        })?;

    let interval_seconds = file
        .sync
        .as_ref()
        .and_then(|s| s.interval_seconds)
        .unwrap_or(DEFAULT_INTERVAL_SECONDS);

    let sqlite_path = resolve_sqlite_path(file.sqlite.as_ref().and_then(|s| s.path.as_deref()))?;

    Ok(ResolvedConfig {
        sqlite: SqliteConfig { path: sqlite_path },
        api: ApiConfig { base_url, api_key },
        sync: SyncConfig { interval_seconds },
        once: cli.once,
        dry_run: cli.dry_run,
    })
}

fn load_file_config(explicit: Option<&Path>) -> Result<FileConfig, Error> {
    if let Some(path) = explicit {
        // Explicit `--config <path>` MUST exist; bail loudly if not.
        let body = std::fs::read_to_string(path)?;
        let parsed: FileConfig = toml::from_str(&body)?;
        return Ok(parsed);
    }

    // Default well-known path: missing file is OK (env may still satisfy required fields).
    if let Some(default) = default_config_path() {
        if default.exists() {
            let body = std::fs::read_to_string(&default)?;
            let parsed: FileConfig = toml::from_str(&body)?;
            return Ok(parsed);
        }
    }
    Ok(FileConfig::default())
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("my-task-sync").join("config.toml"))
}

fn resolve_sqlite_path(file_path: Option<&str>) -> Result<PathBuf, Error> {
    if let Ok(env_path) = std::env::var("MY_TASK_DATA_FILE") {
        if !env_path.is_empty() {
            return Ok(PathBuf::from(env_path));
        }
    }
    if let Some(p) = file_path {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let dir = dirs::data_dir().ok_or_else(|| {
        Error::Config(
            "data_dir is not available on this platform; set MY_TASK_DATA_FILE explicitly".into(),
        )
    })?;
    Ok(dir.join("my-task").join("tasks.db"))
}

/// Default state.db path (`$XDG_CONFIG_HOME/my-task-sync/state.db`).
///
/// Used by the binary; tests pass an explicit path to `SyncState::open`.
pub fn default_state_db_path() -> Result<PathBuf, Error> {
    let dir = dirs::config_dir()
        .ok_or_else(|| Error::Config("config_dir is not available on this platform".into()))?;
    Ok(dir.join("my-task-sync").join("state.db"))
}
