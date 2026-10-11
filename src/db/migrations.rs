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
    (
        "0019_metrics_maintenance_debounce",
        include_str!("../../migrations/0019_metrics_maintenance_debounce.sql"),
    ),
    (
        "0020_recall_metrics_surface",
        include_str!("../../migrations/0020_recall_metrics_surface.sql"),
    ),
    (
        "0021_fact_kind",
        include_str!("../../migrations/0021_fact_kind.sql"),
    ),
    (
        "0022_source_type_note",
        include_str!("../../migrations/0022_source_type_note.sql"),
    ),
    // Wave 6 W3 (issue #96): session-transcript archive pointer table.
    // Number 0023 was pre-assigned so it could land after the sibling
    // Wave 6 migrations 0021/0022 (issues #97/#98) regardless of merge
    // order. Kept in numeric order so `latest_version()` stays correct.
    (
        "0023_session_transcripts",
        include_str!("../../migrations/0023_session_transcripts.sql"),
    ),
    (
        "0024_session_note_provenance",
        include_str!("../../migrations/0024_session_note_provenance.sql"),
    ),
    (
        ARCH_SECTIONS_MIGRATION,
        include_str!("../../migrations/0025_arch_sections.sql"),
    ),
    (
        SUMMARIES_PORTER_FTS_MIGRATION,
        include_str!("../../migrations/0026_summaries_porter_fts.sql"),
    ),
];

/// Migration that adds fact/task summaries and recreates the six recall
/// FTS tables with porter stemming (issue #349).
const SUMMARIES_PORTER_FTS_MIGRATION: &str = "0026_summaries_porter_fts";

/// Migration that adds the derived `arch_sections` table. SQL cannot
/// chunk markdown, so when this one is applied `apply_all` derives the
/// sections of an already-stored latest architecture body in Rust.
const ARCH_SECTIONS_MIGRATION: &str = "0025_arch_sections";

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

    // After the whole batch, so the derive writes against the head schema.
    // FTS-only: ordinary project open never loads the embedding model;
    // `memhub index rebuild` embeds these in hybrid mode.
    if applied.iter().any(|v| v == ARCH_SECTIONS_MIGRATION) {
        crate::commands::narrative::rederive_arch_sections(&tx, crate::config::RetrievalMode::Fts)?;
    }

    tx.commit()?;
    Ok(applied)
}

