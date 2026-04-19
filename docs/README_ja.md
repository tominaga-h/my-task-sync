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

> **Phase 2 完了** — `[ngrok].domain` を設定した状態で起動すると、my-task-sync
> が `ngrok` をサブプロセスとして立ち上げ、ループバックサーバーを安定した
> 公開 URL で Vercel 上の my-own から到達可能にする。

## 前提

- macOS (`launchctl` LaunchAgent を使用)
- Rust 1.75 以上 (native `async fn` in trait を使用)
- [`my-task`](https://github.com/mad-tmng/my-task) がインストール済みである
  こと (このサーバーが読み書きする SQLite スキーマを提供する)
- API キー — my-task-sync と呼び出し側の共有秘密
- (任意) [`ngrok`](https://ngrok.com/) バイナリ + authtoken — インターネット
  から到達可能にしたい場合 (Vercel 上の my-own が叩くなど)

## ngrok セットアップ (任意)

公開 URL が必要な場合のみ。ローカル専用運用ならスキップ可。

```bash
brew install ngrok
ngrok config add-authtoken <your-authtoken>
ngrok config check          # "Valid configuration file at ..." と出れば OK
```

次に https://dashboard.ngrok.com/cloud-edge/domains から無料ドメインを予約
(例: `unedified-carrie-example.ngrok-free.dev`) して、下の config に設定
する。

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

# [ngrok] — optional。`domain` を設定すると、my-task-sync が起動時に
# `ngrok http` をサブプロセスとして立ち上げ、ループバックポートを公開
# する。未設定ならサーバーは localhost からしか到達できない。
# [ngrok]
# domain = "unedified-carrie-example.ngrok-free.dev"
```

環境変数はファイルの値を上書きする:

| 変数名                        | 上書き対象         |
| ----------------------------- | ------------------ |
| `MY_TASK_SYNC_API_KEY`        | `[server].api_key` |
| `MY_TASK_SYNC_PORT`           | `[server].port`    |
| `MY_TASK_DATA_FILE`           | `[sqlite].path`    |
| `MY_TASK_SYNC_NGROK_DOMAIN`   | `[ngrok].domain`   |
| `RUST_LOG`                    | tracing フィルタ   |

`api_key` が解決できない場合、サーバーは起動拒否する (サイレントデフォルト
なし)。`[ngrok].domain` を設定していて `ngrok` バイナリが PATH にない場合
も同様に起動拒否 (fail-fast、上のインストール手順を指すエラーメッセージ
が出る)。

## LaunchAgent としてインストール

```bash
cp target/release/my-task-sync /usr/local/bin/
cp com.my-task-sync.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.my-task-sync.plist
```

ngrok を使う場合は plist の `EnvironmentVariables` 経由、あるいは agent が
読む `config.toml` にドメインを設定する (どちらでも可)。

## 運用

```bash
# 状態確認
launchctl list | grep my-task-sync

# 停止
launchctl unload ~/Library/LaunchAgents/com.my-task-sync.plist

# ログ (my-task-sync 本体)
tail -f /tmp/my-task-sync.out.log
tail -f /tmp/my-task-sync.err.log

# ログ (ngrok サブプロセス — 有効時のみ)
tail -f /tmp/my-task-sync-ngrok.out.log
tail -f /tmp/my-task-sync-ngrok.err.log

# 公開 URL が機能しているか確認 (認証不要)
curl -sS localhost:3333/api/status | jq
```

graceful shutdown は進行中の HTTP リクエストの完了を最大 10 秒待ってから
強制終了するので、`launchctl unload` や再起動サイクルが詰まることはない。
ngrok サブプロセスは shutdown フローで `killpg` (PG 全体 SIGKILL) を受け
確実に reap される。

### ログハイジーン

ngrok のログファイル `/tmp/my-task-sync-ngrok.{out,err}.log` は **append**
モードで開いている (再起動ごとに前回のクラッシュ情報を失わないため)。
自動ローテートはしないので、肥大化したら手動で削除すること — 次回起動時に
再作成される。

## API 動作確認 (Quick Check)

```bash
# 死活監視 (認証不要)
curl localhost:3333/healthz
# → 200 OK, body "ok"

# サーバー + ngrok の状態 (認証不要。ngrok URL 自体が secret ゲート)
curl -sS localhost:3333/api/status | jq

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
- ngrok なしで動かすときは `127.0.0.1` のみ bind。インターネット到達が
  必要なら `[ngrok].domain` を設定 (上記参照)。
- `/api/status` は意図的に認証なし。ngrok URL 自体を soft secret として
  扱うこと — 漏洩したら reserved domain をローテートする。
