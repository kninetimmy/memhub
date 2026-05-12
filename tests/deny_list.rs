use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::MemhubError;
use memhub::commands::{ingest_git, init, search, status};
use memhub::config::{DenyList, ProjectConfig};
use tempfile::tempdir;

fn init_git_repo(repo_root: &Path) {
    git(repo_root, &["init"]);
    git(repo_root, &["config", "user.name", "Memhub Test"]);
    git(repo_root, &["config", "user.email", "memhub@example.com"]);
}

fn write_file(repo_root: &Path, relative_path: &str, contents: &str) {
    let path = repo_root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, contents).expect("write file");
}

fn git(repo_root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn default_config_has_non_empty_deny_list() {
    let cfg = ProjectConfig::default_for_repo_name("memhub");
    assert!(!cfg.deny_list.patterns.is_empty());
    assert!(cfg.deny_list.patterns.iter().any(|p| p == ".env"));
    assert!(cfg.deny_list.patterns.iter().any(|p| p == "*.pem"));
}

#[test]
fn config_without_deny_list_section_falls_back_to_defaults() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("config.toml");
    fs::write(
        &path,
        "project_name = \"memhub\"\nauto_sync_md = false\nlog_level = \"info\"\n",
    )
    .expect("write config");

    let cfg = ProjectConfig::load(&path).expect("load config");
    assert!(!cfg.deny_list.patterns.is_empty());
    assert_eq!(cfg.deny_list.patterns, DenyList::default().patterns);
}

#[test]
fn ingest_git_skips_denied_paths() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    init::run(temp.path()).expect("init");

    write_file(temp.path(), "src/lib.rs", "pub fn ok() {}\n");
    write_file(temp.path(), ".env", "SECRET=hunter2\n");
    write_file(temp.path(), "secrets/api.key", "abc123\n");
    git(
        temp.path(),
        &["add", "src/lib.rs", ".env", "secrets/api.key"],
    );
    git(temp.path(), &["commit", "-m", "add files"]);

    let summary = ingest_git::run(temp.path(), None).expect("ingest");
    assert_eq!(summary.commits_seen, 1);
    assert_eq!(summary.unique_files_seen, 1);
    assert_eq!(summary.commit_file_links_seen, 1);
    assert_eq!(summary.denied_files_skipped, 2);

    let summary = status::run(temp.path()).expect("status");
    assert_eq!(summary.files, 1);
    assert!(summary.deny_patterns >= 1);

    let response = search::run(temp.path(), "src/lib.rs", 10).expect("search lib");
    assert_eq!(response.results.len(), 1);

    let denied = search::run(temp.path(), ".env", 10).expect("search denied");
    assert!(
        denied.results.is_empty(),
        "denied direct lookup should return no matches"
    );
}

#[test]
fn search_filters_denied_paths_already_in_db() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    init::run(temp.path()).expect("init");

    write_file(temp.path(), "config/server.txt", "ok\n");
    git(temp.path(), &["add", "config/server.txt"]);
    git(temp.path(), &["commit", "-m", "add file"]);
    ingest_git::run(temp.path(), None).expect("ingest");

    let ctx = memhub::db::open_project(temp.path()).expect("open");
    ctx.conn
        .execute(
            "UPDATE files SET path = 'config/server.pem' WHERE path = 'config/server.txt'",
            [],
        )
        .expect("rewrite path to denied");
    drop(ctx);

    let response = search::run(temp.path(), "file:config/server.pem", 10).expect("search");
    assert!(
        response.results.is_empty(),
        "search should filter out denied paths present in DB"
    );
}

#[test]
fn ingest_git_errors_on_invalid_deny_pattern() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    init::run(temp.path()).expect("init");

    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load");
    cfg.deny_list.patterns.push("[".to_string());
    cfg.save(&config_path).expect("save");

    write_file(temp.path(), "src/lib.rs", "pub fn ok() {}\n");
    git(temp.path(), &["add", "src/lib.rs"]);
    git(temp.path(), &["commit", "-m", "add file"]);

    match ingest_git::run(temp.path(), None) {
        Err(MemhubError::InvalidInput(message)) => {
            assert!(
                message.contains("deny-list pattern"),
                "expected message to mention deny-list pattern, got: {message}"
            );
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}
