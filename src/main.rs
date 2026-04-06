mod onboarding_surface;
mod setup_support;

use clap::{Parser, Subcommand, ValueEnum};
use onboarding_surface::{
    JoinFlowData, ReadinessSummary, ShareFlowData, StartData, collect_status_data,
    connection_readiness_summary, default_share_output_path, export_connection_file,
    render_join_flow_report, render_share_flow_report, render_start_report, render_status_report,
    summarize_join_flow, summarize_share_flow, summarize_start_flow,
};
use serde::Serialize;
use setup_support::{
    AdapterApplyResult, AdapterDetection, AdapterDoctor, AdapterKind, AdapterPlan,
    auto_clear_restart_pending_on_runtime_contact, clear_restart_pending, detect_adapter,
    doctor_adapter, install_claude, install_codex, install_cursor, install_openclaw, install_plan,
    set_restart_pending,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use thronglets::active_policy::{
    ActivePolicySet, PolicyStrength, compile_active_policy, method_compliance_from_payload,
};
use thronglets::ambient::{
    AMBIENT_PRIOR_SCHEMA_VERSION, AmbientPolicyState, AmbientPriorProjection, AmbientPriorRequest,
    ambient_prior_data, host_history_priors_for_context,
};
use thronglets::anchor::AnchorClient;
use thronglets::context::{simhash, similarity as context_similarity};
use thronglets::continuity::{
    ContinuitySnapshotSummary, ContinuitySpaceData, summarize_recent_continuity,
};
use thronglets::contracts::{
    GIT_HISTORY_MAX_ENTRIES, PREHOOK_HEADER, PREHOOK_MAX_COLLECTIVE_QUERIES, PREHOOK_MAX_HINTS,
};
use thronglets::eval::{
    EvalBaselineComparison, EvalCheckStatus, EvalCheckThresholds, EvalConfig, EvalFocus,
    LocalFeedbackSummary, SignalEvalSummary, evaluate_signal_quality,
};
use thronglets::identity::{
    ConnectionBootstrapManifest, ConnectionFile, ConnectionSeedScope,
    DEFAULT_CONNECTION_FILE_TTL_HOURS, IdentityBinding, NodeIdentity, identity_binding_path,
};
use thronglets::identity_surface::{
    AuthorizationCheckData, IdentitySummary, authorization_check_data, authorization_summary,
    identity_blueprint, identity_summary,
};
use thronglets::mcp::McpContext;
use thronglets::network_runtime::{
    NetworkRuntimeOptions, NetworkRuntimeRequest, attempt_first_connection, start_network_runtime,
};
use thronglets::pheromone::PheromoneField;
use thronglets::posts::{
    DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS, DEFAULT_SIGNAL_TTL_HOURS, SignalPostKind,
    SignalScopeFilter, SignalTraceConfig, create_auto_signal_trace,
    create_feed_reinforcement_traces, create_query_reinforcement_traces, create_signal_trace,
    filter_signal_feed_results, summarize_recent_signal_feed, summarize_signal_traces,
};
use thronglets::presence::{
    DEFAULT_PRESENCE_TTL_MINUTES, PresenceFeedResult, PresenceTraceConfig, create_presence_trace,
    summarize_recent_presence,
};
use thronglets::profile::{ProfileCheckThresholds, summarize_prehook_profiles};
use thronglets::signals::{
    Recommendation, Signal, SignalKind, StepCandidate, select as select_signals,
};
use thronglets::storage::TraceStore;
use thronglets::trace::{MethodCompliance, Outcome, Trace};
use thronglets::workspace::{self, WorkspaceState};
use tracing::info;

const BOOTSTRAP_SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";
const IDENTITY_SCHEMA_VERSION: &str = "thronglets.identity.v2";
const NETWORK_SCHEMA_VERSION: &str = "thronglets.network.v1";
const PRESENCE_SCHEMA_VERSION: &str = "thronglets.presence.v1";
const SPACE_SCHEMA_VERSION: &str = "thronglets.space.v2";
const VERSION_SCHEMA_VERSION: &str = "thronglets.version.v1";
const DERIVED_GUIDANCE_SCHEMA_VERSION: &str = "thronglets.derived-guidance.v1";
const DEFAULT_CONNECTION_FILE_NAME: &str = "thronglets.connection.json";
const TOP_LEVEL_AFTER_HELP: &str = "Normal path:\n  thronglets start\n  thronglets share\n  thronglets join\n  thronglets status\n\nAdvanced and machine-facing commands remain available, but are hidden from this top-level help so normal onboarding stays simple.";
const RELEASE_MAX_LOCAL_RETENTION_DROP_TENTHS_PP: i32 = 50;
const RELEASE_MAX_FAILED_COMMAND_RATE_RISE_TENTHS_PP: i32 = 50;
const RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS: i64 = 5_000;

#[derive(Serialize)]
struct MachineEnvelope<T> {
    schema_version: &'static str,
    command: &'static str,
    data: T,
}

#[derive(Clone, Serialize)]
struct BootstrapSummary {
    status: &'static str,
    healthy: bool,
    restart_pending: bool,
    restart_required: bool,
    restart_commands: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct BootstrapData {
    summary: BootstrapSummary,
    detections: Vec<AdapterDetection>,
    plans: Vec<AdapterPlan>,
    results: Vec<AdapterApplyResult>,
    reports: Vec<AdapterDoctor>,
}

#[derive(Serialize)]
struct DoctorSummary {
    status: &'static str,
    healthy: bool,
    restart_pending: bool,
    restart_commands: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct DoctorData {
    summary: DoctorSummary,
    reports: Vec<AdapterDoctor>,
}

#[derive(Serialize)]
struct DetectSummary {
    status: &'static str,
    detected_agents: Vec<String>,
    recommended_agents: Vec<String>,
}

#[derive(Serialize)]
struct DetectData {
    summary: DetectSummary,
    detections: Vec<AdapterDetection>,
}

#[derive(Serialize)]
struct PlanSummary {
    status: &'static str,
    restart_required: bool,
    restart_commands: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct InstallPlanData {
    summary: PlanSummary,
    plans: Vec<AdapterPlan>,
}

#[derive(Serialize)]
struct ApplySummary {
    status: &'static str,
    restart_required: bool,
    restart_commands: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct ApplyPlanData {
    summary: ApplySummary,
    results: Vec<AdapterApplyResult>,
}

#[derive(Serialize)]
struct ReleaseBaselineCheck {
    status: &'static str,
    violations: Vec<String>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct ClearRestartSummary {
    status: &'static str,
    cleared_agents: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Clone, Serialize)]
struct ClearRestartResult {
    agent: String,
    cleared: bool,
    note: Option<String>,
}

#[derive(Serialize)]
struct ClearRestartData {
    summary: ClearRestartSummary,
    results: Vec<ClearRestartResult>,
}

#[derive(Serialize)]
struct RuntimeReadySummary {
    status: &'static str,
    ready_agents: Vec<String>,
    next_steps: Vec<String>,
}

#[derive(Clone, Serialize)]
struct RuntimeReadyResult {
    agent: String,
    ready: bool,
    note: Option<String>,
}

#[derive(Serialize)]
struct RuntimeReadyData {
    summary: RuntimeReadySummary,
    results: Vec<RuntimeReadyResult>,
}

#[derive(Serialize)]
struct IdentityIdData {
    summary: IdentitySummary,
    node_id: String,
    oasyce_address: String,
    public_key: String,
    data_dir: String,
}

#[derive(Serialize)]
struct IdentityMutationData {
    summary: IdentitySummary,
}

#[derive(Serialize)]
struct ConnectionJoinData {
    summary: ReadinessSummary,
    identity: IdentitySummary,
    file: String,
    signature_verified: bool,
    peer_seed_scope: &'static str,
    imported_trusted_peer_seed_count: usize,
    imported_peer_seed_count: usize,
    source_expires_at: u64,
}

#[derive(Serialize)]
struct ConnectionInspectData {
    summary: ReadinessSummary,
    identity: IdentitySummary,
    file: String,
    preferred_surface: Option<String>,
    surfaces: BTreeMap<String, ConnectionBootstrapManifest>,
    primary_device_pubkey: String,
    peer_seed_scope: &'static str,
    trusted_peer_seed_count: usize,
    peer_seed_count: usize,
    exported_at: u64,
    expires_at: u64,
    ttl_hours: u32,
    signature_verified: bool,
}

#[derive(Serialize)]
struct PeersSummary {
    status: &'static str,
    connected_peers: usize,
    known_peers: usize,
    trusted_peer_seed_count: usize,
    peer_seed_count: usize,
    bootstrap_targets: usize,
    vps_dependency_level: &'static str,
}

#[derive(Serialize)]
struct PeersData {
    summary: PeersSummary,
    peers: Vec<thronglets::network_state::ObservedPeer>,
}

#[derive(Serialize)]
struct NetCheckItem {
    name: &'static str,
    ok: bool,
    detail: String,
}

#[derive(Serialize)]
struct NetCheckSummary {
    scenario: &'static str,
    status: &'static str,
    peer_first_ready: bool,
    same_owner_direct_ready: bool,
    bootstrap_offline_ready: bool,
    transport_mode: &'static str,
    vps_dependency_level: &'static str,
    bootstrap_fallback_mode: &'static str,
    peer_count: usize,
    trusted_peer_seed_count: usize,
    peer_seed_count: usize,
    bootstrap_targets: usize,
}

#[derive(Serialize)]
struct NetCheckData {
    summary: NetCheckSummary,
    checks: Vec<NetCheckItem>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct VersionSummary {
    status: &'static str,
    version: String,
    bootstrap_schema_version: &'static str,
    identity_schema_version: &'static str,
}

#[derive(Serialize)]
struct VersionCapabilities {
    connection_export_surfaces: Vec<&'static str>,
    managed_runtime_surface: &'static str,
    managed_runtime_refresh_command: &'static str,
}

#[derive(Serialize)]
struct VersionData {
    summary: VersionSummary,
    binary_path: String,
    source_hint: &'static str,
    capabilities: VersionCapabilities,
}

#[derive(Serialize)]
struct RebuildPriorsSummary {
    status: &'static str,
    ruleset_epoch: &'static str,
    legacy_auto_signals_removed: u64,
    workspace_caches_cleared: usize,
    raw_traces_preserved: bool,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct RebuildPriorsData {
    summary: RebuildPriorsSummary,
    workspace_reset: workspace::DerivedGuidanceResetReport,
}

#[derive(Serialize)]
struct PresenceSummary {
    status: &'static str,
    active_sessions: usize,
    space: Option<String>,
}

#[derive(Serialize)]
struct PresencePostData {
    summary: PresenceSummary,
    mode: Option<String>,
    ttl_minutes: u32,
    trace_id: String,
}

#[derive(Serialize)]
struct PresenceFeedData {
    summary: PresenceSummary,
    sessions: Vec<PresenceFeedResult>,
}

#[derive(Serialize)]
struct SpaceSnapshotSummary {
    status: &'static str,
    detail: String,
    active_sessions: usize,
    signal_count: usize,
    promoted_signal_count: usize,
    next_step: Option<String>,
}

#[derive(Serialize)]
struct SpaceSnapshotData {
    summary: SpaceSnapshotSummary,
    space: String,
    sessions: Vec<PresenceFeedResult>,
    signals: Vec<thronglets::posts::SignalFeedResult>,
    continuity: ContinuitySpaceData,
    local_feedback: workspace::SpaceFeedbackSummary,
}

#[derive(Parser)]
#[command(
    name = "thronglets",
    version,
    about = "P2P shared memory substrate for AI agents",
    after_help = TOP_LEVEL_AFTER_HELP
)]
struct Cli {
    /// Data directory (default: ~/.thronglets)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EvalSignalFocusArg {
    All,
    Repair,
    Preparation,
    Adjacency,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ReleaseEvalScopeArg {
    Project,
    Global,
    Both,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AdapterArg {
    All,
    Claude,
    Codex,
    Cursor,
    Openclaw,
    Generic,
}

impl From<EvalSignalFocusArg> for EvalFocus {
    fn from(value: EvalSignalFocusArg) -> Self {
        match value {
            EvalSignalFocusArg::All => EvalFocus::All,
            EvalSignalFocusArg::Repair => EvalFocus::Repair,
            EvalSignalFocusArg::Preparation => EvalFocus::Preparation,
            EvalSignalFocusArg::Adjacency => EvalFocus::Adjacency,
        }
    }
}

impl AdapterArg {
    fn includes(self, adapter: AdapterKind) -> bool {
        match self {
            Self::All => true,
            Self::Claude => matches!(adapter, AdapterKind::Claude),
            Self::Codex => matches!(adapter, AdapterKind::Codex),
            Self::Cursor => matches!(adapter, AdapterKind::Cursor),
            Self::Openclaw => matches!(adapter, AdapterKind::OpenClaw),
            Self::Generic => matches!(adapter, AdapterKind::Generic),
        }
    }

    fn as_kind(self) -> Option<AdapterKind> {
        match self {
            Self::All => None,
            Self::Claude => Some(AdapterKind::Claude),
            Self::Codex => Some(AdapterKind::Codex),
            Self::Cursor => Some(AdapterKind::Cursor),
            Self::Openclaw => Some(AdapterKind::OpenClaw),
            Self::Generic => Some(AdapterKind::Generic),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RuntimeArg {
    All,
    Python,
    Node,
    Shell,
}

impl RuntimeArg {
    fn key(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Python => Some("python"),
            Self::Node => Some("node"),
            Self::Shell => Some("shell"),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SignalKindArg {
    Recommend,
    Avoid,
    Watch,
    Info,
    PsycheState,
}

impl From<SignalKindArg> for SignalPostKind {
    fn from(value: SignalKindArg) -> Self {
        match value {
            SignalKindArg::Recommend => SignalPostKind::Recommend,
            SignalKindArg::Avoid => SignalPostKind::Avoid,
            SignalKindArg::Watch => SignalPostKind::Watch,
            SignalKindArg::Info => SignalPostKind::Info,
            SignalKindArg::PsycheState => SignalPostKind::PsycheState,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SignalScopeArg {
    All,
    Local,
    Collective,
    Mixed,
}

impl From<SignalScopeArg> for SignalScopeFilter {
    fn from(value: SignalScopeArg) -> Self {
        match value {
            SignalScopeArg::All => SignalScopeFilter::All,
            SignalScopeArg::Local => SignalScopeFilter::Local,
            SignalScopeArg::Collective => SignalScopeFilter::Collective,
            SignalScopeArg::Mixed => SignalScopeFilter::Mixed,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Show the running binary version and machine-facing schema versions.
    Version {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// One-command first-device setup.
    Start {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// One-command primary-device share flow.
    Share {
        /// Where to write the connection file. Defaults to ~/Desktop/thronglets.connection.json.
        #[arg(long)]
        output: Option<PathBuf>,

        /// How long the exported connection file should remain valid.
        #[arg(long, default_value_t = DEFAULT_CONNECTION_FILE_TTL_HOURS)]
        ttl_hours: u32,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// One-command join flow for a secondary device.
    Join {
        /// Connection file exported from the primary device. Defaults to ~/Desktop/thronglets.connection.json.
        #[arg(long)]
        file: Option<PathBuf>,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Start the Thronglets node
    Run {
        /// Port to listen on (0 = random)
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Bootstrap peer multiaddrs
        #[arg(long)]
        bootstrap: Vec<String>,
    },

    #[command(hide = true)]
    /// Show node identity
    Id {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Show the local authorization snapshot and final truth source.
    AuthorizationCheck {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Bind this device to an owner account.
    OwnerBind {
        /// Root owner account / wallet address.
        #[arg(long)]
        owner_account: String,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Export a connection file from the primary device.
    ConnectionExport {
        /// Where to write the connection file.
        #[arg(long)]
        output: PathBuf,

        /// How long the exported connection file should remain valid.
        #[arg(long, default_value_t = DEFAULT_CONNECTION_FILE_TTL_HOURS)]
        ttl_hours: u32,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Include the richer Oasyce surface alongside the default Thronglets surface.
        #[arg(long, default_value_t = false)]
        include_oasyce_surface: bool,
    },

    #[command(hide = true)]
    /// Join this device to an existing owner account using a connection file.
    ConnectionJoin {
        /// Connection file exported from the primary device.
        #[arg(long)]
        file: PathBuf,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Inspect and verify a connection file before joining.
    ConnectionInspect {
        /// Connection file exported from the primary device.
        #[arg(long)]
        file: PathBuf,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Record a trace manually (for testing/debugging)
    Record {
        /// Capability URI
        capability: String,

        /// Outcome
        #[arg(long, default_value = "succeeded")]
        outcome: String,

        /// Latency in ms
        #[arg(long, default_value_t = 0)]
        latency: u32,

        /// Input size (tokens/bytes)
        #[arg(long, default_value_t = 0)]
        input_size: u32,

        /// Task context (natural language)
        #[arg(long, default_value = "")]
        context: String,

        /// Model identifier
        #[arg(long, default_value = "cli")]
        model: String,
    },

    #[command(hide = true)]
    /// Query aggregate stats for a capability
    Query {
        /// Capability URI to query
        capability: String,
    },

    #[command(hide = true)]
    /// Leave an explicit short signal for future agents.
    SignalPost {
        /// Signal type.
        #[arg(long, value_enum)]
        kind: SignalKindArg,

        /// Task context this signal applies to.
        #[arg(long)]
        context: String,

        /// The short message to leave for future agents.
        #[arg(long)]
        message: String,

        /// Tool name (Bash, Edit, Read, etc.) to auto-format context for hook matching.
        #[arg(long)]
        tool: Option<String>,

        /// Model identifier.
        #[arg(long, default_value = "cli")]
        model: String,

        /// Optional session identifier.
        #[arg(long)]
        session_id: Option<String>,

        /// Optional space this signal belongs to.
        #[arg(long)]
        space: Option<String>,

        /// How long the signal should remain fresh before it decays away.
        #[arg(long, default_value_t = DEFAULT_SIGNAL_TTL_HOURS)]
        ttl_hours: u32,
    },

    #[command(hide = true)]
    /// Query explicit short signals left by other agents.
    SignalQuery {
        /// Task context to search against.
        #[arg(long)]
        context: String,

        /// Restrict to one signal kind.
        #[arg(long, value_enum)]
        kind: Option<SignalKindArg>,

        /// Restrict to one explicit substrate space.
        #[arg(long)]
        space: Option<String>,

        /// Maximum results to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },

    #[command(hide = true)]
    /// Show recent explicit signals that are converging across agents.
    SignalFeed {
        /// Only include signals seen in roughly the last N hours.
        #[arg(long, default_value_t = 24)]
        hours: u32,

        /// Restrict the feed to one signal kind.
        #[arg(long, value_enum)]
        kind: Option<SignalKindArg>,

        /// Filter by evidence scope.
        #[arg(long, value_enum, default_value_t = SignalScopeArg::All)]
        scope: SignalScopeArg,

        /// Restrict to one explicit substrate space.
        #[arg(long)]
        space: Option<String>,

        /// Maximum results to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    #[command(hide = true)]
    /// Announce that this session is active in a space, even without tool calls.
    PresencePing {
        /// Optional explicit substrate space this presence belongs to.
        #[arg(long)]
        space: Option<String>,

        /// Optional lightweight mode label, such as focus / explore / review / blocked.
        #[arg(long)]
        mode: Option<String>,

        /// Model identifier.
        #[arg(long, default_value = "cli")]
        model: String,

        /// Optional session identifier.
        #[arg(long)]
        session_id: Option<String>,

        /// How long this presence heartbeat should remain active.
        #[arg(long, default_value_t = DEFAULT_PRESENCE_TTL_MINUTES)]
        ttl_minutes: u32,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Show recent active sessions in a shared substrate space.
    PresenceFeed {
        /// Only include presence heartbeats seen in roughly the last N hours.
        #[arg(long, default_value_t = 1)]
        hours: u32,

        /// Restrict to one explicit substrate space.
        #[arg(long)]
        space: Option<String>,

        /// Maximum sessions to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Show the ambient state of a shared substrate space.
    Space {
        /// The explicit substrate space to inspect.
        #[arg(long)]
        space: String,

        /// Only include recent activity seen in roughly the last N hours.
        #[arg(long, default_value_t = 24)]
        hours: u32,

        /// Maximum sessions and signals to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Start the MCP adapter and observation surface for AI runtimes (JSON-RPC over stdio).
    /// Ambient hooks stay primary; explicit MCP tools are for inspect / debug / override.
    /// Automatically joins the P2P network so sparse residue can propagate to the collective.
    Mcp {
        /// P2P listen port (0 = random). Defaults to 0 (auto-join on random port).
        #[arg(long)]
        port: Option<u16>,

        /// Bootstrap peer multiaddrs.
        #[arg(long)]
        bootstrap: Vec<String>,

        /// Disable P2P networking (local-only mode).
        #[arg(long)]
        local: bool,

        /// Internal adapter hint used only by managed runtime integrations.
        #[arg(long, hide = true, value_enum)]
        agent: Option<AdapterArg>,
    },

    #[command(hide = true)]
    /// Anchor unanchored traces to the Oasyce blockchain
    Anchor {
        /// Oasyce chain RPC endpoint
        #[arg(long, default_value = "http://localhost:1317")]
        rpc: String,

        /// Chain ID
        #[arg(long, default_value = "oasyce-1")]
        chain_id: String,

        /// Anchor traces from the last N hours
        #[arg(long, default_value_t = 24)]
        hours: u64,
    },

    #[command(hide = true)]
    /// Auto-record traces from agent tool hooks.
    /// Reads a Claude-compatible hook JSON contract from stdin and records a trace.
    /// Designed to be fast (<50ms).
    Hook,

    #[command(hide = true)]
    /// Query substrate before tool calls and emit sparse decision signals.
    /// Reads a Claude-compatible hook JSON contract from stdin.
    /// Silent when no relevant data. Designed to be fast (<50ms).
    Prehook,

    #[command(hide = true)]
    /// Project lightweight ambient priors for a runtime turn.
    /// Reads JSON from stdin: { "text": "...", "space"?: "...", "limit"?: N }.
    /// Emits machine-readable JSON only; does not bootstrap identity.
    AmbientPriors {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Rebuild derived guidance under the current control law without touching raw traces.
    RebuildPriors {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Handle agent lifecycle events (SessionStart, SessionEnd, SubagentStart, SubagentStop).
    /// Records lifecycle traces and optionally emits additionalContext.
    /// Designed to be fast (<50ms).
    LifecycleHook {
        /// The lifecycle event type.
        #[arg(long, value_parser = ["session-start", "session-end", "subagent-start", "subagent-stop"])]
        event: String,
    },

    #[command(hide = true)]
    /// One-command setup: install known local agent adapters and hook integrations.
    Setup,

    #[command(hide = true)]
    /// Detect locally available agent runtimes and bootstrap surfaces.
    Detect {
        /// Restrict detection to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Show the machine-readable install plan for one adapter or all adapters.
    InstallPlan {
        /// Restrict planning to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Restrict generic hook snippets to one runtime.
        #[arg(long, value_enum, default_value_t = RuntimeArg::All)]
        runtime: RuntimeArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Apply the install plan for one adapter or all known adapters.
    ApplyPlan {
        /// Restrict application to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Verify whether a configured adapter is healthy.
    Doctor {
        /// Restrict verification to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Detect, plan, apply, and verify adapter setup in one command.
    Bootstrap {
        /// Restrict bootstrapping to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Clear persisted restart-pending state after the target runtime has been restarted.
    ClearRestart {
        /// Restrict clearing to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Mark a runtime as ready after it has successfully reloaded the Thronglets integration.
    RuntimeReady {
        /// Restrict readiness reporting to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Start the HTTP adapter for non-MCP runtimes (Python, LangChain, etc.).
    /// This is an integration surface, not the primary product mental model.
    /// Automatically joins the P2P network so sparse residue can propagate to the collective.
    Serve {
        /// HTTP port to listen on
        #[arg(long, default_value_t = 7777)]
        port: u16,

        /// P2P listen port (0 = random).
        #[arg(long, default_value_t = 0)]
        p2p_port: u16,

        /// Bootstrap peer multiaddrs.
        #[arg(long)]
        bootstrap: Vec<String>,

        /// Disable P2P networking (local-only mode).
        #[arg(long)]
        local: bool,
    },

    #[command(hide = true)]
    /// Show recently observed peers from the local network snapshot.
    Peers {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Maximum peer entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },

    #[command(hide = true)]
    /// Diagnose whether this node is actually operating peer-first or still mostly depending on bootstrap/VPS.
    NetCheck {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Evaluate the node as if bootstrap / VPS were unavailable right now.
        #[arg(long, default_value_t = false)]
        bootstrap_offline: bool,
    },

    /// Show node status and statistics
    Status {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Summarize stderr lines emitted by THRONGLETS_PROFILE_PREHOOK=1.
    /// Reads log lines from stdin and prints aggregate stats.
    ProfileSummary,

    #[command(hide = true)]
    /// Check whether profiled prehook logs still fit release-oriented sparse-signal thresholds.
    /// Reads log lines from stdin and exits non-zero on regression.
    ProfileCheck,

    #[command(hide = true)]
    /// Run a release-oriented operator gate across prehook profile logs and offline signal quality.
    /// Reads optional prehook profile lines from stdin, then evaluates current offline signal quality.
    ReleaseCheck {
        /// Look back over traces from the last N hours.
        #[arg(long, default_value_t = 168)]
        hours: u64,

        /// Evaluate at most this many recent sessions.
        #[arg(long, default_value_t = 200)]
        max_sessions: usize,

        /// Scope evaluation to this project root when using `project` or `both`.
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Choose whether release-check evaluates the current project, the global trace pool, or both.
        #[arg(long, value_enum, default_value_t = ReleaseEvalScopeArg::Project)]
        eval_scope: ReleaseEvalScopeArg,

        /// Legacy alias for `--eval-scope global`.
        #[arg(long, default_value_t = false, hide = true)]
        global: bool,

        /// Fail if no prehook profile samples are supplied on stdin.
        #[arg(long, default_value_t = false)]
        require_profile_samples: bool,

        /// Compare offline eval against a previous `eval-signals --json` baseline file.
        #[arg(long)]
        compare_baseline: Option<PathBuf>,

        /// Emit machine-readable JSON instead of a text summary.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    /// Replay recent sessions offline and score sparse-signal usefulness.
    EvalSignals {
        /// Look back over traces from the last N hours.
        #[arg(long, default_value_t = 168)]
        hours: u64,

        /// Evaluate at most this many recent sessions.
        #[arg(long, default_value_t = 200)]
        max_sessions: usize,

        /// Scope evaluation to this project root. Defaults to the current working directory.
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Evaluate across the entire trace store instead of scoping to one project.
        #[arg(long, default_value_t = false)]
        global: bool,

        /// Offline trial value for the local file-history gate.
        #[arg(long, default_value_t = 2)]
        local_history_gate_min: u32,

        /// Offline trial value for minimum repeated support before a pattern counts.
        #[arg(long, default_value_t = 2)]
        pattern_support_min: u32,

        /// Compare current results against a previous `eval-signals --json` baseline file.
        #[arg(long)]
        compare_baseline: Option<PathBuf>,

        /// Keep only the top N breakdown rows per category.
        #[arg(long, default_value_t = 5)]
        top_breakdowns: usize,

        /// Focus breakdown output on one signal family.
        #[arg(long, value_enum, default_value_t = EvalSignalFocusArg::All)]
        focus: EvalSignalFocusArg,

        /// Emit machine-readable JSON instead of a text summary.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

fn data_dir(cli_override: &Option<PathBuf>) -> PathBuf {
    cli_override
        .clone()
        .unwrap_or_else(|| home_dir().join(".thronglets"))
}

fn home_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
}

fn load_identity(data_dir: &std::path::Path) -> NodeIdentity {
    NodeIdentity::load_or_generate(&data_dir.join("node.key"))
        .expect("failed to load or generate node identity")
}

fn load_identity_binding(data_dir: &std::path::Path, identity: &NodeIdentity) -> IdentityBinding {
    IdentityBinding::load_or_create(&identity_binding_path(data_dir), identity)
        .expect("failed to load or create identity binding")
}

fn open_store(data_dir: &std::path::Path) -> TraceStore {
    std::fs::create_dir_all(data_dir).expect("failed to create data directory");
    TraceStore::open(&data_dir.join("traces.db")).expect("failed to open trace store")
}

fn load_workspace_state(data_dir: &Path) -> WorkspaceState {
    let mut workspace = WorkspaceState::load(data_dir);
    if workspace.ensure_current_derived_guidance_epoch().is_some() {
        workspace.save(data_dir);
    }
    workspace
}

fn rebuild_priors_data(data_dir: &Path, store: &TraceStore) -> RebuildPriorsData {
    let removed = store
        .delete_legacy_auto_signal_traces()
        .expect("failed to prune legacy auto-derived signals");
    let mut workspace = WorkspaceState::load(data_dir);
    let reset = workspace.reset_derived_guidance(thronglets::posts::DERIVED_GUIDANCE_EPOCH);
    workspace.save(data_dir);
    let cleared = reset.total_cleared();
    RebuildPriorsData {
        summary: RebuildPriorsSummary {
            status: if removed == 0 && cleared == 0 {
                "already-current"
            } else {
                "rebuilt"
            },
            ruleset_epoch: thronglets::posts::DERIVED_GUIDANCE_EPOCH,
            legacy_auto_signals_removed: removed,
            workspace_caches_cleared: cleared,
            raw_traces_preserved: true,
            next_steps: vec![
                "Raw traces were preserved; only derived guidance was rebuilt.".into(),
            ],
        },
        workspace_reset: reset,
    }
}

fn parse_outcome(s: &str) -> Outcome {
    match s {
        "succeeded" | "success" | "ok" => Outcome::Succeeded,
        "failed" | "fail" | "error" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "timeout" => Outcome::Timeout,
        _ => {
            eprintln!("Unknown outcome '{}', defaulting to 'succeeded'", s);
            Outcome::Succeeded
        }
    }
}

fn selected_adapters(target: AdapterArg) -> Vec<AdapterKind> {
    [
        AdapterKind::Claude,
        AdapterKind::Codex,
        AdapterKind::Cursor,
        AdapterKind::OpenClaw,
        AdapterKind::Generic,
    ]
    .into_iter()
    .filter(|adapter| target.includes(*adapter))
    .collect()
}

fn selected_known_adapters(target: AdapterArg) -> Vec<AdapterKind> {
    selected_adapters(target)
        .into_iter()
        .filter(|adapter| !matches!(adapter, AdapterKind::Generic))
        .collect()
}

fn selected_restart_adapters(target: AdapterArg) -> Vec<AdapterKind> {
    selected_known_adapters(target)
        .into_iter()
        .filter(|adapter| {
            matches!(
                adapter,
                AdapterKind::Codex | AdapterKind::Cursor | AdapterKind::OpenClaw
            )
        })
        .collect()
}

fn auto_clear_restart_from_agent_source(data_dir: &Path, agent_source: &str) {
    if let Some(agent) = AdapterKind::from_agent_source(agent_source) {
        let _ = auto_clear_restart_pending_on_runtime_contact(data_dir, agent);
    }
}

fn print_json<T: serde::Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn print_machine_json<T: serde::Serialize>(command: &'static str, value: &T) {
    print_machine_json_with_schema(BOOTSTRAP_SCHEMA_VERSION, command, value);
}

fn print_machine_json_with_schema<T: serde::Serialize>(
    schema_version: &'static str,
    command: &'static str,
    value: &T,
) {
    print_json(&MachineEnvelope {
        schema_version,
        command,
        data: value,
    });
}

fn collect_restart_commands(commands: impl IntoIterator<Item = Option<String>>) -> Vec<String> {
    let mut values: Vec<_> = commands.into_iter().flatten().collect();
    values.sort();
    values.dedup();
    values
}

fn render_detections(detections: &[AdapterDetection]) {
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

fn render_detect_report(data: &DetectData) {
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

fn render_install_plans(plans: &[AdapterPlan]) {
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

fn render_install_plan_report(data: &InstallPlanData) {
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

fn render_signal_query_results(results: &[thronglets::posts::SignalQueryResult]) {
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
        let promotion_suffix = if result.promotion_state != "none" {
            format!(" promoted={}", result.promotion_state)
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
            "    similarity={:.2} posts={} sources={}{}{}{}{}{} (local {} / collective {}) scope={} expires_in≈{}h",
            result.context_similarity,
            result.total_posts,
            result.source_count,
            model_suffix,
            density_suffix,
            promotion_suffix,
            reinforcement_suffix,
            inhibition_suffix,
            result.local_source_count,
            result.collective_source_count,
            result.evidence_scope,
            signal_hours_remaining(result.expires_at)
        );
        for context in &result.contexts {
            println!("    context: {context}");
        }
    }
}

fn render_signal_feed_results(results: &[thronglets::posts::SignalFeedResult]) {
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
        let promotion_suffix = if result.promotion_state != "none" {
            format!(" promoted={}", result.promotion_state)
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
            "    posts={} sources={}{}{}{}{}{}{} (local {} / collective {}) scope={} expires_in≈{}h",
            result.total_posts,
            result.source_count,
            model_suffix,
            focus_suffix,
            density_suffix,
            promotion_suffix,
            reinforcement_suffix,
            inhibition_suffix,
            result.local_source_count,
            result.collective_source_count,
            result.evidence_scope,
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

fn render_presence_feed_results(results: &[PresenceFeedResult]) {
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
            "    scope={} expires_in≈{}m",
            result.evidence_scope,
            presence_minutes_remaining(result.expires_at)
        );
    }
}

fn summarize_space_snapshot(
    space: &str,
    sessions: &[PresenceFeedResult],
    signals: &[thronglets::posts::SignalFeedResult],
    continuity: &ContinuitySnapshotSummary,
    local_feedback: &workspace::SpaceFeedbackSummary,
) -> SpaceSnapshotSummary {
    let promoted_signal_count = signals
        .iter()
        .filter(|signal| signal.promotion_state != "none")
        .count();
    let blocked = signals.iter().any(|signal| {
        signal.kind == "avoid"
            && (signal.promotion_state != "none" || signal.inhibition_penalty > 0)
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

fn render_space_snapshot(data: &SpaceSnapshotData) {
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
                signal.kind, signal.message, signal.promotion_state
            );
        }
    }
}

fn summarize_net_check_for_scenario(
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

fn render_net_check(data: &NetCheckData) {
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

fn render_version_report(data: &VersionData) {
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

fn signal_hours_remaining(expires_at: u64) -> u64 {
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

fn presence_minutes_remaining(expires_at: u64) -> u64 {
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

fn filter_generic_runtime_snippets(plans: &mut [AdapterPlan], runtime: RuntimeArg) {
    let Some(runtime_key) = runtime.key() else {
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

fn render_doctor_reports(reports: &[AdapterDoctor]) {
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

fn render_doctor_report(data: &DoctorData) {
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

fn render_apply_results(results: &[AdapterApplyResult]) {
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

fn render_apply_plan_report(data: &ApplyPlanData) {
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

fn render_bootstrap_report(data: &BootstrapData) {
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

fn render_setup_report(data: &BootstrapData) {
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

fn render_clear_restart_results(results: &[ClearRestartResult]) {
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

fn render_clear_restart_report(data: &ClearRestartData) {
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

fn render_runtime_ready_results(results: &[RuntimeReadyResult]) {
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

fn render_runtime_ready_report(data: &RuntimeReadyData) {
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

fn render_rebuild_priors_report(data: &RebuildPriorsData) {
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

fn summarize_doctor_reports(target: AdapterArg, reports: Vec<AdapterDoctor>) -> DoctorData {
    let healthy = !doctor_should_fail(target, &reports);
    let restart_pending = reports.iter().any(|report| report.restart_pending);
    let restart_commands =
        collect_restart_commands(reports.iter().map(|report| report.restart_command.clone()));
    let mut next_steps: Vec<_> = reports
        .iter()
        .filter(|report| doctor_report_requires_action(target, report))
        .flat_map(|report| report.remediation.iter().cloned())
        .collect();
    next_steps.sort();
    next_steps.dedup();

    DoctorData {
        summary: DoctorSummary {
            status: if !healthy {
                "needs-fix"
            } else if restart_pending {
                "restart-pending"
            } else {
                "healthy"
            },
            healthy,
            restart_pending,
            restart_commands,
            next_steps,
        },
        reports,
    }
}

fn summarize_detections(detections: Vec<AdapterDetection>) -> DetectData {
    let detected_agents = detections
        .iter()
        .filter(|detection| detection.present)
        .map(|detection| detection.agent.clone())
        .collect();
    let recommended_agents = detections
        .iter()
        .filter(|detection| detection.present && detection.apply_by_default)
        .map(|detection| detection.agent.clone())
        .collect();

    DetectData {
        summary: DetectSummary {
            status: "ready",
            detected_agents,
            recommended_agents,
        },
        detections,
    }
}

fn summarize_install_plans(plans: Vec<AdapterPlan>) -> InstallPlanData {
    let restart_required = plans.iter().any(|plan| plan.requires_restart);
    let restart_commands =
        collect_restart_commands(plans.iter().map(|plan| plan.restart_command.clone()));
    let mut next_steps: Vec<_> = plans
        .iter()
        .filter_map(|plan| plan.apply_command.clone())
        .collect();
    next_steps.sort();
    next_steps.dedup();

    InstallPlanData {
        summary: PlanSummary {
            status: "planned",
            restart_required,
            restart_commands,
            next_steps,
        },
        plans,
    }
}

fn summarize_apply_results(results: Vec<AdapterApplyResult>) -> ApplyPlanData {
    let restart_required = results.iter().any(|result| result.requires_restart);
    let restart_commands =
        collect_restart_commands(results.iter().map(|result| result.restart_command.clone()));
    let mut next_steps = Vec::new();
    if restart_required {
        next_steps.push("Restart the targeted agent so the new integration is loaded.".into());
    }

    ApplyPlanData {
        summary: ApplySummary {
            status: "applied",
            restart_required,
            restart_commands,
            next_steps,
        },
        results,
    }
}

fn summarize_clear_restart_results(results: Vec<ClearRestartResult>) -> ClearRestartData {
    let cleared_agents: Vec<_> = results
        .iter()
        .filter(|result| result.cleared)
        .map(|result| result.agent.clone())
        .collect();
    let mut next_steps = Vec::new();
    if cleared_agents.is_empty() {
        next_steps
            .push("Run `thronglets doctor --agent <adapter>` to confirm current status.".into());
    }

    ClearRestartData {
        summary: ClearRestartSummary {
            status: if cleared_agents.is_empty() {
                "already-clear"
            } else {
                "cleared"
            },
            cleared_agents,
            next_steps,
        },
        results,
    }
}

fn summarize_runtime_ready_results(results: Vec<RuntimeReadyResult>) -> RuntimeReadyData {
    let ready_agents: Vec<_> = results
        .iter()
        .filter(|result| result.ready)
        .map(|result| result.agent.clone())
        .collect();
    let mut next_steps = Vec::new();
    if ready_agents.is_empty() {
        next_steps
            .push("Run `thronglets doctor --agent <adapter>` to confirm current status.".into());
    }

    RuntimeReadyData {
        summary: RuntimeReadySummary {
            status: if ready_agents.is_empty() {
                "already-ready"
            } else {
                "ready"
            },
            ready_agents,
            next_steps,
        },
        results,
    }
}

fn apply_selected_adapters(
    target: AdapterArg,
    home_dir: &Path,
    data_dir: &Path,
    bin_path: &Path,
) -> std::io::Result<Vec<AdapterApplyResult>> {
    let mut results = Vec::new();

    for agent in selected_known_adapters(target) {
        match agent {
            AdapterKind::Claude => {
                let result = install_claude(home_dir, data_dir, bin_path)?;
                let mut changed = Vec::new();
                if result.added_post_hook {
                    changed.push("installed PostToolUse hook".into());
                }
                if result.added_pre_hook {
                    changed.push("installed PreToolUse hook".into());
                }
                if result.added_lifecycle_hooks > 0 {
                    changed.push(format!(
                        "installed {} lifecycle hooks (SessionStart/End, SubagentStart/Stop)",
                        result.added_lifecycle_hooks,
                    ));
                }
                if result.mcp_hotloaded {
                    changed.push("MCP server hot-loaded into current session".into());
                }
                if changed.is_empty() {
                    changed.push("hooks already present".into());
                }
                results.push(AdapterApplyResult {
                    agent: agent.key().into(),
                    applied: true,
                    changed,
                    requires_restart: false,
                    restart_command: None,
                    paths: vec![result.settings_path.display().to_string()],
                    note: None,
                });
            }
            AdapterKind::Codex => {
                let force = !matches!(target, AdapterArg::All);
                if let Some(result) = install_codex(home_dir, data_dir, bin_path, force)? {
                    set_restart_pending(data_dir, agent, true)?;
                    let mut changed = Vec::new();
                    if result.created_config {
                        changed.push("created Codex config".into());
                    }
                    if result.updated_server {
                        changed.push("installed Thronglets MCP server".into());
                    }
                    if result.updated_agents_memory {
                        changed.push("updated managed AGENTS block".into());
                    }
                    if changed.is_empty() {
                        changed.push("config already present".into());
                    }
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: true,
                        changed,
                        requires_restart: true,
                        restart_command: Some("Restart Codex".into()),
                        paths: vec![
                            result.config_path.display().to_string(),
                            result.agents_path.display().to_string(),
                        ],
                        note: Some("Restart Codex to load the MCP server.".into()),
                    });
                } else {
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: false,
                        changed: vec![],
                        requires_restart: false,
                        restart_command: None,
                        paths: vec![],
                        note: Some("Codex not detected; skipped in all-adapters mode.".into()),
                    });
                }
            }
            AdapterKind::Cursor => {
                let force = !matches!(target, AdapterArg::All);
                if let Some(result) = install_cursor(home_dir, data_dir, bin_path, force)? {
                    set_restart_pending(data_dir, agent, true)?;
                    let mut changed = Vec::new();
                    if result.created_config {
                        changed.push("created Cursor MCP config".into());
                    }
                    if result.updated_server {
                        changed.push("installed Thronglets MCP server".into());
                    }
                    if changed.is_empty() {
                        changed.push("config already present".into());
                    }
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: true,
                        changed,
                        requires_restart: true,
                        restart_command: Some("Restart Cursor".into()),
                        paths: vec![result.config_path.display().to_string()],
                        note: Some("Restart Cursor to load the MCP server.".into()),
                    });
                } else {
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: false,
                        changed: vec![],
                        requires_restart: false,
                        restart_command: None,
                        paths: vec![],
                        note: Some("Cursor not detected; skipped in all-adapters mode.".into()),
                    });
                }
            }
            AdapterKind::OpenClaw => {
                let force = !matches!(target, AdapterArg::All);
                if let Some(result) = install_openclaw(home_dir, data_dir, bin_path, true, force)? {
                    set_restart_pending(data_dir, agent, true)?;
                    let mut changed = Vec::new();
                    if result.created_config {
                        changed.push("created OpenClaw config".into());
                    } else {
                        changed.push("updated OpenClaw plugin config".into());
                    }
                    changed.push("wrote local plugin assets".into());
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: true,
                        changed,
                        requires_restart: true,
                        restart_command: Some("openclaw gateway restart".into()),
                        paths: vec![
                            result.config_path.display().to_string(),
                            result.plugin_dir.display().to_string(),
                        ],
                        note: Some(if result.restarted_gateway {
                            "Requested OpenClaw gateway restart.".into()
                        } else {
                            "OpenClaw gateway restart may still be required.".into()
                        }),
                    });
                } else {
                    results.push(AdapterApplyResult {
                        agent: agent.key().into(),
                        applied: false,
                        changed: vec![],
                        requires_restart: false,
                        restart_command: None,
                        paths: vec![],
                        note: Some("OpenClaw not detected; skipped in all-adapters mode.".into()),
                    });
                }
            }
            AdapterKind::Generic => {}
        }
    }

    Ok(results)
}

fn bootstrap_selected_adapters(
    target: AdapterArg,
    home_dir: &Path,
    data_dir: &Path,
    bin_path: &Path,
) -> std::io::Result<BootstrapData> {
    let detections: Vec<_> = selected_adapters(target)
        .into_iter()
        .map(|adapter| detect_adapter(home_dir, data_dir, adapter))
        .collect();
    let plans: Vec<_> = selected_adapters(target)
        .into_iter()
        .map(|adapter| install_plan(home_dir, data_dir, adapter))
        .collect();
    let applied = apply_selected_adapters(target, home_dir, data_dir, bin_path)?;
    let doctor_reports: Vec<_> = selected_adapters(target)
        .into_iter()
        .map(|adapter| doctor_adapter(home_dir, data_dir, adapter))
        .collect();
    let doctor_summary = summarize_doctor_reports(target, doctor_reports);
    let healthy = doctor_summary.summary.healthy;
    let restart_pending = doctor_summary.summary.restart_pending;
    let restart_required = applied.iter().any(|result| result.requires_restart);
    let mut next_steps = doctor_summary.summary.next_steps.clone();
    let restart_commands =
        collect_restart_commands(applied.iter().map(|result| result.restart_command.clone()));
    if restart_required {
        next_steps.push("Restart the targeted agent so the new integration is loaded.".into());
    }
    next_steps.sort();
    next_steps.dedup();

    Ok(BootstrapData {
        summary: BootstrapSummary {
            status: if !healthy {
                "needs-fix"
            } else if restart_pending {
                "restart-pending"
            } else {
                "healthy"
            },
            healthy,
            restart_pending,
            restart_required,
            restart_commands,
            next_steps,
        },
        detections,
        plans,
        results: applied,
        reports: doctor_summary.reports,
    })
}

fn clear_selected_restart_state(
    target: AdapterArg,
    data_dir: &Path,
) -> std::io::Result<ClearRestartData> {
    let mut results = Vec::new();

    for agent in selected_restart_adapters(target) {
        let cleared = clear_restart_pending(data_dir, agent)?;
        results.push(ClearRestartResult {
            agent: agent.key().into(),
            cleared,
            note: if cleared {
                Some("Cleared persisted restart-pending state.".into())
            } else {
                Some("No persisted restart-pending state was present.".into())
            },
        });
    }

    Ok(summarize_clear_restart_results(results))
}

fn mark_selected_runtime_ready(
    target: AdapterArg,
    data_dir: &Path,
) -> std::io::Result<RuntimeReadyData> {
    let mut results = Vec::new();

    for agent in selected_restart_adapters(target) {
        let ready = clear_restart_pending(data_dir, agent)?;
        results.push(RuntimeReadyResult {
            agent: agent.key().into(),
            ready,
            note: if ready {
                Some("Marked runtime as ready after a successful reload.".into())
            } else {
                Some("No persisted restart-pending state was present.".into())
            },
        });
    }

    Ok(summarize_runtime_ready_results(results))
}

fn doctor_should_fail(target: AdapterArg, reports: &[AdapterDoctor]) -> bool {
    reports.iter().any(|report| match target {
        AdapterArg::All => report.present && !report.healthy,
        _ => report.agent != AdapterKind::Generic.key() && !report.healthy,
    })
}

fn doctor_report_requires_action(target: AdapterArg, report: &AdapterDoctor) -> bool {
    match target {
        AdapterArg::All => report.present && (!report.healthy || report.restart_pending),
        _ => {
            report.agent != AdapterKind::Generic.key()
                && (!report.healthy || report.restart_pending)
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "thronglets=info".parse().unwrap()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let dir = data_dir(&cli.data_dir);

    if let Commands::Version { json } = &cli.command {
        let binary_path = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("thronglets"))
            .display()
            .to_string();
        let data = VersionData {
            summary: VersionSummary {
                status: "ready",
                version: env!("CARGO_PKG_VERSION").to_string(),
                bootstrap_schema_version: BOOTSTRAP_SCHEMA_VERSION,
                identity_schema_version: IDENTITY_SCHEMA_VERSION,
            },
            binary_path,
            source_hint: "If you are operating inside the Thronglets repo, prefer `cargo run --quiet -- <command>` so the binary matches the checked-out docs and source.",
            capabilities: VersionCapabilities {
                connection_export_surfaces: vec!["thronglets", "oasyce"],
                managed_runtime_surface: "thronglets-managed",
                managed_runtime_refresh_command: "thronglets setup",
            },
        };
        if *json {
            print_machine_json_with_schema(VERSION_SCHEMA_VERSION, "version", &data);
        } else {
            render_version_report(&data);
        }
        return;
    }

    match &cli.command {
        Commands::ProfileSummary => {
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            if let Some(summary) = summarize_prehook_profiles(&input) {
                println!("{}", summary.render());
            } else {
                println!("no prehook profile samples found");
            }
            return;
        }
        Commands::ProfileCheck => {
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            if let Some(summary) = summarize_prehook_profiles(&input) {
                let (passed, rendered) = summary.render_check(&ProfileCheckThresholds::default());
                println!("{rendered}");
                if !passed {
                    std::process::exit(1);
                }
            } else {
                println!("FAIL");
                println!("violations: no prehook profile samples found");
                std::process::exit(1);
            }
            return;
        }
        Commands::ReleaseCheck {
            hours,
            max_sessions,
            project_root,
            eval_scope,
            global,
            require_profile_samples,
            compare_baseline,
            json,
        } => {
            let mut input = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
            let profile_thresholds = ProfileCheckThresholds::default();
            let effective_eval_scope = if *global {
                ReleaseEvalScopeArg::Global
            } else {
                *eval_scope
            };
            let baseline = compare_baseline
                .as_deref()
                .map(load_eval_baseline)
                .transpose()
                .expect("failed to load eval baseline");

            let profile_section = match summarize_prehook_profiles(&input) {
                Some(summary) => {
                    let violations = summary.check(&profile_thresholds);
                    let passed = violations.is_empty();
                    let (_, rendered) = summary.render_check(&profile_thresholds);
                    (
                        if passed { "PASS" } else { "FAIL" },
                        !passed,
                        strip_check_header(&rendered),
                        serde_json::json!({
                            "status": if passed { "PASS" } else { "FAIL" },
                            "thresholds": profile_thresholds,
                            "summary": summary,
                            "violations": violations,
                            "notes": Vec::<String>::new(),
                        }),
                    )
                }
                None => {
                    let status = if *require_profile_samples {
                        "FAIL"
                    } else {
                        "SKIP"
                    };
                    let violations = if *require_profile_samples {
                        vec!["no prehook profile samples found".to_string()]
                    } else {
                        Vec::new()
                    };
                    let notes = if *require_profile_samples {
                        Vec::new()
                    } else {
                        vec!["no prehook profile samples found".to_string()]
                    };
                    (
                        status,
                        *require_profile_samples,
                        if *require_profile_samples {
                            "violations: no prehook profile samples found".to_string()
                        } else {
                            "notes: no prehook profile samples found".to_string()
                        },
                        serde_json::json!({
                            "status": status,
                            "thresholds": profile_thresholds,
                            "summary": serde_json::Value::Null,
                            "violations": violations,
                            "notes": notes,
                        }),
                    )
                }
            };

            let home_dir = home_dir();
            let doctor_section = run_release_doctor_section(&home_dir, &dir);

            let eval_thresholds = EvalCheckThresholds::default();
            let store = open_store(&dir);
            let local_feedback = LocalFeedbackSummary::from_workspace(&load_workspace_state(&dir));
            let default_project_root = project_root.clone().unwrap_or_else(|| {
                std::env::current_dir().expect("failed to determine current working directory")
            });
            let eval_sections: Vec<_> = match effective_eval_scope {
                ReleaseEvalScopeArg::Project => vec![(
                    "project",
                    run_release_eval_section(
                        &store,
                        *hours,
                        *max_sessions,
                        Some(default_project_root.as_path()),
                        local_feedback.clone(),
                        &eval_thresholds,
                        baseline.as_ref(),
                    ),
                )],
                ReleaseEvalScopeArg::Global => vec![(
                    "global",
                    run_release_eval_section(
                        &store,
                        *hours,
                        *max_sessions,
                        None,
                        None,
                        &eval_thresholds,
                        baseline.as_ref(),
                    ),
                )],
                ReleaseEvalScopeArg::Both => vec![
                    (
                        "project",
                        run_release_eval_section(
                            &store,
                            *hours,
                            *max_sessions,
                            Some(default_project_root.as_path()),
                            local_feedback,
                            &eval_thresholds,
                            baseline.as_ref(),
                        ),
                    ),
                    (
                        "global",
                        run_release_eval_section(
                            &store,
                            *hours,
                            *max_sessions,
                            None,
                            None,
                            &eval_thresholds,
                            baseline.as_ref(),
                        ),
                    ),
                ],
            };

            let overall_failed = profile_section.1
                || doctor_section.1
                || eval_sections.iter().any(|(_, section)| section.1);
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": if overall_failed { "FAIL" } else { "PASS" },
                        "eval_scope": match effective_eval_scope {
                            ReleaseEvalScopeArg::Project => "project",
                            ReleaseEvalScopeArg::Global => "global",
                            ReleaseEvalScopeArg::Both => "both",
                        },
                        "profile": profile_section.3,
                        "doctor": doctor_section.3,
                        "eval": if eval_sections.len() == 1 {
                            eval_sections[0].1.3.clone()
                        } else {
                            serde_json::json!({
                                "project": eval_sections[0].1.3.clone(),
                                "global": eval_sections[1].1.3.clone(),
                            })
                        },
                    }))
                    .expect("failed to serialize release check")
                );
            } else {
                println!("{}", if overall_failed { "FAIL" } else { "PASS" });
                print_release_section("profile", profile_section.0, &profile_section.2);
                print_release_section("doctor", doctor_section.0, &doctor_section.2);
                if eval_sections.len() == 1 {
                    let (label, section) = &eval_sections[0];
                    print_release_section(&format!("eval ({label})"), section.0, &section.2);
                } else {
                    for (label, section) in &eval_sections {
                        print_release_section(&format!("eval ({label})"), section.0, &section.2);
                    }
                }
            }
            if overall_failed {
                std::process::exit(1);
            }
            return;
        }
        Commands::EvalSignals {
            hours,
            max_sessions,
            project_root,
            global,
            local_history_gate_min,
            pattern_support_min,
            compare_baseline,
            top_breakdowns,
            focus,
            json,
        } => {
            let store = open_store(&dir);
            let project_scope = if *global {
                None
            } else {
                Some(project_root.clone().unwrap_or_else(|| {
                    std::env::current_dir().expect("failed to determine current working directory")
                }))
            };
            let eval_config = EvalConfig {
                local_history_gate_min: *local_history_gate_min,
                pattern_support_min: *pattern_support_min,
            };
            let default_config = EvalConfig::default();
            match evaluate_signal_quality(
                &store,
                *hours,
                *max_sessions,
                project_scope.as_deref(),
                eval_config,
            )
            .expect("failed to evaluate signal quality")
            {
                Some(summary) => {
                    let summary = if eval_config != default_config {
                        match evaluate_signal_quality(
                            &store,
                            *hours,
                            *max_sessions,
                            project_scope.as_deref(),
                            default_config,
                        )
                        .expect("failed to evaluate default signal quality")
                        {
                            Some(baseline) => summary.with_comparison_to_default(&baseline),
                            None => summary,
                        }
                    } else {
                        summary
                    }
                    .with_local_feedback(if project_scope.is_some() {
                        LocalFeedbackSummary::from_workspace(&load_workspace_state(&dir))
                    } else {
                        None
                    });
                    let summary = if let Some(baseline_path) = compare_baseline.as_ref() {
                        summary.with_comparison_to_baseline(
                            &load_eval_baseline(baseline_path)
                                .expect("failed to load eval baseline"),
                        )
                    } else {
                        summary
                    };
                    let summary = summary.focused((*focus).into(), *top_breakdowns);
                    if *json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&summary)
                                .expect("failed to serialize eval summary")
                        );
                    } else {
                        println!("{}", summary.render());
                    }
                }
                None => {
                    if *json {
                        println!("null");
                    } else {
                        println!("not enough recent session history to evaluate signals yet");
                    }
                }
            }
            return;
        }
        Commands::AmbientPriors { json } => {
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            let request: AmbientPriorRequest = match serde_json::from_str(&input) {
                Ok(request) => request,
                Err(error) => {
                    eprintln!("ambient-priors JSON parse error: {error}");
                    std::process::exit(1);
                }
            };

            let data = ambient_prior_data(&open_store(&dir), &request);
            if *json {
                print_machine_json_with_schema(
                    AMBIENT_PRIOR_SCHEMA_VERSION,
                    "ambient-priors",
                    &data,
                );
            } else {
                for prior in &data.priors {
                    println!("{} ({:.2})", prior.summary, prior.confidence);
                }
            }
            return;
        }
        Commands::RebuildPriors { json } => {
            let store = open_store(&dir);
            let data = rebuild_priors_data(&dir, &store);
            if *json {
                print_machine_json_with_schema(
                    DERIVED_GUIDANCE_SCHEMA_VERSION,
                    "rebuild-priors",
                    &data,
                );
            } else {
                render_rebuild_priors_report(&data);
            }
            return;
        }
        Commands::Setup => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let report = bootstrap_selected_adapters(AdapterArg::All, &home_dir, &dir, &bin)
                .expect("failed to bootstrap adapter plan");
            render_setup_report(&report);
            if !report.summary.healthy {
                std::process::exit(1);
            }
            return;
        }
        Commands::Detect { agent, json } => {
            let home_dir = home_dir();
            let detections: Vec<_> = selected_adapters(*agent)
                .into_iter()
                .map(|adapter| detect_adapter(&home_dir, &dir, adapter))
                .collect();
            let summary = summarize_detections(detections);
            if *json {
                print_machine_json("detect", &summary);
            } else {
                render_detect_report(&summary);
            }
            return;
        }
        Commands::InstallPlan {
            agent,
            runtime,
            json,
        } => {
            let home_dir = home_dir();
            let mut plans: Vec<_> = selected_adapters(*agent)
                .into_iter()
                .map(|adapter| install_plan(&home_dir, &dir, adapter))
                .collect();
            filter_generic_runtime_snippets(&mut plans, *runtime);
            let summary = summarize_install_plans(plans);
            if *json {
                print_machine_json("install-plan", &summary);
            } else {
                render_install_plan_report(&summary);
            }
            return;
        }
        Commands::ApplyPlan { agent, json } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let results = apply_selected_adapters(*agent, &home_dir, &dir, &bin)
                .expect("failed to apply adapter plan");
            let summary = summarize_apply_results(results);
            if *json {
                print_machine_json("apply-plan", &summary);
            } else {
                render_apply_plan_report(&summary);
            }
            return;
        }
        Commands::Doctor { agent, json } => {
            let home_dir = home_dir();
            let reports: Vec<_> = selected_adapters(*agent)
                .into_iter()
                .map(|adapter| doctor_adapter(&home_dir, &dir, adapter))
                .collect();
            let summary = summarize_doctor_reports(*agent, reports);
            if *json {
                print_machine_json("doctor", &summary);
            } else {
                render_doctor_report(&summary);
            }
            if !summary.summary.healthy {
                std::process::exit(1);
            }
            return;
        }
        Commands::Bootstrap { agent, json } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let report = bootstrap_selected_adapters(*agent, &home_dir, &dir, &bin)
                .expect("failed to bootstrap adapter plan");
            if *json {
                print_machine_json("bootstrap", &report);
            } else {
                render_bootstrap_report(&report);
            }
            if !report.summary.healthy {
                std::process::exit(1);
            }
            return;
        }
        Commands::ClearRestart { agent, json } => {
            let report =
                clear_selected_restart_state(*agent, &dir).expect("failed to clear restart state");
            if *json {
                print_machine_json("clear-restart", &report);
            } else {
                render_clear_restart_report(&report);
            }
            return;
        }
        Commands::RuntimeReady { agent, json } => {
            let report =
                mark_selected_runtime_ready(*agent, &dir).expect("failed to mark runtime ready");
            if *json {
                print_machine_json("runtime-ready", &report);
            } else {
                render_runtime_ready_report(&report);
            }
            return;
        }
        _ => {}
    }

    let identity = load_identity(&dir);
    let identity_binding = load_identity_binding(&dir, &identity);

    match cli.command {
        Commands::Version { .. } => unreachable!("version handled before identity bootstrap"),
        Commands::AmbientPriors { .. } => {
            unreachable!("ambient-priors handled before identity bootstrap")
        }
        Commands::RebuildPriors { .. } => {
            unreachable!("rebuild-priors handled before identity bootstrap")
        }

        Commands::Start { json } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            if !json {
                eprint!("detecting AI tools...");
            }
            let report = bootstrap_selected_adapters(AdapterArg::All, &home_dir, &dir, &bin)
                .expect("failed to bootstrap adapter plan");
            if !json {
                eprintln!(" done");
            }
            let status = collect_status_data(&home_dir, &dir, &identity, &identity_binding);
            let data = StartData {
                summary: summarize_start_flow(&report.summary, &status.summary),
                setup: report.summary.clone(),
                readiness: status.summary.clone(),
                identity: status.identity.clone(),
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "start", &data);
            } else {
                render_start_report(&data);
            }
            if !report.summary.healthy {
                std::process::exit(1);
            }
        }

        Commands::Share {
            output,
            ttl_hours,
            json,
        } => {
            let home_dir = home_dir();
            let status = collect_status_data(&home_dir, &dir, &identity, &identity_binding);
            let network_snapshot = thronglets::network_state::NetworkSnapshot::load(&dir);
            if !status.summary.network_path_ready
                && !network_snapshot.bootstrap_seed_addresses(8).is_empty()
            {
                let _ = attempt_first_connection(
                    &dir,
                    &identity,
                    &identity_binding,
                    Arc::new(open_store(&dir)),
                    std::time::Duration::from_secs(12),
                )
                .await;
            }
            let output = output.unwrap_or_else(default_share_output_path);
            let exported = export_connection_file(
                &output,
                ttl_hours,
                false,
                &identity_binding,
                &identity,
                &dir,
            );
            let data = ShareFlowData {
                summary: summarize_share_flow(&exported.summary, &output),
                readiness: exported.summary,
                identity: exported.identity,
                output: exported.output,
                preferred_surface: exported.preferred_surface,
                surfaces: exported.surfaces,
                peer_seed_scope: exported.peer_seed_scope,
                trusted_peer_seed_count: exported.trusted_peer_seed_count,
                peer_seed_count: exported.peer_seed_count,
                ttl_hours: exported.ttl_hours,
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "share", &data);
            } else {
                render_share_flow_report(&data);
            }
        }

        Commands::Join { file, json } => {
            let file = file.unwrap_or_else(default_share_output_path);
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let report = bootstrap_selected_adapters(AdapterArg::All, &home_dir, &dir, &bin)
                .expect("failed to bootstrap adapter plan");
            let connection = ConnectionFile::load(&file).expect("failed to read connection file");
            let inspect_readiness = connection_readiness_summary(
                connection.peer_seed_scope.clone(),
                connection.peer_seeds.len(),
                "export",
            );
            let binding = identity_binding
                .clone()
                .joined_via_connection(
                    connection.owner_account.clone(),
                    connection.oasyce_delegate_policy.clone(),
                    connection.primary_device_identity.clone(),
                )
                .expect("failed to update identity binding");
            binding
                .save(&identity_binding_path(&dir))
                .expect("failed to save identity binding");
            let mut network_snapshot = thronglets::network_state::NetworkSnapshot::load(&dir);
            match connection.peer_seed_scope {
                ConnectionSeedScope::Trusted => {
                    network_snapshot.merge_trusted_peer_seeds(connection.peer_seeds.clone());
                }
                ConnectionSeedScope::Remembered => {
                    network_snapshot.merge_peer_seeds(connection.peer_seeds.clone());
                }
            }
            network_snapshot.save(&dir);
            let mut status = collect_status_data(&home_dir, &dir, &identity, &binding);
            if report.summary.healthy
                && !report.summary.restart_required
                && !report.summary.restart_pending
                && status.summary.network_path_ready
                && !status.summary.connected
                && !status.summary.trusted_same_owner_ready
            {
                let _ = attempt_first_connection(
                    &dir,
                    &identity,
                    &binding,
                    Arc::new(open_store(&dir)),
                    std::time::Duration::from_secs(12),
                )
                .await;
                status = collect_status_data(&home_dir, &dir, &identity, &binding);
            }
            let data = JoinFlowData {
                summary: summarize_join_flow(&report.summary, &status.summary),
                setup: report.summary.clone(),
                inspect: inspect_readiness,
                readiness: status.summary.clone(),
                identity: status.identity.clone(),
                file: file.display().to_string(),
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "join", &data);
            } else {
                render_join_flow_report(&data);
            }
            if !report.summary.healthy {
                std::process::exit(1);
            }
        }

        Commands::Id { json } => {
            let data = IdentityIdData {
                summary: identity_summary("healthy", &identity_binding),
                node_id: identity.short_id(),
                oasyce_address: identity.oasyce_address(),
                public_key: hex_encode(&identity.public_key_bytes()),
                data_dir: dir.display().to_string(),
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "id", &data);
            } else {
                println!("Thronglets v{}", env!("CARGO_PKG_VERSION"));
                println!("Node ID:         {}", data.node_id);
                println!("Oasyce address:  {}", data.oasyce_address);
                println!("Device identity: {}", data.summary.device_identity);
                println!(
                    "Owner account:   {}",
                    identity_binding.owner_account_or_unbound()
                );
                println!("Public key:      {}", data.public_key);
                println!("Binding source:  {}", data.summary.binding_source);
                println!(
                    "Joined from:     {}",
                    identity_binding.joined_from_device_or_none()
                );
                println!("Data directory:  {}", data.data_dir);
            }
        }

        Commands::AuthorizationCheck { json } => {
            let data: AuthorizationCheckData = authorization_check_data(&identity_binding);
            if json {
                print_machine_json_with_schema(
                    IDENTITY_SCHEMA_VERSION,
                    "authorization-check",
                    &data,
                );
            } else {
                println!("Authorization:");
                println!(
                    "  Meaning:             local execution state is cached here; Oasyce Chain remains the final authorization truth source."
                );
                println!(
                    "  Local binding:       {}",
                    data.summary.local_binding_status
                );
                println!(
                    "  Local source:        {}",
                    data.summary.local_binding_source
                );
                println!(
                    "  Owner account:       {}",
                    data.owner_account.as_deref().unwrap_or("unbound")
                );
                println!("  Device identity:     {}", data.device_identity);
                println!(
                    "  Joined from device:  {}",
                    data.joined_from_device.as_deref().unwrap_or("none")
                );
                println!("  Final truth source:  {}", data.summary.final_truth_source);
                println!(
                    "  Authoritative check: {}",
                    data.summary.authoritative_status
                );
                println!("  Execution boundary:  {}", data.summary.execution_boundary);
            }
        }

        Commands::OwnerBind {
            owner_account,
            json,
        } => {
            let binding = identity_binding
                .clone()
                .bind_owner_account(owner_account)
                .expect("failed to update identity binding");
            binding
                .save(&identity_binding_path(&dir))
                .expect("failed to save identity binding");
            let data = IdentityMutationData {
                summary: identity_summary("updated", &binding),
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "owner-bind", &data);
            } else {
                println!("Owner binding updated:");
                println!("  Owner account:   {}", binding.owner_account_or_unbound());
                println!("  Device identity: {}", binding.device_identity);
                println!("  Source:          {}", binding.binding_source_or_local());
            }
        }

        Commands::ConnectionExport {
            output,
            ttl_hours,
            json,
            include_oasyce_surface,
        } => {
            let data = export_connection_file(
                &output,
                ttl_hours,
                include_oasyce_surface,
                &identity_binding,
                &identity,
                &dir,
            );
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "connection-export", &data);
            } else {
                println!("Connection file: {}", data.summary.status);
                println!("  Meaning:            {}", data.summary.detail);
                println!("  Output:             {}", data.output);
                println!(
                    "  Owner account:      {}",
                    data.identity.owner_account.as_deref().unwrap_or("unbound")
                );
                println!("  Primary device:     {}", data.identity.device_identity);
                println!("  Signed by device:   {}", data.signed_by_device);
                println!("  Seed scope:         {}", data.peer_seed_scope);
                println!("  Trusted seeds:      {}", data.trusted_peer_seed_count);
                println!("  Peer seeds:         {}", data.peer_seed_count);
                println!("  Expires in:         {}h", data.ttl_hours);
                if let Some(preferred) = &data.preferred_surface
                    && let Some(surface) = data.surfaces.get(preferred)
                {
                    println!("  Preferred surface:  {}", preferred);
                    println!("  AI join:            {}", surface.join.argv.join(" "));
                }
                if let Some(step) = &data.summary.next_step {
                    println!("  Next:               {step}");
                }
            }
        }

        Commands::ConnectionInspect { file, json } => {
            let connection = ConnectionFile::load(&file).expect("failed to read connection file");
            let inspected_binding = IdentityBinding {
                schema_version: "thronglets.identity.v1".into(),
                owner_account: connection.owner_account.clone(),
                oasyce_delegate_policy: connection.oasyce_delegate_policy.clone(),
                device_identity: connection.primary_device_identity.clone(),
                binding_source: Some("connection_file".into()),
                joined_from_device: None,
                updated_at: connection.exported_at,
            };
            let identity = IdentitySummary {
                status: "valid",
                owner_account: connection.owner_account.clone(),
                device_identity: connection.primary_device_identity.clone(),
                binding_source: "connection_file".into(),
                joined_from_device: None,
                identity_model: identity_blueprint(
                    connection.owner_account.clone(),
                    connection.primary_device_identity.clone(),
                ),
                authorization: authorization_summary(&inspected_binding),
            };
            let inspected_trusted_peer_seed_count = match connection.peer_seed_scope {
                ConnectionSeedScope::Trusted => connection.peer_seeds.len(),
                ConnectionSeedScope::Remembered => 0,
            };
            let peer_seed_scope = connection.peer_seed_scope_label();
            let readiness = connection_readiness_summary(
                connection.peer_seed_scope.clone(),
                connection.peer_seeds.len(),
                "export",
            );
            let data = ConnectionInspectData {
                summary: readiness,
                identity,
                file: file.display().to_string(),
                preferred_surface: connection.effective_preferred_surface(),
                surfaces: connection.effective_surfaces(),
                primary_device_pubkey: connection.primary_device_pubkey.clone(),
                peer_seed_scope,
                trusted_peer_seed_count: inspected_trusted_peer_seed_count,
                peer_seed_count: connection.peer_seeds.len(),
                exported_at: connection.exported_at,
                expires_at: connection.expires_at,
                ttl_hours: connection.ttl_hours(),
                signature_verified: true,
            };
            if json {
                print_machine_json_with_schema(
                    IDENTITY_SCHEMA_VERSION,
                    "connection-inspect",
                    &data,
                );
            } else {
                println!("Connection file: {}", data.summary.status);
                println!("  Meaning:            {}", data.summary.detail);
                println!("  File:               {}", data.file);
                println!(
                    "  Owner account:      {}",
                    data.identity.owner_account.as_deref().unwrap_or("unbound")
                );
                println!("  Primary device:     {}", data.identity.device_identity);
                println!("  Signature verified: yes");
                println!("  Seed scope:         {}", data.peer_seed_scope);
                println!("  Trusted seeds:      {}", data.trusted_peer_seed_count);
                println!("  Peer seeds:         {}", data.peer_seed_count);
                println!("  Expires in:         {}h", data.ttl_hours);
                if let Some(preferred) = &data.preferred_surface
                    && let Some(surface) = data.surfaces.get(preferred)
                {
                    println!("  Preferred surface:  {}", preferred);
                    println!("  AI join:            {}", surface.join.argv.join(" "));
                }
                if let Some(step) = &data.summary.next_step {
                    println!("  Next:               {step}");
                }
            }
        }

        Commands::ConnectionJoin { file, json } => {
            let connection = ConnectionFile::load(&file).expect("failed to read connection file");
            let binding = identity_binding
                .clone()
                .joined_via_connection(
                    connection.owner_account.clone(),
                    connection.oasyce_delegate_policy.clone(),
                    connection.primary_device_identity.clone(),
                )
                .expect("failed to update identity binding");
            binding
                .save(&identity_binding_path(&dir))
                .expect("failed to save identity binding");
            let mut network_snapshot = thronglets::network_state::NetworkSnapshot::load(&dir);
            match connection.peer_seed_scope {
                ConnectionSeedScope::Trusted => {
                    network_snapshot.merge_trusted_peer_seeds(connection.peer_seeds.clone());
                }
                ConnectionSeedScope::Remembered => {
                    network_snapshot.merge_peer_seeds(connection.peer_seeds.clone());
                }
            }
            network_snapshot.save(&dir);
            let imported_trusted_peer_seed_count = match connection.peer_seed_scope {
                ConnectionSeedScope::Trusted => connection.peer_seeds.len(),
                ConnectionSeedScope::Remembered => 0,
            };
            let peer_seed_scope = connection.peer_seed_scope_label();
            let readiness = connection_readiness_summary(
                connection.peer_seed_scope.clone(),
                connection.peer_seeds.len(),
                "export",
            );
            let data = ConnectionJoinData {
                summary: readiness,
                identity: identity_summary("joined", &binding),
                file: file.display().to_string(),
                signature_verified: true,
                peer_seed_scope,
                imported_trusted_peer_seed_count,
                imported_peer_seed_count: connection.peer_seeds.len(),
                source_expires_at: connection.expires_at,
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "connection-join", &data);
            } else {
                println!("Connection join: {}", data.summary.status);
                println!("  Meaning:            {}", data.summary.detail);
                println!("  File:               {}", data.file);
                println!(
                    "  Owner account:      {}",
                    data.identity.owner_account.as_deref().unwrap_or("unbound")
                );
                println!("  Device identity:    {}", data.identity.device_identity);
                println!(
                    "  Joined from device: {}",
                    connection.primary_device_identity
                );
                println!("  Signature verified: yes");
                println!("  Seed scope:         {}", data.peer_seed_scope);
                println!(
                    "  Imported trusted seeds: {}",
                    data.imported_trusted_peer_seed_count
                );
                println!("  Imported peer seeds: {}", data.imported_peer_seed_count);
                if let Some(step) = &data.summary.next_step {
                    println!("  Next:               {step}");
                }
            }
        }

        Commands::Record {
            capability,
            outcome,
            latency,
            input_size,
            context,
            model,
        } => {
            let store = open_store(&dir);
            let outcome = parse_outcome(&outcome);
            let ctx_hash = simhash(&context);
            let ctx_text = if context.is_empty() {
                None
            } else {
                Some(context.clone())
            };
            let trace = Trace::new_with_identity(
                capability.clone(),
                outcome,
                latency,
                input_size,
                ctx_hash,
                ctx_text,
                None,
                identity_binding.owner_account.clone(),
                Some(identity_binding.device_identity.clone()),
                model,
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).expect("failed to insert trace");
            println!("Trace recorded:");
            println!("  ID:         {}", hex_encode(&trace.id[..8]));
            println!("  Capability: {}", capability);
            println!("  Outcome:    {:?}", outcome);
        }

        Commands::Query { capability } => {
            let store = open_store(&dir);
            match store.aggregate(&capability).expect("query failed") {
                Some(stats) => {
                    println!("Aggregate for '{}':", capability);
                    println!("  Total traces:  {}", stats.total_traces);
                    println!("  Success rate:  {:.1}%", stats.success_rate * 100.0);
                    println!("  P50 latency:   {:.0}ms", stats.p50_latency_ms);
                    println!("  P95 latency:   {:.0}ms", stats.p95_latency_ms);
                    println!("  Avg input:     {:.0}", stats.avg_input_size);
                    println!("  Confidence:    {:.2}", stats.confidence);
                }
                None => {
                    println!("No traces found for '{}'", capability);
                }
            }
        }

        Commands::SignalPost {
            kind,
            context,
            message,
            tool,
            model,
            session_id,
            space,
            ttl_hours,
        } => {
            let store = open_store(&dir);
            let context = thronglets::context::format_signal_context(tool.as_deref(), &context);
            let trace = create_signal_trace(
                kind.into(),
                &context,
                &message,
                SignalTraceConfig {
                    model_id: model,
                    session_id,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    agent_id: None,
                    sigil_id: None,
                    space: space.clone(),
                    ttl_hours,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).expect("failed to insert signal trace");
            println!("Signal posted:");
            println!("  Kind:      {}", SignalPostKind::from(kind).as_str());
            println!("  Message:   {}", message);
            if let Some(space) = space {
                println!("  Space:     {}", space);
            }
            println!("  Fresh for: {}h", ttl_hours);
            println!("  Trace ID:  {}", hex_encode(&trace.id[..8]));
        }

        Commands::SignalQuery {
            context,
            kind,
            space,
            limit,
        } => {
            let store = open_store(&dir);
            let query_hash = simhash(&context);
            let traces = store
                .query_signal_traces(
                    &query_hash,
                    kind.map(Into::into),
                    48,
                    limit,
                    space.as_deref(),
                )
                .expect("failed to query signal traces");
            let results = summarize_signal_traces(
                &traces,
                &context,
                &identity_binding.device_identity,
                identity.public_key_bytes(),
                limit,
            );
            for trace in create_query_reinforcement_traces(
                &results,
                &context,
                SignalTraceConfig {
                    model_id: "thronglets-query".into(),
                    session_id: None,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    agent_id: None,
                    sigil_id: None,
                    space: None,
                    ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            ) {
                let _ = store.insert(&trace);
            }
            render_signal_query_results(&results);
        }

        Commands::SignalFeed {
            hours,
            kind,
            scope,
            space,
            limit,
        } => {
            let store = open_store(&dir);
            let traces = store
                .query_recent_signal_traces(hours, kind.map(Into::into), limit, space.as_deref())
                .expect("failed to query recent signal traces");
            let results = filter_signal_feed_results(
                summarize_recent_signal_feed(
                    &traces,
                    &identity_binding.device_identity,
                    identity.public_key_bytes(),
                    limit,
                ),
                scope.into(),
            );
            for trace in create_feed_reinforcement_traces(
                &results,
                SignalTraceConfig {
                    model_id: "thronglets-feed".into(),
                    session_id: None,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    agent_id: None,
                    sigil_id: None,
                    space: None,
                    ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            ) {
                let _ = store.insert(&trace);
            }
            render_signal_feed_results(&results);
        }

        Commands::PresencePing {
            space,
            mode,
            model,
            session_id,
            ttl_minutes,
            json,
        } => {
            let store = open_store(&dir);
            let trace = create_presence_trace(
                PresenceTraceConfig {
                    model_id: model,
                    session_id,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    space: space.clone(),
                    mode: mode.clone(),
                    sigil_id: None,
                    capability: None,
                    ttl_minutes,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store
                .insert(&trace)
                .expect("failed to insert presence trace");
            let data = PresencePostData {
                summary: PresenceSummary {
                    status: "active",
                    active_sessions: 1,
                    space: space.clone(),
                },
                mode: mode.clone(),
                ttl_minutes,
                trace_id: hex_encode(&trace.id[..8]),
            };
            if json {
                print_machine_json_with_schema(PRESENCE_SCHEMA_VERSION, "presence-ping", &data);
            } else {
                println!("Presence recorded:");
                if let Some(space) = space {
                    println!("  Space:      {space}");
                }
                println!("  Mode:       {}", mode.unwrap_or_else(|| "active".into()));
                println!("  Fresh for:  {}m", ttl_minutes);
                println!("  Trace ID:   {}", data.trace_id);
            }
        }

        Commands::PresenceFeed {
            hours,
            space,
            limit,
            json,
        } => {
            let store = open_store(&dir);
            let fetch_limit = if space.is_some() {
                limit.max(1).saturating_mul(10)
            } else {
                limit
            };
            let traces = store
                .query_recent_presence_traces(hours, fetch_limit)
                .expect("failed to query recent presence traces");
            let results = summarize_recent_presence(
                &traces,
                space.as_deref(),
                &identity_binding.device_identity,
                identity.public_key_bytes(),
                limit,
            );
            let data = PresenceFeedData {
                summary: PresenceSummary {
                    status: if results.is_empty() {
                        "quiet"
                    } else {
                        "active"
                    },
                    active_sessions: results.len(),
                    space: space.clone(),
                },
                sessions: results,
            };
            if json {
                print_machine_json_with_schema(PRESENCE_SCHEMA_VERSION, "presence-feed", &data);
            } else {
                render_presence_feed_results(&data.sessions);
            }
        }

        Commands::Space {
            space,
            hours,
            limit,
            json,
        } => {
            let store = open_store(&dir);
            let workspace = load_workspace_state(&dir);
            let presence_traces = store
                .query_recent_presence_traces(hours, limit.max(1).saturating_mul(10))
                .expect("failed to query recent presence traces");
            let sessions = summarize_recent_presence(
                &presence_traces,
                Some(&space),
                &identity_binding.device_identity,
                identity.public_key_bytes(),
                limit,
            );
            let signal_traces = store
                .query_recent_signal_traces(hours, None, limit, Some(&space))
                .expect("failed to query recent signal traces");
            let signals = summarize_recent_signal_feed(
                &signal_traces,
                &identity_binding.device_identity,
                identity.public_key_bytes(),
                limit,
            );
            for trace in create_feed_reinforcement_traces(
                &signals,
                SignalTraceConfig {
                    model_id: "thronglets-space".into(),
                    session_id: None,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    agent_id: None,
                    sigil_id: None,
                    space: Some(space.clone()),
                    ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            ) {
                let _ = store.insert(&trace);
            }
            let local_feedback = workspace.space_feedback_summary(Some(&space));
            let continuity_traces = store
                .query_recent_continuity_traces(hours, limit.max(1).saturating_mul(10))
                .expect("failed to query recent continuity traces");
            let continuity = summarize_recent_continuity(&continuity_traces, Some(&space), limit);
            let data = SpaceSnapshotData {
                summary: summarize_space_snapshot(
                    &space,
                    &sessions,
                    &signals,
                    &continuity.summary,
                    &local_feedback,
                ),
                space,
                sessions,
                signals,
                continuity,
                local_feedback,
            };
            if json {
                print_machine_json_with_schema(SPACE_SCHEMA_VERSION, "space", &data);
            } else {
                render_space_snapshot(&data);
            }
        }

        Commands::Run { port, bootstrap } => {
            let store = Arc::new(open_store(&dir));
            let field = Arc::new(PheromoneField::new());
            field.hydrate_from_store(&store);
            let command_tx = start_network_runtime(NetworkRuntimeRequest {
                data_dir: &dir,
                identity: &identity,
                binding: &identity_binding,
                store: Arc::clone(&store),
                field: Some(Arc::clone(&field)),
                listen_port: port,
                bootstrap: &bootstrap,
                options: NetworkRuntimeOptions::node(),
            })
            .await
            .expect("failed to start network");

            info!(
                "Node {} running. Press Ctrl+C to stop.",
                identity.short_id()
            );

            tokio::signal::ctrl_c()
                .await
                .expect("failed to wait for shutdown signal");
            info!("Shutting down...");
            let _ = command_tx
                .send(thronglets::network::NetworkCommand::Shutdown)
                .await;
        }

        Commands::Mcp {
            port,
            bootstrap,
            local,
            agent,
        } => {
            let store = Arc::new(open_store(&dir));
            let field = Arc::new(PheromoneField::new());

            // Restore pheromone field from disk if available
            let field_path = dir.join("pheromone-field.v1.json");
            if field_path.exists()
                && let Ok(data) = std::fs::read_to_string(&field_path)
                && let Ok(snapshot) = serde_json::from_str(&data)
            {
                field.restore(&snapshot);
                tracing::info!(points = field.len(), "Restored pheromone field from disk");
            }

            // Hydrate field from existing traces
            field.hydrate_from_store(&store);

            if let Some(adapter) = agent.and_then(AdapterArg::as_kind) {
                let _ = auto_clear_restart_pending_on_runtime_contact(&dir, adapter);
            }

            // Auto-join P2P network unless --local is specified.
            // Every MCP session = a P2P participant that contributes traces to the collective.
            let network_tx = if !local {
                let p2p_port = port.unwrap_or(0);
                Some(
                    start_network_runtime(NetworkRuntimeRequest {
                        data_dir: &dir,
                        identity: &identity,
                        binding: &identity_binding,
                        store: Arc::clone(&store),
                        field: Some(Arc::clone(&field)),
                        listen_port: p2p_port,
                        bootstrap: &bootstrap,
                        options: NetworkRuntimeOptions::participant(),
                    })
                    .await
                    .expect("failed to start network"),
                )
            } else {
                None
            };

            let ctx = Arc::new(McpContext {
                identity: Arc::new(identity),
                binding: Arc::new(identity_binding),
                store,
                field: Arc::clone(&field),
                network_tx,
            });

            thronglets::mcp::serve_stdio(ctx).await;

            // Persist pheromone field on shutdown
            let snapshot = field.snapshot();
            if !snapshot.points.is_empty()
                && let Ok(data) = serde_json::to_string(&snapshot)
            {
                let _ = std::fs::write(&field_path, data);
            }
        }

        Commands::Anchor {
            rpc,
            chain_id,
            hours,
        } => {
            let store = open_store(&dir);
            let client = AnchorClient::new(&rpc, &chain_id);

            let traces = store
                .unanchored_traces(hours, 500)
                .expect("failed to query unanchored traces");

            if traces.is_empty() {
                println!("No unanchored traces from the last {} hours.", hours);
                return;
            }

            println!(
                "Found {} unanchored traces. Anchoring to {} (chain: {})...",
                traces.len(),
                rpc,
                chain_id
            );

            // Process in batches of 50
            let mut total_anchored: u32 = 0;
            let mut total_skipped: u32 = 0;

            for batch in traces.chunks(50) {
                match client.anchor_batch(&identity, batch) {
                    Ok(result) => {
                        if !result.tx_hash.is_empty() {
                            // Mark each trace in this batch as anchored
                            for trace in batch {
                                let _ = store.mark_anchored(
                                    &trace.id,
                                    0, // block height unknown until chain is live
                                    &result.tx_hash,
                                );
                            }
                            println!(
                                "  Batch tx: {}... ({} anchored, {} skipped)",
                                &result.tx_hash[..16],
                                result.anchored,
                                result.skipped
                            );
                        }
                        total_anchored += result.anchored;
                        total_skipped += result.skipped;
                    }
                    Err(e) => {
                        eprintln!("  Batch failed: {}", e);
                        total_skipped += batch.len() as u32;
                    }
                }
            }

            println!();
            println!("Anchoring complete:");
            println!("  Anchored: {}", total_anchored);
            println!("  Skipped:  {}", total_skipped);
        }

        Commands::Hook => {
            // Read a generic post-tool hook payload from stdin.
            let hook_debug = std::env::var("THRONGLETS_HOOK_DEBUG").is_ok();

            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                if hook_debug {
                    eprintln!("[thronglets:hook] stdin read failed");
                }
                std::process::exit(0);
            }

            if hook_debug {
                eprintln!(
                    "[thronglets:hook] stdin ({} bytes): {}",
                    input.len(),
                    &input[..input.len().min(200)]
                );
            }

            let payload: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(e) => {
                    if hook_debug {
                        eprintln!("[thronglets:hook] JSON parse error: {e}");
                    }
                    std::process::exit(0);
                }
            };

            let tool_name = payload["tool_name"].as_str().unwrap_or("");

            // Skip thronglets' own MCP calls to avoid recursion
            if tool_name.starts_with("mcp__thronglets") {
                std::process::exit(0);
            }

            // Skip empty tool names
            if tool_name.is_empty() {
                std::process::exit(0);
            }

            let agent_source = payload["agent_source"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("claude-code");
            auto_clear_restart_from_agent_source(&dir, agent_source);
            let current_space = payload_string(&payload, "space");
            let current_mode = payload_string(&payload, "mode");

            // Map tool to capability URI
            let capability = if tool_name.starts_with("mcp__") {
                // MCP tools: mcp__server__tool → mcp:server/tool
                tool_name.replacen("mcp__", "mcp:", 1).replace("__", "/")
            } else {
                format!("{agent_source}/{tool_name}")
            };

            // Determine outcome from tool_response
            let tool_response = &payload["tool_response"];
            let outcome = if tool_response.is_null() || tool_response.is_string() {
                // String response = success (Read, Grep, etc.)
                Outcome::Succeeded
            } else if let Some(obj) = tool_response.as_object() {
                if obj.contains_key("error")
                    || obj.get("success") == Some(&serde_json::Value::Bool(false))
                {
                    Outcome::Failed
                } else {
                    Outcome::Succeeded
                }
            } else {
                Outcome::Succeeded
            };

            // Build context from tool_input
            let context_text =
                thronglets::context::build_hook_context(tool_name, &payload["tool_input"]);
            let active_policy = compile_active_policy(&payload, &payload["tool_input"]);
            let method_compliance =
                method_compliance_from_payload(&payload, &active_policy);

            // Input size = rough byte length of tool_input
            let input_size = payload["tool_input"].to_string().len() as u32;

            // Session ID from the calling agent runtime
            let session_id = payload["session_id"].as_str().map(String::from);

            // Model from environment or default
            let model = payload["model"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| std::env::var("CLAUDE_MODEL").ok())
                .unwrap_or_else(|| agent_source.to_string());

            // Load workspace once for both strategy inference and state update
            let mut ws = load_workspace_state(&dir);
            let enriched_context = if let Some(strategy) = ws.infer_strategy() {
                format!("[{strategy}] {context_text}")
            } else {
                context_text.clone()
            };

            let store = open_store(&dir);
            let ctx_hash = simhash(&enriched_context);
            let is_error = matches!(outcome, Outcome::Failed);
            let trace = Trace::new_with_agent_compliance(
                capability.clone(),
                outcome,
                0, // latency not available from hook
                input_size,
                ctx_hash,
                Some(enriched_context),
                session_id.clone(),
                identity_binding.owner_account.clone(),
                Some(identity_binding.device_identity.clone()),
                None,
                None,
                method_compliance,
                model,
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            match store.insert(&trace) {
                Ok(_) => {
                    if hook_debug {
                        eprintln!("[thronglets:hook] recorded {capability}");
                    }
                }
                Err(e) => {
                    if hook_debug {
                        eprintln!("[thronglets:hook] store insert failed: {e}");
                    }
                }
            }
            if current_space.is_some() || current_mode.is_some() {
                let presence = create_presence_trace(
                    PresenceTraceConfig {
                        model_id: trace.model_id.clone(),
                        session_id: session_id.clone(),
                        owner_account: identity_binding.owner_account.clone(),
                        device_identity: Some(identity_binding.device_identity.clone()),
                        space: current_space.clone(),
                        mode: current_mode,
                        sigil_id: None,
                        capability: None,
                        ttl_minutes: DEFAULT_PRESENCE_TTL_MINUTES,
                    },
                    identity.public_key_bytes(),
                    |msg| identity.sign(msg),
                );
                let _ = store.insert(&presence);
            }
            let outcome_str = if is_error { "failed" } else { "succeeded" };

            // Track file interactions
            let file_path = workspace::extract_file_path(tool_name, &payload["tool_input"]);
            if let Some(ref fp) = file_path {
                ws.record_file(fp.clone(), tool_name, context_text.clone(), outcome_str);
            }

            // Track tool call sequence (for decision context)
            ws.record_action(
                tool_name,
                file_path.clone(),
                outcome_str,
                session_id.as_deref(),
            );
            let feedback_events = ws.resolve_recommendation_feedback(
                session_id.as_deref(),
                current_space.as_deref(),
                tool_name,
                file_path.as_deref(),
                outcome_str,
            );

            // Feedback → trace: make signal evaluation visible to the substrate
            for event in &feedback_events {
                let polarity = if event.positive {
                    "positive"
                } else {
                    "negative"
                };
                let feedback_context = format!(
                    "feedback:{} {} {}",
                    polarity, event.recommendation_kind, event.source_kind,
                );
                let feedback_trace = Trace::new_with_identity(
                    "urn:thronglets:signal:feedback".into(),
                    if event.positive {
                        Outcome::Succeeded
                    } else {
                        Outcome::Failed
                    },
                    0,
                    0,
                    simhash(&feedback_context),
                    Some(feedback_context),
                    session_id.clone(),
                    identity_binding.owner_account.clone(),
                    Some(identity_binding.device_identity.clone()),
                    "thronglets-feedback".into(),
                    identity.public_key_bytes(),
                    |msg| identity.sign(msg),
                );
                let _ = store.insert(&feedback_trace);
            }

            // Track pending feedback for Edit/Write
            if matches!(tool_name, "Edit" | "Write")
                && let Some(fp) = file_path
            {
                ws.add_pending_feedback(fp, tool_name);
            }

            // Resolve pending feedback (check git status for previous edits)
            ws.resolve_feedback();

            // Track errors — auto-post avoid signal on repeated failures
            if is_error && let Some(err) = workspace::extract_error(&payload["tool_response"]) {
                let repeated = ws
                    .recent_errors
                    .iter()
                    .take(10)
                    .any(|e| e.context == context_text);
                if repeated {
                    let msg: String = err.chars().take(200).collect();
                    let auto_signal = create_auto_signal_trace(
                        SignalPostKind::Avoid,
                        &context_text,
                        &msg,
                        SignalTraceConfig {
                            model_id: "thronglets-auto".into(),
                            session_id: session_id.clone(),
                            owner_account: identity_binding.owner_account.clone(),
                            device_identity: Some(identity_binding.device_identity.clone()),
                            agent_id: None,
                            sigil_id: None,
                            space: current_space.clone(),
                            ttl_hours: 48,
                        },
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&auto_signal);
                }
                ws.record_error(tool_name, context_text.clone(), err);
            }

            // Auto-watch: cross-file repair associations from traces.db
            // "editing A failed → editing B fixed it" across 2+ sessions = domain knowledge
            if !is_error {
                let watch_error = ws.recent_errors.front().and_then(|prev| {
                    let age_ms = chrono::Utc::now().timestamp_millis() - prev.timestamp_ms;
                    if age_ms < 600_000 && prev.tool != tool_name && prev.context != context_text {
                        Some(prev.context.clone())
                    } else {
                        None
                    }
                });
                if let Some(error_ctx) = watch_error {
                    let error_hash = simhash(&error_ctx);
                    let repair_hash = simhash(&context_text);
                    if let Ok(assoc_count) = store.count_repair_associations(
                        &error_hash,
                        &repair_hash,
                        48,
                        current_space.as_deref(),
                    ) && assoc_count >= 2
                        && !ws.has_recent_auto_signal("watch", &error_ctx, 86_400_000)
                    {
                        let repair_short: String = context_text.chars().take(80).collect();
                        let msg = format!(
                            "{} often follows errors here ({} sessions)",
                            repair_short, assoc_count
                        );
                        let auto_signal = create_auto_signal_trace(
                            SignalPostKind::Watch,
                            &error_ctx,
                            &msg,
                            SignalTraceConfig {
                                model_id: "thronglets-auto".into(),
                                session_id: session_id.clone(),
                                owner_account: identity_binding.owner_account.clone(),
                                device_identity: Some(identity_binding.device_identity.clone()),
                                agent_id: None,
                                sigil_id: None,
                                space: current_space.clone(),
                                ttl_hours: 168,
                            },
                            identity.public_key_bytes(),
                            |msg| identity.sign(msg),
                        );
                        let _ = store.insert(&auto_signal);
                        ws.record_auto_signal("watch", &error_ctx);
                    }
                }
            }

            // ── Correction capture: success after similar failure ──
            // When an action succeeds and a recent similar action failed,
            // the success IS the correction. Anchor a recommend signal to
            // the failed context so next time, agents see what works.
            if !is_error && matches!(tool_name, "Bash" | "Edit" | "Write") {
                let success_hash = simhash(&context_text);
                let now_corr = chrono::Utc::now().timestamp_millis();
                if let Some(corrected_error) = ws.recent_errors.iter().take(5).find(|e| {
                    let age_ms = now_corr - e.timestamp_ms;
                    age_ms < 600_000 && e.context != context_text && {
                        let e_hash = e.context_hash.unwrap_or_else(|| simhash(&e.context));
                        context_similarity(&success_hash, &e_hash) >= 0.65
                    }
                }) {
                    let error_ctx = corrected_error.context.clone();
                    if !ws.has_recent_auto_signal("recommend", &error_ctx, 86_400_000) {
                        let success_short: String = context_text.chars().take(120).collect();
                        let error_short: String = error_ctx.chars().take(60).collect();
                        let msg = format!("{success_short} (replaces: {error_short})");
                        let auto_signal = create_auto_signal_trace(
                            SignalPostKind::Recommend,
                            &error_ctx,
                            &msg,
                            SignalTraceConfig {
                                model_id: "thronglets-auto".into(),
                                session_id: session_id.clone(),
                                owner_account: identity_binding.owner_account.clone(),
                                device_identity: Some(identity_binding.device_identity.clone()),
                                agent_id: None,
                                sigil_id: None,
                                space: current_space.clone(),
                                ttl_hours: 168,
                            },
                            identity.public_key_bytes(),
                            |msg| identity.sign(msg),
                        );
                        let _ = store.insert(&auto_signal);
                        ws.record_auto_signal("recommend", &error_ctx);
                    }
                }
            }

            // Auto-recommend: convergent behavior across 3+ sessions
            if !is_error && matches!(tool_name, "Edit" | "Write" | "Bash") {
                let rec_hash = simhash(&context_text);
                let residue = store
                    .residue_stats_for_context(&rec_hash, 48, 48, 64, current_space.as_deref())
                    .unwrap_or_default();
                let contradictory_failures = residue.total_failure();
                let convergence_threshold = reinforced_success_threshold(
                    &feedback_events,
                    contradictory_failures,
                    residue.success_noncompliant,
                );
                let hard_policy_active = active_policy
                    .relevant_rules
                    .iter()
                    .any(|rule| rule.strength == PolicyStrength::Hard);
                let compliant_success = residue.success_compliant;
                if compliant_success >= convergence_threshold
                    && can_promote_auto_recommend(
                        method_compliance,
                        hard_policy_active,
                        residue.success_noncompliant,
                    )
                    && !ws.has_recent_auto_signal("recommend", &context_text, 86_400_000)
                {
                    let msg = if convergence_threshold <= 2 {
                        format!(
                            "reinforced stable path: {} compliant sessions followed this successfully",
                            compliant_success
                        )
                    } else {
                        format!(
                            "stable path: {} compliant sessions did this successfully",
                            compliant_success
                        )
                    };
                    let auto_signal = create_auto_signal_trace(
                        SignalPostKind::Recommend,
                        &context_text,
                        &msg,
                        SignalTraceConfig {
                            model_id: "thronglets-auto".into(),
                            session_id: session_id.clone(),
                            owner_account: identity_binding.owner_account.clone(),
                            device_identity: Some(identity_binding.device_identity.clone()),
                            agent_id: None,
                            sigil_id: None,
                            space: current_space.clone(),
                            ttl_hours: 168,
                        },
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&auto_signal);
                    ws.record_auto_signal("recommend", &context_text);
                }
            }

            // Hebbian co-edit: files edited together across sessions → recommend signal
            if !is_error
                && matches!(tool_name, "Edit" | "Write")
                && let Some(current_file) = payload["tool_input"]["file_path"]
                    .as_str()
                    .or_else(|| payload["tool_input"]["path"].as_str())
            {
                // Find other files edited in the same session
                let mut co_files: Vec<String> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for action in ws.recent_actions.iter() {
                    if action.session_id.as_deref() == session_id.as_deref()
                        && session_id.is_some()
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

                for other_file in &co_files {
                    let ctx_a = format!("edit file: {}", current_file);
                    let ctx_b = format!("edit file: {}", other_file);
                    let hash_a = simhash(&ctx_a);
                    let hash_b = simhash(&ctx_b);

                    if let Ok(co_count) = store.count_co_occurring_sessions(
                        &hash_a,
                        &hash_b,
                        168,
                        current_space.as_deref(),
                    ) {
                        // Normalize dedup key so (A,B) == (B,A)
                        let dedup_key = if current_file < other_file.as_str() {
                            format!("co:{}+{}", current_file, other_file)
                        } else {
                            format!("co:{}+{}", other_file, current_file)
                        };

                        if co_count >= 2
                            && !ws.has_recent_auto_signal("recommend", &dedup_key, 86_400_000)
                        {
                            // Short filename for readable message
                            let short_name = std::path::Path::new(other_file.as_str())
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(other_file.as_str());
                            let msg =
                                format!("{} usually co-edited ({} sessions)", short_name, co_count);
                            let auto_signal = create_auto_signal_trace(
                                SignalPostKind::Recommend,
                                &ctx_a,
                                &msg,
                                SignalTraceConfig {
                                    model_id: "thronglets-auto".into(),
                                    session_id: session_id.clone(),
                                    owner_account: identity_binding.owner_account.clone(),
                                    device_identity: Some(identity_binding.device_identity.clone()),
                                    agent_id: None,
                                    sigil_id: None,
                                    space: current_space.clone(),
                                    ttl_hours: 168,
                                },
                                identity.public_key_bytes(),
                                |msg| identity.sign(msg),
                            );
                            let _ = store.insert(&auto_signal);
                            ws.record_auto_signal("recommend", &dedup_key);
                        }
                    }
                }
            }

            // Track session
            if let Some(sid) = &session_id {
                ws.track_session(sid, &capability, is_error);
            }

            ws.save(&dir);
        }

        Commands::Prehook => {
            let mut profiler = PrehookProfiler::from_env();
            let hook_debug = std::env::var("THRONGLETS_HOOK_DEBUG").is_ok();

            // Read a generic pre-tool hook payload from stdin.
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                if hook_debug {
                    eprintln!("[thronglets:prehook] stdin read failed");
                }
                std::process::exit(0);
            }

            let payload: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(e) => {
                    if hook_debug {
                        eprintln!("[thronglets:prehook] JSON parse error: {e}");
                    }
                    std::process::exit(0);
                }
            };

            let tool_name = payload["tool_name"].as_str().unwrap_or("");
            let current_space = payload_string(&payload, "space");
            let current_session_id = payload_string(&payload, "session_id");
            let agent_source = payload["agent_source"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("claude-code");
            auto_clear_restart_from_agent_source(&dir, agent_source);

            // Skip thronglets' own calls and empty names
            if tool_name.starts_with("mcp__thronglets") || tool_name.is_empty() {
                std::process::exit(0);
            }

            // ── Pheromone model: silence is normal. Signal only on anomaly. ──
            //
            // Natural systems (ants, slime mold, bees) use ultra-low-bandwidth
            // signals that change behavioral tendency, not verbose reports.
            // Normal paths get no pheromone. Only food and danger get marked.
            //
            // For AI: git history = spatial context (always useful for Edit/Write).
            // Everything else = pheromone (only emitted on anomaly).

            let mut signals: Vec<Signal> = Vec::new();
            let mut ws = load_workspace_state(&dir);
            let current_file = workspace::extract_file_path(tool_name, &payload["tool_input"]);
            let hook_context =
                thronglets::context::build_hook_context(tool_name, &payload["tool_input"]);
            let supports_file_guidance =
                matches!(tool_name, "Edit" | "Write") && current_file.is_some();
            profiler.stage("workspace");

            let mut collective_store: Option<TraceStore> = None;
            let mut collective_queries_remaining = PREHOOK_MAX_COLLECTIVE_QUERIES;

            let mut has_recent_tool_error = false;
            let active_policy = compile_active_policy(&payload, &payload["tool_input"]);

            if let Some(signal) = active_policy_signal(&active_policy) {
                signals.push(signal);
            }

            // ── Danger pheromone: low edit retention ──
            // If recent edits are mostly reverted, this is a strong warning.
            // Only signal when retention < 50% (anomaly).
            if let Some(retention_warning) = ws.retention_warning(current_file.as_deref()) {
                let score = retention_warning.score
                    + ws.recommendation_score_adjustment(
                        SignalKind::Danger,
                        current_space.as_deref(),
                    );
                signals.push(Signal::danger(retention_warning.body, score));
            }

            // ── Alarm pheromone: recent errors with this tool ──
            // Same-tool errors in the last hour always surface (high urgency).
            // Context-similar errors within 7 days surface at lower urgency —
            // this is the "intuition" path: you tried something similar before
            // and it failed, even if it was days ago.
            let now_ms = chrono::Utc::now().timestamp_millis();
            let ctx_hash = simhash(&hook_context);
            if let Some(recent_error) = ws.recent_errors.iter().find(|e| {
                let age_ms = now_ms - e.timestamp_ms;
                if e.tool == tool_name && age_ms < 3_600_000 {
                    return true; // same tool, last hour — always fire
                }
                // Context-similar error within 7 days — experiential recall
                if age_ms < 604_800_000 && !hook_context.is_empty() {
                    let e_hash = e.context_hash.unwrap_or_else(|| simhash(&e.context));
                    return context_similarity(&ctx_hash, &e_hash) >= 0.75;
                }
                false
            }) {
                let signal = {
                    let e = recent_error;
                    let snippet = if e.error_snippet.len() > 80 {
                        format!("{}...", &e.error_snippet[..80])
                    } else {
                        e.error_snippet.clone()
                    };
                    let age_ms = now_ms - e.timestamp_ms;
                    let score = if age_ms < 3_600_000 { 360 } else { 290 };
                    Signal {
                        kind: SignalKind::History,
                        score,
                        body: format!("  ⚠ past error: {snippet}"),
                        candidate: None,
                    }
                };
                has_recent_tool_error = true;
                signals.push(signal);
            }
            profiler.stage("danger");

            let explicit_signals_checked = !hook_context.is_empty();
            if explicit_signals_checked
                && let Some(store) = cached_collective_store(&mut collective_store, &dir)
            {
                for mut sig in explicit_signals(
                    store,
                    &hook_context,
                    &ctx_hash,
                    current_space.as_deref(),
                    &identity_binding.device_identity,
                    identity.public_key_bytes(),
                ) {
                    sig.score +=
                        ws.recommendation_score_adjustment(sig.kind, current_space.as_deref());
                    signals.push(sig);
                }
            }
            profiler.stage_or_skip("explicit_signals", explicit_signals_checked);

            // ── Experience pheromone: past failures from trace history ──
            // When no explicit avoid signal covers this context, query raw
            // failed traces directly. This catches the "first failure" case
            // before the auto-avoid threshold is met — the substrate remembers
            // even when no signal was posted.
            let has_danger = signals.iter().any(|s| matches!(s.kind, SignalKind::Danger));
            let experience_checked = explicit_signals_checked && !has_danger;
            if experience_checked
                && let Some(store) = cached_collective_store(&mut collective_store, &dir)
                && let Ok(failures) = store.query_similar_failed_traces(
                    &ctx_hash,
                    48,
                    168,
                    5,
                    current_space.as_deref(),
                )
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
                let mut score = 280 + (count as i32).min(5) * 20;
                score += ws
                    .recommendation_score_adjustment(SignalKind::History, current_space.as_deref());
                signals.push(Signal {
                    kind: SignalKind::History,
                    score,
                    body: format!(
                        "  ⚠ risk residue: {count} similar failure session(s) ({snippet})"
                    ),
                    candidate: None,
                });
            }
            profiler.stage_or_skip("experience", experience_checked);

            // ── Conflict prior: mixed outcomes mean the environment has not
            // yet settled on a stable path. This remains a lightweight
            // contextual prior, not a command.
            let history_prior_checked = explicit_signals_checked && !has_danger;
            if history_prior_checked
                && let Some(store) = cached_collective_store(&mut collective_store, &dir)
            {
                for mut prior in host_history_priors_for_context(
                    store,
                    &ctx_hash,
                    current_space.as_deref(),
                    None,
                    &active_policy.relevant_rules,
                )
                .into_iter()
                .filter_map(history_signal_from_projection)
                {
                    prior.score += ws
                        .recommendation_score_adjustment(SignalKind::History, current_space.as_deref());
                    signals.push(prior);
                }
            }
            profiler.stage_or_skip("history_priors", history_prior_checked);

            if has_recent_tool_error
                && let Some(repair_hint) = ws
                    .repair_trajectory_hint(tool_name)
                    .or_else(|| ws.repair_hints(tool_name))
            {
                let mut repair_hint = repair_hint;
                repair_hint.score += ws
                    .recommendation_score_adjustment(SignalKind::Repair, current_space.as_deref());
                if claim_collective_query(&repair_hint.candidate, &mut collective_queries_remaining)
                    && let Some(store) = cached_collective_store(&mut collective_store, &dir)
                    && let Ok(collective_sources) =
                        store.count_repair_sources(tool_name, &repair_hint.candidate.steps, 168)
                {
                    apply_collective_sources(
                        &mut repair_hint.candidate,
                        &mut repair_hint.score,
                        collective_sources,
                    );
                }

                signals.push(Signal::repair_candidate(
                    repair_hint.body,
                    repair_hint.score,
                    repair_hint.candidate,
                ));
            }
            profiler.stage("repair");

            let presence_checked =
                current_space.is_some() && (!supports_file_guidance || !signals.is_empty());
            if presence_checked
                && let Some(store) = cached_collective_store(&mut collective_store, &dir)
                && let Some(space) = current_space.as_deref()
                && let Some(presence_signal) = presence_context_signal(
                    store,
                    space,
                    current_session_id.as_deref(),
                    &identity_binding.device_identity,
                    identity.public_key_bytes(),
                )
            {
                signals.push(presence_signal);
            }
            profiler.stage_or_skip("presence", presence_checked);

            // History is a fallback when we don't already know a likely next move.
            let has_higher_priority_signal = signals
                .iter()
                .any(|signal| signal.kind != SignalKind::History);
            let mut git_checked = false;
            if !has_higher_priority_signal && supports_file_guidance {
                git_checked = true;
                if let Some(git_hints) = current_file
                    .as_ref()
                    .and_then(|fp| git_file_history(fp, GIT_HISTORY_MAX_ENTRIES))
                {
                    signals.push(Signal::history(git_hints));
                }
            }
            profiler.stage_or_skip("git", git_checked);

            // Guardrail: prehook stays short and category-stable.
            let recommendations = ws.suppress_duplicate_recommendations(
                current_session_id.as_deref(),
                current_space.as_deref(),
                select_signals(signals, PREHOOK_MAX_HINTS),
            );
            if !recommendations.is_empty() {
                ws.record_intervention(
                    tool_name,
                    recommendations
                        .iter()
                        .map(|recommendation| recommendation.source_kind.as_str().to_string())
                        .collect(),
                );
                ws.record_recommendation_emissions(
                    tool_name,
                    current_session_id.as_deref(),
                    current_space.as_deref(),
                    &recommendations,
                );
                ws.save(&dir);
            }
            profiler.stage("select");

            // Output: only when there's something worth saying
            let mut stdout_bytes = 0;
            if !recommendations.is_empty() {
                stdout_bytes += PREHOOK_HEADER.len() + 1;
                println!("{PREHOOK_HEADER}");
                for recommendation in &recommendations {
                    let rendered = recommendation.render();
                    stdout_bytes += rendered.len() + 1;
                    println!("{rendered}");
                }
            }
            profiler.finish(
                tool_name,
                &recommendations,
                stdout_bytes,
                profile_file_guidance_gate(supports_file_guidance),
                PREHOOK_MAX_COLLECTIVE_QUERIES - collective_queries_remaining,
            );
            // Normal state → complete silence. Zero tokens.
        }

        Commands::LifecycleHook { event } => {
            // Read lifecycle hook payload from stdin.
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            let payload: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(_) => std::process::exit(0),
            };

            let session_id = payload_string(&payload, "session_id");
            let current_space = payload_string(&payload, "space");

            let store = open_store(&dir);
            let mut ws = load_workspace_state(&dir);

            match event.as_str() {
                "session-start" => {
                    let source = payload["source"].as_str().unwrap_or("startup");
                    let model = payload["model"]
                        .as_str()
                        .unwrap_or("claude-code")
                        .to_string();

                    // Record lifecycle trace
                    let ctx = format!("session:{} source:{}", source, model);
                    let trace = Trace::new_with_identity(
                        "urn:thronglets:lifecycle:session-start".into(),
                        Outcome::Succeeded,
                        0,
                        0,
                        simhash(&ctx),
                        Some(ctx),
                        session_id.clone(),
                        identity_binding.owner_account.clone(),
                        Some(identity_binding.device_identity.clone()),
                        model.clone(),
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&trace);

                    // Emit presence
                    let presence = create_presence_trace(
                        PresenceTraceConfig {
                            model_id: model,
                            session_id: session_id.clone(),
                            owner_account: identity_binding.owner_account.clone(),
                            device_identity: Some(identity_binding.device_identity.clone()),
                            space: current_space.clone(),
                            mode: payload_string(&payload, "mode"),
                            sigil_id: None,
                            capability: None,
                            ttl_minutes: DEFAULT_PRESENCE_TTL_MINUTES,
                        },
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&presence);

                    // Initialize session in workspace
                    if let Some(sid) = &session_id {
                        ws.track_session(sid, "lifecycle/session-start", false);
                    }

                    // ── Session briefing: experience as background knowledge ──
                    // Surface accumulated knowledge as neutral facts, not labeled
                    // warnings. No branding — this becomes part of the agent's
                    // "native" context, indistinguishable from its own knowledge.
                    let mut notes: Vec<String> = Vec::new();

                    // Corrections first (recommend signals) — what works
                    if let Ok(recs) = store.query_recent_signal_traces(
                        168,
                        Some(SignalPostKind::Recommend),
                        5,
                        current_space.as_deref(),
                    ) {
                        for sig in &recs {
                            if let Some(msg) = sig
                                .context_text
                                .as_deref()
                                .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                                .and_then(|v| v["message"].as_str().map(String::from))
                            {
                                let short: String = msg.chars().take(120).collect();
                                notes.push(format!("- {short}"));
                            }
                        }
                    }

                    // Known failures (avoid signals) — what doesn't work
                    if let Ok(avoids) = store.query_recent_signal_traces(
                        48,
                        Some(SignalPostKind::Avoid),
                        3,
                        current_space.as_deref(),
                    ) {
                        for sig in &avoids {
                            if let Some((ctx, msg)) = sig
                                .context_text
                                .as_deref()
                                .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                                .and_then(|v| {
                                    let c = v["context"].as_str()?.to_string();
                                    let m = v["message"].as_str()?.to_string();
                                    Some((c, m))
                                })
                            {
                                let short_ctx: String = ctx.chars().take(40).collect();
                                let short_msg: String = msg.chars().take(60).collect();
                                notes.push(format!("- {short_ctx}: {short_msg}"));
                            }
                        }
                    }

                    // Deduplicate and limit
                    notes.dedup();
                    notes.truncate(5);

                    if !notes.is_empty() {
                        let briefing = format!("Environment notes:\n{}", notes.join("\n"));
                        let output = serde_json::json!({ "additionalContext": briefing });
                        println!("{}", output);
                    }

                    ws.save(&dir);
                }

                "session-end" => {
                    let ctx = format!(
                        "session-end source:{}",
                        payload["source"].as_str().unwrap_or("end"),
                    );
                    let trace = Trace::new_with_identity(
                        "urn:thronglets:lifecycle:session-end".into(),
                        Outcome::Succeeded,
                        0,
                        0,
                        simhash(&ctx),
                        Some(ctx),
                        session_id.clone(),
                        identity_binding.owner_account.clone(),
                        Some(identity_binding.device_identity.clone()),
                        "thronglets-lifecycle".into(),
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&trace);
                    ws.save(&dir);
                }

                "subagent-start" => {
                    let agent_type = payload["agent_type"].as_str().unwrap_or("unknown");
                    let agent_id =
                        payload_string(&payload, "agent_id").unwrap_or_else(|| "anon".into());
                    let ctx = format!("subagent-start type:{} id:{}", agent_type, agent_id);
                    let trace = Trace::new_with_identity(
                        "urn:thronglets:lifecycle:subagent-start".into(),
                        Outcome::Succeeded,
                        0,
                        0,
                        simhash(&ctx),
                        Some(ctx),
                        session_id.clone(),
                        identity_binding.owner_account.clone(),
                        Some(identity_binding.device_identity.clone()),
                        "thronglets-lifecycle".into(),
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&trace);
                }

                "subagent-stop" => {
                    let agent_type = payload["agent_type"].as_str().unwrap_or("unknown");
                    let agent_id =
                        payload_string(&payload, "agent_id").unwrap_or_else(|| "anon".into());
                    // Extract a summary fingerprint from last_assistant_message if present
                    let summary: String = payload["last_assistant_message"]
                        .as_str()
                        .map(|m| m.chars().take(200).collect())
                        .unwrap_or_default();
                    let ctx = if summary.is_empty() {
                        format!("subagent-stop type:{} id:{}", agent_type, agent_id)
                    } else {
                        format!(
                            "subagent-stop type:{} id:{} summary:{}",
                            agent_type, agent_id, summary
                        )
                    };
                    let trace = Trace::new_with_identity(
                        "urn:thronglets:lifecycle:subagent-stop".into(),
                        Outcome::Succeeded,
                        0,
                        0,
                        simhash(&ctx),
                        Some(ctx),
                        session_id.clone(),
                        identity_binding.owner_account.clone(),
                        Some(identity_binding.device_identity.clone()),
                        "thronglets-lifecycle".into(),
                        identity.public_key_bytes(),
                        |msg| identity.sign(msg),
                    );
                    let _ = store.insert(&trace);
                }

                _ => {} // Unknown event — silent exit
            }
        }

        Commands::Setup
        | Commands::Detect { .. }
        | Commands::InstallPlan { .. }
        | Commands::ApplyPlan { .. }
        | Commands::Doctor { .. }
        | Commands::Bootstrap { .. }
        | Commands::ClearRestart { .. }
        | Commands::RuntimeReady { .. } => {
            unreachable!("adapter surfaces handled before identity bootstrap")
        }

        Commands::Serve {
            port,
            p2p_port,
            bootstrap,
            local,
        } => {
            let store = Arc::new(open_store(&dir));
            let field = Arc::new(PheromoneField::new());

            // Restore pheromone field from disk if available
            let field_path = dir.join("pheromone-field.v1.json");
            if field_path.exists()
                && let Ok(data) = std::fs::read_to_string(&field_path)
                && let Ok(snapshot) = serde_json::from_str(&data)
            {
                field.restore(&snapshot);
                tracing::info!(points = field.len(), "Restored pheromone field from disk");
            }
            field.hydrate_from_store(&store);

            // Auto-join P2P network unless --local is specified.
            let _network_tx = if !local {
                Some(
                    start_network_runtime(NetworkRuntimeRequest {
                        data_dir: &dir,
                        identity: &identity,
                        binding: &identity_binding,
                        store: Arc::clone(&store),
                        field: Some(Arc::clone(&field)),
                        listen_port: p2p_port,
                        bootstrap: &bootstrap,
                        options: NetworkRuntimeOptions::participant(),
                    })
                    .await
                    .expect("failed to start network"),
                )
            } else {
                None
            };

            let ctx = Arc::new(thronglets::http::HttpContext {
                identity: Arc::new(identity),
                binding: Arc::new(identity_binding),
                store,
                data_dir: dir.clone(),
            });
            println!("Thronglets HTTP API on http://0.0.0.0:{port}");
            if !local {
                println!("  P2P network joined (port {p2p_port}, 0 = random)");
            }
            println!("  POST /v1/traces       — record a trace");
            println!("  POST /v1/presence     — leave a lightweight session presence heartbeat");
            println!("  POST /v1/signals      — leave an explicit short signal");
            println!("  GET  /v1/presence/feed — show recent active sessions in a space");
            println!("  GET  /v1/signals      — query explicit short signals");
            println!("  GET  /v1/signals/feed — show recent converging explicit signals");
            println!("  GET  /v1/query        — query the substrate");
            println!("  GET  /v1/capabilities — list capabilities");
            println!("  GET  /v1/status       — node status");
            thronglets::http::serve(ctx, port)
                .await
                .expect("HTTP server failed");

            // Persist pheromone field on shutdown
            let snapshot = field.snapshot();
            if !snapshot.points.is_empty()
                && let Ok(data) = serde_json::to_string(&snapshot)
            {
                let _ = std::fs::write(&field_path, data);
            }
        }

        Commands::Peers { json, limit } => {
            let snapshot = thronglets::network_state::NetworkSnapshot::load(&dir);
            let status = snapshot.to_status();
            let peers: Vec<_> = snapshot.peers.into_iter().take(limit).collect();
            let data = PeersData {
                summary: PeersSummary {
                    status: status.activity,
                    connected_peers: status.peer_count,
                    known_peers: status.known_peer_count,
                    trusted_peer_seed_count: status.trusted_peer_seed_count,
                    peer_seed_count: status.peer_seed_count,
                    bootstrap_targets: status.bootstrap_targets,
                    vps_dependency_level: status.vps_dependency_level,
                },
                peers,
            };
            if json {
                print_machine_json_with_schema(NETWORK_SCHEMA_VERSION, "peers", &data);
            } else if data.peers.is_empty() {
                println!("No peers observed yet.");
                println!(
                    "Network status: {} ({})",
                    status.activity, status.vps_dependency_level
                );
            } else {
                println!(
                    "Known peers: {} ({} currently connected, {} trusted seeds, {} total seeds, dependency {})",
                    data.summary.known_peers,
                    data.summary.connected_peers,
                    data.summary.trusted_peer_seed_count,
                    data.summary.peer_seed_count,
                    data.summary.vps_dependency_level
                );
                let now = chrono::Utc::now().timestamp_millis();
                for peer in &data.peers {
                    let age_s = (now.saturating_sub(peer.last_seen_at_ms)) / 1000;
                    let state = if peer.connected { "connected" } else { "seen" };
                    println!("  {state}: {} ({age_s}s ago)", peer.peer_id);
                    if !peer.addresses.is_empty() {
                        println!("    addrs: {}", peer.addresses.join(", "));
                    }
                }
            }
        }

        Commands::NetCheck {
            json,
            bootstrap_offline,
        } => {
            let status = thronglets::network_state::NetworkSnapshot::load(&dir).to_status();
            let data = summarize_net_check_for_scenario(&status, bootstrap_offline);
            if json {
                print_machine_json_with_schema(NETWORK_SCHEMA_VERSION, "net-check", &data);
            } else {
                render_net_check(&data);
            }
        }

        Commands::Status { json } => {
            let home_dir = home_dir();
            let data = collect_status_data(&home_dir, &dir, &identity, &identity_binding);
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "status", &data);
            } else {
                render_status_report(&data, identity_binding.owner_account_or_unbound());
            }
        }

        Commands::ProfileSummary => {
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            if let Some(summary) = summarize_prehook_profiles(&input) {
                println!("{}", summary.render());
            } else {
                println!("no prehook profile samples found");
            }
        }

        Commands::ProfileCheck => {
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            if let Some(summary) = summarize_prehook_profiles(&input) {
                let (passed, rendered) = summary.render_check(&ProfileCheckThresholds::default());
                println!("{rendered}");
                if !passed {
                    std::process::exit(1);
                }
            } else {
                println!("FAIL");
                println!("violations: no prehook profile samples found");
                std::process::exit(1);
            }
        }

        Commands::ReleaseCheck {
            hours,
            max_sessions,
            project_root,
            eval_scope,
            global,
            require_profile_samples,
            compare_baseline,
            json,
        } => {
            let mut input = String::new();
            let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
            let profile_thresholds = ProfileCheckThresholds::default();
            let effective_eval_scope = if global {
                ReleaseEvalScopeArg::Global
            } else {
                eval_scope
            };
            let baseline = compare_baseline
                .as_deref()
                .map(load_eval_baseline)
                .transpose()
                .expect("failed to load eval baseline");

            let profile_section = match summarize_prehook_profiles(&input) {
                Some(summary) => {
                    let violations = summary.check(&profile_thresholds);
                    let passed = violations.is_empty();
                    let (_, rendered) = summary.render_check(&profile_thresholds);
                    (
                        if passed { "PASS" } else { "FAIL" },
                        !passed,
                        strip_check_header(&rendered),
                        serde_json::json!({
                            "status": if passed { "PASS" } else { "FAIL" },
                            "thresholds": profile_thresholds,
                            "summary": summary,
                            "violations": violations,
                            "notes": Vec::<String>::new(),
                        }),
                    )
                }
                None => {
                    let status = if require_profile_samples {
                        "FAIL"
                    } else {
                        "SKIP"
                    };
                    let violations = if require_profile_samples {
                        vec!["no prehook profile samples found".to_string()]
                    } else {
                        Vec::new()
                    };
                    let notes = if require_profile_samples {
                        Vec::new()
                    } else {
                        vec!["no prehook profile samples found".to_string()]
                    };
                    (
                        status,
                        require_profile_samples,
                        if require_profile_samples {
                            "violations: no prehook profile samples found".to_string()
                        } else {
                            "notes: no prehook profile samples found".to_string()
                        },
                        serde_json::json!({
                            "status": status,
                            "thresholds": profile_thresholds,
                            "summary": serde_json::Value::Null,
                            "violations": violations,
                            "notes": notes,
                        }),
                    )
                }
            };

            let home_dir = home_dir();
            let doctor_section = run_release_doctor_section(&home_dir, &dir);

            let eval_thresholds = EvalCheckThresholds::default();
            let store = open_store(&dir);
            let local_feedback = LocalFeedbackSummary::from_workspace(&load_workspace_state(&dir));
            let default_project_root = project_root.unwrap_or_else(|| {
                std::env::current_dir().expect("failed to determine current working directory")
            });
            let eval_sections: Vec<_> = match effective_eval_scope {
                ReleaseEvalScopeArg::Project => vec![(
                    "project",
                    run_release_eval_section(
                        &store,
                        hours,
                        max_sessions,
                        Some(default_project_root.as_path()),
                        local_feedback.clone(),
                        &eval_thresholds,
                        baseline.as_ref(),
                    ),
                )],
                ReleaseEvalScopeArg::Global => vec![(
                    "global",
                    run_release_eval_section(
                        &store,
                        hours,
                        max_sessions,
                        None,
                        None,
                        &eval_thresholds,
                        baseline.as_ref(),
                    ),
                )],
                ReleaseEvalScopeArg::Both => vec![
                    (
                        "project",
                        run_release_eval_section(
                            &store,
                            hours,
                            max_sessions,
                            Some(default_project_root.as_path()),
                            local_feedback,
                            &eval_thresholds,
                            baseline.as_ref(),
                        ),
                    ),
                    (
                        "global",
                        run_release_eval_section(
                            &store,
                            hours,
                            max_sessions,
                            None,
                            None,
                            &eval_thresholds,
                            baseline.as_ref(),
                        ),
                    ),
                ],
            };

            let overall_failed = profile_section.1
                || doctor_section.1
                || eval_sections.iter().any(|(_, section)| section.1);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": if overall_failed { "FAIL" } else { "PASS" },
                        "eval_scope": match effective_eval_scope {
                            ReleaseEvalScopeArg::Project => "project",
                            ReleaseEvalScopeArg::Global => "global",
                            ReleaseEvalScopeArg::Both => "both",
                        },
                        "profile": profile_section.3,
                        "doctor": doctor_section.3,
                        "eval": if eval_sections.len() == 1 {
                            eval_sections[0].1.3.clone()
                        } else {
                            serde_json::json!({
                                "project": eval_sections[0].1.3.clone(),
                                "global": eval_sections[1].1.3.clone(),
                            })
                        },
                    }))
                    .expect("failed to serialize release check")
                );
            } else {
                println!("{}", if overall_failed { "FAIL" } else { "PASS" });
                print_release_section("profile", profile_section.0, &profile_section.2);
                print_release_section("doctor", doctor_section.0, &doctor_section.2);
                if eval_sections.len() == 1 {
                    let (label, section) = &eval_sections[0];
                    print_release_section(&format!("eval ({label})"), section.0, &section.2);
                } else {
                    for (label, section) in &eval_sections {
                        print_release_section(&format!("eval ({label})"), section.0, &section.2);
                    }
                }
            }
            if overall_failed {
                std::process::exit(1);
            }
        }

        Commands::EvalSignals {
            hours,
            max_sessions,
            project_root,
            global,
            local_history_gate_min,
            pattern_support_min,
            compare_baseline,
            top_breakdowns,
            focus,
            json,
        } => {
            let store = open_store(&dir);
            let project_scope = if global {
                None
            } else {
                Some(project_root.unwrap_or_else(|| {
                    std::env::current_dir().expect("failed to determine current working directory")
                }))
            };
            let eval_config = EvalConfig {
                local_history_gate_min,
                pattern_support_min,
            };
            let default_config = EvalConfig::default();
            match evaluate_signal_quality(
                &store,
                hours,
                max_sessions,
                project_scope.as_deref(),
                eval_config,
            )
            .expect("failed to evaluate signal quality")
            {
                Some(summary) => {
                    let summary = if eval_config != default_config {
                        match evaluate_signal_quality(
                            &store,
                            hours,
                            max_sessions,
                            project_scope.as_deref(),
                            default_config,
                        )
                        .expect("failed to evaluate default signal quality")
                        {
                            Some(baseline) => summary.with_comparison_to_default(&baseline),
                            None => summary,
                        }
                    } else {
                        summary
                    }
                    .with_local_feedback(if project_scope.is_some() {
                        LocalFeedbackSummary::from_workspace(&load_workspace_state(&dir))
                    } else {
                        None
                    });
                    let summary = if let Some(baseline_path) = compare_baseline.as_ref() {
                        summary.with_comparison_to_baseline(
                            &load_eval_baseline(baseline_path)
                                .expect("failed to load eval baseline"),
                        )
                    } else {
                        summary
                    };
                    let summary = summary.focused(focus.into(), top_breakdowns);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&summary)
                                .expect("failed to serialize evaluation summary")
                        );
                    } else {
                        println!("{}", summary.render());
                    }
                }
                None => {
                    if json {
                        println!("null");
                    } else {
                        println!("not enough recent session history to evaluate signals yet");
                    }
                }
            }
        }
    }
}

/// Get recent git history for a file. Returns None if not in a git repo or no history.
fn git_file_history(file_path: &str, max_entries: usize) -> Option<String> {
    use std::path::Path;
    use std::process::Command;

    let path = Path::new(file_path);
    let dir = path.parent()?;

    // git log --oneline --no-decorate -n N -- <file>
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

fn payload_string(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload[key]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn apply_collective_sources(
    candidate: &mut StepCandidate,
    score: &mut i32,
    collective_sources: u32,
) {
    *score += candidate.upgrade_collective_sources(collective_sources);
}

/// Query all explicit signal kinds (avoid, watch, recommend) and return matching signals.
fn explicit_signals(
    store: &TraceStore,
    hook_context: &str,
    context_hash: &[u8; 16],
    space: Option<&str>,
    local_device_identity: &str,
    local_node_pubkey: [u8; 32],
) -> Vec<Signal> {
    // Query ALL signal kinds at once (kind: None)
    let traces = match store.query_signal_traces(context_hash, None, 48, 10, space) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let results = summarize_signal_traces(
        &traces,
        hook_context,
        local_device_identity,
        local_node_pubkey,
        6,
    );

    let mut signals = Vec::new();
    for result in results {
        if result.density_score < 1 || result.context_similarity < 0.85 {
            continue;
        }
        let collective_bonus = if result.promotion_state == "collective" {
            30
        } else {
            0
        };
        let density_bonus = i32::from(result.density_score) * 20;

        match result.kind.as_str() {
            "avoid" => {
                let score = 300 + density_bonus + collective_bonus;
                signals.push(Signal::danger(
                    format!("  ⚠ avoid: {}", result.message),
                    score,
                ));
            }
            "watch" => {
                let score = 200 + density_bonus + collective_bonus;
                // Informational — "be careful" not "don't do it".
                signals.push(Signal {
                    kind: SignalKind::History,
                    score,
                    body: format!("  👁 watch: {}", result.message),
                    candidate: None,
                });
            }
            "recommend" => {
                let score = 150 + density_bonus + collective_bonus;
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

fn history_signal_from_projection(prior: AmbientPriorProjection) -> Option<Signal> {
    let (score, body) = match (prior.kind, prior.policy_state) {
        ("success-prior", Some(AmbientPolicyState::StablePath)) => (
            140 + (prior.confidence * 60.0).round() as i32,
            format!("  ✓ stable path: {}", strip_success_prior_prefix(&prior.summary)),
        ),
        ("mixed-residue", Some(AmbientPolicyState::PolicyConflict)) => {
            (235, format!("  ! {}", prior.summary))
        }
        ("mixed-residue", Some(AmbientPolicyState::MethodConflict)) => {
            (205, format!("  ~ {}", prior.summary))
        }
        ("mixed-residue", _) => (
            190,
            format!("  ~ unsettled path: {}", strip_mixed_residue_prefix(&prior.summary)),
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

fn reinforced_success_threshold(
    feedback_events: &[workspace::RecommendationFeedbackEvent],
    contradictory_failures: u32,
    noncompliant_successes: u32,
) -> u32 {
    let mut reinforced = false;
    let mut contradicted = false;
    for event in feedback_events {
        let relevant_recommendation =
            matches!(event.recommendation_kind.as_str(), "do_next" | "maybe_also");
        let relevant_source = matches!(
            event.source_kind.as_str(),
            "repair" | "preparation" | "adjacency"
        );
        if !(relevant_recommendation && relevant_source) {
            continue;
        }
        if event.positive {
            reinforced = true;
        } else {
            contradicted = true;
        }
    }

    let base_threshold = if reinforced && !contradicted { 2 } else { 3 };
    let contradiction_floor = contradictory_failures
        .saturating_add(noncompliant_successes)
        .saturating_add(2);
    let feedback_floor = if contradicted {
        base_threshold.max(4)
    } else {
        base_threshold
    };
    feedback_floor.max(contradiction_floor)
}

fn active_policy_signal(active_policy: &ActivePolicySet) -> Option<Signal> {
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
    if hard {
        Some(Signal::danger(format!("  ! active policy: {summaries}"), 340))
    } else {
        Some(Signal {
            kind: SignalKind::History,
            score: 205,
            body: format!("  ~ active method guidance: {summaries}"),
            candidate: None,
        })
    }
}

fn can_promote_auto_recommend(
    method_compliance: Option<MethodCompliance>,
    hard_policy_active: bool,
    noncompliant_successes: u32,
) -> bool {
    if matches!(method_compliance, Some(MethodCompliance::Noncompliant)) {
        return false;
    }
    if hard_policy_active && noncompliant_successes > 0 {
        return false;
    }
    true
}

fn presence_context_signal(
    store: &TraceStore,
    space: &str,
    current_session_id: Option<&str>,
    local_device_identity: &str,
    local_node_pubkey: [u8; 32],
) -> Option<Signal> {
    let traces = store.query_recent_presence_traces(1, 24).ok()?;
    let active_sessions: Vec<_> = summarize_recent_presence(
        &traces,
        Some(space),
        local_device_identity,
        local_node_pubkey,
        8,
    )
    .into_iter()
    .filter(|session| {
        current_session_id.is_none_or(|sid| session.session_id.as_deref() != Some(sid))
    })
    .collect();

    if active_sessions.is_empty() {
        return None;
    }

    let preview_limit = 2;
    let collective = active_sessions
        .iter()
        .any(|session| session.evidence_scope == "collective");
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
        score: if collective { 145 } else { 135 },
        body: format!("  active in space {space}: {preview}{suffix}"),
        candidate: None,
    })
}

fn claim_collective_query(candidate: &StepCandidate, remaining_queries: &mut usize) -> bool {
    if *remaining_queries == 0 || candidate.source_count >= 2 {
        return false;
    }

    *remaining_queries -= 1;
    true
}

fn cached_collective_store<'a>(
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
enum ProfileStageState {
    Timed(u128),
    Skipped,
}

struct PrehookProfiler {
    enabled: bool,
    started_at: Instant,
    stage_started_at: Instant,
    stages: Vec<(&'static str, ProfileStageState)>,
}

impl PrehookProfiler {
    fn from_env() -> Self {
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

    fn stage(&mut self, name: &'static str) {
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

    fn stage_or_skip(&mut self, name: &'static str, executed: bool) {
        if !self.enabled {
            return;
        }

        if executed {
            self.stage(name);
        } else {
            self.stages.push((name, ProfileStageState::Skipped));
        }
    }

    fn finish(
        &self,
        tool_name: &str,
        recommendations: &[Recommendation],
        stdout_bytes: usize,
        file_guidance_gate: &'static str,
        collective_queries_used: usize,
    ) {
        if !self.enabled {
            return;
        }

        let mut parts = vec![
            format!("tool={tool_name}"),
            format!("emitted={}", recommendations.len()),
            format!("stdout_bytes={stdout_bytes}"),
            format!("output_mode={}", profile_output_mode(recommendations)),
            format!("decision_path={}", profile_decision_path(recommendations)),
            format!("evidence_scope={}", profile_evidence_scope(recommendations)),
            format!("file_guidance_gate={file_guidance_gate}"),
            format!("collective_queries_used={collective_queries_used}"),
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

fn profile_output_mode(recommendations: &[Recommendation]) -> &'static str {
    if recommendations.is_empty() {
        "silent"
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

fn profile_decision_path(recommendations: &[Recommendation]) -> &'static str {
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
        .unwrap_or("none")
}

fn profile_evidence_scope(recommendations: &[Recommendation]) -> &'static str {
    recommendations
        .iter()
        .find_map(|r| r.candidate.as_ref())
        .map(|candidate| match candidate.evidence_scope {
            thronglets::signals::EvidenceScope::Local => "local",
            thronglets::signals::EvidenceScope::Collective => "collective",
        })
        .unwrap_or("none")
}

fn profile_file_guidance_gate(supports_file_guidance: bool) -> &'static str {
    if supports_file_guidance { "open" } else { "na" }
}

fn strip_check_header(rendered: &str) -> String {
    rendered.lines().skip(1).collect::<Vec<_>>().join("\n")
}

fn run_release_eval_section(
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

fn release_baseline_check(
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
        && delta < -RELEASE_MAX_LOCAL_RETENTION_DROP_TENTHS_PP
    {
        violations.push(format!(
            "local edit retention regressed by {}",
            format_release_option_tenths_pp(Some(delta))
        ));
    }
    if comparison.failed_command_rate_delta_tenths_pp
        > RELEASE_MAX_FAILED_COMMAND_RATE_RISE_TENTHS_PP
    {
        violations.push(format!(
            "failed command rate regressed by {}",
            format_release_tenths_pp(comparison.failed_command_rate_delta_tenths_pp)
        ));
    }
    if let Some(delta) = comparison.first_successful_change_latency_avg_delta_ms
        && delta > RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS
    {
        violations.push(format!(
            "first successful change latency avg regressed by {}",
            format_release_option_ms(Some(delta))
        ));
    }
    if let Some(delta) = comparison.first_successful_change_latency_p50_delta_ms
        && delta > RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS
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

fn render_release_baseline_outcome_line(comparison: &EvalBaselineComparison) -> String {
    format!(
        "vs baseline ({} scored): retention {}, failed cmds {}, first change avg {}, p50 {}",
        comparison.baseline_sessions_scored,
        format_release_option_tenths_pp(comparison.local_retention_delta_tenths_pp),
        format_release_tenths_pp(comparison.failed_command_rate_delta_tenths_pp),
        format_release_option_ms(comparison.first_successful_change_latency_avg_delta_ms),
        format_release_option_ms(comparison.first_successful_change_latency_p50_delta_ms),
    )
}

fn render_release_baseline_signal_line(comparison: &EvalBaselineComparison) -> String {
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

fn format_release_tenths_pp(delta: i32) -> String {
    format!("{:+}.{}pp", delta / 10, delta.abs() % 10)
}

fn format_release_option_tenths_pp(delta: Option<i32>) -> String {
    delta
        .map(format_release_tenths_pp)
        .unwrap_or_else(|| "n/a".into())
}

fn format_release_option_ms(delta: Option<i64>) -> String {
    match delta {
        Some(delta) => format!("{delta:+}ms"),
        None => "n/a".into(),
    }
}

fn run_release_doctor_section(
    home_dir: &Path,
    data_dir: &Path,
) -> (&'static str, bool, String, serde_json::Value) {
    let reports: Vec<_> = selected_adapters(AdapterArg::All)
        .into_iter()
        .map(|adapter| doctor_adapter(home_dir, data_dir, adapter))
        .collect();
    let summary = summarize_doctor_reports(AdapterArg::All, reports);
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

fn load_eval_baseline(path: &Path) -> Result<thronglets::eval::SignalEvalSummary, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read baseline {}: {err}", path.display()))?;
    serde_json::from_str(&raw).map_err(|err| format!("parse baseline {}: {err}", path.display()))
}

fn print_release_section(name: &str, status: &str, body: &str) {
    println!("{name}: {status}");
    for line in body.lines() {
        println!("  {line}");
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use thronglets::ambient::ambient_priors_for_context;
    use thronglets::identity::NodeIdentity;
    use thronglets::posts::{DEFAULT_SIGNAL_TTL_HOURS, SignalTraceConfig, create_signal_trace};
    use thronglets::storage::TraceStore;

    fn eval_summary(
        retention_percent: u32,
        failed_calls: usize,
        command_calls: usize,
        avg_latency_ms: Option<u64>,
        p50_latency_ms: Option<u64>,
    ) -> SignalEvalSummary {
        SignalEvalSummary {
            project_scope: Some("/tmp/project".into()),
            eval_config: EvalConfig::default(),
            local_feedback: Some(LocalFeedbackSummary {
                resolved_edits: 10,
                committed_edits: (retention_percent / 10) as usize,
                reverted_edits: 0,
                retention_percent,
            }),
            comparison_to_default: None,
            comparison_to_baseline: None,
            sessions_considered: 8,
            sessions_scored: 8,
            holdout_command_calls: command_calls,
            holdout_failed_command_calls: failed_calls,
            sessions_with_successful_change: 8,
            first_successful_change_latency_avg_ms: avg_latency_ms,
            first_successful_change_latency_p50_ms: p50_latency_ms,
            edit_points: 20,
            edit_points_with_signal: 1,
            repair_opportunities: 4,
            repair_predictions: 3,
            repair_first_step_hits: 2,
            repair_exact_hits: 1,
            preparation_gated_edit_points: 2,
            preparation_predictions: 5,
            preparation_hits: 2,
            adjacency_gated_edit_points: 2,
            adjacency_predictions: 5,
            adjacency_hits: 2,
            repair_breakdown: BTreeMap::new(),
            preparation_breakdown: BTreeMap::new(),
            adjacency_breakdown: BTreeMap::new(),
        }
    }

    #[test]
    fn release_baseline_check_fails_on_outcome_regressions() {
        let baseline = eval_summary(90, 0, 10, Some(1_000), Some(1_000));
        let candidate = eval_summary(80, 2, 10, Some(7_000), Some(7_000))
            .with_comparison_to_baseline(&baseline);

        let check = release_baseline_check(
            candidate.comparison_to_baseline.as_ref(),
            EvalCheckStatus::Pass,
        );

        assert_eq!(check.status, "FAIL");
        assert_eq!(check.violations.len(), 4);
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("local edit retention"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("failed command rate"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("first successful change latency avg"))
        );
        assert!(
            check
                .violations
                .iter()
                .any(|v| v.contains("first successful change latency p50"))
        );
    }

    #[test]
    fn release_baseline_check_skips_while_eval_is_skip() {
        let baseline = eval_summary(90, 0, 10, Some(1_000), Some(1_000));
        let candidate = eval_summary(80, 2, 10, Some(7_000), Some(7_000))
            .with_comparison_to_baseline(&baseline);

        let check = release_baseline_check(
            candidate.comparison_to_baseline.as_ref(),
            EvalCheckStatus::Skip,
        );

        assert_eq!(check.status, "SKIP");
        assert!(check.violations.is_empty());
        assert_eq!(check.notes.len(), 1);
    }

    #[test]
    fn explicit_signals_surfaces_all_kinds() {
        let local_identity = NodeIdentity::generate();
        let store = TraceStore::in_memory().unwrap();

        // Insert avoid signal
        let avoid = create_signal_trace(
            SignalPostKind::Avoid,
            "edit file: src/main.rs",
            "skip the generated lockfile",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("local-a".into()),
                owner_account: None,
                device_identity: Some(local_identity.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        store.insert(&avoid).unwrap();

        // Insert watch signal
        let watch = create_auto_signal_trace(
            SignalPostKind::Watch,
            "bash: cargo test",
            "Bash errors → Edit (2 sessions)",
            SignalTraceConfig {
                model_id: "ignored".into(),
                session_id: Some("local-b".into()),
                owner_account: None,
                device_identity: Some(local_identity.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: 48,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        store.insert(&watch).unwrap();

        // Insert recommend signal
        let recommend = create_auto_signal_trace(
            SignalPostKind::Recommend,
            "bash: Run full test suite",
            "convergent: 4 sessions did this",
            SignalTraceConfig {
                model_id: "ignored".into(),
                session_id: Some("local-c".into()),
                owner_account: None,
                device_identity: Some(local_identity.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: None,
                ttl_hours: 168,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        store.insert(&recommend).unwrap();

        // Query avoid
        let ctx = "edit file: src/main.rs";
        let avoid_signals = explicit_signals(
            &store,
            ctx,
            &simhash(ctx),
            None,
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
        );
        assert!(!avoid_signals.is_empty());
        assert_eq!(avoid_signals[0].kind, SignalKind::Danger);
        assert!(avoid_signals[0].body.contains("avoid"));

        // Query watch
        let ctx = "bash: cargo test";
        let watch_signals = explicit_signals(
            &store,
            ctx,
            &simhash(ctx),
            None,
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
        );
        assert!(!watch_signals.is_empty());
        assert!(watch_signals.iter().any(|s| s.body.contains("watch")));

        // Query recommend
        let ctx = "bash: Run full test suite";
        let rec_signals = explicit_signals(
            &store,
            ctx,
            &simhash(ctx),
            None,
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
        );
        assert!(!rec_signals.is_empty());
        assert!(rec_signals.iter().any(|s| s.body.contains("recommended")));
    }

    #[test]
    fn explicit_signals_respects_space() {
        let local_identity = NodeIdentity::generate();
        let store = TraceStore::in_memory().unwrap();

        let trace = create_signal_trace(
            SignalPostKind::Avoid,
            "edit file: src/main.rs",
            "skip the generated lockfile",
            SignalTraceConfig {
                model_id: "codex".into(),
                session_id: Some("session-local".into()),
                owner_account: None,
                device_identity: Some(local_identity.device_identity()),
                agent_id: None,
                sigil_id: None,
                space: Some("other-space".into()),
                ttl_hours: DEFAULT_SIGNAL_TTL_HOURS,
            },
            local_identity.public_key_bytes(),
            |msg| local_identity.sign(msg),
        );
        store.insert(&trace).unwrap();

        // Wrong space → empty
        let ctx = "edit file: src/main.rs";
        let wrong_space = explicit_signals(
            &store,
            ctx,
            &simhash(ctx),
            Some("psyche"),
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
        );
        assert!(wrong_space.is_empty());

        // Right space → found
        let right_space = explicit_signals(
            &store,
            ctx,
            &simhash(ctx),
            Some("other-space"),
            &local_identity.device_identity(),
            local_identity.public_key_bytes(),
        );
        assert!(!right_space.is_empty());
        assert!(right_space[0].body.contains("skip the generated lockfile"));
    }

    #[test]
    fn history_prior_signals_surface_stable_path_after_three_compliant_sessions() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for idx in 0..3 {
            let trace = Trace::new_with_agent_compliance(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("session-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                Some(MethodCompliance::Compliant),
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).unwrap();
        }

        let prior = host_history_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            None,
            &ActivePolicySet::default().relevant_rules,
        )
        .into_iter()
        .find_map(history_signal_from_projection)
        .unwrap();
        assert_eq!(prior.kind, SignalKind::History);
        assert!(prior.body.contains("stable path"));
        assert!(prior.body.contains("3 compliant session(s) crossed this context"));
    }

    #[test]
    fn history_prior_signals_stay_quiet_below_threshold() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for idx in 0..2 {
            let trace = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("session-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).unwrap();
        }

        let signals: Vec<_> = host_history_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            None,
            &ActivePolicySet::default().relevant_rules,
        )
        .into_iter()
        .filter_map(history_signal_from_projection)
        .collect();
        assert!(signals.iter().all(|signal| !signal.body.contains("stable path")));
    }

    #[test]
    fn history_prior_signals_wait_for_clear_margin_over_recent_failures() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for idx in 0..3 {
            let trace = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).unwrap();
        }
        for idx in 0..2 {
            let failed = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Failed,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("failed-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&failed).unwrap();
        }

        let signals: Vec<_> = host_history_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            None,
            &ActivePolicySet::default().relevant_rules,
        )
        .into_iter()
        .filter_map(history_signal_from_projection)
        .collect();
        assert!(signals.iter().all(|signal| !signal.body.contains("stable path")));
    }

    #[test]
    fn history_prior_signals_surface_unsettled_path_when_outcomes_conflict() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for idx in 0..2 {
            let success = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();

            let failed = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Failed,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("failed-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&failed).unwrap();
        }

        let prior = host_history_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            None,
            &ActivePolicySet::default().relevant_rules,
        )
        .into_iter()
        .find_map(history_signal_from_projection)
        .unwrap();
        assert_eq!(prior.kind, SignalKind::History);
        assert!(prior.body.contains("unsettled path"));
    }

    #[test]
    fn history_prior_signals_stay_quiet_when_one_side_clearly_dominates() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "bash: cargo test --workspace";
        for idx in 0..5 {
            let success = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }
        let failed = Trace::new_with_agent(
            "tool:Bash".into(),
            Outcome::Failed,
            0,
            1,
            simhash(ctx),
            Some(ctx.into()),
            Some("failed-0".into()),
            None,
            Some(identity.device_identity()),
            None,
            None,
            "codex".into(),
            identity.public_key_bytes(),
            |msg| identity.sign(msg),
        );
        store.insert(&failed).unwrap();

        assert!(host_history_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            None,
            &ActivePolicySet::default().relevant_rules,
        )
        .into_iter()
        .find_map(history_signal_from_projection)
        .is_none());
    }

    #[test]
    fn ambient_priors_surface_conflict_and_success_without_new_ontology() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let mixed_ctx = "deploy thronglets service after reviewing recent failures";
        let stable_ctx = "rotate provider endpoint after stable repair path";
        for idx in 0..3 {
            let success = Trace::new_with_agent_compliance(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(mixed_ctx),
                Some(mixed_ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                Some(MethodCompliance::Compliant),
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }
        for idx in 0..2 {
            let failed = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Failed,
                0,
                1,
                simhash(mixed_ctx),
                Some(mixed_ctx.into()),
                Some(format!("failed-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&failed).unwrap();
        }
        for idx in 0..5 {
            let success = Trace::new_with_agent_compliance(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(stable_ctx),
                Some(stable_ctx.into()),
                Some(format!("stable-success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                Some(MethodCompliance::Compliant),
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }

        let mixed_priors = ambient_priors_for_context(&store, &simhash(mixed_ctx), None, None, 3);
        assert!(!mixed_priors.is_empty());
        assert!(
            mixed_priors
                .iter()
                .any(|prior| prior.summary.contains("mixed residue"))
        );
        assert!(
            mixed_priors
                .iter()
                .any(|prior| prior.kind == "mixed-residue")
        );

        let stable_priors = ambient_priors_for_context(&store, &simhash(stable_ctx), None, None, 3);
        assert!(!stable_priors.is_empty());
        assert!(
            stable_priors
                .iter()
                .any(|prior| prior.summary.contains("crossed this context"))
        );
        assert!(
            stable_priors
                .iter()
                .any(|prior| prior.kind == "success-prior")
        );
        assert!(
            mixed_priors
                .iter()
                .all(|prior| prior.provider == "thronglets")
        );
        assert!(
            stable_priors
                .iter()
                .all(|prior| prior.provider == "thronglets")
        );
    }

    #[test]
    fn ambient_priors_limit_results_and_sort_by_confidence() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "investigate repeated frontend regression path";
        for idx in 0..4 {
            let success = Trace::new_with_agent(
                "tool:Edit".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }
        for idx in 0..3 {
            let failed = Trace::new_with_agent(
                "tool:Edit".into(),
                Outcome::Failed,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("failed-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&failed).unwrap();
        }

        let priors = ambient_priors_for_context(&store, &simhash(ctx), None, None, 2);
        assert_eq!(priors.len(), 2);
        assert!(priors[0].confidence >= priors[1].confidence);
    }

    #[test]
    fn ambient_priors_bias_toward_current_goal_without_new_ontology() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "repair deployment after repeated endpoint failures but with one previously stable path";
        for idx in 0..5 {
            let success = Trace::new_with_agent_compliance(
                "tool:Bash".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                Some(MethodCompliance::Compliant),
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }
        for idx in 0..3 {
            let failed = Trace::new_with_agent(
                "tool:Bash".into(),
                Outcome::Failed,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("failed-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&failed).unwrap();
        }

        let build_priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(thronglets::ambient::AmbientTurnGoal::Build),
            3,
        );
        let explore_priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(thronglets::ambient::AmbientTurnGoal::Explore),
            3,
        );
        let repair_priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(thronglets::ambient::AmbientTurnGoal::Repair),
            3,
        );

        let build_success = build_priors
            .iter()
            .find(|prior| prior.kind == "success-prior")
            .unwrap();
        let repair_success = repair_priors
            .iter()
            .find(|prior| prior.kind == "success-prior")
            .unwrap();
        let build_failure = build_priors
            .iter()
            .find(|prior| prior.kind == "failure-residue")
            .unwrap();
        let repair_failure = repair_priors
            .iter()
            .find(|prior| prior.kind == "failure-residue")
            .unwrap();

        assert!(
            explore_priors
                .iter()
                .all(|prior| prior.kind != "success-prior")
        );
        assert!(build_success.confidence > repair_success.confidence);
        assert!(build_success.confidence > 0.8);
        assert!(repair_failure.confidence > build_failure.confidence);
        assert!(repair_priors.iter().all(|prior| prior.goal.is_some()));
    }

    #[test]
    fn ambient_priors_keep_explore_success_soft_and_non_exclusive() {
        let store = TraceStore::in_memory().unwrap();
        let identity = NodeIdentity::generate();
        let ctx = "investigate a non-consensus optimization route with one stable prior path";
        for idx in 0..5 {
            let success = Trace::new_with_agent_compliance(
                "tool:Edit".into(),
                Outcome::Succeeded,
                0,
                1,
                simhash(ctx),
                Some(ctx.into()),
                Some(format!("success-{idx}")),
                None,
                Some(identity.device_identity()),
                None,
                None,
                Some(MethodCompliance::Compliant),
                "codex".into(),
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&success).unwrap();
        }

        let explore_priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(thronglets::ambient::AmbientTurnGoal::Explore),
            3,
        );
        let build_priors = ambient_priors_for_context(
            &store,
            &simhash(ctx),
            None,
            Some(thronglets::ambient::AmbientTurnGoal::Build),
            3,
        );

        let explore_success = explore_priors
            .iter()
            .find(|prior| prior.kind == "success-prior")
            .unwrap();
        let build_success = build_priors
            .iter()
            .find(|prior| prior.kind == "success-prior")
            .unwrap();

        assert!(explore_success.confidence <= 0.68);
        assert!(build_success.confidence > explore_success.confidence);
        assert!(explore_success.summary.contains("non-exclusive baseline"));
    }

    #[test]
    fn reinforced_success_threshold_lowers_when_guidance_proved_useful() {
        let events = vec![workspace::RecommendationFeedbackEvent {
            recommendation_kind: "do_next".into(),
            source_kind: "repair".into(),
            space: Some("psyche".into()),
            positive: true,
            timestamp_ms: 1,
        }];

        assert_eq!(reinforced_success_threshold(&events, 0, 0), 2);
    }

    #[test]
    fn reinforced_success_threshold_stays_default_for_non_reinforcing_feedback() {
        let events = vec![
            workspace::RecommendationFeedbackEvent {
                recommendation_kind: "context".into(),
                source_kind: "history".into(),
                space: None,
                positive: true,
                timestamp_ms: 1,
            },
            workspace::RecommendationFeedbackEvent {
                recommendation_kind: "context".into(),
                source_kind: "history".into(),
                space: None,
                positive: false,
                timestamp_ms: 2,
            },
        ];

        assert_eq!(reinforced_success_threshold(&events, 0, 0), 3);
    }

    #[test]
    fn reinforced_success_threshold_rises_when_feedback_or_failures_contradict() {
        let events = vec![
            workspace::RecommendationFeedbackEvent {
                recommendation_kind: "do_next".into(),
                source_kind: "repair".into(),
                space: None,
                positive: true,
                timestamp_ms: 1,
            },
            workspace::RecommendationFeedbackEvent {
                recommendation_kind: "do_next".into(),
                source_kind: "repair".into(),
                space: None,
                positive: false,
                timestamp_ms: 2,
            },
        ];

        assert_eq!(reinforced_success_threshold(&events, 1, 0), 4);
    }

    #[test]
    fn hard_current_turn_policy_surfaces_as_danger_signal() {
        let active_policy = ActivePolicySet {
            all_rules: vec![thronglets::active_policy::ActivePolicyRule {
                id: "task:reuse-components".into(),
                strength: PolicyStrength::Hard,
                scope: thronglets::active_policy::PolicyScope::Task,
                summary: "reuse existing shared components".into(),
            }],
            relevant_rules: vec![thronglets::active_policy::ActivePolicyRule {
                id: "task:reuse-components".into(),
                strength: PolicyStrength::Hard,
                scope: thronglets::active_policy::PolicyScope::Task,
                summary: "reuse existing shared components".into(),
            }],
        };

        let signal = active_policy_signal(&active_policy).unwrap();
        assert_eq!(signal.kind, SignalKind::Danger);
        assert!(signal.body.contains("reuse existing shared components"));
    }

    #[test]
    fn historical_noncompliance_stays_soft_without_hard_policy() {
        assert!(can_promote_auto_recommend(
            Some(MethodCompliance::Compliant),
            false,
            2,
        ));
        assert!(!can_promote_auto_recommend(
            Some(MethodCompliance::Noncompliant),
            false,
            0,
        ));
        assert!(!can_promote_auto_recommend(
            Some(MethodCompliance::Compliant),
            true,
            1,
        ));
    }
}
