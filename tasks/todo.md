# Phase 1 TODO

> 詳細は `tasks/plan.md`。各タスクは上から順に進め、チェックポイント (CP) で立ち止まってレビューする。

## T1. 旧 daemon 撤去 + axum スケルトン

- [x] `src/api_client.rs` / `src/sync_engine.rs` / `src/sync_state.rs` 削除
- [x] `tests/sync_engine_test.rs` / `tests/sync_state_test.rs` 削除
- [x] `src/lib.rs` から削除モジュール参照を消し、`pub mod http;` 追加
- [x] `Cargo.toml` に `axum` / `tower` / `tower-http = { features = ["trace"] }` 追加 (ctrlc を削除し tokio::signal に寄せた)
- [x] `src/config.rs` から `[api]` / `[sync]` / `MY_TASK_SYNC_BASE_URL` / `default_state_db_path` を削除し、`[server] { port, api_key }` + `MY_TASK_SYNC_PORT` を追加
- [x] `src/main.rs` の polling ループを `axum::serve::with_graceful_shutdown` に差し替え
- [x] `src/main.rs` から `--once` / `--dry-run` とヘルプテキストの該当行を削除
- [x] `src/http/mod.rs` (新規) に `Router` と `/healthz` を置く
- [x] `Arc<Mutex<Connection>>` を `AppState` で保持
- [x] `tests/cli_integration_test.rs` の `--once --dry-run` 系テスト削除
- [x] `config.example.toml` を `[server]` 向けに更新
- [x] `rg "sync_engine|sync_state|api_client|SyncApi|HttpApiClient|last_push_at|last_pull_at"` src/ + tests/ で 0 件
- [x] `cargo build` 成功 / `cargo test` 34 件緑 / `curl localhost:13333/healthz → 200 ok` / SIGTERM で 1s 以内に終了
- [ ] 開発マシン側で `~/.config/my-task-sync/state.db` を手動削除 (ユーザーアクション; マージ後でよい)

→ **CP1 レビュー**

## T2. Bearer 認証 middleware

- [x] `src/error.rs` に `Error::Unauthorized` + `IntoResponse` 実装 (5xx は詳細を server ログのみ、4xx はクライアントに簡潔メッセージ)
- [x] `src/http/auth.rs` (新規) に `require_bearer` middleware + 定数時間比較
- [x] `src/http/mod.rs` で `/api/*` ネストに middleware 適用。`.fallback(api_not_found)` で空ルート時も middleware が発火するよう修正
- [x] `AppState` に `api_key: Arc<String>` 追加、`main.rs` で `cfg.server.api_key` を渡す
- [x] 単体テスト: ヘッダ欠損 / 形式不正 (no Bearer prefix) / key 不一致 / 正しい key 404 fall-through / `/healthz` は認証なしで通過 — 5 パターン + `constant_time_eq` 4 パターン
- [x] 実バイナリ smoke test: `/healthz=200 / /api/foo (no auth)=401 / wrong=401 / malformed=401 / correct=404`

→ **CP2 レビュー**

## T3. GET /api/tasks

- [x] `src/error.rs` に `Error::BadRequest(String)` 追加 (400 のクライアント入力エラー用)
- [x] `src/sqlite.rs` に `read_tasks_filtered(status, since, project, limit)` — 全 None で全件、`LIMIT -1` で unlimited
- [x] `src/model.rs`: v1 DTO (`SyncTask` / `UnsyncedTask` / `ChangedTask` / `ChangesResponse` / `PushResponse` / `PushResultRow` / `PushAction` / `PatchNumberBody`) を削除。`TaskDto` + `TaskListResponse` + `TaskDto::from_task` 追加
- [x] `tests/model_serde_test.rs` を `TaskDto` / `TaskListResponse` 向けに書き直し (8 → 5 件)
- [x] `src/http/tasks.rs` (新規) に `list_tasks` ハンドラ + `ListParams`。status 許容値 validation / since YYYY-MM-DD parse → 400
- [x] ルート登録: `/api/tasks` GET
- [x] `tests/http_tasks_test.rs` 結合テスト 10 件: 空 / 3 件 JOIN / status / since / project / limit / 複合フィルタ / 不正 status 400 / 不正 since 400 / limit=-1 → Query 抽出 400
- [x] `make check` 全緑 (fmt/check/clippy/test — 51 件)
- [x] 実バイナリ smoke test: 実 SQLite に 2 件 seed → `/api/tasks` が JSON で返る / status フィルタ / 400 / 401 を確認

