use super::*;

use std::sync::Arc;

use crate::cli::AdapterArg;
use thronglets::mcp::McpContext;
use thronglets::network_runtime::{
    NetworkRuntimeOptions, NetworkRuntimeRequest, start_network_runtime,
};
use thronglets::pheromone::PheromoneField;
use thronglets::pheromone_tail::{DEFAULT_POLL_INTERVAL, FieldTail};
use tracing::info;

pub(crate) async fn run(ctx: FullCtx, port: u16, bootstrap: Vec<String>) {
    let store = Arc::new(open_store(&ctx.dir));
    let field = Arc::new(PheromoneField::new());
    let cold_start = init_field_from_store(&field, &store, &ctx.dir, false);
    let command_tx = start_network_runtime(NetworkRuntimeRequest {
        data_dir: &ctx.dir,
        identity: &ctx.identity,
        binding: &ctx.binding,
        store: Arc::clone(&store),
        field: Some(Arc::clone(&field)),
        listen_port: port,
        bootstrap: &bootstrap,
        options: NetworkRuntimeOptions::node(),
    })
    .await
    .expect("failed to start network");

    // Background pulse emitter (fail-open: no-op if env vars missing)
    maybe_spawn_pulse(&ctx.dir, &store);

    // Field tail: materializes field from new store rows. Source-of-truth =
    // the SQLite store; the field is a derived view.
    let _tail_handle = spawn_field_tail(
        Arc::clone(&field),
        Arc::clone(&store),
        &ctx.dir,
        ctx.identity.public_key_bytes(),
        cold_start,
    );

    // Field socket: prehook queries the live field via IPC
    let _field_socket = thronglets::pheromone_socket::start_listener(Arc::clone(&field), &ctx.dir);

    info!(
        "Node {} running. Press Ctrl+C to stop.",
        ctx.identity.short_id()
    );

    tokio::signal::ctrl_c()
        .await
        .expect("failed to wait for shutdown signal");
    info!("Shutting down...");
    let _ = command_tx
        .send(thronglets::network::NetworkCommand::Shutdown)
        .await;
}

