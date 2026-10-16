//! Issue #353: commands that only read the machine-global store never
//! migrate it, and recall skips (with a `global_store_skipped` warning) a
//! store whose schema differs from this binary's.
//!
//! Every memhub call here is a spawned child with `HOME` and `USERPROFILE`
//! pointed at a tempdir, so the real `~/.memhub/global.sqlite` is never
//! reached and no in-process env override is needed (see
//! `upgrade/support.rs`: a child gets its own environment snapshot).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};

use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

struct Fixture {
    home: TempDir,
    repo: TempDir,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(args)
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("MEMHUB_LOG", "off")
            .output()
            .expect("spawn memhub")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn ok_json(&self, args: &[&str]) -> Value {
        let stdout = self.ok(args);
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"))
    }

    fn global_db(&self) -> std::path::PathBuf {
        self.home.path().join(".memhub").join("global.sqlite")
    }

    /// The store's recorded schema and its applied-migration list.
    fn schema_state(&self) -> (String, Vec<String>) {
        let conn = Connection::open(self.global_db()).expect("open global");
        let recorded = conn
            .query_row(
                "SELECT schema_version FROM projects WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .expect("recorded schema");
        let mut stmt = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .expect("prepare");
        let applied = stmt
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<Vec<String>, _>>()
            .expect("rows");
        (recorded, applied)
    }

    /// The MCP `recall` tool's structured response, via `memhub serve`.
    fn mcp_recall(&self, query: &str) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_memhub"))
            .arg("serve")
            .current_dir(self.repo.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("MEMHUB_LOG", "off")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn memhub serve");
        let stdin = child.stdin.as_mut().expect("stdin");
        for msg in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "test", "version": "test"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "recall", "arguments": {"query": query, "mode": "fts"}}}),
        ] {
            writeln!(stdin, "{msg}").expect("write json-rpc");
        }
        drop(child.stdin.take());
        let reader = BufReader::new(child.stdout.take().expect("stdout"));
        let call = reader
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .find(|v| v["id"] == json!(2))
            .expect("tools/call response");
        let _ = child.kill();
        let _ = child.wait();
        call["result"]["structuredContent"].clone()
    }
}

/// A repo opted into a global store at this binary's schema, holding one
/// repo fact and one global fact (plus a global doc) that both match
/// "widget".
fn fixture() -> Fixture {
    let f = Fixture {
        home: tempdir().expect("home"),
        repo: tempdir().expect("repo"),
    };
    f.ok(&["init"]);
    f.ok(&["global", "enable"]);
    f.ok(&["fact", "add", "repo widget", "widget lives in the repo"]);
    f.ok(&[
        "fact",
        "add",
        "--global",
        "global widget",
        "widget lives in the global store",
    ]);
    let doc = f.repo.path().join("guide.md");
    std::fs::write(&doc, "# Guide\n\nwidget guide body\n").expect("write doc");
    f.ok(&["doc", "add", "--global", doc.to_str().expect("utf-8 path")]);
    f
}

fn recall_json(f: &Fixture) -> Value {
    f.ok_json(&["recall", "widget", "--mode", "fts", "--no-rerank", "--json"])
}

fn scopes(results: &Value) -> Vec<String> {
    results
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["scope"].as_str().expect("scope").to_string())
        .collect()
}

