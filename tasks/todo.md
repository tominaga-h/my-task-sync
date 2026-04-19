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

- [x] `src/sqlite.rs` の `read_task_by_id` (T5 で前倒し済み) を流用
- [x] `src/model.rs` の `TaskResponse` (T5 で前倒し済み) を流用
- [x] `src/http/tasks.rs` に `get_task` ハンドラ追加
- [x] `src/http/mod.rs` のルートを `get(get_task).patch(patch_task)` に合成 (axum 0.8 の MethodRouter チェイン)
- [x] 結合テスト (http_tasks_test.rs) 7 件追加: 存在 (reminds + project 埋め込み) / projectName=null / reminds なし → [] / 不在 → 404 / 非数値 path → 400 / 認証なし → 401 / POST→GET round-trip
- [x] `docs/API.md` に GET /:n セクション + 実装ステータス表更新
- [x] `make check` 全緑 — 合計 92 件
- [x] 実バイナリ smoke test (mock tasks.db): 4 ケース期待どおり (存在 / 不在 404 / 非数値 400 / 認証なし 401)

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

- [x] `src/error.rs` に `Error::NotFound` + 404 マッピング追加
- [x] `src/http/tasks.rs` に `patch_task` ハンドラ + 5 つの private helper (`patch_required_string` / `patch_required_bool` / `patch_nullable_string` / `patch_nullable_date` / `patch_required_date_from_datetime` + `parse_datetime_to_date` + `parse_reminds_array`)
  - body は `Json<Value>` 受けで nullable 3 状態 (未送信 / null / 値) を explicit に区別
  - `PATCH_ALLOWED_KEYS` allowlist で未知フィールドを 400 に
  - `updatedAt` は未送信時 `Utc::now()` auto-bump、送信時は指定値を採用
  - `taskNumber` 混入 → 400、存在しない task_number → 404
- [x] `src/http/mod.rs` に `/tasks/{task_number}` の `.patch(patch_task)` を登録 (axum 0.8 の `{name}` 記法)
- [x] 結合テスト (http_tasks_test.rs) 12 件追加: title のみ更新で他フィールド保持 / reminds 全置換 / reminds 未送信で保持 / projectName=null でクリア / updatedAt auto-bump / updatedAt 指定値 / 不在 → 404 / taskNumber 混入 → 400 / 未知フィールド → 400 / 不正 status → 400 / 空 `{}` no-op / 認証なし → 401
- [x] `docs/API.md` に PATCH セクションを追加 (フィールド一覧 + 3 状態の説明 + curl 例) + 実装ステータス表を更新
- [x] `make check` 全緑 — 合計 78 テスト
- [x] 実バイナリ smoke test (mock tasks.db): 6 ケース全て期待どおり (title 更新 + auto-bump / projectName クリア / reminds 置換 / 404 / 400 taskNumber / 400 unknown)

## T7. GET /api/projects

- [x] `src/sqlite.rs` に `read_projects(conn) -> Vec<Project>` (`ORDER BY id` で挿入順)
- [x] `src/model.rs` に `Project { id, name }` / `ProjectListResponse { projects, serverTime }`
- [x] `src/http/projects.rs` (新規) に `list_projects` ハンドラ
- [x] `src/http/mod.rs` に `pub mod projects;` + `/projects` ルート登録
- [x] `tests/http_projects_test.rs` (新規) 4 件: 空 → 空配列 / 3 件で順序 id ASC / POST 透過作成したプロジェクトが見える / 認証なし → 401
- [x] `tests/common/mod.rs` に `#![allow(dead_code)]` 追加 (integration test crate ごとに common が再コンパイルされ、未使用ヘルパに警告が出るため)
- [x] `docs/API.md` に GET /api/projects セクション + 実装ステータス表更新
- [x] `make check` 全緑 — 合計 85 件
- [x] 実バイナリ smoke test (mock tasks.db): 3 ケース期待どおり (3 件 seed / 認証なし 401 / POST 透過作成 → projects に反映)

→ **CP4 到達 (全 5 エンドポイントの handler 単体テスト緑)**。T4 (GET /:n) 残で Phase 1 code 完了。

## T8. ローカル my-own 結合テスト

- [x] `cargo run --release` で `:3333` bind 確認
- [x] my-own (`npm run dev`) を my-task-sync 向けに起動
- [x] CLI で `my-task add "foo"` → my-own UI 即時表示
- [x] my-own UI で新規作成 → `my-task ls` 表示
- [x] 両側で title 更新 → 後書きが残る (LWW の挙動)
- [x] `my-task done <n>` → my-own UI status 反映
- [x] my-own で project 新規指定 → `my-task projects` に出現
- [x] Ctrl-C graceful shutdown (進行中が 502 にならない)
- [x] 認証トークン誤り → 401

