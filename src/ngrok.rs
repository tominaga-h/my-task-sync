//! ngrok サブプロセス管理 (Phase 2 / T9)。
//!
//! `NGROK_DOMAIN` 相当 (config.toml の `[ngrok].domain` or 環境変数
//! `MY_TASK_SYNC_NGROK_DOMAIN`) が設定されているときだけ、起動時に
//! `ngrok http <port> --domain <domain>` を子プロセスとして立ち上げる。
//!
//! `NgrokGuard` は `Drop` で child に `start_kill()` を送る「保険」の
//! 役割。T10 で graceful shutdown 経路から `kill_and_wait` を明示呼び
//! 出しするまでは、Drop ガード単体でも Ctrl-C / 通常終了時に ngrok
//! プロセスが残らないことを保証する。

use std::io;
use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, Command};
use tracing::{info, warn};

use crate::error::Error;

const NGROK_STDOUT_LOG: &str = "/tmp/my-task-sync-ngrok.out.log";
const NGROK_STDERR_LOG: &str = "/tmp/my-task-sync-ngrok.err.log";

/// 起動中の ngrok child を保持する RAII ガード。
///
/// Drop で `start_kill()` を投げる (sync、fire-and-forget)。SIGKILL を
/// 送るだけで `wait()` はしない — 親プロセスが終了すれば init が
/// zombie を reap する。T10 で明示 `kill_and_wait()` を入れたら、
/// Drop は二重呼び出し対策の "最終防衛線" になる。
pub struct NgrokGuard {
    /// T10 の `kill_and_wait()` で child を consume したら None になる。
    /// Drop で child.is_some() のときだけ start_kill する。
    child: Option<Child>,
}

impl std::fmt::Debug for NgrokGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // tokio::process::Child は Debug を実装するが、PID などの内部状態
        // を panic メッセージに出したくないので自前 impl。
        f.debug_struct("NgrokGuard")
            .field("child_present", &self.child.is_some())
            .finish()
    }
}

impl NgrokGuard {
    /// child の有無に関わらず安全に再呼び出し可能な Drop 実行本体。
    /// `kill_and_wait()` で child を取った後に drop されても no-op。
    fn drop_inner(&mut self) {
        if let Some(mut child) = self.child.take() {
            match child.start_kill() {
                Ok(()) => info!("ngrok subprocess kill requested (drop)"),
                Err(e) => warn!(error = %e, "failed to signal ngrok subprocess during drop"),
            }
        }
    }
}

impl Drop for NgrokGuard {
    fn drop(&mut self) {
        self.drop_inner();
    }
}

/// `ngrok http <port> --domain <domain>` を起動し、Guard を返す。
///
/// stdout/stderr は `/tmp/my-task-sync-ngrok.{out,err}.log` に追記。
/// ngrok バイナリが PATH に無い場合は `Error::Config` にマップし、
/// ユーザーに `brew install ngrok` + `ngrok config add-authtoken` の
/// 手順を案内する。
pub async fn spawn(domain: &str, port: u16) -> Result<NgrokGuard, Error> {
    spawn_internal(
        "ngrok",
        domain,
        port,
        Path::new(NGROK_STDOUT_LOG),
        Path::new(NGROK_STDERR_LOG),
    )
    .await
}

/// Test 用に program 名と log ファイルパスを差し替え可能にした内部実装。
///
/// `program` を "ngrok" 以外にすると、バイナリ不在時の `ErrorKind::NotFound`
/// 分岐を unit test できる (存在しない絶対パスを渡す)。
async fn spawn_internal(
    program: &str,
    domain: &str,
    port: u16,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<NgrokGuard, Error> {
    let stdout = open_append(stdout_path)?;
    let stderr = open_append(stderr_path)?;

    let port_str = port.to_string();
    let child = Command::new(program)
        .args(["http", &port_str, "--domain", domain])
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| map_spawn_error(program, e))?;

    let pid = child.id().unwrap_or(0);
    info!(
        pid,
        domain,
        port,
        stdout = %stdout_path.display(),
        stderr = %stderr_path.display(),
        "ngrok subprocess started"
    );

    Ok(NgrokGuard { child: Some(child) })
}

fn open_append(path: &Path) -> Result<std::fs::File, Error> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(Error::Io)
}

fn map_spawn_error(program: &str, e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::NotFound {
        Error::Config(format!(
            "ngrok binary not found (tried `{program}`). \
             Install with `brew install ngrok` and configure the authtoken \
             via `ngrok config add-authtoken <token>`. Underlying error: {e}"
        ))
    } else {
        // 他の I/O エラー (権限不足など) は素直に Io として流す。
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_with_missing_binary_returns_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = spawn_internal(
            "/nonexistent/path/ngrok-does-not-exist",
            "example.ngrok-free.dev",
            3333,
            &tmp.path().join("ngrok.out.log"),
            &tmp.path().join("ngrok.err.log"),
        )
        .await
        .expect_err("spawn with missing binary must fail");

        match &err {
            Error::Config(msg) => {
                let lower = msg.to_lowercase();
                assert!(
                    lower.contains("ngrok"),
                    "error message should mention ngrok, got: {msg}"
                );
                assert!(
                    lower.contains("brew install") || lower.contains("add-authtoken"),
                    "error message should hint at install steps, got: {msg}"
                );
            }
            other => panic!("expected Error::Config, got: {other:?}"),
        }
    }

    #[test]
    fn drop_guard_with_no_child_is_noop() {
        // kill_and_wait 後 (T10) を模して child = None にした状態で drop
        // しても panic しないこと。drop_inner は take() で空になった後
        // 再呼び出しされても再入可能。
        let mut guard = NgrokGuard { child: None };
        guard.drop_inner(); // 1 回目
        guard.drop_inner(); // 2 回目 — no-op でなければここで panic する
                            // Drop trait も暗黙に呼ばれるので、スコープ終わりでさらにもう 1 回。
    }
}
