//! `memhub wrapup-policy` (Wave 6 W1+W2, issue #95): a read-only command
//! that renders the full `/wrap-up` policy text for this repo's resolved
//! `[wrap_up] verbosity` level from one canonical Rust source, instead of
//! every agent re-deriving it by eye from `templates/skills/claude/wrap-up.md`,
//! `templates/skills/codex/wrap-up/SKILL.md`, and
//! `templates/skills/opencode/wrap-up/`.
//! No DB is opened — verbosity is a config-only value, same as
//! `audit_md::run` — and this command never writes anything.
//!
//! Level semantics (Q10/W2) are ported faithfully from the wrap-up flow
//! in those three skill files, not reinvented — see
//! [`crate::config::WrapUpVerbosity`] for the short summary and
//! [`render_instructions`] below for the full text:
//!   - `minimal`    — `state set` + task closures only.
//!   - `standard`   — today's eight-item flow, unchanged.
//!   - `full`       — standard, with the decision `--summary` field
//!                    (decision 72) promoted to mandatory whenever a
//!                    decision is drafted, plus pending-write triage
//!                    and the architecture-drift check promoted to
//!                    always-run. Facts have no `--summary` field to
//!                    promote (decision 72 is decisions-only).
//!   - `transcript` — full + a named transcript-archive step. The
//!                    archiver itself is issue #96 (W3); this level
//!                    only needs the step to exist in the policy text,
//!                    and renders gracefully whether or not the
//!                    archiver is implemented yet.

use std::path::Path;

use crate::Result;
use crate::config::{ProjectConfig, WrapUpVerbosity};
use crate::db;

#[derive(Debug, Clone)]
pub struct WrapupPolicyReport {
    pub verbosity: WrapUpVerbosity,
    pub instructions: String,
}

/// Resolve `[wrap_up] verbosity` from `.memhub/config.toml` (falling
/// back to the code default — `standard` — exactly like `audit_md::run`
/// does for a missing/unparseable config) and render its policy text.
/// Read-only: never opens `project.sqlite`.
pub fn run(start: &Path) -> Result<WrapupPolicyReport> {
    let paths = db::discover_paths(start)?;
    let config = ProjectConfig::load(&paths.config_path)
        .unwrap_or_else(|_| ProjectConfig::default_for_repo_name(repo_name(&paths.repo_root)));
    let verbosity = config.wrap_up.verbosity;
    Ok(WrapupPolicyReport {
        verbosity,
        instructions: render_instructions(verbosity),
    })
}

fn repo_name(repo_root: &Path) -> &str {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memhub")
}

/// Render the full wrap-up policy text for `level`. A pure function of
/// the level alone — no I/O — so every level is exhaustively
/// unit-testable without a repo fixture.
pub fn render_instructions(level: WrapUpVerbosity) -> String {
    let mut out = String::new();
    out.push_str(&header(level));
    out.push_str(DETECTION);
    out.push_str(&read_window(level));
    out.push_str(&draft_assembly(level));
    out.push_str(APPROVAL_GATE);
    out.push_str(&db_writes(level));
    out.push_str(RENDER_STEP);
    out.push_str(SYNC_PUSH);
    if level == WrapUpVerbosity::Transcript {
        out.push_str(TRANSCRIPT_ARCHIVE);
    }
    out.push_str(REMINDER);

    let trimmed = out.trim_end();
    let mut result = String::with_capacity(trimmed.len() + 1);
    result.push_str(trimmed);
    result.push('\n');
    result
}

fn header(level: WrapUpVerbosity) -> String {
    format!(
        "memhub wrap-up policy -- level: {}\n\n{}\n\n",
        level.as_str(),
        level_summary(level)
    )
}

