use std::path::Path;
use std::process::Command;

use memhub::commands::{init, session_note};
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

fn last_writes_log_for_table(repo: &Path, table: &str) -> Option<(String, String, String)> {
    let ctx = db::open_project(repo).expect("open project");
    ctx.conn
        .query_row(
            "SELECT actor, table_name, action
             FROM writes_log
             WHERE table_name = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![table],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok()
}

#[test]
fn session_note_add_persists_row_and_logs_audit() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let note = session_note::add(
        temp.path(),
        "tried the new compaction step, no observable effect yet",
        "claude-code",
        "claude-ai",
    )
    .expect("add note");
    assert!(note.id > 0);
    assert_eq!(note.actor, "claude-code");
    assert_eq!(note.actor_raw, "claude-ai");
    assert!(note.text.contains("tried the new compaction"));
    assert!(!note.created_at.is_empty());

    let (actor, table, action) =
        last_writes_log_for_table(temp.path(), "session_notes").expect("audit row exists");
    assert_eq!(actor, "claude-code");
    assert_eq!(table, "session_notes");
    assert_eq!(action, "insert");
}

#[test]
fn session_note_add_rejects_empty_text() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let err = session_note::add(temp.path(), "   ", "x", "x").expect_err("must reject");
    assert!(format!("{err}").contains("must not be empty"));
}

#[test]
fn session_note_add_rejects_overlong_text() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let too_long = "a".repeat(session_note::MAX_TEXT_LEN + 1);
    let err = session_note::add(temp.path(), &too_long, "x", "x").expect_err("must reject");
    assert!(format!("{err}").contains("characters or fewer"));
}

#[test]
fn session_note_list_orders_by_recency_and_filters_by_actor() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    session_note::add(temp.path(), "first claude note", "claude-code", "claude-ai").expect("a");
    session_note::add(temp.path(), "first codex note", "codex", "openai-codex").expect("b");
    session_note::add(
        temp.path(),
        "second claude note",
        "claude-code",
        "claude-ai",
    )
    .expect("c");

    let all = session_note::list(temp.path(), 10, None, None).expect("list all");
    assert_eq!(all.len(), 3);
    // newest first
    assert!(all[0].text.contains("second claude"));

    let claude_only = session_note::list(temp.path(), 10, Some("claude-code"), None).expect("list");
    assert_eq!(claude_only.len(), 2);
    assert!(claude_only.iter().all(|n| n.actor == "claude-code"));
}

#[test]
fn session_note_list_respects_since_days_window() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let old = session_note::add(temp.path(), "old note", "actor", "actor").expect("old");
    session_note::add(temp.path(), "fresh note", "actor", "actor").expect("fresh");

    {
        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE session_notes SET created_at = datetime('now', '-30 days') WHERE id = ?1",
                params![old.id],
            )
            .expect("backdate old note");
    }

    let recent = session_note::list(temp.path(), 10, None, Some(7)).expect("recent");
    assert_eq!(recent.len(), 1);
    assert!(recent[0].text.contains("fresh"));

    let all = session_note::list(temp.path(), 10, None, None).expect("all");
    assert_eq!(all.len(), 2);
}

#[test]
fn note_list_cli_human_output_smoke() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    session_note::add(temp.path(), "human-readable note", "cli:user", "cli:user").expect("add");

    let output = run_cli(temp.path(), &["note", "list"]);
    assert!(
        output.status.success(),
        "note list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("human-readable note"));
    assert!(stdout.contains("actor=cli:user"));
}

#[test]
fn note_list_cli_json_envelope_shape() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    session_note::add(temp.path(), "json envelope test", "cli:user", "cli:user").expect("add");

    let output = run_cli(temp.path(), &["note", "list", "--json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let payload: Value = serde_json::from_str(stdout.trim()).expect("json");

    let notes = payload["session_notes"].as_array().expect("array");
    assert_eq!(notes.len(), 1);
    let row = &notes[0];
    assert!(row["id"].as_i64().expect("id") > 0);
    assert_eq!(row["actor"], "cli:user");
    assert_eq!(row["text"], "json envelope test");
    assert!(row["created_at"].is_string());
}

#[test]
fn note_list_cli_empty_repo_prints_no_match_message() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["note", "list"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No session notes match this filter."));
}

#[test]
fn note_add_cli_creates_note_via_positional_text() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["note", "add", "first cli note"]);
    assert!(
        output.status.success(),
        "note add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Recorded note"));

    let notes = session_note::list(temp.path(), 10, None, None).expect("list");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "first cli note");
    assert_eq!(notes[0].actor, "cli:user");
}

#[test]
fn note_add_cli_reads_body_from_file() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let body_path = temp.path().join("note-body.txt");
    std::fs::write(&body_path, "multi\nline\nnote body").expect("write body file");

    let output = run_cli(
        temp.path(),
        &[
            "note",
            "add",
            "--from-file",
            body_path.to_str().expect("path"),
        ],
    );
    assert!(
        output.status.success(),
        "note add --from-file failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let notes = session_note::list(temp.path(), 10, None, None).expect("list");
    assert_eq!(notes.len(), 1);
    assert!(notes[0].text.contains("multi"));
    assert!(notes[0].text.contains("note body"));
}

#[test]
fn note_add_cli_rejects_text_and_from_file_together() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let body_path = temp.path().join("body.txt");
    std::fs::write(&body_path, "from file").expect("write body file");

    let output = run_cli(
        temp.path(),
        &[
            "note",
            "add",
            "inline text",
            "--from-file",
            body_path.to_str().expect("path"),
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not both"));

    let notes = session_note::list(temp.path(), 10, None, None).expect("list");
    assert!(notes.is_empty(), "no note should have been written");
}

#[test]
fn note_add_cli_requires_text_or_from_file() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["note", "add"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("provide a text argument or --from-file"));
}

#[test]
fn note_add_cli_emits_json_envelope() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["note", "add", "json note", "--json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let payload: Value = serde_json::from_str(stdout.trim()).expect("json");

    assert!(payload["id"].as_i64().expect("id") > 0);
    assert_eq!(payload["actor"], "cli:user");
    assert_eq!(payload["text"], "json note");
    assert!(payload["created_at"].is_string());
}

#[test]
fn note_add_cli_propagates_actor_to_writes_log() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(
        temp.path(),
        &["note", "add", "actor test", "--actor", "claude:wrap-up"],
    );
    assert!(output.status.success());

    let (actor, table, action) =
        last_writes_log_for_table(temp.path(), "session_notes").expect("audit row");
    assert_eq!(actor, "claude:wrap-up");
    assert_eq!(table, "session_notes");
    assert_eq!(action, "insert");
}
