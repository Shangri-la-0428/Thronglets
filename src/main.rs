use std::path::PathBuf;
use std::sync::Arc;
use clap::{Parser, Subcommand};
use thronglets::anchor::AnchorClient;
use thronglets::context::simhash;
use thronglets::identity::NodeIdentity;
use thronglets::mcp::McpContext;
use thronglets::network::{NetworkCommand, NetworkConfig, NetworkEvent};
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};
use thronglets::workspace::{self, WorkspaceState};
use tracing::info;

#[derive(Parser)]
#[command(name = "thronglets", version, about = "P2P shared memory substrate for AI agents")]
struct Cli {
    /// Data directory (default: ~/.thronglets)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
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
    TraceStore::open(&data_dir.join("traces.db"))
        .expect("failed to open trace store")
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
            println!("Public key:      {}", hex_encode(&identity.public_key_bytes()));
            println!("Data directory:  {}", dir.display());
        }

        Commands::Record { capability, outcome, latency, input_size, context, model } => {
            let store = open_store(&dir);
            let outcome = parse_outcome(&outcome);
            let ctx_hash = simhash(&context);
            let ctx_text = if context.is_empty() { None } else { Some(context.clone()) };
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

            let libp2p_keypair = libp2p::identity::Keypair::ed25519_from_bytes(
                &mut identity.secret_key_bytes()
            ).expect("failed to create libp2p keypair");

            let bootstrap_addrs: Vec<libp2p::Multiaddr> = bootstrap.iter()
                .filter_map(|s| s.parse().ok())
                .collect();

            let config = NetworkConfig {
                listen_port: port,
                bootstrap_peers: bootstrap_addrs,
            };

            let (cmd_tx, mut event_rx) = thronglets::network::start(libp2p_keypair, config)
                .await
                .expect("failed to start network");

            info!("Node {} running. Press Ctrl+C to stop.", identity.short_id());

            let mut evaporation_interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            evaporation_interval.tick().await;
            let mut dht_publish_interval = tokio::time::interval(std::time::Duration::from_secs(300));
            dht_publish_interval.tick().await;
            // Scan for locally-recorded traces that haven't been published to the network
            let mut publish_scan_interval = tokio::time::interval(std::time::Duration::from_secs(30));
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
                let libp2p_keypair = libp2p::identity::Keypair::ed25519_from_bytes(
                    &mut identity.secret_key_bytes()
                ).expect("failed to create libp2p keypair");

                let bootstrap_addrs: Vec<libp2p::Multiaddr> = bootstrap.iter()
                    .filter_map(|s| s.parse().ok())
                    .collect();

                let config = NetworkConfig {
                    listen_port: p,
                    bootstrap_peers: bootstrap_addrs,
                };

                let (cmd_tx, mut event_rx) = thronglets::network::start(libp2p_keypair, config)
                    .await
                    .expect("failed to start network");

                let store_bg = Arc::clone(&store);
                tokio::spawn(async move {
                    let mut evaporation_interval = tokio::time::interval(std::time::Duration::from_secs(3600));
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

        Commands::Anchor { rpc, chain_id, hours } => {
            let store = open_store(&dir);
            let client = AnchorClient::new(&rpc, &chain_id);

            let traces = store.unanchored_traces(hours, 500)
                .expect("failed to query unanchored traces");

            if traces.is_empty() {
                println!("No unanchored traces from the last {} hours.", hours);
                return;
            }

            println!("Found {} unanchored traces. Anchoring to {} (chain: {})...",
                traces.len(), rpc, chain_id);

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
                            println!("  Batch tx: {}... ({} anchored, {} skipped)",
                                &result.tx_hash[..16], result.anchored, result.skipped);
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
                if obj.contains_key("error") || obj.get("success") == Some(&serde_json::Value::Bool(false)) {
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
            let model = std::env::var("CLAUDE_MODEL")
                .unwrap_or_else(|_| "claude-opus-4-6".to_string());

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
            ws.record_action(tool_name, file_path.clone());

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

            let mut hints: Vec<String> = Vec::new();
            let ws = WorkspaceState::load(&dir);
            let current_file = workspace::extract_file_path(tool_name, &payload["tool_input"]);

            // ── Git history: the one signal that's always decision-relevant ──
            // Like a bee's waggle dance: direction + distance, always useful.
            if matches!(tool_name, "Edit" | "Write") {
                if let Some(ref fp) = current_file {
                    if let Some(git_hints) = git_file_history(fp, 5) {
                        hints.push(git_hints);
                    }
                }
            }

            // ── Danger pheromone: low edit retention ──
            // If recent edits are mostly reverted, this is a strong warning.
            // Only signal when retention < 50% (anomaly).
            if let Some(fb_hints) = ws.feedback_hints(current_file.as_deref()) {
                // Parse retention rate from the hint
                let is_warning = fb_hints.contains("retention:")
                    && fb_hints.split("retention:").nth(1)
                        .and_then(|s| s.trim().split('%').next())
                        .and_then(|s| s.trim().parse::<f64>().ok())
                        .is_some_and(|rate| rate < 50.0);
                if is_warning {
                    hints.push(format!("  ⚠ {}", fb_hints.trim()));
                }
            }

            // ── Trail pheromone: co-edit patterns ──
            // "Editing A usually means you also need to edit B."
            // Only emitted when patterns exist.
            if let Some(decision_hints) = ws.decision_hints(tool_name, current_file.as_deref()) {
                // Only keep co-edit lines (prep reads are low-value)
                let co_edits: String = decision_hints.lines()
                    .filter(|l| l.contains("co-edited with"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !co_edits.is_empty() {
                    hints.push(co_edits);
                }
            }

            // ── Alarm pheromone: recent errors with this tool ──
            // Only emitted when errors happened in the last hour.
            let recent_errors: Vec<String> = ws.recent_errors.iter()
                .filter(|e| {
                    e.tool == tool_name
                    && (chrono::Utc::now().timestamp_millis() - e.timestamp_ms) < 3_600_000
                })
                .take(1)
                .map(|e| {
                    let snippet = if e.error_snippet.len() > 80 {
                        format!("{}...", &e.error_snippet[..80])
                    } else {
                        e.error_snippet.clone()
                    };
                    format!("  ⚠ recent error: {snippet}")
                })
                .collect();
            for e in recent_errors {
                hints.push(e);
            }

            // Output: only when there's something worth saying
            if !hints.is_empty() {
                println!("[thronglets]");
                for h in &hints {
                    println!("{h}");
                }
            }
            // Normal state → complete silence. Zero tokens.
        }

        Commands::Setup => {
            // Detect thronglets binary path
            let bin = std::env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("thronglets"));
            let bin_str = bin.to_string_lossy();

            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            let settings_path = PathBuf::from(&home).join(".claude").join("settings.json");

            // Read existing settings or create new
            let mut settings: serde_json::Value = if settings_path.exists() {
                let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".into());
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
                            hk["command"].as_str().map_or(false, |c| c.contains("thronglets hook"))
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
                "matcher": "Edit|Write|Bash|Agent",
                "hooks": [{"type": "command", "command": format!("{bin_str} prehook")}]
            });
            let pre_hooks = settings["hooks"]["PreToolUse"]
                .as_array_mut()
                .map(|arr| arr as &mut Vec<serde_json::Value>);

            if let Some(arr) = pre_hooks {
                let has_pre = arr.iter().any(|h| {
                    h["hooks"].as_array().map_or(false, |hooks| {
                        hooks.iter().any(|hk| {
                            hk["command"].as_str().map_or(false, |c| c.contains("thronglets prehook"))
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
            thronglets::http::serve(ctx, port).await
                .expect("HTTP server failed");
        }

        Commands::Peers => {
            println!("The 'peers' command requires a running node.");
            println!("Use 'thronglets run' to start a node, then peers are logged to console.");
        }

        Commands::Status => {
            let store = open_store(&dir);
            let trace_count = store.count().unwrap_or(0);
            let cap_count = store.distinct_capabilities(1000)
                .map(|s| s.len())
                .unwrap_or(0);
            let db_path = dir.join("traces.db");
            let db_size = std::fs::metadata(&db_path)
                .map(|m| m.len())
                .unwrap_or(0);

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
    }
}

/// Build a natural language context string from a Claude Code tool call.
/// This is the "WHY" that future agents can read.
/// Get recent git history for a file. Returns None if not in a git repo or no history.
fn git_file_history(file_path: &str, max_entries: usize) -> Option<String> {
    use std::process::Command;
    use std::path::Path;

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
                let short = if prompt.len() > 200 { &prompt[..200] } else { prompt };
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
            let first_val = tool_input.as_object()
                .and_then(|obj| obj.values().find_map(|v| v.as_str()))
                .unwrap_or("");
            let short = if first_val.len() > 200 { &first_val[..200] } else { first_val };
            format!("{tool_name}: {short}")
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
