use serde::Serialize;
use std::collections::BTreeMap;

use crate::onboarding_surface::ReadinessSummary;
use crate::setup_support::{AdapterApplyResult, AdapterDetection, AdapterDoctor, AdapterPlan};
use thronglets::continuity::ContinuitySpaceData;
use thronglets::identity::ConnectionBootstrapManifest;
use thronglets::identity_surface::IdentitySummary;
use thronglets::network_state::ObservedPeer;
use thronglets::posts::SignalFeedResult;
use thronglets::presence::PresenceFeedResult;
use thronglets::service::SpaceLearningView;
use thronglets::workspace;

#[derive(Serialize)]
pub(crate) struct MachineEnvelope<T> {
    pub(crate) schema_version: &'static str,
    pub(crate) command: &'static str,
    pub(crate) data: T,
}

#[derive(Clone, Serialize)]
pub(crate) struct BootstrapSummary {
    pub(crate) status: &'static str,
    pub(crate) healthy: bool,
    pub(crate) restart_pending: bool,
    pub(crate) restart_required: bool,
    pub(crate) restart_commands: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct BootstrapData {
    pub(crate) summary: BootstrapSummary,
    pub(crate) detections: Vec<AdapterDetection>,
    pub(crate) plans: Vec<AdapterPlan>,
    pub(crate) results: Vec<AdapterApplyResult>,
    pub(crate) reports: Vec<AdapterDoctor>,
}

#[derive(Serialize)]
pub(crate) struct DoctorSummary {
    pub(crate) status: &'static str,
    pub(crate) healthy: bool,
    pub(crate) restart_pending: bool,
    pub(crate) restart_commands: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct DoctorData {
    pub(crate) summary: DoctorSummary,
    pub(crate) reports: Vec<AdapterDoctor>,
}

#[derive(Serialize)]
pub(crate) struct DetectSummary {
    pub(crate) status: &'static str,
    pub(crate) detected_agents: Vec<String>,
    pub(crate) recommended_agents: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct DetectData {
    pub(crate) summary: DetectSummary,
    pub(crate) detections: Vec<AdapterDetection>,
}

#[derive(Serialize)]
pub(crate) struct PlanSummary {
    pub(crate) status: &'static str,
    pub(crate) restart_required: bool,
    pub(crate) restart_commands: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct InstallPlanData {
    pub(crate) summary: PlanSummary,
    pub(crate) plans: Vec<AdapterPlan>,
}

#[derive(Serialize)]
pub(crate) struct ApplySummary {
    pub(crate) status: &'static str,
    pub(crate) restart_required: bool,
    pub(crate) restart_commands: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct ApplyPlanData {
    pub(crate) summary: ApplySummary,
    pub(crate) results: Vec<AdapterApplyResult>,
}

#[derive(Serialize)]
pub(crate) struct ReleaseBaselineCheck {
    pub(crate) status: &'static str,
    pub(crate) violations: Vec<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct ClearRestartSummary {
    pub(crate) status: &'static str,
    pub(crate) cleared_agents: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct ClearRestartResult {
    pub(crate) agent: String,
    pub(crate) cleared: bool,
    pub(crate) note: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ClearRestartData {
    pub(crate) summary: ClearRestartSummary,
    pub(crate) results: Vec<ClearRestartResult>,
}

#[derive(Serialize)]
pub(crate) struct RuntimeReadySummary {
    pub(crate) status: &'static str,
    pub(crate) ready_agents: Vec<String>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct RuntimeReadyResult {
    pub(crate) agent: String,
    pub(crate) ready: bool,
    pub(crate) note: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct RuntimeReadyData {
    pub(crate) summary: RuntimeReadySummary,
    pub(crate) results: Vec<RuntimeReadyResult>,
}

#[derive(Serialize)]
pub(crate) struct IdentityIdData {
    pub(crate) summary: IdentitySummary,
    pub(crate) node_id: String,
    pub(crate) oasyce_address: String,
    pub(crate) public_key: String,
    pub(crate) data_dir: String,
}

#[derive(Serialize)]
pub(crate) struct IdentityMutationData {
    pub(crate) summary: IdentitySummary,
}

#[derive(Serialize)]
pub(crate) struct ConnectionJoinData {
    pub(crate) summary: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) file: String,
    pub(crate) signature_verified: bool,
    pub(crate) peer_seed_scope: &'static str,
    pub(crate) imported_trusted_peer_seed_count: usize,
    pub(crate) imported_peer_seed_count: usize,
    pub(crate) source_expires_at: u64,
}

#[derive(Serialize)]
pub(crate) struct ConnectionInspectData {
    pub(crate) summary: ReadinessSummary,
    pub(crate) identity: IdentitySummary,
    pub(crate) file: String,
    pub(crate) preferred_surface: Option<String>,
    pub(crate) surfaces: BTreeMap<String, ConnectionBootstrapManifest>,
    pub(crate) primary_device_pubkey: String,
    pub(crate) peer_seed_scope: &'static str,
    pub(crate) trusted_peer_seed_count: usize,
    pub(crate) peer_seed_count: usize,
    pub(crate) exported_at: u64,
    pub(crate) expires_at: u64,
    pub(crate) ttl_hours: u32,
    pub(crate) signature_verified: bool,
}

#[derive(Serialize)]
pub(crate) struct PeersSummary {
    pub(crate) status: &'static str,
    pub(crate) connected_peers: usize,
    pub(crate) known_peers: usize,
    pub(crate) trusted_peer_seed_count: usize,
    pub(crate) peer_seed_count: usize,
    pub(crate) bootstrap_targets: usize,
    pub(crate) vps_dependency_level: &'static str,
}

#[derive(Serialize)]
pub(crate) struct PeersData {
    pub(crate) summary: PeersSummary,
    pub(crate) peers: Vec<ObservedPeer>,
}

#[derive(Serialize)]
pub(crate) struct NetCheckItem {
    pub(crate) name: &'static str,
    pub(crate) ok: bool,
    pub(crate) detail: String,
}

#[derive(Serialize)]
pub(crate) struct NetCheckSummary {
    pub(crate) scenario: &'static str,
    pub(crate) status: &'static str,
    pub(crate) peer_first_ready: bool,
    pub(crate) same_owner_direct_ready: bool,
    pub(crate) bootstrap_offline_ready: bool,
    pub(crate) transport_mode: &'static str,
    pub(crate) vps_dependency_level: &'static str,
    pub(crate) bootstrap_fallback_mode: &'static str,
    pub(crate) peer_count: usize,
    pub(crate) trusted_peer_seed_count: usize,
    pub(crate) peer_seed_count: usize,
    pub(crate) bootstrap_targets: usize,
}

#[derive(Serialize)]
pub(crate) struct NetCheckData {
    pub(crate) summary: NetCheckSummary,
    pub(crate) checks: Vec<NetCheckItem>,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct VersionSummary {
    pub(crate) status: &'static str,
    pub(crate) version: String,
    pub(crate) bootstrap_schema_version: &'static str,
    pub(crate) identity_schema_version: &'static str,
}

#[derive(Serialize)]
pub(crate) struct VersionCapabilities {
    pub(crate) connection_export_surfaces: Vec<&'static str>,
    pub(crate) managed_runtime_surface: &'static str,
    pub(crate) managed_runtime_refresh_command: &'static str,
}

#[derive(Serialize)]
pub(crate) struct VersionData {
    pub(crate) summary: VersionSummary,
    pub(crate) binary_path: String,
    pub(crate) source_hint: &'static str,
    pub(crate) capabilities: VersionCapabilities,
}

#[derive(Serialize)]
pub(crate) struct RebuildPriorsSummary {
    pub(crate) status: &'static str,
    pub(crate) ruleset_epoch: &'static str,
    pub(crate) legacy_auto_signals_removed: u64,
    pub(crate) workspace_caches_cleared: usize,
    pub(crate) raw_traces_preserved: bool,
    pub(crate) next_steps: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct RebuildPriorsData {
    pub(crate) summary: RebuildPriorsSummary,
    pub(crate) workspace_reset: workspace::DerivedGuidanceResetReport,
}

#[derive(Serialize)]
pub(crate) struct PresenceSummary {
    pub(crate) status: &'static str,
    pub(crate) active_sessions: usize,
    pub(crate) space: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PresencePostData {
    pub(crate) summary: PresenceSummary,
    pub(crate) mode: Option<String>,
    pub(crate) ttl_minutes: u32,
    pub(crate) trace_id: String,
}

#[derive(Serialize)]
pub(crate) struct PresenceFeedData {
    pub(crate) summary: PresenceSummary,
    pub(crate) sessions: Vec<PresenceFeedResult>,
}

#[derive(Serialize)]
pub(crate) struct SpaceSnapshotSummary {
    pub(crate) status: &'static str,
    pub(crate) detail: String,
    pub(crate) active_sessions: usize,
    pub(crate) signal_count: usize,
    pub(crate) promoted_signal_count: usize,
    pub(crate) next_step: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct SpaceSnapshotData {
    pub(crate) summary: SpaceSnapshotSummary,
    pub(crate) space: String,
    pub(crate) sessions: Vec<PresenceFeedResult>,
    pub(crate) signals: Vec<SignalFeedResult>,
    pub(crate) learning: SpaceLearningView,
    pub(crate) continuity: ContinuitySpaceData,
    pub(crate) local_feedback: workspace::SpaceFeedbackSummary,
}
