use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

const SCHEMA_VERSION: &str = "thronglets.bootstrap.v2";

fn run_bin(args: &[&str], home: &Path, data_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_thronglets"))
        .args(["--data-dir", data_dir.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .env("PATH", "")
        .output()
        .expect("run thronglets")
}

fn parse_command_data(output: &Output, command: &str) -> Value {
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["schema_version"],
        Value::String(SCHEMA_VERSION.into())
    );
    assert_eq!(envelope["command"], Value::String(command.into()));
    envelope["data"].clone()
}

#[test]
fn bootstrap_claude_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let first = run_bin(
        &["bootstrap", "--agent", "claude", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        first.status.success(),
        "first bootstrap failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = parse_command_data(&first, "bootstrap");
    assert_eq!(first["summary"]["status"], Value::String("healthy".into()));
    assert_eq!(first["summary"]["restart_required"], Value::Bool(false));

    let second = run_bin(
        &["bootstrap", "--agent", "claude", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        second.status.success(),
        "second bootstrap failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = parse_command_data(&second, "bootstrap");
    assert_eq!(second["summary"]["status"], Value::String("healthy".into()));
    assert_eq!(
        second["results"].as_array().unwrap()[0]["changed"]
            .as_array()
            .unwrap()[0],
        Value::String("hooks already present".into())
    );
}

#[test]
fn bootstrap_codex_is_idempotent_after_initial_install() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let first = run_bin(
        &["bootstrap", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        first.status.success(),
        "first bootstrap failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = run_bin(
        &["bootstrap", "--agent", "codex", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        second.status.success(),
        "second bootstrap failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = parse_command_data(&second, "bootstrap");
    assert_eq!(
        second["summary"]["status"],
        Value::String("restart-pending".into())
    );
    assert_eq!(second["summary"]["healthy"], Value::Bool(true));
    assert_eq!(second["summary"]["restart_pending"], Value::Bool(true));
    assert_eq!(
        second["results"].as_array().unwrap()[0]["changed"]
            .as_array()
            .unwrap()[0],
        Value::String("config already present".into())
    );
}

#[test]
fn bootstrap_openclaw_reports_restart_pending() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".openclaw")).unwrap();

    let output = run_bin(
        &["bootstrap", "--agent", "openclaw", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse_command_data(&output, "bootstrap");
    assert_eq!(
        data["summary"]["status"],
        Value::String("restart-pending".into())
    );
    assert_eq!(data["summary"]["healthy"], Value::Bool(true));
    assert_eq!(data["summary"]["restart_pending"], Value::Bool(true));
    assert_eq!(data["results"].as_array().unwrap().len(), 1);
}

#[test]
fn bootstrap_generic_stays_contract_only() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");

    let output = run_bin(
        &["bootstrap", "--agent", "generic", "--json"],
        &home,
        &data_dir,
    );
    assert!(
        output.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse_command_data(&output, "bootstrap");
    assert_eq!(data["summary"]["status"], Value::String("healthy".into()));
    assert_eq!(data["summary"]["healthy"], Value::Bool(true));
    assert_eq!(data["summary"]["restart_required"], Value::Bool(false));
    assert_eq!(data["results"], Value::Array(vec![]));
    assert_eq!(
        data["reports"].as_array().unwrap()[0]["agent"],
        Value::String("generic".into())
    );
}

#[test]
fn bootstrap_all_skips_undetected_codex_and_openclaw() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let output = run_bin(&["bootstrap", "--json"], &home, &data_dir);
    assert!(
        output.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = parse_command_data(&output, "bootstrap");
    assert_eq!(data["summary"]["healthy"], Value::Bool(true));

    let results = data["results"].as_array().unwrap();
    let codex = results
        .iter()
        .find(|entry| entry["agent"] == "codex")
        .unwrap();
    assert_eq!(codex["applied"], Value::Bool(false));
    let openclaw = results
        .iter()
        .find(|entry| entry["agent"] == "openclaw")
        .unwrap();
    assert_eq!(openclaw["applied"], Value::Bool(false));
}
