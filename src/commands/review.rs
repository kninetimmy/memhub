use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::commands::{decision, fact};
use crate::db;
use crate::models::{PendingWriteRecord, ReviewExpireSummary};
use crate::sync_md;
use crate::{MemhubError, Result};

pub const DEFAULT_LIST_LIMIT: usize = 25;
pub const DEFAULT_EXPIRY_DAYS: i64 = 30;

/// `writes_log.actor` for the automatic expiry pass in
/// `auto_expire_best_effort` below, distinct from the manual
/// `memhub review expire` command's `cli:user` so the two are
/// distinguishable in the log (Wave 3 Q6).
const AUTO_EXPIRE_ACTOR: &str = "review:auto_expire";

pub fn list(
    start: &Path,
    status_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<PendingWriteRecord>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(
            "review list limit must be greater than zero".to_string(),
        ));
    }
    if let Some(status) = status_filter {
        validate_status_filter(status)?;
    }

    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let rows: Vec<PendingWriteRecord> = match status_filter {
        Some(status) => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                        provenance_json, created_at, reviewed_at
                 FROM pending_writes
                 WHERE project_id = 1 AND status = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            stmt.query_map(params![status, limit as i64], pending_row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                        provenance_json, created_at, reviewed_at
                 FROM pending_writes
                 WHERE project_id = 1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?1",
            )?;
            stmt.query_map(params![limit as i64], pending_row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
    };

    Ok(rows)
}

pub fn show(start: &Path, id: i64) -> Result<PendingWriteRecord> {
    let ctx = db::open_project(start)?;
    let record = ctx
        .conn
        .query_row(
            "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                    provenance_json, created_at, reviewed_at
             FROM pending_writes
             WHERE project_id = 1 AND id = ?1",
            params![id],
            pending_row_to_record,
        )
        .optional()?;

    record.ok_or_else(|| MemhubError::InvalidInput(format!("no pending write with id {id}")))
}

#[derive(Debug)]
pub struct AcceptOutcome {
    pub pending_id: i64,
    pub kind: String,
    pub durable_id: i64,
    pub durable_table: &'static str,
}

