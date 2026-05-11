use crate::responses::*;
use crate::setup_support::{
    AdapterApplyResult, AdapterDetection, AdapterDoctor, AdapterKind, AdapterPlan,
};
use thronglets::continuity::ContinuitySnapshotSummary;
use thronglets::posts::{SignalFeedResult, SignalQueryResult};
use thronglets::presence::PresenceFeedResult;
use thronglets::workspace;

pub(crate) fn signal_hours_remaining(expires_at: u64) -> u64 {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if expires_at <= now_ms {
        0
    } else {
        (expires_at - now_ms).div_ceil(60 * 60 * 1000)
    }
}

pub(crate) fn presence_minutes_remaining(expires_at: u64) -> u64 {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if expires_at <= now_ms {
        0
    } else {
        (expires_at - now_ms).div_ceil(60 * 1000)
    }
}

pub(crate) fn render_detections(detections: &[AdapterDetection]) {
    println!("Detected adapters:");
    for detection in detections {
        println!(
            "  {}: present={} configurable={} integration={}",
            detection.agent,
            if detection.present { "yes" } else { "no" },
            if detection.configurable { "yes" } else { "no" },
            detection.integration
        );
        for path in &detection.paths {
            println!("    path: {path}");
        }
        if let Some(note) = &detection.note {
            println!("    note: {note}");
        }
    }
}

pub(crate) fn render_detect_report(data: &DetectData) {
    println!("Detect status: {}", data.summary.status);
    println!(
        "Detected: {}",
        if data.summary.detected_agents.is_empty() {
            "none".into()
        } else {
            data.summary.detected_agents.join(", ")
        }
    );
    if !data.summary.recommended_agents.is_empty() {
        println!(
            "Recommended: {}",
            data.summary.recommended_agents.join(", ")
        );
    }
    let attention: Vec<_> = data
        .detections
        .iter()
        .filter(|detection| detection.agent != AdapterKind::Generic.key())
        .filter(|detection| !detection.present || !detection.configurable)
        .cloned()
        .collect();
    if !attention.is_empty() {
        println!();
        render_detections(&attention);
    }
}

pub(crate) fn render_install_plans(plans: &[AdapterPlan]) {
    println!("Install plan:");
    for plan in plans {
        println!(
            "  {}: integration={} default={} restart={}",
            plan.agent,
            plan.integration,
            if plan.apply_by_default { "yes" } else { "no" },
            if plan.requires_restart { "yes" } else { "no" }
        );
        for action in &plan.actions {
            println!("    action: {action}");
        }
        if let Some(apply_command) = &plan.apply_command {
            println!("    apply: {apply_command}");
        }
        println!("    doctor: {}", plan.doctor_command);
    }
}

pub(crate) fn render_install_plan_report(data: &InstallPlanData) {
    println!("Install plan status: {}", data.summary.status);
    let planned_agents: Vec<_> = data.plans.iter().map(|plan| plan.agent.as_str()).collect();
    if !planned_agents.is_empty() {
        println!("Plan: {}", planned_agents.join(", "));
    }
    if data.summary.restart_required {
        println!("Restart required: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart: {command}");
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
    }
    if data.plans.iter().any(|plan| plan.contract.is_some()) {
        println!("Next: rerun with --json to inspect contract examples and runtime snippets.");
        println!();
        render_install_plans(&data.plans);
    }
}

pub(crate) fn render_signal_query_results(results: &[SignalQueryResult]) {
    if results.is_empty() {
        println!("No explicit signals found.");
        return;
    }

    println!("Explicit signals:");
    for result in results {
        println!("  {}: {}", result.kind, result.message,);
        if let Some(space) = &result.space {
            println!("    space: {space}");
        }
        let model_suffix = if result.model_count > 1 {
            format!(" models={}", result.model_count)
        } else {
            String::new()
        };
        let density_suffix = if result.density_tier != "sparse" {
            format!(" density={}", result.density_tier)
        } else {
            String::new()
        };
        let reinforcement_suffix = if result.reinforcement_count > 0 {
            format!(" reads={}", result.reinforcement_count)
        } else {
            String::new()
        };
        let inhibition_suffix = if result.inhibition_state != "none" {
            format!(" inhibited={}", result.inhibition_state)
        } else {
            String::new()
        };
        println!(
            "    similarity={:.2} posts={} sources={}{}{}{}{} expires_in≈{}h",
            result.context_similarity,
            result.total_posts,
            result.source_count,
            model_suffix,
            density_suffix,
            reinforcement_suffix,
            inhibition_suffix,
            signal_hours_remaining(result.expires_at)
        );
        for context in &result.contexts {
            println!("    context: {context}");
        }
    }
}

