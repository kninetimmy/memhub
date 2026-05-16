use std::fs;

use memhub::MemhubError;
use memhub::commands::{
    decision, export, fact, import, init, narrative, pending_write, review, search, session_note,
    task,
};
use memhub::export::v1;
use memhub::models::NarrativeKind;
use tempfile::tempdir;

fn seed_project(path: &std::path::Path) {
    init::run(path).expect("init succeeds");

    fact::add(path, "build-command", "cargo build", "user", "cli:user").expect("fact add");
    fact::add(path, "test-command", "cargo test", "user", "cli:user").expect("fact add");

    decision::add(
        path,
        "Use rusqlite bundled mode",
        "Avoid system SQLite friction.",
        "user",
        "cli:user",
    )
    .expect("decision add");

    task::add(
        path,
        "Wire export command",
        Some("Phase A of M4-001"),
        "cli:user",
    )
    .expect("task add");
}

#[test]
fn export_writes_versioned_json_with_durable_tables() {
    let temp = tempdir().expect("tempdir");
    seed_project(temp.path());

    let dest = temp.path().join("backup").join("export.json");
    let summary = export::run(temp.path(), &dest).expect("export succeeds");

    assert!(dest.exists(), "export file should be written");
    assert_eq!(summary.facts, 2);
    assert_eq!(summary.decisions, 1);
    assert_eq!(summary.tasks, 1);

    let raw = fs::read_to_string(&dest).expect("read export");
    let parsed: v1::Export = serde_json::from_str(&raw).expect("parse export");

    assert_eq!(parsed.memhub_export_version, v1::EXPORT_VERSION);
    assert!(!parsed.source_schema_version.is_empty());
    assert!(parsed.exported_by.starts_with("memhub "));
    assert_eq!(parsed.facts.len(), 2);
    assert_eq!(parsed.decisions.len(), 1);
    assert_eq!(parsed.tasks.len(), 1);
    assert_eq!(
        parsed.project.root_path_at_export,
        temp.path().to_string_lossy()
    );
}

#[test]
fn export_preserves_durable_fields_for_each_table() {
    let temp = tempdir().expect("tempdir");
    seed_project(temp.path());

    let dest = temp.path().join("export.json");
    export::run(temp.path(), &dest).expect("export succeeds");

    let raw = fs::read_to_string(&dest).expect("read export");
    let parsed: v1::Export = serde_json::from_str(&raw).expect("parse export");

    let build_command = parsed
        .facts
        .iter()
        .find(|f| f.key == "build-command")
        .expect("build-command fact present");
    assert_eq!(build_command.value, "cargo build");
    assert_eq!(build_command.source, "user");

    let decision = &parsed.decisions[0];
    assert_eq!(decision.title, "Use rusqlite bundled mode");
    assert_eq!(decision.status, "active");
    assert!(decision.superseded_by.is_none());

    let task = &parsed.tasks[0];
    assert_eq!(task.title, "Wire export command");
    assert_eq!(task.status, "open");
    assert_eq!(task.notes.as_deref(), Some("Phase A of M4-001"));
}

#[test]
fn export_excludes_derived_data() {
    let temp = tempdir().expect("tempdir");
    seed_project(temp.path());

    let dest = temp.path().join("export.json");
    export::run(temp.path(), &dest).expect("export succeeds");

    let raw = fs::read_to_string(&dest).expect("read export");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse json value");
    let object = value.as_object().expect("top-level object");

    for derived in [
        "commits",
        "files",
        "commit_files",
        "chunks",
        "chunk_fts",
        "schema_migrations",
    ] {
        assert!(
            !object.contains_key(derived),
            "export should not include derived table '{}'",
            derived
        );
    }
}

#[test]
fn export_on_empty_project_produces_empty_arrays() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let dest = temp.path().join("export.json");
    let summary = export::run(temp.path(), &dest).expect("export succeeds");

    assert_eq!(summary.facts, 0);
    assert_eq!(summary.decisions, 0);
    assert_eq!(summary.tasks, 0);
    assert_eq!(summary.commands, 0);
    assert_eq!(summary.pending_writes, 0);

    let raw = fs::read_to_string(&dest).expect("read export");
    let parsed: v1::Export = serde_json::from_str(&raw).expect("parse export");

    assert!(parsed.facts.is_empty());
    assert!(parsed.decisions.is_empty());
    assert!(parsed.tasks.is_empty());
}

