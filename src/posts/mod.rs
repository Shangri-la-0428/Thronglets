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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalScopeFilter {
    All,
    Local,
    Collective,
    Mixed,
}

impl SignalScopeFilter {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "local" => Some(Self::Local),
            "collective" => Some(Self::Collective),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }

    pub fn matches(self, evidence_scope: &str) -> bool {
        match self {
            Self::All => true,
            Self::Local => evidence_scope == "local",
            Self::Collective => evidence_scope == "collective",
            Self::Mixed => evidence_scope == "mixed",
        }
    }
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
    pub model_count: u32,
    pub corroboration_tier: String,
    pub local_source_count: u32,
    pub collective_source_count: u32,
    pub evidence_scope: String,
    pub latest_timestamp: u64,
    pub expires_at: u64,
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalFeedResult {
    pub kind: String,
    pub message: String,
    pub total_posts: u64,
    pub source_count: u32,
    pub model_count: u32,
    pub corroboration_tier: String,
    pub local_source_count: u32,
    pub collective_source_count: u32,
    pub evidence_scope: String,
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
    models: BTreeSet<String>,
    local_sources: BTreeSet<String>,
    collective_sources: BTreeSet<String>,
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
    local_node_pubkey: [u8; 32],
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
            models: BTreeSet::new(),
            local_sources: BTreeSet::new(),
            collective_sources: BTreeSet::new(),
        });
        entry.best_similarity = entry.best_similarity.max(similarity_score);
        entry.total_posts += 1;
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
        entry.expires_at = entry.expires_at.max(decoded.expires_at);
        if !decoded.context.is_empty() {
            entry.contexts.insert(decoded.context);
        }
        let source = source_key(trace);
        entry.sources.insert(source.clone());
        entry.models.insert(trace.model_id.clone());
        if trace.node_pubkey == local_node_pubkey {
            entry.local_sources.insert(source);
        } else {
            entry.collective_sources.insert(source);
        }
    }

    let mut results: Vec<_> = groups
        .into_values()
        .map(|group| {
            let local_source_count = group.local_sources.len() as u32;
            let collective_source_count = group.collective_sources.len() as u32;
            let evidence_scope =
                signal_evidence_scope(local_source_count, collective_source_count).to_string();
            SignalQueryResult {
                kind: group.kind.as_str().to_string(),
                message: group.message,
                context_similarity: round2(group.best_similarity),
                total_posts: group.total_posts,
                source_count: group.sources.len() as u32,
                model_count: group.models.len() as u32,
                corroboration_tier: signal_corroboration_tier(
                    group.sources.len() as u32,
                    group.models.len() as u32,
                )
                .to_string(),
                local_source_count,
                collective_source_count,
                evidence_scope,
                latest_timestamp: group.latest_timestamp,
                expires_at: group.expires_at,
                contexts: group.contexts.into_iter().take(3).collect(),
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.context_similarity
            .partial_cmp(&a.context_similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.collective_source_count.cmp(&a.collective_source_count))
            .then_with(|| {
                signal_corroboration_rank(b.source_count, b.model_count)
                    .cmp(&signal_corroboration_rank(a.source_count, a.model_count))
            })
            .then_with(|| b.source_count.cmp(&a.source_count))
            .then_with(|| b.model_count.cmp(&a.model_count))
            .then_with(|| b.total_posts.cmp(&a.total_posts))
            .then_with(|| b.latest_timestamp.cmp(&a.latest_timestamp))
    });
    results.truncate(limit);
    results
}

