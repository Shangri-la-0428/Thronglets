use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use thronglets::context::simhash;
use thronglets::identity::NodeIdentity;
use thronglets::storage::TraceStore;
use thronglets::trace::{Outcome, Trace};

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
fn eval_signals_reports_holdout_metrics() {
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
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "eval-signals",
            "--global",
            "--hours",
            "168",
            "--max-sessions",
            "10",
        ])
        .output()
        .expect("spawn thronglets eval-signals");

    assert!(
        output.status.success(),
        "eval-signals failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project scope: global"));
    assert!(stdout.contains("sessions considered: 3"));
    assert!(stdout.contains("repair first-step precision"));
    assert!(stdout.contains("preparation precision"));
    assert!(stdout.contains("adjacency precision"));
}

#[test]
fn eval_signals_can_emit_json() {
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
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "eval-signals",
            "--global",
            "--hours",
            "168",
            "--max-sessions",
            "10",
            "--json",
        ])
        .output()
        .expect("spawn thronglets eval-signals --json");

    assert!(
        output.status.success(),
        "eval-signals --json failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: Value = serde_json::from_slice(&output.stdout).expect("parse eval-signals json");
    assert_eq!(parsed["project_scope"], Value::Null);
    assert_eq!(parsed["sessions_considered"], 3);
    assert!(parsed["repair_breakdown"]["Bash"].is_object());
    assert!(parsed["preparation_breakdown"]["main.rs"].is_object());
}

#[test]
fn eval_signals_json_can_focus_and_trim_breakdowns() {
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
            (
                "claude-code/TaskUpdate",
                Outcome::Failed,
                "TaskUpdate: plan",
            ),
        ] {
            let trace = make_trace(&identity, capability, outcome, context, session, timestamp);
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "eval-signals",
            "--global",
            "--hours",
            "168",
            "--max-sessions",
            "10",
            "--json",
            "--focus",
            "repair",
            "--top-breakdowns",
            "1",
        ])
        .output()
        .expect("spawn focused eval-signals --json");

    assert!(
        output.status.success(),
        "focused eval-signals --json failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("parse focused eval-signals json");
    let repair_keys = parsed["repair_breakdown"]
        .as_object()
        .expect("repair breakdown object");
    let prep_keys = parsed["preparation_breakdown"]
        .as_object()
        .expect("preparation breakdown object");
    let adjacency_keys = parsed["adjacency_breakdown"]
        .as_object()
        .expect("adjacency breakdown object");

    assert_eq!(repair_keys.len(), 1);
    assert!(repair_keys.contains_key("Bash"));
    assert!(prep_keys.is_empty());
    assert!(adjacency_keys.is_empty());
}

#[test]
fn eval_signals_defaults_to_project_scope() {
    let dir = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for (session, root) in [("p1", project.path()), ("p2", project.path()), ("o1", other.path())] {
        let helper = root.join("helper.rs");
        let main = root.join("main.rs");
        for (capability, outcome, context) in [
            (
                "claude-code/Read",
                Outcome::Succeeded,
                format!("read file: {}", helper.display()),
            ),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                format!("edit file: {}", main.display()),
            ),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                format!("edit file: {}", helper.display()),
            ),
        ] {
            let trace = make_trace(&identity, capability, outcome, &context, session, timestamp);
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .current_dir(project.path())
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "eval-signals",
            "--hours",
            "168",
            "--max-sessions",
            "10",
            "--json",
        ])
        .output()
        .expect("spawn project-scoped eval-signals");

    assert!(
        output.status.success(),
        "project-scoped eval-signals failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let parsed: Value = serde_json::from_slice(&output.stdout).expect("parse project-scoped json");
    let scope = parsed["project_scope"]
        .as_str()
        .expect("project scope string");
    let expected = project.path().display().to_string();
    let alternate = if let Some(stripped) = expected.strip_prefix("/var/") {
        format!("/private/var/{stripped}")
    } else if let Some(stripped) = expected.strip_prefix("/private/var/") {
        format!("/var/{stripped}")
    } else {
        expected.clone()
    };
    assert!(scope == expected || scope == alternate, "unexpected project scope: {scope}");
    assert_eq!(parsed["sessions_considered"], 2);
    assert_eq!(parsed["sessions_scored"], 1);
}
