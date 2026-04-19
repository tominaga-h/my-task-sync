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

---

# Phase 2 実装プラン: ngrok 自動起動 + /api/status

> **前提**: Phase 1 完了 (T8 までマージ済み)。my-own 側の統合は
> `docs/MY_OWN_INTEGRATION.md` 通りに実装済みでローカル結合テスト合格。

## ゴール

my-task-sync 起動時に ngrok サブプロセスを自動起動し、予約ドメインで
Vercel 上の my-own から到達可能にする。運用時の到達性確認用に
`/api/status` (認証なし) を生やす。launchctl の再起動サイクルと
SIGTERM で ngrok 子プロセスが確実に後片付けされること。

## スコープ (In)

- `[ngrok].domain` 設定 + 起動時の ngrok 子プロセス起動
- `NgrokGuard` (Drop で子プロセスを確実に殺す)
- graceful shutdown への統合 (HTTP drain → ngrok kill → exit)
- `GET /api/status` (認証なし、`server` / `ngrok` を集約 JSON で返す)
- ngrok バイナリ不在時の fail-fast
- `docs/API.md` / README 両言語版 / `config.example.toml` 更新
- Vercel 上 my-own からの結合テスト (手動 checklist)

## スコープ (Out)

- ngrok 以外のトンネル (cloudflared / tailscale funnel 等)
- ngrok authtoken のコード側管理 (ngrok バイナリにまかせる)
- ngrok admin ポート (`4040`) 以外のカスタマイズ
- Prometheus 形式メトリクス (`/api/status` の JSON で足りる範囲で完結)
- Phase 3 以降の構想 (楽観ロック / マルチユーザー)

## 依存グラフ

```
T9 (config + spawn + Drop guard)
 └─► T10 (shutdown integration)
      └─► T11 (/api/status endpoint)
           └─► T12 (docs 更新)
                └─► T13 (Vercel 結合テスト = CP6)
```

T9→T10 は同じ `NgrokGuard` 型を共有するので順に進める。T11 は T9 が終わ
れば着手可 (Ngrok 稼働中に curl で /api/tunnels を叩ける状態なら独立)。

---

### T9. ngrok 設定 + 子プロセス spawn + Drop ガード

**目的**: `[ngrok].domain` を設定した状態で my-task-sync を起動すると、
ngrok サブプロセスが自動で立ち上がり、HTTP サーバーの寿命と同期する
状態を作る。

**変更**:
- `Cargo.toml`: 依存追加なし (tokio::process は既に tokio full で有効)
- `src/config.rs`:
  - `ResolvedConfig` に `ngrok: NgrokConfig` 追加
  - `NgrokConfig { domain: Option<String> }` (None なら spawn しない)
  - **env 限定**: `resolve()` で `NGROK_DOMAIN` を読み込む。未設定 or 空文字なら None
  - config.toml 側には `[ngrok]` セクションを **作らない** (ドメインは
    deployment ごとの固定値なので env で管理する方が適切 — config file に
    書くとうっかり public repo にコミットするリスクあり)
- `src/ngrok.rs` (新規):
  - `struct NgrokGuard { child: Option<tokio::process::Child> }`
  - `impl Drop for NgrokGuard { fn drop(&mut self) { let _ = child.start_kill(); } }`
  - `pub async fn spawn(domain: &str, port: u16) -> Result<NgrokGuard, Error>`
    - `tokio::process::Command::new("ngrok")`
    - `.args(["http", &port.to_string(), "--domain", domain])`
    - stdout/stderr を `/tmp/my-task-sync-ngrok.{out,err}.log` にリダイレクト
    - ngrok バイナリ不在 (`io::ErrorKind::NotFound`) → `Error::Config` で fail-fast
    - `Child::id()` を tracing::info! でログ
- `src/lib.rs`: `pub mod ngrok;` 追加
- `src/main.rs::run()`:
  - サーバー bind 成功後・serve 開始前に `ngrok::spawn` 呼び出し (domain が Some のとき)
  - 戻り値 `NgrokGuard` を let で保持して serve のスコープ内で生存させる
  - 未設定時は guard = None でスキップ
