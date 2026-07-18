//! `memhub audit md` (Wave 2 C5, issue #32 / decisions Q25 + Q29): a
//! read-only linter over the repo's root memory files that turns
//! CLAUDE.md/AGENTS.md drift and bloat — today caught only by chance in
//! review — into detectable, reported findings.
//!
//! **Exit-code contract (deliberately unlike `doctor`):** default exit
//! is always `0`, findings are printed either way; `--strict` exits `1`
//! iff at least one finding fired, regardless of its severity. Unlike
//! `doctor`'s `Status` (`Ok`/`Warn`/`Error`/`Skipped`, where severity
//! itself gates the exit code), `Severity` here (`Warn`/`Error`) is
//! purely a display label — every entry in `findings` is already a
//! problem, so there is no `Ok`/`Skipped` case to report. This is a CLI
//! surface only — no MCP tool, no DB open, no DB writes; it reads
//! `CLAUDE.md` / `AGENTS.md` / (opt-in) a user-global orientation file
//! straight off disk.
//!
//! Reuses rather than reimplements: [`crate::agents_md::generate_agents_md`]
//! (issue #30) for the drift check, [`crate::managed_block::parse_managed_block`]
//! (issue #31) for the managed-block check, and the same
//! [`CLAUDE_KEYSTONE_PHRASES`] set `tests/skill_parity.rs` asserts against
//! (issue #30) — that test now imports this const rather than keeping its
//! own copy, so the two can never silently drift from each other.

use std::fs;
use std::path::Path;

use crate::agents_md::{CLAUDE_TITLE, generate_agents_md, split_trailing_managed_block};
use crate::commands::sync::expand_home;
use crate::config::ProjectConfig;
use crate::db;
use crate::managed_block::{self, parse_managed_block};
use crate::metrics::tokenizer::tokens_of;
use crate::Result;

