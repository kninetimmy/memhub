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
        "archive_transcript",
    ];
    for name in required {
        assert!(
            names.contains(name),
            "tools/list missing `{name}`; got {names:?}",
        );
    }

    // Issue #318: `recall` advertises an optional boolean `explain`
    // parameter with a description of what it reports.
    let recall = tools
        .iter()
        .find(|t| t["name"] == json!("recall"))
        .expect("recall tool");
    let explain = &recall["inputSchema"]["properties"]["explain"];
    assert!(
        explain.to_string().contains("boolean"),
        "recall.explain must be a boolean: {explain}"
    );
    assert!(
        explain["description"]
            .as_str()
            .is_some_and(|d| d.contains("dropped")),
        "recall.explain must describe what it reports: {explain}"
    );
    assert!(
        !recall["inputSchema"]["required"]
            .to_string()
            .contains("explain"),
        "recall.explain must be optional"
    );

    // Issue #319: `recall` advertises an optional `variants` string list and
    // tells the agent when to send it.
    let variants = &recall["inputSchema"]["properties"]["variants"];
    assert!(
        variants.to_string().contains("array") && variants.to_string().contains("string"),
        "recall.variants must be a list of strings: {variants}"
    );
    assert!(
        variants["description"]
            .as_str()
            .is_some_and(|d| d.contains("extra phrasings") && d.contains("keyword")),
        "recall.variants must say what to send: {variants}"
    );
    assert!(
        recall["description"]
            .as_str()
            .is_some_and(|d| d.contains("`variants`") && d.contains("empty or thin")),
        "recall tool description must say when to send variants"
    );
    assert!(
        !recall["inputSchema"]["required"]
            .to_string()
            .contains("variants"),
        "recall.variants must be optional"
    );

    if cfg!(feature = "metrics") {
        assert!(
            names.contains("metrics"),
            "reactivated build omitted metrics"
        );
    } else {
        assert!(
            !names.contains("metrics"),
            "hibernated build exposed metrics: {names:?}"
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

    // R6 (issue #72): the MCP recall bundle is a trimmed subset of the
    // CLI's diagnostic `--json` shape. `rank`/`score`/`fts_score`/
    // `vector_score`/`confidence` must never appear on the wire here;
    // `rerank_score` must — present (even as `null` when the re-ranker
    // didn't run, as in this fts-mode call) rather than omitted, so
    // callers can rely on the key existing.
    let hit_obj = top.as_object().expect("recall hit is a JSON object");
    for dropped in ["rank", "score", "fts_score", "vector_score", "confidence"] {
        assert!(
            !hit_obj.contains_key(dropped),
            "MCP recall hit still carries dropped field `{dropped}`: {top}",
        );
    }
    assert!(
        hit_obj.contains_key("rerank_score"),
        "MCP recall hit is missing `rerank_score`: {top}",
    );
}

/// Writes `messages` to a fresh `memhub serve` and returns its replies keyed
/// by id. Stdin stays open until every `expected_ids` reply has arrived, and
/// the wait is bounded, so a session that ends early fails instead of hanging.
fn replay(
    repo: &std::path::Path,
    messages: &[Value],
    expected_ids: &[i64],
) -> std::collections::HashMap<i64, Value> {
    let mut child = Command::new(memhub_bin())
        .arg("serve")
        .current_dir(repo)
        .env("MEMHUB_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn memhub serve");

    let mut stdin = child.stdin.take().expect("stdin");
    for msg in messages {
        writeln!(stdin, "{msg}").expect("write json-rpc");
    }

    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(reply) = serde_json::from_str::<Value>(&line)
                && tx.send(reply).is_err()
            {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut replies = std::collections::HashMap::new();
    while !expected_ids.iter().all(|id| replies.contains_key(id)) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(reply) => {
                if let Some(id) = reply["id"].as_i64() {
                    replies.insert(id, reply);
                }
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "memhub serve gave no reply for all of {expected_ids:?} ({err}); got {replies:?}"
                );
            }
        }
    }

    drop(stdin);
    let _ = child.wait_timeout(Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();
    replies
}

// The exact lines Antigravity CLI (agy 1.4.7) was observed sending (issue #419).
fn antigravity_discover() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/clientCapabilities":{"elicitation":{"form":{},"url":{}},"roots":{"listChanged":true}},"io.modelcontextprotocol/clientInfo":{"name":"antigravity-client","version":"v1.0.0"},"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}})
}

fn antigravity_initialize() -> Value {
    json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"clientInfo":{"name":"antigravity-client","version":"v1.0.0"},"protocolVersion":"2025-11-25","capabilities":{"elicitation":{"form":{},"url":{}},"roots":{"listChanged":true}}}})
}

fn antigravity_tail() -> [Value; 2] {
    [
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}),
    ]
}

fn roots_list_changed() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/roots/list_changed","params":{}})
}

/// Asserts the discover probe got method-not-found and the rest of the
/// session matches a client that opened directly with `initialize`.
fn assert_matches_direct_initialize(
    repo: &std::path::Path,
    replies: &std::collections::HashMap<i64, Value>,
) {
    assert_eq!(
        replies[&1]["error"]["code"],
        json!(-32601),
        "server/discover must get method-not-found: {}",
        replies[&1]
    );

    let [initialized, list_tools] = antigravity_tail();
    let direct = replay(
        repo,
        &[antigravity_initialize(), initialized, list_tools],
        &[2, 3],
    );
    assert_eq!(replies[&2], direct[&2], "initialize result differs");
    let tools = replies[&3]["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(
        tools.iter().any(|t| t["name"] == json!("recall")),
        "tool list is missing recall: {tools:?}"
    );
    assert_eq!(replies[&3], direct[&3], "tool list differs");
}

#[test]
fn antigravity_discover_then_initialize_gets_full_tool_list() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let [initialized, list_tools] = antigravity_tail();
    let replies = replay(
        temp.path(),
        &[
            antigravity_discover(),
            antigravity_initialize(),
            initialized,
            list_tools,
        ],
        &[1, 2, 3],
    );
    assert_matches_direct_initialize(temp.path(), &replies);
}

#[test]
fn antigravity_roots_notifications_before_handshake_keep_session() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let [initialized, list_tools] = antigravity_tail();
    let replies = replay(
        temp.path(),
        &[
            antigravity_discover(),
            roots_list_changed(),
            antigravity_initialize(),
            roots_list_changed(),
            initialized,
            list_tools,
        ],
        &[1, 2, 3],
    );
    assert_matches_direct_initialize(temp.path(), &replies);
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
