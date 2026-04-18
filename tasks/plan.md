# Phase 1 実装プラン: REST サーバー移行

> **参照仕様**: `docs/SERVER_DESIGN.md`
> **対象**: Phase 1 (REST サーバー骨格 + `/api/tasks` + `/api/projects`)
> **Phase 2** (ngrok + `/api/status`) は末尾でスケッチのみ
> **このドキュメントはプランのみ — コードは変更しない**

## ゴール

`cargo run` で axum サーバーが `:3333` に bind し、ローカル my-own から Bearer 認証付きで 5 本の REST エンドポイントを叩いてタスクの CRUD が成立する状態にする。polling daemon 時代の `sync_engine` / `sync_state` / `api_client` を削除する。

## スコープ (In)

- 旧 polling daemon コード (`sync_engine.rs` / `sync_state.rs` / `api_client.rs` + 対応テスト) の削除
- `config.toml` の `[server]` セクション追加と `[api]` / `[sync]` 廃止
- axum サーバー + graceful shutdown + `tower_http::trace::TraceLayer`
- Bearer 認証 middleware (`/api/*` に適用)
- 5 エンドポイント: `GET /api/tasks` (クエリ絞り込み) / `GET /api/tasks/:n` / `POST /api/tasks` / `PATCH /api/tasks/:n` / `GET /api/projects`
- `sqlite.rs` への新規 helper 追加 (by-id 取得 / 絞り込み / projects 列挙 / reminds 置換)
- `Error::IntoResponse` 実装 (axum に HTTP マッピング)
- ハンドラ単体テスト (axum `Router::oneshot`)
- ローカル my-own と手動結合テスト (checklist)

## スコープ (Out)

- ngrok 子プロセス管理 / `/api/status` (Phase 2)
- 楽観ロック / `If-Match` (後続検討)
- `DELETE /api/tasks/:n` (合意により生やさない)
- `r2d2` コネクションプール (性能問題が出るまで `Arc<Mutex<Connection>>`)
- `docs/OVERVIEW.md` の削除 (履歴のため `main` に残す)
- Neon 側 my-own の実装変更 (別リポジトリ)

## 依存グラフ

```
T1 (skeleton)
 └─► T2 (auth middleware)
      ├─► T3 (GET /api/tasks)       ─┐
      ├─► T4 (GET /api/tasks/:n)     ├─► T8 (end-to-end)
      ├─► T5 (POST /api/tasks)       │
      ├─► T6 (PATCH /api/tasks/:n)   │
      └─► T7 (GET /api/projects)    ─┘
```

T3〜T7 は互いに独立なので、review コストが気にならなければ並行実装可。推奨順: T3 → T5 → T6 → T4 → T7 (Read → Create → Update の価値順; 404 を返す T4 は先行エンドポイント完成後のほうがスキーマが固まる)。

## 事前準備 (Cargo.toml 変更 / T1 に含める)

**追加**:
```toml
axum = "0.7"
tower = "0.5"
tower-http = { version = "0.6", features = ["trace"] }
```
**維持**: `reqwest` (Phase 2 の `/api/status` で localhost:4040 を叩くため残す)
**維持**: `rusqlite` / `tokio` / `serde*` / `chrono` / `toml` / `dirs` / `tracing*` / `ctrlc`

## 垂直スライス

各タスクは 1 PR ではなく 1 コミット単位想定。タスク完了条件に acceptance と verification を明記。

---

### T1. 旧 daemon 撤去 + axum スケルトン

**目的**: 旧 polling daemon を消し、axum サーバーが `/healthz` だけ返す状態にする。

**変更**:
- 削除: `src/api_client.rs` / `src/sync_engine.rs` / `src/sync_state.rs` / `tests/sync_engine_test.rs` / `tests/sync_state_test.rs`
- `src/lib.rs`: 削除モジュール参照を消し、`pub mod http;` を追加
- `src/config.rs`:
  - `[api]` → `[server]` に置換 (`port: u16` / `api_key: String`)
  - `[sync]` セクションと `DEFAULT_INTERVAL_SECONDS` / `MY_TASK_SYNC_BASE_URL` 削除
  - `MY_TASK_SYNC_PORT` 環境変数追加
  - `default_state_db_path()` 削除 (state.db 不要)
