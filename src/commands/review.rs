use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::commands::{decision, fact};
use crate::db;
use crate::models::{PendingWriteRecord, ReviewExpireSummary};
use crate::retrieval::rerank;
use crate::retrieval::util::sha256_hex;
use crate::sync_md;
use crate::{MemhubError, Result};

pub const DEFAULT_LIST_LIMIT: usize = 25;
pub const DEFAULT_EXPIRY_DAYS: i64 = 30;

/// `writes_log.actor` for the automatic expiry pass in
/// `auto_expire_best_effort` below, distinct from the manual
/// `memhub review expire` command's `cli:user` so the two are
/// distinguishable in the log (Wave 3 Q6).
const AUTO_EXPIRE_ACTOR: &str = "review:auto_expire";

/// Cutoff (in days, by `tasks.updated_at`) for the "done tasks older
/// than N" category in `memhub review stale` (Wave 3 L4, issue #47). A
/// plain constant rather than a config knob: L4's own cost estimate was
/// "one command, no migration" — unlike `fact_stale_after_days` this
/// number never feeds recall scoring, so it does not warrant a new
/// config surface (and the doctor `KNOWN_LEAVES`/`BASELINE_FIELDS`
/// upkeep that comes with one).
pub const DONE_TASK_STALE_DAYS: i64 = 30;

/// Cross-encoder relevance logit at/above which an existing durable row is
/// treated as a contradiction candidate for the payload being accepted (Wave
/// 3 L5, issue #48). Calibrated against the bundled ms-marco-MiniLM reranker:
/// a genuine near-duplicate / same-subject row scores ~+5.6 (facts) to ~+9.0
/// (decisions), while merely-related or unrelated rows sit near −10, so 2.0 —
/// the same relevance floor `[retrieval.scoring] min_rerank_score` already
/// uses — lands in that wide gap with margin on both sides. Because the model
/// scores lexical+semantic *overlap*, this reliably catches restatements and
/// near-duplicates; it is not a general contradiction classifier (a rephrased
/// conflict that shares few tokens will not trip it). A plain constant, not a
/// config knob: like [`DONE_TASK_STALE_DAYS`] it never feeds recall scoring,
/// so it doesn't warrant a config surface (or the doctor baseline upkeep a
/// knob brings). The probe is advisory — a hit only yields a single refusal.
/// Re-verified under the int8-quantized reranker (issue #75 / decision 147,
/// a reranker swap): the reranked-contradiction accept tests still pass, so
/// contradictory rows stay at/above 2.0 and unrelated rows well below it.
/// Unchanged.
const CONTRADICTION_RERANK_THRESHOLD: f32 = 2.0;

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
}

// ---------------------------------------------------------------------
// Accept-time contradiction probe (Wave 3 L5, issue #48). Advisory only:
// it never mutates state and never auto-resolves — a detected conflict
// yields ONE refusal naming the row, and the operator re-runs `review
// accept` with `--supersede <id>` (retire the old row, reusing L3's
// demote-with-link) or `--force` (proceed anyway; for facts the prior
// value is logged by `fact::add_in_tx`). Reuses the existing cross-encoder
// reranker seam rather than a new model.
// ---------------------------------------------------------------------

/// How an accepted payload collided with an existing durable row.
#[derive(Debug, Clone, Copy)]
enum ContradictionKind {
    /// A fact with the same key already holds a *different* value. Accepting
    /// silently overwrites it (last-writer-wins). `--supersede` cannot apply
    /// (a row cannot supersede itself), so `--force` is the escape.
    SameKey,
    /// A *different* existing same-kind row reranked at/above
    /// [`CONTRADICTION_RERANK_THRESHOLD`] against the incoming payload — a
    /// near-duplicate / same-subject candidate. `--supersede <id>` (retire it
    /// in favor of this write) or `--force` (keep both) proceeds.
    Reranked,
}

/// A single conflicting row surfaced by [`probe_contradiction`].
#[derive(Debug)]
struct Contradiction {
    kind: ContradictionKind,
    source_id: i64,
    /// Human label naming the conflicting row, e.g. `fact 'deploy-cmd' (#3)`.
    label: String,
}

