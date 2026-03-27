//! MCP (Model Context Protocol) server for AI agent integration.
//!
//! Exposes Thronglets capabilities as MCP tools over stdio (JSON-RPC 2.0).
//! AI agents connect to this to read/write traces on the substrate.
//!
//! v0.2 tools:
//! - trace_record: Record a trace on the substrate (write)
//! - substrate_query: Query the substrate by intent (read)

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::anchor::AnchorClient;
use crate::context::{simhash, similarity};
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
                "name": "trace_record",
                "description": "Record a trace on the Thronglets substrate. Logs that you used a capability and the outcome.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "capability": {
                            "type": "string",
                            "description": "Capability URI — what was used (e.g. \"urn:mcp:anthropic:claude:code\")"
                        },
                        "outcome": {
                            "type": "string",
                            "enum": ["succeeded", "failed", "partial", "timeout"],
                            "description": "Outcome of the interaction"
                        },
                        "latency_ms": {
                            "type": "integer",
                            "description": "Execution latency in milliseconds (default: 0)"
                        },
                        "input_size": {
                            "type": "integer",
                            "description": "Input size in tokens/bytes/items (default: 0)"
                        },
                        "context": {
                            "type": "string",
                            "description": "Natural language task context — preserved as-is AND SimHashed for search (default: \"\")"
                        },
                        "model": {
                            "type": "string",
                            "description": "Self-reported model identifier (default: \"unknown\")"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Session identifier for workflow tracking — traces with the same session_id form an ordered sequence"
                        }
                    },
                    "required": ["capability", "outcome"]
                }
            },
            {
                "name": "substrate_query",
                "description": "Query the Thronglets substrate. Use intent 'resolve' to find capabilities for a task, 'evaluate' to get stats for a specific capability, or 'explore' to discover what's available.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "context": {
                            "type": "string",
                            "description": "What the agent is trying to do"
                        },
                        "intent": {
                            "type": "string",
                            "enum": ["resolve", "evaluate", "explore"],
                            "description": "Query intent: resolve (find capabilities), evaluate (get stats), explore (discover)"
                        },
                        "capability": {
                            "type": "string",
                            "description": "Specific capability URI (required for 'evaluate' intent)"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return (default: 10)"
                        }
                    },
                    "required": ["context", "intent"]
                }
            },
            {
                "name": "trace_anchor",
                "description": "Anchor a trace to the Oasyce blockchain for on-chain verification proof.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "trace_id": {
                            "type": "string",
                            "description": "Hex trace ID to anchor"
                        },
                        "rpc": {
                            "type": "string",
                            "description": "Oasyce RPC endpoint (default: http://localhost:1317)"
                        }
                    },
                    "required": ["trace_id"]
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
                Some(JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}")))
            }
        };

        if let Some(resp) = response {
            let mut out = serde_json::to_string(&resp).unwrap();
            out.push('\n');
            if stdout.write_all(out.as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    }
}

async fn handle_request(ctx: &McpContext, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    // JSON-RPC 2.0: notifications have no id — server must not respond
    let is_notification = req.id.is_none();
    let id = req.id.unwrap_or(Value::Null);

    match req.method.as_str() {
        // MCP notifications — never respond
        "notifications/initialized" | "notifications/cancelled" => {
            debug!(method = %req.method, "Received MCP notification");
            None
        }
        "initialize" => {
            Some(JsonRpcResponse::success(id, server_info()))
        }
        "tools/list" => {
            Some(JsonRpcResponse::success(id, tool_definitions()))
        }
        "tools/call" => {
            Some(handle_tool_call(ctx, id, req.params).await)
        }
        _ if is_notification => {
            debug!(method = %req.method, "Ignoring unknown notification");
            None
        }
        _ => {
            Some(JsonRpcResponse::error(id, -32601, format!("Method not found: {}", req.method)))
        }
    }
}