/// Test-only: a DB left by the binary before migration 0026 (schema at
/// 0025), seeded with one row per recall FTS table whose text says
/// "migration", plus one embedding per fact/decision/task. Shared by the
/// 0026 migration test and doctor's upgraded-DB test.
#[cfg(test)]
pub(crate) fn seeded_pre_0026_db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
             version TEXT PRIMARY KEY,
             applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );",
    )
    .expect("create migration ledger");
    for (version, sql) in MIGRATIONS
        .iter()
        .take_while(|(v, _)| *v != SUMMARIES_PORTER_FTS_MIGRATION)
    {
        conn.execute_batch(sql).expect("apply legacy migration");
        conn.execute(
            "INSERT INTO schema_migrations(version) VALUES (?1)",
            [version],
        )
        .expect("record legacy migration");
    }
    conn.execute_batch(
        "INSERT INTO projects(id, root_path, schema_version) VALUES (1, 'test', 'test');
         INSERT INTO facts(id, key, value) VALUES (1, 'db-move', 'the schema migration runs on open');
         INSERT INTO decisions(id, title, rationale) VALUES (1, 'Ship one migration', 'fewer steps');
         INSERT INTO tasks(id, title, notes) VALUES (1, 'Plan the migration', 'before release');
         INSERT INTO documents(id, path, title, content_hash, byte_len)
             VALUES (1, 'doc.md', 'Doc', 'h', 1);
         INSERT INTO doc_chunks(id, doc_id, ord, heading_path, body)
             VALUES (1, 1, 0, 'Upgrade', 'a migration guide');
         INSERT INTO session_notes(id, actor, actor_raw, text)
             VALUES (1, 'user', 'cli:user', 'paused mid migration');
         INSERT INTO project_arch(id, body, actor, actor_raw)
             VALUES (1, '# Storage', 'user', 'cli:user');
         INSERT INTO arch_sections(id, arch_id, ord, heading_path, body)
             VALUES (1, 1, 0, 'Storage', 'each migration is ordered');
         INSERT INTO embeddings(source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES ('fact', 1, 'test-model', 1, X'01', 'fact-hash'),
                    ('decision', 1, 'test-model', 1, X'01', 'decision-hash'),
                    ('task', 1, 'test-model', 1, X'01', 'task-hash');",
    )
    .expect("seed legacy rows");
    conn
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

    /// Migration 0020 (issue #70 / gate Q17) adds the recall-surface tag
    /// column to `recall_metrics`. Additive `ALTER TABLE ADD COLUMN`, so
    /// a fresh DB simply has the column, nullable; replay safety is
    /// already covered by `idempotent_reapply_is_a_noop`.
    #[test]
    fn migration_0020_adds_recall_metrics_surface_column() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");
        let has_surface: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('recall_metrics') WHERE name = 'surface'",
                [],
                |r| r.get(0),
            )
            .expect("pragma recall_metrics");
        assert_eq!(
            has_surface, 1,
            "recall_metrics.surface must exist after 0020"
        );
    }

    /// Migration 0022 (Wave 6 W5, issue #98) widens `embeddings.source_type`
    /// to admit 'note' and adds `session_notes_fts`. Mirrors 0014's dance
    /// for 'doc_chunk'; assert both landed and every pre-existing
    /// source_type still inserts cleanly (the CHECK-rebuild must not
    /// narrow anything, and the rebuilt table's UNIQUE/indexes must still
    /// exist so the insert+cleanup round-trips for each type).
    #[test]
    fn migration_0022_widens_embeddings_check_and_adds_session_notes_fts() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");
        // `embeddings.project_id` carries a real FK to `projects(id)` and
        // this connection (unlike `db::open_project`) enforces foreign
        // keys, so seed the row it references before inserting.
        conn.execute(
            "INSERT INTO projects(id, root_path, schema_version) VALUES (1, 'test', 'test')",
            [],
        )
        .expect("seed projects row");

        for source_type in ["fact", "decision", "task", "doc_chunk", "note"] {
            conn.execute(
                "INSERT INTO embeddings(
                    project_id, source_type, source_id, model_name,
                    dimension, vector, content_hash
                 ) VALUES (1, ?1, 1, 'test-model', 1, X'00', 'hash')",
                [source_type],
            )
            .unwrap_or_else(|e| {
                panic!("embeddings CHECK must admit source_type '{source_type}': {e}")
            });
            conn.execute(
                "DELETE FROM embeddings WHERE source_type = ?1",
                [source_type],
            )
            .expect("cleanup");
        }

        let fts_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'session_notes_fts'",
                [],
                |r| r.get(0),
            )
            .expect("pragma session_notes_fts");
        assert_eq!(fts_exists, 1, "session_notes_fts must exist after 0022");
    }

    /// Migration 0021 (Wave 6 W4, issue #97) adds the optional `kind` tag
    /// to `facts`. Additive `ALTER TABLE ADD COLUMN`, no CHECK constraint
    /// (deliberately unenforced vocabulary); replay safety is already
    /// covered by `idempotent_reapply_is_a_noop`.
    #[test]
    fn migration_0021_adds_facts_kind_column() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");
        let has_kind: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name = 'kind'",
                [],
                |r| r.get(0),
            )
            .expect("pragma facts");
        assert_eq!(has_kind, 1, "facts.kind must exist after 0021");
    }

    /// Migration 0023 (Wave 6 W3, issue #96) creates the
    /// `session_transcripts` pointer table via `CREATE TABLE IF NOT
    /// EXISTS`. A fresh DB simply has the table with its full column set;
    /// replay safety is covered by `idempotent_reapply_is_a_noop`.
    #[test]
    fn migration_0023_creates_session_transcripts_table() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");

        let table_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'session_transcripts'",
                [],
                |r| r.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(table_exists, 1, "session_transcripts must exist after 0023");

        // Pin the pointer-row columns so a later schema edit that drops or
        // renames one is caught here, not at an archive-time INSERT.
        for col in [
            "session_id",
            "agent",
            "source_path",
            "archive_path",
            "source_bytes",
            "archive_bytes",
            "created_at",
        ] {
            let has: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('session_transcripts') \
                     WHERE name = ?1",
                    [col],
                    |r| r.get(0),
                )
                .expect("pragma session_transcripts");
            assert_eq!(has, 1, "session_transcripts.{col} must exist after 0023");
        }
    }

    #[test]
    fn migration_0024_preserves_legacy_notes_and_adds_provenance_columns() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .expect("create migration ledger");
        // Every migration but 0024 counts as applied, so only 0024 runs.
        for (version, _) in MIGRATIONS
            .iter()
            .filter(|(v, _)| *v != "0024_session_note_provenance")
        {
            conn.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [version],
            )
            .expect("mark legacy migration applied");
        }
        conn.execute_batch(
            "CREATE TABLE projects (id INTEGER PRIMARY KEY);
             INSERT INTO projects(id) VALUES (1);",
        )
        .expect("create legacy project row");
        conn.execute_batch(include_str!("../../migrations/0006_session_notes.sql"))
            .expect("create legacy session_notes table");
        conn.execute(
            "INSERT INTO session_notes(project_id, actor, actor_raw, text)
             VALUES (1, 'opencode', 'cli', 'legacy note')",
            [],
        )
        .expect("insert legacy note");

        let applied = apply_all(&mut conn).expect("auto-apply 0024");
        assert_eq!(applied, vec!["0024_session_note_provenance"]);
        for column in [
            "session_id",
            "agent_id",
            "provider_id",
            "model_id",
            "variant",
        ] {
            let present: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('session_notes') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .expect("inspect migrated session_notes");
            assert_eq!(present, 1, "session_notes.{column} must exist after 0024");
        }
        let legacy: (String, Option<String>) = conn
            .query_row("SELECT text, session_id FROM session_notes", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("read migrated legacy note");
        assert_eq!(legacy, ("legacy note".to_string(), None));
    }

    /// Migration 0025 (issue #232) on a DB built by the previous head:
    /// every existing embeddings row (all five prior source types)
    /// survives the CHECK-widening rebuild, and an architecture body
    /// stored before the migration gets its heading sections without a
    /// new `arch set` and without any embedding (FTS-only on open).
    #[test]
    fn migration_0025_preserves_embeddings_and_derives_existing_arch_sections() {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                 version TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )
        .expect("create migration ledger");
        for (version, sql) in MIGRATIONS
            .iter()
            .take_while(|(v, _)| *v != ARCH_SECTIONS_MIGRATION)
        {
            conn.execute_batch(sql).expect("apply legacy migration");
            conn.execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [version],
            )
            .expect("record legacy migration");
        }
        conn.execute(
            "INSERT INTO projects(id, root_path, schema_version) VALUES (1, 'test', 'test')",
            [],
        )
        .expect("seed projects row");
        for source_type in ["fact", "decision", "task", "doc_chunk", "note"] {
            conn.execute(
                "INSERT INTO embeddings(
                    project_id, source_type, source_id, model_name,
                    dimension, vector, content_hash
                 ) VALUES (1, ?1, 7, 'test-model', 1, X'01', 'hash')",
                [source_type],
            )
            .expect("seed legacy embedding");
        }
        let before: Vec<(i64, String, i64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, source_type, source_id, content_hash FROM embeddings ORDER BY id",
                )
                .expect("prepare");
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("collect")
        };
        conn.execute(
            "INSERT INTO project_arch(project_id, body, actor, actor_raw)
             VALUES (1, '# System\n\nIntro.\n\n## Storage\n\nSQLite holds the quokkastore.', 'user', 'cli:user')",
            [],
        )
        .expect("seed legacy arch body");

        let applied = apply_all(&mut conn).expect("auto-apply 0025");
        assert_eq!(
            applied,
            vec![ARCH_SECTIONS_MIGRATION, SUMMARIES_PORTER_FTS_MIGRATION]
        );

        let after: Vec<(i64, String, i64, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, source_type, source_id, content_hash FROM embeddings ORDER BY id",
                )
                .expect("prepare");
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("collect")
        };
        assert_eq!(before, after, "every pre-0025 embedding row must survive");

        let paths: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT heading_path FROM arch_sections ORDER BY ord")
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("collect")
        };
        assert_eq!(paths, vec!["", "Storage"], "the System wrapper is left out");
        let fts_hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM arch_sections_fts WHERE arch_sections_fts MATCH 'quokkastore'",
                [],
                |r| r.get(0),
            )
            .expect("fts query");
        assert_eq!(fts_hits, 1, "derived sections must be FTS-indexed");
        let arch_vectors: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM embeddings WHERE source_type = 'arch_section'",
                [],
                |r| r.get(0),
            )
            .expect("count arch vectors");
        assert_eq!(arch_vectors, 0, "open-time derive must not embed");
        conn.execute(
            "INSERT INTO embeddings(
                project_id, source_type, source_id, model_name,
                dimension, vector, content_hash
             ) VALUES (1, 'arch_section', 1, 'test-model', 1, X'01', 'hash')",
            [],
        )
        .expect("widened CHECK must admit 'arch_section'");
    }

    /// Migration 0026 (issue #349) on a DB built by the previous head:
    /// every recall FTS table is rebuilt with porter stemming, so each
    /// pre-existing row is still found, now by another inflection of its
    /// word; the summary columns exist and are NULL; existing embeddings
    /// are untouched; the FTS table outside recall keeps unicode61; and a
    /// second apply changes nothing.
    #[test]
    fn migration_0026_rebuilds_recall_fts_with_porter_and_keeps_rows() {
        let mut conn = seeded_pre_0026_db();
        let read_embeddings = |conn: &Connection| -> Vec<(String, String)> {
            let mut stmt = conn
                .prepare("SELECT source_type, content_hash FROM embeddings ORDER BY id")
                .expect("prepare");
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .expect("query")
                .collect::<std::result::Result<_, _>>()
                .expect("collect")
        };
        let embeddings_before = read_embeddings(&conn);

        let applied = apply_all(&mut conn).expect("auto-apply 0026");
        assert_eq!(applied, vec![SUMMARIES_PORTER_FTS_MIGRATION]);

        for fts in [
            "facts_fts",
            "decisions_fts",
            "tasks_fts",
            "doc_chunks_fts",
            "session_notes_fts",
            "arch_sections_fts",
        ] {
            let hits: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH '\"migrating\"'"),
                    [],
                    |r| r.get(0),
                )
                .expect("fts query");
            assert_eq!(hits, 1, "{fts} must find its pre-0026 row by a stemmed word");
        }
        for table in ["facts", "tasks"] {
            let summary: Option<String> = conn
                .query_row(&format!("SELECT summary FROM {table}"), [], |r| r.get(0))
                .expect("summary column");
            assert_eq!(summary, None, "{table}.summary starts empty");
        }
        assert_eq!(embeddings_before, read_embeddings(&conn));

        let porter_outside_recall: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE name = 'chunk_fts' AND sql LIKE '%porter%'",
                [],
                |r| r.get(0),
            )
            .expect("chunk_fts sql");
        assert_eq!(porter_outside_recall, 0, "chunk_fts keeps unicode61");

        assert!(apply_all(&mut conn).expect("re-apply").is_empty());
    }

    /// The summary columns are indexed: a word that appears only in a
    /// fact's, decision's or task's summary finds that row.
    #[test]
    fn summary_only_words_are_full_text_searchable() {
        let mut conn = Connection::open_in_memory().expect("open");
        apply_all(&mut conn).expect("apply");
        conn.execute_batch(
            "INSERT INTO projects(id, root_path, schema_version) VALUES (1, 'test', 'test');
             INSERT INTO facts(key, value) VALUES ('k', 'v');
             INSERT INTO decisions(title, rationale) VALUES ('t', 'r');
             INSERT INTO tasks(title) VALUES ('t');
             UPDATE facts SET summary = 'which port does the quokka use';
             UPDATE decisions SET summary = 'why the quokka';
             UPDATE tasks SET summary = 'quokka work';",
        )
        .expect("seed");
        for fts in ["facts_fts", "decisions_fts", "tasks_fts"] {
            let hits: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH 'quokkas'"),
                    [],
                    |r| r.get(0),
                )
                .expect("fts query");
            assert_eq!(hits, 1, "{fts} must index the summary column");
        }
    }
}
