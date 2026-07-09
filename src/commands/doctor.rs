//! `memhub doctor` (Wave 1·A, issue #21): one read-only health command
//! that turns memhub's comment-enforced invariants and scattered health
//! signals into detectable, reported states. Absorbs `/check-init`,
//! adds config validation, the D1 integrity surface, the X4 metrics
//! health line, and probes P1 (per-CLI MCP registration) and P4 (sync
//! freshness).
//!
//! **Contract frozen by the main thread — do not redesign:** exit `0`
//! unless an `error`-level check fires (`1` otherwise); `--strict`
//! additionally fails on any `warn`. CLI-only, no MCP tool. Read-only —
//! checks are `SELECT`/`PRAGMA`, except the FTS5 integrity check, which
//! runs the special `INSERT INTO {fts}({fts}) VALUES('integrity-check')`
//! command; that is FTS5's consistency-scan syntax, not a row write —
//! nothing here persists new data.
//!
//! Each check is a reusable, side-effect-free function returning a
//! [`Check`] (plain data, no printing) so `commands::status`'s own
//! refresh (a separate issue) can reuse a subset without pulling in any
//! rendering. Rendering (human text / `--json`) lives in
//! `src/cli/output.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::commands::{index, integrations, sync};
use crate::config::{IntegrationsConfig, ProjectConfig, RetrievalMode};
use crate::db;
use crate::Result;

/// Ordered worst-to-best so `overall` is `checks.iter().map(|c|
/// c.status).max()`. Declaration order is the derive order:
/// `Skipped < Ok < Warn < Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Skipped,
    Ok,
    Warn,
    Error,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Skipped => "skipped",
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Project,
    Config,
    Integrity,
    RetrievalMetrics,
    Integrations,
}

impl Group {
    pub fn as_str(&self) -> &'static str {
        match self {
            Group::Project => "project",
            Group::Config => "config",
            Group::Integrity => "integrity",
            Group::RetrievalMetrics => "retrieval_metrics",
            Group::Integrations => "integrations",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub id: &'static str,
    pub group: Group,
    pub status: Status,
    pub message: String,
    pub detail: Option<String>,
}

impl Check {
    fn new(id: &'static str, group: Group, status: Status, message: impl Into<String>) -> Self {
        Self {
            id,
            group,
            status,
            message: message.into(),
            detail: None,
        }
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct DoctorCounts {
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub project: String,
    pub overall: Status,
    pub exit_code: i32,
    pub counts: DoctorCounts,
    pub checks: Vec<Check>,
}

pub fn run(start: &Path, strict: bool) -> Result<DoctorReport> {
    run_with_home(start, strict, db::home_dir().ok())
}

/// Core orchestration. `home` is injected (rather than resolved inline)
/// so the P1 MCP-registration checks are hermetically testable — they
/// must never depend on the ambient state of the machine running
/// `cargo test`.
fn run_with_home(start: &Path, strict: bool, home: Option<PathBuf>) -> Result<DoctorReport> {
    let ctx = db::open_project(start)?;
    let mut checks = Vec::new();

    // Project (absorbs /check-init).
    checks.push(check_schema(&ctx.conn));
    checks.push(check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config));
    checks.push(check_k9_coexistence(
        &ctx.paths.repo_root,
        &ctx.config.integrations,
    ));
    checks.push(check_writes_log_recency(&ctx.conn));

    // Config.
    checks.extend(check_config(&ctx.paths.config_path, &ctx.paths.memhub_dir));

    // Integrity (D1).
    checks.extend(check_integrity(&ctx.conn));

    // Retrieval / Metrics (X4).
    checks.push(check_retrieval_mode(&ctx.config));
    checks.push(check_embeddings_freshness(start, &ctx.config));
    checks.push(check_metrics_health(&ctx.conn, &ctx.config));

    // Integrations (P1 report-only, P4).
    checks.push(check_mcp_claude(&ctx.paths.repo_root, home.as_deref()));
    checks.push(check_mcp_codex(home.as_deref()));
    checks.push(check_mcp_opencode(&ctx.paths.repo_root, home.as_deref()));
    checks.push(check_sync_freshness(start, &ctx.config));

    Ok(build_report(&ctx.config.project_name, checks, strict))
}

fn build_report(project_name: &str, checks: Vec<Check>, strict: bool) -> DoctorReport {
    let mut counts = DoctorCounts::default();
    for c in &checks {
        match c.status {
            Status::Ok => counts.ok += 1,
            Status::Warn => counts.warn += 1,
            Status::Error => counts.error += 1,
            Status::Skipped => counts.skipped += 1,
        }
    }

    let overall = checks
        .iter()
        .map(|c| c.status)
        .filter(|s| *s != Status::Skipped)
        .max()
        .unwrap_or(Status::Ok);

    let exit_code = if counts.error > 0 || (strict && counts.warn > 0) {
        1
    } else {
        0
    };

    DoctorReport {
        project: project_name.to_string(),
        overall,
        exit_code,
        counts,
        checks,
    }
}

// ---------------------------------------------------------------------
// Project group
// ---------------------------------------------------------------------

// Visibility note (issue #22, Wave 1·C): the functions below are
// `pub(crate)` rather than private specifically so `commands::status`
// can call them directly instead of duplicating their logic. Only the
// signature/visibility line changes — bodies are untouched so this
// stays a clean merge alongside any later change to a check's
// internals (e.g. `check_sync_freshness`).
pub(crate) fn check_schema(conn: &Connection) -> Check {
    let applied: String = conn
        .query_row("SELECT schema_version FROM projects WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let head = db::latest_schema_version();

    if applied == head {
        Check::new(
            "schema",
            Group::Project,
            Status::Ok,
            format!("at head ({head})"),
        )
    } else {
        // Not reachable through `db::open_project` today (it migrates
        // forward on every connect, or refuses outright if the DB is
        // newer than this binary) — kept as a defensive, cheap
        // confirmation rather than removed, since `status`'s refresh
        // (a separate issue) reuses this same function.
        Check::new(
            "schema",
            Group::Project,
            Status::Error,
            format!("schema {applied} is behind head {head} — run `memhub upgrade`"),
        )
    }
}

const RENDER_TABLE_NAME: &str = "render";
const PROJECT_MD_FILENAME: &str = "PROJECT.md";

pub(crate) fn check_render_freshness(conn: &Connection, repo_root: &Path, config: &ProjectConfig) -> Check {
    let output_dir = repo_root.join(&config.render.output_dir);
    let exists = output_dir.join(PROJECT_MD_FILENAME).is_file();

    // Ordered by `id`, not `at`: `writes_log.at` is `CURRENT_TIMESTAMP`
    // (one-second resolution), so a write logged in the same wall-clock
    // second as the render marker would be indistinguishable from it —
    // or worse, invisible to a `>` comparison — under a timestamp
    // comparison. `id` is a strictly monotonic autoincrement, so it
    // orders correctly regardless of timing.
    let last_render_id: Option<i64> = conn
        .query_row(
            "SELECT MAX(id) FROM writes_log WHERE table_name = ?1",
            params![RENDER_TABLE_NAME],
            |r| r.get(0),
        )
        .unwrap_or(None);

    let writes_since: i64 = match last_render_id {
        Some(id) => conn
            .query_row(
                "SELECT COUNT(*) FROM writes_log WHERE id > ?1 AND table_name != ?2",
                params![id, RENDER_TABLE_NAME],
                |r| r.get(0),
            )
            .unwrap_or(0),
        None => conn
            .query_row(
                "SELECT COUNT(*) FROM writes_log WHERE table_name != ?1",
                params![RENDER_TABLE_NAME],
                |r| r.get(0),
            )
            .unwrap_or(0),
    };

    match (exists, last_render_id.is_some(), writes_since) {
        (true, true, 0) => Check::new(
            "render_freshness",
            Group::Project,
            Status::Ok,
            "rendered output is current",
        ),
        (true, true, n) => Check::new(
            "render_freshness",
            Group::Project,
            Status::Warn,
            format!("{n} durable write(s) since last render — run `memhub render`"),
        ),
        (_, false, _) => Check::new(
            "render_freshness",
            Group::Project,
            Status::Warn,
            "never rendered — run `memhub render`",
        ),
        (false, true, n) => Check::new(
            "render_freshness",
            Group::Project,
            Status::Warn,
            format!(
                "{PROJECT_MD_FILENAME} missing despite a recorded render \
                 ({n} write(s) since) — run `memhub render`"
            ),
        ),
    }
}

pub(crate) fn check_k9_coexistence(repo_root: &Path, integrations_cfg: &IntegrationsConfig) -> Check {
    let state = integrations::k9_state(repo_root, integrations_cfg);

    // `drift` is checked before `detected`: the one case it fires with
    // `detected == false` (K9 enabled in config but the markdown is
    // missing) is a real, actionable mismatch, not a "nothing to see
    // here" state — it must not be swallowed by the not-detected skip.
    if let Some(drift) = &state.drift {
        return Check::new("k9_coexistence", Group::Project, Status::Warn, drift.clone());
    }
    if !state.detected {
        return Check::new(
            "k9_coexistence",
            Group::Project,
            Status::Skipped,
            "K9 not detected",
        );
    }
    if state.enabled {
        Check::new(
            "k9_coexistence",
            Group::Project,
            Status::Ok,
            format!("K9 integrated (agent_docs_path: {})", state.agent_docs_path),
        )
    } else {
        Check::new(
            "k9_coexistence",
            Group::Project,
            Status::Skipped,
            "K9 detected but integration disabled (archived; markdown no longer authoritative)",
        )
    }
}

fn check_writes_log_recency(conn: &Connection) -> Check {
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM writes_log", [], |r| r.get(0))
        .unwrap_or(0);
    if total == 0 {
        return Check::new(
            "writes_log_recency",
            Group::Project,
            Status::Ok,
            "no writes recorded yet",
        );
    }

    let last_at: Option<String> = conn
        .query_row("SELECT MAX(at) FROM writes_log", [], |r| r.get(0))
        .unwrap_or(None);
    match last_at {
        Some(ts) => Check::new(
            "writes_log_recency",
            Group::Project,
            Status::Ok,
            format!("{total} write(s) logged; last write at {ts}"),
        ),
        None => Check::new(
            "writes_log_recency",
            Group::Project,
            Status::Ok,
            format!("{total} write(s) logged"),
        ),
    }
}

// ---------------------------------------------------------------------
// Config group
// ---------------------------------------------------------------------

/// Keys `ProjectConfig` actually understands, as dotted leaf paths.
/// Kept in sync by hand with `src/config/mod.rs` — a new field there
/// needs a new entry here or it reads as "unknown".
const KNOWN_LEAVES: &[&str] = &[
    "project_name",
    "auto_sync_md",
    "log_level",
    "deny_list.patterns",
    "integrations.k9.enabled",
    "integrations.k9.agent_docs_path",
    "render.output_dir",
    "retrieval.mode",
    "retrieval.default_max_results",
    "retrieval.accepted_only_by_default",
    "retrieval.include_stale_by_default",
    "retrieval.fact_stale_after_days",
    "retrieval.use_reranker",
    "retrieval.rerank_candidate_pool",
    "retrieval.include_docs_in_default",
    "retrieval.scoring.fts_weight",
    "retrieval.scoring.vector_weight",
    "retrieval.scoring.stale_penalty",
    "retrieval.scoring.superseded_penalty",
    "retrieval.scoring.age_half_life_days",
    "retrieval.scoring.min_rerank_score",
    "retrieval.scoring.doc_min_rerank_score",
    "code_index.fts_weight",
    "code_index.vector_weight",
    "code_index.test_path_penalty",
    "metrics.enabled",
    "metrics.recall_proxy",
    "metrics.session_accounting",
    "metrics.claude_transcripts_dir",
    "metrics.codex_transcripts_dir",
    "metrics.tokenizer",
    "metrics.retention_days",
    "metrics.calibration_factor",
    "global.enabled",
    "global.include_docs_in_default",
    "sync.enabled",
    "sync.project_id",
    "sync.drive_subpath",
    "doc.allowed_dirs",
    "audit.user_md_path",
    "gc.prune_superseded_incremental",
    "gc.prune_large_thirdparty",
    "gc.delete_stale_backups",
    "wrap_up.verbosity",
    "wrap_up.transcript_retention_days",
];

/// Intermediate table paths (never themselves reported as unknown; a
/// key at one of these paths is walked one level deeper).
const KNOWN_TABLES: &[&str] = &[
    "deny_list",
    "integrations",
    "integrations.k9",
    "render",
    "retrieval",
    "retrieval.scoring",
    "code_index",
    "metrics",
    "global",
    "sync",
    "doc",
    "audit",
    "gc",
    "wrap_up",
];

fn check_config(config_path: &Path, memhub_dir: &Path) -> Vec<Check> {
    let raw = match fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) => {
            return skipped_config_group(format!(
                "cannot read {}: {e}",
                config_path.display()
            ));
        }
    };

