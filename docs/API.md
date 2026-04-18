# my-task-sync HTTP API

> Phase 1 進行中。エンドポイントは段階的に追加される:
>
> | エンドポイント | タスク | 状態 |
> |---|---|---|
> | `GET /healthz`                  | T1 | ✅ 実装済み |
> | `GET /api/tasks`                | T3 | ✅ 実装済み |
> | `POST /api/tasks`               | T5 | ✅ 実装済み |
> | `GET /api/tasks/:task_number`   | T4 | ⏳ 未実装 |
> | `PATCH /api/tasks/:task_number` | T6 | ⏳ 未実装 |
> | `GET /api/projects`             | T7 | ⏳ 未実装 |
> | `GET /api/status`               | T11 (Phase 2) | ⏳ 未実装 |
>
> エンドポイントの追加・変更時はこのドキュメントも同時に更新すること。

## 共通ルール

- **bind アドレス**: `127.0.0.1:<port>` (デフォルト `3333`)。
  外部アクセスは Phase 2 で ngrok 経由になる。
- **認証**: `/api/*` は全て `Authorization: Bearer <api_key>` を要求する
  (`[server].api_key` または `MY_TASK_SYNC_API_KEY` で設定)。
- **`/healthz`** は意図的に認証不要。運用者がトークン無しで死活確認できる
  ようにするため。
- **エンコーディング**: リクエスト / レスポンスの JSON body は
  **camelCase** キー。
- **日付形式** (`due`, `doneAt`, `reminds[]`): `YYYY-MM-DD`。
- **タイムスタンプ形式** (`createdAt`, `updatedAt`, `serverTime`):
  RFC 3339 UTC (`2026-04-18T13:00:00Z`)。
- **`taskNumber`** は SQLite rowid。採番はサーバーの専権事項で、クライアントは
  `POST` / `PATCH` の body に **絶対に含めない** (含まれていた場合は `400`)。

## エラーレスポンス

非 2xx のレスポンスは全て以下の形式の JSON を返す:

```json
{"error": "human-readable message"}
```

| ステータス | 意味 |
|-------:|---------|
| `400 Bad Request`          | 不正な入力 (`status` が許容値外 / `since` が RFC 3339 でない / body に `taskNumber` がある / 未知のフィールドがある / `YYYY-MM-DD` 単独の `since` など) |
| `401 Unauthorized`         | Bearer トークン欠落 / 形式不正 / 不一致 |
| `404 Not Found`            | ルートが存在しない (または `GET /:n` 等でリソースが見つからない) |
| `500 Internal Server Error`| サーバー側の障害。詳細は `tracing::error!` にログされるが、レスポンスには含まない |

---

## GET /healthz

死活監視プローブ。**認証不要**。常に `200 OK`、body は `ok`。

```bash
curl localhost:3333/healthz
# ok
```

---

## GET /api/tasks

タスク一覧をオプションフィルタ付きで取得する。

### クエリパラメータ (全て optional)

| パラメータ | 型              | 備考 |
|-----------|-------------------|------|
| `status`  | `open` / `done` / `closed` | 完全一致。上記以外は `400` |
| `since`   | RFC 3339 datetime | `updated >= date(since)` のタスクを返す。UTC 日付に truncate される (ストレージは日単位精度)。非 RFC 3339 (例: `YYYY-MM-DD` 単独) は `400` |
| `project` | string            | プロジェクト名の完全一致 |
| `limit`   | `u32`             | デフォルト **500** (レスポンス爆発を防ぐサーバー側の安全弁)。それ以上必要なら明示的に指定する。`limit=0` は空配列を返す |

### レスポンス 200

```json
{
  "tasks": [
    {
      "taskNumber": 1,
      "title": "buy milk",
      "status": "open",
      "source": "cli",
      "projectName": "home",
      "due": null,
      "doneAt": null,
      "important": false,
      "updatedAt": "2026-04-18T00:00:00Z",
      "createdAt": "2026-04-15T00:00:00Z",
      "reminds": ["2026-04-22"]
    }
  ],
  "serverTime": "2026-04-18T13:24:00Z"
}
```

`serverTime` はレスポンス構築時のサーバー `Utc::now()`。クライアントは
これを次回の `?since=` にそのまま投げて incremental fetch できる。

### 例

```bash
# 全件
curl -H "Authorization: Bearer $KEY" localhost:3333/api/tasks

# 2026-04-10 以降に更新された open タスクを最大 10 件
curl -H "Authorization: Bearer $KEY" \
  "localhost:3333/api/tasks?status=open&since=2026-04-10T00:00:00Z&limit=10"

# プロジェクトで絞り込み
curl -H "Authorization: Bearer $KEY" "localhost:3333/api/tasks?project=home"
```

---

## POST /api/tasks

新規タスクを作成する。サーバーが `taskNumber` (= SQLite rowid) を採番する。

### リクエスト body

Content-Type: `application/json`。

**必須**:
- `title` (string)
- `status` (`open` / `done` / `closed`)
- `source` (string — 通常は `cli` / `web`)
- `createdAt` (RFC 3339 datetime)
- `updatedAt` (RFC 3339 datetime)

**任意**:
- `projectName` (string or `null`) — 未登録の名前を指定すると `projects` に
  透過的に INSERT される
- `due` (`YYYY-MM-DD` or `null`)
- `doneAt` (`YYYY-MM-DD` or `null`)
- `important` (bool, デフォルト `false`)
- `reminds` (`YYYY-MM-DD` の配列, デフォルト `[]`)

**禁止**:
- `taskNumber` (サーバー採番のため含めると `400`)
- 未知のフィールド (`deny_unknown_fields` により `reminders` のような
  typo を `400` で検出)

### レスポンス 201

`GET /api/tasks` の要素と同じ形を `task` に包んで返す:

```json
{
  "task": {
    "taskNumber": 42,
    "title": "buy milk",
    "status": "open",
    "source": "web",
    "projectName": null,
    "due": null,
    "doneAt": null,
    "important": false,
    "updatedAt": "2026-04-18T00:00:00Z",
    "createdAt": "2026-04-18T00:00:00Z",
    "reminds": []
  },
  "serverTime": "2026-04-18T13:35:24Z"
}
```

> **注**: レスポンス中の `updatedAt` / `createdAt` は SQLite の日単位 truncate
> を経た「保存後の値」で、送信した値と正確には一致しない。

### 例

```bash
curl -X POST \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{
    "title": "buy milk",
    "status": "open",
    "source": "web",
    "projectName": "home",
    "important": true,
    "createdAt": "2026-04-18T10:00:00Z",
    "updatedAt": "2026-04-18T10:00:00Z",
    "reminds": ["2026-04-22"]
  }' \
  localhost:3333/api/tasks
```