async fn handle_tool_call(ctx: &McpContext, id: Value, params: Value) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "trace_record" => handle_trace_record(ctx, id, arguments).await,
        "substrate_query" => handle_substrate_query(ctx, id, arguments),
        "trace_anchor" => handle_trace_anchor(ctx, id, arguments),
        _ => JsonRpcResponse::error(id, -32602, format!("Unknown tool: {tool_name}")),
    }
}

// ---------------------------------------------------------------------------
// Tool 1: trace_record
// ---------------------------------------------------------------------------

async fn handle_trace_record(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let capability = match args.get("capability").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: capability".into()),
    };

    let outcome = match args.get("outcome").and_then(|v| v.as_str()).unwrap_or("succeeded") {
        "succeeded" | "success" => Outcome::Succeeded,
        "failed" | "fail" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "timeout" => Outcome::Timeout,
        _ => Outcome::Succeeded,
    };

    let latency_ms = args.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let input_size = args.get("input_size").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let context_str = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
    let model_id = args.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
    let session_id = args.get("session_id").and_then(|v| v.as_str()).map(String::from);

    let context_hash = simhash(context_str);
    let context_text = if context_str.is_empty() { None } else { Some(context_str.to_string()) };

    let trace = Trace::new(
        capability.clone(),
        outcome,
        latency_ms,
        input_size,
        context_hash,
        context_text,
        session_id,
        model_id,
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

    let response_json = json!({
        "recorded": true,
        "trace_id": trace_id_hex,
        "capability": capability,
    });

    JsonRpcResponse::success(id, json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&response_json).unwrap()
        }]
    }))
}

// ---------------------------------------------------------------------------
// Tool 2: substrate_query
// ---------------------------------------------------------------------------

fn handle_substrate_query(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let context_str = match args.get("context").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: context".into()),
    };

    let intent = match args.get("intent").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: intent".into()),
    };

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

    match intent {
        "resolve" => handle_resolve(ctx, id, context_str, limit),
        "evaluate" => {
            let capability = match args.get("capability").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return JsonRpcResponse::error(
                    id, -32602, "Missing required field: capability (required for 'evaluate' intent)".into(),
                ),
            };
            handle_evaluate(ctx, id, capability, limit)
        }
        "explore" => handle_explore(ctx, id, context_str, limit),
        _ => JsonRpcResponse::error(id, -32602, format!("Unknown intent: {intent}. Use 'resolve', 'evaluate', or 'explore'.")),
    }
}

