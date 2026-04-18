//! `/api/tasks` 結合テスト (GET + POST)。
//!
//! axum Router を `oneshot` で叩き、in-memory SQLite を state に渡して
//! response の status + JSON body を検証する。認証は Bearer token 込み
//! (T2 middleware が通ること前提)。
//!
//! `common/mod.rs` は my-task 本物スキーマの in-memory DB を提供する
//! (実ファイルの tasks.db は一切触らない)。

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::{insert_raw_task, make_my_task_db};
use rusqlite::Connection;
use serde_json::{json, Value};
use tower::ServiceExt;

use my_task_sync::http::{router, AppState};
use my_task_sync::sqlite;

const API_KEY: &str = "test-key";

// ---------- helpers ----------

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("parse json body")
}

fn authed_get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("Authorization", format!("Bearer {API_KEY}"))
        .body(Body::empty())
        .expect("build request")
}

fn authed_post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("Authorization", format!("Bearer {API_KEY}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("build request")
}

fn authed_patch(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::PATCH)
        .uri(uri)
        .header("Authorization", format!("Bearer {API_KEY}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .expect("build request")
}

fn app_with(conn: Connection) -> axum::Router {
    router(AppState::new(conn, API_KEY.into()))
}

/// POST 用の最小 body。テストで差分を付けたい箇所だけ上書きする。
fn minimal_create_body() -> Value {
    json!({
        "title": "new task",
        "status": "open",
        "source": "web",
        "createdAt": "2026-04-18T10:00:00Z",
        "updatedAt": "2026-04-18T10:00:00Z"
    })
}

/// 3 件の multipurpose seed:
/// | id | status | project | updated    | remind     |
/// |----|--------|---------|------------|------------|
/// | 1  | open   | home    | 2026-04-10 | 2026-04-20 |
/// | 2  | done   | (null)  | 2026-04-12 | 2026-04-21 |
/// | 3  | closed | home    | 2026-04-14 | (none)     |
fn seed_three_tasks(conn: &Connection) -> (i64, i64, i64) {
    let pid_home = sqlite::resolve_project(conn, "home").expect("create project home");
    let t1 = insert_raw_task(
        conn,
        "t1",
        "open",
        Some(pid_home),
        "2026-04-10",
        "2026-04-01",
    );
    let t2 = insert_raw_task(conn, "t2", "done", None, "2026-04-12", "2026-04-02");
    let t3 = insert_raw_task(
        conn,
        "t3",
        "closed",
        Some(pid_home),
        "2026-04-14",
        "2026-04-03",
    );
    conn.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, '2026-04-20')",
        rusqlite::params![t1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, '2026-04-21')",
        rusqlite::params![t2],
    )
    .unwrap();
    (t1, t2, t3)
}

// ---------- tests ----------

#[tokio::test]
async fn returns_200_and_empty_array_when_db_is_empty() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app.oneshot(authed_get("/api/tasks")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["tasks"].as_array().unwrap().len(), 0);
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn returns_all_rows_with_project_name_and_reminds_joined() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(app.oneshot(authed_get("/api/tasks")).await.unwrap()).await;

    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);

    // t1: project=home, remind=1 件
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[0]["taskNumber"], 1);
    assert_eq!(tasks[0]["projectName"], "home");
    assert_eq!(tasks[0]["reminds"].as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["reminds"][0], "2026-04-20");

    // t2: project=null
    assert_eq!(tasks[1]["title"], "t2");
    assert!(tasks[1]["projectName"].is_null());
    assert_eq!(tasks[1]["reminds"][0], "2026-04-21");

    // t3: remind なし
    assert_eq!(tasks[2]["title"], "t3");
    assert_eq!(tasks[2]["reminds"].as_array().unwrap().len(), 0);
}

// ---------- filters ----------

#[tokio::test]
async fn status_filter_returns_only_matching_rows() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?status=done"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["status"], "done");
    assert_eq!(tasks[0]["title"], "t2");
}

#[tokio::test]
async fn since_filter_is_inclusive_at_same_day_boundary() {
    // since=2026-04-10T00:00:00Z は "同日" を含む (inclusive)。SQLite の
    // updated は日単位なので、クライアントが前回の serverTime を投げ戻した
    // ときに同日内の追加更新を取りこぼさないようにするのが意図。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?since=2026-04-10T00:00:00Z"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[1]["title"], "t2");
    assert_eq!(tasks[2]["title"], "t3");
}

