//! Harness binary for the upgrade / global-memory / GC / infra subsystem
//! tests (Wave 5 U4, issue #90). One `cargo test` binary in place of
//! thirteen separate `tests/*.rs` binaries — same tests, same assertions,
//! just grouped by subsystem and no longer separately linked (each used to
//! statically embed its own copy of the ~250 MB bge-small/MiniLM ONNX
//! models). Per-file `cargo test --test <old-name>` granularity is gone
//! (decision Q13); `cargo test <substring>` filtering still selects the
//! same tests, e.g. `cargo test skill_parity`.
//!
//! `support` is a test-only helper module, not a test file itself: see
//! `upgrade/support.rs` for why several tests below take `support::env_lock()`.
#[path = "upgrade/support.rs"]
mod support;

#[path = "upgrade/discover_outside_repo.rs"]
mod discover_outside_repo;
#[path = "upgrade/global_memory.rs"]
mod global_memory;
#[path = "upgrade/json_contracts.rs"]
mod json_contracts;
#[path = "upgrade/migrations_auto_apply.rs"]
mod migrations_auto_apply;
#[path = "upgrade/skill_parity.rs"]
mod skill_parity;
#[path = "upgrade/upgrade_audit_nag.rs"]
mod upgrade_audit_nag;
#[path = "upgrade/upgrade_degrade.rs"]
mod upgrade_degrade;
#[path = "upgrade/upgrade_manifest.rs"]
mod upgrade_manifest;
#[path = "upgrade/upgrade_registry.rs"]
mod upgrade_registry;
#[path = "upgrade/upgrade_resync_transient.rs"]
mod upgrade_resync_transient;
#[path = "upgrade/upgrade_skills.rs"]
mod upgrade_skills;
