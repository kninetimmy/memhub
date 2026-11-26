//! Resolve opaque candidate IDs before taking a writer lock.

use std::path::PathBuf;

use rusqlite::TransactionBehavior;

use super::*;

enum Candidate {
    Fact(i64),
    Task(i64),
    Document(i64),
    Backups(PathBuf),
    Similarity(similarity::Pair),
}

pub fn dismiss(start: &Path, id: &str, actor: &str) -> Result<()> {
    let paths = db::discover_paths(start)?;
    if !paths.db_path.try_exists()? {
        return Err(MemhubError::MissingDatabase {
            memhub_dir: paths.memhub_dir,
            db_path: paths.db_path,
        });
    }
    let config = scan_config(&paths)?;
    // Do not create a DB, change its journal mode, or run open maintenance.
    let conn = Connection::open_with_flags(&paths.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = OFF;")?;
    let mut ctx = db::ProjectContext {
        paths,
        config,
        conn,
    };
    dismiss_in(&mut ctx, id, actor)
}

fn dismiss_in(ctx: &mut db::ProjectContext, id: &str, actor: &str) -> Result<()> {
    let (candidate, warning) = {
        let snapshot = ctx.conn.transaction()?;
        schema_has_dismissals(&snapshot)?;
        prepare(&snapshot, &ctx.paths, &ctx.config, id)?
    };
    let not_current = || {
        let detail = warning
            .as_ref()
            .map_or_else(String::new, |w| format!("; {w}"));
        MemhubError::InvalidInput(format!(
            "no current prune candidate '{id}'; run `memhub prune scan` again{detail}"
        ))
    };
    let candidate = candidate.ok_or_else(not_current)?;
    let tx = ctx
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_dismissals = schema_has_dismissals(&tx)?;
    if !candidate.is_current(&tx, &ctx.paths, &ctx.config, id)? {
        return Err(not_current());
    }
    if !has_dismissals {
        db::migrate_for_prune_dismissal(&tx, &ctx.paths)?;
    }
    record(&tx, id, actor)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn is_dismissed(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM prune_dismissals WHERE project_id = 1 AND identifier = ?1)",
        [id],
        |r| r.get(0),
    )?)
}

pub(super) fn record(tx: &rusqlite::Transaction<'_>, id: &str, actor: &str) -> Result<()> {
    if tx.execute(
        "INSERT INTO prune_dismissals(identifier) VALUES (?1)
         ON CONFLICT(project_id, identifier) DO NOTHING",
        [id],
    )? > 0
    {
        db::log_write(
            tx,
            actor,
            "prune_dismissals",
            Some(tx.last_insert_rowid()),
            "insert",
            &format!("prune dismiss {id}"),
        )?;
    }
    Ok(())
}

fn schema_has_dismissals(conn: &Connection) -> Result<bool> {
    let schema = db::ledger_schema(conn)?;
    match schema.as_deref() {
        Some(db::PRUNE_DISMISSALS_MIGRATION) => Ok(true),
        Some(db::MEMORY_LIFECYCLE_MIGRATION) => Ok(false),
        _ => Err(MemhubError::InvalidInput(format!(
            "prune dismiss cannot write schema {}; run `memhub upgrade` and try again",
            schema.as_deref().unwrap_or("(unknown)")
        ))),
    }
}

fn find(
    id: &str,
    visit: impl FnOnce(&mut dyn FnMut(Item)) -> Result<()>,
) -> Result<Option<Subject>> {
    let mut found = None;
    visit(&mut |item| {
        if item.id == id {
            found = item.subjects.into_iter().next();
        }
    })?;
    Ok(found)
}

