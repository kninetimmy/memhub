//! End-to-end MCP protocol smoke tests.
//!
//! Spawns `memhub serve` as a subprocess and runs the JSON-RPC handshake
//! over stdio. Exists because the `#[tool_handler]` attribute on the
//! `ServerHandler` impl is load-bearing — if it's ever removed, the
//! server still initializes and identifies itself, but `tools/list`
//! silently returns an empty array. The in-process unit tests don't
//! catch that; only a real handshake does.

use std::io::{BufRead, BufReader, Read, Write};
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
        .env("MEMHUB_LOG", "warn")
        .env("HOME", repo.join(".test-home"))
        .env("USERPROFILE", repo.join(".test-home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        !stderr.contains("unmapped client name"),
        "known MCP client emitted an alias warning: {stderr}"
    );
    replies
}

#[test]
fn sync_forks_surface_baseline_diagnostics_through_cli_and_mcp() {
    use memhub::{commands::sync, db};

    fn repo() -> tempfile::TempDir {
        let temp = tempdir().unwrap();
        init::run(temp.path()).unwrap();
        let ctx = db::open_project(temp.path()).unwrap();
        let mut cfg = ctx.config.clone();
        cfg.sync.enabled = true;
        cfg.sync.project_id = "sync-baseline-test".into();
        cfg.save(&ctx.paths.config_path).unwrap();
        temp
    }

    fn cli(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new(memhub_bin())
            .args(args)
            .current_dir(repo)
            .env("HOME", repo.join(".test-home"))
            .env("USERPROFILE", repo.join(".test-home"))
            .output()
            .unwrap()
    }

    let local = repo();
    for n in 0..4 {
        fact::add(
            local.path(),
            &format!("local-{n}"),
            "keep",
            "user",
            "cli:user",
        )
        .unwrap();
    }
    let baseline = local.path().join("baseline");
    sync::snapshot(local.path(), &baseline, false).unwrap();
    sync::commit(local.path(), &baseline).unwrap();
    let marker_path = local.path().join(".memhub/sync_marker.json");
    let marker_before = std::fs::read(&marker_path).unwrap();
    let before = sync::LogicalVersion::read(&db::open_project(local.path()).unwrap().conn).unwrap();

    // Independent histories with fewer, equal, and more audit writes.
    for facts in [1, 4, 5] {
        let fork = repo();
        for n in 0..facts {
            fact::add(
                fork.path(),
                &format!("fork-{n}"),
                "unique",
                "user",
                "cli:user",
            )
            .unwrap();
        }
        let remote = fork.path().join("remote");
        let summary = sync::snapshot(fork.path(), &remote, false).unwrap();
        let manifest_before = std::fs::read(&summary.manifest_path).unwrap();
        let snapshot_before = std::fs::read(&summary.snapshot_path).unwrap();
        let remote_arg = remote.to_str().unwrap();
        let expected = if facts <= 4 {
            "diverged"
        } else {
            "drive_ahead"
        };

        let output = cli(local.path(), &["sync", "check", remote_arg, "--json"]);
        assert!(output.status.success(), "{:?}", output);
        let check: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(check["verdict"], expected);
        assert!(check["baseline_warning"].is_string());

        let output = cli(local.path(), &["sync", "check", remote_arg]);
        assert!(output.status.success());
        let human = String::from_utf8(output.stdout).unwrap();
        assert!(
            human.contains(check["baseline_warning"].as_str().unwrap()),
            "{human}"
        );

        let output = cli(
            local.path(),
            &["sync", "check", remote_arg, "--diff", "--json"],
        );
        assert!(output.status.success(), "{:?}", output);
        let detail: Value = serde_json::from_slice(&output.stdout).unwrap();
        if facts <= 4 {
            assert!(detail["diff"]["local"].is_null(), "{detail}");
            assert!(detail["diff"]["remote"].is_null(), "{detail}");
            assert!(detail["diff"]["unavailable_reason"].is_string(), "{detail}");
        } else {
            // This focused guard cannot prove that the higher-counter
            // fork shares the baseline: only its last write is shown.
            assert!(detail["diff"]["unavailable_reason"].is_null());
            assert_eq!(detail["diff"]["remote"]["writes_since_baseline"], 1);
            assert!(
                check["baseline_warning"]
                    .as_str()
                    .unwrap()
                    .contains("higher audit counters")
            );
        }

        let [initialized, _] = antigravity_tail();
        let replies = replay(
            local.path(),
            &[
                antigravity_initialize(),
                initialized,
                json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"sync_check","arguments":{"remote":remote_arg}}}),
                json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"sync_adopt","arguments":{"remote":remote_arg}}}),
                json!({"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"sync_snapshot","arguments":{"remote":remote_arg}}}),
            ],
            &[2, 10, 11, 12],
        );
        let mcp_check = &replies[&10]["result"]["structuredContent"];
        assert_eq!(
            mcp_check["verdict"],
            if facts <= 4 {
                "diverged"
            } else {
                "drive-ahead"
            }
        );
        assert_eq!(mcp_check["baseline_warning"], check["baseline_warning"]);
        let refusal = &replies[&11]["result"]["structuredContent"];
        assert_eq!(refusal["adopted"], false);
        assert_eq!(refusal["verdict"], mcp_check["verdict"]);
        assert_eq!(refusal["baseline_warning"], check["baseline_warning"]);
        assert!(refusal["reason"].as_str().unwrap().contains("confirm=true"));
        assert!(
            replies[&12]["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("refusing to overwrite")),
            "{}",
            replies[&12]
        );

        assert_eq!(
            std::fs::read(&summary.manifest_path).unwrap(),
            manifest_before
        );
        assert_eq!(
            std::fs::read(&summary.snapshot_path).unwrap(),
            snapshot_before
        );
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker_before);
        assert_eq!(
            sync::LogicalVersion::read(&db::open_project(local.path()).unwrap().conn).unwrap(),
            before
        );
    }
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

