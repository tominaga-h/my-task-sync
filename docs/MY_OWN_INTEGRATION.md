# my-own 統合ガイド (Phase 1)

> このドキュメントは **my-own** (`~/lab/typescript/REACT/my-own`) 側で
> 必要な実装変更をまとめた作業指示書。my-task-sync v2 (HTTP サーバー化)
> に合わせて、my-own の `/api/tasks` 周りを Neon 直読から my-task-sync
> の REST 呼び出しに切り替える。
>
> - my-task-sync 側の API 仕様: [`API.md`](API.md)
> - v2 設計全体: [`SERVER_DESIGN.md`](SERVER_DESIGN.md)

## 背景

v1 では **my-task-sync が polling daemon として my-own API を叩く** 方向
だった。v2 で **my-own が my-task-sync HTTP サーバーを叩く** 方向に反転
したため:

- my-own の既存 `/api/tasks` Route Handler は Neon の `tasks` テーブルを
  読んでいるが、これを **my-task-sync の `/api/tasks` 呼び出しに置き換える**
- SQLite が真実の源 (my-task CLI が書き込む先)、Neon の `tasks*` は実質
  不要になる (キャッシュとして残すかは要判断 — 後述)
- ユーザーラップトップが稼働中のときだけ my-task-sync が到達可能という
  **オフライン前提** が生まれる

## アーキテクチャ (変更後)

```
┌──────────────────────────┐         ┌──────────────────────────┐
│   my-own (Vercel)        │         │ my-task-sync (local Mac) │
│                          │         │                          │
│  app/tasks/page.tsx      │         │  axum :3333              │
│    │ SWR                 │         │  (launchctl LaunchAgent) │
│    ▼                     │         │                          │
│  /api/tasks (Route)      │  HTTPS  │  GET/POST/PATCH/GET       │
│    │ Bearer auth         ├────────►│    /api/tasks             │
│    │ + MY_TASK_SYNC_URL  │  via    │    /api/tasks/{n}         │
│    ▼                     │  ngrok  │    /api/projects          │
│  lib/api-data.ts         │ (Ph2)   │                          │
│                          │         │         │                │
│  (Neon: notes/links      │         │         ▼                │
│   のみを直接 CRUD)        │         │    SQLite ◄──── my-task  │
└──────────────────────────┘         └──────────────────────────┘
```

my-own が Vercel 上で動くので、Phase 1 (ローカル開発) では **my-own を
`npm run dev` で起動した上で** my-task-sync (localhost:3333) を叩く。
Phase 2 で ngrok 公開 URL に向ける。

## 変更サマリ

| 種別 | 対象 | 内容 |
|------|------|------|
| 追加 | `.env.local` / Vercel env | `MY_TASK_SYNC_BASE_URL`, `MY_TASK_SYNC_API_KEY` |
| 追加 | `lib/my-task-sync.ts` (新規) | my-task-sync HTTP クライアント (TypeScript 型 + fetch wrapper) |
| 書き換え | `lib/api-data.ts::listTasks` | Neon 直読 → my-task-sync `GET /api/tasks` |
| 書き換え | `app/api/tasks/route.ts` | GET のレスポンス形を my-task-sync に合わせる / POST / PATCH / GET /:n を追加 |
| 書き換え | `app/tasks/page.tsx` / `TasksView.tsx` | DTO が `taskNumber` / `projectName` / `reminds` に変わるので型・表示ロジック更新 |
| 削除候補 | `lib/task-migration.ts` | v1 用 (SQLite ファイル直読)。v2 では不要 |
| 削除候補 | Drizzle schema の `tasks` / `task_reminds` / `projects` (タスク関連) | Neon を真実の源から外すなら削除。キャッシュとして残すなら保持 (要判断) |

> **`notes` / `links` は従来通り Neon 直 CRUD。** 影響範囲はタスク系のみ。

## 設定変更

### env vars

`.env.example` に追記:

```bash
# my-task-sync (Phase 1: localhost / Phase 2: ngrok 公開 URL)
MY_TASK_SYNC_BASE_URL=http://localhost:3333
MY_TASK_SYNC_API_KEY=replace_me
```

- **ローカル開発**: `http://localhost:3333`
- **Vercel 本番 (Phase 2)**: `https://<ngrok-domain>.ngrok-free.dev`
- `MY_TASK_SYNC_API_KEY` は my-task-sync 側の `[server].api_key` と同じ値

### 解決ヘルパ (推奨実装)

```ts
// lib/my-task-sync.ts
export function mtsConfig() {
  const baseUrl = process.env.MY_TASK_SYNC_BASE_URL;
  const apiKey = process.env.MY_TASK_SYNC_API_KEY;
  if (!baseUrl) throw new Error("MY_TASK_SYNC_BASE_URL is required");
  if (!apiKey) throw new Error("MY_TASK_SYNC_API_KEY is required");
  return { baseUrl: baseUrl.replace(/\/$/, ""), apiKey };
}
```