pub(crate) fn render_signal_feed_results(results: &[SignalFeedResult]) {
    if results.is_empty() {
        println!("No recent explicit signals found.");
        return;
    }

    let focused: Vec<_> = results
        .iter()
        .filter(|result| result.focus_tier != "background")
        .take(3)
        .collect();
    let display: Vec<_> = if focused.is_empty() {
        results.iter().take(3).collect()
    } else {
        focused
    };
    let omitted_count = results.len().saturating_sub(display.len());

    println!(
        "{}",
        if omitted_count > 0 {
            "Signal focus:"
        } else {
            "Recent explicit signals:"
        }
    );
    for result in display {
        println!("  {}: {}", result.kind, result.message);
        if let Some(space) = &result.space {
            println!("    space: {space}");
        }
        let model_suffix = if result.model_count > 1 {
            format!(" models={}", result.model_count)
        } else {
            String::new()
        };
        let focus_suffix = if result.focus_tier != "background" {
            format!(" focus={}", result.focus_tier)
        } else {
            String::new()
        };
        let density_suffix = if result.density_tier != "sparse" {
            format!(" density={}", result.density_tier)
        } else {
            String::new()
        };
        let reinforcement_suffix = if result.reinforcement_count > 0 {
            format!(" reads={}", result.reinforcement_count)
        } else {
            String::new()
        };
        let inhibition_suffix = if result.inhibition_state != "none" {
            format!(" inhibited={}", result.inhibition_state)
        } else {
            String::new()
        };
        println!(
            "    posts={} sources={}{}{}{}{}{} expires_in≈{}h",
            result.total_posts,
            result.source_count,
            model_suffix,
            focus_suffix,
            density_suffix,
            reinforcement_suffix,
            inhibition_suffix,
            signal_hours_remaining(result.expires_at)
        );
        for context in &result.contexts {
            println!("    context: {context}");
        }
    }
    if omitted_count > 0 {
        println!("  + {omitted_count} lower-signal entries omitted");
    }
}

pub(crate) fn render_presence_feed_results(results: &[PresenceFeedResult]) {
    if results.is_empty() {
        println!("No recent active sessions found.");
        return;
    }

    println!("Active sessions:");
    for result in results {
        let session = result.session_id.as_deref().unwrap_or("unknown-session");
        let mode = result.mode.as_deref().unwrap_or("active");
        println!("  {} [{}]", result.model_id, mode);
        if let Some(space) = &result.space {
            println!("    space: {space}");
        }
        println!("    session: {session}");
        if let Some(device_identity) = &result.device_identity {
            println!("    device: {device_identity}");
        }
        println!(
            "    expires_in≈{}m",
            presence_minutes_remaining(result.expires_at)
        );
    }
}

pub(crate) fn summarize_space_snapshot(
    space: &str,
    sessions: &[PresenceFeedResult],
    signals: &[SignalFeedResult],
    continuity: &ContinuitySnapshotSummary,
    local_feedback: &workspace::SpaceFeedbackSummary,
) -> SpaceSnapshotSummary {
    let promoted_signal_count = signals
        .iter()
        .filter(|signal| signal.density_tier != "sparse" && signal.density_tier != "candidate")
        .count();
    let blocked = signals.iter().any(|signal| {
        signal.kind == "avoid"
            && (signal.density_tier == "promoted"
                || signal.density_tier == "dominant"
                || signal.inhibition_penalty > 0)
    });
    if blocked {
        SpaceSnapshotSummary {
            status: "blocked",
            detail: format!(
                "This space currently has a promoted avoid signal, so the shared environment is steering work away from one path in `{space}`."
            ),
            active_sessions: sessions.len(),
            signal_count: signals.len(),
            promoted_signal_count,
            next_step: Some(
                "Reuse the top avoid signal and pick a different path before pushing more work into this space.".into(),
            ),
        }
    } else if promoted_signal_count > 0
        || continuity.net_summary_candidate_count > 0
        || local_feedback.positive_24h + local_feedback.negative_24h > 0
    {
        SpaceSnapshotSummary {
            status: "converging",
            detail: format!(
                "This space has active local learning, durable continuity evidence, or promoted signals, so shared consensus is starting to form in `{space}`."
            ),
            active_sessions: sessions.len(),
            signal_count: signals.len(),
            promoted_signal_count,
            next_step: Some(
                "Look at the top promoted signal before inventing a new path; this space is already teaching something.".into(),
            ),
        }
    } else if !sessions.is_empty() || !signals.is_empty() || continuity.trace_count > 0 {
        SpaceSnapshotSummary {
            status: "active",
            detail: format!(
                "This space is active, but no strong shared consensus has formed in `{space}` yet."
            ),
            active_sessions: sessions.len(),
            signal_count: signals.len(),
            promoted_signal_count,
            next_step: Some(
                "Keep work in the same space and let presence or signals accumulate before you overfit to one path.".into(),
            ),
        }
    } else {
        SpaceSnapshotSummary {
            status: "quiet",
            detail: format!(
                "No recent ambient activity or converging signals are visible in `{space}` yet."
            ),
            active_sessions: 0,
            signal_count: 0,
            promoted_signal_count: 0,
            next_step: Some(
                "If you start work here, keep using the same space so future agents inherit a real local environment.".into(),
            ),
        }
    }
}