    // "parse" is deliberately the *generic* TOML syntax check, not the
    // strongly-typed `ProjectConfig` parse `db::open_project` already
    // performs (and which would already have failed the whole command
    // before doctor's checks ever ran) — otherwise "known-key
    // type/range" below would be structurally unreachable.
    let Ok(generic) = toml::from_str::<toml::Value>(&raw) else {
        return skipped_config_group("config.toml is not valid TOML".to_string());
    };

    let mut out = vec![Check::new(
        "config_parse",
        Group::Config,
        Status::Ok,
        "config.toml parses",
    )];
    out.push(check_unknown_keys(&generic));
    out.push(check_config_types(&raw));
    out.push(check_baseline_drift(&generic, memhub_dir));
    out
}

fn skipped_config_group(parse_message: String) -> Vec<Check> {
    vec![
        Check::new("config_parse", Group::Config, Status::Error, parse_message),
        Check::new(
            "config_unknown_keys",
            Group::Config,
            Status::Skipped,
            "skipped: config.toml did not parse",
        ),
        Check::new(
            "config_types",
            Group::Config,
            Status::Skipped,
            "skipped: config.toml did not parse",
        ),
        Check::new(
            "config_baseline_drift",
            Group::Config,
            Status::Skipped,
            "skipped: config.toml did not parse",
        ),
    ]
}

fn check_unknown_keys(value: &toml::Value) -> Check {
    let mut unknown = Vec::new();
    if let Some(table) = value.as_table() {
        walk_unknown_keys(table, "", &mut unknown);
    }

    if unknown.is_empty() {
        Check::new(
            "config_unknown_keys",
            Group::Config,
            Status::Ok,
            "no unknown keys",
        )
    } else {
        let n = unknown.len();
        Check::new(
            "config_unknown_keys",
            Group::Config,
            Status::Warn,
            format!("{n} unknown key(s)"),
        )
        .with_detail(unknown.join(", "))
    }
}

fn walk_unknown_keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let full = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };

        if KNOWN_LEAVES.contains(&full.as_str()) {
            continue;
        }
        if KNOWN_TABLES.contains(&full.as_str()) {
            if let Some(inner) = value.as_table() {
                walk_unknown_keys(inner, &full, out);
            }
            continue;
        }
        out.push(full);
    }
}

