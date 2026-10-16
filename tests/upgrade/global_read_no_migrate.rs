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

// ---------------------------------------------------------------------------
// Issue #357: eval, recall text and upgrade report the global store honestly.
// ---------------------------------------------------------------------------

/// Put the global store's ledger one migration behind head (recorded schema
/// moved back with it); returns the ledger's schema.
fn make_ledger_older(f: &Fixture, head: &str) -> String {
    let conn = Connection::open(f.global_db()).expect("open global");
    conn.execute("DELETE FROM schema_migrations WHERE version = ?1", [head])
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
}

fn ratchet_recorded_schema(f: &Fixture) {
    Connection::open(f.global_db())
        .expect("open global")
        .execute(
            "UPDATE projects SET schema_version = ?1 WHERE id = 1",
            ["9999_from_a_newer_memhub"],
        )
        .expect("ratchet recorded schema ahead");
}

fn write_golden(f: &Fixture) -> String {
    let path = f.repo.path().join("golden.json");
    let golden = json!({"version": 1, "queries": [{
        "id": "widget", "query": "widget", "kind": "match",
        "body_contains": ["widget"],
    }]});
    std::fs::write(&path, golden.to_string()).expect("write golden");
    path.to_str().expect("utf-8 path").to_string()
}

/// The `<global store>` row of `upgrade --finish --json`. `--finish` is the
/// migrate-and-verify phase alone: nothing is built or installed.
fn finish_global_row(f: &Fixture) -> Value {
    let work = tempdir().expect("work dir");
    let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(["upgrade", "--finish", "--json", "--no-gc", "--no-hooks"])
        .current_dir(work.path())
        .env("HOME", f.home.path())
        .env("USERPROFILE", f.home.path())
        .env("CARGO_HOME", f.home.path().join(".cargo"))
        .env_remove("CARGO_INSTALL_ROOT")
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn upgrade --finish");
    assert!(
        out.status.success(),
        "upgrade --finish failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let body: Value = serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    body["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .find(|r| r["label"] == "<global store>")
        .unwrap_or_else(|| panic!("no global row: {body}"))
        .clone()
}

/// `upgrade --dry-run --json`'s `global_store` preview (the source repo is
/// the cwd, as upgrade requires).
fn dry_run_global_preview(f: &Fixture) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(["upgrade", "--dry-run", "--json", "--no-gc", "--no-hooks"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("HOME", f.home.path())
        .env("USERPROFILE", f.home.path())
        .env("CARGO_HOME", f.home.path().join(".cargo"))
        .env_remove("CARGO_INSTALL_ROOT")
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn upgrade --dry-run");
    assert!(
        out.status.success(),
        "upgrade --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let body: Value = serde_json::from_str(stdout.lines().next().expect("json line"))
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    body["global_store"].clone()
}

#[test]
fn eval_retrieval_says_when_the_global_store_was_skipped() {
    let f = fixture();
    let golden = write_golden(&f);
    let eval = |json: bool| {
        let mut a = vec!["eval", "retrieval", "--golden", &golden, "--mode", "fts"];
        a.push("--no-rerank");
        if json {
            a.push("--json");
        }
        f.ok(&a)
    };

    // Not skipped: no notice in either output.
    assert!(!eval(false).contains("skipped"));
    let body: Value = serde_json::from_str(eval(true).trim()).expect("eval json");
    assert!(body.get("global_store_skipped").is_none(), "{body}");

    // Skipped: both outputs say so and name `memhub upgrade`.
    let (head, _) = f.schema_state();
    make_ledger_older(&f, &head);
    let text = eval(false);
    let line = text
        .lines()
        .find(|l| l.contains("Global store skipped"))
        .unwrap_or_else(|| panic!("no skipped notice: {text}"));
    assert!(line.contains("memhub upgrade"), "{line}");
    let body: Value = serde_json::from_str(eval(true).trim()).expect("eval json");
    let reason = body["global_store_skipped"].as_str().expect("notice");
    assert!(reason.contains("memhub upgrade"), "{reason}");
}

#[test]
fn recall_text_warning_has_no_count_and_names_the_fix_once() {
    let f = fixture();
    let (head, _) = f.schema_state();
    make_ledger_older(&f, &head);
    let text = f.ok(&["recall", "widget", "--mode", "fts", "--no-rerank"]);
    let line = text
        .lines()
        .find(|l| l.contains("global_store_skipped"))
        .unwrap_or_else(|| panic!("no warning line: {text}"));
    assert!(!line.contains("(0/0)"), "{line}");
    assert_eq!(line.matches("memhub upgrade").count(), 1, "{line}");

    // The structured warning keeps every field.
    let body = recall_json(&f);
    let warn = body["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .find(|w| w["kind"] == "global_store_skipped")
        .expect("warning");
    for key in ["kind", "stale_count", "total_count", "reason", "fix"] {
        assert!(warn.get(key).is_some(), "{key} missing: {warn}");
    }
}

#[test]
fn upgrade_judges_the_global_store_by_its_ledger() {
    // Ledger at head, recorded schema ratcheted ahead: ready both ways.
    let f = fixture();
    let (head, _) = f.schema_state();
    ratchet_recorded_schema(&f);
    let preview = dry_run_global_preview(&f);
    assert_eq!(preview["verdict"], json!("ready"), "{preview}");
    assert_eq!(preview["schema"], json!(head), "{preview}");
    let row = finish_global_row(&f);
    assert_eq!(row["status"], json!("ready"), "{row}");

    // Ledger one migration behind: would migrate, then migrated.
    let f = fixture();
    let (head, _) = f.schema_state();
    let older = make_ledger_older(&f, &head);
    let preview = dry_run_global_preview(&f);
    assert_eq!(preview["verdict"], json!("would migrate"), "{preview}");
    assert_eq!(preview["schema"], json!(older), "{preview}");
    let row = finish_global_row(&f);
    assert_eq!(row["status"], json!("migrated"), "{row}");
    assert_eq!(row["before"], json!(older), "{row}");
    assert_eq!(row["after"], json!(head), "{row}");
}
