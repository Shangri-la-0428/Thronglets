//! Lightweight HTTP API for non-MCP agents.
//!
//! Exposes the same trace/query capabilities as MCP, but over HTTP/JSON.
//! This opens Thronglets to Python/LangChain/AutoGen and any HTTP-capable agent.
//!
//! Endpoints:
//! - POST /v1/traces       — record a trace
//! - POST /v1/signals      — leave an explicit short signal
//! - POST /v1/presence     — leave a lightweight session presence heartbeat
//! - POST /v1/ambient-priors — project runtime-only ambient priors
//! - GET  /v1/signals      — query explicit short signals
//! - GET  /v1/signals/feed — show recent converging explicit signals
//! - GET  /v1/presence/feed — show recent active sessions in a space
//! - GET  /v1/query        — query the substrate
//! - GET  /v1/capabilities — list known capabilities
//! - GET  /v1/status       — node status
//! - GET  /v1/authorization — local authorization snapshot

use crate::ambient::{AmbientPriorRequest, ambient_prior_data};
use crate::continuity::ExternalContinuityInput;
use crate::identity::{IdentityBinding, NodeIdentity};
use crate::identity_surface::{authorization_check_data, identity_summary};
use crate::posts::{SignalPostKind, is_signal_capability};
use crate::presence::is_presence_capability;
use crate::service;
use crate::storage::TraceStore;
use crate::trace::MethodCompliance;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info};

pub struct HttpContext {
    pub identity: Arc<NodeIdentity>,
    pub binding: Arc<IdentityBinding>,
    pub store: Arc<TraceStore>,
    pub data_dir: PathBuf,
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
        ("POST", "/v1/presence") => handle_post_presence(ctx, body),
        ("POST", "/v1/ambient-priors") => handle_post_ambient_priors(ctx, body),
        ("GET", "/v1/signals/feed") => handle_get_signal_feed(ctx, path),
        ("GET", "/v1/signals") => handle_get_signals(ctx, path),
        ("GET", "/v1/presence/feed") => handle_get_presence_feed(ctx, path),
        ("GET", "/v1/query") => handle_get_query(ctx, path),
        ("GET", "/v1/capabilities") => handle_get_capabilities(ctx),
        ("GET", "/v1/field/clusters") => handle_get_field_clusters(ctx, path),
        ("GET", "/v1/status") => handle_get_status(ctx),
        ("GET", "/v1/authorization") => handle_get_authorization(ctx),
        _ => json!({"error": "not found", "endpoints": [
            "POST /v1/traces",
            "POST /v1/signals",
            "POST /v1/presence",
            "POST /v1/ambient-priors",
            "GET /v1/signals?context=...&kind=avoid|recommend|watch|info&space=...&limit=5",
            "GET /v1/signals/feed?hours=24&kind=avoid|recommend|watch|info&min_sources=0&space=...&limit=10",
            "GET /v1/presence/feed?hours=1&space=...&limit=10",
            "GET /v1/query?context=...&intent=resolve|evaluate|explore|signals",
            "GET /v1/field/clusters?min_weight=0.05",
            "GET /v1/capabilities",
            "GET /v1/status",
            "GET /v1/authorization"
        ]})
        .to_string(),
    }
}

