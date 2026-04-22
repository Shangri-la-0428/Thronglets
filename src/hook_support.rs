use std::path::Path;
use std::time::Instant;

use crate::responses::ReleaseBaselineCheck;
use thronglets::active_policy::{ActivePolicySet, PolicyStrength};
use thronglets::ambient::{AmbientPolicyState, AmbientPriorProjection};
use thronglets::context::simhash;
use thronglets::continuity::{
    ContinuityEvent, ContinuityTaxonomy, ExternalContinuityInput, ExternalContinuityRecordConfig,
    record_external_continuity,
};
use thronglets::eval::{
    EvalBaselineComparison, EvalCheckStatus, EvalCheckThresholds, EvalConfig, LocalFeedbackSummary,
    SignalEvalSummary, evaluate_signal_quality,
};
use thronglets::identity::{IdentityBinding, NodeIdentity};
use thronglets::posts::{
    SignalPostKind, SignalTraceConfig, create_signal_trace, summarize_signal_traces,
};
use thronglets::presence::summarize_recent_presence;
use thronglets::signals::{Recommendation, Signal, SignalKind, StepCandidate};
use thronglets::storage::TraceStore;
use thronglets::trace::Outcome;

pub(crate) fn git_file_history(file_path: &str, max_entries: usize) -> Option<String> {
    use std::path::Path;
    use std::process::Command;

    let path = Path::new(file_path);
    let dir = path.parent()?;

    let output = Command::new("git")
        .args([
            "log",
            "--oneline",
            "--no-decorate",
            &format!("-n{}", max_entries),
            "--format=%ar\t%s",
            "--",
            file_path,
        ])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let log = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    let mut result = format!("  git history for {}:", path.file_name()?.to_str()?);
    for line in &lines {
        result.push_str(&format!("\n    {line}"));
    }
    Some(result)
}

pub(crate) fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn derive_space(payload: &serde_json::Value) -> Option<String> {
    // Explicit space in payload takes priority
    if let Some(s) = payload_string(payload, "space") {
        return Some(s);
    }
    // Fall back to working directory
    thronglets::service::space_from_cwd()
}

pub(crate) fn apply_network_sources(
    candidate: &mut StepCandidate,
    score: &mut i32,
    network_sources: u32,
) {
    *score += candidate.apply_network_sources(network_sources);
}

pub(crate) fn explicit_signals(
    store: &TraceStore,
    hook_context: &str,
    context_hash: &[u8; 16],
    space: Option<&str>,
) -> Vec<Signal> {
    let traces = match store.query_signal_traces(context_hash, None, 48, 10, space) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let results = summarize_signal_traces(&traces, hook_context, 6);

    let mut signals = Vec::new();
    for result in results {
        if result.density_score < 1 || result.context_similarity < 0.85 {
            continue;
        }
        let density_bonus = i32::from(result.density_score) * 20;

        match result.kind.as_str() {
            "avoid" => {
                let score = 300 + density_bonus;
                signals.push(Signal::danger(
                    format!("  ⚠ avoid: {}", result.message),
                    score,
                ));
            }
            "watch" => {
                let score = 200 + density_bonus;
                signals.push(Signal {
                    kind: SignalKind::History,
                    score,
                    body: format!("  👁 watch: {}", result.message),
                    candidate: None,
                });
            }
            "recommend" => {
                let score = 150 + density_bonus;
                signals.push(Signal::preparation(
                    format!("  ✦ recommended: {}", result.message),
                    score,
                ));
            }
            _ => {}
        }
    }
    signals
}

pub(crate) fn history_signal_from_projection(prior: AmbientPriorProjection) -> Option<Signal> {
    let (score, body) = match (prior.kind, prior.policy_state) {
        ("success-prior", Some(AmbientPolicyState::StablePath)) => (
            140 + (prior.confidence * 60.0).round() as i32,
            format!(
                "  ✓ stable path: {}",
                strip_success_prior_prefix(&prior.summary)
            ),
        ),
        ("mixed-residue", Some(AmbientPolicyState::PolicyConflict)) => {
            (235, format!("  ! {}", prior.summary))
        }
        ("mixed-residue", Some(AmbientPolicyState::MethodConflict)) => {
            (205, format!("  ~ {}", prior.summary))
        }
        ("mixed-residue", _) => (
            190,
            format!(
                "  ~ unsettled path: {}",
                strip_mixed_residue_prefix(&prior.summary)
            ),
        ),
        _ => return None,
    };
    Some(Signal {
        kind: SignalKind::History,
        score,
        body,
        candidate: None,
    })
}