pub fn accept(start: &Path, id: i64, actor: &str) -> Result<AcceptOutcome> {
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    // Cloned before the repo tx borrows ctx.conn so a global-targeted
    // accept can consult [global] config + open the global DB without
    // a borrow conflict.
    let config = ctx.config.clone();
    // Same reason: the repo root keys the global accept-marker
    // (replay-safe global accept) and is needed after the repo tx
    // takes ctx.conn.
    let repo_root = ctx.paths.repo_root.clone();
    // Immediate behavior acquires the write lock at BEGIN so concurrent acceptors
    // serialize at the lock instead of both racing past the status check.
    let tx = ctx
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;

    let pending = read_pending_in_tx(&tx, id)?;
    if pending.status != "pending" {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} is already {}; only pending writes can be accepted",
            pending.status
        )));
    }

    let payload: Value = serde_json::from_str(&pending.payload_json).map_err(|err| {
        MemhubError::InvalidInput(format!(
            "pending write {id} has invalid payload_json: {err}"
        ))
    })?;

    let derived_source = derive_source_from_pending_actor(&pending.actor);

    // M9: a proposal tagged `target: "global"` makes its durable write
    // land in `~/.memhub/global.sqlite` instead of the repo. The
    // pending_writes row + status update stay in the repo DB (it is the
    // repo's review queue). The global durable write commits first; if
    // the subsequent repo-side status update fails the proposal simply
    // stays `pending` rather than being lost. Re-accepting is then
    // safe because the global write commits an idempotency marker in
    // the same transaction (see `write_durable_global`) — a replay
    // returns the already-written row instead of duplicating it.
    let target_global = payload.get("target").and_then(Value::as_str) == Some("global");

    let (durable_id, durable_table) = if target_global {
        write_durable_global(
            &config,
            &repo_root,
            id,
            &pending.kind,
            &payload,
            &pending.rationale,
            &derived_source,
            actor,
        )?
    } else {
        match pending.kind.as_str() {
            "fact" => {
                let key = payload
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "key"))?;
                let value = payload
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "value"))?;
                let (fact_id, _) = fact::add_in_tx(&tx, key, value, &derived_source, actor, mode)?;
                (fact_id, "facts")
            }
            "decision" => {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "title"))?;
                let decision_id = decision::add_with_decided_at_in_tx(
                    &tx,
                    title,
                    &pending.rationale,
                    None,
                    None,
                    &derived_source,
                    actor,
                    mode,
                )?;
                (decision_id, "decisions")
            }
            // Staged supersession (Wave 3 L3). The agent could only
            // `propose_supersede`; the durable demote-with-link runs here,
            // now that a human accepted it. The demoted OLD row is the
            // durable target reported back. Repo-scoped only — supersede
            // proposals never carry `target: "global"`.
            "supersede" => {
                let target_kind = payload
                    .get("target_kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "target_kind"))?;
                let old = payload
                    .get("old")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "old"))?;
                let new = payload
                    .get("new")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "new"))?;
                match target_kind {
                    "fact" => {
                        let outcome = fact::supersede_in_tx(&tx, old, new, actor)?;
                        (outcome.old_id, "facts")
                    }
                    "decision" => {
                        let old_id = parse_decision_id(id, "old", old)?;
                        let new_id = parse_decision_id(id, "new", new)?;
                        let outcome = decision::supersede_in_tx(&tx, old_id, new_id, actor)?;
                        (outcome.old_id, "decisions")
                    }
                    other => {
                        return Err(MemhubError::InvalidInput(format!(
                            "pending write {id}: supersede target_kind '{other}' must be 'fact' or 'decision'"
                        )));
                    }
                }
            }
            other => {
                return Err(MemhubError::InvalidInput(format!(
                    "pending write {id} has unknown kind '{other}'"
                )));
            }
        }
    };

    let updated = tx.execute(
        "UPDATE pending_writes
         SET status = 'accepted', reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND id = ?1 AND status = 'pending'",
        params![id],
    )?;
    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} could not be updated; it may have been reviewed concurrently"
        )));
    }

    db::log_write(
        &tx,
        actor,
        "pending_writes",
        Some(id),
        "update",
        &format!("accept pending_write:{id}"),
    )?;

    tx.commit()?;
    sync_md::sync_if_enabled(start)?;

    Ok(AcceptOutcome {
        pending_id: id,
        kind: pending.kind,
        durable_id,
        durable_table,
    })
}

pub fn reject(start: &Path, id: i64, reason: Option<&str>, actor: &str) -> Result<()> {
    let pending = load_pending(start, id)?;
    if pending.status != "pending" {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} is already {}; only pending writes can be rejected",
            pending.status
        )));
    }

    let log_reason = match reason {
        Some(text) if !text.trim().is_empty() => {
            format!("reject pending_write:{id}: {}", text.trim())
        }
        _ => format!("reject pending_write:{id}"),
    };

    mark_status(start, id, "rejected", &log_reason, actor)?;
    Ok(())
}

pub fn expire(start: &Path, older_than_days: i64) -> Result<ReviewExpireSummary> {
    if older_than_days < 0 {
        return Err(MemhubError::InvalidInput(
            "older-than-days must not be negative".to_string(),
        ));
    }

    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let expired = expire_in_conn(&tx, older_than_days, "cli:user")?;
    tx.commit()?;

    Ok(ReviewExpireSummary {
        older_than_days,
        expired,
    })
}