- `src/main.rs`:
  - `--once` / `--dry-run` / HELP_TEXT 該当行削除
  - `run()` を polling ループから `axum::serve::with_graceful_shutdown` に差し替え
  - `SyncState::open` 呼び出し削除
  - `Arc<Mutex<Connection>>` を `axum::extract::State` で共有
- `src/http/mod.rs` (新規): `Router::new().route("/healthz", get(|| async { "ok" }))`
- `Cargo.toml`: axum / tower / tower-http 追加
- `tests/cli_integration_test.rs`: `--once --dry-run` 関連テスト削除 (代わりに「bind 成功 & SIGTERM で終了」テスト追加 — ただし別タスクに分けても良い)
- `~/.config/my-task-sync/state.db` を開発マシンから削除 (ドキュメントに記載)

**受け入れ条件**:
- `cargo build` がパス
- `cargo test` が緑 (削除テストを除く残りが通る)
- `cargo run` で `:3333` に bind し `curl localhost:3333/healthz` が `200 ok` を返す
- Ctrl-C で 5 秒以内に終了 (graceful shutdown が効く)
- `rg "sync_engine|sync_state|api_client|SyncApi|HttpApiClient|last_push_at|last_pull_at"` がソースにヒットしない

**検証手順**:
1. `cargo build --release`
2. `cargo test`
3. 別ターミナルで `MY_TASK_SYNC_API_KEY=dummy MY_TASK_SYNC_PORT=3333 cargo run`
4. `curl -v localhost:3333/healthz` → 200
5. サーバーに `Ctrl-C` → 即時終了を確認

---

### T2. Bearer 認証 middleware

**目的**: `/api/*` に `Authorization: Bearer <api_key>` を強制する axum middleware を追加する。

**変更**:
- `src/http/auth.rs` (新規): `axum::middleware::from_fn_with_state` 用の関数を定義
- `src/http/mod.rs`: `/api` ネストルーターに `middleware::from_fn_with_state(state, require_bearer)` を適用
- `/healthz` は認証なしのまま (smoke test 用)
- ヘッダ欠損 / 形式不正 / key 不一致すべて `401 Unauthorized` ({"error": "..."} JSON body)
- `src/error.rs`: `Error::Unauthorized` バリアント追加 + `IntoResponse` 実装 (次タスクで使うので T2 で入れる)

**受け入れ条件**:
- `curl /api/foo` → 401
- `curl -H "Authorization: Bearer <wrong>" /api/foo` → 401
- `curl -H "Authorization: Bearer <correct>" /api/foo` → 404 (route 未定義)
- `curl /healthz` → 200 (認証なしで通る)
- 認証 middleware の unit test (Router::oneshot で 401/404 を assert)

**検証手順**:
1. `cargo test http::auth`
2. 上記 curl を実際に叩いて確認

---

### T3. `GET /api/tasks`

**目的**: クエリ絞り込みありで task 一覧を返す初めての本番エンドポイント。

**変更**:
- `src/sqlite.rs` に `read_tasks_filtered(conn, status, since, project, limit)` 追加
  - `since` は `Option<NaiveDate>` (既存 `read_tasks_since` を内部再利用可だが、追加フィルタを掛けられるよう SQL 直書きで実装)
  - 空フィルタは全件
- `src/model.rs`:
  - `SyncTask` を `TaskDto` にリネーム
  - `UnsyncedTask` / `ChangedTask` / `ChangesResponse` / `PushResponse` / `PushResultRow` / `PushAction` / `PatchNumberBody` を削除
  - 新規: `TaskListResponse { tasks: Vec<TaskDto>, server_time: DateTime<Utc> }`
- `src/http/tasks.rs` (新規): `list_tasks` ハンドラ
- ルート追加: `.route("/api/tasks", get(tasks::list_tasks))`
- Task → TaskDto 変換に reminds JOIN を含める (既存 `read_reminds_for_tasks` を再利用)

**受け入れ条件**:
- SQLite 空 → `{ tasks: [], serverTime: "..." }`
- タスク 3 件登録後 → 3 件返る、reminds が埋め込まれている、project_name が JOIN される
- `?status=done` で status フィルタが効く
- `?since=2026-04-15` で `updated > 2026-04-15` のみ返る
- `?project=home` でプロジェクト一致のみ
- `?limit=2` で先頭 2 件
- 不正な `status` 値 → 400
- 不正な `since` 値 (parse 失敗) → 400

