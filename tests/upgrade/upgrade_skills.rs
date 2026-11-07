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

use std::path::{Path, PathBuf};
use std::process::Command;

use memhub::commands::upgrade::{SkillSync, SkillSyncStatus, sync_skills};
use tempfile::tempdir;

fn find(reports: &[SkillSync], agent: &str) -> SkillSync {
    reports
        .iter()
        .find(|r| r.agent == agent)
        .unwrap_or_else(|| panic!("no report for agent {agent}"))
        .clone()
}

fn active_template(path: &Path) -> bool {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    match name {
        "metrics" => cfg!(feature = "metrics"),
        _ => true,
    }
}

fn count_claude_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("skills").join("claude"))
        .expect("claude templates dir")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .filter(|e| active_template(&e.path()))
        .count()
}

fn count_codex_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("skills").join("codex"))
        .expect("codex templates dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| active_template(&e.path()))
        .count()
}

fn count_opencode_skill_templates(repo: &Path) -> usize {
    ["opencode", "opencode-hibernated"]
        .into_iter()
        .map(|dir| {
            std::fs::read_dir(repo.join("templates").join("skills").join(dir))
                .expect("opencode skill templates dir")
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter(|e| active_template(&e.path()))
                .count()
        })
        .sum()
}

fn count_opencode_command_templates(repo: &Path) -> usize {
    std::fs::read_dir(repo.join("templates").join("commands").join("opencode"))
        .expect("opencode command templates dir")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
        .filter(|e| active_template(&e.path()))
        .count()
}

fn installed_wrapper_paths(home: &Path, name: &str) -> [PathBuf; 5] {
    [
        home.join(".claude")
            .join("commands")
            .join(format!("{name}.md")),
        home.join(".codex")
            .join("skills")
            .join(name)
            .join("SKILL.md"),
        home.join(".config")
            .join("opencode")
            .join("skills")
            .join(name)
            .join("SKILL.md"),
        home.join(".config")
            .join("opencode")
            .join("commands")
            .join(format!("{name}.md")),
        home.join(".gemini")
            .join("config")
            .join("skills")
            .join(name)
            .join("SKILL.md"),
    ]
}

fn report_contains_path(orphans: &[String], home: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(home).expect("wrapper under home");
    orphans
        .iter()
        .any(|orphan| orphan.ends_with(&*relative.to_string_lossy()))
}

