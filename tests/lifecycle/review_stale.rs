//! CLI-level tests for `memhub review stale` (Wave 3 L4, issue #47): the
//! read-only lifecycle audit queue and `status`'s new one-line count.
//! Per-category logic (horizon config-vs-constant, superseded exclusion,
//! read-only invariant) is unit-tested in `src/commands/review.rs`; this
//! file exercises the compiled binary's `--json` wrapped-object shape
//! (decision Q29: `{"review_stale": {...}}`) and human output, plus
//! `status`'s superset-compatible `stale_queue` addition.

use std::path::Path;
use std::process::Command;

use memhub::commands::{doc, fact, init, pending_write, task};
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

/// Seeds exactly one flagged row per category: an over-horizon fact, a
/// long-done task, an expired pending write, and a doc edited after
/// ingest — mirroring `review.rs`'s own `seed_one_of_each_category`
/// fixture, but through the library surface a CLI test reaches for.
fn seed_one_of_each_category(repo: &Path) {
    let (fact_id, _) = fact::add(repo, "k", "v", "user", "cli:user").expect("fact");
    let task_id = task::add(repo, "t", None, "cli:user").expect("task");
    task::done(repo, task_id, "cli:user").expect("done");
    let pending_id = pending_write::propose_fact(repo, "k2", "v2", "r", "codex", "codex", "{}")
        .expect("propose");
    let doc_path = repo.join("d.md");
    std::fs::write(&doc_path, "# D\n\nbody\n").expect("write doc");
    doc::add(repo, &doc_path, None, "cli:user").expect("ingest doc");

    let ctx = db::open_project(repo).expect("open");
    ctx.conn
        .execute(
            "UPDATE facts SET verified_at = datetime('now', '-400 days') WHERE id = ?1",
            params![fact_id],
        )
        .expect("backdate fact");
    ctx.conn
        .execute(
            "UPDATE tasks SET updated_at = datetime('now', '-40 days') WHERE id = ?1",
            params![task_id],
        )
        .expect("backdate task");
    ctx.conn
        .execute(
            "UPDATE pending_writes SET status = 'expired', reviewed_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![pending_id],
        )
        .expect("expire pending write");
    drop(ctx);
    std::fs::write(&doc_path, "# D\n\nedited after ingest\n").expect("edit doc after ingest");
}

#[test]
fn review_stale_json_reports_empty_queue_on_a_clean_repo() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["review", "stale", "--json"]);
    assert!(
        output.status.success(),
        "review stale --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root: Value = serde_json::from_slice(&output.stdout).expect("valid json on stdout");
    let report = &root["review_stale"];
    assert_eq!(report["counts"]["total"], 0);
    assert_eq!(report["counts"]["fact_near_horizon"], 0);
    assert_eq!(report["counts"]["done_task_aged"], 0);
    assert_eq!(report["counts"]["pending_expired"], 0);
    assert_eq!(report["counts"]["doc_hash_drift"], 0);
    assert_eq!(report["items"].as_array().expect("items array").len(), 0);
}

#[test]
fn review_stale_human_reports_empty_queue_message_on_a_clean_repo() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["review", "stale"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Stale queue is empty"), "stdout: {stdout}");
}

#[test]
fn review_stale_json_surfaces_all_four_categories_each_with_a_verb() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    seed_one_of_each_category(temp.path());

    let output = run_cli(temp.path(), &["review", "stale", "--json"]);
    assert!(
        output.status.success(),
        "review stale --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root: Value = serde_json::from_slice(&output.stdout).expect("valid json on stdout");
    let report = &root["review_stale"];

    assert_eq!(report["counts"]["total"], 4);
    assert_eq!(report["counts"]["fact_near_horizon"], 1);
    assert_eq!(report["counts"]["done_task_aged"], 1);
    assert_eq!(report["counts"]["pending_expired"], 1);
    assert_eq!(report["counts"]["doc_hash_drift"], 1);

    let items = report["items"].as_array().expect("items array");
    assert_eq!(items.len(), 4, "{items:#?}");

    let categories: Vec<&str> = items
        .iter()
        .map(|i| i["category"].as_str().expect("category is a string"))
        .collect();
    for expected in [
        "fact_near_horizon",
        "done_task_aged",
        "pending_expired",
        "doc_hash_drift",
    ] {
        assert!(
            categories.contains(&expected),
            "missing category {expected:?} in {categories:?}"
        );
    }

    for item in items {
        let verb = item["verb"].as_str().expect("verb is a string");
        assert!(!verb.trim().is_empty(), "{item:#?}");
        assert!(verb.starts_with("memhub "), "{item:#?}");
        assert!(item["id"].as_i64().is_some(), "{item:#?}");
        assert!(
            !item["message"].as_str().unwrap_or_default().is_empty(),
            "{item:#?}"
        );
    }
}

#[test]
fn review_stale_human_shows_category_headings_and_fix_lines() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    seed_one_of_each_category(temp.path());

    let output = run_cli(temp.path(), &["review", "stale"]);
    assert!(
        output.status.success(),
        "review stale failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Facts near staleness horizon"), "{stdout}");
    assert!(stdout.contains("Done tasks aged out"), "{stdout}");
    assert!(stdout.contains("Expired pending writes"), "{stdout}");
    assert!(
        stdout.contains("Docs drifted from on-disk file"),
        "{stdout}"
    );
    assert!(stdout.contains("fix: memhub fact verify k"), "{stdout}");
    assert!(
        stdout.contains("fix: memhub task list --status done"),
        "{stdout}"
    );
    assert!(stdout.contains("fix: memhub review show"), "{stdout}");
    assert!(stdout.contains("fix: memhub doc add"), "{stdout}");
    assert!(stdout.contains("Summary: 4 item(s)"), "{stdout}");
}

#[test]
fn status_json_includes_stale_queue_alongside_existing_keys() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    seed_one_of_each_category(temp.path());

    let output = run_cli(temp.path(), &["status", "--json"]);
    assert!(
        output.status.success(),
        "status --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root: Value = serde_json::from_slice(&output.stdout).expect("valid json on stdout");
    let status = &root["status"];

    assert_eq!(status["stale_queue"], 4);
    // Superset-compatible: pre-existing keys must still be present.
    assert!(status.get("facts").is_some());
    assert!(status.get("pending_writes").is_some());
}

#[test]
fn status_human_shows_stale_queue_line() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["status"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Stale queue: 0"), "stdout: {stdout}");
}
