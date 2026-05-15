-- Migration 0012: opt-in token-accounting tables (step 1/10 of decision 74).
--
-- recall_metrics captures one row per memhub recall call when
-- [metrics] enabled = true and [metrics.recall_proxy] is on: bundle
-- token count actually returned vs the ledger-equivalent baseline,
-- so the dashboard can report "context offset vs full-ledger
-- baseline". session_id is nullable because attribution to a Claude
-- Code / Codex session is reconciled post-hoc by timestamp window.
-- query_hash is sha256 of the query string; no plaintext queries are
-- stored.
--
-- session_metrics tracks real input/output/cache token usage scraped
-- incrementally from agent transcript JSONL. session_id is UNIQUE so
-- the scraper UPSERTs on resume; last_scanned_offset is the byte
-- offset into the JSONL we have already consumed.
--
-- Both tables are off-default — nothing writes here unless the user
-- opts in via memhub metrics enable. See decision 74 for the design
-- and tasks #26-34 for the rest of the build order.

CREATE TABLE IF NOT EXISTS recall_metrics (
    id INTEGER PRIMARY KEY,
    ts TEXT NOT NULL,
    session_id TEXT,
    query_hash TEXT NOT NULL,
    bundle_tokens INTEGER NOT NULL,
    ledger_tokens INTEGER NOT NULL,
    rerank_used INTEGER NOT NULL,
    result_count INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS recall_metrics_ts_idx
    ON recall_metrics(ts);
CREATE INDEX IF NOT EXISTS recall_metrics_session_idx
    ON recall_metrics(session_id);

CREATE TABLE IF NOT EXISTS session_metrics (
    id INTEGER PRIMARY KEY,
    session_id TEXT UNIQUE NOT NULL,
    agent TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_creation_tokens INTEGER DEFAULT 0,
    recall_calls INTEGER DEFAULT 0,
    last_scanned_offset INTEGER
);

CREATE INDEX IF NOT EXISTS session_metrics_started_idx
    ON session_metrics(started_at);
