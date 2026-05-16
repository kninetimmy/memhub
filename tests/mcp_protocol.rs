//! End-to-end MCP protocol smoke tests.
//!
//! Spawns `memhub serve` as a subprocess and runs the JSON-RPC handshake
//! over stdio. Exists because the `#[tool_handler]` attribute on the
//! `ServerHandler` impl is load-bearing — if it's ever removed, the
//! server still initializes and identifies itself, but `tools/list`
//! silently returns an empty array. The in-process unit tests don't
//! catch that; only a real handshake does.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use memhub::commands::{decision, fact, init};
use serde_json::{Value, json};
use tempfile::tempdir;

fn memhub_bin() -> &'static str {
    env!("CARGO_BIN_EXE_memhub")
}

fn send_handshake(repo: &std::path::Path) -> Vec<Value> {
    let mut child = Command::new(memhub_bin())
        .arg("serve")
        .current_dir(repo)
        .env("MEMHUB_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn memhub serve");

    let stdin = child.stdin.as_mut().expect("stdin");
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "codex", "version": "test"}
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let list_tools = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let call_recall = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "recall",
            "arguments": {"query": "stage agent writes", "max_results": 2}
        }
    });

    for msg in [&initialize, &initialized, &list_tools, &call_recall] {
        writeln!(stdin, "{msg}").expect("write json-rpc");
    }
    // Close stdin so the server exits cleanly after responding.
    drop(child.stdin.take());

    let stdout = child.stdout.take().expect("stdout");
    let mut responses = Vec::new();
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let line = line.expect("read line");
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => responses.push(v),
            Err(_) => continue,
        }
        // We expect exactly 3 responses (init, tools/list, tools/call).
        if responses.len() >= 3 {
            break;
        }
    }

    // Give the child a moment to exit on its own, then kill if needed.
    let _ = child.wait_timeout(Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();

    responses
}

#[test]
fn tools_list_exposes_full_tool_surface() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let responses = send_handshake(temp.path());
    let list = responses
        .iter()
        .find(|r| r["id"] == json!(2))
        .expect("tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");

    let names: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();

    // The MCP surface per addendum §8 and PR4. If any of these go
    // missing the server is silently broken — symptom is an empty
    // tools array even though the server initializes fine.
    let required = [
        "status",
        "search",
        "recall",
        "list_tasks",
        "list_decisions",
        "list_facts",
        "list_pending_writes",
        "get_command",
        "task_add",
        "task_done",
        "record_command",
        "log_session_note",
        "render",
        "propose_fact",
        "propose_decision",
    ];
    for name in required {
        assert!(
            names.contains(name),
            "tools/list missing `{name}`; got {names:?}",
        );
    }
}

#[test]
fn recall_tool_call_round_trips_through_mcp() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    fact::add(temp.path(), "build", "cargo build", "user", "cli:user").expect("fact");
    decision::add(
        temp.path(),
        "Stage agent writes before promotion",
        "Agents may propose facts and decisions but durable rows require human review.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision");

    let responses = send_handshake(temp.path());
    let call = responses
        .iter()
        .find(|r| r["id"] == json!(3))
        .expect("tools/call response");
    let body = &call["result"]["structuredContent"];
    let results = body["results"]
        .as_array()
        .expect("results array in structuredContent");

    assert!(!results.is_empty(), "expected at least one recall hit");
    let top = &results[0];
    assert_eq!(top["source_type"], "decision");
    assert!(
        top["title"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("stage"),
        "unexpected top hit: {top}",
    );
}

// Tiny helper to keep the spawn loop terminating on timeout. Avoid pulling
// in the `wait_timeout` crate; do it with a manual poll loop.
trait WaitTimeoutExt {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeoutExt for std::process::Child {
    fn wait_timeout(&mut self, dur: Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if start.elapsed() >= dur {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
