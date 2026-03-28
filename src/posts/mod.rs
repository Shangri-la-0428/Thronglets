use crate::context::{simhash, similarity};
use crate::trace::{Outcome, Trace};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

pub const SIGNAL_CAPABILITY_PREFIX: &str = "urn:thronglets:signal:";

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
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalQueryResult {
    pub kind: String,
    pub message: String,
    pub context_similarity: f64,
    pub total_posts: u64,
    pub source_count: u32,
    pub latest_timestamp: u64,
    pub contexts: Vec<String>,
}

#[derive(Debug, Clone)]
struct DecodedSignalTrace {
    kind: SignalPostKind,
    context: String,
    message: String,
}

#[derive(Debug)]
struct SignalGroup {
    kind: SignalPostKind,
    message: String,
    best_similarity: f64,
    total_posts: u64,
    latest_timestamp: u64,
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
    model_id: String,
    session_id: Option<String>,
    node_pubkey: [u8; 32],
    sign_fn: impl FnOnce(&[u8]) -> Signature,
) -> Trace {
    let payload = SignalTracePayload {
        context: context.to_string(),
        message: message.to_string(),
    };

    Trace::new(
        kind.capability(),
        Outcome::Succeeded,
        0,
        message.len().min(u32::MAX as usize) as u32,
        simhash(context),
        Some(serde_json::to_string(&payload).expect("signal payload should serialize")),
        session_id,
        model_id,
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
    let mut groups: HashMap<(SignalPostKind, String), SignalGroup> = HashMap::new();

    for trace in traces {
        let Some(decoded) = decode_signal_trace(trace) else {
            continue;
        };

        let similarity_score = similarity(&query_hash, &trace.context_hash);
        let key = (decoded.kind, decoded.message.clone());
        let entry = groups.entry(key).or_insert_with(|| SignalGroup {
            kind: decoded.kind,
            message: decoded.message.clone(),
            best_similarity: similarity_score,
            total_posts: 0,
            latest_timestamp: trace.timestamp,
            contexts: BTreeSet::new(),
            sources: BTreeSet::new(),
        });
        entry.best_similarity = entry.best_similarity.max(similarity_score);
        entry.total_posts += 1;
        entry.latest_timestamp = entry.latest_timestamp.max(trace.timestamp);
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
    })
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
            "codex".into(),
            Some("session-a".into()),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let trace_b = create_signal_trace(
            SignalPostKind::Avoid,
            "repair flaky ci pipeline",
            "skip the generated lockfile",
            "openclaw".into(),
            Some("session-b".into()),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );

        let results = summarize_signal_traces(&[trace_a, trace_b], "fix flaky ci workflow", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "avoid");
        assert_eq!(results[0].message, "skip the generated lockfile");
        assert_eq!(results[0].total_posts, 2);
        assert_eq!(results[0].source_count, 2);
    }
}
