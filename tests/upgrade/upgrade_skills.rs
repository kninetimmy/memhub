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
//! All assertions live in ONE test so the `HOME` override stays in one
//! place. It takes `support::env_lock()` for the whole test — see
//! `upgrade/support.rs` (Wave 5 U4, issue #90) — to stay isolated from
//! sibling tests in this shared harness binary.

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

fn count_opencode_skill_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("skills").join("opencode"))
        .expect("opencode skill templates dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .count()
}

fn count_opencode_command_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("commands").join("opencode"))
        .expect("opencode command templates dir")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .count()
}

#[test]
fn skill_resync_additive_idempotent_and_conservative() {
    // Held for the whole test, across both HOME overrides below (see module
    // header and `upgrade/support.rs`).
    let _env_guard = crate::support::env_lock();

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expect_claude = count_claude_templates(repo);
    let expect_codex = count_codex_templates(repo);
    let expect_opencode_skills = count_opencode_skill_templates(repo);
    let expect_opencode_commands = count_opencode_command_templates(repo);
    assert!(
        expect_claude > 0
            && expect_codex > 0
            && expect_opencode_skills > 0
            && expect_opencode_commands > 0,
        "fixture sanity: the source repo must ship skill templates"
    );

    let home = tempdir().expect("home tempdir");
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    // --- 1. Neither agent dir exists => skipped, NOT created ----------
    let reports = sync_skills(repo, false).agents;
    let claude = find(&reports, "claude");
    let codex = find(&reports, "codex");
    let opencode_skills = find(&reports, "opencode-skills");
    let opencode_commands = find(&reports, "opencode-commands");
    assert_eq!(claude.status, SkillSyncStatus::Skipped);
    assert_eq!(codex.status, SkillSyncStatus::Skipped);
    assert_eq!(opencode_skills.status, SkillSyncStatus::Skipped);
    assert_eq!(opencode_commands.status, SkillSyncStatus::Skipped);
    assert_eq!(claude.synced, 0);
    assert!(
        !home.path().join(".claude").join("commands").exists(),
        "absent Claude dir must NOT be created by a resync"
    );
    assert!(
        !home.path().join(".codex").join("skills").exists(),
        "absent Codex dir must NOT be created by a resync"
    );
    assert!(
        !home
            .path()
            .join(".config")
            .join("opencode")
            .join("skills")
            .exists(),
        "absent OpenCode skills dir must NOT be created by a resync"
    );
    assert!(
        !home
            .path()
            .join(".config")
            .join("opencode")
            .join("commands")
            .exists(),
        "absent OpenCode commands dir must NOT be created by a resync"
    );

    // --- 2. Create the agent dirs => additive real copy --------------
    let claude_dir = home.path().join(".claude").join("commands");
    let codex_dir = home.path().join(".codex").join("skills");
    let opencode_skills_dir = home.path().join(".config").join("opencode").join("skills");
    let opencode_commands_dir = home
        .path()
        .join(".config")
        .join("opencode")
        .join("commands");
    std::fs::create_dir_all(&claude_dir).expect("mk claude dir");
    std::fs::create_dir_all(&codex_dir).expect("mk codex dir");
    std::fs::create_dir_all(&opencode_skills_dir).expect("mk opencode skills dir");
    std::fs::create_dir_all(&opencode_commands_dir).expect("mk opencode commands dir");
    // A user's own skill in the shared dir must survive (additive only).
    std::fs::write(claude_dir.join("user-own.md"), b"keep me").expect("seed user skill");

    let reports = sync_skills(repo, false).agents;
    let claude = find(&reports, "claude");
    let codex = find(&reports, "codex");
    let opencode_skills = find(&reports, "opencode-skills");
    let opencode_commands = find(&reports, "opencode-commands");
    assert_eq!(claude.status, SkillSyncStatus::Synced, "{claude:?}");
    assert_eq!(codex.status, SkillSyncStatus::Synced, "{codex:?}");
    assert_eq!(
        opencode_skills.status,
        SkillSyncStatus::Synced,
        "{opencode_skills:?}"
    );
    assert_eq!(
        opencode_commands.status,
        SkillSyncStatus::Synced,
        "{opencode_commands:?}"
    );
    assert_eq!(claude.synced, expect_claude);
    assert_eq!(codex.synced, expect_codex);
    assert_eq!(opencode_skills.synced, expect_opencode_skills);
    assert_eq!(opencode_commands.synced, expect_opencode_commands);
    // The copy is real, not just a count.
    assert!(
        claude_dir.join("recall.md").is_file(),
        "a known Claude skill template must land on disk"
    );
    assert!(
        codex_dir.join("recall").join("SKILL.md").is_file(),
        "a known Codex skill dir (with SKILL.md) must land on disk"
    );
    assert!(
        opencode_skills_dir
            .join("recall")
            .join("SKILL.md")
            .is_file(),
        "a known OpenCode skill dir (with SKILL.md) must land on disk"
    );
    assert!(
        opencode_commands_dir.join("recall.md").is_file(),
        "a known OpenCode command wrapper must land on disk"
    );
    // Additive: the user's unrelated skill is untouched.
    assert_eq!(
        std::fs::read(claude_dir.join("user-own.md")).expect("user skill"),
        b"keep me",
        "additive resync must not disturb a user's own skill"
    );

    // --- 3. Idempotent re-run: same result, no error -----------------
    let reports = sync_skills(repo, false).agents;
    assert_eq!(find(&reports, "claude").status, SkillSyncStatus::Synced);
    assert_eq!(find(&reports, "claude").synced, expect_claude);
    assert_eq!(find(&reports, "codex").synced, expect_codex);
    assert_eq!(
        find(&reports, "opencode-skills").synced,
        expect_opencode_skills
    );
    assert_eq!(
        find(&reports, "opencode-commands").synced,
        expect_opencode_commands
    );

    // --- 4. Dry-run counts but mutates nothing -----------------------
    let probe = claude_dir.join("__dry_probe__.md");
    assert!(!probe.exists());
    let before = std::fs::read_dir(&claude_dir).unwrap().count();
    let reports = sync_skills(repo, true).agents;
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
    let reports = sync_skills(repo, false).agents;
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