#[tokio::test]
async fn since_filter_excludes_earlier_days() {
    // since=2026-04-11T... は t1 (updated=2026-04-10) を除外、t2/t3 は含む。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?since=2026-04-11T23:59:59Z"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t2");
    assert_eq!(tasks[1]["title"], "t3");
}

#[tokio::test]
async fn since_truncates_to_date_ignoring_time_component() {
    // 同じ日のどの時刻を投げても同じ結果。time 部分は SQLite 日単位粒度
    // で切り捨てられるため、同日内の 00:00 も 23:59 も挙動が同じ。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let early = body_json(
        app.oneshot(authed_get("/api/tasks?since=2026-04-12T00:00:00Z"))
            .await
            .unwrap(),
    )
    .await;
    let conn2 = make_my_task_db();
    seed_three_tasks(&conn2);
    let app2 = app_with(conn2);
    let late = body_json(
        app2.oneshot(authed_get("/api/tasks?since=2026-04-12T23:59:59Z"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(early["tasks"], late["tasks"]);
}

#[tokio::test]
async fn project_filter_returns_only_matching_rows() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_get("/api/tasks?project=home"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[1]["title"], "t3");
}

#[tokio::test]
async fn limit_caps_the_row_count() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(app.oneshot(authed_get("/api/tasks?limit=2")).await.unwrap()).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0]["title"], "t1");
    assert_eq!(tasks[1]["title"], "t2");
}

#[tokio::test]
async fn limit_zero_returns_empty_array() {
    // `?limit=0` は 0 件を明示的に要求する。DEFAULT_LIMIT に退化していない
    // ことの regression guard。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(app.oneshot(authed_get("/api/tasks?limit=0")).await.unwrap()).await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 0);
}

#[tokio::test]
async fn filters_can_combine() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    // status=closed かつ project=home は t3 のみ
    let body = body_json(
        app.oneshot(authed_get("/api/tasks?status=closed&project=home"))
            .await
            .unwrap(),
    )
    .await;
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["title"], "t3");
}

// ---------- 400 paths ----------

#[tokio::test]
async fn invalid_status_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?status=bogus"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("status"));
}

#[tokio::test]
async fn invalid_since_non_iso_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?since=not-a-date"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("since"));
}

#[tokio::test]
async fn invalid_since_date_only_rejected() {
    // I3 の決定: since は RFC 3339 datetime のみ。YYYY-MM-DD 単独は 400。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?since=2026-04-10"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_limit_type_returns_400_via_query_extractor() {
    // axum の Query 抽出が u32 parse に失敗すると 400 を自動で返す。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_get("/api/tasks?limit=-1"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------- POST /api/tasks (T5) ----------

#[tokio::test]
async fn post_minimal_body_returns_201_with_assigned_task_number() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_post("/api/tasks", minimal_create_body()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    // 採番された rowid = 1 (最初の INSERT)
    assert_eq!(body["task"]["taskNumber"], 1);
    assert_eq!(body["task"]["title"], "new task");
    assert_eq!(body["task"]["status"], "open");
    assert_eq!(body["task"]["source"], "web");
    assert!(body["task"]["projectName"].is_null());
    assert_eq!(body["task"]["reminds"].as_array().unwrap().len(), 0);
    assert!(body["serverTime"].is_string());
}

#[tokio::test]
async fn post_persists_row_visible_in_subsequent_get() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let created_resp = app
        .clone()
        .oneshot(authed_post("/api/tasks", minimal_create_body()))
        .await
        .unwrap();
    assert_eq!(created_resp.status(), StatusCode::CREATED);

    // GET で同じ task が返ってくる
    let list = body_json(app.oneshot(authed_get("/api/tasks")).await.unwrap()).await;
    let tasks = list["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["title"], "new task");
    assert_eq!(tasks[0]["taskNumber"], 1);
}

#[tokio::test]
async fn post_with_project_name_transparently_creates_project() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let mut body = minimal_create_body();
    body["projectName"] = json!("brand-new-proj");

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    assert_eq!(body["task"]["projectName"], "brand-new-proj");
}

#[tokio::test]
async fn post_with_reminds_persists_all_reminds() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let mut body = minimal_create_body();
    body["reminds"] = json!(["2026-04-20", "2026-04-25", "2026-05-01"]);

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    let reminds = body["task"]["reminds"].as_array().unwrap();
    assert_eq!(reminds.len(), 3);
    assert_eq!(reminds[0], "2026-04-20");
    assert_eq!(reminds[1], "2026-04-25");
    assert_eq!(reminds[2], "2026-05-01");
}

