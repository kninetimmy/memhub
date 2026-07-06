use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::Result;
use crate::db;
use crate::models::{FACT_STALE_AFTER_DAYS, Fact};
use crate::sync_md;

pub fn add(start: &Path, key: &str, value: &str, source: &str, actor: &str) -> Result<(i64, bool)> {
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;
    let outcome = add_in_tx(&tx, key, value, source, actor, mode)?;
    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(outcome)
}

pub fn add_in_tx(
    tx: &Transaction<'_>,
    key: &str,
    value: &str,
    source: &str,
    actor: &str,
    mode: crate::config::RetrievalMode,
) -> Result<(i64, bool)> {
    crate::commands::validate_source(source)?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM facts WHERE project_id = 1 AND key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;

    let (row_id, created) = if let Some(id) = existing_id {
        tx.execute(
            "UPDATE facts
             SET value = ?1, source = ?2, confidence = 1.0, verified_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            params![value, source, id],
        )?;
        (id, false)
    } else {
        tx.execute(
            "INSERT INTO facts(project_id, key, value, confidence, source, verified_at)
             VALUES (1, ?1, ?2, 1.0, ?3, CURRENT_TIMESTAMP)",
            params![key, value, source],
        )?;
        (tx.last_insert_rowid(), true)
    };

    db::log_write(
        tx,
        actor,
        "facts",
        Some(row_id),
        if created { "insert" } else { "update" },
        "fact add",
    )?;

    let embed_text = crate::retrieval::fact_embed_text(key, value);
    crate::retrieval::eager_embed_in_tx(
        tx,
        mode,
        crate::retrieval::SourceType::Fact,
        row_id,
        &embed_text,
    )?;

    Ok((row_id, created))
}

#[derive(Debug)]
pub struct GlobalFactOutcome {
    pub id: i64,
    pub created: bool,
    /// True when this call created `~/.memhub/global.sqlite`.
    pub store_created: bool,
}

/// Born-global fact write (M9). Requires `memhub global enable` in
/// this repo. Embeds using the *repo's* retrieval mode so global rows
/// stay consistent with how this machine recalls.
pub fn add_global(
    start: &Path,
    key: &str,
    value: &str,
    source: &str,
    actor: &str,
) -> Result<GlobalFactOutcome> {
    let mut gw = crate::commands::global::begin_write(start)?;
    let tx = gw.ctx.conn.transaction()?;
    let (id, created) = add_in_tx(&tx, key, value, source, actor, gw.mode)?;
    tx.commit()?;
    Ok(GlobalFactOutcome {
        id,
        created,
        store_created: gw.store_created,
    })
}

/// Copy an existing repo fact into the machine-global store (copy,
/// not move — the repo row stays and still wins locally). Fact keys
/// are UNIQUE per DB, so re-promoting a key updates the global fact.
pub fn promote(start: &Path, id: i64, actor: &str) -> Result<GlobalFactOutcome> {
    let repo = db::open_project(start)?;
    crate::commands::global::ensure_enabled(&repo.config)?;
    let mode = repo.config.retrieval.mode;

    let (key, value, source): (String, String, String) = repo
        .conn
        .query_row(
            "SELECT key, value, source FROM facts WHERE id = ?1 AND project_id = 1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                crate::MemhubError::InvalidInput(format!("no fact with id {id}"))
            }
            other => crate::MemhubError::from(other),
        })?;

    let repo_root = repo.paths.repo_root.display().to_string();
    let store_created = !db::global_store_exists()?;

    let mut g = db::open_global()?;
    let tx = g.conn.transaction()?;
    let (gid, created) = add_in_tx(&tx, &key, &value, &source, actor, mode)?;
    db::log_write(
        &tx,
        actor,
        "facts",
        Some(gid),
        "promote",
        &format!("promote from {repo_root}"),
    )?;
    tx.commit()?;

    Ok(GlobalFactOutcome {
        id: gid,
        created,
        store_created,
    })
}