fn handle_post_trace(ctx: &HttpContext, body: &str) -> String {
    let args: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("invalid JSON: {e}")}).to_string(),
    };
    let external_continuity: Option<ExternalContinuityInput> = args
        .get("external_continuity")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let capability = args["capability"].as_str().unwrap_or("").to_string();
    if capability.is_empty() && external_continuity.is_none() {
        return json!({"error": "missing field: capability"}).to_string();
    }
    let evidence = match service::parse_evidence_from_json(&args) {
        Ok(value) => value,
        Err(error) => return json!({"error": error}).to_string(),
    };

    let req = service::RecordTraceReq {
        capability,
        outcome: service::parse_outcome(args["outcome"].as_str().unwrap_or("succeeded")),
        latency_ms: args["latency_ms"].as_u64().unwrap_or(0) as u32,
        input_size: args["input_size"].as_u64().unwrap_or(0) as u32,
        context: args["context"].as_str().unwrap_or("").to_string(),
        model: args["model"].as_str().unwrap_or("unknown").to_string(),
        session_id: args["session_id"].as_str().map(String::from),
        space: args["space"].as_str().map(String::from),
        agent_id: args["agent_id"].as_str().map(String::from),
        sigil_id: args["sigil_id"].as_str().map(String::from),
        method_compliance: args["method_compliance"]
            .as_str()
            .and_then(MethodCompliance::parse),
        evidence,
    };

    match service::record_trace(&svc_ctx(ctx), req, external_continuity) {
        Ok(service::RecordResult::Trace(out)) => json!({
            "recorded": true, "trace_id": out.trace_id, "capability": out.capability,
        })
        .to_string(),
        Ok(service::RecordResult::Continuity(out)) => json!({
            "recorded": true, "trace_id": out.trace_id, "capability": out.capability,
            "external_continuity": out.external_continuity,
        })
        .to_string(),
        Err(e) => json!({"error": e}).to_string(),
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
        Some(v) => v,
        None => return json!({"error": "missing field: context"}).to_string(),
    };
    let message = match args["message"].as_str() {
        Some(v) => v,
        None => return json!({"error": "missing field: message"}).to_string(),
    };

    let req = service::PostSignalReq {
        kind,
        context: context.to_string(),
        message: message.to_string(),
        tool_name: None,
        space: args["space"].as_str().map(str::to_string),
        model: args["model"].as_str().unwrap_or("unknown").to_string(),
        session_id: args["session_id"].as_str().map(str::to_string),
        agent_id: args["agent_id"].as_str().map(str::to_string),
        sigil_id: args["sigil_id"].as_str().map(str::to_string),
        ttl_hours: args["ttl_hours"]
            .as_u64()
            .map(|v| v.min(u32::MAX as u64) as u32),
    };

    match service::post_signal(&svc_ctx(ctx), req) {
        Ok(out) => serde_json::to_string(&out).unwrap(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_post_presence(ctx: &HttpContext, body: &str) -> String {
    let args: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("invalid JSON: {e}")}).to_string(),
    };

    let req = service::PingPresenceReq {
        space: args["space"].as_str().map(str::to_string),
        mode: args["mode"].as_str().map(str::to_string),
        model: args["model"].as_str().unwrap_or("unknown").to_string(),
        session_id: args["session_id"].as_str().map(str::to_string),
        sigil_id: args["sigil_id"].as_str().map(str::to_string),
        capability: args["capability"].as_str().map(str::to_string),
        ttl_minutes: args["ttl_minutes"]
            .as_u64()
            .map(|v| v.min(u32::MAX as u64) as u32),
    };

    match service::ping_presence(&svc_ctx(ctx), req) {
        Ok(out) => serde_json::to_string(&out).unwrap(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_post_ambient_priors(ctx: &HttpContext, body: &str) -> String {
    let request: AmbientPriorRequest = match serde_json::from_str(body) {
        Ok(request) => request,
        Err(error) => return json!({"error": format!("invalid JSON: {error}")}).to_string(),
    };

    json!(ambient_prior_data(&ctx.store, &request)).to_string()
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

    let svc = svc_ctx(ctx);
    match intent {
        "resolve" => match service::resolve(&svc, context_str, limit) {
            Ok(data) => data.to_string(),
            Err(e) => json!({"error": e}).to_string(),
        },
        "evaluate" => {
            if capability.is_empty() {
                return json!({"error": "evaluate requires ?capability="}).to_string();
            }
            match service::evaluate(&svc, capability, limit) {
                Ok(data) => data.to_string(),
                Err(e) => json!({"error": e}).to_string(),
            }
        }
        "signals" => handle_signals_query(ctx, &params),
        _ => match service::explore(&svc, context_str, limit) {
            Ok(data) => data.to_string(),
            Err(e) => json!({"error": e}).to_string(),
        },
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
    let min_sources: u32 = params
        .get("min_sources")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let space = params.get("space").map(String::as_str);

    let req = service::SignalFeedReq {
        hours,
        kind,
        min_sources,
        limit,
        space,
    };
    match service::signal_feed(&svc_ctx(ctx), req) {
        Ok(data) => data.to_string(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_get_presence_feed(ctx: &HttpContext, path: &str) -> String {
    let params = parse_query_params(path);
    let hours: u32 = params
        .get("hours")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let limit: usize = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let space = params.get("space").map(String::as_str);

    match service::presence_feed(&svc_ctx(ctx), hours, limit, space) {
        Ok(data) => data.to_string(),
        Err(e) => json!({"error": e}).to_string(),
    }
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
    let space = params.get("space").map(String::as_str);

    let req = service::QuerySignalsReq {
        context: context_str,
        kind,
        limit,
        space,
    };
    match service::query_signals(&svc_ctx(ctx), req) {
        Ok(data) => data.to_string(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_get_capabilities(ctx: &HttpContext) -> String {
    match service::explore(&svc_ctx(ctx), "", 100) {
        Ok(data) => data.to_string(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_get_field_clusters(ctx: &HttpContext, path: &str) -> String {
    let params = parse_query_params(path);
    let min_weight = params
        .get("min_weight")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    match service::field_clusters(&svc_ctx(ctx), min_weight) {
        Ok(data) => data.to_string(),
        Err(e) => json!({"error": e}).to_string(),
    }
}

fn handle_get_status(ctx: &HttpContext) -> String {
    let trace_count = ctx.store.count().unwrap_or(0);
    let attributed_count = ctx.store.count_attributed().unwrap_or(0);
    let workspace = crate::workspace::WorkspaceState::load(&ctx.data_dir);
    let network = crate::network_state::NetworkSnapshot::load(&ctx.data_dir).to_status();
    let cap_count = ctx
        .store
        .distinct_capabilities(1000)
        .map(|caps| {
            caps.into_iter()
                .filter(|capability| {
                    !is_signal_capability(capability) && !is_presence_capability(capability)
                })
                .count()
        })
        .unwrap_or(0);

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "node_id": hex_encode(&ctx.identity.public_key_bytes()[..4]),
        "identity": identity_summary("healthy", ctx.binding.as_ref()),
        "device_identity": ctx.binding.device_identity.clone(),
        "owner_account": ctx.binding.owner_account.clone(),
        "binding_source": ctx.binding.binding_source_or_local(),
        "joined_from_device": ctx.binding.joined_from_device.clone(),
        "substrate": workspace.substrate_activity(),
        "network": network,
        "trace_count": trace_count,
        "attributed_traces": attributed_count,
        "capabilities": cap_count,
    })
    .to_string()
}

fn handle_get_authorization(ctx: &HttpContext) -> String {
    json!(authorization_check_data(ctx.binding.as_ref())).to_string()
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

fn svc_ctx(ctx: &HttpContext) -> service::Ctx<'_> {
    service::Ctx {
        store: &ctx.store,
        field: None,
        identity: &ctx.identity,
        binding: &ctx.binding,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::is_continuity_capability;

    fn make_ctx() -> HttpContext {
        HttpContext {
            identity: Arc::new(NodeIdentity::generate()),
            binding: Arc::new(IdentityBinding::new("oasyce1localdevice".into())),
            store: Arc::new(TraceStore::in_memory().unwrap()),
            data_dir: std::env::temp_dir(),
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
        assert_eq!(
            post_response["ttl_hours"],
            SignalPostKind::Avoid.default_ttl_hours()
        );
        assert_eq!(post_response["ttl_source"], "kind_default");

        let get_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/signals?context=fix%20flaky%20ci%20workflow&kind=avoid&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = get_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "avoid");
        assert_eq!(signals[0]["message"], "skip the generated lockfile");

        let query_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/query?context=fix%20flaky%20ci%20workflow&intent=signals&kind=avoid&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let signals = query_response["signals"].as_array().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0]["kind"], "avoid");
    }

    #[test]
    fn signal_post_uses_kind_specific_default_ttl_and_explicit_override() {
        let ctx = make_ctx();

        let recommend_request = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"recommend\",\"context\":\"repair release flow\",\"message\":\"run release-check before push\"}",
        );
        let recommend_response = parse_body(&handle_http_request(&ctx, recommend_request));
        assert_eq!(
            recommend_response["ttl_hours"],
            SignalPostKind::Recommend.default_ttl_hours()
        );
        assert_eq!(recommend_response["ttl_source"], "kind_default");

        let override_request = concat!(
            "POST /v1/signals HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"kind\":\"watch\",\"context\":\"monitor noisy benchmark\",\"message\":\"compare against yesterday\",\"ttl_hours\":5}",
        );
        let override_response = parse_body(&handle_http_request(&ctx, override_request));
        assert_eq!(override_response["ttl_hours"], 5);
        assert_eq!(override_response["ttl_source"], "explicit");
    }

    #[test]
    fn ambient_priors_returns_runtime_projection() {
        let ctx = make_ctx();

        let post_trace = concat!(
            "POST /v1/traces HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"capability\":\"claude-code/Bash\",\"outcome\":\"failed\",\"latency_ms\":12,\"input_size\":34,\"context\":\"restart thronglets service after ssh timeout\",\"model\":\"codex\"}",
        );
        let _ = handle_http_request(&ctx, post_trace);

        let response = parse_body(&handle_http_request(
            &ctx,
            concat!(
                "POST /v1/ambient-priors HTTP/1.1\r\n",
                "Host: localhost\r\n",
                "Content-Type: application/json\r\n",
                "\r\n",
                "{\"text\":\"restart thronglets service after ssh timeout\",\"goal\":\"repair\",\"limit\":3}"
            ),
        ));
        assert_eq!(response["summary"]["status"], "ready");
        assert_eq!(response["summary"]["goal"], "repair");
        let priors = response["priors"].as_array().unwrap();
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
        assert_eq!(status_response["substrate"]["activity"], "quiet");
        assert_eq!(status_response["substrate"]["recent_interventions_15m"], 0);
        assert_eq!(
            status_response["identity"]["authorization"]["final_truth_source"],
            "oasyce_chain"
        );
        assert_eq!(
            status_response["identity"]["authorization"]["authoritative_status"],
            "not-checked"
        );

        let authorization_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/authorization HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert_eq!(
            authorization_response["summary"]["final_truth_source"],
            "oasyce_chain"
        );
        assert_eq!(
            authorization_response["summary"]["execution_boundary"],
            "device_identity"
        );
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
    }

    #[test]
    fn presence_post_and_feed_roundtrip() {
        let ctx = make_ctx();

        let post_presence = concat!(
            "POST /v1/presence HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"space\":\"psyche\",\"mode\":\"focus\",\"model\":\"codex\",\"session_id\":\"codex-1\"}",
        );
        let post_response = parse_body(&handle_http_request(&ctx, post_presence));
        assert_eq!(post_response["active"], true);
        assert_eq!(post_response["space"], "psyche");
        assert_eq!(post_response["mode"], "focus");

        let feed_response = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/presence/feed?space=psyche&hours=1&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        let sessions = feed_response["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["space"], "psyche");
        assert_eq!(sessions[0]["mode"], "focus");
        assert_eq!(sessions[0]["session_id"], "codex-1");
    }

    #[test]
    fn external_continuity_trace_stays_local_and_can_derive_sparse_signal() {
        let ctx = make_ctx();

        let first = concat!(
            "POST /v1/traces HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"outcome\":\"failed\",\"model\":\"psyche\",\"session_id\":\"s1\",\"external_continuity\":{\"provider\":\"thronglets\",\"mode\":\"optional\",\"version\":1,\"taxonomy\":\"calibration\",\"event\":\"writeback-calibration\",\"summary\":\"writeback drift keeps reopening the same loop\",\"space\":\"psyche\"}}",
        );
        let first_response = parse_body(&handle_http_request(&ctx, first));
        assert_eq!(first_response["recorded"], true);
        assert_eq!(
            first_response["external_continuity"]["local_only_raw"],
            true
        );
        assert_eq!(
            first_response["external_continuity"]["runtime"]["state"],
            "local-only"
        );
        assert!(first_response["external_continuity"]["derived_signal"].is_null());

        let second = concat!(
            "POST /v1/traces HTTP/1.1\r\n",
            "Host: localhost\r\n",
            "Content-Type: application/json\r\n",
            "\r\n",
            "{\"outcome\":\"failed\",\"model\":\"psyche\",\"session_id\":\"s2\",\"external_continuity\":{\"provider\":\"thronglets\",\"mode\":\"optional\",\"version\":1,\"taxonomy\":\"calibration\",\"event\":\"writeback-calibration\",\"summary\":\"writeback drift keeps reopening the same loop\",\"space\":\"psyche\"}}",
        );
        let second_response = parse_body(&handle_http_request(&ctx, second));
        assert_eq!(
            second_response["external_continuity"]["derived_signal"]["kind"],
            "avoid"
        );
        assert_eq!(
            second_response["external_continuity"]["runtime"]["state"],
            "summary-candidate"
        );
        assert_eq!(
            second_response["external_continuity"]["runtime"]["derived_signal_rule"],
            "writeback-calibration.repeated-failure-avoid"
        );
        assert_eq!(
            second_response["external_continuity"]["runtime"]["summary_candidate_rule"],
            "writeback-calibration.repeated-failure-summary"
        );

        let signals = parse_body(&handle_http_request(
            &ctx,
            "GET /v1/signals?context=writeback%20drift%20keeps%20reopening%20the%20same%20loop&kind=avoid&space=psyche&limit=5 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        ));
        assert_eq!(signals["signals"].as_array().unwrap().len(), 1);

        let unpublished = ctx.store.unpublished_traces(10).unwrap();
        assert!(
            unpublished
                .iter()
                .all(|trace| !is_continuity_capability(&trace.capability))
        );
    }
}
