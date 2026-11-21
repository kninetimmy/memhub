use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::commands::{decision, fact};
use crate::db;
use crate::models::{PendingWriteRecord, ReviewExpireSummary};
use crate::retrieval::rerank;
use crate::{MemhubError, Result};

pub const DEFAULT_LIST_LIMIT: usize = 25;
pub const DEFAULT_EXPIRY_DAYS: i64 = 30;

/// `writes_log.actor` for the automatic expiry pass in
/// `auto_expire_best_effort` below, distinct from the manual
/// `memhub review expire` command's `cli:user` so the two are
/// distinguishable in the log (Wave 3 Q6).
const AUTO_EXPIRE_ACTOR: &str = "review:auto_expire";

/// Cross-encoder relevance cutoff for the accept-time probe. The original L5
/// calibration (issue #48, decision 147) used 2.0 for the bundled
/// ms-marco-MiniLM reranker, re-verified under int8 (issue #75). Re-verify
/// it on a model swap. The model scores overlap, not contradiction:
/// compatible same-subject rows can also clear the cutoff. Before #443,
/// reranked hits blocked plain fact and decision accepts. Now a fact hit
/// warns; a decision hit still blocks. This cutoff does not feed recall.
const ACCEPT_SIMILARITY_RERANK_THRESHOLD: f32 = 2.0;

/// Upper bound on same-kind rows the reranked probe scores at accept time.
/// Accept is human-gated and infrequent, but a pathologically large corpus
/// should not make it crawl; the most recent [`PROBE_CANDIDATE_LIMIT`] rows
/// are compared (the same-key fact signal is exact and unbounded).
const PROBE_CANDIDATE_LIMIT: i64 = 200;

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
    pub fact_similarity_warning: Option<String>,
}

/// Protect a plain fact accept from overwriting a different value under the
/// same active key. Exact and model-free; runs before any similarity probe.
fn check_same_key_overwrite(tx: &Transaction<'_>, id: i64, key: &str, value: &str) -> Result<()> {
    if let Some((existing_id, existing_value, false)) = fact::find_writable_fact(tx, key)?
        && existing_value != value
    {
        return Err(MemhubError::InvalidInput(format!(
            "accept blocked: fact '{key}' (#{existing_id}) already holds a different value, so \
             accepting pending write {id} would silently overwrite it. Re-run `memhub review \
             accept {id} --force` to overwrite anyway (the prior value is logged), or reject \
             this proposal."
        )));
    }
    Ok(())
}

/// One existing row the reranked probe scores the incoming payload against.
struct ProbeCandidate {
    source_id: i64,
    /// Reranker doc text, built with the same embed-text helpers recall uses
    /// so the cross-encoder sees the same shape it was calibrated on.
    text: String,
    label: String,
}

/// Find the single best same-kind match if it clears the relevance cutoff.
/// This is a read-only probe and is skipped if the reranker is disabled.
fn probe_similarity(
    tx: &Transaction<'_>,
    kind: &str,
    incoming_query: &str,
    exclude_fact_key: Option<&str>,
    use_reranker: bool,
) -> Result<Option<ProbeCandidate>> {
    if !use_reranker {
        return Ok(None);
    }
    let candidates = gather_probe_candidates(tx, kind, exclude_fact_key)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let docs: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
    // `rerank` returns (input_index, score) sorted by descending relevance,
    // so the first entry is the best-matching existing row.
    let scored = rerank::rerank(incoming_query, &docs)?;
    if let Some(&(idx, score)) = scored.first()
        && score >= ACCEPT_SIMILARITY_RERANK_THRESHOLD
    {
        return Ok(candidates.into_iter().nth(idx));
    }
    Ok(None)
}

/// Gather the same-kind, non-superseded, non-retired rows the reranked probe scores
/// against, most-recent first and capped at [`PROBE_CANDIDATE_LIMIT`]. For
/// facts, `exclude_fact_key` drops the incoming key's own row (handled by the
/// same-key signal, and never a contradiction with itself).
fn gather_probe_candidates(
    tx: &Transaction<'_>,
    kind: &str,
    exclude_fact_key: Option<&str>,
) -> Result<Vec<ProbeCandidate>> {
    let mut out = Vec::new();
    match kind {
        "fact" => {
            let mut stmt = tx.prepare(
                "SELECT id, key, value FROM facts
                 WHERE project_id = 1 AND superseded_by IS NULL AND retired_at IS NULL
                 ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![PROBE_CANDIDATE_LIMIT], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (row_id, key, value) = row?;
                if exclude_fact_key == Some(key.as_str()) {
                    continue;
                }
                out.push(ProbeCandidate {
                    source_id: row_id,
                    text: crate::retrieval::fact_embed_text(&key, &value),
                    label: format!("fact {key:?} (#{row_id})"),
                });
            }
        }
        "decision" => {
            let mut stmt = tx.prepare(
                "SELECT id, title, rationale, summary FROM decisions
                 WHERE project_id = 1 AND superseded_by IS NULL AND status = 'active'
                 ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![PROBE_CANDIDATE_LIMIT], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (row_id, title, rationale, summary) = row?;
                out.push(ProbeCandidate {
                    source_id: row_id,
                    text: crate::retrieval::decision_embed_text(
                        &title,
                        &rationale,
                        summary.as_deref(),
                    ),
                    label: format!("decision #{row_id} '{title}'"),
                });
            }
        }
        // Only fact/decision payloads reach the probe; other kinds gather nothing.
        _ => {}
    }
    Ok(out)
}