→ **CP5: Phase 1 PR マージ可**

---

## Phase 2 TODO

> Phase 1 + T8 (ローカル my-own 結合テスト) クリア済み。Phase 2 は ngrok
> 自動起動 + `/api/status` を追加し、Vercel 上の my-own から public URL
> 経由で到達可能にする。詳細は `tasks/plan.md` Phase 2 セクション参照。

## T9. ngrok 設定 + 子プロセス spawn + Drop ガード

- [x] `src/config.rs` に `FileNgrok { domain: Option<String> }` / `NgrokConfig { domain: Option<String> }` を追加
- [x] `resolve()` で config.toml の `[ngrok].domain` を読み、環境変数 `MY_TASK_SYNC_NGROK_DOMAIN` で上書き可能に (空文字は未設定扱い)
- [x] `src/ngrok.rs` (新規):
  - [x] `struct NgrokGuard { child: Option<tokio::process::Child> }` + 自前 `Debug` impl (child の内部状態を panic メッセージに出さない)
  - [x] `impl Drop` で `drop_inner()` → `child.start_kill()` (再入可能・`take()` で 2 回目は no-op)
  - [x] `pub async fn spawn(domain, port) -> Result<NgrokGuard, Error>` + テスト用の `spawn_internal`
  - [x] stdout/stderr を `/tmp/my-task-sync-ngrok.{out,err}.log` に追記
  - [x] ngrok バイナリ不在 (`io::ErrorKind::NotFound`) を `Error::Config` にマップ (brew/authtoken 手順案内付き)
  - [x] `Child::id()` を tracing::info! でログ
- [x] `src/lib.rs` に `pub mod ngrok;` 追加
- [x] `src/main.rs::run()` で bind 後に spawn し、guard を serve のスコープ内で保持。`domain` 未設定時はスキップ ("ngrok disabled ([ngrok].domain not set)")
- [x] 結合テスト (config_test.rs) 4 件追加: 未設定 → None / TOML から読める / env が TOML を上書き / 空文字は未設定扱い
- [x] `src/ngrok.rs` 単体テスト 2 件: ngrok 不在時のエラーメッセージに "ngrok" + 案内文言 / Drop 2 回 no-op
- [x] `make check` 全緑 — 合計 98 件
- [x] 実バイナリ smoke test: domain 未設定で "ngrok disabled" / domain 設定で "ngrok subprocess started pid=<N>" ログ / SIGTERM → "ngrok subprocess kill requested (drop)" 起動

## T10. graceful shutdown への統合

- [x] `Cargo.toml` に `libc = "0.2"` 追加 (cfg(unix) target-specific、killpg syscall 用)
- [x] `src/ngrok.rs::NgrokGuard::kill_and_wait(mut self) -> Result<(), Error>` を追加
  - [x] `child.id()` を取得 (= pgid、process_group(0) のおかげで)
  - [x] `#[cfg(unix)]` で `libc::killpg(pgid, SIGKILL)` → PG 全体を SIGKILL
  - [x] ESRCH (PG 既に消滅) は info log + no-op
  - [x] killpg 失敗時は `child.start_kill()` にフォールバック
  - [x] `#[cfg(not(unix))]` では `start_kill` のみ (Windows 退避)
  - [x] `child.wait().await` で zombie を reap + 終了ステータスをログ
  - [x] `self` を consume するので 2 重呼び出し不可 (型で防ぐ)
- [x] `src/main.rs::run()` の shutdown フロー:
  - [x] serve drain 完了 or `GRACEFUL_SHUTDOWN_SECS` 超過後に `guard.kill_and_wait()` を呼ぶ
  - [x] ngrok_guard は `Option<NgrokGuard>` で `.take()` 経由で consume
  - [x] Drop ガードは panic / 早期 return の保険として残る (暗黙)
- [x] 結合テスト 3 件 (ngrok.rs inline):
  - [x] `kill_and_wait_terminates_live_child_promptly`: `sleep 30` を立てて 3s 以内に reap (hang 検出)
  - [x] `kill_and_wait_on_empty_guard_is_noop`: child=None で Ok
  - [x] `kill_and_wait_is_idempotent_with_already_exited_child`: child 死後に呼んでも ESRCH を no-op 扱い
- [x] `make check` 全緑 — 合計 101 件
- [x] 実バイナリ smoke test: SIGTERM 時のログに "killing ngrok subprocess group" + "ngrok subprocess reaped status=..." が出ることを確認

### T10 review fixes (S22 / S23)