/// Known-key type/range validation. Bad *types* (a `mode` outside
/// `{fts,hybrid}`, a non-boolean where a bool is expected, ...) show up
/// as a `ProjectConfig` deserialize failure even though the generic
/// TOML parse above already succeeded — that mismatch is exactly this
/// check's `error` case. A value that deserializes fine but is out of
/// a sane range (a weight outside `[0,1]`) is the second failure mode.
fn check_config_types(raw: &str) -> Check {
    let cfg: ProjectConfig = match toml::from_str(raw) {
        Ok(c) => c,
        Err(e) => {
            return Check::new(
                "config_types",
                Group::Config,
                Status::Error,
                format!("config does not match the expected schema: {e}"),
            );
        }
    };

    let mut problems = Vec::new();
    for (label, v) in [
        ("retrieval.scoring.fts_weight", cfg.retrieval.scoring.fts_weight),
        (
            "retrieval.scoring.vector_weight",
            cfg.retrieval.scoring.vector_weight,
        ),
        (
            "retrieval.scoring.stale_penalty",
            cfg.retrieval.scoring.stale_penalty,
        ),
        (
            "retrieval.scoring.superseded_penalty",
            cfg.retrieval.scoring.superseded_penalty,
        ),
        ("code_index.fts_weight", cfg.code_index.fts_weight),
        ("code_index.vector_weight", cfg.code_index.vector_weight),
        (
            "code_index.test_path_penalty",
            cfg.code_index.test_path_penalty,
        ),
    ] {
        if !(0.0..=1.0).contains(&v) {
            problems.push(format!("{label}={v} out of range [0,1]"));
        }
    }
    for (label, v) in [
        (
            "retrieval.scoring.min_rerank_score",
            cfg.retrieval.scoring.min_rerank_score,
        ),
        (
            "retrieval.scoring.doc_min_rerank_score",
            cfg.retrieval.scoring.doc_min_rerank_score,
        ),
    ] {
        if !v.is_finite() {
            problems.push(format!("{label}={v} is not a finite number"));
        }
    }

    // The staleness horizon must be a positive day count — 0 or negative
    // would mark every fact stale, defeating the demote/exclude distinction.
    if cfg.retrieval.fact_stale_after_days < 1 {
        problems.push(format!(
            "retrieval.fact_stale_after_days={} must be >= 1",
            cfg.retrieval.fact_stale_after_days,
        ));
    }

    // Age-decay half-life (Wave 3 L6): 0 = off (the default), > 0 enables
    // decay. A negative half-life is nonsensical — it would invert the
    // exponential into unbounded growth — so only < 0 is a violation.
    if cfg.retrieval.scoring.age_half_life_days < 0 {
        problems.push(format!(
            "retrieval.scoring.age_half_life_days={} must be >= 0 (0 = off)",
            cfg.retrieval.scoring.age_half_life_days,
        ));
    }

    // Transcript-archive retention (Wave 6 W3, issue #96): a u32 day count
    // where 0 = keep forever (pruning disabled). Negatives / non-integers
    // already fail the ProjectConfig parse above; the only reachable
    // violation is a horizon so large it is almost certainly a fat-finger
    // (see MAX_WRAP_UP_TRANSCRIPT_RETENTION_DAYS). Matches the sanity-band
    // posture of the fact_stale_after_days / age_half_life_days guards.
    if cfg.wrap_up.transcript_retention_days > crate::config::MAX_WRAP_UP_TRANSCRIPT_RETENTION_DAYS {
        problems.push(format!(
            "wrap_up.transcript_retention_days={} exceeds the sane maximum {} (0 = keep forever)",
            cfg.wrap_up.transcript_retention_days,
            crate::config::MAX_WRAP_UP_TRANSCRIPT_RETENTION_DAYS,
        ));
    }

    if problems.is_empty() {
        Check::new(
            "config_types",
            Group::Config,
            Status::Ok,
            "known keys have valid types and ranges",
        )
    } else {
        let n = problems.len();
        Check::new(
            "config_types",
            Group::Config,
            Status::Error,
            format!("{n} range violation(s)"),
        )
        .with_detail(problems.join("; "))
    }
}

/// Commit-back-here fields per the header comment in
/// `.memhub/config.example.toml` — recall/locate behavior + security-relevant
/// settings that should be identical on every machine. `integrations.k9.*`
/// is listed there as a commit-back project property, so `k9.enabled` is
/// the one `enabled` toggle included here; `metrics.enabled` and the
/// `global`/`sync` `enabled` flags stay excluded — the header documents
/// those three as fields a machine legitimately diverges on and does not
/// commit back. `doc.allowed_dirs` is excluded for the same per-machine
/// reason. `wrap_up.verbosity` (Wave 6, issue #95) is excluded too, for a
/// related but distinct reason: Q7 rules it a repo baseline (seeded in
/// the tracked example) whose canonical value a machine may still
/// legitimately raise locally — most notably to `transcript`, an
/// explicitly sanctioned per-machine opt-in — so treating any local
/// divergence as drift would warn on exactly the behavior Q7 permits.
/// `wrap_up.transcript_retention_days` (Wave 6 W3, issue #96) is the
/// opposite case and IS included below: the retention horizon is a
/// repo-wide policy value seeded in the tracked example, not a
/// per-machine toggle, so a local change to it is legitimate drift to
/// surface — even though the `transcript` verbosity that activates it is
/// itself a per-machine opt-in.
const BASELINE_FIELDS: &[&str] = &[
    "deny_list.patterns",
    "render.output_dir",
    "integrations.k9.enabled",
    "integrations.k9.agent_docs_path",
    "retrieval.mode",
    "retrieval.fact_stale_after_days",
    "retrieval.use_reranker",
    "retrieval.rerank_candidate_pool",
    "retrieval.scoring.fts_weight",
    "retrieval.scoring.vector_weight",
    "retrieval.scoring.stale_penalty",
    "retrieval.scoring.superseded_penalty",
    "retrieval.scoring.age_half_life_days",
    "retrieval.scoring.min_rerank_score",
    "retrieval.scoring.doc_min_rerank_score",
    "code_index.fts_weight",
    "code_index.vector_weight",
    "code_index.test_path_penalty",
    "wrap_up.transcript_retention_days",
];

fn check_baseline_drift(local: &toml::Value, memhub_dir: &Path) -> Check {
    let example_path = memhub_dir.join(db::CONFIG_EXAMPLE_FILENAME);
    if !example_path.is_file() {
        return Check::new(
            "config_baseline_drift",
            Group::Config,
            Status::Skipped,
            "no config.example.toml tracked in this repo",
        );
    }

    let example_raw = match fs::read_to_string(&example_path) {
        Ok(s) => s,
        Err(e) => {
            return Check::new(
                "config_baseline_drift",
                Group::Config,
                Status::Skipped,
                format!("cannot read config.example.toml: {e}"),
            );
        }
    };
    let Ok(example) = toml::from_str::<toml::Value>(&example_raw) else {
        return Check::new(
            "config_baseline_drift",
            Group::Config,
            Status::Skipped,
            "config.example.toml is not valid TOML; skipping comparison",
        );
    };

    let mut drifted = Vec::new();
    for path in BASELINE_FIELDS {
        if let (Some(l), Some(e)) = (lookup_toml_path(local, path), lookup_toml_path(&example, path))
            && l != e
        {
            drifted.push((*path).to_string());
        }
    }

    if drifted.is_empty() {
        Check::new(
            "config_baseline_drift",
            Group::Config,
            Status::Ok,
            "commit-back fields match config.example.toml",
        )
    } else {
        let n = drifted.len();
        Check::new(
            "config_baseline_drift",
            Group::Config,
            Status::Warn,
            format!("{n} commit-back field(s) differ from config.example.toml"),
        )
        .with_detail(drifted.join(", "))
    }
}

