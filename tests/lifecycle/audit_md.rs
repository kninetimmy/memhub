//! CLI-level tests for `memhub audit md [--json] [--strict]` (Wave 2 C5,
//! issue #32): a clean repo reports no findings and exits 0; an oversize
//! or drifted fixture produces a finding and `--strict` turns that into a
//! nonzero exit while the default stays 0; the `--json` shape is the
//! wrapped `{"audit_md": {...}}` object (Q29 convention, matching
//! `doctor`'s own `{"doctor": {...}}` sibling).
//!
//! Real-binary spawn (not a direct library call) is deliberate here: it
//! is the only way to exercise the actual `--json` text a user/script
//! sees, since `cli::output`'s JSON builders are crate-private (same
//! reasoning `tests/status_health.rs` uses for `status --json`).

use std::path::Path;
use std::process::Command;

use memhub::commands::init;
use memhub::config::ProjectConfig;
use memhub::db;
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

/// A `CLAUDE.md` fixture that satisfies every check at once: starts with
/// the required H1, carries a current-version managed block, holds every
/// N4 keystone phrase, and stays well under the token target. Callers
/// that want exactly one finding to fire start from this and mutate.
fn clean_claude_md() -> String {
    "# memhub\n\n\
     Local-first. Agents are untrusted writers.\n\n\
     <!-- memhub:managed-block v=1 -->\n\
     memhub-primary: true\n\
     db: .memhub/project.sqlite\n\
     rendered: .memhub/rendered/\n\
     config: .memhub/config.toml\n\
     <!-- /memhub:managed-block -->\n\n\
     ## Session Continuity\n\n\
     stale_embeddings gate. sync_adopt gate.\n"
        .to_string()
}

fn write_matched_pair(repo: &Path, claude_md: &str) {
    let agents_md = memhub::agents_md::generate_agents_md(claude_md);
    std::fs::write(repo.join("CLAUDE.md"), claude_md).expect("write CLAUDE.md");
    std::fs::write(repo.join("AGENTS.md"), agents_md).expect("write AGENTS.md");
}

fn findings_json(output: &std::process::Output) -> Value {
    let root: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid json on stdout: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    root["audit_md"].clone()
}

fn has_finding(audit_md: &Value, id: &str) -> bool {
    audit_md["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .any(|f| f["id"] == id)
}

#[test]
fn clean_repo_reports_no_findings_and_exits_zero() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    write_matched_pair(temp.path(), &clean_claude_md());

    let output = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let audit_md = findings_json(&output);
    assert_eq!(audit_md["count"], 0);
    assert_eq!(audit_md["exit_code"], 0);
    assert_eq!(audit_md["findings"].as_array().unwrap().len(), 0);
}

#[test]
fn clean_repo_human_output_says_no_findings() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    write_matched_pair(temp.path(), &clean_claude_md());

    let output = run_cli(temp.path(), &["audit", "md"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No findings."), "stdout: {stdout}");
}

#[test]
fn oversize_claude_md_is_a_finding_and_strict_exits_nonzero() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // Comfortably over the 2,600-token hard ceiling; every other check
    // (managed block, keystones, drift) stays clean so this isolates to
    // exactly one finding.
    let mut claude_md = clean_claude_md();
    claude_md.push_str("\n## Padding\n\n");
    claude_md.push_str(&"word ".repeat(3_000));
    claude_md.push('\n');
    write_matched_pair(temp.path(), &claude_md);

    let plain = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(plain.status.success(), "default run must exit 0");
    let audit_md = findings_json(&plain);
    assert!(has_finding(&audit_md, "claude_md_size"), "{audit_md:#?}");
    assert_eq!(audit_md["exit_code"], 0);

    let strict = run_cli(temp.path(), &["audit", "md", "--strict"]);
    assert_eq!(strict.status.code(), Some(1), "--strict must exit nonzero");
}

#[test]
fn drifted_agents_md_is_a_finding() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let claude_md = clean_claude_md();
    std::fs::write(temp.path().join("CLAUDE.md"), &claude_md).expect("write CLAUDE.md");
    std::fs::write(
        temp.path().join("AGENTS.md"),
        "stale, hand-edited content\n",
    )
    .expect("write stale AGENTS.md");

    let output = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(output.status.success());
    let audit_md = findings_json(&output);
    assert!(has_finding(&audit_md, "agents_md_drift"), "{audit_md:#?}");
}

#[test]
fn missing_managed_block_is_a_finding() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let claude_md = "# memhub\n\n\
        Local-first. Agents are untrusted writers.\n\n\
        ## Session Continuity\n\n\
        stale_embeddings gate. sync_adopt gate.\n"
        .to_string();
    write_matched_pair(temp.path(), &claude_md);

    let output = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(output.status.success());
    let audit_md = findings_json(&output);
    assert!(
        has_finding(&audit_md, "managed_block_missing"),
        "{audit_md:#?}"
    );
}

#[test]
fn missing_keystone_phrase_is_a_finding_with_detail() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // Drop "Local-first" from the otherwise-clean fixture.
    let claude_md = "# memhub\n\n\
        Agents are untrusted writers.\n\n\
        <!-- memhub:managed-block v=1 -->\n\
        memhub-primary: true\n\
        db: .memhub/project.sqlite\n\
        rendered: .memhub/rendered/\n\
        config: .memhub/config.toml\n\
        <!-- /memhub:managed-block -->\n\n\
        ## Session Continuity\n\n\
        stale_embeddings gate. sync_adopt gate.\n"
        .to_string();
    write_matched_pair(temp.path(), &claude_md);

    let output = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(output.status.success());
    let audit_md = findings_json(&output);
    let finding = audit_md["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "keystone_phrases")
        .unwrap_or_else(|| panic!("keystone_phrases finding missing: {audit_md:#?}"));
    assert!(
        finding["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("Local-first")
    );
}

#[test]
fn user_md_path_is_absent_by_default_and_opt_in_when_configured() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    write_matched_pair(temp.path(), &clean_claude_md());

    // Default: clean repo already asserts 0 findings elsewhere, but
    // confirm specifically that no user_md_* finding exists absent config.
    let before = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(before.status.success());
    let audit_md = findings_json(&before);
    assert!(!has_finding(&audit_md, "user_md_size"));
    assert!(!has_finding(&audit_md, "user_md_unreadable"));

    // Opt in: point [audit] user_md_path at an oversized file.
    let user_md_path = temp.path().join("user-global-CLAUDE.md");
    std::fs::write(&user_md_path, "word ".repeat(3_000)).expect("write user md");

    let paths = db::discover_paths(temp.path()).expect("discover paths");
    let mut config = ProjectConfig::load(&paths.config_path).expect("load config");
    config.audit.user_md_path = user_md_path.display().to_string();
    config.save(&paths.config_path).expect("save config");

    let after = run_cli(temp.path(), &["audit", "md", "--json"]);
    assert!(after.status.success());
    let audit_md = findings_json(&after);
    assert!(has_finding(&audit_md, "user_md_size"), "{audit_md:#?}");
}
