use std::path::Path;

use crate::Result;
use crate::commands::doctor;
use crate::commands::fact;
use crate::commands::integrations;
use crate::db;
use crate::models::StatusSummary;

pub fn run(start: &Path) -> Result<StatusSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let schema_version: String = conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let facts: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))?;
    let stale_facts = fact::count_stale(start)?;
    let decisions: i64 = conn.query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))?;
    let tasks_total: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
    let tasks_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status = 'open'",
        [],
        |row| row.get(0),
    )?;
    let commands: i64 = conn.query_row("SELECT COUNT(*) FROM commands", [], |row| row.get(0))?;
    let commits: i64 = conn.query_row("SELECT COUNT(*) FROM commits", [], |row| row.get(0))?;
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
    let pending_writes: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_writes WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    let writes_logged: i64 =
        conn.query_row("SELECT COUNT(*) FROM writes_log", [], |row| row.get(0))?;

    let deny_patterns = ctx.config.deny_list.patterns.len();
    let k9_state = integrations::k9_state(&ctx.paths.repo_root, &ctx.config.integrations);
    let project_name = ctx.config.project_name;
    let repo_root = ctx.paths.repo_root;
    let db_path = ctx.paths.db_path;
    let config_path = ctx.paths.config_path;

    Ok(StatusSummary {
        project_name,
        repo_root,
        db_path,
        config_path,
        schema_version,
        facts,
        stale_facts,
        decisions,
        tasks_open,
        tasks_total,
        commands,
        commits,
        files,
        chunks,
        pending_writes,
        writes_logged,
        deny_patterns,
        k9_detected: k9_state.detected,
        k9_enabled: k9_state.enabled,
        k9_agent_docs_path: k9_state.agent_docs_path,
        k9_drift: k9_state.drift,
    })
}

/// Cheap, always-relevant subsystem-state checks for `status`'s fast
/// path (Wave 1·C, issue #22): a curated subset of `doctor`'s own
/// checks (issue #21), reused by calling them directly instead of
/// duplicating their logic. Deliberately excludes doctor's heavy
/// integrity PRAGMAs, config validation, and MCP-registration probes —
/// those stay `doctor`-only; `status` stays the quick overview.
///
/// Order mirrors `doctor::run`'s own ordering for the same checks, so
/// a user who runs both commands sees the same relative placement.
/// K9 is included via `check_k9_coexistence` itself rather than the
/// raw `k9_detected` flag: that function already reports `Skipped`
/// when K9 isn't present (or is present-but-disabled), so a caller
/// that hides `Skipped` checks — as `status`'s own human view does —
/// naturally shows K9 output only when there's something to say.
pub fn checks(start: &Path) -> Result<Vec<doctor::Check>> {
    let ctx = db::open_project(start)?;
    Ok(vec![
        doctor::check_schema(&ctx.conn),
        doctor::check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config),
        doctor::check_k9_coexistence(&ctx.paths.repo_root, &ctx.config.integrations),
        doctor::check_retrieval_mode(&ctx.config),
        doctor::check_embeddings_freshness(start, &ctx.config),
        doctor::check_metrics_health(&ctx.conn, &ctx.config),
        doctor::check_sync_freshness(start, &ctx.config),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::tempdir;

    #[test]
    fn checks_reuses_the_documented_subsystem_subset_in_order() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let result = checks(temp.path()).expect("status checks");
        let ids: Vec<&str> = result.iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec![
                "schema",
                "render_freshness",
                "k9_coexistence",
                "retrieval_mode",
                "embeddings_freshness",
                "metrics_health",
                "sync_freshness",
            ]
        );
    }

    #[test]
    fn checks_k9_is_skipped_when_not_detected() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let result = checks(temp.path()).expect("status checks");
        let k9 = result
            .iter()
            .find(|c| c.id == "k9_coexistence")
            .expect("k9_coexistence check present");
        assert_eq!(k9.status, doctor::Status::Skipped);
    }
}
