use crate::context::{simhash, similarity};
use crate::trace::{Outcome, Trace};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SIGNAL_CAPABILITY_PREFIX: &str = "urn:thronglets:signal:";
pub const DEFAULT_SIGNAL_TTL_HOURS: u32 = 72;

#[derive(Debug, Clone)]
pub struct SignalTraceConfig {
    pub model_id: String,
    pub session_id: Option<String>,
    pub ttl_hours: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalPostKind {
    Recommend,
    Avoid,
    Watch,
    Info,
}

impl SignalPostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recommend => "recommend",
            Self::Avoid => "avoid",
            Self::Watch => "watch",
            Self::Info => "info",
        }
    }

    pub fn capability(self) -> String {
        format!("{SIGNAL_CAPABILITY_PREFIX}{}", self.as_str())
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "recommend" => Some(Self::Recommend),
            "avoid" => Some(Self::Avoid),
            "watch" => Some(Self::Watch),
            "info" => Some(Self::Info),
            _ => None,
        }
    }

    pub fn from_capability(capability: &str) -> Option<Self> {
        capability
            .strip_prefix(SIGNAL_CAPABILITY_PREFIX)
            .and_then(Self::parse)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SignalTracePayload {
    context: String,
    message: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalQueryResult {
    pub kind: String,
    pub message: String,
    pub context_similarity: f64,
    pub total_posts: u64,
    pub source_count: u32,
    pub latest_timestamp: u64,
    pub expires_at: u64,
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone)]
struct DecodedSignalTrace {
    kind: SignalPostKind,
    context: String,
    message: String,
    expires_at: u64,
}

#[derive(Debug)]
struct SignalGroup {
    kind: SignalPostKind,
    message: String,
    best_similarity: f64,
    total_posts: u64,
    latest_timestamp: u64,
    expires_at: u64,
    contexts: BTreeSet<String>,
    sources: BTreeSet<String>,
}

pub fn is_signal_capability(capability: &str) -> bool {
    capability.starts_with(SIGNAL_CAPABILITY_PREFIX)
}

pub fn create_signal_trace(
    kind: SignalPostKind,
    context: &str,
    message: &str,
    config: SignalTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    create_signal_trace_at(
        kind,
        context,
        message,
        config,
        now_ms(),
        node_pubkey,
        sign_fn,
    )
}

fn create_signal_trace_at(
    kind: SignalPostKind,
    context: &str,
    message: &str,
    config: SignalTraceConfig,
    now_ms: u64,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    let payload = SignalTracePayload {
        context: context.to_string(),
        message: message.to_string(),
        expires_at: expires_at_ms(now_ms, config.ttl_hours),
    };

    Trace::new(
        kind.capability(),
        Outcome::Succeeded,
        0,
        message.len().min(u32::MAX as usize) as u32,
        simhash(context),
        Some(serde_json::to_string(&payload).expect("signal payload should serialize")),
        config.session_id,
        config.model_id,
        node_pubkey,
        sign_fn,
    )
}

pub fn summarize_signal_traces(
    traces: &[Trace],
    query_context: &str,
    limit: usize,
) -> Vec<SignalQueryResult> {
    let query_hash = simhash(query_context);
    let now_ms = now_ms();
    let mut groups: HashMap<(SignalPostKind, String), SignalGroup> = HashMap::new();

    for trace in traces {
        let Some(decoded) = decode_signal_trace(trace) else {
            continue;
        };
        if decoded.expires_at <= now_ms {
            continue;
        }

        let similarity_score = similarity(&query_hash, &trace.context_hash);
        let key = (decoded.kind, decoded.message.clone());
        let entry = groups.entry(key).or_insert_with(|| SignalGroup {
            kind: decoded.kind,
            message: decoded.message.clone(),
            best_similarity: similarity_score,
            total_posts: 0,
            latest_timestamp: trace.timestamp,
            expires_at: decoded.expires_at,
            contexts: BTreeSet::new(),
            sources: BTreeSet::new(),
        });
        entry.best_similarity = entry.best_similarity.max(similarity_score);
        entry.total_posts += 1;
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
        entry.expires_at = entry.expires_at.max(decoded.expires_at);
        if !decoded.context.is_empty() {
            entry.contexts.insert(decoded.context);
        }
        entry.sources.insert(source_key(trace));
    }

    let mut results: Vec<_> = groups
        .into_values()
        .map(|group| SignalQueryResult {
            kind: group.kind.as_str().to_string(),
            message: group.message,
            context_similarity: round2(group.best_similarity),
            total_posts: group.total_posts,
            source_count: group.sources.len() as u32,
            latest_timestamp: group.latest_timestamp,
            expires_at: group.expires_at,
            contexts: group.contexts.into_iter().take(3).collect(),
        })
        .collect();

    results.sort_by(|a, b| {
        b.context_similarity
            .partial_cmp(&a.context_similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.source_count.cmp(&a.source_count))
            .then_with(|| b.total_posts.cmp(&a.total_posts))
            .then_with(|| b.latest_timestamp.cmp(&a.latest_timestamp))
    });
    results.truncate(limit);
    results
}

fn decode_signal_trace(trace: &Trace) -> Option<DecodedSignalTrace> {
    let kind = SignalPostKind::from_capability(&trace.capability)?;
    let payload: SignalTracePayload = serde_json::from_str(trace.context_text.as_deref()?).ok()?;
    Some(DecodedSignalTrace {
        kind,
        context: payload.context,
        message: payload.message,
        expires_at: payload.expires_at,
    })
}

pub fn expires_at_ms(now_ms: u64, ttl_hours: u32) -> u64 {
    now_ms.saturating_add((ttl_hours as u64).saturating_mul(60 * 60 * 1000))
}

fn source_key(trace: &Trace) -> String {
    let node = trace
        .node_pubkey
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    match trace.session_id.as_deref() {
        Some(session_id) => format!("{node}:{session_id}"),
        None => node,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    #[test]
    fn summarize_signal_posts_groups_by_kind_and_message() {
        let identity = NodeIdentity::generate();
        let trace_a = create_signal_trace(
            SignalPostKind::Avoid,
            "fix flaky ci workflow",
            "skip the generated lockfile",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("session-a".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let trace_b = create_signal_trace(
            SignalPostKind::Avoid,
            "repair flaky ci pipeline",
            "skip the generated lockfile",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("session-b".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        let trace_b_timestamp = trace_b.timestamp;

        let results = summarize_signal_traces(&[trace_a, trace_b], "fix flaky ci workflow", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "avoid");
        assert_eq!(results[0].message, "skip the generated lockfile");
        assert_eq!(results[0].total_posts, 2);
        assert_eq!(results[0].source_count, 2);
        assert!(results[0].expires_at >= trace_b_timestamp);
    }

    #[test]
    fn summarize_signal_posts_ignores_expired_entries() {
        let identity = NodeIdentity::generate();
        let base_now = now_ms();
        let expired = create_signal_trace_at(
            SignalPostKind::Watch,
            "ship the current branch",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("session-a".into()),
                ttl_hours: 1,
            },
            base_now.saturating_sub(3 * 60 * 60 * 1000),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        let fresh = create_signal_trace_at(
            SignalPostKind::Watch,
            "ship the current branch",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("session-b".into()),
                ttl_hours: 24,
            },
            base_now,
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );

        let results = summarize_signal_traces(&[expired, fresh], "ship the current branch", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "watch");
        assert_eq!(results[0].total_posts, 1);
    }
}
