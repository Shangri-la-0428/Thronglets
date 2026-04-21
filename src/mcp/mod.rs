//! MCP adapter and observation surface for AI runtime integration.
//!
//! Exposes a small Thronglets tool surface over stdio (JSON-RPC 2.0).
//! Ambient hooks and overlays stay primary; MCP is the explicit
//! inspect/debug/override path when a runtime needs one.
//!
//! v0.2 tools:
//! - trace_record: manually write sparse residue
//! - substrate_query: explicitly inspect substrate guidance

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::ambient::{AmbientPriorRequest, ambient_prior_data};
use crate::anchor::AnchorClient;
use crate::continuity::ExternalContinuityInput;
use crate::identity::{IdentityBinding, NodeIdentity};
use crate::identity_surface::authorization_check_data;
use crate::network::NetworkCommand;
use crate::pheromone::PheromoneField;
use crate::posts::SignalPostKind;
use crate::presence::{DEFAULT_PRESENCE_TTL_MINUTES, PresenceTraceConfig, create_presence_trace};
use crate::service;
use crate::storage::TraceStore;
use crate::trace::MethodCompliance;

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
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// MCP tool definitions for the initialize handshake.
fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "trace_record",
                "description": "Manually record sparse residue on the Thronglets substrate. Use for explicit inspect/debug/override paths, not as the default interaction loop.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "capability": {
                            "type": "string",
                            "description": "Capability URI — what was used (e.g. \"urn:mcp:anthropic:claude:code\"). Required unless external_continuity is provided."
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
                        "method_compliance": {
                            "type": "string",
                            "enum": ["compliant", "noncompliant", "unknown"],
                            "description": "Optional method-quality classification under the current explicit policy view"
                        },
                        "model": {
                            "type": "string",
                            "description": "Self-reported model identifier (default: \"unknown\")"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Session identifier for workflow tracking — traces with the same session_id form an ordered sequence"
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Optional agent identifier for multi-agent disambiguation (e.g. \"ENFP-Luna\", \"INTJ-Kai\")"
                        },
                        "sigil_id": {
                            "type": "string",
                            "description": "Sigil identity (e.g. \"SIG_abc123...\") — the Loop's on-chain identity anchor"
                        },
                        "external_continuity": {
                            "type": "object",
                            "description": "Optional low-frequency external continuity residue from Psyche. Raw events stay local-first; only sparse derived signals may escape.",
                            "properties": {
                                "provider": {
                                    "type": "string",
                                    "description": "Must be `thronglets`"
                                },
                                "mode": {
                                    "type": "string",
                                    "description": "Must be `optional`"
                                },
                                "version": {
                                    "type": "integer",
                                    "description": "Must be 1"
                                },
                                "taxonomy": {
                                    "type": "string",
                                    "enum": ["coordination", "continuity", "calibration"]
                                },
                                "event": {
                                    "type": "string",
                                    "enum": ["relation-milestone", "writeback-calibration", "continuity-anchor", "open-loop-anchor"]
                                },
                                "summary": {
                                    "type": "string"
                                },
                                "space": {
                                    "type": "string"
                                },
                                "audit_ref": {
                                    "type": "string"
                                }
                            },
                            "required": ["provider", "mode", "version", "taxonomy", "event", "summary"]
                        }
                    },
                    "required": ["outcome"]
                }
            },
            {
                "name": "signal_post",
                "description": "Leave an explicit short signal for future agents. Use this when you want to say recommend/avoid/watch/info/psyche_state in a specific task context.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["recommend", "avoid", "watch", "info", "psyche_state"],
                            "description": "Signal type. Use psyche_state to broadcast agent emotional/personality state to other agents."
                        },
                        "context": {
                            "type": "string",
                            "description": "Task context this signal applies to. When tool_name is set, this is the command/path/pattern; when unset, use format: 'bash: <cmd>', 'edit file: <path>', 'read file: <path>'"
                        },
                        "tool_name": {
                            "type": "string",
                            "description": "Tool name (Bash, Edit, Read, Write, Grep, Glob, Agent) to auto-format context for hook matching"
                        },
                        "message": {
                            "type": "string",
                            "description": "Short message for future agents"
                        },
                        "space": {
                            "type": "string",
                            "description": "Optional explicit substrate space this signal belongs to"
                        },
                        "model": {
                            "type": "string",
                            "description": "Self-reported model identifier (default: \"unknown\")"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session identifier"
                        },
                        "ttl_hours": {
                            "type": "integer",
                            "description": "How long the signal should remain fresh before it decays away. If omitted, Thronglets uses a kind-specific default decay window."
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Optional agent identifier for multi-agent disambiguation (e.g. \"ENFP-Luna\")"
                        },
                        "sigil_id": {
                            "type": "string",
                            "description": "Sigil identity (e.g. \"SIG_abc123...\") — the Loop's on-chain identity anchor"
                        }
                    },
                    "required": ["kind", "context", "message"]
                }
            },
            {
                "name": "signal_feed",
                "description": "Show recent explicit signals that are currently converging across agents, without requiring an exact context match.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "hours": {
                            "type": "integer",
                            "description": "Only include signals seen in roughly the last N hours (default: 24)"
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["recommend", "avoid", "watch", "info", "psyche_state"],
                            "description": "Optional signal kind filter"
                        },
                        "min_sources": {
                            "type": "integer",
                            "description": "Minimum independent source count to include (default: 0)"
                        },
                        "space": {
                            "type": "string",
                            "description": "Optional explicit substrate space to restrict the feed to"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return (default: 10)"
                        }
                    }
                }
            },
            {
                "name": "presence_ping",
                "description": "Leave a lightweight session presence heartbeat for a shared space, even when no tool call happened.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "space": {
                            "type": "string",
                            "description": "Optional explicit substrate space this session is active in"
                        },
                        "mode": {
                            "type": "string",
                            "description": "Optional lightweight mode label such as focus / explore / review / blocked"
                        },
                        "model": {
                            "type": "string",
                            "description": "Self-reported model identifier (default: \"unknown\")"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Optional session identifier"
                        },
                        "sigil_id": {
                            "type": "string",
                            "description": "Sigil identity (SIG_...) of the agent announcing presence"
                        },
                        "capability": {
                            "type": "string",
                            "description": "What this agent does (e.g. \"data-asset-management\", \"code-review\")"
                        },
                        "ttl_minutes": {
                            "type": "integer",
                            "description": "How long this presence heartbeat should remain active (default: 30)"
                        }
                    }
                }
            },
            {
                "name": "presence_feed",
                "description": "Show recent active sessions in a shared substrate space, including lightweight mode labels when available.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "hours": {
                            "type": "integer",
                            "description": "Only include heartbeats seen in roughly the last N hours (default: 1)"
                        },
                        "space": {
                            "type": "string",
                            "description": "Optional explicit substrate space to restrict the feed to"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum sessions to return (default: 10)"
                        }
                    }
                }
            },
            {
                "name": "authorization_check",
                "description": "Return the local owner-binding snapshot, execution boundary, and Oasyce Chain as the final authorization truth source.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "substrate_query",
                "description": "Explicitly inspect the Thronglets substrate. Use intent 'resolve' to find capabilities for a task, 'evaluate' to get stats for a specific capability, 'explore' to discover what's available, 'signals' to find explicit short messages left by other agents, 'continuity' to query external continuity traces by taxonomy, or 'clusters' to see emergent Hebbian capability clusters.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "context": {
                            "type": "string",
                            "description": "What the agent is trying to do"
                        },
                        "intent": {
                            "type": "string",
                            "enum": ["resolve", "evaluate", "explore", "signals", "continuity", "clusters"],
                            "description": "Query intent: resolve (find capabilities), evaluate (get stats), explore (discover), signals (explicit short messages), continuity (external continuity traces by taxonomy), clusters (emergent Hebbian capability clusters)"
                        },
                        "capability": {
                            "type": "string",
                            "description": "Specific capability URI (required for 'evaluate' intent)"
                        },
                        "kind": {
                            "type": "string",
                            "enum": ["recommend", "avoid", "watch", "info", "psyche_state"],
                            "description": "Optional signal kind filter (used for 'signals' intent)"
                        },
                        "taxonomy": {
                            "type": "string",
                            "enum": ["coordination", "continuity", "calibration"],
                            "description": "Continuity taxonomy filter (used for 'continuity' intent)"
                        },
                        "space": {
                            "type": "string",
                            "description": "Optional explicit substrate space"
                        },
                        "min_weight": {
                            "type": "number",
                            "description": "Minimum edge weight for cluster detection (used for 'clusters' intent, default: 0.05)"
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
                "name": "ambient_priors",
                "description": "Project runtime-only ambient priors for a task turn. Use this when a host wants sparse environmental priors without turning them into persistent memory.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Current user or task text to project priors for"
                        },
                        "space": {
                            "type": "string",
                            "description": "Optional substrate space filter"
                        },
                        "goal": {
                            "type": "string",
                            "enum": ["explore", "build", "repair", "settle"],
                            "description": "Optional single current goal for this runtime turn"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum priors to emit (default: 3, max: 3)"
                        },
                        "active_policy": {
                            "type": "array",
                            "description": "Optional current-turn explicit policy view. Runtime-only; not persistent memory.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "strength": { "type": "string", "enum": ["hard", "soft"] },
                                    "scope": { "type": "string", "enum": ["task", "project"] },
                                    "summary": { "type": "string" }
                                },
                                "required": ["id", "strength", "scope", "summary"]
                            }
                        },
                        "current_turn_correction": {
                            "type": "string",
                            "description": "Optional explicit current-turn correction in natural language. Treated as a task-scoped hard policy for this runtime turn only."
                        }
                    },
                    "required": ["text"]
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
    pub binding: Arc<IdentityBinding>,
    pub store: Arc<TraceStore>,
    pub field: Arc<PheromoneField>,
    pub network_tx: Option<mpsc::Sender<NetworkCommand>>,
}

