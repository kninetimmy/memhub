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
