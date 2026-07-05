---
name: metrics
description: >
  Show the token-accounting dashboard panel for the current memhub project — 7-day and 30-day recall/session token totals, context offset vs full-ledger baseline, and a recent-sessions table. Trigger on: "token usage", "context cost", "how many tokens", "recall savings", "show memhub metrics", "token accounting".
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-15
---

Display the memhub token-accounting dashboard for this repo.
Read-only — no writes, no config changes.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH, OR `memhub.metrics` MCP tool available.

## Invocation

Prefer the MCP tool — it returns `rendered_panel` directly with no
shell quoting or parsing required:

```
memhub.metrics()
```

CLI fallback:

```bash
memhub metrics status
```

## Rendering

**MCP path (preferred):**

Check the `enabled` field first.

- `enabled = false` → print:
  ```
  Token accounting is disabled for this repo.
  Run `memhub metrics enable` to opt in and start tracking session tokens.
  ```
  Stop here.

- `enabled = true` → print `rendered_panel` verbatim. Do not reformat,
  summarize, or restructure the panel — the Rust formatter owns the
  layout (decision 76).

**CLI fallback:**

Run `memhub metrics status` and surface its output. If it reports
"disabled", print the same one-liner as the MCP disabled branch above.

## What the panel contains

When enabled with data, the panel has three sections:

1. **Last 7 days** — 4-line period block:
   Recalls / Sessions / Real tokens (in/out/cache_read/cache_creation)
   / Context offset vs full-ledger baseline

2. **Last 30 days** — same 4-line shape

3. **Recent sessions (≤10, newest first)** — fixed-width table with
   columns: session (8-char prefix), agent, started (UTC MM-DD HH:MM),
   in, out, recalls

When enabled but no data has been captured yet, the panel reads:
`Metrics enabled — no recall or session data captured yet.`

## Notes

- The `context_offset_pct` field is `bundle_tokens / ledger_tokens * 100`.
  It measures how much smaller the recall bundle is relative to returning
  the full ledger — a proxy for context saved, not exact tokens saved
  (decision 74, documented caveat: tiktoken-cl100k is ±10% vs Anthropic's
  tokenizer; ratios are sound, absolute numbers are approximate).
- Timestamps are local time (converted by SQLite `datetime(..., 'localtime')`).
- Token integers are formatted with comma thousands separators.
