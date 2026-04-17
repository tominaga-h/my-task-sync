//! Three-step sync cycle: push → pull_unsynced → pull_updates.
//!
//! Implementation matches `OVERVIEW.md` § sync engine 詳細.
//!
//! * **push** — read SQLite rows updated since `last_push_at`, send the
//!   batch, then update state.
//! * **pull_unsynced** — pull rows the Web UI created without a
//!   `task_number`, INSERT them into SQLite (auto-increment id) and
//!   PATCH that id back to Neon. INSERT + reminds + PATCH are wrapped
//!   in a single SQLite transaction; if the PATCH fails the transaction
//!   is rolled back so the next cycle won't double-INSERT.
//! * **pull_updates** — pull rows changed since `last_pull_at`, apply
//!   row-level Last-Write-Wins by `updated` date, replace reminds in
//!   full for any updated row, then advance `last_pull_at` to the
//!   server's reported time.
//!
//! `dry_run=true` suppresses every API write (push / patch) **and**
//! every state.set as well as every SQLite mutation that follows them.
//! Read-only API calls still happen so a dry run reflects what *would*
//! be observed.

use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use rusqlite::{params, Connection};

use crate::api_client::SyncApi;
use crate::error::Error;
use crate::model::{ChangedTask, SyncTask, Task, UnsyncedTask};
use crate::sqlite::{self, TaskRow};
use crate::sync_state::SyncState;

const DATE_FMT: &str = "%Y-%m-%d";

// ------------------------------------------------------------------
// sync_cycle: orchestrates the three steps
// ------------------------------------------------------------------

pub async fn sync_cycle<A: SyncApi>(
    conn: &Connection,
    api: &A,
    state: &SyncState,
    dry_run: bool,
) -> Result<(), Error> {
    push(conn, api, state, dry_run).await?;
    pull_unsynced(conn, api, dry_run).await?;
    pull_updates(conn, api, state, dry_run).await?;
    Ok(())
}

// ------------------------------------------------------------------
// push: SQLite → Neon
// ------------------------------------------------------------------

pub async fn push<A: SyncApi>(
    conn: &Connection,
    api: &A,
    state: &SyncState,
    dry_run: bool,
) -> Result<(), Error> {
    // 初回起動時 (state.last_push_at が None) は空文字列 → 全件返る。
    let last_push = state.get("last_push_at")?.unwrap_or_default();
    let tasks = sqlite::read_tasks_since(conn, &last_push)?;
    if tasks.is_empty() {
        return Ok(());
    }

    let task_ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
    let reminds_map = sqlite::read_reminds_for_tasks(conn, &task_ids)?;

    let payload: Vec<SyncTask> = tasks
        .iter()
        .map(|t| {
            let reminds = reminds_map.get(&t.id).cloned().unwrap_or_default();
            task_to_sync(t, reminds)
        })
        .collect();

    if dry_run {
        return Ok(());
    }

    api.push_tasks(payload).await?;
    state.set("last_push_at", &now_iso())?;
    Ok(())
}

// ------------------------------------------------------------------
// pull_unsynced: Web 作成タスクの採番
// ------------------------------------------------------------------

pub async fn pull_unsynced<A: SyncApi>(
    conn: &Connection,
    api: &A,
    dry_run: bool,
) -> Result<(), Error> {
    let unsynced = api.get_unsynced().await?;
    for task in unsynced {
        if dry_run {
            continue;
        }
        insert_unsynced_with_patch(conn, api, &task).await?;
    }
    Ok(())
}

/// INSERT one Web-created task locally, PATCH the assigned rowid back to
/// Neon, then commit. PATCH failure rolls the SQLite write back so the
/// next sync cycle can retry from a clean state instead of double-INSERTing.
async fn insert_unsynced_with_patch<A: SyncApi>(
    conn: &Connection,
    api: &A,
    task: &UnsyncedTask,
) -> Result<(), Error> {
    // unchecked_transaction は &Connection から開始できる版。本クレートは
    // SQLite を単一スレッドからのみ触る前提なのでこれで十分。
    let tx = conn.unchecked_transaction()?;

    let sqlite_id = sqlite::insert_task_row(&tx, &unsynced_to_row(task), None)?;

    for r in &task.reminds {
        tx.execute(
            "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, ?2)",
            params![sqlite_id, format_date(*r)],
        )?;
    }

    // PATCH が失敗するとここで `?` で抜けて `tx` が drop され、rusqlite が
    // 自動 ROLLBACK する。Neon 側は task_number=NULL のままなので、次サイ
    // クルで同じ unsynced タスクが再取得され、新しい sqlite_id で再 INSERT
    // / 再 PATCH される (重複行は残らない)。
    api.patch_task_number(task.neon_id, sqlite_id).await?;

    tx.commit()?;
    Ok(())
}