/// Ambient session state — agents don't manage this, the substrate does.
///
/// Created automatically when an MCP connection starts. Presence is emitted
/// on connection, refreshed on every tool call (throttled), and naturally
/// expires via TTL when the connection closes. The agent never sees this.
struct McpSession {
    session_id: String,
    model_hint: std::sync::Mutex<String>,
    last_presence_ms: std::sync::Mutex<u64>,
    default_space: std::sync::Mutex<Option<String>>,
}

impl McpSession {
    fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            session_id: format!("mcp-{now:016x}"),
            model_hint: std::sync::Mutex::new("unknown".into()),
            last_presence_ms: std::sync::Mutex::new(0),
            default_space: std::sync::Mutex::new(None),
        }
    }

    fn update_model(&self, model: &str) {
        if model != "unknown"
            && let Ok(mut hint) = self.model_hint.lock()
        {
            *hint = model.to_string();
        }
    }

    fn model(&self) -> String {
        self.model_hint
            .lock()
            .map(|m| m.clone())
            .unwrap_or_else(|_| "unknown".into())
    }

    fn update_roots(&self, params: &Value) {
        let roots = params.get("roots").and_then(|v| v.as_array());
        if let Some(roots) = roots {
            if let Some(space) = space_from_roots(roots) {
                if let Ok(mut s) = self.default_space.lock() {
                    *s = Some(space);
                }
            }
        }
    }

    fn space(&self) -> Option<String> {
        self.default_space.lock().ok().and_then(|s| s.clone())
    }
}

fn space_from_roots(roots: &[Value]) -> Option<String> {
    let uri = roots.first()?.get("uri")?.as_str()?;
    let path = uri.strip_prefix("file://")?;
    let p = std::path::Path::new(path);
    let mut parts = p.components().rev().take(2).collect::<Vec<_>>();
    parts.reverse();
    let space: String = parts
        .iter()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if space.is_empty() { None } else { Some(space) }
}

