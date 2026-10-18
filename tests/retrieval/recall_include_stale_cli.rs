//! `memhub recall --include-stale` (issue #394) is accepted and ignored: on
//! a stale fact the binary returns the same flagged results with the flag
//! as without it. Driven as a child process with HOME at a temp dir.

use std::path::Path;
use std::process::{Command, Output};

use memhub::commands::{fact, init};
use serde_json::Value;
use tempfile::tempdir;

fn memhub(root: &Path, home: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .output()
        .expect("spawn memhub");
    assert!(
        out.status.success(),
        "memhub {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn results(root: &Path, home: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["recall", "cliStaleParamProbe", "--mode", "fts", "--json"];
    args.extend_from_slice(extra);
    let out = memhub(root, home, &args);
    let json: Value = serde_json::from_slice(&out.stdout).expect("recall json");
    json["results"].clone()
}

#[test]
fn recall_include_stale_flag_returns_the_same_results_as_omitting_it() {
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    init::run(repo.path()).expect("init");
    let (fact_id, _) = fact::add(
        repo.path(),
        "stale-cli",
        "cliStaleParamProbe value",
        "user",
        "cli:user",
    )
    .expect("fact");
    let conn = rusqlite::Connection::open(repo.path().join(".memhub").join("project.sqlite"))
        .expect("open db");
    conn.execute(
        "UPDATE facts SET verified_at = datetime('now', '-100000 days') WHERE id = ?1",
        [fact_id],
    )
    .expect("age fact");
    drop(conn);

    let omitted = results(repo.path(), home.path(), &[]);
    assert_eq!(omitted.as_array().expect("array").len(), 1, "{omitted}");
    assert_eq!(omitted[0]["stale"], true, "{omitted}");
    assert_eq!(
        results(repo.path(), home.path(), &["--include-stale"]),
        omitted
    );
}