/// Automatic pass called once from `db::open_project` (Wave 3 Q6):
/// expire pending writes older than `DEFAULT_EXPIRY_DAYS` as a cheap,
/// idempotent side effect of every open, so the review queue can't
/// accumulate stale proposals forever even if nobody ever runs
/// `memhub review expire` by hand (PRD §11.3 — "pending writes older
/// than 30 days auto-expire").
///
/// Deliberately a single autocommit statement on `conn` — no explicit
/// transaction wrapper — so this never holds the write lock any
/// longer than one `UPDATE`, and can't fight a concurrent writer's own
/// transaction the way a longer-held explicit transaction could.
///
/// Best-effort, matching the registry/metrics side effects this runs
/// alongside in `open_project`: a failure here (e.g. a transient
/// `SQLITE_BUSY` under contention) is logged and swallowed rather than
/// failing an otherwise successful host command. Nothing is printed on
/// success, and a `writes_log` row is only written when a row actually
/// changed (see `expire_in_conn`), so a no-op pass — the overwhelming
/// majority of calls — adds no output and no log noise.
pub fn auto_expire_best_effort(conn: &Connection) {
    if let Err(err) = expire_in_conn(conn, DEFAULT_EXPIRY_DAYS, AUTO_EXPIRE_ACTOR) {
        log::warn!("automatic pending-write expiry failed: {err}");
    }
}

/// Shared expiry core: mark `pending` rows older than `older_than_days`
/// as `expired`, logging one `writes_log` row under `actor` iff
/// anything actually changed. This is the ONLY copy of the expiry SQL —
/// `expire` (manual) and `auto_expire_best_effort` (automatic) both
/// call it rather than forking their own copy. `conn` accepts either a
/// bare `Connection` (autocommit — the automatic pass) or a
/// `Transaction` (via its `Deref<Target = Connection>` — the manual
/// command, which wraps the update and the log write together).
fn expire_in_conn(conn: &Connection, older_than_days: i64, actor: &str) -> Result<usize> {
    let cutoff_expr = format!("datetime('now', '-{older_than_days} days')");
    let update_sql = format!(
        "UPDATE pending_writes
         SET status = 'expired', reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND status = 'pending' AND created_at < {cutoff_expr}"
    );
    let expired = conn.execute(&update_sql, [])?;

    if expired > 0 {
        db::log_write(
            conn,
            actor,
            "pending_writes",
            None,
            "update",
            &format!("expire {expired} pending writes older than {older_than_days} days"),
        )?;
    }

    Ok(expired)
}

fn load_pending(start: &Path, id: i64) -> Result<PendingWriteRecord> {
    show(start, id)
}

fn read_pending_in_tx(tx: &Transaction<'_>, id: i64) -> Result<PendingWriteRecord> {
    let record = tx
        .query_row(
            "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                    provenance_json, created_at, reviewed_at
             FROM pending_writes
             WHERE project_id = 1 AND id = ?1",
            params![id],
            pending_row_to_record,
        )
        .optional()?;

    record.ok_or_else(|| MemhubError::InvalidInput(format!("no pending write with id {id}")))
}

fn mark_status(start: &Path, id: i64, new_status: &str, reason: &str, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let updated = tx.execute(
        "UPDATE pending_writes
         SET status = ?1, reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND id = ?2 AND status = 'pending'",
        params![new_status, id],
    )?;

    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} could not be updated; it may have been reviewed concurrently"
        )));
    }

    db::log_write(&tx, actor, "pending_writes", Some(id), "update", reason)?;

    tx.commit()?;
    Ok(())
}

fn pending_row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingWriteRecord> {
    Ok(PendingWriteRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        payload_json: row.get(2)?,
        rationale: row.get(3)?,
        status: row.get(4)?,
        actor: row.get(5)?,
        actor_raw: row.get(6)?,
        provenance_json: row.get(7)?,
        created_at: row.get(8)?,
        reviewed_at: row.get(9)?,
    })
}

fn validate_status_filter(status: &str) -> Result<()> {
    match status {
        "pending" | "accepted" | "rejected" | "expired" => Ok(()),
        other => Err(MemhubError::InvalidInput(format!(
            "unknown pending write status '{other}'; expected one of pending|accepted|rejected|expired"
        ))),
    }
}

