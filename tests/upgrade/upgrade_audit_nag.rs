//! `memhub upgrade`'s best-effort audit-md nag (Wave 2 C7, issue #33).
//!
//! Two guarantees are load-bearing and tested here:
//!
//! 1. The nag line appears when `memhub audit md` finds something — a
//!    drifted `AGENTS.md` (the same fixture shape
//!    `tests/audit_md.rs::drifted_agents_md_is_a_finding` uses)
//!    produces `AuditNagStatus::Findings` and a human-readable nag
//!    line; a clean repo produces neither (issue #33: "prints a single
//!    nag line when findings exist", not on every run — unlike the
//!    always-printed skills/gc lines).
//! 2. The audit itself failing to even run (e.g. `.memhub` not
//!    discoverable from `cwd`) degrades to `AuditNagStatus::Warn`
//!    rather than propagating an error. `check_audit_md` returns a
//!    plain `AuditNag`, never a `Result`, so there is no `?` for an
//!    upgrade caller to trip over — this is the structural guarantee
//!    that an audit failure can never fail the upgrade path.
//!
//! Neither test overrides `HOME` itself (unlike `upgrade_registry.rs` /
//! `upgrade_skills.rs`) and `check_audit_md` is read-only and never opens
//! a DB — but it does call `commands::audit_md::run`, which calls
//! `db::discover_paths` (to locate the repo root), which unconditionally
//! resolves `db::home_dir()` to skip the machine-global store dir. Both
//! tests therefore still take `support::env_lock()` for the whole test —
//! see `upgrade/support.rs` (Wave 5 U4, issue #90) — so a sibling test's
//! `HOME` override in this shared harness binary cannot race that read.
//! (The first test's `discover_paths` call short-circuits at its own
//! freshly-`init`'d dir before the resolved value is ever consulted, so
//! only the second test is actually outcome-sensitive to the race — both
//! take the lock anyway rather than leave that invariant resting on
//! "this test happens to init first".)

use std::fs;

use memhub::agents_md::generate_agents_md;
use memhub::commands::init;
use memhub::commands::upgrade::{AuditNagStatus, check_audit_md};
use tempfile::tempdir;

/// Satisfies every `audit md` check at once (size, managed block,
/// keystones) so only the thing each test mutates produces a finding —
/// same fixture `tests/audit_md.rs::clean_claude_md` uses.
const CLEAN_CLAUDE_MD: &str = "# memhub\n\n\
    Local-first. Agents are untrusted writers.\n\n\
    <!-- memhub:managed-block v=1 -->\n\
    memhub-primary: true\n\
    db: .memhub/project.sqlite\n\
    rendered: .memhub/rendered/\n\
    config: .memhub/config.toml\n\
    <!-- /memhub:managed-block -->\n\n\
    ## Session Continuity\n\n\
    stale_embeddings gate. sync_adopt gate.\n";

#[test]
fn nag_line_appears_on_a_drifted_fixture_and_is_silent_when_clean() {
    // Held for the whole test (see module header and `upgrade/support.rs`).
    let _env_guard = crate::support::env_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // --- clean repo: status Clean, no nag line in either mode --------
    let generated = generate_agents_md(CLEAN_CLAUDE_MD);
    fs::write(temp.path().join("CLAUDE.md"), CLEAN_CLAUDE_MD).expect("write CLAUDE.md");
    fs::write(temp.path().join("AGENTS.md"), &generated).expect("write AGENTS.md");

    let clean = check_audit_md(temp.path());
    assert_eq!(clean.status, AuditNagStatus::Clean, "{clean:?}");
    assert_eq!(clean.count, 0);
    assert!(
        clean.nag_line(false).is_none(),
        "a clean repo must not print a nag line: {clean:?}"
    );
    assert!(
        clean.nag_line(true).is_none(),
        "a clean repo must not print a would-nag line either: {clean:?}"
    );

    // --- drifted fixture: AGENTS.md no longer matches the generator --
    fs::write(
        temp.path().join("AGENTS.md"),
        "stale, hand-edited content\n",
    )
    .expect("write stale AGENTS.md");

    let drifted = check_audit_md(temp.path());
    assert_eq!(drifted.status, AuditNagStatus::Findings, "{drifted:?}");
    assert!(drifted.count >= 1, "{drifted:?}");

    let line = drifted
        .nag_line(false)
        .expect("a drifted repo must produce a nag line");
    assert!(line.contains("finding"), "nag line: {line:?}");
    assert!(
        line.contains("audit md") || line.contains("audit-md"),
        "nag line should point at the audit command or skill: {line:?}"
    );

    let dry_line = drifted
        .nag_line(true)
        .expect("dry-run must report it would nag");
    assert!(
        dry_line.contains("would"),
        "dry-run nag line should read as a preview: {dry_line:?}"
    );
}

#[test]
fn audit_error_degrades_to_a_warn_row_not_a_failure() {
    // Held for the whole test (see module header and `upgrade/support.rs`):
    // this is the one that actually walks past its own (nonexistent)
    // project dir up to real ancestors, so it is genuinely outcome-sensitive
    // to a racing `HOME` override.
    let _env_guard = crate::support::env_lock();

    // A tempdir with no `.memhub` anywhere in its ancestry (deliberately
    // no `init::run` here): `audit md`'s own `db::discover_paths` call
    // fails with `NotInitialized`. `check_audit_md` must not propagate
    // that — it surfaces as a `Warn` row instead of a crash or an `Err`
    // an upgrade caller would have to handle.
    let temp = tempdir().expect("tempdir");

    let nag = check_audit_md(temp.path());
    assert_eq!(nag.status, AuditNagStatus::Warn, "{nag:?}");
    assert_eq!(nag.count, 0);
    assert!(
        nag.detail.is_some(),
        "a Warn nag must carry the underlying error as detail: {nag:?}"
    );

    // The degrade-to-warn contract: a caller still gets a one-line,
    // human-readable summary rather than nothing.
    let line = nag
        .nag_line(false)
        .expect("a Warn nag still renders a line");
    assert!(line.starts_with("skipped"), "nag line: {line:?}");

    // The dry-run preview degrades identically — no special-casing that
    // could hide a broken audit specifically in `--dry-run`.
    let dry_line = nag
        .nag_line(true)
        .expect("a Warn nag renders in dry-run too");
    assert!(dry_line.starts_with("skipped"), "dry nag line: {dry_line:?}");
}
