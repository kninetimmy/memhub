//! CLI-level tests for `memhub wrapup-policy [--json]` (Wave 6 W1+W2,
//! issue #95): the resolved level and full policy text render correctly
//! for each `[wrap_up] verbosity`, the `--json` shape is the wrapped
//! `{"wrapup_policy": {...}}` object (Q29 convention, matching
//! `doctor`/`audit_md`'s own siblings), and the command never writes to
//! the DB.
//!
//! Real-binary spawn (not a direct library call) is deliberate here: it
//! is the only way to exercise the actual `--json` text a user/script
//! sees, since `cli::output`'s JSON builders are crate-private (same
//! reasoning `tests/audit_md.rs` uses for `audit md --json`).

use std::path::Path;
use std::process::Command;

use memhub::commands::init;
use memhub::config::{ProjectConfig, WrapUpVerbosity};
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

fn verbosity_from_label(level: &str) -> WrapUpVerbosity {
    match level {
        "minimal" => WrapUpVerbosity::Minimal,
        "standard" => WrapUpVerbosity::Standard,
        "full" => WrapUpVerbosity::Full,
        "transcript" => WrapUpVerbosity::Transcript,
        other => panic!("unknown wrap_up verbosity label {other:?}"),
    }
}

fn set_verbosity(repo: &Path, level: &str) {
    let paths = db::discover_paths(repo).expect("discover paths");
    let mut config = ProjectConfig::load(&paths.config_path).expect("load config");
    config.wrap_up.verbosity = verbosity_from_label(level);
    config.save(&paths.config_path).expect("save config");
}

fn wrapup_policy_json(output: &std::process::Output) -> Value {
    let root: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid json on stdout: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    root["wrapup_policy"].clone()
}

#[test]
fn default_repo_resolves_standard_and_exits_zero() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["wrapup-policy", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wrapup_policy = wrapup_policy_json(&output);
    assert_eq!(wrapup_policy["verbosity"], "standard");
    let instructions = wrapup_policy["instructions"]
        .as_str()
        .expect("instructions is a string");
    assert!(instructions.contains("level: standard"));
    assert!(instructions.contains("Synthesize eight things"));
}

#[test]
fn human_output_prints_the_resolved_level_and_instructions() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["wrapup-policy"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Level: standard"), "stdout: {stdout}");
    assert!(stdout.contains("## Detection"), "stdout: {stdout}");
}

#[test]
fn each_configured_level_is_reflected_in_json_output() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    for level in ["minimal", "standard", "full", "transcript"] {
        set_verbosity(temp.path(), level);

        let output = run_cli(temp.path(), &["wrapup-policy", "--json"]);
        assert!(output.status.success(), "level {level}");
        let wrapup_policy = wrapup_policy_json(&output);
        assert_eq!(wrapup_policy["verbosity"], level, "level {level}");
        let instructions = wrapup_policy["instructions"]
            .as_str()
            .unwrap_or_else(|| panic!("instructions missing for level {level}"));
        assert!(
            instructions.contains(&format!("level: {level}")),
            "level {level}: {instructions}"
        );
    }
}

#[test]
fn transcript_level_names_the_archive_step_and_issue_96() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    set_verbosity(temp.path(), "transcript");

    let output = run_cli(temp.path(), &["wrapup-policy", "--json"]);
    assert!(output.status.success());
    let wrapup_policy = wrapup_policy_json(&output);
    let instructions = wrapup_policy["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("Transcript archive"));
    assert!(instructions.contains("issue #96"));
}

#[test]
fn command_never_writes_to_the_database() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let before = memhub::commands::status::run(temp.path()).expect("status before");

    let output = run_cli(temp.path(), &["wrapup-policy", "--json"]);
    assert!(output.status.success());

    let after = memhub::commands::status::run(temp.path()).expect("status after");
    assert_eq!(before.facts, after.facts);
    assert_eq!(before.decisions, after.decisions);
    assert_eq!(before.tasks_total, after.tasks_total);
    assert_eq!(before.writes_logged, after.writes_logged);
}

#[test]
fn doctor_does_not_warn_on_the_wrap_up_section() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    memhub::commands::render::run(temp.path(), "cli:user").expect("render");
    set_verbosity(temp.path(), "transcript");

    let output = run_cli(temp.path(), &["doctor", "--json"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root: Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let checks = root["doctor"]["checks"].as_array().expect("checks array");
    for check in checks {
        let detail = check["detail"].as_str().unwrap_or("");
        assert!(
            !detail.contains("wrap_up"),
            "doctor flagged wrap_up: {check:#?}"
        );
    }
}
