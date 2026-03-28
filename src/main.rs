use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use thronglets::anchor::AnchorClient;
use thronglets::context::simhash;
use thronglets::contracts::{
    GIT_HISTORY_MAX_ENTRIES, PREHOOK_HEADER, PREHOOK_MATCHER, PREHOOK_MAX_COLLECTIVE_QUERIES,
    PREHOOK_MAX_HINTS,
};
use thronglets::eval::{
    evaluate_signal_quality, EvalCheckStatus, EvalCheckThresholds, EvalConfig, EvalFocus,
};
use thronglets::identity::NodeIdentity;
use thronglets::mcp::McpContext;
use thronglets::network::{NetworkCommand, NetworkConfig, NetworkEvent};
use thronglets::profile::{summarize_prehook_profiles, ProfileCheckThresholds};
use thronglets::signals::{
    select as select_signals, Recommendation, Signal, SignalKind, StepCandidate,
};
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};
use thronglets::workspace::{self, WorkspaceState};
use tracing::info;

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
    Id,

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

    /// Auto-record traces from Claude Code PostToolUse hooks.
    /// Reads hook JSON from stdin, records a trace. Designed to be fast (<50ms).
    Hook,

    /// PreToolUse hook: query substrate before tool calls and inject context.
    /// Returns relevant collective intelligence to stdout (appears in agent context).
    /// Silent when no relevant data. Designed to be fast (<50ms).
    Prehook,

    /// One-command setup: configure MCP server + PostToolUse hook + PreToolUse hook.
    /// Makes Thronglets fully automatic and agent-unaware.
    Setup,

    /// Start HTTP API server for non-MCP agents (Python, LangChain, etc.)
    Serve {
        /// HTTP port to listen on
        #[arg(long, default_value_t = 7777)]
        port: u16,
    },

    /// Show connected peers
    Peers,

    /// Show node status and statistics
    Status,

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
    cli_override.clone().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".thronglets")
    })
}