fn strip_success_prior_prefix(summary: &str) -> &str {
    summary
        .strip_prefix("shared success prior: ")
        .or_else(|| summary.strip_prefix("prior success: "))
        .unwrap_or(summary)
}

fn strip_mixed_residue_prefix(summary: &str) -> &str {
    summary.strip_prefix("mixed residue: ").unwrap_or(summary)
}

pub(crate) fn active_policy_signal(active_policy: &ActivePolicySet) -> Option<Signal> {
    if active_policy.relevant_rules.is_empty() {
        return None;
    }
    let summaries = active_policy
        .relevant_rules
        .iter()
        .take(2)
        .map(|rule| rule.summary.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let hard = active_policy
        .relevant_rules
        .iter()
        .any(|rule| rule.strength == PolicyStrength::Hard);
    // Both hard and soft policy rules are contextual guidance, not
    // predictions of tool failure. Using History/Context prevents the
    // feedback system from scoring them as "wrong" when the tool succeeds.
    // Hard rules get higher score to outrank other context signals.
    let score = if hard { 340 } else { 205 };
    let prefix = if hard { "!" } else { "~" };
    Some(Signal {
        kind: SignalKind::History,
        score,
        body: format!("  {prefix} active policy: {summaries}"),
        candidate: None,
    })
}

/// Derive co-edit signals lazily at Prehook time.
/// Replaces the eager Hebbian co-edit block that was in the Hook handler.
pub(crate) fn co_edit_signals(
    store: &TraceStore,
    current_file: &str,
    recent_actions: &std::collections::VecDeque<thronglets::workspace::RecentAction>,
    session_id: Option<&str>,
    space: Option<&str>,
) -> Vec<Signal> {
    let session_id = match session_id {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Find other files edited in the same session
    let mut co_files: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for action in recent_actions {
        if action.session_id.as_deref() == Some(session_id)
            && matches!(action.tool.as_str(), "Edit" | "Write")
            && action.outcome == "succeeded"
            && let Some(fp) = &action.file_path
            && fp != current_file
            && seen.insert(fp.clone())
        {
            co_files.push(fp.clone());
            if co_files.len() >= 5 {
                break;
            }
        }
    }

    let mut signals = Vec::new();
    let ctx_a = format!("edit file: {}", current_file);
    let hash_a = simhash(&ctx_a);

    for other_file in &co_files {
        let ctx_b = format!("edit file: {}", other_file);
        let hash_b = simhash(&ctx_b);

        if let Ok(co_count) = store.count_co_occurring_sessions(&hash_a, &hash_b, 168, space) {
            if co_count >= 2 {
                let short_name = std::path::Path::new(other_file.as_str())
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(other_file.as_str());
                signals.push(Signal::adjacency_candidate(
                    format!("  ~ co-edited: {} ({} sessions)", short_name, co_count),
                    150,
                    StepCandidate::single(
                        "Edit",
                        Some(other_file.clone()),
                        "co-edit",
                        co_count as u32,
                        1,
                    ),
                ));
            }
        }
    }
    signals
}

pub(crate) fn presence_context_signal(
    store: &TraceStore,
    space: &str,
    current_session_id: Option<&str>,
) -> Option<Signal> {
    let traces = store.query_recent_presence_traces(1, 24).ok()?;
    let active_sessions: Vec<_> = summarize_recent_presence(&traces, Some(space), 8)
        .into_iter()
        .filter(|session| {
            current_session_id.is_none_or(|sid| session.session_id.as_deref() != Some(sid))
        })
        .collect();

    if active_sessions.is_empty() {
        return None;
    }

    let preview_limit = 2;
    let preview = active_sessions
        .iter()
        .take(preview_limit)
        .map(|session| match session.mode.as_deref() {
            Some(mode) => format!("{} ({mode})", session.model_id),
            None => session.model_id.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let remainder = active_sessions.len().saturating_sub(preview_limit);
    let suffix = if remainder > 0 {
        format!(" +{remainder} more")
    } else {
        String::new()
    };

    Some(Signal {
        kind: SignalKind::History,
        score: 135,
        body: format!("  active in space {space}: {preview}{suffix}"),
        candidate: None,
    })
}

pub(crate) fn claim_secondary_query(
    candidate: &StepCandidate,
    remaining_queries: &mut usize,
) -> bool {
    if *remaining_queries == 0 || candidate.source_count >= 2 {
        return false;
    }

    *remaining_queries -= 1;
    true
}

pub(crate) fn cached_store<'a>(
    cache: &'a mut Option<TraceStore>,
    dir: &Path,
) -> Option<&'a TraceStore> {
    let db_path = dir.join("traces.db");
    if !db_path.exists() {
        return None;
    }

    if cache.is_none() {
        *cache = TraceStore::open(&db_path).ok();
    }

    cache.as_ref()
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ProfileStageState {
    Timed(u128),
    Skipped,
}

pub(crate) struct PrehookProfiler {
    enabled: bool,
    started_at: Instant,
    stage_started_at: Instant,
    stages: Vec<(&'static str, ProfileStageState)>,
}

impl PrehookProfiler {
    pub(crate) fn from_env() -> Self {
        let enabled =
            std::env::var_os("THRONGLETS_PROFILE_PREHOOK").is_some_and(|value| value != "0");
        let now = Instant::now();
        Self {
            enabled,
            started_at: now,
            stage_started_at: now,
            stages: Vec::new(),
        }
    }

    pub(crate) fn stage(&mut self, name: &'static str) {
        if !self.enabled {
            return;
        }

        let now = Instant::now();
        self.stages.push((
            name,
            ProfileStageState::Timed(now.duration_since(self.stage_started_at).as_micros()),
        ));
        self.stage_started_at = now;
    }

    pub(crate) fn stage_or_skip(&mut self, name: &'static str, executed: bool) {
        if !self.enabled {
            return;
        }

        if executed {
            self.stage(name);
        } else {
            self.stages.push((name, ProfileStageState::Skipped));
        }
    }

    pub(crate) fn finish(
        &self,
        tool_name: &str,
        recommendations: &[Recommendation],
        field_observation_count: usize,
        stdout_bytes: usize,
        file_guidance_gate: &'static str,
        secondary_queries_used: usize,
    ) {
        if !self.enabled {
            return;
        }

        let mut parts = vec![
            format!("tool={tool_name}"),
            format!("emitted={}", recommendations.len()),
            format!("field_observations={field_observation_count}"),
            format!("stdout_bytes={stdout_bytes}"),
            format!(
                "output_mode={}",
                profile_output_mode(recommendations, field_observation_count > 0)
            ),
            format!(
                "decision_path={}",
                profile_decision_path(recommendations, field_observation_count > 0)
            ),
            format!("file_guidance_gate={file_guidance_gate}"),
            format!("secondary_queries_used={secondary_queries_used}"),
            format!("total_us={}", self.started_at.elapsed().as_micros()),
        ];
        for (name, state) in &self.stages {
            match state {
                ProfileStageState::Timed(duration_us) => {
                    parts.push(format!("{name}_us={duration_us}"));
                }
                ProfileStageState::Skipped => {
                    parts.push(format!("{name}=skipped"));
                }
            }
        }
        eprintln!("[thronglets:prehook] {}", parts.join(" "));
    }
}

pub(crate) fn profile_output_mode(
    recommendations: &[Recommendation],
    has_field_observations: bool,
) -> &'static str {
    if recommendations.is_empty() {
        if has_field_observations {
            "field-only"
        } else {
            "silent"
        }
    } else if recommendations
        .iter()
        .any(|r| r.source_kind == SignalKind::Repair)
    {
        "next-step"
    } else if recommendations
        .iter()
        .any(|r| r.source_kind == SignalKind::Danger)
    {
        "caution"
    } else {
        "context-only"
    }
}

pub(crate) fn profile_decision_path(
    recommendations: &[Recommendation],
    has_field_observations: bool,
) -> &'static str {
    recommendations
        .iter()
        .find(|r| r.source_kind == SignalKind::Repair)
        .or_else(|| recommendations.first())
        .map(|r| match r.source_kind {
            SignalKind::Danger => "danger",
            SignalKind::Repair => "repair",
            SignalKind::Preparation | SignalKind::Adjacency => "legacy",
            SignalKind::History => "history",
        })
        .or_else(|| has_field_observations.then_some("field"))
        .unwrap_or("none")
}

pub(crate) fn profile_file_guidance_gate(supports_file_guidance: bool) -> &'static str {
    if supports_file_guidance { "open" } else { "na" }
}

pub(crate) fn strip_check_header(rendered: &str) -> String {
    rendered.lines().skip(1).collect::<Vec<_>>().join("\n")
}

pub(crate) fn run_release_eval_section(
    store: &TraceStore,
    hours: u64,
    max_sessions: usize,
    project_scope: Option<&Path>,
    local_feedback: Option<LocalFeedbackSummary>,
    thresholds: &EvalCheckThresholds,
    baseline: Option<&SignalEvalSummary>,
) -> (&'static str, bool, String, serde_json::Value) {
    match evaluate_signal_quality(
        store,
        hours,
        max_sessions,
        project_scope,
        EvalConfig::default(),
    )
    .expect("failed to evaluate signal quality")
    {
        Some(summary) => {
            let mut summary = summary.with_local_feedback(local_feedback);
            if let Some(baseline) = baseline {
                summary = summary.with_comparison_to_baseline(baseline);
            }
            let check = summary.check(thresholds);
            let (status, rendered) = summary.render_check(thresholds);
            let baseline_check =
                release_baseline_check(summary.comparison_to_baseline.as_ref(), check.status);
            let mut body = strip_check_header(&rendered);
            if let Some(comparison) = summary.comparison_to_baseline.as_ref() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(&render_release_baseline_outcome_line(comparison));
                body.push('\n');
                body.push_str(&render_release_baseline_signal_line(comparison));
                if !baseline_check.violations.is_empty() {
                    body.push('\n');
                    body.push_str(&format!(
                        "baseline violations: {}",
                        baseline_check.violations.join("; ")
                    ));
                } else if !baseline_check.notes.is_empty() {
                    body.push('\n');
                    body.push_str(&format!(
                        "baseline notes: {}",
                        baseline_check.notes.join("; ")
                    ));
                }
            }
            let effective_status =
                if matches!(status, EvalCheckStatus::Fail) || baseline_check.status == "FAIL" {
                    "FAIL"
                } else {
                    status.label()
                };
            (
                effective_status,
                effective_status == "FAIL",
                body,
                serde_json::json!({
                    "status": effective_status,
                    "thresholds": thresholds,
                    "summary": summary,
                    "check": check,
                    "baseline_check": baseline_check,
                }),
            )
        }
        None => {
            let notes =
                vec!["not enough recent session history to evaluate signals yet".to_string()];
            (
                "SKIP",
                false,
                "notes: not enough recent session history to evaluate signals yet".to_string(),
                serde_json::json!({
                    "status": "SKIP",
                    "thresholds": thresholds,
                    "summary": serde_json::Value::Null,
                    "check": {
                        "status": "Skip",
                        "violations": Vec::<String>::new(),
                        "notes": notes,
                    },
                    "baseline_check": serde_json::Value::Null,
                }),
            )
        }
    }
}

