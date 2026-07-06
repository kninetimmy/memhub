use rusqlite::Connection;

use crate::Result;

pub fn latest_version() -> &'static str {
    MIGRATIONS.last().expect("MIGRATIONS list is non-empty").0
}

/// Numeric prefix of a `NNNN_name` migration id (`0` when unparseable).
fn ordinal(version: &str) -> u32 {
    version
        .split('_')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// The highest `schema_migrations` version whose ordinal exceeds the
/// newest compiled migration, if any — proof the DB was written by a
/// newer binary than this one.
fn newer_than_compiled(conn: &Connection) -> Result<Option<String>> {
    let ceiling = ordinal(latest_version());
    let mut stmt = conn.prepare("SELECT version FROM schema_migrations")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut newest: Option<(u32, String)> = None;
    for row in rows {
        let v = row?;
        let o = ordinal(&v);
        if o > ceiling && newest.as_ref().map(|(n, _)| o > *n).unwrap_or(true) {
            newest = Some((o, v));
        }
    }
    Ok(newest.map(|(_, v)| v))
}

const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_initial",
        include_str!("../../migrations/0001_initial.sql"),
    ),
    (
        "0002_git_search",
        include_str!("../../migrations/0002_git_search.sql"),
    ),
    (
        "0003_pending_writes",
        include_str!("../../migrations/0003_pending_writes.sql"),
    ),
    (
        "0004_pending_write_provenance",
        include_str!("../../migrations/0004_pending_write_provenance.sql"),
    ),
    (
        "0005_pending_write_reviewed_at",
        include_str!("../../migrations/0005_pending_write_reviewed_at.sql"),
    ),
    (
        "0006_session_notes",
        include_str!("../../migrations/0006_session_notes.sql"),
    ),
    (
        "0007_project_narrative",
        include_str!("../../migrations/0007_project_narrative.sql"),
    ),
    (
        "0008_decisions_source",
        include_str!("../../migrations/0008_decisions_source.sql"),
    ),
    (
        "0009_retrieval_indexes",
        include_str!("../../migrations/0009_retrieval_indexes.sql"),
    ),
    (
        "0010_embeddings_delete_triggers",
        include_str!("../../migrations/0010_embeddings_delete_triggers.sql"),
    ),
    (
        "0011_decision_summary",
        include_str!("../../migrations/0011_decision_summary.sql"),
    ),
    (
        "0012_metrics_tables",
        include_str!("../../migrations/0012_metrics_tables.sql"),
    ),
    (
        "0013_session_turn_metrics",
        include_str!("../../migrations/0013_session_turn_metrics.sql"),
    ),
    (
        "0014_documents",
        include_str!("../../migrations/0014_documents.sql"),
    ),
    (
        "0015_known_projects",
        include_str!("../../migrations/0015_known_projects.sql"),
    ),
    (
        "0016_global_accept_markers",
        include_str!("../../migrations/0016_global_accept_markers.sql"),
    ),
    (
        "0017_session_baseline",
        include_str!("../../migrations/0017_session_baseline.sql"),
    ),
    (
        "0018_supersede",
        include_str!("../../migrations/0018_supersede.sql"),
    ),
];

pub fn apply_all(conn: &mut Connection) -> Result<Vec<String>> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )?;

    // Refuse to touch a DB written by a newer binary. If `schema_migrations`
    // records a version this build does not know (a higher ordinal than the
    // newest compiled migration), an older binary would otherwise write into
    // a schema it doesn't understand — silent forward-incompatible corruption.
    // Fail closed and point at the fix. (Sync adopt has an equivalent guard;
    // this covers every other open path.)
    if let Some(newer) = newer_than_compiled(conn)? {
        return Err(crate::MemhubError::InvalidInput(format!(
            "this database was written by a newer memhub (schema '{newer}' is \
             unknown to this build, newest known is '{}'); refusing to open it \
             with an older binary. Run `memhub upgrade` to rebuild, then retry.",
            latest_version()
        )));
    }

    let tx = conn.transaction()?;
    let mut applied = Vec::new();

    for (version, sql) in MIGRATIONS {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )?;

        if !exists {
            tx.execute_batch(sql)?;
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [version],
            )?;
            applied.push((*version).to_string());
        }
    }

    tx.commit()?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn idempotent_reapply_is_a_noop() {
        let mut conn = Connection::open_in_memory().expect("open");
        let first = apply_all(&mut conn).expect("first apply");
        assert!(!first.is_empty(), "fresh DB should apply every migration");
        let second = apply_all(&mut conn).expect("second apply");
        assert!(second.is_empty(), "re-applying should touch nothing");
    }

    #[test]
    fn refuses_a_db_written_by_a_newer_binary() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("first apply");
        // A future memhub recorded a migration this build has never heard of.
        conn.execute(
            "INSERT INTO schema_migrations(version) VALUES ('9999_from_the_future')",
            [],
        )
        .expect("seed future migration");
        let err = apply_all(&mut conn).expect_err("must refuse a newer-schema DB");
        let msg = err.to_string();
        assert!(msg.contains("newer memhub"), "unexpected message: {msg}");
        assert!(
            msg.contains("memhub upgrade"),
            "should point at the fix: {msg}"
        );
    }

    #[test]
    fn ordinal_parses_migration_prefix() {
        assert_eq!(ordinal("0017_session_baseline"), 17);
        assert_eq!(ordinal("0001_initial"), 1);
        assert_eq!(ordinal("garbage"), 0);
    }

    /// Migration 0018 (Wave 3 L3) adds the fact supersession link column.
    /// Decisions already carry `superseded_by` from 0001, so only `facts`
    /// gains it here; assert it exists (and stays replay-safe via the
    /// `idempotent_reapply_is_a_noop` test above).
    #[test]
    fn migration_0018_adds_facts_superseded_by_column() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");
        let facts_has: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name = 'superseded_by'",
                [],
                |r| r.get(0),
            )
            .expect("pragma facts");
        assert_eq!(facts_has, 1, "facts.superseded_by must exist after 0018");
        // Decisions' equivalent column predates this migration (0001); the
        // supersede feature relies on it too, so pin its presence.
        let decisions_has: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('decisions') WHERE name = 'superseded_by'",
                [],
                |r| r.get(0),
            )
            .expect("pragma decisions");
        assert_eq!(decisions_has, 1, "decisions.superseded_by must exist");
    }
}
