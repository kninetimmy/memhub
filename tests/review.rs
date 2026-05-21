use memhub::MemhubError;
use memhub::commands::{decision, fact, init, pending_write, review, search, status};
use rusqlite::params;
use tempfile::tempdir;

fn stage_fact(path: &std::path::Path, key: &str, value: &str, rationale: &str) -> i64 {
    pending_write::propose_fact(
        path,
        key,
        value,
        rationale,
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact")
}

fn stage_fact_from_agent(
    path: &std::path::Path,
    key: &str,
    value: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
) -> i64 {
    pending_write::propose_fact(
        path,
        key,
        value,
        rationale,
        actor,
        actor_raw,
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact")
}

fn stage_decision(path: &std::path::Path, title: &str, rationale: &str) -> i64 {
    pending_write::propose_decision(
        path,
        title,
        rationale,
        "claude-code",
        "claude-ai",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose decision")
}

#[test]
fn review_list_defaults_to_pending_and_filters_by_status() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    stage_fact(
        temp.path(),
        "build-command",
        "cargo build",
        "Observed in repo.",
    );
    let to_reject = stage_fact(temp.path(), "lint-command", "cargo fmt", "Maybe wrong.");
    review::reject(temp.path(), to_reject, Some("Wrong source"), "cli:user").expect("reject");

    let pending = review::list(temp.path(), Some("pending"), 25).expect("list pending");
    let rejected = review::list(temp.path(), Some("rejected"), 25).expect("list rejected");
    let all = review::list(temp.path(), None, 25).expect("list all");

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, "fact");
    assert!(pending[0].payload_json.contains("build-command"));
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].id, to_reject);
    assert_eq!(all.len(), 2);
}

#[test]
fn review_show_returns_full_record() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let id = stage_decision(
        temp.path(),
        "Use rusqlite bundled mode",
        "Avoid system SQLite setup.",
    );

    let record = review::show(temp.path(), id).expect("show");
    assert_eq!(record.id, id);
    assert_eq!(record.kind, "decision");
    assert_eq!(record.status, "pending");
    assert_eq!(record.actor, "claude-code");
    assert_eq!(record.actor_raw, "claude-ai");
    assert_eq!(record.rationale, "Avoid system SQLite setup.");
    assert!(record.provenance_json.contains("\"source\":\"mcp\""));
    assert!(record.reviewed_at.is_none());
}

#[test]
fn review_show_errors_for_unknown_id() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    match review::show(temp.path(), 999) {
        Err(MemhubError::InvalidInput(message)) => assert!(message.contains("no pending write")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn review_accept_promotes_fact_and_marks_pending_accepted() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_fact(
        temp.path(),
        "build-command",
        "cargo build",
        "Observed across recent sessions.",
    );

    let outcome = review::accept(temp.path(), pending_id, "cli:user").expect("accept");
    assert_eq!(outcome.kind, "fact");
    assert_eq!(outcome.durable_table, "facts");
    assert!(outcome.durable_id > 0);

    let facts = fact::list(temp.path()).expect("fact list");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].key, "build-command");
    assert_eq!(facts[0].value, "cargo build");
    assert_eq!(facts[0].source, "user+agent:codex");
    assert!((facts[0].confidence - 1.0).abs() < f64::EPSILON);

    let pending = review::show(temp.path(), pending_id).expect("show");
    assert_eq!(pending.status, "accepted");
    assert!(pending.reviewed_at.is_some());

    let summary = status::run(temp.path()).expect("status");
    assert_eq!(summary.pending_writes, 0);
    assert_eq!(summary.facts, 1);
}

#[test]
fn review_accept_preserves_opencode_source() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_fact_from_agent(
        temp.path(),
        "opencode-test-command",
        "cargo test",
        "OpenCode surfaced this command for review.",
        "opencode",
        "OpenCode",
    );

    review::accept(temp.path(), pending_id, "cli:user").expect("accept");

    let facts = fact::list(temp.path()).expect("fact list");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].source, "user+agent:opencode");
}