#[test]
fn export_creates_parent_directory_when_missing() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let dest = temp
        .path()
        .join("does")
        .join("not")
        .join("exist")
        .join("export.json");
    export::run(temp.path(), &dest).expect("export creates intermediate dirs");

    assert!(dest.exists());
}

#[test]
fn import_restores_data_into_empty_target() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());

    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");

    let summary = import::run(target.path(), &export_path, false).expect("import succeeds");

    assert_eq!(summary.facts, 2);
    assert_eq!(summary.decisions, 1);
    assert_eq!(summary.tasks, 1);
    assert!(!summary.forced);

    let facts = fact::list(target.path()).expect("list facts in target");
    let decisions = decision::list(target.path()).expect("list decisions in target");
    let tasks = task::list(target.path(), Some("all")).expect("list tasks in target");

    assert_eq!(facts.len(), 2);
    assert_eq!(decisions.len(), 1);
    assert_eq!(tasks.len(), 1);
    assert_eq!(decisions[0].title, "Use rusqlite bundled mode");
}

#[test]
fn export_import_round_trips_pending_writes_reviewed_at() {
    let source = tempdir().expect("source tempdir");
    init::run(source.path()).expect("source init");

    let pending_id = pending_write::propose_fact(
        source.path(),
        "deploy-command",
        "./deploy.sh",
        "Looks risky.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");
    review::reject(
        source.path(),
        pending_id,
        Some("Untrusted source"),
        "cli:user",
    )
    .expect("reject pending");

    let before = review::show(source.path(), pending_id).expect("show before export");
    let expected_reviewed_at = before.reviewed_at.expect("reject sets reviewed_at");

    let export_path = source.path().join("export.json");
    export::run(source.path(), &export_path).expect("export");

    let raw = fs::read_to_string(&export_path).expect("read export");
    let parsed: v1::Export = serde_json::from_str(&raw).expect("parse");
    assert_eq!(parsed.pending_writes.len(), 1);
    assert_eq!(
        parsed.pending_writes[0].reviewed_at.as_deref(),
        Some(expected_reviewed_at.as_str()),
        "reviewed_at should survive export"
    );

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");
    import::run(target.path(), &export_path, false).expect("import");

    let restored = review::show(target.path(), pending_id).expect("show after import");
    assert_eq!(restored.status, "rejected");
    assert_eq!(
        restored.reviewed_at.as_deref(),
        Some(expected_reviewed_at.as_str()),
        "reviewed_at should survive import"
    );
}

#[test]
fn import_accepts_v1_export_missing_reviewed_at_field() {
    // Exports written before migration 0005 omit reviewed_at entirely.
    // serde(default) on the field should let those imports succeed.
    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");

    let legacy_export = serde_json::json!({
        "memhub_export_version": 1,
        "exported_at": "2025-01-01T00:00:00Z",
        "exported_by": "memhub legacy",
        "source_schema_version": "1",
        "project": {
            "root_path_at_export": target.path().to_string_lossy(),
            "created_at": "2025-01-01T00:00:00Z",
        },
        "facts": [],
        "decisions": [],
        "tasks": [],
        "commands": [],
        "pending_writes": [{
            "id": 1,
            "kind": "fact",
            "payload_json": "{\"key\":\"k\",\"value\":\"v\"}",
            "rationale": "legacy",
            "status": "pending",
            "actor": "codex",
            "actor_raw": "openai-codex",
            "created_at": "2025-01-01T00:00:00Z",
            "provenance_json": "{}",
        }],
        "writes_log": [],
    });
    let legacy_path = target.path().join("legacy.json");
    fs::write(&legacy_path, legacy_export.to_string()).expect("write legacy export");

    import::run(target.path(), &legacy_path, true).expect("legacy import should succeed");

    let restored = review::show(target.path(), 1).expect("show legacy pending");
    assert_eq!(restored.status, "pending");
    assert!(restored.reviewed_at.is_none());
}

