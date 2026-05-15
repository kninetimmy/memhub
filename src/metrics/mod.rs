//! Opt-in token-accounting subsystem (decision 74).
//!
//! Off by default; users enable it per-machine via
//! `memhub metrics enable`. Two independent components live here:
//!
//! - **Component A — recall proxy**: every `memhub recall` call
//!   appends a row to `recall_metrics` with the bundle size and the
//!   ledger-equivalent baseline so the dashboard can report
//!   "context offset vs full-ledger baseline". Local arithmetic only;
//!   cannot break across Claude Code updates.
//! - **Component B — session accounting**: scrapes agent transcript
//!   JSONL for real `usage.input_tokens` / `usage.output_tokens` /
//!   cache totals. Kept behind its own kill switch in case the
//!   transcript shape shifts.
//!
//! Both share `tokenizer::tokens_of` for size estimates so any
//! ratio between bundle size and ledger size uses the same yardstick.
//!
//! `maintenance` is the shared post-scrape upkeep both components feed:
//! it attributes recall rows to a session by timestamp window and
//! prunes rows past the retention horizon. It runs opportunistically
//! from `db::open_project`, gated by the master switch alone.

pub mod formatter;
pub mod maintenance;
pub mod recall_proxy;
pub mod session_scraper;
pub mod tokenizer;
