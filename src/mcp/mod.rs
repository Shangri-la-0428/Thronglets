//! MCP (Model Context Protocol) server for AI agent integration.
//!
//! Exposes Thronglets capabilities as MCP tools over stdio (JSON-RPC 2.0).
//! AI agents connect to this to read/write traces on the substrate.
//!
//! Tools:
//! - trace_emit: Leave a trace on the substrate
//! - collective_query: Query aggregate stats for a subject
//! - discover: Find capabilities matching tags

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::identity::NodeIdentity;
use crate::network::NetworkCommand;
use crate::storage::TraceStore;
use crate::trace::{Outcome, Trace};

/// JSON-RPC 2.0 request
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    fn error(id: Value, code: i32, message: String) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(JsonRpcError { code, message }) }
    }
}

/// MCP tool definitions for the initialize handshake.
fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "trace_emit",
                "description": "Leave a trace on the Thronglets substrate. Records that you used a capability/tool and the outcome.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "about": {
                            "type": "string",
                            "description": "What this trace is about (tool name, capability ID, resource URI)"
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Tags for routing and filtering (e.g. [\"nlp\", \"translation\"])"
                        },
                        "outcome": {
                            "type": "string",
                            "enum": ["succeeded", "failed", "partial", "timeout"],
                            "description": "Outcome of the interaction"
                        },
                        "quality": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 100,
                            "description": "Quality score 0-100"
                        },
                        "latency_ms": {
                            "type": "integer",
                            "description": "Execution latency in milliseconds"
                        }
                    },
                    "required": ["about", "outcome"]
                }
            },
            {
                "name": "collective_query",
                "description": "Query the collective intelligence of the Thronglets network. Returns aggregate stats for a subject based on traces from all connected nodes.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "about": {
                            "type": "string",
                            "description": "Subject to query (tool name, capability ID)"
                        }
                    },
                    "required": ["about"]
                }
            },
            {
                "name": "discover",
                "description": "Discover capabilities on the Thronglets network matching the given tags.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Tags to search for"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return (default: 20)"
                        }
                    },
                    "required": ["tags"]
                }
            }
        ]
    })
}

/// Server info for the initialize response.
fn server_info() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "thronglets",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

/// Shared context for MCP request handling.
pub struct McpContext {
    pub identity: Arc<NodeIdentity>,
    pub store: Arc<TraceStore>,
    pub network_tx: Option<mpsc::Sender<NetworkCommand>>,
}

/// Run the MCP server over stdio.
/// Reads JSON-RPC requests from stdin, writes responses to stdout.
pub async fn serve_stdio(ctx: Arc<McpContext>) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    debug!("MCP server started on stdio");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => handle_request(&ctx, req).await,
            Err(e) => {
                warn!(%e, "Failed to parse JSON-RPC request");
                JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"))
            }
        };

        let mut out = serde_json::to_string(&response).unwrap();
        out.push('\n');
        if stdout.write_all(out.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
}

async fn handle_request(ctx: &McpContext, req: JsonRpcRequest) -> JsonRpcResponse {
    let id = req.id.unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => {
            JsonRpcResponse::success(id, server_info())
        }
        "notifications/initialized" | "initialized" => {
            // Client acknowledges initialization — no response needed for notifications,
            // but we respond anyway since some clients expect it
            JsonRpcResponse::success(id, json!({}))
        }
        "tools/list" => {
            JsonRpcResponse::success(id, tool_definitions())
        }
        "tools/call" => {
            handle_tool_call(ctx, id, req.params).await
        }
        _ => {
            JsonRpcResponse::error(id, -32601, format!("Method not found: {}", req.method))
        }
    }
}

async fn handle_tool_call(ctx: &McpContext, id: Value, params: Value) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "trace_emit" => handle_trace_emit(ctx, id, arguments).await,
        "collective_query" => handle_collective_query(ctx, id, arguments),
        "discover" => handle_discover(ctx, id, arguments),
        _ => JsonRpcResponse::error(id, -32602, format!("Unknown tool: {tool_name}")),
    }
}

async fn handle_trace_emit(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let about = match args.get("about").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: about".into()),
    };

    let tags: Vec<String> = args.get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let outcome = match args.get("outcome").and_then(|v| v.as_str()).unwrap_or("succeeded") {
        "succeeded" | "success" => Outcome::Succeeded,
        "failed" | "fail" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "timeout" => Outcome::Timeout,
        _ => Outcome::Succeeded,
    };

    let quality = args.get("quality").and_then(|v| v.as_u64()).unwrap_or(50) as u8;
    let latency_ms = args.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let trace = Trace::new(
        about.clone(),
        tags,
        outcome,
        latency_ms,
        quality,
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );

    let trace_id_hex: String = trace.id[..8].iter().map(|b| format!("{b:02x}")).collect();

    // Store locally
    match ctx.store.insert(&trace) {
        Ok(_) => {}
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Storage error: {e}")),
    }

    // Publish to network if connected
    if let Some(tx) = &ctx.network_tx {
        let _ = tx.send(NetworkCommand::PublishTrace(trace)).await;
    }

    JsonRpcResponse::success(id, json!({
        "content": [{
            "type": "text",
            "text": format!("Trace emitted: {about} ({outcome:?}, quality={quality}) id={trace_id_hex}")
        }]
    }))
}

