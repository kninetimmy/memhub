use std::path::Path;
use std::process::Command;

use memhub::commands::init;
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

fn run_cli_expecting_success(repo: &Path, args: &[&str]) -> Value {
    let output = run_cli(repo, args);
    assert!(
        output.status.success(),
        "command {args:?} failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| {
        panic!("expected JSON on stdout, got {stdout:?}: {err}");
    })
}

// `last_writes_log_row` (below) and `memhub::commands::pending_write::
// propose_fact` (called directly, in-process, by several tests) both reach
// `db::open_project` directly, which resolves `db::home_dir()`
// unconditionally, in-process (Wave 5 U4, issue #90; see
// `upgrade/support.rs`'s reader-trigger closure doc). `memhub::commands::
// init::run` used to ALSO reach it transitively via a trailing
// `sync_md::sync_project` call; that call was removed with the `sync_md`
// channel's retirement (audit C5 / task 119), so `init::run` alone no
// longer needs the guard. Every test that calls `init::run`,
// `last_writes_log_row`, or `propose_fact` still takes
// `support::env_read_lock()` for the whole test regardless — cheap and
// harmless even where `init::run` is now the only reason a given test
// looks like it needs it — guarding against a concurrent writer test's
// `HOME`/`USERPROFILE` override elsewhere in this shared harness binary.
// `run_cli`/`run_cli_expecting_success` themselves don't need it — a
// spawned child process gets its own independent snapshot of the
// environment at spawn time, so nothing it does in-process can race the
// parent test's threads — but that only makes a WHOLE test safe if it also
// never calls `init::run` or one of the two above in-process. Every test in
// this file does, so every test takes the guard.
fn last_writes_log_row(repo: &Path) -> (String, String, String) {
    let ctx = db::open_project(repo).expect("open project");
    ctx.conn
        .query_row(
            "SELECT actor, table_name, action
             FROM writes_log
             ORDER BY id DESC
             LIMIT 1",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("writes_log row exists")
}

fn writes_log_count(repo: &Path) -> i64 {
    let ctx = db::open_project(repo).expect("open project");
    ctx.conn
        .query_row("SELECT COUNT(*) FROM writes_log", params![], |row| {
            row.get(0)
        })
        .expect("count writes_log")
}

#[test]
fn fact_add_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let payload = run_cli_expecting_success(
        temp.path(),
        &[
            "fact",
            "add",
            "build-command",
            "cargo build",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );

    assert_eq!(payload["key"], "build-command");
    assert_eq!(payload["value"], "cargo build");
    assert_eq!(payload["source"], "user");
    assert_eq!(payload["created"], true);
    assert!(payload["id"].as_i64().expect("id is i64") > 0);

    let (actor, table, action) = last_writes_log_row(temp.path());
    assert_eq!(actor, "agent:wrap-up");
    assert_eq!(table, "facts");
    assert_eq!(action, "insert");
}

#[test]
fn fact_add_json_marks_upsert_as_not_created() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    run_cli_expecting_success(
        temp.path(),
        &[
            "fact",
            "add",
            "key",
            "v1",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );
    let second = run_cli_expecting_success(
        temp.path(),
        &[
            "fact",
            "add",
            "key",
            "v2",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );

    assert_eq!(second["created"], false);
    assert_eq!(second["value"], "v2");
}

#[test]
fn decision_add_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let payload = run_cli_expecting_success(
        temp.path(),
        &[
            "decision",
            "add",
            "Use bundled rusqlite",
            "--rationale",
            "Local-first builds.",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );

    assert_eq!(payload["title"], "Use bundled rusqlite");
    assert!(payload["id"].as_i64().expect("id") > 0);

    let (actor, table, _) = last_writes_log_row(temp.path());
    assert_eq!(actor, "agent:wrap-up");
    assert_eq!(table, "decisions");
}

#[test]
fn task_add_and_done_json_round_trip() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let added = run_cli_expecting_success(
        temp.path(),
        &[
            "task",
            "add",
            "Ship contract",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );
    let task_id = added["id"].as_i64().expect("task id");
    assert_eq!(added["title"], "Ship contract");

    let done = run_cli_expecting_success(
        temp.path(),
        &[
            "task",
            "done",
            &task_id.to_string(),
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );
    assert_eq!(done["id"], task_id);
    assert_eq!(done["status"], "done");
}

#[test]
fn task_list_brief_limit_show_and_double_done() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let long_notes = format!("{}\nsecond line", "x".repeat(200));
    for args in [
        ["task", "add", "No notes"].as_slice(),
        ["task", "add", "Long notes", "--notes", &long_notes].as_slice(),
    ] {
        assert!(run_cli(temp.path(), args).status.success());
    }

    // --brief: one line per task, notes cut to <=120 chars with a marker.
    let out = run_cli(temp.path(), &["task", "list", "--brief"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout}");
    assert!(lines[0].starts_with("[2] [open] Long notes - xxx"));
    assert!(lines[0].ends_with("..."));
    assert!(!lines[0].contains("second line"));
    assert!(lines[0].len() - "[2] [open] Long notes - ".len() <= 120);
    assert_eq!(lines[1], "[1] [open] No notes");

    // --limit keeps the most-recently-updated-first order; 0 and brief+json fail.
    let limited =
        run_cli_expecting_success(temp.path(), &["task", "list", "--limit", "1", "--json"]);
    assert_eq!(limited["tasks"].as_array().expect("tasks").len(), 1);
    assert_eq!(limited["tasks"][0]["id"], 2);
    assert!(
        !run_cli(temp.path(), &["task", "list", "--limit", "0"])
            .status
            .success()
    );
    assert!(
        !run_cli(temp.path(), &["task", "list", "--brief", "--json"])
            .status
            .success()
    );

    // show --json has the same keys as a `task list --json` entry.
    let shown = run_cli_expecting_success(temp.path(), &["task", "show", "2", "--json"]);
    let listed = run_cli_expecting_success(temp.path(), &["task", "list", "--json"]);
    let keys = |v: &Value| {
        v.as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&shown), keys(&listed["tasks"][0]));
    assert_eq!(shown["notes"], long_notes);
    assert!(
        !run_cli(temp.path(), &["task", "show", "99"])
            .status
            .success()
    );

    // done twice: second call errors and leaves updated_at and the writes log alone.
    assert!(
        run_cli(temp.path(), &["task", "done", "1"])
            .status
            .success()
    );
    let before = run_cli_expecting_success(temp.path(), &["task", "show", "1", "--json"]);
    let writes_before = writes_log_count(temp.path());
    let again = run_cli(temp.path(), &["task", "done", "1"]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("already done"));
    let after = run_cli_expecting_success(temp.path(), &["task", "show", "1", "--json"]);
    assert_eq!(before["updated_at"], after["updated_at"]);
    assert_eq!(writes_before, writes_log_count(temp.path()));
}

#[test]
fn review_accept_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    memhub::commands::pending_write::propose_fact(
        temp.path(),
        "lint-command",
        "cargo fmt --check",
        "Observed in repo.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    let pending_id: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row(
                "SELECT id FROM pending_writes ORDER BY id DESC LIMIT 1",
                params![],
                |row| row.get(0),
            )
            .expect("pending id")
    };

    let payload = run_cli_expecting_success(
        temp.path(),
        &[
            "review",
            "accept",
            &pending_id.to_string(),
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );
    assert_eq!(payload["pending_id"], pending_id);
    assert_eq!(payload["kind"], "fact");
    assert_eq!(payload["durable_table"], "facts");
    assert!(payload["durable_id"].as_i64().expect("durable id") > 0);
}

#[test]
fn review_reject_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    memhub::commands::pending_write::propose_fact(
        temp.path(),
        "deploy-command",
        "./deploy.sh",
        "Risky.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    let pending_id: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row(
                "SELECT id FROM pending_writes ORDER BY id DESC LIMIT 1",
                params![],
                |row| row.get(0),
            )
            .expect("pending id")
    };

    let payload = run_cli_expecting_success(
        temp.path(),
        &[
            "review",
            "reject",
            &pending_id.to_string(),
            "--reason",
            "Untrusted source",
            "--json",
            "--actor",
            "agent:wrap-up",
        ],
    );
    assert_eq!(payload["pending_id"], pending_id);
}