#[test]
fn review_accept_promotes_decision_and_indexes_fts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_decision(
        temp.path(),
        "Adopt the kraken pattern",
        "Sea creatures organize concurrent workloads cleanly.",
    );

    let outcome = review::accept(temp.path(), pending_id, "cli:user").expect("accept decision");
    assert_eq!(outcome.kind, "decision");
    assert_eq!(outcome.durable_table, "decisions");

    let decisions = decision::list(temp.path()).expect("decision list");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].title, "Adopt the kraken pattern");
    assert_eq!(decisions[0].status, "active");
    assert_eq!(decisions[0].source, "user+agent:claude-code");

    let response = search::run(temp.path(), "kraken", 5).expect("search");
    assert!(
        !response.results.is_empty(),
        "promoted decision should be searchable via FTS"
    );
}

#[test]
fn review_accept_errors_on_non_pending_row() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_fact(
        temp.path(),
        "test-command",
        "cargo test",
        "Should be reviewed.",
    );
    review::accept(temp.path(), pending_id, "cli:user").expect("first accept");

    match review::accept(temp.path(), pending_id, "cli:user") {
        Err(MemhubError::InvalidInput(message)) => {
            assert!(message.contains("already accepted"));
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn review_accept_rolls_back_durable_write_when_pending_already_reviewed() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_fact(
        temp.path(),
        "deploy-command",
        "./deploy.sh",
        "Concurrent reviewer scenario.",
    );

    // Simulate a concurrent reviewer that finished first (e.g., rejected the row
    // before this acceptor opens its transaction). The acceptor must not create a
    // durable facts row when it sees the row is no longer pending.
    let ctx = memhub::db::open_project(temp.path()).expect("open");
    ctx.conn
        .execute(
            "UPDATE pending_writes
             SET status = 'rejected', reviewed_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            params![pending_id],
        )
        .expect("simulate concurrent reject");
    drop(ctx);

    match review::accept(temp.path(), pending_id, "cli:user") {
        Err(MemhubError::InvalidInput(message)) => {
            assert!(message.contains("already rejected"), "message: {message}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    let facts = fact::list(temp.path()).expect("fact list");
    assert!(
        facts.is_empty(),
        "no durable fact should be created when accept errors out",
    );
}

#[test]
fn review_reject_records_reason_in_writes_log() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let pending_id = stage_fact(temp.path(), "deploy-command", "./deploy.sh", "Looks risky.");
    review::reject(
        temp.path(),
        pending_id,
        Some("Untrusted source"),
        "cli:user",
    )
    .expect("reject");

    let pending = review::show(temp.path(), pending_id).expect("show");
    assert_eq!(pending.status, "rejected");

    let ctx = memhub::db::open_project(temp.path()).expect("open");
    let reason: String = ctx
        .conn
        .query_row(
            "SELECT reason FROM writes_log
             WHERE table_name = 'pending_writes' AND row_id = ?1
             ORDER BY id DESC LIMIT 1",
            params![pending_id],
            |row| row.get(0),
        )
        .expect("query writes_log");
    assert!(
        reason.contains("Untrusted source"),
        "writes_log reason should preserve user-provided text, got: {reason}"
    );
}

#[test]
fn review_expire_marks_old_pending_writes_and_leaves_fresh_ones() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let old_id = stage_fact(temp.path(), "old-fact", "value", "Old proposal.");
    stage_fact(temp.path(), "fresh-fact", "value", "Fresh proposal.");

    let ctx = memhub::db::open_project(temp.path()).expect("open");
    ctx.conn
        .execute(
            "UPDATE pending_writes
             SET created_at = datetime('now', '-45 days')
             WHERE id = ?1",
            params![old_id],
        )
        .expect("backdate row");
    drop(ctx);

    let summary = review::expire(temp.path(), 30).expect("expire");
    assert_eq!(summary.expired, 1);
    assert_eq!(summary.older_than_days, 30);

    let old = review::show(temp.path(), old_id).expect("show old");
    assert_eq!(old.status, "expired");
    assert!(old.reviewed_at.is_some());

    let fresh = review::list(temp.path(), Some("pending"), 25).expect("list pending");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].kind, "fact");
}

#[test]
fn review_list_rejects_unknown_status_filter() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    match review::list(temp.path(), Some("bogus"), 10) {
        Err(MemhubError::InvalidInput(message)) => assert!(message.contains("bogus")),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}
