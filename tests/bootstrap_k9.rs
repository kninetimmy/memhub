use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::commands::init;
use memhub::db;
use rusqlite::params;
use serde_json::Value;
use tempfile::tempdir;

fn memhub_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memhub")
}

fn run_cli(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(memhub_bin())
        .args(args)
        .current_dir(repo)
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub binary")
}

fn write_agent_docs(repo: &Path, decisions: &str, backlog: &str) {
    let dir = repo.join("agent_docs");
    fs::create_dir_all(&dir).expect("create agent_docs");
    fs::write(dir.join("project_state.md"), "# state\n").expect("write state marker");
    if !decisions.is_empty() {
        fs::write(dir.join("project_decisions.md"), decisions).expect("write decisions");
    }
    if !backlog.is_empty() {
        fs::write(dir.join("project_backlog.md"), backlog).expect("write backlog");
    }
}

fn count(repo: &Path, sql: &str) -> i64 {
    let ctx = db::open_project(repo).expect("open project");
    ctx.conn
        .query_row(sql, params![], |row| row.get::<_, i64>(0))
        .expect("count")
}

const SAMPLE_DECISIONS: &str = "\
# Project Decisions

---

## 2026-04-21 - Adopted continuity docs

- Use AGENTS.md + CLAUDE.md as durable framing.

## 2026-04-22 - Pin migrations forward-only

- Migrations are append-only and embedded into the binary.
- Recovery is via export/import.
";

const SAMPLE_BACKLOG: &str = "\
# Project Backlog

## Items

- `M1-001` - Wire CLI scaffolding.
  Status: completed
  Notes: Done in initial commit.

- `M2-001` - Add git ingestion.
  Status: open
  Notes: Use the git CLI; deny-list filters apply.

- `M3-001` - Wait on upstream rmcp change.
  Status: blocked
  Scope: src/mcp/
";

#[test]
fn bootstrap_k9_happy_path_writes_rows() {
    let temp = tempdir().expect("tempdir");
    write_agent_docs(temp.path(), SAMPLE_DECISIONS, SAMPLE_BACKLOG);
    init::run(temp.path()).expect("init");

    let output = run_cli(
        temp.path(),
        &["integrations", "bootstrap-k9", "--json"],
    );
    assert!(
        output.status.success(),
        "bootstrap-k9 failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: Value = serde_json::from_str(stdout.trim()).expect("parse json");
    assert_eq!(payload["dry_run"], false);
    assert_eq!(payload["decisions_imported"], 2);
    assert_eq!(payload["tasks_imported"], 2);
    assert_eq!(payload["tasks_skipped_completed"], 1);
    assert_eq!(payload["actor"], "k9:bootstrap");

    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM decisions"), 2);
    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM tasks"), 2);
    assert_eq!(
        count(
            temp.path(),
            "SELECT COUNT(*) FROM tasks WHERE status = 'blocked'"
        ),
        1
    );
    assert_eq!(
        count(
            temp.path(),
            "SELECT COUNT(*) FROM writes_log WHERE actor = 'k9:bootstrap'"
        ),
        4
    );
}

#[test]
fn bootstrap_k9_dry_run_writes_nothing() {
    let temp = tempdir().expect("tempdir");
    write_agent_docs(temp.path(), SAMPLE_DECISIONS, SAMPLE_BACKLOG);
    init::run(temp.path()).expect("init");

    let output = run_cli(
        temp.path(),
        &["integrations", "bootstrap-k9", "--dry-run", "--json"],
    );
    assert!(output.status.success());

    let payload: Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("json");
    assert_eq!(payload["dry_run"], true);
    assert_eq!(payload["decisions_imported"], 2);
    assert_eq!(payload["tasks_imported"], 2);

    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM decisions"), 0);
    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM tasks"), 0);
}

#[test]
fn bootstrap_k9_refuses_on_non_empty_db() {
    let temp = tempdir().expect("tempdir");
    write_agent_docs(temp.path(), SAMPLE_DECISIONS, SAMPLE_BACKLOG);
    init::run(temp.path()).expect("init");

    let prep = run_cli(
        temp.path(),
        &[
            "decision", "add", "Pre-existing", "--rationale", "already here", "--actor",
            "cli:user",
        ],
    );
    assert!(prep.status.success());

    let output = run_cli(temp.path(), &["integrations", "bootstrap-k9"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only runs on an empty database"),
        "unexpected stderr: {stderr}"
    );

    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM decisions"), 1);
    assert_eq!(count(temp.path(), "SELECT COUNT(*) FROM tasks"), 0);
}

#[test]
fn bootstrap_k9_refuses_when_k9_disabled() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["integrations", "bootstrap-k9"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("K9 integration is not configured")
            || stderr.contains("K9 integration is disabled"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn bootstrap_k9_refuses_when_no_source_files() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path().join("agent_docs");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("project_state.md"), "# state\n").unwrap();
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["integrations", "bootstrap-k9"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no K9 source files found"),
        "unexpected stderr: {stderr}"
    );
}