## TypeScript 型定義

my-task-sync の DTO を TypeScript に写したもの。`lib/my-task-sync.ts` に
置く想定。

```ts
// lib/my-task-sync.ts

export type TaskStatus = "open" | "done" | "closed";

/** `GET /api/tasks` 要素 / `POST`・`PATCH`・`GET /:n` の `task` フィールド */
export type TaskDto = {
  taskNumber: number;       // = SQLite rowid (サーバー採番、クライアントからは触らない)
  title: string;
  status: TaskStatus;
  source: string;           // "cli" / "web" / その他
  projectName: string | null;
  due: string | null;       // YYYY-MM-DD
  doneAt: string | null;    // YYYY-MM-DD
  important: boolean;
  updatedAt: string;        // RFC 3339 datetime (UTC)
  createdAt: string;        // RFC 3339 datetime (UTC)
  reminds: string[];        // YYYY-MM-DD の配列
};

export type TaskListResponse = {
  tasks: TaskDto[];
  serverTime: string;       // 次回の ?since= に投げ戻せる RFC 3339 datetime
};

export type TaskResponse = {
  task: TaskDto;
  serverTime: string;
};

export type ProjectDto = { id: number; name: string };
export type ProjectListResponse = {
  projects: ProjectDto[];
  serverTime: string;
};

/** POST body。`taskNumber` は絶対に含めない (server が rowid を採番する) */
export type TaskCreateBody = {
  title: string;
  status: TaskStatus;
  source: string;
  projectName?: string | null;
  due?: string | null;
  doneAt?: string | null;
  important?: boolean;
  updatedAt: string;        // 必須
  createdAt: string;        // 必須
  reminds?: string[];       // default []
};

/** PATCH body。全フィールド optional。`null` 明示でクリア。
 *  `updatedAt` を送らないと my-task-sync 側で `Utc::now()` に auto-bump される */
export type TaskPatchBody = Partial<{
  title: string;
  status: TaskStatus;
  source: string;
  projectName: string | null;
  due: string | null;
  doneAt: string | null;
  important: boolean;
  updatedAt: string;
  createdAt: string;
  reminds: string[];        // null 不許可 (400)
}>;
```

## HTTP クライアント (my-task-sync 専用ラッパ)

既存 `lib/api-client.ts` は自前 `/api/*` 向けで、my-task-sync 呼び出しに
は専用ラッパを作ったほうが型安全。

```ts
// lib/my-task-sync.ts (続き)

type MtsError = {
  status: number;
  error: string;           // my-task-sync の {"error": "..."} を解いた値
};

async function mtsFetch<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const { baseUrl, apiKey } = mtsConfig();
  const headers = new Headers(init.headers);
  headers.set("Authorization", `Bearer ${apiKey}`);
  if (init.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }

  const resp = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers,
    // Route Handler (server) から呼ぶので cache: "no-store" が安全
    cache: "no-store",
  });

  if (!resp.ok) {
    let errorMsg = `HTTP ${resp.status}`;
    try {
      const body = await resp.json();
      if (typeof body?.error === "string") errorMsg = body.error;
    } catch {
      /* JSON でないレスポンスは status 文字列のまま */
    }
    const err: MtsError = { status: resp.status, error: errorMsg };
    throw err;
  }
  return resp.json() as Promise<T>;
}

// ---- endpoints ----

export function mtsListTasks(params?: {
  status?: TaskStatus;
  since?: string;          // RFC 3339 datetime
  project?: string;
  limit?: number;
}): Promise<TaskListResponse> {
  const q = new URLSearchParams();
  if (params?.status) q.set("status", params.status);
  if (params?.since) q.set("since", params.since);
  if (params?.project) q.set("project", params.project);
  if (params?.limit !== undefined) q.set("limit", String(params.limit));
  const query = q.toString();
  return mtsFetch<TaskListResponse>(`/api/tasks${query ? `?${query}` : ""}`);
}

export function mtsGetTask(taskNumber: number): Promise<TaskResponse> {
  return mtsFetch<TaskResponse>(`/api/tasks/${taskNumber}`);
}

export function mtsCreateTask(body: TaskCreateBody): Promise<TaskResponse> {
  return mtsFetch<TaskResponse>(`/api/tasks`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function mtsPatchTask(
  taskNumber: number,
  body: TaskPatchBody,
): Promise<TaskResponse> {
  return mtsFetch<TaskResponse>(`/api/tasks/${taskNumber}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export function mtsListProjects(): Promise<ProjectListResponse> {
  return mtsFetch<ProjectListResponse>(`/api/projects`);
}
```

### 重要な制約 (クライアント側で守る)

- **`taskNumber` を POST / PATCH の body に入れない** → 400 返却
- **未知フィールドを送らない** (例: `reminders` は typo → 400)
- **`since` は RFC 3339 datetime** (`YYYY-MM-DD` 単独は 400)。レスポンスの
  `serverTime` をそのまま次回の `since` に投げ戻す想定
- **`reminds: null` を送らない** → 400。未送信は "既存保持"

## 既存コードの書き換え

### `lib/api-data.ts::listTasks`

旧 (Neon 直読):

```ts
export async function listTasks(userId: string) {
  const [rows, reminds, projectRows] = await Promise.all([...]);
  return { rows, remindsMap, projectMap };
}
```

新 (my-task-sync 呼び出し):

```ts
import { mtsListTasks } from "./my-task-sync";