fn missing_payload_field(id: i64, field: &str) -> MemhubError {
    MemhubError::InvalidInput(format!(
        "pending write {id} payload is missing required field '{field}'"
    ))
}

/// Parse a decision supersede identifier (numeric id) from a staged
/// payload, with a clear error naming the offending field. Decisions have
/// no natural key, so a supersede proposal must carry numeric ids.
fn parse_decision_id(id: i64, field: &str, raw: &str) -> Result<i64> {
    raw.trim().parse::<i64>().map_err(|_| {
        MemhubError::InvalidInput(format!(
            "pending write {id}: decision supersede '{field}' must be a numeric id, got '{raw}'"
        ))
    })
}

/// Durable write for a `target: "global"` pending write. Runs in the
/// global DB's own transaction (committed here) so the repo-side
/// status update can follow. Requires the accepting repo to still
/// have `[global] enabled` — accepting a global proposal is itself a
/// machine-global write and obeys the same per-repo gate.
#[allow(clippy::too_many_arguments)]
fn write_durable_global(
    config: &crate::config::ProjectConfig,
    repo_root: &Path,
    pending_id: i64,
    kind: &str,
    payload: &Value,
    rationale: &str,
    derived_source: &str,
    actor: &str,
) -> Result<(i64, &'static str)> {
    crate::commands::global::ensure_enabled(config)?;
    let mode = config.retrieval.mode;
    let mut g = db::open_global()?;
    let gtx = g.conn.transaction()?;

    // Replay guard. A `(repo_key, pending_id)` marker is committed in
    // THIS transaction alongside the durable write below, so if a prior
    // accept committed the global side but lost the repo-side status
    // flip (crash / repo-DB I/O error in the cross-DB window), the
    // proposal is still `pending` and gets re-accepted. Without this,
    // re-accepting a global *decision* would insert a second global row
    // (facts are protected by key upsert; decisions have no natural
    // key) and a bad global write poisons every repo. On a hit we
    // return the already-written row and let the caller retry only the
    // repo-side update.
    let repo_key = crate::db::registry::canonical(repo_root);
    if let Some((table_str, durable_id)) = gtx
        .query_row(
            "SELECT durable_table, durable_id
             FROM global_accept_markers
             WHERE repo_key = ?1 AND pending_id = ?2",
            params![repo_key, pending_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
    {
        return Ok((durable_id, static_durable_table(&table_str)?));
    }

    let (durable_id, durable_table): (i64, &'static str) = match kind {
        "fact" => {
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(pending_id, "key"))?;
            let value = payload
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(pending_id, "value"))?;
            let (fact_id, _) = fact::add_in_tx(&gtx, key, value, derived_source, actor, mode)?;
            (fact_id, "facts")
        }
        "decision" => {
            let title = payload
                .get("title")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(pending_id, "title"))?;
            let decision_id = decision::add_with_decided_at_in_tx(
                &gtx,
                title,
                rationale,
                None,
                None,
                derived_source,
                actor,
                mode,
            )?;
            (decision_id, "decisions")
        }
        other => {
            return Err(MemhubError::InvalidInput(format!(
                "pending write {pending_id} has unknown kind '{other}'"
            )));
        }
    };

    // Idempotency marker, committed atomically with the durable row
    // above. After this commit a replayed accept takes the early
    // return at the top of this function instead of writing again.
    gtx.execute(
        "INSERT INTO global_accept_markers
            (repo_key, pending_id, durable_table, durable_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![repo_key, pending_id, durable_table, durable_id],
    )?;

    db::log_write(
        &gtx,
        actor,
        durable_table,
        Some(durable_id),
        "promote",
        &format!("accept pending_write:{pending_id} → global"),
    )?;
    gtx.commit()?;
    Ok((durable_id, durable_table))
}

