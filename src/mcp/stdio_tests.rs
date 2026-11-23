use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmcp::handler::server::tool::ToolRoute;
use rmcp::model::CallToolResult;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};

use super::*;

async fn exchange(
    write: &mut (impl AsyncWrite + Unpin),
    replies: &mut Lines<BufReader<impl AsyncRead + Unpin>>,
    request: &str,
) -> Value {
    write.write_all(request.as_bytes()).await.unwrap();
    write.write_all(b"\n").await.unwrap();
    serde_json::from_str(&replies.next_line().await.unwrap().unwrap()).unwrap()
}

#[tokio::test]
async fn legacy_task_control_refuses_before_tool_dispatch_in_both_profiles() {
    for profile in [ServerProfile::Full, ServerProfile::LookupOnly] {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let mut server = MemhubServer::try_new_with_profile(temp.path(), None, None, profile)
            .expect("test project binding");
        let calls = Arc::new(AtomicUsize::new(0));
        // Keep the production catalog and replace the two exercised routes
        // (where available) with an echo and a counter. No real tool runs.
        for name in ["recall", "search"] {
            if let Some(attr) = server.tool_router.get(name).cloned() {
                let calls = calls.clone();
                server
                    .tool_router
                    .add_route(ToolRoute::new_dyn(attr, move |context| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        let arguments = context.arguments;
                        Box::pin(
                            async move { Ok(CallToolResult::structured(json!(arguments)).into()) },
                        )
                    }));
            }
        }

        tokio::time::timeout(Duration::from_secs(10), async {
            let (server_pipe, client_pipe) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(server_pipe);
            let transport = PreInitializeTolerance {
                inner: AsyncRwTransport::new(read, write),
                initialize_seen: false,
                legacy_initialize_echo: None,
            };
            let server = tokio::spawn(async move {
                server.serve(transport).await.unwrap().waiting().await.unwrap();
            });
            let (read, mut write) = tokio::io::split(client_pipe);
            let mut replies = BufReader::new(read).lines();
            let initialized = exchange(
                &mut write,
                &mut replies,
                r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"codex","version":"test"}}}"#,
            ).await;
            assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");

            // Exact rmcp 1.8 results: object control rejects known tools,
            // missing/excluded routes used the unsupported enqueue default,
            // and malformed controls fell through to unknown-method lookup.
            for (name, fields, code, message) in [
                ("recall", r#""task":{}"#, -32602, "Tool does not support task-based invocation"),
                ("recall", r#""task":true"#, -32601, "tools/call"),
                ("recall", r#""task":[]"#, -32601, "tools/call"),
                ("recall", r#""task":null,"task":{}"#, -32601, "tools/call"),
                ("missing_test_tool", r#""task":{}"#, -32603, "Task processing not implemented"),
                if profile == ServerProfile::Full {
                    ("search", r#""task":{}"#, -32602, "Tool does not support task-based invocation")
                } else {
                    ("search", r#""task":{}"#, -32603, "Task processing not implemented")
                },
            ] {
                let request = format!(r#"{{"jsonrpc":"2.0","id":"refuse","method":"tools/call","params":{{"name":"{name}",{fields}}}}}"#);
                let reply = exchange(&mut write, &mut replies, &request).await;
                assert_eq!(reply["id"], "refuse");
                assert_eq!(reply["error"], json!({"code":code,"message":message}));
                assert_eq!(calls.load(Ordering::Relaxed), 0, "{profile:?}: {request}");
            }

            for (id, fields) in [(1, ""), (2, r#""task":null,"#)] {
                let request = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{{fields}"name":"recall","arguments":{{"task":{{"query":"harmless echo"}}}}}}}}"#);
                let reply = exchange(&mut write, &mut replies, &request).await;
                assert_eq!(reply["id"], id);
                assert_eq!(reply["result"]["structuredContent"], json!({"task":{"query":"harmless echo"}}));
            }
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            write.shutdown().await.unwrap();
            server.await.unwrap();
        }).await.expect("stdio compatibility test timed out");
    }
}