→ **CP3 (初 write 経路の schema 整合性) は T5 で到来**

## T4. GET /api/tasks/:task_number

- [ ] `src/sqlite.rs` に `read_task_by_id`
- [ ] `src/model.rs` に `TaskResponse`
- [ ] `src/http/tasks.rs` に `get_task` ハンドラ + ルート登録
- [ ] 単体テスト: 存在 / 不在 404 / 非数値 path 400

## T5. POST /api/tasks

- [x] `src/sqlite.rs` に `replace_reminds(conn, task_id, &[NaiveDate])` + `read_task_by_id(conn, id) -> Option<Task>` (後者は T4 から前倒し — POST の書き戻し返却で必要)
- [x] `src/model.rs` に `TaskCreateDto` (body 用、`taskNumber` 含まず) + `TaskResponse { task, serverTime }`
- [x] `src/http/tasks.rs` に `create_task` ハンドラ (Value 受け → taskNumber 検出で 400 → TaskCreateDto parse → status 許容値チェック → unchecked_transaction で INSERT + replace_reminds → read_task_by_id で書き戻し)
- [x] body に `taskNumber` が来たら 400 (サーバー採番の約束)
- [x] ルート登録: `/api/tasks` に `.post(create_task)` 追加
- [x] 結合テスト (http_tasks_test.rs) 9 件追加: 201+rowid=1 / GET で見える / project 透過作成 / reminds 複数 / 連続採番 / taskNumber 混入 → 400 / 不正 status → 400 / title 欠落 → 400 / 認証なし → 401
- [x] `make check` 全緑 — 合計 64 テスト
- [x] 実バイナリ smoke test (mock tasks.db): 5 ケース全て期待どおり

→ **CP3 (初 write 経路の schema 整合性)** — レビュー対象

## T6. PATCH /api/tasks/:task_number

- [ ] `src/model.rs` に `TaskPatchDto` (全フィールド optional、serde `#[serde(default)]`)
- [ ] `src/sqlite.rs` に `update_task_partial` (read → merge → write)
- [ ] `src/http/tasks.rs` に `patch_task` ハンドラ + ルート登録
- [ ] 単体テスト: title のみ / reminds 置換 / reminds 非送信で保持 / 不在 → 404 / taskNumber 混入 → 400

## T7. GET /api/projects

- [ ] `src/sqlite.rs` に `read_projects`
- [ ] `src/model.rs` に `Project` / `ProjectListResponse`
- [ ] `src/http/projects.rs` (新規) に `list_projects` ハンドラ + ルート登録
- [ ] 単体テスト: 空 / n 件 / 順序 id ASC

→ **CP4 レビュー (全 5 エンドポイントの handler 単体テスト緑)**

## T8. ローカル my-own 結合テスト

- [ ] `cargo run --release` で `:3333` bind 確認
- [ ] my-own (`npm run dev`) を my-task-sync 向けに起動
- [ ] CLI で `my-task add "foo"` → my-own UI 即時表示
- [ ] my-own UI で新規作成 → `my-task ls` 表示
- [ ] 両側で title 更新 → 後書きが残る (LWW の挙動)
- [ ] `my-task done <n>` → my-own UI status 反映
- [ ] my-own で project 新規指定 → `my-task projects` に出現
- [ ] Ctrl-C graceful shutdown (進行中が 502 にならない)
- [ ] 認証トークン誤り → 401

→ **CP5: Phase 1 PR マージ可**

---

## Phase 2 TODO (Phase 1 マージ後に解凍)

- [ ] T9: ngrok 子プロセス spawn + `NgrokGuard` の Drop
- [ ] T10: shutdown 拡張 (HTTP drain → `child.kill().await` → guard drop)
- [ ] T11: `/api/status` (reqwest で localhost:4040/api/tunnels → 集約 JSON)
- [ ] T12: `[ngrok].domain` 設定追加 + `config.example.toml` 更新
- [ ] T13: README に ngrok authtoken セットアップ手順追記
- [ ] CP6: Vercel 上 my-own から公開 URL 経由で結合テスト通過
