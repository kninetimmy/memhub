//! Regression: project discovery must never mistake the machine-global
//! store dir (`~/.memhub`) for a per-repo project.
//!
//! Before the fix, running memhub from a cwd that is *not* inside any
//! repo walked discovery up to `$HOME`, found `~/.memhub` (the global
//! store, same `.memhub` dirname), and returned it as a project.
//! `open_project` then raised `MissingDatabase` whose suggested remedy
//! ("remove ~/.memhub to start over") would delete the machine-global
//! store. Discovery must instead fall through to the safe
//! `NotInitialized` error.
//!
//! Single test / own `HOME` override so the env mutation cannot race
//! other tests (separate integration-test binaries are separate
//! processes).
//!
//! Unix-only. The test isolates discovery by pointing `HOME` at a
//! throwaway `tempfile::tempdir()` and walking up from a non-repo dir
//! nested under it. That hinges on the temp dir living *outside* the
//! real home: on Unix it lands in `/tmp`, so the upward walk never
//! crosses the real `~/.memhub`. On Windows `tempfile` roots under
//! `%USERPROFILE%\AppData\Local\Temp`, i.e. *inside* the real profile,
//! so the walk climbs straight through the real `~/.memhub` and
//! adopts it — defeating the isolation regardless of any `HOME` /
//! `USERPROFILE` override (discovery correctly skips the test's
//! global store, then keeps climbing). There is no reliably-writable
//! temp root outside the profile on Windows without elevation, so the
//! discovery-guard logic — which is itself platform-agnostic — is
//! exercised on Unix only.
#![cfg(unix)]

use std::fs;

use memhub::MemhubError;
use memhub::commands::init;
use memhub::db::{discover_paths, open_global, open_project};

#[test]
fn global_store_dir_is_never_discovered_as_a_repo_project() {
    let home = tempfile::tempdir().expect("home tempdir");
    // SAFETY: single-test binary; no other thread reads HOME concurrently.
    // Unix-gated (see module header), so HOME is the only home-resolution
    // input `db::home_dir` consults here.
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    // Create the machine-global store at `$HOME/.memhub/global.sqlite`.
    open_global().expect("create global store");
    assert!(
        home.path().join(".memhub").join("global.sqlite").exists(),
        "global store must exist for the repro"
    );

    // A directory that is NOT a memhub repo, nested under $HOME so
    // discovery walking up reaches `~/.memhub` (mirrors the real repro:
    // `cd ~/Desktop/'Coding Style Guides' && memhub doc add ... --global`).
    let outside = home.path().join("Desktop").join("Coding Style Guides");
    fs::create_dir_all(&outside).expect("create non-repo dir");

    // discover_paths must NOT return the global-store dir.
    match discover_paths(&outside) {
        Err(MemhubError::NotInitialized { .. }) => {}
        other => panic!(
            "expected NotInitialized for a non-repo cwd; got {other:?} \
             (global-store dir leaked into project discovery)"
        ),
    }

    // open_project must surface the safe NotInitialized error, never
    // MissingDatabase pointing at the global-store dir (whose remedy
    // would delete `~/.memhub`).
    match open_project(&outside) {
        Err(MemhubError::NotInitialized { .. }) => {}
        Err(MemhubError::MissingDatabase { memhub_dir, .. }) => panic!(
            "DANGEROUS: open_project returned MissingDatabase for \
             memhub_dir={memhub_dir:?}; its 'remove the dir' remedy \
             would delete the machine-global store"
        ),
        Err(other) => panic!("expected NotInitialized; got {other:?}"),
        Ok(_) => panic!("expected NotInitialized; a non-repo cwd must not open a project"),
    }

    // Sanity: the guard must not break normal discovery. A real repo
    // initialized *under* $HOME still resolves even though `~/.memhub`
    // (global store) sits between it and the filesystem root.
    let real_repo = home.path().join("work").join("proj");
    fs::create_dir_all(&real_repo).expect("create repo dir");
    init::run(&real_repo).expect("init real repo");
    let paths = discover_paths(&real_repo).expect("real repo must still discover");
    assert_eq!(
        paths.repo_root, real_repo,
        "discovery must resolve the real repo, not the global-store dir"
    );
    open_project(&real_repo).expect("real repo must open");
}
