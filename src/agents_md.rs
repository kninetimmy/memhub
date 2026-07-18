//! Derive `AGENTS.md` (the Codex / OpenCode orientation file) from
//! `CLAUDE.md` so the two can never silently drift.
//!
//! `CLAUDE.md` is the single hand-edited source. `AGENTS.md` is a pure
//! transform of it: swap the `# memhub` H1 for the Codex-flavored title,
//! inject a "generated — do not hand-edit" counterpart note after the
//! intro, and append the two Codex/OpenCode-only sections that have no
//! `CLAUDE.md` counterpart (the Codex attribution block and the Q41
//! fail-safe routing block). Nothing else is rewritten — the retained
//! `## ` sections carry through verbatim.
//!
//! If `CLAUDE.md` ends with an Orch-managed block (`<!-- orchestrator:managed:start -->`
//! … `<!-- orchestrator:managed:end -->`, appended and maintained by Orch's
//! installer — see `.orchestrator/config.toml`), it is split off before the
//! transform above and re-appended last, verbatim, after the two
//! Codex/OpenCode-only sections. Orch's installer expects to find that block
//! at EOF, so the generated file keeps it there instead of letting the
//! appended sections push it out of place.
//!
//! `tests/upgrade/skill_parity.rs` asserts `AGENTS.md == generate_agents_md(CLAUDE.md)`
//! (modulo line endings), so any edit to `CLAUDE.md` must be followed by a
//! regenerate-and-commit of `AGENTS.md`. The regeneration path is
//! `MEMHUB_REGEN=1 cargo test skill_parity` (see that test).

/// The `# memhub` H1 line `CLAUDE.md` must start with.
///
/// `pub(crate)` so `commands::audit_md` (issue #32) can check this same
/// precondition itself before calling [`generate_agents_md`] — that
/// function `assert!`s on it (a deliberate contract-frozen panic for a
/// programmer error, not a recoverable `Result`), so a read-only linter
/// over arbitrary repo content must verify it first rather than risk
/// crashing the CLI on a malformed `CLAUDE.md`.
pub(crate) const CLAUDE_TITLE: &str = "# memhub";

/// The H1 line the generated `AGENTS.md` carries instead.
const AGENTS_TITLE: &str = "# memhub — Codex CLI instructions";

/// Injected right after `CLAUDE.md`'s intro paragraphs (before the first
/// `## ` section). Explains that this file is a generated derivative — not
/// present in `CLAUDE.md`.
const COUNTERPART_NOTE: &str = "This file is the Codex / OpenCode counterpart to `CLAUDE.md`, and is **generated** from it by `generate_agents_md` — do not hand-edit it; edit `CLAUDE.md` and regenerate with `MEMHUB_REGEN=1 cargo test skill_parity`. The two exist so Codex CLI, OpenCode CLI, and Claude Code sessions get the same orientation when they open this repo; where they diverge it is intentional (a different H1, plus the Codex/OpenCode-only sections injected at the end).";

/// Codex-specific attribution guidance, appended to the generated file.
/// Has no `CLAUDE.md` counterpart by design (Claude attributes via the MCP
/// `clientInfo.name`, not these CLI flags).
const AGENT_ATTRIBUTION: &str = "\
## Agent attribution (Codex-specific)

When you write to memhub from the CLI, identify yourself so the row gets attributed correctly. Two flags matter:

- `--source` — origin of the claim. Pass `--source user+agent:codex` on `memhub fact add` / `memhub decision add` writes that go through `/wrap-up` (agent surfaced, user approved). For direct CLI writes the user typed themselves, omit the flag and take the `user` default.
- `--actor` — who performed the write. Pass `--actor codex:wrap-up` from the wrap-up skill; `--actor codex:<skill-name>` from other skills.

See [docs/reference/memhub-prd-source-vocabulary-addendum.md](docs/reference/memhub-prd-source-vocabulary-addendum.md) for the full vocabulary (`user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`).

