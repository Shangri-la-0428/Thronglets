use std::path::{Path, PathBuf};

use serde::Serialize;
use thronglets::identity::{ConnectionFile, ConnectionSeedScope, IdentityBinding, NodeIdentity};
use thronglets::identity_surface::{IdentitySummary, identity_summary};
use thronglets::posts::is_signal_capability;
use thronglets::presence::is_presence_capability;
use thronglets::storage::TraceStore;
use thronglets::workspace::{self, WorkspaceState};

use crate::setup_support::{
    AdapterDetection, AdapterDoctor, AdapterKind, detect_adapter, doctor_adapter,
};
use crate::{BootstrapSummary, DEFAULT_CONNECTION_FILE_NAME};

#[derive(Serialize)]
pub(crate) struct OnboardingSummary {
    pub(crate) status: &'static str,
    pub(crate) detail: String,
    pub(crate) next_step: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct StartData {
    pub(crate) summary: OnboardingSummary,
    pub(crate) setup: BootstrapSummary,
    pub(crate) readiness: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
}

#[derive(Serialize)]
pub(crate) struct JoinFlowData {
    pub(crate) summary: OnboardingSummary,
    pub(crate) setup: BootstrapSummary,
    pub(crate) inspect: ReadinessSummary,
    pub(crate) readiness: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) file: String,
}

#[derive(Serialize)]
pub(crate) struct ShareFlowData {
    pub(crate) summary: OnboardingSummary,
    pub(crate) readiness: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) output: String,
    pub(crate) peer_seed_scope: &'static str,
    pub(crate) trusted_peer_seed_count: usize,
    pub(crate) peer_seed_count: usize,
    pub(crate) ttl_hours: u32,
}

#[derive(Clone, Serialize)]
pub(crate) struct ReadinessSummary {
    pub(crate) status: &'static str,
    pub(crate) detail: String,
    pub(crate) identity_ready: bool,
    pub(crate) network_path_ready: bool,
    pub(crate) trusted_same_owner_ready: bool,
    pub(crate) connected: bool,
    pub(crate) next_step: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ConnectionExportData {
    pub(crate) summary: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) output: String,
    pub(crate) primary_device_pubkey: String,
    pub(crate) signed_by_device: String,
    pub(crate) peer_seed_scope: &'static str,
    pub(crate) trusted_peer_seed_count: usize,
    pub(crate) peer_seed_count: usize,
    pub(crate) ttl_hours: u32,
    pub(crate) expires_at: u64,
}

#[derive(Serialize)]
pub(crate) struct StatusData {
    pub(crate) summary: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) runtime: RuntimeSummary,
    pub(crate) node_id: String,
    pub(crate) oasyce_address: String,
    pub(crate) data_dir: String,
    pub(crate) trace_count: u64,
    pub(crate) capabilities: usize,
    pub(crate) database_size_bytes: u64,
    pub(crate) substrate: workspace::SubstrateActivity,
    pub(crate) network: thronglets::network_state::NetworkStatus,
}

#[derive(Clone, Serialize)]
pub(crate) struct RuntimeSummary {
    pub(crate) status: &'static str,
    pub(crate) detail: String,
    pub(crate) next_step: Option<String>,
    pub(crate) detected_agents: Vec<String>,
    pub(crate) ready_agents: Vec<String>,
}

pub(crate) fn connection_readiness_summary(
    peer_seed_scope: ConnectionSeedScope,
    peer_seed_count: usize,
    imported_or_exported: &'static str,
) -> ReadinessSummary {
    let trusted_peer_seed_count = match peer_seed_scope {
        ConnectionSeedScope::Trusted => peer_seed_count,
        ConnectionSeedScope::Remembered => 0,
    };
    if trusted_peer_seed_count > 0 {
        ReadinessSummary {
            status: "trusted-same-owner-ready",
            detail: format!(
                "This connection file will carry identity plus {} trusted same-owner peer seed(s).",
                trusted_peer_seed_count
            ),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: true,
            connected: false,
            next_step: Some(
                "Use this file on the secondary device, then keep the primary device online once so direct same-owner recovery can be proven.".into(),
            ),
        }
    } else if peer_seed_count > 0 {
        ReadinessSummary {
            status: "identity-plus-peer-seeds",
            detail: format!(
                "This connection file will carry identity plus {} remembered peer seed(s), but not a trusted same-owner path yet.",
                peer_seed_count
            ),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: false,
            connected: false,
            next_step: Some(format!(
                "Continue with the secondary device, then let it learn direct peers; re-{imported_or_exported} a trusted connection file later if you want same-owner direct recovery."
            )),
        }
    } else {
        ReadinessSummary {
            status: "identity-only",
            detail:
                "This connection file will transfer identity only. It carries no reusable peer paths."
                    .into(),
            identity_ready: true,
            network_path_ready: false,
            trusted_same_owner_ready: false,
            connected: false,
            next_step: Some(
                "Re-export the connection file from the primary device after it has learned peers, then join again.".into(),
            ),
        }
    }
}