pub(crate) fn release_baseline_check(
    comparison: Option<&EvalBaselineComparison>,
    eval_status: EvalCheckStatus,
) -> ReleaseBaselineCheck {
    let Some(comparison) = comparison else {
        return ReleaseBaselineCheck {
            status: "SKIP",
            violations: Vec::new(),
            notes: Vec::new(),
        };
    };

    if matches!(eval_status, EvalCheckStatus::Skip) {
        return ReleaseBaselineCheck {
            status: "SKIP",
            violations: Vec::new(),
            notes: vec![
                "baseline comparison inactive because offline eval is still in SKIP".into(),
            ],
        };
    }

    let mut violations = Vec::new();
    if let Some(delta) = comparison.local_retention_delta_tenths_pp
        && delta < -crate::cmd::RELEASE_MAX_LOCAL_RETENTION_DROP_TENTHS_PP
    {
        violations.push(format!(
            "local edit retention regressed by {}",
            format_release_option_tenths_pp(Some(delta))
        ));
    }
    if comparison.failed_command_rate_delta_tenths_pp
        > crate::cmd::RELEASE_MAX_FAILED_COMMAND_RATE_RISE_TENTHS_PP
    {
        violations.push(format!(
            "failed command rate regressed by {}",
            format_release_tenths_pp(comparison.failed_command_rate_delta_tenths_pp)
        ));
    }
    if let Some(delta) = comparison.first_successful_change_latency_avg_delta_ms
        && delta > crate::cmd::RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS
    {
        violations.push(format!(
            "first successful change latency avg regressed by {}",
            format_release_option_ms(Some(delta))
        ));
    }
    if let Some(delta) = comparison.first_successful_change_latency_p50_delta_ms
        && delta > crate::cmd::RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS
    {
        violations.push(format!(
            "first successful change latency p50 regressed by {}",
            format_release_option_ms(Some(delta))
        ));
    }

    ReleaseBaselineCheck {
        status: if violations.is_empty() {
            "PASS"
        } else {
            "FAIL"
        },
        violations,
        notes: Vec::new(),
    }
}

