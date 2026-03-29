mod setup_support;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use setup_support::{
    AdapterApplyResult, AdapterDetection, AdapterDoctor, AdapterKind, AdapterPlan,
    clear_restart_pending, detect_adapter, doctor_adapter, install_claude, install_codex,
    install_openclaw, install_plan, set_restart_pending,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use thronglets::anchor::AnchorClient;
use thronglets::context::simhash;
use thronglets::contracts::{
    GIT_HISTORY_MAX_ENTRIES, PREHOOK_HEADER, PREHOOK_MAX_COLLECTIVE_QUERIES, PREHOOK_MAX_HINTS,
};
use thronglets::eval::{
    EvalBaselineComparison, EvalCheckStatus, EvalCheckThresholds, EvalConfig, EvalFocus,
    LocalFeedbackSummary, SignalEvalSummary, evaluate_signal_quality,
};
use thronglets::identity::{
    ConnectionFile, DEFAULT_CONNECTION_FILE_TTL_HOURS, IdentityBinding, NodeIdentity,
    identity_binding_path,
};
use thronglets::mcp::McpContext;
use thronglets::network::{NetworkCommand, NetworkConfig, NetworkEvent};
use thronglets::posts::{
    DEFAULT_SIGNAL_TTL_HOURS, SignalPostKind, SignalScopeFilter, SignalTraceConfig,
    create_signal_trace, filter_signal_feed_results, is_signal_capability,
    summarize_recent_signal_feed, summarize_signal_traces,
};
use thronglets::profile::{ProfileCheckThresholds, summarize_prehook_profiles};
use thronglets::signals::{
    Recommendation, Signal, SignalKind, StepCandidate, select as select_signals,
};
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};
use thronglets::workspace::{self, WorkspaceState};
use tracing::info;

const BOOTSTRAP_SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";
const IDENTITY_SCHEMA_VERSION: &str = "thronglets.identity.v1";
const RELEASE_MAX_LOCAL_RETENTION_DROP_TENTHS_PP: i32 = 50;
const RELEASE_MAX_FAILED_COMMAND_RATE_RISE_TENTHS_PP: i32 = 50;
const RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS: i64 = 5_000;

#[derive(Serialize)]
struct MachineEnvelope<T> {
    schema_version: &'static str,
    command: &'static str,
    data: T,
}

#[derive(Serialize)]
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

#[derive(Clone, Serialize)]
struct IdentitySummary {
    status: &'static str,
    owner_account: Option<String>,
    device_identity: String,
    binding_source: String,
    joined_from_device: Option<String>,
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
struct ConnectionExportData {
    summary: IdentitySummary,
    output: String,
    primary_device_pubkey: String,
    signed_by_device: String,
    ttl_hours: u32,
    expires_at: u64,
}

#[derive(Serialize)]
struct ConnectionJoinData {
    summary: IdentitySummary,
    file: String,
    signature_verified: bool,
    source_expires_at: u64,
}

#[derive(Serialize)]
struct ConnectionInspectData {
    summary: IdentitySummary,
    file: String,
    primary_device_pubkey: String,
    exported_at: u64,
    expires_at: u64,
    ttl_hours: u32,
    signature_verified: bool,
}

#[derive(Serialize)]
struct StatusData {
    summary: IdentitySummary,
    node_id: String,
    oasyce_address: String,
    data_dir: String,
    trace_count: u64,
    capabilities: usize,
    database_size_bytes: u64,
}

#[derive(Parser)]
#[command(
    name = "thronglets",
    version,
    about = "P2P shared memory substrate for AI agents"
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
            Self::Openclaw => matches!(adapter, AdapterKind::OpenClaw),
            Self::Generic => matches!(adapter, AdapterKind::Generic),
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
}