pub(crate) fn collect_status_data(
    home_dir: &Path,
    dir: &Path,
    identity: &NodeIdentity,
    binding: &IdentityBinding,
) -> StatusData {
    std::fs::create_dir_all(dir).expect("failed to create data directory");
    let store = TraceStore::open(&dir.join("traces.db")).expect("failed to open trace store");
    let workspace = WorkspaceState::load(dir);
    let network = thronglets::network_state::NetworkSnapshot::load(dir).to_status();
    let trace_count = store.count().unwrap_or(0);
    let cap_count = store
        .distinct_capabilities(1000)
        .map(|caps| {
            caps.into_iter()
                .filter(|capability| {
                    !is_signal_capability(capability) && !is_presence_capability(capability)
                })
                .count()
        })
        .unwrap_or(0);
    let db_path = dir.join("traces.db");
    let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let readiness = status_readiness_summary(binding, &network);
    let runtime = collect_runtime_summary(home_dir, dir);

    StatusData {
        summary: readiness,
        identity: identity_summary("healthy", binding),
        runtime,
        node_id: identity.short_id(),
        oasyce_address: identity.oasyce_address(),
        data_dir: dir.display().to_string(),
        trace_count,
        capabilities: cap_count,
        database_size_bytes: db_size,
        substrate: workspace.substrate_activity(),
        network,
    }
}

fn collect_runtime_summary(home_dir: &Path, data_dir: &Path) -> RuntimeSummary {
    let adapters = [
        AdapterKind::Claude,
        AdapterKind::Codex,
        AdapterKind::OpenClaw,
    ];
    let detections: Vec<_> = adapters
        .into_iter()
        .map(|agent| detect_adapter(home_dir, data_dir, agent))
        .collect();
    let reports: Vec<_> = adapters
        .into_iter()
        .map(|agent| doctor_adapter(home_dir, data_dir, agent))
        .collect();
    summarize_runtime_summary(&detections, &reports)
}

fn summarize_runtime_summary(
    detections: &[AdapterDetection],
    reports: &[AdapterDoctor],
) -> RuntimeSummary {
    let detected_agents: Vec<String> = detections
        .iter()
        .filter(|detection| detection.present)
        .map(|detection| detection.agent.clone())
        .collect();
    let ready_agents: Vec<String> = reports
        .iter()
        .filter(|report| report.present && report.healthy && !report.restart_pending)
        .map(|report| report.agent.clone())
        .collect();
    let broken_reports: Vec<_> = reports
        .iter()
        .filter(|report| report.present && !report.healthy)
        .collect();
    let restarting_reports: Vec<_> = reports
        .iter()
        .filter(|report| report.present && report.healthy && report.restart_pending)
        .collect();

    if !broken_reports.is_empty() {
        let broken_agents: Vec<_> = broken_reports
            .iter()
            .map(|report| report.agent.clone())
            .collect();
        RuntimeSummary {
            status: "needs-fix",
            detail: if broken_agents.len() == 1 {
                format!(
                    "Thronglets still needs attention in {} on this device.",
                    join_human_agents(&broken_agents)
                )
            } else {
                format!(
                    "Thronglets still needs attention in {} AI runtimes on this device.",
                    broken_agents.len()
                )
            },
            next_step: broken_reports
                .iter()
                .flat_map(|report| report.remediation.iter().cloned())
                .next()
                .or_else(|| {
                    Some("Run `thronglets doctor --json` for full adapter diagnostics.".into())
                }),
            detected_agents,
            ready_agents,
        }
    } else if !restarting_reports.is_empty() {
        let restarting_agents: Vec<_> = restarting_reports
            .iter()
            .map(|report| report.agent.clone())
            .collect();
        RuntimeSummary {
            status: "restart-required",
            detail: if restarting_agents.len() == 1 {
                format!(
                    "Thronglets is wired into {}, but it still needs one restart before the integration is live.",
                    join_human_agents(&restarting_agents)
                )
            } else {
                format!(
                    "Thronglets is wired into {} AI runtimes on this device, but some of them still need one restart before the integration is live.",
                    restarting_agents.len()
                )
            },
            next_step: restarting_reports
                .iter()
                .filter_map(|report| report.restart_command.clone())
                .next()
                .map(|command| format!("{command} once, then run `thronglets status` again.")),
            detected_agents,
            ready_agents,
        }
    } else if !detected_agents.is_empty() {
        RuntimeSummary {
            status: "ready",
            detail: format!(
                "Thronglets is loaded into {} on this device.",
                join_human_agents(&ready_agents)
            ),
            next_step: None,
            detected_agents,
            ready_agents,
        }
    } else {
        RuntimeSummary {
            status: "not-detected",
            detail: "No supported AI runtime was detected on this device yet.".into(),
            next_step: Some(
                "Open Codex, Claude Code, or OpenClaw on this device, then run `thronglets start` once.".into(),
            ),
            detected_agents,
            ready_agents,
        }
    }
}

