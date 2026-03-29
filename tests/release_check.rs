use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
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

fn sparse_profile_input() -> String {
    let mut lines = Vec::new();
    lines.push("[thronglets:prehook] tool=Edit emitted=2 stdout_bytes=40 output_mode=next-step decision_path=repair evidence_scope=collective file_guidance_gate=open collective_queries_used=0 total_us=300".to_string());
    lines.push("[thronglets:prehook] tool=Edit emitted=1 stdout_bytes=30 output_mode=context-only decision_path=history evidence_scope=none file_guidance_gate=closed collective_queries_used=0 total_us=200".to_string());
    for _ in 0..8 {
        lines.push("[thronglets:prehook] tool=Bash emitted=0 stdout_bytes=0 output_mode=silent decision_path=none evidence_scope=none file_guidance_gate=na collective_queries_used=0 total_us=100".to_string());
    }
    format!("{}\n", lines.join("\n"))
}

fn run_release_check_with_home(data_dir: &str, home: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir, "release-check"])
        .args(args)
        .env("HOME", home)
        .env("PATH", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn thronglets release-check");

    {
        let mut stdin = child.stdin.take().expect("stdin available");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }

    child.wait_with_output().expect("wait for thronglets")
}

fn run_release_check(data_dir: &str, args: &[&str], input: &str) -> Output {
    let home = Path::new(data_dir).join("home");
    std::fs::create_dir_all(&home).unwrap();
    run_release_check_with_home(data_dir, &home, args, input)
}

#[test]
fn release_check_passes_with_good_profile_and_skips_thin_eval() {
    let dir = tempfile::TempDir::new().unwrap();

    let output = run_release_check(dir.path().to_str().unwrap(), &[], &sparse_profile_input());

    assert!(
        output.status.success(),
        "release-check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("PASS"));
    assert!(stdout.contains("profile: PASS"));
    assert!(stdout.contains("doctor: PASS"));
    assert!(stdout.contains("eval (project): SKIP"));
    assert!(stdout.contains("not enough recent session history"));
}

#[test]
fn release_check_fails_when_eval_finds_noisy_adjacency() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for session in ["s1", "s2", "s3", "s4", "s5", "s6", "s7"] {
        let companion = if matches!(session, "s1" | "s2") {
            "helper.rs"
        } else {
            "other.rs"
        };
        let events = [
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: main.rs".to_string(),
            ),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                format!("edit file: {companion}"),
            ),
        ];
        for (capability, outcome, context) in events {
            let trace = make_trace(&identity, capability, outcome, &context, session, timestamp);
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &["--global"],
        &sparse_profile_input(),
    );

    assert!(
        !output.status.success(),
        "release-check unexpectedly passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL"));
    assert!(stdout.contains("profile: PASS"));
    assert!(stdout.contains("eval (global): FAIL"));
    assert!(stdout.contains("violations:"));
    assert!(stdout.contains("adjacency precision"));
    assert!(stdout.contains("10.0%"));
}

#[test]
fn release_check_can_require_profile_samples() {
    let dir = tempfile::TempDir::new().unwrap();

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &["--require-profile-samples"],
        "",
    );

    assert!(
        !output.status.success(),
        "release-check unexpectedly passed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL"));
    assert!(stdout.contains("profile: FAIL"));
    assert!(stdout.contains("no prehook profile samples found"));
}

#[test]
fn release_check_can_emit_json() {
    let dir = tempfile::TempDir::new().unwrap();

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &["--json"],
        &sparse_profile_input(),
    );

    assert!(
        output.status.success(),
        "release-check --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("parse release-check json");
    assert_eq!(parsed["status"], "PASS");
    assert_eq!(parsed["eval_scope"], "project");
    assert_eq!(parsed["profile"]["status"], "PASS");
    assert_eq!(parsed["doctor"]["status"], "PASS");
    assert_eq!(parsed["eval"]["status"], "SKIP");
}

#[test]
fn release_check_json_reports_eval_failures() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for session in ["s1", "s2", "s3", "s4", "s5", "s6", "s7"] {
        let companion = if matches!(session, "s1" | "s2") {
            "helper.rs"
        } else {
            "other.rs"
        };
        let events = [
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: main.rs".to_string(),
            ),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                format!("edit file: {companion}"),
            ),
        ];
        for (capability, outcome, context) in events {
            let trace = make_trace(&identity, capability, outcome, &context, session, timestamp);
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &["--global", "--json"],
        &sparse_profile_input(),
    );

    assert!(
        !output.status.success(),
        "release-check --json unexpectedly passed"
    );
    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("parse release-check failure json");
    assert_eq!(parsed["status"], "FAIL");
    assert_eq!(parsed["eval_scope"], "global");
    assert_eq!(parsed["profile"]["status"], "PASS");
    assert_eq!(parsed["eval"]["status"], "FAIL");
    assert!(
        !parsed["eval"]["check"]["violations"]
            .as_array()
            .expect("violations array")
            .is_empty()
    );
}

