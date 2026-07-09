//! U6 resync robustness (Wave 5, follow-up review REQUIRED 1b + REQUIRED 2).
//!
//! 1. First-run adoption hint: the first resync over an EMPTY manifest that
//!    leaves a pre-existing, unverifiable file untouched sets `first_run_hint`.
//! 2. A transient source-read failure must NOT cede ownership. A still-owned
//!    file's manifest entry is carried forward so a later run can still UPDATE
//!    it — instead of dropping it (which would freeze the file at its old
//!    version forever, silently).
//!
//! Single test binary (overrides HOME), matching the other `upgrade_*` tests.
//! A *second*, successful agent (opencode-commands) is set up on purpose: it
//! makes the rebuilt manifest non-empty so `save()` actually runs — the exact
//! condition under which the pre-fix code would have DROPPED the failed
//! agent's still-owned entries.

use std::path::Path;

use memhub::commands::upgrade::sync_skills;
use tempfile::tempdir;

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, contents).expect("write");
}

#[test]
fn transient_source_failure_preserves_ownership_and_first_run_is_flagged() {
    let home = tempdir().expect("home");
    // SAFETY: single-test binary; no other thread reads HOME concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    let repo = tempdir().expect("repo");
    let claude_tpl = repo.path().join("templates").join("skills").join("claude");
    write(&claude_tpl.join("keep.md"), b"memhub keep v1");
    write(&claude_tpl.join("user.md"), b"memhub user (canonical)");
    // A second agent that will keep succeeding, so the rebuilt manifest is
    // non-empty and `save()` runs even while claude's source is unreadable.
    let oc_tpl = repo.path().join("templates").join("commands").join("opencode");
    write(&oc_tpl.join("oc.md"), b"memhub oc v1");

    let commands = home.path().join(".claude").join("commands");
    std::fs::create_dir_all(&commands).expect("mk claude commands");
    let oc_commands = home.path().join(".config").join("opencode").join("commands");
    std::fs::create_dir_all(&oc_commands).expect("mk oc commands");
    // Pre-existing user file, same name as a template memhub ships.
    write(&commands.join("user.md"), b"the user's own user.md");

    // --- Run A: first run over an EMPTY manifest --------------------
    let a = sync_skills(repo.path(), false);
    assert!(
        a.first_run_hint,
        "first run leaving a pre-existing file untouched must flag the notice"
    );
    let claude = a.agents.iter().find(|x| x.agent == "claude").unwrap();
    assert!(claude.protected >= 1, "{claude:?}");
    assert!(
        claude
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("unverified owner"),
        "protected detail must be surfaced and honest: {claude:?}"
    );
    assert_eq!(
        std::fs::read(commands.join("keep.md")).unwrap(),
        b"memhub keep v1"
    );
    assert_eq!(
        std::fs::read(commands.join("user.md")).unwrap(),
        b"the user's own user.md",
        "the user's file is left untouched"
    );

    // --- Run B: claude's source becomes momentarily unreadable ------
    // Replace templates/skills/claude (a dir) with a FILE so read_dir fails.
    // opencode-commands still succeeds, so the manifest is saved this run.
    std::fs::remove_dir_all(&claude_tpl).expect("rm src dir");
    std::fs::write(&claude_tpl, b"transiently not a directory").expect("clobber src");

    let b = sync_skills(repo.path(), false);
    assert!(
        !b.orphans.iter().any(|o| o.contains("keep.md")),
        "a still-owned file must NOT be orphaned because its source read failed: {:?}",
        b.orphans
    );
    assert_eq!(
        std::fs::read(commands.join("keep.md")).unwrap(),
        b"memhub keep v1",
        "the owned file is untouched during the transient failure"
    );
    let manifest_body =
        std::fs::read_to_string(home.path().join(".memhub").join("install-manifest.json"))
            .expect("manifest still present");
    assert!(
        manifest_body.contains("keep.md"),
        "the owned entry must be carried forward through the failure, not dropped: {manifest_body}"
    );

    // --- Run C: source recovers, with a CHANGED template ------------
    // If ownership had been ceded in run B, keep.md would now read as
    // user-owned and freeze at v1. Because it was carried, the update lands.
    std::fs::remove_file(&claude_tpl).expect("rm clobber file");
    write(&claude_tpl.join("keep.md"), b"memhub keep V2");
    write(&claude_tpl.join("user.md"), b"memhub user (canonical)");

    let _c = sync_skills(repo.path(), false);
    assert_eq!(
        std::fs::read(commands.join("keep.md")).unwrap(),
        b"memhub keep V2",
        "ownership survived the transient failure, so the update lands"
    );
    assert_eq!(
        std::fs::read(commands.join("user.md")).unwrap(),
        b"the user's own user.md",
        "the user's file remains untouched throughout"
    );

    unsafe {
        std::env::remove_var("HOME");
    }
}
