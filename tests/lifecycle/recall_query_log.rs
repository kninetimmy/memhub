//! Issue #324: opt-in `[retrieval] log_queries` JSONL log of agent-facing
//! recalls (`.memhub/recall_queries.jsonl`). Runs the real binary so the
//! CLI and MCP call sites are exercised end to end.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use memhub::commands::{fact, init};
use memhub::config::{ProjectConfig, RetrievalMode};
use serde_json::{Value, json};
use tempfile::tempdir;

const LOG_FILE: &str = ".memhub/recall_queries.jsonl";
/// The seeded fact is the first row inserted into a fresh database.
const FACT_ID: i64 = 1;

fn memhub_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memhub")
}

fn seeded_repo(log_queries: bool) -> tempfile::TempDir {
    seeded_repo_in_mode(log_queries, RetrievalMode::Fts)
}

/// Sets the mode before the fact is written so a hybrid repo embeds it.
fn seeded_repo_in_mode(log_queries: bool, mode: RetrievalMode) -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let cfg_path = temp.path().join(".memhub/config.toml");
    let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
    assert!(!cfg.retrieval.log_queries, "log_queries must default off");
    cfg.retrieval.log_queries = log_queries;
    cfg.retrieval.mode = mode;
    cfg.save(&cfg_path).expect("save cfg");
    let (id, _) = fact::add(temp.path(), "build", "cargo build", "user", "cli:user").expect("fact");
    assert_eq!(id, FACT_ID);
    temp
}

fn run_cli(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(memhub_bin())
        .args(args)
        .current_dir(repo)
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub binary")
}

fn cli_recall(repo: &Path) -> std::process::Output {
    run_cli(
        repo,
        &[
            "recall",
            "cargo build",
            "--mode",
            "fts",
            "--source-type",
            "fact",
            "--max-results",
            "3",
            "--variant",
            "compile command",
            "--json",
        ],
    )
}

fn log_lines(repo: &Path) -> Vec<Value> {
    fs::read_to_string(repo.join(LOG_FILE))
        .expect("read log")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each log line is valid JSON"))
        .collect()
}

fn assert_line_fields(line: &Value, surface: &str) {
    assert_eq!(line["surface"], surface);
    assert_eq!(line["query"], "cargo build");
    assert_eq!(line["variants"], json!(["compile command"]));
    assert_eq!(line["source_types"], json!(["fact"]));
    assert_eq!(line["max_results"], 3);
    let ts = line["ts"].as_str().expect("ts string");
    assert!(ts.ends_with('Z') && ts.contains('T'), "UTC ts: {ts}");
    let results = line["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "exactly the seeded fact: {line}");
    // FTS mode runs no re-ranker, so the score is null and nothing is flagged.
    assert_eq!(
        results[0],
        json!({
            "source_type": "fact",
            "source_id": FACT_ID,
            "scope": "repo",
            "title": "build",
            "rerank_score": null,
            "low_confidence": false,
        }),
        "{line}"
    );
    assert_eq!(line["warnings"], json!([]));
}

#[test]
fn log_is_off_by_default_and_writes_nothing() {
    let temp = seeded_repo(false);
    let out = cli_recall(temp.path());
    assert!(out.status.success(), "{out:?}");
    assert!(!temp.path().join(LOG_FILE).exists());
}

#[test]
fn cli_recall_appends_one_line_per_call() {
    let temp = seeded_repo(true);
    let out = cli_recall(temp.path());
    assert!(out.status.success(), "{out:?}");
    let lines = log_lines(temp.path());
    assert_eq!(lines.len(), 1);
    assert_line_fields(&lines[0], "cli");

    assert!(cli_recall(temp.path()).status.success());
    assert_eq!(log_lines(temp.path()).len(), 2);
}

#[test]
fn mcp_recall_appends_one_line_per_call() {
    let temp = seeded_repo(true);
    let mut child = Command::new(memhub_bin())
        .arg("serve")
        .current_dir(temp.path())
        .env("MEMHUB_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn memhub serve");
    let stdin = child.stdin.as_mut().expect("stdin");
    for msg in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "codex", "version": "test"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "recall",
            "arguments": {"query": "cargo build", "mode": "fts", "max_results": 3,
                          "source_types": ["fact"], "variants": ["compile command"]}}}),
    ] {
        writeln!(stdin, "{msg}").expect("write json-rpc");
    }
    let stdout = child.stdout.take().expect("stdout");
    let mut got_recall = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read line");
        if let Ok(v) = serde_json::from_str::<Value>(&line)
            && v["id"] == json!(2)
        {
            got_recall = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(got_recall, "no recall response from serve");

    let lines = log_lines(temp.path());
    assert_eq!(lines.len(), 1);
    assert_line_fields(&lines[0], "mcp");
}

