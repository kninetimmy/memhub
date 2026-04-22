use std::fs;

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
}

#[test]
fn sync_md_renders_db_state_into_managed_block() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

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
    assert!(agents.contains("**Build:** `cargo build`"));
    assert!(agents.contains("**Test:** `cargo test`"));
    assert!(agents.contains("**Active tasks:** 1 open, 0 blocked"));
    assert!(agents.contains("- Managed block lives at the bottom"));
    assert!(
        agents.contains("- quirk.windows-toolchain: Windows builds require the MSVC toolchain.")
    );
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