fn handle_collective_query(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let about = match args.get("about").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: about".into()),
    };

    match ctx.store.aggregate(about) {
        Ok(Some(stats)) => {
            JsonRpcResponse::success(id, json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Collective intelligence for '{about}':\n\
                         - Total traces: {}\n\
                         - Success rate: {:.1}%\n\
                         - Avg latency: {:.0}ms\n\
                         - Avg quality: {:.1}\n\
                         - Confidence: {}",
                        stats.total_traces,
                        stats.success_rate * 100.0,
                        stats.avg_latency_ms,
                        stats.avg_quality,
                        if stats.total_traces > 100 { "high" }
                        else if stats.total_traces > 10 { "medium" }
                        else { "low" }
                    )
                }]
            }))
        }
        Ok(None) => {
            JsonRpcResponse::success(id, json!({
                "content": [{
                    "type": "text",
                    "text": format!("No traces found for '{about}'. No collective intelligence available yet.")
                }]
            }))
        }
        Err(e) => JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    }
}

fn handle_discover(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let tags: Vec<String> = args.get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if tags.is_empty() {
        return JsonRpcResponse::error(id, -32602, "At least one tag is required".into());
    }

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();

    match ctx.store.query_tags(&tag_refs, limit) {
        Ok(traces) => {
            // Group by 'about' and compute per-subject stats
            let mut subjects: std::collections::HashMap<&str, (u32, u32, u64, u64)> = std::collections::HashMap::new();
            for t in &traces {
                let entry = subjects.entry(&t.about).or_insert((0, 0, 0, 0));
                entry.0 += 1; // total
                if matches!(t.outcome, Outcome::Succeeded) { entry.1 += 1; } // successes
                entry.2 += t.latency_ms as u64;
                entry.3 += t.quality as u64;
            }

            let mut results: Vec<Value> = subjects.iter().map(|(about, (total, succ, lat, qual))| {
                json!({
                    "about": about,
                    "total_traces": total,
                    "success_rate": format!("{:.1}%", *succ as f64 / *total as f64 * 100.0),
                    "avg_latency_ms": lat / *total as u64,
                    "avg_quality": qual / *total as u64,
                })
            }).collect();
            results.sort_by(|a, b| {
                b["total_traces"].as_u64().cmp(&a["total_traces"].as_u64())
            });

            let text = if results.is_empty() {
                format!("No capabilities found matching tags: {tags:?}")
            } else {
                let lines: Vec<String> = results.iter().map(|r| {
                    format!("- {} (traces={}, success={}, quality={})",
                        r["about"].as_str().unwrap_or("?"),
                        r["total_traces"],
                        r["success_rate"].as_str().unwrap_or("?"),
                        r["avg_quality"])
                }).collect();
                format!("Discovered capabilities for tags {tags:?}:\n{}", lines.join("\n"))
            };

            JsonRpcResponse::success(id, json!({
                "content": [{ "type": "text", "text": text }]
            }))
        }
        Err(e) => JsonRpcResponse::error(id, -32000, format!("Discover error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> Arc<McpContext> {
        let identity = Arc::new(NodeIdentity::generate());
        let store = Arc::new(TraceStore::in_memory().unwrap());
        Arc::new(McpContext { identity, store, network_tx: None })
    }

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let ctx = make_ctx();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, req).await;
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "thronglets");
    }

    #[tokio::test]
    async fn tools_list_returns_three_tools() {
        let ctx = make_ctx();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "tools/list".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, req).await;
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().len();
        assert_eq!(tools, 3);
    }

    #[tokio::test]
    async fn trace_emit_and_query_roundtrip() {
        let ctx = make_ctx();

        // Emit a trace
        let emit_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "tools/call".into(),
            params: json!({
                "name": "trace_emit",
                "arguments": {
                    "about": "test-tool/v1",
                    "tags": ["test", "mcp"],
                    "outcome": "succeeded",
                    "quality": 90,
                    "latency_ms": 200
                }
            }),
        };
        let resp = handle_request(&ctx, emit_req).await;
        assert!(resp.error.is_none(), "emit should succeed");

        // Query it back
        let query_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "tools/call".into(),
            params: json!({
                "name": "collective_query",
                "arguments": { "about": "test-tool/v1" }
            }),
        };
        let resp = handle_request(&ctx, query_req).await;
        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("100.0%"), "should show 100% success rate");
        assert!(text.contains("Total traces: 1"), "should show 1 trace");
    }

    #[tokio::test]
    async fn discover_by_tags() {
        let ctx = make_ctx();

        // Emit traces with different tags
        for (about, tags) in [("tool-a", vec!["rust", "code"]), ("tool-b", vec!["python"])] {
            let trace = Trace::new(
                about.into(), tags.into_iter().map(String::from).collect(),
                Outcome::Succeeded, 100, 80,
                ctx.identity.public_key_bytes(),
                |m| ctx.identity.sign(m),
            );
            ctx.store.insert(&trace).unwrap();
        }

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(5)),
            method: "tools/call".into(),
            params: json!({
                "name": "discover",
                "arguments": { "tags": ["rust"] }
            }),
        };
        let resp = handle_request(&ctx, req).await;
        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("tool-a"), "should find tool-a");
        assert!(!text.contains("tool-b"), "should not find tool-b");
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let ctx = make_ctx();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(99)),
            method: "nonexistent".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
