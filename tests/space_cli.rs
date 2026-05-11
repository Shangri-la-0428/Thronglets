use serde_json::Value;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_bin(args: &[&str], data_dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .output()
        .expect("failed to run thronglets");
    assert!(
        output.status.success(),
        "command failed: {}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout should be valid json")
}

fn run_text(args: &[&str], data_dir: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .output()
        .expect("failed to run thronglets");
    assert!(
        output.status.success(),
        "command failed: {}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn space_snapshot_is_quiet_when_no_recent_activity_exists() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);

    assert_eq!(data["schema_version"], "thronglets.space.v2");
    assert_eq!(data["command"], "space");
    assert_eq!(data["data"]["summary"]["status"], "quiet");
    assert_eq!(data["data"]["summary"]["active_sessions"], 0);
    assert_eq!(data["data"]["summary"]["signal_count"], 0);
    assert_eq!(data["data"]["learning"]["status"], "quiet");
    assert_eq!(
        data["data"]["learning"]["stable_paths"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(data["data"]["local_feedback"]["positive_24h"], 0);
    assert_eq!(data["data"]["local_feedback"]["negative_24h"], 0);
}

#[test]
fn space_snapshot_surfaces_active_presence_and_signals() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    run_bin(
        &[
            "presence-ping",
            "--space",
            "psyche",
            "--mode",
            "focus",
            "--session-id",
            "codex-psyche-1",
            "--json",
        ],
        &data_dir,
    );
    run_text(
        &[
            "signal-post",
            "--kind",
            "recommend",
            "--space",
            "psyche",
            "--context",
            "shape the psyche roadmap",
            "--message",
            "read the latest plan before editing",
        ],
        &data_dir,
    );

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);

    let status = data["data"]["summary"]["status"].as_str().unwrap();
    assert!(matches!(status, "active" | "converging"));
    assert_eq!(data["data"]["summary"]["active_sessions"], 1);
    assert_eq!(data["data"]["summary"]["signal_count"], 1);
    assert_eq!(data["data"]["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(data["data"]["signals"].as_array().unwrap().len(), 1);
}

#[test]
fn space_learning_surfaces_regression_candidates_from_failed_artifact_traces() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    for session in ["s1", "s2"] {
        run_text(
            &[
                "record",
                "tool:Edit",
                "--outcome",
                "failed",
                "--context",
                "repair flaky login replay",
                "--model",
                "codex",
                "--session-id",
                session,
                "--space",
                "psyche",
                "--method-compliance",
                "unknown",
                "--artifact-ref",
                "/tmp/login-replay.mp4",
                "--trial-key",
                "login-repair",
                "--verification",
                "replay",
            ],
            &data_dir,
        );
    }

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);
    assert_eq!(data["data"]["learning"]["status"], "converging");
    assert_eq!(
        data["data"]["learning"]["regression_candidates"][0]["trial_key"],
        "login-repair"
    );
    assert_eq!(
        data["data"]["learning"]["regression_candidates"][0]["failed_sessions"],
        2
    );
    assert_eq!(
        data["data"]["learning"]["regression_candidates"][0]["verification"],
        "replay"
    );
}

#[test]
fn space_learning_noncompliant_success_does_not_become_stable_path() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    for session in ["s1", "s2", "s3"] {
        run_text(
            &[
                "record",
                "tool:Edit",
                "--outcome",
                "succeeded",
                "--context",
                "duplicate dashboard implementation",
                "--model",
                "codex",
                "--session-id",
                session,
                "--space",
                "psyche",
                "--method-compliance",
                "noncompliant",
            ],
            &data_dir,
        );
    }

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);
    assert_eq!(
        data["data"]["learning"]["stable_paths"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        data["data"]["learning"]["failure_residue"][0]["method_conflict_sessions"],
        3
    );
}

#[test]
fn space_learning_compliant_success_becomes_stable_path() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    for session in ["s1", "s2", "s3"] {
        run_text(
            &[
                "record",
                "tool:Read",
                "--outcome",
                "succeeded",
                "--context",
                "read Cargo.toml before build fix",
                "--model",
                "codex",
                "--session-id",
                session,
                "--space",
                "psyche",
                "--method-compliance",
                "compliant",
            ],
            &data_dir,
        );
    }

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);
    assert_eq!(data["data"]["learning"]["status"], "compressed");
    assert_eq!(
        data["data"]["learning"]["stable_paths"][0]["compliant_success_sessions"],
        3
    );
}

#[test]
fn space_learning_compression_debt_rises_on_conflict_without_regression_artifacts() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join("data");

    for session in ["f1", "f2"] {
        run_text(
            &[
                "record",
                "tool:Edit",
                "--outcome",
                "failed",
                "--context",
                "patch login flow without regression",
                "--model",
                "codex",
                "--session-id",
                session,
                "--space",
                "psyche",
            ],
            &data_dir,
        );
    }
    for session in ["s1", "s2"] {
        run_text(
            &[
                "record",
                "tool:Edit",
                "--outcome",
                "succeeded",
                "--context",
                "patch login flow without regression",
                "--model",
                "codex",
                "--session-id",
                session,
                "--space",
                "psyche",
                "--method-compliance",
                "noncompliant",
            ],
            &data_dir,
        );
    }

    let data = run_bin(&["space", "--space", "psyche", "--json"], &data_dir);
    assert_eq!(data["data"]["learning"]["status"], "blocked");
    assert_eq!(
        data["data"]["learning"]["compression_debt"]["level"],
        "high"
    );
    assert!(
        data["data"]["learning"]["compression_debt"]["score"]
            .as_f64()
            .unwrap()
            >= 0.7
    );
}