pub(crate) fn summarize_start_flow(
    setup: &BootstrapSummary,
    readiness: &ReadinessSummary,
) -> OnboardingSummary {
    if !setup.healthy {
        OnboardingSummary {
            status: "needs-fix",
            detail: "Thronglets could not finish wiring this device into the local AI runtime yet."
                .into(),
            next_step: onboarding_next_step(setup, readiness),
        }
    } else if setup.restart_required || setup.restart_pending {
        OnboardingSummary {
            status: "restart-required",
            detail: "Thronglets is installed on this device, but the AI runtime still needs one restart before the integration is live.".into(),
            next_step: onboarding_next_step(setup, readiness),
        }
    } else if readiness.status == "local-only" {
        OnboardingSummary {
            status: "local-ready",
            detail: "This device is ready to use Thronglets locally. You can keep working now and add another device later if you want.".into(),
            next_step: Some(
                "Optional: when you want to add another device, export a connection file from this device.".into(),
            ),
        }
    } else {
        OnboardingSummary {
            status: readiness.status,
            detail: readiness.detail.clone(),
            next_step: readiness.next_step.clone(),
        }
    }
}

pub(crate) fn summarize_join_flow(
    setup: &BootstrapSummary,
    readiness: &ReadinessSummary,
) -> OnboardingSummary {
    if !setup.healthy {
        OnboardingSummary {
            status: "needs-fix",
            detail: "This device joined the identity flow, but the local AI runtime integration still needs fixing before the device is fully usable.".into(),
            next_step: onboarding_next_step(setup, readiness),
        }
    } else {
        OnboardingSummary {
            status: readiness.status,
            detail: readiness.detail.clone(),
            next_step: readiness.next_step.clone(),
        }
    }
}

pub(crate) fn summarize_share_flow(
    readiness: &ReadinessSummary,
    output: &Path,
) -> OnboardingSummary {
    match readiness.status {
        "trusted-same-owner-ready" => OnboardingSummary {
            status: "share-ready",
            detail: "This device exported a strong same-owner connection file. The next device should inherit identity plus a trusted recovery path.".into(),
            next_step: Some(format!(
                "Send {} to the second device, save it as ~/Desktop/{}, then run `thronglets join` there.",
                output.display(),
                DEFAULT_CONNECTION_FILE_NAME
            )),
        },
        "identity-plus-peer-seeds" => OnboardingSummary {
            status: "share-ready",
            detail: "This device exported a usable connection file with remembered peer paths. The next device should inherit identity plus reusable network paths.".into(),
            next_step: Some(format!(
                "Send {} to the second device, save it as ~/Desktop/{}, then run `thronglets join` there.",
                output.display(),
                DEFAULT_CONNECTION_FILE_NAME
            )),
        },
        _ => OnboardingSummary {
            status: "share-limited",
            detail: "This device exported the best connection file it could right now, but it still only carries identity. A second device can join the same identity, but may still start offline.".into(),
            next_step: Some(
                "Keep this device online until it learns peers, then run `thronglets share` again before onboarding the second device.".into(),
            ),
        },
    }
}

