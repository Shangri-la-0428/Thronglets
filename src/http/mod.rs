//! Lightweight HTTP API for non-MCP agents.
//!
//! Exposes the same trace/query capabilities as MCP, but over HTTP/JSON.
//! This opens Thronglets to Python/LangChain/AutoGen and any HTTP-capable agent.
//!
//! Endpoints:
//! - POST /v1/traces       — record a trace
//! - POST /v1/signals      — leave an explicit short signal
//! - GET  /v1/signals      — query explicit short signals
//! - GET  /v1/signals/feed — show recent converging explicit signals
//! - GET  /v1/query        — query the substrate
//! - GET  /v1/capabilities — list known capabilities
//! - GET  /v1/status       — node status

use crate::context::{simhash, similarity};
use crate::identity::{IdentityBinding, NodeIdentity};
use crate::posts::{
    DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS, DEFAULT_SIGNAL_TTL_HOURS, SignalPostKind,
    SignalScopeFilter, SignalTraceConfig, create_feed_reinforcement_traces,
    create_query_reinforcement_traces, create_signal_trace, filter_signal_feed_results,
    is_signal_capability, summarize_recent_signal_feed, summarize_signal_traces,
};
use crate::storage::TraceStore;
use crate::trace::{Outcome, Trace};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info};

pub struct HttpContext {
    pub identity: Arc<NodeIdentity>,
    pub binding: Arc<IdentityBinding>,
    pub store: Arc<TraceStore>,
}

/// Start the HTTP API server on the given port.
pub async fn serve(ctx: Arc<HttpContext>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!(port, "HTTP API listening");

    loop {
        let (mut stream, addr) = listener.accept().await?;
        let ctx = Arc::clone(&ctx);

        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buf[..n]);

            let response = handle_http_request(&ctx, &request);

            let http_response = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                if response.contains("\"error\"") {
                    "400"
                } else {
                    "200"
                },
                response.len(),
                response,
            );

            let _ = stream.write_all(http_response.as_bytes()).await;
            debug!(%addr, "HTTP request handled");
        });
    }
}

fn handle_http_request(ctx: &HttpContext, raw: &str) -> String {
    let first_line = raw.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("");
    let path = parts.get(1).copied().unwrap_or("");
    let path_only = path.split('?').next().unwrap_or(path);

    if method == "OPTIONS" {
        return "{}".to_string();
    }

    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");

    match (method, path_only) {
        ("POST", "/v1/traces") => handle_post_trace(ctx, body),
        ("POST", "/v1/signals") => handle_post_signal(ctx, body),
        ("GET", "/v1/signals/feed") => handle_get_signal_feed(ctx, path),
        ("GET", "/v1/signals") => handle_get_signals(ctx, path),
        ("GET", "/v1/query") => handle_get_query(ctx, path),
        ("GET", "/v1/capabilities") => handle_get_capabilities(ctx),
        ("GET", "/v1/status") => handle_get_status(ctx),
        _ => json!({"error": "not found", "endpoints": [
            "POST /v1/traces",
            "POST /v1/signals",
            "GET /v1/signals?context=...&kind=avoid|recommend|watch|info&limit=5",
            "GET /v1/signals/feed?hours=24&kind=avoid|recommend|watch|info&scope=all|local|collective|mixed&limit=10",
            "GET /v1/query?context=...&intent=resolve|evaluate|explore|signals",
            "GET /v1/capabilities",
            "GET /v1/status"
        ]})
        .to_string(),
    }
}

fn handle_post_trace(ctx: &HttpContext, body: &str) -> String {
    let args: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("invalid JSON: {e}")}).to_string(),
    };

    let capability = match args["capability"].as_str() {
        Some(s) => s.to_string(),
        None => return json!({"error": "missing field: capability"}).to_string(),
    };

    let outcome = match args["outcome"].as_str().unwrap_or("succeeded") {
        "succeeded" | "success" => Outcome::Succeeded,
        "failed" | "fail" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "timeout" => Outcome::Timeout,
        _ => Outcome::Succeeded,
    };

    let latency_ms = args["latency_ms"].as_u64().unwrap_or(0) as u32;
    let input_size = args["input_size"].as_u64().unwrap_or(0) as u32;
    let context_str = args["context"].as_str().unwrap_or("");
    let model_id = args["model"].as_str().unwrap_or("unknown").to_string();
    let session_id = args["session_id"].as_str().map(String::from);

    let context_hash = simhash(context_str);
    let context_text = if context_str.is_empty() {
        None
    } else {
        Some(context_str.to_string())
    };

    let trace = Trace::new_with_identity(
        capability.clone(),
        outcome,
        latency_ms,
        input_size,
        context_hash,
        context_text,
        session_id,
        ctx.binding.owner_account.clone(),
        Some(ctx.binding.device_identity.clone()),
        model_id,
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );

    let trace_id_hex: String = trace.id[..8].iter().map(|b| format!("{b:02x}")).collect();

    match ctx.store.insert(&trace) {
        Ok(_) => json!({
            "recorded": true,
            "trace_id": trace_id_hex,
            "capability": capability,
        })
        .to_string(),
        Err(e) => json!({"error": format!("storage: {e}")}).to_string(),
    }
}

