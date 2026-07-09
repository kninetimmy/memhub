//! Harness binary for the retrieval / eval / code-index subsystem tests
//! (Wave 5 U4, issue #90). One `cargo test` binary in place of eight
//! separate `tests/*.rs` binaries — same tests, same assertions, just
//! grouped by subsystem and no longer separately linked (each used to
//! statically embed its own copy of the ~250 MB bge-small/MiniLM ONNX
//! models). Per-file `cargo test --test <old-name>` granularity is gone
//! (decision Q13); `cargo test <substring>` filtering still selects the
//! same tests, e.g. `cargo test retrieval_golden_hermetic`.
#[path = "retrieval/code_index.rs"]
mod code_index;
#[path = "retrieval/deny_list.rs"]
mod deny_list;
#[path = "retrieval/locate.rs"]
mod locate;
#[path = "retrieval/locate_polyglot.rs"]
mod locate_polyglot;
#[path = "retrieval/m8_retrieval_embed.rs"]
mod m8_retrieval_embed;
#[path = "retrieval/m8_retrieval_eval.rs"]
mod m8_retrieval_eval;
#[path = "retrieval/m8_retrieval_schema.rs"]
mod m8_retrieval_schema;
#[path = "retrieval/retrieval_golden_hermetic.rs"]
mod retrieval_golden_hermetic;
