use std::fs;
use std::path::{Path, PathBuf};

use memhub::commands::{command, decision, fact, init, sync_md, task};
use memhub::config::ProjectConfig;
use tempfile::tempdir;

fn rendered_agents(repo_root: &Path) -> PathBuf {
    repo_root.join(".memhub").join("rendered").join("AGENTS.md")
}

fn rendered_claude(repo_root: &Path) -> PathBuf {
    repo_root.join(".memhub").join("rendered").join("CLAUDE.md")
}

#[test]
fn init_writes_rendered_markdown_under_memhub_rendered() {
    let temp = tempdir().expect("tempdir");
    // A pre-existing repo-root CLAUDE.md must NOT be modified by memhub init or
    // sync — those tracked files are the user's static guardrails.
    fs::write(
        temp.path().join("CLAUDE.md"),
        "# Tracked guardrails\n\nKeep this as-is.\n",
    )
    .expect("seed claude");

    init::run(temp.path()).expect("init succeeds");

    let tracked = fs::read_to_string(temp.path().join("CLAUDE.md")).expect("read tracked claude");
    assert_eq!(tracked, "# Tracked guardrails\n\nKeep this as-is.\n");
    assert!(!temp.path().join("AGENTS.md").exists());

    let agents = fs::read_to_string(rendered_agents(temp.path())).expect("read rendered agents");
    let claude = fs::read_to_string(rendered_claude(temp.path())).expect("read rendered claude");
    assert!(agents.contains("# Project state (machine-local"));
    assert!(agents.contains("## Durable decisions"));
    assert!(claude.contains("## Known quirks"));

    assert!(markdown_backup_files(temp.path()).is_empty());
}

#[test]
fn sync_md_renders_db_state_to_rendered_dir() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    command::verify(temp.path(), "build", "cargo build", 0, "cli:user").expect("build command");
    command::verify(temp.path(), "test", "cargo test", 0, "cli:user").expect("test command");
    task::add(
        temp.path(),
        "Implement markdown sync",
        Some("Milestone 3"),
        "cli:user",
    )
    .expect("task add");
    decision::add(
        temp.path(),
        "Rendered markdown lives under .memhub/rendered/",
        "Keep tracked files machine-agnostic.",
        "user",
        "cli:user",
    )
    .expect("decision add");
    fact::add(
        temp.path(),
        "quirk.windows-toolchain",
        "Windows builds require the MSVC toolchain.",
        "user",
        "cli:user",
    )
    .expect("fact add");

    let agents_before =
        fs::read_to_string(rendered_agents(temp.path())).expect("read rendered agents before");

    let result = sync_md::run(temp.path()).expect("sync");
    let agents = fs::read_to_string(rendered_agents(temp.path())).expect("read rendered agents");

    assert_eq!(result.updated_files.len(), 2);
    assert_eq!(result.backup_files.len(), 2);
    assert!(agents.contains("**Build:** `cargo build`"));
    assert!(agents.contains("**Test:** `cargo test`"));
    assert!(agents.contains("**Active tasks:** 1 open, 0 blocked"));
    assert!(agents.contains("- Rendered markdown lives under .memhub/rendered/"));
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

    fs::remove_file(rendered_agents(temp.path())).expect("remove rendered agents");
    fs::remove_file(rendered_claude(temp.path())).expect("remove rendered claude");

    let result = sync_md::run(temp.path()).expect("sync");

    assert_eq!(result.updated_files.len(), 2);
    assert!(result.backup_files.is_empty());
    assert!(rendered_agents(temp.path()).exists());
    assert!(rendered_claude(temp.path()).exists());
    assert!(markdown_backup_files(temp.path()).is_empty());
}

#[test]
fn sync_md_never_touches_tracked_repo_root_files() {
    let temp = tempdir().expect("tempdir");
    fs::write(
        temp.path().join("AGENTS.md"),
        "# Tracked AGENTS\n\nHand-authored.\n",
    )
    .expect("seed agents");
    fs::write(
        temp.path().join("CLAUDE.md"),
        "# Tracked CLAUDE\n\nHand-authored.\n",
    )
    .expect("seed claude");

    init::run(temp.path()).expect("init succeeds");
    decision::add(
        temp.path(),
        "Tracked files stay tracked",
        "Machine-local content goes to .memhub/rendered/ only.",
        "user",
        "cli:user",
    )
    .expect("decision add");
    sync_md::run(temp.path()).expect("sync");

    assert_eq!(
        fs::read_to_string(temp.path().join("AGENTS.md")).expect("read tracked agents"),
        "# Tracked AGENTS\n\nHand-authored.\n"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("CLAUDE.md")).expect("read tracked claude"),
        "# Tracked CLAUDE\n\nHand-authored.\n"
    );
}

#[test]
fn auto_sync_md_updates_rendered_markdown_after_writes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.auto_sync_md = true;
    config.save(&config_path).expect("save config");

    decision::add(
        temp.path(),
        "Auto-sync writes the rendered copy",
        "Keep machine-local markdown fresh without manual sync.",
        "user",
        "cli:user",
    )
    .expect("decision add");

    let agents =
        fs::read_to_string(rendered_agents(temp.path())).expect("read rendered agents");
    assert!(agents.contains("- Auto-sync writes the rendered copy"));
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