/// Map a marker's stored `durable_table` string back to the
/// `&'static str` the accept path returns. Only `facts`/`decisions`
/// are ever written as global durable rows, so anything else is a
/// corrupt marker.
fn static_durable_table(name: &str) -> Result<&'static str> {
    match name {
        "facts" => Ok("facts"),
        "decisions" => Ok("decisions"),
        other => Err(MemhubError::InvalidInput(format!(
            "global accept marker has unrecognized durable_table '{other}'"
        ))),
    }
}

/// Compose the durable `source` value for a pending write being accepted.
///
/// `pending_writes.actor` holds the normalized client identity (e.g. `codex`,
/// `claude-code`) captured at MCP `initialize` time, or a free-form CLI actor
/// string. When an agent's claim is accepted by an operator, both signals
/// matter — the operator endorsed it, and the agent surfaced it. See
/// `docs/reference/memhub-prd-source-vocabulary-addendum.md` §2.
fn derive_source_from_pending_actor(actor: &str) -> String {
    match actor {
        "" | "user" | "unknown" => "user".to_string(),
        agent => format!("user+agent:{agent}"),
    }
}

#[cfg(test)]
mod tests {
    use super::derive_source_from_pending_actor;

    #[test]
    fn derive_source_handles_user_and_unknown() {
        assert_eq!(derive_source_from_pending_actor("user"), "user");
        assert_eq!(derive_source_from_pending_actor("unknown"), "user");
        assert_eq!(derive_source_from_pending_actor(""), "user");
    }

    #[test]
    fn derive_source_composes_agent_identities() {
        assert_eq!(
            derive_source_from_pending_actor("codex"),
            "user+agent:codex"
        );
        assert_eq!(
            derive_source_from_pending_actor("claude-code"),
            "user+agent:claude-code"
        );
    }

    // Wave 3 L3 — the MCP supersede surface is propose-only. A staged
    // `propose_supersede` does NOT mutate durable state; the demote-with-link
    // becomes durable only when a human `review accept` runs it here.
    #[test]
    fn accept_supersede_fact_is_staged_then_durable() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) =
            fact::add(temp.path(), "k-old", "v old", "user", "cli:user").expect("old");
        let (new_id, _) =
            fact::add(temp.path(), "k-new", "v new", "user", "cli:user").expect("new");

        let pid = pending_write::propose_supersede(
            temp.path(),
            "fact",
            &old_id.to_string(),
            &new_id.to_string(),
            "old replaced by new",
            "codex",
            "codex",
            "{}",
        )
        .expect("stage");

        // Untrusted-writer guardrail: staging must not durably supersede.
        assert_eq!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.id == old_id)
                .unwrap()
                .superseded_by,
            None,
            "propose_supersede must not durably mutate the fact"
        );

        let outcome = super::accept(temp.path(), pid, "cli:user").expect("accept");
        assert_eq!(outcome.durable_table, "facts");
        assert_eq!(outcome.durable_id, old_id);
        assert_eq!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.id == old_id)
                .unwrap()
                .superseded_by,
            Some(new_id),
            "accept applies the durable demote-with-link"
        );
    }

    #[test]
    fn accept_supersede_decision_is_durable() {
        use crate::commands::{decision, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let old_id = decision::add(temp.path(), "Old", "r1", "user", "cli:user").expect("old");
        let new_id = decision::add(temp.path(), "New", "r2", "user", "cli:user").expect("new");

        let pid = pending_write::propose_supersede(
            temp.path(),
            "decision",
            &old_id.to_string(),
            &new_id.to_string(),
            "replaced",
            "claude-code",
            "claude-code",
            "{}",
        )
        .expect("stage");
        let outcome = super::accept(temp.path(), pid, "cli:user").expect("accept");
        assert_eq!(outcome.durable_table, "decisions");
        assert_eq!(outcome.durable_id, old_id);

        let all = decision::list(temp.path()).expect("list");
        let old = all.iter().find(|d| d.id == old_id).unwrap();
        assert_eq!(old.status, "superseded");
        assert_eq!(old.superseded_by, Some(new_id));
    }
}
