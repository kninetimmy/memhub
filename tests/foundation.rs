use std::fs;

use memhub::commands::{command, decision, fact, init, status, task};
use tempfile::tempdir;

#[test]
fn init_creates_memhub_layout_and_gitignore_entry() {
    let temp = tempdir().expect("tempdir");
    fs::write(temp.path().join(".gitignore"), "/target/\n").expect("seed gitignore");

    let result = init::run(temp.path()).expect("init succeeds");

    assert!(result.db_path.exists());
    assert!(temp.path().join(".memhub").join("config.toml").exists());
    assert!(
        result
            .migrations_applied
            .contains(&"0004_pending_write_provenance".to_string())
    );

    let gitignore = fs::read_to_string(temp.path().join(".gitignore")).expect("read gitignore");
    assert!(gitignore.contains(".memhub/"));
}

#[test]
fn init_does_not_duplicate_existing_memhub_gitignore_entry() {
    let temp = tempdir().expect("tempdir");
    fs::write(temp.path().join(".gitignore"), "/target/\n/.memhub/\n").expect("seed gitignore");

    let result = init::run(temp.path()).expect("init succeeds");
    let gitignore = fs::read_to_string(temp.path().join(".gitignore")).expect("read gitignore");

    assert!(!result.gitignore_updated);
    assert_eq!(gitignore.matches(".memhub/").count(), 1);
}

#[test]
fn core_records_are_persisted_and_status_counts_change() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let (_fact_id, created) =
        fact::add(temp.path(), "build-command", "cargo build", "user").expect("fact add");
    assert!(created);

    let decision_id = decision::add(
        temp.path(),
        "Use rusqlite bundled mode",
        "Avoid system SQLite setup friction.",
    )
    .expect("decision add");
    assert!(decision_id > 0);

    let task_id =
        task::add(temp.path(), "Implement MCP server", Some("Milestone 3")).expect("task add");
    task::done(temp.path(), task_id).expect("task done");

    let facts = fact::list(temp.path()).expect("fact list");
    let decisions = decision::list(temp.path()).expect("decision list");
    let tasks = task::list(temp.path(), Some("all")).expect("task list");
    let summary = status::run(temp.path()).expect("status");

    assert_eq!(facts.len(), 1);
    assert_eq!(decisions.len(), 1);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, "done");
    assert_eq!(summary.facts, 1);
    assert_eq!(summary.decisions, 1);
    assert_eq!(summary.tasks_total, 1);
    assert_eq!(summary.tasks_open, 0);
    assert_eq!(summary.pending_writes, 0);
    assert!(summary.writes_logged >= 3);
}

#[test]
fn command_verify_upserts_history_and_updates_status_counts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let (command_id, created) =
        command::verify(temp.path(), "build", "cargo build", 0).expect("command verify insert");
    assert!(created);
    assert!(command_id > 0);

    let (same_command_id, created) =
        command::verify(temp.path(), "build", "cargo build", 101).expect("command verify update");
    assert!(!created);
    assert_eq!(same_command_id, command_id);

    let commands = command::list(temp.path()).expect("command list");
    let summary = status::run(temp.path()).expect("status");

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].kind, "build");
    assert_eq!(commands[0].cmdline, "cargo build");
    assert_eq!(commands[0].last_exit_code, Some(101));
    assert_eq!(commands[0].success_count, 1);
    assert_eq!(commands[0].fail_count, 1);
    assert_eq!(summary.commands, 1);
    assert_eq!(summary.pending_writes, 0);
    assert!(summary.writes_logged >= 2);
}