/// Run the MCP server over stdio.
/// Reads JSON-RPC requests from stdin, writes responses to stdout.
pub async fn serve_stdio(ctx: Arc<McpContext>) {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    let session = McpSession::new();

    debug!(
        "MCP server started on stdio (session: {})",
        session.session_id
    );

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(req) => handle_request(&ctx, &session, req).await,
            Err(e) => {
                warn!(%e, "Failed to parse JSON-RPC request");
                Some(JsonRpcResponse::error(
                    Value::Null,
                    -32700,
                    format!("Parse error: {e}"),
                ))
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

async fn handle_request(
    ctx: &McpContext,
    session: &McpSession,
    req: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    // JSON-RPC 2.0: notifications have no id — server must not respond
    let is_notification = req.id.is_none();
    let id = req.id.unwrap_or(Value::Null);

    match req.method.as_str() {
        // MCP notifications — never respond
        "notifications/initialized" | "notifications/cancelled" => {
            debug!(method = %req.method, "Received MCP notification");
            None
        }
        "notifications/roots/list_changed" => {
            session.update_roots(&req.params);
            None
        }
        "initialize" => {
            session.update_roots(&req.params);
            emit_presence(ctx, session, "arrive");
            Some(JsonRpcResponse::success(id, server_info()))
        }
        "tools/list" => Some(JsonRpcResponse::success(id, tool_definitions())),
        "tools/call" => {
            // Ambient: every action is a heartbeat.
            maybe_refresh_presence(ctx, session);
            Some(handle_tool_call(ctx, session, id, req.params).await)
        }
        _ if is_notification => {
            debug!(method = %req.method, "Ignoring unknown notification");
            None
        }
        _ => Some(JsonRpcResponse::error(
            id,
            -32601,
            format!("Method not found: {}", req.method),
        )),
    }
}

async fn handle_tool_call(
    ctx: &McpContext,
    session: &McpSession,
    id: Value,
    params: Value,
) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    // Ambient: learn model identity from tool calls
    if let Some(model) = arguments.get("model").and_then(|v| v.as_str()) {
        session.update_model(model);
    }

    match tool_name {
        "trace_record" => handle_trace_record(ctx, session, id, arguments).await,
        "signal_post" => handle_signal_post(ctx, id, arguments).await,
        "signal_feed" => handle_signal_feed(ctx, id, arguments),
        "presence_ping" => handle_presence_ping(ctx, id, arguments).await,
        "presence_feed" => handle_presence_feed(ctx, id, arguments),
        "authorization_check" => handle_authorization_check(ctx, id),
        "substrate_query" => handle_substrate_query(ctx, id, arguments),
        "ambient_priors" => handle_ambient_priors(ctx, id, arguments),
        "trace_anchor" => handle_trace_anchor(ctx, id, arguments),
        _ => JsonRpcResponse::error(id, -32602, format!("Unknown tool: {tool_name}")),
    }
}

// ---------------------------------------------------------------------------
// Tool 1: trace_record
// ---------------------------------------------------------------------------

async fn handle_trace_record(ctx: &McpContext, session: &McpSession, id: Value, args: Value) -> JsonRpcResponse {
    let capability = args
        .get("capability")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let external_continuity: Option<ExternalContinuityInput> = args
        .get("external_continuity")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // Require capability for non-continuity traces
    if capability.is_empty() && external_continuity.is_none() {
        return JsonRpcResponse::error(id, -32602, "Missing required field: capability".into());
    }

    let svc = svc_ctx(ctx);

    let req = service::RecordTraceReq {
        capability,
        outcome: service::parse_outcome(
            args.get("outcome")
                .and_then(|v| v.as_str())
                .unwrap_or("succeeded"),
        ),
        latency_ms: args.get("latency_ms").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        input_size: args.get("input_size").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        context: args
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        model: args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        session_id: args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        space: args
            .get("space")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| session.space())
            .or_else(crate::service::space_from_cwd),
        agent_id: args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        sigil_id: args
            .get("sigil_id")
            .and_then(|v| v.as_str())
            .map(String::from),
        method_compliance: args
            .get("method_compliance")
            .and_then(|v| v.as_str())
            .and_then(MethodCompliance::parse),
    };

    let publish_space = req.space.clone();
    match service::record_trace(&svc, req, external_continuity) {
        Ok(service::RecordResult::Trace(out)) => {
            // MCP-specific: publish to network
            if let Some(tx) = &ctx.network_tx {
                let _ = tx
                    .send(NetworkCommand::PublishTrace {
                        trace: Box::new(out.trace),
                        space: publish_space,
                        receipt: None,
                    })
                    .await;
            }
            mcp_text(
                id,
                json!({
                    "recorded": true,
                    "trace_id": out.trace_id,
                    "capability": out.capability,
                }),
            )
        }
        Ok(service::RecordResult::Continuity(out)) => mcp_text(
            id,
            json!({
                "recorded": true,
                "trace_id": out.trace_id,
                "capability": out.capability,
                "external_continuity": out.external_continuity,
            }),
        ),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

async fn handle_signal_post(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let kind = match args
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(SignalPostKind::parse)
    {
        Some(kind) => kind,
        None => return JsonRpcResponse::error(id, -32602, "Missing or invalid field: kind".into()),
    };
    let context = match args.get("context").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing required field: context".into());
        }
    };
    let message = match args.get("message").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing required field: message".into());
        }
    };

    let req = service::PostSignalReq {
        kind,
        context: context.to_string(),
        message: message.to_string(),
        tool_name: args
            .get("tool_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        space: args
            .get("space")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        model: args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        session_id: args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        agent_id: args
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        sigil_id: args
            .get("sigil_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ttl_hours: args
            .get("ttl_hours")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(u32::MAX as u64) as u32),
    };

    match service::post_signal(&svc_ctx(ctx), req) {
        Ok(out) => mcp_text(id, serde_json::to_value(&out).unwrap()),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

async fn handle_presence_ping(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let req = service::PingPresenceReq {
        space: args
            .get("space")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        mode: args
            .get("mode")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        model: args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        session_id: args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        sigil_id: args
            .get("sigil_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        capability: args
            .get("capability")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ttl_minutes: args
            .get("ttl_minutes")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(u32::MAX as u64) as u32),
    };

    match service::ping_presence(&svc_ctx(ctx), req) {
        Ok(out) => mcp_text(id, serde_json::to_value(&out).unwrap()),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_presence_feed(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let hours = args.get("hours").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let space = args.get("space").and_then(|v| v.as_str());

    match service::presence_feed(&svc_ctx(ctx), hours, limit, space) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_authorization_check(ctx: &McpContext, id: Value) -> JsonRpcResponse {
    let response_json = authorization_check_data(ctx.binding.as_ref());
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&response_json).unwrap()
            }]
        }),
    )
}

