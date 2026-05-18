//! `memhub upgrade` installed-skill resync (decision 97 / task 50).
//!
//! Guarantees that are load-bearing and tested here:
//!
//! 1. An agent dir that does **not** exist is skipped, never created
//!    (mirrors `upgrade`'s "only act on what exists" posture).
//! 2. A present agent dir is additively synced from the repo's
//!    `templates/skills/`; the copy is real and idempotent.
//! 3. `--dry-run` counts but mutates nothing.
//! 4. A path that exists but is not a directory is skipped, not
//!    clobbered.
//!
//! All assertions live in ONE test so the `HOME` override cannot race
//! other tests in this binary.

use std::path::Path;

use memhub::commands::upgrade::{SkillSync, SkillSyncStatus, sync_skills};
use tempfile::tempdir;

fn find(reports: &[SkillSync], agent: &str) -> SkillSync {
    reports
        .iter()
        .find(|r| r.agent == agent)
        .unwrap_or_else(|| panic!("no report for agent {agent}"))
        .clone()
}

fn count_claude_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("skills").join("claude"))
        .expect("claude templates dir")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .count()
}

fn count_codex_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("skills").join("codex"))
        .expect("codex templates dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .count()
}

#[test]
fn skill_resync_additive_idempotent_and_conservative() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expect_claude = count_claude_templates(repo);
    let expect_codex = count_codex_templates(repo);
    assert!(
        expect_claude > 0 && expect_codex > 0,
        "fixture sanity: the source repo must ship skill templates"
    );

    let home = tempdir().expect("home tempdir");
    // SAFETY: single-test binary; no other thread reads HOME concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    // --- 1. Neither agent dir exists => skipped, NOT created ----------
    let reports = sync_skills(repo, false);
    let claude = find(&reports, "claude");
    let codex = find(&reports, "codex");
    assert_eq!(claude.status, SkillSyncStatus::Skipped);
    assert_eq!(codex.status, SkillSyncStatus::Skipped);
    assert_eq!(claude.synced, 0);
    assert!(
        !home.path().join(".claude").join("commands").exists(),
        "absent Claude dir must NOT be created by a resync"
    );
    assert!(
        !home.path().join(".codex").join("skills").exists(),
        "absent Codex dir must NOT be created by a resync"
    );

    // --- 2. Create the agent dirs => additive real copy --------------
    let claude_dir = home.path().join(".claude").join("commands");
    let codex_dir = home.path().join(".codex").join("skills");
    std::fs::create_dir_all(&claude_dir).expect("mk claude dir");
    std::fs::create_dir_all(&codex_dir).expect("mk codex dir");
    // A user's own skill in the shared dir must survive (additive only).
    std::fs::write(claude_dir.join("user-own.md"), b"keep me").expect("seed user skill");

    let reports = sync_skills(repo, false);
    let claude = find(&reports, "claude");
    let codex = find(&reports, "codex");
    assert_eq!(claude.status, SkillSyncStatus::Synced, "{claude:?}");
    assert_eq!(codex.status, SkillSyncStatus::Synced, "{codex:?}");
    assert_eq!(claude.synced, expect_claude);
    assert_eq!(codex.synced, expect_codex);
    // The copy is real, not just a count.
    assert!(
        claude_dir.join("recall.md").is_file(),
        "a known Claude skill template must land on disk"
    );
    assert!(
        codex_dir.join("recall").join("SKILL.md").is_file(),
        "a known Codex skill dir (with SKILL.md) must land on disk"
    );
    // Additive: the user's unrelated skill is untouched.
    assert_eq!(
        std::fs::read(claude_dir.join("user-own.md")).expect("user skill"),
        b"keep me",
        "additive resync must not disturb a user's own skill"
    );

    // --- 3. Idempotent re-run: same result, no error -----------------
    let reports = sync_skills(repo, false);
    assert_eq!(find(&reports, "claude").status, SkillSyncStatus::Synced);
    assert_eq!(find(&reports, "claude").synced, expect_claude);
    assert_eq!(find(&reports, "codex").synced, expect_codex);

    // --- 4. Dry-run counts but mutates nothing -----------------------
    let probe = claude_dir.join("__dry_probe__.md");
    assert!(!probe.exists());
    let before = std::fs::read_dir(&claude_dir).unwrap().count();
    let reports = sync_skills(repo, true);
    assert_eq!(find(&reports, "claude").status, SkillSyncStatus::Synced);
    assert_eq!(find(&reports, "claude").synced, expect_claude);
    assert_eq!(
        std::fs::read_dir(&claude_dir).unwrap().count(),
        before,
        "dry-run must not add or remove any installed skill"
    );

    // --- 5. Path exists but is not a directory => skipped ------------
    let other = tempdir().expect("home2");
    unsafe { std::env::set_var("HOME", other.path()) };
    std::fs::create_dir_all(other.path().join(".codex")).expect("mk .codex");
    std::fs::write(other.path().join(".codex").join("skills"), b"not a dir")
        .expect("file where dir expected");
    let reports = sync_skills(repo, false);
    let codex = find(&reports, "codex");
    assert_eq!(codex.status, SkillSyncStatus::Skipped);
    assert!(
        codex.detail.unwrap_or_default().contains("not a directory"),
        "a non-dir at the agent path must be reported, not clobbered"
    );
    assert_eq!(
        std::fs::read(other.path().join(".codex").join("skills")).unwrap(),
        b"not a dir",
        "the offending file must be left exactly as-is"
    );

    unsafe { std::env::remove_var("HOME") };
}