fn lookup_toml_path<'a>(value: &'a toml::Value, dotted: &str) -> Option<&'a toml::Value> {
    let mut cur = value;
    for part in dotted.split('.') {
        cur = cur.as_table()?.get(part)?;
    }
    Some(cur)
}

// ---------------------------------------------------------------------
// Integrity group (D1)
// ---------------------------------------------------------------------

/// (source table, FTS5 shadow table) pairs. Mirrors every
/// content-external FTS5 table in `project.sqlite` across the
/// migrations (0002 legacy git-search chunks, 0009 facts/decisions/
/// tasks, 0014 doc_chunks). The sibling code-index DB is out of scope
/// (disposable/rebuildable, per the PRD addendum).
const FTS_TABLES: &[(&str, &str)] = &[
    ("facts", "facts_fts"),
    ("decisions", "decisions_fts"),
    ("tasks", "tasks_fts"),
    ("doc_chunks", "doc_chunks_fts"),
    ("chunks", "chunk_fts"),
];

fn check_integrity(conn: &Connection) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(match run_integrity_check(conn) {
        Ok(msgs) if msgs.len() == 1 && msgs[0] == "ok" => Check::new(
            "integrity_check",
            Group::Integrity,
            Status::Ok,
            "PRAGMA integrity_check: ok",
        ),
        Ok(msgs) => {
            let n = msgs.len();
            Check::new(
                "integrity_check",
                Group::Integrity,
                Status::Error,
                format!("PRAGMA integrity_check reported {n} problem(s)"),
            )
            .with_detail(msgs.join("; "))
        }
        Err(e) => Check::new(
            "integrity_check",
            Group::Integrity,
            Status::Error,
            format!("PRAGMA integrity_check failed: {e}"),
        ),
    });

    out.push(match run_foreign_key_check(conn) {
        Ok(0) => Check::new(
            "foreign_key_check",
            Group::Integrity,
            Status::Ok,
            "no foreign key violations",
        ),
        Ok(n) => Check::new(
            "foreign_key_check",
            Group::Integrity,
            Status::Error,
            format!("{n} foreign key violation(s)"),
        ),
        Err(e) => Check::new(
            "foreign_key_check",
            Group::Integrity,
            Status::Error,
            format!("PRAGMA foreign_key_check failed: {e}"),
        ),
    });

    let mut fts_failures = Vec::new();
    for (_, fts) in FTS_TABLES {
        if let Err(e) = conn.execute(
            &format!("INSERT INTO {fts}({fts}) VALUES('integrity-check')"),
            [],
        ) {
            fts_failures.push(format!("{fts}: {e}"));
        }
    }
    out.push(if fts_failures.is_empty() {
        Check::new(
            "fts_integrity",
            Group::Integrity,
            Status::Ok,
            "FTS5 integrity-check passed on all tables",
        )
    } else {
        let n = fts_failures.len();
        Check::new(
            "fts_integrity",
            Group::Integrity,
            Status::Error,
            format!("{n} FTS table(s) failed integrity-check"),
        )
        .with_detail(fts_failures.join("; "))
    });

    // Every table here is *external content* (`content='<source>'`), so a
    // bare `SELECT COUNT(*) FROM <fts>` does not scan the FTS index at
    // all — it resolves rowids through the content-table linkage and
    // therefore always agrees with the source table, even after a
    // segment-level index entry has been removed independently (verified
    // empirically: deleting one entry via the FTS5 'delete' command drops
    // it to zero in the row below but leaves a plain count on the virtual
    // table unchanged). The `<fts>_docsize` shadow table carries one row
    // per rowid actually present in the index and is the reliable signal
    // for index drift.
    let mut mismatches = Vec::new();
    for (source, fts) in FTS_TABLES {
        let source_count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {source}"), [], |r| r.get(0))
            .unwrap_or(-1);
        let fts_count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {fts}_docsize"), [], |r| {
                r.get(0)
            })
            .unwrap_or(-1);
        if source_count != fts_count {
            mismatches.push(format!("{source}={source_count} vs {fts}={fts_count}"));
        }
    }
    out.push(if mismatches.is_empty() {
        Check::new(
            "fts_rowcounts",
            Group::Integrity,
            Status::Ok,
            "FTS rowcounts match source tables",
        )
    } else {
        let n = mismatches.len();
        Check::new(
            "fts_rowcounts",
            Group::Integrity,
            Status::Error,
            format!("{n} FTS/source rowcount mismatch(es)"),
        )
        .with_detail(mismatches.join("; "))
    });

    out.push(match count_orphaned_embeddings(conn) {
        Ok(0) => Check::new(
            "orphaned_embeddings",
            Group::Integrity,
            Status::Ok,
            "no orphaned embeddings",
        ),
        Ok(n) => Check::new(
            "orphaned_embeddings",
            Group::Integrity,
            Status::Warn,
            format!("{n} orphaned embedding row(s)"),
        ),
        Err(e) => Check::new(
            "orphaned_embeddings",
            Group::Integrity,
            Status::Error,
            format!("orphaned-embeddings query failed: {e}"),
        ),
    });

    out
}

fn run_integrity_check(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

fn run_foreign_key_check(conn: &Connection) -> rusqlite::Result<usize> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut count = 0usize;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok(count)
}

fn count_orphaned_embeddings(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM embeddings e
         WHERE (e.source_type = 'fact' AND NOT EXISTS (SELECT 1 FROM facts f WHERE f.id = e.source_id))
            OR (e.source_type = 'decision' AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.id = e.source_id))
            OR (e.source_type = 'task' AND NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = e.source_id))
            OR (e.source_type = 'doc_chunk' AND NOT EXISTS (SELECT 1 FROM doc_chunks c WHERE c.id = e.source_id))",
        [],
        |r| r.get(0),
    )
}

// ---------------------------------------------------------------------
// Retrieval / Metrics group (X4)
// ---------------------------------------------------------------------

pub(crate) fn check_retrieval_mode(config: &ProjectConfig) -> Check {
    let mode = match config.retrieval.mode {
        RetrievalMode::Fts => "fts",
        RetrievalMode::Hybrid => "hybrid",
    };
    let reranker = if config.retrieval.mode == RetrievalMode::Hybrid {
        if config.retrieval.use_reranker {
            "reranker on"
        } else {
            "reranker off"
        }
    } else {
        "reranker n/a in fts mode"
    };
    Check::new(
        "retrieval_mode",
        Group::RetrievalMetrics,
        Status::Ok,
        format!("mode={mode}, {reranker}"),
    )
}

pub(crate) fn check_embeddings_freshness(start: &Path, config: &ProjectConfig) -> Check {
    if config.retrieval.mode != RetrievalMode::Hybrid {
        return Check::new(
            "embeddings_freshness",
            Group::RetrievalMetrics,
            Status::Skipped,
            "fts mode; embeddings not applicable",
        );
    }

    match index::status(start) {
        Ok(summary) if summary.missing_count == 0 => Check::new(
            "embeddings_freshness",
            Group::RetrievalMetrics,
            Status::Ok,
            format!("embeddings current for model {}", summary.model),
        ),
        Ok(summary) => Check::new(
            "embeddings_freshness",
            Group::RetrievalMetrics,
            Status::Warn,
            format!(
                "{} row(s) missing/stale embeddings ({:.0}%) — run `memhub index rebuild`",
                summary.missing_count,
                summary.stale_ratio * 100.0
            ),
        ),
        Err(e) => Check::new(
            "embeddings_freshness",
            Group::RetrievalMetrics,
            Status::Error,
            format!("embeddings status check failed: {e}"),
        ),
    }
}

