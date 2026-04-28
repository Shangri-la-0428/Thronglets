use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use thronglets::context::simhash;
use thronglets::identity::NodeIdentity;
use thronglets::signals::{Recommendation, RecommendationKind, SignalKind, StepCandidate};
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};
use thronglets::workspace::WorkspaceState;

fn make_trace(
    identity: &NodeIdentity,
    capability: &str,
    outcome: Outcome,
    context: &str,
    session_id: &str,
    timestamp: u64,
) -> Trace {
    let mut trace = Trace::new(
        capability.to_string(),
        outcome,
        10,
        1,
        simhash(context),
        Some(context.to_string()),
        Some(session_id.to_string()),
        "test-agent".to_string(),
        identity.public_key_bytes(),
        |msg| identity.sign(msg),
    );
    trace.timestamp = timestamp;
    trace
}

#[test]
fn eval_emergence_reports_signal_eval_and_space_contamination() {
    let dir = TempDir::new().unwrap();
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for session in ["s1", "s2", "s3"] {
        for (capability, outcome, context) in [
            (
                "claude-code/Read",
                Outcome::Succeeded,
                "read file: helper.rs",
            ),
            ("claude-code/Edit", Outcome::Succeeded, "edit file: main.rs"),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: helper.rs",
            ),
            ("claude-code/Bash", Outcome::Failed, "bash: cargo test"),
            (
                "claude-code/Read",
                Outcome::Succeeded,
                "read file: Cargo.toml",
            ),
            ("claude-code/Bash", Outcome::Succeeded, "bash: cargo test"),
        ] {
            let trace = make_trace(&identity, capability, outcome, context, session, timestamp);
            store
                .insert_with_space(&trace, Some("space-alpha"))
                .unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let recommendation = Recommendation {
        kind: RecommendationKind::DoNext,
        source_kind: SignalKind::Preparation,
        body: String::new(),
        candidate: Some(StepCandidate::single(
            "Read",
            Some("helper.rs".into()),
            "medium",
            2,
            1,
        )),
    };

    let mut workspace = WorkspaceState::default();
    workspace.ensure_current_derived_guidance_epoch();
    workspace.record_recommendation_emissions(
        "Edit",
        Some("session-alpha"),
        Some("space-alpha"),
        std::slice::from_ref(&recommendation),
    );
    workspace.resolve_recommendation_feedback(
        Some("session-alpha"),
        Some("space-alpha"),
        "Read",
        Some("/tmp/helper.rs"),
        "succeeded",
    );
    workspace.record_recommendation_emissions(
        "Edit",
        Some("session-beta"),
        Some("space-beta"),
        std::slice::from_ref(&recommendation),
    );
    workspace.resolve_recommendation_feedback(
        Some("session-beta"),
        Some("space-beta"),
        "Bash",
        None,
        "succeeded",
    );
    workspace.record_action("Read", Some("/tmp/helper.rs".into()), "succeeded", None);
    workspace.save(dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "eval-emergence",
            "--global",
            "--hours",
            "168",
            "--max-sessions",
            "10",
            "--json",
        ])
        .output()
        .expect("spawn thronglets eval-emergence --json");

    assert!(
        output.status.success(),
        "eval-emergence --json failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: Value = serde_json::from_slice(&output.stdout).expect("parse eval-emergence json");
    assert_eq!(parsed["project_scope"], Value::Null);
    assert_eq!(parsed["signal_eval"]["sessions_considered"], 3);
    assert_eq!(parsed["workspace_emergence"]["active_spaces_24h"], 2);
    assert_eq!(parsed["workspace_emergence"]["global_positive_24h"], 1);
    assert_eq!(parsed["workspace_emergence"]["global_negative_24h"], 1);
    assert!(
        parsed["workspace_emergence"]["cross_space_contamination_rate"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(
        parsed["workspace_emergence"]["false_signal_pressure"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert_eq!(parsed["substrate_activity"]["activity"], "learning");

    // Field convergence: traces replayed through pheromone field
    let fc = &parsed["field_convergence"];
    assert_eq!(fc["traces_replayed"], 18, "3 sessions × 6 traces each = 18");
    assert!(
        fc["active_capabilities"].as_u64().unwrap() >= 3,
        "should have at least 3 active capabilities (read, edit, exec)"
    );
    assert!(
        fc["total_coupling_edges"].as_u64().unwrap() > 0,
        "Hebbian coupling should form from co-occurring capabilities"
    );
    assert!(
        fc["top_couplings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|edge| edge["level"].as_u64() == Some(1)),
        "space-aware replay should surface Project-level couplings"
    );
}

#[test]
fn eval_emergence_recovers_from_corrupt_workspace_snapshot() {
    let dir = TempDir::new().unwrap();
    let workspace_path = dir.path().join("workspace.json");
    std::fs::write(&workspace_path, "{not valid json").unwrap();

    let workspace = WorkspaceState::load(dir.path());
    assert!(workspace.recent_files.is_empty());

    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("workspace.json.corrupt."))
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(!workspace_path.exists());
}
