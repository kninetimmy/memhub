use serde_json::json;

use crate::code_index::CodeIndexStatus;
use crate::commands::audit_md::{AuditMdReport, Finding, Severity};
use crate::commands::doctor::{Check, DoctorReport, Group, Status};
use crate::code_index::locate::LocateResponse;
use crate::commands;
use crate::commands::import::ImportSummary;
use crate::config::RetrievalMode;
use crate::models::{
    InitResult, NarrativeEntry, NarrativeKind, PendingWriteRecord, StatsSummary, StatusSummary,
};
use crate::retrieval::RecallResponse;

pub(crate) fn print_stats_human(s: &StatsSummary) {
    println!("memhub stats — project: {}", s.project_name);
    println!("Repo: {}", s.repo_root.display());
    println!("Window: {}", s.window_label);
    println!();
    println!("Totals");
    println!(
        "  Facts: {} ({} stale{})",
        s.facts,
        s.stale_facts,
        match s.stale_ratio {
            Some(r) => format!(", {:.0}% stale", r * 100.0),
            None => String::new(),
        }
    );
    println!("  Decisions: {}", s.decisions);
    println!("  Tasks: {} open / {} total", s.tasks_open, s.tasks_total);
    println!("  Commands: {}", s.commands);
    println!("  Commits: {}", s.commits);
    println!("  Files: {}", s.files);
    println!("  Search chunks: {}", s.chunks);
    println!("  Pending writes (open): {}", s.pending_writes_now);
    println!("  Writes logged (all time): {}", s.writes_logged_total);
    println!();
    println!("Activity ({})", s.window_label);
    println!("  Writes: {}", s.writes_in_window);
    if !s.writes_by_actor.is_empty() {
        println!("  By actor:");
        for row in &s.writes_by_actor {
            println!("    {:<24} {}", row.label, row.count);
        }
    }
    if !s.writes_by_table.is_empty() {
        println!("  By table:");
        for row in &s.writes_by_table {
            println!("    {:<24} {}", row.label, row.count);
        }
    }
    println!();
    println!("Pending writes ({})", s.window_label);
    println!("  Created: {}", s.pending_created_in_window);
    println!(
        "  Reviewed: {}{}",
        s.pending_reviewed_in_window,
        match s.review_rate {
            Some(r) => format!(" (review rate: {:.0}%)", r * 100.0),
            None => String::new(),
        }
    );
    if !s.pending_by_status.is_empty() {
        print!("  By status (all time):");
        for row in &s.pending_by_status {
            print!(" {}={}", row.label, row.count);
        }
        println!();
    }
    println!();
    if !s.top_command_kinds.is_empty() {
        println!("Top commands (by runs)");
        for c in &s.top_command_kinds {
            let conf = c
                .confidence
                .map(|v| format!("conf={:.2}", v))
                .unwrap_or_else(|| "conf=n/a".to_string());
            println!(
                "  {:<10} {}  ({}/{} runs)  {}",
                c.kind,
                conf,
                c.success_count,
                c.success_count + c.fail_count,
                c.cmdline,
            );
        }
        println!();
    }
    if !s.recent_facts.is_empty() {
        println!("Recent facts");
        for f in &s.recent_facts {
            let stamp = f.verified_at.as_deref().unwrap_or("never verified");
            let stale = if f.is_stale { " [stale]" } else { "" };
            println!("  {}  {}{}", stamp, f.key, stale);
        }
        println!();
    }
    println!(
        "Note: \"writes\" counts mutations recorded in writes_log. Read activity is not tracked in this slice; see PRD §17."
    );
}