/// Resolve a fact by numeric id or by exact key, mirroring
/// `doc::resolve_doc_id`'s id-first-then-lookup shape. Returns
/// `(id, key)` so callers can report the resolved key even when
/// `ident` was numeric.
fn resolve_fact(tx: &Transaction<'_>, ident: &str) -> Result<Option<(i64, String)>> {
    if let Ok(id) = ident.parse::<i64>() {
        let found: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, key FROM facts WHERE project_id = 1 AND id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    tx.query_row(
        "SELECT id, key FROM facts WHERE project_id = 1 AND key = ?1",
        params![ident],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// Refresh a fact's `verified_at` to now — nothing else durable changes.
/// Unlike `add`, this never touches `value`, `source`, or `confidence`,
/// and never runs the add-upsert dedupe path; it is a pure re-verify
/// (L1). Accepts either a numeric id or an exact key. Returns
/// `(id, key)` on a match, or `Ok(None)` when nothing matched `ident`
/// so the CLI can report a clean miss instead of a false success.
pub fn verify(start: &Path, ident: &str, actor: &str) -> Result<Option<(i64, String)>> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let Some((id, key)) = resolve_fact(&tx, ident)? else {
        return Ok(None);
    };

    tx.execute(
        "UPDATE facts SET verified_at = CURRENT_TIMESTAMP WHERE id = ?1",
        params![id],
    )?;

    db::log_write(
        &tx,
        actor,
        "facts",
        Some(id),
        "verify",
        &format!("fact verify: {ident}"),
    )?;

    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(Some((id, key)))
}

pub fn list(start: &Path) -> Result<Vec<Fact>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, key, value, source, verified_at, created_at,
                CASE
                    WHEN verified_at IS NULL THEN 1
                    WHEN (julianday('now') - julianday(verified_at)) > ?1 THEN 1
                    ELSE 0
                END AS is_stale,
                superseded_by
         FROM facts
         ORDER BY key ASC",
    )?;

    let rows = stmt.query_map(params![FACT_STALE_AFTER_DAYS], |row| {
        let is_stale_int: i64 = row.get(6)?;
        Ok(Fact {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            source: row.get(3)?,
            verified_at: row.get(4)?,
            created_at: row.get(5)?,
            is_stale: is_stale_int != 0,
            superseded_by: row.get(7)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Outcome of a fact supersession — the demoted old row and the row it now
/// points to. Both sides are reported so the CLI can echo resolved keys even
/// when the caller passed numeric ids.
#[derive(Debug)]
pub struct FactSupersedeOutcome {
    pub old_id: i64,
    pub old_key: String,
    pub new_id: i64,
    pub new_key: String,
}

/// Mark `old_ident`'s fact superseded by `new_ident`'s fact (Wave 3 L3).
/// Demote-with-link, no-loss: the old row is NOT deleted — it stays present
/// with `superseded_by` set, is penalized in recall, and is annotated in
/// render. Both idents accept a numeric id or an exact key (mirroring
/// `fact verify`). Errors if either side is missing or if a fact would
/// supersede itself. Transaction-scoped so it composes with the CLI verb and
/// the review-accept path.
pub fn supersede_in_tx(
    tx: &Transaction<'_>,
    old_ident: &str,
    new_ident: &str,
    actor: &str,
) -> Result<FactSupersedeOutcome> {
    let (old_id, old_key) = resolve_fact(tx, old_ident)?.ok_or_else(|| {
        crate::MemhubError::InvalidInput(format!("no fact matched '{old_ident}'"))
    })?;
    let (new_id, new_key) = resolve_fact(tx, new_ident)?.ok_or_else(|| {
        crate::MemhubError::InvalidInput(format!("no fact matched '{new_ident}'"))
    })?;
    if old_id == new_id {
        return Err(crate::MemhubError::InvalidInput(
            "a fact cannot supersede itself".to_string(),
        ));
    }

    tx.execute(
        "UPDATE facts SET superseded_by = ?1 WHERE project_id = 1 AND id = ?2",
        params![new_id, old_id],
    )?;

    db::log_write(
        tx,
        actor,
        "facts",
        Some(old_id),
        "supersede",
        &format!("fact supersede {old_id} by {new_id}"),
    )?;

    Ok(FactSupersedeOutcome {
        old_id,
        old_key,
        new_id,
        new_key,
    })
}

/// CLI wrapper around [`supersede_in_tx`]: open the project, run the
/// supersession in one transaction, then re-render markdown if enabled.
pub fn supersede(
    start: &Path,
    old_ident: &str,
    new_ident: &str,
    actor: &str,
) -> Result<FactSupersedeOutcome> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let outcome = supersede_in_tx(&tx, old_ident, new_ident, actor)?;
    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(outcome)
}

pub fn count_stale(start: &Path) -> Result<i64> {
    let ctx = db::open_project(start)?;
    let count: i64 = ctx.conn.query_row(
        "SELECT COUNT(*)
         FROM facts
         WHERE verified_at IS NULL
            OR (julianday('now') - julianday(verified_at)) > ?1",
        params![FACT_STALE_AFTER_DAYS],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::tempdir;

    fn writes_log_count(start: &Path, action: &str) -> i64 {
        let ctx = db::open_project(start).expect("open");
        ctx.conn
            .query_row(
                "SELECT COUNT(*) FROM writes_log WHERE table_name = 'facts' AND action = ?1",
                params![action],
                |r| r.get(0),
            )
            .expect("count writes_log")
    }

    // Wave 3 L3 — supersede is demote-with-link, no-loss (decision 145):
    // the old fact is NOT deleted; it stays present with `superseded_by`
    // pointing at its replacement.
    #[test]
    fn supersede_links_old_to_new_without_deleting() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) =
            add(temp.path(), "deploy-cmd", "kubectl apply v1", "user", "cli:user").expect("old");
        let (new_id, _) = add(
            temp.path(),
            "deploy-cmd-v2",
            "kubectl apply v2",
            "user",
            "cli:user",
        )
        .expect("new");

        let outcome = supersede(
            temp.path(),
            &old_id.to_string(),
            &new_id.to_string(),
            "cli:user",
        )
        .expect("supersede");
        assert_eq!(outcome.old_id, old_id);
        assert_eq!(outcome.new_id, new_id);

        let facts = list(temp.path()).expect("list");
        let old = facts
            .iter()
            .find(|f| f.id == old_id)
            .expect("superseded fact must still be present (no-loss)");
        assert_eq!(old.superseded_by, Some(new_id), "old links to the new fact");
        let new = facts.iter().find(|f| f.id == new_id).expect("new present");
        assert_eq!(new.superseded_by, None, "the replacement is not superseded");
        assert_eq!(writes_log_count(temp.path(), "supersede"), 1);
    }

    // Both sides resolve by exact key (mirroring `fact verify`), not just id.
    #[test]
    fn supersede_resolves_by_key() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) =
            add(temp.path(), "old-key", "v1", "user", "cli:user").expect("old");
        let (new_id, _) =
            add(temp.path(), "new-key", "v2", "user", "cli:user").expect("new");

        let outcome =
            supersede(temp.path(), "old-key", "new-key", "cli:user").expect("supersede by key");
        assert_eq!(outcome.old_id, old_id);
        assert_eq!(outcome.new_id, new_id);
        assert_eq!(
            list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.id == old_id)
                .unwrap()
                .superseded_by,
            Some(new_id)
        );
    }

    #[test]
    fn supersede_rejects_self_and_missing() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (id, _) = add(temp.path(), "only", "v", "user", "cli:user").expect("fact");

        let self_err = supersede(temp.path(), &id.to_string(), &id.to_string(), "cli:user")
            .expect_err("self-supersede must fail");
        assert!(matches!(self_err, crate::MemhubError::InvalidInput(_)));

        let missing_err = supersede(temp.path(), &id.to_string(), "nope", "cli:user")
            .expect_err("missing new must fail");
        assert!(matches!(missing_err, crate::MemhubError::InvalidInput(_)));
        // The failed op left the durable row untouched.
        assert_eq!(
            list(temp.path())
                .expect("list")
                .iter()
                .find(|f| f.id == id)
                .unwrap()
                .superseded_by,
            None
        );
    }
}
