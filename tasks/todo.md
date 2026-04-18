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

- [ ] `src/error.rs` に `Error::Unauthorized` + `IntoResponse` 実装
- [ ] `src/http/auth.rs` (新規) に `require_bearer` middleware
- [ ] `src/http/mod.rs` で `/api/*` ネストに middleware 適用 (`/healthz` は非適用)
- [ ] 単体テスト: ヘッダ欠損 / key 不一致 / 正しい key / `/healthz` 通過 の 4 パターン

→ **CP2 レビュー**

## T3. GET /api/tasks

- [ ] `src/sqlite.rs` に `read_tasks_filtered(conn, status, since, project, limit)`
- [ ] `src/model.rs`: `SyncTask` → `TaskDto` リネーム、旧同期 DTO (`UnsyncedTask` / `ChangedTask` / `ChangesResponse` / `PushResponse` / `PushResultRow` / `PushAction` / `PatchNumberBody`) を削除、`TaskListResponse` 追加
- [ ] `src/http/tasks.rs` (新規) に `list_tasks` ハンドラ
- [ ] ルート登録: `get(list_tasks)`
- [ ] 単体テスト: 空 / 3 件 / status フィルタ / since フィルタ / project フィルタ / limit / 不正 status 400 / 不正 since 400

## T4. GET /api/tasks/:task_number

- [ ] `src/sqlite.rs` に `read_task_by_id`
- [ ] `src/model.rs` に `TaskResponse`
- [ ] `src/http/tasks.rs` に `get_task` ハンドラ + ルート登録
- [ ] 単体テスト: 存在 / 不在 404 / 非数値 path 400

## T5. POST /api/tasks

- [ ] `src/sqlite.rs` に `replace_reminds(conn, task_id, &[NaiveDate])`
- [ ] `src/http/tasks.rs` に `create_task` ハンドラ (unchecked_transaction でまとめる)
- [ ] body に `taskNumber` が来たら 400
- [ ] ルート登録: `post(create_task)`
- [ ] 単体テスト: 最小 body → rowid=1 / project 透過作成 / reminds 複数 / taskNumber 混入 → 400 / 不正 status → 400

→ **CP3 レビュー (初 write 経路の schema 整合性)**

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