#[test]
fn review_list_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    memhub::commands::pending_write::propose_fact(
        temp.path(),
        "lint-command",
        "cargo fmt --check",
        "Observed in repo.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    let payload = run_cli_expecting_success(
        temp.path(),
        &["review", "list", "--status", "pending", "--json"],
    );

    assert_eq!(payload["status"], "pending");
    let rows = payload["pending_writes"]
        .as_array()
        .expect("pending_writes array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row["id"].as_i64().expect("id") > 0);
    assert_eq!(row["kind"], "fact");
    assert_eq!(row["status"], "pending");
    assert_eq!(row["actor"], "codex");
    assert_eq!(row["actor_raw"], "openai-codex");
    assert_eq!(row["rationale"], "Observed in repo.");
    assert!(row["payload_json"].is_string());
    assert!(row["provenance_json"].is_string());
    assert!(row["created_at"].is_string());
    assert!(row["reviewed_at"].is_null());

    let inner: Value =
        serde_json::from_str(row["payload_json"].as_str().expect("payload_json str"))
            .expect("payload_json parses");
    assert_eq!(inner["key"], "lint-command");
    assert_eq!(inner["value"], "cargo fmt --check");
}

#[test]
fn review_list_json_filters_by_status() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    memhub::commands::pending_write::propose_fact(
        temp.path(),
        "k1",
        "v1",
        "r1",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    let accepted_filter = run_cli_expecting_success(
        temp.path(),
        &["review", "list", "--status", "accepted", "--json"],
    );
    assert_eq!(accepted_filter["status"], "accepted");
    assert_eq!(
        accepted_filter["pending_writes"]
            .as_array()
            .expect("array")
            .len(),
        0
    );

    let all_filter = run_cli_expecting_success(
        temp.path(),
        &["review", "list", "--status", "all", "--json"],
    );
    assert!(all_filter["status"].is_null());
    assert_eq!(
        all_filter["pending_writes"]
            .as_array()
            .expect("array")
            .len(),
        1
    );
}