impl Contradiction {
    /// The single advisory message a blocked accept returns (spec: "refuse
    /// with a single advisory prompt naming the conflicting row"). One shot,
    /// never a loop — the operator re-runs with a flag.
    fn advisory(&self, pending_id: i64) -> String {
        match self.kind {
            ContradictionKind::SameKey => format!(
                "accept blocked: {} already holds a different value, so accepting pending write \
                 {pending_id} would silently overwrite it. Re-run `memhub review accept \
                 {pending_id} --force` to overwrite anyway (the prior value is logged), or reject \
                 this proposal.",
                self.label
            ),
            ContradictionKind::Reranked => format!(
                "accept blocked: pending write {pending_id} looks like it contradicts existing {}. \
                 Re-run `memhub review accept {pending_id} --supersede {}` to retire that row in \
                 favor of this write, or `--force` to keep both.",
                self.label, self.source_id
            ),
        }
    }
}

/// One existing row the reranked probe scores the incoming payload against.
struct ProbeCandidate {
    source_id: i64,
    /// Reranker doc text, built with the same embed-text helpers recall uses
    /// so the cross-encoder sees the same shape it was calibrated on.
    text: String,
    label: String,
}

/// Probe the incoming payload against existing durable rows for a likely
/// contradiction. Returns the first conflict found (same-key beats reranked)
/// or `None`. Pure read: prepares SELECTs and may run the reranker, but never
/// writes.
///
/// `same_key` is `Some((key, value))` for facts (enables the exact same-key
/// signal) and `None` for decisions. The reranked signal is gated on
/// `use_reranker` so an operator who disabled the cross-encoder for recall is
/// not forced to load it here; the same-key signal always runs.
fn probe_contradiction(
    tx: &Transaction<'_>,
    kind: &str,
    incoming_query: &str,
    same_key: Option<(&str, &str)>,
    use_reranker: bool,
) -> Result<Option<Contradiction>> {
    // 1. Same-key (facts): an existing, non-superseded fact with this key but
    //    a different value. Exact and model-free — the strongest signal.
    if let Some((key, value)) = same_key {
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, value FROM facts
                 WHERE project_id = 1 AND key = ?1 AND superseded_by IS NULL",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((existing_id, existing_value)) = existing
            && existing_value != value
        {
            return Ok(Some(Contradiction {
                kind: ContradictionKind::SameKey,
                source_id: existing_id,
                label: format!("fact '{key}' (#{existing_id})"),
            }));
        }
    }

    // 2. Reranked semantic signal over the remaining same-kind rows.
    if !use_reranker {
        return Ok(None);
    }
    let candidates = gather_probe_candidates(tx, kind, same_key.map(|(k, _)| k))?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let docs: Vec<String> = candidates.iter().map(|c| c.text.clone()).collect();
    // `rerank` returns (input_index, score) sorted by descending relevance,
    // so the first entry is the best-matching existing row.
    let scored = rerank::rerank(incoming_query, &docs)?;
    if let Some(&(idx, score)) = scored.first()
        && score >= CONTRADICTION_RERANK_THRESHOLD
        && let Some(candidate) = candidates.get(idx)
    {
        return Ok(Some(Contradiction {
            kind: ContradictionKind::Reranked,
            source_id: candidate.source_id,
            label: candidate.label.clone(),
        }));
    }
    Ok(None)
}

/// Gather the same-kind, non-superseded rows the reranked probe scores
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
                 WHERE project_id = 1 AND superseded_by IS NULL
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
                    label: format!("fact '{key}' (#{row_id})"),
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

