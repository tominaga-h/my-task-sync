//! sync_engine の単体テスト (モック API + in-memory SQLite)。
//!
//! OVERVIEW.md §sync engine 詳細 と指示書 §4「境界条件」を網羅する:
//!   * 初回起動 (state 未設定) は全データ push / pull
//!   * LWW Neon 勝ち: Neon 側 updatedAt > ローカル updated → SQLite 更新
//!   * LWW SQLite 勝ち: ローカル updated >= Neon → スキップ (次 push で反映)
//!   * reminds は pull_updates で全置換
//!   * pull_unsynced は INSERT → task_number を PATCH で書き戻す
//!   * dry_run=true のとき API 書き込み (push / patch) と state.set は発生しない
//!
//! sync_engine は `trait SyncApi` を介して API クライアントを受け取る。
//! テストでは MockApi を実装し、呼び出し履歴と返却値を制御する。

mod common;

use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::params;

use common::{insert_raw_task, make_my_task_db};
use my_task_sync::api_client::SyncApi;
use my_task_sync::model::{
    ChangedTask, ChangesResponse, PushAction, PushResponse, PushResultRow, SyncTask, UnsyncedTask,
};
use my_task_sync::sync_engine;
use my_task_sync::sync_state::SyncState;

// ==============================================================
// Mock SyncApi
// ==============================================================

#[derive(Default)]
struct MockCalls {
    pushed: Vec<SyncTask>,
    patched_numbers: Vec<(i64, i64)>, // (neon_id, task_number)
    get_changes_since: Vec<Option<String>>,
}

struct MockApi {
    calls: Mutex<MockCalls>,
    push_response: Mutex<PushResponse>,
    unsynced: Mutex<Vec<UnsyncedTask>>,
    changes: Mutex<ChangesResponse>,
    // `patch_task_number` を強制失敗させるフラグ。ROLLBACK 動作のリグレッ
    // ションテスト用 (sync_engine::insert_unsynced_with_patch)。
    fail_patch: Mutex<bool>,
}

impl MockApi {
    fn new() -> Self {
        Self {
            calls: Mutex::new(MockCalls::default()),
            push_response: Mutex::new(PushResponse { results: vec![] }),
            unsynced: Mutex::new(vec![]),
            changes: Mutex::new(ChangesResponse {
                tasks: vec![],
                server_time: ts("2026-04-12T12:00:00Z"),
            }),
            fail_patch: Mutex::new(false),
        }
    }

    fn with_push_results(self, results: Vec<PushResultRow>) -> Self {
        *self.push_response.lock().unwrap() = PushResponse { results };
        self
    }

    fn with_unsynced(self, tasks: Vec<UnsyncedTask>) -> Self {
        *self.unsynced.lock().unwrap() = tasks;
        self
    }

    fn with_changes(self, tasks: Vec<ChangedTask>, server_time: DateTime<Utc>) -> Self {
        *self.changes.lock().unwrap() = ChangesResponse { tasks, server_time };
        self
    }

    fn with_patch_failure(self) -> Self {
        *self.fail_patch.lock().unwrap() = true;
        self
    }

    fn pushed(&self) -> Vec<SyncTask> {
        self.calls.lock().unwrap().pushed.clone()
    }

    fn patched(&self) -> Vec<(i64, i64)> {
        self.calls.lock().unwrap().patched_numbers.clone()
    }

    fn changes_since_history(&self) -> Vec<Option<String>> {
        self.calls.lock().unwrap().get_changes_since.clone()
    }
}

impl SyncApi for MockApi {
    async fn push_tasks(
        &self,
        tasks: Vec<SyncTask>,
    ) -> Result<PushResponse, my_task_sync::error::Error> {
        self.calls.lock().unwrap().pushed.extend(tasks);
        Ok(self.push_response.lock().unwrap().clone())
    }

    async fn get_unsynced(&self) -> Result<Vec<UnsyncedTask>, my_task_sync::error::Error> {
        Ok(self.unsynced.lock().unwrap().clone())
    }