#[test]
fn import_refuses_to_overwrite_existing_data_without_force() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());

    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    fact::add(target.path(), "preexisting", "value", "user", "cli:user").expect("seed target fact");

    let result = import::run(target.path(), &export_path, false);
    assert!(result.is_err(), "import without force should refuse");

    let facts = fact::list(target.path()).expect("list facts in target");
    assert_eq!(facts.len(), 1, "preexisting data should remain intact");
    assert_eq!(facts[0].key, "preexisting");
}

#[test]
fn import_with_force_overwrites_existing_data() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());

    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    fact::add(target.path(), "preexisting", "value", "user", "cli:user").expect("seed target fact");
    task::add(target.path(), "target-task", None, "cli:user").expect("seed target task");

    let summary = import::run(target.path(), &export_path, true).expect("forced import succeeds");
    assert!(summary.forced);

    let facts = fact::list(target.path()).expect("list facts in target");
    let keys: Vec<_> = facts.iter().map(|f| f.key.as_str()).collect();
    assert!(
        !keys.contains(&"preexisting"),
        "preexisting fact should be wiped"
    );
    assert!(keys.contains(&"build-command"));
    assert!(keys.contains(&"test-command"));

    let tasks = task::list(target.path(), Some("all")).expect("list tasks in target");
    let titles: Vec<_> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert!(
        !titles.contains(&"target-task"),
        "preexisting task should be wiped"
    );
}

#[test]
fn import_rejects_unsupported_export_version() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let bad_path = temp.path().join("bad-export.json");
    let bad_json = serde_json::json!({
        "memhub_export_version": 99,
        "exported_at": "2026-05-12T00:00:00Z",
        "exported_by": "memhub test",
        "source_schema_version": "0004_pending_write_provenance",
        "project": {
            "root_path_at_export": "/somewhere",
            "created_at": "2026-05-12T00:00:00Z"
        },
        "facts": [],
        "decisions": [],
        "tasks": [],
        "commands": [],
        "pending_writes": [],
        "writes_log": []
    });
    fs::write(&bad_path, serde_json::to_string(&bad_json).unwrap()).expect("write bad export");

    let result = import::run(temp.path(), &bad_path, true);
    assert!(result.is_err(), "import should reject unsupported version");
}