pub(crate) fn render_space_snapshot(data: &SpaceSnapshotData) {
    println!("Space: {}", data.space);
    println!("  Status:  {}", data.summary.status);
    println!("  Meaning: {}", data.summary.detail);
    if let Some(step) = &data.summary.next_step {
        println!("  Next:    {step}");
    }
    println!(
        "  Ambient: {} active sessions, {} visible signals, {} promoted",
        data.summary.active_sessions, data.summary.signal_count, data.summary.promoted_signal_count
    );
    println!(
        "  Learning: {} positive / {} negative local follow events (24h)",
        data.local_feedback.positive_24h, data.local_feedback.negative_24h
    );
    println!(
        "  Loop:     {} (compression debt: {})",
        data.learning.status, data.learning.compression_debt.level
    );
    if data.continuity.summary.trace_count > 0 {
        println!(
            "  Continuity: {} local traces, {} Net-ready summaries",
            data.continuity.summary.trace_count,
            data.continuity.summary.net_summary_candidate_count
        );
    }
    if !data.sessions.is_empty() {
        println!("  Sessions:");
        for session in &data.sessions {
            println!(
                "    {} {} ({})",
                session.model_id,
                session.session_id.as_deref().unwrap_or("session"),
                session.mode.as_deref().unwrap_or("active")
            );
        }
    }
    if !data.signals.is_empty() {
        println!("  Signals:");
        for signal in &data.signals {
            println!(
                "    {}: {} [{}]",
                signal.kind, signal.message, signal.density_tier
            );
        }
    }
}