pub(crate) fn render_start_report(data: &StartData) {
    println!();
    println!("thronglets");
    println!();

    // Installed status
    if data.setup.healthy {
        println!("  installed:  yes");
    } else {
        println!("  installed:  needs fix");
    }

    // Which tools were configured
    let configured: Vec<&str> = data
        .setup
        .restart_commands
        .iter()
        .filter_map(|cmd| {
            if cmd.contains("Claude") || cmd.contains("claude") {
                Some("Claude Code")
            } else if cmd.contains("Cursor") || cmd.contains("cursor") {
                Some("Cursor")
            } else if cmd.contains("Windsurf") || cmd.contains("windsurf") {
                Some("Windsurf")
            } else if cmd.contains("Codex") || cmd.contains("codex") {
                Some("Codex")
            } else {
                None
            }
        })
        .collect();
    if !configured.is_empty() {
        println!("  configured: {}", configured.join(", "));
    }

    // Active status — the key question
    if data.setup.restart_required || data.setup.restart_pending {
        println!("  active:     not yet");
        println!();
        if let Some(cmd) = data.setup.restart_commands.first() {
            println!("  -> {cmd}");
        } else {
            println!("  -> restart your AI tool to activate");
        }
    } else if data.setup.healthy {
        println!("  active:     yes");
    }

    println!();
}

pub(crate) fn render_join_flow_report(data: &JoinFlowData) {
    println!("Thronglets: {}", human_onboarding_label(&data.summary));
    println!("  Meaning: {}", data.summary.detail);
    if let Some(step) = &data.summary.next_step {
        println!("  Next:    {step}");
    }
    if let Some(step) = join_runtime_follow_up(&data.setup) {
        println!("  Also:    {step}");
    }
    println!("  File:    {}", data.file);
    println!("  State:   {}", human_readiness_label(&data.readiness));
    println!("  Device:  {}", data.identity.device_identity);
}

pub(crate) fn render_share_flow_report(data: &ShareFlowData) {
    println!("Thronglets: {}", human_onboarding_label(&data.summary));
    println!("  Meaning: {}", data.summary.detail);
    println!("  Output:  {}", data.output);
    println!("  State:   {}", human_readiness_label(&data.readiness));
    if let Some(step) = &data.summary.next_step {
        println!("  Next:    {step}");
    }
}

pub(crate) fn render_status_report(data: &StatusData, owner_account: &str) {
    let (status_label, meaning, next_step) =
        if matches!(data.runtime.status, "needs-fix" | "restart-required") {
            (
                human_runtime_label(&data.runtime),
                data.runtime.detail.as_str(),
                data.runtime.next_step.as_ref(),
            )
        } else {
            (
                human_readiness_label(&data.summary),
                data.summary.detail.as_str(),
                data.summary.next_step.as_ref(),
            )
        };
    println!("Thronglets v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  Status:           {status_label}");
    println!("  Meaning:          {meaning}");
    if let Some(step) = next_step {
        println!("  Next:             {step}");
    }
    println!();
    println!("  Device identity:  {}", data.identity.device_identity);
    println!("  Owner account:    {owner_account}");
    println!();
    println!("  Runtime:          {}", human_runtime_label(&data.runtime));
    println!(
        "  Network:          {}",
        if data.summary.connected {
            "online"
        } else if data.summary.network_path_ready {
            "waiting to connect"
        } else {
            "offline"
        }
    );
    println!("  Help:             run `thronglets status --json` if you need full diagnostics");
}

pub(crate) fn default_share_output_path() -> PathBuf {
    let desktop =
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join("Desktop");
    if desktop.exists() {
        desktop.join(DEFAULT_CONNECTION_FILE_NAME)
    } else {
        PathBuf::from(DEFAULT_CONNECTION_FILE_NAME)
    }
}