#[test]
fn import_preserves_decision_ids_and_supersession_chain() {
    let source = tempdir().expect("source tempdir");
    init::run(source.path()).expect("source init succeeds");
    let first_id = decision::add(
        source.path(),
        "Pick library A",
        "Initial choice",
        "user",
        "cli:user",
    )
    .expect("first decision");
    let second_id = decision::add(
        source.path(),
        "Pick library B",
        "Library A had bugs",
        "user",
        "cli:user",
    )
    .expect("second decision");

    let ctx = memhub::db::open_project(source.path()).expect("open source");
    ctx.conn
        .execute(
            "UPDATE decisions SET status = 'superseded', superseded_by = ?1 WHERE id = ?2",
            rusqlite::params![second_id, first_id],
        )
        .expect("mark first decision superseded");
    drop(ctx);

    let export_path = source.path().join("export.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    import::run(target.path(), &export_path, false).expect("import succeeds");

    let ctx = memhub::db::open_project(target.path()).expect("open target");
    let mut stmt = ctx
        .conn
        .prepare("SELECT id, status, superseded_by FROM decisions ORDER BY id")
        .expect("prepare query");
    let rows: Vec<(i64, String, Option<i64>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect rows");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, first_id);
    assert_eq!(rows[0].1, "superseded");
    assert_eq!(rows[0].2, Some(second_id));
    assert_eq!(rows[1].0, second_id);
    assert_eq!(rows[1].1, "active");
    assert_eq!(rows[1].2, None);
}

#[test]
fn import_regenerates_decision_chunks_for_fts_search() {
    let source = tempdir().expect("source tempdir");
    init::run(source.path()).expect("source init succeeds");
    decision::add(
        source.path(),
        "Adopt the kraken pattern",
        "Sea creatures organize concurrent workloads cleanly.",
        "user",
        "cli:user",
    )
    .expect("decision add");

    let export_path = source.path().join("export.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    import::run(target.path(), &export_path, false).expect("import succeeds");

    let response = search::run(target.path(), "kraken", 5).expect("search runs");
    assert!(
        !response.results.is_empty(),
        "FTS search should find imported decision after chunk regeneration"
    );
}

#[test]
fn export_import_round_trips_session_notes_and_narratives() {
    let source = tempdir().expect("source tempdir");
    init::run(source.path()).expect("source init");

    session_note::add(
        source.path(),
        "Picked back up after lunch; focusing on retrieval scoring.",
        "user",
        "cli:user",
    )
    .expect("first note");
    session_note::add(
        source.path(),
        "Embeddings cache may be stale — verify before reindex.",
        "codex",
        "openai-codex",
    )
    .expect("second note");

    narrative::set(
        source.path(),
        NarrativeKind::State,
        "Currently building cross-machine export coverage.",
        "user",
        "cli:user",
    )
    .expect("state entry");
    narrative::set(
        source.path(),
        NarrativeKind::Arch,
        "Recall pipeline: SQL+RAG hybrid with stale-penalty.",
        "user",
        "cli:user",
    )
    .expect("arch entry");

    let export_path = source.path().join("export.json");
    let export_summary = export::run(source.path(), &export_path).expect("export succeeds");
    assert_eq!(export_summary.session_notes, 2);
    assert_eq!(export_summary.project_state, 1);
    assert_eq!(export_summary.project_arch, 1);

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");
    let import_summary = import::run(target.path(), &export_path, false).expect("import succeeds");
    assert_eq!(import_summary.session_notes, 2);
    assert_eq!(import_summary.project_state, 1);
    assert_eq!(import_summary.project_arch, 1);

    let notes = session_note::list(target.path(), 50, None, None).expect("list notes");
    let note_texts: Vec<_> = notes.iter().map(|n| n.text.as_str()).collect();
    assert!(
        note_texts
            .iter()
            .any(|t| t.contains("Picked back up after lunch"))
    );
    assert!(
        note_texts
            .iter()
            .any(|t| t.contains("Embeddings cache may be stale"))
    );

    let state = narrative::show(target.path(), NarrativeKind::State)
        .expect("state show")
        .expect("state present");
    assert!(state.body.contains("cross-machine export coverage"));

    let arch = narrative::show(target.path(), NarrativeKind::Arch)
        .expect("arch show")
        .expect("arch present");
    assert!(arch.body.contains("SQL+RAG hybrid"));
}

#[test]
fn import_accepts_legacy_export_without_session_notes_or_narratives() {
    // Exports written before session_notes/project_state/project_arch were
    // added to the format must continue to import cleanly.
    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");

    let legacy_export = serde_json::json!({
        "memhub_export_version": 1,
        "exported_at": "2025-01-01T00:00:00Z",
        "exported_by": "memhub legacy",
        "source_schema_version": "1",
        "project": {
            "root_path_at_export": target.path().to_string_lossy(),
            "created_at": "2025-01-01T00:00:00Z",
        },
        "facts": [],
        "decisions": [],
        "tasks": [],
        "commands": [],
        "pending_writes": [],
        "writes_log": []
    });
    let legacy_path = target.path().join("legacy.json");
    fs::write(&legacy_path, legacy_export.to_string()).expect("write legacy export");

    let summary = import::run(target.path(), &legacy_path, true).expect("legacy import");
    assert_eq!(summary.session_notes, 0);
    assert_eq!(summary.project_state, 0);
    assert_eq!(summary.project_arch, 0);
}

#[test]
fn import_refuses_when_only_session_notes_exist_without_force() {
    // Without force, import must refuse if the target has any durable data —
    // including session notes or narratives, which used to be ignored by
    // count_durable_rows.
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init");
    session_note::add(target.path(), "I exist already.", "user", "cli:user").expect("seed note");

    let result = import::run(target.path(), &export_path, false);
    assert!(
        result.is_err(),
        "import should refuse when target has session notes"
    );

    let notes = session_note::list(target.path(), 50, None, None).expect("list notes");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].text.contains("I exist already"));
}

#[test]
fn export_excludes_embeddings_and_chunks_to_keep_format_portable() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let dest = source.path().join("export.json");
    export::run(source.path(), &dest).expect("export succeeds");

    let raw = fs::read_to_string(&dest).expect("read export");
    let object = serde_json::from_str::<serde_json::Value>(&raw)
        .expect("parse json")
        .as_object()
        .expect("top-level object")
        .clone();

    for derived in ["embeddings", "chunks"] {
        assert!(
            !object.contains_key(derived),
            "export should not include derived table '{}'",
            derived
        );
    }
}