#[test]
fn antigravity_resync_requires_its_host_and_preserves_ownership() {
    let _env_guard = crate::support::env_lock();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let expected = std::fs::read_dir(repo.join("templates/skills/antigravity"))
        .unwrap()
        .flatten()
        .filter(|entry| active_template(&entry.path()))
        .count();
    let home = tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }
    let gemini = home.path().join(".gemini");
    let setup = gemini.join("antigravity-cli");
    let target = gemini.join("config/skills");
    let manifest_path = home.path().join(".memhub/install-manifest.json");
    for dry in [true, false] {
        assert_eq!(
            find(&sync_skills(repo, dry).agents, "antigravity").status,
            SkillSyncStatus::Skipped
        );
        assert!(!gemini.exists());
    }
    std::fs::create_dir(&gemini).unwrap();
    assert_eq!(
        find(&sync_skills(repo, false).agents, "antigravity").status,
        SkillSyncStatus::Skipped
    );
    assert!(
        !target.exists(),
        "Gemini CLI alone must not enable Antigravity installs"
    );
    std::fs::write(&setup, b"not a directory").unwrap();
    let report = find(&sync_skills(repo, false).agents, "antigravity");
    assert_eq!(report.status, SkillSyncStatus::Skipped);
    assert!(report.detail.unwrap().contains("not a directory"));
    assert_eq!(std::fs::read(&setup).unwrap(), b"not a directory");
    std::fs::remove_file(&setup).unwrap();
    std::fs::create_dir(&setup).unwrap();
    let report = find(&sync_skills(repo, true).agents, "antigravity");
    assert_eq!(report.status, SkillSyncStatus::Synced);
    assert_eq!(report.synced, expected);
    assert!(!target.exists());
    assert!(!manifest_path.exists());

    let report = find(&sync_skills(repo, false).agents, "antigravity");
    assert_eq!(report.status, SkillSyncStatus::Synced, "{report:?}");
    assert_eq!(report.synced, expected);
    let recall = target.join("recall/SKILL.md");
    let installed = std::fs::read(&recall).unwrap();
    assert_eq!(
        installed,
        std::fs::read(repo.join("templates/skills/antigravity/recall/SKILL.md")).unwrap()
    );
    assert!(manifest_path.is_file());
    let before = std::fs::read(&manifest_path).unwrap();
    for dry in [true, false] {
        assert_eq!(
            find(&sync_skills(repo, dry).agents, "antigravity").synced,
            expected
        );
        assert_eq!(std::fs::read(&recall).unwrap(), installed);
        assert_eq!(std::fs::read(&manifest_path).unwrap(), before);
    }
    std::fs::write(&recall, b"custom recall").unwrap();
    let unowned = target.join("wrap-up/SKILL.md");
    std::fs::write(&unowned, b"unowned wrap-up").unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]
        .as_object_mut()
        .unwrap()
        .remove(unowned.to_string_lossy().as_ref());
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let unowned_before = std::fs::read(&unowned).unwrap();
    let orphan = target.join("retired/SKILL.md");
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"keep retired wrapper").unwrap();
    let mut manifest = memhub::commands::install_manifest::InstallManifest::load();
    manifest.record(orphan.to_string_lossy().to_string(), "old hash".to_string());
    manifest.save().unwrap();
    for dry in [true, false] {
        let report = sync_skills(repo, dry);
        assert_eq!(find(&report.agents, "antigravity").protected, 2);
        assert_eq!(std::fs::read(&recall).unwrap(), b"custom recall");
        assert_eq!(std::fs::read(&unowned).unwrap(), unowned_before);
        assert!(report_contains_path(&report.orphans, home.path(), &orphan));
        assert_eq!(std::fs::read(&orphan).unwrap(), b"keep retired wrapper");
    }
    assert!(!home.path().join(".codex").exists());

    let malformed = tempdir().unwrap();
    std::fs::create_dir_all(malformed.path().join(".gemini/antigravity-cli")).unwrap();
    let config = malformed.path().join(".gemini/config");
    std::fs::write(&config, b"not a directory").unwrap();
    unsafe { std::env::set_var("HOME", malformed.path()) };
    for dry in [true, false] {
        let report = find(&sync_skills(repo, dry).agents, "antigravity");
        assert_eq!(report.status, SkillSyncStatus::Warn, "{report:?}");
        assert_eq!(std::fs::read(&config).unwrap(), b"not a directory");
    }
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    let target_file = config.join("skills");
    std::fs::write(&target_file, b"not a directory").unwrap();
    for dry in [true, false] {
        let report = find(&sync_skills(repo, dry).agents, "antigravity");
        assert_eq!(report.status, SkillSyncStatus::Skipped, "{report:?}");
        assert!(report.detail.unwrap().contains("not a directory"));
        assert_eq!(std::fs::read(&target_file).unwrap(), b"not a directory");
    }
    unsafe { std::env::remove_var("HOME") };
}