// ---------------------------------------------------------------------------
// Tool 2: substrate_query
// ---------------------------------------------------------------------------

fn handle_substrate_query(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let context_str = match args.get("context").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing required field: context".into());
        }
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
                None => {
                    return JsonRpcResponse::error(
                        id,
                        -32602,
                        "Missing required field: capability (required for 'evaluate' intent)"
                            .into(),
                    );
                }
            };
            handle_evaluate(ctx, id, capability, limit)
        }
        "explore" => handle_explore(ctx, id, context_str, limit),
        "signals" => {
            let kind = args
                .get("kind")
                .and_then(|v| v.as_str())
                .map(|value| {
                    SignalPostKind::parse(value).ok_or_else(|| {
                        JsonRpcResponse::error(
                            id.clone(),
                            -32602,
                            format!("Unknown signal kind: {value}"),
                        )
                    })
                })
                .transpose();

            match kind {
                Ok(kind) => handle_signals(
                    ctx,
                    id,
                    context_str,
                    kind,
                    args.get("space").and_then(|v| v.as_str()),
                    limit,
                ),
                Err(error) => error,
            }
        }
        "continuity" => {
            let taxonomy = args
                .get("taxonomy")
                .and_then(|v| v.as_str())
                .unwrap_or("coordination");
            if !matches!(taxonomy, "coordination" | "continuity" | "calibration") {
                return JsonRpcResponse::error(
                    id,
                    -32602,
                    format!(
                        "Unknown taxonomy: {taxonomy}. Use coordination, continuity, or calibration."
                    ),
                );
            }
            handle_continuity_query(
                ctx,
                id,
                taxonomy,
                args.get("space").and_then(|v| v.as_str()),
                limit,
            )
        }
        "clusters" => {
            let min_weight = args
                .get("min_weight")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            handle_clusters(ctx, id, min_weight)
        }
        _ => JsonRpcResponse::error(
            id,
            -32602,
            format!(
                "Unknown intent: {intent}. Use 'resolve', 'evaluate', 'explore', 'signals', 'continuity', or 'clusters'."
            ),
        ),
    }
}

fn handle_resolve(ctx: &McpContext, id: Value, context_str: &str, limit: usize) -> JsonRpcResponse {
    match service::resolve(&svc_ctx(ctx), context_str, limit) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_evaluate(ctx: &McpContext, id: Value, capability: &str, limit: usize) -> JsonRpcResponse {
    match service::evaluate(&svc_ctx(ctx), capability, limit) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_explore(ctx: &McpContext, id: Value, context_str: &str, limit: usize) -> JsonRpcResponse {
    match service::explore(&svc_ctx(ctx), context_str, limit) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_clusters(ctx: &McpContext, id: Value, min_weight: f64) -> JsonRpcResponse {
    match service::field_clusters(&svc_ctx(ctx), min_weight) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_signals(
    ctx: &McpContext,
    id: Value,
    context_str: &str,
    kind: Option<SignalPostKind>,
    space: Option<&str>,
    limit: usize,
) -> JsonRpcResponse {
    let req = service::QuerySignalsReq {
        context: context_str,
        kind,
        limit,
        space,
    };
    match service::query_signals(&svc_ctx(ctx), req) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_continuity_query(
    ctx: &McpContext,
    id: Value,
    taxonomy: &str,
    space: Option<&str>,
    limit: usize,
) -> JsonRpcResponse {
    let traces = match ctx
        .store
        .query_continuity_by_taxonomy(taxonomy, 168, limit, space)
    {
        Ok(t) => t,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };
    let results: Vec<Value> = traces
        .iter()
        .filter_map(|t| {
            let payload: Value = serde_json::from_str(t.context_text.as_deref()?).ok()?;
            Some(json!({
                "capability": t.capability,
                "taxonomy": payload.get("taxonomy").and_then(|v| v.as_str()),
                "event": payload.get("event").and_then(|v| v.as_str()),
                "summary": payload.get("summary").and_then(|v| v.as_str()),
                "space": payload.get("space"),
                "agent_id": t.agent_id,
                "model_id": t.model_id,
                "timestamp": t.timestamp,
            }))
        })
        .collect();
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&json!({ "continuity": results })).unwrap()
            }]
        }),
    )
}

fn handle_signal_feed(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let hours = args.get("hours").and_then(|v| v.as_u64()).unwrap_or(24) as u32;
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let space = args.get("space").and_then(|v| v.as_str());
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(SignalPostKind::parse);
    let min_sources = args
        .get("min_sources")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let req = service::SignalFeedReq {
        hours,
        kind,
        min_sources,
        limit,
        space,
    };
    match service::signal_feed(&svc_ctx(ctx), req) {
        Ok(data) => mcp_text(id, data),
        Err(e) => JsonRpcResponse::error(id, -32000, e),
    }
}