const CLAUDE_MD_FILENAME: &str = "CLAUDE.md";
const AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Target token budget for `CLAUDE.md` (issue #30 / decision Q22).
pub const CLAUDE_MD_TARGET_TOKENS: usize = 2_500;
/// Hard ceiling before a size finding is `error` rather than `warn`
/// (issue #30 acceptance criteria: "CLAUDE.md <= 2,600 cl100k tokens").
pub const CLAUDE_MD_HARD_CEILING_TOKENS: usize = 2_600;

/// N4 keystone phrases (issue #30) that must survive the CLAUDE.md token
/// diet — each is a safety gate, an identity line, or a guardrail an
/// agent must see inline. This is the single source of truth: both this
/// audit's keystone check and `tests/skill_parity.rs`'s
/// `claude_md_keeps_keystone_phrases` import this same const, so the two
/// can never drift apart.
pub const CLAUDE_KEYSTONE_PHRASES: &[&str] = &[
    "stale_embeddings",
    "sync_adopt",
    "Agents are untrusted writers",
    "memhub-primary",
    "Local-first",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warn,
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub id: &'static str,
    pub severity: Severity,
    pub message: String,
    pub detail: Option<String>,
}

impl Finding {
    fn new(id: &'static str, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            id,
            severity,
            message: message.into(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct AuditMdReport {
    pub findings: Vec<Finding>,
    pub exit_code: i32,
}

/// Run every check against the repo discovered from `start` (same
/// `.memhub`-upward walk every other subcommand uses) and build the
/// report. Read-only: at most a config-file read, never a DB open.
pub fn run(start: &Path, strict: bool) -> Result<AuditMdReport> {
    let paths = db::discover_paths(start)?;
    let config = ProjectConfig::load(&paths.config_path)
        .unwrap_or_else(|_| ProjectConfig::default_for_repo_name(repo_name(&paths.repo_root)));

    let mut findings = Vec::new();

    let claude_path = paths.repo_root.join(CLAUDE_MD_FILENAME);
    match fs::read_to_string(&claude_path) {
        Ok(claude_md) => {
            findings.extend(check_size("claude_md_size", CLAUDE_MD_FILENAME, &claude_md));
            findings.extend(check_agents_md_drift(&paths.repo_root, &claude_md));
            findings.extend(check_managed_block(&claude_md));
            findings.extend(check_keystones(&claude_md));
        }
        Err(e) => {
            findings.push(Finding::new(
                "claude_md_missing",
                Severity::Error,
                format!("cannot read {}: {e}", claude_path.display()),
            ));
        }
    }

    if !config.audit.user_md_path.trim().is_empty() {
        findings.extend(check_user_md(&config.audit.user_md_path));
    }

    Ok(build_report(findings, strict))
}

fn repo_name(repo_root: &Path) -> &str {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memhub")
}

fn build_report(findings: Vec<Finding>, strict: bool) -> AuditMdReport {
    let exit_code = if strict && !findings.is_empty() { 1 } else { 0 };
    AuditMdReport { findings, exit_code }
}

// ---------------------------------------------------------------------
// Size (target ~2,500 tokens, hard ceiling 2,600 — issue #30)
// ---------------------------------------------------------------------

/// Pure boundary classification, split out from [`check_size`] so the
/// target/ceiling edges can be unit-tested with exact integers instead
/// of a hand-crafted string whose real cl100k count is fragile to pin.
fn classify_size(tokens: usize, target: usize, ceiling: usize) -> Option<Severity> {
    if tokens > ceiling {
        Some(Severity::Error)
    } else if tokens > target {
        Some(Severity::Warn)
    } else {
        None
    }
}

fn check_size(id: &'static str, label: &str, content: &str) -> Option<Finding> {
    let tokens = tokens_of(content);
    let severity = classify_size(tokens, CLAUDE_MD_TARGET_TOKENS, CLAUDE_MD_HARD_CEILING_TOKENS)?;
    let message = match severity {
        Severity::Error => format!(
            "{label} is {tokens} cl100k tokens — over the {CLAUDE_MD_HARD_CEILING_TOKENS} hard \
             ceiling (target ~{CLAUDE_MD_TARGET_TOKENS})"
        ),
        Severity::Warn => format!(
            "{label} is {tokens} cl100k tokens — over the ~{CLAUDE_MD_TARGET_TOKENS} target \
             (hard ceiling {CLAUDE_MD_HARD_CEILING_TOKENS})"
        ),
    };
    Some(Finding::new(id, severity, message))
}

// ---------------------------------------------------------------------
// AGENTS.md drift (issue #30's generate_agents_md)
// ---------------------------------------------------------------------

/// Mirrors the two preconditions `generate_agents_md` itself `assert!`s
/// on (first line == [`CLAUDE_TITLE`]; a `## ` section exists) without
/// duplicating the transform logic — just enough to call it safely.
/// `generate_agents_md` is frozen/reused as-is (issue #30), so a
/// malformed `CLAUDE.md` must be caught *before* calling it rather than
/// letting a read-only linter crash the CLI on a programmer-error-style
/// `assert!`.
///
/// Checks the *stripped* text — after a trailing Orch-managed block is
/// split off via [`split_trailing_managed_block`] — because that's what
/// `generate_agents_md` actually runs its `## ` search against (its own
/// step 0 does the same split). Checking the raw file here previously let
/// a `CLAUDE.md` whose only `## ` heading lived inside that managed block
/// pass this precondition and then panic inside `generate_agents_md`,
/// where the block (and its heading) had already been stripped off
/// (issue #148 / audit C3).
fn generate_agents_md_preconditions_met(claude_md: &str) -> bool {
    let (claude, _) = split_trailing_managed_block(claude_md);
    let Some((first_line, rest)) = claude.split_once('\n') else {
        return false;
    };
    first_line == CLAUDE_TITLE && (rest.starts_with("## ") || rest.contains("\n## "))
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn check_agents_md_drift(repo_root: &Path, claude_md: &str) -> Option<Finding> {
    if !generate_agents_md_preconditions_met(claude_md) {
        return Some(Finding::new(
            "claude_md_malformed",
            Severity::Error,
            format!(
                "{CLAUDE_MD_FILENAME} does not start with the `{CLAUDE_TITLE}` H1 title or has \
                 no `## ` section — cannot generate/verify {AGENTS_MD_FILENAME}"
            ),
        ));
    }

    let generated = generate_agents_md(claude_md);
    let agents_path = repo_root.join(AGENTS_MD_FILENAME);
    let agents_md = match fs::read_to_string(&agents_path) {
        Ok(s) => s,
        Err(_) => {
            return Some(Finding::new(
                "agents_md_drift",
                Severity::Error,
                format!(
                    "{AGENTS_MD_FILENAME} not found at {} — generate it with MEMHUB_REGEN=1 \
                     cargo test skill_parity",
                    agents_path.display()
                ),
            ));
        }
    };

    if normalize_newlines(&agents_md) == normalize_newlines(&generated) {
        None
    } else {
        Some(Finding::new(
            "agents_md_drift",
            Severity::Error,
            format!(
                "{AGENTS_MD_FILENAME} does not match generate_agents_md({CLAUDE_MD_FILENAME}) — \
                 regenerate with MEMHUB_REGEN=1 cargo test skill_parity and commit \
                 {AGENTS_MD_FILENAME}"
            ),
        ))
    }
}

// ---------------------------------------------------------------------
// Managed block (issue #31's parse_managed_block)
// ---------------------------------------------------------------------

fn check_managed_block(claude_md: &str) -> Option<Finding> {
    match parse_managed_block(claude_md) {
        None => Some(Finding::new(
            "managed_block_missing",
            Severity::Warn,
            format!("no memhub:managed-block found in {CLAUDE_MD_FILENAME}"),
        )),
        Some(block) if block.version < managed_block::MANAGED_BLOCK_VERSION => Some(Finding::new(
            "managed_block_version",
            Severity::Warn,
            format!(
                "managed block is v{} — this build expects v{}",
                block.version,
                managed_block::MANAGED_BLOCK_VERSION
            ),
        )),
        Some(_) => None,
    }
}

// ---------------------------------------------------------------------
// Keystone phrases (issue #30's N4 set)
// ---------------------------------------------------------------------

fn check_keystones(claude_md: &str) -> Option<Finding> {
    let missing: Vec<&str> = CLAUDE_KEYSTONE_PHRASES
        .iter()
        .filter(|phrase| !claude_md.contains(**phrase))
        .copied()
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(
            Finding::new(
                "keystone_phrases",
                Severity::Error,
                format!(
                    "{} keystone phrase(s) missing from {CLAUDE_MD_FILENAME}",
                    missing.len()
                ),
            )
            .with_detail(missing.join(", ")),
        )
    }
}

// ---------------------------------------------------------------------
// User-global orientation file (opt-in, decision Q25)
// ---------------------------------------------------------------------

fn check_user_md(configured_path: &str) -> Option<Finding> {
    let expanded = match expand_home(configured_path) {
        Ok(p) => p,
        Err(e) => {
            return Some(Finding::new(
                "user_md_unreadable",
                Severity::Warn,
                format!("[audit] user_md_path {configured_path:?} could not be resolved: {e}"),
            ));
        }
    };
    match fs::read_to_string(&expanded) {
        Ok(content) => check_size("user_md_size", &expanded.display().to_string(), &content),
        Err(e) => Some(Finding::new(
            "user_md_unreadable",
            Severity::Warn,
            format!(
                "[audit] user_md_path {} could not be read: {e}",
                expanded.display()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // -- classify_size / check_size ----------------------------------

    #[test]
    fn classify_size_boundaries() {
        assert_eq!(classify_size(0, 2_500, 2_600), None);
        assert_eq!(classify_size(2_500, 2_500, 2_600), None, "at target: not over");
        assert_eq!(
            classify_size(2_501, 2_500, 2_600),
            Some(Severity::Warn)
        );
        assert_eq!(
            classify_size(2_600, 2_500, 2_600),
            Some(Severity::Warn),
            "at ceiling: not over yet"
        );
        assert_eq!(
            classify_size(2_601, 2_500, 2_600),
            Some(Severity::Error)
        );
    }

    #[test]
    fn check_size_none_for_short_content() {
        assert!(check_size("x", "label", "short content").is_none());
    }

    #[test]
    fn check_size_fires_and_reports_tokens_in_message() {
        // Comfortably over the 2,500-token target. Whether this exact
        // repeat count lands in the warn or error band depends on the
        // real tokenizer's tokens-per-word ratio and is not this test's
        // concern — `classify_size_boundaries` above already pins the
        // warn/error edges with exact integers. This just proves
        // `check_size` wires `tokens_of`'s real count into the message.
        let content = "word ".repeat(2_600);
        let finding = check_size("claude_md_size", "CLAUDE.md", &content).expect("finding");
        assert!(finding.message.contains("cl100k tokens"));
        assert!(finding.message.contains("CLAUDE.md"));
    }

    // -- generate_agents_md_preconditions_met / check_agents_md_drift --

    const VALID_CLAUDE_MD: &str = "# memhub\n\nIntro.\n\n## Session Continuity\n\nBody.\n";

    #[test]
    fn preconditions_met_for_well_formed_claude_md() {
        assert!(generate_agents_md_preconditions_met(VALID_CLAUDE_MD));
    }

    #[test]
    fn preconditions_fail_without_the_h1_title() {
        assert!(!generate_agents_md_preconditions_met(
            "# something else\n\n## Section\n"
        ));
    }

    #[test]
    fn preconditions_fail_without_a_section_marker() {
        assert!(!generate_agents_md_preconditions_met("# memhub\n\nno sections here\n"));
    }

    /// Regression (issue #148 / audit C3): a CLAUDE.md whose only `## `
    /// heading lives inside the trailing Orch-managed block used to pass
    /// this precondition check (which scanned the raw file) and then
    /// panic inside `generate_agents_md`, which strips the managed block
    /// (and, with it, that heading) before searching for `## `.
    #[test]
    fn preconditions_fail_when_only_heading_is_inside_managed_block() {
        let claude = "# memhub\n\nIntro.\n\n\
             <!-- orchestrator:managed:start version=1 -->\n\
             ## Inside the block\nmanaged line\n\
             <!-- orchestrator:managed:end -->\n";
        assert!(!generate_agents_md_preconditions_met(claude));
    }

    /// End-to-end regression for the same bug: `memhub audit md` must
    /// report `claude_md_malformed`, not panic, on such a file.
    #[test]
    fn audit_md_does_not_panic_when_only_heading_is_inside_managed_block() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let claude = "# memhub\n\nIntro.\n\n\
             <!-- orchestrator:managed:start version=1 -->\n\
             ## Inside the block\nmanaged line\n\
             <!-- orchestrator:managed:end -->\n";
        fs::write(temp.path().join(CLAUDE_MD_FILENAME), claude).expect("write claude");

        let report = run(temp.path(), false).expect("audit");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.id == "claude_md_malformed"),
            "{:#?}",
            report.findings
        );
    }

    #[test]
    fn drift_none_when_agents_md_matches_generated() {
        let temp = tempdir().expect("tempdir");
        let generated = generate_agents_md(VALID_CLAUDE_MD);
        fs::write(temp.path().join(AGENTS_MD_FILENAME), &generated).expect("write agents");

        assert!(check_agents_md_drift(temp.path(), VALID_CLAUDE_MD).is_none());
    }

    #[test]
    fn drift_flagged_when_agents_md_missing() {
        let temp = tempdir().expect("tempdir");
        let finding = check_agents_md_drift(temp.path(), VALID_CLAUDE_MD).expect("finding");
        assert_eq!(finding.id, "agents_md_drift");
    }

    #[test]
    fn drift_flagged_when_agents_md_content_differs() {
        let temp = tempdir().expect("tempdir");
        fs::write(temp.path().join(AGENTS_MD_FILENAME), "stale content\n").expect("write agents");

        let finding = check_agents_md_drift(temp.path(), VALID_CLAUDE_MD).expect("finding");
        assert_eq!(finding.id, "agents_md_drift");
    }

    #[test]
    fn malformed_claude_md_short_circuits_before_reading_agents_md() {
        let temp = tempdir().expect("tempdir");
        let finding = check_agents_md_drift(temp.path(), "not a valid claude md\n").expect("finding");
        assert_eq!(finding.id, "claude_md_malformed");
    }

    // -- check_managed_block ------------------------------------------

    #[test]
    fn managed_block_missing_is_a_warn() {
        let finding = check_managed_block("# memhub\n\nno block here\n").expect("finding");
        assert_eq!(finding.id, "managed_block_missing");
        assert_eq!(finding.severity, Severity::Warn);
    }

    #[test]
    fn managed_block_current_version_is_clean() {
        let claude = format!(
            "# memhub\n\n<!-- memhub:managed-block v={} -->\nmemhub-primary: true\n\
             <!-- /memhub:managed-block -->\n",
            managed_block::MANAGED_BLOCK_VERSION
        );
        assert!(check_managed_block(&claude).is_none());
    }

    #[test]
    fn managed_block_old_version_is_a_warn() {
        let claude =
            "<!-- memhub:managed-block v=0 -->\nk: v\n<!-- /memhub:managed-block -->\n";
        let finding = check_managed_block(claude).expect("finding");
        assert_eq!(finding.id, "managed_block_version");
        assert_eq!(finding.severity, Severity::Warn);
    }

    // -- check_keystones ------------------------------------------------

    #[test]
    fn keystones_all_present_is_clean() {
        let claude = CLAUDE_KEYSTONE_PHRASES.join(" ");
        assert!(check_keystones(&claude).is_none());
    }

    #[test]
    fn keystones_missing_one_is_an_error_with_detail() {
        let claude = CLAUDE_KEYSTONE_PHRASES[1..].join(" ");
        let finding = check_keystones(&claude).expect("finding");
        assert_eq!(finding.id, "keystone_phrases");
        assert_eq!(finding.severity, Severity::Error);
        assert!(
            finding
                .detail
                .expect("detail")
                .contains(CLAUDE_KEYSTONE_PHRASES[0])
        );
    }

    // -- run() end to end (unit-level; CLI/--json coverage lives in
    //    tests/audit_md.rs) --------------------------------------------

    fn clean_claude_md_fixture() -> String {
        format!(
            "# memhub\n\nLocal-first. Agents are untrusted writers.\n\n\
             <!-- memhub:managed-block v={} -->\nmemhub-primary: true\ndb: .memhub/project.sqlite\n\
             rendered: .memhub/rendered/\nconfig: .memhub/config.toml\n\
             <!-- /memhub:managed-block -->\n\n## Session Continuity\n\n\
             stale_embeddings gate. sync_adopt gate.\n",
            managed_block::MANAGED_BLOCK_VERSION
        )
    }

    #[test]
    fn run_on_a_clean_repo_reports_zero_findings_and_exits_zero() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");

        let claude = clean_claude_md_fixture();
        let generated = generate_agents_md(&claude);
        fs::write(temp.path().join(CLAUDE_MD_FILENAME), &claude).expect("write claude");
        fs::write(temp.path().join(AGENTS_MD_FILENAME), &generated).expect("write agents");

        let report = run(temp.path(), false).expect("audit");
        assert!(report.findings.is_empty(), "{:#?}", report.findings);
        assert_eq!(report.exit_code, 0);
    }

    #[test]
    fn run_strict_exits_nonzero_only_when_a_finding_fires() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        fs::write(temp.path().join(CLAUDE_MD_FILENAME), "not valid\n").expect("write claude");

        let plain = run(temp.path(), false).expect("audit");
        assert!(!plain.findings.is_empty());
        assert_eq!(
            plain.exit_code, 0,
            "non-strict must stay 0 even with findings"
        );

        let strict = run(temp.path(), true).expect("audit");
        assert_eq!(strict.exit_code, 1);
    }

    #[test]
    fn run_default_never_reads_user_md_path_when_unset() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let claude = clean_claude_md_fixture();
        let generated = generate_agents_md(&claude);
        fs::write(temp.path().join(CLAUDE_MD_FILENAME), &claude).expect("write claude");
        fs::write(temp.path().join(AGENTS_MD_FILENAME), &generated).expect("write agents");

        let report = run(temp.path(), false).expect("audit");
        assert!(
            !report.findings.iter().any(|f| f.id.starts_with("user_md")),
            "default config must never produce a user_md_* finding: {:#?}",
            report.findings
        );
    }

    #[test]
    fn run_opts_in_to_user_md_size_check_when_configured() {
        let temp = tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let claude = clean_claude_md_fixture();
        let generated = generate_agents_md(&claude);
        fs::write(temp.path().join(CLAUDE_MD_FILENAME), &claude).expect("write claude");
        fs::write(temp.path().join(AGENTS_MD_FILENAME), &generated).expect("write agents");

        let oversized_user_md = temp.path().join("user-global-CLAUDE.md");
        fs::write(&oversized_user_md, "word ".repeat(2_600)).expect("write user md");

        let paths = db::discover_paths(temp.path()).expect("discover");
        let mut config = ProjectConfig::load(&paths.config_path).expect("load config");
        config.audit.user_md_path = oversized_user_md.display().to_string();
        config.save(&paths.config_path).expect("save config");

        let report = run(temp.path(), false).expect("audit");
        assert!(
            report.findings.iter().any(|f| f.id == "user_md_size"),
            "{:#?}",
            report.findings
        );
    }
}
