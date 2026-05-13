---
name: reindex
description: Rebuild memhub embeddings for the active model. Use after a model upgrade or when memhub.recall surfaces a stale_embeddings warning. Always ask before running.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-13
---

Wipe and rebuild the `embeddings` table for the bundled embedding
model. Runs `memhub index rebuild` under the hood. Local-only,
multi-second operation. Asks the user before mutating.

This is the Codex counterpart to the Claude Code `/reindex` skill.
Both invoke the same `memhub index` CLI surface.

## When to invoke

Reindex is appropriate when:

1. `memhub.recall` returns a `warnings[].kind == "stale_embeddings"`
   entry. The bundle is still usable, but hybrid scoring is
   undercounting some rows.
2. The repo was running on `mode = fts` (no eager-embed on writes),
   and the user has just switched `[retrieval] mode` to `hybrid` in
   `.memhub/config.toml`. Pre-existing rows have no embedding;
   rebuild backfills them.
3. A new memhub release ships with a different bundled model.
   `memhub index status` shows the active model name; if it doesn't
   match the embeddings table, rebuild.

Do **not** invoke reindex:

- Mid-conversation as a "just to be safe" reflex. Recall results are
  usable even with stale embeddings.
- Without surfacing the reason to the user first. This is the user's
  decision, not yours.
- In repos without `.memhub/` (run `/check-init` first).

## Preconditions

- `.memhub/` exists in the working repo.
- `memhub` binary on PATH.
- `[retrieval] mode = "hybrid"` is the long-term target. Reindex
  still works under `fts` mode (the embeddings get populated for
  future hybrid use), but mention that recall won't consult them
  until mode flips.

## Flow

1. **Show the user the reason.** Quote the recall warning, or state
   that the user asked for it, or note the model mismatch. One
   sentence.
2. **Show the size.** Run `memhub index status --json` first and
   report row counts:
   - `facts.total` / `facts.embedded`
   - `decisions.total` / `decisions.embedded`
   - `tasks.total` / `tasks.embedded`
   This tells the user roughly how long the rebuild will take
   (~10-50 ms per row on the bundled BGE-small model).
3. **Ask for confirmation.** "Rebuild N embeddings for model
   `<model>`? This is a few seconds and won't touch durable facts,
   decisions, or tasks." Wait for explicit yes.
4. **Run rebuild.**

   ```bash
   memhub index rebuild --actor codex:reindex --json
   ```

   The actor string should identify the agent and the operation
   (e.g. `codex:reindex`) for the writes_log audit row.

5. **Report.** Echo the JSON summary fields: `facts`, `decisions`,
   `tasks`, `deleted`, `elapsed_ms`. One short line each.

6. **Confirm.** Re-run `memhub index status` and confirm
   `missing_count == 0`. If anything is still missing, surface the
   gap; rebuild may have failed partway.

## Notes

- Reindex deletes all `embeddings` rows for the active model in one
  transaction, then re-inserts from current source bodies. No
  durable facts, decisions, or tasks are touched. The `writes_log`
  gets one summary entry attributed to the actor you passed.
- The rebuild ignores `[retrieval] mode`. Even in `fts` mode, the
  embeddings table gets populated — handy for migrating
  `fts → hybrid` without a separate backfill pass.
- Reindex is **not** automatic. Recall surfaces the warning; you ask
  the user; the user decides. Per the memhub design principle,
  "agents are untrusted writers" — reindex is a writer.
- For the equivalent gesture from Claude Code, see the `/reindex`
  skill under `templates/skills/claude/`.