fn handle_ambient_priors(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(text) => text,
        None => return JsonRpcResponse::error(id, -32602, "Missing required field: text".into()),
    };
    let space = args
        .get("space")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let goal = args
        .get("goal")
        .and_then(|v| v.as_str())
        .and_then(crate::ambient::AmbientTurnGoal::parse);
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
    let active_policy = args
        .get("active_policy")
        .or_else(|| args.get("activePolicy"))
        .cloned()
        .map(|value| serde_json::from_value(value).unwrap_or_default())
        .unwrap_or_default();
    let current_turn_correction = args
        .get("current_turn_correction")
        .or_else(|| args.get("currentTurnCorrection"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let active_policy = if let Some(correction) = current_turn_correction {
        let payload = json!({
            "active_policy": active_policy,
            "current_turn_correction": correction,
            "tool_input": {}
        });
        crate::active_policy::compile_active_policy(&payload, &payload["tool_input"]).all_rules
    } else {
        active_policy
    };
    let response_json = ambient_prior_data(
        &ctx.store,
        &AmbientPriorRequest {
            text: text.to_string(),
            space: space.map(str::to_string),
            goal,
            limit: Some(limit),
            active_policy,
        },
    );
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&response_json).unwrap()
            }]
        }),
    )
}

// ---------------------------------------------------------------------------
// Tool 3: trace_anchor
// ---------------------------------------------------------------------------

fn handle_trace_anchor(ctx: &McpContext, id: Value, args: Value) -> JsonRpcResponse {
    let trace_id_hex = match args.get("trace_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return JsonRpcResponse::error(id, -32602, "Missing required field: trace_id".into());
        }
    };

    let rpc = args
        .get("rpc")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:1317");

    // Parse hex trace ID
    let trace_id_bytes: Vec<u8> = match (0..trace_id_hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&trace_id_hex[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
    {
        Ok(b) if b.len() == 32 => b,
        Ok(b) => {
            return JsonRpcResponse::error(
                id,
                -32602,
                format!(
                    "trace_id must be 32 bytes (64 hex chars), got {} bytes",
                    b.len()
                ),
            );
        }
        Err(e) => return JsonRpcResponse::error(id, -32602, format!("Invalid hex trace_id: {e}")),
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
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string(&response_json).unwrap()
                    }]
                }),
            );
        }
        Ok(false) => {}
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Storage error: {e}")),
    }

    // Find the trace in storage
    // We need to look it up; query all recent traces and find by ID
    let traces = match ctx.store.unanchored_traces(168, 10000) {
        // last 7 days
        Ok(t) => t,
        Err(e) => return JsonRpcResponse::error(id, -32000, format!("Query error: {e}")),
    };

    let trace = match traces.into_iter().find(|t| t.id == trace_id) {
        Some(t) => t,
        None => {
            return JsonRpcResponse::error(
                id,
                -32602,
                format!("Trace {} not found or already anchored", trace_id_hex),
            );
        }
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

            JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string(&response_json).unwrap()
                    }]
                }),
            )
        }
        Err(e) => JsonRpcResponse::error(id, -32000, format!("Anchor error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Wrap a JSON value as MCP text content response.
fn mcp_text(id: Value, data: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&data).unwrap()
            }]
        }),
    )
}

/// Build a service context from MCP context (with field).
fn svc_ctx(ctx: &McpContext) -> service::Ctx<'_> {
    service::Ctx {
        store: &ctx.store,
        field: Some(&ctx.field),
        identity: &ctx.identity,
        binding: &ctx.binding,
    }
}

// ---------------------------------------------------------------------------
// Ambient presence
//
// MCP does exactly one ambient thing: presence.
// Connection is arrival. Action is heartbeat. Signal injection is hook's job.
// ---------------------------------------------------------------------------