    async fn patch_task_number(
        &self,
        neon_id: i64,
        task_number: i64,
    ) -> Result<(), my_task_sync::error::Error> {
        self.calls
            .lock()
            .unwrap()
            .patched_numbers
            .push((neon_id, task_number));
        if *self.fail_patch.lock().unwrap() {
            return Err(my_task_sync::error::Error::Api {
                status: 500,
                body: "boom".into(),
            });
        }
        Ok(())
    }

    async fn get_changes(
        &self,
        since: Option<&str>,
    ) -> Result<ChangesResponse, my_task_sync::error::Error> {
        self.calls
            .lock()
            .unwrap()
            .get_changes_since
            .push(since.map(str::to_owned));
        Ok(self.changes.lock().unwrap().clone())
    }
}

// ==============================================================
// Helpers
// ==============================================================

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn open_state() -> (tempfile::TempDir, SyncState) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let state = SyncState::open(&path).unwrap();
    (tmp, state)
}

fn changed_task(
    task_number: i64,
    title: &str,
    status: &str,
    updated: &str,
    reminds: Vec<NaiveDate>,
) -> ChangedTask {
    // OVERVIEW.md + Open Question #1 推論より、`/api/sync/tasks/changes` は
    // `task_number IS NOT NULL` の行のみを返す仕様で確定。`neon_id` は
    // ローカル側の同定に使わないため `ChangedTask` から除外している。
    ChangedTask {
        task_number,
        title: title.into(),
        status: status.into(),
        source: "web".into(),
        project_name: None,
        due: None,
        done_at: None,
        important: false,
        updated_at: ts(updated),
        created_at: ts("2026-04-01T00:00:00Z"),
        reminds,
    }
}

// ==============================================================
// push: 初回起動 (state 未設定) は全件 push
// ==============================================================

#[tokio::test]
async fn push_sends_all_tasks_on_initial_run() {
    // Given: SQLite に 2 件。state.last_push_at なし (初回)
    let conn = make_my_task_db();
    insert_raw_task(&conn, "a", "open", None, "2026-04-10", "2026-04-10");
    insert_raw_task(&conn, "b", "open", None, "2026-04-12", "2026-04-12");
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_push_results(vec![
        PushResultRow { task_number: 1, action: PushAction::Created, neon_id: 101 },
        PushResultRow { task_number: 2, action: PushAction::Created, neon_id: 102 },
    ]);

    // When
    sync_engine::push(&conn, &api, &state, /* dry_run */ false)
        .await
        .expect("push");

    // Then: 2 件とも送信された
    let pushed = api.pushed();
    assert_eq!(pushed.len(), 2);

    // Then: state が更新された
    assert!(
        state.get("last_push_at").unwrap().is_some(),
        "last_push_at should be set after successful push"
    );
}

#[tokio::test]
async fn push_sends_only_tasks_newer_than_last_push() {
    // Given: 3 件のうち 1 件だけが last_push 以降に更新
    let conn = make_my_task_db();
    insert_raw_task(&conn, "old", "open", None, "2026-04-10", "2026-04-10");
    insert_raw_task(&conn, "mid", "open", None, "2026-04-11", "2026-04-11");
    insert_raw_task(&conn, "new", "open", None, "2026-04-12", "2026-04-12");

    let (_tmp, state) = open_state();
    state.set("last_push_at", "2026-04-11").unwrap();

    let api = MockApi::new().with_push_results(vec![PushResultRow {
        task_number: 3,
        action: PushAction::Created,
        neon_id: 103,
    }]);

    // When
    sync_engine::push(&conn, &api, &state, false).await.unwrap();

    // Then: "new" (2026-04-12) の 1 件だけ送信
    let pushed = api.pushed();
    assert_eq!(pushed.len(), 1);
    assert_eq!(pushed[0].title, "new");
}

// ==============================================================
// pull_unsynced: INSERT → PATCH で task_number を書き戻す
// ==============================================================