pub(crate) fn render_release_baseline_outcome_line(comparison: &EvalBaselineComparison) -> String {
    format!(
        "vs baseline ({} scored): retention {}, failed cmds {}, first change avg {}, p50 {}",
        comparison.baseline_sessions_scored,
        format_release_option_tenths_pp(comparison.local_retention_delta_tenths_pp),
        format_release_tenths_pp(comparison.failed_command_rate_delta_tenths_pp),
        format_release_option_ms(comparison.first_successful_change_latency_avg_delta_ms),
        format_release_option_ms(comparison.first_successful_change_latency_p50_delta_ms),
    )
}

pub(crate) fn render_release_baseline_signal_line(comparison: &EvalBaselineComparison) -> String {
    format!(
        "vs baseline signals: silence {}, repair cov {}, repair step {}, repair exact {}, prep {}, adj {}",
        format_release_tenths_pp(comparison.edit_silence_rate_delta_tenths_pp),
        format_release_tenths_pp(comparison.repair_coverage_delta_tenths_pp),
        format_release_tenths_pp(comparison.repair_first_step_precision_delta_tenths_pp),
        format_release_tenths_pp(comparison.repair_exact_precision_delta_tenths_pp),
        format_release_tenths_pp(comparison.preparation_precision_delta_tenths_pp),
        format_release_tenths_pp(comparison.adjacency_precision_delta_tenths_pp),
    )
}

