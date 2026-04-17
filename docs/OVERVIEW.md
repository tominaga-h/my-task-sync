# Phase 4: my-task-sync 実装プラン

> 新規リポジトリ `my-task-sync` として作成する、macOS ローカル常駐デーモン。
> my-task (SQLite) と my-own (Neon) の双方向同期を行う。

## 関連リポジトリ

### my-own（Web アプリ）

- **リポジトリ**: `~/lab/typescript/REACT/my-own`
- **概要**: 個人の情報を一元管理する統合 Web アプリケーション。Slack の自分宛 DM からメモとリンクを自動収集し、タスク・メモ・リンクを相互に紐付けて管理する
- **技術スタック**: Next.js 15 (App Router) / React 19 / TypeScript / Tailwind CSS v4 / Neon Serverless Postgres / Drizzle ORM
- **ホスティング**: Vercel
- **認証**: `Authorization: Bearer API_KEY` による簡易トークン認証（`APP_USER_ID` 固定の単一ユーザー）
- **DB**: Neon (PostgreSQL) — `tasks`, `task_reminds`, `projects`, `notes`, `links` 等のテーブル
- **sync 用 API** (Phase 4 で追加): `/api/sync/tasks/*` 名前空間。詳細は `docs/PLAN_PHASE4_MY_OWN.md` を参照

### my-task（Rust CLI）

- **リポジトリ**: `~/lab/rust/my-task`
- **概要**: ターミナルベースのタスク管理 CLI ツール
- **技術スタック**: Rust / clap 4 / rusqlite 0.31 (bundled) / chrono
- **DB**: SQLite (`~/Library/Application Support/my-task/tasks.db`)
- **SQLite スキーマ**:
  - `projects`: `id` INTEGER PK, `name` TEXT UNIQUE
  - `tasks`: `id` INTEGER PK, `title` TEXT, `status` TEXT (open/done/closed), `source` TEXT DEFAULT 'private', `project_id` INTEGER FK, `due` TEXT, `done_at` TEXT, `created` TEXT, `updated` TEXT, `important` INTEGER DEFAULT 0
  - `task_reminds`: `id` INTEGER PK, `task_id` INTEGER FK, `remind_at` TEXT
- **日付形式**: `created`, `updated`, `due`, `done_at`, `remind_at` は TEXT 型の `YYYY-MM-DD` 形式
- **DB パス解決**: 環境変数 `MY_TASK_DATA_FILE` → `dirs::data_dir()/my-task/tasks.db`
- **主要コマンド**: `add`, `done`, `close`, `edit`, `list` (`ls`), `show`, `search`, `projects`, `notify`

## 設計方針（決定事項）

| 項目 | 決定 |
|------|------|
| 言語 | **Rust** — my-task の `rusqlite` コードを再利用、単一バイナリ配布 |
| 実行形態 | macOS の **launchctl** で常駐（LaunchAgent） |
| 同期方式 | **ポーリング**（30秒間隔、設定で変更可能） |
| task_number の採番 | **SQLite が唯一の採番元**。SQLite の `id` = `task_number` |
| 競合解決 | `updated_at` / `updated` ベースの **Last-Write-Wins (LWW)**。行単位 |
| 同期状態の保存 | my-task の SQLite とは別ファイル (`state.db`) で管理 |

## アーキテクチャ

```
┌─────────────┐         ┌──────────────────────────┐         ┌─────────────┐
│  my-task    │         │     my-task-sync          │         │   my-own    │
│  (Rust CLI) │         │  (ローカル常駐デーモン)     │         │  (Vercel)   │
│             │         │                          │         │             │
│  SQLite ◄───┼────────►│  sync engine             │◄────────►│  REST API   │
│  (読み書き)  │ 直接読み │                          │  HTTPS   │  (Neon DB)  │
│             │  書き   │  state.db                │          │             │
└─────────────┘         │  ├─ last_push_at         │          └─────────────┘
                        │  └─ last_pull_at         │
                        └──────────────────────────┘
                         launchctl (LaunchAgent)
```

### データフロー