pub(crate) fn summarize_net_check_for_scenario(
    status: &thronglets::network_state::NetworkStatus,
    bootstrap_offline: bool,
) -> NetCheckData {
    let scenario = if bootstrap_offline {
        "bootstrap-offline"
    } else {
        "live"
    };
    let bootstrap_targets = if bootstrap_offline {
        0
    } else {
        status.bootstrap_targets
    };
    let bootstrap_fallback_mode = if bootstrap_offline {
        "disabled"
    } else {
        status.bootstrap_fallback_mode
    };
    let vps_dependency_level = if bootstrap_offline {
        if status.peer_count > 0 {
            "peer-native"
        } else {
            "offline"
        }
    } else {
        status.vps_dependency_level
    };
    let direct_connectivity = matches!(status.transport_mode, "direct" | "mixed");
    let remembered_peers = status.known_peer_count > 0 || status.peer_seed_count > 0;
    let trusted_path = status.trusted_peer_seed_count > 0;
    let bootstrap_offline_ready = bootstrap_targets == 0
        || (remembered_peers && matches!(bootstrap_fallback_mode, "delayed" | "disabled"));
    let low_vps_dependence = matches!(vps_dependency_level, "peer-native" | "low" | "medium");
    let peer_first_ready = status.peer_count > 0 && direct_connectivity && low_vps_dependence;
    let same_owner_direct_ready = status.peer_count > 0 && direct_connectivity && trusted_path;

    let status_label = if peer_first_ready {
        "peer-first"
    } else if status.peer_count == 0 && bootstrap_targets > 0 {
        "bootstrap-only"
    } else if status.peer_count == 0 && !remembered_peers && bootstrap_targets == 0 {
        "offline"
    } else {
        "degraded"
    };

    let checks = vec![
        NetCheckItem {
            name: "direct-connectivity",
            ok: direct_connectivity && status.peer_count > 0,
            detail: if status.peer_count == 0 {
                "No active peer connections yet.".into()
            } else {
                format!(
                    "Current transport mode is {} with {} connected peers.",
                    status.transport_mode, status.peer_count
                )
            },
        },
        NetCheckItem {
            name: "remembered-peers",
            ok: remembered_peers,
            detail: format!(
                "{} known peers, {} trusted seeds, {} total reusable peer seeds.",
                status.known_peer_count, status.trusted_peer_seed_count, status.peer_seed_count
            ),
        },
        NetCheckItem {
            name: "trusted-owner-path",
            ok: same_owner_direct_ready,
            detail: if same_owner_direct_ready {
                format!(
                    "This node already has direct connectivity with {} trusted same-owner seed(s) available for reconnect.",
                    status.trusted_peer_seed_count
                )
            } else if trusted_path {
                format!(
                    "{} trusted peer seeds came from owner-bound connection files, but the node has not yet proven a direct same-owner path in this scenario.",
                    status.trusted_peer_seed_count
                )
            } else {
                "No trusted peer seeds yet; same-owner multi-device reconnects will still lean on generic discovery.".into()
            },
        },
        NetCheckItem {
            name: "bootstrap-offline-path",
            ok: bootstrap_offline_ready,
            detail: if bootstrap_offline_ready {
                format!(
                    "Current bootstrap fallback mode is {}; this node has a non-bootstrap reconnect path.",
                    bootstrap_fallback_mode
                )
            } else {
                "If bootstrap vanished now, this node would still start from bootstrap immediately because it lacks remembered peer paths.".into()
            },
        },
        NetCheckItem {
            name: "vps-dependence",
            ok: low_vps_dependence,
            detail: format!("Current VPS dependency level is {}.", vps_dependency_level),
        },
    ];

    let mut next_steps = Vec::new();
    if !remembered_peers {
        next_steps.push(
            "Export or import a connection file from an already connected device so this node inherits direct peer seeds.".into(),
        );
    }
    if !bootstrap_offline_ready {
        next_steps.push(
            "Establish and retain at least one remembered peer path before treating this node as resilient to bootstrap / VPS loss.".into(),
        );
    }
    if remembered_peers && !trusted_path {
        next_steps.push(
            "For same-owner devices, refresh a connection file so this node learns trusted peer seeds before falling back to generic discovery.".into(),
        );
    }
    if trusted_path && !same_owner_direct_ready {
        next_steps.push(
            "Trusted peer seeds exist, but this node has not yet demonstrated a direct same-owner path; keep the peer online or retry from the joined device before relying on VPS.".into(),
        );
    }
    if status.peer_count == 0 && bootstrap_targets == 0 {
        next_steps.push(
            "Add at least one bootstrap target or join an existing owner/device network before expecting peer discovery.".into(),
        );
    } else if status.peer_count == 0 {
        next_steps.push(
            "This node is still bootstrap-only; keep it online long enough to learn direct peers and seed them locally.".into(),
        );
    }
    if status.transport_mode == "relayed" {
        next_steps.push(
            "Current traffic is relayed; prefer same-owner connection files or local peers to establish direct links.".into(),
        );
    }
    if matches!(vps_dependency_level, "high" | "bootstrap-only") {
        next_steps.push(
            "The network is still VPS-heavy; grow remembered peers so startup can reconnect directly before using bootstrap.".into(),
        );
    }
    next_steps.sort();
    next_steps.dedup();

    NetCheckData {
        summary: NetCheckSummary {
            scenario,
            status: status_label,
            peer_first_ready,
            same_owner_direct_ready,
            bootstrap_offline_ready,
            transport_mode: status.transport_mode,
            vps_dependency_level,
            bootstrap_fallback_mode,
            peer_count: status.peer_count,
            trusted_peer_seed_count: status.trusted_peer_seed_count,
            peer_seed_count: status.peer_seed_count,
            bootstrap_targets,
        },
        checks,
        next_steps,
    }
}