/// Accept a pending write. `supersede` (a fact id/key or a decision id) and
/// `force` are the two escapes for the accept-time contradiction probe (issue
/// #48): with neither, a detected conflict blocks the accept with a single
/// advisory error; `--force` proceeds as-is; `--supersede <id>` proceeds and
/// additionally demotes-with-link the named row (L3) in favor of this write.
/// `--supersede` is honored whenever supplied, contradiction or not.
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

    // The contradiction probe and its `--supersede` link are repo-scoped
    // (issue #48). Rather than silently ignore a `--supersede` handed to a
    // global-targeted accept, refuse loudly so the operator isn't misled into
    // thinking a global row was retired.
    if target_global && supersede.is_some() {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} targets global memory; --supersede at accept time is repo-scoped \
             (Wave 3 L5). Accept it without --supersede."
        )));
    }

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
                // Probe only when we might block — an explicit --force or
                // --supersede is already the operator's acknowledgement.
                if !force
                    && supersede.is_none()
                    && let Some(conflict) = probe_contradiction(
                        &tx,
                        "fact",
                        &crate::retrieval::fact_embed_text(key, value),
                        Some((key, value)),
                        config.retrieval.use_reranker,
                    )?
                {
                    return Err(MemhubError::InvalidInput(conflict.advisory(id)));
                }
                let (fact_id, _) = fact::add_in_tx(&tx, key, value, &derived_source, actor, mode)?;
                // Honor --supersede whenever supplied: retire the named row in
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
                // applies (same_key = None). The cross-encoder catches
                // near-duplicate decisions; a divergently-worded conflict may
                // not trip it — advisory, best-effort.
                if !force
                    && supersede.is_none()
                    && let Some(conflict) = probe_contradiction(
                        &tx,
                        "decision",
                        &crate::retrieval::decision_embed_text(title, &pending.rationale, None),
                        None,
                        config.retrieval.use_reranker,
                    )?
                {
                    return Err(MemhubError::InvalidInput(conflict.advisory(id)));
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

// ---------------------------------------------------------------------
// `memhub review stale` — read-only lifecycle audit queue (Wave 3 L4,
// issue #47; review §2 L4). Unions four signals, each row naming the
// existing verb a human would reach for. Strictly read-only: every
// query below is a SELECT, and reading a file off disk to hash it is
// not a database write either — nothing here touches `writes_log` or
// any table. `status` reuses this exact computation
// (`count_stale_queue`) for its one-line count so the two can never
// silently disagree.
// ---------------------------------------------------------------------

/// One of the four categories `stale` unions. Presence in the queue is
/// itself the signal — unlike `doctor::Status` there is no "ok" case to
/// report, so this is a plain tag rather than a severity grading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleCategory {
    /// Fact at/over the recall-demotion horizon
    /// (`[retrieval] fact_stale_after_days`) and not already superseded.
    FactNearHorizon,
    /// Done task whose `updated_at` is older than [`DONE_TASK_STALE_DAYS`].
    DoneTaskAged,
    /// Pending write sitting at `status = 'expired'`.
    PendingExpired,
    /// Ingested document whose on-disk file no longer hashes to the
    /// `documents.content_hash` recorded at ingest time.
    DocHashDrift,
}

impl StaleCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FactNearHorizon => "fact_near_horizon",
            Self::DoneTaskAged => "done_task_aged",
            Self::PendingExpired => "pending_expired",
            Self::DocHashDrift => "doc_hash_drift",
        }
    }
}

/// One flagged row. `verb` is plain text naming the existing CLI
/// invocation a human could run next — the queue only ever *suggests*;
/// building a [`StaleItem`] never itself mutates anything.
///
/// [`StaleCategory::DoneTaskAged`] and [`StaleCategory::PendingExpired`]
/// have no verb that reverses the flagged state (this CLI has no
/// task-archive or pending-write "un-expire" verb) — their `verb` names
/// the closest existing *inspect* verb instead of overclaiming a fix.
/// [`StaleCategory::FactNearHorizon`] and [`StaleCategory::DocHashDrift`]
/// do name a genuine fix.
#[derive(Debug, Clone)]
pub struct StaleItem {
    pub category: StaleCategory,
    pub source_id: i64,
    pub message: String,
    pub verb: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StaleCounts {
    pub fact_near_horizon: usize,
    pub done_task_aged: usize,
    pub pending_expired: usize,
    pub doc_hash_drift: usize,
}

impl StaleCounts {
    pub fn total(&self) -> usize {
        self.fact_near_horizon + self.done_task_aged + self.pending_expired + self.doc_hash_drift
    }
}

#[derive(Debug, Clone)]
pub struct StaleReport {
    pub items: Vec<StaleItem>,
    pub counts: StaleCounts,
}

/// `memhub review stale` (Wave 3 L4, issue #47): a strictly read-only
/// audit queue unioning four lifecycle signals, pull-based (never
/// nags — nothing here schedules or prompts anything, it only answers
/// when asked). Opens the project read-side only; every query below is
/// a SELECT and nothing commits a transaction or calls `db::log_write`.
pub fn stale(start: &Path) -> Result<StaleReport> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;
    let horizon_days = ctx.config.retrieval.fact_stale_after_days;

