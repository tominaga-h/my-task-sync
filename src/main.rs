//! Binary entry point: parse CLI → resolve config → run axum server.
//!
//! Designed to run under launchctl with `KeepAlive`. Fatal startup errors
//! (config / `SQLite` / bind) cause a non-zero exit so `KeepAlive` can see
//! the failure; inside the server, handler errors are turned into HTTP
//! responses rather than terminating the process.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use my_task_sync::config::{self, ResolvedConfig};
use my_task_sync::error::Error;
use my_task_sync::http;
use my_task_sync::ngrok;
use my_task_sync::sqlite;

/// シャットダウン信号を受けてから in-flight リクエストの drain を待つ
/// 最大時間。超過すると serve を落として強制終了する — launchctl の
/// 再起動サイクルを詰まらせないため。
const GRACEFUL_SHUTDOWN_SECS: u64 = 10;

const HELP_TEXT: &str = "\
my-task-sync — local HTTP server backing my-own with the my-task SQLite.

USAGE:
    my-task-sync [OPTIONS]

OPTIONS:
    --config <path>   Path to TOML config file
                      (default: $HOME/.config/my-task-sync/config.toml)
    --help, -h        Show this help and exit

ENVIRONMENT:
    MY_TASK_SYNC_API_KEY    Override [server].api_key from config
    MY_TASK_SYNC_PORT       Override [server].port
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
            error!(error = %e, "server stopped with error");
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("my_task_sync=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

async fn run(cfg: ResolvedConfig) -> Result<(), Error> {
    info!(
        sqlite = %cfg.sqlite.path.display(),
        port = cfg.server.port,
        "starting server"
    );

    let conn = sqlite::open(&cfg.sqlite.path)?;
    let state = http::AppState::new(conn, cfg.server.api_key.clone());
    let router = http::router(state);

    // Loopback のみ bind: Phase 2 の ngrok が localhost:port を公開 URL に
    // 転送する前提。外部インターフェースに直接 bind させないことで
    // ハードニング面も稼ぐ。
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.server.port));
    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "listening");

    // ngrok subprocess を bind 成功後に spawn (順序逆だと転送先が無い)。
    // 未設定なら skip。guard は run() スコープ終了まで保持。通常経路では
    // serve 終了後に `kill_and_wait()` で明示 reap する (T10)。Drop は
    // panic / 早期 return (bind 失敗など) の保険。
    let mut ngrok_guard = match cfg.ngrok.domain.as_deref() {
        Some(domain) => Some(ngrok::spawn(domain, cfg.server.port).await?),
        None => {
            info!("ngrok disabled ([ngrok].domain not set)");
            None
        }
    };

    // シャットダウン信号 → serve の drain トリガを oneshot で繋ぎ、
    // さらに GRACEFUL_SHUTDOWN_SECS の deadline を被せる。
    let (trigger_tx, trigger_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = trigger_rx.await;
    });
    let deadline = async move {
        shutdown_signal().await;
        let _ = trigger_tx.send(());
        tokio::time::sleep(Duration::from_secs(GRACEFUL_SHUTDOWN_SECS)).await;
    };

    tokio::select! {
        result = serve => {
            result?;
            info!("server stopped");
        }
        () = deadline => {
            warn!(
                "graceful shutdown exceeded {GRACEFUL_SHUTDOWN_SECS}s — forcing exit"
            );
        }
    }

    // HTTP drain 完了 or deadline 超過の後、明示的に ngrok を kill + reap。
    // この順序 (HTTP → ngrok) なら public URL は serve 生存中に使え続け、
    // ngrok 側が勝手に落ちて Vercel 向けが 502 になる期間を作らない。
    if let Some(guard) = ngrok_guard.take() {
        if let Err(e) = guard.kill_and_wait().await {
            warn!(error = %e, "ngrok cleanup failed");
        }
    }
    Ok(())
}

/// Resolve either SIGINT (Ctrl-C) or SIGTERM. launchctl sends SIGTERM
/// on `launchctl unload`; Ctrl-C sends SIGINT from an interactive shell.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let mut sig = signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        sig.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("SIGINT received, shutting down"),
        () = terminate => info!("SIGTERM received, shutting down"),
    }
}