```
[Push: SQLite → Neon]
  SQLite tasks WHERE updated > last_push_at
    → POST /api/sync/tasks/push
    → Neon に upsert（LWW 判定はサーバー側）

[Pull - 採番: Neon → SQLite → Neon]
  GET /api/sync/tasks/unsynced (task_number IS NULL)
    → SQLite に INSERT → id 自動採番
    → PATCH /api/sync/tasks/:id/number { taskNumber: sqlite.id }

[Pull - 更新: Neon → SQLite]
  GET /api/sync/tasks/changes?since=last_pull_at
    → SQLite の対応レコードを updated 比較で LWW
```

## リポジトリ構成

```
my-task-sync/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs           # エントリ: 設定読み込み → sync ループ → シグナルハンドリング
│   ├── config.rs          # TOML 設定読み込み
│   ├── sqlite.rs          # my-task の SQLite 読み書き
│   ├── api_client.rs      # my-own API クライアント (reqwest)
│   ├── sync_engine.rs     # 同期ロジック本体
│   ├── sync_state.rs      # state.db の管理 (last_push_at, last_pull_at)
│   ├── model.rs           # Task / Project / Remind 型定義
│   └── error.rs           # エラー型
├── config.example.toml
└── com.my-task-sync.plist  # launchctl 用
```

## 依存クレート

```toml
[dependencies]
rusqlite = { version = "0.31", features = ["bundled"] }
reqwest = { version = "0.12", features = ["json", "rustls-tls"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
toml = "0.8"
dirs = "6"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
ctrlc = "3"
```

## 設定

```toml
# ~/.config/my-task-sync/config.toml

[sqlite]
# my-task の SQLite パス (省略時: ~/Library/Application Support/my-task/tasks.db)
# path = "/custom/path/tasks.db"

[api]
base_url = "https://my-own-app.vercel.app"
api_key = "your-api-key-here"

[sync]
interval_seconds = 30
```

### 設定の解決順序

1. `~/.config/my-task-sync/config.toml`
2. 環境変数 (`MY_TASK_SYNC_API_KEY`, `MY_TASK_SYNC_BASE_URL` など)
3. CLI 引数 (`--config /path/to/config.toml`)

SQLite パスは my-task と同じロジック:
1. 環境変数 `MY_TASK_DATA_FILE`
2. `dirs::data_dir()/my-task/tasks.db`

## sync engine 詳細

### メインループ

```
fn main():
  config = load_config()
  state = SyncState::open("~/.config/my-task-sync/state.db")
  sqlite = open_my_task_sqlite(config.sqlite.path)
  client = ApiClient::new(config.api)

  loop:
    match sync_cycle(&sqlite, &client, &mut state):
      Ok(report) => log_report(report)
      Err(e) => log_error(e)  // 失敗してもループは継続
    
    sleep(config.sync.interval_seconds)
```

### sync_cycle の各ステップ

#### Step 1: Push (SQLite → Neon)

```rust
fn push_tasks(sqlite: &Connection, client: &ApiClient, state: &SyncState) -> Result<PushReport>:
  let last_push = state.get("last_push_at")  // ISO 8601 文字列 or epoch
  
  // my-task の updated は NaiveDate (日付精度) なので、日付文字列で比較
  let changed_tasks = sqlite.query(
    "SELECT t.*, p.name as project_name FROM tasks t
     LEFT JOIN projects p ON t.project_id = p.id
     WHERE t.updated > ?", [last_push]
  )
  
  let changed_reminds = sqlite.query(
    "SELECT * FROM task_reminds WHERE task_id IN (?)", [task_ids]
  )
  
  // 一括 push
  let response = client.post("/api/sync/tasks/push", {
    tasks: changed_tasks.map(|t| SyncTask {
      task_number: t.id,    // SQLite id = task_number
      title: t.title,
      status: t.status,
      source: t.source,
      project_name: t.project_name,
      due: t.due,
      done_at: t.done_at,
      important: t.important,
      updated_at: t.updated,   // NaiveDate → ISO 文字列
      created_at: t.created,
      reminds: reminds_for(t.id),
    })
  })
  
  state.set("last_push_at", now())
  return report
```

#### Step 2: Pull Unsynced (Web 作成タスクの採番)

