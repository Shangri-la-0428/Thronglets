use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use thronglets::eval::EvalFocus;
use thronglets::identity::DEFAULT_CONNECTION_FILE_TTL_HOURS;
use thronglets::posts::{DEFAULT_SIGNAL_TTL_HOURS, SignalPostKind};
use thronglets::presence::DEFAULT_PRESENCE_TTL_MINUTES;

use crate::setup_support::AdapterKind;

pub(crate) const DEFAULT_CONNECTION_FILE_NAME: &str = "thronglets.connection.json";
const TOP_LEVEL_AFTER_HELP: &str = "Normal path:\n  thronglets start\n  thronglets share\n  thronglets join\n  thronglets status\n\nAdvanced and machine-facing commands remain available, but are hidden from this top-level help so normal onboarding stays simple.";

#[derive(Parser)]
#[command(
    name = "thronglets",
    version,
    about = "P2P shared memory substrate for AI agents",
    after_help = TOP_LEVEL_AFTER_HELP
)]
pub(crate) struct Cli {
    /// Data directory (default: ~/.thronglets)
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum EvalSignalFocusArg {
    All,
    Repair,
    Preparation,
    Adjacency,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ReleaseEvalScopeArg {
    Project,
    Global,
    Both,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum AdapterArg {
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
    pub(crate) fn includes(self, adapter: AdapterKind) -> bool {
        match self {
            Self::All => true,
            Self::Claude => matches!(adapter, AdapterKind::Claude),
            Self::Codex => matches!(adapter, AdapterKind::Codex),
            Self::Cursor => matches!(adapter, AdapterKind::Cursor),
            Self::Openclaw => matches!(adapter, AdapterKind::OpenClaw),
            Self::Generic => matches!(adapter, AdapterKind::Generic),
        }
    }

    pub(crate) fn as_kind(self) -> Option<AdapterKind> {
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
pub(crate) enum RuntimeArg {
    All,
    Python,
    Node,
    Shell,
}

impl RuntimeArg {
    pub(crate) fn key(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Python => Some("python"),
            Self::Node => Some("node"),
            Self::Shell => Some("shell"),
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum SignalKindArg {
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

#[derive(Subcommand)]
pub(crate) enum Commands {
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

        /// Minimum independent source count to include.
        #[arg(long, default_value_t = 0)]
        min_sources: u32,

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
    /// Emit a multi-dimensional pulse to the Oasyce chain
    Pulse {
        /// Sigil ID to pulse for
        #[arg(long)]
        sigil_id: String,

        /// Oasyce chain RPC endpoint
        #[arg(long, default_value = "http://localhost:1317")]
        rpc: String,

        /// Chain ID
        #[arg(long, default_value = "oasyce-1")]
        chain_id: String,
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
    /// Ingest Psyche exports from stdin into the continuity store.
    /// Expects JSON: { "throngletsExports": [...] }
    /// Designed for pipe: `psyche emit ... --json | thronglets ingest`
    Ingest {
        /// Session ID for trace attribution.
        #[arg(long)]
        session: Option<String>,
        /// Model name for trace attribution.
        #[arg(long, default_value = "psyche")]
        model: String,
        /// Substrate space for trace scoping.
        #[arg(long)]
        space: Option<String>,
        /// Emit result as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

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
    /// Reset the pheromone field — delete the persisted field snapshot and
    /// tail cursor so the next daemon boot replays the field from the trace
    /// store. Use after fixing physics bugs or after long pipeline outages
    /// where the in-memory field has diverged from the store of truth.
    ///
    /// The trace store itself is untouched; this only resets the derived
    /// view. Requires `--confirm`.
    RebuildField {
        /// Confirm the destructive action.
        #[arg(long)]
        confirm: bool,
    },

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

    #[command(hide = true)]
    /// Summarize offline signal quality together with space-level emergence indicators.
    EvalEmergence {
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

        /// Emit machine-readable JSON instead of a text summary.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
