---
name: wrap-up
description: Wrap up the OMP session with approved memhub updates, review, and rendering.
---

From the project root, verify `.memhub/` exists and `memhub` is on PATH;
otherwise stop and report the missing prerequisite. Run
`memhub wrapup-policy` (or `--json` and read `wrapup_policy.instructions`).
Follow its detection, schema check, read window, draft assembly, write order,
render and sync instructions for this repo's configured verbosity. At
`transcript` verbosity run the full non-archive flow and report transcript
archiving as unsupported for OMP. Use the current conversation; never read
OMP's internal session files or use another host's identity or transcript reader.

Show the drafts together, grouped by kind, and wait for explicit approval
per item (or an unambiguous approval of all). Do not write rejected items.
Only review existing proposals with the user's explicit decision; a staged
proposal is not a durable fact or decision. Under an active Orch grant that
covers wrap-up, the canonical exception permits agent approval within its
limits: skip pending review decisions, architecture writes and fact
re-verification, stage new facts/decisions via MCP `propose_fact` /
`propose_decision` for human `memhub review accept`, and make no global writes.
If MCP is unavailable, report what could not be staged; do not bypass review.

For approved CLI writes substitute `--actor omp:wrap-up` wherever the command
supports `--actor`. Human-approved repo fact/decision adds also use
`--source user+agent:omp`. Include a one-sentence summary for each new decision.
A verified command is not a fact: record it via `command verify` (no actor
flag) or MCP `record_command`. Record a user-approved reference doc with
`memhub doc add <path> --json --actor omp:wrap-up`. If the user explicitly
requests a global fact or decision, stage it through MCP with `global=true`
for review instead of running the repo add command. Scope and review gates
apply to every proposal, not only facts.

Halt on the first non-zero exit; report the failed command and avoid replaying
successful writes. After the approved writes, run
`memhub render --actor omp:wrap-up`. If render fails, stop before sync and
resume at render after fixing the cause; do not repeat durable writes. Follow the
policy's sync guards, then report the rendered paths, staged count, and
any remaining review actions. No host hook is installed: the next session
reads the project frame once on turn one unless already in context.
