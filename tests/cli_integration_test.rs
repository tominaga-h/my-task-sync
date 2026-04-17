//! CLI 統合テスト。
//!
//! 実 API は叩かず、プロセス起動とフラグ解釈のみを検証する。
//!
//! 検証対象:
//!   * `--help` が 0 終了で usage を出力する
//!   * `--once --dry-run` で設定ファイルが無ければ非ゼロ終了 (panic せず)
//!   * `--config <存在しないパス>` も非ゼロ終了 (panic せず)
//!   * 未知のフラグは非ゼロ終了

use std::process::Command;

/// Cargo が `cargo test` 時に `CARGO_BIN_EXE_<name>` を渡してくれる。
/// bin crate の名前が `my-task-sync` である前提。
fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_my-task-sync")
        .expect("CARGO_BIN_EXE_my-task-sync must be set by cargo test for bin crate 'my-task-sync'")
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    // When
    let output = Command::new(bin_path())
        .arg("--help")
        .output()
        .expect("run --help");

    // Then
    assert!(output.status.success(), "--help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        combined.to_lowercase().contains("usage")
            || combined.contains("my-task-sync")
            || combined.contains("--once")
            || combined.contains("--dry-run"),
        "help output should describe the CLI; got: {combined}"
    );
}

#[test]
fn once_dry_run_without_config_exits_non_zero_gracefully() {
    // Given: 設定ファイルを無効なパスにし、env も空にする
    let tmp = tempfile::tempdir().unwrap();
    let bogus_config = tmp.path().join("does-not-exist.toml");

    // When: api_key / base_url の設定源が一切ない状態
    let output = Command::new(bin_path())
        .arg("--once")
        .arg("--dry-run")
        .arg("--config")
        .arg(&bogus_config)
        .env_remove("MY_TASK_SYNC_API_KEY")
        .env_remove("MY_TASK_SYNC_BASE_URL")
        .env_remove("MY_TASK_DATA_FILE")
        .output()
        .expect("run daemon");

    // Then: panic せず、非ゼロ終了
    assert!(
        !output.status.success(),
        "missing config must result in non-zero exit, got success"
    );

    // Then: 標準エラー or 標準出力にエラー文が出ている (握りつぶし禁止)
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !combined.trim().is_empty(),
        "must log something on config error, got empty output"
    );
}

#[test]
fn unknown_flag_exits_non_zero() {
    // When
    let output = Command::new(bin_path())
        .arg("--no-such-flag")
        .output()
        .expect("run with unknown flag");

    // Then: 未知のフラグはサイレント無視しない
    assert!(
        !output.status.success(),
        "unknown flag must cause non-zero exit"
    );
}
