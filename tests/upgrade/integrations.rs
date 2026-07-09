use std::fs;
use std::path::Path;

use memhub::commands::{init, integrations, status};
use memhub::config::ProjectConfig;
use memhub::db;
use tempfile::tempdir;

// `integrations::enable_k9`, `integrations::disable_k9`, and `status::run`
// all call `db::open_project` as their first fallible step, which calls
// `db::discover_paths`, which resolves `db::home_dir()` unconditionally as
// its first line (Wave 5 U4, issue #90). Every test that calls any of the
// three takes `support::env_read_lock()` for the whole test, guarding
// against a concurrent writer test's `HOME`/`USERPROFILE` override
// elsewhere in this shared harness binary — see `upgrade/support.rs`. The
// tests that only call `init::run` + the local `read_config` helper (which
// bottoms out in `ProjectConfig::load`/`fs::read_to_string` and
// `db::ProjectPaths::for_repo_root`, neither of which touches `home_dir`)
// don't need it.

fn write_k9_marker(repo_root: &Path) {
    let dir = repo_root.join("agent_docs");
    fs::create_dir_all(&dir).expect("create agent_docs");
    fs::write(dir.join("project_state.md"), "# state").expect("write marker");
}

fn write_k9_marker_at(repo_root: &Path, path: &str) {
    let dir = repo_root.join(path);
    fs::create_dir_all(&dir).expect("create custom path");
    fs::write(dir.join("project_state.md"), "# state").expect("write marker");
}

fn read_config(repo_root: &Path) -> ProjectConfig {
    let paths = db::ProjectPaths::for_repo_root(repo_root);
    ProjectConfig::load(&paths.config_path).expect("load config")
}

#[test]
fn fresh_init_without_k9_omits_section() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let config = read_config(temp.path());
    assert!(config.integrations.k9.is_none());
}

#[test]
fn fresh_init_with_k9_writes_enabled_section() {
    let temp = tempdir().expect("tempdir");
    write_k9_marker(temp.path());

    init::run(temp.path()).expect("init");

    let config = read_config(temp.path());
    let k9 = config.integrations.k9.expect("k9 section");
    assert!(k9.enabled);
    assert_eq!(k9.agent_docs_path, "agent_docs");
}

#[test]
fn init_on_existing_config_leaves_integrations_alone() {
    let temp = tempdir().expect("tempdir");
    // First init has no K9 marker, so no section is written.
    init::run(temp.path()).expect("first init");
    assert!(read_config(temp.path()).integrations.k9.is_none());

    // Now add the K9 marker and re-run init.
    write_k9_marker(temp.path());
    init::run(temp.path()).expect("second init");

    let config = read_config(temp.path());
    assert!(
        config.integrations.k9.is_none(),
        "re-init must not silently modify existing config"
    );
}

#[test]
fn enable_k9_requires_detection_without_force() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let err = integrations::enable_k9(temp.path(), None, false)
        .expect_err("enable should refuse without detection or force");
    let message = err.to_string();
    assert!(
        message.contains("not detected"),
        "unexpected error: {message}"
    );
}

#[test]
fn enable_k9_succeeds_when_detected() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    write_k9_marker(temp.path());
    init::run(temp.path()).expect("init");

    // The init path already enabled it; explicitly call enable to verify the
    // command works against an already-configured section too.
    integrations::enable_k9(temp.path(), None, false).expect("enable");

    let k9 = read_config(temp.path())
        .integrations
        .k9
        .expect("k9 section");
    assert!(k9.enabled);
}

#[test]
fn enable_k9_with_force_writes_section_even_when_undetected() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    integrations::enable_k9(temp.path(), None, true).expect("enable forced");

    let k9 = read_config(temp.path())
        .integrations
        .k9
        .expect("k9 section");
    assert!(k9.enabled);
}

#[test]
fn enable_k9_respects_custom_agent_docs_path() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    write_k9_marker_at(temp.path(), "docs/k9");
    init::run(temp.path()).expect("init");

    integrations::enable_k9(temp.path(), Some("docs/k9"), false).expect("enable");

    let k9 = read_config(temp.path())
        .integrations
        .k9
        .expect("k9 section");
    assert_eq!(k9.agent_docs_path, "docs/k9");
    assert!(k9.enabled);
}

#[test]
fn disable_k9_keeps_section_but_flips_enabled() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    write_k9_marker(temp.path());
    init::run(temp.path()).expect("init");

    integrations::disable_k9(temp.path()).expect("disable");

    let k9 = read_config(temp.path())
        .integrations
        .k9
        .expect("k9 section persists");
    assert!(!k9.enabled);
    assert_eq!(k9.agent_docs_path, "agent_docs");
}

#[test]
fn disable_k9_errors_when_not_configured() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let err =
        integrations::disable_k9(temp.path()).expect_err("disable without section should error");
    assert!(err.to_string().contains("not configured"));
}

#[test]
fn status_surfaces_drift_when_enabled_but_marker_missing() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    write_k9_marker(temp.path());
    init::run(temp.path()).expect("init");

    // Remove the marker after init — drift case.
    fs::remove_file(temp.path().join("agent_docs/project_state.md")).expect("remove marker");

    let summary = status::run(temp.path()).expect("status");
    assert!(!summary.k9_detected);
    assert!(summary.k9_enabled);
    let drift = summary.k9_drift.expect("drift message");
    assert!(drift.contains("missing"), "unexpected drift: {drift}");
}

#[test]
fn status_surfaces_available_hint_when_detected_but_not_enabled() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    // No marker at init time; section never gets written.
    init::run(temp.path()).expect("init");
    // Add the marker afterward.
    write_k9_marker(temp.path());

    let summary = status::run(temp.path()).expect("status");
    assert!(summary.k9_detected);
    assert!(!summary.k9_enabled);
    let drift = summary.k9_drift.expect("hint message");
    assert!(drift.contains("enable k9"), "unexpected hint: {drift}");
}

#[test]
fn status_returns_no_drift_in_clean_states() {
    let _env_guard = crate::support::env_read_lock();

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    // No K9 anywhere, no section in config → no drift.
    let summary = status::run(temp.path()).expect("status");
    assert!(!summary.k9_detected);
    assert!(!summary.k9_enabled);
    assert!(summary.k9_drift.is_none());

    // Enable K9 with detection → no drift either.
    write_k9_marker(temp.path());
    integrations::enable_k9(temp.path(), None, false).expect("enable");
    let summary = status::run(temp.path()).expect("status");
    assert!(summary.k9_detected);
    assert!(summary.k9_enabled);
    assert!(summary.k9_drift.is_none());
}