#[tokio::test]
async fn post_assigns_sequential_rowids() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let r1 = app
        .clone()
        .oneshot(authed_post("/api/tasks", minimal_create_body()))
        .await
        .unwrap();
    let r2 = app
        .oneshot(authed_post("/api/tasks", minimal_create_body()))
        .await
        .unwrap();
    let b1 = body_json(r1).await;
    let b2 = body_json(r2).await;
    assert_eq!(b1["task"]["taskNumber"], 1);
    assert_eq!(b2["task"]["taskNumber"], 2);
}

// ---------- POST 400 paths ----------

#[tokio::test]
async fn post_with_task_number_in_body_returns_400() {
    // サーバー採番の約束: body に taskNumber を入れたら 400。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let mut body = minimal_create_body();
    body["taskNumber"] = json!(99);

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = body_json(resp).await;
    assert!(err["error"].as_str().unwrap().contains("taskNumber"));
}

#[tokio::test]
async fn post_with_invalid_status_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let mut body = minimal_create_body();
    body["status"] = json!("wibble");

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = body_json(resp).await;
    assert!(err["error"].as_str().unwrap().contains("status"));
}

#[tokio::test]
async fn post_missing_required_title_returns_400() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    // title を落とす
    let body = json!({
        "status": "open",
        "source": "web",
        "createdAt": "2026-04-18T10:00:00Z",
        "updatedAt": "2026-04-18T10:00:00Z"
    });

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn post_with_all_optional_fields_round_trips() {
    // S12: minimal_create_body だと important=false / due=null / doneAt=null
    // の経路しか通らない。全 optional を埋めた body で書き込み→レスポンス
    // 復元が正しく回ることを 1 テストで pin する。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let body = json!({
        "title": "full task",
        "status": "done",
        "source": "cli",
        "projectName": "work",
        "due": "2026-05-01",
        "doneAt": "2026-04-18",
        "important": true,
        "updatedAt": "2026-04-18T10:00:00Z",
        "createdAt": "2026-04-15T00:00:00Z",
        "reminds": ["2026-04-20", "2026-04-25"]
    });

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp).await;
    let t = &body["task"];
    assert_eq!(t["title"], "full task");
    assert_eq!(t["status"], "done");
    assert_eq!(t["source"], "cli");
    assert_eq!(t["projectName"], "work");
    assert_eq!(t["due"], "2026-05-01");
    assert_eq!(t["doneAt"], "2026-04-18");
    assert_eq!(t["important"], true);
    assert_eq!(t["updatedAt"], "2026-04-18T00:00:00Z"); // 日単位 truncate
    assert_eq!(t["createdAt"], "2026-04-15T00:00:00Z");
    assert_eq!(t["reminds"], json!(["2026-04-20", "2026-04-25"]));
}