pub(crate) fn check_metrics_health(conn: &Connection, config: &ProjectConfig) -> Check {
    let cfg = &config.metrics;
    if !cfg.enabled || !cfg.session_accounting {
        return Check::new(
            "metrics_health",
            Group::RetrievalMetrics,
            Status::Skipped,
            "session accounting disabled",
        );
    }

    let mut problems = Vec::new();
    let claude_set = !cfg.claude_transcripts_dir.is_empty();
    let codex_set = !cfg.codex_transcripts_dir.is_empty();
    if !claude_set && !codex_set {
        problems.push("no transcripts dir resolved for any agent".to_string());
    }
    for (label, dir) in [
        ("claude", &cfg.claude_transcripts_dir),
        ("codex", &cfg.codex_transcripts_dir),
    ] {
        if !dir.is_empty() && !Path::new(dir).exists() {
            problems.push(format!("{label} transcripts dir missing: {dir}"));
        }
    }

    if !problems.is_empty() {
        return Check::new(
            "metrics_health",
            Group::RetrievalMetrics,
            Status::Warn,
            problems.join("; "),
        );
    }

    let last_scrape: Option<String> = conn
        .query_row("SELECT MAX(ended_at) FROM session_metrics", [], |r| {
            r.get(0)
        })
        .unwrap_or(None);
    match last_scrape {
        Some(ts) => Check::new(
            "metrics_health",
            Group::RetrievalMetrics,
            Status::Ok,
            format!("session accounting on; last scrape {ts}"),
        ),
        None => Check::new(
            "metrics_health",
            Group::RetrievalMetrics,
            Status::Ok,
            "session accounting on; no sessions scraped yet",
        ),
    }
}

// ---------------------------------------------------------------------
// Integrations group (P1 report-only, P4)
// ---------------------------------------------------------------------

const MCP_WARN_SUFFIX: &str =
    "is set up but memhub's MCP server is not registered — skills' MCP-first path won't fire";

fn check_mcp_claude(repo_root: &Path, home: Option<&Path>) -> Check {
    // A repo-scoped `.mcp.json` (the Q40 target format per the review's
    // decision 40) always counts as registered, independent of whatever
    // `~/.claude.json` says — it's the more portable of the two and,
    // unlike the global file, is not itself a health signal to demand.
    if json_file_registers_memhub(&repo_root.join(".mcp.json"), "mcpServers") {
        return Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Ok,
            "memhub MCP server registered (repo-scoped .mcp.json)",
        );
    }

    let Some(home) = home else {
        return Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Skipped,
            "cannot resolve home directory",
        );
    };
    let claude_json = home.join(".claude.json");
    let Ok(raw) = fs::read_to_string(&claude_json) else {
        return Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Skipped,
            "Claude Code not set up (no ~/.claude.json)",
        );
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Warn,
            "~/.claude.json is not valid JSON; cannot confirm MCP registration",
        );
    };

    let top_level = value
        .get("mcpServers")
        .and_then(|m| m.get("memhub"))
        .is_some();

    // Claude Code keys per-project entries by an absolute, forward-slash
    // path — even on Windows — so normalize before comparing.
    let repo_key = normalize_slashes(repo_root);
    let project_level = value
        .get("projects")
        .and_then(|p| p.as_object())
        .and_then(|projects| projects.iter().find(|(k, _)| paths_equal_loose(k, &repo_key)))
        .and_then(|(_, entry)| entry.get("mcpServers"))
        .and_then(|m| m.get("memhub"))
        .is_some();

    if top_level || project_level {
        Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Ok,
            "memhub MCP server registered in Claude Code",
        )
    } else {
        Check::new(
            "mcp_registration_claude",
            Group::Integrations,
            Status::Warn,
            format!("Claude Code {MCP_WARN_SUFFIX}"),
        )
    }
}

fn check_mcp_codex(home: Option<&Path>) -> Check {
    let Some(home) = home else {
        return Check::new(
            "mcp_registration_codex",
            Group::Integrations,
            Status::Skipped,
            "cannot resolve home directory",
        );
    };
    let codex_dir = home.join(".codex");
    if !codex_dir.is_dir() {
        return Check::new(
            "mcp_registration_codex",
            Group::Integrations,
            Status::Skipped,
            "Codex CLI not set up (no ~/.codex)",
        );
    }

    let registered = fs::read_to_string(codex_dir.join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
        .and_then(|v| {
            v.get("mcp_servers")
                .and_then(|m| m.get("memhub"))
                .map(|_| ())
        })
        .is_some();

    if registered {
        Check::new(
            "mcp_registration_codex",
            Group::Integrations,
            Status::Ok,
            "memhub MCP server registered in Codex",
        )
    } else {
        Check::new(
            "mcp_registration_codex",
            Group::Integrations,
            Status::Warn,
            format!("Codex {MCP_WARN_SUFFIX}"),
        )
    }
}

fn check_mcp_opencode(repo_root: &Path, home: Option<&Path>) -> Check {
    let global_dir = home.map(|h| h.join(".config").join("opencode"));
    let set_up = global_dir.as_deref().is_some_and(Path::is_dir);
    if !set_up {
        return Check::new(
            "mcp_registration_opencode",
            Group::Integrations,
            Status::Skipped,
            "OpenCode not set up (no ~/.config/opencode)",
        );
    }

    let mut candidates = vec![
        repo_root.join("opencode.json"),
        repo_root.join("opencode.jsonc"),
    ];
    if let Some(dir) = &global_dir {
        candidates.push(dir.join("opencode.json"));
        candidates.push(dir.join("opencode.jsonc"));
    }

    let registered = candidates
        .iter()
        .any(|p| opencode_config_registers_memhub(p));

    if registered {
        Check::new(
            "mcp_registration_opencode",
            Group::Integrations,
            Status::Ok,
            "memhub MCP server registered in OpenCode",
        )
    } else {
        Check::new(
            "mcp_registration_opencode",
            Group::Integrations,
            Status::Warn,
            format!("OpenCode {MCP_WARN_SUFFIX}"),
        )
    }
}

fn json_file_registers_memhub(path: &Path, servers_key: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.get(servers_key).and_then(|m| m.get("memhub")).map(|_| ()))
        .is_some()
}

/// Best-effort JSONC-tolerant check for an `mcp`/`mcpServers` block
/// containing a `memhub` key. OpenCode's exact MCP registration schema
/// is not pinned down anywhere in this repo (see
/// docs/reviews/2026-07-improvement-review.md §13.2 P1/P5 — precedence
/// between `opencode.json` and `opencode.jsonc` is itself flagged
/// unverified), so this is deliberately lenient: strip `//` line
/// comments (a reasonable JSONC approximation), try a real JSON parse
/// under either key name, and fall back to a raw substring heuristic
/// for JSONC constructs that pass still can't handle (e.g. trailing
/// commas). This is report-only (P1) — a false negative here costs an
/// unnecessary warn, never a crash or a write.
fn opencode_config_registers_memhub(path: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let stripped = strip_jsonc_line_comments(&raw);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stripped) {
        let has = |key: &str| value.get(key).and_then(|m| m.get("memhub")).is_some();
        return has("mcp") || has("mcpServers");
    }
    raw.contains("\"memhub\"") && (raw.contains("\"mcp\"") || raw.contains("\"mcpServers\""))
}