#[tokio::test]
async fn pull_unsynced_inserts_and_patches_task_number_back_to_neon() {
    // Given: Neon 側に task_number=NULL のタスクが 1 件
    let conn = make_my_task_db();

    let unsynced = vec![UnsyncedTask {
        neon_id: 42,
        title: "Web 作成".into(),
        status: "open".into(),
        source: "web".into(),
        project_name: Some("inbox".into()),
        due: None,
        done_at: None,
        important: false,
        updated_at: ts("2026-04-12T09:00:00Z"),
        created_at: ts("2026-04-12T09:00:00Z"),
        reminds: vec![d(2026, 4, 20)],
    }];
    let api = MockApi::new().with_unsynced(unsynced);

    // When
    sync_engine::pull_unsynced(&conn, &api, false).await.unwrap();

    // Then: SQLite に 1 行 INSERT され、reminds も作られた
    let sqlite_id: i64 = conn
        .query_row("SELECT id FROM tasks WHERE title = 'Web 作成'", [], |r| r.get(0))
        .expect("task inserted");
    let reminds_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_reminds WHERE task_id = ?1",
            params![sqlite_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reminds_count, 1);

    // Then: その id が Neon に PATCH された
    let patched = api.patched();
    assert_eq!(patched, vec![(42, sqlite_id)]);

    // Then: projects に "inbox" が作成された
    let project_exists: Option<String> = conn
        .query_row(
            "SELECT name FROM projects WHERE name = 'inbox'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert_eq!(project_exists.as_deref(), Some("inbox"));
}

#[tokio::test]
async fn pull_unsynced_rolls_back_sqlite_when_patch_fails() {
    // Given: Neon 側に unsynced が 1 件 (reminds 付き)、かつ
    // patch_task_number は 500 を返す設定。
    // 目的: INSERT + reminds + PATCH がトランザクションで包まれており、
    //       PATCH 失敗で自動 ROLLBACK されることを検証する (リグレッション
    //       ガード。sync_engine::insert_unsynced_with_patch が tx 無しで
    //       書き戻された場合に失敗する)。
    let conn = make_my_task_db();

    let api = MockApi::new()
        .with_unsynced(vec![UnsyncedTask {
            neon_id: 42,
            title: "will-rollback".into(),
            status: "open".into(),
            source: "web".into(),
            project_name: None,
            due: None,
            done_at: None,
            important: false,
            updated_at: ts("2026-04-12T09:00:00Z"),
            created_at: ts("2026-04-12T09:00:00Z"),
            reminds: vec![d(2026, 4, 20), d(2026, 4, 21)],
        }])
        .with_patch_failure();

    // When
    let result = sync_engine::pull_unsynced(&conn, &api, /* dry_run */ false).await;

    // Then (c): エラーが呼び出し元まで伝播する
    assert!(
        matches!(
            result,
            Err(my_task_sync::error::Error::Api { status: 500, .. })
        ),
        "PATCH 失敗は呼び出し元まで Err として伝播すべき: got {:?}",
        result
    );

    // Then (a): tasks には 1 行も残っていない (INSERT が ROLLBACK された)
    let tasks_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        tasks_count, 0,
        "PATCH 失敗時は tasks への INSERT もロールバックされるべき"
    );

    // Then (b): task_reminds にも 1 行も残っていない
    let reminds_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM task_reminds", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        reminds_count, 0,
        "PATCH 失敗時は task_reminds もロールバックされるべき"
    );

    // Sanity: PATCH は確かに試みられた (フラグの効き方を検証)
    assert_eq!(
        api.patched(),
        vec![(42, 1)],
        "PATCH 試行自体は記録される (失敗は tx drop 後に伝播)"
    );
}

// ==============================================================
// pull_updates: LWW (Neon 勝ち / SQLite 勝ち) と reminds 全置換
// ==============================================================

