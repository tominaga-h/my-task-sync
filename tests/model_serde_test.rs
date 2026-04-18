//! DTO serde round-trip tests.
//!
//! my-own の HTTP 境界は camelCase で JSON を送受信する想定。ここでは:
//!   * `TaskDto` のキーが camelCase でシリアライズされる
//!   * `null` プロジェクト名 / 日付を許容する
//!   * `TaskListResponse` が `tasks` + `serverTime` を camelCase で返す
//!   * 日付は `YYYY-MM-DD` (NaiveDate)、時刻は ISO 8601 文字列
//!
//! v1 の push/pull DTO (`SyncTask`, `UnsyncedTask`, `ChangedTask`,
//! `ChangesResponse`, `PushResponse`, `PushAction`, `PatchNumberBody`)
//! は v2 で削除されたため、このファイルも T3 で書き直した。

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

use my_task_sync::model::{Status, Task, TaskDto, TaskListResponse};

fn dt(s: &str) -> DateTime<Utc> {
    s.parse::<DateTime<Utc>>().expect("parse ISO 8601")
}

fn date(s: &str) -> NaiveDate {
    s.parse::<NaiveDate>().expect("parse YYYY-MM-DD")
}

// ---------- TaskDto ----------

#[test]
fn task_dto_json_uses_camel_case_keys() {
    let dto = TaskDto {
        task_number: 42,
        title: "write tests".into(),
        status: "open".into(),
        source: "cli".into(),
        project_name: Some("home".into()),
        due: Some(date("2026-05-01")),
        done_at: None,
        important: true,
        updated_at: dt("2026-04-18T10:30:00Z"),
        created_at: dt("2026-04-18T00:00:00Z"),
        reminds: vec![date("2026-04-20")],
    };

    let v: Value = serde_json::to_value(&dto).unwrap();

    // camelCase keys
    assert_eq!(v["taskNumber"], 42);
    assert_eq!(v["projectName"], "home");
    assert_eq!(v["doneAt"], Value::Null);
    assert_eq!(v["updatedAt"], "2026-04-18T10:30:00Z");
    assert_eq!(v["createdAt"], "2026-04-18T00:00:00Z");
    assert_eq!(v["reminds"][0], "2026-04-20");

    // snake_case キーが出現していないこと (逆方向の誤変換検出)
    assert!(v.get("task_number").is_none());
    assert!(v.get("project_name").is_none());
    assert!(v.get("updated_at").is_none());
}

#[test]
fn task_dto_accepts_null_project_name_and_dates() {
    // Given: プロジェクト無し / due / doneAt null のタスク JSON
    let body = json!({
        "taskNumber": 7,
        "title": "t",
        "status": "open",
        "source": "web",
        "projectName": null,
        "due": null,
        "doneAt": null,
        "important": false,
        "updatedAt": "2026-04-18T00:00:00Z",
        "createdAt": "2026-04-18T00:00:00Z",
        "reminds": []
    });

    // When
    let dto: TaskDto = serde_json::from_value(body).unwrap();

    // Then
    assert_eq!(dto.task_number, 7);
    assert!(dto.project_name.is_none());
    assert!(dto.due.is_none());
    assert!(dto.done_at.is_none());
    assert!(dto.reminds.is_empty());
}

#[test]
fn task_dto_from_task_maps_fields_and_promotes_date_to_utc_midnight() {
    // Given: SQLite から読み出した Task
    let t = Task {
        id: 3,
        title: "hello".into(),
        status: Status::Done,
        source: "cli".into(),
        project: Some("work".into()),
        due: Some(date("2026-05-10")),
        done_at: Some(date("2026-04-17")),
        created: date("2026-04-01"),
        updated: date("2026-04-15"),
        important: false,
    };
    let reminds = vec![date("2026-04-20"), date("2026-04-25")];

    // When
    let dto = TaskDto::from_task(t, reminds.clone());

    // Then: `id` → `task_number`、status は小文字文字列、日付は 00:00:00 UTC
    assert_eq!(dto.task_number, 3);
    assert_eq!(dto.status, "done");
    assert_eq!(dto.project_name.as_deref(), Some("work"));
    assert_eq!(dto.updated_at, dt("2026-04-15T00:00:00Z"));
    assert_eq!(dto.created_at, dt("2026-04-01T00:00:00Z"));
    assert_eq!(dto.reminds, reminds);
}

// ---------- TaskListResponse ----------

#[test]
fn task_list_response_serializes_camel_case_and_server_time() {
    let resp = TaskListResponse {
        tasks: vec![],
        server_time: dt("2026-04-18T12:00:00Z"),
    };

    let v: Value = serde_json::to_value(&resp).unwrap();
    assert!(v["tasks"].is_array());
    assert_eq!(v["tasks"].as_array().unwrap().len(), 0);
    assert_eq!(v["serverTime"], "2026-04-18T12:00:00Z");
    assert!(v.get("server_time").is_none());
}

#[test]
fn task_list_response_round_trips_with_tasks() {
    let original = TaskListResponse {
        tasks: vec![TaskDto {
            task_number: 1,
            title: "x".into(),
            status: "open".into(),
            source: "cli".into(),
            project_name: None,
            due: None,
            done_at: None,
            important: false,
            updated_at: dt("2026-04-18T00:00:00Z"),
            created_at: dt("2026-04-18T00:00:00Z"),
            reminds: vec![],
        }],
        server_time: dt("2026-04-18T12:00:00Z"),
    };

    let v = serde_json::to_value(&original).unwrap();
    let back: TaskListResponse = serde_json::from_value(v).unwrap();
    assert_eq!(back.tasks.len(), 1);
    assert_eq!(back.tasks[0].task_number, 1);
    assert_eq!(back.server_time, original.server_time);
}
