use memhub::commands::init;
use memhub::db;
use tempfile::tempdir;

// Every test below calls `db::open_project`, which calls `db::discover_paths`,
// which resolves `db::home_dir()` unconditionally as its first line (Wave 5
// U4, issue #90) — so each takes `support::env_read_lock()` for the whole
// test, guarding against a concurrent writer test's `HOME`/`USERPROFILE`
// override elsewhere in this shared harness binary. See `upgrade/support.rs`.

/// A clean clone on a new machine has no `.memhub/` and no DB. The first
/// `memhub` call must bring the schema up to the head version on its own
/// — no manual `migrate` step. This locks in the cross-machine claim in
/// CLAUDE.md / AGENTS.md so a future regression that broke auto-apply
/// would be caught here instead of in production.
#[test]
fn fresh_init_applies_all_migrations_to_head() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let ctx = db::open_project(temp.path()).expect("open project");
    let mut stmt = ctx
        .conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .expect("prepare");
    let versions: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect");

    assert!(
        versions.contains(&"0001_initial".to_string()),
        "expected the initial migration to be applied; got {versions:?}"
    );
    assert!(
        versions.contains(&db::latest_schema_version().to_string()),
        "expected head migration {} to be applied; got {versions:?}",
        db::latest_schema_version()
    );
}

/// Cross-machine fidelity: a DB last touched by an older build is missing
/// `schema_migrations` rows for migrations introduced on the other
/// machine. Every `db::open_project` call invokes `migrations::apply_all`,
/// which fills the gap. This test forces that scenario by deleting a
/// `schema_migrations` row and verifying the next open re-applies it.
///
/// The probe migration is pinned to `0010_embeddings_delete_triggers`
/// (CREATE TRIGGER IF NOT EXISTS — idempotent at the SQL level) rather
/// than head, because the simulation deletes only the gating row and not
/// the schema effect, so a non-idempotent head migration (e.g.
/// `ALTER TABLE … ADD COLUMN`) would fail with `duplicate column` on
/// re-apply. Keeping the probe pinned isolates this test to the gap-fill
/// mechanism without coupling it to whichever migration happens to be
/// newest.
#[test]
fn open_project_reapplies_a_missing_migration_row() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let probe = "0010_embeddings_delete_triggers";

    {
        let ctx = db::open_project(temp.path()).expect("open project");
        let removed = ctx
            .conn
            .execute("DELETE FROM schema_migrations WHERE version = ?1", [probe])
            .expect("simulate stale schema");
        assert_eq!(
            removed, 1,
            "test setup error: expected to remove exactly one schema_migrations row"
        );
    }

    // Next open_project should detect the missing row and re-apply.
    let ctx = db::open_project(temp.path()).expect("re-open after stale schema");
    let count: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
            [probe],
            |row| row.get(0),
        )
        .expect("count probe migration rows");
    assert_eq!(count, 1, "missing migration row should be auto-applied");
}

/// `open_project` runs `apply_all` on every call. If migrations are not
/// idempotent — e.g. someone adds a migration that re-creates a table
/// without `IF NOT EXISTS` — that would surface as an error or a
/// duplicate row here.
#[test]
fn open_project_is_idempotent_against_an_already_migrated_db() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let count_before: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count before")
    };

    // Open three more times — each call invokes apply_all internally.
    for _ in 0..3 {
        let _ = db::open_project(temp.path()).expect("re-open project");
    }

    let count_after: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count after")
    };

    assert_eq!(
        count_before, count_after,
        "repeated open_project calls should not duplicate or add schema_migrations rows"
    );
}

const PROBE: &str = "0010_embeddings_delete_triggers";

/// Init a repo holding one fact, then drop `PROBE`'s ledger row so the next
/// open has a pending migration (a gap). Same simulation as above.
fn repo_behind_by_one(root: &std::path::Path) -> std::path::PathBuf {
    init::run(root).expect("init succeeds");
    memhub::commands::fact::add(root, "keeper", "1", "user", "cli:user").expect("fact");
    let ctx = db::open_project(root).expect("open project");
    ctx.conn
        .execute("DELETE FROM schema_migrations WHERE version = ?1", [PROBE])
        .expect("simulate stale schema");
    db::pre_migration_backup_path(&ctx.paths)
}

fn has_version(conn: &rusqlite::Connection, version: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        [version],
        |r| r.get(0),
    )
    .expect("query ledger")
}

/// Issue #405: an open with a pending migration first copies the DB as it
/// was to the single pre-migration slot, replacing an earlier copy.
#[test]
fn open_with_pending_migration_writes_pre_migration_backup() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    let backup = repo_behind_by_one(temp.path());
    std::fs::create_dir_all(backup.parent().unwrap()).expect("mkdir");
    std::fs::write(&backup, b"an earlier copy").expect("seed prior slot");

    let ctx = db::open_project(temp.path()).expect("open migrates");
    assert!(has_version(&ctx.conn, PROBE), "live DB is migrated");

    let copy = rusqlite::Connection::open(&backup).expect("backup opens");
    assert!(
        !has_version(&copy, PROBE),
        "backup holds the pre-migration ledger"
    );
    assert!(has_version(&copy, "0001_initial"));
    let facts: i64 = copy
        .query_row("SELECT COUNT(*) FROM facts WHERE key = 'keeper'", [], |r| {
            r.get(0)
        })
        .expect("count facts");
    assert_eq!(facts, 1, "backup holds the pre-migration rows");
    let entries = std::fs::read_dir(backup.parent().unwrap()).unwrap().count();
    assert_eq!(entries, 1, "only the slot remains: no staged temp copy");
}

/// Issue #405: neither creating a DB nor opening one at head writes a
/// pre-migration backup.
#[test]
fn fresh_init_and_up_to_date_open_write_no_pre_migration_backup() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");
    let ctx = db::open_project(temp.path()).expect("open at head");
    let backup = db::pre_migration_backup_path(&ctx.paths);
    assert!(
        !backup.exists(),
        "no backup for a fresh init or a head open"
    );
    assert!(!backup.parent().unwrap().exists());
}

/// Issue #405: when the pre-migration backup cannot be written, the open
/// fails naming the backup path and applies no migration.
#[test]
fn unwritable_pre_migration_backup_leaves_db_unmigrated() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    let backup = repo_behind_by_one(temp.path());
    // A regular file where the slot's directory belongs.
    std::fs::create_dir_all(backup.parent().unwrap().parent().unwrap()).expect("mkdir");
    std::fs::write(backup.parent().unwrap(), b"not a directory").expect("occupy");

    let err = match db::open_project(temp.path()) {
        Ok(_) => panic!("open must fail when the backup cannot be written"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains(&backup.display().to_string()),
        "error names the backup path: {err}"
    );
    let db_path = temp.path().join(".memhub").join("project.sqlite");
    let conn = rusqlite::Connection::open(&db_path).expect("open raw");
    assert!(!has_version(&conn, PROBE), "no pending migration applied");
}
