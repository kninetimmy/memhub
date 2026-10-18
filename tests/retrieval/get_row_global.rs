//! `memhub get-row --scope global` success path (issue #394): in a repo
//! opted into machine-global memory it returns a global fact and a global
//! decision with scope `global` and their full stored text, and refuses a
//! missing global id. A child process with HOME at a temp dir keeps the
//! real machine-global store untouched and avoids mutating this test
//! process's environment.

use std::path::Path;
use std::process::{Command, Output};

use memhub::commands::init;
use serde_json::Value;
use tempfile::tempdir;

fn memhub(root: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .output()
        .expect("spawn memhub")
}

fn ok(root: &Path, home: &Path, args: &[&str]) -> Output {
    let out = memhub(root, home, args);
    assert!(
        out.status.success(),
        "memhub {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn row_json(root: &Path, home: &Path, source_type: &str, id: &str) -> Value {
    let out = ok(
        root,
        home,
        &["get-row", source_type, id, "--scope", "global", "--json"],
    );
    serde_json::from_slice(&out.stdout).expect("get-row json")
}

#[test]
fn get_row_returns_global_fact_and_decision_and_refuses_a_missing_id() {
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    init::run(repo.path()).expect("init");
    ok(repo.path(), home.path(), &["global", "enable"]);

    // Well over the 300-character recall excerpt, to show the full text.
    let fact_value = format!("globalfactprobe {}", "full stored fact text ".repeat(30));
    let decision_rationale = format!(
        "globaldecisionprobe {}",
        "full stored rationale ".repeat(30)
    );
    ok(
        repo.path(),
        home.path(),
        &["fact", "add", "global-probe-key", &fact_value, "--global"],
    );
    ok(
        repo.path(),
        home.path(),
        &[
            "decision",
            "add",
            "Global probe decision",
            "--rationale",
            &decision_rationale,
            "--global",
        ],
    );

    let fact = row_json(repo.path(), home.path(), "fact", "1");
    assert_eq!(fact["scope"], "global", "{fact}");
    assert_eq!(fact["source_type"], "fact", "{fact}");
    assert_eq!(fact["title"], "global-probe-key", "{fact}");
    assert!(
        fact["body"].as_str().expect("body").contains(&fact_value),
        "{fact}"
    );

    let decision = row_json(repo.path(), home.path(), "decision", "1");
    assert_eq!(decision["scope"], "global", "{decision}");
    assert_eq!(decision["source_type"], "decision", "{decision}");
    assert_eq!(decision["title"], "Global probe decision", "{decision}");
    assert!(
        decision["body"]
            .as_str()
            .expect("body")
            .contains(&decision_rationale),
        "{decision}"
    );

    let missing = memhub(
        repo.path(),
        home.path(),
        &["get-row", "fact", "999", "--scope", "global"],
    );
    assert!(!missing.status.success(), "a missing global id must fail");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("no fact with id 999 in the global store"),
        "{stderr}"
    );
}
