//! Shared service layer — transport-agnostic business logic.
//!
//! HTTP and MCP handlers are thin adapters: parse transport input,
//! call these functions, format the result for their protocol.
//! Business logic lives here, once.

use crate::context::{format_signal_context, simhash, similarity};
use crate::continuity::{
    ExternalContinuityInput, ExternalContinuityRecordConfig, record_external_continuity,
};
use crate::identity::{IdentityBinding, NodeIdentity};
pub use crate::learning::SpaceLearningView;
use crate::pheromone::PheromoneField;
use crate::posts::{
    DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS, SignalPostKind, SignalTraceConfig,
    create_auto_signal_trace, create_feed_reinforcement_traces, create_query_reinforcement_traces,
    create_signal_trace, filter_signal_feed_results, is_signal_capability,
    summarize_recent_signal_feed, summarize_signal_traces,
};
use crate::presence::{
    DEFAULT_PRESENCE_TTL_MINUTES, PresenceTraceConfig, create_presence_trace,
    is_presence_capability, summarize_recent_presence,
};
use crate::storage::TraceStore;
use crate::trace::{EvidenceVerification, MethodCompliance, Outcome, Trace, TraceEvidence};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

// ── Context ──────────────────────────────────────────────────

/// Shared context for all service operations.
/// Field is optional: MCP has it, HTTP doesn't.
pub struct Ctx<'a> {
    pub store: &'a TraceStore,
    pub field: Option<&'a PheromoneField>,
    pub identity: &'a NodeIdentity,
    pub binding: &'a IdentityBinding,
}

// ── Helpers ──────────────────────────────────────────────────

/// Project manifest filenames used to detect package boundaries when
/// there is no `.git` above the current path.
const PROJECT_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// Derive a project-identity space string from a path.
///
/// Walks up `path.ancestors()` looking for (in priority order):
///   1. a `.git` file or directory — the repo root (authoritative)
///   2. a common project manifest (Cargo.toml, package.json, …)
///
/// If neither is found, falls back to the last two path components
/// joined with `/` (the legacy heuristic).
///
/// Intent: one project = one space. Subdirectories of the same project
/// map to the same space string so their traces cross-reinforce at
/// Level 1.
pub fn derive_project_space(path: &Path) -> Option<String> {
    for dir in path.ancestors() {
        if dir.join(".git").exists() {
            return dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.is_empty());
        }
    }
    for dir in path.ancestors() {
        if PROJECT_MANIFESTS.iter().any(|m| dir.join(m).exists()) {
            return dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|s| !s.is_empty());
        }
    }
    let mut parts = path.components().rev().take(2).collect::<Vec<_>>();
    parts.reverse();
    let space: String = parts
        .iter()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if space.is_empty() { None } else { Some(space) }
}

/// Derive space from cwd. Delegates to [`derive_project_space`].
pub fn space_from_cwd() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    derive_project_space(&cwd)
}

pub fn parse_outcome(s: &str) -> Outcome {
    match s {
        "succeeded" | "success" => Outcome::Succeeded,
        "failed" | "fail" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "timeout" => Outcome::Timeout,
        _ => Outcome::Succeeded,
    }
}

