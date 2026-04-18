# my-task-sync

**my-task** の SQLite を REST で公開し、**my-own** (Next.js 製 Web アプリ) が
タスクを読み書きできるようにする macOS ローカル HTTP サーバー。`launchctl`
配下でループバックポート (デフォルト `:3333`) で常駐し、Bearer トークン
認証を要求する。

単一ユーザー・単一マシン向け設計 — マルチテナント用途ではない。

> English version: [`../README.md`](../README.md)

- 設計仕様: [`SERVER_DESIGN.md`](SERVER_DESIGN.md)
- HTTP API リファレンス: [`API.md`](API.md)
- 移行プランと進捗: [`../tasks/plan.md`](../tasks/plan.md),
  [`../tasks/todo.md`](../tasks/todo.md)

> **Phase 1** — ローカルで走らせた my-own からのループバックアクセスのみ。
> Phase 2 で ngrok サブプロセスを組み込み、Vercel にデプロイ済みの my-own から
> 安定した公開 URL 経由で到達できるようにする。

## 前提

- macOS (`launchctl` LaunchAgent を使用)
- Rust 1.75 以上 (native `async fn` in trait を使用)
- [`my-task`](https://github.com/mad-tmng/my-task) がインストール済みである
  こと (このサーバーが読み書きする SQLite スキーマを提供する)
- API キー — my-task-sync と呼び出し側の共有秘密

## ビルド

```bash
cargo build --release           # リリースビルド → target/release/my-task-sync
make check                      # fmt + check + clippy + test (pre-push ゲート)
```

## 設定

```bash
mkdir -p ~/.config/my-task-sync
cp config.example.toml ~/.config/my-task-sync/config.toml
$EDITOR ~/.config/my-task-sync/config.toml     # api_key を設定
```

`config.toml` の形:

```toml
[sqlite]
# path = "/custom/path/tasks.db"    # デフォルト: ~/Library/Application Support/my-task/tasks.db

[server]
port    = 3333
api_key = "your-api-key-here"
```

環境変数はファイルの値を上書きする:

| 変数名                    | 上書き対象          |
|---------------------------|---------------------|
| `MY_TASK_SYNC_API_KEY`    | `[server].api_key`  |
| `MY_TASK_SYNC_PORT`       | `[server].port`     |
| `MY_TASK_DATA_FILE`       | `[sqlite].path`     |
| `RUST_LOG`                | tracing フィルタ    |

`api_key` が解決できない場合、サーバーは起動拒否する (サイレントデフォルト
なし)。

## LaunchAgent としてインストール

```bash
cp target/release/my-task-sync /usr/local/bin/
cp com.my-task-sync.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.my-task-sync.plist
```

## 運用

```bash
# 状態確認
launchctl list | grep my-task-sync

# 停止
launchctl unload ~/Library/LaunchAgents/com.my-task-sync.plist

# ログ
tail -f /tmp/my-task-sync.out.log
tail -f /tmp/my-task-sync.err.log
```

graceful shutdown は進行中の HTTP リクエストの完了を最大 10 秒待ってから
強制終了するので、`launchctl unload` や再起動サイクルが詰まることはない。

## API 動作確認 (Quick Check)

```bash
# 死活監視 (認証不要)
curl localhost:3333/healthz
# → 200 OK, body "ok"

# タスク一覧 (認証必要)
curl -H "Authorization: Bearer $MY_TASK_SYNC_API_KEY" localhost:3333/api/tasks

# タスク作成
curl -X POST \
  -H "Authorization: Bearer $MY_TASK_SYNC_API_KEY" \
  -H "content-type: application/json" \
  -d '{"title":"buy milk","status":"open","source":"web","createdAt":"2026-04-18T10:00:00Z","updatedAt":"2026-04-18T10:00:00Z"}' \
  localhost:3333/api/tasks
```

全エンドポイントのリクエスト / レスポンス形式とエラーコードは
[`API.md`](API.md) にまとめてある。

## 既知の制約

- `updated` は日単位精度 (my-task の `NaiveDate` スキーマを継承)。
  `?since=<RFC 3339 datetime>` フィルタは UTC 日付に truncate して
  inclusive (`>=`) で比較するため、同日内の更新は incremental fetch の
  複数回にまたがって現れる可能性がある — クライアントは `taskNumber` で
  dedup すること。
- サーバーは `127.0.0.1` にのみ bind する。公開アクセスは Phase 2 の
  ngrok サブプロセスが担当する。
