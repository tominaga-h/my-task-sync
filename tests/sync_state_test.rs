//! state.db (sync 状態保存用) の単体テスト。
//!
//! OVERVIEW.md §state.db に従い以下を検証する:
//!   * `sync_state(key TEXT PK, value TEXT NOT NULL)` テーブルを作成する
//!   * `get(key)` は未設定なら None、設定済みなら Some(value)
//!   * `set(key, value)` は新規 INSERT / 既存は UPDATE (upsert)
//!   * 保存先が tasks.db とは別ファイルで良い (独立した state を保持)

use my_task_sync::sync_state::SyncState;

#[test]
fn get_returns_none_for_unset_key() {
    // Given: 新規 state.db
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let state = SyncState::open(&path).expect("open state.db");

    // When
    let value = state.get("last_push_at").expect("get");

    // Then
    assert!(value.is_none());
}

#[test]
fn set_then_get_returns_same_value() {
    // Given
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let state = SyncState::open(&path).unwrap();

    // When
    state
        .set("last_push_at", "2026-04-12T10:00:00Z")
        .expect("set");
    let got = state.get("last_push_at").unwrap();

    // Then
    assert_eq!(got.as_deref(), Some("2026-04-12T10:00:00Z"));
}

#[test]
fn set_on_existing_key_overwrites_value() {
    // Given: 既に値がある
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let state = SyncState::open(&path).unwrap();
    state.set("last_pull_at", "2026-04-10T00:00:00Z").unwrap();

    // When: 新しい値で上書き
    state.set("last_pull_at", "2026-04-12T12:00:00Z").unwrap();

    // Then
    let got = state.get("last_pull_at").unwrap();
    assert_eq!(got.as_deref(), Some("2026-04-12T12:00:00Z"));
}

#[test]
fn multiple_keys_are_independent() {
    // Given: 2 つのキーを設定
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let state = SyncState::open(&path).unwrap();
    state.set("last_push_at", "push-value").unwrap();
    state.set("last_pull_at", "pull-value").unwrap();

    // When / Then: それぞれ独立に取得できる
    assert_eq!(state.get("last_push_at").unwrap().as_deref(), Some("push-value"));
    assert_eq!(state.get("last_pull_at").unwrap().as_deref(), Some("pull-value"));
}

#[test]
fn state_persists_across_reopen() {
    // Given: ファイルに書き込む
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    {
        let state = SyncState::open(&path).unwrap();
        state.set("last_push_at", "2026-04-12T10:00:00Z").unwrap();
    } // drop

    // When: 同じファイルを再 open
    let reopened = SyncState::open(&path).unwrap();

    // Then: 値が残っている
    assert_eq!(
        reopened.get("last_push_at").unwrap().as_deref(),
        Some("2026-04-12T10:00:00Z")
    );
}

#[test]
fn state_file_is_independent_from_tasks_db() {
    // Given: 同一ディレクトリに state.db と tasks.db
    let tmp = tempfile::tempdir().unwrap();
    let state_path = tmp.path().join("state.db");
    let tasks_path = tmp.path().join("tasks.db");

    // tasks.db に無関係なデータを置く
    let tasks_conn = rusqlite::Connection::open(&tasks_path).unwrap();
    tasks_conn
        .execute("CREATE TABLE unrelated (x INTEGER)", [])
        .unwrap();
    drop(tasks_conn);

    // When: state.db を開いて書き込む
    let state = SyncState::open(&state_path).unwrap();
    state.set("last_push_at", "value").unwrap();

    // Then: tasks.db には sync_state テーブルが作られない (state.db 側にだけ作られる)
    let tasks_conn = rusqlite::Connection::open(&tasks_path).unwrap();
    let tbl: Option<String> = tasks_conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='sync_state'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(tbl.is_none(), "sync_state must not be created in tasks.db");
}

#[test]
fn open_creates_parent_directory_if_missing() {
    // Given: ~/.config/my-task-sync/ が存在しないケースを想定した多階層パス
    //        my-task の `db::open` は親ディレクトリを `create_dir_all` するパターンを
    //        採用しており、my-task-sync も同じ作法に従う (OVERVIEW の参照実装パターン)。
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("nested/my-task-sync/state.db");
    assert!(!deep.parent().unwrap().exists());

    // When
    let state = SyncState::open(&deep).expect("open creates parent dir");
    state.set("last_push_at", "v").unwrap();

    // Then
    assert_eq!(state.get("last_push_at").unwrap().as_deref(), Some("v"));
}
