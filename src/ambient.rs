use serde::{Deserialize, Serialize};

use crate::active_policy::{ActivePolicyRule, PolicyStrength};
use crate::context::simhash;
use crate::contracts::PREHOOK_MAX_HINTS;
use crate::posts::DERIVED_GUIDANCE_EPOCH;
use crate::storage::{ContextResidueStats, TraceStore};

pub const AMBIENT_PRIOR_SCHEMA_VERSION: &str = "thronglets.ambient.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AmbientTurnGoal {
    Explore,
    Build,
    Repair,
    Settle,
}

impl AmbientTurnGoal {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "explore" => Some(Self::Explore),
            "build" => Some(Self::Build),
            "repair" => Some(Self::Repair),
            "settle" => Some(Self::Settle),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AmbientPriorRequest {
    pub text: String,
    #[serde(default)]
    pub space: Option<String>,
    #[serde(default)]
    pub goal: Option<AmbientTurnGoal>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub active_policy: Vec<ActivePolicyRule>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AmbientPolicyState {
    PolicyConflict,
    MethodConflict,
    StablePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct AmbientPriorProjection {
    pub kind: &'static str,
    pub summary: String,
    pub confidence: f32,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<AmbientTurnGoal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_state: Option<AmbientPolicyState>,
    pub refs: Vec<String>,
}

#[derive(Serialize)]
pub struct AmbientPriorSummary {
    pub status: &'static str,
    pub emitted: usize,
    pub ruleset_epoch: &'static str,
    pub context_hash: String,
    pub space: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<AmbientTurnGoal>,
}

#[derive(Serialize)]
pub struct AmbientPriorData {
    pub summary: AmbientPriorSummary,
    pub priors: Vec<AmbientPriorProjection>,
}

pub fn ambient_prior_data(store: &TraceStore, request: &AmbientPriorRequest) -> AmbientPriorData {
    let text = request.text.trim();
    let context_hash = simhash(text);
    let space = request
        .space
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let limit = request
        .limit
        .unwrap_or(PREHOOK_MAX_HINTS)
        .clamp(1, PREHOOK_MAX_HINTS);
    let goal = request.goal;
    let priors = if text.is_empty() {
        Vec::new()
    } else {
        ambient_priors_for_context_with_policy(
            store,
            &context_hash,
            space,
            goal,
            limit,
            &request.active_policy,
        )
    };

    AmbientPriorData {
        summary: AmbientPriorSummary {
            status: if priors.is_empty() { "quiet" } else { "ready" },
            emitted: priors.len(),
            ruleset_epoch: DERIVED_GUIDANCE_EPOCH,
            context_hash: hex_encode(&context_hash),
            space: space.map(str::to_string),
            goal,
        },
        priors,
    }
}

pub fn ambient_priors_for_context(
    store: &TraceStore,
    context_hash: &[u8; 16],
    space: Option<&str>,
    goal: Option<AmbientTurnGoal>,
    limit: usize,
) -> Vec<AmbientPriorProjection> {
    ambient_priors_for_context_with_policy(store, context_hash, space, goal, limit, &[])
}

pub fn host_history_priors_for_context(
    store: &TraceStore,
    context_hash: &[u8; 16],
    space: Option<&str>,
    goal: Option<AmbientTurnGoal>,
    active_policy: &[ActivePolicyRule],
) -> Vec<AmbientPriorProjection> {
    ambient_priors_for_context_with_policy(store, context_hash, space, goal, PREHOOK_MAX_HINTS, active_policy)
        .into_iter()
        .filter(|prior| matches!(prior.kind, "mixed-residue" | "success-prior"))
        .collect()
}

pub fn ambient_priors_for_context_with_policy(
    store: &TraceStore,
    context_hash: &[u8; 16],
    space: Option<&str>,
    goal: Option<AmbientTurnGoal>,
    limit: usize,
    active_policy: &[ActivePolicyRule],
) -> Vec<AmbientPriorProjection> {
    let mut priors = Vec::new();
    let ctx_ref = format!("ctx:{}", hex_encode(context_hash));
    let space_ref = space.map(|value| format!("space:{value}"));
    let stats = store
        .residue_stats_for_context(context_hash, 48, 168, 64, space)
        .unwrap_or_default();
    let total_failures = stats.total_failure();
    let total_success = stats.total_success();
    let hard_policy_active = active_policy
        .iter()
        .any(|rule| rule.strength == PolicyStrength::Hard);

    if total_failures > 0 {
        let confidence = (0.58 + (total_failures.min(4) as f32) * 0.08).min(0.9);
        let mut refs = vec![
            ctx_ref.clone(),
            format!("failed-sessions:{total_failures}"),
            format!("failed-compliant:{}", stats.failure_compliant),
            format!("failed-noncompliant:{}", stats.failure_noncompliant),
        ];
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "failure-residue",
            summary: failure_residue_summary(stats, hard_policy_active),
            confidence,
            provider: "thronglets".into(),
            goal,
            policy_state: if hard_policy_active && stats.failure_noncompliant > 0 {
                Some(AmbientPolicyState::PolicyConflict)
            } else {
                None
            },
            refs,
        });
    }

    if should_emit_mixed_residue(stats, hard_policy_active) {
        let confidence = mixed_residue_confidence(stats);
        let mut refs = vec![
            ctx_ref.clone(),
            format!("success-sessions:{total_success}"),
            format!("failed-sessions:{total_failures}"),
            format!("success-compliant:{}", stats.success_compliant),
            format!("success-noncompliant:{}", stats.success_noncompliant),
        ];
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "mixed-residue",
            summary: mixed_residue_summary(stats, hard_policy_active),
            confidence,
            provider: "thronglets".into(),
            goal,
            policy_state: if hard_policy_active && stats.total_noncompliant() > 0 {
                Some(AmbientPolicyState::PolicyConflict)
            } else if stats.success_noncompliant > 0 {
                Some(AmbientPolicyState::MethodConflict)
            } else {
                None
            },
            refs,
        });
    }

    let convergence_threshold =
        success_prior_threshold(goal, total_failures as usize, stats.success_noncompliant as usize);
    if can_form_success_prior(stats, hard_policy_active, convergence_threshold) {
        let compliant_success = stats.success_compliant;
        let confidence = (0.56 + (compliant_success.min(6) as f32) * 0.06).min(0.92);
        let scope = if compliant_success >= 5 {
            "shared success prior"
        } else {
            "prior success"
        };
        let mut refs = vec![
            ctx_ref,
            format!("success-compliant:{compliant_success}"),
            format!("success-unknown:{}", stats.success_unknown),
        ];
        if total_failures > 0 {
            refs.push(format!("failed-sessions:{total_failures}"));
        }
        if let Some(space_ref) = &space_ref {
            refs.push(space_ref.clone());
        }
        priors.push(AmbientPriorProjection {
            kind: "success-prior",
            summary: format!(
                "{scope}: {compliant_success} compliant session(s) crossed this context"
            ),
            confidence,
            provider: "thronglets".into(),
            goal,
            policy_state: Some(AmbientPolicyState::StablePath),
            refs,
        });
    }

    apply_goal_bias(&mut priors, goal);
    priors.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    priors.truncate(limit);
    priors
}