- `config.example.toml`: `[ngrok]` セクション追加
- `tests/common/` にモックは書かない (実 ngrok に依存しないよう Drop /
  spawn のエラーパスを集中テスト)

**受け入れ条件**:
- `NGROK_DOMAIN` 未設定で `cargo run` → サーバー起動、ngrok 起動なし (ログに `ngrok disabled (NGROK_DOMAIN not set)` 等)
- `NGROK_DOMAIN=x.ngrok-free.dev cargo run` → ngrok バイナリが PATH にあれば child 起動、PID がログ出力
- ngrok バイナリ不在 → 起動時 exit(1) + 明示的エラーメッセージ
- `cargo run` → Ctrl-C (T10 前の簡易確認) で **ngrok プロセスも止まる** (ps で残存しない)
- `NGROK_DOMAIN=""` (空文字) は未設定と同じ扱い → ngrok 起動なし
- unit tests: NgrokGuard::drop が 2 回呼ばれても安全 (再入可能) / Error::Config メッセージに "ngrok" を含む

**検証手順**:
1. `cargo build --release`
2. `ngrok` が PATH にあること (`which ngrok`)
3. ngrok authtoken 設定済みであること (`ngrok config check`)
4. `NGROK_DOMAIN=<domain>.ngrok-free.dev MY_TASK_SYNC_API_KEY=... ./target/release/my-task-sync` → ログに "listening" + "ngrok started pid=<N>" が両方出る
5. `curl https://<domain>.ngrok-free.dev/healthz` → 200 "ok"
6. Ctrl-C → ngrok プロセスが ps に残っていないこと

**懸念・リスク**:
- ngrok 無料プランは同一 authtoken で 1 tunnel 制限 → 二重起動すると conflict。launchctl KeepAlive で突貫再起動する時に衝突する可能性。対策: `T9` では愚直に child spawn し、`T10` の shutdown で確実に kill する
- authtoken が ngrok バイナリの config (`~/.ngrok2/ngrok.yml`) に保存されている前提。T12 (docs) で手順を README に追記

---

### T10. graceful shutdown への統合

**目的**: SIGINT/SIGTERM 受信時、HTTP drain 完了を待ってから ngrok child
に明示的 kill を投げ、Drop guard はあくまで "panic / 早期 return /
unwind" の保険として機能させる。

**変更**:
- `src/main.rs::run()`:
  - shutdown_signal を受けたら:
    1. HTTP serve の graceful drain を起動 (既存の `with_graceful_shutdown` 経由)
    2. drain 完了後 or `GRACEFUL_SHUTDOWN_SECS` タイムアウト後に、
       保持している `NgrokGuard::kill_and_wait()` を呼ぶ (新メソッド)
    3. `child.wait()` で reaper 処理を完了させ、ゾンビ回避
  - drop 順序: `NgrokGuard` → `TcpListener` / `Router` の順 (ngrok を先に殺すと
    Vercel 側のリクエストが即座にエラーになる。逆だと HTTP は生きてるのに
    外からは見えない期間ができる。現実的には serve drain が先で OK)
- `src/ngrok.rs`:
  - `impl NgrokGuard { pub async fn kill_and_wait(mut self) -> Result<(), Error> }` 追加
  - 内部で `child.kill().await` + `child.wait().await`
  - `self.child = None` にして Drop での再 kill を無害化

**受け入れ条件**:
- Ctrl-C → HTTP drain 完了 → ngrok kill → プロセス終了、の順でログ出力
- `kill_and_wait` 成功後に Drop が発火しても `start_kill` が再呼び出しされない (child = None)
- `launchctl unload` → ngrok プロセスも消える (ps で残存しない)
- panic 誘発テスト (cfg(test) の強制パニック) → Drop ガードが子を殺すこと
- `GRACEFUL_SHUTDOWN_SECS` 超過時でも ngrok kill は実行される

**検証手順**:
1. ローカルで起動 → ps で ngrok PID を控える → Ctrl-C → `ps <PID>` で消失確認
2. `kill -TERM <pid of my-task-sync>` → 同上
3. panic テスト (cargo test の `#[should_panic]`) で Drop 動作を pin

---

### T11. `GET /api/status` エンドポイント