    let (fact_count, mut items) = stale_facts(conn, horizon_days)?;
    let (task_count, task_items) = stale_done_tasks(conn)?;
    let (pending_count, pending_items) = expired_pending_writes(conn)?;
    let (doc_count, doc_items) = drifted_documents(conn)?;

    items.extend(task_items);
    items.extend(pending_items);
    items.extend(doc_items);

    Ok(StaleReport {
        items,
        counts: StaleCounts {
            fact_near_horizon: fact_count,
            done_task_aged: task_count,
            pending_expired: pending_count,
            doc_hash_drift: doc_count,
        },
    })
}

/// Count-only convenience for `status`'s one-line stale-queue summary —
/// the exact same computation as [`stale`], so the two surfaces can
/// never silently drift apart.
pub fn count_stale_queue(start: &Path) -> Result<i64> {
    Ok(stale(start)?.counts.total() as i64)
}

/// Facts at/over the L2 recall-demotion horizon
/// (`[retrieval] fact_stale_after_days`) — the identical predicate
/// `retrieval::recall`'s hydrate path applies (issue #47 grounding:
/// "reuse that horizon logic; don't reinvent it"), deliberately NOT the
/// fixed `models::FACT_STALE_AFTER_DAYS` constant that `fact::list` /
/// `fact::count_stale` use for the non-recall hygiene surfaces. The two
/// horizons default to the same 90 days but are independently
/// configurable by design (see `config/mod.rs`'s
/// `DEFAULT_FACT_STALE_AFTER_DAYS` doc comment); this queue intentionally
/// tracks the recall-facing one. Already-superseded facts are excluded —
/// L3 gave them their own lifecycle signal, and re-verifying a fact that
/// is meant to be replaced is not the fix.
fn stale_facts(conn: &Connection, horizon_days: i64) -> Result<(usize, Vec<StaleItem>)> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts
         WHERE project_id = 1 AND superseded_by IS NULL
           AND (verified_at IS NULL
                OR (julianday('now') - julianday(verified_at)) > ?1)",
        params![horizon_days],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, key, verified_at FROM facts
         WHERE project_id = 1 AND superseded_by IS NULL
           AND (verified_at IS NULL
                OR (julianday('now') - julianday(verified_at)) > ?1)
         ORDER BY (verified_at IS NULL) DESC, verified_at ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![horizon_days, DEFAULT_LIST_LIMIT as i64], |row| {
        let id: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let verified_at: Option<String> = row.get(2)?;
        Ok((id, key, verified_at))
    })?;

    let mut items = Vec::new();
    for row in rows {
        let (id, key, verified_at) = row?;
        let age_desc = match &verified_at {
            Some(ts) => format!("last verified {ts}"),
            None => "never verified".to_string(),
        };
        items.push(StaleItem {
            category: StaleCategory::FactNearHorizon,
            source_id: id,
            message: format!(
                "fact '{key}' (#{id}) is past the {horizon_days}-day staleness horizon \
                 ({age_desc})"
            ),
            verb: format!("memhub fact verify {key}"),
        });
    }
    Ok((count as usize, items))
}

