use crate::context::{simhash, similarity};
use crate::trace::{Outcome, Trace};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SIGNAL_CAPABILITY_PREFIX: &str = "urn:thronglets:signal:";
pub const SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX: &str = "urn:thronglets:signal-read:";
pub const DEFAULT_SIGNAL_TTL_HOURS: u32 = 72;
pub const DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS: u32 = 24;

#[derive(Debug, Clone)]
pub struct SignalTraceConfig {
    pub model_id: String,
    pub session_id: Option<String>,
    pub owner_account: Option<String>,
    pub device_identity: Option<String>,
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

    pub fn reinforcement_capability(self) -> String {
        format!("{SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX}{}", self.as_str())
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

    pub fn from_capability(capability: &str) -> Option<(Self, bool)> {
        if let Some(value) = capability.strip_prefix(SIGNAL_CAPABILITY_PREFIX) {
            return Self::parse(value).map(|kind| (kind, false));
        }
        capability
            .strip_prefix(SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX)
            .and_then(Self::parse)
            .map(|kind| (kind, true))
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
    pub reinforcement_count: u32,
    pub source_count: u32,
    pub model_count: u32,
    pub corroboration_tier: String,
    pub density_score: u8,
    pub density_tier: String,
    pub promotion_state: String,
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
    pub reinforcement_count: u32,
    pub source_count: u32,
    pub model_count: u32,
    pub corroboration_tier: String,
    pub density_score: u8,
    pub density_tier: String,
    pub promotion_state: String,
    pub focus_score: u8,
    pub focus_tier: String,
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
    reinforcement: bool,
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
    reinforcement_count: u32,
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
        || capability.starts_with(SIGNAL_REINFORCEMENT_CAPABILITY_PREFIX)
}

pub fn create_signal_trace(
    kind: SignalPostKind,
    context: &str,
    message: &str,
    config: SignalTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    create_signal_trace_at(kind, context, message, config, now_ms(), node_pubkey, sign_fn)
}

pub fn create_signal_reinforcement_trace(
    kind: SignalPostKind,
    context: &str,
    message: &str,
    config: SignalTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    create_signal_reinforcement_trace_at(
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
    create_signal_trace_with_capability(
        kind.capability(),
        context,
        message,
        config,
        now_ms,
        node_pubkey,
        sign_fn,
    )
}

fn create_signal_reinforcement_trace_at(
    kind: SignalPostKind,
    context: &str,
    message: &str,
    config: SignalTraceConfig,
    now_ms: u64,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    create_signal_trace_with_capability(
        kind.reinforcement_capability(),
        context,
        message,
        config,
        now_ms,
        node_pubkey,
        sign_fn,
    )
}

fn create_signal_trace_with_capability(
    capability: String,
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

    let mut trace = Trace::new_with_identity(
        capability,
        Outcome::Succeeded,
        0,
        message.len().min(u32::MAX as usize) as u32,
        simhash(context),
        Some(serde_json::to_string(&payload).expect("signal payload should serialize")),
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

pub fn summarize_signal_traces(
    traces: &[Trace],
    query_context: &str,
    local_device_identity: &str,
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
            reinforcement_count: 0,
            latest_timestamp: trace.timestamp,
            expires_at: decoded.expires_at,
            contexts: BTreeSet::new(),
            sources: BTreeSet::new(),
            models: BTreeSet::new(),
            local_sources: BTreeSet::new(),
            collective_sources: BTreeSet::new(),
        });
        entry.best_similarity = entry.best_similarity.max(similarity_score);
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
        entry.expires_at = entry.expires_at.max(decoded.expires_at);
        if !decoded.context.is_empty() {
            entry.contexts.insert(decoded.context);
        }
        if decoded.reinforcement {
            entry.reinforcement_count = entry.reinforcement_count.saturating_add(1);
        } else {
            entry.total_posts += 1;
            let source = source_key(trace);
            entry.sources.insert(source.clone());
            entry.models.insert(trace.model_id.clone());
            if is_local_source(trace, local_device_identity, &local_node_pubkey) {
                entry.local_sources.insert(source);
            } else {
                entry.collective_sources.insert(source);
            }
        }
    }

    let mut results: Vec<_> = groups
        .into_values()
        .filter(|group| group.total_posts > 0)
        .map(|group| {
            let local_source_count = group.local_sources.len() as u32;
            let collective_source_count = group.collective_sources.len() as u32;
            let source_count = group.sources.len() as u32;
            let model_count = group.models.len() as u32;
            let evidence_scope =
                signal_evidence_scope(local_source_count, collective_source_count).to_string();
            let density_score = signal_density_score(
                local_source_count,
                collective_source_count,
                source_count,
                model_count,
                0,
                group.reinforcement_count,
            );
            let promotion_state =
                signal_promotion_state(density_score, local_source_count, collective_source_count);
            SignalQueryResult {
                kind: group.kind.as_str().to_string(),
                message: group.message,
                context_similarity: round2(group.best_similarity),
                total_posts: group.total_posts,
                reinforcement_count: group.reinforcement_count,
                source_count,
                model_count,
                corroboration_tier: signal_corroboration_tier(source_count, model_count).to_string(),
                density_score,
                density_tier: signal_density_tier(density_score).to_string(),
                promotion_state: promotion_state.to_string(),
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
            .then_with(|| b.density_score.cmp(&a.density_score))
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
    local_device_identity: &str,
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
            reinforcement_count: 0,
            latest_timestamp: trace.timestamp,
            expires_at: decoded.expires_at,
            contexts: BTreeSet::new(),
            sources: BTreeSet::new(),
            models: BTreeSet::new(),
            local_sources: BTreeSet::new(),
            collective_sources: BTreeSet::new(),
        });
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
        entry.expires_at = entry.expires_at.max(decoded.expires_at);
        if !decoded.context.is_empty() {
            entry.contexts.insert(decoded.context);
        }
        if decoded.reinforcement {
            entry.reinforcement_count = entry.reinforcement_count.saturating_add(1);
        } else {
            entry.total_posts += 1;
            let source = source_key(trace);
            entry.sources.insert(source.clone());
            entry.models.insert(trace.model_id.clone());
            if is_local_source(trace, local_device_identity, &local_node_pubkey) {
                entry.local_sources.insert(source);
            } else {
                entry.collective_sources.insert(source);
            }
        }
    }

    let mut results: Vec<_> = groups
        .into_values()
        .filter(|group| group.total_posts > 0)
        .map(|group| {
            let local_source_count = group.local_sources.len() as u32;
            let collective_source_count = group.collective_sources.len() as u32;
            let source_count = group.sources.len() as u32;
            let model_count = group.models.len() as u32;
            let evidence_scope =
                signal_evidence_scope(local_source_count, collective_source_count).to_string();
            let freshness_rank =
                signal_freshness_rank(now_ms, group.latest_timestamp, group.expires_at);
            let density_score = signal_density_score(
                local_source_count,
                collective_source_count,
                source_count,
                model_count,
                freshness_rank,
                group.reinforcement_count,
            );
            let promotion_state =
                signal_promotion_state(density_score, local_source_count, collective_source_count);
            let focus_score = signal_focus_score(
                collective_source_count,
                source_count,
                model_count,
                freshness_rank,
            );
            SignalFeedResult {
                kind: group.kind.as_str().to_string(),
                message: group.message,
                total_posts: group.total_posts,
                reinforcement_count: group.reinforcement_count,
                source_count,
                model_count,
                corroboration_tier: signal_corroboration_tier(source_count, model_count)
                    .to_string(),
                density_score,
                density_tier: signal_density_tier(density_score).to_string(),
                promotion_state: promotion_state.to_string(),
                focus_score,
                focus_tier: signal_focus_tier(focus_score).to_string(),
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
        b.focus_score
            .cmp(&a.focus_score)
            .then_with(|| b.density_score.cmp(&a.density_score))
            .then_with(|| b.collective_source_count.cmp(&a.collective_source_count))
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
    let filtered: Vec<_> = results
        .into_iter()
        .filter(|result| scope.matches(&result.evidence_scope))
        .collect();

    let has_promoted = filtered
        .iter()
        .any(|result| result.promotion_state != "none");
    if !has_promoted {
        return filtered;
    }

    let mut promoted: Vec<_> = filtered
        .iter()
        .filter(|result| result.promotion_state != "none")
        .cloned()
        .collect();
    let mut background: Vec<_> = filtered
        .iter()
        .filter(|result| result.promotion_state == "none")
        .cloned()
        .collect();
    promoted.append(&mut background);
    promoted
}

pub fn create_query_reinforcement_traces(
    results: &[SignalQueryResult],
    query_context: &str,
    config: SignalTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl Fn(&[u8]) -> Signature,
) -> Vec<Trace> {
    results
        .iter()
        .filter(|result| result.promotion_state != "none")
        .take(3)
        .filter_map(|result| {
            let kind = SignalPostKind::parse(&result.kind)?;
            Some(create_signal_reinforcement_trace(
                kind,
                preferred_reinforcement_context(&result.contexts, query_context),
                &result.message,
                config.clone(),
                node_pubkey,
                |msg| sign_fn(msg),
            ))
        })
        .collect()
}

pub fn create_feed_reinforcement_traces(
    results: &[SignalFeedResult],
    config: SignalTraceConfig,
    node_pubkey: [u8; 32],
    sign_fn: impl Fn(&[u8]) -> Signature,
) -> Vec<Trace> {
    results
        .iter()
        .filter(|result| result.promotion_state != "none")
        .take(3)
        .filter_map(|result| {
            let kind = SignalPostKind::parse(&result.kind)?;
            Some(create_signal_reinforcement_trace(
                kind,
                preferred_reinforcement_context(&result.contexts, &result.message),
                &result.message,
                config.clone(),
                node_pubkey,
                |msg| sign_fn(msg),
            ))
        })
        .collect()
}

fn decode_signal_trace(trace: &Trace) -> Option<DecodedSignalTrace> {
    let (kind, reinforcement) = SignalPostKind::from_capability(&trace.capability)?;
    let payload: SignalTracePayload = serde_json::from_str(trace.context_text.as_deref()?).ok()?;
    Some(DecodedSignalTrace {
        kind,
        reinforcement,
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
        .device_identity
        .clone()
        .unwrap_or_else(|| {
            trace
                .node_pubkey
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
    match trace.session_id.as_deref() {
        Some(session_id) => format!("{node}:{session_id}"),
        None => node,
    }
}

fn is_local_source(trace: &Trace, local_device_identity: &str, local_node_pubkey: &[u8; 32]) -> bool {
    trace
        .device_identity
        .as_deref()
        .map(|device_identity| device_identity == local_device_identity)
        .unwrap_or(trace.node_pubkey == *local_node_pubkey)
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

fn signal_freshness_rank(now_ms: u64, latest_timestamp: u64, expires_at: u64) -> u8 {
    let total_lifetime = expires_at.saturating_sub(latest_timestamp).max(1);
    let remaining_lifetime = expires_at.saturating_sub(now_ms);
    let freshness_ratio = remaining_lifetime as f64 / total_lifetime as f64;
    if freshness_ratio >= 0.66 {
        2
    } else if freshness_ratio >= 0.33 {
        1
    } else {
        0
    }
}

fn signal_focus_score(
    collective_source_count: u32,
    source_count: u32,
    model_count: u32,
    freshness_rank: u8,
) -> u8 {
    collective_source_count.min(2) as u8
        + signal_corroboration_rank(source_count, model_count)
        + freshness_rank
}

fn signal_density_score(
    local_source_count: u32,
    collective_source_count: u32,
    source_count: u32,
    model_count: u32,
    freshness_rank: u8,
    reinforcement_count: u32,
) -> u8 {
    local_source_count.min(2) as u8
        + collective_source_count.min(2) as u8
        + signal_corroboration_rank(source_count, model_count)
        + freshness_rank
        + reinforcement_count.min(2) as u8
}

fn preferred_reinforcement_context<'a>(contexts: &'a [String], fallback: &'a str) -> &'a str {
    contexts
        .iter()
        .find(|context| !context.is_empty())
        .map(String::as_str)
        .unwrap_or(fallback)
}

fn signal_density_tier(density_score: u8) -> &'static str {
    if density_score >= 5 {
        "dominant"
    } else if density_score >= 3 {
        "promoted"
    } else if density_score >= 1 {
        "candidate"
    } else {
        "sparse"
    }
}

fn signal_promotion_state(
    density_score: u8,
    local_source_count: u32,
    collective_source_count: u32,
) -> &'static str {
    if signal_density_tier(density_score) == "sparse" || signal_density_tier(density_score) == "candidate" {
        "none"
    } else if collective_source_count > 0 {
        "collective"
    } else if local_source_count > 0 {
        "local"
    } else {
        "none"
    }
}

fn signal_focus_tier(focus_score: u8) -> &'static str {
    if focus_score >= 5 {
        "primary"
    } else if focus_score >= 3 {
        "secondary"
    } else {
        "background"
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeIdentity;

    fn signal_config(identity: &NodeIdentity, model: &str, session_id: &str) -> SignalTraceConfig {
        SignalTraceConfig {
            model_id: model.into(),
            session_id: Some(session_id.into()),
            owner_account: None,
            device_identity: Some(identity.device_identity()),
            ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
        }
    }

    #[test]
    fn summarize_signal_posts_groups_by_kind_and_message() {
        let identity = NodeIdentity::generate();
        let trace_a = create_signal_trace(
            SignalPostKind::Avoid,
            "fix flaky ci workflow",
            "skip the generated lockfile",
            signal_config(&identity, "codex", "session-a"),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let trace_b = create_signal_trace(
            SignalPostKind::Avoid,
            "repair flaky ci pipeline",
            "skip the generated lockfile",
            signal_config(&identity, "openclaw", "session-b"),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        let trace_b_timestamp = trace_b.timestamp;

        let results = summarize_signal_traces(
            &[trace_a, trace_b],
            "fix flaky ci workflow",
            &identity.device_identity(),
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
        assert_eq!(results[0].density_tier, "promoted");
        assert_eq!(results[0].promotion_state, "local");
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
                owner_account: None,
                device_identity: Some(identity.device_identity()),
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
                owner_account: None,
                device_identity: Some(identity.device_identity()),
                ttl_hours: 24,
            },
            base_now,
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );

        let results = summarize_signal_traces(
            &[expired, fresh],
            "ship the current branch",
            &identity.device_identity(),
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
            signal_config(&local_identity, "codex", "local-1"),
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        let remote = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_identity, "openclaw", "remote-1"),
            remote_identity.public_key_bytes(),
            |msg| remote_identity.sign(msg),
        );

        let results = summarize_signal_traces(
            &[local, remote],
            "repair release flow",
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].local_source_count, 1);
        assert_eq!(results[0].collective_source_count, 1);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].density_tier, "promoted");
        assert_eq!(results[0].promotion_state, "collective");
        assert_eq!(results[0].evidence_scope, "mixed");
    }

    #[test]
    fn summarize_signal_posts_reinforcement_increases_density_without_new_sources() {
        let identity = NodeIdentity::generate();
        let signal = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&identity, "codex", "session-a"),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        let reinforcement_a = create_signal_reinforcement_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "thronglets-query".into(),
                session_id: None,
                owner_account: None,
                device_identity: Some(identity.device_identity()),
                ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
            },
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        let reinforcement_b = create_signal_reinforcement_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "thronglets-feed".into(),
                session_id: None,
                owner_account: None,
                device_identity: Some(identity.device_identity()),
                ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
            },
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );

        let results = summarize_signal_traces(
            &[signal, reinforcement_a, reinforcement_b],
            "repair release flow",
            &identity.device_identity(),
            identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].total_posts, 1);
        assert_eq!(results[0].reinforcement_count, 2);
        assert_eq!(results[0].source_count, 1);
        assert_eq!(results[0].model_count, 1);
        assert_eq!(results[0].density_tier, "promoted");
        assert_eq!(results[0].promotion_state, "local");
    }