- [x] **S22**: Drop 経路 (`drop_inner`) も `killpg(pgid, SIGKILL)` 優先に。panic / 早期 return でも PG 全体 kill が効く。killpg 失敗時 + 非 unix は `start_kill` フォールバック
- [x] **S23**: main.rs の `kill_and_wait` 呼び出しに `tokio::time::timeout(GRACEFUL_SHUTDOWN_SECS)` を被せる。`child.wait()` が stuck した場合も shutdown を infinite hang させない
- [x] 結合テスト 1 件追加 (ngrok.rs inline): `drop_kills_live_child_pg_without_panic` で Drop path が panic なく完走することを pin
- [x] `make check` 全緑 — 合計 102 件

## T11. `GET /api/status`

- [x] `src/http/status.rs` (新規) に `get_status` ハンドラ + DTO (`StatusResponse` / `ServerStatus` / `SqliteStatus` / `NgrokStatus`) を定義
- [x] server セクション: version (`env!("CARGO_PKG_VERSION")`) / uptime_seconds / sqlite (path + ok)
  - [x] `AppState` に `started_at: Instant` / `sqlite_path: Arc<String>` / `ngrok_domain: Option<Arc<String>>` を追加
  - [x] SQLite health は `SELECT 1` で ok 判定、mutex 毒化 / SQL 失敗は `false` に寄せて 200 を保つ
- [x] ngrok セクション: 3 状態 (disabled / unreachable / up) をフラット JSON で表現 (`skip_serializing_if = Option::is_none`)
  - [x] `fetch_ngrok_tunnels()` で `reqwest::Client::builder().timeout(2s)` を使い `http://localhost:4040/api/tunnels` を叩く
  - [x] `parse_tunnel_status()` 純関数で `publicUrl` / `forwardingTo` / `httpRequestsTotal` / `httpRequestsPerMinute` (`rate1 * 60`) / `connectionsTotal` を抽出
- [x] `src/http/mod.rs` の router で `/api/status` を **認証 middleware の外側** に配置 (exact match `/api/status` が `.nest("/api", ...)` より優先)
- [x] `AppState::new` シグネチャ変更に伴い tests/http_tasks_test.rs / tests/http_projects_test.rs / inline test helper を合わせて更新
- [x] 単体テスト 5 件 (status.rs inline): parse_tunnel_status の 4 shape (full / empty array / missing metrics / top-level garbage) + NgrokStatus serialize の disabled/unreachable shape
- [x] 結合テスト 4 件 (tests/http_status_test.rs 新規): 認証なしで 200 / server セクション / ngrok disabled / ngrok unreachable
- [x] `make check` 全緑 — 合計 112 件
- [x] 実バイナリ smoke test で 3 状態すべて確認:
  - [x] disabled: `--config` で空設定 → `{ "enabled": false }` のみ
  - [x] up: ユーザー実 config + 実 ngrok → publicUrl / forwardingTo / metrics 全填
  - [x] unreachable: bogus domain で ngrok admin が未起動 → reachable:false + error メッセージ

## T12. docs 更新

- [ ] `docs/API.md`:
  - [ ] ステータス表で `GET /api/status` を ✅ に
  - [ ] 新規セクションで 3 状態のレスポンス例
- [ ] `docs/SERVER_DESIGN.md` Phase 2 を「実装済み」へ、`/api/status` shape を実装と揃える
- [ ] `README.md` + `docs/README_ja.md`:
  - [ ] Install に `brew install ngrok` + `ngrok config add-authtoken ...` + `ngrok config check`
  - [ ] Configure に `[ngrok].domain` の例を追加
  - [ ] 環境変数表に `MY_TASK_SYNC_NGROK_DOMAIN` 追加 (未設定 = ngrok 無効と明記)
  - [ ] Manage に `curl localhost:3333/api/status` で到達性確認
- [ ] `config.example.toml` に `[ngrok]` セクション (`# domain = "..."` のコメント形で「未設定がデフォルト」と分かる形)
- [ ] `docs/MY_OWN_INTEGRATION.md` Phase 2 セクションを更新 (ngrok URL への env 切替 + `/api/status` で到達性確認)

## T13. CP6 — Vercel 上 my-own からの結合テスト

- [ ] ngrok 稼働中に `curl https://<domain>/healthz` → 200
- [ ] `curl https://<domain>/api/status | jq '.ngrok.reachable'` → `true`
- [ ] Vercel my-own の env を更新: `MY_TASK_SYNC_BASE_URL=https://<domain>`
- [ ] my-own 本番 (Vercel) でタスク一覧が表示
- [ ] my-own UI で新規作成 → `my-task ls` で確認
- [ ] CLI で `my-task add` → my-own 本番で確認
- [ ] my-task-sync Ctrl-C → my-own が 502 エラー表示 (期待通り)
- [ ] my-task-sync 再起動 → my-own 回復
- [ ] `tasks/phase2-integration-check-YYYY-MM-DD.md` に checklist 結果を記録

→ **CP6 到達 = Phase 2 完了**