/// Resolve: find capabilities matching a task context via SimHash similarity.
fn handle_resolve(ctx: &McpContext, id: Value, context_str: &str, limit: usize) -> JsonRpcResponse {
    let context_hash = simhash(context_str);

    // Query traces with similar context hashes (max hamming distance 48)
    let traces = match ctx.store.query_similar(&context_hash, 48, limit * 10) {
        Ok(t) => t,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    // Group by capability, compute per-capability stats
    let mut cap_groups: HashMap<&str, Vec<&Trace>> = HashMap::new();
    for t in &traces {
        cap_groups.entry(&t.capability).or_default().push(t);
    }

    let mut capabilities: Vec<Value> = cap_groups.iter().map(|(cap, group)| {
        let total = group.len() as u64;
        let successes = group.iter().filter(|t| matches!(t.outcome, Outcome::Succeeded)).count() as f64;
        let success_rate = if total > 0 { successes / total as f64 } else { 0.0 };

        let mut latencies: Vec<u32> = group.iter().map(|t| t.latency_ms).collect();
        latencies.sort();
        let p50 = percentile(&latencies, 50);

        // Best context similarity for this capability
        let best_trace = group.iter()
            .max_by(|a, b| {
                similarity(&context_hash, &a.context_hash)
                    .partial_cmp(&similarity(&context_hash, &b.context_hash))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let best_similarity = best_trace
            .map(|t| similarity(&context_hash, &t.context_hash))
            .unwrap_or(0.0);

        // Include recent context_text samples so agents can understand WHY
        let context_samples: Vec<&str> = group.iter()
            .filter_map(|t| t.context_text.as_deref())
            .take(3)
            .collect();

        json!({
            "capability": cap,
            "context_similarity": round2(best_similarity),
            "success_rate": round2(success_rate),
            "p50_latency_ms": p50,
            "total_traces": total,
            "context_samples": context_samples,
        })
    }).collect();

    // Sort by context_similarity descending
    capabilities.sort_by(|a, b| {
        b["context_similarity"].as_f64().unwrap_or(0.0)
            .partial_cmp(&a["context_similarity"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    capabilities.truncate(limit);

    let response_json = json!({
        "capabilities": capabilities,
    });

    JsonRpcResponse::success(id, json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&response_json).unwrap()
        }]
    }))
}

/// Evaluate: get aggregate stats + per-model breakdown for a specific capability.
fn handle_evaluate(ctx: &McpContext, id: Value, capability: &str, limit: usize) -> JsonRpcResponse {
    let stats = match ctx.store.aggregate(capability) {
        Ok(Some(s)) => s,
        Ok(None) => {
            let response_json = json!({
                "capability": capability,
                "stats": null,
                "by_model": {},
            });
            return JsonRpcResponse::success(id, json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&response_json).unwrap()
                }]
            }));
        }
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    // Get individual traces for per-model breakdown
    let traces = match ctx.store.query_capability(capability, limit.max(1000)) {
        Ok(t) => t,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    // Group by model_id
    let mut by_model: HashMap<&str, (u64, u64)> = HashMap::new(); // (total, successes)
    for t in &traces {
        let entry = by_model.entry(&t.model_id).or_insert((0, 0));
        entry.0 += 1;
        if matches!(t.outcome, Outcome::Succeeded) {
            entry.1 += 1;
        }
    }

    let model_stats: HashMap<&str, Value> = by_model.iter().map(|(model, (total, successes))| {
        let rate = if *total > 0 { *successes as f64 / *total as f64 } else { 0.0 };
        (*model, json!({
            "success_rate": round2(rate),
            "count": total,
        }))
    }).collect();

    let response_json = json!({
        "capability": capability,
        "stats": {
            "total_traces": stats.total_traces,
            "success_rate": round2(stats.success_rate),
            "p50_latency_ms": stats.p50_latency_ms,
            "p95_latency_ms": stats.p95_latency_ms,
            "avg_input_size": stats.avg_input_size,
            "confidence": round2(stats.confidence),
        },
        "by_model": model_stats,
    });

    JsonRpcResponse::success(id, json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&response_json).unwrap()
        }]
    }))
}

/// Explore: discover available capabilities with aggregate stats.
fn handle_explore(ctx: &McpContext, id: Value, context_str: &str, limit: usize) -> JsonRpcResponse {
    let caps = match ctx.store.distinct_capabilities(limit) {
        Ok(c) => c,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    let context_hash = simhash(context_str);

    let mut capabilities: Vec<Value> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();

    for cap in &caps {
        match ctx.store.aggregate(cap) {
            Ok(Some(stats)) => {
                // Check if this cap has traces with similar context
                let traces = ctx.store.query_capability(cap, 10).unwrap_or_default();
                let best_sim = traces.iter()
                    .map(|t| similarity(&context_hash, &t.context_hash))
                    .fold(0.0_f64, f64::max);

                capabilities.push(json!({
                    "capability": cap,
                    "total_traces": stats.total_traces,
                    "success_rate": round2(stats.success_rate),
                    "p50_latency_ms": stats.p50_latency_ms,
                    "context_similarity": round2(best_sim),
                }));

                if stats.success_rate < 0.5 {
                    gaps.push(format!("no high-success capability for {}", cap));
                }
            }
            Ok(None) => {}
            Err(_) => {}
        }
    }

    // Sort by total_traces descending
    capabilities.sort_by(|a, b| {
        b["total_traces"].as_u64().unwrap_or(0)
            .cmp(&a["total_traces"].as_u64().unwrap_or(0))
    });

    if capabilities.is_empty() && !context_str.is_empty() {
        gaps.push(format!("no capabilities found matching context: {}", context_str));
    }

    let response_json = json!({
        "capabilities": capabilities,
        "gaps": gaps,
    });

    JsonRpcResponse::success(id, json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&response_json).unwrap()
        }]
    }))
}

