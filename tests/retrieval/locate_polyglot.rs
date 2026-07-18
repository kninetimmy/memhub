//! Polyglot code-locate eval (task 88). Validates the M11 code locator —
//! including the task-87 module-doc capture — across the six non-Rust AST
//! languages (Python, Go, TypeScript, JavaScript, C#, Java).
//!
//! memhub's own tree is pure Rust, so `memhub eval locate` (which indexes
//! the current repo) can only ever exercise Rust. This test writes a small
//! fixture set into a throwaway git repo and runs the real
//! `eval::run_locate` harness over it against
//! `tests/code_locate_golden_polyglot.json`, so the non-Rust grammars are
//! covered by the same Recall@K contract as the Rust golden. The fixture
//! paths and symbols below are pinned by that golden's matchers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use memhub::code_index::{self, code_index_db_path};
use memhub::commands::eval::{self, GoldenKind, LocateEvalOptions, LocateEvalSummary};
use memhub::commands::init;
use memhub::config::{ProjectConfig, RetrievalMode};

// --- Fixtures -------------------------------------------------------------
// Each file carries a DETACHED leading doc (module/package/file doc) so the
// task-87 module-doc chunk is emitted, plus a couple of named symbols. The
// detachment rules mirror the chunker's own unit tests: a Go package doc
// immediately above `package`, a Python module docstring as the first
// statement, a TS/JS `/** @module */` separated from the first export by a
// blank line, and a C#/Java `///`/`/** */` before a using/package preamble.

const PAYMENTS_PY: &str = r#""""Payment capture and refund orchestration for the checkout service."""

import logging

logger = logging.getLogger(__name__)


class PaymentProcessor:
    """Coordinates charge authorization against the payment gateway."""

    def authorize_charge(self, amount_cents: int, token: str) -> bool:
        """Authorize a charge for the given amount and card token."""
        logger.info("authorizing %d cents", amount_cents)
        return amount_cents > 0


def issue_refund(transaction_id: str, amount_cents: int) -> bool:
    """Issue a partial or full refund for a settled transaction."""
    return amount_cents >= 0
"#;

const DISTANCE_GO: &str = r#"// Package geo provides planar geometry helpers for the mapping subsystem.
package geo

import "math"

// Point is a 2D coordinate in the plane.
type Point struct {
	X float64
	Y float64
}

// EuclideanDistance returns the straight-line distance between two points.
func EuclideanDistance(a, b Point) float64 {
	dx := a.X - b.X
	dy := a.Y - b.Y
	return math.Sqrt(dx*dx + dy*dy)
}
"#;

const SESSION_TS: &str = r#"/** @module session - Browser session token lifecycle for the web client. */

export interface Session {
  token: string;
  expiresAt: number;
}

export function createSession(token: string, ttlSeconds: number): Session {
  return { token, expiresAt: Date.now() + ttlSeconds * 1000 };
}

export function isSessionExpired(session: Session): boolean {
  return Date.now() > session.expiresAt;
}
"#;

const RETRY_JS: &str = r#"/** @module retry - Exponential backoff retry helper for flaky network calls. */

export function backoffDelay(attempt) {
  return Math.min(1000 * 2 ** attempt, 30000);
}

export async function retryWithBackoff(fn, maxAttempts) {
  let lastError;
  for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
    try {
      return await fn();
    } catch (err) {
      lastError = err;
    }
  }
  throw lastError;
}
"#;

const STOCK_LEDGER_CS: &str = r#"/// <summary>Warehouse stock ledger tracking for the fulfillment pipeline.</summary>
using System;

namespace Inventory
{
    public class StockLedger
    {
        public int AvailableUnits(string sku)
        {
            return sku.Length;
        }

        public void ReserveStock(string sku, int quantity)
        {
            Console.WriteLine($"reserving {quantity} of {sku}");
        }
    }
}
"#;

const RATE_LIMITER_JAVA: &str = r#"/** Token-bucket rate limiting for the public API gateway. */
package app;

public class RateLimiter {
    private final int capacity;

    public RateLimiter(int capacity) {
        this.capacity = capacity;
    }

    public boolean tryAcquire(int tokens) {
        return tokens <= capacity;
    }
}
"#;

/// The six fixture files, keyed by their repo-relative path (which the
/// golden's `path_contains` matchers reference verbatim).
fn fixtures() -> Vec<(&'static str, &'static str)> {
    vec![
        ("services/payments.py", PAYMENTS_PY),
        ("geo/distance.go", DISTANCE_GO),
        ("src/auth/session.ts", SESSION_TS),
        ("lib/retry.js", RETRY_JS),
        ("src/Inventory/StockLedger.cs", STOCK_LEDGER_CS),
        ("src/main/java/app/RateLimiter.java", RATE_LIMITER_JAVA),
    ]
}