fn failure_residue_summary(stats: ContextResidueStats, hard_policy_active: bool) -> String {
    if hard_policy_active && stats.failure_noncompliant > 0 {
        return format!(
            "policy conflict: {} recent failure session(s) violated the active method policy",
            stats.failure_noncompliant
        );
    }
    if stats.failure_noncompliant > 0 {
        return format!(
            "recent failure residue: {} similar failure session(s), including {} method-conflict path(s)",
            stats.total_failure(),
            stats.failure_noncompliant
        );
    }
    format!(
        "recent failure residue: {} similar failure session(s)",
        stats.total_failure()
    )
}

fn should_emit_mixed_residue(stats: ContextResidueStats, hard_policy_active: bool) -> bool {
    if hard_policy_active && stats.total_noncompliant() > 0 {
        return true;
    }
    if stats.success_noncompliant > 0 {
        return true;
    }
    let minority = stats.total_success().min(stats.total_failure());
    let majority = stats.total_success().max(stats.total_failure());
    minority >= 2 && majority.saturating_sub(minority) < 2
}

fn mixed_residue_summary(stats: ContextResidueStats, hard_policy_active: bool) -> String {
    if hard_policy_active && stats.total_noncompliant() > 0 {
        return format!(
            "policy conflict: similar context still succeeds sometimes, but {} session(s) violate the active hard method rule",
            stats.total_noncompliant()
        );
    }
    if stats.success_noncompliant > 0 {
        return format!(
            "unsettled due to method conflict: {} success session(s) exist, but {} used a contested method",
            stats.total_success(),
            stats.success_noncompliant
        );
    }
    format!(
        "mixed residue: similar context still shows {} success / {} failure sessions",
        stats.total_success(),
        stats.total_failure()
    )
}

