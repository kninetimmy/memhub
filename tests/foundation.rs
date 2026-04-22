use std::fs;

use memhub::commands::{decision, fact, init, status, task};
use tempfile::tempdir;

#[test]
fn init_creates_memhub_layout_and_gitignore_entry() {
    let temp = tempdir().expect("tempdir");
    fs::write(temp.path().join(".gitignore"), "/target/\n").expect("seed gitignore");

    let result = init::run(temp.path()).expect("init succeeds");

    assert!(result.db_path.exists());
    assert!(temp.path().join(".memhub").join("config.toml").exists());

    let gitignore = fs::read_to_string(temp.path().join(".gitignore")).expect("read gitignore");
    assert!(gitignore.contains(".memhub/"));
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
    assert!(summary.writes_logged >= 3);
}
