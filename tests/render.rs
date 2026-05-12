use std::fs;

use memhub::commands::{decision, fact, init, narrative, render, task};
use memhub::config::ProjectConfig;
use memhub::models::NarrativeKind;
use tempfile::tempdir;

fn read_string(path: &std::path::Path) -> String {
    fs::read_to_string(path).expect("read rendered file")
}

#[test]
fn render_empty_repo_writes_placeholder_files() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let result = render::run(temp.path()).expect("render");

    assert_eq!(
        result.output_dir,
        temp.path().join("agent_docs"),
        "default output dir should be agent_docs"
    );
    assert!(result.project_md_path.exists());
    assert!(result.ledger_md_path.exists());
    assert!(
        result.backup_files.is_empty(),
        "no prior files to back up on first render"
    );
    assert_eq!(result.written_files.len(), 2);

    let project = read_string(&result.project_md_path);
    assert!(project.contains("<!-- memhub:rendered -->"));
    assert!(project.contains("# "));
    assert!(project.contains("No project_state recorded"));
    assert!(project.contains("No project_arch recorded"));
    assert!(project.contains("No session notes recorded"));

    let ledger = read_string(&result.ledger_md_path);
    assert!(ledger.contains("<!-- memhub:rendered -->"));
    assert!(ledger.contains("Ledger"));
    assert!(ledger.contains("No decisions recorded"));
    assert!(ledger.contains("No tasks recorded"));
    assert!(ledger.contains("No facts recorded"));
}

#[test]
fn render_includes_state_arch_decisions_tasks_facts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    narrative::set(
        temp.path(),
        NarrativeKind::State,
        "Currently shipping the render slice.",
        "cli:user",
        "cli:user",
    )
    .expect("state set");
    narrative::set(
        temp.path(),
        NarrativeKind::Arch,
        "## Subsystems\n\nRust CLI + SQLite + render.",
        "cli:user",
        "cli:user",
    )
    .expect("arch set");
    decision::add(
        temp.path(),
        "Adopt two-file render shape",
        "Distinguishes from K9; clean diff cadence.",
        "cli:user",
    )
    .expect("decision");
    let _t = task::add(
        temp.path(),
        "Wire render core",
        Some("Step 2 of the render slice."),
        "cli:user",
    )
    .expect("task");
    fact::add(
        temp.path(),
        "build-command",
        "cargo build",
        "user",
        "cli:user",
    )
    .expect("fact");

    let result = render::run(temp.path()).expect("render");

    let project = read_string(&result.project_md_path);
    assert!(project.contains("Currently shipping the render slice."));
    assert!(project.contains("Rust CLI + SQLite + render."));

    let ledger = read_string(&result.ledger_md_path);
    assert!(ledger.contains("D1 — Adopt two-file render shape"));
    assert!(ledger.contains("Distinguishes from K9"));
    assert!(ledger.contains("T1 — Wire render core"));
    assert!(ledger.contains("Step 2 of the render slice."));
    assert!(ledger.contains("| build-command | cargo build |"));
    assert!(ledger.contains("Recent activity"));
}

#[test]
fn re_render_after_change_creates_a_backup() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    render::run(temp.path()).expect("first render");

    decision::add(
        temp.path(),
        "Add a decision",
        "Triggers a content change.",
        "cli:user",
    )
    .expect("decision");

    let result = render::run(temp.path()).expect("second render");
    assert_eq!(
        result.backup_files.len(),
        2,
        "both PROJECT.md and PROJECT_LEDGER.md should be backed up"
    );

    let backup_dir = temp.path().join(".memhub").join("backups").join("rendered");
    assert!(backup_dir.exists());
    let entries: Vec<_> = fs::read_dir(&backup_dir)
        .expect("read backup dir")
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 2);

    let ledger = read_string(&result.ledger_md_path);
    assert!(ledger.contains("Add a decision"));
}

#[test]
fn render_respects_custom_output_dir_from_config() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.render.output_dir = "docs/state".to_string();
    config.save(&config_path).expect("save config");

    let result = render::run(temp.path()).expect("render");

    assert_eq!(result.output_dir, temp.path().join("docs").join("state"));
    assert!(result.project_md_path.starts_with(temp.path().join("docs").join("state")));
    assert!(result.project_md_path.exists());
    assert!(result.ledger_md_path.exists());
}

#[test]
fn render_overwrites_human_edit_after_backing_it_up() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let first = render::run(temp.path()).expect("first render");

    fs::write(&first.project_md_path, "# I edited this by hand\n").expect("hand edit");
    let stale_body = read_string(&first.project_md_path);
    assert!(stale_body.contains("I edited this by hand"));

    let second = render::run(temp.path()).expect("second render");

    let restored = read_string(&second.project_md_path);
    assert!(restored.contains("<!-- memhub:rendered -->"));
    assert!(!restored.contains("I edited this by hand"));

    let backup_dir = temp.path().join(".memhub").join("backups").join("rendered");
    let backups: Vec<_> = fs::read_dir(&backup_dir)
        .expect("read backups")
        .map(|e| e.unwrap().path())
        .collect();
    let mut found_edit = false;
    for path in backups {
        if read_string(&path).contains("I edited this by hand") {
            found_edit = true;
            break;
        }
    }
    assert!(found_edit, "the edited body should survive in a backup");
}

#[test]
fn render_logs_a_writes_log_entry() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    render::run(temp.path()).expect("render");

    let conn = rusqlite::Connection::open(temp.path().join(".memhub").join("project.sqlite"))
        .expect("open db");
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM writes_log
             WHERE table_name = 'render' AND action = 'render'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1);
}