fn mixed_residue_confidence(stats: ContextResidueStats) -> f32 {
    let minority = stats.total_success().min(stats.total_failure());
    let majority = stats.total_success().max(stats.total_failure());
    (0.52 + (minority.min(3) as f32) * 0.08 + (majority.min(4) as f32) * 0.03).min(0.84)
}

fn can_form_success_prior(
    stats: ContextResidueStats,
    hard_policy_active: bool,
    threshold: usize,
) -> bool {
    if hard_policy_active && stats.total_noncompliant() > 0 {
        return false;
    }
    let compliant = stats.success_compliant as usize;
    if compliant < threshold || compliant == 0 {
        return false;
    }
    compliant > stats.success_noncompliant as usize
        && compliant > stats.total_failure() as usize
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn apply_goal_bias(priors: &mut [AmbientPriorProjection], goal: Option<AmbientTurnGoal>) {
    let Some(goal) = goal else {
        return;
    };

    for prior in priors.iter_mut() {
        let (factor, cap, note) = match (goal, prior.kind) {
            (AmbientTurnGoal::Explore, "failure-residue") => (
                0.84,
                0.72,
                Some("avoid repeating the same local damage, but keep novel variants reversible"),
            ),
            (AmbientTurnGoal::Explore, "mixed-residue") => (
                1.02,
                0.78,
                Some("context remains open; prefer cheap probes over premature convergence"),
            ),
            (AmbientTurnGoal::Explore, "success-prior") => (
                0.74,
                0.68,
                Some("treat this as a non-exclusive baseline during exploration"),
            ),
            (AmbientTurnGoal::Build, "failure-residue") => (0.96, 0.98, None),
            (AmbientTurnGoal::Build, "mixed-residue") => (0.98, 0.98, None),
            (AmbientTurnGoal::Build, "success-prior") => (1.10, 0.98, None),
            (AmbientTurnGoal::Repair, "failure-residue") => (1.12, 0.98, None),
            (AmbientTurnGoal::Repair, "mixed-residue") => (1.08, 0.98, None),
            (AmbientTurnGoal::Repair, "success-prior") => (0.90, 0.98, None),
            (AmbientTurnGoal::Settle, "failure-residue") => (1.04, 0.98, None),
            (AmbientTurnGoal::Settle, "mixed-residue") => (1.10, 0.98, None),
            (AmbientTurnGoal::Settle, "success-prior") => (1.06, 0.98, None),
            _ => (1.0, 0.98, None),
        };
        prior.confidence = (prior.confidence * factor).clamp(0.0, cap);
        if let Some(note) = note
            && !prior.summary.contains(note)
        {
            prior.summary.push_str("; ");
            prior.summary.push_str(note);
        }
    }
}

fn success_prior_threshold(
    goal: Option<AmbientTurnGoal>,
    contradictory_failures: usize,
    noncompliant_successes: usize,
) -> usize {
    match goal {
        Some(AmbientTurnGoal::Explore) => {
            4usize.max(contradictory_failures + noncompliant_successes + 3)
        }
        _ => 3usize.max(contradictory_failures + noncompliant_successes + 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::active_policy::compile_active_policy;
    use crate::active_policy::{ActivePolicyRule, PolicyScope};
    use crate::context::simhash;
    use crate::identity::NodeIdentity;
    use crate::trace::{MethodCompliance, Outcome, Trace};
    use serde_json::json;

    fn insert_trace(
        store: &TraceStore,
        identity: &NodeIdentity,
        context: &str,
        session_id: &str,
        outcome: Outcome,
        compliance: Option<MethodCompliance>,
    ) {
        let trace = Trace::new_with_agent_compliance(
            "tool:Edit".into(),
            outcome,
            0,
            1,
            simhash(context),
            Some(context.into()),
            Some(session_id.into()),
            None,
            Some(identity.device_identity()),
            None,
            None,
            compliance,
            "codex".into(),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        store.insert(&trace).unwrap();
    }

    #[test]
    fn repeated_compliant_success_forms_stable_prior() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";
        for idx in 0..3 {
            insert_trace(
                &store,
                &identity,
                ctx,
                &format!("success-{idx}"),
                Outcome::Succeeded,
                Some(MethodCompliance::Compliant),
            );
        }

        let priors = ambient_priors_for_context(&store, &simhash(ctx), None, None, 3);
        let stable = priors.iter().find(|prior| prior.kind == "success-prior");
        assert!(stable.is_some(), "{priors:#?}");
        assert_eq!(stable.unwrap().policy_state, Some(AmbientPolicyState::StablePath));
    }

    #[test]
    fn noncompliant_success_stays_mixed_and_never_hardens_into_stable_path() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";
        for idx in 0..4 {
            insert_trace(
                &store,
                &identity,
                ctx,
                &format!("success-{idx}"),
                Outcome::Succeeded,
                Some(MethodCompliance::Noncompliant),
            );
        }

        let priors = ambient_priors_for_context(&store, &simhash(ctx), None, None, 3);
        assert!(priors.iter().all(|prior| prior.kind != "success-prior"), "{priors:#?}");
        let mixed = priors.iter().find(|prior| prior.kind == "mixed-residue");
        assert!(mixed.is_some(), "{priors:#?}");
        assert_eq!(mixed.unwrap().policy_state, Some(AmbientPolicyState::MethodConflict));
    }

    #[test]
    fn hard_active_policy_turns_noncompliance_into_policy_conflict() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";
        for idx in 0..2 {
            insert_trace(
                &store,
                &identity,
                ctx,
                &format!("success-{idx}"),
                Outcome::Succeeded,
                Some(MethodCompliance::Noncompliant),
            );
        }
        let active_policy = vec![ActivePolicyRule {
            id: "task:reuse-components".into(),
            strength: PolicyStrength::Hard,
            scope: PolicyScope::Task,
            summary: "reuse existing shared components".into(),
        }];

        let priors = ambient_priors_for_context_with_policy(
            &store,
            &simhash(ctx),
            None,
            Some(AmbientTurnGoal::Build),
            3,
            &active_policy,
        );
        let conflict = priors
            .iter()
            .find(|prior| prior.policy_state == Some(AmbientPolicyState::PolicyConflict));
        assert!(conflict.is_some(), "{priors:#?}");
        assert!(conflict.unwrap().summary.contains("policy conflict"));
    }

    #[test]
    fn explore_keeps_stable_prior_soft_and_nonexclusive() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard.tsx";
        for idx in 0..5 {
            insert_trace(
                &store,
                &identity,
                ctx,
                &format!("success-{idx}"),
                Outcome::Succeeded,
                Some(MethodCompliance::Compliant),
            );
        }

        let priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(AmbientTurnGoal::Explore),
            3,
        );
        let stable = priors.iter().find(|prior| prior.kind == "success-prior");
        assert!(stable.is_some(), "{priors:#?}");
        assert!(stable
            .unwrap()
            .summary
            .contains("non-exclusive baseline during exploration"));
        assert!(stable.unwrap().confidence <= 0.68);
    }

    #[test]
    fn duplicate_frontend_success_stays_unsettled_under_current_turn_component_policy() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "edit file: src/app/dashboard/page.tsx";
        for idx in 0..4 {
            insert_trace(
                &store,
                &identity,
                ctx,
                &format!("duplicate-ui-success-{idx}"),
                Outcome::Succeeded,
                Some(MethodCompliance::Noncompliant),
            );
        }

        let payload = json!({
            "currentTurnCorrection": "reuse existing shared components instead of hand-writing duplicate page UI",
            "tool_input": {
                "file_path": "/repo/src/app/dashboard/page.tsx"
            }
        });
        let active_policy = compile_active_policy(&payload, &payload["tool_input"]);

        let priors = ambient_priors_for_context_with_policy(
            &store,
            &simhash(ctx),
            None,
            Some(AmbientTurnGoal::Build),
            3,
            &active_policy.relevant_rules,
        );
        assert!(
            priors.iter().all(|prior| prior.kind != "success-prior"),
            "{priors:#?}"
        );
        let conflict = priors
            .iter()
            .find(|prior| prior.policy_state == Some(AmbientPolicyState::PolicyConflict));
        assert!(conflict.is_some(), "{priors:#?}");
        assert!(conflict.unwrap().summary.contains("policy conflict"));
    }
}
