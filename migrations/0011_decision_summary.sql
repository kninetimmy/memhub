-- Migration 0011: optional natural-language `summary` on decisions.
--
-- Supports embed-text augmentation for jargon-titled decisions (task #23,
-- decision 72). When non-NULL, the summary is prepended to the embed text
-- the bi-encoder sees AND to the (title, body) pair the cross-encoder
-- re-ranker scores, so natural-language queries surface decisions whose
-- title/rationale share no surface tokens with the query.
--
-- Nullable: existing rows continue to work unchanged. Callers that want
-- the recall lift on a specific decision must either pass --summary at
-- `memhub decision add` time or backfill via `memhub decision
-- set-summary <ID> <SUMMARY>`.
--
-- FTS5 indexes are deliberately NOT touched in this migration. The
-- augmentation path is bi-encoder + cross-encoder; FTS5 already handles
-- the keyword case well and rebuilding decisions_fts adds churn for no
-- proven win. A follow-up migration can index summary if FTS-side
-- paraphrase matching turns out to matter.

ALTER TABLE decisions ADD COLUMN summary TEXT;