pub fn trace_id_hex(trace: &Trace) -> String {
    trace.id[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn sign_config(ctx: &Ctx) -> (Option<String>, Option<String>) {
    (
        ctx.binding.owner_account.clone(),
        Some(ctx.binding.device_identity.clone()),
    )
}

// ── Trace Record ─────────────────────────────────────────────

pub struct RecordTraceReq {
    pub capability: String,
    pub outcome: Outcome,
    pub latency_ms: u32,
    pub input_size: u32,
    pub context: String,
    pub model: String,
    pub session_id: Option<String>,
    pub space: Option<String>,
    pub agent_id: Option<String>,
    pub sigil_id: Option<String>,
    pub method_compliance: Option<MethodCompliance>,
    pub evidence: TraceEvidence,
}

pub struct RecordTraceOut {
    pub trace_id: String,
    pub capability: String,
    pub trace: Trace,
}

pub use crate::continuity::ExternalContinuityRecordResult as RecordContinuityOut;

pub enum RecordResult {
    Trace(RecordTraceOut),
    Continuity(RecordContinuityOut),
}

/// Record a trace or external continuity event.
/// Returns the trace so callers can do post-processing (field excite, network publish).
pub fn record_trace(
    ctx: &Ctx,
    req: RecordTraceReq,
    external_continuity: Option<ExternalContinuityInput>,
) -> Result<RecordResult, String> {
    let RecordTraceReq {
        capability,
        outcome,
        latency_ms,
        input_size,
        context,
        model,
        session_id,
        space,
        agent_id,
        sigil_id,
        method_compliance,
        evidence,
    } = req;

    // External continuity path
    if let Some(input) = external_continuity {
        input.validate()?;
        let result = record_external_continuity(
            ctx.store,
            ctx.identity,
            &input,
            ExternalContinuityRecordConfig {
                owner_account: ctx.binding.owner_account.clone(),
                device_identity: ctx.binding.device_identity.clone(),
                outcome,
                model_id: model,
                session_id,
            },
        )?;
        return Ok(RecordResult::Continuity(result));
    }

    // Normal trace path
    let context_hash = simhash(&context);
    let context_text = if context.is_empty() {
        None
    } else {
        Some(context)
    };
    let (owner, device) = sign_config(ctx);

    let trace = Trace::new_with_agent_compliance_evidence(
        capability.clone(),
        outcome,
        latency_ms,
        input_size,
        context_hash,
        context_text,
        session_id,
        owner,
        device,
        agent_id,
        sigil_id,
        method_compliance,
        normalize_evidence(evidence),
        model,
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );

    let tid = trace_id_hex(&trace);
    ctx.store
        .insert_with_space(&trace, space.as_deref())
        .map_err(|e| format!("storage: {e}"))?;

    // NOTE: We do not excite the pheromone field here. The field is a
    // derived view of the store — the daemon's `pheromone_tail` loop
    // picks up new rows and excites the field exactly once. This keeps
    // hook-path traces (which insert directly to SQLite) and MCP-path
    // traces (which go through this function) on a single excitation
    // pipeline, eliminating the divergence that left the field stuck
    // for ~7 days under v1.x.

    // ── Outcome reflexivity: auto-avoid on repeated failures ──
    if trace.outcome == Outcome::Failed && !is_signal_capability(&trace.capability) {
        outcome_reflexivity(ctx, &trace);
    }

    Ok(RecordResult::Trace(RecordTraceOut {
        trace_id: tid,
        capability: trace.capability.clone(),
        trace,
    }))
}

fn normalize_evidence(mut evidence: TraceEvidence) -> TraceEvidence {
    evidence.artifact_refs = evidence
        .artifact_refs
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .take(16)
        .map(|value| {
            if value.len() > 512 {
                value.chars().take(512).collect()
            } else {
                value
            }
        })
        .collect();
    evidence.trial_key = evidence
        .trial_key
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.len() > 128 {
                value.chars().take(128).collect()
            } else {
                value
            }
        });
    evidence
}

pub fn parse_evidence_from_json(args: &serde_json::Value) -> Result<TraceEvidence, String> {
    let mut evidence = TraceEvidence::default();
    if let Some(value) = args.get("artifact_refs") {
        let Some(items) = value.as_array() else {
            return Err("artifact_refs must be an array of strings".into());
        };
        let mut refs = Vec::new();
        for item in items {
            let Some(rendered) = item.as_str() else {
                return Err("artifact_refs must be an array of strings".into());
            };
            refs.push(rendered.to_string());
        }
        evidence.artifact_refs = refs;
    }
    evidence.trial_key = args
        .get("trial_key")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    evidence.verification = args
        .get("verification")
        .and_then(|value| value.as_str())
        .map(|value| {
            EvidenceVerification::parse(value).ok_or_else(|| {
                "verification must be one of test, replay, eval, human, none".to_string()
            })
        })
        .transpose()?;
    Ok(normalize_evidence(evidence))
}