pub(crate) async fn mcp(
    ctx: FullCtx,
    port: Option<u16>,
    bootstrap: Vec<String>,
    local: bool,
    agent: Option<AdapterArg>,
) {
    let store = Arc::new(open_store(&ctx.dir));
    let field = Arc::new(PheromoneField::new());

    let cold_start = init_field_from_store(&field, &store, &ctx.dir, true);

    if let Some(adapter) = agent.and_then(AdapterArg::as_kind) {
        let _ =
            crate::setup_support::auto_clear_restart_pending_on_runtime_contact(&ctx.dir, adapter);
    }

    // Auto-join P2P network unless --local is specified.
    let network_tx = if !local {
        let p2p_port = port.unwrap_or(0);
        Some(
            start_network_runtime(NetworkRuntimeRequest {
                data_dir: &ctx.dir,
                identity: &ctx.identity,
                binding: &ctx.binding,
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

    // Non-blocking update check (background thread, never fails)
    thronglets::update::check_for_update();

    // Background pulse emitter (fail-open: no-op if env vars missing)
    maybe_spawn_pulse(&ctx.dir, &store);

    // Field tail: any new trace inserted into the store (by hooks, MCP, or
    // network ingest) gets excited into the field on next poll.
    let _tail_handle = spawn_field_tail(
        Arc::clone(&field),
        Arc::clone(&store),
        &ctx.dir,
        ctx.identity.public_key_bytes(),
        cold_start,
    );

    // Field socket: prehook queries the live field via IPC instead of loading stale JSON
    let _field_socket = thronglets::pheromone_socket::start_listener(Arc::clone(&field), &ctx.dir);

    let mcp_ctx = Arc::new(McpContext {
        identity: Arc::new(ctx.identity),
        binding: Arc::new(ctx.binding),
        store,
        field: Arc::clone(&field),
        network_tx,
    });

    thronglets::mcp::serve_stdio(mcp_ctx).await;

    persist_field_on_shutdown(&field, &ctx.dir);
}

pub(crate) async fn serve(
    ctx: FullCtx,
    port: u16,
    p2p_port: u16,
    bootstrap: Vec<String>,
    local: bool,
) {
    let store = Arc::new(open_store(&ctx.dir));
    let field = Arc::new(PheromoneField::new());

    let cold_start = init_field_from_store(&field, &store, &ctx.dir, true);

    // Auto-join P2P network unless --local is specified.
    let _network_tx = if !local {
        Some(
            start_network_runtime(NetworkRuntimeRequest {
                data_dir: &ctx.dir,
                identity: &ctx.identity,
                binding: &ctx.binding,
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

    // Non-blocking update check (background thread, never fails)
    thronglets::update::check_for_update();

    // Background pulse emitter (fail-open: no-op if env vars missing)
    maybe_spawn_pulse(&ctx.dir, &store);

    // Field tail: any new trace inserted into the store gets excited into
    // the field on next poll.
    let _tail_handle = spawn_field_tail(
        Arc::clone(&field),
        Arc::clone(&store),
        &ctx.dir,
        ctx.identity.public_key_bytes(),
        cold_start,
    );

    // Field socket: prehook queries the live field via IPC
    let _field_socket = thronglets::pheromone_socket::start_listener(Arc::clone(&field), &ctx.dir);

    let http_ctx = Arc::new(thronglets::http::HttpContext {
        identity: Arc::new(ctx.identity),
        binding: Arc::new(ctx.binding),
        store,
        data_dir: ctx.dir.clone(),
    });
    println!("Thronglets HTTP API on http://0.0.0.0:{port}");
    if !local {
        println!("  P2P network joined (port {p2p_port}, 0 = random)");
    }
    println!("  POST /v1/traces       \u{2014} record a trace");
    println!("  POST /v1/presence     \u{2014} leave a lightweight session presence heartbeat");
    println!("  POST /v1/signals      \u{2014} leave an explicit short signal");
    println!("  GET  /v1/presence/feed \u{2014} show recent active sessions in a space");
    println!("  GET  /v1/signals      \u{2014} query explicit short signals");
    println!("  GET  /v1/signals/feed \u{2014} show recent converging explicit signals");
    println!("  GET  /v1/query        \u{2014} query the substrate");
    println!("  GET  /v1/capabilities \u{2014} list capabilities");
    println!("  GET  /v1/status       \u{2014} node status");
    thronglets::http::serve(http_ctx, port)
        .await
        .expect("HTTP server failed");

    persist_field_on_shutdown(&field, &ctx.dir);
}

/// Initialize the in-memory field at daemon boot.
///
/// Strategy:
/// 1. If `<data_dir>/pheromone-field.v1.json` exists and `try_disk_first`,
///    restore from snapshot — this preserves field state across daemon restarts.
///    Cursor (already on disk) governs where the tail picks up.
/// 2. Otherwise (cold start, or daemon=`run` which always rehydrates from store),
///    call `hydrate_from_store`. Seed the tail cursor to the latest agent
///    trace timestamp so we don't double-excite traces hydrate just replayed.
///
/// Returns `true` for cold start (we hydrated and need to seed cursor).
fn init_field_from_store(
    field: &Arc<PheromoneField>,
    store: &Arc<thronglets::storage::TraceStore>,
    data_dir: &std::path::Path,
    try_disk_first: bool,
) -> bool {
    let field_path = data_dir.join("pheromone-field.v1.json");
    if try_disk_first
        && field_path.exists()
        && let Ok(data) = std::fs::read_to_string(&field_path)
        && let Ok(snapshot) = serde_json::from_str(&data)
    {
        field.restore(&snapshot);
        tracing::info!(points = field.len(), "Restored pheromone field from disk");
        return false;
    }
    field.hydrate_from_store(store);
    true
}

fn spawn_field_tail(
    field: Arc<PheromoneField>,
    store: Arc<thronglets::storage::TraceStore>,
    data_dir: &std::path::Path,
    local_node_pubkey: [u8; 32],
    cold_start: bool,
) -> tokio::task::JoinHandle<()> {
    let tail = Arc::new(FieldTail::new(
        field,
        Arc::clone(&store),
        data_dir,
        local_node_pubkey,
    ));
    if cold_start && let Ok(Some(latest_ts)) = store.latest_agent_trace_timestamp_ms() {
        // Hydrate already excited every agent trace through this timestamp.
        // Seed cursor so the tail loop only handles strictly-later traces.
        // SQLite returns i64; cursor space is u64 (matches Trace::timestamp).
        tail.seed_cursor(latest_ts.max(0) as u64);
    }
    // On warm start (restored from disk), the cursor on disk already governs
    // where to resume — drain whatever arrived while we were down.
    let drained = tail.drain();
    if drained > 0 {
        tracing::info!(drained, "Field tail drained backlog");
    }
    Arc::clone(&tail).spawn(DEFAULT_POLL_INTERVAL)
}

fn persist_field_on_shutdown(field: &Arc<PheromoneField>, data_dir: &std::path::Path) {
    let field_path = data_dir.join("pheromone-field.v1.json");
    let snapshot = field.snapshot();
    if snapshot.points.is_empty() {
        let _ = std::fs::remove_file(&field_path);
    } else if let Ok(data) = serde_json::to_string(&snapshot) {
        let _ = std::fs::write(&field_path, data);
    }
}