pub(crate) fn render_net_check(data: &NetCheckData) {
    println!(
        "Network check: {} ({})",
        data.summary.status, data.summary.scenario
    );
    println!(
        "Peer-first ready: {}",
        if data.summary.peer_first_ready {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Same-owner direct ready: {}",
        if data.summary.same_owner_direct_ready {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Bootstrap-offline ready: {}",
        if data.summary.bootstrap_offline_ready {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "Transport: {} | dependency: {} | fallback: {}",
        data.summary.transport_mode,
        data.summary.vps_dependency_level,
        data.summary.bootstrap_fallback_mode
    );
    println!(
        "Peers: {} connected, {} trusted seeds, {} total remembered seeds, {} bootstrap targets",
        data.summary.peer_count,
        data.summary.trusted_peer_seed_count,
        data.summary.peer_seed_count,
        data.summary.bootstrap_targets
    );
    for check in &data.checks {
        println!(
            "  [{}] {} — {}",
            if check.ok { "ok" } else { "missing" },
            check.name,
            check.detail
        );
    }
    for step in &data.next_steps {
        println!("Next: {step}");
    }
}

pub(crate) fn render_version_report(data: &VersionData) {
    println!("Thronglets version: {}", data.summary.version);
    println!(
        "Schemas: bootstrap={}, identity={}",
        data.summary.bootstrap_schema_version, data.summary.identity_schema_version
    );
    println!(
        "Connection export surfaces: {}",
        data.capabilities.connection_export_surfaces.join(", ")
    );
    println!(
        "Managed runtime: {} (refresh with `{}`)",
        data.capabilities.managed_runtime_surface,
        data.capabilities.managed_runtime_refresh_command
    );
    println!("Binary: {}", data.binary_path);
    println!("Hint: {}", data.source_hint);
}

pub(crate) fn filter_generic_runtime_snippets(
    plans: &mut [AdapterPlan],
    runtime_key: Option<&str>,
) {
    let Some(runtime_key) = runtime_key else {
        return;
    };

    for plan in plans {
        if plan.agent != AdapterKind::Generic.key() {
            continue;
        }
        if let Some(contract) = plan.contract.as_mut() {
            contract.runtimes.retain(|name, _| name == runtime_key);
        }
    }
}

pub(crate) fn render_doctor_reports(reports: &[AdapterDoctor]) {
    println!("Adapter health:");
    for report in reports {
        println!("  {}: {}", report.agent, report.status);
        for check in &report.checks {
            println!(
                "    [{}] {} — {}",
                if check.ok { "ok" } else { "missing" },
                check.name,
                check.detail
            );
        }
        for remediation in &report.remediation {
            println!("    fix: {remediation}");
        }
        if let Some(note) = &report.note {
            println!("    note: {note}");
        }
    }
}

pub(crate) fn render_doctor_report(data: &DoctorData) {
    println!("Doctor status: {}", data.summary.status);
    let healthy_agents: Vec<_> = data
        .reports
        .iter()
        .filter(|report| report.healthy && !report.restart_pending)
        .map(|report| report.agent.as_str())
        .collect();
    if !healthy_agents.is_empty() {
        println!("Healthy: {}", healthy_agents.join(", "));
    }
    let pending_agents: Vec<_> = data
        .reports
        .iter()
        .filter(|report| report.restart_pending)
        .map(|report| report.agent.as_str())
        .collect();
    if !pending_agents.is_empty() {
        println!("Pending restart: {}", pending_agents.join(", "));
    }
    if data.summary.restart_pending {
        println!("Restart still pending: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart once: {command}");
    }
    for step in &data.summary.next_steps {
        println!("After restart: {step}");
    }
    let unhealthy: Vec<_> = data
        .reports
        .iter()
        .filter(|report| !report.healthy)
        .cloned()
        .collect();
    if !unhealthy.is_empty() {
        println!();
        render_doctor_reports(&unhealthy);
    }
}

pub(crate) fn render_apply_results(results: &[AdapterApplyResult]) {
    println!("Applied adapter plan:");
    for result in results {
        println!(
            "  {}: {}",
            result.agent,
            if result.applied { "applied" } else { "skipped" }
        );
        for changed in &result.changed {
            println!("    change: {changed}");
        }
        for path in &result.paths {
            println!("    path: {path}");
        }
        if result.requires_restart {
            println!("    restart: required");
        }
        if let Some(note) = &result.note {
            println!("    note: {note}");
        }
    }
}

pub(crate) fn render_apply_plan_report(data: &ApplyPlanData) {
    println!("Apply status: {}", data.summary.status);
    let applied_agents: Vec<_> = data
        .results
        .iter()
        .filter(|result| result.applied)
        .map(|result| result.agent.as_str())
        .collect();
    if !applied_agents.is_empty() {
        println!("Applied: {}", applied_agents.join(", "));
    }
    if data.summary.restart_required {
        println!("Restart required: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart: {command}");
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
    }
    let skipped: Vec<_> = data
        .results
        .iter()
        .filter(|result| !result.applied)
        .cloned()
        .collect();
    if !skipped.is_empty() {
        println!();
        render_apply_results(&skipped);
    }
}

pub(crate) fn render_bootstrap_report(data: &BootstrapData) {
    println!("Bootstrap status: {}", data.summary.status);
    let installed: Vec<_> = data
        .results
        .iter()
        .filter(|result| result.applied)
        .map(|result| result.agent.as_str())
        .collect();
    if !installed.is_empty() {
        println!("Installed: {}", installed.join(", "));
    }
    if data.summary.restart_required {
        println!("Finish loading the integration by restarting the affected runtime once.");
    }
    if data.summary.restart_pending {
        println!("Restart still pending: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart once: {command}");
    }
    for step in &data.summary.next_steps {
        println!("After restart: {step}");
    }
    if !data.summary.healthy {
        let unhealthy: Vec<_> = data
            .reports
            .iter()
            .filter(|doctor| !doctor.healthy)
            .cloned()
            .collect();
        if !unhealthy.is_empty() {
            println!();
            render_doctor_reports(&unhealthy);
        }
    }
}

pub(crate) fn render_setup_report(data: &BootstrapData) {
    println!("Thronglets setup: {}", data.summary.status);
    let installed: Vec<_> = data
        .results
        .iter()
        .filter(|result| result.applied)
        .map(|result| result.agent.as_str())
        .collect();
    if !installed.is_empty() {
        println!("Installed: {}", installed.join(", "));
    }
    if data.summary.restart_pending {
        println!("Restart still pending: yes");
    }
    if data.summary.restart_required {
        println!("Finish loading the integration by restarting the affected runtime once.");
    }
    for command in &data.summary.restart_commands {
        println!("Restart once: {command}");
    }
    for step in &data.summary.next_steps {
        println!("After restart: {step}");
    }
    println!("Other agents can reuse `thronglets prehook` and `thronglets hook`.");
}

pub(crate) fn render_clear_restart_results(results: &[ClearRestartResult]) {
    println!("Cleared restart state:");
    for result in results {
        println!(
            "  {}: {}",
            result.agent,
            if result.cleared {
                "cleared"
            } else {
                "already-clear"
            }
        );
        if let Some(note) = &result.note {
            println!("    note: {note}");
        }
    }
}

pub(crate) fn render_clear_restart_report(data: &ClearRestartData) {
    println!("Clear restart status: {}", data.summary.status);
    if !data.summary.cleared_agents.is_empty() {
        println!("Cleared: {}", data.summary.cleared_agents.join(", "));
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
    }
    let unchanged: Vec<_> = data
        .results
        .iter()
        .filter(|result| !result.cleared)
        .cloned()
        .collect();
    if !unchanged.is_empty() {
        println!();
        render_clear_restart_results(&unchanged);
    }
}

pub(crate) fn render_runtime_ready_results(results: &[RuntimeReadyResult]) {
    println!("Runtime ready:");
    for result in results {
        println!(
            "  {}: {}",
            result.agent,
            if result.ready {
                "ready"
            } else {
                "already-ready"
            }
        );
        if let Some(note) = &result.note {
            println!("    note: {note}");
        }
    }
}

pub(crate) fn render_runtime_ready_report(data: &RuntimeReadyData) {
    println!("Runtime ready status: {}", data.summary.status);
    if !data.summary.ready_agents.is_empty() {
        println!("Ready: {}", data.summary.ready_agents.join(", "));
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
    }
    let unchanged: Vec<_> = data
        .results
        .iter()
        .filter(|result| !result.ready)
        .cloned()
        .collect();
    if !unchanged.is_empty() {
        println!();
        render_runtime_ready_results(&unchanged);
    }
}

pub(crate) fn render_rebuild_priors_report(data: &RebuildPriorsData) {
    println!("Derived guidance: {}", data.summary.status);
    println!("Ruleset epoch: {}", data.summary.ruleset_epoch);
    println!(
        "Legacy auto-signals removed: {}",
        data.summary.legacy_auto_signals_removed
    );
    println!(
        "Workspace caches cleared: {}",
        data.summary.workspace_caches_cleared
    );
    println!(
        "Raw traces preserved: {}",
        if data.summary.raw_traces_preserved {
            "yes"
        } else {
            "no"
        }
    );
    for step in &data.summary.next_steps {
        println!("Next: {step}");
    }
}