#[test]
fn import_logs_audit_entry_for_the_restore_event() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let export_path = source.path().join("export.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    import::run(target.path(), &export_path, false).expect("import succeeds");

    let ctx = memhub::db::open_project(target.path()).expect("open target");
    let count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM writes_log WHERE table_name = 'import' AND action = 'import'",
            [],
            |row| row.get(0),
        )
        .expect("query writes_log");
    assert_eq!(count, 1, "expected one import audit entry");
}

#[test]
fn open_project_errors_when_memhub_exists_but_db_is_missing() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let db_path = temp.path().join(".memhub").join("project.sqlite");
    fs::remove_file(&db_path).expect("remove db file");

    let result = memhub::db::open_project(temp.path());
    match result {
        Err(MemhubError::MissingDatabase {
            db_path: missing_db,
            ..
        }) => {
            assert_eq!(missing_db, db_path);
        }
        Err(other) => panic!("expected MissingDatabase, got {other:?}"),
        Ok(_) => panic!("open should fail when db is missing"),
    }
}

#[test]
fn init_refuses_to_silently_recreate_db_inside_existing_memhub() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let db_path = temp.path().join(".memhub").join("project.sqlite");
    fs::remove_file(&db_path).expect("remove db file");

    let err = init::run(temp.path()).expect_err("init should refuse");
    assert!(
        matches!(err, MemhubError::MissingDatabase { .. }),
        "expected MissingDatabase, got {err:?}"
    );
    assert!(
        !db_path.exists(),
        "init must not silently recreate the database"
    );
}

#[test]
fn init_from_backup_recovers_when_db_is_missing() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    let target_db = target.path().join(".memhub").join("project.sqlite");
    fs::remove_file(&target_db).expect("remove target db");

    let (init_result, import_summary) =
        init::run_with_backup(target.path(), &export_path).expect("recovery succeeds");

    assert!(init_result.memhub_preexisting);
    assert!(target_db.exists());
    assert_eq!(import_summary.facts, 2);
    assert_eq!(import_summary.decisions, 1);
    assert_eq!(import_summary.tasks, 1);

    let facts = fact::list(target.path()).expect("list facts");
    assert_eq!(facts.len(), 2);
}

#[test]
fn init_from_backup_works_when_memhub_directory_does_not_exist() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    let target_memhub = target.path().join(".memhub");
    assert!(
        !target_memhub.exists(),
        "target should not yet have .memhub/"
    );

    let (init_result, import_summary) =
        init::run_with_backup(target.path(), &export_path).expect("recovery succeeds");

    assert!(!init_result.memhub_preexisting);
    assert!(target_memhub.join("project.sqlite").exists());
    assert_eq!(import_summary.facts, 2);

    let decisions = decision::list(target.path()).expect("list decisions");
    assert_eq!(decisions.len(), 1);
}

#[test]
fn init_from_backup_refuses_when_db_already_exists() {
    let source = tempdir().expect("source tempdir");
    seed_project(source.path());
    let export_path = source.path().join("backup.json");
    export::run(source.path(), &export_path).expect("export succeeds");

    let target = tempdir().expect("target tempdir");
    init::run(target.path()).expect("target init succeeds");
    fact::add(target.path(), "preexisting", "value", "user", "cli:user").expect("seed fact");

    let result = init::run_with_backup(target.path(), &export_path);
    match result {
        Err(MemhubError::InvalidInput(_)) => {}
        Err(other) => panic!("expected InvalidInput, got {other:?}"),
        Ok(_) => panic!("recovery should refuse when db already exists"),
    }

    let facts = fact::list(target.path()).expect("list facts");
    assert_eq!(
        facts.len(),
        1,
        "preexisting data should remain when recovery refuses"
    );
}