```rust
fn pull_unsynced(sqlite: &Connection, client: &ApiClient) -> Result<PullUnsyncedReport>:
  let unsynced = client.get("/api/sync/tasks/unsynced")
  
  for task in unsynced.tasks:
    // SQLite に INSERT → autoincrement で id が振られる
    let project_id = resolve_project(sqlite, task.project_name)
    let sqlite_id = sqlite.execute(
      "INSERT INTO tasks (title, status, source, project_id, due, done_at, created, updated, important)
       VALUES (?, ?, 'web', ?, ?, ?, ?, ?, ?)",
      [task.title, task.status, project_id, task.due, task.done_at, task.created_at, task.updated_at, task.important]
    ).last_insert_rowid()
    
    // Neon に task_number を書き戻す
    client.patch(f"/api/sync/tasks/{task.neon_id}/number", {
      taskNumber: sqlite_id
    })
    
    // reminds も同期
    for remind in task.reminds:
      sqlite.execute(
        "INSERT INTO task_reminds (task_id, remind_at) VALUES (?, ?)",
        [sqlite_id, remind]
      )
  
  return report
```

#### Step 3: Pull Updates (Neon → SQLite)

```rust
fn pull_updates(sqlite: &Connection, client: &ApiClient, state: &SyncState) -> Result<PullReport>:
  let last_pull = state.get("last_pull_at")
  let response = client.get(f"/api/sync/tasks/changes?since={last_pull}")
  
  for task in response.tasks:
    let existing = sqlite.query_optional(
      "SELECT * FROM tasks WHERE id = ?", [task.task_number]
    )
    
    match existing:
      None =>
        // SQLite にない → INSERT (id を明示指定)
        sqlite.execute(
          "INSERT INTO tasks (id, title, status, ...) VALUES (?, ?, ?, ...)",
          [task.task_number, task.title, task.status, ...]
        )
      
      Some(local) =>
        if task.updated_at > local.updated:
          // Neon が新しい → SQLite を更新
          sqlite.execute(
            "UPDATE tasks SET title=?, status=?, ... WHERE id=?",
            [task.title, task.status, ..., task.task_number]
          )
        // else: SQLite が新しい → 次の push で Neon に反映されるのでスキップ
    
    // reminds の同期 (全置換)
    sqlite.execute("DELETE FROM task_reminds WHERE task_id = ?", [task.task_number])
    for remind in task.reminds:
      sqlite.execute("INSERT INTO task_reminds (task_id, remind_at) VALUES (?, ?)", [task.task_number, remind])
  
  state.set("last_pull_at", response.server_time)
  return report
```

#### Step 4: Projects 同期

```rust
fn sync_projects(sqlite: &Connection, client: &ApiClient):
  // Push: SQLite → Neon (tasks push 時に project_name 含めて送るので、API 側で自動解決)
  // Pull: Neon → SQLite
  //   changes の各タスクに project_name が含まれる
  //   → SQLite 側で resolve_project(name) → INSERT OR IGNORE INTO projects
```

projects は tasks の push/pull に含めて同期する（別エンドポイント不要）。

## state.db

sync daemon 専用の状態管理 DB。my-task の `tasks.db` とは別ファイル。

```sql
-- ~/.config/my-task-sync/state.db
CREATE TABLE IF NOT EXISTS sync_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

| key | value | 説明 |
|-----|-------|------|
| `last_push_at` | ISO 8601 文字列 | 最後に push が成功した時刻 |
| `last_pull_at` | ISO 8601 文字列 | 最後に pull が成功した時刻（API の serverTime） |

### 初回起動時

`last_push_at` / `last_pull_at` が存在しない場合、**全データの初回同期**を行う:

1. SQLite の全タスクを push
2. Neon の全タスク（task_number IS NULL）を pull して採番
3. 成功後に state を書き込み

## エラーハンドリング

| ケース | 対策 |
|--------|------|
| ネットワークエラー | ログに記録、次サイクルでリトライ。state は更新しない |
| API 401 | ログにエラー出力。設定を確認するようメッセージを表示 |
| API 409 (task_number 重複) | 理論上は発生しない設計だが、ログに warning を記録 |
| SQLite BUSY | WAL モードなので読み取りは並行可能。書き込みロックは短い待機後にリトライ (busy_timeout) |
| sync daemon クラッシュ | launchctl KeepAlive で自動再起動。state.db が残るので途中から再開 |
| API レスポンスが不正 | serde のデシリアライズ失敗 → ログに記録、スキップ |

### リトライ戦略

- 各 API 呼び出しは最大 3 回リトライ（exponential backoff: 1s → 2s → 4s）
- sync_cycle 全体が失敗しても、メインループの sleep 後に再実行

## launchctl 設定

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.my-task-sync</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/my-task-sync</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/my-task-sync.out.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/my-task-sync.err.log</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_LOG</key>
    <string>my_task_sync=info</string>
  </dict>
</dict>
</plist>
```

