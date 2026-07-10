use std::fs;
use std::process::Command;

use memhub::commands::{decision, fact, init, narrative, render, task};
use memhub::config::ProjectConfig;
use memhub::models::NarrativeKind;
use tempfile::tempdir;

fn read_string(path: &std::path::Path) -> String {
    fs::read_to_string(path).expect("read rendered file")
}

fn run_cli(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run memhub CLI")
}

#[test]
fn render_empty_repo_writes_placeholder_files() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let result = render::run(temp.path(), "cli:user").expect("render");

    assert_eq!(
        result.output_dir,
        temp.path().join(".memhub").join("rendered"),
        "default output dir should be local memhub rendered dir"
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
        "user",
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

    let result = render::run(temp.path(), "cli:user").expect("render");

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
    render::run(temp.path(), "cli:user").expect("first render");

    decision::add(
        temp.path(),
        "Add a decision",
        "Triggers a content change.",
        "user",
        "cli:user",
    )
    .expect("decision");

    let result = render::run(temp.path(), "cli:user").expect("second render");
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

    let result = render::run(temp.path(), "cli:user").expect("render");

    assert_eq!(result.output_dir, temp.path().join("docs").join("state"));
    assert!(
        result
            .project_md_path
            .starts_with(temp.path().join("docs").join("state"))
    );
    assert!(result.project_md_path.exists());
    assert!(result.ledger_md_path.exists());
}

#[test]
fn render_overwrites_human_edit_after_backing_it_up() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let first = render::run(temp.path(), "cli:user").expect("first render");

    fs::write(&first.project_md_path, "# I edited this by hand\n").expect("hand edit");
    let stale_body = read_string(&first.project_md_path);
    assert!(stale_body.contains("I edited this by hand"));

    let second = render::run(temp.path(), "cli:user").expect("second render");

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

    render::run(temp.path(), "cli:user").expect("render");

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

#[test]
fn render_cli_accepts_actor_and_preserves_default_attribution() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let attributed = run_cli(temp.path(), &["render", "--actor", "codex:wrap-up"]);
    assert!(
        attributed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&attributed.stderr)
    );
    assert!(temp.path().join(".memhub/rendered/PROJECT.md").is_file());
    assert!(
        temp.path()
            .join(".memhub/rendered/PROJECT_LEDGER.md")
            .is_file()
    );

    let conn =
        rusqlite::Connection::open(temp.path().join(".memhub/project.sqlite")).expect("open db");
    let actor: String = conn
        .query_row(
            "SELECT actor FROM writes_log
             WHERE table_name = 'render' AND action = 'render'
             ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("custom render actor");
    assert_eq!(actor, "codex:wrap-up");

    let defaulted = run_cli(temp.path(), &["render"]);
    assert!(
        defaulted.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&defaulted.stderr)
    );
    let actor: String = conn
        .query_row(
            "SELECT actor FROM writes_log
             WHERE table_name = 'render' AND action = 'render'
             ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("default render actor");
    assert_eq!(actor, "cli:user");
}

#[test]
fn render_cli_rejects_an_invalid_actor_before_rendering() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let output = run_cli(temp.path(), &["render", "--actor", "   "]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--actor cannot be empty"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!temp.path().join(".memhub/rendered/PROJECT.md").exists());
}

#[test]
fn render_does_not_double_up_section_headings_in_narrative_bodies() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // Wrap-up bodies are drafted as full mini-docs that lead with the section
    // heading they belong under. Render should not stack a second copy of the
    // same heading on top.
    narrative::set(
        temp.path(),
        NarrativeKind::State,
        "## Currently building\n\nM8 retrieval slice.\n",
        "cli:user",
        "cli:user",
    )
    .expect("state set");
    narrative::set(
        temp.path(),
        NarrativeKind::Arch,
        "## Architecture\n\nRust CLI + SQLite.\n",
        "cli:user",
        "cli:user",
    )
    .expect("arch set");

    let result = render::run(temp.path(), "cli:user").expect("render");
    let project = read_string(&result.project_md_path);

    let state_heading_count = project.matches("## Currently building").count();
    assert_eq!(
        state_heading_count, 1,
        "expected exactly one '## Currently building' heading, found {state_heading_count}:\n{project}",
    );

    let arch_heading_count = project.matches("## Architecture").count();
    assert_eq!(
        arch_heading_count, 1,
        "expected exactly one '## Architecture' heading, found {arch_heading_count}:\n{project}",
    );
    assert!(project.contains("M8 retrieval slice."));
    assert!(project.contains("Rust CLI + SQLite."));
}

#[test]
fn render_leaves_project_md_untouched_when_ledger_path_is_unwritable() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // Establish a baseline PROJECT.md so we can detect changes after a failed
    // re-render. After this call both files are present and consistent.
    render::run(temp.path(), "cli:user").expect("baseline render");

    let rendered_dir = temp.path().join(".memhub").join("rendered");
    let project_path = rendered_dir.join("PROJECT.md");
    let ledger_path = rendered_dir.join("PROJECT_LEDGER.md");
    let baseline_project = read_string(&project_path);

    // Replace the ledger file with a directory. Backup of a directory via
    // fs::copy will fail in phase 1, so phase 2 should never run and
    // PROJECT.md must remain at its baseline content.
    fs::remove_file(&ledger_path).expect("remove ledger");
    fs::create_dir(&ledger_path).expect("plant directory at ledger path");

    // Change DB state so a successful render would alter PROJECT.md.
    decision::add(
        temp.path(),
        "Added between renders",
        "Should not appear in PROJECT.md if render aborts.",
        "user",
        "cli:user",
    )
    .expect("decision");

    let result = render::run(temp.path(), "cli:user");
    assert!(
        result.is_err(),
        "render should fail when the ledger path is a directory",
    );

    let after_project = read_string(&project_path);
    assert_eq!(
        after_project, baseline_project,
        "PROJECT.md must remain at its prior snapshot when the ledger write cannot complete",
    );

    // PROJECT_LEDGER.md is still a directory because the render aborted in
    // phase 1; recovery is up to the operator.
    assert!(ledger_path.is_dir());
}
