-- Migration 0023: pointer table for archived session transcripts
-- (Wave 6 W3, issue #96 / decision Q7+Q8).
--
-- When `/wrap-up` runs at `transcript` verbosity, the archiver copies a
-- session's raw agent JSONL into `.memhub/transcripts/<date>-<session-id>
-- .jsonl.zst` and records ONE pointer row here per archived transcript:
-- the session id, the source path it was copied from, the on-disk archive
-- path, the source + compressed byte sizes, and when it was archived.
--
-- This table is deliberately NOT a retrieval source: it is never embedded
-- (no `SourceType` variant, not on the eager-embed path), never surfaced
-- by recall, and excluded from `memhub export` / import (the Export shape
-- is a fixed field list that does not include it). The archive itself
-- lands under gitignored, export-excluded `.memhub/` and is UNREDACTED by
-- design (Q8: v1 is warn + explicit per-wrap-up approval, not content
-- redaction). Retention (`[wrap_up] transcript_retention_days`) prunes
-- both the archive files and these rows past the horizon.
--
-- Pre-assigned migration number 0023 (issue #96): the two sibling Wave 6
-- migrations 0021/0022 (issues #97/#98) may land in either order, so this
-- appends after the current head rather than auto-incrementing. Number
-- gaps are irrelevant -- `db::migrations::apply_all` keys exactly-once
-- application on the `schema_migrations` ledger, not on contiguity.
--
-- `session_id` is UNIQUE: re-archiving a session (a later wrap-up after
-- more turns) overwrites the prior archive and updates the row in place,
-- so the table holds at most one live archive per session.

CREATE TABLE IF NOT EXISTS session_transcripts (
    id            INTEGER PRIMARY KEY,
    session_id    TEXT NOT NULL UNIQUE,
    agent         TEXT NOT NULL,
    source_path   TEXT NOT NULL,
    archive_path  TEXT NOT NULL,
    source_bytes  INTEGER NOT NULL,
    archive_bytes INTEGER NOT NULL,
    created_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
