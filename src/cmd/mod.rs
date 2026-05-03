mod daemon;
mod eval;
mod hooks;
mod inspect;
mod onboard;
mod priors;
mod setup;
mod substrate;

use std::path::{Path, PathBuf};

use crate::adapter_ops::*;
use crate::cli::{Cli, Commands};
use crate::hook_support::*;
use crate::render::*;
use crate::setup_support::{AdapterKind, auto_clear_restart_pending_on_runtime_contact};
use crate::*;

use thronglets::identity::{IdentityBinding, NodeIdentity, identity_binding_path};
use thronglets::storage::TraceStore;
use thronglets::workspace::{self, WorkspaceState};

pub(crate) const BOOTSTRAP_SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";
pub(crate) const IDENTITY_SCHEMA_VERSION: &str = "thronglets.identity.v2";
pub(crate) const NETWORK_SCHEMA_VERSION: &str = "thronglets.network.v1";
pub(crate) const PRESENCE_SCHEMA_VERSION: &str = "thronglets.presence.v1";
pub(crate) const SPACE_SCHEMA_VERSION: &str = "thronglets.space.v2";
pub(crate) const VERSION_SCHEMA_VERSION: &str = "thronglets.version.v1";
pub(crate) const DERIVED_GUIDANCE_SCHEMA_VERSION: &str = "thronglets.derived-guidance.v1";
pub(crate) const RELEASE_MAX_LOCAL_RETENTION_DROP_TENTHS_PP: i32 = 50;
pub(crate) const RELEASE_MAX_FAILED_COMMAND_RATE_RISE_TENTHS_PP: i32 = 50;
pub(crate) const RELEASE_MAX_FIRST_CHANGE_LATENCY_RISE_MS: i64 = 5_000;

pub(crate) struct BaseCtx {
    pub dir: PathBuf,
}

pub(crate) struct FullCtx {
    pub dir: PathBuf,
    pub identity: NodeIdentity,
    pub binding: IdentityBinding,
}

pub(crate) fn data_dir(cli_override: &Option<PathBuf>) -> PathBuf {
    cli_override
        .clone()
        .unwrap_or_else(|| home_dir().join(".thronglets"))
}

pub(crate) fn home_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
}

pub(crate) fn load_identity(data_dir: &Path) -> NodeIdentity {
    NodeIdentity::load_or_generate(&data_dir.join("node.key"))
        .expect("failed to load or generate node identity")
}

pub(crate) fn load_identity_binding(data_dir: &Path, identity: &NodeIdentity) -> IdentityBinding {
    IdentityBinding::load_or_create(&identity_binding_path(data_dir), identity)
        .expect("failed to load or create identity binding")
}

pub(crate) fn open_store(data_dir: &Path) -> TraceStore {
    std::fs::create_dir_all(data_dir).expect("failed to create data directory");
    TraceStore::open(&data_dir.join("traces.db")).expect("failed to open trace store")
}

/// Try to spawn a background pulse emitter. Requires both THRONGLETS_SIGIL_ID
/// and THRONGLETS_CHAIN_RPC env vars to be set. Fail-open: returns silently if
/// either is missing. Reloads identity from disk for the background thread.
pub(crate) fn maybe_spawn_pulse(data_dir: &Path, store: &std::sync::Arc<TraceStore>) {
    use thronglets::pulse::PulseEmitter;

    let sigil_id = match std::env::var("THRONGLETS_SIGIL_ID") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let rpc = match std::env::var("THRONGLETS_CHAIN_RPC") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };
    let chain_id = std::env::var("THRONGLETS_CHAIN_ID").unwrap_or_else(|_| "oasyce-1".to_string());
    let key_path = data_dir.join("node.key");

    let emitter = PulseEmitter::new(&sigil_id, &rpc, &chain_id);
    let st = std::sync::Arc::clone(store);

    std::thread::spawn(move || {
        let identity = match NodeIdentity::load_or_generate(&key_path) {
            Ok(id) => std::sync::Arc::new(id),
            Err(e) => {
                tracing::warn!("Pulse: failed to load identity: {e}");
                return;
            }
        };
        thronglets::pulse::pulse_loop(emitter, identity, st);
    });
}