**検証手順**:
1. `cargo test http::tasks::list` (in-memory SQLite + `Router::oneshot`)
2. `my-task add "…"` で実 DB に書いてから `curl -H "Authorization: Bearer …" localhost:3333/api/tasks`

---

### T4. `GET /api/tasks/:task_number`

**目的**: 単一取得 + 404。

**変更**:
- `src/sqlite.rs` に `read_task_by_id(conn, id) -> Option<Task>` 追加
- `src/http/tasks.rs` に `get_task` ハンドラ
- ルート追加: `.route("/api/tasks/:task_number", get(tasks::get_task))`
- `TaskResponse { task: TaskDto, server_time: DateTime<Utc> }` を model.rs に追加

**受け入れ条件**:
- 存在する task_number → 200, `{ task, serverTime }`
- 存在しない task_number → 404, `{ "error": "not found" }`
- 非数値パス (`/api/tasks/abc`) → 400
- reminds が task に埋め込まれている

**検証手順**:
1. unit test 3 ケース (存在 / 不在 / 不正 path)
2. 手動 curl 確認

---

### T5. `POST /api/tasks`

**目的**: 新規作成 + rowid 採番 + reminds 挿入 + project 透過解決。

**変更**:
- `src/sqlite.rs` に `replace_reminds(conn, task_id, &[NaiveDate])` 追加 (DELETE + INSERT をひとまとめ)
- `src/http/tasks.rs` に `create_task` ハンドラ
- ルート追加: `.route("/api/tasks", get(list).post(create))`
- 処理: `unchecked_transaction` → `resolve_project` → `insert_task_row(..., None)` → reminds INSERT → commit → `read_task_by_id` で再読み込みして返す
- `task_number` を body で受け取ったら `400 Bad Request` (サーバー採番の約束を守る)

**受け入れ条件**:
- body 最小構成 (title / status / source / createdAt / updatedAt + `reminds: []`) で 201 + 採番された task_number
- `projectName: "new-proj"` を含めると projects テーブルに透過 INSERT
- reminds 指定時、task_reminds テーブルに n 行入る
- `taskNumber` を body に入れると 400
- 不正な status (CHECK 制約違反) → 400 (SQLite エラーを拾って 400 に変換)

**検証手順**:
1. unit test (空 DB に POST して rowid=1 が返る)
2. CLI `my-task ls` で新規行が見えることを手動確認

---

### T6. `PATCH /api/tasks/:task_number`

**目的**: 部分更新。reminds は送ったときだけ全置換。

**変更**:
- `src/model.rs` に `TaskPatchDto` (全フィールド `Option<T>`) を追加
- `src/sqlite.rs` に `update_task_partial(conn, id, patch) -> Option<Task>` 追加
  - 内部: `read_task_by_id` → 存在しなければ None → 存在すればフィールドマージ → `update_task_row`
  - reminds は `Option<Vec<NaiveDate>>`。`Some(_)` のときだけ `replace_reminds` を呼ぶ
- `src/http/tasks.rs` に `patch_task` ハンドラ
- ルート追加: `.route("/api/tasks/:task_number", get(get_task).patch(patch_task))`

**受け入れ条件**:
- `{"title": "updated"}` のみ送る → title だけ上書き、他は保持
- `{"reminds": ["2026-05-01"]}` → reminds 全置換
- reminds 非送信 → 既存 reminds 保持
- 存在しない task_number → 404
- `taskNumber` を body に入れると 400 (URL と不一致の罠を避ける)

**検証手順**:
1. unit test (部分フィールド / reminds 置換 / reminds 非送信)
2. curl で `-X PATCH -d '{"status":"done"}'` を流して CLI で確認

---

### T7. `GET /api/projects`

**目的**: projects 一覧。

**変更**:
- `src/sqlite.rs` に `read_projects(conn) -> Vec<Project>`
- `src/model.rs` に `Project { id, name }` / `ProjectListResponse`
- `src/http/projects.rs` (新規): `list_projects`
- ルート追加: `.route("/api/projects", get(projects::list_projects))`

**受け入れ条件**:
- 空 → `{ projects: [], serverTime }`
- n 件 → n 件返る
- 順序は `id ASC` (挿入順)

**検証手順**:
1. unit test
2. curl 確認

---

### T8. ローカル my-own 結合テスト (checkpoint)

**目的**: `docs/SERVER_DESIGN.md` § 動作確認手順 を実機で通す。自動テスト化は本タスクでは不要 (手動 checklist)。

