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

// S20: 意図的に append モード。起動ごとに truncate するとデバッグ時に
// 前回クラッシュの情報が飛ぶため。無制限成長のリスクがあるが、単一
// ユーザーかつ launchctl 再起動頻度が低い運用想定では許容範囲。
// 手動削除手順は T12 で README に追記する。

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
    ///
    /// `kill_and_wait()` と挙動を揃えて `killpg(pgid, SIGKILL)` を優先
    /// (S22)。panic / 早期 return で Drop が発火した場合でも、ngrok が
    /// fork したヘルパーを含めて PG 全体が殺される。`wait()` は Drop が
    /// sync なので呼ばない — 親プロセス終了後に init が zombie を reap。
    fn drop_inner(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };

        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SAFETY: `process_group(0)` で作った自作成 PG への SIGKILL。
            // 引数の妥当性は静的にクリア。Drop 経路では ret をログ以外で
            // 消費せず (panic unwinding 中の余計な分岐を避ける)、失敗時は
            // 下の start_kill にフォールバック。
            if unsafe { libc::killpg(pid as i32, libc::SIGKILL) } == 0 {
                info!(pid, "ngrok subprocess PG kill requested (drop)");
                return;
            }
        }

        // 非 unix、または killpg 失敗 (pid 取れないケース含む): 単一 PID SIGKILL
        match child.start_kill() {
            Ok(()) => info!("ngrok subprocess kill requested (drop / fallback)"),
            Err(e) => warn!(error = %e, "failed to signal ngrok subprocess during drop"),
        }
    }

    /// graceful shutdown 経路から呼ぶ明示 kill。
    ///
    /// 1. `killpg(pgid, SIGKILL)` で PG 全体を SIGKILL (ngrok が fork した
    ///    ヘルパーも含めて一掃)。`ESRCH` (PG が既に消滅) は no-op 扱い。
    /// 2. `child.wait()` で zombie を reap してリソースを解放。
    ///
    /// `self` を consume するので二重呼び出し不可 (型で防ぐ)。戻ってくる
    /// ときには ngrok プロセスの終了ステータスをログ出力済み。
    pub async fn kill_and_wait(mut self) -> Result<(), Error> {
        let Some(mut child) = self.child.take() else {
            // kill_and_wait が child = None の guard に対して呼ばれる正当
            // な経路は無いが (spawn 直後のみ Some)、防御的に no-op。
            return Ok(());
        };

        let pid = child.id();
        info!(pid, "killing ngrok subprocess group");

        // PG 全体に SIGKILL。`process_group(0)` を spawn 時に設定してある
        // ので pid == pgid。unix 以外は tokio::Child::start_kill に退避。
        #[cfg(unix)]
        if let Some(pid) = pid {
            let pgid = pid as i32;
            // SAFETY: killpg は signum, pgid を取る単純な syscall。引数の
            // 妥当性チェック (pgid > 0, SIGKILL が valid signal) は全て
            // 静的にクリアしている。副作用はプロセス終了のみ。
            let ret = unsafe { libc::killpg(pgid, libc::SIGKILL) };
            if ret == -1 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::ESRCH) {
                    // 既に exit してる (authtoken 不正で早期死など) → OK
                    info!(pgid, "ngrok process group already gone");
                } else {
                    warn!(error = %err, pgid, "killpg failed; falling back to start_kill");
                    let _ = child.start_kill();
                }
            }
        }

        #[cfg(not(unix))]
        {
            // non-unix (Windows 等): PG 概念がないので単一 PID kill に退避。
            let _ = child.start_kill();
        }

        // wait で zombie を reap。killpg 済みの child はすぐ返るはず。
        match child.wait().await {
            Ok(status) => info!(?status, "ngrok subprocess reaped"),
            Err(e) => warn!(error = %e, "waiting for ngrok subprocess failed"),
        }
        Ok(())
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
    let mut cmd = Command::new(program);
    cmd.args(["http", &port_str, "--domain", domain])
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    // 自プロセスグループ化 (S19 の defense-in-depth)。
    // `process_group(0)` は "新しい PG を作り、pgid = child の pid にする"。
    // ngrok 本体は v3 で fork しない想定だが、万一ヘルパーを spawn しても
    // T10 の `killpg(pgid, SIGKILL)` で PG 全体を一括で殺せる。
    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd.spawn().map_err(|e| map_spawn_error(program, e))?;

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

    // ---- kill_and_wait tests (T10) ----

    /// テスト用: `sleep N` を長寿命 child として立てる。ngrok を実際に
    /// 動かすには authtoken / domain が要るので、`sleep` で代用する。
    /// spawn 本体と同じく `process_group(0)` を設定して PG kill の挙動を
    /// 再現可能にする。
    async fn spawn_sleep_guard(seconds: u64) -> NgrokGuard {
        use tokio::process::Command;
        let mut cmd = Command::new("sleep");
        cmd.arg(seconds.to_string());
        #[cfg(unix)]
        cmd.process_group(0);
        let child = cmd.spawn().expect("spawn sleep");
        NgrokGuard { child: Some(child) }
    }

    #[tokio::test]
    async fn kill_and_wait_terminates_live_child_promptly() {
        // sleep 30 が立ち上がり、kill_and_wait で即座に終了することを確認。
        // 3s のタイムアウトを被せて "hang しないこと" を pin する。
        let guard = spawn_sleep_guard(30).await;
        tokio::time::timeout(std::time::Duration::from_secs(3), guard.kill_and_wait())
            .await
            .expect("kill_and_wait must complete within 3s")
            .expect("kill_and_wait must succeed");
    }

    #[tokio::test]
    async fn kill_and_wait_on_empty_guard_is_noop() {
        // child = None (kill_and_wait が 2 回目相当) でも panic / error
        // なく返ること。spawn 失敗 → 何らかの経路で None 化した guard も
        // 同じ扱い。
        let guard = NgrokGuard { child: None };
        guard.kill_and_wait().await.expect("noop path");
    }

    #[tokio::test]
    async fn kill_and_wait_is_idempotent_with_already_exited_child() {
        // `sleep 0.01` で極短命な child を立て、完全に exit してから
        // kill_and_wait を呼んでも ESRCH を正しく no-op 扱いできること。
        let guard = spawn_sleep_guard(0).await;
        // child が死ぬのを待つ
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // この時点で child は既に exit 済み。killpg は ESRCH を返すが
        // エラーにせず wait() で reap する。
        guard.kill_and_wait().await.expect("ESRCH path");
    }

    #[tokio::test]
    async fn drop_kills_live_child_pg_without_panic() {
        // S22: Drop 経路で killpg が走り、live child がシグナルされる
        // ことの smoke check。Drop は sync で wait しないため zombie が
        // 残るが、panic せず完走することを pin する。
        //
        // 直接 killpg が呼ばれたかの観測は難しいので、Drop の path を
        // 通してパニックなく終わることで十分とする (killpg ロジック自体は
        // kill_and_wait_terminates_live_child_promptly で pin 済み)。
        let guard = spawn_sleep_guard(30).await;
        let pid = guard.child.as_ref().and_then(|c| c.id()).expect("pid");
        drop(guard); // Drop::drop → drop_inner → killpg
                     // SIGKILL 送達を少し待つ (同期 syscall だが kernel scheduler 経由)。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // kill(pid, 0) は zombie でも 0 を返すので生死判定には不向き。
        // ここでは panic なく到達できた事実をもって OK とする。
        let _ = pid;
    }
}