#[test]
fn antigravity_discover_error_is_consistent_after_initialize() {
    let temp = tempdir().unwrap();
    init::run(temp.path()).unwrap();
    let [initialized, list_tools] = antigravity_tail();
    let mut after = antigravity_discover();
    after["id"] = json!(4);
    let replies = replay(
        temp.path(),
        &[
            antigravity_discover(),
            antigravity_initialize(),
            initialized,
            after,
            list_tools,
        ],
        &[1, 2, 3, 4],
    );
    assert_eq!(replies[&1]["error"]["code"], -32601);
    assert_eq!(replies[&1]["error"], replies[&4]["error"]);
}

#[test]
fn antigravity_proposals_keep_identity_and_wait_for_acceptance() {
    let temp = tempdir().unwrap();
    init::run(temp.path()).unwrap();
    let [initialized, _] = antigravity_tail();
    let fact = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"propose_fact","arguments":{"key":"test-antigravity","value":"fixture only","rationale":"Review this fixture"}}});
    let decision = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"propose_decision","arguments":{"title":"Review this Antigravity fixture","rationale":"Keep it pending"}}});
    let replies = replay(
        temp.path(),
        &[
            antigravity_discover(),
            antigravity_initialize(),
            initialized,
            fact,
            decision,
        ],
        &[1, 2, 4, 5],
    );
    for id in [4, 5] {
        let body = &replies[&id]["result"]["structuredContent"];
        assert_eq!(body["status"], "pending", "{:?}", replies[&id]);
        assert_eq!(body["actor"], "antigravity");
        assert_eq!(body["actor_raw"], "antigravity-client");
    }
    let ctx = memhub::db::open_project(temp.path()).unwrap();
    let mut statement = ctx.conn.prepare("SELECT actor, actor_raw, provenance_json FROM pending_writes WHERE status = 'pending' ORDER BY id").unwrap();
    let rows: Vec<(String, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    for (actor, raw, provenance) in rows {
        assert_eq!(actor, "antigravity");
        assert_eq!(raw, "antigravity-client");
        assert_eq!(
            serde_json::from_str::<Value>(&provenance).unwrap()["client_name"],
            "antigravity-client"
        );
    }
    for table in ["facts", "decisions"] {
        let count: i64 = ctx
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "a proposal must not become a durable {table} row before acceptance"
        );
    }
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