export async function listTasks(_userId: string) {
  // my-task-sync は単一ユーザーなので userId は無視 (署名は互換維持)。
  // limit は explicit に指定して DEFAULT_LIMIT (500) を明示化。
  const res = await mtsListTasks({ limit: 500 });
  return { tasks: res.tasks, serverTime: res.serverTime };
}
```

### `app/api/tasks/route.ts`

GET / POST / PATCH を揃える:

```ts
import { NextResponse } from "next/server";
import {
  mtsListTasks,
  mtsCreateTask,
  mtsPatchTask,
  TaskCreateBody,
  TaskPatchBody,
} from "../../../lib/my-task-sync";
import { requireApiUserId, toApiAuthResponse } from "../../../lib/api-auth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(request: Request) {
  try {
    requireApiUserId(request);
    const url = new URL(request.url);
    const since = url.searchParams.get("since") ?? undefined;
    const data = await mtsListTasks({ since, limit: 500 });
    return NextResponse.json(data);
  } catch (e) {
    const authResp = toApiAuthResponse(e);
    if (authResp) return authResp;
    return mapMtsError(e);
  }
}

export async function POST(request: Request) {
  try {
    requireApiUserId(request);
    const body = (await request.json()) as TaskCreateBody;
    const data = await mtsCreateTask(body);
    return NextResponse.json(data, { status: 201 });
  } catch (e) {
    const authResp = toApiAuthResponse(e);
    if (authResp) return authResp;
    return mapMtsError(e);
  }
}

// PATCH は別 route file: app/api/tasks/[taskNumber]/route.ts を作る

function mapMtsError(e: unknown): NextResponse {
  if (typeof e === "object" && e && "status" in e && "error" in e) {
    const err = e as { status: number; error: string };
    return NextResponse.json({ error: err.error }, { status: err.status });
  }
  // ネットワーク障害など: 502 Bad Gateway で "my-task-sync unreachable"
  return NextResponse.json(
    { error: "my-task-sync unreachable" },
    { status: 502 },
  );
}
```

PATCH / 単一 GET 用:

```ts
// app/api/tasks/[taskNumber]/route.ts
import { NextResponse } from "next/server";
import {
  mtsGetTask,
  mtsPatchTask,
  TaskPatchBody,
} from "../../../../lib/my-task-sync";
import { requireApiUserId, toApiAuthResponse } from "../../../../lib/api-auth";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ taskNumber: string }> },
) {
  try {
    requireApiUserId(request);
    const { taskNumber } = await params;
    const data = await mtsGetTask(Number(taskNumber));
    return NextResponse.json(data);
  } catch (e) {
    const authResp = toApiAuthResponse(e);
    if (authResp) return authResp;
    return mapMtsError(e);
  }
}

export async function PATCH(
  request: Request,
  { params }: { params: Promise<{ taskNumber: string }> },
) {
  try {
    requireApiUserId(request);
    const { taskNumber } = await params;
    const body = (await request.json()) as TaskPatchBody;
    const data = await mtsPatchTask(Number(taskNumber), body);
    return NextResponse.json(data);
  } catch (e) {
    const authResp = toApiAuthResponse(e);
    if (authResp) return authResp;
    return mapMtsError(e);
  }
}

// mapMtsError は共通ユーティリティ化を推奨 (lib/my-task-sync.ts に移動など)
```

### `app/tasks/page.tsx` + `TasksView.tsx`

SWR の型を更新:

```ts
// 旧
type TasksResponse = {
  rows: Array<{ id: number; userId: string; /* ... */ }>;
  remindsMap: Record<number, string[]>;
  projectMap: Record<number, string>;
};

