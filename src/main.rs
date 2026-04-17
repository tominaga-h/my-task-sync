//! Binary entry point: parse CLI → resolve config → loop sync_cycle.
//!
//! Designed to run under launchctl with KeepAlive — failures inside the
//! loop are logged and retried on the next interval; only fatal startup
//! errors (config / SQLite / HTTP client construction) cause a non-zero
//! exit.

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use my_task_sync::api_client::HttpApiClient;
use my_task_sync::config::{self, ResolvedConfig};
use my_task_sync::error::Error;
use my_task_sync::sqlite;
use my_task_sync::sync_engine;
use my_task_sync::sync_state::SyncState;

const HELP_TEXT: &str = "\
my-task-sync — local daemon syncing my-task SQLite with my-own (Neon) over HTTP.

USAGE:
    my-task-sync [OPTIONS]

OPTIONS:
    --config <path>   Path to TOML config file
                      (default: $XDG_CONFIG_HOME/my-task-sync/config.toml)
    --once            Run a single sync cycle and exit
    --dry-run         Read state but suppress API writes and state updates
    --help, -h        Show this help and exit

ENVIRONMENT:
    MY_TASK_SYNC_API_KEY    Override [api].api_key from config
    MY_TASK_SYNC_BASE_URL   Override [api].base_url from config
    MY_TASK_DATA_FILE       Override [sqlite].path
    RUST_LOG                tracing filter (e.g. my_task_sync=info)
";

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let cli = match config::parse_cli_args(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("{HELP_TEXT}");
            return ExitCode::from(2);
        }
    };

    if cli.help {
        println!("{HELP_TEXT}");
        return ExitCode::SUCCESS;
    }

    let resolved = match config::resolve(cli) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "failed to resolve configuration");
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    match run(resolved).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(error = %e, "daemon stopped with error");
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("my_task_sync=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn run(cfg: ResolvedConfig) -> Result<(), Error> {
    info!(
        once = cfg.once,
        dry_run = cfg.dry_run,
        sqlite = %cfg.sqlite.path.display(),
        base_url = %cfg.api.base_url,
        interval_seconds = cfg.sync.interval_seconds,
        "starting sync daemon"
    );

    let conn = sqlite::open(&cfg.sqlite.path)?;
    let state_path = config::default_state_db_path()?;
    let state = SyncState::open(&state_path)?;
    let api = HttpApiClient::new(cfg.api.base_url.clone(), cfg.api.api_key.clone())?;

    let shutdown = Arc::new(AtomicBool::new(false));
    install_shutdown_handler(shutdown.clone())?;

    loop {
        match sync_engine::sync_cycle(&conn, &api, &state, cfg.dry_run).await {
            Ok(()) => info!("sync cycle ok"),
            Err(e) => error!(error = %e, "sync cycle failed; will retry on next tick"),
        }

        if cfg.once {
            break;
        }
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        let interval = Duration::from_secs(cfg.sync.interval_seconds);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = wait_for_shutdown(shutdown.clone()) => break,
        }
    }

    info!("sync loop stopped");
    Ok(())
}

/// `ctrlc` registers a process-wide handler. Setting it more than once
/// returns an error; we treat that as a non-fatal warning so re-runs in
/// the same process (e.g. integration tests) don't fail.
fn install_shutdown_handler(shutdown: Arc<AtomicBool>) -> Result<(), Error> {
    let s = shutdown.clone();
    match ctrlc::set_handler(move || s.store(true, Ordering::SeqCst)) {
        Ok(()) => Ok(()),
        Err(ctrlc::Error::MultipleHandlers) => {
            warn!("ctrlc handler already installed; reusing existing handler");
            Ok(())
        }
        Err(e) => Err(Error::Config(format!("failed to install ctrlc handler: {e}"))),
    }
}

async fn wait_for_shutdown(shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
