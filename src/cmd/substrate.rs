use super::*;

use crate::cli::SignalKindArg;
use thronglets::context::simhash;
use thronglets::continuity::summarize_recent_continuity;
use thronglets::posts::{
    DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS, SignalPostKind, SignalTraceConfig,
    create_feed_reinforcement_traces, create_query_reinforcement_traces, create_signal_trace,
    filter_signal_feed_results, summarize_recent_signal_feed, summarize_signal_traces,
};
use thronglets::presence::{PresenceTraceConfig, create_presence_trace, summarize_recent_presence};
use thronglets::trace::{EvidenceVerification, MethodCompliance, TraceEvidence};

#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    ctx: &FullCtx,
    capability: String,
    outcome: String,
    latency: u32,
    input_size: u32,
    context: String,
    model: String,
    session_id: Option<String>,
    space: Option<String>,
    method_compliance: Option<String>,
    artifact_refs: Vec<String>,
    trial_key: Option<String>,
    verification: Option<String>,
) {
    let store = open_store(&ctx.dir);
    let outcome = parse_outcome(&outcome);
    let method_compliance = method_compliance.as_deref().map(|value| {
        MethodCompliance::parse(value).unwrap_or_else(|| {
            panic!("invalid method_compliance: {value}");
        })
    });
    let verification = verification.as_deref().map(|value| {
        EvidenceVerification::parse(value).unwrap_or_else(|| {
            panic!("invalid verification: {value}");
        })
    });
    let evidence = TraceEvidence {
        artifact_refs,
        trial_key,
        verification,
    };

    let service_ctx = thronglets::service::Ctx {
        store: &store,
        field: None,
        identity: &ctx.identity,
        binding: &ctx.binding,
    };
    let recorded = thronglets::service::record_trace(
        &service_ctx,
        thronglets::service::RecordTraceReq {
            capability: capability.clone(),
            outcome,
            latency_ms: latency,
            input_size,
            context,
            model,
            session_id,
            space,
            agent_id: None,
            sigil_id: None,
            method_compliance,
            evidence,
        },
        None,
    )
    .expect("failed to record trace");
    let thronglets::service::RecordResult::Trace(recorded) = recorded else {
        unreachable!("record command does not write external continuity traces");
    };
    println!("Trace recorded:");
    println!("  ID:         {}", recorded.trace_id);
    println!("  Capability: {}", capability);
    println!("  Outcome:    {:?}", outcome);
}

pub(crate) fn query(ctx: &FullCtx, capability: String) {
    let store = open_store(&ctx.dir);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn signal_post(
    ctx: &FullCtx,
    kind: SignalKindArg,
    context: String,
    message: String,
    tool: Option<String>,
    model: String,
    session_id: Option<String>,
    space: Option<String>,
    ttl_hours: u32,
) {
    let store = open_store(&ctx.dir);
    let context = thronglets::context::format_signal_context(tool.as_deref(), &context);
    let trace = create_signal_trace(
        kind.into(),
        &context,
        &message,
        SignalTraceConfig {
            model_id: model,
            session_id,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            agent_id: None,
            sigil_id: None,
            space: space.clone(),
            ttl_hours,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
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

pub(crate) fn signal_query(
    ctx: &FullCtx,
    context: String,
    kind: Option<SignalKindArg>,
    space: Option<String>,
    limit: usize,
) {
    let store = open_store(&ctx.dir);
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
    let results = summarize_signal_traces(&traces, &context, limit);
    for trace in create_query_reinforcement_traces(
        &results,
        &context,
        SignalTraceConfig {
            model_id: "thronglets-query".into(),
            session_id: None,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            agent_id: None,
            sigil_id: None,
            space: None,
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = store.insert(&trace);
    }
    render_signal_query_results(&results);
}

pub(crate) fn signal_feed(
    ctx: &FullCtx,
    hours: u32,
    kind: Option<SignalKindArg>,
    min_sources: u32,
    space: Option<String>,
    limit: usize,
) {
    let store = open_store(&ctx.dir);
    let traces = store
        .query_recent_signal_traces(hours, kind.map(Into::into), limit, space.as_deref())
        .expect("failed to query recent signal traces");
    let results =
        filter_signal_feed_results(summarize_recent_signal_feed(&traces, limit), min_sources);
    for trace in create_feed_reinforcement_traces(
        &results,
        SignalTraceConfig {
            model_id: "thronglets-feed".into(),
            session_id: None,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            agent_id: None,
            sigil_id: None,
            space: None,
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = store.insert(&trace);
    }
    render_signal_feed_results(&results);
}

pub(crate) fn presence_ping(
    ctx: &FullCtx,
    space: Option<String>,
    mode: Option<String>,
    model: String,
    session_id: Option<String>,
    ttl_minutes: u32,
    json: bool,
) {
    let store = open_store(&ctx.dir);
    let trace = create_presence_trace(
        PresenceTraceConfig {
            model_id: model,
            session_id,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            space: space.clone(),
            mode: mode.clone(),
            sigil_id: None,
            capability: None,
            ttl_minutes,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
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

pub(crate) fn presence_feed(
    ctx: &FullCtx,
    hours: u32,
    space: Option<String>,
    limit: usize,
    json: bool,
) {
    let store = open_store(&ctx.dir);
    let fetch_limit = if space.is_some() {
        limit.max(1).saturating_mul(10)
    } else {
        limit
    };
    let traces = store
        .query_recent_presence_traces(hours, fetch_limit)
        .expect("failed to query recent presence traces");
    let results = summarize_recent_presence(&traces, space.as_deref(), limit);
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

pub(crate) fn space(ctx: &FullCtx, space: String, hours: u32, limit: usize, json: bool) {
    let store = open_store(&ctx.dir);
    let ws = load_workspace_state(&ctx.dir);
    let presence_traces = store
        .query_recent_presence_traces(hours, limit.max(1).saturating_mul(10))
        .expect("failed to query recent presence traces");
    let sessions = summarize_recent_presence(&presence_traces, Some(&space), limit);
    let signal_traces = store
        .query_recent_signal_traces(hours, None, limit, Some(&space))
        .expect("failed to query recent signal traces");
    let signals = summarize_recent_signal_feed(&signal_traces, limit);
    for trace in create_feed_reinforcement_traces(
        &signals,
        SignalTraceConfig {
            model_id: "thronglets-space".into(),
            session_id: None,
            owner_account: ctx.binding.owner_account.clone(),
            device_identity: Some(ctx.binding.device_identity.clone()),
            agent_id: None,
            sigil_id: None,
            space: Some(space.clone()),
            ttl_hours: DEFAULT_SIGNAL_REINFORCEMENT_TTL_HOURS,
        },
        ctx.identity.public_key_bytes(),
        |msg| ctx.identity.sign(msg),
    ) {
        let _ = store.insert(&trace);
    }
    let local_feedback = ws.space_feedback_summary(Some(&space));
    let learning =
        thronglets::service::space_learning_view(&store, &local_feedback, &space, hours, limit);
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
        learning,
        continuity,
        local_feedback,
    };
    if json {
        print_machine_json_with_schema(SPACE_SCHEMA_VERSION, "space", &data);
    } else {
        render_space_snapshot(&data);
    }
}
