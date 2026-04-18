# my-task-sync 概要

> ドキュメント群のハブ。プロジェクト全体像 + 関連リポジトリ + 各詳細ドキュメント
> への入口をまとめる。詳細な設計・実装・API はリンク先を参照。

## これは何

my-task の SQLite を REST で公開し、my-own から直接 CRUD できるようにする
macOS ローカル HTTP サーバー。単一ユーザー・単一マシン運用を前提とする。

v1 (polling daemon) は完全削除済み。v2 は axum サーバーとして `launchctl` 配下で
常駐し、ループバックポート (デフォルト `:3333`) に bind する。公開アクセスは
Phase 2 で導入予定の ngrok サブプロセス経由になる。

## 関連リポジトリ

### my-own (Web アプリ、呼び出し側)

- **リポジトリ**: `~/lab/typescript/REACT/my-own`
- **技術スタック**: Next.js 15 (App Router) / React 19 / TypeScript / Tailwind CSS v4 /
  Neon Serverless Postgres / Drizzle ORM
- **ホスティング**: Vercel
- **役割** (v2): my-task-sync が公開する `/api/tasks` を直接叩いてタスクを
  表示・編集する
- **認証**: `Authorization: Bearer <api_key>` を my-task-sync に送る

### my-task (Rust CLI、データオーナー)

- **リポジトリ**: `~/lab/rust/my-task`
- **技術スタック**: Rust / clap 4 / rusqlite 0.31 (bundled) / chrono
- **DB**: SQLite (`~/Library/Application Support/my-task/tasks.db`) — my-task-sync は
  このファイルを直接読み書きする
- **スキーマ**: `tasks` / `projects` / `task_reminds` (3 テーブル)。
  詳細は `~/lab/rust/my-task/src/db.rs` L14-38 が一次情報源で、
  `tests/common/mod.rs::make_my_task_db` はこれと byte-identical に保つこと。

## アーキテクチャ

```
my-task (CLI)   ──writes──► SQLite ◄──reads/writes── my-task-sync server
                              ▲                       (axum, :<port>)
                              │                              ▲
                              │                              │ Bearer auth
                              │                     ┌────────┤
                              │                     │        │
                              │             (Phase 2)        │ (Phase 1: 直接)
                              │            ngrok tunnel      │
                              │                     │        │
                              │                     ▼        │
                              │               my-own (Vercel)
                              │                     │
                              │                     ▼
                              │                   Neon (tasks 以外)
                              │
                     launchctl LaunchAgent
```

## フェーズ状況

| Phase | スコープ | 状態 |
|-------|---------|------|
| **1** | REST サーバー骨格 + 認証 + `/api/tasks` CRUD + `/api/projects` | 進行中 (T6 / T7 / T8 残) |
| **2** | ngrok サブプロセス自動起動 + `/api/status` | 未着手 |

詳細タスク分解は [`../tasks/plan.md`](../tasks/plan.md)、進捗チェックリストは
[`../tasks/todo.md`](../tasks/todo.md)。

## ドキュメント案内

| 目的 | 参照先 |
|------|-------|
| v2 設計仕様 (motivation / アーキテクチャ / Phase 構成) | [`SERVER_DESIGN.md`](SERVER_DESIGN.md) |
| HTTP API リファレンス (エンドポイント・パラメータ・例) | [`API.md`](API.md) |
| インストール / 起動 / 設定 (英語) | [`../README.md`](../README.md) |
| インストール / 起動 / 設定 (日本語) | [`README_ja.md`](README_ja.md) |
| 実装プラン (垂直スライス / チェックポイント / リスク) | [`../tasks/plan.md`](../tasks/plan.md) |
| 実装 TODO (チェックボックス) | [`../tasks/todo.md`](../tasks/todo.md) |
| Claude Code 向け運用ガイド (不変量・コマンド・編集ガイド) | [`../CLAUDE.md`](../CLAUDE.md) |

## 設計決定事項 (抜粋)

詳細は [`SERVER_DESIGN.md`](SERVER_DESIGN.md) と [`../CLAUDE.md`](../CLAUDE.md)。

| 項目 | 決定 |
|------|------|
| 言語 | **Rust** (my-task と rusqlite コード共有、単一バイナリ配布) |
| HTTP フレームワーク | **axum 0.8** + tower / tower-http |
| 実行形態 | macOS **launchctl LaunchAgent** 常駐 |
| 通信方向 | my-own が my-task-sync を呼ぶ **REST** (v1 の polling は廃止) |
| 認証 | 静的 **Bearer token** (`[server].api_key` or `MY_TASK_SYNC_API_KEY`) |
| bind | **loopback (`127.0.0.1`) のみ** — 公開は Phase 2 の ngrok 経由 |
| `task_number` 採番 | **SQLite rowid が唯一の採番元**。サーバーが POST 時に付与、body 側は禁止 (400) |
| 日付精度 | **日単位** (`NaiveDate` / `YYYY-MM-DD`) — my-task スキーマ継承 |
| `since` の比較 | RFC 3339 datetime を UTC 日付に truncate → `updated >= date(since)` (inclusive) |
| config 場所 | `$HOME/.config/my-task-sync/config.toml` (macOS でも `Library/Application Support` は使わない) |
| テスト DB | `tests/common/mod.rs::make_my_task_db` の in-memory モック。実 `tasks.db` は絶対に触らない |

## 廃止された v1 概念

v2 移行時 (commit `5524cbc`) に削除されたもの。コード・ドキュメントに残骸を
見つけたら [`SERVER_DESIGN.md`](SERVER_DESIGN.md) 準拠に修正すること。

- `sync_engine` の 3 段階 cycle (push → pull_unsynced → pull_updates)
- `state.db` と `last_push_at` / `last_pull_at`
- CLI フラグ `--once` / `--dry-run`
- `api_client` (my-task-sync が my-own を叩いていた側)
- 環境変数 `MY_TASK_SYNC_BASE_URL`
- 設定セクション `[api]` / `[sync]`