fn prepare(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    id: &str,
) -> Result<(Option<Candidate>, Option<String>)> {
    // A model-free dismissal never loads the similarity corpus. Filesystem
    // discovery and inference stay in this read snapshot, even for stale IDs.
    if let Some(Subject::Fact { id, .. }) = find(id, |visit| {
        facts(conn, config.retrieval.fact_stale_after_days, None, visit)
    })? {
        return Ok((Some(Candidate::Fact(id)), None));
    }
    if let Some(Subject::Task { id, .. }) = find(id, |visit| tasks(conn, None, visit))? {
        return Ok((Some(Candidate::Task(id)), None));
    }
    if let Some(Subject::Document { id, .. }) =
        find(id, |visit| documents(conn, &paths.repo_root, None, visit))?
    {
        return Ok((Some(Candidate::Document(id)), None));
    }
    let warning = match backups::surplus(paths, None) {
        Ok((groups, warnings)) => {
            if let Some(group) = groups.into_iter().find(|g| g.id == id) {
                return Ok((Some(Candidate::Backups(group.directory)), None));
            }
            backups::warning(warnings)
        }
        Err(err) => Some(format!("Surplus backups not counted: {err}")),
    };
    let pair = if config.retrieval.use_reranker {
        similarity::prepare_dismissal(conn, &paths.repo_root, id)?
    } else {
        None
    };
    Ok((pair.map(Candidate::Similarity), warning))
}

impl Candidate {
    fn is_current(
        &self,
        conn: &Connection,
        paths: &ProjectPaths,
        config: &ProjectConfig,
        id: &str,
    ) -> Result<bool> {
        let found = match self {
            Self::Fact(row) => find(id, |visit| {
                facts(
                    conn,
                    config.retrieval.fact_stale_after_days,
                    Some(*row),
                    visit,
                )
            }),
            Self::Task(row) => find(id, |visit| tasks(conn, Some(*row), visit)),
            Self::Document(row) => find(id, |visit| {
                documents(conn, &paths.repo_root, Some(*row), visit)
            }),
            Self::Backups(dir) => return backups::group_is_current(paths, dir, id, None),
            Self::Similarity(pair) => return pair.is_current(conn, &paths.repo_root, id, None),
        }?;
        Ok(found.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_model_free_id_skips_an_unavailable_similarity_corpus() {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let mut ctx = db::open_project(temp.path()).unwrap();
        ctx.config.retrieval.use_reranker = true;
        ctx.conn
            .execute_batch(
                "INSERT INTO facts(key,value) VALUES ('key','value'); DROP TABLE decisions;",
            )
            .unwrap();
        let mut id = String::new();
        facts(&ctx.conn, 60, None, &mut |item| id = item.id).unwrap();
        for _ in 0..2 {
            dismiss_in(&mut ctx, &id, "test").unwrap();
        }
        assert_eq!(
            ctx.conn
                .query_row(
                    "SELECT count(*) FROM writes_log WHERE table_name = 'prune_dismissals'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn stale_dismissal_refuses_without_taking_the_writer_lock() {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let mut ctx = db::open_project(temp.path()).unwrap();
        ctx.conn.busy_timeout(Duration::ZERO).unwrap();
        ctx.conn
            .execute_batch(
                "INSERT INTO facts(key,value) VALUES
             ('datastore','We will run a Postgres server to hold project memory.'),
             ('database','We will run a Postgres server to hold project memory.');",
            )
            .unwrap();
        ctx.config.retrieval.use_reranker = true;
        let mut id = None;
        similarity::visit(&ctx.conn, &ctx.paths.repo_root, None, None, &mut |item| {
            assert_eq!(item.category, Category::DuplicateFacts);
            id = Some(item.id);
        })
        .unwrap();
        let id = id.expect("similarity match");
        ctx.conn
            .execute("UPDATE facts SET value = 'changed' WHERE id = 2", [])
            .unwrap();
        let mut other = Connection::open(&ctx.paths.db_path).unwrap();
        let _writer = other
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let err = dismiss_in(&mut ctx, &id, "test").unwrap_err();
        assert!(
            err.to_string().contains("no current prune candidate"),
            "{err}"
        );
    }
}
