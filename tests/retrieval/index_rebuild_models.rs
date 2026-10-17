//! `memhub index rebuild` leaves each store holding vectors only for the
//! bundled embedding model: it re-embeds the repo DB and, for a repo opted in
//! to the machine-global store, the global store too, removing vectors
//! written under any other model.
//!
//! Every memhub call is a spawned child with `HOME` and `USERPROFILE`
//! pointed at a tempdir, so the real `~/.memhub/global.sqlite` is never
//! reached and no in-process env override is needed.

use std::path::{Path, PathBuf};
use std::process::Command;

use memhub::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    home: TempDir,
    repo: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let f = Fixture {
            home: tempdir().expect("home"),
            repo: tempdir().expect("repo"),
        };
        f.ok(&["init"]);
        f.ok(&["fact", "add", "repo key", "repo value"]);
        f
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("MEMHUB_LOG", "off")
            .output()
            .expect("spawn memhub");
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn rebuild(&self) -> Value {
        let stdout = self.ok(&["index", "rebuild", "--json"]);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"))
    }

    fn repo_db(&self) -> PathBuf {
        self.repo.path().join(".memhub").join("project.sqlite")
    }

    fn global_db(&self) -> PathBuf {
        self.home.path().join(".memhub").join("global.sqlite")
    }
}

/// Plant a vector for fact 1 under a model the binary does not bundle.
fn plant_foreign_vector(db: &Path) {
    Connection::open(db)
        .expect("open db")
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, \
             dimension, vector, content_hash) \
             VALUES (1, 'fact', 1, 'other-model', 8, zeroblob(32), 'stale')",
            [],
        )
        .expect("plant foreign vector");
}

/// `(rows under another model, rows under the bundled model at its full
/// dimension)`.
fn vector_counts(db: &Path) -> (i64, i64) {
    let conn = Connection::open(db).expect("open db");
    let count = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> i64 {
        conn.query_row(sql, p, |r| r.get(0)).expect("count")
    };
    (
        count(
            "SELECT COUNT(*) FROM embeddings WHERE model_name <> ?1",
            &[&EMBEDDING_MODEL_NAME],
        ),
        count(
            "SELECT COUNT(*) FROM embeddings \
             WHERE model_name = ?1 AND dimension = ?2 AND LENGTH(vector) = ?3",
            &[
                &EMBEDDING_MODEL_NAME,
                &(EMBEDDING_DIMENSION as i64),
                &((EMBEDDING_DIMENSION * 4) as i64),
            ],
        ),
    )
}

#[test]
fn rebuild_drops_other_model_vectors_in_repo_and_opted_in_global_store() {
    let f = Fixture::new();
    f.ok(&["global", "enable"]);
    f.ok(&["fact", "add", "--global", "global key", "global value"]);
    plant_foreign_vector(&f.repo_db());
    plant_foreign_vector(&f.global_db());

    let out = f.rebuild();

    assert_eq!(out["facts"], 1);
    assert_eq!(out["deleted"], 1);
    assert_eq!(out["global"]["facts"], 1, "global counts reported: {out}");
    assert_eq!(out["global"]["deleted"], 1);
    assert_eq!(vector_counts(&f.repo_db()), (0, 1));
    assert_eq!(vector_counts(&f.global_db()), (0, 1));
}

#[test]
fn rebuild_leaves_the_global_store_alone_when_the_repo_is_not_opted_in() {
    let f = Fixture::new();
    // Create the store, then opt back out: the store stays on disk.
    f.ok(&["global", "enable"]);
    f.ok(&["fact", "add", "--global", "global key", "global value"]);
    f.ok(&["global", "disable"]);
    plant_foreign_vector(&f.repo_db());
    plant_foreign_vector(&f.global_db());

    let out = f.rebuild();

    assert!(out["global"].is_null(), "no global counts: {out}");
    assert_eq!(vector_counts(&f.repo_db()), (0, 1));
    assert_eq!(
        vector_counts(&f.global_db()),
        (1, 0),
        "the global store must be untouched"
    );
}

#[test]
fn rebuild_does_not_create_the_global_store_for_an_opted_in_repo() {
    let f = Fixture::new();
    f.ok(&["global", "enable"]);
    std::fs::remove_file(f.global_db()).expect("remove global store");

    let out = f.rebuild();

    assert!(out["global"].is_null(), "no global counts: {out}");
    assert!(!f.global_db().exists(), "rebuild must not create the store");
}

#[test]
fn rebuild_reports_the_repo_result_when_the_global_store_refuses() {
    let f = Fixture::new();
    f.ok(&["global", "enable"]);
    plant_foreign_vector(&f.repo_db());
    // A store whose ledger is ahead of this binary is refused by open_global.
    Connection::open(f.global_db())
        .expect("open global")
        .execute(
            "INSERT INTO schema_migrations(version) VALUES ('9999_from_a_newer_memhub')",
            [],
        )
        .expect("add future migration row");

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
            .current_dir(f.repo.path())
            .env("HOME", f.home.path())
            .env("USERPROFILE", f.home.path())
            .env("MEMHUB_LOG", "off")
            .output()
            .expect("spawn memhub")
    };

    let json = run(&["index", "rebuild", "--json"]);
    assert!(!json.status.success(), "global failure must exit non-zero");
    let stdout = String::from_utf8_lossy(&json.stdout);
    let out: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(out["facts"], 1, "repo result still reported: {out}");
    assert_eq!(out["deleted"], 1);
    assert!(out["global"].is_null());
    assert!(
        out["global_error"].as_str().is_some_and(|e| !e.is_empty()),
        "global error reported: {out}"
    );
    assert_eq!(vector_counts(&f.repo_db()), (0, 1), "repo store committed");

    let human = run(&["index", "rebuild"]);
    assert!(!human.status.success());
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("repo:"), "repo line printed: {stdout}");
    assert!(
        stdout.contains("global: rebuild failed:"),
        "global error printed: {stdout}"
    );
}