    #[test]
    fn summarize_signal_posts_ignores_reinforcement_without_base_signal() {
        let identity = NodeIdentity::generate();
        let reinforcement = create_signal_reinforcement_trace(
            SignalPostKind::Watch,
            "ship the current branch",
            "run release-check before push",
            SignalTraceConfig {
                model_id: "thronglets-query".into(),
                session_id: None,
                owner_account: None,
                device_identity: Some(identity.device_identity()),
                ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
            },
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );

        let results = summarize_signal_traces(
            &[reinforcement],
            "ship the current branch",
            &identity.device_identity(),
            identity.public_key_bytes(),
            10,
        );
        assert!(results.is_empty());
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
            signal_config(&local_identity, "codex", "local-1"),
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        let collective_a = create_signal_trace(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_a, "openclaw", "remote-a"),
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let collective_b = create_signal_trace(
            SignalPostKind::Recommend,
            "ship the current branch",
            "run release-check before push",
            signal_config(&remote_b, "claude", "remote-b"),
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[local_signal, collective_a, collective_b],
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].collective_source_count, 2);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].density_tier, "dominant");
        assert_eq!(results[0].promotion_state, "collective");
        assert_eq!(results[0].focus_tier, "primary");
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
            signal_config(&remote_a, "codex", "remote-a"),
            base_now,
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let multi_model_b = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_b, "openclaw", "remote-b"),
            base_now,
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );
        let single_model_a = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_c, "codex", "remote-c"),
            base_now,
            remote_c.public_key_bytes(),
            |msg| remote_c.sign(msg),
        );
        let single_model_b = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_d, "codex", "remote-d"),
            base_now,
            remote_d.public_key_bytes(),
            |msg| remote_d.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[single_model_a, single_model_b, multi_model_a, multi_model_b],
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].source_count, 2);
        assert_eq!(results[0].model_count, 2);
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].density_tier, "dominant");
        assert_eq!(results[0].promotion_state, "collective");
        assert_eq!(results[0].focus_tier, "primary");
        assert_eq!(results[1].message, "rerun the targeted test first");
        assert_eq!(results[1].source_count, 2);
        assert_eq!(results[1].model_count, 1);
        assert_eq!(results[1].corroboration_tier, "repeated_source");
        assert_eq!(results[1].density_tier, "dominant");
        assert_eq!(results[1].promotion_state, "collective");
        assert_eq!(results[1].focus_tier, "primary");
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
            signal_config(&remote_a, "codex", "remote-a"),
            base_now,
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let multi_model_b = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_b, "openclaw", "remote-b"),
            base_now,
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );
        let repeated_a = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_c, "codex", "remote-c"),
            base_now,
            remote_c.public_key_bytes(),
            |msg| remote_c.sign(msg),
        );
        let repeated_b = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_d, "codex", "remote-d"),
            base_now,
            remote_d.public_key_bytes(),
            |msg| remote_d.sign(msg),
        );
        let repeated_c = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&local_identity, "codex", "local-1"),
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
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "run release-check before push");
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].density_tier, "dominant");
        assert_eq!(results[0].promotion_state, "collective");
        assert_eq!(results[1].message, "rerun the targeted test first");
        assert_eq!(results[1].corroboration_tier, "repeated_source");
        assert_eq!(results[1].density_tier, "dominant");
        assert_eq!(results[1].promotion_state, "collective");
        assert!(results[1].source_count > results[0].source_count);
    }

    #[test]
    fn summarize_recent_signal_feed_prefers_fresher_signal_when_support_is_close() {
        let local_identity = NodeIdentity::generate();
        let remote_a = NodeIdentity::generate();
        let remote_b = NodeIdentity::generate();
        let remote_c = NodeIdentity::generate();
        let remote_d = NodeIdentity::generate();
        let remote_e = NodeIdentity::generate();
        let base_now = now_ms();
        let old_now = base_now.saturating_sub(50 * 60 * 60 * 1000);

        let old_a = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_a, "codex", "remote-a"),
            old_now,
            remote_a.public_key_bytes(),
            |msg| remote_a.sign(msg),
        );
        let old_b = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_b, "openclaw", "remote-b"),
            old_now,
            remote_b.public_key_bytes(),
            |msg| remote_b.sign(msg),
        );
        let old_c = create_signal_trace_at(
            SignalPostKind::Recommend,
            "repair release flow",
            "run release-check before push",
            signal_config(&remote_e, "claude", "remote-old-c"),
            old_now,
            remote_e.public_key_bytes(),
            |msg| remote_e.sign(msg),
        );

        let fresh_a = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_c, "codex", "remote-c"),
            base_now,
            remote_c.public_key_bytes(),
            |msg| remote_c.sign(msg),
        );
        let fresh_b = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_d, "openclaw", "remote-d"),
            base_now,
            remote_d.public_key_bytes(),
            |msg| remote_d.sign(msg),
        );
        let fresh_c = create_signal_trace_at(
            SignalPostKind::Watch,
            "repair release flow",
            "rerun the targeted test first",
            signal_config(&remote_e, "claude", "remote-e"),
            base_now,
            remote_e.public_key_bytes(),
            |msg| remote_e.sign(msg),
        );

        let results = summarize_recent_signal_feed(
            &[old_a, old_b, old_c, fresh_a, fresh_b, fresh_c],
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
            10,
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].message, "rerun the targeted test first");
        assert_eq!(results[0].corroboration_tier, "multi_model");
        assert_eq!(results[0].density_tier, "dominant");
        assert_eq!(results[0].promotion_state, "collective");
        assert_eq!(results[0].focus_tier, "primary");
        assert_eq!(results[1].message, "run release-check before push");
        assert_eq!(results[1].corroboration_tier, "multi_model");
        assert_eq!(results[1].density_tier, "promoted");
        assert_eq!(results[1].promotion_state, "collective");
        assert_eq!(results[1].focus_tier, "secondary");
        assert_eq!(results[0].source_count, results[1].source_count);
        assert_eq!(
            results[0].collective_source_count,
            results[1].collective_source_count
        );
    }

    #[test]
    fn filter_signal_feed_results_by_scope() {
        let results = vec![
            SignalFeedResult {
                kind: "recommend".into(),
                message: "local".into(),
                total_posts: 1,
                reinforcement_count: 0,
                source_count: 1,
                model_count: 1,
                corroboration_tier: "single_source".into(),
                density_score: 0,
                density_tier: "sparse".into(),
                promotion_state: "none".into(),
                focus_score: 0,
                focus_tier: "background".into(),
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
                reinforcement_count: 0,
                source_count: 2,
                model_count: 2,
                corroboration_tier: "multi_model".into(),
                density_score: 6,
                density_tier: "dominant".into(),
                promotion_state: "collective".into(),
                focus_score: 6,
                focus_tier: "primary".into(),
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

    #[test]
    fn signal_freshness_rank_prefers_newer_signals() {
        let now = now_ms();
        let recent_latest = now.saturating_sub(6 * 60 * 60 * 1000);
        let stale_latest = now.saturating_sub(50 * 60 * 60 * 1000);
        let expires = now.saturating_add(22 * 60 * 60 * 1000);
        assert!(
            signal_freshness_rank(now, recent_latest, now + 72 * 60 * 60 * 1000)
                > signal_freshness_rank(now, stale_latest, stale_latest + 72 * 60 * 60 * 1000)
        );
        assert!(signal_freshness_rank(now, now, expires) >= 1);
    }

    #[test]
    fn signal_focus_tier_prefers_collective_multi_model() {
        assert_eq!(signal_focus_tier(signal_focus_score(2, 2, 2, 2)), "primary");
        assert_eq!(
            signal_focus_tier(signal_focus_score(1, 2, 1, 1)),
            "secondary"
        );
        assert_eq!(
            signal_focus_tier(signal_focus_score(0, 1, 1, 0)),
            "background"
        );
    }

    #[test]
    fn signal_density_tier_moves_from_sparse_to_dominant() {
        assert_eq!(
            signal_density_tier(signal_density_score(0, 0, 1, 1, 0, 0)),
            "sparse"
        );
        assert_eq!(
            signal_density_tier(signal_density_score(1, 0, 1, 1, 0, 0)),
            "candidate"
        );
        assert_eq!(
            signal_density_tier(signal_density_score(1, 1, 2, 2, 0, 0)),
            "promoted"
        );
        assert_eq!(
            signal_density_tier(signal_density_score(1, 2, 2, 2, 1, 0)),
            "dominant"
        );
    }

    #[test]
    fn signal_promotion_state_distinguishes_local_collective_and_none() {
        assert_eq!(signal_promotion_state(0, 1, 0), "none");
        assert_eq!(signal_promotion_state(2, 1, 0), "none");
        assert_eq!(signal_promotion_state(3, 1, 0), "local");
        assert_eq!(signal_promotion_state(4, 1, 1), "collective");
    }
}