pub fn summarize_recent_signal_feed(
    traces: &[Trace],
    local_node_pubkey: [u8; 32],
    limit: usize,
) -> Vec<SignalFeedResult> {
    let now_ms = now_ms();
    let mut groups: HashMap<(SignalPostKind, String), SignalGroup> = HashMap::new();

    for trace in traces {
        let Some(decoded) = decode_signal_trace(trace) else {
            continue;
        };
        if decoded.expires_at <= now_ms {
            continue;
        }

        let key = (decoded.kind, decoded.message.clone());
        let entry = groups.entry(key).or_insert_with(|| SignalGroup {
            kind: decoded.kind,
            message: decoded.message.clone(),
            best_similarity: 0.0,
            total_posts: 0,
            latest_timestamp: trace.timestamp,
            expires_at: decoded.expires_at,
            contexts: BTreeSet::new(),
            sources: BTreeSet::new(),
            models: BTreeSet::new(),
            local_sources: BTreeSet::new(),
            collective_sources: BTreeSet::new(),
        });
        entry.total_posts += 1;
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
        entry.expires_at = entry.expires_at.max(decoded.expires_at);
        if !decoded.context.is_empty() {
            entry.contexts.insert(decoded.context);
        }
        let source = source_key(trace);
        entry.sources.insert(source.clone());
        entry.models.insert(trace.model_id.clone());
        if trace.node_pubkey == local_node_pubkey {
            entry.local_sources.insert(source);
        } else {
            entry.collective_sources.insert(source);
        }
    }

    let mut results: Vec<_> = groups
        .into_values()
        .map(|group| {
            let local_source_count = group.local_sources.len() as u32;
            let collective_source_count = group.collective_sources.len() as u32;
            let evidence_scope =
                signal_evidence_scope(local_source_count, collective_source_count).to_string();
            SignalFeedResult {
                kind: group.kind.as_str().to_string(),
                message: group.message,
                total_posts: group.total_posts,
                source_count: group.sources.len() as u32,
                model_count: group.models.len() as u32,
                corroboration_tier: signal_corroboration_tier(
                    group.sources.len() as u32,
                    group.models.len() as u32,
                )
                .to_string(),
                local_source_count,
                collective_source_count,
                evidence_scope,
                latest_timestamp: group.latest_timestamp,
                expires_at: group.expires_at,
                contexts: group.contexts.into_iter().take(3).collect(),
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.collective_source_count
            .cmp(&a.collective_source_count)
            .then_with(|| {
                signal_corroboration_rank(b.source_count, b.model_count)
                    .cmp(&signal_corroboration_rank(a.source_count, a.model_count))
            })
            .then_with(|| b.source_count.cmp(&a.source_count))
            .then_with(|| b.model_count.cmp(&a.model_count))
            .then_with(|| b.latest_timestamp.cmp(&a.latest_timestamp))
            .then_with(|| b.total_posts.cmp(&a.total_posts))
    });
    results.truncate(limit);
    results
}

pub fn filter_signal_feed_results(
    results: Vec<SignalFeedResult>,
    scope: SignalScopeFilter,
) -> Vec<SignalFeedResult> {
    results
        .into_iter()
        .filter(|result| scope.matches(&result.evidence_scope))
        .collect()
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

fn signal_evidence_scope(local_sources: u32, collective_sources: u32) -> &'static str {
    match (local_sources > 0, collective_sources > 0) {
        (true, true) => "mixed",
        (true, false) => "local",
        (false, true) => "collective",
        (false, false) => "unknown",
    }
}

fn signal_corroboration_tier(source_count: u32, model_count: u32) -> &'static str {
    match (source_count > 1, model_count > 1) {
        (_, true) => "multi_model",
        (true, false) => "repeated_source",
        (false, false) => "single_source",
    }
}

