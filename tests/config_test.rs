//! 設定解決の単体テスト。
//!
//! OVERVIEW.md § 設定の解決順序:
//!   1. `~/.config/my-task-sync/config.toml`
//!   2. 環境変数 (`MY_TASK_SYNC_API_KEY`, `MY_TASK_SYNC_BASE_URL`)
//!   3. CLI 引数 (`--config /path/to/config.toml`)
//!
//! 下位 (CLI) が上位を上書きする。env は file より優先。
//!
//! SQLite パス解決:
//!   1. 環境変数 `MY_TASK_DATA_FILE`
//!   2. `dirs::data_dir()/my-task/tasks.db`

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

fn cli_with(config_path: Option<PathBuf>, dry_run: bool) -> Cli {
    Cli {
        config_path,
        once: false,
        dry_run,
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

[api]
base_url = "https://example.test"
api_key  = "file-key"

[sync]
interval_seconds = 60
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When
            let resolved =
                config::resolve(cli_with(Some(path.clone()), false)).expect("resolve");

            // Then
            assert_eq!(resolved.api.base_url, "https://example.test");
            assert_eq!(resolved.api.api_key, "file-key");
            assert_eq!(resolved.sync.interval_seconds, 60);
            assert_eq!(resolved.sqlite.path, PathBuf::from("/custom/tasks.db"));
            assert!(!resolved.dry_run);
        },
    );
}

#[test]
fn interval_defaults_when_sync_section_omitted() {
    // Given: [sync] セクションを省略した TOML
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/custom/tasks.db"

[api]
base_url = "https://example.test"
api_key  = "file-key"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When
            let resolved = config::resolve(cli_with(Some(path), false)).unwrap();

            // Then: OVERVIEW.md のサンプルが 30 秒なのでデフォルトは 30
            assert_eq!(resolved.sync.interval_seconds, 30);
        },
    );
}

// ---------- env による上書き ----------

#[test]
fn env_overrides_api_key_and_base_url() {
    // Given: ファイルの値を env で上書きする
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/custom/tasks.db"

[api]
base_url = "https://file.test"
api_key  = "file-key"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", Some("env-key")),
            ("MY_TASK_SYNC_BASE_URL", Some("https://env.test")),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            let resolved = config::resolve(cli_with(Some(path), false)).unwrap();
            assert_eq!(resolved.api.api_key, "env-key");
            assert_eq!(resolved.api.base_url, "https://env.test");
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
[api]
base_url = "https://example.test"
api_key  = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", Some(db.to_str().unwrap())),
        ],
        || {
            let resolved = config::resolve(cli_with(Some(path), false)).unwrap();
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

[api]
base_url = "https://example.test"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When / Then: Fail Fast (フォールバックで "" を返してはならない)
            let err = config::resolve(cli_with(Some(path), false)).expect_err("must fail");
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("api_key") || msg.contains("api key"),
                "error should mention api_key, got: {msg}"
            );
        },
    );
}

#[test]
fn missing_base_url_is_a_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/tmp/x.db"

[api]
api_key = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            let err = config::resolve(cli_with(Some(path), false)).expect_err("must fail");
            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("base_url") || msg.contains("base url"),
                "error should mention base_url, got: {msg}"
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
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            // When / Then: 握りつぶさずエラーを返す (panic 禁止)
            let err = config::resolve(cli_with(Some(bogus), false)).expect_err("must fail");
            let _ = err.to_string(); // Display が動くこと
        },
    );
}

// ---------- dry_run flag flows through ----------

#[test]
fn dry_run_flag_is_preserved_in_resolved_config() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_config(
        tmp.path(),
        r#"
[sqlite]
path = "/tmp/x.db"

[api]
base_url = "https://example.test"
api_key  = "k"
"#,
    );

    with_env(
        &[
            ("MY_TASK_SYNC_API_KEY", None),
            ("MY_TASK_SYNC_BASE_URL", None),
            ("MY_TASK_DATA_FILE", None),
        ],
        || {
            let resolved = config::resolve(cli_with(Some(path), true)).unwrap();
            assert!(resolved.dry_run, "dry_run=true must reach ResolvedConfig");
        },
    );
}

// ---------- CLI 引数パース ----------

#[test]
fn parse_cli_args_detects_once_and_dry_run() {
    // Given
    let argv = ["my-task-sync", "--once", "--dry-run"];

    // When
    let cli = config::parse_cli_args(argv.iter().map(|s| s.to_string()))
        .expect("parse cli args");

    // Then
    assert!(cli.once, "--once should set once=true");
    assert!(cli.dry_run, "--dry-run should set dry_run=true");
    assert!(cli.config_path.is_none());
    assert!(!cli.help);
}

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
