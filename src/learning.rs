//! Derived learning views for a space.
//!
//! Learning is not stored as a new object. Traces remain the fact source; this
//! module compresses recent evidence into the sparse read surface exposed by
//! `space --json`.

use crate::trace::{EvidenceVerification, MethodCompliance, Outcome, Trace};
use crate::workspace::SpaceFeedbackSummary;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct LearningStablePath {
    pub capability: String,
    pub context: String,
    pub compliant_success_sessions: u32,
    pub artifact_ref_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningFailureResidue {
    pub capability: String,
    pub context: String,
    pub failed_sessions: u32,
    pub method_conflict_sessions: u32,
    pub artifact_backed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningRegressionCandidate {
    pub capability: String,
    pub context: String,
    pub trial_key: Option<String>,
    pub failed_sessions: u32,
    pub artifact_refs: Vec<String>,
    pub verification: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningCompressionDebt {
    pub level: &'static str,
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpaceLearningView {
    pub status: &'static str,
    pub stable_paths: Vec<LearningStablePath>,
    pub failure_residue: Vec<LearningFailureResidue>,
    pub regression_candidates: Vec<LearningRegressionCandidate>,
    pub compression_debt: LearningCompressionDebt,
}

#[derive(Default)]
struct LearningGroup {
    capability: String,
    context: String,
    trial_key: Option<String>,
    success_compliant: BTreeSet<String>,
    success_noncompliant: BTreeSet<String>,
    failure_sessions: BTreeSet<String>,
    failure_noncompliant: BTreeSet<String>,
    artifact_refs: BTreeSet<String>,
    verification: Option<EvidenceVerification>,
}

pub fn view_from_traces(
    traces: &[Trace],
    local_feedback: &SpaceFeedbackSummary,
    limit: usize,
) -> SpaceLearningView {
    let mut groups: BTreeMap<(String, [u8; 16], Option<String>), LearningGroup> = BTreeMap::new();
    for trace in traces {
        let key = (
            trace.capability.clone(),
            trace.context_hash,
            trace.evidence.trial_key.clone(),
        );
        let group = groups.entry(key).or_insert_with(|| LearningGroup {
            capability: trace.capability.clone(),
            context: compact_context(trace.context_text.as_deref(), &trace.capability),
            trial_key: trace.evidence.trial_key.clone(),
            ..LearningGroup::default()
        });

        for artifact_ref in &trace.evidence.artifact_refs {
            group.artifact_refs.insert(artifact_ref.clone());
        }
        if group.verification.is_none() {
            group.verification = trace.evidence.verification;
        }

        let session_key = learning_session_key(trace);
        let compliance = trace.method_compliance.unwrap_or(MethodCompliance::Unknown);
        match trace.outcome {
            Outcome::Succeeded => match compliance {
                MethodCompliance::Compliant => {
                    group.success_compliant.insert(session_key);
                }
                MethodCompliance::Noncompliant => {
                    group.success_noncompliant.insert(session_key);
                }
                MethodCompliance::Unknown => {}
            },
            Outcome::Failed | Outcome::Timeout => {
                group.failure_sessions.insert(session_key.clone());
                if compliance == MethodCompliance::Noncompliant {
                    group.failure_noncompliant.insert(session_key);
                }
            }
            Outcome::Partial => {}
        }
    }

    let mut stable_paths = Vec::new();
    let mut failure_residue = Vec::new();
    let mut regression_candidates = Vec::new();
    let mut method_conflict_groups = 0usize;
    let mut unverified_failure_groups = 0usize;
    let mut conflicting_outcome_groups = 0usize;
    let mut patch_like_traces = 0usize;

    for trace in traces {
        let cap = trace.capability.to_ascii_lowercase();
        if cap.contains("edit") || cap.contains("write") {
            patch_like_traces += 1;
        }
    }

    for group in groups.values() {
        let compliant_success = group.success_compliant.len() as u32;
        let noncompliant_success = group.success_noncompliant.len() as u32;
        let failed = group.failure_sessions.len() as u32;
        let method_conflicts =
            (group.success_noncompliant.len() + group.failure_noncompliant.len()) as u32;
        let artifact_backed = !group.artifact_refs.is_empty();

        if method_conflicts > 0 {
            method_conflict_groups += 1;
        }
        if failed >= 2 && !artifact_backed {
            unverified_failure_groups += 1;
        }
        if failed > 0 && compliant_success + noncompliant_success > 0 {
            conflicting_outcome_groups += 1;
        }

        if compliant_success >= 3 && noncompliant_success == 0 && failed == 0 {
            stable_paths.push(LearningStablePath {
                capability: group.capability.clone(),
                context: group.context.clone(),
                compliant_success_sessions: compliant_success,
                artifact_ref_count: group.artifact_refs.len() as u32,
            });
        }

        if failed >= 2 || method_conflicts > 0 {
            failure_residue.push(LearningFailureResidue {
                capability: group.capability.clone(),
                context: group.context.clone(),
                failed_sessions: failed,
                method_conflict_sessions: method_conflicts,
                artifact_backed,
            });
        }

        if failed >= 2 && artifact_backed {
            regression_candidates.push(LearningRegressionCandidate {
                capability: group.capability.clone(),
                context: group.context.clone(),
                trial_key: group.trial_key.clone(),
                failed_sessions: failed,
                artifact_refs: group.artifact_refs.iter().take(5).cloned().collect(),
                verification: group.verification.map(|value| value.as_str().to_string()),
            });
        }
    }

    stable_paths.sort_by(|a, b| {
        b.compliant_success_sessions
            .cmp(&a.compliant_success_sessions)
            .then_with(|| a.capability.cmp(&b.capability))
    });
    failure_residue.sort_by(|a, b| {
        b.failed_sessions
            .cmp(&a.failed_sessions)
            .then_with(|| b.method_conflict_sessions.cmp(&a.method_conflict_sessions))
            .then_with(|| a.capability.cmp(&b.capability))
    });
    regression_candidates.sort_by(|a, b| {
        b.failed_sessions
            .cmp(&a.failed_sessions)
            .then_with(|| a.capability.cmp(&b.capability))
    });

    let limit = limit.max(1);
    stable_paths.truncate(limit);
    failure_residue.truncate(limit);
    regression_candidates.truncate(limit);

    let compression_debt = compression_debt(
        method_conflict_groups,
        unverified_failure_groups,
        conflicting_outcome_groups,
        patch_like_traces,
        local_feedback,
    );
    let status = learning_status(
        traces,
        &stable_paths,
        &failure_residue,
        &regression_candidates,
        &compression_debt,
        local_feedback,
    );

    SpaceLearningView {
        status,
        stable_paths,
        failure_residue,
        regression_candidates,
        compression_debt,
    }
}

fn learning_status(
    traces: &[Trace],
    stable_paths: &[LearningStablePath],
    failure_residue: &[LearningFailureResidue],
    regression_candidates: &[LearningRegressionCandidate],
    compression_debt: &LearningCompressionDebt,
    local_feedback: &SpaceFeedbackSummary,
) -> &'static str {
    if traces.is_empty() && local_feedback.positive_24h == 0 && local_feedback.negative_24h == 0 {
        return "quiet";
    }
    if compression_debt.level == "high" && !failure_residue.is_empty() {
        return "blocked";
    }
    if !stable_paths.is_empty() && compression_debt.level == "low" {
        return "compressed";
    }
    if !stable_paths.is_empty()
        || !regression_candidates.is_empty()
        || local_feedback.positive_24h > 0
    {
        return "converging";
    }
    "accumulating"
}

fn compression_debt(
    method_conflict_groups: usize,
    unverified_failure_groups: usize,
    conflicting_outcome_groups: usize,
    patch_like_traces: usize,
    local_feedback: &SpaceFeedbackSummary,
) -> LearningCompressionDebt {
    let mut score = 0.0f64;
    let mut reasons = Vec::new();
    if method_conflict_groups > 0 {
        score += 0.35;
        reasons.push(format!("{method_conflict_groups} method-conflict group(s)"));
    }
    if unverified_failure_groups > 0 {
        score += 0.30;
        reasons.push(format!(
            "{unverified_failure_groups} repeated failure group(s) without artifact-backed regression evidence"
        ));
    }
    if conflicting_outcome_groups > 0 {
        score += 0.20;
        reasons.push(format!(
            "{conflicting_outcome_groups} context group(s) have both success and failure residue"
        ));
    }
    if local_feedback.negative_24h > 0 {
        score += 0.15;
        reasons.push(format!(
            "{} negative local follow event(s) in 24h",
            local_feedback.negative_24h
        ));
    }
    if patch_like_traces >= 8 && unverified_failure_groups > 0 {
        score += 0.10;
        reasons.push("patch-heavy residue is accumulating without regression artifacts".into());
    }

    let score = score.min(1.0);
    let level = if score >= 0.70 {
        "high"
    } else if score >= 0.35 {
        "medium"
    } else {
        "low"
    };
    LearningCompressionDebt {
        level,
        score: (score * 100.0).round() / 100.0,
        reasons,
    }
}

fn learning_session_key(trace: &Trace) -> String {
    trace
        .session_id
        .clone()
        .or_else(|| trace.device_identity.clone())
        .unwrap_or_else(|| trace.id[..8].iter().map(|b| format!("{b:02x}")).collect())
}

fn compact_context(context: Option<&str>, fallback: &str) -> String {
    let raw = context
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    if raw.chars().count() <= 140 {
        return raw.to_string();
    }
    raw.chars().take(137).collect::<String>() + "..."
}
