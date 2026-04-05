use serde::{Deserialize, Serialize};

use crate::context::simhash;
use crate::contracts::PREHOOK_MAX_HINTS;
use crate::storage::TraceStore;

pub const AMBIENT_PRIOR_SCHEMA_VERSION: &str = "thronglets.ambient.v1";

#[derive(Debug, Deserialize)]
pub struct AmbientPriorRequest {
    pub text: String,
    #[serde(default)]
    pub space: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Serialize)]
pub struct AmbientPriorProjection {
    pub kind: &'static str,
    pub summary: String,
    pub confidence: f32,
    pub provider: String,
    pub refs: Vec<String>,
}

#[derive(Serialize)]
pub struct AmbientPriorSummary {
    pub status: &'static str,
    pub emitted: usize,
    pub context_hash: String,
    pub space: Option<String>,
}

#[derive(Serialize)]
pub struct AmbientPriorData {
    pub summary: AmbientPriorSummary,
    pub priors: Vec<AmbientPriorProjection>,
}

pub fn ambient_prior_data(
    store: &TraceStore,
    request: &AmbientPriorRequest,
) -> AmbientPriorData {
    let text = request.text.trim();
    let context_hash = simhash(text);
    let space = request
        .space
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let limit = request.limit.unwrap_or(PREHOOK_MAX_HINTS).clamp(1, PREHOOK_MAX_HINTS);
    let priors = if text.is_empty() {
        Vec::new()
    } else {
        ambient_priors_for_context(store, &context_hash, space, limit)
    };

    AmbientPriorData {
        summary: AmbientPriorSummary {
            status: if priors.is_empty() { "quiet" } else { "ready" },
            emitted: priors.len(),
            context_hash: hex_encode(&context_hash),
            space: space.map(str::to_string),
        },
        priors,
    }
}

pub fn ambient_priors_for_context(
    store: &TraceStore,
    context_hash: &[u8; 16],
    space: Option<&str>,
    limit: usize,
) -> Vec<AmbientPriorProjection> {
    let mut priors = Vec::new();
    let ctx_ref = format!("ctx:{}", hex_encode(context_hash));
    let space_ref = space.map(|value| format!("space:{value}"));

    if let Ok(failures) = store.query_similar_failed_traces(context_hash, 48, 168, 5, space)
        && !failures.is_empty()
    {
        let count = failures.len();
        let snippet: String = failures[0]
            .context_text
            .as_deref()
            .unwrap_or("unknown")
            .chars()
            .take(80)
            .collect();
        let confidence = (0.58 + (count.min(4) as f32) * 0.08).min(0.9);
        let mut refs = vec![ctx_ref.clone(), format!("failed-sessions:{count}")];
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "failure-residue",
            summary: format!(
                "recent failure residue: {count} similar session(s) failed ({snippet})"
            ),
            confidence,
            provider: "thronglets".into(),
            refs,
        });
    }

    let convergent = store
        .count_convergent_sessions(context_hash, 48, space)
        .ok()
        .unwrap_or(0);
    let contradictory_failures = store
        .count_contradicting_failed_sessions(context_hash, 48, 48, space)
        .ok()
        .unwrap_or(0);

    let minority = convergent.min(contradictory_failures);
    let majority = convergent.max(contradictory_failures);
    if minority >= 2 && majority.saturating_sub(minority) < 2 {
        let confidence =
            (0.52 + (minority.min(3) as f32) * 0.08 + (majority.min(4) as f32) * 0.03).min(0.82);
        let mut refs = vec![
            ctx_ref.clone(),
            format!("success-sessions:{convergent}"),
            format!("failed-sessions:{contradictory_failures}"),
        ];
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "mixed-residue",
            summary: format!(
                "mixed residue: similar context still shows {convergent} success / {contradictory_failures} failure sessions"
            ),
            confidence,
            provider: "thronglets".into(),
            refs,
        });
    }

    let convergence_threshold = 3usize.max(contradictory_failures as usize + 2);
    if convergent as usize >= convergence_threshold {
        let confidence = (0.56 + (convergent.min(6) as f32) * 0.06).min(0.92);
        let scope = if convergent >= 5 {
            "shared success prior"
        } else {
            "prior success"
        };
        let mut refs = vec![ctx_ref, format!("success-sessions:{convergent}")];
        if contradictory_failures > 0 {
            refs.push(format!("failed-sessions:{contradictory_failures}"));
        }
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "success-prior",
            summary: format!("{scope}: {convergent} similar session(s) crossed this context"),
            confidence,
            provider: "thronglets".into(),
            refs,
        });
    }

    priors.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    priors.truncate(limit);
    priors
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
