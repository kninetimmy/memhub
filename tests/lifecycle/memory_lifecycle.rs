use std::{fs, path::Path, process::Command};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Repo {
    home: TempDir,
    dir: TempDir,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            home: tempdir().unwrap(),
            dir: tempdir().unwrap(),
        };
        repo.ok(&["init"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
            .current_dir(self.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("MEMHUB_LOG", "off")
            .args(args)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (ok, stdout, stderr) = self.run(args);
        assert!(ok, "{args:?}: {stdout}{stderr}");
        stdout
    }

    fn fails_with(&self, args: &[&str], hint: &str) {
        let (ok, stdout, stderr) = self.run(args);
        assert!(!ok && stderr.contains(hint), "{args:?}: {stdout}{stderr}");
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }

    fn conn(&self) -> Connection {
        Connection::open(self.path().join(".memhub/project.sqlite")).unwrap()
    }

    fn recall(&self, query: &str, mode: &str, scope: Option<&str>) -> Value {
        let mut args = vec!["recall", query, "--mode", mode, "--no-rerank", "--json"];
        if let Some(scope) = scope {
            args.extend(["--source-type", scope]);
        }
        self.json(&args)
    }

    fn assert_logged(&self, action: &str, reason: &str) {
        let logged: (String, String) = self
            .conn()
            .query_row(
                "SELECT actor, reason FROM writes_log WHERE action = ?1",
                [action],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(logged, (format!("test:{action}"), reason.into()));
    }
}

#[test]
fn memory_lifecycle_facts_and_docs_leave_recall_and_return_unchanged() {
    let repo = Repo::new();
    let fact = repo.json(&["fact", "add", "kwamak", "kwamak old value", "--json"]);
    let fact_id = fact["id"].to_string();
    repo.conn()
        .execute("UPDATE facts SET verified_at = '2000-01-01'", [])
        .unwrap();
    let original = repo.json(&["fact", "list", "--json"]);
    fs::write(
        repo.path().join("guide.md"),
        "# Guide\n\nkwamak old passage\n",
    )
    .unwrap();
    let doc = repo.json(&["doc", "add", "guide.md", "--json"]);
    let doc_id = doc["id"].to_string();
    let original_doc = repo.json(&["doc", "show", &doc_id, "--json"]);
    let named_fact = format!("fact {fact_id}");
    assert_eq!(repo.recall(&named_fact, "fts", None)["returned_count"], 1);
    assert_eq!(
        repo.recall("kwamak", "fts", Some("doc"))["returned_count"],
        1
    );

    repo.ok(&[
        "fact",
        "retire",
        "kwamak",
        "--reason",
        "obsolete",
        "--actor",
        "test:retire",
    ]);
    repo.ok(&[
        "doc",
        "archive",
        "guide.md",
        "--reason",
        "old guide",
        "--actor",
        "test:archive",
    ]);
    let facts = repo.json(&["fact", "list", "--json"]);
    assert_eq!(facts["facts"].as_array().unwrap().len(), 0);
    let facts = repo.json(&["fact", "list", "--include-retired", "--json"]);
    assert_eq!(facts["facts"][0]["retired_reason"], "obsolete");
    assert!(facts["facts"][0]["retired_at"].is_string());
    assert!(
        repo.ok(&["fact", "list", "--include-retired"])
            .contains("obsolete")
    );
    assert!(
        repo.ok(&["get-row", "fact", &fact_id])
            .contains("kwamak old value")
    );
    assert_eq!(
        repo.json(&["doc", "ls", "--json"])["docs"][0]["archived_reason"],
        "old guide"
    );
    assert!(repo.ok(&["doc", "ls"]).contains("archived at"));
    assert_eq!(repo.json(&["doc", "show", &doc_id, "--json"]), original_doc);

    for mode in ["fts", "hybrid"] {
        for query in ["kwamak", &named_fact] {
            let result = repo.recall(query, mode, None);
            assert_eq!(result["returned_count"], 0, "{mode}: {result}");
            assert_eq!(result["available_docs"], 0, "{mode}: {result}");
        }
        for source_type in ["fact", "doc"] {
            let result = repo.recall("kwamak", mode, Some(source_type));
            assert_eq!(
                result["returned_count"], 0,
                "{mode} {source_type}: {result}"
            );
        }
    }
    repo.ok(&["render"]);
    for path in ["PROJECT.md", "PROJECT_LEDGER.md"] {
        let rendered = fs::read_to_string(repo.path().join(".memhub/rendered").join(path)).unwrap();
        assert!(!rendered.contains("kwamak old value"), "{path}: {rendered}");
    }
    repo.fails_with(
        &["fact", "add", "kwamak", "replacement"],
        "memhub fact unretire",
    );
    fs::write(
        repo.path().join("guide.md"),
        "# Guide\n\nreplacement passage\n",
    )
    .unwrap();
    repo.fails_with(&["doc", "add", "guide.md"], "memhub doc unarchive");
    assert_eq!(repo.json(&["doc", "show", &doc_id, "--json"]), original_doc);

    repo.ok(&["fact", "unretire", &fact_id, "--actor", "test:unretire"]);
    repo.ok(&["doc", "unarchive", &doc_id, "--actor", "test:unarchive"]);
    assert_eq!(repo.json(&["fact", "list", "--json"]), original);
    assert_eq!(repo.json(&["doc", "show", &doc_id, "--json"]), original_doc);
    let restored = repo.recall("kwamak", "fts", Some("doc"));
    assert_eq!(restored["returned_count"], 1);
    for (action, reason) in [
        ("retire", "obsolete"),
        ("unretire", "fact unretire"),
        ("archive", "old guide"),
        ("unarchive", "doc unarchive"),
    ] {
        repo.assert_logged(action, reason);
    }
}

#[test]
fn keyword_retry_ignores_retired_and_archived_full_token_hits() {
    let repo = Repo::new();
    let query = "zorbax veltrix";
    repo.ok(&["fact", "add", "full", "zorbax veltrix full match"]);
    repo.ok(&["fact", "add", "partial", "zorbax partial match"]);
    for (path, body) in [
        ("full.md", "zorbax veltrix full match\n"),
        ("partial.md", "zorbax partial match\n"),
    ] {
        fs::write(repo.path().join(path), body).unwrap();
        repo.ok(&["doc", "add", path]);
    }

    // Each scope has one full-token hit that prevents the partial retry.
    for source_type in ["fact", "doc"] {
        let result = repo.recall(query, "fts", Some(source_type));
        assert_eq!(result["returned_count"], 1, "{source_type}: {result}");
        assert_eq!(result["results"][0]["text"], "zorbax veltrix full match");
    }

    repo.ok(&["fact", "retire", "full", "--reason", "obsolete"]);
    repo.ok(&["doc", "archive", "full.md", "--reason", "obsolete"]);
    // Both full-token hits remain indexed. Filtering them after deciding
    // whether to retry would leave each scope's result empty.
    for (source_type, count_full_hits) in [
        (
            "fact",
            "SELECT count(*) FROM facts_fts WHERE facts_fts MATCH ?1",
        ),
        (
            "doc",
            "SELECT count(*) FROM doc_chunks_fts WHERE doc_chunks_fts MATCH ?1",
        ),
    ] {
        let full_hits: i64 = repo
            .conn()
            .query_row(count_full_hits, [query], |row| row.get(0))
            .unwrap();
        assert_eq!(full_hits, 1, "{source_type}");
        let result = repo.recall(query, "fts", Some(source_type));
        assert_eq!(result["returned_count"], 1, "{source_type}: {result}");
        assert_eq!(result["results"][0]["text"], "zorbax partial match");
    }
}

#[test]
fn memory_lifecycle_task_keep_drop_show_and_portable_export() {
    let repo = Repo::new();
    repo.ok(&["fact", "add", "legacy", "previous value"]);
    repo.ok(&["fact", "retire", "legacy", "--reason", "obsolete"]);
    let task = repo.json(&[
        "task",
        "add",
        "idle task",
        "--notes",
        "still here",
        "--json",
    ]);
    let id = task["id"].to_string();
    repo.conn()
        .execute(
            "UPDATE tasks SET status = 'blocked', updated_at = '2000-01-01 00:00:00'",
            [],
        )
        .unwrap();
    let before = repo.json(&["task", "show", &id, "--json"]);
    repo.fails_with(&["task", "unkeep", &id], "no keep to undo");
    repo.ok(&["task", "keep", &id, "--actor", "test:keep"]);
    let before_drop = repo.json(&["task", "show", &id, "--json"]);
    assert_ne!(before["updated_at"], before_drop["updated_at"]);
    let mut expected = before.clone();
    expected["updated_at"] = before_drop["updated_at"].clone();
    assert_eq!(expected, before_drop);
    repo.ok(&[
        "task",
        "drop",
        &id,
        "--reason",
        "no longer needed",
        "--actor",
        "test:drop",
    ]);
    assert!(
        repo.ok(&["task", "show", &id])
            .contains("drop reason: no longer needed")
    );
    let dropped = repo.json(&["task", "show", &id, "--json"]);
    assert_eq!(dropped["status"], "dropped");
    assert_eq!(dropped["dropped_reason"], "no longer needed");
    assert_eq!(
        repo.json(&["task", "list", "--status", "dropped", "--json"])["tasks"][0],
        dropped
    );
    for status in ["open", "blocked"] {
        assert!(
            repo.json(&["task", "list", "--status", status, "--json"])["tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    repo.fails_with(&["task", "keep", &id], "not open or blocked");
    repo.fails_with(&["task", "unkeep", &id], "changed since it was kept");
    repo.ok(&["render"]);
    let ledger =
        fs::read_to_string(repo.path().join(".memhub/rendered/PROJECT_LEDGER.md")).unwrap();
    assert!(ledger.contains("1 task(s), 0 open"));
    assert!(ledger.contains("**Status:** dropped"));
    repo.assert_logged("keep", "task keep");
    repo.assert_logged("drop", "no longer needed");

    let output = repo.path().join("export.json");
    let out = output.to_str().unwrap();
    repo.ok(&["export", out]);
    let export: Value = serde_json::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
    assert_eq!(export["memhub_export_version"], 2);
    assert_eq!(export["facts"][0]["retired_reason"], "obsolete");
    assert_eq!(export["tasks"][0]["status"], "dropped");
    let target = Repo::new();
    target.ok(&["import", out]);
    assert_eq!(target.json(&["task", "show", &id, "--json"]), dropped);
    assert_eq!(
        target.json(&["fact", "list", "--include-retired", "--json"]),
        repo.json(&["fact", "list", "--include-retired", "--json"])
    );
    target.ok(&["task", "undrop", &id, "--actor", "test:undrop"]);
    assert_eq!(target.json(&["task", "show", &id, "--json"]), before_drop);
    target.assert_logged("undrop", "task undrop");
    target.fails_with(&["task", "undrop", &id], "no recorded drop to undo");
    target.ok(&["task", "unkeep", &id, "--actor", "test:unkeep"]);
    assert_eq!(target.json(&["task", "show", &id, "--json"]), before);
    target.assert_logged("unkeep", "task unkeep");
    target.fails_with(&["task", "unkeep", &id], "no keep to undo");
}

#[test]
fn dropped_task_can_be_done_but_cannot_be_restored() {
    let repo = Repo::new();
    let task = repo.json(&["task", "add", "idle task", "--json"]);
    let id = task["id"].to_string();
    repo.ok(&["task", "keep", &id]);
    repo.ok(&["task", "drop", &id, "--reason", "obsolete"]);
    repo.conn()
        .execute(
            "UPDATE tasks SET updated_at = '2000-01-01 00:00:00' WHERE id = ?1",
            [task["id"].as_i64().unwrap()],
        )
        .unwrap();

    repo.ok(&["task", "done", &id]);
    let done = repo.json(&["task", "show", &id, "--json"]);
    assert_eq!(done["status"], "done");
    assert_ne!(done["updated_at"], "2000-01-01 00:00:00");
    assert!(done["dropped_reason"].is_null());
    let undo_cleared: bool = repo
        .conn()
        .query_row(
            "SELECT pre_drop_status IS NULL AND pre_drop_updated_at IS NULL
                    AND pre_keep_updated_at IS NULL AND kept_at IS NULL
             FROM tasks WHERE id = ?1",
            [task["id"].as_i64().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(undo_cleared);
    repo.fails_with(&["task", "undrop", &id], "no recorded drop to undo");
    repo.fails_with(&["task", "unkeep", &id], "no keep to undo");
}

#[test]
fn task_drop_round_trips_legacy_blank_updated_at() {
    assert_legacy_task_undo(
        "open",
        &["task", "drop", "1", "--reason", "obsolete"],
        "undrop",
    );
}

#[test]
fn task_keep_round_trips_legacy_blank_updated_at() {
    assert_legacy_task_undo("blocked", &["task", "keep", "1"], "unkeep");
}

fn assert_legacy_task_undo(status: &str, action: &[&str], undo: &str) {
    let source = Repo::new();
    let path = source.path().join("legacy.json");
    let legacy = serde_json::json!({
        "memhub_export_version": 1,
        "exported_at": "2025-01-01T00:00:00Z",
        "exported_by": "memhub legacy",
        "source_schema_version": "0025_arch_sections",
        "project": {
            "root_path_at_export": source.path().to_string_lossy(),
            "created_at": "2025-01-01T00:00:00Z",
        },
        "facts": [],
        "decisions": [],
        "tasks": [{
            "id": 1,
            "title": "legacy task",
            "status": status,
            "notes": "keep these",
            "created_at": "2025-01-01 00:00:00",
            "updated_at": "",
        }],
        "commands": [],
        "pending_writes": [],
        "writes_log": [],
    });
    fs::write(&path, legacy.to_string()).unwrap();
    source.ok(&["import", path.to_str().unwrap()]);
    let show = ["task", "show", "1", "--json"];
    let before = source.json(&show);
    assert_eq!(before["updated_at"], "");
    source.ok(action);
    let after = source.json(&show);
    assert_ne!(after["updated_at"], before["updated_at"]);

    let path = source.path().join("export.json");
    source.ok(&["export", path.to_str().unwrap()]);
    let target = Repo::new();
    target.ok(&["import", path.to_str().unwrap()]);
    assert_eq!(target.json(&show), after);
    target.ok(&["task", undo, "1"]);
    assert_eq!(target.json(&show), before);
}

#[test]
fn task_add_with_status_cannot_bypass_drop() {
    use memhub::commands::task;

    let repo = Repo::new();
    let err = task::add_with_status(repo.path(), "bad task", None, "dropped", "test:add")
        .expect_err("use the drop command to save the reason and prior state");
    assert!(err.to_string().contains("memhub task drop"));
    assert!(
        task::list_by_status(repo.path(), "dropped", 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn task_import_refuses_inconsistent_undo_without_replacing_target() {
    let source = Repo::new();
    source.ok(&["fact", "add", "incoming", "new value"]);
    let task = source.json(&["task", "add", "task to restore", "--json"]);
    let id = task["id"].to_string();
    source.ok(&["task", "keep", &id]);
    source.ok(&["task", "drop", &id, "--reason", "obsolete"]);
    let export_path = source.path().join("export.json");
    let path = export_path.to_str().unwrap();
    source.ok(&["export", path]);
    let export: Value = serde_json::from_slice(&fs::read(&export_path).unwrap()).unwrap();

    let target = Repo::new();
    target.ok(&["fact", "add", "original", "old value"]);
    let original = target.json(&["fact", "list", "--json"]);
    for (field, value, action) in [
        ("dropped_reason", Value::Null, "drop"),
        ("dropped_reason", "  ".into(), "drop"),
        ("pre_drop_status", Value::Null, "drop"),
        ("pre_drop_status", "done".into(), "drop"),
        ("pre_drop_updated_at", Value::Null, "drop"),
        ("status", "open".into(), "drop"),
        ("pre_keep_updated_at", Value::Null, "keep"),
        ("kept_at", Value::Null, "keep"),
        ("kept_at", "2000-01-01 00:00:00".into(), "keep"),
    ] {
        let mut invalid = export.clone();
        let row = invalid["tasks"][0].as_object_mut().unwrap();
        if value.is_null() {
            row.remove(field);
        } else {
            row.insert(field.into(), value);
        }
        fs::write(&export_path, invalid.to_string()).unwrap();
        target.fails_with(
            &["import", path, "--force"],
            &format!("task {id} has invalid {action} metadata"),
        );
        assert_eq!(target.json(&["fact", "list", "--json"]), original);
        assert!(!target.path().join(".memhub/backups/import").exists());
        assert!(
            target.json(&["task", "list", "--status", "all", "--json"])["tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn task_unkeep_refuses_after_later_changes_and_undoes_only_the_latest_keep() {
    let repo = Repo::new();
    let task = repo.json(&["task", "add", "idle task", "--json"]);
    let id = task["id"].to_string();
    repo.conn()
        .execute("UPDATE tasks SET updated_at = '2000-01-01 00:00:00'", [])
        .unwrap();
    repo.ok(&["task", "keep", &id]);
    let first_keep = repo.json(&["task", "show", &id, "--json"]);
    repo.ok(&["task", "keep", &id]);
    repo.ok(&["task", "unkeep", &id]);
    assert_eq!(repo.json(&["task", "show", &id, "--json"]), first_keep);
    repo.fails_with(&["task", "unkeep", &id], "no keep to undo");

    repo.ok(&["task", "keep", &id]);
    repo.conn()
        .execute("UPDATE tasks SET updated_at = '2001-01-01 00:00:00'", [])
        .unwrap();
    let changed = repo.json(&["task", "show", &id, "--json"]);
    repo.fails_with(&["task", "unkeep", &id], "changed since it was kept");
    assert_eq!(repo.json(&["task", "show", &id, "--json"]), changed);

    repo.ok(&["task", "keep", &id]);
    repo.ok(&["task", "done", &id]);
    let done = repo.json(&["task", "show", &id, "--json"]);
    repo.fails_with(&["task", "unkeep", &id], "no keep to undo");
    assert_eq!(repo.json(&["task", "show", &id, "--json"]), done);
}
