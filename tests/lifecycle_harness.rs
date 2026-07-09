//! Harness binary for the lifecycle / review / CLI / render / MCP / stats
//! subsystem tests (Wave 5 U4, issue #90) — everything that isn't in
//! `retrieval_harness` or `upgrade_harness`. One `cargo test` binary in
//! place of nineteen separate `tests/*.rs` binaries — same tests, same
//! assertions, just grouped and no longer separately linked (each used to
//! statically embed its own copy of the ~250 MB bge-small/MiniLM ONNX
//! models). Per-file `cargo test --test <old-name>` granularity is gone
//! (decision Q13); `cargo test <substring>` filtering still selects the
//! same tests, e.g. `cargo test status_health`.
#[path = "lifecycle/audit_md.rs"]
mod audit_md;
#[path = "lifecycle/cli_args.rs"]
mod cli_args;
#[path = "lifecycle/config_example.rs"]
mod config_example;
#[path = "lifecycle/export_import.rs"]
mod export_import;
#[path = "lifecycle/foundation.rs"]
mod foundation;
#[path = "lifecycle/mcp_protocol.rs"]
mod mcp_protocol;
#[path = "lifecycle/metrics_maintenance.rs"]
mod metrics_maintenance;
#[path = "lifecycle/metrics_session_scraper.rs"]
mod metrics_session_scraper;
#[path = "lifecycle/metrics_tokenizer.rs"]
mod metrics_tokenizer;
#[path = "lifecycle/milestone2.rs"]
mod milestone2;
#[path = "lifecycle/narrative.rs"]
mod narrative;
#[path = "lifecycle/render.rs"]
mod render;
#[path = "lifecycle/review.rs"]
mod review;
#[path = "lifecycle/review_stale.rs"]
mod review_stale;
#[path = "lifecycle/session_notes.rs"]
mod session_notes;
#[path = "lifecycle/staleness.rs"]
mod staleness;
#[path = "lifecycle/stats.rs"]
mod stats;
#[path = "lifecycle/status_health.rs"]
mod status_health;
#[path = "lifecycle/sync_md.rs"]
mod sync_md;
#[path = "lifecycle/transcript_archive.rs"]
mod transcript_archive;
#[path = "lifecycle/wrapup_policy.rs"]
mod wrapup_policy;