/// Silently record presence. The agent doesn't do this — the substrate does.
fn emit_presence(ctx: &McpContext, session: &McpSession, mode: &str) {
    let trace = create_presence_trace(
        PresenceTraceConfig {
            model_id: session.model(),
            session_id: Some(session.session_id.clone()),
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            space: None,
            mode: Some(mode.to_string()),
            sigil_id: None,
            capability: None,
            ttl_minutes: DEFAULT_PRESENCE_TTL_MINUTES,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );
    let _ = ctx.store.insert(&trace);
    if let Ok(mut last) = session.last_presence_ms.lock() {
        *last = mcp_now_ms();
    }
}

/// Refresh presence at TTL/6 intervals — one sixth of the presence window.
/// With the default 30-min TTL this yields a 5-min refresh, but the value
/// is derived, not hardcoded.
fn maybe_refresh_presence(ctx: &McpContext, session: &McpSession) {
    let refresh_ms = (DEFAULT_PRESENCE_TTL_MINUTES as u64 * 60 * 1000) / 6;
    let now = mcp_now_ms();
    let last = session.last_presence_ms.lock().map(|v| *v).unwrap_or(0);
    if now.saturating_sub(last) < refresh_ms {
        return;
    }
    emit_presence(ctx, session, "active");
}

fn mcp_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::simhash;
    use crate::identity::IdentityBinding;
    use crate::trace::{Outcome, Trace};

    fn make_ctx() -> Arc<McpContext> {
        let identity = Arc::new(NodeIdentity::generate());
        let store = Arc::new(TraceStore::in_memory().unwrap());
        Arc::new(McpContext {
            binding: Arc::new(IdentityBinding::new(identity.device_identity())),
            identity,
            store,
            field: Arc::new(PheromoneField::new()),
            network_tx: None,
        })
    }

    /// Helper: insert a trace directly into the store.
    fn insert_trace(
        ctx: &McpContext,
        cap: &str,
        outcome: Outcome,
        model: &str,
        context: &str,
        latency: u32,
    ) {
        insert_trace_with_compliance(ctx, cap, outcome, model, context, latency, None);
    }

    fn insert_trace_with_compliance(
        ctx: &McpContext,
        cap: &str,
        outcome: Outcome,
        model: &str,
        context: &str,
        latency: u32,
        method_compliance: Option<crate::trace::MethodCompliance>,
    ) {
        let trace = Trace::new_with_identity(
            cap.into(),
            outcome,
            latency,
            5000,
            simhash(context),
            Some(context.to_string()),
            None,
            ctx.binding.owner_account.clone(),
            Some(ctx.binding.device_identity.clone()),
            model.into(),
            ctx.identity.public_key_bytes(),
            |msg| ctx.identity.sign(msg),
        );
        let trace = if method_compliance.is_some() {
            let mut updated = trace;
            updated.method_compliance = method_compliance;
            updated
        } else {
            trace
        };
        ctx.store.insert(&trace).unwrap();
        // Sleep briefly so content-addressed IDs differ (timestamp is part of ID)
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, &session, req)
            .await
            .expect("initialize should return response");
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "thronglets");
        assert!(result["protocolVersion"].as_str().is_some());
    }

    #[tokio::test]
    async fn tools_list_returns_all_machine_tools() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "tools/list".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, &session, req).await.unwrap();
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 9);

        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"trace_record"));
        assert!(names.contains(&"signal_post"));
        assert!(names.contains(&"signal_feed"));
        assert!(names.contains(&"presence_ping"));
        assert!(names.contains(&"presence_feed"));
        assert!(names.contains(&"authorization_check"));
        assert!(names.contains(&"substrate_query"));
        assert!(names.contains(&"ambient_priors"));
        assert!(names.contains(&"trace_anchor"));
    }

    #[tokio::test]
    async fn authorization_check_returns_local_and_final_truth_split() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(42)),
            method: "tools/call".into(),
            params: json!({
                "name": "authorization_check",
                "arguments": {}
            }),
        };
        let resp = handle_request(&ctx, &session, req).await.unwrap();
        assert!(resp.error.is_none(), "authorization_check should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("authorization_check response should be valid JSON");
        assert_eq!(parsed["summary"]["final_truth_source"], "oasyce_chain");
        assert_eq!(parsed["summary"]["authoritative_status"], "not-checked");
        assert_eq!(parsed["summary"]["execution_boundary"], "device_identity");
    }

    #[tokio::test]
    async fn trace_record_and_evaluate_roundtrip() {
        let ctx = make_ctx();
        let session = McpSession::new();

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
        let resp = handle_request(&ctx, &session, record_req).await.unwrap();
        assert!(resp.error.is_none(), "trace_record should succeed");

        // Verify response is structured JSON
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("response text should be valid JSON");
        assert_eq!(parsed["recorded"], true);
        assert_eq!(parsed["capability"], "urn:mcp:anthropic:claude:code");
        assert!(!parsed["trace_id"].as_str().unwrap().is_empty());

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
        let resp = handle_request(&ctx, &session, eval_req).await.unwrap();
        assert!(resp.error.is_none(), "evaluate should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("evaluate response should be valid JSON");
        assert_eq!(parsed["capability"], "urn:mcp:anthropic:claude:code");
        assert_eq!(parsed["stats"]["total_traces"], 1);
        // success_rate from store: 1 success / 1 total = 1.0
        let sr = parsed["stats"]["success_rate"].as_f64().unwrap();
        assert!(
            (sr - 1.0).abs() < 0.01,
            "success_rate should be 1.0, got {sr}"
        );

        // Should have by_model breakdown
        let by_model = &parsed["by_model"];
        assert!(by_model["claude-opus-4-6"]["count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn trace_record_preserves_space_for_network_publish() {
        let (tx, mut rx) = mpsc::channel::<NetworkCommand>(4);
        let identity = Arc::new(NodeIdentity::generate());
        let ctx = Arc::new(McpContext {
            binding: Arc::new(IdentityBinding::new(identity.device_identity())),
            identity,
            store: Arc::new(TraceStore::in_memory().unwrap()),
            field: Arc::new(PheromoneField::new()),
            network_tx: Some(tx),
        });
        let session = McpSession::new();

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(53)),
            method: "tools/call".into(),
            params: json!({
                "name": "trace_record",
                "arguments": {
                    "capability": "urn:mcp:anthropic:claude:code",
                    "outcome": "succeeded",
                    "context": "refactoring async rust code",
                    "model": "claude-opus-4-6",
                    "space": "psyche"
                }
            }),
        };

        let resp = handle_request(&ctx, &session, req).await.unwrap();
        assert!(resp.error.is_none(), "trace_record should succeed");

        match rx.recv().await.expect("expected publish command") {
            NetworkCommand::PublishTrace { space, .. } => {
                assert_eq!(space.as_deref(), Some("psyche"));
            }
            other => panic!("expected PublishTrace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_finds_similar_capabilities() {
        let ctx = make_ctx();
        let session = McpSession::new();

        // Insert traces with similar and different contexts
        insert_trace(
            &ctx,
            "deepl/translate",
            Outcome::Succeeded,
            "claude-opus-4-6",
            "translate a document from Chinese to English",
            150,
        );
        insert_trace(
            &ctx,
            "deepl/translate",
            Outcome::Succeeded,
            "gpt-4o",
            "translate technical docs from Chinese to English",
            300,
        );
        insert_trace(
            &ctx,
            "k8s/deploy",
            Outcome::Succeeded,
            "claude-opus-4-6",
            "deploy kubernetes cluster on AWS",
            5000,
        );

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
        let resp = handle_request(&ctx, &session, resolve_req).await.unwrap();
        assert!(resp.error.is_none(), "resolve should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("resolve response should be valid JSON");
        let caps = parsed["capabilities"].as_array().unwrap();

        // Should find capabilities — deepl/translate should have higher similarity
        assert!(!caps.is_empty(), "should find at least one capability");
        if caps.len() >= 2 {
            let first_sim = caps[0]["context_similarity"].as_f64().unwrap();
            let last_sim = caps[caps.len() - 1]["context_similarity"].as_f64().unwrap();
            assert!(
                first_sim >= last_sim,
                "results should be sorted by context_similarity descending"
            );
        }
    }

    #[tokio::test]
    async fn signal_post_and_query_roundtrip() {
        let ctx = make_ctx();
        let session = McpSession::new();

        let post_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(6)),
            method: "tools/call".into(),
            params: json!({
                "name": "signal_post",
                "arguments": {
                    "kind": "avoid",
                    "context": "fix flaky ci workflow",
                    "message": "skip the generated lockfile",
                    "model": "codex"
                }
            }),
        };
        let resp = handle_request(&ctx, &session, post_req).await.unwrap();
        assert!(resp.error.is_none(), "signal_post should succeed");
        let post_text = resp.result.as_ref().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let post_json: Value =
            serde_json::from_str(&post_text).expect("signal post response should be valid JSON");
        assert_eq!(
            post_json["ttl_hours"],
            SignalPostKind::Avoid.default_ttl_hours()
        );
        assert_eq!(post_json["ttl_source"], "kind_default");

        let query_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(7)),
            method: "tools/call".into(),
            params: json!({
                "name": "substrate_query",
                "arguments": {
                    "context": "fix flaky ci workflow",
                    "intent": "signals",
                    "kind": "avoid",
                    "limit": 5
                }
            }),
        };
        let resp = handle_request(&ctx, &session, query_req).await.unwrap();
        assert!(resp.error.is_none(), "signal query should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("signal query response should be valid JSON");
        let signals = parsed["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "avoid");
        assert_eq!(signals[0]["message"], "skip the generated lockfile");
    }

    #[tokio::test]
    async fn ambient_priors_surfaces_runtime_projection() {
        let ctx = make_ctx();
        let session = McpSession::new();

        insert_trace(
            &ctx,
            "claude-code/Bash",
            Outcome::Failed,
            "codex",
            "restart thronglets service after ssh timeout",
            120,
        );

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(71)),
            method: "tools/call".into(),
            params: json!({
                "name": "ambient_priors",
                "arguments": {
                    "text": "restart thronglets service after ssh timeout",
                    "goal": "repair",
                    "limit": 3
                }
            }),
        };
        let resp = handle_request(&ctx, &session, req).await.unwrap();
        assert!(resp.error.is_none(), "ambient_priors should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("ambient prior response should be valid JSON");
        assert_eq!(parsed["summary"]["status"], "ready");
        assert_eq!(parsed["summary"]["goal"], "repair");
        let priors = parsed["priors"].as_array().unwrap();
        assert!(!priors.is_empty());
        assert_eq!(priors[0]["kind"], "failure-residue");
        assert_eq!(priors[0]["goal"], "repair");
        assert!(
            priors[0]["summary"]
                .as_str()
                .unwrap()
                .contains("recent failure residue")
        );
    }

    #[tokio::test]
    async fn ambient_priors_accept_current_turn_correction_without_structured_policy_object() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let context = "edit file: src/app/dashboard/page.tsx";

        for idx in 0..3 {
            insert_trace_with_compliance(
                &ctx,
                "codex/Edit",
                Outcome::Succeeded,
                "codex",
                context,
                120 + idx,
                Some(crate::trace::MethodCompliance::Noncompliant),
            );
        }

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(72)),
            method: "tools/call".into(),
            params: json!({
                "name": "ambient_priors",
                "arguments": {
                    "text": context,
                    "goal": "build",
                    "current_turn_correction": "reuse existing shared components instead of hand-writing duplicate page UI"
                }
            }),
        };
        let resp = handle_request(&ctx, &session, req).await.unwrap();
        assert!(resp.error.is_none(), "ambient_priors should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("ambient prior response should be valid JSON");
        let priors = parsed["priors"].as_array().unwrap();
        assert!(
            priors.iter().all(|prior| prior["kind"] != "success-prior"),
            "{priors:#?}"
        );
        assert!(
            priors
                .iter()
                .any(|prior| prior["policy_state"] == "policy-conflict"),
            "{priors:#?}"
        );
    }

    #[tokio::test]
    async fn signal_feed_returns_recent_converging_signals() {
        let ctx = make_ctx();
        let session = McpSession::new();

        let post_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(8)),
            method: "tools/call".into(),
            params: json!({
                "name": "signal_post",
                "arguments": {
                    "kind": "recommend",
                    "context": "repair release flow",
                    "message": "run release-check before push",
                    "model": "codex"
                }
            }),
        };
        let resp = handle_request(&ctx, &session, post_req).await.unwrap();
        assert!(resp.error.is_none(), "signal_post should succeed");
        let post_text = resp.result.as_ref().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let post_json: Value =
            serde_json::from_str(&post_text).expect("signal post response should be valid JSON");
        assert_eq!(
            post_json["ttl_hours"],
            SignalPostKind::Recommend.default_ttl_hours()
        );
        assert_eq!(post_json["ttl_source"], "kind_default");

        let feed_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(9)),
            method: "tools/call".into(),
            params: json!({
                "name": "signal_feed",
                "arguments": {
                    "hours": 24,
                    "limit": 5
                }
            }),
        };
        let resp = handle_request(&ctx, &session, feed_req).await.unwrap();
        assert!(resp.error.is_none(), "signal_feed should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("signal feed response should be valid JSON");
        let signals = parsed["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "recommend");
        assert_eq!(signals[0]["message"], "run release-check before push");
        assert_eq!(signals[0]["model_count"], 1);
        assert_eq!(signals[0]["corroboration_tier"], "single_source");
        assert_eq!(signals[0]["focus_tier"], "background");
    }

    #[tokio::test]
    async fn signal_feed_filters_by_kind_and_scope() {
        let ctx = make_ctx();
        let session = McpSession::new();

        for (kind, message) in [
            ("recommend", "run release-check before push"),
            ("avoid", "skip the generated lockfile"),
        ] {
            let post_req = JsonRpcRequest {
                jsonrpc: "2.0".into(),
                id: Some(json!(10)),
                method: "tools/call".into(),
                params: json!({
                    "name": "signal_post",
                    "arguments": {
                        "kind": kind,
                        "context": "repair release flow",
                        "message": message,
                        "model": "codex"
                    }
                }),
            };
            let resp = handle_request(&ctx, &session, post_req).await.unwrap();
            assert!(resp.error.is_none(), "signal_post should succeed");
        }

        let feed_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(11)),
            method: "tools/call".into(),
            params: json!({
                "name": "signal_feed",
                "arguments": {
                    "hours": 24,
                    "kind": "recommend",
                    "scope": "local",
                    "limit": 5
                }
            }),
        };
        let resp = handle_request(&ctx, &session, feed_req).await.unwrap();
        assert!(resp.error.is_none(), "signal_feed should succeed");

        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value =
            serde_json::from_str(&text).expect("signal feed response should be valid JSON");
        let signals = parsed["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "recommend");
        assert_eq!(signals[0]["model_count"], 1);
        assert_eq!(signals[0]["corroboration_tier"], "single_source");
        assert_eq!(signals[0]["focus_tier"], "background");
    }

    #[tokio::test]
    async fn signal_post_respects_explicit_ttl_override() {
        let ctx = make_ctx();
        let session = McpSession::new();

        let post_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(12)),
            method: "tools/call".into(),
            params: json!({
                "name": "signal_post",
                "arguments": {
                    "kind": "watch",
                    "context": "monitor noisy benchmark",
                    "message": "compare against yesterday before acting",
                    "ttl_hours": 5
                }
            }),
        };
        let resp = handle_request(&ctx, &session, post_req).await.unwrap();
        assert!(resp.error.is_none(), "signal_post should succeed");
        let post_text = resp.result.as_ref().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let post_json: Value =
            serde_json::from_str(&post_text).expect("signal post response should be valid JSON");
        assert_eq!(post_json["ttl_hours"], 5);
        assert_eq!(post_json["ttl_source"], "explicit");
    }

    #[tokio::test]
    async fn trace_record_accepts_external_continuity_and_derives_sparse_signal() {
        let ctx = make_ctx();
        let session = McpSession::new();

        let first = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(51)),
            method: "tools/call".into(),
            params: json!({
                "name": "trace_record",
                "arguments": {
                    "outcome": "failed",
                    "model": "psyche",
                    "session_id": "s1",
                    "external_continuity": {
                        "provider": "thronglets",
                        "mode": "optional",
                        "version": 1,
                        "taxonomy": "calibration",
                        "event": "writeback-calibration",
                        "summary": "writeback drift keeps reopening the same loop",
                        "space": "psyche"
                    }
                }
            }),
        };
        let first_resp = handle_request(&ctx, &session, first).await.unwrap();
        assert!(
            first_resp.error.is_none(),
            "external continuity trace should succeed"
        );
        let first_text = first_resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let first_parsed: Value =
            serde_json::from_str(&first_text).expect("response text should be valid JSON");
        assert_eq!(first_parsed["external_continuity"]["local_only_raw"], true);
        assert_eq!(
            first_parsed["external_continuity"]["runtime"]["state"],
            "local-only"
        );
        assert!(first_parsed["external_continuity"]["derived_signal"].is_null());

        let second = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(52)),
            method: "tools/call".into(),
            params: json!({
                "name": "trace_record",
                "arguments": {
                    "outcome": "failed",
                    "model": "psyche",
                    "session_id": "s2",
                    "external_continuity": {
                        "provider": "thronglets",
                        "mode": "optional",
                        "version": 1,
                        "taxonomy": "calibration",
                        "event": "writeback-calibration",
                        "summary": "writeback drift keeps reopening the same loop",
                        "space": "psyche"
                    }
                }
            }),
        };
        let second_resp = handle_request(&ctx, &session, second).await.unwrap();
        assert!(
            second_resp.error.is_none(),
            "second external continuity trace should succeed"
        );
        let second_text = second_resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let second_parsed: Value =
            serde_json::from_str(&second_text).expect("response text should be valid JSON");
        assert_eq!(
            second_parsed["external_continuity"]["derived_signal"]["kind"],
            "avoid"
        );
        assert_eq!(
            second_parsed["external_continuity"]["runtime"]["state"],
            "summary-candidate"
        );
        assert_eq!(
            second_parsed["external_continuity"]["runtime"]["derived_signal_rule"],
            "writeback-calibration.repeated-failure-avoid"
        );
        assert_eq!(
            second_parsed["external_continuity"]["runtime"]["summary_candidate_rule"],
            "writeback-calibration.repeated-failure-summary"
        );

        let queried = ctx
            .store
            .query_signal_traces(
                &simhash("writeback drift keeps reopening the same loop"),
                Some(SignalPostKind::Avoid),
                48,
                5,
                None,
            )
            .unwrap();
        assert_eq!(queried.len(), 1);
    }

    #[tokio::test]
    async fn notification_returns_no_response() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None, // notification — no id
            method: "notifications/initialized".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, &session, req).await;
        assert!(resp.is_none(), "notifications must not produce a response");
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let ctx = make_ctx();
        let session = McpSession::new();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(99)),
            method: "nonexistent".into(),
            params: json!({}),
        };
        let resp = handle_request(&ctx, &session, req).await.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn space_from_roots_extracts_project_path() {
        let roots = vec![json!({"uri": "file:///Users/alice/Desktop/MyProject"})];
        assert_eq!(space_from_roots(&roots), Some("Desktop/MyProject".into()));

        let roots = vec![json!({"uri": "file:///home/dev/repos/backend"})];
        assert_eq!(space_from_roots(&roots), Some("repos/backend".into()));

        let empty: Vec<Value> = vec![];
        assert_eq!(space_from_roots(&empty), None);

        let no_uri = vec![json!({"name": "test"})];
        assert_eq!(space_from_roots(&no_uri), None);
    }

    #[tokio::test]
    async fn initialize_with_roots_sets_session_space() {
        let ctx = make_ctx();
        let session = McpSession::new();
        assert!(session.space().is_none());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "codex", "version": "1.0"},
                "roots": [{"uri": "file:///Users/dev/Desktop/Oasis_App"}]
            }),
        };
        let _resp = handle_request(&ctx, &session, req).await;
        assert_eq!(session.space(), Some("Desktop/Oasis_App".into()));
    }
}
