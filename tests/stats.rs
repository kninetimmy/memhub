use std::path::Path;
use std::process::Command;

use memhub::commands::{decision, fact, init, pending_write, review, stats};
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

fn backdate_writes_log(path: &Path, table: &str, days_ago: i64) {
    let ctx = db::open_project(path).expect("open project");
    ctx.conn
        .execute(
            "UPDATE writes_log SET at = datetime('now', ?1) WHERE table_name = ?2",
            params![format!("-{days_ago} days"), table],
        )
        .expect("backdate writes_log");
}

#[test]
fn stats_empty_repo_returns_zeros() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let summary = stats::run(temp.path(), stats::StatsWindow::Days(30)).expect("stats");
    assert_eq!(summary.facts, 0);
    assert_eq!(summary.decisions, 0);
    assert_eq!(summary.tasks_total, 0);
    assert_eq!(summary.commands, 0);
    assert!(summary.stale_ratio.is_none());
    assert!(summary.review_rate.is_none());
    assert!(summary.top_command_kinds.is_empty());
    assert!(summary.recent_facts.is_empty());
    assert_eq!(summary.window_label, "last 30 days");
    assert_eq!(summary.window_days, Some(30));
    assert_eq!(summary.pending_created_in_window, 0);
    assert_eq!(summary.pending_reviewed_in_window, 0);
}

#[test]
fn stats_counts_writes_within_window() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(temp.path(), "k1", "v1", "user", "cli:user").expect("add fact");
    decision::add(temp.path(), "d1", "because", "user", "cli:user").expect("add decision");

    let summary = stats::run(temp.path(), stats::StatsWindow::Days(30)).expect("stats");
    assert!(summary.writes_in_window >= 2);
    assert!(
        summary
            .writes_by_actor
            .iter()
            .any(|r| r.label == "cli:user" && r.count >= 2),
        "expected cli:user to appear with >= 2 writes; got {:?}",
        summary.writes_by_actor
    );
    assert!(
        summary.writes_by_table.iter().any(|r| r.label == "facts"),
        "expected facts table to appear; got {:?}",
        summary.writes_by_table
    );
    assert!(
        summary
            .writes_by_table
            .iter()
            .any(|r| r.label == "decisions"),
        "expected decisions table to appear; got {:?}",
        summary.writes_by_table
    );
}

#[test]
fn stats_window_excludes_old_writes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(temp.path(), "old", "v", "user", "cli:user").expect("add old fact");
    backdate_writes_log(temp.path(), "facts", 100);
    fact::add(temp.path(), "fresh", "v", "user", "cli:user").expect("add fresh fact");

    let week = stats::run(temp.path(), stats::StatsWindow::Days(7)).expect("week");
    let all = stats::run(temp.path(), stats::StatsWindow::All).expect("all");

    assert!(
        all.writes_in_window > week.writes_in_window,
        "all-time count ({}) should exceed 7-day count ({})",
        all.writes_in_window,
        week.writes_in_window
    );
    assert_eq!(all.window_days, None);
    assert_eq!(all.window_label, "all time");
}

#[test]
fn stats_review_rate_reflects_pending_writes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    pending_write::propose_fact(
        temp.path(),
        "k",
        "v",
        "r",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose 1");
    pending_write::propose_fact(
        temp.path(),
        "k2",
        "v",
        "r",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose 2");

    let pending_id: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row(
                "SELECT id FROM pending_writes ORDER BY id ASC LIMIT 1",
                params![],
                |row| row.get(0),
            )
            .expect("pending id")
    };
    review::accept(temp.path(), pending_id, "cli:user").expect("accept");

    let summary = stats::run(temp.path(), stats::StatsWindow::Days(30)).expect("stats");
    assert_eq!(summary.pending_created_in_window, 2);
    assert_eq!(summary.pending_reviewed_in_window, 1);
    let rate = summary.review_rate.expect("review_rate is some");
    assert!(
        (rate - 0.5).abs() < 1e-9,
        "review rate should be 0.5, got {rate}"
    );

    let by_status: std::collections::HashMap<String, i64> = summary
        .pending_by_status
        .into_iter()
        .map(|c| (c.label, c.count))
        .collect();
    assert_eq!(by_status.get("pending").copied(), Some(1));
    assert_eq!(by_status.get("accepted").copied(), Some(1));
}

#[test]
fn stats_stale_ratio_reflects_fact_state() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(temp.path(), "fresh", "v", "user", "cli:user").expect("add fresh");
    fact::add(temp.path(), "old", "v", "user", "cli:user").expect("add old");

    let ctx = db::open_project(temp.path()).expect("open project");
    ctx.conn
        .execute(
            "UPDATE facts SET verified_at = datetime('now', '-200 days') WHERE key = ?1",
            params!["old"],
        )
        .expect("backdate fact");
    drop(ctx);

    let summary = stats::run(temp.path(), stats::StatsWindow::Days(30)).expect("stats");
    assert_eq!(summary.facts, 2);
    assert_eq!(summary.stale_facts, 1);
    let ratio = summary.stale_ratio.expect("stale_ratio is some");
    assert!(
        (ratio - 0.5).abs() < 1e-9,
        "stale ratio should be 0.5, got {ratio}"
    );
}

#[test]
fn stats_cli_json_envelope_shape() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    fact::add(temp.path(), "k", "v", "user", "cli:user").expect("add fact");

    let output = run_cli(temp.path(), &["stats", "--window", "30d", "--json"]);
    assert!(
        output.status.success(),
        "stats --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let payload: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("expected JSON on stdout, got {stdout:?}: {err}"));

    assert!(payload["project_name"].is_string());
    assert!(payload["repo_root"].is_string());
    assert_eq!(payload["window"]["label"], "last 30 days");
    assert_eq!(payload["window"]["days"], 30);
    assert_eq!(payload["totals"]["facts"], 1);
    assert!(payload["activity"]["writes_in_window"].as_i64().unwrap() >= 1);
    assert!(payload["activity"]["writes_by_actor"].is_array());
    assert!(payload["activity"]["writes_by_table"].is_array());
    assert!(payload["pending_writes"]["by_status_all_time"].is_array());
    assert!(payload["top_command_kinds"].is_array());
    assert!(payload["recent_facts"].is_array());
    assert!(payload["notes"].is_array());
}

#[test]
fn stats_cli_all_window_emits_null_days() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["stats", "--window", "all", "--json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let payload: Value = serde_json::from_str(stdout.trim()).expect("json");
    assert!(payload["window"]["days"].is_null());
    assert_eq!(payload["window"]["label"], "all time");
}