// ------------------------------------------------------------------
// pull_updates: Neon → SQLite (LWW + reminds 全置換)
// ------------------------------------------------------------------

pub async fn pull_updates<A: SyncApi>(
    conn: &Connection,
    api: &A,
    state: &SyncState,
    dry_run: bool,
) -> Result<(), Error> {
    let since = state.get("last_pull_at")?;
    let response = api.get_changes(since.as_deref()).await?;

    for ct in &response.tasks {
        if dry_run {
            continue;
        }
        apply_changed_task(conn, ct)?;
    }

    if !dry_run {
        state.set("last_pull_at", &dt_to_iso(response.server_time))?;
    }
    Ok(())
}

/// Apply one `ChangedTask` to SQLite using row-level LWW.
fn apply_changed_task(conn: &Connection, ct: &ChangedTask) -> Result<(), Error> {
    let local_updated: Option<String> = match conn.query_row(
        "SELECT updated FROM tasks WHERE id = ?1",
        params![ct.task_number],
        |row| row.get::<_, String>(0),
    ) {
        Ok(v) => Some(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };

    let row = changed_to_row(ct);
    match local_updated {
        None => {
            // SQLite に存在しない → id を明示して INSERT (id = task_number)。
            sqlite::insert_task_row(conn, &row, Some(ct.task_number))?;
        }
        Some(local) => {
            let local_date = parse_local_date(&local)?;
            if ct.updated_at.date_naive() > local_date {
                // Neon 勝ち → 上書き
                sqlite::update_task_row(conn, ct.task_number, &row)?;
            } else {
                // SQLite 勝ち → 何も変更しない (reminds も触らない)。
                // 次の push サイクルで Neon 側に local の値が反映される。
                return Ok(());
            }
        }
    }

    // INSERT or UPDATE が走ったら reminds は全置換 (OVERVIEW L258〜261)。
    conn.execute(
        "DELETE FROM task_reminds WHERE task_id = ?1",
        params![ct.task_number],
    )?;
    for r in &ct.reminds {
        conn.execute(
            "INSERT INTO task_reminds (task_id, remind_at) VALUES (?1, ?2)",
            params![ct.task_number, format_date(*r)],
        )?;
    }

    Ok(())
}

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

fn task_to_sync(t: &Task, reminds: Vec<NaiveDate>) -> SyncTask {
    SyncTask {
        task_number: t.id,
        title: t.title.clone(),
        status: t.status.as_str().to_string(),
        source: t.source.clone(),
        project_name: t.project.clone(),
        due: t.due,
        done_at: t.done_at,
        important: t.important,
        updated_at: date_to_dt(t.updated),
        created_at: date_to_dt(t.created),
        reminds,
    }
}

fn unsynced_to_row(u: &UnsyncedTask) -> TaskRow<'_> {
    TaskRow {
        title: &u.title,
        status: &u.status,
        source: &u.source,
        project: u.project_name.as_deref(),
        due: u.due,
        done_at: u.done_at,
        created: u.created_at.date_naive(),
        updated: u.updated_at.date_naive(),
        important: u.important,
    }
}

fn changed_to_row(c: &ChangedTask) -> TaskRow<'_> {
    TaskRow {
        title: &c.title,
        status: &c.status,
        source: &c.source,
        project: c.project_name.as_deref(),
        due: c.due,
        done_at: c.done_at,
        created: c.created_at.date_naive(),
        updated: c.updated_at.date_naive(),
        important: c.important,
    }
}

fn date_to_dt(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0)
        .expect("00:00:00 is a valid wall time")
        .and_utc()
}

fn format_date(d: NaiveDate) -> String {
    d.format(DATE_FMT).to_string()
}

fn parse_local_date(s: &str) -> Result<NaiveDate, Error> {
    NaiveDate::parse_from_str(s, DATE_FMT)
        .map_err(|e| Error::Config(format!("invalid local updated {s:?}: {e}")))
}

fn now_iso() -> String {
    dt_to_iso(Utc::now())
}

fn dt_to_iso(dt: DateTime<Utc>) -> String {
    // Use "Z" suffix instead of "+00:00" so round-trips with the server are
    // stable (server sends "...Z").
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}
