//! Configuration: TOML file → environment overrides → CLI flags.
//!
//! Resolution order:
//! 1. TOML file (`--config <path>` or `$XDG_CONFIG_HOME/my-task-sync/config.toml`)
//! 2. Environment variables (`MY_TASK_SYNC_API_KEY` / `MY_TASK_SYNC_PORT` /
//!    `MY_TASK_DATA_FILE`) — override individual fields
//!
//! SQLite path:
//! 1. `MY_TASK_DATA_FILE` env
//! 2. `[sqlite].path` from TOML
//! 3. `dirs::data_dir()/my-task/tasks.db`
//!
//! Missing `api_key` is a `ConfigError` — never silently filled in (Fail Fast).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Error;

/// Default bind port when `[server].port` is not set.
const DEFAULT_PORT: u16 = 3333;

// ------------------------------------------------------------------
// CLI
// ------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cli {
    pub config_path: Option<PathBuf>,
    pub help: bool,
}

/// Parse `argv` (including the program name as the first element) into a [`Cli`].
///
/// Unknown flags are rejected — silently ignoring would let typos go undetected.
/// v1 flags `--once` and `--dry-run` are intentionally not accepted; they fall
/// through to the "unknown flag" path so stale launchctl plists fail loudly.
pub fn parse_cli_args<I>(args: I) -> Result<Cli, Error>
where
    I: IntoIterator<Item = String>,
{
    let mut iter = args.into_iter();
    let _ = iter.next();

    let mut cli = Cli::default();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
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
    pub server: ServerConfig,
}

#[derive(Debug, Clone)]
pub struct SqliteConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub api_key: String,
}

// ------------------------------------------------------------------
// File schema
// ------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    sqlite: Option<FileSqlite>,
    #[serde(default)]
    server: Option<FileServer>,
}

#[derive(Debug, Deserialize)]
struct FileSqlite {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileServer {
    port: Option<u16>,
    api_key: Option<String>,
}

// ------------------------------------------------------------------
// resolve()
// ------------------------------------------------------------------

pub fn resolve(cli: Cli) -> Result<ResolvedConfig, Error> {
    let file = load_file_config(cli.config_path.as_deref())?;

    let api_key = std::env::var("MY_TASK_SYNC_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| file.server.as_ref().and_then(|s| s.api_key.clone()))
        .ok_or_else(|| {
            Error::Config(
                "api_key is not set (provide [server].api_key in config or MY_TASK_SYNC_API_KEY env)"
                    .into(),
            )
        })?;

    let port = match std::env::var("MY_TASK_SYNC_PORT") {
        Ok(s) if !s.is_empty() => s
            .parse::<u16>()
            .map_err(|e| Error::Config(format!("MY_TASK_SYNC_PORT is not a valid port: {e}")))?,
        _ => file
            .server
            .as_ref()
            .and_then(|s| s.port)
            .unwrap_or(DEFAULT_PORT),
    };

    let sqlite_path = resolve_sqlite_path(file.sqlite.as_ref().and_then(|s| s.path.as_deref()))?;

    Ok(ResolvedConfig {
        sqlite: SqliteConfig { path: sqlite_path },
        server: ServerConfig { port, api_key },
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

/// `$HOME/.config/my-task-sync/config.toml` を返す (unix 系のみ)。
///
/// `dirs::config_dir()` は macOS で `~/Library/Application Support/` を
/// 返してしまい、README / `docs/SERVER_DESIGN.md` / my-task 本体の
/// ドキュメント (全て `~/.config/` 前提) と食い違うため、`home_dir()` +
/// `.config` を自前で組み立てる。
pub fn default_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|p| p.join(".config").join("my-task-sync").join("config.toml"))
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