#[cfg(unix)]
#[test]
fn antigravity_resync_reports_an_unreadable_target() {
    use std::os::unix::fs::PermissionsExt;
    let _env_guard = crate::support::env_lock();
    let home = tempdir().unwrap();
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }
    std::fs::create_dir_all(home.path().join(".gemini/antigravity-cli")).unwrap();
    let target = home.path().join(".gemini/config/skills");
    std::fs::create_dir_all(&target).unwrap();
    let permissions = std::fs::metadata(&target).unwrap().permissions();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read_dir(&target).is_err();
    let reports: Vec<_> = [true, false]
        .into_iter()
        .map(|dry| {
            find(
                &sync_skills(Path::new(env!("CARGO_MANIFEST_DIR")), dry).agents,
                "antigravity",
            )
        })
        .collect();
    std::fs::set_permissions(&target, permissions).unwrap();
    unsafe { std::env::remove_var("HOME") };
    if unreadable {
        for report in reports {
            assert_eq!(report.status, SkillSyncStatus::Warn, "{report:?}");
            assert!(report.detail.unwrap().contains("cannot inspect"));
        }
    }
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
    std::fs::create_dir_all(home.path().join(".gemini/antigravity-cli"))
        .expect("mk Antigravity setup");
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
    // viz has no template in any build, so no viz wrapper is ever installed.
    for (name, enabled) in [("metrics", cfg!(feature = "metrics")), ("viz", false)] {
        for path in installed_wrapper_paths(home.path(), name) {
            assert_eq!(
                path.exists(),
                enabled,
                "{name} wrapper installation must follow the compile-time feature: {}",
                path.display()
            );
        }
    }
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

#[test]
fn hibernated_wrappers_are_reported_without_mutation() {
    let _env_guard = crate::support::env_lock();

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let home = tempdir().expect("home tempdir");
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    let seeded = b"pre-existing wrapper";
    for name in ["metrics", "viz"] {
        for path in installed_wrapper_paths(home.path(), name) {
            std::fs::create_dir_all(path.parent().expect("wrapper parent")).expect("mkdir");
            std::fs::write(path, seeded).expect("seed wrapper");
        }
    }

    for dry in [true, false] {
        let report = sync_skills(repo, dry);
        // A leftover viz wrapper is an orphan in every build.
        for (name, hibernated) in [("metrics", !cfg!(feature = "metrics")), ("viz", true)] {
            for path in installed_wrapper_paths(home.path(), name) {
                assert_eq!(
                    report_contains_path(&report.orphans, home.path(), &path),
                    hibernated,
                    "{name} wrapper report must follow the compile-time feature: {}",
                    path.display()
                );
                assert_eq!(
                    std::fs::read(&path).expect("wrapper survives resync"),
                    seeded,
                    "dry-run and real resync must leave the wrapper unchanged: {}",
                    path.display()
                );
            }
        }
    }

    if !cfg!(feature = "metrics") {
        let output = Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(["upgrade", "--dry-run", "--json", "--no-gc"])
            .current_dir(repo)
            .env("HOME", home.path())
            .env_remove("USERPROFILE")
            .env("CARGO_HOME", home.path().join(".cargo"))
            .env_remove("CARGO_INSTALL_ROOT")
            .env("MEMHUB_LOG", "off")
            .output()
            .expect("run JSON dry-run");
        assert!(
            output.status.success(),
            "JSON dry-run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse dry-run JSON");
        assert!(
            json["resync_orphans"]
                .as_array()
                .is_some_and(|paths| paths.len() >= 8),
            "JSON must report every hibernated wrapper: {json}"
        );
        assert!(
            json["resync_orphan_remediation"]
                .as_str()
                .is_some_and(|text| text.contains("remove the file manually")),
            "JSON must provide safe manual remediation: {json}"
        );

        let output = Command::new(env!("CARGO_BIN_EXE_memhub"))
            .args(["upgrade", "--dry-run", "--no-gc"])
            .current_dir(repo)
            .env("HOME", home.path())
            .env_remove("USERPROFILE")
            .env("CARGO_HOME", home.path().join(".cargo"))
            .env_remove("CARGO_INSTALL_ROOT")
            .env("MEMHUB_LOG", "off")
            .output()
            .expect("run human dry-run");
        assert!(output.status.success(), "human dry-run failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("remove the file manually if unwanted"),
            "human output must provide safe manual remediation: {stdout}"
        );
    }

    unsafe { std::env::remove_var("HOME") };
}