#[test]
fn review_show_json_emits_contract_shape() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    memhub::commands::pending_write::propose_fact(
        temp.path(),
        "deploy-command",
        "./deploy.sh",
        "Risky.",
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose fact");

    let pending_id: i64 = {
        let ctx = db::open_project(temp.path()).expect("open project");
        ctx.conn
            .query_row(
                "SELECT id FROM pending_writes ORDER BY id DESC LIMIT 1",
                params![],
                |row| row.get(0),
            )
            .expect("pending id")
    };

    let payload = run_cli_expecting_success(
        temp.path(),
        &["review", "show", &pending_id.to_string(), "--json"],
    );

    assert_eq!(payload["id"], pending_id);
    assert_eq!(payload["kind"], "fact");
    assert_eq!(payload["status"], "pending");
    assert_eq!(payload["actor"], "codex");
    assert_eq!(payload["actor_raw"], "openai-codex");
    assert_eq!(payload["rationale"], "Risky.");
    assert!(payload["payload_json"].is_string());
    assert!(payload["provenance_json"].is_string());
    assert!(payload["created_at"].is_string());
    assert!(payload["reviewed_at"].is_null());
}

#[test]
fn review_show_json_missing_id_exits_nonzero() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["review", "show", "999", "--json"]);
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "missing-id show must not emit JSON on stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn actor_validation_rejects_empty_and_overlong_values() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let empty = run_cli(
        temp.path(),
        &["fact", "add", "k", "v", "--actor", "", "--json"],
    );
    assert!(!empty.status.success());

    let long = "a".repeat(65);
    let overlong = run_cli(
        temp.path(),
        &["fact", "add", "k", "v", "--actor", &long, "--json"],
    );
    assert!(!overlong.status.success());
}