**目的**: 認証なしで叩けて、server / ngrok の状態を集約 JSON で返す運用
エンドポイント。公開 URL が機能しているかを my-own デプロイ前に curl で
確認したい。

**変更**:
- `src/http/status.rs` (新規):
  - `pub async fn get_status(State<AppState>) -> Result<Json<StatusResponse>, Error>`
  - 処理:
    1. server セクションを組み立て (version は `env!("CARGO_PKG_VERSION")`, uptime は起動時刻を `AppState` に追加)
    2. SQLite に `SELECT 1` を発行して `sqlite.ok` を判定
    3. ngrok 設定済みなら `reqwest::get("http://localhost:4040/api/tunnels")` で `/api/tunnels` を叩き、最初の tunnel の `public_url` / `config.addr` / `metrics.http.{count,rate1}` / `metrics.conns.count` を抽出
    4. ngrok 未設定: `{ "enabled": false }` のみ
    5. ngrok 到達不能: `{ "enabled": true, "reachable": false, "error": "..." }`
- `src/http/mod.rs`:
  - `pub mod status;`
  - **認証 middleware の外側に** `/api/status` を配置するため、ネスト構造を見直す:
    ```
    Router::new()
      .route("/healthz", get(healthz))
      .route("/api/status", get(status::get_status))   // ← unauthenticated
      .nest("/api", api_with_auth)                      // ← Bearer 必須
    ```
    ただし `/api/status` が `nest("/api", ...)` のルート解決優先度に
    巻き込まれないか確認。もし問題なら `/status` (非 /api) に移す案も検討
- `src/model.rs` もしくは `src/http/status.rs` 内に DTO:
  - `StatusResponse { server: ServerStatus, ngrok: NgrokStatus }`
  - `ServerStatus { version, uptime_seconds, sqlite: SqliteStatus }`
  - `SqliteStatus { path, ok }`
  - `NgrokStatus` は enum ({Disabled, Unreachable{error}, Up{...}}) で JSON は `enabled` / `reachable` / metrics 等をフラットに並べる
- `AppState` に `started_at: Instant` を追加 (uptime 計算用)

**受け入れ条件**:
- `curl /api/status` (認証なし) → 200 JSON
- 認証不要ルート: Bearer ヘッダ無しで叩ける (middleware を跨がないこと)
- レスポンスの形 (ngrok 3 状態):
  - `{ "server": {...}, "ngrok": { "enabled": false } }`
  - `{ "server": {...}, "ngrok": { "enabled": true, "reachable": false, "error": "..." } }`
  - `{ "server": {...}, "ngrok": { "enabled": true, "reachable": true, "publicUrl": "...", "forwardingTo": "...", "httpRequestsTotal": ..., "httpRequestsPerMinute": ..., "connectionsTotal": ... } }`
- `rate1` (秒レート) * 60 = `httpRequestsPerMinute` として返す
- `serverTime` も含めるかは任意だが、他エンドポイントと揃える意味では入れる
- 単体テスト: mock ngrok admin (tests/support に簡易 server を建てる)、または reqwest::Client を差し替え可能にして 3 状態を pin

**検証手順**:
1. `cargo test http::status` (ngrok 到達成功 / 失敗 / 無効の 3 ケース)
2. 実 ngrok 稼働状態で `curl localhost:3333/api/status | jq` → 期待形
3. ngrok 停止状態で `curl ...` → `enabled: true, reachable: false`
4. `[ngrok].domain` 未設定状態で `curl ...` → `enabled: false`

---

### T12. docs 更新

**目的**: Phase 2 完了時点の仕様を README / API.md / config.example.toml /
SERVER_DESIGN.md に反映。

**変更**:
- `docs/API.md`:
  - エンドポイント表で `GET /api/status` を ✅ 実装済みに
  - 新規セクション: GET /api/status のレスポンス例 (3 状態すべて)
- `docs/SERVER_DESIGN.md`:
  - Phase 2 セクションを「実装済み」に更新
  - `/api/status` のレスポンス shape の記述を実装と揃える