#[test]
fn release_check_can_evaluate_both_scopes() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    let project_root = dir.path().join("project");
    std::fs::create_dir_all(&project_root).unwrap();
    let other_root = dir.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for session in ["p1", "p2"] {
        let main = project_root.join("main.rs");
        let trace = make_trace(
            &identity,
            "claude-code/Edit",
            Outcome::Succeeded,
            &format!("edit file: {}", main.display()),
            session,
            timestamp,
        );
        store.insert(&trace).unwrap();
        timestamp += 61_000;
    }

    for session in ["g1", "g2", "g3", "g4", "g5", "g6", "g7"] {
        let companion = if matches!(session, "g1" | "g2") {
            other_root.join("helper.rs")
        } else {
            other_root.join("other.rs")
        };
        let main = other_root.join("main.rs");
        let events = [
            format!("edit file: {}", main.display()),
            format!("edit file: {}", companion.display()),
        ];
        for context in events {
            let trace = make_trace(
                &identity,
                "claude-code/Edit",
                Outcome::Succeeded,
                &context,
                session,
                timestamp,
            );
            store.insert(&trace).unwrap();
            timestamp += 1_000;
        }
        timestamp += 60_000;
    }

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &[
            "--eval-scope",
            "both",
            "--project-root",
            project_root.to_str().unwrap(),
            "--json",
        ],
        &sparse_profile_input(),
    );

    assert!(
        !output.status.success(),
        "release-check both unexpectedly passed"
    );
    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("parse release-check both json");
    assert_eq!(parsed["status"], "FAIL");
    assert_eq!(parsed["eval_scope"], "both");
    assert_eq!(parsed["doctor"]["status"], "PASS");
    assert_eq!(parsed["eval"]["project"]["status"], "SKIP");
    assert_eq!(parsed["eval"]["global"]["status"], "FAIL");
}

#[test]
fn release_check_can_fail_on_baseline_outcome_regression() {
    let dir = tempfile::TempDir::new().unwrap();
    let baseline_path = dir.path().join("baseline.json");
    let store = TraceStore::open(&dir.path().join("traces.db")).unwrap();
    let identity = NodeIdentity::generate();

    std::fs::write(
        &baseline_path,
        serde_json::json!({
            "project_scope": serde_json::Value::Null,
            "eval_config": {
                "local_history_gate_min": 2,
                "pattern_support_min": 2
            },
            "comparison_to_default": serde_json::Value::Null,
            "sessions_considered": 6,
            "sessions_scored": 6,
            "holdout_command_calls": 6,
            "holdout_failed_command_calls": 0,
            "sessions_with_successful_change": 6,
            "first_successful_change_latency_avg_ms": 0,
            "first_successful_change_latency_p50_ms": 0,
            "edit_points": 12,
            "edit_points_with_signal": 0,
            "repair_opportunities": 0,
            "repair_predictions": 0,
            "repair_first_step_hits": 0,
            "repair_exact_hits": 0,
            "preparation_gated_edit_points": 0,
            "preparation_predictions": 0,
            "preparation_hits": 0,
            "adjacency_gated_edit_points": 0,
            "adjacency_predictions": 0,
            "adjacency_hits": 0,
            "repair_breakdown": {},
            "preparation_breakdown": {},
            "adjacency_breakdown": {}
        })
        .to_string(),
    )
    .unwrap();

    let mut timestamp = chrono::Utc::now().timestamp_millis() as u64 - 10_000;
    for session in ["s1", "s2", "s3", "s4", "s5", "s6"] {
        for (capability, outcome, context) in [
            ("claude-code/Bash", Outcome::Failed, "bash: cargo test".to_string()),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: main.rs".to_string(),
            ),
            (
                "claude-code/Edit",
                Outcome::Succeeded,
                "edit file: helper.rs".to_string(),
            ),
        ] {
            let trace = make_trace(&identity, capability, outcome, &context, session, timestamp);
            store.insert(&trace).unwrap();
            timestamp += 5_000;
        }
        timestamp += 60_000;
    }

    let output = run_release_check(
        dir.path().to_str().unwrap(),
        &[
            "--global",
            "--json",
            "--compare-baseline",
            baseline_path.to_str().unwrap(),
        ],
        &sparse_profile_input(),
    );

    assert!(
        !output.status.success(),
        "release-check unexpectedly passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let parsed: Value =
        serde_json::from_slice(&output.stdout).expect("parse release-check baseline json");
    assert_eq!(parsed["status"], "FAIL");
    assert_eq!(parsed["eval"]["status"], "FAIL");
    assert_eq!(parsed["eval"]["baseline_check"]["status"], "FAIL");
    assert!(parsed["eval"]["summary"]["comparison_to_baseline"].is_object());
}

#[test]
fn release_check_fails_when_local_doctor_is_restart_pending() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let apply = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "apply-plan",
            "--agent",
            "codex",
        ])
        .env("HOME", &home)
        .env("PATH", "")
        .output()
        .expect("run apply-plan");
    assert!(
        apply.status.success(),
        "apply-plan failed: {}",
        String::from_utf8_lossy(&apply.stderr)
    );

    let output = run_release_check_with_home(
        dir.path().to_str().unwrap(),
        &home,
        &[],
        &sparse_profile_input(),
    );

    assert!(
        !output.status.success(),
        "release-check unexpectedly passed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("FAIL"));
    assert!(stdout.contains("doctor: FAIL"));
    assert!(stdout.contains("status: restart-pending"));
    assert!(stdout.contains("thronglets runtime-ready --agent codex"));
}