fn load_identity(data_dir: &std::path::Path) -> NodeIdentity {
    NodeIdentity::load_or_generate(&data_dir.join("node.key"))
        .expect("failed to load or generate node identity")
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

    match cli.command {
        Commands::Id => {
            println!("Thronglets v{}", env!("CARGO_PKG_VERSION"));
            println!("Node ID:         {}", identity.short_id());
            println!("Oasyce address:  {}", identity.oasyce_address());
            println!(
                "Public key:      {}",
                hex_encode(&identity.public_key_bytes())
            );
            println!("Data directory:  {}", dir.display());
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
            let trace = Trace::new(
                capability.clone(),
                outcome,
                latency,
                input_size,
                ctx_hash,
                ctx_text,
                None,
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
                        if let Ok(traces) = store.unpublished_traces(50) {
                            if !traces.is_empty() {
                                info!(count = traces.len(), "Publishing local traces to network");
                                let mut ids: Vec<[u8; 32]> = Vec::new();
                                for trace in traces {
                                    ids.push(trace.id);
                                    let _ = cmd_tx.send(NetworkCommand::PublishTrace(trace)).await;
                                }
                                let _ = store.mark_published(&ids);
                            }
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
            // Read PostToolUse JSON from stdin (Claude Code hook payload)
            let mut input = String::new();
            if std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).is_err() {
                std::process::exit(0); // silent fail — don't break Claude Code
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

            // Map tool to capability URI
            let capability = if tool_name.starts_with("mcp__") {
                // MCP tools: mcp__server__tool → mcp:server/tool
                tool_name.replacen("mcp__", "mcp:", 1).replace("__", "/")
            } else {
                format!("claude-code/{tool_name}")
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

            // Session ID from Claude Code
            let session_id = payload["session_id"].as_str().map(String::from);

            // Model from environment or default
            let model =
                std::env::var("CLAUDE_MODEL").unwrap_or_else(|_| "claude-opus-4-6".to_string());

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
            let trace = Trace::new(
                capability.clone(),
                outcome,
                0, // latency not available from hook
                input_size,
                ctx_hash,
                Some(enriched_context),
                session_id.clone(),
                model,
                identity.public_key_bytes(),
                |msg| identity.sign(msg),
            );
            let _ = store.insert(&trace); // silent — never break Claude Code
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
            if matches!(tool_name, "Edit" | "Write") {
                if let Some(fp) = file_path {
                    ws.add_pending_feedback(fp, tool_name);
                }
            }

            // Resolve pending feedback (check git status for previous edits)
            ws.resolve_feedback();

            // Track errors
            if is_error {
                if let Some(err) = workspace::extract_error(&payload["tool_response"]) {
                    ws.record_error(tool_name, context_text, err);
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

            // Read PreToolUse JSON from stdin (Claude Code hook payload)
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

            if has_recent_tool_error {
                if let Some(repair_hint) = ws
                    .repair_trajectory_hint(tool_name)
                    .or_else(|| ws.repair_hints(tool_name))
                {
                    let mut repair_hint = repair_hint;
                    if claim_collective_query(
                        &repair_hint.candidate,
                        &mut collective_queries_remaining,
                    ) {
                        if let Some(store) = cached_collective_store(&mut collective_store, &dir) {
                            if let Ok(collective_sources) = store.count_repair_sources(
                                tool_name,
                                &repair_hint.candidate.steps,
                                168,
                            ) {
                                apply_collective_sources(
                                    &mut repair_hint.candidate,
                                    &mut repair_hint.score,
                                    collective_sources,
                                );
                            }
                        }
                    }

                    signals.push(Signal::repair_candidate(
                        repair_hint.body,
                        repair_hint.score,
                        repair_hint.candidate,
                    ));
                }
            }
            profiler.stage("repair");

            let has_do_next_signal = signals
                .iter()
                .any(|s| matches!(s.kind, SignalKind::Repair | SignalKind::Preparation));
            if has_repeated_local_file_actions && !has_do_next_signal {
                if let Some(mut preparation_hint) =
                    ws.preparation_hint(tool_name, current_file.as_deref())
                {
                    if let (Some(current_file), Some(target)) = (
                        current_file.as_deref(),
                        preparation_hint.candidate.primary_target(),
                    ) {
                        if claim_collective_query(
                            &preparation_hint.candidate,
                            &mut collective_queries_remaining,
                        ) {
                            let edit_target = file_target(current_file);
                            if let Some(store) =
                                cached_collective_store(&mut collective_store, &dir)
                            {
                                if let Ok(collective_sources) =
                                    store.count_preparation_sources(edit_target, target, 168)
                                {
                                    apply_collective_sources(
                                        &mut preparation_hint.candidate,
                                        &mut preparation_hint.score,
                                        collective_sources,
                                    );
                                }
                            }
                        }
                    }

                    signals.push(Signal::preparation_candidate(
                        preparation_hint.body,
                        preparation_hint.score,
                        preparation_hint.candidate,
                    ));
                }
            }
            profiler.stage("preparation");

            // ── Trail pheromone: co-edit patterns ──
            // "Editing A usually means you also need to edit B."
            // Only emitted when patterns exist.
            if has_repeated_local_file_actions {
                if let Some(mut adjacency_hint) =
                    ws.adjacency_hint(tool_name, current_file.as_deref())
                {
                    if let (Some(current_file), Some(target)) = (
                        current_file.as_deref(),
                        adjacency_hint.candidate.primary_target(),
                    ) {
                        if claim_collective_query(
                            &adjacency_hint.candidate,
                            &mut collective_queries_remaining,
                        ) {
                            let current_target = file_target(current_file);
                            if let Some(store) =
                                cached_collective_store(&mut collective_store, &dir)
                            {
                                if let Ok(collective_sources) =
                                    store.count_adjacency_sources(current_target, target, 168)
                                {
                                    apply_collective_sources(
                                        &mut adjacency_hint.candidate,
                                        &mut adjacency_hint.score,
                                        collective_sources,
                                    );
                                }
                            }
                        }
                    }

                    signals.push(Signal::adjacency_candidate(
                        adjacency_hint.body,
                        adjacency_hint.score,
                        adjacency_hint.candidate,
                    ));
                }
            }
            profiler.stage("adjacency");

            // History is a fallback when we don't already know a likely next move.
            let has_higher_priority_signal = !signals.is_empty();
            let mut git_checked = false;
            if !has_higher_priority_signal {
                if supports_file_guidance {
                    git_checked = true;
                    if let Some(git_hints) = current_file
                        .as_ref()
                        .and_then(|fp| git_file_history(fp, GIT_HISTORY_MAX_ENTRIES))
                    {
                        signals.push(Signal::history(git_hints));
                    }
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
            // Detect thronglets binary path
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("thronglets"));
            let bin_str = bin.to_string_lossy();

            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");

            // Read existing settings or create new
            let mut settings: serde_json::Value = if settings_path.exists() {
                let content =
                    std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
                serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
            } else {
                serde_json::json!({})
            };

            // Ensure hooks structure exists
            if settings["hooks"].is_null() {
                settings["hooks"] = serde_json::json!({});
            }

            // Add PostToolUse hook (write path)
            let post_hook = serde_json::json!({
                "matcher": "",
                "hooks": [{"type": "command", "command": format!("{bin_str} hook")}]
            });
            let post_hooks = settings["hooks"]["PostToolUse"]
                .as_array_mut()
                .map(|arr| arr as &mut Vec<serde_json::Value>);

            if let Some(arr) = post_hooks {
                // Check if thronglets hook already exists
                let has_post = arr.iter().any(|h| {
                    h["hooks"].as_array().map_or(false, |hooks| {
                        hooks.iter().any(|hk| {
                            hk["command"]
                                .as_str()
                                .map_or(false, |c| c.contains("thronglets hook"))
                        })
                    })
                });
                if !has_post {
                    arr.push(post_hook);
                }
            } else {
                settings["hooks"]["PostToolUse"] = serde_json::json!([post_hook]);
            }

            // Add PreToolUse hook — only decision-point tools (Edit/Write/Bash/Agent)
            // Read/Grep/Glob are information-gathering, injecting context has zero value
            let pre_hook = serde_json::json!({
                "matcher": PREHOOK_MATCHER,
                "hooks": [{"type": "command", "command": format!("{bin_str} prehook")}]
            });
            let pre_hooks = settings["hooks"]["PreToolUse"]
                .as_array_mut()
                .map(|arr| arr as &mut Vec<serde_json::Value>);

            if let Some(arr) = pre_hooks {
                let has_pre = arr.iter().any(|h| {
                    h["hooks"].as_array().map_or(false, |hooks| {
                        hooks.iter().any(|hk| {
                            hk["command"]
                                .as_str()
                                .map_or(false, |c| c.contains("thronglets prehook"))
                        })
                    })
                });
                if !has_pre {
                    arr.push(pre_hook);
                }
            } else {
                settings["hooks"]["PreToolUse"] = serde_json::json!([pre_hook]);
            }

            // Write settings
            let parent = settings_path.parent().unwrap();
            std::fs::create_dir_all(parent).expect("failed to create .claude directory");
            let formatted = serde_json::to_string_pretty(&settings).unwrap();
            std::fs::write(&settings_path, &formatted).expect("failed to write settings.json");

            // Also configure MCP server
            println!("Thronglets setup complete.");
            println!();
            println!("  ✓ PostToolUse hook  (write: auto-record every tool call)");
            println!("  ✓ PreToolUse hook   (read: inject substrate context before tool calls)");
            println!();
            println!("Settings written to: {}", settings_path.display());
            println!();
            println!("To also enable MCP tools (substrate_query, trace_record):");
            println!("  claude mcp add thronglets -- {bin_str} mcp");
            println!();
            println!("Your AI now has collective memory. It doesn't need to know.");
        }

        Commands::Serve { port } => {
            let store = open_store(&dir);
            let ctx = Arc::new(thronglets::http::HttpContext {
                identity: Arc::new(identity),
                store: Arc::new(store),
            });
            println!("Thronglets HTTP API on http://0.0.0.0:{port}");
            println!("  POST /v1/traces       — record a trace");
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

        Commands::Status => {
            let store = open_store(&dir);
            let trace_count = store.count().unwrap_or(0);
            let cap_count = store
                .distinct_capabilities(1000)
                .map(|s| s.len())
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

            println!("Thronglets v{}", env!("CARGO_PKG_VERSION"));
            println!();
            println!("  Node ID:          {}", identity.short_id());
            println!("  Oasyce address:   {}", identity.oasyce_address());
            println!("  Data directory:   {}", dir.display());
            println!();
            println!("  Trace count:      {}", trace_count);
            println!("  Capabilities:     {}", cap_count);
            println!("  Database size:    {}", size_display);
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

            let eval_thresholds = EvalCheckThresholds::default();
            let store = open_store(&dir);
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
                        &eval_thresholds,
                    ),
                )],
                ReleaseEvalScopeArg::Global => vec![(
                    "global",
                    run_release_eval_section(&store, hours, max_sessions, None, &eval_thresholds),
                )],
                ReleaseEvalScopeArg::Both => vec![
                    (
                        "project",
                        run_release_eval_section(
                            &store,
                            hours,
                            max_sessions,
                            Some(default_project_root.as_path()),
                            &eval_thresholds,
                        ),
                    ),
                    (
                        "global",
                        run_release_eval_section(
                            &store,
                            hours,
                            max_sessions,
                            None,
                            &eval_thresholds,
                        ),
                    ),
                ],
            };

            let overall_failed =
                profile_section.1 || eval_sections.iter().any(|(_, section)| section.1);
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

/// Build a natural language context string from a Claude Code tool call.
/// This is the "WHY" that future agents can read.
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
    thresholds: &EvalCheckThresholds,
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
            let check = summary.check(thresholds);
            let (status, rendered) = summary.render_check(thresholds);
            (
                status.label(),
                matches!(status, EvalCheckStatus::Fail),
                strip_check_header(&rendered),
                serde_json::json!({
                    "status": status.label(),
                    "thresholds": thresholds,
                    "summary": summary,
                    "check": check,
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
                }),
            )
        }
    }
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
