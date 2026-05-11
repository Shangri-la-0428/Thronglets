use serde_json::json;
use thronglets::active_policy::compile_active_policy;
use thronglets::ambient::{
    AmbientPolicyState, AmbientTurnGoal, ambient_priors_for_context_with_policy,
};
use thronglets::context::simhash;
use thronglets::continuity::{ContinuityEvent, ContinuityTaxonomy, ExternalContinuityInput};
use thronglets::identity::IdentityBinding;
use thronglets::identity::NodeIdentity;
use thronglets::pheromone::PheromoneField;
use thronglets::pulse::{PRESENCE_DIMENSION_NAME, PulseEmitter, VIABILITY_DIMENSION_NAME};
use thronglets::service::{self, Ctx, RecordTraceReq};
use thronglets::storage::TraceStore;
use thronglets::trace::{MethodCompliance, Outcome, Trace, TraceEvidence};

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

fn hard_task_policy(summary: &str) -> Vec<thronglets::active_policy::ActivePolicyRule> {
    compile_active_policy(
        &json!({
            "current_turn_correction": summary,
        }),
        &json!({}),
    )
    .all_rules
}

#[test]
fn regression_corpus_pulse_dimensions_use_canonical_names() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let emitter = PulseEmitter::new("SIG_test", "http://localhost:1317", "oasyce-1");

    let dims = emitter.aggregate_dimensions(&store, &identity);

    assert_eq!(dims.get(PRESENCE_DIMENSION_NAME), Some(&false));
    assert_eq!(dims.get(VIABILITY_DIMENSION_NAME), Some(&false));
    assert!(!dims.contains_key("thronglets"));
    assert!(!dims.contains_key("psyche"));
}

#[test]
fn regression_corpus_record_trace_scopes_storage_and_rejects_invalid_continuity() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let binding = IdentityBinding::new(identity.device_identity());
    let field = PheromoneField::new();
    let ctx = Ctx {
        store: &store,
        field: Some(&field),
        identity: &identity,
        binding: &binding,
    };

    let req = RecordTraceReq {
        capability: "tool:Edit".into(),
        outcome: Outcome::Failed,
        latency_ms: 120,
        input_size: 2,
        context: "repair flaky login flow".into(),
        model: "claude-opus-4-6".into(),
        session_id: Some("session-space".into()),
        space: Some("psyche".into()),
        agent_id: None,
        sigil_id: None,
        method_compliance: Some(MethodCompliance::Unknown),
        evidence: TraceEvidence::default(),
    };

    let recorded = service::record_trace(&ctx, req, None).expect("record_trace should succeed");
    let service::RecordResult::Trace(trace) = recorded else {
        panic!("expected normal trace result");
    };
    assert_eq!(trace.trace.capability, "tool:Edit");

    let matching = store
        .query_similar_failed_traces(
            &simhash("repair flaky login flow"),
            0,
            24,
            10,
            Some("psyche"),
        )
        .unwrap();
    assert_eq!(matching.len(), 1);

    let other_space = store
        .query_similar_failed_traces(
            &simhash("repair flaky login flow"),
            0,
            24,
            10,
            Some("other-space"),
        )
        .unwrap();
    assert!(other_space.is_empty());

    let invalid_continuity = ExternalContinuityInput {
        provider: "thronglets".into(),
        mode: "optional".into(),
        version: 1,
        taxonomy: ContinuityTaxonomy::Coordination,
        event: ContinuityEvent::WritebackCalibration,
        summary: "writeback drift keeps reopening the same loop".into(),
        space: Some("psyche".into()),
        audit_ref: None,
    };
    let err = match service::record_trace(
        &ctx,
        RecordTraceReq {
            capability: "".into(),
            outcome: Outcome::Succeeded,
            latency_ms: 0,
            input_size: 0,
            context: "".into(),
            model: "claude-opus-4-6".into(),
            session_id: Some("session-continuity".into()),
            space: Some("psyche".into()),
            agent_id: None,
            sigil_id: None,
            method_compliance: None,
            evidence: TraceEvidence::default(),
        },
        Some(invalid_continuity),
    ) {
        Ok(_) => panic!("invalid continuity should be rejected gracefully"),
        Err(err) => err,
    };
    assert!(err.contains("taxonomy"));
}

