//! DTO serde round-trip tests.
//!
//! my-own の API は camelCase で JSON を送受信する想定 (OVERVIEW.md +
//! 本リポジトリのタスク指示書 Open Question #1 を起点とした推論。my-own
//! 側の実装が入った時点で再検証)。ここでは以下を保証する:
//!   * Rust 側 DTO が camelCase キーでシリアライズ/デシリアライズされる
//!   * 日付は `YYYY-MM-DD` (NaiveDate)、時刻は ISO 8601 文字列
//!   * action enum が `created` / `updated` / `skipped_newer` 文字列にマップされる

use serde_json::{json, Value};

use my_task_sync::model::{
    ChangesResponse, PushAction, PushResponse, SyncTask, UnsyncedTask,
};

// ---------- SyncTask (POST /api/sync/tasks/push の body 要素) ----------

#[test]
fn sync_task_json_uses_camel_case_keys() {
    // Given: API 仕様通りの JSON
    let input = json!({
        "taskNumber": 150,
        "title": "買い物",
        "status": "open",
        "source": "cli",
        "projectName": "personal",
        "due": "2026-04-15",
        "doneAt": null,
        "important": false,
        "updatedAt": "2026-04-12T10:05:00Z",
        "createdAt": "2026-04-12T10:00:00Z",
        "reminds": ["2026-04-14"]
    });

    // When: SyncTask にデシリアライズし、再度シリアライズする
    let parsed: SyncTask = serde_json::from_value(input.clone()).expect("deserialize SyncTask");
    let out = serde_json::to_value(&parsed).expect("serialize SyncTask");

    // Then: camelCase キーが保持されている
    assert!(out.get("taskNumber").is_some(), "taskNumber missing");
    assert!(out.get("projectName").is_some(), "projectName missing");
    assert!(out.get("updatedAt").is_some(), "updatedAt missing");
    assert!(out.get("createdAt").is_some(), "createdAt missing");
    assert!(out.get("doneAt").is_some(), "doneAt missing");

    // Then: snake_case は存在してはならない (rename_all = "camelCase" の動作確認)
    assert!(out.get("task_number").is_none(), "task_number leaked");
    assert!(out.get("project_name").is_none(), "project_name leaked");
    assert!(out.get("updated_at").is_none(), "updated_at leaked");
    assert!(out.get("created_at").is_none(), "created_at leaked");
    assert!(out.get("done_at").is_none(), "done_at leaked");

    // Then: 値も保たれている
    assert_eq!(out["taskNumber"], 150);
    assert_eq!(out["projectName"], "personal");
    assert_eq!(out["due"], "2026-04-15");
    assert_eq!(out["doneAt"], Value::Null);
    assert_eq!(out["reminds"], json!(["2026-04-14"]));
}

#[test]
fn sync_task_accepts_null_project_name_and_dates() {
    // Given: オプショナル項目がすべて null の最小ケース
    let input = json!({
        "taskNumber": 1,
        "title": "no project",
        "status": "open",
        "source": "cli",
        "projectName": null,
        "due": null,
        "doneAt": null,
        "important": false,
        "updatedAt": "2026-04-12T00:00:00Z",
        "createdAt": "2026-04-12T00:00:00Z",
        "reminds": []
    });

    // When / Then: デシリアライズが失敗しない
    let parsed: SyncTask = serde_json::from_value(input).expect("nullable fields accepted");
    let out = serde_json::to_value(&parsed).unwrap();
    assert_eq!(out["projectName"], Value::Null);
    assert_eq!(out["due"], Value::Null);
    assert_eq!(out["doneAt"], Value::Null);
    assert_eq!(out["reminds"], json!([]));
}

// ---------- UnsyncedTask (GET /api/sync/tasks/unsynced) ----------