pub(crate) fn export_connection_file(
    output: &Path,
    ttl_hours: u32,
    identity_binding: &IdentityBinding,
    identity: &NodeIdentity,
    dir: &Path,
) -> ConnectionExportData {
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).expect("failed to create connection file directory");
    }
    let network_snapshot = thronglets::network_state::NetworkSnapshot::load(dir);
    let trusted_peer_seeds = network_snapshot.trusted_peer_seed_addresses(16);
    let (peer_seed_scope, peer_seeds) = if trusted_peer_seeds.is_empty() {
        (
            ConnectionSeedScope::Remembered,
            network_snapshot.peer_seed_addresses(16),
        )
    } else {
        (ConnectionSeedScope::Trusted, trusted_peer_seeds)
    };
    let connection = ConnectionFile::from_binding(
        identity_binding,
        identity,
        ttl_hours,
        peer_seed_scope,
        peer_seeds,
    )
    .expect("failed to create connection file");
    connection
        .save(output)
        .expect("failed to write connection file");
    let exported_trusted_peer_seed_count = match connection.peer_seed_scope {
        ConnectionSeedScope::Trusted => connection.peer_seeds.len(),
        ConnectionSeedScope::Remembered => 0,
    };
    let peer_seed_scope = connection.peer_seed_scope_label();
    let readiness = connection_readiness_summary(
        connection.peer_seed_scope.clone(),
        connection.peer_seeds.len(),
        "export",
    );
    ConnectionExportData {
        summary: readiness,
        identity: identity_summary("exported", identity_binding),
        output: output.display().to_string(),
        primary_device_pubkey: connection.primary_device_pubkey.clone(),
        signed_by_device: connection.primary_device_identity.clone(),
        peer_seed_scope,
        trusted_peer_seed_count: exported_trusted_peer_seed_count,
        peer_seed_count: connection.peer_seeds.len(),
        ttl_hours: connection.ttl_hours(),
        expires_at: connection.expires_at,
    }
}

fn status_readiness_summary(
    binding: &IdentityBinding,
    network: &thronglets::network_state::NetworkStatus,
) -> ReadinessSummary {
    if network.peer_count > 0 {
        ReadinessSummary {
            status: "network-ready",
            detail: "Identity is ready and this device already has live peer connectivity.".into(),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: network.trusted_peer_seed_count > 0,
            connected: true,
            next_step: Some(
                "Optional: run `thronglets net-check --bootstrap-offline --json` to confirm bootstrap-free recovery."
                    .into(),
            ),
        }
    } else if network.trusted_peer_seed_count > 0 {
        ReadinessSummary {
            status: "trusted-same-owner-ready",
            detail: "Identity is ready and this device has already proven a trusted same-owner recovery path.".into(),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: true,
            connected: false,
            next_step: Some(
                "Start your AI runtime normally; this device should reconnect through the trusted same-owner path."
                    .into(),
            ),
        }
    } else if network.known_peer_count > 0 || network.peer_seed_count > 0 {
        ReadinessSummary {
            status: "network-paths-ready",
            detail: "Identity is ready and this device has reusable peer paths, but it is not currently connected.".into(),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: network.trusted_peer_seed_count > 0,
            connected: false,
            next_step: Some(
                "Keep a remembered peer online once so this device can re-establish a live connection.".into(),
            ),
        }
    } else if binding.owner_account.is_some() || binding.joined_from_device.is_some() {
        ReadinessSummary {
            status: "identity-only",
            detail: "Identity joined successfully, but this device still has no reusable peer paths.".into(),
            identity_ready: true,
            network_path_ready: false,
            trusted_same_owner_ready: false,
            connected: false,
            next_step: Some(
                "Re-export a connection file from the primary device after it has learned peers, then join again on this device.".into(),
            ),
        }
    } else {
        ReadinessSummary {
            status: "local-only",
            detail: "This device is usable locally, but it has not joined an owner or inherited any peer paths yet.".into(),
            identity_ready: true,
            network_path_ready: false,
            trusted_same_owner_ready: false,
            connected: false,
            next_step: Some(
                "If this is your first device, keep using it locally; otherwise import a connection file from an existing device.".into(),
            ),
        }
    }
}

fn onboarding_next_step(setup: &BootstrapSummary, readiness: &ReadinessSummary) -> Option<String> {
    if let Some(step) = runtime_follow_up_next_step(setup) {
        Some(step)
    } else if let Some(step) = setup.next_steps.first() {
        Some(step.clone())
    } else {
        readiness.next_step.clone()
    }
}

fn join_runtime_follow_up(setup: &BootstrapSummary) -> Option<String> {
    if !setup.healthy {
        None
    } else if setup.restart_required || setup.restart_pending {
        runtime_follow_up_next_step(setup)
    } else {
        None
    }
}

fn runtime_follow_up_next_step(setup: &BootstrapSummary) -> Option<String> {
    setup
        .restart_commands
        .first()
        .map(|command| format!("Restart your AI runtime once: {command}"))
}