Once `memhub serve` is registered in `~/.codex/config.toml` (a per-machine step — see README's \"Register the MCP server\"; `memhub doctor` reports current status), writes via MCP attribute automatically — the server reads `clientInfo.name` from `initialize` and tags writes as `codex` / `codex:wrap-up` without you needing to pass anything.";

/// The Q41 fail-safe routing block (decision Q41, issue #30), appended after
/// the attribution section. Claude Code receives these routing rules through
/// the MCP server's `instructions` field; Codex/OpenCode delivery is
/// unverified until the Wave 4 spike, so they carry this compact copy here.
/// Deliberately **not** in `CLAUDE.md`.
const ROUTING_BLOCK: &str = "\
## memhub routing (Codex / OpenCode)
memhub is this repo's project memory. When intent matches, use the memhub MCP tools — do
not fall through to Grep/Read/manual scan:
- past decisions / status / \"is there a fact/task about X\" → `recall`
- find code by what it does / \"where is X\" → `locate`
- session start (turn 1 only) → read `.memhub/rendered/PROJECT.md` once
- new task / mark done → `task_add` / `task_done`; ingest a spec doc → `doc_add`
Never Grep for code by intent before `locate`. Never read `PROJECT_LEDGER.md` before `recall`
(it is the fallback). Never write facts/decisions directly — stage via `propose_fact` /
`propose_decision`, durable on `memhub review accept`.
(Carrier note: Claude Code gets these rules from the MCP `instructions` field; this block is
the Codex/OpenCode fallback pending the Wave 4 delivery spike.)";

/// Opening marker of the Orch-managed block Orch's installer appends to
/// `CLAUDE.md` (and, via [`generate_agents_md`], to `AGENTS.md`). See the
/// module doc comment.
const MANAGED_BLOCK_START: &str = "<!-- orchestrator:managed:start";

/// Closing marker of the Orch-managed block; see [`MANAGED_BLOCK_START`].
const MANAGED_BLOCK_END: &str = "<!-- orchestrator:managed:end -->";

/// Split `claude_md` into `(body, managed_block)`: `body` is the content
/// before a trailing Orch-managed block (line-ending-normalized to `\n`),
/// and `managed_block` is that block's verbatim text when the input ends
/// (modulo trailing newlines) with one, `None` otherwise.
///
/// `pub(crate)` and shared between [`generate_agents_md`]'s step 0 and
/// `commands::audit_md`'s precondition check (issue #148 / audit C3) — the
/// two must agree on exactly what "the managed block" is, or a `CLAUDE.md`
/// whose only `## ` heading lives inside that block can pass the
/// precondition check against the raw text while `generate_agents_md`
/// still panics against the stripped text it actually transforms.
pub(crate) fn split_trailing_managed_block(claude_md: &str) -> (String, Option<String>) {
    let claude = claude_md.replace("\r\n", "\n");
    let trimmed = claude.trim_end_matches('\n');
    let managed_block = trimmed.rfind(MANAGED_BLOCK_START).and_then(|start| {
        let end = start + trimmed[start..].find(MANAGED_BLOCK_END)? + MANAGED_BLOCK_END.len();
        (end == trimmed.len()).then(|| (start, trimmed[start..end].to_string()))
    });
    match managed_block {
        Some((start, block)) => (
            trimmed[..start].trim_end_matches('\n').to_string(),
            Some(block),
        ),
        None => (claude, None),
    }
}

/// Transform `CLAUDE.md` content into the `AGENTS.md` content.
///
/// Pure string work, no dependencies. Line-ending-agnostic: the input is
/// normalized to `\n` and the output is emitted with `\n`, so the committed
/// `AGENTS.md` compares equal to this on every platform (this repo checks
/// out CRLF on Windows under `core.autocrlf=true`).
pub fn generate_agents_md(claude_md: &str) -> String {
    // 0. Split off a trailing Orch-managed block, if present, so it can be
    //    re-appended last after the Codex/OpenCode-only sections rather
    //    than getting pushed out of EOF position by step 3.
    let (claude, managed_block) = split_trailing_managed_block(claude_md);

    // 1. Swap the H1 title line.
    let (first_line, rest) = claude.split_once('\n').unwrap_or((claude.as_str(), ""));
    assert_eq!(
        first_line, CLAUDE_TITLE,
        "CLAUDE.md must start with the `{CLAUDE_TITLE}` H1 title"
    );
    let retitled = format!("{AGENTS_TITLE}\n{rest}");

    // 2. Inject the counterpart note just before the first `## ` section, so
    //    it sits after the intro paragraphs and before Session Continuity.
    let marker = "\n## ";
    let idx = retitled
        .find(marker)
        .expect("CLAUDE.md must contain at least one `## ` section");
    let (head, tail) = retitled.split_at(idx);
    let with_note = format!("{head}\n{COUNTERPART_NOTE}\n{tail}");

    // 3. Append the Codex/OpenCode-only sections after the last CLAUDE.md
    //    section, separated by a blank line, with a single trailing newline.
    //    A trailing Orch-managed block (split off in step 0) is carried
    //    through verbatim and re-appended last of all, so it stays at EOF.
    let body = with_note.trim_end_matches('\n');
    match managed_block {
        Some(block) => {
            format!("{body}\n\n{AGENT_ATTRIBUTION}\n\n{ROUTING_BLOCK}\n\n{block}\n")
        }
        None => format!("{body}\n\n{AGENT_ATTRIBUTION}\n\n{ROUTING_BLOCK}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# memhub\n\nIntro line.\n\nPointer line.\n\n## Session Continuity\n\nBody.\n\n## Build / Test / Run\n\n```bash\ncargo build\n```\n";

    const MANAGED_BLOCK: &str = "<!-- orchestrator:managed:start version=1 -->\nManaged line one.\nManaged line two.\n<!-- orchestrator:managed:end -->";

    const SAMPLE_WITH_MANAGED_BLOCK: &str = "# memhub\n\nIntro line.\n\nPointer line.\n\n## Session Continuity\n\nBody.\n\n## Build / Test / Run\n\n```bash\ncargo build\n```\n\n<!-- orchestrator:managed:start version=1 -->\nManaged line one.\nManaged line two.\n<!-- orchestrator:managed:end -->";

    #[test]
    fn swaps_title_and_keeps_body_sections() {
        let out = generate_agents_md(SAMPLE);
        assert!(out.starts_with("# memhub — Codex CLI instructions\n"));
        assert!(!out.contains("\n# memhub\n"));
        assert!(out.contains("## Session Continuity"));
        assert!(out.contains("## Build / Test / Run"));
        // Intro content carries through verbatim.
        assert!(out.contains("Intro line."));
        assert!(out.contains("Pointer line."));
    }

    #[test]
    fn injects_counterpart_note_before_first_section() {
        let out = generate_agents_md(SAMPLE);
        let note_at = out.find("generated").expect("counterpart note present");
        let first_section = out.find("## Session Continuity").expect("first section");
        assert!(
            note_at < first_section,
            "counterpart note must precede the first ## section"
        );
    }

    #[test]
    fn appends_codex_only_sections_at_end() {
        let out = generate_agents_md(SAMPLE);
        let attrib = out
            .find("## Agent attribution (Codex-specific)")
            .expect("attribution section appended");
        let routing = out
            .find("## memhub routing (Codex / OpenCode)")
            .expect("routing block appended");
        let last_body = out.find("## Build / Test / Run").expect("body section");
        assert!(last_body < attrib, "injected sections come after the body");
        assert!(attrib < routing, "attribution precedes routing");
        assert!(
            out.ends_with("delivery spike.)\n"),
            "single trailing newline"
        );
    }

    #[test]
    fn is_line_ending_agnostic() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        assert_eq!(generate_agents_md(SAMPLE), generate_agents_md(&crlf));
    }

    #[test]
    fn keeps_trailing_managed_block_last() {
        let out = generate_agents_md(SAMPLE_WITH_MANAGED_BLOCK);
        let last_body = out.find("## Build / Test / Run").expect("body section");
        let attrib = out
            .find("## Agent attribution (Codex-specific)")
            .expect("attribution section appended");
        let routing = out
            .find("## memhub routing (Codex / OpenCode)")
            .expect("routing block appended");
        let managed = out
            .find(MANAGED_BLOCK_START)
            .expect("managed block carried through");
        assert!(last_body < attrib, "injected sections come after the body");
        assert!(attrib < routing, "attribution precedes routing");
        assert!(routing < managed, "managed block stays last, after routing");
        assert!(
            out.ends_with("<!-- orchestrator:managed:end -->\n"),
            "single trailing newline after the managed block"
        );
    }

    #[test]
    fn carries_managed_block_verbatim() {
        let out = generate_agents_md(SAMPLE_WITH_MANAGED_BLOCK);
        assert!(
            out.contains(MANAGED_BLOCK),
            "managed block content must be carried through unmodified"
        );
    }

    #[test]
    fn is_line_ending_agnostic_with_managed_block() {
        let crlf = SAMPLE_WITH_MANAGED_BLOCK.replace('\n', "\r\n");
        assert_eq!(
            generate_agents_md(SAMPLE_WITH_MANAGED_BLOCK),
            generate_agents_md(&crlf)
        );
    }
}