#[test]
fn regression_corpus_duplicate_ui_repair_never_hardens_noncompliant_success() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let ctx = "edit file: src/app/dashboard.tsx duplicate page UI";
    for idx in 0..4 {
        insert_trace(
            &store,
            &identity,
            ctx,
            &format!("session-{idx}"),
            Outcome::Succeeded,
            Some(MethodCompliance::Noncompliant),
        );
    }

    let priors = ambient_priors_for_context_with_policy(
        &store,
        &simhash(ctx),
        None,
        Some(AmbientTurnGoal::Repair),
        4,
        &hard_task_policy(
            "reuse existing shared components instead of hand-writing duplicate page UI",
        ),
    );

    assert!(
        priors.iter().all(|prior| prior.kind != "success-prior"),
        "{priors:#?}"
    );
    let conflict = priors
        .iter()
        .find(|prior| prior.policy_state == Some(AmbientPolicyState::PolicyConflict))
        .expect("expected policy conflict");
    assert!(
        conflict.summary.contains("policy conflict"),
        "{conflict:#?}"
    );
}

#[test]
fn regression_corpus_repair_prefers_conflict_visibility_over_false_confidence() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let ctx = "repair flaky settings panel with repeated handwritten fixes";
    for idx in 0..3 {
        insert_trace(
            &store,
            &identity,
            ctx,
            &format!("bad-success-{idx}"),
            Outcome::Succeeded,
            Some(MethodCompliance::Noncompliant),
        );
    }
    for idx in 0..2 {
        insert_trace(
            &store,
            &identity,
            ctx,
            &format!("bad-failure-{idx}"),
            Outcome::Failed,
            Some(MethodCompliance::Noncompliant),
        );
    }

    let priors = ambient_priors_for_context_with_policy(
        &store,
        &simhash(ctx),
        None,
        Some(AmbientTurnGoal::Repair),
        4,
        &hard_task_policy(
            "reuse existing shared components instead of hand-writing duplicate page UI",
        ),
    );

    let first = priors.first().expect("expected at least one prior");
    assert_eq!(first.policy_state, Some(AmbientPolicyState::PolicyConflict));
    assert!(
        priors.iter().all(|prior| prior.kind != "success-prior"),
        "repair should not emit stable path under repeated noncompliant success: {priors:#?}"
    );
}

#[test]
fn regression_corpus_explore_keeps_reversible_nonconsensus_probe_available() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let ctx = "experiment with a new deploy rollback flow";
    for idx in 0..4 {
        insert_trace(
            &store,
            &identity,
            ctx,
            &format!("stable-success-{idx}"),
            Outcome::Succeeded,
            Some(MethodCompliance::Compliant),
        );
    }
    insert_trace(
        &store,
        &identity,
        ctx,
        "reversible-probe-failure",
        Outcome::Failed,
        Some(MethodCompliance::Unknown),
    );

    let priors = ambient_priors_for_context_with_policy(
        &store,
        &simhash(ctx),
        None,
        Some(AmbientTurnGoal::Explore),
        4,
        &[],
    );

    let success = priors
        .iter()
        .find(|prior| prior.kind == "success-prior")
        .expect("expected a success prior");
    assert!(
        success
            .summary
            .contains("non-exclusive baseline during exploration"),
        "{success:#?}"
    );
    assert!(
        success.confidence < 0.7,
        "explore should keep stable paths soft, got {}",
        success.confidence
    );

    let mixed = priors
        .iter()
        .find(|prior| prior.kind == "mixed-residue")
        .expect("expected mixed residue");
    assert!(
        mixed.summary.contains("cheap probes") || mixed.summary.contains("open"),
        "{mixed:#?}"
    );
}

#[test]
fn regression_corpus_compliant_success_still_forms_stable_path() {
    let store = TraceStore::in_memory().unwrap();
    let identity = NodeIdentity::generate();
    let ctx = "refactor settings page to reuse shared components";
    for idx in 0..3 {
        insert_trace(
            &store,
            &identity,
            ctx,
            &format!("good-success-{idx}"),
            Outcome::Succeeded,
            Some(MethodCompliance::Compliant),
        );
    }

    let priors = ambient_priors_for_context_with_policy(
        &store,
        &simhash(ctx),
        None,
        Some(AmbientTurnGoal::Build),
        3,
        &hard_task_policy("reuse existing shared components"),
    );

    let stable = priors
        .iter()
        .find(|prior| prior.kind == "success-prior")
        .expect("expected stable path");
    assert_eq!(stable.policy_state, Some(AmbientPolicyState::StablePath));
}
