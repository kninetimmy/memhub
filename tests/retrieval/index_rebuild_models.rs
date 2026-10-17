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
fn rebuild_clears_the_stale_warning_for_vectors_written_by_the_previous_bundled_model() {
    let f = Fixture::new();
    f.rebuild();
    // Relabel the fact's vector as the previously bundled bge-small (384-dim).
    Connection::open(f.repo_db())
        .expect("open db")
        .execute(
            "UPDATE embeddings SET model_name = 'bge-small-en-v1.5', dimension = 384, \
             vector = zeroblob(1536)",
            [],
        )
        .expect("relabel to the previous model");
    let stale_warning = |f: &Fixture| -> bool {
        let out = f.ok(&["recall", "repo key", "--mode", "hybrid", "--json"]);
        let v: Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out}"));
        v["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|w| w["kind"] == "stale_embeddings")
    };

    assert!(stale_warning(&f), "old-model vectors must read as stale");
    f.rebuild();
    assert!(!stale_warning(&f), "rebuild must clear the warning");
    assert_eq!(vector_counts(&f.repo_db()), (0, 1));
}