**受け入れ条件** (全項目チェック):
- [ ] `cargo run --release` で `:3333` に bind
- [ ] my-own (`npm run dev`) の環境変数を my-task-sync に向けて起動
- [ ] CLI で `my-task add "foo"` → my-own UI で即時表示
- [ ] my-own UI で新規タスク作成 → `my-task ls` で表示
- [ ] 両側で同一タスクの title を更新 → 後に書いた方が残る
- [ ] CLI で `my-task done <n>` → my-own UI で status=done 反映
- [ ] my-own UI で project 新規指定 → `my-task projects` に現れる
- [ ] サーバー Ctrl-C → graceful shutdown (進行中 HTTP が 502 にならない)
- [ ] 認証トークン間違い → my-own UI で 401 エラー表示 (or ログ)

**成果物**: `tasks/integration-check-YYYY-MM-DD.md` に checklist 結果を記録 (Optional だが推奨)。

---

## チェックポイント

各チェックポイントで止まり、次に進むかレビューする。

| CP      | 位置    | ゲート                                                      |
| ------- | ----- | -------------------------------------------------------- |
| **CP1** | T1 完了 | 旧 daemon コード全撤去、axum `/healthz` で boot、graceful shutdown |
| **CP2** | T2 完了 | 認証 middleware が 401 / 通過の両方を返す                           |
| **CP3** | T5 完了 | 初の write 経路 (POST) が動く — schema 設計が妥当か中間検査               |
| **CP4** | T7 完了 | 5 エンドポイント全部のハンドラ単体テスト緑                                   |
| **CP5** | T8 完了 | ローカル my-own と結合成功、Phase 1 PR マージ可                        |

## リスクと緩和策

| リスク | 緩和策 |
|------|-------|
| my-task CLI との SQLite 同時書き込みで BUSY | 既存 `busy_timeout=5s` を維持。サーバーは `Arc<Mutex<Connection>>` で書き込みを直列化 |
| DTO 名前変更で my-own 側の既存コードが壊れる | フィールド名 (camelCase) は温存。型名変更は Rust 側のみ |
| axum 0.7 → 0.8 の破壊的変更 | Cargo.lock 固定、`axum = "=0.7.x"` 相当の運用。Phase 1 で 0.8 へは上げない |
| 認証 middleware の順序ミスで `/api/*` が素通り | T2 の単体テストで 401 を明示 assert、T3 以降の各テストでも認証あり/なしを両方通す |
| PATCH の部分更新でフィールドを誤って null 化 | `Option<Option<T>>` で「送らない」と「null 送る」を区別するのではなく、null 送信を `Some(None)` とせず常に「送らない = 変更しない」扱いに統一 (serde `#[serde(default, skip_serializing_if = "Option::is_none")]` で対応) |
| `launchctl` 下で SIGTERM を受けた際 in-flight が落ちる | `axum::serve::with_graceful_shutdown` + `KeepAlive` プラストでタイムアウト確保 |

## Phase 2 スケッチ (着手は Phase 1 マージ後)

- **T9** ngrok child spawn + NgrokGuard (Drop で `start_kill`)
- **T10** shutdown flow 拡張 (HTTP drain → `child.kill().await` → guard drop)
- **T11** `/api/status` 実装 (localhost:4040/api/tunnels を reqwest で取得 → 集約 JSON)
- **T12** `[ngrok]` 設定セクション / 環境変数 / `config.example.toml` 更新
- **T13** README の Install セクションに ngrok authtoken 設定手順を追記
- **CP6** Vercel 上 my-own から公開 URL 経由で Phase 1 と同じ結合テストが通る

## 見積もり (参考)

- T1: 2-3h (削除が主で慎重に)
- T2: 1h
- T3: 1.5h (クエリ絞り込みの SQL が最も手が混む)
- T4-T7: 各 0.5-1h
- T8: 1h (手動確認)
- **Phase 1 合計**: 半日〜1 日

Phase 2 は ngrok の検証込みで 3-4h 想定。

## 未決事項 (プラン段階でクローズしない)

1. `docs/OVERVIEW.md` を DEPRECATED マークするか削除するか
2. `config.example.toml` を Phase 1 完了時点で `[server]` 向けに書き換えるか (Phase 2 で `[ngrok]` 追加するので二度手間を避けたい)
3. 認証失敗ログを info レベルで出すか (総当たり検知用) — Phase 1 はデフォルトで出さず、Phase 2 以降で要検討