// ---------------------------------------------------------------------------
// Tool 3: trace_anchor
// ---------------------------------------------------------------------------

fn handle_trace_anchor(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let trace_id_hex = match args.get("trace_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: trace_id".into()),
    };

    let rpc = args.get("rpc")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:1317");

    // Parse hex trace ID
    let trace_id_bytes: Vec<u8> = match (0..trace_id_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&trace_id_hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(b) if b.len() == 32 => b,
        Ok(b) => return JsonRpcResponse::error(
            id, -32602,
            format!("trace_id must be 32 bytes (64 hex chars), got {} bytes", b.len()),
        ),
        Err(e) => return JsonRpcResponse::error(
            id, -32602,
            format!("Invalid hex trace_id: {e}"),
        ),
    };

    let trace_id: [u8; 32] = trace_id_bytes.try_into().unwrap();

    // Check if already anchored
    match ctx.store.is_anchored(&trace_id) {
        Ok(true) => {
            let response_json = json!({
                "anchored": true,
                "already_anchored": true,
                "trace_id": trace_id_hex,
            });
            return JsonRpcResponse::success(id, json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&response_json).unwrap()
                }]
            }));
        }
        Ok(false) => {}
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Storage error: {e}")),
    }

    // Find the trace in storage
    // We need to look it up; query all recent traces and find by ID
    let traces = match ctx.store.unanchored_traces(168, 10000) { // last 7 days
        Ok(t) => t,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    let trace = match traces.into_iter().find(|t| t.id == trace_id) {
        Some(t) => t,
        None => return JsonRpcResponse::error(
            id, -32602,
            format!("Trace {} not found or already anchored", trace_id_hex),
        ),
    };

    let client = AnchorClient::new(rpc, "oasyce-1");
    match client.anchor_trace(&ctx.identity, &trace) {
        Ok(result) => {
            if !result.tx_hash.is_empty() {
                let _ = ctx.store.mark_anchored(&trace_id, 0, &result.tx_hash);
            }

            let response_json = json!({
                "anchored": result.anchored > 0,
                "already_anchored": false,
                "trace_id": trace_id_hex,
                "tx_hash": result.tx_hash,
            });

            JsonRpcResponse::success(id, json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&response_json).unwrap()
                }]
            }))
        }
        Err(e) => JsonRpcResponse::error(id, -32000, format!("Anchor error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute percentile from a sorted slice of u32 values.
fn percentile(sorted: &[u32], pct: u8) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((pct as f64 / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Round f64 to 2 decimal places.
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;

    fn make_ctx() -> Arc<McpContext> {
        let identity = Arc::new(NodeIdentity::generate());
        let store = Arc::new(TraceStore::in_memory().unwrap());
        Arc::new(McpContext { identity, store, network_tx: None })
    }

    /// Helper: insert a trace directly into the store.
    fn insert_trace(ctx: &McpContext, cap: &str, outcome: Outcome, model: &str, context: &str, latency: u32) {
        let trace = Trace::new(
            cap.into(),
            outcome,
            latency,
            5000,
            simhash(context),
            Some(context.to_string()),
            None,
            model.into(),
            ctx.identity.public_key_bytes(),
            |msg| ctx.identity.sign(msg),
        );
        ctx.store.insert(&trace).unwrap();
        // Sleep briefly so content-addressed IDs differ (timestamp is part of ID)
        std::thread::sleep(std::time::Duration::from_millis(2));
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
        let resp = handle_request(&ctx, req).await.expect("initialize should return response");
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "thronglets");
        assert!(result["protocolVersion"].as_str().is_some());
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
        let resp = handle_request(&ctx, req).await.unwrap();
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 3);

        let names: Vec<&str> = tools.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"trace_record"));
        assert!(names.contains(&"substrate_query"));
        assert!(names.contains(&"trace_anchor"));
    }

    #[tokio::test]
    async fn trace_record_and_evaluate_roundtrip() {
        let ctx = make_ctx();

        // Record a trace via MCP
        let record_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "tools/call".into(),
            params: json!({
                "name": "trace_record",
                "arguments": {
                    "capability": "urn:mcp:anthropic:claude:code",
                    "outcome": "succeeded",
                    "latency_ms": 200,
                    "input_size": 3000,
                    "context": "refactoring async rust code",
                    "model": "claude-opus-4-6"
                }
            }),
        };
        let resp = handle_request(&ctx, record_req).await.unwrap();
        assert!(resp.error.is_none(), "trace_record should succeed");

        // Verify response is structured JSON
        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let parsed: Value = serde_json::from_str(&text).expect("response text should be valid JSON");
        assert_eq!(parsed["recorded"], true);
        assert_eq!(parsed["capability"], "urn:mcp:anthropic:claude:code");
        assert!(parsed["trace_id"].as_str().unwrap().len() > 0);

        // Evaluate the capability
        let eval_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "tools/call".into(),
            params: json!({
                "name": "substrate_query",
                "arguments": {
                    "context": "evaluating code tools",
                    "intent": "evaluate",
                    "capability": "urn:mcp:anthropic:claude:code"
                }
            }),
        };
        let resp = handle_request(&ctx, eval_req).await.unwrap();
        assert!(resp.error.is_none(), "evaluate should succeed");

        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let parsed: Value = serde_json::from_str(&text).expect("evaluate response should be valid JSON");
        assert_eq!(parsed["capability"], "urn:mcp:anthropic:claude:code");
        assert_eq!(parsed["stats"]["total_traces"], 1);
        assert_eq!(parsed["stats"]["success_rate"], 1.0);

        // Should have by_model breakdown
        let by_model = &parsed["by_model"];
        assert!(by_model["claude-opus-4-6"]["count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn resolve_finds_similar_capabilities() {
        let ctx = make_ctx();

        // Insert traces with similar and different contexts
        insert_trace(&ctx, "deepl/translate", Outcome::Succeeded, "claude-opus-4-6", "translate a document from Chinese to English", 150);
        insert_trace(&ctx, "deepl/translate", Outcome::Succeeded, "gpt-4o", "translate technical docs from Chinese to English", 300);
        insert_trace(&ctx, "k8s/deploy", Outcome::Succeeded, "claude-opus-4-6", "deploy kubernetes cluster on AWS", 5000);

        // Resolve with a translation-related context
        let resolve_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(5)),
            method: "tools/call".into(),
            params: json!({
                "name": "substrate_query",
                "arguments": {
                    "context": "translate a legal document from Chinese to English",
                    "intent": "resolve",
                    "limit": 10
                }
            }),
        };
        let resp = handle_request(&ctx, resolve_req).await.unwrap();
        assert!(resp.error.is_none(), "resolve should succeed");

        let text = resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        let parsed: Value = serde_json::from_str(&text).expect("resolve response should be valid JSON");
        let caps = parsed["capabilities"].as_array().unwrap();

        // Should find capabilities — deepl/translate should have higher similarity
        assert!(!caps.is_empty(), "should find at least one capability");
        if caps.len() >= 2 {
            let first_sim = caps[0]["context_similarity"].as_f64().unwrap();
            let last_sim = caps[caps.len() - 1]["context_similarity"].as_f64().unwrap();
            assert!(first_sim >= last_sim, "results should be sorted by context_similarity descending");
        }
    }

    #[tokio::test]
    async fn notification_returns_no_response() {
        let ctx = make_ctx();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None, // notification — no id
            method: "notifications/initialized".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, req).await;
        assert!(resp.is_none(), "notifications must not produce a response");
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
        let resp = handle_request(&ctx, req).await.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }
}