// --- Harness --------------------------------------------------------------

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Switch the repo to hybrid so a refresh embeds chunks (the golden's
/// module-doc queries lean on vector similarity, not just FTS).
fn set_hybrid(root: &Path) {
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    config.save(&config_path).expect("save config");
}

/// A throwaway git+memhub repo (hybrid mode) with the polyglot fixtures
/// committed, mirroring `repo_with_files` in tests/locate.rs.
fn polyglot_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    git(root, &["init"]);
    for (rel, body) in fixtures() {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&abs, body).expect("write fixture");
    }
    init::run(root).expect("memhub init");
    set_hybrid(root);
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "polyglot fixtures",
        ],
    );
    temp
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/code_locate_golden_polyglot.json")
}

fn run(temp: &tempfile::TempDir, use_reranker: bool, floor: Option<f32>) -> LocateEvalSummary {
    eval::run_locate(
        temp.path(),
        LocateEvalOptions {
            golden_path: golden_path(),
            k: 3,
            use_reranker,
            min_rerank_score: floor,
        },
    )
    .expect("run_locate")
}

// --- Tests ----------------------------------------------------------------

/// The headline contract: every `match` query in the polyglot golden lands
/// the expected file in the top 3 over fusion (reranker off), the same bar
/// the Rust golden holds at. Curated fixtures, so the bar is the full set.
#[test]
fn polyglot_locate_recall_at_3_is_total() {
    let temp = polyglot_repo();
    let summary = run(&temp, false, None);

    assert_eq!(
        summary.match_queries, 17,
        "golden match-query count drifted"
    );
    let misses: Vec<String> = summary
        .outcomes
        .iter()
        .filter(|o| o.kind == GoldenKind::Match && !o.passed)
        .map(|o| format!("{}: {}", o.id, o.failure_reason.clone().unwrap_or_default()))
        .collect();
    assert!(
        misses.is_empty(),
        "fusion Recall@3 should be total across all six languages; misses:\n{}",
        misses.join("\n")
    );
    assert!((summary.recall_at_k - 1.0).abs() < 1e-9);
}

/// The task-87 proof, deterministically (no embedding ranking involved):
/// every fixture file produces exactly the module/package/file-doc chunk
/// its grammar hook is meant to capture.
#[test]
fn polyglot_each_language_emits_a_module_doc_chunk() {
    let temp = polyglot_repo();
    let root = temp.path();
    code_index::refresh(root).expect("refresh");

    let conn = rusqlite::Connection::open(code_index_db_path(root)).expect("open code_index");
    for (rel, _) in fixtures() {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_chunks c
                 JOIN indexed_files f ON f.id = c.file_id
                 WHERE f.path = ?1 AND c.kind = 'module-doc'",
                rusqlite::params![rel],
                |r| r.get(0),
            )
            .expect("count module-doc chunks");
        assert_eq!(
            n, 1,
            "{rel} should emit exactly one module-doc chunk, found {n}"
        );
    }
}

/// The module-doc queries specifically (no symbol matcher) must pass, so the
/// module-doc chunk is load-bearing for retrieval, not just present in the DB.
#[test]
fn polyglot_module_doc_queries_retrieve_their_file() {
    let temp = polyglot_repo();
    let summary = run(&temp, false, None);

    let doc_misses: Vec<String> = summary
        .outcomes
        .iter()
        .filter(|o| o.id.ends_with("-moduledoc") && !o.passed)
        .map(|o| format!("{}: {}", o.id, o.failure_reason.clone().unwrap_or_default()))
        .collect();
    assert!(
        doc_misses.is_empty(),
        "every *-moduledoc query should retrieve its file via the module-doc chunk; misses:\n{}",
        doc_misses.join("\n")
    );
    // Sanity: there really are six of them (one per language).
    let doc_total = summary
        .outcomes
        .iter()
        .filter(|o| o.id.ends_with("-moduledoc"))
        .count();
    assert_eq!(doc_total, 6, "expected one module-doc query per language");
}

/// Mirrors the Rust golden's no-floor stance: under fusion the nonsense
/// probes leak (no gate), but a `--rerank` floor of 0.0 rejects both.
#[test]
fn polyglot_nonsense_probes_reject_under_rerank_floor() {
    let temp = polyglot_repo();

    let fusion = run(&temp, false, None);
    assert_eq!(fusion.empty_queries, 2, "two nonsense probes expected");

    let reranked = run(&temp, true, Some(0.0));
    assert_eq!(
        reranked.safety_failures, 0,
        "a rerank floor of 0.0 should reject both nonsense probes"
    );
}
