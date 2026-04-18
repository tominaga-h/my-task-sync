# my-task-sync HTTP API

> Phase 1 / work-in-progress. Endpoints land incrementally:
>
> | Endpoint | Task | Status |
> |---|---|---|
> | `GET /healthz`                  | T1 | ✅ implemented |
> | `GET /api/tasks`                | T3 | ✅ implemented |
> | `POST /api/tasks`               | T5 | ✅ implemented |
> | `GET /api/tasks/:task_number`   | T4 | ⏳ planned |
> | `PATCH /api/tasks/:task_number` | T6 | ⏳ planned |
> | `GET /api/projects`             | T7 | ⏳ planned |
> | `GET /api/status`               | T11 (Phase 2) | ⏳ planned |
>
> Keep this doc in sync with code when an endpoint lands or changes.

## Conventions

- **Bind address**: `127.0.0.1:<port>` (default `3333`). External access comes
  via ngrok in Phase 2.
- **Auth**: all `/api/*` endpoints require `Authorization: Bearer <api_key>`
  (configure via `[server].api_key` or `MY_TASK_SYNC_API_KEY`).
- **`/healthz`** is intentionally unauthenticated so operators can smoke-test
  without the token.
- **Encoding**: request / response JSON bodies use **camelCase** keys.
- **Date format** (`due`, `doneAt`, `reminds[]`): `YYYY-MM-DD`.
- **Timestamp format** (`createdAt`, `updatedAt`, `serverTime`): RFC 3339
  UTC (`2026-04-18T13:00:00Z`).
- **`taskNumber`** is the SQLite rowid. The server is the sole numbering
  authority; clients must **never** send `taskNumber` in `POST` / `PATCH`
  request bodies (`400` otherwise).

## Error responses

All non-2xx responses return JSON of the form:

```json
{"error": "human-readable message"}
```

| Status | Meaning |
|-------:|---------|
| `400 Bad Request`          | invalid input (bad `status` / `since`, `taskNumber` in body, unknown fields, date-only `since` rejected, etc.) |
| `401 Unauthorized`         | missing / wrong / malformed Bearer token |
| `404 Not Found`            | route does not exist (or, for `GET /:n` etc., resource not found) |
| `500 Internal Server Error`| server-side failure; the detail is logged via `tracing::error!` but **not** returned to the client |

---

## GET /healthz

Liveness probe. **No authentication.** Always `200 OK` with body `ok`.

```bash
curl localhost:3333/healthz
# ok
```

---

## GET /api/tasks

List tasks, optionally filtered.

### Query parameters (all optional)

| Param     | Type              | Notes |
|-----------|-------------------|-------|
| `status`  | `open` / `done` / `closed` | exact match; other values → `400` |
| `since`   | RFC 3339 datetime | returns tasks with `updated >= date(since)`. Truncated to UTC date (storage is day-precision). Non-RFC 3339 (e.g. `YYYY-MM-DD` alone) → `400`. |
| `project` | string            | exact project name match |
| `limit`   | `u32`             | default **500** (server-side cap to avoid unbounded responses). Pass a larger value if you genuinely need more. `limit=0` returns an empty array. |

### Response 200

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

`serverTime` is the server's `Utc::now()` at response construction. Clients
can round-trip it into the next `?since=` for incremental fetches.

### Examples

```bash
# All tasks
curl -H "Authorization: Bearer $KEY" localhost:3333/api/tasks

# Only open tasks updated on/after 2026-04-10, limited to 10
curl -H "Authorization: Bearer $KEY" \
  "localhost:3333/api/tasks?status=open&since=2026-04-10T00:00:00Z&limit=10"

# Filter by project
curl -H "Authorization: Bearer $KEY" "localhost:3333/api/tasks?project=home"
```

---

## POST /api/tasks

Create a new task. The server assigns `taskNumber` (= SQLite rowid).

### Request body

Content-Type: `application/json`.

Required fields:
- `title` (string)
- `status` (`open` / `done` / `closed`)
- `source` (string — typically `cli` / `web`)
- `createdAt` (RFC 3339 datetime)
- `updatedAt` (RFC 3339 datetime)

Optional fields:
- `projectName` (string or `null`) — if the name doesn't yet exist in
  `projects`, a row is created transparently.
- `due` (`YYYY-MM-DD` or `null`)
- `doneAt` (`YYYY-MM-DD` or `null`)
- `important` (bool, default `false`)
- `reminds` (array of `YYYY-MM-DD`, default `[]`)

Forbidden:
- `taskNumber` (server-assigned — `400` if present)
- Unknown fields (`deny_unknown_fields` catches typos like `reminders` → `400`)

### Response 201

Same shape as `TaskDto` in `GET /api/tasks`, wrapped:

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

> **Note**: the response's `updatedAt` / `createdAt` are the stored values
> after SQLite's day-precision truncation — not exactly what you sent.

### Example

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