pub(crate) fn format_release_tenths_pp(delta: i32) -> String {
    format!("{:+}.{}pp", delta / 10, delta.abs() % 10)
}

pub(crate) fn format_release_option_tenths_pp(delta: Option<i32>) -> String {
    delta
        .map(format_release_tenths_pp)
        .unwrap_or_else(|| "n/a".into())
}

pub(crate) fn format_release_option_ms(delta: Option<i64>) -> String {
    match delta {
        Some(delta) => format!("{delta:+}ms"),
        None => "n/a".into(),
    }
}

pub(crate) fn run_release_doctor_section(
    home_dir: &Path,
    data_dir: &Path,
) -> (&'static str, bool, String, serde_json::Value) {
    let reports: Vec<_> = crate::adapter_ops::selected_adapters(crate::cli::AdapterArg::All)
        .into_iter()
        .map(|adapter| crate::setup_support::doctor_adapter(home_dir, data_dir, adapter))
        .collect();
    let summary =
        crate::adapter_ops::summarize_doctor_reports(crate::cli::AdapterArg::All, reports);
    let status = if summary.summary.status == "healthy" {
        "PASS"
    } else {
        "FAIL"
    };
    let mut lines = vec![format!("status: {}", summary.summary.status)];
    if summary.summary.restart_pending {
        lines.push("restart pending: yes".into());
    }
    for step in &summary.summary.next_steps {
        lines.push(format!("next: {step}"));
    }

    (
        status,
        status == "FAIL",
        lines.join("\n"),
        serde_json::json!({
            "status": status,
            "summary": summary.summary,
            "reports": summary.reports,
        }),
    )
}

