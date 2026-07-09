//! U7: `memhub upgrade` degrades instead of aborting on a corrupt registry.
//!
//! Overrides `HOME` to redirect `~/.memhub/global.sqlite` into a tempdir,
//! matching `upgrade_registry.rs`'s discipline (both take
//! `support::env_lock()` for the whole test — see `upgrade/support.rs`,
//! Wave 5 U4 issue #90 — to stay isolated from sibling tests in this
//! shared harness binary). A corrupt registry read used to propagate via
//! `?` and abort the whole `--finish` phase *after* the new binary was
//! already installed. It must now degrade to a warning and a
//! source-repo-only continuation.

use memhub::commands::upgrade::known_projects_or_warn;
use tempfile::tempdir;

#[test]
fn corrupt_registry_degrades_to_source_repo_only_plus_warning() {
    // Held for the whole test (see module header and `upgrade/support.rs`).
    let _env_guard = crate::support::env_lock();

    let home = tempdir().expect("home");
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
    }

    // No global store yet => a clean, empty enumeration, and crucially
    // NOT a warning (absence is normal, not a degrade).
    let mut warnings: Vec<String> = Vec::new();
    let known = known_projects_or_warn(&mut warnings);
    assert!(known.is_empty(), "no store => no known roots");
    assert!(
        warnings.is_empty(),
        "absence of a global store is normal, not a warning: {warnings:?}"
    );

    // Deliberately corrupt the registry. The global store is a SQLite
    // file; garbage bytes make every read of it fail.
    let global = home.path().join(".memhub").join("global.sqlite");
    std::fs::create_dir_all(global.parent().unwrap()).expect("mk .memhub");
    std::fs::write(&global, b"this is not a sqlite database, at all").expect("corrupt store");

    // The upgrade must NOT abort: it degrades to an empty enumeration plus
    // a warning, so the source repo (and any --also roots) still upgrade.
    let mut warnings: Vec<String> = Vec::new();
    let known = known_projects_or_warn(&mut warnings);
    assert!(
        known.is_empty(),
        "a corrupt registry contributes no roots (source-repo-only continuation)"
    );
    assert_eq!(warnings.len(), 1, "exactly one degrade warning: {warnings:?}");
    assert!(
        warnings[0].contains("registry") && warnings[0].to_lowercase().contains("unreadable"),
        "the warning must name the corrupt registry: {}",
        warnings[0]
    );

    unsafe {
        std::env::remove_var("HOME");
    }
}