/// Recall (CLI + MCP) returns repo rows only, with a `global_store_skipped`
/// warning naming `recorded`, the binary's head schema and `memhub upgrade`.
fn assert_recall_skips(f: &Fixture, recorded: &str, head: &str) {
    for (surface, body) in [("cli", recall_json(f)), ("mcp", f.mcp_recall("widget"))] {
        let s = scopes(&body["results"]);
        assert!(
            s.contains(&"repo".to_string()),
            "{surface}: repo rows missing: {body}"
        );
        assert!(
            !s.contains(&"global".to_string()),
            "{surface}: global rows returned: {body}"
        );
        let warn = body["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .find(|w| w["kind"] == "global_store_skipped")
            .unwrap_or_else(|| panic!("{surface}: no global_store_skipped warning: {body}"));
        let reason = warn["reason"].as_str().expect("reason");
        for needle in [recorded, head, "memhub upgrade"] {
            assert!(
                reason.contains(needle),
                "{surface}: warning lacks {needle:?}: {reason}"
            );
        }
    }
}

/// `doc ls|show --global` fail naming both schemas and `memhub upgrade`;
/// `global status` succeeds reporting the recorded schema.
fn assert_doc_reads_refuse_and_status_reports(f: &Fixture, recorded: &str, head: &str) {
    for args in [
        &["doc", "ls", "--global"][..],
        &["doc", "show", "--global", "1"][..],
    ] {
        let out = f.run(args);
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        for needle in [recorded, head, "memhub upgrade"] {
            assert!(
                stderr.contains(needle),
                "{args:?} lacks {needle:?}: {stderr}"
            );
        }
    }
    let status = f.ok_json(&["global", "status", "--json"]);
    assert_eq!(status["schema_version"], json!(recorded), "{status}");
}

#[test]
fn global_reads_leave_an_older_store_untouched() {
    let f = fixture();
    let (head, _) = f.schema_state();
    // At head (criterion 5): recall merges global rows.
    assert!(scopes(&recall_json(&f)["results"]).contains(&"global".to_string()));

    // Make the store one migration behind this binary.
    let older = {
        let conn = Connection::open(f.global_db()).expect("open global");
        conn.execute("DELETE FROM schema_migrations WHERE version = ?1", [&head])
            .expect("drop newest migration row");
        let prev: String = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                r.get(0)
            })
            .expect("previous version");
        conn.execute(
            "UPDATE projects SET schema_version = ?1 WHERE id = 1",
            [&prev],
        )
        .expect("set recorded schema");
        prev
    };
    let before = f.schema_state();
    assert_eq!(before.0, older);

    assert_recall_skips(&f, &older, &head);
    assert_doc_reads_refuse_and_status_reports(&f, &older, &head);
    assert_eq!(f.schema_state(), before, "a read migrated the older store");

    // A global write still migrates it (criterion 6).
    f.ok(&["global", "enable"]);
    let after = f.schema_state();
    assert_eq!(after.0, head);
    assert!(after.1.contains(&head));
}

#[test]
fn global_reads_leave_a_newer_store_untouched() {
    let f = fixture();
    let (head, _) = f.schema_state();
    let newer = "9999_from_a_newer_memhub";
    {
        let conn = Connection::open(f.global_db()).expect("open global");
        conn.execute(
            "INSERT INTO schema_migrations(version) VALUES (?1)",
            [newer],
        )
        .expect("add future migration row");
        conn.execute(
            "UPDATE projects SET schema_version = ?1 WHERE id = 1",
            [newer],
        )
        .expect("set recorded schema");
    }
    let before = f.schema_state();

    assert_recall_skips(&f, newer, &head);
    assert_doc_reads_refuse_and_status_reports(&f, newer, &head);
    assert_eq!(f.schema_state(), before, "a read touched the newer store");

    // A global write still refuses it (criterion 6).
    let out = f.run(&["global", "enable"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("written by a newer memhub"));
    assert_eq!(f.schema_state(), before);
}

/// `projects.schema_version` only ratchets upward, so a store a newer
/// build once touched can record a newer schema while its migration
/// ledger is back at head. Reads judge the ledger, so that store is at
/// head and is still read.
#[test]
fn global_reads_judge_the_ledger_not_the_ratcheted_schema_version() {
    let f = fixture();
    let (head, _) = f.schema_state();
    {
        let conn = Connection::open(f.global_db()).expect("open global");
        conn.execute(
            "UPDATE projects SET schema_version = ?1 WHERE id = 1",
            ["9999_from_a_newer_memhub"],
        )
        .expect("ratchet recorded schema ahead");
    }
    let before = f.schema_state();

    let body = recall_json(&f);
    assert!(
        scopes(&body["results"]).contains(&"global".to_string()),
        "{body}"
    );
    assert!(
        !body["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|w| w["kind"] == "global_store_skipped"),
        "{body}"
    );
    f.ok(&["doc", "ls", "--global"]);
    let status = f.ok_json(&["global", "status", "--json"]);
    assert_eq!(status["schema_version"], json!(head), "{status}");
    assert_eq!(f.schema_state(), before, "a read touched the store");
}

/// A store with no `schema_migrations` table has a ledger that cannot be
/// read: reads skip it or refuse with "could not be read", and
/// `global status` still succeeds, reporting no schema.
#[test]
fn global_reads_skip_a_store_whose_ledger_cannot_be_read() {
    let f = fixture();
    let (head, _) = f.schema_state();
    {
        let conn = Connection::open(f.global_db()).expect("open global");
        conn.execute("DROP TABLE schema_migrations", [])
            .expect("drop ledger");
    }

    assert_recall_skips(&f, "could not be read", &head);
    for args in [
        &["doc", "ls", "--global"][..],
        &["doc", "show", "--global", "1"][..],
    ] {
        let out = f.run(args);
        assert!(!out.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        for needle in ["could not be read", head.as_str(), "memhub upgrade"] {
            assert!(
                stderr.contains(needle),
                "{args:?} lacks {needle:?}: {stderr}"
            );
        }
    }
    let status = f.ok_json(&["global", "status", "--json"]);
    assert_eq!(status["schema_version"], Value::Null, "{status}");
    assert_eq!(status["exists"], json!(true), "{status}");
}
