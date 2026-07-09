use std::path::Path;

use rusqlite::params;
use serde_json::json;

use crate::MemhubError;
use crate::Result;
use crate::db;

/// Stage an agent-proposed fact for the repo's review queue. The
/// common (repo-targeted) case; M9 global proposals use
/// [`propose_fact_scoped`].
pub fn propose_fact(
    start: &Path,
    key: &str,
    value: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
    provenance_json: &str,
) -> Result<i64> {
    propose_fact_scoped(
        start,
        key,
        value,
        rationale,
        None,
        false,
        actor,
        actor_raw,
        provenance_json,
    )
}

/// As [`propose_fact`], but `global = true` tags the staged row
/// `target: "global"` so an accepted proposal lands in the
/// machine-global store (M9). Still staged; the human `review accept`
/// remains the only path to a durable global write. `kind` (issue #97) is
/// the same optional, unenforced classifier `fact add --kind` sets
/// directly; staged here in the payload and applied at accept time.
#[allow(clippy::too_many_arguments)]
pub fn propose_fact_scoped(
    start: &Path,
    key: &str,
    value: &str,
    rationale: &str,
    kind: Option<&str>,
    global: bool,
    actor: &str,
    actor_raw: &str,
    provenance_json: &str,
) -> Result<i64> {
    ensure_non_empty("fact key", key)?;
    ensure_non_empty("fact value", value)?;
    ensure_non_empty("fact rationale", rationale)?;
    let mut payload = json!({
        "key": key,
        "value": value,
    });
    if let Some(k) = kind.map(str::trim).filter(|k| !k.is_empty()) {
        payload["kind"] = json!(k);
    }
    if global {
        payload["target"] = json!("global");
    }

    insert_pending_write(
        start,
        "fact",
        &payload.to_string(),
        rationale,
        actor,
        actor_raw,
        "mcp propose_fact",
        provenance_json,
    )
}

/// Stage an agent-proposed decision for the repo's review queue. M9
/// global proposals use [`propose_decision_scoped`].
pub fn propose_decision(
    start: &Path,
    title: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
    provenance_json: &str,
) -> Result<i64> {
    propose_decision_scoped(
        start,
        title,
        rationale,
        false,
        actor,
        actor_raw,
        provenance_json,
    )
}

/// As [`propose_decision`], but `global = true` tags the staged row
/// `target: "global"` (M9).
#[allow(clippy::too_many_arguments)]
pub fn propose_decision_scoped(
    start: &Path,
    title: &str,
    rationale: &str,
    global: bool,
    actor: &str,
    actor_raw: &str,
    provenance_json: &str,
) -> Result<i64> {
    ensure_non_empty("decision title", title)?;
    ensure_non_empty("decision rationale", rationale)?;
    let mut payload = json!({
        "title": title,
    });
    if global {
        payload["target"] = json!("global");
    }

    insert_pending_write(
        start,
        "decision",
        &payload.to_string(),
        rationale,
        actor,
        actor_raw,
        "mcp propose_decision",
        provenance_json,
    )
}

/// Stage an agent-proposed supersession for the repo's review queue (Wave 3
/// L3). This is the ONLY supersede surface an agent has: it merely stages a
/// `kind: "supersede"` pending write — the durable, demote-with-link
/// supersession happens exclusively on human `memhub review accept` (or via
/// the CLI `fact/decision supersede` verbs). The untrusted-writer guardrail
/// means an agent can never retire a fact/decision on its own.
///
/// `target_kind` is `"fact"` or `"decision"`. `old`/`new` are identifiers
/// resolved at accept time (fact: numeric id or exact key; decision: numeric
/// id) — deferring resolution keeps staging cheap and lets the reviewer act
/// on the freshest rows.
#[allow(clippy::too_many_arguments)]
pub fn propose_supersede(
    start: &Path,
    target_kind: &str,
    old: &str,
    new: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
    provenance_json: &str,
) -> Result<i64> {
    let target_kind = target_kind.trim();
    if target_kind != "fact" && target_kind != "decision" {
        return Err(MemhubError::InvalidInput(format!(
            "supersede target_kind must be 'fact' or 'decision', got '{target_kind}'"
        )));
    }
    ensure_non_empty("supersede old", old)?;
    ensure_non_empty("supersede new", new)?;
    ensure_non_empty("supersede rationale", rationale)?;
    if old.trim() == new.trim() {
        return Err(MemhubError::InvalidInput(
            "supersede old and new must differ".to_string(),
        ));
    }
    let payload = json!({
        "target_kind": target_kind,
        "old": old,
        "new": new,
    });

    insert_pending_write(
        start,
        "supersede",
        &payload.to_string(),
        rationale,
        actor,
        actor_raw,
        "mcp propose_supersede",
        provenance_json,
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_pending_write(
    start: &Path,
    kind: &str,
    payload_json: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
    reason: &str,
    provenance_json: &str,
) -> Result<i64> {
    ensure_non_empty("pending write kind", kind)?;
    ensure_non_empty("pending write actor", actor)?;
    ensure_json("pending write provenance", provenance_json)?;
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO pending_writes(
            project_id,
            kind,
            payload_json,
            rationale,
            actor,
            actor_raw,
            provenance_json
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            kind,
            payload_json,
            rationale,
            actor,
            actor_raw,
            provenance_json
        ],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(&tx, actor, "pending_writes", Some(row_id), "insert", reason)?;

    tx.commit()?;
    Ok(row_id)
}

fn ensure_non_empty(field_name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{field_name} must not be empty"
        )));
    }

    Ok(())
}

fn ensure_json(field_name: &str, value: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(value).map_err(|err| {
        MemhubError::InvalidInput(format!("{field_name} must be valid JSON: {err}"))
    })?;
    Ok(())
}