pub(crate) fn human_readiness_label(summary: &ReadinessSummary) -> &'static str {
    match summary.status {
        "local-only" => "ready on this device",
        "identity-only" => "waiting for a better share file",
        "network-paths-ready" => "waiting for the first live connection",
        "network-ready" => "ready now",
        "trusted-same-owner-ready" => "ready now, with fast recovery",
        _ => summary.status,
    }
}

pub(crate) fn human_runtime_label(summary: &RuntimeSummary) -> &'static str {
    match summary.status {
        "ready" => "ready in your AI app",
        "restart-required" => "restart once",
        "needs-fix" => "needs attention",
        "not-detected" => "not connected to an AI app yet",
        _ => summary.status,
    }
}

fn human_onboarding_label(summary: &OnboardingSummary) -> &'static str {
    match summary.status {
        "needs-fix" => "needs attention",
        "restart-required" => "restart once",
        "local-ready" => "ready on this device",
        "share-ready" => "share this file",
        "share-limited" => "share now, then keep learning peers",
        "identity-only" => "waiting for a better share file",
        "network-paths-ready" => "waiting for the first live connection",
        "network-ready" => "ready now",
        "trusted-same-owner-ready" => "ready now, with fast recovery",
        _ => summary.status,
    }
}

fn join_human_agents(agents: &[String]) -> String {
    let labels: Vec<_> = agents
        .iter()
        .map(|agent| match agent.as_str() {
            "claude-code" => "Claude Code".to_string(),
            "codex" => "Codex".to_string(),
            "openclaw" => "OpenClaw".to_string(),
            other => other.to_string(),
        })
        .collect();
    match labels.as_slice() {
        [] => "no AI runtime".into(),
        [only] => only.clone(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let mut rendered = labels[..labels.len() - 1].join(", ");
            rendered.push_str(", and ");
            rendered.push_str(labels.last().unwrap());
            rendered
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BootstrapSummary, ReadinessSummary, status_readiness_summary, summarize_join_flow,
    };
    use thronglets::identity::{IdentityBinding, NodeIdentity};
    use thronglets::network_state::NetworkStatus;

    fn base_network_status() -> NetworkStatus {
        NetworkStatus {
            activity: "offline",
            transport_mode: "offline",
            vps_dependency_level: "offline",
            bootstrap_fallback_mode: "disabled",
            peer_count: 0,
            direct_peer_count: 0,
            relay_peer_count: 0,
            bootstrap_targets: 0,
            bootstrap_contacted_recently: false,
            known_peer_count: 0,
            trusted_peer_seed_count: 0,
            peer_seed_count: 0,
            last_peer_connected_age_ms: None,
            last_trace_received_age_ms: None,
            last_bootstrap_contact_age_ms: None,
        }
    }

    #[test]
    fn trusted_same_owner_paths_upgrade_readiness_without_live_peer_count() {
        let identity = NodeIdentity::generate();
        let binding = IdentityBinding::new(identity.device_identity())
            .bind_owner_account("oasyce1owner".into())
            .unwrap();
        let mut network = base_network_status();
        network.known_peer_count = 1;
        network.trusted_peer_seed_count = 1;
        network.peer_seed_count = 1;

        let summary = status_readiness_summary(&binding, &network);
        assert_eq!(summary.status, "trusted-same-owner-ready");
        assert!(summary.trusted_same_owner_ready);
        assert!(!summary.connected);
    }

    #[test]
    fn join_flow_keeps_network_ready_as_top_level_result_when_restart_is_pending() {
        let setup = BootstrapSummary {
            status: "restart-required",
            healthy: true,
            restart_pending: true,
            restart_required: true,
            restart_commands: vec!["codex restart".into()],
            next_steps: vec![],
        };
        let readiness = ReadinessSummary {
            status: "network-ready",
            detail: "Identity is ready and this device already has live peer connectivity.".into(),
            identity_ready: true,
            network_path_ready: true,
            trusted_same_owner_ready: false,
            connected: true,
            next_step: Some("Optional: run `thronglets net-check --bootstrap-offline --json` to confirm bootstrap-free recovery.".into()),
        };

        let summary = summarize_join_flow(&setup, &readiness);
        assert_eq!(summary.status, "network-ready");
        assert_eq!(summary.detail, readiness.detail);
        assert_eq!(summary.next_step, readiness.next_step);
    }
}
