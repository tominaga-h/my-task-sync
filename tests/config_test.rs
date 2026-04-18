//! 設定解決の単体テスト。
//!
//! 解決順序 (v2):
//!   1. `--config <path>` or `$XDG_CONFIG_HOME/my-task-sync/config.toml`
//!   2. env (`MY_TASK_SYNC_API_KEY`, `MY_TASK_SYNC_PORT`, `MY_TASK_DATA_FILE`)
//!
//! env は file より優先。
//!
//! SQLite パス解決:
//!   1. `MY_TASK_DATA_FILE`
//!   2. `[sqlite].path`
//!   3. `dirs::data_dir()/my-task/tasks.db`

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use my_task_sync::config::{self, Cli};

// env は process-global のため、並列テストで衝突しないように直列化
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
    let _guard = ENV_LOCK.lock().unwrap();
    let saved: Vec<(String, Option<String>)> = vars
        .iter()
        .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
        .collect();
    for (k, v) in vars {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
    f();
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(&k, val),
            None => std::env::remove_var(&k),
        }
    }
}

fn write_config(dir: &std::path::Path, body: &str) -> PathBuf {
    let path = dir.join("config.toml");
    fs::write(&path, body).unwrap();
    path
}

fn cli_with(config_path: Option<PathBuf>) -> Cli {
    Cli {
        config_path,
        help: false,
    }
}

// ---------- TOML 読み込み ----------

#[test]
fn resolves_from_toml_file() {
    // Given: 全項目が埋まった TOML
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/custom/tasks.db"

[server]
port    = 4444
api_key = "file-key"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When
            let resolved = config::resolve(cli_with(Some(path.clone()))).expect("resolve");

            // Then
            assert_eq!(resolved.server.api_key, "file-key");
            assert_eq!(resolved.server.port, 4444);
            assert_eq!(resolved.sqlite.path, PathBuf::from("/custom/tasks.db"));
        },
    );
}

#[test]
fn port_defaults_when_server_port_omitted() {
    // Given: [server].port を省略した TOML
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[server]
api_key = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When
            let resolved = config::resolve(cli_with(Some(path))).unwrap();

            // Then: SERVER_DESIGN.md のデフォルトは 3333
            assert_eq!(resolved.server.port, 3333);
        },
    );
}

// ---------- env による上書き ----------

#[test]
fn env_overrides_api_key_and_port() {
    // Given: ファイルの値を env で上書きする
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/custom/tasks.db"

[server]
port    = 4444
api_key = "file-key"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", Some("env-key")),
            ("MY_TASK_SYNC_PORT", Some("5555")),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            let resolved = config::resolve(cli_with(Some(path))).unwrap();
            assert_eq!(resolved.server.api_key, "env-key");
            assert_eq!(resolved.server.port, 5555);
        },
    );
}

#[test]
fn sqlite_path_env_overrides_file_and_default() {
    // Given: ファイル側に [sqlite] section なし
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("from-env.db");
    let path = write_config(
        tmp.path(),
        r#"
[server]
api_key = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", None),
            ("MY_TASK_DATA_FILE", Some(db.to_str().unwrap())),
        ],
        || {
            let resolved = config::resolve(cli_with(Some(path))).unwrap();
            assert_eq!(resolved.sqlite.path, db);
        },
    );
}

// ---------- 欠如時のエラー ----------

#[test]
fn missing_api_key_is_a_config_error() {
    // Given: ファイルに api_key がなく、env にもない
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/tmp/x.db"

[server]
port = 3333
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When / Then: Fail Fast (フォールバックで "" を返してはならない)
            let err = config::resolve(cli_with(Some(path))).expect_err("must fail");
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("api_key") || msg.contains("api key"),
                "error should mention api_key, got: {msg}"
            );
        },
    );
}

#[test]
fn invalid_port_env_is_rejected() {
    // Given: MY_TASK_SYNC_PORT に数値以外
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[server]
api_key = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", Some("not-a-port")),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            let err = config::resolve(cli_with(Some(path))).expect_err("must fail");
            assert!(
                err.to_string().to_lowercase().contains("port"),
                "error should mention port, got: {err}"
            );
        },
    );
}

#[test]
fn missing_config_file_returns_config_error() {
    // Given: 存在しないパス
    let tmp = tempfile::tempdir().unwrap();
    let bogus = tmp.path().join("does-not-exist.toml");

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_PORT", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When / Then: 握りつぶさずエラーを返す (panic 禁止)
            let err = config::resolve(cli_with(Some(bogus))).expect_err("must fail");
            let _ = err.to_string(); // Display が動くこと
        },
    );
}

// ---------- CLI 引数パース ----------

#[test]
fn parse_cli_args_captures_config_path() {
    // Given
    let argv = ["my-task-sync", "--config", "/etc/my-task-sync.toml"];

    // When
    let cli = config::parse_cli_args(argv.iter().map(|s| s.to_string())).unwrap();

    // Then
    assert_eq!(
        cli.config_path.as_deref().and_then(|p| p.to_str()),
        Some("/etc/my-task-sync.toml")
    );
}

#[test]
fn parse_cli_args_detects_help_flag() {
    let argv = ["my-task-sync", "--help"];
    let cli = config::parse_cli_args(argv.iter().map(|s| s.to_string())).unwrap();
    assert!(cli.help);
}

#[test]
fn parse_cli_args_rejects_unknown_flag() {
    // Given: 未知のフラグ (サイレント無視は禁止)
    let argv = ["my-task-sync", "--bogus"];

    // When / Then: エラー
    let result = config::parse_cli_args(argv.iter().map(|s| s.to_string()));
    assert!(result.is_err(), "unknown flag must be rejected");
}

#[test]
fn parse_cli_args_rejects_deprecated_v1_flags() {
    // v1 の --once / --dry-run は v2 で廃止。古い launchctl plist や
    // スクリプトがサイレントに通るのを避けるため、未知フラグとして扱う。
    for flag in ["--once", "--dry-run"] {
        let argv = ["my-task-sync", flag];
        let result = config::parse_cli_args(argv.iter().map(|s| s.to_string()));
        assert!(result.is_err(), "{flag} must be rejected (v1 legacy)");
    }
}
