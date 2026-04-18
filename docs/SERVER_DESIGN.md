# my-task-sync v2: バックエンドサーバー化設計

> 本ドキュメントは `docs/OVERVIEW.md` (v1: polling daemon) を置き換える新設計。
> 旧設計の実装は `main` ブランチに残る。

## 移行の動機

v1 は「my-task-sync (client) → my-own (server)」の polling daemon 構成。v2 で **方向を反転** し、my-task-sync を HTTP サーバーとして常駐させる。理由:

- **真実の源を単一化**: SQLite (my-task 側) を一次ソースにし、Neon の `tasks` 系テーブルはキャッシュまたは廃止へ。`task_number` = SQLite rowid の不変量がよりシンプルに強制できる。
- **ポーリング廃止**: 30 秒間隔の差分吸い上げが消え、my-own からの即時反映になる。`last_push_at` / `last_pull_at` の state 管理も不要。
- **書き込み経路の一本化**: 旧設計の 3 ステップ (push / pull_unsynced / pull_updates) が薄い REST CRUD に畳まれる。

## アーキテクチャ

### Before (v1)

```
my-task (CLI) ──writes──► SQLite ◄──reads──── my-task-sync daemon
                                                │  ▲
                                                │  │
                                     30s polling│  │state.db
                                                ▼  │
                                         my-own (Vercel) ──► Neon
```

### After (v2)

```
my-task (CLI) ──writes──► SQLite ◄──reads/writes── my-task-sync server
                                                   (axum, :{port})
                                                          ▲
                                                          │ Bearer auth
                                    ┌─────────────────────┤
                                    │                     │
                           ngrok tunnel (phase 2)          │ (ローカル統合テスト時は直接)
                                    │                     │
                                    ▼                     │
                              my-own (Vercel) ────────────┘
                                    │
                                    ▼
                                  Neon (tasks 以外)
```

## フェーズ分割

| フェーズ    | 範囲                                                     | 検証ゴール                                                            |
| ----------- | -------------------------------------------------------- | --------------------------------------------------------------------- |
| **Phase 1** | REST サーバー骨格 + `/api/tasks` + `/api/projects`       | ローカル my-own と直結し、タスク CRUD が双方向に動くことを手動確認    |
| **Phase 2** | ngrok サブプロセス自動起動 + Drop ガード + `/api/status` | Vercel 上の my-own から公開 URL 経由で Phase 1 と同等に動くことを確認 |

Phase 間で PR を分割する。Phase 1 が動かないまま Phase 2 を載せない。

---

## Phase 1: REST サーバー

### 設定 (`config.toml`)

```toml
[sqlite]
# path = "/custom/path/tasks.db"   # 省略時: $XDG_DATA_HOME/my-task/tasks.db

[server]
port = 3333                        # bind port (v2 新設)
api_key = "your-api-key-here"      # Bearer 認証の共有秘密

# [ngrok] は Phase 2 で追加
```

環境変数オーバーライド:
| 変数 | 上書き対象 |
|------|-----------|
| `MY_TASK_SYNC_API_KEY` | `[server].api_key` |
| `MY_TASK_SYNC_PORT` | `[server].port` |
| `MY_TASK_DATA_FILE` | `[sqlite].path` |
| `RUST_LOG` | tracing フィルタ |

**廃止**: `[api].base_url` / `[sync].interval_seconds` / `MY_TASK_SYNC_BASE_URL`。

### API サーフェス

5 エンドポイント。全て `Authorization: Bearer <api_key>` を middleware で強制。未認証は `401 Unauthorized` を即返す (retry 余地なし)。

```
GET    /api/tasks?status=&since=&project=&limit=   → 200 { tasks, serverTime }
GET    /api/tasks/:task_number                      → 200 { task, serverTime } | 404
POST   /api/tasks                                   → 201 { task, serverTime }
PATCH  /api/tasks/:task_number                      → 200 { task, serverTime } | 404
GET    /api/projects                                → 200 { projects, serverTime }
```

#### `GET /api/tasks`

クエリパラメータ (すべて任意):

