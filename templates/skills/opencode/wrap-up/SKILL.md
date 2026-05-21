---
name: wrap-up
description: Wrap up an OpenCode memhub session; use at session end to route durable memory updates for review.
compatibility: opencode
---

# Skill: wrap-up

Summarize the session, propose durable memory updates, and render after approved writes.

Workflow:
- Review what changed in the conversation and working tree; do not invent project history.
- Stage agent-originated facts and decisions for human review, or use CLI writes only after explicit user approval.
- Attribute OpenCode skill writes with `--actor opencode:wrap-up`; accepted agent-surfaced facts/decisions should use source `user+agent:opencode`.
- Record useful session notes and completed tasks when the user confirms them.
- Run `memhub render` after durable writes land.
- End with what was recorded, what was skipped, and any pending review items.