#[test]
fn hybrid_recall_logs_a_numeric_rerank_score_and_the_effective_max_results() {
    let temp = seeded_repo_in_mode(true, RetrievalMode::Hybrid);
    // A floor no logit can fall below keeps `low_confidence` deterministic
    // (false) whatever score the model gives the seeded fact.
    let cfg_path = temp.path().join(".memhub/config.toml");
    let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
    cfg.retrieval.scoring.min_rerank_score = -1000.0;
    cfg.save(&cfg_path).expect("save cfg");
    // No --max-results: the log holds the resolved config default.
    let out = run_cli(
        temp.path(),
        &["recall", "cargo build", "--source-type", "fact", "--json"],
    );
    assert!(out.status.success(), "{out:?}");
    let lines = log_lines(temp.path());
    assert_eq!(lines.len(), 1);
    let default = ProjectConfig::load(&temp.path().join(".memhub/config.toml"))
        .expect("load cfg")
        .retrieval
        .default_max_results;
    assert_eq!(lines[0]["max_results"], default);
    let row = &lines[0]["results"][0];
    assert_eq!(row["source_id"], FACT_ID);
    assert_eq!(row["title"], "build");
    assert_eq!(row["low_confidence"], false, "{}", lines[0]);
    assert!(row["rerank_score"].is_number(), "{}", lines[0]);
}

#[test]
fn warning_kinds_are_logged() {
    let temp = seeded_repo_in_mode(true, RetrievalMode::Hybrid);
    let ctx = memhub::db::open_project(temp.path()).expect("open project");
    ctx.conn
        .execute(
            "DELETE FROM embeddings WHERE source_type = 'fact' AND source_id = ?1",
            [FACT_ID],
        )
        .expect("drop the fact's embedding");
    drop(ctx);
    let out = run_cli(
        temp.path(),
        &["recall", "cargo build", "--source-type", "fact", "--json"],
    );
    assert!(out.status.success(), "{out:?}");
    let lines = log_lines(temp.path());
    assert_eq!(lines[0]["warnings"], json!(["stale_embeddings"]));
}

#[test]
fn eval_retrieval_never_writes_the_log() {
    let temp = seeded_repo(true);
    let golden = temp.path().join("golden.json");
    fs::write(
        &golden,
        r#"{"version":1,"queries":[{"id":"probe","query":"cargo build","kind":"empty"}]}"#,
    )
    .expect("write golden");
    let out = run_cli(
        temp.path(),
        &[
            "eval",
            "retrieval",
            "--golden",
            golden.to_str().expect("utf8 path"),
            "--mode",
            "fts",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(!temp.path().join(LOG_FILE).exists());
}

#[test]
fn log_write_failure_is_stderr_only_and_does_not_change_recall() {
    let temp = seeded_repo(true);
    // A directory where the file should be makes the append fail.
    fs::create_dir(temp.path().join(LOG_FILE)).expect("block log path");
    let out = cli_recall(temp.path());
    assert!(out.status.success(), "{out:?}");
    let stdout: Value = serde_json::from_slice(&out.stdout).expect("stdout is still pure JSON");
    assert!(!stdout["results"].as_array().expect("results").is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("recall query log not written"),
        "{out:?}"
    );
}