/// Done tasks whose `updated_at` (set when `commands::task::done`
/// transitions status) is older than [`DONE_TASK_STALE_DAYS`].
fn stale_done_tasks(conn: &Connection) -> Result<(usize, Vec<StaleItem>)> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks
         WHERE project_id = 1 AND status = 'done'
           AND (julianday('now') - julianday(updated_at)) > ?1",
        params![DONE_TASK_STALE_DAYS],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, title, updated_at FROM tasks
         WHERE project_id = 1 AND status = 'done'
           AND (julianday('now') - julianday(updated_at)) > ?1
         ORDER BY updated_at ASC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![DONE_TASK_STALE_DAYS, DEFAULT_LIST_LIMIT as i64],
        |row| {
            let id: i64 = row.get(0)?;
            let title: String = row.get(1)?;
            let updated_at: String = row.get(2)?;
            Ok((id, title, updated_at))
        },
    )?;

    let mut items = Vec::new();
    for row in rows {
        let (id, title, updated_at) = row?;
        items.push(StaleItem {
            category: StaleCategory::DoneTaskAged,
            source_id: id,
            message: format!(
                "task #{id} '{title}' done since {updated_at} (> {DONE_TASK_STALE_DAYS} days)"
            ),
            // No archive/un-done verb exists; name the closest existing
            // inspect verb instead (see StaleItem doc comment).
            verb: "memhub task list --status done".to_string(),
        });
    }
    Ok((count as usize, items))
}

/// Pending writes sitting at `status = 'expired'`. Automatic expiry
/// (Wave 3 Q6) flips this silently as a side effect of every DB open,
/// so short of this queue a human never sees what quietly fell through.
fn expired_pending_writes(conn: &Connection) -> Result<(usize, Vec<StaleItem>)> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_writes WHERE project_id = 1 AND status = 'expired'",
        [],
        |row| row.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, kind, reviewed_at FROM pending_writes
         WHERE project_id = 1 AND status = 'expired'
         ORDER BY reviewed_at DESC, id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![DEFAULT_LIST_LIMIT as i64], |row| {
        let id: i64 = row.get(0)?;
        let kind: String = row.get(1)?;
        let reviewed_at: Option<String> = row.get(2)?;
        Ok((id, kind, reviewed_at))
    })?;

    let mut items = Vec::new();
    for row in rows {
        let (id, kind, reviewed_at) = row?;
        let when = reviewed_at.map(|ts| format!(" at {ts}")).unwrap_or_default();
        items.push(StaleItem {
            category: StaleCategory::PendingExpired,
            source_id: id,
            message: format!("pending write #{id} (kind={kind}) expired{when}"),
            // No verb un-expires a row; name the inspect verb instead
            // (see StaleItem doc comment).
            verb: format!("memhub review show {id}"),
        });
    }
    Ok((count as usize, items))
}

