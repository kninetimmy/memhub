//! Hermetic retrieval golden eval (Wave 3 rider N28, issue #44).
//!
//! `tests/retrieval_golden.json`'s queries are self-referential: they were
//! written to match memhub's own real decisions/facts/tasks (e.g. decision
//! 34 "Agents prefer recall over reading PROJECT_LEDGER.md", decision 48
//! "recall is read-only"), so running `memhub eval retrieval` from this
//! repo's root scores against *this machine's* live `.memhub/project.sqlite`
//! — a corpus that drifts as new facts/decisions/tasks land. That makes the
//! §12 verification contract ("the golden-set evals must hold their
//! documented numbers") a property of a given DB's row population, not just
//! of the code, exactly as N28 (review §14) describes.
//!
//! This test seeds a disposable tempdir project whose facts/decisions/task
//! reproduce (faithfully, not gamed) the specific rows the shipped golden
//! set's 18 queries target, switches it to hybrid mode *before* seeding (so
//! every row is eagerly embedded per decision 27 — writing the rows first
//! and flipping modes after would leave the embeddings table empty), then
//! drives the real compiled `memhub eval retrieval --json` binary against
//! it with `--golden` pointed at the actual shipped
//! `tests/retrieval_golden.json` (not a private copy). That is the same
//! hermetic pattern `tests/locate_polyglot.rs` already established for
//! `eval locate` — a fixture DB seeded fresh per run, independent of the
//! developer's live `.memhub` state — applied to the retrieval golden.
//!
//! This test is the new hermetic CI gate for the base retrieval golden set.
//! The live-DB `memhub eval retrieval` run from the repo root (what
//! `/eval-recall` still drives by default) remains available as a
//! self-hosted calibration/dogfood signal, per N28's explicit guidance to
//! "keep the live-DB run as calibration, not the gate."
//!
//! ## Regenerating the fixture
//!
//! There is no persisted fixture DB to regenerate — by design, mirroring
//! `locate_polyglot.rs`: the corpus is defined entirely by the `fact::add` /
//! `decision::add` / `task::add` calls in [`seed_hermetic_corpus`] below,
//! rebuilt from scratch into a fresh tempdir every time this test runs. When
//! `tests/retrieval_golden.json` legitimately changes (a query is added,
//! reworded, or retargeted), update [`seed_hermetic_corpus`] so its rows
//! satisfy the new/changed matchers, update the query-count drift guards
//! in this file, then `cargo test --test retrieval_golden_hermetic`.

use std::path::{Path, PathBuf};
use std::process::Command;

use memhub::commands::{decision, fact, init, task};
use memhub::config::{ProjectConfig, RetrievalMode};
use serde::Deserialize;
use tempfile::tempdir;

// --- Fixture ---------------------------------------------------------------

/// Switch the freshly-initialized project to hybrid mode. Must run BEFORE
/// any fact/decision/task add — eager-embed (decision 27) only fires when
/// `[retrieval] mode = hybrid` at write time, so seeding first and
/// flipping modes after would leave every row's embedding missing and
/// silently degrade hybrid recall to fts-equivalent scoring.
fn set_hybrid(root: &Path) {
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    config.save(&config_path).expect("save config");
}

/// Enable continuous age decay (Wave 3 L6) at the given half-life on the
/// already-seeded fixture. Applied *after* seeding on purpose: decay is a
/// recall-time scoring knob, so it does not touch the embeddings written at
/// seed time — only how the blended score is computed at query time.
fn set_age_half_life(root: &Path, days: i64) {
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.scoring.age_half_life_days = days;
    config.save(&config_path).expect("save config");
}

