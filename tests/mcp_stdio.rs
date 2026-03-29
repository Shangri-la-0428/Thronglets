//! Integration test: verify the MCP server works end-to-end over stdio.
//! Spawns `thronglets mcp` as a subprocess, sends JSON-RPC messages,
//! and verifies responses match the MCP protocol.

use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Send a JSON-RPC request and read the response line.
async fn rpc_call(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    request: Value,
) -> Value {
    let mut line = serde_json::to_string(&request).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();

    let resp_line = tokio::time::timeout(std::time::Duration::from_secs(5), stdout.next_line())
        .await
        .expect("timeout waiting for MCP response")
        .expect("IO error reading response")
        .expect("unexpected EOF");

    serde_json::from_str(&resp_line).expect("response should be valid JSON")
}

/// Send a notification (no response expected).
async fn rpc_notify(stdin: &mut tokio::process::ChildStdin, request: Value) {
    let mut line = serde_json::to_string(&request).unwrap();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

#[tokio::test]
async fn mcp_full_protocol_handshake() {
    // Build the binary first
    let status = std::process::Command::new("cargo")
        .args(["build"])
        .status()
        .expect("cargo build failed");
    assert!(status.success(), "cargo build must succeed");

    // Use a temp dir so we don't pollute the real data dir
    let tmp_dir = tempfile::tempdir().unwrap();

    let mut child = Command::new("cargo")
        .args([
            "run",
            "--",
            "--data-dir",
            tmp_dir.path().to_str().unwrap(),
            "mcp",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn thronglets mcp");

    let mut stdin = child.stdin.take().unwrap();
    let stdout_reader = BufReader::new(child.stdout.take().unwrap());
    let mut lines = stdout_reader.lines();

    // 1. Initialize
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }),
    )
    .await;

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["serverInfo"]["name"], "thronglets");
    assert!(resp["result"]["protocolVersion"].as_str().is_some());

    // 2. Send initialized notification (no response expected)
    rpc_notify(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;

    // 3. List tools — should expose the full machine-facing MCP surface.
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )
    .await;

    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7);
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"trace_record"));
    assert!(tool_names.contains(&"substrate_query"));
    assert!(tool_names.contains(&"trace_anchor"));
    assert!(tool_names.contains(&"signal_post"));
    assert!(tool_names.contains(&"signal_feed"));
    assert!(tool_names.contains(&"presence_ping"));
    assert!(tool_names.contains(&"presence_feed"));

    // 4. Record a trace
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "trace_record",
                "arguments": {
                    "capability": "mcp-test/integration",
                    "outcome": "succeeded",
                    "latency_ms": 42,
                    "input_size": 1000,
                    "context": "integration testing MCP protocol",
                    "model": "test-model"
                }
            }
        }),
    )
    .await;

    assert!(
        resp["error"].is_null(),
        "trace_record should succeed: {resp}"
    );
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).expect("response should be structured JSON");
    assert_eq!(parsed["recorded"], true);
    assert_eq!(parsed["capability"], "mcp-test/integration");
    assert!(!parsed["trace_id"].as_str().unwrap().is_empty());

    // 5. Evaluate the capability
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "substrate_query",
                "arguments": {
                    "context": "evaluating test capabilities",
                    "intent": "evaluate",
                    "capability": "mcp-test/integration"
                }
            }
        }),
    )
    .await;

    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).expect("evaluate response should be JSON");
    assert_eq!(parsed["capability"], "mcp-test/integration");
    assert_eq!(parsed["stats"]["total_traces"], 1);
    assert_eq!(parsed["stats"]["success_rate"], 1.0);

    // 6. Explore available capabilities
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "substrate_query",
                "arguments": {
                    "context": "discovering what tools are available",
                    "intent": "explore"
                }
            }
        }),
    )
    .await;

    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).expect("explore response should be JSON");
    let caps = parsed["capabilities"].as_array().unwrap();
    assert!(!caps.is_empty(), "should find at least one capability");
    assert_eq!(caps[0]["capability"], "mcp-test/integration");

    // 7. Unknown method returns error
    let resp = rpc_call(
        &mut stdin,
        &mut lines,
        json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "nonexistent"
        }),
    )
    .await;

    assert_eq!(resp["error"]["code"], -32601);

    // Cleanup
    drop(stdin);
    let _ = child.kill().await;
}