/// Ingested documents whose on-disk file no longer hashes to the
/// `content_hash` recorded at ingest time — edited or deleted since.
/// A missing or unreadable file is itself a drift finding rather than a
/// propagated error: surfacing exactly that is this category's job, not
/// a failure of this command.
fn drifted_documents(conn: &Connection) -> Result<(usize, Vec<StaleItem>)> {
    let mut stmt = conn.prepare(
        "SELECT id, path, content_hash FROM documents WHERE project_id = 1 ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let content_hash: String = row.get(2)?;
        Ok((id, path, content_hash))
    })?;

    let mut items = Vec::new();
    for row in rows {
        let (id, path, content_hash) = row?;
        let drift_message = match fs::read_to_string(&path) {
            Ok(content) if sha256_hex(content.as_bytes()) == content_hash => None,
            Ok(_) => Some(format!("document #{id} at {path} has changed since ingest")),
            Err(err) => Some(format!(
                "document #{id} at {path} is unreadable on disk: {err}"
            )),
        };
        if let Some(message) = drift_message {
            items.push(StaleItem {
                category: StaleCategory::DocHashDrift,
                source_id: id,
                message,
                verb: format!("memhub doc add {path}"),
            });
        }
    }
    let count = items.len();
    items.truncate(DEFAULT_LIST_LIMIT);
    Ok((count, items))
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
    use super::*;

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

    // -- accept-time contradiction probe (Wave 3 L5, issue #48) ---------

    // Same-key signal (deterministic, model-free): an existing fact with the
    // same key but a different value blocks the accept unless a flag is given.
    #[test]
    fn accept_blocks_on_same_key_fact_contradiction() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "deploy-cmd", "kubectl apply v1", "user", "cli:user").expect("v1");
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

        let err = super::accept(temp.path(), pid, "cli:user", None, false)
            .expect_err("same-key contradiction must block");
        let msg = err.to_string();
        assert!(msg.contains("deploy-cmd"), "advisory must name the row: {msg}");
        assert!(msg.contains("--force"), "advisory must name the escape: {msg}");

        // Durable value untouched, and the proposal stays pending (retryable).
        assert_eq!(
            fact::list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.key == "deploy-cmd")
                .unwrap()
                .value,
            "kubectl apply v1"
        );
        assert_eq!(super::show(temp.path(), pid).expect("show").status, "pending");
    }

    // The `--force` escape proceeds with the overwrite, and the prior value is
    // recoverable from writes_log (the L5 prior-value logging).
    #[test]
    fn accept_same_key_fact_contradiction_proceeds_with_force() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "deploy-cmd", "kubectl apply v1", "user", "cli:user").expect("v1");
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

    // Reranked signal (bundled cross-encoder): a different-key fact about the
    // same subject blocks; `--supersede <old>` then proceeds and applies the
    // L3 demote-with-link. Loads the reranker model.
    #[test]
    fn accept_blocks_on_reranked_fact_contradiction_then_supersede_links() {
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

        // Block branch: reranked conflict, no flags.
        let err = super::accept(temp.path(), pid, "cli:user", None, false)
            .expect_err("reranked contradiction must block");
        let msg = err.to_string();
        assert!(
            msg.contains("prod-db-host"),
            "advisory must name the conflicting row: {msg}"
        );
        assert!(
            msg.contains("--supersede"),
            "reranked advisory must offer --supersede: {msg}"
        );

        // Proceed branch: acknowledge by retiring the old row in favor of this.
        let outcome =
            super::accept(temp.path(), pid, "cli:user", Some(&old_id.to_string()), false)
                .expect("supersede accept");
        assert_eq!(outcome.durable_table, "facts");
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

    // False-positive guard: an unrelated proposal must NOT be blocked — the
    // probe is a single advisory on a real conflict, not a hard gate on every
    // accept. Loads the reranker model.
    #[test]
    fn accept_unrelated_fact_proceeds_without_flags() {
        use crate::commands::{fact, init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "build-command", "cargo build", "user", "cli:user").expect("f1");
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
        let old_id =
            decision::add(temp.path(), "Use JSON config", "chosen early", "user", "cli:user")
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

        let outcome =
            super::accept(temp.path(), pid, "cli:user", Some(&old_id.to_string()), false)
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

    // -- `review stale` (Wave 3 L4, issue #47) --------------------------

    #[test]
    fn stale_flags_facts_using_the_configured_horizon_not_the_fixed_constant() {
        use crate::commands::init;
        use crate::config::ProjectConfig;
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let (id, _) = fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        // 20 days old: under the fixed 90-day `models::FACT_STALE_AFTER_DAYS`,
        // so this must come from the config knob, not that constant.
        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = datetime('now', '-20 days') WHERE id = ?1",
                params![id],
            )
            .expect("backdate");
        drop(ctx);

        let report = super::stale(temp.path()).expect("stale before narrowing horizon");
        assert_eq!(
            report.counts.fact_near_horizon, 0,
            "20 days old must not be flagged at the default 90-day horizon"
        );

        let paths = db::discover_paths(temp.path()).expect("discover");
        let mut cfg = ProjectConfig::load(&paths.config_path).expect("load config");
        cfg.retrieval.fact_stale_after_days = 10;
        cfg.save(&paths.config_path).expect("save config");

        let report = super::stale(temp.path()).expect("stale after narrowing horizon");
        assert_eq!(report.counts.fact_near_horizon, 1);
        assert_eq!(
            report.items[0].category,
            super::StaleCategory::FactNearHorizon
        );
        assert_eq!(report.items[0].verb, "memhub fact verify k");
    }

    #[test]
    fn stale_excludes_superseded_facts_from_the_fact_category() {
        use crate::commands::init;
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let (old_id, _) = fact::add(temp.path(), "old", "v", "user", "cli:user").expect("old");
        let (new_id, _) = fact::add(temp.path(), "new", "v2", "user", "cli:user").expect("new");
        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = datetime('now', '-400 days') WHERE id = ?1",
                params![old_id],
            )
            .expect("backdate");
        drop(ctx);
        fact::supersede(
            temp.path(),
            &old_id.to_string(),
            &new_id.to_string(),
            "cli:user",
        )
        .expect("supersede");

        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(
            report.counts.fact_near_horizon, 0,
            "a superseded fact must not double-surface in the near-horizon category: {:#?}",
            report.items
        );
    }

    #[test]
    fn stale_flags_only_done_tasks_older_than_the_threshold() {
        use crate::commands::{init, task};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let old_done = task::add(temp.path(), "old done task", None, "cli:user").expect("add");
        task::done(temp.path(), old_done, "cli:user").expect("done");
        let fresh_done =
            task::add(temp.path(), "fresh done task", None, "cli:user").expect("add");
        task::done(temp.path(), fresh_done, "cli:user").expect("done");
        let old_open = task::add(temp.path(), "old open task", None, "cli:user").expect("add");

        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE tasks SET updated_at = datetime('now', '-40 days') WHERE id IN (?1, ?2)",
                params![old_done, old_open],
            )
            .expect("backdate");
        drop(ctx);

        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(report.counts.done_task_aged, 1, "{:#?}", report.items);
        let item = report
            .items
            .iter()
            .find(|i| i.category == super::StaleCategory::DoneTaskAged)
            .expect("done task item present");
        assert_eq!(item.source_id, old_done);
        assert_eq!(item.verb, "memhub task list --status done");
    }

    #[test]
    fn stale_flags_only_expired_pending_writes() {
        use crate::commands::{init, pending_write};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let expired_id =
            pending_write::propose_fact(temp.path(), "k", "v", "r", "codex", "codex", "{}")
                .expect("propose");
        let still_pending =
            pending_write::propose_fact(temp.path(), "k2", "v2", "r", "codex", "codex", "{}")
                .expect("propose");

        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE pending_writes SET status = 'expired', reviewed_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![expired_id],
            )
            .expect("mark expired");
        drop(ctx);

        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(report.counts.pending_expired, 1, "{:#?}", report.items);
        let item = report
            .items
            .iter()
            .find(|i| i.category == super::StaleCategory::PendingExpired)
            .expect("pending item present");
        assert_eq!(item.source_id, expired_id);
        assert_eq!(item.verb, format!("memhub review show {expired_id}"));
        assert!(
            !report
                .items
                .iter()
                .any(|i| i.source_id == still_pending
                    && i.category == super::StaleCategory::PendingExpired),
            "a still-pending row must not surface as expired"
        );
    }

    #[test]
    fn stale_flags_doc_hash_drift_for_edited_and_missing_files_but_not_unchanged() {
        use crate::commands::{doc, init};
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let unchanged = temp.path().join("unchanged.md");
        fs::write(&unchanged, "# Unchanged\n\nbody\n").expect("write");
        doc::add(temp.path(), &unchanged, None, "cli:user").expect("ingest unchanged");

        let edited = temp.path().join("edited.md");
        fs::write(&edited, "# Edited\n\noriginal\n").expect("write");
        doc::add(temp.path(), &edited, None, "cli:user").expect("ingest edited");
        fs::write(&edited, "# Edited\n\nchanged after ingest\n").expect("edit after ingest");

        let missing = temp.path().join("missing.md");
        fs::write(&missing, "# Missing\n\nbody\n").expect("write");
        doc::add(temp.path(), &missing, None, "cli:user").expect("ingest missing");
        fs::remove_file(&missing).expect("delete after ingest");

        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(
            report.counts.doc_hash_drift, 2,
            "exactly the edited + missing docs should drift: {:#?}",
            report.items
        );
        let drifted: Vec<&str> = report
            .items
            .iter()
            .filter(|i| i.category == super::StaleCategory::DocHashDrift)
            .map(|i| i.message.as_str())
            .collect();
        assert!(drifted.iter().any(|m| m.contains("edited.md")));
        assert!(drifted.iter().any(|m| m.contains("missing.md")));
        assert!(!drifted.iter().any(|m| m.contains("unchanged.md")));
    }

    /// Builds one fixture row per category so the shared helper below can
    /// exercise all four at once (acceptance criteria: "all four
    /// categories surface", "each row names the verb to fix it", and the
    /// read-only guarantee).
    fn seed_one_of_each_category(temp: &std::path::Path) {
        use crate::commands::{doc, pending_write, task};

        let (fact_id, _) = fact::add(temp, "k", "v", "user", "cli:user").expect("fact");
        let task_id = task::add(temp, "t", None, "cli:user").expect("task");
        task::done(temp, task_id, "cli:user").expect("done");
        let pending_id =
            pending_write::propose_fact(temp, "k2", "v2", "r", "codex", "codex", "{}")
                .expect("propose");
        let doc_path = temp.join("d.md");
        fs::write(&doc_path, "# D\n\nbody\n").expect("write");
        doc::add(temp, &doc_path, None, "cli:user").expect("doc");

        let ctx = db::open_project(temp).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = datetime('now', '-400 days') WHERE id = ?1",
                params![fact_id],
            )
            .expect("backdate fact");
        ctx.conn
            .execute(
                "UPDATE tasks SET updated_at = datetime('now', '-40 days') WHERE id = ?1",
                params![task_id],
            )
            .expect("backdate task");
        ctx.conn
            .execute(
                "UPDATE pending_writes SET status = 'expired', reviewed_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                params![pending_id],
            )
            .expect("expire pending");
        drop(ctx);
        fs::write(&doc_path, "# D\n\nedited after ingest\n").expect("edit doc");
    }

    #[test]
    fn stale_every_item_names_a_non_empty_memhub_verb() {
        use crate::commands::init;
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        seed_one_of_each_category(temp.path());

        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(report.items.len(), 4, "{:#?}", report.items);
        for item in &report.items {
            assert!(
                !item.verb.trim().is_empty(),
                "every stale item must name a verb: {item:?}"
            );
            assert!(
                item.verb.starts_with("memhub "),
                "verb should be a runnable memhub invocation: {item:?}"
            );
        }
    }

    #[test]
    fn stale_is_strictly_read_only() {
        use crate::commands::init;
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        seed_one_of_each_category(temp.path());

        fn snapshot(path: &std::path::Path) -> (i64, i64, i64, i64, i64, i64) {
            let ctx = db::open_project(path).expect("open");
            let count =
                |sql: &str| -> i64 { ctx.conn.query_row(sql, [], |r| r.get(0)).expect("count") };
            (
                count("SELECT COUNT(*) FROM facts"),
                count("SELECT COUNT(*) FROM tasks"),
                count("SELECT COUNT(*) FROM pending_writes"),
                count("SELECT COUNT(*) FROM documents"),
                count("SELECT COUNT(*) FROM writes_log"),
                count("SELECT IFNULL(MAX(id), 0) FROM writes_log"),
            )
        }

        let before = snapshot(temp.path());
        let report = super::stale(temp.path()).expect("stale");
        let after = snapshot(temp.path());

        assert_eq!(
            before, after,
            "review stale must not add/remove rows or writes_log entries"
        );
        assert_eq!(report.counts.total(), 4, "{:#?}", report.items);
    }

    #[test]
    fn count_stale_queue_matches_stale_report_total() {
        use crate::commands::init;
        let temp = tempfile::tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        seed_one_of_each_category(temp.path());

        let total = super::count_stale_queue(temp.path()).expect("count");
        let report = super::stale(temp.path()).expect("stale");
        assert_eq!(total, report.counts.total() as i64);
        assert_eq!(total, 4);
    }
}