fn level_summary(level: WrapUpVerbosity) -> &'static str {
    match level {
        WrapUpVerbosity::Minimal => {
            "Minimal is the turn-1 continuity floor: update the state narrative and close \
             finished tasks. Nothing else is drafted."
        }
        WrapUpVerbosity::Standard => {
            "Standard is memhub's original /wrap-up flow, unchanged: state, decisions, \
             backlog, facts, pending-write triage, a short session note, opportunistic \
             architecture drift, and stale-fact re-verify candidates."
        }
        WrapUpVerbosity::Full => {
            "Full is standard with the decision --summary field (decision 72's \
             natural-language paraphrase) promoted from optional to mandatory whenever a \
             decision is drafted, the architecture-drift check and pending-write triage \
             promoted from conditional to always-run, and a richer session note. Facts \
             have no --summary field to promote and are unchanged from standard."
        }
        WrapUpVerbosity::Transcript => {
            "Transcript is full plus a named transcript-archive step. The archiver itself \
             is tracked separately (issue #96); this level only needs the step to exist \
             in the policy."
        }
    }
}

const DETECTION: &str = "\
## Detection

Run once, at the top, and let the result gate everything below:

1. `.memhub/` exists in this repo -- if not, stop and tell the user to run `memhub init`
   first.
2. The `memhub` binary is on PATH -- if not, stop until it is.
3. The schema is current (`memhub status` shows the latest applied migration) -- if not,
   stop; wrapping up against a stale schema produces silently wrong rows.

Every memhub invocation below passes `--actor <agent>:wrap-up` so `writes_log`
distinguishes wrap-up writes from raw CLI use.

";

fn read_window(level: WrapUpVerbosity) -> String {
    match level {
        WrapUpVerbosity::Minimal => "\
## Read window

Capture only what minimal drafts:

1. `memhub state show --json` -- the current state narrative (or null for a fresh repo).
2. `memhub task list --status open` -- open work items, so closures can be matched to ids.
3. From the prior state row's `created_at` (or the last 10 commits if there is no prior
   state row), `git log --since=<that timestamp> --oneline` -- just enough to write an
   accurate state update.

"
        .to_string(),
        WrapUpVerbosity::Standard | WrapUpVerbosity::Full | WrapUpVerbosity::Transcript => "\
## Read window

Capture the boundary of \"this session\" implicitly: the most recent `project_state`
row's `created_at` is the previous wrap-up timestamp. Anything newer in the DB or git
history is in-window. Run, in order, and keep the JSON for draft assembly:

1. `memhub state show --json` -- the current state narrative (or null for a fresh repo).
2. `memhub arch show --json` -- the current architecture narrative.
3. `memhub note list --since-days 7 --json` -- recent session notes.
4. `memhub review list --status pending --json` -- staged proposals no human has
   reviewed yet.
5. `memhub task list --status open` -- open work items.
6. From the state row's `created_at` (or the last 10 commits if there is no prior state
   row), `git log --since=<that timestamp> --oneline`.
7. `git status --porcelain` -- uncommitted changes worth surfacing.

If `memhub status --json`'s `k9_detected` is true and the operator hasn't migrated,
note it informationally -- it never blocks.

"
        .to_string(),
    }
}

