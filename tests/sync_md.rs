use std::fs;
use std::path::{Path, PathBuf};

use memhub::MemhubError;
use memhub::commands::{command, decision, fact, init, sync_md, task};
use memhub::config::ProjectConfig;
use tempfile::tempdir;

#[test]
fn init_creates_managed_markdown_and_preserves_manual_content() {
    let temp = tempdir().expect("tempdir");
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Local notes\n\nKeep this content.\n",
    )
    .expect("seed agents");

    init::run(temp.path()).expect("init succeeds");

    let agents = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");
    let claude = fs::read_to_string(temp.path().join("CLAUDE.md")).expect("read claude");

    assert!(agents.starts_with("# Local notes\n\nKeep this content.\n"));
    assert!(agents.contains("<!-- memhub:managed:start -->"));
    assert!(agents.contains("## Project state (auto-generated)"));
    assert!(claude.contains("<!-- memhub:managed:start -->"));
    assert!(claude.contains("### Durable decisions"));

    let backups = markdown_backup_files(temp.path());
    assert_eq!(backups.len(), 1);
    assert!(backups[0].display().to_string().contains("AGENTS.md"));
}

#[test]
fn sync_md_renders_db_state_and_creates_backups_for_existing_files() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let agents_before = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");

    command::verify(temp.path(), "build", "cargo build", 0).expect("build command");
    command::verify(temp.path(), "test", "cargo test", 0).expect("test command");
    task::add(temp.path(), "Implement markdown sync", Some("Milestone 3")).expect("task add");
    decision::add(
        temp.path(),
        "Managed block lives at the bottom",
        "Keep hand-authored instructions visible first.",
    )
    .expect("decision add");
    fact::add(
        temp.path(),
        "quirk.windows-toolchain",
        "Windows builds require the MSVC toolchain.",
        "user",
    )
    .expect("fact add");

    let result = sync_md::run(temp.path()).expect("sync");
    let agents = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");

    assert_eq!(result.updated_files.len(), 2);
    assert_eq!(result.backup_files.len(), 2);
    assert!(agents.contains("**Build:** `cargo build`"));
    assert!(agents.contains("**Test:** `cargo test`"));
    assert!(agents.contains("**Active tasks:** 1 open, 0 blocked"));
    assert!(agents.contains("- Managed block lives at the bottom"));
    assert!(
        agents.contains("- quirk.windows-toolchain: Windows builds require the MSVC toolchain.")
    );

    let agent_backup = find_backup_for(&result.backup_files, "AGENTS.md");
    let backup_contents = fs::read_to_string(agent_backup).expect("read backup");
    assert_eq!(backup_contents, agents_before);
}

#[test]
fn sync_md_does_not_create_backup_for_noop_sync() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let result = sync_md::run(temp.path()).expect("sync");

    assert!(result.updated_files.is_empty());
    assert!(result.backup_files.is_empty());
    assert!(markdown_backup_files(temp.path()).is_empty());
}

#[test]
fn sync_md_does_not_create_backup_for_brand_new_files() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    fs::remove_file(temp.path().join("AGENTS.md")).expect("remove agents");
    fs::remove_file(temp.path().join("CLAUDE.md")).expect("remove claude");

    let result = sync_md::run(temp.path()).expect("sync");

    assert_eq!(result.updated_files.len(), 2);
    assert!(result.backup_files.is_empty());
    assert!(temp.path().join("AGENTS.md").exists());
    assert!(temp.path().join("CLAUDE.md").exists());
    assert!(markdown_backup_files(temp.path()).is_empty());
}

#[test]
fn sync_md_rejects_invalid_markers_without_writing_any_files() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let claude_before = fs::read_to_string(temp.path().join("CLAUDE.md")).expect("read claude");
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Notes\n\n<!-- memhub:managed:start -->\nBroken\n",
    )
    .expect("write invalid agents");

    decision::add(
        temp.path(),
        "Marker validation should fail closed",
        "Never rewrite malformed managed blocks.",
    )
    .expect("decision add");

    let expected_agents = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");
    let err = sync_md::run(temp.path()).expect_err("sync should fail");

    assert!(matches!(err, MemhubError::InvalidManagedMarkdown { .. }));
    assert_eq!(
        fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents"),
        expected_agents
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("CLAUDE.md")).expect("read claude"),
        claude_before
    );
    assert!(markdown_backup_files(temp.path()).is_empty());
}

#[test]
fn sync_md_preserves_manual_content_outside_managed_block() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let existing = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");
    let customized =
        format!("# Manual intro\n\nKeep this.\n\n{existing}\n\n## Manual footer\nDo not remove.\n");
    fs::write(temp.path().join("AGENTS.md"), customized).expect("customize agents");

    decision::add(
        temp.path(),
        "Preserve manual content around sync blocks",
        "Only the managed section should change.",
    )
    .expect("decision add");

    sync_md::run(temp.path()).expect("sync");

    let agents = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");
    assert!(agents.starts_with("# Manual intro\n\nKeep this.\n\n"));
    assert!(agents.contains("\n\n## Manual footer\nDo not remove.\n"));
    assert!(agents.contains("- Preserve manual content around sync blocks"));
}

#[test]
fn auto_sync_md_updates_markdown_after_writes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.auto_sync_md = true;
    config.save(&config_path).expect("save config");

    decision::add(
        temp.path(),
        "Use explicit markdown sync markers",
        "Only rewrite the managed section.",
    )
    .expect("decision add");

    let agents = fs::read_to_string(temp.path().join("AGENTS.md")).expect("read agents");
    assert!(agents.contains("- Use explicit markdown sync markers"));
}

fn markdown_backup_files(repo_root: &Path) -> Vec<PathBuf> {
    let backup_dir = repo_root.join(".memhub").join("backups").join("markdown");
    if !backup_dir.exists() {
        return Vec::new();
    }

    let mut entries = fs::read_dir(backup_dir)
        .expect("read backup dir")
        .map(|entry| entry.expect("dir entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn find_backup_for<'a>(paths: &'a [PathBuf], file_name: &str) -> &'a Path {
    paths
        .iter()
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.contains(file_name))
                .unwrap_or(false)
        })
        .map(PathBuf::as_path)
        .expect("backup for file")
}