pub fn space_learning_view(
    store: &TraceStore,
    local_feedback: &crate::workspace::SpaceFeedbackSummary,
    space: &str,
    hours: u32,
    limit: usize,
) -> SpaceLearningView {
    let fetch_limit = limit.max(1).saturating_mul(80).max(200);
    let traces = store
        .recent_agent_traces_for_space(hours as u64, fetch_limit, Some(space))
        .unwrap_or_default();
    crate::learning::view_from_traces(&traces, local_feedback, limit)
}

// ── Outcome Reflexivity ─────────────────────────────────────
//
// When a trace fails, check if the same context has been failing
// repeatedly. If so, auto-generate an avoid signal.
// The avoid is low-power (auto-derived, needs corroboration to promote).

/// Minimum traces in context window before checking failure rate.
const REFLEXIVITY_MIN_TRACES: u32 = 3;
/// Failure rate threshold to trigger auto-avoid.
const REFLEXIVITY_FAILURE_THRESHOLD: f64 = 0.5;
/// TTL for auto-generated avoid signals (short — gives room for recovery).
const REFLEXIVITY_AVOID_TTL_HOURS: u32 = 12;

fn outcome_reflexivity(ctx: &Ctx, failed_trace: &Trace) {
    let stats = match ctx.store.residue_stats_for_context(
        &failed_trace.context_hash,
        8,    // Hamming distance
        1,    // last 1 hour
        20,   // up to 20 traces
        None, // all spaces
    ) {
        Ok(s) => s,
        Err(_) => return,
    };

    let total = stats.total_success() + stats.total_failure();
    if total < REFLEXIVITY_MIN_TRACES {
        return;
    }

    let failure_rate = stats.total_failure() as f64 / total as f64;
    if failure_rate < REFLEXIVITY_FAILURE_THRESHOLD {
        return;
    }

    let context = failed_trace
        .context_text
        .as_deref()
        .unwrap_or(&failed_trace.capability);
    let message = format!(
        "auto: repeated failures ({}/{} in context, {:.0}% failure rate)",
        stats.total_failure(),
        total,
        failure_rate * 100.0,
    );

    let (owner, device) = sign_config(ctx);
    let signal = create_auto_signal_trace(
        SignalPostKind::Avoid,
        context,
        &message,
        SignalTraceConfig {
            model_id: String::new(), // overridden by create_auto_signal_trace
            session_id: failed_trace.session_id.clone(),
            owner_account: owner,
            device_identity: device,
            agent_id: None,
            sigil_id: failed_trace.sigil_id.clone(),
            space: None,
            ttl_hours: REFLEXIVITY_AVOID_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );

    // Best-effort insert — don't fail the original trace recording
    let _ = ctx.store.insert(&signal);
}

// ── Signal Post ──────────────────────────────────────────────

pub struct PostSignalReq {
    pub kind: SignalPostKind,
    pub context: String,
    pub message: String,
    pub tool_name: Option<String>,
    pub space: Option<String>,
    pub model: String,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub sigil_id: Option<String>,
    pub ttl_hours: Option<u32>,
}

#[derive(Serialize)]
pub struct PostSignalOut {
    pub posted: bool,
    pub kind: String,
    pub message: String,
    pub space: Option<String>,
    pub ttl_hours: u32,
    pub ttl_source: &'static str,
    pub trace_id: String,
}

pub fn post_signal(ctx: &Ctx, req: PostSignalReq) -> Result<PostSignalOut, String> {
    let context = match &req.tool_name {
        Some(name) => format_signal_context(Some(name), &req.context),
        None => req.context,
    };
    let explicit_ttl = req.ttl_hours;
    let ttl_hours = explicit_ttl.unwrap_or_else(|| req.kind.default_ttl_hours());
    let (owner, device) = sign_config(ctx);

    let trace = create_signal_trace(
        req.kind,
        &context,
        &req.message,
        SignalTraceConfig {
            model_id: req.model,
            session_id: req.session_id,
            owner_account: owner,
            device_identity: device,
            agent_id: req.agent_id,
            sigil_id: req.sigil_id,
            space: req.space.clone(),
            ttl_hours,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );
    let tid = trace_id_hex(&trace);

    ctx.store
        .insert(&trace)
        .map_err(|e| format!("storage: {e}"))?;

    Ok(PostSignalOut {
        posted: true,
        kind: req.kind.as_str().to_string(),
        message: req.message,
        space: req.space,
        ttl_hours,
        ttl_source: if explicit_ttl.is_some() {
            "explicit"
        } else {
            "kind_default"
        },
        trace_id: tid,
    })
}

// ── Presence Ping ────────────────────────────────────────────

pub struct PingPresenceReq {
    pub space: Option<String>,
    pub mode: Option<String>,
    pub model: String,
    pub session_id: Option<String>,
    pub sigil_id: Option<String>,
    pub capability: Option<String>,
    pub ttl_minutes: Option<u32>,
}

#[derive(Serialize)]
pub struct PingPresenceOut {
    pub active: bool,
    pub space: Option<String>,
    pub mode: Option<String>,
    pub ttl_minutes: u32,
    pub trace_id: String,
}

pub fn ping_presence(ctx: &Ctx, req: PingPresenceReq) -> Result<PingPresenceOut, String> {
    let ttl_minutes = req.ttl_minutes.unwrap_or(DEFAULT_PRESENCE_TTL_MINUTES);
    let (owner, device) = sign_config(ctx);

    let trace = create_presence_trace(
        PresenceTraceConfig {
            model_id: req.model,
            session_id: req.session_id,
            owner_account: owner,
            device_identity: device,
            space: req.space.clone(),
            mode: req.mode.clone(),
            sigil_id: req.sigil_id,
            capability: req.capability,
            ttl_minutes,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    );
    let tid = trace_id_hex(&trace);

    ctx.store
        .insert(&trace)
        .map_err(|e| format!("storage: {e}"))?;

    Ok(PingPresenceOut {
        active: true,
        space: req.space,
        mode: req.mode,
        ttl_minutes,
        trace_id: tid,
    })
}

// ── Signal Query ─────────────────────────────────────────────

pub struct QuerySignalsReq<'a> {
    pub context: &'a str,
    pub kind: Option<SignalPostKind>,
    pub limit: usize,
    pub space: Option<&'a str>,
}

pub fn query_signals(ctx: &Ctx, req: QuerySignalsReq) -> Result<Value, String> {
    let context_hash = simhash(req.context);
    let traces = ctx
        .store
        .query_signal_traces(&context_hash, req.kind, 48, req.limit, req.space)
        .map_err(|e| format!("query: {e}"))?;

    let results = summarize_signal_traces(&traces, req.context, req.limit);

    // Record reinforcement traces
    let (owner, device) = sign_config(ctx);
    for trace in create_query_reinforcement_traces(
        &results,
        req.context,
        SignalTraceConfig {
            model_id: "thronglets-query".into(),
            session_id: None,
            owner_account: owner.clone(),
            device_identity: device.clone(),
            agent_id: None,
            sigil_id: None,
            space: None,
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = ctx.store.insert(&trace);
    }

    Ok(json!({ "signals": results }))
}

// ── Signal Feed ──────────────────────────────────────────────

pub struct SignalFeedReq<'a> {
    pub hours: u32,
    pub kind: Option<SignalPostKind>,
    pub min_sources: u32,
    pub limit: usize,
    pub space: Option<&'a str>,
}

pub fn signal_feed(ctx: &Ctx, req: SignalFeedReq) -> Result<Value, String> {
    let traces = ctx
        .store
        .query_recent_signal_traces(req.hours, req.kind, req.limit, req.space)
        .map_err(|e| format!("query: {e}"))?;

    let results = filter_signal_feed_results(
        summarize_recent_signal_feed(&traces, req.limit),
        req.min_sources,
    );

    // Record reinforcement traces
    let (owner, device) = sign_config(ctx);
    for trace in create_feed_reinforcement_traces(
        &results,
        SignalTraceConfig {
            model_id: "thronglets-feed".into(),
            session_id: None,
            owner_account: owner.clone(),
            device_identity: device.clone(),
            agent_id: None,
            sigil_id: None,
            space: None,
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = ctx.store.insert(&trace);
    }

    Ok(json!({ "signals": results }))
}

// ── Presence Feed ────────────────────────────────────────────

pub fn presence_feed(
    ctx: &Ctx,
    hours: u32,
    limit: usize,
    space: Option<&str>,
) -> Result<Value, String> {
    let fetch_limit = if space.is_some() {
        limit.max(1).saturating_mul(10)
    } else {
        limit
    };
    let traces = ctx
        .store
        .query_recent_presence_traces(hours, fetch_limit)
        .map_err(|e| format!("query: {e}"))?;

    let results = summarize_recent_presence(&traces, space, limit);
    let attributed = results.iter().filter(|r| r.sigil_id.is_some()).count();
    let anonymous = results.len() - attributed;

    Ok(json!({
        "sessions": results,
        "attributed_count": attributed,
        "anonymous_count": anonymous,
    }))
}

// ── Substrate: Resolve ───────────────────────────────────────

pub fn resolve(ctx: &Ctx, context_str: &str, limit: usize) -> Result<Value, String> {
    let context_hash = simhash(context_str);

    // Primary: pheromone field scan with level fallback (if available)
    if let Some(field) = ctx.field {
        let scans = field.scan_with_fallback(
            &context_hash,
            None, // no space — let fallback walk all levels
            crate::target_kind::extract_file_path(context_str),
            limit,
        );
        if !scans.is_empty() {
            let capabilities: Vec<Value> = scans
                .iter()
                .map(|s| {
                    let store_sr = ctx
                        .store
                        .aggregate(&s.capability)
                        .ok()
                        .flatten()
                        .map(|ss| ss.success_rate)
                        .unwrap_or(s.valence);
                    json!({
                        "capability": s.capability,
                        "context_similarity": round2(s.context_similarity),
                        "success_rate": round2(store_sr),
                        "p50_latency_ms": s.latency.round() as u64,
                        "total_traces": s.total_excitations,
                        "field_intensity": round2(s.intensity),
                        "level": format!("{:?}", s.level),
                    })
                })
                .collect();
            return Ok(json!({ "capabilities": capabilities }));
        }
    }

    // Fallback: cold store query
    let traces = ctx
        .store
        .query_similar(&context_hash, 48, limit * 10)
        .map_err(|e| format!("query: {e}"))?;

    let mut cap_groups: HashMap<&str, Vec<&Trace>> = HashMap::new();
    for t in &traces {
        if is_signal_capability(&t.capability) || is_presence_capability(&t.capability) {
            continue;
        }
        cap_groups.entry(&t.capability).or_default().push(t);
    }

    let mut capabilities: Vec<Value> = cap_groups
        .iter()
        .map(|(cap, group)| {
            let total = group.len() as u64;
            let successes = group
                .iter()
                .filter(|t| matches!(t.outcome, Outcome::Succeeded))
                .count() as f64;
            let success_rate = if total > 0 {
                successes / total as f64
            } else {
                0.0
            };
            let best_sim = group
                .iter()
                .map(|t| similarity(&context_hash, &t.context_hash))
                .fold(0.0_f64, f64::max);

            json!({
                "capability": cap,
                "context_similarity": round2(best_sim),
                "success_rate": round2(success_rate),
                "total_traces": total,
            })
        })
        .collect();

    capabilities.sort_by(|a, b| {
        b["context_similarity"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["context_similarity"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    capabilities.truncate(limit);

    Ok(json!({ "capabilities": capabilities }))
}

// ── Substrate: Evaluate ──────────────────────────────────────

pub fn evaluate(ctx: &Ctx, capability: &str, limit: usize) -> Result<Value, String> {
    let store_stats = ctx.store.aggregate(capability);
    let field_agg = ctx.field.and_then(|f| f.aggregate(capability));

    // Per-model breakdown from store
    let traces = ctx
        .store
        .query_capability(capability, limit.max(1000))
        .map_err(|e| format!("query: {e}"))?;

    let stats_json = match (store_stats, field_agg) {
        (Ok(Some(ss)), Some(fa)) => json!({
            "total_traces": ss.total_traces,
            "success_rate": round2(ss.success_rate),
            "p50_latency_ms": ss.p50_latency_ms,
            "field_intensity": round2(fa.intensity),
            "variance": round2(fa.variance),
        }),
        (Ok(Some(ss)), None) => json!({
            "total_traces": ss.total_traces,
            "success_rate": round2(ss.success_rate),
            "p50_latency_ms": ss.p50_latency_ms,
            "p95_latency_ms": ss.p95_latency_ms,
            "confidence": round2(ss.confidence),
        }),
        (_, Some(fa)) => json!({
            "total_traces": fa.total_excitations,
            "success_rate": round2(fa.valence),
            "field_intensity": round2(fa.intensity),
        }),
        _ => {
            return Ok(json!({
                "capability": capability,
                "stats": null,
                "by_model": {},
            }));
        }
    };

    // Group by model_id
    let mut by_model: HashMap<&str, (u64, u64)> = HashMap::new();
    for t in &traces {
        let entry = by_model.entry(&t.model_id).or_insert((0, 0));
        entry.0 += 1;
        if matches!(t.outcome, Outcome::Succeeded) {
            entry.1 += 1;
        }
    }
    let model_stats: HashMap<&str, Value> = by_model
        .iter()
        .map(|(model, (total, successes))| {
            let rate = if *total > 0 {
                *successes as f64 / *total as f64
            } else {
                0.0
            };
            (
                *model,
                json!({ "success_rate": round2(rate), "count": total }),
            )
        })
        .collect();

    Ok(json!({
        "capability": capability,
        "stats": stats_json,
        "by_model": model_stats,
    }))
}

// ── Substrate: Explore ───────────────────────────────────────

pub fn explore(ctx: &Ctx, context_str: &str, limit: usize) -> Result<Value, String> {
    let mut capabilities: Vec<Value> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();

    // Primary: pheromone field capabilities (if available)
    if let Some(field) = ctx.field {
        let field_caps = field.capabilities(limit);
        if !field_caps.is_empty() {
            for s in &field_caps {
                if is_signal_capability(&s.capability) || is_presence_capability(&s.capability) {
                    continue;
                }
                let store_sr = ctx
                    .store
                    .aggregate(&s.capability)
                    .ok()
                    .flatten()
                    .map(|ss| ss.success_rate)
                    .unwrap_or(s.valence);
                capabilities.push(json!({
                    "capability": s.capability,
                    "total_traces": s.total_excitations,
                    "success_rate": round2(store_sr),
                    "p50_latency_ms": s.latency.round() as u64,
                    "field_intensity": round2(s.intensity),
                }));
                if store_sr < 0.5 {
                    gaps.push(format!("low success rate for {}", s.capability));
                }
            }

            if !capabilities.is_empty() {
                return Ok(json!({ "capabilities": capabilities, "gaps": gaps }));
            }
        }
    }

    // Fallback: cold store
    let caps = ctx
        .store
        .distinct_capabilities(limit)
        .map_err(|e| format!("query: {e}"))?;

    let context_hash = simhash(context_str);

    for cap in &caps {
        if is_signal_capability(cap) || is_presence_capability(cap) {
            continue;
        }
        if let Ok(Some(stats)) = ctx.store.aggregate(cap) {
            let traces = ctx.store.query_capability(cap, 10).unwrap_or_default();
            let best_sim = traces
                .iter()
                .map(|t| similarity(&context_hash, &t.context_hash))
                .fold(0.0_f64, f64::max);

            capabilities.push(json!({
                "capability": cap,
                "total_traces": stats.total_traces,
                "success_rate": round2(stats.success_rate),
                "p50_latency_ms": stats.p50_latency_ms,
                "context_similarity": round2(best_sim),
            }));

            if stats.success_rate < 0.5 {
                gaps.push(format!("low success rate for {}", cap));
            }
        }
    }

    if capabilities.is_empty() && !context_str.is_empty() {
        gaps.push(format!(
            "no capabilities found matching context: {}",
            context_str
        ));
    }

    Ok(json!({ "capabilities": capabilities, "gaps": gaps }))
}

// ── Field Clusters ──────────────────────────────────────────

pub fn field_clusters(ctx: &Ctx, min_weight: f64) -> Result<Value, String> {
    let field = ctx.field.ok_or("field not available (local-only mode)")?;
    let clusters = field.clusters(min_weight);
    let cluster_json: Vec<Value> = clusters
        .iter()
        .map(|c| {
            json!({
                "capabilities": c.capabilities,
                "total_weight": round2(c.total_weight),
                "edge_count": c.edge_count,
                "level": c.level,
                "bucket": c.bucket,
            })
        })
        .collect();
    Ok(json!({ "clusters": cluster_json }))
}

#[cfg(test)]
mod derive_project_space_tests {
    use super::derive_project_space;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detects_git_root_from_nested_subdir() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("my-project");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        let nested = repo.join("src").join("deep").join("nested");
        fs::create_dir_all(&nested).unwrap();

        let space = derive_project_space(&nested).expect("space");
        assert_eq!(space, "my-project");
    }

    #[test]
    fn git_as_file_also_detected() {
        // Worktrees and submodules store `.git` as a file pointing to the real dir.
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("worktree-proj");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join(".git"), "gitdir: /elsewhere\n").unwrap();
        let subdir = repo.join("sub");
        fs::create_dir_all(&subdir).unwrap();

        let space = derive_project_space(&subdir).expect("space");
        assert_eq!(space, "worktree-proj");
    }

    #[test]
    fn innermost_git_root_wins() {
        // Nested git repos — the inner repo is a distinct unit.
        let dir = TempDir::new().unwrap();
        let outer = dir.path().join("outer");
        let inner = outer.join("libs").join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::create_dir_all(outer.join(".git")).unwrap();
        fs::create_dir_all(inner.join(".git")).unwrap();
        let nested = inner.join("src");
        fs::create_dir_all(&nested).unwrap();

        let space = derive_project_space(&nested).expect("space");
        assert_eq!(space, "inner");
    }

    #[test]
    fn falls_back_to_manifest_when_no_git() {
        let dir = TempDir::new().unwrap();
        let proj = dir.path().join("rust-proj");
        let src = proj.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();

        let space = derive_project_space(&src).expect("space");
        assert_eq!(space, "rust-proj");
    }

    #[test]
    fn falls_back_to_two_components_when_no_markers() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();

        // Pure fallback: last 2 components joined by "/".
        let space = derive_project_space(&nested).expect("space");
        assert_eq!(space, "b/c");
    }

    #[test]
    fn git_wins_over_manifest() {
        // If both .git and Cargo.toml are present at the same level, git wins.
        // Intentional — repo scope is the stronger signal.
        let dir = TempDir::new().unwrap();
        let proj = dir.path().join("mixed");
        let sub = proj.join("crate-a");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::write(sub.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

        let space = derive_project_space(&sub).expect("space");
        assert_eq!(space, "mixed");
    }
}