- `README.md` / `docs/README_ja.md`:
  - Install セクションに ngrok authtoken セットアップ手順を追記:
    ```bash
    brew install ngrok    # 未インストールなら
    ngrok config add-authtoken <token>
    ngrok config check    # 動作確認
    ```
  - 環境変数表に `NGROK_DOMAIN` を追加 (未設定なら ngrok 無効と明記)
  - `com.my-task-sync.plist` の `EnvironmentVariables` に `NGROK_DOMAIN` を
    足すサンプルを提示 (launchctl 運用の人向け)
  - Manage セクションに "公開 URL 確認" として `curl localhost:3333/api/status` を追加
- `config.example.toml`: 触らない (ngrok 設定は env 限定のため)
- `docs/MY_OWN_INTEGRATION.md`:
  - Phase 2 セクションを更新 (ngrok URL への env 切り替え手順を確定 / `/api/status` で到達性確認できる旨)

**受け入れ条件**:
- README の手順だけで ngrok セットアップから my-task-sync 起動までできる
- `docs/API.md` の `/api/status` 節が 3 状態のレスポンス例を載せる
- `config.example.toml` をコピーしただけでは ngrok 起動しない (`domain = ""` or コメントアウト状態で出荷)

---

### T13. Vercel 上 my-own からの結合テスト = CP6

**目的**: ngrok 経由で Vercel 上の my-own から Phase 1 と同等の CRUD が
動くことを手動で確認 (Phase 2 の最終ゲート)。

**前提**: my-own 側が `docs/MY_OWN_INTEGRATION.md` 通りに実装済みで、
Vercel に本番デプロイされていること。

**受け入れ条件** (全項目チェック):
- [ ] my-task-sync をローカルで起動、ngrok トンネル稼働中
- [ ] `curl https://<domain>.ngrok-free.dev/healthz` → 200
- [ ] `curl https://<domain>.ngrok-free.dev/api/status | jq '.ngrok.reachable'` → `true`
- [ ] Vercel my-own の env を更新 (`MY_TASK_SYNC_BASE_URL=https://<domain>.ngrok-free.dev`)
- [ ] my-own 本番 URL (Vercel) → タスク一覧が表示される
- [ ] my-own で新規タスク作成 → 反映される (`my-task ls` で確認)
- [ ] CLI で `my-task add` → my-own で表示
- [ ] my-task-sync を Ctrl-C → my-own が 502 エラー表示 (期待通り)
- [ ] my-task-sync 再起動 → ngrok 自動起動 → my-own が回復

**成果物**: `tasks/phase2-integration-check-YYYY-MM-DD.md` に checklist 結果を記録。

---

## Phase 2 チェックポイント

| CP | 位置 | ゲート |
|----|------|-------|
| **CP6** | T13 完了 | ngrok 経由で Vercel → my-task-sync が動く。Phase 1 と同等の CRUD が public URL 経由で通る |

## Phase 2 見積もり (参考)

- T9: 2-3h (ngrok spawn + Drop guard + エラー経路)
- T10: 1h (shutdown 統合 + テスト)
- T11: 2h (status 集約 + mock ngrok の tests 設計)
- T12: 1h (docs 更新)
- T13: 1h (手動結合テスト)
- **Phase 2 合計**: 半日〜1 日

## Phase 2 リスクと緩和策

| リスク | 緩和策 |
|------|-------|
| ngrok 孤児プロセスが残り、二重起動で authtoken 衝突 | Drop ガード + 明示 kill の二段構え。T10 で panic 時の動作を pin |
| ngrok バイナリが PATH にない環境で黙って起動 | `spawn()` で `io::ErrorKind::NotFound` を即座に `Error::Config` にマップ、その旨をメッセージに入れる |
| ngrok admin API (:4040) の port が他プロセスと競合 | `/api/status` の reqwest を short timeout (2s) にして、到達不能状態を正常フローで扱う |
| `/api/status` が認証なしで metrics を公開し情報漏洩 | public_url と forwardingTo は既に URL に出ており機密性なし。metrics も数値だけ。api_key は絶対に漏らさない (Debug に注意 — 既に S2 で redact 検討済) |
| ngrok 無料プランの同時 1 tunnel 制限 | launchctl `ThrottleInterval` を設定 (30s 以上推奨) して再起動時の衝突を減らす |

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