#[tokio::test]
async fn post_with_unknown_field_returns_400() {
    // S11: deny_unknown_fields で クライアント typo を捕まえる。
    // 例: `reminders` (s 余分) を silently 捨てずに 400 にする。
    let conn = make_my_task_db();
    let app = app_with(conn);

    let mut body = minimal_create_body();
    body["reminders"] = json!(["2026-04-20"]); // typo: should be "reminds"

    let resp = app.oneshot(authed_post("/api/tasks", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let err = body_json(resp).await;
    // serde のエラーメッセージに未知フィールド名が含まれるので、
    // クライアントが何を間違えたかすぐ分かる。
    assert!(
        err["error"].as_str().unwrap().contains("reminders"),
        "error should name the unknown field, got: {}",
        err["error"]
    );
}

#[tokio::test]
async fn post_without_auth_returns_401() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    // Authorization ヘッダなしで POST
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/tasks")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&minimal_create_body()).unwrap(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---------- PATCH /api/tasks/{task_number} (T6) ----------

#[tokio::test]
async fn patch_partial_title_only_keeps_other_fields() {
    // seed: t1 = open/home/2026-04-10, remind 2026-04-20
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch("/api/tasks/1", json!({"title": "renamed"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    let t = &body["task"];
    assert_eq!(t["taskNumber"], 1);
    assert_eq!(t["title"], "renamed");
    // 以下は元の seed 値のまま
    assert_eq!(t["status"], "open");
    assert_eq!(t["projectName"], "home");
    assert_eq!(t["reminds"], json!(["2026-04-20"]));
}

#[tokio::test]
async fn patch_reminds_replaces_wholesale() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_patch(
            "/api/tasks/1",
            json!({"reminds": ["2026-05-01", "2026-05-10"]}),
        ))
        .await
        .unwrap(),
    )
    .await;
    // 既存の 2026-04-20 は消え、新しい 2 件で全置換。
    assert_eq!(body["task"]["reminds"], json!(["2026-05-01", "2026-05-10"]));
}

#[tokio::test]
async fn patch_without_reminds_key_preserves_existing() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    // reminds キー未送信なので既存 2026-04-20 が残るはず。
    let body = body_json(
        app.oneshot(authed_patch("/api/tasks/1", json!({"title": "x"})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["task"]["reminds"], json!(["2026-04-20"]));
}

#[tokio::test]
async fn patch_nullable_project_name_to_null_clears_it() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    // t1 は projectName=home。null 送信で未所属にクリアできること。
    let body = body_json(
        app.oneshot(authed_patch("/api/tasks/1", json!({"projectName": null})))
            .await
            .unwrap(),
    )
    .await;
    assert!(body["task"]["projectName"].is_null());
}

#[tokio::test]
async fn patch_updated_at_auto_bumps_when_not_sent() {
    // updatedAt 未送信 → サーバーが Utc::now() で更新 (今日の日付に書き換わる)。
    // seed の t1 は updatedAt=2026-04-10T00:00:00Z。PATCH 後は今日に進む。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let before = chrono::Utc::now().date_naive();
    let body = body_json(
        app.oneshot(authed_patch("/api/tasks/1", json!({"title": "bumped"})))
            .await
            .unwrap(),
    )
    .await;
    let updated_str = body["task"]["updatedAt"].as_str().unwrap();
    let updated_dt: chrono::DateTime<chrono::Utc> = updated_str.parse().unwrap();
    // auto-bump された日付は今日と同日 (テスト実行時の境界で前後するのを
    // 避けるため、before 以降を許容)。
    assert!(
        updated_dt.date_naive() >= before,
        "updatedAt should be auto-bumped to today, got {updated_str}"
    );
    // seed の 2026-04-10 からは必ず進んでいること。
    assert!(updated_dt.date_naive() > chrono::NaiveDate::from_ymd_opt(2026, 4, 10).unwrap());
}

#[tokio::test]
async fn patch_updated_at_uses_sent_value_when_provided() {
    // updatedAt を明示送信したときは auto-bump せず送信値を使う。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let body = body_json(
        app.oneshot(authed_patch(
            "/api/tasks/1",
            json!({"title": "x", "updatedAt": "2026-06-15T08:00:00Z"}),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(body["task"]["updatedAt"], "2026-06-15T00:00:00Z");
}

#[tokio::test]
async fn patch_nonexistent_task_returns_404() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch("/api/tasks/9999", json!({"title": "x"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["error"], "not found");
}

#[tokio::test]
async fn patch_with_task_number_in_body_returns_400() {
    // URL 側の task_number が唯一の権威。body 側は禁止。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch(
            "/api/tasks/1",
            json!({"taskNumber": 99, "title": "x"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("taskNumber"));
}

#[tokio::test]
async fn patch_with_unknown_field_returns_400() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch("/api/tasks/1", json!({"reminders": []})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("reminders"));
}

#[tokio::test]
async fn patch_with_invalid_status_returns_400() {
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch("/api/tasks/1", json!({"status": "wibble"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("status"));
}

#[tokio::test]
async fn patch_empty_body_is_a_noop_200() {
    // `{}` はすべてのフィールドが未送信扱い。updatedAt だけ auto-bump する。
    let conn = make_my_task_db();
    seed_three_tasks(&conn);
    let app = app_with(conn);

    let resp = app
        .oneshot(authed_patch("/api/tasks/1", json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["task"]["title"], "t1");
    assert_eq!(body["task"]["projectName"], "home");
    assert_eq!(body["task"]["reminds"], json!(["2026-04-20"]));
}

#[tokio::test]
async fn patch_without_auth_returns_401() {
    let conn = make_my_task_db();
    let app = app_with(conn);

    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/tasks/1")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"title": "x"})).unwrap(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