### インストール手順

```bash
cargo build --release
cp target/release/my-task-sync /usr/local/bin/
mkdir -p ~/.config/my-task-sync
cp config.example.toml ~/.config/my-task-sync/config.toml
# config.toml を編集して api_key を設定

cp com.my-task-sync.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.my-task-sync.plist
```

### 管理コマンド

```bash
# 状態確認
launchctl list | grep my-task-sync

# 停止
launchctl unload ~/Library/LaunchAgents/com.my-task-sync.plist

# ログ確認
tail -f /tmp/my-task-sync.out.log

# 手動実行 (デバッグ用)
my-task-sync --once   # 1サイクルだけ実行して終了
my-task-sync --dry-run # API を叩かずに差分を表示
```

## 実装順序

```
1. cargo init + Cargo.toml 依存追加
2. config.rs: TOML 設定読み込み
3. model.rs: Task / Project / Remind 型定義
4. sqlite.rs: my-task SQLite の読み書き (rusqlite)
   - read_tasks_since(updated_after) → Vec<Task>
   - read_all_tasks() → Vec<Task>
   - insert_task(task) → sqlite_id
   - update_task(task)
   - read_reminds_for_tasks(task_ids) → HashMap<i32, Vec<String>>
   - resolve_project(name) → project_id
5. api_client.rs: my-own API クライアント (reqwest)
   - push_tasks(tasks) → PushResponse
   - get_unsynced() → Vec<UnsyncedTask>
   - patch_task_number(neon_id, task_number)
   - get_changes(since) → ChangesResponse
6. sync_state.rs: state.db 管理
7. sync_engine.rs: sync_cycle の各ステップ
   - push()
   - pull_unsynced()
   - pull_updates()
8. main.rs: ループ + graceful shutdown (SIGTERM/SIGINT)
9. --once / --dry-run フラグ対応
10. launchctl plist + README
```

## my-task 側の推奨改修（別リポジトリ）

sync の精度を上げるために、以下の改修が望ましい（必須ではない）:

| 改修 | 理由 | 優先度 |
|------|------|--------|
| `updated` カラムをタイムスタンプ精度に | 同日内の LWW 判定ができない（現在は `NaiveDate` = 日付精度） | 高 |
| `created` カラムも同様 | 同上 | 中 |
| SQLite マイグレーション | TEXT `YYYY-MM-DD` → TEXT `YYYY-MM-DDTHH:MM:SSZ` への変換 | 上記に付随 |

この改修がない場合、同日内に CLI と Web の両方で同じタスクを更新すると、LWW が正しく判定できない。ただし個人利用で同日内の競合は稀なので、初期リリースでは日付精度のままでも運用可能。

## テスト方針

```
Unit tests:
  - sqlite.rs: テスト用 in-memory SQLite で各関数をテスト
  - model.rs: シリアライズ/デシリアライズのテスト
  - sync_engine.rs: モック API クライアント + テスト用 SQLite で各ステップをテスト

Integration tests:
  - 実際の my-own (dev 環境) に対して sync_cycle を実行
  - --once --dry-run で差分を確認

Manual validation:
  - CLI でタスク作成 → 30秒以内に Web に反映されることを確認
  - Web でタスク作成 → 30秒以内に CLI (`my-task list`) に反映されることを確認
  - 両方で同じタスクを更新 → updated が新しい方が勝つことを確認
```