/// Seeds a fresh `.memhub` project whose rows satisfy every matcher in the
/// real, shipped `tests/retrieval_golden.json` — faithfully reproducing
/// memhub's own durable decisions (by content, not by row id, since the
/// golden's matchers are substring checks against title/body) rather than
/// inventing softball text that happens to pass.
fn seed_hermetic_corpus(root: &Path) {
    init::run(root).expect("memhub init");
    set_hybrid(root);

    // -- decisions --------------------------------------------------------
    // Verbatim title/rationale/summary text below is copied from this
    // repo's own live `.memhub/project.sqlite` (decision ids in each
    // comment) wherever a golden query cites a specific real decision —
    // not a paraphrase — so the fixture reproduces the exact content the
    // documented production baseline (operations.md's "backfilling
    // summaries on four jargon-titled decisions lifted Recall@3 from
    // 76.5% to 100%") was measured against. Two rows have no citable
    // decision id (the golden targets dogfooding text, not a numbered
    // decision) and stay hand-written; those are called out inline.

    // (decision-recall-readonly / semantic-decision-read-only-audit — decision 48)
    decision::add(
        root,
        "memhub recall is read-only and never writes to writes_log",
        "Recall fetches FTS hits per source table, computes brute-force \
         cosine over the active-model embeddings (hybrid only), blends \
         them via the scoring config, and returns a ranked bundle. No row \
         in writes_log, no durable mutation, no pending_writes entry. \
         Addendum §8 says 'read-only' but codifying it as a decision \
         because the natural temptation when adding observability would \
         be to log every recall call, and that would distort memhub stats \
         and writes_log activity metrics. Logging belongs to writers; \
         recall is a reader.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision recall-readonly");

    // (decision-stale-embedding-warning — no specific decision id; this
    // golden query targets the stale_embeddings UX rule as documented in
    // CLAUDE.md's Session Continuity section, not a single numbered
    // decision, so this row is hand-written rather than copied verbatim.)
    decision::add(
        root,
        "Stale embedding warnings surface before /reindex, never auto-run",
        "When recall detects a content-hash mismatch on a row's embedding \
         it returns a stale_embeddings warning instead of silently \
         degrading. The agent must surface that warning and ask the user \
         before invoking /reindex; recall results stay usable meanwhile.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision stale-embedding-warning");

    // (decision-prefer-recall-over-ledger — decision 34)
    decision::add(
        root,
        "Agents prefer recall over reading PROJECT_LEDGER.md",
        "Load-bearing rule for the token-savings win. Encoded in CLAUDE.md \
         and the existing skills: at session start read PROJECT.md only; \
         reach for the ledger only after recall comes up short.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision prefer-recall-over-ledger");

    // (decision-fts5-virtual-tables — decision 26)
    decision::add(
        root,
        "FTS5 virtual tables attached to source tables",
        "Contentless FTS5 over facts.body, decisions.rationale, and \
         tasks.body. Triggers keep FTS indexes synced with source on \
         insert/update/delete. No data duplication.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision fts5-virtual-tables");

    // (decision-bundle-bge-model — decision 42)
    decision::add(
        root,
        "Bundle BGE-small via build.rs auto-download into OUT_DIR",
        "build.rs fetches model.onnx + 4 tokenizer files from \
         BAAI/bge-small-en-v1.5@main on first build, verifies each against \
         a pinned SHA256, and short-circuits on cache hit. Rejected the \
         manual-fetch-script alternative because contributor ergonomics \
         favor zero-setup cargo build. model.onnx SHA256 (828e1496...cf35, \
         133 MB) came from HF's x-linked-etag; tokenizer file hashes \
         computed locally over downloaded bytes. Decision 23 said \
         'bundled in binary' without specifying how; this is the \
         implementation choice.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision bundle-bge-model");

    // (decision-eager-embed-on-write — decision 27)
    decision::add(
        root,
        "Eager-embed on writes inside the same transaction",
        "Fact, decision, and task add paths re-embed the affected row \
         synchronously. ~50ms write overhead is acceptable for the \
         consistency guarantee. Avoids a background queue and stale-index \
         window.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision eager-embed-on-write");

    // (decision-recall-at-3-metric — no specific decision id; dogfooding
    // text mirroring the M8 PR6 seed in tests/m8_retrieval_eval.rs, not a
    // numbered decision, so hand-written.)
    decision::add(
        root,
        "Eval metric: Recall@3 via tests/retrieval_golden.json",
        "Single-number test: across the golden queries, what fraction had \
         the expected row in the top 3 results?",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision recall-at-3-metric");

    // (decision-source-vocabulary-enforcement — decision 39)
    decision::add(
        root,
        "Source vocabulary is writer-enforced, schema stays unconstrained TEXT",
        "The source-vocabulary addendum already specified writer \
         enforcement of the user / git / observed / agent:<id> / \
         user+agent:<id> vocabulary; until this session, CLI and \
         acceptance paths accepted any string, so typos like \
         user+agnet:codex silently persisted. validate_source() now lives \
         in commands::mod and is called from fact::add_in_tx and \
         decision::add_with_decided_at_in_tx, so both CLI writes and \
         review acceptance go through the same gate. The schema stays \
         unconstrained TEXT on purpose: enforcement lives at the writer \
         layer where it can evolve without migrations.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision source-vocabulary-enforcement");

    // (semantic-decision-stale-embedding — decision 28, content_hash drift;
    // distinct from the hand-written stale_embeddings UX decision seeded
    // above. Carries decision 28's real backfilled `summary` — per
    // operations.md this is one of the "four jargon-titled decisions"
    // whose summary backfill lifted the real Recall@3 from 76.5% to 100%,
    // and it is load-bearing here too: without it the cross-encoder logit
    // for this paraphrase falls below `[retrieval.scoring]
    // min_rerank_score` and the query returns zero hits.)
    decision::add_with_decided_at(
        root,
        "content_hash drift detection per embedding",
        "Store a hash of source body alongside each vector. Mismatch on \
         read marks the embedding stale and triggers re-embed on next \
         eager-embed pass or forces a /reindex prompt.",
        None,
        Some(
            "How does memhub know when a stored embedding has gone stale \
             or out of date relative to the source row it embeds?",
        ),
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision content-hash-drift");

    // (semantic-decision-agent-trust — decision 17; no summary backfilled
    // in production, so the real title+rationale text alone must clear
    // the rerank floor.)
    decision::add(
        root,
        "MCP tool trust split: direct writes for intent, staged writes for claims",
        "Tasks are intent that the user prunes; session notes are scratch; \
         render regenerates from the DB — all low-trust and worth direct \
         MCP tools. Facts and decisions are claims about reality and need \
         the user-approval staging gate; bypassing it via direct MCP \
         fact_add / decision_add would erode the 'agents are untrusted \
         writers' principle that makes memhub trustworthy as a \
         multi-agent store. Codified by which MCP tools exist (task_add, \
         task_done, list_facts, render direct; propose_fact, \
         propose_decision staged).",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision mcp-trust-split");

    // (semantic-decision-cross-machine-transfer — decision 66, real summary)
    decision::add_with_decided_at(
        root,
        "Export format covers session_notes, project_state, and project_arch alongside durable rows",
        "Cross-machine memory transfer use case: exporting only \
         facts/decisions/tasks/commands/pending_writes/writes_log lost \
         the 'what I was thinking when I left off' context (session_notes) \
         and the current-state narratives (project_state, project_arch) \
         that drive .memhub/rendered/PROJECT.md. Added as additive fields \
         with #[serde(default)] in src/export/v1.rs so older exports \
         continue to import cleanly. Excluded by design: commits, files, \
         chunks, embeddings (all derivable on the target via memhub \
         index).",
        None,
        Some(
            "Can I move my project memory between machines? How does \
             cross-machine transfer of memhub state work, and what does \
             the export format include?",
        ),
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision export-format-cross-machine");

    // (semantic-decision-empty-result — decision 33, real summary)
    decision::add_with_decided_at(
        root,
        "Zero-result behavior: empty bundle, no automatic fallback",
        "When recall finds no matches it returns an empty results array, \
         not an automatic dump of PROJECT_LEDGER.md. The agent decides \
         whether to read the ledger as fallback based on the question.",
        None,
        Some(
            "What does memhub return when a query matches nothing? What \
             is the empty-bundle or zero-result behavior?",
        ),
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision zero-result-behavior");

    // (semantic-decision-machine-local — decision 64, real summary)
    decision::add_with_decided_at(
        root,
        "Memhub runtime and render output are machine-local by default",
        "Cross-machine Git conflicts showed that committing DB-derived \
         render output makes generated local state look like shared \
         source. Going forward, .memhub/ stays ignored, render defaults \
         to .memhub/rendered/, legacy agent_docs/PROJECT*.md render paths \
         are ignored, and repos must explicitly opt in if they want \
         tracked rendered markdown.",
        None,
        Some(
            "Which files and directories stay local to your laptop and \
             are not pushed to git? What does memhub keep machine-local \
             by default?",
        ),
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision machine-local-default");

    // -- facts --------------------------------------------------------------
    // (fact-build-command)
    fact::add(root, "build-command", "cargo build", "user", "cli:user").expect("fact build");
    // (fact-test-command)
    fact::add(root, "test-command", "cargo test", "user", "cli:user").expect("fact test");

    // -- tasks ----------------------------------------------------------
    // (task-pr6-eval-harness)
    task::add(
        root,
        "PR6: eval harness — golden queries + /eval-recall skill",
        Some(
            "tests/retrieval_golden.json with 12 seeded queries. memhub \
             eval retrieval command computes Recall@3. /eval-recall skill \
             invokes it and reports the number. Acceptance gate for M8: \
             harness exists and reports a baseline.",
        ),
        "cli:user",
    )
    .expect("task pr6-eval-harness");

    // Deliberately nothing here mentions "zxqv" or similar gibberish — the
    // shipped golden's one `kind: empty` safety probe
    // (`negative-nonsense-tokens`) must find zero hits against this corpus.
}

/// Absolute path to the real, shipped golden set — not a private copy, so
/// this test exercises the actual acceptance contract other PRs edit.
fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/retrieval_golden.json")
}

// --- CLI-shape response (mirrors `cli::output::eval_summary_to_json`) -----

#[derive(Debug, Deserialize)]
struct EvalCliTotals {
    queries: usize,
    match_queries: usize,
    empty_queries: usize,
    match_passes: usize,
    empty_passes: usize,
    safety_failures: usize,
}

#[derive(Debug, Deserialize)]
struct EvalCliOutcome {
    id: String,
    #[allow(dead_code)]
    kind: String,
    passed: bool,
    failure_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EvalCliResult {
    totals: EvalCliTotals,
    recall_at_k: f64,
    outcomes: Vec<EvalCliOutcome>,
}

/// Drives the actual compiled `memhub` binary — `memhub eval retrieval
/// --golden <real-golden> --mode hybrid --json` — with `cwd` set to the
/// seeded fixture, so this exercises the literal CLI surface an agent or
/// `/eval-recall` would invoke, not just the library function it wraps.
fn run_cli_eval(root: &Path) -> EvalCliResult {
    let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
        .current_dir(root)
        .arg("eval")
        .arg("retrieval")
        .arg("--golden")
        .arg(golden_path())
        .arg("--mode")
        .arg("hybrid")
        .arg("--json")
        .output()
        .expect("spawn memhub eval retrieval");
    assert!(
        out.status.success(),
        "memhub eval retrieval failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "parse eval JSON: {e}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

// --- Tests -------------------------------------------------------------

/// The headline hermetic contract: `memhub eval retrieval` run against the
/// fixture (never the live repo DB) reproduces a fixed Recall@3 over the
/// real shipped golden set. This is the reference baseline L2/L3/L6 compare
/// against (issue #44).
#[test]
fn hermetic_retrieval_recall_at_3_matches_baseline() {
    let temp = tempdir().expect("tempdir");
    seed_hermetic_corpus(temp.path());

    let result = run_cli_eval(temp.path());

    // Drift guard: catches the golden file changing shape out from under
    // this fixture (mirrors `shipped_golden_file_parses_cleanly` in
    // tests/m8_retrieval_eval.rs, but against a live run, not just parsing).
    assert_eq!(result.totals.queries, 18, "golden query count drifted");
    assert_eq!(result.totals.match_queries, 17);
    assert_eq!(result.totals.empty_queries, 1);

    let misses: Vec<String> = result
        .outcomes
        .iter()
        .filter(|o| !o.passed)
        .map(|o| format!("{}: {}", o.id, o.failure_reason.clone().unwrap_or_default()))
        .collect();
    assert!(
        misses.is_empty(),
        "hermetic fixture should reproduce the recorded baseline; misses:\n{}",
        misses.join("\n"),
    );
    assert_eq!(result.totals.match_passes, result.totals.match_queries);
    assert!(
        (result.recall_at_k - 1.0).abs() < 1e-9,
        "expected Recall@3 = 100% on the curated fixture, got {}",
        result.recall_at_k,
    );
}

/// Isolates the safety-probe contract: the gibberish query must never leak
/// a hit, independent of whatever else is in the corpus. `/eval-recall`
/// treats a non-zero `safety_failures` as a hard regression signal, so this
/// gets its own assertion rather than folding into the recall test above.
#[test]
fn hermetic_retrieval_safety_probe_never_leaks() {
    let temp = tempdir().expect("tempdir");
    seed_hermetic_corpus(temp.path());

    let result = run_cli_eval(temp.path());

    assert_eq!(result.totals.empty_queries, 1);
    assert_eq!(
        result.totals.safety_failures, 0,
        "gibberish probe leaked a hit against the hermetic fixture"
    );
    assert_eq!(result.totals.empty_passes, 1);
}

/// Wave 3 L6 eval sweep, ON case. With `age_half_life_days = 30` enabled on
/// the same hermetic fixture, Recall@3 holds at 100%. The golden corpus
/// seeds freshly-verified facts and freshly-decided decisions (age ~0), so
/// their decay multiplier is ~1.0 and ranking is unchanged — and decisions
/// are excluded from decay entirely (Q2 / decision 145). This documents the
/// "limited practical effect" caveat empirically: switching decay on does
/// not move the golden numbers because the corpus is fresh; decay only bites
/// on genuinely aged rows (that demotion is unit-tested in
/// `recall::tests::age_decay_demotes_an_aged_fact_when_on`). The OFF baseline
/// is `hermetic_retrieval_recall_at_3_matches_baseline` above.
#[test]
fn hermetic_retrieval_age_decay_on_holds_baseline_on_fresh_corpus() {
    let temp = tempdir().expect("tempdir");
    seed_hermetic_corpus(temp.path());
    set_age_half_life(temp.path(), 30);

    let result = run_cli_eval(temp.path());

    assert_eq!(result.totals.queries, 18, "golden query count drifted");
    let misses: Vec<String> = result
        .outcomes
        .iter()
        .filter(|o| !o.passed)
        .map(|o| format!("{}: {}", o.id, o.failure_reason.clone().unwrap_or_default()))
        .collect();
    assert!(
        misses.is_empty(),
        "age_half_life_days=30 must not regress the fresh golden corpus; misses:\n{}",
        misses.join("\n"),
    );
    assert!(
        (result.recall_at_k - 1.0).abs() < 1e-9,
        "expected Recall@3 = 100% with decay on (fresh corpus), got {}",
        result.recall_at_k,
    );
    assert_eq!(
        result.totals.safety_failures, 0,
        "gibberish probe must still find zero hits with decay on"
    );
}
