use crate::context::simhash;
use crate::trace::{Outcome, Trace};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PRESENCE_CAPABILITY_PREFIX: &str = "urn:thronglets:presence:";
pub const PRESENCE_HEARTBEAT_CAPABILITY: &str = "urn:thronglets:presence:heartbeat";
pub const DEFAULT_PRESENCE_TTL_MINUTES: u32 = 30;

#[derive(Debug, Clone)]
pub struct PresenceTraceConfig {
    pub model_id: String,
    pub session_id: Option<String>,
    pub owner_account: Option<String>,
    pub device_identity: Option<String>,
    pub space: Option<String>,
    pub mode: Option<String>,
    pub ttl_minutes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PresenceTracePayload {
    space: Option<String>,
    mode: Option<String>,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceFeedResult {
    pub space: Option<String>,
    pub mode: Option<String>,
    pub model_id: String,
    pub session_id: Option<String>,
    pub device_identity: Option<String>,
    pub evidence_scope: String,
    pub latest_timestamp: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone)]
struct DecodedPresenceTrace {
    space: Option<String>,
    mode: Option<String>,
    expires_at: u64,
}

#[derive(Debug, Clone)]
struct PresenceGroup {
    space: Option<String>,
    mode: Option<String>,
    model_id: String,
    session_id: Option<String>,
    device_identity: Option<String>,
    evidence_scope: String,
    latest_timestamp: u64,
    expires_at: u64,
}

pub fn is_presence_capability(capability: &str) -> bool {
    capability.starts_with(PRESENCE_CAPABILITY_PREFIX)
}

pub fn create_presence_trace(
    config: PresenceTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    let now_ms = now_ms();
    let payload = PresenceTracePayload {
        space: config.space.clone(),
        mode: config.mode.clone(),
        expires_at: expires_at_ms(now_ms, config.ttl_minutes),
    };
    let context = format!(
        "presence:{}:{}",
        config.space.as_deref().unwrap_or("global"),
        config.mode.as_deref().unwrap_or("active")
    );
    let mut trace = Trace::new_with_identity(
        PRESENCE_HEARTBEAT_CAPABILITY.to_string(),
        Outcome::Succeeded,
        0,
        0,
        simhash(&context),
        Some(serde_json::to_string(&payload).expect("presence payload should serialize")),
        config.session_id,
        config.owner_account,
        config.device_identity,
        config.model_id,
        node_pubkey,
        sign_fn,
    );
    trace.timestamp = now_ms;
    trace
}

pub fn summarize_recent_presence(
    traces: &[Trace],
    space: Option<&str>,
    local_device_identity: &str,
    local_node_pubkey: [u8; 32],
    limit: usize,
) -> Vec<PresenceFeedResult> {
    let now_ms = now_ms();
    let mut groups: HashMap<(String, String, Option<String>, Option<String>), PresenceGroup> =
        HashMap::new();

    for trace in traces {
        let Some(decoded) = decode_presence_trace(trace) else {
            continue;
        };
        if decoded.expires_at <= now_ms {
            continue;
        }
        if !matches_presence_space(decoded.space.as_deref(), space) {
            continue;
        }

        let source = source_key(trace);
        let key = (
            source,
            trace.model_id.clone(),
            trace.session_id.clone(),
            decoded.space.clone(),
        );
        let evidence_scope = if is_local_source(trace, local_device_identity, &local_node_pubkey) {
            "local"
        } else {
            "collective"
        };
        let entry = groups.entry(key).or_insert_with(|| PresenceGroup {
            space: decoded.space.clone(),
            mode: decoded.mode.clone(),
            model_id: trace.model_id.clone(),
            session_id: trace.session_id.clone(),
            device_identity: trace.device_identity.clone(),
            evidence_scope: evidence_scope.to_string(),
            latest_timestamp: trace.timestamp,
            expires_at: decoded.expires_at,
        });
        if trace.timestamp >= entry.latest_timestamp {
            entry.mode = decoded.mode;
            entry.latest_timestamp = trace.timestamp;
            entry.expires_at = decoded.expires_at.max(entry.expires_at);
        }
    }

    let mut results: Vec<_> = groups
        .into_values()
        .map(|group| PresenceFeedResult {
            space: group.space,
            mode: group.mode,
            model_id: group.model_id,
            session_id: group.session_id,
            device_identity: group.device_identity,
            evidence_scope: group.evidence_scope,
            latest_timestamp: group.latest_timestamp,
            expires_at: group.expires_at,
        })
        .collect();

    results.sort_by(|a, b| {
        b.latest_timestamp
            .cmp(&a.latest_timestamp)
            .then_with(|| a.evidence_scope.cmp(&b.evidence_scope))
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    results.truncate(limit);
    results
}

fn decode_presence_trace(trace: &Trace) -> Option<DecodedPresenceTrace> {
    if !is_presence_capability(&trace.capability) {
        return None;
    }
    let payload = trace.context_text.as_ref()?;
    let decoded: PresenceTracePayload = serde_json::from_str(payload).ok()?;
    Some(DecodedPresenceTrace {
        space: decoded.space,
        mode: decoded.mode,
        expires_at: decoded.expires_at,
    })
}

fn matches_presence_space(result_space: Option<&str>, filter_space: Option<&str>) -> bool {
    match filter_space {
        Some(filter) => result_space == Some(filter),
        None => true,
    }
}

fn source_key(trace: &Trace) -> String {
    trace
        .device_identity
        .clone()
        .unwrap_or_else(|| hex_encode(&trace.node_pubkey))
}

fn is_local_source(
    trace: &Trace,
    local_device_identity: &str,
    local_node_pubkey: &[u8; 32],
) -> bool {
    trace.device_identity.as_deref() == Some(local_device_identity)
        || &trace.node_pubkey == local_node_pubkey
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn expires_at_ms(now_ms: u64, ttl_minutes: u32) -> u64 {
    now_ms.saturating_add(ttl_minutes as u64 * 60 * 1000)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_trace(
        space: Option<&str>,
        mode: Option<&str>,
        model_id: &str,
        session_id: Option<&str>,
        device_identity: &str,
    ) -> Trace {
        create_presence_trace(
            PresenceTraceConfig {
                model_id: model_id.into(),
                session_id: session_id.map(str::to_string),
                owner_account: None,
                device_identity: Some(device_identity.into()),
                space: space.map(str::to_string),
                mode: mode.map(str::to_string),
                ttl_minutes: DEFAULT_PRESENCE_TTL_MINUTES,
            },
            [7u8; 32],
            |_| Signature::from_bytes(&[0u8; 64]),
        )
    }

    #[test]
    fn summarize_recent_presence_keeps_latest_mode_per_session() {
        let first = local_trace(Some("psyche"), Some("focus"), "codex", Some("s1"), "dev1");
        let mut second =
            local_trace(Some("psyche"), Some("review"), "codex", Some("s1"), "dev1");
        second.timestamp = first.timestamp + 10;
        let results = summarize_recent_presence(&[first, second], Some("psyche"), "dev1", [7u8; 32], 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].mode.as_deref(), Some("review"));
        assert_eq!(results[0].evidence_scope, "local");
    }
}
