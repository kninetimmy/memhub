-- Migration 0020: record the calling surface (CLI vs MCP) on each
-- recall_metrics row (issue #70, Wave 4 gate Q17 / decision 148).
--
-- Q17 asks whether CLI-issued recalls feel slower than MCP-issued ones,
-- so R7's quantization work can be sequenced by where the latency is
-- actually felt. `recall_metrics` today has no way to tell the two
-- call sites apart -- every row is anonymous as to its caller. This
-- adds a nullable `surface` column ('cli' | 'mcp') so a follow-up
-- analysis can split by caller once enough rows accumulate.
--
-- Additive and replay-safe: a plain `ALTER TABLE ... ADD COLUMN`, the
-- same safe shape as 0017/0019. Existing rows get NULL (unknown
-- surface, predating this column); `db::migrations::apply_all`'s
-- `schema_migrations` ledger (not the SQL) owns exactly-once
-- application. Every recall_metrics read in commands::metrics,
-- dashboard, and render selects an explicit column list -- never
-- `SELECT *` -- so they tolerate the new column without changes.

ALTER TABLE recall_metrics ADD COLUMN surface TEXT;