fn signal_corroboration_rank(source_count: u32, model_count: u32) -> u8 {
    match signal_corroboration_tier(source_count, model_count) {
        "multi_model" => 2,
        "repeated_source" => 1,
        _ => 0,
    }
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

        let results = summarize_signal_traces(
            &[trace_a, trace_b],
            "fix flaky ci workflow",
            identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "avoid");
        assert_eq!(results[0].message, "skip the generated lockfile");
        assert_eq!(results[0].total_posts, 2);
        assert_eq!(results[0].source_count, 2);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].local_source_count, 2);
        assert_eq!(results[0].collective_source_count, 0);
        assert_eq!(results[0].evidence_scope, "local");
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

        let results = summarize_signal_traces(
            &[expired, fresh],
            "ship the current branch",
            identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "watch");
        assert_eq!(results[0].total_posts, 1);
    }

    #[test]
    fn summarize_signal_posts_distinguishes_local_and_collective_sources() {
        let local_identity = NodeIdentity::generate();
        let remote_identity = NodeIdentity::generate();

        let local = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("local-1".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        let remote = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("remote-1".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            remote_identity.public_key_bytes(),
            |msg| remote_identity.sign(msg),
        );

        let results = summarize_signal_traces(
            &[local, remote],
            "repair release flow",
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].local_source_count, 1);
        assert_eq!(results[0].collective_source_count, 1);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].evidence_scope, "mixed");
    }

    #[test]
    fn summarize_recent_signal_feed_prioritizes_collective_support() {
        let local_identity = NodeIdentity::generate();
        let remote_a = NodeIdentity::generate();
        let remote_b = NodeIdentity::generate();

        let local_signal = create_signal_trace(
            SignalPostKind::Info,
            "repair release flow",
            "update changelog before tagging",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("local-1".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        let collective_a = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("remote-a".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let collective_b = create_signal_trace(
            SignalPostKind::Recommend,
            "ship the current branch",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "claude".into(),
                session_id: Some("remote-b".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[local_signal, collective_a, collective_b],
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].collective_source_count, 2);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].evidence_scope, "collective");
    }

    #[test]
    fn summarize_recent_signal_feed_prefers_multi_model_support_when_counts_tie() {
        let local_identity = NodeIdentity::generate();
        let remote_a = NodeIdentity::generate();
        let remote_b = NodeIdentity::generate();
        let remote_c = NodeIdentity::generate();
        let remote_d = NodeIdentity::generate();
        let base_now = now_ms();

        let multi_model_a = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-a".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let multi_model_b = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("remote-b".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );
        let single_model_a = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-c".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_c.public_key_bytes(),
            |msg| remote_c.sign(msg),
        );
        let single_model_b = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-d".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_d.public_key_bytes(),
            |msg| remote_d.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[single_model_a, single_model_b, multi_model_a, multi_model_b],
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].source_count, 2);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[1].message, "rerun the targeted test first");
        assert_eq!(results[1].source_count, 2);
        assert_eq!(results[1].model_count, 1);
        assert_eq!(results[1].corroboration_tier, "repeated_source");
    }

    #[test]
    fn summarize_recent_signal_feed_prefers_multi_model_over_repeated_source() {
        let local_identity = NodeIdentity::generate();
        let remote_a = NodeIdentity::generate();
        let remote_b = NodeIdentity::generate();
        let remote_c = NodeIdentity::generate();
        let remote_d = NodeIdentity::generate();
        let base_now = now_ms();

        let multi_model_a = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-a".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let multi_model_b = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "openclaw".into(),
                session_id: Some("remote-b".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );
        let repeated_a = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-c".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_c.public_key_bytes(),
            |msg| remote_c.sign(msg),
        );
        let repeated_b = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("remote-d".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            remote_d.public_key_bytes(),
            |msg| remote_d.sign(msg),
        );
        let repeated_c = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("local-1".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            base_now,
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[
                repeated_a,
                repeated_b,
                repeated_c,
                multi_model_a,
                multi_model_b,
            ],
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[1].message, "rerun the targeted test first");
        assert_eq!(results[1].corroboration_tier, "repeated_source");
        assert!(results[1].source_count > results[0].source_count);
    }

    #[test]
    fn filter_signal_feed_results_by_scope() {
        let results = vec![
            SignalFeedResult {
                kind: "recommend".into(),
                message: "local".into(),
                total_posts: 1,
                source_count: 1,
                model_count: 1,
                corroboration_tier: "single_source".into(),
                local_source_count: 1,
                collective_source_count: 0,
                evidence_scope: "local".into(),
                latest_timestamp: 1,
                expires_at: 2,
                contexts: vec!["ctx".into()],
            },
            SignalFeedResult {
                kind: "recommend".into(),
                message: "collective".into(),
                total_posts: 2,
                source_count: 2,
                model_count: 2,
                corroboration_tier: "multi_model".into(),
                local_source_count: 0,
                collective_source_count: 2,
                evidence_scope: "collective".into(),
                latest_timestamp: 2,
                expires_at: 3,
                contexts: vec!["ctx".into()],
            },
        ];

        let filtered = filter_signal_feed_results(results, SignalScopeFilter::Collective);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message, "collective");
    }

    #[test]
    fn signal_corroboration_tier_distinguishes_single_repeated_and_multi_model() {
        assert_eq!(signal_corroboration_tier(1, 1), "single_source");
        assert_eq!(signal_corroboration_tier(2, 1), "repeated_source");
        assert_eq!(signal_corroboration_tier(1, 2), "multi_model");
        assert_eq!(signal_corroboration_tier(3, 2), "multi_model");
    }

    #[test]
    fn signal_corroboration_rank_prefers_multi_model() {
        assert!(signal_corroboration_rank(1, 2) > signal_corroboration_rank(3, 1));
        assert!(signal_corroboration_rank(2, 1) > signal_corroboration_rank(1, 1));
    }
}