#[test]
fn unsynced_task_deserializes_neon_id_and_reminds() {
    // Given: sync daemon が pull_unsynced で受け取る JSON
    let input = json!({
        "neonId": 42,
        "title": "Web 作成タスク",
        "status": "open",
        "source": "web",
        "projectName": "inbox",
        "due": null,
        "doneAt": null,
        "important": true,
        "updatedAt": "2026-04-12T09:00:00Z",
        "createdAt": "2026-04-12T09:00:00Z",
        "reminds": ["2026-04-20"]
    });

    // When
    let parsed: UnsyncedTask = serde_json::from_value(input).expect("deserialize UnsyncedTask");
    let out = serde_json::to_value(&parsed).unwrap();

    // Then: neonId が保持される (task_number は採番前なので存在しない or None)
    assert_eq!(out["neonId"], 42);
    assert_eq!(out["projectName"], "inbox");
    assert_eq!(out["reminds"], json!(["2026-04-20"]));
}

// ---------- ChangesResponse (GET /api/sync/tasks/changes) ----------

#[test]
fn changes_response_deserializes_tasks_and_server_time() {
    // Given: pull_updates が受け取る JSON
    let input = json!({
        "tasks": [
            {
                "taskNumber": 150,
                "neonId": 42,
                "title": "買い物",
                "status": "done",
                "source": "cli",
                "projectName": "personal",
                "due": "2026-04-15",
                "doneAt": "2026-04-12",
                "important": false,
                "updatedAt": "2026-04-12T10:05:00Z",
                "createdAt": "2026-04-12T10:00:00Z",
                "reminds": ["2026-04-14"]
            }
        ],
        "serverTime": "2026-04-12T12:00:00Z"
    });

    // When
    let parsed: ChangesResponse =
        serde_json::from_value(input).expect("deserialize ChangesResponse");

    // Then: tasks が 1 件、serverTime が保持される
    assert_eq!(parsed.tasks.len(), 1);

    let out = serde_json::to_value(&parsed).unwrap();
    assert!(out.get("serverTime").is_some(), "serverTime missing");
    assert!(out.get("server_time").is_none(), "server_time leaked");
}

#[test]
fn changes_response_handles_empty_task_list() {
    // Given: 変更がなかった場合の応答
    let input = json!({
        "tasks": [],
        "serverTime": "2026-04-12T12:00:00Z"
    });

    // When
    let parsed: ChangesResponse = serde_json::from_value(input).unwrap();

    // Then
    assert!(parsed.tasks.is_empty());
}

// ---------- PushResponse / PushAction ----------

#[test]
fn push_response_deserializes_all_action_variants() {
    // Given: POST /api/sync/tasks/push の応答 (3 種の action すべてを含む)
    let input = json!({
        "results": [
            { "taskNumber": 150, "action": "created",        "neonId": 42 },
            { "taskNumber": 151, "action": "updated",        "neonId": 43 },
            { "taskNumber": 152, "action": "skipped_newer",  "neonId": 44 }
        ]
    });

    // When
    let parsed: PushResponse = serde_json::from_value(input).expect("deserialize PushResponse");

    // Then: 3 件すべてがパースされる
    assert_eq!(parsed.results.len(), 3);

    // Then: action enum がタグ文字列にマップされている
    let actions: Vec<PushAction> = parsed.results.iter().map(|r| r.action).collect();
    assert!(matches!(actions[0], PushAction::Created));
    assert!(matches!(actions[1], PushAction::Updated));
    assert!(matches!(actions[2], PushAction::SkippedNewer));
}

#[test]
fn push_action_serializes_to_snake_case_string() {
    // Given: 3 種の action
    let variants = [
        (PushAction::Created, "created"),
        (PushAction::Updated, "updated"),
        (PushAction::SkippedNewer, "skipped_newer"),
    ];

    // When / Then: enum → API の文字列値になる
    for (variant, expected) in variants {
        let v = serde_json::to_value(variant).unwrap();
        assert_eq!(v, Value::String(expected.into()), "mapping for {expected}");
    }
}

#[test]
fn push_action_rejects_unknown_variant() {
    // Given: 未知の action 値
    let bad = json!("deleted");

    // When / Then: パースに失敗 (静かに受け入れない = Fail Fast)
    let result: Result<PushAction, _> = serde_json::from_value(bad);
    assert!(result.is_err(), "unknown action must be rejected");
}
