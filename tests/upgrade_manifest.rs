//! Install-manifest resync honesty (Wave 5 / U6, decision Q15).
//!
//! End-to-end over `sync_skills` against a **fabricated** source repo, so
//! the template set is entirely under the test's control. All assertions
//! live in ONE test because it overrides `HOME` (process-global) — the same
//! single-test-binary discipline as `upgrade_skills.rs` /
//! `upgrade_registry.rs`.
//!
//! Only the Claude template dir is populated: every other agent's target
//! dir is absent, so those agents skip before their source is ever read.

use std::path::Path;

use memhub::commands::upgrade::{SkillSync, sync_skills};
use tempfile::tempdir;

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, contents).expect("write");
}

fn claude_report(repo: &Path) -> SkillSync {
    sync_skills(repo, false)
        .agents
        .into_iter()
        .find(|a| a.agent == "claude")
        .expect("claude agent report")
}

#[test]
fn resync_never_clobbers_user_files_and_reports_orphans() {
    let home = tempdir().expect("home");
    // SAFETY: single-test binary; no other thread reads HOME concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    let repo = tempdir().expect("repo");
    let claude_tpl = repo.path().join("templates").join("skills").join("claude");
    write(&claude_tpl.join("foo.md"), b"memhub foo v1");
    write(&claude_tpl.join("bar.md"), b"memhub bar v1");

    // memhub only syncs into a dir the user has already set up.
    let commands = home.path().join(".claude").join("commands");
    std::fs::create_dir_all(&commands).expect("mk commands");

    // --- Run 1: fresh install ---------------------------------------
    let r1 = claude_report(repo.path());
    assert_eq!(r1.synced, 2, "both templates install: {r1:?}");
    assert_eq!(r1.protected, 0);
    assert_eq!(
        std::fs::read(commands.join("foo.md")).unwrap(),
        b"memhub foo v1"
    );
    assert_eq!(
        std::fs::read(commands.join("bar.md")).unwrap(),
        b"memhub bar v1"
    );

    // The manifest is written and records what memhub installed.
    let manifest = home.path().join(".memhub").join("install-manifest.json");
    let manifest_body = std::fs::read_to_string(&manifest).expect("manifest written");
    assert!(
        manifest_body.contains("foo.md") && manifest_body.contains("bar.md"),
        "manifest must record the files memhub wrote: {manifest_body}"
    );

    // --- Run 2: a user's own, same-named file must NOT be clobbered ---
    // The user hand-authors baz.md; memhub then starts shipping a
    // *different* baz.md. memhub never wrote the user's file, so it must
    // leave it exactly as-is.
    write(&commands.join("baz.md"), b"the user's own baz");
    write(&claude_tpl.join("baz.md"), b"memhub baz (different)");
    let r2 = claude_report(repo.path());
    assert_eq!(
        std::fs::read(commands.join("baz.md")).unwrap(),
        b"the user's own baz",
        "a user-authored, same-named file must survive resync untouched"
    );
    assert!(
        r2.protected >= 1,
        "the user's file is reported as protected: {r2:?}"
    );
    assert!(
        r2.synced >= 2,
        "memhub's own files (foo, bar) still sync: {r2:?}"
    );

    // --- Run 3: an orphan is reported but NEVER deleted -------------
    std::fs::remove_file(claude_tpl.join("foo.md")).expect("drop template");
    let full = sync_skills(repo.path(), false);
    assert!(
        full.orphans.iter().any(|o| o.contains("foo.md")),
        "a no-longer-shipped file memhub wrote must be reported as an orphan: {:?}",
        full.orphans
    );
    assert!(
        commands.join("foo.md").exists(),
        "an orphan must be reported, NEVER deleted"
    );
    assert_eq!(
        std::fs::read(commands.join("foo.md")).unwrap(),
        b"memhub foo v1",
        "the orphaned file's contents are left exactly as-is"
    );

    // --- Run 4: a corrupt manifest fails safe -----------------------
    // Corrupt the manifest, then ship a NEW bar.md. With no trustworthy
    // ownership record, memhub must NOT overwrite the existing bar.md —
    // an unknown file is treated as the user's.
    std::fs::write(&manifest, b"}}} not json").expect("corrupt manifest");
    write(&claude_tpl.join("bar.md"), b"memhub bar v2 (new)");
    let r4 = claude_report(repo.path());
    assert_eq!(
        std::fs::read(commands.join("bar.md")).unwrap(),
        b"memhub bar v1",
        "a corrupt manifest must fail safe: never overwrite an existing file"
    );
    assert!(
        r4.protected >= 1,
        "with a corrupt manifest, pre-existing files read as user-owned: {r4:?}"
    );

    unsafe {
        std::env::remove_var("HOME");
    }
}
