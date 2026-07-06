use memhub::commands::{command, fact, init, pending_write, review, status};
use memhub::db;
use memhub::models::FACT_STALE_AFTER_DAYS;
use rusqlite::params;
use tempfile::tempdir;

fn backdate_fact(path: &std::path::Path, key: &str, days_ago: i64) {
    let ctx = db::open_project(path).expect("open project");
    let affected = ctx
        .conn
        .execute(
            "UPDATE facts
             SET verified_at = datetime('now', ?1)
             WHERE key = ?2",
            params![format!("-{days_ago} days"), key],
        )
        .expect("backdate fact");
    assert_eq!(affected, 1, "expected fact {key} to exist");
}

fn null_verified_at(path: &std::path::Path, key: &str) {
    let ctx = db::open_project(path).expect("open project");
    let affected = ctx
        .conn
        .execute(
            "UPDATE facts SET verified_at = NULL WHERE key = ?1",
            params![key],
        )
        .expect("null verified_at");
    assert_eq!(affected, 1, "expected fact {key} to exist");
}

#[test]
fn fact_threshold_is_ninety_days() {
    assert_eq!(FACT_STALE_AFTER_DAYS, 90);
}

#[test]
fn fresh_fact_is_not_stale() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("add fact");

    let facts = fact::list(temp.path()).expect("list facts");
    assert_eq!(facts.len(), 1);
    assert!(!facts[0].is_stale);
}

#[test]
fn fact_just_under_threshold_is_not_stale() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("add fact");
    backdate_fact(temp.path(), "build-command", 89);

    let facts = fact::list(temp.path()).expect("list facts");
    assert!(!facts[0].is_stale);
}

#[test]
fn fact_over_threshold_is_stale() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("add fact");
    backdate_fact(temp.path(), "build-command", 91);

    let facts = fact::list(temp.path()).expect("list facts");
    assert!(facts[0].is_stale);
}

#[test]
fn fact_with_null_verified_at_is_stale() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("add fact");
    null_verified_at(temp.path(), "build-command");

    let facts = fact::list(temp.path()).expect("list facts");
    assert!(facts[0].is_stale);
}

#[test]
fn fact_add_upsert_clears_stale() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("add fact");
    backdate_fact(temp.path(), "build-command", 200);
    assert!(fact::list(temp.path()).expect("list").remove(0).is_stale);

    // Same key, new value — verified_at should be refreshed by the upsert.
    fact::add(
        temp.path(),
        "build-command",
        "cargo build --release",
        "user",
        "cli:user",
    )
    .expect("upsert fact");

    let facts = fact::list(temp.path()).expect("list facts");
    assert!(!facts[0].is_stale);
    assert_eq!(facts[0].value, "cargo build --release");
}

#[test]
fn review_accept_produces_fresh_fact() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = pending_write::propose_fact(
        temp.path(),
        "lint-command",
        "cargo fmt --check",
        "Observed in repo.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    review::accept(temp.path(), pending_id, "cli:user").expect("accept");

    let facts = fact::list(temp.path()).expect("list facts");
    let accepted = facts
        .iter()
        .find(|f| f.key == "lint-command")
        .expect("accepted fact");
    assert!(!accepted.is_stale);
    assert_eq!(accepted.source, "user+agent:codex");
}

#[test]
fn count_stale_reports_only_stale_facts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(temp.path(), "fresh", "value", "user", "cli:user").expect("fresh fact");
    fact::add(temp.path(), "old", "value", "user", "cli:user").expect("old fact");
    fact::add(temp.path(), "older", "value", "user", "cli:user").expect("older fact");
    backdate_fact(temp.path(), "old", 100);
    backdate_fact(temp.path(), "older", 365);

    assert_eq!(fact::count_stale(temp.path()).expect("count stale"), 2);

    let summary = status::run(temp.path()).expect("status");
    assert_eq!(summary.facts, 3);
    assert_eq!(summary.stale_facts, 2);
}

#[test]
fn command_confidence_is_none_with_no_runs() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let ctx = db::open_project(temp.path()).expect("open project");
    ctx.conn
        .execute(
            "INSERT INTO commands(project_id, kind, cmdline, success_count, fail_count)
             VALUES (1, 'build', 'cargo build', 0, 0)",
            [],
        )
        .expect("insert command");
    drop(ctx);

    let listed = command::list(temp.path()).expect("list");
    assert_eq!(listed[0].confidence(), None);
}

#[test]
fn command_confidence_reflects_success_and_failure_counts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    command::verify(temp.path(), "build", "cargo build", 0, "cli:user").expect("ok run");
    command::verify(temp.path(), "build", "cargo build", 0, "cli:user").expect("ok run");
    command::verify(temp.path(), "build", "cargo build", 0, "cli:user").expect("ok run");
    command::verify(temp.path(), "build", "cargo build", 1, "cli:user").expect("fail run");

    let record = command::latest_by_kind(temp.path(), "build")
        .expect("latest")
        .expect("row exists");
    let confidence = record.confidence().expect("confidence");
    assert!((confidence - 0.75).abs() < 1e-9);
}

#[test]
fn command_confidence_all_failures_is_zero() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    command::verify(temp.path(), "build", "cargo build", 1, "cli:user").expect("fail run");
    command::verify(temp.path(), "build", "cargo build", 2, "cli:user").expect("fail run");

    let record = command::latest_by_kind(temp.path(), "build")
        .expect("latest")
        .expect("row exists");
    assert_eq!(record.confidence(), Some(0.0));
}