fn draft_assembly(level: WrapUpVerbosity) -> String {
    match level {
        WrapUpVerbosity::Minimal => "\
## Draft assembly

Two items only -- this is the continuity floor, not the full flow:

1. New `state` body. Currently building / next up / open questions, kept tight (well
   under the ~100-line render budget). Propose an update only if something real changed
   since the last wrap-up.
2. Task closures. For tasks finished this session, propose closing them, using the ids
   from the read window. New tasks discovered this session are noted in the state body,
   not drafted as separate task adds.

No decisions, no new facts, no pending-write triage, no session-summary note, no
architecture check, and no stale-fact re-verify candidates are drafted at this level.

"
        .to_string(),
        WrapUpVerbosity::Standard => "\
## Draft assembly

Synthesize eight things, drafted separately so each can be approved or rejected on its
own (memhub's original flow, unchanged):

1. New `state` body -- currently building / next up / open questions, kept tight (under
   ~100 lines).
2. New decisions -- architectural / workflow / contract decisions locked this session,
   each title + rationale.
3. Backlog changes -- new tasks discovered, status changes on existing tasks.
4. New facts -- durable key-value records that are NOT command invocations, skipping
   anything already recorded with the same value. A build/test/run/lint command you
   actually ran this session, with an observed exit code, is a verified command, not a
   fact -- route it to `memhub command verify` (CLI) / `record_command` (MCP) instead,
   go-forward only (do not backfill existing command-shaped facts).
5. Pending-write triage -- for each row from the read window, propose accept or reject
   with a one-line reason.
6. Session-summary note -- two to four sentences on what actually shipped, anchored to
   commit hashes where possible. Bias toward truth; say so plainly if the session was
   exploratory.
7. Architecture drift -- touch only if a real architectural shift occurred (new
   subsystem, schema change, invariant change); default is no arch update.
8. Stale-fact re-verify candidates -- run `memhub fact list --json` and pick up to 5
   facts ordered oldest-first by `verified_at` (null sorts as oldest), preferring rows
   already flagged `is_stale`. Skip this draft entirely if there are none. Present each
   as its own accept/reject item, never a single grouped prompt.

"
        .to_string(),
        WrapUpVerbosity::Full => format!(
            "\
## Draft assembly

The same eight items as `standard`. The decision `--summary` field is promoted from
optional to mandatory whenever a decision is drafted (facts have no `--summary` field
to promote), and pending-write triage plus the architecture-drift check are promoted
from conditional to always-run:

{}",
            MANDATORY_EIGHT_ITEMS
        ),
        WrapUpVerbosity::Transcript => format!(
            "\
## Draft assembly

The same eight items as `full`, plus a ninth:

{}9. Transcript archive. Archive this session's agent transcript after the DB writes and
   render below succeed. The archiver itself is tracked separately (issue #96 / W3) --
   if it is not yet available in this build, say so plainly and skip it without failing
   the rest of wrap-up.

",
            MANDATORY_EIGHT_ITEMS
        ),
    }
}

/// The `standard` eight-item list, fully spelled out (not
/// cross-referenced), with item 2 gaining a mandatory `--summary`
/// requirement (decision 72; decisions only -- facts have no
/// `--summary` field, see item 4) and items 5-7 promoted to
/// always-run/mandatory per `full`'s semantics. Shared by `full` and
/// `transcript` so the wording can never drift between the two levels
/// that both carry it.
const MANDATORY_EIGHT_ITEMS: &str = "\
1. New `state` body -- currently building / next up / open questions, kept tight (under
   ~100 lines).
2. New decisions -- architectural / workflow / contract decisions locked this session,
   each title + rationale. MANDATORY when any are drafted: also include `--summary`
   (decision 72's natural-language paraphrase that lifts recall). Still skip this item
   entirely when there is nothing to record -- mandatory applies to what you draft, not
   to inventing decisions.
3. Backlog changes -- new tasks discovered, status changes on existing tasks.
4. New facts -- durable key-value records that are NOT command invocations, skipping
   anything already recorded with the same value. Facts have no `--summary` field
   (decision 72 is decisions-only). A build/test/run/lint command you actually ran this
   session, with an observed exit code, is a verified command, not a fact -- route it to
   `memhub command verify` (CLI) / `record_command` (MCP) instead, go-forward only (do
   not backfill existing command-shaped facts). Otherwise unchanged from `standard`.
5. Pending-write triage -- MANDATORY: always run this pass and report its outcome, even
   'queue empty, nothing to triage' -- never silently skip it because it looked empty.
6. Session-summary note -- MANDATORY and richer: a fuller account of what shipped than
   the standard two-to-four-sentence version, still anchored to commit hashes where
   possible. Bias toward truth over polish.
7. Architecture drift -- MANDATORY CHECK: explicitly assess and report whether a real
   architectural shift occurred every time. The conclusion may still be 'no drift', but
   it must be stated, not assumed by omission.
8. Stale-fact re-verify candidates -- run `memhub fact list --json` and pick up to 5
   facts ordered oldest-first by `verified_at` (null sorts as oldest), preferring rows
   already flagged `is_stale`. Skip this draft entirely if there are none. Present each
   as its own accept/reject item, never a single grouped prompt.

";

const APPROVAL_GATE: &str = "\
## Approval gate

Show all drafts in one block, grouped by kind. The user approves, edits, or rejects
each item individually. Wait for explicit approval per item, or a clear 'all good',
before moving on. A rejected draft is dropped; do not retry without being told to.

";

fn db_writes(level: WrapUpVerbosity) -> String {
    let mut s = String::from(
        "\
## DB writes -- first, atomic per item, halt on failure

Once approved, invoke each write in this order, every command taking `--json --actor
<agent>:wrap-up`:

1. State (only if changed) -- `memhub state set <body> --json --actor <agent>:wrap-up`.
2. Pending-write promotions/rejections -- `memhub review accept <id>` / `memhub review
   reject <id> --reason <reason>`.
3. New decisions -- `memhub decision add <title> --rationale <rationale> --source
   user+agent:<agent>`.
4. New tasks + closures -- `memhub task add <title> --notes <notes>` / `memhub task done
   <id>`.
5. New facts -- `memhub fact add <key> <value> --source user+agent:<agent>`.
6. Session summary (always, unless rejected) -- `memhub note add <summary>`.
7. Architecture (only if approved this session) -- `memhub arch set <body>`.
8. Stale-fact re-verifications -- one `memhub fact verify <id>` call per approved fact,
   never a bulk pass.

For multi-line state or arch bodies, write to a temp file and pass `--from-file <path>`
instead of inlining.

Halt on the first non-zero exit. Do not retry, do not skip, do not proceed to render --
report which command failed and what stderr said. Writes that already succeeded are
durable; re-running wrap-up later picks up the rest.

",
    );

    match level {
        WrapUpVerbosity::Minimal => {
            s.push_str(
                "At minimal, only steps 1 (state) and 4 (task closures) apply -- steps 2, 3, \
                 5, 6, 7, and 8 have nothing drafted to write.\n\n",
            );
        }
        WrapUpVerbosity::Full | WrapUpVerbosity::Transcript => {
            s.push_str(
                "At this level, step 2 (pending-write triage) always runs and is reported \
                 even when the queue is empty, step 3 (decisions) must include --summary \
                 for anything drafted (facts have no --summary field), and step 6 (session \
                 summary) carries the richer note from draft assembly.\n\n",
            );
        }
        WrapUpVerbosity::Standard => {}
    }

    s
}

const RENDER_STEP: &str = "\
## Render

After all approved DB writes succeed, run `memhub render`. This emits fresh local
`PROJECT.md` and `PROJECT_LEDGER.md` from the new DB state, backing up the prior
versions under `.memhub/backups/rendered/<stamp>/`. Report what got written and any
backup paths.

";

const SYNC_PUSH: &str = "\
## Cross-machine sync push (only if enabled)

Run `memhub sync status --json`. If `enabled` is false, skip this section silently.
Otherwise: `memhub sync check` first, and stop to /catch-up instead of pushing if the
verdict is `drive-ahead` or `diverged`. If safe, `memhub sync snapshot` (writes directly
into the synced folder) then `memhub sync commit` to record the new baseline. If
snapshot fails or refuses, say so and do not run commit -- the local DB is unaffected
either way.

";

const TRANSCRIPT_ARCHIVE: &str = "\
## Transcript archive (transcript level only)

After the render step above, archive this session's agent transcript. memhub does not
yet ship the archiver (issue #96 / W3) as of this build -- check whether an archive
command exists before invoking it. If it does not exist yet, say so plainly and
continue; a missing archiver is not a failure of wrap-up itself.

";

const REMINDER: &str = "\
## Reminder, not a commit

Tell the user they can audit what landed via `memhub stats --window 7d` or `memhub note
list --since-days 1`, that the rendered files are local generated output and not meant
to be committed unless this repo explicitly opts into a tracked render path, and that
they can start a new session whenever. Do not run `git commit` -- that is the user's
call; the local-vs-shared boundary is intentional.
";

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -- render_instructions: pure, per-level content ---------------------

    #[test]
    fn minimal_names_the_continuity_floor_and_excludes_higher_levels() {
        let text = render_instructions(WrapUpVerbosity::Minimal);
        assert!(text.contains("level: minimal"));
        assert!(text.contains("Task closures."));
        assert!(text.contains("turn-1"));
        assert!(!text.contains("MANDATORY"), "{text}");
        assert!(!text.contains("Transcript archive"), "{text}");
    }

    #[test]
    fn standard_names_the_eight_item_flow_and_excludes_full_and_transcript_markers() {
        let text = render_instructions(WrapUpVerbosity::Standard);
        assert!(text.contains("level: standard"));
        assert!(text.contains("Synthesize eight things"));
        assert!(!text.contains("MANDATORY"), "{text}");
        assert!(!text.contains("Transcript archive"), "{text}");
    }

    #[test]
    fn full_promotes_four_items_to_mandatory_and_excludes_transcript_archive() {
        let text = render_instructions(WrapUpVerbosity::Full);
        assert!(text.contains("level: full"));
        // Pending-write triage, session note, and architecture check are
        // each called out MANDATORY (three call sites minimum).
        assert!(text.matches("MANDATORY").count() >= 3, "{text}");
        assert!(!text.contains("Transcript archive"), "{text}");
    }

    #[test]
    fn transcript_includes_full_markers_plus_the_named_archive_step() {
        let text = render_instructions(WrapUpVerbosity::Transcript);
        assert!(text.contains("level: transcript"));
        assert!(text.matches("MANDATORY").count() >= 3, "{text}");
        assert!(text.contains("Transcript archive"));
        assert!(text.contains("issue #96"));
    }

    #[test]
    fn minimal_is_shorter_than_standard_which_is_shorter_than_full() {
        let minimal = render_instructions(WrapUpVerbosity::Minimal);
        let standard = render_instructions(WrapUpVerbosity::Standard);
        let full = render_instructions(WrapUpVerbosity::Full);
        let transcript = render_instructions(WrapUpVerbosity::Transcript);
        assert!(minimal.len() < standard.len());
        assert!(standard.len() < full.len());
        assert!(full.len() < transcript.len());
    }

    #[test]
    fn rendered_text_ends_with_exactly_one_trailing_newline() {
        for level in [
            WrapUpVerbosity::Minimal,
            WrapUpVerbosity::Standard,
            WrapUpVerbosity::Full,
            WrapUpVerbosity::Transcript,
        ] {
            let text = render_instructions(level);
            assert!(text.ends_with('\n'));
            assert!(!text.ends_with("\n\n"), "level {level:?}: {text:?}");
        }
    }

    /// Q11 (issue #99 review): the facts item must not teach command-as-fact,
    /// and must route verified build/test/run/lint commands to `command
    /// verify` (CLI) / `record_command` (MCP) instead, go-forward only. Skips
    /// `minimal`, which drafts no facts at all.
    #[test]
    fn facts_item_routes_verified_commands_away_from_facts_at_every_level_that_drafts_them()
     {
        for level in [
            WrapUpVerbosity::Standard,
            WrapUpVerbosity::Full,
            WrapUpVerbosity::Transcript,
        ] {
            let text = render_instructions(level);
            assert!(
                !text.contains("build / test / run commands"),
                "level {level:?} still teaches command-as-fact: {text}"
            );
            assert!(
                text.contains("NOT command invocations"),
                "level {level:?}: {text}"
            );
            assert!(
                text.contains("memhub command verify") && text.contains("record_command"),
                "level {level:?} missing Q11 routing to command verify/record_command: {text}"
            );
            assert!(
                text.contains("go-forward only"),
                "level {level:?}: {text}"
            );
        }
    }

    // -- run(): config resolution ------------------------------------------

    #[test]
    fn run_on_a_fresh_repo_resolves_the_standard_default() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");

        let report = run(temp.path()).expect("wrapup-policy");
        assert_eq!(report.verbosity, WrapUpVerbosity::Standard);
        assert!(report.instructions.contains("level: standard"));
    }

    #[test]
    fn run_reflects_a_configured_verbosity_round_trip() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");

        let paths = db::discover_paths(temp.path()).expect("discover paths");
        let mut config = ProjectConfig::load(&paths.config_path).expect("load config");
        config.wrap_up.verbosity = WrapUpVerbosity::Full;
        config.save(&paths.config_path).expect("save config");

        let report = run(temp.path()).expect("wrapup-policy");
        assert_eq!(report.verbosity, WrapUpVerbosity::Full);
        assert!(report.instructions.contains("level: full"));
    }
}
