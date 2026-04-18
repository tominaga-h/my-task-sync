# my-task-sync HTTP API

> Phase 1 進行中。エンドポイントは段階的に追加される:
>
> | エンドポイント                  | タスク        | 状態        |
> | ------------------------------- | ------------- | ----------- |
> | `GET /healthz`                  | T1            | ✅ 実装済み |
> | `GET /api/tasks`                | T3            | ✅ 実装済み |
> | `GET /api/tasks/:task_number`   | T4            | ✅ 実装済み |
> | `POST /api/tasks`               | T5            | ✅ 実装済み |
> | `PATCH /api/tasks/:task_number` | T6            | ✅ 実装済み |
> | `GET /api/projects`             | T7            | ✅ 実装済み |
> | `GET /api/status`               | T11 (Phase 2) | ⏳ 未実装   |
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
{ "error": "human-readable message" }
```

|                  ステータス | 意味                                                                                                                                                    |
| --------------------------: | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
|           `400 Bad Request` | 不正な入力 (`status` が許容値外 / `since` が RFC 3339 でない / body に `taskNumber` がある / 未知のフィールドがある / `YYYY-MM-DD` 単独の `since` など) |
|          `401 Unauthorized` | Bearer トークン欠落 / 形式不正 / 不一致                                                                                                                 |
|             `404 Not Found` | ルートが存在しない (または `GET /:n` 等でリソースが見つからない)                                                                                        |
| `500 Internal Server Error` | サーバー側の障害。詳細は `tracing::error!` にログされるが、レスポンスには含まない                                                                       |

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

| パラメータ | 型                         | 備考                                                                                                                                       |
| ---------- | -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `status`   | `open` / `done` / `closed` | 完全一致。上記以外は `400`                                                                                                                 |
| `since`    | RFC 3339 datetime          | `updated >= date(since)` のタスクを返す。UTC 日付に truncate される (ストレージは日単位精度)。非 RFC 3339 (例: `YYYY-MM-DD` 単独) は `400` |
| `project`  | string                     | プロジェクト名の完全一致                                                                                                                   |
| `limit`    | `u32`                      | デフォルト **500** (レスポンス爆発を防ぐサーバー側の安全弁)。それ以上必要なら明示的に指定する。`limit=0` は空配列を返す                    |

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

## GET /api/tasks/:task_number

単一タスクを取得する。

### パスパラメータ

- `task_number` — 対象タスクの ID (= SQLite rowid)。数値パース失敗時は `400`、
  該当タスクが存在しなければ `404`。

### レスポンス 200

`POST` / `PATCH` と同じ `{ task, serverTime }` 形:

```json
{
  "task": {
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
  },
  "serverTime": "2026-04-18T23:21:22Z"
}
```

### レスポンス 404

```json
{"error": "not found"}
```

### 例

```bash
curl -H "Authorization: Bearer $KEY" localhost:3333/api/tasks/1
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

---

## PATCH /api/tasks/:task_number

既存タスクの部分更新。送ったフィールドだけ上書きされ、送らなかったフィールドは
既存値が維持される。

### パスパラメータ

- `task_number` — 対象タスクの ID (= SQLite rowid)。数値パース失敗時は `400`、
  該当タスクが存在しなければ `404`。

### リクエスト body

Content-Type: `application/json`。**body は JSON オブジェクト必須** (`null` や
配列は `400`)。

**許容フィールド** (全て optional):

| フィールド    | 型                  | 動作                                            |
|--------------|---------------------|------------------------------------------------|
| `title`       | string              | 上書き                                         |
| `status`      | `open`/`done`/`closed` | 上書き。他の値は `400`                        |
| `source`      | string              | 上書き                                         |
| `projectName` | string or `null`    | 値: 上書き (未登録プロジェクトは透過 INSERT) / `null`: 所属をクリア |
| `due`         | `YYYY-MM-DD` or `null` | 値: 上書き / `null`: クリア                   |
| `doneAt`      | `YYYY-MM-DD` or `null` | 値: 上書き / `null`: クリア                   |
| `important`   | bool                | 上書き                                         |
| `createdAt`   | RFC 3339 datetime   | 上書き (通常は送らない運用を想定)               |
| `updatedAt`   | RFC 3339 datetime   | 送信時: その値で上書き / **未送信時: `Utc::now()` で auto-bump** |
| `reminds`     | `YYYY-MM-DD` の配列 | 送信時: 全置換 / 未送信時: 既存を保持 / `null` は `400` |

**禁止フィールド**:
- `taskNumber` (URL 側が唯一の権威 — 含めると `400`)
- 上記以外の未知フィールド (typo 防止 — `400`)

**フィールドの 3 状態**: nullable フィールド (`projectName` / `due` / `doneAt`) は
「未送信 (既存維持)」「`null` 送信 (クリア)」「値送信 (上書き)」の 3 通り。
非 nullable フィールドは `null` 送信すると `400`。

### 空 body の扱い

`{}` は全フィールド未送信扱い → `updatedAt` だけ auto-bump され、他は変化なしで
`200 OK` を返す (no-op + タイムスタンプ更新)。

### レスポンス 200

`POST` と同じ `{ task, serverTime }` 形 (書き戻し直後の完全な `TaskDto` を返す)。

### レスポンス 404

```json
{"error": "not found"}
```

### 例

```bash
# タイトルだけ変更 (updatedAt は auto-bump される)
curl -X PATCH \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"title":"renamed"}' \
  localhost:3333/api/tasks/42

# projectName をクリア
curl -X PATCH \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"projectName":null}' \
  localhost:3333/api/tasks/42

# reminds を全置換
curl -X PATCH \
  -H "Authorization: Bearer $KEY" \
  -H "content-type: application/json" \
  -d '{"reminds":["2026-06-01","2026-06-10"]}' \
  localhost:3333/api/tasks/42
```

---

## GET /api/projects

プロジェクト一覧を取得する。件数制限 / ページングなし (単一ユーザー運用前提)。

### クエリパラメータ

なし。

### レスポンス 200

```json
{
  "projects": [
    { "id": 1, "name": "home" },
    { "id": 2, "name": "work" },
    { "id": 3, "name": "hobby" }
  ],
  "serverTime": "2026-04-18T14:36:11Z"
}
```

順序は `id ASC` (= 挿入順)。`id` は `projects.id` (`task.projectName` から JOIN
で引ける)。

### プロジェクト作成について

**新規作成エンドポイントは存在しない**。プロジェクトは `POST /api/tasks` / `PATCH /api/tasks/:n`
で `projectName` に未登録の名前を指定すると、サーバー側で透過的に `INSERT OR IGNORE`
される (= `sqlite::resolve_project` が担う)。したがってクライアントは
「プロジェクト作成」を意識する必要がない。

### 例

```bash
curl -H "Authorization: Bearer $KEY" localhost:3333/api/projects
```
