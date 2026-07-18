//! CLI-level tests for the `status` subsystem-state refresh (Wave 1·C,
//! issue #22): the human view's new "Subsystems:" block, and
//! `status --json` staying superset-compatible with F1's existing keys
//! while adding a new `checks` array reused from `doctor`'s own check
//! functions.

use std::path::Path;
use std::process::Command;

use memhub::commands::init;
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

fn find_check<'a>(checks: &'a [Value], id: &str) -> &'a Value {
    checks
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("check {id:?} missing from checks array: {checks:#?}"))
}

#[test]
fn status_json_adds_checks_and_keeps_existing_keys() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root: Value = serde_json::from_slice(&output.stdout).expect("valid json on stdout");
    let status = &root["status"];

    // F1's wrapped-object keys must all still be present — a
    // superset-compatible change, not a replacement (issue #22
    // constraint; existing `{"status":{...}}` consumers must not
    // break).
    for key in [
        "project_name",
        "repo_root",
        "db_path",
        "config_path",
        "schema_version",
        "facts",
        "stale_facts",
        "decisions",
        "tasks_open",
        "tasks_total",
        "commands",
        "commits",
        "files",
        "chunks",
        "pending_writes",
        "writes_logged",
        "deny_patterns",
    ] {
        assert!(
            status.get(key).is_some(),
            "expected pre-existing key {key:?} to survive, got {status}"
        );
    }

    // New: a `checks` array reusing doctor's per-check JSON shape
    // (id/group/status/message) for a curated subset of subsystems.
    let checks = status["checks"].as_array().expect("checks is an array");
    assert_eq!(find_check(checks, "schema")["status"], "ok");
    assert_eq!(find_check(checks, "render_freshness")["status"], "warn");
    assert_eq!(find_check(checks, "retrieval_mode")["status"], "ok");
    // Heavy doctor-only checks (integrity, config, MCP registration)
    // must NOT leak into status's fast path.
    assert!(
        !checks.iter().any(|c| c["id"] == "integrity_check"
            || c["id"] == "config_parse"
            || c["id"]
                .as_str()
                .unwrap_or("")
                .starts_with("mcp_registration")),
        "status leaked a doctor-only check: {checks:#?}"
    );
}

#[test]
fn status_human_shows_subsystems_block() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["status"]);
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Subsystems:"), "stdout: {stdout}");
    assert!(stdout.contains("schema:"), "stdout: {stdout}");
    assert!(stdout.contains("render_freshness:"), "stdout: {stdout}");
    assert!(stdout.contains("retrieval_mode:"), "stdout: {stdout}");
}