pub(crate) fn load_eval_baseline(path: &Path) -> Result<SignalEvalSummary, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read baseline {}: {err}", path.display()))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse baseline {}: {err}", path.display()))
}

pub(crate) fn print_release_section(name: &str, status: &str, body: &str) {
    println!("{name}: {status}");
    for line in body.lines() {
        println!("  {line}");
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Psyche → Thronglets bridge ─────────────────────────────────
//
// Each substrate is sovereign. Psyche produces sparse exports as
// a byproduct of processing; Thronglets decides what to ingest.
// The hook is the natural bridge — it sees every MCP response and
// can route exports without either substrate knowing the other exists.

/// Ingest `throngletsExports` from a Psyche MCP response into the
/// Thronglets continuity store and signal layer.
/// Returns the number of exports successfully ingested.
pub(crate) fn bridge_psyche_exports(
    tool_response: &serde_json::Value,
    store: &TraceStore,
    identity: &NodeIdentity,
    binding: &IdentityBinding,
    session_id: Option<&str>,
    model: &str,
    space: Option<&str>,
) -> usize {
    let exports = extract_thronglets_exports(tool_response);
    if exports.is_empty() {
        return 0;
    }

    let mut ingested = 0;

    for export in &exports {
        let kind = export["kind"].as_str().unwrap_or("");
        let summary = build_export_summary(export);

        if kind == "self-state" || kind == "viability" {
            // Self-state / viability → psyche_state signal (ephemeral broadcast, not continuity)
            let key = export["key"].as_str().unwrap_or("");
            let context = format!("psyche:{kind}:{key}");
            let signal = create_signal_trace(
                SignalPostKind::PsycheState,
                &context,
                &summary,
                SignalTraceConfig {
                    model_id: model.into(),
                    session_id: session_id.map(String::from),
                    owner_account: binding.owner_account.clone(),
                    device_identity: Some(binding.device_identity.clone()),
                    agent_id: None,
                    sigil_id: None,
                    space: space.map(String::from),
                    ttl_hours: 6,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            if store.insert(&signal).is_ok() {
                ingested += 1;
            }
        } else if let Some((taxonomy, event)) = map_continuity_event(kind) {
            // Continuity events → record_external_continuity (local-only, never gossip)
            let input = ExternalContinuityInput {
                provider: "thronglets".into(),
                mode: "optional".into(),
                version: 1,
                taxonomy,
                event,
                summary,
                space: space.map(String::from),
                audit_ref: None,
            };
            if record_external_continuity(
                store,
                identity,
                &input,
                ExternalContinuityRecordConfig {
                    owner_account: binding.owner_account.clone(),
                    device_identity: binding.device_identity.clone(),
                    outcome: Outcome::Succeeded,
                    model_id: model.into(),
                    session_id: session_id.map(String::from),
                },
            )
            .is_ok()
            {
                ingested += 1;
            }
        }
    }

    ingested
}

fn extract_thronglets_exports(response: &serde_json::Value) -> Vec<serde_json::Value> {
    // Direct access — MCP responses may arrive as parsed JSON objects
    if let Some(arr) = response.get("throngletsExports").and_then(|v| v.as_array()) {
        return arr.clone();
    }

    // String response — parse and retry
    if let Some(s) = response.as_str()
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s)
        && let Some(arr) = parsed.get("throngletsExports").and_then(|v| v.as_array())
    {
        return arr.clone();
    }

    Vec::new()
}

fn map_continuity_event(kind: &str) -> Option<(ContinuityTaxonomy, ContinuityEvent)> {
    match kind {
        "relation-milestone" => Some((
            ContinuityTaxonomy::Coordination,
            ContinuityEvent::RelationMilestone,
        )),
        "open-loop-anchor" => Some((
            ContinuityTaxonomy::Coordination,
            ContinuityEvent::OpenLoopAnchor,
        )),
        "continuity-anchor" => Some((
            ContinuityTaxonomy::Continuity,
            ContinuityEvent::ContinuityAnchor,
        )),
        "writeback-calibration" => Some((
            ContinuityTaxonomy::Calibration,
            ContinuityEvent::WritebackCalibration,
        )),
        _ => None,
    }
}

fn build_export_summary(export: &serde_json::Value) -> String {
    match export["kind"].as_str().unwrap_or("") {
        "self-state" => export["summary"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                format!(
                    "O:{:.0} F:{:.0} B:{:.0} R:{:.0}",
                    export["order"].as_f64().unwrap_or(0.0),
                    export["flow"].as_f64().unwrap_or(0.0),
                    export["boundary"].as_f64().unwrap_or(0.0),
                    export["resonance"].as_f64().unwrap_or(0.0),
                )
            }),
        "relation-milestone" => format!(
            "phase={} trust={:.1} intimacy={:.1}",
            export["phase"].as_str().unwrap_or("?"),
            export["trust"].as_f64().unwrap_or(0.0),
            export["intimacy"].as_f64().unwrap_or(0.0),
        ),
        "open-loop-anchor" => {
            let loops = join_string_array(&export["loopTypes"]);
            format!(
                "loops=[{loops}] tension={:.2} carry={:.2}",
                export["unfinishedTension"].as_f64().unwrap_or(0.0),
                export["silentCarry"].as_f64().unwrap_or(0.0),
            )
        }
        "continuity-anchor" => {
            let loops = join_string_array(&export["activeLoopTypes"]);
            format!(
                "mode={} floor={:.2} loops=[{loops}]",
                export["continuityMode"].as_str().unwrap_or("?"),
                export["continuityFloor"].as_f64().unwrap_or(0.0),
            )
        }
        "writeback-calibration" => format!(
            "signal={} effect={} metric={} conf={:.2}",
            export["signal"].as_str().unwrap_or("?"),
            export["effect"].as_str().unwrap_or("?"),
            export["metric"].as_str().unwrap_or("?"),
            export["confidence"].as_f64().unwrap_or(0.0),
        ),
        _ => export.to_string(),
    }
}

fn join_string_array(value: &serde_json::Value) -> String {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use thronglets::signals::RecommendationKind;

    #[test]
    fn extract_from_json_object() {
        let response = json!({
            "throngletsExports": [
                {"kind": "self-state", "key": "self-state:O50:F80:B50:R70", "summary": "flowing"}
            ]
        });
        let exports = extract_thronglets_exports(&response);
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0]["kind"], "self-state");
    }

    #[test]
    fn extract_from_json_string() {
        // MCP responses arrive as JSON strings
        let inner = json!({
            "throngletsExports": [
                {"kind": "relation-milestone", "phase": "familiar", "trust": 50.5, "intimacy": 30.1}
            ]
        });
        let response = serde_json::Value::String(inner.to_string());
        let exports = extract_thronglets_exports(&response);
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0]["kind"], "relation-milestone");
    }

    #[test]
    fn extract_empty_when_missing() {
        assert!(extract_thronglets_exports(&json!({})).is_empty());
        assert!(extract_thronglets_exports(&json!("no json here")).is_empty());
        assert!(extract_thronglets_exports(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn extract_null_exports_returns_empty() {
        let response = json!({"throngletsExports": null});
        assert!(extract_thronglets_exports(&response).is_empty());
    }

    #[test]
    fn map_all_continuity_events() {
        use thronglets::continuity::{ContinuityEvent, ContinuityTaxonomy};
        assert_eq!(
            map_continuity_event("relation-milestone"),
            Some((
                ContinuityTaxonomy::Coordination,
                ContinuityEvent::RelationMilestone
            ))
        );
        assert_eq!(
            map_continuity_event("open-loop-anchor"),
            Some((
                ContinuityTaxonomy::Coordination,
                ContinuityEvent::OpenLoopAnchor
            ))
        );
        assert_eq!(
            map_continuity_event("continuity-anchor"),
            Some((
                ContinuityTaxonomy::Continuity,
                ContinuityEvent::ContinuityAnchor
            ))
        );
        assert_eq!(
            map_continuity_event("writeback-calibration"),
            Some((
                ContinuityTaxonomy::Calibration,
                ContinuityEvent::WritebackCalibration
            ))
        );
        assert!(map_continuity_event("self-state").is_none());
        assert!(map_continuity_event("unknown").is_none());
    }

    #[test]
    fn build_self_state_summary_uses_field() {
        let export = json!({"kind": "self-state", "summary": "flowing, attuned"});
        assert_eq!(build_export_summary(&export), "flowing, attuned");
    }

    #[test]
    fn build_self_state_summary_fallback() {
        let export = json!({"kind": "self-state", "order": 45.0, "flow": 78.0, "boundary": 47.0, "resonance": 72.0});
        assert_eq!(build_export_summary(&export), "O:45 F:78 B:47 R:72");
    }

    #[test]
    fn build_relation_milestone_summary() {
        let export = json!({"kind": "relation-milestone", "phase": "familiar", "trust": 50.5, "intimacy": 30.1});
        assert_eq!(
            build_export_summary(&export),
            "phase=familiar trust=50.5 intimacy=30.1"
        );
    }

    #[test]
    fn build_open_loop_summary() {
        let export = json!({
            "kind": "open-loop-anchor",
            "loopTypes": ["implicit-promise", "task-dependency"],
            "unfinishedTension": 0.67,
            "silentCarry": 0.12
        });
        assert_eq!(
            build_export_summary(&export),
            "loops=[implicit-promise,task-dependency] tension=0.67 carry=0.12"
        );
    }

    #[test]
    fn build_writeback_summary() {
        let export = json!({
            "kind": "writeback-calibration",
            "signal": "warmth", "effect": "amplified", "metric": "trust", "confidence": 0.85
        });
        assert_eq!(
            build_export_summary(&export),
            "signal=warmth effect=amplified metric=trust conf=0.85"
        );
    }

    #[test]
    fn field_only_output_profiles_as_field() {
        assert_eq!(profile_output_mode(&[], true), "field-only");
        assert_eq!(profile_decision_path(&[], true), "field");
    }

    #[test]
    fn repair_path_still_wins_when_field_output_is_present() {
        let recommendations = vec![Recommendation {
            kind: RecommendationKind::DoNext,
            source_kind: SignalKind::Repair,
            body: "  repair path".into(),
            candidate: None,
        }];

        assert_eq!(profile_output_mode(&recommendations, true), "next-step");
        assert_eq!(profile_decision_path(&recommendations, true), "repair");
    }

    #[test]
    fn bridge_full_integration() {
        // Simulate a full Psyche MCP response as JSON string (how hooks receive it)
        let psyche_response = json!({
            "systemContext": "",
            "dynamicContext": "...",
            "throngletsExports": [
                {
                    "kind": "self-state",
                    "subject": "session",
                    "primitive": "signal",
                    "key": "self-state:O50:F80:B50:R70",
                    "strength": 0.5,
                    "summary": "flowing, attuned",
                    "order": 50.0, "flow": 80.0, "boundary": 50.0, "resonance": 70.0
                },
                {
                    "kind": "relation-milestone",
                    "subject": "delegate",
                    "primitive": "signal",
                    "key": "milestone:_default:familiar",
                    "strength": 0.45,
                    "phase": "familiar", "trust": 50.5, "intimacy": 30.1
                }
            ]
        });
        let response_str = serde_json::Value::String(psyche_response.to_string());
        let exports = extract_thronglets_exports(&response_str);
        assert_eq!(exports.len(), 2);

        // Verify self-state is identified correctly
        assert_eq!(exports[0]["kind"], "self-state");
        assert!(map_continuity_event("self-state").is_none()); // → signal path

        // Verify relation-milestone maps to continuity
        assert_eq!(exports[1]["kind"], "relation-milestone");
        assert!(map_continuity_event("relation-milestone").is_some()); // → continuity path

        // Both produce valid summaries
        assert_eq!(build_export_summary(&exports[0]), "flowing, attuned");
        assert_eq!(
            build_export_summary(&exports[1]),
            "phase=familiar trust=50.5 intimacy=30.1"
        );
    }
}