- `status` — `open` / `done` / `closed` のいずれか。未指定なら全 status
- `since` — `YYYY-MM-DD`。`tasks.updated > since` のみ返す (差分取得用)
- `project` — プロジェクト名完全一致
- `limit` — 返す件数上限 (未指定: 全件)

レスポンス:

```json
{
  "tasks": [ { "taskNumber": 1, "title": "…", … , "reminds": ["2026-04-20"] } ],
  "serverTime": "2026-04-18T10:30:00Z"
}
```

`serverTime` はクライアントが次回 `since` として使う値。サーバー時計を基準にする (v1 と同じ)。

#### `GET /api/tasks/:task_number`

単一取得。存在しなければ `404 Not Found`。

#### `POST /api/tasks`

新規作成。body から `taskNumber` は受け取らない — **SQLite の rowid がサーバー採番される**。

リクエスト body:

```json
{
  "title": "…",
  "status": "open",
  "source": "web",
  "projectName": "home",
  "due": "2026-05-01",
  "doneAt": null,
  "important": false,
  "createdAt": "2026-04-18T10:30:00Z",
  "updatedAt": "2026-04-18T10:30:00Z",
  "reminds": ["2026-04-20"]
}
```

レスポンス (`201 Created`):

```json
{ "task": { "taskNumber": 42, "title": "…", … }, "serverTime": "…" }
```

プロジェクト名が `projects` テーブルに無ければ透過的に `INSERT OR IGNORE` される (v1 の `resolve_project` と同じ)。

#### `PATCH /api/tasks/:task_number`

部分更新。送信されたフィールドのみ上書き。

- **LWW / 楽観ロックは Phase 1 では入れない**。単一ユーザーで競合は稀。必要なら後付けで `If-Match: <updatedAt>` を導入。
- `reminds` を送った場合のみ `task_reminds` を **全置換** (v1 と同じ挙動)。送らなければ既存 remind はそのまま。
- 存在しない `task_number` は `404`。

body は `POST` と同一スキーマだが全フィールド optional。

#### `GET /api/projects`

```json
{
  "projects": [
    { "id": 1, "name": "home" },
    { "id": 2, "name": "work" }
  ],
  "serverTime": "…"
}
```

新規作成エンドポイントは **不要** (タスク作成時に透過解決される)。

### 非機能要件

- **Graceful shutdown**: SIGTERM / SIGINT で HTTP サーバーを drain (`axum::serve::with_graceful_shutdown`)。進行中リクエストは完了させる。
- **ロギング**: 全リクエストを `tower_http::trace::TraceLayer` で記録。4xx/5xx は警告以上で出す。
- **エラー**: 既存の `Error` 列挙を再利用し、HTTP レスポンスへの変換レイヤー (`IntoResponse`) を足す。`Error::Sqlite` → 500, `Error::Config`/bad body → 400, 不在リソース → 404。
- **SQLite 同時アクセス**: my-task CLI と同じ DB を読み書きする。WAL モード前提で `busy_timeout` を維持。サーバーは `Arc<Mutex<Connection>>` (書き込み単一化) から始め、性能問題が出たら `r2d2` コネクションプールへ昇格。

### 廃止するコード

Phase 1 の PR でまとめて削除:

- `src/api_client.rs` (SyncApi トレイト + HttpApiClient)
- `src/sync_engine.rs` (push / pull_unsynced / pull_updates / sync_cycle)
- `src/sync_state.rs` (state.db 管理)
- `src/main.rs` の polling ループと `--once` / `--dry-run` フラグ
- `tests/sync_engine_test.rs` / `tests/sync_state_test.rs`
- `config.rs` の `[api]` / `[sync]` セクションと関連環境変数処理
- `~/.config/my-task-sync/state.db` (マイグレーション不要 — 削除してよい)

### ディレクトリ構成 (Phase 1 後)

```
src/
├── main.rs          # axum サーバー起動 + graceful shutdown
├── config.rs        # TOML + env (server / sqlite のみ)
├── error.rs         # 既存 + IntoResponse 実装
├── model.rs         # 既存 DTO を流用 (SyncTask → TaskDto に改名)
├── sqlite.rs        # 既存 (Arc<Mutex<Connection>> 向けに微調整)
└── http/
    ├── mod.rs       # Router 組み立て
    ├── auth.rs      # Bearer 認証 middleware
    ├── tasks.rs     # /api/tasks ハンドラ 4 本
    └── projects.rs  # /api/projects ハンドラ
```

