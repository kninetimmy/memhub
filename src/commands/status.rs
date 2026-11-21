use std::path::Path;

use crate::Result;
use crate::commands::doctor;
use crate::commands::fact;
use crate::commands::prune;
use crate::db;
use crate::models::StatusSummary;

pub fn run(start: &Path) -> Result<StatusSummary> {
    let ctx = db::open_project(start)?;
    summarize(start, ctx)
}

/// CLI status includes the prune review; MCP status exposes only memory counts.
pub fn run_with_prune(start: &Path) -> Result<(StatusSummary, usize)> {
    let ctx = db::open_project(start)?;
    let count = prune::scan_project(&ctx)?.total;
    Ok((summarize(start, ctx)?, count))
}

fn summarize(start: &Path, ctx: db::ProjectContext) -> Result<StatusSummary> {
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
pub fn checks(start: &Path) -> Result<Vec<doctor::Check>> {
    let ctx = db::open_project(start)?;
    Ok(vec![
        doctor::check_schema(&ctx.conn),
        doctor::check_render_freshness(&ctx.conn, &ctx.paths.repo_root, &ctx.config),
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
                "retrieval_mode",
                "embeddings_freshness",
                "metrics_health",
                "sync_freshness",
            ]
        );
    }

    #[test]
    fn run_reports_zero_prune_candidates_on_a_clean_repo() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let (_, count) = run_with_prune(temp.path()).expect("status");
        assert_eq!(count, 0);
    }

    #[test]
    fn run_prune_review_count_matches_scan() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let (id, _) = fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        let ctx = db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = datetime('now', '-400 days') WHERE id = ?1",
                rusqlite::params![id],
            )
            .expect("backdate");
        drop(ctx);

        let (_, count) = run_with_prune(temp.path()).expect("status");
        let report = prune::scan(temp.path()).expect("scan");
        assert_eq!(count, report.total);
        assert_eq!(count, 1);
    }
}