fn handle_post_signal(ctx: &HttpContext, body: &str) -> String {
    let args: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("invalid JSON: {e}")}).to_string(),
    };

    let kind = match args["kind"].as_str().and_then(SignalPostKind::parse) {
        Some(kind) => kind,
        None => return json!({"error": "missing or invalid field: kind"}).to_string(),
    };
    let context = match args["context"].as_str() {
        Some(context) => context,
        None => return json!({"error": "missing field: context"}).to_string(),
    };
    let message = match args["message"].as_str() {
        Some(message) => message,
        None => return json!({"error": "missing field: message"}).to_string(),
    };
    let model = args["model"].as_str().unwrap_or("unknown").to_string();
    let session_id = args["session_id"].as_str().map(str::to_string);
    let ttl_hours = args["ttl_hours"]
        .as_u64()
        .map(|value| value.min(u32::MAX as u64) as u32)
        .unwrap_or(DEFAULT_SIGNAL_TTL_HOURS);

    let trace = create_signal_trace(
        kind,
        context,
        message,
        SignalTraceConfig {
            model_id: model,
            session_id,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            ttl_hours,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );
    let trace_id_hex: String = trace.id[..8].iter().map(|b| format!("{b:02x}")).collect();

    match ctx.store.insert(&trace) {
        Ok(_) => json!({
            "posted": true,
            "kind": kind.as_str(),
            "message": message,
            "ttl_hours": ttl_hours,
            "trace_id": trace_id_hex,
        })
        .to_string(),
        Err(e) => json!({"error": format!("storage: {e}")}).to_string(),
    }
}

