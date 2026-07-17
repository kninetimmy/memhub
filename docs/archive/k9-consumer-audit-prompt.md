# K9 consumer audit prompt

This is a copy-pasteable prompt for the K9 Claude Framework repo. Paste it
into a Claude Code session after `cd`-ing into the K9 repo to get an audit
of whether K9's `/wrap-up` skill is in compliance with the v1 memhub
contract at [`k9-wrap-up-contract.md`](k9-wrap-up-contract.md).

The K9 consumer edit (calling memhub commands from `/wrap-up`) is
explicitly tracked as "outside this repo" in
[`docs/archive/k9-integration.md`](k9-integration.md). This
prompt is the operator-facing handoff for that work.

## When to use it

- After installing or updating memhub on a repo that uses K9.
- When you want to verify the K9 side is consuming the latest contract.
- Before opening a PR against the K9 repo to wire up `/wrap-up`.

## The prompt

```
You're in the K9 Claude Framework repo. I need you to evaluate whether any
changes are required here to consume the memhub ↔ K9 wrap-up integration
contract, which is now shipped memhub-side.

# Background

memhub is a sibling project (local-first Rust CLI for durable per-repo
project memory, lives at /Users/stephenelswick/memhub). It defines a stable
v1 contract describing how K9's /wrap-up skill should shell out to the
memhub CLI when both systems are installed in the same repo. Phases 1, 2,
and 3 of the integration are shipped *on the memhub side*. The K9-repo
consumer edit — the part that actually calls those memhub commands from
/wrap-up — is explicitly tracked as "outside this repo" and is the open
question.

# Authoritative references (read these first, verbatim)

1. /Users/stephenelswick/memhub/docs/archive/k9-wrap-up-contract.md
   — the v1 contract: gate command, read surfaces, mutating commands,
     JSON shapes, exit codes, actor convention.
2. /Users/stephenelswick/memhub/docs/archive/k9-integration.md
   — phasing, operating modes (K9-only, memhub-only, K9+memhub),
     source-of-truth model, non-goals.

Treat those two files as source of truth. Do not invent commands or
flags that aren't in the contract.

# What I want from you

Audit this K9 repo and report — DO NOT MODIFY ANYTHING YET — on:

1. Does /wrap-up (or whatever the equivalent skill/command is named here)
   currently do anything memhub-aware? Find the file(s) and quote the
   relevant sections.

2. Gap analysis vs. the v1 contract. Specifically:
   - Is there a `memhub integrations check-k9` exit-code gate near the
     top of /wrap-up?
   - On the gate's success path, does /wrap-up read pending writes via
     `memhub review list --status pending --json` and fold them into
     the human-approval draft?
   - On approval, does it shell out to `memhub fact add`,
     `decision add`, `task add`, `task done`, `review accept`,
     `review reject` with `--json --actor k9:wrap-up`?
   - Is the DB-writes-first / Markdown-writes-second ordering enforced,
     with hard abort on any non-zero exit before touching agent_docs/?

3. Are /init-project and /check-init affected? The contract is scoped to
   /wrap-up, but flag anything those skills do that would conflict with
   or duplicate memhub's behavior.

4. List concrete proposed edits (file + section + intent) needed to
   bring /wrap-up into compliance with v1. If no changes are needed,
   say so plainly and show me the evidence.

5. Flag anything in the K9 repo that contradicts the operating-modes
   contract: K9 must continue to work standalone when no .memhub/
   exists; memhub never required.

6. Separately from the /wrap-up audit: a new memhub command
   `memhub integrations bootstrap-k9` exists for first-install priming
   of an empty memhub DB from existing K9 files. See
   /Users/stephenelswick/memhub/README.md ("Priming an empty database
   from existing K9 files") for behavior. Question: should K9's
   /init-project skill suggest this command to the user when it
   detects an existing .memhub/ alongside populated agent_docs/?
   Flag this as an optional follow-up — it is not part of the v1
   /wrap-up contract.

Report findings as a punch list. Wait for my approval before making any
edits.
```

## Maintenance

When the contract version bumps (e.g., `v1` → `v2`), update the prompt's
references and add a section asking the K9 audit to call out anything in
the existing consumer code that was wired against the previous version.