#[tokio::test]
async fn pull_updates_applies_when_neon_is_newer_lww_neon_wins() {
    // Given: SQLite の updated=2026-04-10、Neon の updatedAt=2026-04-12
    let conn = make_my_task_db();
    let id = insert_raw_task(&conn, "original", "open", None, "2026-04-10", "2026-04-01");
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(
        vec![changed_task(
            id,
            "updated-by-neon",
            "done",
            "2026-04-12T10:00:00Z",
            vec![],
        )],
        ts("2026-04-12T12:00:00Z"),
    );

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: SQLite の行が Neon の値で上書き
    let (title, status, updated): (String, String, String) = conn
        .query_row(
            "SELECT title, status, updated FROM tasks WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(title, "updated-by-neon");
    assert_eq!(status, "done");
    assert_eq!(updated, "2026-04-12");
}

#[tokio::test]
async fn pull_updates_skips_when_sqlite_is_newer_lww_sqlite_wins() {
    // Given: SQLite の updated=2026-04-12 > Neon の updatedAt=2026-04-10
    let conn = make_my_task_db();
    let id = insert_raw_task(
        &conn,
        "local-newer",
        "open",
        None,
        "2026-04-12",
        "2026-04-01",
    );
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(
        vec![changed_task(
            id,
            "neon-stale",
            "closed",
            "2026-04-10T00:00:00Z",
            vec![],
        )],
        ts("2026-04-12T12:00:00Z"),
    );

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: SQLite は上書きされない
    let (title, status): (String, String) = conn
        .query_row(
            "SELECT title, status FROM tasks WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(title, "local-newer", "SQLite should win LWW");
    assert_eq!(status, "open");
}

#[tokio::test]
async fn pull_updates_inserts_row_when_sqlite_missing() {
    // Given: SQLite にタスクなし。Neon の changes に 1 件 (task_number=7)
    let conn = make_my_task_db();
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(
        vec![changed_task(7, "new-from-neon", "open", "2026-04-12T10:00:00Z", vec![])],
        ts("2026-04-12T12:00:00Z"),
    );

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: id=7 で INSERT される (task_number = sqlite id を明示)
    let title: String = conn
        .query_row("SELECT title FROM tasks WHERE id = 7", [], |r| r.get(0))
        .expect("row id=7 inserted");
    assert_eq!(title, "new-from-neon");
}

#[tokio::test]
async fn pull_updates_replaces_all_reminds_for_task() {
    // Given: SQLite に既存 reminds が 2 件、Neon の変更に reminds が 1 件
    let conn = make_my_task_db();
    let id = insert_raw_task(&conn, "with-reminds", "open", None, "2026-04-10", "2026-04-01");
    conn.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, '2026-04-13'), (?1, '2026-04-14')",
        params![id],
    )
    .unwrap();
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(
        vec![changed_task(
            id,
            "with-reminds",
            "open",
            "2026-04-12T10:00:00Z",
            vec![d(2026, 4, 20)],
        )],
        ts("2026-04-12T12:00:00Z"),
    );

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: 既存 reminds (04-13, 04-14) は削除され、新しい (04-20) のみ残る
    let dates: Vec<String> = conn
        .prepare("SELECT remind_at FROM task_reminds WHERE task_id = ?1 ORDER BY remind_at")
        .unwrap()
        .query_map(params![id], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(dates, vec!["2026-04-20".to_string()]);
}

#[tokio::test]
async fn pull_updates_uses_none_since_on_initial_run() {
    // Given: state に last_pull_at が無い初回
    let conn = make_my_task_db();
    let (_tmp, state) = open_state();

    let api = MockApi::new();

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: get_changes は since=None で呼ばれる (全件取得)
    let history = api.changes_since_history();
    assert_eq!(history, vec![None]);
}

#[tokio::test]
async fn pull_updates_passes_last_pull_at_as_since_on_subsequent_run() {
    // Given: state に last_pull_at が設定済み
    let conn = make_my_task_db();
    let (_tmp, state) = open_state();
    state
        .set("last_pull_at", "2026-04-11T00:00:00Z")
        .unwrap();

    let api = MockApi::new();

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then
    let history = api.changes_since_history();
    assert_eq!(history, vec![Some("2026-04-11T00:00:00Z".to_string())]);
}

#[tokio::test]
async fn pull_updates_writes_server_time_to_state() {
    // Given
    let conn = make_my_task_db();
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(vec![], ts("2026-04-12T12:00:00Z"));

    // When
    sync_engine::pull_updates(&conn, &api, &state, false)
        .await
        .unwrap();

    // Then: last_pull_at = serverTime
    let stored = state.get("last_pull_at").unwrap();
    assert_eq!(stored.as_deref(), Some("2026-04-12T12:00:00Z"));
}

// ==============================================================
// dry_run: API 書き込み + state.set をすべて抑止
// ==============================================================

#[tokio::test]
async fn dry_run_push_does_not_call_api_nor_update_state() {
    // Given
    let conn = make_my_task_db();
    insert_raw_task(&conn, "a", "open", None, "2026-04-12", "2026-04-12");
    let (_tmp, state) = open_state();

    let api = MockApi::new();

    // When: dry_run=true
    sync_engine::push(&conn, &api, &state, true).await.unwrap();

    // Then: API 呼び出しなし、state 変更なし
    assert!(api.pushed().is_empty(), "push must not be called in dry_run");
    assert!(
        state.get("last_push_at").unwrap().is_none(),
        "last_push_at must NOT be set in dry_run"
    );
}

#[tokio::test]
async fn dry_run_pull_unsynced_does_not_insert_or_patch() {
    // Given: Neon 側に unsynced が 1 件
    let conn = make_my_task_db();
    let api = MockApi::new().with_unsynced(vec![UnsyncedTask {
        neon_id: 42,
        title: "dry".into(),
        status: "open".into(),
        source: "web".into(),
        project_name: None,
        due: None,
        done_at: None,
        important: false,
        updated_at: ts("2026-04-12T00:00:00Z"),
        created_at: ts("2026-04-12T00:00:00Z"),
        reminds: vec![],
    }]);

    // When
    sync_engine::pull_unsynced(&conn, &api, true).await.unwrap();

    // Then: SQLite に INSERT されていない
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "dry_run must not INSERT");

    // Then: PATCH されていない
    assert!(api.patched().is_empty(), "dry_run must not PATCH");
}