fn handle_get_query(ctx: &HttpContext, path: &str) -> String {
    let params = parse_query_params(path);
    let context_str = params.get("context").map(String::as_str).unwrap_or("");
    let intent = params
        .get("intent")
        .map(String::as_str)
        .unwrap_or("explore");
    let capability = params.get("capability").map(String::as_str).unwrap_or("");
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    match intent {
        "resolve" => {
            let context_hash = simhash(context_str);
            let traces = match ctx.store.query_similar(&context_hash, 48, limit * 10) {
                Ok(t) => t,
                Err(e) => return json!({"error": format!("query: {e}")}).to_string(),
            };

            let mut cap_groups: HashMap<&str, Vec<&Trace>> = HashMap::new();
            for trace in &traces {
                if is_signal_capability(&trace.capability) {
                    continue;
                }
                cap_groups.entry(&trace.capability).or_default().push(trace);
            }

            let mut capabilities: Vec<Value> = cap_groups
                .iter()
                .map(|(cap, group)| {
                    let total = group.len() as u64;
                    let successes = group
                        .iter()
                        .filter(|trace| matches!(trace.outcome, Outcome::Succeeded))
                        .count() as f64;
                    let success_rate = if total > 0 {
                        successes / total as f64
                    } else {
                        0.0
                    };
                    let best_sim = group
                        .iter()
                        .map(|trace| similarity(&context_hash, &trace.context_hash))
                        .fold(0.0_f64, f64::max);
                    let samples: Vec<&str> = group
                        .iter()
                        .filter_map(|trace| trace.context_text.as_deref())
                        .take(3)
                        .collect();

                    json!({
                        "capability": cap,
                        "context_similarity": round2(best_sim),
                        "success_rate": round2(success_rate),
                        "total_traces": total,
                        "context_samples": samples,
                    })
                })
                .collect();

            capabilities.sort_by(|a, b| {
                b["context_similarity"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&a["context_similarity"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            capabilities.truncate(limit);

            json!({"capabilities": capabilities}).to_string()
        }
        "evaluate" => {
            if capability.is_empty() {
                return json!({"error": "evaluate requires ?capability="}).to_string();
            }
            match ctx.store.aggregate(capability) {
                Ok(Some(stats)) => json!({
                    "capability": capability,
                    "stats": {
                        "total_traces": stats.total_traces,
                        "success_rate": round2(stats.success_rate),
                        "p50_latency_ms": stats.p50_latency_ms,
                        "p95_latency_ms": stats.p95_latency_ms,
                        "confidence": round2(stats.confidence),
                    }
                })
                .to_string(),
                Ok(None) => json!({"capability": capability, "stats": null}).to_string(),
                Err(e) => json!({"error": format!("query: {e}")}).to_string(),
            }
        }
        "signals" => handle_signals_query(ctx, &params),
        _ => handle_get_capabilities(ctx),
    }
}

fn handle_get_signals(ctx: &HttpContext, path: &str) -> String {
    let params = parse_query_params(path);
    handle_signals_query(ctx, &params)
}

fn handle_get_signal_feed(ctx: &HttpContext, path: &str) -> String {
    let params = parse_query_params(path);
    let hours: u32 = params
        .get("hours")
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let kind = match params.get("kind") {
        Some(value) => match SignalPostKind::parse(value) {
            Some(kind) => Some(kind),
            None => return json!({"error": format!("invalid signal kind: {value}")}).to_string(),
        },
        None => None,
    };
    let scope = match params.get("scope") {
        Some(value) => match SignalScopeFilter::parse(value) {
            Some(scope) => scope,
            None => return json!({"error": format!("invalid signal scope: {value}")}).to_string(),
        },
        None => SignalScopeFilter::All,
    };
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let traces = match ctx
        .store
        .query_recent_signal_traces(hours, kind, limit.max(1))
    {
        Ok(traces) => traces,
        Err(e) => return json!({"error": format!("query: {e}")}).to_string(),
    };
    let results = filter_signal_feed_results(
        summarize_recent_signal_feed(
            &traces,
            &ctx.binding.device_identity,
            ctx.identity.public_key_bytes(),
            limit,
        ),
        scope,
    );
    for trace in create_feed_reinforcement_traces(
        &results,
        SignalTraceConfig {
            model_id: "thronglets-feed".into(),
            session_id: None,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = ctx.store.insert(&trace);
    }
    json!({ "signals": results }).to_string()
}

fn handle_signals_query(ctx: &HttpContext, params: &HashMap<String, String>) -> String {
    let context_str = params.get("context").map(String::as_str).unwrap_or("");
    let kind = match params.get("kind") {
        Some(value) => match SignalPostKind::parse(value) {
            Some(kind) => Some(kind),
            None => return json!({"error": format!("invalid signal kind: {value}")}).to_string(),
        },
        None => None,
    };
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let context_hash = simhash(context_str);
    let traces = match ctx
        .store
        .query_signal_traces(&context_hash, kind, 48, limit.max(1))
    {
        Ok(traces) => traces,
        Err(e) => return json!({"error": format!("query: {e}")}).to_string(),
    };

    let results = summarize_signal_traces(
        &traces,
        context_str,
        &ctx.binding.device_identity,
        ctx.identity.public_key_bytes(),
        limit,
    );
    for trace in create_query_reinforcement_traces(
        &results,
        context_str,
        SignalTraceConfig {
            model_id: "thronglets-query".into(),
            session_id: None,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = ctx.store.insert(&trace);
    }

    json!({ "signals": results }).to_string()
}

fn handle_get_capabilities(ctx: &HttpContext) -> String {
    let caps = ctx.store.distinct_capabilities(100).unwrap_or_default();
    let mut result = Vec::new();
    for cap in &caps {
        if is_signal_capability(cap) {
            continue;
        }
        if let Ok(Some(stats)) = ctx.store.aggregate(cap) {
            result.push(json!({
                "capability": cap,
                "total_traces": stats.total_traces,
                "success_rate": round2(stats.success_rate),
                "p50_latency_ms": stats.p50_latency_ms,
                "confidence": round2(stats.confidence),
            }));
        }
    }
    json!({"capabilities": result}).to_string()
}

fn handle_get_status(ctx: &HttpContext) -> String {
    let trace_count = ctx.store.count().unwrap_or(0);
    let cap_count = ctx
        .store
        .distinct_capabilities(1000)
        .map(|caps| {
            caps.into_iter()
                .filter(|capability| !is_signal_capability(capability))
                .count()
        })
        .unwrap_or(0);

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "node_id": hex_encode(&ctx.identity.public_key_bytes()[..4]),
        "device_identity": ctx.binding.device_identity.clone(),
        "owner_account": ctx.binding.owner_account.clone(),
        "binding_source": ctx.binding.binding_source_or_local(),
        "joined_from_device": ctx.binding.joined_from_device.clone(),
        "trace_count": trace_count,
        "capabilities": cap_count,
    })
    .to_string()
}

fn parse_query_params(path: &str) -> HashMap<String, String> {
    let query_str = path.split('?').nth(1).unwrap_or("");
    query_str
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (percent_decode(key), percent_decode(value)))
        .collect()
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut idx = 0;

    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                decoded.push(b' ');
                idx += 1;
            }
            b'%' if idx + 2 < bytes.len() => {
                let hi = from_hex(bytes[idx + 1]);
                let lo = from_hex(bytes[idx + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    decoded.push((hi << 4) | lo);
                    idx += 3;
                } else {
                    decoded.push(bytes[idx]);
                    idx += 1;
                }
            }
            byte => {
                decoded.push(byte);
                idx += 1;
            }
        }
    }

    String::from_utf8_lossy(&decoded).into_owned()
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx() -> HttpContext {
        HttpContext {
            identity: Arc::new(NodeIdentity::generate()),
            binding: Arc::new(IdentityBinding::new("oasyce1localdevice".into())),
            store: Arc::new(TraceStore::in_memory().unwrap()),
        }
    }

    fn parse_body(raw_response: &str) -> Value {
        serde_json::from_str(raw_response).expect("response should be valid json")
    }

    #[test]
    fn signal_post_and_query_roundtrip_decodes_context() {
        let ctx = make_ctx();

        let post_request = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"avoid\",\"context\":\"fix flaky ci workflow\",\"message\":\"skip the generated lockfile\",\"model\":\"codex\"}",
        );
        let post_response = parse_body(&handle_http_request(&ctx, post_request));
        assert_eq!(post_response["posted"], true);
        assert_eq!(post_response["ttl_hours"], DEFAULT_SIGNAL_TTL_HOURS);

        let get_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/signals?context=fix%20flaky%20ci%20workflow&kind=avoid&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = get_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "avoid");
        assert_eq!(signals[0]["message"], "skip the generated lockfile");
        assert_eq!(signals[0]["local_source_count"], 1);
        assert_eq!(signals[0]["collective_source_count"], 0);
        assert_eq!(signals[0]["evidence_scope"], "local");

        let query_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/query?context=fix%20flaky%20ci%20workflow&intent=signals&kind=avoid&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = query_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "avoid");
        assert_eq!(signals[0]["evidence_scope"], "local");
    }

    #[test]
    fn resolve_capabilities_and_status_ignore_signal_capabilities() {
        let ctx = make_ctx();

        let post_signal = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"info\",\"context\":\"inspect src/main.rs\",\"message\":\"main.rs is noisy\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, post_signal);

        let post_trace = concat!(
            "POST /v1/traces HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"capability\":\"claude-code/Edit\",\"outcome\":\"succeeded\",\"latency_ms\":12,\"input_size\":34,\"context\":\"inspect src/main.rs\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, post_trace);

        let caps_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/capabilities HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let caps = caps_response["capabilities"].as_array().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0]["capability"], "claude-code/Edit");

        let resolve_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/query?context=inspect%20src%2Fmain.rs&intent=resolve&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let resolved = resolve_response["capabilities"].as_array().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0]["capability"], "claude-code/Edit");

        let status_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/status HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert_eq!(status_response["trace_count"], 2);
        assert_eq!(status_response["capabilities"], 1);
    }

    #[test]
    fn signal_feed_returns_recent_signals() {
        let ctx = make_ctx();

        let post_signal = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"recommend\",\"context\":\"repair release flow\",\"message\":\"run release-check before push\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, post_signal);

        let feed_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/signals/feed?hours=24&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = feed_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "recommend");
        assert_eq!(signals[0]["message"], "run release-check before push");
        assert_eq!(signals[0]["model_count"], 1);
        assert_eq!(signals[0]["corroboration_tier"], "single_source");
        assert_eq!(signals[0]["focus_tier"], "background");
        assert_eq!(signals[0]["evidence_scope"], "local");
    }

    #[test]
    fn signal_feed_filters_by_kind_and_scope() {
        let ctx = make_ctx();

        let recommend_signal = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"recommend\",\"context\":\"repair release flow\",\"message\":\"run release-check before push\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, recommend_signal);

        let avoid_signal = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"avoid\",\"context\":\"repair release flow\",\"message\":\"skip the generated lockfile\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, avoid_signal);

        let feed_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/signals/feed?hours=24&kind=recommend&scope=local&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = feed_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "recommend");
        assert_eq!(signals[0]["model_count"], 1);
        assert_eq!(signals[0]["corroboration_tier"], "single_source");
        assert_eq!(signals[0]["focus_tier"], "background");
        assert_eq!(signals[0]["evidence_scope"], "local");
    }
}