/// Accept a pending write. On plain repo accepts, exact fact overwrites and
/// similar decisions block; similar facts under different keys only warn.
/// `--force` skips those checks; `--supersede` also links the named repo row
/// to its replacement, whether or not the probe would have matched it.
pub fn accept(
    start: &Path,
    id: i64,
    actor: &str,
    supersede: Option<&str>,
    force: bool,
) -> Result<AcceptOutcome> {
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

    // The accept-time checks and the `--supersede` link are repo-scoped
    // (issue #48). Rather than silently ignore a `--supersede` handed to a
    // global-targeted accept, refuse loudly so the operator isn't misled into
    // thinking a global row was retired.
    if target_global && supersede.is_some() {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} targets global memory; --supersede at accept time is repo-scoped \
             (Wave 3 L5). Accept it without --supersede."
        )));
    }

    let mut fact_similarity_warning = None;
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
                // Optional, unenforced tag staged by `propose_fact` (issue
                // #97); absent from every payload written before this
                // field existed, which `Value::as_str` on a missing key
                // already treats as `None`.
                let kind = payload.get("kind").and_then(Value::as_str);
                let similar_fact = if !force && supersede.is_none() {
                    check_same_key_overwrite(&tx, id, key, value)?;
                    probe_similarity(
                        &tx,
                        "fact",
                        &crate::retrieval::fact_embed_text(key, value),
                        Some(key),
                        config.retrieval.use_reranker,
                    )?
                } else {
                    None
                };
                let (fact_id, _) =
                    fact::add_with_kind_in_tx(&tx, key, value, kind, &derived_source, actor, mode)?;
                if let Some(candidate) = similar_fact {
                    fact_similarity_warning = Some(format!(
                        "similarity match with existing {} (different key); \
                         no supersession link was created.",
                        candidate.label
                    ));
                }
                // Honor --supersede whenever supplied: supersede the named row in
                // favor of the fact just written (L3 demote-with-link). A
                // same-key overwrite yields the same row id, which
                // `supersede_in_tx` rejects as self-supersede — the correct,
                // loud outcome (use --force for a same-key overwrite).
                if let Some(old_ident) = supersede {
                    fact::supersede_in_tx(&tx, old_ident, &fact_id.to_string(), actor)?;
                }
                (fact_id, "facts")
            }
            "decision" => {
                let title = payload
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| missing_payload_field(id, "title"))?;
                // Decisions have no natural key, so only the reranked signal
                // applies. The cross-encoder catches
                // near-duplicate decisions; a divergently-worded conflict may
                // not trip it — advisory, best-effort.
                if !force
                    && supersede.is_none()
                    && let Some(candidate) = probe_similarity(
                        &tx,
                        "decision",
                        &crate::retrieval::decision_embed_text(title, &pending.rationale, None),
                        None,
                        config.retrieval.use_reranker,
                    )?
                {
                    return Err(MemhubError::InvalidInput(format!(
                        "accept blocked: pending write {id} looks like it contradicts existing {}. \
                         Re-run `memhub review accept {id} --supersede {}` to retire that row in \
                         favor of this write, or `--force` to keep both.",
                        candidate.label, candidate.source_id
                    )));
                }
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
                // Honor --supersede: retire the named decision (numeric id
                // only — decisions have no natural key) in favor of the one
                // just written (L3 demote-with-link).
                if let Some(old_ident) = supersede {
                    let old_id = old_ident.trim().parse::<i64>().map_err(|_| {
                        MemhubError::InvalidInput(format!(
                            "--supersede for a decision must be a numeric decision id, got \
                             '{old_ident}'"
                        ))
                    })?;
                    decision::supersede_in_tx(&tx, old_id, decision_id, actor)?;
                }
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

    Ok(AcceptOutcome {
        pending_id: id,
        kind: pending.kind,
        durable_id,
        durable_table,
        fact_similarity_warning,
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
            // Optional, unenforced tag staged by `propose_fact` (issue
            // #97). Named `fact_kind` to avoid shadowing this function's
            // own `kind: &str` pending-write-kind parameter.
            let fact_kind = payload.get("kind").and_then(Value::as_str);
            let (fact_id, _) = fact::add_with_kind_in_tx(
                &gtx,
                key,
                value,
                fact_kind,
                derived_source,
                actor,
                mode,
            )?;
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
    use super::*;

    #[test]
    fn retired_facts_are_not_similarity_candidates_and_cannot_be_overwritten() {
        let repo = tempfile::tempdir().unwrap();
        crate::commands::init::run(repo.path()).unwrap();
        let mut ctx = db::open_project(repo.path()).unwrap();
        ctx.conn
            .execute_batch(
                "INSERT INTO facts(id, key, value) VALUES (1, 'active', 'value');
                 INSERT INTO facts(id, key, value, retired_at, retired_reason)
                 VALUES (2, 'retired', 'prior', CURRENT_TIMESTAMP, 'old');",
            )
            .unwrap();
        let tx = ctx.conn.transaction().unwrap();
        let candidates = gather_probe_candidates(&tx, "fact", None).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_id, 1);
        tx.rollback().unwrap();
        drop(ctx);

        let id = crate::commands::pending_write::propose_fact(
            repo.path(),
            "retired",
            "new",
            "changed",
            "codex",
            "openai-codex",
            "{}",
        )
        .unwrap();
        for (supersede, force) in [(None, false), (None, true), (Some("active"), false)] {
            let err = accept(repo.path(), id, "cli:user", supersede, force).unwrap_err();
            assert!(err.to_string().contains("memhub fact unretire"), "{err}");
        }
        assert_eq!(show(repo.path(), id).unwrap().status, "pending");
        let facts = fact::list_with_retired(repo.path(), true).unwrap();
        assert_eq!(facts[1].value, "prior");
        assert_eq!(facts[1].retired_reason.as_deref(), Some("old"));
    }

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

        let outcome = super::accept(temp.path(), pid, "cli:user", None, false).expect("accept");
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
        let outcome = super::accept(temp.path(), pid, "cli:user", None, false).expect("accept");
        assert_eq!(outcome.durable_table, "decisions");
        assert_eq!(outcome.durable_id, old_id);

        let all = decision::list(temp.path()).expect("list");
        let old = all.iter().find(|d| d.id == old_id).unwrap();
        assert_eq!(old.status, "superseded");
        assert_eq!(old.superseded_by, Some(new_id));
    }

    // -- accept-time overwrite protection and similarity probe ---------

    // Same-key signal (deterministic, model-free): an existing fact with the
    // same key but a different value blocks a plain accept.
    #[test]
    fn accept_blocks_on_same_key_fact_contradiction() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "deploy-cmd",
            "kubectl apply v1",
            "user",
            "cli:user",
        )
        .expect("v1");
        let pid = pending_write::propose_fact(
            temp.path(),
            "deploy-cmd",
            "kubectl apply v2",
            "conflicting update",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");
        let ctx = db::open_project(temp.path()).expect("open");
        let write_count = || {
            ctx.conn
                .query_row("SELECT count(*) FROM writes_log", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count writes")
        };
        let before = write_count();

        let err = super::accept(temp.path(), pid, "cli:user", None, false)
            .expect_err("same-key contradiction must block");
        let msg = err.to_string();
        assert!(
            msg.contains("deploy-cmd"),
            "advisory must name the row: {msg}"
        );
        assert!(
            msg.contains("--force"),
            "advisory must name the escape: {msg}"
        );

        // Durable value untouched, and the proposal stays pending (retryable).
        assert_eq!(write_count(), before);
        assert_eq!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.key == "deploy-cmd")
                .unwrap()
                .value,
            "kubectl apply v1"
        );
        assert_eq!(
            super::show(temp.path(), pid).expect("show").status,
            "pending"
        );
    }

    // The `--force` escape proceeds with the overwrite, and the prior value is
    // recoverable from writes_log (the L5 prior-value logging).
    #[test]
    fn accept_same_key_fact_contradiction_proceeds_with_force() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "deploy-cmd",
            "kubectl apply v1",
            "user",
            "cli:user",
        )
        .expect("v1");
        let pid = pending_write::propose_fact(
            temp.path(),
            "deploy-cmd",
            "kubectl apply v2",
            "update",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let outcome =
            super::accept(temp.path(), pid, "cli:user", None, true).expect("force accept");
        assert_eq!(outcome.durable_table, "facts");
        assert!(outcome.fact_similarity_warning.is_none());
        assert_eq!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.key == "deploy-cmd")
                .unwrap()
                .value,
            "kubectl apply v2"
        );
        let ctx = db::open_project(temp.path()).expect("open");
        let reason: String = ctx
            .conn
            .query_row(
                "SELECT reason FROM writes_log
                 WHERE table_name = 'facts' AND action = 'update' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("update reason");
        assert!(
            reason.contains("kubectl apply v1"),
            "prior value must be recoverable from the log: {reason}"
        );
    }

    // Explicit --supersede still applies the L3 demote-with-link and skips
    // the similarity probe.
    #[test]
    fn accept_fact_with_supersede_applies_link() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) = fact::add(
            temp.path(),
            "prod-db-host",
            "db-alpha.example.com",
            "user",
            "cli:user",
        )
        .expect("existing");
        let pid = pending_write::propose_fact(
            temp.path(),
            "database-host",
            "db-beta.example.com",
            "new host",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let outcome = super::accept(
            temp.path(),
            pid,
            "cli:user",
            Some(&old_id.to_string()),
            false,
        )
        .expect("supersede accept");
        assert_eq!(outcome.durable_table, "facts");
        assert!(outcome.fact_similarity_warning.is_none());
        let facts = fact::list(temp.path()).expect("list");
        let new = facts
            .iter()
            .find(|f| f.key == "database-host")
            .expect("new fact durable");
        let old = facts
            .iter()
            .find(|f| f.id == old_id)
            .expect("old fact still present (no-loss)");
        assert_eq!(
            old.superseded_by,
            Some(new.id),
            "old fact demoted-with-link to the new one"
        );
    }

    // An unrelated fact must not warn. Loads the bundled reranker.
    #[test]
    fn accept_unrelated_fact_proceeds_without_flags() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
        .expect("f1");
        fact::add(temp.path(), "favorite-color", "blue", "user", "cli:user").expect("f2");
        let pid = pending_write::propose_fact(
            temp.path(),
            "onboarding-url",
            "https://wiki.example.com/onboard",
            "reference link",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let outcome = super::accept(temp.path(), pid, "cli:user", None, false)
            .expect("an unrelated proposal must proceed");
        assert_eq!(outcome.durable_table, "facts");
        assert!(outcome.fact_similarity_warning.is_none());
        assert!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .any(|f| f.key == "onboarding-url")
        );
    }

    // Reranked signal for decisions (near-duplicate). Decisions have no
    // natural key, so only the reranked signal applies. Loads the model.
    #[test]
    fn accept_blocks_on_reranked_decision_contradiction() {
        use crate::commands::{decision, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        decision::add(
            temp.path(),
            "Adopt Postgres as the datastore",
            "We will run a Postgres server to hold project memory.",
            "user",
            "cli:user",
        )
        .expect("existing");
        let pid = pending_write::propose_decision(
            temp.path(),
            "Adopt Postgres as the datastore",
            "We will run a managed Postgres server to hold project memory.",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let err = super::accept(temp.path(), pid, "cli:user", None, false)
            .expect_err("near-duplicate decision must block");
        let msg = err.to_string();
        assert!(
            msg.contains("Postgres"),
            "advisory must name the conflicting decision: {msg}"
        );
        assert!(
            msg.contains("--supersede"),
            "decision advisory offers --supersede: {msg}"
        );
        // The blocked accept wrote nothing durable.
        assert_eq!(decision::list(temp.path()).expect("list").len(), 1);
    }

    // `--supersede` on a decision accept applies the link deterministically
    // (supersede present ⇒ probe skipped, so no model load).
    #[test]
    fn accept_decision_with_supersede_applies_link() {
        use crate::commands::{decision, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let old_id = decision::add(
            temp.path(),
            "Use JSON config",
            "chosen early",
            "user",
            "cli:user",
        )
        .expect("old");
        let pid = pending_write::propose_decision(
            temp.path(),
            "Use TOML config",
            "switch to TOML for inline comments",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let outcome = super::accept(
            temp.path(),
            pid,
            "cli:user",
            Some(&old_id.to_string()),
            false,
        )
        .expect("decision supersede accept");
        assert_eq!(outcome.durable_table, "decisions");
        let all = decision::list(temp.path()).expect("list");
        let old = all.iter().find(|d| d.id == old_id).unwrap();
        assert_eq!(old.status, "superseded");
        assert_eq!(
            old.superseded_by,
            Some(outcome.durable_id),
            "old decision links to the newly accepted one"
        );
    }

    // A non-numeric --supersede for a decision fails loudly (numeric id only).
    #[test]
    fn accept_decision_rejects_non_numeric_supersede() {
        use crate::commands::{decision, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        decision::add(temp.path(), "Prior", "r", "user", "cli:user").expect("prior");
        let pid = pending_write::propose_decision(
            temp.path(),
            "New choice",
            "rationale",
            "codex",
            "codex",
            "{}",
        )
        .expect("propose");

        let err = super::accept(temp.path(), pid, "cli:user", Some("not-a-number"), false)
            .expect_err("non-numeric decision supersede must fail");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }
}