fn strip_jsonc_line_comments(raw: &str) -> String {
    raw.lines()
        .map(strip_line_comment)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strips a trailing `//` comment from one line, honoring double-quoted
/// string content (a `//` inside a JSON string is not a comment). Not a
/// full JSONC parser — block comments and escaped quotes inside strings
/// are out of scope for this best-effort heuristic.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut i = 0;
    while i + 1 < bytes.len() {
        match bytes[i] {
            b'"' => in_string = !in_string,
            b'/' if !in_string && bytes[i + 1] == b'/' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

fn normalize_slashes(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn paths_equal_loose(a: &str, b: &str) -> bool {
    a.replace('\\', "/").eq_ignore_ascii_case(&b.replace('\\', "/"))
}

pub(crate) fn check_sync_freshness(start: &Path, config: &ProjectConfig) -> Check {
    if !config.sync.enabled {
        return Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Skipped,
            "cross-machine sync disabled",
        );
    }

    let remote = match sync::default_remote_dir(start) {
        Ok(p) => p,
        Err(e) => {
            return Check::new(
                "sync_freshness",
                Group::Integrations,
                Status::Warn,
                format!("sync enabled but remote dir cannot be resolved: {e}"),
            );
        }
    };

    match sync::check(start, &remote) {
        Ok(report) => sync_report_to_check(&report),
        Err(e) => Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Warn,
            format!("sync check failed: {e}"),
        ),
    }
}

/// Interprets one `sync::CheckReport` into a `Check`. Checks
/// `project_id_mismatch` and `schema_blocks_adopt` before falling back
/// to the git-style `verdict` — both are independent of the logical-
/// version comparison that drives `verdict`, so either can be true
/// under an otherwise-OK-looking verdict (e.g. a wrong-project remote
/// can still land on `UpToDate` if its logical version happens to
/// match). Surfacing them first means neither is masked.
fn sync_report_to_check(report: &sync::CheckReport) -> Check {
    if let Some(remote_project_id) = &report.project_id_mismatch {
        return Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Warn,
            format!(
                "remote snapshot is for a different project ({remote_project_id}) — do not adopt"
            ),
        );
    }

    if report.schema_blocks_adopt {
        return Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Warn,
            "remote snapshot schema is newer than this binary — run `memhub upgrade` before adopting",
        );
    }

    match report.verdict {
        sync::SyncVerdict::DriveAhead | sync::SyncVerdict::Diverged => Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Warn,
            "remote is ahead — run /catch-up before writing",
        ),
        sync::SyncVerdict::UpToDate
        | sync::SyncVerdict::LocalAhead
        | sync::SyncVerdict::NoRemote => Check::new(
            "sync_freshness",
            Group::Integrations,
            Status::Ok,
            format!("sync verdict: {}", report.verdict.as_str()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{decision, fact, index, init, render};
    use tempfile::tempdir;

    fn healthy_repo() -> tempfile::TempDir {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        render::run(temp.path(), "cli:user").expect("render");
        temp
    }

    fn empty_home() -> tempfile::TempDir {
        tempdir().expect("tempdir for fake home")
    }

    // -- Top-level report shape / exit codes ----------------------------

    #[test]
    fn healthy_repo_reports_ok_and_exits_zero() {
        let temp = healthy_repo();
        let home = empty_home();

        let report =
            run_with_home(temp.path(), false, Some(home.path().to_path_buf())).expect("doctor");

        assert_eq!(report.overall, Status::Ok, "checks: {:#?}", report.checks);
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.counts.error, 0);
        assert_eq!(report.counts.warn, 0, "checks: {:#?}", report.checks);
        assert!(!report.checks.is_empty());
    }

    #[test]
    fn strict_promotes_a_warn_to_exit_one() {
        let temp = healthy_repo();
        let home = empty_home();

        // A durable write after the render with no follow-up render call
        // seeds exactly one warn (render_freshness).
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");

        let plain =
            run_with_home(temp.path(), false, Some(home.path().to_path_buf())).expect("doctor");
        assert!(plain.counts.warn >= 1, "checks: {:#?}", plain.checks);
        assert_eq!(
            plain.exit_code, 0,
            "a warn alone must not fail the plain (non-strict) exit code"
        );

        let strict =
            run_with_home(temp.path(), true, Some(home.path().to_path_buf())).expect("doctor");
        assert_eq!(strict.exit_code, 1, "checks: {:#?}", strict.checks);
        assert_eq!(strict.overall, Status::Warn);
    }

    fn find<'a>(checks: &'a [Check], id: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check with id {id:?}; have {checks:#?}"))
    }

    // -- Integrity (D1) ---------------------------------------------------

    #[test]
    fn integrity_checks_all_ok_on_a_fresh_db() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");

        let checks = check_integrity(&ctx.conn);
        for c in &checks {
            assert_eq!(c.status, Status::Ok, "{}: {}", c.id, c.message);
        }
    }

    #[test]
    fn orphaned_embeddings_are_counted_as_a_warn() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
                 VALUES (1, 'fact', 999999, 'test-model', 1, x'00', 'deadbeef')",
                [],
            )
            .expect("seed orphan");

        let checks = check_integrity(&ctx.conn);
        let orphan = find(&checks, "orphaned_embeddings");
        assert_eq!(orphan.status, Status::Warn);
        assert!(orphan.message.contains('1'));
    }

    #[test]
    fn fts_rowcount_drift_is_an_error() {
        let temp = healthy_repo();
        let (fact_id, _) = fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        let ctx = db::open_project(temp.path()).expect("open");

        // Desync the shadow index directly via the FTS5 'delete' command
        // — the same form the source table's own AFTER DELETE trigger
        // uses — so only the fts5 postings for this rowid are removed;
        // the real `facts` row (and its rowcount) is left untouched,
        // producing a genuine facts=1 / facts_fts=0 drift. A bare
        // `SELECT COUNT(*)` against an *external content* fts5 table
        // does not detect this (it resolves rowids through the content
        // table and so always agrees with it); `check_integrity` reads
        // the `<fts>_docsize` shadow table instead — see its comment.
        ctx.conn
            .execute(
                "INSERT INTO facts_fts(facts_fts, rowid, key, value) VALUES ('delete', ?1, 'k', 'v')",
                params![fact_id],
            )
            .expect("desync fts");

        let checks = check_integrity(&ctx.conn);
        let mismatch = find(&checks, "fts_rowcounts");
        assert_eq!(mismatch.status, Status::Error);
        assert!(mismatch.detail.as_deref().unwrap_or("").contains("facts"));
    }

    // -- Config group ------------------------------------------------------

    #[test]
    fn config_types_catches_an_invalid_mode() {
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[retrieval]
mode = "bogus"
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Error, "{}", check.message);
    }

    #[test]
    fn config_types_catches_an_invalid_wrap_up_verbosity() {
        // Doctor parity for the Wave 6 W1 `[wrap_up]` section (issue
        // #95): an out-of-vocabulary verbosity must fail the same way
        // an invalid `retrieval.mode` does — via the natural
        // `ProjectConfig` deserialize failure, not a bespoke check.
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[wrap_up]
verbosity = "extremely-verbose"
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Error, "{}", check.message);
    }

    #[test]
    fn config_types_catches_an_out_of_range_weight() {
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[retrieval.scoring]
fts_weight = 5.0
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Error);
        assert!(check.detail.unwrap_or_default().contains("fts_weight"));
    }

    #[test]
    fn config_types_catches_an_out_of_range_code_index_weight() {
        // Doctor parity for the R11 (issue #73) [code_index] split: the
        // new keys must get the same range validation as their
        // [retrieval.scoring] counterparts, not read as "unknown" or
        // silently accepted out of range.
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[code_index]
fts_weight = 5.0
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Error);
        assert!(
            check
                .detail
                .unwrap_or_default()
                .contains("code_index.fts_weight")
        );
    }

    #[test]
    fn config_types_ok_on_defaults() {
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn unknown_keys_are_detected_and_known_ones_are_not() {
        let value: toml::Value = toml::from_str(
            r#"
project_name = "x"
auto_sync_md = false
log_level = "info"
made_up_top_level_key = true

[retrieval]
mode = "fts"
made_up_nested_key = 1
"#,
        )
        .expect("parse");

        let check = check_unknown_keys(&value);
        assert_eq!(check.status, Status::Warn);
        let detail = check.detail.unwrap_or_default();
        assert!(detail.contains("made_up_top_level_key"));
        assert!(detail.contains("retrieval.made_up_nested_key"));
        assert!(!detail.contains("retrieval.mode"));
    }

    #[test]
    fn code_index_keys_are_known_not_unknown() {
        // Doctor parity for the R11 (issue #73) [code_index] split.
        let value: toml::Value = toml::from_str(
            r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[code_index]
fts_weight = 0.6
vector_weight = 0.4
test_path_penalty = 0.8
"#,
        )
        .expect("parse");

        let check = check_unknown_keys(&value);
        assert_eq!(check.status, Status::Ok, "detail: {:?}", check.detail);
    }

    #[test]
    fn gc_keys_are_known_not_unknown() {
        // Doctor parity for the Wave 5 U5/U8 `[gc]` section.
        let value: toml::Value = toml::from_str(
            r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[gc]
prune_superseded_incremental = true
prune_large_thirdparty = false
delete_stale_backups = false
"#,
        )
        .expect("parse");

        let check = check_unknown_keys(&value);
        assert_eq!(check.status, Status::Ok, "detail: {:?}", check.detail);
    }

    #[test]
    fn wrap_up_keys_are_known_not_unknown() {
        // Doctor parity for the Wave 6 W1 `[wrap_up]` section (issue #95).
        let value: toml::Value = toml::from_str(
            r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[wrap_up]
verbosity = "full"
"#,
        )
        .expect("parse");

        let check = check_unknown_keys(&value);
        assert_eq!(check.status, Status::Ok, "detail: {:?}", check.detail);
    }

    #[test]
    fn wrap_up_verbosity_is_excluded_from_baseline_drift() {
        // Q7: verbosity is a repo baseline, but a machine may legitimately
        // raise it locally (e.g. to "transcript") without that being
        // flagged as drift — see the doc comment above BASELINE_FIELDS.
        let local: toml::Value =
            toml::from_str("[wrap_up]\nverbosity = \"transcript\"\n").expect("local");
        let example: toml::Value =
            toml::from_str("[wrap_up]\nverbosity = \"standard\"\n").expect("example");

        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join(db::CONFIG_EXAMPLE_FILENAME),
            toml::to_string(&example).unwrap(),
        )
        .expect("write example");

        let check = check_baseline_drift(&local, temp.path());
        assert_eq!(check.status, Status::Ok, "{:?}", check.detail);
    }

    #[test]
    fn transcript_retention_days_is_a_known_key() {
        // Doctor parity for the Wave 6 W3 `[wrap_up]` retention knob
        // (issue #96): the new leaf must not read as an unknown key.
        let value: toml::Value = toml::from_str(
            r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[wrap_up]
verbosity = "transcript"
transcript_retention_days = 30
"#,
        )
        .expect("parse");

        let check = check_unknown_keys(&value);
        assert_eq!(check.status, Status::Ok, "detail: {:?}", check.detail);
    }

    #[test]
    fn config_types_flags_an_absurd_transcript_retention_horizon() {
        // 0 = keep forever (valid); a sane finite horizon is valid; only a
        // value past the fat-finger ceiling is a range violation.
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"

[wrap_up]
transcript_retention_days = 99999999
"#;
        let check = check_config_types(raw);
        assert_eq!(check.status, Status::Error, "{}", check.message);
        assert!(
            check
                .detail
                .unwrap_or_default()
                .contains("wrap_up.transcript_retention_days")
        );
    }

    #[test]
    fn config_types_accepts_zero_and_normal_transcript_retention() {
        for days in ["0", "1", "90", "3650"] {
            let raw = format!(
                "project_name = \"x\"\nauto_sync_md = false\nlog_level = \"info\"\n\
                 [wrap_up]\ntranscript_retention_days = {days}\n"
            );
            let check = check_config_types(&raw);
            assert_eq!(check.status, Status::Ok, "days={days}: {}", check.message);
        }
    }

    #[test]
    fn transcript_retention_days_is_baseline_drift_checked() {
        // Unlike verbosity, the retention horizon IS a repo baseline: a
        // local value differing from the tracked example is real drift.
        let local: toml::Value =
            toml::from_str("[wrap_up]\ntranscript_retention_days = 7\n").expect("local");
        let example: toml::Value =
            toml::from_str("[wrap_up]\ntranscript_retention_days = 90\n").expect("example");

        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join(db::CONFIG_EXAMPLE_FILENAME),
            toml::to_string(&example).unwrap(),
        )
        .expect("write example");

        let check = check_baseline_drift(&local, temp.path());
        assert_eq!(check.status, Status::Warn);
        assert!(
            check
                .detail
                .unwrap_or_default()
                .contains("wrap_up.transcript_retention_days")
        );
    }

    #[test]
    fn baseline_drift_detects_a_changed_commit_back_field() {
        let local: toml::Value = toml::from_str("[retrieval]\nmode = \"fts\"\n").expect("local");
        let example: toml::Value =
            toml::from_str("[retrieval]\nmode = \"hybrid\"\n").expect("example");

        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join(db::CONFIG_EXAMPLE_FILENAME),
            toml::to_string(&example).unwrap(),
        )
        .expect("write example");

        let check = check_baseline_drift(&local, temp.path());
        assert_eq!(check.status, Status::Warn);
        assert!(check.detail.unwrap_or_default().contains("retrieval.mode"));
    }

    #[test]
    fn baseline_drift_skips_without_a_tracked_example() {
        let local: toml::Value = toml::from_str("[retrieval]\nmode = \"fts\"\n").expect("local");
        let temp = tempdir().expect("tempdir");

        let check = check_baseline_drift(&local, temp.path());
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn baseline_drift_detects_a_changed_k9_enabled() {
        let local: toml::Value =
            toml::from_str("[integrations.k9]\nenabled = true\nagent_docs_path = \"agent_docs\"\n")
                .expect("local");
        let example: toml::Value =
            toml::from_str("[integrations.k9]\nenabled = false\nagent_docs_path = \"agent_docs\"\n")
                .expect("example");

        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join(db::CONFIG_EXAMPLE_FILENAME),
            toml::to_string(&example).unwrap(),
        )
        .expect("write example");

        let check = check_baseline_drift(&local, temp.path());
        assert_eq!(check.status, Status::Warn);
        assert!(
            check
                .detail
                .unwrap_or_default()
                .contains("integrations.k9.enabled")
        );
    }

    // -- K9 coexistence ------------------------------------------------------

    #[test]
    fn k9_not_detected_is_skipped() {
        let temp = tempdir().expect("tempdir");
        let check = check_k9_coexistence(temp.path(), &IntegrationsConfig::default());
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn k9_detected_and_enabled_is_ok() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("agent_docs");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("project_state.md"), "# state").expect("write");

        let cfg = IntegrationsConfig {
            k9: Some(crate::config::K9Config {
                enabled: true,
                agent_docs_path: "agent_docs".to_string(),
            }),
        };
        let check = check_k9_coexistence(temp.path(), &cfg);
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn k9_detected_but_disabled_is_archived_and_skipped() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("agent_docs");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(dir.join("project_state.md"), "# state").expect("write");

        let cfg = IntegrationsConfig {
            k9: Some(crate::config::K9Config {
                enabled: false,
                agent_docs_path: "agent_docs".to_string(),
            }),
        };
        let check = check_k9_coexistence(temp.path(), &cfg);
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn k9_enabled_but_path_missing_is_a_warn() {
        let temp = tempdir().expect("tempdir");
        let cfg = IntegrationsConfig {
            k9: Some(crate::config::K9Config {
                enabled: true,
                agent_docs_path: "agent_docs".to_string(),
            }),
        };
        let check = check_k9_coexistence(temp.path(), &cfg);
        assert_eq!(check.status, Status::Warn);
    }

    // -- Render freshness ------------------------------------------------------

    #[test]
    fn render_freshness_warns_when_never_rendered() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let ctx = db::open_project(temp.path()).expect("open");

        let check = check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config);
        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("never rendered"));
    }

    #[test]
    fn render_freshness_ok_immediately_after_render() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");

        let check = check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config);
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn render_freshness_warns_on_drift_since_last_render() {
        let temp = healthy_repo();
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        let ctx = db::open_project(temp.path()).expect("open");

        let check = check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config);
        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("since last render"));
    }

    // -- Embeddings freshness ------------------------------------------------------

    #[test]
    fn embeddings_freshness_skipped_in_fts_mode() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_embeddings_freshness(temp.path(), &ctx.config);
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn embeddings_freshness_warns_on_missing_coverage_in_hybrid_mode() {
        let temp = healthy_repo();
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");

        let ctx = db::open_project(temp.path()).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&ctx.paths.config_path).expect("save config");

        let check = check_embeddings_freshness(temp.path(), &cfg);
        assert_eq!(check.status, Status::Warn);
    }

    #[test]
    fn embeddings_freshness_ok_once_rebuilt_in_hybrid_mode() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&ctx.paths.config_path).expect("save config");

        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        index::rebuild(temp.path(), "cli:user").expect("rebuild");

        let ctx = db::open_project(temp.path()).expect("reopen");
        let check = check_embeddings_freshness(temp.path(), &ctx.config);
        assert_eq!(check.status, Status::Ok, "{}", check.message);
    }

    // -- Metrics health ------------------------------------------------------

    #[test]
    fn metrics_health_skipped_when_disabled() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_metrics_health(&ctx.conn, &ctx.config);
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn metrics_health_warns_on_missing_transcripts_dir() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.metrics.enabled = true;
        cfg.metrics.session_accounting = true;
        cfg.metrics.claude_transcripts_dir = temp
            .path()
            .join("does-not-exist")
            .display()
            .to_string();
        cfg.save(&ctx.paths.config_path).expect("save config");

        let ctx = db::open_project(temp.path()).expect("reopen");
        let check = check_metrics_health(&ctx.conn, &ctx.config);
        assert_eq!(check.status, Status::Warn);
    }

    // -- MCP registration (P1) ------------------------------------------------------

    #[test]
    fn mcp_claude_skipped_when_not_set_up() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        let check = check_mcp_claude(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn mcp_claude_warns_when_set_up_but_not_registered() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        fs::write(home.path().join(".claude.json"), r#"{"mcpServers": {}}"#).expect("write");

        let check = check_mcp_claude(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Warn);
    }

    #[test]
    fn mcp_claude_ok_when_registered_top_level() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers": {"memhub": {"command": "memhub", "args": ["serve"]}}}"#,
        )
        .expect("write");

        let check = check_mcp_claude(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn mcp_claude_ok_via_repo_scoped_mcp_json() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        fs::write(
            temp.path().join(".mcp.json"),
            r#"{"mcpServers": {"memhub": {"command": "memhub", "args": ["serve"]}}}"#,
        )
        .expect("write");

        let check = check_mcp_claude(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn mcp_codex_warns_when_set_up_but_not_registered() {
        let home = empty_home();
        fs::create_dir_all(home.path().join(".codex")).expect("mkdir");

        let check = check_mcp_codex(Some(home.path()));
        assert_eq!(check.status, Status::Warn);
    }

    #[test]
    fn mcp_codex_ok_when_registered() {
        let home = empty_home();
        fs::create_dir_all(home.path().join(".codex")).expect("mkdir");
        fs::write(
            home.path().join(".codex").join("config.toml"),
            "[mcp_servers.memhub]\ncommand = \"memhub\"\nargs = [\"serve\"]\n",
        )
        .expect("write");

        let check = check_mcp_codex(Some(home.path()));
        assert_eq!(check.status, Status::Ok);
    }

    #[test]
    fn mcp_opencode_skipped_when_not_set_up() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        let check = check_mcp_opencode(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Skipped);
    }

    #[test]
    fn mcp_opencode_ok_via_repo_scoped_jsonc_with_comments() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        fs::create_dir_all(home.path().join(".config").join("opencode")).expect("mkdir");
        fs::write(
            temp.path().join("opencode.jsonc"),
            "{\n  // memhub MCP registration\n  \"mcp\": { \"memhub\": { \"command\": \"memhub\" } }\n}\n",
        )
        .expect("write");

        let check = check_mcp_opencode(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Ok);
    }

    /// Exact shape committed to this repo's own `opencode.json` (issue
    /// #65 R2): array-form `command`, plus `type`/`enabled` siblings the
    /// presence-only checker must still see past to find `memhub`.
    #[test]
    fn mcp_opencode_ok_via_committed_opencode_json_shape() {
        let temp = tempdir().expect("tempdir");
        let home = empty_home();
        fs::create_dir_all(home.path().join(".config").join("opencode")).expect("mkdir");
        fs::write(
            temp.path().join("opencode.json"),
            r#"{"mcp": {"memhub": {"type": "local", "command": ["memhub", "serve"], "enabled": true}}}"#,
        )
        .expect("write");

        let check = check_mcp_opencode(temp.path(), Some(home.path()));
        assert_eq!(check.status, Status::Ok);
    }

    // -- Sync freshness (P4) ------------------------------------------------------

    #[test]
    fn sync_freshness_skipped_when_disabled() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_sync_freshness(temp.path(), &ctx.config);
        assert_eq!(check.status, Status::Skipped);
    }

    /// A deliberately "boring" report — up-to-date verdict, no mismatch,
    /// no schema block — so each test below only needs to flip the one
    /// field it's exercising.
    fn base_check_report() -> sync::CheckReport {
        sync::CheckReport {
            verdict: sync::SyncVerdict::UpToDate,
            baseline_present: true,
            project_id: "proj-a".to_string(),
            local_logical: sync::LogicalVersion {
                writes_log_max_id: 0,
                writes_log_count: 0,
                digest: "d".to_string(),
            },
            remote_logical: None,
            local_schema: "0017_x".to_string(),
            remote_schema: None,
            schema_blocks_adopt: false,
            project_id_mismatch: None,
            remote_machine_id: None,
            remote_created_at: None,
        }
    }

    #[test]
    fn sync_freshness_warns_on_project_id_mismatch_despite_ok_verdict() {
        let mut report = base_check_report();
        report.project_id_mismatch = Some("other-project".to_string());

        let check = sync_report_to_check(&report);
        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("different project"));
    }

    #[test]
    fn sync_freshness_warns_on_schema_blocks_adopt_despite_ok_verdict() {
        let mut report = base_check_report();
        report.schema_blocks_adopt = true;

        let check = sync_report_to_check(&report);
        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("memhub upgrade"));
    }

    // -- misc ------------------------------------------------------

    #[test]
    fn retrieval_mode_check_reports_both_fields() {
        let temp = healthy_repo();
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_retrieval_mode(&ctx.config);
        assert_eq!(check.status, Status::Ok);
        assert!(check.message.contains("mode="));
    }

    #[test]
    fn decision_add_does_not_affect_writes_log_recency_zero_case() {
        // Sanity: a fresh, untouched repo (no init helper writes yet)
        // reports ok, not a warn — "0 writes" is healthy, not stale.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_writes_log_recency(&ctx.conn);
        assert_eq!(check.status, Status::Ok);

        // And once something real happens, it stays ok and mentions the count.
        decision::add(temp.path(), "t", "r", "user", "cli:user").expect("decision");
        let ctx = db::open_project(temp.path()).expect("open");
        let check = check_writes_log_recency(&ctx.conn);
        assert_eq!(check.status, Status::Ok);
        assert!(check.message.contains("logged"));
    }
}