// 新 (my-task-sync のレスポンス形をそのまま使う)
import { TaskListResponse } from "../../lib/my-task-sync";
type TasksResponse = TaskListResponse;
```

`TasksView.tsx` の props も `tasks: TaskDto[]` に変更。フィールド名の
書き換え:
- `id` → `taskNumber`
- `projectId` + `projectMap` → `projectName` 直参照
- `createdAt` / `updatedAt` は ISO 文字列のまま
- `remindsMap[id]` → `task.reminds`

## 未決事項 (要判断)

### 1. Neon の `tasks` / `task_reminds` / `projects` テーブルをどうするか

選択肢:

**(a) 完全削除** — Neon からタスク系を落とす
  - メリット: スキーマが単純化、二重管理の誤解を防げる
  - デメリット: my-task-sync オフライン時に my-own が何も表示できない

**(b) read-through キャッシュとして残す** — my-own は my-task-sync を
    優先し、到達不能時に Neon から読む。書き込みは常に my-task-sync 経由
  - メリット: オフライン時も直近のタスク閲覧が可能
  - デメリット: 整合性管理が必要 (いつキャッシュを更新するか)

**(c) 書き込みも Neon に二重化** — 過剰設計、非推奨

**推奨: 最初は (a)**。`lib/task-migration.ts` は 1 回ローカル同期する用途
で存在していたので削除、Drizzle schema からも tasks 系を落とす。オフライン
閲覧が必要になったら (b) に移行。

### 2. `userId` の扱い

my-own の既存スキーマは `userId` で分離 (マルチユーザー想定)。my-task-sync
は単一ユーザー前提で `userId` を知らない。Phase 1 では:

- my-own の Route Handler は `requireApiUserId(request)` で引き続き認証
- my-task-sync には `userId` を送らない (必要なし)
- 将来マルチユーザー対応する場合、my-task-sync 側も user 分離するか、
  インスタンスを user ごとに立てるかを別途判断

### 3. オフライン時の UX

my-task-sync がオフライン (ラップトップ shut down / ngrok 切断) のとき:
- fetch が失敗 → Route Handler が 502 を返す → SWR がエラー状態
- **最低限**: ページ側で "my-task-sync に到達できません" バナーを出す
- **推奨**: retry ボタン + 直近の成功レスポンスを localStorage にキャッシュ
  して読み取り専用モードで表示

## 開発フロー (ローカル)

my-task-sync と my-own を同時に立ち上げる:

```bash
# terminal 1: my-task-sync
cd ~/lab/rust/my-task-sync
MY_TASK_SYNC_API_KEY=dev-secret MY_TASK_SYNC_PORT=3333 cargo run

# terminal 2: my-own
cd ~/lab/typescript/REACT/my-own
# .env.local に以下を追加
# MY_TASK_SYNC_BASE_URL=http://localhost:3333
# MY_TASK_SYNC_API_KEY=dev-secret
npm run dev
# → http://localhost:3000
```

### 結合テスト checklist (my-task-sync T8 と同じ)

- [ ] `my-task add "…"` (CLI) → my-own `/tasks` で即時表示
- [ ] my-own で新規作成 → `my-task ls` で表示
- [ ] 両側で同一タスクを編集 → `updatedAt` が新しい方が勝つ (後勝ち)
- [ ] `my-task done <n>` → my-own で status=done 反映
- [ ] my-own で project 新規指定 → `my-task projects` に出現
- [ ] my-task-sync を Ctrl-C で落とす → my-own に 502 エラーバナー
- [ ] my-task-sync 再起動 → my-own が回復

## Phase 2 での切り替え

my-task-sync が ngrok サブプロセスを自動起動するようになったら、Vercel の
環境変数を更新するだけ:

| Vercel env | 値 |
|------------|------|
| `MY_TASK_SYNC_BASE_URL` | `https://<ngrok-domain>.ngrok-free.dev` |
| `MY_TASK_SYNC_API_KEY`  | (変更なし) |

クライアントコードは変更不要。

## 実装順 (推奨)

1. `.env.example` に `MY_TASK_SYNC_BASE_URL` / `MY_TASK_SYNC_API_KEY` 追加
2. `lib/my-task-sync.ts` 新規作成 (型 + `mtsFetch` + 5 ハンドラ)
3. `lib/api-data.ts::listTasks` を差し替え (旧実装をコメント保持でもよい)
4. `app/api/tasks/route.ts` を GET + POST 対応に更新
5. `app/api/tasks/[taskNumber]/route.ts` を新規作成 (GET + PATCH)
6. `app/tasks/page.tsx` + `TasksView.tsx` を新 DTO に合わせる
7. ローカルで my-task-sync + my-own を両方立てて結合テスト
8. (判断後) Neon スキーマから tasks 系削除 + `lib/task-migration.ts` 削除
9. Phase 2 で Vercel env を ngrok URL に更新

## 参考: my-task-sync 側の開発状況

Phase 1 のエンドポイント実装は完了済み ([`API.md`](API.md) 参照)。残タスク
は T8 (このドキュメントに書いた結合テスト) のみ。Phase 2 (ngrok 自動起動
+ `/api/status`) はこの統合後に着手する。