### 動作確認手順

1. `cargo run` でローカル :3333 に起動
2. ローカル my-own (`npm run dev`) を my-task-sync に向ける環境変数を設定
3. CLI で `my-task add "…"` → my-own の UI で即時表示を確認
4. my-own の UI で新規作成 → `my-task ls` で表示を確認
5. 両側で更新 → 最後に書いた方の値が残ることを確認

---

## Phase 2: ngrok 子プロセス管理 + /api/status

### 設定追加

```toml
[ngrok]
domain = "unedified-carrie-nondiathermanous.ngrok-free.dev"  # ngrok 予約ドメイン
# 未設定なら ngrok を起動しない (ローカル開発 / テスト用)
```

### ngrok 起動

- `tokio::process::Command::new("ngrok").args(["http", &port, "--domain", &domain])`
- stdout/stderr は `/tmp/my-task-sync-ngrok.{out,err}.log` にリダイレクト
- サーバーが bind 成功後に spawn (順序逆だとトンネル先が無い)
- ngrok バイナリが PATH に無ければ起動時に `Error::Config` で fail-fast

### Drop ガード

```rust
struct NgrokGuard {
    child: Option<tokio::process::Child>,
}

impl Drop for NgrokGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.start_kill();  // best-effort, async wait は Drop では呼べない
        }
    }
}
```

- `?` 早期 return / panic / `ctrl_c` 受信 / `launchctl` の SIGTERM すべてで子プロセスが殺されることを保証
- graceful shutdown の流れ: HTTP サーバー drain → 明示的に `child.kill().await` で完了待ち → 最後に guard drop (二重 kill は no-op)

### `/api/status` エンドポイント

認証 **不要** (運用確認用; センシティブ情報を返さない)。

実装:

1. `reqwest::get("http://localhost:4040/api/tunnels")` で ngrok admin API を叩く
2. 最初の tunnel を抽出 (`--domain` で 1 本に固定しているので single tunnel 前提)
3. 必要なフィールドを抽出して集約 JSON を返す

レスポンス (正常時):

```json
{
  "server": {
    "version": "0.2.0",
    "uptimeSeconds": 12345,
    "sqlite": { "path": "/Users/…/tasks.db", "ok": true }
  },
  "ngrok": {
    "enabled": true,
    "reachable": true,
    "publicUrl": "https://unedified-carrie-nondiathermanous.ngrok-free.dev",
    "forwardingTo": "http://localhost:3333",
    "httpRequestsTotal": 56,
    "httpRequestsPerMinute": 49.5,
    "connectionsTotal": 50
  }
}
```

- `httpRequestsPerMinute` = `metrics.http.rate1 * 60` (ngrok の `rate1` は 1 分平均の秒レート)
- ngrok 無効時: `"ngrok": { "enabled": false }` のみ
- `/api/tunnels` 到達失敗: `"ngrok": { "enabled": true, "reachable": false, "error": "<reqwest エラー>" }`
- `sqlite.ok` は `SELECT 1` で判定

---

## 未決事項 (後続フェーズで詰める)

| 論点                       | 選択肢                                                                                    | 保留理由                                    |
| -------------------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------- |
| Neon の `tasks` 系テーブル | (a) 廃止 / (b) my-own が read-through キャッシュとして保持 / (c) 書き込みも Neon に二重化 | オフライン時の my-own UI をどう見せるか次第 |
| my-own のオフライン挙動    | (a) 「daemon unreachable」表示 / (b) Neon キャッシュから読み取り                          | Neon の扱いと一体                           |
| ngrok 以外のトンネル手段   | cloudflared / tailscale funnel に切り替えるか                                             | 無料枠・ドメイン固定・起動安定性の比較待ち  |
| LaunchAgent の KeepAlive   | 現状どおり / 明示 `ThrottleInterval` 設定                                                 | Phase 2 の ngrok 再起動頻度を見てから       |

これらは Phase 1/2 の完了後に別 issue で議論する。