impl From<SignalKindArg> for SignalPostKind {
    fn from(value: SignalKindArg) -> Self {
        match value {
            SignalKindArg::Recommend => SignalPostKind::Recommend,
            SignalKindArg::Avoid => SignalPostKind::Avoid,
            SignalKindArg::Watch => SignalPostKind::Watch,
            SignalKindArg::Info => SignalPostKind::Info,
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
    /// Start the Thronglets node
    Run {
        /// Port to listen on (0 = random)
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Bootstrap peer multiaddrs
        #[arg(long)]
        bootstrap: Vec<String>,
    },

    /// Show node identity
    Id {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Bind this device to an owner account.
    OwnerBind {
        /// Root owner account / wallet address.
        #[arg(long)]
        owner_account: String,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

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
    },

    /// Join this device to an existing owner account using a connection file.
    ConnectionJoin {
        /// Connection file exported from the primary device.
        #[arg(long)]
        file: PathBuf,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Inspect and verify a connection file before joining.
    ConnectionInspect {
        /// Connection file exported from the primary device.
        #[arg(long)]
        file: PathBuf,

        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

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

    /// Query aggregate stats for a capability
    Query {
        /// Capability URI to query
        capability: String,
    },

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

        /// Model identifier.
        #[arg(long, default_value = "cli")]
        model: String,

        /// Optional session identifier.
        #[arg(long)]
        session_id: Option<String>,

        /// How long the signal should remain fresh before it decays away.
        #[arg(long, default_value_t = DEFAULT_SIGNAL_TTL_HOURS)]
        ttl_hours: u32,
    },

    /// Query explicit short signals left by other agents.
    SignalQuery {
        /// Task context to search against.
        #[arg(long)]
        context: String,

        /// Restrict to one signal kind.
        #[arg(long, value_enum)]
        kind: Option<SignalKindArg>,

        /// Maximum results to return.
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },

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

        /// Maximum results to return.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    /// Start MCP server for AI agent integration (JSON-RPC over stdio)
    Mcp {
        /// Also start P2P network on this port (0 = random, omit = local only)
        #[arg(long)]
        port: Option<u16>,

        /// Bootstrap peer multiaddrs (only used if --port is set)
        #[arg(long)]
        bootstrap: Vec<String>,
    },

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

    /// Auto-record traces from agent tool hooks.
    /// Reads a Claude-compatible hook JSON contract from stdin and records a trace.
    /// Designed to be fast (<50ms).
    Hook,

    /// Query substrate before tool calls and emit sparse decision signals.
    /// Reads a Claude-compatible hook JSON contract from stdin.
    /// Silent when no relevant data. Designed to be fast (<50ms).
    Prehook,

    /// One-command setup: install known local agent adapters and hook integrations.
    Setup,

    /// Detect locally available agent runtimes and bootstrap surfaces.
    Detect {
        /// Restrict detection to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

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

    /// Apply the install plan for one adapter or all known adapters.
    ApplyPlan {
        /// Restrict application to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Verify whether a configured adapter is healthy.
    Doctor {
        /// Restrict verification to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Detect, plan, apply, and verify adapter setup in one command.
    Bootstrap {
        /// Restrict bootstrapping to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Clear persisted restart-pending state after the target runtime has been restarted.
    ClearRestart {
        /// Restrict clearing to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Mark a runtime as ready after it has successfully reloaded the Thronglets integration.
    RuntimeReady {
        /// Restrict readiness reporting to one adapter family.
        #[arg(long, value_enum, default_value_t = AdapterArg::All)]
        agent: AdapterArg,

        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Start HTTP API server for non-MCP agents (Python, LangChain, etc.)
    Serve {
        /// HTTP port to listen on
        #[arg(long, default_value_t = 7777)]
        port: u16,
    },

    /// Show connected peers
    Peers,

    /// Show node status and statistics
    Status {
        /// Emit machine-readable JSON instead of text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Summarize stderr lines emitted by THRONGLETS_PROFILE_PREHOOK=1.
    /// Reads log lines from stdin and prints aggregate stats.
    ProfileSummary,

    /// Check whether profiled prehook logs still fit release-oriented sparse-signal thresholds.
    /// Reads log lines from stdin and exits non-zero on regression.
    ProfileCheck,

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
        .filter(|adapter| matches!(adapter, AdapterKind::Codex | AdapterKind::OpenClaw))
        .collect()
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

fn identity_summary(status: &'static str, binding: &IdentityBinding) -> IdentitySummary {
    IdentitySummary {
        status,
        owner_account: binding.owner_account.clone(),
        device_identity: binding.device_identity.clone(),
        binding_source: binding.binding_source_or_local().to_string(),
        joined_from_device: binding.joined_from_device.clone(),
    }
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
        let model_suffix = if result.model_count > 1 {
            format!(" models={}", result.model_count)
        } else {
            String::new()
        };
        println!(
            "    similarity={:.2} posts={} sources={}{} (local {} / collective {}) scope={} expires_in≈{}h",
            result.context_similarity,
            result.total_posts,
            result.source_count,
            model_suffix,
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
        println!(
            "    posts={} sources={}{}{} (local {} / collective {}) scope={} expires_in≈{}h",
            result.total_posts,
            result.source_count,
            model_suffix,
            focus_suffix,
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
        println!("Restart pending: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart: {command}");
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
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
        println!("Restart required: yes");
    }
    if data.summary.restart_pending {
        println!("Restart pending: yes");
    }
    for command in &data.summary.restart_commands {
        println!("Restart: {command}");
    }
    for step in &data.summary.next_steps {
        println!("Next: {step}");
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
        println!("Restart pending: yes");
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
        next_steps.push("Run `thronglets doctor --agent <adapter>` to confirm current status.".into());
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
                let result = install_claude(home_dir, bin_path)?;
                let mut changed = Vec::new();
                if result.added_post_hook {
                    changed.push("installed PostToolUse hook".into());
                }
                if result.added_pre_hook {
                    changed.push("installed PreToolUse hook".into());
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
        .map(|adapter| install_plan(home_dir, data_dir, bin_path, adapter))
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
        .init();

    let cli = Cli::parse();
    let dir = data_dir(&cli.data_dir);
    let identity = load_identity(&dir);
    let identity_binding = load_identity_binding(&dir, &identity);

    match cli.command {
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
        } => {
            let connection = ConnectionFile::from_binding(&identity_binding, &identity, ttl_hours)
                .expect("failed to create connection file");
            connection
                .save(&output)
                .expect("failed to write connection file");
            let data = ConnectionExportData {
                summary: identity_summary("exported", &identity_binding),
                output: output.display().to_string(),
                primary_device_pubkey: connection.primary_device_pubkey.clone(),
                signed_by_device: connection.primary_device_identity.clone(),
                ttl_hours: connection.ttl_hours(),
                expires_at: connection.expires_at,
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "connection-export", &data);
            } else {
                println!("Connection file exported:");
                println!("  Output:             {}", data.output);
                println!(
                    "  Owner account:      {}",
                    identity_binding.owner_account_or_unbound()
                );
                println!("  Primary device:     {}", identity_binding.device_identity);
                println!("  Signed by device:   {}", data.signed_by_device);
                println!("  Expires in:         {}h", data.ttl_hours);
            }
        }

        Commands::ConnectionInspect { file, json } => {
            let connection = ConnectionFile::load(&file).expect("failed to read connection file");
            let summary = IdentitySummary {
                status: "valid",
                owner_account: connection.owner_account.clone(),
                device_identity: connection.primary_device_identity.clone(),
                binding_source: "connection_file".into(),
                joined_from_device: None,
            };
            let data = ConnectionInspectData {
                summary,
                file: file.display().to_string(),
                primary_device_pubkey: connection.primary_device_pubkey.clone(),
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
                println!("Connection file valid:");
                println!("  File:               {}", data.file);
                println!(
                    "  Owner account:      {}",
                    data.summary.owner_account.as_deref().unwrap_or("unbound")
                );
                println!("  Primary device:     {}", data.summary.device_identity);
                println!("  Signature verified: yes");
                println!("  Expires in:         {}h", data.ttl_hours);
            }
        }

        Commands::ConnectionJoin { file, json } => {
            let connection = ConnectionFile::load(&file).expect("failed to read connection file");
            let binding = identity_binding
                .clone()
                .joined_via_connection(
                    connection.owner_account.clone(),
                    connection.primary_device_identity.clone(),
                )
                .expect("failed to update identity binding");
            binding
                .save(&identity_binding_path(&dir))
                .expect("failed to save identity binding");
            let data = ConnectionJoinData {
                summary: identity_summary("joined", &binding),
                file: file.display().to_string(),
                signature_verified: true,
                source_expires_at: connection.expires_at,
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "connection-join", &data);
            } else {
                println!("Connection file joined:");
                println!("  File:               {}", data.file);
                println!(
                    "  Owner account:      {}",
                    binding.owner_account_or_unbound()
                );
                println!("  Device identity:    {}", binding.device_identity);
                println!(
                    "  Joined from device: {}",
                    connection.primary_device_identity
                );
                println!("  Signature verified: yes");
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
            model,
            session_id,
            ttl_hours,
        } => {
            let store = open_store(&dir);
            let trace = create_signal_trace(
                kind.into(),
                &context,
                &message,
                SignalTraceConfig {
                    model_id: model,
                    session_id,
                    owner_account: identity_binding.owner_account.clone(),
                    device_identity: Some(identity_binding.device_identity.clone()),
                    ttl_hours,
                },
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            store.insert(&trace).expect("failed to insert signal trace");
            println!("Signal posted:");
            println!("  Kind:      {}", SignalPostKind::from(kind).as_str());
            println!("  Message:   {}", message);
            println!("  Fresh for: {}h", ttl_hours);
            println!("  Trace ID:  {}", hex_encode(&trace.id[..8]));
        }

        Commands::SignalQuery {
            context,
            kind,
            limit,
        } => {
            let store = open_store(&dir);
            let query_hash = simhash(&context);
            let traces = store
                .query_signal_traces(&query_hash, kind.map(Into::into), 48, limit)
                .expect("failed to query signal traces");
            let results = summarize_signal_traces(
                &traces,
                &context,
                &identity_binding.device_identity,
                identity.public_key_bytes(),
                limit,
            );
            render_signal_query_results(&results);
        }

        Commands::SignalFeed {
            hours,
            kind,
            scope,
            limit,
        } => {
            let store = open_store(&dir);
            let traces = store
                .query_recent_signal_traces(hours, kind.map(Into::into), limit)
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
            render_signal_feed_results(&results);
        }

        Commands::Run { port, bootstrap } => {
            let store = open_store(&dir);

            let libp2p_keypair =
                libp2p::identity::Keypair::ed25519_from_bytes(&mut identity.secret_key_bytes())
                    .expect("failed to create libp2p keypair");

            let bootstrap_addrs: Vec<libp2p::Multiaddr> =
                bootstrap.iter().filter_map(|s| s.parse().ok()).collect();

            let config = NetworkConfig {
                listen_port: port,
                bootstrap_peers: bootstrap_addrs,
            };

            let (cmd_tx, mut event_rx) = thronglets::network::start(libp2p_keypair, config)
                .await
                .expect("failed to start network");

            info!(
                "Node {} running. Press Ctrl+C to stop.",
                identity.short_id()
            );

            let mut evaporation_interval =
                tokio::time::interval(std::time::Duration::from_secs(3600));
            evaporation_interval.tick().await;
            let mut dht_publish_interval =
                tokio::time::interval(std::time::Duration::from_secs(300));
            dht_publish_interval.tick().await;
            // Scan for locally-recorded traces that haven't been published to the network
            let mut publish_scan_interval =
                tokio::time::interval(std::time::Duration::from_secs(30));
            publish_scan_interval.tick().await;

            loop {
                tokio::select! {
                    Some(event) = event_rx.recv() => {
                        match event {
                            NetworkEvent::PeerConnected(peer) => {
                                info!(%peer, "Peer connected");
                            }
                            NetworkEvent::PeerDisconnected(peer) => {
                                info!(%peer, "Peer disconnected");
                            }
                            NetworkEvent::TraceReceived(trace) => {
                                let tid = trace.id;
                                match store.insert(&trace) {
                                    Ok(true) => {
                                        // Mark as published — came from network, don't re-broadcast
                                        let _ = store.mark_published(&[tid]);
                                        info!(
                                            capability = %trace.capability,
                                            outcome = ?trace.outcome,
                                            "Stored new trace from network"
                                        );
                                    }
                                    Ok(false) => {}
                                    Err(e) => {
                                        tracing::warn!(%e, "Failed to store received trace");
                                    }
                                }
                            }
                        }
                    }
                    _ = evaporation_interval.tick() => {
                        match store.evaporate(None) {
                            Ok(n) if n > 0 => info!(deleted = n, "Evaporated expired traces"),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(%e, "Evaporation failed"),
                        }
                    }
                    _ = dht_publish_interval.tick() => {
                        if let Ok(caps) = store.distinct_capabilities(100) {
                            for cap in caps {
                                if is_signal_capability(&cap) {
                                    continue;
                                }
                                if let Ok(Some(stats)) = store.aggregate(&cap) {
                                    let _ = cmd_tx.send(NetworkCommand::PublishSummary {
                                        capability: cap,
                                        stats,
                                    }).await;
                                }
                            }
                        }
                    }
                    _ = publish_scan_interval.tick() => {
                        // Bridge: publish locally-recorded traces (from hooks) to the network
                        if let Ok(traces) = store.unpublished_traces(50)
                            && !traces.is_empty()
                        {
                            info!(count = traces.len(), "Publishing local traces to network");
                            let mut ids: Vec<[u8; 32]> = Vec::new();
                            for trace in traces {
                                ids.push(trace.id);
                                let _ = cmd_tx.send(NetworkCommand::PublishTrace(Box::new(trace))).await;
                            }
                            let _ = store.mark_published(&ids);
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        info!("Shutting down...");
                        break;
                    }
                }
            }

            drop(cmd_tx);
        }

        Commands::Mcp { port, bootstrap } => {
            let store = open_store(&dir);
            let store = Arc::new(store);

            let network_tx = if let Some(p) = port {
                let libp2p_keypair =
                    libp2p::identity::Keypair::ed25519_from_bytes(&mut identity.secret_key_bytes())
                        .expect("failed to create libp2p keypair");

                let bootstrap_addrs: Vec<libp2p::Multiaddr> =
                    bootstrap.iter().filter_map(|s| s.parse().ok()).collect();

                let config = NetworkConfig {
                    listen_port: p,
                    bootstrap_peers: bootstrap_addrs,
                };

                let (cmd_tx, mut event_rx) = thronglets::network::start(libp2p_keypair, config)
                    .await
                    .expect("failed to start network");

                let store_bg = Arc::clone(&store);
                tokio::spawn(async move {
                    let mut evaporation_interval =
                        tokio::time::interval(std::time::Duration::from_secs(3600));
                    evaporation_interval.tick().await;

                    loop {
                        tokio::select! {
                            event = event_rx.recv() => {
                                match event {
                                    Some(NetworkEvent::PeerConnected(peer)) => {
                                        info!(%peer, "Peer connected");
                                    }
                                    Some(NetworkEvent::PeerDisconnected(peer)) => {
                                        info!(%peer, "Peer disconnected");
                                    }
                                    Some(NetworkEvent::TraceReceived(trace)) => {
                                        match store_bg.insert(&trace) {
                                            Ok(true) => {
                                                info!(capability = %trace.capability, "Stored trace from network");
                                            }
                                            Ok(false) => {}
                                            Err(e) => {
                                                tracing::warn!(%e, "Failed to store received trace");
                                            }
                                        }
                                    }
                                    None => break,
                                }
                            }
                            _ = evaporation_interval.tick() => {
                                match store_bg.evaporate(None) {
                                    Ok(n) if n > 0 => info!(deleted = n, "Evaporated expired traces"),
                                    Ok(_) => {}
                                    Err(e) => tracing::warn!(%e, "Evaporation failed"),
                                }
                            }
                        }
                    }
                });

                Some(cmd_tx)
            } else {
                None
            };

            let ctx = Arc::new(McpContext {
                identity: Arc::new(identity),
                binding: Arc::new(identity_binding),
                store,
                network_tx,
            });

            thronglets::mcp::serve_stdio(ctx).await;
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
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0); // silent fail — never break the calling agent
            }

            let payload: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(_) => std::process::exit(0),
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
            let context_text = build_hook_context(tool_name, &payload["tool_input"]);

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
            let mut ws = WorkspaceState::load(&dir);
            let enriched_context = if let Some(strategy) = ws.infer_strategy() {
                format!("[{strategy}] {context_text}")
            } else {
                context_text.clone()
            };

            let store = open_store(&dir);
            let ctx_hash = simhash(&enriched_context);
            let is_error = matches!(outcome, Outcome::Failed);
            let trace = Trace::new_with_identity(
                capability.clone(),
                outcome,
                0, // latency not available from hook
                input_size,
                ctx_hash,
                Some(enriched_context),
                session_id.clone(),
                identity_binding.owner_account.clone(),
                Some(identity_binding.device_identity.clone()),
                model,
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            let _ = store.insert(&trace); // silent — never break the calling agent
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

            // Track pending feedback for Edit/Write
            if matches!(tool_name, "Edit" | "Write")
                && let Some(fp) = file_path
            {
                ws.add_pending_feedback(fp, tool_name);
            }

            // Resolve pending feedback (check git status for previous edits)
            ws.resolve_feedback();

            // Track errors
            if is_error && let Some(err) = workspace::extract_error(&payload["tool_response"]) {
                ws.record_error(tool_name, context_text, err);
            }

            // Track session
            if let Some(sid) = &session_id {
                ws.track_session(sid, &capability, is_error);
            }

            ws.save(&dir);
        }

        Commands::Prehook => {
            let mut profiler = PrehookProfiler::from_env();

            // Read a generic pre-tool hook payload from stdin.
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0);
            }

            let payload: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(_) => std::process::exit(0),
            };

            let tool_name = payload["tool_name"].as_str().unwrap_or("");

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
            let ws = WorkspaceState::load(&dir);
            let current_file = workspace::extract_file_path(tool_name, &payload["tool_input"]);
            let supports_file_guidance =
                matches!(tool_name, "Edit" | "Write") && current_file.is_some();
            let has_repeated_local_file_actions = supports_file_guidance
                && ws.has_repeated_recent_file_actions(current_file.as_deref());
            profiler.stage("workspace");

            let mut collective_store: Option<TraceStore> = None;
            let mut collective_queries_remaining = PREHOOK_MAX_COLLECTIVE_QUERIES;

            let mut has_recent_tool_error = false;

            // ── Danger pheromone: low edit retention ──
            // If recent edits are mostly reverted, this is a strong warning.
            // Only signal when retention < 50% (anomaly).
            if let Some(retention_warning) = ws.retention_warning(current_file.as_deref()) {
                signals.push(Signal::danger(
                    retention_warning.body,
                    retention_warning.score,
                ));
            }

            // ── Alarm pheromone: recent errors with this tool ──
            // Only emitted when errors happened in the last hour.
            if let Some(recent_error) = ws.recent_errors.iter().find(|e| {
                e.tool == tool_name
                    && (chrono::Utc::now().timestamp_millis() - e.timestamp_ms) < 3_600_000
            }) {
                let signal = {
                    let e = recent_error;
                    let snippet = if e.error_snippet.len() > 80 {
                        format!("{}...", &e.error_snippet[..80])
                    } else {
                        e.error_snippet.clone()
                    };
                    Signal::danger(format!("  ⚠ recent error: {snippet}"), 360)
                };
                has_recent_tool_error = true;
                signals.push(signal);
            }
            profiler.stage("danger");

            if has_recent_tool_error
                && let Some(repair_hint) = ws
                    .repair_trajectory_hint(tool_name)
                    .or_else(|| ws.repair_hints(tool_name))
            {
                let mut repair_hint = repair_hint;
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

            let has_do_next_signal = signals
                .iter()
                .any(|s| matches!(s.kind, SignalKind::Repair | SignalKind::Preparation));
            if has_repeated_local_file_actions
                && !has_do_next_signal
                && let Some(mut preparation_hint) =
                    ws.preparation_hint(tool_name, current_file.as_deref())
            {
                if let (Some(current_file), Some(target)) = (
                    current_file.as_deref(),
                    preparation_hint.candidate.primary_target(),
                ) && claim_collective_query(
                    &preparation_hint.candidate,
                    &mut collective_queries_remaining,
                ) {
                    let edit_target = file_target(current_file);
                    if let Some(store) = cached_collective_store(&mut collective_store, &dir)
                        && let Ok(collective_sources) =
                            store.count_preparation_sources(edit_target, target, 168)
                    {
                        apply_collective_sources(
                            &mut preparation_hint.candidate,
                            &mut preparation_hint.score,
                            collective_sources,
                        );
                    }
                }

                signals.push(Signal::preparation_candidate(
                    preparation_hint.body,
                    preparation_hint.score,
                    preparation_hint.candidate,
                ));
            }
            profiler.stage("preparation");

            // ── Trail pheromone: co-edit patterns ──
            // "Editing A usually means you also need to edit B."
            // Only emitted when patterns exist.
            if has_repeated_local_file_actions
                && let Some(mut adjacency_hint) =
                    ws.adjacency_hint(tool_name, current_file.as_deref())
            {
                if let (Some(current_file), Some(target)) = (
                    current_file.as_deref(),
                    adjacency_hint.candidate.primary_target(),
                ) && claim_collective_query(
                    &adjacency_hint.candidate,
                    &mut collective_queries_remaining,
                ) {
                    let current_target = file_target(current_file);
                    if let Some(store) = cached_collective_store(&mut collective_store, &dir)
                        && let Ok(collective_sources) =
                            store.count_adjacency_sources(current_target, target, 168)
                    {
                        apply_collective_sources(
                            &mut adjacency_hint.candidate,
                            &mut adjacency_hint.score,
                            collective_sources,
                        );
                    }
                }

                signals.push(Signal::adjacency_candidate(
                    adjacency_hint.body,
                    adjacency_hint.score,
                    adjacency_hint.candidate,
                ));
            }
            profiler.stage("adjacency");

            // History is a fallback when we don't already know a likely next move.
            let has_higher_priority_signal = !signals.is_empty();
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
            let recommendations = select_signals(signals, PREHOOK_MAX_HINTS);
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
                profile_file_guidance_gate(supports_file_guidance, has_repeated_local_file_actions),
                PREHOOK_MAX_COLLECTIVE_QUERIES - collective_queries_remaining,
            );
            // Normal state → complete silence. Zero tokens.
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
        }

        Commands::Detect { agent, json } => {
            let home_dir = home_dir();
            let detections: Vec<_> = selected_adapters(agent)
                .into_iter()
                .map(|adapter| detect_adapter(&home_dir, &dir, adapter))
                .collect();
            let summary = summarize_detections(detections);
            if json {
                print_machine_json("detect", &summary);
            } else {
                render_detect_report(&summary);
            }
        }

        Commands::InstallPlan {
            agent,
            runtime,
            json,
        } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let mut plans: Vec<_> = selected_adapters(agent)
                .into_iter()
                .map(|adapter| install_plan(&home_dir, &dir, &bin, adapter))
                .collect();
            filter_generic_runtime_snippets(&mut plans, runtime);
            let summary = summarize_install_plans(plans);
            if json {
                print_machine_json("install-plan", &summary);
            } else {
                render_install_plan_report(&summary);
            }
        }

        Commands::ApplyPlan { agent, json } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let results = apply_selected_adapters(agent, &home_dir, &dir, &bin)
                .expect("failed to apply adapter plan");
            let summary = summarize_apply_results(results);
            if json {
                print_machine_json("apply-plan", &summary);
            } else {
                render_apply_plan_report(&summary);
            }
        }

        Commands::Doctor { agent, json } => {
            let home_dir = home_dir();
            let reports: Vec<_> = selected_adapters(agent)
                .into_iter()
                .map(|adapter| doctor_adapter(&home_dir, &dir, adapter))
                .collect();
            let summary = summarize_doctor_reports(agent, reports);
            if json {
                print_machine_json("doctor", &summary);
            } else {
                render_doctor_report(&summary);
            }
            if !summary.summary.healthy {
                std::process::exit(1);
            }
        }

        Commands::Bootstrap { agent, json } => {
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let home_dir = home_dir();
            let report = bootstrap_selected_adapters(agent, &home_dir, &dir, &bin)
                .expect("failed to bootstrap adapter plan");
            if json {
                print_machine_json("bootstrap", &report);
            } else {
                render_bootstrap_report(&report);
            }
            if !report.summary.healthy {
                std::process::exit(1);
            }
        }

        Commands::ClearRestart { agent, json } => {
            let report =
                clear_selected_restart_state(agent, &dir).expect("failed to clear restart state");
            if json {
                print_machine_json("clear-restart", &report);
            } else {
                render_clear_restart_report(&report);
            }
        }

        Commands::RuntimeReady { agent, json } => {
            let report =
                mark_selected_runtime_ready(agent, &dir).expect("failed to mark runtime ready");
            if json {
                print_machine_json("runtime-ready", &report);
            } else {
                render_runtime_ready_report(&report);
            }
        }

        Commands::Serve { port } => {
            let store = open_store(&dir);
            let ctx = Arc::new(thronglets::http::HttpContext {
                identity: Arc::new(identity),
                binding: Arc::new(identity_binding),
                store: Arc::new(store),
            });
            println!("Thronglets HTTP API on http://0.0.0.0:{port}");
            println!("  POST /v1/traces       — record a trace");
            println!("  POST /v1/signals      — leave an explicit short signal");
            println!("  GET  /v1/signals      — query explicit short signals");
            println!("  GET  /v1/signals/feed — show recent converging explicit signals");
            println!("  GET  /v1/query        — query the substrate");
            println!("  GET  /v1/capabilities — list capabilities");
            println!("  GET  /v1/status       — node status");
            thronglets::http::serve(ctx, port)
                .await
                .expect("HTTP server failed");
        }

        Commands::Peers => {
            println!("The 'peers' command requires a running node.");
            println!("Use 'thronglets run' to start a node, then peers are logged to console.");
        }

        Commands::Status { json } => {
            let store = open_store(&dir);
            let trace_count = store.count().unwrap_or(0);
            let cap_count = store
                .distinct_capabilities(1000)
                .map(|caps| {
                    caps.into_iter()
                        .filter(|capability| !is_signal_capability(capability))
                        .count()
                })
                .unwrap_or(0);
            let db_path = dir.join("traces.db");
            let db_size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

            let size_display = if db_size >= 1_048_576 {
                format!("{:.1} MB", db_size as f64 / 1_048_576.0)
            } else if db_size >= 1024 {
                format!("{:.1} KB", db_size as f64 / 1024.0)
            } else {
                format!("{} B", db_size)
            };
            let data = StatusData {
                summary: identity_summary("healthy", &identity_binding),
                node_id: identity.short_id(),
                oasyce_address: identity.oasyce_address(),
                data_dir: dir.display().to_string(),
                trace_count,
                capabilities: cap_count,
                database_size_bytes: db_size,
            };
            if json {
                print_machine_json_with_schema(IDENTITY_SCHEMA_VERSION, "status", &data);
            } else {
                println!("Thronglets v{}", env!("CARGO_PKG_VERSION"));
                println!();
                println!("  Node ID:          {}", data.node_id);
                println!("  Oasyce address:   {}", data.oasyce_address);
                println!("  Device identity:  {}", data.summary.device_identity);
                println!(
                    "  Owner account:    {}",
                    identity_binding.owner_account_or_unbound()
                );
                println!("  Binding source:   {}", data.summary.binding_source);
                println!(
                    "  Joined from:      {}",
                    identity_binding.joined_from_device_or_none()
                );
                println!("  Data directory:   {}", data.data_dir);
                println!();
                println!("  Trace count:      {}", data.trace_count);
                println!("  Capabilities:     {}", data.capabilities);
                println!("  Database size:    {}", size_display);
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
            let local_feedback = LocalFeedbackSummary::from_workspace(&WorkspaceState::load(&dir));
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
                        LocalFeedbackSummary::from_workspace(&WorkspaceState::load(&dir))
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

/// Build a natural-language context string from a hook payload.
/// This is the "WHY" that future agents can read.
fn build_hook_context(tool_name: &str, tool_input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => {
            let cmd = tool_input["command"].as_str().unwrap_or("");
            let desc = tool_input["description"].as_str().unwrap_or("");
            if !desc.is_empty() {
                format!("bash: {desc}")
            } else {
                // Truncate long commands
                let cmd_short = if cmd.len() > 200 { &cmd[..200] } else { cmd };
                format!("bash: {cmd_short}")
            }
        }
        "Read" => {
            let path = tool_input["file_path"].as_str().unwrap_or("");
            format!("read file: {path}")
        }
        "Write" => {
            let path = tool_input["file_path"].as_str().unwrap_or("");
            format!("write file: {path}")
        }
        "Edit" => {
            let path = tool_input["file_path"].as_str().unwrap_or("");
            format!("edit file: {path}")
        }
        "Grep" => {
            let pattern = tool_input["pattern"].as_str().unwrap_or("");
            let path = tool_input["path"].as_str().unwrap_or(".");
            format!("search for '{pattern}' in {path}")
        }
        "Glob" => {
            let pattern = tool_input["pattern"].as_str().unwrap_or("");
            format!("find files matching: {pattern}")
        }
        "Agent" => {
            let desc = tool_input["description"].as_str().unwrap_or("");
            let prompt = tool_input["prompt"].as_str().unwrap_or("");
            if !desc.is_empty() {
                format!("agent: {desc}")
            } else {
                let short = if prompt.len() > 200 {
                    &prompt[..200]
                } else {
                    prompt
                };
                format!("agent: {short}")
            }
        }
        "WebFetch" => {
            let url = tool_input["url"].as_str().unwrap_or("");
            format!("fetch: {url}")
        }
        "WebSearch" => {
            let query = tool_input["query"].as_str().unwrap_or("");
            format!("search: {query}")
        }
        _ => {
            // MCP tools or unknown: use tool name + first string value
            let first_val = tool_input
                .as_object()
                .and_then(|obj| obj.values().find_map(|v| v.as_str()))
                .unwrap_or("");
            let short = if first_val.len() > 200 {
                &first_val[..200]
            } else {
                first_val
            };
            format!("{tool_name}: {short}")
        }
    }
}

fn apply_collective_sources(
    candidate: &mut StepCandidate,
    score: &mut i32,
    collective_sources: u32,
) {
    *score += candidate.upgrade_collective_sources(collective_sources);
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

fn file_target(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
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
    } else if recommendations.iter().any(|r| {
        matches!(
            r.source_kind,
            SignalKind::Repair | SignalKind::Preparation | SignalKind::Adjacency
        )
    }) {
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
        .find(|r| {
            matches!(
                r.source_kind,
                SignalKind::Repair | SignalKind::Preparation | SignalKind::Adjacency
            )
        })
        .or_else(|| recommendations.first())
        .map(|r| match r.source_kind {
            SignalKind::Danger => "danger",
            SignalKind::Repair => "repair",
            SignalKind::Preparation => "preparation",
            SignalKind::Adjacency => "adjacency",
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

fn profile_file_guidance_gate(
    supports_file_guidance: bool,
    has_repeated_local_file_actions: bool,
) -> &'static str {
    if !supports_file_guidance {
        "na"
    } else if has_repeated_local_file_actions {
        "open"
    } else {
        "closed"
    }
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
}