#[tokio::test]
async fn dry_run_pull_updates_does_not_mutate_sqlite_or_state() {
    // Given
    let conn = make_my_task_db();
    let id = insert_raw_task(&conn, "keep-me", "open", None, "2026-04-10", "2026-04-01");
    let (_tmp, state) = open_state();

    let api = MockApi::new().with_changes(
        vec![changed_task(
            id,
            "would-overwrite",
            "done",
            "2026-04-12T00:00:00Z",
            vec![d(2026, 4, 20)],
        )],
        ts("2026-04-12T12:00:00Z"),
    );

    // When
    sync_engine::pull_updates(&conn, &api, &state, true)
        .await
        .unwrap();

    // Then: SQLite は変更されない
    let title: String = conn
        .query_row("SELECT title FROM tasks WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "keep-me");

    // Then: state も変更されない
    assert!(
        state.get("last_pull_at").unwrap().is_none(),
        "dry_run must not write state"
    );
}

// ==============================================================
// sync_cycle: 3 ステップが順に呼ばれる
// ==============================================================

#[tokio::test]
async fn sync_cycle_executes_push_then_pull_unsynced_then_pull_updates() {
    // Given: push 対象 1 件、unsynced 1 件、changes 1 件
    let conn = make_my_task_db();
    insert_raw_task(&conn, "to-push", "open", None, "2026-04-12", "2026-04-12");
    let (_tmp, state) = open_state();

    let api = MockApi::new()
        .with_push_results(vec![PushResultRow {
            task_number: 1,
            action: PushAction::Created,
            neon_id: 101,
        }])
        .with_unsynced(vec![UnsyncedTask {
            neon_id: 42,
            title: "unsynced".into(),
            status: "open".into(),
            source: "web".into(),
            project_name: None,
            due: None,
            done_at: None,
            important: false,
            updated_at: ts("2026-04-12T00:00:00Z"),
            created_at: ts("2026-04-12T00:00:00Z"),
            reminds: vec![],
        }])
        .with_changes(vec![], ts("2026-04-12T12:00:00Z"));

    // When
    sync_engine::sync_cycle(&conn, &api, &state, false)
        .await
        .expect("sync_cycle");

    // Then: 3 ステップがすべて発火している
    assert_eq!(api.pushed().len(), 1, "push happened");
    assert_eq!(api.patched().len(), 1, "pull_unsynced happened");
    assert_eq!(
        api.changes_since_history().len(),
        1,
        "pull_updates happened"
    );

    // Then: state が両方更新される
    assert!(state.get("last_push_at").unwrap().is_some());
    assert!(state.get("last_pull_at").unwrap().is_some());
}