/// Ad-hoc codesign the binary on macOS so the firewall doesn't prompt on every rebuild.
/// Silent no-op on non-macOS or if codesign fails (not critical).
pub(crate) fn codesign_if_macos(bin_path: &Path) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("codesign")
            .args(["-s", "-", "--force"])
            .arg(bin_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

pub(crate) fn load_workspace_state(data_dir: &Path) -> WorkspaceState {
    let mut workspace = WorkspaceState::load(data_dir);
    if workspace.ensure_current_derived_guidance_epoch().is_some() {
        workspace.save(data_dir);
    }
    workspace
}

pub(crate) fn rebuild_priors_data(data_dir: &Path, store: &TraceStore) -> RebuildPriorsData {
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

pub(crate) fn parse_outcome(s: &str) -> thronglets::trace::Outcome {
    use thronglets::trace::Outcome;
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

pub(crate) fn auto_clear_restart_from_agent_source(data_dir: &Path, agent_source: &str) {
    if let Some(agent) = AdapterKind::from_agent_source(agent_source) {
        let _ = auto_clear_restart_pending_on_runtime_contact(data_dir, agent);
    }
}

pub(crate) fn print_json<T: serde::Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

pub(crate) fn print_machine_json<T: serde::Serialize>(command: &'static str, value: &T) {
    print_machine_json_with_schema(BOOTSTRAP_SCHEMA_VERSION, command, value);
}

pub(crate) fn print_machine_json_with_schema<T: serde::Serialize>(
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

pub(crate) async fn dispatch(cli: Cli) {
    let dir = data_dir(&cli.data_dir);
    let base = BaseCtx { dir: dir.clone() };

    // ── Phase 1: commands that don't need identity ──
    if let Commands::Version { json } = &cli.command {
        priors::version(&base, *json);
        return;
    }

    match &cli.command {
        Commands::ProfileSummary => {
            eval::profile_summary();
            return;
        }
        Commands::ProfileCheck => {
            eval::profile_check();
            return;
        }
        Commands::RebuildField { confirm } => {
            if !*confirm {
                eprintln!(
                    "rebuild-field: this deletes the persisted pheromone field snapshot \
                     and tail cursor in {}. The trace store is not affected.\n\
                     Re-run with --confirm to proceed.\n\
                     The next daemon boot (mcp/run/serve) will replay the field from store.",
                    base.dir.display()
                );
                std::process::exit(2);
            }
            let removed = thronglets::pheromone_tail::reset_field_state(&base.dir);
            if removed {
                println!(
                    "rebuild-field: cleared pheromone field state in {}.\n\
                     Restart `thronglets mcp` (or kill the existing MCP processes and \
                     let your AI tool relaunch them) to trigger a clean replay from the \
                     trace store.",
                    base.dir.display()
                );
            } else {
                println!(
                    "rebuild-field: nothing to clear (field snapshot and cursor not present)."
                );
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
            eval::release_check(
                &base,
                *hours,
                *max_sessions,
                project_root.clone(),
                *eval_scope,
                *global,
                *require_profile_samples,
                compare_baseline.clone(),
                *json,
            );
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
            eval::eval_signals(
                &base,
                *hours,
                *max_sessions,
                project_root.clone(),
                *global,
                *local_history_gate_min,
                *pattern_support_min,
                compare_baseline.clone(),
                *top_breakdowns,
                *focus,
                *json,
            );
            return;
        }
        Commands::EvalEmergence {
            hours,
            max_sessions,
            project_root,
            global,
            json,
        } => {
            eval::eval_emergence(
                &base,
                *hours,
                *max_sessions,
                project_root.clone(),
                *global,
                *json,
            );
            return;
        }
        Commands::AmbientPriors { json } => {
            priors::ambient_priors(&base, *json);
            return;
        }
        Commands::RebuildPriors { json } => {
            priors::rebuild_priors(&base, *json);
            return;
        }
        Commands::Setup => {
            setup::setup(&base);
            return;
        }
        Commands::Detect { agent, json } => {
            setup::detect(&base, *agent, *json);
            return;
        }
        Commands::InstallPlan {
            agent,
            runtime,
            json,
        } => {
            setup::install_plan_cmd(&base, *agent, *runtime, *json);
            return;
        }
        Commands::ApplyPlan { agent, json } => {
            setup::apply_plan(&base, *agent, *json);
            return;
        }
        Commands::Doctor { agent, json } => {
            setup::doctor(&base, *agent, *json);
            return;
        }
        Commands::Bootstrap { agent, json } => {
            setup::bootstrap(&base, *agent, *json);
            return;
        }
        Commands::ClearRestart { agent, json } => {
            setup::clear_restart(&base, *agent, *json);
            return;
        }
        Commands::RuntimeReady { agent, json } => {
            setup::runtime_ready(&base, *agent, *json);
            return;
        }
        _ => {}
    }

    // ── Phase 2: commands that need identity ──
    let identity = load_identity(&dir);
    let identity_binding = load_identity_binding(&dir, &identity);
    let ctx = FullCtx {
        dir: dir.clone(),
        identity,
        binding: identity_binding,
    };

    match cli.command {
        Commands::Version { .. } => unreachable!("version handled before identity bootstrap"),
        Commands::AmbientPriors { .. } => {
            unreachable!("ambient-priors handled before identity bootstrap")
        }
        Commands::RebuildPriors { .. } => {
            unreachable!("rebuild-priors handled before identity bootstrap")
        }
        Commands::EvalEmergence { .. } => {
            unreachable!("eval-emergence handled before identity bootstrap")
        }

        Commands::Start { json } => onboard::start(&ctx, json),
        Commands::Share {
            output,
            ttl_hours,
            json,
        } => onboard::share(&ctx, output, ttl_hours, json).await,
        Commands::Join { file, json } => onboard::join(&ctx, file, json).await,

        Commands::Id { json } => inspect::id(&ctx, json),
        Commands::AuthorizationCheck { json } => inspect::authorization_check(&ctx, json),
        Commands::OwnerBind {
            owner_account,
            json,
        } => inspect::owner_bind(&ctx, owner_account, json),
        Commands::ConnectionExport {
            output,
            ttl_hours,
            json,
            include_oasyce_surface,
        } => onboard::connection_export(&ctx, output, ttl_hours, json, include_oasyce_surface),
        Commands::ConnectionInspect { file, json } => onboard::connection_inspect(&ctx, file, json),
        Commands::ConnectionJoin { file, json } => onboard::connection_join(&ctx, file, json),

        Commands::Record {
            capability,
            outcome,
            latency,
            input_size,
            context,
            model,
        } => substrate::record(
            &ctx, capability, outcome, latency, input_size, context, model,
        ),
        Commands::Query { capability } => substrate::query(&ctx, capability),
        Commands::SignalPost {
            kind,
            context,
            message,
            tool,
            model,
            session_id,
            space,
            ttl_hours,
        } => substrate::signal_post(
            &ctx, kind, context, message, tool, model, session_id, space, ttl_hours,
        ),
        Commands::SignalQuery {
            context,
            kind,
            space,
            limit,
        } => substrate::signal_query(&ctx, context, kind, space, limit),
        Commands::SignalFeed {
            hours,
            kind,
            min_sources,
            space,
            limit,
        } => substrate::signal_feed(&ctx, hours, kind, min_sources, space, limit),
        Commands::PresencePing {
            space,
            mode,
            model,
            session_id,
            ttl_minutes,
            json,
        } => substrate::presence_ping(&ctx, space, mode, model, session_id, ttl_minutes, json),
        Commands::PresenceFeed {
            hours,
            space,
            limit,
            json,
        } => substrate::presence_feed(&ctx, hours, space, limit, json),
        Commands::Space {
            space,
            hours,
            limit,
            json,
        } => substrate::space(&ctx, space, hours, limit, json),

        Commands::Run { port, bootstrap } => daemon::run(ctx, port, bootstrap).await,
        Commands::Mcp {
            port,
            bootstrap,
            local,
            agent,
        } => daemon::mcp(ctx, port, bootstrap, local, agent).await,
        Commands::Serve {
            port,
            p2p_port,
            bootstrap,
            local,
        } => daemon::serve(ctx, port, p2p_port, bootstrap, local).await,

        Commands::Hook => hooks::hook(&ctx),
        Commands::Prehook => hooks::prehook(&ctx),
        Commands::Ingest {
            session,
            model,
            space,
            json,
        } => hooks::ingest(&ctx, session, model, space, json),
        Commands::LifecycleHook { event } => hooks::lifecycle_hook(&ctx, event),

        Commands::Anchor {
            rpc,
            chain_id,
            hours,
        } => inspect::anchor(&ctx, rpc, chain_id, hours).await,
        Commands::Pulse {
            sigil_id,
            rpc,
            chain_id,
        } => inspect::pulse(&ctx, sigil_id, rpc, chain_id).await,
        Commands::Peers { json, limit } => inspect::peers(&ctx, json, limit),
        Commands::NetCheck {
            json,
            bootstrap_offline,
        } => inspect::net_check(&ctx, json, bootstrap_offline),
        Commands::Status { json } => inspect::status(&ctx, json),

        Commands::ProfileSummary
        | Commands::ProfileCheck
        | Commands::RebuildField { .. }
        | Commands::ReleaseCheck { .. }
        | Commands::EvalSignals { .. } => {
            unreachable!("eval/maintenance commands handled before identity bootstrap")
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
    }
}
