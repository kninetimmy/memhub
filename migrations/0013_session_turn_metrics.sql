-- Migration 0013: per-turn token granularity for the burn-up chart.
--
-- session_metrics (migration 0012) stores ONE accumulated row per
-- session. That is enough for the cross-session burn-up but cannot
-- render an intra-session curve: "how did this session's token use
-- grow turn by turn". session_turn_metrics adds one append-only row
-- per usage-bearing assistant line the scraper consumes, so the
-- dashboard's "current session" view can plot a live per-turn curve.
--
-- This is finer-grained Component B data, not a new subsystem: it is
-- written by the same incremental scraper pass, gated by the same
-- [metrics] session_accounting sub-switch, and pruned by the same
-- retention window as session_metrics. session_id is NOT unique here
-- (many turns per session); rows are never updated, only inserted as
-- the scraper advances past last_scanned_offset, so each transcript
-- usage line maps to exactly one row. ts is the assistant turn's
-- transcript timestamp (nullable: a usage line may lack one).
--
-- Off-default like the rest of the token-accounting tables — nothing
-- writes here unless the user opts in via `memhub metrics enable` with
-- session_accounting on. See decision 74 and task #35 follow-up.

CREATE TABLE IF NOT EXISTS session_turn_metrics (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    ts TEXT,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0
);

-- Ordered scan per session (id is the stable append order, which is
-- also transcript order since the scraper consumes the JSONL linearly).
CREATE INDEX IF NOT EXISTS session_turn_metrics_session_idx
    ON session_turn_metrics(session_id, id);

-- Retention prune is by ts (same window as session_metrics.ended_at).
CREATE INDEX IF NOT EXISTS session_turn_metrics_ts_idx
    ON session_turn_metrics(ts);