pub(crate) fn print_stats_json(s: &StatsSummary) {
    let payload = json!({
        "project_name": s.project_name,
        "repo_root": s.repo_root.display().to_string(),
        "window": {
            "label": s.window_label,
            "days": s.window_days,
        },
        "totals": {
            "facts": s.facts,
            "stale_facts": s.stale_facts,
            "stale_ratio": s.stale_ratio,
            "decisions": s.decisions,
            "tasks_total": s.tasks_total,
            "tasks_open": s.tasks_open,
            "commands": s.commands,
            "commits": s.commits,
            "files": s.files,
            "chunks": s.chunks,
            "pending_writes_now": s.pending_writes_now,
            "writes_logged_total": s.writes_logged_total,
        },
        "activity": {
            "writes_in_window": s.writes_in_window,
            "writes_by_actor": s.writes_by_actor.iter().map(|r| json!({
                "label": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
            "writes_by_table": s.writes_by_table.iter().map(|r| json!({
                "label": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
        },
        "pending_writes": {
            "created_in_window": s.pending_created_in_window,
            "reviewed_in_window": s.pending_reviewed_in_window,
            "review_rate": s.review_rate,
            "by_status_all_time": s.pending_by_status.iter().map(|r| json!({
                "status": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
        },
        "top_command_kinds": s.top_command_kinds.iter().map(|c| json!({
            "kind": c.kind,
            "cmdline": c.cmdline,
            "success_count": c.success_count,
            "fail_count": c.fail_count,
            "confidence": c.confidence,
            "last_run_at": c.last_run_at,
        })).collect::<Vec<_>>(),
        "recent_facts": s.recent_facts.iter().map(|f| json!({
            "key": f.key,
            "verified_at": f.verified_at,
            "is_stale": f.is_stale,
        })).collect::<Vec<_>>(),
        "notes": [
            "writes counts mutations recorded in writes_log; read activity is not tracked in this slice (PRD §17 read counter deferred)",
        ],
    });
    println!("{payload}");
}

pub(crate) fn pending_write_record_to_json(row: &PendingWriteRecord) -> serde_json::Value {
    json!({
        "id": row.id,
        "kind": row.kind,
        "status": row.status,
        "actor": row.actor,
        "actor_raw": row.actor_raw,
        "rationale": row.rationale,
        "payload_json": row.payload_json,
        "provenance_json": row.provenance_json,
        "created_at": row.created_at,
        "reviewed_at": row.reviewed_at,
    })
}

pub(crate) fn narrative_entry_to_json(
    kind: NarrativeKind,
    entry: &NarrativeEntry,
) -> serde_json::Value {
    json!({
        "kind": kind.as_str(),
        "id": entry.id,
        "body": entry.body,
        "actor": entry.actor,
        "actor_raw": entry.actor_raw,
        "created_at": entry.created_at,
    })
}

pub(crate) fn index_status_to_json(s: &commands::index::IndexStatusSummary) -> serde_json::Value {
    json!({
        "model": s.model,
        "mode": recall_mode_label(s.mode),
        "facts": { "total": s.facts_total, "embedded": s.facts_embedded },
        "decisions": { "total": s.decisions_total, "embedded": s.decisions_embedded },
        "tasks": { "total": s.tasks_total, "embedded": s.tasks_embedded },
        "doc_chunks": { "total": s.doc_chunks_total, "embedded": s.doc_chunks_embedded },
        "total_embeddings": s.total_embeddings,
        "missing_count": s.missing_count,
        "stale_ratio": s.stale_ratio,
    })
}

pub(crate) fn print_index_status(s: &commands::index::IndexStatusSummary) {
    println!("Embedding model: {}", s.model);
    println!("Retrieval mode:  {}", recall_mode_label(s.mode));
    println!(
        "Facts:     {} embedded / {} total",
        s.facts_embedded, s.facts_total,
    );
    println!(
        "Decisions: {} embedded / {} total",
        s.decisions_embedded, s.decisions_total,
    );
    println!(
        "Tasks:     {} embedded / {} total",
        s.tasks_embedded, s.tasks_total,
    );
    println!(
        "Doc chunks:{} embedded / {} total",
        s.doc_chunks_embedded, s.doc_chunks_total,
    );
    println!("Total embeddings: {}", s.total_embeddings);
    println!(
        "Missing: {} ({:.0}% of source rows lack embeddings)",
        s.missing_count,
        s.stale_ratio * 100.0,
    );
    if s.missing_count > 0 {
        println!("Run `memhub index rebuild` (or /reindex) to refresh.");
    }
}

fn recall_mode_label(mode: RetrievalMode) -> &'static str {
    match mode {
        RetrievalMode::Fts => "fts",
        RetrievalMode::Hybrid => "hybrid",
    }
}

pub(crate) fn recall_response_to_json(response: &RecallResponse) -> serde_json::Value {
    let results = response
        .results
        .iter()
        .map(|hit| {
            json!({
                "rank": hit.rank,
                "source_type": hit.source_type,
                "scope": hit.scope,
                "source_id": hit.source_id,
                "title": hit.title,
                "body": hit.body,
                "score": hit.score,
                "fts_score": hit.fts_score,
                "vector_score": hit.vector_score,
                "stale": hit.stale,
                "source": hit.source,
                "created_at": hit.created_at,
                "rerank_score": hit.rerank_score,
            })
        })
        .collect::<Vec<_>>();
    let warnings = response
        .warnings
        .iter()
        .map(|w| {
            json!({
                "kind": w.kind,
                "stale_count": w.stale_count,
                "total_count": w.total_count,
                "reason": w.reason,
                "fix": w.fix,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "query": response.query,
        "mode": recall_mode_label(response.mode),
        "results": results,
        "candidate_count": response.candidate_count,
        "returned_count": response.returned_count,
        "available_docs": response.available_docs,
        "warnings": warnings,
        "provenance": {
            "matcher": response.matcher,
            "elapsed_ms": response.elapsed_ms,
        },
    })
}

pub(crate) fn print_recall_human(response: &RecallResponse) {
    println!(
        "Query: {}  (mode: {}, matcher: {}, elapsed: {} ms)",
        response.query,
        recall_mode_label(response.mode),
        response.matcher,
        response.elapsed_ms,
    );
    println!(
        "Candidates: {} | Returned: {}",
        response.candidate_count, response.returned_count,
    );
    if response.available_docs > 0 {
        println!(
            "Note: {} ingested doc chunk(s) not searched — re-run with --source-type doc to include them.",
            response.available_docs,
        );
    }
    if response.results.is_empty() {
        println!("No matches.");
    } else {
        println!();
        for hit in &response.results {
            let stale_tag = if hit.stale { " [stale]" } else { "" };
            // Only annotate global provenance; repo is the unmarked
            // default so existing output stays visually unchanged.
            let scope_tag = if hit.scope == "global" {
                " [global]"
            } else {
                ""
            };
            let source_label = if hit.source.is_empty() {
                String::new()
            } else {
                format!(" source={}", hit.source)
            };
            println!(
                "#{rank} [{stype}:{sid}] {title}{scope}{stale}  score={score:.3} (fts={fts:.3}, vec={vec:.3}){src}",
                rank = hit.rank,
                stype = hit.source_type,
                sid = hit.source_id,
                scope = scope_tag,
                title = hit.title,
                stale = stale_tag,
                score = hit.score,
                fts = hit.fts_score,
                vec = hit.vector_score,
                src = source_label,
            );
            if !hit.body.is_empty() {
                println!("    {}", hit.body);
            }
        }
    }
    if !response.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for warn in &response.warnings {
            println!(
                "  {} ({}/{}): {} — {}",
                warn.kind, warn.stale_count, warn.total_count, warn.reason, warn.fix,
            );
        }
    }
}

pub(crate) fn eval_summary_to_json(summary: &commands::eval::EvalSummary) -> serde_json::Value {
    let outcomes = summary
        .outcomes
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "query": o.query,
                "kind": match o.kind {
                    commands::eval::GoldenKind::Match => "match",
                    commands::eval::GoldenKind::Empty => "empty",
                },
                "passed": o.passed,
                "matched_rank": o.matched_rank,
                "matched_score": o.matched_score,
                "returned_count": o.returned_count,
                "failure_reason": o.failure_reason,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "golden_path": summary.golden_path.display().to_string(),
        "mode": recall_mode_label(summary.mode),
        "k": summary.k,
        "totals": {
            "queries": summary.total_queries,
            "match_queries": summary.match_queries,
            "empty_queries": summary.empty_queries,
            "match_passes": summary.match_passes,
            "empty_passes": summary.empty_passes,
            "safety_failures": summary.safety_failures,
        },
        "recall_at_k": summary.recall_at_k,
        "elapsed_ms": summary.elapsed_ms,
        "outcomes": outcomes,
    })
}

pub(crate) fn print_eval_summary(summary: &commands::eval::EvalSummary) {
    println!(
        "memhub eval retrieval — {} ({} queries)",
        summary.golden_path.display(),
        summary.total_queries,
    );
    println!(
        "Mode: {}  |  K: {}  |  Elapsed: {} ms",
        recall_mode_label(summary.mode),
        summary.k,
        summary.elapsed_ms,
    );
    println!(
        "Recall@{k}: {passes}/{total} = {pct:.1}%",
        k = summary.k,
        passes = summary.match_passes,
        total = summary.match_queries,
        pct = summary.recall_at_k * 100.0,
    );
    if summary.empty_queries > 0 {
        println!(
            "Safety: {pass}/{total} empty-query probes returned no results{failed}",
            pass = summary.empty_passes,
            total = summary.empty_queries,
            failed = if summary.safety_failures > 0 {
                format!("  [{} FAILED]", summary.safety_failures)
            } else {
                String::new()
            },
        );
    }
    println!();
    println!("Per-query outcomes:");
    for outcome in &summary.outcomes {
        let glyph = if outcome.passed { "PASS" } else { "FAIL" };
        let kind = match outcome.kind {
            commands::eval::GoldenKind::Match => "match",
            commands::eval::GoldenKind::Empty => "empty",
        };
        let detail = match outcome.matched_rank {
            Some(rank) => format!(
                "rank {rank}, score {score:.3}",
                rank = rank,
                score = outcome.matched_score.unwrap_or(0.0),
            ),
            None => match outcome.kind {
                commands::eval::GoldenKind::Empty => {
                    format!("{} hit(s) returned", outcome.returned_count)
                }
                commands::eval::GoldenKind::Match => {
                    format!("{} hit(s) returned, no match", outcome.returned_count)
                }
            },
        };
        println!("  [{glyph}] {id} ({kind}) — {detail}", id = outcome.id,);
        if let Some(reason) = &outcome.failure_reason {
            println!("        {reason}");
        }
    }
}

pub(crate) fn locate_eval_summary_to_json(
    summary: &commands::eval::LocateEvalSummary,
) -> serde_json::Value {
    let outcomes = summary
        .outcomes
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "query": o.query,
                "kind": match o.kind {
                    commands::eval::GoldenKind::Match => "match",
                    commands::eval::GoldenKind::Empty => "empty",
                },
                "passed": o.passed,
                "passed_at_1": o.passed_at_1,
                "matched_rank": o.matched_rank,
                "matched_score": o.matched_score,
                "matched_rerank": o.matched_rerank,
                "returned_count": o.returned_count,
                "failure_reason": o.failure_reason,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "golden_path": summary.golden_path.display().to_string(),
        "mode": recall_mode_label(summary.mode),
        "k": summary.k,
        "reranked": summary.reranked,
        "min_rerank_score": summary.min_rerank_score,
        "totals": {
            "queries": summary.total_queries,
            "match_queries": summary.match_queries,
            "empty_queries": summary.empty_queries,
            "match_passes_at_1": summary.match_passes_at_1,
            "match_passes_at_k": summary.match_passes_at_k,
            "empty_passes": summary.empty_passes,
            "safety_failures": summary.safety_failures,
        },
        "recall_at_1": summary.recall_at_1,
        "recall_at_k": summary.recall_at_k,
        "elapsed_ms": summary.elapsed_ms,
        "outcomes": outcomes,
    })
}

pub(crate) fn print_locate_eval_summary(summary: &commands::eval::LocateEvalSummary) {
    println!(
        "memhub eval locate — {} ({} queries)",
        summary.golden_path.display(),
        summary.total_queries,
    );
    let floor = match summary.min_rerank_score {
        Some(f) if summary.reranked => format!("  |  Floor: {f:.2}"),
        _ => String::new(),
    };
    println!(
        "Mode: {}{}  |  K: {}{}  |  Elapsed: {} ms",
        recall_mode_label(summary.mode),
        if summary.reranked { " +rerank" } else { "" },
        summary.k,
        floor,
        summary.elapsed_ms,
    );
    println!(
        "Recall@1: {p1}/{total} = {pct1:.1}%   Recall@{k}: {pk}/{total} = {pctk:.1}%",
        p1 = summary.match_passes_at_1,
        pk = summary.match_passes_at_k,
        total = summary.match_queries,
        k = summary.k,
        pct1 = summary.recall_at_1 * 100.0,
        pctk = summary.recall_at_k * 100.0,
    );
    if summary.empty_queries > 0 {
        println!(
            "Safety: {pass}/{total} nonsense probes returned no results{failed}",
            pass = summary.empty_passes,
            total = summary.empty_queries,
            failed = if summary.safety_failures > 0 {
                format!("  [{} leaked]", summary.safety_failures)
            } else {
                String::new()
            },
        );
    }
    println!();
    println!("Per-query outcomes:");
    for outcome in &summary.outcomes {
        let glyph = if outcome.passed { "PASS" } else { "FAIL" };
        let kind = match outcome.kind {
            commands::eval::GoldenKind::Match => "match",
            commands::eval::GoldenKind::Empty => "empty",
        };
        let detail = match outcome.matched_rank {
            Some(rank) => format!(
                "rank {rank}, score {score:.3}{rerank}",
                score = outcome.matched_score.unwrap_or(0.0),
                rerank = match outcome.matched_rerank {
                    Some(r) => format!(", rerank {r:.2}"),
                    None => String::new(),
                },
            ),
            None => match outcome.kind {
                commands::eval::GoldenKind::Empty => {
                    format!("{} hit(s) returned", outcome.returned_count)
                }
                commands::eval::GoldenKind::Match => {
                    format!("{} hit(s) returned, no match", outcome.returned_count)
                }
            },
        };
        println!("  [{glyph}] {id} ({kind}) — {detail}", id = outcome.id);
        if let Some(reason) = &outcome.failure_reason {
            println!("        {reason}");
        }
    }
}

pub(crate) fn print_metrics_status_human(s: &commands::metrics::MetricsStatus) {
    let status_label = if s.config.enabled {
        "enabled"
    } else {
        "disabled"
    };
    println!("memhub metrics — {status_label}");
    println!(
        "  session_accounting: {}",
        if s.config.session_accounting {
            "on"
        } else {
            "off"
        }
    );
    println!("  tokenizer:          {}", s.config.tokenizer);
    if (s.config.calibration_factor - 1.0).abs() < f64::EPSILON {
        println!("  calibration:        uncalibrated (run `memhub metrics calibrate`)");
    } else {
        println!(
            "  calibration:        {:.4}x (vs Anthropic count_tokens)",
            s.config.calibration_factor
        );
    }
    println!(
        "  retention:          {}",
        if s.config.retention_days == 0 {
            "keep forever".to_string()
        } else {
            format!("{} days", s.config.retention_days)
        }
    );
    let transcripts = if s.config.claude_transcripts_dir.is_empty() {
        "(not set)".to_string()
    } else {
        s.config.claude_transcripts_dir.clone()
    };
    println!("  claude transcripts: {transcripts}");
    println!();
    println!(
        "  Recall rows:  {} ({} attributed to sessions)",
        s.recall_rows, s.attributed_rows
    );
    println!("  Sessions:     {}", s.session_rows);
    if let Some(sess) = &s.current_session {
        let id = &sess.session_id[..sess.session_id.len().min(8)];
        println!();
        println!("  Current session ({id}):");
        println!(
            "    started:      {}",
            &sess.started_at[..sess.started_at.len().min(16)]
        );
        println!("    input tokens: {}", sess.input_tokens);
        println!("    output tokens:{}", sess.output_tokens);
        println!("    cache read:   {}", sess.cache_read_tokens);
        println!("    recalls:      {}", sess.recall_calls);
    }
    if let Some(t) = &s.token_totals_7d {
        println!();
        println!("  Last 7 days:");
        println!("    input tokens:         {}", t.input);
        println!("    output tokens:        {}", t.output);
        println!("    cache read tokens:    {}", t.cache_read);
        println!("    cache creation tokens:{}", t.cache_creation);
    }
    if let Some(t) = &s.token_totals {
        println!();
        println!("  Last 30 days:");
        println!("    input tokens:         {}", t.input);
        println!("    output tokens:        {}", t.output);
        println!("    cache read tokens:    {}", t.cache_read);
        println!("    cache creation tokens:{}", t.cache_creation);
    }
    if !s.recent_sessions.is_empty() {
        println!();
        println!("  Recent sessions (newest first):");
        for sess in &s.recent_sessions {
            let ended = sess.ended_at.as_deref().unwrap_or("(open)");
            println!(
                "    {}  agent={}  {}..{}  in={} out={} cread={} ccreate={} recalls={}",
                &sess.session_id[..sess.session_id.len().min(8)],
                sess.agent,
                &sess.started_at[..sess.started_at.len().min(19)],
                &ended[..ended.len().min(19)],
                sess.input_tokens,
                sess.output_tokens,
                sess.cache_read_tokens,
                sess.cache_creation_tokens,
                sess.recall_calls,
            );
        }
    }
    if s.recalls_pruned > 0 || s.sessions_pruned > 0 {
        println!();
        println!(
            "  Pruned this pass: {} recalls, {} sessions",
            s.recalls_pruned, s.sessions_pruned
        );
    }
}

pub(crate) fn metrics_status_to_json(s: &commands::metrics::MetricsStatus) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = s
        .recent_sessions
        .iter()
        .map(|sess| {
            json!({
                "session_id": sess.session_id,
                "agent": sess.agent,
                "started_at": sess.started_at,
                "ended_at": sess.ended_at,
                "input_tokens": sess.input_tokens,
                "output_tokens": sess.output_tokens,
                "cache_read_tokens": sess.cache_read_tokens,
                "cache_creation_tokens": sess.cache_creation_tokens,
                "recall_calls": sess.recall_calls,
            })
        })
        .collect();
    json!({
        "config": {
            "enabled": s.config.enabled,
            "session_accounting": s.config.session_accounting,
            "tokenizer": s.config.tokenizer,
            "calibration_factor": s.config.calibration_factor,
            "retention_days": s.config.retention_days,
            "claude_transcripts_dir": s.config.claude_transcripts_dir,
            "codex_transcripts_dir": s.config.codex_transcripts_dir,
        },
        "stats": {
            "recall_rows": s.recall_rows,
            "attributed_rows": s.attributed_rows,
            "session_rows": s.session_rows,
        },
        "current_session": s.current_session.as_ref().map(|sess| json!({
            "session_id": sess.session_id,
            "agent": sess.agent,
            "started_at": sess.started_at,
            "ended_at": sess.ended_at,
            "input_tokens": sess.input_tokens,
            "output_tokens": sess.output_tokens,
            "cache_read_tokens": sess.cache_read_tokens,
            "cache_creation_tokens": sess.cache_creation_tokens,
            "recall_calls": sess.recall_calls,
        })),
        "token_totals_7d": s.token_totals_7d.as_ref().map(|t| json!({
            "input": t.input,
            "output": t.output,
            "cache_read": t.cache_read,
            "cache_creation": t.cache_creation,
        })),
        "token_totals_30d": s.token_totals.as_ref().map(|t| json!({
            "input": t.input,
            "output": t.output,
            "cache_read": t.cache_read,
            "cache_creation": t.cache_creation,
        })),
        "recent_sessions": sessions,
        "pruned": {
            "recalls": s.recalls_pruned,
            "sessions": s.sessions_pruned,
        },
    })
}

pub(crate) fn print_init_result(result: &InitResult) {
    println!("Initialized memhub at {}", result.repo_root.display());
    println!("Database: {}", result.db_path.display());
    println!(
        "Config created: {}",
        if result.config_created { "yes" } else { "no" }
    );
    println!(
        ".memhub existed already: {}",
        if result.memhub_preexisting {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        ".gitignore updated: {}",
        if result.gitignore_updated {
            "yes"
        } else {
            "no"
        }
    );
    if result.migrations_applied.is_empty() {
        println!("Migrations applied: none");
    } else {
        println!(
            "Migrations applied: {}",
            result.migrations_applied.join(", ")
        );
    }
}

pub(crate) fn init_result_to_json(result: &InitResult) -> serde_json::Value {
    json!({
        "repo_root": result.repo_root.display().to_string(),
        "db_path": result.db_path.display().to_string(),
        "config_created": result.config_created,
        "gitignore_updated": result.gitignore_updated,
        "memhub_preexisting": result.memhub_preexisting,
        "migrations_applied": result.migrations_applied,
    })
}

pub(crate) fn import_summary_to_json(summary: &ImportSummary) -> serde_json::Value {
    json!({
        "source": summary.source.display().to_string(),
        "target_root": summary.target_root.display().to_string(),
        "forced": summary.forced,
        "facts": summary.facts,
        "decisions": summary.decisions,
        "tasks": summary.tasks,
        "commands": summary.commands,
        "pending_writes": summary.pending_writes,
        "writes_log": summary.writes_log,
        "session_notes": summary.session_notes,
        "project_state": summary.project_state,
        "project_arch": summary.project_arch,
        "retained_doc_chunks": summary.retained_doc_chunks,
    })
}

pub(crate) fn status_summary_to_json(s: &StatusSummary) -> serde_json::Value {
    json!({
        "project_name": s.project_name,
        "repo_root": s.repo_root.display().to_string(),
        "db_path": s.db_path.display().to_string(),
        "config_path": s.config_path.display().to_string(),
        "schema_version": s.schema_version,
        "facts": s.facts,
        "stale_facts": s.stale_facts,
        "decisions": s.decisions,
        "tasks_open": s.tasks_open,
        "tasks_total": s.tasks_total,
        "commands": s.commands,
        "commits": s.commits,
        "files": s.files,
        "chunks": s.chunks,
        "pending_writes": s.pending_writes,
        "writes_logged": s.writes_logged,
        "deny_patterns": s.deny_patterns,
        "k9_detected": s.k9_detected,
        "k9_enabled": s.k9_enabled,
        "k9_agent_docs_path": s.k9_agent_docs_path,
        "k9_drift": s.k9_drift,
    })
}

/// Subsystem-state checks (issue #22) as a JSON array, same per-check
/// shape as `doctor --json`'s own `checks` array (via the shared
/// `check_to_json`) — a superset-compatible addition alongside
/// `status_summary_to_json`'s existing keys, not a replacement.
pub(crate) fn status_checks_to_json(checks: &[Check]) -> serde_json::Value {
    json!(checks.iter().map(check_to_json).collect::<Vec<_>>())
}

/// Human-readable subsystem-state lines for `status` (issue #22).
/// Unlike `print_doctor_report_human`, `Skipped` checks are omitted
/// entirely rather than shown with a `·` glyph — `status` is the quick
/// overview, so a not-applicable/disabled subsystem (K9 not detected,
/// sync/metrics disabled, embeddings n/a in fts mode, ...) says
/// nothing rather than spraying placeholder lines. Nothing is printed
/// at all if every check is skipped.
pub(crate) fn print_status_checks_human(checks: &[Check]) {
    let visible: Vec<&Check> = checks
        .iter()
        .filter(|c| c.status != Status::Skipped)
        .collect();
    if visible.is_empty() {
        return;
    }

    println!();
    println!("Subsystems:");
    for c in visible {
        println!("  {} {}: {}", status_glyph(c.status), c.id, c.message);
        if let Some(detail) = &c.detail {
            println!("      {detail}");
        }
    }
}

pub(crate) fn locate_response_to_json(response: &LocateResponse) -> serde_json::Value {
    let results = response
        .results
        .iter()
        .map(|hit| {
            json!({
                "rank": hit.rank,
                "path": hit.path,
                "start_line": hit.start_line,
                "end_line": hit.end_line,
                "symbol": hit.symbol,
                "kind": hit.kind,
                "score": hit.score,
                "fts_score": hit.fts_score,
                "vector_score": hit.vector_score,
                "rerank_score": hit.rerank_score,
                "snippet": hit.snippet,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "query": response.query,
        "mode": recall_mode_label(response.mode),
        "results": results,
        "candidate_count": response.candidate_count,
        "returned_count": response.returned_count,
        "reranked": response.reranked,
        "files_total": response.files_total,
        "chunks_total": response.chunks_total,
        "head": response.head,
        "elapsed_ms": response.elapsed_ms,
    })
}

pub(crate) fn print_locate(response: &LocateResponse) {
    println!(
        "locate \"{}\" — mode: {}{} ({} files, {} chunks indexed)",
        response.query,
        recall_mode_label(response.mode),
        if response.reranked { " +rerank" } else { "" },
        response.files_total,
        response.chunks_total,
    );
    if response.results.is_empty() {
        println!("  no matches.");
        return;
    }
    for hit in &response.results {
        let symbol = hit.symbol.as_deref().unwrap_or("—");
        println!(
            "{:>2}. {}:{}-{}  [{} {}]  score {:.3}{}",
            hit.rank,
            hit.path,
            hit.start_line,
            hit.end_line,
            hit.kind,
            symbol,
            hit.score,
            match hit.rerank_score {
                Some(s) => format!("  rerank {s:.2}"),
                None => String::new(),
            },
        );
        for line in hit.snippet.lines() {
            println!("      {line}");
        }
    }
    println!(
        "({} of {} candidates, {} ms)",
        response.returned_count, response.candidate_count, response.elapsed_ms
    );
}

pub(crate) fn code_status_to_json(status: &CodeIndexStatus) -> serde_json::Value {
    json!({
        "db_path": status.db_path,
        "exists": status.exists,
        "schema_version": status.schema_version,
        "mode": recall_mode_label(status.mode),
        "files_total": status.files_total,
        "chunks_total": status.chunks_total,
        "embeddings_total": status.embeddings_total,
        "last_head": status.last_head,
        "current_head": status.current_head,
        "head_stale": status.head_stale(),
    })
}

pub(crate) fn print_code_status(status: &CodeIndexStatus) {
    println!("Code index: {}", status.db_path.display());
    if !status.exists {
        println!("  not built yet — run `memhub code index` or `memhub locate <query>`.");
        return;
    }
    println!(
        "  schema version: {}",
        status
            .schema_version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!("  mode:           {}", recall_mode_label(status.mode));
    println!("  files:          {}", status.files_total);
    println!("  chunks:         {}", status.chunks_total);
    println!("  embeddings:     {}", status.embeddings_total);
    println!(
        "  indexed HEAD:   {}",
        status.last_head.as_deref().unwrap_or("(none)")
    );
    println!(
        "  current HEAD:   {}",
        status.current_head.as_deref().unwrap_or("(none)")
    );
    if status.head_stale() {
        println!("  staleness:      HEAD moved since last index — run `memhub code index`.");
    } else {
        println!("  staleness:      up to date with HEAD");
    }
}

pub(crate) fn doctor_report_to_json(r: &DoctorReport) -> serde_json::Value {
    json!({
        "project": r.project,
        "overall": r.overall.as_str(),
        "exit_code": r.exit_code,
        "counts": {
            "ok": r.counts.ok,
            "warn": r.counts.warn,
            "error": r.counts.error,
            "skipped": r.counts.skipped,
        },
        "checks": r.checks.iter().map(check_to_json).collect::<Vec<_>>(),
    })
}

fn check_to_json(c: &Check) -> serde_json::Value {
    let mut v = json!({
        "id": c.id,
        "group": c.group.as_str(),
        "status": c.status.as_str(),
        "message": c.message,
    });
    if let Some(detail) = &c.detail {
        v["detail"] = json!(detail);
    }
    v
}

fn status_glyph(status: Status) -> &'static str {
    match status {
        Status::Ok => "\u{2714}",      // ✔
        Status::Warn => "\u{26A0}",    // ⚠
        Status::Error => "\u{2716}",   // ✖
        Status::Skipped => "\u{00B7}", // ·
    }
}

const DOCTOR_GROUP_ORDER: &[Group] = &[
    Group::Project,
    Group::Config,
    Group::Integrity,
    Group::RetrievalMetrics,
    Group::Integrations,
];

fn group_heading(group: Group) -> &'static str {
    match group {
        Group::Project => "Project",
        Group::Config => "Config",
        Group::Integrity => "Integrity",
        Group::RetrievalMetrics => "Retrieval / Metrics",
        Group::Integrations => "Integrations",
    }
}

pub(crate) fn print_doctor_report_human(r: &DoctorReport) {
    println!("memhub doctor — project: {}", r.project);
    println!();

    for &group in DOCTOR_GROUP_ORDER {
        let in_group: Vec<&Check> = r.checks.iter().filter(|c| c.group == group).collect();
        if in_group.is_empty() {
            continue;
        }
        println!("{}", group_heading(group));
        for c in in_group {
            println!("  {} {}: {}", status_glyph(c.status), c.id, c.message);
            if let Some(detail) = &c.detail {
                println!("      {detail}");
            }
        }
        println!();
    }

    println!(
        "Summary: {} ok · {} warn · {} error → exit {}",
        r.counts.ok, r.counts.warn, r.counts.error, r.exit_code
    );
}

/// `{"audit_md": {...}}` — the wrapped noun-keyed shape (Q29), matching
/// `doctor`'s own `{"doctor": {...}}` sibling. `count` is redundant with
/// `findings.len()` but is included as a cheap convenience for scripts
/// that only want to check "any findings?" without counting the array.
pub(crate) fn audit_md_report_to_json(r: &AuditMdReport) -> serde_json::Value {
    json!({
        "exit_code": r.exit_code,
        "count": r.findings.len(),
        "findings": r.findings.iter().map(finding_to_json).collect::<Vec<_>>(),
    })
}

fn finding_to_json(f: &Finding) -> serde_json::Value {
    let mut v = json!({
        "id": f.id,
        "severity": f.severity.as_str(),
        "message": f.message,
    });
    if let Some(detail) = &f.detail {
        v["detail"] = json!(detail);
    }
    v
}

fn severity_glyph(severity: Severity) -> &'static str {
    match severity {
        Severity::Warn => "\u{26A0}",  // ⚠
        Severity::Error => "\u{2716}", // ✖
    }
}

pub(crate) fn print_audit_md_report_human(r: &AuditMdReport) {
    println!("memhub audit md");
    println!();

    if r.findings.is_empty() {
        println!("No findings.");
        return;
    }

    for f in &r.findings {
        println!(
            "  {} {} [{}]: {}",
            severity_glyph(f.severity),
            f.id,
            f.severity.as_str(),
            f.message
        );
        if let Some(detail) = &f.detail {
            println!("      {detail}");
        }
    }
    println!();
    println!(
        "Summary: {} finding(s) → exit {}",
        r.findings.len(),
        r.exit_code
    );
}
