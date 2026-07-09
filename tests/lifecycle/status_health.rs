//! CLI-level tests for the `status` subsystem-state refresh (Wave 1·C,
//! issue #22): the human view's new "Subsystems:" block, K9 lines
//! appearing only when K9 is actually detected, and `status --json`
//! staying superset-compatible with F1's existing keys while adding a
//! new `checks` array reused from `doctor`'s own check functions.

use std::path::Path;
use std::process::Command;

use memhub::commands::{init, integrations};
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
        "k9_detected",
        "k9_enabled",
        "k9_agent_docs_path",
        "k9_drift",
    ] {
        assert!(
            status.get(key).is_some(),
            "expected pre-existing key {key:?} to survive, got {status}"
        );
    }
    assert_eq!(status["k9_detected"], false);

    // New: a `checks` array reusing doctor's per-check JSON shape
    // (id/group/status/message) for a curated subset of subsystems.
    let checks = status["checks"].as_array().expect("checks is an array");
    assert_eq!(find_check(checks, "schema")["status"], "ok");
    assert_eq!(find_check(checks, "render_freshness")["status"], "warn");
    assert_eq!(find_check(checks, "retrieval_mode")["status"], "ok");
    // Not detected on a clean repo -> still present in JSON (complete
    // data); the human view is what hides skipped checks (see below).
    assert_eq!(find_check(checks, "k9_coexistence")["status"], "skipped");
    // Heavy doctor-only checks (integrity, config, MCP registration)
    // must NOT leak into status's fast path.
    assert!(
        !checks.iter().any(|c| c["id"] == "integrity_check"
            || c["id"] == "config_parse"
            || c["id"].as_str().unwrap_or("").starts_with("mcp_registration")),
        "status leaked a doctor-only check: {checks:#?}"
    );
}

#[test]
fn status_human_shows_subsystems_and_hides_k9_when_not_detected() {
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

    // The bug this issue fixes: a clean, non-K9 repo must not spray
    // any K9 text into the human view (today's `k9_detected:false`
    // used to print "K9 detected: no" / "K9 integration: disabled"
    // unconditionally).
    assert!(!stdout.contains("K9"), "unexpected K9 output: {stdout}");
}

#[test]
fn status_human_shows_k9_line_once_detected() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let agent_docs = temp.path().join("agent_docs");
    std::fs::create_dir_all(&agent_docs).expect("create agent_docs");
    std::fs::write(agent_docs.join("project_state.md"), "# state").expect("write marker");
    integrations::enable_k9(temp.path(), None, false).expect("enable k9");

    let output = run_cli(temp.path(), &["status"]);
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("k9_coexistence: K9 integrated"),
        "stdout: {stdout}"
    );
}
