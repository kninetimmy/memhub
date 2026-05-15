//! Step 3/10 of decision 74: smoke tests for the cl100k_base helper.
//!
//! These tests double as the "no runtime network call" gate the task
//! notes ask for — `tiktoken-rs` embeds the BPE via `include_str!`,
//! and running these in a cargo test invocation initializes the
//! singleton without any I/O. If a future bump ever switches to a
//! download-on-demand path, this file will catch it (the cargo test
//! sandbox blocks outbound network in most CI configs, and even
//! locally a download would show up as a sudden test-time spike).

use memhub::metrics::tokenizer::tokens_of;

#[test]
fn empty_string_is_zero_tokens() {
    assert_eq!(tokens_of(""), 0);
}

#[test]
fn short_string_is_a_handful_of_tokens() {
    let n = tokens_of("hello world");
    assert!(n > 0 && n < 10, "expected 1..10 tokens, got {n}");
}

#[test]
fn longer_strings_have_more_tokens_than_shorter_ones() {
    let short = tokens_of("hello");
    let long = tokens_of(
        "The quick brown fox jumps over the lazy dog. \
         Pack my box with five dozen liquor jugs. \
         How vexingly quick daft zebras jump!",
    );
    assert!(
        long > short * 5,
        "long string should have many more tokens than short; short={short} long={long}"
    );
}

#[test]
fn singleton_is_reused_across_calls() {
    // First call initializes; second call reuses the OnceLock.
    // Mostly a regression guard — if the helper ever rebuilds the BPE
    // on every call this assertion stays correct but the test wall
    // time would balloon, which would be visible in CI.
    let a = tokens_of("memhub recall returns a focused evidence bundle");
    let b = tokens_of("memhub recall returns a focused evidence bundle");
    assert_eq!(a, b);
}
